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
//! are not under a path, are reached by their prefixes.
//!
//! The listing is engine-neutral by the contract of the two seams: a node
//! is shown in Merk's encoding whatever an engine keeps of it, and a flat
//! record is the bytes that were put.

use grovedb::{engine::SubtreeHandle, Element};
use grovedb_merk::{element::decode::ElementDecodeExtensions, store::NodeStore};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    prefix::{build_prefix, secondary_prefix_for},
    FlatStore, RawIterator,
};
use grovedb_version::version::GroveVersion;

use crate::workload::{Db, StreamEngine, Tx};

/// The tags an indexed tree keeps its secondary trees under: one for each
/// axis it can be ranked on. A tag no tree uses names an empty keyspace.
const SECONDARY_TAGS: std::ops::RangeInclusive<u8> = 0..=7;

/// The keyspace of a subtree a record is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl Keyspace {
    fn name(self) -> &'static str {
        match self {
            Keyspace::RootKey => "root-key",
            Keyspace::Nodes => "node",
            Keyspace::Flat => "flat",
            Keyspace::Aux => "aux",
            Keyspace::Meta => "meta",
        }
    }
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
            "{} /{}{secondary} key {} = {} bytes {value}",
            self.keyspace.name(),
            path.join("/"),
            hex::encode(&self.key),
            self.value.len(),
        )
    }
}

/// Every record `db` stores, as `transaction` sees it, in the order of the
/// walk: a subtree before the subtrees under it.
pub fn stored<'db, E: StreamEngine>(
    db: &'db Db<E>,
    transaction: &Tx<'db, E>,
    grove_version: &GroveVersion,
) -> Result<Vec<Record>, String> {
    let engine = db.engine();
    let mut records = Vec::new();
    let mut merks = vec![Vec::new()];
    while let Some(path) = merks.pop() {
        let handle = engine
            .subtree(SubtreePath::from(path.as_slice()), None, transaction)
            .unwrap();
        if path.is_empty() {
            let root_key = NodeStore::root_key(&handle)
                .unwrap()
                .map_err(|error| format!("reading the root key: {error}"))?;
            if let Some(root_key) = root_key {
                records.push(Record {
                    path: path.clone(),
                    secondary: None,
                    keyspace: Keyspace::RootKey,
                    key: Vec::new(),
                    value: root_key,
                });
            }
        }
        let from = records.len();
        subtree(&handle, &path, None, Keyspace::Nodes, &mut records)?;
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
            let mut below = path.clone();
            below.push(key);
            if element.uses_non_merk_data_storage() {
                let handle = engine
                    .subtree(SubtreePath::from(below.as_slice()), None, transaction)
                    .unwrap();
                subtree(&handle, &below, None, Keyspace::Flat, &mut records)?;
                continue;
            }
            if element.is_indexed_tree() {
                let primary = build_prefix(SubtreePath::from(below.as_slice())).unwrap();
                for tag in SECONDARY_TAGS {
                    let prefix = secondary_prefix_for(&primary, tag).unwrap();
                    let handle = engine.subtree_by_prefix(prefix, None, transaction).unwrap();
                    subtree(&handle, &below, Some(tag), Keyspace::Nodes, &mut records)?;
                }
            }
            merks.push(below);
        }
    }
    Ok(records)
}

/// Lists the three keyspaces of one subtree: its data, as nodes or as flat
/// records, its aux records and its meta records.
fn subtree<'db, S: SubtreeHandle<'db>>(
    handle: &S,
    path: &[Vec<u8>],
    secondary: Option<u8>,
    data: Keyspace,
    records: &mut Vec<Record>,
) -> Result<(), String> {
    let mut list = |keyspace, key: &[u8], value: &[u8]| {
        records.push(Record {
            path: path.to_vec(),
            secondary,
            keyspace,
            key: key.to_vec(),
            value: value.to_vec(),
        });
    };
    match data {
        Keyspace::Flat => walk(FlatStore::cursor(handle), |key, value| {
            list(data, key, value)
        })?,
        _ => walk(NodeStore::cursor(handle), |key, value| {
            list(data, key, value)
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
    use grovedb::{element::reference_path::ReferencePathType, Element, GroveDb, RocksDbEngine};

    use super::*;
    use crate::{
        diff::engines::StandIn,
        workload::{
            apply, apply::INSERT_OPTIONS, AppendEntry, BatchOp, IndexedTree, Op, Transactions,
            Write,
        },
    };

    fn open<E: StreamEngine>() -> (tempfile::TempDir, GroveDb<E>) {
        let dir = tempfile::tempdir().expect("a directory");
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
        let grove_version = GroveVersion::latest();
        let mut transactions = Transactions::new(db);
        for op in grove().iter().chain([&Op::Commit]) {
            transactions
                .apply(op, grove_version)
                .unwrap()
                .unwrap_or_else(|error| panic!("{op:?}: {error}"));
        }
    }

    fn listing<E: StreamEngine>(db: &GroveDb<E>) -> Vec<Record> {
        let transaction = db.start_transaction();
        stored(db, &transaction, GroveVersion::latest()).expect("a listing")
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
        let inside = stored(&db, &transaction, grove_version).expect("a listing");
        assert_eq!(inside.len(), 2, "the root key and the node");
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
        };
        let (one, two) = (vec![record(1)], vec![record(1), record(2)]);
        assert_eq!(first_difference(&one, &one), None);
        assert_eq!(first_difference(&one, &two), Some((None, Some(&two[1]))));
        assert_eq!(first_difference(&two, &one), Some((Some(&two[1]), None)));
        let text = record(1).text();
        assert!(text.starts_with("node /0102/03 secondary 1 key 01 = 300 bytes abab"));
        assert!(text.ends_with('…'));
        for (keyspace, name) in [
            (Keyspace::RootKey, "root-key"),
            (Keyspace::Flat, "flat"),
            (Keyspace::Meta, "meta"),
        ] {
            assert_eq!(keyspace.name(), name);
        }
    }

    #[test]
    fn a_batch_that_deletes_a_tree_leaves_none_of_its_records() {
        let grove_version = GroveVersion::latest();
        let (_dir, db) = open::<RocksDbEngine>();
        filled(&db);
        let mut transactions = Transactions::new(&db);
        let delete = Op::Batch(vec![BatchOp::DeleteTree {
            path: Vec::new(),
            key: b"log".to_vec(),
            tree_type: grovedb::TreeType::MmrTree,
        }]);
        for op in [&delete, &Op::Commit] {
            transactions
                .apply(op, grove_version)
                .unwrap()
                .expect("the tree is deleted");
        }
        drop(transactions);
        assert!(listing(&db)
            .iter()
            .all(|record| record.keyspace != Keyspace::Flat));
    }
}
