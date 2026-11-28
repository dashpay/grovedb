//! "Checkpoint of the reference database within 40 ms (target 15 ms),
//! opening within 60 ms."
//!
//! The reference database is one million documents of 400 bytes with a
//! reference to each (`docs/lignindb/benchmarks.md`). GroveDB does not run
//! on this engine yet, so the store is filled here through the engine's
//! own write transaction and commit, with what Merk would hand it: three
//! Merks, of the documents, of the references and of the two elements that
//! hold their root hashes, each a search tree whose nodes are written with
//! their hashes along the path of every insert. The documents are stored
//! out of line. Then blocks of 20 transitions are committed with
//! write-back at work between them, and a checkpoint is made right after
//! a block, as Platform makes one.
//!
//! It is left out of a normal run:
//!
//! ```text
//! cargo test --release -p lignindb --lib -- --ignored --nocapture the_reference_database
//! ```
//!
//! | Variable | |
//! | --- | --- |
//! | `LIGNIN_TXN06_DIR` | the directory of the store, kept from one run to the next: a store that is there is opened and not built again. Without it the store is built in a temporary directory, which `TMPDIR` places. |
//! | `LIGNIN_TXN06_DOCUMENTS` | how many documents, 1,000,000 unless set |
//! | `LIGNIN_TXN06_CHECKPOINTS` | how many checkpoints are made, 15 unless set |
//! | `LIGNIN_TXN06_OPENS` | how many of them are opened, all unless set |
//! | `LIGNIN_TXN06_BLOCKS` | how many blocks are committed after the warm ones, a checkpoint after each of the first; as many as there are checkpoints unless set |
//! | `LIGNIN_TXN06_WARM` | how many blocks are committed, with write-back between them, before the first checkpoint: 200 unless set. They are what the log holds when it is made. |
//! | `LIGNIN_TXN06_LOG_FILE` | the length in bytes at which the log turns to its next file, the log's own unless set |
//! | `LIGNIN_TXN06_FRAMES` | how many pages the buffer pool of the store, and of each checkpoint that is opened, has room for: 16,384 unless set, which is 128 MiB. A pool is made empty and whole by an open, so its size is in the time of one. |
//! | `LIGNIN_TXN06_NO_LIMITS` | the times are printed and not held to the limits: on a disk whose syncs take seconds they measure the disk |
//! | `LIGNIN_TXN06_OPEN_ONLY` | a checkpoint that is opened is not read: the difference between a run with opens and one without is then what the opens cost, as `perf stat` counts it |
//! | `LIGNIN_TXN06_DURABLE` | the log is made durable before each checkpoint, outside its time, as it is where every commit syncs the log: the creation then asks for no sync of the log |

