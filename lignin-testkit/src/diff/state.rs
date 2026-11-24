//! What a database stores, read through the seam.
//!
//! A root hash says less than it used to. Since GroveDB #1059 the referrer
//! list of an element that takes backward references is in no hash, so two
//! engines can agree on every root hash, proof and cost and hold different
//! lists. No public read of GroveDB returns the list either: `get_raw` and
//! the queries strip it. And the records of the auxiliary keyspace were
//! never in a hash.
//!
//! So the fuzzer also compares what is stored. [`stored`] walks a database
//! from the root tree down and lists every record it reaches, through the
//! handles any engine gives: the nodes of a Merk in Merk's own encoding
//! (the cursor of `NodeStore`), which holds the element as it is stored,
//! list included, with the hashes and links of its node; the records of an
//! append-only tree (the cursor of `FlatStore`); and the aux and meta
//! records of each subtree. The secondary trees of an indexed tree, which
//! are not under a path, are reached by their prefixes. The drops that are
//! pending are listed twice: as the engine reports them, which are the
//! committed ones, and as the records the transaction sees, which include
//! the ones it staged itself.
//!
//! The listing is engine-neutral by the contract of the two seams: a node
//! is shown in Merk's encoding whatever an engine keeps of it, and a flat
//! record is the bytes that were put.
//!
//! # What a subtree leaves behind
//!
//! A subtree that is deleted, moved away, or rolled back with the
//! transaction that created it is no longer under a path the walk follows.
//! What an engine left of it would show only if a tree were created at
//! that path again. So a walk is told the subtrees the walk before it
//! listed ([`Listing::places`]), and lists the keyspaces of each one it no
//! longer reaches, as records that were left behind. Two engines have to
//! leave the same, which is nothing wherever GroveDB cleared the subtree.
//!
//! # One column shown twice
//!
//! The data of a subtree is the nodes of a Merk or the records of an
//! append-only tree, and the walk reads each through its own cursor. An
//! engine may keep the two in one column of the subtree, as RocksDB does,
//! or apart, as LigninDB does. Where a tree of the one kind was moved away
//! and a tree of the other kind was moved to its path, the place of the
//! old kind is gone and the walk looks at what was left under it: on an
//! engine with one column that cursor shows the records of the tree that
//! lives there now, which the walk has listed already under its own kind.
//! So a record that would be listed as left behind is left out where the
//! same walk listed the same record, key and bytes, at the same path
//! under the other kind of data. Any other record there is still listed:
//! it was left behind.
//!
//! # What the walk does not reach
//!
//! A subtree that was dropped flat is left out of that for as long as the
//! drop is pending: its records stay until the drop is flushed on an
//! engine that reclaims then, and are gone at once on one that hides them
//! at the drop, and both are right. The walk knows such a subtree by the
//! records of the drops its transaction sees, which name the prefixes each
//! drop frees, so a drop that failed or was rolled back excuses nothing.
//! It hands the subtree on ([`Listing::dropped`]) for the next walk to be
//! told of, and the walk after the flush looks at it like at any other
//! subtree that is gone. Storage under a prefix that never was a place of
//! a walk is not looked at either: the walk reads what GroveDB can
//! address, not what a file holds.
//!
//! A cost that a handle or a cursor reports is dropped (`unwrap` on a
//! `CostContext` returns the value and cannot fail): the walk is no
//! operation of the stream.

use std::collections::BTreeSet;

use grovedb::{
    element::indexed::IndexAxis,
    engine::{Engine, SubtreeHandle},
    Element,
};
use grovedb_merk::{element::decode::ElementDecodeExtensions, store::NodeStore};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    flat::{pending_prefix_drops_namespace, PendingPrefixDropRecord, SubtreePrefix},
    prefix::{build_prefix, secondary_prefix_for},
    FlatStore, RawIterator,
};
use grovedb_version::version::GroveVersion;

use crate::workload::{Db, StreamEngine, Tx};

/// The axes an indexed tree can be ranked on. A secondary tree is kept
/// under the tag of its axis, so these are the keyspaces the walk looks
/// into for every indexed tree. One that no tree uses is empty: the big
/// sum axis keeps its rows under the tag of the sum axis.
const AXES: [IndexAxis; 4] = [
    IndexAxis::Count,
    IndexAxis::Sum,
    IndexAxis::Avg,
    IndexAxis::BigSum,
];

/// Where `axis` is in [`AXES`]. An axis added to GroveDB does not compile
/// here until it is listed.
const fn listed(axis: IndexAxis) -> usize {
    match axis {
        IndexAxis::Count => 0,
        IndexAxis::Sum => 1,
        IndexAxis::Avg => 2,
        IndexAxis::BigSum => 3,
    }
}

const _: () = {
    let mut index = 0;
    while index < AXES.len() {
        assert!(listed(AXES[index]) == index);
        index += 1;
    }
};

/// The keyspace of a subtree a record is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Keyspace {
    /// The key of the root node, which the root tree records itself.
    RootKey,
    /// The nodes of a Merk, in Merk's encoding.
    Nodes,
    /// The records of an append-only tree.
    Flat,
    /// The auxiliary records.
    Aux,
    /// The meta records.
    Meta,
    /// The drops of subtrees that wait to be flushed, as the engine
    /// reports them: the prefixes each one frees, under the prefix of the
    /// subtree.
    PendingDrop,
    /// The records of those drops as the transaction sees them, the ones
    /// it staged and has not committed among them: the record as it was
    /// put, under the prefix of the subtree.
    DropRecord,
}

impl Keyspace {
    fn name(self) -> &'static str {
        match self {
            Keyspace::RootKey => "root-key",
            Keyspace::Nodes => "node",
            Keyspace::Flat => "flat",
            Keyspace::Aux => "aux",
            Keyspace::Meta => "meta",
            Keyspace::PendingDrop => "pending-drop",
            Keyspace::DropRecord => "drop-record",
        }
    }
}

