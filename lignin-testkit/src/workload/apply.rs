//! Applying workload operations to a GroveDB.
//!
//! This is the one place that turns an [`Op`] into GroveDB calls, so a
//! recorder, a replayer and a benchmark all drive the database the same way.
//! Block handling stays with the caller: [`apply`] runs inside the
//! transaction it is given and reports [`Outcome::EndOfBlock`] for
//! [`Op::Commit`] without committing anything. The one operation that does
//! not run inside it is [`Op::FlushPendingDrops`], which GroveDB only offers
//! on the database itself.
//!
//! The caller owns its transactions, so four outcomes ask it to act:
//!
//! - [`Outcome::EndOfBlock`]: commit the transaction and start another.
//! - [`Outcome::Abandoned`]: drop the transaction, with no commit and no
//!   rollback, and start another.
//! - [`Outcome::SnapshotOpens`]: start a snapshot read transaction and keep
//!   it beside the block's transaction, in place of one already open.
//! - [`Outcome::SnapshotCloses`]: drop it.
//!
//! A [`TransactionOp::SnapshotRead`] is the one operation applied with the
//! snapshot as its transaction, and [`apply`] refuses it any other. Every
//! other operation is applied with the block's transaction, and a step on
//! that transaction or a read inside the block is refused the snapshot.
//! [`Transactions`] does all of this for a caller that has no reason to do
//! it itself.
//!
//! # Storage-flag callbacks
//!
//! GroveDB takes two callbacks with a batch, and Drive passes them with
//! every batch it applies. One is asked what flags an element that is
//! replaced is stored with, given the flags it had, the flags written and
//! what the update costs. The other is asked how the bytes an update or a
//! delete frees divide over the epochs they were paid in. A stream is plain
//! data and cannot carry a callback, so the ones Drive passes are fixed
//! here, for [`Op::FlagsBatch`], [`Op::FlagsHandoverBatch`] and
//! [`Op::FlagsPartialBatch`]. Every other operation runs with GroveDB's
//! own: flags are stored as written and freed bytes are not divided.
//!
//! The operations of those three kinds are also built the way Drive builds
//! every operation it puts in a batch: each declares that the value it
//! replaces or deletes takes no part in a backward reference
//! (`dont_check_for_backwards_references`). For the elements the streams
//! hold, GroveDB returns the same cost, root hash and proof either way.
//!
//! Drive passes one of three first callbacks:
//!
//! - **With a batch**: [`StorageFlags::update_element_flags`]. An update
//!   that grows an element in a later epoch than its first records the
//!   added bytes under that epoch, one that shrinks it takes the freed
//!   bytes off the epochs recorded, and one that keeps its size keeps the
//!   stored flags. Where the flags written name another owner than the
//!   stored ones, the written owner is stored, except that the stored flags
//!   are kept whole by an update that keeps the size and by one that
//!   shrinks an element holding bytes of one epoch only.
//! - **With a batch that updates a document**, from protocol version 15:
//!   the same merge, after which the owner written is stored in every
//!   case ([`StorageFlags::update_element_flags_with_written_owner`]). An
//!   update between two owners of which one is a gas sponsor is accounted
//!   as a replacement instead: the stored element is freed whole and the
//!   written one charged whole
//!   ([`StorageFlags::update_element_flags_replacing_on_owner_change`]).
//! - **With a partial batch**: the first merge, except that an update that
//!   changes the size is refused when the two owners differ, unless it
//!   shrinks an element holding bytes of one epoch only, and that one that
//!   keeps the size stores the flags as written.
//!
//! All divide freed bytes with [`StorageFlags::split_removal_bytes`]: the
//! latest epochs first, each down to the three bytes its own entry in the
//! flags takes, and what is left to the epoch the element was created in.
//! [`Write::SectionalDelete`] and [`Write::SectionalDeleteUpTree`] pass the
//! same callback to the two delete methods that take one, which Drive
//! itself does not call: it deletes through batches.

#![expect(
    clippy::result_large_err,
    reason = "an operation fails with GroveDB's own error, whatever its size"
)]

use std::collections::BTreeSet;

use grovedb::{
    batch::{BatchApplyOptions, OpsByLevelPath, QualifiedGroveDbOp, SubelementsDeletionBehavior},
    operations::{
        delete::{ClearOptions, DeleteOptions, DeleteUpTreeOptions},
        insert::InsertOptions,
        proof::ProveOptions,
        QueryItemOrSumReturnType,
    },
    query_result_type::{PathKeyOptionalElementTrio, QueryResultElements, QueryResultType},
    replication::CURRENT_STATE_SYNC_VERSION,
    AggregateSumQueryOptions, AggregateSumQueryResult, BackwardsReferences, Element, ElementFlags,
    Error, PathBranchChunkQuery, PathQuery, PathQueryRun, PathTrunkChunkQuery,
    PendingPrefixDropsReport, RangePage, SizedQuery, TreeType,
};
use grovedb_commitment_tree::{deserialize_ciphertext, DashMemo};
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::{
        removal::StorageRemovedBytes,
        transition::{ElementFlagsUpdate, OperationStorageTransitionType},
        StorageCost,
    },
    CostResult, CostsExt, OperationCost,
};
use grovedb_epoch_based_storage_flags::{
    error::StorageFlagsError, MergingOwnersStrategy, StorageFlags,
};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::version::GroveVersion;

use super::op::{
    AppendEntry, BatchOp, ChunkIds, Fetch, IndexedTree, Op, PartialBatch, Prove, Query, QueryItem,
    Read, TransactionOp, TreeRead, Write,
};
// Used where `GroveDb` has no savepoint methods of its own: the engine the
// corpus is pinned to.
#[allow(unused_imports)]
use super::pinned::Savepoints as _;
// The one engine there is where `GroveDb` is not generic over its engine.
#[cfg(not(grovedb_engine_seam))]
use super::pinned::Upstream as E;
use super::pinned::{Db, DefaultEngine, StreamEngine, Tx, TxArg};

// The RocksDB engine's transactions say what they are themselves.
#[cfg(grovedb_engine_seam)]
impl StreamEngine for grovedb::RocksDbEngine {
    fn is_snapshot_read(transaction: &Tx<'_, Self>) -> bool {
        transaction.is_snapshot_read()
    }
}

