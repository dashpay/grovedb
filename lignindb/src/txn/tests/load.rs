//! Loads: change sets that are whole Merks, laid out by the bulk loader,
//! with their values out of line in the value log before the commit
//! (`crate::txn`, "A load"; TXN-10).

use lignin_tree::fragment::Layout;

use super::{
    model::{self, NodeRec, OwnedBatch, OwnedMerk, OwnedSubtree, Val, Write},
    store::{Db, Outcome, Rng, ENTERED, SIZE, TREE_TYPE},
    *,
};
use crate::commit::{CommitError, Published};

/// The shortest value that the store of these tests puts out of line.
const LONG: usize = lignin_tree::value::InlinePolicy::DEFAULT_THRESHOLD + 1;

/// The keys and values of one Merk.
pub(crate) type Items = BTreeMap<Vec<u8>, Vec<u8>>;

/// The key `index` of the keys that begin with `lead`. The bytes after
/// the lead are mixed, so that the tree the model makes of such keys, by a
/// hash of each, is about as deep as a balanced one.
fn key(lead: u8, index: usize) -> Vec<u8> {
    let mut key = vec![lead];
    key.extend_from_slice(&Rng::new(index as u64).next().to_be_bytes());
    key
}

/// `count` items whose keys begin with `lead`. One value in `long` is out
/// of line, and the others are short.
pub(crate) fn items(rng: &mut Rng, lead: u8, count: usize, long: usize) -> Items {
    let item = |index: usize| {
        let len = match index % long {
            0 => LONG + rng.below(300),
            _ => 8 + rng.below(40),
        };
        (key(lead, index), model::value(rng, len))
    };
    (0..count).map(item).collect()
}

/// How many values of `items` are out of line.
fn long_values(items: &Items) -> usize {
    items.values().filter(|value| value.len() >= LONG).count()
}

/// The change set that hands the Merk of `items` over whole: every node
/// once, a child before its parent, with its value stated, and the root.
pub(crate) fn whole(name: u8, items: &Items) -> OwnedMerk {
    fn below(
        key: &[u8],
        records: &mut BTreeMap<Vec<u8>, NodeRec>,
        out: &mut Vec<(Vec<u8>, NodeRec, bool)>,
    ) {
        let node = records.remove(key).expect("a node of the Merk");
        for link in [&node.left, &node.right].into_iter().flatten() {
            below(&link.key, records, out);
        }
        out.push((key.to_vec(), node, true));
    }
    let (root, mut records) = model::records(items);
    let mut nodes = Vec::new();
    if let Some(root) = &root {
        below(root, &mut records, &mut nodes);
    }
    OwnedMerk {
        subtree: model::subtree(name),
        nodes,
        removed: Vec::new(),
        root: Some(root),
        hosted: BTreeMap::new(),
        stale: BTreeMap::new(),
        anchored: false,
        hints: BTreeMap::new(),
    }
}

/// The change set of [`whole`] with its nodes in another order that has a
/// child before its parent: every leaf first, and then the nodes by their
/// height. No node of it has its parent before every node that is lower
/// has arrived, so the values of all the leaves, a third of the nodes of
/// the tree the model makes, wait for a page at once.
pub(crate) fn whole_by_height(name: u8, items: &Items) -> OwnedMerk {
    let mut merk = whole(name, items);
    let mut heights: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    for (key, node, _) in &merk.nodes {
        let children = [&node.left, &node.right].into_iter().flatten();
        let below = children.map(|link| heights[&link.key]).max();
        heights.insert(key.clone(), below.map_or(1, |below| below + 1));
    }
    merk.nodes.sort_by_key(|(key, ..)| heights[key]);
    let leaves = heights.values().filter(|height| **height == 1).count();
    let first =
        (merk.nodes.iter()).take_while(|(_, node, _)| node.left.is_none() && node.right.is_none());
    assert_eq!(first.count(), leaves, "every leaf comes first");
    assert!(leaves * 4 > merk.nodes.len(), "{leaves} leaves");
    merk
}

/// A batch of `merks`, each with the record that a change of its root
/// writes beside it.
pub(crate) fn batch_of(merks: Vec<OwnedMerk>) -> OwnedBatch {
    let records = merks.iter().filter_map(World::root_record).collect();
    OwnedBatch {
        merks,
        records,
        ..OwnedBatch::default()
    }
}

/// Applies `batch` through a scope of `tx` that merges, with the change
/// sets of the Merks `whole` said to be whole, and tells the model.
fn load<'s>(
    world: &World<'s>,
    tx: &Tx<'s>,
    batch: &OwnedBatch,
    whole: &[u8],
) -> Result<Applied, TxnError> {
    let whole: Vec<Subtree> = whole.iter().map(|name| model::subtree(*name)).collect();
    let scope = tx.txn.scope()?;
    let first_write = world.latest().get();
    let applied = batch.loading(&whole, |batch| scope.apply(batch));
    scope.merge();
    let applied = applied?;
    world.note(tx, batch, first_write);
    Ok(applied)
}

/// Commits `tx`, which no commit has met, in the store and in the model,
/// and returns what the commit did.
fn commit<'s>(world: &mut World<'s>, tx: Tx<'s>) -> Box<Published> {
    let committer = world.db.committer(world.versions);
    let published = world.db.commit_as(&committer, &tx.txn, None);
    let published = published.expect("a commit").expect("a version");
    let writes: Vec<Write> = (tx.model.borrow().writes.iter())
        .map(|(write, ..)| write.clone())
        .collect();
    world.model.commit(&writes);
    assert_eq!(
        tx.txn.loaded_runs(),
        0,
        "a commit ends the hold on its values"
    );
    published
}

/// How many values the cells of the latest version hold in the value log,
/// as the store counts them, and that no count is known to be wrong.
fn counted(db: &Db) -> u64 {
    let counts = db.values.counts();
    assert!(counts.iter().all(|count| !count.miscounted), "{counts:?}");
    counts.iter().map(|count| count.values).sum()
}

/// What `tx` reads of every node of the Merk `name`, by key.
fn nodes_of(tx: &Tx<'_>, name: u8, items: &Items) -> Vec<NodeRec> {
    let read = |key: &Vec<u8>| {
        let shown = tx.txn.node(model::subtree(name), key, |view| {
            let view = view.expect("a node of the Merk");
            let link = |side| {
                view.link(side).map(|link| model::LinkRec {
                    key: Vec::new(),
                    hash: *link.hash,
                    heights: link.heights,
                    aggregate: link.aggregate.to_vec(),
                })
            };
            NodeRec {
                value: value_of(&view),
                feature: view.feature().to_vec(),
                value_hash: *view.value_hash(),
                kv_hash: *view.kv_hash(),
                left: link(lignin_format::tree_page::Side::Left),
                right: link(lignin_format::tree_page::Side::Right),
            }
        });
        shown.expect("a read")
    };
    items.keys().map(read).collect()
}

/// TXN-10's first "done when", in the engine: a Merk that is handed over
/// whole has the records it has when it is written node by node, and the
/// pages the loader gives.
#[test]
fn a_merk_handed_over_whole_has_the_records_of_one_written_node_by_node() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(10);
    let merk = items(&mut rng, b'k', 6_000, 4);
    let long = long_values(&merk);

    let tx = world.begin();
    let handed = whole(1, &merk);
    let applied = load(&world, &tx, &batch_of(vec![handed.clone()]), &[1]).expect("a load");
    // The loader laid it out: every node, in pages that are full.
    let counters = applied.load.expect("the loader laid the Merk out");
    assert_eq!(counters.nodes, merk.len() as u64);
    assert_eq!(counters.values, long as u64);
    assert!(counters.pages > 40, "{counters:?}");
    let fill = counters.fill_percent(&Layout::proposed(SIZE));
    assert!(fill >= 90.0, "the pages of a load are {fill:.1}% full");
    // No value out of line is with the transaction: each is in the value
    // log, and the write set has where.
    let stats = tx.txn.stats();
    assert_eq!(stats.value_pages, 0);
    assert_eq!(stats.tree_pages as u64, counters.pages);
    let bytes: usize = merk.values().map(Vec::len).sum();
    let long_bytes: usize = (merk.values().filter(|value| value.len() >= LONG))
        .map(Vec::len)
        .sum();
    assert!(
        stats.write_set_bytes < bytes - long_bytes + merk.len() * 400,
        "the write set holds {} bytes for {bytes} bytes of values, {long_bytes} out of line",
        stats.write_set_bytes
    );
    assert_eq!(tx.txn.loaded_runs(), 1);
    // The handles are those of the nodes, in the order of the change set.
    assert_eq!(applied.nodes.len(), handed.nodes.len());
    for (handle, (key, node, _)) in applied.nodes.iter().zip(&handed.nodes).step_by(53) {
        let followed = tx.txn.follow(model::subtree(1), *handle, key, |view| {
            let view = view.expect("the node the handle names");
            assert_eq!(view.handle(), Some(*handle), "the handle is followed");
            node.check(key, &view);
        });
        followed.expect("a read");
    }
    assert_eq!(tx.txn.stats().keyed_reads, 0);

    // The same Merk under another name, written as any Merk is: a few
    // nodes with each batch.
    let keys: Vec<&Vec<u8>> = merk.keys().collect();
    for chunk in keys.chunks(500) {
        world.edit(&tx, 2, |items| {
            items.extend(chunk.iter().map(|key| ((*key).clone(), merk[*key].clone())));
        });
    }
    world.check(&tx);
    assert_eq!(nodes_of(&tx, 1, &merk), nodes_of(&tx, 2, &merk));
    let roots = [1, 2].map(|name| {
        let root = tx.txn.subtree(model::subtree(name), |view| {
            view.expect("the subtree is entered")
                .root_key
                .map(<[u8]>::to_vec)
        });
        root.expect("a read")
    });
    assert_eq!(roots[0], roots[1]);
    assert_eq!(roots[0], handed.root.clone().flatten());

    // The commit takes the values of the load over, and carries those of
    // the other Merk in its group.
    let published = commit(&mut world, tx);
    assert_eq!(published.acquired, long);
    assert_eq!(published.released, 0);
    assert!(published.value_bytes > 0);
    assert_eq!(counted(&db), long as u64);
    world.check_committed();
    let reader = world.begin();
    assert_eq!(nodes_of(&reader, 1, &merk), nodes_of(&reader, 2, &merk));
}

