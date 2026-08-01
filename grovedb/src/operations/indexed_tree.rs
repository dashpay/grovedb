//! Direct operations for indexed-tree elements (`ProvableCountIndexedTree`,
//! `ProvableSumIndexedTree`, `ProvableCountProvableSumIndexedTree`).
//!
//! Direct mutations against an indexed-tree primary must go through the
//! dedicated APIs here (`insert_into_indexed_tree`,
//! `delete_from_indexed_tree`) rather than the generic `insert`/`delete`,
//! which reject an indexed primary as a target.
//!
//! Those APIs are **thin wrappers over the batch path**: they build a
//! one-op batch and hand it to `apply_batch`, so the two-Merk machinery
//! (primary + 1..=3 axis-specific secondaries) lives in exactly one place,
//! [`crate::batch::indexed_tree`]. The consolidation is what makes the
//! dedicated and batch entry points agree by construction instead of by
//! two implementations being kept in sync; the earlier split was where the
//! divergences came from.
//!
//! What remains here is the part the batch mirror cannot own: opening the
//! per-axis secondary Merks for a batch to write into
//! (`open_indexed_secondaries_for_batch`), the axis-derived key and
//! sort-key encodings the mirror calls, the child-shape validation shared
//! by both entry points, and the storage sweep for a dedicated indexed
//! child being replaced.
//!
//! Deep ops *under* a sub-tree of an indexed primary need none of this —
//! they propagate through the ordinary
//! `propagate_changes_with_transaction_with_initial_deferred` machinery.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::indexed::{
    decode_avg_sort_key, decode_sum_sort_key, encode_avg_sort_key, encode_count_sort_key,
    encode_sum_sort_key, IndexAxis,
};
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, decode::ElementDecodeExtensions,
        delete::ElementDeleteFromStorageExtensions, get::ElementFetchFromStorageExtensions,
        insert::ElementInsertToStorageExtensions, reconstruct::ElementReconstructExtensions,
        tree_type::ElementTreeTypeExtensions,
    },
    merk::KVIterator,
    proofs::Query,
    Merk, TreeType,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbTransactionContext, RocksDbStorage},
    Storage, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;

use crate::{util::TxRef, Element, Error, GroveDb, Transaction, TransactionArg};

/// Per-axis Merk tree type to open the secondary with.
#[inline]
pub(crate) fn axis_secondary_tree_type(axis: IndexAxis) -> TreeType {
    match axis {
        // Each count entry contributes count = 1.
        IndexAxis::Count => TreeType::ProvableCountTree,
        // Each sum entry contributes its own SumValue.
        IndexAxis::Sum => TreeType::ProvableSumTree,
        // Each avg entry contributes (count = 1, sum = item's SumValue).
        IndexAxis::Avg => TreeType::ProvableCountProvableSumTree,
    }
}

/// Build the secondary key bytes for an entry at `item_key` under the
/// given axis, given the relevant aggregate values:
/// - count axis → `count_be(8) ‖ item_key`
/// - sum axis   → `sum_sortable_be(8) ‖ item_key`
/// - avg axis   → `avg_sortable_be(16) ‖ item_key`
#[inline]
pub(crate) fn make_axis_secondary_key(
    axis: IndexAxis,
    count: u64,
    sum: i64,
    item_key: &[u8],
) -> Vec<u8> {
    match axis {
        IndexAxis::Count => {
            let prefix = encode_count_sort_key(count);
            let mut k = Vec::with_capacity(prefix.len() + item_key.len());
            k.extend_from_slice(&prefix);
            k.extend_from_slice(item_key);
            k
        }
        IndexAxis::Sum => {
            let prefix = encode_sum_sort_key(sum);
            let mut k = Vec::with_capacity(prefix.len() + item_key.len());
            k.extend_from_slice(&prefix);
            k.extend_from_slice(item_key);
            k
        }
        IndexAxis::Avg => {
            let avg_fp = grovedb_element::indexed::compute_avg_fixed_point(sum, count);
            let prefix = encode_avg_sort_key(avg_fp);
            let mut k = Vec::with_capacity(prefix.len() + item_key.len());
            k.extend_from_slice(&prefix);
            k.extend_from_slice(item_key);
            k
        }
    }
}

/// Width in bytes of an axis's sort-key prefix inside a secondary key
/// (`sort_key ‖ item_key`).
#[inline]
pub(crate) fn axis_sort_key_len(axis: IndexAxis) -> usize {
    match axis {
        IndexAxis::Count | IndexAxis::Sum => 8,
        IndexAxis::Avg => 16,
    }
}