/// What applying an [`Op`] produced.
#[derive(Debug)]
pub enum Outcome {
    /// A write or a batch was applied, and GroveDB returned nothing for
    /// it.
    Written,
    /// The element a [`Read::Get`] returned.
    Element(Element),
    /// The rows a [`Read::Query`] returned, each with its path and key.
    Rows(QueryResultElements),
    /// The op was [`Op::Commit`]: the caller commits its transaction.
    EndOfBlock,
    /// An [`Op::PartialBatch`] or an [`Op::FlagsPartialBatch`] was applied.
    WrittenInSegments {
        /// The cost GroveDB reported to its caller when the first segment
        /// paused: what the call had cost so far, the first segment's
        /// pending writes included.
        first_segment: OperationCost,
    },
    /// What [`Op::FlushPendingDrops`] reclaimed.
    Reclaimed(PendingPrefixDropsReport),
    /// What a [`Read::PathQuery`] returned, in the form of its shape, or the
    /// rows a [`Read::Rows`] returned with how many its offset skipped.
    Run(PathQueryRun),
    /// What a [`Read::SumBudget`] returned.
    Sums(AggregateSumQueryResult),
    /// The record a [`Read::GetAux`] returned, if there is one.
    Aux(Option<Vec<u8>>),
    /// The records a [`Read::AuxByPrefix`] returned, in key order.
    AuxEntries(Vec<(Vec<u8>, Vec<u8>)>),
    /// Whether a conditional write wrote or deleted, or what a check
    /// answered. A [`Write::DeleteBuilt`] always deletes: the flag says
    /// that GroveDB built a delete, which it refuses to do for a key it
    /// cannot delete.
    Flag(bool),
    /// The element a [`Write::InsertIfNotExists`] found in the way, or the
    /// one a [`Fetch::RawOptional`] found, if there is one.
    Existing(Option<Element>),
    /// What a [`Write::InsertIfChanged`] did.
    Changed {
        /// Whether the element was stored.
        changed: bool,
        /// The element it replaced, if it was stored over one.
        previous: Option<Element>,
    },
    /// How many keys a [`Write::DeleteUpTree`] or a
    /// [`Write::SectionalDeleteUpTree`] deleted.
    Deleted(u16),
    /// What an append through a typed method returned: a
    /// [`Write::Append`] other than of a commitment, which goes through a
    /// batch, or a [`Write::CommitmentInsert`].
    Appended {
        /// The root of the tree after the append. For a commitment tree it
        /// is the Sinsemilla root.
        root: [u8; 32],
        /// The position the value was stored at.
        position: u64,
    },
    /// The whole transaction, or what followed the newest savepoint, was
    /// rolled back.
    RolledBack,
    /// A savepoint was set.
    SavepointSet,
    /// The op was [`TransactionOp::Abandon`]: the caller drops its
    /// transaction and starts another.
    Abandoned,
    /// The op was [`TransactionOp::OpenSnapshot`]: the caller starts a
    /// snapshot read transaction.
    SnapshotOpens,
    /// The op was [`TransactionOp::CloseSnapshot`]: the caller drops its
    /// snapshot read transaction.
    SnapshotCloses,
    /// The value a typed read found at a position, if there is one.
    Value(Option<Vec<u8>>),
    /// The values a [`TreeRead::BulkBuffer`] returned.
    Values(Vec<Vec<u8>>),
    /// A root hash or an anchor of an append-only tree.
    Hash([u8; 32]),
    /// A count of an append-only tree.
    Count(u64),
    /// The page a typed range read returned.
    Page(RangePage),
    /// The rows a [`Read::KeysOptional`] returned: one for every key asked
    /// for.
    OptionalRows(Vec<PathKeyOptionalElementTrio>),
    /// What a [`Read::ItemValues`] returned.
    ItemValues {
        /// The values, in the order of the query.
        values: Vec<Vec<u8>>,
        /// How many rows the offset skipped.
        skipped: u16,
    },
    /// What a [`Read::ItemValues`] returned when it asked for sums as well.
    ItemValuesOrSums {
        /// The values and sums, in the order of the query.
        values: Vec<QueryItemOrSumReturnType>,
        /// How many rows the offset skipped.
        skipped: u16,
    },
    /// What a [`Read::SumValues`] returned.
    SumValues {
        /// The sums, in the order of the query.
        sums: Vec<i64>,
        /// How many rows the offset skipped.
        skipped: u16,
    },
    /// What a [`Read::BigSum`] returned for a range.
    BigSum {
        /// The count of the range, when it was asked for.
        count: Option<u64>,
        /// The sum of the range.
        sum: i128,
    },
    /// What a [`Read::BigSum`] returned for each selected key: the key, the
    /// count when it was asked for, and the sum.
    BigSumPerKey(Vec<(Vec<u8>, Option<u64>, i128)>),
    /// The proof bytes a [`Read::Prove`] returned.
    Proof(Vec<u8>),
}

/// How [`Write::Insert`] is applied: an insert replaces a value but not a
/// tree.
pub const INSERT_OPTIONS: InsertOptions = InsertOptions {
    validate_insertion_does_not_override: false,
    validate_insertion_does_not_override_tree: true,
    base_root_storage_is_free: true,
    backwards_references: BackwardsReferences::Check,
};

/// How [`Write::Delete`] and [`Write::SectionalDelete`] are applied: a tree
/// is deleted with everything under it.
pub const DELETE_OPTIONS: DeleteOptions = DeleteOptions {
    allow_deleting_non_empty_trees: true,
    deleting_non_empty_trees_returns_error: true,
    base_root_storage_is_free: true,
    validate_tree_at_path_exists: false,
    backwards_references: BackwardsReferences::Check,
};

/// How [`Write::ClearSubtree`] is applied: the subtrees under the cleared
/// tree go with it.
pub const CLEAR_OPTIONS: ClearOptions = ClearOptions {
    check_for_subtrees: true,
    allow_deleting_subtrees: true,
    trying_to_clear_with_subtrees_returns_error: true,
    backwards_references: BackwardsReferences::Check,
};

/// How [`Op::Batch`], [`Op::FlagsBatch`] and [`Op::FlagsHandoverBatch`]
/// are applied. They are the options Drive applies a batch with when it
/// does not ask for inserts to be checked against what is stored, except
/// that the operations are checked for consistency, which Drive's default
/// configuration leaves out of a whole batch. The check refuses a batch or
/// lets it pass, and changes no cost.
pub const BATCH_OPTIONS: BatchApplyOptions = BatchApplyOptions {
    validate_insertion_does_not_override: false,
    validate_insertion_does_not_override_tree: false,
    disable_operation_consistency_check: false,
    base_root_storage_is_free: true,
    batch_pause_height: None,
};

/// How a [`Write::DeleteUpTree`] and a [`Write::SectionalDeleteUpTree`] are
/// applied, each with its stop height: the key
/// may hold a tree only if the tree is empty, and as in Drive nothing it
/// deletes takes part in a backward reference.
pub const DELETE_UP_TREE_OPTIONS: DeleteUpTreeOptions = DeleteUpTreeOptions {
    allow_deleting_non_empty_trees: false,
    deleting_non_empty_trees_returns_error: true,
    base_root_storage_is_free: true,
    validate_tree_at_path_exists: false,
    stop_path_height: None,
    backwards_references: BackwardsReferences::DontCheck,
};

/// How a [`Write::DeleteBuilt`] builds its delete: the key may hold a tree
/// only if the tree is empty, and as in Drive it takes part in no backward
/// reference.
pub const DELETE_BUILT_OPTIONS: DeleteOptions = DeleteOptions {
    allow_deleting_non_empty_trees: false,
    deleting_non_empty_trees_returns_error: true,
    base_root_storage_is_free: true,
    validate_tree_at_path_exists: false,
    backwards_references: BackwardsReferences::DontCheck,
};

/// How a [`Read::Prove`] is proven.
pub const PROVE_OPTIONS: ProveOptions = ProveOptions {
    decrease_limit_on_empty_sub_query_result: true,
};

/// How a [`Read::PathQuery`] is run: the same three switches a
/// [`Read::Query`] is run with.
const ALLOW_CACHE: bool = true;
const DECREASE_LIMIT_ON_EMPTY_SUBQUERY: bool = true;
const ERROR_IF_PATH_NOT_PRESENT: bool = true;

