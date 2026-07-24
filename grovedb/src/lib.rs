// CI trigger
//! GroveDB is a database that enables cryptographic proofs for complex queries.
//!
//! # Examples
//!
//! ## Open
//! Open an existing instance of GroveDB or create a new one at a given path.
//! ```
//! use grovedb::GroveDb;
//! use tempfile::TempDir;
//!
//! // Specify the path where you want to set up the GroveDB instance
//! let tmp_dir = TempDir::new().unwrap();
//! let path = tmp_dir.path();
//!
//! // Open a new GroveDB at the path
//! let db = GroveDb::open(&path).unwrap();
//! ```
//!
//! ## Basic Operations
//! Insert, Update, Delete and Prove elements.
//! ```
//! use grovedb::{Element, GroveDb};
//! use grovedb_version::version::GroveVersion;
//! use tempfile::TempDir;
//!
//! let grove_version = GroveVersion::latest();
//!
//! // Specify the path where you want to set up the GroveDB instance
//! let tmp_dir = TempDir::new().unwrap();
//! let path = tmp_dir.path();
//!
//! // Open a new GroveDB at the path
//! let db = GroveDb::open(&path).unwrap();
//!
//! let root_path: &[&[u8]] = &[];
//!
//! // Insert new tree to root
//! db.insert(
//!     root_path,
//!     b"tree1",
//!     Element::empty_tree(),
//!     None,
//!     None,
//!     grove_version,
//! )
//! .unwrap()
//! .expect("successful tree insert");
//!
//! // Insert key-value 1 into tree1
//! // key - hello, value - world
//! db.insert(
//!     &[b"tree1"],
//!     b"hello",
//!     Element::new_item(b"world".to_vec()),
//!     None,
//!     None,
//!     grove_version,
//! )
//! .unwrap()
//! .expect("successful key1 insert");
//!
//! // Insert key-value 2 into tree1
//! // key - grovedb, value = rocks
//! db.insert(
//!     &[b"tree1"],
//!     b"grovedb",
//!     Element::new_item(b"rocks".to_vec()),
//!     None,
//!     None,
//!     grove_version,
//! )
//! .unwrap()
//! .expect("successful key2 insert");
//!
//! // Retrieve inserted elements
//! let elem = db
//!     .get(&[b"tree1"], b"hello", None, grove_version)
//!     .unwrap()
//!     .expect("successful get");
//! assert_eq!(elem, Element::new_item(b"world".to_vec()));
//!
//! let elem = db
//!     .get(&[b"tree1"], b"grovedb", None, grove_version)
//!     .unwrap()
//!     .expect("successful get");
//! assert_eq!(elem, Element::new_item(b"rocks".to_vec()));
//!
//! // Update inserted element
//! // for non-tree elements, insertion to an already existing key updates it
//! db.insert(
//!     &[b"tree1"],
//!     b"hello",
//!     Element::new_item(b"WORLD".to_vec()),
//!     None,
//!     None,
//!     grove_version,
//! )
//! .unwrap()
//! .expect("successful update");
//!
//! // Retrieve updated element
//! let elem = db
//!     .get(&[b"tree1"], b"hello", None, grove_version)
//!     .unwrap()
//!     .expect("successful get");
//! assert_eq!(elem, Element::new_item(b"WORLD".to_vec()));
//!
//! // Deletion
//! db.delete(&[b"tree1"], b"hello", None, None, grove_version)
//!     .unwrap()
//!     .expect("successful delete");
//! let elem_result = db.get(&[b"tree1"], b"hello", None, grove_version).unwrap();
//! assert_eq!(elem_result.is_err(), true);
//!
//! // State Root
//! // Get the GroveDB root hash
//! let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();
//! assert_eq!(
//!     hex::encode(root_hash),
//!     "3884be3d197ac49981e54b21ea423351fc4ccdb770aaf7cf40f5e65dc3e2e1aa"
//! );
//! ```
//!
//! For more documentation see our
//! [Architectural Decision Records](https://github.com/dashpay/grovedb/tree/master/adr) or
//! [Tutorial](https://www.grovedb.org/tutorials.html)

// Pre-existing patterns throughout the crate; fix incrementally.
#![deny(missing_docs)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(clippy::result_large_err)]
#![allow(clippy::drop_non_drop)] // Intentional drops to release borrows before re-borrowing

#[cfg(feature = "minimal")]
pub mod batch;
#[cfg(feature = "minimal")]
mod checkpoints;
#[cfg(feature = "grovedbg")]
pub mod debugger;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod element;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod error;
#[cfg(feature = "estimated_costs")]
mod estimated_costs;
#[cfg(feature = "minimal")]
#[allow(dead_code)] // WIP module, will be used in future batch rework
mod merk_cache;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod operations;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod query;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod query_result_type;
#[cfg(feature = "minimal")]
#[allow(dead_code)] // WIP module, will be used in future batch rework
pub mod reference_path;
#[cfg(feature = "minimal")]
/// State replication and synchronization support.
pub mod replication;
#[cfg(all(test, feature = "minimal"))]
mod tests;
#[cfg(feature = "minimal")]
mod util;
#[cfg(feature = "minimal")]
mod visualize;

#[cfg(feature = "grovedbg")]
use std::sync::Arc;
#[cfg(feature = "minimal")]
use std::{collections::HashMap, option::Option::None, path::Path};

#[cfg(feature = "grovedbg")]
use debugger::start_visualizer;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use element::aggregate_sum_query::{AggregateSumQueryOptions, AggregateSumQueryResult};
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use element::Element;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use element::ElementFlags;
#[cfg(feature = "minimal")]
use grovedb_costs::cost_return_on_error_into;
#[cfg(feature = "minimal")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_merk::calculate_max_tree_depth_from_count;
#[cfg(feature = "minimal")]
use grovedb_merk::element::{
    costs::ElementCostExtensions, decode::ElementDecodeExtensions,
    get::ElementFetchFromStorageExtensions, insert::ElementInsertToStorageExtensions,
    reconstruct::ElementReconstructExtensions, tree_type::ElementTreeTypeExtensions, ElementExt,
};
#[cfg(feature = "estimated_costs")]
pub use grovedb_merk::estimated_costs::{
    average_case_costs::{
        EstimatedLayerCount, EstimatedLayerInformation, EstimatedLayerSizes, EstimatedSumTrees,
    },
    worst_case_costs::WorstCaseLayerInformation,
};
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_merk::proofs::query::query_item::QueryItem;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_merk::proofs::query::SubqueryBranch;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_merk::proofs::query::VerifyOptions;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_merk::proofs::Query;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_merk::proofs::{
    encoding::Decoder as MerkProofDecoder, Node as MerkProofNode, Op as MerkProofOp,
};
#[cfg(feature = "minimal")]
use grovedb_merk::tree::kv::ValueDefinedCostType;
#[cfg(feature = "minimal")]
pub use grovedb_merk::tree::AggregateData;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grovedb_merk::tree::TreeFeatureType;
#[cfg(feature = "minimal")]
pub use grovedb_merk::tree_type::{MaybeTree, TreeType};
#[cfg(feature = "minimal")]
use grovedb_merk::{
    self,
    tree::{combine_hash, combine_hash_three, value_hash},
    BatchEntry, CryptoHash, KVIterator, Merk,
};
#[cfg(feature = "minimal")]
use grovedb_path::SubtreePath;
#[cfg(feature = "minimal")]
use grovedb_storage::rocksdb_storage::PrefixedRocksDbImmediateStorageContext;
#[cfg(feature = "minimal")]
use grovedb_storage::rocksdb_storage::RocksDbStorage;
#[cfg(feature = "minimal")]
use grovedb_storage::{rocksdb_storage::PrefixedRocksDbTransactionContext, StorageBatch};
#[cfg(feature = "minimal")]
use grovedb_storage::{Storage, StorageContext};
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;
#[cfg(feature = "minimal")]
use grovedb_visualize::DebugByteVectors;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use query::{
    aggregate_sum_path_query::AggregateSumPathQuery, GroveBranchQueryResult, GroveTrunkQueryResult,
    LeafInfo, PathBranchChunkQuery, PathQuery, PathTrunkChunkQuery, SizedQuery,
};
#[cfg(feature = "minimal")]
use reference_path::path_from_reference_path_type;
#[cfg(feature = "grovedbg")]
use tokio::net::ToSocketAddrs;
#[cfg(feature = "minimal")]
use util::{compat, TxRef};

#[cfg(any(feature = "minimal", feature = "verify"))]
pub use crate::error::Error;
#[cfg(feature = "minimal")]
use crate::operations::proof::util::hex_to_ascii;
#[cfg(feature = "minimal")]
use crate::Error::MerkError;

#[cfg(feature = "minimal")]
type Hash = [u8; 32];

/// GroveDb is a hierarchical authenticated data structure database.
///
/// # Concurrency and Transaction Safety
///
/// `GroveDb` is `Send + Sync` because the underlying RocksDB
/// `OptimisticTransactionDB` is thread-safe at the storage level. However,
/// **GroveDb is designed for single-writer access**. Callers must ensure that
/// at most one write transaction is active at any given time.
///
/// While RocksDB's optimistic transaction mechanism will detect conflicting
/// concurrent writes and fail one transaction at commit time (returning a
/// `Busy` or `TryAgain` error), GroveDb builds in-memory Merk tree state
/// (hashes, balancing, root propagation) during the transaction that cannot
/// be cheaply rolled back. A commit failure therefore requires the caller to
/// discard all in-memory state derived from that transaction and retry the
/// entire operation from scratch.
///
/// Concurrent **reads** (queries, proofs) are safe alongside a single writer.
///
/// In Dash Platform, the primary consumer of GroveDb, this constraint is
/// naturally satisfied because block processing (state transitions) is
/// sequential.
pub struct GroveDb {
    #[cfg(feature = "minimal")]
    db: RocksDbStorage,
}

#[cfg(feature = "minimal")]
pub(crate) type SubtreePrefix = [u8; blake3::OUT_LEN];

/// Transaction
#[cfg(feature = "minimal")]
pub type Transaction<'db> = <RocksDbStorage as Storage<'db>>::Transaction;
/// TransactionArg
#[cfg(feature = "minimal")]
pub type TransactionArg<'db, 'a> = Option<&'a Transaction<'db>>;

/// Type alias for the return type of the `verify_merk_and_submerks` and
/// `verify_grovedb` functions. It represents a mapping of paths (as vectors of
/// vectors of bytes) to a tuple of three cryptographic hashes: the root hash,
/// the combined value hash, and the expected value hash.
#[cfg(feature = "minimal")]
type VerificationIssues = HashMap<Vec<Vec<u8>>, (CryptoHash, CryptoHash, CryptoHash)>;

/// Type alias for the return type of the `open_merk_for_replication` function.
/// It represents a tuple containing:
/// - A `Merk` instance with a prefixed RocksDB immediate storage context.
/// - An optional `root_key`, represented as a vector of bytes.
/// - A boolean indicating whether the Merk is a sum tree.
#[cfg(feature = "minimal")]
type OpenedMerkForReplication<'tx> = (
    Merk<PrefixedRocksDbImmediateStorageContext<'tx>>,
    Option<Vec<u8>>,
    TreeType,
);

