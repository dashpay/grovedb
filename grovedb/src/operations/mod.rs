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

#[cfg(feature = "minimal")]
pub mod indexed_tree;

// Backward-compatible alias for callers that referenced the old module
// name before Phase 2 generalized count_indexed_tree.rs into
// indexed_tree.rs (now serving all three indexed-tree variants).
#[cfg(feature = "minimal")]
pub use indexed_tree as count_indexed_tree;

#[cfg(feature = "minimal")]
pub use get::{QueryItemOrSumReturnType, MAX_REFERENCE_HOPS};
