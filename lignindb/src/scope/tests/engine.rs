//! Write scopes on a store on disk (`crate::scope`, `transactions.md`
//! section 9; tests T-SC1 to T-SC6), and the conflict rule under a dropped
//! subtree (CF5).
//!
//! This file is a part of the transaction's tests, whose store and model it
//! uses. The scopes' own structures, and two scopes under every
//! interleaving of their operations, are in `scope/tests`.

use std::{
    cell::RefCell,
    sync::{Barrier, Mutex},
    thread,
};

use lignin_format::{tree_page::Side, CellAddress};

use super::{
    model::{self, Committed, OwnedBatch, OwnedMerk, OwnedSubtree},
    store::{Db, Outcome, Rng, ENTERED},
    *,
};

/// The keys and values of one Merk.
type Items = BTreeMap<Vec<u8>, Vec<u8>>;

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

fn key(index: usize) -> Vec<u8> {
    format!("n{index:04}").into_bytes()
}

fn data(name: u8) -> Unit {
    Unit::data(model::subtree(name))
}

fn aux(name: u8) -> Unit {
    model::unit(Column::Aux, name)
}

fn held(error: &TxnError, by: Unit) -> bool {
    matches!(error, TxnError::ScopeOverlap { unit } if *unit == by)
}

/// Which scope made each write of a transaction's model, while the scope
/// is open: the model has the writes of one transaction in the order they
/// were made, and a scope that is rolled back takes its own out.
#[derive(Default)]
struct Owners(RefCell<Vec<u32>>);

impl Owners {
    /// Applies `batch` through `scope`, which is the one of the number
    /// `tag`, and tells the model.
    fn apply<'s>(
        &self,
        world: &World<'s>,
        tx: &Tx<'s>,
        (tag, scope): (u32, &Scope<'_, 's>),
        batch: &OwnedBatch,
    ) -> Result<Applied, TxnError> {
        let first_write = world.latest().get();
        // The scope reads each node whose value it keeps, and names the
        // read (`OwnedBatch::through`).
        let applied = (batch.through(scope)).with(|batch| scope.apply(batch))?;
        let before = tx.model.borrow().writes.len();
        world.note(tx, batch, first_write);
        let mut owners = self.0.borrow_mut();
        owners.resize(before, 0);
        owners.resize(tx.model.borrow().writes.len(), tag);
        Ok(applied)
    }

    /// One change of the Merk `name` through `scope`, with the record that
    /// a change of its root writes beside it.
    fn edit<'s>(
        &self,
        world: &World<'s>,
        tx: &Tx<'s>,
        scope: (u32, &Scope<'_, 's>),
        name: u8,
        edit: impl FnOnce(&mut Items),
    ) -> Result<Applied, TxnError> {
        let merk = world.merk(tx, name, edit);
        let records = World::root_record(&merk).into_iter().collect();
        let batch = OwnedBatch {
            merks: vec![merk],
            records,
            ..OwnedBatch::default()
        };
        self.apply(world, tx, scope, &batch)
    }

    /// The scope of the number `tag` was rolled back: its writes leave the
    /// model.
    fn forget(&self, tx: &Tx<'_>, tag: u32) {
        let mut owners = self.0.borrow_mut();
        let mut model = tx.model.borrow_mut();
        owners.resize(model.writes.len(), 0);
        let mut at = 0;
        model.writes.retain(|_| {
            at += 1;
            owners[at - 1] != tag
        });
        owners.retain(|owner| *owner != tag);
    }
}

/// Checks every node of the Merks `names` as `scope` reads them against
/// the model of `tx`, which has the scope's writes.
fn check_through<'s>(world: &World<'s>, tx: &Tx<'s>, scope: &Scope<'_, 's>, names: &[u8]) {
    let model = tx.model.borrow();
    for name in names {
        let unit = data(*name);
        for key in world.keys(tx, unit) {
            let expected = model.read(&world.model.records, unit, &key);
            let shown = |view: Option<NodeView<'_>>| match (expected, view) {
                (None, None) => {}
                (Some(Val::Node(node)), Some(view)) => node.check(&key, &view),
                (expected, view) => panic!("{key:?} of {unit}: {expected:?}, {view:?}"),
            };
            (scope.node(unit.subtree, &key, shown)).expect("the scope reads its Merk");
        }
    }
}

/// A store with the Merks `1` to `last` committed, each with a few nodes.
fn planted(world: &mut World<'_>, last: u8) {
    let tx = world.begin();
    for name in 1..=last {
        world.edit(&tx, name, |items| {
            for node in 0..4 + usize::from(name) {
                items.insert(key(node * 2), vec![name; 30 + node * 9]);
            }
        });
    }
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// T-SC2: each overlap is refused at once and leaves the scope usable:
/// write against write, read against write, write against read (SC3). A
/// reader outside any scope is a scope that ends with its call. Scopes
/// that only read one unit do not overlap, and a scope that ends gives up
/// what it held.
#[test]
fn an_overlap_is_refused_at_once_and_leaves_the_scope_usable() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    planted(&mut world, 4);
    let tx = world.begin();
    let owners = Owners::default();
    assert!(!tx.txn.scopes_open());
    let one = tx.txn.scope().expect("a scope");
    let two = tx.txn.scope().expect("any number of scopes");
    assert!(tx.txn.scopes_open());
    // The first writes on the transaction's pages, the second beside it
    // keeps its writes until it merges.
    assert!(one.is_laid_out() && !two.is_laid_out());
    assert!(format!("{two:?}").contains("Scope"));

    let applied = (owners.edit(&world, &tx, (1, &one), 1, |items| {
        items.insert(key(1), b"of the first".to_vec());
    }))
    .expect("nobody holds the Merk");
    let records = vec![put(aux(1), b"k", b"v")];
    let flat = OwnedBatch {
        records,
        ..OwnedBatch::default()
    };
    (owners.apply(&world, &tx, (1, &one), &flat)).expect("nobody holds the unit");

    // Write against write: a change set, and a flat record.
    let merk = world.merk(&tx, 1, |items| {
        drop(items.insert(key(3), b"second".to_vec()))
    });
    let batch = OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    let error = batch.with(|batch| two.apply(batch)).expect_err("held");
    assert!(held(&error, data(1)), "{error}");
    assert_eq!(
        error.to_string(),
        format!("another open scope of the transaction holds {}", data(1))
    );
    assert!(std::error::Error::source(&error).is_none());
    let error = flat.with(|batch| two.apply(batch)).expect_err("held");
    assert!(held(&error, aux(1)), "{error}");
    // Read against write: every read of the unit, through the scope and
    // outside any.
    let subtree = model::subtree(1);
    let mut out = Vec::new();
    let seen = |view: Option<NodeView<'_>>| view.is_some();
    assert!((two.node(subtree, &key(1), seen)).is_err_and(|error| held(&error, data(1))));
    assert!(two
        .follow(subtree, applied.nodes[0], &key(1), seen)
        .is_err_and(|error| held(&error, data(1))));
    assert!(two
        .child(subtree, applied.nodes[0], Side::Left, seen)
        .is_err_and(|error| held(&error, data(1))));
    assert!(two
        .committed_node(subtree, &key(0), seen)
        .is_err_and(|error| held(&error, data(1))));
    assert!((two.get(aux(1), b"k", &mut out)).is_err_and(|error| held(&error, aux(1))));
    assert!(two
        .committed_get(aux(1), b"k", &mut out)
        .is_err_and(|error| held(&error, aux(1))));
    let mut cursor = two.cursor(aux(1));
    assert!(cursor
        .seek_to_first()
        .is_err_and(|error| held(&error, aux(1))));
    let mut cursor = two
        .merk_cursor(subtree)
        .expect("a cursor reads when it moves");
    assert!(cursor
        .seek_to_first()
        .is_err_and(|error| held(&error, data(1))));
    assert!((tx.txn.node(subtree, &key(1), seen)).is_err_and(|error| held(&error, data(1))));
    assert!((tx.txn.get(aux(1), b"k", &mut out)).is_err_and(|error| held(&error, aux(1))));
    drop(cursor);

    // Read against read: both scopes and a reader outside read one Merk.
    // Write against read: neither scope writes it then.
    assert!(two.node(model::subtree(3), &key(0), seen).expect("a read"));
    assert!(one.node(model::subtree(3), &key(0), seen).expect("a read"));
    assert!(tx
        .txn
        .node(model::subtree(3), &key(0), seen)
        .expect("a read"));
    for (tag, scope) in [(1, &one), (2, &two)] {
        let error = (owners.edit(&world, &tx, (tag, scope), 3, |items| {
            items.insert(key(1), b"x".to_vec());
        }))
        .expect_err("the other scope has read it");
        assert!(held(&error, data(3)), "{error}");
    }
    // The other columns of a subtree are units of their own.
    assert!(!two.get(aux(3), b"k", &mut out).expect("a read"));

    // The scope that was refused is usable: it writes a Merk of its own,
    // and reads its writes, which are not laid out.
    (owners.edit(&world, &tx, (2, &two), 2, |items| {
        items.insert(key(1), b"of the second".to_vec());
        items.remove(&key(0));
    }))
    .expect("nobody holds the Merk");
    check_through(&world, &tx, &two, &[2]);
    check_through(&world, &tx, &one, &[1]);
    assert_eq!(tx.txn.stats().writes, tx.model.borrow().writes.len());

    // The first merges: what it held is free, and the second reads what
    // it wrote (SC4), and then writes the Merk itself.
    one.merge();
    check_through(&world, &tx, &two, &[1, 2]);
    (owners.edit(&world, &tx, (2, &two), 3, |items| {
        items.insert(key(1), b"x".to_vec());
    }))
    .expect("its own read is the only one left");
    two.merge();
    assert!(!tx.txn.scopes_open());
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// The Merk whose elements host the two small Merks of [`hosted`], and
/// those two, which no store has entered.
const PARENT: u8 = 7;
const SMALL: [u8; 2] = [ENTERED + 21, ENTERED + 22];

/// The key of the element of [`PARENT`] that hosts the Merk `SMALL[at]`.
fn host_of(at: usize) -> Vec<u8> {
    format!("host-{at}").into_bytes()
}

/// Commits [`PARENT`] with two elements that each host a small Merk, with
/// a flat record beside each. The small Merks are in the page of their
/// hosts (`tree-layout.md` P2), so two scopes that write one each write
/// Merks that share a page.
fn hosted(world: &mut World<'_>, db: &Db, versions: &Versions) {
    let tx = world.begin();
    let mut parent = world.merk(&tx, PARENT, |items| {
        for node in 0..6 {
            items.insert(key(node), vec![3; 40]);
        }
        for at in 0..SMALL.len() {
            items.insert(host_of(at), b"tree".to_vec());
        }
    });
    let mut batch = OwnedBatch::default();
    for (at, name) in SMALL.into_iter().enumerate() {
        let subtree = model::subtree(name);
        parent.hosted.insert(host_of(at), vec![subtree]);
        let nodes = (0..5).map(|node| (key(node * 2), vec![name; 30 + node * 7]));
        batch.merks.push(OwnedMerk::change(
            subtree,
            &BTreeMap::new(),
            &nodes.collect(),
        ));
        batch.records.push(put(aux(name), b"beside", &[name; 8]));
        let host = (model::subtree(PARENT), host_of(at), 0);
        batch
            .subtrees
            .push(OwnedSubtree::Enter(subtree, Some(host)));
    }
    batch.records.extend(World::root_record(&parent));
    batch.merks.push(parent);
    world.apply(&tx, &batch).expect("the Merks are entered");
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let roots = db.roots(versions, versions.latest());
    let page = |name| roots[&model::subtree(name)].page();
    assert_eq!(page(SMALL[0]), page(SMALL[1]), "the two Merks share a page");
}

/// A change of the Merk `SMALL[at]` that stops below its host: the batch
/// writes the Merk and nothing of [`PARENT`] (`tree-layout.md` T3).
fn stopped<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    at: usize,
    edit: impl FnOnce(&mut Items),
) -> OwnedBatch {
    let mut merk = world.merk(tx, SMALL[at], edit);
    merk.anchored = true;
    OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    }
}

