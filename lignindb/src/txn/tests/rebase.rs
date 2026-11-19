//! What a commit outside a write transaction does to it
//! (`transactions.md` WT7, WT8, CF2): the rebase with an empty overlap set
//! (T-RB2), the records served by key with a non-empty one (T-RB3, 14.3),
//! and the cases of section 16.3 that the overlap set decides.

use lignin_format::tree_page::Side;

use super::{
    model::{self, value, OwnedBatch},
    store::{Db, Outcome, Rng},
    *,
};

fn aux() -> Unit {
    model::unit(Column::Aux, 0)
}

fn records(records: Vec<Write>) -> OwnedBatch {
    OwnedBatch {
        merks: Vec::new(),
        records,
    }
}

fn get(tx: &Tx<'_>, unit: Unit, key: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let found = tx.txn.get(unit, key, &mut out).expect("a read");
    found.then_some(out)
}

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

/// The cases of 16.3 whose result follows from the overlap set, on flat
/// keys. `W` is a write with no transaction.
#[test]
fn a_commit_fails_exactly_when_a_commit_outside_wrote_a_key_after_the_first_write() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    let k = |name: &str| name.as_bytes().to_vec();
    let mut case = 0;
    let mut key = |name: &str| {
        case += 1;
        k(&format!("{name}{case}"))
    };

    // A1: T puts K; W puts K; T commits.
    let (tx, at) = (world.begin(), key("a1"));
    world.write(&tx, vec![put(aux(), &at, b"t")]);
    world.write_outside(&records(vec![put(aux(), &at, b"w")]), &[&tx]);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // A1b, A13: two transactions write K; the second commits first.
    let (first, second, at) = (world.begin(), world.begin(), key("a13"));
    world.write(&first, vec![put(aux(), &at, b"1")]);
    world.write(&second, vec![put(aux(), &at, b"2")]);
    assert!(committed(world.commit(second, &[&first])));
    assert_eq!(world.commit(first, &[]), Outcome::Conflict);
    assert_eq!(get(&world.begin(), aux(), &at), Some(b"2".to_vec()));

    // A13b: two transactions write different keys; both commit.
    let (first, second) = (world.begin(), world.begin());
    world.write(&first, vec![put(aux(), &key("a13b"), b"1")]);
    world.write(&second, vec![put(aux(), &key("a13b"), b"2")]);
    assert!(committed(world.commit(second, &[&first])));
    assert!(committed(world.commit(first, &[])));

    // A2: T starts; W puts K; T puts K; T commits and K holds T's value.
    let (tx, at) = (world.begin(), key("a2"));
    world.write_outside(&records(vec![put(aux(), &at, b"w")]), &[&tx]);
    world.write(&tx, vec![put(aux(), &at, b"t")]);
    assert!(committed(world.commit(tx, &[])));
    assert_eq!(get(&world.begin(), aux(), &at), Some(b"t".to_vec()));

    // A6: the same subtree and key in another column do not meet.
    let (tx, at) = (world.begin(), key("a6"));
    world.write(&tx, vec![put(aux(), &at, b"t")]);
    let other = Unit::new(Column::Meta, aux().subtree);
    world.write_outside(&records(vec![put(other, &at, b"w")]), &[&tx]);
    assert_eq!(tx.txn.stats().overlapped, 0);
    assert!(committed(world.commit(tx, &[])));

    // A7: W deletes K, which never existed. A7b: W puts the value K had.
    let (tx, at) = (world.begin(), key("a7"));
    world.write(&tx, vec![put(aux(), &at, b"t")]);
    world.write_outside(&records(vec![delete(aux(), &at)]), &[&tx]);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    let at = key("a7b");
    world.write_outside(&records(vec![put(aux(), &at, b"same")]), &[]);
    let tx = world.begin();
    world.write(&tx, vec![put(aux(), &at, b"t")]);
    world.write_outside(&records(vec![put(aux(), &at, b"same")]), &[&tx]);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // A8: T puts K1; W puts K2; T puts K2; T commits.
    let (tx, one, two) = (world.begin(), key("a8"), key("a8"));
    world.write(&tx, vec![put(aux(), &one, b"t")]);
    world.write_outside(&records(vec![put(aux(), &two, b"w")]), &[&tx]);
    world.write(&tx, vec![put(aux(), &two, b"t")]);
    assert!(committed(world.commit(tx, &[])));
    assert_eq!(get(&world.begin(), aux(), &two), Some(b"t".to_vec()));

    // A9: T puts K; W puts K; T puts K again. The first write is met.
    let (tx, at) = (world.begin(), key("a9"));
    world.write(&tx, vec![put(aux(), &at, b"t")]);
    world.write_outside(&records(vec![put(aux(), &at, b"w")]), &[&tx]);
    world.write(&tx, vec![put(aux(), &at, b"t again")]);
    assert_eq!(get(&tx, aux(), &at), Some(b"t again".to_vec()));
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // A10, CF9: a range delete outside over a key T wrote always conflicts,
    // and so does a key written outside inside a range T deleted.
    let (tx, at) = (world.begin(), key("a10"));
    world.write(&tx, vec![put(aux(), &at, b"t")]);
    let range = Write::Range(aux(), k("a10"), Some(k("a11")));
    world.write_outside(&records(vec![range.clone()]), &[&tx]);
    assert_eq!(get(&tx, aux(), &at), Some(b"t".to_vec()));
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    let tx = world.begin();
    world.write(&tx, vec![range]);
    world.write_outside(&records(vec![put(aux(), &at, b"w")]), &[&tx]);
    assert_eq!(
        get(&tx, aux(), &at),
        None,
        "its range delete hides the record"
    );
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // A14: T puts K; W puts K; T rolls back entirely, puts K, commits.
    let (tx, at) = (world.begin(), key("a14"));
    world.write(&tx, vec![put(aux(), &at, b"t")]);
    world.write_outside(&records(vec![put(aux(), &at, b"w")]), &[&tx]);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    world.write(&tx, vec![put(aux(), &at, b"t again")]);
    assert!(committed(world.commit(tx, &[])));
    assert_eq!(get(&world.begin(), aux(), &at), Some(b"t again".to_vec()));

    // A15: T writes nothing; W puts K; T commits.
    let tx = world.begin();
    world.write_outside(&records(vec![put(aux(), &key("a15"), b"w")]), &[&tx]);
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
    world.check_committed();
}