/// The form query rows are returned in.
const RESULT_TYPE: QueryResultType = QueryResultType::QueryPathKeyElementTrioResultType;

/// Applies one operation inside `transaction`.
///
/// Every option GroveDB takes is spelled out in the constants above, so a
/// change to one of GroveDB's defaults cannot change how a stream replays.
pub fn apply<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    op: &Op,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> CostResult<Outcome, Error> {
    match op {
        Op::Write(write) => apply_write(db, write, transaction, grove_version),
        Op::Batch(ops) => db
            .apply_batch(
                ops.iter().map(BatchOp::to_grove_op).collect(),
                Some(BATCH_OPTIONS),
                transaction,
                grove_version,
            )
            .map_ok(|()| Outcome::Written),
        Op::Read(read) => apply_read(db, read, transaction, grove_version),
        Op::Commit => Ok(Outcome::EndOfBlock).wrap_with_cost(OperationCost::default()),
        Op::PartialBatch(batch) => {
            apply_partial_batch(db, batch, Callbacks::GroveDb, transaction, grove_version)
        }
        Op::FlagsBatch(ops) => db
            .apply_batch_with_element_flags_update(
                ops.iter().map(drive_op).collect(),
                Some(BATCH_OPTIONS),
                merge_flags,
                split_removed_bytes,
                transaction,
                grove_version,
            )
            .map_ok(|()| Outcome::Written),
        Op::FlagsPartialBatch(batch) => {
            apply_partial_batch(db, batch, Callbacks::Drive, transaction, grove_version)
        }
        Op::FlagsHandoverBatch(batch) => db
            .apply_batch_with_element_flags_update(
                batch.ops.iter().map(drive_op).collect(),
                Some(BATCH_OPTIONS),
                |cost, stored, written| {
                    merge_flags_for_owner_change(cost, stored, written, &batch.full_handover_owners)
                        .map_err(|error| {
                            Error::JustInTimeElementFlagsClientError(error.to_string())
                        })
                },
                split_removed_bytes,
                transaction,
                grove_version,
            )
            .map_ok(|()| Outcome::Written),
        Op::FlushPendingDrops => db
            .flush_pending_prefix_drops(grove_version)
            .map(Outcome::Reclaimed)
            .wrap_with_cost(OperationCost::default()),
        Op::Transaction(step) => apply_transaction_op(db, step, transaction, grove_version),
    }
}

fn free<T>(result: Result<T, Error>) -> CostResult<T, Error> {
    result.wrap_with_cost(OperationCost::default())
}

fn refused<T>(reason: &'static str) -> CostResult<T, Error> {
    free(Err(Error::InvalidInput(reason)))
}

fn apply_transaction_op<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    step: &TransactionOp,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> CostResult<Outcome, Error> {
    // A snapshot read transaction is no block's transaction: it holds no
    // write to roll back and reads an older state.
    let open = transaction
        .filter(|transaction| !E::is_snapshot_read(transaction))
        .ok_or(Error::InvalidInput(
            "a step on the block's transaction needs that transaction",
        ));
    match step {
        TransactionOp::Rollback => free(
            open.and_then(|transaction| db.rollback_transaction(transaction))
                .map(|()| Outcome::RolledBack),
        ),
        TransactionOp::SetSavepoint => free(
            open.and_then(|transaction| db.set_savepoint(transaction))
                .map(|()| Outcome::SavepointSet),
        ),
        TransactionOp::RollbackToSavepoint => free(
            open.and_then(|transaction| db.rollback_to_savepoint(transaction))
                .map(|()| Outcome::RolledBack),
        ),
        TransactionOp::Abandon => free(Ok(Outcome::Abandoned)),
        TransactionOp::OpenSnapshot => free(Ok(Outcome::SnapshotOpens)),
        TransactionOp::CloseSnapshot => free(Ok(Outcome::SnapshotCloses)),
        TransactionOp::SnapshotRead(read) => {
            if transaction.is_some_and(|transaction| E::is_snapshot_read(transaction)) {
                apply_read(db, read, transaction, grove_version)
            } else {
                refused("a snapshot read runs in a snapshot read transaction")
            }
        }
        TransactionOp::BlockRead(read) => match open {
            Ok(block) => apply_read(db, read, Some(block), grove_version),
            Err(error) => free(Err(error)),
        },
    }
}

/// The transactions a stream runs in: the transaction of the block and,
/// while one is open, a snapshot read transaction beside it.
///
/// [`Transactions::apply`] applies an operation in the transaction it
/// belongs to and then does what its outcome asks of the caller of
/// [`apply`], so a consumer that keeps its transactions here cannot handle
/// one of them differently from another consumer.
pub struct Transactions<'db, E: StreamEngine = DefaultEngine> {
    db: &'db Db<E>,
    block: Option<Tx<'db, E>>,
    snapshot: Option<Tx<'db, E>>,
}

impl<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine> Transactions<'db, E> {
    /// A first block's transaction on `db`, and no snapshot.
    pub fn new(db: &'db Db<E>) -> Self {
        Self {
            db,
            block: Some(db.start_transaction()),
            snapshot: None,
        }
    }

    /// The transaction of the open block.
    pub fn block(&self) -> &Tx<'db, E> {
        self.block
            .as_ref()
            .expect("a block's transaction is open between two operations")
    }

    /// The snapshot read transaction, while one is open.
    pub fn snapshot(&self) -> Option<&Tx<'db, E>> {
        self.snapshot.as_ref()
    }

    /// Applies one operation.
    ///
    /// The cost of an [`Op::Commit`] is the cost of the commit. A commit
    /// that fails is the error returned, and the next block has started all
    /// the same.
    pub fn apply(&mut self, op: &Op, grove_version: &GroveVersion) -> CostResult<Outcome, Error> {
        let transaction = match op {
            Op::Transaction(TransactionOp::SnapshotRead(_)) => match &self.snapshot {
                Some(snapshot) => snapshot,
                None => return refused("a snapshot read needs an open snapshot"),
            },
            _ => self.block(),
        };
        let mut cost = OperationCost::default();
        let outcome = cost_return_on_error!(
            &mut cost,
            apply(self.db, op, Some(transaction), grove_version)
        );
        match outcome {
            Outcome::EndOfBlock => {
                if let Some(block) = self.block.take() {
                    let committed = self.db.commit_transaction(block);
                    self.block = Some(self.db.start_transaction());
                    cost_return_on_error!(&mut cost, committed);
                }
            }
            Outcome::Abandoned => {
                drop(self.block.take());
                self.block = Some(self.db.start_transaction());
            }
            Outcome::SnapshotOpens => {
                self.snapshot = Some(self.db.start_snapshot_read_transaction());
            }
            Outcome::SnapshotCloses => self.snapshot = None,
            _ => {}
        }
        Ok(outcome).wrap_with_cost(cost)
    }
}

/// The flags update Drive applies a batch with.
fn merge_flags(
    cost: &StorageCost,
    stored: Option<ElementFlags>,
    written: &mut ElementFlags,
) -> Result<bool, Error> {
    StorageFlags::update_element_flags(cost, stored, written)
        .map_err(|error| Error::JustInTimeElementFlagsClientError(error.to_string()))
}

