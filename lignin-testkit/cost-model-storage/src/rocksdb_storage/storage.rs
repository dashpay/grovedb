//! The storage, its transactions and its write batches, on the cost model.

use std::{
    cell::{Ref, RefCell},
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use grovedb_costs::{cost_return_on_error, CostContext, CostResult, CostsExt, OperationCost};
use grovedb_path::SubtreePath;

use super::{PrefixedRocksDbImmediateStorageContext, PrefixedRocksDbTransactionContext};
use crate::{
    cost_model::{
        rules::{self, Keyspace},
        state::{
            charge_batch, BatchOp, Cursor, Database, Record, StoredKey, Tables, Transaction, Write,
        },
    },
    error::Error,
    flat::{pending_prefix_drops_namespace, PendingPrefixDropRecord, PendingPrefixDrops},
    storage::AbstractBatchOperation,
    worst_case_costs::WorstKeyLength,
    Storage, StorageBatch,
};

/// The 32 bytes that name a subtree.
pub type SubtreePrefix = [u8; 32];

/// The name of the file the rule counts are written to when a storage is
/// dropped, if this variable is set.
const HITS_FILE_VARIABLE: &str = "LIGNIN_COST_MODEL_HITS";

/// The checkpoints of this process: the committed state each was taken of,
/// under the directory that stands for it on disk.
static CHECKPOINTS: Mutex<BTreeMap<PathBuf, Arc<Tables>>> = Mutex::new(BTreeMap::new());

/// What the engine under the storage layer refused. The model has no such
/// engine, so this is only ever a savepoint that was not set or a
/// checkpoint that cannot be taken or opened.
#[derive(Debug)]
pub struct EngineError(String);

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EngineError {}

/// The writes of charged batches, waiting to be handed to a transaction or
/// to the database.
#[derive(Default)]
pub struct WriteBatchWithTransaction<const TRANSACTION: bool> {
    writes: Vec<Write>,
}

/// A started storage transaction.
pub struct Tx<'db> {
    db: &'db Database,
    state: RefCell<Transaction>,
    snapshot_read_since: Option<Instant>,
}

impl<'db> Tx<'db> {
    fn new(db: &'db Database, state: Transaction) -> Self {
        Tx {
            db,
            snapshot_read_since: state.is_pinned().then(Instant::now),
            state: RefCell::new(state),
        }
    }

    /// Whether this transaction was created via
    /// `start_snapshot_read_transaction`: reads are pinned to its
    /// creation-time committed state and writes and commit are refused.
    pub fn is_snapshot_read(&self) -> bool {
        self.snapshot_read_since.is_some()
    }

    /// How long this transaction's snapshot has been held, or `None` for a
    /// plain transaction.
    pub fn snapshot_age(&self) -> Option<Duration> {
        self.snapshot_read_since.map(|since| since.elapsed())
    }

    fn refuse_snapshot_write(&self, operation: &'static str) -> Result<(), Error> {
        if self.is_snapshot_read() {
            Err(Error::SnapshotReadOnlyTransaction(operation))
        } else {
            Ok(())
        }
    }

    pub(super) fn state(&self) -> Ref<'_, Transaction> {
        self.state.borrow()
    }

    pub(super) fn get(&self, keyspace: Keyspace, key: &StoredKey) -> Option<Record> {
        self.state.borrow().get(self.db, keyspace, key)
    }

    pub(super) fn cursor(&self, keyspace: Keyspace) -> Cursor {
        Cursor::new(keyspace, self.state.borrow().base(self.db))
    }

    pub(super) fn write(
        &self,
        operation: &'static str,
        writes: impl IntoIterator<Item = Write>,
    ) -> Result<(), Error> {
        self.refuse_snapshot_write(operation)?;
        self.state.borrow_mut().write(writes);
        Ok(())
    }

    /// Consume and commit the transaction. Refused on a snapshot read
    /// transaction.
    pub fn commit(self) -> Result<(), Error> {
        self.refuse_snapshot_write("commit")?;
        self.state.into_inner().commit(self.db);
        Ok(())
    }

    /// Roll back the transaction's pending writes.
    pub fn rollback(&self) -> Result<(), Error> {
        self.state.borrow_mut().rollback();
        Ok(())
    }

    /// Set a savepoint: a later [`Tx::rollback_to_savepoint`] undoes every
    /// operation in this transaction since this call.
    pub fn set_savepoint(&self) {
        self.state.borrow_mut().set_savepoint();
    }

    /// Undo all operations in this transaction since the most recent
    /// [`Tx::set_savepoint`], popping that savepoint.
    pub fn rollback_to_savepoint(&self) -> Result<(), Error> {
        if self.state.borrow_mut().rollback_to_savepoint() {
            Ok(())
        } else {
            Err(Error::RocksDBError(EngineError(
                "NotFound: no savepoint is set".to_string(),
            )))
        }
    }
}

