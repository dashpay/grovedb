//! Resolving canonical secondary rows into proof nodes that carry the
//! primary value they point at.
//!
//! Every indexed secondary row is a canonical one-hop
//! `ReferenceWithSumItem(SiblingReference(primary_key), Some(1), sum)`
//! (see [`axis_row_reference`]). Merk emits such a row as a
//! `KVValueHashFeatureType` node carrying the REFERENCE's bytes, and
//! leaves dereferencing to the layer above — the same division of labour
//! the regular count-tree proof flow uses. This module is that layer for
//! indexed axes.
//!
//! # The logical-origin rule
//!
//! A secondary Merk lives at a derived storage prefix
//! (`blake3(primary_prefix ‖ axis_tag)`) and has **no `SubtreePath`** —
//! there is no GroveDB path that names it, so the generic path-keyed
//! reference machinery (`follow_reference` / `MerkCache`) cannot express
//! a row's origin at all. Resolution here is therefore purpose-built:
//! a row's logical origin is the INDEXED PRIMARY's path, so
//! `SiblingReference(primary_key)` resolves to `primary_key` in the
//! primary Merk. Nothing in this module treats the derived secondary
//! prefix as a user-visible path, and no caller may.
//!
//! # What the rewrite binds
//!
//! The emitted node is `KVRefValueHash{Count,CountSum}(key,
//! target_bytes, H(reference bytes), ..)`, whose hash reconstruction is
//! `combine_hash(H(reference bytes), H(target_bytes))`. That is exactly
//! the row's committed value hash, so the resolved target value is bound
//! into the secondary root — which the axis verifier in turn binds to the
//! indexed element via `combine_hash_three`. Substituting either the
//! target value or the reference breaks the root.
//!
//! # Immediate-node binding
//!
//! Resolution stops at the primary entry. It does NOT follow a chain to a
//! terminal, because a canonical row binds the IMMEDIATE primary node's
//! committed value hash — see [`INDEXED_SECONDARY_MAX_HOP`]. Ordinary
//! user references keep their terminal semantics; this one-hop rule is
//! selected explicitly here and applies only to indexed rows.
//!
//! [`axis_row_reference`]: crate::operations::indexed_tree::axis_row_reference
//! [`INDEXED_SECONDARY_MAX_HOP`]:
//!     crate::operations::indexed_tree::INDEXED_SECONDARY_MAX_HOP

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::{costs::ElementCostExtensions, get::ElementFetchFromStorageExtensions},
    proofs::{Node, Op},
    tree::{combine_hash, value_hash},
    Merk, TreeFeatureType,
};
use grovedb_storage::{StorageBatch, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{
    operations::indexed_tree::{axis_sort_key_len, indexed_row_target_key},
    Element, Error, GroveDb, Transaction,
};

/// Rewrite every canonical reference row in a secondary proof's op stream
/// into a resolved-value node.
///
/// `primary_merk` must be the indexed primary the secondary mirrors —
/// that is the row's logical origin, and reading the target anywhere else
/// would resolve a `SiblingReference` against the wrong parent.
///
/// Rows that are not references are left untouched, so this is safe to
/// run over any secondary proof.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_axis_reference_nodes<'a, 'db, S: StorageContext<'db>>(
    ops: impl IntoIterator<Item = &'a mut Op>,
    axis: IndexAxis,
    primary_merk: &Merk<S>,
    grovedb: &'db GroveDb,
    primary_path: &[Vec<u8>],
    transaction: &'db Transaction,
    batch: &'db StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    let sort_len = axis_sort_key_len(axis);

    for op in ops {
        let node = match op {
            Op::Push(node) | Op::PushInverted(node) => node,
            _ => continue,
        };
        let Node::KVValueHashFeatureType(key, value, _committed_value_hash, feature_type) = node
        else {
            continue;
        };

        // Only canonical rows are rewritten. Anything else in a secondary
        // is corruption, but this pass is not the checker for that — the
        // node is left alone and the shape checks reject it downstream.
        let Ok(row) = Element::deserialize(value.as_slice(), grove_version) else {
            continue;
        };
        if !row.is_reference() {
            continue;
        }
        if key.len() < sort_len {
            return Err(Error::CorruptedData(format!(
                "indexed-axis reference resolution: secondary key {} is shorter than the \
                 {axis:?} axis sort key ({sort_len} bytes)",
                hex::encode(key)
            )))
            .wrap_with_cost(cost);
        }
        let key_suffix = key[sort_len..].to_vec();
        // Enforces the canonical shape AND that the reference target
        // agrees with the secondary-key suffix. Both are needed: the
        // suffix is what the verifier decodes the primary key from, so a
        // row whose commitment points elsewhere would authenticate a
        // value filed under a key it does not belong to.
        let (target_key, _carried_sum) = cost_return_on_error!(
            &mut cost,
            indexed_row_target_key(&row, &key_suffix, "indexed-axis reference resolution")
                .wrap_with_cost(OperationCost::default())
        );
        let target_key = target_key.to_vec();

        let (target_element, target_value_hash) = cost_return_on_error!(
            &mut cost,
            Element::get_with_value_hash(primary_merk, &target_key, true, grove_version).map_err(
                |e| Error::CorruptedData(format!(
                    "indexed-axis reference resolution: reading primary entry {}: {e}",
                    hex::encode(&target_key)
                ))
            )
        );

        let target_bytes = cost_return_on_error!(
            &mut cost,
            target_element
                .serialize(grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "indexed-axis reference resolution: serializing primary entry {}: {e}",
                    hex::encode(&target_key)
                )))
                .wrap_with_cost(OperationCost::default())
        );

        // The reference element's OWN value hash — not the node's stored
        // (combined) hash. This is the half of the commitment the node
        // carries explicitly; the other half the verifier recomputes.
        let reference_element_hash = value_hash(value.as_slice()).unwrap_add_cost(&mut cost);

        // Which node family can express this target's commitment?
        //
        // `KVRefValueHashCountSum` reconstructs the row's hash as
        // `combine_hash(H(reference), H(target_bytes))`, which is correct
        // only when the target's own committed hash IS `H(target_bytes)` —
        // true for item-shaped entries. A tree-shaped entry (the NORMAL
        // case under a count-indexed primary, whose whole purpose is
        // indexing its children's counts) commits a combined hash instead,
        // and no choice of target bytes makes `H(bytes)` equal it. Those
        // get the target-witness variant, which carries the missing
        // child commitment so the verifier can rebuild both combines.
        let simple_target_hash = value_hash(&target_bytes).unwrap_add_cost(&mut cost);
        let target_child_hash = if simple_target_hash == target_value_hash {
            None
        } else {
            // Recover the child commitment the target folded in. It is
            // read from the target's own subtree root rather than derived
            // from the committed hash (which is one-way), and then checked
            // against that committed hash below — so a wrong witness is
            // caught here rather than becoming an unverifiable proof.
            let child_hash = cost_return_on_error!(
                &mut cost,
                target_layered_child_hash(
                    &target_element,
                    primary_path,
                    &target_key,
                    grovedb,
                    transaction,
                    batch,
                    grove_version,
                )
            );
            let rebuilt = combine_hash(&simple_target_hash, &child_hash).unwrap_add_cost(&mut cost);
            if rebuilt != target_value_hash {
                return Err(Error::NotSupported(format!(
                    "indexed-axis proofs cannot yet witness the committed value hash of primary \
                     entry {} ({}): its stored hash is a combined hash that \
                     combine_hash(H(value), child_root) does not reproduce, so no single \
                     child-hash witness expresses it. Reads, writes and integrity verification \
                     handle this entry normally; only proving it is unsupported.",
                    hex::encode(&target_key),
                    target_element.type_str()
                )))
                .wrap_with_cost(cost);
            }
            Some(child_hash)
        };

        *node = match (feature_type, target_child_hash) {
            (
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum),
                Some(child_hash),
            ) => Node::KVRefValueHashCountSumWithTargetChildHash(
                key.clone(),
                target_bytes,
                reference_element_hash,
                *count,
                *sum,
                child_hash,
            ),
            (TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum), None) => {
                Node::KVRefValueHashCountSum(
                    key.clone(),
                    target_bytes,
                    reference_element_hash,
                    *count,
                    *sum,
                )
            }
            (TreeFeatureType::ProvableCountedMerkNode(count), None) => {
                Node::KVRefValueHashCount(key.clone(), target_bytes, reference_element_hash, *count)
            }
            (TreeFeatureType::ProvableCountedMerkNode(_), Some(_)) => {
                return Err(Error::NotSupported(
                    "indexed-axis proofs do not support a layered target under a count-only \
                     secondary; every axis secondary is a dual-aggregate \
                     ProvableCountProvableSumTree, so this shape should be unreachable"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
            (other, _) => {
                return Err(Error::CorruptedData(format!(
                    "indexed-axis reference resolution: secondary row {} carries feature type \
                     {other:?}; every axis secondary is a dual-aggregate \
                     ProvableCountProvableSumTree, so its rows must be count-bearing",
                    hex::encode(key)
                )))
                .wrap_with_cost(cost);
            }
        };
    }

    Ok(()).wrap_with_cost(cost)
}

