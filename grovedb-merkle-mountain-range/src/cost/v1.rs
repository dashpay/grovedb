//! Corrected MMR hash accounting: one charge per blake3 merge computed.
//!
//! Used from GROVE_V4.

/// A push calls `MmrNode::merge` once per peak it collapses.
pub(super) fn merge_hashes(merges: u32) -> u32 {
    merges
}

/// Bagging folds the peaks right-to-left, so `n` peaks cost `n - 1` merges.
/// One peak (or none) folds nothing.
pub(super) fn bagging_hashes(peaks: usize) -> u32 {
    peaks.saturating_sub(1) as u32
}
