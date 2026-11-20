use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Arc,
};

use grovedb_element::{reference_path::ReferencePathType, BackwardReference, Element};
use grovedb_version::version::GroveVersion;
use lignin_format::{
    tree_page::{CellContent, CellValue, HashField, Side, TreeLink},
    CellAddress, PageId, PageSize, SlotId,
};
use lignin_tree::{
    hash::{HashRules, Stale},
    node::{Lease, NodePage, NodePageMut, NodeRef},
};
use lignindb::{
    commit::{Committed, NoStaleHashes},
    snapshot::Versions,
    txn::{testing::Db, Subtree, WritePath, WriteTxn},
    verify::{Finding, Place, Rule, Scope},
};

use super::{
    aggregate_of, held_link, refuse_stale, stale_hashes, HashRefusal, MerkHashRules, StatedByWriter,
};
use crate::{
    element::ElementExt,
    store::{
        lignin::{
            feature::{encode_aggregate, encode_feature},
            LigninBatch, LigninHost, LigninNodeStore, LigninSubtree, LigninWriter,
        },
        NodeChange, NodeChangeSet, NodeStore,
    },
    tree::{
        axes_digest, combine_hash,
        kv::{ValueDefinedCostType, KV},
        value_hash, AggregateData, NoopCommit, TreeNode, TreeNodeInner, NULL_HASH,
    },
    CryptoHash, Error, Link, Merk, Op, TreeFeatureType, TreeType,
};

fn version() -> &'static GroveVersion {
    GroveVersion::latest()
}

/// No value of the tests has a cost of its own.
const NO_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> = None;

/// Every tree type whose data is a Merk.
const TREE_TYPES: [TreeType; 17] = [
    TreeType::NormalTree,
    TreeType::SumTree,
    TreeType::BigSumTree,
    TreeType::CountTree,
    TreeType::CountSumTree,
    TreeType::CountBigSumTree,
    TreeType::ProvableCountTree,
    TreeType::ProvableCountSumTree,
    TreeType::ProvableSumTree,
    TreeType::ProvableBigSumTree,
    TreeType::ProvableCountProvableSumTree,
    TreeType::ProvableCountProvableBigSumTree,
    TreeType::ProvableSumIndexedTree,
    TreeType::ProvableBigSumIndexedTree,
    TreeType::ProvableCountIndexedTree,
    TreeType::ProvableCountProvableSumIndexedTree,
    TreeType::ProvableCountProvableBigSumIndexedTree,
];

/// The feature of the node `n` of a tree of `tree_type`, with numbers of
/// its own.
fn feature(tree_type: TreeType, n: u64) -> TreeFeatureType {
    use TreeFeatureType::*;
    let (count, sum, big) = (n % 5 + 1, n as i64 * 3 - 20, (n as i128 - 7) << 70);
    match tree_type.empty_tree_feature_type() {
        BasicMerkNode => BasicMerkNode,
        SummedMerkNode(_) => SummedMerkNode(sum),
        BigSummedMerkNode(_) => BigSummedMerkNode(big),
        CountedMerkNode(_) => CountedMerkNode(count),
        CountedSummedMerkNode(..) => CountedSummedMerkNode(count, sum),
        ProvableCountedMerkNode(_) => ProvableCountedMerkNode(count),
        ProvableCountedSummedMerkNode(..) => ProvableCountedSummedMerkNode(count, sum),
        ProvableSummedMerkNode(_) => ProvableSummedMerkNode(sum),
        ProvableCountedAndProvableSummedMerkNode(..) => {
            ProvableCountedAndProvableSummedMerkNode(count, sum)
        }
        ProvableCountedAndProvableBigSummedMerkNode(..) => {
            ProvableCountedAndProvableBigSummedMerkNode(count, big)
        }
        CountedBigSummedMerkNode(..) => CountedBigSummedMerkNode(count, big),
        ProvableBigSummedMerkNode(_) => ProvableBigSummedMerkNode(big),
    }
}

fn key(n: u64) -> Vec<u8> {
    n.to_be_bytes().to_vec()
}

/// An item as Merk stores it.
fn item(n: u64) -> Vec<u8> {
    let bytes = vec![n as u8; 1 + (n % 40) as usize];
    Element::new_item(bytes).serialize(version()).unwrap()
}

/// A tree over `keys` that no commit has hashed: every link is modified.
fn modified(keys: std::ops::Range<u64>, tree_type: TreeType) -> Option<TreeNode> {
    if keys.is_empty() {
        return None;
    }
    let middle = keys.start + (keys.end - keys.start) / 2;
    let node = TreeNode::new(key(middle), item(middle), None, feature(tree_type, middle)).unwrap();
    let node = node.attach(true, modified(keys.start..middle, tree_type));
    let node = node
        .unwrap()
        .attach(false, modified(middle + 1..keys.end, tree_type));
    Some(node.unwrap())
}

/// Hashes `tree` as a commit of Merk does, and keeps every node in memory.
fn hashed(mut tree: TreeNode) -> TreeNode {
    tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
        .unwrap()
        .unwrap();
    tree
}

/// What a link that holds a hash holds.
fn held(link: &Link) -> (CryptoHash, (u8, u8), AggregateData) {
    match link {
        Link::Reference {
            hash,
            child_heights,
            aggregate_data,
            ..
        }
        | Link::Loaded {
            hash,
            child_heights,
            aggregate_data,
            ..
        }
        | Link::Uncommitted {
            hash,
            child_heights,
            aggregate_data,
            ..
        } => (*hash, *child_heights, *aggregate_data),
        Link::Modified { .. } => panic!("a modified link holds no hash"),
    }
}

/// The address a link points at: a cell in another page.
fn elsewhere() -> CellAddress {
    CellAddress::new(PageId::new(77).unwrap(), SlotId::from_index(0).unwrap())
}

/// Shows `read` the cell of `content` in a page of its own.
fn with_content<R>(content: &CellContent<'_>, read: impl FnOnce(NodeRef<'_>) -> R) -> R {
    let (size, lease) = (PageSize::DEFAULT, Lease::take());
    let mut frame = vec![0; size.get()];
    let mut page = NodePageMut::create(&mut frame, size, PageId::MIN, lease, ()).unwrap();
    let cell = page.insert(content).unwrap();
    let page = NodePage::open(&frame, lease).unwrap();
    read(page.node(cell.slot()).expect("the cell that was inserted"))
}

