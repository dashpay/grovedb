//! The estimates of the cost rules of stop points and finalize
//! (`docs/lignindb/cost-model.md`, section 16.5), against the costs that
//! the model applies.

use std::collections::HashMap;

use grovedb_costs::{storage_cost::removal::StorageRemovedBytes::NoStorageRemoval, OperationCost};
use grovedb_merk::{
    estimated_costs::{
        average_case_costs::{
            EstimatedLayerCount::ApproximateElements,
            EstimatedLayerInformation,
            EstimatedLayerSizes::{AllItems, AllSubtrees, Mix},
            EstimatedSumTrees,
        },
        worst_case_costs::WorstCaseLayerInformation,
    },
    tree::TreeNode,
    tree_type::{TreeType, SUM_ITEM_COST_SIZE},
};
use grovedb_version::version::{v5::GROVE_V5, v6::GROVE_V6, GroveVersion, GROVE_VERSIONS};

use super::{
    chain, child, element, every_kind, key, model::Grove, path, put, tree_type_of, trees, Path,
    Shape, Write,
};
use crate::{
    batch::{key_info::KeyInfo::KnownKey, KeyInfoPath},
    estimated_costs::marked_hosts,
    Element, Error, GroveDb,
};

fn known(path: &[Vec<u8>]) -> KeyInfoPath {
    KeyInfoPath::from_known_owned_path(path.to_vec())
}

fn open_estimate(path: &[Vec<u8>], empty: bool, version: &GroveVersion) -> OperationCost {
    let mut cost = OperationCost::default();
    GroveDb::add_average_case_get_merk_at_path_engine_free(
        &mut cost,
        &known(path),
        empty,
        TreeType::NormalTree,
        version,
    )
    .expect("the open is estimated");
    cost
}

fn worst_open_estimate(path: &[Vec<u8>], version: &GroveVersion) -> OperationCost {
    let mut cost = OperationCost::default();
    GroveDb::add_worst_case_get_merk_at_path_engine_free(
        &mut cost,
        &known(path),
        TreeType::NormalTree,
        version,
    )
    .expect("the open is estimated");
    cost
}

#[test]
fn es1_the_estimate_of_an_open_has_no_read_of_the_host() {
    let below_root = path(&[b"root", b"child"]);
    // The prefix of a path of two segments of four and five bytes: nine
    // bytes, the count of segments and a byte for each, one block.
    let prefix = 1;
    let host_as_a_node = u64::from(TreeNode::average_case_encoded_tree_size(
        5,
        32,
        TreeType::NormalTree.inner_node_type(),
    ));
    let worst_host_as_a_node = u64::from(TreeNode::worst_case_encoded_tree_size(
        5,
        32,
        TreeType::NormalTree.inner_node_type(),
    ));

    // Every grove version in force keeps the read of the host.
    for version in GROVE_VERSIONS {
        for (empty, seeks) in [(false, 2), (true, 1)] {
            assert_eq!(
                open_estimate(&below_root, empty, version),
                OperationCost {
                    seek_count: seeks,
                    storage_loaded_bytes: host_as_a_node,
                    hash_node_calls: prefix,
                    ..Default::default()
                },
                "grove version {}",
                version.protocol_version
            );
        }
        assert_eq!(
            worst_open_estimate(&below_root, version),
            OperationCost {
                seek_count: 2,
                storage_loaded_bytes: worst_host_as_a_node,
                hash_node_calls: prefix,
                ..Default::default()
            },
            "grove version {}",
            version.protocol_version
        );
    }

    // Grove version 6 has the fetch of the root, and no seek and no byte
    // for the host.
    for (empty, seeks) in [(false, 1), (true, 0)] {
        assert_eq!(
            open_estimate(&below_root, empty, &GROVE_V6),
            OperationCost {
                seek_count: seeks,
                hash_node_calls: prefix,
                ..Default::default()
            }
        );
    }
    assert_eq!(
        worst_open_estimate(&below_root, &GROVE_V6),
        OperationCost {
            seek_count: 1,
            hash_node_calls: prefix,
            ..Default::default()
        }
    );

    // The root Merk has no host: its estimate is the same on both.
    for (empty, seeks) in [(false, 2), (true, 1)] {
        let expected = OperationCost {
            seek_count: seeks,
            ..Default::default()
        };
        assert_eq!(open_estimate(&[], empty, &GROVE_V5), expected);
        assert_eq!(open_estimate(&[], empty, &GROVE_V6), expected);
    }
    assert_eq!(
        worst_open_estimate(&[], &GROVE_V5),
        worst_open_estimate(&[], &GROVE_V6)
    );
    assert_eq!(worst_open_estimate(&[], &GROVE_V6).seek_count, 2);
}

#[test]
fn es1_the_estimate_of_an_open_has_the_seeks_and_the_hashes_of_the_open() {
    for tree in trees() {
        let tree_type = tree_type_of(&tree);
        for depth in [1, 3] {
            for elements in [0, 5] {
                let (shape, leaf) = chain(&tree, depth, 1, elements);
                let db = shape.build();
                let tx = db.start_transaction();
                let mut grove = Grove::new(&db, &tx, &GROVE_V6);
                let open = grove
                    .write(&leaf, &put(&leaf, key(200), element(tree_type, 2)).changes)
                    .open;
                let estimate = open_estimate(&leaf, elements == 0, &GROVE_V6);
                assert_eq!(estimate.seek_count, open.seek_count, "{tree_type}");
                assert_eq!(
                    estimate.hash_node_calls, open.hash_node_calls,
                    "{tree_type}"
                );
                assert_eq!(estimate.storage_cost, open.storage_cost, "{tree_type}");
                let worst = worst_open_estimate(&leaf, &GROVE_V6);
                assert!(worst.seek_count >= open.seek_count, "{tree_type}");
                assert_eq!(worst.hash_node_calls, open.hash_node_calls, "{tree_type}");
            }
        }
    }
}

/// The size a layer gives for elements like `like`: what a node's value is
/// charged by. An item is charged by its bytes, and a sum item by a size
/// that does not follow its sum.
fn size_in_a_layer(like: &Element) -> u32 {
    match like {
        Element::SumItem(..) => SUM_ITEM_COST_SIZE,
        _ => like
            .serialized_size(&GROVE_V6)
            .expect("an element has a size") as u32,
    }
}

/// The layer of a Merk that holds `count` elements like `like`, under keys
/// of two bytes.
fn layer_of_elements(tree_type: TreeType, count: u8, like: &Element) -> EstimatedLayerInformation {
    EstimatedLayerInformation {
        tree_type,
        estimated_layer_count: ApproximateElements(u32::from(count)),
        estimated_layer_sizes: AllItems(2, size_in_a_layer(like), None),
    }
}

/// The two estimates of the write that puts `added` under a new key of the
/// Merk at `leaf`, which holds `elements` elements like `like`: its open
/// and its own level.
fn write_estimates(
    leaf: &[Vec<u8>],
    tree_type: TreeType,
    elements: u8,
    like: &Element,
    added: &Element,
    version: &GroveVersion,
) -> (OperationCost, OperationCost) {
    let layer = layer_of_elements(tree_type, elements, like);
    let mut average = open_estimate(leaf, elements == 0, version);
    average += GroveDb::average_case_merk_insert_element(
        &KnownKey(key(200)),
        added,
        tree_type,
        Some(&layer),
        version,
    )
    .cost_as_result()
    .expect("the write is estimated");
    let mut worst = worst_open_estimate(leaf, version);
    worst += GroveDb::worst_case_merk_insert_element(
        &KnownKey(key(200)),
        added,
        tree_type,
        Some(&WorstCaseLayerInformation::MaxElementsNumber(u32::from(
            elements,
        ))),
        version,
    )
    .cost_as_result()
    .expect("the write is estimated");
    if leaf.is_empty() {
        // The put of the root key record, where the root of the root Merk
        // changes: a batch's estimate adds it at the root Merk.
        average.seek_count += 1;
        worst.seek_count += 1;
    }
    (average, worst)
}

#[test]
fn es2_the_estimates_of_a_write_hold_for_its_own_level_for_every_tree_type() {
    let mut compared = 0;
    for tree in trees() {
        let tree_type = tree_type_of(&tree);
        for depth in [2] {
            for elements in [0u8, 1, 7, 30] {
                for size in [1, 4] {
                    let (shape, leaf) = chain(&tree, depth, 1, elements);
                    let added = element(tree_type, size);
                    let write = put(&leaf, key(200), added.clone());
                    let db = shape.build();
                    let tx = db.start_transaction();
                    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
                    let applied = grove.write(&write.path, &write.changes).total();

                    let (average, worst) = write_estimates(
                        &leaf,
                        tree_type,
                        elements,
                        &element(tree_type, 2),
                        &added,
                        &GROVE_V6,
                    );
                    let case = format!(
                        "{tree_type}, {depth} deep, {elements} elements, size {size}: applied \
                         {applied:?}, average {average:?}, worst {worst:?}"
                    );
                    assert_eq!(
                        average.storage_cost.added_bytes, applied.storage_cost.added_bytes,
                        "{case}"
                    );
                    assert!(average.worse_or_eq_than(&applied), "{case}");
                    assert!(worst.worse_or_eq_than(&applied), "{case}");
                    if elements == 0 {
                        // A write into a Merk with no node is estimated to
                        // the byte and the hash.
                        assert_eq!(average, applied, "{case}");
                    }
                    compared += 1;
                }
            }
        }
    }
    assert_eq!(compared, 12 * 4 * 2);
}

#[test]
fn es2_the_estimate_of_a_write_has_no_level_above_its_stop_point() {
    let tree = Element::empty_tree();
    let added = element(TreeType::NormalTree, 3);
    let like = element(TreeType::NormalTree, 2);
    let estimates_at = |depth: usize, version: &GroveVersion| {
        let (_, leaf) = chain(&tree, depth, 0, 9);
        write_estimates(&leaf, TreeType::NormalTree, 9, &like, &added, version)
    };
    // Eight levels deeper, the estimates of the write are what they are one
    // level deep, but for the prefix of a longer path: eight segments of
    // two bytes take a block as one does.
    assert_eq!(estimates_at(8, &GROVE_V6), estimates_at(1, &GROVE_V6));

    // Grove version 5 estimates the same level with the read of the host,
    // and a batch adds the levels above to it.
    let (average, worst) = estimates_at(1, &GROVE_V6);
    let (carried_average, carried_worst) = estimates_at(1, &GROVE_V5);
    assert!(carried_average.storage_loaded_bytes > average.storage_loaded_bytes);
    assert_eq!(carried_average.storage_cost, average.storage_cost);
    assert_eq!(carried_average.hash_node_calls, average.hash_node_calls);
    // A Merk of nine elements has four levels: the fetches of the three
    // nodes below its root and of the node beside the way, less the read
    // of the host.
    assert_eq!(average.seek_count, carried_average.seek_count + 4 - 1);
    assert!(worst.seek_count > carried_worst.seek_count);
}

/// How a layer describes the trees it holds: by the one word for their
/// kind where they are of one kind, and by the number of each kind
/// otherwise.
fn sum_trees_among(kinds: &[TreeType]) -> EstimatedSumTrees {
    if kinds.iter().all(|kind| *kind == kinds[0]) {
        return sum_trees_of(kinds[0]);
    }
    let of = |kind: TreeType| kinds.iter().filter(|held| **held == kind).count() as u8;
    assert_eq!(
        kinds.len(),
        trees()
            .iter()
            .map(|tree| of(tree_type_of(tree)) as usize)
            .sum::<usize>(),
        "a layer has a word for each kind of tree of these tests"
    );
    EstimatedSumTrees::SomeSumTrees {
        sum_trees_weight: of(TreeType::SumTree),
        big_sum_trees_weight: of(TreeType::BigSumTree),
        count_trees_weight: of(TreeType::CountTree),
        count_sum_trees_weight: of(TreeType::CountSumTree),
        non_sum_trees_weight: of(TreeType::NormalTree),
        provable_sum_trees_weight: of(TreeType::ProvableSumTree),
        provable_count_trees_weight: of(TreeType::ProvableCountTree),
        provable_count_sum_trees_weight: of(TreeType::ProvableCountSumTree),
        provable_count_provable_sum_trees_weight: of(TreeType::ProvableCountProvableSumTree),
        provable_count_provable_big_sum_trees_weight: of(TreeType::ProvableCountProvableBigSumTree),
        count_big_sum_trees_weight: of(TreeType::CountBigSumTree),
        provable_big_sum_trees_weight: of(TreeType::ProvableBigSumTree),
    }
}

fn sum_trees_of(tree_type: TreeType) -> EstimatedSumTrees {
    match tree_type {
        TreeType::NormalTree => EstimatedSumTrees::NoSumTrees,
        TreeType::SumTree => EstimatedSumTrees::AllSumTrees,
        TreeType::BigSumTree => EstimatedSumTrees::AllBigSumTrees,
        TreeType::CountTree => EstimatedSumTrees::AllCountTrees,
        TreeType::CountSumTree => EstimatedSumTrees::AllCountSumTrees,
        TreeType::ProvableCountTree => EstimatedSumTrees::AllProvableCountTrees,
        TreeType::ProvableCountSumTree => EstimatedSumTrees::AllProvableCountSumTrees,
        TreeType::ProvableSumTree => EstimatedSumTrees::AllProvableSumTrees,
        TreeType::ProvableBigSumTree => EstimatedSumTrees::AllProvableBigSumTrees,
        TreeType::ProvableCountProvableSumTree => {
            EstimatedSumTrees::AllProvableCountProvableSumTrees
        }
        TreeType::ProvableCountProvableBigSumTree => {
            EstimatedSumTrees::AllProvableCountProvableBigSumTrees
        }
        TreeType::CountBigSumTree => EstimatedSumTrees::AllCountBigSumTrees,
        other => panic!("{other} is no tree of these tests"),
    }
}

/// The layers of every Merk of `shape`, as its trees and elements are.
fn layers(shape: &Shape) -> HashMap<KeyInfoPath, EstimatedLayerInformation> {
    let mut paths: Vec<Path> = shape.trees.iter().map(|(at, _)| at.clone()).collect();
    paths.push(Path::new());
    paths
        .into_iter()
        .map(|at| {
            let hosted: Vec<&(Path, Element)> = shape
                .trees
                .iter()
                .filter(|(below, _)| below.split_last().is_some_and(|(_, parent)| *parent == at))
                .collect();
            let held: Vec<&(Path, Vec<u8>, Element)> =
                shape.elements.iter().filter(|(of, ..)| *of == at).collect();
            let subtrees = hosted.first().map(|(below, _)| {
                let kinds: Vec<TreeType> =
                    hosted.iter().map(|(_, tree)| tree_type_of(tree)).collect();
                (
                    below.last().expect("a tree has a key").len() as u8,
                    sum_trees_among(&kinds),
                    None,
                )
            });
            let elements = held
                .first()
                .map(|(_, key, element)| (key.len() as u8, size_in_a_layer(element), None));
            let sizes = match (subtrees, elements) {
                (Some((key, sum_trees, flags)), None) => AllSubtrees(key, sum_trees, flags),
                (None, Some((key, size, flags))) => AllItems(key, size, flags),
                (None, None) => AllItems(2, 1, None),
                (Some((tree_key, sum_trees, _)), Some((key, size, _))) => Mix {
                    subtrees_size: Some((tree_key, sum_trees, None, hosted.len() as u8)),
                    items_size: Some((key, size, None, held.len() as u8)),
                    references_size: None,
                    items_with_sum_item_size: None,
                    references_with_sum_item_size: None,
                },
            };
            let layer = EstimatedLayerInformation {
                tree_type: shape.tree_type(&at),
                estimated_layer_count: ApproximateElements(shape.count(&at) as u32),
                estimated_layer_sizes: sizes,
            };
            (known(&at), layer)
        })
        .collect()
}

fn worst_layers(shape: &Shape) -> HashMap<KeyInfoPath, WorstCaseLayerInformation> {
    layers(shape)
        .into_keys()
        .map(|at| {
            let count = shape.count(&at.to_path()) as u32;
            (at, WorstCaseLayerInformation::MaxElementsNumber(count))
        })
        .collect()
}

