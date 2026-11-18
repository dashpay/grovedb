//! `ENOSPC` and `EIO`, injected by the simulated file system.
//!
//! A step that meets one completes or fails with the injected error inside a
//! [`SegmentStoreError`], and the directory keeps a whole manifest: the one
//! it had, or the new one where the failure came once the new one was in
//! place.

use std::{collections::BTreeMap, slice};

use lignin_io::sim::{CrashPoint, Fault, FaultRule, OpKind, OpRecord, SimConfig, SimVfs};

use super::{
    model::{check_recovered, first_manifest, with_another_log, Run, SEGMENT_LIFE},
    sim::*,
    *,
};

const FAULTS: [Fault; 2] = [Fault::Eio, Fault::Enospc];

/// The place of the rename among the operations of a replacement: after the
/// failed removal of a temporary file that is not there, its creation, its
/// write and its sync.
const RENAME_OF_A_REPLACEMENT: u64 = 5;

/// How the disk fails, from one operation on.
#[derive(Clone, Copy, Debug)]
enum Disk {
    /// One operation fails. If it is an append, the file layer takes it back.
    FailsOnce(Fault),
    /// One append fails, and the file layer cannot take it back.
    TearsAnAppend(Fault),
    /// Every operation that needs space fails.
    IsFull,
}

impl Disk {
    fn fault(self) -> Fault {
        match self {
            Self::FailsOnce(fault) | Self::TearsAnAppend(fault) => fault,
            Self::IsFull => Fault::Enospc,
        }
    }

    fn rule(self, from_op: u64) -> FaultRule {
        let rule = FaultRule::new(self.fault()).from_op(from_op);
        match self {
            Self::FailsOnce(_) => rule.torn_append(false),
            Self::TearsAnAppend(_) => rule.on(OpKind::Append).torn_append(true),
            Self::IsFull => rule.forever().torn_append(false),
        }
    }
}

/// Whether the failed operation of a step that was installing a manifest
/// came once the new manifest was in place: the sync of the directory after
/// the rename, or the deletion of a file the new manifest no longer names.
fn new_manifest_is_in_place(failed: &OpRecord) -> bool {
    match failed.kind {
        OpKind::SyncDir => failed.path == DB,
        OpKind::RemoveFile => failed.path.starts_with("/db/seg/"),
        _ => false,
    }
}

/// Runs `workload` once for every operation of it that `disk` can fail,
/// with the disk failing from that operation on, and checks what the failure
/// leaves. Returns how often each kind of operation was failed.
fn fail_everywhere(
    seed: u64,
    disk: Disk,
    workload: impl Fn(&mut Run) -> Result<(), SegmentStoreError>,
) -> BTreeMap<OpKind, usize> {
    let mut failed_kinds = BTreeMap::new();
    let mut from_op = 1;
    loop {
        let fs = sim_fs(traced(seed, 512));
        fs.add_rule(disk.rule(from_op));
        let mut run = Run::new(shared(&fs), db_dir(), seed);
        let result = workload(&mut run);
        let Some(failed) = failed_ops(&fs).into_iter().next() else {
            result.expect("the workload runs when nothing fails");
            break;
        };
        let context = || format!("seed {seed}, {disk:?}: {failed}");
        let _context = OnFailure(&context);
        let error = result.expect_err("the step that met the fault fails");
        assert_eq!(injected_op(&error), Some((disk.fault(), failed.op)));
        let model = &run.model;

        let installing = model.pending.is_some();
        let new_in_place = installing && new_manifest_is_in_place(&failed);
        let not_deleted = installing && failed.kind == OpKind::RemoveFile;
        assert_eq!(
            matches!(error, SegmentStoreError::NotDeleted { .. }),
            not_deleted
        );
        let expected = if new_in_place {
            &model.pending
        } else {
            &model.acked
        };
        assert_eq!(&manifest_in(&fs), expected, "while the disk still fails");

        let image = fs.crash_image();
        let drawn = (0..4).map(|draw| image.sample(draw));
        for plan in [image.lose_all(), image.keep_all()]
            .into_iter()
            .chain(drawn)
        {
            let context = || format!("{image:?}\n{plan:?}");
            let _context = OnFailure(&context);
            let state = image.recover(&plan).expect("a plan of the image fits it");
            check_recovered(Arc::new(state), db_dir(), model);
        }

        run.end_session();
        fs.clear_rules();
        assert_eq!(
            check_recovered(shared(&fs), db_dir(), &run.model),
            new_in_place
        );

        *failed_kinds.entry(failed.kind).or_insert(0) += 1;
        from_op = failed.op + 1;
    }
    failed_kinds
}

