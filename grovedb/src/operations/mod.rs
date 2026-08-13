//! Operations for the manipulation of GroveDB state

#[cfg(feature = "minimal")]
pub(crate) mod auxiliary;
#[cfg(feature = "minimal")]
pub mod delete;
#[cfg(feature = "minimal")]
pub(crate) mod get;
#[cfg(feature = "minimal")]
pub mod insert;
#[cfg(feature = "minimal")]
pub(crate) mod is_empty_tree;

#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod proof;

#[cfg(feature = "minimal")]
pub mod commitment_tree;

#[cfg(feature = "minimal")]
pub mod mmr_tree;

#[cfg(feature = "minimal")]
pub mod bulk_append_tree;

#[cfg(feature = "minimal")]
pub mod dense_tree;

/// Caller-driven subtree-root replacement. Bypasses grovedb's normal
/// "compute child hash from subtree state" invariant — see the module-level
/// docs in `replace_subtree_root.rs` for the safety contract.
#[cfg(all(feature = "minimal", feature = "unsafe-dump-load"))]
pub mod replace_subtree_root;

#[cfg(feature = "minimal")]
pub mod indexed_tree;

/// The axis path query front door: read, prove and verify
/// axis-ordered reads of an indexed tree.
pub mod axis_path_query;

#[cfg(feature = "minimal")]
pub use get::{QueryItemOrSumReturnType, MAX_REFERENCE_HOPS};