/// The flags update Drive applies a partial batch with.
fn merge_flags_of_one_owner(
    cost: &StorageCost,
    stored: Option<ElementFlags>,
    written: &mut ElementFlags,
) -> Result<bool, Error> {
    let refused = |what: &str, error: &dyn std::fmt::Display| {
        Error::JustInTimeElementFlagsClientError(format!("{what} {error}"))
    };
    // An element stored without flags takes the flags written.
    let Some(stored) = stored else {
        return Ok(false);
    };
    let stored = StorageFlags::from_element_flags_ref(&stored).map_err(|error| {
        refused(
            "drive did not understand flags of old item being updated",
            &error,
        )
    })?;
    let new = StorageFlags::from_element_flags_ref(written)
        .map_err(|error| {
            refused(
                "drive did not understand updated item flag information",
                &error,
            )
        })?
        .ok_or_else(|| {
            Error::JustInTimeElementFlagsClientError(
                "removing flags from an item with flags is not allowed".to_string(),
            )
        })?;
    let (merged, what) = match cost.transition_type() {
        OperationStorageTransitionType::OperationUpdateBiggerSize => (
            StorageFlags::optional_combine_added_bytes(
                stored,
                new,
                cost.added_bytes,
                MergingOwnersStrategy::RaiseIssue,
            ),
            "drive could not combine storage flags (new flags were bigger):",
        ),
        OperationStorageTransitionType::OperationUpdateSmallerSize => (
            StorageFlags::optional_combine_removed_bytes(
                stored,
                new,
                &cost.removed_bytes,
                MergingOwnersStrategy::RaiseIssue,
            ),
            "drive could not combine storage flags (new flags were smaller):",
        ),
        _ => return Ok(false),
    };
    let merged = merged
        .map_err(|error| refused(what, &error))?
        .to_element_flags();
    if merged == *written {
        return Ok(false);
    }
    *written = merged;
    Ok(true)
}

/// The flags update Drive applies a batch that updates a document with,
/// given the owners a handover to or from which is a full one.
fn merge_flags_for_owner_change(
    cost: &StorageCost,
    stored: Option<ElementFlags>,
    written: &mut ElementFlags,
    full_handover_owners: &BTreeSet<[u8; 32]>,
) -> Result<ElementFlagsUpdate, StorageFlagsError> {
    let names_full_handover_owner = |flags: &ElementFlags| -> Result<bool, StorageFlagsError> {
        Ok(
            StorageFlags::from_element_flags_ref(flags)?.is_some_and(|flags| {
                flags
                    .owner_id()
                    .is_some_and(|owner| full_handover_owners.contains(owner))
            }),
        )
    };
    let is_full_handover = !full_handover_owners.is_empty()
        && (names_full_handover_owner(written)?
            || stored
                .as_ref()
                .map(names_full_handover_owner)
                .transpose()?
                .unwrap_or(false));
    if is_full_handover {
        StorageFlags::update_element_flags_replacing_on_owner_change(cost, stored, written)
    } else {
        StorageFlags::update_element_flags_with_written_owner(cost, stored, written)
            .map(ElementFlagsUpdate::from)
    }
}

/// The split of freed bytes Drive applies a batch with.
fn split_removed_bytes(
    flags: &mut ElementFlags,
    removed_key_bytes: u32,
    removed_value_bytes: u32,
) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error> {
    StorageFlags::split_removal_bytes(flags, removed_key_bytes, removed_value_bytes)
        .map_err(|error| Error::SplitRemovalBytesClientError(error.to_string()))
}

/// The split of freed bytes Drive applies a partial batch with: the same
/// split, and another text where it cannot read the flags.
fn split_removed_bytes_of_a_partial_batch(
    flags: &mut ElementFlags,
    removed_key_bytes: u32,
    removed_value_bytes: u32,
) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error> {
    let flags = StorageFlags::from_element_flags_ref(flags).map_err(|error| {
        Error::SplitRemovalBytesClientError(format!(
            "drive did not understand flags of item being updated: {error}"
        ))
    })?;
    Ok(match flags {
        None => (
            StorageRemovedBytes::BasicStorageRemoval(removed_key_bytes),
            StorageRemovedBytes::BasicStorageRemoval(removed_value_bytes),
        ),
        Some(flags) => flags.split_storage_removed_bytes(removed_key_bytes, removed_value_bytes),
    })
}

/// The GroveDB batch operation Drive builds for `op`: one that declares
/// the value it replaces or deletes to take no part in a backward
/// reference.
fn drive_op(op: &BatchOp) -> QualifiedGroveDbOp {
    op.to_grove_op().dont_check_for_backwards_references()
}

/// The callbacks a partial batch is applied with.
#[derive(Clone, Copy)]
enum Callbacks {
    /// GroveDB's own: [`Op::PartialBatch`].
    GroveDb,
    /// The ones Drive passes: [`Op::FlagsPartialBatch`].
    Drive,
}

fn apply_partial_batch<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    batch: &PartialBatch,
    callbacks: Callbacks,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> CostResult<Outcome, Error> {
    let mut first_segment = OperationCost::default();
    let grove_op = match callbacks {
        Callbacks::GroveDb => BatchOp::to_grove_op,
        Callbacks::Drive => drive_op,
    };
    let ops = batch.ops.iter().map(grove_op).collect();
    let options = Some(BatchApplyOptions {
        batch_pause_height: batch.pause_height,
        ..BATCH_OPTIONS
    });
    let add_on = |cost: &OperationCost, _left_over: &Option<OpsByLevelPath>| {
        first_segment = cost.clone();
        Ok(batch.add_on.iter().map(grove_op).collect())
    };
    match callbacks {
        Callbacks::GroveDb => {
            db.apply_partial_batch(ops, options, add_on, transaction, grove_version)
        }
        Callbacks::Drive => db.apply_partial_batch_with_element_flags_update(
            ops,
            options,
            merge_flags_of_one_owner,
            split_removed_bytes_of_a_partial_batch,
            add_on,
            transaction,
            grove_version,
        ),
    }
    .map_ok(|()| Outcome::WrittenInSegments { first_segment })
}

/// Runs `path_query` with `GroveDb::query_raw` when `raw` is set, otherwise
/// with `GroveDb::query`, and returns the rows with how many the offset
/// skipped.
fn query_rows<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    path_query: &PathQuery,
    raw: bool,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> CostResult<(QueryResultElements, u16), Error> {
    if raw {
        db.query_raw(
            path_query,
            ALLOW_CACHE,
            DECREASE_LIMIT_ON_EMPTY_SUBQUERY,
            ERROR_IF_PATH_NOT_PRESENT,
            RESULT_TYPE,
            transaction,
            grove_version,
        )
    } else {
        db.query(
            path_query,
            ALLOW_CACHE,
            DECREASE_LIMIT_ON_EMPTY_SUBQUERY,
            ERROR_IF_PATH_NOT_PRESENT,
            RESULT_TYPE,
            transaction,
            grove_version,
        )
    }
}