use std::{
    any::Any,
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use lignin_format::tree_page::Hash;
use lignin_io::{DirEntry, OpenMode, StdVfs, SyncItem, Vfs, VfsFile, VfsLock};
use lignin_store::{flush::FlushConfig, segment::read_manifest, wal::LogOptions};
use lignin_tree::{hash::Stale, node::NodeHandle};

use super::{checkpointer, checkpoints_in, root_hash, store_files, ROOT};
use crate::{
    snapshot::Versions,
    txn::{
        tests::{
            model,
            store::{mix, Db, Outcome, SIZE},
            value_of,
        },
        Batch, Column, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite, RootPut, Subtree,
        ValuePut,
    },
};

const DOCUMENTS: u8 = 61;
const REFERENCES: u8 = 62;
const GROVE: u8 = ROOT;

/// The transitions of a block.
const BLOCK: u64 = 20;
/// The transitions of one commit while the store is filled.
const LOAD_BLOCK: u64 = 5_000;
/// The blocks that are committed, with write-back between them, before
/// the first checkpoint is made, unless `LIGNIN_TXN06_WARM` says how many.
const WARM_BLOCKS: u64 = 200;

type Key = [u8; 16];

const NO_NODE: u32 = u32::MAX;

/// A node of a Merk as its writer holds it: the key, the hashes, and the
/// links by which the children are known.
struct Node {
    key: Key,
    value_hash: Hash,
    kv_hash: Hash,
    hash: Hash,
    /// The feature type of a tree that counts, with the count below.
    feature: [u8; 9],
    left: u32,
    right: u32,
    height: u8,
    /// The write that last changed the node.
    written: u32,
}

/// A Merk as its writer holds it, with the nodes that the writes since
/// the last batch changed. Its keys are scattered, so that it stays as
/// low as a balanced tree without a rotation.
struct Merk {
    subtree: Subtree,
    nodes: Vec<Node>,
    root: u32,
    changed: Vec<u32>,
    /// The values that were written, by node.
    values: HashMap<u32, Vec<u8>>,
    root_set: bool,
    batch: u32,
}

impl Merk {
    fn new(name: u8) -> Self {
        Self {
            subtree: model::subtree(name),
            nodes: Vec::new(),
            root: NO_NODE,
            changed: Vec::new(),
            values: HashMap::new(),
            root_set: false,
            batch: 1,
        }
    }

    fn root_hash(&self) -> Hash {
        match self.root {
            NO_NODE => [0; 32],
            root => self.nodes[root as usize].hash,
        }
    }

    /// Puts `value` under `key`, and makes the hashes of the node and of
    /// every node above it, as a Merk does that propagates to its root.
    fn put(&mut self, key: Key, value: &[u8]) {
        let mut path = Vec::with_capacity(48);
        let mut at = self.root;
        let mut found = NO_NODE;
        while at != NO_NODE {
            path.push(at);
            let node = &self.nodes[at as usize];
            at = match key.cmp(&node.key) {
                std::cmp::Ordering::Less => node.left,
                std::cmp::Ordering::Greater => node.right,
                std::cmp::Ordering::Equal => {
                    found = at;
                    break;
                }
            };
        }
        let value_hash = model::hash_of(&[value]);
        let kv_hash = model::hash_of(&[&key, &value_hash]);
        if found == NO_NODE {
            found = self.nodes.len() as u32;
            self.nodes.push(Node {
                key,
                value_hash,
                kv_hash,
                hash: [0; 32],
                feature: [0; 9],
                left: NO_NODE,
                right: NO_NODE,
                height: 1,
                written: 0,
            });
            match path.last() {
                Some(&parent) => {
                    let parent = &mut self.nodes[parent as usize];
                    match key < parent.key {
                        true => parent.left = found,
                        false => parent.right = found,
                    }
                }
                None => {
                    self.root = found;
                    self.root_set = true;
                }
            }
            path.push(found);
        } else {
            let node = &mut self.nodes[found as usize];
            (node.value_hash, node.kv_hash) = (value_hash, kv_hash);
        }
        self.values.insert(found, value.to_vec());
        for &at in path.iter().rev() {
            self.make(at);
            let node = &mut self.nodes[at as usize];
            if node.written != self.batch {
                node.written = self.batch;
                self.changed.push(at);
            }
        }
    }

    fn make(&mut self, at: u32) {
        let none = [0; 32];
        let of = |child: u32| match child {
            NO_NODE => (&none, 0, 0),
            child => {
                let child = &self.nodes[child as usize];
                let count = u64::from_le_bytes(child.feature[1..].try_into().expect("8 bytes"));
                (&child.hash, child.height, count)
            }
        };
        let node = &self.nodes[at as usize];
        let ((left, left_height, left_count), (right, right_height, right_count)) =
            (of(node.left), of(node.right));
        let hash = model::hash_of(&[&node.kv_hash, left, right]);
        let height = left_height.max(right_height) + 1;
        assert!(height < 200, "the tree is not low");
        let mut feature = [3; 9];
        feature[1..].copy_from_slice(&(1 + left_count + right_count).to_le_bytes());
        let node = &mut self.nodes[at as usize];
        (node.hash, node.height, node.feature) = (hash, height, feature);
    }

    fn link(&self, child: u32) -> Option<LinkPut<'_>> {
        let child = self.nodes.get(child as usize)?;
        let height = |child: u32| {
            self.nodes
                .get(child as usize)
                .map_or(0, |child| child.height)
        };
        Some(LinkPut {
            key: &child.key,
            hash: &child.hash,
            heights: (height(child.left), height(child.right)),
            aggregate: &child.feature,
        })
    }

    /// The nodes that changed since the last batch, each as Merk hands it
    /// to storage: whole, and with its value where that was written. A
    /// node that keeps its value is read with `read` first, as Merk loads
    /// a node before it changes it, and names that read.
    fn puts(&self, mut read: impl FnMut(&[u8]) -> Option<NodeHandle>) -> Vec<NodePut<'_>> {
        (self.changed.iter())
            .map(|&at| {
                let node = &self.nodes[at as usize];
                let value = self.values.get(&at);
                NodePut {
                    key: &node.key,
                    feature: &node.feature,
                    value_hash: &node.value_hash,
                    kv_hash: &node.kv_hash,
                    left: self.link(node.left),
                    right: self.link(node.right),
                    hosted: Hosted::Keep,
                    value: match value {
                        Some(value) => ValuePut::Value(value),
                        None => ValuePut::Keep,
                    },
                    stale: Stale::NONE,
                    at: match value {
                        Some(_) => None,
                        None => read(&node.key),
                    },
                }
            })
            .collect()
    }

    fn write<'a>(&'a self, nodes: &'a [NodePut<'a>]) -> MerkWrite<'a> {
        MerkWrite {
            subtree: self.subtree,
            nodes,
            removed: &[],
            root: match self.root_set {
                true => RootPut::Set(self.nodes.get(self.root as usize).map(|root| &root.key[..])),
                false => RootPut::Keep,
            },
            anchored: false,
            whole: false,
        }
    }

    /// The batch was applied, or is not to be.
    fn written(&mut self) {
        self.changed.clear();
        self.values.clear();
        self.root_set = false;
        self.batch += 1;
    }
}

