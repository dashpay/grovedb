//! The acceptance tests of TXN-05, one for each sentence of its "Done
//! when", and the run the owner asked for beside them: commits back to
//! back with write-back at work.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use lignin_io::{sim::OpKind, Vfs};
use lignin_store::{
    flush::FlushConfig,
    wal::{LogOptions, SyncPolicy},
};

use super::{
    aux, changes_since, counted, groups, pointers_held, sim, store_on, store_with, writes_of, DIR,
};
use crate::{
    commit::{CommitError, Committed, NoStaleHashes},
    txn::{
        tests::{
            delete,
            model::{self, OwnedBatch},
            put,
            store::{Outcome, Rng, SIZE},
            Tx, World,
        },
        Column, Unit,
    },
};

/// The root Merk of the grove of these tests, the Merk of the documents
/// and the Merk of the references to them.
const ROOT: u8 = 30;
const DOCUMENTS: u8 = 31;
const REFERENCES: u8 = 32;

type Items = BTreeMap<Vec<u8>, Vec<u8>>;

/// What the element of a tree holds of the Merk below it.
fn element(items: &Items) -> Vec<u8> {
    let parts: Vec<&[u8]> = items
        .iter()
        .flat_map(|(key, value)| [&key[..], &value[..]])
        .collect();
    model::hash_of(&parts).to_vec()
}

/// The batch of one operation that changes the documents and the
/// references: the change sets of the two Merks, that of the root Merk,
/// whose elements change with them, and the base root key if the root
/// Merk's root changes (`transactions.md` 14.1).
fn grove_batch<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    documents: impl FnOnce(&mut Items),
    references: impl FnOnce(&mut Items),
) -> OwnedBatch {
    let (mut held_documents, mut held_references) = (Vec::new(), Vec::new());
    let changed_documents = world.merk(tx, DOCUMENTS, |items| {
        documents(items);
        held_documents = element(items);
    });
    let changed_references = world.merk(tx, REFERENCES, |items| {
        references(items);
        held_references = element(items);
    });
    let root = world.merk(tx, ROOT, |items| {
        items.insert(b"documents".to_vec(), held_documents);
        items.insert(b"references".to_vec(), held_references);
    });
    let records = World::root_record(&root).into_iter().collect();
    OwnedBatch {
        merks: vec![changed_documents, changed_references, root],
        records,
        ..OwnedBatch::default()
    }
}

fn document_key(serial: u64) -> Vec<u8> {
    let scattered = model::hash_of(&[&serial.to_be_bytes()]);
    [&scattered[..], &serial.to_be_bytes()[..]].concat()
}

fn reference_key(serial: u64) -> Vec<u8> {
    format!(
        "by-age/{:04}/{serial:08}",
        model::hash_of(&[b"age", &serial.to_le_bytes()])[0]
    )
    .into_bytes()
}

/// One state transition of the reference workload: a document of 400
/// bytes, which is stored out of line, and a reference to it.
fn transition<'s>(world: &World<'s>, tx: &Tx<'s>, rng: &mut Rng, serial: u64) {
    let (key, document) = (document_key(serial), model::value(rng, 400));
    let batch = grove_batch(
        world,
        tx,
        |items| {
            items.insert(key.clone(), document);
        },
        |items| {
            items.insert(reference_key(serial), key.clone());
        },
    );
    world.apply(tx, &batch).expect("a transition is applied");
}

/// Commits `count` documents from `first` on, with their references, as
/// one operation of one transaction: what fills a store before a test.
fn block<'s>(world: &mut World<'s>, rng: &mut Rng, first: u64, count: u64) {
    let tx = world.begin();
    let serials = first..first + count;
    let batch = grove_batch(
        world,
        &tx,
        |items| {
            for serial in serials.clone() {
                items.insert(document_key(serial), model::value(rng, 400));
            }
        },
        |items| {
            for serial in serials.clone() {
                items.insert(reference_key(serial), document_key(serial));
            }
        },
    );
    world.apply(&tx, &batch).expect("the documents are applied");
    let outcome = world.commit(tx, &[]);
    assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}");
}

