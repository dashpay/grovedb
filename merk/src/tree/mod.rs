mod commit;
#[cfg(feature = "full")]
mod debug;
mod encoding;
mod fuzz_tests;
mod hash;
mod iter;
pub mod kv;
mod link;
mod ops;
mod walk;

use std::cmp::{max, Ordering};

use anyhow::Result;
pub use commit::{Commit, NoopCommit};
use costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::{
        key_value_cost::KeyValueStorageCost,
        removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
        StorageCost,
    },
    CostContext, CostsExt, OperationCost,
};
use ed::{Decode, Encode, Terminated};
pub use hash::{
    combine_hash, kv_digest_to_kv_hash, kv_hash, node_hash, value_hash, CryptoHash,
    HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32, HASH_LENGTH, HASH_LENGTH_U32, HASH_LENGTH_U32_X2,
    NULL_HASH,
};
use kv::KV;
pub use link::Link;
pub use ops::{AuxMerkBatch, BatchEntry, MerkBatch, Op, PanicSource};
pub use walk::{Fetch, RefWalker, Walker};

// TODO: remove need for `TreeInner`, and just use `Box<Self>` receiver for
// relevant methods

/// The fields of the `Tree` type, stored on the heap.
#[derive(Clone, Encode, Decode, Debug)]
pub struct TreeInner {
    pub(crate) left: Option<Link>,
    pub(crate) right: Option<Link>,
    pub(crate) kv: KV,
}

impl TreeInner {
    /// Get the value as owned of the key value struct
    pub fn value_as_owned(self) -> Vec<u8> {
        self.kv.value
    }

    /// Get the value as slice of the key value struct
    pub fn value_as_slice(&self) -> &[u8] {
        self.kv.value.as_slice()
    }

    /// Get the key as owned of the key value struct
    pub fn key_as_owned(self) -> Vec<u8> {
        self.kv.key
    }

    /// Get the key as slice of the key value struct
    pub fn key_as_slice(&self) -> &[u8] {
        self.kv.key.as_slice()
    }
}

impl Terminated for Box<TreeInner> {}

/// A binary AVL tree data structure, with Merkle hashes.
///
/// Trees' inner fields are stored on the heap so that nodes can recursively
/// link to each other, and so we can detach nodes from their parents, then
/// reattach without allocating or freeing heap memory.
#[derive(Clone)]
pub struct Tree {
    pub(crate) inner: Box<TreeInner>,
    pub(crate) old_size_with_parent_to_child_hook: u32,
    pub(crate) old_value: Option<Vec<u8>>,
}

impl Tree {
    /// Creates a new `Tree` with the given key and value, and no children.
    ///
    /// Hashes the key/value pair and initializes the `kv_hash` field.
    pub fn new(key: Vec<u8>, value: Vec<u8>) -> CostContext<Self> {
        KV::new(key, value).map(|kv| Self {
            inner: Box::new(TreeInner {
                kv,
                left: None,
                right: None,
            }),
            old_size_with_parent_to_child_hook: 0,
            old_value: None,
        })
    }

    /// Creates a new `Tree` given an inner tree
    pub fn new_with_tree_inner(inner_tree: TreeInner) -> Self {
        let decode_size = inner_tree.kv.value_byte_cost_size();
        let old_value = inner_tree.kv.value.clone();
        Self {
            inner: Box::new(inner_tree),
            old_size_with_parent_to_child_hook: decode_size as u32,
            old_value: Some(old_value),
        }
    }

    pub fn kv_with_parent_hook_size_and_storage_cost_from_old_cost(
        &self,
        current_value_byte_cost: u32,
        old_cost: u32,
    ) -> Result<(u32, KeyValueStorageCost)> {
        let key_storage_cost = StorageCost {
            ..Default::default()
        };
        let mut value_storage_cost = StorageCost {
            ..Default::default()
        };

        // Update the value storage_cost cost
        match old_cost.cmp(&current_value_byte_cost) {
            Ordering::Equal => {
                value_storage_cost.replaced_bytes += old_cost;
            }
            Ordering::Greater => {
                // old size is greater than current size, storage_cost will be freed
                value_storage_cost.replaced_bytes += current_value_byte_cost;
                value_storage_cost.removed_bytes +=
                    BasicStorageRemoval(old_cost - current_value_byte_cost);
            }
            Ordering::Less => {
                // current size is greater than old size, storage_cost will be created
                // this also handles the case where the tree.old_size = 0
                value_storage_cost.replaced_bytes += old_cost;
                value_storage_cost.added_bytes += current_value_byte_cost - old_cost;
            }
        }

        let key_value_storage_cost = KeyValueStorageCost {
            key_storage_cost, // the key storage cost is added later
            value_storage_cost,
            new_node: self.old_size_with_parent_to_child_hook == 0,
            needs_value_verification: self.inner.kv.value_defined_cost.is_none(),
        };

        Ok((current_value_byte_cost, key_value_storage_cost))
    }