/// The write that names the roots of the Merks `SMALL[at]` again in their
/// hosts, as finalize makes it: each element is written with the Merk it
/// hosts.
fn finalize<'s>(world: &World<'s>, tx: &Tx<'s>, which: &[usize], value: &[u8]) -> OwnedBatch {
    let mut parent = world.merk(tx, PARENT, |items| {
        for at in which {
            items.insert(host_of(*at), value.to_vec());
        }
    });
    for at in which {
        (parent.hosted).insert(host_of(*at), vec![model::subtree(SMALL[*at])]);
    }
    OwnedBatch {
        records: World::root_record(&parent).into_iter().collect(),
        merks: vec![parent],
        ..OwnedBatch::default()
    }
}

/// The cell of the node under `key` of the Merk `name` on the pages of
/// `tx`.
fn cell_of(tx: &Tx<'_>, name: u8, key: &[u8]) -> CellAddress {
    let handle = |view: Option<NodeView<'_>>| view.and_then(|view| view.handle());
    let handle = tx.txn.node(model::subtree(name), key, handle);
    handle.expect("a read").expect("a cell").address()
}

/// Checks that the pages of `tx` answer the tree layer "behind" for the
/// cells of the elements `hosts` of [`PARENT`], where they are now, and
/// for no other cell (`lignin_tree::fragment::Pages::host_is_behind`).
fn behind_are(tx: &Tx<'_>, hosts: &[usize]) {
    let cells = hosts.iter().map(|at| cell_of(tx, PARENT, &host_of(*at)));
    let mut cells: Vec<CellAddress> = cells.collect();
    cells.sort_unstable();
    let other = cell_of(tx, PARENT, &key(0));
    assert_eq!(tx.txn.behind(other), Some((cells, false)));
}

/// T-SC3: a scope that rolls back beside a scope that merges leaves
/// exactly the merging scope's writes (SC6), with their Merks in one page
/// (LT4): whichever of the two writes on the transaction's pages, and in
/// whichever order they end. Each write stops below its host, whose
/// element the scope marks; the mark of the scope that is rolled back
/// goes with it, and the other is taken off by the write that names the
/// root again.
#[test]
fn a_rollback_beside_a_merge_leaves_exactly_the_merged_writes() {
    for case in 0..8 {
        let (first_merges, merge_first, third) = (case & 1 == 0, case & 2 == 0, case & 4 == 0);
        let (db, versions) = Db::new(64);
        let mut world = World::new(&db, &versions);
        hosted(&mut world, &db, &versions);
        let parent = model::subtree(PARENT);
        let tx = world.begin();
        let owners = Owners::default();
        // With a third scope open first, neither of the two writes on the
        // pages before it is laid out.
        let other = third.then(|| tx.txn.scope().expect("a scope"));
        let first = tx.txn.scope().expect("a scope");
        let second = tx.txn.scope().expect("a scope");
        assert_eq!(first.is_laid_out(), !third);
        for round in 0..3 {
            for (at, scope) in [&first, &second].into_iter().enumerate() {
                let tag = at as u32 + 1;
                // The root of the Merk is another node after the write.
                let batch = stopped(&world, &tx, at, |items| {
                    items.insert(key(100 + round), vec![tag as u8; 40 + round]);
                    items.remove(&key(round * 2));
                });
                (owners.apply(&world, &tx, (tag, scope), &batch)).expect("a Merk of its own");
                scope.mark_pending(parent, &host_of(at)).expect("a mark");
                let records = vec![put(aux(SMALL[at]), &key(round), &[tag as u8; 9])];
                let batch = OwnedBatch {
                    records,
                    ..OwnedBatch::default()
                };
                (owners.apply(&world, &tx, (tag, scope), &batch)).expect("a unit of its own");
            }
        }
        check_through(&world, &tx, &first, &[SMALL[0]]);
        check_through(&world, &tx, &second, &[SMALL[1]]);
        let end = |merged: bool, tag: u32, scope: Scope<'_, '_>| match merged {
            true => scope.merge(),
            false => {
                scope.rollback();
                owners.forget(&tx, tag);
            }
        };
        match merge_first == first_merges {
            true => {
                end(first_merges, 1, first);
                end(!first_merges, 2, second);
            }
            false => {
                end(!first_merges, 2, second);
                end(first_merges, 1, first);
            }
        }
        drop(other);
        assert!(!tx.txn.scopes_open());
        // The writes of one scope are left, with its mark, and the pages
        // show them.
        let kept = usize::from(!first_merges);
        assert_eq!(
            tx.txn.stats().writes,
            tx.model.borrow().writes.len(),
            "{case}"
        );
        assert!(tx.txn.is_pending(parent, &host_of(kept)));
        assert!(!tx.txn.is_pending(parent, &host_of(1 - kept)));
        world.check(&tx);
        let mut out = Vec::new();
        let found = |at: usize, out: &mut Vec<u8>| {
            (tx.txn.get(aux(SMALL[at]), &key(0), out)).expect("a read")
        };
        assert!(found(kept, &mut out) && !found(1 - kept, &mut out));
        let commit = db.commit(&versions, &tx.txn);
        assert!(matches!(commit, Err(TxnError::NotFinalized)), "{commit:?}");

        let scope = tx.txn.scope().expect("a scope");
        let batch = finalize(&world, &tx, &[kept], b"tree, again");
        (owners.apply(&world, &tx, (3, &scope), &batch)).expect("the host is written");
        scope
            .clear_pending(parent, &host_of(kept))
            .expect("a mark is taken off");
        scope.merge();
        world.check(&tx);
        assert!(committed(world.commit(tx, &[])), "{case}");
        world.check_committed();
        assert_eq!(db.private_ids(), 0, "{case}");
    }
}