/// Shows `read` the cell of `node` in a page of its own, with the roots
/// `hosted` and every link of the node pointing into another page.
fn with_cell<R>(
    node: &TreeNode,
    hosted: &[Option<CellAddress>],
    read: impl FnOnce(NodeRef<'_>) -> R,
) -> R {
    let feature = encode_feature(&node.feature_type());
    let links = [true, false].map(|left| {
        node.link(left).map(|link| {
            let (hash, heights, aggregate) = held(link);
            let key_len = link.key().len() as u8;
            (hash, heights, key_len, encode_aggregate(&aggregate))
        })
    });
    let link = |at: usize| {
        (links[at].as_ref()).map(|(hash, heights, key_len, aggregate)| TreeLink {
            child: elsewhere(),
            hash,
            heights: *heights,
            child_key_len: *key_len,
            aggregate: aggregate.as_slice(),
        })
    };
    let content = CellContent {
        key: node.key(),
        feature: feature.as_slice(),
        value_hash: node.value_hash(),
        kv_hash: node.kv_hash(),
        left: link(0),
        right: link(1),
        hosted_roots: hosted,
        value: CellValue::Inline(node.value_as_slice()),
        fragment_root: true,
    };
    with_content(&content, read)
}

/// The cell of a node with no child and the given parts.
fn with_leaf<R>(
    key: &[u8],
    value: &[u8],
    value_hash: CryptoHash,
    read: impl FnOnce(NodeRef<'_>) -> R,
) -> R {
    let kv = KV::from_fields(
        key.to_vec(),
        value.to_vec(),
        [9; 32],
        value_hash,
        TreeFeatureType::BasicMerkNode,
    );
    let node = TreeNode::new_with_tree_inner(TreeNodeInner {
        marks: Default::default(),
        left: None,
        right: None,
        kv,
    });
    with_cell(&node, &[], read)
}

/// Checks every hash that Merk holds of `node` and of what is below it
/// against what the rules make of the cells, and returns the nodes it saw.
fn check_below(rules: &mut MerkHashRules<'_>, node: &TreeNode, at: &str) -> usize {
    let mut seen = 1;
    with_cell(node, &[], |cell| {
        let kv_hash = rules.kv_hash(cell.key(), cell.value_hash()).unwrap();
        assert_eq!(&kv_hash, node.kv_hash(), "the key-value hash, {at}");
        let value = cell.value().inline().expect("a value in its cell");
        let own = rules.value_hash(cell, value, &[]).unwrap();
        assert_eq!(&own, node.value_hash(), "the value hash, {at}");
    });
    for left in [true, false] {
        let Some(link) = node.link(left) else {
            continue;
        };
        let child = link.tree().expect("the child is in memory");
        let made = with_cell(child, &[], |cell| rules.node_hash(cell).unwrap());
        assert_eq!(&made, link.hash(), "the hash of a child, {at}");
        seen += check_below(rules, child, at);
    }
    seen
}

#[test]
fn the_rules_make_the_hashes_a_commit_of_merk_makes() {
    let mut kinds = HashSet::new();
    for tree_type in TREE_TYPES {
        let at = format!("{tree_type:?}");
        let tree = hashed(modified(0..23, tree_type).unwrap());
        let mut rules = MerkHashRules::new(version());
        assert_eq!(check_below(&mut rules, &tree, &at), 23);

        // The hash of the root, which no cell stores, is the root hash of
        // the Merk: by the tree's type for Merk, by the node's feature
        // for the rule.
        let root = with_cell(&tree, &[], |cell| rules.node_hash(cell).unwrap());
        assert_eq!(root, tree.hash_for_link(tree_type).unwrap(), "{at}");
        kinds.insert(std::mem::discriminant(&tree.feature_type()));

        // 23 key-value hashes of one block, 23 value hashes of one, and
        // 23 node hashes of two, as Merk's functions count them.
        assert!(rules.cost().hash_node_calls >= 23 * 4, "{at}");
        assert_eq!(rules.cost().seek_count, 0);
    }
    assert_eq!(kinds.len(), 12, "every feature a node of Merk has");
}

#[test]
fn a_provable_feature_is_in_the_node_hash_and_another_is_not() {
    let mut rules = MerkHashRules::new(version());
    let mut hash_of = |tree_type| {
        let tree = hashed(modified(0..3, tree_type).unwrap());
        let plain = tree.hash().unwrap();
        let made = with_cell(&tree, &[], |cell| rules.node_hash(cell).unwrap());
        (made, plain)
    };
    for tree_type in [
        TreeType::NormalTree,
        TreeType::SumTree,
        TreeType::BigSumTree,
        TreeType::CountTree,
        TreeType::CountSumTree,
        TreeType::CountBigSumTree,
    ] {
        let (made, plain) = hash_of(tree_type);
        assert_eq!(made, plain, "{tree_type:?}");
    }
    for tree_type in [
        TreeType::ProvableCountTree,
        TreeType::ProvableCountSumTree,
        TreeType::ProvableSumTree,
        TreeType::ProvableBigSumTree,
        TreeType::ProvableCountProvableSumTree,
        TreeType::ProvableCountProvableBigSumTree,
    ] {
        let (made, plain) = hash_of(tree_type);
        assert_ne!(made, plain, "{tree_type:?}");
    }
}

fn path() -> ReferencePathType {
    ReferencePathType::AbsolutePathReference(vec![b"a".to_vec(), b"b".to_vec()])
}

/// `element` with one referrer registered.
fn referred(mut element: Element) -> Element {
    let referrers = element
        .backward_references_mut()
        .expect("it takes referrers");
    referrers.push(BackwardReference {
        inverted_reference: path(),
        cascade_on_update: false,
    });
    element
}

fn bytes(element: &Element) -> Vec<u8> {
    element
        .serialize(version())
        .expect("the element serializes")
}

fn made(value: &[u8], hosted: &[Option<CryptoHash>]) -> Result<CryptoHash, HashRefusal> {
    MerkHashRules::new(version()).element_value_hash(value, hosted)
}

#[test]
fn the_value_hash_of_an_item_is_made_from_its_bytes() {
    let items = [
        Element::new_item(b"value".to_vec()),
        Element::new_item_with_flags(vec![], Some(vec![1, 2])),
        Element::new_sum_item(-7),
        Element::new_item_with_sum_item(b"value".to_vec(), 5),
        Element::new_non_counted(Element::new_item(b"value".to_vec())).unwrap(),
        Element::new_non_counted(Element::new_sum_item(3)).unwrap(),
    ];
    for element in items {
        let value = bytes(&element);
        let node = KV::new(
            b"key".to_vec(),
            value.clone(),
            None,
            TreeFeatureType::BasicMerkNode,
        )
        .unwrap();
        assert_eq!(made(&value, &[]).as_ref(), Ok(node.value_hash()));
        assert_eq!(
            made(&value, &[None]),
            Err(HashRefusal::Hosted {
                element: 0,
                cell: 1
            })
        );
    }
}

#[test]
fn the_value_hash_of_an_item_with_referrers_leaves_the_referrers_out() {
    let items = [
        Element::new_item_allowing_bidirectional_references(b"value".to_vec()),
        Element::new_sum_item_allowing_bidirectional_references(11),
        Element::new_item_with_sum_item_allowing_bidirectional_references(b"value".to_vec(), 4),
    ];
    for element in items {
        let alone = bytes(&element);
        let element = referred(element);
        let value = bytes(&element);
        assert_ne!(value, alone);

        let expected = element.logical_value_hash(version()).unwrap().unwrap();
        assert_eq!(made(&value, &[]), Ok(expected));
        assert_eq!(made(&alone, &[]), Ok(expected));
        assert_eq!(expected, value_hash(&alone).unwrap());
        assert_ne!(expected, value_hash(&value).unwrap());
        assert!(matches!(
            made(&value, &[None]),
            Err(HashRefusal::Hosted { .. })
        ));
    }

    // Bytes that carry the type of such an item and are none.
    let value = bytes(&referred(
        Element::new_item_allowing_bidirectional_references(vec![1; 9]),
    ));
    let refused = made(&value[..value.len() - 3], &[]).unwrap_err();
    assert!(matches!(refused, HashRefusal::NotAnElement(_)), "{refused}");
}

#[test]
fn the_value_hash_of_a_tree_is_combined_with_the_root_hash_of_its_merk() {
    let trees = [
        Element::empty_tree(),
        Element::new_tree(Some(b"root".to_vec())),
        Element::empty_sum_tree(),
        Element::empty_big_sum_tree(),
        Element::empty_count_tree(),
        Element::empty_count_sum_tree(),
        Element::empty_count_big_sum_tree(),
        Element::empty_provable_count_tree(),
        Element::empty_provable_count_sum_tree(),
        Element::empty_provable_sum_tree(),
        Element::empty_provable_big_sum_tree(),
        Element::empty_provable_count_provable_sum_tree(),
        Element::empty_provable_count_provable_big_sum_tree(),
        Element::new_non_counted(Element::empty_tree()).unwrap(),
        Element::new_not_summed(Element::empty_sum_tree()).unwrap(),
        Element::new_not_counted_or_summed(Element::empty_count_sum_tree()).unwrap(),
    ];
    let root = [0xab; 32];
    for element in trees {
        let value = bytes(&element);
        let layered = |root| {
            let feature = TreeFeatureType::BasicMerkNode;
            let node = KV::new_with_layered_value_hash(vec![1], value.clone(), 3, root, feature);
            *node.unwrap().value_hash()
        };
        assert_eq!(made(&value, &[Some(root)]), Ok(layered(root)));
        // A Merk with no root has the hash of an empty Merk.
        assert_eq!(made(&value, &[None]), Ok(layered(NULL_HASH)));
        assert_ne!(layered(root), layered(NULL_HASH));

        // So has a Merk the cell has no pointer for: one that never had
        // a node.
        assert_eq!(made(&value, &[]), Ok(layered(NULL_HASH)));
        // A cell does not host more Merks than its element has.
        assert_eq!(
            made(&value, &[Some(root), None]),
            Err(HashRefusal::Hosted {
                element: 1,
                cell: 2
            })
        );
    }

    // What the rule counts is what Merk's functions count: a value hash
    // and the combination.
    let value = bytes(&Element::empty_tree());
    let mut rules = MerkHashRules::new(version());
    rules.element_value_hash(&value, &[Some(root)]).unwrap();
    let own = value_hash(&value);
    let expected =
        own.cost().hash_node_calls + combine_hash(own.value(), &root).cost().hash_node_calls;
    assert_eq!(rules.cost().hash_node_calls, expected);
}

#[test]
fn a_tree_that_old_versions_insert_as_a_plain_value_has_either_hash_until_it_has_a_root() {
    let inserted_plain = [
        Element::empty_count_sum_tree(),
        Element::empty_provable_count_tree(),
        Element::empty_provable_count_sum_tree(),
        Element::new_non_counted(Element::empty_provable_count_tree()).unwrap(),
        Element::new_not_summed(Element::empty_count_sum_tree()).unwrap(),
        Element::new_not_counted_or_summed(Element::empty_count_sum_tree()).unwrap(),
    ];
    let root = [0xab; 32];
    for element in inserted_plain {
        let value = bytes(&element);
        let plain = value_hash(&value).unwrap();
        let combined = made(&value, &[None]).unwrap();
        assert_ne!(plain, combined);
        let mut rules = MerkHashRules::new(version());
        assert_eq!(rules.value_hash_at_insert(&value), Some(plain));

        let answer = |mut rules: MerkHashRules<'_>, stored, hosted: &[Option<CryptoHash>]| {
            with_leaf(b"key", &value, stored, |cell| {
                rules.value_hash(cell, &value, hosted)
            })
            .expect("a tree's value hash is made")
        };
        // An evaluation makes the hash a write below the tree gives it,
        // whatever the cell stores.
        for stored in [plain, combined, [7; 32]] {
            assert_eq!(answer(MerkHashRules::new(version()), stored, &[]), combined);
        }
        // A check takes the hash of the insert for a tree with no root,
        // and no other hash, and none for a tree that has a root.
        let check = || MerkHashRules::for_check(version());
        assert_eq!(answer(check(), plain, &[]), plain);
        assert_eq!(answer(check(), plain, &[None]), plain);
        assert_eq!(answer(check(), combined, &[None]), combined);
        assert_eq!(answer(check(), [7; 32], &[None]), combined);
        let rooted = made(&value, &[Some(root)]).unwrap();
        assert_eq!(answer(check(), plain, &[Some(root)]), rooted);
    }

    // Every other value has one hash.
    let others = [
        Element::empty_tree(),
        // No version that inserts a tree as a plain value has this one.
        Element::empty_count_big_sum_tree(),
        Element::empty_sum_tree(),
        Element::empty_count_tree(),
        Element::empty_provable_sum_tree(),
        Element::empty_provable_count_indexed_tree(),
        Element::new_item(vec![1]),
        Element::new_reference(path()),
        Element::empty_mmr_tree(),
    ];
    for element in others {
        let value = bytes(&element);
        assert_eq!(
            MerkHashRules::new(version()).value_hash_at_insert(&value),
            None
        );
        if let Ok(combined) = made(&value, &[]) {
            let stored = value_hash(&value).unwrap();
            let checked = with_leaf(b"key", &value, stored, |cell| {
                MerkHashRules::for_check(version()).value_hash(cell, &value, &[])
            });
            assert_eq!(checked.unwrap(), combined);
        }
    }
    assert_eq!(
        MerkHashRules::new(version()).value_hash_at_insert(&[]),
        None
    );
}

#[test]
fn the_value_hash_of_an_indexed_tree_is_combined_with_both_of_its_merks() {
    let trees = [
        Element::empty_provable_sum_indexed_tree(),
        Element::empty_provable_count_indexed_tree(),
        Element::empty_provable_big_sum_indexed_tree(),
        Element::new_provable_sum_indexed_tree_with_root_keys_and_sum_value(
            Some(vec![1]),
            Some(vec![2]),
            9,
            None,
        ),
        Element::new_non_counted(Element::empty_provable_count_indexed_tree()).unwrap(),
    ];
    let (primary, secondary) = ([0x11; 32], [0x22; 32]);
    for element in trees {
        let value = bytes(&element);
        let layered = |primary, secondary| {
            let feature = TreeFeatureType::BasicMerkNode;
            let node = KV::new_with_layered_value_hash_three(
                vec![1],
                value.clone(),
                3,
                primary,
                secondary,
                feature,
            );
            *node.unwrap().value_hash()
        };
        assert_eq!(
            made(&value, &[Some(primary), Some(secondary)]),
            Ok(layered(primary, secondary))
        );
        assert_eq!(
            made(&value, &[Some(primary), None]),
            Ok(layered(primary, NULL_HASH))
        );
        assert_eq!(
            made(&value, &[None, None]),
            Ok(layered(NULL_HASH, NULL_HASH))
        );
        assert_ne!(layered(primary, secondary), layered(secondary, primary));
        assert_eq!(
            made(&value, &[Some(primary)]),
            Ok(layered(primary, NULL_HASH))
        );
        assert_eq!(made(&value, &[]), Ok(layered(NULL_HASH, NULL_HASH)));
        assert_eq!(
            made(&value, &[Some(primary), None, None]),
            Err(HashRefusal::Hosted {
                element: 2,
                cell: 3
            })
        );
    }
}

#[test]
fn the_value_hash_of_a_tree_with_axes_is_combined_with_the_digest_of_their_roots() {
    let two = vec![(0, None), (2, Some(vec![7]))];
    let three = vec![(0, None), (2, None), (3, None)];
    let trees = [
        (
            Element::new_provable_count_provable_sum_indexed_tree(None, 0, 0, two.clone(), None)
                .unwrap(),
            two,
        ),
        (
            Element::new_provable_count_provable_big_sum_indexed_tree(
                Some(vec![5]),
                3,
                1 << 80,
                three.clone(),
                None,
            )
            .unwrap(),
            three,
        ),
    ];
    for (element, axes) in trees {
        let value = bytes(&element);
        let primary = [0x33; 32];
        let roots: Vec<Option<CryptoHash>> = (0..axes.len() as u8)
            .map(|n| (n != 1).then_some([0x40 + n; 32]))
            .collect();
        let tagged: Vec<(u8, CryptoHash)> = (axes.iter().zip(&roots))
            .map(|((tag, _), root)| (*tag, root.unwrap_or(NULL_HASH)))
            .collect();
        let digest = axes_digest(&tagged).unwrap();
        let expected = KV::new_with_layered_value_hash_three(
            vec![1],
            value.clone(),
            3,
            primary,
            digest,
            TreeFeatureType::BasicMerkNode,
        );
        let expected = *expected.unwrap().value_hash();

        let mut hosted = vec![Some(primary)];
        hosted.extend(&roots);
        assert_eq!(made(&value, &hosted), Ok(expected));

        // The axes are taken in the order the element lists them.
        hosted[1..].reverse();
        assert_ne!(made(&value, &hosted), Ok(expected));
        hosted.push(None);
        assert_eq!(
            made(&value, &hosted),
            Err(HashRefusal::Hosted {
                element: 1 + axes.len(),
                cell: 2 + axes.len()
            })
        );

        // An axis the cell has no pointer for has no root.
        let all_empty: Vec<(u8, CryptoHash)> =
            axes.iter().map(|(tag, _)| (*tag, NULL_HASH)).collect();
        let expected = KV::new_with_layered_value_hash_three(
            vec![1],
            value.clone(),
            3,
            primary,
            axes_digest(&all_empty).unwrap(),
            TreeFeatureType::BasicMerkNode,
        );
        let expected = *expected.unwrap().value_hash();
        assert_eq!(made(&value, &[Some(primary)]), Ok(expected));
        assert_eq!(made(&value, &[Some(primary), None]), Ok(expected));
    }
}

#[test]
fn a_value_hash_that_only_its_writer_can_make_is_refused() {
    let references = [
        Element::new_reference(path()),
        Element::new_reference_with_sum_item(path(), 3),
        Element::new_bidirectional_reference(path()),
        Element::new_non_counted(Element::new_reference(path())).unwrap(),
    ];
    let other_trees = [
        Element::empty_commitment_tree(5).unwrap(),
        Element::empty_mmr_tree(),
        Element::empty_bulk_append_tree(5).unwrap(),
        Element::empty_dense_tree(3),
        Element::empty_private_document_store(32, 5).unwrap(),
        Element::new_non_counted(Element::empty_mmr_tree()).unwrap(),
    ];
    let references = references
        .iter()
        .map(|element| (element, StatedByWriter::Reference));
    let other_trees = (other_trees.iter()).map(|element| (element, StatedByWriter::NonMerkTree));
    for (element, why) in references.chain(other_trees) {
        let value = bytes(element);
        let stored = [0x5c; 32];
        assert_eq!(made(&value, &[]), Err(HashRefusal::StatedByWriter(why)));
        // Such an element hosts no Merk: a cell that does is refused as
        // that, for an evaluation and for a check.
        for hosted in [&[None][..], &[Some([1; 32]), None][..]] {
            let hosts = HashRefusal::Hosted {
                element: 0,
                cell: hosted.len(),
            };
            assert_eq!(made(&value, hosted), Err(hosts.clone()));
            let checked = with_leaf(b"key", &value, stored, |cell| {
                MerkHashRules::for_check(version()).value_hash(cell, &value, hosted)
            });
            let checked = checked.expect_err("a cell that hosts a Merk");
            assert_eq!(checked.downcast_ref::<HashRefusal>(), Some(&hosts));
        }

        // As a rule of an evaluation it refuses, and the hash stays marked.
        let refused = with_leaf(b"key", &value, stored, |cell| {
            MerkHashRules::new(version()).value_hash(cell, &value, &[])
        });
        let refused = refused.expect_err("the rule refuses");
        assert_eq!(
            refused.downcast_ref::<HashRefusal>(),
            Some(&HashRefusal::StatedByWriter(why))
        );
        assert!(refused.to_string().contains("its writer states it"));

        // For a check it answers what the cell stores.
        let answered = with_leaf(b"key", &value, stored, |cell| {
            MerkHashRules::for_check(version()).value_hash(cell, &value, &[])
        });
        assert_eq!(answered.unwrap(), stored);
    }
}

#[test]
fn a_value_that_is_no_element_is_refused_also_for_a_check() {
    let truncated_wrapper = &bytes(&Element::new_non_counted(Element::new_item(vec![1])).unwrap());
    let values: [&[u8]; 3] = [&[], &[0xff, 1, 2], &truncated_wrapper[..1]];
    for value in values {
        for mut rules in [
            MerkHashRules::new(version()),
            MerkHashRules::for_check(version()),
        ] {
            let refused = with_leaf(b"key", value, [1; 32], |cell| {
                rules.value_hash(cell, value, &[])
            });
            let refused = refused.expect_err("no element");
            let refused = refused.downcast_ref::<HashRefusal>().expect("a refusal");
            assert!(matches!(refused, HashRefusal::NotAnElement(_)), "{refused}");
            assert!(refused.to_string().starts_with("the value is no element"));
        }
    }
    // A cell that hosts more than its element has is refused for a check
    // as well.
    let value = bytes(&Element::empty_tree());
    let refused = with_leaf(b"key", &value, [1; 32], |cell| {
        MerkHashRules::for_check(version()).value_hash(cell, &value, &[None, None])
    });
    assert_eq!(
        refused.unwrap_err().to_string(),
        "the element has 1 Merks and its cell hosts 2"
    );
}

#[test]
fn the_key_value_hash_is_merks() {
    let mut rules = MerkHashRules::new(version());
    for key in [&b""[..], b"key", &[7; 255]] {
        let node = KV::new(key.to_vec(), item(3), None, TreeFeatureType::BasicMerkNode).unwrap();
        let made = rules.kv_hash(key, node.value_hash()).unwrap();
        assert_eq!(&made, node.hash());
    }
}

#[test]
fn a_cell_that_holds_no_node_of_merk_is_refused() {
    let node = hashed(modified(0..3, TreeType::SumTree).unwrap());
    let refusal = |feature: &[u8], aggregate: &[u8]| {
        let content = CellContent {
            key: node.key(),
            feature,
            value_hash: node.value_hash(),
            kv_hash: node.kv_hash(),
            left: Some(TreeLink {
                child: elsewhere(),
                hash: &[1; 32],
                heights: (0, 0),
                child_key_len: 8,
                aggregate,
            }),
            right: None,
            hosted_roots: &[],
            value: CellValue::Inline(b""),
            fragment_root: true,
        };
        let refused = with_content(&content, |cell| {
            MerkHashRules::new(version()).node_hash(cell)
        });
        refused.expect_err("no node of Merk").to_string()
    };
    let sum = |sum: i64| encode_feature(&TreeFeatureType::SummedMerkNode(sum));
    let linked = |sum: i64| encode_aggregate(&AggregateData::Sum(sum));

    let unknown = refusal(&[0xee], linked(1).as_slice());
    assert!(
        unknown.starts_with("the cell holds no node of Merk"),
        "{unknown}"
    );
    assert!(unknown.contains("feature"), "{unknown}");
    let cut = refusal(sum(1).as_slice(), &linked(1).as_slice()[..4]);
    assert!(cut.contains("aggregate"), "{cut}");
    // The aggregates of the node and of its child do not add up.
    let over = refusal(sum(i64::MAX).as_slice(), linked(1).as_slice());
    assert!(over.contains("overflow"), "{over}");
}

#[test]
fn a_refusal_says_what_was_refused() {
    let said = |refusal: HashRefusal| refusal.to_string();
    assert!(said(HashRefusal::StatedByWriter(StatedByWriter::Reference)).contains("reference"));
    assert!(said(HashRefusal::StatedByWriter(StatedByWriter::NonMerkTree)).contains("no Merk"));
    assert!(said(HashRefusal::NotAnElement("why".to_owned())).ends_with("why"));
    assert!(said(HashRefusal::Node("why".to_owned())).ends_with("why"));
    let hosted = HashRefusal::Hosted {
        element: 2,
        cell: 1,
    };
    assert_eq!(said(hosted), "the element has 2 Merks and its cell hosts 1");

    assert!(refuse_stale(Stale::NONE).is_ok());
    let refused = refuse_stale(Stale::of(HashField::Child(Side::Left))).unwrap_err();
    assert!(matches!(refused, Error::StorageError(_)), "{refused:?}");
    assert!(refused.to_string().contains("hash of the left child"));
}

#[test]
fn a_modified_link_is_the_one_whose_hash_merk_does_not_hold() {
    let fresh = modified(0..7, TreeType::CountSumTree).unwrap();
    let both: Stale = [Side::Left, Side::Right]
        .map(HashField::Child)
        .into_iter()
        .collect();
    assert_eq!(stale_hashes(&fresh), both);
    assert!(held_link(fresh.link(true).unwrap()).unwrap().hash.is_none());
    assert_eq!(stale_hashes(fresh.child(true).unwrap()), both);
    let leaf = fresh.child(true).unwrap().child(false).unwrap();
    assert_eq!(stale_hashes(leaf), Stale::NONE);

    // Loaded: hashed by a commit and kept in memory.
    let loaded = hashed(fresh.clone());
    assert!(loaded.link(true).unwrap().is_stored());
    assert_eq!(stale_hashes(&loaded), Stale::NONE);

    // Reference: read from its record.
    let stored =
        TreeNode::decode_raw(&loaded.encode(), loaded.key().to_vec(), NO_COST, version()).unwrap();
    assert!(stored.link(true).unwrap().is_reference());
    assert_eq!(stale_hashes(&stored), Stale::NONE);

    // Uncommitted: changed since the last commit, and hashed since.
    let uncommitted = |link: &Link| {
        let (hash, child_heights, aggregate_data) = held(link);
        Link::Uncommitted {
            hash,
            child_heights,
            tree: link.tree().unwrap().clone(),
            aggregate_data,
        }
    };
    let mut one_side = loaded.clone();
    one_side.inner.left = one_side.inner.left.as_ref().map(uncommitted);
    assert!(one_side.link(true).unwrap().is_uncommitted());
    assert_eq!(stale_hashes(&one_side), Stale::NONE);

    // Every state but the modified one hands its hash, its heights and
    // its aggregate over as they are.
    for (node, left) in [
        (&loaded, true),
        (&loaded, false),
        (&stored, true),
        (&one_side, true),
    ] {
        let link = node.link(left).unwrap();
        let handed = held_link(link).unwrap();
        let (hash, heights, aggregate) = held(link);
        assert_eq!(
            (handed.hash, handed.heights, handed.aggregate),
            (Some(hash), heights, aggregate)
        );
    }

    // One modified side leaves the other as it is.
    let mut mixed = loaded.clone();
    mixed.inner.right = fresh.inner.right.clone();
    assert_eq!(
        stale_hashes(&mixed),
        Stale::of(HashField::Child(Side::Right))
    );
}

/// Compares what a modified link hands over, at every level, with the link
/// a commit of Merk makes of it.
fn same_as_committed(fresh: &TreeNode, committed: &TreeNode, at: &str) {
    use ed::Encode;

    for left in [true, false] {
        let (Some(link), Some(after)) = (fresh.link(left), committed.link(left)) else {
            assert!(fresh.link(left).is_none() && committed.link(left).is_none());
            continue;
        };
        let handed = held_link(link).unwrap();
        let (_, heights, aggregate) = held(after);
        assert_eq!(handed.hash, None, "{at}");
        assert_eq!(
            (handed.heights, handed.aggregate),
            (heights, aggregate),
            "{at}"
        );
        assert_eq!(
            handed.record_len(link.key().len()).unwrap(),
            after.encoding_length().unwrap(),
            "{at}"
        );
        same_as_committed(link.tree().unwrap(), after.tree().unwrap(), at);
    }
    assert_eq!(
        aggregate_of(fresh).unwrap(),
        committed.aggregate_data().unwrap(),
        "{at}"
    );
}

/// A tree of three modified nodes whose middle one has a sum that its
/// child's sum takes past `i64::MAX`.
fn overflowing() -> TreeNode {
    let sum = TreeFeatureType::SummedMerkNode;
    let child = TreeNode::new(key(1), item(1), None, sum(1)).unwrap();
    let parent = TreeNode::new(key(2), item(2), None, sum(i64::MAX)).unwrap();
    let parent = parent.attach(true, Some(child)).unwrap();
    let above = TreeNode::new(key(3), item(3), None, sum(0)).unwrap();
    above.attach(true, Some(parent)).unwrap()
}

#[test]
fn a_modified_link_is_handed_over_with_the_aggregate_a_commit_would_give_it() {
    for tree_type in TREE_TYPES {
        let fresh = modified(0..40, tree_type).unwrap();
        let committed = hashed(fresh.clone());
        same_as_committed(&fresh, &committed, &format!("{tree_type:?}"));
    }

    // Aggregates that do not add up are an error, as they are in a commit.
    let above = overflowing();
    assert!(matches!(
        held_link(above.link(true).unwrap()),
        Err(Error::Overflow(_))
    ));
}

/// A LigninDB store on disk, committed with the hash rules a test gives.
struct Lignin {
    db: Arc<Db>,
    versions: Versions,
}

const SUBTREE: [u8; 32] = [0; 32];

impl Lignin {
    fn new() -> Self {
        let (db, versions) = Db::new(512);
        db.check_hashes_by(Box::new(MerkHashRules::for_check(version())));
        Self { db, versions }
    }

    fn path(&self) -> WritePath<'_> {
        self.db.path(&self.versions)
    }

    /// Commits `transaction` with `rules` and returns how many nodes had a
    /// stale hash that the commit evaluated.
    fn commit(&self, transaction: &WriteTxn<'_>, rules: &mut impl HashRules) -> usize {
        let committer = self.db.committer(&self.versions);
        match committer.commit(transaction, rules).expect("the commit") {
            Committed::Published(published) => published.evaluated,
            Committed::Empty => panic!("the transaction wrote nothing"),
        }
    }
}