/// Child-element rules specific to an indexed primary's variant.
///
/// Merk's `validate_insertable_into` answers the generic question of whether an
/// element can live in a tree of some type. These are the extra rules the
/// indexed variants impose on top, because a child that contributes nothing to
/// the indexed axis makes its secondary entry meaningless:
///
/// - **PSIT** indexes an `i64` sum, so children must be sum-bearing.
///   `BigSumTree` is excluded deliberately: its aggregate is `i128` and
///   `sum_value_or_default` has no narrowing conversion, so accepting one
///   would silently mirror zero into authenticated state.
/// - **PCPSIT** can index count, sum and avg, and avg needs both, so children
///   must contribute count AND sum.
/// - **PCIT** adds nothing beyond the generic rule — every element carries a
///   count contribution.
///
/// Applied on both the dedicated and the batch path. Keeping it in one place
/// is the point: while each door had its own rules, batches accepted children
/// the dedicated API refused.
pub(crate) fn validate_indexed_child_for_variant(
    item: &Element,
    primary_tree_type: TreeType,
) -> Result<(), Error> {
    match primary_tree_type {
        TreeType::ProvableSumIndexedTree => {
            if !item.is_sum_bearing_child() {
                return Err(Error::InvalidInput(
                    "ProvableSumIndexedTree only accepts sum-bearing children (SumItem, \
                     ItemWithSumItem, ReferenceWithSumItem, or any sum-bearing tree variant)",
                ));
            }
            if matches!(item.underlying(), Element::BigSumTree(..)) {
                return Err(Error::InvalidInput(
                    "ProvableSumIndexedTree does not accept BigSumTree children; its sum axis \
                     is i64",
                ));
            }
            Ok(())
        }
        TreeType::ProvableCountProvableSumIndexedTree => {
            if !item.is_count_and_sum_bearing_child() {
                return Err(Error::InvalidInput(
                    "ProvableCountProvableSumIndexedTree only accepts children that contribute \
                     both count and sum (ItemWithSumItem, ReferenceWithSumItem, CountSumTree, \
                     ProvableCountSumTree, ProvableCountProvableSumTree, or a nested PCPSIT)",
                ));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Reject a generic (indexed-unaware) leaf mutation whose target subtree
/// is an indexed-tree primary.
///
/// A generic `db.insert` / `db.delete` / `clear_subtree` against a primary
/// mutates a child whose element carries the secondary index's ordering
/// value, but the generic paths have no hook to mirror that change into the
/// per-axis secondary Merk(s). Letting one proceed commits an element whose
/// secondary root key / axes digest no longer matches on-disk secondary
/// state: for PCIT that silently corrupts the index (`verify_grovedb`
/// reports `__cidx_count_mismatch__`), and for PSIT / PCPSIT the legacy
/// `update_tree_item_preserve_flag` path fails with a misleading
/// `InvalidPath("can only propagate on tree items")` because
/// `reconstruct_with_root_key` has no indexed arm.
///
/// This is deliberately enforced at the generic call sites rather than
/// inside `propagate_changes_*`: that function is shared with the typed
/// non-Merk append APIs (`mmr_tree_append`, `commitment_tree_insert`,
/// `bulk_append`, `dense_tree_insert`), whose element ordering value is a
/// constant on both sides of the append, and with the dedicated
/// indexed-tree APIs, which mirror the secondary themselves. Both are
/// legitimate and must not be rejected.
pub(crate) fn reject_generic_write_into_indexed_primary(
    tree_type: TreeType,
    api_label: &str,
) -> Result<(), Error> {
    if !tree_type.is_indexed_primary() {
        return Ok(());
    }
    Err(Error::NotSupported(format!(
        "{api_label}: generic writes into an indexed-tree primary ({tree_type:?}) are not \
         supported because they cannot mirror the change into the secondary index; use the \
         dedicated indexed-tree APIs instead — insert_into_count_indexed_tree / \
         delete_from_count_indexed_tree (ProvableCountIndexedTree), \
         insert_into_provable_sum_indexed_tree / delete_from_provable_sum_indexed_tree \
         (ProvableSumIndexedTree), or insert_into_provable_count_provable_sum_indexed_tree / \
         delete_from_provable_count_provable_sum_indexed_tree \
         (ProvableCountProvableSumIndexedTree)"
    )))
}

/// Reject a non-empty tree / indexed `item` claim on the dedicated
/// indexed-tree insert paths (PCIT / PSIT / PCPSIT).
///
/// These dedicated paths short-circuit child subtree roots to
/// `NULL_HASH` (the child is created empty and populated by subsequent
/// `insert_into_*` calls). A `Some(root_key)` / non-zero-aggregate /
/// populated-secondary claim would persist a serialized element whose
/// stored roots disagree with the empty merk node it is bound to —
/// breaking the H1-A chain until a later deep write happens to repair
/// it, and (the security-relevant case) letting a caller commit a node
/// that references on-disk child data the dedicated path never
/// validated. Non-empty claims must go through generic `db.insert`,
/// which opens the child merks and validates the claimed roots.
///
/// Mirrors the inline guard the PCIT path has carried since the start;
/// extends it to the PSIT and PCPSIT indexed claims.
fn reject_non_empty_dedicated_indexed_child_claim(
    item: &Element,
    api_label: &str,
) -> Result<(), Error> {
    // A child is "non-empty" if it claims a root key OR a non-zero aggregate.
    //
    // The aggregate half matters as much as the root key: a ROOTLESS child
    // carrying a non-zero count/sum has no contents to derive that value from,
    // so the value is a bare assertion — and for an indexed tree it becomes the
    // authenticated secondary sort key and the parent's aggregate contribution.
    // Accepting it lets a caller claim any position in the index for an empty
    // subtree.
    //
    // Aggregates are DERIVED everywhere else in this codebase: `verify_grovedb`
    // reports a rootless non-zero aggregate as corruption (a non-zero recorded
    // value against an empty inner Merk's `NoAggregateData` falls through to the
    // mismatch arm of `aggregate_consistency_labels`), and the sole consumer
    // constructs only empty forms. Children enter an indexed tree empty and gain
    // their ordering value from propagation once they are populated.
    let non_empty = match item.underlying() {
        Element::Tree(Some(_), _) => true,
        Element::SumTree(root, sum, _) | Element::ProvableSumTree(root, sum, _) => {
            root.is_some() || *sum != 0
        }
        Element::BigSumTree(root, big_sum, _) => root.is_some() || *big_sum != 0,
        Element::CountTree(root, count, _) | Element::ProvableCountTree(root, count, _) => {
            root.is_some() || *count != 0
        }
        Element::CountSumTree(root, count, sum, _)
        | Element::ProvableCountSumTree(root, count, sum, _)
        | Element::ProvableCountProvableSumTree(root, count, sum, _) => {
            root.is_some() || *count != 0 || *sum != 0
        }
        Element::ProvableCountIndexedTree(p, s, c, _) => p.is_some() || s.is_some() || *c != 0,
        Element::ProvableSumIndexedTree(p, s, sum, _) => p.is_some() || s.is_some() || *sum != 0,
        Element::ProvableCountProvableSumIndexedTree(p, c, sum, axes, _) => {
            p.is_some() || *c != 0 || *sum != 0 || axes.iter().any(|(_, sk)| sk.is_some())
        }
        _ => false,
    };
    if non_empty {
        return Err(Error::NotSupported(format!(
            "{api_label} only accepts EMPTY tree/indexed child elements (all child root \
             keys = None, aggregates = 0, no populated secondaries). A rootless child with \
             a non-zero aggregate is rejected too: with no contents to derive it from, the \
             value would be a bare assertion that becomes the authenticated sort key. The \
             dedicated insert \
             short-circuits child roots to NULL_HASH; a non-empty claim would persist a \
             serialized element whose stored roots disagree with the empty merk node it is \
             bound to. Use generic db.insert for non-empty claims, or insert empty here \
             then populate via subsequent insert_into_*_tree calls"
        )));
    }
    Ok(())
}

impl GroveDb {
    /// Open the per-axis secondary Merk for any indexed-tree element
    /// (`ProvableCountIndexedTree`, `ProvableSumIndexedTree`, or
    /// `ProvableCountProvableSumIndexedTree`) at `path`. The secondary
    /// lives at `Blake3(primary_prefix ‖ axis_tag)` per the (now
    /// generalized) S2-B prefix derivation. The Merk's
    /// [`TreeType`] is selected by [`axis_secondary_tree_type`].
    ///
    /// `secondary_root_key` is read from the parent indexed-tree
    /// element's matching field.
    pub(crate) fn open_indexed_secondary_at_path<'db, 'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        axis: IndexAxis,
        secondary_root_key: Option<Vec<u8>>,
        tx: &'db Transaction,
        batch: Option<&'db StorageBatch>,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();
        let primary_prefix = RocksDbStorage::build_prefix(path).unwrap_add_cost(&mut cost);
        let secondary_prefix = RocksDbStorage::secondary_prefix_for(&primary_prefix, axis.tag())
            .unwrap_add_cost(&mut cost);
        let storage = self
            .db
            .get_transactional_storage_context_by_subtree_prefix(secondary_prefix, batch, tx)
            .unwrap_add_cost(&mut cost);
        let tree_type = axis_secondary_tree_type(axis);
        if secondary_root_key.is_some() {
            Merk::open_layered_with_root_key(
                storage,
                secondary_root_key,
                tree_type,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "cannot open indexed-tree secondary (axis {:?}) by prefix with given root \
                     key: {e}",
                    axis
                ))
            })
            .add_cost(cost)
        } else {
            Merk::open_base(
                storage,
                tree_type,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "cannot open empty indexed-tree secondary (axis {:?}) by prefix: {e}",
                    axis
                ))
            })
            .add_cost(cost)
        }
    }

    /// Helper used by the batch path: open the secondary Merk for the cidx
    /// primary at `path`. Reads the parent merk's cidx element to discover
    /// the secondary's current root_key, then opens the secondary at the
    /// derived prefix sharing the supplied storage batch and transaction.
    /// Open every configured axis secondary for an indexed primary, for the
    /// batch apply path.
    ///
    /// Reads the indexed element from the parent merk once to learn which axes
    /// are configured and each axis's current secondary root key, then opens
    /// one Merk per axis. PCIT and PSIT have exactly one axis (count / sum
    /// respectively); PCPSIT carries a canonical 1..=3 axes TLV, so a tree
    /// indexing count+sum+avg yields three secondaries here.
    ///
    /// Returns them in the element's canonical axis order so the caller's
    /// per-axis state stays aligned with the axes digest.
    pub(crate) fn open_indexed_secondaries_for_batch<'db, 'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        fresh_element: Option<&Element>,
        batch: &'db StorageBatch,
        tx: &'db Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(IndexAxis, Merk<PrefixedRocksDbTransactionContext<'db>>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();
        // A primary CREATED in this same batch has no stored element yet; the
        // caller hands its in-batch element over instead. Its secondary root
        // keys are necessarily unset (the rootless-aggregate rule refuses
        // anything else), so every axis opens as an empty Merk at its derived
        // prefix — which the delete/overwrite storage sweeps guarantee holds
        // no leftover rows.
        let element = if let Some(fresh) = fresh_element {
            fresh.clone()
        } else {
            let (parent_path, indexed_key) = match path.derive_parent() {
                Some(p) => p,
                None => {
                    return Err(Error::InvalidPath(
                        "cannot open indexed secondaries at root path".to_string(),
                    ))
                    .wrap_with_cost(cost);
                }
            };
            let parent_merk = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(parent_path, tx, Some(batch), grove_version)
            );
            cost_return_on_error!(
                &mut cost,
                Element::get(&parent_merk, indexed_key, true, grove_version)
                    .map_err(Error::MerkError)
            )
        };
        let axes: Vec<(IndexAxis, Option<Vec<u8>>)> = match element.underlying() {
            Element::ProvableCountIndexedTree(_, s, ..) => vec![(IndexAxis::Count, s.clone())],
            Element::ProvableSumIndexedTree(_, s, ..) => vec![(IndexAxis::Sum, s.clone())],
            Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _) => {
                let mut out = Vec::with_capacity(axes.len());
                for (tag, root_key) in axes {
                    let axis = cost_return_on_error_no_add!(
                        cost,
                        IndexAxis::try_from_tag(*tag).map_err(|e| Error::CorruptedData(format!(
                            "open_indexed_secondaries_for_batch: invalid axis tag: {e}"
                        )))
                    );
                    out.push((axis, root_key.clone()));
                }
                out
            }
            other => {
                return Err(Error::CorruptedData(format!(
                    "open_indexed_secondaries_for_batch: parent element is not an indexed tree, \
                     got {}",
                    other.type_str()
                )))
                .wrap_with_cost(cost);
            }
        };

        let mut merks = Vec::with_capacity(axes.len());
        for (axis, root_key) in axes {
            let merk = cost_return_on_error!(
                &mut cost,
                self.open_indexed_secondary_at_path(
                    path.clone(),
                    axis,
                    root_key,
                    tx,
                    Some(batch),
                    grove_version,
                )
            );
            merks.push((axis, merk));
        }
        Ok(merks).wrap_with_cost(cost)
    }

    /// Clear orphaned child-subtree storage (and any indexed-secondary
    /// namespaces) for an existing tree/indexed entry that is about to
    /// be overwritten or deleted at `entry_path`. No-op when `existing`
    /// is not a tree.
    ///
    /// Shared by the PCIT/PSIT/PCPSIT dedicated insert (overwrite) and
    /// delete paths. Without it, replacing or deleting a tree-typed
    /// child orphans the child's storage namespace — the entry is gone
    /// from the primary Merk but its descendants still occupy storage
    /// and resurface to `verify_grovedb`'s raw_iter pass (and to a
    /// future insert at the same key). For an indexed-tree child the
    /// per-axis secondary namespaces at `Blake3(primary_prefix ‖
    /// axis_tag)` must be cleared too.
    fn cleanup_dedicated_indexed_child_storage<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        existing: &Element,
        entry_path: SubtreePath<'b, B>,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        if !existing.is_any_tree() {
            return Ok(()).wrap_with_cost(cost);
        }
        // Recursively clear all primary subtree storage under entry_path.
        let subtrees_paths = cost_return_on_error!(
            &mut cost,
            self.find_subtrees(&entry_path, Some(transaction), grove_version)
        );
        for subtree_path in subtrees_paths {
            let p: SubtreePath<_> = subtree_path.as_slice().into();
            let mut storage = self
                .db
                .get_transactional_storage_context(p, Some(batch), transaction)
                .unwrap_add_cost(&mut cost);
            cost_return_on_error!(
                &mut cost,
                storage.clear().map_err(|e| {
                    Error::CorruptedData(format!(
                        "unable to clean up old subtree storage in dedicated indexed-tree \
                         overwrite/delete: {e}",
                    ))
                })
            );
        }
        // Clear the per-axis secondary namespaces for indexed primaries.
        let axes: Vec<IndexAxis> = match existing.underlying() {
            Element::ProvableCountIndexedTree(..) => vec![IndexAxis::Count],
            Element::ProvableSumIndexedTree(..) => vec![IndexAxis::Sum],
            Element::ProvableCountProvableSumIndexedTree(_, _, _, axes_tlv, _) => axes_tlv
                .iter()
                .filter_map(|(tag, _)| IndexAxis::try_from_tag(*tag).ok())
                .collect(),
            _ => Vec::new(),
        };
        if !axes.is_empty() {
            let primary_prefix =
                RocksDbStorage::build_prefix(entry_path.clone()).unwrap_add_cost(&mut cost);
            for axis in axes {
                let secondary_prefix =
                    RocksDbStorage::secondary_prefix_for(&primary_prefix, axis.tag())
                        .unwrap_add_cost(&mut cost);
                let mut secondary_storage = self
                    .db
                    .get_transactional_storage_context_by_subtree_prefix(
                        secondary_prefix,
                        Some(batch),
                        transaction,
                    )
                    .unwrap_add_cost(&mut cost);
                cost_return_on_error!(
                    &mut cost,
                    secondary_storage.clear().map_err(|e| {
                        Error::CorruptedData(format!(
                            "unable to clean up indexed secondary (axis {axis:?}) during \
                             dedicated indexed-tree overwrite/delete: {e}",
                        ))
                    })
                );
            }
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Validate that `path` names an indexed primary of the expected variant,
    /// then hand the write to the batch pipeline.
    ///
    /// The dedicated `insert_into_*` APIs exist for callers that want to name
    /// the variant explicitly and get an error if the target is not one. Every
    /// other guard — child-type acceptance, the rootless-aggregate rule, the
    /// per-axis key ceiling — and all of the mirroring now live in the batch
    /// path, so this is the whole difference between the two doors.
    ///
    /// Keeping one mirror implementation matters beyond tidiness: while the
    /// dedicated and batch paths each had their own, a guard added to one
    /// silently left the other open. That is how the rootless-aggregate
    /// forgery stayed reachable through batches after the dedicated path was
    /// fixed, and how batches came to accept child types the dedicated path
    /// refused.
    fn insert_into_indexed_tree_via_batch<'b, B, P>(
        &self,
        path: P,
        item_key: &[u8],
        item: Element,
        expect: TreeType,
        api_label: &str,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let mut cost = OperationCost::default();
        // Variant rule first: "this index does not accept BigSumTree children"
        // tells the caller more than "the child must be empty" when both
        // apply, and the batch path enforces the same rule.
        cost_return_on_error_no_add!(cost, validate_indexed_child_for_variant(&item, expect));
        // Contract of the dedicated APIs specifically, not of indexed trees:
        // these entry points create child subtrees empty, so a claimed child
        // root key or populated secondary would describe state they do not
        // build. A caller with genuinely non-empty child state wants the batch
        // path, which writes it properly.
        cost_return_on_error_no_add!(
            cost,
            reject_non_empty_dedicated_indexed_child_claim(&item, api_label)
        );
        let path: SubtreePath<B> = path.into();
        if path.derive_parent().is_none() {
            return Err(Error::InvalidPath(format!(
                "{api_label}: cannot insert into an indexed tree at the root path"
            )))
            .wrap_with_cost(cost);
        }

        let tx = TxRef::new(&self.db, transaction);
        let probe_batch = StorageBatch::new();
        let primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                tx.as_ref(),
                Some(&probe_batch),
                grove_version,
            )
        );
        if primary_merk.tree_type != expect {
            return Err(Error::InvalidPath(format!(
                "{api_label}: the path's last segment must be a {expect:?} element, found {:?}",
                primary_merk.tree_type
            )))
            .wrap_with_cost(cost);
        }
        let existing_child = cost_return_on_error!(
            &mut cost,
            Element::get_optional_from_storage(&primary_merk.storage, item_key, grove_version)
                .map_err(Error::MerkError)
        );
        drop(primary_merk);
        drop(probe_batch);

        // Overwriting a TREE-typed child replaces the element but leaves the
        // subtree it owned at its derived prefix. Prefixes are path-derived, so
        // the replacement occupies the same namespace and inherits those rows:
        // `db.query` returns them via raw iteration and `verify_grovedb`
        // rejects them as data the Merk cannot attest to.
        // `GroveOp::InsertOrReplace` does not sweep them, and the batch path's
        // overwrite hook classifies only INDEXED existing children, so the
        // sweep runs here against the same transaction.
        if let Some(existing) = existing_child.as_ref()
            && existing.tree_type().is_some()
        {
            let cleanup_batch = StorageBatch::new();
            let entry_path_owned = path.derive_owned_with_child(item_key);
            cost_return_on_error!(
                &mut cost,
                self.cleanup_dedicated_indexed_child_storage(
                    existing,
                    SubtreePath::from(&entry_path_owned),
                    tx.as_ref(),
                    &cleanup_batch,
                    grove_version,
                )
            );
            cost_return_on_error!(
                &mut cost,
                self.db
                    .commit_multi_context_batch(cleanup_batch, Some(tx.as_ref()))
                    .map_err(Into::into)
            );
        }

        let op = crate::batch::QualifiedGroveDbOp::insert_or_replace_op(
            path.to_vec(),
            item_key.to_vec(),
            item,
        );
        cost_return_on_error!(
            &mut cost,
            self.apply_batch(vec![op], None, Some(tx.as_ref()), grove_version)
        );
        tx.commit_local().wrap_with_cost(cost)
    }

    /// Validate that `path` names an indexed primary of the expected variant,
    /// then hand the delete to the batch pipeline.
    ///
    /// Returns whether anything was removed, which the batch op itself does
    /// not report — so the entry is probed first, and an absent key is a
    /// no-op returning `false` rather than an empty batch.
    ///
    /// The mirroring, including removing the entry from every configured axis,
    /// belongs to the batch path; this is only the variant check and the
    /// existence probe.
    fn delete_from_indexed_tree_via_batch<'b, B, P>(
        &self,
        path: P,
        item_key: &[u8],
        expect: TreeType,
        api_label: &str,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        if path.derive_parent().is_none() {
            return Err(Error::InvalidPath(format!(
                "{api_label}: cannot delete from an indexed tree at the root path"
            )))
            .wrap_with_cost(cost);
        }

        let tx = TxRef::new(&self.db, transaction);
        let probe_batch = StorageBatch::new();
        let primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                tx.as_ref(),
                Some(&probe_batch),
                grove_version,
            )
        );
        if primary_merk.tree_type != expect {
            return Err(Error::InvalidPath(format!(
                "{api_label}: the path's last segment must be a {expect:?} element, found {:?}",
                primary_merk.tree_type
            )))
            .wrap_with_cost(cost);
        }
        let existing = cost_return_on_error!(
            &mut cost,
            Element::get_optional_from_storage(&primary_merk.storage, item_key, grove_version)
                .map_err(Error::MerkError)
        );
        drop(primary_merk);
        drop(probe_batch);
        if existing.is_none() {
            return Ok(false).wrap_with_cost(cost);
        }

        // A tree-typed child owns a whole storage namespace at its derived
        // prefix, and `GroveOp::Delete` only unlinks the entry — the batch
        // path's recursive storage cleanup and per-axis secondary sweep are
        // driven by `DeleteTree`. Emitting a plain delete here orphaned the
        // child's subtree: re-creating at the same key (prefixes are
        // path-derived, so it is the same namespace) resurrected the old rows,
        // which `db.query` returns via raw iteration and `verify_grovedb`
        // rejects as data the Merk cannot attest to.
        let op = match existing
            .as_ref()
            .expect("existence checked above")
            .tree_type()
        {
            Some(child_tree_type) => crate::batch::QualifiedGroveDbOp::delete_tree_op(
                path.to_vec(),
                item_key.to_vec(),
                child_tree_type,
                crate::batch::SubelementsDeletionBehavior::DeleteChildren,
            ),
            None => crate::batch::QualifiedGroveDbOp::delete_op(path.to_vec(), item_key.to_vec()),
        };
        cost_return_on_error!(
            &mut cost,
            self.apply_batch(vec![op], None, Some(tx.as_ref()), grove_version)
        );
        cost_return_on_error_no_add!(cost, tx.commit_local());
        Ok(true).wrap_with_cost(cost)
    }

    /// Insert (or update) an item under a key into a `CountIndexedTree`
    /// element. Mirrors the change in the count-ordered secondary index and
    /// updates the parent's element bytes (primary_root_key,
    /// secondary_root_key, count_value) using the H1-A three-input value
    /// hash. Propagates resulting parent changes up the regular Merk
    /// hierarchy.
    ///
    /// `path` is the path **to the CountIndexedTree element** — i.e. the
    /// path of its primary Merk. `item_key` is the key under which to
    /// insert in the primary.
    ///
    /// The path's last segment must point to a `CountIndexedTree` /
    /// `ProvableCountIndexedTree` element; otherwise an error is returned.
    pub fn insert_into_count_indexed_tree<'b, B, P>(
        &self,
        path: P,
        item_key: &[u8],
        item: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.insert_into_indexed_tree_via_batch(
            path,
            item_key,
            item,
            TreeType::ProvableCountIndexedTree,
            "insert_into_count_indexed_tree",
            transaction,
            grove_version,
        )
    }

    /// Rebuild the count-ordered secondary index for a `CountIndexedTree`
    /// element from scratch by walking its primary Merk.
    ///
    /// **When you need this: normally, never.**
    ///
    /// This is a repair API. No supported write can desync the secondary
    /// any more, so nothing in ordinary operation requires calling it:
    ///
    /// - The dedicated `insert_into_*` / `delete_from_*` APIs maintain
    ///   the secondary inline.
    /// - The batch path mirrors it per axis, and a generic write that
    ///   targets an indexed primary directly is rejected outright rather
    ///   than silently skipping the mirror.
    /// - A deep write *under* a child of the primary — inserting into a
    ///   sub-`CountTree` held in the primary, so the sub-tree's aggregate
    ///   propagates up into the primary's element bytes — keeps the index
    ///   in sync through both paths. That case used to be exactly what
    ///   this method was for; it is pinned now by
    ///   `a_deep_write_under_an_indexed_child_keeps_the_secondary_in_sync`.
    ///
    /// What remains is repair of a secondary already damaged by something
    /// outside those paths: storage-level corruption, a partially applied
    /// migration, or a bug. `verify_grovedb` reporting
    /// `__cidx_count_mismatch__` for a primary is the signal.
    ///
    /// The rebuild is canonical rather than incremental, so the repaired
    /// secondary depends only on the primary's contents — two differently
    /// damaged indexes over the same primary reconcile to the same bytes.
    ///
    /// Calling this method when the secondary is already correct is
    /// idempotent — it will rewrite the secondary to a state that
    /// matches the primary, producing identical bytes.
    ///
    /// **Cost:** `O(n)` where `n` is the number of entries in the
    /// primary. Intended for occasional use (post-batch reconciliation,
    /// migration, repair). For maintaining the secondary in real time
    /// during high-frequency updates, use the dedicated insert/delete
    /// APIs.
    pub fn reconcile_count_indexed_tree_secondary<'b, B, P>(
        &self,
        path: P,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        cost_return_on_error!(
            &mut cost,
            self.reconcile_count_indexed_tree_secondary_on_transaction(
                path,
                tx_ref,
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx_ref))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    fn reconcile_count_indexed_tree_secondary_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let (parent_path, count_indexed_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot reconcile a count-indexed tree at the root path".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        // 1. Open primary and read all (key, count_value) pairs.
        let primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        if !primary_merk.tree_type.is_count_indexed_primary() {
            return Err(Error::InvalidPath(
                "reconcile_count_indexed_tree_secondary requires the path's last segment to be \
                 a CountIndexedTree or ProvableCountIndexedTree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }

        let mut all_query = Query::new();
        all_query.insert_all();
        let mut iter =
            KVIterator::new(primary_merk.storage.raw_iter(), &all_query).unwrap_add_cost(&mut cost);
        let mut entries: Vec<(u64, Vec<u8>)> = Vec::new();
        while let Some((key, value_bytes)) = iter.next_kv().unwrap_add_cost(&mut cost) {
            // Reject oversized primary keys before they can drive
            // make_secondary_key to synthesize a secondary key that
            // violates Merk's < 256-byte invariant. The cidx write
            // paths now enforce this (commit 978dc2d9), but reconcile
            // operates over EXISTING storage which may contain legacy
            // or externally-injected oversize keys; fail closed
            // rather than corrupting the secondary.
            if key.len() > MAX_CIDX_ITEM_KEY_LEN {
                return Err(Error::CorruptedData(format!(
                    "reconcile_count_indexed_tree_secondary found a primary key of length \
                     {} bytes which exceeds the cidx ceiling of {} bytes; refusing to \
                     synthesize an oversize secondary key. The cidx primary at this path \
                     was written by a code path that bypassed the cidx-key length check \
                     and is corrupt — investigate the source before re-running reconcile",
                    key.len(),
                    MAX_CIDX_ITEM_KEY_LEN
                )))
                .wrap_with_cost(cost);
            }
            let element = cost_return_on_error_no_add!(
                cost,
                Element::raw_decode(&value_bytes, grove_version).map_err(|e| {
                    Error::CorruptedData(format!(
                        "failed to decode element while reconciling secondary: {e}"
                    ))
                })
            );
            entries.push((element.count_value_or_default(), key));
        }

        // 2. Open parent merk to get current secondary_root_key, then open
        //    the secondary.
        let mut parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                parent_path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        let count_indexed_element = cost_return_on_error!(
            &mut cost,
            Element::get(&parent_merk, count_indexed_key, true, grove_version)
                .map_err(Error::MerkError)
        );
        let secondary_root_key_before = match count_indexed_element.underlying() {
            Element::ProvableCountIndexedTree(_, secondary, ..) => secondary.clone(),
            _ => {
                return Err(Error::CorruptedData(
                    "parent element at count-indexed key is not a ProvableCountIndexedTree"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        let mut secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_indexed_secondary_at_path(
                path.clone(),
                IndexAxis::Count,
                secondary_root_key_before,
                transaction,
                Some(batch),
                grove_version,
            )
        );

        // 3. Collect all current secondary keys.
        let mut all_query_sec = Query::new();
        all_query_sec.insert_all();
        let mut sec_iter = KVIterator::new(secondary_merk.storage.raw_iter(), &all_query_sec)
            .unwrap_add_cost(&mut cost);
        let mut existing_secondary_keys: Vec<Vec<u8>> = Vec::new();
        while let Some((key, _value)) = sec_iter.next_kv().unwrap_add_cost(&mut cost) {
            existing_secondary_keys.push(key);
        }

        // 4. Compute the desired secondary keys from the primary entries.
        //    `BTreeSet`, not `HashSet`: step 6 iterates this set to drive the
        //    repair inserts, and a hashed iteration order would build the
        //    secondary AVL in a different shape on every run — so two
        //    operators repairing identical databases would derive different
        //    secondary root hashes, and therefore different
        //    `combine_hash_three` parent bindings, contradicting this API's
        //    "producing identical bytes" contract.
        let desired_keys: std::collections::BTreeSet<Vec<u8>> = entries
            .iter()
            .map(|(count, key)| make_secondary_key(*count, key))
            .collect();

        // 5. Delete entries that should not be present.
        let existing_set: std::collections::BTreeSet<Vec<u8>> =
            existing_secondary_keys.iter().cloned().collect();
        for key in existing_secondary_keys {
            if !desired_keys.contains(&key) {
                cost_return_on_error!(
                    &mut cost,
                    Element::delete(
                        &mut secondary_merk,
                        key.as_slice(),
                        None,
                        false,
                        TreeType::ProvableCountTree,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
                );
            }
        }

        // 6. Insert entries that are missing.
        for desired_key in &desired_keys {
            if !existing_set.contains(desired_key) {
                let entry = Element::new_item(Vec::new());
                cost_return_on_error!(
                    &mut cost,
                    entry
                        .insert(
                            &mut secondary_merk,
                            desired_key.as_slice(),
                            None,
                            grove_version,
                        )
                        .map_err(Error::MerkError)
                );
            }
        }

        // 7. Snapshot updated states and rebuild the parent's element.
        let (primary_root_hash, primary_root_key, primary_aggregate_data) = cost_return_on_error!(
            &mut cost,
            primary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(Error::MerkError)
        );
        let (secondary_root_hash, secondary_root_key, _) = cost_return_on_error!(
            &mut cost,
            secondary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(Error::MerkError)
        );

        let reconstructed = cost_return_on_error_no_add!(
            cost,
            count_indexed_element
                .reconstruct_with_two_root_keys(
                    primary_root_key,
                    secondary_root_key,
                    primary_aggregate_data,
                )
                .ok_or(Error::CorruptedCodeExecution(
                    "reconstruct_with_two_root_keys returned None for a CountIndexedTree element"
                ))
        );
        cost_return_on_error!(
            &mut cost,
            reconstructed
                .insert_count_indexed_subtree(
                    &mut parent_merk,
                    count_indexed_key,
                    primary_root_hash,
                    secondary_root_hash,
                    None,
                    grove_version,
                )
                .map_err(Error::MerkError)
        );

        // 8. Hand off to shared propagation (CountIndexedTree-aware).
        let mut merk_cache: std::collections::HashMap<
            SubtreePath<B>,
            Merk<PrefixedRocksDbTransactionContext>,
        > = std::collections::HashMap::default();
        merk_cache.insert(parent_path.clone(), parent_merk);
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction(
                merk_cache,
                parent_path,
                transaction,
                batch,
                grove_version,
            )
        );

        Ok(()).wrap_with_cost(cost)
    }

    // -----------------------------------------------------------------
    // Direct (non-proof) per-axis query APIs.
    //
    // The three families below — `indexed_count_*`, `indexed_sum_*`,
    // `indexed_avg_*` — operate uniformly across any indexed-tree
    // variant that supports the chosen axis. Each function:
    //
    //   1. Reads the parent indexed-tree element and validates that the
    //      requested axis is indexed at this path (PCIT → Count only;
    //      PSIT → Sum only; PCPSIT → whichever axes are in its TLV).
    //   2. Opens the per-axis secondary at the derived prefix.
    //   3. Iterates the secondary in axis-sort order (its bytes are
    //      already lex-equivalent to the typed axis value).
    //   4. Decodes each secondary key into the typed `(axis_value,
    //      original_key)` pair.
    //
    // For verifiable variants see the `prove_*_indexed_*` /
    // `verify_*_indexed_*` families in the proof submodule.
    // -----------------------------------------------------------------

    // -----------------------------------------------------------------
    // Generic per-axis query cores.
    //
    // The three `indexed_axis_*_generic` methods below carry the ONE
    // implementation of each direct-query shape (top_k, top_k_paginated,
    // range). The per-axis public wrappers (`indexed_count_*`,
    // `indexed_sum_*`, `indexed_avg_*`) differ only in the [`IndexAxis`]
    // passed, the typed value `T` each secondary key decodes to, and —
    // for range — the bound-encoding of `T`. Each core takes:
    //   - `axis`: which per-axis secondary to open + validate.
    //   - `decode`: split a secondary key into `(T, original_key)`,
    //     returning `None` on a malformed (too-short) key so the core
    //     can surface `corrupted_secondary_key_error(axis, ..)`
    //     identically to the former per-axis clones.
    // The range core additionally takes the resolved byte bounds.
    //
    // Mirrors the generic-core-plus-thin-wrapper shape the proof side
    // already uses in `operations/proof/indexed_axis.rs`.
    // -----------------------------------------------------------------

    /// One implementation of the `indexed_<axis>_top_k` shape. See the
    /// per-axis wrappers for the public contract.
    fn indexed_axis_top_k_generic<'b, B, T>(
        &self,
        path: SubtreePath<'b, B>,
        axis: IndexAxis,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
        decode: impl Fn(&[u8]) -> Option<(T, Vec<u8>)>,
    ) -> CostResult<Vec<(T, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_validated_axis_secondary(path, axis, tx_ref, grove_version)
        );

        let mut all_query = Query::new();
        all_query.left_to_right = !descending;
        all_query.insert_all();

        let mut iter = KVIterator::new(secondary_merk.storage.raw_iter(), &all_query)
            .unwrap_add_cost(&mut cost);

        let mut results = Vec::with_capacity(k as usize);
        while results.len() < k as usize {
            match iter.next_kv().unwrap_add_cost(&mut cost) {
                Some((secondary_key, _)) => match decode(&secondary_key) {
                    Some(decoded) => results.push(decoded),
                    None => {
                        return Err(corrupted_secondary_key_error(axis, &secondary_key))
                            .wrap_with_cost(cost);
                    }
                },
                None => break,
            }
        }

        Ok(results).wrap_with_cost(cost)
    }

    /// One implementation of the `indexed_<axis>_top_k_paginated` shape.
    fn indexed_axis_top_k_paginated_generic<'b, B, T>(
        &self,
        path: SubtreePath<'b, B>,
        axis: IndexAxis,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
        decode: impl Fn(&[u8]) -> Option<(T, Vec<u8>)>,
    ) -> CostResult<Vec<(T, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_validated_axis_secondary(path, axis, tx_ref, grove_version)
        );

        let mut all_query = Query::new();
        all_query.left_to_right = !descending;
        all_query.insert_all();

        let mut iter = KVIterator::new(secondary_merk.storage.raw_iter(), &all_query)
            .unwrap_add_cost(&mut cost);

        // Skip `offset` entries; surface corruption defensively.
        let mut skipped: u64 = 0;
        while skipped < offset {
            match iter.next_kv().unwrap_add_cost(&mut cost) {
                Some((secondary_key, _)) => {
                    if decode(&secondary_key).is_none() {
                        return Err(corrupted_secondary_key_error(axis, &secondary_key))
                            .wrap_with_cost(cost);
                    }
                    skipped += 1;
                }
                None => return Ok(Vec::new()).wrap_with_cost(cost),
            }
        }

        let mut results = Vec::with_capacity(k as usize);
        while results.len() < k as usize {
            match iter.next_kv().unwrap_add_cost(&mut cost) {
                Some((secondary_key, _)) => match decode(&secondary_key) {
                    Some(decoded) => results.push(decoded),
                    None => {
                        return Err(corrupted_secondary_key_error(axis, &secondary_key))
                            .wrap_with_cost(cost);
                    }
                },
                None => break,
            }
        }

        Ok(results).wrap_with_cost(cost)
    }

    /// One implementation of the `indexed_<axis>_range` shape. The
    /// per-axis wrapper resolves `lo`/`hi` into the secondary keyspace
    /// bounds `lo_bytes` (inclusive lower) and `upper_bytes` (exclusive
    /// upper, `None` for an unbounded `RangeFrom` when `hi` is the axis
    /// maximum) and passes them here. `decode` returns the typed value
    /// per matched key.
    #[allow(clippy::too_many_arguments)]
    fn indexed_axis_range_generic<'b, B, T>(
        &self,
        path: SubtreePath<'b, B>,
        axis: IndexAxis,
        lo_bytes: Vec<u8>,
        upper_bytes: Option<Vec<u8>>,
        descending: bool,
        limit: u16,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
        decode: impl Fn(&[u8]) -> Option<(T, Vec<u8>)>,
    ) -> CostResult<Vec<(T, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_validated_axis_secondary(path, axis, tx_ref, grove_version)
        );

        let mut q = Query::new();
        q.left_to_right = !descending;
        match upper_bytes {
            Some(upper) => q.insert_range(lo_bytes..upper),
            None => q.insert_range_from(lo_bytes..),
        }

        let mut iter =
            KVIterator::new(secondary_merk.storage.raw_iter(), &q).unwrap_add_cost(&mut cost);

        let mut results = Vec::new();
        while results.len() < limit as usize {
            match iter.next_kv().unwrap_add_cost(&mut cost) {
                Some((secondary_key, _)) => {
                    let Some(decoded) = decode(&secondary_key) else {
                        return Err(corrupted_secondary_key_error(axis, &secondary_key))
                            .wrap_with_cost(cost);
                    };
                    results.push(decoded);
                }
                None => break,
            }
        }

        Ok(results).wrap_with_cost(cost)
    }

    // ---- count axis ----

    /// Iterate the count-axis secondary in count-order and return the
    /// **top `k`** entries by `count_value`. When `descending` is `true`
    /// (the typical "highest first" use case) entries are walked
    /// right-to-left through the secondary's keyspace; ties on `count`
    /// are broken in descending lex order of the original key.
    ///
    /// Accepts [`Element::ProvableCountIndexedTree`] or
    /// [`Element::ProvableCountProvableSumIndexedTree`] (only if its TLV
    /// contains the count axis). Any other variant — or a PCPSIT
    /// without the count axis — is rejected with `Error::InvalidPath`.
    ///
    /// Each returned entry is `(count, original_key)`. Resolving the
    /// primary value is the caller's responsibility (use
    /// `db.get(path, original_key, ...)`).
    ///
    /// For a verifiable variant, see [`Self::prove_indexed_count_top_k`]
    /// and [`Self::verify_indexed_count_top_k`].
    pub fn indexed_count_top_k<'b, B, P>(
        &self,
        path: P,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(u64, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.indexed_axis_top_k_generic(
            path.into(),
            IndexAxis::Count,
            k,
            descending,
            transaction,
            grove_version,
            decode_secondary_key,
        )
    }

    /// Paginated form of [`Self::indexed_count_top_k`]. Skips `offset`
    /// entries in the directional scan before collecting up to `k`
    /// results.
    ///
    /// `offset = 0` is equivalent to plain `indexed_count_top_k`. The
    /// skip is performed at the secondary's storage iterator level —
    /// this is not a verifiable / proof-bounded skip; for the provable
    /// variant use
    /// [`Self::prove_indexed_count_top_k_paginated`] which relies on the
    /// merk-level count-offset proof to commit the skipped count via
    /// `HashWithCount`.
    pub fn indexed_count_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(u64, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.indexed_axis_top_k_paginated_generic(
            path.into(),
            IndexAxis::Count,
            k,
            offset,
            descending,
            transaction,
            grove_version,
            decode_secondary_key,
        )
    }

    /// Iterate the count-axis secondary over a count range `[lo_count,
    /// hi_count_inclusive]` and return matching `(count, original_key)`
    /// entries up to `limit`. Direction is controlled by `descending`.
    ///
    /// Bounds are inclusive on both sides; `(0, u64::MAX, false, limit)`
    /// is equivalent to a full scan. `lo_count > hi_count` returns an
    /// empty vector.
    pub fn indexed_count_range<'b, B, P>(
        &self,
        path: P,
        lo_count: u64,
        hi_count: u64,
        descending: bool,
        limit: u16,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(u64, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let cost = OperationCost::default();
        if lo_count > hi_count {
            return Ok(Vec::new()).wrap_with_cost(cost);
        }

        // Seek directly to the encoded count bounds in the secondary's
        // keyspace instead of doing a full scan with post-filtering. The
        // secondary keys are `count_be_bytes ‖ original_key`; we build a
        // range query that brackets all encodings whose count falls in
        // `[lo_count, hi_count]`. Exclusive on the next-count upper
        // boundary is equivalent to inclusive on `hi_count` for any
        // `original_key` suffix.
        let lo_bytes = lo_count.to_be_bytes().to_vec();
        let upper_bytes = if hi_count == u64::MAX {
            None
        } else {
            Some((hi_count + 1).to_be_bytes().to_vec())
        };

        self.indexed_axis_range_generic(
            path.into(),
            IndexAxis::Count,
            lo_bytes,
            upper_bytes,
            descending,
            limit,
            transaction,
            grove_version,
            |sk| {
                decode_secondary_key(sk).inspect(|(count, _)| {
                    debug_assert!(*count >= lo_count && *count <= hi_count);
                })
            },
        )
        .add_cost(cost)
    }

    /// Count the number of indexed entries whose `count_value` falls in
    /// `[lo_count, hi_count]`, without returning the entries themselves.
    ///
    /// Wraps `Merk::count_aggregate_on_range` against the count-axis
    /// secondary (which carries an aggregate count at every node — see
    /// [`axis_secondary_tree_type`]); the merk walks the secondary in
    /// O(log n + boundary) using each internal node's stored count to
    /// short-circuit fully-inside / fully-outside subtrees.
    ///
    /// Use this when the caller only needs the *count* of matching
    /// entries (e.g. "how many users have a score in `[100, 500]`?")
    /// rather than the list. For the listing form use
    /// [`Self::indexed_count_range`].
    ///
    /// `lo_count > hi_count` returns `Ok(0)`. `lo_count == 0 && hi_count
    /// == u64::MAX` is equivalent to "how many entries does this
    /// indexed-tree have?". This call has no cryptographic guarantee —
    /// the returned count is whatever the merk reports. For a
    /// verifiable count, use
    /// [`Self::prove_indexed_count_range_aggregate`] +
    /// [`Self::verify_indexed_count_range_aggregate`].
    pub fn indexed_count_range_aggregate<'b, B, P>(
        &self,
        path: P,
        lo_count: u64,
        hi_count: u64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<u64, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        use grovedb_merk::proofs::query::QueryItem as MerkQueryItemForRange;

        let mut cost = OperationCost::default();
        if lo_count > hi_count {
            return Ok(0u64).wrap_with_cost(cost);
        }
        let path: SubtreePath<B> = path.into();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_validated_axis_secondary(path, IndexAxis::Count, tx_ref, grove_version,)
        );

        let lo_bytes = lo_count.to_be_bytes().to_vec();
        let inner_range = if hi_count == u64::MAX {
            MerkQueryItemForRange::RangeFrom(lo_bytes..)
        } else {
            let upper_bytes = (hi_count + 1).to_be_bytes().to_vec();
            MerkQueryItemForRange::Range(lo_bytes..upper_bytes)
        };

        let count = cost_return_on_error!(
            &mut cost,
            secondary_merk
                .count_aggregate_on_range(&inner_range, grove_version)
                .map_err(|e| Error::CorruptedData(format!(
                    "indexed count aggregate on range: {e}"
                )))
        );

        Ok(count).wrap_with_cost(cost)
    }

    // ---- sum axis ----

    /// Iterate the sum-axis secondary in sum-order and return the
    /// **top `k`** entries by `sum_value`. `descending = true` returns
    /// the largest sums first; ties on sum are broken in descending lex
    /// order of the original key.
    ///
    /// Accepts [`Element::ProvableSumIndexedTree`] or
    /// [`Element::ProvableCountProvableSumIndexedTree`] (only if its TLV
    /// contains the sum axis). Any other variant — or a PCPSIT without
    /// the sum axis — is rejected with `Error::InvalidPath`.
    ///
    /// Each returned entry is `(sum, original_key)`. The signed `i64`
    /// sum is decoded from the secondary's sign-flipped big-endian
    /// prefix (see [`grovedb_element::indexed::encode_sum_sort_key`]).
    pub fn indexed_sum_top_k<'b, B, P>(
        &self,
        path: P,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(i64, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.indexed_axis_top_k_generic(
            path.into(),
            IndexAxis::Sum,
            k,
            descending,
            transaction,
            grove_version,
            decode_sum_secondary_key,
        )
    }

    /// Paginated form of [`Self::indexed_sum_top_k`]. Skips `offset`
    /// entries in the directional scan before collecting up to `k`
    /// results. `offset = 0` is equivalent to plain
    /// `indexed_sum_top_k`; the skip is iterator-level only (not
    /// proof-bounded).
    pub fn indexed_sum_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(i64, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.indexed_axis_top_k_paginated_generic(
            path.into(),
            IndexAxis::Sum,
            k,
            offset,
            descending,
            transaction,
            grove_version,
            decode_sum_secondary_key,
        )
    }

    /// Iterate the sum-axis secondary over a sum range `[lo_sum,
    /// hi_sum_inclusive]` and return matching `(sum, original_key)`
    /// entries up to `limit`. Direction is controlled by `descending`.
    ///
    /// Bounds are inclusive on both sides. `lo_sum > hi_sum` returns
    /// an empty vector. `lo_sum == i64::MIN && hi_sum == i64::MAX` is
    /// equivalent to a full scan.
    pub fn indexed_sum_range<'b, B, P>(
        &self,
        path: P,
        lo_sum: i64,
        hi_sum: i64,
        descending: bool,
        limit: u16,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(i64, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let cost = OperationCost::default();
        if lo_sum > hi_sum {
            return Ok(Vec::new()).wrap_with_cost(cost);
        }

        // Encoded sum bounds: secondary keys are
        // `encode_sum_sort_key(sum) ‖ original_key`. The encoding is
        // lex-equivalent to signed numeric order, so an inclusive
        // numeric range `[lo, hi]` maps to a byte range
        // `[encode(lo), encode(hi+1))` — exclusive on the upper. When
        // `hi == i64::MAX` no representable next-sum exists, so we use
        // an unbounded `RangeFrom`.
        let lo_bytes = encode_sum_sort_key(lo_sum).to_vec();
        let upper_bytes = if hi_sum == i64::MAX {
            None
        } else {
            Some(encode_sum_sort_key(hi_sum + 1).to_vec())
        };

        self.indexed_axis_range_generic(
            path.into(),
            IndexAxis::Sum,
            lo_bytes,
            upper_bytes,
            descending,
            limit,
            transaction,
            grove_version,
            |sk| {
                decode_sum_secondary_key(sk).inspect(|(sum, _)| {
                    debug_assert!(*sum >= lo_sum && *sum <= hi_sum);
                })
            },
        )
        .add_cost(cost)
    }

    /// Sum the `sum_value`s of indexed entries whose sum falls in
    /// `[lo_sum, hi_sum]`, without returning the entries themselves.
    ///
    /// Wraps `Merk::sum_aggregate_on_range` against the sum-axis
    /// secondary; the merk walks in O(log n + boundary) using each
    /// internal node's stored aggregate sum to short-circuit subtrees
    /// fully inside or fully outside the range.
    ///
    /// `lo_sum > hi_sum` returns `Ok(0)`. `lo_sum == i64::MIN && hi_sum
    /// == i64::MAX` is equivalent to "the total sum of this
    /// indexed-tree". Like the count counterpart, this call has no
    /// cryptographic guarantee; for a verifiable sum use the
    /// proof-bound variant in the proof submodule.
    pub fn indexed_sum_range_aggregate<'b, B, P>(
        &self,
        path: P,
        lo_sum: i64,
        hi_sum: i64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<i64, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        use grovedb_merk::proofs::query::QueryItem as MerkQueryItemForRange;

        let mut cost = OperationCost::default();
        if lo_sum > hi_sum {
            return Ok(0i64).wrap_with_cost(cost);
        }
        let path: SubtreePath<B> = path.into();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_validated_axis_secondary(path, IndexAxis::Sum, tx_ref, grove_version)
        );

        let lo_bytes = encode_sum_sort_key(lo_sum).to_vec();
        let inner_range = if hi_sum == i64::MAX {
            MerkQueryItemForRange::RangeFrom(lo_bytes..)
        } else {
            let upper_bytes = encode_sum_sort_key(hi_sum + 1).to_vec();
            MerkQueryItemForRange::Range(lo_bytes..upper_bytes)
        };

        let sum = cost_return_on_error!(
            &mut cost,
            secondary_merk
                .sum_aggregate_on_range(&inner_range, grove_version)
                .map_err(|e| Error::CorruptedData(format!("indexed sum aggregate on range: {e}")))
        );

        Ok(sum).wrap_with_cost(cost)
    }

    // ---- avg axis (PCPSIT-only) ----

    /// Iterate the avg-axis secondary in avg-order and return the
    /// **top `k`** entries by fixed-point average `floor(sum * SCALE /
    /// count)` (`SCALE = 10^19`). `descending = true` returns the
    /// largest averages first; ties on avg are broken in descending
    /// lex order of the original key.
    ///
    /// Only accepts [`Element::ProvableCountProvableSumIndexedTree`]
    /// with the avg axis present in its TLV; any other variant — or a
    /// PCPSIT without the avg axis — is rejected with
    /// `Error::InvalidPath`.
    ///
    /// Each returned entry is `(avg_fixed_point_i128, original_key)`.
    /// Divide by `AVG_FIXED_POINT_SCALE` (`10^19`) to recover a float
    /// view if you need one — noting an `f64` view is approximate at
    /// this scale; the `i128` fixed-point value is the exact consensus
    /// value.
    pub fn indexed_avg_top_k<'b, B, P>(
        &self,
        path: P,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(i128, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.indexed_axis_top_k_generic(
            path.into(),
            IndexAxis::Avg,
            k,
            descending,
            transaction,
            grove_version,
            decode_avg_secondary_key,
        )
    }

    /// Paginated form of [`Self::indexed_avg_top_k`]. Skips `offset`
    /// entries in the directional scan before collecting up to `k`
    /// results.
    pub fn indexed_avg_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(i128, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.indexed_axis_top_k_paginated_generic(
            path.into(),
            IndexAxis::Avg,
            k,
            offset,
            descending,
            transaction,
            grove_version,
            decode_avg_secondary_key,
        )
    }

    /// Iterate the avg-axis secondary over an avg range `[lo_avg,
    /// hi_avg_inclusive]` and return matching `(avg_fixed_point,
    /// original_key)` entries up to `limit`. Direction is controlled
    /// by `descending`.
    ///
    /// The `lo_avg` / `hi_avg` bounds are i128 fixed-point values at
    /// `SCALE = 10^19`. To filter by a floating-point threshold `t`,
    /// pass `(t * SCALE) as i128`. `lo_avg > hi_avg` returns an empty
    /// vector. `lo_avg == i128::MIN && hi_avg == i128::MAX` is
    /// equivalent to a full scan.
    ///
    /// No `indexed_avg_range_aggregate` exists — averaging an average
    /// over a range is not a closed-form aggregate. Callers that need
    /// "aggregate avg in range" should compute it client-side from
    /// `indexed_count_range_aggregate` + `indexed_sum_range_aggregate`
    /// against the same path's count and sum secondaries.
    pub fn indexed_avg_range<'b, B, P>(
        &self,
        path: P,
        lo_avg: i128,
        hi_avg: i128,
        descending: bool,
        limit: u16,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(i128, Vec<u8>)>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let cost = OperationCost::default();
        if lo_avg > hi_avg {
            return Ok(Vec::new()).wrap_with_cost(cost);
        }

        let lo_bytes = encode_avg_sort_key(lo_avg).to_vec();
        let upper_bytes = if hi_avg == i128::MAX {
            None
        } else {
            // `hi_avg + 1` is the exclusive upper boundary at the next
            // representable avg; safe because we already returned for
            // `hi_avg == i128::MAX` above.
            Some(encode_avg_sort_key(hi_avg + 1).to_vec())
        };

        self.indexed_axis_range_generic(
            path.into(),
            IndexAxis::Avg,
            lo_bytes,
            upper_bytes,
            descending,
            limit,
            transaction,
            grove_version,
            |sk| {
                decode_avg_secondary_key(sk).inspect(|(avg, _)| {
                    debug_assert!(*avg >= lo_avg && *avg <= hi_avg);
                })
            },
        )
        .add_cost(cost)
    }

    /// Shared scaffolding for the per-axis direct query APIs: validate
    /// that the indexed-tree at `path` carries the requested `axis`,
    /// read that axis's secondary root key, and open the secondary
    /// merk at the derived prefix.
    ///
    /// Used by every `indexed_count_*`, `indexed_sum_*`, and
    /// `indexed_avg_*` function. Returns `Error::InvalidPath` if the
    /// axis is not indexed at this path (e.g. an avg query against a
    /// PCIT, or a sum query against a PCPSIT without the sum axis).
    fn open_validated_axis_secondary<'db, 'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        axis: IndexAxis,
        tx_ref: &'db Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();
        let secondary_root_key = cost_return_on_error!(
            &mut cost,
            self.read_indexed_secondary_root_key_for_axis(
                path.clone(),
                axis,
                tx_ref,
                None,
                grove_version,
            )
        );
        self.open_indexed_secondary_at_path(
            path,
            axis,
            secondary_root_key,
            tx_ref,
            None,
            grove_version,
        )
        .add_cost(cost)
    }

    /// Read the per-axis `secondary_root_key` from the indexed-tree
    /// element at the given `path`, validating that the requested `axis`
    /// is supported by the variant at the path's last segment.
    ///
    /// Axis-compatibility rules:
    /// - [`IndexAxis::Count`] accepts
    ///   [`Element::ProvableCountIndexedTree`] (single-axis, always count)
    ///   or [`Element::ProvableCountProvableSumIndexedTree`] (PCPSIT) iff
    ///   its TLV contains the count axis.
    /// - [`IndexAxis::Sum`] accepts
    ///   [`Element::ProvableSumIndexedTree`] (single-axis, always sum) or
    ///   PCPSIT iff its TLV contains the sum axis.
    /// - [`IndexAxis::Avg`] only accepts PCPSIT, and only if its TLV
    ///   contains the avg axis.
    ///
    /// Any other variant — or a PCPSIT whose TLV does not carry the
    /// requested axis — is rejected with
    /// `Error::InvalidPath("<axis> axis not indexed at this path")`.
    fn read_indexed_secondary_root_key_for_axis<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        axis: IndexAxis,
        transaction: &'db Transaction,
        batch: Option<&'db StorageBatch>,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Vec<u8>>, Error> {
        let mut cost = OperationCost::default();
        let (parent_path, indexed_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot query an indexed tree at the root path".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };
        let parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(parent_path, transaction, batch, grove_version)
        );
        let element = cost_return_on_error!(
            &mut cost,
            Element::get(&parent_merk, indexed_key, true, grove_version).map_err(Error::MerkError)
        );
        match (axis, element.underlying()) {
            // PCIT carries a single secondary; only Count axis is valid.
            (IndexAxis::Count, Element::ProvableCountIndexedTree(_, secondary, ..)) => {
                Ok(secondary.clone()).wrap_with_cost(cost)
            }
            // PSIT carries a single secondary; only Sum axis is valid.
            (IndexAxis::Sum, Element::ProvableSumIndexedTree(_, secondary, ..)) => {
                Ok(secondary.clone()).wrap_with_cost(cost)
            }
            // PCPSIT carries a TLV of 1..=3 axis-tagged secondaries; the
            // requested axis must appear in the TLV.
            (_, Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _)) => {
                let want_tag = axis.tag();
                match axes.iter().find(|(t, _)| *t == want_tag) {
                    Some((_, sec)) => Ok(sec.clone()).wrap_with_cost(cost),
                    None => Err(Error::InvalidPath(format!(
                        "{:?} axis not indexed at this path",
                        axis
                    )))
                    .wrap_with_cost(cost),
                }
            }
            _ => Err(Error::InvalidPath(format!(
                "{:?} axis not indexed at this path",
                axis
            )))
            .wrap_with_cost(cost),
        }
    }

    /// Delete an item from a `CountIndexedTree` element. Removes the
    /// matching secondary index entry and updates the parent's element
    /// bytes to reflect the new (primary_root_key, secondary_root_key,
    /// count_value).
    ///
    /// Returns `Ok(true)` when an item was removed, `Ok(false)` when the
    /// key did not exist (no-op).
    pub fn delete_from_count_indexed_tree<'b, B, P>(
        &self,
        path: P,
        item_key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.delete_from_indexed_tree_via_batch(
            path,
            item_key,
            TreeType::ProvableCountIndexedTree,
            "delete_from_count_indexed_tree",
            transaction,
            grove_version,
        )
    }

    // -----------------------------------------------------------------
    // ProvableSumIndexedTree (PSIT) direct-insert / direct-delete path.
    // -----------------------------------------------------------------

    /// Insert (or update) an item under a key into a
    /// `ProvableSumIndexedTree` (PSIT) primary, mirroring the change
    /// into the sum-ordered secondary index and updating the parent's
    /// stored element bytes (primary_root_key, secondary_root_key,
    /// sum_value) via the H1-A three-input value hash.
    ///
    /// `path` is the path to the PSIT element (its primary Merk's path). The
    /// child element must carry an `i64` sum (see
    /// [`Element::is_sum_bearing_child`]); `BigSumTree` is excluded because
    /// its aggregate is `i128`. Unsupported children return `InvalidInput`.
    pub fn insert_into_provable_sum_indexed_tree<'b, B, P>(
        &self,
        path: P,
        item_key: &[u8],
        item: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.insert_into_indexed_tree_via_batch(
            path,
            item_key,
            item,
            TreeType::ProvableSumIndexedTree,
            "insert_into_provable_sum_indexed_tree",
            transaction,
            grove_version,
        )
    }

    /// Delete an item from a `ProvableSumIndexedTree` primary.
    pub fn delete_from_provable_sum_indexed_tree<'b, B, P>(
        &self,
        path: P,
        item_key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.delete_from_indexed_tree_via_batch(
            path,
            item_key,
            TreeType::ProvableSumIndexedTree,
            "delete_from_provable_sum_indexed_tree",
            transaction,
            grove_version,
        )
    }

    // -----------------------------------------------------------------
    // ProvableCountProvableSumIndexedTree (PCPSIT) direct-insert /
    // direct-delete path.
    // -----------------------------------------------------------------

    /// Insert (or update) an item under a key into a
    /// `ProvableCountProvableSumIndexedTree` primary, mirroring the
    /// change into every axis's secondary index that the parent's
    /// `axes` field declares, and updating the parent's element bytes
    /// (primary_root_key, count_value, sum_value, axes) via the H1-A
    /// three-input hash with `axes_digest` as the second input.
    pub fn insert_into_provable_count_provable_sum_indexed_tree<'b, B, P>(
        &self,
        path: P,
        item_key: &[u8],
        item: Element,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.insert_into_indexed_tree_via_batch(
            path,
            item_key,
            item,
            TreeType::ProvableCountProvableSumIndexedTree,
            "insert_into_provable_count_provable_sum_indexed_tree",
            transaction,
            grove_version,
        )
    }

    /// Delete an item from a `ProvableCountProvableSumIndexedTree`
    /// primary, removing its secondary entries from every configured
    /// axis.
    pub fn delete_from_provable_count_provable_sum_indexed_tree<'b, B, P>(
        &self,
        path: P,
        item_key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.delete_from_indexed_tree_via_batch(
            path,
            item_key,
            TreeType::ProvableCountProvableSumIndexedTree,
            "delete_from_provable_count_provable_sum_indexed_tree",
            transaction,
            grove_version,
        )
    }
}