/// The cases of 16.3 that a rollback to a mark decides (WT8, CF4): what a
/// savepoint and a scope build on.
#[test]
fn a_rollback_to_a_mark_takes_its_writes_out_of_the_reads_and_of_the_conflict_rule() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    let rewind = |tx: &Tx<'_>, mark: Mark, writes: usize| {
        tx.txn.rewind(mark);
        tx.model.borrow_mut().writes.truncate(writes);
    };

    // A4, A4b: T marks; puts K; W puts K; T rolls back to the mark. It
    // reads W's value, not the state of the moment of the mark, and
    // commits.
    let tx = world.begin();
    let mark = tx.txn.mark();
    world.write(&tx, vec![put(aux(), b"a4", b"t")]);
    world.write_outside(&records(vec![put(aux(), b"a4", b"w")]), &[&tx]);
    assert_eq!(get(&tx, aux(), b"a4"), Some(b"t".to_vec()));
    rewind(&tx, mark, 0);
    assert_eq!(get(&tx, aux(), b"a4"), Some(b"w".to_vec()));
    assert_eq!(
        tx.txn.stats(),
        TxnStats {
            keyed_reads: 0,
            ..TxnStats::default()
        },
        "it holds nothing again"
    );
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);

    // A5: T puts K; marks; puts K; W puts K; T rolls back to the mark. The
    // write from before the mark stays, with its first-write version.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(), b"a5", b"t")]);
    let mark = tx.txn.mark();
    world.write(&tx, vec![put(aux(), b"a5", b"t again")]);
    world.write_outside(&records(vec![put(aux(), b"a5", b"w")]), &[&tx]);
    rewind(&tx, mark, 1);
    assert_eq!(get(&tx, aux(), b"a5"), Some(b"t".to_vec()));
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // A5b: T marks; puts K; W puts K; T rolls back; puts K; commits.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(), b"other", b"kept")]);
    let mark = tx.txn.mark();
    world.write(&tx, vec![put(aux(), b"a5b", b"t")]);
    world.write_outside(&records(vec![put(aux(), b"a5b", b"w")]), &[&tx]);
    assert_eq!(get(&tx, aux(), b"a5b"), Some(b"t".to_vec()));
    assert!(tx.txn.stats().overlapped > 0);
    rewind(&tx, mark, 1);
    world.write(&tx, vec![put(aux(), b"a5b", b"t again")]);
    let stats = tx.txn.stats();
    assert_eq!((stats.overlapped, stats.base), (0, Some(world.latest())));
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    assert_eq!(
        get(&world.begin(), aux(), b"a5b"),
        Some(b"t again".to_vec())
    );

    // A16: marks 1 and 2 with K before 2 and K2 after it; W puts K; T rolls
    // back to 2.
    let tx = world.begin();
    world.write(&tx, vec![put(aux(), b"a16", b"t")]);
    let second = tx.txn.mark();
    world.write(&tx, vec![put(aux(), b"a16-2", b"t")]);
    world.write_outside(&records(vec![put(aux(), b"a16", b"w")]), &[&tx]);
    rewind(&tx, second, 1);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
}