/// A load alone: its commit carries no value in its group, and every
/// value the cells hold is counted in its file.
#[test]
fn the_commit_of_a_load_carries_no_value_and_counts_each() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(11);
    let merk = items(&mut rng, b'v', 900, 2);
    let long = long_values(&merk);
    let tx = world.begin();
    load(&world, &tx, &batch_of(vec![whole(1, &merk)]), &[1]).expect("a load");
    let published = commit(&mut world, tx);
    assert_eq!((published.acquired, published.released), (long, 0));
    assert_eq!((published.value_bytes, published.value_pages), (0, 0));
    assert_eq!(counted(&db), long as u64);
    world.check_committed();

    // A commit after it writes a loaded value over, and takes a node out:
    // each releases the pointer the load's commit acquired.
    let tx = world.begin();
    world.edit(&tx, 1, |items| {
        items.insert(key(b'v', 0), b"short".to_vec());
        items.remove(&key(b'v', 2));
    });
    let published = commit(&mut world, tx);
    assert_eq!(published.acquired, 0);
    assert!(published.released >= 2, "{}", published.released);
    world.check_committed();
}

/// What a rebase and a rollback to a mark do with loaded pages: the load
/// is laid out again by the loader, and its values are appended anew for
/// the pages their cells are in now. The records are what they were, and
/// the commit counts each value once. So it is in whatever order the
/// nodes are stated: a child before its parent as a walk of the tree
/// finds them, and every leaf first, where the values of a third of the
/// nodes wait for their pages at once, the first time from the batch and
/// then from the value log.
#[test]
fn frames_that_are_made_again_lay_a_load_out_again() {
    for stated in [whole, whole_by_height] {
        frames_are_made_again(stated);
    }
}

fn frames_are_made_again(stated: fn(u8, &Items) -> OwnedMerk) {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(12);
    let merk = items(&mut rng, b'r', 1_500, 3);
    let long = long_values(&merk);

    let tx = world.begin();
    load(&world, &tx, &batch_of(vec![stated(1, &merk)]), &[1]).expect("a load");
    let pages = tx.txn.stats().tree_pages;
    // A commit of another transaction, which meets no write of this one.
    let landed = world.edit_outside(
        9,
        |items| drop(items.insert(b"k".to_vec(), b"v".to_vec())),
        &[&tx],
    );
    assert!(matches!(landed, Outcome::Committed(_)));
    world.check(&tx);
    let stats = tx.txn.stats();
    assert_eq!((stats.rebuilds, stats.overlapped), (1, 0));
    assert_eq!(stats.tree_pages, pages, "the same nodes fill as many pages");
    assert_eq!(stats.value_pages, 0);

    // A write after a mark, and a rollback to the mark.
    let mark = tx.txn.mark();
    let writes = tx.model.borrow().writes.len();
    world.edit(&tx, 1, |items| {
        items.insert(key(b'r', 0), vec![7; LONG + 5]);
        items.insert(key(b'r', 9_999), vec![8; LONG]);
    });
    world.check(&tx);
    assert!(tx.txn.rewind(mark));
    tx.model.borrow_mut().writes.truncate(writes);
    world.check(&tx);
    assert_eq!(tx.txn.stats().rebuilds, 2);
    assert_eq!(tx.txn.stats().tree_pages, pages);

    // Three copies of each value are in the value log, and the cells hold
    // one of each: that one is counted.
    let published = commit(&mut world, tx);
    assert_eq!((published.acquired, published.released), (long, 0));
    assert_eq!((published.value_bytes, published.value_pages), (0, 0));
    assert_eq!(counted(&db), long as u64);
    let long_bytes: usize = (merk.values().map(Vec::len))
        .filter(|len| *len >= LONG)
        .sum();
    let files = db.values.files_named().expect("the files");
    let appended: u64 = files.iter().map(|file| file.valid_len()).sum();
    assert!(
        appended >= 3 * long_bytes as u64,
        "{appended} bytes hold three copies of {long_bytes}"
    );
    world.check_committed();
}

/// The nodes of a load are the transaction's own from the load on: a
/// later write of it changes them as it changes any node, and the commit
/// acquires the values that cells still hold and releases none that it
/// never counted.
#[test]
fn a_write_after_a_load_changes_its_nodes() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(13);
    let merk = items(&mut rng, b'w', 700, 2);
    let long = long_values(&merk);
    let tx = world.begin();
    load(&world, &tx, &batch_of(vec![whole(1, &merk)]), &[1]).expect("a load");
    world.edit(&tx, 1, |items| {
        // A loaded value out of line is written over by a short one and
        // by a long one, a node with one is taken out, and nodes are
        // added, which cut the pages of the load.
        items.insert(key(b'w', 0), b"short".to_vec());
        items.insert(key(b'w', 2), vec![3; LONG + 40]);
        items.remove(&key(b'w', 4));
        for index in 0..300 {
            items.insert(key(b'x', index), vec![index as u8; 24]);
        }
    });
    world.check(&tx);
    let published = commit(&mut world, tx);
    assert_eq!(published.released, 0, "nothing was counted before");
    // What the write gave a cell anew travels in the group, and so does
    // the value of each loaded cell that was carried to another page.
    assert!(published.acquired <= long - 3, "{}", published.acquired);
    assert!(published.value_pages > 0);
    assert!(counted(&db) == published.acquired as u64);
    world.check_committed();
}

/// A rollback gives the values of a load up: no count includes one, and
/// the transaction holds no file.
#[test]
fn a_rollback_gives_up_the_values_of_a_load() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(14);
    let merk = items(&mut rng, b'g', 400, 2);
    let tx = world.begin();
    load(&world, &tx, &batch_of(vec![whole(1, &merk)]), &[1]).expect("a load");
    assert_eq!(tx.txn.loaded_runs(), 1);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();
    assert_eq!(tx.txn.loaded_runs(), 0);
    assert_eq!(tx.txn.stats(), TxnStats::default());
    world.check(&tx);

    // A rollback to a mark from before the load takes the load out, and
    // what is left commits.
    world.edit(&tx, 2, |items| {
        drop(items.insert(b"a".to_vec(), vec![1; 9]))
    });
    let mark = tx.txn.mark();
    let writes = tx.model.borrow().writes.len();
    load(&world, &tx, &batch_of(vec![whole(1, &merk)]), &[1]).expect("a load");
    assert!(tx.txn.rewind(mark));
    tx.model.borrow_mut().writes.truncate(writes);
    world.check(&tx);
    let published = commit(&mut world, tx);
    assert_eq!((published.acquired, published.released), (0, 0));
    assert_eq!(counted(&db), 0);
    world.check_committed();

    // The Merk is as it was: it can be loaded.
    let tx = world.begin();
    load(&world, &tx, &batch_of(vec![whole(1, &merk)]), &[1]).expect("a load");
    let published = commit(&mut world, tx);
    assert_eq!(published.acquired, long_values(&merk));
    world.check_committed();
}

