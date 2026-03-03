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
            _ => None,
        }
    }
}