/// T-RB2: after a rebase with an empty overlap set the transaction commits,
/// and the committed pages pass the physical check.
#[test]
fn a_transaction_is_rebased_over_commits_that_wrote_other_merks() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(21);
    let key = |index: usize| format!("node-{index:03}").into_bytes();
    for name in [1, 2, 3] {
        world.edit_outside(
            name,
            |items| {
                for index in 0..50 {
                    items.insert(key(index * 2), value(&mut rng, 120));
                }
            },
            &[],
        );
    }
    let tx = world.begin();
    world.write(&tx, vec![put(aux(), b"x", b"1")]);
    world.edit(&tx, 1, |items| {
        for index in 0..30 {
            items.insert(key(index * 2 + 1), value(&mut rng, 150));
        }
        items.remove(&key(10));
    });
    let (base, pages) = (tx.txn.stats().base, tx.txn.stats().tree_pages);
    assert!(pages > 2, "the Merk is in several pages");

    // Commits outside rewrite the two other Merks, with splits and frees,
    // and flat records. None writes a key of the transaction.
    for round in 0..4 {
        world.edit_outside(
            2 + round % 2,
            |items| {
                for index in 0..40 {
                    if rng.chance(50) {
                        items.remove(&key(index * 2));
                    } else {
                        items.insert(key(200 + round as usize * 50 + index), value(&mut rng, 180));
                    }
                }
            },
            &[&tx],
        );
        world.write_outside(&records(vec![put(aux(), b"y", &[round])]), &[&tx]);
        // Each of its operations brings the transaction to the latest
        // version: its frames are made again from its write set.
        world.check(&tx);
        let stats = tx.txn.stats();
        assert_eq!((stats.overlapped, stats.base), (0, Some(world.latest())));
        assert_eq!(stats.pinned, stats.base);
    }
    assert_ne!(tx.txn.stats().base, base);
    assert_eq!(tx.txn.stats().rebuilds, 4);
    // It writes on, on the rebased frames, and commits.
    world.edit(&tx, 1, |items| {
        items.insert(key(999), b"after the rebase".to_vec());
    });
    world.edit(&tx, 3, |items| {
        items.insert(key(998), b"a Merk a commit outside wrote before".to_vec());
    });
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// The steps of 14.3 at the level of records, with two Merks under a root
/// Merk: the transaction is overlapped, reads its own nodes and the
/// committed ones beside them, writes on, and fails at commit.
#[test]
fn an_overlapped_transaction_serves_its_records_as_they_are() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    // The root Merk holds an element for each of the Merks A (1) and B (2).
    fn root_edit<'s>(world: &World<'s>, tx: &Tx<'s>, a: &[u8], b: &[u8]) -> model::OwnedMerk {
        world.merk(tx, 0, |items| {
            items.insert(b"A".to_vec(), a.to_vec());
            items.insert(b"B".to_vec(), b.to_vec());
        })
    }
    let setup = world.begin();
    let merks = vec![root_edit(&world, &setup, b"empty", b"empty")];
    let roots = merks.iter().filter_map(World::root_record).collect();
    (world.apply(
        &setup,
        &OwnedBatch {
            merks,
            records: roots,
        },
    ))
    .expect("the root Merk");
    assert!(committed(world.commit(setup, &[])));

    // An insert into a Merk rewrites the element that hosts it, in the
    // root Merk, as every write propagates to the root (14.1).
    fn insert<'s>(world: &World<'s>, tx: &Tx<'s>, merk: u8, name: &str, a: &[u8], b: &[u8]) {
        let merks = vec![
            world.merk(tx, merk, |items| {
                items.insert(name.as_bytes().to_vec(), name.as_bytes().to_vec());
            }),
            root_edit(world, tx, a, b),
        ];
        let records = merks.iter().filter_map(World::root_record).collect();
        (world.apply(tx, &OwnedBatch { merks, records })).expect("an insert");
    }
    // The element of A in the root Merk as the transaction reads it, with
    // the hashes of its children: every write of the transaction rewrites
    // it.
    let root_hash = |tx: &Tx<'_>| {
        let hashes = |view: Option<NodeView<'_>>| {
            let view = view.expect("the root Merk has its elements");
            let child = |side| view.link(side).map(|link| link.hash.to_vec());
            let own = (view.value_hash().to_vec(), view.kv_hash().to_vec());
            (own, child(Side::Left), child(Side::Right))
        };
        (tx.txn.node(model::subtree(0), b"A", hashes)).expect("a read")
    };

    // 1. T inserts A/a1.
    let tx = world.begin();
    insert(&world, &tx, 1, "a1", b"{a1}", b"empty");
    let with_a1 = root_hash(&tx);
    // 2. A write with no transaction inserts B/b1. T still reads its own
    //    root nodes, which carry the old state of B.
    let outside = world.begin();
    insert(&world, &outside, 2, "b1", b"empty", b"{b1}");
    assert!(committed(world.commit(outside, &[&tx])));
    assert_eq!(root_hash(&tx), with_a1);
    let stats = tx.txn.stats();
    assert!(stats.overlapped > 0);
    assert_eq!(
        (stats.base, stats.pinned, stats.tree_pages),
        (None, None, 0)
    );
    // 3. T reads B/b1: it never wrote that node.
    let found = |view: Option<NodeView<'_>>| view.map(|view| view.is_record());
    let b1 = (tx.txn.node(model::subtree(2), b"b1", found)).expect("a read");
    assert_eq!(b1, Some(false), "the committed node");
    let a1 = (tx.txn.node(model::subtree(1), b"a1", found)).expect("a read");
    assert_eq!(a1, Some(true), "its own node, from the write set");
    world.check(&tx);
    // 4. T inserts A/a2: it writes on, by key.
    insert(&world, &tx, 1, "a2", b"{a1,a2}", b"empty");
    world.check(&tx);
    // 5. T inserts B/b2: this write read B as committed.
    insert(&world, &tx, 2, "b2", b"{a1,a2}", b"{b1,b2}");
    let view = tx
        .model
        .borrow()
        .view(&world.model.records, Unit::data(model::subtree(2)));
    assert_eq!(view.len(), 2, "b1 and b2");
    world.check(&tx);
    // 6. T commits: conflict. 7. The database keeps b1 and nothing of T.
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
    let reader = world.begin();
    let b = (reader.txn.node(model::subtree(2), b"b1", found)).expect("a read");
    let a = (reader.txn.node(model::subtree(1), b"a1", found)).expect("a read");
    assert_eq!((a, b), (None, Some(false)));
}

