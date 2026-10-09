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
//!   carries it up like any other change.
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

use std::collections::{BTreeMap, HashMap, HashSet};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into_no_add, cost_return_on_error_no_add,
    storage_cost::{key_value_cost::KeyValueStorageCost, StorageCost},
    CostResult, CostsExt, OperationCost,
};
use grovedb_element::{indexed::IndexAxis, reference_path::ReferencePathType};
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, decode::ElementDecodeExtensions,
        tree_type::ElementTreeTypeExtensions,
    },
    Merk, Op,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageBatch, StorageContext,
};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};
use integer_encoding::VarInt;

use super::{GroveOp, KeyInfoPath, QualifiedGroveDbOp};
use crate::{Element, Error, GroveDb, Transaction};

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

/// The path and keys of a move, borrowed from its op.
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

fn qualified(path: &[Vec<u8>], key: &[u8]) -> Vec<Vec<u8>> {
    let mut qualified = path.to_vec();
    qualified.push(key.to_vec());
    qualified
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

    let slot = grove_version.grovedb_versions.apply_batch.move_element;
    if slot != 1 {
        return Err(GroveVersionError::UnknownVersionMismatch {
            method: "apply_batch: move".to_string(),
            known_versions: vec![1],
            received: slot,
        }
        .into());
    }

    // Qualified paths of every moved key and every target: nothing else in
    // the batch may work at or under them.
    let mut moved_positions: HashSet<Vec<Vec<u8>>> = HashSet::new();
    let mut sources: HashMap<Vec<Vec<u8>>, usize> = HashMap::new();
    let mut targets: HashMap<Vec<Vec<u8>>, usize> = HashMap::new();
    for spec in &moves {
        if spec.new_key == spec.key {
            return Err(Error::InvalidBatchOperation(
                "a move needs a new key different from its key",
            ));
        }
        if spec.new_key.len() > u8::MAX as usize {
            return Err(Error::InvalidInput("key length must be at most 255 bytes"));
        }
        let source = qualified(&spec.path, &spec.key);
        let target = qualified(&spec.path, &spec.new_key);
        sources.insert(source.clone(), 0);
        *targets.entry(target.clone()).or_default() += 1;
        moved_positions.insert(source);
        moved_positions.insert(target);
    }
    if targets.values().any(|&count| count > 1) {
        return Err(Error::InvalidBatchOperation(
            "two moves in one batch have the same target",
        ));
    }

    let mut deleted_positions: HashSet<Vec<Vec<u8>>> = HashSet::new();
    for op in ops {
        // The path an op works in (for a keyless append, the tree it appends
        // to): a moved position at or above it means it works under a moved
        // key or a target.
        let op_path = op.path.to_path();
        if (1..=op_path.len()).any(|len| moved_positions.contains(&op_path[..len])) {
            return Err(Error::InvalidBatchOperation(
                "a batch that moves an element cannot also change anything under its key or its \
                 target",
            ));
        }
        let Some(key) = &op.key else {
            continue;
        };
        let position = qualified(&op_path, key.as_slice());
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
        if targets.contains_key(&position) && !matches!(op.op, GroveOp::Move { .. }) {
            return Err(Error::InvalidBatchOperation(
                "the target of a move can only be freed by moving its element away in the same \
                 batch",
            ));
        }
        if matches!(
            op.op,
            GroveOp::Delete
                | GroveOp::DeleteDontCheckForBackwardsReferences
                | GroveOp::DeleteTree(..)
                | GroveOp::DeleteTreeDontCheckForBackwardsReferences(..)
        ) {
            deleted_positions.insert(position);
        }
    }
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
pub(super) fn refuse_moves_in_partial_batch(ops: &[QualifiedGroveDbOp]) -> Result<(), Error> {
    if ops.iter().any(|op| matches!(op.op, GroveOp::Move { .. })) {
        return Err(Error::NotSupported(
            "partial batches cannot move elements; use a full batch".to_owned(),
        ));
    }
    Ok(())
}

/// Cost estimation does not carry moves: a move's cost grows with the
/// subtree it carries, which a layer estimate does not describe.
pub(super) fn refuse_moves_in_estimated_costs(ops: &[QualifiedGroveDbOp]) -> Result<(), Error> {
    if ops.iter().any(|op| matches!(op.op, GroveOp::Move { .. })) {
        return Err(Error::NotSupported(
            "costs are not estimated for a batch that moves an element: the cost grows with the \
             moved subtree"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Refuse to resolve a reference into an element a batch moves, or into its
/// subtree: the stored state the resolution would read is leaving.
pub(super) fn refuse_reference_into_moved_element(
    qualified_path: &[Vec<u8>],
    ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, GroveOp>,
) -> Result<(), Error> {
    if (1..=qualified_path.len()).any(|len| {
        matches!(
            ops_by_qualified_paths.get(&qualified_path[..len]),
            Some(GroveOp::Move { .. })
        )
    }) {
        return Err(Error::InvalidBatchOperation(
            "references can not point to an element this batch moves, or into its subtree",
        ));
    }
    Ok(())
}

/// Refuse a backward-reference participant anywhere in a move: its
/// referrers, or its own forward path, name where it is.
fn refuse_participant(element: &Element) -> Result<(), Error> {
    if element.supports_backward_references() {
        return Err(Error::NotSupported(
            "an element that takes part in backward references cannot be moved".to_owned(),
        ));
    }
    Ok(())
}

/// Refuse a moved element the move cannot carry: a backward-reference
/// participant, or a reference whose path is built from its own key (a
/// cousin or removed-cousin reference), which a rename would point at
/// another element. Below the moved element no key changes, only a segment
/// of every path, so references there resolve as before.
fn refuse_unmovable(element: &Element) -> Result<(), Error> {
    refuse_participant(element)?;
    if let Element::Reference(reference_path, ..)
    | Element::ReferenceWithSumItem(reference_path, ..) = element.underlying()
        && matches!(
            reference_path,
            ReferencePathType::CousinReference(_) | ReferencePathType::RemovedCousinReference(_)
        )
    {
        return Err(Error::NotSupported(
            "a cousin or removed-cousin reference resolves through its own key, so moving it \
             would point it at another element; delete and insert it instead"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Turn the moves filed at one level into Merk ops on the parent Merk at
/// `path`, given as `(key, new_key)` pairs.
///
/// Each element and its node value hash are read at the old key from the
/// Merk's state before this level applies. A target must be absent unless
/// another move at this level frees it. Returns the Merk ops, sorted with
/// one per key, and the moved trees whose storage must follow.
pub(super) fn plan_level_moves<'db, S: StorageContext<'db>>(
    merk: &Merk<S>,
    path: &[Vec<u8>],
    moves: Vec<(Vec<u8>, Vec<u8>)>,
    grove_version: &GroveVersion,
) -> CostResult<(Vec<(Vec<u8>, Op)>, Vec<MovedSubtree>), Error> {
    let mut cost = OperationCost::default();
    let in_tree_type = merk.tree_type;
    let freed: HashSet<Vec<u8>> = moves.iter().map(|(key, _)| key.clone()).collect();
    let mut merk_ops: BTreeMap<Vec<u8>, Op> = BTreeMap::new();
    let mut moved_subtrees = Vec::new();

    for (key, new_key) in moves {
        let stored = cost_return_on_error!(
            &mut cost,
            merk.get_value_and_value_hash(
                &key,
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| Error::CorruptedData(e.to_string()))
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
        cost_return_on_error_no_add!(cost, refuse_unmovable(&element));

        if !freed.contains(&new_key) {
            let occupied = cost_return_on_error!(
                &mut cost,
                merk.exists(
                    &new_key,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(|e| Error::CorruptedData(e.to_string()))
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
        if element.is_any_tree() {
            moved_subtrees.push(MovedSubtree {
                from: qualified(path, &key),
                to: qualified(path, &new_key),
                element,
            });
        }
    }

    Ok((merk_ops.into_iter().collect(), moved_subtrees)).wrap_with_cost(cost)
}

/// What a record copied to a moved tree's new prefix is billed: replaced
/// bytes at its stored size, key and value with their length prefixes, as a
/// write of it would otherwise add.
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
        needs_value_verification: false,
        prepaid: false,
    }
}

impl GroveDb {
    /// Copy every record of a tree a batch moved from the storage prefixes
    /// under its old path to the ones under its new path, and clear the old
    /// ones: its own namespace, an indexed tree's per-axis secondary
    /// namespaces, and the same for every tree nested in it. Merk records are
    /// decoded only to find nested trees and to refuse a backward-reference
    /// participant; a non-Merk tree's records are copied without decoding.
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

        // Depth first: each entry is a tree still to copy, as its path below
        // the moved element and its element.
        let mut pending: Vec<(Vec<Vec<u8>>, Element)> = vec![(Vec::new(), moved.element.clone())];
        while let Some((relative_path, element)) = pending.pop() {
            let mut from = moved.from.clone();
            from.extend(relative_path.iter().cloned());
            let mut to = moved.to.clone();
            to.extend(relative_path.iter().cloned());
            let from_prefix = RocksDbStorage::build_prefix(SubtreePath::from(from.as_slice()))
                .unwrap_add_cost(&mut cost);
            let to_prefix = RocksDbStorage::build_prefix(SubtreePath::from(to.as_slice()))
                .unwrap_add_cost(&mut cost);

            let is_merk = !element.uses_non_merk_data_storage();
            cost_return_on_error!(
                &mut cost,
                self.move_namespace(from_prefix, to_prefix, transaction, batch, |key, value| {
                    if !is_merk {
                        return Ok(());
                    }
                    let child = Element::raw_decode(value, grove_version)?;
                    refuse_participant(&child)?;
                    if child.is_any_tree() {
                        let mut child_path = relative_path.clone();
                        child_path.push(key.to_vec());
                        pending.push((child_path, child));
                    }
                    Ok(())
                },)
            );

            if element.is_indexed_tree() {
                for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
                    let from_secondary =
                        RocksDbStorage::secondary_prefix_for(&from_prefix, axis.tag())
                            .unwrap_add_cost(&mut cost);
                    let to_secondary = RocksDbStorage::secondary_prefix_for(&to_prefix, axis.tag())
                        .unwrap_add_cost(&mut cost);
                    cost_return_on_error!(
                        &mut cost,
                        self.move_namespace(
                            from_secondary,
                            to_secondary,
                            transaction,
                            batch,
                            |_, _| Ok(()),
                        )
                    );
                }
            }
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Stream every record of the `from` namespace into the `to` namespace
    /// and delete it from `from`, calling `inspect` with each record's key
    /// and stored value first.
    fn move_namespace(
        &self,
        from: crate::SubtreePrefix,
        to: crate::SubtreePrefix,
        transaction: &Transaction,
        batch: &StorageBatch,
        mut inspect: impl FnMut(&[u8], &[u8]) -> Result<(), Error>,
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
        iter.seek_to_first().unwrap_add_cost(&mut cost);
        while iter.valid().unwrap_add_cost(&mut cost) {
            let (Some(key), Some(value)) = (
                iter.key().unwrap_add_cost(&mut cost).map(<[u8]>::to_vec),
                iter.value().unwrap_add_cost(&mut cost).map(<[u8]>::to_vec),
            ) else {
                break;
            };
            cost_return_on_error_no_add!(cost, inspect(&key, &value));
            let prefixed_key_len = (to.len() + key.len()) as u32;
            cost_return_on_error!(
                &mut cost,
                destination
                    .put(
                        &key,
                        &value,
                        None,
                        Some(moved_record_cost(prefixed_key_len, value.len() as u32)),
                    )
                    .map_err(Into::into)
            );
            // Billed where it lands. In a swap the other move's put of the
            // same record wins over this delete.
            cost_return_on_error!(
                &mut cost,
                source
                    .delete(&key, Some(KeyValueStorageCost::default()))
                    .map_err(Into::into)
            );
            iter.next().unwrap_add_cost(&mut cost);
        }
        Ok(()).wrap_with_cost(cost)
    }
}