/// T-SC4: a commit and a rollback of the transaction need every scope
/// closed, and a commit needs no pending mark left (CM1, SC8, SC9). Marks
/// are shown to their scope and, once it merged, to everybody; a scope's
/// rollback and a rollback to a savepoint take them back. A snapshot is
/// always answered "no".
#[test]
fn a_commit_needs_every_scope_closed_and_no_mark_left() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    planted(&mut world, 3);
    let tx = world.begin();
    let owners = Owners::default();
    let subtree = model::subtree(1);
    let host = key(2);

    let scope = tx.txn.scope().expect("a scope");
    (owners.edit(&world, &tx, (1, &scope), 2, |items| {
        items.insert(key(1), b"x".to_vec());
    }))
    .expect("a write");
    let commit = db.commit(&versions, &tx.txn);
    assert!(matches!(commit, Err(TxnError::ScopeOpen)), "{commit:?}");
    assert!(matches!(tx.txn.rollback(), Err(TxnError::ScopeOpen)));
    assert_eq!(
        TxnError::ScopeOpen.to_string(),
        "a scope of the transaction is open"
    );

    // A mark is the scope's until it merges, and it is no claim: the
    // Merk of the marked element is anybody's to write.
    scope.mark_pending(subtree, &host).expect("a mark");
    assert!(scope.is_pending(subtree, &host));
    assert!(!scope.is_pending(subtree, &key(4)));
    assert!(!tx.txn.is_pending(subtree, &host));
    let beside = tx.txn.scope().expect("a scope");
    assert!(!beside.is_pending(subtree, &host));
    (owners.edit(&world, &tx, (2, &beside), 1, |items| {
        items.insert(key(1), b"y".to_vec());
    }))
    .expect("a mark claims nothing");
    // A scope that is rolled back takes its marks along.
    beside.mark_pending(subtree, &key(4)).expect("a mark");
    assert!(beside.is_pending(subtree, &key(4)));
    beside.rollback();
    owners.forget(&tx, 2);
    scope.merge();
    assert!(tx.txn.is_pending(subtree, &host));
    assert!(!tx.txn.is_pending(subtree, &key(4)));

    // The transaction is not committed while the mark stands, and stays
    // as it was.
    let commit = db.commit(&versions, &tx.txn);
    assert!(matches!(commit, Err(TxnError::NotFinalized)), "{commit:?}");
    assert_eq!(
        TxnError::NotFinalized.to_string(),
        "the transaction has a pending mark left"
    );
    assert!(std::error::Error::source(&TxnError::NotFinalized).is_none());
    world.check(&tx);
    let reads = db.reads(&versions);
    assert!(!reads.snapshot().is_pending(subtree, &host));

    // A rollback to a savepoint takes back the marks that were set after
    // it, and puts back those that were taken off (WT8).
    tx.txn.set_savepoint().expect("no scope is open");
    let scope = tx.txn.scope().expect("a scope");
    assert!(scope.is_pending(subtree, &host));
    scope
        .clear_pending(subtree, &host)
        .expect("a mark is taken off");
    scope.mark_pending(subtree, &key(6)).expect("a mark");
    assert!(!scope.is_pending(subtree, &host));
    assert!(tx.txn.is_pending(subtree, &host), "until the scope merges");
    scope.merge();
    assert!(!tx.txn.is_pending(subtree, &host));
    assert!(tx.txn.is_pending(subtree, &key(6)));
    tx.txn.rollback_to_savepoint().expect("a savepoint is set");
    assert!(tx.txn.is_pending(subtree, &host));
    assert!(!tx.txn.is_pending(subtree, &key(6)));
    world.check(&tx);

    // Finalized: the mark is taken off, and the commit goes through.
    let scope = tx.txn.scope().expect("a scope");
    scope
        .clear_pending(subtree, &host)
        .expect("a mark is taken off");
    scope.merge();
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();

    // A rollback of the transaction takes every mark off.
    let tx = world.begin();
    let scope = tx.txn.scope().expect("a scope");
    scope.mark_pending(subtree, &host).expect("a mark");
    scope.merge();
    assert!(tx.txn.is_pending(subtree, &host));
    tx.txn.rollback().expect("no scope is open");
    assert!(!tx.txn.is_pending(subtree, &host));
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
}

/// T-SC5, the directory: scopes write Merks whose entries in the subtree
/// directory share a page, and each moves its root. Every entry names its
/// Merk's root after either order of the merges, also where a scope's
/// writes are laid out before it merges and where one is rolled back
/// after that.
#[test]
fn the_roots_of_merks_that_scopes_write_side_by_side_are_where_the_directory_says() {
    for case in 0..4 {
        let (db, versions) = Db::new(64);
        let mut world = World::new(&db, &versions);
        planted(&mut world, 6);
        let tx = world.begin();
        let owners = Owners::default();
        let scopes: Vec<Scope<'_, '_>> = (0..3).map(|_| tx.txn.scope().expect("a scope")).collect();
        for round in 0..4 {
            for (at, scope) in scopes.iter().enumerate() {
                let (tag, name) = (at as u32 + 1, at as u8 + 1);
                // Every node the Merk had goes, a few at a time, and more
                // come: its root is another node again and again.
                (owners.edit(&world, &tx, (tag, scope), name, |items| {
                    let old: Vec<Vec<u8>> = items.keys().take(2).cloned().collect();
                    for key in old {
                        items.remove(&key);
                    }
                    for node in 0..5 {
                        items.insert(key(200 + round * 10 + node), vec![name; 50]);
                    }
                }))
                .expect("a Merk of its own");
            }
        }
        let mut scopes: Vec<(u32, Scope<'_, '_>)> = (1..).zip(scopes).collect();
        if case & 1 == 1 {
            scopes.reverse();
        }
        if case & 2 == 2 {
            // The last one's writes are laid out while the others are
            // open, and it writes on the pages from then on.
            let (tag, scope) = scopes.last().expect("three scopes");
            scope.lay_out().expect("its writes are laid out");
            assert!(scope.is_laid_out());
            scope.lay_out().expect("nothing is left to lay out");
            let applied = (owners.edit(&world, &tx, (*tag, scope), *tag as u8, |items| {
                items.insert(key(900), b"on the pages".to_vec());
            }))
            .expect("a write on the pages");
            assert!(!applied.nodes.is_empty(), "it is given its handles now");
        }
        for (tag, scope) in scopes {
            check_through(&world, &tx, &scope, &[tag as u8]);
            match (case, tag) {
                (2, 1) => {
                    scope.rollback();
                    owners.forget(&tx, tag);
                }
                _ => scope.merge(),
            }
        }
        world.check(&tx);
        assert!(committed(world.commit(tx, &[])), "{case}");
        // The check of the committed pages compares every entry of the
        // directory with the cell it names.
        world.check_committed();
        assert_eq!(db.private_ids(), 0, "{case}");
    }
}

/// The subtree of the tests' hosted Merk, which no store has entered.
const INNER: u8 = ENTERED + 20;
/// The Merk that hosts it, and the Merk beside that one.
const OUTER: u8 = 9;
const BESIDE: u8 = 8;

