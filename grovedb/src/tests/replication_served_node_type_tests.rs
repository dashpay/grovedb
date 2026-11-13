//! Serving-side checks on every node of a chunk, not only the root.
//!
//! The prefix, root key and tree type in a global chunk id all come from the
//! requesting peer. `replication::ensure_served_merk_family` compares the
//! family of the node the root key names with the requested type, which is
//! enough when the prefix holds a Merk: every node of a Merk shares one
//! family. It is not enough when the prefix holds something else. The
//! non-Merk append-only trees keep raw entries in their data namespace, an
//! entry can have the byte layout of a stored Merk node, and such entries can
//! link to each other. A request naming one of them as the root of a
//! `Provable*` Merk passed the root check and then reached a linked entry of
//! another family, which `TreeNode::hash_for_link` answers with a `panic!`.
//!
//! The chunk producer now checks every node it emits or hashes and hashes
//! fallibly, so these requests are refused with an error.

use grovedb_merk::{
    tree::{AggregateData, Link, TreeNode},
    tree_type::TreeType,
    TreeFeatureType,
};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::version::GroveVersion;

use crate::{
    replication::{
        indexed_sync::IndexedHeaderRequest,
        utils::{encode_global_chunk_id, pack_nested_bytes},
    },
    tests::{
        make_empty_grovedb,
        replication_serving_tree_type_tests::{honest_global_chunk_ids, serve, Served},
        TempGroveDb,
    },
    Element, GroveDb,
};

/// The message the chunk producer refuses a node of another family with.
const NODE_REFUSAL: &str = "does not belong to the tree type";

/// Height of the dense trees, and chunk power of the bulk append trees, the
/// entries are stored in: 15 entries fit before a bulk append tree compacts
/// its buffer.
const HOST_HEIGHT: u8 = 4;

/// The non-Merk tree whose data namespace holds the entries. Both keep an
/// entry as its raw bytes under its two-byte position: a bulk append tree
/// buffers its entries in a dense tree until the buffer fills.
#[derive(Debug, Clone, Copy)]
enum Host {
    Dense,
    BulkAppend,
}

/// The storage key of the dense tree entry at `position`.
fn entry_key(position: u16) -> Vec<u8> {
    position.to_be_bytes().to_vec()
}

/// The aggregate a link to a node of `feature` carries.
fn link_aggregate(feature: TreeFeatureType) -> AggregateData {
    match feature {
        TreeFeatureType::BasicMerkNode => AggregateData::NoAggregateData,
        TreeFeatureType::SummedMerkNode(sum) => AggregateData::Sum(sum),
        TreeFeatureType::BigSummedMerkNode(sum) => AggregateData::BigSum(sum),
        TreeFeatureType::CountedMerkNode(count) => AggregateData::Count(count),
        TreeFeatureType::CountedSummedMerkNode(count, sum) => {
            AggregateData::CountAndSum(count, sum)
        }
        TreeFeatureType::ProvableCountedMerkNode(count) => AggregateData::ProvableCount(count),
        TreeFeatureType::ProvableCountedSummedMerkNode(count, sum) => {
            AggregateData::ProvableCountAndSum(count, sum)
        }
        TreeFeatureType::ProvableSummedMerkNode(sum) => AggregateData::ProvableSum(sum),
        TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum) => {
            AggregateData::ProvableCountAndProvableSum(count, sum)
        }
    }
}

/// Entries with the byte layout of stored Merk nodes, forming a chain: the
/// entry at position `i` carries `features[i]` and a left link to the entry
/// at position `i + 1`.
fn chained_entries(features: &[TreeFeatureType]) -> Vec<Vec<u8>> {
    let len = features.len();
    (0..len)
        .map(|i| {
            let left = (i + 1 < len).then(|| Link::Reference {
                hash: [7u8; 32],
                // The child at `i + 1` heads a left chain of `len - i - 1`
                // nodes.
                child_heights: ((len - i - 2) as u8, 0),
                key: entry_key(i as u16 + 1),
                aggregate_data: link_aggregate(features[i + 1]),
            });
            TreeNode::from_fields(
                entry_key(i as u16),
                vec![i as u8; 8],
                [9u8; 32],
                left,
                None,
                features[i],
            )
            .unwrap()
            .encode()
        })
        .collect()
}

