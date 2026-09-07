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

//! Storage for GroveDB

use std::{
    cell::RefCell,
    collections::{btree_map::IntoValues, BTreeMap},
    path::Path,
};

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostContext,
    CostResult, OperationCost,
};
use grovedb_path::SubtreePath;
use grovedb_visualize::visualize_to_vec;

use crate::{worst_case_costs::WorstKeyLength, Error};
pub type SubtreePrefix = [u8; 32];

/// Top-level storage abstraction.
/// Should be able to hold a storage connection and to start transactions when
/// needed. All query operations will be exposed using [StorageContext].
///
/// # Single-Writer Constraint
///
/// Implementations assume at most one write transaction is active at a time.
/// The RocksDB-backed implementation uses `OptimisticTransactionDB`, which
/// allows multiple concurrent transactions at the storage level but detects
/// write conflicts only at commit time. Upper layers (GroveDb) build
/// in-memory Merk tree state during a transaction that cannot be cheaply
/// unwound on commit failure. Callers must therefore serialize write
/// transactions externally.
pub trait Storage<'db> {
    /// Storage transaction type
    type Transaction;

    /// Storage context type for multi-tree batch operations inside transaction
    type BatchTransactionalStorageContext: StorageContext<'db>;

    /// Storage context type for direct writes to the storage. The only use case
    /// is replication process.
    type ImmediateStorageContext: StorageContext<'db>;

    /// Starts a new transaction.
    ///
    /// Only one write transaction should be active at a time. See the
    /// [trait-level documentation](Storage) for details.
    fn start_transaction(&'db self) -> Self::Transaction;

    /// Consumes and commits a transaction.
    ///
    /// For the `OptimisticTransactionDB` backend, commit may fail with a
    /// `Busy` or `TryAgain` error if a concurrent transaction modified the
    /// same keys. On failure the transaction is consumed and the caller must
    /// discard any derived in-memory state.
    fn commit_transaction(&self, transaction: Self::Transaction) -> CostResult<(), Error>;

    /// Rolls back a transaction, reverting its pending writes.
    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Error>;

    /// Consumes and applies multi-context batch.
    fn commit_multi_context_batch(
        &self,
        batch: StorageBatch,
        transaction: Option<&'db Self::Transaction>,
    ) -> CostResult<(), Error>;

    /// Forces data to be written
    fn flush(&self) -> Result<(), Error>;

    /// Make context for a subtree on transactional data, keeping all write
    /// operations inside a `batch` if provided.
    fn get_transactional_storage_context<'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        batch: Option<&'db StorageBatch>,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::BatchTransactionalStorageContext>
    where
        B: AsRef<[u8]> + 'b;

    /// Make context for a subtree by prefix on transactional data, keeping all
    /// write operations inside a `batch` if provided.
    fn get_transactional_storage_context_by_subtree_prefix(
        &'db self,
        prefix: SubtreePrefix,
        batch: Option<&'db StorageBatch>,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::BatchTransactionalStorageContext>;

    /// Make context for a subtree on transactional data that will apply all
    /// operations straight to the storage.
    fn get_immediate_storage_context<'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::ImmediateStorageContext>
    where
        B: AsRef<[u8]> + 'b;

    /// Make context for a subtree by prefix on transactional data that will
    /// apply all operations straight to the storage.
    fn get_immediate_storage_context_by_subtree_prefix(
        &'db self,
        prefix: SubtreePrefix,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::ImmediateStorageContext>;

    /// Creates a database checkpoint in a specified path
    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error>;

    /// Return worst case cost for storage context creation.
    fn get_storage_context_cost<L: WorstKeyLength>(path: &[L]) -> OperationCost;
}

pub use grovedb_costs::ChildrenSizes;

/// Storage context.
/// Provides operations expected from a database abstracting details such as
/// whether it is a transaction or not.
pub trait StorageContext<'db> {
    /// Storage batch type
    type Batch: Batch;

    /// Storage raw iterator type (to iterate over storage without
    /// supplying a key)
    type RawIterator: RawIterator;

    /// Put `value` into data storage with `key`
    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error>;

    /// Put `value` into auxiliary data storage with `key`
    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error>;

    /// Put `value` into trees roots storage with `key`
    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error>;

    /// Put `value` into GroveDB metadata storage with `key`
    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error>;

    /// Delete entry with `key` from data storage
    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error>;

    /// Delete entry with `key` from auxiliary data storage
    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error>;

    /// Delete entry with `key` from trees roots storage
    fn delete_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error>;

    /// Delete entry with `key` from GroveDB metadata storage
    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error>;

    /// Get entry by `key` from data storage
    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error>;

    /// Get entry by `key` from auxiliary data storage
    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error>;

    /// Get entry by `key` from trees roots storage
    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error>;

    /// Get entry by `key` from GroveDB metadata storage
    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error>;

    /// Initialize a new batch
    fn new_batch(&self) -> Self::Batch;

    /// Commits changes from batch into storage
    fn commit_batch(&self, batch: Self::Batch) -> CostResult<(), Error>;

    /// Get raw iterator over storage
    fn raw_iter(&self) -> Self::RawIterator;
}

/// Database batch (not to be confused with multi-tree operations batch).
pub trait Batch {
    /// Appends to the database batch a put operation for a data record.
    fn put<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error>;

    /// Appends to the database batch a put operation for aux storage.
    fn put_aux<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error>;

    /// Appends to the database batch a put operation for subtrees roots
    /// storage.
    fn put_root<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error>;

    /// Appends to the database batch a delete operation for a data record.
    fn delete<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>);

    /// Appends to the database batch a delete operation for aux storage.
    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>);

    /// Appends to the database batch a delete operation for a record in subtree
    /// roots storage.
    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>);
}

