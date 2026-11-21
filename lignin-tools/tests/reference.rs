//! OPS-01's acceptance test: "Every subcommand works on a live checkpoint
//! of the reference database."
//!
//! **The reference database** is one million documents of 400 bytes over
//! 10,000 owners, each with a reference in the `$ownerId` index
//! (`docs/lignindb/benchmarks.md`), in Drive's layout: the root, three
//! trees down to the document type, the type's primary tree of documents
//! and its index tree, and under the index one tree for each owner with a
//! tree of the owner's references. That is 20,007 Merks and 2,020,006
//! nodes, the store of the reference size that M3's gate checked (D-218).
//! GroveDB does not load through the engine yet (GROVE-10), so the grove is
//! built here as GroveDB would write it: every value a serialized element,
//! every hash made by Merk's functions, and the whole grove in one write
//! transaction through the engine's facade.
//!
//! **Live.** After the build the database stays open, and a writer thread
//! commits blocks of 20 transitions, each the update of one document and
//! of the path above it to the root, as Merk writes it. Some blocks in,
//! the checkpoint is made beside that writer ([`Db::checkpoint`]), and the
//! writer goes on committing while every subcommand runs on the
//! checkpoint:
//!
//! 1. `manifest` and `segments --blocks`, before anything opens the
//!    checkpoint;
//! 2. `pages` (every page, and a few with `--cells`), `directory`,
//!    `stats`;
//! 3. `verify` on the still store and `--beside-background`, both making
//!    every hash again by Merk's rules;
//! 4. `dump --all`, and `dump` of one owner's tree; `load` of each into a
//!    new database; `verify` of the whole load, whose dump is byte for
//!    byte the checkpoint's;
//! 5. `compact`, then `verify` and `stats` once more.
//!
//! The root hash the checkpoint holds is compared with the one the writer
//! had made at the checkpoint's version. Each report goes to
//! `<dir>/reports/`, and the run prints the time of each subcommand
//! (on this machine, as an indication and not as a measurement).
//!
//! It is left out of a normal run, and wants a release build:
//!
//! ```text
//! LIGNIN_OPS01_DIR=/path cargo test --release -p lignin-tools --test reference -- \
//!     --ignored --nocapture
//! ```
//!
//! | Variable | |
//! | --- | --- |
//! | `LIGNIN_OPS01_DIR` | where the databases, the dumps and the reports go: a directory that is empty or not there, which is made. A temporary directory, which `TMPDIR` places, unless given. |
//! | `LIGNIN_OPS01_DOCUMENTS` | how many documents: 1,000,000 unless given |
//! | `LIGNIN_OPS01_OWNERS` | how many owners: 10,000 unless given |
//! | `LIGNIN_OPS01_WARM` | how many blocks are committed before the checkpoint: 100 unless given |

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
    time::Instant,
};

use grovedb_element::{reference_path::ReferencePathType, Element};
use grovedb_merk::tree::{combine_hash, kv_digest_to_kv_hash, node_hash, value_hash, NULL_HASH};
use grovedb_version::version::GroveVersion;
use lignin_format::{tree_page::Hash, CommitVersion};
use lignin_tools::Outcome;
use lignin_tree::hash::Stale;
use lignindb::{
    checkpoint::CheckpointOptions,
    commit::{Committed, NoStaleHashes},
    txn::{
        Batch, HostPut, Hosted, LinkPut, MerkWrite, NodePut, RootPut, Subtree, SubtreeWrite,
        ValuePut,
    },
    Db, Options,
};

const FEATURE: &[u8] = &[0];
const AGGREGATE: &[u8] = &[0];
const TREE: &[u8] = &[0];
const TRANSITIONS: usize = 20;

fn setting(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .map_or(default, |value| value.parse().expect("a number"))
}

