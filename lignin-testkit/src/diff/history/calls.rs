//! One recorded call, applied to a database of one engine.
//!
//! A call that is made is made with what the stream has of it: the method,
//! path, key and element, the options of its caller, the grove version of
//! the call, and for a batch the answers its caller's callbacks gave, in
//! the order they were asked. A callback that is asked something else than
//! the recording node's was, or in another order, says so in the
//! [`Answer`].
//!
//! Two kinds of call are made with a stand-in for an argument the stream
//! does not have, and say so ([`Answer::stand_in`]): a write of an aux value
//! too long for the recorder, and the two reads that make delete operations.
//! A read in [`READS_NOT_MADE`] is not made. A call with an argument that
//! nothing here reads, a method that is in none of the lists, and a write
//! that cannot be made as it was recorded each end a replay ([`apply`]).

#![expect(
    clippy::result_large_err,
    reason = "a callback of a batch returns GroveDB's own error, whatever its size"
)]

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    fmt::Debug,
};

use grovedb::{
    batch::{
        key_info::KeyInfo, BatchApplyOptions, GroveOp, KeyInfoPath, OpsByLevelPath,
        QualifiedGroveDbOp, RefreshReferenceMode, SubelementsDeletionBehavior,
    },
    operations::{
        delete::{ClearOptions, DeleteOptions, DeleteUpTreeOptions},
        insert::InsertOptions,
    },
    query_result_type::QueryResultType,
    BackwardsReferences, Element, ElementFlags, Engine, Error, GroveDb, MaybeTree, PathQuery,
    Query, SizedQuery, Transaction, TransactionArg, TreeType,
};
use grovedb_costs::{
    storage_cost::{removal::StorageRemovedBytes, StorageCost},
    CostResult, OperationCost,
};
use grovedb_epoch_based_storage_flags::StorageFlags;
use grovedb_path::SubtreePath;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::stream::{
    bytes, clean, cost_text, debug_bool, debug_field, debug_option, path, removed_bytes, same_cost,
    shorten, Call,
};
use crate::golden::run::error_name;

/// The methods of `GroveDb` the recorder knows that change a database or
/// start something that does. A call of one that [`apply`] cannot make ends
/// a replay: the databases would no longer hold what the recording node's
/// did.
pub const WRITES: &[&str] = &[
    "insert",
    "insert_if_not_exists",
    "insert_if_not_exists_return_existing_element",
    "insert_if_changed_value",
    "delete",
    "delete_if_empty_tree",
    "delete_up_tree_while_empty",
    "delete_with_sectional_storage_function",
    "delete_up_tree_while_empty_with_sectional_storage",
    "clear_subtree",
    "drop_flat_subtree",
    "flush_pending_prefix_drops",
    "apply_batch",
    "apply_partial_batch",
    "apply_batch_with_element_flags_update",
    "apply_partial_batch_with_element_flags_update",
    "apply_operations_without_batching",
    "put_aux",
    "delete_aux",
    "mmr_tree_append",
    "bulk_append",
    "dense_tree_insert",
    "private_document_store_insert",
    "commitment_tree_insert",
    "insert_into_count_indexed_tree",
    "delete_from_count_indexed_tree",
    "insert_into_provable_sum_indexed_tree",
    "delete_from_provable_sum_indexed_tree",
    "insert_into_provable_count_provable_sum_indexed_tree",
    "delete_from_provable_count_provable_sum_indexed_tree",
    "reconcile_indexed_tree_secondaries",
    "replace_subtree_root",
    "ingest_subtree_sst",
    "wipe",
    "flush",
    "create_checkpoint",
    "start_snapshot_syncing",
    "start_snapshot_syncing_with_mode",
    "start_syncing_session",
    "start_syncing_session_with_mode",
    "commit_session",
    "commit_transaction",
    "rollback_transaction",
    "tx_set_savepoint",
    "tx_rollback_to_savepoint",
];

/// The reads of `GroveDb` the recorder knows that [`apply`] does not make.
/// They change nothing, so the calls after one are made on the same state;
/// a replay counts them by method.
pub const READS_NOT_MADE: &[&str] = &[
    "add_delete_operations_for_delete_up_tree_while_empty",
    "bulk_chunk_count",
    "bulk_count",
    "bulk_get_buffer",
    "bulk_get_chunk",
    "bulk_get_range",
    "bulk_get_value",
    "check_subtree_exists_invalid_path",
    "commitment_tree_anchor",
    "commitment_tree_count",
    "commitment_tree_get_range",
    "commitment_tree_get_value",
    "dense_tree_count",
    "dense_tree_get",
    "dense_tree_root_hash",
    "fetch_chunk",
    "find_subtrees",
    "follow_reference",
    "follow_reference_as_stored",
    "get_aux_by_key_prefix",
    "get_proved_path_query",
    "get_raw_optional_caching_optional",
    "has_incomplete_restore",
    "mmr_tree_get_value",
    "mmr_tree_leaf_count",
    "mmr_tree_root_hash",
    "private_document_store_count",
    "private_document_store_get_value",
    "prove_branch_chunk",
    "prove_branch_chunk_non_serialized",
    "prove_bulk_position_range",
    "prove_query_many",
    "prove_query_non_serialized",
    "prove_trunk_chunk",
    "prove_trunk_chunk_non_serialized",
    "query_aggregate_count_and_sum",
    "query_aggregate_count_and_sum_per_key",
    "query_aggregate_count_per_key",
    "query_aggregate_sum_per_key",
    "query_aggregate_sums",
    "query_aggregate_sums_with_options",
    "query_encoded_many",
    "query_many_raw",
    "query_sums",
    "root_key",
    "verify_grovedb",
    "visualize_verify_grovedb",
];

/// What an engine answered to a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// The `Debug` text of the value, or the name of the error as two
    /// engines are compared by it (`golden::run::error_name`).
    pub outcome: Result<String, String>,
    /// The error as the engine gave it, by its `Debug` text.
    pub error: Option<String>,
    /// The cost, of a method that returns one.
    pub cost: Option<OperationCost>,
    /// What a callback was asked that the recording node's was not.
    pub notes: Vec<String>,
    /// The argument the call was made with a stand-in for, which the stream
    /// does not have.
    pub stand_in: Option<&'static str>,
}

impl Answer {
    /// The answer on one line: the outcome and the cost.
    pub fn line(&self) -> String {
        let outcome = match &self.outcome {
            Ok(value) => format!("ok {}", shorten(&clean(value))),
            Err(name) => format!(
                "error {name} ({})",
                shorten(&clean(self.error.as_deref().unwrap_or("")))
            ),
        };
        match &self.cost {
            Some(cost) => format!("{outcome}; cost {}", cost_text(cost)),
            None => outcome,
        }
    }
}

fn answer<T: Debug>(result: Result<T, Error>, cost: Option<OperationCost>) -> Answer {
    match result {
        Ok(value) => Answer {
            outcome: Ok(format!("{value:?}")),
            error: None,
            cost,
            notes: Vec::new(),
            stand_in: None,
        },
        Err(error) => Answer {
            outcome: Err(error_name(&error)),
            error: Some(format!("{error:?}")),
            cost,
            notes: Vec::new(),
            stand_in: None,
        },
    }
}

fn with_cost<T: Debug>(result: CostResult<T, Error>) -> Answer {
    answer(result.value, Some(result.cost))
}

/// What became of a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// It was made, and this is what the engine said.
    Answered(Answer),
    /// It is a read this replay does not make, and why.
    NotReplayed(String),
}

/// The transactions of one database that the stream has started and not
/// ended, by the recorder's numbers.
pub struct Transactions<'db, E: Engine> {
    open: HashMap<u64, Transaction<'db, E>>,
}

impl<'db, E: Engine> Default for Transactions<'db, E> {
    fn default() -> Self {
        Transactions {
            open: HashMap::new(),
        }
    }
}

