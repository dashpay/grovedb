//! A `ShardStore` implementation backed by a generic key-value store.
//!
//! This module provides [`KvShardStore`], which adapts any [`KvStore`]
//! implementation into a `ShardStore` suitable for use with `ShardTree`. Data
//! is persisted through simple key-value operations with a prefix-based key
//! scheme.
//!
//! # Key Scheme
//!
//! All keys use single-byte prefixes to avoid collisions between different data
//! types:
//! - `S` + 8-byte BE shard_index -> serialized `LocatedPrunableTree`
//! - `C` -> serialized `PrunableTree` (cap)
//! - `K` + 8-byte BE checkpoint_id -> serialized `Checkpoint`

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

use incrementalmerkletree::{Address, Level};
use shardtree::{
    store::{Checkpoint, ShardStore},
    LocatedPrunableTree, PrunableTree, Tree,
};

use crate::{
    serialization::{self, HashSer},
    SHARD_HEIGHT,
};

/// A simple in-memory implementation of [`KvStore`] backed by a `BTreeMap`.
///
/// Used as a buffer for loading/saving commitment tree data from/to GroveDB
/// storage contexts.
#[derive(Debug, Default, Clone)]
pub struct MemKvStore {
    data: BTreeMap<Vec<u8>, Vec<u8>>,
}

/// Error type for [`MemKvStore`] operations.
#[derive(Debug, thiserror::Error)]
pub enum MemKvError {
    /// I/O error from serialization/deserialization.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

impl MemKvStore {
    /// Create a new empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a store pre-populated with entries.
    pub fn from_entries(entries: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>) -> Self {
        Self {
            data: entries.into_iter().collect(),
        }
    }

    /// Get a reference to the underlying data.
    pub fn data(&self) -> &BTreeMap<Vec<u8>, Vec<u8>> {
        &self.data
    }

    /// Serialize the store contents to bytes.
    ///
    /// Format: `[num_entries: u32][key_len: u32][key][value_len:
    /// u32][value]...`
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        let count = self.data.len() as u32;
        buf.extend_from_slice(&count.to_le_bytes());
        for (k, v) in &self.data {
            let key_len = k.len() as u32;
            buf.extend_from_slice(&key_len.to_le_bytes());
            buf.extend_from_slice(k);
            let val_len = v.len() as u32;
            buf.extend_from_slice(&val_len.to_le_bytes());
            buf.extend_from_slice(v);
        }
        buf
    }

    /// Deserialize a store from bytes.
    pub fn deserialize(bytes: &[u8]) -> Result<Self, io::Error> {
        let mut pos = 0;
        if bytes.len() < 4 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "too short"));
        }
        let count = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let mut data = BTreeMap::new();
        for _ in 0..count {
            if pos + 4 > bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated key len",
                ));
            }
            let key_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            if pos + key_len > bytes.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated key"));
            }
            let key = bytes[pos..pos + key_len].to_vec();
            pos += key_len;
            if pos + 4 > bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated value len",
                ));
            }
            let val_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            if pos + val_len > bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated value",
                ));
            }
            let value = bytes[pos..pos + val_len].to_vec();
            pos += val_len;
            data.insert(key, value);
        }
        Ok(Self { data })
    }
}

