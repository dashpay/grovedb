use std::{cell::RefCell, collections::HashSet};

use grovedb_costs::CostResult;
use grovedb_version::version::GroveVersion;

use crate::{
    store::NodeStore,
    tree::{kv::ValueDefinedCostType, Fetch, TreeNode},
    tree_type::TreeType,
    Error, Link, Merk,
};

/// The keys of the nodes a walk of a Merk fetched without their values
/// ([`RefWalker::values_on_demand`](crate::tree::RefWalker::values_on_demand)),
/// was charged for, and did not keep in the Merk's tree.
///
/// A Merk on a store that returns whole nodes keeps every node a walk
/// fetches in its tree, and is charged for a node once: what it does with
/// the node afterwards reads memory. A node that came without its value is
/// not kept, so the Merk keeps its key here instead, and reads the node
/// again where the other Merk reads its tree, at no charge and with no
/// value-defined cost, as the walk fetched it. So what an operation costs
/// does not depend on whether its store leaves values out.
///
/// It is empty for as long as the store returns whole nodes.
#[derive(Debug, Default)]
pub(crate) struct PaidNodes(RefCell<HashSet<Vec<u8>>>);

impl PaidNodes {
    pub(crate) fn contains(&self, key: &[u8]) -> bool {
        let paid = self.0.borrow();
        !paid.is_empty() && paid.contains(key)
    }

    fn insert(&self, key: &[u8]) {
        self.0.borrow_mut().insert(key.to_vec());
    }

    fn remove(&self, key: &[u8]) {
        self.0.borrow_mut().remove(key);
    }

    /// Forgets every node: the tree they would be in is gone.
    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

const NO_VALUE_DEFINED_COST: Option<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> =
    None;

impl<'db, S> Merk<S>
where
    S: NodeStore<'db>,
{
    pub(in crate::merk) fn source(&self) -> MerkSource<'_, S> {
        MerkSource {
            storage: &self.storage,
            tree_type: self.tree_type,
            paid: &self.paid_nodes,
        }
    }

    /// The whole node `link` points to, which is a node of
    /// [`PaidNodes`]: read at no charge and with no value-defined cost.
    pub(in crate::merk) fn fetch_paid_node(
        &self,
        link: &Link,
        grove_version: &GroveVersion,
    ) -> Result<TreeNode, Error> {
        self.storage
            .fetch_node(link, NO_VALUE_DEFINED_COST, grove_version)
            .value
    }
}

#[derive(Debug)]
/// A `Fetch` implementation that loads tree nodes from the Merk's node store.
pub struct MerkSource<'s, S> {
    storage: &'s S,
    tree_type: TreeType,
    paid: &'s PaidNodes,
}

impl<S> Clone for MerkSource<'_, S> {
    fn clone(&self) -> Self {
        MerkSource {
            storage: self.storage,
            tree_type: self.tree_type,
            paid: self.paid,
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
        if !self.paid.contains(link.key()) {
            return self
                .storage
                .fetch_node(link, value_defined_cost_fn, grove_version);
        }
        // Whoever fetches a whole node keeps it in the tree or takes it out
        // of the tree: from here on the tree says what the Merk holds.
        let mut fetched = self
            .storage
            .fetch_node(link, NO_VALUE_DEFINED_COST, grove_version);
        fetched.cost = Default::default();
        if fetched.value.is_ok() {
            self.paid.remove(link.key());
        }
        fetched
    }

    fn fetch_without_value(
        &self,
        link: &Link,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        let paid = self.paid.contains(link.key());
        let mut fetched = self.storage.fetch_node_without_value(link, grove_version);
        match &fetched.value {
            Ok(node) if node.value_absent() && !paid => self.paid.insert(link.key()),
            Ok(node) if paid => {
                fetched.cost = Default::default();
                if !node.value_absent() {
                    self.paid.remove(link.key());
                }
            }
            _ => {}
        }
        fetched
    }

    fn fetch_value(&self, node: &mut TreeNode) -> CostResult<(), Error> {
        self.storage.load_node_value(node)
    }
}