fn store<'db>(writer: &'db LigninWriter<'_, '_>) -> LigninNodeStore<'db> {
    LigninNodeStore::immediate(writer, LigninSubtree::alone(SUBTREE))
}

/// Writes `nodes` and removes `removed` as one change set of the Merk
/// whose root is `root`.
fn write(
    store: &LigninNodeStore<'_>,
    nodes: &[(&TreeNode, NodeChange)],
    removed: &[&[u8]],
    root: &TreeNode,
) {
    let mut change_set = store.change_set();
    change_set.state_root(Some(root.key()));
    for key in removed {
        change_set.delete_node(key, None);
    }
    for (node, change) in nodes {
        let prepared = change_set.prepare_node(node, *change);
        change_set.put_node(node.key(), prepared, None).unwrap();
    }
    store
        .commit(change_set)
        .unwrap()
        .expect("the write is taken");
}

/// The stale hashes of the node under `key`, and the bytes its links have
/// for the hashes of its children.
fn stored_hashes<'t, 's>(
    writer: LigninWriter<'t, 's>,
    key: &[u8],
) -> (Stale, [Option<CryptoHash>; 2], LigninWriter<'t, 's>) {
    let scope = writer.into_scope();
    let read = scope.node(Subtree::new(SUBTREE), key, |view| {
        let view = view.expect("the node is there");
        let links = Side::BOTH.map(|side| view.link(side).map(|link| *link.hash));
        (view.stale(), links)
    });
    let (stale, links) = read.expect("the node is read");
    (stale, links, LigninWriter::new(scope))
}