/// The child commitment a LAYERED primary entry folded into its stored
/// value hash — the second half of
/// `combine_hash(H(element bytes), child)`.
///
/// Two shapes reach here:
///
/// - **Subtree-ish elements** (`Tree`, `SumTree`, the `Provable*` family,
///   and the non-Merk trees): the child is the subtree's root hash, read
///   by opening the subtree.
/// - **Reference-shaped entries**: the child is the NEXT target's
///   committed value hash, read one hop along.
///
/// An indexed-tree element commits with `combine_hash_three` (value,
/// primary root, axes digest) and therefore has no single child witness;
/// it is rejected by the caller's rebuild check rather than here, so the
/// rejection is driven by the hash not reconstructing rather than by a
/// type list that could drift.
#[allow(clippy::too_many_arguments)]
fn target_layered_child_hash<'db>(
    target_element: &Element,
    primary_path: &[Vec<u8>],
    target_key: &[u8],
    grovedb: &'db GroveDb,
    transaction: &'db Transaction,
    batch: &'db StorageBatch,
    grove_version: &GroveVersion,
) -> CostResult<grovedb_merk::CryptoHash, Error> {
    let mut cost = OperationCost::default();

    if let Element::Reference(reference_path, ..)
    | Element::ReferenceWithSumItem(reference_path, ..) = target_element
    {
        // One hop only: the primary entry's own commitment folds in its
        // IMMEDIATE target's hash, so that is the witness — following
        // further would bind something the primary node does not.
        let mut qualified = primary_path.to_vec();
        qualified.push(target_key.to_vec());
        let next_absolute = match crate::reference_path::path_from_reference_path_type(
            reference_path.clone(),
            &qualified,
            Some(target_key),
        ) {
            Ok(p) => p,
            Err(e) => return Err(Error::from(e)).wrap_with_cost(cost),
        };
        let Some((next_key, next_parent)) = next_absolute.split_last() else {
            return Err(Error::CorruptedData(
                "indexed-axis reference resolution: a reference-shaped primary entry resolves \
                 to an empty path"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        };
        let next_parent_refs: Vec<&[u8]> = next_parent.iter().map(|s| s.as_slice()).collect();
        let next_merk = cost_return_on_error!(
            &mut cost,
            grovedb.open_transactional_merk_at_path(
                next_parent_refs.as_slice().into(),
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let next_value_hash = cost_return_on_error!(
            &mut cost,
            next_merk
                .get_value_hash(
                    next_key.as_slice(),
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(|e| Error::CorruptedData(format!(
                    "indexed-axis reference resolution: reading the next hop's value hash: {e}"
                )))
        );
        let resolved = match next_value_hash {
            Some(h) => h,
            None => {
                return Err(Error::CorruptedReferencePathKeyNotFound(format!(
                    "indexed-axis reference resolution: a reference-shaped primary entry points \
                     at {}, which does not exist",
                    hex::encode(next_key)
                )))
                .wrap_with_cost(cost);
            }
        };
        return Ok(resolved).wrap_with_cost(cost);
    }

    // Subtree-ish: the child is the subtree's own root hash.
    let mut subtree_path = primary_path.to_vec();
    subtree_path.push(target_key.to_vec());
    let subtree_refs: Vec<&[u8]> = subtree_path.iter().map(|s| s.as_slice()).collect();
    let subtree = cost_return_on_error!(
        &mut cost,
        grovedb.open_transactional_merk_at_path(
            subtree_refs.as_slice().into(),
            transaction,
            Some(batch),
            grove_version,
        )
    );
    let (root_hash, ..) = cost_return_on_error!(
        &mut cost,
        subtree
            .root_hash_key_and_aggregate_data()
            .map_err(|e| Error::CorruptedData(format!(
                "indexed-axis reference resolution: reading a layered primary entry's subtree \
                 root: {e}"
            )))
    );
    Ok(root_hash).wrap_with_cost(cost)
}