fn apply_read<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    read: &Read,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> CostResult<Outcome, Error> {
    match read {
        Read::Get { path, key } => db
            .get(path.as_slice(), key, transaction, grove_version)
            .map_ok(Outcome::Element),
        Read::Query(query) => query_rows(
            db,
            &query.to_path_query(),
            query.raw,
            transaction,
            grove_version,
        )
        .map_ok(|(rows, _skipped)| Outcome::Rows(rows)),
        Read::Rows { query, raw } => query_rows(db, query, *raw, transaction, grove_version)
            .map_ok(|(elements, skipped)| {
                Outcome::Run(PathQueryRun::Elements { elements, skipped })
            }),
        Read::PathQuery { query, .. } => db
            .run_path_query(
                query,
                ALLOW_CACHE,
                DECREASE_LIMIT_ON_EMPTY_SUBQUERY,
                ERROR_IF_PATH_NOT_PRESENT,
                RESULT_TYPE,
                transaction,
                grove_version,
            )
            .map_ok(Outcome::Run),
        Read::SumBudget {
            query,
            skip_non_sum_items,
            ignore_references,
        } => db
            .query_aggregate_sums_with_options(
                query,
                AggregateSumQueryOptions {
                    allow_cache: ALLOW_CACHE,
                    error_if_intermediate_path_tree_not_present: ERROR_IF_PATH_NOT_PRESENT,
                    error_if_non_sum_item_found: !skip_non_sum_items,
                    ignore_references: *ignore_references,
                },
                transaction,
                grove_version,
            )
            .map_ok(Outcome::Sums),
        Read::GetAux { key } => db.get_aux(key, transaction).map_ok(Outcome::Aux),
        Read::AuxByPrefix { prefix } => db
            .get_aux_by_key_prefix(prefix, transaction)
            .map_ok(Outcome::AuxEntries),
        Read::HasRaw { path, key } => db
            .has_raw(path.as_slice(), key, transaction, grove_version)
            .map_ok(Outcome::Flag),
        Read::IsEmptyTree { path } => db
            .is_empty_tree(path.as_slice(), transaction, grove_version)
            .map_ok(Outcome::Flag),
        Read::Fetch {
            path,
            key,
            fetch,
            cache,
        } => {
            let path = path.as_slice().into();
            match (fetch, cache) {
                (Fetch::Resolved, _) => db
                    .get_caching_optional(path, key, *cache, transaction, grove_version)
                    .map_ok(Outcome::Element),
                (Fetch::Raw, true) => db
                    .get_raw(path, key, transaction, grove_version)
                    .map_ok(Outcome::Element),
                (Fetch::Raw, false) => db
                    .get_raw_caching_optional(path, key, false, transaction, grove_version)
                    .map_ok(Outcome::Element),
                (Fetch::RawOptional, true) => db
                    .get_raw_optional(path, key, transaction, grove_version)
                    .map_ok(Outcome::Existing),
                (Fetch::RawOptional, false) => db
                    .get_raw_optional_caching_optional(path, key, false, transaction, grove_version)
                    .map_ok(Outcome::Existing),
            }
        }
        Read::Tree { path, read } => apply_tree_read(db, path, *read, transaction, grove_version),
        Read::KeysOptional { query, raw } => if *raw {
            db.query_raw_keys_optional(
                query,
                ALLOW_CACHE,
                DECREASE_LIMIT_ON_EMPTY_SUBQUERY,
                ERROR_IF_PATH_NOT_PRESENT,
                transaction,
                grove_version,
            )
        } else {
            db.query_keys_optional(
                query,
                ALLOW_CACHE,
                DECREASE_LIMIT_ON_EMPTY_SUBQUERY,
                ERROR_IF_PATH_NOT_PRESENT,
                transaction,
                grove_version,
            )
        }
        .map_ok(Outcome::OptionalRows),
        Read::ItemValues {
            query,
            or_sums: false,
        } => db
            .query_item_value(
                query,
                ALLOW_CACHE,
                DECREASE_LIMIT_ON_EMPTY_SUBQUERY,
                ERROR_IF_PATH_NOT_PRESENT,
                transaction,
                grove_version,
            )
            .map_ok(|(values, skipped)| Outcome::ItemValues { values, skipped }),
        Read::ItemValues {
            query,
            or_sums: true,
        } => db
            .query_item_value_or_sum(
                query,
                ALLOW_CACHE,
                DECREASE_LIMIT_ON_EMPTY_SUBQUERY,
                ERROR_IF_PATH_NOT_PRESENT,
                transaction,
                grove_version,
            )
            .map_ok(|(values, skipped)| Outcome::ItemValuesOrSums { values, skipped }),
        Read::SumValues { query } => db
            .query_sums(
                query,
                ALLOW_CACHE,
                DECREASE_LIMIT_ON_EMPTY_SUBQUERY,
                ERROR_IF_PATH_NOT_PRESENT,
                transaction,
                grove_version,
            )
            .map_ok(|(sums, skipped)| Outcome::SumValues { sums, skipped }),
        Read::Many { queries } => db
            .query_many_raw(
                &queries.iter().collect::<Vec<_>>(),
                ALLOW_CACHE,
                DECREASE_LIMIT_ON_EMPTY_SUBQUERY,
                ERROR_IF_PATH_NOT_PRESENT,
                RESULT_TYPE,
                transaction,
                grove_version,
            )
            .map_ok(Outcome::Rows),
        Read::BigSum {
            query,
            with_count,
            per_key,
        } => match (with_count, per_key) {
            (false, false) => db
                .query_aggregate_big_sum(query, transaction, grove_version)
                .map_ok(|sum| Outcome::BigSum { count: None, sum }),
            (true, false) => db
                .query_aggregate_count_and_big_sum(query, transaction, grove_version)
                .map_ok(|(count, sum)| Outcome::BigSum {
                    count: Some(count),
                    sum,
                }),
            (false, true) => db
                .query_aggregate_big_sum_per_key(query, transaction, grove_version)
                .map_ok(|per_key| {
                    let per_key = per_key.into_iter().map(|(key, sum)| (key, None, sum));
                    Outcome::BigSumPerKey(per_key.collect())
                }),
            (true, true) => db
                .query_aggregate_count_and_big_sum_per_key(query, transaction, grove_version)
                .map_ok(|per_key| {
                    let per_key = per_key
                        .into_iter()
                        .map(|(key, count, sum)| (key, Some(count), sum));
                    Outcome::BigSumPerKey(per_key.collect())
                }),
        },
        Read::Prove(prove) => {
            apply_prove(db, prove, transaction, grove_version).map_ok(Outcome::Proof)
        }
    }
}

/// The subtree holding a tree and the tree's key there, from the path of
/// the tree itself.
fn tree_at(path: &[Vec<u8>]) -> Result<(&[Vec<u8>], &Vec<u8>), Error> {
    let (key, parent) = path.split_last().ok_or(Error::InvalidInput(
        "an operation on a tree needs the path of the tree",
    ))?;
    Ok((parent, key))
}