#[test]
fn a_fault_at_any_operation_fails_the_step_and_leaves_a_whole_manifest() {
    let every_kind = [
        OpKind::Open,
        OpKind::Rename,
        OpKind::RemoveFile,
        OpKind::CreateDir,
        OpKind::ReadDir,
        OpKind::SyncDir,
        OpKind::Read,
        OpKind::Write,
        OpKind::Append,
        OpKind::SyncData,
        OpKind::SyncAll,
        OpKind::Len,
    ];
    let needs_space = [
        OpKind::Open,
        OpKind::Rename,
        OpKind::CreateDir,
        OpKind::SyncDir,
        OpKind::Write,
        OpKind::Append,
        OpKind::SyncData,
        OpKind::SyncAll,
    ];
    let cases: [(Disk, &[OpKind]); 5] = [
        (Disk::FailsOnce(Fault::Eio), &every_kind),
        (Disk::FailsOnce(Fault::Enospc), &needs_space),
        (Disk::IsFull, &needs_space),
        (Disk::TearsAnAppend(Fault::Eio), &[OpKind::Append]),
        (Disk::TearsAnAppend(Fault::Enospc), &[OpKind::Append]),
    ];
    for (disk, kinds) in cases {
        let mut failed = fail_everywhere(51, disk, |run| run.script(&SEGMENT_LIFE));
        for (kind, count) in fail_everywhere(52, disk, |run| run.run(60)) {
            *failed.entry(kind).or_insert(0) += count;
        }
        println!("{disk:?}: {failed:?}");
        assert_eq!(failed.keys().copied().collect::<Vec<_>>(), kinds);
    }
}

fn created(fs: &SimVfs, manifest: Manifest) -> ManifestStore {
    ManifestStore::create(shared(fs), db_dir(), manifest).expect("the first manifest is written")
}

fn manifests_a_crash_can_leave(fs: &SimVfs) -> Vec<Manifest> {
    let image = fs.crash_image();
    let crash = crash_states(&image, &EVERY_STATE);
    assert!(crash.exhaustive);
    let mut found = Vec::new();
    for (_, state) in crash.states {
        let manifest = manifest_in(&state).expect("a manifest");
        if !found.contains(&manifest) {
            found.push(manifest);
        }
    }
    found
}

#[test]
fn a_replacement_that_fails_before_its_rename_can_be_made_again() {
    let first = first_manifest();
    let second = with_another_log(&first);
    for fault in FAULTS {
        for step in [OpKind::Open, OpKind::Write, OpKind::SyncAll] {
            let fs = sim_fs(SimConfig::new(61));
            let mut store = created(&fs, first.clone());
            fs.add_rule(FaultRule::new(fault).on(step).forever());

            for _ in 0..2 {
                let error = replace(&mut store, second.clone()).unwrap_err();
                assert_eq!(injected_op(&error).map(|(fault, _)| fault), Some(fault));
                assert_eq!(store.manifest().unwrap(), &first);
                assert!(!has_temporary_file(&fs), "{fault} in {step}");
                assert_eq!(manifests_a_crash_can_leave(&fs), slice::from_ref(&first));
            }

            fs.clear_rules();
            replace(&mut store, second.clone()).expect("the disk is in order again");
            assert_eq!(store.manifest().unwrap(), &second);
            assert_eq!(manifests_a_crash_can_leave(&fs), slice::from_ref(&second));
        }
    }
}

#[test]
fn a_replacement_that_fails_from_its_rename_on_is_settled_by_opening_the_manifest_again() {
    let first = first_manifest();
    let second = with_another_log(&first);
    let third = with_another_log(&second);
    for fault in FAULTS {
        for (step, visible, after_a_crash) in [
            (OpKind::Rename, &first, vec![first.clone()]),
            (
                OpKind::SyncDir,
                &second,
                vec![first.clone(), second.clone()],
            ),
        ] {
            let fs = sim_fs(SimConfig::new(62));
            let mut store = created(&fs, first.clone());
            fs.add_rule(FaultRule::new(fault).on(step));

            let error = replace(&mut store, second.clone()).unwrap_err();
            assert_eq!(injected_op(&error).map(|(fault, _)| fault), Some(fault));
            for error in [
                store.manifest().unwrap_err(),
                replace(&mut store, third.clone()).unwrap_err(),
            ] {
                assert!(matches!(error, SegmentStoreError::ManifestInDoubt { .. }));
            }
            assert_eq!(
                manifest_in(&fs).as_ref(),
                Some(visible),
                "{fault} in {step}"
            );
            let mut left = manifests_a_crash_can_leave(&fs);
            left.sort_by_key(|manifest| manifest.files().len());
            assert_eq!(left, after_a_crash, "{fault} in {step}");

            let mut store = ManifestStore::open(shared(&fs), db_dir()).expect("the manifest opens");
            assert_eq!(store.manifest().unwrap(), visible);
            assert_eq!(manifests_a_crash_can_leave(&fs), slice::from_ref(visible));
            replace(&mut store, third.clone()).expect("the manifest is replaced");
            assert_eq!(manifests_a_crash_can_leave(&fs), slice::from_ref(&third));
        }
    }
}