/// 64 bits of `seed`, mixed (splitmix64).
fn mix(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// 32 bytes of `seed` in `domain`.
fn id(domain: u64, seed: u64) -> [u8; 32] {
    let mut id = [0; 32];
    for (at, chunk) in id.chunks_mut(8).enumerate() {
        chunk.copy_from_slice(&mix(domain << 48 ^ seed << 2 ^ at as u64).to_be_bytes());
    }
    id
}

fn serialized(element: Element) -> Vec<u8> {
    element
        .serialize(GroveVersion::latest())
        .expect("serialized")
}

/// The value of a document, at its `version`.
fn document(number: u64, version: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(400);
    while bytes.len() < 400 {
        bytes.extend_from_slice(&mix(number ^ version << 40 ^ bytes.len() as u64).to_le_bytes());
    }
    serialized(Element::new_item(bytes))
}

/// One node of a Merk of the model.
struct Node {
    key: Vec<u8>,
    value: Vec<u8>,
    /// What the value hash combines with the hash of the value: the root
    /// of the hosted Merk, or, for a reference, the value hash of what it
    /// names.
    combined: Option<Hash>,
    hosted: Vec<Subtree>,
    value_hash: Hash,
    kv_hash: Hash,
    hash: Hash,
    left: Option<u32>,
    right: Option<u32>,
    parent: Option<u32>,
    heights: (u8, u8),
}

/// One Merk of the model.
struct Merk {
    subtree: Subtree,
    /// The Merk and the node that host this one, and its position.
    host: Option<(usize, u32)>,
    nodes: Vec<Node>,
    root: Option<u32>,
}

/// A node of a Merk to build: its key, its value, the root hash of the
/// Merk it hosts, and the subtrees it hosts.
type Item = (Vec<u8>, Vec<u8>, Option<Hash>, Vec<Subtree>);

impl Merk {
    fn root_hash(&self) -> Hash {
        self.root
            .map_or(NULL_HASH, |root| self.nodes[root as usize].hash)
    }

    fn height(&self, node: Option<u32>) -> u8 {
        node.map_or(0, |at| {
            let (left, right) = self.nodes[at as usize].heights;
            left.max(right) + 1
        })
    }

    fn hash_of(&self, node: Option<u32>) -> Hash {
        node.map_or(NULL_HASH, |at| self.nodes[at as usize].hash)
    }

    /// Makes the hashes of the node `at` again from its value and its
    /// children.
    fn rehash(&mut self, at: u32) {
        let (left, right) = {
            let node = &self.nodes[at as usize];
            (node.left, node.right)
        };
        let (left_hash, right_hash) = (self.hash_of(left), self.hash_of(right));
        let heights = (self.height(left), self.height(right));
        let node = &mut self.nodes[at as usize];
        let own = value_hash(&node.value).unwrap();
        node.value_hash = match node.combined {
            Some(other) => combine_hash(&own, &other).unwrap(),
            None => own,
        };
        node.kv_hash = kv_digest_to_kv_hash(&node.key, &node.value_hash).unwrap();
        node.hash = node_hash(&node.kv_hash, &left_hash, &right_hash).unwrap();
        node.heights = heights;
    }

    /// A Merk of `items`, which are in key order, as a balanced tree.
    fn of(subtree: Subtree, items: Vec<Item>) -> Self {
        fn build(merk: &mut Merk, low: usize, high: usize, parent: Option<u32>) -> Option<u32> {
            if low == high {
                return None;
            }
            let top = (low + high) / 2;
            merk.nodes[top].parent = parent;
            let at = top as u32;
            merk.nodes[top].left = build(merk, low, top, Some(at));
            merk.nodes[top].right = build(merk, top + 1, high, Some(at));
            merk.rehash(at);
            Some(at)
        }
        let nodes: Vec<Node> = items
            .into_iter()
            .map(|(key, value, combined, hosted)| Node {
                key,
                value,
                combined,
                hosted,
                value_hash: NULL_HASH,
                kv_hash: NULL_HASH,
                hash: NULL_HASH,
                left: None,
                right: None,
                parent: None,
                heights: (0, 0),
            })
            .collect();
        let len = nodes.len();
        let mut merk = Self {
            subtree,
            host: None,
            nodes,
            root: None,
        };
        merk.root = build(&mut merk, 0, len, None);
        merk
    }

    fn put(&self, at: u32) -> NodePut<'_> {
        let node = &self.nodes[at as usize];
        let link = |child: Option<u32>| {
            child.map(|child| {
                let child = &self.nodes[child as usize];
                LinkPut {
                    key: &child.key,
                    hash: &child.hash,
                    heights: child.heights,
                    aggregate: AGGREGATE,
                }
            })
        };
        NodePut {
            key: &node.key,
            feature: FEATURE,
            value_hash: &node.value_hash,
            kv_hash: &node.kv_hash,
            left: link(node.left),
            right: link(node.right),
            hosted: Hosted::Set(&node.hosted),
            value: ValuePut::Value(&node.value),
            stale: Stale::NONE,
            at: None,
        }
    }
}