fn apply_tree_read<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    path: &[Vec<u8>],
    read: TreeRead,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> CostResult<Outcome, Error> {
    let (parent, key) = match tree_at(path) {
        Ok(tree) => tree,
        Err(error) => return free(Err(error)),
    };
    match read {
        TreeRead::MmrValue(index) => db
            .mmr_tree_get_value(parent, key, index, transaction, grove_version)
            .map_ok(Outcome::Value),
        TreeRead::MmrRootHash => db
            .mmr_tree_root_hash(parent, key, transaction, grove_version)
            .map_ok(Outcome::Hash),
        TreeRead::MmrLeafCount => db
            .mmr_tree_leaf_count(parent, key, transaction, grove_version)
            .map_ok(Outcome::Count),
        TreeRead::BulkValue(position) => db
            .bulk_get_value(parent, key, position, transaction, grove_version)
            .map_ok(Outcome::Value),
        TreeRead::BulkRange { start, limit } => db
            .bulk_get_range(parent, key, start, limit, transaction, grove_version)
            .map_ok(Outcome::Page),
        TreeRead::BulkChunk(index) => db
            .bulk_get_chunk(parent, key, index, transaction, grove_version)
            .map_ok(Outcome::Value),
        TreeRead::BulkBuffer => db
            .bulk_get_buffer(parent, key, transaction, grove_version)
            .map_ok(Outcome::Values),
        TreeRead::BulkCount => db
            .bulk_count(parent, key, transaction, grove_version)
            .map_ok(Outcome::Count),
        TreeRead::BulkChunkCount => db
            .bulk_chunk_count(parent, key, transaction, grove_version)
            .map_ok(Outcome::Count),
        TreeRead::DenseValue(position) => db
            .dense_tree_get(parent, key, position, transaction, grove_version)
            .map_ok(Outcome::Value),
        TreeRead::DenseRootHash => db
            .dense_tree_root_hash(parent, key, transaction, grove_version)
            .map_ok(Outcome::Hash),
        TreeRead::DenseCount => db
            .dense_tree_count(parent, key, transaction, grove_version)
            .map_ok(|count| Outcome::Count(u64::from(count))),
        TreeRead::CommitmentValue(position) => db
            .commitment_tree_get_value(parent, key, position, transaction, grove_version)
            .map_ok(Outcome::Value),
        TreeRead::CommitmentRange { start, limit } => db
            .commitment_tree_get_range(parent, key, start, limit, transaction, grove_version)
            .map_ok(Outcome::Page),
        TreeRead::CommitmentAnchor => db
            .commitment_tree_anchor(parent, key, transaction, grove_version)
            .map_ok(|anchor| Outcome::Hash(anchor.to_bytes())),
        TreeRead::CommitmentCount => db
            .commitment_tree_count(parent, key, transaction, grove_version)
            .map_ok(Outcome::Count),
        TreeRead::PrivateDocumentValue(position) => db
            .private_document_store_get_value(parent, key, position, transaction, grove_version)
            .map_ok(Outcome::Value),
        TreeRead::PrivateDocumentRange { start, limit } => db
            .private_document_store_get_range(parent, key, start, limit, transaction, grove_version)
            .map_ok(Outcome::Page),
        TreeRead::PrivateDocumentCount => db
            .private_document_store_count(parent, key, transaction, grove_version)
            .map_ok(Outcome::Count),
    }
}

fn apply_prove<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    prove: &Prove,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> CostResult<Vec<u8>, Error> {
    match prove {
        Prove::PathQuery(query) => {
            db.get_proved_path_query(query, Some(PROVE_OPTIONS), None, grove_version)
        }
        Prove::Many(queries) => {
            db.prove_query_many(queries.iter().collect(), Some(PROVE_OPTIONS), grove_version)
        }
        Prove::BulkPositionRange {
            path,
            key,
            start,
            limit,
        } => db.prove_bulk_position_range(
            path.clone(),
            key,
            *start,
            *limit,
            Some(PROVE_OPTIONS),
            grove_version,
        ),
        Prove::TrunkChunk {
            path,
            max_depth,
            min_depth,
        } => db.prove_trunk_chunk(
            &PathTrunkChunkQuery {
                path: path.clone(),
                max_depth: *max_depth,
                min_depth: *min_depth,
            },
            grove_version,
        ),
        Prove::BranchChunk { path, key, depth } => db.prove_branch_chunk(
            &PathBranchChunkQuery {
                path: path.clone(),
                key: key.clone(),
                depth: *depth,
            },
            grove_version,
        ),
        Prove::Chunk { path, ids } => {
            let mut cost = OperationCost::default();
            let request = cost_return_on_error!(
                &mut cost,
                chunk_request(db, path, ids, transaction, grove_version)
            );
            db.fetch_chunk(
                &request,
                transaction,
                CURRENT_STATE_SYNC_VERSION,
                grove_version,
            )
            .wrap_with_cost(cost)
        }
    }
}

/// The request a syncing node sends for chunks of the subtree at `path`,
/// in the wire form `GroveDb::fetch_chunk` takes.
///
/// The root chunk of the root tree is asked for by the root hash. Any other
/// request is one global chunk id: the hash of the subtree's path, its root
/// key behind a 16-bit big-endian length, its tree type, and the local
/// chunk ids, packed. A page of entries is asked for by one local id: the
/// position it starts at and the state of the tree as two 64-bit big-endian
/// numbers, and the parameter of the tree.
///
/// GroveDB charges nothing for serving a chunk. The cost returned is that
/// of the reads that name it: the root hash, or the root key, or the
/// element of the tree.
fn chunk_request<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    path: &[Vec<u8>],
    ids: &ChunkIds,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> CostResult<Vec<u8>, Error> {
    let mut cost = OperationCost::default();
    let entries_page;
    let local_ids = match ids {
        ChunkIds::Merk(local_ids) => local_ids.as_slice(),
        ChunkIds::Entries {
            start,
            state,
            parameter,
        } => {
            let mut cursor = start.to_be_bytes().to_vec();
            cursor.extend_from_slice(&state.to_be_bytes());
            cursor.push(*parameter);
            entries_page = [cursor];
            entries_page.as_slice()
        }
    };
    let (root_key, tree_type) = match path.split_last() {
        None if local_ids.is_empty() => {
            return db
                .root_hash(transaction, grove_version)
                .map_ok(|root_hash| root_hash.to_vec())
                .add_cost(cost);
        }
        None => {
            let root_key =
                cost_return_on_error!(&mut cost, db.root_key(transaction, grove_version));
            (root_key, TreeType::NormalTree)
        }
        Some((key, parent)) => {
            let element = cost_return_on_error!(
                &mut cost,
                db.get_raw(parent.into(), key, transaction, grove_version)
            );
            cost_return_on_error_no_add!(
                cost,
                element
                    .root_key_and_tree_type_owned()
                    .ok_or(Error::InvalidInput("a chunk is served from a tree"))
            )
        }
    };
    let root_key = root_key.unwrap_or_default();
    let root_key_len = cost_return_on_error_no_add!(
        cost,
        u16::try_from(root_key.len())
            .map_err(|_| Error::InvalidInput("a root key is at most 65535 bytes"))
    );
    let mut global_id = RocksDbStorage::build_prefix(path.into()).value.to_vec();
    global_id.extend_from_slice(&root_key_len.to_be_bytes());
    global_id.extend_from_slice(&root_key);
    global_id.push(tree_type.discriminant());
    pack_nested_bytes(&mut global_id, local_ids);
    let mut request = Vec::new();
    pack_nested_bytes(&mut request, &[global_id]);
    Ok(request).wrap_with_cost(cost)
}

