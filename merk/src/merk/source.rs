use grovedb_costs::CostResult;
use grovedb_version::version::GroveVersion;

use crate::{
    store::NodeStore,
    tree::{kv::ValueDefinedCostType, Fetch, TreeNode},
    tree_type::TreeType,
    Error, Link, Merk,
};

impl<'db, S> Merk<S>
where
    S: NodeStore<'db>,
{
    pub(in crate::merk) fn source(&self) -> MerkSource<'_, S> {
        MerkSource {
            storage: &self.storage,
            tree_type: self.tree_type,
        }
    }
}

#[derive(Debug)]
/// A `Fetch` implementation that loads tree nodes from the Merk's node store.
pub struct MerkSource<'s, S> {
    storage: &'s S,
    tree_type: TreeType,
}

impl<S> Clone for MerkSource<'_, S> {
    fn clone(&self) -> Self {
        MerkSource {
            storage: self.storage,
            tree_type: self.tree_type,
        }
    }
}

impl<'db, S> Fetch for MerkSource<'_, S>
where
    S: NodeStore<'db>,
{
    fn fetch(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        self.storage
            .fetch_node(link, value_defined_cost_fn, grove_version)
    }
}