/// Enters [`INNER`] under the element `host` of [`OUTER`], with nodes, and
/// with `flat` also flat records in two columns, through a transaction of
/// its own. [`OUTER`] has a flat record beside its nodes.
fn nested(world: &mut World<'_>, flat: bool) {
    let tx = world.begin();
    let mut outer = world.merk(&tx, OUTER, |items| {
        items.insert(b"host".to_vec(), b"tree".to_vec());
        for node in 0..6 {
            items.insert(key(node), vec![3; 40]);
        }
    });
    let inner = model::subtree(INNER);
    outer.hosted.insert(b"host".to_vec(), vec![inner]);
    let merk = OwnedMerk::change(
        inner,
        &BTreeMap::new(),
        &(0..5).map(|node| (key(node), vec![7; 60])).collect(),
    );
    let mut records: Vec<Write> = [&outer, &merk]
        .into_iter()
        .filter_map(World::root_record)
        .collect();
    if flat {
        records.push(put(aux(INNER), b"below", b"an aux record"));
        records.push(put(
            model::unit(Column::Meta, INNER),
            b"below",
            b"a meta record",
        ));
    }
    records.push(put(aux(OUTER), b"own", b"beside the nodes"));
    let host = (model::subtree(OUTER), b"host".to_vec(), 0);
    let batch = OwnedBatch {
        merks: vec![merk, outer],
        records,
        subtrees: vec![OwnedSubtree::Enter(inner, Some(host))],
    };
    world.apply(&tx, &batch).expect("the Merk is entered");
    world.edit(&tx, BESIDE, |items| {
        drop(items.insert(key(0), b"beside".to_vec()))
    });
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A batch that drops [`OUTER`], with everything below it.
fn drop_outer() -> OwnedBatch {
    OwnedBatch {
        subtrees: vec![OwnedSubtree::Drop(model::subtree(OUTER))],
        ..OwnedBatch::default()
    }
}

/// A drop in a scope (CV9, SC3): the scope holds every unit below the
/// dropped subtree, in every column, so that no other scope reads its
/// drop as "nothing is there", and it reads none of what it dropped
/// itself, also while the pages still show it. A scope that is rolled
/// back leaves the subtree as it was, and one that merges takes it out,
/// with the flat records of the Merk below.
#[test]
fn a_scope_that_drops_a_subtree_holds_everything_below_it() {
    for case in 0..4 {
        let (laid, merges) = (case & 1 == 0, case & 2 == 0);
        let (db, versions) = Db::new(64);
        let mut world = World::new(&db, &versions);
        nested(&mut world, true);
        let tx = world.begin();
        let owners = Owners::default();
        let other = tx.txn.scope().expect("a scope");
        let dropper = tx.txn.scope().expect("a scope");
        // A drop above a unit that another scope holds is refused.
        let seen = |view: Option<NodeView<'_>>| view.is_some();
        let mut out = Vec::new();
        assert!(other.get(aux(INNER), b"below", &mut out).expect("a read"));
        let error = (owners.apply(&world, &tx, (2, &dropper), &drop_outer())).expect_err("held");
        assert!(held(&error, aux(INNER)), "{error}");
        other.rollback();
        let other = tx.txn.scope().expect("a scope");
        if laid {
            dropper.lay_out().expect("nothing to lay out");
        }

        (owners.apply(&world, &tx, (2, &dropper), &drop_outer())).expect("the subtree is dropped");
        // The dropping scope reads nothing under the subtree or below it.
        let inner = model::subtree(INNER);
        assert!(!dropper.node(inner, &key(0), seen).expect("a read"));
        assert!(!dropper
            .node(model::subtree(OUTER), b"host", seen)
            .expect("a read"));
        assert!(!dropper.get(aux(INNER), b"below", &mut out).expect("a read"));
        let mut cursor = dropper.merk_cursor(inner).expect("a cursor");
        cursor.seek_to_first().expect("a seek");
        assert!(!cursor.valid());
        drop(cursor);
        // Nobody else reaches below it, in any column, by any read.
        let above = data(OUTER);
        assert!((other.node(inner, &key(0), seen)).is_err_and(|error| held(&error, above)));
        assert!((other.get(aux(INNER), b"below", &mut out)).is_err_and(|error| held(&error, above)));
        assert!((tx.txn.node(inner, &key(0), seen)).is_err_and(|error| held(&error, above)));
        let merk = world.merk(&tx, INNER, |items| {
            drop(items.insert(key(9), b"late".to_vec()))
        });
        let batch = OwnedBatch {
            merks: vec![merk],
            ..OwnedBatch::default()
        };
        // The dropping scope has read that Merk, which is one more claim.
        let error = batch.with(|batch| other.apply(batch)).expect_err("held");
        assert!(held(&error, data(INNER)), "{error}");
        // What is beside the dropped subtree is free.
        assert!(other
            .node(model::subtree(BESIDE), &key(0), seen)
            .expect("a read"));

        // The dropping scope enters the subtree again: it starts empty, in
        // every column, and holds what the scope writes now.
        let outer = model::subtree(OUTER);
        let merk = OwnedMerk::change(
            outer,
            &BTreeMap::new(),
            &Items::from([(key(1), b"again".to_vec())]),
        );
        let batch = OwnedBatch {
            records: vec![put(aux(OUTER), b"new", b"after the drop")],
            merks: vec![merk],
            subtrees: vec![OwnedSubtree::Enter(outer, None)],
        };
        (owners.apply(&world, &tx, (2, &dropper), &batch)).expect("it is entered again");
        check_through(&world, &tx, &dropper, &[OUTER]);
        assert!(dropper.node(outer, &key(1), seen).expect("a read"));
        assert!(!dropper.node(outer, &key(0), seen).expect("a read"));
        assert!(!dropper.get(aux(OUTER), b"own", &mut out).expect("a read"));
        assert!(dropper.get(aux(OUTER), b"new", &mut out).expect("a read"));
        assert!(!dropper.node(inner, &key(0), seen).expect("a read"));

        match merges {
            true => dropper.merge(),
            false => {
                dropper.rollback();
                owners.forget(&tx, 2);
            }
        }
        // The other scope reads below the subtree again: what is there if
        // the drop was rolled back, and nothing if it merged.
        assert_eq!(other.node(inner, &key(0), seen).expect("a read"), !merges);
        assert_eq!(
            other.get(aux(INNER), b"below", &mut out).expect("a read"),
            !merges
        );
        other.merge();
        world.check(&tx);
        // A transaction whose one writer was rolled back commits nothing.
        assert_eq!(committed(world.commit(tx, &[])), merges, "{case}");
        world.check_committed();
        let reader = world.begin();
        let found = reader
            .txn
            .get(aux(INNER), b"below", &mut out)
            .expect("a read");
        assert_eq!(found, !merges, "{case}");
        let found = reader
            .txn
            .get(aux(OUTER), b"own", &mut out)
            .expect("a read");
        assert_eq!(found, !merges, "{case}");
        assert_eq!(db.private_ids(), 0, "{case}");
    }
}

/// A subtree that a batch enters is below its host from that batch on
/// (CV9, SC3): a scope cannot enter one below a subtree that another open
/// scope has dropped or cleared, also where its batch writes nothing of
/// the subtree above, and nobody drops or clears above a subtree that
/// another open scope has entered.
#[test]
fn a_subtree_is_below_its_host_from_the_batch_that_enters_it() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    nested(&mut world, false);
    let new = ENTERED + 26;
    let enter = OwnedBatch {
        subtrees: vec![OwnedSubtree::Enter(
            model::subtree(new),
            Some((model::subtree(INNER), key(0), 0)),
        )],
        ..OwnedBatch::default()
    };
    let leave = |clears: bool| OwnedBatch {
        subtrees: vec![match clears {
            true => OwnedSubtree::Clear(model::subtree(OUTER)),
            false => OwnedSubtree::Drop(model::subtree(OUTER)),
        }],
        ..OwnedBatch::default()
    };
    for case in 0..4 {
        let (clears, enters_first) = (case & 1 == 1, case & 2 == 2);
        let tx = world.begin();
        let owners = Owners::default();
        let leaver = tx.txn.scope().expect("a scope");
        let enterer = tx.txn.scope().expect("a scope");
        match enters_first {
            false => {
                (owners.apply(&world, &tx, (1, &leaver), &leave(clears))).expect("it leaves");
                let error = enter.with(|batch| enterer.apply(batch)).expect_err("held");
                assert!(held(&error, data(OUTER)), "{case}: {error}");
            }
            true => {
                (owners.apply(&world, &tx, (2, &enterer), &enter)).expect("it is entered");
                let leave = leave(clears);
                let error = leave.with(|batch| leaver.apply(batch)).expect_err("held");
                assert!(held(&error, data(new)), "{case}: {error}");
            }
        }
        enterer.rollback();
        leaver.rollback();
        tx.txn.rollback().expect("no scope is open");
    }
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// The look at what the drops of commits cover reads pages, and a read can
/// fail (CF5). The commits it was owed stay owed: the next operation, or
/// the commit, makes the look, and the transaction's drop is met.
#[test]
fn a_look_at_what_a_drop_covers_that_fails_is_made_again() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    nested(&mut world, false);
    for at_commit in [false, true] {
        let below = OwnedBatch {
            records: vec![put(aux(INNER), b"late", &[u8::from(at_commit); 4])],
            ..OwnedBatch::default()
        };
        let tx = world.begin();
        world.apply(&tx, &drop_outer()).expect("the drop");
        assert!(committed(world.write_outside(&below, &[&tx])));
        tx.txn.refuse_look();
        let refused = match at_commit {
            true => tx.txn.seal().map(drop),
            false => (tx.txn.get(aux(BESIDE), b"k", &mut Vec::new())).map(drop),
        };
        assert!(matches!(refused, Err(TxnError::Damaged(_))), "{refused:?}");
        assert_eq!(tx.txn.stats().overlapped, 0);
        if !at_commit {
            world.check(&tx);
            assert_eq!(tx.txn.stats().overlapped, 1, "the drop is met");
        }
        assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    }
    // A commit whose look fails answers with the conflict that the key
    // sets have shown it: the error is no reason to try again (CF6).
    let beside = |value: &[u8]| OwnedBatch {
        records: vec![put(aux(BESIDE), b"k", value)],
        ..OwnedBatch::default()
    };
    let tx = world.begin();
    world.apply(&tx, &drop_outer()).expect("the drop");
    world.apply(&tx, &beside(b"mine")).expect("a record beside");
    assert!(committed(world.write_outside(&beside(b"theirs"), &[&tx])));
    tx.txn.refuse_look();
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// CF5, both ways (D-090 item 4), with writes that name no key of the
/// subtree above, which is what a write that stops below its host is:
///
/// - a commit outside drops a subtree and enters it again, with the Merk
///   below it, and a transaction that had written a flat record below
///   fails with the conflict, and does not put its record into the new
///   Merk;
/// - a transaction drops a subtree, a commit outside writes a flat record
///   below it, and the transaction's commit fails with the conflict.
#[test]
fn a_drop_meets_every_write_below_it() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    // The Merk below has no flat record of its own at first: one that is
    // entered again below a dropped subtree shows the flat records of
    // before once it is committed, which is open in `crate::txn` ("What
    // is not here") and not what is shown here.
    nested(&mut world, false);
    let below = |value: &[u8]| OwnedBatch {
        records: vec![put(aux(INNER), b"late", value)],
        ..OwnedBatch::default()
    };

    // The transaction wrote below, and a commit outside drops above and
    // enters both again.
    let tx = world.begin();
    world.apply(&tx, &below(b"mine")).expect("a record below");
    let unmet = world.begin();
    world
        .apply(
            &unmet,
            &OwnedBatch {
                records: vec![put(aux(BESIDE), b"k", b"v")],
                ..OwnedBatch::default()
            },
        )
        .expect("a record beside");
    let outside = world.begin();
    world.apply(&outside, &drop_outer()).expect("the drop");
    let inner = model::subtree(INNER);
    let mut outer = world.merk(&outside, OUTER, |items| {
        items.insert(b"host".to_vec(), b"tree again".to_vec());
    });
    outer.hosted.insert(b"host".to_vec(), vec![inner]);
    let host = (model::subtree(OUTER), b"host".to_vec(), 0);
    let batch = OwnedBatch {
        records: World::root_record(&outer).into_iter().collect(),
        merks: vec![outer],
        subtrees: vec![
            OwnedSubtree::Enter(model::subtree(OUTER), None),
            OwnedSubtree::Enter(inner, Some(host)),
        ],
    };
    world
        .apply(&outside, &batch)
        .expect("both are entered again");
    assert!(committed(world.commit(outside, &[&tx, &unmet])));
    // Its write is met at its next operation: it serves its record by key
    // (WT7b), and its commit fails.
    world.check(&tx);
    assert_eq!(tx.txn.stats().overlapped, 1);
    assert_eq!(tx.txn.stats().base, None);
    assert_eq!(world.commit(tx, &[&unmet]), Outcome::Conflict);
    // A transaction that wrote beside the dropped subtree is not met.
    world.check(&unmet);
    assert_eq!(unmet.txn.stats().overlapped, 0);
    assert!(committed(world.commit(unmet, &[])));
    world.check_committed();
    let reader = world.begin();
    let mut out = Vec::new();
    assert!(!reader
        .txn
        .get(aux(INNER), b"late", &mut out)
        .expect("a read"));
    drop(reader);

    // The transaction drops the subtree, and a commit outside writes
    // below it, or clears the Merk below it.
    for clears in [false, true] {
        let tx = world.begin();
        world.apply(&tx, &drop_outer()).expect("the drop");
        let later = world.begin();
        world
            .apply(&later, &below(b"after"))
            .expect("a record below");
        let outside = match clears {
            false => below(b"theirs"),
            true => OwnedBatch {
                subtrees: vec![OwnedSubtree::Clear(inner)],
                ..OwnedBatch::default()
            },
        };
        assert!(committed(world.write_outside(&outside, &[&tx, &later])));
        world.check(&tx);
        assert_eq!(tx.txn.stats().overlapped, 1, "the drop is met");
        assert_eq!(world.commit(tx, &[&later]), Outcome::Conflict);
        // A write below that was made after the commit is not met by it.
        world.check(&later);
        let met = usize::from(!clears);
        assert_eq!(later.txn.stats().overlapped, met);
        later.txn.rollback().expect("no scope is open");
    }
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// Writes [`PARENT`] until a write carries the cell of the element that
/// hosts `SMALL[0]` to another place: nodes beside the element, and the
/// element itself with a longer value, which keeps the Merk it hosts.
/// Every read through `tx` is checked after each write.
fn grow<'s>(world: &World<'s>, tx: &Tx<'s>) {
    let host = cell_of(tx, PARENT, &host_of(0));
    for round in 0..8 {
        world.edit(tx, PARENT, |items| {
            for node in 0..14 {
                let beside = format!("host-0-0000-{round:03}-{node:02}");
                items.insert(beside.into_bytes(), vec![9; 180]);
            }
            items.insert(host_of(0), vec![4; 30 + round * 30]);
        });
        world.check(tx);
        if cell_of(tx, PARENT, &host_of(0)) != host {
            return;
        }
    }
    panic!("no write carried the host's cell");
}