    pub fn kv_with_parent_hook_size_and_storage_cost(
        &self,
        old_tree_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32>,
    ) -> Result<(u32, KeyValueStorageCost)> {
        let current_value_byte_cost =
            self.value_encoding_length_with_parent_to_child_reference() as u32;

        let old_cost = if self.inner.kv.value_defined_cost.is_some() && self.old_value.is_some() {
            old_tree_cost(self.key_as_ref(), self.old_value.as_ref().unwrap())
        } else {
            Ok(self.old_size_with_parent_to_child_hook)
        }?;

        self.kv_with_parent_hook_size_and_storage_cost_from_old_cost(
            current_value_byte_cost,
            old_cost,
        )
    }

    /// Creates a new `Tree` with the given key, value and value hash, and no
    /// children.
    ///
    /// Hashes the key/value pair and initializes the `kv_hash` field.
    pub fn new_with_value_hash(
        key: Vec<u8>,
        value: Vec<u8>,
        value_hash: CryptoHash,
    ) -> CostContext<Self> {
        KV::new_with_value_hash(key, value, value_hash).map(|kv| Self {
            inner: Box::new(TreeInner {
                kv,
                left: None,
                right: None,
            }),
            old_size_with_parent_to_child_hook: 0,
            old_value: None,
        })
    }

    /// Creates a new `Tree` with the given key, value and value hash, and no
    /// children.
    /// Sets the tree's value_hash = hash(value, supplied_value_hash)
    pub fn new_with_combined_value_hash(
        key: Vec<u8>,
        value: Vec<u8>,
        value_hash: CryptoHash,
    ) -> CostContext<Self> {
        KV::new_with_combined_value_hash(key, value, value_hash).map(|kv| Self {
            inner: Box::new(TreeInner {
                kv,
                left: None,
                right: None,
            }),
            old_size_with_parent_to_child_hook: 0,
            old_value: None,
        })
    }

    /// Creates a new `Tree` with the given key, value, value cost and value
    /// hash, and no children.
    /// Sets the tree's value_hash = hash(value, supplied_value_hash)
    pub fn new_with_layered_value_hash(
        key: Vec<u8>,
        value: Vec<u8>,
        value_cost: u32,
        value_hash: CryptoHash,
    ) -> CostContext<Self> {
        KV::new_with_layered_value_hash(key, value, value_cost, value_hash).map(|kv| Self {
            inner: Box::new(TreeInner {
                kv,
                left: None,
                right: None,
            }),
            old_size_with_parent_to_child_hook: 0,
            old_value: None,
        })
    }

    /// Creates a `Tree` by supplying all the raw struct fields (mainly useful
    /// for testing). The `kv_hash` and `Link`s are not ensured to be correct.
    pub fn from_fields(
        key: Vec<u8>,
        value: Vec<u8>,
        kv_hash: CryptoHash,
        left: Option<Link>,
        right: Option<Link>,
    ) -> CostContext<Self> {
        value_hash(value.as_slice()).map(|vh| Self {
            inner: Box::new(TreeInner {
                kv: KV::from_fields(key, value, kv_hash, vh),
                left,
                right,
            }),
            old_size_with_parent_to_child_hook: 0,
            old_value: None,
        })
    }

    /// Returns the root node's key as a slice.
    #[inline]
    pub fn key(&self) -> &[u8] {
        self.inner.kv.key()
    }

    /// Returns the root node's key as a slice.
    #[inline]
    pub fn key_as_ref(&self) -> &Vec<u8> {
        self.inner.kv.key_as_ref()
    }