#[test]
fn a_node_with_a_modified_link_leaves_that_hash_to_the_engine() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());

    // A Merk of three nodes, hashed by Merk.
    let tree = hashed(modified(10..13, TreeType::CountTree).unwrap());
    let (left, right) = (tree.child(true).unwrap(), tree.child(false).unwrap());
    let all = [
        (left, NodeChange::ValueWritten),
        (right, NodeChange::ValueWritten),
        (&tree, NodeChange::ValueWritten),
    ];
    write(&store(&writer), &all, &[], &tree);
    assert_eq!(transaction.stats().stale_nodes, 0);
    let (stale, before, writer) = stored_hashes(writer, tree.key());
    assert_eq!(stale, Stale::NONE);
    assert_eq!(
        before,
        [true, false].map(|left| Some(*tree.child_hash(left)))
    );

    // The root again, with another node on its left that no commit has
    // hashed, and a new node on the left of that one.
    let below = TreeNode::new(key(4), item(4), None, feature(TreeType::CountTree, 4)).unwrap();
    let other = TreeNode::new(key(5), item(5), None, feature(TreeType::CountTree, 5)).unwrap();
    let other = other.attach(true, Some(below)).unwrap();
    let mut root = tree.clone();
    root.inner.left = None;
    let root = root.attach(true, Some(other)).unwrap();
    let other = root.child(true).unwrap();
    let changed = [
        (other.child(true).unwrap(), NodeChange::ValueWritten),
        (other, NodeChange::ValueWritten),
        (&root, NodeChange::StructureChanged),
    ];
    let handle = store(&writer);
    write(&handle, &changed, &[left.key()], &root);
    assert_eq!(transaction.stats().stale_nodes, 2);

    // The store refuses to hand Merk a node with such a hash, and reads
    // the nodes around it.
    for stale in [root.key(), other.key()] {
        let refused = handle.get_node(stale, NO_COST, version());
        let refused = refused.unwrap().expect_err("a stale hash is no hash");
        assert!(matches!(refused, Error::StorageError(_)), "{refused:?}");
        assert!(
            refused.to_string().contains("not all made yet"),
            "{refused}"
        );
        assert!(handle.get_node_for_read(stale).unwrap().is_err());
        assert!(handle.has_node(stale).unwrap().unwrap());
    }
    let read = handle.get_node(right.key(), NO_COST, version());
    assert_eq!(read.unwrap().unwrap().unwrap().kv_hash(), right.kv_hash());
    drop(handle);

    // The link that was there keeps the bytes it had, so that the write
    // changes no hash, and the link that is new has none.
    let left_stale = Stale::of(HashField::Child(Side::Left));
    let (stale, links, writer) = stored_hashes(writer, root.key());
    assert_eq!(stale, left_stale);
    assert_eq!(links, before);
    let (stale, links, writer) = stored_hashes(writer, other.key());
    assert_eq!(stale, left_stale);
    assert_eq!(links, [Some(NULL_HASH), None]);

    // The aggregate in the link is exact: a count of two below the root's
    // left, as a commit of Merk would have it.
    let expected = hashed(root.clone());
    let committed = held(expected.link(true).unwrap());
    let handed = held_link(root.link(true).unwrap()).unwrap();
    assert_eq!(
        (handed.heights, handed.aggregate),
        (committed.1, committed.2)
    );

    // A later write of the node with the hash made takes the mark off.
    let rehashed = expected.child(true).unwrap();
    write(
        &store(&writer),
        &[(rehashed, NodeChange::HashOnly)],
        &[],
        &root,
    );
    assert_eq!(transaction.stats().stale_nodes, 1);
    let (stale, links, writer) = stored_hashes(writer, other.key());
    assert_eq!(stale, Stale::NONE);
    assert_eq!(links, [Some(*rehashed.child_hash(true)), None]);

    writer.into_scope().merge();

    // The commit makes what is left with Merk's rules: one node hash, of
    // the node under the stale link, which is the hash a commit of Merk
    // gives that link.
    let mut rules = MerkHashRules::new(version());
    assert_eq!(lignin.commit(&transaction, &mut rules), 1);
    assert_eq!(rules.cost().hash_node_calls, 2);
    let reader = path.begin();
    let committed = LigninNodeStore::read_only(&reader, LigninSubtree::alone(SUBTREE));
    let read = committed.get_node(root.key(), NO_COST, version());
    let read = read.unwrap().unwrap().expect("the root is committed");
    for left in [true, false] {
        assert_eq!(
            held(read.link(left).unwrap()),
            held(expected.link(left).unwrap())
        );
    }
}

