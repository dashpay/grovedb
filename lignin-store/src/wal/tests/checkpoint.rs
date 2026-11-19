//! The log and the page-table checkpoint: what `TableCheckpoints` asks of
//! the log (K14 of `page-table.md`), with the real table. The log says
//! where it ends and how far it is durable, and keeps its files from the
//! place of the sample of every checkpoint that a manifest names or may
//! come to name, through a deletion and a crash.

use lignin_io::sim::{Fault, FaultRule, OpKind};

use super::*;
use crate::{
    segment::{SegmentStore, SegmentStoreError},
    table::{CheckpointInfo, CountSample, PageTable, PageTableError, TableCheckpoints},
};

const PLANS: sim::Plans = sim::Plans {
    every_plan_limit: 400,
    samples: 16,
};

#[derive(Debug, Default)]
struct Seen {
    states: u64,
    with_a_checkpoint: u64,
    whole_log: u64,
}

/// Recovers every state a crash at this moment can leave, as far as the
/// log goes: the manifest's checkpoint loads, the log reads and holds what
/// the model says, and the log file that the checkpoint's sample names is
/// there from its start, with the checkpoint's mark ending at the place.
fn check_crash_states(run: &Run, seen: &mut Seen) {
    let image = run.fs.crash_image();
    for (plan, state) in &sim::crash_states(&image, &PLANS).states {
        let at = || format!("{}, plan {plan:?}", run.at());
        let manifest = sim::manifest_in(state).expect("a manifest");
        let (_, info) = PageTable::load(state, sim::db_dir(), &manifest)
            .unwrap_or_else(|error| panic!("{}: {error}", at()));
        let (items, end) = read_log(&sim::shared(state), sim::db_dir())
            .unwrap_or_else(|error| panic!("{}: {error}", at()));
        if let Err(wrong) = run.model.check_in(state, &items, false) {
            panic!("{}: {wrong}", at());
        }
        seen.states += 1;
        let sample = info.and_then(|info| Some((info.log_kept_from()?, info.covers())));
        let Some((place, covers)) = sample else {
            // No checkpoint: the whole log is there.
            let first = run.model.items.first().map(|item| item.at);
            if run.model.durable > 0 {
                assert_eq!(items.first().map(|item| item.at), first, "{}", at());
            }
            seen.whole_log += 1;
            continue;
        };
        // The file of the place is part of the log, and is read from its
        // first item.
        let kept = end.files().iter().find(|file| file.file() == place.file());
        let kept = kept.unwrap_or_else(|| {
            panic!(
                "{}: the log file of the sample's place {place:?} is gone",
                at()
            )
        });
        assert!(kept.valid_len() >= place.offset(), "{}", at());
        let first = items.iter().find(|item| item.at.file() == place.file());
        assert_eq!(first.map(|item| item.at.offset()), Some(FIRST), "{}", at());
        // The item that ends at the place is the mark, and it covers what
        // the checkpoint covers.
        let marked = items.iter().find(|item| item.end == place);
        let marked = marked.unwrap_or_else(|| panic!("{}: no item ends at {place:?}", at()));
        assert_eq!(marked.bytes, mark(covers), "{}", at());
        seen.with_a_checkpoint += 1;
    }
}

/// Appends items until the log has turned to the file `file`, and looks at
/// what a crash after each of them leaves.
fn fill_to(run: &mut Run, file: u32, seen: &mut Seen) {
    while run.db.as_ref().unwrap().log.end().unwrap().file() < file_id(file) {
        run.append();
        check_crash_states(run, seen);
        if run.db.as_ref().unwrap().log.is_due() {
            run.roll();
        }
    }
}