/// The grove of the model: the Merks in the order they are entered, each
/// after its host.
struct Grove {
    merks: Vec<Merk>,
    /// The Merk of the documents.
    primary: usize,
    /// How many times each document was written.
    versions: Vec<u64>,
}

/// The tree element that hosts a Merk whose root key is `root`.
fn tree(root: Option<&[u8]>) -> Vec<u8> {
    serialized(Element::new_tree(root.map(<[u8]>::to_vec)))
}

impl Grove {
    /// The reference grove: `documents` over `owners`.
    fn reference(documents: u64, owners: u64) -> Self {
        let subtree = |number: u64| Subtree::new(id(1, number));
        let mut built: Vec<Merk> = Vec::new();
        let mut next = 0;
        let mut fresh = || {
            next += 1;
            subtree(next)
        };
        // The documents, by id.
        let mut docs: Vec<(Vec<u8>, u64)> = (0..documents)
            .map(|number| (id(2, number).to_vec(), number))
            .collect();
        docs.sort();
        let primary_subtree = fresh();
        let primary = Merk::of(
            primary_subtree,
            docs.iter()
                .map(|(key, number)| (key.clone(), document(*number, 0), None, Vec::new()))
                .collect(),
        );
        let doc_hash: BTreeMap<&[u8], Hash> = (primary.nodes.iter())
            .map(|node| (node.key.as_slice(), node.value_hash))
            .collect();
        // Each owner's references, and the owner's tree above them.
        let mut by_owner: BTreeMap<Vec<u8>, Vec<&[u8]>> = BTreeMap::new();
        for (key, number) in &docs {
            let owner = id(3, number % owners).to_vec();
            by_owner.entry(owner).or_default().push(key);
        }
        let mut owner_items = Vec::new();
        let mut families = Vec::new();
        for (owner, ids) in &by_owner {
            let refs_subtree = fresh();
            let refs = Merk::of(
                refs_subtree,
                ids.iter()
                    .map(|key| {
                        let path = ReferencePathType::UpstreamRootHeightReference(
                            4,
                            vec![b"0".to_vec(), key.to_vec()],
                        );
                        let value = serialized(Element::new_reference(path));
                        (key.to_vec(), value, Some(doc_hash[key]), Vec::new())
                    })
                    .collect(),
            );
            let owner_subtree = fresh();
            let refs_root = refs.root.map(|root| refs.nodes[root as usize].key.clone());
            let owner_merk = Merk::of(
                owner_subtree,
                vec![(
                    b"0".to_vec(),
                    tree(refs_root.as_deref()),
                    Some(refs.root_hash()),
                    vec![refs_subtree],
                )],
            );
            let owner_root = owner_merk.nodes[0].key.clone();
            owner_items.push((
                owner.clone(),
                tree(Some(&owner_root)),
                Some(owner_merk.root_hash()),
                vec![owner_subtree],
            ));
            families.push((owner_merk, refs));
        }
        let index = Merk::of(fresh(), owner_items);
        let root_key = |merk: &Merk| merk.root.map(|root| merk.nodes[root as usize].key.clone());
        let host_of = |key: &[u8], merk: &Merk| {
            (
                key.to_vec(),
                tree(root_key(merk).as_deref()),
                Some(merk.root_hash()),
                vec![merk.subtree],
            )
        };
        let type_merk = Merk::of(
            fresh(),
            vec![host_of(b"$ownerId", &index), host_of(b"0", &primary)],
        );
        let documents_merk = Merk::of(fresh(), vec![host_of(b"note", &type_merk)]);
        let contract = Merk::of(fresh(), vec![host_of(&[1], &documents_merk)]);
        let contracts = Merk::of(fresh(), vec![host_of(&id(4, 0), &contract)]);
        let root = Merk::of(
            Subtree::new([0; 32]),
            vec![host_of(b"documents", &contracts)],
        );

        // Parent first: the root, the chain, the type, the primary tree,
        // the index, and each owner's two Merks.
        built.extend([
            root,
            contracts,
            contract,
            documents_merk,
            type_merk,
            primary,
            index,
        ]);
        for (owner, refs) in families {
            built.push(owner);
            built.push(refs);
        }
        let place: BTreeMap<Subtree, usize> = (built.iter().enumerate())
            .map(|(at, merk)| (merk.subtree, at))
            .collect();
        let mut hosts = Vec::new();
        for (at, merk) in built.iter().enumerate() {
            for (node, entry) in merk.nodes.iter().enumerate() {
                for hosted in &entry.hosted {
                    hosts.push((place[hosted], at, node as u32));
                }
            }
        }
        for (hosted, merk, node) in hosts {
            built[hosted].host = Some((merk, node));
        }
        Grove {
            merks: built,
            primary: 5,
            versions: vec![0; documents as usize],
        }
    }

