// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Merk tree ops

#[cfg(feature = "full")]
use std::{
    collections::{BTreeSet, LinkedList},
    fmt,
};

#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::{
        key_value_cost::KeyValueStorageCost,
        removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
        StorageCost,
    },
    CostContext, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "full")]
use integer_encoding::VarInt;
#[cfg(feature = "full")]
use Op::*;

#[cfg(feature = "full")]
use super::{Fetch, Link, Tree, Walker};
use crate::merk::KeyUpdates;
#[cfg(feature = "full")]
use crate::{error::Error, tree::tree_feature_type::TreeFeatureType, CryptoHash, HASH_LENGTH_U32};

#[cfg(feature = "full")]
/// An operation to be applied to a key in the store.
#[derive(PartialEq, Clone, Eq)]
pub enum Op {
    /// Insert or Update an element into the Merk tree
    Put(Vec<u8>, TreeFeatureType),
    /// Combined references include the value in the node hash
    /// because the value is independent of the reference hash
    /// In GroveDB this is used for references
    PutCombinedReference(Vec<u8>, CryptoHash, TreeFeatureType),
    /// Layered references include the value in the node hash
    /// because the value is independent of the reference hash
    /// In GroveDB this is used for trees
    /// A layered reference does not pay for the tree's value,
    /// instead providing a cost for the value
    PutLayeredReference(Vec<u8>, u32, CryptoHash, TreeFeatureType),
    /// Replacing a layered reference is slightly more efficient
    /// than putting it as the replace will not modify the size
    /// hence there is no need to calculate a difference in
    /// costs
    ReplaceLayeredReference(Vec<u8>, u32, CryptoHash, TreeFeatureType),
    /// Delete an element from the Merk tree
    Delete,
    /// Delete a layered element from the Merk tree, currently the
    /// only layered elements are GroveDB subtrees. A layered
    /// element uses a different calculation for its costs
    DeleteLayered,
    /// Very close to DeleteLayered. A sum layered
    /// element uses a different calculation for its costs.
    DeleteLayeredHavingSum,
}

#[cfg(feature = "full")]
impl fmt::Debug for Op {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(
            f,
            "{}",
            match self {
                Put(value, _) => format!("Put({value:?})"),
                PutCombinedReference(value, referenced_value, feature_type) => format!(
                    "Put Combined Reference({value:?}) for ({referenced_value:?}). \
                     ({feature_type:?})"
                ),
                PutLayeredReference(value, cost, referenced_value, feature_type) => format!(
                    "Put Layered Reference({value:?}) with cost ({cost:?}) for \
                     ({referenced_value:?}). ({feature_type:?})"
                ),
                ReplaceLayeredReference(value, cost, referenced_value, feature_type) => format!(
                    "Replace Layered Reference({value:?}) with cost ({cost:?}) for \
                     ({referenced_value:?}). ({feature_type:?})"
                ),
                Delete => "Delete".to_string(),
                DeleteLayered => "Delete Layered".to_string(),
                DeleteLayeredHavingSum => "Delete Layered Having Sum".to_string(),
            }
        )
    }
}

/// A single `(key, operation)` pair.
pub type BatchEntry<K> = (K, Op);

/// A single `(key, operation, cost)` triple.
pub type AuxBatchEntry<K> = (K, Op, Option<KeyValueStorageCost>);

/// A mapping of keys and operations. Keys should be sorted and unique.
pub type MerkBatch<K> = [BatchEntry<K>];

/// A mapping of keys and operations with potential costs. Keys should be sorted
/// and unique.
pub type AuxMerkBatch<K> = [AuxBatchEntry<K>];

#[cfg(feature = "full")]
/// A source of data which panics when called. Useful when creating a store
/// which always keeps the state in memory.
#[derive(Clone)]
pub struct PanicSource {}

#[cfg(feature = "full")]
impl Fetch for PanicSource {
    fn fetch(&self, _link: &Link) -> CostResult<Tree, Error> {
        unreachable!("'fetch' should not have been called")
    }
}