/// Allows to iterate over database record inside of storage context.
pub trait RawIterator {
    /// Move iterator to first valid record.
    fn seek_to_first(&mut self) -> CostContext<()>;

    /// Move iterator to last valid record.
    fn seek_to_last(&mut self) -> CostContext<()>;

    /// Move iterator forward until `key` is hit.
    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()>;

    /// Move iterator backward until `key` is hit.
    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()>;

    /// Move iterator to next record.
    fn next(&mut self) -> CostContext<()>;

    /// Move iterator to previous record.
    fn prev(&mut self) -> CostContext<()>;

    /// Return value of key-value pair where raw iterator points at.
    fn value(&self) -> CostContext<Option<&[u8]>>;

    /// Return key of key-value pair where raw iterator points at.
    fn key(&self) -> CostContext<Option<&[u8]>>;

    /// Check if raw iterator points into a valid record
    fn valid(&self) -> CostContext<bool>;
}

/// Structure to hold deferred database operations in "batched" storage
/// contexts.
///
/// A batch is a keyed map of the *final* state of each key, not an ordered
/// log. Within one operation (one Merk commit) a `put` always wins over a
/// `delete` for the same key, because rebalancing legitimately deletes and
/// re-inserts a node in one commit. Callers that run SEVERAL independent
/// operations against one batch (the `MerkCache`) mark the boundary between
/// them with [`StorageBatch::next_operation`]; across such a boundary the
/// later operation's outcome wins, so a delete issued by a later operation
/// removes a key an earlier operation had put.
#[derive(Debug)]
pub struct StorageBatch {
    operations: RefCell<Operations>,
}

/// A deferred operation stamped with the operation generation it belongs to
/// (see [`StorageBatch::next_operation`]).
struct Entry {
    generation: u64,
    op: AbstractBatchOperation,
}

#[derive(Default)]
struct Operations {
    /// The current operation generation; entries recorded now carry it.
    generation: u64,
    data: BTreeMap<Vec<u8>, Entry>,
    roots: BTreeMap<Vec<u8>, Entry>,
    aux: BTreeMap<Vec<u8>, Entry>,
    meta: BTreeMap<Vec<u8>, Entry>,
}

