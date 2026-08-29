use grovedb_costs::OperationCost;
use grovedb_element::{error::ElementError, Element};

use crate::tree::value_hash;

#[cfg(feature = "minimal")]
/// Element cost calculation extensions.
pub mod costs;
#[cfg(feature = "minimal")]
/// Element decoding extensions.
pub mod decode;
#[cfg(feature = "minimal")]
pub mod delete;
#[cfg(feature = "minimal")]
pub mod exists;
#[cfg(feature = "minimal")]
pub mod get;
#[cfg(feature = "minimal")]
pub mod insert;
#[cfg(feature = "minimal")]
pub mod reconstruct;
/// Element tree type extensions.
pub mod tree_type;

/// The hash components of a backward-references-capable element's node
/// value hash. See `grovedb_element::bidirectional_reference` for the
/// scheme.
#[derive(Debug, Clone, Copy)]
pub struct BackwardsReferencesHashes {
    /// `H(serialize(element with backward_references = []))` — what forward
    /// references and result sets commit to.
    pub inner: [u8; 32],
    /// `H(serialize(backward_references))`.
    pub backrefs: [u8; 32],
    /// `combine(inner, backrefs)` — the node value hash for the ITEM
    /// variants. (A `BidirectionalReference`'s node value hash additionally
    /// combines the resolved target hash: `combine3(inner, target, backrefs)`.)
    pub combined: [u8; 32],
}

/// Extension trait for computing element value hashes.
pub trait ElementExt {
    /// Computes the value hash for this element.
    fn value_hash(
        &self,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<[u8; 32], ElementError>;

    /// The hash a REFERENCE to this element must store — the "logical"
    /// value hash. For backward-references-capable elements this is the
    /// inner (stripped) hash, so registering/removing referrers never
    /// invalidates hashes held by other referrers; for every other element
    /// it is the plain serialized-bytes hash.
    fn logical_value_hash(
        &self,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<[u8; 32], ElementError>;

    /// The backward-references hash components for this element, or `None`
    /// for elements without backward-references capability.
    fn backward_references_hashes(
        &self,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<Option<BackwardsReferencesHashes>, ElementError>;
}

impl ElementExt for Element {
    fn value_hash(
        &self,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<[u8; 32], ElementError> {
        let bytes = grovedb_costs::cost_return_on_error_default!(self.serialize(grove_version));
        value_hash(&bytes).map(Ok)
    }

    fn logical_value_hash(
        &self,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<[u8; 32], ElementError> {
        if self.supports_backward_references() {
            self.stripped_of_backward_references()
                .value_hash(grove_version)
        } else {
            self.value_hash(grove_version)
        }
    }

    fn backward_references_hashes(
        &self,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<Option<BackwardsReferencesHashes>, ElementError> {
        use grovedb_costs::{cost_return_on_error, CostsExt};

        let mut cost = Default::default();
        let Some(backward_references) = self.backward_references() else {
            return Ok(None).wrap_with_cost(cost);
        };
        let inner = cost_return_on_error!(
            &mut cost,
            self.stripped_of_backward_references()
                .value_hash(grove_version)
        );
        let backrefs_bytes = grovedb_costs::cost_return_on_error_no_add!(
            cost,
            grovedb_element::serialize_backward_references(backward_references)
        );
        let backrefs = value_hash(&backrefs_bytes).unwrap_add_cost(&mut cost);
        let combined =
            crate::tree::hash::combine_hash(&inner, &backrefs).unwrap_add_cost(&mut cost);
        Ok(Some(BackwardsReferencesHashes {
            inner,
            backrefs,
            combined,
        }))
        .wrap_with_cost(cost)
    }
}