impl<'db, E: Engine> Transactions<'db, E> {
    /// Starts the transaction the recorder numbered `id`.
    pub fn start(&mut self, db: &'db GroveDb<E>, id: u64, snapshot: bool) {
        let transaction = if snapshot {
            db.start_snapshot_read_transaction()
        } else {
            db.start_transaction()
        };
        self.open.insert(id, transaction);
    }

    /// Drops a transaction that ended with no commit. Whether it was open.
    pub fn end(&mut self, id: u64) -> bool {
        self.open.remove(&id).is_some()
    }

    /// Drops every transaction: the recording process ended.
    pub fn end_all(&mut self) {
        self.open.clear();
    }

    /// How many are open.
    pub fn open(&self) -> usize {
        self.open.len()
    }
}

/// The grove version of a protocol number.
pub fn grove_version(protocol: u32) -> Result<&'static GroveVersion, String> {
    GROVE_VERSIONS
        .iter()
        .find(|version| version.protocol_version == protocol)
        .ok_or_else(|| format!("no grove version {protocol}"))
}

fn element(hex_text: &str, grove_version: &GroveVersion) -> Result<Element, String> {
    Element::deserialize(&bytes(hex_text)?, grove_version)
        .map_err(|e| format!("not an element ({e:?}): `{}`", shorten(hex_text)))
}

fn tree_type(text: &str) -> Result<TreeType, String> {
    let (name, argument) = match text.split_once('(') {
        Some((name, rest)) => (name, rest.strip_suffix(')')),
        None => (text, None),
    };
    let with = || -> Result<u8, String> {
        argument
            .and_then(|a| a.parse().ok())
            .ok_or_else(|| format!("not a tree type: `{text}`"))
    };
    Ok(match name {
        "NormalTree" => TreeType::NormalTree,
        "SumTree" => TreeType::SumTree,
        "BigSumTree" => TreeType::BigSumTree,
        "CountTree" => TreeType::CountTree,
        "CountSumTree" => TreeType::CountSumTree,
        "ProvableCountTree" => TreeType::ProvableCountTree,
        "ProvableCountSumTree" => TreeType::ProvableCountSumTree,
        "CommitmentTree" => TreeType::CommitmentTree(with()?),
        "MmrTree" => TreeType::MmrTree,
        "BulkAppendTree" => TreeType::BulkAppendTree(with()?),
        "DenseAppendOnlyFixedSizeTree" => TreeType::DenseAppendOnlyFixedSizeTree(with()?),
        "ProvableSumTree" => TreeType::ProvableSumTree,
        "ProvableCountProvableSumTree" => TreeType::ProvableCountProvableSumTree,
        "ProvableSumIndexedTree" => TreeType::ProvableSumIndexedTree,
        "ProvableCountIndexedTree" => TreeType::ProvableCountIndexedTree,
        "ProvableCountProvableSumIndexedTree" => TreeType::ProvableCountProvableSumIndexedTree,
        "PrivateDocumentStore" => TreeType::PrivateDocumentStore(with()?),
        _ => {
            return Err(format!(
                "not a tree type of the recorded revision: `{text}`"
            ))
        }
    })
}

fn deletion_behavior(text: &str) -> Result<SubelementsDeletionBehavior, String> {
    Ok(match text {
        "DontCheckWithNoCleanup" => SubelementsDeletionBehavior::DontCheckWithNoCleanup,
        "Error" => SubelementsDeletionBehavior::Error,
        "DeleteChildren" => SubelementsDeletionBehavior::DeleteChildren,
        "Skip" => SubelementsDeletionBehavior::Skip,
        "DropFlat" => SubelementsDeletionBehavior::DropFlat,
        _ => return Err(format!("not a deletion behaviour: `{text}`")),
    })
}

fn refresh_mode(text: &str) -> Result<RefreshReferenceMode, String> {
    let sum = |rest: &str| -> Result<i64, String> {
        rest.strip_suffix(')')
            .and_then(|sum| sum.parse().ok())
            .ok_or_else(|| format!("not a refresh mode: `{text}`"))
    };
    Ok(match text {
        "PlainReferenceTrusted" => RefreshReferenceMode::PlainReferenceTrusted,
        "PlainReferenceUntrusted" => RefreshReferenceMode::PlainReferenceUntrusted,
        "SumItemReferenceUntrustedNoValueUpdate" => {
            RefreshReferenceMode::SumItemReferenceUntrustedNoValueUpdate
        }
        other => {
            if let Some(rest) = other.strip_prefix("SumItemReferenceTrusted(") {
                RefreshReferenceMode::SumItemReferenceTrusted(sum(rest)?)
            } else if let Some(rest) = other.strip_prefix("SumItemReferenceUntrustedValueUpdate(") {
                RefreshReferenceMode::SumItemReferenceUntrustedValueUpdate(sum(rest)?)
            } else {
                return Err(format!("not a refresh mode: `{text}`"));
            }
        }
    })
}

fn backwards_references(text: &str) -> Result<BackwardsReferences, String> {
    match text {
        "Check" => Ok(BackwardsReferences::Check),
        "DontCheck" => Ok(BackwardsReferences::DontCheck),
        _ => Err(format!("not a backward-references setting: `{text}`")),
    }
}

fn flag(text: &str) -> Result<bool, String> {
    match text {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => Err(format!("not a flag: `{text}`")),
    }
}

/// One operation of a batch from the recorder's text:
/// `path|key|Variant|field|...`.
pub fn operation(text: &str, grove_version: &GroveVersion) -> Result<QualifiedGroveDbOp, String> {
    let parts: Vec<&str> = text.split('|').collect();
    let wrong = |what: &str| format!("{what} in the operation `{}`", shorten(text));
    let [path_text, key_text, variant, fields @ ..] = parts.as_slice() else {
        return Err(wrong("too few parts"));
    };
    if path_text.contains('~') {
        return Err(wrong("a path segment that is no known key"));
    }
    let key = match key_text.split_at_checked(1) {
        Some(("K", key)) => Some(KeyInfo::KnownKey(bytes(key)?)),
        Some(("-", "")) => None,
        _ => return Err(wrong("a key that is no known key")),
    };
    let field = |index: usize| {
        fields
            .get(index)
            .copied()
            .ok_or_else(|| wrong("a missing field"))
    };
    let hash = |index: usize| -> Result<[u8; 32], String> {
        bytes(field(index)?)?
            .try_into()
            .map_err(|_| wrong("a hash that is not 32 bytes"))
    };
    let op = match *variant {
        "InsertWithKnownToNotAlreadyExist" => GroveOp::InsertWithKnownToNotAlreadyExist {
            element: element(field(0)?, grove_version)?,
        },
        "InsertIfNotExists" => GroveOp::InsertIfNotExists {
            element: element(field(0)?, grove_version)?,
            error_if_exists: flag(field(1)?)?,
        },
        "InsertOrReplace" => GroveOp::InsertOrReplace {
            element: element(field(0)?, grove_version)?,
        },
        "InsertOrReplaceDontCheckForBackwardsReferences" => {
            GroveOp::InsertOrReplaceDontCheckForBackwardsReferences {
                element: element(field(0)?, grove_version)?,
            }
        }
        "Replace" => GroveOp::Replace {
            element: element(field(0)?, grove_version)?,
        },
        "ReplaceDontCheckForBackwardsReferences" => {
            GroveOp::ReplaceDontCheckForBackwardsReferences {
                element: element(field(0)?, grove_version)?,
            }
        }
        "Patch" => GroveOp::Patch {
            element: element(field(0)?, grove_version)?,
            change_in_bytes: field(1)?.parse().map_err(|_| wrong("a change in bytes"))?,
        },
        "PatchDontCheckForBackwardsReferences" => GroveOp::PatchDontCheckForBackwardsReferences {
            element: element(field(0)?, grove_version)?,
            change_in_bytes: field(1)?.parse().map_err(|_| wrong("a change in bytes"))?,
        },
        "RefreshReference" => {
            // The recorder wrote the reference as the element it refreshes.
            let Element::Reference(reference_path_type, max_reference_hop, flags) =
                element(field(0)?, grove_version)?
            else {
                return Err(wrong("no reference"));
            };
            GroveOp::RefreshReference {
                reference_path_type,
                max_reference_hop,
                mode: refresh_mode(field(1)?)?,
                flags,
                non_counted: flag(field(2)?)?,
            }
        }
        "Delete" => GroveOp::Delete,
        "DeleteDontCheckForBackwardsReferences" => GroveOp::DeleteDontCheckForBackwardsReferences,
        "DeleteTree" => GroveOp::DeleteTree(tree_type(field(0)?)?, deletion_behavior(field(1)?)?),
        "DeleteTreeDontCheckForBackwardsReferences" => {
            GroveOp::DeleteTreeDontCheckForBackwardsReferences(
                tree_type(field(0)?)?,
                deletion_behavior(field(1)?)?,
            )
        }
        "CommitmentTreeInsert" => GroveOp::CommitmentTreeInsert {
            cmx: hash(0)?,
            rho: hash(1)?,
            cv_net: hash(2)?,
            payload: bytes(field(3)?)?,
        },
        "MmrTreeAppend" => GroveOp::MmrTreeAppend {
            value: bytes(field(0)?)?,
        },
        "BulkAppend" => GroveOp::BulkAppend {
            value: bytes(field(0)?)?,
        },
        "DenseTreeInsert" => GroveOp::DenseTreeInsert {
            value: bytes(field(0)?)?,
        },
        "PrivateDocumentStoreInsert" => GroveOp::PrivateDocumentStoreInsert {
            entry: bytes(field(0)?)?,
        },
        "Move" => GroveOp::Move {
            new_key: bytes(field(0)?)?,
        },
        _ => return Err(wrong("a kind this replay does not make")),
    };
    Ok(QualifiedGroveDbOp {
        path: KeyInfoPath(
            path(path_text)?
                .into_iter()
                .map(KeyInfo::KnownKey)
                .collect(),
        ),
        key,
        op,
    })
}