/// A node of a load hosts a Merk of the same batch that is a load too,
/// one that an earlier write made, and one with no node. And a node that
/// is written as any other hosts a load.
#[test]
fn the_nodes_of_a_load_host_merks() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(15);
    let (below, before) = (ENTERED + 1, ENTERED + 2);
    let (empty, under) = (ENTERED + 3, ENTERED + 4);
    let host = |name: u8| key(b'h', name as usize);

    // The Merk that is there before, with no host yet: it is entered with
    // the node that will host it named.
    let tx = world.begin();
    let enters = [below, before, empty].map(|name| {
        OwnedSubtree::Enter(
            model::subtree(name),
            Some((model::subtree(1), host(name), 0)),
        )
    });
    let mut first = batch_of(vec![world.merk(&tx, before, |items| {
        items.extend(items_of(&mut rng, b'b', 30));
    })]);
    first.merks[0].anchored = true;
    first.subtrees = enters.to_vec();
    world.apply(&tx, &first).expect("the Merks are entered");

    // The host Merk and the Merk below it, both whole, the host first in
    // the batch: the loader takes the hosted one first.
    let mut parent = items(&mut rng, b'p', 300, 5);
    for name in [below, before, empty] {
        parent.insert(host(name), format!("tree:{name}").into_bytes());
    }
    let mut parent = whole(1, &parent);
    for name in [below, before, empty] {
        parent.hosted.insert(host(name), vec![model::subtree(name)]);
    }
    let child = whole(below, &items(&mut rng, b'c', 200, 3));
    let batch = batch_of(vec![parent, child]);
    let applied = load(&world, &tx, &batch, &[1, below]).expect("a load");
    assert_eq!(applied.load.expect("two loads").nodes, 503);
    world.check(&tx);
    let root = |tx: &Tx<'_>, name: u8| {
        let shown = tx.txn.subtree(model::subtree(name), |view| {
            view.expect("the subtree is entered")
                .root_key
                .map(<[u8]>::to_vec)
        });
        shown.expect("a read")
    };
    assert!(root(&tx, below).is_some() && root(&tx, before).is_some());
    assert_eq!(root(&tx, empty), None);

    // A node that is written as any other, in a Merk that has nodes,
    // hosts a load.
    let enter = OwnedSubtree::Enter(
        model::subtree(under),
        Some((model::subtree(before), host(under), 0)),
    );
    let mut over = world.merk(&tx, before, |items| {
        items.insert(host(under), b"tree".to_vec());
    });
    over.hosted.insert(host(under), vec![model::subtree(under)]);
    over.anchored = true;
    // And a node of that load hosts a load in turn: the root of the
    // innermost is named by the cell of its host, which the write to the
    // tree pages does not hold.
    let deep = ENTERED + 5;
    let enter_deep = OwnedSubtree::Enter(
        model::subtree(deep),
        Some((model::subtree(under), host(deep), 0)),
    );
    let mut middle = items(&mut rng, b'u', 150, 2);
    middle.insert(host(deep), b"tree".to_vec());
    let mut middle = whole(under, &middle);
    middle.hosted.insert(host(deep), vec![model::subtree(deep)]);
    let innermost = whole(deep, &items(&mut rng, b'd', 90, 2));
    let mut batch = batch_of(vec![over, middle, innermost]);
    batch.subtrees = vec![enter, enter_deep];
    load(&world, &tx, &batch, &[under, deep]).expect("loads below a node that is written");
    world.check(&tx);
    // A write after it gives the innermost Merk more nodes and another
    // root, and writes the node that hosts it, a cell of a load, which
    // names it again. The Merk of that node stops below its own host, so
    // its root stays in its page (`tree-layout.md` T3).
    let grown = world.merk(&tx, deep, |items| {
        items.extend(items_of(&mut rng, b'e', 200));
    });
    assert!(grown.root.is_some(), "the innermost Merk has another root");
    let mut above = world.merk(&tx, under, |items| {
        items.insert(host(deep), b"tree, grown".to_vec());
    });
    assert!(above.root.is_none(), "the root is the node it was");
    above.hosted.insert(host(deep), vec![model::subtree(deep)]);
    above.anchored = true;
    world
        .apply(&tx, &batch_of(vec![above, grown]))
        .expect("a write to two Merks that were loaded");
    world.check(&tx);

    // The checker follows each host to the root it names, and compares
    // it with the directory: the four Merks with a node, and not the one
    // with none.
    commit(&mut world, tx);
    world.check_committed();
    let report = db.verify_still();
    assert!(db.unsound(&report).is_empty(), "{report}");
    assert_eq!(report.census.hosted_roots, 4, "{report}");
}

fn items_of(rng: &mut Rng, lead: u8, count: usize) -> Items {
    items(rng, lead, count, usize::MAX)
}

/// What a change set that says it is the whole Merk is refused for, with
/// nothing changed.
#[test]
fn a_change_set_that_is_no_whole_merk_is_refused() {
    let (db, versions) = Db::new(64);
    let world = World::new(&db, &versions);
    let mut rng = Rng::new(16);
    let merk = items_of(&mut rng, b'f', 40);
    let tx = world.begin();
    let refused = |merks: Vec<OwnedMerk>, whole: &[u8]| {
        let error = load(&world, &tx, &batch_of(merks), whole).expect_err("no load");
        assert_eq!(tx.txn.stats(), TxnStats::default(), "{error}");
        assert_eq!(tx.txn.loaded_runs(), 0);
        match error {
            TxnError::Batch(BatchError::NotWhole { subtree, fault }) => {
                assert!(error.to_string().contains(&fault.to_string()));
                (subtree, fault)
            }
            other => panic!("refused for {other}"),
        }
    };
    let one = model::subtree(1);
    let good = || whole(1, &merk);

    // A child after its parent: the order of the keys is not that order.
    let mut by_key = good();
    by_key.nodes.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(refused(vec![by_key], &[1]), (one, WholeFault::ChildLater));
    // A node that keeps its value.
    let mut keeps = good();
    keeps.nodes[3].2 = false;
    assert_eq!(refused(vec![keeps], &[1]), (one, WholeFault::KeepsValue));
    // A hash that is left to the commit.
    let mut stale = good();
    let last = stale.nodes[5].0.clone();
    stale.stale.insert(last, lignin_tree::hash::Stale::ALL);
    assert_eq!(refused(vec![stale], &[1]), (one, WholeFault::StaleHash));
    // A key that is removed.
    let mut removes = good();
    removes.removed.push(b"gone".to_vec());
    assert_eq!(refused(vec![removes], &[1]), (one, WholeFault::Removes));
    // A root that is kept, one that is no node, and one that is not the
    // top of the tree.
    for root in [None, Some(None), Some(Some(b"none".to_vec()))] {
        let mut rootless = good();
        rootless.root = root;
        assert_eq!(refused(vec![rootless], &[1]), (one, WholeFault::NoRoot));
    }
    let mut low = good();
    low.root = Some(Some(low.nodes[0].0.clone()));
    assert_eq!(refused(vec![low], &[1]), (one, WholeFault::NoRoot));
    // Two trees: the root and a node that nothing links.
    let mut two = good();
    let top = two.nodes.len() - 1;
    two.nodes[top].1.left = None;
    assert_eq!(refused(vec![two], &[1]), (one, WholeFault::NotBelowRoot));
    // A node that hosts a Merk which the batch writes node by node.
    let mut hosts = good();
    hosts
        .hosted
        .insert(hosts.nodes[0].0.clone(), vec![model::subtree(2)]);
    let other = whole(2, &merk);
    assert_eq!(
        refused(vec![hosts.clone(), other.clone()], &[1]),
        (one, WholeFault::HostsWritten)
    );
    // Two loads that host each other, and one that hosts itself.
    let mut back = other.clone();
    back.hosted.insert(back.nodes[0].0.clone(), vec![one]);
    let (_, fault) = refused(vec![hosts.clone(), back], &[1, 2]);
    assert_eq!(fault, WholeFault::HostsItself);
    let mut own = good();
    own.hosted.insert(own.nodes[0].0.clone(), vec![one]);
    assert_eq!(refused(vec![own], &[1]), (one, WholeFault::HostsItself));

    // A Merk has one host. A node of a load that hosts a Merk twice, two
    // nodes of a load, two loads, and a load beside a node that is written
    // as any other: none of these cells is among those the write to the
    // tree pages holds, which would refuse the second host.
    let (two, three) = (model::subtree(2), model::subtree(3));
    let mut twice = good();
    twice
        .hosted
        .insert(twice.nodes[0].0.clone(), vec![two, two]);
    assert_eq!(refused(vec![twice], &[1]), (one, WholeFault::HostedTwice));
    let mut nodes = hosts.clone();
    nodes.hosted.insert(nodes.nodes[1].0.clone(), vec![two]);
    assert_eq!(refused(vec![nodes], &[1]), (one, WholeFault::HostedTwice));
    let mut second = whole(3, &merk);
    second.hosted.insert(second.nodes[0].0.clone(), vec![two]);
    assert_eq!(
        refused(vec![hosts.clone(), second], &[1, 3]),
        (three, WholeFault::HostedTwice)
    );
    let mut written = world.merk(&tx, 4, |items| {
        items.insert(b"host".to_vec(), b"tree".to_vec());
    });
    written.hosted.insert(b"host".to_vec(), vec![two]);
    assert_eq!(
        refused(vec![written, other, hosts], &[2, 1]),
        (one, WholeFault::HostedTwice)
    );

    // A change set with no node loads nothing, whatever it says and
    // whatever it removes: it is taken as any other.
    let mut removes = good();
    (removes.nodes, removes.root) = (Vec::new(), None);
    removes.removed.push(b"no such key".to_vec());
    let applied = load(&world, &tx, &batch_of(vec![removes]), &[1]).expect("a delete");
    assert_eq!(applied.load, None);
    tx.txn.rollback().expect("no scope is open");
    tx.forget();

    // A Merk that has a root is no Merk to load, before and after the
    // transaction's own write of it.
    world.edit(&tx, 3, |items| {
        drop(items.insert(b"a".to_vec(), vec![1; 4]))
    });
    let error = load(&world, &tx, &batch_of(vec![whole(3, &merk)]), &[3]).expect_err("no load");
    let has_root = BatchError::NotWhole {
        subtree: model::subtree(3),
        fault: WholeFault::HasRoot,
    };
    assert!(matches!(error, TxnError::Batch(refused) if refused == has_root));
    world.check(&tx);

    // A change set with no node loads nothing, whatever it says.
    let nothing = OwnedMerk {
        nodes: Vec::new(),
        root: None,
        ..good()
    };
    let applied = load(&world, &tx, &batch_of(vec![nothing]), &[1]).expect("nothing is written");
    assert_eq!(applied.load, None);
}