#[cfg(feature = "minimal")]
impl GroveDb {
    /// Opens a given path
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = RocksDbStorage::default_rocksdb_with_path(path)?;
        Ok(GroveDb { db })
    }

    /// Open a GroveDB and run `verify_grovedb` before returning,
    /// surfacing any integrity issues — including the H1-A chain and
    /// primary↔secondary content-consistency checks that
    /// `verify_grovedb` performs for each `CountIndexedTree` /
    /// `ProvableCountIndexedTree` element.
    ///
    /// Use this when opening a database that may have been written by
    /// untrusted code, recovered after a crash, or that an external
    /// system claims is consistent.
    ///
    /// Returns `Err(CorruptedData)` if any integrity issues were
    /// found, with the issue keys embedded in the error message for
    /// diagnostic. Returns the opened DB otherwise.
    ///
    /// **Cost**: a full `verify_grovedb` traversal of every subtree —
    /// including each cidx primary's primary AND secondary Merk. Not
    /// a no-op even when the DB contains no cidx elements; budget
    /// accordingly. For an open path that bypasses the integrity
    /// check, use [`open`] and call `verify_grovedb` selectively.
    ///
    /// [`open`]: GroveDb::open
    pub fn open_with_cidx_integrity_check<P: AsRef<Path>>(
        path: P,
        grove_version: &GroveVersion,
    ) -> Result<Self, Error> {
        let db = Self::open(path)?;
        let issues = db.verify_grovedb(None, false, true, grove_version)?;
        if !issues.is_empty() {
            // verify_grovedb returns ALL integrity issues it found
            // (cidx H1-A chain mismatches, cidx content-drift
            // sentinels, AND any non-cidx issues — value-hash
            // mismatches, dangling references, etc.). Surface them
            // all in the error rather than filtering; the caller may
            // want to know about non-cidx issues too.
            return Err(Error::CorruptedData(format!(
                "integrity check on open found {} issue(s): {:?}",
                issues.len(),
                issues.keys().collect::<Vec<_>>()
            )));
        }
        Ok(db)
    }

    /// Starts a visualizer server for the GroveDB instance.
    #[cfg(feature = "grovedbg")]
    pub fn start_visualizer<A>(self: &Arc<Self>, addr: A)
    where
        A: ToSocketAddrs + Send + 'static,
    {
        let weak = Arc::downgrade(self);
        start_visualizer(weak, addr);
    }

    /// Uses raw iter to delete GroveDB key values pairs from rocksdb
    pub fn wipe(&self) -> Result<(), Error> {
        self.db.wipe()?;
        Ok(())
    }

    /// Reborrow the underlying [`grovedb_storage::rocksdb_storage::RocksDbStorage`]
    /// for callers that need to use the public [`grovedb_storage::Storage`]
    /// trait directly — notably to open a [`grovedb_storage::StorageContext`]
    /// at a path for raw iteration or low-level reads.
    ///
    /// This is intended for snapshot/replication tooling that needs to walk
    /// a subtree's raw RocksDB state without going through GroveDb's typed
    /// element API. Normal callers should NOT use this — go through GroveDb's
    /// typed operations (`insert`, `get`, `commitment_tree_*`) instead.
    ///
    /// Stability: this is an escape hatch. The exact `RocksDbStorage` shape
    /// is subject to change as grovedb's internals evolve.
    ///
    /// Gated behind the `unsafe-dump-load` feature — production builds should
    /// leave it off so this escape hatch isn't even compiled in.
    #[cfg(feature = "unsafe-dump-load")]
    pub fn raw_storage(&self) -> &grovedb_storage::rocksdb_storage::RocksDbStorage {
        &self.db
    }

    /// Bulk-ingest a single SST file (produced by `rocksdb::SstFileWriter`)
    /// into the named column family of the underlying RocksDB.
    ///
    /// Delegates to
    /// [`grovedb_storage::rocksdb_storage::RocksDbStorage::ingest_subtree_sst`].
    /// Intended for snapshot-based bootstrap of a single subtree's storage
    /// state — see that method's docs for safety contract.
    ///
    /// CF name for ordinary data storage is the default CF
    /// (`rocksdb::DEFAULT_COLUMN_FAMILY_NAME`). Aux/roots/meta CFs are also
    /// valid targets if a snapshot tool happens to cover them.
    ///
    /// This call bypasses any open transaction. The caller is responsible for
    /// transaction semantics at a higher layer (e.g. only call when the
    /// destination subtree is known empty, and rely on InitChain
    /// abort = wipe-and-restart for failure recovery).
    ///
    /// Gated behind the `unsafe-dump-load` feature.
    #[cfg(feature = "unsafe-dump-load")]
    pub fn ingest_subtree_sst(
        &self,
        cf_name: &str,
        sst_path: &std::path::Path,
    ) -> Result<(), Error> {
        self.db.ingest_subtree_sst(cf_name, sst_path)?;
        Ok(())
    }

    /// Opens the transactional Merk at the given path. Returns CostResult.
    fn open_transactional_merk_at_path<'db, 'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        tx: &'db Transaction,
        batch: Option<&'db StorageBatch>,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        struct Compat;

        impl compat::OpenMerkErrorsCompat for Compat {
            fn parent_key_not_found<B: AsRef<[u8]>>(
                e: Error,
                parent_path: SubtreePath<B>,
                parent_key: &[u8],
            ) -> Error {
                Error::InvalidParentLayerPath(format!(
                    "could not get key {} for parent {:?} of subtree: {}",
                    hex::encode(parent_key),
                    DebugByteVectors(parent_path.to_vec()),
                    e
                ))
            }

            fn open_base_error() -> Error {
                Error::CorruptedData("cannot open a the root subtree".to_owned())
            }

            fn parent_must_be_tree() -> Error {
                Error::CorruptedData("cannot open a subtree with given root key".to_owned())
            }
        }

        compat::open_merk::<_, Compat>(&self.db, path, tx, batch, grove_version)
    }

    fn open_transactional_merk_by_prefix<'db>(
        &'db self,
        prefix: SubtreePrefix,
        root_key: Option<Vec<u8>>,
        tree_type: TreeType,
        tx: &'db Transaction,
        batch: Option<&'db StorageBatch>,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error> {
        let mut cost = OperationCost::default();
        let storage = self
            .db
            .get_transactional_storage_context_by_subtree_prefix(prefix, batch, tx)
            .unwrap_add_cost(&mut cost);
        if root_key.is_some() {
            Merk::open_layered_with_root_key(
                storage,
                root_key,
                tree_type,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "cannot open a subtree by prefix with given root key: {e}"
                ))
            })
            .add_cost(cost)
        } else {
            Merk::open_base(
                storage,
                TreeType::NormalTree,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| Error::CorruptedData(format!("cannot open a root subtree by prefix: {e}")))
            .add_cost(cost)
        }
    }

    /// Opens a Merk at given path for with direct write access. Intended for
    /// replication purposes.
    fn open_merk_for_replication<'tx, 'db: 'tx, 'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        tx: &'tx Transaction<'db>,
        grove_version: &GroveVersion,
    ) -> Result<OpenedMerkForReplication<'tx>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();

        let storage = self
            .db
            .get_immediate_storage_context(path.clone(), tx)
            .unwrap_add_cost(&mut cost);
        if let Some((parent_path, parent_key)) = path.derive_parent() {
            let parent_storage = self
                .db
                .get_immediate_storage_context(parent_path.clone(), tx)
                .unwrap_add_cost(&mut cost);
            let element = Element::get_from_storage(&parent_storage, parent_key, grove_version)
                .map_err(|e| {
                    Error::InvalidParentLayerPath(format!(
                        "could not get key {} for parent {:?} of subtree: {}",
                        hex::encode(parent_key),
                        DebugByteVectors(parent_path.to_vec()),
                        e
                    ))
                })
                .unwrap()?;
            if let Some((root_key, tree_type)) = element.root_key_and_tree_type_owned() {
                Ok((
                    Merk::open_layered_with_root_key(
                        storage,
                        root_key.clone(),
                        tree_type,
                        Some(&Element::value_defined_cost_for_serialized_value),
                        grove_version,
                    )
                    .map_err(|e| {
                        Error::CorruptedData(format!(
                            "cannot open a subtree with given root key: {e}"
                        ))
                    })
                    .unwrap()?,
                    root_key,
                    tree_type,
                ))
            } else {
                Err(Error::CorruptedPath(
                    "cannot open a subtree as parent exists but is not a tree".to_string(),
                ))
            }
        } else {
            Ok((
                Merk::open_base(
                    storage,
                    TreeType::NormalTree,
                    None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                    grove_version,
                )
                .map_err(|e| Error::CorruptedData(format!("cannot open the root subtree: {e}")))
                .unwrap()?,
                None,
                TreeType::NormalTree,
            ))
        }
    }

    /// Returns root key of GroveDb.
    /// Will be `None` if GroveDb is empty.
    pub fn root_key(
        &self,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Vec<u8>>, Error> {
        let mut cost = OperationCost {
            ..Default::default()
        };

        let tx = TxRef::new(&self.db, transaction);

        let root_merk =
            cost_return_on_error!(&mut cost, self.open_root_merk(tx.as_ref(), grove_version));

        let root_key = root_merk.root_key();
        Ok(root_key).wrap_with_cost(cost)
    }

    /// Returns root hash of GroveDb.
    pub fn root_hash(
        &self,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Hash, Error> {
        let mut cost = OperationCost {
            ..Default::default()
        };

        let tx = TxRef::new(&self.db, transaction);

        let root_merk =
            cost_return_on_error!(&mut cost, self.open_root_merk(tx.as_ref(), grove_version));

        root_merk.root_hash().map(Ok).add_cost(cost)
    }

    fn open_root_merk<'tx, 'db>(
        &'db self,
        tx: &'tx Transaction<'db>,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'tx>>, Error> {
        self.db
            .get_transactional_storage_context(SubtreePath::empty(), None, tx)
            .flat_map(|storage_ctx| {
                grovedb_merk::Merk::open_base(
                    storage_ctx,
                    TreeType::NormalTree,
                    Some(Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map(|merk_res| {
                    merk_res.map_err(|e| {
                        crate::Error::CorruptedData(format!("cannot open a subtree: {e}"))
                    })
                })
            })
    }

    /// Method to propagate updated subtree key changes one level up inside a
    /// transaction
    fn propagate_changes_with_batch_transaction<'b, B: AsRef<[u8]>>(
        &self,
        storage_batch: &StorageBatch,
        mut merk_cache: HashMap<SubtreePath<'b, B>, Merk<PrefixedRocksDbTransactionContext>>,
        path: &SubtreePath<'b, B>,
        transaction: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut child_tree = cost_return_on_error_no_add!(
            cost,
            merk_cache.remove(path).ok_or(Error::CorruptedCodeExecution(
                "Merk Cache should always contain the last path",
            ))
        );

        let mut current_path = path.clone();

        while let Some((parent_path, parent_key)) = current_path.derive_parent() {
            let mut parent_tree = cost_return_on_error!(
                &mut cost,
                self.open_batch_transactional_merk_at_path(
                    storage_batch,
                    parent_path.clone(),
                    transaction,
                    false,
                    grove_version,
                )
            );
            let (root_hash, root_key, aggregate_data) = cost_return_on_error!(
                &mut cost,
                child_tree
                    .root_hash_key_and_aggregate_data()
                    .map_err(Error::MerkError)
            );
            cost_return_on_error!(
                &mut cost,
                Self::update_tree_item_preserve_flag(
                    &mut parent_tree,
                    parent_key,
                    root_key,
                    root_hash,
                    aggregate_data,
                    grove_version,
                )
            );
            child_tree = parent_tree;
            current_path = parent_path;
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Method to propagate updated subtree key changes one level up inside a
    /// transaction
    fn propagate_changes_with_transaction<'b, B: AsRef<[u8]>>(
        &self,
        merk_cache: HashMap<SubtreePath<'b, B>, Merk<PrefixedRocksDbTransactionContext>>,
        path: SubtreePath<'b, B>,
        transaction: &Transaction,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        self.propagate_changes_with_transaction_with_initial_deferred(
            merk_cache,
            path,
            None,
            transaction,
            batch,
            grove_version,
        )
    }

    pub(crate) fn propagate_changes_with_transaction_with_initial_deferred<'b, B: AsRef<[u8]>>(
        &self,
        mut merk_cache: HashMap<SubtreePath<'b, B>, Merk<PrefixedRocksDbTransactionContext>>,
        path: SubtreePath<'b, B>,
        initial_deferred_secondary: Option<(Hash, Option<Vec<u8>>)>,
        transaction: &Transaction,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        use grovedb_merk::element::{
            get::ElementFetchFromStorageExtensions, insert::ElementInsertToStorageExtensions,
            reconstruct::ElementReconstructExtensions,
        };

        use crate::operations::indexed_tree::mirror_to_secondary;

        let mut cost = OperationCost::default();

        let mut child_tree = cost_return_on_error_no_add!(
            cost,
            merk_cache
                .remove(&path)
                .ok_or(Error::CorruptedCodeExecution(
                    "Merk Cache should always contain the last path",
                ))
        );

        // NOTE: generic leaf mutations against an indexed-tree primary are
        // rejected by the callers that perform them
        // (`reject_generic_write_into_indexed_primary`, called from the
        // generic insert/delete paths), NOT here. This function is shared
        // with callers whose mutation provably cannot change a child's
        // ordering value — the typed non-Merk append APIs (`mmr_tree_append`,
        // `commitment_tree_insert`, `bulk_append`, `dense_tree_insert`, whose
        // element count is a constant `1` on both sides of the append, matching
        // `GroveOp::can_mutate_child_count == false` on the batch path) and the
        // dedicated indexed-tree APIs, which mirror the secondary themselves.
        // Guarding here rejected all of those too.

        let mut current_path = path.clone();

        // Carries the secondary's new (root_hash, root_key) when the
        // previous iteration's `parent_tree` was a CountIndexedTree
        // primary and its corresponding secondary needs to be folded into
        // the next iteration's CountIndexedTree element via the H1-A
        // three-input combine. Cleared once consumed.
        //
        // Initial value is supplied by callers like the dedicated
        // count-indexed insert/delete APIs which have already mirrored to
        // the secondary at the boundary (so propagate doesn't have to
        // re-open the secondary by stale root_key).
        let mut deferred_secondary: Option<(Hash, Option<Vec<u8>>)> = initial_deferred_secondary;

        while let Some((parent_path, parent_key)) = current_path.derive_parent() {
            let mut parent_tree: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(
                    parent_path.clone(),
                    transaction,
                    Some(batch),
                    grove_version
                )
            );
            let (root_hash, root_key, aggregate_data) = cost_return_on_error!(
                &mut cost,
                child_tree
                    .root_hash_key_and_aggregate_data()
                    .map_err(Error::MerkError)
            );

            let parent_is_cidx_primary = parent_tree.tree_type.is_count_indexed_primary();

            // Snapshot the old count_value of the element at parent_key
            // BEFORE we mutate parent_tree. We need it later to compute
            // the count delta for secondary mirroring.
            let old_count_in_parent = if parent_is_cidx_primary {
                let old_element = cost_return_on_error!(
                    &mut cost,
                    Element::get(&parent_tree, parent_key, true, grove_version)
                        .map_err(Error::MerkError)
                );
                Some(old_element.count_value_or_default())
            } else {
                None
            };

            // Whether the element at parent_key in parent_tree is a
            // CountIndexedTree element. True if either:
            //   - we have a `deferred_secondary` from the previous
            //     iteration (parent's child was a CountIndexedTree
            //     primary, so its parent-side element is the
            //     CountIndexedTree), OR
            //   - child_tree itself IS a CountIndexedTree primary, even
            //     without a `deferred_secondary` from below (this happens
            //     on the very first iteration after a direct write into
            //     the cidx primary that bypassed the dedicated insert
            //     API; we read the secondary state from disk).
            let child_is_cidx_primary = child_tree.tree_type.is_count_indexed_primary();
            if deferred_secondary.is_some() || child_is_cidx_primary {
                let cidx_element = cost_return_on_error!(
                    &mut cost,
                    Element::get(&parent_tree, parent_key, true, grove_version)
                        .map_err(Error::MerkError)
                );
                let (sec_hash, sec_key) = if let Some(s) = deferred_secondary.take() {
                    s
                } else {
                    // Read secondary's current state from on-disk root.
                    let secondary_root_key_before = match cidx_element.underlying() {
                        Element::ProvableCountIndexedTree(_, secondary, ..) => secondary.clone(),
                        _ => {
                            return Err(Error::CorruptedData(
                                "expected ProvableCountIndexedTree element when child_tree is a \
                                 count-indexed primary"
                                    .to_string(),
                            ))
                            .wrap_with_cost(cost);
                        }
                    };
                    let secondary_merk = cost_return_on_error!(
                        &mut cost,
                        self.open_indexed_secondary_at_path(
                            current_path.clone(),
                            grovedb_element::indexed::IndexAxis::Count,
                            secondary_root_key_before,
                            transaction,
                            Some(batch),
                            grove_version,
                        )
                    );
                    let (sh, sk, _) = cost_return_on_error!(
                        &mut cost,
                        secondary_merk
                            .root_hash_key_and_aggregate_data()
                            .map_err(Error::MerkError)
                    );
                    (sh, sk)
                };
                let reconstructed = cost_return_on_error_no_add!(
                    cost,
                    cidx_element
                        .reconstruct_with_two_root_keys(root_key, sec_key, aggregate_data)
                        .ok_or(Error::CorruptedCodeExecution(
                            "reconstruct_with_two_root_keys returned None for a \
                             CountIndexedTree element during propagation"
                        ))
                );
                cost_return_on_error!(
                    &mut cost,
                    reconstructed
                        .insert_count_indexed_subtree(
                            &mut parent_tree,
                            parent_key,
                            root_hash,
                            sec_hash,
                            None,
                            grove_version,
                        )
                        .map_err(Error::MerkError)
                );
            } else {
                cost_return_on_error!(
                    &mut cost,
                    Self::update_tree_item_preserve_flag(
                        &mut parent_tree,
                        parent_key,
                        root_key,
                        root_hash,
                        aggregate_data,
                        grove_version,
                    )
                );
            }

            // If parent_tree IS a CountIndexedTree primary, mirror the
            // count delta into its secondary and stage the new secondary
            // state for the NEXT iteration (which will reach the element
            // that holds primary_root_key and secondary_root_key).
            if let Some(old_count) = old_count_in_parent {
                // Take the new ordering key from the element we just wrote,
                // NOT from `aggregate_data`. The batch path reads both sides
                // of the delta with `Element::count_value_or_default`
                // (`batch::indexed_tree::capture_cidx_pre_state` /
                // `apply_cidx_secondary_mirror_post_apply`), and the two
                // differ for children whose element carries no count
                // aggregate — a plain `Tree` or `SumTree` child defaults to
                // 1 via the element, but `AggregateData::as_count_u64`
                // reports 0. Deriving it from the aggregate here made
                // `db.insert` and `apply_batch` place the same child in
                // different secondary buckets, committing different root
                // hashes for byte-identical writes.
                let new_element_in_parent = cost_return_on_error!(
                    &mut cost,
                    Element::get(&parent_tree, parent_key, true, grove_version)
                        .map_err(Error::MerkError)
                );
                let new_count = new_element_in_parent.count_value_or_default();

                // Find secondary_root_key: it lives in the
                // CountIndexedTree element which is at grandparent[cidx_key].
                let (grandparent_path, cidx_key) = match parent_path.derive_parent() {
                    Some(p) => p,
                    None => {
                        return Err(Error::CorruptedCodeExecution(
                            "CountIndexedTree primary requires a grandparent for the \
                             CountIndexedTree element",
                        ))
                        .wrap_with_cost(cost);
                    }
                };
                let grandparent_merk = cost_return_on_error!(
                    &mut cost,
                    self.open_transactional_merk_at_path(
                        grandparent_path,
                        transaction,
                        Some(batch),
                        grove_version,
                    )
                );
                let cidx_element = cost_return_on_error!(
                    &mut cost,
                    Element::get(&grandparent_merk, cidx_key, true, grove_version)
                        .map_err(Error::MerkError)
                );
                let secondary_root_key_before = match cidx_element.underlying() {
                    Element::ProvableCountIndexedTree(_, secondary, ..) => secondary.clone(),
                    _ => {
                        return Err(Error::CorruptedData(
                            "expected ProvableCountIndexedTree element in grandparent during \
                             cascading aggregation"
                                .to_string(),
                        ))
                        .wrap_with_cost(cost);
                    }
                };

                let mut secondary_merk = cost_return_on_error!(
                    &mut cost,
                    self.open_indexed_secondary_at_path(
                        parent_path.clone(),
                        grovedb_element::indexed::IndexAxis::Count,
                        secondary_root_key_before,
                        transaction,
                        Some(batch),
                        grove_version,
                    )
                );

                cost_return_on_error!(
                    &mut cost,
                    mirror_to_secondary(
                        &mut secondary_merk,
                        parent_key,
                        Some(old_count),
                        new_count,
                        grove_version,
                    )
                );

                let (sec_hash, sec_key, _) = cost_return_on_error!(
                    &mut cost,
                    secondary_merk
                        .root_hash_key_and_aggregate_data()
                        .map_err(Error::MerkError)
                );
                deferred_secondary = Some((sec_hash, sec_key));
            }

            child_tree = parent_tree;
            current_path = parent_path;
        }

        if deferred_secondary.is_some() {
            return Err(Error::CorruptedCodeExecution(
                "deferred secondary state was set but never consumed (loop reached the root \
                 before updating the CountIndexedTree element above its primary)",
            ))
            .wrap_with_cost(cost);
        }

        Ok(()).wrap_with_cost(cost)
    }

    /// Updates a tree item and preserves flags. Returns CostResult.
    pub(crate) fn update_tree_item_preserve_flag<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        parent_tree: &mut Merk<S>,
        key: K,
        maybe_root_key: Option<Vec<u8>>,
        root_tree_hash: Hash,
        aggregate_data: AggregateData,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let key_ref = key.as_ref();

        Self::get_element_from_subtree(parent_tree, key_ref, grove_version).flat_map_ok(|element| {
            match element.reconstruct_with_root_key(maybe_root_key, aggregate_data) {
                Some(tree) => tree
                    .insert_subtree(parent_tree, key_ref, root_tree_hash, None, grove_version)
                    .map_err(|e| e.into()),
                None => Err(Error::InvalidPath(
                    "can only propagate on tree items".to_owned(),
                ))
                .wrap_with_cost(Default::default()),
            }
        })
    }

    /// Pushes to batch an operation which updates a tree item and preserves
    /// flags. Returns CostResult.
    pub(crate) fn update_tree_item_preserve_flag_into_batch_operations<
        'db,
        K: AsRef<[u8]>,
        S: StorageContext<'db>,
    >(
        parent_tree: &Merk<S>,
        key: K,
        maybe_root_key: Option<Vec<u8>>,
        root_tree_hash: Hash,
        aggregate_data: AggregateData,
        batch_operations: &mut Vec<BatchEntry<K>>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        Self::get_element_from_subtree(parent_tree, key.as_ref(), grove_version).flat_map_ok(
            |element| match element.reconstruct_with_root_key(maybe_root_key, aggregate_data) {
                Some(tree) => {
                    let merk_feature_type = cost_return_on_error_into!(
                        &mut cost,
                        tree.get_feature_type(parent_tree.tree_type)
                            .wrap_with_cost(OperationCost::default())
                    );
                    tree.insert_subtree_into_batch_operations(
                        key,
                        root_tree_hash,
                        true,
                        batch_operations,
                        merk_feature_type,
                        grove_version,
                    )
                    .map_err(|e| e.into())
                }
                None => Err(Error::InvalidPath(
                    "can only propagate on tree items".to_owned(),
                ))
                .wrap_with_cost(Default::default()),
            },
        )
    }

    /// Pushes to batch an operation that updates a `CountIndexedTree` /
    /// `ProvableCountIndexedTree` element on the parent merk and recomputes
    /// its `value_hash` via H1-A — `combine_hash_three(value_hash(cidx_bytes),
    /// primary_root_hash, secondary_root_hash)`. Preserves the existing
    /// element's flags. Used by the batch path's cidx primary bubble-up.
    pub(crate) fn update_count_indexed_tree_item_preserve_flag_into_batch_operations<
        'db,
        K: AsRef<[u8]>,
        S: StorageContext<'db>,
    >(
        parent_tree: &Merk<S>,
        key: K,
        primary_root_key: Option<Vec<u8>>,
        secondary_root_key: Option<Vec<u8>>,
        primary_aggregate_data: AggregateData,
        primary_root_hash: Hash,
        secondary_root_hash: Hash,
        batch_operations: &mut Vec<BatchEntry<K>>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        Self::get_element_from_subtree(parent_tree, key.as_ref(), grove_version).flat_map_ok(
            |element| match element.reconstruct_with_two_root_keys(
                primary_root_key,
                secondary_root_key,
                primary_aggregate_data,
            ) {
                Some(tree) => {
                    let merk_feature_type = cost_return_on_error_into!(
                        &mut cost,
                        tree.get_feature_type(parent_tree.tree_type)
                            .wrap_with_cost(OperationCost::default())
                    );
                    tree.insert_count_indexed_subtree_into_batch_operations(
                        key,
                        primary_root_hash,
                        secondary_root_hash,
                        true,
                        batch_operations,
                        merk_feature_type,
                        grove_version,
                    )
                    .map_err(|e| e.into())
                }
                None => Err(Error::InvalidPath(
                    "update_count_indexed_tree_item_preserve_flag: existing element is not a \
                     CountIndexedTree / ProvableCountIndexedTree"
                        .to_owned(),
                ))
                .wrap_with_cost(Default::default()),
            },
        )
    }

    /// Get element from subtree. Return CostResult.
    fn get_element_from_subtree<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        subtree: &Merk<S>,
        key: K,
        grove_version: &GroveVersion,
    ) -> CostResult<Element, Error> {
        subtree
            .get(
                key.as_ref(),
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| {
                Error::InvalidPath(format!(
                    "can't find subtree in parent during propagation: {e}"
                ))
            })
            .map_ok(|subtree_opt| {
                subtree_opt.ok_or_else(|| {
                    let key = hex::encode(key.as_ref());
                    Error::PathKeyNotFound(format!(
                        "can't find subtree with key {} in parent during propagation (subtree is \
                         {})",
                        key,
                        if subtree.root_key().is_some() {
                            "not empty"
                        } else {
                            "empty"
                        }
                    ))
                })
            })
            .flatten()
            .map_ok(|element_bytes| {
                Element::deserialize(&element_bytes, grove_version).map_err(|e| {
                    Error::CorruptedData(format!(
                        "failed to deserialize parent during propagation: {e}"
                    ))
                })
            })
            .flatten()
    }

    /// Flush memory table to disk.
    pub fn flush(&self) -> Result<(), Error> {
        Ok(self.db.flush()?)
    }

    /// Starts a new database transaction.
    ///
    /// # Single-Writer Requirement
    ///
    /// Only one write transaction should be active at a time. While the
    /// underlying RocksDB `OptimisticTransactionDB` permits multiple
    /// concurrent transactions, GroveDb does not enforce mutual exclusion
    /// internally. If two write transactions run concurrently and touch
    /// overlapping keys, one will fail at commit time with a RocksDB `Busy`
    /// or `TryAgain` error. In that case, all in-memory Merk state built
    /// during the failed transaction is invalid and must be discarded; the
    /// operation must be retried from the beginning.
    ///
    /// Concurrent read-only operations (e.g., `get`, `query`, `prove`) are
    /// safe to perform alongside a single active write transaction.
    ///
    /// ## Examples:
    /// ```
    /// # use grovedb::{Element, Error, GroveDb};
    /// # use std::convert::TryFrom;
    /// # use tempfile::TempDir;
    /// # use grovedb_path::SubtreePath;
    /// # use grovedb_version::version::GroveVersion;
    /// #
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::option::Option::None;
    /// ///
    ///
    /// const TEST_LEAF: &[u8] = b"test_leaf";
    ///
    /// let grove_version = GroveVersion::latest();
    ///
    /// let tmp_dir = TempDir::new().unwrap();
    /// let mut db = GroveDb::open(tmp_dir.path())?;
    /// db.insert(
    ///     SubtreePath::empty(),
    ///     TEST_LEAF,
    ///     Element::empty_tree(),
    ///     None,
    ///     None,
    ///     grove_version,
    /// )
    /// .unwrap()?;
    ///
    /// let tx = db.start_transaction();
    ///
    /// let subtree_key = b"subtree_key";
    /// db.insert(
    ///     [TEST_LEAF].as_ref(),
    ///     subtree_key,
    ///     Element::empty_tree(),
    ///     None,
    ///     Some(&tx),
    ///     grove_version,
    /// )
    /// .unwrap()?;
    ///
    /// // This action exists only inside the transaction for now
    /// let result = db
    ///     .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
    ///     .unwrap();
    /// assert!(matches!(result, Err(Error::PathKeyNotFound(_))));
    ///
    /// // To access values inside the transaction, transaction needs to be passed to the `db::get`
    /// let result_with_transaction = db
    ///     .get([TEST_LEAF].as_ref(), subtree_key, Some(&tx), grove_version)
    ///     .unwrap()?;
    /// assert_eq!(result_with_transaction, Element::empty_tree());
    ///
    /// // After transaction is committed, the value from it can be accessed normally.
    /// let _ = db.commit_transaction(tx);
    /// let result = db
    ///     .get([TEST_LEAF].as_ref(), subtree_key, None, grove_version)
    ///     .unwrap()?;
    /// assert_eq!(result, Element::empty_tree());
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn start_transaction(&self) -> Transaction<'_> {
        self.db.start_transaction()
    }

    /// Consumes and commits a previously started transaction.
    ///
    /// On success the transaction's writes become visible to subsequent
    /// operations. On failure (e.g., a `Busy` error from an optimistic
    /// concurrency conflict) the transaction is consumed and all in-memory
    /// Merk state derived from it must be discarded.
    ///
    /// For more details on the transaction usage, please check
    /// [`GroveDb::start_transaction`].
    pub fn commit_transaction(&self, transaction: Transaction) -> CostResult<(), Error> {
        self.db.commit_transaction(transaction).map_err(Into::into)
    }

    /// Rolls back a previously started transaction to its initial state.
    ///
    /// After rollback, any in-memory Merk state derived from the transaction
    /// is invalid and must be discarded. The transaction object itself remains
    /// valid and can be reused for new operations.
    ///
    /// For more details on the transaction usage, please check
    /// [`GroveDb::start_transaction`].
    pub fn rollback_transaction(&self, transaction: &Transaction) -> Result<(), Error> {
        Ok(self.db.rollback_transaction(transaction)?)
    }

    /// Method to visualize hash mismatch after verification
    pub fn visualize_verify_grovedb(
        &self,
        transaction: TransactionArg,
        verify_references: bool,
        allow_cache: bool,
        grove_version: &GroveVersion,
    ) -> Result<HashMap<String, (String, String, String)>, Error> {
        Ok(self
            .verify_grovedb(transaction, verify_references, allow_cache, grove_version)?
            .iter()
            .map(|(path, (root_hash, expected, actual))| {
                (
                    path.iter()
                        .map(hex::encode)
                        .collect::<Vec<String>>()
                        .join("/"),
                    (
                        hex::encode(root_hash),
                        hex::encode(expected),
                        hex::encode(actual),
                    ),
                )
            })
            .collect())
    }

    /// Method to check that the value_hash of Element::Tree nodes are computed
    /// correctly.
    pub fn verify_grovedb(
        &self,
        transaction: TransactionArg,
        verify_references: bool,
        allow_cache: bool,
        grove_version: &GroveVersion,
    ) -> Result<VerificationIssues, Error> {
        let tx = TxRef::new(&self.db, transaction);

        let root_merk = self
            .open_transactional_merk_at_path(SubtreePath::empty(), tx.as_ref(), None, grove_version)
            .unwrap()?;
        self.verify_merk_and_submerks_in_transaction(
            root_merk,
            &SubtreePath::empty(),
            None,
            tx.as_ref(),
            verify_references,
            allow_cache,
            grove_version,
        )
    }

    fn verify_merk_and_submerks_in_transaction<'db, B: AsRef<[u8]>, S: StorageContext<'db>>(
        &'db self,
        merk: Merk<S>,
        path: &SubtreePath<B>,
        batch: Option<&'db StorageBatch>,
        transaction: &Transaction,
        verify_references: bool,
        allow_cache: bool,
        grove_version: &GroveVersion,
    ) -> Result<VerificationIssues, Error> {
        let mut all_query = Query::new();
        all_query.insert_all();

        let mut issues = HashMap::new();
        let mut element_iterator = KVIterator::new(merk.storage.raw_iter(), &all_query).unwrap();

        while let Some((key, element_value)) = element_iterator.next_kv().unwrap() {
            // Look through NonCounted: verification dispatches on inner type.
            // The on-disk value bytes are still the wrapper bytes, so the
            // hash checks below operate on those.
            let element = Element::raw_decode(&element_value, grove_version)?.into_underlying();
            match element {
                // CountIndexedTree / ProvableCountIndexedTree integrity:
                // open both child Merks, read their root hashes, and
                // verify the parent's recorded value_hash equals the
                // H1-A three-input combine of
                // (value_hash(cidx_bytes), primary_root, secondary_root).
                // Then recurse into the primary Merk normally; the
                // secondary's contents are auto-mirrored from the
                // primary, so we only check its root-hash contribution
                // here.
                Element::ProvableCountIndexedTree(..) => {
                    let (kv_value, element_value_hash) = merk
                        .get_value_and_value_hash(
                            &key,
                            allow_cache,
                            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                            grove_version,
                        )
                        .unwrap()
                        .map_err(MerkError)?
                        .ok_or(Error::CorruptedData(format!(
                            "expected merk to contain value at key {} for {}",
                            hex_to_ascii(&key),
                            element.type_str()
                        )))?;
                    let new_path = path.derive_owned_with_child(key);
                    let new_path_ref = SubtreePath::from(&new_path);

                    let primary_merk = self
                        .open_transactional_merk_at_path(
                            new_path_ref.clone(),
                            transaction,
                            batch,
                            grove_version,
                        )
                        .unwrap()?;
                    let primary_root_hash = primary_merk.root_hash().unwrap();

                    let secondary_root_key = match element {
                        Element::ProvableCountIndexedTree(_, ref s, ..) => s.clone(),
                        _ => unreachable!("matched cidx variant above"),
                    };
                    let secondary_merk = self
                        .open_indexed_secondary_at_path(
                            new_path_ref.clone(),
                            grovedb_element::indexed::IndexAxis::Count,
                            secondary_root_key,
                            transaction,
                            batch,
                            grove_version,
                        )
                        .unwrap()?;
                    let secondary_root_hash = secondary_merk.root_hash().unwrap();

                    let actual_value_hash = value_hash(&kv_value).unwrap();
                    let combined_value_hash = combine_hash_three(
                        &actual_value_hash,
                        &primary_root_hash,
                        &secondary_root_hash,
                    )
                    .unwrap();

                    if combined_value_hash != element_value_hash {
                        // Use the primary root hash in the issues record;
                        // verifying the secondary separately would
                        // double-count if the same parent had multiple
                        // mismatches in the same scan.
                        issues.insert(
                            new_path.to_vec(),
                            (primary_root_hash, combined_value_hash, element_value_hash),
                        );
                    }

                    // Cidx content-consistency check.
                    //
                    // The H1-A check above verifies *chain* integrity —
                    // the cidx element's recorded value_hash matches
                    // `combine_hash_three(value_hash(bytes),
                    // primary_root, secondary_root)`. That binds the
                    // two Merks' root hashes into the parent's hash,
                    // but it does NOT verify the secondary's contents
                    // match what the primary says they should be.
                    //
                    // A bug like nested-cidx batch bubble-up forgetting
                    // to mirror the count change (caught at audit in
                    // a8bb34fb) leaves the secondary internally
                    // consistent — its root hash is the correct hash
                    // of its on-disk content — but its content is
                    // stale relative to the primary. H1-A passes;
                    // queries return wrong results.
                    //
                    // Walk both Merks here and assert per-entry
                    // consistency: every primary entry with
                    // count_value=c at key=k must correspond to
                    // exactly one secondary entry at
                    // (c.to_be_bytes() ‖ k). Each mismatch is recorded
                    // in `issues` with a sentinel path suffix so the
                    // existing `VerificationIssues` type doesn't need
                    // to change.
                    let mut primary_entries: HashMap<Vec<u8>, u64> = HashMap::new();
                    let mut content_iter =
                        KVIterator::new(primary_merk.storage.raw_iter(), &all_query).unwrap();
                    while let Some((p_key, p_value)) = content_iter.next_kv().unwrap() {
                        // Cidx primary keys must be ≤ 247 bytes so the
                        // derived secondary key (count_be ‖ key) fits
                        // under Merk's < 256-byte invariant. Oversize
                        // keys can only enter the primary via a code
                        // path that bypassed the cidx-key length check
                        // (legacy data, corruption, external storage
                        // injection). Flag explicitly via a sentinel
                        // path so the cause is visible.
                        if p_key.len() > crate::operations::indexed_tree::MAX_CIDX_ITEM_KEY_LEN {
                            let mut p = new_path.to_vec();
                            p.push(b"__cidx_primary_key_oversize__".to_vec());
                            p.push(p_key.clone());
                            // Encode the key's actual length in the
                            // last 8 bytes of the third hash slot for
                            // diagnostic.
                            let mut len_slot = [0u8; 32];
                            let len_be = (p_key.len() as u64).to_be_bytes();
                            len_slot[24..32].copy_from_slice(&len_be);
                            issues.insert(p, ([0u8; 32], [0u8; 32], len_slot));
                        }
                        let p_elem = Element::raw_decode(&p_value, grove_version)?;
                        primary_entries.insert(p_key, p_elem.count_value_or_default());
                    }
                    drop(content_iter);

                    // Use Vec<u64> rather than u64 so duplicate
                    // secondary rows for the same original_key (a real
                    // drift class — two count buckets pointing to the
                    // same item key) don't get silently collapsed.
                    // After collection, every original_key must have
                    // exactly one count; more = duplicate; zero =
                    // orphan.
                    let mut secondary_entries: HashMap<Vec<u8>, Vec<u64>> = HashMap::new();
                    let mut sec_iter =
                        KVIterator::new(secondary_merk.storage.raw_iter(), &all_query).unwrap();
                    while let Some((sec_key, _sec_value)) = sec_iter.next_kv().unwrap() {
                        // Secondary keys are (count_be ‖ original_key);
                        // a malformed key short of 8 bytes is itself a
                        // drift indicator.
                        if sec_key.len() < 8 {
                            let mut p = new_path.to_vec();
                            p.push(b"__cidx_secondary_malformed_key__".to_vec());
                            p.push(sec_key.clone());
                            issues.insert(p, ([0u8; 32], [0u8; 32], [0u8; 32]));
                            continue;
                        }
                        let mut count_bytes = [0u8; 8];
                        count_bytes.copy_from_slice(&sec_key[..8]);
                        let sec_count = u64::from_be_bytes(count_bytes);
                        let original_key = sec_key[8..].to_vec();
                        secondary_entries
                            .entry(original_key)
                            .or_default()
                            .push(sec_count);
                    }
                    drop(sec_iter);

                    // Surface duplicate-count entries explicitly. Each
                    // (key, [count_a, count_b, ...]) with more than one
                    // count is a duplicate-row drift — the cidx
                    // invariant is "exactly one secondary entry per
                    // primary entry".
                    for (s_key, counts) in &secondary_entries {
                        if counts.len() > 1 {
                            let mut p = new_path.to_vec();
                            p.push(b"__cidx_secondary_duplicate__".to_vec());
                            p.push(s_key.clone());
                            // Encode the duplicate count value in slot 2
                            // (just one of them; the consumer can scan
                            // the secondary to enumerate all).
                            let mut dup = [0u8; 32];
                            dup[24..32].copy_from_slice(&counts[0].to_be_bytes());
                            issues.insert(p, ([0u8; 32], [0u8; 32], dup));
                        }
                    }

                    // For each primary entry, the secondary must have
                    // a matching entry at the same count_value. We use
                    // `.first()` on the Vec — if there are multiple
                    // entries that's a duplicate (already flagged
                    // above); the first one is sufficient for the
                    // count-mismatch comparison here.
                    for (p_key, p_count) in &primary_entries {
                        match secondary_entries.get(p_key).and_then(|v| v.first()) {
                            None => {
                                let mut p = new_path.to_vec();
                                p.push(b"__cidx_primary_orphan__".to_vec());
                                p.push(p_key.clone());
                                issues.insert(p, ([0u8; 32], [0u8; 32], [0u8; 32]));
                            }
                            Some(s_count) if s_count != p_count => {
                                let mut p = new_path.to_vec();
                                p.push(b"__cidx_count_mismatch__".to_vec());
                                p.push(p_key.clone());
                                let mut expected = [0u8; 32];
                                expected[24..32].copy_from_slice(&p_count.to_be_bytes());
                                let mut actual = [0u8; 32];
                                actual[24..32].copy_from_slice(&s_count.to_be_bytes());
                                issues.insert(p, ([0u8; 32], expected, actual));
                            }
                            Some(_) => { /* matches */ }
                        }
                    }
                    // For each secondary entry, the primary must have
                    // a matching entry at that exact key (count match
                    // is already checked above; here we look for
                    // orphans in the secondary).
                    for s_key in secondary_entries.keys() {
                        if !primary_entries.contains_key(s_key) {
                            let mut p = new_path.to_vec();
                            p.push(b"__cidx_secondary_orphan__".to_vec());
                            p.push(s_key.clone());
                            issues.insert(p, ([0u8; 32], [0u8; 32], [0u8; 32]));
                        }
                    }

                    issues.extend(self.verify_merk_and_submerks_in_transaction(
                        primary_merk,
                        &new_path_ref,
                        batch,
                        transaction,
                        verify_references,
                        true,
                        grove_version,
                    )?);
                }
                Element::SumTree(..)
                | Element::Tree(..)
                | Element::BigSumTree(..)
                | Element::CountTree(..)
                | Element::CountSumTree(..)
                | Element::ProvableCountTree(..)
                | Element::ProvableCountSumTree(..)
                | Element::ProvableSumTree(..)
                | Element::ProvableCountProvableSumTree(..)
                | Element::CommitmentTree(..)
                | Element::MmrTree(..)
                | Element::BulkAppendTree(..)
                | Element::DenseAppendOnlyFixedSizeTree(..) => {
                    let (kv_value, element_value_hash) = merk
                        .get_value_and_value_hash(
                            &key,
                            allow_cache,
                            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                            grove_version,
                        )
                        .unwrap()
                        .map_err(MerkError)?
                        .ok_or(Error::CorruptedData(format!(
                            "expected merk to contain value at key {} for {}",
                            hex_to_ascii(&key),
                            element.type_str()
                        )))?;
                    let new_path = path.derive_owned_with_child(key);
                    let new_path_ref = SubtreePath::from(&new_path);

                    let inner_merk = self
                        .open_transactional_merk_at_path(
                            new_path_ref.clone(),
                            transaction,
                            batch,
                            grove_version,
                        )
                        .unwrap()?;
                    let merk_root_hash = inner_merk.root_hash().unwrap();

                    // Non-Merk tree types use their own root hash as the
                    // Merk child hash (not the inner Merk root, which is
                    // always empty for these types).
                    let root_hash = self.compute_non_merk_child_hash(
                        &element,
                        new_path_ref.clone(),
                        transaction,
                        merk_root_hash,
                    );

                    let actual_value_hash = value_hash(&kv_value).unwrap();
                    let combined_value_hash = combine_hash(&actual_value_hash, &root_hash).unwrap();

                    if combined_value_hash != element_value_hash {
                        issues.insert(
                            new_path.to_vec(),
                            (root_hash, combined_value_hash, element_value_hash),
                        );
                    }

                    // Software-consistency check: the aggregate fields
                    // stored in the parent's tree element (e.g.
                    // `sum_value` in `ProvableSumTree(_, sum_value, _)`)
                    // must agree with the inner Merk's actual
                    // `aggregate_data()`. This is distinct from the
                    // cryptographic check above: for the provable
                    // variants, both the recorded aggregate field AND
                    // the actual inner aggregate are bound into
                    // element_value_hash, but they are independently
                    // representable on disk and could disagree if a
                    // propagation bug (or storage corruption) drifts
                    // them out of sync. For non-provable variants, the
                    // aggregate field is stored alongside but not bound
                    // into the hash; an out-of-sync field is therefore
                    // a pure software bug, and the cryptographic check
                    // would not catch it.
                    //
                    // Non-Merk data trees (CommitmentTree, MmrTree,
                    // BulkAppendTree, DenseTree) keep an empty inner
                    // Merk by design, so their `aggregate_data()` is
                    // always `NoAggregateData`. Skip them here; the
                    // recursion below is already skipped for them via
                    // `uses_non_merk_data_storage`.
                    //
                    // For aggregate-mismatch logging we reuse the
                    // existing `VerificationIssues` shape
                    // (HashMap<path, (CryptoHash, CryptoHash,
                    // CryptoHash)>) by packing the recorded vs. actual
                    // aggregate values into a deterministic placeholder
                    // hash via blake3. This avoids breaking the type
                    // signature and all callers (including
                    // `visualize_verify_grovedb`), at the cost of the
                    // hex output being a placeholder rather than a
                    // real Merk hash. The recorded-aggregate hash is
                    // placed in the "expected" slot and the
                    // actual-aggregate hash in the "actual" slot; the
                    // "root" slot reuses the inner-Merk root hash for
                    // path-locality.
                    //
                    // Indexed-primary children with an EMPTY inner Merk: skip
                    // this check (see the narrowed guard below). Children of a
                    // `ProvableCountIndexedTree` / `ProvableSumIndexedTree` /
                    // `ProvableCountProvableSumIndexedTree` primary may carry an
                    // explicit aggregate field that encodes the child's
                    // secondary ordering key (the count/sum under which this
                    // child sorts) rather than a claim about the child's own
                    // inner Merk. A `CountTree(None, 42, None)` inserted at an
                    // indexed primary level legitimately has recorded=42 with an
                    // empty inner Merk; the user-supplied 42 is the ordering
                    // key, not a self-consistency claim. The primary-level
                    // secondary-index check already validates that each child's
                    // recorded aggregate matches the secondary index entry.
                    // Applying the generic aggregate-consistency check to such a
                    // child would false-positive (recorded=42 vs an empty inner
                    // Merk's `NoAggregateData`), so it is skipped — but ONLY for
                    // the empty-inner-Merk case. A POPULATED child under an
                    // indexed primary keeps recorded == actual (propagation
                    // maintains the invariant), so the check remains active and
                    // still catches software-bug corruption that the hash chain
                    // alone cannot detect.
                    if !element.uses_non_merk_data_storage() {
                        let actual_aggregate = inner_merk.aggregate_data().map_err(MerkError)?;
                        // Under an indexed-tree primary, a child element's
                        // recorded aggregate (e.g. `CountTree(None, 42, None)`)
                        // may legitimately be a user-supplied secondary
                        // ordering key rather than a claim about the child's
                        // own inner Merk — but ONLY when that inner Merk is
                        // empty (`NoAggregateData`). In that empty case the
                        // generic consistency check would false-positive (a
                        // non-zero recorded value against `NoAggregateData`
                        // falls through to the catch-all mismatch arm of
                        // `aggregate_consistency_labels`); the primary-level
                        // secondary-index check already validates the ordering
                        // key, so skip here. For a POPULATED child the recorded
                        // value must equal the inner Merk's actual aggregate
                        // (propagation keeps them in lock-step), so the check
                        // stays active and still catches software-bug
                        // corruption that the hash chain alone cannot.
                        let skip_indexed_primary_empty_child = merk.tree_type.is_indexed_primary()
                            && actual_aggregate == AggregateData::NoAggregateData;
                        if !skip_indexed_primary_empty_child
                            && let Some((recorded_label, actual_label)) =
                                aggregate_consistency_labels(&element, &actual_aggregate)
                        {
                            let expected_placeholder: CryptoHash =
                                blake3::hash(recorded_label.as_bytes()).into();
                            let actual_placeholder: CryptoHash =
                                blake3::hash(actual_label.as_bytes()).into();
                            // Use `.entry().or_insert(...)` so we don't
                            // clobber an earlier cryptographic
                            // (`combined_value_hash != element_value_hash`)
                            // entry inserted above for this same path —
                            // the real Merk-hash chain mismatch is more
                            // diagnostic than the aggregate placeholder.
                            issues.entry(new_path.to_vec()).or_insert((
                                root_hash,
                                expected_placeholder,
                                actual_placeholder,
                            ));
                        }
                    }

                    // Non-Merk data trees (CommitmentTree, MmrTree,
                    // BulkAppendTree, DenseTree) store data in the data
                    // namespace as non-Element entries.  Recursing into
                    // verify_merk_and_submerks would try to deserialize
                    // those entries as Elements and fail.
                    if !element.uses_non_merk_data_storage() {
                        issues.extend(self.verify_merk_and_submerks_in_transaction(
                            inner_merk,
                            &new_path_ref,
                            batch,
                            transaction,
                            verify_references,
                            true,
                            grove_version,
                        )?);
                    }
                }
                Element::Item(..) | Element::SumItem(..) | Element::ItemWithSumItem(..) => {
                    let (kv_value, element_value_hash) = merk
                        .get_value_and_value_hash(
                            &key,
                            allow_cache,
                            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                            grove_version,
                        )
                        .unwrap()
                        .map_err(MerkError)?
                        .ok_or(Error::CorruptedData(format!(
                            "expected merk to contain value at key {} for {}",
                            hex_to_ascii(&key),
                            element.type_str()
                        )))?;
                    let actual_value_hash = value_hash(&kv_value).unwrap();
                    if actual_value_hash != element_value_hash {
                        issues.insert(
                            path.derive_owned_with_child(key).to_vec(),
                            (actual_value_hash, element_value_hash, actual_value_hash),
                        );
                    }
                }
                Element::Reference(ref reference_path, ..)
                | Element::ReferenceWithSumItem(ref reference_path, ..) => {
                    // Skip this whole check if we don't `verify_references`.
                    // `ReferenceWithSumItem` shares this verification path —
                    // the sum is hashed as part of the serialized value
                    // bytes, so the combined-hash check below is identical.
                    if !verify_references {
                        continue;
                    }

                    // Merk we're checking:
                    let (kv_value, element_value_hash) = merk
                        .get_value_and_value_hash(
                            &key,
                            allow_cache,
                            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                            grove_version,
                        )
                        .unwrap()
                        .map_err(MerkError)?
                        .ok_or(Error::CorruptedData(format!(
                            "expected merk to contain value at key {} for reference",
                            hex_to_ascii(&key)
                        )))?;

                    let referenced_value_hash = {
                        let full_path = path_from_reference_path_type(
                            reference_path.clone(),
                            &path.to_vec(),
                            Some(&key),
                        )?;
                        let item = self
                            .follow_reference(
                                (full_path.as_slice()).into(),
                                allow_cache,
                                Some(transaction),
                                grove_version,
                            )
                            .unwrap()?;
                        item.value_hash(grove_version).unwrap()?
                    };

                    // Take the current item (reference) hash and combine it with referenced value's
                    // hash
                    let self_actual_value_hash = value_hash(&kv_value).unwrap();
                    let combined_value_hash =
                        combine_hash(&self_actual_value_hash, &referenced_value_hash).unwrap();

                    if combined_value_hash != element_value_hash {
                        issues.insert(
                            path.derive_owned_with_child(key).to_vec(),
                            (combined_value_hash, element_value_hash, combined_value_hash),
                        );
                    }
                }
                // ProvableSumIndexedTree integrity: identical shape to
                // PCIT but the secondary is a `ProvableSumTree`. Open
                // both Merks, recompute `combine_hash_three(value_hash,
                // primary_root_hash, secondary_root_hash)`, compare
                // to the parent's stored combined value hash, and
                // recurse into the primary.
                Element::ProvableSumIndexedTree(..) => {
                    let (kv_value, element_value_hash) = merk
                        .get_value_and_value_hash(
                            &key,
                            allow_cache,
                            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                            grove_version,
                        )
                        .unwrap()
                        .map_err(MerkError)?
                        .ok_or(Error::CorruptedData(format!(
                            "expected merk to contain value at key {} for {}",
                            hex_to_ascii(&key),
                            element.type_str()
                        )))?;
                    let new_path = path.derive_owned_with_child(key);
                    let new_path_ref = SubtreePath::from(&new_path);

                    let primary_merk = self
                        .open_transactional_merk_at_path(
                            new_path_ref.clone(),
                            transaction,
                            batch,
                            grove_version,
                        )
                        .unwrap()?;
                    let primary_root_hash = primary_merk.root_hash().unwrap();

                    let secondary_root_key = match element {
                        Element::ProvableSumIndexedTree(_, ref s, ..) => s.clone(),
                        _ => unreachable!("matched PSIT variant above"),
                    };
                    let secondary_merk = self
                        .open_indexed_secondary_at_path(
                            new_path_ref.clone(),
                            grovedb_element::indexed::IndexAxis::Sum,
                            secondary_root_key,
                            transaction,
                            batch,
                            grove_version,
                        )
                        .unwrap()?;
                    let secondary_root_hash = secondary_merk.root_hash().unwrap();

                    let actual_value_hash = value_hash(&kv_value).unwrap();
                    let combined_value_hash = combine_hash_three(
                        &actual_value_hash,
                        &primary_root_hash,
                        &secondary_root_hash,
                    )
                    .unwrap();

                    if combined_value_hash != element_value_hash {
                        issues.insert(
                            new_path.to_vec(),
                            (primary_root_hash, combined_value_hash, element_value_hash),
                        );
                    }

                    issues.extend(self.verify_merk_and_submerks_in_transaction(
                        primary_merk,
                        &new_path_ref,
                        batch,
                        transaction,
                        verify_references,
                        true,
                        grove_version,
                    )?);
                }
                // ProvableCountProvableSumIndexedTree integrity: open
                // each axis's secondary Merk, build the canonical
                // `axes_digest`, recompute `combine_hash_three(
                // value_hash, primary_root_hash, axes_digest)`, and
                // compare to the parent's stored hash. Then recurse
                // into the primary.
                Element::ProvableCountProvableSumIndexedTree(..) => {
                    let (kv_value, element_value_hash) = merk
                        .get_value_and_value_hash(
                            &key,
                            allow_cache,
                            None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                            grove_version,
                        )
                        .unwrap()
                        .map_err(MerkError)?
                        .ok_or(Error::CorruptedData(format!(
                            "expected merk to contain value at key {} for {}",
                            hex_to_ascii(&key),
                            element.type_str()
                        )))?;
                    let new_path = path.derive_owned_with_child(key);
                    let new_path_ref = SubtreePath::from(&new_path);

                    let primary_merk = self
                        .open_transactional_merk_at_path(
                            new_path_ref.clone(),
                            transaction,
                            batch,
                            grove_version,
                        )
                        .unwrap()?;
                    let primary_root_hash = primary_merk.root_hash().unwrap();

                    let axes = match element {
                        Element::ProvableCountProvableSumIndexedTree(_, _, _, ref a, _) => {
                            a.clone()
                        }
                        _ => unreachable!("matched PCPSIT variant above"),
                    };
                    let mut axis_hashes: Vec<(u8, grovedb_merk::CryptoHash)> =
                        Vec::with_capacity(axes.len());
                    for (tag, sec_root_key) in &axes {
                        let axis = grovedb_element::indexed::IndexAxis::try_from_tag(*tag)
                            .map_err(|e| {
                                Error::CorruptedData(format!(
                                    "invalid axis tag in PCPSIT element: {e}"
                                ))
                            })?;
                        let secondary_merk = self
                            .open_indexed_secondary_at_path(
                                new_path_ref.clone(),
                                axis,
                                sec_root_key.clone(),
                                transaction,
                                batch,
                                grove_version,
                            )
                            .unwrap()?;
                        axis_hashes.push((*tag, secondary_merk.root_hash().unwrap()));
                    }
                    // `axes_digest` returns a `CostContext`; its
                    // `hash_node_calls` cost is intentionally discarded here.
                    // `verify_merk_and_submerks_in_transaction` is an
                    // integrity-verification path with no cost accumulator in
                    // scope (every hash/root-hash call in this function is
                    // `.unwrap()`ed and its cost dropped), so we follow the
                    // same convention rather than thread a cost through purely
                    // for the verifier.
                    let axes_digest_value = grovedb_merk::tree::axes_digest(&axis_hashes).unwrap();

                    let actual_value_hash = value_hash(&kv_value).unwrap();
                    let combined_value_hash = combine_hash_three(
                        &actual_value_hash,
                        &primary_root_hash,
                        &axes_digest_value,
                    )
                    .unwrap();

                    if combined_value_hash != element_value_hash {
                        issues.insert(
                            new_path.to_vec(),
                            (primary_root_hash, combined_value_hash, element_value_hash),
                        );
                    }

                    issues.extend(self.verify_merk_and_submerks_in_transaction(
                        primary_merk,
                        &new_path_ref,
                        batch,
                        transaction,
                        verify_references,
                        true,
                        grove_version,
                    )?);
                }
                Element::NonCounted(_) | Element::NotSummed(_) | Element::NotCountedOrSummed(_) => {
                    unreachable!("unwrapped above")
                }
            }
        }
        Ok(issues)
    }

    /// Compute the child hash for a non-Merk tree element by reconstructing
    /// its tree from storage and computing the state root.
    /// Falls back to `merk_root_hash` on any error or for standard Merk trees.
    fn compute_non_merk_child_hash<'b, B: AsRef<[u8]>>(
        &self,
        element: &Element,
        subtree_path: SubtreePath<'b, B>,
        transaction: &Transaction,
        merk_root_hash: [u8; 32],
    ) -> [u8; 32] {
        match element {
            Element::CommitmentTree(total_count, chunk_power, _) => {
                if *total_count == 0 {
                    return grovedb_commitment_tree::EMPTY_COMMITMENT_TREE_STATE_ROOT;
                }
                let storage_ctx = self
                    .db
                    .get_transactional_storage_context(subtree_path, None, transaction)
                    .unwrap();
                match grovedb_commitment_tree::CommitmentTree::<_>::open(
                    *total_count,
                    *chunk_power,
                    storage_ctx,
                )
                .value
                {
                    Ok(ct) => ct.compute_current_state_root().unwrap_or(merk_root_hash),
                    Err(_) => merk_root_hash,
                }
            }
            Element::BulkAppendTree(total_count, chunk_power, _) => {
                if *total_count == 0 {
                    return merk_root_hash;
                }
                let storage_ctx = self
                    .db
                    .get_transactional_storage_context(subtree_path, None, transaction)
                    .unwrap();
                match grovedb_bulk_append_tree::BulkAppendTree::from_state(
                    *total_count,
                    *chunk_power,
                    storage_ctx,
                ) {
                    Ok(tree) => tree.compute_current_state_root().unwrap_or(merk_root_hash),
                    Err(_) => merk_root_hash,
                }
            }
            Element::MmrTree(mmr_size, _) => {
                if *mmr_size == 0 {
                    return merk_root_hash;
                }
                let storage_ctx = self
                    .db
                    .get_transactional_storage_context(subtree_path, None, transaction)
                    .unwrap();
                let store = grovedb_merkle_mountain_range::MmrStore::new(&storage_ctx);
                let mmr = grovedb_merkle_mountain_range::MMR::new(*mmr_size, &store);
                match mmr.get_root().value {
                    Ok(root) => root.hash(),
                    Err(_) => merk_root_hash,
                }
            }
            Element::DenseAppendOnlyFixedSizeTree(count, height, _) => {
                if *count == 0 {
                    return merk_root_hash;
                }
                let storage_ctx = self
                    .db
                    .get_transactional_storage_context(subtree_path, None, transaction)
                    .unwrap();
                use grovedb_dense_fixed_sized_merkle_tree::DenseFixedSizedMerkleTree;
                match DenseFixedSizedMerkleTree::from_state(*height, *count, storage_ctx) {
                    Ok(t) => match t.root_hash().unwrap() {
                        Ok(hash) => hash,
                        Err(_) => merk_root_hash,
                    },
                    Err(_) => merk_root_hash,
                }
            }
            _ => merk_root_hash,
        }
    }
}

