//! The conflict rule at the commit (`transactions.md` CF2, T-CF1, T-CF2):
//! every case that TXN-01 ran against RocksDB (sections 16.3 and 16.4),
//! with the result the specification lists for LigninDB.
//!
//! The cases of 16.3 are puts, deletes and range deletes of flat records.
//! Those of 16.4 are GroveDB operations, which are written here as the
//! record keys such an operation writes (14.1): the node of the element in
//! its Merk, the element that holds that Merk in the root Merk, every node
//! on the way to each root, and the base root key where the root Merk's
//! root changes. The same cases on both engines, through GroveDB, are
//! GROVE-01's (T-RD2).
//!
//! The cases with savepoints (A4, A4b, A5, A5b, A16, B6, B7) are written
//! with the marks that a savepoint is made of. `txn::tests::savepoints`
//! (TXN-04, T-SP1) has them through the savepoint calls, on this commit.
//! A11's flush of the memtables has no counterpart, so A11 is a
//! transaction that commits what it wrote.

use std::collections::BTreeMap;

use super::{aux, records};
use crate::{
    commit::{CommitError, NoStaleHashes},
    savepoint::SavepointError,
    snapshot::SnapshotError,
    txn::{
        tests::{
            delete,
            model::{self, OwnedBatch, OwnedMerk, Val, Write},
            put, scan,
            store::{Db, Outcome},
            Tx, World,
        },
        Applied, Column, KeySet, Mark, Unit,
    },
};

const K: &[u8] = b"k";
const K1: &[u8] = b"k1";
const K2: &[u8] = b"k2";

/// The flat records that stand for RocksDB's default column family.
fn data() -> Unit {
    model::unit(Column::Data, 7)
}

fn world(run: impl FnOnce(&mut World<'_>)) {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    run(&mut world);
    world.check_committed();
    assert_eq!(db.pool.census().private, 0, "no copy is left");
    assert_eq!(db.private_ids(), 0, "no id is left with a transaction");
}

/// *W*: a write made outside every open transaction, which commits at
/// once (RD7). `open` are the transactions that are open beside it.
fn outside<'s>(world: &mut World<'s>, writes: Vec<Write>, open: &[&Tx<'s>]) {
    let outcome = world.write_outside(&records(writes), open);
    assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}");
}

fn conflicts<'s>(world: &mut World<'s>, tx: Tx<'s>) {
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
}

fn commits<'s>(world: &mut World<'s>, tx: Tx<'s>) {
    let outcome = world.commit(tx, &[]);
    assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}");
}

/// What `tx` reads under `key`.
fn read(tx: &Tx<'_>, unit: Unit, key: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let found = tx.txn.get(unit, key, &mut out).expect("a read");
    found.then_some(out)
}

/// What a read with no transaction finds under `key`.
fn committed(world: &World<'_>, unit: Unit, key: &[u8]) -> Option<Vec<u8>> {
    read(&world.begin(), unit, key)
}

/// Rolls `tx` back to `mark`, which leaves it `writes` writes.
fn rewind(tx: &Tx<'_>, mark: Mark, writes: usize) {
    assert!(tx.txn.rewind(mark));
    tx.model.borrow_mut().writes.truncate(writes);
}

#[test]
fn a1_a_put_outside_after_the_transaction_put_the_key_is_a_conflict() {
    world(|world| {
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![put(data(), K, b"w")], &[&t]);
        conflicts(world, t);
        assert_eq!(committed(world, data(), K).as_deref(), Some(&b"w"[..]));
    });
}

#[test]
fn a1b_and_a13_the_second_of_two_transactions_that_wrote_one_key_fails() {
    world(|world| {
        // A13: the transaction that wrote first commits second.
        let first = world.begin();
        let second = world.begin();
        world.write(&first, vec![put(data(), K, b"first")]);
        world.write(&second, vec![put(data(), K, b"second")]);
        let outcome = world.commit(second, &[&first]);
        assert!(matches!(outcome, Outcome::Committed(_)));
        conflicts(world, first);
        assert_eq!(committed(world, data(), K).as_deref(), Some(&b"second"[..]));
    });
}