/// What a load is to the conflict rule: the puts of its nodes. Two loads
/// of one Merk meet in each of them, and in the record that every first
/// writer of a Merk writes beside it.
#[test]
fn a_load_is_the_puts_of_its_nodes_to_the_conflict_rule() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(17);
    let merk = items(&mut rng, b'c', 300, 3);

    // Two transactions load one Merk. The first commits. The second is
    // met in the record that every first writer of a Merk writes (GM3).
    let (first, second) = (world.begin(), world.begin());
    load(&world, &first, &batch_of(vec![whole(1, &merk)]), &[1]).expect("a load");
    load(&world, &second, &batch_of(vec![whole(1, &merk)]), &[1]).expect("a load");
    assert!(matches!(
        world.commit(first, &[&second]),
        Outcome::Committed(_)
    ));
    // The next operation of the second takes the commit in: every node
    // of its load is met, and the record beside them.
    world.check(&second);
    let met = second.txn.stats().overlapped;
    assert!(met > merk.len(), "{met} entries are met");
    assert_eq!(world.commit(second, &[]), Outcome::Conflict);
    assert_eq!(counted(&db), long_values(&merk) as u64);
}

/// Applies the load `batch` of the Merk `name` through a scope of `tx`
/// that is laid out and merges, or, with `by_key`, through one that is
/// open beside another and keeps its writes in the write set. Those two
/// scopes are returned open, to be merged: the load is laid out then.
fn take<'t, 's>(
    world: &World<'s>,
    tx: &'t Tx<'s>,
    (batch, name): (&OwnedBatch, u8),
    by_key: bool,
) -> Result<Vec<Scope<'t, 's>>, TxnError> {
    if !by_key {
        return load(world, tx, batch, &[name]).map(|_| Vec::new());
    }
    let first = tx.txn.scope()?;
    let beside = tx.txn.scope()?;
    let first_write = world.latest().get();
    let applied = batch.loading(&[model::subtree(name)], |batch| beside.apply(batch));
    match applied {
        Ok(applied) => {
            assert!(applied.load.is_none(), "the load was taken by key");
            world.note(tx, batch, first_write);
            Ok(vec![first, beside])
        }
        Err(refused) => {
            first.merge();
            beside.merge();
            Err(refused)
        }
    }
}

