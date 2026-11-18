//! Applying workload operations to a GroveDB.
//!
//! This is the one place that turns an [`Op`] into GroveDB calls, so a
//! recorder, a replayer and a benchmark all drive the database the same way.
//! Block handling stays with the caller: [`apply`] runs inside the
//! transaction it is given and reports [`Outcome::EndOfBlock`] for
//! [`Op::Commit`] without committing anything. The one operation that does
//! not run inside it is [`Op::FlushPendingDrops`], which GroveDB only offers
//! on the database itself.

use grovedb::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp, SubelementsDeletionBehavior},
    operations::{
        delete::{ClearOptions, DeleteOptions},
        insert::InsertOptions,
    },
    query_result_type::{QueryResultElements, QueryResultType},
    AggregateSumQueryOptions, AggregateSumQueryResult, BackwardsReferences, Element, Error,
    GroveDb, PathQuery, PathQueryRun, PendingPrefixDropsReport, SizedQuery, TransactionArg,
};
use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use super::op::{AppendEntry, BatchOp, Op, PartialBatch, Query, QueryItem, Read, Write};

/// What applying an [`Op`] produced.
#[derive(Debug)]
pub enum Outcome {
    /// A write or a batch was applied.
    Written,
    /// The element a [`Read::Get`] returned.
    Element(Element),
    /// The rows a [`Read::Query`] returned, each with its path and key.
    Rows(QueryResultElements),
    /// The op was [`Op::Commit`]: the caller commits its transaction.
    EndOfBlock,
    /// An [`Op::PartialBatch`] was applied.
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
}

/// How [`Write::Insert`] is applied: an insert replaces a value but not a
/// tree.
pub const INSERT_OPTIONS: InsertOptions = InsertOptions {
    validate_insertion_does_not_override: false,
    validate_insertion_does_not_override_tree: true,
    base_root_storage_is_free: true,
    backwards_references: BackwardsReferences::Check,
};

/// How [`Write::Delete`] is applied: a tree is deleted with everything under
/// it.
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

/// How [`Op::Batch`] is applied.
pub const BATCH_OPTIONS: BatchApplyOptions = BatchApplyOptions {
    validate_insertion_does_not_override: false,
    validate_insertion_does_not_override_tree: false,
    disable_operation_consistency_check: false,
    base_root_storage_is_free: true,
    batch_pause_height: None,
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
pub fn apply(
    db: &GroveDb,
    op: &Op,
    transaction: TransactionArg,
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
        Op::PartialBatch(batch) => apply_partial_batch(db, batch, transaction, grove_version),
        Op::FlushPendingDrops => db
            .flush_pending_prefix_drops(grove_version)
            .map(Outcome::Reclaimed)
            .wrap_with_cost(OperationCost::default()),
    }
}

#[expect(
    clippy::result_large_err,
    reason = "GroveDB fixes the error type of the callback"
)]
fn apply_partial_batch(
    db: &GroveDb,
    batch: &PartialBatch,
    transaction: TransactionArg,
    grove_version: &GroveVersion,
) -> CostResult<Outcome, Error> {
    let mut first_segment = OperationCost::default();
    db.apply_partial_batch(
        batch.ops.iter().map(BatchOp::to_grove_op).collect(),
        Some(BatchApplyOptions {
            batch_pause_height: batch.pause_height,
            ..BATCH_OPTIONS
        }),
        |cost, _left_over| {
            first_segment = cost.clone();
            Ok(batch.add_on.iter().map(BatchOp::to_grove_op).collect())
        },
        transaction,
        grove_version,
    )
    .map_ok(|()| Outcome::WrittenInSegments { first_segment })
}

/// Runs `path_query` with `GroveDb::query_raw` when `raw` is set, otherwise
/// with `GroveDb::query`, and returns the rows with how many the offset
/// skipped.
fn query_rows(
    db: &GroveDb,
    path_query: &PathQuery,
    raw: bool,
    transaction: TransactionArg,
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

fn apply_read(
    db: &GroveDb,
    read: &Read,
    transaction: TransactionArg,
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
    }
}

fn apply_write(
    db: &GroveDb,
    write: &Write,
    transaction: TransactionArg,
    grove_version: &GroveVersion,
) -> CostResult<Outcome, Error> {
    match write {
        Write::Insert { path, key, element } => db.insert(
            path.as_slice(),
            key,
            element.clone(),
            Some(INSERT_OPTIONS),
            transaction,
            grove_version,
        ),
        Write::Delete { path, key } => db.delete(
            path.as_slice(),
            key,
            Some(DELETE_OPTIONS),
            transaction,
            grove_version,
        ),
        Write::ClearSubtree { path } => db
            .clear_subtree(
                path.as_slice(),
                Some(CLEAR_OPTIONS),
                transaction,
                grove_version,
            )
            .map(|_cleared| ())
            .wrap_with_cost(OperationCost::default()),
        Write::Append { path, entry } => {
            let Some((key, parent)) = path.split_last() else {
                return Err(Error::InvalidPath(
                    "an append needs the path of its tree".to_string(),
                ))
                .wrap_with_cost(OperationCost::default());
            };
            match entry {
                AppendEntry::Mmr(value) => db
                    .mmr_tree_append(parent, key, value.clone(), transaction, grove_version)
                    .map_ok(|_| ()),
                AppendEntry::Bulk(value) => db
                    .bulk_append(parent, key, value.clone(), transaction, grove_version)
                    .map_ok(|_| ()),
                AppendEntry::Dense(value) => db
                    .dense_tree_insert(parent, key, value.clone(), transaction, grove_version)
                    .map_ok(|_| ()),
                AppendEntry::PrivateDocument(value) => db
                    .private_document_store_insert(
                        parent,
                        key,
                        value.clone(),
                        transaction,
                        grove_version,
                    )
                    .map_ok(|_| ()),
                AppendEntry::Commitment { .. } => db.apply_batch(
                    vec![append_op(path.clone(), entry)],
                    Some(BATCH_OPTIONS),
                    transaction,
                    grove_version,
                ),
            }
        }
        Write::PutAux { key, value } => db.put_aux(key, value, None, transaction),
        Write::DeleteAux { key } => db.delete_aux(key, None, transaction),
        Write::DropFlat { path, key } => db.drop_flat_subtree(
            path.as_slice(),
            key,
            BackwardsReferences::DontCheck,
            transaction,
            grove_version,
        ),
    }
    .map_ok(|()| Outcome::Written)
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