/// A subtree a walk listed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Place {
    /// The path of the subtree. For a secondary tree of an indexed tree it
    /// is the path of the indexed tree.
    pub path: Vec<Vec<u8>>,
    /// The tag of the secondary tree, for one.
    pub secondary: Option<u8>,
    /// What its data keyspace holds: [`Keyspace::Nodes`] or
    /// [`Keyspace::Flat`].
    pub data: Keyspace,
}

/// One stored record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The path of the subtree. For a secondary tree of an indexed tree it
    /// is the path of the indexed tree.
    pub path: Vec<Vec<u8>>,
    /// The tag of the secondary tree, for a record of one.
    pub secondary: Option<u8>,
    /// The keyspace.
    pub keyspace: Keyspace,
    /// The key.
    pub key: Vec<u8>,
    /// The bytes stored.
    pub value: Vec<u8>,
    /// Whether the subtree is one the walk no longer reaches: the record
    /// was left behind.
    pub left_behind: bool,
}

impl Record {
    /// The record on one line, its value cut to a readable length.
    pub fn text(&self) -> String {
        let path: Vec<String> = self.path.iter().map(hex::encode).collect();
        let secondary = self
            .secondary
            .map_or(String::new(), |tag| format!(" secondary {tag}"));
        let mut value = hex::encode(&self.value);
        if value.len() > 512 {
            value.truncate(512);
            value.push('…');
        }
        format!(
            "{}{} /{}{secondary} key {} = {} bytes {value}",
            if self.left_behind {
                "left behind: "
            } else {
                ""
            },
            self.keyspace.name(),
            path.join("/"),
            hex::encode(&self.key),
            self.value.len(),
        )
    }
}

/// What a walk listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// The records, in the order of the walk: a subtree before the
    /// subtrees under it, then the pending drops, then what was left
    /// behind.
    pub records: Vec<Record>,
    /// The subtrees the walk reached from the root tree.
    pub places: BTreeSet<Place>,
    /// The subtrees of `before` the walk did not reach and did not look
    /// at, because a drop its transaction sees is to free their storage.
    pub dropped: BTreeSet<Place>,
}

impl Listing {
    /// The subtrees to tell the next walk of: the ones this walk reached,
    /// and the ones a pending drop kept it from looking at.
    pub fn handed_on(self) -> BTreeSet<Place> {
        let mut places = self.places;
        places.extend(self.dropped);
        places
    }
}

/// Every record `db` stores, as `transaction` sees it.
///
/// `before` are the subtrees an earlier walk handed on
/// ([`Listing::handed_on`]). The keyspaces of each one this walk does not
/// reach are listed as well, as records left behind, unless a drop the
/// transaction sees is to free them.
pub fn stored<'db, E: StreamEngine>(
    db: &'db Db<E>,
    transaction: &Tx<'db, E>,
    grove_version: &GroveVersion,
    before: &BTreeSet<Place>,
) -> Result<Listing, String> {
    let engine = db.engine();
    let mut records = Vec::new();
    let mut places = BTreeSet::new();
    let mut merks = vec![Vec::new()];
    while let Some(path) = merks.pop() {
        let place = Place {
            path,
            secondary: None,
            data: Keyspace::Nodes,
        };
        let handle = engine
            .subtree(SubtreePath::from(place.path.as_slice()), None, transaction)
            .unwrap();
        if place.path.is_empty() {
            let root_key = NodeStore::root_key(&handle)
                .unwrap()
                .map_err(|error| format!("reading the root key: {error}"))?;
            if let Some(root_key) = root_key {
                records.push(Record {
                    path: Vec::new(),
                    secondary: None,
                    keyspace: Keyspace::RootKey,
                    key: Vec::new(),
                    value: root_key,
                    left_behind: false,
                });
            }
        }
        let from = records.len();
        subtree(&handle, &place, false, &mut records)?;
        let nodes: Vec<(Vec<u8>, Vec<u8>)> = records[from..]
            .iter()
            .filter(|record| record.keyspace == Keyspace::Nodes)
            .map(|record| (record.key.clone(), record.value.clone()))
            .collect();
        drop(handle);
        // In reverse, so that the subtrees are walked in the order of their
        // keys.
        for (key, node) in nodes.into_iter().rev() {
            // A node that does not decode is listed and holds no subtree
            // the walk could name.
            let Ok(element) = Element::raw_decode(&node, grove_version) else {
                continue;
            };
            if !element.is_any_tree() {
                continue;
            }
            let mut below = place.path.clone();
            below.push(key);
            if element.uses_non_merk_data_storage() {
                let flat = Place {
                    path: below,
                    secondary: None,
                    data: Keyspace::Flat,
                };
                list(engine, transaction, &flat, false, &mut records)?;
                places.insert(flat);
                continue;
            }
            if element.is_indexed_tree() {
                for axis in AXES {
                    let secondary = Place {
                        path: below.clone(),
                        secondary: Some(axis.tag()),
                        data: Keyspace::Nodes,
                    };
                    list(engine, transaction, &secondary, false, &mut records)?;
                    places.insert(secondary);
                }
            }
            merks.push(below);
        }
        places.insert(place);
    }

    let pending = engine
        .pending_prefix_drop_records()
        .map_err(|error| format!("reading the pending drops: {error}"))?;
    for drop in pending {
        records.push(Record {
            path: drop.path,
            secondary: None,
            keyspace: Keyspace::PendingDrop,
            key: drop.primary_prefix.to_vec(),
            value: drop.doomed_prefixes.concat(),
            left_behind: false,
        });
    }

    let mut doomed: BTreeSet<SubtreePrefix> = BTreeSet::new();
    {
        let namespace = engine
            .subtree_by_prefix(*pending_prefix_drops_namespace(), None, transaction)
            .unwrap();
        walk(FlatStore::cursor(&namespace.meta()), |key, value| {
            // A record that does not decode is listed and frees nothing
            // the walk could name.
            let drop = PendingPrefixDropRecord::decode(key, value).ok();
            records.push(Record {
                path: drop
                    .as_ref()
                    .map(|drop| drop.path.clone())
                    .unwrap_or_default(),
                secondary: None,
                keyspace: Keyspace::DropRecord,
                key: key.to_vec(),
                value: value.to_vec(),
                left_behind: false,
            });
            doomed.extend(drop.into_iter().flat_map(|drop| drop.doomed_prefixes));
        })?;
    }

    let mut dropped = BTreeSet::new();
    for gone in before.difference(&places) {
        if doomed.contains(&prefix(gone)) {
            dropped.insert(gone.clone());
        } else {
            list(engine, transaction, gone, true, &mut records)?;
        }
    }
    Ok(Listing {
        records: without_a_column_shown_twice(records),
        places,
        dropped,
    })
}