/// Takes the sample of a checkpoint as write-back does: the mark, with the
/// hold from its end. The mark is the last item of its file, which then
/// holds no commit the checkpoint does not cover.
fn sample(run: &mut Run) -> (CountSample, CommitVersion, LogHold) {
    // A roll leaves a file ready to turn to.
    run.roll();
    while !run.db.as_ref().unwrap().log.is_due() {
        run.append();
    }
    let db = run.db.as_ref().unwrap();
    let covers = db.log.latest();
    let appended = db.log.append_mark(covers).unwrap();
    run.model
        .items
        .push(Item::appended(appended, mark(covers), None));
    let hold = db.log.hold_from(Some(appended.end)).unwrap();
    assert!(db.log.rotate().unwrap());
    assert_eq!(
        db.log.end(),
        Some(place(appended.end.file().get() + 1, FIRST))
    );
    // A commit behind the sample, which the copy of the checkpoint may hold
    // a change of.
    run.append();
    let sample = CountSample {
        place: Some(appended.end),
        counts: Vec::new(),
    };
    (sample, covers, hold)
}

/// Syncs the log, drops every file the log lets go of when nothing before
/// its end is asked for, and replaces the manifest.
fn delete(run: &mut Run) -> Vec<FileId> {
    let db = run.db.as_mut().unwrap();
    db.log.sync().unwrap();
    let dropped = db.log.drop_before(db.log.end().unwrap()).unwrap();
    if let Some(last) = dropped.last() {
        let kept = |item: &Item| item.at.file() > *last;
        let first = run.model.items.iter().position(kept);
        run.model.first = first.unwrap_or(run.model.items.len());
    }
    let committed = run.with_manifest(|db| db.log.commit(&mut db.manifest));
    committed.unwrap().unwrap();
    run.note_durable();
    dropped
}

fn on_disk(run: &Run, file: FileId) -> bool {
    let path = FileNames::WAL.path(sim::db_dir(), file);
    run.fs.open(&path, lignin_io::OpenMode::Read).is_ok()
}

/// What a session has of the table: its stores, and the checkpoint the
/// manifest names with the hold that keeps the log for it.
struct Table {
    segments: SegmentStore,
    checkpoints: TableCheckpoints,
    table: PageTable,
    in_force: LogHold,
}

impl Table {
    /// Opens the table of a session, and takes the hold for the checkpoint
    /// its manifest names: the whole log if it names none.
    fn open(run: &Run) -> Self {
        let db = run.db.as_ref().unwrap();
        let manifest = db.manifest.manifest().unwrap();
        let (table, info) = PageTable::load(&run.fs, sim::db_dir(), manifest).unwrap();
        let kept_from = info.and_then(|info| info.log_kept_from());
        Self {
            segments: SegmentStore::open(&db.manifest).unwrap(),
            checkpoints: TableCheckpoints::open(&db.manifest).unwrap(),
            table,
            in_force: db.log.hold_from(kept_from).unwrap(),
        }
    }

    /// Writes a checkpoint for a sample: the table asks the log for its end
    /// before it copies the first chain and after the last.
    fn write(&self, run: &Run, sample: &CountSample, covers: CommitVersion) -> CheckpointInfo {
        let log = &run.db.as_ref().unwrap().log;
        let written = self
            .checkpoints
            .write(&self.table, covers, sample, || log.end())
            .unwrap();
        assert_eq!(written.log_kept_from(), sample.place);
        assert_eq!(written.log_place_needed(), log.end());
        written
    }

    fn install(&self, run: &mut Run, written: &CheckpointInfo) -> Result<(), PageTableError> {
        let db = run.db.as_mut().unwrap();
        let durable = db.log.durable();
        self.checkpoints
            .install(written, durable, &mut db.manifest, &self.segments)?;
        db.log.manifest_replaced(&db.manifest).unwrap();
        Ok(())
    }
}

