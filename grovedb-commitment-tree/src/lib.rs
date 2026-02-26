//! Orchard-style commitment tree integration for GroveDB.
//!
//! This crate provides a lightweight frontier-based Sinsemilla Merkle tree
//! for tracking note commitment anchors. It wraps the `incrementalmerkletree`
//! `Frontier` type with `orchard::tree::MerkleHashOrchard` hashing.
//!
//! # Architecture
//!
//! - Uses `Frontier<MerkleHashOrchard, 32>` for O(1) append and root
//!   computation
//! - Stores only the rightmost path (~1KB constant size) rather than the full
//!   tree
//! - Items (cmx || encrypted_note) are stored as GroveDB CountTree items
//! - The frontier is serialized to data storage alongside the BulkAppendTree
//! - Historical anchors are managed by Platform in a separate tree (not here)

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "client")]
pub use client::client_memory_commitment_tree::ClientMemoryCommitmentTree;
#[cfg(feature = "sqlite")]
mod sqlite_client;
#[cfg(feature = "sqlite")]
mod sqlite_store;
#[cfg(feature = "storage")]
mod commitment_tree;
mod error;
mod commitment_frontier;
pub use commitment_frontier::*;
pub use error::CommitmentTreeError;

pub use grovedb_costs::{self};
#[cfg(feature = "server")]
use grovedb_costs::CostsExt;
pub use incrementalmerkletree::{Hashable, Level, Position, Retention};
// Builder for constructing shielded transactions
pub use orchard::builder::{Builder, BundleType};
/// Re-export of `orchard::bundle::BatchValidator` for verifying Orchard
/// bundles.
///
/// # Sighash Requirement
///
/// [`BatchValidator::add_bundle`] requires a `sighash: [u8; 32]` parameter —
/// the transaction hash that the Orchard bundle commits to. This hash covers
/// the transaction data excluding the Orchard bundle itself and is used to
/// verify both spend authorization signatures and the binding signature.
///
/// Platform **must** compute the sighash according to the Dash-adapted
/// equivalent of ZIP-244's transaction digest algorithm and pass it when adding
/// each bundle. Without the correct sighash, signature verification will fail
/// even if the ZK proofs are valid.
///
/// # Usage
///
/// ```ignore
/// use grovedb_commitment_tree::{BatchValidator, VerifyingKey};
/// use rand::rngs::OsRng;
///
/// let mut validator = BatchValidator::new();
/// // sighash must be the transaction digest for this bundle
/// validator.add_bundle(&bundle, sighash);
/// // Validate all accumulated bundles (ZK proofs + signatures)
/// let valid = validator.validate(&verifying_key, OsRng);
/// ```
pub use orchard::bundle::BatchValidator;
// Bundle/Action types
pub use orchard::bundle::{Authorized, Flags};
// Proof creation/verification (requires orchard "circuit" feature)
pub use orchard::circuit::{ProvingKey, VerifyingKey};
// Key management
pub use orchard::keys::{
    FullViewingKey, IncomingViewingKey, OutgoingViewingKey, PreparedIncomingViewingKey, Scope,
    SpendAuthorizingKey, SpendValidatingKey, SpendingKey,
};
// Compact note size constant (52 bytes, same for all memo sizes)
pub use orchard::memo::COMPACT_NOTE_SIZE;
// Memo size types for Dash 36-byte memos
pub use orchard::memo::{DashMemo, MemoSize};
// Note types (orchard::Address aliased to avoid conflict with incrementalmerkletree::Address)
pub use orchard::note::RandomSeed;
// Bundle reconstruction types (needed for deserializing bundles from bytes)
pub use orchard::note::TransmittedNoteCiphertext;
// Orchard tree types
pub use orchard::note::{ExtractedNoteCommitment, Nullifier};
// Note encryption / trial decryption
pub use orchard::note_encryption::{CompactAction, OrchardDomain};
// Byte wrapper and trait for constructing note ciphertexts
pub use orchard::zcash_note_encryption::note_bytes::{NoteBytes, NoteBytesData};
// Trial decryption functions and traits
pub use orchard::zcash_note_encryption::{
    try_compact_note_decryption, try_note_decryption, Domain, EphemeralKeyBytes, ShieldedOutput,
};
pub use orchard::{
    note::Rho,
    primitives::redpallas,
    tree::{Anchor, MerkleHashOrchard, MerklePath},
    value::{NoteValue, ValueCommitment},
    Action, Address as PaymentAddress, Bundle, Note, Proof, NOTE_COMMITMENT_TREE_DEPTH,
};
#[cfg(feature = "sqlite")]
pub use rusqlite;
#[cfg(feature = "sqlite")]
pub use sqlite_client::ClientPersistentCommitmentTree;
#[cfg(feature = "sqlite")]
pub use sqlite_store::{SqliteShardStore, SqliteShardStoreError};
#[cfg(feature = "storage")]
pub use grovedb_bulk_append_tree::{
    deserialize_chunk_blob, serialize_chunk_blob, BulkAppendError, BulkAppendTree,
};
#[cfg(feature = "storage")]
pub use commitment_tree::{
    ciphertext_payload_size, deserialize_ciphertext, serialize_ciphertext, CommitmentAppendResult,
    CommitmentTree, COMMITMENT_TREE_DATA_KEY,
};