impl Operations {
    /// Record a put: the newest value always wins.
    fn put_into(
        map: &mut BTreeMap<Vec<u8>, Entry>,
        generation: u64,
        key: Vec<u8>,
        op: AbstractBatchOperation,
    ) {
        map.insert(key, Entry { generation, op });
    }

    /// Record a delete. An entry from the SAME operation generation keeps
    /// precedence (put-wins within one Merk commit — see the documentation
    /// on [`StorageBatch::delete`]); an entry from an EARLIER generation is
    /// superseded, so a later operation's delete removes the key.
    fn delete_into(
        map: &mut BTreeMap<Vec<u8>, Entry>,
        generation: u64,
        key: Vec<u8>,
        op: AbstractBatchOperation,
    ) {
        match map.get(&key) {
            Some(existing) if existing.generation >= generation => {}
            _ => {
                map.insert(key, Entry { generation, op });
            }
        }
    }
}

impl std::fmt::Debug for Operations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut fmt = f.debug_struct("Operations");

        fmt.field(
            "data",
            &self.data.values().map(|e| &e.op).collect::<Vec<_>>(),
        );
        fmt.field("aux", &self.aux.values().map(|e| &e.op).collect::<Vec<_>>());
        fmt.field(
            "roots",
            &self.roots.values().map(|e| &e.op).collect::<Vec<_>>(),
        );
        fmt.field(
            "meta",
            &self.meta.values().map(|e| &e.op).collect::<Vec<_>>(),
        );

        fmt.finish()
    }
}

impl StorageBatch {
    /// Create empty batch.
    pub fn new() -> Self {
        StorageBatch {
            operations: RefCell::new(Operations::default()),
        }
    }

    /// Get batch length
    pub fn len(&self) -> usize {
        let operations = self.operations.borrow();
        operations.data.len()
            + operations.roots.len()
            + operations.aux.len()
            + operations.meta.len()
    }

    /// Batch emptiness predicate
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Move every pending operation out into a new batch, leaving this one
    /// empty but alive.
    ///
    /// Storage contexts hold a shared reference to the batch they write
    /// into, so a batch cannot be consumed while any of them is still open.
    /// A caller that needs to flush part-way (the partial batch apply
    /// reports the initial segment's pending cost to its caller before the
    /// continuation runs) drains the batch here and keeps writing into the
    /// same object afterwards.
    pub fn take_pending(&self) -> StorageBatch {
        StorageBatch {
            operations: RefCell::new(std::mem::take(&mut *self.operations.borrow_mut())),
        }
    }

    /// Add deferred `put` operation
    pub(crate) fn put(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) {
        let ops = &mut *self.operations.borrow_mut();
        let generation = ops.generation;
        Operations::put_into(
            &mut ops.data,
            generation,
            key.clone(),
            AbstractBatchOperation::Put {
                key,
                value,
                children_sizes,
                cost_info,
            },
        );
    }

    /// Mark the boundary between two independent operations sharing this
    /// batch (e.g. consecutive Merk commits driven through a `MerkCache`).
    /// Everything recorded so far is treated as the settled outcome of
    /// earlier operations: a `delete` issued after this point supersedes an
    /// earlier `put` of the same key, while within a single operation the
    /// put-wins rule still holds.
    pub fn next_operation(&self) {
        self.operations.borrow_mut().generation += 1;
    }

    /// Add deferred `put` operation for aux storage
    pub(crate) fn put_aux(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    ) {
        let ops = &mut *self.operations.borrow_mut();
        let generation = ops.generation;
        Operations::put_into(
            &mut ops.aux,
            generation,
            key.clone(),
            AbstractBatchOperation::PutAux {
                key,
                value,
                cost_info,
            },
        );
    }

    /// Add deferred `put` operation for subtree roots storage
    pub(crate) fn put_root(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    ) {
        let ops = &mut *self.operations.borrow_mut();
        let generation = ops.generation;
        Operations::put_into(
            &mut ops.roots,
            generation,
            key.clone(),
            AbstractBatchOperation::PutRoot {
                key,
                value,
                cost_info,
            },
        );
    }