/// Apply a PCPSIT axis secondary mirror covering insert, update, and
/// delete via the (old, new) Option pair. Reads `old_count`/`old_sum`
/// from the prior primary state and `new_count`/`new_sum` from the
/// post-mutation state.
///
/// The secondary entry is a no-payload `Item` whose own sum / count
/// contribution comes from its position in a sum/count-bearing tree.
/// For the sum axis the secondary entry is a `SumItem(sum)`; for the
/// avg axis the secondary entry is an `ItemWithSumItem(empty, sum)` so
/// both count (= 1) and sum (= the entry's sum_value) propagate to the
/// secondary's `ProvableCountProvableSumTree`. For the count axis the
/// secondary entry is a plain `Item` (count = 1, no sum).
#[allow(clippy::too_many_arguments)]
pub(crate) fn mirror_indexed_axis_to_secondary<'db, S: StorageContext<'db>>(
    secondary: &mut Merk<S>,
    axis: IndexAxis,
    item_key: &[u8],
    old_count: Option<u64>,
    old_sum: Option<i64>,
    new_count: Option<u64>,
    new_sum: Option<i64>,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    let secondary_tree_type = axis_secondary_tree_type(axis);

    // The per-axis secondary payload for an entry with the given
    // aggregate values. Kept in lockstep with the insert branch below:
    // - Count → constant empty Item (contributes count = 1)
    // - Sum   → SumItem(sum)
    // - Avg   → ItemWithSumItem(empty, sum) (contributes (1, sum))
    let axis_payload = |sum: i64| -> Element {
        match axis {
            IndexAxis::Count => Element::new_item(Vec::new()),
            IndexAxis::Sum => Element::new_sum_item(sum),
            IndexAxis::Avg => Element::new_item_with_sum_item(Vec::new(), sum),
        }
    };

    // Compute old and new sort keys. Either may be None (no entry).
    let old_key = match (old_count, old_sum) {
        (Some(c), Some(s)) => Some(make_axis_secondary_key(axis, c, s, item_key)),
        _ => None,
    };
    let new_key = match (new_count, new_sum) {
        (Some(c), Some(s)) => Some(make_axis_secondary_key(axis, c, s, item_key)),
        _ => None,
    };

    // Fast path: skip the delete+insert only when BOTH the sort key AND
    // the stored payload are unchanged.
    //
    // Equal sort keys do NOT imply equal payloads on the Avg axis: the
    // avg key is floor(sum * 10^19 / count) while the payload carries the
    // raw `sum`, so e.g. (count, sum) = (1, 5) and (2, 10) share a key
    // (avg 5.0) but differ in payload sum (5 vs 10). Returning early on
    // key-equality alone would leave a stale hash-committed sum in the
    // avg secondary. On the Count / Sum axes the payload is a pure
    // function of what the key already encodes, so this reduces to the
    // former key-only check and preserves the fast path for the common
    // case.
    if old_key == new_key && new_key.is_some() {
        let payload_unchanged = match (old_sum, new_sum) {
            (Some(os), Some(ns)) => axis_payload(os) == axis_payload(ns),
            _ => false,
        };
        if payload_unchanged {
            // The sort key didn't move and the stored value is identical;
            // the previous insert already wrote the correct value, so
            // nothing more is needed.
            return Ok(()).wrap_with_cost(cost);
        }
    }

    if let Some(ok) = &old_key {
        cost_return_on_error!(
            &mut cost,
            Element::delete(
                secondary,
                ok.as_slice(),
                None,
                false,
                secondary_tree_type,
                grove_version,
            )
            .map_err(Error::MerkError)
        );
    }
    if let (Some(nk), Some(new_sum_val)) = (&new_key, new_sum) {
        // Derived by the same `axis_payload` closure the fast-path
        // equality check above uses, so the two stay in lockstep:
        // - Count → empty Item (secondary is a ProvableCountTree; every
        //   entry contributes count = 1)
        // - Sum   → SumItem(sum) (secondary is a ProvableSumTree)
        // - Avg   → ItemWithSumItem(empty, sum) (secondary is a
        //   ProvableCountProvableSumTree; contributes (1, sum))
        let entry = axis_payload(new_sum_val);
        cost_return_on_error!(
            &mut cost,
            entry
                .insert(secondary, nk.as_slice(), None, grove_version)
                .map_err(Error::MerkError)
        );
    }
    Ok(()).wrap_with_cost(cost)
}