/// The storage GroveDB opens: the committed state of the model.
#[derive(Default)]
pub struct RocksDbStorage {
    pub(super) db: Database,
}

impl Drop for RocksDbStorage {
    fn drop(&mut self) {
        if let Some(file) = std::env::var_os(HITS_FILE_VARIABLE) {
            let _ = std::fs::write(file, rules::hits_report());
        }
    }
}

fn key_of(stored: &[u8]) -> StoredKey {
    let (prefix, key) = stored.split_at(32);
    let mut subtree = [0; 32];
    subtree.copy_from_slice(prefix);
    StoredKey::new(subtree, key)
}

fn batch_op(op: AbstractBatchOperation) -> BatchOp {
    let put = |keyspace, key: Vec<u8>, value, children_sizes, cost_info| BatchOp::Put {
        keyspace,
        key: key_of(&key),
        value,
        children_sizes,
        cost_info,
    };
    let delete = |keyspace, key: Vec<u8>, cost_info| BatchOp::Delete {
        keyspace,
        key: key_of(&key),
        cost_info,
    };
    match op {
        AbstractBatchOperation::Put {
            key,
            value,
            children_sizes,
            cost_info,
        } => put(Keyspace::Data, key, value, children_sizes, cost_info),
        AbstractBatchOperation::PutAux {
            key,
            value,
            cost_info,
        } => put(Keyspace::Aux, key, value, None, cost_info),
        AbstractBatchOperation::PutRoot {
            key,
            value,
            cost_info,
        } => put(Keyspace::Roots, key, value, None, cost_info),
        AbstractBatchOperation::PutMeta {
            key,
            value,
            cost_info,
        } => put(Keyspace::Meta, key, value, None, cost_info),
        AbstractBatchOperation::Delete { key, cost_info } => delete(Keyspace::Data, key, cost_info),
        AbstractBatchOperation::DeleteAux { key, cost_info } => {
            delete(Keyspace::Aux, key, cost_info)
        }
        AbstractBatchOperation::DeleteRoot { key, cost_info } => {
            delete(Keyspace::Roots, key, cost_info)
        }
        AbstractBatchOperation::DeleteMeta { key, cost_info } => {
            delete(Keyspace::Meta, key, cost_info)
        }
    }
}

impl RocksDbStorage {
    /// An empty storage. The model keeps nothing on disk and does not use
    /// `path`.
    pub fn default_rocksdb_with_path<P: AsRef<Path>>(_path: P) -> Result<Self, Error> {
        Ok(Self::default())
    }

