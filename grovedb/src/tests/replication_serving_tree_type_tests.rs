//! Serving-side tree-type checks for state sync.
//!
//! `GroveDb::fetch_chunk` answers untrusted peers, and every global chunk id
//! carries a tree-type byte the peer chose. The source opens the addressed
//! subtree with that type as-is. Naming a `Provable*` or indexed type for a
//! subtree whose nodes belong to another aggregate family used to drive chunk
//! generation, or the indexed header's `root_hash`, into the fail-closed
//! `panic!` in `TreeNode::hash_for_link`: one request crashed the serving
//! node. The source now compares the stored family with the named type before
//! anything is hashed (`replication::ensure_served_merk_family`).
//!
//! The tests replay every global chunk id an honest target sends while
//! syncing a grove that holds one populated subtree of every kind, plus every
//! top-level subtree and the grove root, with each tree-type byte swapped in,
//! with the root key kept or dropped, and with extra local ids of every
//! request shape. Every such request must be answered with `Ok` or an `Err`,
//! never a panic; the honest request must still be served; and a Merk subtree
//! named with a type of another family must be refused descriptively. The
//! serving path is not version-gated, so the matrix runs under `GROVE_V3` as
//! well as the latest version.

use std::{
    collections::{BTreeMap, VecDeque},
    panic::{catch_unwind, AssertUnwindSafe},
};

use grovedb_merk::{element::tree_type::ElementTreeTypeExtensions, tree_type::TreeType};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::version::GroveVersion;

use crate::{
    replication::{
        indexed_sync::IndexedHeaderRequest,
        non_merk_sync::{supports_entry_replay, NonMerkChunkId},
        utils::{
            decode_global_chunk_id, encode_global_chunk_id, pack_nested_bytes, unpack_nested_bytes,
        },
        CURRENT_STATE_SYNC_VERSION,
    },
    tests::{make_empty_grovedb, TempGroveDb},
    Element, GroveDb, SubtreePrefix,
};

/// Children per plain Merk subtree: enough for a height where the root chunk
/// ends in `Hash` boundaries, which is where chunk generation hashes nodes
/// under the requested type.
const CHILDREN_PER_MERK: u8 = 32;

/// Top-level subtree keys of [`every_subtree_kind_source`].
pub(super) const SUBTREE_KEYS: &[&[u8]] = &[
    b"normal",
    b"sum",
    b"big_sum",
    b"count",
    b"count_sum",
    b"pc",
    b"pcs",
    b"ps",
    b"pcps",
    b"empty",
    b"pcit",
    b"psit",
    b"pcpsit",
    b"ct",
    b"mmr",
    b"bulk",
    b"dense",
    b"pds",
];

/// The message `ensure_served_merk_family` refuses a mismatched type with.
pub(super) const FAMILY_REFUSAL: &str = "does not belong to the subtree";