fn batch_options(text: &str) -> Result<Option<BatchApplyOptions>, String> {
    let Some(inner) = debug_option(text)? else {
        return Ok(None);
    };
    let pause = debug_field(inner, "batch_pause_height")
        .ok_or_else(|| format!("no batch_pause_height in `{}`", shorten(inner)))?;
    Ok(Some(BatchApplyOptions {
        validate_insertion_does_not_override: debug_bool(
            inner,
            "validate_insertion_does_not_override",
        )?,
        validate_insertion_does_not_override_tree: debug_bool(
            inner,
            "validate_insertion_does_not_override_tree",
        )?,
        disable_operation_consistency_check: debug_bool(
            inner,
            "disable_operation_consistency_check",
        )?,
        base_root_storage_is_free: debug_bool(inner, "base_root_storage_is_free")?,
        batch_pause_height: match debug_option(pause)? {
            Some(height) => Some(
                height
                    .parse()
                    .map_err(|_| format!("pause height `{height}`"))?,
            ),
            None => None,
        },
    }))
}

fn references_of(text: &str) -> Result<BackwardsReferences, String> {
    backwards_references(
        debug_field(text, "backwards_references")
            .ok_or_else(|| format!("no backwards_references in `{}`", shorten(text)))?,
    )
}

fn insert_options(text: &str) -> Result<Option<InsertOptions>, String> {
    let Some(inner) = debug_option(text)? else {
        return Ok(None);
    };
    Ok(Some(InsertOptions {
        validate_insertion_does_not_override: debug_bool(
            inner,
            "validate_insertion_does_not_override",
        )?,
        validate_insertion_does_not_override_tree: debug_bool(
            inner,
            "validate_insertion_does_not_override_tree",
        )?,
        base_root_storage_is_free: debug_bool(inner, "base_root_storage_is_free")?,
        backwards_references: references_of(inner)?,
    }))
}

fn delete_options(text: &str) -> Result<DeleteOptions, String> {
    Ok(DeleteOptions {
        allow_deleting_non_empty_trees: debug_bool(text, "allow_deleting_non_empty_trees")?,
        deleting_non_empty_trees_returns_error: debug_bool(
            text,
            "deleting_non_empty_trees_returns_error",
        )?,
        base_root_storage_is_free: debug_bool(text, "base_root_storage_is_free")?,
        validate_tree_at_path_exists: debug_bool(text, "validate_tree_at_path_exists")?,
        backwards_references: references_of(text)?,
    })
}

fn delete_up_tree_options(text: &str) -> Result<DeleteUpTreeOptions, String> {
    let stop = debug_field(text, "stop_path_height")
        .ok_or_else(|| format!("no stop_path_height in `{}`", shorten(text)))?;
    Ok(DeleteUpTreeOptions {
        allow_deleting_non_empty_trees: debug_bool(text, "allow_deleting_non_empty_trees")?,
        deleting_non_empty_trees_returns_error: debug_bool(
            text,
            "deleting_non_empty_trees_returns_error",
        )?,
        base_root_storage_is_free: debug_bool(text, "base_root_storage_is_free")?,
        validate_tree_at_path_exists: debug_bool(text, "validate_tree_at_path_exists")?,
        stop_path_height: match debug_option(stop)? {
            Some(height) => Some(
                height
                    .parse()
                    .map_err(|_| format!("stop height `{height}`"))?,
            ),
            None => None,
        },
        backwards_references: references_of(text)?,
    })
}

fn clear_options(text: &str) -> Result<Option<ClearOptions>, String> {
    let Some(inner) = debug_option(text)? else {
        return Ok(None);
    };
    Ok(Some(ClearOptions {
        check_for_subtrees: debug_bool(inner, "check_for_subtrees")?,
        allow_deleting_subtrees: debug_bool(inner, "allow_deleting_subtrees")?,
        trying_to_clear_with_subtrees_returns_error: debug_bool(
            inner,
            "trying_to_clear_with_subtrees_returns_error",
        )?,
        backwards_references: references_of(inner)?,
    }))
}

fn maybe_tree(text: &str) -> Result<Option<MaybeTree>, String> {
    Ok(match debug_option(text)? {
        None => None,
        Some("NotTree") => Some(MaybeTree::NotTree),
        Some(tree) => {
            let inner = tree
                .strip_prefix("Tree(")
                .and_then(|rest| rest.strip_suffix(')'))
                .ok_or_else(|| format!("not a MaybeTree: `{tree}`"))?;
            Some(MaybeTree::Tree(tree_type(inner)?))
        }
    })
}

fn result_type(text: &str) -> Result<QueryResultType, String> {
    Ok(match text {
        "QueryElementResultType" => QueryResultType::QueryElementResultType,
        "QueryKeyElementPairResultType" => QueryResultType::QueryKeyElementPairResultType,
        "QueryPathKeyElementTrioResultType" => QueryResultType::QueryPathKeyElementTrioResultType,
        _ => return Err(format!("not a query result type: `{text}`")),
    })
}

/// The path query of a call, which the recorder wrote in its bincode
/// encoding (the standard configuration) at the recorded revision.
///
/// A stream written by hand may name the query of everything directly
/// under a path instead: `range:` and the path, as in `range:/aa/bb`.
pub fn path_query(hex_text: &str) -> Result<PathQuery, String> {
    if let Some(of) = hex_text.strip_prefix("range:") {
        return Ok(PathQuery::new(
            path(of)?,
            SizedQuery::new(Query::new_range_full(), None, None),
        ));
    }
    let encoded = bytes(hex_text)?;
    let (query, read): (PathQuery, usize) =
        bincode::decode_from_slice(&encoded, bincode::config::standard())
            .map_err(|e| format!("a path query this revision does not decode ({e})"))?;
    if read != encoded.len() {
        return Err(format!(
            "a path query of {} bytes of which this revision decodes {read}",
            encoded.len()
        ));
    }
    Ok(query)
}

