//! The cost rules of stop points and finalize
//! (`docs/lignindb/cost-model.md`, section 16), held to numbers.
//!
//! Each test is named after the rule it holds. The grove of these tests is
//! [`model::Grove`], which applies a write and a finalize as the rules say
//! they are charged.

mod estimates;
mod model;

use grovedb_costs::{
    storage_cost::removal::StorageRemovedBytes::{
        BasicStorageRemoval, NoStorageRemoval, SectionedStorageRemoval,
    },
    OperationCost,
};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    tree::{axes_digest, combine_hash, combine_hash_three, kv::ValueDefinedCostType, value_hash},
    tree_type::TreeType,
};
use grovedb_path::SubtreePath;
use grovedb_version::version::{v5::GROVE_V5, v6::GROVE_V6, GroveVersion};

use self::model::{Change, Grove, Path, WriteCost};
use crate::{
    tests::{make_empty_grovedb, TempGroveDb},
    Element,
};

/// Every tree that keeps its elements in a Merk of its own and is no
/// indexed tree: the trees a write can stop at.
fn trees() -> Vec<Element> {
    vec![
        Element::empty_tree(),
        Element::empty_sum_tree(),
        Element::empty_big_sum_tree(),
        Element::empty_count_tree(),
        Element::empty_count_sum_tree(),
        Element::empty_provable_count_tree(),
        Element::empty_provable_count_sum_tree(),
        Element::empty_provable_sum_tree(),
        Element::empty_provable_big_sum_tree(),
        Element::empty_provable_count_provable_sum_tree(),
        Element::empty_provable_count_provable_big_sum_tree(),
        Element::empty_count_big_sum_tree(),
    ]
}

fn tree_type_of(tree: &Element) -> TreeType {
    tree.tree_type().expect("a tree")
}

/// An element for a Merk of `tree_type`, of `size`: an item, or where the
/// tree keeps a sum, a sum item whose value takes `size` bytes as a varint,
/// so that the aggregates above it change in length with it.
fn element(tree_type: TreeType, size: usize) -> Element {
    if tree_type.is_sum_bearing() {
        Element::new_sum_item(1i64 << (7 * size.clamp(1, 8) - 2))
    } else {
        Element::new_item(vec![0xA5; size * 20])
    }
}

fn path(segments: &[&[u8]]) -> Path {
    segments.iter().map(|segment| segment.to_vec()).collect()
}

fn child(parent: &[Vec<u8>], key: &[u8]) -> Path {
    let mut path = parent.to_vec();
    path.push(key.to_vec());
    path
}

fn refs(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

fn key(number: u8) -> Vec<u8> {
    vec![b'k', number]
}

/// A grove before any write of a test: its trees, each by its path and in
/// an order in which a host comes before what it hosts, and the elements
/// they hold.
#[derive(Clone, Default)]
struct Shape {
    trees: Vec<(Path, Element)>,
    elements: Vec<(Path, Vec<u8>, Element)>,
}

impl Shape {
    fn tree(&mut self, path: Path, tree: &Element) -> Path {
        self.trees.push((path.clone(), tree.clone()));
        path
    }

    /// A tree at `path` that holds `count` elements of `size`.
    fn filled(&mut self, path: Path, tree: &Element, count: u8, size: usize) -> Path {
        let tree_type = tree_type_of(tree);
        self.tree(path.clone(), tree);
        for number in 0..count {
            self.elements
                .push((path.clone(), key(number), element(tree_type, size)));
        }
        path
    }

    fn tree_type(&self, path: &[Vec<u8>]) -> TreeType {
        if path.is_empty() {
            return TreeType::NormalTree;
        }
        let (_, tree) = self
            .trees
            .iter()
            .find(|(at, _)| at == path)
            .expect("a tree of the shape");
        tree_type_of(tree)
    }

    /// The elements a Merk of the shape holds, its trees among them.
    fn count(&self, path: &[Vec<u8>]) -> usize {
        let hosted = |at: &Path| at.split_last().is_some_and(|(_, parent)| parent == path);
        self.trees.iter().filter(|(at, _)| hosted(at)).count()
            + self.elements.iter().filter(|(at, ..)| at == path).count()
    }

    /// The shape as `writes` leave it.
    fn after(&self, writes: &[Write]) -> Shape {
        let mut after = self.clone();
        for write in writes {
            for (key, change) in &write.changes {
                let at = child(&write.path, key);
                // What was under the key goes, with everything below it.
                after.trees.retain(|(path, _)| !path.starts_with(&at));
                after.elements.retain(|(path, held, _)| {
                    !path.starts_with(&at) && !(*path == write.path && held == key)
                });
                if let Change::Put(element) = change {
                    if element.is_any_tree() {
                        after.trees.push((at, element.clone()));
                    } else {
                        after
                            .elements
                            .push((write.path.clone(), key.clone(), element.clone()));
                    }
                }
            }
        }
        after
    }

    /// The grove, committed, as grove version 5 writes it.
    fn build(&self) -> TempGroveDb {
        let db = make_empty_grovedb();
        for (path, tree) in &self.trees {
            let (key, parent) = path.split_last().expect("a tree has a host");
            let parent = refs(parent);
            db.insert(
                SubtreePath::from(parent.as_slice()),
                key,
                tree.clone(),
                None,
                None,
                &GROVE_V5,
            )
            .unwrap()
            .expect("a tree of the shape is inserted");
        }
        for (path, key, element) in &self.elements {
            let path = refs(path);
            db.insert(
                SubtreePath::from(path.as_slice()),
                key,
                element.clone(),
                None,
                None,
                &GROVE_V5,
            )
            .unwrap()
            .expect("an element of the shape is inserted");
        }
        db
    }
}

/// One write: the change of one key of one Merk.
#[derive(Clone)]
struct Write {
    path: Path,
    changes: Vec<(Vec<u8>, Change)>,
}

fn put(path: &[Vec<u8>], key: Vec<u8>, element: Element) -> Write {
    Write {
        path: path.to_vec(),
        changes: vec![(key, Change::Put(element))],
    }
}

/// An element with flags for a Merk of `tree_type`.
fn with_flags(tree_type: TreeType) -> Element {
    let mut element = element(tree_type, 2);
    element.set_flags(Some(vec![3; 5]));
    element
}

/// The key every leaf of the order tests holds an element with flags
/// under.
const WITH_FLAGS: u8 = 100;

fn delete_owned_by(path: &[Vec<u8>], key: Vec<u8>, owner: [u8; 32]) -> Write {
    Write {
        path: path.to_vec(),
        changes: vec![(key, Change::DeleteOwnedBy(owner))],
    }
}

fn delete(path: &[Vec<u8>], key: Vec<u8>) -> Write {
    Write {
        path: path.to_vec(),
        changes: vec![(key, Change::Delete)],
    }
}

/// What grove version 5 charges the write, which carries to the root of the
/// grove.
fn carried_to_the_root(
    db: &TempGroveDb,
    tx: &crate::Transaction<'_, crate::tests::SuiteEngine>,
    write: &Write,
) -> OperationCost {
    let path = refs(&write.path);
    let mut cost = OperationCost::default();
    for (key, change) in &write.changes {
        cost += match change {
            Change::Put(element) => {
                let inserted = db.insert(
                    SubtreePath::from(path.as_slice()),
                    key,
                    element.clone(),
                    None,
                    Some(tx),
                    &GROVE_V5,
                );
                inserted.value.expect("grove version 5 inserts the element");
                inserted.cost
            }
            Change::Delete => {
                let deleted = db.delete(
                    SubtreePath::from(path.as_slice()),
                    key,
                    None,
                    Some(tx),
                    &GROVE_V5,
                );
                deleted.value.expect("grove version 5 deletes the element");
                deleted.cost
            }
            Change::DeleteOwnedBy(owner) => {
                let owned = |bytes: u32| {
                    let mut by_epoch = intmap::IntMap::new();
                    by_epoch.insert(0u16, bytes);
                    SectionedStorageRemoval(std::collections::BTreeMap::from([(*owner, by_epoch)]))
                };
                let deleted = db.delete_with_sectional_storage_function(
                    SubtreePath::from(path.as_slice()),
                    key,
                    None,
                    Some(tx),
                    &mut |_, key_bytes, value_bytes| Ok((owned(key_bytes), owned(value_bytes))),
                    &GROVE_V5,
                );
                deleted.value.expect("grove version 5 deletes the element");
                deleted.cost
            }
        };
    }
    cost
}

/// The read of the host of every Merk on the way from the root Merk to the
/// Merk at `path`, that Merk's among them: what grove version 5 charges to
/// open each of them.
fn host_reads(grove: &Grove<'_>, path: &[Vec<u8>]) -> OperationCost {
    (1..=path.len()).fold(OperationCost::default(), |sum, len| {
        sum + grove.host_read(&path[..len])
    })
}

/// A chain of trees of one kind, `depth` deep, each beside `siblings` trees
/// of the same kind, the last holding `elements` elements.
fn chain(tree: &Element, depth: usize, siblings: u8, elements: u8) -> (Shape, Path) {
    let mut shape = Shape::default();
    let mut at = Path::new();
    for level in 0..depth {
        for sibling in 0..siblings {
            shape.filled(child(&at, &[b's', sibling]), tree, 1, 1);
        }
        let below = child(&at, &[b'm', level as u8]);
        if level + 1 == depth {
            shape.filled(below.clone(), tree, elements, 2);
        } else {
            shape.tree(below.clone(), tree);
        }
        at = below;
    }
    (shape, at)
}

#[test]
fn sp1_a_write_and_its_finalize_cost_what_grove_version_5_charges_less_the_reads_of_hosts() {
    let mut compared = 0;
    for tree in trees() {
        let tree_type = tree_type_of(&tree);
        for (depth, siblings, elements) in [(1, 0, 0), (2, 1, 1), (3, 6, 9)] {
            let (shape, leaf) = chain(&tree, depth, siblings, elements);
            let writes = [
                // A key the Merk does not have.
                Some(put(&leaf, key(200), element(tree_type, 3))),
                // A key it has, with a longer and with a shorter value.
                (elements > 0).then(|| put(&leaf, key(0), element(tree_type, 5))),
                (elements > 0).then(|| put(&leaf, key(0), element(tree_type, 1))),
                // An element with flags, under a new key and under one the
                // Merk has.
                Some(put(&leaf, key(200), with_flags(tree_type))),
                (elements > 0).then(|| put(&leaf, key(0), with_flags(tree_type))),
                // A new tree of the kind, with no element.
                Some(put(&leaf, key(200), tree.clone())),
            ];
            for write in writes.into_iter().flatten() {
                let carried_db = shape.build();
                let carried_tx = carried_db.start_transaction();
                let carried = carried_to_the_root(&carried_db, &carried_tx, &write);

                let db = shape.build();
                let tx = db.start_transaction();
                let mut grove = Grove::new(&db, &tx, &GROVE_V6);
                let hosts = host_reads(&grove, &leaf);
                let stopped = grove.write(&write.path, &write.changes);
                let finalize = grove.finalize();

                assert_eq!(
                    stopped.total() + finalize.clone() + hosts.clone(),
                    carried,
                    "{tree_type}, {depth} deep, {siblings} siblings, {elements} elements: write \
                     {stopped:?}, finalize {finalize:?}, hosts {hosts:?}"
                );
                // The two groves hold the same.
                assert_eq!(
                    db.root_hash(Some(&tx), &GROVE_V6).unwrap().unwrap(),
                    carried_db
                        .root_hash(Some(&carried_tx), &GROVE_V5)
                        .unwrap()
                        .unwrap(),
                    "{tree_type}, {depth} deep"
                );
                compared += 1;
            }
        }
    }
    assert_eq!(compared, 12 * (3 + 6 + 6));
}

#[test]
fn sp2_a_reference_and_its_finalize_cost_what_grove_version_5_charges_less_the_reads_of_hosts() {
    use grovedb_element::reference_path::ReferencePathType;
    // A write that puts a reference opens the Merk of its target as well.
    // Grove version 5 reads the host of that Merk to open it, and the host
    // of every Merk on the way to the Merk it writes.
    let tree = Element::empty_tree();
    let mut compared = 0;
    for (depth, siblings, elements) in [(1, 0, 0), (2, 1, 1), (3, 6, 9)] {
        for target_size in [1usize, 10] {
            let (mut shape, leaf) = chain(&tree, depth, siblings, elements);
            let target_merk = shape.filled(path(&[b"target"]), &tree, 3, target_size);
            let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                target_merk[0].clone(),
                key(1),
            ]));
            let write = put(&leaf, key(200), reference);
            let carried_db = shape.build();
            let carried_tx = carried_db.start_transaction();
            let carried = carried_to_the_root(&carried_db, &carried_tx, &write);

            let db = shape.build();
            let tx = db.start_transaction();
            let mut grove = Grove::new(&db, &tx, &GROVE_V6);
            let hosts = host_reads(&grove, &leaf) + grove.host_read(&target_merk);
            let stopped = grove.write(&write.path, &write.changes);
            let finalize = grove.finalize();
            assert_eq!(
                stopped.total() + finalize.clone() + hosts.clone(),
                carried,
                "{depth} deep, a target of {target_size}: write {stopped:?}, finalize \
                 {finalize:?}, hosts {hosts:?}"
            );
            assert_eq!(
                db.root_hash(Some(&tx), &GROVE_V6).unwrap().unwrap(),
                carried_db
                    .root_hash(Some(&carried_tx), &GROVE_V5)
                    .unwrap()
                    .unwrap(),
                "{depth} deep"
            );
            compared += 1;
        }
    }
    assert_eq!(compared, 6);
}