/// A node that was rewritten without its value keeps that value after a
/// commit outside overwrote it, and after the transaction gave up the
/// version it read it at (WT3, T-VL4 for a value in its cell).
#[test]
fn a_kept_value_is_the_transactions_own_after_a_commit_outside_overwrote_it() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let key = |index: u8| vec![b'k', index];
    world.edit_outside(
        1,
        |items| {
            for index in 0..20 {
                items.insert(key(index), vec![index; 60]);
            }
        },
        &[],
    );
    // T changes one value. Every node on the way to the root is written
    // with its value kept.
    let tx = world.begin();
    world.edit(&tx, 1, |items| {
        items.insert(key(3), b"changed".to_vec());
    });
    let kept: Vec<Vec<u8>> = (tx.model.borrow().writes.iter())
        .filter_map(|(write, ..)| match write {
            Write::Put(_, at, Val::Node(_)) if *at != key(3) => Some(at.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !kept.is_empty(),
        "nodes were written with their values kept"
    );
    // A commit outside gives every node another value.
    world.edit_outside(
        1,
        |items| {
            for value in items.values_mut() {
                *value = b"overwritten outside".to_vec();
            }
        },
        &[&tx],
    );
    let value = |view: Option<NodeView<'_>>| match view.expect("a node").value() {
        NodeValue::Bytes(bytes) => bytes.to_vec(),
        other => panic!("{other:?}"),
    };
    for at in &kept {
        let index = at[1];
        let shown = (tx.txn.node(model::subtree(1), at, value)).expect("a read");
        assert_eq!(shown, vec![index; 60], "the value it kept");
        let now = (tx.txn.committed_node(model::subtree(1), at, value)).expect("a read");
        assert_eq!(now, b"overwritten outside");
    }
    assert_eq!(tx.txn.stats().pinned, None, "it pins nothing any more");
    // More commits, which free and reuse the pages the values were in.
    for round in 0..4u8 {
        world.edit_outside(
            1,
            |items| {
                items.clear();
                for index in 0..30 {
                    items.insert(key(index), vec![round; 100]);
                }
            },
            &[&tx],
        );
        world.edit_outside(1, |items| items.clear(), &[&tx]);
    }
    for at in &kept {
        let shown = (tx.txn.node(model::subtree(1), at, value)).expect("a read");
        assert_eq!(shown, vec![at[1]; 60]);
    }
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
}

