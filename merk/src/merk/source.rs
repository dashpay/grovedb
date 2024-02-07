use grovedb_costs::CostResult;
use grovedb_storage::StorageContext;

use crate::{
    tree::{kv::ValueDefinedCostType, Fetch, TreeNode},
    Error, Link, Merk,
};

impl<'db, C> Merk<C>
where
    C: StorageContext<'db>,
{
    pub(in crate::merk) fn source(&self) -> MerkSource<C> {
        MerkSource {
            storage: &self.storage,
            is_sum_tree: self.is_sum_tree,
        }
    }
}

#[derive(Debug)]
pub struct MerkSource<'s, S> {
    storage: &'s S,
    is_sum_tree: bool,
}

impl<'s, S> Clone for MerkSource<'s, S> {
    fn clone(&self) -> Self {
        MerkSource {
            storage: self.storage,
            is_sum_tree: self.is_sum_tree,
        }
    }
}

impl<'s, 'db, C> Fetch for MerkSource<'s, C>
where
    C: StorageContext<'db>,
{
    fn fetch(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<&impl Fn(&[u8]) -> Option<ValueDefinedCostType>>,
    ) -> CostResult<TreeNode, Error> {
        TreeNode::get(self.storage, link.key(), value_defined_cost_fn)
            .map_ok(|x| x.ok_or(Error::KeyNotFoundError("Key not found for fetch")))
            .flatten()
    }
}
