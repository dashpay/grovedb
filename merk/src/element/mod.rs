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

/// Extension trait for computing element value hashes.
pub trait ElementExt {
    /// Computes the value hash for this element.
    fn value_hash(
        &self,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<[u8; 32], ElementError>;

    /// The hash this element commits to: the hash of its serialization with
    /// the referrer list emptied. For a backward-references item this is the
    /// node value hash itself; for a `BidirectionalReference` it is the half
    /// of the node value hash that is combined with the resolved end hash;
    /// and it is what a reference to either stores. Registering or removing
    /// a referrer therefore moves no hash. For every other element it is the
    /// plain serialized-bytes hash.
    fn logical_value_hash(
        &self,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<[u8; 32], ElementError>;
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
}