#[test]
fn sp1_a_removal_and_its_finalize_cost_what_grove_version_5_charges_less_the_reads_of_hosts() {
    let mut compared = 0;
    for tree in trees() {
        let tree_type = tree_type_of(&tree);
        for (depth, siblings, elements) in [(1, 0, 2), (2, 1, 3), (3, 6, 9)] {
            let (mut shape, leaf) = chain(&tree, depth, siblings, elements);
            // And an element with flags, which has an owner to ask for.
            shape
                .elements
                .push((leaf.clone(), key(100), with_flags(tree_type)));
            // The first, the middle and the last element of the Merk, and
            // the one with flags; then the first and the one with flags
            // again, with a client that gives the bytes to an owner.
            let plain =
                [0, elements / 2, elements - 1, 100].map(|number| delete(&leaf, key(number)));
            let owned = [0, 100].map(|number| delete_owned_by(&leaf, key(number), [7; 32]));
            for write in plain.into_iter().chain(owned) {
                let number = write.changes[0].0[1];
                // The client is asked for the owner of an element by its
                // flags: without them the bytes are a plain number.
                let by_owner = matches!(write.changes[0].1, Change::DeleteOwnedBy(_));
                let carried_db = shape.build();
                let carried_tx = carried_db.start_transaction();
                let carried = carried_to_the_root(&carried_db, &carried_tx, &write);

                let db = shape.build();
                let tx = db.start_transaction();
                let mut grove = Grove::new(&db, &tx, &GROVE_V6);
                let hosts = host_reads(&grove, &leaf);
                let stopped = grove.write(&write.path, &write.changes);
                let finalize = grove.finalize();

                assert_eq!(
                    stopped.total() + finalize.clone() + hosts.clone(),
                    carried,
                    "{tree_type}, {depth} deep, {siblings} siblings, {elements} elements, the \
                     element {number}: write {stopped:?}, finalize {finalize:?}, hosts {hosts:?}"
                );
                assert_eq!(
                    matches!(
                        carried.storage_cost.removed_bytes,
                        SectionedStorageRemoval(_)
                    ),
                    by_owner && number == 100,
                    "{tree_type}, the element {number}"
                );
                assert_eq!(
                    matches!(carried.storage_cost.removed_bytes, BasicStorageRemoval(_)),
                    !(by_owner && number == 100),
                    "{tree_type}, the element {number}"
                );
                assert_eq!(
                    db.root_hash(Some(&tx), &GROVE_V6).unwrap().unwrap(),
                    carried_db
                        .root_hash(Some(&carried_tx), &GROVE_V5)
                        .unwrap()
                        .unwrap(),
                    "{tree_type}, {depth} deep"
                );
                compared += 1;
            }
        }
    }
    assert_eq!(compared, 12 * 3 * 6);
}

#[test]
fn sp1_a_write_is_charged_nothing_above_its_stop_point() {
    // The same Merk one level deep and eight levels deep, written alike:
    // the write costs the same but for the hash of its longer path, which
    // eight segments of two bytes take in the one block that one does.
    for tree in [Element::empty_tree(), Element::empty_count_sum_tree()] {
        let tree_type = tree_type_of(&tree);
        let write_at = |depth: usize| {
            let (shape, leaf) = chain(&tree, depth, 3, 9);
            let db = shape.build();
            let tx = db.start_transaction();
            let mut grove = Grove::new(&db, &tx, &GROVE_V6);
            let cost = grove.write(&leaf, &[(key(200), Change::Put(element(tree_type, 3)))]);
            (cost, grove.marks().len())
        };
        let (shallow, deep) = (write_at(1), write_at(8));
        assert_eq!(shallow, deep, "{tree_type}");
        // One mark, on the host of the Merk, however deep it is.
        assert_eq!(deep.1, 1, "{tree_type}");
    }
}

#[test]
fn sp2_a_write_opens_a_merk_by_its_path_and_root_and_is_charged_nothing_for_its_host() {
    for tree in trees() {
        let tree_type = tree_type_of(&tree);
        for (depth, elements) in [(1, 0), (3, 4)] {
            let (shape, leaf) = chain(&tree, depth, 2, elements);
            let db = shape.build();
            let tx = db.start_transaction();
            let leaf_refs = refs(&leaf);
            let through_the_host = db
                .open_transactional_merk_at_path(
                    SubtreePath::from(leaf_refs.as_slice()),
                    &tx,
                    None,
                    &GROVE_V5,
                )
                .cost;

            let mut grove = Grove::new(&db, &tx, &GROVE_V6);
            let host = grove.host_read(&leaf);
            let open = grove
                .write(&leaf, &[(key(200), Change::Put(element(tree_type, 2)))])
                .open;

            assert_eq!(
                open.clone() + host.clone(),
                through_the_host,
                "{tree_type}, {depth} deep, {elements} elements"
            );
            assert_eq!(host.seek_count, 1, "{tree_type}");
            assert!(host.storage_loaded_bytes > 0, "{tree_type}");
            // The open is the fetch of the root, and nothing for a Merk with
            // no node.
            assert_eq!(open.seek_count, u32::from(elements > 0), "{tree_type}");
            assert_eq!(open.storage_loaded_bytes == 0, elements == 0, "{tree_type}");
            assert_eq!(open.storage_cost, Default::default(), "{tree_type}");
        }
    }
}