/// `records` without the records left behind that the walk listed as
/// well, with the same key and bytes at the same path, as data of the
/// other kind of a subtree it reached: the one column of an engine that
/// keeps nodes and flat records together, shown twice (the module
/// documentation, "One column shown twice").
fn without_a_column_shown_twice(records: Vec<Record>) -> Vec<Record> {
    let other_kind = |keyspace| match keyspace {
        Keyspace::Flat => Some(Keyspace::Nodes),
        Keyspace::Nodes => Some(Keyspace::Flat),
        _ => None,
    };
    if !(records.iter()).any(|record| record.left_behind && other_kind(record.keyspace).is_some()) {
        return records;
    }
    /// Where a record is, and what it is: its path, its secondary, its
    /// keyspace, its key and its bytes.
    type Shown<'a> = (&'a [Vec<u8>], Option<u8>, Keyspace, &'a [u8], &'a [u8]);
    let reached: BTreeSet<Shown<'_>> = records
        .iter()
        .filter(|record| !record.left_behind)
        .map(|record| {
            let Record {
                path,
                secondary,
                keyspace,
                key,
                value,
                ..
            } = record;
            (
                path.as_slice(),
                *secondary,
                *keyspace,
                key.as_slice(),
                value.as_slice(),
            )
        })
        .collect();
    let shown_twice: Vec<bool> = records
        .iter()
        .map(|record| {
            let Some(other) = other_kind(record.keyspace) else {
                return false;
            };
            record.left_behind
                && reached.contains(&(
                    record.path.as_slice(),
                    record.secondary,
                    other,
                    record.key.as_slice(),
                    record.value.as_slice(),
                ))
        })
        .collect();
    let mut shown_twice = shown_twice.into_iter();
    let mut records = records;
    records.retain(|_| !shown_twice.next().unwrap_or(false));
    records
}

/// Whether a secondary tree of the indexed tree at `path` holds a row as
/// `transaction` sees `db`: a node under the tag of one of the axes an
/// indexed tree can be ranked on ([`AXES`]).
pub fn a_secondary_holds_a_row<'db, E: StreamEngine>(
    db: &'db Db<E>,
    transaction: &Tx<'db, E>,
    path: &[Vec<u8>],
) -> Result<bool, String> {
    let mut records = Vec::new();
    for axis in AXES {
        let secondary = Place {
            path: path.to_vec(),
            secondary: Some(axis.tag()),
            data: Keyspace::Nodes,
        };
        list(db.engine(), transaction, &secondary, false, &mut records)?;
    }
    let mut rows = records
        .iter()
        .filter(|record| record.keyspace == Keyspace::Nodes);
    Ok(rows.next().is_some())
}

/// Whether a record is stored under the path `path` as `transaction`
/// sees `db`, whether or not an element leads to it: a node, an aux
/// record or a meta record under the prefix of the subtree at that path.
pub fn a_record_is_under<'db, E: StreamEngine>(
    db: &'db Db<E>,
    transaction: &Tx<'db, E>,
    path: &[Vec<u8>],
) -> Result<bool, String> {
    let place = Place {
        path: path.to_vec(),
        secondary: None,
        data: Keyspace::Nodes,
    };
    let mut records = Vec::new();
    list(db.engine(), transaction, &place, true, &mut records)?;
    Ok(!records.is_empty())
}

/// Whether a node record is stored under `key` in the subtree at `path`
/// as `transaction` sees `db`, whether or not an element leads to it.
pub fn a_node_is_under<'db, E: StreamEngine>(
    db: &'db Db<E>,
    transaction: &Tx<'db, E>,
    path: &[Vec<u8>],
    key: &[u8],
) -> Result<bool, String> {
    let place = Place {
        path: path.to_vec(),
        secondary: None,
        data: Keyspace::Nodes,
    };
    let mut records = Vec::new();
    list(db.engine(), transaction, &place, true, &mut records)?;
    let mut nodes = (records.iter()).filter(|record| record.keyspace == Keyspace::Nodes);
    Ok(nodes.any(|record| record.key == key))
}

/// A node record by the path of its subtree and its key.
pub type NodeKey = (Vec<Vec<u8>>, Vec<u8>);