#[test]
fn a13b_two_transactions_whose_keys_do_not_meet_both_commit() {
    world(|world| {
        let first = world.begin();
        let second = world.begin();
        world.write(&first, vec![put(data(), K1, b"first")]);
        world.write(&second, vec![put(data(), K2, b"second")]);
        let outcome = world.commit(second, &[&first]);
        assert!(matches!(outcome, Outcome::Committed(_)));
        commits(world, first);
        assert_eq!(committed(world, data(), K1).as_deref(), Some(&b"first"[..]));
        assert_eq!(
            committed(world, data(), K2).as_deref(),
            Some(&b"second"[..])
        );
    });
}

#[test]
fn a2_a_put_outside_before_the_transaction_first_writes_the_key_is_none() {
    world(|world| {
        let t = world.begin();
        outside(world, vec![put(data(), K, b"w")], &[&t]);
        world.write(&t, vec![put(data(), K, b"t")]);
        commits(world, t);
        assert_eq!(committed(world, data(), K).as_deref(), Some(&b"t"[..]));
    });
}

#[test]
fn a3_the_transaction_reads_what_was_committed_outside_under_a_key_it_has_not_written() {
    world(|world| {
        // A3.
        let t = world.begin();
        world.write(&t, vec![put(data(), K1, b"t")]);
        outside(world, vec![put(data(), K2, b"w")], &[&t]);
        assert_eq!(read(&t, data(), K2).as_deref(), Some(&b"w"[..]));
        world.check(&t);
        commits(world, t);

        // A3b: what the transaction read does not matter (CF4).
        outside(world, vec![put(data(), K, b"0")], &[]);
        let t = world.begin();
        assert_eq!(read(&t, data(), K).as_deref(), Some(&b"0"[..]));
        outside(world, vec![put(data(), K, b"w")], &[&t]);
        assert_eq!(read(&t, data(), K).as_deref(), Some(&b"w"[..]));
        world.write(&t, vec![put(data(), b"k3", b"t")]);
        commits(world, t);
    });
}

#[test]
fn a3c_and_a3d_the_transactions_own_write_wins_over_a_later_one_outside() {
    world(|world| {
        // A3c.
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![put(data(), K, b"w")], &[&t]);
        assert_eq!(read(&t, data(), K).as_deref(), Some(&b"t"[..]));
        world.check(&t);
        conflicts(world, t);

        // A3d.
        let t = world.begin();
        world.write(&t, vec![delete(data(), K)]);
        outside(world, vec![put(data(), K, b"w2")], &[&t]);
        assert_eq!(read(&t, data(), K), None);
        world.check(&t);
        conflicts(world, t);
    });
}

#[test]
fn a4_a_write_that_was_rolled_back_to_a_mark_is_no_write() {
    world(|world| {
        // A4 and A4b.
        let t = world.begin();
        let mark = t.txn.mark();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![put(data(), K, b"w")], &[&t]);
        rewind(&t, mark, 0);
        assert_eq!(read(&t, data(), K).as_deref(), Some(&b"w"[..]));
        assert_eq!(world.commit(t, &[]), Outcome::Empty);

        // A5b: the key is written again after the rollback, and after the
        // commit outside.
        let t = world.begin();
        let mark = t.txn.mark();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![put(data(), K, b"w2")], &[&t]);
        rewind(&t, mark, 0);
        world.write(&t, vec![put(data(), K, b"t2")]);
        commits(world, t);
        assert_eq!(committed(world, data(), K).as_deref(), Some(&b"t2"[..]));
    });
}

#[test]
fn a5_and_a16_a_write_from_before_the_mark_keeps_its_first_write_version() {
    world(|world| {
        // A5.
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        let mark = t.txn.mark();
        world.write(&t, vec![put(data(), K, b"t again")]);
        outside(world, vec![put(data(), K, b"w")], &[&t]);
        rewind(&t, mark, 1);
        conflicts(world, t);

        // A16.
        let t = world.begin();
        let _first = t.txn.mark();
        world.write(&t, vec![put(data(), K, b"t")]);
        let second = t.txn.mark();
        world.write(&t, vec![put(data(), K2, b"t")]);
        outside(world, vec![put(data(), K, b"w2")], &[&t]);
        rewind(&t, second, 1);
        conflicts(world, t);
    });
}

