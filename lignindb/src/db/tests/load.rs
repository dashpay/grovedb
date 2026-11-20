//! Loads through the facade (TXN-10): a Merk that is handed over whole is
//! the Merk that is written node by node, it is there after the database
//! is opened again, and a power cut at any point of its commit leaves the
//! database without it or with all of it.

use std::sync::Arc;

use lignin_io::sim::{CrashPoint, SimConfig, SimVfs};
use lignin_tree::{
    bulk::LoadCounters,
    fragment::Layout,
    value::{read_value, ValueBuf},
};

use super::*;
use crate::snapshot::LatestLease;

/// One node as a reader finds it: its key, its value, its feature, and for
/// each link the key of the child with the heights and the aggregate.
type Dumped = (
    Vec<u8>,
    Vec<u8>,
    Vec<u8>,
    [Option<(Vec<u8>, (u8, u8), Vec<u8>)>; 2],
);

/// Every node of the Merk of `subtree` as a reader of `db` finds it, in
/// the order of their keys, or `None` for a subtree that is not entered.
fn dump(db: &Db, subtree: Subtree) -> Option<Vec<Dumped>> {
    let mut lease = LatestLease::new();
    let operation = db.read(&mut lease);
    let opened = operation.open(subtree).expect("a lookup")?;
    let reader = operation.nodes();
    let values = &db.shared.store.values;
    let mut buffers = LoadBuffers::new();
    let mut nodes = Vec::new();
    let mut stack: Vec<CellAddress> = opened.root().into_iter().collect();
    while let Some(address) = stack.pop() {
        let pinned = reader.node(address, &mut buffers).expect("a node");
        let node = pinned.node();
        let mut buf = ValueBuf::new();
        let value = read_value(values, &pinned, &mut buf).expect("a value");
        let links = [Side::Left, Side::Right].map(|side| {
            let link = node.link(side)?;
            stack.push(link.child);
            let child = reader.node(link.child, &mut LoadBuffers::new());
            let key = child.expect("a child").node().key().to_vec();
            Some((key, link.heights, link.aggregate.to_vec()))
        });
        let (key, feature) = (node.key().to_vec(), node.feature().to_vec());
        nodes.push((key, value.to_vec(), feature, links));
    }
    nodes.sort();
    Some(nodes)
}

/// A Merk of `count` nodes, of which one value in three is out of line.
fn mixed(lead: u8, count: u32) -> Merk {
    let items = (0..count).map(|number| {
        let mut key = vec![lead];
        key.extend_from_slice(&number.to_be_bytes());
        let len = if number % 3 == 0 {
            300 + number as usize % 500
        } else {
            24
        };
        (key, vec![lead ^ number as u8; len])
    });
    Merk::of(&items.collect())
}

/// Loads `merk` into `subtree`, which is entered, through a transaction of
/// its own, and says what the loader did and what the commit did.
fn load(db: &Db, subtree: Subtree, merk: Merk) -> (LoadCounters, Box<Published>) {
    let writes = Writes {
        merks: vec![(subtree, merk)],
        load: true,
        ..Writes::default()
    };
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let applied = writes.with(|batch| scope.apply(batch)).expect("a load");
    scope.merge();
    let counters = applied.load.expect("the loader laid the Merk out");
    match db.commit(&txn, &mut NoStaleHashes) {
        Ok(Committed::Published(published)) => (counters, published),
        other => panic!("the load is committed, and not {other:?}"),
    }
}

fn entered(db: &Db, subtrees: &[Subtree]) {
    let writes = Writes {
        enter: subtrees.iter().map(|subtree| (*subtree, None)).collect(),
        ..Writes::default()
    };
    writes.commit(db);
}

fn sound(db: &Db, context: &str) {
    let report = checked(db);
    assert!(
        report.findings.is_empty() && report.more_findings == 0,
        "{context}:\n{report}"
    );
}

