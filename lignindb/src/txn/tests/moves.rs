//! The move of a subtree to another prefix ([`SubtreeWrite::Move`],
//! `tree-layout.md` D5 and D6, `transactions.md` CF5): what the
//! transaction that makes it reads afterwards, what the store keeps, and
//! what it does to the transactions beside it.

use lignin_tree::{directory::MerkId, node::NodeHandle};

use super::{
    model::{self, handle_of, OwnedBatch, OwnedMerk, OwnedSubtree},
    store::{Db, Outcome, ENTERED},
    subtrees::{aux, enter, hosted, shown, subtrees, Shown},
    *,
};

/// A Merk with no host, whose nodes host the subtrees of these tests.
const TOP: u8 = ENTERED + 40;
/// The subtree that the node `a` of `TOP` hosts, and the one that its
/// node `in` hosts.
const A: u8 = ENTERED + 41;
const A_IN: u8 = ENTERED + 42;
/// The prefixes of the two under the key `b` of `TOP`, and under `c`.
const B: u8 = ENTERED + 43;
const B_IN: u8 = ENTERED + 44;
const C: u8 = ENTERED + 45;
const C_IN: u8 = ENTERED + 46;
/// A subtree with no node under the node `e` of `A`, the prefix it would
/// have under `b`, and two prefixes that no store has a subtree under.
const EMPTY: u8 = ENTERED + 47;
const EMPTY_B: u8 = ENTERED + 48;
const NONE: u8 = ENTERED + 49;
const NONE_B: u8 = ENTERED + 50;
const EMPTY_2: u8 = ENTERED + 51;
/// A Merk with no host, whose node `t` hosts `TOP` where a test says so.
const ROOT: u8 = ENTERED + 52;
/// Two more prefixes that no store has a subtree under.
const FREE: u8 = ENTERED + 38;
const FREE_IN: u8 = ENTERED + 39;

fn sub(name: u8) -> Subtree {
    model::subtree(name)
}

fn committed(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Committed(_))
}

/// The batch that enters a subtree `name` under the node `key` of `TOP`,
/// with nodes and flat records in every column beside them, and a subtree
/// `inner` under its node `in`, with nodes and flat records of its own.
/// `salt` tells the values of one such pair from those of another.
fn nested<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    key: &[u8],
    name: u8,
    inner: u8,
    salt: u8,
) -> OwnedBatch {
    let mut top = world.merk(tx, TOP, |items| {
        items.insert(key.to_vec(), vec![b't', salt]);
    });
    top.hosted.insert(key.to_vec(), vec![sub(name)]);
    let mut outer = world.merk(tx, name, |items| {
        items.insert(b"in".to_vec(), b"tree in".to_vec());
        items.insert(b"k1".to_vec(), vec![salt; 40]);
        items.insert(b"k2".to_vec(), vec![salt + 1; 300]);
    });
    outer.hosted.insert(b"in".to_vec(), vec![sub(inner)]);
    let below = world.merk(tx, inner, |items| {
        items.insert(b"one".to_vec(), vec![salt + 2; 20]);
        items.insert(b"two".to_vec(), vec![salt + 3; 20]);
    });
    OwnedBatch {
        merks: vec![below, outer, top],
        records: vec![
            put(aux(name), b"a", &[salt; 3]),
            put(model::unit(Column::Meta, name), b"m", &[salt; 5]),
            put(model::unit(Column::Roots, name), b"r", &[salt; 7]),
            put(aux(inner), b"ia", &[salt; 9]),
            put(model::unit(Column::Meta, inner), b"im", &[salt; 400]),
        ],
        subtrees: vec![hosted(name, TOP, key), hosted(inner, name, b"in")],
    }
}

/// Commits a grove: `TOP` with two items, and under its node `key` the
/// subtree `name` with `inner` below it ([`nested`]).
fn grove<'s>(world: &mut World<'s>, key: &[u8], name: u8, inner: u8) {
    let tx = world.begin();
    let top = world.merk(&tx, TOP, |items| {
        items.insert(b"x".to_vec(), vec![9; 30]);
        items.insert(b"y".to_vec(), vec![8; 300]);
    });
    let made = OwnedBatch {
        merks: vec![top],
        records: vec![put(aux(TOP), b"t", b"top")],
        subtrees: vec![enter(TOP)],
    };
    world.apply(&tx, &made).expect("the top is made");
    let batch = nested(world, &tx, key, name, inner, 1);
    world.apply(&tx, &batch).expect("the subtrees are made");
    assert!(committed(world.commit(tx, &[])));
}

/// The batch that renames the node `from` of `TOP` to `to` and moves the
/// subtree it hosts with it: `pairs` has the prefix of that subtree
/// before and afterwards, and then those of the subtrees below it that
/// the move names. The move names the read of the old host that is made
/// here.
fn rename<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    from: &[u8],
    to: &[u8],
    pairs: &[(u8, u8)],
) -> OwnedBatch {
    let mut top = world.merk(tx, TOP, |items| {
        let value = items.remove(from).expect("the node that hosts the subtree");
        items.insert(to.to_vec(), value);
    });
    top.hosted.insert(to.to_vec(), vec![sub(pairs[0].1)]);
    let read = (tx.txn.node(sub(TOP), from, handle_of)).expect("the host is read");
    OwnedBatch {
        merks: vec![top],
        subtrees: vec![moving(to, read, pairs)],
        ..OwnedBatch::default()
    }
}

/// The move of the subtree `pairs[0].0` to `pairs[0].1` under the node
/// `to` of `TOP`, which names `read` and the other pairs.
fn moving(to: &[u8], read: Option<NodeHandle>, pairs: &[(u8, u8)]) -> OwnedSubtree {
    let below = pairs[1..].iter().map(|(old, new)| (sub(*old), sub(*new)));
    OwnedSubtree::Move {
        from: sub(pairs[0].0),
        to: sub(pairs[0].1),
        host: (sub(TOP), to.to_vec(), 0),
        read,
        below: below.collect(),
    }
}

/// A change of the Merk at the end of `path`, with every host above it
/// written in the same batch, each naming the Merk it hosts again: `path`
/// has, from `TOP` down, each Merk with the key of the node that hosts it.
fn edit_at<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    path: &[(u8, &[u8])],
    edit: impl FnOnce(&mut BTreeMap<Vec<u8>, Vec<u8>>),
) -> OwnedBatch {
    let (last, _) = *path.last().expect("a Merk");
    let mut merks = vec![world.merk(tx, last, edit)];
    let stamp = merks[0].nodes.len() as u8;
    for (depth, (name, key)) in path.iter().enumerate().rev() {
        let above = depth.checked_sub(1).map_or(TOP, |above| path[above].0);
        let mut host = world.merk(tx, above, |items| {
            let mut value = items.get(*key).cloned().unwrap_or_default();
            value.push(stamp);
            items.insert(key.to_vec(), value);
        });
        host.hosted.insert(key.to_vec(), vec![sub(*name)]);
        merks.push(host);
    }
    OwnedBatch {
        merks,
        ..OwnedBatch::default()
    }
}

fn batch_error(result: Result<Applied, TxnError>) -> BatchError {
    match result {
        Err(TxnError::Batch(error)) => error,
        other => panic!("a refused batch is expected, and not {other:?}"),
    }
}

/// Commits, under the node `e` of `A`, the subtree `EMPTY` with no node
/// and a flat record: what a tree that was emptied leaves.
fn emptied<'s>(world: &mut World<'s>) {
    let tx = world.begin();
    let mut batch = edit_at(world, &tx, &[(A, b"a")], |items| {
        items.insert(b"e".to_vec(), b"an empty tree".to_vec());
    });
    (batch.merks[0].hosted).insert(b"e".to_vec(), vec![sub(EMPTY)]);
    batch.subtrees = vec![hosted(EMPTY, A, b"e")];
    batch.records = vec![put(aux(EMPTY), b"x", b"flat")];
    world.apply(&tx, &batch).expect("the empty subtree is made");
    assert!(committed(world.commit(tx, &[])));
}

fn get(tx: &Tx<'_>, unit: Unit, key: &[u8]) -> Result<Option<Vec<u8>>, TxnError> {
    let mut out = Vec::new();
    Ok(tx.txn.get(unit, key, &mut out)?.then_some(out))
}

fn host(name: u8, key: &[u8]) -> Option<(Subtree, Vec<u8>, u8)> {
    Some((sub(name), key.to_vec(), 0))
}

/// What `shown` was, under another host.
fn under(shown: &Shown, name: u8, key: &[u8]) -> Shown {
    let (root, tree_type, _) = shown.clone().expect("a subtree that is entered");
    Some((root, tree_type, host(name, key)))
}

/// What the directory of the latest version holds: of every Merk its
/// entry without the cell its root is in, the Merks it is below, the
/// Merks it hosts by the records that lead to them, and the prefix that
/// the record of its root's cell names; the id the next Merk gets; and
/// every record of every flat column.
type Kept = (
    Vec<(
        Subtree,
        MerkId,
        Shown,
        Vec<MerkId>,
        Vec<(u8, Vec<u8>, Subtree, MerkId)>,
        Option<Option<Subtree>>,
    )>,
    MerkId,
    Vec<Vec<(Vec<u8>, Vec<u8>)>>,
);

fn kept(world: &World<'_>) -> Kept {
    let guard = world.path.versions.guard();
    let at = world.latest();
    let pages = world.path.committed(&guard, at);
    let directory = world.path.directory_at(at).expect("a directory");
    assert!(
        directory.drops().next(&pages).expect("a walk").is_none(),
        "nothing is left to reclaim"
    );
    let mut entries = Vec::new();
    let mut walk = directory.entries();
    while let Some((prefix, entry)) = walk.next(&pages).expect("an entry") {
        assert!(directory.is_live(&entry), "every entry is in use");
        let host = entry
            .host()
            .map(|host| (Subtree::new(*host.parent), host.key.to_vec(), host.position));
        let root = entry.root().map(|root| root.key.to_vec());
        let shown = Some((root, entry.tree_type().to_vec(), host));
        let mut hosted = Vec::new();
        let mut children = directory.children(entry.id());
        while let Some(child) = children.next(&pages).expect("a child") {
            hosted.push((
                child.position,
                child.key,
                Subtree::new(child.prefix),
                child.id,
            ));
        }
        let lineage = entry.lineage().collect();
        let owner = entry.root().map(|root| {
            let owner = directory.root_owner(&pages, root.address);
            owner.expect("a lookup").map(Subtree::new)
        });
        let prefix = Subtree::new(prefix);
        entries.push((prefix, entry.id(), shown, lineage, hosted, owner));
    }
    let next = directory.next_id(&pages).expect("the next id");
    let columns = world.path.keyspaces.columns().map(|column| {
        let mut cursor = column.tree.cursor();
        let mut records = Vec::new();
        cursor.seek_to_first(&pages).expect("a seek");
        while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
            let mut value = Vec::new();
            assert!(cursor.value(&pages, &mut value).expect("a value"));
            records.push((key, value));
            cursor.next(&pages).expect("a step");
        }
        records
    });
    (entries, next, columns.to_vec())
}

/// Commits, under the node `e` of the subtree `parent`, which the node
/// `key` of `TOP` hosts, the subtree `name` with no node and with flat
/// records, one of them in its data column.
fn flat_below<'s>(world: &mut World<'s>, key: &[u8], parent: u8, name: u8) {
    let tx = world.begin();
    let mut batch = edit_at(world, &tx, &[(parent, key)], |items| {
        items.insert(b"e".to_vec(), b"a tree with flat data".to_vec());
    });
    (batch.merks[0].hosted).insert(b"e".to_vec(), vec![sub(name)]);
    batch.subtrees = vec![hosted(name, parent, b"e")];
    batch.records = vec![
        put(aux(name), b"x", b"flat"),
        put(model::unit(Column::Data, name), b"d", b"flat data"),
    ];
    world.apply(&tx, &batch).expect("the subtree is made");
    assert!(committed(world.commit(tx, &[])));
}

/// Section 4 of the interface: after a move the transaction reads under
/// the new prefixes what it read under the old ones, its own writes from
/// before the move among it, and nothing under the old ones; the
/// committed reads are those of the committed state (RD10), and another
/// transaction reads the old prefixes until the mover commits.
#[test]
fn a_move_takes_a_subtree_and_those_below_it_to_their_new_prefixes() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    // Writes of the transaction under the old prefixes, before the move.
    let before = edit_at(&world, &tx, &[(A, b"a"), (A_IN, b"in")], |items| {
        items.insert(b"three".to_vec(), vec![6; 50]);
        items.remove(&b"one"[..]);
    });
    world
        .apply(&tx, &before)
        .expect("a change under the source");
    world.write(&tx, vec![put(aux(A), b"own", b"before the move")]);
    let was = [A, A_IN].map(|name| shown(&tx, name));

    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    world.check(&tx);
    assert_eq!([shown(&tx, A), shown(&tx, A_IN)], [None, None]);
    assert_eq!(shown(&tx, B), under(&was[0], TOP, b"b"));
    assert_eq!(shown(&tx, B_IN), under(&was[1], B, b"in"));
    let mut value = Vec::new();
    assert!(tx.txn.get(aux(B), b"own", &mut value).expect("a read"));
    assert_eq!(value, b"before the move");

    // Another transaction reads the committed state, which has the old
    // prefixes still.
    let reader = world.begin();
    assert_eq!(shown(&reader, A), under(&was[0], TOP, b"a"));
    assert_eq!(shown(&reader, B), None);
    world.check(&reader);

    // The transaction's writes go under the new prefixes from here on.
    let after = edit_at(&world, &tx, &[(B, b"b"), (B_IN, b"in")], |items| {
        items.insert(b"four".to_vec(), vec![7; 280]);
        items.remove(&b"two"[..]);
    });
    world.apply(&tx, &after).expect("a change under the target");
    let records = vec![
        put(aux(B), b"a", b"again"),
        delete(model::unit(Column::Meta, B_IN), b"im"),
    ];
    world.write(&tx, records);
    world.check(&tx);

    assert!(committed(world.commit(tx, &[&reader])));
    world.check_committed();
    world.check(&reader);
    assert_eq!(shown(&reader, A), None);
    assert_eq!(db.private_ids(), 0);
}

/// D-212 item 3: after a move the directory and every flat keyspace hold,
/// record for record, what a store holds in which the subtree was written
/// under the new prefix from the start: the entries with their ids, the
/// records that lead from a host to its Merks and from a root's cell to
/// its Merk, the id the next Merk gets, and the flat records of every
/// column, those of a subtree that keeps its data flat among them. Each
/// store is read by the physical checker when it is dropped.
#[test]
fn a_moved_subtree_is_kept_as_one_that_was_written_under_the_new_prefix() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    flat_below(&mut world, b"a", A, EMPTY);
    let tx = world.begin();
    let pairs = [(A, B), (A_IN, B_IN), (EMPTY, EMPTY_B)];
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    world.apply(&tx, &batch).expect("the subtree is moved");
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();

    let (other_db, other_versions) = Db::new(96);
    let mut other = World::new(&other_db, &other_versions);
    grove(&mut other, b"b", B, B_IN);
    flat_below(&mut other, b"b", B, EMPTY_B);
    other.check_committed();
    let (moved, written) = (kept(&world), kept(&other));
    assert_eq!(moved.0, written.0, "the directory");
    // The four Merks of the test, beside those every store has.
    let own =
        |entry: &&(Subtree, MerkId, Shown, Vec<MerkId>, _, _)| entry.0.as_bytes()[0] >= ENTERED;
    assert_eq!(moved.0.iter().filter(own).count(), 4);
    let rooted = moved.0.iter().filter(own).filter(|entry| entry.5.is_some());
    assert_eq!(rooted.count(), 3, "three of them have a root");
    for (prefix, .., owner) in &moved.0 {
        assert!(owner.is_none_or(|owner| owner == Some(*prefix)));
    }
    assert_eq!(moved.1, written.1, "the id the next Merk gets");
    for (column, (moved, written)) in moved.2.iter().zip(&written.2).enumerate() {
        assert!(!moved.is_empty(), "column {column} is used");
        assert_eq!(moved, written, "the flat records of column {column}");
    }
    assert_eq!(world.model.records, other.model.records);
}

/// The batch in which the nodes `a` and `b` of `TOP` change places: each
/// is put with the value of the other and hosts what the other hosted.
fn swap<'s>(world: &World<'s>, tx: &Tx<'s>) -> OwnedBatch {
    let mut top = world.merk(tx, TOP, |items| {
        let (a, b) = (items[&b"a"[..]].clone(), items[&b"b"[..]].clone());
        items.insert(b"a".to_vec(), b);
        items.insert(b"b".to_vec(), a);
    });
    top.hosted.insert(b"a".to_vec(), vec![sub(A)]);
    top.hosted.insert(b"b".to_vec(), vec![sub(B)]);
    let read = |key: &[u8]| (tx.txn.node(sub(TOP), key, handle_of)).expect("a read");
    OwnedBatch {
        merks: vec![top],
        subtrees: vec![
            moving(b"b", read(b"a"), &[(A, B), (A_IN, B_IN)]),
            moving(b"a", read(b"b"), &[(B, A), (B_IN, A_IN)]),
        ],
        ..OwnedBatch::default()
    }
}

/// Case 4: two subtrees whose nested subtrees have equal keys change
/// places in one batch, and three move in a chain.
#[test]
fn the_moves_of_one_batch_are_made_together() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let second = nested(&world, &tx, b"b", B, B_IN, 50);
    world.apply(&tx, &second).expect("a second subtree");
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    let names = [A, A_IN, B, B_IN];
    let was = names.map(|name| shown(&tx, name));
    let records = world.model.records.clone();
    world
        .apply(&tx, &swap(&world, &tx))
        .expect("the two change places");
    world.check(&tx);
    assert_eq!(shown(&tx, B), under(&was[0], TOP, b"b"));
    assert_eq!(shown(&tx, B_IN), under(&was[1], B, b"in"));
    assert_eq!(shown(&tx, A), under(&was[2], TOP, b"a"));
    assert_eq!(shown(&tx, A_IN), under(&was[3], A, b"in"));
    // And back, in the same transaction: every record is where it was.
    world.apply(&tx, &swap(&world, &tx)).expect("and back");
    world.check(&tx);
    assert_eq!(names.map(|name| shown(&tx, name)), was);
    world.apply(&tx, &swap(&world, &tx)).expect("and once more");
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_ne!(world.model.records, records);

    // A chain: what is under `b` goes to `c`, and what is under `a` to `b`.
    let tx = world.begin();
    let was = names.map(|name| shown(&tx, name));
    let mut top = world.merk(&tx, TOP, |items| {
        let a = items.remove(&b"a"[..]).expect("a node");
        let b = items.insert(b"b".to_vec(), a).expect("a node");
        items.insert(b"c".to_vec(), b);
    });
    top.hosted.insert(b"b".to_vec(), vec![sub(B)]);
    top.hosted.insert(b"c".to_vec(), vec![sub(C)]);
    let read = |key: &[u8]| (tx.txn.node(sub(TOP), key, handle_of)).expect("a read");
    let chain = OwnedBatch {
        merks: vec![top],
        subtrees: vec![
            moving(b"b", read(b"a"), &[(A, B), (A_IN, B_IN)]),
            moving(b"c", read(b"b"), &[(B, C), (B_IN, C_IN)]),
        ],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &chain).expect("the chain is moved");
    world.check(&tx);
    assert_eq!([shown(&tx, A), shown(&tx, A_IN)], [None, None]);
    assert_eq!(shown(&tx, B), under(&was[0], TOP, b"b"));
    assert_eq!(shown(&tx, B_IN), under(&was[1], B, b"in"));
    assert_eq!(shown(&tx, C), under(&was[2], TOP, b"c"));
    assert_eq!(shown(&tx, C_IN), under(&was[3], C, b"in"));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// Section 2 of the interface: a subtree below the moved one goes to the
/// prefix it is named, leaves with the move if it has no node and is not
/// named, and refuses the batch if it has a node and is not named. A
/// pair for a subtree that is not entered gives no subtree another prefix
/// when the batch is applied.
#[test]
fn a_subtree_below_with_no_node_leaves_with_the_move_and_one_with_a_node_is_named() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    emptied(&mut world);
    let tx = world.begin();
    assert!(shown(&tx, EMPTY).is_some());
    assert_eq!(
        get(&tx, aux(EMPTY), b"x").expect("a read"),
        Some(b"flat".to_vec())
    );

    let unnamed = rename(&world, &tx, b"a", b"b", &[(A, B)]);
    let subtree = sub(A_IN);
    assert_eq!(
        batch_error(world.apply(&tx, &unnamed)),
        BatchError::MoveUnnamed { subtree }
    );
    world.check(&tx);
    assert_eq!(tx.txn.stats().writes, 0, "nothing was written");

    let pairs = [(A, B), (A_IN, B_IN), (NONE, NONE_B)];
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    world.apply(&tx, &batch).expect("the subtree is moved");
    world.check(&tx);
    // The one with no node is gone, with its flat record, and nothing is
    // under the prefix it would have under the new key.
    for gone in [EMPTY, EMPTY_B, NONE, NONE_B, A, A_IN] {
        assert_eq!(shown(&tx, gone), None, "{gone}");
    }
    assert_eq!(get(&tx, aux(EMPTY), b"x").expect("a read"), None);
    assert_eq!(get(&tx, aux(EMPTY_B), b"x").expect("a read"), None);
    assert!(shown(&tx, B_IN).is_some());

    // Under the new key it is a tree nothing was written into: its first
    // write enters it if it is absent.
    let first = OwnedBatch {
        subtrees: vec![OwnedSubtree::EnterIfAbsent(sub(EMPTY_B), host(B, b"e"))],
        records: vec![put(aux(EMPTY_B), b"x", b"new")],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &first).expect("a first write");
    world.check(&tx);
    assert_eq!(shown(&tx, EMPTY_B).expect("entered").2, host(B, b"e"));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// Case 6: a rollback to a savepoint from before a move takes the move
