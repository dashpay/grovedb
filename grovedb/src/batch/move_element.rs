//! `GroveOp::Move` (issue #1014): rename a key within its parent and keep
//! the stored nodes of everything under it.
//!
//! A move is applied in two steps:
//!
//! - At its level, `execute_ops_on_path` hands the level's moves to
//!   [`plan_level_moves`], which reads each element and its node value hash
//!   from the parent Merk and turns the move into Merk ops: `Op::DeleteMoved`
//!   at the old key and `Op::PutMoved` at the new one (in a swap, a
//!   `PutMoved` replaces the node another move leaves). The node keeps its
//!   value hash, which commits to everything below it, so the parent's new
//!   root hash comes out right without opening the subtree, and propagation
//!   carries it up like any other change. A moved reference is followed from
//!   its new key, as a reference the batch writes is, and commits to what it
//!   reaches there after the batch ([`PlannedMoves::commit_reference`]).
//! - After the apply, [`GroveDb::move_subtree_storage`] copies every record
//!   of a moved tree from the storage prefixes under its old path to the ones
//!   under its new path and clears the old ones. It reads through the
//!   transaction, which does not see the pending storage batch, so every
//!   move reads the state from before the batch: in a swap each subtree is
//!   read before the other lands on its prefixes. Within the storage batch a
//!   put wins over a delete of the same key, so a record both left and
//!   landed on keeps what landed, whatever order the moves run in.
//!
//! [`validate_move_ops`] runs first, whether or not the batch consistency
//! check is on, because the copy depends on its rules: no other op may write
//! under a moved key or its target (the copy would overwrite it, or it would
//! be lost with the old prefixes), and no op may delete an ancestor (the
//! deletion's cleanup walks the state from before the batch and would never
//! find the copy).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into_no_add, cost_return_on_error_no_add,
    storage_cost::{key_value_cost::KeyValueStorageCost, StorageCost},
    CostResult, CostsExt, OperationCost,
};
use grovedb_element::{reference_path::ReferencePathType, ElementType};
use grovedb_merk::{
    element::{costs::ElementCostExtensions, tree_type::ElementTreeTypeExtensions},
    tree::{combine_hash, value_hash, TreeNode},
    CryptoHash, Merk, Op,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbTransactionContext, RocksDbStorage},
    RawIterator, Storage, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use super::{GroveOp, KeyInfoPath, QualifiedGroveDbOp};
use crate::{
    element::MaxReferenceHop,
    operations::indexed_tree::indexed_element_axes,
    util::{check_v1_slot_enabled, subtree_path_with_key, MAX_KEY_LENGTH},
    Element, Error, GroveDb, Transaction,
};

/// A tree a batch moved, whose storage [`GroveDb::move_subtree_storage`]
/// copies after the apply.
#[derive(Debug, Clone)]
pub(crate) struct MovedSubtree {
    /// The element's qualified path before the move.
    pub(crate) from: Vec<Vec<u8>>,
    /// The element's qualified path after the move.
    pub(crate) to: Vec<Vec<u8>>,
    /// The moved element; its type says how its storage is laid out.
    pub(crate) element: Element,
}

/// The path and keys of a move, copied out of its op.
struct MoveSpec {
    path: Vec<Vec<u8>>,
    key: Vec<u8>,
    new_key: Vec<u8>,
}

fn known_path(path: &KeyInfoPath) -> Option<Vec<Vec<u8>>> {
    path.iterator()
        .map(|key_info| match key_info {
            super::KeyInfo::KnownKey(key) => Some(key.clone()),
            super::KeyInfo::MaxKeySize { .. } => None,
        })
        .collect()
}

/// Qualified positions a batch moves elements from or to, with the path
/// lengths they sit at: a path is at or under one of them only if its prefix
/// of one of those lengths is, so a check hashes one prefix per length
/// (usually one) rather than every prefix.
#[derive(Debug, Default)]
pub(crate) struct MovedPositions {
    positions: HashSet<Vec<Vec<u8>>>,
    lengths: BTreeSet<usize>,
}

impl MovedPositions {
    pub(crate) fn insert(&mut self, position: Vec<Vec<u8>>) {
        self.lengths.insert(position.len());
        self.positions.insert(position);
    }

    /// Whether `path` is one of the positions or lies under one.
    pub(crate) fn covers(&self, path: &[Vec<u8>]) -> bool {
        self.lengths
            .iter()
            .take_while(|&&len| len <= path.len())
            .any(|&len| self.positions.contains(&path[..len]))
    }
}