/// The cost of the finalize after `writes` on the grove of `shape`, as the
/// model applies it, with its two estimates.
fn finalize_and_its_estimates(
    shape: &Shape,
    writes: &[Write],
    version: &'static GroveVersion,
) -> (OperationCost, OperationCost, OperationCost) {
    finalize_after_hosts_removed(shape, writes, &[], version)
}

/// As `finalize_and_its_estimates`, after the trees under the keys of
/// `removed` were removed as well (SP6).
fn finalize_after_hosts_removed(
    shape: &Shape,
    writes: &[Write],
    removed: &[Write],
    version: &'static GroveVersion,
) -> (OperationCost, OperationCost, OperationCost) {
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, version);
    for write in writes {
        grove.write(&write.path, &write.changes);
    }
    for host in removed {
        grove.host_removed_by_a_write(&host.path, &host.changes[0].0, None);
    }
    let all: Vec<Write> = writes.iter().chain(removed).cloned().collect();
    let writes = all.as_slice();
    // The stop points whose marks stand when the finalize begins.
    let stop_points: Vec<KeyInfoPath> = grove
        .marks()
        .iter()
        .map(|(host, key)| known(&child(host, key)))
        .collect();
    let applied = grove.finalize();
    // The layers of the grove as the finalize finds it.
    let found = shape.after(writes);
    let average = GroveDb::average_case_finalize(&stop_points, &layers(&found), version)
        .cost_as_result()
        .expect("the finalize is estimated");
    let worst = GroveDb::worst_case_finalize(&stop_points, &worst_layers(&found), version)
        .cost_as_result()
        .expect("the finalize is estimated");
    (applied, average, worst)
}

/// The bytes that the average case takes one node to load where the nodes
/// are trees of `tree_type` with no flags: in a Merk of that tree type under
/// a key of two bytes, and in the root Merk under a key of four.
///
/// A tree is 37 bytes as a value: 3 of its own, a hash and 2. Its key is
/// stored under a prefix of 32 bytes with its length, and the link to the
/// node is its key with its length, a hash and 4. That is 111 and 115. A
/// tree that keeps aggregates is longer by them, and in a Merk that keeps
/// them a node and the link to it carry them once more each.
fn bytes_of_a_tree_node(tree_type: TreeType) -> (u64, u64) {
    let (in_the_element, in_the_node_and_its_link) = match tree_type {
        TreeType::NormalTree => (0, 0),
        TreeType::SumTree
        | TreeType::ProvableSumTree
        | TreeType::CountTree
        | TreeType::ProvableCountTree => (9, 16),
        TreeType::BigSumTree | TreeType::ProvableBigSumTree => (16, 32),
        TreeType::CountSumTree
        | TreeType::ProvableCountSumTree
        | TreeType::ProvableCountProvableSumTree => (18, 32),
        TreeType::CountBigSumTree | TreeType::ProvableCountProvableBigSumTree => (25, 48),
        other => unreachable!("{other} is no tree a write stops at"),
    };
    (
        111 + in_the_element + in_the_node_and_its_link,
        115 + in_the_element,
    )
}

#[test]
fn es3_the_estimate_of_a_finalize_is_its_seeks_its_bytes_and_its_hashes_in_full_merks() {
    for tree in trees() {
        let tree_type = tree_type_of(&tree);
        // A host of seven trees is a full Merk of three levels: its first,
        // third, fifth and seventh keys are its leaves, two under each
        // child of its root.
        let mut shape = Shape::default();
        let host = shape.tree(path(&[b"host"]), &tree);
        let hosted: Vec<Path> = (0..7u8)
            .map(|number| shape.filled(child(&host, &[b'h', number]), &tree, 2, 1))
            .collect();
        for marked in [vec![0], vec![0, 4], vec![2, 6], vec![0, 2, 4, 6]] {
            let writes: Vec<Write> = marked
                .iter()
                .map(|number| put(&hosted[*number], key(200), element(tree_type, 2)))
                .collect();
            let (applied, average, worst) = finalize_and_its_estimates(&shape, &writes, &GROVE_V6);
            assert_eq!(
                average.seek_count, applied.seek_count,
                "{tree_type} {marked:?}"
            );
            assert_eq!(
                average.storage_cost, applied.storage_cost,
                "{tree_type} {marked:?}"
            );
            assert_eq!(
                average.hash_node_calls, applied.hash_node_calls,
                "{tree_type} {marked:?}"
            );
            // The host is fetched in three levels by the average case: the
            // nodes above the marked elements, the marked elements, and
            // each of them again by its key. The root Merk has one node,
            // and is opened by its root key record, which holds a key of
            // four bytes.
            let above = [0, 2, 3, 3, 3][marked.len()];
            let loads = above + 2 * marked.len() as u64;
            let (in_the_host, in_the_root) = bytes_of_a_tree_node(tree_type);
            assert_eq!(
                average.storage_loaded_bytes,
                loads * in_the_host + in_the_root + 4,
                "{tree_type} {marked:?}"
            );
            assert!(
                average.storage_loaded_bytes >= applied.storage_loaded_bytes,
                "{tree_type} {marked:?}"
            );
            // The worst case takes seven elements to be in four levels,
            // and every read to be of the biggest record. The one node of
            // the root Merk is read twice, and its root key record once, a
            // key at its longest.
            let worst_above = [0, 3, 5, 7, 7][marked.len()];
            let worst_reads = worst_above + 2 * marked.len() as u64 + 2;
            assert_eq!(
                worst.storage_loaded_bytes,
                worst_reads * (65_535 + 256) + 256,
                "{tree_type} {marked:?}"
            );
            assert!(
                worst.worse_or_eq_than(&applied),
                "{tree_type} {marked:?}: worst {worst:?}, applied {applied:?}"
            );
        }
    }
}

/// Groves shaped as the traces of the golden corpus are, each with writes
/// that stop in it.
fn corpus_shapes() -> Vec<(&'static str, Shape, Vec<Write>)> {
    let tree = Element::empty_tree();
    let mut shapes = Vec::new();

    // `balances`: sum items in one sum tree at the root.
    let mut shape = Shape::default();
    let balances = shape.filled(path(&[b"balances"]), &Element::empty_sum_tree(), 40, 3);
    shape.filled(path(&[b"other"]), &tree, 1, 1);
    let writes = vec![put(&balances, key(200), element(TreeType::SumTree, 5))];
    shapes.push(("balances", shape, writes));

    // `documents`: documents of a type under a contract, with an index tree
    // that holds a tree for each value.
    let mut shape = Shape::default();
    let contracts = shape.tree(path(&[b"contracts"]), &tree);
    let contract = shape.tree(child(&contracts, &[7; 32]), &tree);
    let types = shape.tree(child(&contract, &[1]), &tree);
    let of_type = shape.tree(child(&types, b"note"), &tree);
    let documents = shape.filled(child(&of_type, &[0]), &tree, 12, 4);
    let index = shape.tree(child(&of_type, b"owner"), &tree);
    let values: Vec<Path> = (0..9u8)
        .map(|number| shape.filled(child(&index, &[number; 32]), &tree, 2, 1))
        .collect();
    let mut writes = vec![put(&documents, key(200), element(TreeType::NormalTree, 5))];
    writes.extend(
        [1, 4, 8].map(|number| put(&values[number], key(200), element(TreeType::NormalTree, 2))),
    );
    shapes.push(("documents", shape, writes));

    // `tokens`: balances as sum items in a count-sum tree for each token,
    // under a big sum tree.
    let mut shape = Shape::default();
    let tokens = shape.tree(path(&[b"tokens"]), &Element::empty_big_sum_tree());
    let of_token: Vec<Path> = (0..5u8)
        .map(|number| {
            shape.filled(
                child(&tokens, &[number; 32]),
                &Element::empty_count_sum_tree(),
                6,
                2,
            )
        })
        .collect();
    let writes = [0, 3]
        .map(|number| {
            put(
                &of_token[number],
                key(200),
                element(TreeType::CountSumTree, 6),
            )
        })
        .to_vec();
    shapes.push(("tokens", shape, writes));

    // `element-zoo`: every kind of tree, each holding trees of its kind.
    let (shape, hosts, leaves) = every_kind();
    let writes = leaves
        .iter()
        .chain(&hosts)
        .map(|at| put(at, key(200), element(shape.tree_type(at), 3)))
        .collect();
    shapes.push(("element-zoo", shape, writes));

    // `deep-nesting`: a tree in a tree, eight deep.
    let (shape, leaf) = chain(&tree, 8, 0, 3);
    let writes = vec![put(&leaf, key(200), element(TreeType::NormalTree, 2))];
    shapes.push(("deep-nesting", shape, writes));

    // `tiny-subtrees`: many trees of one element in one host, all written.
    let mut shape = Shape::default();
    let host = shape.tree(path(&[b"host"]), &tree);
    let tiny: Vec<Path> = (0..40u8)
        .map(|number| shape.filled(child(&host, &[b'h', number]), &tree, 1, 1))
        .collect();
    let writes = tiny
        .iter()
        .map(|at| put(at, key(200), element(TreeType::NormalTree, 1)))
        .collect();
    shapes.push(("tiny-subtrees", shape, writes));

    // `long-keys`: keys of 255 bytes, which take blocks more to hash.
    let mut shape = Shape::default();
    let host = shape.tree(path(&[&[b'x'; 255]]), &tree);
    let long: Vec<Path> = (0..5u8)
        .map(|number| shape.filled(child(&host, &[number; 255]), &tree, 2, 1))
        .collect();
    let writes = [0, 2, 4]
        .map(|number| put(&long[number], key(200), element(TreeType::NormalTree, 2)))
        .to_vec();
    shapes.push(("long-keys", shape, writes));

    // `wide-sums`: sums that take every byte a varint can.
    let mut shape = Shape::default();
    let sums = shape.tree(path(&[b"sums"]), &Element::empty_sum_tree());
    let wide: Vec<Path> = (0..3u8)
        .map(|number| {
            shape.filled(
                child(&sums, &[b'w', number]),
                &Element::empty_sum_tree(),
                3,
                8,
            )
        })
        .collect();
    let writes = wide
        .iter()
        .map(|at| put(at, key(200), element(TreeType::SumTree, 8)))
        .collect();
    shapes.push(("wide-sums", shape, writes));

    shapes
}

#[test]
fn es2_the_estimates_of_a_write_hold_for_the_shapes_of_the_corpus() {
    for (name, shape, writes) in corpus_shapes() {
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        for (number, write) in writes.iter().enumerate() {
            let applied = grove.write(&write.path, &write.changes).total();
            let (written_key, super::Change::Put(added)) = &write.changes[0] else {
                unreachable!("the writes of the shapes put an element")
            };
            let layer = &layers(&shape)[&known(&write.path)];
            let empty = shape.count(&write.path) == 0;
            let mut average = open_estimate(&write.path, empty, &GROVE_V6);
            average += GroveDb::average_case_merk_insert_element(
                &KnownKey(written_key.clone()),
                added,
                layer.tree_type,
                Some(layer),
                &GROVE_V6,
            )
            .cost_as_result()
            .expect("the write is estimated");
            let mut worst = worst_open_estimate(&write.path, &GROVE_V6);
            worst += GroveDb::worst_case_merk_insert_element(
                &KnownKey(written_key.clone()),
                added,
                layer.tree_type,
                Some(&WorstCaseLayerInformation::MaxElementsNumber(
                    shape.count(&write.path) as u32,
                )),
                &GROVE_V6,
            )
            .cost_as_result()
            .expect("the write is estimated");
            let case = format!(
                "{name}, write {number}: applied {applied:?}, average {average:?}, worst \
                 {worst:?}"
            );
            assert_eq!(
                average.storage_cost.added_bytes, applied.storage_cost.added_bytes,
                "{case}"
            );
            assert!(average.worse_or_eq_than(&applied), "{case}");
            assert!(worst.worse_or_eq_than(&applied), "{case}");
        }
    }
}

#[test]
fn es3_the_estimates_of_a_finalize_hold_for_the_shapes_of_the_corpus() {
    for (name, shape, writes) in corpus_shapes() {
        let (applied, average, worst) = finalize_and_its_estimates(&shape, &writes, &GROVE_V6);
        let case = format!("{name}: applied {applied:?}, average {average:?}, worst {worst:?}");
        assert_eq!(average.storage_cost.added_bytes, 0, "{case}");
        assert_eq!(
            average.storage_cost.removed_bytes, NoStorageRemoval,
            "{case}"
        );
        assert_eq!(applied.storage_cost.added_bytes, 0, "{case}");
        assert_eq!(
            applied.storage_cost.removed_bytes, NoStorageRemoval,
            "{case}"
        );
        assert!(average.worse_or_eq_than(&applied), "{case}");
        assert!(worst.worse_or_eq_than(&applied), "{case}");
        if name == "deep-nesting" {
            // Eight Merks of one element each: every one is full, and the
            // marked element is its only node.
            assert_eq!(average.seek_count, applied.seek_count, "{case}");
            assert_eq!(average.storage_cost, applied.storage_cost, "{case}");
            assert_eq!(average.hash_node_calls, applied.hash_node_calls, "{case}");
            // One node is loaded in each of the eight, a tree under a key
            // of two bytes, and the root key record of the root Merk, which
            // holds such a key; the worst case reads each node twice, and
            // takes the root key at its longest.
            assert_eq!(average.storage_loaded_bytes, 8 * 111 + 2, "{case}");
            assert_eq!(
                worst.storage_loaded_bytes,
                16 * (65_535 + 256) + 256,
                "{case}"
            );
        }
    }
}

#[test]
fn es3_the_marks_of_a_finalize_are_the_hosts_above_its_stop_points_deepest_first() {
    let stop_points = [
        known(&path(&[b"a", b"b", b"c"])),
        known(&path(&[b"a", b"b", b"d"])),
        known(&path(&[b"a", b"e"])),
        known(&path(&[b"f"])),
        // The root Merk has no host.
        known(&[]),
    ];
    let marks: Vec<(Path, Vec<Vec<u8>>)> = marked_hosts(&stop_points)
        .into_iter()
        .map(|(host, keys)| {
            let keys = keys.iter().map(|key| key.as_slice().to_vec()).collect();
            (host.to_path(), keys)
        })
        .collect();
    assert_eq!(
        marks,
        [
            (path(&[b"a", b"b"]), vec![b"c".to_vec(), b"d".to_vec()]),
            (path(&[b"a"]), vec![b"b".to_vec(), b"e".to_vec()]),
            (Path::new(), vec![b"a".to_vec(), b"f".to_vec()]),
        ]
    );
    assert!(marked_hosts(&[known(&[])]).is_empty());
}

#[test]
fn es3_a_grove_version_with_no_finalize_has_no_estimate_of_one() {
    let (shape, leaf) = chain(&Element::empty_tree(), 2, 0, 1);
    let stop_points = [known(&leaf)];
    for version in GROVE_VERSIONS {
        assert!(matches!(
            GroveDb::average_case_finalize(&stop_points, &layers(&shape), version).value,
            Err(Error::NotSupported(_))
        ));
        assert!(matches!(
            GroveDb::worst_case_finalize(&stop_points, &worst_layers(&shape), version).value,
            Err(Error::NotSupported(_))
        ));
    }
    // A Merk above a stop point that the caller gave no layer for.
    let mut missing = layers(&shape);
    missing.remove(&known(&[]));
    assert!(matches!(
        GroveDb::average_case_finalize(&stop_points, &missing, &GROVE_V6).value,
        Err(Error::PathNotFoundInCacheForEstimatedCosts(_))
    ));
}