/// out with what came after it, one to a savepoint from after it keeps
/// it, the move is made again, and a move goes with its transaction.
#[test]
fn a_move_goes_with_a_rollback_to_a_savepoint_from_before_it() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    emptied(&mut world);
    let tx = world.begin();
    world.write(&tx, vec![put(aux(A), b"own", b"before")]);
    let was = [A, A_IN, EMPTY].map(|name| shown(&tx, name));
    let pairs = [(A, B), (A_IN, B_IN)];

    tx.txn.set_savepoint().expect("no scope is open");
    let before = tx.model.borrow().writes.len();
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    world.apply(&tx, &batch).expect("the subtree is moved");
    tx.txn.set_savepoint().expect("no scope is open");
    let after = tx.model.borrow().writes.len();
    world.write(&tx, vec![put(aux(B), b"late", b"after")]);
    world.check(&tx);

    // To the savepoint after the move: the move stays.
    tx.txn.rollback_to_savepoint().expect("a savepoint is set");
    tx.model.borrow_mut().writes.truncate(after);
    world.check(&tx);
    assert_eq!(shown(&tx, B), under(&was[0], TOP, b"b"));
    assert_eq!(
        get(&tx, aux(B), b"own").expect("a read"),
        Some(b"before".to_vec())
    );
    assert_eq!(get(&tx, aux(B), b"late").expect("a read"), None);

    // To the one before it: every subtree is under its old prefix, the
    // one that left is back, and the transaction's own write is read
    // where it was made.
    tx.txn.rollback_to_savepoint().expect("a savepoint is set");
    tx.model.borrow_mut().writes.truncate(before);
    world.check(&tx);
    assert_eq!([A, A_IN, EMPTY].map(|name| shown(&tx, name)), was);
    assert_eq!([shown(&tx, B), shown(&tx, B_IN)], [None, None]);
    assert_eq!(
        get(&tx, aux(A), b"own").expect("a read"),
        Some(b"before".to_vec())
    );
    assert_eq!(
        get(&tx, aux(EMPTY), b"x").expect("a read"),
        Some(b"flat".to_vec())
    );

    // Made again, and committed.
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    world
        .apply(&tx, &batch)
        .expect("the subtree is moved again");
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();

    // A move that is rolled back with its transaction leaves nothing.
    let tx = world.begin();
    let was = [B, B_IN].map(|name| shown(&tx, name));
    let batch = rename(&world, &tx, b"b", b"c", &[(B, C), (B_IN, C_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    assert!(shown(&tx, C).is_some());
    tx.txn.rollback().expect("a rollback");
    tx.forget();
    world.check(&tx);
    assert_eq!([B, B_IN].map(|name| shown(&tx, name)), was);
    assert_eq!(shown(&tx, C), None);
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// Case 5: one transaction writes under the source, moves it, reads and
/// writes under the new prefix, inserts a tree at the old key and writes
/// it, moves the subtree to a third key and back to the second, and
/// commits.
#[test]
fn one_transaction_moves_a_subtree_several_times_and_writes_at_every_prefix() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let before = edit_at(&world, &tx, &[(A, b"a")], |items| {
        items.insert(b"k3".to_vec(), vec![5; 120]);
    });
    world
        .apply(&tx, &before)
        .expect("a change under the source");
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the first move");
    world.check(&tx);

    // A tree is inserted at the old key and written: it is new, and holds
    // nothing of the one that was moved.
    let again = nested(&world, &tx, b"a", A, A_IN, 90);
    world.apply(&tx, &again).expect("a tree at the old key");
    world.check(&tx);
    assert_eq!(get(&tx, aux(A), b"a").expect("a read"), Some(vec![90; 3]));
    assert_eq!(get(&tx, aux(B), b"a").expect("a read"), Some(vec![1; 3]));

    // The moved one goes on to a third key, is written there, and comes
    // back to the second.
    let batch = rename(&world, &tx, b"b", b"c", &[(B, C), (B_IN, C_IN)]);
    world.apply(&tx, &batch).expect("the second move");
    world.check(&tx);
    let there = edit_at(&world, &tx, &[(C, b"c"), (C_IN, b"in")], |items| {
        items.insert(b"three".to_vec(), vec![6; 50]);
        items.remove(&b"one"[..]);
    });
    world
        .apply(&tx, &there)
        .expect("a change under the third key");
    world.write(&tx, vec![put(aux(C_IN), b"ia", b"written at c")]);
    let batch = rename(&world, &tx, b"c", b"b", &[(C, B), (C_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the move back");
    world.check(&tx);
    assert_eq!([shown(&tx, C), shown(&tx, C_IN)], [None, None]);
    assert_eq!(
        get(&tx, aux(B_IN), b"ia").expect("a read"),
        Some(b"written at c".to_vec())
    );

    // The moved tree is emptied node by node, the tree below it first.
    let mut emptied = edit_at(&world, &tx, &[(B, b"b")], |items| {
        items.remove(&b"in"[..]);
    });
    emptied.subtrees = vec![OwnedSubtree::Drop(sub(B_IN))];
    world
        .apply(&tx, &emptied)
        .expect("the nested tree goes with its element");
    for key in [&b"k1"[..], b"k2", b"k3"] {
        let batch = edit_at(&world, &tx, &[(B, b"b")], |items| {
            items.remove(key);
        });
        world.apply(&tx, &batch).expect("a node is removed");
    }
    world.check(&tx);
    assert_eq!(shown(&tx, B).expect("entered").0, None, "no node is left");

    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// The same batch, in which each node that keeps its value names the read
/// of it that is made here, through `tx`.
fn read_now(tx: &Tx<'_>, batch: &OwnedBatch) -> OwnedBatch {
    batch.reading(|subtree, key| (tx.txn.node(subtree, key, handle_of)).expect("a read"))
}

/// Whether `error` is the refusal of a transaction that serves its
/// records by key where a move decides what is under `name`.
fn moved_by_key(error: &TxnError, name: u8) -> bool {
    let named = matches!(
        error,
        TxnError::Batch(BatchError::MovedByKey { subtree }) if *subtree == sub(name)
    );
    named && error.is_conflict()
}

/// Whether `error` is the refusal of a cursor that reads another version
/// than the one its transaction's writes are laid out over, where a move
/// decides what is under `name`: it is no conflict.
fn another_version(error: &TxnError, name: u8) -> bool {
    let named = matches!(
        error,
        TxnError::Batch(BatchError::MoveBeside { subtree }) if *subtree == sub(name)
    );
    named && !error.is_conflict()
}

/// The refusal of a batch that `error` is, which is a conflict.
fn conflict(error: TxnError) -> BatchError {
    assert!(error.is_conflict(), "{error}");
    match error {
        TxnError::Batch(error) => error,
        other => panic!("a refused batch is expected, and not {other:?}"),
    }
}

/// Cases 10 and 12, and section 4 by key: a commit outside that writes
/// under a prefix a move names, after the move, meets the move. The mover
/// then serves its records by key: it refuses, as a conflict, every read
/// where a move decides what is there, and a move; it reads everything
/// else; and its commit is the conflict.
#[test]
fn a_commit_under_a_prefix_a_move_names_meets_the_move() {
    for case in 0..8 {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        // A subtree with no node below the source, which the move does
        // not name: it leaves with the move.
        emptied(&mut world);
        let mover = world.begin();
        let batch = rename(&world, &mover, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
        world.apply(&mover, &batch).expect("the subtree is moved");
        world.write(&mover, vec![put(aux(B_IN), b"late", b"under the target")]);
        // And it enters a subtree of its own below the moved one.
        let own = subtrees(vec![hosted(NONE, B, b"k1")]);
        world.apply(&mover, &own).expect("a subtree is entered");

        let other = world.begin();
        match case {
            // Nodes under the source, with the nodes that host them.
            0 => {
                let path: [(u8, &[u8]); 2] = [(A, b"a"), (A_IN, b"in")];
                let batch = edit_at(&world, &other, &path, |items| {
                    items.insert(b"foreign".to_vec(), vec![7; 60]);
                });
                world.apply(&other, &batch).expect("a change");
            }
            // A flat record of a subtree below the source, and no node.
            1 => world.write(&other, vec![put(aux(A_IN), b"f", b"foreign")]),
            // An element at the key the subtree is moved to.
            2 => drop(world.edit(&other, TOP, |items| {
                items.insert(b"b".to_vec(), b"an item".to_vec());
            })),
            // A drop of the subtree below the source, with its element.
            3 => {
                let mut batch = edit_at(&world, &other, &[(A, b"a")], |items| {
                    items.remove(&b"in"[..]);
                });
                batch.subtrees = vec![OwnedSubtree::Drop(sub(A_IN))];
                world.apply(&other, &batch).expect("a drop");
            }
            // A subtree that is entered under the prefix the moved one
            // went to, with no host, a flat record, and a subtree below
            // it that has a flat record too.
            4 => {
                let mut nodes = world.merk(&other, B, |items| {
                    items.insert(b"z".to_vec(), b"a tree".to_vec());
                });
                nodes.hosted.insert(b"z".to_vec(), vec![sub(EMPTY_2)]);
                let entered = OwnedBatch {
                    merks: vec![nodes],
                    records: vec![
                        put(aux(B), b"f", b"foreign"),
                        put(aux(EMPTY_2), b"f", b"foreign"),
                    ],
                    subtrees: vec![enter(B), hosted(EMPTY_2, B, b"z")],
                };
                world.apply(&other, &entered).expect("a subtree is entered");
            }
            // A drop of the subtree above the source, which names neither
            // the source nor a key under it.
            5 => {
                let dropped = subtrees(vec![OwnedSubtree::Drop(sub(TOP))]);
                world.apply(&other, &dropped).expect("a drop");
            }
            // A flat record under the subtree that left with the move.
            6 => world.write(&other, vec![put(aux(EMPTY), b"f", b"foreign")]),
            // And a drop of that subtree, which writes no node.
            _ => {
                let dropped = subtrees(vec![OwnedSubtree::Drop(sub(EMPTY))]);
                world.apply(&other, &dropped).expect("a drop");
            }
        }
        assert!(committed(world.commit(other, &[&mover])), "case {case}");
        if case == 4 {
            // What the committed state has below a prefix the move names
            // is not answered for either, on whichever side.
            let refused = get(&mover, aux(EMPTY_2), b"f").expect_err("a refusal");
            assert!(moved_by_key(&refused, EMPTY_2), "{refused}");
            let refused = (mover.txn.subtree(sub(EMPTY_2), |_| ())).expect_err("a refusal");
            assert!(moved_by_key(&refused, EMPTY_2), "{refused}");
            let record = OwnedBatch {
                records: vec![put(aux(EMPTY_2), b"more", b"by key")],
                ..OwnedBatch::default()
            };
            let refused = world.apply(&mover, &record).expect_err("a refusal");
            assert!(moved_by_key(&refused, EMPTY_2), "{refused}");
        }

        for (unit, key) in [(aux(B), &b"a"[..]), (aux(A), b"a"), (aux(B_IN), b"late")] {
            let refused = get(&mover, unit, key).expect_err("a refusal");
            let name = unit.subtree.as_bytes()[0];
            assert!(moved_by_key(&refused, name), "case {case}: {refused}");
        }
        let refused = (mover.txn.node(sub(B_IN), b"one", |_| ())).expect_err("a refusal");
        assert!(moved_by_key(&refused, B_IN), "case {case}: {refused}");
        // Nor is the subtree that the mover entered below the moved one.
        let refused = get(&mover, aux(NONE), b"x").expect_err("a refusal");
        assert!(moved_by_key(&refused, NONE), "case {case}: {refused}");
        let refused = mover.txn.subtree(sub(A), |_| ()).expect_err("a refusal");
        assert!(moved_by_key(&refused, A), "case {case}: {refused}");
        let refused = (mover.txn.cursor(aux(B)).seek_to_first()).expect_err("a refusal");
        assert!(moved_by_key(&refused, B), "case {case}: {refused}");
        let mut cursor = mover.txn.merk_cursor(sub(B)).expect("a cursor");
        let refused = cursor.seek(b"k").expect_err("a refusal");
        assert!(moved_by_key(&refused, B), "case {case}: {refused}");
        // A write there, and another move, are refused the same way.
        let record = OwnedBatch {
            records: vec![put(aux(B), b"more", b"by key")],
            ..OwnedBatch::default()
        };
        let refused = world.apply(&mover, &record).expect_err("a refusal");
        assert!(moved_by_key(&refused, B), "case {case}: {refused}");
        // And so is a drop or a clear, of whichever subtree, by the first
        // that the batch states.
        let (top, far) = (
            OwnedSubtree::Drop(sub(TOP)),
            OwnedSubtree::Clear(model::subtree(1)),
        );
        for (leaves, name) in [
            (vec![top.clone()], TOP),
            (vec![far.clone()], 1),
            (vec![far.clone(), top.clone()], 1),
            (vec![top, far], TOP),
        ] {
            let refused = (world.apply(&mover, &subtrees(leaves))).expect_err("a refusal");
            assert!(moved_by_key(&refused, name), "case {case}: {refused}");
        }
        // What no move decides is read as before, and the committed
        // records are those of the committed state (RD10).
        let top = (case != 5).then(|| b"top".to_vec());
        assert_eq!(get(&mover, aux(TOP), b"t").expect("a read"), top);
        // The model has as many writes met as the engine. In case 5 the
        // move is one of them, by the place that the subtree it took from
        // its prefix had among the Merks: the commit names no prefix that
        // the move covers.
        let (met, moved) = {
            let model = mover.model.borrow();
            let moved = |(write, _, met): &(Write, u64, bool)| {
                matches!(write, Write::Move(_)).then_some(*met)
            };
            (model.met(), model.writes.iter().find_map(moved))
        };
        assert_eq!(mover.txn.stats().overlapped, met, "case {case}");
        if case == 5 {
            assert_eq!(moved, Some(true), "the move is met");
        }
        let mut out = Vec::new();
        let found = mover.txn.committed_get(aux(B), b"a", &mut out);
        assert!(
            !found.expect("a committed read"),
            "nothing is committed there"
        );
        assert_eq!(world.commit(mover, &[]), Outcome::Conflict, "case {case}");
        world.check_committed();
    }
}

/// Section 6.2, the second and third rows: a mover that commits is a
/// conflict for a transaction beside it that wrote under the source or
/// under a subtree below it, named by the move or not, and a batch of a
/// transaction that read there and is laid out after the mover's commit
/// is refused as a conflict.
#[test]
fn a_move_that_commits_is_a_conflict_for_those_who_wrote_and_read_under_its_source() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    emptied(&mut world);
    // One wrote a flat record under the subtree below the source, and
    // one under the subtree with no node there, which the move does not
    // name and takes out.
    let wrote = world.begin();
    world.write(&wrote, vec![put(aux(A_IN), b"f", b"beside")]);
    let wrote_below = world.begin();
    world.write(&wrote_below, vec![put(aux(EMPTY), b"f", b"beside")]);
    // And one dropped that subtree.
    let dropped_below = world.begin();
    let dropped = subtrees(vec![OwnedSubtree::Drop(sub(EMPTY))]);
    (world.apply(&dropped_below, &dropped)).expect("a drop");
    // And one entered a subtree under the prefix the moved one goes to,
    // and wrote a flat record there.
    let wrote_target = world.begin();
    let entered = OwnedBatch {
        records: vec![put(aux(B), b"f", b"beside")],
        subtrees: vec![enter(B)],
        ..OwnedBatch::default()
    };
    (world.apply(&wrote_target, &entered)).expect("a subtree is entered");
    // One read there, wrote elsewhere, and made a batch from what it read.
    let reader = world.begin();
    world.write(&reader, vec![put(aux(1), b"elsewhere", b"1")]);
    let path: [(u8, &[u8]); 2] = [(A, b"a"), (A_IN, b"in")];
    let stale = edit_at(&world, &reader, &path, |items| {
        items.insert(b"late".to_vec(), vec![7; 60]);
    });
    let stale = read_now(&reader, &stale);
    // One read a node there and kept no value, and one read no node at
    // all: it keeps the key set of no commit.
    let looked = world.begin();
    (looked.txn.node(sub(A_IN), b"one", handle_of)).expect("a read");
    world.write(&looked, vec![put(aux(2), b"elsewhere", b"2")]);
    let blind = world.begin();
    assert!(shown(&blind, A_IN).is_some());
    world.write(&blind, vec![put(aux(3), b"elsewhere", b"3")]);
    let records = OwnedBatch {
        records: vec![put(aux(A_IN), b"f", b"after the move")],
        ..OwnedBatch::default()
    };

    let mover = world.begin();
    let batch = rename(&world, &mover, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&mover, &batch).expect("the subtree is moved");
    let beside = [
        &wrote,
        &wrote_below,
        &dropped_below,
        &wrote_target,
        &reader,
        &looked,
        &blind,
    ];
    assert!(committed(world.commit(mover, &beside)));

    // The batch that was made from the reads under the old prefix: the
    // subtree it writes into is not entered, and the commit took it.
    let refused = world.apply_as_read(&reader, &stale).expect_err("a refusal");
    let subtree = sub(A_IN);
    assert_eq!(conflict(refused), BatchError::ValueTaken { subtree });
    // A record for it, by one that kept no value there and has the key
    // set of the commit: the same.
    let refused = world.apply(&looked, &records).expect_err("a refusal");
    assert_eq!(conflict(refused), BatchError::ValueTaken { subtree });
    // One that read no node has the key set of no commit to tell by: the
    // subtree is not entered, and that is all it says.
    assert_eq!(
        batch_error(world.apply(&blind, &records)),
        BatchError::NotEntered { subtree }
    );
    for tx in [&reader, &looked, &blind] {
        world.check(tx);
    }
    assert!(committed(world.commit(reader, &[&wrote, &looked, &blind])));
    assert!(committed(world.commit(looked, &[&wrote, &blind])));
    assert!(committed(world.commit(blind, &[&wrote])));
    assert_eq!(world.commit(wrote, &[&wrote_below]), Outcome::Conflict);
    world.check(&wrote_below);
    assert_eq!(wrote_below.txn.stats().overlapped, 1);
    assert_eq!(world.commit(wrote_below, &[]), Outcome::Conflict);
    assert_eq!(world.commit(dropped_below, &[]), Outcome::Conflict);
    // What the move writes: every key under the prefix it brought a
    // subtree to.
    assert_eq!(world.commit(wrote_target, &[]), Outcome::Conflict);
    world.check_committed();
}

/// CF10: a commit that changed only flat records under the moved subtree,
/// between the mover's read of the host and its batch, wrote no host and
/// is not dated. The move is taken, is met by nothing, and carries those
/// records as they are.
#[test]
fn a_move_carries_the_flat_records_that_a_commit_wrote_since_the_read_of_the_host() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let pairs = [(A, B), (A_IN, B_IN)];
    let stated = read_now(&tx, &rename(&world, &tx, b"a", b"b", &pairs));
    let other = world.begin();
    let flat = vec![
        put(aux(A_IN), b"since", b"flat"),
        put(aux(A), b"a", b"again"),
    ];
    world.write(&other, flat);
    assert!(committed(world.commit(other, &[&tx])));

    (world.apply_as_read(&tx, &stated)).expect("the subtree is moved");
    world.check(&tx);
    assert_eq!(tx.txn.stats().overlapped, 0);
    let since = get(&tx, aux(B_IN), b"since").expect("a read");
    assert_eq!(since, Some(b"flat".to_vec()));
    let again = get(&tx, aux(B), b"a").expect("a read");
    assert_eq!(again, Some(b"again".to_vec()));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// D-213 item 5: the names that an enter if absent recorded above its
/// host stay as recorded when its transaction moves the subtree the host
/// is in. A commit beside the transaction that drops that subtree, under
/// the prefix the committed state has it, meets the write the
/// transaction made under the entered subtree before its move: one more
/// write is met than where the transaction made none there.
#[test]
fn the_names_of_an_enter_if_absent_stay_when_its_transaction_moves_the_host() {
    let met = |first: bool| {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        let tx = world.begin();
        if first {
            // The first write into a subtree that nothing has entered,
            // under the node `k1` of the subtree that is moved next.
            let host = Some((sub(A), b"k1".to_vec(), 0));
            let batch = OwnedBatch {
                records: vec![put(aux(EMPTY_2), b"f", b"first")],
                subtrees: vec![OwnedSubtree::EnterIfAbsent(sub(EMPTY_2), host)],
                ..OwnedBatch::default()
            };
            world.apply(&tx, &batch).expect("a first write");
        }
        let pairs = [(A, B), (A_IN, B_IN), (EMPTY_2, NONE_B)];
        let batch = rename(&world, &tx, b"a", b"b", &pairs);
        world.apply(&tx, &batch).expect("the subtree is moved");
        world.check(&tx);
        let written = get(&tx, aux(NONE_B), b"f").expect("a read");
        assert_eq!(written.is_some(), first);

        // The subtree is dropped with its element, under the prefix the
        // committed state has it.
        let other = world.begin();
        let top = world.merk(&other, TOP, |items| {
            items.remove(&b"a"[..]);
        });
        let dropped = OwnedBatch {
            merks: vec![top],
            subtrees: vec![OwnedSubtree::Drop(sub(A))],
            ..OwnedBatch::default()
        };
        world.apply(&other, &dropped).expect("a drop");
        assert!(committed(world.commit(other, &[&tx])));
        let refused = get(&tx, aux(B), b"a").expect_err("a refusal");
        assert!(moved_by_key(&refused, B), "{refused}");
        let met = tx.txn.stats().overlapped;
        assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
        world.check_committed();
        met
    };
    let (without, with) = (met(false), met(true));
    assert!(without > 0);
    assert_eq!(with, without + 1);
}

/// The batch that moves what the node `b` of `TOP` hosts to `c`, and
/// names no read of that node.
fn unread<'s>(world: &World<'s>, tx: &Tx<'s>, pairs: &[(u8, u8)]) -> OwnedBatch {
    let mut batch = rename(world, tx, b"b", b"c", pairs);
    let OwnedSubtree::Move { read, .. } = &mut batch.subtrees[0] else {
        unreachable!("a move");
    };
    *read = None;
    batch
}

/// Case 11, section 6.1: a commit outside that writes the node which
/// hosts the subtree, after the mover read it and before its batch,
/// refuses the batch as a conflict. The transaction is as it was, and the
/// move made again from a new read is taken.
#[test]
fn a_move_is_dated_at_the_read_of_the_node_that_hosted_the_subtree() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"own", b"kept")]);
    let pairs = [(A, B), (A_IN, B_IN)];
    let stale = read_now(&tx, &rename(&world, &tx, b"a", b"b", &pairs));

    // A change under the subtree, which writes the node that hosts it.
    let other = world.begin();
    let path: [(u8, &[u8]); 2] = [(A, b"a"), (A_IN, b"in")];
    let change = edit_at(&world, &other, &path, |items| {
        items.insert(b"foreign".to_vec(), vec![7; 60]);
    });
    world.apply(&other, &change).expect("a change");
    assert!(committed(world.commit(other, &[&tx])));

    let refused = world.apply_as_read(&tx, &stale).expect_err("a refusal");
    let subtree = sub(TOP);
    assert_eq!(conflict(refused), BatchError::ValueTaken { subtree });
    world.check(&tx);
    assert_eq!(tx.txn.stats().overlapped, 0);
    assert!(shown(&tx, A).is_some() && shown(&tx, B).is_none());

    // From a new read the move is taken, with what the commit wrote.
    let fresh = rename(&world, &tx, b"a", b"b", &pairs);
    world.apply(&tx, &fresh).expect("the subtree is moved");
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();

    // A move that names no read of a node the committed state has is
    // refused, and one of a node the transaction wrote itself is taken.
    let tx = world.begin();
    let pairs = [(B, C), (B_IN, C_IN)];
    assert_eq!(
        batch_error(world.apply(&tx, &unread(&world, &tx, &pairs))),
        BatchError::NoRead { subtree }
    );
    let own = edit_at(&world, &tx, &[(B, b"b")], |items| {
        items.insert(b"k9".to_vec(), vec![9; 9]);
    });
    world.apply(&tx, &own).expect("the host is written");
    (world.apply(&tx, &unread(&world, &tx, &pairs))).expect("the subtree is moved");
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();

    // A read that the transaction cannot date is taken to be met: it
    // forgot its reads when it was rolled back.
    let tx = world.begin();
    let pairs = [(C, A), (C_IN, A_IN)];
    let old = read_now(&tx, &rename(&world, &tx, b"c", b"a", &pairs));
    tx.txn.rollback().expect("no scope is open");
    let refused = world.apply_as_read(&tx, &old).expect_err("a refusal");
    assert_eq!(conflict(refused), BatchError::ValueTaken { subtree });
    world.check(&tx);
    let fresh = rename(&world, &tx, b"c", b"a", &pairs);
    world.apply(&tx, &fresh).expect("the subtree is moved");
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// A rollback to a savepoint from before a move that a commit has met
/// gives the transaction its frames again, and a move that is stated
/// while it serves its records by key is refused as a conflict.
#[test]
fn a_met_move_goes_with_a_rollback_and_none_is_stated_by_key() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let pairs = [(A, B), (A_IN, B_IN)];
    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"own", b"kept")]);
    tx.txn.set_savepoint().expect("no scope is open");
    let before = tx.model.borrow().writes.len();
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    world.apply(&tx, &batch).expect("the subtree is moved");

    let other = world.begin();
    world.write(&other, vec![put(aux(A), b"f", b"foreign")]);
    assert!(committed(world.commit(other, &[&tx])));
    let refused = get(&tx, aux(B), b"f").expect_err("a refusal");
    assert!(moved_by_key(&refused, B), "{refused}");

    tx.txn.rollback_to_savepoint().expect("a savepoint is set");
    tx.model.borrow_mut().writes.truncate(before);
    world.check(&tx);
    assert_eq!(
        get(&tx, aux(A), b"f").expect("a read"),
        Some(b"foreign".to_vec())
    );
    assert!(tx.txn.stats().base.is_some(), "frames again");

    // A write of its own that a commit meets: it serves its records by
    // key, and a move is not stated then.
    world.write(&tx, vec![put(aux(A_IN), b"mine", b"1")]);
    let stated = rename(&world, &tx, b"a", b"b", &pairs);
    let other = world.begin();
    world.write(&other, vec![put(aux(A_IN), b"mine", b"2")]);
    assert!(committed(world.commit(other, &[&tx])));
    let refused = world.apply(&tx, &stated).expect_err("a refusal");
    assert!(moved_by_key(&refused, A), "{refused}");
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
}

