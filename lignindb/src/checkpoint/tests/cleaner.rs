//! Checkpoints beside the segment cleaner (`lignin_store::cleaner`,
//! STORE-08).
//!
//! The cleaner knows of no checkpoint. It copies the live blocks of a
//! segment into a segment of its own, and deletes the live store's name of
//! the emptied one under the lock on the manifest, which a creation holds
//! while it links. It never cuts a segment short and writes nothing into
//! one it cleans, so a checkpoint that has its own name of the file reads
//! the bytes it was made with.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use lignin_format::FileId;
use lignin_io::StdVfs;
use lignin_store::{
    cleaner::{Cleaner, CleanerConfig, CleanerState},
    flush::{Checkpointed, FlushConfig},
    segment::FileNames,
    wal::LogOptions,
};

use super::{
    block, building, bytes_of, checkpointer, checkpoints_in, open, remove_block, root_hash, turn,
    Opened,
};
use crate::{
    checkpoint::create::Step,
    snapshot::Versions,
    txn::tests::{
        store::{Db, NoSync, Rng, SIZE},
        World,
    },
};

const SEGMENT_LEN: u64 = 32 << 10;

/// An empty store whose write-back seals a segment at 32 KiB, so that the
/// blocks of a few commits lie in several segments that take no more
/// blocks.
fn store() -> (Arc<Db>, Versions) {
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().to_path_buf();
    let flush = FlushConfig {
        segment_len: SEGMENT_LEN,
        ..FlushConfig::new(SIZE)
    };
    let options = LogOptions::default();
    Db::on(Arc::new(NoSync), &path, 256, options, flush, Some(dir))
}

/// Fourteen blocks, of which every other one removes the documents of an
/// earlier block, each written back, and a table checkpoint after them:
/// segments that take no more blocks hold dead blocks beside live ones,
/// and some hold only dead ones.
fn churn<'s>(world: &mut World<'s>, rng: &mut Rng, db: &Db, versions: &Versions) {
    for round in 0..14 {
        block(world, rng, round * 20, 20);
        if round >= 4 && round % 2 == 0 {
            remove_block(world, (round - 4) * 20, 20);
        }
        turn(db, versions);
    }
    let taken = (db.write_back(versions).checkpoint()).expect("a table checkpoint");
    assert!(matches!(taken, Checkpointed::Installed { .. }), "{taken:?}");
}

/// A cleaner that leaves no dead block where it is: it moves the live
/// blocks out of every segment that holds a dead one, and has the table
/// checkpoint taken that lets an emptied segment go.
fn eager() -> CleanerConfig {
    CleanerConfig {
        target_dead_percent: 0,
        checkpoint_dead_percent: 0,
        seal_dead_percent: 0,
        max_segment_percent: 101,
        min_segment_len: 0,
        max_pass_bytes: u64::MAX,
        segment_len: SEGMENT_LEN,
        value_dead_percent: 0,
        ..CleanerConfig::new()
    }
}

/// A cleaner that moves nothing: it takes the links of superseded page
/// versions out of the chains, where write-back has left any, and deletes
/// the segments that are left with no live block. What a segment that
/// takes no more blocks holds after it is live until something moves it.
fn trimming() -> CleanerConfig {
    CleanerConfig {
        target_dead_percent: 100,
        seal_dead_percent: 101,
        ..eager()
    }
}

fn segment_in(dir: &Path, file: FileId) -> PathBuf {
    FileNames::SEGMENT.path(dir, file)
}

