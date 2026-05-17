//! Shared helpers for `AggregateCountOnRange` and `AggregateSumOnRange`
//! provers/verifiers.
//!
//! Range-bound classification is independent of the aggregate flavor
//! (count vs. sum) — it depends only on how a subtree's possible-key
//! window overlaps the query's inner range. The same `SubtreeClassification`
//! and the same `classify_subtree` decision drive both aggregate variants
//! identically. Keeping a single source of truth here prevents drift
//! between the two paths and removes a malleability surface (one of the
//! verifiers accepting a subtree the other rejects, or vice versa).
//!
//! Items exported here are `pub(super)` so only the two aggregate modules
//! that live alongside this one can use them.

#[cfg(any(feature = "minimal", feature = "verify"))]
use grovedb_query::QueryItem;

#[cfg(any(feature = "minimal", feature = "verify"))]
use crate::CryptoHash;

/// All-zero `CryptoHash`. Used as the placeholder for missing children
/// in `Node::HashWithCount` / `Node::HashWithSum` proof reconstruction.
#[cfg(any(feature = "minimal", feature = "verify"))]
pub(super) const NULL_HASH: CryptoHash = [0u8; 32];

/// How a subtree's possible-key window relates to the inner range we're
/// aggregating over.
#[cfg(any(feature = "minimal", feature = "verify"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SubtreeClassification {
    /// Every possible key in this subtree falls **outside** the range.
    Disjoint,
    /// Every possible key in this subtree falls **inside** the range.
    Contained,
    /// The subtree straddles a range boundary (or directly contains one).
    Boundary,
}

/// Classify a subtree relative to the inner range.
///
/// `subtree_lo_excl` and `subtree_hi_excl` are the **exclusive** bounds
/// on what keys can appear under the subtree (derived from ancestors
/// during the walk; both `None` at the root). The range bounds come
/// from the inner `QueryItem`'s `lower_bound` / `upper_bound`.
///
/// The comparisons treat `subtree_hi_excl` as exclusive (subtree keys
/// are strictly < `subtree_hi_excl`) and `subtree_lo_excl` as exclusive
/// (subtree keys are strictly > `subtree_lo_excl`). For the range
/// bounds, the inclusivity flag returned by `lower_bound` /
/// `upper_bound` is **not** load-bearing for the disjoint/contained
/// tests below — see the inline proofs.
#[cfg(any(feature = "minimal", feature = "verify"))]
pub(super) fn classify_subtree(
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    range: &QueryItem,
) -> SubtreeClassification {
    let (range_lo, _range_lo_excl) = range.lower_bound();
    let (range_hi, _range_hi_incl) = range.upper_bound();

    // Disjoint-LEFT: subtree entirely below the range.
    //
    // Subtree keys are < subtree_hi_excl. If subtree_hi_excl <= range_lo,
    // every subtree key < subtree_hi_excl <= range_lo is also < range_lo,
    // so excluded regardless of whether range_lo is inclusive or exclusive.
    if let (Some(s_hi), Some(r_lo)) = (subtree_hi_excl, range_lo)
        && s_hi <= r_lo
    {
        return SubtreeClassification::Disjoint;
    }

    // Disjoint-RIGHT: subtree entirely above the range.
    //
    // Subtree keys are > subtree_lo_excl. If subtree_lo_excl >= range_hi,
    // every subtree key > subtree_lo_excl >= range_hi is also > range_hi,
    // so excluded regardless of whether range_hi is inclusive or exclusive.
    if let (Some(s_lo), Some(r_hi)) = (subtree_lo_excl, range_hi)
        && s_lo >= r_hi
    {
        return SubtreeClassification::Disjoint;
    }

    // Contained: subtree (s_lo, s_hi) ⊆ range.
    //
    // Lower side: every subtree key > s_lo. If s_lo >= r_lo, every subtree
    // key > s_lo >= r_lo, so > r_lo, satisfying both inclusive and exclusive
    // r_lo. If subtree has no lower bound (s_lo = -inf) but range does, the
    // subtree could include arbitrarily small keys → not contained.
    let lower_contained = match range_lo {
        None => true,
        Some(r_lo) => match subtree_lo_excl {
            Some(s_lo) => s_lo >= r_lo,
            None => false,
        },
    };
    // Upper side: every subtree key < s_hi. If s_hi <= r_hi, every subtree
    // key < s_hi <= r_hi, so < r_hi, satisfying both inclusive and exclusive
    // r_hi. (We forgo the slightly tighter "s_hi <= r_hi+1" optimization for
    // inclusive r_hi because we don't have key arithmetic.)
    let upper_contained = match range_hi {
        None => true,
        Some(r_hi) => match subtree_hi_excl {
            Some(s_hi) => s_hi <= r_hi,
            None => false,
        },
    };

    if lower_contained && upper_contained {
        SubtreeClassification::Contained
    } else {
        SubtreeClassification::Boundary
    }
}

/// Returns true when `key` lies strictly between the exclusive bounds
/// `(lo, hi)`, where `None` represents `-inf` / `+inf`.
///
/// Used at every `Boundary` node during the shape walk to verify that a
/// `KVDigest{Count,Sum}` carries a key consistent with its inherited
/// subtree window. Without this check, a forged proof could place a
/// boundary key outside the window its ancestors implied, and the
/// classification logic would silently miscount/misadd children that
/// don't actually exist at that position in the tree.
#[cfg(any(feature = "minimal", feature = "verify"))]
pub(super) fn key_strictly_inside(key: &[u8], lo: Option<&[u8]>, hi: Option<&[u8]>) -> bool {
    let lo_ok = lo.is_none_or(|l| key > l);
    let hi_ok = hi.is_none_or(|h| key < h);
    lo_ok && hi_ok
}