#[test]
fn the_log_is_kept_from_the_sample_of_every_checkpoint_through_a_deletion_and_a_crash() {
    let mut seen = Seen::default();
    for seed in 1..=24 {
        let in_doubt_names_the_new_one = seed % 2 == 0;
        let mut run = Run::new(sim::traced(seed, 64), small_files(700), PLANS);
        let mut table = Table::open(&run);
        assert_eq!(table.in_force.place(), None, "no checkpoint yet");

        // No checkpoint is in force: nothing of the log is deleted,
        // whatever is asked for.
        fill_to(&mut run, 4, &mut seen);
        assert_eq!(delete(&mut run), []);
        check_crash_states(&run, &mut seen);

        // The first checkpoint. Its sample is taken at the end of file F,
        // which holds nothing but commits the checkpoint covers.
        let (first_sample, covers, hold) = sample(&mut run);
        let first_place = first_sample.place.unwrap();
        let written = table.write(&run, &first_sample, covers);
        // The log says truthfully how far it is durable, so the checkpoint
        // is not named before the log is synced: through the commits it
        // covers, and through the end of its copy.
        let error = table.install(&mut run, &written).unwrap_err();
        assert!(
            matches!(
                error,
                PageTableError::LogBehind { .. } | PageTableError::LogBehindCopy { .. }
            ),
            "{error}"
        );
        // Written and not yet named: the log is kept for it, and for the
        // state without a checkpoint that is still in force.
        fill_to(&mut run, first_place.file().get() + 3, &mut seen);
        assert_eq!(delete(&mut run), []);
        check_crash_states(&run, &mut seen);
        table.install(&mut run, &written).unwrap();
        table.in_force = hold;
        check_crash_states(&run, &mut seen);

        // A deletion, and then a crash. Every file before F goes. F stays,
        // though the mark is its last item and every commit in it is
        // covered, and so does every file after it.
        let dropped = delete(&mut run);
        let before: Vec<FileId> = (1..first_place.file().get()).map(file_id).collect();
        assert_eq!(dropped, before);
        assert!(dropped.iter().all(|file| !on_disk(&run, *file)));
        assert!(on_disk(&run, first_place.file()));
        check_crash_states(&run, &mut seen);
        for _ in 0..4 {
            run.append();
            check_crash_states(&run, &mut seen);
        }

        // The next session takes the hold again from the checkpoint its
        // manifest names, and deletes nothing more.
        drop(table);
        run.restart();
        let mut table = Table::open(&run);
        assert_eq!(table.in_force.place(), Some(first_place));
        fill_to(&mut run, first_place.file().get() + 6, &mut seen);
        assert_eq!(delete(&mut run), []);
        check_crash_states(&run, &mut seen);

        // A second checkpoint is written and then given up. While it may
        // still be named, the log is kept for both: from the earlier
        // place.
        let (second_sample, covers, hold) = sample(&mut run);
        let written = table.write(&run, &second_sample, covers);
        assert_eq!(delete(&mut run), []);
        let db = run.db.as_ref().unwrap();
        table.checkpoints.discard(written, &db.manifest).unwrap();
        drop(hold);
        assert_eq!(delete(&mut run), [], "the first is still in force");
        check_crash_states(&run, &mut seen);

        // A third checkpoint, whose installation leaves the manifest in
        // doubt: it names the first or the third. The log is kept for
        // both until the manifest has been opened again.
        let (third_sample, covers, hold) = sample(&mut run);
        let third_place = third_sample.place.unwrap();
        let written = table.write(&run, &third_sample, covers);
        run.db.as_ref().unwrap().log.sync().unwrap();
        let (op, on) = match in_doubt_names_the_new_one {
            true => (OpKind::SyncDir, sim::db_dir().to_path_buf()),
            false => (
                OpKind::Rename,
                sim::db_dir().join(ManifestStore::TEMP_FILE_NAME),
            ),
        };
        run.fs.add_rule(FaultRule::new(Fault::Eio).on(op).at(on));
        table.install(&mut run, &written).unwrap_err();
        let db = run.db.as_mut().unwrap();
        assert!(matches!(
            db.manifest.manifest(),
            Err(SegmentStoreError::ManifestInDoubt { .. })
        ));
        let end = db.log.end().unwrap();
        assert_eq!(db.log.drop_before(end).unwrap(), []);
        check_crash_states(&run, &mut seen);

        // The manifest is opened again, and says which one it names.
        let db = run.db.as_mut().unwrap();
        db.manifest = ManifestStore::open(sim::shared(&run.fs), sim::db_dir()).unwrap();
        let named = db.manifest.manifest().unwrap().table_checkpoint().unwrap();
        assert_eq!(named.file() == written.file(), in_doubt_names_the_new_one);
        db.log.manifest_replaced(&db.manifest).unwrap();
        table.checkpoints.manifest_replaced(&db.manifest).unwrap();
        let expected: Vec<FileId> = if in_doubt_names_the_new_one {
            // The third is in force: the log before its sample goes, the
            // file of the first sample with it.
            table.in_force = hold;
            let mut files: Vec<FileId> = run.model.items[run.model.first..]
                .iter()
                .map(|item| item.at.file())
                .filter(|file| *file < third_place.file())
                .collect();
            files.dedup();
            assert_eq!(files.first(), Some(&first_place.file()));
            files
        } else {
            table.checkpoints.discard(written, &db.manifest).unwrap();
            drop(hold);
            Vec::new()
        };
        assert_eq!(delete(&mut run), expected);
        check_crash_states(&run, &mut seen);
        for _ in 0..3 {
            run.append();
            run.roll();
            check_crash_states(&run, &mut seen);
        }
        run.restart();
        let table = Table::open(&run);
        let place = match in_doubt_names_the_new_one {
            true => third_place,
            false => first_place,
        };
        assert_eq!(table.in_force.place(), Some(place));
        assert_eq!(run.stats.other_errors, 0, "{}", run.at());
    }
    println!("{seen:?}");
    assert!(seen.states > 20_000, "{seen:?}");
    assert!(
        seen.with_a_checkpoint > 7_000 && seen.whole_log > 10_000,
        "{seen:?}"
    );
}