impl KvStore for MemKvStore {
    type Error = MemKvError;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.data.get(key).cloned())
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        self.data.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), Self::Error> {
        self.data.remove(key);
        Ok(())
    }

    fn prefix_iter(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, Self::Error> {
        let result: Vec<_> = self
            .data
            .range::<Vec<u8>, _>(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Ok(result)
    }

    fn delete_range_from(&mut self, prefix: &[u8], start_key: &[u8]) -> Result<(), Self::Error> {
        let keys_to_delete: Vec<Vec<u8>> = self
            .data
            .range::<Vec<u8>, _>(start_key.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys_to_delete {
            self.data.remove(&key);
        }
        Ok(())
    }
}

/// Key prefix for shard entries.
const PREFIX_SHARD: u8 = b'S';
/// Key for the cap entry.
const KEY_CAP: &[u8] = b"C";
/// Key prefix for checkpoint entries.
const PREFIX_CHECKPOINT: u8 = b'K';

/// Simple key-value storage trait for commitment tree persistence.
///
/// Implementations must provide ordered iteration for prefix scans and
/// range deletion. Keys and values are arbitrary byte slices.
pub trait KvStore {
    /// The error type for operations on this store.
    type Error: std::error::Error + From<io::Error>;

    /// Get the value associated with the given key, or `None` if not found.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Insert or replace the value at the given key.
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), Self::Error>;

    /// Delete the value at the given key. No-op if key does not exist.
    fn delete(&mut self, key: &[u8]) -> Result<(), Self::Error>;

    /// Get all key-value pairs with keys starting with the given prefix,
    /// ordered by key.
    #[allow(clippy::type_complexity)]
    fn prefix_iter(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, Self::Error>;

    /// Delete all keys that start with the given prefix and are >= start_key in
    /// sort order.
    fn delete_range_from(&mut self, prefix: &[u8], start_key: &[u8]) -> Result<(), Self::Error>;
}

/// Encode a shard index into a 9-byte key: prefix `S` + 8-byte BE index.
fn shard_key(index: u64) -> [u8; 9] {
    let mut key = [0u8; 9];
    key[0] = PREFIX_SHARD;
    key[1..9].copy_from_slice(&index.to_be_bytes());
    key
}

/// Decode a shard index from a 9-byte key.
fn decode_shard_index(key: &[u8]) -> Option<u64> {
    if key.len() == 9 && key[0] == PREFIX_SHARD {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&key[1..9]);
        Some(u64::from_be_bytes(bytes))
    } else {
        None
    }
}

/// Encode a checkpoint id into a 9-byte key: prefix `K` + 8-byte BE id.
fn checkpoint_key(id: u64) -> [u8; 9] {
    let mut key = [0u8; 9];
    key[0] = PREFIX_CHECKPOINT;
    key[1..9].copy_from_slice(&id.to_be_bytes());
    key
}

/// Decode a checkpoint id from a 9-byte key.
fn decode_checkpoint_id(key: &[u8]) -> Option<u64> {
    if key.len() == 9 && key[0] == PREFIX_CHECKPOINT {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&key[1..9]);
        Some(u64::from_be_bytes(bytes))
    } else {
        None
    }
}

/// A `ShardStore` implementation backed by a generic key-value store.
///
/// The hash type `H` must implement `HashSer` for serialization and the
/// standard `Clone` and `PartialEq` traits required by `PrunableTree`
/// operations.
///
/// The checkpoint ID type is fixed to `u64` for simplicity and efficient
/// binary encoding.
pub struct KvShardStore<S: KvStore, H: HashSer + Clone> {
    store: S,
    _phantom: std::marker::PhantomData<H>,
}