    /// Opens the checkpoint taken at `path` by a storage of this process,
    /// as a storage of its own.
    pub fn checkpoint_rocksdb_with_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let path = path.as_ref();
        let taken = CHECKPOINTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            .cloned()
            .filter(|_| path.is_dir());
        match taken {
            Some(tables) => Ok(RocksDbStorage {
                db: Database::from_committed(tables),
            }),
            None => Err(Error::RocksDBError(EngineError(format!(
                "Invalid argument: {}: no checkpoint of this process",
                path.display()
            )))),
        }
    }

    /// The prefix of the subtree at `path`: the Blake3 hash of its
    /// segments from the last to the first, their number as a machine word,
    /// and the length of each as a byte. Charged by rule P1.
    pub fn build_prefix<B>(path: SubtreePath<B>) -> CostContext<SubtreePrefix>
    where
        B: AsRef<[u8]>,
    {
        let mut body = Vec::new();
        let mut lengths = Vec::new();
        for segment in path.into_reverse_iter() {
            body.extend_from_slice(segment);
            lengths.push(u8::try_from(segment.len()).expect(
                "path segment length must not exceed 255 bytes; this is enforced at insert time \
                 via validate_key_length",
            ));
        }
        let cost = rules::open_by_path(lengths.iter().map(|len| usize::from(*len)));
        if lengths.is_empty() {
            return SubtreePrefix::default().wrap_with_cost(cost);
        }
        body.extend(lengths.len().to_ne_bytes());
        body.extend(lengths);
        SubtreePrefix::from(blake3::hash(&body)).wrap_with_cost(cost)
    }

    /// The prefix of the secondary of an indexed tree: the Blake3 hash of
    /// the primary prefix and the axis tag. Charged by rule P3.
    pub fn secondary_prefix_for(
        primary_prefix: &SubtreePrefix,
        axis_tag: u8,
    ) -> CostContext<SubtreePrefix> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(primary_prefix);
        hasher.update(&[axis_tag]);
        SubtreePrefix::from(hasher.finalize()).wrap_with_cost(rules::secondary_prefix())
    }

    /// Charges a batch and returns its writes with the cost that is owed
    /// once they are written.
    pub fn build_write_batch(
        &self,
        storage_batch: StorageBatch,
    ) -> CostResult<(WriteBatchWithTransaction<true>, OperationCost), Error> {
        let mut db_batch = WriteBatchWithTransaction::<true>::default();
        self.continue_write_batch(&mut db_batch, storage_batch)
            .map_ok(|operation_cost| (db_batch, operation_cost))
    }

    /// Charges a batch, adds its writes to `db_batch` and returns the cost
    /// that is owed once they are written.
    pub fn continue_write_batch(
        &self,
        db_batch: &mut WriteBatchWithTransaction<true>,
        storage_batch: StorageBatch,
    ) -> CostResult<OperationCost, Error> {
        let (on_success, at_once) = charge_batch(
            &self.db,
            storage_batch.into_iter().map(batch_op),
            &mut db_batch.writes,
        );
        on_success.map_err(Error::CostError).wrap_with_cost(at_once)
    }

    /// Hands the writes of charged batches to `transaction`, or commits them
    /// at once when there is none.
    pub fn commit_db_write_batch(
        &self,
        db_batch: WriteBatchWithTransaction<true>,
        pending_costs: OperationCost,
        transaction: Option<&<RocksDbStorage as Storage>::Transaction>,
    ) -> CostResult<(), Error> {
        let result = match transaction {
            None => {
                self.db.write(db_batch.writes);
                Ok(())
            }
            Some(transaction) => transaction.write("batch apply", db_batch.writes),
        };
        let cost = rules::batch_written(pending_costs, result.is_ok());
        result.wrap_with_cost(cost)
    }

    /// The model reads no SST files.
    #[cfg(feature = "unsafe-dump-load")]
    pub fn ingest_subtree_sst(&self, _cf_name: &str, _sst_path: &Path) -> Result<(), Error> {
        Err(Error::StorageError(
            "the cost model reads no SST files".to_string(),
        ))
    }

    /// Start a transaction whose reads are pinned to the committed state as
    /// of this call, and which refuses writes.
    pub fn start_snapshot_read_transaction(&self) -> Tx<'_> {
        Tx::new(&self.db, Transaction::pinned(&self.db))
    }

    /// Removes every record of every keyspace.
    pub fn wipe(&self) -> Result<(), Error> {
        self.db.wipe();
        Ok(())
    }

    /// Removes every record under each of `prefixes`, in all four
    /// keyspaces, at once and outside any transaction.
    pub fn delete_prefix_ranges(&self, prefixes: &[SubtreePrefix]) -> Result<(), Error> {
        if prefixes.contains(&[0xFF; 32]) {
            return Err(Error::StorageError(
                "delete_prefix_ranges: refusing to range-delete the all-0xFF prefix".to_string(),
            ));
        }
        if !prefixes.is_empty() {
            self.db.delete_prefixes(prefixes);
        }
        Ok(())
    }

    /// Lists every committed pending-prefix-drop record, in the order of
    /// their primary prefixes.
    pub fn pending_prefix_drop_records(&self) -> Result<Vec<PendingPrefixDropRecord>, Error> {
        self.db
            .committed()
            .records_of(Keyspace::Meta, *pending_prefix_drops_namespace())
            .map(|(key, record)| PendingPrefixDropRecord::decode(&key.key, &record.bytes()))
            .collect()
    }

    /// Removes one pending-prefix-drop record, at once and outside any
    /// transaction.
    pub fn remove_pending_prefix_drop_record(
        &self,
        primary_prefix: &SubtreePrefix,
    ) -> Result<(), Error> {
        self.db.write([Write {
            keyspace: Keyspace::Meta,
            key: StoredKey::new(*pending_prefix_drops_namespace(), primary_prefix),
            record: None,
        }]);
        Ok(())
    }
}