/// An aux value: its bytes, or for `#<hash>:<length>` (a value too long
/// for the recorder) bytes of that length made from the hash. Whether it is
/// such a stand-in.
fn aux_value(text: &str) -> Result<(Vec<u8>, bool), String> {
    let Some(rest) = text.strip_prefix('#') else {
        return Ok((bytes(text)?, false));
    };
    let (hash, length) = rest
        .split_once(':')
        .ok_or_else(|| format!("not an aux value: `{}`", shorten(text)))?;
    let hash = bytes(hash)?;
    let length: usize = length
        .parse()
        .map_err(|_| format!("aux length `{length}`"))?;
    if hash.is_empty() {
        return Err("an aux value with no hash".to_string());
    }
    Ok((hash.iter().copied().cycle().take(length).collect(), true))
}

/// What the recording node's flag-merge callback was asked and answered.
struct FlagsMerge {
    cost: String,
    stored: Option<ElementFlags>,
    written: ElementFlags,
    left: ElementFlags,
    answer: Result<bool, String>,
}

impl FlagsMerge {
    /// Whether `cost` is the storage cost the recording node's callback was
    /// asked with: `<added>,<replaced>,<removed>`.
    fn asked_with(&self, cost: &StorageCost) -> bool {
        let mut parts = self.cost.splitn(3, ',');
        let number = |part: Option<&str>| part.and_then(|part| part.parse::<u32>().ok());
        number(parts.next()) == Some(cost.added_bytes)
            && number(parts.next()) == Some(cost.replaced_bytes)
            && parts.next().map(removed_bytes) == Some(Ok(cost.removed_bytes.clone()))
    }
}

/// What the recording node's removal-split callback was asked and answered.
struct RemovalSplit {
    given: ElementFlags,
    key_bytes: u32,
    value_bytes: u32,
    left: ElementFlags,
    answer: Result<(StorageRemovedBytes, StorageRemovedBytes), String>,
}

/// What the recording node's add-on callback of a partial batch was asked
/// and answered.
struct AddOn {
    cost: String,
    left_over: bool,
    answer: Result<Vec<QualifiedGroveDbOp>, String>,
}

/// What one callback of the recording node's batch was asked and answered.
enum Asked {
    Merge(FlagsMerge),
    Split(RemovalSplit),
    AddOn(AddOn),
}

const MERGE: &str = "flag-merge";
const SPLIT: &str = "removal-split";
const ADD_ON: &str = "add-on";

impl Asked {
    fn kind(&self) -> &'static str {
        match self {
            Asked::Merge(_) => MERGE,
            Asked::Split(_) => SPLIT,
            Asked::AddOn(_) => ADD_ON,
        }
    }
}

/// The callbacks' answers of one recorded batch, in the order they were
/// given, and what the replayed batch asked otherwise.
#[derive(Default)]
struct Callbacks {
    /// The batch was written by hand (`callbacks=storage-flags`): its
    /// callbacks are those of `grovedb-epoch-based-storage-flags`, and not
    /// recorded answers.
    storage_flags: bool,
    recorded: VecDeque<Asked>,
    notes: Vec<String>,
}

/// Whether the add-on that is last in `recorded` lists as many operations
/// as it names.
fn lists_what_it_names(
    recorded: &VecDeque<Asked>,
    named: &mut Option<usize>,
) -> Result<(), String> {
    if let (Some(named), Some(Asked::AddOn(add_on))) = (named.take(), recorded.back())
        && let Ok(listed) = &add_on.answer
        && listed.len() != named
    {
        return Err(format!(
            "an add-on that names {named} operations and lists {}",
            listed.len()
        ));
    }
    Ok(())
}

impl Callbacks {
    fn of(call: &Call, grove_version: &GroveVersion) -> Result<Self, String> {
        let mut callbacks = Callbacks {
            storage_flags: match call.argument("callbacks") {
                None => false,
                Some("storage-flags") => true,
                Some(other) => {
                    return Err(format!(
                        "callbacks `{other}`: only `storage-flags` is known"
                    ))
                }
            },
            ..Callbacks::default()
        };
        // The count of operations the add-on that is being read names.
        let mut named: Option<usize> = None;
        for (name, value) in call.every(&["uf", "sr", "ao", "aop"]) {
            if name != "aop" {
                lists_what_it_names(&callbacks.recorded, &mut named)?;
            }
            match name {
                "uf" => {
                    let parts: Vec<&str> = value.split(';').collect();
                    let [cost, stored, written, left, answer] = parts.as_slice() else {
                        return Err(format!("a flag merge of {} parts", parts.len()));
                    };
                    callbacks.recorded.push_back(Asked::Merge(FlagsMerge {
                        cost: cost.to_string(),
                        stored: match *stored {
                            "-" => None,
                            flags => Some(bytes(flags)?),
                        },
                        written: bytes(written)?,
                        left: bytes(left)?,
                        answer: match answer.strip_prefix('!') {
                            Some(error) => Err(error.to_string()),
                            None => Ok(flag(answer)?),
                        },
                    }));
                }
                "sr" => {
                    let parts: Vec<&str> = value.split(';').collect();
                    let (given, key_bytes, value_bytes, left, rest) = match parts.as_slice() {
                        [given, key_bytes, value_bytes, left, rest @ ..] => {
                            (given, key_bytes, value_bytes, left, rest)
                        }
                        _ => return Err(format!("a removal split of {} parts", parts.len())),
                    };
                    let answer = match rest {
                        [key, value] => Ok((removed_bytes(key)?, removed_bytes(value)?)),
                        [error] if error.starts_with('!') => Err(error[1..].to_string()),
                        _ => return Err("a removal split with no answer".to_string()),
                    };
                    callbacks.recorded.push_back(Asked::Split(RemovalSplit {
                        given: bytes(given)?,
                        key_bytes: key_bytes.parse().map_err(|_| "key bytes".to_string())?,
                        value_bytes: value_bytes.parse().map_err(|_| "value bytes".to_string())?,
                        left: bytes(left)?,
                        answer,
                    }));
                }
                "ao" => {
                    // The cost is first and has `;` in it where bytes were
                    // removed by section: the two other parts are read from
                    // the right.
                    let mut parts = value.rsplitn(3, ';');
                    let (Some(count), Some(left_over), Some(cost)) =
                        (parts.next(), parts.next(), parts.next())
                    else {
                        return Err("an add-on of fewer than three parts".to_string());
                    };
                    let answer = match count.strip_prefix('!') {
                        Some(error) => Err(error.to_string()),
                        None => {
                            named = Some(
                                count
                                    .parse()
                                    .map_err(|_| format!("add-on count `{count}`"))?,
                            );
                            Ok(Vec::new())
                        }
                    };
                    callbacks.recorded.push_back(Asked::AddOn(AddOn {
                        cost: cost.to_string(),
                        left_over: flag(left_over)?,
                        answer,
                    }));
                }
                _ => {
                    let (Some(_), Some(Asked::AddOn(add_on))) =
                        (named, callbacks.recorded.back_mut())
                    else {
                        return Err("an added operation with no add-on that names it".to_string());
                    };
                    if let Ok(operations) = add_on.answer.as_mut() {
                        operations.push(operation(value, grove_version)?);
                    }
                }
            }
        }
        lists_what_it_names(&callbacks.recorded, &mut named)?;
        if callbacks.storage_flags && !callbacks.recorded.is_empty() {
            return Err("`callbacks=storage-flags` beside recorded callbacks".to_string());
        }
        Ok(callbacks)
    }

    /// The recorded callback that is next, where it is of this kind. Where
    /// the next is of another kind, or there is none, that is noted and the
    /// first later one of this kind is taken, so that the batch still gets
    /// a recorded answer where there is one.
    fn next(&mut self, kind: &'static str) -> Option<Asked> {
        let front = self.recorded.front().map(Asked::kind);
        if front == Some(kind) {
            return self.recorded.pop_front();
        }
        self.notes.push(match front {
            Some(other) => format!(
                "the {kind} callback was asked where the recording node's next was the {other} \
                 callback"
            ),
            None => {
                format!("the {kind} callback was asked once more than on the recording node")
            }
        });
        let at = self
            .recorded
            .iter()
            .position(|asked| asked.kind() == kind)?;
        self.recorded.remove(at)
    }