/// GroveDB's state sync packing of a list of byte strings: a 32-bit
/// big-endian count, then each string behind a 32-bit big-endian length.
fn pack_nested_bytes(out: &mut Vec<u8>, nested: &[Vec<u8>]) {
    let len = |len: usize| {
        u32::try_from(len)
            .expect("a chunk request fits in a u32")
            .to_be_bytes()
    };
    out.extend_from_slice(&len(nested.len()));
    for bytes in nested {
        out.extend_from_slice(&len(bytes.len()));
        out.extend_from_slice(bytes);
    }
}

fn apply_write<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    write: &Write,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> CostResult<Outcome, Error> {
    let written = |()| Outcome::Written;
    match write {
        Write::Insert { path, key, element } => db
            .insert(
                path.as_slice(),
                key,
                element.clone(),
                Some(INSERT_OPTIONS),
                transaction,
                grove_version,
            )
            .map_ok(written),
        Write::Delete { path, key } => db
            .delete(
                path.as_slice(),
                key,
                Some(DELETE_OPTIONS),
                transaction,
                grove_version,
            )
            .map_ok(written),
        Write::ClearSubtree { path } => db
            .clear_subtree(
                path.as_slice(),
                Some(CLEAR_OPTIONS),
                transaction,
                grove_version,
            )
            .map(|_cleared| Outcome::Written)
            .wrap_with_cost(OperationCost::default()),
        Write::Append { path, entry } => {
            let Some((key, parent)) = path.split_last() else {
                return Err(Error::InvalidPath(
                    "an append needs the path of its tree".to_string(),
                ))
                .wrap_with_cost(OperationCost::default());
            };
            let appended = |(root, position)| Outcome::Appended { root, position };
            match entry {
                AppendEntry::Mmr(value) => db
                    .mmr_tree_append(parent, key, value.clone(), transaction, grove_version)
                    .map_ok(appended),
                AppendEntry::Bulk(value) => db
                    .bulk_append(parent, key, value.clone(), transaction, grove_version)
                    .map_ok(appended),
                AppendEntry::Dense(value) => db
                    .dense_tree_insert(parent, key, value.clone(), transaction, grove_version)
                    .map_ok(|(root, position)| appended((root, u64::from(position)))),
                AppendEntry::PrivateDocument(value) => db
                    .private_document_store_insert(
                        parent,
                        key,
                        value.clone(),
                        transaction,
                        grove_version,
                    )
                    .map_ok(appended),
                AppendEntry::Commitment { .. } => db
                    .apply_batch(
                        vec![append_op(path.clone(), entry)],
                        Some(BATCH_OPTIONS),
                        transaction,
                        grove_version,
                    )
                    .map_ok(written),
            }
        }
        Write::PutAux { key, value } => db.put_aux(key, value, None, transaction).map_ok(written),
        Write::DeleteAux { key } => db.delete_aux(key, None, transaction).map_ok(written),
        Write::DropFlat { path, key } => db
            .drop_flat_subtree(
                path.as_slice(),
                key,
                BackwardsReferences::DontCheck,
                transaction,
                grove_version,
            )
            .map_ok(written),
        Write::InsertIfNotExists {
            path,
            key,
            element,
            return_existing: false,
        } => db
            .insert_if_not_exists(
                path.as_slice(),
                key,
                element.clone(),
                transaction,
                grove_version,
            )
            .map_ok(Outcome::Flag),
        Write::InsertIfNotExists {
            path,
            key,
            element,
            return_existing: true,
        } => db
            .insert_if_not_exists_return_existing_element(
                path.as_slice(),
                key,
                element.clone(),
                transaction,
                grove_version,
            )
            .map_ok(Outcome::Existing),
        Write::InsertIfChanged { path, key, element } => db
            .insert_if_changed_value(
                path.as_slice(),
                key,
                element.clone(),
                transaction,
                grove_version,
            )
            .map_ok(|(changed, previous)| Outcome::Changed { changed, previous }),
        Write::DeleteIfEmptyTree { path, key } => db
            .delete_if_empty_tree(path.as_slice(), key, transaction, grove_version)
            .map_ok(Outcome::Flag),
        Write::DeleteUpTree {
            path,
            key,
            stop_path_height,
            built,
        } => {
            let options = DeleteUpTreeOptions {
                stop_path_height: *stop_path_height,
                ..DELETE_UP_TREE_OPTIONS
            };
            if !*built {
                return db
                    .delete_up_tree_while_empty(
                        path.as_slice(),
                        key,
                        &options,
                        transaction,
                        grove_version,
                    )
                    .map_ok(Outcome::Deleted);
            }
            let mut cost = OperationCost::default();
            let deletes = cost_return_on_error!(
                &mut cost,
                db.delete_operations_for_delete_up_tree_while_empty(
                    path.as_slice().into(),
                    key,
                    &options,
                    None,
                    NOTHING_PENDING,
                    transaction,
                    grove_version,
                )
            );
            let deleted = cost_return_on_error_no_add!(
                cost,
                u16::try_from(deletes.len())
                    .map_err(|_| Error::InvalidInput("a path has at most 65535 levels"))
            );
            apply_built(db, deletes, transaction, grove_version)
                .map_ok(|()| Outcome::Deleted(deleted))
                .add_cost(cost)
        }
        Write::DeleteBuilt { path, key } => {
            let mut cost = OperationCost::default();
            let delete = cost_return_on_error!(
                &mut cost,
                db.delete_operation_for_delete_internal(
                    path.as_slice().into(),
                    key,
                    &DELETE_BUILT_OPTIONS,
                    None,
                    NOTHING_PENDING,
                    transaction,
                    grove_version,
                )
            );
            let deleted = delete.is_some();
            apply_built(db, delete.into_iter().collect(), transaction, grove_version)
                .map_ok(|()| Outcome::Flag(deleted))
                .add_cost(cost)
        }
        Write::IndexedInsert {
            path,
            key,
            element,
            tree,
        } => {
            let (path, element) = (path.as_slice(), element.clone());
            match tree {
                IndexedTree::Count => db.insert_into_count_indexed_tree(
                    path,
                    key,
                    element,
                    transaction,
                    grove_version,
                ),
                IndexedTree::Sum => db.insert_into_provable_sum_indexed_tree(
                    path,
                    key,
                    element,
                    transaction,
                    grove_version,
                ),
                IndexedTree::CountSum => db.insert_into_provable_count_provable_sum_indexed_tree(
                    path,
                    key,
                    element,
                    transaction,
                    grove_version,
                ),
                IndexedTree::BigSum => db.insert_into_provable_big_sum_indexed_tree(
                    path,
                    key,
                    element,
                    transaction,
                    grove_version,
                ),
                IndexedTree::CountBigSum => db
                    .insert_into_provable_count_provable_big_sum_indexed_tree(
                        path,
                        key,
                        element,
                        transaction,
                        grove_version,
                    ),
            }
            .map_ok(written)
        }
        Write::IndexedDelete { path, key, tree } => {
            let path = path.as_slice();
            match tree {
                IndexedTree::Count => {
                    db.delete_from_count_indexed_tree(path, key, transaction, grove_version)
                }
                IndexedTree::Sum => {
                    db.delete_from_provable_sum_indexed_tree(path, key, transaction, grove_version)
                }
                IndexedTree::CountSum => db.delete_from_provable_count_provable_sum_indexed_tree(
                    path,
                    key,
                    transaction,
                    grove_version,
                ),
                IndexedTree::BigSum => db.delete_from_provable_big_sum_indexed_tree(
                    path,
                    key,
                    transaction,
                    grove_version,
                ),
                IndexedTree::CountBigSum => db
                    .delete_from_provable_count_provable_big_sum_indexed_tree(
                        path,
                        key,
                        transaction,
                        grove_version,
                    ),
            }
            .map_ok(Outcome::Flag)
        }
        Write::ReconcileIndexed { path } => db
            .reconcile_indexed_tree_secondaries(path.as_slice(), transaction, grove_version)
            .map_ok(written),
        Write::CommitmentInsert {
            path,
            cmx,
            rho,
            cv_net,
            payload,
        } => {
            let (parent, key) = match tree_at(path) {
                Ok(tree) => tree,
                Err(error) => return free(Err(error)),
            };
            let Some(ciphertext) = deserialize_ciphertext::<DashMemo>(payload) else {
                return refused("a commitment payload is a note ciphertext");
            };
            db.commitment_tree_insert(
                parent,
                key,
                *cmx,
                *rho,
                *cv_net,
                ciphertext,
                transaction,
                grove_version,
            )
            .map_ok(|(root, position)| Outcome::Appended { root, position })
        }
        Write::SectionalDelete { path, key } => db
            .delete_with_sectional_storage_function(
                path.as_slice().into(),
                key,
                Some(DELETE_OPTIONS),
                transaction,
                &mut split_removed_bytes,
                grove_version,
            )
            .map_ok(written),
        Write::SectionalDeleteUpTree {
            path,
            key,
            stop_path_height,
        } => db
            .delete_up_tree_while_empty_with_sectional_storage(
                path.as_slice().into(),
                key,
                &DeleteUpTreeOptions {
                    stop_path_height: *stop_path_height,
                    ..DELETE_UP_TREE_OPTIONS
                },
                transaction,
                split_removed_bytes,
                grove_version,
            )
            .map_ok(Outcome::Deleted),
    }
}