    fn root_hash(&self) -> Hash {
        self.merks[0].root_hash()
    }

    /// Writes the whole grove in one transaction: every subtree entered,
    /// then each Merk with the node that hosts it written again, naming
    /// its root.
    fn write(&self, db: &Db) -> CommitVersion {
        let txn = db.begin();
        let scope = txn.scope().expect("a scope");
        let enters: Vec<SubtreeWrite<'_>> = (self.merks.iter())
            .map(|merk| SubtreeWrite::Enter {
                subtree: merk.subtree,
                tree_type: TREE,
                host: merk.host.map(|(host, node)| HostPut {
                    subtree: self.merks[host].subtree,
                    key: &self.merks[host].nodes[node as usize].key,
                    position: 0,
                }),
            })
            .collect();
        scope
            .apply(&Batch {
                subtrees: &enters,
                ..Batch::default()
            })
            .expect("the enters");
        for merk in &self.merks {
            let puts: Vec<NodePut<'_>> = (0..merk.nodes.len() as u32)
                .map(|at| merk.put(at))
                .collect();
            let host: Vec<NodePut<'_>> = (merk.host.iter())
                .map(|&(host, node)| NodePut {
                    value: ValuePut::Keep,
                    ..self.merks[host].put(node)
                })
                .collect();
            let mut merks = vec![MerkWrite {
                subtree: merk.subtree,
                nodes: &puts,
                removed: &[],
                root: RootPut::Set(merk.root.map(|root| &merk.nodes[root as usize].key[..])),
                anchored: false,
            }];
            if let Some((host_merk, _)) = merk.host {
                // The node above the host is not written here: the host's
                // Merk keeps its root where that node points.
                merks.push(MerkWrite {
                    subtree: self.merks[host_merk].subtree,
                    nodes: &host,
                    removed: &[],
                    root: RootPut::Keep,
                    anchored: self.merks[host_merk].host.is_some(),
                });
            }
            scope
                .apply(&Batch {
                    merks: &merks,
                    ..Batch::default()
                })
                .expect("a Merk");
        }
        scope.merge();
        match db
            .commit(&txn, &mut NoStaleHashes)
            .expect("the grove is committed")
        {
            Committed::Published(published) => published.version,
            Committed::Empty => unreachable!("the grove is written"),
        }
    }