    /// Add deferred `put` operation for metadata storage
    pub(crate) fn put_meta(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    ) {
        let ops = &mut *self.operations.borrow_mut();
        let generation = ops.generation;
        Operations::put_into(
            &mut ops.meta,
            generation,
            key.clone(),
            AbstractBatchOperation::PutMeta {
                key,
                value,
                cost_info,
            },
        );
    }

    /// Add deferred `delete` operation.
    ///
    /// If a `put` for the same key already exists in this batch FROM THE SAME
    /// OPERATION, the delete is silently dropped — the put always wins within
    /// a single operation. This is intentional: during tree rebalancing, a
    /// node may be deleted from one position and re-inserted at another
    /// within the same commit.
    ///
    /// AUDIT NOTE (issue #698 — intentional, do not re-flag): a `StorageBatch`
    /// is a keyed map of the *final* state for each key within one atomic
    /// commit, NOT an ordered operation log. There is therefore no meaningful
    /// "put then delete then commit" ordering to honor within one Merk commit
    /// — rebalancing legitimately emits a delete and a put for the same key,
    /// and the surviving value (the put) is exactly the intended end state.
    /// Making a later delete win INSIDE an operation would drop nodes that
    /// rebalancing just re-inserted and corrupt the tree.
    ///
    /// Across operations it is the opposite: when several Merk commits share
    /// one batch (a `MerkCache` flow — e.g. a delete whose rebalancing
    /// rewrote a neighbouring node, followed by a cascade that deletes that
    /// very node), the later operation's delete must remove the key an
    /// earlier one had put. [`StorageBatch::next_operation`] marks those
    /// boundaries; an entry from an earlier generation is superseded.
    pub(crate) fn delete(&self, key: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        let ops = &mut *self.operations.borrow_mut();
        let generation = ops.generation;
        Operations::delete_into(
            &mut ops.data,
            generation,
            key.clone(),
            AbstractBatchOperation::Delete { key, cost_info },
        );
    }

    /// Add deferred `delete` operation for aux storage.
    ///
    /// Same put-wins semantics as [`Self::delete`].
    pub(crate) fn delete_aux(&self, key: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        let ops = &mut *self.operations.borrow_mut();
        let generation = ops.generation;
        Operations::delete_into(
            &mut ops.aux,
            generation,
            key.clone(),
            AbstractBatchOperation::DeleteAux { key, cost_info },
        );
    }

    /// Add deferred `delete` operation for subtree roots storage.
    ///
    /// Same put-wins semantics as [`Self::delete`].
    pub(crate) fn delete_root(&self, key: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        let ops = &mut *self.operations.borrow_mut();
        let generation = ops.generation;
        Operations::delete_into(
            &mut ops.roots,
            generation,
            key.clone(),
            AbstractBatchOperation::DeleteRoot { key, cost_info },
        );
    }

    /// Add deferred `delete` operation for metadata storage
    pub(crate) fn delete_meta(&self, key: Vec<u8>, cost_info: Option<KeyValueStorageCost>) {
        let ops = &mut *self.operations.borrow_mut();
        let generation = ops.generation;
        Operations::delete_into(
            &mut ops.meta,
            generation,
            key.clone(),
            AbstractBatchOperation::DeleteMeta { key, cost_info },
        );
    }

    /// Merge batch into this one
    pub fn merge(&self, other: StorageBatch) {
        for op in other.into_iter() {
            match op {
                AbstractBatchOperation::Put {
                    key,
                    value,
                    children_sizes,
                    cost_info,
                } => self.put(key, value, children_sizes, cost_info),
                AbstractBatchOperation::PutAux {
                    key,
                    value,
                    cost_info,
                } => self.put_aux(key, value, cost_info),
                AbstractBatchOperation::PutRoot {
                    key,
                    value,
                    cost_info,
                } => self.put_root(key, value, cost_info),
                AbstractBatchOperation::PutMeta {
                    key,
                    value,
                    cost_info,
                } => self.put_meta(key, value, cost_info),
                AbstractBatchOperation::Delete { key, cost_info } => self.delete(key, cost_info),
                AbstractBatchOperation::DeleteAux { key, cost_info } => {
                    self.delete_aux(key, cost_info)
                }
                AbstractBatchOperation::DeleteRoot { key, cost_info } => {
                    self.delete_root(key, cost_info)
                }
                AbstractBatchOperation::DeleteMeta { key, cost_info } => {
                    self.delete_meta(key, cost_info)
                }
            }
        }
    }

