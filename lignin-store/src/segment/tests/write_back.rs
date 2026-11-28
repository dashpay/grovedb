//! A write-back that fails unreported (`lignin_io::sim`, "A write-back that
//! fails"), on a file layer that tells of it only the handle that wrote:
//! the least the file layer's contract promises.
//!
//! A segment is synced through the handle that appended to it, so the
//! first sync of the segment after the failure is told of it and fails,
//! whichever way that sync is asked for. The segment has then failed, and
//! nothing vouches for the bytes.

use lignin_format::segment::DATA_START;
use lignin_io::{
    sim::{Fault, SimConfig, SimError, SimVfs, Told, UnknownFate, WriteBackRule},
    OpenMode,
};

use super::{sim::*, *};

const FATES: [UnknownFate; 3] = [UnknownFate::Lost, UnknownFate::Kept, UnknownFate::Drawn(3)];

/// The ways a segment that is being written is made durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    /// [`SegmentWriter::sync`].
    Writer,
    /// [`SegmentWriter::seal`].
    Seal,
    /// [`SegmentStore::sync`] while the segment has its writer.
    Store,
    /// [`SegmentStore::sync`] once the writer is dropped.
    StoreAfterTheWriter,
}

const WAYS: [Way; 4] = [Way::Writer, Way::Seal, Way::Store, Way::StoreAfterTheWriter];

struct Sim {
    fs: SimVfs,
    manifest: ManifestStore,
    segments: SegmentStore,
}

impl Sim {
    fn new(unknown: UnknownFate) -> Self {
        let fs = sim_fs(SimConfig {
            unknown,
            ..traced(11, 512)
        });
        let manifest = ManifestStore::create(shared(&fs), db_dir(), empty_manifest())
            .expect("the manifest is created");
        let segments = SegmentStore::open(&manifest).expect("the segments open");
        Self {
            fs,
            manifest,
            segments,
        }
    }

    fn commit(&mut self) {
        self.segments
            .commit(&mut self.manifest)
            .expect("the manifest is replaced");
    }

    fn named(&self) -> Vec<LiveFile> {
        let manifest = self.manifest.manifest().expect("not in doubt");
        manifest.files().to_vec()
    }
}

fn segment_path(raw_id: u32) -> PathBuf {
    FileNames::SEGMENT.path(db_dir(), id(raw_id))
}

/// Fails the write-back of segment `raw_id` after the append that follows
/// `skip` more of them.
fn fail_write_back(fs: &SimVfs, raw_id: u32, skip: u64, forgotten: bool) {
    let rule = WriteBackRule::new(Fault::Eio)
        .at(segment_path(raw_id))
        .skip(skip)
        .told(Told::Writers)
        .forgotten(forgotten);
    fs.add_write_back(rule);
}

/// Whether `error` is a sync of segment `raw_id` that the file layer
/// failed with the error of the write-back.
fn is_told(error: &SegmentStoreError, raw_id: u32) -> bool {
    let SegmentStoreError::Io { op, path, source } = error else {
        return false;
    };
    let injected = matches!(
        SimError::of(source),
        Some(SimError::Injected {
            fault: Fault::Eio,
            ..
        })
    );
    *op == "sync" && *path == segment_path(raw_id) && injected
}

/// The blocks of segment `raw_id` in the store of `fs`, opened as the
/// next session opens it.
fn blocks_in(fs: &SimVfs, raw_id: u32) -> Vec<(BlockLocation, BlockContent, Vec<u8>)> {
    let manifest = ManifestStore::open(shared(fs), db_dir()).expect("the manifest opens");
    let segments = SegmentStore::open(&manifest).expect("the segments open");
    let mut walk = segments
        .walk(id(raw_id), MAX)
        .expect("the segment is walked");
    let mut blocks = Vec::new();
    while let Some((location, block)) = walk.next_block().expect("a whole block") {
        let payload = block.payload().expect("decompresses").into_owned();
        blocks.push((location, block.content(), payload));
    }
    blocks
}

fn as_walked(blocks: &[Appended]) -> Vec<(BlockLocation, BlockContent, Vec<u8>)> {
    blocks
        .iter()
        .map(|block| (block.location, block.content, block.payload.clone()))
        .collect()
}

