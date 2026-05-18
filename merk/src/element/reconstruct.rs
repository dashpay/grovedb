//! Reconstruct
//! Functions for reconstructing tree elements with updated root keys

use grovedb_element::{indexed::IndexedTreeAxes, Element};

use crate::tree::AggregateData;

/// Extension trait for reconstructing tree elements with updated root key and
/// aggregate data while preserving flags and type-specific fields.
pub trait ElementReconstructExtensions {
    /// Reconstruct a tree element with updated root key and aggregate data,
    /// preserving flags and type-specific fields.
    /// Returns `None` for non-tree elements.
    fn reconstruct_with_root_key(
        &self,
        maybe_root_key: Option<Vec<u8>>,
        aggregate_data: AggregateData,
    ) -> Option<Element>;

    /// Reconstruct a two-secondary indexed-tree element
    /// (`ProvableSumIndexedTree` or `ProvableCountIndexedTree`) with
    /// updated primary and secondary root keys plus aggregate data,
    /// preserving flags. Returns `None` for any other element type — the
    /// regular `reconstruct_with_root_key` covers single-Merk tree
    /// elements, and `ProvableCountProvableSumIndexedTree` uses the
    /// `reconstruct_with_axes` API instead.
    ///
    /// Looks through `Element::NonCounted` and re-wraps the inner element
    /// once reconstructed.
    fn reconstruct_with_two_root_keys(
        &self,
        primary_root_key: Option<Vec<u8>>,
        secondary_root_key: Option<Vec<u8>>,
        aggregate_data: AggregateData,
    ) -> Option<Element>;

    /// Reconstruct a `ProvableCountProvableSumIndexedTree` element with
    /// updated primary root key, count + sum aggregates, and canonical
    /// `axes` TLV, preserving flags. Returns `None` for any other
    /// element type.
    ///
    /// `axes` is the canonical (sorted-by-tag, deduped, 1..=3 entries)
    /// list of `(axis_tag, secondary_root_key)` pairs — see
    /// `grovedb_element::indexed::IndexAxis` for the tag values.
    ///
    /// Looks through `Element::NonCounted` and re-wraps the inner element
    /// once reconstructed.
    fn reconstruct_with_axes(
        &self,
        primary_root_key: Option<Vec<u8>>,
        aggregate_data: AggregateData,
        axes: IndexedTreeAxes,
    ) -> Option<Element>;
}

impl ElementReconstructExtensions for Element {
    fn reconstruct_with_root_key(
        &self,
        maybe_root_key: Option<Vec<u8>>,
        aggregate_data: AggregateData,
    ) -> Option<Element> {
        match self {
            Element::Tree(_, f) => Some(Element::Tree(maybe_root_key, f.clone())),
            Element::SumTree(.., f) => Some(Element::SumTree(
                maybe_root_key,
                aggregate_data.as_sum_i64(),
                f.clone(),
            )),
            Element::BigSumTree(.., f) => Some(Element::BigSumTree(
                maybe_root_key,
                aggregate_data.as_summed_i128(),
                f.clone(),
            )),
            Element::CountTree(.., f) => Some(Element::CountTree(
                maybe_root_key,
                aggregate_data.as_count_u64(),
                f.clone(),
            )),
            Element::CountSumTree(.., f) => Some(Element::CountSumTree(
                maybe_root_key,
                aggregate_data.as_count_u64(),
                aggregate_data.as_sum_i64(),
                f.clone(),
            )),
            Element::ProvableCountTree(.., f) => Some(Element::ProvableCountTree(
                maybe_root_key,
                aggregate_data.as_count_u64(),
                f.clone(),
            )),
            Element::ProvableCountSumTree(.., f) => Some(Element::ProvableCountSumTree(
                maybe_root_key,
                aggregate_data.as_count_u64(),
                aggregate_data.as_sum_i64(),
                f.clone(),
            )),
            Element::ProvableSumTree(.., f) => Some(Element::ProvableSumTree(
                maybe_root_key,
                aggregate_data.as_sum_i64(),
                f.clone(),
            )),
            Element::ProvableCountProvableSumTree(.., f) => {
                Some(Element::ProvableCountProvableSumTree(
                    maybe_root_key,
                    aggregate_data.as_count_u64(),
                    aggregate_data.as_sum_i64(),
                    f.clone(),
                ))
            }
            Element::CommitmentTree(tc, cp, f) => {
                Some(Element::CommitmentTree(*tc, *cp, f.clone()))
            }
            Element::MmrTree(sz, f) => Some(Element::MmrTree(*sz, f.clone())),
            Element::BulkAppendTree(tc, cp, f) => {
                Some(Element::BulkAppendTree(*tc, *cp, f.clone()))
            }
            Element::DenseAppendOnlyFixedSizeTree(c, h, f) => {
                Some(Element::DenseAppendOnlyFixedSizeTree(*c, *h, f.clone()))
            }
            // Recurse on the inner element and re-wrap. Without this, a
            // batch that mutates a subtree under a wrapped tree would lose
            // the wrapper on the parent's stored element when its root key
            // gets propagated upward.
            Element::NonCounted(inner) => inner
                .reconstruct_with_root_key(maybe_root_key, aggregate_data)
                .map(|reconstructed| Element::NonCounted(Box::new(reconstructed))),
            Element::NotSummed(inner) => inner
                .reconstruct_with_root_key(maybe_root_key, aggregate_data)
                .map(|reconstructed| Element::NotSummed(Box::new(reconstructed))),
            Element::NotCountedOrSummed(inner) => inner
                .reconstruct_with_root_key(maybe_root_key, aggregate_data)
                .map(|reconstructed| Element::NotCountedOrSummed(Box::new(reconstructed))),
            _ => None,
        }
    }