/// The node records at and below `path` as `transaction` sees `db`,
/// whether or not an element leads to them, each by the path of its
/// subtree and its key: those under the prefix of the subtree at `path`,
/// and those under the path of every key among them whose record is the
/// element of a tree that keeps a Merk, and so on down. That is the
/// walk GroveDB makes for the elements of a tree that take part in
/// backward references. None where there are more than `most`.
pub fn node_keys_below<'db, E: StreamEngine>(
    db: &'db Db<E>,
    transaction: &Tx<'db, E>,
    path: &[Vec<u8>],
    most: usize,
    grove_version: &GroveVersion,
) -> Result<Option<BTreeSet<NodeKey>>, String> {
    let mut found = BTreeSet::new();
    let mut queue = vec![path.to_vec()];
    while let Some(path) = queue.pop() {
        let place = Place {
            path: path.clone(),
            secondary: None,
            data: Keyspace::Nodes,
        };
        let mut records = Vec::new();
        list(db.engine(), transaction, &place, true, &mut records)?;
        for record in records {
            if record.keyspace != Keyspace::Nodes {
                continue;
            }
            let element = Element::raw_decode(&record.value, grove_version);
            if element.is_ok_and(|tree| tree.is_any_tree() && !tree.uses_non_merk_data_storage()) {
                let mut below = path.clone();
                below.push(record.key.clone());
                queue.push(below);
            }
            found.insert((path.clone(), record.key));
            if found.len() > most {
                return Ok(None);
            }
        }
    }
    Ok(Some(found))
}

/// The paths of the trees whose flat drops have a pending record as
/// `transaction` sees `db`, the records it staged itself among them. A
/// record that does not decode names no path.
pub fn pending_drop_paths<'db, E: StreamEngine>(
    db: &'db Db<E>,
    transaction: &Tx<'db, E>,
) -> Result<Vec<Vec<Vec<u8>>>, String> {
    let namespace = db
        .engine()
        .subtree_by_prefix(*pending_prefix_drops_namespace(), None, transaction)
        .unwrap();
    let mut paths = Vec::new();
    walk(FlatStore::cursor(&namespace.meta()), |key, value| {
        let drop = PendingPrefixDropRecord::decode(key, value).ok();
        paths.extend(drop.map(|drop| drop.path));
    })?;
    Ok(paths)
}

/// The prefix the storage of `place` is under.
fn prefix(place: &Place) -> SubtreePrefix {
    let primary = build_prefix(SubtreePath::from(place.path.as_slice())).unwrap();
    match place.secondary {
        None => primary,
        Some(tag) => secondary_prefix_for(&primary, tag).unwrap(),
    }
}

/// Opens the subtree at `place` and lists its keyspaces.
fn list<'db, E: Engine>(
    engine: &'db E,
    transaction: &E::Transaction<'db>,
    place: &Place,
    left_behind: bool,
    records: &mut Vec<Record>,
) -> Result<(), String> {
    let path = SubtreePath::from(place.path.as_slice());
    let handle = match place.secondary {
        None => engine.subtree(path, None, transaction).unwrap(),
        Some(_) => engine
            .subtree_by_prefix(prefix(place), None, transaction)
            .unwrap(),
    };
    subtree(&handle, place, left_behind, records)
}

/// Lists the three keyspaces of one subtree: its data, as nodes or as flat
/// records, its aux records and its meta records.
fn subtree<'db, S: SubtreeHandle<'db>>(
    handle: &S,
    place: &Place,
    left_behind: bool,
    records: &mut Vec<Record>,
) -> Result<(), String> {
    let mut list = |keyspace, key: &[u8], value: &[u8]| {
        records.push(Record {
            path: place.path.clone(),
            secondary: place.secondary,
            keyspace,
            key: key.to_vec(),
            value: value.to_vec(),
            left_behind,
        });
    };
    match place.data {
        Keyspace::Flat => walk(FlatStore::cursor(handle), |key, value| {
            list(Keyspace::Flat, key, value)
        })?,
        _ => walk(NodeStore::cursor(handle), |key, value| {
            list(Keyspace::Nodes, key, value)
        })?,
    }
    walk(FlatStore::cursor(&handle.aux()), |key, value| {
        list(Keyspace::Aux, key, value)
    })?;
    walk(FlatStore::cursor(&handle.meta()), |key, value| {
        list(Keyspace::Meta, key, value)
    })
}

/// Hands every record under `cursor` to `visit`, in key order.
fn walk(mut cursor: impl RawIterator, mut visit: impl FnMut(&[u8], &[u8])) -> Result<(), String> {
    cursor.seek_to_first().unwrap();
    while cursor.valid().unwrap() {
        match (cursor.key().unwrap(), cursor.value().unwrap()) {
            (Some(key), Some(value)) => visit(key, value),
            _ => return Err("a cursor at a record returned no key or no value".to_string()),
        }
        cursor.next().unwrap();
    }
    cursor
        .status()
        .map_err(|error| format!("a cursor stopped on an error: {error}"))
}

/// The first place two listings differ: the record of each at that place,
/// or none for a listing that has ended.
pub fn first_difference<'a>(
    reference: &'a [Record],
    candidate: &'a [Record],
) -> Option<(Option<&'a Record>, Option<&'a Record>)> {
    let at = reference
        .iter()
        .zip(candidate)
        .position(|(reference, candidate)| reference != candidate)
        .or_else(|| {
            (reference.len() != candidate.len()).then(|| reference.len().min(candidate.len()))
        })?;
    Some((reference.get(at), candidate.get(at)))
}

#[cfg(test)]
mod tests {
    use grovedb::{
        element::reference_path::ReferencePathType, Element, GroveDb, RocksDbEngine, TreeType,
    };

    use super::*;
    use crate::{
        diff::{engines::StandIn, pair::testing::scratch},
        workload::{
            apply, apply::INSERT_OPTIONS, AppendEntry, BatchOp, IndexedTree, Op, Transactions,
            Write,
        },
    };

    fn open<E: StreamEngine>() -> (tempfile::TempDir, GroveDb<E>) {
        let dir = scratch();
        let db = GroveDb::open_with_engine(dir.path()).expect("a database");
        (dir, db)
    }

