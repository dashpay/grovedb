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

//! Implementation for a storage abstraction over RocksDB.

use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use error::Error;
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval, CostContext, CostResult,
    CostsExt, OperationCost,
};
use grovedb_path::SubtreePath;
use integer_encoding::VarInt;
use lazy_static::lazy_static;
#[cfg(feature = "unsafe-dump-load")]
use rocksdb::IngestExternalFileOptions;
use rocksdb::{
    checkpoint::Checkpoint, ColumnFamily, ColumnFamilyDescriptor, DBRawIteratorWithThreadMode,
    FlushOptions, OptimisticTransactionDB, OptimisticTransactionOptions, ReadOptions, Transaction,
    WriteBatchWithTransaction, WriteOptions, DEFAULT_COLUMN_FAMILY_NAME,
};

use super::{PrefixedRocksDbImmediateStorageContext, PrefixedRocksDbTransactionContext};
use crate::{
    error,
    error::Error::{CostError, RocksDBError},
    storage::AbstractBatchOperation,
    worst_case_costs::WorstKeyLength,
    Storage, StorageBatch,
};

const BLAKE_BLOCK_LEN: usize = 64;
pub type SubtreePrefix = [u8; 32];

fn blake_block_count(len: usize) -> usize {
    if len == 0 {
        1
    } else {
        1 + (len - 1) / BLAKE_BLOCK_LEN
    }
}

/// Name of column family used to store auxiliary data
pub(crate) const AUX_CF_NAME: &str = "aux";
/// Name of column family used to store subtrees roots data
pub(crate) const ROOTS_CF_NAME: &str = "roots";
/// Name of column family used to store metadata
pub(crate) const META_CF_NAME: &str = "meta";

lazy_static! {
    static ref DEFAULT_OPTS: rocksdb::Options = {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.increase_parallelism(num_cpus::get() as i32);
        opts.set_allow_mmap_writes(true);
        opts.set_allow_mmap_reads(true);
        opts.create_missing_column_families(true);
        opts.set_atomic_flush(true);
        opts
    };
}

lazy_static! {
    static ref READ_ONLY_CHECKPOINTS_OPTS: rocksdb::Options = {
        let mut opts = rocksdb::Options::default();
        // Absolutely do NOT create or modify anything
        opts.create_if_missing(false);
        opts.create_missing_column_families(false);

        // Read-only DBs should not write WALs or SSTs
        opts.set_allow_mmap_writes(false);

        // mmap reads are fine and often beneficial for read-heavy workloads
        opts.set_allow_mmap_reads(true);

        // Avoid background work that could try to write files
        opts.set_disable_auto_compactions(true);

        // Optional but recommended: reduce background threads
        opts.increase_parallelism(1);
        opts
    };
}

/// Type alias for a database
pub(crate) type Db = OptimisticTransactionDB;

/// Type alias for the raw RocksDB transaction. Private to this module:
/// every read and write the storage layer performs through a transaction
/// must go through the [`Tx`] wrapper's methods, never the raw handle.
pub(crate) type RawTx<'db> = Transaction<'db, Db>;

/// Reads executing on a snapshot held longer than this trip a loud
/// debug-build warning. A snapshot is O(1) to take but pins every
/// post-snapshot version while held, so a leaked or session-scoped
/// snapshot transaction silently turns into compaction debt and write
/// amplification. The intended holders are millisecond-scoped
/// multi-operation reads; one second is three orders of magnitude above
/// that, so a trip is a bug in the caller, not load jitter.
#[cfg(debug_assertions)]
const SNAPSHOT_AGE_WARN_THRESHOLD: Duration = Duration::from_secs(1);

/// A started storage transaction.
///
/// Wraps the raw RocksDB optimistic transaction together with the
/// snapshot-read marker set by
/// [`RocksDbStorage::start_snapshot_read_transaction`]. The wrapper is
/// the single funnel for everything the storage layer does through a
/// transaction:
///
/// - **Reads** ([`Tx::get`], [`Tx::get_cf`], [`Tx::raw_iterator`])
///   inject the transaction's snapshot into every read's options.
///   RocksDB transactions do NOT read from their snapshot by default,
///   so a read added later that bypassed this funnel would silently
///   revert to latest-committed reads; keeping the raw un-optioned
///   accessors private makes that bypass impossible outside this
///   module.
/// - **Writes and commit** ([`Tx::put`], [`Tx::delete`],
///   [`Tx::rebuild_from_writebatch`], [`Tx::commit`], and their `_cf`
///   variants) refuse a snapshot read transaction with
///   [`Error::SnapshotReadOnlyTransaction`]: `set_snapshot` arms
///   commit-time conflict detection, so writes through one may fail
///   with `Busy`/`TryAgain` where a plain transaction's would have
///   committed. Refusing up front turns that timing-dependent trap
///   into a deterministic typed error.
pub struct Tx<'db> {
    tx: RawTx<'db>,
    /// `Some(creation time)` if and only if this transaction was
    /// created via `start_snapshot_read_transaction`. Doubles as the
    /// read-only marker and the age baseline for [`Tx::snapshot_age`].
    snapshot_read_since: Option<Instant>,
    /// Debug builds warn once per transaction on a long-held snapshot;
    /// this remembers that the warning already fired.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    age_warned: AtomicBool,
}

impl<'db> Tx<'db> {
    /// Wrap a plain transaction (reads latest committed state).
    pub(crate) fn new_plain(tx: RawTx<'db>) -> Self {
        Tx {
            tx,
            snapshot_read_since: None,
            age_warned: AtomicBool::new(false),
        }
    }

    /// Wrap a snapshot read transaction (reads pinned to creation-time
    /// committed state; writes and commit refused).
    pub(crate) fn new_snapshot_read(tx: RawTx<'db>) -> Self {
        Tx {
            tx,
            snapshot_read_since: Some(Instant::now()),
            age_warned: AtomicBool::new(false),
        }
    }

