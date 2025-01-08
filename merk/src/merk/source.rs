use grovedb_costs::CostResult;
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use crate::{
    merk::TreeType,
    tree::{kv::ValueDefinedCostType, Fetch, TreeNode},
    Error, Link, Merk,
};

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
{
    pub(in crate::merk) fn source(&self) -> MerkSource<S> {
        MerkSource {
            storage: &self.storage,
            tree_type: self.tree_type,
        }
    }
}

#[derive(Debug)]
pub struct MerkSource<'s, S> {
    storage: &'s S,
    tree_type: TreeType,
}

impl<'s, S> Clone for MerkSource<'s, S> {
    fn clone(&self) -> Self {
        MerkSource {
            storage: self.storage,
            tree_type: self.tree_type,
        }
    }
}

impl<'s, 'db, S> Fetch for MerkSource<'s, S>
where
    S: StorageContext<'db>,
{
    fn fetch(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        TreeNode::get(
            self.storage,
            link.key(),
            value_defined_cost_fn,
            grove_version,
        )
        .map_ok(|x| x.ok_or(Error::KeyNotFoundError("Key not found for fetch")))
        .flatten()
    }
}