/// Inserts a tree of kind `host` at `key` under the grove root holding
/// `entries`.
fn insert_host_tree(
    db: &GroveDb,
    host: Host,
    key: &[u8],
    entries: Vec<Vec<u8>>,
    grove_version: &GroveVersion,
) {
    let root: &[&[u8]] = &[];
    let element = match host {
        Host::Dense => Element::empty_dense_tree(HOST_HEIGHT),
        Host::BulkAppend => {
            Element::empty_bulk_append_tree(HOST_HEIGHT).expect("valid chunk power")
        }
    };
    db.insert(root, key, element, None, None, grove_version)
        .unwrap()
        .expect("insert host tree");
    for entry in entries {
        match host {
            Host::Dense => db
                .dense_tree_insert(root, key, entry, None, grove_version)
                .unwrap()
                .map(drop)
                .expect("dense insert"),
            Host::BulkAppend => db
                .bulk_append(root, key, entry, None, grove_version)
                .unwrap()
                .map(drop)
                .expect("bulk append"),
        }
    }
}

/// A request for the subtree at `key`, addressed by the entry at position 0
/// as its root, under `tree_type`.
fn request_for(key: &[u8], tree_type: TreeType, locals: Vec<Vec<u8>>) -> Vec<u8> {
    let path: &[&[u8]] = &[key];
    let prefix = RocksDbStorage::build_prefix(SubtreePath::from(path)).unwrap();
    pack_nested_bytes(vec![encode_global_chunk_id(
        prefix,
        Some(entry_key(0)),
        tree_type,
        locals,
    )
    .expect("encode global chunk id")])
    .expect("pack request")
}

/// An indexed header request with one axis and no secondary root key.
fn header_request() -> Vec<u8> {
    IndexedHeaderRequest {
        axes: vec![(0, None)],
    }
    .encode()
}

/// The four provable families: the feature type their nodes carry and every
/// tree type whose nodes are of that family.
fn provable_families() -> Vec<(&'static str, TreeFeatureType, Vec<TreeType>)> {
    vec![
        (
            "count",
            TreeFeatureType::ProvableCountedMerkNode(1),
            vec![
                TreeType::ProvableCountTree,
                TreeType::ProvableCountIndexedTree,
            ],
        ),
        (
            "count_sum",
            TreeFeatureType::ProvableCountedSummedMerkNode(1, 1),
            vec![TreeType::ProvableCountSumTree],
        ),
        (
            "sum",
            TreeFeatureType::ProvableSummedMerkNode(1),
            vec![TreeType::ProvableSumTree, TreeType::ProvableSumIndexedTree],
        ),
        (
            "count_provable_sum",
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(1, 1),
            vec![
                TreeType::ProvableCountProvableSumTree,
                TreeType::ProvableCountProvableSumIndexedTree,
            ],
        ),
    ]
}

/// One node of every family.
fn every_feature() -> Vec<(&'static str, TreeFeatureType)> {
    vec![
        ("basic", TreeFeatureType::BasicMerkNode),
        ("summed", TreeFeatureType::SummedMerkNode(1)),
        ("big_summed", TreeFeatureType::BigSummedMerkNode(1)),
        ("counted", TreeFeatureType::CountedMerkNode(1)),
        (
            "counted_summed",
            TreeFeatureType::CountedSummedMerkNode(1, 1),
        ),
        ("p_count", TreeFeatureType::ProvableCountedMerkNode(1)),
        (
            "p_count_sum",
            TreeFeatureType::ProvableCountedSummedMerkNode(1, 1),
        ),
        ("p_sum", TreeFeatureType::ProvableSummedMerkNode(1)),
        (
            "p_count_p_sum",
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(1, 1),
        ),
    ]
}

/// A grove with one tree of kind `host` per `(key, entries)` pair.
fn grove_with_host_trees(
    host: Host,
    trees: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
    grove_version: &GroveVersion,
) -> TempGroveDb {
    let db = make_empty_grovedb();
    for (key, entries) in trees {
        insert_host_tree(&db, host, &key, entries, grove_version);
    }
    db
}

/// A grove with one dense tree per `(key, entries)` pair.
fn grove_with_dense_trees(
    trees: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
    grove_version: &GroveVersion,
) -> TempGroveDb {
    grove_with_host_trees(Host::Dense, trees, grove_version)
}