/// TXN-10's first "done when", through the facade: the Merk that is
/// handed over whole has the nodes of the one that is written node by
/// node, in pages at the loader's fill, with its values out of line in the
/// value log, and it is the same Merk after the database is opened again.
#[test]
fn a_loaded_merk_is_the_merk_that_is_written_node_by_node_also_after_a_reopen() {
    let dir = temp();
    let merk = mixed(b'm', 9_000);
    let long = 3_000;
    {
        let db = Db::open(dir.path(), &options()).expect("a new database");
        entered(&db, &[subtree(1), subtree(2)]);
        // Node by node, as any Merk is written: one change set, which the
        // tree layer lays out cell by cell.
        let by_node = Writes {
            merks: vec![(subtree(1), merk.clone())],
            ..Writes::default()
        };
        let written = by_node.commit(&db);
        assert_eq!(written.acquired, 0);
        assert!(written.value_bytes > 0);

        let (counters, published) = load(&db, subtree(2), merk.clone());
        assert_eq!(counters.nodes, merk.len() as u64);
        assert_eq!(counters.values, long);
        let layout = Layout::proposed(db.page_size());
        let fill = counters.fill_percent(&layout);
        assert!(fill >= 90.0, "the pages of the load are {fill:.1}% full");
        assert_eq!(published.created.len() as u64, counters.pages);
        // The values are in the value log before the commit, which takes
        // their pointers over: its group carries none.
        assert_eq!(published.acquired as u64, long);
        assert_eq!((published.value_bytes, published.value_pages), (0, 0));
        assert!(
            published.created.len() < written.created.len(),
            "{} pages of the load, {} of the write",
            published.created.len(),
            written.created.len()
        );

        let (one, two) = (dump(&db, subtree(1)), dump(&db, subtree(2)));
        assert_eq!(one.as_ref().map(Vec::len), Some(merk.len()));
        assert_eq!(one, two);
        db.close().expect("the database closes");
    }
    let db = Db::open(dir.path(), &options()).expect("the database opens again");
    let (one, two) = (dump(&db, subtree(1)), dump(&db, subtree(2)));
    assert_eq!(one.as_ref().map(Vec::len), Some(merk.len()));
    assert_eq!(one, two);
    // Each value of the load is counted in its file, and so is each of the
    // other Merk once write-back has placed it.
    wait_for("the values of the write are placed", || {
        let counts = db.shared.store.values.counts();
        counts.iter().map(|count| count.values).sum::<u64>() == 2 * long
    });
    halt_background(&db);
    sound(&db, "after the reopen");
}

/// What a sweep of power cuts over the commit of a load came to.
#[derive(Debug, Default)]
struct Swept {
    /// The cuts: one after each change to the files.
    cuts: u32,
    /// How many of them came after the flush had returned.
    durable_cuts: u32,
    /// The states of the files that were opened.
    states: u32,
    /// In how many of them the subtree of the load had no node, and in
    /// how many it had the whole Merk.
    without: u32,
    with: u32,
}

