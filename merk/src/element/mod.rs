use grovedb_costs::OperationCost;
use grovedb_element::{error::ElementError, Element};

use crate::tree::value_hash;

pub mod costs;
pub mod delete;
pub mod exists;
pub mod get;
pub mod insert;
pub mod tree_type;

pub trait ElementExt {
    fn value_hash(
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
}
