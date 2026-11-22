//! `ENOSPC` and `EIO`, injected by the simulated file system.
//!
//! A step that meets one fails with the injected error inside a
//! [`ValueStoreError`]. Whatever the failure left, a crash at that moment
//! recovers pages, values and counts that agree, the session goes on where
//! the store says it can, and the next session opens the database.

use std::collections::BTreeMap;

use lignin_io::sim::{Fault, FaultRule, OpKind, SimError, SimVfs};

use super::{
    crash::life,
    run::{check_recovered, put, Run},
    sim::*,
    store::{bytes, three_values, Store},
    *,
};

const FAULTS: [Fault; 2] = [Fault::Eio, Fault::Enospc];

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

/// The fault and the number of the operation whose injected failure `error`
/// reports.
fn injected_op(error: &ValueStoreError) -> Option<(Fault, u64)> {
    match SimError::of(io_error(error)?)? {
        SimError::Injected { fault, op, .. } => Some((*fault, *op)),
        SimError::PowerLoss => None,
    }
}

/// What the failures of one workload came to.
#[derive(Debug, Default)]
struct Outcome {
    /// How often each kind of operation was failed.
    kinds: BTreeMap<OpKind, usize>,
    /// Failures after which the session went on.
    went_on: usize,
    /// Failures after which the database had to be opened again.
    reopened: usize,
}

/// Runs `workload` once for every operation of it that `disk` can fail,
/// with the disk failing from that operation on, and checks what the failure
/// leaves: after a crash at that moment, in the session if it can go on, and
/// after the process has ended.
fn fail_everywhere(
    seed: u64,
    disk: Disk,
    workload: impl Fn(&mut Run) -> Result<(), ValueStoreError>,
) -> Outcome {
    let mut outcome = Outcome::default();
    let mut from_op = 1;
    loop {
        let fs = sim_fs(traced(seed, 512));
        fs.add_rule(disk.rule(from_op));
        let mut run = Run::new(&fs, seed);
        let result = workload(&mut run);
        let Some(failed) = failed_ops(&fs).into_iter().next() else {
            result.expect("the workload runs when nothing fails");
            break;
        };
        let context = || format!("seed {seed}, {disk:?}: {failed}");
        let _context = OnFailure(&context);
        let error = result.expect_err("the step that met the fault fails");
        assert_eq!(injected_op(&error), Some((disk.fault(), failed.op)));

        let image = fs.crash_image();
        for (plan, state) in crash_states(&image, &SOME_STATES).states {
            let context = || format!("{image:?}\n{plan:?}");
            let _context = OnFailure(&context);
            check_recovered(&state, &run);
        }

        fs.clear_rules();
        if run.can_go_on() {
            run.drop_writers();
            run.check_session();
            let target = run.steps + 25;
            run.run(target).expect("the session goes on");
            run.check_session();
            run.flush().expect("the journal is synced");
            outcome.went_on += 1;
        } else {
            outcome.reopened += 1;
        }
        run.end_session();
        check_recovered(&fs, &run);

        *outcome.kinds.entry(failed.kind).or_insert(0) += 1;
        from_op = failed.op + 1;
    }
    outcome
}

#[test]
fn a_fault_at_any_operation_fails_the_step_and_loses_nothing() {
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
    for fault in FAULTS {
        let outcome = fail_everywhere(51, Disk::FailsOnce(fault), life);
        println!("{fault:?}: {outcome:?}");
        let failed: Vec<OpKind> = outcome.kinds.keys().copied().collect();
        let expected: &[OpKind] = match fault {
            Fault::Eio => &every_kind,
            Fault::Enospc => &needs_space,
        };
        assert_eq!(failed, expected, "{fault:?}");
        assert!(outcome.went_on > 20 && outcome.reopened > 20, "{outcome:?}");
    }
}

#[test]
fn an_append_that_cannot_be_taken_back_loses_nothing() {
    for fault in FAULTS {
        let outcome = fail_everywhere(52, Disk::TearsAnAppend(fault), life);
        println!("{fault:?}: {outcome:?}");
        assert_eq!(outcome.kinds.keys().collect::<Vec<_>>(), [&OpKind::Append]);
        assert!(outcome.kinds[&OpKind::Append] >= 20, "{outcome:?}");
        assert!(outcome.went_on > 5 && outcome.reopened > 5, "{outcome:?}");
    }
}

#[test]
fn a_disk_that_fills_up_at_any_operation_loses_nothing() {
    let outcome = fail_everywhere(53, Disk::IsFull, life);
    println!("{outcome:?}");
    assert!(outcome.went_on > 20 && outcome.reopened > 20, "{outcome:?}");
}