#[test]
fn a6_the_column_is_part_of_the_key() {
    world(|world| {
        let t = world.begin();
        world.write(&t, vec![put(aux(7), K, b"t")]);
        outside(world, vec![put(data(), K, b"w")], &[&t]);
        commits(world, t);
    });
}

#[test]
fn a7_a_write_outside_that_changes_nothing_is_a_write() {
    world(|world| {
        // A7: a delete of a key that never had a record.
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![delete(data(), K)], &[&t]);
        conflicts(world, t);

        // A7b: a put of the value the key already had.
        outside(world, vec![put(data(), K, b"same")], &[]);
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![put(data(), K, b"same")], &[&t]);
        conflicts(world, t);
    });
}

#[test]
fn a8_and_a9_only_the_first_write_of_a_key_counts() {
    world(|world| {
        // A8: the key is first written after the commit outside.
        let t = world.begin();
        world.write(&t, vec![put(data(), K1, b"t")]);
        outside(world, vec![put(data(), K2, b"w")], &[&t]);
        world.write(&t, vec![put(data(), K2, b"t")]);
        commits(world, t);
        assert_eq!(committed(world, data(), K2).as_deref(), Some(&b"t"[..]));

        // A9: writing the key again afterwards changes nothing.
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![put(data(), K, b"w")], &[&t]);
        world.write(&t, vec![put(data(), K, b"t again")]);
        conflicts(world, t);
    });
}

/// CF9, T-CF2: a range delete outside always meets a key the transaction
/// wrote, where RocksDB's answer depends on its memtables.
#[test]
fn a10_a_range_delete_outside_over_a_key_the_transaction_wrote_is_a_conflict() {
    world(|world| {
        let range = |from: &[u8], to: &[u8]| Write::Range(data(), from.to_vec(), Some(to.to_vec()));
        // A10: the key was new. RocksDB commits.
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![range(b"a", b"z")], &[&t]);
        conflicts(world, t);

        // A10b and A10c: the key had a committed record.
        outside(world, vec![put(data(), K, b"0")], &[]);
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![range(b"a", b"z")], &[&t]);
        conflicts(world, t);

        // A10d: and other keys were written in between. RocksDB commits
        // once they have pushed the key out of its memtables.
        outside(world, vec![put(data(), K, b"0")], &[]);
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        for round in 0..20u8 {
            outside(world, vec![put(aux(9), &[round], &[round; 64])], &[&t]);
        }
        outside(world, vec![range(b"a", b"z")], &[&t]);
        conflicts(world, t);

        // A range that does not hold the key is no conflict.
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![range(b"l", b"z")], &[&t]);
        commits(world, t);
    });
}

/// A dropped subtree is a range of every column (CV9, CF5).
#[test]
fn a_drop_outside_meets_every_key_under_the_subtree() {
    world(|world| {
        let entered = model::subtree(230);
        let unit = Unit::new(Column::Aux, entered);
        let enter = OwnedBatch {
            subtrees: vec![model::OwnedSubtree::Enter(entered, None)],
            records: vec![put(unit, b"there", b"0")],
            ..OwnedBatch::default()
        };
        assert!(matches!(
            world.write_outside(&enter, &[]),
            Outcome::Committed(_)
        ));
        let t = world.begin();
        world.write(&t, vec![put(unit, K, b"t")]);
        let other = world.begin();
        world.write(&other, vec![put(aux(7), K, b"elsewhere")]);
        let drop = OwnedBatch {
            subtrees: vec![model::OwnedSubtree::Drop(entered)],
            ..OwnedBatch::default()
        };
        let outcome = world.write_outside(&drop, &[&t, &other]);
        assert!(matches!(outcome, Outcome::Committed(_)));
        conflicts(world, t);
        commits(world, other);
    });
}

#[test]
fn a11_a_transaction_that_nobody_met_commits_however_much_was_written_meanwhile() {
    world(|world| {
        // A11.
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        commits(world, t);

        // A11b at a small scale: RocksDB answers `TryAgain` once the
        // commits outside have outgrown its memtable history. T-CF4 has
        // the 400 MiB.
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t2")]);
        for round in 0..200u16 {
            let key = round.to_be_bytes();
            outside(world, vec![put(aux(9), &key, &[round as u8; 300])], &[&t]);
        }
        commits(world, t);
        assert_eq!(committed(world, data(), K).as_deref(), Some(&b"t2"[..]));
    });
}