/// Check the moves in `ops` before anything is read: the grove version
/// accepts them, each names a new key, no two land on one key, a target is
/// left alone except by a move that frees it, nothing else in the batch
/// works under a moved key or a target, and nothing deletes the tree a move
/// happens in or one of its ancestors.
///
/// Runs whether or not the batch consistency check is enabled: the storage
/// copy relies on these rules (see the module docs).
pub(super) fn validate_move_ops(
    ops: &[QualifiedGroveDbOp],
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    let mut moves = Vec::new();
    for op in ops {
        let GroveOp::Move { new_key } = &op.op else {
            continue;
        };
        let (Some(path), Some(super::KeyInfo::KnownKey(key))) = (known_path(&op.path), &op.key)
        else {
            return Err(Error::InvalidBatchOperation(
                "a move needs a known path and a known key",
            ));
        };
        moves.push(MoveSpec {
            path,
            key: key.clone(),
            new_key: new_key.clone(),
        });
    }
    if moves.is_empty() {
        return Ok(());
    }

    check_v1_slot_enabled(
        "apply_batch: move",
        grove_version.grovedb_versions.apply_batch.move_element,
    )?;

    // Every moved key, with the number of ops the batch files at it, and
    // every target: nothing else in the batch may work at or under them.
    let mut sources: HashMap<Vec<Vec<u8>>, usize> = HashMap::new();
    let mut targets: HashSet<Vec<Vec<u8>>> = HashSet::new();
    let mut moved_positions = MovedPositions::default();
    for spec in &moves {
        if spec.new_key == spec.key {
            return Err(Error::InvalidBatchOperation(
                "a move needs a new key different from its key",
            ));
        }
        if spec.new_key.len() > MAX_KEY_LENGTH {
            return Err(Error::InvalidInput("key length must be at most 255 bytes"));
        }
        let path = SubtreePath::from(spec.path.as_slice());
        let source = subtree_path_with_key(&path, &spec.key);
        let target = subtree_path_with_key(&path, &spec.new_key);
        if !targets.insert(target.clone()) {
            return Err(Error::InvalidBatchOperation(
                "two moves in one batch have the same target",
            ));
        }
        moved_positions.insert(source.clone());
        moved_positions.insert(target);
        sources.insert(source, 0);
    }

    for op in ops {
        // The path an op works in (for a keyless append, the tree it appends
        // to): a moved position at or above it means it works under a moved
        // key or a target.
        let mut position = op.path.to_path();
        if moved_positions.covers(&position) {
            return Err(Error::InvalidBatchOperation(
                "a batch that moves an element cannot also change anything under its key or its \
                 target",
            ));
        }
        let Some(key) = &op.key else {
            continue;
        };
        position.push(key.get_key_clone());
        // Without the consistency check a second op at a moved key would
        // silently replace the move, or be replaced by it.
        if let Some(ops_at_source) = sources.get_mut(&position) {
            *ops_at_source += 1;
            if *ops_at_source > 1 {
                return Err(Error::InvalidBatchOperation(
                    "a moved key cannot carry another operation in the same batch",
                ));
            }
        }
        if targets.contains(&position) && !matches!(op.op, GroveOp::Move { .. }) {
            return Err(Error::InvalidBatchOperation(
                "the target of a move can only be freed by moving its element away in the same \
                 batch",
            ));
        }
    }
    let deleted_positions = super::explicitly_deleted_positions(ops);
    for spec in &moves {
        if (1..=spec.path.len()).any(|len| deleted_positions.contains(&spec.path[..len])) {
            return Err(Error::InvalidBatchOperation(
                "a batch cannot move an element inside a tree it deletes",
            ));
        }
    }
    Ok(())
}

/// Partial batches do not carry moves: a move's storage copy runs after the
/// whole apply, which a paused batch does not reach in one piece.
pub(super) const PARTIAL_BATCH_MOVE_REFUSAL: &str =
    "partial batches cannot move elements; use a full batch";

/// Cost estimation does not carry moves: a move's cost grows with the
/// subtree it carries, which a layer estimate does not describe.
#[cfg(feature = "estimated_costs")]
pub(super) const ESTIMATED_COSTS_MOVE_REFUSAL: &str =
    "costs are not estimated for a batch that moves an element: the cost grows with the moved \
     subtree";

/// Refuse `ops` if they carry a move, for a batch path that cannot (see
/// [`PARTIAL_BATCH_MOVE_REFUSAL`] and `ESTIMATED_COSTS_MOVE_REFUSAL`).
pub(super) fn refuse_moves(ops: &[QualifiedGroveDbOp], reason: &str) -> Result<(), Error> {
    if ops.iter().any(|op| matches!(op.op, GroveOp::Move { .. })) {
        return Err(Error::NotSupported(reason.to_owned()));
    }
    Ok(())
}

/// The error of a match arm for `GroveOp::Move` that the move's own checks
/// keep unreachable (the arm exists because the match is exhaustive):
/// reaching it means one of those checks is missing.
pub(super) fn unrefused_move_error() -> Error {
    Error::CorruptedCodeExecution("a move reached a step its validation should have refused")
}

/// Refuse to resolve a reference at or under a moved key or a move's target
/// (`moved_positions`): the stored state the resolution would read is
/// leaving, or not there yet.
pub(super) fn refuse_reference_into_moved_element(
    qualified_path: &[Vec<u8>],
    moved_positions: &MovedPositions,
) -> Result<(), Error> {
    if moved_positions.covers(qualified_path) {
        return Err(Error::InvalidBatchOperation(
            "references can not point to an element this batch moves, or into its subtree",
        ));
    }
    Ok(())
}

