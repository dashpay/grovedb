//! Operations for the manipulation of GroveDB state

#[cfg(feature = "engine_generic")]
pub(crate) mod auxiliary;
#[cfg(feature = "engine_generic")]
pub mod delete;
#[cfg(feature = "engine_generic")]
pub(crate) mod get;
#[cfg(feature = "engine_generic")]
pub mod insert;
#[cfg(feature = "engine_generic")]
pub(crate) mod is_empty_tree;

#[cfg(any(feature = "engine_generic", feature = "verify"))]
pub mod proof;

#[cfg(feature = "engine_generic")]
pub mod commitment_tree;

#[cfg(feature = "engine_generic")]
pub mod mmr_tree;

#[cfg(feature = "engine_generic")]
pub mod bulk_append_tree;

#[cfg(feature = "engine_generic")]
pub mod dense_tree;
#[cfg(feature = "engine_generic")]
/// Private document store operations
pub mod private_document_store;

#[cfg(feature = "engine_generic")]
pub(crate) mod rewrap_non_merk_tree_parent_element;

/// Caller-driven subtree-root replacement. Bypasses grovedb's normal
/// "compute child hash from subtree state" invariant — see the module-level
/// docs in `replace_subtree_root.rs` for the safety contract.
#[cfg(all(feature = "engine_generic", feature = "unsafe-dump-load"))]
pub mod replace_subtree_root;

#[cfg(feature = "engine_generic")]
pub mod indexed_tree;

#[cfg(feature = "engine_generic")]
pub use get::{QueryItemOrSumReturnType, MAX_REFERENCE_HOPS};