    fn insert(path: &[&[u8]], key: &[u8], element: Element) -> Op {
        Op::Write(Write::Insert {
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            key: key.to_vec(),
            element,
        })
    }

    /// A grove with a Merk under a Merk, an append-only tree, an indexed
    /// tree with an entry, an aux record, and an item two references point
    /// at.
    fn grove() -> Vec<Op> {
        let reference = |to: &[u8]| {
            Element::new_bidirectional_reference(ReferencePathType::AbsolutePathReference(vec![
                b"tree".to_vec(),
                to.to_vec(),
            ]))
        };
        vec![
            insert(&[], b"tree", Element::empty_tree()),
            insert(&[b"tree"], b"inner", Element::empty_tree()),
            insert(&[b"tree", b"inner"], b"item", Element::new_item(vec![1])),
            insert(
                &[b"tree"],
                b"target",
                Element::new_item_allowing_bidirectional_references(vec![2]),
            ),
            insert(&[b"tree"], b"first", reference(b"target")),
            insert(&[b"tree"], b"second", reference(b"target")),
            insert(&[], b"log", Element::empty_mmr_tree()),
            Op::Write(Write::Append {
                path: vec![b"log".to_vec()],
                entry: AppendEntry::Mmr(vec![9; 40]),
            }),
            insert(&[], b"ranks", Element::empty_provable_count_indexed_tree()),
            Op::Write(Write::IndexedInsert {
                path: vec![b"ranks".to_vec()],
                key: b"entry".to_vec(),
                element: Element::empty_count_tree(),
                tree: IndexedTree::Count,
            }),
            Op::Write(Write::PutAux {
                key: b"aux".to_vec(),
                value: vec![7],
            }),
        ]
    }

    fn filled<E: StreamEngine>(db: &GroveDb<E>) {
        applied(db, &grove());
    }

    fn listed_after<E: StreamEngine>(db: &GroveDb<E>, before: &BTreeSet<Place>) -> Listing {
        let transaction = db.start_transaction();
        stored(db, &transaction, GroveVersion::latest(), before).expect("a listing")
    }

    fn listing<E: StreamEngine>(db: &GroveDb<E>) -> Vec<Record> {
        listed_after(db, &BTreeSet::new()).records
    }

    fn applied<E: StreamEngine>(db: &GroveDb<E>, ops: &[Op]) {
        let mut transactions = Transactions::new(db);
        for op in ops.iter().chain([&Op::Commit]) {
            transactions
                .apply(op, GroveVersion::latest())
                .unwrap()
                .unwrap_or_else(|error| panic!("{op:?}: {error}"));
        }
    }

    #[test]
    fn the_walk_lists_every_keyspace_it_reaches() {
        let (_dir, db) = open::<RocksDbEngine>();
        assert_eq!(listing(&db), Vec::new(), "an empty database stores nothing");
        filled(&db);
        let records = listing(&db);
        let has = |keyspace, path: &[&[u8]], secondary: Option<u8>, key: &[u8]| {
            records.iter().any(|record| {
                record.keyspace == keyspace
                    && record.path == path
                    && record.secondary == secondary
                    && record.key == key
            })
        };
        assert!(has(Keyspace::RootKey, &[], None, b""));
        assert!(has(Keyspace::Nodes, &[], None, b"tree"));
        assert!(has(Keyspace::Nodes, &[b"tree"], None, b"inner"));
        assert!(has(Keyspace::Nodes, &[b"tree", b"inner"], None, b"item"));
        assert!(has(Keyspace::Nodes, &[b"ranks"], None, b"entry"));
        assert!(has(Keyspace::Aux, &[], None, b"aux"));
        let secondary: Vec<&Record> = records
            .iter()
            .filter(|record| record.secondary.is_some())
            .collect();
        assert_eq!(secondary.len(), 1, "one row ranks the one entry");
        assert_eq!(secondary[0].path, [b"ranks".to_vec()]);
        assert_eq!(secondary[0].keyspace, Keyspace::Nodes);
        let flat: Vec<&Record> = records
            .iter()
            .filter(|record| record.keyspace == Keyspace::Flat)
            .collect();
        assert!(!flat.is_empty(), "the leaf of the append-only tree");
        assert!(flat.iter().all(|record| record.path == [b"log".to_vec()]));
        assert!(flat.iter().any(|record| record.value.ends_with(&[9; 40])));
        // A subtree comes before the subtrees under it, and those in the
        // order of their keys.
        let order: Vec<&[Vec<u8>]> = records
            .iter()
            .filter(|record| record.keyspace == Keyspace::Nodes && record.secondary.is_none())
            .map(|record| record.path.as_slice())
            .collect();
        let mut subtrees = order.clone();
        subtrees.dedup();
        assert_eq!(
            subtrees,
            [
                &[][..],
                &[b"ranks".to_vec()],
                &[b"tree".to_vec()],
                &[b"tree".to_vec(), b"inner".to_vec()],
            ]
        );
    }