/// "The Merk is entered and has no root" is a precondition of a load that
/// holds from the operation that takes it to its commit (LD6). Another
/// transaction commits a first node of the Merk, under a key that the
/// load does not hold and with no record beside it:
///
/// - after the transaction looked at the subtree and before it hands the
///   load over: the load is refused where it is taken, as a conflict
///   (`BatchError::MerkTaken`), and the batch adds nothing to the write
///   set. So it is where the other commit drops the subtree. A
///   transaction that did not look, or looked again since, is refused
///   too, and not as a conflict: the Merk has a root, as far as it knows
///   from before;
/// - after the load was taken: the load is a range over the nodes of its
///   subtree, the commit meets it, and the commit of the load ends with
///   the conflict;
/// - before the load was taken, by a transaction that commits after the
///   load: that transaction is met by the range of the load, and ends with
///   the conflict.
///
/// And a load into a subtree that is not entered is refused where it is
/// taken: one that never was, and one below a subtree that the batch of
/// the load drops. A subtree that the batch drops and enters again takes
/// the load, and one that the batch enters while it is entered does not,
/// nor one that it enters under a host that is not entered. A commit
/// that deletes a range with no key in it from the Merk does not meet the
/// load. So it is where the load is laid out and where it is taken by
/// key, by a scope that is still open beside another while the other
/// transaction commits, with one difference: by key a batch that drops
/// or clears a subtree is not taken with a load (`LoadBeside`), and no
/// refusal of a batch that drops or clears is told as a conflict by a
/// look.
#[test]
fn a_load_holds_that_its_merk_has_no_root_from_the_write_to_the_commit() {
    for by_key in [false, true] {
        let (db, versions) = Db::new(256);
        let mut world = World::new(&db, &versions);
        let merk = items(&mut Rng::new(31), b'q', 120, 3);
        let alone = |name: u8| OwnedBatch {
            merks: vec![whole(name, &merk)],
            ..OwnedBatch::default()
        };
        fn first_node<'s>(world: &World<'s>, tx: &Tx<'s>, name: u8) {
            let mut batch = OwnedBatch::default();
            batch.merks.push(world.merk(tx, name, |items| {
                items.insert(b"zz-another-key".to_vec(), vec![5; 12]);
            }));
            world.apply(tx, &batch).expect("a first node");
        }
        let ends = |tx: &Tx<'_>| {
            let committer = db.committer(&versions);
            db.commit_as(&committer, &tx.txn, None).map(drop)
        };

        // A look, the other commit, the write.
        let tx = world.begin();
        let rootless = tx.txn.subtree(model::subtree(1), |view| {
            view.expect("the subtree is entered").root_key.is_none()
        });
        assert!(rootless.expect("a look"), "by key: {by_key}");
        let other = world.begin();
        first_node(&world, &other, 1);
        assert!(matches!(world.commit(other, &[&tx]), Outcome::Committed(_)));
        let refused = take(&world, &tx, (&alone(1), 1), by_key)
            .map(drop)
            .expect_err("a root");
        let taken = |name: u8| BatchError::MerkTaken {
            subtree: model::subtree(name),
        };
        let beside_a_leave = |name: u8| BatchError::LoadBeside {
            subtree: model::subtree(name),
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == taken(1)),
            "by key: {by_key}: {refused}"
        );
        assert!(refused.is_conflict());
        world.check(&tx);
        assert_eq!(tx.txn.stats().overlapped, 0);
        // The same where the other commit took the subtree out.
        let rootless = tx.txn.subtree(model::subtree(5), |view| {
            view.expect("the subtree is entered").root_key.is_none()
        });
        assert!(rootless.expect("a look"), "by key: {by_key}");
        let dropped = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(model::subtree(5))],
            ..OwnedBatch::default()
        };
        let dropped = world.write_outside(&dropped, &[&tx]);
        assert!(matches!(dropped, Outcome::Committed(_)));
        // The transaction holds the key set of that commit, which names
        // the subtree as one it dropped: the refusal is that of any
        // write into a subtree that a commit took (CF10).
        let refused = take(&world, &tx, (&alone(5), 5), by_key)
            .map(drop)
            .expect_err("no entry");
        let took = BatchError::ValueTaken {
            subtree: model::subtree(5),
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == took),
            "by key: {by_key}: {refused}"
        );
        assert!(refused.is_conflict());
        // A transaction that holds no key set yet is told by its look.
        let fresh = world.begin();
        let rootless = fresh.txn.subtree(model::subtree(7), |view| {
            view.expect("the subtree is entered").root_key.is_none()
        });
        assert!(rootless.expect("a look"), "by key: {by_key}");
        let dropped = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(model::subtree(7))],
            ..OwnedBatch::default()
        };
        let dropped = world.write_outside(&dropped, &[&tx, &fresh]);
        assert!(matches!(dropped, Outcome::Committed(_)));
        let refused = take(&world, &fresh, (&alone(7), 7), by_key)
            .map(drop)
            .expect_err("no entry");
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == taken(7)),
            "by key: {by_key}: {refused}"
        );
        assert!(refused.is_conflict());
        world.check(&fresh);
        assert_eq!(fresh.txn.stats().overlapped, 0);
        // A batch that drops or clears a subtree is told no conflict by
        // a look, whatever it clears: it may be why itself (LD6). Laid
        // out, the refusal is the one of a subtree that is not entered.
        // By key a load is taken beside no drop and no clear at all.
        let mut clearing = alone(7);
        clearing.subtrees = vec![OwnedSubtree::Clear(model::subtree(6))];
        let refused = take(&world, &fresh, (&clearing, 7), by_key)
            .map(drop)
            .expect_err("no entry");
        let plain = match by_key {
            true => beside_a_leave(7),
            false => BatchError::NotEntered {
                subtree: model::subtree(7),
            },
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == plain),
            "by key: {by_key}: {refused}"
        );
        assert!(!refused.is_conflict());
        world.check(&fresh);
        // A subtree that the transaction took out itself is none that a
        // commit took, whatever was committed since its look.
        let own = world.begin();
        let rootless = own.txn.subtree(model::subtree(8), |view| {
            view.expect("the subtree is entered").root_key.is_none()
        });
        assert!(rootless.expect("a look"), "by key: {by_key}");
        let dropped = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(model::subtree(8))],
            ..OwnedBatch::default()
        };
        world.apply(&own, &dropped).expect("its own drop");
        let beside = world.edit_outside(
            9,
            |items| drop(items.insert(b"k".to_vec(), b"v".to_vec())),
            &[&tx, &fresh, &own],
        );
        assert!(matches!(beside, Outcome::Committed(_)));
        let refused = take(&world, &own, (&alone(8), 8), by_key)
            .map(drop)
            .expect_err("no entry");
        let own_drop = BatchError::NotEntered {
            subtree: model::subtree(8),
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == own_drop),
            "by key: {by_key}: {refused}"
        );
        assert!(!refused.is_conflict());
        own.txn.rollback().expect("no scope is open");
        own.forget();
        // A transaction that looks again finds the root, and one that
        // never looked knows of no time when the Merk had none: neither
        // is told of a conflict.
        let rooted = BatchError::NotWhole {
            subtree: model::subtree(1),
            fault: WholeFault::HasRoot,
        };
        let has_root = tx.txn.subtree(model::subtree(1), |view| {
            view.expect("the subtree is entered").root_key.is_some()
        });
        assert!(has_root.expect("a look"), "by key: {by_key}");
        let blind = world.begin();
        for tx in [&tx, &blind] {
            let refused = take(&world, tx, (&alone(1), 1), by_key)
                .map(drop)
                .expect_err("a root");
            assert!(
                matches!(&refused, TxnError::Batch(error) if *error == rooted),
                "by key: {by_key}: {refused}"
            );
            assert!(!refused.is_conflict());
            world.check(tx);
        }

        // A subtree that is not entered takes no load.
        let lost = ENTERED + 9;
        let refused = take(&world, &tx, (&alone(lost), lost), by_key)
            .map(drop)
            .expect_err("no entry");
        let not_entered = BatchError::NotEntered {
            subtree: model::subtree(lost),
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == not_entered),
            "by key: {by_key}: {refused}"
        );
        world.check(&tx);
        assert!(matches!(ends(&tx), Ok(())), "nothing was taken");

        // Nor does a subtree below one that the batch of the load drops.
        let (above, below) = (ENTERED + 3, ENTERED + 4);
        let host = key(b'h', 3);
        let tx = world.begin();
        let mut outer = world.merk(&tx, above, |items| {
            items.insert(host.clone(), b"tree".to_vec());
        });
        outer
            .hosted
            .insert(host.clone(), vec![model::subtree(below)]);
        let mut batch = batch_of(vec![outer]);
        batch.subtrees = vec![
            OwnedSubtree::Enter(model::subtree(above), None),
            OwnedSubtree::Enter(
                model::subtree(below),
                Some((model::subtree(above), host.clone(), 0)),
            ),
        ];
        world
            .apply(&tx, &batch)
            .expect("a Merk and an empty one below it");
        assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));
        let tx = world.begin();
        let mut dropping = alone(below);
        dropping.subtrees = vec![OwnedSubtree::Drop(model::subtree(above))];
        let refused = take(&world, &tx, (&dropping, below), by_key)
            .map(drop)
            .expect_err("it went with the subtree above");
        let gone = match by_key {
            true => beside_a_leave(below),
            false => BatchError::NotEntered {
                subtree: model::subtree(below),
            },
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == gone),
            "by key: {by_key}: {refused}"
        );
        assert!(!refused.is_conflict());
        world.check(&tx);

        // A subtree that the batch drops and enters again takes the load,
        // wherever the two statements stand: a batch drops before it
        // enters. By key the batch is not taken, with its drop, and
        // nothing of it is in the write set.
        let mut again = alone(4);
        again.subtrees = vec![
            OwnedSubtree::Enter(model::subtree(4), None),
            OwnedSubtree::Drop(model::subtree(4)),
        ];
        if by_key {
            let refused = take(&world, &tx, (&again, 4), true)
                .map(drop)
                .expect_err("beside a drop");
            assert!(
                matches!(&refused, TxnError::Batch(error) if *error == beside_a_leave(4)),
                "{refused}"
            );
            assert!(!refused.is_conflict());
            world.check(&tx);
            assert_eq!(tx.txn.stats().writes, 0);
        }
        take(&world, &tx, (&again, 4), false).expect("a load");
        world.check(&tx);
        let published = commit(&mut world, tx);
        assert_eq!(published.acquired, long_values(&merk));

        // A subtree that is entered is not entered again, and a subtree
        // is entered under no host that is not entered itself: a batch
        // that says so is refused with its load.
        let tx = world.begin();
        let mut twice = alone(6);
        twice.subtrees = vec![OwnedSubtree::Enter(model::subtree(6), None)];
        let refused = take(&world, &tx, (&twice, 6), by_key)
            .map(drop)
            .expect_err("it is entered");
        let entered = BatchError::Entered {
            subtree: model::subtree(6),
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == entered),
            "by key: {by_key}: {refused}"
        );
        let (orphan, nowhere) = (ENTERED + 5, ENTERED + 6);
        let mut hostless = alone(orphan);
        hostless.subtrees = vec![OwnedSubtree::Enter(
            model::subtree(orphan),
            Some((model::subtree(nowhere), key(b'h', 5), 0)),
        )];
        let refused = take(&world, &tx, (&hostless, orphan), by_key)
            .map(drop)
            .expect_err("its host is not entered");
        let no_host = BatchError::NotEntered {
            subtree: model::subtree(nowhere),
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == no_host),
            "by key: {by_key}: {refused}"
        );
        // Where a commit dropped the host's subtree, and the transaction
        // holds the key set of that commit because it was shown a node
        // before, the refusal is that of a write into a subtree that a
        // commit took (CF10).
        world.check(&tx);
        let dropped = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(model::subtree(10))],
            ..OwnedBatch::default()
        };
        let dropped = world.write_outside(&dropped, &[&tx]);
        assert!(matches!(dropped, Outcome::Committed(_)));
        let mut under = alone(orphan);
        under.subtrees = vec![OwnedSubtree::Enter(
            model::subtree(orphan),
            Some((model::subtree(10), key(b'h', 5), 0)),
        )];
        let refused = take(&world, &tx, (&under, orphan), by_key)
            .map(drop)
            .expect_err("its host was dropped");
        let took = BatchError::ValueTaken {
            subtree: model::subtree(10),
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == took),
            "by key: {by_key}: {refused}"
        );
        assert!(refused.is_conflict());
        world.check(&tx);
        assert!(matches!(ends(&tx), Ok(())), "nothing was taken");

        // The write, the other commit, the commit of the load.
        let tx = world.begin();
        let open = take(&world, &tx, (&alone(2), 2), by_key).expect("a load");
        let other = world.begin();
        first_node(&world, &other, 2);
        assert!(matches!(world.commit(other, &[&tx]), Outcome::Committed(_)));
        open.into_iter().for_each(|scope| scope.merge());
        let ended = ends(&tx);
        assert!(
            matches!(ended, Err(CommitError::Conflict)),
            "by key: {by_key}: {ended:?}"
        );

        // The other write, the load and its commit, the other commit.
        let other = world.begin();
        first_node(&world, &other, 3);
        let tx = world.begin();
        let open = take(&world, &tx, (&alone(3), 3), by_key).expect("a load");
        open.into_iter().for_each(|scope| scope.merge());
        let published = commit(&mut world, tx);
        assert_eq!(published.acquired, long_values(&merk));
        let ended = ends(&other);
        assert!(
            matches!(ended, Err(CommitError::Conflict)),
            "by key: {by_key}: {ended:?}"
        );

        // A commit that deletes a range of the Merk with no key in it
        // wrote no node of the Merk, and does not meet the load.
        let tx = world.begin();
        let open = take(&world, &tx, (&alone(6), 6), by_key).expect("a load");
        let nothing = OwnedBatch {
            records: vec![
                Write::Range(
                    Unit::data(model::subtree(6)),
                    b"k".to_vec(),
                    Some(b"k".to_vec()),
                ),
                put(model::unit(Column::Aux, 8), b"beside", b"a record"),
            ],
            ..OwnedBatch::default()
        };
        let nothing = world.write_outside(&nothing, &[&tx]);
        assert!(matches!(nothing, Outcome::Committed(_)));
        open.into_iter().for_each(|scope| scope.merge());
        let published = commit(&mut world, tx);
        assert_eq!(published.acquired, long_values(&merk));

        // The Merks are the first writer's, each whole.
        world.check_committed();
        assert_eq!(counted(&db), 3 * long_values(&merk) as u64);
        let report = db.verify_still();
        assert!(db.unsound(&report).is_empty(), "{report}");
    }
}

