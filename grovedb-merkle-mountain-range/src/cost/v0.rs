//! Shipped MMR hash accounting: merges are not charged.
//!
//! Used by GROVE_V1..V3. These are released versions, so this is locked —
//! see [`super`] for why a cost correction cannot replace it in place.

use crate::helper::hash_count_for_push;

/// Charge for the peak collapses of a push: none.
pub(super) fn merge_hashes(_merges: u32) -> u32 {
    0
}

/// Charge for peak bagging: none.
pub(super) fn bagging_hashes(_peaks: usize) -> u32 {
    0
}

/// The caller owes the leaf hash and every collapse, because `push` bills
/// none of them under this version.
pub(super) fn call_site_hashes(leaf_count: u64) -> u32 {
    hash_count_for_push(leaf_count)
}