#[test]
fn a_fault_at_any_operation_of_a_drawn_workload_loses_nothing() {
    for (seed, disk) in [
        (61, Disk::FailsOnce(Fault::Eio)),
        (62, Disk::IsFull),
        (63, Disk::TearsAnAppend(Fault::Enospc)),
    ] {
        let outcome = fail_everywhere(seed, disk, |run| {
            run.open()?;
            run.run(60)
        });
        println!("seed {seed}, {disk:?}: {outcome:?}");
        assert!(outcome.went_on > 5 && outcome.reopened > 5, "{outcome:?}");
    }
}

fn fail(fs: &SimVfs, kind: OpKind, name: &str) {
    let path = FileNames::VALUE_LOG.dir(db_dir()).join(name);
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(kind)
            .at(path)
            .torn_append(false),
    );
}

fn fail_torn(fs: &SimVfs, name: &str) {
    let path = FileNames::VALUE_LOG.dir(db_dir()).join(name);
    let rule = FaultRule::new(Fault::Enospc).on(OpKind::Append);
    fs.add_rule(rule.at(path).torn_append(true));
}

#[test]
fn a_writer_goes_on_after_an_append_that_failed_and_was_taken_back() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let first = writer.append(&mut hold, page(1), b"first").unwrap();
    let end = writer.end();

    fail(&store.fs, OpKind::Append, "00000001.val");
    let error = writer.append(&mut hold, page(1), b"failed").unwrap_err();
    assert!(matches!(
        &error,
        ValueStoreError::Io { op: "append to", path, .. } if path.ends_with("00000001.val")
    ));
    assert_eq!(writer.end(), end);

    let second = writer.append(&mut hold, page(1), b"second").unwrap();
    assert_eq!(second.offset(), end);
    assert_eq!(store.read(first, 1).unwrap(), b"first");
    assert_eq!(store.read(second, 1).unwrap(), b"second");
    assert_eq!(
        writer.finish().unwrap(),
        second.end(lignin_format::FormatVersion::V1)
    );
}

#[test]
fn a_writer_whose_append_could_not_be_taken_back_appends_no_more() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let first = writer.append(&mut hold, page(1), b"first").unwrap();
    writer.sync().unwrap();
    let end = writer.end();

    fail_torn(&store.fs, "00000001.val");
    writer
        .append(&mut hold, page(1), &bytes(3000, 1))
        .unwrap_err();
    assert_eq!(writer.end(), end);
    assert!(matches!(
        writer.append(&mut hold, page(1), b"again"),
        Err(ValueStoreError::Io {
            op: "append to",
            ..
        })
    ));
    assert_eq!(writer.finish().unwrap(), end);
    store.commit();
    assert_eq!(store.named(), [value_file(1, FileState::Closed, end)]);
    assert_eq!(store.read(first, 1).unwrap(), b"first");

    let mut next = store.writer();
    let second = next.append(&mut hold, page(1), b"elsewhere").unwrap();
    assert_eq!(second.file(), file_id(2));
    drop(hold);
    let store = store.reopen(&[]);
    assert_eq!(store.read(first, 1).unwrap(), b"first");
}

#[test]
fn a_file_whose_sync_failed_ends_where_it_was_last_synced() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let synced = writer.append(&mut hold, page(1), b"synced").unwrap();
    let durable = writer.sync().unwrap();
    let unsynced = writer.append(&mut hold, page(1), b"not synced").unwrap();

    fail(&store.fs, OpKind::SyncData, "00000001.val");
    assert!(matches!(
        writer.sync(),
        Err(ValueStoreError::Io { op: "sync", .. })
    ));
    for _ in 0..2 {
        assert!(matches!(
            writer.sync(),
            Err(ValueStoreError::FileFailed { file }) if file == file_id(1)
        ));
    }
    assert!(matches!(
        writer.append(&mut hold, page(1), b"more"),
        Err(ValueStoreError::FileFailed { .. })
    ));
    assert!(matches!(
        store.values.sync(),
        Err(ValueStoreError::FileFailed { .. })
    ));
    assert_eq!(
        store.read(unsynced, 1).unwrap(),
        b"not synced",
        "readable while the writer is there to append it again elsewhere"
    );
    store.commit();
    assert_eq!(store.named(), [value_file(1, FileState::Open, durable)]);

    drop(writer);
    assert!(matches!(
        store.read(unsynced, 1),
        Err(ValueStoreError::OutOfBounds { end, .. }) if end == durable
    ));
    assert_eq!(store.read(synced, 1).unwrap(), b"synced");
    store.values.sync().unwrap();
    store.commit();
    assert_eq!(store.named(), [value_file(1, FileState::Closed, durable)]);
}