#[test]
fn a_checkpoint_asks_the_log_where_it_ends_and_how_far_it_is_durable() {
    let mut run = Run::new(sim::traced(21, 64), LogOptions::default(), PLANS);
    let table = Table::open(&run);
    for _ in 0..5 {
        run.append();
    }
    let log = &run.db.as_ref().unwrap().log;

    // A sample that names a place the log has not reached is refused: its
    // mark cannot be in the log.
    let end = log.end().unwrap();
    let ahead = CountSample {
        place: Some(place(end.file().get(), end.offset() + 17)),
        counts: Vec::new(),
    };
    let error = table
        .checkpoints
        .write(&table.table, log.latest(), &ahead, || log.end())
        .unwrap_err();
    assert!(
        matches!(error, PageTableError::SampleAhead { log_end, .. } if log_end == Some(end)),
        "{error}"
    );

    // The mark, then the copy. Items appended while the table copies its
    // chains move the end the checkpoint waits for.
    let covers = log.latest();
    let marked = log.append_mark(covers).unwrap();
    let taken = CountSample {
        place: Some(marked.end),
        counts: Vec::new(),
    };
    let asked = std::cell::Cell::new(0);
    let mut rng = Rng(3);
    let during_the_copy = change(&mut rng);
    let written = table
        .checkpoints
        .write(&table.table, covers, &taken, || {
            asked.set(asked.get() + 1);
            if asked.get() == 2 {
                log.append_change(&during_the_copy).unwrap();
            }
            log.end()
        })
        .unwrap();
    assert_eq!(asked.get(), 2, "before the first chain and after the last");
    assert!(written.log_place_needed() > Some(marked.end));
    assert_eq!(written.log_place_needed(), log.end());

    // Durable up to the place of the sample is not enough: the log is
    // synced after the copy, and what that sync returns is what installs
    // the checkpoint.
    let through_sample = log.sync_through(marked.end).unwrap();
    assert_eq!(
        through_sample.place,
        log.end(),
        "a sync covers all there is"
    );
    let db = run.db.as_mut().unwrap();
    db.log.append_change(&change(&mut rng)).unwrap();
    let durable = db.log.sync().unwrap();
    assert_eq!(durable.version, covers);
    assert!(durable.place > written.log_place_needed());
    table
        .checkpoints
        .install(&written, durable, &mut db.manifest, &table.segments)
        .unwrap();
}
