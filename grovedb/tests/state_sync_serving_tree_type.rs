//! A state-sync peer names the tree type of the subtree it asks for, and the
//! serving node must not trust it.
//!
//! `GroveDb::fetch_chunk` opens the addressed subtree with the tree type in
//! the peer's global chunk id. Naming a `Provable*` type (plain chunk route)
//! or an indexed type with a header request (header route) for a subtree that
//! stores plain nodes used to reach the fail-closed `panic!` in
//! `TreeNode::hash_for_link`, so any peer could crash the serving node at
//! will. The request is now refused with an error.
//!
//! Only the public API is used, the way a remote peer sees the node: the
//! global chunk id is hand-encoded in the documented wire layout
//! `prefix(32) ‖ root_key_len(u16 BE) ‖ root_key ‖ tree_type(1) ‖ pack(ids)`
//! and the request is `pack([global_id])`. The serving path is not
//! version-gated, so the checks run under `GROVE_V3` as well as the latest
//! version.
#![cfg(feature = "minimal")]

use std::panic::{catch_unwind, AssertUnwindSafe};

use grovedb::{replication::CURRENT_STATE_SYNC_VERSION, Element, GroveDb};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

const SUBTREE: &[u8] = b"plain";

/// Packs byte strings the way the state-sync wire does: a `u32` BE count,
/// then each element as a `u32` BE length and its bytes.
fn pack(parts: Vec<Vec<u8>>) -> Vec<u8> {
    let mut out = (parts.len() as u32).to_be_bytes().to_vec();
    for part in parts {
        out.extend_from_slice(&(part.len() as u32).to_be_bytes());
        out.extend(part);
    }
    out
}

fn request(prefix: [u8; 32], root_key: &[u8], tree_type: u8, local_ids: Vec<Vec<u8>>) -> Vec<u8> {
    let mut id = prefix.to_vec();
    id.extend_from_slice(&(root_key.len() as u16).to_be_bytes());
    id.extend_from_slice(root_key);
    id.push(tree_type);
    id.extend(pack(local_ids));
    pack(vec![id])
}

/// A grove with a `NormalTree` subtree tall enough for its root chunk to end
/// in `Hash` boundaries, returning the subtree's prefix and root key.
fn plain_subtree_source(
    grove_version: &GroveVersion,
) -> (tempfile::TempDir, GroveDb, [u8; 32], Vec<u8>) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let db = GroveDb::open(dir.path()).expect("open grove");
    db.insert::<&[u8], _>(
        &[],
        SUBTREE,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert subtree");
    for i in 0..100u32 {
        db.insert(
            &[SUBTREE],
            &i.to_be_bytes(),
            Element::new_item(vec![7u8; 8]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");
    }
    let root_key = match db
        .get::<&[u8], _>(&[], SUBTREE, None, grove_version)
        .unwrap()
        .expect("get subtree element")
    {
        Element::Tree(Some(root_key), _) => root_key,
        other => panic!("unexpected subtree element {other:?}"),
    };
    let path: &[&[u8]] = &[SUBTREE];
    let prefix = RocksDbStorage::build_prefix(SubtreePath::from(path)).unwrap();
    (dir, db, prefix, root_key)
}

/// Serves `request` like a node answering a peer; a panic is returned as
/// `Err(Some(message))` instead of unwinding through the test.
fn serve(db: &GroveDb, request: &[u8], grove_version: &GroveVersion) -> Result<(), Option<String>> {
    let served = catch_unwind(AssertUnwindSafe(|| {
        db.fetch_chunk(request, None, CURRENT_STATE_SYNC_VERSION, grove_version)
            .map(drop)
            .map_err(|error| error.to_string())
    }));
    match served {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => {
            assert!(
                message.contains("does not belong to the subtree"),
                "expected the tree-type refusal, got: {message}"
            );
            Err(None)
        }
        Err(payload) => Err(Some(
            payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default(),
        )),
    }
}

fn assert_mismatched_tree_types_are_refused(grove_version: &GroveVersion) {
    let (_dir, db, prefix, root_key) = plain_subtree_source(grove_version);
    let honest = request(prefix, &root_key, 0, vec![]);
    assert_eq!(serve(&db, &honest, grove_version), Ok(()), "honest request");

    // Plain chunk route: each `Provable*` type for the plain subtree.
    let plain_route = [5u8, 6, 11, 12].map(|tree_type| (tree_type, vec![]));
    // Header route: each indexed type with a well-formed header request
    // (marker 0xFE, one axis, tag 0, no secondary root key).
    let header_route = [13u8, 14, 15].map(|tree_type| (tree_type, vec![vec![0xFE, 1, 0, 0, 0]]));
    for (tree_type, local_ids) in plain_route.into_iter().chain(header_route) {
        let attack = request(prefix, &root_key, tree_type, local_ids);
        assert_eq!(
            serve(&db, &attack, grove_version),
            Err(None),
            "tree type {tree_type} must be refused with an error, not served or panicked on"
        );
    }

    assert_eq!(
        serve(&db, &honest, grove_version),
        Ok(()),
        "the node keeps serving honest requests"
    );
}

#[test]
fn fetch_chunk_refuses_a_peer_chosen_tree_type_of_another_family() {
    assert_mismatched_tree_types_are_refused(GroveVersion::latest());
}

#[test]
fn fetch_chunk_refuses_a_peer_chosen_tree_type_of_another_family_under_grove_v3() {
    assert_mismatched_tree_types_are_refused(&GROVE_V3);
}