/// Inspect a tree-bearing Element together with the actual aggregate data of
/// its inner Merk. Returns `Some((recorded_label, actual_label))` if the
/// aggregate field(s) stored in the element disagree with `actual`, or
/// `None` if they match (or if `element` is not a tree variant that carries
/// an aggregate field reflecting the inner Merk's `aggregate_data()`).
///
/// The string labels are intended to be hashed into deterministic placeholder
/// `CryptoHash` values for inclusion in `VerificationIssues`.
///
/// Coverage:
/// - `SumTree(_, n, _)` vs. `AggregateData::Sum(m)`.
/// - `ProvableSumTree(_, n, _)` vs. `AggregateData::ProvableSum(m)`.
/// - `BigSumTree(_, n, _)` vs. `AggregateData::BigSum(m)`.
/// - `CountTree(_, n, _)` vs. `AggregateData::Count(m)`.
/// - `CountSumTree(_, c, s, _)` vs. `AggregateData::CountAndSum(cm, sm)`.
/// - `ProvableCountTree(_, n, _)` vs. `AggregateData::ProvableCount(m)`.
/// - `ProvableCountSumTree(_, c, s, _)` vs.
///   `AggregateData::ProvableCountAndSum(cm, sm)`.
/// - `ProvableCountProvableSumTree(_, c, s, _)` vs.
///   `AggregateData::ProvableCountAndProvableSum(cm, sm)`.
///
/// A plain `Element::Tree(..)` has no aggregate field; the inner Merk's
/// `aggregate_data` is `NoAggregateData` by construction, and any other
/// value would be a separate corruption (caught by the type/feature checks
/// elsewhere). We return `None` for it here.
///
/// A variant/aggregate-shape mismatch (e.g. `ProvableSumTree` whose inner
/// Merk reports `AggregateData::Count(_)` instead of `ProvableSum(_)`) is
/// also reported, because the inner Merk's tree-type has drifted from what
/// the parent element claims.
#[cfg(feature = "minimal")]
fn aggregate_consistency_labels(
    element: &Element,
    actual: &AggregateData,
) -> Option<(String, String)> {
    match (element, actual) {
        // --- Plain Tree: no aggregate, never reports a mismatch.
        (Element::Tree(..), AggregateData::NoAggregateData) => None,

        // --- SumTree variants ---
        (Element::SumTree(_, recorded, _), AggregateData::Sum(actual_sum)) => {
            if recorded == actual_sum {
                None
            } else {
                Some((
                    format!("SumTree recorded sum {}", recorded),
                    format!("inner aggregate Sum {}", actual_sum),
                ))
            }
        }
        (Element::ProvableSumTree(_, recorded, _), AggregateData::ProvableSum(actual_sum)) => {
            if recorded == actual_sum {
                None
            } else {
                Some((
                    format!("ProvableSumTree recorded sum {}", recorded),
                    format!("inner aggregate ProvableSum {}", actual_sum),
                ))
            }
        }
        (Element::BigSumTree(_, recorded, _), AggregateData::BigSum(actual_sum)) => {
            if recorded == actual_sum {
                None
            } else {
                Some((
                    format!("BigSumTree recorded sum {}", recorded),
                    format!("inner aggregate BigSum {}", actual_sum),
                ))
            }
        }
        (Element::CountTree(_, recorded, _), AggregateData::Count(actual_count)) => {
            if recorded == actual_count {
                None
            } else {
                Some((
                    format!("CountTree recorded count {}", recorded),
                    format!("inner aggregate Count {}", actual_count),
                ))
            }
        }
        (
            Element::CountSumTree(_, recorded_count, recorded_sum, _),
            AggregateData::CountAndSum(actual_count, actual_sum),
        ) => {
            if recorded_count == actual_count && recorded_sum == actual_sum {
                None
            } else {
                Some((
                    format!(
                        "CountSumTree recorded count {} sum {}",
                        recorded_count, recorded_sum
                    ),
                    format!(
                        "inner aggregate CountAndSum count {} sum {}",
                        actual_count, actual_sum
                    ),
                ))
            }
        }
        (
            Element::ProvableCountTree(_, recorded, _),
            AggregateData::ProvableCount(actual_count),
        ) => {
            if recorded == actual_count {
                None
            } else {
                Some((
                    format!("ProvableCountTree recorded count {}", recorded),
                    format!("inner aggregate ProvableCount {}", actual_count),
                ))
            }
        }
        (
            Element::ProvableCountSumTree(_, recorded_count, recorded_sum, _),
            AggregateData::ProvableCountAndSum(actual_count, actual_sum),
        ) => {
            if recorded_count == actual_count && recorded_sum == actual_sum {
                None
            } else {
                Some((
                    format!(
                        "ProvableCountSumTree recorded count {} sum {}",
                        recorded_count, recorded_sum
                    ),
                    format!(
                        "inner aggregate ProvableCountAndSum count {} sum {}",
                        actual_count, actual_sum
                    ),
                ))
            }
        }
        (
            Element::ProvableCountProvableSumTree(_, recorded_count, recorded_sum, _),
            AggregateData::ProvableCountAndProvableSum(actual_count, actual_sum),
        ) => {
            if recorded_count == actual_count && recorded_sum == actual_sum {
                None
            } else {
                Some((
                    format!(
                        "ProvableCountProvableSumTree recorded count {} sum {}",
                        recorded_count, recorded_sum
                    ),
                    format!(
                        "inner aggregate ProvableCountAndProvableSum count {} sum {}",
                        actual_count, actual_sum
                    ),
                ))
            }
        }

        // --- Empty-merk edge case: an empty Merk returns NoAggregateData
        // for any tree type. This is the correct initial state for a
        // freshly-inserted tree element. Treat as not-mismatching as long
        // as the recorded aggregate is the identity for that variant
        // (zero / zero counts). Anything else is a real mismatch. ---
        (Element::SumTree(_, recorded, _), AggregateData::NoAggregateData) if *recorded == 0 => {
            None
        }
        (Element::ProvableSumTree(_, recorded, _), AggregateData::NoAggregateData)
            if *recorded == 0 =>
        {
            None
        }
        (Element::BigSumTree(_, recorded, _), AggregateData::NoAggregateData) if *recorded == 0 => {
            None
        }
        (Element::CountTree(_, recorded, _), AggregateData::NoAggregateData) if *recorded == 0 => {
            None
        }
        (
            Element::CountSumTree(_, recorded_count, recorded_sum, _),
            AggregateData::NoAggregateData,
        ) if *recorded_count == 0 && *recorded_sum == 0 => None,
        (Element::ProvableCountTree(_, recorded, _), AggregateData::NoAggregateData)
            if *recorded == 0 =>
        {
            None
        }
        (
            Element::ProvableCountSumTree(_, recorded_count, recorded_sum, _),
            AggregateData::NoAggregateData,
        ) if *recorded_count == 0 && *recorded_sum == 0 => None,
        (
            Element::ProvableCountProvableSumTree(_, recorded_count, recorded_sum, _),
            AggregateData::NoAggregateData,
        ) if *recorded_count == 0 && *recorded_sum == 0 => None,

        // --- Non-Merk data trees: caller skips us via
        // `uses_non_merk_data_storage`; if we end up here anyway, do not
        // report. ---
        (Element::CommitmentTree(..), _)
        | (Element::MmrTree(..), _)
        | (Element::BulkAppendTree(..), _)
        | (Element::DenseAppendOnlyFixedSizeTree(..), _) => None,

        // --- Anything else is a variant/aggregate-shape mismatch (e.g.
        // the inner Merk's tree-type has drifted from what the parent
        // claims). Report with descriptive labels. ---
        (element, actual) => Some((
            format!("element variant {}", element.type_str()),
            format!("inner aggregate variant {:?}", actual),
        )),
    }
}