/// A change set of the Merk `name` whose node links a child that the Merk
/// does not have.
fn broken<'s>(world: &World<'s>, tx: &Tx<'s>, name: u8) -> OwnedBatch {
    let mut merk = world.merk(tx, name, |items| drop(items.insert(key(1), b"x".to_vec())));
    let node = (merk.nodes.iter_mut())
        .find_map(|(_, node, _)| node.left.as_mut().or(node.right.as_mut()))
        .expect("a node with a child");
    node.key = b"no such node".to_vec();
    OwnedBatch {
        merks: vec![merk],
        ..OwnedBatch::default()
    }
}

/// T-SC5, the marks, and `tree-layout.md` T3: a write that stops below its
/// host leaves the host's pointer to the root behind, and marks the host's
/// element. While the mark stands the Merk is opened by its entry in the
/// directory, and a write that moves the host's cell does not follow the
/// pointer: also where the frames are made again from the write set, and
/// where the stopped write is laid out at its scope's merge. The write
/// that names the root again takes the mark off, and the committed pages
/// pass the check.
#[test]
fn the_pointer_of_a_marked_host_is_not_followed() {
    for case in 0..4 {
        let (beside, rebuilt, rebased) = (case == 1, case == 2, case == 3);
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        hosted(&mut world, &db, &versions);
        let parent = model::subtree(PARENT);
        let tx = world.begin();
        let owners = Owners::default();
        let other = beside.then(|| tx.txn.scope().expect("a scope"));
        let scope = tx.txn.scope().expect("a scope");
        assert_eq!(scope.is_laid_out(), !beside);
        let before = db.roots(&versions, versions.latest())[&model::subtree(SMALL[0])];
        // The Merk loses nodes until one is left: its root is another
        // node, and the cell that the host's pointer names holds none.
        for round in 0..4 {
            let batch = stopped(&world, &tx, 0, |items| {
                items.remove(&key(round * 2));
            });
            (owners.apply(&world, &tx, (1, &scope), &batch)).expect("its Merk");
            if round == 0 {
                scope.mark_pending(parent, &host_of(0)).expect("a mark");
            }
        }
        check_through(&world, &tx, &scope, &[SMALL[0]]);
        scope.merge();
        drop(other);
        assert!(tx.txn.is_pending(parent, &host_of(0)));
        behind_are(&tx, &[0]);

        // The parent grows until the cell of the marked host is carried.
        // The Merk below it is read by its entry all along.
        let saved = tx.model.borrow().writes.len();
        if rebuilt {
            tx.txn.set_savepoint().expect("no scope is open");
        }
        grow(&world, &tx);
        behind_are(&tx, &[0]);
        if rebuilt {
            // The frames are made again from the write set, with the mark
            // standing, and the parent grows again.
            let rebuilds = tx.txn.stats().rebuilds;
            tx.txn.rollback_to_savepoint().expect("a savepoint is set");
            tx.model.borrow_mut().writes.truncate(saved);
            assert!(tx.txn.is_pending(parent, &host_of(0)));
            behind_are(&tx, &[0]);
            grow(&world, &tx);
            assert!(tx.txn.stats().rebuilds > rebuilds);
            behind_are(&tx, &[0]);
        }
        if rebased {
            // A commit outside lands: the frames are made again over it,
            // with the write that carried the host among those that are
            // applied again.
            let rebuilds = tx.txn.stats().rebuilds;
            world.edit_outside(
                BESIDE,
                |items| drop(items.insert(key(7), b"x".to_vec())),
                &[&tx],
            );
            world.edit(&tx, PARENT, |items| {
                drop(items.insert(b"host-0-later".to_vec(), vec![9; 180]))
            });
            world.check(&tx);
            assert!(tx.txn.stats().rebuilds > rebuilds);
            behind_are(&tx, &[0]);
        }
        let root = |tx: &Tx<'_>| {
            let name = model::subtree(SMALL[0]);
            let records = tx
                .model
                .borrow()
                .view(&world.model.records, Unit::data(name));
            model::records(&Committed::items(&records)).0
        };
        assert!(root(&tx).is_some());

        let scope = tx.txn.scope().expect("a scope");
        let batch = finalize(&world, &tx, &[0], b"tree, again");
        (owners.apply(&world, &tx, (2, &scope), &batch)).expect("the host is written");
        scope
            .clear_pending(parent, &host_of(0))
            .expect("the mark is taken off");
        scope.merge();
        behind_are(&tx, &[]);
        world.check(&tx);
        assert!(committed(world.commit(tx, &[])), "{case}");
        world.check_committed();
        let after = db.roots(&versions, versions.latest())[&model::subtree(SMALL[0])];
        assert_ne!(before, after, "the root of the Merk is another cell");
        assert_eq!(db.private_ids(), 0, "{case}");
    }
}