/// A clear of a Merk with no node leaves the directory as it is: such a
/// Merk hosts none (`tree-layout.md` D6). A transaction enters a Merk and
/// another under an element it has not written yet, and then clears the
/// first, which has no node, in the batch that loads the second. Where
/// the load is laid out, it is taken: the frames have the directory,
/// which says what a clear takes with it. By key a batch that clears a
/// subtree is not taken with a load (LD6, `LoadBeside`): nothing of it is
/// in the write set, and it is no conflict.
///
/// A subtree that the committed state has below a Merk is another case
/// of the same. The transaction takes the only node of that Merk out and
/// then clears it in the batch that loads the subtree below: laid out,
/// the clear takes nothing with it and the load is taken, and by key the
/// batch is not taken.
#[test]
fn a_load_below_a_merk_with_no_node_that_its_batch_clears() {
    for by_key in [false, true] {
        let (db, versions) = Db::new(256);
        let mut world = World::new(&db, &versions);
        let merk = items(&mut Rng::new(33), b'k', 80, 3);
        let (holder, held) = (model::subtree(ENTERED + 7), model::subtree(ENTERED + 8));
        let tx = world.begin();
        let enters = OwnedBatch {
            subtrees: vec![
                OwnedSubtree::Enter(holder, None),
                OwnedSubtree::Enter(held, Some((holder, key(b'h', 7), 0))),
            ],
            ..OwnedBatch::default()
        };
        let scope = tx.txn.scope().expect("a scope");
        let entered = enters.with(|batch| scope.apply(batch));
        entered.expect("two subtrees with no node");
        scope.merge();

        let mut load = whole(ENTERED + 8, &merk);
        load.anchored = true;
        let batch = OwnedBatch {
            merks: vec![load],
            subtrees: vec![OwnedSubtree::Clear(holder)],
            ..OwnedBatch::default()
        };
        if by_key {
            let writes = tx.txn.stats().writes;
            let first = tx.txn.scope().expect("a scope");
            let beside = tx.txn.scope().expect("a scope beside it");
            let refused = batch.loading(&[held], |batch| beside.apply(batch));
            let refused = refused.expect_err("beside a clear, by key");
            let beside_a_clear = BatchError::LoadBeside { subtree: held };
            assert!(
                matches!(&refused, TxnError::Batch(error) if *error == beside_a_clear),
                "{refused}"
            );
            assert!(!refused.is_conflict());
            first.merge();
            beside.merge();
            assert_eq!(tx.txn.stats().writes, writes, "nothing of the batch");
        }
        let scope = tx.txn.scope().expect("a scope");
        let applied = batch.loading(&[held], |batch| scope.apply(batch));
        let applied = applied.unwrap_or_else(|refused| panic!("by key: {by_key}: {refused}"));
        assert!(applied.load.is_some());
        scope.merge();
        let rooted = tx.txn.subtree(held, |view| {
            view.expect("the subtree is entered").root_key.is_some()
        });
        assert!(rooted.expect("a look"), "by key: {by_key}");
        tx.txn.rollback().expect("no scope is open");
        assert_eq!(db.private_ids(), 0);

        // A Merk with one node, and a subtree with no node that the node
        // hosts, both committed.
        let (above, below) = (model::subtree(ENTERED + 14), model::subtree(ENTERED + 15));
        let host = key(b'h', 9);
        let maker = world.begin();
        let mut outer = world.merk(&maker, ENTERED + 14, |items| {
            items.insert(host.clone(), b"tree".to_vec());
        });
        outer.hosted.insert(host.clone(), vec![below]);
        let mut made = batch_of(vec![outer]);
        made.subtrees = vec![
            OwnedSubtree::Enter(above, None),
            OwnedSubtree::Enter(below, Some((above, host.clone(), 0))),
        ];
        world.apply(&maker, &made).expect("a Merk and one below it");
        assert!(matches!(world.commit(maker, &[]), Outcome::Committed(_)));

        let tx = world.begin();
        world.edit(&tx, ENTERED + 14, |items| drop(items.remove(&host)));
        let mut load = whole(ENTERED + 15, &merk);
        load.anchored = true;
        let batch = OwnedBatch {
            merks: vec![load],
            subtrees: vec![OwnedSubtree::Clear(above)],
            ..OwnedBatch::default()
        };
        if by_key {
            let first = tx.txn.scope().expect("a scope");
            let beside = tx.txn.scope().expect("a scope beside it");
            let refused = batch.loading(&[below], |batch| beside.apply(batch));
            let refused = refused.expect_err("beside a clear, by key");
            let beside_a_clear = BatchError::LoadBeside { subtree: below };
            assert!(
                matches!(&refused, TxnError::Batch(error) if *error == beside_a_clear),
                "{refused}"
            );
            assert!(!refused.is_conflict());
            first.merge();
            beside.merge();
        }
        let scope = tx.txn.scope().expect("a scope");
        let applied = batch.loading(&[below], |batch| scope.apply(batch));
        let applied = applied.unwrap_or_else(|refused| panic!("by key: {by_key}: {refused}"));
        assert!(applied.load.is_some());
        scope.merge();
        let rooted = tx.txn.subtree(below, |view| {
            view.expect("the subtree is entered").root_key.is_some()
        });
        assert!(rooted.expect("a look"), "by key: {by_key}");
        assert!(tx.txn.stats().tree_pages > 0, "the load is laid out");
        tx.txn.rollback().expect("no scope is open");
        assert_eq!(db.private_ids(), 0);
    }
}

/// A batch that enters the subtree of its load if it is absent hands the
/// load to the subtree it finds, where that has no node, and to a new one
/// where none is entered. Where the subtree it finds has a node, the load
/// is refused as any load into a Merk with a root. So it is where the
/// load is laid out and where it is taken by key.
#[test]
fn a_load_into_a_subtree_that_its_batch_enters_if_it_is_absent() {
    for by_key in [false, true] {
        let (db, versions) = Db::new(256);
        let mut world = World::new(&db, &versions);
        let merk = items(&mut Rng::new(41), b'e', 90, 3);
        let absent = ENTERED + 13;
        let entering = |name: u8| OwnedBatch {
            merks: vec![whole(name, &merk)],
            subtrees: vec![OwnedSubtree::EnterIfAbsent(model::subtree(name), None)],
            ..OwnedBatch::default()
        };
        let tx = world.begin();
        for name in [1, absent] {
            let open = take(&world, &tx, (&entering(name), name), by_key);
            let open = open.unwrap_or_else(|refused| panic!("by key: {by_key}: {refused}"));
            open.into_iter().for_each(|scope| scope.merge());
            world.check(&tx);
        }
        let published = commit(&mut world, tx);
        assert_eq!(published.acquired, 2 * long_values(&merk));

        // Each has a node now.
        let tx = world.begin();
        for name in [1, absent] {
            let refused = take(&world, &tx, (&entering(name), name), by_key)
                .map(drop)
                .expect_err("a root");
            let rooted = BatchError::NotWhole {
                subtree: model::subtree(name),
                fault: WholeFault::HasRoot,
            };
            assert!(
                matches!(&refused, TxnError::Batch(error) if *error == rooted),
                "by key: {by_key}: {refused}"
            );
            assert!(!refused.is_conflict());
        }
        world.check(&tx);

        // An enter if it is absent that found the subtree says nothing of
        // the root of its Merk: where a commit gives the Merk a node after
        // the transaction looked at it, the load is told so.
        let tx = world.begin();
        let found = OwnedBatch {
            subtrees: vec![OwnedSubtree::EnterIfAbsent(model::subtree(2), None)],
            ..OwnedBatch::default()
        };
        world.apply(&tx, &found).expect("the subtree is found");
        let rootless = tx.txn.subtree(model::subtree(2), |view| {
            view.expect("the subtree is entered").root_key.is_none()
        });
        assert!(rootless.expect("a look"), "by key: {by_key}");
        let landed = world.edit_outside(
            2,
            |items| drop(items.insert(b"zz".to_vec(), vec![5; 12])),
            &[&tx],
        );
        assert!(matches!(landed, Outcome::Committed(_)));
        let alone = OwnedBatch {
            merks: vec![whole(2, &merk)],
            ..OwnedBatch::default()
        };
        let refused = take(&world, &tx, (&alone, 2), by_key)
            .map(drop)
            .expect_err("a root");
        let taken = BatchError::MerkTaken {
            subtree: model::subtree(2),
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == taken),
            "by key: {by_key}: {refused}"
        );
        assert!(refused.is_conflict());
        world.check_committed();
        assert_eq!(counted(&db), 2 * long_values(&merk) as u64);
    }
}