#[test]
fn no_sync_of_a_segment_vouches_for_blocks_whose_write_back_failed() {
    for (way, unknown, forgotten) in WAYS
        .into_iter()
        .flat_map(|way| FATES.map(|unknown| (way, unknown)))
        .flat_map(|(way, unknown)| [false, true].map(|forgotten| (way, unknown, forgotten)))
    {
        let context = || format!("{way:?}, {unknown:?}, forgotten: {forgotten}");
        let _context = OnFailure(&context);
        let mut sim = Sim::new(unknown);
        let mut writer = sim.segments.create().expect("a segment is created");
        sim.commit();
        let synced = append_blocks(&mut writer, 2, 1);
        let durable = writer.sync().expect("the first blocks are synced");

        // The write-back fails after the second of three more blocks.
        fail_write_back(&sim.fs, 1, 1, forgotten);
        append_blocks(&mut writer, 3, 7);
        assert_eq!(sim.fs.stats().write_backs, 1);

        let mut writer = Some(writer);
        let refused = match way {
            Way::Writer => writer.as_mut().unwrap().sync().map(drop),
            Way::Seal => writer.as_mut().unwrap().seal().map(drop),
            Way::Store => sim.segments.sync(),
            Way::StoreAfterTheWriter => {
                writer = None;
                sim.segments.sync()
            }
        };
        let error = refused.expect_err("the sync is told of the write-back that failed");
        assert!(is_told(&error, 1), "{error}");

        // The segment has failed as after any failed sync, and no later
        // sync of it says otherwise.
        if let Some(writer) = &mut writer {
            for _ in 0..2 {
                assert!(matches!(
                    writer.sync(),
                    Err(SegmentStoreError::SegmentFailed { .. })
                ));
                assert!(matches!(
                    sim.segments.sync(),
                    Err(SegmentStoreError::SegmentFailed { .. })
                ));
            }
            assert!(matches!(
                writer.seal(),
                Err(SegmentStoreError::SegmentFailed { .. })
            ));
        }
        drop(writer);
        sim.segments.sync().expect("a closed segment is not synced");

        // The manifest names what was synced before the failure, and
        // every state a crash can leave has it.
        sim.commit();
        assert_eq!(sim.named(), [segment(1, FileState::Closed, durable)]);
        let states = crash_states(&sim.fs.crash_image(), &EVERY_STATE);
        assert!(states.exhaustive);
        for (plan, state) in &states.states {
            assert_eq!(blocks_in(state, 1), as_walked(&synced), "{plan:?}");
        }
        let Sim {
            fs,
            manifest,
            segments,
        } = sim;
        drop((manifest, segments));
        assert_eq!(blocks_in(&fs, 1), as_walked(&synced));
    }
}

#[test]
fn a_write_back_that_fails_under_the_header_of_a_segment_fails_its_creation() {
    for unknown in FATES {
        let mut sim = Sim::new(unknown);
        fail_write_back(&sim.fs, 1, 0, false);
        let error = sim
            .segments
            .create()
            .expect_err("the sync of the header is told");
        assert!(is_told(&error, 1), "{error}");
        assert_eq!(sim.fs.stats().write_backs, 1);
        assert!(sim.fs.open(&segment_path(1), OpenMode::Read).is_err());

        let mut writer = sim.segments.create().expect("the next segment is created");
        assert_eq!(writer.file_id(), id(2));
        sim.commit();
        let blocks = append_blocks(&mut writer, 2, 1);
        let sealed = writer.seal().expect("the segment is sealed");
        drop(writer);
        sim.commit();
        assert_eq!(sim.named(), [segment(2, FileState::Sealed, sealed)]);
        let states = crash_states(&sim.fs.crash_image(), &EVERY_STATE);
        for (plan, state) in &states.states {
            assert_eq!(blocks_in(state, 2), as_walked(&blocks), "{plan:?}");
        }
    }
}

/// Where storage kept every byte of a write-back that failed, the sync is
/// told all the same, and the segment ends where it was last synced: the
/// store does not learn what storage has.
#[test]
fn a_sync_is_told_of_a_failed_write_back_that_kept_every_block() {
    let mut sim = Sim::new(UnknownFate::Kept);
    let mut writer = sim.segments.create().expect("a segment is created");
    sim.commit();
    fail_write_back(&sim.fs, 1, 0, false);
    let kept = append_blocks(&mut writer, 1, 1);
    let error = writer.sync().expect_err("the sync is told");
    assert!(is_told(&error, 1), "{error}");
    assert!(
        sim.segments
            .read(kept[0].location, MAX, &mut Vec::new())
            .is_ok(),
        "readable while the writer is there to write it elsewhere"
    );
    drop(writer);
    sim.commit();
    assert_eq!(sim.named(), [segment(1, FileState::Closed, DATA_START)]);
    assert!(matches!(
        sim.segments.read(kept[0].location, MAX, &mut Vec::new()),
        Err(SegmentStoreError::OutOfBounds { .. })
    ));
}
