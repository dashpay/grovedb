//! Walk

#[cfg(feature = "minimal")]
use grovedb_costs::{CostResult, CostsExt};
use grovedb_version::version::GroveVersion;

#[cfg(feature = "minimal")]
use super::super::{Link, TreeNode};
#[cfg(feature = "minimal")]
use crate::error::Error;
#[cfg(feature = "minimal")]
use crate::tree::kv::ValueDefinedCostType;

#[cfg(feature = "minimal")]
/// A source of data to be used by the tree when encountering a pruned node.
/// This typically means fetching the tree node from a backing store by its key,
/// but could also implement an in-memory cache for example.
pub trait Fetch {
    /// Called when the tree needs to fetch a node with the given `Link`. The
    /// `link` value will always be a `Link::Reference` variant.
    fn fetch(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error>;

    /// Fetches the node `link` points to for a walk that only reads the tree
    /// and asks for the value of a node where it needs it
    /// ([`RefWalker::values_on_demand`](super::RefWalker::values_on_demand)).
    ///
    /// A source may leave the value out, and then says so in the node
    /// ([`TreeNode::value_absent`]): the walk keeps such a node to itself
    /// and has its value read with [`Self::fetch_value`]. The fetch is
    /// charged as [`Self::fetch`] is, whether or not the value comes with
    /// it. The default fetches the whole node, with no value-defined cost.
    fn fetch_without_value(
        &self,
        link: &Link,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        self.fetch(
            link,
            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
    }

    /// Reads the value of `node`, which [`Self::fetch_without_value`]
    /// returned without it. A source that fetches whole nodes returns none
    /// without its value, so the default refuses.
    fn fetch_value(&self, _node: &mut TreeNode) -> CostResult<(), Error> {
        Err(Error::CorruptedCodeExecution(
            "a source of whole nodes was asked for the value of a node",
        ))
        .wrap_with_cost(Default::default())
    }
}