/// A tree of every tree type GroveDB has: the trees a write can stop at,
/// the trees that keep their data flat, and the indexed trees.
fn every_tree_type() -> Vec<Element> {
    let mut all = trees();
    all.extend([
        Element::empty_mmr_tree(),
        Element::empty_bulk_append_tree(4).expect("a chunk power"),
        Element::empty_dense_tree(4),
        Element::empty_commitment_tree(4).expect("a chunk power"),
        Element::empty_private_document_store(64, 4).expect("a store"),
        Element::empty_provable_sum_indexed_tree(),
        Element::empty_provable_count_indexed_tree(),
        Element::empty_provable_big_sum_indexed_tree(),
        // The count axis and the sum axis of its width.
        Element::empty_provable_count_provable_sum_indexed_tree(vec![(0, None), (1, None)])
            .expect("two axes"),
        Element::empty_provable_count_provable_big_sum_indexed_tree(vec![(0, None), (3, None)])
            .expect("two axes"),
    ]);
    all
}

/// How a layer describes the trees it holds, where it has a word for their
/// tree type.
fn hosted_trees_of(tree_type: TreeType) -> EstimatedSumTrees {
    if tree_type.uses_non_merk_data_storage() || tree_type.is_indexed_primary() {
        EstimatedSumTrees::NoSumTrees
    } else {
        sum_trees_of(tree_type)
    }
}

/// The finalize after a write stopped at the first of `hosted` trees of
/// `tree`, under one host in a normal tree, with its estimates: the
/// finalize as the model applies it, the average case and the worst case.
fn finalize_of_a_stopped_tree(
    tree: &Element,
    hosted: u8,
    version: &'static GroveVersion,
) -> (OperationCost, OperationCost, OperationCost) {
    let tree_type = tree_type_of(tree);
    let mut shape = Shape::default();
    let host = shape.tree(path(&[b"host"]), &Element::empty_tree());
    let trees: Vec<Path> = (0..hosted)
        .map(|number| shape.tree(child(&host, &[b'h', number]), tree))
        .collect();
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, version);
    grove.stopped(&trees[0], None);
    let applied = grove.finalize();

    let mut paths = HashMap::new();
    paths.insert(
        known(&host),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(u32::from(hosted)),
            estimated_layer_sizes: AllSubtrees(2, hosted_trees_of(tree_type), None),
        },
    );
    paths.insert(
        known(&[]),
        EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(1),
            estimated_layer_sizes: AllSubtrees(4, EstimatedSumTrees::NoSumTrees, None),
        },
    );
    paths.insert(
        known(&trees[0]),
        EstimatedLayerInformation {
            tree_type,
            estimated_layer_count: ApproximateElements(0),
            estimated_layer_sizes: AllItems(2, 1, None),
        },
    );
    let stop_points = [known(&trees[0])];
    let average = GroveDb::average_case_finalize(&stop_points, &paths, version)
        .cost_as_result()
        .expect("the finalize is estimated");
    let worst_paths = [
        (
            known(&host),
            WorstCaseLayerInformation::MaxElementsNumber(u32::from(hosted)),
        ),
        (known(&[]), WorstCaseLayerInformation::MaxElementsNumber(1)),
    ]
    .into_iter()
    .collect();
    let worst = GroveDb::worst_case_finalize(&stop_points, &worst_paths, version)
        .cost_as_result()
        .expect("the finalize is estimated");
    (applied, average, worst)
}

#[test]
fn es3_the_estimate_of_a_finalize_is_its_cost_for_a_tree_of_every_tree_type() {
    let mut seen = std::collections::BTreeSet::new();
    for tree in every_tree_type() {
        let tree_type = tree_type_of(&tree);
        seen.insert(format!("{:?}", std::mem::discriminant(&tree_type)));
        // The tree is the only element of its host, so the finalize
        // rewrites the tree's element in the host and the host's element in
        // the root Merk, and nothing above either.
        let (applied, average, worst) = finalize_of_a_stopped_tree(&tree, 1, &GROVE_V6);
        let case = format!("{tree_type}: applied {applied:?}, average {average:?}");
        assert_eq!(applied.storage_cost.added_bytes, 0, "{case}");
        assert_eq!(
            applied.storage_cost.removed_bytes, NoStorageRemoval,
            "{case}"
        );
        assert_eq!(average.seek_count, applied.seek_count, "{case}");
        assert_eq!(average.storage_cost, applied.storage_cost, "{case}");
        // For an indexed tree of several axes the digest of the roots of
        // its axes is among them: two blocks for two axes and for three.
        assert_eq!(average.hash_node_calls, applied.hash_node_calls, "{case}");
        assert!(
            average.storage_loaded_bytes >= applied.storage_loaded_bytes,
            "{case}"
        );
        assert!(
            worst.worse_or_eq_than(&applied),
            "{tree_type}: worst {worst:?}, applied {applied:?}"
        );
    }
    assert_eq!(seen.len(), 22, "every tree type");
}

#[test]
fn es3_a_node_above_a_marked_element_is_sized_by_what_a_layer_can_say_of_it() {
    use grovedb_merk::tree_type::CostSize;
    // The tree is the first of three in its host: a leaf, with the second
    // above it as the root of the host. A layer describes the trees it
    // holds by what their aggregates take, and has a word for a tree that
    // keeps its elements in one Merk and no other. So the node above is
    // sized in full for those, and as a tree with no aggregate for a tree
    // that keeps its data flat and for an indexed tree: short by what the
    // element of such a tree costs over that.
    for tree in every_tree_type() {
        let tree_type = tree_type_of(&tree);
        let (applied, average, worst) = finalize_of_a_stopped_tree(&tree, 3, &GROVE_V6);
        let case = format!("{tree_type}: applied {applied:?}, average {average:?}");
        assert_eq!(average.seek_count, applied.seek_count, "{case}");
        assert_eq!(average.hash_node_calls, applied.hash_node_calls, "{case}");
        assert!(worst.worse_or_eq_than(&applied), "{case}");
        let unnamed = if tree_type.uses_non_merk_data_storage() || tree_type.is_indexed_primary() {
            tree_type.cost_size() - TreeType::NormalTree.cost_size()
        } else {
            0
        };
        assert_eq!(
            applied.storage_cost.replaced_bytes,
            average.storage_cost.replaced_bytes + unnamed,
            "{case}"
        );
    }
}

#[test]
fn es3_a_wrapped_host_costs_a_byte_that_the_estimate_of_a_finalize_does_not_have() {
    // A layer does not say that a tree it holds is wrapped, and a wrapper
    // is a byte of the element's cost.
    let wrapped = Element::new_non_counted(Element::empty_count_tree()).expect("a count tree");
    let bare = Element::empty_count_tree();
    let finalize_of = |inner: &Element| {
        let mut shape = Shape::default();
        let outer = shape.tree(path(&[b"outer"]), &Element::empty_count_tree());
        let inner = shape.tree(child(&outer, b"inner"), inner);
        let write = put(&inner, key(0), Element::new_item(vec![0]));
        finalize_and_its_estimates(&shape, &[write], &GROVE_V6)
    };
    let (applied, average, _) = finalize_of(&bare);
    assert_eq!(average.storage_cost, applied.storage_cost);
    let (applied_wrapped, average_wrapped, worst) = finalize_of(&wrapped);
    assert_eq!(average_wrapped.storage_cost, average.storage_cost);
    assert_eq!(
        applied_wrapped.storage_cost.replaced_bytes,
        average_wrapped.storage_cost.replaced_bytes + 1
    );
    assert!(worst.worse_or_eq_than(&applied_wrapped));
}

#[test]
fn es3_the_estimate_of_a_finalize_does_not_follow_the_order_of_its_stop_points() {
    use crate::batch::key_info::KeyInfo::MaxKeySize;
    // Two stop points whose last keys have the same bytes, one known and
    // one described by its longest length. A map of layers tells them
    // apart, and so does the estimate: two marks in the root Merk, in
    // either order.
    let known_key = known(&path(&[b"a"]));
    let described = KeyInfoPath::from_vec(vec![MaxKeySize {
        unique_id: b"a".to_vec(),
        max_size: 1,
    }]);
    let layer = |tree_type, sizes| EstimatedLayerInformation {
        tree_type,
        estimated_layer_count: ApproximateElements(2),
        estimated_layer_sizes: sizes,
    };
    let mut paths = HashMap::new();
    paths.insert(
        known(&[]),
        layer(
            TreeType::NormalTree,
            AllSubtrees(1, EstimatedSumTrees::NoSumTrees, None),
        ),
    );
    paths.insert(
        known_key.clone(),
        layer(TreeType::NormalTree, AllItems(2, 1, None)),
    );
    paths.insert(
        described.clone(),
        layer(TreeType::SumTree, AllItems(2, 1, None)),
    );
    let estimate = |stop_points: &[KeyInfoPath]| {
        GroveDb::average_case_finalize(stop_points, &paths, &GROVE_V6)
            .cost_as_result()
            .expect("the finalize is estimated")
    };
    let one_way = estimate(&[known_key.clone(), described.clone()]);
    let the_other = estimate(&[described.clone(), known_key.clone()]);
    assert_eq!(one_way, the_other);
    // Both are in it: a tree element and a sum tree element under a key of
    // one byte, 75 bytes and nine more, and the root of the root Merk above
    // the two, sized as the layer says: as a tree element.
    assert_eq!(one_way.storage_cost.replaced_bytes, 75 + 84 + 75);
    // Named twice, a stop point is one mark.
    assert_eq!(
        estimate(&[known_key.clone(), known_key.clone()]),
        estimate(std::slice::from_ref(&known_key))
    );
    let marks = marked_hosts(&[described, known_key]);
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].1.len(), 2);
}

#[test]
fn es3_a_stop_point_far_down_takes_no_more_than_its_path() {
    // Every Merk above a stop point hosts a mark, so a stop point 200
    // levels down has 200 marks. The estimate asks for the layer of each,
    // and refuses at the first it is not given.
    let deep: Path = (0..200u8).map(|level| vec![level]).collect();
    let marks = marked_hosts(&[known(&deep)]);
    assert_eq!(marks.len(), 200);
    assert_eq!(marks[0].0.len(), 199);
    assert!(marks.iter().all(|(_, keys)| keys.len() == 1));
    assert!(matches!(
        GroveDb::average_case_finalize(&[known(&deep)], &HashMap::new(), &GROVE_V6).value,
        Err(Error::PathNotFoundInCacheForEstimatedCosts(_))
    ));
}

/// The keys of an AVL tree of `levels` levels with the fewest nodes such a
/// tree can have, in an order that inserts them with no rotation. One side
/// of every node is a level lower than the other: the left side of the
/// root and of every left child, and the right side of every right child.
/// So the removal of the first key takes a level from the lower side of
/// every node above it, and each of those rebalances by two rotations.
fn leanest_merk(levels: u32) -> Vec<u16> {
    fn nodes(levels: u32) -> u16 {
        match levels {
            0 => 0,
            1 => 1,
            _ => nodes(levels - 2) + nodes(levels - 1) + 1,
        }
    }
    // Each node as its depth and its place in key order.
    fn place(levels: u32, depth: u32, first: u16, lower_left: bool, placed: &mut Vec<(u32, u16)>) {
        if levels == 0 {
            return;
        }
        let (left, right) = if lower_left {
            (levels.saturating_sub(2), levels - 1)
        } else {
            (levels - 1, levels.saturating_sub(2))
        };
        let at = first + nodes(left);
        placed.push((depth, at));
        place(left, depth + 1, first, true, placed);
        place(right, depth + 1, at + 1, false, placed);
    }
    let mut placed = Vec::new();
    place(levels, 0, 0, true, &mut placed);
    placed.sort();
    placed.into_iter().map(|(_, key)| key).collect()
}

#[test]
fn es2_the_worst_case_of_a_level_holds_for_a_removal_that_rebalances_at_every_level() {
    let tree = Element::empty_tree();
    for levels in [1u32, 2, 3, 5, 7, 9] {
        let keys = leanest_merk(levels);
        assert_eq!(
            grovedb_merk::estimated_costs::most_levels(keys.len() as u32),
            levels,
            "{} keys",
            keys.len()
        );
        let mut shape = Shape::default();
        let leaf = shape.tree(path(&[b"a"]), &tree);
        for key in &keys {
            shape.elements.push((
                leaf.clone(),
                key.to_be_bytes().to_vec(),
                Element::new_item(vec![0]),
            ));
        }
        let layer = WorstCaseLayerInformation::MaxElementsNumber(keys.len() as u32);
        // The removal of every key of the smaller Merks, and of the first,
        // the root, and the last of the larger: the first is under the
        // lower side at every level, so its removal rebalances all the way
        // up.
        let removed: Vec<u16> = if keys.len() <= 33 {
            keys.clone()
        } else {
            vec![0, keys[0], keys.len() as u16 - 1]
        };
        let mut most = 0;
        for key in removed {
            let key = key.to_be_bytes().to_vec();
            let db = shape.build();
            let tx = db.start_transaction();
            let mut grove = Grove::new(&db, &tx, &GROVE_V6);
            let applied = grove
                .write(&leaf, &super::delete(&leaf, key.clone()).changes)
                .total();
            let mut worst = worst_open_estimate(&leaf, &GROVE_V6);
            worst += GroveDb::worst_case_merk_delete_element(
                &KnownKey(key.clone()),
                &layer,
                true,
                &GROVE_V6,
            )
            .cost_as_result()
            .expect("the removal is estimated");
            assert!(
                worst.seek_count >= applied.seek_count
                    && worst.hash_node_calls >= applied.hash_node_calls
                    && worst.storage_loaded_bytes >= applied.storage_loaded_bytes
                    && worst.storage_cost.replaced_bytes >= applied.storage_cost.replaced_bytes,
                "{levels} levels, key {key:?}: applied {applied:?}, worst {worst:?}"
            );
            most = most.max(applied.seek_count);
        }
        if levels == 9 {
            // 88 nodes in nine levels. The removal of the first key seeks 80
            // times: it rebalances by two rotations at each of seven levels.
            // The worst case has 94 seeks for it. The estimate of grove
            // version 5 for the same level has 12, with the read of the
            // host among them.
            assert_eq!(most, 80);
            let estimate = |version| {
                let mut worst = worst_open_estimate(&leaf, version);
                worst += GroveDb::worst_case_merk_delete_element(
                    &KnownKey(vec![0, 0]),
                    &layer,
                    true,
                    version,
                )
                .cost_as_result()
                .expect("the removal is estimated");
                worst.seek_count
            };
            assert_eq!(estimate(&GROVE_V6), 1 + 1 + 2 * (5 * 9 + 1));
            assert_eq!(estimate(&GROVE_V5), 2 + 1 + (7 + 2));
        }
    }
}

