//! The storage, its transactions and its write batches, on the cost model.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
    time::{Duration, Instant},
};

use grovedb_costs::{cost_return_on_error, CostContext, CostResult, CostsExt, OperationCost};
use grovedb_path::SubtreePath;

use super::{PrefixedRocksDbImmediateStorageContext, PrefixedRocksDbTransactionContext};
use crate::{
    cost_model::{
        rules::{self, Keyspace},
        state::{charge_batch, BatchOp, Cursor, Database, Record, StoredKey, Transaction, Write},
    },
    error::Error,
    flat::{pending_prefix_drops_namespace, PendingPrefixDropRecord, PendingPrefixDrops},
    storage::AbstractBatchOperation,
    worst_case_costs::WorstKeyLength,
    Storage, StorageBatch,
};

/// The 32 bytes that name a subtree.
pub type SubtreePrefix = [u8; 32];

/// The directory the rule counts are written to when a storage is dropped,
/// if this variable is set. Every process writes a file of its own there,
/// named by its process id, so that the counts of a test run of several
/// processes can be added up.
const HITS_DIRECTORY_VARIABLE: &str = "LIGNIN_COST_MODEL_HITS";

/// Held while the counts are written, so that the last storage a process
/// drops leaves its final counts.
static HITS_WRITER: Mutex<()> = Mutex::new(());

/// The file that marks a directory as holding a database of the model, and
/// names which.
const MARK_FILE: &str = "COST-MODEL";

/// A database of this process and the directory it was last found in.
struct Opened {
    at: PathBuf,
    db: Arc<Database>,
}

/// The databases of this process by the mark in their directory, so that a
/// storage or a checkpoint opened again finds what was committed there. A
/// database goes when its directory, or the mark in it, has gone.
static DATABASES: Mutex<BTreeMap<String, Opened>> = Mutex::new(BTreeMap::new());

static NEXT_MARK: AtomicU64 = AtomicU64::new(0);

fn refused(path: &Path, what: impl fmt::Display) -> Error {
    Error::RocksDBError(EngineError(format!("{}: {what}", path.display())))
}

fn mark_in(directory: &Path) -> Option<String> {
    std::fs::read_to_string(directory.join(MARK_FILE)).ok()
}

/// The database whose mark `directory` holds, if this process has it.
/// Every call also lets go of the databases whose directory has gone, so a
/// run that opens one directory after another keeps only the live ones.
fn database_at(directory: &Path) -> Option<Arc<Database>> {
    let mark = mark_in(directory);
    let mut databases = DATABASES.lock().unwrap_or_else(PoisonError::into_inner);
    let found = mark
        .and_then(|mark| databases.get_mut(&mark))
        .map(|opened| {
            opened.at = directory.to_path_buf();
            Arc::clone(&opened.db)
        });
    databases.retain(|mark, opened| mark_in(&opened.at).as_deref() == Some(mark));
    found
}

/// Marks `directory` as holding `db`.
fn keep_at(directory: &Path, db: &Arc<Database>) -> Result<(), Error> {
    let mark = format!(
        "{}-{}",
        std::process::id(),
        NEXT_MARK.fetch_add(1, Ordering::Relaxed)
    );
    std::fs::write(directory.join(MARK_FILE), &mark).map_err(|error| refused(directory, error))?;
    DATABASES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            mark,
            Opened {
                at: directory.to_path_buf(),
                db: Arc::clone(db),
            },
        );
    Ok(())
}

/// What the engine under the storage layer refused. The model has no such
/// engine, so this is only ever a savepoint that was not set or a directory
/// that cannot be used.
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
    state: Mutex<Transaction>,
    snapshot_read_since: Option<Instant>,
}

impl<'db> Tx<'db> {
    fn new(db: &'db Database, state: Transaction) -> Self {
        Tx {
            db,
            snapshot_read_since: state.is_pinned().then(Instant::now),
            state: Mutex::new(state),
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

    pub(super) fn state(&self) -> MutexGuard<'_, Transaction> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn get(&self, keyspace: Keyspace, key: &StoredKey) -> Option<Record> {
        self.state().get(self.db, keyspace, key)
    }

    pub(super) fn cursor(&self, keyspace: Keyspace) -> Cursor {
        Cursor::new(keyspace, self.state().base(self.db))
    }

    pub(super) fn write(
        &self,
        operation: &'static str,
        writes: impl IntoIterator<Item = Write>,
    ) -> Result<(), Error> {
        self.refuse_snapshot_write(operation)?;
        self.state().write(writes);
        Ok(())
    }

    /// Consume and commit the transaction. Refused on a snapshot read
    /// transaction.
    pub fn commit(self) -> Result<(), Error> {
        self.refuse_snapshot_write("commit")?;
        self.state
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .commit(self.db);
        Ok(())
    }

    /// Roll back the transaction's pending writes.
    pub fn rollback(&self) -> Result<(), Error> {
        self.state().rollback();
        Ok(())
    }

    /// Set a savepoint: a later [`Tx::rollback_to_savepoint`] undoes every
    /// operation in this transaction since this call.
    pub fn set_savepoint(&self) {
        self.state().set_savepoint();
    }

    /// Undo all operations in this transaction since the most recent
    /// [`Tx::set_savepoint`], popping that savepoint.
    pub fn rollback_to_savepoint(&self) -> Result<(), Error> {
        if self.state().rollback_to_savepoint() {
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
    db: Arc<Database>,
}

impl Drop for RocksDbStorage {
    fn drop(&mut self) {
        if let Some(directory) = std::env::var_os(HITS_DIRECTORY_VARIABLE) {
            let directory = PathBuf::from(directory);
            let process = std::process::id();
            let _writing = HITS_WRITER.lock().unwrap_or_else(PoisonError::into_inner);
            let written = directory.join(format!("{process}.tmp"));
            if std::fs::write(&written, rules::hits_report()).is_ok() {
                let _ = std::fs::rename(&written, directory.join(format!("{process}.tsv")));
            }
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
    /// The storage at `path`. The model keeps its state in memory: `path`
    /// is created as a directory and marked, so that opening the same
    /// directory again in this process, or a checkpoint taken into it,
    /// finds what was committed there.
    pub fn default_rocksdb_with_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let path = path.as_ref();
        std::fs::create_dir_all(path).map_err(|error| refused(path, error))?;
        let db = match database_at(path) {
            Some(db) => db,
            None => {
                let db = Arc::default();
                keep_at(path, &db)?;
                db
            }
        };
        Ok(RocksDbStorage { db })
    }

    /// Opens the database or checkpoint at `path`, which has to be there.
    pub fn checkpoint_rocksdb_with_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let path = path.as_ref();
        database_at(path)
            .map(|db| RocksDbStorage { db })
            .ok_or_else(|| refused(path, "no database of this process"))
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
        let refused = prefixes.iter().position(|prefix| *prefix == [0xFF; 32]);
        let taken = &prefixes[..refused.unwrap_or(prefixes.len())];
        if !taken.is_empty() {
            self.db.delete_prefixes(taken);
        }
        match refused {
            Some(_) => Err(Error::StorageError(
                "delete_prefix_ranges: refusing to range-delete the all-0xFF prefix".to_string(),
            )),
            None => Ok(()),
        }
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
        std::fs::create_dir(path).map_err(|error| refused(path, error))?;
        let checkpoint = Arc::new(Database::from_committed(self.db.committed()));
        keep_at(path, &checkpoint)
    }
}