#[test]
fn sp2_the_root_merk_is_opened_by_its_root_key_record_and_its_root() {
    let (shape, _) = chain(&Element::empty_tree(), 1, 3, 2);
    let db = shape.build();
    let tx = db.start_transaction();
    let through_the_record = db
        .open_transactional_merk_at_path(SubtreePath::empty(), &tx, None, &GROVE_V5)
        .cost;
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    assert_eq!(grove.host_read(&[]), OperationCost::default());
    let open = grove
        .write(&[], &[(key(200), Change::Put(Element::new_item(vec![1])))])
        .open;
    assert_eq!(open, through_the_record);
    assert_eq!(open.seek_count, 2);
}

#[test]
fn sp3_a_write_is_charged_the_root_of_its_stop_point_and_nothing_for_the_mark() {
    for tree in trees() {
        let tree_type = tree_type_of(&tree);
        let (shape, leaf) = chain(&tree, 2, 1, 3);
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        let cost = grove.write(&leaf, &[(key(200), Change::Put(element(tree_type, 2)))]);
        assert_eq!(
            cost.roots,
            OperationCost {
                hash_node_calls: 2,
                ..Default::default()
            },
            "{tree_type}"
        );
        assert_eq!(cost.marks, OperationCost::default(), "{tree_type}");
        let (host_key, host_path) = leaf.split_last().expect("the leaf has a host");
        assert_eq!(
            grove.marks().iter().collect::<Vec<_>>(),
            [&(host_path.to_vec(), host_key.clone())],
            "{tree_type}"
        );

        // A Merk that the write leaves with no node has no root to hash.
        let (shape, leaf) = chain(&tree, 2, 1, 1);
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        let cost = grove.write(&leaf, &[(key(0), Change::Delete)]);
        assert_eq!(cost.roots, OperationCost::default(), "{tree_type}");
        assert_eq!(grove.marks().len(), 1, "{tree_type}");
    }
}

#[test]
fn sp3_the_root_merk_has_no_host_and_gets_no_mark() {
    let (shape, _) = chain(&Element::empty_tree(), 1, 3, 2);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let cost = grove.write(&[], &[(key(200), Change::Put(Element::new_item(vec![1])))]);
    assert_eq!(cost.roots, OperationCost::default());
    assert_eq!(cost.marks, OperationCost::default());
    assert!(grove.marks().is_empty());
    assert_eq!(grove.finalize(), OperationCost::default());
    assert!(grove.finalized.is_empty());
}

#[test]
fn sp4_a_write_is_charged_every_fetch_whatever_an_earlier_write_loaded() {
    for tree in trees() {
        let tree_type = tree_type_of(&tree);
        let (shape, leaf) = chain(&tree, 2, 1, 9);
        let first = put(&leaf, key(200), element(tree_type, 2));
        let second = put(&leaf, key(201), element(tree_type, 3));

        // The second write after the first, in one transaction.
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        grove.write(&first.path, &first.changes);
        let after_the_first = grove.write(&second.path, &second.changes);

        // The second write on a grove that was committed with the first in
        // it, where nothing of the first is in memory.
        let mut with_the_first = shape.clone();
        let Change::Put(first_element) = &first.changes[0].1 else {
            unreachable!()
        };
        with_the_first
            .elements
            .push((leaf.clone(), key(200), first_element.clone()));
        let db = with_the_first.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        let alone = grove.write(&second.path, &second.changes);

        assert_eq!(after_the_first, alone, "{tree_type}");
        assert!(alone.open.seek_count == 1 && alone.open.storage_loaded_bytes > 0);
    }
}

/// A grove of every kind of tree: each kind at the root, holding two trees
/// of its kind, which hold elements.
fn every_kind() -> (Shape, Vec<Path>, Vec<Path>) {
    every_kind_holding(3, 2)
}

/// [`every_kind`], with an element that has flags in each of the trees at
/// the bottom: one whose owner a client is asked for when it is removed.
fn every_kind_with_flags() -> (Shape, Vec<Path>, Vec<Path>) {
    let (mut shape, hosts, leaves) = every_kind();
    for leaf in &leaves {
        let tree_type = shape.tree_type(leaf);
        shape
            .elements
            .push((leaf.clone(), key(WITH_FLAGS), with_flags(tree_type)));
    }
    (shape, hosts, leaves)
}

/// [`every_kind`], with `elements` or one more elements of `size` in each
/// of the trees at the bottom.
fn every_kind_holding(elements: u8, size: usize) -> (Shape, Vec<Path>, Vec<Path>) {
    let mut shape = Shape::default();
    let (mut hosts, mut leaves) = (Vec::new(), Vec::new());
    for (number, tree) in trees().iter().enumerate() {
        let host = shape.filled(path(&[&[b't', number as u8]]), tree, 2, 1);
        for below in 0..2u8 {
            leaves.push(shape.filled(child(&host, &[b'c', below]), tree, elements + below, size));
        }
        hosts.push(host);
    }
    (shape, hosts, leaves)
}

#[test]
fn fz1_the_charge_of_a_finalize_is_no_part_of_a_write() {
    let (shape, hosts, leaves) = every_kind();
    let writes: Vec<Write> = leaves
        .iter()
        .chain(&hosts)
        .map(|at| put(at, key(200), element(shape.tree_type(at), 2)))
        .collect();

    // Every write, and one finalize after them.
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let finalized_once: Vec<WriteCost> = writes
        .iter()
        .map(|write| grove.write(&write.path, &write.changes))
        .collect();
    let one_finalize = grove.finalize();

    // The same writes with a finalize after each.
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let mut finalizes = OperationCost::default();
    let finalized_each: Vec<WriteCost> = writes
        .iter()
        .map(|write| {
            let cost = grove.write(&write.path, &write.changes);
            finalizes += grove.finalize();
            cost
        })
        .collect();

    for (number, (once, each)) in finalized_once.iter().zip(&finalized_each).enumerate() {
        let (once, each) = (once.total(), each.total());
        // No seek, no hash and no byte of storage of a finalize is in a
        // write, whether a finalize ran before the write or not, and
        // whether its host was marked or not.
        assert_eq!(once.seek_count, each.seek_count, "write {number}");
        assert_eq!(once.hash_node_calls, each.hash_node_calls, "write {number}");
        assert_eq!(once.storage_cost, each.storage_cost, "write {number}");
        if number < leaves.len() {
            // A Merk that hosts nothing is not written by a finalize, so a
            // write of it reads the same records either way.
            assert_eq!(once, each, "write {number}");
        }
    }
    // The finalizes are what differs, and one costs less than one a write.
    assert!(finalizes.seek_count > one_finalize.seek_count);
    assert!(finalizes.hash_node_calls > one_finalize.hash_node_calls);
    assert!(finalizes.storage_cost.replaced_bytes > one_finalize.storage_cost.replaced_bytes);
}

#[test]
fn fz1_a_write_after_a_finalize_reads_the_records_the_finalize_left() {
    // What FZ1 does not say. A finalize is a write of the Merks that hold
    // marked hosts, and a later write of such a Merk reads what it left:
    // its own fetches, of records as they then are.
    let (shape, leaf) = chain(&Element::empty_tree(), 1, 0, 0);
    let after = |finalize_between: bool| {
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        // The first element of the tree, under a key of 100 bytes: the
        // host will name that key as the root of its Merk.
        grove.write(
            &leaf,
            &[(vec![7; 100], Change::Put(Element::new_item(vec![1])))],
        );
        if finalize_between {
            grove.finalize();
        }
        let root_merk = grove.write(
            &[],
            &[(b"z".to_vec(), Change::Put(Element::new_item(vec![2])))],
        );
        grove.finalize();
        let root_hash = db.root_hash(Some(&tx), &GROVE_V6).unwrap().unwrap();
        (root_merk.total(), root_hash)
    };
    let ((before_the_finalize, root_hash), (after_the_finalize, same_root_hash)) =
        (after(false), after(true));
    assert_eq!(root_hash, same_root_hash);
    // The write of the root Merk fetches the host in both. After the
    // finalize the host names a root key of 100 bytes with its length.
    assert_eq!(
        after_the_finalize.storage_loaded_bytes,
        before_the_finalize.storage_loaded_bytes + 100 + 1
    );
    assert_eq!(
        after_the_finalize.seek_count,
        before_the_finalize.seek_count
    );
    assert_eq!(
        after_the_finalize.hash_node_calls,
        before_the_finalize.hash_node_calls
    );
    assert_eq!(
        after_the_finalize.storage_cost,
        before_the_finalize.storage_cost
    );
}