/// A backward-reference participant cannot be moved anywhere in a move: its
/// referrers, or its own forward path, name where it is.
fn participant_refusal() -> Error {
    Error::NotSupported(
        "an element that takes part in backward references cannot be moved".to_owned(),
    )
}

/// Refuse a moved element the move cannot carry, given whether its key
/// changes length:
///
/// - a backward-reference participant;
/// - a reference whose path is built from its own key (a cousin or
///   removed-cousin reference), which a rename would point at another
///   element;
/// - an element with flags moving to a key of another length: flags may
///   count an element's stored bytes per epoch, and a move keeps the
///   element's bytes, its flags with them, as they are, so the bytes it is
///   billed for would no longer match what its flags count.
///
/// What the move does to references stored below the element, or naming
/// its key from elsewhere, is described on
/// [`QualifiedGroveDbOp::move_op`]; the one the move itself would retarget
/// is refused during the copy ([`refuse_parent_key_reference`]).
fn refuse_unmovable(element: &Element, changes_key_length: bool) -> Result<(), Error> {
    if element.supports_backward_references() {
        return Err(participant_refusal());
    }
    if let Element::Reference(reference_path, ..)
    | Element::ReferenceWithSumItem(reference_path, ..) = element.underlying()
    {
        // Exhaustive so that a new path type has to say whether it reads
        // the reference's own key.
        let reads_own_key = match reference_path {
            ReferencePathType::CousinReference(_)
            | ReferencePathType::RemovedCousinReference(_) => true,
            ReferencePathType::AbsolutePathReference(_)
            | ReferencePathType::UpstreamRootHeightReference(..)
            | ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(..)
            | ReferencePathType::UpstreamFromElementHeightReference(..)
            | ReferencePathType::SiblingReference(_) => false,
        };
        if reads_own_key {
            return Err(Error::NotSupported(
                "a cousin or removed-cousin reference resolves through its own key, so moving it \
                 would point it at another element; delete and insert it instead"
                    .to_owned(),
            ));
        }
    }
    if changes_key_length && element.get_flags().is_some() {
        return Err(Error::NotSupported(
            "an element with flags can only move to a key of the same length: its flags may \
             count its stored bytes, which a move does not rewrite"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Refuse a reference stored directly under a moved element whose path ends
/// with its parent's key (`UpstreamRootHeightWithParentPathAdditionReference`):
/// that key is the moved one, so the move would point it at another element.
fn refuse_parent_key_reference(element: &Element) -> Result<(), Error> {
    let (Element::Reference(reference_path, ..)
    | Element::ReferenceWithSumItem(reference_path, ..)) = element.underlying()
    else {
        return Ok(());
    };
    // Exhaustive so that a new path type has to say whether, stored directly
    // under the moved element, it reads that element's key.
    let reads_parent_key = match reference_path {
        ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(..) => true,
        ReferencePathType::AbsolutePathReference(_)
        | ReferencePathType::UpstreamRootHeightReference(..)
        | ReferencePathType::UpstreamFromElementHeightReference(..)
        | ReferencePathType::CousinReference(_)
        | ReferencePathType::RemovedCousinReference(_)
        | ReferencePathType::SiblingReference(_) => false,
    };
    if reads_parent_key {
        return Err(Error::NotSupported(
            "a reference directly under a moved element that appends its parent's key resolves \
             through the moved key, so the move would point it at another element"
                .to_owned(),
        ));
    }
    Ok(())
}

/// A Merk read failure while planning a move: a failed storage read keeps
/// its type, anything else is reported as corrupted data with `context`.
fn merk_read_error(error: grovedb_merk::Error, context: String) -> Error {
    match error {
        grovedb_merk::Error::StorageError(_) => Error::from(error),
        error => Error::CorruptedData(format!("{context}: {error}")),
    }
}

/// A reference a level moves, which must still resolve from its new key.
pub(super) struct MovedReference {
    pub(super) new_key: Vec<u8>,
    pub(super) path: ReferencePathType,
    pub(super) max_hop: MaxReferenceHop,
}

/// What [`plan_level_moves`] derives from one level's moves.
pub(super) struct PlannedMoves {
    /// The Merk ops on the parent Merk, one per key.
    pub(super) merk_ops: BTreeMap<Vec<u8>, Op>,
    /// The moved trees whose storage must follow.
    pub(super) moved_subtrees: Vec<MovedSubtree>,
    /// Every moved reference; see [`Self::commit_reference`].
    pub(super) moved_references: Vec<MovedReference>,
}

impl PlannedMoves {
    /// Commit the reference moved to `new_key` to `referenced_value_hash`,
    /// the value hash its path reaches from there after the batch, as a
    /// reference the batch writes is: its node value hash becomes
    /// `combine_hash(value_hash(value), referenced_value_hash)`. Its bytes
    /// do not change, so neither does what the move bills for them.
    pub(super) fn commit_reference(
        &mut self,
        new_key: &[u8],
        referenced_value_hash: &CryptoHash,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let Some(Op::PutMoved(value, node_value_hash, ..)) = self.merk_ops.get_mut(new_key) else {
            return Err(Error::CorruptedCodeExecution(
                "a moved reference has no moved put at its new key",
            ))
            .wrap_with_cost(cost);
        };
        let own_value_hash = value_hash(value).unwrap_add_cost(&mut cost);
        *node_value_hash =
            combine_hash(&own_value_hash, referenced_value_hash).unwrap_add_cost(&mut cost);
        Ok(()).wrap_with_cost(cost)
    }
}

/// Turn the moves filed at one level into Merk ops on the parent Merk at
/// `path`, given as `(key, new_key)` pairs.
///
/// Each element and its node value hash are read at the old key from the
/// Merk's state before this level applies. A target must be absent unless
/// another move at this level frees it. Both reads retain the nodes they
/// fetch, which applying the moves walks again.
pub(super) fn plan_level_moves<'db, S: StorageContext<'db>>(
    merk: &Merk<S>,
    path: &[Vec<u8>],
    moves: Vec<(Vec<u8>, Vec<u8>)>,
    grove_version: &GroveVersion,
) -> CostResult<PlannedMoves, Error> {
    let mut cost = OperationCost::default();
    let in_tree_type = merk.tree_type;
    let subtree_path = SubtreePath::from(path);
    let freed: HashSet<Vec<u8>> = moves.iter().map(|(key, _)| key.clone()).collect();
    let mut merk_ops: BTreeMap<Vec<u8>, Op> = BTreeMap::new();
    let mut moved_subtrees = Vec::new();
    let mut moved_references = Vec::new();

    for (key, new_key) in moves {
        // Retains the nodes it reads: applying the move walks the same path.
        let stored = cost_return_on_error!(
            &mut cost,
            merk.observe_value_and_value_hash(
                &key,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| merk_read_error(
                e,
                format!(
                    "unable to read the element to move at key {}",
                    hex::encode(&key)
                )
            ))
        );
        let Some((value, value_hash)) = stored else {
            return Err(Error::PathKeyNotFound(format!(
                "cannot move key {} at path {}: it does not exist",
                hex::encode(&key),
                path.iter().map(hex::encode).collect::<Vec<_>>().join("/"),
            )))
            .wrap_with_cost(cost);
        };
        let element =
            cost_return_on_error_into_no_add!(cost, Element::deserialize(&value, grove_version));
        cost_return_on_error_no_add!(cost, refuse_unmovable(&element, new_key.len() != key.len()));

        if !freed.contains(&new_key) {
            let occupied = cost_return_on_error!(
                &mut cost,
                merk.observe_old_value(
                    &new_key,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    &mut |_, _| {},
                    grove_version,
                )
                .map_err(|e| merk_read_error(
                    e,
                    format!(
                        "unable to check whether the move target {} exists",
                        hex::encode(&new_key)
                    )
                ))
            );
            if occupied {
                return Err(Error::InvalidBatchOperation(
                    "the target of a move already exists; free it by moving its element away in \
                     the same batch, or move through a temporary key",
                ))
                .wrap_with_cost(cost);
            }
        }

        let feature_type =
            cost_return_on_error_into_no_add!(cost, element.get_feature_type(in_tree_type));
        // In a swap or a chain the old key may already hold the element
        // another move brings in; that put stands.
        merk_ops.entry(key.clone()).or_insert(Op::DeleteMoved);
        merk_ops.insert(
            new_key.clone(),
            Op::PutMoved(value, value_hash, key.len() as u32, feature_type),
        );
        if let Element::Reference(reference_path, max_hop, _)
        | Element::ReferenceWithSumItem(reference_path, max_hop, ..) = element.underlying()
        {
            moved_references.push(MovedReference {
                new_key: new_key.clone(),
                path: reference_path.clone(),
                max_hop: *max_hop,
            });
        } else if element.is_any_tree() {
            moved_subtrees.push(MovedSubtree {
                from: subtree_path_with_key(&subtree_path, &key),
                to: subtree_path_with_key(&subtree_path, &new_key),
                element,
            });
        }
    }

    Ok(PlannedMoves {
        merk_ops,
        moved_subtrees,
        moved_references,
    })
    .wrap_with_cost(cost)
}

/// What a record copied to a moved tree's new prefix is billed: replaced
/// bytes at its stored size, key and value with their length prefixes, as a
/// write of it would otherwise add. The storage layer checks the value half
/// against the bytes it writes, as for any put.
fn moved_record_cost(prefixed_key_len: u32, value_len: u32) -> KeyValueStorageCost {
    KeyValueStorageCost {
        key_storage_cost: StorageCost {
            replaced_bytes: prefixed_key_len + prefixed_key_len.required_space() as u32,
            ..Default::default()
        },
        value_storage_cost: StorageCost {
            replaced_bytes: value_len + value_len.required_space() as u32,
            ..Default::default()
        },
        new_node: false,
        needs_value_verification: true,
        prepaid: false,
    }
}

/// Copy the record `iter` is at, if any, and step past it: hand it to
/// `inspect`, then to `land` (with the cost a moved record is billed, its
/// prefixed key `prefix_len + key` bytes long) and to `clear`, and return
/// what `inspect` found. `None` once the records run out. Fails rather than
/// end early when the iterator stopped on an error, which `valid` reports
/// like the end of its records: a move must not commit a partial copy.
fn copy_next_record<I: RawIterator, T>(
    iter: &mut I,
    prefix_len: usize,
    inspect: impl FnOnce(&[u8], &[u8]) -> Result<T, Error>,
    land: impl FnOnce(&[u8], &[u8], KeyValueStorageCost) -> CostResult<(), Error>,
    clear: impl FnOnce(&[u8]) -> CostResult<(), Error>,
) -> CostResult<Option<T>, Error> {
    let mut cost = OperationCost::default();
    if !iter.valid().unwrap_add_cost(&mut cost) {
        cost_return_on_error_no_add!(cost, iter.status().map_err(Error::StorageError));
        return Ok(None).wrap_with_cost(cost);
    }
    let (Some(key), Some(value)) = (
        iter.key().unwrap_add_cost(&mut cost),
        iter.value().unwrap_add_cost(&mut cost),
    ) else {
        return Err(Error::CorruptedData(
            "a raw iterator at a record returned no key or value".to_owned(),
        ))
        .wrap_with_cost(cost);
    };
    let found = cost_return_on_error_no_add!(cost, inspect(key, value));
    let prefixed_key_len = (prefix_len + key.len()) as u32;
    cost_return_on_error!(
        &mut cost,
        land(
            key,
            value,
            moved_record_cost(prefixed_key_len, value.len() as u32)
        )
    );
    cost_return_on_error!(&mut cost, clear(key));
    iter.next().unwrap_add_cost(&mut cost);
    Ok(Some(found)).wrap_with_cost(cost)
}

/// [`copy_next_record`] for every record of `iter`, from the first.
fn copy_records<I: RawIterator>(
    iter: &mut I,
    prefix_len: usize,
    mut inspect: impl FnMut(&[u8], &[u8]) -> Result<(), Error>,
    mut land: impl FnMut(&[u8], &[u8], KeyValueStorageCost) -> CostResult<(), Error>,
    mut clear: impl FnMut(&[u8]) -> CostResult<(), Error>,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    iter.seek_to_first().unwrap_add_cost(&mut cost);
    loop {
        let copied = cost_return_on_error!(
            &mut cost,
            copy_next_record(iter, prefix_len, &mut inspect, &mut land, &mut clear)
        );
        if copied.is_none() {
            return Ok(()).wrap_with_cost(cost);
        }
    }
}

/// A record under a moved tree that cannot be decoded, at `key` in the
/// namespace of `path`.
fn moved_record_error(path: &[Vec<u8>], key: &[u8], error: impl std::fmt::Display) -> Error {
    Error::CorruptedData(format!(
        "unable to decode the record at key {} under {} while moving it: {error}",
        hex::encode(key),
        path.iter().map(hex::encode).collect::<Vec<_>>().join("/"),
    ))
}

/// Classify a record of a Merk tree the copy moves, at `key` in the
/// namespace of `path` (before the move), by its element's type byte:
/// refuse a backward-reference participant, and a reference stored directly
/// under the moved element that appends its parent's key; hand back a nested
/// tree, as its key and element, for the copy to descend into. The element
/// is decoded only for those two cases.
fn classify_moved_record(
    path: &[Vec<u8>],
    directly_under: bool,
    key: &[u8],
    value: &[u8],
    grove_version: &GroveVersion,
) -> Result<Option<(Vec<u8>, Element)>, Error> {
    let node =
        TreeNode::decode_raw_for_read(value).map_err(|e| moved_record_error(path, key, e))?;
    let element_type = ElementType::from_serialized_value(node.value_as_slice())
        .map_err(|e| moved_record_error(path, key, e))?;
    if element_type.supports_backward_references() {
        return Err(participant_refusal());
    }
    if element_type.is_tree() {
        let child = Element::deserialize(node.value_as_slice(), grove_version)
            .map_err(|e| moved_record_error(path, key, e))?;
        Ok(Some((key.to_vec(), child)))
    } else if directly_under && element_type.is_reference() {
        let reference = Element::deserialize(node.value_as_slice(), grove_version)
            .map_err(|e| moved_record_error(path, key, e))?;
        refuse_parent_key_reference(&reference)?;
        Ok(None)
    } else {
        Ok(None)
    }
}

/// The open scan of a tree the copy moves: its paths before and after the
/// move, whether its records are Merk nodes, and the scan of its namespace.
struct TreeScan<'db> {
    from: Vec<Vec<u8>>,
    to: Vec<Vec<u8>>,
    is_merk: bool,
    prefix_len: usize,
    source: PrefixedRocksDbTransactionContext<'db>,
    destination: PrefixedRocksDbTransactionContext<'db>,
    iter: <PrefixedRocksDbTransactionContext<'db> as StorageContext<'db>>::RawIterator,
}

impl GroveDb {
    /// Copy every record of a tree a batch moved from the storage prefixes
    /// under its old path to the ones under its new path, and clear the old
    /// ones: its own namespace, an indexed tree's configured secondary
    /// namespaces, and the same for every tree nested in it, skipping the
    /// namespaces its element shows empty. A Merk record's element is
    /// classified by its type byte (to find nested trees and refuse
    /// backward-reference participants) and decoded only for a nested tree
    /// or a reference directly under the moved element; a non-Merk tree's
    /// records are copied without decoding.
    ///
    /// The walk is depth first: a nested tree is copied when its record is
    /// met, before the scan of its parent goes on. The open scans live on the
    /// heap, one per level of depth, so neither the call stack nor what the
    /// walk holds besides the storage batch grows with the subtree's depth
    /// or width beyond that.
    ///
    /// Reads go through the transaction and see the state from before the
    /// batch; writes stream into `batch` (see the module docs).
    pub(crate) fn move_subtree_storage(
        &self,
        moved: &MovedSubtree,
        transaction: &Transaction,
        batch: &StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let mut scans = Vec::new();
        if let Some(scan) = cost_return_on_error!(
            &mut cost,
            self.start_tree_copy(
                moved.from.clone(),
                moved.to.clone(),
                &moved.element,
                transaction,
                batch,
            )
        ) {
            scans.push(scan);
        }
        loop {
            // The first scan is the moved element's own namespace.
            let directly_under = scans.len() == 1;
            let Some(TreeScan {
                from,
                to,
                is_merk,
                prefix_len,
                source,
                destination,
                iter,
            }) = scans.last_mut()
            else {
                return Ok(()).wrap_with_cost(cost);
            };
            let is_merk = *is_merk;
            let copied = cost_return_on_error!(
                &mut cost,
                copy_next_record(
                    iter,
                    *prefix_len,
                    |key, value| {
                        if is_merk {
                            classify_moved_record(from, directly_under, key, value, grove_version)
                        } else {
                            Ok(None)
                        }
                    },
                    |key, value, record_cost| {
                        destination
                            .put(key, value, None, Some(record_cost))
                            .map_err(Into::into)
                    },
                    // Billed where it lands. In a swap the other move's put of
                    // the same record wins over this delete.
                    |key| {
                        source
                            .delete(key, Some(KeyValueStorageCost::default()))
                            .map_err(Into::into)
                    },
                )
            );
            match copied {
                // This namespace is done; the scan of its parent goes on.
                None => {
                    scans.pop();
                }
                Some(None) => {}
                Some(Some((key, child))) => {
                    let mut child_from = from.clone();
                    child_from.push(key.clone());
                    let mut child_to = to.clone();
                    child_to.push(key);
                    if let Some(scan) = cost_return_on_error!(
                        &mut cost,
                        self.start_tree_copy(child_from, child_to, &child, transaction, batch)
                    ) {
                        scans.push(scan);
                    }
                }
            }
        }
    }

    /// Start copying the tree `element`, at `from` before the move and `to`
    /// after it: copy its configured secondary namespaces, and open the scan
    /// of its own namespace unless its element shows it empty.
    fn start_tree_copy<'db>(
        &'db self,
        from: Vec<Vec<u8>>,
        to: Vec<Vec<u8>>,
        element: &Element,
        transaction: &'db Transaction<'db>,
        batch: &'db StorageBatch,
    ) -> CostResult<Option<TreeScan<'db>>, Error> {
        let mut cost = OperationCost::default();
        let is_merk = !element.uses_non_merk_data_storage();
        // A Merk without a root key holds no records; a non-Merk tree's data
        // is there whatever its element says.
        let copies_primary = !is_merk
            || element
                .root_key_and_tree_type()
                .is_some_and(|(root_key, _)| root_key.is_some());
        let secondary_axes: Vec<_> = if element.is_indexed_tree() {
            let axes = cost_return_on_error_no_add!(cost, indexed_element_axes(element));
            axes.into_iter()
                .filter(|(_, root_key)| root_key.is_some())
                .map(|(axis, _)| axis)
                .collect()
        } else {
            Vec::new()
        };
        if !copies_primary && secondary_axes.is_empty() {
            return Ok(None).wrap_with_cost(cost);
        }

        let from_prefix = RocksDbStorage::build_prefix(SubtreePath::from(from.as_slice()))
            .unwrap_add_cost(&mut cost);
        let to_prefix = RocksDbStorage::build_prefix(SubtreePath::from(to.as_slice()))
            .unwrap_add_cost(&mut cost);
        for axis in secondary_axes {
            let from_secondary = RocksDbStorage::secondary_prefix_for(&from_prefix, axis.tag())
                .unwrap_add_cost(&mut cost);
            let to_secondary = RocksDbStorage::secondary_prefix_for(&to_prefix, axis.tag())
                .unwrap_add_cost(&mut cost);
            cost_return_on_error!(
                &mut cost,
                self.move_namespace(from_secondary, to_secondary, transaction, batch)
            );
        }
        if !copies_primary {
            return Ok(None).wrap_with_cost(cost);
        }

        let source = self
            .db
            .get_transactional_storage_context_by_subtree_prefix(
                from_prefix,
                Some(batch),
                transaction,
            )
            .unwrap_add_cost(&mut cost);
        let destination = self
            .db
            .get_transactional_storage_context_by_subtree_prefix(
                to_prefix,
                Some(batch),
                transaction,
            )
            .unwrap_add_cost(&mut cost);
        let mut iter = source.raw_iter();
        iter.seek_to_first().unwrap_add_cost(&mut cost);
        Ok(Some(TreeScan {
            from,
            to,
            is_merk,
            prefix_len: to_prefix.len(),
            source,
            destination,
            iter,
        }))
        .wrap_with_cost(cost)
    }

    /// Stream every record of the `from` namespace into the `to` namespace
    /// and delete it from `from`; for namespaces whose records hold nothing
    /// the copy has to look at (indexed secondaries).
    fn move_namespace(
        &self,
        from: crate::SubtreePrefix,
        to: crate::SubtreePrefix,
        transaction: &Transaction,
        batch: &StorageBatch,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let source = self
            .db
            .get_transactional_storage_context_by_subtree_prefix(from, Some(batch), transaction)
            .unwrap_add_cost(&mut cost);
        let destination = self
            .db
            .get_transactional_storage_context_by_subtree_prefix(to, Some(batch), transaction)
            .unwrap_add_cost(&mut cost);

        let mut iter = source.raw_iter();
        copy_records(
            &mut iter,
            to.len(),
            |_, _| Ok(()),
            |key, value, record_cost| {
                destination
                    .put(key, value, None, Some(record_cost))
                    .map_err(Into::into)
            },
            // Billed where it lands. In a swap the other move's put of the
            // same record wins over this delete.
            |key| {
                source
                    .delete(key, Some(KeyValueStorageCost::default()))
                    .map_err(Into::into)
            },
        )
        .add_cost(cost)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use grovedb_costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        CostContext, CostsExt, OperationCost,
    };
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageBatch, StorageContext,
    };
    use grovedb_version::version::GroveVersion;
    use integer_encoding::VarInt;

    use super::{copy_records, MovedSubtree};
    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    /// An iterator over in-memory records that can stop on an error once
    /// they run out, or claim a record it does not hand over. It charges one
    /// seek per positioning call and nothing else.
    struct ScriptedIterator {
        records: Vec<(Vec<u8>, Vec<u8>)>,
        position: usize,
        fails_after_last: bool,
        withholds_records: bool,
    }

    impl ScriptedIterator {
        fn record(&self) -> Option<&(Vec<u8>, Vec<u8>)> {
            if self.withholds_records {
                None
            } else {
                self.records.get(self.position)
            }
        }
    }

    impl RawIterator for ScriptedIterator {
        fn seek_to_first(&mut self) -> CostContext<()> {
            self.position = 0;
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn seek_to_last(&mut self) -> CostContext<()> {
            self.position = self.records.len().saturating_sub(1);
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn seek<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn seek_for_prev<K: AsRef<[u8]>>(&mut self, _key: K) -> CostContext<()> {
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn next(&mut self) -> CostContext<()> {
            self.position += 1;
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn prev(&mut self) -> CostContext<()> {
            self.position = self.position.saturating_sub(1);
            ().wrap_with_cost(OperationCost::with_seek_count(1))
        }

        fn value(&self) -> CostContext<Option<&[u8]>> {
            self.record()
                .map(|(_, value)| value.as_slice())
                .wrap_with_cost(OperationCost::default())
        }

        fn key(&self) -> CostContext<Option<&[u8]>> {
            self.record()
                .map(|(key, _)| key.as_slice())
                .wrap_with_cost(OperationCost::default())
        }

        fn valid(&self) -> CostContext<bool> {
            (self.position < self.records.len()).wrap_with_cost(OperationCost::default())
        }

        fn status(&self) -> Result<(), grovedb_storage::error::Error> {
            if self.fails_after_last && self.position >= self.records.len() {
                Err(grovedb_storage::error::Error::StorageError(
                    "injected read failure".to_owned(),
                ))
            } else {
                Ok(())
            }
        }
    }

    fn scripted(fails_after_last: bool, withholds_records: bool) -> ScriptedIterator {
        ScriptedIterator {
            records: vec![(b"a".to_vec(), vec![1; 10]), (b"b".to_vec(), vec![2; 20])],
            position: 0,
            fails_after_last,
            withholds_records,
        }
    }

    /// Run `copy_records` over `iter`, counting the records it lands and
    /// clears.
    fn copy(iter: &mut ScriptedIterator) -> (CostContext<Result<(), Error>>, usize, usize) {
        let (landed, cleared) = (Cell::new(0), Cell::new(0));
        let result = copy_records(
            iter,
            32,
            |_, _| Ok(()),
            |_, _, _| {
                landed.set(landed.get() + 1);
                Ok(()).wrap_with_cost(OperationCost::default())
            },
            |_| {
                cleared.set(cleared.get() + 1);
                Ok(()).wrap_with_cost(OperationCost::default())
            },
        );
        (result, landed.get(), cleared.get())
    }

    #[test]
    fn a_read_error_after_staged_records_fails_the_copy() {
        let (result, landed, cleared) = copy(&mut scripted(true, false));
        // Both records were staged into the batch before the error, which
        // the batch then discards with everything else.
        assert_eq!((landed, cleared), (2, 2));
        // One seek to start, one to step past each record.
        assert_eq!(result.cost.seek_count, 3);
        assert!(
            matches!(&result.value, Err(Error::StorageError(e)) if e.to_string().contains("injected read failure")),
            "got {:?}",
            result.value
        );

        let (result, ..) = copy(&mut scripted(false, false));
        assert!(result.value.is_ok());
    }

    #[test]
    fn a_valid_iterator_without_a_record_fails_the_copy() {
        let (result, landed, cleared) = copy(&mut scripted(false, true));
        assert_eq!((landed, cleared), (0, 0));
        assert!(
            matches!(&result.value, Err(Error::CorruptedData(_))),
            "got {:?}",
            result.value
        );
    }

    #[test]
    fn a_fresh_target_is_checked_on_the_path_the_move_walks() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::new_item(vec![1]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("item");
        let tx = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path(
                SubtreePath::from([TEST_LEAF].as_ref()),
                &tx,
                None,
                grove_version,
            )
            .unwrap()
            .expect("merk");

        // The root, "a", is loaded when the Merk opens, and "b" would hang
        // off its empty right link: neither the source nor the target needs
        // a storage read.
        let planned = super::plan_level_moves(
            &merk,
            &[TEST_LEAF.to_vec()],
            vec![(b"a".to_vec(), b"b".to_vec())],
            grove_version,
        );
        assert_eq!(planned.cost.seek_count, 0);
        planned.value.expect("plan");
    }

    #[test]
    fn a_moved_record_is_billed_exactly() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"src",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("tree");
        for (key, size) in [(b"a".as_slice(), 3usize), (b"bb", 40), (b"ccc", 200)] {
            db.insert(
                [TEST_LEAF, b"src"].as_ref(),
                key,
                Element::new_item(vec![7; size]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("item");
        }
        let from = vec![TEST_LEAF.to_vec(), b"src".to_vec()];
        let to = vec![TEST_LEAF.to_vec(), b"dst".to_vec()];
        let tx = db.start_transaction();

        // The stored records, as (prefixed key length, value length).
        let records: Vec<(u32, u32)> = {
            let source = db
                .db
                .get_transactional_storage_context(SubtreePath::from(from.as_slice()), None, &tx)
                .unwrap();
            let mut iter = source.raw_iter();
            iter.seek_to_first().unwrap();
            let mut records = Vec::new();
            while iter.valid().unwrap() {
                let key_len = iter.key().unwrap().expect("key").len() as u32;
                let value_len = iter.value().unwrap().expect("value").len() as u32;
                records.push((32 + key_len, value_len));
                iter.next().unwrap();
            }
            records
        };
        assert_eq!(records.len(), 3);
        let element = db
            .get_raw(
                [TEST_LEAF].as_ref().into(),
                b"src",
                Some(&tx),
                grove_version,
            )
            .unwrap()
            .expect("element");

        let batch = StorageBatch::new();
        let copy = db.move_subtree_storage(
            &MovedSubtree {
                from: from.clone(),
                to: to.clone(),
                element,
            },
            &tx,
            &batch,
            grove_version,
        );
        copy.value.expect("copy");
        let prefix_hashes = RocksDbStorage::build_prefix(SubtreePath::from(from.as_slice()))
            .cost
            .hash_node_calls
            + RocksDbStorage::build_prefix(SubtreePath::from(to.as_slice()))
                .cost
                .hash_node_calls;
        // A seek to start and one per record; the prefixed key is loaded by
        // `valid`, `key` and `value` alike, the value once, and the scan ends
        // on the fixed 288-byte boundary charge.
        assert_eq!(
            copy.cost,
            OperationCost {
                seek_count: 1 + records.len() as u32,
                storage_loaded_bytes: records
                    .iter()
                    .map(|(key, value)| (3 * key + value) as u64)
                    .sum::<u64>()
                    + 288,
                hash_node_calls: prefix_hashes,
                ..Default::default()
            }
        );

        // Each record is billed as replaced at its stored size, key and value
        // with their length prefixes; its delete from the old prefix bills
        // nothing but the write.
        let commit = db.db.commit_multi_context_batch(batch, Some(&tx));
        commit.value.expect("commit");
        assert_eq!(commit.cost.seek_count, 2 * records.len() as u32);
        assert_eq!(
            commit.cost.storage_cost,
            StorageCost {
                added_bytes: 0,
                replaced_bytes: records
                    .iter()
                    .map(|(key, value)| {
                        key + key.required_space() as u32 + value + value.required_space() as u32
                    })
                    .sum(),
                removed_bytes: NoStorageRemoval,
            }
        );
    }
}