/// The batch a built delete goes into holds nothing else.
const NOTHING_PENDING: &[QualifiedGroveDbOp] = &[];

/// Applies the deletes GroveDB built, as one batch. No delete is no batch.
fn apply_built<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    deletes: Vec<QualifiedGroveDbOp>,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    if deletes.is_empty() {
        return free(Ok(()));
    }
    db.apply_batch(deletes, Some(BATCH_OPTIONS), transaction, grove_version)
}

fn append_op(path: Vec<Vec<u8>>, entry: &AppendEntry) -> QualifiedGroveDbOp {
    match entry {
        AppendEntry::Mmr(value) => QualifiedGroveDbOp::mmr_tree_append_op(path, value.clone()),
        AppendEntry::Bulk(value) => QualifiedGroveDbOp::bulk_append_op(path, value.clone()),
        AppendEntry::Dense(value) => QualifiedGroveDbOp::dense_tree_insert_op(path, value.clone()),
        AppendEntry::PrivateDocument(value) => {
            QualifiedGroveDbOp::private_document_store_insert_op(path, value.clone())
        }
        AppendEntry::Commitment {
            cmx,
            rho,
            cv_net,
            payload,
        } => QualifiedGroveDbOp::commitment_tree_insert_op(
            path,
            *cmx,
            *rho,
            *cv_net,
            payload.clone(),
        ),
    }
}

impl BatchOp {
    /// The GroveDB batch operation this stands for.
    pub fn to_grove_op(&self) -> QualifiedGroveDbOp {
        match self {
            BatchOp::InsertOrReplace { path, key, element } => {
                QualifiedGroveDbOp::insert_or_replace_op(path.clone(), key.clone(), element.clone())
            }
            BatchOp::InsertOnly { path, key, element } => {
                QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op(
                    path.clone(),
                    key.clone(),
                    element.clone(),
                )
            }
            BatchOp::InsertIfNotExists { path, key, element } => {
                QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                    path.clone(),
                    key.clone(),
                    element.clone(),
                )
            }
            BatchOp::Replace { path, key, element } => {
                QualifiedGroveDbOp::replace_op(path.clone(), key.clone(), element.clone())
            }
            BatchOp::RefreshReference {
                path,
                key,
                reference,
                max_hop,
                flags,
                non_counted,
                trusted,
            } => QualifiedGroveDbOp::refresh_reference_op(
                path.clone(),
                key.clone(),
                reference.clone(),
                *max_hop,
                flags.clone(),
                *non_counted,
                *trusted,
            ),
            BatchOp::Delete { path, key } => {
                QualifiedGroveDbOp::delete_op(path.clone(), key.clone())
            }
            BatchOp::DeleteTree {
                path,
                key,
                tree_type,
            } => QualifiedGroveDbOp::delete_tree_op(
                path.clone(),
                key.clone(),
                *tree_type,
                SubelementsDeletionBehavior::DeleteChildren,
            ),
            BatchOp::Append { path, entry } => append_op(path.clone(), entry),
            BatchOp::Patch {
                path,
                key,
                element,
                change_in_bytes,
            } => QualifiedGroveDbOp::patch_op(
                path.clone(),
                key.clone(),
                element.clone(),
                *change_in_bytes,
            ),
            BatchOp::Move { path, key, new_key } => {
                QualifiedGroveDbOp::move_op(path.clone(), key.clone(), new_key.clone())
            }
            BatchOp::DropFlat {
                path,
                key,
                tree_type,
            } => QualifiedGroveDbOp::delete_tree_op(
                path.clone(),
                key.clone(),
                *tree_type,
                SubelementsDeletionBehavior::DropFlat,
            )
            .dont_check_for_backwards_references(),
        }
    }
}

impl Query {
    /// The GroveDB path query this stands for.
    pub fn to_path_query(&self) -> PathQuery {
        let mut query = grovedb::Query::new_with_direction(self.left_to_right);
        for item in &self.items {
            query.insert_item(item.to_grove_item());
        }
        if let Some(path) = &self.subquery_path {
            query.set_subquery_path(path.clone());
        }
        if self.subquery_all {
            query.set_subquery(grovedb::Query::new_range_full());
        }
        PathQuery::new(self.path.clone(), SizedQuery::new(query, self.limit, None))
    }
}

impl QueryItem {
    fn to_grove_item(&self) -> grovedb::QueryItem {
        match self {
            QueryItem::Key(key) => grovedb::QueryItem::Key(key.clone()),
            QueryItem::Range(start, end) => grovedb::QueryItem::Range(start.clone()..end.clone()),
            QueryItem::RangeInclusive(start, end) => {
                grovedb::QueryItem::RangeInclusive(start.clone()..=end.clone())
            }
            QueryItem::RangeFull => grovedb::QueryItem::RangeFull(..),
            QueryItem::RangeFrom(start) => grovedb::QueryItem::RangeFrom(start.clone()..),
            QueryItem::RangeTo(end) => grovedb::QueryItem::RangeTo(..end.clone()),
            QueryItem::RangeAfter(start) => grovedb::QueryItem::RangeAfter(start.clone()..),
        }
    }
}
