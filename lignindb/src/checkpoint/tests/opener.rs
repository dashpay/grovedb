//! What the engine's open makes of a creation that the power cut (P-19).
//!
//! Platform opens a checkpoint with the open that makes a database where
//! it finds none ([`Db::open`](crate::Db::open) with
//! [`Options::create_if_missing`]), and that open takes a directory that
//! is empty, or holds a lock file alone, for a place to make one in. So
//! nothing but the whole checkpoint may ever have the name of a target: a
//! directory that a creation left there half made would be opened as an
//! empty database.
//!
//! The database of these tests is made by the engine, so that the engine
//! opens its checkpoints. A [`Checkpointer`](crate::checkpoint::Checkpointer)
//! is made of a commit path, which the engine's handle does not give out,
//! so the directory is opened a second time as the store of the write
//! transaction's tests, once the engine has closed it.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::CommitVersion;
use lignin_io::sim::SimVfs;
use lignin_store::{flush::FlushConfig, wal::LogOptions};

use super::{
    building, checkpointer,
    crash::{plans, Files, Left},
    gone, sim, DIR, NO_CATCH_UP,
};
use crate::{
    checkpoint,
    commit::{Committed, NoStaleHashes},
    snapshot::Versions,
    txn::{
        tests::store::{Db, SIZE},
        Batch, Column, RecordWrite, Subtree, SubtreeWrite,
    },
    Options,
};

type Records = BTreeMap<Vec<u8>, Vec<u8>>;

fn subtree() -> Subtree {
    Subtree::new([7; 32])
}

/// How the engine opens a directory of `fs`: with the defaults, which make
/// a database where there is none.
fn options(fs: &SimVfs) -> Options {
    let options = Options {
        vfs: Arc::new(fs.clone()),
        page_size: Some(SIZE),
        ..Options::default()
    };
    assert!(options.create_if_missing);
    options
}

/// A database that the engine made in [`DIR`] of a simulated file system
/// and closed, with the records of the aux column of one subtree and the
/// version it was closed at.
fn engine_made(seed: u64) -> (SimVfs, Records, CommitVersion) {
    let fs = sim(seed);
    let db = crate::Db::open(DIR, &options(&fs)).expect("the engine makes a database");
    assert!(db.created());
    let mut records = Records::new();
    for round in 0..6u8 {
        let txn = db.begin();
        let scope = txn.scope().expect("a scope");
        let enter = [SubtreeWrite::Enter {
            subtree: subtree(),
            tree_type: b"tree",
            host: None,
        }];
        let written: Vec<(Vec<u8>, Vec<u8>)> = (0..8u8)
            .map(|at| {
                let key = format!("key-{round}-{at}").into_bytes();
                // Some values are long enough to lie in the value log.
                let len = 40 + 90 * usize::from(at);
                (key, vec![round ^ at; len])
            })
            .collect();
        let puts: Vec<RecordWrite<'_>> = (written.iter())
            .map(|(key, value)| RecordWrite::put(Column::Aux, subtree(), key, value))
            .collect();
        scope
            .apply(&Batch {
                subtrees: if round == 0 { &enter } else { &[] },
                records: &puts,
                ..Batch::default()
            })
            .expect("the records are applied");
        scope.merge();
        let committed = db.commit(&txn, &mut NoStaleHashes).expect("a commit");
        assert!(
            matches!(committed, Committed::Published(_)),
            "{committed:?}"
        );
        drop(txn);
        records.extend(written);
    }
    let version = db.version();
    db.close().expect("the engine closes the database");
    (fs, records, version)
}

/// The database in [`DIR`] of `fs`, opened as the store of these tests
/// opens any directory, on a file layer that links or copies as `files`
/// says. Its checkpoints are of the engine's database.
///
/// The store is taken out of the check at its drop: the checker would
/// look for the keyspaces where the tests' own stores have them, and this
/// one has the engine's. The engine reads what it holds, below.
fn store_of(fs: &SimVfs, files: Files) -> (Arc<Db>, Versions) {
    let vfs = files.on(Arc::new(fs.clone()));
    let (like, _) = Db::new(16);
    let (options, flush) = (LogOptions::default(), FlushConfig::new(SIZE));
    let (db, versions, _) =
        Db::open(vfs, Path::new(DIR), 256, options, flush, &like).expect("the store opens");
    db.unchecked();
    (db, versions)
}