    fn merge(
        &mut self,
        cost: &StorageCost,
        stored: Option<ElementFlags>,
        written: &mut ElementFlags,
    ) -> Result<bool, Error> {
        if self.storage_flags {
            return StorageFlags::update_element_flags(cost, stored, written)
                .map_err(|error| Error::JustInTimeElementFlagsClientError(error.to_string()));
        }
        let Some(Asked::Merge(recorded)) = self.next(MERGE) else {
            return Ok(false);
        };
        let asked = format!(
            "{},{},{}",
            cost.added_bytes,
            cost.replaced_bytes,
            removed_text(&cost.removed_bytes)
        );
        if !recorded.asked_with(cost) || stored != recorded.stored || *written != recorded.written {
            self.notes.push(format!(
                "the flag-merge callback was asked ({asked}; {}; {}) where the recording node's \
                 was asked ({}; {}; {})",
                stored.as_ref().map_or("-".into(), hex::encode),
                hex::encode(&*written),
                recorded.cost,
                recorded.stored.as_ref().map_or("-".into(), hex::encode),
                hex::encode(&recorded.written),
            ));
        }
        *written = recorded.left;
        recorded
            .answer
            .map_err(Error::JustInTimeElementFlagsClientError)
    }

    fn split(
        &mut self,
        flags: &mut ElementFlags,
        key_bytes: u32,
        value_bytes: u32,
    ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error> {
        if self.storage_flags {
            return StorageFlags::split_removal_bytes(flags, key_bytes, value_bytes)
                .map_err(|error| Error::SplitRemovalBytesClientError(error.to_string()));
        }
        let Some(Asked::Split(recorded)) = self.next(SPLIT) else {
            return Ok((
                StorageRemovedBytes::BasicStorageRemoval(key_bytes),
                StorageRemovedBytes::BasicStorageRemoval(value_bytes),
            ));
        };
        if *flags != recorded.given
            || key_bytes != recorded.key_bytes
            || value_bytes != recorded.value_bytes
        {
            self.notes.push(format!(
                "the removal-split callback was asked ({}; {key_bytes}; {value_bytes}) where the \
                 recording node's was asked ({}; {}; {})",
                hex::encode(&*flags),
                hex::encode(&recorded.given),
                recorded.key_bytes,
                recorded.value_bytes,
            ));
        }
        *flags = recorded.left;
        recorded.answer.map_err(Error::SplitRemovalBytesClientError)
    }

    fn add_on(
        &mut self,
        cost: &OperationCost,
        left_over: &Option<OpsByLevelPath>,
    ) -> Result<Vec<QualifiedGroveDbOp>, Error> {
        let Some(Asked::AddOn(recorded)) = self.next(ADD_ON) else {
            return Ok(Vec::new());
        };
        let asked = cost_text(cost);
        if !same_cost(&recorded.cost, cost) || left_over.is_some() != recorded.left_over {
            self.notes.push(format!(
                "the add-on callback was asked ({asked}; {}) where the recording node's was \
                 asked ({}; {})",
                left_over.is_some(),
                recorded.cost,
                recorded.left_over,
            ));
        }
        recorded.answer.map_err(Error::InternalError)
    }

    /// The recorded callbacks the batch did not come to.
    fn left(&self) -> Option<String> {
        let of = |kind: &str| {
            self.recorded
                .iter()
                .filter(|asked| asked.kind() == kind)
                .count()
        };
        (!self.recorded.is_empty()).then(|| {
            format!(
                "{} flag merges, {} removal splits and {} add-ons of the recording node were not \
                 asked for",
                of(MERGE),
                of(SPLIT),
                of(ADD_ON)
            )
        })
    }
}

/// Removed bytes as the recorder writes them in a callback's field.
fn removed_text(removed: &StorageRemovedBytes) -> String {
    match removed {
        StorageRemovedBytes::NoStorageRemoval => "N".to_string(),
        StorageRemovedBytes::BasicStorageRemoval(bytes) => format!("B{bytes}"),
        StorageRemovedBytes::SectionedStorageRemoval(by_identifier) => {
            let mut text = "S".to_string();
            for (index, (identifier, by_epoch)) in by_identifier.iter().enumerate() {
                if index > 0 {
                    text.push('&');
                }
                text.push_str(&hex::encode(identifier));
                text.push(':');
                let mut epochs: Vec<(u16, u32)> = by_epoch
                    .iter()
                    .map(|(e, b)| (e.to_owned(), b.to_owned()))
                    .collect();
                epochs.sort_unstable();
                let epochs: Vec<String> = epochs
                    .iter()
                    .map(|(epoch, bytes)| format!("{epoch}={bytes}"))
                    .collect();
                text.push_str(&epochs.join("+"));
            }
            text
        }
    }
}

/// The operations of a batch, which must be as many as the call names.
fn operations(
    call: &Call,
    grove_version: &GroveVersion,
) -> Result<Vec<QualifiedGroveDbOp>, String> {
    let named: usize = call
        .required("ops")?
        .parse()
        .map_err(|_| "a batch whose count of operations is no number".to_string())?;
    let listed = call.every(&["op"]);
    if listed.len() != named {
        return Err(format!(
            "a batch that names {named} operations and lists {}",
            listed.len()
        ));
    }
    listed
        .into_iter()
        .map(|(_, value)| operation(value, grove_version))
        .collect()
}

/// Makes a recorded call on `db`.
///
/// `Err` is a call that cannot be made as it was recorded, and the replay
/// ends there: a write this does not make, a method in none of the lists,
/// an argument that cannot be read, or one that nothing read. The last is
/// found when the call has been made.
pub fn apply<'db, E: Engine>(
    db: &'db GroveDb<E>,
    transactions: &mut Transactions<'db, E>,
    call: &Call,
) -> Result<Applied, String> {
    let applied = make(db, transactions, call)?;
    if let (Applied::Answered(_), Some(name)) = (&applied, call.unread()) {
        return Err(format!(
            "`{}` has an argument `{name}` that this replay does not read",
            call.method
        ));
    }
    Ok(applied)
}