    pub fn set_key(&mut self, key: Vec<u8>) {
        self.inner.kv.key = key;
    }

    /// Consumes the tree and returns its root node's key, without having to
    /// clone or allocate.
    #[inline]
    pub fn take_key(self) -> Vec<u8> {
        self.inner.kv.take_key()
    }

    /// Returns the root node's value as a ref.
    #[inline]
    pub fn value_ref(&self) -> &Vec<u8> {
        self.inner.kv.value.as_ref()
    }

    /// Returns the root node's value as a ref.
    #[inline]
    pub fn value_mut_ref(&mut self) -> &mut Vec<u8> {
        &mut self.inner.kv.value
    }

    /// Returns the root node's value as a slice.
    #[inline]
    pub fn value_as_slice(&self) -> &[u8] {
        self.inner.kv.value_as_slice()
    }

    /// Returns the hash of the root node's key/value pair.
    #[inline]
    pub const fn kv_hash(&self) -> &CryptoHash {
        self.inner.kv.hash()
    }

    /// Returns the hash of the node's valu
    #[inline]
    pub const fn value_hash(&self) -> &CryptoHash {
        self.inner.kv.value_hash()
    }

    /// Returns a reference to the root node's `Link` on the given side, if any.
    /// If there is no child, returns `None`.
    #[inline]
    pub const fn link(&self, left: bool) -> Option<&Link> {
        if left {
            self.inner.left.as_ref()
        } else {
            self.inner.right.as_ref()
        }
    }

    /// Returns a mutable reference to the root node's `Link` on the given side,
    /// if any. If there is no child, returns `None`.
    #[inline]
    pub fn link_mut(&mut self, left: bool) -> Option<&mut Link> {
        if left {
            self.inner.left.as_mut()
        } else {
            self.inner.right.as_mut()
        }
    }

    /// Returns a the size of node's child on the given side, if any.
    /// If there is no child, returns `None`.
    pub fn child_ref_size(&self, left: bool) -> Option<u32> {
        self.link(left).map(|link| link.key().len() as u32 + 35)
    }

    /// Returns a reference to the root node's child on the given side, if any.
    /// If there is no child, returns `None`.
    #[inline]
    pub const fn child(&self, left: bool) -> Option<&Self> {
        match self.link(left) {
            None => None,
            Some(link) => link.tree(),
        }
    }

    /// Returns a mutable reference to the root node's child on the given side,
    /// if any. If there is no child, returns `None`.
    #[inline]
    pub fn child_mut(&mut self, left: bool) -> Option<&mut Self> {
        match self.slot_mut(left).as_mut() {
            None => None,
            Some(Link::Reference { .. }) => None,
            Some(Link::Modified { tree, .. }) => Some(tree),
            Some(Link::Uncommitted { tree, .. }) => Some(tree),
            Some(Link::Loaded { tree, .. }) => Some(tree),
        }
    }

    /// Returns the hash of the root node's child on the given side, if any. If
    /// there is no child, returns the null hash (zero-filled).
    #[inline]
    pub const fn child_hash(&self, left: bool) -> &CryptoHash {
        match self.link(left) {
            Some(link) => link.hash(),
            _ => &NULL_HASH,
        }
    }

    /// Computes and returns the hash of the root node.
    #[inline]
    pub fn hash(&self) -> CostContext<CryptoHash> {
        // TODO: should we compute node hash as we already have a node hash?
        node_hash(
            self.inner.kv.hash(),
            self.child_hash(true),
            self.child_hash(false),
        )
    }

    /// Returns the number of pending writes for the child on the given side, if
    /// any. If there is no child, returns 0.
    #[inline]
    pub const fn child_pending_writes(&self, left: bool) -> usize {
        match self.link(left) {
            Some(Link::Modified { pending_writes, .. }) => *pending_writes,
            _ => 0,
        }
    }

    /// Returns the height of the child on the given side, if any. If there is
    /// no child, returns 0.
    #[inline]
    pub const fn child_height(&self, left: bool) -> u8 {
        match self.link(left) {
            Some(child) => child.height(),
            _ => 0,
        }
    }

    #[inline]
    pub const fn child_heights(&self) -> (u8, u8) {
        (self.child_height(true), self.child_height(false))
    }