#[test]
fn a12_a_cursor_reads_the_version_it_was_opened_on_and_the_transactions_writes() {
    world(|world| {
        outside(world, vec![put(data(), K1, b"0")], &[]);
        let keys = |records: Vec<(Vec<u8>, Vec<u8>)>| -> Vec<Vec<u8>> {
            records.into_iter().map(|(key, _)| key).collect()
        };
        // A12.
        let t = world.begin();
        let mut first = t.txn.cursor(data());
        first.seek_to_first().expect("a seek");
        outside(world, vec![put(data(), K2, b"w")], &[&t]);
        assert_eq!(keys(scan(&mut first, false)), [K1.to_vec()]);
        let mut second = t.txn.cursor(data());
        assert_eq!(keys(scan(&mut second, false)), [K1.to_vec(), K2.to_vec()]);
        drop((first, second));

        // A12b.
        let mut cursor = t.txn.cursor(data());
        cursor.seek_to_first().expect("a seek");
        world.write(&t, vec![put(data(), b"k3", b"t")]);
        assert_eq!(
            keys(scan(&mut cursor, false)),
            [K1.to_vec(), K2.to_vec(), b"k3".to_vec()]
        );
        drop(cursor);
        commits(world, t);
    });
}

#[test]
fn a14_a_transaction_that_was_rolled_back_starts_fresh() {
    world(|world| {
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        outside(world, vec![put(data(), K, b"w")], &[&t]);
        let committer = world.db.committer(world.versions);
        committer.rollback(&t.txn).expect("no scope is open");
        t.forget();
        assert_eq!(read(&t, data(), K).as_deref(), Some(&b"w"[..]));
        world.write(&t, vec![put(data(), K, b"t2")]);
        commits(world, t);
        assert_eq!(committed(world, data(), K).as_deref(), Some(&b"t2"[..]));
        assert_eq!(committer.stats().rollbacks, 1);
    });
}

#[test]
fn a15_a_transaction_that_wrote_nothing_cannot_conflict() {
    world(|world| {
        let t = world.begin();
        outside(world, vec![put(data(), K, b"w")], &[&t]);
        assert_eq!(world.commit(t, &[]), Outcome::Empty);
    });
}

#[test]
fn a17_a_rollback_to_a_savepoint_that_is_not_set_changes_nothing() {
    world(|world| {
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        let before = t.txn.stats();
        assert_eq!(
            t.txn.rollback_to_savepoint(),
            Err(SavepointError::NoSavepoint)
        );
        assert_eq!(t.txn.stats(), before);
        assert_eq!(read(&t, data(), K).as_deref(), Some(&b"t"[..]));
        commits(world, t);
        assert_eq!(committed(world, data(), K).as_deref(), Some(&b"t"[..]));
    });
}

/// Commits `writes` outside every open transaction with `also` in the key
/// set of the commit, which the models of `open` take as `counted`.
fn outside_covering<'s>(
    world: &mut World<'s>,
    writes: Vec<Write>,
    also: &KeySet,
    counted: Write,
    open: &[&Tx<'s>],
) {
    let w = world.begin();
    world.write(&w, writes.clone());
    let committer = world.db.committer(world.versions);
    let published = (world.db)
        .commit_as(&committer, &w.txn, Some(also))
        .expect("the commit is made")
        .expect("it takes a version");
    let commit = world.model.commit(&writes);
    assert_eq!(published.version.get(), commit);
    let mut met = writes;
    met.push(counted);
    for tx in open {
        tx.model.borrow_mut().foreign(commit, &met);
    }
}

/// CF9, T-CF2: records loaded outside the transaction under a key it
/// wrote. RocksDB's file ingestion is invisible to its check.
#[test]
fn a18_a_load_outside_over_a_key_the_transaction_wrote_is_a_conflict() {
    world(|world| {
        let t = world.begin();
        world.write(&t, vec![put(data(), K, b"t")]);
        let elsewhere = world.begin();
        world.write(&elsewhere, vec![put(aux(7), K, b"t")]);
        // The load is counted as one write of the subtree's column (CF5),
        // whichever keys it holds.
        let mut loaded = KeySet::new();
        loaded.range(data(), &[], None);
        outside_covering(
            world,
            vec![put(data(), b"loaded", b"w")],
            &loaded,
            Write::Range(data(), Vec::new(), None),
            &[&t, &elsewhere],
        );
        conflicts(world, t);
        commits(world, elsewhere);
    });
}