/// Frames that are made again apply a move again, over a directory that
/// can have other subtrees below the moved one than when it was stated:
/// one with no node that a commit beside entered leaves as well, with its
/// flat records, and the mover reads none of them from then on.
#[test]
fn a_move_is_made_again_over_a_directory_with_other_subtrees_below_it() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    emptied(&mut world);
    let tx = world.begin();
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    let rebuilt = tx.txn.stats().rebuilds;

    // A commit that enters a subtree below the moved one and writes no
    // key under a prefix the move names meets nothing.
    let other = world.begin();
    let entered = subtrees(vec![hosted(EMPTY_2, A, b"e")]);
    world
        .apply(&other, &entered)
        .expect_err("the host has a subtree");
    let entered = OwnedBatch {
        records: vec![put(aux(EMPTY_2), b"f", b"of another")],
        subtrees: vec![hosted(EMPTY_2, A, b"k1")],
        ..OwnedBatch::default()
    };
    world.apply(&other, &entered).expect("a subtree is entered");
    assert!(committed(world.commit(other, &[&tx])));
    assert_eq!(tx.txn.stats().overlapped, 0);
    world.check(&tx);
    assert!(
        tx.txn.stats().rebuilds > rebuilt,
        "the frames were made again"
    );
    for gone in [EMPTY, EMPTY_2, A, A_IN] {
        assert_eq!(shown(&tx, gone), None, "{gone}");
    }
    // The move takes the subtree out with its flat record, which the
    // committed state has until the mover commits.
    assert_eq!(get(&tx, aux(EMPTY_2), b"f").expect("a read"), None);
    let mut cursor = tx.txn.cursor(aux(EMPTY_2));
    cursor.seek_to_first().expect("a move");
    assert!(!cursor.valid());
    drop(cursor);
    let mut was = Vec::new();
    assert!((tx.txn.committed_get(aux(EMPTY_2), b"f", &mut was)).expect("a read"));
    // The move covers what it covered when it was stated: a later commit
    // under the subtree that leaves with it since is not met either.
    let later = world.begin();
    world.write(&later, vec![put(aux(EMPTY_2), b"g", b"later")]);
    assert!(committed(world.commit(later, &[&tx])));
    assert_eq!(tx.txn.stats().overlapped, 0);
    world.check(&tx);
    assert_eq!(get(&tx, aux(EMPTY_2), b"g").expect("a read"), None);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert_eq!(
        [shown(&reader, EMPTY), shown(&reader, EMPTY_2)],
        [None, None]
    );
    for key in [b"f", b"g"] {
        assert_eq!(get(&reader, aux(EMPTY_2), key).expect("a read"), None);
    }
    assert!(shown(&reader, B_IN).is_some());
    assert_eq!(db.private_ids(), 0);
}

/// D-213 item 3: where a move cannot be made again, the transaction is
/// not laid out, and that is no conflict. A commit beside the mover
/// enters a subtree under the prefix the moved one went to and writes no
/// key: it has no host, so no node is written for it. No key set tells
/// of it, and the move finds the prefix taken. From then on the mover
/// refuses every read that its move decides, and its commit, with the
/// refusal that stops the move; it reads everything else, and can be
/// rolled back. Whichever call is the first after that commit finds the
/// layout stopped: a read by key, or a move of a cursor of either kind.
#[test]
fn a_move_that_cannot_be_made_again_is_refused_with_what_stops_it() {
    for first in 0..3 {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        let tx = world.begin();
        let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
        world.apply(&tx, &batch).expect("the subtree is moved");

        let other = world.begin();
        let entered = subtrees(vec![enter(B)]);
        world.apply(&other, &entered).expect("a subtree is entered");
        assert!(committed(world.commit(other, &[&tx])));

        let stops = |error: &TxnError| {
            let entered = matches!(
                error,
                TxnError::Batch(BatchError::Entered { subtree }) if *subtree == sub(B)
            );
            entered && !error.is_conflict()
        };
        let refused = match first {
            0 => get(&tx, aux(B), b"a").expect_err("a refusal"),
            1 => (tx.txn.cursor(aux(B)).seek_to_first()).expect_err("a refusal"),
            _ => {
                let mut cursor = tx.txn.merk_cursor(sub(B)).expect("a cursor");
                cursor.seek_to_first().expect_err("a refusal")
            }
        };
        assert!(stops(&refused), "{first}: {refused}");
        for (unit, key) in [(aux(B), &b"a"[..]), (aux(A), b"a"), (aux(B_IN), b"ia")] {
            let refused = get(&tx, unit, key).expect_err("a refusal");
            assert!(stops(&refused), "{refused}");
            let refused = (tx.txn.cursor(unit).seek(key)).expect_err("a refusal");
            assert!(stops(&refused), "{refused}");
        }
        let refused = tx.txn.subtree(sub(B), |_| ()).expect_err("a refusal");
        assert!(stops(&refused), "{refused}");
        assert_eq!(tx.txn.stats().overlapped, 0);
        assert_eq!(
            get(&tx, aux(TOP), b"t").expect("a read"),
            Some(b"top".to_vec())
        );
        let refused = db.commit(&versions, &tx.txn).expect_err("no layout");
        assert!(stops(&refused), "{refused}");
        tx.txn.rollback().expect("no scope is open");
        tx.forget();
        world.check(&tx);
        world.check_committed();
        assert_eq!(db.private_ids(), 0);
    }
}

/// A look at what the drops of commits cover that fails leaves its
/// commits owed (CF5). A cursor of a transaction that moved a subtree
/// begins as each of its operations does. One that was opened before a
/// commit, and steps after the look at that commit failed, makes the look
/// that is owed, at the version the commit has: the write below the
/// subtree that the commit dropped is met.
#[test]
fn a_cursor_that_steps_after_a_failed_look_makes_the_look_that_is_owed() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    // A Merk with no host, whose node `x` hosts a subtree with no node.
    let tx = world.begin();
    let mut root = world.merk(&tx, ROOT, |items| {
        items.insert(b"x".to_vec(), b"a tree".to_vec());
    });
    root.hosted.insert(b"x".to_vec(), vec![sub(NONE)]);
    let made = OwnedBatch {
        merks: vec![root],
        subtrees: vec![enter(ROOT), hosted(NONE, ROOT, b"x")],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &made).expect("the subtrees are made");
    assert!(committed(world.commit(tx, &[])));

    let mover = world.begin();
    let batch = rename(&world, &mover, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&mover, &batch).expect("the subtree is moved");
    world.write(&mover, vec![put(aux(NONE), b"k", b"below")]);
    let mut cursor = mover.txn.cursor(aux(TOP));
    cursor.seek_to_first().expect("a move");
    assert_eq!(cursor.key(), Some(&b"t"[..]));

    // A commit drops the Merk above, and names no key of the subtree.
    let other = world.begin();
    let dropped = subtrees(vec![OwnedSubtree::Drop(sub(ROOT))]);
    world.apply(&other, &dropped).expect("a drop");
    assert!(committed(world.commit(other, &[&mover])));
    mover.txn.refuse_look();
    let refused = get(&mover, aux(TOP), b"t");
    assert!(matches!(refused, Err(TxnError::Damaged(_))), "{refused:?}");
    assert_eq!(mover.txn.stats().overlapped, 0);
    cursor.next().expect("a step");
    drop(cursor);
    assert_eq!(
        mover.txn.stats().overlapped,
        1,
        "the write below the drop is met"
    );
    assert_eq!(world.commit(mover, &[]), Outcome::Conflict);
    world.check_committed();
}

/// A pair that a move states for a subtree that is not there gives no
/// subtree another prefix when it is stated, and stays stated. Once a
/// commit beside has entered a subtree under that prefix below the moved
/// one, the move that is made again takes it to the prefix the pair
/// names. That commit writes no key under a prefix the move covers and
/// is not met.
#[test]
fn a_pair_for_a_subtree_that_is_entered_later_takes_it_when_the_move_is_made_again() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let pairs = [(A, B), (A_IN, B_IN), (NONE, NONE_B)];
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    world.apply(&tx, &batch).expect("the subtree is moved");
    assert_eq!([shown(&tx, NONE), shown(&tx, NONE_B)], [None, None]);

    let other = world.begin();
    let entered = subtrees(vec![hosted(NONE, A, b"k1")]);
    world.apply(&other, &entered).expect("a subtree is entered");
    assert!(committed(world.commit(other, &[&tx])));
    assert_eq!(tx.txn.stats().overlapped, 0);
    world.check(&tx);
    let moved = |tx: &Tx<'_>| {
        assert_eq!(shown(tx, NONE), None);
        let (root, _, above) = shown(tx, NONE_B).expect("the subtree the pair names");
        assert_eq!((root, above), (None, host(B, b"k1")));
    };
    moved(&tx);
    let hosts = tx.model.borrow().hosts(&world.model.hosts);
    assert_eq!(hosts.get(&sub(NONE_B)), Some(&sub(B)));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    moved(&reader);
}

/// CF10 between two movers: a move covers every prefix it names and
/// every subtree that left with it. One transaction enters a subtree with
/// no node below the subtree it moves and names no prefix for it. Another
/// moves a subtree elsewhere and names that prefix, in a pair for a
/// subtree that is not there. The two write no key in common but those
/// under that prefix, and whichever commits second is the conflict.
#[test]
fn two_moves_meet_under_a_prefix_that_one_lets_leave_and_the_other_names() {
    for order in [0, 1] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        // A Merk with no host, whose node `c` hosts a subtree.
        let tx = world.begin();
        let mut root = world.merk(&tx, ROOT, |items| {
            items.insert(b"c".to_vec(), b"a tree".to_vec());
        });
        root.hosted.insert(b"c".to_vec(), vec![sub(C)]);
        let inner = world.merk(&tx, C, |items| {
            items.insert(b"n".to_vec(), b"v".to_vec());
        });
        let made = OwnedBatch {
            merks: vec![inner, root],
            subtrees: vec![enter(ROOT), hosted(C, ROOT, b"c")],
            ..OwnedBatch::default()
        };
        world.apply(&tx, &made).expect("the subtrees are made");
        assert!(committed(world.commit(tx, &[])));

        let one = world.begin();
        let entered = subtrees(vec![hosted(NONE, A, b"k1")]);
        world.apply(&one, &entered).expect("a subtree is entered");
        let batch = rename(&world, &one, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
        world.apply(&one, &batch).expect("the subtree is moved");
        assert_eq!(shown(&one, NONE), None, "it left with the move");

        let two = world.begin();
        let mut root = world.merk(&two, ROOT, |items| {
            let value = items.remove(&b"c"[..]).expect("the host");
            items.insert(b"d".to_vec(), value);
        });
        root.hosted.insert(b"d".to_vec(), vec![sub(C_IN)]);
        let read = (two.txn.node(sub(ROOT), b"c", handle_of)).expect("the host is read");
        let batch = OwnedBatch {
            merks: vec![root],
            subtrees: vec![OwnedSubtree::Move {
                from: sub(C),
                to: sub(C_IN),
                host: (sub(ROOT), b"d".to_vec(), 0),
                read,
                below: vec![(sub(NONE), sub(NONE_B))],
            }],
            ..OwnedBatch::default()
        };
        world.apply(&two, &batch).expect("the subtree is moved");

        let (first, second) = match order {
            0 => (one, two),
            _ => (two, one),
        };
        assert!(committed(world.commit(first, &[&second])), "{order}");
        assert_eq!(get(&second, aux(1), b"none").expect("a read"), None);
        assert_eq!(second.txn.stats().overlapped, 1, "{order}: the move is met");
        assert_eq!(world.commit(second, &[]), Outcome::Conflict, "{order}");
        world.check_committed();
    }
}

/// Commits, under the node `f` of `TOP`, a Merk `C` whose one node `h`
/// hosts the subtree `C_IN`, which has no node and one flat record. A
/// transaction then removes that node and clears `C` while it has none,
/// which leaves `C_IN` entered: that transaction is returned.
///
/// With `found`, the transaction also enters `C` if it is absent, as it
/// is entered, which finds it and leaves it as it is: before it removes
/// the node (`false`), or in the batch of the clear (`true`).
///
/// The model is not told of the clear, nor of that enter.
fn cleared_with_no_node<'s>(world: &mut World<'s>, found: Option<bool>) -> Tx<'s> {
    let tx = world.begin();
    let mut top = world.merk(&tx, TOP, |items| {
        items.insert(b"f".to_vec(), b"a tree".to_vec());
    });
    top.hosted.insert(b"f".to_vec(), vec![sub(C)]);
    let mut outer = world.merk(&tx, C, |items| {
        items.insert(b"h".to_vec(), b"a tree with no node".to_vec());
    });
    outer.hosted.insert(b"h".to_vec(), vec![sub(C_IN)]);
    let made = OwnedBatch {
        merks: vec![outer, top],
        records: vec![put(aux(C_IN), b"k", b"kept")],
        subtrees: vec![hosted(C, TOP, b"f"), hosted(C_IN, C, b"h")],
    };
    world.apply(&tx, &made).expect("the subtrees are made");
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    let entered = OwnedSubtree::EnterIfAbsent(sub(C), host(TOP, b"f"));
    if found == Some(false) {
        let entered = subtrees(vec![entered.clone()]);
        let scope = tx.txn.scope().expect("a scope");
        (entered.with(|batch| scope.apply(batch))).expect("the subtree is found");
        scope.merge();
    }
    let removed = edit_at(world, &tx, &[(C, b"f")], |items| {
        items.remove(&b"h"[..]);
    });
    world.apply(&tx, &removed).expect("the node is removed");
    assert!(
        shown(&tx, C_IN).is_some(),
        "the subtree below stays entered"
    );
    let mut cleared = vec![OwnedSubtree::Clear(sub(C))];
    cleared.extend((found == Some(true)).then_some(entered));
    let cleared = subtrees(cleared);
    let scope = tx.txn.scope().expect("a scope");
    cleared.with(|batch| scope.apply(batch)).expect("a clear");
    scope.merge();
    assert!(shown(&tx, C_IN).is_some(), "and stays so through the clear");
    tx
}

/// Gives the Merk `C` of [`cleared_with_no_node`] its node again.
fn node_again<'s>(world: &World<'s>, tx: &Tx<'s>) {
    let mut again = edit_at(world, tx, &[(C, b"f")], |items| {
        items.insert(b"h".to_vec(), b"a tree with no node".to_vec());
    });
    (again.merks[0].hosted).insert(b"h".to_vec(), vec![sub(C_IN)]);
    world.apply(tx, &again).expect("the node is put back");
}

/// What the transaction dropped or cleared before a move says nothing of
/// the committed records under the prefix a subtree was taken from: which
/// Merk the transaction has under the new prefix is its frames' to say.
/// A Merk whose one node hosts a subtree with no node loses that node, is
/// cleared while it has none, which leaves the subtree below entered,
/// gets the node back, and is moved with the subtree below it. The record
/// of that subtree is read under its prefix before the move, and under
/// its new prefix afterwards, by key and by cursor, and is there after
/// the commit. An enter of the Merk if it is absent, which finds it,
/// changes nothing of that, before the node is removed or in the batch
/// of the clear.
#[test]
fn a_clear_from_before_a_move_hides_nothing_of_a_subtree_it_left_entered() {
    for found in [None, Some(false), Some(true)] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        let tx = cleared_with_no_node(&mut world, found);
        hides_nothing(&mut world, tx);
    }
}

/// What [`a_clear_from_before_a_move_hides_nothing_of_a_subtree_it_left_entered`]
/// asks of the transaction of [`cleared_with_no_node`].
fn hides_nothing<'s>(world: &mut World<'s>, tx: Tx<'s>) {
    let was = vec![(b"k".to_vec(), b"kept".to_vec())];
    let read = |tx: &Tx<'_>, name: u8| get(tx, aux(name), b"k").expect("a read");
    let mut source = tx.txn.cursor(aux(C_IN));
    assert_eq!(read(&tx, C_IN), Some(b"kept".to_vec()));
    assert_eq!(records(&mut source), was);
    node_again(world, &tx);
    assert_eq!(read(&tx, C_IN), Some(b"kept".to_vec()));
    assert_eq!(records(&mut source), was);
    let batch = rename(world, &tx, b"f", b"g", &[(C, FREE), (C_IN, FREE_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    world.check(&tx);
    let mut target = tx.txn.cursor(aux(FREE_IN));
    assert_eq!(records(&mut target), was);
    assert_eq!(read(&tx, C_IN), None);
    assert!(records(&mut source).is_empty());
    drop((source, target));
    let kept = |tx: &Tx<'_>| {
        assert_eq!(
            get(tx, aux(FREE_IN), b"k").expect("a read"),
            Some(b"kept".to_vec())
        );
        let mut cursor = tx.txn.cursor(aux(FREE_IN));
        cursor.seek_to_first().expect("a move");
        assert_eq!(cursor.key(), Some(&b"k"[..]));
    };
    kept(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    kept(&reader);
}

/// A drop takes what is below its subtree, whatever the committed state
/// has of the Merk's nodes. A Merk is committed with no node and a
/// subtree entered below it. A transaction gives it a node again and
/// drops it: the record of the subtree below is read no more, by key or
/// by cursor, and the subtree is not entered, before the commit and
/// after.
#[test]
fn a_drop_takes_the_subtree_below_a_merk_that_was_committed_with_no_node() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let mut top = world.merk(&tx, TOP, |items| {
        items.insert(b"f".to_vec(), b"a tree".to_vec());
    });
    top.hosted.insert(b"f".to_vec(), vec![sub(C)]);
    let mut outer = world.merk(&tx, C, |items| {
        items.insert(b"h".to_vec(), b"a tree with no node".to_vec());
    });
    outer.hosted.insert(b"h".to_vec(), vec![sub(C_IN)]);
    let made = OwnedBatch {
        merks: vec![outer, top],
        records: vec![put(aux(C_IN), b"k", b"kept")],
        subtrees: vec![hosted(C, TOP, b"f"), hosted(C_IN, C, b"h")],
    };
    world.apply(&tx, &made).expect("the subtrees are made");
    assert!(committed(world.commit(tx, &[])));
    // The Merk loses its node, and the subtree below it stays entered:
    // no state of a grove, and committed so here.
    let tx = world.begin();
    let removed = edit_at(&world, &tx, &[(C, b"f")], |items| {
        items.remove(&b"h"[..]);
    });
    world.apply(&tx, &removed).expect("the node is removed");
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    node_again(&world, &tx);
    let top = world.merk(&tx, TOP, |items| {
        items.remove(&b"f"[..]);
    });
    let dropped = OwnedBatch {
        merks: vec![top],
        subtrees: vec![OwnedSubtree::Drop(sub(C))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &dropped).expect("a drop");
    let gone = |tx: &Tx<'_>| {
        assert_eq!(get(tx, aux(C_IN), b"k").expect("a read"), None);
        assert!(records(&mut tx.txn.cursor(aux(C_IN))).is_empty());
        assert_eq!(shown(tx, C_IN), None);
        assert_eq!(shown(tx, C), None);
    };
    gone(&tx);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    gone(&reader);
}

/// An enter of a Merk if it is absent that finds the Merk says nothing of
/// the Merk's root. A Merk with nodes hosts a subtree with a flat record.
/// A transaction enters the Merk if it is absent, as it is entered, and
/// then clears it: the clear takes the subtree below, and its record is
/// read no more, by key or by a cursor, before the commit and after.
///
/// The model is not told of the enter, which changes nothing.
#[test]
fn a_clear_after_an_enter_if_absent_that_found_the_merk_takes_what_is_below_it() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let entered = subtrees(vec![OwnedSubtree::EnterIfAbsent(sub(A), host(TOP, b"a"))]);
    let scope = tx.txn.scope().expect("a scope");
    (entered.with(|batch| scope.apply(batch))).expect("the subtree is found");
    scope.merge();
    assert_eq!(
        get(&tx, aux(A_IN), b"ia").expect("a read"),
        Some(vec![1; 9])
    );
    let mut top = world.merk(&tx, TOP, |items| {
        items.get_mut(&b"a"[..]).expect("the host").push(0);
    });
    top.hosted.insert(b"a".to_vec(), vec![sub(A)]);
    let cleared = OwnedBatch {
        merks: vec![top],
        subtrees: vec![OwnedSubtree::Clear(sub(A))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &cleared).expect("a clear");
    let gone = |tx: &Tx<'_>| {
        assert_eq!(get(tx, aux(A_IN), b"ia").expect("a read"), None);
        assert!(records(&mut tx.txn.cursor(aux(A_IN))).is_empty());
        assert_eq!(shown(tx, A_IN), None);
        let (root, ..) = shown(tx, A).expect("the Merk is entered");
        assert_eq!(root, None);
    };
    gone(&tx);
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    gone(&reader);
}