/// An append to a file and a sync of that file are in one order: the
/// file layer has answered an append that the file admitted before a
/// sync of the file that fails begins, and no append is handed to it for
/// the file after that sync. Here another thread syncs the value log
/// while the writer stands between the admission of its append and the
/// file layer: the sync waits for the append, which the device takes.
#[test]
fn an_append_that_a_file_admitted_is_answered_before_a_sync_of_the_file_that_fails() {
    use std::sync::mpsc;

    use lignin_io::sim::{Event, Outcome as Ended};

    use crate::values::file::hooks::{self, Point};

    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    writer.append(&mut hold, page(1), b"dirty").unwrap();
    fail(&store.fs, OpKind::SyncData, "00000001.val");
    let before = store.fs.stats().ops;
    let on_the_file = |fs: &SimVfs| -> Vec<(OpKind, bool)> {
        let ops = fs.trace().into_iter().filter_map(|event| match event {
            Event::Op(op) if op.op > before && op.path.ends_with("00000001.val") => Some(op),
            _ => None,
        });
        ops.filter(|op| matches!(op.kind, OpKind::Append | OpKind::SyncData))
            .map(|op| (op.kind, op.outcome == Ended::Ok))
            .collect()
    };

    let values = &store.values;
    let (go, gone) = mpsc::channel();
    let (tell, told) = mpsc::channel();
    let admitted = std::thread::scope(|scope| {
        let waits = tell.clone();
        scope.spawn(move || {
            gone.recv().expect("the writer stands at its append");
            hooks::set(Point::Waits, move || {
                waits.send("waits").expect("the writer")
            });
            let synced = values.sync();
            assert!(
                matches!(synced, Err(ValueStoreError::Io { op: "sync", .. })),
                "{synced:?}"
            );
            // Nobody listens any more where the sync waited.
            let _ = tell.send("returned");
        });
        hooks::set(Point::Admitted, move || {
            go.send(()).expect("the syncing thread");
            let heard = told.recv().expect("the syncing thread");
            assert_eq!(heard, "waits", "the sync did not wait for the append");
        });
        writer.append(&mut hold, page(1), b"admitted")
    });
    admitted.expect("the append was admitted before the sync failed");
    assert_eq!(
        on_the_file(&store.fs),
        [(OpKind::Append, true), (OpKind::SyncData, false)],
        "the append is in the file before the sync that failed"
    );
    assert!(matches!(
        writer.append(&mut hold, page(1), b"more"),
        Err(ValueStoreError::FileFailed { .. })
    ));
    assert!(matches!(
        store.values.sync(),
        Err(ValueStoreError::FileFailed { .. })
    ));
    let since = on_the_file(&store.fs).len() - 2;
    assert_eq!(since, 0, "appends and syncs that reached the file since");
}

#[test]
fn a_finish_whose_sync_fails_closes_the_file_where_it_was_last_synced() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let synced = writer.append(&mut hold, page(1), b"synced").unwrap();
    let durable = writer.sync().unwrap();
    writer.append(&mut hold, page(1), b"not synced").unwrap();
    fail(&store.fs, OpKind::SyncData, "00000001.val");
    assert!(matches!(
        writer.finish(),
        Err(ValueStoreError::Io { op: "sync", .. })
    ));
    store.commit();
    assert_eq!(store.named(), [value_file(1, FileState::Closed, durable)]);
    assert_eq!(store.read(synced, 1).unwrap(), b"synced");
}

#[test]
fn a_store_wide_sync_goes_on_past_a_file_that_fails() {
    let mut store = Store::new();
    let (mut first, mut second) = (store.values.create().unwrap(), store.writer());
    let mut hold = ValueHold::new();
    first.append(&mut hold, page(1), b"in the first").unwrap();
    second.append(&mut hold, page(1), b"in the second").unwrap();
    fail(&store.fs, OpKind::SyncData, "00000001.val");
    assert!(matches!(
        store.values.sync(),
        Err(ValueStoreError::Io { op: "sync", path, .. }) if path.ends_with("00000001.val")
    ));
    store.commit();
    assert_eq!(
        store.named(),
        [
            value_file(1, FileState::Open, 20),
            value_file(2, FileState::Open, second.end())
        ]
    );
}