/// The root Merk of the small grove of the cases of 16.4, and the two
/// trees under it.
const ROOT: u8 = 20;
const A: u8 = 21;
const B: u8 = 22;

/// What the element of a tree holds of the Merk below it.
fn element(items: &BTreeMap<Vec<u8>, Vec<u8>>) -> Vec<u8> {
    let parts: Vec<&[u8]> = items
        .iter()
        .flat_map(|(key, value)| [&key[..], &value[..]])
        .collect();
    model::hash_of(&parts).to_vec()
}

/// The batch of a GroveDB operation that changes the tree `tree` (14.1):
/// the change set of its Merk, that of the root Merk, whose element of the
/// tree changes with it, and the base root key if the root Merk's root
/// changes. An operation that changes nothing applies an empty batch
/// (GM0).
fn tree_batch<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    tree: u8,
    edit: impl FnOnce(&mut BTreeMap<Vec<u8>, Vec<u8>>),
) -> OwnedBatch {
    let mut held = Vec::new();
    let child = world.merk(tx, tree, |items| {
        edit(items);
        held = element(items);
    });
    if child.nodes.is_empty() && child.removed.is_empty() {
        return OwnedBatch::default();
    }
    let name = vec![b'A' + (tree - A)];
    let root = world.merk(tx, ROOT, |items| {
        items.insert(name, held);
    });
    let records = World::root_record(&root).into_iter().collect();
    OwnedBatch {
        merks: vec![child, root],
        records,
        ..OwnedBatch::default()
    }
}

/// `tx` inserts `key` into the tree `tree`.
fn insert<'s>(world: &World<'s>, tx: &Tx<'s>, tree: u8, key: &[u8]) -> Applied {
    let batch = tree_batch(world, tx, tree, |items| {
        items.insert(key.to_vec(), [b"value of ", key].concat());
    });
    world.apply(tx, &batch).expect("the insert is applied")
}

/// An insert with no transaction.
fn insert_outside<'s>(world: &mut World<'s>, tree: u8, key: &[u8], open: &[&Tx<'s>]) {
    let w = world.begin();
    insert(world, &w, tree, key);
    let outcome = world.commit(w, open);
    assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}");
}

/// The keys of the tree `tree` as a read with no transaction finds them.
fn keys_of(world: &World<'_>, tree: u8) -> Vec<Vec<u8>> {
    let trees = world.db.check_trees(world.versions, world.latest());
    trees
        .get(&model::subtree(tree))
        .cloned()
        .unwrap_or_default()
}