    /// One block: `TRANSITIONS` documents written again, each with the
    /// path above it to the root. Returns the version and the root hash.
    fn block(&mut self, db: &Db, block: u64) -> (CommitVersion, Hash) {
        let mut changed: BTreeMap<usize, BTreeSet<u32>> = BTreeMap::new();
        let documents = self.versions.len() as u64;
        for transition in 0..TRANSITIONS as u64 {
            let number = mix(block << 8 ^ transition) % documents;
            self.versions[number as usize] += 1;
            let value = document(number, self.versions[number as usize]);
            let key = id(2, number);
            let merk = &mut self.merks[self.primary];
            let at = merk
                .nodes
                .binary_search_by(|node| node.key.as_slice().cmp(&key));
            let at = at.expect("a document") as u32;
            merk.nodes[at as usize].value = value;
            self.climb(self.primary, at, &mut changed);
        }
        let puts: Vec<(Subtree, Vec<NodePut<'_>>)> = (changed.iter())
            .map(|(&merk, nodes)| {
                let merk = &self.merks[merk];
                (merk.subtree, nodes.iter().map(|&at| merk.put(at)).collect())
            })
            .collect();
        let merks: Vec<MerkWrite<'_>> = (puts.iter())
            .map(|(subtree, nodes)| MerkWrite {
                subtree: *subtree,
                nodes,
                removed: &[],
                root: RootPut::Keep,
                anchored: false,
            })
            .collect();
        let batch = Batch {
            merks: &merks,
            ..Batch::default()
        };
        let version = match db.write(&batch, &mut NoStaleHashes).expect("a block") {
            Committed::Published(published) => published.version,
            Committed::Empty => unreachable!("a block writes"),
        };
        (version, self.root_hash())
    }

    /// Makes the hashes of the node `at` of `merk` and of every node above
    /// it again, up to the root of the grove, and notes each.
    fn climb(
        &mut self,
        mut merk: usize,
        mut at: u32,
        changed: &mut BTreeMap<usize, BTreeSet<u32>>,
    ) {
        loop {
            let mut node = Some(at);
            while let Some(here) = node {
                self.merks[merk].rehash(here);
                changed.entry(merk).or_default().insert(here);
                node = self.merks[merk].nodes[here as usize].parent;
            }
            let Some((host, host_node)) = self.merks[merk].host else {
                return;
            };
            let root = self.merks[merk].root_hash();
            self.merks[host].nodes[host_node as usize].combined = Some(root);
            (merk, at) = (host, host_node);
        }
    }
}

/// The peak resident size of this process, as Linux tells it, for the
/// record of the run.
fn peak_memory() -> String {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let peak = status.lines().find(|line| line.starts_with("VmHWM:"));
    peak.map_or("not told".to_owned(), |line| {
        line["VmHWM:".len()..].trim().to_owned()
    })
}

/// The root hash the checkpoint at `path` holds: the node hash of the
/// root of its root Merk, from the hashes the cells store.
fn stored_root_hash(path: &Path) -> Hash {
    let db = Db::open(path, &Options::default()).expect("the checkpoint opens");
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let root = operation
        .open(Subtree::new([0; 32]))
        .expect("a read")
        .expect("the root");
    let mut walk = root.walk();
    walk.seek_to_first(&operation).expect("a move");
    let key = root.entry().root().expect("a root node").key.to_vec();
    walk.seek(&operation, &key).expect("a move");
    let mut children = lignindb::txn::ChildKeys::new();
    let hash = walk
        .node_with_children(&operation, &mut children, |view| {
            let view = view.expect("the root node");
            let link = |side| view.link(side).map_or(NULL_HASH, |link| *link.hash);
            use lignin_format::tree_page::Side;
            node_hash(view.kv_hash(), &link(Side::Left), &link(Side::Right)).unwrap()
        })
        .expect("a read");
    drop(operation);
    drop(snapshot);
    db.close().expect("a close");
    hash
}

/// Runs `lignin` with `args`, its report to `<reports>/<name>.txt`, and
/// returns the report's first lines and its last ones.
fn lignin(
    reports: &Path,
    name: &str,
    args: &[&dyn AsRef<std::ffi::OsStr>],
    outcome: Outcome,
) -> String {
    let args: Vec<OsString> = args.iter().map(|arg| arg.as_ref().to_owned()).collect();
    let mut out = Vec::new();
    let began = Instant::now();
    let result = lignin_tools::run(&args, &mut out);
    let took = began.elapsed();
    fs::write(reports.join(format!("{name}.txt")), &out).expect("the report");
    let report = String::from_utf8(out).expect("text");
    match result {
        Ok(got) => assert_eq!(got, outcome, "{name}:\n{report}"),
        Err(error) => panic!("{name}: {error}\n{report}"),
    }
    let lines: Vec<&str> = report.lines().collect();
    let shown = match lines.len() {
        0..=12 => lines.join("\n"),
        len => format!(
            "{}\n  [{} lines]\n{}",
            lines[..6].join("\n"),
            len - 12,
            lines[len - 6..].join("\n")
        ),
    };
    eprintln!(
        "== lignin {name} ({:.2} s here)\n{shown}",
        took.as_secs_f64()
    );
    report
}

#[test]
#[ignore = "builds the reference database: a release build and some minutes"]
fn every_subcommand_works_on_a_live_checkpoint_of_the_reference_database() {
    let documents = setting("LIGNIN_OPS01_DOCUMENTS", 1_000_000) as u64;
    let owners = setting("LIGNIN_OPS01_OWNERS", 10_000) as u64;
    let warm = setting("LIGNIN_OPS01_WARM", 100) as u64;
    let temp;
    let dir: PathBuf = match std::env::var_os("LIGNIN_OPS01_DIR") {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            fs::create_dir_all(&dir).expect("the directory");
            let mut entries = fs::read_dir(&dir).expect("the directory is read");
            assert!(
                entries.next().is_none(),
                "LIGNIN_OPS01_DIR names a directory that is not empty: {}",
                dir.display()
            );
            dir
        }
        None => {
            temp = tempfile::tempdir().expect("a directory");
            temp.path().to_path_buf()
        }
    };
    let (live, checkpoint, reports) = (
        dir.join("live"),
        dir.join("checkpoint"),
        dir.join("reports"),
    );
    fs::create_dir(&reports).expect("the reports");

