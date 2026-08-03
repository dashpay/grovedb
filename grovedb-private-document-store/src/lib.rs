#![deny(missing_docs)]
//! PrivateDocumentStore: an append-only store of fixed-size opaque entries
//! for GroveDB.
//!
//! This crate is a thin wrapper over [`BulkAppendTree`] — the same
//! relationship `grovedb-commitment-tree` has to it, but with **no
//! Sinsemilla frontier**: entries here are write-once and never proven
//! against later, so no anchor is needed.
//!
//! # Committed configuration
//!
//! The store's configuration `{entry_size, chunk_power}` is bound into the
//! state root:
//!
//! ```text
//! pds_state_root = blake3("pds_state" || config_hash || bulk_state_root)
//! config_hash    = blake3("pds_config" || entry_size_be(4) || chunk_power(1))
//! ```
//!
//! so the declared entry size is consensus-visible and a proof can never be
//! reinterpreted under a different configuration.
//!
//! # Platform use
//!
//! Dash Platform stores each private document as a hiding commitment plus a
//! ciphertext of uniform, contract-declared size. GroveDB never interprets a
//! "document" — behaviorally this is a fully generic append-only log of
//! fixed-size opaque entries; the name simply describes its Platform use.

mod error;
#[cfg(feature = "storage")]
mod store;
#[cfg(all(test, feature = "storage"))]
pub(crate) mod test_utils;

pub use error::PrivateDocumentStoreError;
pub use grovedb_bulk_append_tree::{
    deserialize_chunk_blob, serialize_chunk_blob, BulkAppendError, BulkAppendTree,
};
#[cfg(feature = "storage")]
pub use store::{PrivateDocumentStore, PrivateDocumentStoreAppendResult};

/// Pre-computed state root of an empty [`BulkAppendTree`]:
/// `blake3("bulk_state" || [0; 32] || [0; 32])`.
///
/// The empty bulk root is independent of `chunk_power` (an empty MMR and an
/// empty dense tree both contribute a zero hash regardless of height), so it
/// can be a true constant. The full empty *store* root additionally binds the
/// configuration and is therefore a function —
/// [`empty_private_document_store_state_root`].
///
/// The `test_empty_bulk_append_tree_state_root_constant` test pins this
/// constant to the runtime computation.
pub const EMPTY_BULK_APPEND_TREE_STATE_ROOT: [u8; 32] = [
    0x41, 0xe0, 0x80, 0xa7, 0xfc, 0x26, 0x32, 0x3a, 0x1a, 0x44, 0x90, 0x5d, 0xa2, 0x0d, 0x6d, 0x59,
    0x85, 0x11, 0xf8, 0x39, 0xef, 0xd7, 0x03, 0x42, 0xe2, 0x1e, 0x7e, 0xdc, 0xd5, 0xc3, 0xff, 0x61,
];

/// Compute the committed-configuration hash for a private document store.
///
/// `config_hash = blake3("pds_config" || entry_size.to_be_bytes() || [chunk_power])`
///
/// The fixed-width big-endian encoding (4 bytes for `entry_size`, 1 byte for
/// `chunk_power`) makes the preimage unambiguous.
pub fn private_document_store_config_hash(entry_size: u32, chunk_power: u8) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"pds_config");
    hasher.update(&entry_size.to_be_bytes());
    hasher.update(&[chunk_power]);
    *hasher.finalize().as_bytes()
}

/// Compute the combined PrivateDocumentStore state root that binds the
/// committed configuration to the [`BulkAppendTree`] data root.
///
/// `pds_state_root = blake3("pds_state" || config_hash || bulk_state_root)`
///
/// This is the value that flows as the Merk child hash, ensuring both the
/// configuration (entry size, chunk power) and the appended data are
/// authenticated by the GroveDB root hash.
pub fn compute_private_document_store_state_root(
    config_hash: &[u8; 32],
    bulk_state_root: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"pds_state");
    hasher.update(config_hash);
    hasher.update(bulk_state_root);
    *hasher.finalize().as_bytes()
}

/// The state root of an empty private document store with the given
/// configuration.
///
/// Unlike `EMPTY_COMMITMENT_TREE_STATE_ROOT` (a constant — the commitment
/// tree root does not bind its configuration), the empty PDS root depends on
/// `{entry_size, chunk_power}`, so it is a function built from the
/// pre-computed [`EMPTY_BULK_APPEND_TREE_STATE_ROOT`] constant: two blake3
/// calls instead of opening the tree.
pub fn empty_private_document_store_state_root(entry_size: u32, chunk_power: u8) -> [u8; 32] {
    compute_private_document_store_state_root(
        &private_document_store_config_hash(entry_size, chunk_power),
        &EMPTY_BULK_APPEND_TREE_STATE_ROOT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_bulk_append_tree_state_root_constant() {
        let null = [0u8; 32];
        let computed = grovedb_bulk_append_tree::compute_state_root(&null, &null);
        assert_eq!(
            computed, EMPTY_BULK_APPEND_TREE_STATE_ROOT,
            "EMPTY_BULK_APPEND_TREE_STATE_ROOT constant does not match runtime computation"
        );
    }

    /// Pin the full empty-store root for one canonical configuration so any
    /// accidental change to a domain tag or preimage encoding is caught.
    #[test]
    fn test_empty_private_document_store_state_root_pinned_vector() {
        let expected: [u8; 32] = [
            0x56, 0x45, 0xf9, 0x7a, 0xe8, 0x5d, 0xba, 0xec, 0x29, 0x55, 0x47, 0x7a, 0x61, 0x65,
            0xdc, 0xbd, 0x17, 0xa6, 0x40, 0x71, 0xd5, 0x5a, 0xed, 0x9f, 0x0f, 0xf7, 0xe5, 0xff,
            0x41, 0xe0, 0x7f, 0xd9,
        ];
        assert_eq!(empty_private_document_store_state_root(64, 4), expected);
    }

    /// The state root must change when either configuration parameter
    /// changes — that is the whole point of binding the config.
    #[test]
    fn test_state_root_binds_configuration() {
        let base = empty_private_document_store_state_root(64, 4);
        assert_ne!(base, empty_private_document_store_state_root(65, 4));
        assert_ne!(base, empty_private_document_store_state_root(64, 5));
        // Field boundaries are unambiguous: (entry_size, chunk_power)
        // pairs that would collide under a length-prefix-free encoding
        // must still differ thanks to the fixed-width layout.
        assert_ne!(
            private_document_store_config_hash(0x0102, 0x03),
            private_document_store_config_hash(0x01, 0x02),
        );
    }

    /// The PDS domain tags must not collide with the commitment tree's
    /// `"ct_state"` or the bulk tree's `"bulk_state"` domains for identical
    /// 64-byte payloads.
    #[test]
    fn test_domain_separation() {
        let a = [7u8; 32];
        let b = [9u8; 32];
        let pds = compute_private_document_store_state_root(&a, &b);
        let bulk = grovedb_bulk_append_tree::compute_state_root(&a, &b);
        assert_ne!(pds, bulk);
    }
}