    /// Whether this transaction was created via
    /// `start_snapshot_read_transaction`: reads are pinned to its
    /// creation-time committed state and writes/commit are refused.
    pub fn is_snapshot_read(&self) -> bool {
        self.snapshot_read_since.is_some()
    }

    /// How long this transaction's snapshot has been held, or `None`
    /// for a plain transaction. While held, the snapshot pins every
    /// post-snapshot version in RocksDB — intended holds are
    /// millisecond-scoped, and debug builds log loudly when a read
    /// executes on a snapshot older than one second.
    pub fn snapshot_age(&self) -> Option<Duration> {
        self.snapshot_read_since.map(|since| since.elapsed())
    }

    /// The typed refusal for write operations on a snapshot read
    /// transaction, `Ok(())` on a plain transaction.
    fn refuse_snapshot_write(&self, operation: &'static str) -> Result<(), Error> {
        if self.is_snapshot_read() {
            Err(Error::SnapshotReadOnlyTransaction(operation))
        } else {
            Ok(())
        }
    }

    /// Read options honoring the transaction's snapshot, when one was
    /// requested at creation.
    ///
    /// A plain transaction's snapshot handle is null, which RocksDB
    /// documents as leaving reads on the latest committed state, so
    /// this is a no-op for every non-snapshot transaction. The handle
    /// stored into the options is owned by the transaction (which
    /// outlives every context borrowing it); the temporary wrapper
    /// only frees its C shell on drop.
    fn read_options(&self) -> ReadOptions {
        #[cfg(debug_assertions)]
        if let Some(age) = self.snapshot_age()
            && age > SNAPSHOT_AGE_WARN_THRESHOLD
            && !self
                .age_warned
                .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            eprintln!(
                "WARNING (grovedb-storage): read on a snapshot read transaction whose \
                 snapshot has been held for {age:?} (threshold {SNAPSHOT_AGE_WARN_THRESHOLD:?}). \
                 A held snapshot pins every post-snapshot RocksDB version — a leaked or \
                 session-scoped snapshot transaction becomes compaction debt and write \
                 amplification under load. Scope snapshot transactions to a single \
                 multi-operation read. (Warning fires once per transaction.)"
            );
        }
        let mut read_options = ReadOptions::default();
        read_options.set_snapshot(&self.tx.snapshot());
        read_options
    }

    /// Snapshot-honoring point read from the default column family.
    pub(crate) fn get<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, rocksdb::Error> {
        self.tx.get_opt(key, &self.read_options())
    }

    /// Snapshot-honoring point read from a named column family.
    pub(crate) fn get_cf<K: AsRef<[u8]>>(
        &self,
        cf: &ColumnFamily,
        key: K,
    ) -> Result<Option<Vec<u8>>, rocksdb::Error> {
        self.tx.get_cf_opt(cf, key, &self.read_options())
    }

    /// Snapshot-honoring raw iterator over the default column family.
    pub(crate) fn raw_iterator(&self) -> DBRawIteratorWithThreadMode<'_, RawTx<'db>> {
        self.tx.raw_iterator_opt(self.read_options())
    }

    /// Write into the default column family. Refused on a snapshot
    /// read transaction.
    pub(crate) fn put<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &self,
        key: K,
        value: V,
    ) -> Result<(), Error> {
        self.refuse_snapshot_write("put")?;
        self.tx.put(key, value).map_err(RocksDBError)
    }

    /// Write into a named column family. Refused on a snapshot read
    /// transaction.
    pub(crate) fn put_cf<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &self,
        cf: &ColumnFamily,
        key: K,
        value: V,
    ) -> Result<(), Error> {
        self.refuse_snapshot_write("put")?;
        self.tx.put_cf(cf, key, value).map_err(RocksDBError)
    }

    /// Delete from the default column family. Refused on a snapshot
    /// read transaction.
    pub(crate) fn delete<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Error> {
        self.refuse_snapshot_write("delete")?;
        self.tx.delete(key).map_err(RocksDBError)
    }

    /// Delete from a named column family. Refused on a snapshot read
    /// transaction.
    pub(crate) fn delete_cf<K: AsRef<[u8]>>(&self, cf: &ColumnFamily, key: K) -> Result<(), Error> {
        self.refuse_snapshot_write("delete")?;
        self.tx.delete_cf(cf, key).map_err(RocksDBError)
    }

    /// Replay a write batch into this transaction. Refused on a
    /// snapshot read transaction — this is the batch-apply write entry
    /// point.
    pub(crate) fn rebuild_from_writebatch(
        &self,
        batch: &WriteBatchWithTransaction<true>,
    ) -> Result<(), Error> {
        self.refuse_snapshot_write("batch apply")?;
        self.tx.rebuild_from_writebatch(batch).map_err(RocksDBError)
    }

    /// Consume and commit the transaction. Refused on a snapshot read
    /// transaction (which by construction has nothing to commit — its
    /// writes were already refused).
    pub fn commit(self) -> Result<(), Error> {
        self.refuse_snapshot_write("commit")?;
        self.tx.commit().map_err(RocksDBError)
    }

    /// Roll back the transaction's pending writes. Allowed on a
    /// snapshot read transaction: it is a harmless no-op there and an
    /// error would only complicate callers' cleanup paths.
    pub fn rollback(&self) -> Result<(), Error> {
        self.tx.rollback().map_err(RocksDBError)
    }

    /// Set a savepoint: a later [`Tx::rollback_to_savepoint`] undoes
    /// every operation in this transaction since this call. Used by
    /// callers that interleave many independent write groups in one
    /// transaction (e.g. per-state-transition savepoints in block
    /// processing) to unwind one failed group without discarding the
    /// transaction. Allowed on a snapshot read transaction — like
    /// [`Tx::rollback`], the savepoint family only ever *unwinds*
    /// writes (which a snapshot read transaction cannot accumulate),
    /// so there is nothing to refuse.
    pub fn set_savepoint(&self) {
        self.tx.set_savepoint()
    }

    /// Undo all operations in this transaction since the most recent
    /// [`Tx::set_savepoint`], popping that savepoint. See
    /// [`Tx::set_savepoint`] for the intended use and the
    /// snapshot-read policy.
    pub fn rollback_to_savepoint(&self) -> Result<(), Error> {
        self.tx.rollback_to_savepoint().map_err(RocksDBError)
    }
}