    let began = Instant::now();
    let mut grove = Grove::reference(documents, owners);
    let merks = grove.merks.len();
    let nodes: usize = grove.merks.iter().map(|merk| merk.nodes.len()).sum();
    eprintln!(
        "the model: {merks} Merks, {nodes} nodes, {:.1} s",
        began.elapsed().as_secs_f64()
    );
    let options = Options {
        cache_size: 256 << 20,
        ..Options::default()
    };
    let db = Db::open(&live, &options).expect("a new database");
    let built = grove.write(&db);
    eprintln!(
        "the grove is committed as version {built}, {:.1} s",
        began.elapsed().as_secs_f64()
    );
    let mut hashes: Vec<(CommitVersion, Hash)> = vec![(built, grove.root_hash())];
    for block in 0..warm {
        hashes.push(grove.block(&db, block));
    }
    eprintln!(
        "{warm} blocks committed, {:.1} s, peak memory {}",
        began.elapsed().as_secs_f64(),
        peak_memory()
    );

    let hashes = Mutex::new(hashes);
    let stop = AtomicBool::new(false);
    let (created, blocks_beside) = thread::scope(|scope| {
        // The writer stops when this ends, a panic of it included, or the
        // scope would wait for the writer for ever.
        let _stop = Stop(&stop);
        let (live, model, written, stopped) = (&db, &mut grove, &hashes, &stop);
        let writer = scope.spawn(move || {
            let mut block = warm;
            while !stopped.load(Ordering::Acquire) {
                let committed = model.block(live, block);
                written.lock().unwrap().push(committed);
                block += 1;
            }
            block - warm
        });
        // The writer is at work: the checkpoint is made beside it.
        while hashes.lock().unwrap().len() < warm as usize + 5 {
            assert!(!writer.is_finished(), "the writer stopped");
            thread::yield_now();
        }
        let created = db
            .checkpoint(&checkpoint, CheckpointOptions::default())
            .expect("the checkpoint");
        eprintln!(
            "the checkpoint: version {}, {} files linked, {} copied",
            created.version, created.linked, created.copied
        );

        let shown = checkpoint.as_path();
        let report = lignin(&reports, "manifest", &[&"manifest", &shown], Outcome::Done);
        assert!(report.contains("table checkpoint: table/"), "{report}");
        let report = lignin(
            &reports,
            "segments",
            &[&"segments", &shown, &"--blocks"],
            Outcome::Done,
        );
        assert!(!report.contains("DAMAGED"), "{report}");
        let cache = ["--cache", "256"];
        let report = lignin(
            &reports,
            "pages",
            &[&"pages", &shown, &cache[0], &cache[1]],
            Outcome::Done,
        );
        assert!(
            report.contains(&format!("version {}, ", created.version)),
            "{report}"
        );
        lignin(
            &reports,
            "pages-cells",
            &[&"pages", &shown, &"1", &"2", &"7", &"--cells"],
            Outcome::Done,
        );
        let report = lignin(
            &reports,
            "directory",
            &[&"directory", &shown],
            Outcome::Done,
        );
        assert!(
            report.contains(&format!(
                "{merks} entries: {merks} in use, 0 left over; 1 with no host"
            )),
            "{report}"
        );
        let report = lignin(&reports, "stats", &[&"stats", &shown], Outcome::Done);
        assert!(
            report.contains(&format!("version {}\n", created.version)),
            "{report}"
        );
        for (name, flags) in [
            ("verify", None),
            ("verify-beside", Some("--beside-background")),
        ] {
            let mut args: Vec<&dyn AsRef<std::ffi::OsStr>> =
                vec![&"verify", &shown, &cache[0], &cache[1]];
            if let Some(flag) = &flags {
                args.push(flag);
            }
            let report = lignin(&reports, name, &args, Outcome::Done);
            assert!(
                report.contains("every hash made again by Merk's rules"),
                "{report}"
            );
            assert!(report.contains("sound: every rule holds"), "{report}");
            assert!(
                report.contains(&format!("of {merks} Merks in use"))
                    || report.contains(&format!("{merks} Merks")),
                "{report}"
            );
        }

        // The whole database, and one owner's tree, dumped and loaded.
        let (all, again) = (dir.join("all.dump"), dir.join("again.dump"));
        let report = lignin(
            &reports,
            "dump-all",
            &[&"dump", &shown, &"--all", &all],
            Outcome::Done,
        );
        assert!(
            report.contains(&format!(
                "dumped version {}: {merks} subtrees, {nodes} nodes",
                created.version
            )),
            "{report}"
        );
        let loaded = dir.join("loaded");
        Db::open(&loaded, &Options::default())
            .expect("a new database")
            .close()
            .expect("a close");
        let report = lignin(
            &reports,
            "load-all",
            &[&"load", &loaded, &all],
            Outcome::Done,
        );
        assert!(
            report.contains(&format!("loaded {merks} subtrees, {nodes} nodes")),
            "{report}"
        );
        let report = lignin(
            &reports,
            "verify-loaded",
            &[&"verify", &loaded, &cache[0], &cache[1]],
            Outcome::Done,
        );
        assert!(report.contains("sound: every rule holds"), "{report}");
        lignin(
            &reports,
            "dump-loaded",
            &[&"dump", &loaded, &"--all", &again],
            Outcome::Done,
        );
        assert!(
            fs::read(&all).expect("a dump") == fs::read(&again).expect("a dump"),
            "the load holds what the checkpoint held"
        );
        let owner = grove_owner_subtree(&reports);
        let one = dir.join("owner.dump");
        let report = lignin(
            &reports,
            "dump-owner",
            &[&"dump", &shown, &owner, &one],
            Outcome::Done,
        );
        assert!(report.contains(": 2 subtrees, "), "{report}");
        let alone = dir.join("alone");
        Db::open(&alone, &Options::default())
            .expect("a new database")
            .close()
            .expect("a close");
        lignin(
            &reports,
            "load-owner",
            &[&"load", &alone, &one],
            Outcome::Done,
        );
        let report = lignin(
            &reports,
            "verify-owner",
            &[&"verify", &alone],
            Outcome::Done,
        );
        assert!(report.contains("sound: every rule holds"), "{report}");

        let report = lignin(
            &reports,
            "compact",
            &[&"compact", &shown, &cache[0], &cache[1]],
            Outcome::Done,
        );
        assert!(!report.contains("the timeout ended the work"), "{report}");
        let report = lignin(
            &reports,
            "verify-compacted",
            &[&"verify", &shown, &cache[0], &cache[1]],
            Outcome::Done,
        );
        assert!(report.contains("sound: every rule holds"), "{report}");
        lignin(
            &reports,
            "stats-compacted",
            &[&"stats", &shown],
            Outcome::Done,
        );

        stop.store(true, Ordering::Release);
        (created, writer.join().expect("the writer"))
    });
    let hashes = hashes.into_inner().unwrap();
    let at = hashes
        .iter()
        .find(|(version, _)| *version == created.version);
    let (_, expected) = at.expect("the checkpoint's version is one the writer committed");
    assert_eq!(
        &stored_root_hash(&checkpoint),
        expected,
        "the checkpoint's root hash"
    );
    let latest = hashes.last().expect("a commit").0;
    assert!(
        latest > created.version,
        "the live database committed after the checkpoint"
    );
    eprintln!(
        "the live database committed {blocks_beside} blocks beside the checkpoint and the \
         subcommands, to version {latest}; the checkpoint is of version {}, with the root hash \
         the writer made for it",
        created.version
    );
    eprintln!("peak memory {}", peak_memory());
    let report = db.close_and_verify(None).expect("the live database closes");
    assert!(report.is_sound(), "{report}");
}

/// Stops the writer when it is dropped.
struct Stop<'a>(&'a AtomicBool);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The subtree of one owner's tree, as `lignin directory` listed it: an
/// entry whose host is in the index, at depth 6.
fn grove_owner_subtree(reports: &Path) -> String {
    let listing = fs::read_to_string(reports.join("directory.txt")).expect("the listing");
    let line = listing
        .lines()
        .find(|line| line.ends_with(" depth 6"))
        .expect("an owner's tree");
    line.split(' ').next().expect("its subtree").to_owned()
}