/// Cuts the power after each change to the files that a load of `merk`,
/// its commit, the flush after it, a pass of write-back and a table
/// checkpoint make, and opens the database in the states each cut can
/// leave: nothing kept that was not synced, all of it, all of it up to an
/// operation, and states drawn from the seed. With `every` the last two
/// are every operation and six draws, and otherwise at most twelve
/// operations, evenly spread, and two draws.
///
/// In each state the next open has
///
/// - the Merk that was there before, whole;
/// - the loaded Merk with every node and every value, or its subtree with
///   no node, and never anything between: with every node wherever the
///   flush had returned before the cut;
/// - a store that the physical checker finds sound, which counts the
///   values of each value-log file against the cells that point into it.
///
/// Where nothing was kept and where all was, the load is made again if it
/// is not there, and is whole then.
fn sweep_power_cuts(merk: &Merk, every: bool) -> Swept {
    let before = mixed(b'b', 120);
    let whole = Merk::len(merk);
    let mut swept = Swept::default();
    for cut in 1.. {
        let fs = SimVfs::new(SimConfig::new(4_010));
        let options = |fs: &SimVfs| Options {
            vfs: Arc::new(fs.clone()),
            ..options()
        };
        let db = Db::open("/db", &options(&fs)).expect("a new database");
        halt_background(&db);
        entered(&db, &[subtree(1), subtree(9)]);
        load(&db, subtree(9), before.clone());
        db.flush().expect("what was there before is durable");

        fs.crash_at(CrashPoint::AfterChange(fs.stats().changes + cut));
        // The load, its commit, and what makes it durable and writes it
        // back. A step that the cut fails ends them.
        let flushed = (|| {
            let writes = Writes {
                merks: vec![(subtree(1), merk.clone())],
                load: true,
                ..Writes::default()
            };
            let txn = db.begin();
            let scope = txn.scope().ok()?;
            let applied = writes.with(|batch| scope.apply(batch));
            scope.merge();
            applied.ok()?;
            db.commit(&txn, &mut NoStaleHashes).ok()?;
            let flushed = db.flush().is_ok();
            let write_back = db.shared.write_back();
            let _ = write_back.pass().and_then(|_| write_back.checkpoint());
            Some(flushed)
        })();
        let flushed = flushed == Some(true);
        if !fs.is_powered_off() {
            // Everything was done before the cut.
            assert!(flushed);
            assert_eq!(dump(&db, subtree(1)).map(|nodes| nodes.len()), Some(whole));
            drop(db);
            break;
        }
        swept.cuts += 1;
        swept.durable_cuts += u32::from(flushed);
        let image = fs.crash_image();
        drop(db);

        let mut plans = vec![image.lose_all(), image.keep_all()];
        let (stride, draws) = match every {
            true => (1, 6),
            false => (image.ops() / 12 + 1, 2),
        };
        plans.extend(
            (0..=image.ops())
                .step_by(stride as usize)
                .map(|op| image.keep_through(op)),
        );
        plans.extend((0..draws).map(|draw| image.sample(draw)));
        plans.dedup();
        for (index, plan) in plans.iter().enumerate() {
            let context = format!("a cut after change {cut}, crash plan {index}");
            let Ok(left) = image.recover(plan) else {
                continue;
            };
            swept.states += 1;
            let db = Db::open("/db", &options(&left))
                .unwrap_or_else(|error| panic!("{context}: the next open: {error}"));
            halt_background(&db);
            let kept = dump(&db, subtree(9)).expect("the subtree of before");
            assert_eq!(kept.len(), before.len(), "{context}: the Merk of before");
            let loaded = dump(&db, subtree(1)).expect("the subtree of the load");
            match loaded.len() {
                0 => {
                    assert!(!flushed, "{context}: a load that was durable is gone");
                    swept.without += 1;
                }
                nodes => {
                    assert_eq!(nodes, whole, "{context}: a part of the Merk");
                    swept.with += 1;
                }
            }
            sound(&db, &context);
            if loaded.is_empty() && (every || index < 2) {
                // The store takes the load again.
                let (counters, _) = load(&db, subtree(1), merk.clone());
                assert_eq!(counters.nodes, whole as u64, "{context}");
                assert_eq!(dump(&db, subtree(1)).map(|nodes| nodes.len()), Some(whole));
                sound(&db, &format!("{context}, loaded again"));
            }
            drop(db);
        }
    }
    println!("{swept:?}");
    swept
}

/// TXN-10's last "done when": **a power cut at any point of a load's
/// commit leaves the store without the Merk or with all of it**
/// ([`sweep_power_cuts`]). The load is of a Merk of a few pages with
/// thirty values out of line, and of each cut a dozen states are opened.
#[test]
fn a_power_cut_at_any_point_of_a_loads_commit_leaves_no_merk_or_all_of_it() {
    let swept = sweep_power_cuts(&mixed(b'p', 90), false);
    assert!(swept.cuts > 20, "{swept:?}");
    assert!(
        swept.without > 0 && swept.with > 0 && swept.durable_cuts > 0,
        "{swept:?}"
    );
}

/// The same sweep over a load of 700 nodes with 234 values out of line,
/// with every state that keeps the changes up to one operation: about
/// 280 cuts and 30,000 states, a quarter of an hour in a debug build.
#[test]
#[ignore = "about 30,000 states of a power cut; a quarter of an hour"]
fn a_power_cut_at_any_point_of_a_larger_loads_commit_in_every_state() {
    let swept = sweep_power_cuts(&mixed(b'p', 700), true);
    assert!(swept.cuts > 200 && swept.states > 20_000, "{swept:?}");
    assert!(
        swept.without > 0 && swept.with > 0 && swept.durable_cuts > 0,
        "{swept:?}"
    );
}

/// A balanced Merk of `count` nodes with keys of 32 bytes, stated a child
/// before its parent, whose values are all the same `value_len` bytes: the
/// batch that states it borrows one value, so what its caller holds does
/// not grow with the bytes of the values.
struct Uniform {
    keys: Vec<[u8; 32]>,
    /// The nodes in the order they are stated.
    nodes: Vec<StatedNode>,
    value: Vec<u8>,
}

/// A child of a node of a [`Uniform`] Merk: the place of its key, and the
/// heights of its two subtrees.
type StatedChild = Option<(usize, (u8, u8))>;

