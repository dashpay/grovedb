use grovedb_costs::CostResult;
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;
use crate::{
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

impl<'s, 'db, S> Fetch for MerkSource<'s, S>
where
    S: StorageContext<'db>,
{
    fn fetch(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<&impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        TreeNode::get(self.storage, link.key(), value_defined_cost_fn, grove_version)
            .map_ok(|x| x.ok_or(Error::KeyNotFoundError("Key not found for fetch")))
            .flatten()
    }
}