#[test]
fn es2_the_estimates_of_a_replacement_and_of_a_removal_hold_for_their_own_level() {
    for tree in trees() {
        let tree_type = tree_type_of(&tree);
        for elements in [1u8, 7, 30] {
            let (shape, leaf) = chain(&tree, 2, 1, elements);
            let like = element(tree_type, 2);
            let layer = layer_of_elements(tree_type, elements, &like);
            let worst_layer = WorstCaseLayerInformation::MaxElementsNumber(u32::from(elements));

            // An element of the size that was there.
            let db = shape.build();
            let tx = db.start_transaction();
            let mut grove = Grove::new(&db, &tx, &GROVE_V6);
            let applied = grove
                .write(&leaf, &put(&leaf, key(0), like.clone()).changes)
                .total();
            let mut average = open_estimate(&leaf, false, &GROVE_V6);
            average += GroveDb::average_case_merk_replace_element(
                &KnownKey(key(0)),
                &like,
                tree_type,
                Some(&layer),
                &GROVE_V6,
            )
            .cost_as_result()
            .expect("the replacement is estimated");
            let mut worst = worst_open_estimate(&leaf, &GROVE_V6);
            worst += GroveDb::worst_case_merk_replace_element(
                &KnownKey(key(0)),
                &like,
                tree_type,
                Some(&worst_layer),
                &GROVE_V6,
            )
            .cost_as_result()
            .expect("the replacement is estimated");
            let case = format!(
                "{tree_type}, {elements} elements: applied {applied:?}, average {average:?}, \
                 worst {worst:?}"
            );
            assert_eq!(applied.storage_cost.added_bytes, 0, "{case}");
            assert!(average.worse_or_eq_than(&applied), "{case}");
            assert!(worst.worse_or_eq_than(&applied), "{case}");

            // A longer element. The average case takes a replacement to be
            // by an element of the size that was there, and has no added
            // bytes for it. The worst case has.
            let longer = element(tree_type, 6);
            let db = shape.build();
            let tx = db.start_transaction();
            let mut grove = Grove::new(&db, &tx, &GROVE_V6);
            let applied = grove
                .write(&leaf, &put(&leaf, key(0), longer.clone()).changes)
                .total();
            let average = GroveDb::average_case_merk_replace_element(
                &KnownKey(key(0)),
                &longer,
                tree_type,
                Some(&layer),
                &GROVE_V6,
            )
            .cost_as_result()
            .expect("the replacement is estimated");
            assert_eq!(average.storage_cost.added_bytes, 0, "{tree_type}");
            if !tree_type.is_sum_bearing() {
                // A sum item costs the same whatever its sum.
                assert!(applied.storage_cost.added_bytes > 0, "{tree_type}");
            }

            // The removal of the first, of the middle and of the last.
            for removed in [0, elements / 2, elements - 1] {
                let db = shape.build();
                let tx = db.start_transaction();
                let mut grove = Grove::new(&db, &tx, &GROVE_V6);
                let applied = grove
                    .write(&leaf, &super::delete(&leaf, key(removed)).changes)
                    .total();
                let mut average = open_estimate(&leaf, false, &GROVE_V6);
                average += GroveDb::average_case_merk_delete_element(
                    &KnownKey(key(removed)),
                    &layer,
                    true,
                    &GROVE_V6,
                )
                .cost_as_result()
                .expect("the removal is estimated");
                let mut worst = worst_open_estimate(&leaf, &GROVE_V6);
                worst += GroveDb::worst_case_merk_delete_element(
                    &KnownKey(key(removed)),
                    &worst_layer,
                    true,
                    &GROVE_V6,
                )
                .cost_as_result()
                .expect("the removal is estimated");
                let case = format!(
                    "{tree_type}, {elements} elements, the removal of {removed}: applied \
                     {applied:?}, average {average:?}, worst {worst:?}"
                );
                // An estimate of a removal has no removed bytes: what a
                // removal frees is the caller's to say. The rest holds.
                assert!(applied.storage_cost.removed_bytes.has_removal(), "{case}");
                assert!(!average.storage_cost.removed_bytes.has_removal(), "{case}");
                assert!(!worst.storage_cost.removed_bytes.has_removal(), "{case}");
                assert!(average.seek_count >= applied.seek_count, "{case}");
                assert!(average.hash_node_calls >= applied.hash_node_calls, "{case}");
                assert!(
                    average.storage_cost.replaced_bytes >= applied.storage_cost.replaced_bytes,
                    "{case}"
                );
                assert!(
                    average.storage_loaded_bytes >= applied.storage_loaded_bytes,
                    "{case}"
                );
                assert!(worst.seek_count >= applied.seek_count, "{case}");
                assert!(worst.hash_node_calls >= applied.hash_node_calls, "{case}");
                assert!(
                    worst.storage_cost.replaced_bytes >= applied.storage_cost.replaced_bytes,
                    "{case}"
                );
                assert!(
                    worst.storage_loaded_bytes >= applied.storage_loaded_bytes,
                    "{case}"
                );
            }
        }
    }
}

#[test]
fn es2_a_write_of_the_root_merk_is_estimated_with_the_put_of_its_root_key_record() {
    let added = Element::new_item(vec![1]);
    for elements in [0u8, 1, 6] {
        let mut shape = Shape::default();
        for number in 0..elements {
            shape
                .elements
                .push((Path::new(), key(number), Element::new_item(vec![1])));
        }
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        let applied = grove
            .write(&[], &put(&[], key(200), added.clone()).changes)
            .total();
        let (average, worst) = write_estimates(
            &[],
            TreeType::NormalTree,
            elements,
            &added,
            &added,
            &GROVE_V6,
        );
        let case = format!(
            "{elements} elements: applied {applied:?}, average {average:?}, worst {worst:?}"
        );
        assert_eq!(
            average.storage_cost.added_bytes, applied.storage_cost.added_bytes,
            "{case}"
        );
        assert!(average.worse_or_eq_than(&applied), "{case}");
        assert!(worst.worse_or_eq_than(&applied), "{case}");
        if elements == 0 {
            // The read of the root key record, the put of the element and
            // the put of the root key record. The root of the root Merk is
            // hashed for nobody (SP3), and the estimate has its two hash
            // calls all the same.
            assert_eq!(applied.seek_count, 3, "{case}");
            assert_eq!(average.seek_count, 3, "{case}");
            assert_eq!(
                average.hash_node_calls,
                applied.hash_node_calls + 2,
                "{case}"
            );
        }
    }
}

#[test]
fn es2_the_worst_case_takes_a_value_to_be_no_longer_than_its_biggest() {
    use grovedb_merk::estimated_costs::worst_case_costs::MERK_BIGGEST_VALUE_SIZE;
    // The worst case sizes a node at `MERK_BIGGEST_VALUE_SIZE`, as it does
    // on every grove version. GroveDB takes a longer value, and a Merk that
    // holds one is charged more for it than the worst case has.
    let replaced_beside = |held: usize| {
        let mut shape = Shape::default();
        let leaf = shape.tree(path(&[b"a"]), &Element::empty_tree());
        shape
            .elements
            .push((leaf.clone(), key(0), Element::new_item(vec![1; held])));
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        let added = Element::new_item(vec![1]);
        let applied = grove
            .write(&leaf, &put(&leaf, key(200), added.clone()).changes)
            .total();
        let (_, worst) = write_estimates(&leaf, TreeType::NormalTree, 1, &added, &added, &GROVE_V6);
        (applied, worst)
    };
    let (applied, worst) = replaced_beside(60_000);
    assert!(worst.worse_or_eq_than(&applied));
    // One level: six nodes of the biggest value.
    assert_eq!(
        worst.storage_cost.replaced_bytes,
        6 * MERK_BIGGEST_VALUE_SIZE
    );
    let (applied, worst) = replaced_beside(500_000);
    assert!(applied.storage_cost.replaced_bytes > 500_000);
    assert!(applied.storage_cost.replaced_bytes > worst.storage_cost.replaced_bytes);
}

/// Seeks, loaded bytes, added bytes, replaced bytes and hash calls.
type Figures = (u32, u64, u32, u32, u32);

fn figures(cost: &OperationCost) -> Figures {
    assert_eq!(cost.storage_cost.removed_bytes, NoStorageRemoval);
    assert_eq!(cost.sinsemilla_hash_calls, 0);
    (
        cost.seek_count,
        cost.storage_loaded_bytes,
        cost.storage_cost.added_bytes,
        cost.storage_cost.replaced_bytes,
        cost.hash_node_calls,
    )
}

/// The table of `docs/lignindb/cost-model.md`, section 16.8: for the writes
/// of each shape, what grove version 5 charges them, what grove version 6
/// charges them, and what it charges the finalize after them.
#[test]
fn the_costs_that_move_from_the_writes_to_the_finalize_are_those_of_the_document() {
    let table: [(&str, usize, Figures, Figures, Figures); 8] = [
        (
            "balances",
            1,
            (17, 1056, 166, 877, 20),
            (13, 838, 166, 786, 17),
            (3, 127, 0, 91, 3),
        ),
        (
            "documents",
            4,
            (121, 7804, 789, 4373, 214),
            (24, 1444, 789, 1514, 44),
            (29, 1988, 0, 1132, 58),
        ),
        (
            "tokens",
            2,
            (34, 2666, 364, 1698, 48),
            (14, 700, 364, 882, 22),
            (11, 1083, 0, 564, 18),
        ),
        (
            "element-zoo",
            36,
            (696, 43732, 6837, 27824, 822),
            (228, 12544, 6837, 13856, 372),
            (96, 6578, 0, 3892, 190),
        ),
        (
            "deep-nesting",
            1,
            (30, 1490, 182, 902, 61),
            (5, 296, 182, 294, 9),
            (17, 586, 0, 608, 45),
        ),
        (
            "tiny-subtrees",
            40,
            (725, 42764, 6480, 22108, 966),
            (120, 3600, 6480, 5080, 280),
            (122, 8732, 0, 3118, 204),
        ),
        (
            "long-keys",
            3,
            (49, 8090, 546, 4381, 151),
            (15, 654, 546, 762, 51),
            (16, 2331, 0, 1974, 47),
        ),
        (
            "wide-sums",
            3,
            (42, 2533, 498, 1552, 61),
            (15, 792, 498, 786, 27),
            (11, 635, 0, 390, 19),
        ),
    ];
    let shapes = corpus_shapes();
    assert_eq!(shapes.len(), table.len());
    for ((name, shape, writes), (row, count, carried, stopped, finalize)) in
        shapes.into_iter().zip(table)
    {
        assert_eq!((name, writes.len()), (row, count));
        let carried_db = shape.build();
        let carried_tx = carried_db.start_transaction();
        let mut carried_sum = OperationCost::default();
        for write in &writes {
            carried_sum += super::carried_to_the_root(&carried_db, &carried_tx, write);
        }
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        let mut stopped_sum = OperationCost::default();
        for write in &writes {
            stopped_sum += grove.write(&write.path, &write.changes).total();
        }
        let finalized = grove.finalize();
        assert_eq!(
            figures(&carried_sum),
            carried,
            "{name}, carried to the root"
        );
        assert_eq!(figures(&stopped_sum), stopped, "{name}, stopped");
        assert_eq!(figures(&finalized), finalize, "{name}, the finalize");
        // The bytes the writes add are the same: only a write adds bytes.
        assert_eq!(carried.2, stopped.2, "{name}");
        // The grove is the same after both.
        assert_eq!(
            db.root_hash(Some(&tx), &GROVE_V6).unwrap().unwrap(),
            carried_db
                .root_hash(Some(&carried_tx), &GROVE_V5)
                .unwrap()
                .unwrap(),
            "{name}"
        );
    }
}

/// What the insert of `added` under `key(200)` into the Merk at `leaf` is
/// charged, in a transaction of its own that is given up.
fn applied_insert(
    db: &crate::tests::TempGroveDb,
    leaf: &Path,
    added: &Element,
    version: &'static GroveVersion,
) -> OperationCost {
    let tx = db.start_transaction();
    let mut grove = Grove::new(db, &tx, version);
    grove
        .write(leaf, &put(leaf, key(200), added.clone()).changes)
        .total()
}

/// Lengths on both sides of every place where a count of bytes changes: a
/// block of the hash (64), the length of a length as a cost counts it (128)
/// and as an element is serialized (251).
fn lengths_about_the_edges() -> impl Iterator<Item = usize> {
    (0..=70)
        .chain(118..=134)
        .chain(180..=196)
        .chain(244..=262)
        .chain(308..=324)
}

#[test]
fn es2_the_hash_calls_of_an_insert_are_counted_from_the_bytes_of_the_element() {
    let cases =
        [
            (
                Element::empty_tree(),
                (0..=330usize)
                    .chain(16_370..=16_390)
                    .map(|len| Element::new_item(vec![1; len]))
                    .chain((0..=330).map(|flags| {
                        Element::new_item_with_flags(vec![1; 3], Some(vec![9; flags]))
                    }))
                    .collect::<Vec<_>>(),
            ),
            (
                Element::empty_sum_tree(),
                lengths_about_the_edges()
                    .flat_map(|len| {
                        [
                            Element::new_sum_item_with_flags(7, Some(vec![9; len])),
                            Element::new_sum_item_with_flags(i64::MIN, Some(vec![9; len])),
                            Element::new_item_with_sum_item(vec![1; len], -3),
                            Element::new_item_with_sum_item_with_flags(
                                vec![1; 5],
                                i64::MAX,
                                Some(vec![9; len]),
                            ),
                        ]
                    })
                    .collect::<Vec<_>>(),
            ),
        ];
    let mut compared = 0;
    for (tree, elements) in cases {
        let tree_type = tree_type_of(&tree);
        let (shape, leaf) = chain(&tree, 1, 0, 0);
        let db = shape.build();
        for added in elements {
            let applied = applied_insert(&db, &leaf, &added, &GROVE_V6);
            let (average, worst) = write_estimates(&leaf, tree_type, 0, &added, &added, &GROVE_V6);
            let bytes = added.serialized_size(&GROVE_V6).expect("a size");
            // Into a Merk with no node the estimate is the write, and the
            // worst case has its hash calls.
            assert_eq!(average, applied, "{tree_type}, an element of {bytes} bytes");
            assert_eq!(
                worst.hash_node_calls, applied.hash_node_calls,
                "{tree_type}, an element of {bytes} bytes"
            );
            assert!(worst.worse_or_eq_than(&applied), "{bytes} bytes");
            compared += 1;
        }
    }
    assert_eq!(
        compared,
        331 + 21 + 331 + 4 * lengths_about_the_edges().count()
    );

    // Where the length of the value carries it into one block more, the
    // estimate of grove version 5 is a hash call short for the same level.
    let (shape, leaf) = chain(&Element::empty_tree(), 1, 0, 0);
    let db = shape.build();
    for (item, short) in [
        (60, 0),
        (61, 1),
        (62, 0),
        (124, 0),
        (125, 1),
        (126, 0),
        (187, 0),
        (188, 1),
        (189, 1),
        (190, 0),
    ] {
        let added = Element::new_item(vec![1; item]);
        let applied = applied_insert(&db, &leaf, &added, &GROVE_V6);
        let (earlier, _) =
            write_estimates(&leaf, TreeType::NormalTree, 0, &added, &added, &GROVE_V5);
        assert_eq!(
            applied.hash_node_calls - earlier.hash_node_calls,
            short,
            "an element of {} bytes",
            item + 3
        );
    }
}

/// A tree of every tree type, and the trees that name their axes with one,
/// two and three of them.
fn trees_to_create() -> Vec<Element> {
    let mut all = every_tree_type();
    all.extend([
        Element::empty_provable_count_provable_sum_indexed_tree(vec![(1, None)]).expect("an axis"),
        Element::empty_provable_count_provable_sum_indexed_tree(vec![
            (0, None),
            (1, None),
            (2, None),
        ])
        .expect("three axes"),
        Element::empty_provable_count_provable_big_sum_indexed_tree(vec![(3, None)])
            .expect("an axis"),
        Element::empty_provable_count_provable_big_sum_indexed_tree(vec![
            (0, None),
            (2, None),
            (3, None),
        ])
        .expect("three axes"),
    ]);
    all
}

#[test]
fn es2_the_creation_of_a_tree_of_every_tree_type_is_estimated_to_its_hash_calls() {
    let mut compared = 0;
    for parent in [
        Element::empty_tree(),
        Element::empty_count_tree(),
        Element::empty_sum_tree(),
    ] {
        let parent_type = tree_type_of(&parent);
        let (shape, leaf) = chain(&parent, 1, 0, 0);
        let db = shape.build();
        for tree in trees_to_create() {
            let tree_type = tree_type_of(&tree);
            let flags = [None]
                .into_iter()
                .chain(lengths_about_the_edges().map(Some));
            for flags in flags {
                let mut created = tree.clone();
                created.set_flags(flags.map(|len| vec![9; len]));
                // A count tree holds a tree in a wrapper as well.
                let mut both = vec![created.clone()];
                if parent_type == TreeType::CountTree {
                    both.push(Element::new_non_counted(created).expect("a tree can be wrapped"));
                }
                for created in both {
                    let applied = applied_insert(&db, &leaf, &created, &GROVE_V6);
                    let (average, worst) =
                        write_estimates(&leaf, parent_type, 0, &created, &created, &GROVE_V6);
                    let case = format!(
                        "{tree_type} with flags of {flags:?} in {parent_type}, wrapped: {}",
                        created.is_wrapped()
                    );
                    // Into a Merk with no node the estimate is the write,
                    // and the worst case has its hash calls. A store is
                    // estimated with the check that it is not there yet as
                    // a read that finds its element, and the check finds
                    // nothing: loaded bytes over.
                    let mut but_for_the_check = average.clone();
                    if matches!(tree_type, TreeType::PrivateDocumentStore(_)) {
                        assert!(average.storage_loaded_bytes > applied.storage_loaded_bytes);
                        but_for_the_check.storage_loaded_bytes = applied.storage_loaded_bytes;
                    }
                    assert_eq!(but_for_the_check, applied, "{case}");
                    assert_eq!(worst.hash_node_calls, applied.hash_node_calls, "{case}");
                    assert!(worst.worse_or_eq_than(&applied), "{case}");
                    compared += 1;
                }
            }
        }
    }
    assert_eq!(compared, 4 * 26 * (1 + lengths_about_the_edges().count()));
}