/// Maximum allowed length for a key inserted directly into a cidx
/// primary's content (the item key the secondary will mirror).
///
/// The secondary key is `count_be (8 bytes) ‖ item_key`. Merk's internal
/// invariant requires Merk-tree keys to be `< 256` bytes (enforced by
/// `debug_assert!` in `merk/src/tree/link.rs`), so the secondary key
/// must be at most 255 bytes — i.e. `item_key.len() <= 247`. Generic
/// GroveDB allows 255-byte keys, so cidx primaries have an additional
/// 8-byte ceiling relative to the generic limit.
///
/// Every cidx primary write path (direct insert, batch insert) MUST
/// enforce this on the item key before the merk write. A violation
/// would corrupt the secondary Merk via the debug-assert in production
/// builds (where assertions are disabled) by silently writing a key
/// the Merk format does not support, leading to invariant breaks on
/// later reads.
pub const MAX_CIDX_ITEM_KEY_LEN: usize = 247;

/// Maximum item-key length for a `ProvableCountProvableSumIndexedTree`
/// whose configured axes include `Avg`. The avg secondary is keyed by
/// `avg_sortable_be (16 bytes) ‖ item_key`, so to keep the secondary key
/// under Merk's 256-byte ceiling the item key must be `<= 239` bytes —
/// 8 bytes tighter than the count/sum (`MAX_CIDX_ITEM_KEY_LEN`) limit.
pub const MAX_AVG_INDEXED_ITEM_KEY_LEN: usize = 239;