/// Has `cleaner`, which is eager, work on a store that nothing commits to
/// and that a trimming cleaner has been over. Returns the segments that
/// the checkpoint in `target` links, that held live blocks, and that the
/// live store has no more, each with the bytes its file had before the
/// cleaner began. No link was trimmed meanwhile, so nothing but a move
/// took a live block out of such a segment: the cleaner moved every one
/// of them. At least one segment went that way.
fn empty_linked_segments(
    db: &Db,
    cleaner: &Cleaner<'_>,
    target: &Path,
) -> BTreeMap<FileId, Vec<u8>> {
    let space = cleaner.space().expect("the space");
    let linked: BTreeMap<FileId, (u64, Vec<u8>)> = (space.segments.iter())
        .filter(|segment| !segment.has_writer && segment.live_blocks > 0)
        .filter(|segment| segment_in(target, segment.file).exists())
        .map(|segment| {
            let bytes = bytes_of(&StdVfs, &segment_in(target, segment.file));
            (segment.file, (segment.live_blocks, bytes))
        })
        .collect();
    assert!(
        linked.len() >= 2,
        "the checkpoint links {:?} of {space:?}",
        linked.keys()
    );

    let before = cleaner.state().stats();
    for _ in 0..4 {
        cleaner.pass().expect("a pass over the segments");
    }
    let did = cleaner.state().stats();
    assert_eq!(did.errors, 0, "{did:?}");
    assert_eq!(did.links_trimmed, before.links_trimmed, "{did:?}");
    let (mut emptied, mut moved_out) = (BTreeMap::new(), 0);
    for (file, (live_blocks, bytes)) in linked {
        if !segment_in(&db.dir, file).exists() {
            moved_out += live_blocks;
            emptied.insert(file, bytes);
        }
    }
    assert!(
        !emptied.is_empty(),
        "no linked segment was deleted: {did:?}"
    );
    assert!(
        did.blocks_moved - before.blocks_moved >= moved_out,
        "{moved_out} live blocks were in the segments that went: {did:?}"
    );
    assert!(
        did.segments_dropped - before.segments_dropped >= emptied.len() as u64,
        "{did:?}"
    );
    emptied
}

/// A checkpoint is made of a store whose blocks lie in several segments.
/// The cleaner moves the live blocks out of segments that the checkpoint
/// links and deletes them from the live store: after the creation, or,
/// with `while_created`, while the creation is under way, between the
/// step that links the segments and the one that brings the log.
///
/// The link keeps each of those files, byte for byte. The checkpoint
/// opens at its version with the root hash that version had and reads
/// every document of it, while the live store goes on writing and
/// cleaning, and again from its own directory once it was closed.
fn the_cleaner_deletes_segments_that_a_checkpoint_links(while_created: bool) {
    let (db, versions) = store();
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(31);
    churn(&mut world, &mut rng, &db, &versions);
    let write_back = db.write_back(&versions);
    let trimmed = CleanerState::new(trimming());
    for _ in 0..3 {
        (Cleaner::new(write_back, &trimmed).pass()).expect("a pass that moves nothing");
    }
    let swept = trimmed.stats();
    assert_eq!((swept.blocks_moved, swept.errors), (0, 0), "{swept:?}");

    let captured = world.model.clone();
    let hash = root_hash(&db, &versions, versions.latest()).expect("the grove has a root");
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(write_back, &state).with_stats(&db.stats);
    let target = checkpoints_in(&db).join("1");
    let emptied = OnceLock::new();
    let clean = |checkpoint: &Path| {
        let went = empty_linked_segments(&db, &cleaner, checkpoint);
        emptied.set(went).expect("one cleaning");
    };
    let pause = |step: Step| {
        if while_created && step == Step::Sampled {
            assert_eq!(versions.pins_held(), 1, "the creation holds its pin");
            clean(&building(&target));
        }
    };
    let created = (checkpointer(&db, &versions).pausing(&pause))
        .create(&target)
        .expect("a checkpoint");
    assert_eq!(created.version.get(), captured.version);
    assert!(created.linked > 0 && created.copied == 0, "{created:?}");
    if !while_created {
        clean(&target);
    }
    let emptied = emptied.into_inner().expect("a cleaning");
    let still_whole = || {
        for (file, bytes) in &emptied {
            assert!(
                &bytes_of(&StdVfs, &segment_in(&target, *file)) == bytes,
                "the checkpoint's segment {file} changed"
            );
        }
    };
    let reads_as_made = |opened: &Opened| {
        let hashed = root_hash(&opened.db, &opened.versions, created.version);
        assert_eq!(hashed, Some(hash));
        opened.check(&captured);
    };
    still_whole();
    let opened = open(&db, &target);
    reads_as_made(&opened);

    for round in 14..22 {
        block(&mut world, &mut rng, round * 20, 20);
        remove_block(&mut world, (round - 6) * 20, 20);
        turn(&db, &versions);
        cleaner.pass().expect("a pass over the segments");
        (db.clean_values(&versions, &cleaner)).expect("a pass over the value log");
        opened.check(&captured);
    }
    let did = state.stats();
    assert_eq!(did.errors, 0, "{did:?}");
    assert_ne!(root_hash(&db, &versions, versions.latest()), Some(hash));
    world.check_committed();
    cleaner.finish().expect("the cleaner's files are closed");

    still_whole();
    drop(opened);
    reads_as_made(&open(&db, &target));
}