#[test]
fn a_file_that_cannot_be_created_leaves_nothing_and_its_id_unused() {
    let store = Store::new();
    for kind in [OpKind::Open, OpKind::Append, OpKind::SyncData] {
        let before = store.values.files_named().unwrap().len();
        fail(&store.fs, kind, "00000001.val");
        fail(&store.fs, kind, "00000002.val");
        fail(&store.fs, kind, "00000003.val");
        let error = store.values.create().unwrap_err();
        assert!(matches!(error, ValueStoreError::Io { .. }), "{error}");
        assert_eq!(store.values.files_named().unwrap().len(), before);
        store.fs.clear_rules();
    }
    for raw_id in 1..=3 {
        assert!(!store.exists(raw_id));
    }
    let writer = store.values.create().unwrap();
    assert_eq!(writer.file_id(), file_id(4));

    let path = FileNames::VALUE_LOG.dir(db_dir()).join("00000005.val");
    let rule = FaultRule::new(Fault::Eio).on(OpKind::Open).at(path);
    store.fs.add_rule(rule.skip(1));
    assert!(matches!(
        store.values.create(),
        Err(ValueStoreError::Io { op: "open", .. })
    ));
    assert!(
        !store.exists(5),
        "the file is removed when it cannot be opened for reading"
    );
}

#[test]
fn a_dropped_file_that_cannot_be_deleted_is_deleted_by_the_next_call() {
    let mut store = Store::new();
    let mut hold = ValueHold::new();
    for _ in 0..2 {
        let mut writer = store.writer();
        writer.append(&mut hold, page(1), b"dead").unwrap();
        writer.finish().unwrap();
    }
    drop(hold);
    store.commit();
    store.all_durable();
    let dropped = store
        .values
        .drop_empty(store.values.retire(version(0)))
        .unwrap();
    assert_eq!(dropped, [file_id(1), file_id(2)]);

    fail(&store.fs, OpKind::RemoveFile, "00000001.val");
    let error = store.values.commit(&mut store.manifest).unwrap_err();
    assert!(matches!(
        &error,
        ValueStoreError::NotDeleted { path, .. } if path.ends_with("00000001.val")
    ));
    assert!(io_error(&error).is_some());
    assert!(store.named().is_empty(), "the new manifest is in force");
    assert!(store.exists(1) && !store.exists(2));

    store.values.manifest_replaced(&store.manifest).unwrap();
    assert!(!store.exists(1));
}

#[test]
fn a_pass_whose_copies_cannot_be_made_durable_installs_nothing() {
    for failing in [OpKind::Append, OpKind::SyncData, OpKind::Rename] {
        let (mut store, mut cleaner, pointers) = three_values(3);
        match failing {
            OpKind::Rename => {
                let rule = FaultRule::new(Fault::Eio).on(OpKind::Rename);
                store.fs.add_rule(rule);
            }
            kind => fail(&store.fs, kind, "00000002.val"),
        }
        let error = store
            .values
            .clean(file_id(1), &mut cleaner, &mut store.manifest, &Everything)
            .unwrap_err();
        assert!(io_error(&error).is_some() || matches!(error, ValueStoreError::Manifest(_)));
        assert_eq!(live(&store.values, 1), Some(3), "{failing:?}");
        assert_eq!(live(&store.values, 2), Some(0));
        for (at, &pointer) in pointers.iter().enumerate() {
            assert_eq!(store.read(pointer, 1).unwrap(), bytes(30, at as u8));
        }
    }
}

#[test]
fn a_transaction_whose_value_a_failed_sync_cut_off_is_not_written_to_the_journal() {
    let fs = sim_fs(traced(54, 512));
    let mut run = Run::started(&fs, 54);
    run.commit(&[put(1, 10, 40)]).unwrap();
    let (outstanding, next) = run.begin(&[put(3, 30, 60)]).unwrap();

    fail(&fs, OpKind::SyncData, "00000001.val");
    let error = run.commit(&[put(5, 50, 20)]).unwrap_err();
    assert!(
        matches!(error, ValueStoreError::Io { op: "sync", .. }),
        "{error}"
    );
    fs.clear_rules();
    run.drop_writers();
    run.check_session();

    let error = run.end(outstanding, next, true).unwrap_err();
    assert!(
        matches!(error, ValueStoreError::NotCovered { pointer } if pointer.file() == file_id(1)),
        "{error}"
    );
    assert_eq!(run.db().pages.latest(), 1);
    run.check_session();
    run.commit(&[put(3, 30, 61)]).unwrap();
    run.flush().unwrap();
    run.check_session();
    run.end_session();
    assert_eq!(check_recovered(&fs, &run).pages.latest(), 2);
}