#[cfg(all(test, feature = "minimal"))]
mod aggregate_consistency_labels_tests {
    //! Unit tests for the `aggregate_consistency_labels` helper. Each
    //! aggregate-bearing tree variant has a match arm; covering each arm
    //! requires one matching pair (matches, returns None) plus one
    //! mismatching pair (disagrees, returns Some). Plus the
    //! NoAggregateData identity arms, the non-Merk-data-tree arms, and
    //! the catch-all variant/shape mismatch.

    use grovedb_merk::tree::AggregateData;

    use super::{aggregate_consistency_labels, Element};

    // --- SumTree -----------------------------------------------------------
    #[test]
    fn sum_tree_match_returns_none() {
        let e = Element::SumTree(None, 42, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::Sum(42)).is_none());
    }

    #[test]
    fn sum_tree_mismatch_returns_labels() {
        let e = Element::SumTree(None, 42, None);
        let labels = aggregate_consistency_labels(&e, &AggregateData::Sum(1)).expect("labels");
        assert!(labels.0.contains("SumTree recorded sum 42"));
        assert!(labels.1.contains("Sum 1"));
    }

    // --- ProvableSumTree ---------------------------------------------------
    #[test]
    fn provable_sum_tree_match_returns_none() {
        let e = Element::ProvableSumTree(None, 7, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::ProvableSum(7)).is_none());
    }

    #[test]
    fn provable_sum_tree_mismatch_returns_labels() {
        let e = Element::ProvableSumTree(None, 7, None);
        let labels =
            aggregate_consistency_labels(&e, &AggregateData::ProvableSum(0)).expect("labels");
        assert!(labels.0.contains("ProvableSumTree recorded sum 7"));
        assert!(labels.1.contains("ProvableSum 0"));
    }

    // --- BigSumTree --------------------------------------------------------
    #[test]
    fn big_sum_tree_match_returns_none() {
        let e = Element::BigSumTree(None, 100, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::BigSum(100)).is_none());
    }

    #[test]
    fn big_sum_tree_mismatch_returns_labels() {
        let e = Element::BigSumTree(None, 100, None);
        let labels = aggregate_consistency_labels(&e, &AggregateData::BigSum(0)).expect("labels");
        assert!(labels.0.contains("BigSumTree recorded sum 100"));
        assert!(labels.1.contains("BigSum 0"));
    }

    // --- CountTree ---------------------------------------------------------
    #[test]
    fn count_tree_match_returns_none() {
        let e = Element::CountTree(None, 9, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::Count(9)).is_none());
    }

    #[test]
    fn count_tree_mismatch_returns_labels() {
        let e = Element::CountTree(None, 9, None);
        let labels = aggregate_consistency_labels(&e, &AggregateData::Count(0)).expect("labels");
        assert!(labels.0.contains("CountTree recorded count 9"));
        assert!(labels.1.contains("Count 0"));
    }

    // --- CountSumTree -------------------------------------------------------
    #[test]
    fn count_sum_tree_match_returns_none() {
        let e = Element::CountSumTree(None, 3, 14, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::CountAndSum(3, 14)).is_none());
    }

    #[test]
    fn count_sum_tree_mismatch_returns_labels() {
        let e = Element::CountSumTree(None, 3, 14, None);
        let labels =
            aggregate_consistency_labels(&e, &AggregateData::CountAndSum(3, 0)).expect("labels");
        assert!(labels.0.contains("recorded count 3 sum 14"));
        assert!(labels.1.contains("count 3 sum 0"));
    }

    // --- ProvableCountTree -------------------------------------------------
    #[test]
    fn provable_count_tree_match_returns_none() {
        let e = Element::ProvableCountTree(None, 5, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::ProvableCount(5)).is_none());
    }

    #[test]
    fn provable_count_tree_mismatch_returns_labels() {
        let e = Element::ProvableCountTree(None, 5, None);
        let labels =
            aggregate_consistency_labels(&e, &AggregateData::ProvableCount(0)).expect("labels");
        assert!(labels.0.contains("ProvableCountTree recorded count 5"));
        assert!(labels.1.contains("ProvableCount 0"));
    }

    // --- ProvableCountSumTree ----------------------------------------------
    #[test]
    fn provable_count_sum_tree_match_returns_none() {
        let e = Element::ProvableCountSumTree(None, 4, 8, None);
        assert!(
            aggregate_consistency_labels(&e, &AggregateData::ProvableCountAndSum(4, 8)).is_none()
        );
    }

    #[test]
    fn provable_count_sum_tree_mismatch_returns_labels() {
        let e = Element::ProvableCountSumTree(None, 4, 8, None);
        let labels = aggregate_consistency_labels(&e, &AggregateData::ProvableCountAndSum(4, 0))
            .expect("labels");
        assert!(labels.0.contains("recorded count 4 sum 8"));
        assert!(labels.1.contains("count 4 sum 0"));
    }

    // --- Plain Tree / NoAggregateData --------------------------------------
    #[test]
    fn plain_tree_no_aggregate_returns_none() {
        let e = Element::Tree(None, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    // --- NoAggregateData with empty-merk identity arms ---------------------
    #[test]
    fn sum_tree_zero_recorded_with_no_aggregate_is_ok() {
        let e = Element::SumTree(None, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    #[test]
    fn sum_tree_nonzero_recorded_with_no_aggregate_is_mismatch() {
        // Should fall through to the catch-all variant/shape mismatch arm.
        let e = Element::SumTree(None, 7, None);
        let labels =
            aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).expect("labels");
        assert!(labels.0.contains("element variant"));
        assert!(labels.1.contains("NoAggregateData"));
    }

    #[test]
    fn provable_sum_tree_zero_recorded_with_no_aggregate_is_ok() {
        let e = Element::ProvableSumTree(None, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    #[test]
    fn big_sum_tree_zero_recorded_with_no_aggregate_is_ok() {
        let e = Element::BigSumTree(None, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    #[test]
    fn count_tree_zero_recorded_with_no_aggregate_is_ok() {
        let e = Element::CountTree(None, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    #[test]
    fn count_sum_tree_zero_zero_with_no_aggregate_is_ok() {
        let e = Element::CountSumTree(None, 0, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    #[test]
    fn provable_count_tree_zero_recorded_with_no_aggregate_is_ok() {
        let e = Element::ProvableCountTree(None, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    #[test]
    fn provable_count_sum_tree_zero_zero_with_no_aggregate_is_ok() {
        let e = Element::ProvableCountSumTree(None, 0, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    // --- Non-Merk data trees: always None ---------------------------------
    #[test]
    fn commitment_tree_always_returns_none() {
        let e = Element::CommitmentTree(0, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
        // Even a non-NoAggregateData paired with these returns None per the
        // explicit catch arm.
        assert!(aggregate_consistency_labels(&e, &AggregateData::Sum(5)).is_none());
    }

    #[test]
    fn mmr_tree_always_returns_none() {
        let e = Element::MmrTree(0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    #[test]
    fn bulk_append_tree_always_returns_none() {
        let e = Element::BulkAppendTree(0, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    #[test]
    fn dense_append_only_tree_always_returns_none() {
        let e = Element::DenseAppendOnlyFixedSizeTree(0, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    // --- Catch-all variant/shape mismatch ---------------------------------
    #[test]
    fn provable_sum_tree_paired_with_wrong_aggregate_kind_is_mismatch() {
        // ProvableSumTree vs Count → catch-all variant-mismatch arm.
        let e = Element::ProvableSumTree(None, 7, None);
        let labels = aggregate_consistency_labels(&e, &AggregateData::Count(0)).expect("labels");
        assert!(labels.0.contains("element variant"));
        assert!(labels.1.contains("inner aggregate variant"));
    }

    #[test]
    fn item_element_paired_with_no_aggregate_is_mismatch() {
        // Item isn't a tree at all → catch-all (no specific arm matches).
        let e = Element::Item(b"x".to_vec(), None);
        let labels =
            aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).expect("labels");
        assert!(labels.0.contains("element variant"));
    }

    // --- ProvableCountProvableSumTree (dual-axis) -------------------------
    //
    // The new PCPS host carries `(count, sum)` recorded values that must
    // line up with the inner Merk's `AggregateData::ProvableCountAndProvableSum`
    // variant. The arm we added in `aggregate_consistency_labels` handles
    // three cases:
    //   1. Equal recorded vs. actual → returns None (no mismatch).
    //   2. Equal recorded vs. actual diverging → returns labels.
    //   3. Recorded == (0, 0) + inner aggregate == NoAggregateData → None
    //      (empty-merk edge case).
    //
    // These tests pin each branch so future refactors of the helper can't
    // silently break PCPS aggregate-consistency reporting.

    #[test]
    fn provable_count_provable_sum_tree_equal_recorded_and_actual_is_ok() {
        let e = Element::ProvableCountProvableSumTree(None, 5, 42, None);
        assert!(aggregate_consistency_labels(
            &e,
            &AggregateData::ProvableCountAndProvableSum(5, 42),
        )
        .is_none());
    }

    #[test]
    fn provable_count_provable_sum_tree_count_mismatch_reports_labels() {
        let e = Element::ProvableCountProvableSumTree(None, 5, 42, None);
        let labels =
            aggregate_consistency_labels(&e, &AggregateData::ProvableCountAndProvableSum(6, 42))
                .expect("labels");
        assert!(
            labels
                .0
                .contains("ProvableCountProvableSumTree recorded count 5"),
            "left label: {}",
            labels.0
        );
        assert!(
            labels
                .1
                .contains("ProvableCountAndProvableSum count 6 sum 42"),
            "right label: {}",
            labels.1
        );
    }

    #[test]
    fn provable_count_provable_sum_tree_sum_mismatch_reports_labels() {
        let e = Element::ProvableCountProvableSumTree(None, 5, 42, None);
        let labels =
            aggregate_consistency_labels(&e, &AggregateData::ProvableCountAndProvableSum(5, -100))
                .expect("labels");
        assert!(
            labels.0.contains("sum 42"),
            "left label should contain recorded sum: {}",
            labels.0
        );
        assert!(
            labels.1.contains("sum -100"),
            "right label should contain actual sum: {}",
            labels.1
        );
    }

    #[test]
    fn provable_count_provable_sum_tree_zero_zero_with_no_aggregate_is_ok() {
        // Empty-merk edge case: a freshly-inserted PCPS element has
        // recorded (0, 0) and an inner merk reporting NoAggregateData.
        // The dedicated arm we added must short-circuit this to None.
        let e = Element::ProvableCountProvableSumTree(None, 0, 0, None);
        assert!(aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).is_none());
    }

    #[test]
    fn provable_count_provable_sum_tree_nonzero_with_no_aggregate_is_mismatch() {
        // Non-zero recorded + NoAggregateData is NOT the empty-merk
        // shape — it should fall into the catch-all and surface a
        // variant-mismatch label.
        let e = Element::ProvableCountProvableSumTree(None, 1, 5, None);
        let labels =
            aggregate_consistency_labels(&e, &AggregateData::NoAggregateData).expect("labels");
        assert!(labels.0.contains("element variant"));
        assert!(labels.1.contains("inner aggregate variant"));
    }

    #[test]
    fn provable_count_provable_sum_tree_paired_with_wrong_aggregate_kind_is_mismatch() {
        // PCPS vs. ProvableCountAndSum (the single-axis sum aggregate)
        // → catch-all variant-mismatch arm. The inner merk's tree-type
        // has drifted from what the parent element claims.
        let e = Element::ProvableCountProvableSumTree(None, 5, 42, None);
        let labels = aggregate_consistency_labels(&e, &AggregateData::ProvableCountAndSum(5, 42))
            .expect("labels");
        assert!(
            labels.0.contains("element variant"),
            "expected element-variant catch-all label: {}",
            labels.0
        );
        assert!(
            labels.1.contains("inner aggregate variant"),
            "expected aggregate-variant catch-all label: {}",
            labels.1
        );
    }
}

/// Test-only helpers for verifying internal storage state.
#[cfg(all(test, feature = "minimal"))]
impl GroveDb {
    /// Read a raw key from a subtree's transactional storage context.
    ///
    /// This bypasses all element-level checks (count, type) and reads directly
    /// from the subtree's storage. Used in tests to verify that no data leaked
    /// into the transaction after a failed batch.
    pub(crate) fn raw_subtree_get<'b, B: AsRef<[u8]> + 'b>(
        &self,
        path: SubtreePath<'b, B>,
        key: &[u8],
        transaction: &Transaction,
    ) -> Result<Option<Vec<u8>>, Error> {
        let storage_ctx = self
            .db
            .get_transactional_storage_context(path, None, transaction)
            .value;

        let result = storage_ctx.get(key).value;
        match result {
            Ok(opt) => Ok(opt.map(|v| v.to_vec())),
            Err(e) => Err(e.into()),
        }
    }
}