/// Opens `target` in `state` with the engine's open that makes a database
/// where it finds none, and checks that what it opened is the checkpoint:
/// not made by this open, at `version`, with `records`.
fn opens_as_the_checkpoint(
    state: &SimVfs,
    target: &Path,
    version: CommitVersion,
    records: &Records,
    context: &str,
) {
    let opened = crate::Db::open(target, &options(state))
        .unwrap_or_else(|error| panic!("{context}: the engine's open: {error}"));
    assert!(!opened.created(), "{context}: the open made a database");
    assert_eq!(opened.version(), version, "{context}");
    {
        let snapshot = opened.snapshot();
        let operation = snapshot.operation();
        let read = (operation.open(subtree()))
            .unwrap_or_else(|error| panic!("{context}: the subtree: {error}"))
            .unwrap_or_else(|| panic!("{context}: the subtree is not entered"));
        let mut value = Vec::new();
        for (key, held) in records {
            value.clear();
            let found = (read.get(Column::Aux, key, &mut value))
                .unwrap_or_else(|error| panic!("{context}: a record: {error}"));
            assert!(
                found && value == *held,
                "{context}: {:?}",
                String::from_utf8_lossy(key)
            );
        }
    }
    opened
        .close()
        .unwrap_or_else(|error| panic!("{context}: the close: {error}"));
}

fn every_state_opens_as_the_checkpoint_or_has_nothing(files: Files) {
    let (fs, records, version) = engine_made(91);
    // Each cut begins from a copy of the file system as the engine left
    // it: the same state, whatever the engine's own threads did when.
    let copy = || {
        let image = fs.crash_image();
        image.recover(&image.keep_all()).expect("a state")
    };
    let target = Path::new(DIR).join("checkpoints").join("1");
    let begin = |fs: &SimVfs| -> (Arc<Db>, Versions, PathBuf, u64) {
        let (db, versions) = store_of(fs, files);
        assert_eq!(versions.latest(), version);
        let made = super::checkpoints_in(&db).join("1");
        assert_eq!(made, target);
        let from = fs.stats().ops;
        (db, versions, made, from)
    };

    let whole = copy();
    let (db, versions, target, from) = begin(&whole);
    let created = checkpointer(&db, &versions)
        .with_options(NO_CATCH_UP)
        .create(&target)
        .expect("a checkpoint");
    assert_eq!(created.version, version);
    assert_eq!(created.copied > 0, files == Files::Copied, "{created:?}");
    let ops = whole.stats().ops - from;
    assert!(ops > 12, "a creation of {ops} operations");
    drop((db, versions));
    opens_as_the_checkpoint(&whole, &target, version, &records, "no cut");

    let mut seen = Left::default();
    for cut in 1..=ops {
        let fs = copy();
        let (db, versions, target, began) = begin(&fs);
        assert_eq!(began, from, "the same store");
        fs.crash_at(lignin_io::sim::CrashPoint::AfterOp(from + cut));
        let created = checkpointer(&db, &versions)
            .with_options(NO_CATCH_UP)
            .create(&target);
        assert_eq!(
            created.is_ok(),
            cut == ops,
            "cut {cut} of {ops}: {created:?}"
        );
        drop((db, versions));
        let image = fs.crash_image();
        for (index, plan) in plans(&image).iter().enumerate() {
            let Ok(state) = image.recover(plan) else {
                continue;
            };
            let context = format!("{files:?}, cut {cut} of {ops}, crash plan {index}");
            seen.built += usize::from(!gone(&state, &building(&target)));
            if gone(&state, &target) {
                // What was at that name before: nothing.
                assert!(created.is_err(), "{context}: the checkpoint is gone");
                seen.none += 1;
            } else {
                opens_as_the_checkpoint(&state, &target, version, &records, &context);
                seen.whole += 1;
            }
            if index < 2 {
                // What is left goes with a deletion of the target, and
                // the engine then makes a database of its own there.
                match checkpoint::delete(&state, &target) {
                    Ok(()) | Err(checkpoint::CheckpointError::NotADirectory { .. }) => {}
                    Err(error) => panic!("{context}: the deletion: {error}"),
                }
                assert!(gone(&state, &building(&target)), "{context}");
                let made = crate::Db::open(&target, &options(&state))
                    .unwrap_or_else(|error| panic!("{context}: a new database: {error}"));
                assert!(made.created(), "{context}");
            }
        }
    }
    println!(
        "{files:?}: a creation of {ops} operations, cut after each; of the states that the \
         engine's open was given, {} had the checkpoint at the target and {} nothing there, and \
         {} had something under the name it was built under",
        seen.whole, seen.none, seen.built
    );
    assert!(
        seen.whole > 0 && seen.none > 0 && seen.built > 0,
        "{files:?}: {seen:?}"
    );
}

/// **No state that a cut creation leaves at the name of its target is
/// opened as an empty database.** The power is cut after each operation of
/// a creation, and each state that is looked at of those the crash can
/// leave is opened at the target with the engine's open that makes a
/// database where it finds none. Either nothing has the name of the
/// target, which is what was there before, or the open finds the
/// checkpoint: it makes no database, lands on the checkpoint's version and
/// reads every record of it.
#[test]
fn a_cut_creation_leaves_the_checkpoint_or_nothing_for_the_open_that_creates() {
    every_state_opens_as_the_checkpoint_or_has_nothing(Files::Linked);
}

/// The same for a checkpoint whose files are copied.
#[test]
fn a_cut_copying_creation_leaves_the_checkpoint_or_nothing_for_the_open_that_creates() {
    every_state_opens_as_the_checkpoint_or_has_nothing(Files::Copied);
}
