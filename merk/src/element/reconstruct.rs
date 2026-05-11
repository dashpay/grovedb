//! Reconstruct
//! Functions for reconstructing tree elements with updated root keys

use grovedb_element::Element;

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
}
