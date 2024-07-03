//! Walk

#[cfg(feature = "full")]
use grovedb_costs::CostResult;

#[cfg(feature = "full")]
use super::super::{Link, TreeNode};
#[cfg(feature = "full")]
use crate::error::Error;
#[cfg(feature = "full")]
use crate::tree::kv::ValueDefinedCostType;

#[cfg(feature = "full")]
/// A source of data to be used by the tree when encountering a pruned node.
/// This typically means fetching the tree node from a backing store by its key,
/// but could also implement an in-memory cache for example.
pub trait Fetch {
    /// Called when the tree needs to fetch a node with the given `Link`. The
    /// `link` value will always be a `Link::Reference` variant.
    fn fetch(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<&impl Fn(&[u8]) -> Option<ValueDefinedCostType>>,
    ) -> CostResult<TreeNode, Error>;
}