/// An element that is deleted while it is marked, with the Merk it hosts,
/// is behind no more, whatever takes its cell: the pages answer for cells,
/// and the mark is on a key (`tree-layout.md` T3). The mark stays the
/// caller's to take off.
#[test]
fn a_marked_host_that_is_deleted_is_behind_no_more() {
    for beside in [false, true] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        hosted(&mut world, &db, &versions);
        let parent = model::subtree(PARENT);
        let tx = world.begin();
        let owners = Owners::default();
        let other = beside.then(|| tx.txn.scope().expect("a scope"));
        let scope = tx.txn.scope().expect("a scope");
        for at in 0..SMALL.len() {
            let batch = stopped(&world, &tx, at, |items| {
                items.remove(&key(2));
                items.insert(key(300), vec![5; 70]);
            });
            (owners.apply(&world, &tx, (1, &scope), &batch)).expect("its Merk");
            scope.mark_pending(parent, &host_of(at)).expect("a mark");
        }
        // The first element goes, with the Merk it hosts.
        let gone = world.merk(&tx, PARENT, |items| drop(items.remove(&host_of(0))));
        let batch = OwnedBatch {
            records: World::root_record(&gone).into_iter().collect(),
            merks: vec![gone],
            subtrees: vec![OwnedSubtree::Drop(model::subtree(SMALL[0]))],
        };
        (owners.apply(&world, &tx, (1, &scope), &batch)).expect("the element is deleted");
        scope.merge();
        drop(other);
        behind_are(&tx, &[1]);
        assert!(tx.txn.is_pending(parent, &host_of(0)));
        assert!(matches!(tx.txn.seal(), Err(TxnError::NotFinalized)));

        let scope = tx.txn.scope().expect("a scope");
        (scope.clear_pending(parent, &host_of(0))).expect("the mark is taken off");
        let batch = finalize(&world, &tx, &[1], b"tree, again");
        (owners.apply(&world, &tx, (2, &scope), &batch)).expect("the host is written");
        (scope.clear_pending(parent, &host_of(1))).expect("the mark is taken off");
        scope.merge();
        behind_are(&tx, &[]);
        world.check(&tx);
        assert!(committed(world.commit(tx, &[])), "{beside}");
        world.check_committed();
        assert_eq!(db.private_ids(), 0, "{beside}");
    }
}

/// A node that names a Merk it hosts has the Merk's root read for it
/// (SC2): a batch with such a node and no change set for the Merk reads
/// the Merk's unit. It is refused while another scope writes the Merk,
/// and a scope that reads it so keeps every other scope from writing it.
#[test]
fn a_node_that_names_a_merk_reads_its_unit() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    hosted(&mut world, &db, &versions);
    let tx = world.begin();
    let owners = Owners::default();
    let namer = tx.txn.scope().expect("a scope");
    let writer = tx.txn.scope().expect("a scope");
    let batch = stopped(&world, &tx, 0, |items| {
        drop(items.insert(key(300), vec![5; 70]))
    });
    (owners.apply(&world, &tx, (2, &writer), &batch)).expect("its Merk");
    let names = finalize(&world, &tx, &[0, 1], b"tree, again");
    let error = names.with(|batch| namer.apply(batch)).expect_err("held");
    assert!(held(&error, data(SMALL[0])), "{error}");
    // None of the claims of the batch that was refused is kept.
    let parent = model::subtree(PARENT);
    let seen = |view: Option<NodeView<'_>>| view.is_some();
    assert!(tx.txn.node(parent, &key(0), seen).expect("nobody holds it"));
    let names = finalize(&world, &tx, &[1], b"tree, again");
    (owners.apply(&world, &tx, (1, &namer), &names)).expect("nobody writes that Merk");
    assert!((tx.txn.node(parent, &key(0), seen)).is_err_and(|error| held(&error, data(PARENT))));
    let batch = stopped(&world, &tx, 1, |items| {
        drop(items.insert(key(300), vec![5; 70]))
    });
    let error = batch.with(|batch| writer.apply(batch)).expect_err("held");
    assert!(held(&error, data(SMALL[1])), "{error}");
    check_through(&world, &tx, &namer, &[PARENT, SMALL[1]]);
    check_through(&world, &tx, &writer, &[SMALL[0]]);
    namer.merge();
    writer.rollback();
    owners.forget(&tx, 2);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// The small Merks of [`many_hosted`], which no store has entered, and the
/// Merk whose elements host them.
const MANY: u8 = 12;
const WIDE: u8 = 6;

fn small(at: u8) -> u8 {
    ENTERED + 30 + at
}

/// The key of the element of [`WIDE`] that hosts the Merk `small(at)`: the
/// hosts stand between the other nodes of the Merk.
fn wide_host(at: u8) -> Vec<u8> {
    format!("n{:04}host", usize::from(at) * 5).into_bytes()
}