/// What a clear took is asked of the transaction's writes and of the
/// version that is read, and not of the transaction's frames. So a
/// record of a subtree that no move names, below a Merk that was cleared
/// with no node, is read whether or not the transaction has frames, and
/// whichever cursor asks. A rollback to a
/// savepoint takes the frames away, and a cursor that is opened then
/// reads the record at its first seek, as a read by key does. Of two
/// cursors of one version one has read the record; a commit is published,
/// and the transaction's writes are laid out over it; both read the
/// record. A scope beside another drops a subtree and keeps that write in
/// the write set; a cursor reads the record while it does, and again once
/// the scope has merged, as a cursor that is opened then does.
#[test]
fn a_record_below_a_merk_that_was_cleared_with_no_node_is_read_with_frames_and_without() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    alone(&mut world);
    let tx = cleared_with_no_node(&mut world, None);
    node_again(&world, &tx);
    let was = vec![(b"k".to_vec(), b"kept".to_vec())];
    let read = |tx: &Tx<'_>| get(tx, aux(C_IN), b"k").expect("a read");

    tx.txn.set_savepoint().expect("no scope is open");
    let before = tx.model.borrow().writes.len();
    world.write(&tx, vec![put(aux(TOP), b"s", b"taken back")]);
    tx.txn.rollback_to_savepoint().expect("a savepoint is set");
    tx.model.borrow_mut().writes.truncate(before);
    let mut first_call = tx.txn.cursor(aux(C_IN));
    assert_eq!(records(&mut first_call), was, "with no frames");
    assert_eq!(read(&tx), Some(b"kept".to_vec()));
    assert_eq!(records(&mut first_call), was);
    drop(first_call);

    let mut warmed = tx.txn.cursor(aux(C_IN));
    let mut unused = tx.txn.cursor(aux(C_IN));
    assert_eq!(records(&mut warmed), was);
    let beside = OwnedBatch {
        records: vec![put(aux(TOP), b"u", b"beside")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&beside, &[&tx])));
    assert_eq!(
        get(&tx, aux(TOP), b"u").expect("a read"),
        Some(b"beside".to_vec())
    );
    assert_eq!(tx.txn.stats().overlapped, 0);
    assert_eq!(records(&mut warmed), was);
    assert_eq!(records(&mut unused), was);
    drop((warmed, unused));

    let alone = tx.txn.scope().expect("a scope");
    let beside = tx.txn.scope().expect("a scope");
    assert!(!beside.is_laid_out());
    let dropped = subtrees(vec![OwnedSubtree::Drop(sub(ROOT))]);
    world.apply_in(&tx, &beside, &dropped).expect("a drop");
    let mut cursor = tx.txn.cursor(aux(C_IN));
    assert_eq!(records(&mut cursor), was);
    alone.merge();
    beside.merge();
    assert_eq!(records(&mut cursor), was);
    let mut opened_then = tx.txn.cursor(aux(C_IN));
    assert_eq!(records(&mut opened_then), was);
    assert_eq!(read(&tx), Some(b"kept".to_vec()));
    drop((cursor, opened_then));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert_eq!(read(&reader), Some(b"kept".to_vec()));
}

/// How many Merks the cell of the node `key` of `TOP` hosts in the latest
/// version.
fn pointers(world: &World<'_>, key: &[u8]) -> usize {
    use crate::txn::pages::{find, NodePages};
    let guard = world.path.versions.guard();
    let at = world.latest();
    let directory = world.path.directory_at(at).expect("a directory");
    let entry = {
        let pages = world.path.committed(&guard, at);
        directory
            .open(&pages, sub(TOP).as_bytes())
            .expect("a lookup")
    };
    let root = entry.expect("the Merk").root().map(|root| root.address);
    let lease = lignin_tree::node::Lease::take();
    let mut buffers = lignin_store::load::LoadBuffers::new();
    let mut pages = world.path.committed_pages(&guard, at, lease, &mut buffers);
    let cell = find(&mut pages, root, key).expect("a search");
    let cell = cell.expect("the element");
    let hosted = pages.page(cell.page(), |page| {
        Some(page.node(cell.slot())?.hosted_roots().count())
    });
    hosted.expect("the page").expect("the cell")
}

/// MV3, for a subtree whose host's cell hosts no Merk: the entry of the
/// subtree names the element, and the element's cell points at nothing,
/// as that of a tree that keeps its data flat does. Such a subtree is
/// moved by a batch whose new host names no Merk either, by a list that
/// is empty or by none, with its flat records in every column, its data
/// among them. The cell of the new host hosts none, the store is sound,
/// and the next reader finds the subtree under the new key. A new host
/// that names the new prefix is refused, with nothing changed.
#[test]
fn a_subtree_whose_host_hosts_no_merk_is_moved_as_it_is() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let data = |name: u8| model::unit(Column::Data, name);
    let meta = |name: u8| model::unit(Column::Meta, name);
    // The element `f` of `TOP` hosts no Merk, and the subtree `C` is
    // entered under it, with flat records.
    let tx = world.begin();
    let top = world.merk(&tx, TOP, |items| {
        items.insert(b"f".to_vec(), b"a tree with flat data".to_vec());
    });
    let made = OwnedBatch {
        merks: vec![top],
        records: vec![
            put(data(C), b"d", b"flat"),
            put(aux(C), b"x", b"beside"),
            put(meta(C), b"m", b"meta"),
            put(model::unit(Column::Roots, C), b"r", b"roots"),
        ],
        subtrees: vec![hosted(C, TOP, b"f")],
    };
    world.apply(&tx, &made).expect("a subtree with flat data");
    assert!(committed(world.commit(tx, &[])));
    assert_eq!(pointers(&world, b"f"), 0, "the element hosts no Merk");
    assert_eq!(pointers(&world, b"a"), 1);

    for stated in [false, true] {
        let tx = world.begin();
        let was = shown(&tx, C);
        let named = rename(&world, &tx, b"f", b"g", &[(C, C_IN)]);
        assert!(named.merks[0].hosted.contains_key(&b"g"[..]));
        let refused = world
            .apply(&tx, &named)
            .expect_err("the new host names a Merk");
        assert!(!refused.is_conflict(), "{refused}");
        let subtree = sub(C);
        assert_eq!(batch_error(Err(refused)), BatchError::MoveHost { subtree });
        // Nor one that names it at another position than the stated one.
        let mut elsewhere = named.clone();
        (elsewhere.merks[0].hosted).insert(b"g".to_vec(), vec![sub(A), sub(C_IN)]);
        let refused = world
            .apply(&tx, &elsewhere)
            .expect_err("named at another position");
        assert_eq!(batch_error(Err(refused)), BatchError::MoveHost { subtree });
        // Nor one that names another subtree at the position.
        let mut other = named.clone();
        (other.merks[0].hosted).insert(b"g".to_vec(), vec![sub(A)]);
        let refused = world
            .apply(&tx, &other)
            .expect_err("another Merk at the position");
        assert_eq!(batch_error(Err(refused)), BatchError::MoveHost { subtree });
        world.check(&tx);
        assert_eq!(tx.txn.stats().writes, 0);
        let mut batch = named.clone();
        match stated {
            true => drop((batch.merks[0].hosted).insert(b"g".to_vec(), Vec::new())),
            false => drop(batch.merks[0].hosted.remove(&b"g"[..])),
        }
        world.apply(&tx, &batch).expect("the subtree is moved");
        world.check(&tx);
        assert_eq!(shown(&tx, C), None);
        assert_eq!(shown(&tx, C_IN), under(&was, TOP, b"g"));
        assert_eq!(
            get(&tx, data(C_IN), b"d").expect("a read"),
            Some(b"flat".to_vec())
        );
        match stated {
            true => assert!(committed(world.commit(tx, &[]))),
            false => {
                tx.txn.rollback().expect("no scope is open");
                tx.forget();
            }
        }
    }
    world.check_committed();
    assert_eq!(pointers(&world, b"g"), 0, "nor does the new host");
    let reader = world.begin();
    assert!(shown(&reader, C_IN).is_some() && shown(&reader, C).is_none());
    for (column, key, value) in [
        (Column::Data, &b"d"[..], &b"flat"[..]),
        (Column::Aux, b"x", b"beside"),
        (Column::Meta, b"m", b"meta"),
        (Column::Roots, b"r", b"roots"),
    ] {
        let (unit, was) = (model::unit(column, C_IN), model::unit(column, C));
        assert_eq!(get(&reader, was, key).expect("a read"), None, "{was}");
        assert_eq!(
            get(&reader, unit, key).expect("a read"),
            Some(value.to_vec())
        );
    }
    assert_eq!(db.private_ids(), 0);
}

/// MV3 and MV4, for a subtree whose host's cell hosts no Merk and one that
/// its host's cell points at: they change places in one batch. The node
/// that hosted the Merk hosts none afterwards and is put saying so; one
/// that keeps the Merks it hosts would go on pointing at the Merk that
/// left, and is refused. The node that hosted none names the Merk.
#[test]
fn a_subtree_whose_host_hosts_no_merk_changes_places_with_one_that_is_hosted() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let data = |name: u8| model::unit(Column::Data, name);
    let tx = world.begin();
    let top = world.merk(&tx, TOP, |items| {
        items.insert(b"f".to_vec(), b"a tree with flat data".to_vec());
    });
    let made = OwnedBatch {
        merks: vec![top],
        records: vec![put(data(C), b"d", b"flat")],
        subtrees: vec![hosted(C, TOP, b"f")],
    };
    world.apply(&tx, &made).expect("a subtree with flat data");
    assert!(committed(world.commit(tx, &[])));
    assert_eq!([pointers(&world, b"f"), pointers(&world, b"a")], [0, 1]);

    let tx = world.begin();
    let mut top = world.merk(&tx, TOP, |items| {
        let (a, f) = (items[&b"a"[..]].clone(), items[&b"f"[..]].clone());
        items.insert(b"a".to_vec(), f);
        items.insert(b"f".to_vec(), a);
    });
    top.hosted.insert(b"f".to_vec(), vec![sub(B)]);
    top.hosted.insert(b"a".to_vec(), Vec::new());
    let read = |key: &[u8]| (tx.txn.node(sub(TOP), key, handle_of)).expect("the host is read");
    let good = OwnedBatch {
        merks: vec![top],
        subtrees: vec![
            OwnedSubtree::Move {
                from: sub(C),
                to: sub(FREE),
                host: (sub(TOP), b"a".to_vec(), 0),
                read: read(b"f"),
                below: Vec::new(),
            },
            OwnedSubtree::Move {
                from: sub(A),
                to: sub(B),
                host: (sub(TOP), b"f".to_vec(), 0),
                read: read(b"a"),
                below: vec![(sub(A_IN), sub(B_IN))],
            },
        ],
        ..OwnedBatch::default()
    };
    let mut kept = good.clone();
    kept.merks[0].hosted.remove(&b"a"[..]);
    let refused = world
        .apply(&tx, &kept)
        .expect_err("the node keeps its pointer");
    let subtree = sub(C);
    assert_eq!(batch_error(Err(refused)), BatchError::MoveHost { subtree });
    world.check(&tx);
    world.apply(&tx, &good).expect("the two change places");
    world.check(&tx);
    let places = |tx: &Tx<'_>| {
        let hosts = [FREE, B].map(|name| shown(tx, name).expect("a subtree").2);
        assert_eq!(hosts, [host(TOP, b"a"), host(TOP, b"f")]);
        assert_eq!([shown(tx, C), shown(tx, A)], [None, None]);
        assert_eq!(
            get(tx, data(FREE), b"d").expect("a read"),
            Some(b"flat".to_vec())
        );
        assert_eq!(get(tx, aux(B_IN), b"ia").expect("a read"), Some(vec![1; 9]));
    };
    places(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!([pointers(&world, b"a"), pointers(&world, b"f")], [0, 1]);
    let reader = world.begin();
    places(&reader);
    assert_eq!(db.private_ids(), 0);
}

/// And what the transaction dropped before a move did take is not read:
/// it drops the Merk above a subtree, enters both again, and moves the
/// new subtree. Under the new prefix it reads what it wrote into the new
/// one, and nothing of what the committed state has under the prefix the
/// new one was taken from, which is of the subtree that was dropped.
#[test]
fn a_subtree_that_is_entered_again_and_moved_holds_nothing_of_the_one_before() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    world.write(&tx, vec![put(aux(A), b"old", b"of the one before")]);
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    let dropped = subtrees(vec![OwnedSubtree::Drop(sub(TOP))]);
    world.apply(&tx, &dropped).expect("a drop");
    let top = world.merk(&tx, TOP, |items| {
        items.insert(b"x".to_vec(), vec![9; 30]);
    });
    let made = OwnedBatch {
        merks: vec![top],
        subtrees: vec![enter(TOP)],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &made).expect("the top is made again");
    let again = nested(&world, &tx, b"a", A, A_IN, 90);
    world
        .apply(&tx, &again)
        .expect("the subtrees are made again");
    assert_eq!(get(&tx, aux(A), b"old").expect("a read"), None);
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the new subtree is moved");
    world.check(&tx);
    let moved = |tx: &Tx<'_>| {
        assert_eq!(get(tx, aux(B), b"old").expect("a read"), None);
        assert_eq!(get(tx, aux(B), b"a").expect("a read"), Some(vec![90; 3]));
        let mut cursor = tx.txn.cursor(aux(B));
        cursor.seek_to_first().expect("a move");
        assert_eq!(cursor.key(), Some(&b"a"[..]));
        cursor.next().expect("a step");
        assert!(!cursor.valid());
    };
    moved(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    moved(&reader);
}

/// A clear of a Merk that has nodes takes them and what is below the
/// Merk, and leaves the subtree its flat records in the other columns
/// (`tree-layout.md` D6).
/// A subtree that the transaction cleared and moved, in either order,
/// has those records under the new prefix, the committed ones among
/// them, by key and by cursor, and no node. So has the next reader, and
/// so has a subtree that was cleared, given a node, and cleared again
/// before it was moved.
#[test]
fn a_subtree_that_is_cleared_and_moved_keeps_its_flat_records() {
    for (clear_first, twice) in [(true, false), (false, false), (true, true)] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        let tx = world.begin();
        // A clear, with the node that hosts the cleared Merk written
        // again, as its writer writes it.
        let cleared = |key: &[u8], name: u8| {
            let mut top = world.merk(&tx, TOP, |items| {
                items.get_mut(key).expect("the host").push(0);
            });
            top.hosted.insert(key.to_vec(), vec![sub(name)]);
            OwnedBatch {
                merks: vec![top],
                subtrees: vec![OwnedSubtree::Clear(sub(name))],
                ..OwnedBatch::default()
            }
        };
        // The clear takes the subtree below: the move that comes after
        // it has none to name.
        let pairs: &[(u8, u8)] = match clear_first {
            true => &[(A, B)],
            false => &[(A, B), (A_IN, B_IN)],
        };
        if clear_first {
            world.apply(&tx, &cleared(b"a", A)).expect("a clear");
        }
        if twice {
            let node = edit_at(&world, &tx, &[(A, b"a")], |items| {
                items.insert(b"n".to_vec(), b"a node again".to_vec());
            });
            world.apply(&tx, &node).expect("a node");
            world.apply(&tx, &cleared(b"a", A)).expect("a second clear");
        }
        let batch = rename(&world, &tx, b"a", b"b", pairs);
        world.apply(&tx, &batch).expect("the subtree is moved");
        if !clear_first {
            world.apply(&tx, &cleared(b"b", B)).expect("a clear");
        }
        world.check(&tx);
        let kept = |tx: &Tx<'_>| {
            let flat = [
                (aux(B), &b"a"[..], vec![1; 3]),
                (model::unit(Column::Meta, B), b"m", vec![1; 5]),
                (model::unit(Column::Roots, B), b"r", vec![1; 7]),
            ];
            for (unit, key, value) in flat {
                assert_eq!(get(tx, unit, key).expect("a read"), Some(value), "{unit}");
                let mut cursor = tx.txn.cursor(unit);
                cursor.seek_to_first().expect("a move");
                assert_eq!(cursor.key(), Some(key), "{unit}");
            }
            let node = tx.txn.node(sub(B), b"k1", |view| view.is_some());
            assert!(!node.expect("a read"), "the nodes went with the clear");
            let mut cursor = tx.txn.merk_cursor(sub(B)).expect("a cursor");
            cursor.seek_to_first().expect("a move");
            assert!(!cursor.valid());
            drop(cursor);
            let (root, ..) = shown(tx, B).expect("the subtree is entered");
            assert_eq!(root, None);
            for gone in [A, A_IN, B_IN] {
                assert_eq!(shown(tx, gone), None, "{gone}");
            }
            assert_eq!(get(tx, aux(B_IN), b"ia").expect("a read"), None);
        };
        kept(&tx);
        assert!(committed(world.commit(tx, &[])));
        world.check_committed();
        let reader = world.begin();
        kept(&reader);
        assert_eq!(db.private_ids(), 0);
    }
}

/// A cursor reads one version. Whether a move decides what is under its
/// subtree is asked of that version, by the seek and by the read of the
/// node that the seek found alike. A subtree that a later commit entered
/// below a prefix the move names was not below it for the cursor; by key,
/// at the latest version, it is.
#[test]
fn a_cursor_reads_the_node_it_found_by_the_version_it_reads() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    more(&mut world);
    let mover = world.begin();
    let batch = rename(&world, &mover, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&mover, &batch).expect("the subtree is moved");
    let mut cursor = mover.txn.merk_cursor(sub(EMPTY_2)).expect("a cursor");

    // A commit drops the Merk the cursor is over, and enters one of its
    // prefix below a subtree it enters where the moved one went.
    let other = world.begin();
    let mut nodes = world.merk(&other, B, |items| {
        items.insert(b"z".to_vec(), b"a tree".to_vec());
    });
    nodes.hosted.insert(b"z".to_vec(), vec![sub(EMPTY_2)]);
    let entered = OwnedBatch {
        merks: vec![nodes],
        records: vec![put(aux(B), b"f", b"foreign")],
        subtrees: vec![
            OwnedSubtree::Drop(sub(EMPTY_2)),
            enter(B),
            hosted(EMPTY_2, B, b"z"),
        ],
    };
    world.apply(&other, &entered).expect("a subtree is entered");
    assert!(committed(world.commit(other, &[&mover])));

    cursor.seek(b"n").expect("a seek");
    assert_eq!(cursor.key(), Some(&b"n"[..]));
    assert_eq!(mover.txn.stats().overlapped, 1);
    let value = cursor.node(|view| view.map(|view| value_of(&view)));
    assert_eq!(value.expect("the node is read"), Some(b"v".to_vec()));
    drop(cursor);
    let refused = get(&mover, aux(EMPTY_2), b"x").expect_err("a refusal");
    assert!(moved_by_key(&refused, EMPTY_2), "{refused}");
    assert_eq!(world.commit(mover, &[]), Outcome::Conflict);
    world.check_committed();
}

/// Every record of a cursor, from the first.
fn records(cursor: &mut crate::txn::Cursor<'_, '_>) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut records = Vec::new();
    cursor.seek_to_first().expect("a seek");
    while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
        let mut value = Vec::new();
        assert!(cursor.value(&mut value).expect("a value"));
        records.push((key, value));
        cursor.next().expect("a step");
    }
    records
}

/// A cursor reads the committed records of the version it was opened on
/// (RD6), and the moves of a transaction are laid out over the latest
/// version. So a cursor from before a commit reads on until the
/// transaction moves a subtree, and is refused from then on where a move
/// decides what is under its subtree (MV7): by a seek, by a step, and by
/// the read of the record or the node it stands on. The refusal is no
/// conflict, and it is of that cursor alone. A cursor over a subtree that
/// no move decides reads on, of its version. A cursor that is opened
/// after the commit reads what the transaction reads by key, until the
/// next commit is published. The commit between is a clear of the
/// subtree, or a drop of it and an enter of another under its prefix.
#[test]
fn a_cursor_from_before_a_commit_is_refused_where_a_move_decides() {
    for dropped in [false, true] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        let tx = world.begin();
        let mut to = tx.txn.cursor(aux(B));
        let mut from = tx.txn.cursor(aux(A));
        let mut stands = tx.txn.cursor(aux(A));
        stands.seek(b"a").expect("a seek");
        let mut nodes = tx.txn.merk_cursor(sub(A)).expect("a cursor");
        nodes.seek(b"k1").expect("a seek");
        let mut top = tx.txn.cursor(aux(TOP));

        let other = world.begin();
        if dropped {
            let top = world.merk(&other, TOP, |items| {
                items.remove(&b"a"[..]);
            });
            let gone = OwnedBatch {
                merks: vec![top],
                subtrees: vec![OwnedSubtree::Drop(sub(A))],
                ..OwnedBatch::default()
            };
            world.apply(&other, &gone).expect("a drop");
            let again = nested(&world, &other, b"a", A, A_IN, 90);
            world.apply(&other, &again).expect("another subtree there");
        } else {
            let mut top = world.merk(&other, TOP, |items| {
                items.get_mut(&b"a"[..]).expect("the host").push(0);
            });
            top.hosted.insert(b"a".to_vec(), vec![sub(A)]);
            let cleared = OwnedBatch {
                merks: vec![top],
                subtrees: vec![OwnedSubtree::Clear(sub(A))],
                ..OwnedBatch::default()
            };
            world.apply(&other, &cleared).expect("a clear");
        }
        assert!(committed(world.commit(other, &[&tx])));
        // Before the move, each cursor reads its version.
        let was = vec![(b"a".to_vec(), vec![1; 3])];
        assert_eq!(records(&mut from), was, "dropped {dropped}");
        assert!(records(&mut to).is_empty());
        assert_eq!(node_value(&mut nodes), Some(vec![1; 40]));

        // The clear took the subtree below: the move has none to name.
        let pairs: &[(u8, u8)] = match dropped {
            true => &[(A, B), (A_IN, B_IN)],
            false => &[(A, B)],
        };
        let batch = rename(&world, &tx, b"a", b"b", pairs);
        world.apply(&tx, &batch).expect("the subtree is moved");
        world.check(&tx);
        let refused = to.seek_to_first().expect_err("a refusal");
        assert!(another_version(&refused, B), "{refused}");
        let refused = from.seek_to_first().expect_err("a refusal");
        assert!(another_version(&refused, A), "{refused}");
        let mut out = Vec::new();
        let refused = stands.value(&mut out).expect_err("a refusal");
        assert!(another_version(&refused, A), "{refused}");
        let refused = stands.next().expect_err("a refusal");
        assert!(another_version(&refused, A), "{refused}");
        let refused = nodes.node(|view| view.is_some()).expect_err("a refusal");
        assert!(another_version(&refused, A), "{refused}");
        let refused = nodes.seek(b"k1").expect_err("a refusal");
        assert!(another_version(&refused, A), "{refused}");

        let top_was = vec![(b"t".to_vec(), b"top".to_vec())];
        assert_eq!(records(&mut top), top_was, "dropped {dropped}");
        let latest = match dropped {
            true => vec![90; 3],
            false => vec![1; 3],
        };
        let now_reads = vec![(b"a".to_vec(), latest.clone())];
        let mut now = tx.txn.cursor(aux(B));
        assert_eq!(records(&mut now), now_reads, "dropped {dropped}");
        let mut source = tx.txn.cursor(aux(A));
        assert!(records(&mut source).is_empty());
        assert_eq!(get(&tx, aux(B), b"a").expect("a read"), Some(latest));

        // The next commit, whichever it is, does the same to the cursor
        // that was opened after the move.
        let beside = OwnedBatch {
            records: vec![put(aux(TOP), b"u", b"beside")],
            ..OwnedBatch::default()
        };
        assert!(committed(world.write_outside(&beside, &[&tx])));
        let refused = now.seek_to_first().expect_err("a refusal");
        assert!(another_version(&refused, B), "{refused}");
        assert_eq!(records(&mut top), top_was, "dropped {dropped}");
        let mut newer = tx.txn.cursor(aux(B));
        assert_eq!(records(&mut newer), now_reads, "dropped {dropped}");
        assert_eq!(tx.txn.stats().overlapped, 0);
        drop((to, from, stands, nodes, top, now, source, newer));
        assert!(committed(world.commit(tx, &[])));
        world.check_committed();
    }
}