/// Maximum item-key length permitted for a given axis: the sort-key
/// prefix width is what eats into Merk's 256-byte key ceiling.
#[inline]
pub(crate) fn max_item_key_len_for_axis(axis: IndexAxis) -> usize {
    match axis {
        IndexAxis::Count | IndexAxis::Sum => MAX_CIDX_ITEM_KEY_LEN,
        IndexAxis::Avg => MAX_AVG_INDEXED_ITEM_KEY_LEN,
    }
}

#[inline]
fn make_secondary_key(count: u64, item_key: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(8 + item_key.len());
    k.extend_from_slice(&count.to_be_bytes());
    k.extend_from_slice(item_key);
    k
}

/// Inverse of `make_secondary_key`: split a secondary key into
/// `(count, original_key)`. Returns `None` if the key is shorter than the
/// 8-byte count prefix.
#[inline]
fn decode_secondary_key(secondary_key: &[u8]) -> Option<(u64, Vec<u8>)> {
    if secondary_key.len() < 8 {
        return None;
    }
    let mut count_bytes = [0u8; 8];
    count_bytes.copy_from_slice(&secondary_key[..8]);
    let count = u64::from_be_bytes(count_bytes);
    Some((count, secondary_key[8..].to_vec()))
}

/// Inverse of `make_axis_secondary_key(IndexAxis::Sum, ..)`: split a
/// sum-secondary key into `(sum, original_key)`. Returns `None` if the key
/// is shorter than the 8-byte sum-sort prefix.
///
/// The 8-byte prefix is the sign-flipped big-endian encoding of an `i64`
/// (see [`grovedb_element::indexed::encode_sum_sort_key`]); the suffix is
/// the entry's original primary key.
#[inline]
fn decode_sum_secondary_key(secondary_key: &[u8]) -> Option<(i64, Vec<u8>)> {
    if secondary_key.len() < 8 {
        return None;
    }
    let mut sum_bytes = [0u8; 8];
    sum_bytes.copy_from_slice(&secondary_key[..8]);
    let sum = decode_sum_sort_key(&sum_bytes);
    Some((sum, secondary_key[8..].to_vec()))
}