/// Storage which uses RocksDB as its backend.
///
/// Uses `OptimisticTransactionDB` for transaction support. Optimistic
/// transactions defer conflict detection to commit time rather than
/// acquiring locks up front. This means multiple transactions can be
/// started concurrently, but at most one write transaction should be
/// active at a time. If two transactions modify overlapping keys, the
/// later commit will fail with a `Busy` or `TryAgain` error.
///
/// See the [`Storage`] trait documentation for the single-writer
/// requirement.
pub struct RocksDbStorage {
    db: OptimisticTransactionDB,
}
const DEFAULT_LOG_SIZE_FOR_CHECKPOINT_FLUSH: u64 = u64::MAX; // Never flush

impl RocksDbStorage {
    /// Create RocksDb storage with default parameters using `path`.
    pub fn default_rocksdb_with_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = Db::open_cf_descriptors(
            &DEFAULT_OPTS,
            &path,
            [
                ColumnFamilyDescriptor::new(AUX_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(ROOTS_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(META_CF_NAME, DEFAULT_OPTS.clone()),
            ],
        )
        .map_err(RocksDBError)?;
        Ok(RocksDbStorage { db })
    }

    /// Create RocksDb storage with checkpoint parameters using `path` in read
    /// only mode.
    pub fn checkpoint_rocksdb_with_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = Db::open_cf_descriptors(
            &READ_ONLY_CHECKPOINTS_OPTS,
            &path,
            [
                ColumnFamilyDescriptor::new(AUX_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(ROOTS_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(META_CF_NAME, DEFAULT_OPTS.clone()),
            ],
        )
        .map_err(RocksDBError)?;
        Ok(RocksDbStorage { db })
    }

    fn build_prefix_body<B>(path: SubtreePath<B>) -> (Vec<u8>, usize)
    where
        B: AsRef<[u8]>,
    {
        let segments_iter = path.into_reverse_iter();
        let mut segments_count: usize = 0;
        let mut res = Vec::new();
        let mut lengths = Vec::new();

        for s in segments_iter {
            segments_count += 1;
            res.extend_from_slice(s);
            lengths.push(u8::try_from(s.len()).expect(
                "path segment length must not exceed 255 bytes; \
                 this is enforced at insert time via validate_key_length",
            ));
        }

        // Note: this uses native-endian encoding. Changing to big-endian would
        // be a breaking change since the output is hashed with Blake3 to produce
        // subtree prefixes stored in RocksDB — existing databases would become
        // unreadable. In practice GroveDB only targets little-endian platforms
        // (x86_64, aarch64), so this is not a portability concern.
        res.extend(segments_count.to_ne_bytes());
        res.extend(lengths);
        (res, segments_count)
    }

    /// A helper method to build a prefix to rocksdb keys or identify a subtree
    /// in `subtrees` map by tree path;
    pub fn build_prefix<B>(path: SubtreePath<B>) -> CostContext<SubtreePrefix>
    where
        B: AsRef<[u8]>,
    {
        let (body, segments_count) = Self::build_prefix_body(path);
        if segments_count == 0 {
            SubtreePrefix::default().wrap_with_cost(OperationCost::default())
        } else {
            let blocks_count = blake_block_count(body.len());
            SubtreePrefix::from(blake3::hash(&body))
                .wrap_with_cost(OperationCost::with_hash_node_calls(blocks_count as u32))
        }
    }

    /// Derive the per-axis secondary `SubtreePrefix` for any indexed-tree
    /// element (`ProvableSumIndexedTree`, `ProvableCountIndexedTree`, or
    /// `ProvableCountProvableSumIndexedTree`) whose primary prefix is
    /// given and whose axis is identified by `axis_tag`.
    ///
    /// Per S2-B (generalized): `secondary_prefix = Blake3(primary_prefix ‖
    /// axis_tag)`. The axis tag byte is the `IndexAxis` value carried by
    /// `grovedb-element::indexed::IndexAxis` (`0 = count`, `1 = sum`,
    /// `2 = avg`). PCPSIT carries 1..=3 secondaries — one per axis — and
    /// each lives at a distinct prefix derived via this function.
    ///
    /// Three useful properties hold:
    /// - **Primary parity with `Tree`.** The primary prefix is unchanged
    ///   from `build_prefix` for the same path, so an indexed-tree's
    ///   primary Merk lives where a `Tree` would.
    /// - **Collision-free secondary.** The secondary's Blake3 input is a
    ///   33-byte block (prefix ‖ axis_tag) that no path-derived prefix can
    ///   produce — `build_prefix_body` always ends with per-segment length
    ///   bytes, never a single tag byte after a 32-byte block.
    /// - **Per-axis isolation.** Different `axis_tag` values map the same
    ///   primary to different storage namespaces, so a PCPSIT's count /
    ///   sum / avg secondaries do not collide with each other.
    ///
    /// Cost: one Blake3 call (33 bytes fits in a single 64-byte block).
    pub fn secondary_prefix_for(
        primary_prefix: &SubtreePrefix,
        axis_tag: u8,
    ) -> CostContext<SubtreePrefix> {
        // 33 bytes (prefix + tag) fits within one Blake3 block.
        let mut hasher = blake3::Hasher::new();
        hasher.update(primary_prefix);
        hasher.update(&[axis_tag]);
        SubtreePrefix::from(hasher.finalize())
            .wrap_with_cost(OperationCost::with_hash_node_calls(1))
    }

    fn worst_case_body_size<L: WorstKeyLength>(path: &[L]) -> usize {
        // body = segment_bytes + segments_count.to_ne_bytes() + lengths
        // segments_count.to_ne_bytes() contributes size_of::<usize>() bytes
        path.iter().map(|a| a.max_length() as usize).sum::<usize>()
            + std::mem::size_of::<usize>()
            + path.len()
    }

    /// Returns the write batch, with costs and pending costs
    /// Pending costs are costs that should only be applied after successful
    /// write of the write batch.
    pub fn build_write_batch(
        &self,
        storage_batch: StorageBatch,
    ) -> CostResult<(WriteBatchWithTransaction<true>, OperationCost), Error> {
        let mut db_batch = WriteBatchWithTransaction::<true>::default();
        self.continue_write_batch(&mut db_batch, storage_batch)
            .map_ok(|operation_cost| (db_batch, operation_cost))
    }

    /// Continues the write batch, returning pending costs.
    ///
    /// Pending costs are costs that should only be applied after successful
    /// write of the write batch.
    ///
    /// # Delete cost computation when `cost_info` is `None`
    ///
    /// When a delete operation lacks pre-computed `cost_info`, the freed-bytes
    /// cost is estimated by reading the current value from **committed**
    /// database state (`self.db.get()`), not from the in-flight batch or
    /// transaction. This is a known TOCTOU-style limitation in cost
    /// accounting only -- it does **not** affect data integrity.
    ///
    /// In practice this is acceptable because:
    ///
    /// 1. The main tree-node delete path (Merk commit) **always** provides
    ///    `Some(cost_info)` via `KeyUpdates::deleted_keys`, so the fallback
    ///    never runs for the performance-critical and cost-sensitive path.
    ///
    /// 2. The `None` fallback is only reached by cleanup/utility operations:
    ///    - `Merk::clear()` and `PrefixedRocksDbTransactionContext::clear()`
    ///      (subtree deletion)
    ///    - `Merk::delete_meta()` (metadata cleanup)
    ///    - `Merk::set_base_root_key(None)` (root key removal)
    ///    - `GroveDb::delete_aux()` when the caller omits cost info
    ///      These operations intentionally trade cost precision for simplicity.
    ///
    /// 3. The `StorageBatch` put-wins semantics (see [`StorageBatch::delete`])
    ///    prevents the worst-case scenario where a same-batch put+delete for
    ///    the same key would make the committed-state read completely stale.
    ///
    /// 4. If the key was inserted within the current (uncommitted) transaction
    ///    but is not yet committed, `self.db.get()` returns `None` / the old
    ///    committed value, causing freed bytes to be **underestimated** -- a
    ///    safe direction for cost accounting.
    pub fn continue_write_batch(
        &self,
        db_batch: &mut WriteBatchWithTransaction<true>,
        storage_batch: StorageBatch,
    ) -> CostResult<OperationCost, Error> {
        let mut cost = OperationCost::default();
        // Until batch is committed these costs are pending (should not be added in case
        // of early termination).
        let mut pending_costs = OperationCost::default();

        for op in storage_batch.into_iter() {
            match op {
                AbstractBatchOperation::Put {
                    key,
                    value,
                    children_sizes,
                    cost_info,
                } => {
                    db_batch.put(&key, &value);
                    // A fully prepaid put (`KeyValueStorageCost::prepaid`)
                    // was billed by its owner in advance, the write
                    // included: no seek here. Only the append-only family's
                    // GROVE_V4 accounting issues such puts.
                    if !cost_info.as_ref().is_some_and(|c| c.is_prepaid()) {
                        cost.seek_count += 1;
                    }
                    cost_return_on_error_no_add!(
                        cost,
                        pending_costs
                            .add_key_value_storage_costs(
                                key.len() as u32,
                                value.len() as u32,
                                children_sizes,
                                cost_info
                            )
                            .map_err(CostError)
                    );
                }
                AbstractBatchOperation::PutAux {
                    key,
                    value,
                    cost_info,
                } => {
                    db_batch.put_cf(cf_aux(&self.db), &key, &value);
                    if !cost_info.as_ref().is_some_and(|c| c.is_prepaid()) {
                        cost.seek_count += 1;
                    }
                    cost_return_on_error_no_add!(
                        cost,
                        pending_costs
                            .add_key_value_storage_costs(
                                key.len() as u32,
                                value.len() as u32,
                                None,
                                cost_info
                            )
                            .map_err(CostError)
                    );
                }
                AbstractBatchOperation::PutRoot {
                    key,
                    value,
                    cost_info,
                } => {
                    db_batch.put_cf(cf_roots(&self.db), &key, &value);
                    if !cost_info.as_ref().is_some_and(|c| c.is_prepaid()) {
                        cost.seek_count += 1;
                    }
                    // We only add costs for put root if they are set, otherwise it is free
                    if cost_info.is_some() {
                        cost_return_on_error_no_add!(
                            cost,
                            pending_costs
                                .add_key_value_storage_costs(
                                    key.len() as u32,
                                    value.len() as u32,
                                    None,
                                    cost_info
                                )
                                .map_err(CostError)
                        );
                    }
                }
                AbstractBatchOperation::PutMeta {
                    key,
                    value,
                    cost_info,
                } => {
                    db_batch.put_cf(cf_meta(&self.db), &key, &value);
                    if !cost_info.as_ref().is_some_and(|c| c.is_prepaid()) {
                        cost.seek_count += 1;
                    }
                    cost_return_on_error_no_add!(
                        cost,
                        pending_costs
                            .add_key_value_storage_costs(
                                key.len() as u32,
                                value.len() as u32,
                                None,
                                cost_info
                            )
                            .map_err(CostError)
                    );
                }
                AbstractBatchOperation::Delete { key, cost_info } => {
                    db_batch.delete(&key);

                    // Non-atomic freed-size fallback: reads committed state.
                    // See method-level doc comment for rationale.

                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        // lets get the values
                        let value_len = cost_return_on_error_no_add!(
                            cost,
                            self.db.get(&key).map_err(RocksDBError)
                        )
                        .map(|x| x.len() as u32)
                        .unwrap_or(0);
                        cost.storage_loaded_bytes += value_len as u64;
                        let key_len = key.len() as u32;
                        pending_costs.storage_cost.removed_bytes += BasicStorageRemoval(
                            key_len
                                + value_len
                                + key_len.required_space() as u32
                                + value_len.required_space() as u32,
                        );
                    }
                }
                AbstractBatchOperation::DeleteAux { key, cost_info } => {
                    db_batch.delete_cf(cf_aux(&self.db), &key);

                    // Non-atomic freed-size fallback: reads committed state.
                    // See method-level doc comment for rationale.
                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        let value_len = cost_return_on_error_no_add!(
                            cost,
                            self.db.get_cf(cf_aux(&self.db), &key).map_err(RocksDBError)
                        )
                        .map(|x| x.len() as u32)
                        .unwrap_or(0);
                        cost.storage_loaded_bytes += value_len as u64;

                        let key_len = key.len() as u32;
                        pending_costs.storage_cost.removed_bytes += BasicStorageRemoval(
                            key_len
                                + value_len
                                + key_len.required_space() as u32
                                + value_len.required_space() as u32,
                        );
                    }
                }
                AbstractBatchOperation::DeleteRoot { key, cost_info } => {
                    db_batch.delete_cf(cf_roots(&self.db), &key);

                    // Non-atomic freed-size fallback: reads committed state.
                    // See method-level doc comment for rationale.
                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        let value_len = cost_return_on_error_no_add!(
                            cost,
                            self.db
                                .get_cf(cf_roots(&self.db), &key)
                                .map_err(RocksDBError)
                        )
                        .map(|x| x.len() as u32)
                        .unwrap_or(0);
                        cost.storage_loaded_bytes += value_len as u64;

                        let key_len = key.len() as u32;
                        pending_costs.storage_cost.removed_bytes += BasicStorageRemoval(
                            key_len
                                + value_len
                                + key_len.required_space() as u32
                                + value_len.required_space() as u32,
                        );
                    }
                }
                AbstractBatchOperation::DeleteMeta { key, cost_info } => {
                    db_batch.delete_cf(cf_meta(&self.db), &key);

                    // Non-atomic freed-size fallback: reads committed state.
                    // See method-level doc comment for rationale.
                    if let Some(key_value_removed_bytes) = cost_info {
                        cost.seek_count += 1;
                        pending_costs.storage_cost.removed_bytes +=
                            key_value_removed_bytes.combined_removed_bytes();
                    } else {
                        cost.seek_count += 2;
                        let value_len = cost_return_on_error_no_add!(
                            cost,
                            self.db
                                .get_cf(cf_meta(&self.db), &key)
                                .map_err(RocksDBError)
                        )
                        .map(|x| x.len() as u32)
                        .unwrap_or(0);
                        cost.storage_loaded_bytes += value_len as u64;

                        let key_len = key.len() as u32;
                        pending_costs.storage_cost.removed_bytes += BasicStorageRemoval(
                            key_len
                                + value_len
                                + key_len.required_space() as u32
                                + value_len.required_space() as u32,
                        );
                    }
                }
            }
        }
        Ok(pending_costs).wrap_with_cost(cost)
    }

    /// Commits a write batch
    pub fn commit_db_write_batch(
        &self,
        db_batch: WriteBatchWithTransaction<true>,
        pending_costs: OperationCost,
        transaction: Option<&<RocksDbStorage as Storage>::Transaction>,
    ) -> CostResult<(), Error> {
        let result = match transaction {
            None => self.db.write(db_batch).map_err(RocksDBError),
            // Refused with a typed error on a snapshot read transaction.
            Some(transaction) => transaction.rebuild_from_writebatch(&db_batch),
        };

        if result.is_ok() {
            result.wrap_with_cost(pending_costs)
        } else {
            result.wrap_with_cost(OperationCost::default())
        }
    }

    /// Bulk-ingest a single SST file (produced by `rocksdb::SstFileWriter`)
    /// into the named column family.
    ///
    /// Used by snapshot-based bootstrap (e.g. the shielded-pool genesis
    /// snapshot) to load a precomputed subtree's keys without paying the
    /// per-write WAL + fsync cost. The SST must be sorted and its key range
    /// must NOT overlap with any keys already in the CF — otherwise ingest
    /// fails. For genesis-time usage this is satisfied by definition (the
    /// target subtree is empty when this is called).
    ///
    /// Security notes (set by this method, not caller-configurable):
    /// - `allow_global_seqno=false`: rejects ingests that would inject a
    ///   global sequence number, preventing a malicious snapshot from
    ///   reordering its writes relative to subsequent transactional writes.
    /// - `snapshot_consistency=false`: snapshot-based bootstrap runs before
    ///   any reader could hold a RocksDB snapshot of the empty state.
    ///
    /// The ingest happens at the DB level and bypasses any open transaction.
    /// Callers must arrange for txn semantics at a higher layer.
    ///
    /// Gated behind the `unsafe-dump-load` feature — production builds (which
    /// have no need to bulk-load precomputed subtree state) should leave it
    /// off so this API isn't even compiled in.
    #[cfg(feature = "unsafe-dump-load")]
    pub fn ingest_subtree_sst(&self, cf_name: &str, sst_path: &Path) -> Result<(), Error> {
        let cf_handle = self
            .db
            .cf_handle(cf_name)
            .ok_or(Error::StorageError(format!(
                "ingest_subtree_sst: missing CF {cf_name}"
            )))?;
        let mut opts = IngestExternalFileOptions::default();
        opts.set_allow_global_seqno(false);
        opts.set_snapshot_consistency(false);
        self.db
            .ingest_external_file_cf_opts(&cf_handle, &opts, vec![sst_path])
            .map_err(|e| {
                Error::StorageError(format!(
                    "ingest_subtree_sst({cf_name}, {}) failed: {e}",
                    sst_path.display()
                ))
            })
    }

    /// Start a transaction whose reads are **pinned to the committed
    /// state as of this call** when routed through the transactional
    /// storage contexts.
    ///
    /// A plain [`Storage::start_transaction`] transaction reads the
    /// latest committed state on every operation: RocksDB transactions
    /// do not read from a snapshot unless one is requested at creation
    /// and injected into each read's options. This constructor requests
    /// the snapshot; the contexts inject it (`read_options` on the
    /// prefixed transaction contexts), so every get and iterator through
    /// this transaction observes one consistent committed state, however
    /// many operations the caller spreads over it.
    ///
    /// Intended for multi-operation READS that must not tear across a
    /// concurrent commit — e.g. a branched axis read probing and walking
    /// several subtrees. Read-only **enforced**: `set_snapshot` also arms
    /// commit-time conflict detection against the snapshot, so writes
    /// through such a transaction could fail with `Busy` where a plain
    /// transaction's would have committed — instead of leaving that
    /// timing-dependent trap open, every write entry point and `commit`
    /// refuse the transaction with
    /// [`Error::SnapshotReadOnlyTransaction`].
    ///
    /// A snapshot is O(1) to take but pins every post-snapshot RocksDB
    /// version while held: scope the transaction to a single
    /// multi-operation read and drop it promptly. [`Tx::snapshot_age`]
    /// exposes the hold time, and debug builds log loudly when a read
    /// executes on a snapshot held longer than a second.
    pub fn start_snapshot_read_transaction(&self) -> Tx<'_> {
        let mut transaction_options = OptimisticTransactionOptions::default();
        transaction_options.set_snapshot(true);
        Tx::new_snapshot_read(
            self.db
                .transaction_opt(&WriteOptions::default(), &transaction_options),
        )
    }

    /// Clears all data from the database using range deletion on each
    /// column family. Uses a single range tombstone per CF instead of
    /// iterating and deleting every key individually.
    pub fn wipe(&self) -> Result<(), Error> {
        for cf_name in [
            DEFAULT_COLUMN_FAMILY_NAME,
            ROOTS_CF_NAME,
            AUX_CF_NAME,
            META_CF_NAME,
        ] {
            self.wipe_column_family(cf_name)?;
        }
        Ok(())
    }

    fn wipe_column_family(&self, column_family_name: &str) -> Result<(), Error> {
        let cf_handle = self
            .db
            .cf_handle(column_family_name)
            .ok_or(Error::StorageError(
                "failed to get column family handle".to_string(),
            ))?;
        let mut iter = self.db.raw_iterator_cf(&cf_handle);
        iter.seek_to_first();
        let Some(first_key) = iter.key().map(|k| k.to_vec()) else {
            return Ok(()); // CF is already empty
        };
        iter.seek_to_last();
        let Some(last_key) = iter.key() else {
            return Ok(());
        };
        // delete_range_cf is [from, to) exclusive — extend last_key by a zero byte
        // to ensure it's included in the range.
        let mut upper = last_key.to_vec();
        upper.push(0);
        self.db
            .delete_range_cf(&cf_handle, first_key, upper)
            .map_err(|e| {
                Error::StorageError(format!(
                    "wipe_column_family({column_family_name}) delete_range_cf failed: {e}"
                ))
            })?;
        Ok(())
    }
}