#[test]
fn a_segment_directory_whose_sync_failed_is_made_durable_by_the_next_open() {
    for fault in FAULTS {
        let fs = sim_fs(SimConfig::new(63));
        let mut manifest = created(&fs, empty_manifest());
        fs.add_rule(FaultRule::new(fault).on(OpKind::SyncDir));
        let error = SegmentStore::open(&manifest).unwrap_err();
        assert_eq!(injected_op(&error).map(|(fault, _)| fault), Some(fault));

        let segments = SegmentStore::open(&manifest).expect("the segments open");
        let _writer = segments.create().expect("a segment is created");
        fs.crash_at(CrashPoint::AfterOp(
            fs.stats().ops + RENAME_OF_A_REPLACEMENT,
        ));
        let error = segments.commit(&mut manifest).unwrap_err();
        assert!(is_power_loss(&error), "{error}");

        let image = fs.crash_image();
        let crash = crash_states(&image, &EVERY_STATE);
        assert!(crash.exhaustive);
        let mut named = 0;
        for (plan, state) in crash.states {
            let context = || format!("{image:?}\n{plan:?}");
            let _context = OnFailure(&context);
            let mut manifest =
                ManifestStore::open(shared(&state), db_dir()).expect("the manifest opens");
            let segments = SegmentStore::open(&manifest).expect("the segments open");
            named += manifest.manifest().unwrap().files().len();
            let mut writer = segments.create().expect("a segment is created");
            segments.commit(&mut manifest).expect("it is named");
            append_blocks(&mut writer, 2, 1);
        }
        assert!(
            named > 0,
            "no state kept the manifest that names the segment"
        );
    }
}

/// A database with one segment that a durable manifest names.
struct OneSegment {
    fs: SimVfs,
    manifest: ManifestStore,
    segments: SegmentStore,
    writer: SegmentWriter,
}

impl OneSegment {
    fn new(seed: u64) -> Self {
        let fs = sim_fs(SimConfig::new(seed));
        let mut manifest = created(&fs, empty_manifest());
        let segments = SegmentStore::open(&manifest).expect("the segments open");
        let writer = segments.create().expect("a segment is created");
        segments.commit(&mut manifest).expect("it is named");
        Self {
            fs,
            manifest,
            segments,
            writer,
        }
    }

    fn commit(&mut self) {
        self.segments
            .commit(&mut self.manifest)
            .expect("the manifest is replaced");
    }

    fn append_one(&mut self) -> Result<BlockLocation, SegmentStoreError> {
        self.writer
            .append(page(1), BlockContent::Delta, Compression::Raw, b"one more")
    }

    /// Checks that after any crash now, the database opens with segment 1
    /// named as `named` and holding exactly `blocks`.
    #[track_caller]
    fn assert_any_crash_leaves(&self, named: LiveFile, blocks: &[Appended]) {
        let expected: Vec<_> = blocks.iter().map(|block| block.payload.clone()).collect();
        let image = self.fs.crash_image();
        for (plan, state) in crash_states(&image, &SOME_STATES).states {
            let context = || format!("{image:?}\n{plan:?}");
            let _context = OnFailure(&context);
            let manifest =
                ManifestStore::open(shared(&state), db_dir()).expect("the manifest opens");
            assert_eq!(manifest.manifest().unwrap().files(), [named]);
            let segments = SegmentStore::open(&manifest).expect("the segments open");
            let mut walk = segments.walk(id(1), MAX).expect("the segment is walked");
            let mut payloads = Vec::new();
            while let Some((_, block)) = walk.next_block().expect("a whole block") {
                payloads.push(block.payload().expect("decompresses").into_owned());
            }
            assert_eq!(payloads, expected);
        }
    }
}

fn is_failed(error: SegmentStoreError) -> bool {
    matches!(error, SegmentStoreError::SegmentFailed { .. })
}