/// A grove holding one populated subtree of every kind a source serves: the
/// plain and aggregate Merks, an empty tree, the three indexed trees (their
/// primaries and axis secondaries), and every non-Merk append-only tree.
pub(super) fn every_subtree_kind_source(grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_empty_grovedb();
    let root: &[&[u8]] = &[];
    let insert_subtree = |key: &[u8], element: Element| {
        db.insert(root, key, element, None, None, grove_version)
            .unwrap()
            .expect("insert subtree");
    };

    for (key, tree, sum_children) in [
        (b"normal".as_slice(), Element::empty_tree(), false),
        (b"sum", Element::empty_sum_tree(), true),
        (b"big_sum", Element::empty_big_sum_tree(), true),
        (b"count", Element::empty_count_tree(), false),
        (b"count_sum", Element::empty_count_sum_tree(), true),
        (b"pc", Element::empty_provable_count_tree(), false),
        (b"pcs", Element::empty_provable_count_sum_tree(), true),
        (b"ps", Element::empty_provable_sum_tree(), true),
        (
            b"pcps",
            Element::empty_provable_count_provable_sum_tree(),
            true,
        ),
    ] {
        insert_subtree(key, tree);
        for i in 0..CHILDREN_PER_MERK {
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
    insert_subtree(b"empty", Element::empty_tree());

    insert_subtree(b"pcit", Element::empty_provable_count_indexed_tree());
    insert_subtree(b"psit", Element::empty_provable_sum_indexed_tree());
    insert_subtree(
        b"pcpsit",
        Element::empty_provable_count_provable_sum_indexed_tree(vec![
            (0, None),
            (1, None),
            (2, None),
        ])
        .expect("canonical axes"),
    );
    for i in 0..16u8 {
        db.insert_into_count_indexed_tree(
            [b"pcit"].as_ref(),
            &[i],
            Element::new_item(vec![i]),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCIT entry");
        db.insert_into_provable_sum_indexed_tree(
            [b"psit"].as_ref(),
            &[i],
            Element::new_sum_item(i as i64 - 5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PSIT entry");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [b"pcpsit"].as_ref(),
            &[i],
            Element::new_item_with_sum_item(vec![i], i as i64 - 5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCPSIT entry");
    }

    insert_subtree(
        b"ct",
        Element::empty_commitment_tree(4).expect("valid chunk power"),
    );
    db.commitment_tree_insert_raw(
        root,
        b"ct",
        [1u8; 32],
        [2u8; 32],
        [3u8; 32],
        vec![0u8; 216],
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree note");
    insert_subtree(b"mmr", Element::empty_mmr_tree());
    insert_subtree(
        b"bulk",
        Element::empty_bulk_append_tree(2).expect("valid chunk power"),
    );
    insert_subtree(b"dense", Element::empty_dense_tree(4));
    insert_subtree(
        b"pds",
        Element::empty_private_document_store(16, 2).expect("valid config"),
    );
    for i in 0..10u8 {
        db.mmr_tree_append(root, b"mmr", vec![i; 8], None, grove_version)
            .unwrap()
            .expect("append MMR leaf");
        db.bulk_append(root, b"bulk", vec![i; 8], None, grove_version)
            .unwrap()
            .expect("bulk append");
        db.private_document_store_insert(root, b"pds", vec![i; 16], None, grove_version)
            .unwrap()
            .expect("insert private document");
        if i < 3 {
            db.dense_tree_insert(root, b"dense", vec![i; 32], None, grove_version)
                .unwrap()
                .expect("dense insert");
        }
    }
    db
}

/// Syncs `source` into a fresh grove and returns every global chunk id the
/// honest target requested, unpacked, in request order. Every request is
/// served and applied successfully along the way.
pub(super) fn honest_global_chunk_ids(
    source: &GroveDb,
    grove_version: &GroveVersion,
) -> Vec<Vec<u8>> {
    let app_hash = source
        .root_hash(None, grove_version)
        .unwrap()
        .expect("source root hash");
    let dest = make_empty_grovedb();
    let mut session = dest
        .start_snapshot_syncing(app_hash, 64, CURRENT_STATE_SYNC_VERSION, grove_version)
        .expect("start honest sync");
    let mut queue = VecDeque::from([app_hash.to_vec()]);
    let mut ids = Vec::new();
    while let Some(request) = queue.pop_front() {
        if request.as_slice() != app_hash.as_slice() {
            ids.extend(unpack_nested_bytes(&request).expect("honest request unpacks"));
        }
        let response = source
            .fetch_chunk(&request, None, CURRENT_STATE_SYNC_VERSION, grove_version)
            .expect("honest request is served");
        queue.extend(
            session
                .apply_chunk(
                    &request,
                    &response,
                    CURRENT_STATE_SYNC_VERSION,
                    grove_version,
                )
                .expect("honest chunk applies"),
        );
    }
    assert!(session.is_sync_completed(), "honest sync must complete");
    dest.commit_session(session, grove_version)
        .expect("honest sync commits");
    ids
}

/// One subtree as a peer can address it: prefix and the root key the
/// honest target names, with the tree type its element carries.
#[derive(Debug, Clone)]
pub(super) struct ServedTarget {
    pub prefix: SubtreePrefix,
    pub root_key: Option<Vec<u8>>,
    pub honest_type: TreeType,
    /// Local chunk ids the honest target sent for this subtree, one entry per
    /// global chunk id.
    pub honest_locals: Vec<Vec<Vec<u8>>>,
}

/// Every subtree of `source` a peer can address: the targets of `honest_ids`
/// with their honest local ids, every top-level subtree of
/// [`every_subtree_kind_source`] (including the empty and non-Merk ones the
/// target only asks pages of), and the grove root by prefix.
pub(super) fn served_targets(
    source: &GroveDb,
    honest_ids: &[Vec<u8>],
    grove_version: &GroveVersion,
) -> Vec<ServedTarget> {
    let app_hash = source
        .root_hash(None, grove_version)
        .unwrap()
        .expect("source root hash");
    let mut targets: BTreeMap<(SubtreePrefix, Option<Vec<u8>>), ServedTarget> = BTreeMap::new();
    let mut add = |prefix, root_key: Option<Vec<u8>>, honest_type, locals: Option<Vec<Vec<u8>>>| {
        let target = targets
            .entry((prefix, root_key.clone()))
            .or_insert_with(|| ServedTarget {
                prefix,
                root_key,
                honest_type,
                honest_locals: Vec::new(),
            });
        // The wire carries only the discriminant, so a non-Merk type decodes
        // without the parameter its element holds.
        assert_eq!(
            target.honest_type.discriminant(),
            honest_type.discriminant(),
            "one type per subtree"
        );
        target.honest_locals.extend(locals);
    };

    for id in honest_ids {
        let (prefix, root_key, tree_type, locals) =
            decode_global_chunk_id(id, &app_hash).expect("honest id decodes");
        add(prefix, root_key, tree_type, Some(locals));
    }

    let root: &[&[u8]] = &[];
    for key in SUBTREE_KEYS {
        let element = source
            .get(root, key, None, grove_version)
            .unwrap()
            .expect("fixture subtree");
        let (root_key, tree_type) = element
            .root_key_and_tree_type_owned()
            .expect("fixture element is a tree");
        let path: &[&[u8]] = &[key];
        let prefix = RocksDbStorage::build_prefix(SubtreePath::from(path)).unwrap();
        add(prefix, root_key, tree_type, None);
    }

    let tx = source.start_transaction();
    let grove_root_key = source
        .open_transactional_merk_by_prefix(
            SubtreePrefix::default(),
            None,
            TreeType::NormalTree,
            &tx,
            None,
            grove_version,
        )
        .unwrap()
        .expect("open grove root")
        .root_key();
    assert!(grove_root_key.is_some(), "the fixture grove is not empty");
    add(
        SubtreePrefix::default(),
        grove_root_key,
        TreeType::NormalTree,
        None,
    );

    targets.into_values().collect()
}

/// Local-id shapes every tree-type byte is tried with on every subtree, on
/// top of the target's honest ones: the Merk root chunk, a deeper traversal
/// instruction, an indexed header request, and non-Merk page cursors of
/// several shapes (in range, past the end, and at the extreme values).
pub(super) fn synthetic_locals() -> Vec<Vec<Vec<u8>>> {
    let cursor = |start, state, param| NonMerkChunkId {
        start,
        state,
        param,
    };
    vec![
        vec![],
        vec![vec![1]],
        vec![vec![0, 1]],
        vec![IndexedHeaderRequest {
            axes: vec![(0, None)],
        }
        .encode()],
        vec![cursor(0, 10, 2).encode()],
        vec![cursor(3, 3, 4).encode()],
        vec![cursor(0, u64::MAX, u8::MAX).encode()],
    ]
}

/// A global chunk id in the documented wire layout with an arbitrary
/// tree-type byte, including bytes that decode to no tree type.
pub(super) fn global_chunk_id_with_type_byte(
    prefix: SubtreePrefix,
    root_key: Option<Vec<u8>>,
    type_byte: u8,
    locals: Vec<Vec<u8>>,
) -> Vec<u8> {
    let type_offset = 32 + 2 + root_key.as_ref().map_or(0, Vec::len);
    let mut id = encode_global_chunk_id(prefix, root_key, TreeType::NormalTree, locals)
        .expect("encode global chunk id");
    id[type_offset] = type_byte;
    id
}

/// How the source answered one request.
#[derive(Debug)]
pub(super) enum Served {
    Ok,
    Err(String),
    Panic(String),
}

/// Serves `request` as `fetch_chunk` would for a remote peer, catching a
/// panic instead of unwinding through the test.
pub(super) fn serve(source: &GroveDb, request: &[u8], grove_version: &GroveVersion) -> Served {
    match catch_unwind(AssertUnwindSafe(|| {
        source
            .fetch_chunk(request, None, CURRENT_STATE_SYNC_VERSION, grove_version)
            .map(drop)
            .map_err(|e| e.to_string())
    })) {
        Ok(Ok(())) => Served::Ok,
        Ok(Err(message)) => Served::Err(message),
        Err(payload) => Served::Panic(
            payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<non-string panic payload>".to_string()),
        ),
    }
}

/// Whether the source must refuse `type_byte` for `target` addressed by
/// `root_key` on family grounds: the subtree is a non-empty Merk addressed
/// by its real root key, and the byte names a Merk type of another family.
pub(super) fn must_refuse_family(
    target: &ServedTarget,
    root_key: &Option<Vec<u8>>,
    type_byte: u8,
) -> bool {
    let Ok(named) = TreeType::try_from(type_byte) else {
        return false;
    };
    root_key.is_some()
        && *root_key == target.root_key
        && !supports_entry_replay(target.honest_type)
        && !supports_entry_replay(named)
        && named.inner_node_type() != target.honest_type.inner_node_type()
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use super::*;

    /// The audited case in isolation: a plain `NormalTree` subtree tall
    /// enough for `Hash` boundaries in its root chunk, named as each
    /// `Provable*` type (plain chunk route) and each indexed type with a
    /// header request (header route). Every one used to panic in
    /// `hash_for_link`; every one is now refused with the family error, and
    /// the source keeps serving the honest request afterwards.
    #[test]
    fn fetch_chunk_refuses_provable_and_indexed_types_for_a_plain_subtree() {
        let source = every_subtree_kind_source(GroveVersion::latest());
        for grove_version in [GroveVersion::latest(), &GROVE_V3] {
            let root: &[&[u8]] = &[];
            let (root_key, honest_type) = source
                .get(root, b"normal", None, grove_version)
                .unwrap()
                .expect("normal subtree")
                .root_key_and_tree_type_owned()
                .expect("a tree");
            assert_eq!(honest_type, TreeType::NormalTree);
            let path: &[&[u8]] = &[b"normal"];
            let prefix = RocksDbStorage::build_prefix(SubtreePath::from(path)).unwrap();
            let request = |type_byte: u8, locals: Vec<Vec<u8>>| {
                pack_nested_bytes(vec![global_chunk_id_with_type_byte(
                    prefix,
                    root_key.clone(),
                    type_byte,
                    locals,
                )])
                .expect("pack request")
            };
            let header = IndexedHeaderRequest {
                axes: vec![(0, None)],
            }
            .encode();

            let cases = [5u8, 6, 11, 12]
                .into_iter()
                .map(|byte| (byte, vec![]))
                .chain(
                    [13u8, 14, 15]
                        .into_iter()
                        .map(|byte| (byte, vec![header.clone()])),
                );
            for (type_byte, locals) in cases {
                match serve(&source, &request(type_byte, locals), grove_version) {
                    Served::Err(message) => assert!(
                        message.contains(FAMILY_REFUSAL),
                        "type byte {type_byte}: expected the family refusal, got {message}"
                    ),
                    other => panic!("type byte {type_byte}: expected a refusal, got {other:?}"),
                }
            }
            assert!(
                matches!(
                    serve(&source, &request(0, vec![]), grove_version),
                    Served::Ok
                ),
                "the honest request must still be served"
            );
        }
    }

    /// The full matrix: every subtree kind (as the honest target addresses
    /// it, and by prefix), every tree-type byte, with the root key kept or
    /// dropped, and every local-id shape. No request may panic; a Merk
    /// subtree named with a type of another family is refused with the
    /// family error.
    #[test]
    fn fetch_chunk_answers_every_tree_type_byte_for_every_subtree_kind() {
        let source = every_subtree_kind_source(GroveVersion::latest());
        let honest_ids = honest_global_chunk_ids(&source, GroveVersion::latest());
        let targets = served_targets(&source, &honest_ids, GroveVersion::latest());
        assert!(
            targets
                .iter()
                .any(|t| t.honest_type.is_indexed_primary() && !t.honest_locals.is_empty()),
            "the honest sync must address an indexed primary"
        );
        assert!(
            targets
                .iter()
                .any(|t| supports_entry_replay(t.honest_type) && !t.honest_locals.is_empty()),
            "the honest sync must page a non-Merk subtree"
        );

        for grove_version in [GroveVersion::latest(), &GROVE_V3] {
            let mut panics = Vec::new();
            let mut refusals = 0usize;
            for target in &targets {
                let mut root_keys = vec![target.root_key.clone()];
                if target.root_key.is_some() {
                    root_keys.push(None);
                }
                let shapes: Vec<Vec<Vec<u8>>> = target
                    .honest_locals
                    .iter()
                    .cloned()
                    .chain(synthetic_locals())
                    .collect();
                for root_key in &root_keys {
                    for type_byte in (0..=16u8).chain([17, 128, u8::MAX]) {
                        for locals in &shapes {
                            let request = pack_nested_bytes(vec![global_chunk_id_with_type_byte(
                                target.prefix,
                                root_key.clone(),
                                type_byte,
                                locals.clone(),
                            )])
                            .expect("pack request");
                            let served = serve(&source, &request, grove_version);
                            let case = format!(
                                "{:?} subtree {} (root key {}), type byte {type_byte}, locals \
                                 {locals:?}",
                                target.honest_type,
                                hex::encode(target.prefix),
                                if root_key.is_some() {
                                    "kept"
                                } else {
                                    "dropped"
                                },
                            );
                            match served {
                                Served::Panic(message) => panics.push(format!("{case}: {message}")),
                                Served::Err(message)
                                    if must_refuse_family(target, root_key, type_byte) =>
                                {
                                    assert!(
                                        message.contains(FAMILY_REFUSAL),
                                        "{case}: expected the family refusal, got {message}"
                                    );
                                    refusals += 1;
                                }
                                Served::Ok if must_refuse_family(target, root_key, type_byte) => {
                                    panic!("{case}: a mismatched family must be refused")
                                }
                                Served::Ok if type_byte > 16 => {
                                    panic!("{case}: an unknown tree type must be refused")
                                }
                                Served::Ok | Served::Err(_) => {}
                            }
                        }
                    }
                }
                // The honest ids themselves are still served.
                for locals in &target.honest_locals {
                    let request = pack_nested_bytes(vec![encode_global_chunk_id(
                        target.prefix,
                        target.root_key.clone(),
                        target.honest_type,
                        locals.clone(),
                    )
                    .expect("encode honest id")])
                    .expect("pack request");
                    assert!(
                        matches!(serve(&source, &request, grove_version), Served::Ok),
                        "{:?} subtree {}: the honest request must be served",
                        target.honest_type,
                        hex::encode(target.prefix)
                    );
                }
            }
            assert!(
                panics.is_empty(),
                "fetch_chunk panicked on {} peer requests under protocol version {}: {panics:#?}",
                panics.len(),
                grove_version.protocol_version
            );
            assert!(refusals > 0, "the matrix must reach the family refusal");
        }
    }

    /// The grove root and an empty grove: the root is served as a
    /// `NormalTree` whatever the peer names when it drops the root key, and
    /// is refused on family grounds when the peer names the root key with
    /// another family. An empty grove has nothing to hash under any type.
    #[test]
    fn fetch_chunk_answers_every_tree_type_byte_for_the_grove_root() {
        let grove_version = GroveVersion::latest();
        for source in [
            make_empty_grovedb(),
            every_subtree_kind_source(grove_version),
        ] {
            let tx = source.start_transaction();
            let grove_root_key = source
                .open_transactional_merk_by_prefix(
                    SubtreePrefix::default(),
                    None,
                    TreeType::NormalTree,
                    &tx,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("open grove root")
                .root_key();
            drop(tx);
            let target = ServedTarget {
                prefix: SubtreePrefix::default(),
                root_key: grove_root_key.clone(),
                honest_type: TreeType::NormalTree,
                honest_locals: Vec::new(),
            };
            for root_key in [grove_root_key.clone(), None] {
                for type_byte in 0..=16u8 {
                    for locals in synthetic_locals() {
                        let request = pack_nested_bytes(vec![global_chunk_id_with_type_byte(
                            target.prefix,
                            root_key.clone(),
                            type_byte,
                            locals.clone(),
                        )])
                        .expect("pack request");
                        let served = serve(&source, &request, grove_version);
                        let case = format!(
                            "grove root (root key {root_key:?}), type byte {type_byte}, locals \
                             {locals:?}"
                        );
                        match served {
                            Served::Panic(message) => panic!("{case}: panicked: {message}"),
                            Served::Ok if must_refuse_family(&target, &root_key, type_byte) => {
                                panic!("{case}: a mismatched family must be refused")
                            }
                            Served::Err(message)
                                if must_refuse_family(&target, &root_key, type_byte) =>
                            {
                                assert!(message.contains(FAMILY_REFUSAL), "{case}: {message}")
                            }
                            Served::Ok | Served::Err(_) => {}
                        }
                    }
                }
            }
        }
    }
}
