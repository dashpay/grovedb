//! What [`hosted_by_element`] says an element hosts is what the engine
//! entered under that element: for every entry of the subtree directory
//! that names a host, the subtree, its position and its tree type, and
//! the root where the element names one.

use std::collections::{BTreeMap, BTreeSet};

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    store::{
        lignin::{keeps_flat_records, tree_type_bytes, LigninNodeStore, LigninSubtree},
        NodeStore,
    },
    tree::kv::ValueDefinedCostType,
    TreeType,
};
use grovedb_storage::prefix::secondary_prefix_for;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{Appended, Bulk, Commitment, Dense, Mmr},
    sync::Made,
    Lignin, LigninEngine,
};
use crate::{
    batch::QualifiedGroveDbOp,
    engine::lignin::{hosted_by_element, HostedSubtree},
    operations::indexed_tree::axis_storage_tag,
    Element,
};

/// What the directory holds of one subtree under a host: its position,
/// its prefix, the bytes of its tree type and the key of its root.
type Entered = (u8, [u8; 32], Vec<u8>, Option<Vec<u8>>);

/// The value of the node under `key` of the Merk at `parent`.
fn value_of(db: &Lignin, parent: [u8; 32], key: &[u8]) -> Vec<u8> {
    let tx = db.start_transaction();
    let handle = LigninNodeStore::read_only(tx.view(), LigninSubtree::alone(parent));
    let no_cost = None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>;
    let node = handle.get_node(key, no_cost, GroveVersion::latest());
    let node = node.unwrap().expect("a read of the host");
    node.expect("a host is a node").value_as_slice().to_vec()
}

/// What a grove was found to hold, by the elements that host a subtree.
#[derive(Debug, Default)]
struct Met {
    /// The tree types of the trees of one Merk.
    merks: BTreeSet<Vec<u8>>,
    /// The tree types of the trees that keep flat records.
    flats: BTreeSet<Vec<u8>>,
    /// How many of those have a Merk beside the records: an entry with a
    /// root.
    flats_with_a_merk: usize,
    /// The indexed trees, each with how many Merks it hosts.
    indexed: Vec<usize>,
}

/// Holds every element that the directory of `db` names as a host to what
/// [`hosted_by_element`] says of it, and returns what was met.
fn held(db: &Lignin) -> Met {
    let mut by_host: BTreeMap<([u8; 32], Vec<u8>), Vec<Entered>> = BTreeMap::new();
    {
        let snapshot = db.engine().database().snapshot();
        let operation = snapshot.operation();
        let mut walk = operation.entries().expect("the directory");
        while let Some(listed) = walk.next().expect("an entry") {
            let entry = &listed.entry;
            let Some(host) = entry.host().filter(|_| listed.live) else {
                continue;
            };
            let under = by_host.entry((*host.parent, host.key.to_vec()));
            under.or_default().push((
                host.position,
                *listed.subtree.as_bytes(),
                entry.tree_type().to_vec(),
                entry.root().map(|root| root.key.to_vec()),
            ));
        }
    }
    let mut met = Met::default();
    for ((parent, key), mut entered) in by_host {
        entered.sort();
        let (first, own) = (entered[0].0, entered[0].1);
        assert_eq!(
            first, 0,
            "the subtree of the element's own path is its first"
        );
        let value = value_of(db, parent, &key);
        let hosted = hosted_by_element(&value, own).expect("the element is read");
        let said: Vec<(u8, [u8; 32], Vec<u8>)> = (hosted.iter().enumerate())
            .map(|(at, hosted)| (at as u8, hosted.prefix, tree_type_bytes(hosted.tree_type)))
            .collect();
        let found: Vec<(u8, [u8; 32], Vec<u8>)> = (entered.iter())
            .map(|(at, prefix, tree_type, _)| (*at, *prefix, tree_type.clone()))
            .collect();
        assert_eq!(said, found, "what the element under {key:?} hosts");
        for (hosted, (_, _, _, root)) in hosted.iter().zip(&entered) {
            if hosted.root_key.is_some() {
                assert_eq!(&hosted.root_key, root, "a root the element names");
            }
        }
        let (tree_type, root) = (&entered[0].2, &entered[0].3);
        match (entered.len(), keeps_flat_records(tree_type)) {
            (1, true) => {
                assert_eq!(hosted[0].root_key, None, "a flat tree names no root");
                met.flats.insert(tree_type.clone());
                met.flats_with_a_merk += usize::from(root.is_some());
            }
            (1, false) => {
                met.merks.insert(tree_type.clone());
            }
            (many, _) => met.indexed.push(many),
        }
    }
    met
}