/// One history that such a cursor would have to answer for. A cursor is
/// opened over a subtree, and one over a prefix with none. A commit drops
/// the subtree and enters another under its prefix, below another Merk.
/// The transaction clears the Merk that hosted the first: its cursor
/// reads nothing more of that one (CV9). Then it moves the other subtree
/// to the second prefix. Both cursors are refused, and a cursor that is
/// opened now reads there the records of the subtree that was moved.
#[test]
fn a_cursor_from_before_a_commit_is_refused_after_a_clear_above_its_subtree_and_a_move() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    alone(&mut world);
    let tx = world.begin();
    let mut from = tx.txn.cursor(aux(A));
    let mut to = tx.txn.cursor(aux(B));

    let other = world.begin();
    let top = world.merk(&other, TOP, |items| {
        items.remove(&b"a"[..]);
    });
    let gone = OwnedBatch {
        merks: vec![top],
        subtrees: vec![OwnedSubtree::Drop(sub(A))],
        ..OwnedBatch::default()
    };
    world.apply(&other, &gone).expect("a drop");
    let mut root = world.merk(&other, ROOT, |items| {
        items.get_mut(&b"x"[..]).expect("the host").push(1);
    });
    root.hosted.insert(b"x".to_vec(), vec![sub(A)]);
    let fresh = world.merk(&other, A, |items| {
        items.insert(b"k1".to_vec(), vec![70; 40]);
    });
    let again = OwnedBatch {
        merks: vec![fresh, root],
        records: vec![put(aux(A), b"a", &[70; 3])],
        subtrees: vec![hosted(A, ROOT, b"x")],
    };
    world
        .apply(&other, &again)
        .expect("another subtree, elsewhere");
    assert!(committed(world.commit(other, &[&tx])));
    assert_eq!(records(&mut from), vec![(b"a".to_vec(), vec![1; 3])]);

    let cleared = subtrees(vec![OwnedSubtree::Clear(sub(TOP))]);
    world.apply(&tx, &cleared).expect("a clear");
    world.check(&tx);
    assert!(records(&mut from).is_empty(), "below the cleared Merk");
    assert!(records(&mut to).is_empty());

    let mut root = world.merk(&tx, ROOT, |items| {
        let value = items.remove(&b"x"[..]).expect("the host");
        items.insert(b"z".to_vec(), value);
    });
    root.hosted.insert(b"z".to_vec(), vec![sub(B)]);
    let read = (tx.txn.node(sub(ROOT), b"x", handle_of)).expect("the host is read");
    let moved = OwnedBatch {
        merks: vec![root],
        subtrees: vec![OwnedSubtree::Move {
            from: sub(A),
            to: sub(B),
            host: (sub(ROOT), b"z".to_vec(), 0),
            read,
            below: Vec::new(),
        }],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &moved).expect("the subtree is moved");
    world.check(&tx);
    let refused = to.seek_to_first().expect_err("a refusal");
    assert!(another_version(&refused, B), "{refused}");
    let refused = from.seek_to_first().expect_err("a refusal");
    assert!(another_version(&refused, A), "{refused}");
    let mut now = tx.txn.cursor(aux(B));
    assert_eq!(records(&mut now), vec![(b"a".to_vec(), vec![70; 3])]);
    assert_eq!(tx.txn.stats().overlapped, 0);
    drop((from, to, now));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// Whose commit it is makes no difference. A cursor is kept across the
/// commit of its own transaction, and the transaction, used again, moves
/// a subtree to the cursor's prefix: the cursor is refused, and one that
/// is opened then reads the subtree there.
#[test]
fn a_cursor_kept_across_the_commit_of_its_own_transaction_is_refused_where_a_move_decides() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let mut cursor = tx.txn.cursor(aux(B));
    world.write(&tx, vec![put(aux(TOP), b"u", b"own")]);
    let outcome = world.db.commit(world.versions, &tx.txn).expect("a commit");
    assert!(committed(outcome));
    let writes: Vec<Write> = (tx.model.borrow().writes.iter())
        .map(|(write, ..)| write.clone())
        .collect();
    world.model.commit(&writes);
    tx.forget();
    assert!(records(&mut cursor).is_empty());

    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    let refused = cursor.seek_to_first().expect_err("a refusal");
    assert!(another_version(&refused, B), "{refused}");
    let mut now = tx.txn.cursor(aux(B));
    assert_eq!(records(&mut now), vec![(b"a".to_vec(), vec![1; 3])]);
    drop((cursor, now));
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// Another: two cursors of one version over a prefix that a pair of the
/// move names, where the pair's subtree is not there when the move is
/// stated. A later commit enters it below the moved subtree, and the
/// move, made again, takes it to the prefix. Both cursors are refused,
/// before that commit and after it; a cursor that is opened after it
/// reads the subtree there.
#[test]
fn two_cursors_of_one_version_are_refused_alike_under_a_prefix_a_pair_names() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let mut first = tx.txn.cursor(aux(B_IN));
    let mut second = tx.txn.cursor(aux(B_IN));

    let other = world.begin();
    let mut gone = edit_at(&world, &other, &[(A, b"a")], |items| {
        items.remove(&b"in"[..]);
    });
    gone.subtrees = vec![OwnedSubtree::Drop(sub(A_IN))];
    world.apply(&other, &gone).expect("a drop");
    assert!(committed(world.commit(other, &[&tx])));

    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    assert_eq!(shown(&tx, B_IN), None);
    let refused = first.seek_to_first().expect_err("a refusal");
    assert!(another_version(&refused, B_IN), "{refused}");

    let other = world.begin();
    let entered = subtrees(vec![hosted(A_IN, A, b"k1")]);
    world.apply(&other, &entered).expect("a subtree is entered");
    assert!(committed(world.commit(other, &[&tx])));
    for cursor in [&mut first, &mut second] {
        let refused = cursor.seek_to_first().expect_err("a refusal");
        assert!(another_version(&refused, B_IN), "{refused}");
    }
    assert!(shown(&tx, B_IN).is_some(), "the pair takes the subtree");
    let mut now = tx.txn.cursor(aux(B_IN));
    assert!(records(&mut now).is_empty());
    world.check(&tx);
    assert_eq!(tx.txn.stats().overlapped, 0);
    drop((first, second, now));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// No record is read under a prefix without the entry of a subtree there
/// (CV9), and a subtree that the transaction entered itself holds what
/// the transaction wrote under it and nothing committed. A subtree below
/// one that was dropped leaves its records to the reclaimer. The
/// transaction enters a subtree under that prefix, writes a record there
/// and moves the subtree. Under the new prefix a cursor reads that record
/// and none of those that are left under the old one.
#[test]
fn a_cursor_reads_nothing_committed_under_a_subtree_the_transaction_entered_and_moved() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let top = world.merk(&tx, TOP, |items| {
        items.remove(&b"a"[..]);
    });
    let gone = OwnedBatch {
        merks: vec![top],
        subtrees: vec![OwnedSubtree::Drop(sub(A))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &gone).expect("a drop");
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    let mut before = tx.txn.cursor(aux(FREE));
    let entered = OwnedBatch {
        records: vec![put(aux(A_IN), b"n", b"new")],
        subtrees: vec![hosted(A_IN, TOP, b"x")],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &entered).expect("a subtree is entered");
    // The element hosts no Merk, and neither does the one it becomes.
    let mut batch = rename(&world, &tx, b"x", b"z", &[(A_IN, FREE)]);
    batch.merks[0].hosted.remove(&b"z"[..]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    world.check(&tx);
    let own = vec![(b"n".to_vec(), b"new".to_vec())];
    assert_eq!(records(&mut before), own);
    let mut after = tx.txn.cursor(aux(FREE));
    assert_eq!(records(&mut after), own);
    assert_eq!(get(&tx, aux(FREE), b"ia").expect("a read"), None);
    drop((before, after));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// The value of the node under the key a cursor stands on.
fn node_value(cursor: &mut crate::txn::MerkCursor<'_, '_>) -> Option<Vec<u8>> {
    let value = cursor.node(|view| view.map(|view| value_of(&view)));
    value.expect("the node is read")
}

/// The read of the node under a cursor's key is one operation of the
/// transaction: what the transaction holds is looked at once. A rollback
/// by another thread comes before that or after it. Here it comes at the
/// moment the read takes the transaction's lock, after the seek that
/// found the key under a prefix a subtree was moved to. The read shows
/// what the transaction has under the key then, which is no node: not
/// its own node, which the rollback took, and not the node that the
/// cursor's version has under the prefix the subtree was moved from.
#[test]
fn a_cursor_reads_a_node_as_the_transaction_has_it_at_one_moment() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let own = edit_at(&world, &tx, &[(A, b"a")], |items| {
        items.insert(b"k1".to_vec(), b"own".to_vec());
    });
    world.apply(&tx, &own).expect("a node of its own");
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    let mut cursor = tx.txn.merk_cursor(sub(B)).expect("a cursor");
    cursor.seek(b"k1").expect("a seek");
    assert_eq!(cursor.key(), Some(&b"k1"[..]));
    assert_eq!(node_value(&mut cursor), Some(b"own".to_vec()));

    tx.txn.roll_back_before_cursor_read();
    assert_eq!(node_value(&mut cursor), None);
    drop(cursor);
    tx.forget();
    world.check(&tx);
    assert_eq!(tx.txn.stats().writes, 0);
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
    world.check_committed();
}

/// The read of the record under a cursor's key is one look at what the
/// transaction holds as well. The rollback comes at the moment the read
/// takes the transaction's lock, after the seek that found the key under
/// a prefix a subtree was moved to. The read finds no record: not the
/// transaction's own, which the rollback took, and not the record that
/// the cursor's version has under the prefix the subtree was moved from.
#[test]
fn a_cursor_reads_a_record_as_the_transaction_has_it_at_one_moment() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    world.write(&tx, vec![put(aux(A), b"a", b"own")]);
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    let mut cursor = tx.txn.cursor(aux(B));
    cursor.seek(b"a").expect("a seek");
    assert_eq!(cursor.key(), Some(&b"a"[..]));
    let mut out = Vec::new();
    assert!(cursor.value(&mut out).expect("a value"));
    assert_eq!(out, b"own");

    tx.txn.roll_back_before_cursor_read();
    out.clear();
    assert!(!cursor.value(&mut out).expect("a value"));
    assert!(out.is_empty());
    drop(cursor);
    tx.forget();
    world.check(&tx);
    assert_eq!(tx.txn.stats().writes, 0);
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
    world.check_committed();
}

/// A cursor's read of a node that fails after the transaction began to
/// watch the commits for it gives the watch up where nothing else needs
/// it. Two subtrees change places, a cursor stands on a node under one of
/// the prefixes, and the transaction is rolled back. The next read of
/// that node is of the cursor's version under the cursor's own prefix
/// again. The change of prefix fails, and the read with it: the
/// transaction, which holds nothing, watches no commit. The read after
/// that is served.
#[test]
fn a_cursor_whose_read_of_a_node_fails_leaves_no_watch() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let second = nested(&world, &tx, b"b", B, B_IN, 50);
    world.apply(&tx, &second).expect("a second subtree");
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    world
        .apply(&tx, &swap(&world, &tx))
        .expect("the two change places");
    let mut cursor = tx.txn.merk_cursor(sub(B)).expect("a cursor");
    cursor.seek(b"k1").expect("a seek");
    assert_eq!(node_value(&mut cursor), Some(vec![1; 40]));
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    assert!(!tx.txn.watches());

    cursor.refuse_place();
    let refused = cursor.node(|view| view.is_some()).expect_err("no read");
    assert!(matches!(refused, TxnError::Damaged(_)), "{refused}");
    assert!(!tx.txn.watches(), "no node was shown");
    assert_eq!(node_value(&mut cursor), Some(vec![50; 40]));
    drop(cursor);
    world.check(&tx);
    assert_eq!(world.commit(tx, &[]), Outcome::Empty);
    world.check_committed();
}

/// The read of a node that a cursor makes of its own version is under
/// way from the moment the transaction hands it back until the cursor
/// says what it read, and the transaction's lock is not held meanwhile.
/// For that long the watch on the commits stays, and the key sets that
/// the transaction takes in are kept, as many as it keeps for a lease,
/// whatever another thread does with the transaction. Two cursors stand
/// on a node, and the transaction, which holds nothing, watches no
/// commit. While the read of the first is under way, another thread uses
/// the transaction and commits are published. In one order the read of
/// the second cursor fails and two commits follow; in another a commit
/// comes first and the read of the second cursor, which takes it in,
/// fails; in a third the transaction takes a commit in by a read and is
/// rolled back; in a fourth it takes in as many commits as it keeps key
/// sets of. A write then keeps the node's value by the handle that the
/// first read gave: the transaction knows what the commits wrote, and
/// commits. With one commit more than it keeps key sets of, it cannot
/// say what the first of them wrote: the keep is taken to be met, and
/// its commit is a conflict.
#[test]
fn a_read_of_a_node_that_is_under_way_keeps_the_watch_on_the_commits() {
    use crate::txn::reads::KEPT_COMMITS;

    let orders = [
        "two commits after the other read fails",
        "a commit before the other read fails",
        "a commit that is taken in, and a rollback",
        "as many commits as key sets are kept of",
        "one commit more than key sets are kept of",
    ];
    for order in orders {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        let tx = world.begin();
        let second = nested(&world, &tx, b"b", B, B_IN, 50);
        world.apply(&tx, &second).expect("a second subtree");
        assert!(committed(world.commit(tx, &[])));

        let tx = world.begin();
        // The write that is made at the end: it keeps the value of a node.
        let added = edit_at(&world, &tx, &[(B, b"b")], |items| {
            items.insert(b"k0".to_vec(), b"new".to_vec());
        });
        let kept = (added.merks[0].nodes.iter()).find(|(.., stated)| !stated);
        let (kept, ..) = kept.expect("a node whose value is kept").clone();
        world
            .apply(&tx, &swap(&world, &tx))
            .expect("the two change places");
        let mut first = tx.txn.merk_cursor(sub(B)).expect("a cursor");
        let mut other = tx.txn.merk_cursor(sub(B)).expect("a cursor");
        for cursor in [&mut first, &mut other] {
            cursor.seek(&kept).expect("a seek");
            assert_eq!(cursor.key(), Some(&kept[..]));
        }
        tx.txn.rollback().expect("no scope is open");
        tx.forget();
        assert!(!tx.txn.watches());

        other.refuse_place();
        let txn = &tx.txn;
        let mut publish = |key: &[u8]| {
            let beside = OwnedBatch {
                records: vec![put(aux(TOP), key, b"beside")],
                ..OwnedBatch::default()
            };
            assert!(committed(world.write_outside(&beside, &[])));
        };
        // Another thread uses the transaction while the read is under
        // way: the reading thread waits for it.
        let taken_in = |key: &[u8]| {
            std::thread::scope(|threads| {
                let used = threads.spawn(|| {
                    let mut out = Vec::new();
                    assert!(txn.get(aux(TOP), key, &mut out).expect("a read"));
                    assert!(txn.watches(), "{order}: a read is under way");
                });
                used.join().expect("the other thread");
            });
        };
        let handle = first.node(|view| {
            match orders.iter().position(|other| *other == order) {
                Some(at @ 0..=1) => {
                    if at == 1 {
                        publish(b"u");
                    }
                    std::thread::scope(|threads| {
                        let used = threads.spawn(|| {
                            let refused = other.node(|view| view.is_some());
                            let refused = refused.expect_err("no read");
                            assert!(matches!(refused, TxnError::Damaged(_)), "{refused}");
                            assert!(txn.watches(), "{order}: a read is under way");
                        });
                        used.join().expect("the other thread");
                    });
                    if at == 0 {
                        publish(b"u");
                        publish(b"w");
                    }
                }
                Some(2) => {
                    publish(b"u");
                    taken_in(b"u");
                    std::thread::scope(|threads| {
                        let used = threads.spawn(|| {
                            txn.rollback().expect("no scope is open");
                            assert!(txn.watches(), "{order}: a read is under way");
                        });
                        used.join().expect("the other thread");
                    });
                }
                at => {
                    for commit in 0..KEPT_COMMITS + usize::from(at == Some(4)) {
                        let key = [b'u', commit as u8];
                        publish(&key);
                        taken_in(&key);
                    }
                }
            }
            handle_of(view)
        });
        let handle = handle.expect("the node is read");
        assert!(handle.is_some());
        drop((first, other));
        let batch = added.reading(|subtree, key| match subtree == sub(B) && key == &kept[..] {
            true => handle,
            false => (tx.txn.node(subtree, key, handle_of)).expect("a read"),
        });
        world.apply_as_read(&tx, &batch).expect("the value is kept");
        let met = usize::from(order == orders[4]);
        assert_eq!(tx.txn.stats().overlapped, met, "{order}");
        if met == 0 {
            world.check(&tx);
            assert!(committed(world.commit(tx, &[])), "{order}");
        } else {
            let ended = world.db.commit(world.versions, &tx.txn).expect("a commit");
            assert_eq!(ended, Outcome::Conflict, "{order}");
        }
        world.check_committed();
    }
}

/// A read that is under way answers for no keep before it has shown a
/// node, and one that shows none answers for none. A cursor shows a
/// committed node, whose handle is kept, and comes to stand on a node of
/// the transaction's own. The transaction is rolled back, and the handle
/// ends with it. The cursor then reads the node it stands on, which is
/// none, and while that read is under way another thread writes the
/// first node with its value kept by the handle from before the
/// rollback. The keep is taken to be met (CF2a), and the commit is a
/// conflict.
#[test]
fn a_read_under_way_answers_for_no_handle_from_before_a_rollback() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    alone(&mut world);
    let tx = world.begin();
    let mut cursor = tx.txn.merk_cursor(sub(ROOT)).expect("a cursor");
    cursor.seek(b"x").expect("a seek");
    let before = cursor.node(handle_of).expect("the node is read");
    assert!(before.is_some());
    world.edit(&tx, ROOT, |items| {
        items.insert(b"y".to_vec(), b"its own".to_vec());
    });
    cursor.seek(b"y").expect("a seek");
    assert_eq!(cursor.key(), Some(&b"y"[..]));
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    assert!(!tx.txn.watches());

    // A change of the Merk that keeps the value of the committed node.
    let keeps = (b'a'..=b'w').find_map(|key| {
        let merk = world.merk(&tx, ROOT, |items| {
            items.insert(vec![key], b"new".to_vec());
        });
        let kept = |(key, _, stated): &(Vec<u8>, _, bool)| key == b"x" && !stated;
        merk.nodes.iter().any(kept).then_some(merk)
    });
    let mut merk = keeps.expect("a change that keeps the node");
    merk.hints.insert(b"x".to_vec(), before.expect("a handle"));
    let batch = OwnedBatch {
        records: World::root_record(&merk).into_iter().collect(),
        merks: vec![merk],
        ..OwnedBatch::default()
    };
    let txn = &tx.txn;
    let shown = cursor.node(|view| {
        assert!(view.is_none(), "the transaction's own node is gone");
        std::thread::scope(|threads| {
            let used = threads.spawn(|| {
                let scope = txn.scope().expect("a scope");
                (batch.with(|batch| scope.apply(batch))).expect("the batch is taken");
                scope.merge();
                assert_eq!(txn.stats().overlapped, 1, "the keep is met");
            });
            used.join().expect("the other thread");
        });
    });
    shown.expect("the read ends");
    drop(cursor);
    assert_eq!(tx.txn.stats().overlapped, 1);
    let ended = world.db.commit(world.versions, &tx.txn).expect("a commit");
    assert_eq!(ended, Outcome::Conflict);
    world.check_committed();
}

/// A node that a cursor shows is the caller's from the moment it is
/// given it: another thread can keep its value by the handle before the
/// read has ended. A cursor stands on a committed node and the
/// transaction is rolled back, which ends every handle. The cursor reads
/// the node again, and while the caller has it, another thread writes
/// the Merk with that node's value kept by the handle it was just shown.
/// No commit was published since: the keep is answered for, and the
/// transaction commits.
#[test]
fn a_node_that_a_cursor_shows_is_kept_by_its_handle_before_the_read_has_ended() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    alone(&mut world);
    let tx = world.begin();
    let mut cursor = tx.txn.merk_cursor(sub(ROOT)).expect("a cursor");
    cursor.seek(b"x").expect("a seek");
    assert!(cursor
        .node(|view| view.is_some())
        .expect("the node is read"));
    tx.txn.rollback().expect("no scope is open");
    assert!(!tx.txn.watches());

    // A change of the Merk that keeps the value of the committed node.
    let keeps = (b'a'..=b'w').find_map(|key| {
        let merk = world.merk(&tx, ROOT, |items| {
            items.insert(vec![key], b"new".to_vec());
        });
        let kept = |(key, _, stated): &(Vec<u8>, _, bool)| key == b"x" && !stated;
        merk.nodes.iter().any(kept).then_some(merk)
    });
    let merk = keeps.expect("a change that keeps the node");
    let txn = &tx.txn;
    let shown = cursor.node(|view| {
        let mut merk = merk.clone();
        (merk.hints).insert(b"x".to_vec(), handle_of(view).expect("the node is shown"));
        let batch = OwnedBatch {
            records: World::root_record(&merk).into_iter().collect(),
            merks: vec![merk],
            ..OwnedBatch::default()
        };
        std::thread::scope(|threads| {
            let used = threads.spawn(|| {
                let scope = txn.scope().expect("a scope");
                (batch.with(|batch| scope.apply(batch))).expect("the batch is taken");
                scope.merge();
                assert_eq!(txn.stats().overlapped, 0, "the keep is answered for");
            });
            used.join().expect("the other thread");
        });
        batch
    });
    let batch = shown.expect("the read ends");
    drop(cursor);
    assert_eq!(tx.txn.stats().overlapped, 0);
    world.note(&tx, &batch, world.latest().get());
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// What a commit below a subtree that a transaction cleared meets goes by
/// the entries that the directory has stored (CF5), and the model's
/// answer with it. A Merk loses its one node and is cleared while it has
/// none, which leaves the subtree below it entered. A commit that drops
/// that subtree by name, and writes nothing else, takes its entry away:
/// nothing leads from it to the cleared Merk, and the clear is not met.
/// A commit that writes a record under that subtree leaves the entry,
/// and meets the clear.
#[test]
fn a_commit_below_a_cleared_merk_meets_the_clear_by_the_entries_that_are_stored() {
    for unlinks in [true, false] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        let tx = world.begin();
        let mut top = world.merk(&tx, TOP, |items| {
            items.insert(b"f".to_vec(), b"a tree".to_vec());
        });
        top.hosted.insert(b"f".to_vec(), vec![sub(C)]);
        let mut outer = world.merk(&tx, C, |items| {
            items.insert(b"h".to_vec(), b"a tree with no node".to_vec());
        });
        outer.hosted.insert(b"h".to_vec(), vec![sub(C_IN)]);
        let made = OwnedBatch {
            merks: vec![outer, top],
            records: vec![put(aux(C_IN), b"k", b"kept")],
            subtrees: vec![hosted(C, TOP, b"f"), hosted(C_IN, C, b"h")],
        };
        world.apply(&tx, &made).expect("the subtrees are made");
        assert!(committed(world.commit(tx, &[])));

        let tx = world.begin();
        let removed = edit_at(&world, &tx, &[(C, b"f")], |items| {
            items.remove(&b"h"[..]);
        });
        world.apply(&tx, &removed).expect("the node is removed");
        let cleared = subtrees(vec![OwnedSubtree::Clear(sub(C))]);
        world.apply(&tx, &cleared).expect("a clear");
        assert!(shown(&tx, C_IN).is_some(), "the subtree below stays");
        world.check(&tx);

        let beside = match unlinks {
            true => subtrees(vec![OwnedSubtree::Drop(sub(C_IN))]),
            false => OwnedBatch {
                records: vec![put(aux(C_IN), b"z", b"beside")],
                ..OwnedBatch::default()
            },
        };
        assert!(committed(world.write_outside(&beside, &[&tx])));
        // The transaction takes the commit in at its next call.
        let read = get(&tx, aux(TOP), b"t").expect("a read");
        assert_eq!(read, Some(b"top".to_vec()));
        let met = usize::from(!unlinks);
        assert_eq!(tx.txn.stats().overlapped, met, "unlinks: {unlinks}");
        assert_eq!(tx.model.borrow().conflicts(), !unlinks);
        match unlinks {
            true => {
                world.check(&tx);
                assert!(committed(world.commit(tx, &[])));
            }
            false => assert_eq!(world.commit(tx, &[]), Outcome::Conflict),
        }
        world.check_committed();
    }
}