/// Inverse of `make_axis_secondary_key(IndexAxis::Avg, ..)`: split an
/// avg-secondary key into `(avg_fixed_point_i128, original_key)`. Returns
/// `None` if the key is shorter than the 16-byte avg-sort prefix.
///
/// The 16-byte prefix is the sign-flipped big-endian encoding of an `i128`
/// fixed-point average with `SCALE = 10^19` (see
/// [`grovedb_element::indexed::encode_avg_sort_key`]); the suffix is the
/// entry's original primary key.
#[inline]
fn decode_avg_secondary_key(secondary_key: &[u8]) -> Option<(i128, Vec<u8>)> {
    if secondary_key.len() < 16 {
        return None;
    }
    let mut avg_bytes = [0u8; 16];
    avg_bytes.copy_from_slice(&secondary_key[..16]);
    let avg = decode_avg_sort_key(&avg_bytes);
    Some((avg, secondary_key[16..].to_vec()))
}

/// Width (in bytes) of the per-axis sort-key prefix in a secondary key.
#[inline]
fn axis_sort_prefix_len(axis: IndexAxis) -> usize {
    match axis {
        IndexAxis::Count | IndexAxis::Sum => 8,
        IndexAxis::Avg => 16,
    }
}

/// Build a corruption error for a secondary key that's shorter than its
/// axis's sort-key prefix. Used by every per-axis direct query API to
/// surface storage corruption (rather than silently dropping the entry)
/// when the secondary's keyspace contains a malformed entry.
#[inline]
fn corrupted_secondary_key_error(axis: IndexAxis, secondary_key: &[u8]) -> Error {
    Error::CorruptedData(format!(
        "secondary key in indexed-tree (axis {:?}) is shorter than {} bytes: {:?}",
        axis,
        axis_sort_prefix_len(axis),
        secondary_key
    ))
}