impl<'db> Storage<'db> for RocksDbStorage {
    type BatchTransactionalStorageContext = PrefixedRocksDbTransactionContext<'db>;
    type ImmediateStorageContext = PrefixedRocksDbImmediateStorageContext<'db>;
    type Transaction = Tx<'db>;

    fn start_transaction(&'db self) -> Self::Transaction {
        Tx::new_plain(self.db.transaction())
    }

    fn commit_transaction(&self, transaction: Self::Transaction) -> CostResult<(), Error> {
        // All transaction costs were provided on method calls.
        // Note: for OptimisticTransactionDB, commit() performs conflict
        // validation and may return a Busy or TryAgain error if another
        // transaction modified the same keys concurrently. A snapshot
        // read transaction is refused with a typed error.
        transaction.commit().wrap_with_cost(Default::default())
    }

    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Error> {
        transaction.rollback()
    }

    fn flush(&self) -> Result<(), Error> {
        // Flush all column families: `set_atomic_flush(true)` requires it, and a
        // default-only flush leaves the roots/meta/aux memtables unpersisted.
        self.db
            .flush_cfs_opt(
                &[
                    cf_default(&self.db),
                    cf_aux(&self.db),
                    cf_roots(&self.db),
                    cf_meta(&self.db),
                ],
                &FlushOptions::default(),
            )
            .map_err(RocksDBError)
    }