    /// Returns the height of the tree (the number of levels). For example, a
    /// single node has height 1, a node with a single descendant has height 2,
    /// etc.
    #[inline]
    pub fn height(&self) -> u8 {
        1 + max(self.child_height(true), self.child_height(false))
    }

    /// Returns the balance factor of the root node. This is the difference
    /// between the height of the right child (if any) and the height of the
    /// left child (if any). For example, a balance factor of 2 means the right
    /// subtree is 2 levels taller than the left subtree.
    #[inline]
    pub const fn balance_factor(&self) -> i8 {
        let left_height = self.child_height(true) as i8;
        let right_height = self.child_height(false) as i8;
        right_height - left_height
    }

    /// Attaches the child (if any) to the root node on the given side. Creates
    /// a `Link` of variant `Link::Modified` which contains the child.
    ///
    /// Panics if there is already a child on the given side.
    #[inline]
    pub fn attach(mut self, left: bool, maybe_child: Option<Self>) -> Self {
        debug_assert_ne!(
            Some(self.key()),
            maybe_child.as_ref().map(|c| c.key()),
            "Tried to attach tree with same key"
        );

        // let parent = std::str::from_utf8(self.key());
        // if maybe_child.is_some(){
        //     let child = std::str::from_utf8(maybe_child.as_ref().unwrap().key());
        //     println!("attaching {} to {}", child.unwrap(), parent.unwrap());
        // } else {
        //     println!("attaching nothing to {}", parent.unwrap());
        // }

        let slot = self.slot_mut(left);

        if slot.is_some() {
            panic!(
                "Tried to attach to {} tree slot, but it is already Some",
                side_to_str(left)
            );
        }
        *slot = Link::maybe_from_modified_tree(maybe_child);

        // dbg!(&self);
        self
    }

    /// Detaches the child on the given side (if any) from the root node, and
    /// returns `(root_node, maybe_child)`.
    ///
    /// One will usually want to reattach (see `attach`) a child on the same
    /// side after applying some operation to the detached child.
    #[inline]
    pub fn detach(mut self, left: bool) -> (Self, Option<Self>) {
        let maybe_child = match self.slot_mut(left).take() {
            None => None,
            Some(Link::Reference { .. }) => None,
            Some(Link::Modified { tree, .. }) => Some(tree),
            Some(Link::Uncommitted { tree, .. }) => Some(tree),
            Some(Link::Loaded { tree, .. }) => Some(tree),
        };
        // println!("detaching {}",
        // std::str::from_utf8(maybe_child.as_ref().unwrap().key()).unwrap());

        (self, maybe_child)
    }

    /// Detaches the child on the given side from the root node, and
    /// returns `(root_node, child)`.
    ///
    /// Panics if there is no child on the given side.
    ///
    /// One will usually want to reattach (see `attach`) a child on the same
    /// side after applying some operation to the detached child.
    #[inline]
    pub fn detach_expect(self, left: bool) -> (Self, Self) {
        let (parent, maybe_child) = self.detach(left);

        if let Some(child) = maybe_child {
            (parent, child)
        } else {
            panic!(
                "Expected tree to have {} child, but got None",
                side_to_str(left)
            );
        }
    }

    /// Detaches the child on the given side and passes it into `f`, which must
    /// return a new child (either the same child, a new child to take its
    /// place, or `None` to explicitly keep the slot empty).
    ///
    /// This is the same as `detach`, but with the function interface to enforce
    /// at compile-time that an explicit final child value is returned. This is
    /// less error prone that detaching with `detach` and reattaching with
    /// `attach`.
    #[inline]
    pub fn walk<F>(self, left: bool, f: F) -> Self
    where
        F: FnOnce(Option<Self>) -> Option<Self>,
    {
        let (tree, maybe_child) = self.detach(left);
        tree.attach(left, f(maybe_child))
    }

    /// Like `walk`, but panics if there is no child on the given side.
    #[inline]
    pub fn walk_expect<F>(self, left: bool, f: F) -> Self
    where
        F: FnOnce(Self) -> Option<Self>,
    {
        let (tree, child) = self.detach_expect(left);
        tree.attach(left, f(child))
    }

