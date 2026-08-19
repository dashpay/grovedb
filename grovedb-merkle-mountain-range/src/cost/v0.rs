//! Shipped MMR hash accounting: merges are not charged.
//!
//! Used by GROVE_V1..V3. These are released versions, so this is locked —
//! see [`super`] for why a cost correction cannot replace it in place.

/// Charge for the peak collapses of a push: none.
pub(super) fn merge_hashes(_merges: u32) -> u32 {
    0
}

/// Charge for peak bagging: none.
pub(super) fn bagging_hashes(_peaks: usize) -> u32 {
    0
}