/// Writes `node` and everything below it into `change_set`, children
/// before parents, as Merk holds them: with no hash for a modified link.
/// Returns how many of the nodes have a child.
fn put_below(change_set: &mut impl NodeChangeSet, node: &TreeNode) -> usize {
    let mut with_children = 0;
    for left in [true, false] {
        if let Some(child) = node.child(left) {
            with_children += put_below(change_set, child);
        }
    }
    let prepared = change_set.prepare_node(node, NodeChange::ValueWritten);
    change_set.put_node(node.key(), prepared, None).unwrap();
    with_children + usize::from(node.link(true).is_some() || node.link(false).is_some())
}

/// Compares the tree under the key of `expected` in `store` with
/// `expected`, node by node and hash by hash.
fn same_hashes(store: &LigninNodeStore<'_>, expected: &TreeNode, at: &str) -> usize {
    let read = store.get_node(expected.key(), NO_COST, version()).unwrap();
    let read = read.unwrap().expect("the node is committed");
    assert_eq!(read.value_hash(), expected.value_hash(), "{at}");
    assert_eq!(read.kv_hash(), expected.kv_hash(), "{at}");
    assert_eq!(read.feature_type(), expected.feature_type(), "{at}");
    let mut nodes = 1;
    for left in [true, false] {
        let (Some(link), Some(below)) = (read.link(left), expected.link(left)) else {
            assert!(read.link(left).is_none() && expected.link(left).is_none());
            continue;
        };
        assert_eq!(
            held(link),
            held(below),
            "a link under {:?}, {at}",
            read.key()
        );
        nodes += same_hashes(store, below.tree().unwrap(), at);
    }
    nodes
}