fn scattered(kind: &[u8], serial: u64) -> Key {
    let mut key = [0; 16];
    key[..8].copy_from_slice(&model::hash_of(&[kind, &serial.to_be_bytes()])[..8]);
    key[8..].copy_from_slice(&serial.to_be_bytes());
    key
}

fn element_key(name: &[u8]) -> Key {
    let mut key = [0; 16];
    key[..name.len()].copy_from_slice(name);
    key
}

/// The document of `serial`: 400 bytes that no other document has.
fn document(serial: u64) -> Vec<u8> {
    let mut state = mix(serial, b"document");
    (0..50)
        .flat_map(|_| {
            state = mix(state, &serial.to_le_bytes());
            state.to_le_bytes()
        })
        .collect()
}

/// The three Merks of the grove, as their writer holds them.
struct Grove {
    documents: Merk,
    references: Merk,
    root: Merk,
    transitions: u64,
}

impl Grove {
    fn new() -> Self {
        Self {
            documents: Merk::new(DOCUMENTS),
            references: Merk::new(REFERENCES),
            root: Merk::new(GROVE),
            transitions: 0,
        }
    }

    /// One state transition: a document and a reference to it, and the
    /// two elements of the root Merk with the root hashes they hold.
    fn transition(&mut self) {
        let serial = self.transitions;
        let key = scattered(b"document", serial);
        self.documents.put(key, &document(serial));
        self.references.put(scattered(b"reference", serial), &key);
        (self.root).put(element_key(b"documents"), &self.documents.root_hash());
        (self.root).put(element_key(b"references"), &self.references.root_hash());
        self.transitions += 1;
    }