    /// What no public read returns: the referrer list of the target, in the
    /// record the walk lists.
    #[test]
    fn the_walk_reads_the_referrer_list_no_hash_holds() {
        let grove_version = GroveVersion::latest();
        let (_dir, db) = open::<RocksDbEngine>();
        filled(&db);
        let read = db
            .get_raw([b"tree"].as_ref().into(), b"target", None, grove_version)
            .unwrap()
            .expect("the target");
        let Element::ItemWithBackwardsReferences(_, referrers, _) = &read else {
            panic!("{read:?}")
        };
        assert!(referrers.entries.is_empty(), "a read strips the list");

        let records = listing(&db);
        let target = records
            .iter()
            .find(|record| record.keyspace == Keyspace::Nodes && record.key == b"target")
            .expect("the node of the target");
        let stored = Element::raw_decode(&target.value, grove_version).expect("an element");
        let Element::ItemWithBackwardsReferences(_, referrers, _) = &stored else {
            panic!("{stored:?}")
        };
        assert_eq!(referrers.entries.len(), 2, "both references are listed");

        // A database with one reference less: every read of the target
        // returns the same, and its record differs in the list alone.
        let (_dir, other) = open::<RocksDbEngine>();
        filled(&other);
        other
            .delete([b"tree"].as_ref(), b"second", None, None, grove_version)
            .unwrap()
            .expect("the second reference is deleted");
        let read_again = other
            .get_raw([b"tree"].as_ref().into(), b"target", None, grove_version)
            .unwrap()
            .expect("the target");
        assert_eq!(read_again, read);
        let fewer = listing(&other);
        let other_target = fewer
            .iter()
            .find(|record| record.keyspace == Keyspace::Nodes && record.key == b"target")
            .expect("the node of the target");
        assert_ne!(other_target.value, target.value);
        let other_stored =
            Element::raw_decode(&other_target.value, grove_version).expect("an element");
        let Element::ItemWithBackwardsReferences(_, fewer_referrers, _) = &other_stored else {
            panic!("{other_stored:?}")
        };
        assert_eq!(fewer_referrers.entries.len(), 1);
        assert_eq!(
            other_stored.stripped_of_backward_references(),
            stored.stripped_of_backward_references()
        );
        assert!(first_difference(&records, &fewer).is_some());
    }

    #[test]
    fn two_engines_that_hold_the_same_list_the_same() {
        let (_dir, reference) = open::<RocksDbEngine>();
        let (_dir, candidate) = open::<StandIn>();
        filled(&reference);
        filled(&candidate);
        let (reference_records, candidate_records) = (listing(&reference), listing(&candidate));
        assert!(reference_records.len() > 10);
        assert_eq!(
            first_difference(&reference_records, &candidate_records),
            None
        );

        // A record no hash covers, in one of them only.
        let grove_version = GroveVersion::latest();
        apply(
            &candidate,
            &Op::Write(Write::PutAux {
                key: b"aux".to_vec(),
                value: vec![8],
            }),
            None,
            grove_version,
        )
        .unwrap()
        .expect("an aux record");
        let root = |db: &GroveDb<_>| db.root_hash(None, grove_version).unwrap().expect("a root");
        assert_eq!(
            reference.root_hash(None, grove_version).unwrap().unwrap(),
            root(&candidate)
        );
        let candidate_records = listing(&candidate);
        let (was, is) =
            first_difference(&reference_records, &candidate_records).expect("the aux record");
        let (was, is) = (was.expect("a record"), is.expect("a record"));
        assert_eq!((was.keyspace, is.keyspace), (Keyspace::Aux, Keyspace::Aux));
        assert_eq!(
            (was.value.as_slice(), is.value.as_slice()),
            (&[7][..], &[8][..])
        );
        assert_eq!(was.text(), "aux / key 617578 = 1 bytes 07");
    }