/// "Commit of a 20-transition block appends once."
///
/// The file layer under the store records every operation. Between the
/// call of the commit and its return it sees one operation that could
/// change a file: one append, to the log file, of exactly the bytes of the
/// commit's group. So the documents, which are too long for their cells,
/// reach no value-log file and no manifest is replaced (CM7, FL7), and a
/// recovery reads the block as one group.
#[test]
fn the_commit_of_a_block_of_twenty_transitions_appends_once() {
    let fs = sim(1);
    let (db, versions) = store_on(&fs, LogOptions::default());
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(1);
    // Blocks before it, so that the block changes pages that are there.
    for round in 0..4 {
        block(&mut world, &mut rng, round * 50, 50);
    }
    let before = groups(&db).len();

    let tx = world.begin();
    for serial in 1000..1020 {
        // Each transition is an operation of its own, through a scope
        // that merges.
        transition(&world, &tx, &mut rng, serial);
    }
    world.check(&tx);
    let stats = tx.txn.stats();
    assert!(stats.writes > 20 * 4 && stats.tree_pages > 3, "{stats:?}");

    let committer = db.committer(&versions);
    let (from, logged_before) = (fs.trace().len(), committer.stats().group_bytes);
    let outcome = world.commit(tx, &[]);
    let changes = changes_since(&fs, from);

    assert_eq!(outcome, Outcome::Committed(versions.latest()));
    assert_eq!(
        changes.len(),
        1,
        "the commit changed more than its log: {changes:?}"
    );
    let append = &changes[0];
    assert_eq!(append.kind, OpKind::Append);
    assert!(
        append.path.contains("wal") && append.path.ends_with(".log"),
        "{append:?}"
    );
    let group_len = committer.stats().group_bytes - logged_before;
    assert_eq!(append.len, group_len, "the append is the group, whole");

    // A recovery reads the block as one group, with the 20 documents in it.
    let logged = groups(&db);
    assert_eq!(logged.len(), before + 1);
    let group = logged.last().expect("the block's group");
    assert_eq!(group.version, versions.latest().get());
    let documents = (group.values.iter()).filter(|(_, value)| value.len() == 400);
    assert!(documents.count() >= 20, "{} values", group.values.len());
    assert!(!group.tree_runs.is_empty() && group.freed.is_empty());
    assert!(group.tree_runs.is_sorted());

    // And everybody reads the block, the documents from the frames of
    // their pages: no value-log file has them.
    world.check_committed();
    assert!(db.values.counts().iter().all(|count| count.values == 0));
    assert!(db.flush_errors.lock().expect("the errors").is_empty());
}

/// The same through the files of the operating system: the log grows by
/// the group and no other file of the store changes its length.
#[test]
fn a_commit_writes_to_the_log_and_to_no_other_file() {
    let (db, versions) = crate::txn::tests::store::Db::new(64);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(2);
    block(&mut world, &mut rng, 0, 30);
    let lengths = || -> BTreeMap<String, u64> {
        let mut found = BTreeMap::new();
        let mut dirs = vec![db.dir.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).expect("a directory") {
                let entry = entry.expect("an entry");
                let meta = entry.metadata().expect("its metadata");
                match meta.is_dir() {
                    true => dirs.push(entry.path()),
                    false => {
                        found.insert(entry.path().display().to_string(), meta.len());
                    }
                }
            }
        }
        found
    };
    let before = lengths();
    let logged = db.committer(&versions).stats().group_bytes;
    block(&mut world, &mut rng, 100, 20);
    let after = lengths();
    let grown: Vec<_> = (after.iter())
        .filter(|(path, len)| before.get(*path) != Some(*len))
        .collect();
    assert_eq!(grown.len(), 1, "{grown:?}");
    let (path, len) = grown[0];
    assert!(path.ends_with(".log"));
    let group_len = db.committer(&versions).stats().group_bytes - logged;
    assert_eq!(len - before[path], group_len);
    assert_eq!(before.len(), after.len(), "no file was made or removed");
}

/// What a rollback of a transaction did to the store: the file operations
/// it made, the page versions it asked the pool for, the private copies
/// and the page ids it gave up, and how long it took.
struct Rollback {
    file_ops: u64,
    page_reads: u64,
    copies: usize,
    ids: usize,
    elapsed: Duration,
}

