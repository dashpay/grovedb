use std::ops::Range;

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

/// The updates of one commit: what is done to the node under each key, with
/// the keys one after the other in one buffer.
pub(in crate::merk) struct NodeUpdates<P> {
    /// The bytes of the keys.
    keys: Vec<u8>,
    /// Each update with where its key is in `keys` and how many updates
    /// were added before it, in the order the updates were added.
    updates: Vec<(Range<usize>, usize, NodeUpdate<P>)>,
}

impl<P> NodeUpdates<P> {
    /// The updates a commit is given room for with its first.
    const FIRST_ROOM: usize = 64;

    pub(in crate::merk) const fn new() -> Self {
        Self {
            keys: Vec::new(),
            updates: Vec::new(),
        }
    }

    pub(in crate::merk) fn push(&mut self, key: &[u8], update: NodeUpdate<P>) {
        if self.updates.capacity() == 0 {
            self.updates.reserve(Self::FIRST_ROOM);
            self.keys.reserve((Self::FIRST_ROOM * 32).max(key.len()));
        }
        let start = self.keys.len();
        self.keys.extend_from_slice(key);
        let added_before = self.updates.len();
        self.updates
            .push((start..self.keys.len(), added_before, update));
    }

    /// The bytes of the keys, and the updates in ascending order of their
    /// keys, each with where its key is in those bytes. Two updates of one
    /// key are in the order they were added.
    pub(in crate::merk) fn into_sorted(
        mut self,
    ) -> (Vec<u8>, impl Iterator<Item = (Range<usize>, NodeUpdate<P>)>) {
        let keys = self.keys;
        // No two updates compare equal, so the order is one order.
        self.updates.sort_unstable_by(|a, b| {
            let by_key = keys[a.0.clone()].cmp(&keys[b.0.clone()]);
            by_key.then(a.1.cmp(&b.1))
        });
        let sorted = self.updates.into_iter();
        (keys, sorted.map(|(key, _, update)| (key, update)))
    }

    #[cfg(test)]
    pub(in crate::merk) fn iter(&self) -> impl Iterator<Item = (&[u8], &NodeUpdate<P>)> {
        (self.updates.iter()).map(|(at, _, update)| (&self.keys[at.clone()], update))
    }

    #[cfg(test)]
    pub(in crate::merk) fn is_empty(&self) -> bool {
        self.updates.is_empty()
    }
}

/// Collects tree nodes during commit for writing to the node store.
pub struct MerkCommitter<'c, C: NodeChangeSet> {
    change_set: &'c mut C,
    /// The updated nodes by key, in the order the commit walk wrote them.
    pub(in crate::merk) batch: NodeUpdates<C::Prepared>,
    pub(in crate::merk) height: u8,
    pub(in crate::merk) levels: u8,
}

impl<'c, C: NodeChangeSet> MerkCommitter<'c, C> {
    pub(in crate::merk) fn new(change_set: &'c mut C, height: u8, levels: u8) -> Self {
        Self {
            change_set,
            batch: NodeUpdates::new(),
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
        self.batch.push(
            tree.key(),
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
        );
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
        let changes: Vec<(&[u8], NodeChange)> = committer
            .batch
            .iter()
            .map(|(key, update)| match update {
                NodeUpdate::Write((_, change), _) => (key, *change),
                NodeUpdate::Remove(_) => unreachable!("only writes were made"),
            })
            .collect();
        assert_eq!(
            changes,
            [
                (&[5][..], NodeChange::StructureChanged),
                (&[5][..], NodeChange::HashOnly)
            ]
        );
    }

    #[test]
    fn the_updates_of_a_commit_come_in_the_order_of_their_keys() {
        let mut updates = NodeUpdates::<u8>::new();
        assert!(updates.is_empty());
        let cost = KeyValueStorageCost::default;
        for (key, update) in [
            (&b"pear"[..], NodeUpdate::Remove(cost())),
            (b"", NodeUpdate::Remove(cost())),
            (b"apple", NodeUpdate::Remove(cost())),
            (b"pear", NodeUpdate::Remove(cost())),
            (&[9; 300][..], NodeUpdate::Remove(cost())),
            (b"pea", NodeUpdate::Remove(cost())),
        ] {
            updates.push(key, update);
        }
        let added: Vec<&[u8]> = updates.iter().map(|(key, _)| key).collect();
        assert_eq!(added[0], b"pear");
        assert_eq!(added.len(), 6);

        let (keys, sorted) = updates.into_sorted();
        let sorted: Vec<_> = sorted.collect();
        let order: Vec<(&[u8], usize)> = (sorted.iter())
            .map(|(at, _)| (&keys[at.clone()], at.start))
            .collect();
        assert_eq!(
            order,
            [
                (&b""[..], 4),
                (&[9; 300][..], 13),
                (b"apple", 4),
                (b"pea", 313),
                (b"pear", 0),
                (b"pear", 9),
            ]
        );
    }

    #[test]
    fn updates_of_the_empty_key_come_in_the_order_they_were_added() {
        // The keys of these updates all begin at one place in the buffer.
        let mut updates = NodeUpdates::<u8>::new();
        for key in 1..=23u8 {
            updates.push(&[key], NodeUpdate::Remove(KeyValueStorageCost::default()));
        }
        for added in 0..25 {
            let mut cost = KeyValueStorageCost::default();
            cost.key_storage_cost.added_bytes = added;
            updates.push(b"", NodeUpdate::Remove(cost));
        }
        let (keys, sorted) = updates.into_sorted();
        let order: Vec<(Vec<u8>, u32)> = sorted
            .map(|(at, update)| match update {
                NodeUpdate::Remove(cost) => (keys[at].to_vec(), cost.key_storage_cost.added_bytes),
                NodeUpdate::Write(..) => unreachable!("no write was added"),
            })
            .collect();
        let of_the_empty_key = (0..25).map(|added| (Vec::new(), added));
        let of_the_others = (1..=23u8).map(|key| (vec![key], 0));
        assert_eq!(
            order,
            of_the_empty_key.chain(of_the_others).collect::<Vec<_>>()
        );
    }
}