    /// Applies what the transitions since the last batch wrote, as one
    /// operation of `txn`.
    fn apply(&mut self, txn: &crate::txn::WriteTxn<'_>) {
        let state = self.transitions.to_be_bytes();
        let scope = txn.scope().expect("a scope");
        let merks = [&self.documents, &self.references, &self.root];
        let nodes = merks.map(|merk| {
            let read = |key: &[u8]| scope.node(merk.subtree, key, model::handle_of);
            merk.puts(|key| read(key).expect("a read"))
        });
        let writes = [0, 1, 2].map(|at| merks[at].write(&nodes[at]));
        let records = [RecordWrite::put(
            Column::Aux,
            model::subtree(GROVE),
            b"state",
            &state,
        )];
        (scope.apply(&Batch {
            records: &records,
            merks: &writes,
            subtrees: &[],
        }))
        .expect("the batch is applied");
        scope.merge();
        self.written();
    }

    fn written(&mut self) {
        for merk in [&mut self.documents, &mut self.references, &mut self.root] {
            merk.written();
        }
    }

    /// Commits `count` transitions as one block. Each is an operation of
    /// its own where `apart`, as Platform applies them, and all are one
    /// operation otherwise, as a load applies them.
    fn block(&mut self, db: &Db, versions: &Versions, count: u64, apart: bool) {
        let txn = db.path(versions).begin();
        for _ in 0..count {
            self.transition();
            if apart {
                self.apply(&txn);
            }
        }
        if !apart {
            self.apply(&txn);
        }
        let outcome = db.commit(versions, &txn).expect("a commit");
        assert!(matches!(outcome, Outcome::Committed(_)), "{outcome:?}");
    }

    /// The transitions made in memory alone: the store has them already.
    fn remember(&mut self, count: u64) {
        for _ in 0..count {
            self.transition();
        }
        self.written();
    }
}

/// A file layer that counts what a creation asks of the system, and how
/// long the system took for it. The syncs are those that are asked for one
/// by one, and `in_sets` those of the sets (`Vfs::sync_together`).
#[derive(Debug, Default)]
struct Counts {
    links: AtomicU64,
    syncs: AtomicU64,
    dir_syncs: AtomicU64,
    opens: AtomicU64,
    sets: AtomicU64,
    in_sets: AtomicU64,
    /// Nanoseconds in the links, in the syncs that were asked for one by
    /// one, in the sets, and in every other call on a name.
    link_time: AtomicU64,
    sync_time: AtomicU64,
    set_time: AtomicU64,
    name_time: AtomicU64,
}

/// Runs `call` and adds the time it took to `spent`.
fn clocked<T>(spent: &AtomicU64, call: impl FnOnce() -> T) -> T {
    let began = Instant::now();
    let answer = call();
    spent.fetch_add(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
    answer
}

#[derive(Debug)]
struct Counting {
    inner: StdVfs,
    counts: Arc<Counts>,
}

#[derive(Debug)]
struct CountingFile {
    inner: Box<dyn VfsFile>,
    counts: Arc<Counts>,
}

impl VfsFile for CountingFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.inner.read_at(buf, offset)
    }

    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        self.inner.read_exact_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.inner.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.inner.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.counts.syncs.fetch_add(1, Ordering::Relaxed);
        clocked(&self.counts.sync_time, || self.inner.sync_data())
    }

    fn sync_all(&self) -> io::Result<()> {
        self.counts.syncs.fetch_add(1, Ordering::Relaxed);
        clocked(&self.counts.sync_time, || self.inner.sync_all())
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
}