    /// Returns a mutable reference to the child slot for the given side.
    #[inline]
    pub(crate) fn slot_mut(&mut self, left: bool) -> &mut Option<Link> {
        if left {
            &mut self.inner.left
        } else {
            &mut self.inner.right
        }
    }

    /// Replaces the root node's value with the given value and returns the
    /// modified `Tree`.
    #[inline]
    pub fn put_value(mut self, value: Vec<u8>) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        self.inner.kv = self
            .inner
            .kv
            .put_value_then_update(value)
            .unwrap_add_cost(&mut cost);
        self.wrap_with_cost(cost)
    }

    /// Replaces the root node's value with the given value and value hash
    /// and returns the modified `Tree`.
    #[inline]
    pub fn put_value_and_reference_value_hash(
        mut self,
        value: Vec<u8>,
        value_hash: CryptoHash,
    ) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        self.inner.kv = self
            .inner
            .kv
            .put_value_and_reference_value_hash_then_update(value, value_hash)
            .unwrap_add_cost(&mut cost);
        self.wrap_with_cost(cost)
    }

    /// Replaces the root node's value with the given value and value hash
    /// and returns the modified `Tree`.
    #[inline]
    pub fn put_value_with_reference_value_hash_and_value_cost(
        mut self,
        value: Vec<u8>,
        value_hash: CryptoHash,
        value_cost: u32,
    ) -> CostContext<Self> {
        let mut cost = OperationCost::default();
        self.inner.kv = self
            .inner
            .kv
            .put_value_with_reference_value_hash_and_value_cost_then_update(
                value, value_hash, value_cost,
            )
            .unwrap_add_cost(&mut cost);
        self.wrap_with_cost(cost)
    }

    // TODO: add compute_hashes method

    /// Called to finalize modifications to a tree, recompute its hashes, and
    /// write the updated nodes to a backing store.
    ///
    /// Traverses through the tree, computing hashes for all modified links and
    /// replacing them with `Link::Loaded` variants, writes out all changes to
    /// the given `Commit` object's `write` method, and calls the its `prune`
    /// method to test whether or not to keep or prune nodes from memory.
    pub fn commit<C: Commit>(
        &mut self,
        c: &mut C,
        old_tree_cost: &impl Fn(&Vec<u8>, &Vec<u8>) -> Result<u32>,
        update_tree_value_based_on_costs: &mut impl FnMut(
            &StorageCost,
            &Vec<u8>,
            &mut Vec<u8>,
        ) -> Result<(bool, Option<u32>)>,
        section_removal_bytes: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        )
            -> Result<(StorageRemovedBytes, StorageRemovedBytes)>,
    ) -> CostContext<Result<()>> {
        // TODO: make this method less ugly
        // TODO: call write in-order for better performance in writing batch to db?

        // println!("about to commit {}", std::str::from_utf8(self.key()).unwrap());
        let mut cost = OperationCost::default();

        if let Some(Link::Modified { .. }) = self.inner.left {
            // println!("left is modified");
            if let Some(Link::Modified {
                mut tree,
                child_heights,
                ..
            }) = self.inner.left.take()
            {
                // println!("key is {}", std::str::from_utf8(tree.key()).unwrap());
                cost_return_on_error!(
                    &mut cost,
                    tree.commit(
                        c,
                        old_tree_cost,
                        update_tree_value_based_on_costs,
                        section_removal_bytes
                    )
                );
                self.inner.left = Some(Link::Loaded {
                    hash: tree.hash().unwrap_add_cost(&mut cost),
                    tree,
                    child_heights,
                });
            } else {
                unreachable!()
            }
        }

        if let Some(Link::Modified { .. }) = self.inner.right {
            // println!("right is modified");
            if let Some(Link::Modified {
                mut tree,
                child_heights,
                ..
            }) = self.inner.right.take()
            {
                // println!("key is {}", std::str::from_utf8(tree.key()).unwrap());
                cost_return_on_error!(
                    &mut cost,
                    tree.commit(
                        c,
                        old_tree_cost,
                        update_tree_value_based_on_costs,
                        section_removal_bytes
                    )
                );
                self.inner.right = Some(Link::Loaded {
                    hash: tree.hash().unwrap_add_cost(&mut cost),
                    tree,
                    child_heights,
                });
            } else {
                unreachable!()
            }
        }

        cost_return_on_error_no_add!(
            &cost,
            c.write(
                self,
                old_tree_cost,
                update_tree_value_based_on_costs,
                section_removal_bytes
            )
        );

        // println!("done committing {}", std::str::from_utf8(self.key()).unwrap());

        let (prune_left, prune_right) = c.prune(self);
        if prune_left {
            self.inner.left = self.inner.left.take().map(|link| link.into_reference());
        }
        if prune_right {
            self.inner.right = self.inner.right.take().map(|link| link.into_reference());
        }

        Ok(()).wrap_with_cost(cost)
    }

    /// Fetches the child on the given side using the given data source, and
    /// places it in the child slot (upgrading the link from `Link::Reference`
    /// to `Link::Loaded`).
    pub fn load<S: Fetch>(&mut self, left: bool, source: &S) -> CostContext<Result<()>> {
        // TODO: return Err instead of panic?
        let link = self.link(left).expect("Expected link");
        let (child_heights, hash) = match link {
            Link::Reference {
                child_heights,
                hash,
                ..
            } => (child_heights, hash),
            _ => panic!("Expected Some(Link::Reference)"),
        };

        let mut cost = OperationCost::default();
        let tree = cost_return_on_error!(&mut cost, source.fetch(link));
        debug_assert_eq!(tree.key(), link.key());
        *self.slot_mut(left) = Some(Link::Loaded {
            tree,
            hash: *hash,
            child_heights: *child_heights,
        });
        Ok(()).wrap_with_cost(cost)
    }
}