fn make<'db, E: Engine>(
    db: &'db GroveDb<E>,
    transactions: &mut Transactions<'db, E>,
    call: &Call,
) -> Result<Applied, String> {
    let method = call.method.as_str();
    // What the recorder found at the place of a write before it: a note for
    // a count of states, and no argument of the call.
    call.argument("was");
    let id = call.transaction();
    let version = || -> Result<&'static GroveVersion, String> {
        grove_version(
            call.grove_version()
                .ok_or_else(|| format!("`{method}` was recorded with no grove version"))?,
        )
    };

    // What ends or steps the transaction itself.
    match method {
        "commit_transaction" => {
            let id = id.ok_or("a commit with no transaction")?;
            let transaction = transactions
                .open
                .remove(&id)
                .ok_or_else(|| format!("the commit of transaction {id}, which is not open"))?;
            return Ok(Applied::Answered(with_cost(
                db.commit_transaction(transaction),
            )));
        }
        "rollback_transaction" | "tx_set_savepoint" | "tx_rollback_to_savepoint" => {
            let id = id.ok_or("a transaction step with no transaction")?;
            let transaction = transactions
                .open
                .get(&id)
                .ok_or_else(|| format!("a step on transaction {id}, which is not open"))?;
            let result = match method {
                "rollback_transaction" => db.rollback_transaction(transaction),
                "tx_set_savepoint" => db.set_savepoint(transaction),
                _ => db.rollback_to_savepoint(transaction),
            };
            return Ok(Applied::Answered(answer(result, None)));
        }
        _ => {}
    }

    if id.is_some() && matches!(method, "prove_query" | "flush_pending_prefix_drops") {
        return Err(format!(
            "`{method}` takes no transaction and the stream names one"
        ));
    }
    let transaction: TransactionArg<'db, '_, E> = match id {
        None => None,
        Some(id) => Some(
            transactions
                .open
                .get(&id)
                .ok_or_else(|| format!("`{method}` on transaction {id}, which is not open"))?,
        ),
    };
    let segments = |name: &str| -> Result<Vec<Vec<u8>>, String> { path(call.required(name)?) };
    let hex_of = |name: &str| -> Result<Vec<u8>, String> { bytes(call.required(name)?) };
    let truth = |name: &str| -> Result<bool, String> { flag(call.required(name)?) };
    let query = || -> Result<PathQuery, String> { path_query(call.required("path_query")?) };
    let answered = |answer: Answer| Ok(Applied::Answered(answer));

    macro_rules! subtree {
        ($segments:expr) => {
            SubtreePath::from(
                $segments
                    .iter()
                    .map(Vec::as_slice)
                    .collect::<Vec<&[u8]>>()
                    .as_slice(),
            )
        };
    }
    // A read whose arguments this revision cannot rebuild is left out and
    // counted; it changes nothing.
    macro_rules! read_query {
        () => {
            match query() {
                Ok(query) => query,
                Err(why) => return Ok(Applied::NotReplayed(why)),
            }
        };
    }

    match method {
        // ---- writes ----
        "insert" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            let element = element(call.required("element")?, version()?)?;
            let options = insert_options(call.required("options")?)?;
            answered(with_cost(db.insert(
                subtree!(p),
                &key,
                element,
                options,
                transaction,
                version()?,
            )))
        }
        "insert_if_not_exists" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            let element = element(call.required("element")?, version()?)?;
            answered(with_cost(db.insert_if_not_exists(
                subtree!(p),
                &key,
                element,
                transaction,
                version()?,
            )))
        }
        "insert_if_not_exists_return_existing_element" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            let element = element(call.required("element")?, version()?)?;
            answered(with_cost(db.insert_if_not_exists_return_existing_element(
                subtree!(p),
                &key,
                element,
                transaction,
                version()?,
            )))
        }
        "insert_if_changed_value" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            let element = element(call.required("element")?, version()?)?;
            answered(with_cost(db.insert_if_changed_value(
                subtree!(p),
                &key,
                element,
                transaction,
                version()?,
            )))
        }
        "delete" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            let options = match debug_option(call.required("options")?)? {
                Some(inner) => Some(delete_options(inner)?),
                None => None,
            };
            answered(with_cost(db.delete(
                subtree!(p),
                &key,
                options,
                transaction,
                version()?,
            )))
        }
        "delete_if_empty_tree" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            answered(with_cost(db.delete_if_empty_tree(
                subtree!(p),
                &key,
                transaction,
                version()?,
            )))
        }
        "delete_up_tree_while_empty" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            let options = delete_up_tree_options(call.required("options")?)?;
            answered(with_cost(db.delete_up_tree_while_empty(
                subtree!(p),
                &key,
                &options,
                transaction,
                version()?,
            )))
        }
        "clear_subtree" => {
            let p = segments("path")?;
            let options = clear_options(call.required("options")?)?;
            answered(answer(
                db.clear_subtree(subtree!(p), options, transaction, version()?),
                None,
            ))
        }
        "drop_flat_subtree" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            let references = backwards_references(call.required("backwards_references")?)?;
            answered(with_cost(db.drop_flat_subtree(
                subtree!(p),
                &key,
                references,
                transaction,
                version()?,
            )))
        }
        "flush_pending_prefix_drops" => {
            answered(answer(db.flush_pending_prefix_drops(version()?), None))
        }
        "put_aux" => {
            if call.required("cost_info")? != "None" {
                return Err("an aux write with a cost of its own".to_string());
            }
            let (value, stand_in) = aux_value(call.required("value")?)?;
            let mut answer = with_cost(db.put_aux(hex_of("key")?, &value, None, transaction));
            if stand_in {
                answer.stand_in =
                    Some("an aux value too long for the recorder: other bytes of its length");
            }
            answered(answer)
        }
        "delete_aux" => {
            if call.required("cost_info")? != "None" {
                return Err("an aux delete with a cost of its own".to_string());
            }
            answered(with_cost(db.delete_aux(hex_of("key")?, None, transaction)))
        }
        "apply_batch" | "apply_operations_without_batching" => {
            let ops = operations(call, version()?)?;
            let options = batch_options(
                call.required("batch_apply_options")
                    .or_else(|_| call.required("options"))?,
            )?;
            let result = if method == "apply_batch" {
                db.apply_batch(ops, options, transaction, version()?)
            } else {
                db.apply_operations_without_batching(ops, options, transaction, version()?)
            };
            answered(with_cost(result))
        }
        "apply_batch_with_element_flags_update"
        | "apply_partial_batch"
        | "apply_partial_batch_with_element_flags_update" => {
            let grove_version = version()?;
            let ops = operations(call, grove_version)?;
            let options = batch_options(call.required("batch_apply_options")?)?;
            let callbacks = RefCell::new(Callbacks::of(call, grove_version)?);
            let merge =
                |cost: &StorageCost, stored: Option<ElementFlags>, written: &mut ElementFlags| {
                    callbacks.borrow_mut().merge(cost, stored, written)
                };
            let split = |flags: &mut ElementFlags, key_bytes: u32, value_bytes: u32| {
                callbacks.borrow_mut().split(flags, key_bytes, value_bytes)
            };
            let add_on = |cost: &OperationCost, left_over: &Option<OpsByLevelPath>| {
                callbacks.borrow_mut().add_on(cost, left_over)
            };
            let result = match method {
                "apply_batch_with_element_flags_update" => db
                    .apply_batch_with_element_flags_update(
                        ops,
                        options,
                        merge,
                        split,
                        transaction,
                        grove_version,
                    ),
                "apply_partial_batch" => {
                    db.apply_partial_batch(ops, options, add_on, transaction, grove_version)
                }
                _ => db.apply_partial_batch_with_element_flags_update(
                    ops,
                    options,
                    merge,
                    split,
                    add_on,
                    transaction,
                    grove_version,
                ),
            };
            let mut answer = with_cost(result);
            let callbacks = callbacks.into_inner();
            answer.notes.extend(callbacks.left());
            answer.notes.extend(callbacks.notes);
            answered(answer)
        }

        // ---- reads ----
        "get" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            answered(with_cost(db.get(
                subtree!(p),
                &key,
                transaction,
                version()?,
            )))
        }
        "get_raw" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            answered(with_cost(db.get_raw(
                subtree!(p),
                &key,
                transaction,
                version()?,
            )))
        }
        "get_raw_optional" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            answered(with_cost(db.get_raw_optional(
                subtree!(p),
                &key,
                transaction,
                version()?,
            )))
        }
        "get_caching_optional" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            answered(with_cost(db.get_caching_optional(
                subtree!(p),
                &key,
                truth("allow_cache")?,
                transaction,
                version()?,
            )))
        }
        "get_raw_caching_optional" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            answered(with_cost(db.get_raw_caching_optional(
                subtree!(p),
                &key,
                truth("allow_cache")?,
                transaction,
                version()?,
            )))
        }
        "has_raw" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            answered(with_cost(db.has_raw(
                subtree!(p),
                &key,
                transaction,
                version()?,
            )))
        }
        "is_empty_tree" => {
            let p = segments("path")?;
            answered(with_cost(db.is_empty_tree(
                subtree!(p),
                transaction,
                version()?,
            )))
        }
        "root_hash" => answered(with_cost(db.root_hash(transaction, version()?))),
        "get_aux" => answered(with_cost(db.get_aux(hex_of("key")?, transaction))),
        "query" | "query_raw" => {
            let query = read_query!();
            let flags = (
                truth("allow_cache")?,
                truth("decrease_limit_on_range_with_no_sub_elements")?,
                truth("error_if_intermediate_path_tree_not_present")?,
            );
            let kind = result_type(call.required("result_type")?)?;
            let result = if method == "query" {
                db.query(
                    &query,
                    flags.0,
                    flags.1,
                    flags.2,
                    kind,
                    transaction,
                    version()?,
                )
            } else {
                db.query_raw(
                    &query,
                    flags.0,
                    flags.1,
                    flags.2,
                    kind,
                    transaction,
                    version()?,
                )
            };
            answered(with_cost(result))
        }
        "query_item_value"
        | "query_item_value_or_sum"
        | "query_raw_keys_optional"
        | "query_keys_optional" => {
            let query = read_query!();
            let flags = (
                truth("allow_cache")?,
                truth("decrease_limit_on_range_with_no_sub_elements")?,
                truth("error_if_intermediate_path_tree_not_present")?,
            );
            let version = version()?;
            answered(match method {
                "query_item_value" => with_cost(db.query_item_value(
                    &query,
                    flags.0,
                    flags.1,
                    flags.2,
                    transaction,
                    version,
                )),
                "query_item_value_or_sum" => with_cost(db.query_item_value_or_sum(
                    &query,
                    flags.0,
                    flags.1,
                    flags.2,
                    transaction,
                    version,
                )),
                "query_raw_keys_optional" => with_cost(db.query_raw_keys_optional(
                    &query,
                    flags.0,
                    flags.1,
                    flags.2,
                    transaction,
                    version,
                )),
                _ => with_cost(db.query_keys_optional(
                    &query,
                    flags.0,
                    flags.1,
                    flags.2,
                    transaction,
                    version,
                )),
            })
        }
        "prove_query" => {
            let query = read_query!();
            if call
                .argument("prove_options")
                .is_some_and(|options| options != "None")
            {
                return Ok(Applied::NotReplayed("a proof with options".to_string()));
            }
            answered(with_cost(db.prove_query(&query, None, version()?)))
        }
        "query_aggregate_sum" => {
            let query = read_query!();
            answered(with_cost(db.query_aggregate_sum(
                &query,
                transaction,
                version()?,
            )))
        }
        "query_aggregate_count" => {
            let query = read_query!();
            answered(with_cost(db.query_aggregate_count(
                &query,
                transaction,
                version()?,
            )))
        }
        // The two reads that make delete operations were handed the batch
        // their caller was building, which the recorder did not write down.
        // They are made with none, and say so.
        "delete_operation_for_delete_internal" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            let options = delete_options(call.required("options")?)?;
            let known = maybe_tree(call.required("is_known_to_be_subtree")?)?;
            let none = pending_operations(call)?;
            let mut answer = with_cost(db.delete_operation_for_delete_internal(
                subtree!(p),
                &key,
                &options,
                known,
                &none,
                transaction,
                version()?,
            ));
            answer.stand_in = Some(PENDING_OPERATIONS);
            answered(answer)
        }
        "delete_operations_for_delete_up_tree_while_empty" => {
            let (p, key) = (segments("path")?, hex_of("key")?);
            let options = delete_up_tree_options(call.required("options")?)?;
            let known = maybe_tree(call.required("is_known_to_be_subtree")?)?;
            let none = pending_operations(call)?;
            let mut answer = with_cost(db.delete_operations_for_delete_up_tree_while_empty(
                subtree!(p),
                &key,
                &options,
                known,
                &none,
                transaction,
                version()?,
            ));
            answer.stand_in = Some(PENDING_OPERATIONS);
            answered(answer)
        }
        _ if READS_NOT_MADE.contains(&method) => Ok(Applied::NotReplayed(format!(
            "`{method}` is a read this replay does not make"
        ))),
        _ if WRITES.contains(&method) => {
            Err(format!("the write `{method}` is not made by this replay"))
        }
        _ => Err(format!("`{method}` is no method this replay knows")),
    }
}

