//! The hashes of the committed pages against Merk's hash rules
//! (`grovedb_merk::store::lignin::hash`, MERK-02).
//!
//! The replay writes every node with the hashes Merk made on RocksDB, and
//! compares each committed node with RocksDB's record. Here the rules make
//! the same hashes again, from the committed pages alone:
//!
//! 1. Every cell of every Merk is checked with [`hash::check`]: each hash
//!    the cell stores is what the rules make of what it is made from, as
//!    that is stored. A value hash that only its writer can make, a
//!    reference's or that of a tree whose data is no Merk, is taken as it
//!    is stored.
//! 2. Then the engine makes them: in a copy of the pages, every hash the
//!    rules can make is overwritten and marked stale, and one evaluation
//!    makes them all ([`Hashes::evaluate_all`]). Every cell then holds,
//!    byte for byte, the hashes it held, which are RocksDB's.
//! 3. The hash the rules make of the root of the grove's first Merk, which
//!    no cell stores, is the root hash GroveDB gives on RocksDB. The root
//!    of every other Merk is hosted by a cell whose value hash was made
//!    from it in step 2.

use std::collections::{BTreeMap, HashMap, HashSet};

use grovedb_merk::store::lignin::hash::{HashRefusal, MerkHashRules};
use grovedb_version::version::GroveVersion;
use lignin_format::{
    tree_page::{Hash, HashField, Side},
    value_log::ValuePlace,
    CellAddress, CommitVersion, PageId,
};
use lignin_store::load::{LoadBuffers, PageLoader};
use lignin_tree::{
    fragment::{FragmentError, PageMut, Pages},
    hash::{self, Hashes, Stale, ValueReader},
    node::{ChangeSink, Lease, NodeError, NodePage, NodePageMut},
    value::{InlinePolicy, TreeValues, ValueSource},
};
use lignindb::{
    snapshot::Versions,
    txn::{
        testing::{Db, SIZE},
        Subtree,
    },
};

/// What one check made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Made {
    /// The cells whose hashes were checked and made again.
    pub cells: u64,
    /// The hashes the evaluation made and wrote.
    pub hashes: u64,
    /// The value hashes that only their writer can make, which were left
    /// as they are.
    pub stated: u64,
}

/// The committed tree pages at one version, with the pages that were
/// changed since kept in memory.
struct Frames<'a> {
    loader: PageLoader<'a>,
    at: CommitVersion,
    buffers: LoadBuffers,
    lease: Lease,
    changed: HashMap<PageId, Vec<u8>>,
}

fn unread(page: PageId, error: impl std::fmt::Display) -> FragmentError {
    FragmentError::Pages(format!("{page}: {error}").into())
}

impl Pages for Frames<'_> {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        if let Some(frame) = self.changed.get(&page) {
            return Ok(read(NodePage::open(frame, self.lease)?));
        }
        let frame = (self.loader.get(page, self.at, &mut self.buffers))
            .map_err(|error| unread(page, error))?;
        Ok(read(NodePage::open(&frame, self.lease)?))
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        if !self.changed.contains_key(&page) {
            let frame = (self.loader.get(page, self.at, &mut self.buffers))
                .map_err(|error| unread(page, error))?;
            self.changed.insert(page, frame.to_vec());
        }
        let frame = self.changed.get_mut(&page).expect("the copy was made");
        let sink: &mut dyn ChangeSink = &mut ();
        let mut page = NodePageMut::open(frame, SIZE, self.lease, sink)?;
        Ok(change(&mut page)?)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        Err(FragmentError::Pages("an evaluation makes no page".into()))
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        Err(unread(page, "an evaluation frees no page"))
    }
}

/// One cell as it is committed: the hashes it stores, and what its value
/// hash is made from.
struct Cell {
    at: CellAddress,
    hashes: [Option<Hash>; 4],
    /// The value, where it is in the cell, and its place where it is not.
    value: Result<Vec<u8>, ValuePlace>,
    hosted: usize,
}

fn stored(pages: &mut Frames<'_>, at: CellAddress) -> [Option<Hash>; 4] {
    let read = pages.read(at.page(), |page| {
        let node = page.node(at.slot()).expect("a cell");
        HashField::ALL.map(|field| node.hash(field).copied())
    });
    read.expect("the page is read")
}