impl<S: KvStore, H: HashSer + Clone> KvShardStore<S, H> {
    /// Create a new `KvShardStore` wrapping the given key-value store.
    pub fn new(store: S) -> Self {
        Self {
            store,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Access the underlying key-value store.
    pub fn inner(&self) -> &S {
        &self.store
    }

    /// Access the underlying key-value store mutably.
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Consume this wrapper and return the underlying store.
    pub fn into_inner(self) -> S {
        self.store
    }

    /// Helper: serialize a `LocatedPrunableTree<H>` to bytes.
    fn serialize_located_tree(tree: &LocatedPrunableTree<H>) -> Result<Vec<u8>, io::Error> {
        let mut buf = Vec::new();
        serialization::write_located_prunable_tree(tree, &mut buf)?;
        Ok(buf)
    }

    /// Helper: deserialize a `LocatedPrunableTree<H>` from bytes.
    fn deserialize_located_tree(bytes: &[u8]) -> Result<LocatedPrunableTree<H>, io::Error> {
        serialization::read_located_prunable_tree(&mut &bytes[..])
    }

    /// Helper: serialize a `PrunableTree<H>` to bytes.
    fn serialize_tree(tree: &PrunableTree<H>) -> Result<Vec<u8>, io::Error> {
        let mut buf = Vec::new();
        serialization::write_prunable_tree(tree, &mut buf)?;
        Ok(buf)
    }

    /// Helper: deserialize a `PrunableTree<H>` from bytes.
    fn deserialize_tree(bytes: &[u8]) -> Result<PrunableTree<H>, io::Error> {
        serialization::read_prunable_tree(&mut &bytes[..])
    }

    /// Helper: serialize a `Checkpoint` to bytes.
    fn serialize_checkpoint(cp: &Checkpoint) -> Result<Vec<u8>, io::Error> {
        let mut buf = Vec::new();
        serialization::write_checkpoint(cp, &mut buf)?;
        Ok(buf)
    }

    /// Helper: deserialize a `Checkpoint` from bytes.
    fn deserialize_checkpoint(bytes: &[u8]) -> Result<Checkpoint, io::Error> {
        serialization::read_checkpoint(&mut &bytes[..])
    }
}

impl<S: KvStore, H: HashSer + Clone + PartialEq> ShardStore for KvShardStore<S, H> {
    type CheckpointId = u64;
    type Error = S::Error;
    type H = H;

    fn get_shard(
        &self,
        shard_root: Address,
    ) -> Result<Option<LocatedPrunableTree<Self::H>>, Self::Error> {
        let key = shard_key(shard_root.index());
        match self.store.get(&key)? {
            Some(bytes) => {
                let tree = Self::deserialize_located_tree(&bytes)?;
                Ok(Some(tree))
            }
            None => Ok(None),
        }
    }

    fn last_shard(&self) -> Result<Option<LocatedPrunableTree<Self::H>>, Self::Error> {
        let entries = self.store.prefix_iter(&[PREFIX_SHARD])?;
        match entries.last() {
            Some((_, bytes)) => {
                let tree = Self::deserialize_located_tree(bytes)?;
                Ok(Some(tree))
            }
            None => Ok(None),
        }
    }

    fn put_shard(&mut self, subtree: LocatedPrunableTree<Self::H>) -> Result<(), Self::Error> {
        let index = subtree.root_addr().index();
        let key = shard_key(index);
        let bytes = Self::serialize_located_tree(&subtree)?;
        self.store.put(&key, &bytes)
    }

    fn get_shard_roots(&self) -> Result<Vec<Address>, Self::Error> {
        let entries = self.store.prefix_iter(&[PREFIX_SHARD])?;
        let mut roots = Vec::with_capacity(entries.len());
        for (key, _) in &entries {
            if let Some(index) = decode_shard_index(key) {
                roots.push(Address::from_parts(Level::from(SHARD_HEIGHT), index));
            }
        }
        Ok(roots)
    }

    fn truncate_shards(&mut self, shard_index: u64) -> Result<(), Self::Error> {
        let start_key = shard_key(shard_index);
        self.store.delete_range_from(&[PREFIX_SHARD], &start_key)
    }

    fn get_cap(&self) -> Result<PrunableTree<Self::H>, Self::Error> {
        match self.store.get(KEY_CAP)? {
            Some(bytes) => {
                let tree = Self::deserialize_tree(&bytes)?;
                Ok(tree)
            }
            None => Ok(Tree::empty()),
        }
    }

    fn put_cap(&mut self, cap: PrunableTree<Self::H>) -> Result<(), Self::Error> {
        let bytes = Self::serialize_tree(&cap)?;
        self.store.put(KEY_CAP, &bytes)
    }

    fn min_checkpoint_id(&self) -> Result<Option<Self::CheckpointId>, Self::Error> {
        let entries = self.store.prefix_iter(&[PREFIX_CHECKPOINT])?;
        match entries.first() {
            Some((key, _)) => Ok(decode_checkpoint_id(key)),
            None => Ok(None),
        }
    }

    fn max_checkpoint_id(&self) -> Result<Option<Self::CheckpointId>, Self::Error> {
        let entries = self.store.prefix_iter(&[PREFIX_CHECKPOINT])?;
        match entries.last() {
            Some((key, _)) => Ok(decode_checkpoint_id(key)),
            None => Ok(None),
        }
    }

    fn add_checkpoint(
        &mut self,
        checkpoint_id: Self::CheckpointId,
        checkpoint: Checkpoint,
    ) -> Result<(), Self::Error> {
        let key = checkpoint_key(checkpoint_id);
        let bytes = Self::serialize_checkpoint(&checkpoint)?;
        self.store.put(&key, &bytes)
    }

    fn checkpoint_count(&self) -> Result<usize, Self::Error> {
        let entries = self.store.prefix_iter(&[PREFIX_CHECKPOINT])?;
        Ok(entries.len())
    }

    fn get_checkpoint_at_depth(
        &self,
        checkpoint_depth: usize,
    ) -> Result<Option<(Self::CheckpointId, Checkpoint)>, Self::Error> {
        let entries = self.store.prefix_iter(&[PREFIX_CHECKPOINT])?;
        // Depth 0 is the most recent (last in sorted order), depth 1 is one before,
        // etc.
        let target_idx = entries.len().checked_sub(checkpoint_depth + 1);
        match target_idx {
            Some(idx) => {
                let (key, val) = &entries[idx];
                let id = decode_checkpoint_id(key).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid checkpoint key")
                })?;
                let cp = Self::deserialize_checkpoint(val)?;
                Ok(Some((id, cp)))
            }
            None => Ok(None),
        }
    }