#[cfg(test)]
mod secondary_key_codec_tests {
    //! The secondary keyspace is `sort_key ‖ item_key`, and every per-axis
    //! direct query API round-trips through these helpers. They are the reason
    //! a caller gets `(count, original_key)` back rather than raw bytes, and
    //! the reason a malformed row surfaces as `CorruptedData` instead of being
    //! silently dropped — so the prefix widths and the short-key rejections are
    //! pinned here directly.

    use grovedb_element::indexed::IndexAxis;

    use super::{
        axis_sort_key_len, axis_sort_prefix_len, corrupted_secondary_key_error,
        decode_avg_secondary_key, decode_secondary_key, decode_sum_secondary_key,
        make_axis_secondary_key, make_secondary_key, max_item_key_len_for_axis,
        MAX_AVG_INDEXED_ITEM_KEY_LEN, MAX_CIDX_ITEM_KEY_LEN,
    };
    use crate::Error;

    #[test]
    fn count_secondary_keys_round_trip_and_sort_by_count() {
        let key = make_secondary_key(258, b"row");
        assert_eq!(
            key,
            vec![0, 0, 0, 0, 0, 0, 1, 2, b'r', b'o', b'w'],
            "count prefix must be 8-byte big-endian followed by the item key"
        );
        assert_eq!(
            decode_secondary_key(&key),
            Some((258u64, b"row".to_vec())),
            "decode must invert make_secondary_key"
        );
        // Big-endian is what makes byte order equal count order.
        assert!(make_secondary_key(2, b"a") < make_secondary_key(10, b"a"));
        // The shared axis builder must agree with the count-specific one.
        assert_eq!(
            make_axis_secondary_key(IndexAxis::Count, 258, 0, b"row"),
            key
        );
    }

    #[test]
    fn sum_and_avg_secondary_keys_round_trip_through_their_axis_builders() {
        let sum_key = make_axis_secondary_key(IndexAxis::Sum, 0, -9, b"row");
        assert_eq!(sum_key.len(), 8 + 3, "sum prefix is 8 bytes");
        assert_eq!(
            decode_sum_secondary_key(&sum_key),
            Some((-9i64, b"row".to_vec()))
        );
        // Sign-flipped big-endian: negative sums sort below positive ones.
        assert!(sum_key < make_axis_secondary_key(IndexAxis::Sum, 0, 1, b"row"));

        // avg = floor(sum * SCALE / count) = 5 * SCALE for (count 2, sum 10).
        let avg_key = make_axis_secondary_key(IndexAxis::Avg, 2, 10, b"row");
        assert_eq!(avg_key.len(), 16 + 3, "avg prefix is 16 bytes");
        assert_eq!(
            decode_avg_secondary_key(&avg_key),
            Some((
                5 * grovedb_element::indexed::AVG_FIXED_POINT_SCALE,
                b"row".to_vec()
            ))
        );
        assert!(avg_key < make_axis_secondary_key(IndexAxis::Avg, 2, 11, b"row"));
    }

    /// A key one byte shorter than its axis prefix must decode to `None` — that
    /// is what makes the query cores raise `CorruptedData` rather than index
    /// out of bounds or truncate a row into a bogus value.
    #[test]
    fn a_key_shorter_than_its_axis_prefix_fails_to_decode() {
        assert_eq!(decode_secondary_key(&[0u8; 7]), None);
        assert_eq!(
            decode_secondary_key(&[0u8; 8]),
            Some((0u64, Vec::new())),
            "exactly the prefix width is a valid (empty item key) row"
        );
        assert_eq!(decode_sum_secondary_key(&[0u8; 7]), None);
        assert!(decode_sum_secondary_key(&[0u8; 8]).is_some());
        assert_eq!(decode_avg_secondary_key(&[0u8; 15]), None);
        assert!(decode_avg_secondary_key(&[0u8; 16]).is_some());
    }

