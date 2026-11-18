use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;

use crate::{
    store::{NodeChangeSet, NodeWriteCost},
    tree::{Commit, TreeNode},
    Error,
};

/// What a commit does to the node under one key.
pub(in crate::merk) enum NodeUpdate<P> {
    /// The node is written: what the store prepared of it, and the cost.
    Write(P, NodeWriteCost),
    /// The node is removed.
    Remove(KeyValueStorageCost),
}

/// Collects tree nodes during commit for writing to the node store.
pub struct MerkCommitter<'c, C: NodeChangeSet> {
    change_set: &'c mut C,
    /// The updated nodes by key, in the order the commit walk wrote them.
    pub(in crate::merk) batch: Vec<(Vec<u8>, NodeUpdate<C::Prepared>)>,
    pub(in crate::merk) height: u8,
    pub(in crate::merk) levels: u8,
}

impl<'c, C: NodeChangeSet> MerkCommitter<'c, C> {
    pub(in crate::merk) fn new(change_set: &'c mut C, height: u8, levels: u8) -> Self {
        Self {
            change_set,
            batch: Vec::with_capacity(10000),
            height,
            levels,
        }
    }
}

impl<C: NodeChangeSet> Commit for MerkCommitter<'_, C> {
    fn write(
        &mut self,
        tree: &mut TreeNode,
        old_specialized_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
    ) -> Result<(), Error> {
        let value_put = tree.known_storage_cost.is_some();
        let storage_cost = if let Some(storage_cost) = tree.known_storage_cost.take() {
            storage_cost
        } else {
            tree.kv_with_parent_hook_size_and_storage_cost(old_specialized_cost)?
                .1
        };
        let change = tree.take_node_change(value_put);

        let prepared = self.change_set.prepare_node(tree, change);

        let left_child = tree.child_ref_and_sum_size(true);
        let right_child = tree.child_ref_and_sum_size(false);
        self.batch.push((
            tree.key().to_vec(),
            NodeUpdate::Write(
                prepared,
                NodeWriteCost {
                    feature: tree
                        .feature_type()
                        .tree_feature_specialized_type_and_length(),
                    left_child,
                    right_child,
                    storage_cost,
                },
            ),
        ));
        Ok(())
    }

    fn prune(&self, tree: &TreeNode) -> (bool, bool) {
        // keep N top levels of tree
        let prune = (self.height - tree.height()) >= self.levels;
        (prune, prune)
    }
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use super::*;
    use crate::{
        store::{mem::MemNodeStore, NodeChange, NodeStore},
        tree::kv::ValueDefinedCostType,
        TreeFeatureType::BasicMerkNode,
    };

    #[test]
    fn a_write_that_fails_keeps_what_the_node_has_to_report() {
        let new = TreeNode::new(vec![5], vec![7], None, BasicMerkNode).unwrap();
        let mut loaded = TreeNode::decode_raw(
            &new.encode(),
            vec![5],
            None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            GroveVersion::latest(),
        )
        .unwrap();
        loaded.set_link(true, None);

        let mut change_set = MemNodeStore::new().change_set();
        let mut committer = MerkCommitter::new(&mut change_set, 1, 100);

        let failed = committer.write(&mut loaded, &|_, _| Err(Error::InvalidInputError("cost")));
        assert!(matches!(failed, Err(Error::InvalidInputError("cost"))));
        assert!(committer.batch.is_empty());

        committer.write(&mut loaded, &|_, _| Ok(0)).unwrap();
        committer.write(&mut loaded, &|_, _| Ok(0)).unwrap();
        let changes: Vec<NodeChange> = committer
            .batch
            .iter()
            .map(|(_, update)| match update {
                NodeUpdate::Write((_, change), _) => *change,
                NodeUpdate::Remove(_) => unreachable!("only writes were made"),
            })
            .collect();
        assert_eq!(
            changes,
            [NodeChange::StructureChanged, NodeChange::HashOnly]
        );
    }
}