#[test]
fn a_modified_child_that_is_not_written_with_its_parent_is_refused() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let fresh = modified(0..3, TreeType::NormalTree).unwrap();

    // The root and its left child: the hash of the right child would be
    // made of whatever is stored under its key.
    let without_right = |store: &LigninNodeStore<'_>| {
        let mut change_set = store.change_set();
        change_set.state_root(Some(fresh.key()));
        for node in [fresh.child(true).unwrap(), &fresh] {
            let prepared = change_set.prepare_node(node, NodeChange::ValueWritten);
            change_set.put_node(node.key(), prepared, None).unwrap();
        }
        let refused = store.commit(change_set).unwrap();
        let refused = refused.expect_err("a child is not written");
        assert!(matches!(refused, Error::StorageError(_)), "{refused:?}");
        assert!(refused.to_string().contains("not written with its parent"));
    };
    let immediate = store(&writer);
    without_right(&immediate);
    let refused = immediate.write_node(&fresh, NodeChange::ValueWritten);
    assert!(matches!(refused.unwrap(), Err(Error::StorageError(_))));
    assert!(!immediate.has_node(fresh.key()).unwrap().unwrap());

    let batch = LigninBatch::new();
    let subtree = LigninSubtree::alone(SUBTREE);
    without_right(&LigninNodeStore::deferred(&writer, subtree, Some(&batch)));
    assert!(batch.is_empty());
    assert_eq!(transaction.stats().stale_nodes, 0);
}

