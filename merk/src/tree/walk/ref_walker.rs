//! Merk reference walker

#[cfg(feature = "full")]
use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;
#[cfg(feature = "full")]
use super::{
    super::{Link, TreeNode},
    Fetch,
};
use crate::tree::kv::ValueDefinedCostType;
#[cfg(feature = "full")]
use crate::Error;

#[cfg(feature = "full")]
/// Allows read-only traversal of a `Tree`, fetching from the given source when
/// traversing to a pruned node. The fetched nodes are then retained in memory
/// until they (possibly) get pruned on the next commit.
///
/// Only finalized trees may be walked (trees which have had `commit` called
/// since the last update).
pub struct RefWalker<'a, S>
where
    S: Fetch + Sized + Clone,
{
    tree: &'a mut TreeNode,
    source: S,
}

#[cfg(feature = "full")]
impl<'a, S> RefWalker<'a, S>
where
    S: Fetch + Sized + Clone,
{
    /// Creates a `RefWalker` with the given tree and source.
    pub fn new(tree: &'a mut TreeNode, source: S) -> Self {
        // TODO: check if tree has modified links, panic if so
        RefWalker { tree, source }
    }

    /// Gets an immutable reference to the `Tree` wrapped by this `RefWalker`.
    pub fn tree(&self) -> &TreeNode {
        self.tree
    }

    /// Traverses to the child on the given side (if any), fetching from the
    /// source if pruned. When fetching, the link is upgraded from
    /// `Link::Reference` to `Link::Loaded`.
    pub fn walk<V>(
        &mut self,
        left: bool,
        value_defined_cost_fn: Option<&V>,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<RefWalker<S>>, Error>
    where
        V: Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
    {
        let link = match self.tree.link(left) {
            None => return Ok(None).wrap_with_cost(Default::default()),
            Some(link) => link,
        };

        let mut cost = OperationCost::default();
        match link {
            Link::Reference { .. } => {
                let load_res = self
                    .tree
                    .load(left, &self.source, value_defined_cost_fn, grove_version)
                    .unwrap_add_cost(&mut cost);
                if let Err(e) = load_res {
                    return Err(e).wrap_with_cost(cost);
                }
            }
            Link::Modified { .. } => panic!("Cannot traverse Link::Modified"),
            Link::Uncommitted { .. } | Link::Loaded { .. } => {}
        }

        let child = self.tree.child_mut(left).unwrap();
        Ok(Some(RefWalker::new(child, self.source.clone()))).wrap_with_cost(cost)
    }
}