pub const fn side_to_str(left: bool) -> &'static str {
    if left {
        "left"
    } else {
        "right"
    }
}

#[cfg(test)]
mod test {
    use costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;

    use super::{commit::NoopCommit, hash::NULL_HASH, Tree};

    #[test]
    fn build_tree() {
        let tree = Tree::new(vec![1], vec![101]).unwrap();
        assert_eq!(tree.key(), &[1]);
        assert_eq!(tree.value_as_slice(), &[101]);
        assert!(tree.child(true).is_none());
        assert!(tree.child(false).is_none());

        let tree = tree.attach(true, None);
        assert!(tree.child(true).is_none());
        assert!(tree.child(false).is_none());

        let tree = tree.attach(true, Some(Tree::new(vec![2], vec![102]).unwrap()));
        assert_eq!(tree.key(), &[1]);
        assert_eq!(tree.child(true).unwrap().key(), &[2]);
        assert!(tree.child(false).is_none());

        let tree = Tree::new(vec![3], vec![103])
            .unwrap()
            .attach(false, Some(tree));
        assert_eq!(tree.key(), &[3]);
        assert_eq!(tree.child(false).unwrap().key(), &[1]);
        assert!(tree.child(true).is_none());
    }

    #[should_panic]
    #[test]
    fn attach_existing() {
        Tree::new(vec![0], vec![1])
            .unwrap()
            .attach(true, Some(Tree::new(vec![2], vec![3]).unwrap()))
            .attach(true, Some(Tree::new(vec![4], vec![5]).unwrap()));
    }

    #[test]
    fn modify() {
        let tree = Tree::new(vec![0], vec![1])
            .unwrap()
            .attach(true, Some(Tree::new(vec![2], vec![3]).unwrap()))
            .attach(false, Some(Tree::new(vec![4], vec![5]).unwrap()));

        let tree = tree.walk(true, |left_opt| {
            assert_eq!(left_opt.as_ref().unwrap().key(), &[2]);
            None
        });
        assert!(tree.child(true).is_none());
        assert!(tree.child(false).is_some());

        let tree = tree.walk(true, |left_opt| {
            assert!(left_opt.is_none());
            Some(Tree::new(vec![2], vec![3]).unwrap())
        });
        assert_eq!(tree.link(true).unwrap().key(), &[2]);

        let tree = tree.walk_expect(false, |right| {
            assert_eq!(right.key(), &[4]);
            None
        });
        assert!(tree.child(true).is_some());
        assert!(tree.child(false).is_none());
    }