/// A node of a [`Uniform`] Merk: the place of its key, and its children.
type StatedNode = (usize, [StatedChild; 2]);

impl Uniform {
    fn new(count: usize, value_len: usize) -> Self {
        fn build(from: usize, to: usize, out: &mut Vec<StatedNode>) -> StatedChild {
            if from == to {
                return None;
            }
            let top = from + (to - from) / 2;
            let children = [build(from, top, out), build(top + 1, to, out)];
            let height =
                |child: StatedChild| child.map_or(0, |(_, heights)| heights.0.max(heights.1) + 1);
            out.push((top, children));
            Some((top, (height(children[0]), height(children[1]))))
        }
        let key = |index: usize| {
            let mut key = [0x6b; 32];
            key[..8].copy_from_slice(&(index as u64).to_be_bytes());
            key
        };
        let mut nodes = Vec::with_capacity(count);
        build(0, count, &mut nodes);
        Self {
            keys: (0..count).map(key).collect(),
            nodes,
            value: vec![0x76; value_len],
        }
    }

    /// Calls `run` with the batch that enters `subtree` and loads the Merk
    /// into it.
    fn with<R>(&self, subtree: Subtree, run: impl FnOnce(&Batch<'_>) -> R) -> R {
        let link = |child: StatedChild| {
            child.map(|(at, heights)| LinkPut {
                key: &self.keys[at],
                hash: &HASH,
                heights,
                aggregate: &[0],
            })
        };
        let nodes: Vec<NodePut<'_>> = (self.nodes.iter())
            .map(|(at, children)| NodePut {
                key: &self.keys[*at],
                feature: &[0],
                value_hash: &HASH,
                kv_hash: &HASH,
                left: link(children[0]),
                right: link(children[1]),
                hosted: Hosted::Keep,
                value: ValuePut::Value(&self.value),
                stale: Stale::NONE,
                at: None,
            })
            .collect();
        let root = self.nodes.last().map(|(at, _)| &self.keys[*at][..]);
        let merks = [MerkWrite {
            subtree,
            nodes: &nodes,
            removed: &[],
            root: RootPut::Set(root),
            anchored: false,
            whole: true,
        }];
        let subtrees = [SubtreeWrite::Enter {
            subtree,
            tree_type: TREE_TYPE,
            host: None,
        }];
        run(&Batch {
            records: &[],
            merks: &merks,
            subtrees: &subtrees,
        })
    }
}

/// TXN-10's third "done when", by what a transaction says it holds: the
/// write set and the pages of a load are the same whether its values out
/// of line are 300 bytes long or 6,000, and no page holds a value for the
/// commit. A load whose values are in their cells holds each twice: in its
/// cell and in the write set.
#[test]
fn what_a_load_holds_does_not_grow_with_the_bytes_of_its_values_out_of_line() {
    let held = |value_len: usize| {
        let dir = temp();
        let db = Db::open(dir.path(), &options()).expect("a new database");
        let merk = Uniform::new(4_000, value_len);
        let txn = db.begin();
        let scope = txn.scope().expect("a scope");
        let applied = merk
            .with(subtree(1), |batch| scope.apply(batch))
            .expect("a load");
        scope.merge();
        let counters = applied.load.expect("the loader laid the Merk out");
        let stats = txn.stats();
        assert_eq!(stats.value_pages, 0);
        match db.commit(&txn, &mut NoStaleHashes) {
            Ok(Committed::Published(published)) => {
                assert_eq!(published.acquired as u64, counters.values);
                assert_eq!(published.value_bytes, 0);
            }
            other => panic!("the load is committed, and not {other:?}"),
        }
        (stats.write_set_bytes, stats.tree_pages, counters.values)
    };
    let (short, long) = (held(300), held(6_000));
    assert_eq!(
        short, long,
        "20 times the bytes out of line, and the same held"
    );
    assert_eq!(long.2, 4_000);
    let inline = held(200);
    assert_eq!(inline.2, 0);
    assert!(
        inline.0 > long.0 + 4_000 * 150 && inline.1 > long.1,
        "{inline:?} {long:?}"
    );
}

/// What the system counts as the memory of this process, in bytes.
#[cfg(target_os = "linux")]
fn resident() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("the status of the process");
    let line = status.lines().find(|line| line.starts_with("VmRSS:"));
    let kib = line.and_then(|line| line.split_whitespace().nth(1));
    kib.and_then(|kib| kib.parse::<u64>().ok())
        .expect("the resident set")
        << 10
}