    #[test]
    fn the_walk_sees_what_its_transaction_wrote() {
        let grove_version = GroveVersion::latest();
        let (_dir, db) = open::<RocksDbEngine>();
        let transaction = db.start_transaction();
        db.insert(
            <&[&[u8]]>::default(),
            b"tree",
            Element::empty_tree(),
            Some(INSERT_OPTIONS),
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("an insert");
        let inside = stored(&db, &transaction, grove_version, &BTreeSet::new()).expect("a listing");
        assert_eq!(inside.records.len(), 2, "the root key and the node");
        let paths: Vec<&[Vec<u8>]> = inside.places.iter().map(|place| &place.path[..]).collect();
        assert_eq!(paths, [&[][..], &[b"tree".to_vec()]]);
        assert_eq!(listing(&db), Vec::new(), "nothing is committed");
    }

    #[test]
    fn listings_of_other_lengths_differ_where_the_shorter_ends() {
        let record = |key: u8| Record {
            path: vec![vec![1, 2], vec![3]],
            secondary: Some(1),
            keyspace: Keyspace::Nodes,
            key: vec![key],
            value: vec![0xab; 300],
            left_behind: key == 2,
        };
        let (one, two) = (vec![record(1)], vec![record(1), record(2)]);
        assert_eq!(first_difference(&one, &one), None);
        assert_eq!(first_difference(&one, &two), Some((None, Some(&two[1]))));
        assert_eq!(first_difference(&two, &one), Some((Some(&two[1]), None)));
        let text = record(1).text();
        assert!(text.starts_with("node /0102/03 secondary 1 key 01 = 300 bytes abab"));
        assert!(text.ends_with('…'));
        assert!(record(2).text().starts_with("left behind: node /0102/03 "));
        for (keyspace, name) in [
            (Keyspace::RootKey, "root-key"),
            (Keyspace::Flat, "flat"),
            (Keyspace::Meta, "meta"),
            (Keyspace::PendingDrop, "pending-drop"),
            (Keyspace::DropRecord, "drop-record"),
        ] {
            assert_eq!(keyspace.name(), name);
        }
    }

    /// The subtrees an earlier walk listed and this one does not reach are
    /// looked into again: what a delete left behind is listed, and a
    /// delete that cleared its subtree leaves nothing.
    #[test]
    fn what_is_left_under_a_path_that_is_gone_is_listed() {
        let (_dir, db) = open::<RocksDbEngine>();
        filled(&db);
        let before = listed_after(&db, &BTreeSet::new());
        let gone = |path: &[&[u8]], secondary: Option<u8>, data| Place {
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            secondary,
            data,
        };
        for place in [
            gone(&[], None, Keyspace::Nodes),
            gone(&[b"tree"], None, Keyspace::Nodes),
            gone(&[b"tree", b"inner"], None, Keyspace::Nodes),
            gone(&[b"log"], None, Keyspace::Flat),
            gone(&[b"ranks"], None, Keyspace::Nodes),
            gone(&[b"ranks"], Some(IndexAxis::Count.tag()), Keyspace::Nodes),
            gone(&[b"ranks"], Some(IndexAxis::BigSum.tag()), Keyspace::Nodes),
            gone(&[b"ranks", b"entry"], None, Keyspace::Nodes),
        ] {
            assert!(before.places.contains(&place), "{place:?}");
        }
        assert_eq!(before.places.len(), 4 + 1 + AXES.len() + 1);
        assert!(before.records.iter().all(|record| !record.left_behind));

        // The append-only tree, the indexed tree and the nested tree are
        // deleted with what they hold.
        let delete_tree = |key: &[u8], tree_type| BatchOp::DeleteTree {
            path: Vec::new(),
            key: key.to_vec(),
            tree_type,
        };
        applied(
            &db,
            &[
                Op::Batch(vec![
                    delete_tree(b"log", TreeType::MmrTree),
                    delete_tree(b"ranks", TreeType::ProvableCountIndexedTree),
                ]),
                Op::Write(Write::Delete {
                    path: vec![b"tree".to_vec()],
                    key: b"inner".to_vec(),
                }),
            ],
        );
        let after = listed_after(&db, &before.places);
        let paths: Vec<&[Vec<u8>]> = after.places.iter().map(|place| &place.path[..]).collect();
        assert_eq!(paths, [&[][..], &[b"tree".to_vec()]]);
        let left: Vec<String> = after
            .records
            .iter()
            .filter(|record| record.left_behind)
            .map(Record::text)
            .collect();
        assert_eq!(left, Vec::<String>::new(), "GroveDB cleared every subtree");

        // A record put under a path that is gone, as an engine that did
        // not clear it would have left one.
        let transaction = db.start_transaction();
        {
            let handle = db
                .engine()
                .immediate_subtree(
                    SubtreePath::from([b"log".as_slice()].as_slice()),
                    &transaction,
                )
                .unwrap();
            FlatStore::put(&handle, b"stale", b"leaf", None)
                .unwrap()
                .expect("a record");
            FlatStore::put(&handle.aux(), b"stale", b"aux", None)
                .unwrap()
                .expect("a record");
        }
        let listed =
            stored(&db, &transaction, GroveVersion::latest(), &before.places).expect("a listing");
        let left: Vec<String> = listed
            .records
            .iter()
            .filter(|record| record.left_behind)
            .map(Record::text)
            .collect();
        assert_eq!(
            left,
            [
                "left behind: flat /6c6f67 key 7374616c65 = 4 bytes 6c656166",
                "left behind: aux /6c6f67 key 7374616c65 = 3 bytes 617578",
            ]
        );
        // A walk that was told of no earlier place does not look there.
        let unaware =
            stored(&db, &transaction, GroveVersion::latest(), &BTreeSet::new()).expect("a listing");
        assert!(unaware.records.iter().all(|record| !record.left_behind));
        assert_eq!(unaware.records.len() + 2, listed.records.len());
    }

    /// A record left behind under one kind of data is left out where the
    /// walk listed the same record at the same path under the other kind,
    /// and only there: another key, other bytes, another path or another
    /// secondary is a record that was left behind.
    #[test]
    fn a_column_shown_twice_is_listed_once_and_what_was_left_behind_stays() {
        let record = |path: &[u8], keyspace, key: &[u8], value: u8, left_behind| Record {
            path: vec![path.to_vec()],
            secondary: None,
            keyspace,
            key: key.to_vec(),
            value: vec![value],
            left_behind,
        };
        let (nodes, flat) = (Keyspace::Nodes, Keyspace::Flat);
        // A Merk lives at `p` where a flat tree was, with the nodes `a`
        // and `b`; a flat tree lives at `q` where a Merk was.
        let reached = vec![
            record(b"p", nodes, b"a", 1, false),
            record(b"p", nodes, b"b", 2, false),
            record(b"p", Keyspace::Aux, b"x", 9, false),
            record(b"q", flat, b"r", 3, false),
        ];
        let twice = vec![
            record(b"p", flat, b"a", 1, true),
            record(b"p", flat, b"b", 2, true),
            record(b"q", nodes, b"r", 3, true),
        ];
        let mut secondary = record(b"p", flat, b"a", 1, true);
        secondary.secondary = Some(1);
        let left = vec![
            // Planted beside the Merk's nodes: a key no node has, the bytes
            // of no node, the aux record's key, another path, a secondary.
            record(b"p", flat, b"planted", 1, true),
            record(b"p", flat, b"a", 7, true),
            record(b"p", flat, b"x", 9, true),
            record(b"other", flat, b"a", 1, true),
            secondary,
            record(b"q", nodes, b"r", 4, true),
            record(b"p", Keyspace::Aux, b"x", 9, true),
        ];
        let mut listed = reached.clone();
        listed.extend(twice);
        listed.extend(left.clone());
        let mut expected = reached.clone();
        expected.extend(left);
        assert_eq!(without_a_column_shown_twice(listed), expected);
        // Nothing left behind: nothing is looked for.
        assert_eq!(without_a_column_shown_twice(reached.clone()), reached);
        // A record that was reached twice is no record left behind.
        let mut both = reached.clone();
        both.push(record(b"p", flat, b"a", 1, false));
        assert_eq!(without_a_column_shown_twice(both.clone()), both);
    }

    /// A subtree dropped flat is listed as a pending drop until the drop is
    /// flushed, by the report every engine gives.
    #[test]
    fn the_drops_that_are_pending_are_listed() {
        let grove_version = GroveVersion::latest();
        let (_dir, db) = open::<RocksDbEngine>();
        filled(&db);
        applied(
            &db,
            &[Op::Write(Write::DropFlat {
                path: Vec::new(),
                key: b"log".to_vec(),
            })],
        );
        let pending: Vec<Record> = listing(&db)
            .into_iter()
            .filter(|record| record.keyspace == Keyspace::PendingDrop)
            .collect();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].path, [b"log".to_vec()]);
        let prefix = build_prefix(SubtreePath::from([b"log".as_slice()].as_slice())).unwrap();
        assert_eq!(pending[0].key, prefix);
        assert_eq!(pending[0].value, prefix, "the one prefix the drop frees");