#[test]
fn fz2_a_host_of_many_changed_merks_is_rewritten_once_deepest_first() {
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let top = shape.tree(path(&[b"top"]), &tree);
    let mid = shape.tree(child(&top, b"mid"), &tree);
    let leaves: Vec<Path> = (0..8u8)
        .map(|number| shape.filled(child(&mid, &[b'l', number]), &tree, 2, 1))
        .collect();
    let other = shape.filled(path(&[b"other"]), &tree, 2, 1);

    let finalize_after = |written: &[&Path]| {
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        for at in written {
            grove.write(
                at,
                &[(key(200), Change::Put(element(TreeType::NormalTree, 2)))],
            );
        }
        let cost = grove.finalize();
        assert!(grove.marks().is_empty());
        (cost, grove.finalized.clone())
    };

    let all: Vec<&Path> = leaves.iter().chain([&other]).collect();
    let (together, finalized) = finalize_after(&all);
    // Each Merk that hosts a marked element once, the deepest first, and
    // among those of one depth by path.
    assert_eq!(finalized, [mid.clone(), top.clone(), Path::new()]);

    // One write under `mid`: the hosts above it are the same three.
    let (one, finalized) = finalize_after(&[&leaves[3]]);
    assert_eq!(finalized, [mid.clone(), top.clone(), Path::new()]);

    // A tree element with no flags costs 74 bytes and its key: 76 for one
    // under `mid`, 77 for `mid` and for `top`, 79 for `other`. `top` is the
    // root of the root Merk and `other` its child, so `top` is rewritten
    // with either.
    let (none_under_mid, _) = finalize_after(&[&other]);
    assert_eq!(none_under_mid.storage_cost.replaced_bytes, 79 + 77);
    // Eight marked elements under `mid` are all its nodes. Each is put
    // once: eight puts, where eight finalizes would put the root of `mid`
    // eight times.
    assert_eq!(
        together.storage_cost.replaced_bytes,
        8 * 76 + 77 + (79 + 77)
    );
    // One marked element under `mid`, with the nodes above it: no more
    // than the four levels of a tree of eight.
    let under_mid = one.storage_cost.replaced_bytes - (77 + 77);
    assert!(under_mid % 76 == 0 && (76..=4 * 76).contains(&under_mid));
    // The seeks of the finalize after the nine writes. In `mid`: the fetch
    // of its root at the open, the fetch of each of its seven other nodes,
    // the read by key of those seven before they are rewritten, and eight
    // puts. In `top`: the fetch of its one node and its put. In the root
    // Merk: its root key record and its root at the open, the fetch and the
    // read by key of `other`, and two puts.
    assert_eq!(together.seek_count, (1 + 7 + 7 + 8) + (1 + 1) + (2 + 2 + 2));
    // Its hash calls. For each of the eleven rewritten elements three: its
    // value, the root below it, and its key with the two. For each changed
    // node but the root of its Merk two, at the commit: seven in `mid`,
    // none in `top`, one in the root Merk. Two for the root of `mid` and
    // two for that of `top`, which are asked for. And the prefix of the
    // paths of `mid` and of `top`, a block each.
    assert_eq!(together.hash_node_calls, 11 * 3 + (7 + 1) * 2 + 2 * 2 + 2);
    assert!(together.seek_count < 8 * one.seek_count);
    assert!(together.hash_node_calls < 8 * one.hash_node_calls);
    for cost in [&together, &one, &none_under_mid] {
        assert_eq!(cost.storage_cost.added_bytes, 0);
        assert_eq!(cost.storage_cost.removed_bytes, NoStorageRemoval);
    }
}

#[test]
fn fz3_a_finalize_opens_no_merk_to_learn_its_root() {
    let finalize_over = |elements: u8, size: usize, hosts_too: bool| {
        let (shape, hosts, leaves) = every_kind_holding(elements, size);
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        for leaf in &leaves {
            let tree_type = shape.tree_type(leaf);
            grove.write(leaf, &[(key(200), Change::Put(element(tree_type, 2)))]);
        }
        if hosts_too {
            for host in &hosts {
                let tree_type = shape.tree_type(host);
                grove.write(host, &[(key(200), Change::Put(element(tree_type, 2)))]);
            }
        }
        let finalize = grove.finalize();
        // The Merks a finalize opened are those that hold a marked host:
        // the twelve hosts, each to rewrite the two elements it hosts, and
        // the root Merk. None of the 24 Merks that only were written is
        // opened, though each is under a marked element.
        let mut expected = hosts.clone();
        expected.push(Path::new());
        assert_eq!(grove.finalized, expected);
        assert!(leaves.iter().all(|leaf| !grove.finalized.contains(leaf)));
        finalize
    };
    // A host that was written itself is under a marked element too. It is
    // opened once all the same, for the elements it hosts.
    finalize_over(3, 2, true);
    // Whatever the marked Merks hold, the finalize seeks, hashes and puts
    // the same: it fetches no node of theirs. The bytes it loads follow the
    // aggregates the hosts held before, which are part of their records.
    let (small, large) = (finalize_over(1, 1, false), finalize_over(40, 6, false));
    assert_eq!(small.seek_count, large.seek_count);
    assert_eq!(small.hash_node_calls, large.hash_node_calls);
    assert_eq!(small.storage_cost, large.storage_cost);
    // Twelve hosts of two marked elements each, and the root Merk: the
    // prefix of each host's path is one hash, and the root Merk's is none.
    // Each of the 36 rewritten elements is three hashes (its value, the
    // root below it, its key). The commit hashes every changed node but
    // the root of its Merk, and each host is asked for its root.
    let nodes = (small.hash_node_calls - 12 - 36 * 3) / 2;
    // In a host: its two marked elements and what is above them. In the
    // root Merk: the twelve hosts and what is above them, but its root.
    assert!(nodes >= 12 * 2 + 11);
    assert_eq!(small.hash_node_calls % 2, 0);
}

#[test]
fn fz4_a_finalize_adds_and_removes_no_bytes_whatever_the_writes_did() {
    let (shape, hosts, leaves) = every_kind();
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let mut written = OperationCost::default();
    // The hosts are written before the Merks they host. The engine keeps
    // the place of a hosted root in the host, which is behind from a write
    // that stops until the finalize, and a write of the host in between
    // needs the pending marks of the transaction to know that: they come
    // with stop points (`transactions.md`, SC9).
    for host in &hosts {
        written += grove.write(host, &[(key(0), Change::Delete)]).total();
    }
    for (number, leaf) in leaves.iter().enumerate() {
        let tree_type = shape.tree_type(leaf);
        // By turns: a new element, a longer one over a shorter, a shorter
        // one over a longer, the removal of one, and the removal of all.
        let changes = match number % 5 {
            0 => vec![(key(200), Change::Put(element(tree_type, 8)))],
            1 => vec![(key(0), Change::Put(element(tree_type, 8)))],
            2 => vec![(key(0), Change::Put(element(tree_type, 1)))],
            3 => vec![(key(1), Change::Delete)],
            _ => (0..shape.count(leaf) as u8)
                .map(|number| (key(number), Change::Delete))
                .collect(),
        };
        // Each change is a write of its own.
        for change in changes {
            written += grove.write(leaf, &[change]).total();
        }
    }
    // The writes added bytes and removed bytes.
    assert!(written.storage_cost.added_bytes > 0);
    assert!(written.storage_cost.removed_bytes.total_removed_bytes() > 0);

    let finalize = grove.finalize();
    assert_eq!(finalize.storage_cost.added_bytes, 0);
    assert_eq!(finalize.storage_cost.removed_bytes, NoStorageRemoval);
    // Every element it rewrote is a tree element, whose cost its tree type
    // fixes: the 24 marked ones and the 12 hosts, and no node above them
    // that is no tree. Each of the 12 kinds of tree holds two of its kind.
    let rewritten: u32 = trees()
        .iter()
        .map(|tree| {
            use grovedb_merk::tree_type::CostSize;
            let tree_type = tree_type_of(tree);
            let class = tree_type.inner_node_type().cost();
            // Under a key of two bytes in a Merk of its kind, and the host
            // under a key of two bytes in the root Merk.
            let hosted = tree_type.cost_size() + class + 35 + (2 + 36 + class);
            let host = tree_type.cost_size() + 35 + (2 + 36);
            2 * hosted + host
        })
        .sum();
    assert!(finalize.storage_cost.replaced_bytes >= rewritten);
    // The root hash is the one grove version 5 gives for the same writes.
    let carried_db = shape.build();
    let carried_tx = carried_db.start_transaction();
    for host in &hosts {
        carried_to_the_root(&carried_db, &carried_tx, &delete(host, key(0)));
    }
    for (number, leaf) in leaves.iter().enumerate() {
        let tree_type = shape.tree_type(leaf);
        let writes = match number % 5 {
            0 => vec![put(leaf, key(200), element(tree_type, 8))],
            1 => vec![put(leaf, key(0), element(tree_type, 8))],
            2 => vec![put(leaf, key(0), element(tree_type, 1))],
            3 => vec![delete(leaf, key(1))],
            _ => (0..shape.count(leaf) as u8)
                .map(|number| delete(leaf, key(number)))
                .collect(),
        };
        for write in &writes {
            carried_to_the_root(&carried_db, &carried_tx, write);
        }
    }
    assert_eq!(
        db.root_hash(Some(&tx), &GROVE_V6).unwrap().unwrap(),
        carried_db
            .root_hash(Some(&carried_tx), &GROVE_V5)
            .unwrap()
            .unwrap()
    );
}

/// Every order of `0..count`.
fn every_order(count: usize) -> Vec<Vec<usize>> {
    if count == 0 {
        return vec![Vec::new()];
    }
    every_order(count - 1)
        .into_iter()
        .flat_map(|order| {
            (0..count).map(move |at| {
                let mut order = order.clone();
                order.insert(at, count - 1);
                order
            })
        })
        .collect()
}

/// `orders` orders of `0..count`, by the numbers of a seed.
fn random_orders(count: usize, orders: usize, seed: u64) -> Vec<Vec<usize>> {
    let mut state = seed;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..orders)
        .map(|_| {
            let mut order: Vec<usize> = (0..count).collect();
            for at in (1..count).rev() {
                order.swap(at, (next() % (at as u64 + 1)) as usize);
            }
            order
        })
        .collect()
}

/// The sum of two costs as an operation of `version` adds them. Where one
/// has removed bytes as a plain number and the other by owner, the sum is
/// made by the arithmetic the grove version selects
/// (`storage_costs.add_basic_storage_removal_to_sectioned_storage_removal`),
/// which a caller that adds costs outside an operation selects itself.
fn added_as_the_grove_version_adds(
    sum: OperationCost,
    cost: OperationCost,
    version: &GroveVersion,
) -> OperationCost {
    grovedb_costs::storage_cost::removal::with_basic_sectioned_removal_addition_version(
        version
            .grovedb_versions
            .storage_costs
            .add_basic_storage_removal_to_sectioned_storage_removal,
        || sum + cost,
    )
}