/// A grove with a tree of every kind the grove version makes: each
/// element that hosts a subtree is said to host what the engine entered
/// for it.
#[test]
fn an_element_of_every_kind_hosts_what_the_engine_entered_for_it() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let made = Made::<LigninEngine>::new(v);
        let met = held(&made.db);
        let kinds = if number >= 5 { 12 } else { 9 };
        let append_only = if number >= 4 { 5 } else { 4 };
        assert_eq!(met.merks.len(), kinds, "grove version {number}: {met:?}");
        assert_eq!(met.flats.len(), append_only, "grove version {number}");
        assert_eq!(met.flats_with_a_merk, 0, "grove version {number}");
        // Every filled indexed tree, with its primary and a secondary for
        // each axis.
        let filled = made.shape.indexed.iter();
        let filled = filled
            .filter(|(at, _)| !(at.last()).is_some_and(|key| key.ends_with(b" with nothing")));
        let mut expected: Vec<usize> = filled.map(|(_, axes)| 1 + axes.len()).collect();
        let mut found = met.indexed.clone();
        expected.sort_unstable();
        found.sort_unstable();
        assert_eq!(found, expected, "grove version {number}");
        assert!(found.len() >= 4 && found.iter().any(|merks| *merks > 2));
    }
}

/// On grove versions 1 to 3 a tree that keeps flat records can be given a
/// keyed child, whose level is a Merk under the tree's prefix: the tree
/// hosts that one subtree, in the first place, and its element names no
/// root for it.
#[test]
fn a_flat_tree_with_a_keyed_child_hosts_its_one_subtree_in_the_first_place() {
    fn case<T: Appended>(v: &GroveVersion) {
        let tree = T::NAME.as_bytes();
        let dir = tempfile::tempdir().expect("a directory");
        let db = Lignin::open_with_engine(dir.path()).expect("a database");
        let made = QualifiedGroveDbOp::insert_or_replace_op(vec![], tree.to_vec(), T::empty(None));
        let item = Element::new_item(b"a keyed child".to_vec());
        let child =
            QualifiedGroveDbOp::insert_or_replace_op(vec![tree.to_vec()], b"child".to_vec(), item);
        (db.apply_batch(vec![made, child], None, None, v).unwrap()).expect("the batch is taken");
        for n in 0..2 {
            (T::append(&db, &[], tree, n, None, v).unwrap()).expect("an append");
        }
        let met = held(&db);
        assert_eq!((met.flats.len(), met.flats_with_a_merk), (1, 1), "{met:?}");
        assert!(met.merks.is_empty() && met.indexed.is_empty(), "{met:?}");
    }
    for v in GROVE_VERSIONS.iter().take(3) {
        case::<Mmr>(v);
        case::<Bulk>(v);
        case::<Dense>(v);
        case::<Commitment>(v);
    }
}

/// What is no tree hosts nothing, and an axis that an element names twice
/// has one secondary, at the place and with the root of its first naming.
#[test]
fn what_is_no_tree_hosts_nothing_and_an_axis_named_twice_is_hosted_once() {
    let v = GroveVersion::latest();
    let own = [7; 32];
    assert_eq!(hosted_by_element(b"no element", own).unwrap(), Vec::new());
    let item = Element::new_item(b"an item".to_vec()).serialize(v).unwrap();
    assert_eq!(hosted_by_element(&item, own).unwrap(), Vec::new());

    let tree = Element::new_tree(Some(b"root".to_vec()))
        .serialize(v)
        .unwrap();
    let hosted = hosted_by_element(&tree, own).unwrap();
    let expected = HostedSubtree {
        prefix: own,
        tree_type: TreeType::NormalTree,
        root_key: Some(b"root".to_vec()),
    };
    assert_eq!(hosted, vec![expected]);

    let (count, sum) = (IndexAxis::Count, IndexAxis::Sum);
    let axes = vec![
        (count.tag(), Some(b"first".to_vec())),
        (sum.tag(), None),
        (count.tag(), Some(b"second".to_vec())),
    ];
    let twice = Element::ProvableCountProvableSumIndexedTree(None, 0, 0, axes, None);
    let hosted = hosted_by_element(&twice.serialize(v).unwrap(), own).unwrap();
    let secondary = |axis| secondary_prefix_for(&own, axis_storage_tag(axis)).value;
    let found: Vec<([u8; 32], Option<Vec<u8>>)> = (hosted.iter())
        .map(|hosted| (hosted.prefix, hosted.root_key.clone()))
        .collect();
    assert_eq!(
        found,
        vec![
            (own, None),
            (secondary(count), Some(b"first".to_vec())),
            (secondary(sum), None),
        ]
    );
    assert_eq!(
        hosted[0].tree_type,
        TreeType::ProvableCountProvableSumIndexedTree
    );
}