    #[test]
    fn child_and_link() {
        let mut tree = Tree::new(vec![0], vec![1])
            .unwrap()
            .attach(true, Some(Tree::new(vec![2], vec![3]).unwrap()));
        assert!(tree.link(true).expect("expected link").is_modified());
        assert!(tree.child(true).is_some());
        assert!(tree.link(false).is_none());
        assert!(tree.child(false).is_none());

        tree.commit(
            &mut NoopCommit {},
            &|_, _| Ok(0),
            &mut |_, _, _| Ok((false, None)),
            &mut |_, _, _| Ok((NoStorageRemoval, NoStorageRemoval)),
        )
        .unwrap()
        .expect("commit failed");
        assert!(tree.link(true).expect("expected link").is_stored());
        assert!(tree.child(true).is_some());

        // tree.link(true).prune(true);
        // assert!(tree.link(true).expect("expected link").is_pruned());
        // assert!(tree.child(true).is_none());

        let tree = tree.walk(true, |_| None);
        assert!(tree.link(true).is_none());
        assert!(tree.child(true).is_none());
    }

    #[test]
    fn child_hash() {
        let mut tree = Tree::new(vec![0], vec![1])
            .unwrap()
            .attach(true, Some(Tree::new(vec![2], vec![3]).unwrap()));
        tree.commit(
            &mut NoopCommit {},
            &|_, _| Ok(0),
            &mut |_, _, _| Ok((false, None)),
            &mut |_, _, _| Ok((NoStorageRemoval, NoStorageRemoval)),
        )
        .unwrap()
        .expect("commit failed");
        assert_eq!(
            tree.child_hash(true),
            &[
                132, 211, 39, 192, 19, 164, 57, 106, 128, 9, 35, 145, 86, 12, 57, 192, 239, 69,
                113, 148, 33, 220, 206, 207, 237, 199, 214, 241, 97, 144, 224, 185
            ]
        );
        assert_eq!(tree.child_hash(false), &NULL_HASH);
    }

    #[test]
    fn hash() {
        let tree = Tree::new(vec![0], vec![1]).unwrap();
        assert_eq!(
            tree.hash().unwrap(),
            [
                10, 108, 153, 163, 54, 173, 62, 155, 228, 204, 102, 172, 158, 203, 197, 126, 230,
                234, 97, 110, 227, 208, 64, 21, 65, 8, 82, 2, 241, 122, 66, 207
            ]
        );
    }

    #[test]
    fn child_pending_writes() {
        let tree = Tree::new(vec![0], vec![1]).unwrap();
        assert_eq!(tree.child_pending_writes(true), 0);
        assert_eq!(tree.child_pending_writes(false), 0);

        let tree = tree.attach(true, Some(Tree::new(vec![2], vec![3]).unwrap()));
        assert_eq!(tree.child_pending_writes(true), 1);
        assert_eq!(tree.child_pending_writes(false), 0);
    }

    #[test]
    fn height_and_balance() {
        let tree = Tree::new(vec![0], vec![1]).unwrap();
        assert_eq!(tree.height(), 1);
        assert_eq!(tree.child_height(true), 0);
        assert_eq!(tree.child_height(false), 0);
        assert_eq!(tree.balance_factor(), 0);

        let tree = tree.attach(true, Some(Tree::new(vec![2], vec![3]).unwrap()));
        assert_eq!(tree.height(), 2);
        assert_eq!(tree.child_height(true), 1);
        assert_eq!(tree.child_height(false), 0);
        assert_eq!(tree.balance_factor(), -1);

        let (tree, maybe_child) = tree.detach(true);
        let tree = tree.attach(false, maybe_child);
        assert_eq!(tree.height(), 2);
        assert_eq!(tree.child_height(true), 0);
        assert_eq!(tree.child_height(false), 1);
        assert_eq!(tree.balance_factor(), 1);
    }

    #[test]
    fn commit() {
        let mut tree = Tree::new(vec![0], vec![1])
            .unwrap()
            .attach(false, Some(Tree::new(vec![2], vec![3]).unwrap()));
        tree.commit(
            &mut NoopCommit {},
            &|_, _| Ok(0),
            &mut |_, _, _| Ok((false, None)),
            &mut |_, _, _| Ok((NoStorageRemoval, NoStorageRemoval)),
        )
        .unwrap()
        .expect("commit failed");

        assert!(tree.link(false).expect("expected link").is_stored());
    }
}