/// Applies `writes` in each of `orders`, each on the grove of `shape` in a
/// transaction of its own, and a finalize after them. Each write's cost and
/// the cost of the finalize have to be the same in every order, and the
/// grove has to end as grove version 5 leaves it.
fn the_same_in_every_order(
    shape: &Shape,
    writes: &[Write],
    orders: &[Vec<usize>],
    version: &'static GroveVersion,
) {
    let carried_db = shape.build();
    let carried_tx = carried_db.start_transaction();
    for write in writes {
        carried_to_the_root(&carried_db, &carried_tx, write);
    }
    let carried_root = carried_db
        .root_hash(Some(&carried_tx), &GROVE_V5)
        .unwrap()
        .unwrap();

    let mut first: Option<(Vec<WriteCost>, OperationCost, OperationCost)> = None;
    for order in orders {
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, version);
        let mut costs = vec![WriteCost::default(); writes.len()];
        // The total as a transaction adds it up: each charge as its write
        // returns, in the order the writes ran, and the finalize last.
        let mut total = OperationCost::default();
        for &number in order {
            costs[number] = grove.write(&writes[number].path, &writes[number].changes);
            total = added_as_the_grove_version_adds(total, costs[number].total(), version);
        }
        let finalize = grove.finalize();
        total = added_as_the_grove_version_adds(total, finalize.clone(), version);
        assert_eq!(
            db.root_hash(Some(&tx), version).unwrap().unwrap(),
            carried_root,
            "{order:?}"
        );
        match &first {
            None => first = Some((costs, finalize, total)),
            Some((first_costs, first_finalize, first_total)) => {
                for (number, (cost, first_cost)) in costs.iter().zip(first_costs).enumerate() {
                    assert_eq!(cost, first_cost, "write {number} in {order:?}");
                }
                assert_eq!(&finalize, first_finalize, "the finalize after {order:?}");
                assert_eq!(&total, first_total, "the total of {order:?}");
            }
        }
    }
}

/// Disjoint writes on the grove of every kind of tree: each changes another
/// Merk. A host and the Merks it hosts are among them, and the root Merk.
fn disjoint_writes(shape: &Shape, hosts: &[Path], leaves: &[Path]) -> Vec<Write> {
    let mut writes = Vec::new();
    for (number, leaf) in leaves.iter().enumerate() {
        let tree_type = shape.tree_type(leaf);
        writes.push(match number % 4 {
            0 => put(leaf, key(200), element(tree_type, 6)),
            1 => put(leaf, key(0), element(tree_type, 7)),
            2 => put(leaf, key(1), element(tree_type, 1)),
            // A removal whose bytes are a plain number, and one whose bytes
            // the client gives to an owner.
            _ if number % 8 == 3 => delete(leaf, key(0)),
            // The owner is the one a plain number of removed bytes is put
            // under where the two are added, or another. The element has
            // flags: the client is asked for the owner of no other.
            _ => delete_owned_by(
                leaf,
                key(WITH_FLAGS),
                [if number % 16 == 7 { 0 } else { number as u8 }; 32],
            ),
        });
    }
    for (number, host) in hosts.iter().enumerate() {
        let tree_type = shape.tree_type(host);
        writes.push(match number % 3 {
            // A new element beside the trees the host holds, which moves
            // its nodes about.
            0 => put(host, vec![b'a', number as u8], element(tree_type, 4)),
            // A new tree in the host.
            1 => put(host, vec![b'n', number as u8], trees()[number].clone()),
            _ => delete(host, key(1)),
        });
    }
    writes.push(put(
        &[],
        b"at the root".to_vec(),
        Element::new_item(vec![3; 40]),
    ));
    writes
}

#[test]
fn oi1_disjoint_writes_cost_the_same_in_every_order_of_a_small_set() {
    let (shape, hosts, leaves) = every_kind_with_flags();
    let all = disjoint_writes(&shape, &hosts, &leaves);
    // Two Merks under one host, the host itself, a Merk under another host
    // and the root Merk: 120 orders. One of the five removes bytes as a
    // plain number and one gives them to an owner, so the total is one sum
    // only as the grove version adds.
    let picked: Vec<Write> = [2, 3, 25, 7, all.len() - 1]
        .into_iter()
        .map(|number| all[number].clone())
        .collect();
    assert_eq!(picked[2].path, hosts[1]);
    assert!(picked[..2]
        .iter()
        .all(|write| write.path[..1] == hosts[1][..]));
    assert!(picked[4].path.is_empty());
    assert!(matches!(picked[1].changes[0].1, Change::Delete));
    assert!(matches!(
        picked[3].changes[0].1,
        Change::DeleteOwnedBy(owner) if owner == [0; 32]
    ));
    let orders = every_order(picked.len());
    assert_eq!(orders.len(), 120);
    the_same_in_every_order(&shape, &picked, &orders, &GROVE_V6);
}

#[test]
fn oi1_disjoint_writes_cost_the_same_in_random_orders_of_a_large_set() {
    let (shape, hosts, leaves) = every_kind_with_flags();
    let writes = disjoint_writes(&shape, &hosts, &leaves);
    assert_eq!(writes.len(), 24 + 12 + 1);
    let mut paths: Vec<&Path> = writes.iter().map(|write| &write.path).collect();
    paths.sort();
    paths.dedup();
    assert_eq!(paths.len(), writes.len(), "each write changes another Merk");
    let mut orders = random_orders(writes.len(), 30, 0x9E37_79B9_7F4A_7C15);
    // The order they are listed in, and its reverse: every Merk before its
    // host, and every host before the Merks it hosts.
    orders.push((0..writes.len()).collect());
    orders.push((0..writes.len()).rev().collect());
    the_same_in_every_order(&shape, &writes, &orders, &GROVE_V6);
}

#[test]
fn oi1_two_writes_of_one_merk_are_not_disjoint_and_their_costs_follow_their_order() {
    // What the rule does not claim. Two writes of one Merk cannot be open
    // together, and the second is charged from what the first left.
    let tree = Element::empty_tree();
    let (shape, leaf) = chain(&tree, 1, 0, 1);
    let grow = put(&leaf, key(0), Element::new_item(vec![1; 300]));
    let shrink = put(&leaf, key(0), Element::new_item(vec![1; 3]));
    let in_order = |first: &Write, second: &Write| {
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        let first = grove.write(&first.path, &first.changes).total();
        let second = grove.write(&second.path, &second.changes).total();
        (first, second)
    };
    let (grown, shrunk_after) = in_order(&grow, &shrink);
    let (shrunk, grown_after) = in_order(&shrink, &grow);
    assert_ne!(grown, grown_after);
    assert_ne!(shrunk, shrunk_after);
}

/// A count tree `ix/group/bucket` that holds one item, as a batch of grove
/// version 5 makes it, under `ix` as a tree of `top`.
fn two_levels_under(top: Element) -> (TempGroveDb, Path, Path, Path) {
    use crate::batch::QualifiedGroveDbOp as Op;
    let db = make_empty_grovedb();
    db.insert(SubtreePath::empty(), b"ix", top, None, None, &GROVE_V5)
        .unwrap()
        .expect("the tree at the root");
    let ix = path(&[b"ix"]);
    let group = child(&ix, b"group");
    let bucket = child(&group, b"bucket");
    db.apply_batch(
        vec![
            Op::insert_or_replace_op(ix.clone(), b"group".to_vec(), Element::empty_count_tree()),
            Op::insert_or_replace_op(
                group.clone(),
                b"bucket".to_vec(),
                Element::empty_count_tree(),
            ),
            Op::insert_or_replace_op(bucket.clone(), b"one".to_vec(), Element::new_item(vec![1])),
        ],
        None,
        None,
        &GROVE_V5,
    )
    .unwrap()
    .expect("the trees under it");
    (db, ix, group, bucket)
}

#[test]
fn sp5_a_write_under_an_indexed_tree_has_its_stop_point_at_the_primary() {
    let (db, ix, group, bucket) = two_levels_under(Element::empty_provable_count_indexed_tree());
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    // The primary itself, its entry, and a tree under its entry, whose own
    // host is an element of a count tree: all stop at the primary.
    assert_eq!(grove.stop_point(&ix), 1);
    assert_eq!(grove.stop_point(&group), 1);
    assert_eq!(grove.stop_point(&bucket), 1);

    // Under a count tree in the place of the indexed tree, each stops at
    // itself.
    let (db, ix, group, bucket) = two_levels_under(Element::empty_count_tree());
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    assert_eq!(grove.stop_point(&ix), 1);
    assert_eq!(grove.stop_point(&group), 2);
    assert_eq!(grove.stop_point(&bucket), 3);
    assert_eq!(grove.stop_point(&[]), 0);
}

#[test]
#[should_panic(expected = "a write under an indexed tree does not stop at the Merk it changed")]
fn sp5_the_model_stops_no_write_below_an_indexed_tree() {
    let (db, _, _, bucket) = two_levels_under(Element::empty_provable_count_indexed_tree());
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.write(
        &bucket,
        &[(b"two".to_vec(), Change::Put(Element::new_item(vec![1])))],
    );
}