    /// Merge batch into this one prioritizing operations of the provided batch
    /// for deletions. The original [[merge]] doesn't overwrite operations
    /// with deletions keeping keys if they were inserted before.
    pub fn merge_overwriting(&self, other: StorageBatch) {
        let other_ops = other.operations.into_inner();
        let mut ops = self.operations.borrow_mut();
        let generation = ops.generation;

        fn restamp(
            entries: BTreeMap<Vec<u8>, Entry>,
            generation: u64,
        ) -> impl Iterator<Item = (Vec<u8>, Entry)> {
            entries.into_iter().map(move |(key, entry)| {
                (
                    key,
                    Entry {
                        generation,
                        op: entry.op,
                    },
                )
            })
        }

        ops.data.extend(restamp(other_ops.data, generation));
        ops.meta.extend(restamp(other_ops.meta, generation));
        ops.aux.extend(restamp(other_ops.aux, generation));
        ops.roots.extend(restamp(other_ops.roots, generation));
        // The merged content is the settled outcome of the other batch's
        // operations; whatever follows is a later operation.
        ops.generation += 1;
    }
}

/// Iterator over storage batch operations.
pub(crate) struct StorageBatchIter {
    data: IntoValues<Vec<u8>, Entry>,
    aux: IntoValues<Vec<u8>, Entry>,
    meta: IntoValues<Vec<u8>, Entry>,
    roots: IntoValues<Vec<u8>, Entry>,
}

impl Iterator for StorageBatchIter {
    type Item = AbstractBatchOperation;

    fn next(&mut self) -> Option<Self::Item> {
        self.meta
            .next()
            .or_else(|| self.aux.next())
            .or_else(|| self.roots.next())
            .or_else(|| self.data.next())
            .map(|entry| entry.op)
    }
}

// Making this a method rather than `IntoIter` implementation as we don't want
// to leak multi context batch internals in any way
impl StorageBatch {
    pub(crate) fn into_iter(self) -> StorageBatchIter {
        let operations = self.operations.into_inner();

        StorageBatchIter {
            data: operations.data.into_values(),
            aux: operations.aux.into_values(),
            meta: operations.meta.into_values(),
            roots: operations.roots.into_values(),
        }
    }
}

impl Default for StorageBatch {
    fn default() -> Self {
        Self::new()
    }
}