/// Commits [`WIDE`] with [`MANY`] elements that each host a small Merk,
/// between other nodes.
fn many_hosted(world: &mut World<'_>) {
    let tx = world.begin();
    let mut parent = world.merk(&tx, WIDE, |items| {
        for node in 0..60 {
            items.insert(key(node), vec![3; 40]);
        }
        for at in 0..MANY {
            items.insert(wide_host(at), b"tree".to_vec());
        }
    });
    let mut batch = OwnedBatch::default();
    for at in 0..MANY {
        let subtree = model::subtree(small(at));
        parent.hosted.insert(wide_host(at), vec![subtree]);
        let nodes = (0..5).map(|node| (key(node * 2), vec![at; 30 + node * 7]));
        batch.merks.push(OwnedMerk::change(
            subtree,
            &BTreeMap::new(),
            &nodes.collect(),
        ));
        let host = (model::subtree(WIDE), wide_host(at), 0);
        batch
            .subtrees
            .push(OwnedSubtree::Enter(subtree, Some(host)));
    }
    batch.records.extend(World::root_record(&parent));
    batch.merks.push(parent);
    world.apply(&tx, &batch).expect("the Merks are entered");
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// T-SC6, the application at the merge (LT4): one scope writes on the
/// transaction's pages and carries cells of pages that the Merks of other
/// scopes are in, the root of a small Merk among them, which travels with
/// its host (`tree-layout.md` T2). Another scope claims that Merk a moment
/// later, or had claimed it a moment before, and its writes are laid out
/// at its merge on the pages the first one left. After both merges the
/// records and hashes are those of the two run one after another, each
/// has read what it reads then, and the committed pages pass the check.
#[test]
fn a_merk_that_another_scope_carried_is_written_where_it_is_now() {
    // Which small Merk the first scope's writes carry, and with which
    // write: the same in every run, since the same writes lay the pages
    // out the same way.
    let mut found: Option<(u8, usize)> = None;
    for claimed_first in [false, true] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        many_hosted(&mut world);
        let parent = model::subtree(WIDE);
        let roots = db.roots(&versions, versions.latest());
        let tx = world.begin();
        let owners = Owners::default();
        let mut batches: Vec<(u32, OwnedBatch)> = Vec::new();
        let first = tx.txn.scope().expect("a scope");
        let second = tx.txn.scope().expect("a scope");
        assert!(first.is_laid_out() && !second.is_laid_out());
        let write_small = |at: u8, batches: &mut Vec<(u32, OwnedBatch)>| {
            for round in 0..3 {
                let mut merk = world.merk(&tx, small(at), |items| {
                    items.remove(&key(round * 2));
                    items.insert(key(400 + round), vec![6; 90]);
                });
                merk.anchored = true;
                let batch = OwnedBatch {
                    merks: vec![merk],
                    ..OwnedBatch::default()
                };
                (owners.apply(&world, &tx, (2, &second), &batch)).expect("its Merk");
                batches.push((2, batch));
            }
            second.mark_pending(parent, &wide_host(at)).expect("a mark");
            check_through(&world, &tx, &second, &[small(at)]);
        };
        if let Some((at, _)) = found.filter(|_| claimed_first) {
            write_small(at, &mut batches);
        }
        // The first scope writes the parent until a write carries the root
        // of a small Merk.
        let mut carried = None;
        for round in 0..80 {
            let merk = world.merk(&tx, WIDE, |items| {
                for node in 0..14 {
                    let between = format!("n{:04}x{round:03}", (round * 7 + node * 5) % 60);
                    items.insert(between.into_bytes(), vec![9; 180]);
                }
            });
            let batch = OwnedBatch {
                records: World::root_record(&merk).into_iter().collect(),
                merks: vec![merk],
                ..OwnedBatch::default()
            };
            let applied = (owners.apply(&world, &tx, (1, &first), &batch)).expect("the parent");
            batches.push((1, batch));
            let moved = |at: &u8| {
                let was = roots[&model::subtree(small(*at))];
                applied.carried.iter().any(|(from, _)| *from == was)
            };
            if let Some(at) = (0..MANY).find(moved) {
                carried = Some((at, round));
                break;
            }
        }
        let (at, round) = carried.expect("no root of a small Merk was carried");
        match found {
            None => found = Some((at, round)),
            Some(found) => assert_eq!(found, (at, round), "the same writes, the same pages"),
        }
        if !claimed_first {
            write_small(at, &mut batches);
        }
        check_through(&world, &tx, &first, &[WIDE]);
        first.merge();
        check_through(&world, &tx, &second, &[small(at)]);
        second.merge();
        world.check(&tx);
        let scope = tx.txn.scope().expect("a scope");
        let mut host = world.merk(&tx, WIDE, |items| {
            items.insert(wide_host(at), b"tree, again".to_vec());
        });
        (host.hosted).insert(wide_host(at), vec![model::subtree(small(at))]);
        let batch = OwnedBatch {
            records: World::root_record(&host).into_iter().collect(),
            merks: vec![host],
            ..OwnedBatch::default()
        };
        (owners.apply(&world, &tx, (3, &scope), &batch)).expect("the host is written");
        batches.push((3, batch));
        scope
            .clear_pending(parent, &wide_host(at))
            .expect("the mark is taken off");
        scope.merge();
        world.check(&tx);
        assert!(committed(world.commit(tx, &[])));
        world.check_committed();

        // One after another: the first scope, then the second, then the
        // write of the host.
        let (serial_db, serial_versions) = Db::new(96);
        let mut serial = World::new(&serial_db, &serial_versions);
        many_hosted(&mut serial);
        let tx = serial.begin();
        for tag in 1..=3 {
            let scope = tx.txn.scope().expect("a scope");
            for (_, batch) in batches.iter().filter(|(of, _)| *of == tag) {
                (owners.apply(&serial, &tx, (tag, &scope), batch)).expect("the same batch");
            }
            check_through(&serial, &tx, &scope, &[WIDE, small(at)]);
            scope.merge();
        }
        serial.check(&tx);
        assert!(committed(serial.commit(tx, &[])));
        serial.check_committed();
        assert_eq!(world.model.records, serial.model.records);
        assert_eq!(db.private_ids(), 0);
    }
}

/// What only the pages can refuse is refused when a scope's writes are
/// laid out (`crate::scope`): at once for a scope that is alone, and for a
/// scope that was opened beside another when it asks, which leaves it as
/// it was, or at its merge, which leaves a transaction that is not
/// committed until those writes are taken out.
#[test]
fn a_write_that_the_pages_refuse_is_refused_when_it_is_laid_out() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    planted(&mut world, 3);
    let no_child = |error: &TxnError, name: u8| matches!(error, TxnError::Batch(BatchError::NoChild { subtree }) if *subtree == model::subtree(name));

    let tx = world.begin();
    let owners = Owners::default();
    let alone = tx.txn.scope().expect("a scope");
    let bad = broken(&world, &tx, 1).through(&alone);
    let error = bad.with(|batch| alone.apply(batch)).expect_err("refused");
    assert!(no_child(&error, 1), "{error}");
    let beside = tx.txn.scope().expect("a scope");
    (owners.edit(&world, &tx, (2, &beside), 3, |items| {
        items.insert(key(1), b"good".to_vec());
    }))
    .expect("a write that the pages take");
    let bad = broken(&world, &tx, 2);
    let applied = (bad.through(&beside))
        .with(|batch| beside.apply(batch))
        .expect("kept in the write set");
    assert!(
        applied.nodes.is_empty(),
        "no handle before the pages have it"
    );
    let writes = tx.txn.stats().writes;
    // Asked, the scope is told, and is as it was: open, usable, with its
    // writes in the write set and none of them on the pages.
    let error = beside.lay_out().expect_err("refused");
    assert!(no_child(&error, 2), "{error}");
    assert!(!beside.is_laid_out());
    assert_eq!(tx.txn.stats().writes, writes);
    let seen = |view: Option<NodeView<'_>>| view.map(|view| value_of(&view));
    let read = beside
        .node(model::subtree(3), &key(1), seen)
        .expect("a read");
    assert_eq!(read, Some(b"good".to_vec()));
    alone.merge();
    beside.rollback();
    owners.forget(&tx, 2);
    assert_eq!(tx.txn.stats().writes, 0);
    world.check(&tx);

    // Merged all the same, its writes are the transaction's, which cannot
    // be laid out: it reads its records by key and is not committed.
    let alone = tx.txn.scope().expect("a scope");
    let beside = tx.txn.scope().expect("a scope");
    (bad.through(&beside))
        .with(|batch| beside.apply(batch))
        .expect("kept in the write set");
    beside.merge();
    alone.merge();
    assert!(tx.txn.stats().writes > 0);
    let read = (tx.txn.node(model::subtree(2), &key(1), seen)).expect("a read by key");
    assert_eq!(read, Some(b"x".to_vec()));
    assert_eq!(tx.txn.stats().base, None);
    let commit = db.commit(&versions, &tx.txn);
    assert!(commit.is_err_and(|error| no_child(&error, 2)));
    tx.txn.rollback().expect("no scope is open");
    world.check(&tx);
    assert!(matches!(world.commit(tx, &[]), Outcome::Empty));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// One more node of the Merk that has the scope's number, through the
/// scope.
fn write_own<'s>(
    owners: &Owners,
    world: &World<'s>,
    tx: &Tx<'s>,
    scope: (u32, &Scope<'_, 's>),
    node: usize,
) {
    let name = scope.0 as u8;
    (owners.edit(world, tx, scope, name, |items| {
        items.insert(key(100 + node), vec![name; 50]);
    }))
    .expect("a Merk of its own");
}