impl Vfs for Counting {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        self.counts.opens.fetch_add(1, Ordering::Relaxed);
        let inner = clocked(&self.counts.name_time, || self.inner.open(path, mode))?;
        Ok(Box::new(CountingFile {
            inner,
            counts: self.counts.clone(),
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        clocked(&self.counts.name_time, || self.inner.rename(from, to))
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.counts.links.fetch_add(1, Ordering::Relaxed);
        clocked(&self.counts.link_time, || {
            self.inner.hard_link(original, link)
        })
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        clocked(&self.counts.name_time, || self.inner.remove_file(path))
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        clocked(&self.counts.name_time, || self.inner.create_dir(path))
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        clocked(&self.counts.name_time, || self.inner.remove_dir(path))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        clocked(&self.counts.name_time, || self.inner.read_dir(path))
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.counts.dir_syncs.fetch_add(1, Ordering::Relaxed);
        clocked(&self.counts.sync_time, || self.inner.sync_dir(path))
    }

    /// The set is handed to the system's file layer as a set of its own
    /// files, so that it costs there what it costs a store that is opened
    /// on that layer: a file layer shares the cost of a set only among
    /// files it knows for its own.
    fn sync_together(&self, items: &[SyncItem<'_>]) -> io::Result<()> {
        fn own(file: &dyn VfsFile) -> &dyn VfsFile {
            let any: &dyn Any = file;
            match any.downcast_ref::<CountingFile>() {
                Some(counting) => &*counting.inner,
                None => file,
            }
        }
        self.counts.sets.fetch_add(1, Ordering::Relaxed);
        (self.counts.in_sets).fetch_add(items.len() as u64, Ordering::Relaxed);
        let items: Vec<SyncItem<'_>> = (items.iter())
            .map(|item| match *item {
                SyncItem::Data(file) => SyncItem::Data(own(file)),
                SyncItem::All(file) => SyncItem::All(own(file)),
                SyncItem::Dir(path) => SyncItem::Dir(path),
            })
            .collect();
        clocked(&self.counts.set_time, || self.inner.sync_together(&items))
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        clocked(&self.counts.name_time, || self.inner.lock(path))
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        clocked(&self.counts.name_time, || self.inner.lock_existing(path))
    }
}

impl Counts {
    /// The links, the syncs of files, the syncs of directories, the opens,
    /// the sets and the syncs in them so far.
    fn read(&self) -> [u64; 6] {
        [
            &self.links,
            &self.syncs,
            &self.dir_syncs,
            &self.opens,
            &self.sets,
            &self.in_sets,
        ]
        .map(|count| count.load(Ordering::Relaxed))
    }

    /// The time so far in the links, in the syncs that were asked for one
    /// by one, in the sets, and in the other calls on a name.
    fn spent(&self) -> [Duration; 4] {
        [
            &self.link_time,
            &self.sync_time,
            &self.set_time,
            &self.name_time,
        ]
        .map(|nanos| Duration::from_nanos(nanos.load(Ordering::Relaxed)))
    }
}

fn number(variable: &str, unless_set: u64) -> u64 {
    match std::env::var(variable) {
        Ok(set) => set.parse().unwrap_or_else(|_| panic!("{variable}={set}")),
        Err(_) => unless_set,
    }
}

/// How long this thread has run on a processor, where the system says.
/// On a machine that is busy with other work it is what a call cost, and
/// the time on the clock is that and the wait for a processor. The system
/// counts it in steps of about a millisecond, so of a call that takes less
/// only the mean over many says anything.
fn on_cpu() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    let nanos = stat.split_whitespace().next()?.parse().ok()?;
    Some(Duration::from_nanos(nanos))
}

/// What `call` returns, with the time it took on the clock and on a
/// processor.
fn timed<T>(call: impl FnOnce() -> T) -> (T, Duration, Option<Duration>) {
    let (began, ran) = (Instant::now(), on_cpu());
    let returned = call();
    let took = began.elapsed();
    let on_cpu = on_cpu().zip(ran).map(|(now, then)| now - then);
    (returned, took, on_cpu)
}

fn median(samples: &[Duration]) -> Duration {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn mean(samples: &[Duration]) -> Duration {
    samples.iter().sum::<Duration>() / samples.len() as u32
}

fn millis(samples: &[Duration]) -> String {
    let each: Vec<String> = (samples.iter())
        .map(|sample| format!("{:.2}", sample.as_secs_f64() * 1e3))
        .collect();
    each.join(" ")
}

/// What write-back's loop does in a turn.
fn turn(db: &Db, versions: &Versions) {
    (db.write_back(versions).tick()).expect("a turn of write-back");
}

/// The size of everything under `dir` but the checkpoints.
fn size_on_disk(dir: &Path) -> u64 {
    (store_files(&StdVfs, dir).iter())
        .map(|file| std::fs::metadata(dir.join(file)).map_or(0, |file| file.len()))
        .sum()
}

#[test]
#[ignore = "builds a store of a million documents: see the module documentation"]
fn the_reference_database_is_checkpointed_and_opened_in_time() {
    let documents = number("LIGNIN_TXN06_DOCUMENTS", 1_000_000);
    let checkpoints = number("LIGNIN_TXN06_CHECKPOINTS", 15);
    let opens = number("LIGNIN_TXN06_OPENS", checkpoints).min(checkpoints);
    let blocks = number("LIGNIN_TXN06_BLOCKS", checkpoints).max(checkpoints);
    let warm = number("LIGNIN_TXN06_WARM", WARM_BLOCKS);
    let limits = std::env::var_os("LIGNIN_TXN06_NO_LIMITS").is_none();
    let reads = std::env::var_os("LIGNIN_TXN06_OPEN_ONLY").is_none();
    let durable_first = std::env::var_os("LIGNIN_TXN06_DURABLE").is_some();
    let frames = number("LIGNIN_TXN06_FRAMES", 16_384) as usize;
    let options = LogOptions {
        target_file_len: number(
            "LIGNIN_TXN06_LOG_FILE",
            LogOptions::default().target_file_len,
        ),
        ..LogOptions::default()
    };
    let flush = FlushConfig::new(SIZE);

    let kept = std::env::var_os("LIGNIN_TXN06_DIR").map(PathBuf::from);
    let temporary = kept
        .is_none()
        .then(|| tempfile::tempdir().expect("a directory"));
    let dir = match (&kept, &temporary) {
        (Some(kept), _) => kept.clone(),
        (None, Some(temporary)) => temporary.path().to_path_buf(),
        (None, None) => unreachable!("one of the two"),
    };
    let counts = Arc::new(Counts::default());
    let vfs: Arc<dyn Vfs> = Arc::new(Counting {
        inner: StdVfs,
        counts: counts.clone(),
    });

    let mut grove = Grove::new();
    let began = Instant::now();
    let built = read_manifest(&StdVfs, &dir).is_err();
    let (db, versions) = if built {
        std::fs::create_dir_all(&dir).expect("the directory of the store");
        let (db, versions) = Db::on(vfs.clone(), &dir, frames, options, flush, None);
        let mut loaded = 0;
        while loaded < documents {
            let count = LOAD_BLOCK.min(documents - loaded);
            grove.block(&db, &versions, count, false);
            turn(&db, &versions);
            loaded += count;
        }
        (db, versions)
    } else {
        // The roots of the keyspaces are the same in every store these
        // tests make, so any of them says which pages they are.
        let (like, _) = Db::new(16);
        let opened = Instant::now();
        let (db, versions, report) =
            Db::open(vfs.clone(), &dir, frames, options, flush, &like).expect("the store opens");
        println!(
            "the store was opened in {:.2} ms: {report:?}",
            opened.elapsed().as_secs_f64() * 1e3
        );
        // The store says how many transitions it has, and they are the
        // same transitions in every run.
        let reader = db.path(&versions).begin();
        let mut state = Vec::new();
        let unit = crate::txn::Unit::new(Column::Aux, model::subtree(GROVE));
        assert!(reader.get(unit, b"state", &mut state).expect("a read"));
        drop(reader);
        let transitions = u64::from_be_bytes(state[..].try_into().expect("a count"));
        assert!(
            transitions >= documents,
            "the store in {} has {transitions} documents",
            dir.display()
        );
        grove.remember(transitions);
        (db, versions)
    };
    println!(
        "{} documents and {} references, {} in {:.1} s: {:.0} MiB on disk, version {}",
        grove.documents.nodes.len(),
        grove.references.nodes.len(),
        if built { "built" } else { "opened" },
        began.elapsed().as_secs_f64(),
        size_on_disk(&dir) as f64 / (1 << 20) as f64,
        versions.latest(),
    );
    assert_eq!(
        root_hash(&db, &versions, versions.latest()),
        Some(grove.root.root_hash())
    );
    for _ in 0..warm {
        grove.block(&db, &versions, BLOCK, true);
        turn(&db, &versions);
    }

    // A checkpoint right after each block, and write-back after it.
    let targets = checkpoints_in(&db);
    let mut made = Vec::new();
    let (mut taken, mut taken_on_cpu) = (Vec::new(), Vec::new());
    // Of the checkpoints with no catch-up: the whole time, and the time in
    // the links, in the syncs one by one, in the sets and in the other
    // calls on names.
    let mut parts: [Vec<Duration>; 5] = Default::default();
    for index in 0..blocks {
        grove.block(&db, &versions, BLOCK, true);
        if index >= checkpoints {
            turn(&db, &versions);
            continue;
        }
        let target = targets.join(format!("{}-{index}", versions.latest()));
        let held = db.write_back(&versions).log_held();
        if durable_first {
            (db.write_back(&versions).sync_log()).expect("the log is made durable");
        }
        let (before, spent_before) = (counts.read(), counts.spent());
        let (created, took, on_cpu) = timed(|| checkpointer(&db, &versions).create(&target));
        let created = created.expect("a checkpoint");
        taken.push(took);
        taken_on_cpu.extend(on_cpu);
        let (after, spent_after) = (counts.read(), counts.spent());
        let [links, syncs, dir_syncs, file_opens, sets, in_sets] =
            [0, 1, 2, 3, 4, 5].map(|at| after[at] - before[at]);
        let spent = [0, 1, 2, 3].map(|at| spent_after[at] - spent_before[at]);
        println!(
            "checkpoint {index}: version {}, {} files, {held} bytes of log held, \
             {links} links, {syncs} syncs of files and {dir_syncs} of directories one by one, \
             {sets} sets of {in_sets} syncs, {file_opens} opens, catch-up {:?}; \
             {:.2} ms: {} in the links, the syncs one by one, the sets, other calls on names",
            created.version,
            created.linked,
            created.catch_up,
            took.as_secs_f64() * 1e3,
            millis(&spent)
        );
        if created.catch_up.is_none() {
            parts[0].push(took);
            for (part, spent) in parts[1..].iter_mut().zip(spent) {
                part.push(spent);
            }
        }
        assert_eq!(created.version, versions.latest());
        assert_eq!(created.copied, 0);
        made.push((
            target,
            created.version,
            grove.root.root_hash(),
            grove.transitions,
        ));
        turn(&db, &versions);
    }

    let (like, _) = Db::new(16);
    let (mut opened_in, mut opened_on_cpu) = (Vec::new(), Vec::new());
    // Of each open: the whole time, and the time in the syncs one by one,
    // in the sets and in the other calls on names.
    let mut open_parts: [Vec<Duration>; 4] = Default::default();
    for (target, version, hash, transitions) in made.iter().take(opens as usize) {
        let (before, spent_before) = (counts.read(), counts.spent());
        let (opened, took, on_cpu) =
            timed(|| Db::open(vfs.clone(), target, frames, options, flush, &like));
        let (checkpoint, its_versions, report) = opened.expect("the checkpoint opens");
        opened_in.push(took);
        opened_on_cpu.extend(on_cpu);
        let (after, spent_after) = (counts.read(), counts.spent());
        let [_, syncs, dir_syncs, file_opens, sets, in_sets] =
            [0, 1, 2, 3, 4, 5].map(|at| after[at] - before[at]);
        let spent = [1, 2, 3].map(|at| spent_after[at] - spent_before[at]);
        println!(
            "opened version {} in {:.2} ms: {} bytes of log read, {} commits replayed, \
             {} pages rebuilt; {syncs} syncs of files and {dir_syncs} of directories one by \
             one, {sets} sets of {in_sets} syncs, {file_opens} opens; ms: {} in the syncs one \
             by one, the sets, other calls on names",
            report.landed,
            took.as_secs_f64() * 1e3,
            report.log_bytes,
            report.commits,
            report.pages_rebuilt,
            millis(&spent)
        );
        open_parts[0].push(took);
        for (part, spent) in open_parts[1..].iter_mut().zip(spent) {
            part.push(spent);
        }
        assert_eq!(its_versions.latest(), *version);
        if !reads {
            continue;
        }
        assert_eq!(
            root_hash(&checkpoint, &its_versions, *version),
            Some(*hash),
            "the root hash of version {version}"
        );
        // Documents of the whole history, the last one among them, and
        // none of a later block.
        let reader = checkpoint.path(&its_versions).begin();
        let subtree = model::subtree(DOCUMENTS);
        let serials = (0..40).map(|step| step * (transitions - 1) / 39);
        for serial in serials.chain([*transitions]) {
            let key = scattered(b"document", serial);
            let found = reader
                .node(subtree, &key, |view| view.as_ref().map(value_of))
                .expect("a read");
            let expected = (serial < *transitions).then(|| document(serial));
            assert_eq!(found, expected, "document {serial} at version {version}");
        }
    }
    for (what, samples) in [
        ("checkpoint", &taken),
        ("checkpoint, on a processor", &taken_on_cpu),
        ("open of a checkpoint", &opened_in),
        ("open of a checkpoint, on a processor", &opened_on_cpu),
    ] {
        if !samples.is_empty() {
            println!(
                "{what} (ms): median {:.2}, mean {:.2}; {}",
                median(samples).as_secs_f64() * 1e3,
                mean(samples).as_secs_f64() * 1e3,
                millis(samples)
            );
        }
    }
    if !parts[0].is_empty() {
        println!(
            "a checkpoint with no catch-up, {} of them (ms, the median of each): {} whole, in \
             the links, in the syncs one by one, in the sets, in other calls on names",
            parts[0].len(),
            millis(&parts.each_ref().map(|part| median(part)))
        );
    }
    if !open_parts[0].is_empty() {
        println!(
            "an open of a checkpoint, {} of them (ms, the median of each): {} whole, in the \
             syncs one by one, in the sets, in other calls on names",
            open_parts[0].len(),
            millis(&open_parts.each_ref().map(|part| median(part)))
        );
    }
    println!(
        "{:.0} MiB on disk in {} files",
        size_on_disk(&dir) as f64 / (1 << 20) as f64,
        store_files(&StdVfs, &dir).len()
    );
    // The session ends as one does, so that a store that is kept opens
    // without a log to replay.
    for (target, ..) in &made {
        std::fs::remove_dir_all(target).expect("a checkpoint is removed");
    }
    if kept.is_some() {
        (db.write_back(&versions).finish()).expect("the session ends");
        println!("the store is kept in {}", dir.display());
    }
    if limits && !made.is_empty() {
        assert!(
            median(&taken) <= Duration::from_millis(40),
            "a checkpoint takes {:?}",
            median(&taken)
        );
    }
    if limits && !opened_in.is_empty() {
        assert!(
            median(&opened_in) <= Duration::from_millis(60),
            "the open of a checkpoint takes {:?}",
            median(&opened_in)
        );
    }
}