#[test]
fn a_commit_whose_rules_refuse_a_stale_hash_fails_and_ends_the_transaction() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let fresh = modified(0..3, TreeType::NormalTree).unwrap();
    let handle = store(&writer);
    let mut change_set = handle.change_set();
    change_set.state_root(Some(fresh.key()));
    assert_eq!(put_below(&mut change_set, &fresh), 1);
    handle.commit(change_set).unwrap().expect("the write");
    drop(handle);
    writer.into_scope().merge();

    // The rules of a writer that makes every hash itself, which are those
    // of a commit on existing GroveVersions, make none.
    let committer = lignin.db.committer(&lignin.versions);
    let failed = committer.commit(&transaction, &mut NoStaleHashes);
    let failed = failed.expect_err("a hash is stale");
    assert!(!failed.leaves_transaction_open(), "{failed}");
    assert!(failed.to_string().contains("a hash is stale"), "{failed}");
    assert_eq!(transaction.stats().stale_nodes, 0);
    let again = committer.commit(&transaction, &mut MerkHashRules::new(version()));
    assert!(matches!(again, Ok(Committed::Empty)), "{again:?}");
}

#[test]
fn a_commit_makes_the_hashes_merk_left_out_as_merk_would_have() {
    for tree_type in TREE_TYPES {
        let at = format!("{tree_type:?}");
        let lignin = Lignin::new();
        let path = lignin.path();
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());

        // A Merk that no commit of Merk has hashed: every link is
        // modified, so the hash of every child is left to the engine.
        let fresh = modified(0..150, tree_type).unwrap();
        let handle = store(&writer);
        let mut change_set = handle.change_set();
        change_set.state_root(Some(fresh.key()));
        let with_children = put_below(&mut change_set, &fresh);
        handle.commit(change_set).unwrap().expect("the write");
        drop(handle);
        assert_eq!(transaction.stats().stale_nodes, with_children, "{at}");

        writer.into_scope().merge();
        let mut rules = MerkHashRules::new(version());
        assert_eq!(lignin.commit(&transaction, &mut rules), with_children);
        // One node hash for each link, of two calls of the hash function.
        assert_eq!(rules.cost().hash_node_calls, 149 * 2, "{at}");

        // Every committed hash, heights and aggregate is the one a commit
        // of Merk gives the same tree, and so is the root hash.
        let expected = hashed(fresh);
        let reader = path.begin();
        let committed = LigninNodeStore::read_only(&reader, LigninSubtree::alone(SUBTREE));
        assert_eq!(same_hashes(&committed, &expected, &at), 150);
        let merk = Merk::open_layered_with_root_key(
            committed,
            Some(expected.key().to_vec()),
            tree_type,
            NO_COST,
            version(),
        );
        let merk = merk.unwrap().expect("the Merk opens");
        assert_eq!(
            merk.root_hash().unwrap(),
            expected.hash_for_link(tree_type).unwrap(),
            "{at}"
        );
    }
}

#[test]
fn a_node_that_cannot_be_handed_over_is_refused_when_it_is_put() {
    let lignin = Lignin::new();
    let path = lignin.path();
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().unwrap());
    let handle = store(&writer);
    let above = overflowing();

    let mut change_set = handle.change_set();
    let prepared = change_set.prepare_node(&above, NodeChange::ValueWritten);
    let refused = change_set.put_node(above.key(), prepared, None);
    assert!(matches!(refused, Err(Error::Overflow(_))), "{refused:?}");
    let refused = handle.write_node(&above, NodeChange::ValueWritten);
    assert!(matches!(refused.unwrap(), Err(Error::Overflow(_))));
    assert_eq!(transaction.stats().stale_nodes, 0);
}

struct Random(u64);

impl Random {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

/// Reads the tree under `root` through `store` and checks every hash of
/// every node against the rules. Returns the hash the rules make of the
/// root and the nodes that were read.
fn check_stored(
    rules: &mut MerkHashRules<'_>,
    store: &LigninNodeStore<'_>,
    root: &[u8],
    at: &str,
) -> (CryptoHash, usize) {
    let node = store.get_node(root, NO_COST, version()).unwrap();
    let node = node.unwrap().expect("a node that is linked is stored");
    let mut seen = 1;
    with_cell(&node, &[], |cell| {
        let value = cell.value().inline().expect("a value in its cell");
        assert_eq!(
            &rules.value_hash(cell, value, &[]).unwrap(),
            node.value_hash()
        );
        let kv_hash = rules.kv_hash(cell.key(), cell.value_hash()).unwrap();
        assert_eq!(&kv_hash, node.kv_hash(), "{at}");
    });
    for left in [true, false] {
        if let Some(link) = node.link(left) {
            let (below, nodes) = check_stored(rules, store, link.key(), at);
            assert_eq!(&below, link.hash(), "the hash of a child, {at}");
            seen += nodes;
        }
    }
    (
        with_cell(&node, &[], |cell| rules.node_hash(cell).unwrap()),
        seen,
    )
}

#[test]
fn a_merk_makes_every_hash_itself_and_the_rules_make_the_same() {
    for (n, tree_type) in TREE_TYPES.into_iter().enumerate() {
        let at = format!("{tree_type:?}");
        let lignin = Lignin::new();
        let path = lignin.path();
        let transaction = path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let merk = Merk::open_base(store(&writer), tree_type, NO_COST, version());
        let mut merk = merk.unwrap().expect("the Merk opens");
        let mut random = Random(0x9e37_79b9 + n as u64);
        let mut live = BTreeSet::new();
        for round in 0..12 {
            let mut batch = BTreeMap::new();
            for _ in 0..1 + random.below(30) {
                let k = random.below(160);
                let op = match live.contains(&k) && random.below(4) == 0 {
                    true => Op::Delete,
                    false => Op::Put(item(random.below(1000)), feature(tree_type, k + round)),
                };
                batch.insert(key(k), op);
            }
            for (key, op) in &batch {
                let k = u64::from_be_bytes(key.as_slice().try_into().unwrap());
                match op {
                    Op::Delete => live.remove(&k),
                    _ => live.insert(k),
                };
            }
            let batch: Vec<(Vec<u8>, Op)> = batch.into_iter().collect();
            let applied = merk.apply::<_, Vec<u8>>(&batch, &[], None, version());
            applied.unwrap().expect("the batch applies");

            // Merk left no hash to the engine.
            assert_eq!(transaction.stats().stale_nodes, 0, "{at}");
            let Some(root) = merk.root_key() else {
                assert!(live.is_empty());
                assert_eq!(merk.root_hash().unwrap(), NULL_HASH);
                continue;
            };
            let mut rules = MerkHashRules::new(version());
            let (root_hash, nodes) = check_stored(&mut rules, &merk.storage, &root, &at);
            assert_eq!(nodes, live.len(), "{at}");
            assert_eq!(root_hash, merk.root_hash().unwrap(), "the root hash, {at}");
        }
        assert!(live.len() > 20, "{at}");
        drop(merk);
        // No rule runs at the commit: one that is asked refuses.
        writer.into_scope().merge();
        assert_eq!(lignin.commit(&transaction, &mut NoStaleHashes), 0, "{at}");
    }
}

/// The hash of a grove that Merk would not have given it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Planted {
    /// The value hash of an item.
    Item,
    /// The value hash of the tree, whose Merk has a root.
    Tree,
    /// The key-value hash of the root.
    KeyValue,
    /// The hash of a child in a link of the root.
    Child(Side),
}