/// A database that holds two empty trees `A` and `B` under the root.
fn grove(run: impl FnOnce(&mut World<'_>)) {
    world(|world| {
        let outcome = world.edit_outside(
            ROOT,
            |items| {
                items.insert(b"A".to_vec(), element(&BTreeMap::new()));
                items.insert(b"B".to_vec(), element(&BTreeMap::new()));
            },
            &[],
        );
        assert!(matches!(outcome, Outcome::Committed(_)));
        run(world);
    });
}

#[test]
fn b1_and_b14_any_two_tree_changes_meet() {
    grove(|world| {
        // B1: the two paths are as far apart as they can be (GM2).
        let t = world.begin();
        insert(world, &t, A, b"a1");
        insert_outside(world, B, b"b1", &[&t]);
        conflicts(world, t);
        assert_eq!(keys_of(world, B), [b"b1".to_vec()]);
        assert!(keys_of(world, A).is_empty());

        // B14: the same Merk.
        let t = world.begin();
        insert(world, &t, A, b"a1");
        insert_outside(world, A, b"a2", &[&t]);
        conflicts(world, t);
        assert_eq!(keys_of(world, A), [b"a2".to_vec()]);
    });
}

#[test]
fn b2_a_tree_change_outside_before_the_transactions_first_is_built_on() {
    grove(|world| {
        let t = world.begin();
        insert_outside(world, B, b"b1", &[&t]);
        insert(world, &t, A, b"a1");
        commits(world, t);
        assert_eq!(keys_of(world, A), [b"a1".to_vec()]);
        assert_eq!(keys_of(world, B), [b"b1".to_vec()]);
    });
}

#[test]
fn b3_b4_b15_b16_aux_records_and_tree_changes_do_not_meet() {
    grove(|world| {
        // B3.
        let t = world.begin();
        world.write(&t, vec![put(aux(ROOT), b"x", b"t")]);
        insert_outside(world, B, b"b1", &[&t]);
        commits(world, t);

        // B4.
        let t = world.begin();
        insert(world, &t, A, b"a1");
        outside(world, vec![put(aux(ROOT), b"y", b"w")], &[&t]);
        commits(world, t);

        // B15: two writers of one aux key do meet.
        let t = world.begin();
        world.write(&t, vec![put(aux(ROOT), b"x", b"t")]);
        outside(world, vec![put(aux(ROOT), b"x", b"w")], &[&t]);
        conflicts(world, t);

        // B16: the empty overlap set on one flat page (WT7a).
        let t = world.begin();
        world.write(&t, vec![put(aux(ROOT), b"x", b"t")]);
        outside(world, vec![put(aux(ROOT), b"z", b"w")], &[&t]);
        assert_eq!(read(&t, aux(ROOT), b"z").as_deref(), Some(&b"w"[..]));
        commits(world, t);
        assert_eq!(
            committed(world, aux(ROOT), b"x").as_deref(),
            Some(&b"t"[..])
        );
        assert_eq!(
            committed(world, aux(ROOT), b"z").as_deref(),
            Some(&b"w"[..])
        );
    });
}

#[test]
fn b5_a_transaction_that_only_reads_sees_the_commit_outside_and_commits() {
    grove(|world| {
        let t = world.begin();
        world.check(&t);
        insert_outside(world, B, b"b1", &[&t]);
        let found = (t.txn)
            .node(model::subtree(B), b"b1", |node| node.is_some())
            .expect("a read");
        assert!(found);
        world.check(&t);
        assert_eq!(world.commit(t, &[]), Outcome::Empty);
    });
}

#[test]
fn b6_and_b7_after_a_rollback_to_a_mark_what_remains_decides() {
    grove(|world| {
        // B6.
        let t = world.begin();
        let mark = t.txn.mark();
        insert(world, &t, A, b"a1");
        insert_outside(world, B, b"b1", &[&t]);
        rewind(&t, mark, 0);
        insert(world, &t, A, b"a2");
        commits(world, t);
        assert_eq!(keys_of(world, A), [b"a2".to_vec()]);

        // B7.
        let t = world.begin();
        insert(world, &t, A, b"a0");
        let writes = t.model.borrow().writes.len();
        let mark = t.txn.mark();
        insert(world, &t, A, b"a1");
        insert_outside(world, B, b"b2", &[&t]);
        rewind(&t, mark, writes);
        conflicts(world, t);
    });
}

/// The seven steps of 14.3, the shape of mainnet heights 32326 to 32328,
/// as far as an engine without Merk shows them: what the transaction
/// reads at each step, and how it ends. The root hashes and the costs are
/// GROVE-01's (T-RD4).
#[test]
fn b8_the_block_transaction_that_a_write_with_no_transaction_overlapped() {
    grove(|world| {
        let t = world.begin();
        insert(world, &t, A, b"a1");
        world.check(&t);
        insert_outside(world, B, b"b1", &[&t]);
        // Step 2: the transaction reads its own root node, which carries
        // the element of `B` from before. Its next operation finds its
        // writes met.
        world.check(&t);
        assert!(t.txn.stats().overlapped > 0);
        // Step 3.
        let found = (t.txn)
            .node(model::subtree(B), b"b1", |node| node.is_some())
            .expect("a read");
        assert!(found, "the transaction never wrote that node");
        // Steps 4 and 5: the write to `B` loads `B` as committed.
        insert(world, &t, A, b"a2");
        world.check(&t);
        insert(world, &t, B, b"b2");
        world.check(&t);
        let b = t
            .model
            .borrow()
            .view(&world.model.records, Unit::data(model::subtree(B)));
        assert_eq!(b.len(), 2, "b1 from outside and its own b2");
        // Steps 6 and 7.
        conflicts(world, t);
        assert_eq!(keys_of(world, B), [b"b1".to_vec()]);
        assert!(keys_of(world, A).is_empty());
    });
}

#[test]
fn b9_and_b10_two_transactions_that_change_the_tree() {
    grove(|world| {
        // B9.
        let first = world.begin();
        let second = world.begin();
        insert(world, &first, A, b"a1");
        insert(world, &second, B, b"b1");
        let outcome = world.commit(first, &[&second]);
        assert!(matches!(outcome, Outcome::Committed(_)));
        conflicts(world, second);

        // B10: the second one writes only after the first has committed.
        let first = world.begin();
        let second = world.begin();
        insert(world, &first, A, b"a2");
        let outcome = world.commit(first, &[&second]);
        assert!(matches!(outcome, Outcome::Committed(_)));
        insert(world, &second, B, b"b1");
        commits(world, second);
        assert_eq!(keys_of(world, A), [b"a1".to_vec(), b"a2".to_vec()]);
        assert_eq!(keys_of(world, B), [b"b1".to_vec()]);
    });
}

#[test]
fn b11_b12_b13_who_reads_what() {
    grove(|world| {
        // B11: a snapshot never sees a later commit.
        let reads = world.db.reads(world.versions);
        let snapshot = reads.snapshot();
        insert_outside(world, B, b"b1", &[]);
        let at_snapshot = world.db.check_trees(world.versions, snapshot.version());
        assert!(!at_snapshot.contains_key(&model::subtree(B)));
        assert_eq!(keys_of(world, B), [b"b1".to_vec()]);

        // B13: and nothing is written or committed through it.
        let batch = records(vec![put(aux(ROOT), b"x", b"through a snapshot")]);
        let refused = batch.with(|batch| snapshot.apply(batch));
        assert!(matches!(refused, Err(SnapshotError::ReadOnly)));
        assert!(matches!(snapshot.commit(), Err(SnapshotError::ReadOnly)));
        drop(snapshot);

        // B12: nobody but the transaction sees its writes.
        let t = world.begin();
        insert(world, &t, A, b"a1");
        assert!(keys_of(world, A).is_empty());
        let own = (t.txn)
            .node(model::subtree(A), b"a1", |node| node.is_some())
            .expect("a read");
        assert!(own);
        world.check(&t);
        commits(world, t);
    });
}

/// CF9, T-CF2: a wipe is a commit whose key set is everything (CF5), so it
/// meets every write of every open transaction. On RocksDB the answer
/// depends on what the memtables still hold.
#[test]
fn b17_a_wipe_meets_every_write() {
    grove(|world| {
        let t = world.begin();
        insert(world, &t, A, b"a1");
        let only_aux = world.begin();
        world.write(&only_aux, vec![put(aux(9), K, b"t")]);
        let idle = world.begin();
        let mut everything = KeySet::new();
        everything.everything();
        // What the wipe writes is the engine's facade's (TXN-07). Its key
        // set is what the conflict rule sees.
        let each_unit = |unit: Unit| Write::Range(unit, Vec::new(), None);
        outside_covering(
            world,
            vec![delete(aux(ROOT), b"gone")],
            &everything,
            each_unit(aux(9)),
            &[&only_aux],
        );
        t.model
            .borrow_mut()
            .writes
            .iter_mut()
            .for_each(|(_, _, met)| *met = true);
        conflicts(world, t);
        conflicts(world, only_aux);
        assert_eq!(world.commit(idle, &[]), Outcome::Empty);
    });
}

#[test]
fn b18_b20_b22_an_operation_that_applies_an_empty_batch_adds_nothing() {
    grove(|world| {
        insert_outside(world, A, b"a1", &[]);
        // B18 and B20: `insert_if_not_exists` for a key that is there, and
        // `insert_if_changed_value` with the value it has, apply nothing.
        // B22: a delete of a key that is not there fails before it applies
        // its batch.
        let t = world.begin();
        let nothing = tree_batch(world, &t, A, |items| {
            items.insert(b"a1".to_vec(), b"value of a1".to_vec());
        });
        assert!(nothing.merks.is_empty());
        let applied = world.apply(&t, &nothing).expect("nothing is applied");
        assert_eq!(applied, Applied::default());
        assert_eq!(t.txn.stats().writes, 0);
        insert_outside(world, B, b"b1", &[&t]);
        assert_eq!(world.commit(t, &[]), Outcome::Empty);
    });
}

/// The change set of an operation that writes the node `key` of `tree`
/// again with the record it has, and every node on the way to the root
/// with it (ST5): what a plain insert of an equal element hands to
/// storage.
fn rewrite<'s>(world: &World<'s>, tx: &Tx<'s>, tree: u8, key: &[u8]) -> OwnedMerk {
    let subtree = model::subtree(tree);
    let view = tx
        .model
        .borrow()
        .view(&world.model.records, Unit::data(subtree));
    let mut nodes = Vec::new();
    let root = OwnedMerk::change(subtree, &BTreeMap::new(), &items_of(&view)).root;
    let mut at = root.flatten();
    while let Some(here) = at {
        let Some(Val::Node(node)) = view.get(&here) else {
            panic!("a link leads to no node");
        };
        at = match key.cmp(&here) {
            std::cmp::Ordering::Less => node.left.as_ref().map(|link| link.key.clone()),
            std::cmp::Ordering::Greater => node.right.as_ref().map(|link| link.key.clone()),
            std::cmp::Ordering::Equal => None,
        };
        nodes.push((here.clone(), node.clone(), here == key));
    }
    OwnedMerk {
        subtree,
        nodes,
        removed: Vec::new(),
        root: None,
        hosted: BTreeMap::new(),
        stale: BTreeMap::new(),
        anchored: false,
    }
}