/// Fills a store with `documents` documents, dirties pages with `dirty`
/// transitions in one transaction, and rolls it back: the best of
/// `repeats` times.
fn roll_back(documents: u64, dirty: u64, repeats: usize) -> Rollback {
    let fs = sim(3);
    let (db, versions) = store_on(&fs, LogOptions::default());
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(3);
    for first in (0..documents).step_by(500) {
        block(&mut world, &mut rng, first, 500.min(documents - first));
    }
    let committer = db.committer(&versions);
    let mut best: Option<Rollback> = None;
    for _ in 0..repeats {
        let tx = world.begin();
        for serial in 0..dirty {
            transition(&world, &tx, &mut rng, 1_000_000 + serial);
        }
        world.write(&tx, vec![put(aux(ROOT), b"note", b"uncommitted")]);
        let stats = tx.txn.stats();
        let copies = stats.tree_pages + stats.flat_pages;
        let held = db.pool.census();
        assert_eq!(held.private, copies);
        let ids = db.private_ids();
        let (ops, pool) = (fs.stats().ops, db.pool.stats());

        let started = Instant::now();
        committer.rollback(&tx.txn).expect("no scope is open");
        let elapsed = started.elapsed();

        let read = db.pool.stats();
        let this = Rollback {
            file_ops: fs.stats().ops - ops,
            page_reads: (read.hits + read.misses) - (pool.hits + pool.misses),
            copies,
            ids,
            elapsed,
        };
        // The copies and the ids went, and nothing else of the pool
        // changed: no page version was touched (WT9).
        let left = db.pool.census();
        assert_eq!(left.private, 0, "the copies went");
        assert_eq!(
            (left.published, left.unwritten),
            (held.published, held.unwritten)
        );
        assert_eq!(db.private_ids(), 0, "the ids went back");
        assert_eq!(tx.txn.stats().writes, 0);
        assert_eq!(tx.txn.stats().pinned, None);
        // The transaction reads the committed state again.
        tx.forget();
        let mut note = Vec::new();
        assert!(!tx.txn.get(aux(ROOT), b"note", &mut note).expect("a read"));
        let uncommitted = Unit::data(model::subtree(DOCUMENTS));
        let found = (tx.txn)
            .node(uncommitted.subtree, &document_key(1_000_000), |node| {
                node.is_some()
            })
            .expect("a read");
        assert!(!found, "a document of the transaction is still read");
        if best.as_ref().is_none_or(|best| this.elapsed < best.elapsed) {
            best = Some(this);
        }
    }
    best.expect("at least one rollback")
}

/// "Rollback cost is proportional to dirty pages only."
///
/// A rollback gives up the private copies the transaction holds, one for
/// each page it dirtied, and the ids of the pages it made, and does
/// nothing else to the store: it makes no file operation, asks the buffer
/// pool for no page and touches no page version, whatever the database
/// holds. So its work is one step for each dirty page. The measurement
/// below has the time of that step.
#[test]
fn a_rollback_gives_up_the_dirty_pages_and_does_nothing_else() {
    let few = roll_back(1500, 10, 1);
    let more = roll_back(1500, 80, 1);
    let larger = roll_back(6000, 10, 1);
    for rollback in [&few, &more, &larger] {
        assert_eq!(rollback.file_ops, 0, "a rollback touched a file");
        assert_eq!(rollback.page_reads, 0, "a rollback read a page");
        assert!(rollback.copies > 0);
    }
    assert!(more.ids > 0, "80 transitions make no page");
    // What there is to give up follows the writes: eight times as many
    // dirty several times the pages. Four times the database dirties what
    // a tree that is a level deeper does, and no more.
    assert!(
        more.copies > few.copies * 2,
        "{} and {}",
        more.copies,
        few.copies
    );
    assert!(
        larger.copies < few.copies * 2,
        "{} and {}",
        larger.copies,
        few.copies
    );
}

/// The measurement behind the sentence, on this machine: the time of a
/// rollback by the number of dirty pages and by the size of the database,
/// the best of several runs each.
///
/// `cargo test --release -p lignindb --lib -- --ignored --nocapture rollback_time`
#[test]
#[ignore = "a measurement: it prints, and asserts only what holds on a busy machine"]
fn rollback_time_by_dirty_pages_and_by_the_size_of_the_database() {
    println!("documents  transitions  dirty pages  rollback (best of 25)  per dirty page");
    let mut by_size = Vec::new();
    for (documents, dirty) in [
        (2_000, 5),
        (2_000, 20),
        (2_000, 80),
        (2_000, 320),
        (500, 80),
        (8_000, 80),
    ] {
        let rollback = roll_back(documents, dirty, 25);
        let nanos = rollback.elapsed.as_nanos() as f64;
        println!(
            "{documents:>9}  {dirty:>11}  {:>11}  {:>18.1} us  {:>11.0} ns",
            rollback.copies,
            nanos / 1e3,
            nanos / rollback.copies as f64
        );
        if dirty == 80 {
            by_size.push(nanos / rollback.copies as f64);
        }
    }
    let (least, most) = by_size
        .iter()
        .fold((f64::MAX, 0f64), |(least, most), each| {
            (least.min(*each), most.max(*each))
        });
    assert!(
        most < least * 4.0,
        "per dirty page, by database size: {by_size:?}"
    );
}