    #[test]
    fn prefix_widths_and_key_ceilings_agree_across_the_axis_helpers() {
        for (axis, width) in [
            (IndexAxis::Count, 8usize),
            (IndexAxis::Sum, 8),
            (IndexAxis::Avg, 16),
        ] {
            assert_eq!(axis_sort_prefix_len(axis), width, "{axis:?} prefix width");
            assert_eq!(axis_sort_key_len(axis), width, "{axis:?} sort key length");
            // Merk requires keys < 256 bytes, so the ceiling is 255 - prefix.
            assert_eq!(
                max_item_key_len_for_axis(axis),
                255 - width,
                "{axis:?} item-key ceiling must leave room for its sort key"
            );
            assert_eq!(
                make_axis_secondary_key(axis, 1, 1, &vec![b'k'; max_item_key_len_for_axis(axis)])
                    .len(),
                255,
                "a max-length item key must produce exactly a 255-byte secondary key"
            );
        }
        assert_eq!(
            max_item_key_len_for_axis(IndexAxis::Count),
            MAX_CIDX_ITEM_KEY_LEN
        );
        assert_eq!(
            max_item_key_len_for_axis(IndexAxis::Avg),
            MAX_AVG_INDEXED_ITEM_KEY_LEN
        );
    }

    #[test]
    fn the_corruption_error_names_the_axis_and_its_expected_width() {
        match corrupted_secondary_key_error(IndexAxis::Avg, &[1, 2, 3]) {
            Error::CorruptedData(message) => {
                assert!(
                    message.contains("axis Avg") && message.contains("shorter than 16 bytes"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod bug2_avg_axis_mirror_tests {
    //! BUG 2 regression: `mirror_indexed_axis_to_secondary` must not
    //! early-return on the Avg axis when the sort key is unchanged but
    //! the stored payload sum differs.
    //!
    //! The avg sort key is `floor(sum * 10^19 / count)`, while the stored
    //! payload is `ItemWithSumItem(_, sum)`. Two `(count, sum)` pairs can
    //! share a key yet differ in payload sum — e.g. `(1, 5)` and `(2, 10)`
    //! both encode avg `5.0` but carry payload sums `5` and `10`. The old
    //! key-only early-return left the stale hash-committed sum `5` in the
    //! avg secondary.
    //!
    //! This path is currently unreachable through the public dedicated
    //! APIs (each child contributes count 0 or 1, so the mirror never sees
    //! a `(1, 5) -> (2, 10)` transition for one item key), so we drive the
    //! module-private mirror function directly against a real Avg
    //! secondary Merk.

    use grovedb_costs::OperationCost;
    use grovedb_element::indexed::{compute_avg_fixed_point, IndexAxis};
    use grovedb_merk::{element::get::ElementFetchFromStorageExtensions, tree::AggregateData};
    use grovedb_path::SubtreePath;
    use grovedb_storage::StorageBatch;
    use grovedb_version::version::GroveVersion;

    use super::{make_axis_secondary_key, mirror_indexed_axis_to_secondary};
    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element,
    };

    /// Sanity: `(1, 5)` and `(2, 10)` share an avg sort key but produce
    /// different payload sums — the precondition that made the old
    /// key-only early-return unsound.
    #[test]
    fn avg_key_collision_with_distinct_payload_sums() {
        let item_key = b"row";
        let k_1_5 = make_axis_secondary_key(IndexAxis::Avg, 1, 5, item_key);
        let k_2_10 = make_axis_secondary_key(IndexAxis::Avg, 2, 10, item_key);
        assert_eq!(
            k_1_5, k_2_10,
            "(1,5) and (2,10) must map to the same avg sort key"
        );
        assert_eq!(
            compute_avg_fixed_point(5, 1),
            compute_avg_fixed_point(10, 2),
            "avg fixed points must match"
        );
        // Payloads differ (the hash-committed sum the mirror stores).
        assert_ne!(
            Element::new_item_with_sum_item(Vec::new(), 5),
            Element::new_item_with_sum_item(Vec::new(), 10),
            "payload sums 5 and 10 must differ"
        );
    }

    /// Drive the mirror directly: first write `(1, 5)`, then transition to
    /// `(2, 10)` (same avg key). The stored avg-secondary payload sum must
    /// update to `10`; the old key-only early-return would have left the
    /// stale `5`.
    #[test]
    fn avg_axis_mirror_updates_stale_payload_when_key_unchanged() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Establish a PCPSIT with an Avg axis so the secondary namespace
        // is valid.
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(IndexAxis::Avg.tag(), None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("empty PCPSIT insert");

        let tx = db.start_transaction();
        let item_key = b"row";
        let shared_key = make_axis_secondary_key(IndexAxis::Avg, 1, 5, item_key);
        assert_eq!(
            shared_key,
            make_axis_secondary_key(IndexAxis::Avg, 2, 10, item_key),
            "test setup: keys must collide"
        );

        // Open the Avg secondary (empty) and drive the mirror twice
        // against the same in-memory Merk. A StorageBatch is required so
        // the merk's Element::insert can stage its commit_batch.
        let batch = StorageBatch::new();
        let path_segments: [&[u8]; 2] = [TEST_LEAF, b"pcpsit".as_ref()];
        let path: SubtreePath<_> = (&path_segments).into();
        let mut secondary = db
            .open_indexed_secondary_at_path(
                path,
                IndexAxis::Avg,
                None,
                &tx,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("open empty avg secondary");

        // 1) Insert (count=1, sum=5).
        mirror_indexed_axis_to_secondary(
            &mut secondary,
            IndexAxis::Avg,
            item_key,
            None,
            None,
            Some(1),
            Some(5),
            grove_version,
        )
        .unwrap()
        .expect("insert (1,5)");

        let after_first = Element::get(&secondary, shared_key.as_slice(), true, grove_version)
            .unwrap()
            .expect("entry present after first mirror");
        assert_eq!(
            after_first.sum_value_or_default(),
            5,
            "payload sum after (1,5) must be 5"
        );

        // 2) Transition to (count=2, sum=10). Same avg key, new sum.
        mirror_indexed_axis_to_secondary(
            &mut secondary,
            IndexAxis::Avg,
            item_key,
            Some(1),
            Some(5),
            Some(2),
            Some(10),
            grove_version,
        )
        .unwrap()
        .expect("transition (1,5)->(2,10)");

        let after_second = Element::get(&secondary, shared_key.as_slice(), true, grove_version)
            .unwrap()
            .expect("entry present after second mirror");
        assert_eq!(
            after_second.sum_value_or_default(),
            10,
            "payload sum must update to 10 even though the avg sort key did not \
             move (BUG 2 regression: old key-only early-return left stale 5)"
        );
    }

    /// The fast path must still short-circuit when BOTH the key and the
    /// payload are unchanged (a genuine no-op transition). We verify the
    /// stored payload is untouched for an identical (count, sum) rewrite.
    #[test]
    fn avg_axis_mirror_noop_when_key_and_payload_unchanged() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(IndexAxis::Avg.tag(), None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("empty PCPSIT insert");

        let tx = db.start_transaction();
        let item_key = b"row";
        let batch = StorageBatch::new();
        let path_segments: [&[u8]; 2] = [TEST_LEAF, b"pcpsit".as_ref()];
        let path: SubtreePath<_> = (&path_segments).into();
        let mut secondary = db
            .open_indexed_secondary_at_path(
                path,
                IndexAxis::Avg,
                None,
                &tx,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("open empty avg secondary");

        mirror_indexed_axis_to_secondary(
            &mut secondary,
            IndexAxis::Avg,
            item_key,
            None,
            None,
            Some(2),
            Some(10),
            grove_version,
        )
        .unwrap()
        .expect("insert (2,10)");

        // Identical (count, sum) rewrite: key AND payload unchanged.
        // Assert on the COST, not just the stored value — a delete+reinsert
        // would leave the same value behind, so a value-only assertion passes
        // even with the fast path deleted entirely.
        let mut noop_cost = OperationCost::default();
        mirror_indexed_axis_to_secondary(
            &mut secondary,
            IndexAxis::Avg,
            item_key,
            Some(2),
            Some(10),
            Some(2),
            Some(10),
            grove_version,
        )
        .unwrap_add_cost(&mut noop_cost)
        .expect("noop rewrite");
        // Byte deltas are NOT a usable signal here: a delete-then-reinsert of
        // an identical entry nets zero added/replaced/removed bytes. Merk
        // work is the discriminator — the fast path touches storage not at
        // all, while the delete+insert it replaces seeks and rehashes.
        assert_eq!(
            noop_cost.seek_count, 0,
            "an unchanged rewrite must not touch storage"
        );
        assert_eq!(
            noop_cost.hash_node_calls, 0,
            "an unchanged rewrite must not rehash the secondary"
        );
        assert_eq!(
            noop_cost.storage_loaded_bytes, 0,
            "an unchanged rewrite must not load"
        );

        let key = make_axis_secondary_key(IndexAxis::Avg, 2, 10, item_key);
        let entry = Element::get(&secondary, key.as_slice(), true, grove_version)
            .unwrap()
            .expect("entry present");
        assert_eq!(entry.sum_value_or_default(), 10, "payload must remain 10");
    }

    /// A delete reaches the mirror as `new_count = new_sum = None`, which must
    /// resolve to "no new key" and leave only the removal. The row has to
    /// disappear from the axis secondary — not merely stop being findable — or
    /// the secondary's own aggregate keeps counting it.
    #[test]
    fn avg_axis_mirror_removes_the_row_when_the_new_state_is_absent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(IndexAxis::Avg.tag(), None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("empty PCPSIT insert");

        let tx = db.start_transaction();
        let item_key = b"row";
        let batch = StorageBatch::new();
        let path_segments: [&[u8]; 2] = [TEST_LEAF, b"pcpsit".as_ref()];
        let path: SubtreePath<_> = (&path_segments).into();
        let mut secondary = db
            .open_indexed_secondary_at_path(
                path,
                IndexAxis::Avg,
                None,
                &tx,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("open empty avg secondary");

        mirror_indexed_axis_to_secondary(
            &mut secondary,
            IndexAxis::Avg,
            item_key,
            None,
            None,
            Some(2),
            Some(10),
            grove_version,
        )
        .unwrap()
        .expect("insert (2,10)");
        let key = make_axis_secondary_key(IndexAxis::Avg, 2, 10, item_key);
        assert!(
            Element::get(&secondary, key.as_slice(), true, grove_version)
                .unwrap()
                .is_ok(),
            "entry must exist before the delete"
        );
        let (_, _, aggregate_before) = secondary
            .root_hash_key_and_aggregate_data()
            .unwrap()
            .expect("secondary root state");
        assert_eq!(
            aggregate_before,
            AggregateData::ProvableCountAndProvableSum(1, 10),
            "the avg secondary must aggregate (count 1, sum 10) while the row is present"
        );

        mirror_indexed_axis_to_secondary(
            &mut secondary,
            IndexAxis::Avg,
            item_key,
            Some(2),
            Some(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete transition");

        assert!(
            Element::get(&secondary, key.as_slice(), true, grove_version)
                .unwrap()
                .is_err(),
            "the row must be gone from the avg secondary"
        );
        let (_, root_key_after, aggregate_after) = secondary
            .root_hash_key_and_aggregate_data()
            .unwrap()
            .expect("secondary root state");
        assert!(
            root_key_after.is_none(),
            "the secondary must be empty after removing its only row"
        );
        assert_eq!(
            aggregate_after,
            AggregateData::NoAggregateData,
            "an empty secondary reports no aggregate at all, so the removed row \
             cannot still be contributing"
        );
    }
}