#[test]
fn sp5_a_carry_into_an_entry_of_an_indexed_tree_writes_its_rows() {
    use crate::batch::QualifiedGroveDbOp as Op;
    // The same insert two levels under an entry, on grove version 5, with
    // the tree at the top indexed and not. The write carries to the root in
    // both. Under the indexed tree it also writes the row of the entry in
    // the secondary, which is a Merk of the indexed tree that all its
    // entries share: five seeks, 206 replaced bytes and eight hash calls.
    let cost_under = |top: Element| {
        let (db, _, _, bucket) = two_levels_under(top);
        db.apply_batch(
            vec![Op::insert_or_replace_op(
                bucket,
                b"two".to_vec(),
                Element::new_item(vec![1]),
            )],
            None,
            None,
            &GROVE_V5,
        )
        .cost_as_result()
        .expect("the insert")
    };
    let indexed = cost_under(Element::empty_provable_count_indexed_tree());
    let not_indexed = cost_under(Element::empty_count_tree());
    assert_eq!((indexed.seek_count, not_indexed.seek_count), (19, 14));
    assert_eq!(
        (
            indexed.storage_cost.replaced_bytes,
            not_indexed.storage_cost.replaced_bytes
        ),
        (625, 419)
    );
    assert_eq!(
        (indexed.hash_node_calls, not_indexed.hash_node_calls),
        (34, 26)
    );
    assert_eq!(
        indexed.storage_cost.added_bytes,
        not_indexed.storage_cost.added_bytes
    );
}

/// A grove with a tree `kept` and a tree `gone`, which holds an element and
/// a tree `inner`, after three writes that stopped: one of `kept`, one of
/// `gone` and one of `inner`. So `kept`, `gone` and `inner` are marked, the
/// last of them in the Merk of `gone`.
fn marks_at_and_under_a_host(shape: &mut Shape) -> (Path, Path, Vec<Write>) {
    let tree = Element::empty_tree();
    let kept = shape.filled(path(&[b"kept"]), &tree, 2, 1);
    let gone = shape.filled(path(&[b"gone"]), &tree, 1, 1);
    let inner = shape.filled(child(&gone, b"inner"), &tree, 1, 1);
    let writes = vec![
        put(&kept, key(200), element(TreeType::NormalTree, 1)),
        put(&gone, key(200), element(TreeType::NormalTree, 1)),
        put(&inner, key(200), element(TreeType::NormalTree, 1)),
    ];
    (kept, gone, writes)
}

#[test]
fn sp6_a_write_that_removes_a_host_takes_its_mark_with_it() {
    let mut shape = Shape::default();
    let (_, gone, writes) = marks_at_and_under_a_host(&mut shape);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    for write in &writes {
        grove.write(&write.path, &write.changes);
    }
    assert_eq!(
        grove.marks().iter().collect::<Vec<_>>(),
        [
            &(Path::new(), b"gone".to_vec()),
            &(Path::new(), b"kept".to_vec()),
            &(gone.clone(), b"inner".to_vec()),
        ]
    );

    // A write of the root Merk removes `gone` with what it holds. What
    // that write is charged besides is not the model's to say.
    let for_the_marks = grove.host_removed_by_a_write(&[], b"gone", None);
    // The mark on it and the mark under it are gone, for nothing, and the
    // mark on `kept` stays.
    assert_eq!(for_the_marks, OperationCost::default());
    assert_eq!(
        grove.marks().iter().collect::<Vec<_>>(),
        [&(Path::new(), b"kept".to_vec())]
    );

    // The finalize rewrites `kept` in the root Merk and nothing else.
    let finalize = grove.finalize();
    assert_eq!(grove.finalized, [Path::new()]);
    assert_eq!(finalize.storage_cost.added_bytes, 0);
    assert_eq!(finalize.storage_cost.removed_bytes, NoStorageRemoval);

    // The grove is the one grove version 5 leaves after the same writes.
    let carried_db = shape.build();
    let carried_tx = carried_db.start_transaction();
    for write in &writes {
        carried_to_the_root(&carried_db, &carried_tx, write);
    }
    carried_db
        .delete(
            SubtreePath::empty(),
            b"gone",
            Some(crate::operations::delete::DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            Some(&carried_tx),
            &GROVE_V5,
        )
        .unwrap()
        .expect("grove version 5 deletes a tree with what it holds");
    assert_eq!(
        db.root_hash(Some(&tx), &GROVE_V6).unwrap().unwrap(),
        carried_db
            .root_hash(Some(&carried_tx), &GROVE_V5)
            .unwrap()
            .unwrap()
    );
}

#[test]
fn sp6_a_write_that_puts_an_element_over_a_host_takes_its_mark_with_it() {
    let mut shape = Shape::default();
    let (_, _, writes) = marks_at_and_under_a_host(&mut shape);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    for write in &writes {
        grove.write(&write.path, &write.changes);
    }
    assert_eq!(grove.marks().len(), 3);

    // A write of the root Merk puts an item where the tree was.
    let item = Element::new_item(vec![1]);
    let for_the_marks = grove.host_removed_by_a_write(&[], b"gone", Some(item.clone()));
    assert_eq!(for_the_marks, OperationCost::default());
    assert_eq!(
        grove.marks().iter().collect::<Vec<_>>(),
        [&(Path::new(), b"kept".to_vec())]
    );

    // The finalize rewrites `kept` in the root Merk and nothing else.
    let finalize = grove.finalize();
    assert_eq!(grove.finalized, [Path::new()]);
    assert_eq!(finalize.storage_cost.added_bytes, 0);
    assert_eq!(finalize.storage_cost.removed_bytes, NoStorageRemoval);

    // The grove is the one grove version 5 leaves after the same writes.
    let carried_db = shape.build();
    let carried_tx = carried_db.start_transaction();
    for write in &writes {
        carried_to_the_root(&carried_db, &carried_tx, write);
    }
    carried_db
        .insert(
            SubtreePath::empty(),
            b"gone",
            item,
            Some(crate::operations::insert::InsertOptions {
                validate_insertion_does_not_override_tree: false,
                ..Default::default()
            }),
            Some(&carried_tx),
            &GROVE_V5,
        )
        .unwrap()
        .expect("grove version 5 puts an item over a tree");
    assert_eq!(
        db.root_hash(Some(&tx), &GROVE_V6).unwrap().unwrap(),
        carried_db
            .root_hash(Some(&carried_tx), &GROVE_V5)
            .unwrap()
            .unwrap()
    );
}

#[test]
#[should_panic(expected = "the model's write changes one key")]
fn sp1_the_model_changes_one_key_in_a_write() {
    let (shape, leaf) = chain(&Element::empty_tree(), 1, 0, 3);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.write(&leaf, &[(key(0), Change::Delete), (key(1), Change::Delete)]);
}

#[test]
#[should_panic(expected = "the model does not write a tree that keeps its data flat")]
fn sp1_the_model_writes_no_tree_that_keeps_its_data_flat() {
    // A tree that keeps its data flat has no Merk under it. Its writes are
    // appends, which the model leaves as `stopped` does.
    let mut shape = Shape::default();
    let flat = shape.tree(path(&[b"flat"]), &Element::empty_mmr_tree());
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.write(&flat, &[(key(0), Change::Put(Element::new_item(vec![1])))]);
}

#[test]
#[should_panic(expected = "the model removes an element that is there")]
fn sp1_the_model_removes_an_element_that_is_there() {
    // GroveDB's removal of nothing fails, and a write that fails leaves no
    // mark. The model makes no write that fails.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let leaf = shape.filled(path(&[b"a"]), &tree, 1, 1);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.write(&leaf, &[(key(30), Change::Delete)]);
}

#[test]
#[should_panic(expected = "the model puts no tree that names a root")]
fn sp1_the_model_puts_no_tree_that_names_a_root() {
    // GroveDB's insert of a tree that names a root opens the Merk under it
    // to hold the element to it, through the element it is putting.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let leaf = shape.filled(path(&[b"a"]), &tree, 1, 1);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.write(
        &leaf,
        &[(key(30), Change::Put(Element::new_tree(Some(key(0)))))],
    );
}

#[test]
#[should_panic(expected = "the model's write neither removes a tree nor puts an element over one")]
fn sp6_the_models_write_removes_no_tree() {
    // GroveDB's removal of a tree opens it, frees what it holds and follows
    // the referrers of what it held. The model has a fixture for the marks
    // (SP6) and no write for the rest.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    shape.tree(path(&[b"a"]), &tree);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.write(&[], &[(b"a".to_vec(), Change::Delete)]);
}

#[test]
#[should_panic(expected = "the model keeps no list of referrers")]
fn sp6_the_fixture_removes_no_tree_that_holds_an_element_with_referrers() {
    use grovedb_element::reference_path::ReferencePathType;
    // GroveDB's removal of `sub` removes the reference to what it held, in
    // the root Merk. The fixture takes the element of `sub` out and nothing
    // else, so it does not remove such a tree.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let sub = shape.tree(path(&[b"sub"]), &tree);
    shape.elements.push((
        sub.clone(),
        key(0),
        Element::new_item_allowing_bidirectional_references(vec![1]),
    ));
    shape.elements.push((
        Path::new(),
        b"r".to_vec(),
        Element::new_bidirectional_reference(ReferencePathType::AbsolutePathReference(vec![
            sub[0].clone(),
            key(0),
        ])),
    ));
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.host_removed_by_a_write(&[], b"sub", None);
}