#[test]
fn a_sync_that_fails_never_lets_a_manifest_name_what_it_did_not_sync() {
    type Sync = fn(&mut OneSegment) -> Result<(), SegmentStoreError>;
    let syncs: [Sync; 3] = [
        |db| db.writer.sync().map(drop),
        |db| db.segments.sync(),
        |db| db.writer.seal().map(drop),
    ];
    for fault in FAULTS {
        for sync in syncs {
            let mut db = OneSegment::new(71);
            let synced = append_blocks(&mut db.writer, 4, 1);
            let durable = db.writer.sync().unwrap();
            db.commit();
            let in_limbo = append_blocks(&mut db.writer, 4, 20);

            db.fs.add_rule(FaultRule::new(fault).on(OpKind::SyncData));
            let error = sync(&mut db).unwrap_err();
            assert_eq!(injected_op(&error).map(|(fault, _)| fault), Some(fault));
            assert!(is_failed(db.append_one().unwrap_err()));
            assert!(is_failed(db.writer.seal().unwrap_err()));
            assert!(is_failed(db.segments.sync().unwrap_err()));
            let mut buf = Vec::new();
            for block in synced.iter().chain(&in_limbo) {
                let read = db.segments.read(block.location, MAX, &mut buf).unwrap();
                assert_eq!(read.payload().unwrap(), block.payload.as_slice());
            }

            db.commit();
            db.assert_any_crash_leaves(segment(1, FileState::Open, durable), &synced);

            let OneSegment {
                fs,
                mut manifest,
                segments,
                writer,
            } = db;
            drop(writer);
            segments.sync().expect("nothing is left to sync");
            segments.commit(&mut manifest).unwrap();
            let db = OneSegment {
                writer: segments.create().expect("another segment"),
                fs,
                manifest,
                segments,
            };
            let closed = segment(1, FileState::Closed, durable);
            assert_eq!(db.manifest.manifest().unwrap().files(), [closed]);
            db.assert_any_crash_leaves(closed, &synced);
        }
    }
}

#[test]
fn an_append_that_is_taken_back_leaves_the_segment_as_it_was() {
    for fault in FAULTS {
        let mut db = OneSegment::new(72);
        let mut blocks = append_blocks(&mut db.writer, 4, 1);
        let durable = db.writer.sync().unwrap();
        db.commit();
        blocks.extend(append_blocks(&mut db.writer, 2, 30));
        let end = db.writer.end();

        let rule = FaultRule::new(fault).on(OpKind::Append).torn_append(false);
        db.fs.add_rule(rule);
        let error = db
            .writer
            .append(page(2), BlockContent::Base, Compression::Raw, &[7; 3000])
            .unwrap_err();
        assert_eq!(injected_op(&error).map(|(fault, _)| fault), Some(fault));
        assert_eq!(db.writer.end(), end);
        db.assert_any_crash_leaves(segment(1, FileState::Open, durable), &blocks[..4]);

        let location = db.append_one().expect("the writer goes on");
        assert_eq!(location.offset, end);
        blocks.push(Appended {
            location,
            page: page(1),
            content: BlockContent::Delta,
            payload: b"one more".to_vec(),
        });
        let sealed_len = db.writer.seal().expect("the segment is sealed");
        db.commit();
        db.assert_any_crash_leaves(segment(1, FileState::Sealed, sealed_len), &blocks);
    }
}

#[test]
fn an_append_that_cannot_be_taken_back_ends_the_writer_and_keeps_the_blocks_before_it() {
    for fault in FAULTS {
        let mut db = OneSegment::new(73);
        let blocks = append_blocks(&mut db.writer, 5, 1);
        let end = db.writer.end();

        let rule = FaultRule::new(fault).on(OpKind::Append).torn_append(true);
        db.fs.add_rule(rule);
        let error = db
            .writer
            .append(page(2), BlockContent::Base, Compression::Raw, &[7; 3000])
            .unwrap_err();
        assert_eq!(injected_op(&error).map(|(fault, _)| fault), Some(fault));
        for error in [db.append_one().unwrap_err(), db.writer.seal().unwrap_err()] {
            assert!(matches!(error, SegmentStoreError::Io { .. }), "{error}");
            assert_eq!(injected_op(&error), None, "the handle appends no more");
        }
        assert_eq!(db.writer.end(), end);

        db.segments.sync().expect("the blocks before it are synced");
        db.commit();
        db.assert_any_crash_leaves(segment(1, FileState::Open, end), &blocks);

        let OneSegment {
            fs,
            manifest,
            segments,
            writer,
        } = db;
        drop((writer, segments, manifest));
        let mut manifest = ManifestStore::open(shared(&fs), db_dir()).expect("the manifest opens");
        let segments = SegmentStore::open(&manifest).expect("the segments open");
        segments.commit(&mut manifest).unwrap();
        let db = OneSegment {
            writer: segments.create().expect("another segment"),
            fs,
            manifest,
            segments,
        };
        db.assert_any_crash_leaves(segment(1, FileState::Closed, end), &blocks);
    }
}