fn items_of(view: &BTreeMap<Vec<u8>, Val>) -> BTreeMap<Vec<u8>, Vec<u8>> {
    crate::txn::tests::model::Committed::items(view)
}

#[test]
fn b19_and_b21_a_plain_insert_of_an_equal_element_and_a_delete_do_write() {
    grove(|world| {
        insert_outside(world, A, b"a1", &[]);
        // B19.
        let t = world.begin();
        let batch = OwnedBatch {
            merks: vec![rewrite(world, &t, A, b"a1"), rewrite(world, &t, ROOT, b"A")],
            ..OwnedBatch::default()
        };
        world.apply(&t, &batch).expect("the insert is applied");
        assert!(t.txn.stats().writes >= 2);
        world.check(&t);
        insert_outside(world, B, b"b1", &[&t]);
        conflicts(world, t);

        // B21.
        let t = world.begin();
        let batch = tree_batch(world, &t, A, |items| {
            items.remove(&b"a1"[..]);
        });
        world.apply(&t, &batch).expect("the delete is applied");
        insert_outside(world, B, b"b2", &[&t]);
        conflicts(world, t);
        assert_eq!(keys_of(world, A), [b"a1".to_vec()]);
    });
}

/// The typed error, and that it is used for a conflict and for nothing
/// else (CF6): through the commit path itself, not the store's wrapper.
#[test]
fn a_conflict_is_one_typed_error_and_ends_the_transaction() {
    let (db, versions) = Db::new(64);
    let committer = db.committer(&versions);
    let unit = data();
    let records = |value: &'static [u8]| records(vec![put(unit, K, value)]);
    let t = committer.path().begin();
    let scope = t.scope().expect("a scope");
    records(b"t")
        .with(|batch| scope.apply(batch))
        .expect("a write");
    scope.merge();
    let w = records(b"w").with(|batch| committer.write(batch, &mut NoStaleHashes));
    let w = w.expect("the write commits at once");
    assert_eq!(w.version(), Some(versions.latest()));

    let error = committer
        .commit(&t, &mut NoStaleHashes)
        .expect_err("a conflict");
    assert!(matches!(error, CommitError::Conflict), "{error}");
    assert!(!error.leaves_transaction_open() && error.published().is_none());
    assert_eq!(
        t.stats().writes,
        0,
        "the commit ended the transaction (CM6)"
    );
    assert_eq!(committer.stats().conflicts, 1);
    assert_eq!(versions.latest(), w.version().expect("a version"));
    // The same transaction, written again, commits.
    let scope = t.scope().expect("a scope");
    records(b"t2")
        .with(|batch| scope.apply(batch))
        .expect("a write");
    scope.merge();
    let again = committer
        .commit(&t, &mut NoStaleHashes)
        .expect("no conflict");
    assert_eq!(again.version(), Some(versions.latest()));
}