#[test]
fn fz2_a_carried_tree_is_counted_by_a_host_that_counts() {
    use grovedb_merk::tree::AggregateData;
    // A tree is an element of its host's Merk, and a Merk that counts
    // counts it: a tree that keeps its data flat as one element, and an
    // indexed tree by the entries it holds. The finalize puts the element
    // with the feature it has there.
    let flat = [
        Element::empty_mmr_tree(),
        Element::empty_bulk_append_tree(4).expect("a chunk power"),
        Element::empty_dense_tree(4),
        Element::empty_commitment_tree(4).expect("a chunk power"),
        Element::empty_private_document_store(64, 4).expect("a store"),
    ];
    let cases = flat.into_iter().map(|tree| (tree, None, 1)).chain([
        (Element::empty_provable_count_indexed_tree(), None, 0),
        (
            Element::empty_provable_count_indexed_tree(),
            Some(AggregateData::ProvableCount(3)),
            3,
        ),
    ]);
    for (tree, left_with, counted) in cases {
        let mut shape = Shape::default();
        let host = shape.tree(path(&[b"host"]), &Element::empty_count_sum_tree());
        let carried = shape.tree(child(&host, b"carried"), &tree);
        let db = shape.build();
        let tx = db.start_transaction();
        let aggregate = || {
            db.open_transactional_merk_at_path(
                SubtreePath::from(refs(&host).as_slice()),
                &tx,
                None,
                &GROVE_V6,
            )
            .unwrap()
            .expect("the Merk of the host")
            .aggregate_data()
            .expect("the aggregate of the host")
        };
        // What GroveDB's own insert of the tree left the host with.
        let before = aggregate();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        grove.stopped_naming(&carried, left_with.map(|_| key(0)), &[]);
        if let Some(left_with) = left_with {
            grove.stopped_with(&carried, left_with);
        }
        grove.finalize();
        assert_eq!(
            aggregate(),
            AggregateData::CountAndSum(counted, 0),
            "{tree}"
        );
        if left_with.is_none() {
            assert_eq!(aggregate(), before, "{tree}");
        }
    }
}

#[test]
fn fz2_an_mmr_is_carried_at_the_size_of_one_leaf_more() {
    // The size of an MMR counts its nodes. One leaf is one node, and a
    // second makes three: the two leaves and the node above them.
    let mut shape = Shape::default();
    let host = shape.tree(path(&[b"host"]), &Element::empty_tree());
    let flat = shape.tree(child(&host, b"flat"), &Element::empty_mmr_tree());
    let db = shape.build();
    db.mmr_tree_append(
        SubtreePath::from(refs(&host).as_slice()),
        b"flat",
        vec![1; 8],
        None,
        &GROVE_V5,
    )
    .unwrap()
    .expect("a leaf is appended");
    let size = |tx| {
        let element = db
            .get(
                SubtreePath::from(refs(&host).as_slice()),
                b"flat",
                tx,
                &GROVE_V6,
            )
            .unwrap()
            .expect("the host is read");
        match element {
            Element::MmrTree(size, _) => size,
            other => unreachable!("{other} is no MMR"),
        }
    };
    assert_eq!(size(None), 1);
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.stopped(&flat, None);
    grove.finalize();
    assert_eq!(size(Some(&tx)), 3);
}

#[test]
#[should_panic(expected = "the model keeps no list of referrers")]
fn sp1_the_model_keeps_no_list_of_referrers() {
    use grovedb_element::reference_path::ReferencePathType;
    // GroveDB's insert over an item that has a referrer keeps the list and
    // writes the referrer, in another Merk. The model's write is the put
    // alone, so it does not make that write.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let left = shape.tree(path(&[b"left"]), &tree);
    let right = shape.tree(path(&[b"right"]), &tree);
    shape.elements.push((
        left.clone(),
        key(0),
        Element::new_item_allowing_bidirectional_references(vec![1]),
    ));
    shape.elements.push((
        right.clone(),
        key(0),
        Element::new_bidirectional_reference(ReferencePathType::AbsolutePathReference(vec![
            left[0].clone(),
            key(0),
        ])),
    ));
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.write(
        &left,
        &[(
            key(0),
            Change::Put(Element::new_item_allowing_bidirectional_references(vec![2])),
        )],
    );
}

#[test]
fn sp1_two_removals_of_one_merk_free_what_grove_version_5_frees() {
    // Two writes of one Merk, each the removal of an element: both
    // elements are gone, and the two free the bytes that grove version 5
    // frees for the same removals.
    let (shape, leaf) = chain(&Element::empty_tree(), 1, 0, 3);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let freed: u32 = [key(0), key(1)]
        .into_iter()
        .map(|removed| {
            grove
                .write(&leaf, &[(removed, Change::Delete)])
                .total()
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        })
        .sum();
    grove.finalize();
    for (held, there) in [(key(0), false), (key(1), false), (key(2), true)] {
        let found = db
            .get_raw_optional(
                SubtreePath::from(refs(&leaf).as_slice()),
                &held,
                Some(&tx),
                &GROVE_V6,
            )
            .unwrap()
            .expect("the read");
        assert_eq!(found.is_some(), there, "{held:?}");
    }
    let carried_db = shape.build();
    let carried_tx = carried_db.start_transaction();
    let carried: u32 = [key(0), key(1)]
        .into_iter()
        .map(|removed| {
            carried_to_the_root(&carried_db, &carried_tx, &delete(&leaf, removed))
                .storage_cost
                .removed_bytes
                .total_removed_bytes()
        })
        .sum();
    assert_eq!(freed, carried);
    assert_eq!(
        db.root_hash(Some(&tx), &GROVE_V6).unwrap().unwrap(),
        carried_db
            .root_hash(Some(&carried_tx), &GROVE_V5)
            .unwrap()
            .unwrap()
    );
}

#[test]
#[should_panic(expected = "the model does not write the rows of an indexed tree")]
fn sp5_the_model_writes_no_primary_of_an_indexed_tree() {
    let (db, ix, ..) = two_levels_under(Element::empty_provable_count_indexed_tree());
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.write(
        &ix,
        &[(b"other".to_vec(), Change::Put(Element::empty_count_tree()))],
    );
}

#[test]
fn fz2_a_tree_that_keeps_its_data_flat_is_carried_with_its_counts() {
    // A write of such a tree leaves its counts with its root, and the
    // finalize makes its element again from them, as a batch does
    // (`ReplaceNonMerkTreeRoot`). The element costs what it cost. Each tree
    // has flags, which the carry keeps, and is left one entry more than it
    // had: the host is then the element written out beside it here, with
    // what the tree was made with and nothing read back.
    let flags = Some(vec![7; 9]);
    let with_flags = |mut tree: Element| {
        tree.set_flags(flags.clone());
        tree
    };
    let plain = Element::empty_tree();
    for (in_a, tree, expected) in [
        (
            plain.clone(),
            with_flags(Element::empty_mmr_tree()),
            Element::MmrTree(1, flags.clone()),
        ),
        // Wrapped, in a host that counts: the carry keeps the wrapper.
        (
            Element::empty_count_tree(),
            Element::new_non_counted(with_flags(Element::empty_mmr_tree()))
                .expect("a tree can be wrapped"),
            Element::new_non_counted(Element::MmrTree(1, flags.clone()))
                .expect("a tree can be wrapped"),
        ),
        (
            plain.clone(),
            with_flags(Element::empty_bulk_append_tree(4).expect("a chunk power")),
            Element::BulkAppendTree(1, 4, flags.clone()),
        ),
        (
            plain.clone(),
            with_flags(Element::empty_dense_tree(4)),
            Element::DenseAppendOnlyFixedSizeTree(1, 4, flags.clone()),
        ),
        (
            plain.clone(),
            with_flags(Element::empty_commitment_tree(4).expect("a chunk power")),
            Element::CommitmentTree(1, 4, flags.clone()),
        ),
        (
            plain.clone(),
            with_flags(Element::empty_private_document_store(64, 4).expect("a store")),
            Element::PrivateDocumentStore(1, 64, 4, flags.clone()),
        ),
    ] {
        let mut shape = Shape::default();
        let host = shape.tree(path(&[b"host"]), &in_a);
        let flat = shape.tree(child(&host, b"flat"), &tree);
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        grove.stopped(&flat, None);
        let finalize = grove.finalize();
        assert_eq!(finalize.storage_cost.added_bytes, 0, "{tree}");
        assert_eq!(
            finalize.storage_cost.removed_bytes, NoStorageRemoval,
            "{tree}"
        );
        let carried = db
            .get_raw(
                SubtreePath::from(refs(&host).as_slice()),
                b"flat",
                Some(&tx),
                &GROVE_V6,
            )
            .unwrap()
            .expect("the element of the tree");
        assert_eq!(carried, expected, "{tree}");
        // And the host commits to the root the write left: the hash of its
        // value is the hash of that element with that root.
        let committed = db
            .open_transactional_merk_at_path(
                SubtreePath::from(refs(&host).as_slice()),
                &tx,
                None,
                &GROVE_V6,
            )
            .unwrap()
            .expect("the Merk of the host")
            .get_value_hash(
                b"flat",
                false,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                &GROVE_V6,
            )
            .unwrap()
            .expect("the read")
            .expect("the element is there");
        let element = expected
            .serialize(&GROVE_V6)
            .expect("the bytes of the element");
        assert_eq!(
            committed,
            combine_hash(&value_hash(&element).unwrap(), &Grove::LEFT_ROOT).unwrap(),
            "{tree}"
        );
    }
}