/// A node record that no link of a tree leads to: `n` tells one from
/// another.
fn loose(n: u8) -> model::NodeRec {
    model::NodeRec {
        value: vec![n; 40],
        feature: vec![3, 1],
        value_hash: [n; 32],
        kv_hash: [n + 1; 32],
        left: None,
        right: None,
    }
}

/// The batch that writes `nodes` into the Merk at the end of `path`,
/// which has, from `TOP` down, each Merk with the key of the node that
/// hosts it, and states the root that the Merk has: its tree reaches
/// none of them, and the transaction keeps each as an unlinked record
/// (`MerkWrite`, "Unlinked records"). `beside` has the keys that hold
/// such a record already, which are no nodes of the tree.
fn unlinked_in<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    path: &[(u8, &[u8])],
    nodes: Vec<(&[u8], model::NodeRec, bool)>,
    beside: &[&[u8]],
) -> OwnedBatch {
    let (name, _) = *path.last().expect("a Merk");
    let mut tree = (tx.model.borrow()).view(&world.model.records, Unit::data(sub(name)));
    tree.retain(|key, _| !beside.contains(&&key[..]));
    let items = model::Committed::items(&tree);
    let mut merk = OwnedMerk::change(sub(name), &tree, &items);
    assert!(merk.nodes.is_empty() && merk.removed.is_empty());
    merk.root = Some(model::records(&items).0);
    (merk.nodes)
        .extend((nodes.into_iter()).map(|(key, node, stated)| (key.to_vec(), node, stated)));
    let mut batch = edit_at(world, tx, path, |_| {});
    batch.merks[0] = merk;
    batch
}

/// Whether `tx` shows the node under `key` of the Merk `name` as an
/// unlinked record, as a node of the tree (`Some(false)`), or not at all.
fn unlinked(tx: &Tx<'_>, name: u8, key: &[u8]) -> Option<bool> {
    let shown = (tx.txn).node(sub(name), key, |view| {
        view.map(|view| {
            assert_eq!(view.handle().is_none(), view.is_unlinked());
            view.is_unlinked()
        })
    });
    shown.expect("the node is read")
}

/// Every key that a cursor of `tx` over the nodes of the Merk `name`
/// shows, in order, each with whether it is an unlinked record.
fn under_a_cursor(tx: &Tx<'_>, name: u8) -> Vec<(Vec<u8>, bool)> {
    let mut cursor = tx.txn.merk_cursor(sub(name)).expect("a cursor");
    let mut keys = Vec::new();
    cursor.seek_to_first().expect("a seek");
    while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
        let loose = cursor.node(|view| view.expect("a node").is_unlinked());
        keys.push((key, loose.expect("the node is read")));
        cursor.next().expect("a step");
    }
    keys
}

/// The keys of the flat records that `tx`, which has written no node
/// under the prefix `name`, reads in the data column there: those of the
/// unlinked records of a Merk.
fn beside_the_tree(tx: &Tx<'_>, name: u8) -> Vec<Vec<u8>> {
    let mut cursor = tx.txn.cursor(Unit::data(sub(name)));
    (records(&mut cursor).into_iter())
        .map(|(key, _)| key)
        .collect()
}

/// A move takes the unlinked records of a Merk with the Merk: they are
/// flat records of its data column (`MerkWrite`, "Unlinked records";
/// MV5). A Merk holds one that is committed, and the mover leaves a
/// second. After the move the transaction finds each under the new
/// prefix, by key and under a cursor over the nodes, among the nodes in
/// key order, and none under the old one, with frames that were made
/// again as well; and so does a transaction that reads the committed
/// state after the commit. The record that the mover left is its own
/// node still, under the new prefix: a later write of it keeps its
/// value. The committed one has no value for a write to keep.
#[test]
fn a_move_carries_the_unlinked_records_of_a_merk() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let batch = unlinked_in(
        &world,
        &tx,
        &[(A, b"a")],
        vec![(b"u1", loose(7), true)],
        &[],
    );
    world
        .apply(&tx, &batch)
        .expect("a record that no node links");
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    let below: &[(u8, &[u8])] = &[(A, b"a")];
    let batch = unlinked_in(&world, &tx, below, vec![(b"u2", loose(9), true)], &[b"u1"]);
    let applied = world.apply(&tx, &batch).expect("a second one");
    assert_eq!(applied.unlinked.len(), 1);
    assert_eq!(
        [b"u1", b"u2", b"k1"].map(|key| unlinked(&tx, A, key)),
        [Some(true), Some(true), Some(false)]
    );
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    let all: Vec<(Vec<u8>, bool)> = [(&b"in"[..], false), (b"k1", false), (b"k2", false)]
        .into_iter()
        .chain([(&b"u1"[..], true), (b"u2", true)])
        .map(|(key, loose)| (key.to_vec(), loose))
        .collect();
    let moved = |tx: &Tx<'_>| {
        assert_eq!(
            [b"u1", b"u2", b"k1"].map(|key| unlinked(tx, B, key)),
            [Some(true), Some(true), Some(false)]
        );
        assert_eq!([b"u1", b"u2"].map(|key| unlinked(tx, A, key)), [None, None]);
        assert_eq!(under_a_cursor(tx, B), all);
        assert!(under_a_cursor(tx, A).is_empty());
    };
    moved(&tx);
    world.check(&tx);
    // Frames that are made again over a commit make the move again.
    let rebuilds = tx.txn.stats().rebuilds;
    let beside = OwnedBatch {
        records: vec![put(aux(TOP), b"u", b"beside")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&beside, &[&tx])));
    moved(&tx);
    world.check(&tx);
    assert!(tx.txn.stats().rebuilds > rebuilds);

    // The committed record has no value to keep, and the mover's own has.
    let kept = |key: &'static [u8]| {
        let now = (tx.model.borrow()).view(&world.model.records, Unit::data(sub(B)));
        let Some(Val::Node(node)) = now.get(key) else {
            panic!("{key:?} is no node");
        };
        let node = model::NodeRec {
            kv_hash: [0x77; 32],
            ..node.clone()
        };
        let at: &[(u8, &[u8])] = &[(B, b"b")];
        unlinked_in(&world, &tx, at, vec![(key, node, false)], &[b"u1", b"u2"])
    };
    let refused = batch_error(world.apply(&tx, &kept(b"u1")));
    assert!(matches!(refused, BatchError::NoValue { .. }), "{refused}");
    world.apply(&tx, &kept(b"u2")).expect("its own node");
    moved(&tx);
    world.check(&tx);

    assert!(committed(world.commit(tx, &[])));
    let reader = world.begin();
    world.check(&reader);
    moved(&reader);
    assert_eq!(
        beside_the_tree(&reader, B),
        [b"u1".to_vec(), b"u2".to_vec()]
    );
    assert!(beside_the_tree(&reader, A).is_empty());
    let trees = db.check_trees(&versions, world.latest());
    let nodes: Vec<Vec<u8>> = [&b"in"[..], b"k1", b"k2"].map(<[u8]>::to_vec).into();
    assert_eq!(trees.get(&sub(B)), Some(&nodes), "the nodes of the tree");
    assert_eq!(trees.get(&sub(A)), None);
    assert_eq!(db.private_ids(), 0);
}

/// The unlinked record that a move brings under a prefix is the record
/// of the Merk that was moved, and not one that the transaction left
/// there before. A transaction leaves a node out of the tree of a Merk
/// and drops the subtree above it, and then moves another Merk to that
/// prefix, which holds a committed unlinked record under the same key.
/// The record it reads there is the committed one, which has no value
/// for a write to keep.
#[test]
fn a_record_that_a_move_brings_is_not_the_one_the_transaction_left_under_the_prefix() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let second = nested(&world, &tx, b"c", C, C_IN, 50);
    world.apply(&tx, &second).expect("a second subtree");
    let below_c: &[(u8, &[u8])] = &[(C, b"c"), (C_IN, b"in")];
    let batch = unlinked_in(&world, &tx, below_c, vec![(b"u1", loose(7), true)], &[]);
    world
        .apply(&tx, &batch)
        .expect("a record that no node links");
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    let below_a: &[(u8, &[u8])] = &[(A, b"a"), (A_IN, b"in")];
    let batch = unlinked_in(&world, &tx, below_a, vec![(b"u1", loose(9), true)], &[]);
    world
        .apply(&tx, &batch)
        .expect("the transaction's own record");
    assert_eq!(unlinked(&tx, A_IN, b"u1"), Some(true));
    let top = world.merk(&tx, TOP, |items| {
        items.remove(&b"a"[..]).expect("the host");
    });
    let dropped = OwnedBatch {
        merks: vec![top],
        subtrees: vec![OwnedSubtree::Drop(sub(A))],
        ..OwnedBatch::default()
    };
    world
        .apply(&tx, &dropped)
        .expect("the subtree above is dropped");
    assert_eq!(unlinked(&tx, A_IN, b"u1"), None);
    let batch = rename(&world, &tx, b"c", b"d", &[(C, FREE), (C_IN, A_IN)]);
    world.apply(&tx, &batch).expect("another Merk comes there");
    world.check(&tx);
    let brought = (tx.model.borrow()).view(&world.model.records, Unit::data(sub(A_IN)));
    let Some(Val::Node(node)) = brought.get(&b"u1"[..]) else {
        panic!("the record that the move brought");
    };
    assert_eq!(node.value, loose(7).value, "the committed record");
    let again = model::NodeRec {
        kv_hash: [0x77; 32],
        ..node.clone()
    };
    let moved: &[(u8, &[u8])] = &[(FREE, b"d"), (A_IN, b"in")];
    let kept = unlinked_in(&world, &tx, moved, vec![(b"u1", again, false)], &[b"u1"]);
    let refused = batch_error(world.apply(&tx, &kept));
    assert!(matches!(refused, BatchError::NoValue { .. }), "{refused}");
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    let reader = world.begin();
    world.check(&reader);
    assert_eq!(unlinked(&reader, A_IN, b"u1"), Some(true));
}

/// What a transaction knows of whether a Merk holds unlinked records goes
/// with the Merk where a move takes it: the moves of a batch are made
/// together. One of two Merks holds an unlinked record, and a
/// transaction that has asked of both has them change places. It finds
/// the record under the prefix the Merk went to and none under the other,
/// and asks the data column no more for either.
#[test]
fn two_merks_that_change_places_take_what_is_known_of_their_unlinked_records() {
    use crate::txn::unlinked::probe::lookups;

    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let second = nested(&world, &tx, b"b", B, B_IN, 50);
    world.apply(&tx, &second).expect("a second subtree");
    let batch = unlinked_in(
        &world,
        &tx,
        &[(A, b"a")],
        vec![(b"u1", loose(7), true)],
        &[],
    );
    world
        .apply(&tx, &batch)
        .expect("a record that no node links");
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    // A write gives the transaction frames, and a read of a key with no
    // node asks once of each Merk.
    world.write(&tx, vec![put(aux(TOP), b"w", b"frames")]);
    let asked = lookups();
    assert_eq!(
        [unlinked(&tx, A, b"none"), unlinked(&tx, B, b"none")],
        [None, None]
    );
    assert_eq!(lookups(), asked + 2);
    assert_eq!(
        [unlinked(&tx, A, b"u1"), unlinked(&tx, B, b"u1")],
        [Some(true), None]
    );
    assert_eq!(lookups(), asked + 2);

    world
        .apply(&tx, &swap(&world, &tx))
        .expect("the two change places");
    let asked = lookups();
    assert_eq!(
        [unlinked(&tx, A, b"u1"), unlinked(&tx, B, b"u1")],
        [None, Some(true)]
    );
    assert_eq!(
        [unlinked(&tx, A, b"none"), unlinked(&tx, B, b"none")],
        [None, None]
    );
    assert_eq!(lookups(), asked, "what was known went with each Merk");
    world.check(&tx);
    assert_eq!(beside_the_tree(&tx, B), [b"u1".to_vec()]);
    assert!(beside_the_tree(&tx, A).is_empty());
    assert!(committed(world.commit(tx, &[])));

    // A transaction that reads the committed state asks once of each.
    let reader = world.begin();
    let asked = lookups();
    assert_eq!(
        [unlinked(&reader, A, b"u1"), unlinked(&reader, B, b"u1")],
        [None, Some(true)]
    );
    assert_eq!(
        [unlinked(&reader, A, b"none"), unlinked(&reader, B, b"none")],
        [None, None]
    );
    assert_eq!(lookups(), asked + 2);
    world.check(&reader);
}

/// A subtree that a move of a batch names is not stated anew by that
/// batch, under the prefix it has or the one it gets: the batch is
/// refused whole (`ConvertMoved`), whichever of the two statements comes
/// first. A subtree that leaves with the move is not entered where the
/// batch states its subtrees anew, after its moves (MV4). A later batch
/// states the moved subtree anew under the prefix it then has, and
/// frames that are made again state it so. Where the transaction serves
/// its records by key, a subtree that a move decides is not stated anew
/// (MV7).
#[test]
fn a_subtree_is_stated_anew_after_its_move_and_not_in_the_batch_of_the_move() {
    const WIDE: &[u8] = b"a wide merk";
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    emptied(&mut world);
    let pairs = [(A, B), (A_IN, B_IN)];
    let convert = |name: u8| OwnedSubtree::Convert(sub(name), WIDE.to_vec(), None);
    let tree_type = |tx: &Tx<'_>, name: u8| shown(tx, name).map(|(_, tree_type, _)| tree_type);

    let tx = world.begin();
    for (named, first) in [A, B, A_IN, B_IN]
        .into_iter()
        .zip([false, true].into_iter().cycle())
    {
        let mut batch = rename(&world, &tx, b"a", b"b", &pairs);
        batch.subtrees.insert(usize::from(!first), convert(named));
        let refused = batch_error(world.apply(&tx, &batch));
        let stated = BatchError::ConvertMoved {
            subtree: sub(named),
        };
        assert_eq!(refused, stated, "{named}");
        world.check(&tx);
        assert_eq!(tx.txn.stats().writes, 0, "the batch changed nothing");
    }
    let mut batch = rename(&world, &tx, b"a", b"b", &pairs);
    batch.subtrees.push(convert(EMPTY));
    let refused = batch_error(world.apply(&tx, &batch));
    let left = BatchError::NotEntered {
        subtree: sub(EMPTY),
    };
    assert_eq!(refused, left, "the subtree that leaves with the move");
    world.check(&tx);
    assert_eq!(tx.txn.stats().writes, 0, "the batch changed nothing");
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    world.apply(&tx, &batch).expect("the subtree is moved");
    // Nothing is entered under the prefix it had.
    let old = world.apply(&tx, &subtrees(vec![convert(A)]));
    let old = batch_error(old);
    assert!(matches!(old, BatchError::NotEntered { .. }), "{old}");
    world
        .apply(&tx, &subtrees(vec![convert(B)]))
        .expect("the moved subtree is stated anew");
    assert_eq!(tree_type(&tx, B), Some(WIDE.to_vec()));
    assert_eq!(tree_type(&tx, A), None);
    world.check(&tx);
    let beside = OwnedBatch {
        records: vec![put(aux(TOP), b"u", b"beside")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&beside, &[&tx])));
    assert_eq!(tree_type(&tx, B), Some(WIDE.to_vec()));
    world.check(&tx);

    // A commit meets the move: by key, the moved subtree is not stated
    // anew, and the refusal is the conflict that names it.
    let other = world.begin();
    world.write(&other, vec![put(aux(A), b"f", b"foreign")]);
    assert!(committed(world.commit(other, &[&tx])));
    let refused = world.apply_as_read(&tx, &subtrees(vec![convert(B)]));
    let refused = refused.expect_err("a refusal");
    assert!(moved_by_key(&refused, B), "{refused}");
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // With no commit in between, the statement is committed with the move.
    let tx = world.begin();
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    world.apply(&tx, &batch).expect("the subtree is moved");
    world
        .apply(&tx, &subtrees(vec![convert(B)]))
        .expect("the moved subtree is stated anew");
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert_eq!(tree_type(&reader, B), Some(WIDE.to_vec()));
    assert_eq!(tree_type(&reader, A), None);
}

/// A Merk with no host and one node, `x`, which hosts nothing.
fn alone<'s>(world: &mut World<'s>) {
    let tx = world.begin();
    let root = world.merk(&tx, ROOT, |items| {
        items.insert(b"x".to_vec(), b"an item".to_vec());
    });
    let made = OwnedBatch {
        merks: vec![root],
        subtrees: vec![enter(ROOT)],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &made).expect("the Merk is made");
    assert!(committed(world.commit(tx, &[])));
}

/// A layout that stopped over one version is tried again over the next
/// (WT7). A commit enters a subtree with no node under the prefix that a
/// moved one went to, below a Merk of its own, and the move stops. A
/// later commit drops that Merk, and neither meets the mover. The mover
/// then has its frames again: it reads what its move decides, takes a
/// clear, and commits.
#[test]
fn a_move_that_was_stopped_is_made_once_the_commits_let_it() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    alone(&mut world);
    let tx = world.begin();
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");

    let other = world.begin();
    let entered = subtrees(vec![hosted(B, ROOT, b"x")]);
    world.apply(&other, &entered).expect("a subtree is entered");
    assert!(committed(world.commit(other, &[&tx])));
    let refused = get(&tx, aux(B), b"a").expect_err("a refusal");
    let stopped = matches!(
        &refused,
        TxnError::Batch(BatchError::Entered { subtree }) if *subtree == sub(B)
    );
    assert!(stopped && !refused.is_conflict(), "{refused}");

    let other = world.begin();
    let dropped = subtrees(vec![OwnedSubtree::Drop(sub(ROOT))]);
    world.apply(&other, &dropped).expect("a drop");
    assert!(committed(world.commit(other, &[&tx])));
    assert_eq!(
        get(&tx, aux(B), b"a").expect("a read"),
        Some(vec![1; 3]),
        "the move is made again"
    );
    assert_eq!(tx.txn.stats().overlapped, 0);
    assert!(tx.txn.stats().base.is_some(), "frames again");
    world.check(&tx);
    let cleared = subtrees(vec![OwnedSubtree::Clear(model::subtree(1))]);
    world.apply(&tx, &cleared).expect("a clear");
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert_eq!(
        get(&reader, aux(B), b"a").expect("a read"),
        Some(vec![1; 3])
    );
    assert_eq!(shown(&reader, ROOT), None);
    assert_eq!(db.private_ids(), 0);
}

/// What leaves with a move is asked again each time the move is made
/// again. A subtree that a commit entered below the moved one leaves
/// with it. Once a later commit has dropped that subtree and entered one
/// of its prefix under a Merk beside, the move leaves that one alone:
/// the mover reads it, before its commit and after.
#[test]
fn a_subtree_that_left_with_a_move_and_is_entered_elsewhere_since_stays() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    alone(&mut world);
    let tx = world.begin();
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");

    let other = world.begin();
    let entered = OwnedBatch {
        records: vec![put(aux(NONE), b"f", b"below the moved one")],
        subtrees: vec![hosted(NONE, A, b"k1")],
        ..OwnedBatch::default()
    };
    world.apply(&other, &entered).expect("a subtree is entered");
    assert!(committed(world.commit(other, &[&tx])));
    world.check(&tx);
    assert_eq!(shown(&tx, NONE), None, "it leaves with the move");
    assert_eq!(get(&tx, aux(NONE), b"f").expect("a read"), None);

    let other = world.begin();
    let again = OwnedBatch {
        records: vec![put(aux(NONE), b"z", b"elsewhere")],
        subtrees: vec![OwnedSubtree::Drop(sub(NONE)), hosted(NONE, ROOT, b"x")],
        ..OwnedBatch::default()
    };
    world
        .apply(&other, &again)
        .expect("it is entered elsewhere");
    assert!(committed(world.commit(other, &[&tx])));
    world.check(&tx);
    assert_eq!(tx.txn.stats().overlapped, 0);
    let stays = |tx: &Tx<'_>| {
        let (_, _, above) = shown(tx, NONE).expect("the subtree is entered");
        assert_eq!(above, host(ROOT, b"x"));
        assert_eq!(
            get(tx, aux(NONE), b"z").expect("a read"),
            Some(b"elsewhere".to_vec())
        );
        assert_eq!(get(tx, aux(NONE), b"f").expect("a read"), None);
    };
    stays(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    stays(&reader);
}

/// The first rule of CF5 goes by the Merks that a transaction found a
/// subtree below when it last took the commits in. A mover takes a
/// subtree to a prefix and on from there, so the second move takes its
/// subtree from a prefix that the committed state has none under. A
/// commit enters a subtree there, below a Merk of its own, and a later
/// commit drops that Merk. Where the mover took the first commit in
/// before the second was made, it saw a subtree there and then none:
/// both moves cover the prefix, and both are met. A read does that, and
/// so does a batch that is refused because the move cannot be laid out
/// over the first commit. Where the mover made no call between the two,
/// or one that is answered before it begins, as an empty batch is, or
/// one whose look at the commit failed on a read after it had found the
/// subtree there, it saw none at either time, is not met, and commits.
#[test]
fn a_drop_above_a_prefix_a_move_takes_from_meets_the_move_where_the_mover_saw_a_subtree_there() {
    let between = [
        "nothing",
        "a read",
        "an empty batch",
        "a refused clear",
        "a look that fails",
    ];
    for between in between {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        alone(&mut world);
        let tx = world.begin();
        let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
        world.apply(&tx, &batch).expect("the subtree is moved");
        let batch = rename(&world, &tx, b"b", b"c", &[(B, C), (B_IN, C_IN)]);
        world.apply(&tx, &batch).expect("and moved on");
        let top = |tx: &Tx<'_>| get(tx, aux(TOP), b"t").expect("a read");

        let other = world.begin();
        let entered = subtrees(vec![hosted(B, ROOT, b"x")]);
        world.apply(&other, &entered).expect("a subtree is entered");
        assert!(committed(world.commit(other, &[&tx])));
        match between {
            "a read" => assert_eq!(top(&tx), Some(b"top".to_vec())),
            "an empty batch" => {
                let empty = world.apply_as_read(&tx, &OwnedBatch::default());
                empty.expect("an empty batch");
            }
            "a refused clear" => {
                let cleared = subtrees(vec![OwnedSubtree::Clear(sub(B))]);
                let refused = batch_error(world.apply_as_read(&tx, &cleared));
                assert_eq!(refused, BatchError::Entered { subtree: sub(B) });
            }
            "a look that fails" => {
                tx.txn.refuse_look();
                let refused = get(&tx, aux(TOP), b"t").expect_err("no look");
                assert!(matches!(refused, TxnError::Damaged(_)), "{refused}");
            }
            _ => {}
        }
        let other = world.begin();
        let dropped = subtrees(vec![OwnedSubtree::Drop(sub(ROOT))]);
        world.apply(&other, &dropped).expect("a drop");
        assert!(committed(world.commit(other, &[&tx])));

        assert_eq!(top(&tx), Some(b"top".to_vec()));
        let looked = matches!(between, "a read" | "a refused clear");
        let met = match looked {
            true => 2,
            false => 0,
        };
        assert_eq!(tx.txn.stats().overlapped, met, "{between}");
        assert_eq!(tx.model.borrow().met(), met, "{between}");
        if !looked {
            world.check(&tx);
            assert_eq!(get(&tx, aux(C), b"a").expect("a read"), Some(vec![1; 3]));
        }
        let outcome = world.commit(tx, &[]);
        assert_eq!(outcome == Outcome::Conflict, looked, "{between}");
        world.check_committed();
    }
}