    fn get_transactional_storage_context<'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        batch: Option<&'db StorageBatch>,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::BatchTransactionalStorageContext>
    where
        B: AsRef<[u8]> + 'b,
    {
        Self::build_prefix(path).map(|prefix| {
            PrefixedRocksDbTransactionContext::new(&self.db, transaction, prefix, batch)
        })
    }

    fn get_transactional_storage_context_by_subtree_prefix(
        &'db self,
        prefix: SubtreePrefix,
        batch: Option<&'db StorageBatch>,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::BatchTransactionalStorageContext> {
        PrefixedRocksDbTransactionContext::new(&self.db, transaction, prefix, batch)
            .wrap_with_cost(OperationCost::default())
    }

    fn get_immediate_storage_context<'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::ImmediateStorageContext>
    where
        B: AsRef<[u8]> + 'b,
    {
        Self::build_prefix(path).map(|prefix| {
            PrefixedRocksDbImmediateStorageContext::new(&self.db, transaction, prefix)
        })
    }

    fn get_immediate_storage_context_by_subtree_prefix(
        &'db self,
        prefix: SubtreePrefix,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::ImmediateStorageContext> {
        PrefixedRocksDbImmediateStorageContext::new(&self.db, transaction, prefix)
            .wrap_with_cost(OperationCost::default())
    }

    fn commit_multi_context_batch(
        &self,
        batch: StorageBatch,
        transaction: Option<&'db Self::Transaction>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let (db_batch, pending_costs) =
            cost_return_on_error!(&mut cost, self.build_write_batch(batch));

        self.commit_db_write_batch(db_batch, pending_costs, transaction)
            .add_cost(cost)
    }

    fn get_storage_context_cost<L: WorstKeyLength>(path: &[L]) -> OperationCost {
        if path.is_empty() {
            OperationCost::default()
        } else {
            let body_size = Self::worst_case_body_size(path);
            // the block size of blake3 is 64
            let blocks_num = blake_block_count(body_size) as u32;
            OperationCost::with_hash_node_calls(blocks_num)
        }
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        Checkpoint::new(&self.db)
            .and_then(|x| {
                x.create_checkpoint_with_log_size(path, DEFAULT_LOG_SIZE_FOR_CHECKPOINT_FLUSH)
            })
            .map_err(RocksDBError)
    }
}

/// Get auxiliary data column family
fn cf_aux(storage: &Db) -> &ColumnFamily {
    storage
        .cf_handle(AUX_CF_NAME)
        .expect("aux column family must exist")
}

/// Get trees roots data column family
fn cf_roots(storage: &Db) -> &ColumnFamily {
    storage
        .cf_handle(ROOTS_CF_NAME)
        .expect("roots column family must exist")
}

/// Get metadata column family
fn cf_meta(storage: &Db) -> &ColumnFamily {
    storage
        .cf_handle(META_CF_NAME)
        .expect("meta column family must exist")
}

/// Get the default column family
fn cf_default(storage: &Db) -> &ColumnFamily {
    storage
        .cf_handle(DEFAULT_COLUMN_FAMILY_NAME)
        .expect("default column family must exist")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        rocksdb_storage::{test_utils::TempStorage, RocksDbStorage},
        RawIterator, Storage, StorageContext,
    };
    use grovedb_path::SubtreePath;