    fn get_checkpoint(
        &self,
        checkpoint_id: &Self::CheckpointId,
    ) -> Result<Option<Checkpoint>, Self::Error> {
        let key = checkpoint_key(*checkpoint_id);
        match self.store.get(&key)? {
            Some(bytes) => {
                let cp = Self::deserialize_checkpoint(&bytes)?;
                Ok(Some(cp))
            }
            None => Ok(None),
        }
    }

    fn with_checkpoints<F>(&mut self, limit: usize, mut callback: F) -> Result<(), Self::Error>
    where
        F: FnMut(&Self::CheckpointId, &Checkpoint) -> Result<(), Self::Error>,
    {
        let entries = self.store.prefix_iter(&[PREFIX_CHECKPOINT])?;
        for (key, val) in entries.iter().take(limit) {
            let id = decode_checkpoint_id(key).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid checkpoint key")
            })?;
            let cp = Self::deserialize_checkpoint(val)?;
            callback(&id, &cp)?;
        }
        Ok(())
    }

    fn for_each_checkpoint<F>(&self, limit: usize, mut callback: F) -> Result<(), Self::Error>
    where
        F: FnMut(&Self::CheckpointId, &Checkpoint) -> Result<(), Self::Error>,
    {
        let entries = self.store.prefix_iter(&[PREFIX_CHECKPOINT])?;
        for (key, val) in entries.iter().take(limit) {
            let id = decode_checkpoint_id(key).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid checkpoint key")
            })?;
            let cp = Self::deserialize_checkpoint(val)?;
            callback(&id, &cp)?;
        }
        Ok(())
    }

    fn update_checkpoint_with<F>(
        &mut self,
        checkpoint_id: &Self::CheckpointId,
        update: F,
    ) -> Result<bool, Self::Error>
    where
        F: Fn(&mut Checkpoint) -> Result<(), Self::Error>,
    {
        let key = checkpoint_key(*checkpoint_id);
        match self.store.get(&key)? {
            Some(bytes) => {
                let mut cp = Self::deserialize_checkpoint(&bytes)?;
                update(&mut cp)?;
                let new_bytes = Self::serialize_checkpoint(&cp)?;
                self.store.put(&key, &new_bytes)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn remove_checkpoint(&mut self, checkpoint_id: &Self::CheckpointId) -> Result<(), Self::Error> {
        let key = checkpoint_key(*checkpoint_id);
        self.store.delete(&key)
    }

    fn truncate_checkpoints_retaining(
        &mut self,
        checkpoint_id: &Self::CheckpointId,
    ) -> Result<(), Self::Error> {
        // Delete all checkpoints with id > checkpoint_id
        let next_id = checkpoint_id.checked_add(1);
        if let Some(next) = next_id {
            let start_key = checkpoint_key(next);
            self.store
                .delete_range_from(&[PREFIX_CHECKPOINT], &start_key)?;
        }

        // Update the retained checkpoint to clear marks_removed
        let key = checkpoint_key(*checkpoint_id);
        if let Some(bytes) = self.store.get(&key)? {
            let cp = Self::deserialize_checkpoint(&bytes)?;
            // Reconstruct with empty marks_removed
            let updated = Checkpoint::from_parts(cp.tree_state(), BTreeSet::new());
            let new_bytes = Self::serialize_checkpoint(&updated)?;
            self.store.put(&key, &new_bytes)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use incrementalmerkletree::{Hashable, Position, Retention};
    use orchard::tree::MerkleHashOrchard;
    use shardtree::{
        store::{Checkpoint, TreeState},
        LocatedTree, RetentionFlags,
    };

    use super::*;

    /// Create a deterministic test leaf from an index.
    fn test_leaf(index: u64) -> MerkleHashOrchard {
        let empty = MerkleHashOrchard::empty_leaf();
        MerkleHashOrchard::combine(Level::from((index % 31) as u8 + 1), &empty, &empty)
    }

    /// Helper to create a KvShardStore with the in-memory backend.
    fn new_kv_store() -> KvShardStore<MemKvStore, MerkleHashOrchard> {
        KvShardStore::new(MemKvStore::default())
    }

    #[test]
    fn test_empty_store_returns_none() {
        let store = new_kv_store();
        let addr = Address::from_parts(Level::from(SHARD_HEIGHT), 0);

        assert!(store.get_shard(addr).unwrap().is_none());
        assert!(store.last_shard().unwrap().is_none());
        assert!(store.get_shard_roots().unwrap().is_empty());
        assert_eq!(store.checkpoint_count().unwrap(), 0);
        assert!(store.min_checkpoint_id().unwrap().is_none());
        assert!(store.max_checkpoint_id().unwrap().is_none());
        assert!(store.get_checkpoint_at_depth(0).unwrap().is_none());
        assert!(store.get_checkpoint(&0).unwrap().is_none());

        // Cap should return empty tree
        let cap = store.get_cap().unwrap();
        assert!(cap.is_empty());
    }

    #[test]
    fn test_put_and_get_shard() {
        let mut store = new_kv_store();
        let h1 = test_leaf(1);
        let addr = Address::from_parts(Level::from(SHARD_HEIGHT), 3);
        let tree = LocatedTree::from_parts(addr, Tree::leaf((h1, RetentionFlags::MARKED))).unwrap();

        store.put_shard(tree.clone()).unwrap();

        let retrieved = store.get_shard(addr).unwrap().unwrap();
        assert_eq!(retrieved, tree);
    }

    #[test]
    fn test_last_shard() {
        let mut store = new_kv_store();
        let h1 = test_leaf(1);
        let h2 = test_leaf(2);

        let addr0 = Address::from_parts(Level::from(SHARD_HEIGHT), 0);
        let tree0 =
            LocatedTree::from_parts(addr0, Tree::leaf((h1, RetentionFlags::EPHEMERAL))).unwrap();

        let addr5 = Address::from_parts(Level::from(SHARD_HEIGHT), 5);
        let tree5 =
            LocatedTree::from_parts(addr5, Tree::leaf((h2, RetentionFlags::MARKED))).unwrap();

        store.put_shard(tree0).unwrap();
        store.put_shard(tree5.clone()).unwrap();

        let last = store.last_shard().unwrap().unwrap();
        assert_eq!(last, tree5);
    }

    #[test]
    fn test_shard_roots() {
        let mut store = new_kv_store();
        let h = test_leaf(0);

        for idx in [0u64, 2, 7] {
            let addr = Address::from_parts(Level::from(SHARD_HEIGHT), idx);
            let tree =
                LocatedTree::from_parts(addr, Tree::leaf((h, RetentionFlags::EPHEMERAL))).unwrap();
            store.put_shard(tree).unwrap();
        }

        let roots = store.get_shard_roots().unwrap();
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[0].index(), 0);
        assert_eq!(roots[1].index(), 2);
        assert_eq!(roots[2].index(), 7);
    }

    #[test]
    fn test_truncate_shards() {
        let mut store = new_kv_store();
        let h = test_leaf(0);

        for idx in 0..5u64 {
            let addr = Address::from_parts(Level::from(SHARD_HEIGHT), idx);
            let tree =
                LocatedTree::from_parts(addr, Tree::leaf((h, RetentionFlags::EPHEMERAL))).unwrap();
            store.put_shard(tree).unwrap();
        }

        store.truncate_shards(3).unwrap();

        let roots = store.get_shard_roots().unwrap();
        assert_eq!(roots.len(), 3);
        assert_eq!(roots.last().unwrap().index(), 2);
    }

    #[test]
    fn test_cap_roundtrip() {
        let mut store = new_kv_store();
        let h1 = test_leaf(10);
        let h2 = test_leaf(20);

        let cap: PrunableTree<MerkleHashOrchard> = Tree::parent(
            None,
            Tree::leaf((h1, RetentionFlags::EPHEMERAL)),
            Tree::leaf((h2, RetentionFlags::EPHEMERAL)),
        );

        store.put_cap(cap.clone()).unwrap();
        let retrieved = store.get_cap().unwrap();
        assert_eq!(cap, retrieved);
    }

    #[test]
    fn test_checkpoint_operations() {
        let mut store = new_kv_store();

        let cp0 = Checkpoint::tree_empty();
        let cp1 = Checkpoint::at_position(Position::from(10u64));
        let cp2 = Checkpoint::at_position(Position::from(20u64));

        store.add_checkpoint(0, cp0).unwrap();
        store.add_checkpoint(5, cp1).unwrap();
        store.add_checkpoint(10, cp2).unwrap();

        assert_eq!(store.checkpoint_count().unwrap(), 3);
        assert_eq!(store.min_checkpoint_id().unwrap(), Some(0));
        assert_eq!(store.max_checkpoint_id().unwrap(), Some(10));

        // get_checkpoint
        let retrieved = store.get_checkpoint(&5).unwrap().unwrap();
        assert_eq!(
            retrieved.tree_state(),
            TreeState::AtPosition(Position::from(10u64))
        );

        // get_checkpoint_at_depth: depth 0 = most recent = id 10
        let (id, _) = store.get_checkpoint_at_depth(0).unwrap().unwrap();
        assert_eq!(id, 10);

        // depth 1 = second most recent = id 5
        let (id, _) = store.get_checkpoint_at_depth(1).unwrap().unwrap();
        assert_eq!(id, 5);

        // depth 2 = oldest = id 0
        let (id, _) = store.get_checkpoint_at_depth(2).unwrap().unwrap();
        assert_eq!(id, 0);

        // depth 3 = not enough checkpoints
        assert!(store.get_checkpoint_at_depth(3).unwrap().is_none());
    }

    #[test]
    fn test_remove_checkpoint() {
        let mut store = new_kv_store();

        store.add_checkpoint(1, Checkpoint::tree_empty()).unwrap();
        store
            .add_checkpoint(2, Checkpoint::at_position(Position::from(5u64)))
            .unwrap();

        assert_eq!(store.checkpoint_count().unwrap(), 2);

        store.remove_checkpoint(&1).unwrap();
        assert_eq!(store.checkpoint_count().unwrap(), 1);
        assert!(store.get_checkpoint(&1).unwrap().is_none());
        assert!(store.get_checkpoint(&2).unwrap().is_some());
    }

    #[test]
    fn test_update_checkpoint_with() {
        let mut store = new_kv_store();

        store
            .add_checkpoint(1, Checkpoint::at_position(Position::from(5u64)))
            .unwrap();

        // Update should return true for existing checkpoint
        let result = store.update_checkpoint_with(&1, |_cp| Ok(())).unwrap();
        assert!(result);

        // Update should return false for non-existing checkpoint
        let result = store.update_checkpoint_with(&99, |_cp| Ok(())).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_truncate_checkpoints_retaining() {
        let mut store = new_kv_store();

        let mut marks = BTreeSet::new();
        marks.insert(Position::from(42u64));

        store.add_checkpoint(1, Checkpoint::tree_empty()).unwrap();
        store
            .add_checkpoint(
                5,
                Checkpoint::from_parts(TreeState::AtPosition(Position::from(10u64)), marks),
            )
            .unwrap();
        store
            .add_checkpoint(10, Checkpoint::at_position(Position::from(20u64)))
            .unwrap();

        // Retain checkpoint 5, which should remove checkpoint 10 and clear marks from 5
        store.truncate_checkpoints_retaining(&5).unwrap();

        assert_eq!(store.checkpoint_count().unwrap(), 2); // 1 and 5 remain
        assert!(store.get_checkpoint(&10).unwrap().is_none());
        assert!(store.get_checkpoint(&1).unwrap().is_some());

        let retained = store.get_checkpoint(&5).unwrap().unwrap();
        assert!(
            retained.marks_removed().is_empty(),
            "marks_removed should be cleared on the retained checkpoint"
        );
        assert_eq!(
            retained.tree_state(),
            TreeState::AtPosition(Position::from(10u64))
        );
    }

    #[test]
    fn test_for_each_checkpoint() {
        let mut store = new_kv_store();

        store.add_checkpoint(1, Checkpoint::tree_empty()).unwrap();
        store
            .add_checkpoint(5, Checkpoint::at_position(Position::from(10u64)))
            .unwrap();
        store
            .add_checkpoint(10, Checkpoint::at_position(Position::from(20u64)))
            .unwrap();

        let mut seen = Vec::new();
        store
            .for_each_checkpoint(2, |id, _cp| {
                seen.push(*id);
                Ok(())
            })
            .unwrap();

        assert_eq!(seen, vec![1, 5]);
    }

    #[test]
    fn test_with_checkpoints() {
        let mut store = new_kv_store();

        store.add_checkpoint(1, Checkpoint::tree_empty()).unwrap();
        store
            .add_checkpoint(5, Checkpoint::at_position(Position::from(10u64)))
            .unwrap();

        let mut seen = Vec::new();
        store
            .with_checkpoints(10, |id, _cp| {
                seen.push(*id);
                Ok(())
            })
            .unwrap();

        assert_eq!(seen, vec![1, 5]);
    }

    /// Integration test: use KvShardStore as the backing store for a full
    /// ShardTree and verify that the tree operates correctly with append,
    /// checkpoint, and witness.
    #[test]
    fn test_kv_store_with_shard_tree() {
        use crate::CommitmentTree;

        let kv_store = new_kv_store();
        let mut tree = CommitmentTree::new(kv_store, 100);

        // Append leaves
        let mut leaves = Vec::new();
        for i in 0..10u64 {
            let leaf = test_leaf(i);
            leaves.push(leaf);
            tree.append_raw(leaf, Retention::Marked).unwrap();
        }

        tree.checkpoint(0u64).unwrap();

        // Verify root is not empty
        let root = tree.root_hash().unwrap();
        let empty = MerkleHashOrchard::empty_leaf();
        let empty_root =
            MerkleHashOrchard::empty_root(Level::from(orchard::NOTE_COMMITMENT_TREE_DEPTH as u8));
        assert_ne!(root, empty_root.to_bytes());
        assert_ne!(root, empty.to_bytes());

        // Verify witnesses
        for i in 0..10u64 {
            let witness = tree
                .witness(Position::from(i))
                .expect("witness generation should not error")
                .expect("witness should exist for marked position");

            let computed_root = witness.root(leaves[i as usize]);
            let expected_root = tree.root().unwrap();
            assert_eq!(
                computed_root, expected_root,
                "witness for position {} should produce correct root",
                i
            );
        }
    }

    /// Verify that KvShardStore produces the same results as MemoryShardStore.
    #[test]
    fn test_kv_store_matches_memory_store() {
        use crate::{new_memory_store, CommitmentTree};

        let kv_store = new_kv_store();
        let mem_store = new_memory_store();

        let mut kv_tree = CommitmentTree::new(kv_store, 100);
        let mut mem_tree = CommitmentTree::new(mem_store, 100);

        for i in 0..20u64 {
            let leaf = test_leaf(i);
            kv_tree.append_raw(leaf, Retention::Marked).unwrap();
            mem_tree.append_raw(leaf, Retention::Marked).unwrap();
        }

        // Use u64 checkpoint for kv_tree and u32 for mem_tree
        kv_tree.checkpoint(0u64).unwrap();
        mem_tree.checkpoint(0u32).unwrap();

        let kv_root = kv_tree.root_hash().unwrap();
        let mem_root = mem_tree.root_hash().unwrap();
        assert_eq!(
            kv_root, mem_root,
            "KV and memory stores should produce the same root"
        );

        // Verify witnesses match
        for i in 0..20u64 {
            let kv_witness = kv_tree.witness(Position::from(i)).unwrap().unwrap();
            let mem_witness = mem_tree.witness(Position::from(i)).unwrap().unwrap();
            assert_eq!(
                kv_witness, mem_witness,
                "witnesses should match at position {}",
                i
            );
        }
    }
}