/// Two transactions each make the first node of one Merk that was empty,
/// and write no record in common, which no writer on an existing
/// GroveVersion does (GM3). The conflict rule does not see it. The second
/// transaction is not laid out over the other's root: it serves its
/// records by key, and its commit is refused.
#[test]
fn a_root_that_two_transactions_made_in_an_empty_merk_is_refused() {
    let (db, versions) = Db::new(48);
    let mut world = World::new(&db, &versions);
    fn make<'s>(world: &World<'s>, tx: &Tx<'s>, name: &[u8]) {
        let merks = vec![world.merk(tx, 5, |items| {
            items.insert(name.to_vec(), b"a first node".to_vec());
        })];
        let records = Vec::new();
        (world.apply(tx, &OwnedBatch { merks, records })).expect("a first node");
    }
    let (first, second) = (world.begin(), world.begin());
    make(&world, &first, b"one");
    make(&world, &second, b"two");
    assert!(committed(world.commit(first, &[&second])));

    // By key the second reads both nodes.
    world.check(&second);
    let stats = second.txn.stats();
    assert_eq!(
        (stats.overlapped, stats.base, stats.pinned),
        (0, None, None)
    );
    // It can still write, by key.
    world.write(&second, vec![put(aux(), b"k", b"v")]);
    world.check(&second);
    let refused = db.commit(&versions, &second.txn).expect_err("two roots");
    assert!(
        matches!(refused, TxnError::Batch(BatchError::RootTaken { subtree }) if subtree == model::subtree(5)),
        "{refused}"
    );
    // The transaction is as it was, and can be rolled back.
    world.check(&second);
    second.txn.rollback().expect("no scope is open");
    second.forget();
    world.check(&second);
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}