#[test]
fn fz2_an_indexed_tree_is_carried_with_the_roots_its_write_left() {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::tree::AggregateData;
    // A write under an indexed tree leaves the root of its primary and of
    // each secondary at its stop point, with their root keys and the
    // aggregate of the primary, and the finalize carries them. The host is
    // then the element those make, and the hash of its value is the hash
    // of that element with the root of the primary and the root of the
    // secondary, or for a tree that names its axes the digest of their
    // roots. The model leaves each axis another root and another root key,
    // so that a carry that takes one axis for another is seen.
    assert_ne!(Grove::left_secondary_root(0), Grove::left_secondary_root(1));
    let root_key = vec![6; 5];
    let (of_the_count, of_the_sum) = (vec![7; 9], vec![8; 10]);
    for (tree, aggregate, expected, roots) in [
        (
            Element::empty_provable_count_indexed_tree(),
            AggregateData::ProvableCount(3),
            Element::ProvableCountIndexedTree(
                Some(root_key.clone()),
                Some(of_the_count.clone()),
                3,
                None,
            ),
            vec![(IndexAxis::Count.tag(), of_the_count.clone())],
        ),
        (
            Element::empty_provable_sum_indexed_tree(),
            AggregateData::ProvableSum(-5),
            Element::ProvableSumIndexedTree(
                Some(root_key.clone()),
                Some(of_the_sum.clone()),
                -5,
                None,
            ),
            vec![(IndexAxis::Sum.tag(), of_the_sum.clone())],
        ),
        (
            Element::empty_provable_count_provable_sum_indexed_tree(vec![(0, None), (1, None)])
                .expect("two axes"),
            AggregateData::ProvableCountAndProvableSum(3, -5),
            Element::ProvableCountProvableSumIndexedTree(
                Some(root_key.clone()),
                3,
                -5,
                vec![
                    (0, Some(of_the_count.clone())),
                    (1, Some(of_the_sum.clone())),
                ],
                None,
            ),
            vec![(0, of_the_count.clone()), (1, of_the_sum.clone())],
        ),
    ] {
        let named = matches!(tree, Element::ProvableCountProvableSumIndexedTree(..));
        let mut shape = Shape::default();
        let host = shape.tree(path(&[b"host"]), &Element::empty_tree());
        let indexed = shape.tree(child(&host, b"indexed"), &tree);
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        let secondaries: Vec<Vec<u8>> = roots.iter().map(|(_, key)| key.clone()).collect();
        grove.stopped_naming(&indexed, Some(root_key.clone()), &secondaries);
        grove.stopped_with(&indexed, aggregate);
        let finalize = grove.finalize();
        assert_eq!(finalize.storage_cost.added_bytes, 0, "{tree}");
        assert_eq!(
            finalize.storage_cost.removed_bytes, NoStorageRemoval,
            "{tree}"
        );
        // The host is the element the write's roots, root keys and
        // aggregate make, and nothing read back from it is in `expected`.
        let carried = db
            .get(
                SubtreePath::from(refs(&host).as_slice()),
                b"indexed",
                Some(&tx),
                &GROVE_V6,
            )
            .unwrap()
            .expect("the host is read");
        assert_eq!(carried, expected, "{tree}");
        let committed = db
            .open_transactional_merk_at_path(
                SubtreePath::from(refs(&host).as_slice()),
                &tx,
                None,
                &GROVE_V6,
            )
            .unwrap()
            .expect("the Merk of the host")
            .get_value_hash(
                b"indexed",
                false,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                &GROVE_V6,
            )
            .unwrap()
            .expect("the read")
            .expect("the element is there");
        let of_the_secondaries = if named {
            let roots: Vec<_> = roots
                .iter()
                .map(|(tag, _)| (*tag, Grove::left_secondary_root(*tag)))
                .collect();
            axes_digest(&roots).unwrap()
        } else {
            Grove::left_secondary_root(roots[0].0)
        };
        assert_eq!(
            committed,
            combine_hash_three(
                &value_hash(
                    &expected
                        .serialize(&GROVE_V6)
                        .expect("the bytes of the element")
                )
                .unwrap(),
                &Grove::LEFT_ROOT,
                &of_the_secondaries
            )
            .unwrap(),
            "{tree}"
        );
    }
}

#[test]
fn oi1_the_total_is_added_by_the_arithmetic_of_the_grove_version() {
    // A removal whose bytes are a plain number, and one whose bytes the
    // client's callback gives to the owner that plain numbers are put
    // under where the two are added, in two Merks.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let plain = shape.filled(path(&[b"a"]), &tree, 2, 1);
    let owned = shape.filled(path(&[b"b"]), &tree, 2, 1);
    // The client is asked for the owner of an element that has flags.
    shape.elements.push((
        owned.clone(),
        key(WITH_FLAGS),
        with_flags(TreeType::NormalTree),
    ));
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let plain = grove.write(&plain, &[(key(0), Change::Delete)]).total();
    let owned = grove
        .write(&owned, &[(key(WITH_FLAGS), Change::DeleteOwnedBy([0; 32]))])
        .total();
    let freed = |cost: &OperationCost| cost.storage_cost.removed_bytes.total_removed_bytes();
    assert!(freed(&plain) > 0 && freed(&owned) > 0);
    assert!(matches!(
        plain.storage_cost.removed_bytes,
        BasicStorageRemoval(_)
    ));
    assert!(matches!(
        owned.storage_cost.removed_bytes,
        SectionedStorageRemoval(_)
    ));

    // Added as grove version 6 adds them, the two orders give one total
    // with every removed byte in it.
    let one_way = added_as_the_grove_version_adds(plain.clone(), owned.clone(), &GROVE_V6);
    let the_other = added_as_the_grove_version_adds(owned.clone(), plain.clone(), &GROVE_V6);
    assert_eq!(one_way, the_other);
    assert_eq!(freed(&one_way), freed(&plain) + freed(&owned));

    // Outside an operation the costs crate adds by its first arithmetic,
    // which grove versions 1 to 3 select. It is no sum: one way it has
    // every removed byte, and the other way none.
    let mut first = plain.clone();
    first += owned.clone();
    let mut second = owned.clone();
    second += plain.clone();
    assert_eq!(freed(&first), 0);
    assert_eq!(freed(&second), freed(&plain) + freed(&owned));
}

#[test]
fn sp2_the_merk_of_a_references_target_is_opened_by_its_own_path_and_root() {
    use grovedb_element::reference_path::ReferencePathType;
    // A write of `r` puts a reference to an element of `t`. It is charged
    // the prefix of the path of `t`, the fetch of its root, the read of the
    // target and the hashes, and nothing for the host of `t`: what it is
    // charged does not follow what the host of `t` names.
    let tree = Element::empty_tree();
    let costs: Vec<OperationCost> = [1usize, 100]
        .into_iter()
        .map(|root_key| {
            let mut shape = Shape::default();
            let t = shape.tree(path(&[b"t"]), &tree);
            let r = shape.tree(path(&[b"r"]), &tree);
            // The one element of `t`, so its key is the root key that the
            // host of `t` names.
            shape
                .elements
                .push((t.clone(), vec![7; root_key], Element::new_item(vec![1; 30])));
            let db = shape.build();
            let tx = db.start_transaction();
            let mut grove = Grove::new(&db, &tx, &GROVE_V6);
            let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                t[0].clone(),
                vec![7; root_key],
            ]));
            // References of one length, whatever the key they name.
            let mut cost = grove.write(&r, &[(key(0), Change::Put(reference))]).total();
            cost.storage_cost = Default::default();
            cost
        })
        .collect();
    // The host of `t` is 99 bytes longer in the second grove, and the write
    // seeks and loads the same in both: the put, the fetch of the root of
    // `t` and the read of the target.
    assert_eq!(costs[0].seek_count, 3);
    assert_eq!(costs[1].seek_count, costs[0].seek_count);
    assert_eq!(costs[1].storage_loaded_bytes, costs[0].storage_loaded_bytes);
}

#[test]
#[should_panic(expected = "the model reads no target through a marked host")]
fn sp2_the_model_reads_no_target_of_a_reference_under_a_marked_host() {
    use grovedb_element::reference_path::ReferencePathType;
    // The Merk of a target is opened by its root as the directory has it.
    // GroveDB's direct insert reads it through its host, which is behind
    // while it is marked, so the model does not make that write.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let t = shape.tree(path(&[b"t"]), &tree);
    let r = shape.tree(path(&[b"r"]), &tree);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.write(&t, &[(key(0), Change::Put(Element::new_item(vec![1; 30])))]);
    let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
        t[0].clone(),
        key(0),
    ]));
    grove.write(&r, &[(key(0), Change::Put(reference))]);
}

#[test]
#[should_panic(expected = "the model reads no target in the Merk it writes")]
fn sp2_the_model_reads_no_target_of_a_reference_in_the_merk_it_writes() {
    use grovedb_element::reference_path::ReferencePathType;
    // GroveDB's direct insert opens the Merk a second time to read such a
    // target, and a batch reads it in the Merk it has open. Which of the two
    // a write does is not the model's to say, so it does not make that write.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let r = shape.filled(path(&[b"r"]), &tree, 3, 1);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
        r[0].clone(),
        key(1),
    ]));
    grove.write(&r, &[(key(9), Change::Put(reference))]);
}

#[test]
#[should_panic(expected = "the model follows one hop of a reference")]
fn sp2_the_model_follows_one_hop_of_a_reference() {
    use grovedb_element::reference_path::ReferencePathType;
    // A reference to a reference is read in the Merk of each, and the model
    // takes out the read of one host.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let t = shape.filled(path(&[b"t"]), &tree, 1, 1);
    let u = shape.tree(path(&[b"u"]), &tree);
    let r = shape.tree(path(&[b"r"]), &tree);
    shape.elements.push((
        u.clone(),
        key(0),
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            t[0].clone(),
            key(0),
        ])),
    ));
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
        u[0].clone(),
        key(0),
    ]));
    grove.write(&r, &[(key(0), Change::Put(reference))]);
}

#[test]
#[should_panic(expected = "the model puts no reference but a plain one")]
fn sp2_the_model_puts_no_reference_but_a_plain_one() {
    use grovedb_element::reference_path::ReferencePathType;
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let t = shape.filled(path(&[b"t"]), &tree, 1, 1);
    let r = shape.tree(path(&[b"r"]), &tree);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let reference =
        Element::new_bidirectional_reference(ReferencePathType::AbsolutePathReference(vec![
            t[0].clone(),
            key(0),
        ]));
    grove.write(&r, &[(key(0), Change::Put(reference))]);
}
