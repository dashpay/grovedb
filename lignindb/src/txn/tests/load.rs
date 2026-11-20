//! Loads: change sets that are whole Merks, laid out by the bulk loader,
//! with their values out of line in the value log before the commit
//! (`crate::txn`, "A load"; TXN-10).

use lignin_tree::fragment::Layout;

use super::{
    model::{self, NodeRec, OwnedBatch, OwnedMerk, OwnedSubtree, Val, Write},
    store::{Db, Outcome, Rng, ENTERED, SIZE},
    *,
};
use crate::commit::{CommitError, Published};

/// The shortest value that the store of these tests puts out of line.
const LONG: usize = lignin_tree::value::InlinePolicy::DEFAULT_THRESHOLD + 1;

/// The keys and values of one Merk.
type Items = BTreeMap<Vec<u8>, Vec<u8>>;

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
fn items(rng: &mut Rng, lead: u8, count: usize, long: usize) -> Items {
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
fn whole(name: u8, items: &Items) -> OwnedMerk {
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

/// A batch of `merks`, each with the record that a change of its root
/// writes beside it.
fn batch_of(merks: Vec<OwnedMerk>) -> OwnedBatch {
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
/// the commit counts each value once.
#[test]
fn frames_that_are_made_again_lay_a_load_out_again() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(12);
    let merk = items(&mut rng, b'r', 1_500, 3);
    let long = long_values(&merk);

    let tx = world.begin();
    load(&world, &tx, &batch_of(vec![whole(1, &merk)]), &[1]).expect("a load");
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
    let mut batch = batch_of(vec![over, whole(under, &items(&mut rng, b'u', 150, 2))]);
    batch.subtrees = vec![enter];
    load(&world, &tx, &batch, &[under]).expect("a load below a node that is written");
    world.check(&tx);

    // The checker follows each host to the root it names, and compares
    // it with the directory: the three Merks with a node, and not the one
    // with none.
    commit(&mut world, tx);
    world.check_committed();
    let report = db.verify_still();
    assert!(db.unsound(&report).is_empty(), "{report}");
    assert_eq!(report.census.hosted_roots, 3, "{report}");
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
    // Two loads that host each other.
    let mut back = other;
    back.hosted.insert(back.nodes[0].0.clone(), vec![one]);
    let (_, fault) = refused(vec![hosts, back], &[1, 2]);
    assert_eq!(fault, WholeFault::HostsItself);

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

/// What a load is to the conflict rule: the puts of its nodes, and the
/// first node of a Merk. A commit that wrote another key of another Merk
/// does not meet it. A commit that gave the Merk a root leaves a write set
/// that cannot be laid out, and the commit of the load is refused; a
/// commit that wrote a key of the load meets it.
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
    assert_eq!(world.commit(second, &[]), Outcome::Conflict);
    assert_eq!(counted(&db), long_values(&merk) as u64);

    // With no record in common, the load is no range: the commit that
    // gave the Merk its root wrote none of the load's keys. The load
    // cannot be laid out over that root, and its commit is refused with
    // the transaction as it was.
    let tx = world.begin();
    let alone = OwnedBatch {
        merks: vec![whole(2, &merk)],
        ..OwnedBatch::default()
    };
    load(&world, &tx, &alone, &[2]).expect("a load");
    let other = world.begin();
    let mut batch = OwnedBatch::default();
    batch.merks.push(world.merk(&other, 2, |items| {
        items.insert(b"zz-another-key".to_vec(), vec![5; 12]);
    }));
    world.apply(&other, &batch).expect("a first node");
    assert!(matches!(world.commit(other, &[&tx]), Outcome::Committed(_)));
    let committer = db.committer(&versions);
    let refused = db
        .commit_as(&committer, &tx.txn, None)
        .expect_err("no commit");
    let taken = BatchError::RootTaken {
        subtree: model::subtree(2),
    };
    assert!(
        matches!(&refused, CommitError::Refused(TxnError::Batch(error)) if *error == taken),
        "{refused}"
    );
    assert!(refused.leaves_transaction_open());
    assert_eq!(tx.txn.stats().overlapped, 0);
    tx.txn.rollback().expect("no scope is open");
    assert_eq!(counted(&db), long_values(&merk) as u64);
}

/// A transaction that a commit has met serves its records by key, and a
/// load it is given then has no page: its values stay with the write set,
/// and every record is read as it was stated. And a load keeps no value,
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