/// A node that is written again and keeps its value has the value of the
/// write before it (WT3). Where that write kept it too and is laid out,
/// the value is in the transaction's pages and nowhere in the write set: a
/// scope that was opened beside another, whose write goes into the write
/// set alone, takes it from there.
#[test]
fn a_scope_beside_another_keeps_a_value_that_only_the_pages_hold() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    planted(&mut world, 2);
    let tx = world.begin();
    let owners = Owners::default();
    // Every node from the new one up to the root is written again, and
    // keeps its value.
    let alone = tx.txn.scope().expect("a scope");
    (owners.edit(&world, &tx, (1, &alone), 1, |items| {
        drop(items.insert(key(51), b"first".to_vec()))
    }))
    .expect("its Merk");
    alone.merge();
    let other = tx.txn.scope().expect("a scope");
    let beside = tx.txn.scope().expect("a scope");
    assert!(!beside.is_laid_out());
    for node in [53, 1] {
        (owners.edit(&world, &tx, (2, &beside), 1, |items| {
            drop(items.insert(key(node), b"second".to_vec()))
        }))
        .expect("its Merk");
        check_through(&world, &tx, &beside, &[1]);
    }
    // A commit outside meets nothing of it, and the frames are made again
    // from the write set, which has the values now.
    world.edit_outside(2, |items| drop(items.insert(key(7), b"x".to_vec())), &[&tx]);
    check_through(&world, &tx, &beside, &[1]);
    beside.merge();
    other.rollback();
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// SC8, SC5, CF4: a commit outside lands while scopes are open. The
/// transaction is rebased with them open, the writes of each have the
/// first-write versions of their own operations, and a write of an open
/// scope that the commit meets leaves the conflict rule with the scope if
/// the scope is rolled back.
#[test]
fn a_commit_outside_lands_while_scopes_are_open() {
    for rolls_back in [false, true] {
        let (db, versions) = Db::new(64);
        let mut world = World::new(&db, &versions);
        planted(&mut world, 5);
        let tx = world.begin();
        let owners = Owners::default();
        let one = tx.txn.scope().expect("a scope");
        let two = tx.txn.scope().expect("a scope");
        write_own(&owners, &world, &tx, (1, &one), 0);
        write_own(&owners, &world, &tx, (2, &two), 0);
        // A commit that meets neither: both go on, on frames that are
        // made again over it, and read it.
        let rebuilds = tx.txn.stats().rebuilds;
        world.edit_outside(
            5,
            |items| drop(items.insert(key(1), b"outside".to_vec())),
            &[&tx],
        );
        write_own(&owners, &world, &tx, (1, &one), 1);
        write_own(&owners, &world, &tx, (2, &two), 1);
        assert!(tx.txn.stats().rebuilds > rebuilds);
        assert_eq!(tx.txn.stats().overlapped, 0);
        let seen = |view: Option<NodeView<'_>>| view.map(|view| value_of(&view));
        let read = two.node(model::subtree(5), &key(1), seen).expect("a read");
        assert_eq!(read, Some(b"outside".to_vec()));
        check_through(&world, &tx, &one, &[1]);
        check_through(&world, &tx, &two, &[2]);

        // A commit that writes a key the second scope wrote, whose writes
        // are not laid out: the transaction serves its records by key.
        let theirs = |items: &mut Items| drop(items.insert(key(100), b"theirs".to_vec()));
        assert!(committed(world.edit_outside(2, theirs, &[&tx])));
        write_own(&owners, &world, &tx, (1, &one), 2);
        assert!(tx.txn.stats().overlapped > 0);
        assert_eq!(tx.txn.stats().base, None);
        check_through(&world, &tx, &one, &[1]);
        check_through(&world, &tx, &two, &[2]);
        one.merge();
        match rolls_back {
            true => {
                two.rollback();
                owners.forget(&tx, 2);
            }
            false => two.merge(),
        }
        world.check(&tx);
        // With the scope its met writes went, and the transaction commits.
        assert_eq!(tx.txn.stats().overlapped > 0, !rolls_back);
        let outcome = world.commit(tx, &[]);
        assert_eq!(committed(outcome), rolls_back, "{outcome:?}");
        world.check_committed();
        assert_eq!(db.private_ids(), 0);
    }
}

/// One thread's part of the stress test: the batches it applies through a
/// scope of its own, and the records it reads on the way.
struct Part {
    name: u8,
    batches: Vec<OwnedBatch>,
    merges: bool,
}

impl Part {
    /// The changes a scope makes to the Merk `name`, each made of the Merk
    /// as the one before left it, with a flat record beside each.
    fn new(rng: &mut Rng, name: u8, before: &Items, merges: bool) -> Self {
        let subtree = model::subtree(name);
        let view = |items: &Items| {
            let (_, records) = model::records(items);
            records
                .into_iter()
                .map(|(key, node)| (key, Val::Node(node)))
                .collect::<BTreeMap<_, _>>()
        };
        let mut items = before.clone();
        let mut batches = Vec::new();
        for round in 0..3 + rng.below(4) {
            let was = view(&items);
            for _ in 0..1 + rng.below(6) {
                let node = key(rng.below(40));
                match rng.chance(30) {
                    true => drop(items.remove(&node)),
                    false => {
                        let len = match rng.chance(20) {
                            true => 300 + rng.below(500),
                            false => 5 + rng.below(90),
                        };
                        drop(items.insert(node, model::value(rng, len)));
                    }
                }
            }
            let merk = OwnedMerk::change(subtree, &was, &items);
            let mut records: Vec<Write> = World::root_record(&merk).into_iter().collect();
            records.push(put(aux(name), &key(round), &[name; 12]));
            batches.push(OwnedBatch {
                merks: vec![merk],
                records,
                ..OwnedBatch::default()
            });
        }
        Self {
            name,
            batches,
            merges,
        }
    }

    /// Runs the part through `scope`: every batch, with reads of its own
    /// Merk and of the Merk that every part reads between them. Returns
    /// what it read.
    fn run(&self, scope: Scope<'_, '_>, pause: &dyn Fn()) -> Vec<Option<Vec<u8>>> {
        let mut seen = Vec::new();
        let value = |view: Option<NodeView<'_>>| view.map(|view| value_of(&view));
        for (round, batch) in self.batches.iter().enumerate() {
            pause();
            seen.push((scope.node(model::subtree(SHARED), &key(round), value)).expect("a read"));
            pause();
            (batch.through(&scope))
                .with(|batch| scope.apply(batch))
                .expect("its own Merk");
            pause();
            let own = model::subtree(self.name);
            seen.push(scope.node(own, &key(round), value).expect("a read"));
            let mut out = Vec::new();
            let found = (scope.get(aux(self.name), &key(round), &mut out)).expect("a read");
            seen.push(found.then_some(out));
        }
        pause();
        match self.merges {
            true => scope.merge(),
            false => scope.rollback(),
        }
        seen
    }
}

/// The Merk every part of the stress test reads, and none writes.
const SHARED: u8 = 40;

/// T-SC1 on a store: scopes on threads of their own, each writing a Merk
/// and a flat unit of its own and all reading one Merk, leave the records,
/// the hashes and the pages' contents that the same scopes leave one after
/// another on one thread, and each reads what it reads then (SC7). Some
/// roll back. The committed pages pass the physical check both times.
#[test]
fn scopes_on_threads_leave_what_they_leave_one_after_another() {
    const THREADS: u8 = 5;
    for seed in 0..if cfg!(miri) { 1 } else { 12 } {
        let mut rng = Rng::new(seed);
        let plant = |world: &mut World<'_>| -> Vec<Items> {
            let tx = world.begin();
            let mut before = Vec::new();
            for name in (1..=THREADS).chain([SHARED]) {
                world.edit(&tx, name, |items| {
                    for node in 0..8 {
                        items.insert(key(node * 3), vec![name; 20 + node * 30]);
                    }
                    before.push(items.clone());
                });
            }
            assert!(committed(world.commit(tx, &[])));
            before
        };
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        let before = plant(&mut world);
        let parts: Vec<Part> = (1..=THREADS)
            .map(|name| {
                let merges = rng.chance(75);
                Part::new(&mut rng, name, &before[usize::from(name) - 1], merges)
            })
            .collect();

        // On threads, all scopes open before any of them writes.
        let tx = world.begin();
        let start = Barrier::new(parts.len());
        let turn = Mutex::new(Rng::new(seed ^ 0xfeed));
        let pause = || {
            let spins = turn.lock().expect("no thread panics").below(4);
            for _ in 0..spins {
                thread::yield_now();
            }
        };
        let seen: Vec<Vec<Option<Vec<u8>>>> = thread::scope(|threads| {
            let runs: Vec<_> = (parts.iter())
                .map(|part| {
                    let (txn, start, pause) = (&tx.txn, &start, &pause);
                    threads.spawn(move || {
                        let scope = txn.scope().expect("a scope");
                        start.wait();
                        part.run(scope, pause)
                    })
                })
                .collect();
            (runs.into_iter())
                .map(|run| run.join().expect("a part"))
                .collect()
        });
        assert!(!tx.txn.scopes_open());

        // One after another, on a store of its own.
        let (serial_db, serial_versions) = Db::new(96);
        let mut serial = World::new(&serial_db, &serial_versions);
        assert_eq!(plant(&mut serial), before);
        let serial_tx = serial.begin();
        for (part, seen) in parts.iter().zip(&seen) {
            let scope = serial_tx.txn.scope().expect("a scope");
            assert!(scope.is_laid_out());
            assert_eq!(&part.run(scope, &|| ()), seen, "what the scope read");
        }
        // The models are told what merged, and both transactions read it.
        for part in parts.iter().filter(|part| part.merges) {
            for batch in &part.batches {
                world.note(&tx, batch, world.latest().get());
                serial.note(&serial_tx, batch, serial.latest().get());
            }
        }
        assert_eq!(tx.txn.stats().writes, serial_tx.txn.stats().writes);
        world.check(&tx);
        serial.check(&serial_tx);
        assert!(committed(world.commit(tx, &[])), "{seed}");
        assert!(committed(serial.commit(serial_tx, &[])), "{seed}");
        // The same records and hashes, and pages that pass the check.
        world.check_committed();
        serial.check_committed();
        assert_eq!(world.model.records, serial.model.records, "{seed}");
        assert_eq!(db.private_ids(), 0, "{seed}");
    }
}