    #[test]
    fn flush_persists_all_column_families() {
        let storage = TempStorage::new();
        let db = &storage.db;

        // Populate every column family.
        db.put_cf(cf_default(db), b"kd", b"vd")
            .expect("default put");
        db.put_cf(cf_aux(db), b"ka", b"va").expect("aux put");
        db.put_cf(cf_roots(db), b"kr", b"vr").expect("roots put");
        db.put_cf(cf_meta(db), b"km", b"vm").expect("meta put");

        storage.flush().expect("flush");

        // Every CF's active memtable must now be empty (data flushed to SST).
        for (name, cf) in [
            (DEFAULT_COLUMN_FAMILY_NAME, cf_default(db)),
            (AUX_CF_NAME, cf_aux(db)),
            (ROOTS_CF_NAME, cf_roots(db)),
            (META_CF_NAME, cf_meta(db)),
        ] {
            let entries = db
                .property_int_value_cf(cf, "rocksdb.num-entries-active-mem-table")
                .expect("memtable property read")
                .expect("memtable property present");
            assert_eq!(
                entries, 0,
                "column family '{name}' was not flushed: {entries} entries still in the memtable",
            );
        }
    }

    #[test]
    fn test_build_prefix() {
        let path_a = [b"aa".as_ref(), b"b"];
        let path_b = [b"a".as_ref(), b"ab"];
        assert_ne!(
            RocksDbStorage::build_prefix(path_a.as_ref().into()),
            RocksDbStorage::build_prefix(path_b.as_ref().into()),
        );
        assert_eq!(
            RocksDbStorage::build_prefix(path_a.as_ref().into()),
            RocksDbStorage::build_prefix(path_a.as_ref().into()),
        );
    }