/// A subtree that the store of the tests is not made with.
const BELOW: [u8; 32] = [222; 32];
/// A hash that no rule makes.
const WRONG: CryptoHash = [0x5a; 32];

/// Commits a grove as GroveDB writes one, with one hash `planted`: a Merk
/// of five items, a reference and a tree, and the Merk of the tree with
/// items of its own.
fn grove(planted: Option<Planted>) -> Lignin {
    let lignin = Lignin::new();
    {
        let write_path = lignin.path();
        let transaction = write_path.begin();
        let writer = LigninWriter::new(transaction.scope().unwrap());
        let staged = LigninBatch::new();
        let basic = TreeFeatureType::BasicMerkNode;
        let puts = |keys: std::ops::Range<u64>| -> Vec<(Vec<u8>, Op)> {
            keys.map(|n| (key(n), Op::Put(item(n), basic))).collect()
        };

        let below = LigninSubtree {
            prefix: BELOW,
            host: Some(LigninHost {
                subtree: SUBTREE,
                key: key(5),
                hosted: vec![BELOW],
            }),
        };
        let lower = LigninNodeStore::deferred(&writer, below, Some(&staged));
        lower.enter(TreeType::NormalTree).unwrap();
        let lower =
            Merk::open_layered_with_root_key(lower, None, TreeType::NormalTree, NO_COST, version());
        let mut lower = lower.unwrap().expect("the Merk opens");
        let applied = lower.apply::<_, Vec<u8>>(&puts(0..5), &[], None, version());
        applied.unwrap().expect("the batch applies");
        let tree = bytes(&Element::new_tree(lower.root_key()));
        let root_hash = lower.root_hash().unwrap();
        drop(lower);

        let upper = LigninSubtree::alone(SUBTREE);
        let upper = LigninNodeStore::deferred(&writer, upper, Some(&staged));
        let upper = Merk::open_base(upper, TreeType::NormalTree, NO_COST, version());
        let mut upper = upper.unwrap().expect("the Merk opens");
        let mut batch = puts(0..7);
        if planted == Some(Planted::Item) {
            batch[0].1 = Op::PutWithProvidedValueHash(item(0), WRONG, None, basic);
        }
        // The value hash of a reference is its writer's: nothing that a
        // cell holds makes it, whatever it is.
        let reference = bytes(&Element::new_reference(path()));
        batch[1].1 = Op::PutCombinedReference(reference, WRONG, basic);
        let root_below = match planted {
            Some(Planted::Tree) => WRONG,
            _ => root_hash,
        };
        batch[5].1 = Op::PutLayeredReference(tree, 3, root_below, basic);
        let applied = upper.apply::<_, Vec<u8>>(&batch, &[], None, version());
        applied.unwrap().expect("the batch applies");
        let root = upper.root_key().expect("a root");
        assert_eq!(root, key(3));
        drop(upper);
        staged
            .apply(&writer)
            .unwrap()
            .expect("the batch is applied");

        // The root is an item. It is written again with one hash that is
        // not its own, by a write of one node.
        if matches!(planted, Some(Planted::KeyValue | Planted::Child(_))) {
            let store = store(&writer);
            let node = store.get_node(&root, NO_COST, version()).unwrap();
            let node = node.unwrap().expect("the root is stored");
            let link = |side: Side| {
                let mut link = node.link(side == Side::Left).cloned();
                let Some(Link::Reference { hash, .. }) = &mut link else {
                    panic!("a link that was read is a reference");
                };
                if planted == Some(Planted::Child(side)) {
                    *hash = WRONG;
                }
                link
            };
            let kv_hash = match planted == Some(Planted::KeyValue) {
                true => WRONG,
                false => *node.kv_hash(),
            };
            let value = node.value_as_slice().to_vec();
            let (left, right) = (link(Side::Left), link(Side::Right));
            let wrong = TreeNode::from_fields(root, value, kv_hash, left, right, basic).unwrap();
            let written = store.write_node(&wrong, NodeChange::HashOnly);
            written.unwrap().expect("the node is written");
        }
        writer.into_scope().merge();
        assert_eq!(lignin.commit(&transaction, &mut NoStaleHashes), 0);
    }
    lignin
}

/// The cell of the node under `key` in the upper Merk of a grove that is
/// committed.
fn cell_of(lignin: &Lignin, key: &[u8]) -> CellAddress {
    let write_path = lignin.path();
    let transaction = write_path.begin();
    let scope = transaction.scope().unwrap();
    let read = scope.node(Subtree::new(SUBTREE), key, |view| {
        let handle = view.expect("the node is there").handle();
        handle.expect("a committed node has a cell").address()
    });
    scope.rollback();
    read.expect("the node is read")
}

#[test]
fn the_check_of_a_dropped_store_recomputes_the_hashes_of_its_merks() {
    // A grove that Merk hashed is sound, the hash that the writer of its
    // reference stated with it, and every cell of it was checked.
    let sound = grove(None);
    let report = sound.db.verify_still();
    assert!(report.is_sound(), "{report}");
    let census = &report.census;
    assert_eq!((census.live_cells, census.hashed_cells), (12, 12));
    assert_eq!(census.hosted_roots, 1);
    drop(sound);

    // Each hash, with the key of the node whose cell stores it.
    let (left, right) = (Side::Left, Side::Right);
    let planted = [
        (Planted::Child(left), HashField::Child(left), 3),
        (Planted::Child(right), HashField::Child(right), 3),
        (Planted::KeyValue, HashField::KeyValue, 3),
        (Planted::Item, HashField::Value, 0),
        (Planted::Tree, HashField::Value, 5),
    ];
    for (planted, field, stored_at) in planted {
        let lignin = grove(Some(planted));
        // A check that is given no rules recomputes no hash and finds
        // nothing.
        let at = lignin.versions.latest();
        let unhashed = lignin.db.verify(at, Scope::Still, None);
        assert!(unhashed.is_sound(), "{planted:?}: {unhashed}");
        assert_eq!(unhashed.census.hashed_cells, 0);

        // With Merk's rules it finds the hash, once, where it is stored.
        let report = lignin.db.verify_still();
        let found = |finding: &Finding| (finding.rule, finding.place);
        let found: Vec<_> = report.findings.iter().map(found).collect();
        let cell = Place::Cell(cell_of(&lignin, &key(stored_at)));
        assert_eq!(found, [(Rule::Hash(field), cell)], "{planted:?}: {report}");
        assert_eq!(report.more_findings, 0, "{planted:?}");

        // So the store of a test that leaves such a hash fails when it is
        // dropped, where the store is checked at its drop, and says which
        // rule.
        if cfg!(debug_assertions) {
            let dropped = catch_unwind(AssertUnwindSafe(move || drop(lignin)));
            let failure = dropped.expect_err("the store is not sound");
            let failure = failure.downcast_ref::<String>().expect("a message");
            let rule = Rule::Hash(field).text();
            assert!(failure.contains(rule), "{planted:?}: {failure}");
        }
    }
}