/// **A checkpoint reads on when the cleaner has emptied and deleted
/// segments that it links** (CK7).
#[test]
fn a_checkpoint_keeps_the_segments_that_the_cleaner_empties_and_deletes() {
    the_cleaner_deletes_segments_that_a_checkpoint_links(false);
}

/// **T-CK3 for the segments: the cleaner running during a creation takes
/// no file from it** (CK3). Once the creation has given back the lock on
/// the manifest, it has its names of the segments, and the cleaner deletes
/// the live store's names of them while the creation still brings the log.
#[test]
fn a_cleaner_that_runs_while_a_checkpoint_is_created_takes_no_segment_from_it() {
    the_cleaner_deletes_segments_that_a_checkpoint_links(true);
}

/// **T-CK3 for the segments, at the lock.** A pass of the cleaner that has
/// segments to delete begins while a creation holds the lock on the
/// manifest and has linked nothing yet. The pass retires its segments and
/// then waits for the lock: nothing is deleted. The creation links every
/// segment of its sample, gives the lock back, and only then do the live
/// store's names go. The checkpoint has the files, and opens with the
/// root hash of its version.
#[test]
fn a_pass_that_begins_while_a_creation_links_deletes_nothing_before_the_links() {
    let (db, versions) = store();
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(32);
    churn(&mut world, &mut rng, &db, &versions);
    let state = CleanerState::new(trimming());
    let cleaner = Cleaner::new(db.write_back(&versions), &state);
    let space = cleaner.space().expect("the space");
    let empty: Vec<FileId> = (space.segments.iter())
        .filter(|segment| !segment.has_writer && segment.live_blocks == 0)
        .map(|segment| segment.file)
        .collect();
    assert!(!empty.is_empty(), "{space:?}");
    let captured = world.model.clone();
    let hash = root_hash(&db, &versions, versions.latest()).expect("the grove has a root");
    let target = checkpoints_in(&db).join("1");
    let built = building(&target);

    let created = std::thread::scope(|scope| {
        let pass = Mutex::new(None);
        let pause = |step: Step| match step {
            Step::Linking => {
                let began = scope.spawn(|| cleaner.pass().expect("a pass"));
                *pass.lock().expect("the pass") = Some(began);
                let started = Instant::now();
                while state.stats().segments_retired < empty.len() as u64 {
                    assert!(
                        started.elapsed() < Duration::from_secs(60),
                        "the pass retired nothing"
                    );
                    std::thread::yield_now();
                }
                std::thread::sleep(Duration::from_millis(20));
                assert_eq!(state.stats().segments_dropped, 0);
                for file in &empty {
                    assert!(segment_in(&db.dir, *file).exists(), "segment {file}");
                    assert!(!segment_in(&built, *file).exists(), "segment {file}");
                }
            }
            Step::Sampled => {
                for file in &empty {
                    assert!(segment_in(&built, *file).exists(), "segment {file}");
                }
                let began = pass.lock().expect("the pass").take().expect("a pass");
                let done = began.join().expect("the pass");
                assert!(done.dropped >= empty.len(), "{done:?}");
                for file in &empty {
                    assert!(!segment_in(&db.dir, *file).exists(), "segment {file}");
                    assert!(segment_in(&built, *file).exists(), "segment {file}");
                }
            }
            _ => {}
        };
        (checkpointer(&db, &versions).pausing(&pause))
            .create(&target)
            .expect("a checkpoint")
    });
    assert!(created.linked > 0 && created.copied == 0, "{created:?}");
    let did = state.stats();
    assert_eq!((did.blocks_moved, did.errors), (0, 0), "{did:?}");
    cleaner.finish().expect("the cleaner's files are closed");

    let opened = open(&db, &target);
    let hashed = root_hash(&opened.db, &opened.versions, created.version);
    assert_eq!(hashed, Some(hash));
    opened.check(&captured);
    world.check_committed();
}