    fn reconstruct_with_two_root_keys(
        &self,
        primary_root_key: Option<Vec<u8>>,
        secondary_root_key: Option<Vec<u8>>,
        aggregate_data: AggregateData,
    ) -> Option<Element> {
        match self {
            Element::ProvableSumIndexedTree(.., f) => Some(Element::ProvableSumIndexedTree(
                primary_root_key,
                secondary_root_key,
                aggregate_data.as_sum_i64(),
                f.clone(),
            )),
            Element::ProvableCountIndexedTree(.., f) => Some(Element::ProvableCountIndexedTree(
                primary_root_key,
                secondary_root_key,
                aggregate_data.as_count_u64(),
                f.clone(),
            )),
            Element::NonCounted(inner) => inner
                .reconstruct_with_two_root_keys(
                    primary_root_key,
                    secondary_root_key,
                    aggregate_data,
                )
                .map(|reconstructed| Element::NonCounted(Box::new(reconstructed))),
            // PCPSIT carries an axes TLV rather than a single secondary
            // root key; callers must use `reconstruct_with_axes`.
            Element::ProvableCountProvableSumIndexedTree(..) => None,
            _ => None,
        }
    }

    fn reconstruct_with_axes(
        &self,
        primary_root_key: Option<Vec<u8>>,
        aggregate_data: AggregateData,
        axes: IndexedTreeAxes,
    ) -> Option<Element> {
        match self {
            Element::ProvableCountProvableSumIndexedTree(.., f) => {
                Some(Element::ProvableCountProvableSumIndexedTree(
                    primary_root_key,
                    aggregate_data.as_count_u64(),
                    aggregate_data.as_sum_i64(),
                    axes,
                    f.clone(),
                ))
            }
            Element::NonCounted(inner) => inner
                .reconstruct_with_axes(primary_root_key, aggregate_data, axes)
                .map(|reconstructed| Element::NonCounted(Box::new(reconstructed))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use grovedb_element::Element;

    use super::ElementReconstructExtensions;
    use crate::tree::AggregateData;

    #[test]
    fn reconstruct_preserves_non_counted_wrapper() {
        // A NonCounted-wrapped tree must come back wrapped after a root-key
        // propagation, otherwise update_tree_item_preserve_flag on a
        // subtree mutation would silently strip the wrapper from the
        // on-disk parent element.
        let inner = Element::new_count_tree_with_flags_and_count_value(None, 7, None);
        let wrapped = Element::new_non_counted(inner).expect("wrap ok");
        let new_root = Some(b"new_root".to_vec());
        let reconstructed = wrapped
            .reconstruct_with_root_key(new_root.clone(), AggregateData::Count(7))
            .expect("reconstruct ok");
        // Outer is still NonCounted.
        assert!(matches!(reconstructed, Element::NonCounted(_)));
        // Inner is the same kind of tree with the new root key.
        if let Element::NonCounted(boxed) = reconstructed {
            assert!(matches!(*boxed, Element::CountTree(ref k, 7, _) if k == &new_root));
        }
    }

    #[test]
    fn reconstruct_preserves_not_summed_wrapper() {
        // Symmetric to reconstruct_preserves_non_counted_wrapper.
        let inner = Element::new_sum_tree_with_flags_and_sum_value(None, 100, None);
        let wrapped = Element::new_not_summed(inner).expect("wrap ok");
        let new_root = Some(b"new_root".to_vec());
        let reconstructed = wrapped
            .reconstruct_with_root_key(new_root.clone(), AggregateData::Sum(100))
            .expect("reconstruct ok");
        assert!(matches!(reconstructed, Element::NotSummed(_)));
        if let Element::NotSummed(boxed) = reconstructed {
            assert!(matches!(*boxed, Element::SumTree(ref k, 100, _) if k == &new_root));
        }
    }

    #[test]
    fn reconstruct_returns_none_for_non_tree() {
        let item = Element::new_item(b"x".to_vec());
        assert!(item
            .reconstruct_with_root_key(None, AggregateData::NoAggregateData)
            .is_none());
    }

    #[test]
    fn reconstruct_preserves_not_counted_or_summed_wrapper() {
        // Symmetric to reconstruct_preserves_non_counted_wrapper /
        // reconstruct_preserves_not_summed_wrapper. A wrapped tree must come
        // back wrapped after root-key propagation; otherwise the on-disk
        // parent element would lose its wrapper on subtree mutations and the
        // parent's aggregate would erroneously include the subtree.
        let inner =
            Element::new_count_sum_tree_with_flags_and_sum_and_count_value(None, 3, 100, None);
        let wrapped = Element::new_not_counted_or_summed(inner).expect("wrap ok");
        let new_root = Some(b"new_root".to_vec());
        let reconstructed = wrapped
            .reconstruct_with_root_key(new_root.clone(), AggregateData::CountAndSum(3, 100))
            .expect("reconstruct ok");
        assert!(matches!(reconstructed, Element::NotCountedOrSummed(_)));
        if let Element::NotCountedOrSummed(boxed) = reconstructed {
            assert!(matches!(*boxed, Element::CountSumTree(ref k, 3, 100, _) if k == &new_root));
        }
    }
}