/// Deferred storage operation not tied to any storage implementation,
/// required for multi-tree batches.
#[allow(missing_docs)]
#[derive(strum::AsRefStr)]
pub(crate) enum AbstractBatchOperation {
    /// Deferred put operation
    Put {
        key: Vec<u8>,
        value: Vec<u8>,
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    },
    /// Deferred put operation for aux storage
    PutAux {
        key: Vec<u8>,
        value: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    /// Deferred put operation for roots storage
    PutRoot {
        key: Vec<u8>,
        value: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    /// Deferred put operation for metadata storage
    PutMeta {
        key: Vec<u8>,
        value: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    /// Deferred delete operation
    Delete {
        key: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    /// Deferred delete operation for aux storage
    DeleteAux {
        key: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    /// Deferred delete operation for roots storage
    DeleteRoot {
        key: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
    /// Deferred delete operation for metadata storage
    DeleteMeta {
        key: Vec<u8>,
        cost_info: Option<KeyValueStorageCost>,
    },
}

impl std::fmt::Debug for AbstractBatchOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut fmt = f.debug_struct(self.as_ref());

        let mut key_buf = Vec::new();
        let mut value_buf = Vec::new();

        match self {
            AbstractBatchOperation::Put { key, value, .. }
            | AbstractBatchOperation::PutAux { key, value, .. }
            | AbstractBatchOperation::PutMeta { key, value, .. }
            | AbstractBatchOperation::PutRoot { key, value, .. } => {
                key_buf.clear();
                value_buf.clear();
                visualize_to_vec(&mut key_buf, key.as_slice());
                visualize_to_vec(&mut value_buf, value.as_slice());
                fmt.field("key", &String::from_utf8_lossy(&key_buf))
                    .field("value", &String::from_utf8_lossy(&value_buf));
            }
            AbstractBatchOperation::Delete { key, .. }
            | AbstractBatchOperation::DeleteAux { key, .. }
            | AbstractBatchOperation::DeleteMeta { key, .. }
            | AbstractBatchOperation::DeleteRoot { key, .. } => {
                key_buf.clear();
                visualize_to_vec(&mut key_buf, key.as_slice());
                fmt.field("key", &String::from_utf8_lossy(&key_buf));
            }
        }

        fmt.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grovedb_costs::{
        storage_cost::{
            key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes::BasicStorageRemoval,
            StorageCost,
        },
        ChildrenSizesWithIsSumTree,
    };

    fn dummy_children_sizes() -> ChildrenSizesWithIsSumTree {
        None
    }

    fn removed_bytes_cost(bytes: u32) -> KeyValueStorageCost {
        KeyValueStorageCost {
            key_storage_cost: StorageCost {
                added_bytes: 0,
                replaced_bytes: 0,
                removed_bytes: BasicStorageRemoval(bytes),
            },
            value_storage_cost: StorageCost::default(),
            new_node: false,
            needs_value_verification: false,
            prepaid: false,
        }
    }

    #[test]
    fn test_debug_output_batch_operation() {
        let op1 = AbstractBatchOperation::PutMeta {
            key: b"key1".to_vec(),
            value: b"value1".to_vec(),
            cost_info: None,
        };
        let op2 = AbstractBatchOperation::DeleteRoot {
            key: b"key1".to_vec(),
            cost_info: None,
        };
        assert_eq!(
            format!("{op1:?}"),
            "PutMeta { key: \"[hex: 6b657931, str: key1]\", value: \"[hex: 76616c756531, str: \
             value1]\" }"
        );
        assert_eq!(
            format!("{op2:?}"),
            "DeleteRoot { key: \"[hex: 6b657931, str: key1]\" }"
        );
    }

    #[test]
    fn test_storage_batch_put_then_delete_keeps_put_operation() {
        let batch = StorageBatch::new();

        batch.put(
            b"key".to_vec(),
            b"value".to_vec(),
            dummy_children_sizes(),
            None,
        );
        batch.delete(b"key".to_vec(), None);

        assert_eq!(batch.len(), 1);
        let operations: Vec<_> = batch.into_iter().collect();
        assert_eq!(operations.len(), 1);
        assert!(matches!(operations[0], AbstractBatchOperation::Put { .. }));
    }

    #[test]
    fn test_storage_batch_delete_then_put_overwrites_delete() {
        let batch = StorageBatch::new();

        batch.delete(b"key".to_vec(), Some(removed_bytes_cost(5)));
        batch.put(
            b"key".to_vec(),
            b"value".to_vec(),
            dummy_children_sizes(),
            None,
        );

        assert_eq!(batch.len(), 1);
        let operations: Vec<_> = batch.into_iter().collect();
        assert_eq!(operations.len(), 1);
        assert!(matches!(operations[0], AbstractBatchOperation::Put { .. }));
    }

    #[test]
    fn test_storage_batch_duplicate_delete_does_not_replace_cost_info() {
        let batch = StorageBatch::new();

        batch.delete_aux(b"key".to_vec(), Some(removed_bytes_cost(3)));
        batch.delete_aux(b"key".to_vec(), Some(removed_bytes_cost(7)));

        let mut operations = batch.into_iter();
        match operations.next().expect("expected one operation") {
            AbstractBatchOperation::DeleteAux { cost_info, .. } => {
                let removed = cost_info.expect("cost info should be set");
                assert_eq!(removed.combined_removed_bytes(), BasicStorageRemoval(3));
            }
            op => panic!("unexpected operation: {op:?}"),
        }
        assert!(operations.next().is_none());
    }

    #[test]
    fn test_storage_batch_later_operation_delete_supersedes_earlier_put() {
        let batch = StorageBatch::new();

        // Operation 1 (e.g. a Merk commit whose rebalancing rewrote a node).
        batch.put(
            b"key".to_vec(),
            b"value".to_vec(),
            dummy_children_sizes(),
            None,
        );
        batch.next_operation();
        // Operation 2 deletes that very node: it must win.
        batch.delete(b"key".to_vec(), Some(removed_bytes_cost(5)));

        let operations: Vec<_> = batch.into_iter().collect();
        assert_eq!(operations.len(), 1);
        assert!(matches!(
            operations[0],
            AbstractBatchOperation::Delete { .. }
        ));
    }

    #[test]
    fn test_storage_batch_put_wins_within_one_operation_after_boundary() {
        let batch = StorageBatch::new();
        batch.next_operation();

        // Within the new operation the put-wins rule is unchanged, in both
        // orders.
        batch.put(b"a".to_vec(), b"1".to_vec(), dummy_children_sizes(), None);
        batch.delete(b"a".to_vec(), None);
        batch.delete(b"b".to_vec(), None);
        batch.put(b"b".to_vec(), b"2".to_vec(), dummy_children_sizes(), None);

        let variants: Vec<_> = batch
            .into_iter()
            .map(|op| matches!(op, AbstractBatchOperation::Put { .. }))
            .collect();
        assert_eq!(variants, vec![true, true]);
    }

    #[test]
    fn test_storage_batch_merge_overwriting_settles_generation() {
        let batch = StorageBatch::new();
        let other = StorageBatch::new();
        other.put(b"key".to_vec(), b"v".to_vec(), dummy_children_sizes(), None);

        batch.merge_overwriting(other);
        // The merged put is an earlier operation's outcome: a delete issued
        // afterwards removes the key.
        batch.delete(b"key".to_vec(), None);

        let operations: Vec<_> = batch.into_iter().collect();
        assert_eq!(operations.len(), 1);
        assert!(matches!(
            operations[0],
            AbstractBatchOperation::Delete { .. }
        ));
    }

    #[test]
    fn test_storage_batch_merge_and_iteration_order() {
        let batch = StorageBatch::new();
        let other = StorageBatch::new();

        other.put(b"d".to_vec(), b"1".to_vec(), dummy_children_sizes(), None);
        other.put_root(b"r".to_vec(), b"2".to_vec(), None);
        other.put_aux(b"a".to_vec(), b"3".to_vec(), None);
        other.put_meta(b"m".to_vec(), b"4".to_vec(), None);
        other.delete_meta(b"m_del".to_vec(), Some(removed_bytes_cost(9)));

        batch.merge(other);

        assert_eq!(batch.len(), 5);
        assert!(!batch.is_empty());

        let variants: Vec<_> = batch
            .into_iter()
            .map(|op| match op {
                AbstractBatchOperation::Put { .. } => "Put",
                AbstractBatchOperation::PutAux { .. } => "PutAux",
                AbstractBatchOperation::PutRoot { .. } => "PutRoot",
                AbstractBatchOperation::PutMeta { .. } => "PutMeta",
                AbstractBatchOperation::Delete { .. } => "Delete",
                AbstractBatchOperation::DeleteAux { .. } => "DeleteAux",
                AbstractBatchOperation::DeleteRoot { .. } => "DeleteRoot",
                AbstractBatchOperation::DeleteMeta { .. } => "DeleteMeta",
            })
            .collect();

        // Iterator traverses maps in this fixed order: meta, aux, roots, data.
        assert_eq!(
            variants,
            vec!["PutMeta", "DeleteMeta", "PutAux", "PutRoot", "Put"]
        );
    }
}