/// The memory a load holds, measured on the process: a Merk of
/// `LOAD_NODES` nodes (a million) whose values are `LOAD_VALUE_LEN` bytes
/// (441, the documents of the reference workload) is loaded in one
/// transaction of a database in `LOAD_DIR` (a temporary directory), and
/// the resident set is read before the load, with the load held, and after
/// its commit. Where the values are out of line, the load holds less than
/// half of their bytes, whatever it holds.
///
/// It is run alone, in a unit with a cap on its memory, so that a load
/// that held its values would not get through:
///
/// ```text
/// systemd-run --user --wait --collect -p MemoryMax=2G -p MemorySwapMax=0 --same-dir \
///     tools/slot cargo test --release -p lignindb --lib -- --ignored --nocapture \
///     the_memory_a_load_holds
/// ```
#[test]
#[ignore = "measures the memory of the process; run it alone, in a capped unit"]
#[cfg(target_os = "linux")]
fn the_memory_a_load_holds_is_measured_on_the_process() {
    let setting = |name: &str, default: usize| {
        let set = std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok());
        set.unwrap_or(default)
    };
    let (nodes, value_len) = (
        setting("LOAD_NODES", 1_000_000),
        setting("LOAD_VALUE_LEN", 441),
    );
    let kept = std::env::var_os("LOAD_DIR").map(std::path::PathBuf::from);
    let temporary = temp();
    let dir = kept.as_deref().unwrap_or(temporary.path());
    let store = dir.join("load-memory");
    if store.exists() {
        std::fs::remove_dir_all(&store).expect("the store of an earlier run is removed");
    }
    let options = Options {
        page_size: None,
        ..options()
    };
    let db = Db::open(&store, &options).expect("a new database");
    let merk = Uniform::new(nodes, value_len);
    let mib = |bytes: u64| bytes as f64 / (1u64 << 20) as f64;
    let txn = db.begin();
    let scope = txn.scope().expect("a scope");
    let (before, began) = (resident(), Instant::now());
    let (applied, stated) = merk.with(subtree(1), |batch| {
        // The batch is its caller's: what it takes is not the load's.
        let stated = resident();
        (scope.apply(batch).expect("a load"), stated)
    });
    scope.merge();
    let (held, loaded) = (resident(), began.elapsed());
    let counters = applied.load.expect("the loader laid the Merk out");
    let stats = txn.stats();
    let began = Instant::now();
    let published = match db.commit(&txn, &mut NoStaleHashes) {
        Ok(Committed::Published(published)) => published,
        other => panic!("the load is committed, and not {other:?}"),
    };
    let (after, committed) = (resident(), began.elapsed());
    let load = held.saturating_sub(stated);
    let page = db.page_size().get() as u64;
    println!(
        "{nodes} nodes with values of {value_len} bytes: {} values out of line of {:.1} MiB, {} \
         pages",
        counters.values,
        mib(counters.value_bytes),
        counters.pages
    );
    println!(
        "resident: {:.1} MiB before, {:.1} MiB with the batch stated, {:.1} MiB with the load \
         held, {:.1} MiB after the commit",
        mib(before),
        mib(stated),
        mib(held),
        mib(after)
    );
    println!(
        "the load holds {:.1} MiB, {:.0} bytes for a node: {:.1} MiB of pages ({} of {page} \
         bytes), {:.1} MiB in the arenas of the write set, and the entries and the index of the \
         write set beside them",
        mib(load),
        load as f64 / nodes as f64,
        mib(stats.tree_pages as u64 * page),
        stats.tree_pages,
        mib(stats.write_set_bytes as u64),
    );
    println!(
        "the load took {:.2} s and its commit {:.2} s; the commit took over {} pointers in a \
         group of {} bytes",
        loaded.as_secs_f64(),
        committed.as_secs_f64(),
        published.acquired,
        published.group_len
    );
    assert_eq!(published.acquired as u64, counters.values);
    assert_eq!((published.value_bytes, stats.value_pages), (0, 0));
    if counters.value_bytes > 100 << 20 {
        assert!(
            load < counters.value_bytes / 2,
            "the load holds {:.1} MiB for {:.1} MiB of values out of line",
            mib(load),
            mib(counters.value_bytes)
        );
    }
}