#[cfg(feature = "full")]
impl<S> Walker<S>
where
    S: Fetch + Sized + Clone,
{
    /// Applies a batch of operations, possibly creating a new tree if
    /// `maybe_tree` is `None`. This is similar to `Walker<S>::apply`, but does
    /// not require a non-empty tree.
    ///
    /// Keys in batch must be sorted and unique.
    pub fn apply_to<K: AsRef<[u8]>, C, R>(
        maybe_tree: Option<Self>,
        batch: &MerkBatch<K>,
        source: S,
        old_tree_cost: &C,
        section_removal_bytes: &mut R,
    ) -> CostContext<Result<(Option<Tree>, KeyUpdates), Error>>
    where
        C: Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        R: FnMut(&Vec<u8>, u32, u32) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        let mut cost = OperationCost::default();

        let (maybe_walker, key_updates) = if batch.is_empty() {
            (
                maybe_tree,
                KeyUpdates::new(
                    BTreeSet::default(),
                    BTreeSet::default(),
                    LinkedList::default(),
                    None,
                ),
            )
        } else {
            match maybe_tree {
                None => {
                    return Self::build(batch, source, old_tree_cost, section_removal_bytes).map_ok(
                        |tree| {
                            let new_keys: BTreeSet<Vec<u8>> = batch
                                .iter()
                                .map(|batch_entry| batch_entry.0.as_ref().to_vec())
                                .collect();
                            (
                                tree,
                                KeyUpdates::new(
                                    new_keys,
                                    BTreeSet::default(),
                                    LinkedList::default(),
                                    None,
                                ),
                            )
                        },
                    )
                }
                Some(tree) => {
                    cost_return_on_error!(
                        &mut cost,
                        tree.apply_sorted(batch, old_tree_cost, section_removal_bytes)
                    )
                }
            }
        };

        let maybe_tree = maybe_walker.map(|walker| walker.into_inner());
        Ok((maybe_tree, key_updates)).wrap_with_cost(cost)
    }

    /// Builds a `Tree` from a batch of operations.
    ///
    /// Keys in batch must be sorted and unique.
    fn build<K: AsRef<[u8]>, C, R>(
        batch: &MerkBatch<K>,
        source: S,
        old_tree_cost: &C,
        section_removal_bytes: &mut R,
    ) -> CostResult<Option<Tree>, Error>
    where
        C: Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        R: FnMut(&Vec<u8>, u32, u32) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        let mut cost = OperationCost::default();

        if batch.is_empty() {
            return Ok(None).wrap_with_cost(cost);
        }

        let mid_index = batch.len() / 2;
        let (mid_key, mid_op) = &batch[mid_index];
        let (mid_value, mid_feature_type) = match mid_op {
            Delete | DeleteLayered | DeleteLayeredHavingSum => {
                let left_batch = &batch[..mid_index];
                let right_batch = &batch[mid_index + 1..];

                let maybe_tree = cost_return_on_error!(
                    &mut cost,
                    Self::build(
                        left_batch,
                        source.clone(),
                        old_tree_cost,
                        section_removal_bytes
                    )
                )
                .map(|tree| Self::new(tree, source.clone()));
                let maybe_tree = match maybe_tree {
                    Some(tree) => {
                        cost_return_on_error!(
                            &mut cost,
                            tree.apply_sorted(right_batch, old_tree_cost, section_removal_bytes)
                        )
                        .0
                    }
                    None => cost_return_on_error!(
                        &mut cost,
                        Self::build(
                            right_batch,
                            source.clone(),
                            old_tree_cost,
                            section_removal_bytes
                        )
                    )
                    .map(|tree| Self::new(tree, source.clone())),
                };
                return Ok(maybe_tree.map(|tree| tree.into())).wrap_with_cost(cost);
            }
            Put(value, feature_type)
            | PutCombinedReference(value, .., feature_type)
            | PutLayeredReference(value, .., feature_type)
            | ReplaceLayeredReference(value, .., feature_type) => (value.to_vec(), feature_type),
        };

        // TODO: take from batch so we don't have to clone

        let mid_tree = match mid_op {
            Put(..) => Tree::new(
                mid_key.as_ref().to_vec(),
                mid_value.to_vec(),
                mid_feature_type.to_owned(),
            )
            .unwrap_add_cost(&mut cost),
            PutCombinedReference(_, referenced_value, _) => Tree::new_with_combined_value_hash(
                mid_key.as_ref().to_vec(),
                mid_value,
                referenced_value.to_owned(),
                mid_feature_type.to_owned(),
            )
            .unwrap_add_cost(&mut cost),
            PutLayeredReference(_, value_cost, referenced_value, _)
            | ReplaceLayeredReference(_, value_cost, referenced_value, _) => {
                Tree::new_with_layered_value_hash(
                    mid_key.as_ref().to_vec(),
                    mid_value,
                    *value_cost,
                    referenced_value.to_owned(),
                    mid_feature_type.to_owned(),
                )
                .unwrap_add_cost(&mut cost)
            }
            Delete | DeleteLayered | DeleteLayeredHavingSum => {
                unreachable!("cannot get here, should return at the top")
            }
        };
        let mid_walker = Walker::new(mid_tree, PanicSource {});

        // use walker, ignore deleted_keys since it should be empty
        Ok(cost_return_on_error!(
            &mut cost,
            mid_walker.recurse(
                batch,
                mid_index,
                true,
                KeyUpdates::new(
                    BTreeSet::default(),
                    BTreeSet::default(),
                    LinkedList::default(),
                    None
                ),
                old_tree_cost,
                section_removal_bytes,
            )
        )
        .0
        .map(|w| w.into_inner()))
        .wrap_with_cost(cost)
    }

    #[allow(dead_code)]
    fn apply_sorted_without_costs<K: AsRef<[u8]>>(
        self,
        batch: &MerkBatch<K>,
    ) -> CostResult<(Option<Self>, KeyUpdates), Error> {
        self.apply_sorted(
            batch,
            &|_, _| Ok(0),
            &mut |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
        )
    }

    /// Applies a batch of operations to an existing tree. This is similar to
    /// `Walker<S>::apply`_to, but requires a populated tree.
    ///
    /// Keys in batch must be sorted and unique.
    fn apply_sorted<K: AsRef<[u8]>, C, R>(
        self,
        batch: &MerkBatch<K>,
        old_tree_cost: &C,
        section_removal_bytes: &mut R,
    ) -> CostResult<(Option<Self>, KeyUpdates), Error>
    where
        C: Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        R: FnMut(&Vec<u8>, u32, u32) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        let mut cost = OperationCost::default();

        let key_vec = self.tree().key().to_vec();
        // binary search to see if this node's key is in the batch, and to split
        // into left and right batches
        let search = batch.binary_search_by(|(key, _op)| key.as_ref().cmp(self.tree().key()));

        let tree = if let Ok(index) = search {
            let (_, op) = &batch[index];

            // a key matches this node's key, apply op to this node
            match op {
                // TODO: take vec from batch so we don't need to clone
                Put(value, feature_type) => self
                    .put_value(value.to_vec(), feature_type.to_owned())
                    .unwrap_add_cost(&mut cost),
                PutCombinedReference(value, referenced_value, feature_type) => self
                    .put_value_and_reference_value_hash(
                        value.to_vec(),
                        referenced_value.to_owned(),
                        feature_type.to_owned(),
                    )
                    .unwrap_add_cost(&mut cost),
                PutLayeredReference(value, value_cost, referenced_value, feature_type)
                | ReplaceLayeredReference(value, value_cost, referenced_value, feature_type) => {
                    self.put_value_with_reference_value_hash_and_value_cost(
                        value.to_vec(),
                        referenced_value.to_owned(),
                        *value_cost,
                        feature_type.to_owned(),
                    )
                    .unwrap_add_cost(&mut cost)
                }
                Delete | DeleteLayered | DeleteLayeredHavingSum => {
                    // TODO: we shouldn't have to do this as 2 different calls to apply
                    let source = self.clone_source();
                    let wrap = |maybe_tree: Option<Tree>| {
                        maybe_tree.map(|tree| Self::new(tree, source.clone()))
                    };
                    let key = self.tree().key().to_vec();
                    let key_len = key.len() as u32;

                    let prefixed_key_len = HASH_LENGTH_U32 + key_len;
                    let total_key_len = prefixed_key_len + prefixed_key_len.required_space() as u32;

                    let deletion_cost = match &batch[index].1 {
                        Delete | DeleteLayered | DeleteLayeredHavingSum => {
                            let value = self.tree().value_ref();

                            let old_cost = match &batch[index].1 {
                                Delete => self.tree().inner.kv.value_byte_cost_size(),
                                DeleteLayered | DeleteLayeredHavingSum => {
                                    cost_return_on_error_no_add!(&cost, old_tree_cost(&key, value))
                                }
                                _ => 0, // can't get here anyways
                            };

                            let (r_key_cost, r_value_cost) = cost_return_on_error_no_add!(
                                &cost,
                                section_removal_bytes(value, total_key_len, old_cost)
                            );
                            Some(KeyValueStorageCost {
                                key_storage_cost: StorageCost {
                                    added_bytes: 0,
                                    replaced_bytes: 0,
                                    removed_bytes: r_key_cost,
                                },
                                value_storage_cost: StorageCost {
                                    added_bytes: 0,
                                    replaced_bytes: 0,
                                    removed_bytes: r_value_cost,
                                },
                                new_node: false,
                                needs_value_verification: false,
                            })
                        }
                        _ => None,
                    };

                    let maybe_tree = cost_return_on_error!(&mut cost, self.remove());

                    #[rustfmt::skip]
                    let (maybe_tree, mut key_updates)
                        = cost_return_on_error!(
                        &mut cost,
                        Self::apply_to(
                            maybe_tree,
                            &batch[..index],
                            source.clone(),
                            old_tree_cost,
                            section_removal_bytes
                        )
                    );
                    let maybe_walker = wrap(maybe_tree);

                    let (maybe_tree, mut key_updates_right) = cost_return_on_error!(
                        &mut cost,
                        Self::apply_to(
                            maybe_walker,
                            &batch[index + 1..],
                            source.clone(),
                            old_tree_cost,
                            section_removal_bytes
                        )
                    );
                    let maybe_walker = wrap(maybe_tree);

                    key_updates.new_keys.append(&mut key_updates_right.new_keys);
                    key_updates
                        .updated_keys
                        .append(&mut key_updates_right.updated_keys);
                    key_updates
                        .deleted_keys
                        .append(&mut key_updates_right.deleted_keys);
                    key_updates.deleted_keys.push_back((key, deletion_cost));
                    key_updates.updated_root_key_from = Some(key_vec);

                    return Ok((maybe_walker, key_updates)).wrap_with_cost(cost);
                }
            }
        } else {
            self
        };

        let (mid, exclusive) = match search {
            Ok(index) => (index, true),
            Err(index) => (index, false),
        };

        let mut updated_keys = BTreeSet::new();
        let mut new_keys = BTreeSet::new();
        if exclusive {
            updated_keys.insert(key_vec);
        } else {
            new_keys.insert(key_vec);
        }

        tree.recurse(
            batch,
            mid,
            exclusive,
            KeyUpdates::new(new_keys, updated_keys, LinkedList::default(), None),
            old_tree_cost,
            section_removal_bytes,
        )
        .add_cost(cost)
    }

    /// Recursively applies operations to the tree's children (if there are any
    /// operations for them).
    ///
    /// This recursion executes serially in the same thread, but in the future
    /// will be dispatched to workers in other threads.
    fn recurse<K: AsRef<[u8]>, C, R>(
        self,
        batch: &MerkBatch<K>,
        mid: usize,
        exclusive: bool,
        mut key_updates: KeyUpdates,
        old_tree_cost: &C,
        section_removal_bytes: &mut R,
    ) -> CostResult<(Option<Self>, KeyUpdates), Error>
    where
        C: Fn(&Vec<u8>, &Vec<u8>) -> Result<u32, Error>,
        R: FnMut(&Vec<u8>, u32, u32) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        let mut cost = OperationCost::default();

        let left_batch = &batch[..mid];
        let right_batch = if exclusive {
            &batch[mid + 1..]
        } else {
            &batch[mid..]
        };

        let old_root_key = self.tree().key().to_vec();

        let tree = if !left_batch.is_empty() {
            let source = self.clone_source();
            cost_return_on_error!(
                &mut cost,
                self.walk(true, |maybe_left| {
                    Self::apply_to(
                        maybe_left,
                        left_batch,
                        source,
                        old_tree_cost,
                        section_removal_bytes,
                    )
                    .map_ok(|(maybe_left, mut key_updates_left)| {
                        key_updates.new_keys.append(&mut key_updates_left.new_keys);
                        key_updates
                            .updated_keys
                            .append(&mut key_updates_left.updated_keys);
                        key_updates
                            .deleted_keys
                            .append(&mut key_updates_left.deleted_keys);
                        maybe_left
                    })
                })
            )
        } else {
            self
        };

        let tree = if !right_batch.is_empty() {
            let source = tree.clone_source();
            cost_return_on_error!(
                &mut cost,
                tree.walk(false, |maybe_right| {
                    Self::apply_to(
                        maybe_right,
                        right_batch,
                        source,
                        old_tree_cost,
                        section_removal_bytes,
                    )
                    .map_ok(|(maybe_right, mut key_updates_right)| {
                        key_updates.new_keys.append(&mut key_updates_right.new_keys);
                        key_updates
                            .updated_keys
                            .append(&mut key_updates_right.updated_keys);
                        key_updates
                            .deleted_keys
                            .append(&mut key_updates_right.deleted_keys);
                        maybe_right
                    })
                })
            )
        } else {
            tree
        };

        let tree = cost_return_on_error!(&mut cost, tree.maybe_balance());

        let new_root_key = tree.tree().key();

        let updated_from = if !old_root_key.eq(new_root_key) {
            Some(old_root_key)
        } else {
            None
        };
        key_updates.updated_root_key_from = updated_from;

        Ok((Some(tree), key_updates)).wrap_with_cost(cost)
    }

    /// Gets the wrapped tree's balance factor.
    #[inline]
    fn balance_factor(&self) -> i8 {
        self.tree().balance_factor()
    }

    /// Checks if the tree is unbalanced and if so, applies AVL tree rotation(s)
    /// to rebalance the tree and its subtrees. Returns the root node of the
    /// balanced tree after applying the rotations.
    fn maybe_balance(self) -> CostResult<Self, Error> {
        let mut cost = OperationCost::default();

        let balance_factor = self.balance_factor();
        if balance_factor.abs() <= 1 {
            return Ok(self).wrap_with_cost(cost);
        }

        let left = balance_factor < 0;

        // maybe do a double rotation
        let tree = if left == (self.tree().link(left).unwrap().balance_factor() > 0) {
            cost_return_on_error!(
                &mut cost,
                self.walk_expect(left, |child| child.rotate(!left).map_ok(Option::Some))
            )
        } else {
            self
        };

        let rotate = tree.rotate(left).unwrap_add_cost(&mut cost);
        rotate.wrap_with_cost(cost)
    }

    /// Applies an AVL tree rotation, a constant-time operation which only needs
    /// to swap pointers in order to rebalance a tree.
    fn rotate(self, left: bool) -> CostResult<Self, Error> {
        let mut cost = OperationCost::default();

        let (tree, child) = cost_return_on_error!(&mut cost, self.detach_expect(left));
        let (child, maybe_grandchild) = cost_return_on_error!(&mut cost, child.detach(!left));

        // attach grandchild to self
        tree.attach(left, maybe_grandchild)
            .maybe_balance()
            .flat_map_ok(|tree| {
                // attach self to child, return child
                child.attach(!left, Some(tree)).maybe_balance()
            })
            .add_cost(cost)
    }

    /// Removes the root node from the tree. Rearranges and rebalances
    /// descendants (if any) in order to maintain a valid tree.
    pub fn remove(self) -> CostResult<Option<Self>, Error> {
        let mut cost = OperationCost::default();

        let tree = self.tree();
        let has_left = tree.link(true).is_some();
        let has_right = tree.link(false).is_some();
        let left = tree.child_height(true) > tree.child_height(false);

        let maybe_tree = if has_left && has_right {
            // two children, promote edge of taller child
            let (tree, tall_child) = cost_return_on_error!(&mut cost, self.detach_expect(left));
            let (_, short_child) = cost_return_on_error!(&mut cost, tree.detach_expect(!left));
            let promoted =
                cost_return_on_error!(&mut cost, tall_child.promote_edge(!left, short_child));
            Some(promoted)
        } else if has_left || has_right {
            // single child, promote it
            Some(cost_return_on_error!(&mut cost, self.detach_expect(left)).1)
        } else {
            // no child
            None
        };

        Ok(maybe_tree).wrap_with_cost(cost)
    }

    /// Traverses to find the tree's edge on the given side, removes it, and
    /// reattaches it at the top in order to fill in a gap when removing a root
    /// node from a tree with both left and right children. Attaches `attach` on
    /// the opposite side. Returns the promoted node.
    fn promote_edge(self, left: bool, attach: Self) -> CostResult<Self, Error> {
        self.remove_edge(left).flat_map_ok(|(edge, maybe_child)| {
            edge.attach(!left, maybe_child)
                .attach(left, Some(attach))
                .maybe_balance()
        })
    }

    /// Traverses to the tree's edge on the given side and detaches it
    /// (reattaching its child, if any, to its former parent). Return value is
    /// `(edge, maybe_updated_tree)`.
    fn remove_edge(self, left: bool) -> CostResult<(Self, Option<Self>), Error> {
        let mut cost = OperationCost::default();

        if self.tree().link(left).is_some() {
            // this node is not the edge, recurse
            let (tree, child) = cost_return_on_error!(&mut cost, self.detach_expect(left));
            let (edge, maybe_child) = cost_return_on_error!(&mut cost, child.remove_edge(left));
            tree.attach(left, maybe_child)
                .maybe_balance()
                .map_ok(|tree| (edge, Some(tree)))
                .add_cost(cost)
        } else {
            // this node is the edge, detach its child if present
            self.detach(!left)
        }
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        test_utils::{apply_memonly, assert_tree_invariants, del_entry, make_tree_seq, seq_key},
        tree::{tree_feature_type::TreeFeatureType::BasicMerk, *},
    };

    #[test]
    fn simple_insert() {
        let batch = [(b"foo2".to_vec(), Op::Put(b"bar2".to_vec(), BasicMerk))];
        let tree = Tree::new(b"foo".to_vec(), b"bar".to_vec(), BasicMerk).unwrap();
        let (maybe_walker, key_updates) = Walker::new(tree, PanicSource {})
            .apply_sorted_without_costs(&batch)
            .unwrap()
            .expect("apply errored");
        let walker = maybe_walker.expect("should be Some");
        assert_eq!(walker.tree().key(), b"foo");
        assert_eq!(walker.into_inner().child(false).unwrap().key(), b"foo2");
        assert!(key_updates.updated_keys.is_empty());
        assert!(key_updates.deleted_keys.is_empty());
        assert_eq!(key_updates.new_keys.len(), 2)
    }

    #[test]
    fn simple_update() {
        let batch = [(b"foo".to_vec(), Op::Put(b"bar2".to_vec(), BasicMerk))];
        let tree = Tree::new(b"foo".to_vec(), b"bar".to_vec(), BasicMerk).unwrap();
        let (maybe_walker, key_updates) = Walker::new(tree, PanicSource {})
            .apply_sorted_without_costs(&batch)
            .unwrap()
            .expect("apply errored");
        let walker = maybe_walker.expect("should be Some");
        assert_eq!(walker.tree().key(), b"foo");
        assert_eq!(walker.tree().value_as_slice(), b"bar2");
        assert!(walker.tree().link(true).is_none());
        assert!(walker.tree().link(false).is_none());
        assert!(!key_updates.updated_keys.is_empty());
        assert!(key_updates.deleted_keys.is_empty());
    }

    #[test]
    fn simple_delete() {
        let batch = [(b"foo2".to_vec(), Op::Delete)];
        let tree = Tree::from_fields(
            b"foo".to_vec(),
            b"bar".to_vec(),
            [123; 32],
            None,
            Some(Link::Loaded {
                hash: [123; 32],
                sum: None,
                child_heights: (0, 0),
                tree: Tree::new(b"foo2".to_vec(), b"bar2".to_vec(), BasicMerk).unwrap(),
            }),
            BasicMerk,
        )
        .unwrap();
        let (maybe_walker, key_updates) = Walker::new(tree, PanicSource {})
            .apply_sorted_without_costs(&batch)
            .unwrap()
            .expect("apply errored");
        let walker = maybe_walker.expect("should be Some");
        assert_eq!(walker.tree().key(), b"foo");
        assert_eq!(walker.tree().value_as_slice(), b"bar");
        assert!(walker.tree().link(true).is_none());
        assert!(walker.tree().link(false).is_none());
        assert!(key_updates.updated_keys.is_empty());
        assert_eq!(key_updates.deleted_keys.len(), 1);
        assert_eq!(
            key_updates.deleted_keys.front().unwrap().0.as_slice(),
            b"foo2"
        );
    }

    #[test]
    fn delete_non_existent() {
        let batch = [(b"foo2".to_vec(), Op::Delete)];
        let tree = Tree::new(b"foo".to_vec(), b"bar".to_vec(), BasicMerk).unwrap();
        Walker::new(tree, PanicSource {})
            .apply_sorted_without_costs(&batch)
            .unwrap()
            .unwrap();
    }

    #[test]
    fn delete_only_node() {
        let batch = [(b"foo".to_vec(), Op::Delete)];
        let tree = Tree::new(b"foo".to_vec(), b"bar".to_vec(), BasicMerk).unwrap();
        let (maybe_walker, key_updates) = Walker::new(tree, PanicSource {})
            .apply_sorted_without_costs(&batch)
            .unwrap()
            .expect("apply errored");
        assert!(maybe_walker.is_none());
        assert!(key_updates.updated_keys.is_empty());
        assert_eq!(key_updates.deleted_keys.len(), 1);
        assert_eq!(
            key_updates.deleted_keys.front().unwrap().0.as_slice(),
            b"foo"
        );
    }

    #[test]
    fn delete_deep() {
        let tree = make_tree_seq(50);
        let batch = [del_entry(5)];
        let (maybe_walker, key_updates) = Walker::new(tree, PanicSource {})
            .apply_sorted_without_costs(&batch)
            .unwrap()
            .expect("apply errored");
        maybe_walker.expect("should be Some");
        assert!(key_updates.updated_keys.is_empty());
        assert_eq!(key_updates.deleted_keys.len(), 1);
        assert_eq!(
            key_updates.deleted_keys.front().unwrap().0.as_slice(),
            seq_key(5)
        );
    }

    #[test]
    fn delete_recursive() {
        let tree = make_tree_seq(50);
        let batch = [del_entry(29), del_entry(34)];
        let (maybe_walker, mut key_updates) = Walker::new(tree, PanicSource {})
            .apply_sorted_without_costs(&batch)
            .unwrap()
            .expect("apply errored");
        maybe_walker.expect("should be Some");
        assert!(key_updates.updated_keys.is_empty());
        assert_eq!(key_updates.deleted_keys.len(), 2);
        assert_eq!(
            key_updates.deleted_keys.pop_front().unwrap().0.as_slice(),
            seq_key(29)
        );
        assert_eq!(
            key_updates.deleted_keys.pop_front().unwrap().0.as_slice(),
            seq_key(34)
        );
    }

    #[test]
    fn delete_recursive_2() {
        let tree = make_tree_seq(10);
        let batch = [del_entry(7), del_entry(9)];
        let (maybe_walker, key_updates) = Walker::new(tree, PanicSource {})
            .apply_sorted_without_costs(&batch)
            .unwrap()
            .expect("apply errored");
        maybe_walker.expect("should be Some");
        let mut deleted_keys: Vec<&Vec<u8>> =
            key_updates.deleted_keys.iter().map(|(v, _)| v).collect();
        deleted_keys.sort();
        assert!(key_updates.updated_keys.is_empty());
        assert_eq!(deleted_keys, vec![&seq_key(7), &seq_key(9)]);
    }

    #[test]
    fn apply_empty_none() {
        let (maybe_tree, key_updates) = Walker::<PanicSource>::apply_to::<Vec<u8>, _, _>(
            None,
            &[],
            PanicSource {},
            &|_, _| Ok(0),
            &mut |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
        )
        .unwrap()
        .expect("apply_to failed");
        assert!(maybe_tree.is_none());
        assert!(key_updates.updated_keys.is_empty());
        assert!(key_updates.deleted_keys.is_empty());
    }

    #[test]
    fn insert_empty_single() {
        let batch = vec![(vec![0], Op::Put(vec![1], BasicMerk))];
        let (maybe_tree, key_updates) = Walker::<PanicSource>::apply_to(
            None,
            &batch,
            PanicSource {},
            &|_, _| Ok(0),
            &mut |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
        )
        .unwrap()
        .expect("apply_to failed");
        let tree = maybe_tree.expect("expected tree");
        assert_eq!(tree.key(), &[0]);
        assert_eq!(tree.value_as_slice(), &[1]);
        assert_tree_invariants(&tree);
        assert!(key_updates.updated_keys.is_empty());
        assert!(key_updates.deleted_keys.is_empty());
    }

    #[test]
    fn insert_updated_single() {
        let batch = vec![(vec![0], Op::Put(vec![1], BasicMerk))];
        let (maybe_tree, key_updates) = Walker::<PanicSource>::apply_to(
            None,
            &batch,
            PanicSource {},
            &|_, _| Ok(0),
            &mut |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
        )
        .unwrap()
        .expect("apply_to failed");
        assert!(key_updates.updated_keys.is_empty());
        assert!(key_updates.deleted_keys.is_empty());

        let maybe_walker = maybe_tree.map(|tree| Walker::<PanicSource>::new(tree, PanicSource {}));
        let batch = vec![
            (vec![0], Op::Put(vec![2], BasicMerk)),
            (vec![1], Op::Put(vec![2], BasicMerk)),
        ];
        let (maybe_tree, key_updates) = Walker::<PanicSource>::apply_to(
            maybe_walker,
            &batch,
            PanicSource {},
            &|_, _| Ok(0),
            &mut |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
        )
        .unwrap()
        .expect("apply_to failed");
        let tree = maybe_tree.expect("expected tree");
        assert_eq!(tree.key(), &[0]);
        assert_eq!(tree.value_as_slice(), &[2]);
        assert_eq!(key_updates.updated_keys.len(), 1);
        assert!(key_updates.deleted_keys.is_empty());
    }

    #[test]
    fn insert_updated_multiple() {
        let batch = vec![
            (vec![0], Op::Put(vec![1], BasicMerk)),
            (vec![1], Op::Put(vec![2], BasicMerk)),
            (vec![2], Op::Put(vec![3], BasicMerk)),
        ];
        let (maybe_tree, key_updates) = Walker::<PanicSource>::apply_to(
            None,
            &batch,
            PanicSource {},
            &|_, _| Ok(0),
            &mut |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
        )
        .unwrap()
        .expect("apply_to failed");
        assert!(key_updates.updated_keys.is_empty());
        assert!(key_updates.deleted_keys.is_empty());

        let maybe_walker = maybe_tree.map(|tree| Walker::<PanicSource>::new(tree, PanicSource {}));
        let batch = vec![
            (vec![0], Op::Put(vec![5], BasicMerk)),
            (vec![1], Op::Put(vec![8], BasicMerk)),
            (vec![2], Op::Delete),
        ];
        let (maybe_tree, key_updates) = Walker::<PanicSource>::apply_to(
            maybe_walker,
            &batch,
            PanicSource {},
            &|_, _| Ok(0),
            &mut |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
        )
        .unwrap()
        .expect("apply_to failed");
        let tree = maybe_tree.expect("expected tree");
        assert_eq!(tree.key(), &[1]);
        assert_eq!(tree.value_as_slice(), &[8]);
        assert_eq!(key_updates.updated_keys.len(), 2);
        assert_eq!(key_updates.updated_keys, BTreeSet::from([vec![0], vec![1]]));
        assert_eq!(key_updates.deleted_keys.len(), 1);
    }

    #[test]
    fn insert_root_single() {
        let tree = Tree::new(vec![5], vec![123], BasicMerk).unwrap();
        let batch = vec![(vec![6], Op::Put(vec![123], BasicMerk))];
        let tree = apply_memonly(tree, &batch);
        assert_eq!(tree.key(), &[5]);
        assert!(tree.child(true).is_none());
        assert_eq!(tree.child(false).expect("expected child").key(), &[6]);
    }

    #[test]
    fn insert_root_double() {
        let tree = Tree::new(vec![5], vec![123], BasicMerk).unwrap();
        let batch = vec![
            (vec![4], Op::Put(vec![123], BasicMerk)),
            (vec![6], Op::Put(vec![123], BasicMerk)),
        ];
        let tree = apply_memonly(tree, &batch);
        assert_eq!(tree.key(), &[5]);
        assert_eq!(tree.child(true).expect("expected child").key(), &[4]);
        assert_eq!(tree.child(false).expect("expected child").key(), &[6]);
    }

    #[test]
    fn insert_rebalance() {
        let tree = Tree::new(vec![5], vec![123], BasicMerk).unwrap();

        let batch = vec![(vec![6], Op::Put(vec![123], BasicMerk))];
        let tree = apply_memonly(tree, &batch);

        let batch = vec![(vec![7], Op::Put(vec![123], BasicMerk))];
        let tree = apply_memonly(tree, &batch);

        assert_eq!(tree.key(), &[6]);
        assert_eq!(tree.child(true).expect("expected child").key(), &[5]);
        assert_eq!(tree.child(false).expect("expected child").key(), &[7]);
    }

    #[test]
    fn insert_100_sequential() {
        let mut tree = Tree::new(vec![0], vec![123], BasicMerk).unwrap();

        for i in 0..100 {
            let batch = vec![(vec![i + 1], Op::Put(vec![123], BasicMerk))];
            tree = apply_memonly(tree, &batch);
        }

        assert_eq!(tree.key(), &[63]);
        assert_eq!(tree.child(true).expect("expected child").key(), &[31]);
        assert_eq!(tree.child(false).expect("expected child").key(), &[79]);
    }
}