    #[test]
    #[should_panic(expected = "path segment length must not exceed 255 bytes")]
    fn test_build_prefix_rejects_oversized_segment() {
        let oversized_key = vec![0xABu8; 256];
        let path: &[&[u8]] = &[&oversized_key];
        // Previously this would silently truncate the length to 0 (256 as u8 == 0),
        // causing different paths to hash to the same prefix (collision).
        let _ = RocksDbStorage::build_prefix(path.as_ref().into());
    }

    #[test]
    fn secondary_prefix_for_is_deterministic_and_distinct_from_primary() {
        let primary =
            RocksDbStorage::build_prefix([b"foo".as_ref(), b"bar"].as_ref().into()).unwrap();

        let s1 = RocksDbStorage::secondary_prefix_for(&primary, 0).unwrap();
        let s2 = RocksDbStorage::secondary_prefix_for(&primary, 0).unwrap();
        assert_eq!(s1, s2, "secondary prefix derivation must be deterministic");
        assert_ne!(primary, s1, "secondary prefix must differ from its primary");
    }

    #[test]
    fn secondary_prefix_for_distinguishes_different_primaries() {
        let primary_a =
            RocksDbStorage::build_prefix([b"foo".as_ref(), b"bar"].as_ref().into()).unwrap();
        let primary_b =
            RocksDbStorage::build_prefix([b"foo".as_ref(), b"baz"].as_ref().into()).unwrap();

        let s_a = RocksDbStorage::secondary_prefix_for(&primary_a, 0).unwrap();
        let s_b = RocksDbStorage::secondary_prefix_for(&primary_b, 0).unwrap();
        assert_ne!(
            s_a, s_b,
            "different primaries must produce different secondaries"
        );
    }

    #[test]
    fn secondary_prefix_for_handles_empty_path_root_prefix() {
        // A root-level indexed tree has primary_prefix = all-zero default.
        // The derivation must still yield a well-defined 32-byte hash.
        let primary = SubtreePrefix::default();
        let secondary = RocksDbStorage::secondary_prefix_for(&primary, 0).unwrap();
        assert_ne!(secondary, SubtreePrefix::default());
        // And the derivation is stable.
        let secondary_again = RocksDbStorage::secondary_prefix_for(&primary, 0).unwrap();
        assert_eq!(secondary, secondary_again);
    }

    #[test]
    fn secondary_prefix_for_distinguishes_different_axes() {
        // The axis tag byte enters the Blake3 input — different axes for
        // the SAME primary must produce different secondary prefixes so
        // PCPSIT's count / sum / avg secondaries do not collide.
        let primary =
            RocksDbStorage::build_prefix([b"foo".as_ref(), b"bar"].as_ref().into()).unwrap();
        let s_count = RocksDbStorage::secondary_prefix_for(&primary, 0).unwrap();
        let s_sum = RocksDbStorage::secondary_prefix_for(&primary, 1).unwrap();
        let s_avg = RocksDbStorage::secondary_prefix_for(&primary, 2).unwrap();
        assert_ne!(s_count, s_sum, "count and sum axes must be distinct");
        assert_ne!(s_count, s_avg, "count and avg axes must be distinct");
        assert_ne!(s_sum, s_avg, "sum and avg axes must be distinct");
    }

