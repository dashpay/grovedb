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
/// pair for a subtree that is not entered says nothing.
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
        // What no move decides is read as before, and the committed
        // records are those of the committed state (RD10).
        let top = (case != 5).then(|| b"top".to_vec());
        assert_eq!(get(&mover, aux(TOP), b"t").expect("a read"), top);
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
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
    let reader = world.begin();
    assert_eq!(
        [shown(&reader, EMPTY), shown(&reader, EMPTY_2)],
        [None, None]
    );
    assert_eq!(get(&reader, aux(EMPTY_2), b"f").expect("a read"), None);
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
/// rolled back.
#[test]
fn a_move_that_cannot_be_made_again_is_refused_with_what_stops_it() {
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
    for (unit, key) in [(aux(B), &b"a"[..]), (aux(A), b"a"), (aux(B_IN), b"ia")] {
        let refused = get(&tx, unit, key).expect_err("a refusal");
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

    // A pair may name a subtree that is not entered: it says nothing.
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
#[test]
fn the_data_column_of_a_moved_subtree_holds_what_it_held() {
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
    // It is moved by the transaction that made it, whose own record the
    // host is: the move names no read.
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
    assert!(committed(world.commit(tx, &[])));
    world.check_committed();
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
/// move states a pair that says nothing.
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