/// What a clear takes below its Merk is asked again each time the
/// transaction's writes are applied over a commit, and so is what a move
/// that comes after it finds there. A transaction clears a Merk that has
/// nodes and moves it, with a pair for a subtree that is not there. A
/// commit then enters a subtree under that pair's first prefix, below the
/// Merk as the committed state has it. Made again, the clear takes that
/// subtree, and the move has none to take to the pair's second prefix.
#[test]
fn a_subtree_entered_below_a_merk_that_was_cleared_before_its_move_goes_with_the_clear() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let mut top = world.merk(&tx, TOP, |items| {
        items.get_mut(&b"a"[..]).expect("the host").push(0);
    });
    top.hosted.insert(b"a".to_vec(), vec![sub(A)]);
    let cleared = OwnedBatch {
        merks: vec![top],
        subtrees: vec![OwnedSubtree::Clear(sub(A))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &cleared).expect("a clear");
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (NONE, NONE_B)]);
    world.apply(&tx, &batch).expect("the subtree is moved");

    let other = world.begin();
    let entered = subtrees(vec![hosted(NONE, A, b"k1")]);
    world.apply(&other, &entered).expect("a subtree is entered");
    assert!(committed(world.commit(other, &[&tx])));
    world.check(&tx);
    assert_eq!(tx.txn.stats().overlapped, 0);
    let pairs = (tx.model.borrow().writes.iter())
        .find_map(|(write, ..)| match write {
            Write::Move(moved) => Some(moved.pairs.clone()),
            _ => None,
        })
        .expect("the move");
    assert_eq!(pairs, vec![(sub(A), sub(B))]);
    let hosts = tx.model.borrow().hosts(&world.model.hosts);
    for gone in [NONE, NONE_B, A_IN] {
        assert_eq!(shown(&tx, gone), None, "{gone}");
        assert_eq!(hosts.get(&sub(gone)), None, "{gone}");
    }
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    for gone in [NONE, NONE_B, A_IN] {
        assert_eq!(shown(&reader, gone), None, "{gone}");
        assert_eq!(world.model.hosts.get(&sub(gone)), None, "{gone}");
    }
    assert!(shown(&reader, B).is_some());
    assert_eq!(db.private_ids(), 0);
}

/// What a drop or a clear covers below its subtree is settled when it is
/// stated: the subtrees there that its transaction had a write under
/// (CF5). That an earlier clear took a subtree when it was made again
/// over a commit is no write under it. A transaction clears a Merk, a
/// commit enters a subtree below that Merk, and the clear, made again,
/// takes the subtree. The transaction then enters a subtree under that
/// prefix below another Merk, and drops that Merk. Where it wrote
/// nothing under the prefix, the drop covers its own Merk alone, and
/// the commit of the transaction does not meet a mover beside it whose
/// move names the prefix. Where it put a record there before the drop,
/// the drop covers the prefix, and the commit meets the mover.
#[test]
fn a_later_drop_covers_a_subtree_that_a_clear_took_when_made_again_only_where_it_was_written() {
    for written in [false, true] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        alone(&mut world);
        more(&mut world);
        let mover = world.begin();
        let pairs = [(A, B), (A_IN, B_IN), (NONE, NONE_B)];
        let batch = rename(&world, &mover, b"a", b"b", &pairs);
        world.apply(&mover, &batch).expect("the subtree is moved");

        let tx = world.begin();
        let cleared = subtrees(vec![OwnedSubtree::Clear(sub(ROOT))]);
        world.apply(&tx, &cleared).expect("a clear");
        let other = world.begin();
        let entered = subtrees(vec![hosted(NONE, ROOT, b"x")]);
        world.apply(&other, &entered).expect("a subtree is entered");
        assert!(committed(world.commit(other, &[&tx, &mover])));
        assert_eq!(shown(&tx, NONE), None, "the clear takes it");
        assert_eq!(tx.txn.stats().overlapped, 0);

        let entered = subtrees(vec![hosted(NONE, EMPTY_2, b"n")]);
        world.apply(&tx, &entered).expect("a subtree is entered");
        if written {
            world.write(&tx, vec![put(aux(NONE), b"k", b"written")]);
        }
        let dropped = subtrees(vec![OwnedSubtree::Drop(sub(EMPTY_2))]);
        world.apply(&tx, &dropped).expect("a drop");
        let last = tx.model.borrow().writes.last().cloned();
        let Some((Write::Leave(left), ..)) = last else {
            panic!("the last write is the drop");
        };
        let cut = match written {
            true => vec![sub(NONE)],
            false => Vec::new(),
        };
        assert_eq!((left.below, left.cut), (vec![sub(NONE)], cut));
        world.check(&tx);
        assert!(committed(world.commit(tx, &[&mover])));

        let met = usize::from(written);
        let read = get(&mover, aux(B), b"a");
        if written {
            let refused = read.expect_err("a refusal");
            assert!(moved_by_key(&refused, B), "{refused}");
        } else {
            assert_eq!(read.expect("a read"), Some(vec![1; 3]));
        }
        assert_eq!(mover.txn.stats().overlapped, met, "written {written}");
        assert_eq!(mover.model.borrow().met(), met, "written {written}");
        if !written {
            world.check(&mover);
        }
        let outcome = world.commit(mover, &[]);
        assert_eq!(outcome == Outcome::Conflict, written, "written {written}");
        world.check_committed();
    }
}

/// A commit enters a subtree below the moved one, and below that one a
/// subtree for which the move states a pair. Made again over it, the move
/// would be refused: a subtree that is named is below one that leaves
/// (MV2). The commit wrote a node of the moved subtree, so it met the
/// move, and the mover is told of the conflict, not of that. A rollback
/// to a savepoint from before the move gives it its frames again.
#[test]
fn a_commit_that_enters_subtrees_below_a_moved_one_with_their_hosts_meets_the_move() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"own", b"kept")]);
    tx.txn.set_savepoint().expect("no scope is open");
    let before = tx.model.borrow().writes.len();
    let pairs = [(A, B), (A_IN, B_IN), (NONE, NONE_B)];
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    world.apply(&tx, &batch).expect("the subtree is moved");

    let other = world.begin();
    let mut entered = edit_at(&world, &other, &[(A, b"a")], |items| {
        items.insert(b"p".to_vec(), b"a tree".to_vec());
    });
    (entered.merks[0].hosted).insert(b"p".to_vec(), vec![sub(FREE)]);
    let mut inner = world.merk(&other, FREE, |items| {
        items.insert(b"x".to_vec(), b"a tree with no node".to_vec());
    });
    inner.hosted.insert(b"x".to_vec(), vec![sub(NONE)]);
    entered.merks.insert(0, inner);
    entered.subtrees = vec![hosted(FREE, A, b"p"), hosted(NONE, FREE, b"x")];
    world
        .apply(&other, &entered)
        .expect("the subtrees are entered");
    assert!(committed(world.commit(other, &[&tx])));

    let refused = get(&tx, aux(B), b"a").expect_err("a refusal");
    assert!(moved_by_key(&refused, B), "{refused}");
    // The move is met, and so are the nodes of the Merk above that both
    // wrote.
    assert_eq!(tx.txn.stats().overlapped, tx.model.borrow().met());
    assert!(tx.model.borrow().conflicts() && tx.model.borrow().stopped());

    tx.txn.rollback_to_savepoint().expect("a savepoint is set");
    tx.model.borrow_mut().writes.truncate(before);
    assert!(!tx.model.borrow().conflicts() && !tx.model.borrow().stopped());
    world.check(&tx);
    assert!(tx.txn.stats().base.is_some(), "frames again");
    let (_, _, above) = shown(&tx, NONE).expect("the subtree the commit entered");
    assert_eq!(above, host(FREE, b"x"));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// CF2 and CF10 for a range delete with no key in it and a move. In a
/// commit, it meets no move by its keys: it has none. A commit that moved
/// a subtree took it whole, and meets every write that another
/// transaction has under a prefix it covers, such a range delete among
/// them. A range delete with a key in it and a move meet in both orders.
/// The move is one that leaves the subtree entered under its prefix, at
/// another key: no conflict here is one for a subtree that is gone.
#[test]
fn a_range_delete_and_a_move_meet_by_the_keys_of_the_one_and_the_subtree_of_the_other() {
    for (empty, mover_first) in [(true, false), (true, true), (false, false), (false, true)] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        let mover = world.begin();
        let batch = rename(&world, &mover, b"a", b"b", &[(A, A), (A_IN, A_IN)]);
        world.apply(&mover, &batch).expect("the subtree is moved");
        world.check(&mover);
        let other = world.begin();
        let end = match empty {
            true => b"a".to_vec(),
            false => b"b".to_vec(),
        };
        world.write(&other, vec![Write::Range(aux(A), b"a".to_vec(), Some(end))]);

        let (first, second, met) = match mover_first {
            true => (mover, other, true),
            false => (other, mover, !empty),
        };
        assert!(committed(world.commit(first, &[&second])));
        assert_eq!(
            get(&second, aux(TOP), b"t").expect("a read"),
            Some(b"top".to_vec())
        );
        let case = format!("empty {empty}, the mover first {mover_first}");
        assert_eq!(second.txn.stats().overlapped, usize::from(met), "{case}");
        let outcome = world.commit(second, &[]);
        assert_eq!(outcome == Outcome::Conflict, met, "{case}");
        world.check_committed();
        let reader = world.begin();
        let host_key: &[u8] = match (mover_first, met) {
            (true, _) | (false, false) => b"b",
            (false, true) => b"a",
        };
        let (_, _, above) = shown(&reader, A).expect("the subtree is entered");
        assert_eq!(above, host(TOP, host_key), "{case}");
    }
}

/// MV7, where the transaction's own enters and the committed state do
/// not agree about a subtree: the mover entered it under a Merk that no
/// move names, and a commit beside entered a subtree of that prefix below
/// the prefix the moved one went to. By key the mover does not answer for
/// it: not a read, by key or by cursor, and not a write.
#[test]
fn a_subtree_that_a_commit_entered_below_a_target_is_not_answered_for_by_key() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let mover = world.begin();
    let mut top = world.merk(&mover, TOP, |items| {
        items.insert(b"x".to_vec(), b"a tree".to_vec());
    });
    top.hosted.insert(b"x".to_vec(), vec![sub(EMPTY_2)]);
    let own = OwnedBatch {
        merks: vec![top],
        subtrees: vec![hosted(EMPTY_2, TOP, b"x")],
        ..OwnedBatch::default()
    };
    world.apply(&mover, &own).expect("a subtree of its own");
    let batch = rename(&world, &mover, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&mover, &batch).expect("the subtree is moved");

    let other = world.begin();
    let mut nodes = world.merk(&other, B, |items| {
        items.insert(b"z".to_vec(), b"a tree".to_vec());
    });
    nodes.hosted.insert(b"z".to_vec(), vec![sub(EMPTY_2)]);
    let entered = OwnedBatch {
        merks: vec![nodes],
        records: vec![put(aux(EMPTY_2), b"f", b"foreign")],
        subtrees: vec![enter(B), hosted(EMPTY_2, B, b"z")],
    };
    world.apply(&other, &entered).expect("a subtree is entered");
    assert!(committed(world.commit(other, &[&mover])));

    let refused = get(&mover, aux(EMPTY_2), b"f").expect_err("a refusal");
    assert!(moved_by_key(&refused, EMPTY_2), "{refused}");
    let refused = (mover.txn.cursor(aux(EMPTY_2)).seek_to_first()).expect_err("a refusal");
    assert!(moved_by_key(&refused, EMPTY_2), "{refused}");
    let refused = (mover.txn.subtree(sub(EMPTY_2), |_| ())).expect_err("a refusal");
    assert!(moved_by_key(&refused, EMPTY_2), "{refused}");
    let record = OwnedBatch {
        records: vec![put(aux(EMPTY_2), b"more", b"by key")],
        ..OwnedBatch::default()
    };
    let refused = world.apply(&mover, &record).expect_err("a refusal");
    assert!(moved_by_key(&refused, EMPTY_2), "{refused}");
    assert_eq!(world.commit(mover, &[]), Outcome::Conflict);
    world.check_committed();
}

/// A second pair of subtrees under the node `d` of `TOP`, `C` with
/// `C_IN` below it, and the Merk `EMPTY_2`, which has a node and no host.
fn more<'s>(world: &mut World<'s>) {
    let tx = world.begin();
    let batch = nested(world, &tx, b"d", C, C_IN, 50);
    world.apply(&tx, &batch).expect("the subtrees are made");
    let alone = world.merk(&tx, EMPTY_2, |items| {
        items.insert(b"n".to_vec(), b"v".to_vec());
    });
    let made = OwnedBatch {
        merks: vec![alone],
        subtrees: vec![enter(EMPTY_2)],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &made).expect("a Merk with no host");
    assert!(committed(world.commit(tx, &[])));
}