#[test]
fn es2_a_replacement_and_a_patch_count_the_hash_calls_of_an_insert() {
    let key_info = KnownKey(key(0));
    let hashes = |estimate: crate::CostResult<(), Error>| {
        estimate
            .cost_as_result()
            .expect("the estimate")
            .hash_node_calls
    };
    // A Merk of one node, whose element the write replaces.
    let (shape, leaf) = chain(&Element::empty_tree(), 1, 0, 1);
    let db = shape.build();
    let layer = layer_of_elements(TreeType::NormalTree, 1, &element(TreeType::NormalTree, 2));
    let worst_layer = WorstCaseLayerInformation::MaxElementsNumber(1);
    let mut over = Vec::new();
    for len in 0..=330 {
        let item = Element::new_item(vec![1; len]);
        let tree = TreeType::NormalTree;
        let insert = hashes(GroveDb::average_case_merk_insert_element(
            &key_info, &item, tree, None, &GROVE_V6,
        ));
        for same in [
            hashes(GroveDb::worst_case_merk_insert_element(
                &key_info, &item, tree, None, &GROVE_V6,
            )),
            hashes(GroveDb::average_case_merk_replace_element(
                &key_info, &item, tree, None, &GROVE_V6,
            )),
            hashes(GroveDb::worst_case_merk_replace_element(
                &key_info, &item, tree, None, &GROVE_V6,
            )),
            hashes(GroveDb::average_case_merk_patch_element(
                &key_info, &item, 0, tree, None, &GROVE_V6,
            )),
            hashes(GroveDb::worst_case_merk_patch_element(
                &key_info, &item, 0, tree, None, &GROVE_V6,
            )),
        ] {
            assert_eq!(same, insert, "an item of {len} bytes");
        }

        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        let applied = grove
            .write(&leaf, &put(&leaf, key(0), item.clone()).changes)
            .total();
        let mut average = open_estimate(&leaf, false, &GROVE_V6);
        average += GroveDb::average_case_merk_replace_element(
            &key_info,
            &item,
            tree,
            Some(&layer),
            &GROVE_V6,
        )
        .cost_as_result()
        .expect("the replacement is estimated");
        let mut worst = worst_open_estimate(&leaf, &GROVE_V6);
        worst += GroveDb::worst_case_merk_replace_element(
            &key_info,
            &item,
            tree,
            Some(&worst_layer),
            &GROVE_V6,
        )
        .cost_as_result()
        .expect("the replacement is estimated");
        assert!(worst.hash_node_calls >= average.hash_node_calls);
        over.push(average.hash_node_calls - applied.hash_node_calls);
    }
    // The estimate of the level counts the two hash calls of the node
    // twice, with the put and with the way to the root, at every length:
    // nowhere is it short, and nowhere over by more.
    assert_eq!(over, vec![2; 331]);
}

#[test]
fn es2_a_reference_is_estimated_without_the_reads_of_its_target() {
    use grovedb_element::reference_path::ReferencePathType;
    // A write that puts a reference reads its target for the hash it
    // commits to, in the Merk the target is in, which it opens by that
    // Merk's own path and root (SP2). The estimate of one operation has
    // the hash call that combines the two hashes and no read of a target,
    // on any grove version.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let leaf = shape.filled(path(&[b"a"]), &tree, 3, 1);
    let other = shape.filled(path(&[b"b"]), &tree, 3, 1);
    shape
        .elements
        .push((other.clone(), key(9), Element::new_item(vec![1; 200])));
    let far = shape.filled(path(&[&[b'c'; 130]]), &tree, 3, 1);
    let db = shape.build();
    let like = Element::new_item(vec![0xA5; 20]);
    for (reference, path_hash_calls, target_hash_calls) in [
        // The target is an item of 20 bytes in another Merk.
        (
            ReferencePathType::AbsolutePathReference(vec![other[0].clone(), key(1)]),
            1,
            1,
        ),
        // An item of 200 bytes there: its hash takes four blocks.
        (
            ReferencePathType::AbsolutePathReference(vec![other[0].clone(), key(9)]),
            1,
            4,
        ),
        // An item of 20 bytes in a Merk under a key of 130 bytes: the
        // prefix of its path takes three.
        (
            ReferencePathType::AbsolutePathReference(vec![far[0].clone(), key(1)]),
            3,
            1,
        ),
    ] {
        let reference = Element::new_reference(reference);
        let size = reference.serialized_size(&GROVE_V6).expect("a size");
        // An item of as many bytes as the reference.
        let item = Element::new_item(vec![1; size - 3]);
        assert_eq!(item.serialized_size(&GROVE_V6).expect("a size"), size);

        let (of_the_reference, of_the_item) = (
            applied_insert(&db, &leaf, &reference, &GROVE_V6),
            applied_insert(&db, &leaf, &item, &GROVE_V6),
        );
        let (average, worst) =
            write_estimates(&leaf, TreeType::NormalTree, 3, &like, &reference, &GROVE_V6);
        let (average_of_the_item, worst_of_the_item) =
            write_estimates(&leaf, TreeType::NormalTree, 3, &like, &item, &GROVE_V6);
        // The estimate of the reference is that of the item, and the hash
        // call that combines the reference's hash with its target's.
        let mut with_the_combine = average_of_the_item.clone();
        with_the_combine.hash_node_calls += 1;
        assert_eq!(average, with_the_combine);
        assert_eq!(worst.hash_node_calls, worst_of_the_item.hash_node_calls + 1);
        assert!(average_of_the_item.worse_or_eq_than(&of_the_item));
        // The write of the reference stores what the write of the item
        // stores. Besides the combine it reads its target: the prefix of
        // the path of the target's Merk, the fetch of that Merk's root and
        // the read of the target, and the hash of the target's bytes.
        assert_eq!(of_the_reference.storage_cost, of_the_item.storage_cost);
        assert_eq!(of_the_reference.seek_count, of_the_item.seek_count + 2);
        assert_eq!(
            of_the_reference.hash_node_calls,
            of_the_item.hash_node_calls + 1 + path_hash_calls + target_hash_calls
        );
        assert!(of_the_reference.storage_loaded_bytes > of_the_item.storage_loaded_bytes);
    }
}

#[test]
fn es3_the_estimate_of_a_finalize_is_of_the_marks_that_stand_when_it_runs() {
    // A write stops at `gone` and marks it, and a write of the root Merk
    // then removes `gone` (SP6). The finalize has nothing to do there, and
    // its estimate is given the stop points whose marks stand.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let gone = shape.filled(path(&[b"gone"]), &tree, 1, 1);
    let kept = shape.filled(path(&[b"kept"]), &tree, 2, 1);
    let stopped = [super::delete(&gone, key(0))];
    let removed = [super::delete(&[], b"gone".to_vec())];
    let (applied, average, worst) =
        finalize_after_hosts_removed(&shape, &stopped, &removed, &GROVE_V6);
    assert_eq!(applied, OperationCost::default());
    assert_eq!(average, OperationCost::default());
    assert_eq!(worst, OperationCost::default());
    // Given the stop point that was given up, the estimate is of a carry
    // that no finalize makes.
    let given_up = GroveDb::average_case_finalize(&[known(&gone)], &layers(&shape), &GROVE_V6)
        .cost_as_result()
        .expect("the finalize is estimated");
    assert!(given_up.seek_count > 0 && given_up.storage_cost.replaced_bytes > 0);

    let mut with_another = stopped.to_vec();
    with_another.push(put(&kept, key(200), element(TreeType::NormalTree, 1)));
    let (applied, average, worst) =
        finalize_after_hosts_removed(&shape, &with_another, &removed, &GROVE_V6);
    // The finalize rewrites `kept` in a root Merk of one element, and the
    // estimate is of that: the stop point that was given up is no part of
    // it, and its layers are those the finalize finds.
    assert_eq!(average.seek_count, applied.seek_count);
    assert_eq!(average.storage_cost, applied.storage_cost);
    assert_eq!(average.hash_node_calls, applied.hash_node_calls);
    assert_eq!(
        (applied.seek_count, applied.storage_cost.replaced_bytes),
        (3, 78)
    );
    assert!(worst.worse_or_eq_than(&applied));
}

/// The finalize after a write stopped at the tree `t` under `host`, which
/// left `t` naming `root_key`, and for an indexed tree `secondaries`: as
/// the model applies it, and its average and worst estimates, with a layer
/// that says the truth of every Merk.
fn finalize_naming(
    db: &crate::tests::TempGroveDb,
    tree: &Element,
    root_key: &[u8],
    secondaries: &[Vec<u8>],
    hosted_sizes: grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerSizes,
    version: &'static GroveVersion,
) -> (OperationCost, OperationCost, OperationCost) {
    let tree_type = tree_type_of(tree);
    let host = path(&[b"host"]);
    let t = child(&host, b"t");
    let tx = db.start_transaction();
    let mut grove = Grove::new(db, &tx, version);
    grove.stopped_naming(&t, Some(root_key.to_vec()), secondaries);
    let applied = grove.finalize();

    let flags = tree.get_flags().as_ref().map(|flags| flags.len() as u32);
    let layer = |tree_type, count, sizes| EstimatedLayerInformation {
        tree_type,
        estimated_layer_count: ApproximateElements(count),
        estimated_layer_sizes: sizes,
    };
    let paths: HashMap<_, _> = [
        (
            known(&[]),
            layer(
                TreeType::NormalTree,
                1,
                AllSubtrees(4, EstimatedSumTrees::NoSumTrees, None),
            ),
        ),
        (
            known(&host),
            layer(
                TreeType::NormalTree,
                1,
                AllSubtrees(1, hosted_trees_of(tree_type), flags),
            ),
        ),
        (known(&t), layer(tree_type, 1, hosted_sizes)),
    ]
    .into_iter()
    .collect();
    let average = GroveDb::average_case_finalize(&[known(&t)], &paths, version)
        .cost_as_result()
        .expect("the finalize is estimated");
    let worst_paths = [
        (known(&[]), WorstCaseLayerInformation::MaxElementsNumber(1)),
        (
            known(&host),
            WorstCaseLayerInformation::MaxElementsNumber(1),
        ),
    ]
    .into_iter()
    .collect();
    let worst = GroveDb::worst_case_finalize(&[known(&t)], &worst_paths, version)
        .cost_as_result()
        .expect("the finalize is estimated");
    (applied, average, worst)
}

/// A grove with a tree `host` at the root that holds `tree` under `t`.
fn host_of(tree: &Element) -> crate::tests::TempGroveDb {
    let mut shape = Shape::default();
    let host = shape.tree(path(&[b"host"]), &Element::empty_tree());
    shape.tree(child(&host, b"t"), tree);
    shape.build()
}

#[test]
fn es3_the_hash_of_a_carried_element_is_counted_with_the_roots_it_names() {
    // An element is hashed with its length before it, and it is serialized
    // with the length of its flags and of each root key it names in one
    // byte below 251 bytes and three from there.
    let mut compared = 0;
    for flags in [None, Some(1), Some(62), Some(250), Some(251), Some(313)] {
        let tree = Element::empty_tree_with_flags(flags.map(|len| vec![9; len]));
        let db = host_of(&tree);
        for root_key in 1..=255usize {
            let (applied, average, worst) = finalize_naming(
                &db,
                &tree,
                &vec![7; root_key],
                &[],
                AllItems(root_key as u8, 1, None),
                &GROVE_V6,
            );
            let case = format!("flags of {flags:?}, a root key of {root_key} bytes");
            assert_eq!(average.hash_node_calls, applied.hash_node_calls, "{case}");
            assert_eq!(average.seek_count, applied.seek_count, "{case}");
            assert_eq!(average.storage_cost, applied.storage_cost, "{case}");
            assert!(worst.worse_or_eq_than(&applied), "{case}");
            compared += 1;
        }
    }
    assert_eq!(compared, 6 * 255);
}

#[test]
fn es3_the_hash_of_a_carried_indexed_tree_has_the_root_keys_of_its_secondaries() {
    // The secondary of an axis keeps a row for each key of the primary,
    // under the sort key of the axis and that key: 8 bytes for a count or a
    // sum, 16 for an average or a big sum.
    // The last number of each: the most hash calls the estimate is over by.
    for (tree, sort_keys, over) in [
        (Element::empty_provable_count_indexed_tree(), vec![8], 1),
        (Element::empty_provable_sum_indexed_tree(), vec![8], 1),
        (Element::empty_provable_big_sum_indexed_tree(), vec![16], 1),
        (
            Element::empty_provable_count_provable_sum_indexed_tree(vec![
                (0, None),
                (1, None),
                (2, None),
            ])
            .expect("three axes"),
            vec![8, 8, 16],
            1,
        ),
        (
            Element::empty_provable_count_provable_big_sum_indexed_tree(vec![
                (0, None),
                (2, None),
                (3, None),
            ])
            .expect("three axes"),
            vec![8, 16, 16],
            1,
        ),
        // Two axes where three may be named: the estimate has the third.
        (
            Element::empty_provable_count_provable_sum_indexed_tree(vec![(0, None), (1, None)])
                .expect("two axes"),
            vec![8, 8],
            5,
        ),
        // And one: the estimate has the other two.
        (
            Element::empty_provable_count_provable_sum_indexed_tree(vec![(0, None)])
                .expect("an axis"),
            vec![8],
            9,
        ),
        (
            Element::empty_provable_count_provable_big_sum_indexed_tree(vec![(3, None)])
                .expect("an axis"),
            vec![16],
            10,
        ),
    ] {
        let tree_type = tree_type_of(&tree);
        let db = host_of(&tree);
        let mut most_over = 0;
        for root_key in 1..=239usize {
            let secondaries: Vec<Vec<u8>> = sort_keys
                .iter()
                .map(|sort_key| vec![7; sort_key + root_key])
                .collect();
            let (applied, average, worst) = finalize_naming(
                &db,
                &tree,
                &vec![7; root_key],
                &secondaries,
                AllItems(root_key as u8, 1, None),
                &GROVE_V6,
            );
            let case = format!("{tree_type}, a root key of {root_key} bytes");
            assert!(
                average.hash_node_calls >= applied.hash_node_calls,
                "{case}: {} and {}",
                average.hash_node_calls,
                applied.hash_node_calls
            );
            most_over = most_over.max(average.hash_node_calls - applied.hash_node_calls);
            assert_eq!(average.seek_count, applied.seek_count, "{case}");
            assert_eq!(average.storage_cost, applied.storage_cost, "{case}");
            assert!(worst.worse_or_eq_than(&applied), "{case}");
        }
        // The estimate takes the aggregates of the tree at their longest,
        // which is one block over at some lengths. Where an axis may be
        // named and is not, it is over by the root key of that axis, and
        // for one named axis of three by a block of the digest as well.
        assert_eq!(most_over, over, "{tree_type}, {} axes", sort_keys.len());
    }
}