const PENDING_OPERATIONS: &str =
    "the operations its caller was building, which the recorder did not write: none";

/// The stand-in for the operations the caller of a read that makes delete
/// operations was building: none. The recorder writes `?` for them.
fn pending_operations(call: &Call) -> Result<Vec<QualifiedGroveDbOp>, String> {
    match call.required("current_batch_operations")? {
        "?" => Ok(Vec::new()),
        other => Err(format!(
            "pending operations `{}`, where the recorder writes `?`",
            shorten(other)
        )),
    }
}

/// What of a call's answer is held against the recording node's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// The outcome and the cost.
    OutcomeAndCost,
    /// The cost only.
    Cost,
    /// Neither.
    Neither,
}

/// What is held against the recording node's answer, by method. The two
/// reads that make delete operations were given other operations there
/// ([`apply`]). An aux value too long for the recorder is written as other
/// bytes of its length, so of a read of aux the cost is held, and the value
/// only where a replay knows that the key was not written so.
pub fn held_against_the_recording(method: &str) -> Held {
    match method {
        "delete_operation_for_delete_internal"
        | "delete_operations_for_delete_up_tree_while_empty" => Held::Neither,
        "get_aux" => Held::Cost,
        _ => Held::OutcomeAndCost,
    }
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::v1::GROVE_V1;

    use super::*;

    #[test]
    fn an_operation_is_read_from_the_recorders_text() {
        let put = operation("/61/62|K63|InsertOrReplace|000301020300", &GROVE_V1).unwrap();
        assert_eq!(put.path.to_path(), vec![vec![0x61], vec![0x62]]);
        assert_eq!(put.key, Some(KeyInfo::KnownKey(vec![0x63])));
        assert_eq!(
            put.op,
            GroveOp::InsertOrReplace {
                element: Element::Item(vec![1, 2, 3], None)
            }
        );

        let delete = operation("|K61|DeleteTree|SumTree|DeleteChildren", &GROVE_V1).unwrap();
        assert!(delete.path.0.is_empty());
        assert_eq!(
            delete.op,
            GroveOp::DeleteTree(
                TreeType::SumTree,
                SubelementsDeletionBehavior::DeleteChildren
            )
        );

        let patch = operation(
            "/61|K62|PatchDontCheckForBackwardsReferences|000101 00|-3",
            &GROVE_V1,
        );
        assert!(patch.is_err(), "an element that is no hex");
        let patch = operation(
            "/61|K62|PatchDontCheckForBackwardsReferences|00010100|-3",
            &GROVE_V1,
        )
        .unwrap();
        assert!(matches!(
            patch.op,
            GroveOp::PatchDontCheckForBackwardsReferences {
                change_in_bytes: -3,
                ..
            }
        ));

        assert!(
            operation("/61|M00,8|Delete", &GROVE_V1).is_err(),
            "a key of a maximal size"
        );
        assert!(
            operation("/~61|K00|Delete", &GROVE_V1).is_err(),
            "a path of an estimate"
        );
        assert!(operation("/61|K00|ReplaceTreeRootKey", &GROVE_V1).is_err());
        assert!(tree_type("CommitmentTree(11)") == Ok(TreeType::CommitmentTree(11)));
        assert!(
            tree_type("CountBigSumTree").is_err(),
            "no tree type of the recorded revision"
        );
    }

    #[test]
    fn options_are_read_from_their_debug_text() {
        let batch = batch_options(
            "Some(BatchApplyOptions { validate_insertion_does_not_override: false, \
             validate_insertion_does_not_override_tree: true, disable_operation_consistency_check: \
             true, base_root_storage_is_free: true, batch_pause_height: Some(2) })",
        )
        .unwrap()
        .unwrap();
        assert!(!batch.validate_insertion_does_not_override);
        assert!(batch.validate_insertion_does_not_override_tree);
        assert!(batch.disable_operation_consistency_check);
        assert_eq!(batch.batch_pause_height, Some(2));
        assert!(batch_options("None").unwrap().is_none());

        let up = delete_up_tree_options(
            "DeleteUpTreeOptions { allow_deleting_non_empty_trees: false, \
             deleting_non_empty_trees_returns_error: true, base_root_storage_is_free: true, \
             validate_tree_at_path_exists: false, stop_path_height: Some(4), backwards_references: \
             DontCheck }",
        )
        .unwrap();
        assert_eq!(up.stop_path_height, Some(4));
        assert!(up.deleting_non_empty_trees_returns_error);
        assert_eq!(up.backwards_references, BackwardsReferences::DontCheck);

        let clear = clear_options(
            "Some(ClearOptions { check_for_subtrees: false, allow_deleting_subtrees: false, \
             trying_to_clear_with_subtrees_returns_error: false, backwards_references: Check })",
        )
        .unwrap()
        .unwrap();
        assert!(!clear.check_for_subtrees);
        assert_eq!(clear.backwards_references, BackwardsReferences::Check);

        assert_eq!(
            maybe_tree("Some(NotTree)").unwrap(),
            Some(MaybeTree::NotTree)
        );
        assert_eq!(
            maybe_tree("Some(Tree(SumTree))").unwrap(),
            Some(MaybeTree::Tree(TreeType::SumTree))
        );
        assert_eq!(maybe_tree("None").unwrap(), None);
    }

    #[test]
    fn a_hand_written_range_query_and_a_long_aux_value_are_read() {
        let query = path_query("range:/61/62").unwrap();
        assert_eq!(query.path, vec![vec![0x61], vec![0x62]]);
        assert_eq!(query.query.limit, None);
        assert!(path_query("zz").is_err());

        assert_eq!(aux_value("0102").unwrap(), (vec![1, 2], false));
        let long = aux_value("#a1b2:5").unwrap();
        assert_eq!(long, (vec![0xa1, 0xb2, 0xa1, 0xb2, 0xa1], true));
        assert!(aux_value("#:5").is_err());
    }

    #[test]
    fn the_callbacks_of_a_batch_are_read_in_their_order() {
        let text = "C\t5\t1\tapply_partial_batch_with_element_flags_update\tops=0\tbatch_apply_options=None\ttx=1\tgv=1\tuf=4,0,N;-;0001;0002;1\tsr=0001;3;40;0001;B3;B40\tao=1,0,4,0,NoStorageRemoval,2,0;0;1\taop=/61|K62|Delete\t|\tok";
        let super::super::stream::Line::Call(call) = super::super::stream::line(text) else {
            panic!("a call line");
        };
        let mut callbacks = Callbacks::of(&call, &GROVE_V1).unwrap();
        assert!(!callbacks.storage_flags);
        let kinds: Vec<&str> = callbacks.recorded.iter().map(Asked::kind).collect();
        assert_eq!(kinds, vec![MERGE, SPLIT, ADD_ON]);

        let cost = StorageCost {
            added_bytes: 4,
            replaced_bytes: 0,
            removed_bytes: StorageRemovedBytes::NoStorageRemoval,
        };
        let mut written = vec![0, 1];
        assert!(matches!(
            callbacks.merge(&cost, None, &mut written),
            Ok(true)
        ));
        assert_eq!(
            written,
            vec![0, 2],
            "the flags the recording node's callback left"
        );

        let mut flags = vec![0, 1];
        let split = callbacks.split(&mut flags, 3, 41).unwrap();
        assert_eq!(
            split,
            (
                StorageRemovedBytes::BasicStorageRemoval(3),
                StorageRemovedBytes::BasicStorageRemoval(40)
            )
        );
        assert_eq!(
            callbacks.notes.len(),
            1,
            "it was asked for 41 bytes, not 40"
        );

        let added = callbacks.add_on(&OperationCost::default(), &None).unwrap();
        assert_eq!(added.len(), 1);
        assert_eq!(callbacks.notes.len(), 2, "it was asked with another cost");
        assert_eq!(callbacks.left(), None);
        assert!(matches!(
            callbacks.merge(&cost, None, &mut written),
            Ok(false)
        ));
        assert_eq!(callbacks.notes.len(), 3, "one merge more than was recorded");
    }

    fn call(text: &str) -> Call {
        match super::super::stream::line(text) {
            super::super::stream::Line::Call(call) => call,
            other => panic!("a call line, not {other:?}"),
        }
    }

    #[test]
    fn a_callback_asked_out_of_the_recorded_order_is_noted() {
        let call = call(
            "C\t5\t1\tapply_batch_with_element_flags_update\tops=0\tbatch_apply_options=None\ttx=1\tgv=1\tuf=4,0,N;-;0001;0002;1\tsr=0001;3;40;0001;B3;B40\t|\tok",
        );
        let mut callbacks = Callbacks::of(&call, &GROVE_V1).unwrap();
        let mut flags = vec![0, 1];
        let split = callbacks.split(&mut flags, 3, 40).unwrap();
        assert_eq!(
            split.1,
            StorageRemovedBytes::BasicStorageRemoval(40),
            "the recorded answer is still given"
        );
        assert_eq!(callbacks.notes.len(), 1, "{:?}", callbacks.notes);
        assert!(callbacks.notes[0].contains("next was the flag-merge callback"));
        assert_eq!(
            callbacks.left().as_deref(),
            Some(
                "1 flag merges, 0 removal splits and 0 add-ons of the recording node were not \
                 asked for"
            )
        );
    }

    #[test]
    fn an_add_on_whose_cost_has_bytes_removed_by_section_is_read() {
        let identifier = "17; ".repeat(31) + "17";
        let cost = format!("1,0,4,0,SectionedStorageRemoval({{[{identifier}]: {{0: 5}}}}),2,0");
        let text = format!(
            "C\t5\t1\tapply_partial_batch\tops=0\tbatch_apply_options=None\ttx=1\tgv=1\tao={cost};1;2\taop=/61|K62|Delete\taop=/61|K63|Delete\tao={cost};0;0\t|\tok"
        );
        let mut callbacks = Callbacks::of(&call(&text), &GROVE_V1).unwrap();
        assert_eq!(callbacks.recorded.len(), 2);
        let Some(Asked::AddOn(first)) = callbacks.next(ADD_ON) else {
            panic!("an add-on");
        };
        assert_eq!(first.cost, cost);
        assert!(first.left_over);
        assert_eq!(first.answer.unwrap().len(), 2);
        assert!(callbacks.notes.is_empty());

        for wrong in [
            // An add-on that names two operations and lists one, first and last.
            "ao=1,0,4,0,NoStorageRemoval,2,0;0;2\taop=/61|K62|Delete\tao=1,0,4,0,NoStorageRemoval,2,0;0;0",
            "ao=1,0,4,0,NoStorageRemoval,2,0;0;2\taop=/61|K62|Delete",
            // An added operation that no add-on names.
            "uf=4,0,N;-;0001;0002;1\taop=/61|K62|Delete",
            // Recorded callbacks beside the ones of a hand-written batch.
            "callbacks=storage-flags\tuf=4,0,N;-;0001;0002;1",
        ] {
            let text = format!(
                "C\t5\t1\tapply_partial_batch\tops=0\tbatch_apply_options=None\ttx=1\tgv=1\t{wrong}\t|\tok"
            );
            assert!(
                Callbacks::of(&call(&text), &GROVE_V1).is_err(),
                "`{wrong}` is read"
            );
        }
    }

    #[test]
    fn a_batch_lists_as_many_operations_as_it_names() {
        let whole = call("C\t5\t1\tapply_batch\tops=1\top=|K61|Delete\tbatch_apply_options=None\ttx=-\tgv=1\t|\tok");
        assert_eq!(operations(&whole, &GROVE_V1).unwrap().len(), 1);
        for cut in [
            "ops=2\top=|K61|Delete",
            "ops=0\top=|K61|Delete",
            "ops=x\top=|K61|Delete",
            "op=|K61|Delete",
        ] {
            let text =
                format!("C\t5\t1\tapply_batch\t{cut}\tbatch_apply_options=None\ttx=-\tgv=1\t|\tok");
            assert!(operations(&call(&text), &GROVE_V1).is_err(), "`{cut}`");
        }
    }

    #[test]
    fn a_proof_is_answered_by_the_text_of_its_bytes() {
        // As the recorder writes what `prove_query` returned.
        let proof = answer(Ok::<Vec<u8>, Error>(vec![1, 2, 255]), None);
        assert_eq!(proof.outcome, Ok("[1, 2, 255]".to_string()));
    }

    #[test]
    fn every_method_is_in_one_list_at_most() {
        for method in READS_NOT_MADE {
            assert!(!WRITES.contains(method), "{method}");
        }
        assert_eq!(
            held_against_the_recording("get_aux"),
            Held::Cost,
            "an aux value may be a stand-in"
        );
        assert_eq!(held_against_the_recording("get"), Held::OutcomeAndCost);
    }
}