        let report = db
            .flush_pending_prefix_drops(grove_version)
            .expect("the drop is flushed");
        assert_eq!(report.reclaimed_records, 1);
        assert!(listing(&db)
            .iter()
            .all(|record| record.keyspace != Keyspace::PendingDrop));
    }

    /// A subtree a pending drop is to free is not looked at, whether the
    /// drop is staged in the transaction of the walk or committed, and is
    /// handed on. A drop that was rolled back excuses nothing. The walk
    /// after the flush looks at the subtree again.
    #[test]
    fn a_pending_drop_excuses_its_subtree_until_it_is_flushed() {
        let grove_version = GroveVersion::latest();
        let (_dir, db) = open::<RocksDbEngine>();
        filled(&db);
        let before = listed_after(&db, &BTreeSet::new());
        let log = Place {
            path: vec![b"log".to_vec()],
            secondary: None,
            data: Keyspace::Flat,
        };
        assert!(before.places.contains(&log) && before.dropped.is_empty());
        let drop = Op::Write(Write::DropFlat {
            path: Vec::new(),
            key: b"log".to_vec(),
        });
        let count = |listing: &Listing, keyspace| {
            listing
                .records
                .iter()
                .filter(|record| record.keyspace == keyspace)
                .count()
        };
        let left = |listing: &Listing| {
            listing
                .records
                .iter()
                .filter(|record| record.left_behind)
                .count()
        };
        let kept = |transaction| {
            let mut records = Vec::new();
            list(db.engine(), transaction, &log, true, &mut records).expect("a listing");
            records.len()
        };

        // Staged: the transaction sees the record of its drop, the engine
        // reports no drop yet, and the records of the tree are still there.
        let transaction = db.start_transaction();
        apply(&db, &drop, Some(&transaction), grove_version)
            .unwrap()
            .expect("a drop");
        let staged = stored(&db, &transaction, grove_version, &before.places).expect("a listing");
        assert!(!staged.places.contains(&log));
        assert_eq!(staged.dropped, BTreeSet::from([log.clone()]));
        assert_eq!(
            (
                left(&staged),
                count(&staged, Keyspace::DropRecord),
                count(&staged, Keyspace::PendingDrop)
            ),
            (0, 1, 0)
        );
        assert!(kept(&transaction) > 0);
        let handed_on = staged.handed_on();
        assert!(handed_on.contains(&log));

        // Rolled back: the tree is under its path again.
        std::mem::drop(transaction);
        let rolled_back = listed_after(&db, &handed_on);
        assert!(rolled_back.places.contains(&log) && rolled_back.dropped.is_empty());
        assert_eq!(count(&rolled_back, Keyspace::DropRecord), 0);

        // Committed: the engine reports the drop as well.
        applied(&db, std::slice::from_ref(&drop));
        let committed = listed_after(&db, &rolled_back.handed_on());
        assert_eq!(committed.dropped, BTreeSet::from([log.clone()]));
        assert_eq!(
            (
                left(&committed),
                count(&committed, Keyspace::DropRecord),
                count(&committed, Keyspace::PendingDrop)
            ),
            (0, 1, 1)
        );
        let handed_on = committed.handed_on();

        // A subtree no drop names is looked at as before.
        let other = Place {
            path: vec![b"gone".to_vec()],
            ..log.clone()
        };
        let mut told = handed_on.clone();
        told.insert(other.clone());
        let listed = listed_after(&db, &told);
        assert_eq!(listed.dropped, BTreeSet::from([log.clone()]));
        assert!(!listed.handed_on().contains(&other));

        // Flushed: the subtree is looked at, holds nothing, and is handed
        // on no further.
        db.flush_pending_prefix_drops(grove_version)
            .expect("the drop is flushed");
        let flushed = listed_after(&db, &handed_on);
        assert!(flushed.dropped.is_empty());
        assert_eq!(
            (
                left(&flushed),
                count(&flushed, Keyspace::DropRecord),
                count(&flushed, Keyspace::PendingDrop)
            ),
            (0, 0, 0)
        );
        assert!(!flushed.handed_on().contains(&log));

        // A record that a flush did not free is listed as left behind.
        let transaction = db.start_transaction();
        {
            let handle = db
                .engine()
                .immediate_subtree(SubtreePath::from(log.path.as_slice()), &transaction)
                .unwrap();
            FlatStore::put(&handle, b"kept", b"leaf", None)
                .unwrap()
                .expect("a record");
        }
        let listed = stored(&db, &transaction, grove_version, &handed_on).expect("a listing");
        assert_eq!(left(&listed), 1);
    }

    #[test]
    fn every_axis_of_an_indexed_tree_has_a_keyspace_of_its_own_tag() {
        let tags: BTreeSet<u8> = AXES.iter().map(|axis| axis.tag()).collect();
        assert_eq!(tags.len(), AXES.len());
        assert_eq!(listed(IndexAxis::BigSum), AXES.len() - 1);
    }
}