#[test]
fn es3_the_root_key_of_a_merk_is_sized_by_any_kind_of_element_its_layer_names() {
    use grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerSizes::{
        AllItemsWithSumItem, AllReferencesWithSumItem, Mix,
    };
    // A layer that names the keys of its elements in another way names the
    // same keys.
    let tree = Element::empty_sum_tree();
    let db = host_of(&tree);
    let root_key = vec![7; 128];
    let named = |sizes| finalize_naming(&db, &tree, &root_key, &[], sizes, &GROVE_V6).1;
    let all = named(AllItemsWithSumItem(128, 1, None));
    assert_eq!(named(AllReferencesWithSumItem(128, 1, None)), all);
    let only = |items_with_sum_item_size, references_with_sum_item_size| Mix {
        subtrees_size: None,
        items_size: None,
        references_size: None,
        items_with_sum_item_size,
        references_with_sum_item_size,
    };
    assert_eq!(named(only(Some((128, 1, None, 1)), None)), all);
    assert_eq!(named(only(None, Some((128, 1, None, 1)))), all);
    assert_eq!(
        named(only(Some((3, 1, None, 1)), Some((128, 1, None, 1)))),
        all
    );
    // And the hash of the carried element has the three blocks a root key
    // of 128 bytes takes: the finalize is five hash calls there.
    let (applied, average, _) = finalize_naming(
        &db,
        &tree,
        &root_key,
        &[],
        AllItemsWithSumItem(128, 1, None),
        &GROVE_V6,
    );
    assert_eq!(average.hash_node_calls, applied.hash_node_calls);
}

#[test]
fn es3_the_root_merk_is_opened_by_its_root_key_record() {
    // The root Merk has no host: it is opened by its root key record, which
    // holds the key of its root. A finalize that reaches the root Merk
    // loads it, and the estimate has its bytes at the size the layer gives
    // the keys of the root Merk. A tree under a key of 80 bytes, the one
    // element of the root Merk, with one item under a key of 196.
    let mut shape = Shape::default();
    let outer = shape.tree(vec![vec![1; 80]], &Element::empty_tree());
    shape
        .elements
        .push((outer.clone(), vec![2; 196], Element::new_item(vec![1])));
    let write = put(&outer, vec![2; 196], Element::new_item(vec![2]));
    let (applied, average, worst) =
        finalize_and_its_estimates(&shape, std::slice::from_ref(&write), &GROVE_V6);
    assert_eq!(average.seek_count, applied.seek_count);
    assert_eq!(average.storage_cost, applied.storage_cost);
    assert_eq!(average.hash_node_calls, applied.hash_node_calls);
    // The node of the root Merk, and the 80 bytes of the record.
    assert_eq!(
        (applied.storage_loaded_bytes, average.storage_loaded_bytes),
        (267 + 80, 267 + 80)
    );
    assert!(worst.worse_or_eq_than(&applied));
}

#[test]
fn es3_a_sum_that_does_not_fit_leaves_the_total_as_it_was() {
    use grovedb_costs::storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval;

    use crate::estimated_costs::add_in_full;
    // The sum of a finalize estimate is made in full or not at all, in
    // every field of a cost, and a finalize removes no bytes.
    let part = OperationCost {
        seek_count: 1,
        storage_loaded_bytes: 1,
        hash_node_calls: 1,
        sinsemilla_hash_calls: 1,
        ..Default::default()
    };
    let mut fits = OperationCost::default();
    add_in_full(&mut fits, part.clone()).expect("the sum fits");
    assert_eq!(fits, part);
    for field in 0..6 {
        let mut total = OperationCost {
            seek_count: 3,
            ..Default::default()
        };
        let mut added = part.clone();
        match field {
            0 => total.seek_count = u32::MAX,
            1 => total.storage_loaded_bytes = u64::MAX,
            2 => total.hash_node_calls = u32::MAX,
            3 => total.sinsemilla_hash_calls = u32::MAX,
            4 => {
                total.storage_cost.replaced_bytes = u32::MAX;
                added.storage_cost.replaced_bytes = 1;
            }
            _ => added.storage_cost.removed_bytes = BasicStorageRemoval(1),
        }
        let before = total.clone();
        assert!(add_in_full(&mut total, added).is_err(), "{field}");
        assert_eq!(total, before, "{field}");
    }
}

#[test]
fn es3_a_wrapped_indexed_tree_is_hashed_with_the_byte_of_its_wrapper() {
    use grovedb_merk::tree::AggregateData;
    // Where ES3 stops: a layer does not say that a tree is wrapped, and the
    // wrapper is a byte of the element. Where that byte carries the element
    // with its length over a block, the hash of the carried element is a
    // call more than the estimate has: an indexed tree by its sum, with a
    // root key of 20 bytes, a secondary root key of 28 and the longest sum,
    // is 64 bytes with its length, and 65 wrapped.
    let hash_calls = |tree: &Element| {
        let mut shape = Shape::default();
        let host = shape.tree(path(&[b"host"]), &Element::empty_count_tree());
        let t = shape.tree(child(&host, b"t"), tree);
        let db = shape.build();
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, &GROVE_V6);
        grove.stopped_naming(&t, Some(vec![1; 20]), &[vec![1; 28]]);
        grove.stopped_with(&t, AggregateData::ProvableSum(i64::MAX));
        let applied = grove.finalize();
        let layer = |tree_type, sizes| EstimatedLayerInformation {
            tree_type,
            estimated_layer_count: ApproximateElements(1),
            estimated_layer_sizes: sizes,
        };
        let layers: HashMap<_, _> = [
            (
                known(&[]),
                layer(
                    TreeType::NormalTree,
                    AllSubtrees(4, EstimatedSumTrees::AllCountTrees, None),
                ),
            ),
            (
                known(&host),
                layer(
                    TreeType::CountTree,
                    AllSubtrees(1, EstimatedSumTrees::NoSumTrees, None),
                ),
            ),
            (
                known(&t),
                layer(TreeType::ProvableSumIndexedTree, AllItems(20, 3, None)),
            ),
        ]
        .into_iter()
        .collect();
        let average = GroveDb::average_case_finalize(&[known(&t)], &layers, &GROVE_V6)
            .cost_as_result()
            .expect("the finalize is estimated");
        (applied.hash_node_calls, average.hash_node_calls)
    };
    let plain = Element::empty_provable_sum_indexed_tree();
    let wrapped = Element::new_non_counted(plain.clone()).expect("a tree can be wrapped");
    assert_eq!(hash_calls(&plain), (10, 10));
    assert_eq!(hash_calls(&wrapped), (11, 10));
}

#[test]
fn es3_a_tree_that_keeps_its_data_flat_names_no_root_key() {
    // A tree that keeps its data flat has no Merk, so its element names no
    // root key, whatever its layer says of what it holds: an MMR of one
    // leaf, with 50 bytes of flags, is carried in the hash calls of the
    // estimate for a layer of one element as for a layer of none.
    let mut tree = Element::empty_mmr_tree();
    tree.set_flags(Some(vec![9; 50]));
    let mut shape = Shape::default();
    let host = shape.tree(path(&[b"host"]), &Element::empty_tree());
    let flat = shape.tree(child(&host, b"flat"), &tree);
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.stopped(&flat, None);
    let applied = grove.finalize();
    let estimate = |elements| {
        let layer = |tree_type, count, sizes| EstimatedLayerInformation {
            tree_type,
            estimated_layer_count: ApproximateElements(count),
            estimated_layer_sizes: sizes,
        };
        let layers: HashMap<_, _> = [
            (
                known(&[]),
                layer(
                    TreeType::NormalTree,
                    1,
                    AllSubtrees(4, EstimatedSumTrees::NoSumTrees, None),
                ),
            ),
            (
                known(&host),
                layer(
                    TreeType::NormalTree,
                    1,
                    AllSubtrees(4, EstimatedSumTrees::NoSumTrees, Some(50)),
                ),
            ),
            (
                known(&flat),
                layer(TreeType::MmrTree, elements, AllItems(8, 32, None)),
            ),
        ]
        .into_iter()
        .collect();
        GroveDb::average_case_finalize(&[known(&flat)], &layers, &GROVE_V6)
            .cost_as_result()
            .expect("the finalize is estimated")
    };
    assert_eq!(estimate(1), estimate(0));
    assert_eq!(estimate(1).hash_node_calls, applied.hash_node_calls);
}

#[test]
fn es3_a_kind_of_no_weight_sizes_no_root_key() {
    use grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerSizes::Mix;
    // A layer of several kinds holds no element of a kind it gives no
    // weight, so the keys of that kind are no key the Merk's root can have.
    let tree = Element::empty_tree();
    let db = host_of(&tree);
    let root_key = vec![7; 1];
    let named = |references_size| {
        finalize_naming(
            &db,
            &tree,
            &root_key,
            &[],
            Mix {
                subtrees_size: None,
                items_size: Some((1, 1, None, 1)),
                references_size,
                items_with_sum_item_size: None,
                references_with_sum_item_size: None,
            },
            &GROVE_V6,
        )
    };
    let (applied, without, _) = named(None);
    let (_, with_no_weight, _) = named(Some((255, 1, None, 0)));
    assert_eq!(with_no_weight, without);
    assert_eq!(without.hash_node_calls, applied.hash_node_calls);
    // A kind with a weight sizes it: a key of 255 bytes is four blocks more.
    let (_, with_a_weight, _) = named(Some((255, 1, None, 1)));
    assert_eq!(with_a_weight.hash_node_calls, without.hash_node_calls + 4);
}