/// A refusal of a load is told as a conflict by a look only where no
/// write of the transaction says anything of the root of the Merk (LD6).
/// A transaction clears a Merk that has a node, looks at it, gives it a
/// node of its own, and enters the subtree if it is absent, which finds
/// it. A commit outside writes elsewhere. The load is refused for the
/// root that the transaction wrote itself, which is no conflict: laid out
/// and by key.
#[test]
fn a_root_that_the_transaction_wrote_is_no_conflict_of_a_load() {
    for by_key in [false, true] {
        let (db, versions) = Db::new(256);
        let mut world = World::new(&db, &versions);
        let merk = items(&mut Rng::new(37), b'r', 40, 2);
        fn node<'s>(world: &World<'s>, tx: &Tx<'s>, key: &[u8]) {
            let mut batch = OwnedBatch::default();
            batch.merks.push(world.merk(tx, 1, |items| {
                items.insert(key.to_vec(), vec![5; 12]);
            }));
            world.apply(tx, &batch).expect("a first node");
        }
        let maker = world.begin();
        node(&world, &maker, b"theirs");
        assert!(matches!(world.commit(maker, &[]), Outcome::Committed(_)));

        let tx = world.begin();
        let cleared = OwnedBatch {
            subtrees: vec![OwnedSubtree::Clear(model::subtree(1))],
            ..OwnedBatch::default()
        };
        world.apply(&tx, &cleared).expect("its own clear");
        let rootless = tx.txn.subtree(model::subtree(1), |view| {
            view.expect("the subtree is entered").root_key.is_none()
        });
        assert!(rootless.expect("a look"), "by key: {by_key}");
        node(&world, &tx, b"mine");
        let found = OwnedBatch {
            subtrees: vec![OwnedSubtree::EnterIfAbsent(model::subtree(1), None)],
            ..OwnedBatch::default()
        };
        world.apply(&tx, &found).expect("the subtree is found");
        let elsewhere = world.edit_outside(
            9,
            |items| drop(items.insert(b"k".to_vec(), b"v".to_vec())),
            &[&tx],
        );
        assert!(matches!(elsewhere, Outcome::Committed(_)));

        let alone = OwnedBatch {
            merks: vec![whole(1, &merk)],
            ..OwnedBatch::default()
        };
        let refused = take(&world, &tx, (&alone, 1), by_key)
            .map(drop)
            .expect_err("its own root");
        let rooted = BatchError::NotWhole {
            subtree: model::subtree(1),
            fault: WholeFault::HasRoot,
        };
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == rooted),
            "by key: {by_key}: {refused}"
        );
        assert!(!refused.is_conflict());
        world.check(&tx);
        assert_eq!(tx.txn.stats().overlapped, 0);
        assert_eq!(counted(&db), 0);
    }
}

/// What the directory names as the host of a Merk is asked as the batch
/// of a load leaves it (LD1). A subtree that the batch states anew at
/// another position among those of its host is hosted there: a node of
/// the load that names it at the position it had is refused, and one
/// that names it at the position it gets hosts it, beside a subtree that
/// the batch enters at the position it left.
#[test]
fn a_node_of_a_load_hosts_a_merk_at_the_position_its_batch_states() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(23);
    let (stated, beside) = (ENTERED + 11, ENTERED + 12);
    let host = key(b'h', 2);
    let mut outer = items(&mut rng, b'c', 40, 3);
    outer.insert(host.clone(), b"tree".to_vec());
    let at = |position| Some((model::subtree(3), host.clone(), position));
    let anew = || OwnedSubtree::Convert(model::subtree(stated), TREE_TYPE.to_vec(), Some(1));

    // A Merk is entered under a node that is not written yet, and gets
    // its nodes.
    let tx = world.begin();
    let inner = world.merk(&tx, stated, |items| {
        items.extend(items_of(&mut rng, b'i', 30));
    });
    let mut batch = batch_of(vec![inner]);
    batch.subtrees = vec![OwnedSubtree::Enter(model::subtree(stated), at(0))];
    world
        .apply(&tx, &batch)
        .expect("a Merk under a host to come");

    // The node names it where it was.
    let mut merk = whole(3, &outer);
    merk.hosted
        .insert(host.clone(), vec![model::subtree(stated)]);
    let batch = OwnedBatch {
        merks: vec![merk],
        subtrees: vec![anew()],
        ..OwnedBatch::default()
    };
    let refused = load(&world, &tx, &batch, &[3]).expect_err("it is at another position");
    let not_its_host = BatchError::NotWhole {
        subtree: model::subtree(3),
        fault: WholeFault::NotItsHost,
    };
    assert!(
        matches!(&refused, TxnError::Batch(error) if *error == not_its_host),
        "{refused}"
    );
    world.check(&tx);

    // The node names it where the batch puts it.
    let mut merk = whole(3, &outer);
    merk.hosted.insert(
        host.clone(),
        vec![model::subtree(beside), model::subtree(stated)],
    );
    let batch = OwnedBatch {
        merks: vec![merk],
        subtrees: vec![anew(), OwnedSubtree::Enter(model::subtree(beside), at(0))],
        ..OwnedBatch::default()
    };
    let applied = load(&world, &tx, &batch, &[3]).expect("a load");
    assert!(applied.load.is_some());
    world.check(&tx);
    commit(&mut world, tx);
    world.check_committed();
    let report = db.verify_still();
    assert!(db.unsound(&report).is_empty(), "{report}");
}

/// The commit of a transaction that holds a load makes it ready before
/// the commit lock is taken, and what that comes to is no answer of the
/// commit. A transaction that a commit has met ends with the conflict,
/// also where the look at what its drop covers fails while it is made
/// ready: the error is no reason to try again (CF6).
#[test]
fn a_commit_of_a_load_that_is_met_ends_with_the_conflict() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let merk = items(&mut Rng::new(19), b'm', 200, 3);
    let dropped = world.edit_outside(
        9,
        |items| items.extend(items_of(&mut Rng::new(20), b'g', 12)),
        &[],
    );
    assert!(matches!(dropped, Outcome::Committed(_)));
    let beside = |value: &[u8]| OwnedBatch {
        records: vec![put(model::unit(Column::Aux, 8), b"k", value)],
        ..OwnedBatch::default()
    };

    let tx = world.begin();
    load(&world, &tx, &batch_of(vec![whole(1, &merk)]), &[1]).expect("a load");
    let drop = OwnedBatch {
        subtrees: vec![OwnedSubtree::Drop(model::subtree(9))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &drop).expect("the drop");
    world.apply(&tx, &beside(b"mine")).expect("a record beside");
    let theirs = world.write_outside(&beside(b"theirs"), &[&tx]);
    assert!(matches!(theirs, Outcome::Committed(_)));
    tx.txn.refuse_look();
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
    assert_eq!(counted(&db), 0, "no value of the load is counted");
}

/// A node of a load hosts a Merk only where the Merk was entered with
/// that node as its host (LD1): the directory names the host of a Merk,
/// and a node of a load is written with the root of no other. A Merk that
/// another node hosts is refused to it before anything is laid out, with
/// the transaction as it was:
///
/// - where the batch writes the other host and it keeps what it hosts,
///   which is refused as well where every node is written one by one;
/// - where the batch has the other host say that it hosts nothing;
/// - where the batch does not write the other host.
///
/// So is one that the batch itself enters with another key of the load's
/// Merk, at another position, or with no host. A load that is taken by
/// key is in the write set, and is refused when it is laid out: its commit
/// is refused with the transaction as it was.
///
/// A Merk that was entered with the node of the load as its host, and got
/// its nodes before the load, is hosted by it
/// (`the_nodes_of_a_load_host_merks`).
#[test]
fn a_node_of_a_load_hosts_no_merk_that_was_entered_with_another_host() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(21);
    let hosted = ENTERED + 1;
    let host = key(b'h', 1);

    // A node of the Merk 1 hosts a Merk with nodes.
    let tx = world.begin();
    let mut outer = world.merk(&tx, 1, |items| {
        items.extend(items_of(&mut rng, b'o', 20));
        items.insert(host.clone(), b"tree".to_vec());
    });
    outer
        .hosted
        .insert(host.clone(), vec![model::subtree(hosted)]);
    let inner = world.merk(&tx, hosted, |items| {
        items.extend(items_of(&mut rng, b'i', 30));
    });
    let mut batch = batch_of(vec![outer, inner]);
    batch.subtrees = vec![OwnedSubtree::Enter(
        model::subtree(hosted),
        Some((model::subtree(1), host.clone(), 0)),
    )];
    world.apply(&tx, &batch).expect("a host and its Merk");
    assert!(matches!(world.commit(tx, &[]), Outcome::Committed(_)));

    let other = items(&mut rng, b'a', 60, 3);
    let second = other.keys().nth(7).expect("a node").clone();
    let not_its_host = BatchError::NotWhole {
        subtree: model::subtree(2),
        fault: WholeFault::NotItsHost,
    };
    for other_host in ["keeps", "hosts nothing", "is not written"] {
        // A node of another Merk, which the batch hands over whole, names
        // the Merk that the first node hosts.
        let tx = world.begin();
        let mut load = whole(2, &other);
        load.hosted
            .insert(second.clone(), vec![model::subtree(hosted)]);
        let mut merks = Vec::new();
        if other_host != "is not written" {
            let mut written = world.merk(&tx, 1, |items| {
                items.insert(host.clone(), b"tree, written".to_vec());
            });
            assert!(written.hosted.is_empty(), "the host keeps what it hosts");
            if other_host == "hosts nothing" {
                written.hosted.insert(host.clone(), Vec::new());
            }
            merks.push(written);
        }
        merks.push(load);
        let batch = batch_of(merks);
        // The nodes above the other host keep their values: each is read,
        // and the write names that read.
        let scope = tx.txn.scope().expect("a scope");
        let read = batch.through(&scope);
        let refused = read.loading(&[model::subtree(2)], |batch| scope.apply(batch));
        scope.merge();
        let refused = refused.expect_err("the Merk has another host");
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == not_its_host),
            "the other host {other_host}: {refused}"
        );
        world.check(&tx);
        assert_eq!(tx.txn.stats().tree_pages, 0, "nothing was laid out");
        if other_host == "keeps" {
            let one_by_one = world.apply(&tx, &batch);
            let two_pointers = |error: &TxnError| {
                use lignin_tree::fragment::{FragmentError, WriteError};
                matches!(
                    error,
                    TxnError::Fragment(FragmentError::Write(WriteError::TwoPointers { .. }))
                )
            };
            assert!(
                matches!(&one_by_one, Err(error) if two_pointers(error)),
                "two hosts, node by node: {one_by_one:?}"
            );
            world.check(&tx);
        }
        tx.txn.rollback().expect("no scope is open");
    }

    // The batch enters the Merk itself, and not with the node that hosts
    // it in the load.
    let tx = world.begin();
    let entered = |name: u8| model::subtree(ENTERED + name);
    let loaded = model::subtree(2);
    let enters = [
        ("another key", entered(2), Some((loaded, key(b'z', 1), 0))),
        (
            "another position",
            entered(3),
            Some((loaded, second.clone(), 1)),
        ),
        ("no host", entered(4), None),
    ];
    for (with, subtree, host) in enters {
        let mut hosting = whole(2, &other);
        hosting.hosted.insert(second.clone(), vec![subtree]);
        let mut batch = batch_of(vec![hosting]);
        batch.subtrees = vec![OwnedSubtree::Enter(subtree, host)];
        let refused = load(&world, &tx, &batch, &[2]).expect_err("the Merk has another host");
        assert!(
            matches!(&refused, TxnError::Batch(error) if *error == not_its_host),
            "entered with {with}: {refused}"
        );
        world.check(&tx);
    }
    tx.txn.rollback().expect("no scope is open");

    // Taken by key, the load is in the write set. It is refused where it
    // is laid out, which its commit asks for.
    let tx = world.begin();
    let mut hosting = whole(2, &other);
    hosting
        .hosted
        .insert(second.clone(), vec![model::subtree(hosted)]);
    let batch = batch_of(vec![hosting]);
    let open = take(&world, &tx, (&batch, 2), true).expect("taken by key");
    open.into_iter().for_each(|scope| scope.merge());
    let committer = db.committer(&versions);
    let refused = db
        .commit_as(&committer, &tx.txn, None)
        .expect_err("no commit");
    assert!(
        matches!(&refused, CommitError::Refused(TxnError::Batch(error)) if *error == not_its_host),
        "{refused}"
    );
    assert!(refused.leaves_transaction_open());
    tx.txn.rollback().expect("no scope is open");

    world.check_committed();
    let report = db.verify_still();
    assert!(db.unsound(&report).is_empty(), "{report}");
}