/// Checks the hashes of every Merk that `roots` names against Merk's hash
/// rules: as they are stored, and made again by an evaluation. `top` is the
/// first Merk of the grove, and `root_hash` the grove's root hash on
/// RocksDB.
pub fn check(
    db: &Db,
    versions: &Versions,
    at: CommitVersion,
    roots: &BTreeMap<Subtree, CellAddress>,
    top: Subtree,
    root_hash: Hash,
    grove_version: &GroveVersion,
) -> Made {
    let _guard = versions.guard();
    let mut pages = Frames {
        loader: db.loader(),
        at,
        buffers: LoadBuffers::new(),
        lease: Lease::take(),
        changed: HashMap::new(),
    };
    let source = ValueSource::new(&db.values, db.loader(), at);
    let no_values = TreeValues::new(InlinePolicy::DEFAULT);
    let mut values = no_values.reader(&source);

    // Every cell that a root of a Merk reaches.
    let mut cells = Vec::new();
    let mut hosted_roots = HashSet::new();
    let mut ahead: Vec<CellAddress> = roots.values().copied().collect();
    while let Some(at) = ahead.pop() {
        let read = pages.read(at.page(), |page| {
            let node = page.node(at.slot()).expect("a cell");
            let below = Side::BOTH.map(|side| node.link(side).map(|link| link.child));
            let hosted: Vec<Option<CellAddress>> = node.hosted_roots().collect();
            let value = match node.value().inline() {
                Some(value) => Ok(value.to_vec()),
                None => Err(node
                    .value()
                    .place()
                    .expect("a value out of line has a place")),
            };
            let hashes = HashField::ALL.map(|field| node.hash(field).copied());
            let cell = Cell {
                at,
                hashes,
                value,
                hosted: hosted.len(),
            };
            (cell, below, hosted)
        });
        let (cell, below, hosted) = read.expect("the page is read");
        ahead.extend(below.into_iter().flatten());
        hosted_roots.extend(hosted.into_iter().flatten());
        cells.push(cell);
    }

    // 1. As they are stored.
    let mut check_rules = MerkHashRules::for_check(grove_version);
    for cell in &cells {
        let differing = hash::check(&mut pages, &mut values, &mut check_rules, cell.at);
        let differing = differing.unwrap_or_else(|error| panic!("{}: {error}", cell.at));
        assert_eq!(differing, Stale::NONE, "the hashes of {}", cell.at);
    }

    // 2. Made by the engine. Every hash the rules can make is marked, and
    //    its bytes are turned over so that none is right by having been
    //    left.
    let mut made = Made::default();
    let mut hashes = Hashes::new();
    let mut classify = MerkHashRules::new(grove_version);
    let mut buffer = Vec::new();
    for cell in &cells {
        let value = match &cell.value {
            Ok(value) => value,
            Err(place) => {
                buffer.clear();
                (values.read(*place, cell.at.page(), &mut buffer)).expect("the value is read");
                &buffer
            }
        };
        let mut marked: Stale = (HashField::ALL.into_iter().zip(&cell.hashes))
            .filter(|(_, hash)| hash.is_some())
            .map(|(field, _)| field)
            .collect();
        match classify.element_value_hash(value, &vec![None; cell.hosted]) {
            Ok(_) => {}
            Err(HashRefusal::StatedByWriter(_)) => {
                marked = marked.without(HashField::Value);
                made.stated += 1;
            }
            Err(refused) => panic!("the value of {}: {refused}", cell.at),
        }
        let turned = pages.write(cell.at.page(), |page| {
            let mut node = page.node_mut(cell.at.slot()).expect("a cell");
            for field in marked.fields() {
                let stored = node.node().hash(field).expect("a stored hash");
                node.set_hash(field, &stored.map(|byte| !byte))?;
            }
            Ok(())
        });
        turned.expect("the hashes are overwritten");
        for field in marked.fields() {
            hashes.mark(cell.at, field);
        }
        made.hashes += marked.len() as u64;
    }
    made.cells = cells.len() as u64;

    let mut rules = MerkHashRules::new(grove_version);
    hashes
        .evaluate_all(&mut pages, &mut values, &mut rules)
        .expect("every marked hash is made");
    assert!(hashes.is_empty());
    let counters = hashes.counters();
    assert_eq!(counters.written, made.hashes, "every made hash was written");
    assert_eq!((counters.unchanged, counters.nodes), (0, made.cells));
    assert_eq!(counters.kv_hashes, made.cells);
    assert_eq!(counters.value_hashes, made.cells - made.stated);
    for cell in &cells {
        assert_eq!(
            stored(&mut pages, cell.at),
            cell.hashes,
            "the hashes of {} as the rules made them",
            cell.at
        );
    }

    // 3. The root hashes.
    let top_hash = match roots.get(&top) {
        None => [0; 32],
        Some(root) => hashes
            .hash_of(&mut pages, &mut values, &mut rules, *root)
            .expect("the root hash is made"),
    };
    assert_eq!(top_hash, root_hash, "the root hash of the grove");
    for (subtree, root) in roots {
        assert!(
            *subtree == top || hosted_roots.contains(root),
            "the root of {subtree} is hosted by no cell"
        );
    }
    made
}