impl PendingPrefixDrops for RocksDbStorage {
    fn pending_prefix_drop_records(&self) -> Result<Vec<PendingPrefixDropRecord>, Error> {
        RocksDbStorage::pending_prefix_drop_records(self)
    }

    fn delete_prefix_ranges(&self, prefixes: &[SubtreePrefix]) -> Result<(), Error> {
        RocksDbStorage::delete_prefix_ranges(self, prefixes)
    }

    fn remove_pending_prefix_drop_record(
        &self,
        primary_prefix: &SubtreePrefix,
    ) -> Result<(), Error> {
        RocksDbStorage::remove_pending_prefix_drop_record(self, primary_prefix)
    }
}

impl<'db> Storage<'db> for RocksDbStorage {
    type BatchTransactionalStorageContext = PrefixedRocksDbTransactionContext<'db>;
    type ImmediateStorageContext = PrefixedRocksDbImmediateStorageContext<'db>;
    type Transaction = Tx<'db>;

    fn start_transaction(&'db self) -> Self::Transaction {
        Tx::new(&self.db, Transaction::new())
    }

    fn commit_transaction(&self, transaction: Self::Transaction) -> CostResult<(), Error> {
        transaction.commit().wrap_with_cost(Default::default())
    }

    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Error> {
        transaction.rollback()
    }

    fn flush(&self) -> Result<(), Error> {
        Ok(())
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
        Self::build_prefix(path)
            .map(|prefix| PrefixedRocksDbTransactionContext::new(transaction, prefix, batch))
    }

    fn get_transactional_storage_context_by_subtree_prefix(
        &'db self,
        prefix: SubtreePrefix,
        batch: Option<&'db StorageBatch>,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::BatchTransactionalStorageContext> {
        PrefixedRocksDbTransactionContext::new(transaction, prefix, batch)
            .wrap_with_cost(rules::open_by_prefix())
    }

    fn get_immediate_storage_context<'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::ImmediateStorageContext>
    where
        B: AsRef<[u8]> + 'b,
    {
        Self::build_prefix(path)
            .map(|prefix| PrefixedRocksDbImmediateStorageContext::new(transaction, prefix))
    }

    fn get_immediate_storage_context_by_subtree_prefix(
        &'db self,
        prefix: SubtreePrefix,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::ImmediateStorageContext> {
        PrefixedRocksDbImmediateStorageContext::new(transaction, prefix)
            .wrap_with_cost(rules::open_by_prefix())
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
            let (body_len, _) =
                rules::prefix_body_len(path.iter().map(|segment| segment.max_length() as usize));
            OperationCost::with_hash_node_calls(rules::blake3_blocks(body_len))
        }
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        let path = path.as_ref();
        std::fs::create_dir(path).map_err(|error| {
            Error::RocksDBError(EngineError(format!(
                "Invalid argument: {}: {error}",
                path.display()
            )))
        })?;
        CHECKPOINTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_path_buf(), self.db.committed());
        Ok(())
    }
}