fn assert_refused(served: Served, needle: &str, case: &str) {
    match served {
        Served::Err(message) => assert!(
            message.contains(needle),
            "{case}: expected an error containing {needle:?}, got {message}"
        ),
        other => panic!("{case}: expected an error, got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reproduction: a dense tree whose first two entries read as
    /// `ProvableCountedMerkNode` nodes and whose third reads as a
    /// `BasicMerkNode`, requested as a `ProvableCountTree` rooted at entry 0.
    /// The root entry is of the named family, so the root check passes; the
    /// root chunk of the four-node chain ends at the third entry, which is
    /// hashed under the requested type. A bulk append tree buffers its
    /// entries the same way and is refused the same way.
    #[test]
    fn should_refuse_non_merk_entries_requested_as_a_provable_count_tree() {
        let grove_version = GroveVersion::latest();
        let pc = TreeFeatureType::ProvableCountedMerkNode(1);
        let basic = TreeFeatureType::BasicMerkNode;
        for host in [Host::Dense, Host::BulkAppend] {
            let db = grove_with_host_trees(
                host,
                vec![(b"host".to_vec(), chained_entries(&[pc, pc, basic, basic]))],
                grove_version,
            );

            let served = serve(
                &db,
                &request_for(b"host", TreeType::ProvableCountTree, vec![]),
                grove_version,
            );
            assert_refused(
                served,
                NODE_REFUSAL,
                &format!("{host:?} entries as ProvableCountTree"),
            );
        }
    }

    /// Every provable family, on every tree type of that family, with a
    /// linked entry of every other family, at a depth where the chunk hashes
    /// it (third in the chain) and at a depth where the chunk emits it
    /// (second in the chain).
    #[test]
    fn should_refuse_a_mismatched_node_in_each_provable_family() {
        let grove_version = GroveVersion::latest();
        let mut trees = Vec::new();
        let mut cases = Vec::new();
        for (family_name, family_feature, tree_types) in provable_families() {
            for (other_name, other_feature) in every_feature() {
                if other_feature.node_type() == family_feature.node_type() {
                    continue;
                }
                let hashed_key = format!("{family_name}/{other_name}/hashed").into_bytes();
                trees.push((
                    hashed_key.clone(),
                    chained_entries(&[
                        family_feature,
                        family_feature,
                        other_feature,
                        other_feature,
                    ]),
                ));
                let emitted_key = format!("{family_name}/{other_name}/emitted").into_bytes();
                trees.push((
                    emitted_key.clone(),
                    chained_entries(&[family_feature, other_feature]),
                ));
                for tree_type in &tree_types {
                    cases.push((hashed_key.clone(), *tree_type));
                    cases.push((emitted_key.clone(), *tree_type));
                }
            }
        }
        let db = grove_with_dense_trees(trees, grove_version);

        for (key, tree_type) in cases {
            let case = format!("{} as {tree_type:?}", String::from_utf8_lossy(&key));
            assert_refused(
                serve(&db, &request_for(&key, tree_type, vec![]), grove_version),
                NODE_REFUSAL,
                &case,
            );
            // A deeper chunk id recovers to the same root chunk or walks to
            // the mismatched node; neither may be served.
            for locals in [vec![vec![1u8]], vec![vec![1u8, 1]]] {
                match serve(&db, &request_for(&key, tree_type, locals), grove_version) {
                    Served::Err(_) => {}
                    other => panic!("{case} (deeper id): expected an error, got {other:?}"),
                }
            }
            // The header route hashes the root before it builds the root
            // chunk, so it refuses either there (the root's aggregate cannot
            // be computed from a child of some families) or in the chunk.
            if tree_type.is_indexed_primary() {
                match serve(
                    &db,
                    &request_for(&key, tree_type, vec![header_request()]),
                    grove_version,
                ) {
                    Served::Err(_) => {}
                    other => panic!("{case} (header): expected an error, got {other:?}"),
                }
            }
        }
    }

    /// A node of the named family whose aggregate cannot be computed (its own
    /// count plus its child's overflows) is an error wherever it is hashed:
    /// at the boundary of a chunk and as the root of an indexed header. Where
    /// a chunk only emits such a node nothing is hashed; the request is
    /// answered without a panic and the target's own checks decide.
    #[test]
    fn should_refuse_to_hash_a_node_whose_aggregate_overflows() {
        let grove_version = GroveVersion::latest();
        let pc = TreeFeatureType::ProvableCountedMerkNode(1);
        let pc_max = TreeFeatureType::ProvableCountedMerkNode(u64::MAX);
        let db = grove_with_dense_trees(
            vec![
                (b"hashed".to_vec(), chained_entries(&[pc, pc, pc_max, pc])),
                (b"root".to_vec(), chained_entries(&[pc_max, pc])),
            ],
            grove_version,
        );

        for (key, tree_type, locals) in [
            (b"hashed".as_slice(), TreeType::ProvableCountTree, vec![]),
            (
                b"root",
                TreeType::ProvableCountIndexedTree,
                vec![header_request()],
            ),
        ] {
            let case = format!("{} as {tree_type:?}", String::from_utf8_lossy(key));
            match serve(&db, &request_for(key, tree_type, locals), grove_version) {
                Served::Err(_) => {}
                other => panic!("{case}: expected an error, got {other:?}"),
            }
        }
        let emitted = serve(
            &db,
            &request_for(b"root", TreeType::ProvableCountTree, vec![]),
            grove_version,
        );
        assert!(
            !matches!(emitted, Served::Panic(_)),
            "an emitted node must not panic: {emitted:?}"
        );
    }

    /// A link to an entry that is not a stored node, or to no entry at all,
    /// is an error.
    #[test]
    fn should_refuse_a_link_to_a_row_that_is_not_a_node() {
        let grove_version = GroveVersion::latest();
        let pc = TreeFeatureType::ProvableCountedMerkNode(1);
        // Entry 1 is plain bytes; entry 0 links to it.
        let mut undecodable = chained_entries(&[pc, pc]);
        undecodable[1] = vec![0xFF; 16];
        // Entry 0 links to position 1, which was never written.
        let mut dangling = chained_entries(&[pc, pc]);
        dangling.truncate(1);
        let db = grove_with_dense_trees(
            vec![
                (b"undecodable".to_vec(), undecodable),
                (b"dangling".to_vec(), dangling),
            ],
            grove_version,
        );

        for key in [b"undecodable".as_slice(), b"dangling"] {
            let case = String::from_utf8_lossy(key).into_owned();
            match serve(
                &db,
                &request_for(key, TreeType::ProvableCountTree, vec![]),
                grove_version,
            ) {
                Served::Err(_) => {}
                other => panic!("{case}: expected an error, got {other:?}"),
            }
        }
    }

    /// Entries that all read as nodes of the named family are served: the
    /// check refuses a mismatch, not a root key the source did not hand out.
    /// What the target does with such a chunk is decided by its own hash
    /// checks.
    #[test]
    fn should_serve_entries_that_all_match_the_named_type() {
        let grove_version = GroveVersion::latest();
        let pc = TreeFeatureType::ProvableCountedMerkNode(1);
        let db = grove_with_dense_trees(
            vec![(b"dense".to_vec(), chained_entries(&[pc, pc, pc, pc]))],
            grove_version,
        );
        assert!(matches!(
            serve(
                &db,
                &request_for(b"dense", TreeType::ProvableCountTree, vec![]),
                grove_version,
            ),
            Served::Ok
        ));
    }

    /// An honest sync of a grove holding provable trees next to non-Merk
    /// trees with node-shaped entries still completes: the target pages the
    /// non-Merk trees' entries and never asks for them as Merk chunks.
    #[test]
    fn should_complete_an_honest_sync_next_to_node_shaped_entries() {
        let grove_version = GroveVersion::latest();
        let pc = TreeFeatureType::ProvableCountedMerkNode(1);
        let basic = TreeFeatureType::BasicMerkNode;
        let db = grove_with_dense_trees(
            vec![(b"dense".to_vec(), chained_entries(&[pc, pc, basic, basic]))],
            grove_version,
        );
        insert_host_tree(
            &db,
            Host::BulkAppend,
            b"bulk",
            chained_entries(&[pc, pc, basic, basic]),
            grove_version,
        );
        let root: &[&[u8]] = &[];
        for (key, tree, sum_children) in [
            (
                b"pc".as_slice(),
                Element::empty_provable_count_tree(),
                false,
            ),
            (b"pcs", Element::empty_provable_count_sum_tree(), true),
            (b"ps", Element::empty_provable_sum_tree(), true),
            (
                b"pcps",
                Element::empty_provable_count_provable_sum_tree(),
                true,
            ),
        ] {
            db.insert(root, key, tree, None, None, grove_version)
                .unwrap()
                .expect("insert provable tree");
            for i in 0..32u8 {
                let child = if sum_children {
                    Element::new_sum_item(i as i64 - 7)
                } else {
                    Element::new_item(vec![i])
                };
                db.insert([key].as_ref(), &[i], child, None, None, grove_version)
                    .unwrap()
                    .expect("insert child");
            }
        }

        // Syncs into a fresh grove and asserts completion and the commit.
        let ids = honest_global_chunk_ids(&db, grove_version);
        assert!(!ids.is_empty(), "the honest sync must request subtrees");
    }
}