/// A transaction that a commit has met serves its records by key, and a
/// load it is given then has no page: its values that fit their cells
/// stay with the write set, the others are in the value log, and every
/// record is read as it was stated. And a load keeps no value,
/// so no read is dated for it (CF2a): it is taken after any number of
/// commits, with no handle named.
#[test]
fn a_load_by_key_and_a_load_after_many_commits() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(18);
    let merk = items(&mut rng, b'y', 200, 2);

    let tx = world.begin();
    world.edit(&tx, 5, |items| {
        drop(items.insert(b"met".to_vec(), vec![1; 8]))
    });
    let landed = world.edit_outside(
        5,
        |items| drop(items.insert(b"met".to_vec(), vec![2; 8])),
        &[&tx],
    );
    assert!(matches!(landed, Outcome::Committed(_)));
    let applied = load(&world, &tx, &batch_of(vec![whole(1, &merk)]), &[1]).expect("by key");
    assert_eq!((applied.load, applied.nodes.len()), (None, 0));
    assert_eq!(tx.txn.loaded_runs(), 0);
    assert!(tx.txn.stats().overlapped > 0);
    world.check(&tx);
    // By key too, a Merk that has a root is no Merk to load.
    let error = load(&world, &tx, &batch_of(vec![whole(1, &merk)]), &[1]).expect_err("a root");
    assert!(matches!(
        error,
        TxnError::Batch(BatchError::NotWhole {
            fault: WholeFault::HasRoot,
            ..
        })
    ));
    assert_eq!(world.commit(tx, &[]), Outcome::Conflict);

    // More commits than a transaction keeps key sets of, and then a load
    // through a transaction that read a node before them.
    let tx = world.begin();
    let seen = tx.txn.node(model::subtree(5), b"met", model::handle_of);
    assert!(seen.expect("a read").is_some());
    for round in 0..70u8 {
        world.edit_outside(
            6,
            |items| drop(items.insert(vec![round], vec![round; 4])),
            &[&tx],
        );
    }
    load(&world, &tx, &batch_of(vec![whole(1, &merk)]), &[1]).expect("a load");
    assert_eq!(tx.txn.stats().overlapped, 0);
    world.check(&tx);
    let published = commit(&mut world, tx);
    assert_eq!(published.acquired, long_values(&merk));
    world.check_committed();
}

/// A load that no frames take, here that of a scope that is open beside
/// another, has no page. Its values that are too long for a cell are in
/// the value log all the same, and the write set holds as much of the
/// load whether such a value is 257 bytes long or 9,000 more: while the
/// scope keeps its writes there, and once they are laid out.
#[test]
fn a_load_that_no_frames_take_keeps_its_long_values_in_the_value_log() {
    let held = |more: usize| {
        let (db, versions) = Db::new(256);
        let mut world = World::new(&db, &versions);
        let mut merk = items(&mut Rng::new(23), b'n', 240, 2);
        for value in merk.values_mut().filter(|value| value.len() >= LONG) {
            value.resize(LONG + more, 0x5a);
        }
        let long = long_values(&merk);
        let tx = world.begin();
        let first = tx.txn.scope().expect("a scope");
        let beside = tx.txn.scope().expect("a scope beside it");
        let batch = batch_of(vec![whole(1, &merk)]);
        let first_write = world.latest().get();
        let applied = batch.loading(&[model::subtree(1)], |batch| beside.apply(batch));
        assert_eq!(applied.expect("kept in the write set").load, None);
        world.note(&tx, &batch, first_write);
        let unlaid = tx.txn.stats().write_set_bytes;
        assert_eq!(tx.txn.stats().tree_pages, 0);
        // A value that went into the value log for no page is read from
        // there, by key.
        let (long_key, long_value) = (merk.iter())
            .find(|(_, value)| value.len() >= LONG)
            .expect("a value out of line");
        let read = beside.node(model::subtree(1), long_key, |view| {
            value_of(&view.expect("a node of the load"))
        });
        assert_eq!(&read.expect("a read by key"), long_value);
        first.merge();
        beside.merge();
        world.check(&tx);
        let laid = tx.txn.stats();
        assert!(laid.tree_pages > 0 && laid.value_pages == 0, "{laid:?}");
        let published = commit(&mut world, tx);
        assert_eq!((published.acquired, published.value_bytes), (long, 0));
        assert_eq!(counted(&db), long as u64);
        world.check_committed();
        (unlaid, laid.write_set_bytes, long)
    };
    let (short, long) = (held(0), held(9_000));
    assert_eq!(short, long, "9,000 bytes more in each value out of line");
    assert!(short.2 > 100, "{short:?}");
}

/// A load of a scope that is open beside another is kept in the write
/// set, and laid out when the scope merges. And a batch clears a Merk and
/// loads it: the load is of the Merk the clear left.
#[test]
fn a_load_of_a_scope_beside_another_and_a_load_into_a_cleared_merk() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(19);
    let merk = items(&mut rng, b's', 250, 2);
    let long = long_values(&merk);

    let tx = world.begin();
    let first = tx.txn.scope().expect("a scope");
    let beside = tx.txn.scope().expect("a scope beside it");
    let batch = batch_of(vec![whole(1, &merk)]);
    let first_write = world.latest().get();
    let applied = batch.loading(&[model::subtree(1)], |batch| beside.apply(batch));
    assert_eq!(applied.expect("kept in the write set").load, None);
    world.note(&tx, &batch, first_write);
    first.merge();
    beside.merge();
    world.check(&tx);
    assert!(tx.txn.stats().tree_pages > 0);
    let published = commit(&mut world, tx);
    assert_eq!(published.acquired, long);
    world.check_committed();

    // The Merk is cleared and loaded with other nodes, in one batch.
    let other = items(&mut rng, b't', 180, 2);
    let tx = world.begin();
    let mut batch = batch_of(vec![whole(1, &other)]);
    batch.subtrees = vec![OwnedSubtree::Clear(model::subtree(1))];
    load(&world, &tx, &batch, &[1]).expect("a load into the Merk the clear left");
    world.check(&tx);
    let published = commit(&mut world, tx);
    assert_eq!(published.acquired, long_values(&other));
    world.check_committed();
    let records = &world.model.records;
    let nodes = (records.iter()).filter(|((unit, _), value)| {
        *unit == Unit::data(model::subtree(1)) && matches!(value, Val::Node(_))
    });
    assert_eq!(nodes.count(), other.len());
}