/// Section 6.3: a move that cannot be made is refused, by what the batch
/// says alone or by what the directory has, with the refusal that names
/// the cause. None is a conflict where no commit outside explains it,
/// each leaves the transaction as it was, and the move that can be made
/// is taken afterwards.
#[test]
fn a_move_that_cannot_be_made_is_refused_and_changes_nothing() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    more(&mut world);

    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"own", b"kept")]);
    let full = [(A, B), (A_IN, B_IN)];
    let good = rename(&world, &tx, b"a", b"b", &full);
    let OwnedSubtree::Move { read, .. } = &good.subtrees[0] else {
        unreachable!("a move");
    };
    let read = *read;
    // The batch of `good` with other moves in place of its own.
    let stating = |moves: Vec<OwnedSubtree>| OwnedBatch {
        subtrees: moves,
        ..good.clone()
    };
    let with = |pairs: &[(u8, u8)]| stating(vec![moving(b"b", read, pairs)]);
    let changed = |change: &dyn Fn(&mut OwnedBatch)| {
        let mut batch = good.clone();
        change(&mut batch);
        batch
    };
    let (twice, host) = (
        |subtree| BatchError::SubtreeTwice { subtree },
        |subtree| BatchError::MoveHost { subtree },
    );
    let moved = |subtree| BatchError::Moved { subtree };

    let mut refused: Vec<(&str, OwnedBatch, BatchError)> = Vec::new();
    // What the directory has.
    refused.push((
        "the subtree is not entered",
        rename(&world, &tx, b"a", b"b", &[(NONE, B)]),
        BatchError::NotEntered { subtree: sub(NONE) },
    ));
    refused.push((
        "a subtree is entered where the moved one goes",
        rename(&world, &tx, b"a", b"b", &[(A, C), (A_IN, B_IN)]),
        BatchError::Entered { subtree: sub(C) },
    ));
    refused.push((
        "a subtree is entered where one below goes",
        with(&[(A, B), (A_IN, C_IN)]),
        BatchError::Entered { subtree: sub(C_IN) },
    ));
    refused.push((
        "a subtree is entered where a pair that names none would bring one",
        with(&[(A, B), (A_IN, B_IN), (NONE, C_IN)]),
        BatchError::Entered { subtree: sub(C_IN) },
    ));
    refused.push((
        "a Merk with a node below is not named",
        with(&[(A, B)]),
        BatchError::MoveUnnamed { subtree: sub(A_IN) },
    ));
    refused.push((
        "a pair names a subtree that is entered elsewhere",
        with(&[(A, B), (A_IN, B_IN), (C_IN, NONE)]),
        moved(sub(C_IN)),
    ));
    // The host.
    refused.push((
        "the batch puts no node",
        subtrees(vec![moving(b"b", read, &full)]),
        host(sub(A)),
    ));
    refused.push((
        "the node hosts another subtree",
        changed(&|batch| {
            (batch.merks[0].hosted).insert(b"b".to_vec(), vec![sub(NONE)]);
        }),
        host(sub(A)),
    ));
    refused.push((
        "the subtree is hosted at another position",
        changed(&|batch| {
            (batch.merks[0].hosted).insert(b"b".to_vec(), vec![sub(NONE), sub(B)]);
            let OwnedSubtree::Move { host, .. } = &mut batch.subtrees[0] else {
                unreachable!("a move");
            };
            host.2 = 1;
        }),
        host(sub(A)),
    ));
    refused.push((
        "the node does not name the subtree that the old host's cell hosts",
        changed(&|batch| {
            batch.merks[0].hosted.remove(&b"b"[..]);
        }),
        host(sub(A)),
    ));
    refused.push((
        "the node hosts no Merk, and the old host's cell hosts the subtree",
        changed(&|batch| {
            (batch.merks[0].hosted).insert(b"b".to_vec(), Vec::new());
        }),
        host(sub(A)),
    ));
    refused.push((
        "the node names the new prefix at another position than the stated one",
        changed(&|batch| {
            (batch.merks[0].hosted).insert(b"b".to_vec(), vec![sub(NONE), sub(B)]);
        }),
        host(sub(A)),
    ));
    refused.push((
        "the node names the new prefix at the stated position and at another",
        changed(&|batch| {
            (batch.merks[0].hosted).insert(b"b".to_vec(), vec![sub(B), sub(B)]);
        }),
        host(sub(A)),
    ));
    let mut elsewhere = world.merk(&tx, C, |items| {
        items.insert(b"z".to_vec(), b"a tree".to_vec());
    });
    elsewhere.hosted.insert(b"z".to_vec(), vec![sub(B)]);
    refused.push((
        "the node is of another Merk",
        OwnedBatch {
            merks: vec![elsewhere],
            subtrees: vec![OwnedSubtree::Move {
                from: sub(A),
                to: sub(B),
                host: (sub(C), b"z".to_vec(), 0),
                read,
                below: vec![(sub(A_IN), sub(B_IN))],
            }],
            ..OwnedBatch::default()
        },
        host(sub(A)),
    ));
    let mut hosting = world.merk(&tx, TOP, |items| {
        items.insert(b"h".to_vec(), b"a tree".to_vec());
    });
    hosting.hosted.insert(b"h".to_vec(), vec![sub(NONE_B)]);
    refused.push((
        "the subtree has no host",
        OwnedBatch {
            merks: vec![hosting],
            subtrees: vec![moving(b"h", None, &[(EMPTY_2, NONE_B)])],
            ..OwnedBatch::default()
        },
        host(sub(EMPTY_2)),
    ));
    // A prefix twice.
    refused.push((
        "one subtree is moved twice",
        stating(vec![
            moving(b"b", read, &full),
            moving(b"b", read, &[(A, NONE), (A_IN, NONE_B)]),
        ]),
        twice(sub(A)),
    ));
    refused.push((
        "two subtrees are moved to one prefix",
        stating(vec![
            moving(b"b", read, &full),
            moving(b"b", read, &[(C, B), (C_IN, NONE)]),
        ]),
        twice(sub(B)),
    ));
    refused.push((
        "two subtrees of one move go to one prefix",
        with(&[(A, B), (A_IN, B)]),
        twice(sub(B)),
    ));
    // What else the batch says of a prefix a move names.
    refused.push((
        "a record under a new prefix",
        changed(&|batch| batch.records = vec![put(aux(B_IN), b"k", b"v")]),
        moved(sub(B_IN)),
    ));
    let inside = world.merk(&tx, A, |items| {
        items.insert(b"k9".to_vec(), vec![9; 9]);
    });
    refused.push((
        "a change set of the moved Merk",
        changed(&|batch| batch.merks.push(inside.clone())),
        moved(sub(A)),
    ));
    refused.push((
        "an enter where the subtree goes",
        changed(&|batch| batch.subtrees.push(enter(B))),
        moved(sub(B)),
    ));
    refused.push((
        "a drop of a subtree that is moved",
        changed(&|batch| batch.subtrees.insert(0, OwnedSubtree::Drop(sub(A_IN)))),
        moved(sub(A_IN)),
    ));
    refused.push((
        "a clear where a subtree goes",
        changed(&|batch| batch.subtrees.insert(0, OwnedSubtree::Clear(sub(B_IN)))),
        moved(sub(B_IN)),
    ));
    // The host of another move is in a Merk that is moved.
    let mut outer = world.merk(&tx, A, |items| {
        let value = items.remove(&b"in"[..]).expect("the node");
        items.insert(b"in2".to_vec(), value);
    });
    outer.hosted.insert(b"in2".to_vec(), vec![sub(NONE_B)]);
    refused.push((
        "a host in a Merk that is moved",
        changed(&|batch| {
            batch.merks.push(outer.clone());
            batch.subtrees = vec![
                moving(b"b", read, &[(A, B)]),
                OwnedSubtree::Move {
                    from: sub(A_IN),
                    to: sub(NONE_B),
                    host: (sub(A), b"in2".to_vec(), 0),
                    read: None,
                    below: Vec::new(),
                },
            ];
        }),
        moved(sub(A)),
    ));
    // The batch drops the Merk that the host is in.
    refused.push((
        "a drop of the Merk the host is in",
        changed(&|batch| batch.subtrees.insert(0, OwnedSubtree::Drop(sub(TOP)))),
        moved(sub(TOP)),
    ));

    for (what, batch, expected) in &refused {
        let error = world.apply(&tx, batch).expect_err(what);
        assert!(!error.is_conflict(), "{what}: {error}");
        assert_eq!(batch_error(Err(error)), *expected, "{what}");
        assert_eq!(tx.txn.stats().writes, 1, "{what}");
    }
    world.check(&tx);
    // The node that the batch puts for the moved subtree hosts one that
    // stays: the directory's refusal, as for an enter.
    let taken = rename(&world, &tx, b"a", b"d", &[(A, NONE), (A_IN, NONE_B)]);
    let error = world.apply(&tx, &taken).expect_err("the host is taken");
    let host_taken = lignin_tree::directory::DirectoryError::HostTaken {
        parent: *sub(TOP).as_bytes(),
        position: 0,
    };
    assert_eq!(
        format!("{error}"),
        format!("{}", TxnError::from(host_taken))
    );
    assert!(!error.is_conflict());
    assert_eq!(tx.txn.stats().writes, 1);

    // A pair may name a subtree that is not entered: no entry is re-keyed
    // for it by this application.
    let batch = with(&[(A, B), (A_IN, B_IN), (NONE, NONE_B)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    world.check(&tx);
    assert_eq!(shown(&tx, NONE_B), None);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// Section 2: what is entered where a subtree is moved to is dropped by
/// the batch of the move, before the move, with the subtrees below it.
#[test]
fn a_move_goes_where_the_batch_drops_what_is_entered() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    more(&mut world);
    let tx = world.begin();
    let mut top = world.merk(&tx, TOP, |items| {
        let value = items.remove(&b"a"[..]).expect("the node");
        items.insert(b"d".to_vec(), value);
    });
    top.hosted.insert(b"d".to_vec(), vec![sub(C)]);
    let read = (tx.txn.node(sub(TOP), b"a", handle_of)).expect("the host is read");
    let batch = OwnedBatch {
        merks: vec![top],
        subtrees: vec![
            OwnedSubtree::Drop(sub(C)),
            moving(b"d", read, &[(A, C), (A_IN, C_IN)]),
        ],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the subtree is moved");
    world.check(&tx);
    // The model has the move made too, when it is stated and when the
    // writes are made again over a commit.
    assert!(!tx.model.borrow().stopped(), "the drop frees the prefix");
    let beside = OwnedBatch {
        records: vec![put(aux(TOP), b"u", b"beside")],
        ..OwnedBatch::default()
    };
    assert!(committed(world.write_outside(&beside, &[&tx])));
    world.check(&tx);
    assert!(!tx.model.borrow().stopped(), "and where it is made again");
    assert_eq!(tx.txn.stats().overlapped, 0);
    assert_eq!([shown(&tx, A), shown(&tx, A_IN)], [None, None]);
    assert_eq!(get(&tx, aux(C), b"a").expect("a read"), Some(vec![1; 3]));
    assert_eq!(
        get(&tx, aux(C_IN), b"ia").expect("a read"),
        Some(vec![1; 9])
    );
    let meta = model::unit(Column::Meta, C_IN);
    assert_eq!(get(&tx, meta, b"im").expect("a read"), Some(vec![1; 400]));
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}

/// Section 6.3: a refusal of a move that a commit outside explains, which
/// the transaction took in since the read the move names, is a conflict:
/// the subtree is not entered and a commit took a subtree out, here one
/// above the Merk the host is in, whose key set names neither; or a
/// subtree is entered where one goes and a commit wrote under that
/// prefix.
#[test]
fn a_refusal_of_a_move_that_a_commit_explains_is_a_conflict() {
    let pairs = [(A, B), (A_IN, B_IN)];
    // With a drop in the batch of the move the refusal comes before the
    // application, where the batch asks what its drop takes.
    for (entered, beside_a_drop) in [(false, false), (true, false), (false, true), (true, true)] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        // `TOP` is hosted by the node `t` of `ROOT`.
        let tx = world.begin();
        let mut root = world.merk(&tx, ROOT, |items| {
            items.insert(b"t".to_vec(), b"a tree".to_vec());
        });
        root.hosted.insert(b"t".to_vec(), vec![sub(TOP)]);
        let top = world.merk(&tx, TOP, |items| {
            items.insert(b"x".to_vec(), vec![9; 30]);
        });
        let made = OwnedBatch {
            merks: vec![top, root],
            subtrees: vec![enter(ROOT), hosted(TOP, ROOT, b"t"), enter(EMPTY_2)],
            ..OwnedBatch::default()
        };
        world.apply(&tx, &made).expect("the top is made");
        // The batch that changes `TOP` writes the node that hosts it.
        let mut batch = nested(&world, &tx, b"a", A, A_IN, 1);
        let mut root = world.merk(&tx, ROOT, |items| {
            items.insert(b"t".to_vec(), b"a tree with a tree".to_vec());
        });
        root.hosted.insert(b"t".to_vec(), vec![sub(TOP)]);
        batch.merks.push(root);
        world.apply(&tx, &batch).expect("the subtrees are made");
        assert!(committed(world.commit(tx, &[])));

        let tx = world.begin();
        world.write(&tx, vec![put(aux(1), b"own", b"kept")]);
        let mut stale = read_now(&tx, &rename(&world, &tx, b"a", b"b", &pairs));
        if beside_a_drop {
            (stale.subtrees).insert(0, OwnedSubtree::Drop(sub(EMPTY_2)));
        }

        // A commit that writes no node of the Merk the host is in.
        let other = world.begin();
        let (batch, named) = match entered {
            true => {
                let made = OwnedBatch {
                    records: vec![put(aux(B), b"k", b"of another")],
                    subtrees: vec![enter(B)],
                    ..OwnedBatch::default()
                };
                (made, B)
            }
            false => (subtrees(vec![OwnedSubtree::Drop(sub(ROOT))]), A),
        };
        world.apply(&other, &batch).expect("a batch");
        assert!(committed(world.commit(other, &[&tx])));

        let refused = world.apply_as_read(&tx, &stale).expect_err("a refusal");
        let subtree = sub(named);
        assert_eq!(conflict(refused), BatchError::ValueTaken { subtree });
        world.check(&tx);
        assert_eq!(tx.txn.stats().overlapped, 0);
        assert!(committed(world.commit(tx, &[])));
        world.check_committed();
    }
}

/// Sections 5 and 6 of the answer: a move is laid out when it is stated.
/// A scope that was opened beside another keeps its writes in the write
/// set and refuses a move until its writes are laid out; the move then
/// holds every column of every prefix it names against the scopes beside
/// it. A move is refused, too, while an element of a Merk it names is one
/// that was marked pending.
#[test]
fn a_move_is_made_through_a_scope_whose_writes_are_laid_out() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let pairs = [(A, B), (A_IN, B_IN)];
    let beside_error = |error: TxnError| {
        assert!(!error.is_conflict(), "{error}");
        batch_error(Err(error))
    };
    let subtree = sub(A);

    let tx = world.begin();
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    // A scope that made a move and is rolled back takes the move with it.
    let made = tx.model.borrow().writes.len();
    let scope = tx.txn.scope().expect("a scope");
    assert!(scope.is_laid_out());
    let through = batch.through(&scope);
    (world.apply_in(&tx, &scope, &through)).expect("the subtree is moved");
    assert_eq!(tx.txn.stats().writes, through.writes().len());
    scope.rollback();
    tx.model.borrow_mut().writes.truncate(made);
    assert_eq!(tx.txn.stats().writes, 0);
    world.check(&tx);
    assert!(shown(&tx, A).is_some() && shown(&tx, B).is_none());

    let alone = tx.txn.scope().expect("a scope");
    let beside = tx.txn.scope().expect("a scope");
    assert!(!beside.is_laid_out());
    let batch = batch.through(&beside);
    let refused = world.apply_in(&tx, &beside, &batch).expect_err("refused");
    assert_eq!(beside_error(refused), BatchError::MoveBeside { subtree });
    assert_eq!(tx.txn.stats().writes, 0);
    beside.lay_out().expect("it has no write");
    world
        .apply_in(&tx, &beside, &batch)
        .expect("the subtree is moved");
    // The scope beside it reads and writes nothing under a prefix the
    // move names, old or new, in any column.
    let mut out = Vec::new();
    for unit in [aux(A), aux(B_IN), model::unit(Column::Meta, A_IN)] {
        let error = alone.get(unit, b"a", &mut out).expect_err("held");
        assert!(
            matches!(error, TxnError::ScopeOverlap { unit: by } if by.subtree == unit.subtree),
            "{error}"
        );
    }
    let error = alone.subtree(sub(B), |_| ()).expect_err("held");
    assert!(matches!(error, TxnError::ScopeOverlap { .. }), "{error}");
    beside.merge();
    alone.merge();
    world.check(&tx);
    // A scope that keeps its writes in the write set does not answer
    // where the move decides, until its writes are laid out.
    let first = tx.txn.scope().expect("a scope");
    let second = tx.txn.scope().expect("a scope");
    let refused = second.get(aux(B), b"a", &mut out).expect_err("refused");
    let subtree = sub(B);
    assert_eq!(beside_error(refused), BatchError::MoveBeside { subtree });
    // Neither does it take a write there, under the new prefix or under
    // the old one.
    let record = |name: u8| OwnedBatch {
        records: vec![put(aux(name), b"more", b"of a scope")],
        ..OwnedBatch::default()
    };
    for name in [B, A] {
        let refused = (world.apply_in(&tx, &second, &record(name))).expect_err("refused");
        let subtree = sub(name);
        assert_eq!(beside_error(refused), BatchError::MoveBeside { subtree });
    }
    // Nor a node that names the moved subtree as one it hosts: the batch
    // reads the entry of that subtree.
    let mut hosts = world.merk(&tx, TOP, |items| {
        items.insert(b"b".to_vec(), b"a tree, again".to_vec());
    });
    hosts.hosted.insert(b"b".to_vec(), vec![sub(B)]);
    let hosts = OwnedBatch {
        merks: vec![hosts],
        ..OwnedBatch::default()
    };
    let hosts = hosts.through(&second);
    let refused = (world.apply_in(&tx, &second, &hosts)).expect_err("refused");
    let subtree = sub(B);
    assert_eq!(beside_error(refused), BatchError::MoveBeside { subtree });
    // Nor a drop or a clear, of whichever subtree: what it takes would be
    // asked of a directory that does not have the moved subtree where it
    // is. The refusal names the first that the batch states.
    let (top, far) = (sub(TOP), model::subtree(1));
    for (leaves, subtree) in [
        (vec![OwnedSubtree::Drop(top)], top),
        (vec![OwnedSubtree::Clear(far)], far),
        (vec![OwnedSubtree::Drop(far), OwnedSubtree::Drop(top)], far),
        (vec![OwnedSubtree::Drop(top), OwnedSubtree::Clear(far)], top),
    ] {
        let refused = (world.apply_in(&tx, &second, &subtrees(leaves))).expect_err("refused");
        assert_eq!(beside_error(refused), BatchError::MoveBeside { subtree });
    }
    // A write that is refused for a move claims nothing: the scope beside
    // reads what those batches named.
    assert!(!first.get(aux(A), b"more", &mut out).expect("a read"));
    (first.subtree(top, |view| assert!(view.is_some()))).expect("a read");
    (first.get(aux(1), b"none", &mut out)).expect("a read");
    // Neither does a read that is refused: the scope beside writes there.
    let refused = second.get(aux(B_IN), b"ia", &mut out).expect_err("refused");
    let subtree = sub(B_IN);
    assert_eq!(beside_error(refused), BatchError::MoveBeside { subtree });
    (world.apply_in(&tx, &first, &record(B_IN))).expect("the record is written");
    second.lay_out().expect("it has no write");
    out.clear();
    assert!(second.get(aux(B), b"a", &mut out).expect("a read"));
    assert_eq!(out, vec![1; 3]);
    (world.apply_in(&tx, &second, &record(B))).expect("the record is written");
    second.merge();
    first.merge();
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();

    // An element of the moved Merk was marked pending by a scope: the
    // move knows nothing of the mark.
    let tx = world.begin();
    let batch = rename(&world, &tx, b"b", b"c", &[(B, C), (B_IN, C_IN)]);
    let subtree = sub(B);
    let scope = tx.txn.scope().expect("a scope");
    scope.mark_pending(subtree, b"in").expect("a mark");
    let through = batch.through(&scope);
    let refused = world.apply_in(&tx, &scope, &through).expect_err("refused");
    assert_eq!(beside_error(refused), BatchError::MoveBeside { subtree });
    scope.rollback();
    world.apply(&tx, &batch).expect("the subtree is moved");
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
}

/// The prefix a move took a subtree from is another subtree's once the
/// transaction enters one there. What the transaction then clears and
/// drops under that prefix takes nothing of the subtree that was moved:
/// its records, and those of the subtree below it, are read under the new
/// prefixes by key and by cursor, and are there after the commit.
#[test]
fn a_drop_at_the_old_prefix_takes_nothing_of_the_subtree_that_was_moved() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    world.apply(&tx, &batch).expect("the subtree is moved");
    let again = nested(&world, &tx, b"a", A, A_IN, 90);
    world.apply(&tx, &again).expect("a tree at the old key");
    let moved = |tx: &Tx<'_>| {
        let below = get(tx, aux(B_IN), b"ia").expect("a read");
        let meta = get(tx, model::unit(Column::Meta, B_IN), b"im").expect("a read");
        let own = get(tx, aux(B), b"a").expect("a read");
        assert_eq!(
            (below, meta, own),
            (Some(vec![1; 9]), Some(vec![1; 400]), Some(vec![1; 3]))
        );
        let mut cursor = tx.txn.cursor(aux(B_IN));
        cursor.seek_to_first().expect("a move");
        assert_eq!(cursor.key(), Some(&b"ia"[..]));
        assert!(shown(tx, B_IN).is_some());
    };
    moved(&tx);

    // The new one is cleared, which takes the subtree below it.
    let cleared = subtrees(vec![OwnedSubtree::Clear(sub(A))]);
    world.apply(&tx, &cleared).expect("a clear");
    world.check(&tx);
    moved(&tx);
    assert_eq!(shown(&tx, A_IN), None);
    // And it is dropped with its element.
    let top = world.merk(&tx, TOP, |items| {
        items.remove(&b"a"[..]);
    });
    let dropped = OwnedBatch {
        merks: vec![top],
        subtrees: vec![OwnedSubtree::Drop(sub(A))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &dropped).expect("a drop");
    world.check(&tx);
    moved(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    moved(&reader);
    assert_eq!(shown(&reader, A), None);
}

/// What a subtree holds in its data column goes with it: flat records
/// that the transaction wrote under the old prefix make the subtree one
/// that takes no node under the new prefix (ST1), as under the old one.
/// Once a commit has met the transaction, a write under the new prefix is
/// refused for the move, as a conflict, before it is asked what the
/// subtree holds.
#[test]
fn the_data_column_of_a_moved_subtree_holds_what_it_held() {
    for met in [false, true] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        let data = |name: u8| model::unit(Column::Data, name);
        let node = |name: u8| {
            let items = BTreeMap::from([(b"n".to_vec(), b"a node".to_vec())]);
            OwnedBatch {
                merks: vec![OwnedMerk::change(sub(name), &BTreeMap::new(), &items)],
                ..OwnedBatch::default()
            }
        };
        let tx = world.begin();
        // A subtree with no node under the node `f` of `TOP`, with a flat
        // record in its data column.
        let mut top = world.merk(&tx, TOP, |items| {
            items.insert(b"f".to_vec(), b"a tree with flat data".to_vec());
        });
        top.hosted.insert(b"f".to_vec(), vec![sub(C)]);
        let made = OwnedBatch {
            merks: vec![top],
            records: vec![put(data(C), b"d", b"flat")],
            subtrees: vec![hosted(C, TOP, b"f")],
        };
        world.apply(&tx, &made).expect("a subtree with flat data");
        assert_eq!(
            batch_error(world.apply(&tx, &node(C))),
            BatchError::NotMerk { subtree: sub(C) }
        );
        // It is moved by the transaction that made it, whose own record
        // the host is: the move names no read.
        let batch = rename(&world, &tx, b"f", b"g", &[(C, C_IN)]);
        world.apply(&tx, &batch).expect("the subtree is moved");
        world.check(&tx);
        assert_eq!(
            batch_error(world.apply(&tx, &node(C_IN))),
            BatchError::NotMerk { subtree: sub(C_IN) }
        );
        world.write(&tx, vec![put(data(C_IN), b"e", b"more flat")]);
        world.check(&tx);
        assert_eq!(
            get(&tx, data(C_IN), b"d").expect("a read"),
            Some(b"flat".to_vec())
        );
        if met {
            world.write(&tx, vec![put(aux(1), b"own", b"mine")]);
            let other = world.begin();
            world.write(&other, vec![put(aux(1), b"own", b"theirs")]);
            assert!(committed(world.commit(other, &[&tx])));
            let refused = world.apply(&tx, &node(C_IN)).expect_err("a refusal");
            assert!(moved_by_key(&refused, C_IN), "{refused}");
            assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
        } else {
            assert!(committed(world.commit(tx, &[])));
        }
        world.check_committed();
    }
}

/// A cursor is the first call of the mover after a commit met its move.
/// It reads a version that has the commit, so it takes the commit in and
/// is refused where the move decides, as a read by key is.
#[test]
fn a_cursor_that_is_opened_after_a_commit_met_the_move_is_refused() {
    for over_nodes in [false, true] {
        let (db, versions) = Db::new(96);
        let mut world = World::new(&db, &versions);
        grove(&mut world, b"a", A, A_IN);
        let mover = world.begin();
        let batch = rename(&world, &mover, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
        world.apply(&mover, &batch).expect("the subtree is moved");
        let other = world.begin();
        world.write(&other, vec![put(aux(A), b"f", b"foreign")]);
        assert!(committed(world.commit(other, &[&mover])));

        let refused = match over_nodes {
            true => {
                let mut cursor = mover.txn.merk_cursor(sub(B)).expect("a cursor");
                cursor.seek_to_first().expect_err("a refusal")
            }
            false => (mover.txn.cursor(aux(B)).seek_to_first()).expect_err("a refusal"),
        };
        assert!(moved_by_key(&refused, B), "{refused}");
        assert_eq!(mover.txn.stats().overlapped, 1);
        assert_eq!(world.commit(mover, &[]), Outcome::Conflict);
        world.check_committed();
    }
}

/// D-213 item 3: a move that is stated while a commit has met the
/// transaction is refused as a conflict, through a scope whose writes are
/// laid out as through one that keeps them in the write set.
#[test]
fn a_move_stated_through_a_scope_after_a_commit_met_the_transaction_is_a_conflict() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    world.write(&tx, vec![put(aux(1), b"own", b"mine")]);
    let batch = rename(&world, &tx, b"a", b"b", &[(A, B), (A_IN, B_IN)]);
    let other = world.begin();
    world.write(&other, vec![put(aux(1), b"own", b"theirs")]);
    assert!(committed(world.commit(other, &[&tx])));

    let first = tx.txn.scope().expect("a scope");
    let second = tx.txn.scope().expect("a scope");
    assert!(first.is_laid_out() && !second.is_laid_out());
    for scope in [&second, &first] {
        let refused = world.apply_in(&tx, scope, &batch).expect_err("refused");
        assert!(moved_by_key(&refused, A), "{refused}");
    }
    second.merge();
    first.merge();
    assert_eq!(tx.txn.stats().overlapped, 1);
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
}

/// Case 3 of the interface: a subtree five subtrees deep is moved, with
/// nodes and flat records at every level. Among the subtrees below it
/// are one that was emptied, which has an entry and no node and leaves
/// with the move, and one that nothing was written into, for which the
/// move states a pair that gives no subtree another prefix.
#[test]
fn a_subtree_five_deep_is_moved_with_all_that_is_below_it() {
    const OLD: [u8; 5] = [A, A_IN, C, C_IN, EMPTY_2];
    const NEW: [u8; 5] = [B, B_IN, ROOT, ENTERED + 53, ENTERED + 54];
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    // Three more levels, each under the node `in` of the one above.
    for depth in 2..5 {
        let tx = world.begin();
        let path: Vec<(u8, &[u8])> = (0..depth)
            .map(|at| (OLD[at], if at == 0 { &b"a"[..] } else { &b"in"[..] }))
            .collect();
        let mut batch = edit_at(&world, &tx, &path, |items| {
            items.insert(b"in".to_vec(), b"tree in".to_vec());
        });
        let (parent, name) = (OLD[depth - 1], OLD[depth]);
        (batch.merks[0].hosted).insert(b"in".to_vec(), vec![sub(name)]);
        let below = world.merk(&tx, name, |items| {
            items.insert(b"n".to_vec(), vec![depth as u8; 30]);
        });
        batch.merks.insert(0, below);
        batch.subtrees = vec![hosted(name, parent, b"in")];
        batch.records = vec![
            put(aux(name), b"deep", &[depth as u8; 4]),
            put(
                model::unit(Column::Meta, name),
                b"deep",
                &[depth as u8; 300],
            ),
        ];
        world.apply(&tx, &batch).expect("a level is made");
        assert!(committed(world.commit(tx, &[])));
    }
    emptied(&mut world);

    let tx = world.begin();
    let was = OLD.map(|name| shown(&tx, name));
    let mut pairs: Vec<(u8, u8)> = OLD.into_iter().zip(NEW).collect();
    pairs.push((NONE, NONE_B));
    let batch = rename(&world, &tx, b"a", b"b", &pairs);
    world.apply(&tx, &batch).expect("the subtree is moved");
    world.check(&tx);
    for depth in 0..5 {
        assert_eq!(shown(&tx, OLD[depth]), None, "level {depth}");
        let (above, key): (u8, &[u8]) = match depth {
            0 => (TOP, b"b"),
            _ => (NEW[depth - 1], b"in"),
        };
        let expected = under(&was[depth], above, key);
        assert_eq!(shown(&tx, NEW[depth]), expected, "level {depth}");
    }
    for (depth, name) in NEW.into_iter().enumerate().skip(2) {
        let deep = get(&tx, aux(name), b"deep").expect("a read");
        assert_eq!(deep, Some(vec![depth as u8; 4]));
    }
    for gone in [EMPTY, EMPTY_B, NONE, NONE_B] {
        assert_eq!(shown(&tx, gone), None, "{gone}");
    }
    // A change at the deepest level, with every host above it written.
    let path: Vec<(u8, &[u8])> = (0..5)
        .map(|at| (NEW[at], if at == 0 { &b"b"[..] } else { &b"in"[..] }))
        .collect();
    let change = edit_at(&world, &tx, &path, |items| {
        items.insert(b"late".to_vec(), vec![5; 50]);
    });
    world
        .apply(&tx, &change)
        .expect("a change at the deepest level");
    world.check(&tx);
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    let deep = get(&reader, model::unit(Column::Meta, NEW[4]), b"deep");
    assert_eq!(deep.expect("a read"), Some(vec![4; 300]));
    assert_eq!(db.private_ids(), 0);
}

/// Case 4 of the interface: a tree and an item change places. The node
/// at the tree's old key is put as an item and hosts nothing, the node at
/// the item's key hosts the tree, and one move takes the subtree, with
/// the one below it, to the prefixes under that key.
#[test]
fn a_tree_and_an_item_change_places() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    grove(&mut world, b"a", A, A_IN);
    let tx = world.begin();
    world.edit(&tx, TOP, |items| {
        items.insert(b"i".to_vec(), b"an item".to_vec());
    });
    assert!(committed(world.commit(tx, &[])));

    let tx = world.begin();
    let was = [shown(&tx, A), shown(&tx, A_IN)];
    let mut top = world.merk(&tx, TOP, |items| {
        let (tree, item) = (items[&b"a"[..]].clone(), items[&b"i"[..]].clone());
        items.insert(b"a".to_vec(), item);
        items.insert(b"i".to_vec(), tree);
    });
    top.hosted.insert(b"a".to_vec(), Vec::new());
    top.hosted.insert(b"i".to_vec(), vec![sub(B)]);
    let read = (tx.txn.node(sub(TOP), b"a", handle_of)).expect("the host is read");
    let batch = OwnedBatch {
        merks: vec![top],
        subtrees: vec![moving(b"i", read, &[(A, B), (A_IN, B_IN)])],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the two change places");
    world.check(&tx);
    assert_eq!([shown(&tx, A), shown(&tx, A_IN)], [None, None]);
    assert_eq!(shown(&tx, B), under(&was[0], TOP, b"i"));
    assert_eq!(shown(&tx, B_IN), under(&was[1], B, b"in"));
    let item = (tx
        .txn
        .node(sub(TOP), b"a", |view| view.map(|view| value_of(&view))))
    .expect("a read");
    assert_eq!(item, Some(b"an item".to_vec()));
    assert_eq!(
        get(&tx, aux(B_IN), b"ia").expect("a read"),
        Some(vec![1; 9])
    );
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
}