    #[test]
    fn secondary_prefix_for_does_not_collide_with_path_derived_prefix() {
        // The secondary's Blake3 input is `primary || axis_tag` (33 bytes).
        // Path-derived prefixes hash a variable-length `path_body` that
        // always ends with per-segment-length bytes followed by a single
        // segment-count word — never a single tag byte after a 32-byte
        // block. So a collision would require Blake3 output collision
        // (infeasible) AND is structurally impossible to construct via
        // build_prefix. This test sanity-checks that a few path-derived
        // prefixes do not happen to collide with secondaries.
        let primary = RocksDbStorage::build_prefix([b"a".as_ref(), b"b"].as_ref().into()).unwrap();
        let secondary = RocksDbStorage::secondary_prefix_for(&primary, 0).unwrap();

        for path in [
            [b"a".as_ref(), b"b"].as_ref(),
            [b"x".as_ref(), b"y"].as_ref(),
            [b"foo".as_ref(), b"bar", b"baz"].as_ref(),
            [].as_ref(),
        ] {
            let p = RocksDbStorage::build_prefix(path.into()).unwrap();
            assert_ne!(p, secondary);
        }
    }

    #[test]
    fn test_build_prefix_for_root_and_storage_context_cost() {
        struct TestKeyLen(u8);
        impl WorstKeyLength for TestKeyLen {
            fn max_length(&self) -> u8 {
                self.0
            }
        }

        assert_eq!(
            RocksDbStorage::build_prefix(SubtreePath::empty()).unwrap(),
            SubtreePrefix::default()
        );
        assert_eq!(
            RocksDbStorage::get_storage_context_cost::<TestKeyLen>(&[]),
            OperationCost::default()
        );

        let cost = RocksDbStorage::get_storage_context_cost(&[TestKeyLen(70)]);
        assert_eq!(cost.hash_node_calls, 2);
    }

    #[test]
    fn rocksdb_layout_not_affect_iteration_costs() {
        // The test checks that key lengths of seemingly unrelated subtrees
        // won't affect iteration costs. To achieve this we'll have two subtrees
        // and see that nothing nasty will happen if key lengths of the next subtree
        // change.
        let storage = TempStorage::new();

        let path_a = SubtreePath::from(&[b"ayya" as &[u8]]);
        let path_b = SubtreePath::from(&[b"ayyb" as &[u8]]);
        let prefix_a = RocksDbStorage::build_prefix(path_a.clone()).unwrap();
        let prefix_b = RocksDbStorage::build_prefix(path_b.clone()).unwrap();

        // Here by "left" I mean a subtree that goes first in RocksDB.
        let (left_path, right_path) = if prefix_a < prefix_b {
            (path_a, path_b)
        } else {
            (path_b, path_a)
        };

        let batch = StorageBatch::new();
        let transaction = storage.start_transaction();

        let left = storage
            .get_transactional_storage_context(left_path.clone(), Some(&batch), &transaction)
            .unwrap();
        let right = storage
            .get_transactional_storage_context(right_path.clone(), Some(&batch), &transaction)
            .unwrap();

        left.put(b"a", b"a", None, None).unwrap().unwrap();
        left.put(b"b", b"b", None, None).unwrap().unwrap();
        left.put(b"c", b"c", None, None).unwrap().unwrap();

        right.put(b"a", b"a", None, None).unwrap().unwrap();
        right.put(b"b", b"b", None, None).unwrap().unwrap();
        right.put(b"c", b"c", None, None).unwrap().unwrap();

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit batch");

        let batch = StorageBatch::new();
        let left = storage
            .get_transactional_storage_context(left_path.clone(), Some(&batch), &transaction)
            .unwrap();
        let right = storage
            .get_transactional_storage_context(right_path.clone(), Some(&batch), &transaction)
            .unwrap();

        // Iterate over left subtree while right subtree contains 1 byte keys:
        let mut iteration_cost_before = OperationCost::default();
        let mut iter = left.raw_iter();
        iter.seek_to_first().unwrap();
        // Collect sum of `valid` and `key` to check both ways to mess things up
        while iter.valid().unwrap_add_cost(&mut iteration_cost_before)
            && iter
                .key()
                .unwrap_add_cost(&mut iteration_cost_before)
                .is_some()
        {
            iter.next().unwrap_add_cost(&mut iteration_cost_before);
        }

        // Update right subtree to have keys of different size
        right.delete(b"a", None).unwrap().unwrap();
        right.delete(b"b", None).unwrap().unwrap();
        right.delete(b"c", None).unwrap().unwrap();
        right
            .put(b"aaaaaaaaaaaa", b"a", None, None)
            .unwrap()
            .unwrap();
        right
            .put(b"bbbbbbbbbbbb", b"b", None, None)
            .unwrap()
            .unwrap();
        right
            .put(b"cccccccccccc", b"c", None, None)
            .unwrap()
            .unwrap();

        drop(iter);

        storage
            .commit_multi_context_batch(batch, None)
            .unwrap()
            .expect("cannot commit batch");

        let left = storage
            .get_transactional_storage_context(left_path, None, &transaction)
            .unwrap();
        // Iterate over left subtree once again
        let mut iteration_cost_after = OperationCost::default();
        let mut iter = left.raw_iter();
        iter.seek_to_first().unwrap();
        while iter.valid().unwrap_add_cost(&mut iteration_cost_after)
            && iter
                .key()
                .unwrap_add_cost(&mut iteration_cost_after)
                .is_some()
        {
            iter.next().unwrap_add_cost(&mut iteration_cost_after);
        }

        assert_eq!(iteration_cost_before, iteration_cost_after);
    }
}