#[test]
fn es2_no_size_of_a_layer_overflows_an_estimate_of_an_operation_on_grove_version_6() {
    use grovedb_merk::estimated_costs::average_case_costs::{
        EstimatedLayerCount::{EstimatedLevel, PotentiallyAtMaxElements},
        EstimatedLayerSizes::{AllItemsWithSumItem, AllReference, Mix},
    };
    // Sizes at the end of 32 bits, on both sides of what an estimate
    // refuses, for every kind of layer and every estimate of one operation
    // that takes a layer: each is counted or refused. A panic fails the
    // test.
    let edges: Vec<u32> = [0u32, 1, 2, 3, 5, 9, 11, 12, 13, 15, 16, 17, 60]
        .into_iter()
        .map(|below| u32::MAX - below)
        .chain([u32::MAX - 65_535, u32::MAX - 65_536, u32::MAX - 65_537])
        .chain([0, 1, 1 << 31])
        .collect();
    let item = Element::new_item(vec![1; 20]);
    let (mut counted, mut refused) = (0, 0);
    for value in &edges {
        for flags in [None, Some(1), Some(u32::MAX - 11), Some(u32::MAX)] {
            let layers = [
                AllItems(255, *value, flags),
                AllReference(255, *value, flags),
                AllItemsWithSumItem(255, *value, flags),
                AllSubtrees(255, EstimatedSumTrees::NoSumTrees, Some(*value)),
                AllSubtrees(255, EstimatedSumTrees::AllCountTrees, Some(*value)),
                AllSubtrees(255, EstimatedSumTrees::AllCountSumTrees, Some(*value)),
                Mix {
                    subtrees_size: Some((255, EstimatedSumTrees::AllCountTrees, Some(*value), 1)),
                    items_size: Some((255, *value, flags, 1)),
                    references_size: Some((255, *value, flags, 0)),
                    items_with_sum_item_size: Some((255, *value, flags, 255)),
                    references_with_sum_item_size: None,
                },
            ];
            for sizes in layers {
                for count in [
                    ApproximateElements(1),
                    EstimatedLevel(91, false),
                    PotentiallyAtMaxElements,
                ] {
                    let layer = EstimatedLayerInformation {
                        tree_type: TreeType::CountSumTree,
                        estimated_layer_count: count,
                        estimated_layer_sizes: sizes,
                    };
                    let k = KnownKey(key(0));
                    let tree = TreeType::CountSumTree;
                    // The estimate of the removal of a tree has one version
                    // for every grove version, and adds the flags of its
                    // layer unchecked on each: it is given the sizes that
                    // the others count.
                    if layer.estimated_layer_sizes.check_sizes().is_ok() {
                        let removal = GroveDb::average_case_merk_delete_tree(
                            &k, tree, &layer, true, &GROVE_V6,
                        );
                        match removal.value {
                            Ok(()) => counted += 1,
                            Err(_) => refused += 1,
                        }
                    }
                    let results = [
                        GroveDb::average_case_merk_delete_element(&k, &layer, false, &GROVE_V6),
                        GroveDb::average_case_merk_delete_element(&k, &layer, true, &GROVE_V6),
                        GroveDb::average_case_merk_insert_element(
                            &k,
                            &item,
                            tree,
                            Some(&layer),
                            &GROVE_V6,
                        ),
                        GroveDb::average_case_merk_replace_element(
                            &k,
                            &item,
                            tree,
                            Some(&layer),
                            &GROVE_V6,
                        ),
                        GroveDb::average_case_merk_patch_element(
                            &k,
                            &item,
                            1,
                            tree,
                            Some(&layer),
                            &GROVE_V6,
                        ),
                        GroveDb::average_case_merk_insert_tree(
                            &k,
                            &None,
                            tree,
                            tree,
                            0,
                            Some(&layer),
                            &GROVE_V6,
                        ),
                        GroveDb::average_case_finalize(
                            &[known(&path(&[b"a", b"b"]))],
                            &[
                                (known(&[]), layer),
                                (known(&path(&[b"a"])), layer),
                                (known(&path(&[b"a", b"b"])), layer),
                            ]
                            .into_iter()
                            .collect(),
                            &GROVE_V6,
                        ),
                    ];
                    for (number, result) in results.into_iter().enumerate() {
                        match result.value {
                            Ok(()) => counted += 1,
                            Err(_) => {
                                refused += 1;
                                // The insert, the replacement and the patch
                                // of an element and the finalize, each at a
                                // version of grove version 6, refuse with
                                // nothing added. The others are the
                                // functions of every grove version.
                                if matches!(number, 2 | 3 | 4 | 6) {
                                    assert_eq!(
                                        result.cost,
                                        OperationCost::default(),
                                        "{number}: {layer:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        counted > 0 && refused > 0,
        "{counted} counted, {refused} refused"
    );
}

#[test]
fn es2_a_tree_put_where_an_item_was_adds_bytes_that_no_estimate_of_its_replacement_has() {
    // Where ES2 stops: a replacement is estimated as one that adds no byte
    // for a tree, in the average case and in the worst, on every grove
    // version. A tree with 128 bytes of flags over an item with no bytes.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let leaf = shape.tree(path(&[b"a"]), &tree);
    shape
        .elements
        .push((leaf.clone(), key(0), Element::new_item(vec![])));
    let put_over = Element::empty_tree_with_flags(Some(vec![9; 128]));
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let applied = grove
        .write(&leaf, &put(&leaf, key(0), put_over.clone()).changes)
        .total();
    assert_eq!(applied.storage_cost.added_bytes, 99);
    for version in [&GROVE_V5, &GROVE_V6] {
        let average = GroveDb::average_case_merk_replace_element(
            &KnownKey(key(0)),
            &put_over,
            TreeType::NormalTree,
            Some(&layer_of_elements(TreeType::NormalTree, 1, &put_over)),
            version,
        )
        .cost_as_result()
        .expect("the replacement is estimated");
        let worst = GroveDb::worst_case_merk_replace_element(
            &KnownKey(key(0)),
            &put_over,
            TreeType::NormalTree,
            Some(&WorstCaseLayerInformation::MaxElementsNumber(1)),
            version,
        )
        .cost_as_result()
        .expect("the replacement is estimated");
        assert_eq!(
            (
                average.storage_cost.added_bytes,
                worst.storage_cost.added_bytes
            ),
            (0, 0),
            "grove version {}",
            version.protocol_version
        );
    }
}

#[test]
fn es2_the_removal_of_a_tree_in_a_layer_too_long_to_count_is_refused() {
    use grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerCount::ApproximateElements as Elements;
    // As for the removal of an element: the size of a subtree of a layer
    // has the aggregates of a tree, a byte longer on grove version 6. A
    // removal up the tree, of an element under `a/b` whose host is one of
    // a layer of trees with a count and flags of this length.
    let layer = |sizes| EstimatedLayerInformation {
        tree_type: TreeType::NormalTree,
        estimated_layer_count: Elements(1),
        estimated_layer_sizes: sizes,
    };
    let layers = |flags| {
        let mut layers = intmap::IntMap::new();
        layers.insert(
            0u16,
            layer(AllSubtrees(
                1,
                EstimatedSumTrees::AllCountTrees,
                Some(flags),
            )),
        );
        layers.insert(1u16, layer(AllItems(1, 8, None)));
        layers
    };
    let removal = |flags, version| {
        GroveDb::average_case_delete_operations_for_delete_up_tree_while_empty_engine_free(
            &known(&path(&[b"a", b"b"])),
            &KnownKey(key(0)),
            None,
            false,
            layers(flags),
            crate::BackwardsReferences::Check,
            version,
        )
    };
    let flags = u32::MAX - 11;
    assert_eq!(
        removal(flags, &GROVE_V5)
            .value
            .expect("grove version 5 counts the layer")
            .len(),
        2
    );
    assert!(matches!(
        removal(flags, &GROVE_V6).value,
        Err(Error::MerkError(grovedb_merk::Error::Overflow(_)))
    ));
    assert_eq!(
        removal(1_000_000, &GROVE_V6)
            .value
            .expect("flags a layer can have are counted")
            .len(),
        2
    );
}

#[test]
fn es3_a_finalize_too_long_to_count_is_refused() {
    use grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerCount::ApproximateElements as Elements;
    // A cost adds up to the most its fields hold and stops there, and a
    // sum that stopped is no count. A stop point 70 levels down, with
    // flags of four thousand million bytes on the tree of every level:
    // their hash alone is more calls than 32 bits count.
    let segments: Vec<Vec<u8>> = (0..70u8).map(|level| vec![level]).collect();
    let layers = |flags: u32| -> HashMap<KeyInfoPath, EstimatedLayerInformation> {
        (0..=segments.len())
            .map(|length| {
                (
                    known(&segments[..length]),
                    EstimatedLayerInformation {
                        tree_type: TreeType::NormalTree,
                        estimated_layer_count: Elements(1),
                        estimated_layer_sizes: AllSubtrees(
                            1,
                            EstimatedSumTrees::NoSumTrees,
                            Some(flags),
                        ),
                    },
                )
            })
            .collect()
    };
    let refused =
        GroveDb::average_case_finalize(&[known(&segments)], &layers(4_000_000_000), &GROVE_V6);
    assert!(matches!(
        refused.value,
        Err(Error::MerkError(grovedb_merk::Error::Overflow(_)))
    ));
    assert_eq!(refused.cost, OperationCost::default());
    // With flags a tree can have it is counted.
    let counted = GroveDb::average_case_finalize(&[known(&segments)], &layers(40), &GROVE_V6)
        .cost_as_result()
        .expect("the finalize is estimated");
    assert!(counted.seek_count > 0 && counted.hash_node_calls < u32::MAX);
    // More marked elements in one Merk than 32 bits count are refused.
    #[cfg(target_pointer_width = "64")]
    assert!(crate::estimated_costs::marked_count(u32::MAX as usize + 1).is_err());
    assert_eq!(
        crate::estimated_costs::marked_count(u32::MAX as usize).expect("a count"),
        u32::MAX
    );
}

#[test]
fn es2_a_removal_in_a_layer_too_long_to_count_is_refused() {
    // The aggregates of a tree are a byte longer on grove version 6, and
    // the size of a value is added in 32 bits: a layer of trees with a
    // count and flags of this length is counted on grove version 5 and
    // would overflow here. It is refused, with nothing added.
    let layer = |flags| EstimatedLayerInformation {
        tree_type: TreeType::NormalTree,
        estimated_layer_count: ApproximateElements(1),
        estimated_layer_sizes: AllSubtrees(1, EstimatedSumTrees::AllCountTrees, Some(flags)),
    };
    let removal = |flags, propagate, version| {
        GroveDb::average_case_merk_delete_element(
            &KnownKey(key(0)),
            &layer(flags),
            propagate,
            version,
        )
    };
    let flags = u32::MAX - 11;
    assert!(removal(flags, false, &GROVE_V5).value.is_ok());
    for propagate in [false, true] {
        let refused = removal(flags, propagate, &GROVE_V6);
        assert!(matches!(
            refused.value,
            Err(Error::MerkError(grovedb_merk::Error::Overflow(_)))
        ));
        assert_eq!(refused.cost, OperationCost::default());
    }
    // Flags a layer can have are counted.
    assert!(removal(1_000_000, true, &GROVE_V6).value.is_ok());
}

#[test]
fn es2_a_sum_item_whose_flags_grow_adds_bytes_that_no_estimate_of_its_replacement_has() {
    // Where ES2 stops: the replacement of a sum item is estimated as one by
    // a sum item of the same size, in the average case and in the worst, on
    // every grove version. The sum is of a fixed length. The flags are not.
    let mut shape = Shape::default();
    let leaf = shape.tree(path(&[b"a"]), &Element::empty_sum_tree());
    shape
        .elements
        .push((leaf.clone(), key(0), Element::new_sum_item(7)));
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    let flagged = Element::new_sum_item_with_flags(7, Some(vec![9; 16]));
    let applied = grove
        .write(&leaf, &put(&leaf, key(0), flagged.clone()).changes)
        .total();
    // Sixteen bytes of flags and the byte of their length.
    assert_eq!(applied.storage_cost.added_bytes, 17);
    for version in [&GROVE_V5, &GROVE_V6] {
        let average = GroveDb::average_case_merk_replace_element(
            &KnownKey(key(0)),
            &flagged,
            TreeType::SumTree,
            None,
            version,
        )
        .cost_as_result()
        .expect("the replacement is estimated");
        let worst = GroveDb::worst_case_merk_replace_element(
            &KnownKey(key(0)),
            &flagged,
            TreeType::SumTree,
            None,
            version,
        )
        .cost_as_result()
        .expect("the replacement is estimated");
        assert_eq!(average.storage_cost.added_bytes, 0);
        assert_eq!(worst.storage_cost.added_bytes, 0);
    }
    // An item that grows is estimated to add bytes by the worst case alone.
    let longer = Element::new_item(vec![1; 40]);
    let worst = GroveDb::worst_case_merk_replace_element(
        &KnownKey(key(0)),
        &longer,
        TreeType::NormalTree,
        None,
        &GROVE_V6,
    )
    .cost_as_result()
    .expect("the replacement is estimated");
    assert!(worst.storage_cost.added_bytes > 40);
}

/// The insert of an item of 20 bytes under `added` into a Merk that holds
/// such items under `keys`, put there in that order, and its average
/// estimate for a layer of `count`. The worst case is held here.
fn insert_among(
    keys: &[Vec<u8>],
    added: Vec<u8>,
    count: grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerCount,
    version: &'static GroveVersion,
) -> (OperationCost, OperationCost) {
    let tree = Element::empty_tree();
    let item = Element::new_item(vec![1; 20]);
    let mut shape = Shape::default();
    let leaf = shape.tree(path(&[b"a"]), &tree);
    for held in keys {
        shape
            .elements
            .push((leaf.clone(), held.clone(), item.clone()));
    }
    let db = shape.build();
    let applied = {
        let tx = db.start_transaction();
        let mut grove = Grove::new(&db, &tx, version);
        grove
            .write(&leaf, &put(&leaf, added.clone(), item.clone()).changes)
            .total()
    };
    let layer = EstimatedLayerInformation {
        tree_type: TreeType::NormalTree,
        estimated_layer_count: count,
        estimated_layer_sizes: AllItems(keys[0].len() as u8, size_in_a_layer(&item), None),
    };
    let mut average = open_estimate(&leaf, false, version);
    average += GroveDb::average_case_merk_insert_element(
        &KnownKey(added.clone()),
        &item,
        TreeType::NormalTree,
        Some(&layer),
        version,
    )
    .cost_as_result()
    .expect("the write is estimated");
    let mut worst = worst_open_estimate(&leaf, version);
    worst += GroveDb::worst_case_merk_insert_element(
        &KnownKey(added),
        &item,
        TreeType::NormalTree,
        Some(&WorstCaseLayerInformation::MaxElementsNumber(
            keys.len() as u32
        )),
        version,
    )
    .cost_as_result()
    .expect("the write is estimated");
    assert!(worst.worse_or_eq_than(&applied));
    (applied, average)
}

#[test]
fn es2_the_average_case_has_the_nodes_of_one_rotation() {
    use grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerCount::EstimatedLevel;
    // Where ES2 stops: the estimate of a level has one node beside the way
    // of a write, as every version of it has. Merk makes a double rotation
    // of rotations that each rebalance what they leave, and they fetch and
    // rewrite more nodes beside the way than one: here three. Eleven items
    // in four levels, which is as shallow as eleven can be, and a twelfth.
    let keys: Vec<Vec<u8>> = [4u8, 12, 6, 2, 20, 14, 10, 16, 0, 8, 18]
        .iter()
        .map(|number| key(*number))
        .collect();
    let (applied, by_levels) = insert_among(&keys, key(11), EstimatedLevel(4, false), &GROVE_V6);
    assert_eq!((applied.seek_count, by_levels.seek_count), (15, 11));
    assert_eq!(
        (applied.hash_node_calls, by_levels.hash_node_calls),
        (19, 15)
    );
    assert_eq!(
        (
            applied.storage_cost.replaced_bytes,
            by_levels.storage_cost.replaced_bytes
        ),
        (889, 635)
    );
    assert_eq!(
        (applied.storage_loaded_bytes, by_levels.storage_loaded_bytes),
        (934, 810)
    );
    assert_eq!(
        applied.storage_cost.added_bytes,
        by_levels.storage_cost.added_bytes
    );
}

#[test]
fn es3_a_node_above_a_marked_element_is_sized_at_the_average_of_the_kinds_of_its_layer() {
    // Where ES3 stops: a layer of several kinds of tree gives their
    // weights, and a node is sized at their average. A host of seven
    // trees, six plain and one with a sum of 128 bits, which is the root of
    // the host's Merk and so above every marked element.
    let mut shape = Shape::default();
    let host = shape.tree(path(&[b"host"]), &Element::empty_tree());
    let hosted: Vec<Path> = (0..7u8)
        .map(|number| {
            let tree = if number == 3 {
                Element::empty_big_sum_tree()
            } else {
                Element::empty_tree()
            };
            shape.tree(child(&host, &[b'h', number]), &tree)
        })
        .collect();
    let write = put(&hosted[0], key(0), Element::new_item(vec![1]));
    let (applied, average, worst) =
        finalize_and_its_estimates(&shape, std::slice::from_ref(&write), &GROVE_V6);
    assert_eq!(average.seek_count, applied.seek_count);
    assert_eq!(average.hash_node_calls, applied.hash_node_calls);
    assert_eq!(
        (
            applied.storage_cost.replaced_bytes,
            average.storage_cost.replaced_bytes
        ),
        (322, 310)
    );
    assert!(worst.worse_or_eq_than(&applied));
}

#[test]
fn es2_the_referrers_of_an_element_are_no_part_of_its_hash() {
    use grovedb_element::{
        reference_path::ReferencePathType, BackwardReference, BackwardReferences,
    };
    use grovedb_merk::element::insert::ElementInsertToStorageExtensions;
    // An item that keeps the list of its referrers is hashed without the
    // list. With one referrer under a path segment of 54 bytes it is 64
    // bytes, and 6 without.
    let element = Element::ItemWithBackwardsReferences(
        vec![1],
        BackwardReferences::new(
            1,
            vec![BackwardReference {
                inverted_reference: ReferencePathType::AbsolutePathReference(vec![vec![7; 54]]),
                cascade_on_update: false,
            }],
        ),
        None,
    );
    let sizes = (
        element.serialized_size(&GROVE_V6).expect("a size"),
        element
            .stripped_of_backward_references()
            .serialized_size(&GROVE_V6)
            .expect("a size"),
    );
    assert_eq!(sizes, (64, 6));
    // A plain item of as many bytes as the element is stored in.
    let plain = Element::new_item(vec![1; 61]);
    assert_eq!(plain.serialized_size(&GROVE_V6).expect("a size"), 64);
    // The put by itself, into a Merk with no node, by Merk's own insert.
    let tree = Element::empty_tree();
    let put_alone = |element: &Element| {
        let mut shape = Shape::default();
        let leaf = shape.tree(path(&[b"a"]), &tree);
        let db = shape.build();
        let tx = db.start_transaction();
        let batch = crate::tests::SuiteBatch::default();
        let mut merk = db
            .open_transactional_merk_at_path(
                grovedb_path::SubtreePath::from(super::refs(&leaf).as_slice()),
                &tx,
                Some(&batch),
                &GROVE_V6,
            )
            .unwrap()
            .expect("the Merk opens");
        let applied = element
            .insert(&mut merk, key(200), None, &GROVE_V6)
            .cost_as_result()
            .expect("the element is put")
            .hash_node_calls;
        let estimates = [
            GroveDb::average_case_merk_insert_element(
                &KnownKey(key(200)),
                element,
                TreeType::NormalTree,
                None,
                &GROVE_V6,
            ),
            GroveDb::worst_case_merk_insert_element(
                &KnownKey(key(200)),
                element,
                TreeType::NormalTree,
                None,
                &GROVE_V6,
            ),
        ]
        .map(|estimate| {
            estimate
                .cost_as_result()
                .expect("the put is estimated")
                .hash_node_calls
        });
        (applied, estimates)
    };
    let without = put_alone(&element.stripped_of_backward_references());
    assert_eq!(put_alone(&element), without);
    assert_eq!(without, (2, [4, 4]));
    // The plain item takes a block more for its 64 bytes and their length.
    assert_eq!(put_alone(&plain), (3, [5, 5]));
}

#[test]
fn es2_a_patch_by_more_bytes_than_the_item_has_is_refused() {
    // The estimate of a patch takes the bytes the item grew by from its
    // size in unsigned arithmetic. Grove version 6 refuses a growth the
    // item cannot have had, and estimates one it can.
    let item = Element::new_item(vec![1]);
    let size = item.serialized_size(&GROVE_V6).expect("a size") as i32;
    for (change, estimated) in [(size + 1, false), (100, false), (size, true), (-100, true)] {
        let average = GroveDb::average_case_merk_patch_element(
            &KnownKey(key(0)),
            &item,
            change,
            TreeType::NormalTree,
            None,
            &GROVE_V6,
        )
        .cost_as_result();
        let worst = GroveDb::worst_case_merk_patch_element(
            &KnownKey(key(0)),
            &item,
            change,
            TreeType::NormalTree,
            None,
            &GROVE_V6,
        )
        .cost_as_result();
        assert_eq!(average.is_ok(), estimated, "{change}");
        assert_eq!(worst.is_ok(), estimated, "{change}");
        if !estimated {
            assert!(matches!(average, Err(Error::InvalidParameter(_))));
            assert!(matches!(worst, Err(Error::InvalidParameter(_))));
        }
    }
}

#[test]
fn es2_the_average_case_takes_a_merk_to_be_as_shallow_as_its_count_allows() {
    use grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerCount::EstimatedLevel;
    // Where ES2 stops: a layer that is given by its count is taken to have
    // the levels a full tree of that count has. 54 elements fill six
    // levels, and the leanest AVL tree of 54 has eight.
    let tree = Element::empty_tree();
    let item = Element::new_item(vec![1; 20]);
    let lean: Vec<Vec<u8>> = leanest_merk(8)
        .into_iter()
        .map(|number| number.to_be_bytes().to_vec())
        .collect();
    assert_eq!(lean.len(), 54);
    // A new key under a node of the eighth level.
    let mut under_the_deepest = lean[53].clone();
    under_the_deepest.push(1);
    let (applied, by_count) = insert_among(
        &lean,
        under_the_deepest.clone(),
        ApproximateElements(54),
        &GROVE_V6,
    );
    let (_, by_levels) = insert_among(
        &lean,
        under_the_deepest,
        EstimatedLevel(8, false),
        &GROVE_V6,
    );
    // By its count the estimate has the nodes of six levels, and the write
    // goes through eight.
    assert_eq!((applied.seek_count, by_count.seek_count), (17, 15));
    assert!(by_levels.worse_or_eq_than(&applied));

    // A rotation rewrites a node beside the way of the write, and fetches
    // it. The estimate of a level has one such node, with its fetch: an
    // AVL tree of five elements in three levels, and a sixth that turns it.
    let keys: Vec<Vec<u8>> = [2u8, 10, 4, 8, 6].iter().map(|key| vec![*key]).collect();
    let (applied, by_levels) = insert_among(&keys, vec![9], EstimatedLevel(3, false), &GROVE_V6);
    assert_eq!((applied.seek_count, by_levels.seek_count), (9, 9));
    assert!(by_levels.worse_or_eq_than(&applied));

    // The same for a finalize: a host of seven trees in four levels, with
    // the deepest of them marked.
    let mut shape = Shape::default();
    let host = shape.tree(path(&[b"host"]), &tree);
    let hosted: Vec<Path> = [4u8, 2, 6, 1, 3, 7, 0]
        .iter()
        .map(|number| shape.filled(child(&host, &[*number]), &tree, 1, 1))
        .collect();
    let write = put(&hosted[6], key(200), item.clone());
    let (applied, by_count, worst) =
        finalize_and_its_estimates(&shape, std::slice::from_ref(&write), &GROVE_V6);
    assert_eq!((applied.seek_count, by_count.seek_count), (12, 10));
    assert!(by_count.hash_node_calls < applied.hash_node_calls);
    assert!(by_count.storage_cost.replaced_bytes < applied.storage_cost.replaced_bytes);
    assert!(worst.worse_or_eq_than(&applied));
    let mut by_levels = layers(&shape.after(std::slice::from_ref(&write)));
    by_levels
        .get_mut(&known(&host))
        .expect("the layer of the host")
        .estimated_layer_count = EstimatedLevel(4, false);
    let by_levels = GroveDb::average_case_finalize(&[known(&hosted[6])], &by_levels, &GROVE_V6)
        .cost_as_result()
        .expect("the finalize is estimated");
    assert_eq!(by_levels.seek_count, applied.seek_count);
    assert_eq!(by_levels.hash_node_calls, applied.hash_node_calls);
    assert_eq!(by_levels.storage_cost, applied.storage_cost);
}

#[test]
fn es3_a_sum_of_128_bits_is_hashed_at_its_longest() {
    // The cost of a tree with a sum of 128 bits has 16 bytes for the sum,
    // and a sum beyond 64 bits is 17 bytes of the element. Two sum items of
    // the most a sum item holds make one, and with a root key of 43 bytes
    // the 17th byte is the 65th the hash takes.
    let mut shape = Shape::default();
    let wide = shape.tree(path(&[b"t"]), &Element::empty_big_sum_tree());
    let writes = [
        put(&wide, vec![1; 43], Element::new_sum_item(i64::MAX)),
        put(&wide, vec![2; 43], Element::new_sum_item(i64::MAX)),
    ];
    let (applied, average, worst) = finalize_and_its_estimates(&shape, &writes, &GROVE_V6);
    assert_eq!(applied.hash_node_calls, 4);
    assert_eq!(average.hash_node_calls, applied.hash_node_calls);
    assert_eq!(average.seek_count, applied.seek_count);
    assert_eq!(average.storage_cost, applied.storage_cost);
    assert!(worst.worse_or_eq_than(&applied));
}

#[test]
fn es3_the_root_key_is_taken_at_the_size_the_layer_gives_its_keys() {
    // Where ES3 stops: a layer gives one size for its keys, and the root
    // key of a Merk is one of its keys. Where it is longer than that size,
    // the hash of the carried element is short by its blocks. The worst
    // case takes a root key at the longest a key can be.
    let tree = Element::empty_tree();
    let db = host_of(&tree);
    let (applied, average, worst) =
        finalize_naming(&db, &tree, &[7; 199], &[], AllItems(67, 4, None), &GROVE_V6);
    assert_eq!(applied.hash_node_calls, average.hash_node_calls + 2);
    assert_eq!(average.seek_count, applied.seek_count);
    assert_eq!(average.storage_cost, applied.storage_cost);
    assert!(worst.worse_or_eq_than(&applied));

    // The root of a secondary is the row of whichever entry is the root
    // there, not of the entry that is the root of the primary. A primary
    // whose root key is of the size the layer gives, 67 bytes, and a
    // secondary whose root is the row of an entry of 133: 141 bytes with
    // the sort key, where the estimate has 75.
    let indexed = Element::empty_provable_count_indexed_tree();
    let db = host_of(&indexed);
    let (applied, average, worst) = finalize_naming(
        &db,
        &indexed,
        &[7; 67],
        &[vec![7; 8 + 133]],
        AllItems(67, 4, None),
        &GROVE_V6,
    );
    assert_eq!(applied.hash_node_calls, average.hash_node_calls + 1);
    assert!(worst.worse_or_eq_than(&applied));
}

#[test]
fn es3_a_node_is_loaded_with_the_links_to_its_children_and_sized_with_one() {
    // Where ES3 stops: the bytes of a node are those the estimate of a
    // level gives one, which has one link. A record has the links to the
    // node's children: none for a leaf, two for a node with both. On a way
    // down a full Merk every node but the last has two. A host of 31
    // trees is a full Merk of five levels, and the leftmost is marked.
    let tree = Element::empty_tree();
    let mut shape = Shape::default();
    let host = shape.tree(path(&[b"host"]), &tree);
    let hosted: Vec<Path> = (0..31u8)
        .map(|number| shape.tree(child(&host, &[b'h', number]), &tree))
        .collect();
    let write = put(&hosted[0], key(200), Element::new_item(vec![1]));
    let (applied, average, worst) =
        finalize_and_its_estimates(&shape, std::slice::from_ref(&write), &GROVE_V6);
    assert_eq!(average.seek_count, applied.seek_count);
    assert_eq!(average.storage_cost, applied.storage_cost);
    assert_eq!(average.hash_node_calls, applied.hash_node_calls);
    assert_eq!(
        (applied.storage_loaded_bytes, average.storage_loaded_bytes),
        (801, 785)
    );
    assert!(worst.worse_or_eq_than(&applied));
}

#[test]
fn es2_the_loaded_bytes_of_a_write_hold_in_a_full_merk() {
    // A node is sized with one link here too, and a write is fetched
    // through nodes with two. The estimate of a level has a node beside the
    // way, which a write that turns nothing does not load, and that is more
    // than the links: full Merks of five, six and seven levels, and a new
    // key under the last node of each.
    let tree = Element::empty_tree();
    let item = Element::new_item(vec![0xA5; 20]);
    let mut loaded = Vec::new();
    for count in [31u8, 63, 127] {
        let mut shape = Shape::default();
        let leaf = shape.filled(path(&[b"a"]), &tree, count, 1);
        let db = shape.build();
        let applied = applied_insert(&db, &leaf, &item, &GROVE_V6);
        let (average, worst) =
            write_estimates(&leaf, TreeType::NormalTree, count, &item, &item, &GROVE_V6);
        assert!(average.worse_or_eq_than(&applied), "{count}");
        assert!(worst.worse_or_eq_than(&applied), "{count}");
        loaded.push((applied.storage_loaded_bytes, average.storage_loaded_bytes));
    }
    // A level more is 166 bytes loaded and 162 estimated.
    assert_eq!(loaded, [(754, 972), (920, 1134), (1086, 1296)]);
}

#[test]
fn es2_a_patch_is_estimated_with_its_flags_counted_twice() {
    use crate::batch::QualifiedGroveDbOp as Op;
    // Where ES2 stops: the estimate of a patch adds the flags of the item
    // to its bytes, which have them. An item with a flag byte that grows
    // from 62 bytes to 63 is 64 with its length, which takes a second byte
    // there: two bytes are added, and the estimate, which takes the item
    // to be 65 and its length to stay at two bytes, has one.
    let mut shape = Shape::default();
    let leaf = shape.tree(path(&[b"a"]), &Element::empty_tree());
    let grown = Element::new_item_with_flags(vec![1; 58], Some(vec![9]));
    shape.elements.push((
        leaf.clone(),
        key(0),
        Element::new_item_with_flags(vec![1; 57], Some(vec![9])),
    ));
    let db = shape.build();
    let applied = db
        .apply_batch(
            vec![Op::patch_op(leaf.clone(), key(0), grown.clone(), 1)],
            None,
            None,
            &GROVE_V5,
        )
        .cost_as_result()
        .expect("grove version 5 patches the item");
    assert_eq!(applied.storage_cost.added_bytes, 2);
    for version in [&GROVE_V5, &GROVE_V6] {
        let estimate = GroveDb::average_case_merk_patch_element(
            &KnownKey(key(0)),
            &grown,
            1,
            TreeType::NormalTree,
            None,
            version,
        )
        .cost_as_result()
        .expect("the patch is estimated");
        assert_eq!(estimate.storage_cost.added_bytes, 1);
    }
}

#[test]
fn es2_a_patch_is_estimated_by_the_growth_of_the_element_as_it_is_serialized() {
    use crate::batch::QualifiedGroveDbOp as Op;
    // The growth a patch is estimated by is that of the element's bytes,
    // not of what the item holds: an item that grows from 250 bytes to 251
    // has its length in three bytes where it had one, and is three bytes
    // longer.
    let (before, grown) = (
        Element::new_item(vec![1; 250]),
        Element::new_item(vec![1; 251]),
    );
    let size = |element: &Element| element.serialized_size(&GROVE_V6).expect("a size");
    assert_eq!(size(&grown) - size(&before), 3);
    let mut shape = Shape::default();
    let leaf = shape.tree(path(&[b"a"]), &Element::empty_tree());
    shape.elements.push((leaf.clone(), key(0), before));
    let db = shape.build();
    let applied = db
        .apply_batch(
            vec![Op::patch_op(leaf.clone(), key(0), grown.clone(), 1)],
            None,
            None,
            &GROVE_V5,
        )
        .cost_as_result()
        .expect("grove version 5 patches the item");
    assert_eq!(applied.storage_cost.added_bytes, 3);
    for version in [&GROVE_V5, &GROVE_V6] {
        let added = |growth| {
            GroveDb::average_case_merk_patch_element(
                &KnownKey(key(0)),
                &grown,
                growth,
                TreeType::NormalTree,
                None,
                version,
            )
            .cost_as_result()
            .expect("the patch is estimated")
            .storage_cost
            .added_bytes
        };
        assert_eq!((added(1), added(3)), (1, 3));
    }
}

#[test]
fn es2_the_rows_of_an_indexed_tree_are_estimated_with_a_hash_call_twice() {
    use grovedb_element::indexed::IndexAxis;
    // For the task that lets a batch stop: the estimate of the rows of an
    // indexed tree adds the hash call that combines a row with its target,
    // and on grove version 6 the estimate of the row's insert has that
    // call too. One call over for each axis, until the estimate of the rows
    // has a version of its own.
    let primary = EstimatedLayerInformation {
        tree_type: TreeType::ProvableCountIndexedTree,
        estimated_layer_count: ApproximateElements(0),
        estimated_layer_sizes: AllItems(2, 1, None),
    };
    let rows = |axes: &[IndexAxis], version| {
        GroveDb::average_case_indexed_secondary_mirror(
            &known(&path(&[b"ix"])),
            &primary,
            axes,
            version,
        )
        .cost_as_result()
        .expect("the rows are estimated")
        .hash_node_calls
    };
    assert_eq!(
        rows(&[IndexAxis::Count], &GROVE_V6),
        rows(&[IndexAxis::Count], &GROVE_V5) + 1
    );
    assert_eq!(
        rows(&[IndexAxis::Count, IndexAxis::Sum], &GROVE_V6),
        rows(&[IndexAxis::Count, IndexAxis::Sum], &GROVE_V5) + 2
    );
}

#[test]
fn es3_a_layer_longer_than_an_estimate_can_count_is_refused() {
    // The flags of the trees of a layer, at a size no estimate can add to.
    let tree = Element::empty_tree();
    let host = path(&[b"host"]);
    let t = child(&host, b"t");
    let layer = |count, sizes| EstimatedLayerInformation {
        tree_type: TreeType::NormalTree,
        estimated_layer_count: ApproximateElements(count),
        estimated_layer_sizes: sizes,
    };
    for flags in [u32::MAX, u32::MAX - 65_535] {
        let paths: HashMap<_, _> = [
            (
                known(&[]),
                layer(
                    1,
                    AllSubtrees(4, EstimatedSumTrees::NoSumTrees, Some(flags)),
                ),
            ),
            (
                known(&host),
                layer(
                    1,
                    AllSubtrees(1, EstimatedSumTrees::NoSumTrees, Some(flags)),
                ),
            ),
            (known(&t), layer(1, AllItems(1, 1, None))),
        ]
        .into_iter()
        .collect();
        let refused = GroveDb::average_case_finalize(&[known(&t)], &paths, &GROVE_V6);
        assert!(matches!(
            refused.value,
            Err(Error::MerkError(grovedb_merk::Error::Overflow(_)))
        ));
        assert_eq!(refused.cost, OperationCost::default());
    }
    let _ = tree;
}

#[test]
fn es3_a_layer_of_several_kinds_sizes_the_flags_of_a_tree_without_their_length() {
    use grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerSizes::Mix;
    // Where ES3 stops: the bytes of a node above a marked element are those
    // the estimate of a level gives a node, and for a layer of several
    // kinds that estimate is the one of grove versions 3 to 5, which sizes
    // the flags of a tree without the byte of their length.
    let flagged = Element::empty_tree_with_flags(Some(vec![1]));
    let mut shape = Shape::default();
    let host = shape.tree(path(&[b"host"]), &Element::empty_tree());
    let trees: Vec<Path> = (0..3u8)
        .map(|number| shape.tree(child(&host, &[b'h', number]), &flagged))
        .collect();
    let db = shape.build();
    let tx = db.start_transaction();
    let mut grove = Grove::new(&db, &tx, &GROVE_V6);
    grove.stopped(&trees[0], None);
    let applied = grove.finalize();
    let estimate = |sizes| {
        let layer = |count, sizes| EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(count),
            estimated_layer_sizes: sizes,
        };
        let paths: HashMap<_, _> = [
            (
                known(&[]),
                layer(1, AllSubtrees(4, EstimatedSumTrees::NoSumTrees, None)),
            ),
            (known(&host), layer(3, sizes)),
            (known(&trees[0]), layer(0, AllItems(2, 1, None))),
        ]
        .into_iter()
        .collect();
        GroveDb::average_case_finalize(&[known(&trees[0])], &paths, &GROVE_V6)
            .cost_as_result()
            .expect("the finalize is estimated")
    };
    let of_one_kind = estimate(AllSubtrees(2, EstimatedSumTrees::NoSumTrees, Some(1)));
    let of_several = estimate(Mix {
        subtrees_size: Some((2, EstimatedSumTrees::NoSumTrees, Some(1), 1)),
        items_size: None,
        references_size: None,
        items_with_sum_item_size: None,
        references_with_sum_item_size: None,
    });
    assert_eq!(of_one_kind.storage_cost, applied.storage_cost);
    assert_eq!(
        of_several.storage_cost.replaced_bytes + 1,
        applied.storage_cost.replaced_bytes
    );
    assert_eq!(of_several.seek_count, applied.seek_count);
    assert_eq!(of_several.hash_node_calls, applied.hash_node_calls);
}

#[test]
fn es2_an_indexed_tree_that_names_more_axes_than_one_can_is_refused() {
    // An indexed tree names one to three axes. An element with more is one
    // GroveDB stores nowhere, and the digest of its axes is not counted.
    let four_axes = Element::ProvableCountProvableSumIndexedTree(
        None,
        0,
        0,
        vec![(0, None), (1, None), (2, None), (2, None)],
        None,
    );
    let key_info = KnownKey(key(0));
    for refused in [
        GroveDb::average_case_merk_insert_element(
            &key_info,
            &four_axes,
            TreeType::NormalTree,
            None,
            &GROVE_V6,
        ),
        GroveDb::worst_case_merk_insert_element(
            &key_info,
            &four_axes,
            TreeType::NormalTree,
            None,
            &GROVE_V6,
        ),
        GroveDb::average_case_merk_replace_element(
            &key_info,
            &four_axes,
            TreeType::NormalTree,
            None,
            &GROVE_V6,
        ),
    ] {
        assert!(matches!(refused.value, Err(Error::InvalidInput(_))));
    }
}