/// "A write committed outside an open write transaction makes that
/// transaction's commit fail with a typed conflict error, in the cases
/// TXN-01 names and no others."
///
/// The rule in one table, through the commit path and its typed errors:
/// for each kind of write the open transaction made first, and each kind
/// of write that was committed outside it afterwards, whether the two
/// meet (`transactions.md` CF2, GM1 to GM5). `conflicts.rs` has every case
/// of the specification's tables one by one.
#[test]
fn a_write_outside_fails_the_open_transaction_exactly_where_their_keys_meet() {
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Does {
        Nothing,
        Document(u64),
        Aux(&'static [u8]),
        Meta(&'static [u8]),
        AuxRange,
    }
    fn write<'s>(world: &World<'s>, tx: &Tx<'s>, rng: &mut Rng, does: Does) {
        let meta = model::unit(Column::Meta, ROOT);
        match does {
            Does::Nothing => {}
            Does::Document(serial) => transition(world, tx, rng, serial),
            Does::Aux(key) => world.write(tx, vec![put(aux(ROOT), key, b"written")]),
            Does::Meta(key) => world.write(tx, vec![delete(meta, key)]),
            Does::AuxRange => {
                let all = model::Write::Range(aux(ROOT), Vec::new(), None);
                world.write(tx, vec![all]);
            }
        }
    }
    let fs = sim(4);
    let (db, versions) = store_on(&fs, LogOptions::default());
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(4);
    block(&mut world, &mut rng, 0, 30);
    let committer = db.committer(&versions);
    // (the open transaction's write, the write outside, whether they meet)
    let table = [
        // Any two changes of the tree meet in the root of the root Merk,
        // however far apart their paths are (GM2).
        (Does::Document(100), Does::Document(200), true),
        // Two writers of one key of another column meet (B15), and of two
        // keys they do not (B16).
        (Does::Aux(b"x"), Does::Aux(b"x"), true),
        (Does::Aux(b"x"), Does::Aux(b"y"), false),
        // The column is part of the key (A6), and a tree change writes no
        // aux or meta record (GM4).
        (Does::Aux(b"x"), Does::Meta(b"x"), false),
        (Does::Aux(b"x"), Does::Document(300), false),
        (Does::Document(400), Does::Aux(b"x"), false),
        (Does::Meta(b"m"), Does::Meta(b"m"), true),
        // A range meets the keys inside it and no others (CF5).
        (Does::Aux(b"x"), Does::AuxRange, true),
        (Does::AuxRange, Does::Aux(b"z"), true),
        (Does::Meta(b"m"), Does::AuxRange, false),
        // A transaction that wrote nothing cannot conflict (GM5).
        (Does::Nothing, Does::Document(500), false),
        // A write outside that writes nothing fails nobody.
        (Does::Document(600), Does::Nothing, false),
    ];
    for (row, (own, outside, meets)) in table.into_iter().enumerate() {
        let before = versions.latest();
        let tx = world.begin();
        write(&world, &tx, &mut rng, own);
        // The write outside commits at once, while the transaction is
        // open (RD7).
        let w = world.begin();
        write(&world, &w, &mut rng, outside);
        let committed = world.commit(w, &[&tx]);
        match outside {
            Does::Nothing => assert_eq!(committed, Outcome::Empty),
            _ => assert_eq!(
                committed,
                Outcome::Committed(before.next().expect("a version"))
            ),
        }
        // The model of the tests agrees with the table, and the commit
        // with both.
        assert_eq!(
            tx.model.borrow().conflicts(),
            meets,
            "row {row}: {own:?}, {outside:?}"
        );
        let wrote = tx.txn.stats().writes > 0;
        let ended = committer.commit(&tx.txn, &mut NoStaleHashes);
        match (meets, wrote) {
            (true, _) => {
                assert!(
                    matches!(ended, Err(CommitError::Conflict)),
                    "row {row}: {ended:?}"
                );
            }
            (false, false) => assert!(matches!(ended, Ok(Committed::Empty)), "row {row}"),
            (false, true) => {
                let published = ended.expect("no conflict");
                assert_eq!(published.version(), Some(versions.latest()), "row {row}");
                let writes = writes_of(&tx);
                world.model.commit(&writes);
            }
        }
        assert_eq!(
            tx.txn.stats().writes,
            0,
            "row {row}: the commit ended the transaction"
        );
        world.check_committed();
    }
    assert_eq!(committer.stats().conflicts, 5);
}

/// The owner's run: blocks committed back to back with write-back at
/// work, by a loop on another thread or by the committers' own turns. The
/// log that is held stays within its bound, although many times that much
/// is logged, and what is read at the end comes through pages that
/// write-back put into segments and values that it placed in the value
/// log.
fn blocks_back_to_back(seed: u64, background: bool) {
    const BLOCKS: u64 = 160;
    let fs = sim(seed);
    let options = LogOptions {
        target_file_len: 48 << 10,
        sync: SyncPolicy::Never,
    };
    let flush = FlushConfig {
        max_unwritten: 64,
        max_pending_bytes: 96 << 10,
        max_log_held: 192 << 10,
        checkpoint_log_bytes: 64 << 10,
        idle: Duration::from_millis(1),
        ..FlushConfig::new(SIZE)
    };
    let (db, versions) = store_with(&fs, 48, options, flush);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(seed);
    let write_back = db.write_back(&versions);
    let committer = db.committer(&versions);
    let long = aux(ROOT);
    let (mut held_most, mut block_most, mut released) = (0, 0, 0);
    let failed = AtomicBool::new(false);

    std::thread::scope(|scope| {
        if background {
            scope.spawn(|| write_back.run(|_| failed.store(true, Ordering::Relaxed)));
        }
        for round in 0..BLOCKS {
            let tx = world.begin();
            for serial in round * 5..round * 5 + 5 {
                transition(&world, &tx, &mut rng, serial);
            }
            if round > 4 {
                // A document of an earlier block gets a new value, one is
                // deleted with its reference, and a flat record with a
                // value out of line is written again: each cell stops
                // holding the value it held, placed or still pending.
                let (changed, gone) = ((round - 3) * 5, (round - 4) * 5 + 1);
                let batch = grove_batch(
                    &world,
                    &tx,
                    |items| {
                        items.insert(document_key(changed), model::value(&mut rng, 400));
                        items.remove(&document_key(gone));
                    },
                    |items| {
                        items.remove(&reference_key(gone));
                    },
                );
                world.apply(&tx, &batch).expect("the changes are applied");
            }
            let note = [b'n', (round % 7) as u8];
            world.write(&tx, vec![put(long, &note, &model::value(&mut rng, 3000))]);

            let logged = committer.stats().group_bytes;
            let outcome = world.commit(tx, &[]);
            assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}");
            block_most = block_most.max(committer.stats().group_bytes - logged);
            // The committer has waited for room by now (FL5).
            held_most = held_most.max(write_back.log_held());
            released = db.flush.stats().values_placed;
        }
        write_back.stop();
    });
    assert!(
        !failed.load(Ordering::Relaxed),
        "a turn of write-back failed"
    );
    assert!(db.flush_errors.lock().expect("the errors").is_empty());

    let (stats, flushed) = (committer.stats(), db.flush.stats());
    assert_eq!(stats.published, BLOCKS);
    assert_eq!((stats.failed, stats.write_back_errors), (0, 0));
    // The log that was held never passed its bound by more than the block
    // that was committed last, and far more than the bound was logged.
    assert!(
        held_most <= flush.max_log_held + block_most,
        "{held_most} bytes of log held, the bound is {} and a block is {block_most}",
        flush.max_log_held
    );
    assert!(stats.group_bytes > 8 * flush.max_log_held, "{stats:?}");
    assert!(
        flushed.checkpoints >= 4 && flushed.log_files_dropped >= 4,
        "{flushed:?}"
    );
    assert!(
        released > 0 && flushed.values_placed >= released,
        "{flushed:?}"
    );
    // The files of the log that are left are a few, not the whole log.
    let wal: Vec<_> = (fs
        .read_dir(&Path::new(DIR).join("wal"))
        .expect("the log's directory"))
    .into_iter()
    .collect();
    let on_disk: u64 = (wal.iter())
        .map(|entry| {
            let path = Path::new(DIR).join("wal").join(&entry.name);
            let file = fs
                .open(&path, lignin_io::OpenMode::Read)
                .expect("a log file");
            file.len().expect("its length")
        })
        .sum();
    assert!(
        on_disk < stats.group_bytes / 4,
        "{on_disk} bytes of log are left of {} that were logged",
        stats.group_bytes
    );

    // Everything reads as the model has it, and the counts of live values
    // are those of the pointers the pages hold: every commit released
    // exactly what its cells stopped holding (PV6).
    world.check_committed();
    let held = pointers_held(&db, &versions);
    assert!(!held.is_empty(), "no value reached the value log");
    assert_eq!(counted(&db), held);
    assert_eq!(db.pool.census().private, 0);
    assert_eq!(db.private_ids(), 0);
}

#[test]
fn blocks_back_to_back_with_a_write_back_loop_keep_the_log_bounded() {
    blocks_back_to_back(5, true);
}

#[test]
fn blocks_back_to_back_whose_committers_make_the_turns_keep_the_log_bounded() {
    blocks_back_to_back(6, false);
}
