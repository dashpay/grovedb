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
    tree::{kv::ValueDefinedCostType, AggregateData, TreeNode},
    Merk, TreeType,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbTransactionContext, RocksDbStorage},
    RawIterator, Storage, StorageBatch, StorageContext,
};
use grovedb_version::version::GroveVersion;

use crate::{
    query_result_type::IndexedAxisEntry, util::TxRef, Element, Error, GroveDb, Transaction,
    TransactionArg,
};

/// The canonical row definition lives in
/// [`crate::operations::proof::indexed_axis::canonical_row`] so a
/// verify-only build can reach it — a light client rebuilds the row a
/// proof claims from the same definition the mirror writes with. Re-exported
/// here because this is where the write paths look for it.
pub(crate) use crate::operations::proof::indexed_axis::canonical_row::{
    axis_row_reference, axis_sort_key_len, decode_axis_row_reference, make_axis_secondary_key,
};

/// Per-axis Merk tree type to open the secondary with.
///
/// Every axis uses the dual-aggregate
/// [`TreeType::ProvableCountProvableSumTree`]: the count half is what
/// makes positional queries provable in `O(log n)` (the count-offset
/// proof primitive skips whole subtrees via counted node commitments),
/// and the sum half is what makes a TOTAL over a value band answerable
/// as one committed scalar ([`AggregateFold::Total`], issue #806).
///
/// **Dual-aggregate is a SECURITY requirement here, not a
/// convenience.** The single-aggregate node hashes share a preimage
/// layout (`node_hash_with_count` and `node_hash_with_sum` are both
/// `Blake3(kv ‖ l ‖ r ‖ 8 bytes)` with no domain tag), so on a
/// single-aggregate secondary a count proof could be node-type
/// relabeled into a byte-different "sum" proof reconstructing the
/// IDENTICAL root — making a band Total forgeable from a Population
/// proof at zero cost. Dual-aggregate nodes hash a 112-byte preimage
/// (`count_be8 ‖ sum_be8`), which closes the rewrite. Do not
/// "optimize" any axis back to a single-aggregate tree type without
/// first adding domain separation to the node-hash functions
/// (per the #809 security audit, finding C).
#[inline]
pub(crate) fn axis_secondary_tree_type(axis: IndexAxis) -> TreeType {
    match axis {
        // Each count entry contributes (count = 1, sum = its
        // count_value as an i64) — the sum half is the band-total.
        IndexAxis::Count => TreeType::ProvableCountProvableSumTree,
        // Each sum entry contributes (count = 1, sum = its own
        // SumValue).
        IndexAxis::Sum => TreeType::ProvableCountProvableSumTree,
        // Each avg entry contributes (count = 1, sum = item's SumValue).
        IndexAxis::Avg => TreeType::ProvableCountProvableSumTree,
    }
}

/// One primary entry's mirror-relevant state.
///
/// The aggregates decide the row's sort key and carried sum; the value
/// hash decides its reference commitment. Every mirror compares both
/// sides of a transition as all three, because a canonical row binds the
/// primary node — an entry whose value changed while its `(count, sum)`
/// stayed put still needs its row rewritten, which is exactly the case
/// the pre-reference mirror was free to skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IndexedEntryState {
    /// The entry's count aggregate.
    pub(crate) count: u64,
    /// The entry's sum aggregate.
    pub(crate) sum: i64,
    /// The primary node's Merk-stored committed value hash — a simple
    /// hash for an item, a layered/combined hash for a tree, a combined
    /// hash for a nested reference. This is the immediate-node binding
    /// target; it is NOT resolved through to a terminal (see
    /// [`INDEXED_SECONDARY_MAX_HOP`]).
    pub(crate) value_hash: grovedb_merk::CryptoHash,
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

    /// Rebuild EVERY configured axis secondary of an indexed tree (PCIT /
    /// PSIT / PCPSIT) from scratch by walking its primary Merk — the
    /// indexed-tree family's `REINDEX`.
    ///
    /// **When you need this: normally, never.**
    ///
    /// This is a repair API. No supported write can desync a secondary
    /// any more, so nothing in ordinary operation requires calling it:
    ///
    /// - The dedicated `insert_into_*` / `delete_from_*` APIs maintain
    ///   the secondaries inline.
    /// - The batch path mirrors every axis, and a generic write that
    ///   targets an indexed primary directly is rejected outright rather
    ///   than silently skipping the mirror.
    /// - A deep write *under* a child of the primary — inserting into a
    ///   sub-`CountTree` held in the primary, so the sub-tree's aggregate
    ///   propagates up into the primary's element bytes — keeps the index
    ///   in sync through both paths, pinned by
    ///   `a_deep_write_under_an_indexed_child_keeps_the_secondary_in_sync`.
    ///
    /// What remains is repair of secondaries damaged by something outside
    /// those paths: storage-level corruption, a partially applied
    /// migration, or a bug. `verify_grovedb` reporting an indexed-tree
    /// mismatch for a primary is the signal. The secondary is DERIVED
    /// state, so rebuilding it from the intact primary is the one kind of
    /// local repair that provably reconverges: the recomputed secondaries
    /// restore the canonical H1-A binding, so a node whose root hash
    /// diverged through index damage returns to the network's root.
    ///
    /// The repair is incremental over CONTENT: orphan rows are deleted,
    /// missing rows inserted, and payload-damaged rows rewritten in place.
    /// Calling it when every secondary is already correct is a no-op on
    /// the root hash.
    ///
    /// **Shape caveat.** A Merk root hash commits to the tree's SHAPE,
    /// and shape is a function of operation history, which no content
    /// repair can recover. Reconcile therefore guarantees a
    /// content-correct, `verify_grovedb`-clean index whose H1-A binding
    /// is consistent — and root-hash equality with an undamaged twin
    /// only when the damage was shape-preserving (payload corruption in
    /// place, or row damage whose undo restores the previous rotations,
    /// which covers the common cases the drift tests pin). Damage that
    /// permanently rotated the surviving nodes yields a self-consistent
    /// index whose root may differ from an undamaged peer's; in a
    /// consensus deployment that state still requires a state sync to
    /// reconverge, once state sync supports indexed trees.
    ///
    /// Sum and avg rows carry PAYLOADS (`SumItem` / `ItemWithSumItem`), so
    /// the repair compares row content, not just row keys: a row sitting
    /// at the right sort key with a damaged payload is rewritten.
    ///
    /// **Cost:** `O(n · axes)` where `n` is the number of entries in the
    /// primary. Intended for occasional use (migration, repair). For
    /// maintaining the secondaries in real time, use the dedicated
    /// insert/delete APIs or batches.
    pub fn reconcile_indexed_tree_secondaries<'b, B, P>(
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
            self.reconcile_indexed_tree_secondaries_on_transaction(
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

    fn reconcile_indexed_tree_secondaries_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let (parent_path, indexed_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot reconcile an indexed tree at the root path".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        // 1. Open the primary and confirm it is an indexed primary of any
        //    variant.
        let primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        if !primary_merk.tree_type.is_indexed_primary() {
            return Err(Error::InvalidPath(
                "reconcile_indexed_tree_secondaries requires the path's last segment to be \
                 a ProvableCountIndexedTree, ProvableSumIndexedTree or \
                 ProvableCountProvableSumIndexedTree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }

        // 2. Read the parent element FIRST: its variant determines which
        //    axes exist, and each axis's stored root key is needed to open
        //    that (possibly damaged) secondary.
        let mut parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                parent_path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        let indexed_element = cost_return_on_error!(
            &mut cost,
            Element::get(&parent_merk, indexed_key, true, grove_version).map_err(Error::MerkError)
        );
        let axes: Vec<(IndexAxis, Option<Vec<u8>>)> = match indexed_element.underlying() {
            Element::ProvableCountIndexedTree(_, s, ..) => vec![(IndexAxis::Count, s.clone())],
            Element::ProvableSumIndexedTree(_, s, ..) => vec![(IndexAxis::Sum, s.clone())],
            Element::ProvableCountProvableSumIndexedTree(_, _, _, axes_tlv, _) => {
                let mut out = Vec::with_capacity(axes_tlv.len());
                for (tag, root_key) in axes_tlv {
                    let axis = cost_return_on_error_no_add!(
                        cost,
                        IndexAxis::try_from_tag(*tag).map_err(|e| Error::CorruptedData(format!(
                            "reconcile_indexed_tree_secondaries: invalid axis tag: {e}"
                        )))
                    );
                    out.push((axis, root_key.clone()));
                }
                out
            }
            _ => {
                return Err(Error::CorruptedData(
                    "parent element at the indexed key is not an indexed tree".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };
        // The tightest ceiling across the configured axes (avg prepends a
        // 16-byte sort key against count/sum's 8).
        let max_item_key_len = axes
            .iter()
            .map(|(axis, _)| max_item_key_len_for_axis(*axis))
            .min()
            .expect("every indexed variant carries at least one axis");

        // 3. Walk the primary once, collecting each entry's state — every
        //    axis derives its rows from the same `(count, sum)` pair, and
        //    every axis binds the same primary node commitment.
        let mut all_query = Query::new();
        all_query.insert_all();
        let mut iter =
            KVIterator::new(primary_merk.storage.raw_iter(), &all_query).unwrap_add_cost(&mut cost);
        let mut entries: Vec<(Vec<u8>, IndexedEntryState)> = Vec::new();
        while let Some((key, value_bytes)) = iter.next_kv().unwrap_add_cost(&mut cost) {
            // Reject oversized primary keys before they can drive
            // make_axis_secondary_key to synthesize a secondary key that
            // violates Merk's < 256-byte invariant. The indexed write paths
            // enforce this, but reconcile operates over EXISTING storage
            // which may contain externally-injected oversize keys; fail
            // closed rather than corrupting a secondary.
            if key.len() > max_item_key_len {
                return Err(Error::CorruptedData(format!(
                    "reconcile_indexed_tree_secondaries found a primary key of length {} \
                     bytes which exceeds this tree's per-axis ceiling of {} bytes; refusing \
                     to synthesize an oversize secondary key. The primary at this path was \
                     written by a code path that bypassed the indexed key-length check and \
                     is corrupt — investigate the source before re-running reconcile",
                    key.len(),
                    max_item_key_len
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
            let value_hash = cost_return_on_error!(
                &mut cost,
                primary_merk
                    .get_value_hash(
                        key.as_slice(),
                        true,
                        Some(&Element::value_defined_cost_for_serialized_value),
                        grove_version,
                    )
                    .map_err(|e| Error::CorruptedData(format!(
                        "reading primary node value hash while reconciling secondary: {e}"
                    )))
            );
            let value_hash = cost_return_on_error_no_add!(
                cost,
                value_hash.ok_or_else(|| Error::CorruptedData(format!(
                    "primary entry {} has element bytes but no node value hash",
                    hex::encode(&key)
                )))
            );
            let (count, sum) = element.count_sum_value_or_default();
            entries.push((
                key,
                IndexedEntryState {
                    count,
                    sum,
                    value_hash,
                },
            ));
        }

        // 4. Rebuild each axis's secondary and capture its post-repair
        //    state in the element's canonical axis order.
        let mut per_axis_state: Vec<(u8, grovedb_merk::CryptoHash, Option<Vec<u8>>)> =
            Vec::with_capacity(axes.len());
        for (axis, stored_root_key) in &axes {
            let axis = *axis;
            let mut secondary_merk = cost_return_on_error!(
                &mut cost,
                self.open_indexed_secondary_at_path(
                    path.clone(),
                    axis,
                    stored_root_key.clone(),
                    transaction,
                    Some(batch),
                    grove_version,
                )
            );
            let secondary_tree_type = axis_secondary_tree_type(axis);

            // Desired rows: key, serialized canonical row, and the primary
            // node commitment the row must bind. `BTreeMap`, not a hashed
            // map: the repair loops below iterate it, and a hashed iteration
            // order would build the secondary AVL in a different shape on
            // every run — two operators repairing identical databases must
            // derive identical secondary root hashes, or the H1-A parent
            // bindings would disagree.
            let mut desired: std::collections::BTreeMap<
                Vec<u8>,
                (Vec<u8>, grovedb_merk::CryptoHash),
            > = std::collections::BTreeMap::new();
            for (key, state) in &entries {
                let secondary_key = make_axis_secondary_key(axis, state.count, state.sum, key);
                let row = cost_return_on_error_no_add!(
                    cost,
                    axis_row_reference(axis, key, state.count, state.sum)
                );
                let row_bytes = cost_return_on_error_no_add!(
                    cost,
                    row.serialize(grove_version).map_err(|e| {
                        Error::CorruptedData(format!(
                            "failed to serialize desired secondary row: {e}"
                        ))
                    })
                );
                desired.insert(secondary_key, (row_bytes, state.value_hash));
            }

            // Existing row KEYS, raw-iterated so unlinked-but-present rows
            // are still observed. (The raw value is the Merk NODE encoding —
            // links and child hashes, not element bytes — so payloads are
            // compared via `merk.get` below, which yields the element.)
            let mut all_query_sec = Query::new();
            all_query_sec.insert_all();
            let mut sec_iter = KVIterator::new(secondary_merk.storage.raw_iter(), &all_query_sec)
                .unwrap_add_cost(&mut cost);
            let mut existing_keys: std::collections::BTreeSet<Vec<u8>> =
                std::collections::BTreeSet::new();
            while let Some((key, _node_bytes)) = sec_iter.next_kv().unwrap_add_cost(&mut cost) {
                existing_keys.insert(key);
            }

            // Delete rows that should not exist.
            for key in &existing_keys {
                if !desired.contains_key(key) {
                    cost_return_on_error!(
                        &mut cost,
                        Element::delete(
                            &mut secondary_merk,
                            key.as_slice(),
                            None,
                            false,
                            secondary_tree_type,
                            grove_version,
                        )
                        .map_err(Error::MerkError)
                    );
                }
            }

            // Insert missing rows and rewrite damaged ones. Key-presence
            // alone does not imply row-correctness on any axis: the row
            // carries a reference path and a payload sum the key does not
            // encode, and its COMMITMENT can be stale even when every byte
            // of the row is right. Compare all three.
            for (desired_key, (desired_row_bytes, desired_target_hash)) in &desired {
                let needs_write = if existing_keys.contains(desired_key) {
                    let stored = cost_return_on_error!(
                        &mut cost,
                        secondary_merk
                            .get(
                                desired_key.as_slice(),
                                true,
                                Some(&Element::value_defined_cost_for_serialized_value),
                                grove_version,
                            )
                            .map_err(|e| Error::CorruptedData(format!(
                                "reading secondary row for compare: {e}"
                            )))
                    );
                    let stored_value_hash = cost_return_on_error!(
                        &mut cost,
                        secondary_merk
                            .get_value_hash(
                                desired_key.as_slice(),
                                true,
                                Some(&Element::value_defined_cost_for_serialized_value),
                                grove_version,
                            )
                            .map_err(|e| Error::CorruptedData(format!(
                                "reading secondary row commitment for compare: {e}"
                            )))
                    );
                    let want_committed = grovedb_merk::tree::combine_hash(
                        &grovedb_merk::tree::value_hash(desired_row_bytes)
                            .unwrap_add_cost(&mut cost),
                        desired_target_hash,
                    )
                    .unwrap_add_cost(&mut cost);
                    stored.as_deref() != Some(desired_row_bytes.as_slice())
                        || stored_value_hash != Some(want_committed)
                } else {
                    true
                };
                if needs_write {
                    let entry = cost_return_on_error_no_add!(
                        cost,
                        Element::deserialize(desired_row_bytes.as_slice(), grove_version).map_err(
                            |e| {
                                Error::CorruptedData(format!(
                                    "failed to round-trip desired secondary row: {e}"
                                ))
                            }
                        )
                    );
                    cost_return_on_error!(
                        &mut cost,
                        entry
                            .insert_reference(
                                &mut secondary_merk,
                                desired_key.as_slice(),
                                *desired_target_hash,
                                None,
                                grove_version,
                            )
                            .map_err(Error::MerkError)
                    );
                }
            }

            let (sec_hash, sec_root_key, _) = cost_return_on_error!(
                &mut cost,
                secondary_merk
                    .root_hash_key_and_aggregate_data()
                    .map_err(Error::MerkError)
            );
            per_axis_state.push((axis.tag(), sec_hash, sec_root_key));
        }

        // 5. Snapshot the primary and rebuild the parent's element with the
        //    repaired per-axis state — the lone axis root hash for PCIT /
        //    PSIT, the axes digest for PCPSIT.
        let (primary_root_hash, primary_root_key, primary_aggregate_data) = cost_return_on_error!(
            &mut cost,
            primary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(Error::MerkError)
        );
        let is_multi_axis = matches!(
            indexed_element.underlying(),
            Element::ProvableCountProvableSumIndexedTree(..)
        );
        let (reconstructed, second_hash) = if is_multi_axis {
            let axes_tlv: Vec<(u8, Option<Vec<u8>>)> = per_axis_state
                .iter()
                .map(|(tag, _, root_key)| (*tag, root_key.clone()))
                .collect();
            let axis_hashes: Vec<(u8, grovedb_merk::CryptoHash)> = per_axis_state
                .iter()
                .map(|(tag, hash, _)| (*tag, *hash))
                .collect();
            let digest = grovedb_merk::tree::axes_digest(&axis_hashes).unwrap_add_cost(&mut cost);
            (
                indexed_element.reconstruct_with_axes(
                    primary_root_key,
                    primary_aggregate_data,
                    axes_tlv,
                ),
                digest,
            )
        } else {
            (
                indexed_element.reconstruct_with_two_root_keys(
                    primary_root_key,
                    per_axis_state[0].2.clone(),
                    primary_aggregate_data,
                ),
                per_axis_state[0].1,
            )
        };
        let reconstructed = cost_return_on_error_no_add!(
            cost,
            reconstructed.ok_or(Error::CorruptedCodeExecution(
                "reconstructing the indexed element with repaired root state returned None"
            ))
        );
        cost_return_on_error!(
            &mut cost,
            reconstructed
                .insert_count_indexed_subtree(
                    &mut parent_merk,
                    indexed_key,
                    primary_root_hash,
                    second_hash,
                    None,
                    grove_version,
                )
                .map_err(Error::MerkError)
        );

        // 6. Hand off to shared propagation.
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
    ) -> CostResult<Vec<IndexedAxisEntry<T>>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        grovedb_version::check_grovedb_v0_with_cost!(
            "indexed_axis_top_k_generic",
            grove_version.grovedb_versions.operations.indexed_axis.read
        );
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_validated_axis_secondary(path.clone(), axis, tx_ref, grove_version)
        );

        let rows = cost_return_on_error!(
            &mut cost,
            collect_top_k_via_iterator(&secondary_merk, axis, k, descending, &decode)
        );
        drop(secondary_merk);
        resolve_axis_entries(self, path, rows, transaction, grove_version).add_cost(cost)
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
    ) -> CostResult<IndexedTopKPage<T>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        grovedb_version::check_grovedb_v0_with_cost!(
            "indexed_axis_top_k_paginated_generic",
            grove_version.grovedb_versions.operations.indexed_axis.read
        );
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_validated_axis_secondary(path.clone(), axis, tx_ref, grove_version)
        );

        // `offset == 0` is the overwhelmingly common shape, and the raw
        // iterator is the cheapest way to serve it: one directional seek
        // plus `k` sequential steps, no tree-path loads. It shares the
        // `top_k` core's implementation, so "offset 0 costs exactly what
        // plain top-k costs" is structural, not coincidental. Zero offset
        // skips zero entries, so `skipped = min(0, population) = 0` needs
        // no tree read.
        if offset == 0 {
            let rows = cost_return_on_error!(
                &mut cost,
                collect_top_k_via_iterator(&secondary_merk, axis, k, descending, &decode)
            );
            drop(secondary_merk);
            return resolve_axis_entries(self, path, rows, transaction, grove_version)
                .map_ok(|entries| IndexedTopKPage {
                    entries,
                    skipped: 0,
                })
                .add_cost(cost);
        }
        // The open above serves validation (path shape, element variant,
        // axis compatibility) and the offset-0 fast path only. For the
        // counted read, nothing it loaded is trusted as page data.
        drop(secondary_merk);

        // `offset > 0`: counted skip. NOTHING THE PAGE IS BUILT FROM IS
        // READ OUTSIDE ONE PINNED VIEW: a single raw iterator (implicit
        // RocksDB snapshot at creation, plus the transaction's own
        // uncommitted writes) serves the indexed element's re-read — the
        // authoritative secondary root key — and then, retargeted to the
        // secondary's prefix, the root node and every descent and collect
        // fetch. Discovering the root key outside the view would let a
        // commit that rotates the secondary root between discovery and
        // traversal leave the old root key resolving to a *demoted child*
        // in the newer view: an internally consistent subtree that every
        // count check accepts, silently truncating the page. Re-reading
        // the element inside the view closes that hole.
        let Some((parent_path, indexed_key)) = path.derive_parent() else {
            // Unreachable: the validated open above already rejected the
            // root path.
            return Err(Error::InvalidPath(
                "cannot query an indexed tree at the root path".to_string(),
            ))
            .wrap_with_cost(cost);
        };
        let parent_prefix =
            RocksDbStorage::build_prefix(parent_path.clone()).unwrap_add_cost(&mut cost);
        // Kept for resolving the page's primary values once the counted
        // descent has produced its keys.
        let path_for_resolution = path.clone();
        let primary_prefix = RocksDbStorage::build_prefix(path).unwrap_add_cost(&mut cost);
        let secondary_prefix = RocksDbStorage::secondary_prefix_for(&primary_prefix, axis.tag())
            .unwrap_add_cost(&mut cost);
        let parent_ctx = self
            .db
            .get_transactional_storage_context_by_subtree_prefix(parent_prefix, None, tx_ref)
            .unwrap_add_cost(&mut cost);

        // The pinned view. Created under the parent merk's prefix to read
        // the indexed element, then retargeted to the secondary's prefix
        // for the traversal — same underlying iterator, same snapshot.
        let mut view = parent_ctx.raw_iter();
        let parent_node = match cost_return_on_error!(
            &mut cost,
            snapshot_fetch_node(&mut view, indexed_key, grove_version)
        ) {
            Some(node) => node,
            None => {
                return Err(Error::CorruptedData(
                    "indexed-tree element is absent from the read snapshot — the tree was \
                     removed between validation and read"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };
        let element = cost_return_on_error_no_add!(
            cost,
            Element::deserialize(parent_node.value_as_slice(), grove_version).map_err(|e| {
                Error::CorruptedData(format!("indexed-tree element failed to deserialize: {e}"))
            })
        );
        let secondary_root_key = cost_return_on_error_no_add!(
            cost,
            axis_secondary_root_key_from_element(axis, &element)
        );
        let view = view.retarget(secondary_prefix);

        let (secondary_keys, skipped) = cost_return_on_error!(
            &mut cost,
            counted_skip_page(
                view,
                secondary_root_key,
                offset,
                u64::from(k),
                !descending,
                grove_version
            )
        );
        let mut rows = Vec::with_capacity(secondary_keys.len());
        for secondary_key in secondary_keys {
            match decode(&secondary_key) {
                Some(decoded) => rows.push(decoded),
                None => {
                    return Err(corrupted_secondary_key_error(axis, &secondary_key))
                        .wrap_with_cost(cost);
                }
            }
        }
        resolve_axis_entries(self, path_for_resolution, rows, transaction, grove_version)
            .map_ok(|entries| IndexedTopKPage { entries, skipped })
            .add_cost(cost)
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
    ) -> CostResult<Vec<IndexedAxisEntry<T>>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        grovedb_version::check_grovedb_v0_with_cost!(
            "indexed_axis_range_generic",
            grove_version.grovedb_versions.operations.indexed_axis.read
        );
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_validated_axis_secondary(path.clone(), axis, tx_ref, grove_version)
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
        drop(iter);
        drop(secondary_merk);

        resolve_axis_entries(self, path, results, transaction, grove_version).add_cost(cost)
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
    ) -> CostResult<Vec<IndexedAxisEntry<u64>>, Error>
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
    /// `offset = 0` is equivalent to plain `indexed_count_top_k` and is
    /// served by the same storage-iterator scan. A positive `offset` is
    /// skipped by counted descent over the secondary merk — subtrees are
    /// consumed from their aggregate counts without loading their
    /// entries, so the skip costs `O(log n)` node loads rather than
    /// `O(offset)` iterator steps. The returned
    /// [`IndexedTopKPage::skipped`] is the true skipped count,
    /// `min(offset, population)` — an offset past the end reports the
    /// secondary's population rather than echoing the request. Neither
    /// shape is a verifiable / proof-bounded read; for the provable
    /// variant use [`Self::prove_indexed_count_top_k_paginated`] which
    /// relies on the merk-level count-offset proof to commit the skipped
    /// count via `HashWithCount`.
    pub fn indexed_count_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<IndexedTopKPage<u64>, Error>
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
    ) -> CostResult<Vec<IndexedAxisEntry<u64>>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        // The version gate has to precede the degenerate-range
        // fast path below: returning the empty answer first would
        // leave inverted bounds outside the version contract that
        // every other input to this entry point is held to.
        grovedb_version::check_grovedb_v0_with_cost!(
            "indexed_count_range",
            grove_version.grovedb_versions.operations.indexed_axis.read
        );
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
    /// [`Self::prove_indexed_count_aggregate_over_value_range`] +
    /// [`Self::verify_indexed_count_aggregate_over_value_range`].
    pub fn indexed_count_aggregate_over_value_range<'b, B, P>(
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
        grovedb_version::check_grovedb_v0_with_cost!(
            "indexed_count_aggregate_over_value_range",
            grove_version.grovedb_versions.operations.indexed_axis.read
        );
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
    ) -> CostResult<Vec<IndexedAxisEntry<i64>>, Error>
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
    /// results. `offset = 0` is equivalent to plain `indexed_sum_top_k`;
    /// a positive `offset` is skipped by counted descent over the
    /// secondary merk in `O(log n)` node loads, and
    /// [`IndexedTopKPage::skipped`] reports the true
    /// `min(offset, population)`. Not a verifiable / proof-bounded read.
    pub fn indexed_sum_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<IndexedTopKPage<i64>, Error>
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
    ) -> CostResult<Vec<IndexedAxisEntry<i64>>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        // The version gate has to precede the degenerate-range
        // fast path below: returning the empty answer first would
        // leave inverted bounds outside the version contract that
        // every other input to this entry point is held to.
        grovedb_version::check_grovedb_v0_with_cost!(
            "indexed_sum_range",
            grove_version.grovedb_versions.operations.indexed_axis.read
        );
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
    pub fn indexed_sum_aggregate_over_value_range<'b, B, P>(
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
        grovedb_version::check_grovedb_v0_with_cost!(
            "indexed_sum_aggregate_over_value_range",
            grove_version.grovedb_versions.operations.indexed_axis.read
        );
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

    /// How many entries of the sum axis fall in the inclusive sum band
    /// `[lo_sum, hi_sum]` — the POPULATION of the band, each selected
    /// entry contributing 1 regardless of its value. The
    /// [`AggregateFold::Population`](grovedb_query::AggregateFold)
    /// counterpart of [`Self::indexed_sum_aggregate_over_value_range`],
    /// answered off the sum secondary's count aggregate (the sum
    /// secondary is count-bearing — `ProvableCountProvableSumTree` —
    /// which is also what makes its offset pagination provable).
    ///
    /// `O(log n)`: the walk folds contained subtrees' stored counts and
    /// descends only along the two band boundaries.
    pub fn indexed_sum_population_over_value_range<'b, B, P>(
        &self,
        path: P,
        lo_sum: i64,
        hi_sum: i64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<u64, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        // The version gate precedes the degenerate-range fast path:
        // every input to this entry point is held to the same version
        // contract, inverted bounds included.
        grovedb_version::check_grovedb_v0_with_cost!(
            "indexed_sum_population_over_value_range",
            grove_version.grovedb_versions.operations.indexed_axis.read
        );
        use grovedb_merk::proofs::query::QueryItem as MerkQueryItemForRange;

        let mut cost = OperationCost::default();
        if lo_sum > hi_sum {
            return Ok(0u64).wrap_with_cost(cost);
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

        let population = cost_return_on_error!(
            &mut cost,
            secondary_merk
                .count_aggregate_on_range(&inner_range, grove_version)
                .map_err(|e| Error::CorruptedData(format!("indexed sum population on range: {e}")))
        );

        Ok(population).wrap_with_cost(cost)
    }

    /// The TOTAL of the count values of every entry whose `count_value`
    /// falls in the inclusive band `[lo_count, hi_count]` — the
    /// [`AggregateFold::Total`](grovedb_query::AggregateFold)
    /// counterpart of [`Self::indexed_count_aggregate_over_value_range`]
    /// (which answers the band's POPULATION), enabled by the count
    /// secondary mirroring each entry's `count_value` into its sum half
    /// (issue #806).
    ///
    /// `O(log n)`: the walk folds contained subtrees' stored sums and
    /// descends only along the two band boundaries.
    pub fn indexed_count_total_over_value_range<'b, B, P>(
        &self,
        path: P,
        lo_count: u64,
        hi_count: u64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<i64, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        // The version gate precedes the degenerate-range fast path:
        // every input to this entry point is held to the same version
        // contract, inverted bounds included.
        grovedb_version::check_grovedb_v0_with_cost!(
            "indexed_count_total_over_value_range",
            grove_version.grovedb_versions.operations.indexed_axis.read
        );
        use grovedb_merk::proofs::query::QueryItem as MerkQueryItemForRange;

        let mut cost = OperationCost::default();
        if lo_count > hi_count {
            return Ok(0i64).wrap_with_cost(cost);
        }
        let path: SubtreePath<B> = path.into();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_validated_axis_secondary(path, IndexAxis::Count, tx_ref, grove_version)
        );

        let lo_bytes = encode_count_sort_key(lo_count).to_vec();
        let inner_range = if hi_count == u64::MAX {
            MerkQueryItemForRange::RangeFrom(lo_bytes..)
        } else {
            let upper_bytes = encode_count_sort_key(hi_count + 1).to_vec();
            MerkQueryItemForRange::Range(lo_bytes..upper_bytes)
        };

        let total = cost_return_on_error!(
            &mut cost,
            secondary_merk
                .sum_aggregate_on_range(&inner_range, grove_version)
                .map_err(|e| Error::CorruptedData(format!("indexed count total on range: {e}")))
        );

        Ok(total).wrap_with_cost(cost)
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
    ) -> CostResult<Vec<IndexedAxisEntry<i128>>, Error>
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
    /// results. `offset = 0` is equivalent to plain `indexed_avg_top_k`;
    /// a positive `offset` is skipped by counted descent over the
    /// secondary merk in `O(log n)` node loads, and
    /// [`IndexedTopKPage::skipped`] reports the true
    /// `min(offset, population)`. Not a verifiable / proof-bounded read.
    pub fn indexed_avg_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<IndexedTopKPage<i128>, Error>
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
    /// No `indexed_avg_aggregate_over_value_range` exists — averaging an average
    /// over a range is not a closed-form aggregate. Callers that need
    /// "aggregate avg in range" should compute it client-side from
    /// `indexed_count_aggregate_over_value_range` + `indexed_sum_aggregate_over_value_range`
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
    ) -> CostResult<Vec<IndexedAxisEntry<i128>>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        // The version gate has to precede the degenerate-range
        // fast path below: returning the empty answer first would
        // leave inverted bounds outside the version contract that
        // every other input to this entry point is held to.
        grovedb_version::check_grovedb_v0_with_cost!(
            "indexed_avg_range",
            grove_version.grovedb_versions.operations.indexed_axis.read
        );
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
        axis_secondary_root_key_from_element(axis, &element).wrap_with_cost(cost)
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

/// Apply an axis secondary mirror covering insert, update, and delete via
/// the (old, new) [`IndexedEntryState`] pair.
///
/// The row is [`axis_row_reference`] — a canonical one-hop
/// `ReferenceWithSumItem` back to the primary entry — written as a
/// COMBINED reference so the secondary root binds
/// `combine_hash(H(reference bytes), primary_node_value_hash)`.
pub(crate) fn mirror_indexed_axis_to_secondary<'db, S: StorageContext<'db>>(
    secondary: &mut Merk<S>,
    axis: IndexAxis,
    item_key: &[u8],
    old: Option<IndexedEntryState>,
    new: Option<IndexedEntryState>,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    let secondary_tree_type = axis_secondary_tree_type(axis);

    let row_for = |state: Option<IndexedEntryState>| -> Result<_, Error> {
        state
            .map(|s| {
                Ok((
                    make_axis_secondary_key(axis, s.count, s.sum, item_key),
                    axis_row_reference(axis, item_key, s.count, s.sum)?,
                    s.value_hash,
                ))
            })
            .transpose()
    };
    let old_entry = cost_return_on_error_no_add!(cost, row_for(old));
    let new_entry = cost_return_on_error_no_add!(cost, row_for(new));

    // Fast path: skip only when the sort key, the row bytes AND the bound
    // target hash are all unchanged. The target hash is what makes this
    // strictly narrower than the pre-reference check: a value-only primary
    // update leaves key and row identical while moving the commitment, and
    // skipping it would leave a row authenticating a value that is no
    // longer there.
    if old_entry == new_entry {
        return Ok(()).wrap_with_cost(cost);
    }

    // A row change at a FIXED key must replace in place. Deleting and
    // reinserting the same key rebalances the AVL twice and can settle a
    // DIFFERENT shape than the batch mirror's single replacement write —
    // two write paths committing different secondary (hence grove) root
    // hashes for identical data, which
    // `direct_and_batch_agree_on_root_for_a_fixed_key_avg_payload_change`
    // reproduced on an interior node before this skip existed. The insert
    // below overwrites the value in place, exactly like the batch path's
    // put.
    let old_key = old_entry.as_ref().map(|(k, ..)| k);
    let new_key = new_entry.as_ref().map(|(k, ..)| k);
    if let Some(ok) = old_key
        && Some(ok) != new_key
    {
        let ok = ok.clone();
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
    if let Some((nk, row, target_value_hash)) = new_entry {
        cost_return_on_error!(
            &mut cost,
            row.insert_reference(
                secondary,
                nk.as_slice(),
                target_value_hash,
                None,
                grove_version
            )
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
pub(crate) fn corrupted_secondary_key_error(axis: IndexAxis, secondary_key: &[u8]) -> Error {
    Error::CorruptedData(format!(
        "secondary key in indexed-tree (axis {:?}) is shorter than {} bytes: {:?}",
        axis,
        axis_sort_prefix_len(axis),
        secondary_key
    ))
}

/// Directional top-`k` collect over the secondary's storage iterator.
/// Shared by the `top_k` core and the paginated core's `offset == 0` fast
/// path, so "offset 0 costs exactly what plain top-k costs" is a
/// structural fact rather than two implementations kept in sync.
fn collect_top_k_via_iterator<'db, S: StorageContext<'db>, T>(
    secondary_merk: &Merk<S>,
    axis: IndexAxis,
    k: u16,
    descending: bool,
    decode: &impl Fn(&[u8]) -> Option<(T, Vec<u8>)>,
) -> CostResult<Vec<(T, Vec<u8>)>, Error> {
    let mut cost = OperationCost::default();

    let mut all_query = Query::new();
    all_query.left_to_right = !descending;
    all_query.insert_all();

    let mut iter =
        KVIterator::new(secondary_merk.storage.raw_iter(), &all_query).unwrap_add_cost(&mut cost);

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

/// Turn decoded `(ordering_value, primary_key)` pairs into full entries by
/// reading each primary value.
///
/// Resolution applies ORDINARY GroveDB reference semantics: a
/// reference-shaped primary entry resolves to its terminal, exactly as
/// `db.get` on that key would. That is deliberately not the rule a row is
/// *bound* by — a row commits its immediate primary node, which is what
/// keeps the mirror's invariant local — and the two stay consistent
/// because the immediate node's commitment transitively covers whatever it
/// pointed at when written.
///
/// The primary Merk is opened once for the whole page rather than per row.
fn resolve_axis_entries<'b, B, T>(
    db: &GroveDb,
    indexed_path: SubtreePath<'b, B>,
    rows: Vec<(T, Vec<u8>)>,
    transaction: TransactionArg,
    grove_version: &GroveVersion,
) -> CostResult<Vec<IndexedAxisEntry<T>>, Error>
where
    B: AsRef<[u8]> + 'b,
{
    let mut cost = OperationCost::default();
    if rows.is_empty() {
        return Ok(Vec::new()).wrap_with_cost(cost);
    }

    let tx = TxRef::new(&db.db, transaction);
    let primary_merk = cost_return_on_error!(
        &mut cost,
        db.open_transactional_merk_at_path(indexed_path.clone(), tx.as_ref(), None, grove_version)
    );

    let mut entries = Vec::with_capacity(rows.len());
    for (ordering_value, primary_key) in rows {
        let element = cost_return_on_error!(
            &mut cost,
            Element::get(&primary_merk, &primary_key, true, grove_version).map_err(|e| {
                Error::CorruptedData(format!(
                    "indexed axis read: primary entry {} named by a secondary row is missing: {e}",
                    hex::encode(&primary_key)
                ))
            })
        );
        let value = match element.underlying() {
            Element::Reference(reference_path, ..)
            | Element::ReferenceWithSumItem(reference_path, ..) => {
                // The entry's PARENT path, not its own qualified path: a
                // relative reference resolves against the parent
                // (`SiblingReference` appends its key to what it is
                // given), so passing the entry's own path would look for a
                // child underneath the entry itself.
                let parent_path = indexed_path.to_vec();
                let absolute = match crate::reference_path::path_from_reference_path_type(
                    reference_path.clone(),
                    &parent_path,
                    Some(primary_key.as_slice()),
                ) {
                    Ok(p) => p,
                    Err(e) => return Err(Error::from(e)).wrap_with_cost(cost),
                };
                cost_return_on_error!(
                    &mut cost,
                    db.follow_reference(
                        absolute.as_slice().into(),
                        true,
                        transaction,
                        grove_version
                    )
                )
            }
            _ => element,
        };
        entries.push(IndexedAxisEntry {
            ordering_value,
            primary_key,
            value,
        });
    }

    Ok(entries).wrap_with_cost(cost)
}

/// Strict provable-count read of an aggregate. The counted skip only ever
/// runs against axis secondaries, whose tree types (`ProvableCountTree` /
/// `ProvableCountProvableSumTree`) bind a provable count into every node;
/// any other aggregate shape here is corruption, never a fallback.
/// Deliberately not `AggregateData::as_count_u64`, which returns 0 for
/// non-count variants — silently, which is exactly what this matcher
/// exists to prevent. Mirrors merk's `pub(super)`
/// `provable_count_from_aggregate` (unreachable from here).
#[inline]
fn provable_count_from_aggregate(aggregate: AggregateData) -> Result<u64, Error> {
    match aggregate {
        AggregateData::ProvableCount(c)
        | AggregateData::ProvableCountAndSum(c, _)
        | AggregateData::ProvableCountAndProvableSum(c, _) => Ok(c),
        other => Err(Error::CorruptedData(format!(
            "indexed secondary node carries a non-provable-count aggregate: {:?}",
            other
        ))),
    }
}

/// One page of an `indexed_<axis>_top_k_paginated` read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedTopKPage<T> {
    /// Page entries in directional order, each carrying its resolved
    /// primary value.
    pub entries: Vec<IndexedAxisEntry<T>>,
    /// How many entries the offset actually skipped:
    /// `min(offset, population)`. When the offset runs past the end this
    /// reports the secondary's true population instead of echoing the
    /// request — the same quantity the proved path attests through its
    /// count commitments, though here it is the local tree's unverified
    /// claim, like the entries themselves.
    pub skipped: u64,
}

/// Mutable state threaded through the counted descent.
struct CountedPageState {
    /// In-range entries still to skip before returning starts.
    offset_remaining: u64,
    /// Page slots still to fill. The recursion is only ever entered while
    /// this is non-zero and unwinds the moment it reaches zero.
    limit_remaining: u64,
    /// `true` = ascending (left child first), `false` = descending.
    left_to_right: bool,
}

/// Pure extraction of the per-axis `secondary_root_key` from an
/// indexed-tree element. Shared by the merk-backed reader (which drives
/// every per-axis query API's validation) and by the counted paginated
/// path's pinned-view re-read, so the two agree on axis-compatibility by
/// construction.
fn axis_secondary_root_key_from_element(
    axis: IndexAxis,
    element: &Element,
) -> Result<Option<Vec<u8>>, Error> {
    match (axis, element.underlying()) {
        // PCIT carries a single secondary; only Count axis is valid.
        (IndexAxis::Count, Element::ProvableCountIndexedTree(_, secondary, ..)) => {
            Ok(secondary.clone())
        }
        // PSIT carries a single secondary; only Sum axis is valid.
        (IndexAxis::Sum, Element::ProvableSumIndexedTree(_, secondary, ..)) => {
            Ok(secondary.clone())
        }
        // PCPSIT carries a TLV of 1..=3 axis-tagged secondaries; the
        // requested axis must appear in the TLV.
        (_, Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _)) => {
            let want_tag = axis.tag();
            match axes.iter().find(|(t, _)| *t == want_tag) {
                Some((_, sec)) => Ok(sec.clone()),
                None => Err(Error::InvalidPath(format!(
                    "{:?} axis not indexed at this path",
                    axis
                ))),
            }
        }
        _ => Err(Error::InvalidPath(format!(
            "{:?} axis not indexed at this path",
            axis
        ))),
    }
}

/// Recursion ceiling for the counted descent. An AVL tree cannot exceed
/// 1.44·64 ≈ 93 levels even at the u64 population limit, so anything
/// deeper means a corrupt link structure (e.g. a cyclic link) — fail with
/// an error instead of overflowing the stack.
const COUNTED_SKIP_MAX_DEPTH: u32 = 128;

/// Read a child link's provable count. A present link points at a
/// non-empty subtree, whose count is therefore at least 1 — a zero is
/// corruption, not an empty side (that is `None`).
fn provable_count_from_link(link: Option<&grovedb_merk::tree::Link>) -> Result<u64, Error> {
    match link {
        None => Ok(0),
        Some(link) => match provable_count_from_aggregate(link.aggregate_data())? {
            0 => Err(Error::CorruptedData(
                "secondary link is present but carries aggregate count 0".to_string(),
            )),
            count => Ok(count),
        },
    }
}

/// Serve one page of secondary keys at `offset` in directional order by
/// counted descent over the secondary merk: subtrees whose whole
/// population fits inside the remaining offset are consumed from their
/// parent's link aggregate without being fetched, so the skip costs one
/// root-to-position path instead of one step per skipped entry. Returns
/// the raw secondary keys (`sort_key ‖ item_key`) plus the true skipped
/// count, `min(offset, population)`; values are never needed — the
/// caller decodes keys exactly as the iterator path does.
///
/// **Every node in the page comes from one pinned view.** The caller
/// hands in the raw iterator already carrying the view that the
/// secondary root key was discovered in (retargeted to the secondary's
/// prefix), and the root fetch, descent, and collect all go through it.
/// A RocksDB transaction iterator pins an implicit snapshot of the
/// committed state at creation plus the transaction's own uncommitted
/// writes — the same consistency guarantee the replaced linear scan had
/// from its single `KVIterator`. Independent point-gets through the
/// (snapshotless) transaction would not have it: a commit landing
/// mid-descent could hand back a child from a newer state than its
/// resident parent, and merk's child loads do not verify the child
/// against the parent's recorded link hash, so the result would be a
/// silently mixed page rather than an error.
fn counted_skip_page<I: RawIterator>(
    mut iter: I,
    root_key: Option<Vec<u8>>,
    offset: u64,
    limit: u64,
    left_to_right: bool,
    grove_version: &GroveVersion,
) -> CostResult<(Vec<Vec<u8>>, u64), Error> {
    let mut cost = OperationCost::default();

    let Some(root_key) = root_key else {
        // Empty secondary: nothing to skip, nothing to return.
        return Ok((Vec::new(), 0)).wrap_with_cost(cost);
    };

    // The root key was read from the indexed element inside this same
    // view, so a miss here is corruption, not a race.
    let root = match cost_return_on_error!(
        &mut cost,
        snapshot_fetch_node(&mut iter, &root_key, grove_version)
    ) {
        Some(root) => root,
        None => {
            return Err(Error::CorruptedData(
                "secondary root node named by the indexed element is absent from the same \
                 read snapshot"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }
    };

    let population = cost_return_on_error_no_add!(
        cost,
        root.aggregate_data()
            .map_err(|e| Error::CorruptedData(format!("secondary aggregate_data: {e}")))
            .and_then(provable_count_from_aggregate)
    );
    let skipped = offset.min(population);
    if limit == 0 || offset >= population {
        return Ok((Vec::new(), skipped)).wrap_with_cost(cost);
    }
    let mut state = CountedPageState {
        offset_remaining: offset,
        limit_remaining: limit,
        left_to_right,
    };
    // Pre-allocation is a hint, not a promise: `limit` is caller-supplied
    // and `population` comes from an on-disk aggregate, so an unclamped
    // capacity would let a huge limit (or a forged aggregate) reserve
    // memory the page can never fill. The vector grows past the clamp
    // only by actually being filled, one visited node at a time.
    const PAGE_CAPACITY_CLAMP: usize = 1024;
    let page_len = (population - offset).min(limit) as usize;
    let mut out = Vec::with_capacity(page_len.min(PAGE_CAPACITY_CLAMP));
    cost_return_on_error!(
        &mut cost,
        counted_skip_collect(&mut iter, &root, &mut state, &mut out, 0, grove_version)
    );
    Ok((out, skipped)).wrap_with_cost(cost)
}

/// Fetch and decode one merk node from the pinned iterator view. Returns
/// `Ok(None)` when the key is absent from that view.
fn snapshot_fetch_node<I: RawIterator>(
    iter: &mut I,
    key: &[u8],
    grove_version: &GroveVersion,
) -> CostResult<Option<TreeNode>, Error> {
    let mut cost = OperationCost::default();
    iter.seek(key).unwrap_add_cost(&mut cost);
    match iter.key().unwrap_add_cost(&mut cost) {
        Some(found) if found == key => {}
        _ => return Ok(None).wrap_with_cost(cost),
    }
    let Some(bytes) = iter.value().unwrap_add_cost(&mut cost) else {
        return Ok(None).wrap_with_cost(cost);
    };
    TreeNode::decode(
        key.to_vec(),
        bytes,
        None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
        grove_version,
    )
    .map(Some)
    .map_err(|e| Error::CorruptedData(format!("secondary node failed to decode: {e}")))
    .wrap_with_cost(cost)
}

/// Recursive counted descent over nodes served by the pinned iterator.
/// Entered only on subtrees whose population exceeds the remaining
/// offset (the caller and both descend sites guarantee it) and only
/// while the page has room.
fn counted_skip_collect<I: RawIterator>(
    iter: &mut I,
    node: &TreeNode,
    state: &mut CountedPageState,
    out: &mut Vec<Vec<u8>>,
    depth: u32,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();

    if depth > COUNTED_SKIP_MAX_DEPTH {
        return Err(Error::CorruptedData(format!(
            "secondary tree exceeds the maximum plausible depth {COUNTED_SKIP_MAX_DEPTH} — link \
             structure is corrupt"
        )))
        .wrap_with_cost(cost);
    }

    let node_count = cost_return_on_error_no_add!(
        cost,
        node.aggregate_data()
            .map_err(|e| Error::CorruptedData(format!("secondary aggregate_data: {e}")))
            .and_then(provable_count_from_aggregate)
    );
    let left_count = cost_return_on_error_no_add!(cost, provable_count_from_link(node.link(true)));
    let right_count =
        cost_return_on_error_no_add!(cost, provable_count_from_link(node.link(false)));

    // Every valid secondary row contributes structural count exactly 1
    // (`mirror_indexed_axis_to_secondary` writes nothing else, and
    // `verify_indexed_axis_content` enforces it). This is a payload check
    // on the node's own count value — a link whose cached count disagrees
    // with its child's real subtree is caught separately, by the
    // cross-check in `counted_skip_descend`.
    let own_count = node_count
        .checked_sub(left_count)
        .and_then(|n| n.checked_sub(right_count));
    if own_count != Some(1) {
        return Err(Error::CorruptedData(format!(
            "indexed secondary node must have own structural count 1: aggregate {} with child \
             counts {} + {}",
            node_count, left_count, right_count
        )))
        .wrap_with_cost(cost);
    }

    let (first_is_left, first_count, second_count) = if state.left_to_right {
        (true, left_count, right_count)
    } else {
        (false, right_count, left_count)
    };

    // First child in directional order: consumed wholesale from the link
    // aggregate (no fetch) when its entire population fits inside the
    // remaining offset; descended into otherwise.
    if first_count > 0 {
        if first_count <= state.offset_remaining {
            state.offset_remaining -= first_count;
        } else {
            cost_return_on_error!(
                &mut cost,
                counted_skip_descend(
                    iter,
                    node,
                    first_is_left,
                    first_count,
                    state,
                    out,
                    depth,
                    grove_version
                )
            );
            if state.limit_remaining == 0 {
                return Ok(()).wrap_with_cost(cost);
            }
        }
    }

    // The node itself: burn one unit of offset, or emit its key. The
    // entry invariant (population > offset, page not full) makes the
    // emit branch safe without re-checking `limit_remaining`.
    if state.offset_remaining > 0 {
        state.offset_remaining -= 1;
    } else {
        out.push(node.key().to_vec());
        state.limit_remaining -= 1;
        if state.limit_remaining == 0 {
            return Ok(()).wrap_with_cost(cost);
        }
    }

    // Second child. With consistent aggregates the offset can never
    // swallow it whole (this frame was entered because its subtree
    // outlasts the offset), but the count arithmetic keeps the skip
    // branch as the safe symmetric action.
    if second_count > 0 {
        if second_count <= state.offset_remaining {
            state.offset_remaining -= second_count;
        } else {
            cost_return_on_error!(
                &mut cost,
                counted_skip_descend(
                    iter,
                    node,
                    !first_is_left,
                    second_count,
                    state,
                    out,
                    depth,
                    grove_version
                )
            );
        }
    }

    Ok(()).wrap_with_cost(cost)
}

/// Fetch one child from the pinned view and recurse into it.
///
/// `link_count` is the aggregate count read off the parent's link — the
/// number that authorized this descent (and that whole-subtree skips
/// trust without fetching). It is cross-checked against the loaded
/// child's own aggregate, so a link whose cached count disagrees with
/// its subtree fails loud instead of shifting every position after it.
#[allow(clippy::too_many_arguments)]
fn counted_skip_descend<I: RawIterator>(
    iter: &mut I,
    parent: &TreeNode,
    left: bool,
    link_count: u64,
    state: &mut CountedPageState,
    out: &mut Vec<Vec<u8>>,
    depth: u32,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    let child_key = match parent.link(left) {
        Some(link) => link.key().to_vec(),
        None => {
            // The caller only descends where the link (and its non-zero
            // count) was just read, so this is unreachable short of a
            // logic error — fail loud regardless.
            return Err(Error::CorruptedData(
                "secondary descend without a link".to_string(),
            ))
            .wrap_with_cost(cost);
        }
    };
    let child = match cost_return_on_error!(
        &mut cost,
        snapshot_fetch_node(iter, &child_key, grove_version)
    ) {
        Some(child) => child,
        None => {
            // The parent's link names a key the pinned view does not
            // contain: corruption (or a view predating the parent, which
            // a single snapshot rules out).
            return Err(Error::CorruptedData(
                "secondary link is present but its child is absent from the read snapshot"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }
    };
    let child_count = cost_return_on_error_no_add!(
        cost,
        child
            .aggregate_data()
            .map_err(|e| Error::CorruptedData(format!("secondary aggregate_data: {e}")))
            .and_then(provable_count_from_aggregate)
    );
    if child_count != link_count {
        return Err(Error::CorruptedData(format!(
            "secondary link claims aggregate count {link_count} but its subtree carries \
             {child_count}"
        )))
        .wrap_with_cost(cost);
    }
    counted_skip_collect(iter, &child, state, out, depth + 1, grove_version).add_cost(cost)
}

#[cfg(test)]
impl GroveDb {
    /// The pre-counted-skip linear implementation, specialized to the
    /// Count axis (the generic `decode`/`axis` parameters are the only
    /// change from the replaced code). Kept as the measurement baseline
    /// for `indexed_axis_paginated_cost_tests::measure_paginated_costs`;
    /// compiled only for tests, never reachable in production builds.
    ///
    /// Returns bare `(count, key)` pairs, NOT resolved entries: it exists
    /// to measure what the SECONDARY traversal costs, and resolving
    /// primary values would fold an unrelated cost into the baseline.
    pub(crate) fn legacy_linear_indexed_count_top_k_paginated<'b, B, P>(
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
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_validated_axis_secondary(
                path.into(),
                IndexAxis::Count,
                tx_ref,
                grove_version
            )
        );

        let mut all_query = Query::new();
        all_query.left_to_right = !descending;
        all_query.insert_all();

        let mut iter = KVIterator::new(secondary_merk.storage.raw_iter(), &all_query)
            .unwrap_add_cost(&mut cost);

        let mut skipped: u64 = 0;
        while skipped < offset {
            match iter.next_kv().unwrap_add_cost(&mut cost) {
                Some((secondary_key, _)) => {
                    if decode_secondary_key(&secondary_key).is_none() {
                        return Err(corrupted_secondary_key_error(
                            IndexAxis::Count,
                            &secondary_key,
                        ))
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
                Some((secondary_key, _)) => match decode_secondary_key(&secondary_key) {
                    Some(decoded) => results.push(decoded),
                    None => {
                        return Err(corrupted_secondary_key_error(
                            IndexAxis::Count,
                            &secondary_key,
                        ))
                        .wrap_with_cost(cost);
                    }
                },
                None => break,
            }
        }

        Ok(results).wrap_with_cost(cost)
    }
}

#[cfg(test)]
mod axis_row_reference_tests {
    //! The row function is THE definition every writer and checker
    //! shares; this grid pins its output per (axis, count, sum) so any
    //! change to the shape is a deliberate, reviewed event — the bytes
    //! land in hash-committed state, so an accidental change here means
    //! mirrors and checkers disagree about healthy databases.

    use grovedb_element::{indexed::IndexAxis, reference_path::ReferencePathType};
    use grovedb_version::version::GroveVersion;

    use super::{axis_row_reference, decode_axis_row_reference};
    use crate::operations::proof::indexed_axis::canonical_row::{
        axis_payload_sum, INDEXED_SECONDARY_MAX_HOP,
    };
    use crate::Element;

    fn sibling(key: &[u8], sum: i64) -> Element {
        Element::new_reference_with_sum_item_with_hops(
            ReferencePathType::SiblingReference(key.to_vec()),
            INDEXED_SECONDARY_MAX_HOP,
            sum,
        )
    }

    #[test]
    fn row_grid_is_pinned_per_axis() {
        let counts = [0u64, 1, 2, i64::MAX as u64];
        let sums = [i64::MIN, -1, 0, 1, i64::MAX];
        let key = b"item".as_slice();
        for &count in &counts {
            for &sum in &sums {
                assert_eq!(
                    axis_row_reference(IndexAxis::Count, key, count, sum).unwrap(),
                    sibling(key, count as i64),
                    "count axis carries the COUNT as its payload sum; the sum input is ignored"
                );
                assert_eq!(
                    axis_row_reference(IndexAxis::Sum, key, count, sum).unwrap(),
                    sibling(key, sum),
                    "sum axis carries the sum; the count input is ignored"
                );
                assert_eq!(
                    axis_row_reference(IndexAxis::Avg, key, count, sum).unwrap(),
                    sibling(key, sum),
                    "avg axis carries the sum, exactly as the sum axis does"
                );
            }
        }
        // Above the sum-item domain the count axis fails closed.
        axis_row_reference(IndexAxis::Count, key, i64::MAX as u64 + 1, 0)
            .expect_err("count above i64::MAX must fail closed");
        axis_payload_sum(IndexAxis::Count, i64::MAX as u64 + 1, 0)
            .expect_err("count above i64::MAX must fail closed");
    }

    #[test]
    fn every_axis_uses_one_canonical_element_family() {
        // Locked decision 2: one element family across all three axes. A
        // plain `Reference` on the count axis would fold to (1, 0) in a
        // PCPS secondary and silently zero every band Total (#806), and a
        // single-aggregate secondary would reopen the #809 finding-C proof
        // relabeling. Both regressions start by this assertion failing.
        for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            let row = axis_row_reference(axis, b"k", 3, 5).unwrap();
            assert!(
                matches!(row, Element::ReferenceWithSumItem(..)),
                "{axis:?} row must be ReferenceWithSumItem, got {}",
                row.type_str()
            );
            assert_eq!(
                super::axis_secondary_tree_type(axis),
                grovedb_merk::TreeType::ProvableCountProvableSumTree,
                "{axis:?} secondary must stay dual-aggregate"
            );
        }
        // The count axis's payload sum is the count, not the primary sum.
        assert_eq!(axis_payload_sum(IndexAxis::Count, 3, 5).unwrap(), 3);
        assert_eq!(axis_payload_sum(IndexAxis::Sum, 3, 5).unwrap(), 5);
        assert_eq!(axis_payload_sum(IndexAxis::Avg, 3, 5).unwrap(), 5);
    }

    #[test]
    fn decode_round_trips_and_rejects_non_canonical_rows() {
        let row = axis_row_reference(IndexAxis::Sum, b"target", 1, 42).unwrap();
        assert_eq!(
            decode_axis_row_reference(&row, "test").unwrap(),
            (b"target".as_slice(), 42)
        );
        // Suffix agreement is now enforced by the verifier rebuilding the
        // canonical row from the authenticated primary value, so it has no
        // separate helper to unit-test here.

        // Legacy placeholder rows must be rejected outright.
        for legacy in [
            Element::new_sum_item(7),
            Element::new_item_with_sum_item(Vec::new(), 7),
            Element::new_item(Vec::new()),
        ] {
            decode_axis_row_reference(&legacy, "test")
                .expect_err("legacy placeholder rows are not valid indexed rows");
        }
        // A plain `Reference` folds to (1, 0) in a PCPS secondary — it must
        // not be accepted as a canonical row.
        decode_axis_row_reference(
            &Element::new_reference(ReferencePathType::SiblingReference(b"target".to_vec())),
            "test",
        )
        .expect_err("a plain Reference carries no payload sum and is not canonical");
        // Non-sibling reference types would make row size grow with grove
        // depth and break the logical-origin rule.
        decode_axis_row_reference(
            &Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::AbsolutePathReference(vec![b"a".to_vec()]),
                INDEXED_SECONDARY_MAX_HOP,
                7,
            ),
            "test",
        )
        .expect_err("only SiblingReference is canonical");
        // Wrong hop budget: the binding rule is one hop to the immediate
        // primary node, and a different budget means a different binding.
        decode_axis_row_reference(
            &Element::new_reference_with_sum_item_with_hops(
                ReferencePathType::SiblingReference(b"target".to_vec()),
                Some(2),
                7,
            ),
            "test",
        )
        .expect_err("canonical rows are one-hop");
        decode_axis_row_reference(
            &Element::new_reference_with_sum_item(
                ReferencePathType::SiblingReference(b"target".to_vec()),
                7,
            ),
            "test",
        )
        .expect_err("an unbounded hop budget is not canonical");
    }

    #[test]
    fn serialized_bytes_are_stable() {
        // The exact bytes the mirror hash-commits, pinned as FIXED
        // vectors — not re-serialized at assertion time, so a
        // serialization-format change cannot move both sides of the
        // comparison and slip through. If this test fails, row bytes in
        // authenticated state have changed: that is a consensus event,
        // not a refactor.
        let grove_version = GroveVersion::latest();
        assert_eq!(
            axis_row_reference(IndexAxis::Count, b"k", 7, 0)
                .unwrap()
                .serialize(grove_version)
                .unwrap(),
            vec![18, 6, 1, 107, 1, 1, 14, 0],
            "count axis: ReferenceWithSumItem(SiblingReference('k'), hop 1, sum 7)"
        );
        assert_eq!(
            axis_row_reference(IndexAxis::Sum, b"k", 1, -3)
                .unwrap()
                .serialize(grove_version)
                .unwrap(),
            vec![18, 6, 1, 107, 1, 1, 5, 0],
            "sum axis: ReferenceWithSumItem(SiblingReference('k'), hop 1, sum -3)"
        );
        assert_eq!(
            axis_row_reference(IndexAxis::Avg, b"k", 1, 5)
                .unwrap()
                .serialize(grove_version)
                .unwrap(),
            vec![18, 6, 1, 107, 1, 1, 10, 0],
            "avg axis: ReferenceWithSumItem(SiblingReference('k'), hop 1, sum 5)"
        );
    }
}

#[cfg(test)]
mod count_value_as_sum_tests {
    //! The count-axis secondary stores count_value as an i64 sum item;
    //! the conversion FAILS CLOSED above i64::MAX rather than clamping,
    //! because a clamped value would flow into hash-bound authenticated
    //! state as a silently wrong total.

    use crate::operations::proof::indexed_axis::canonical_row::count_value_as_sum;

    #[test]
    fn converts_in_domain_and_fails_closed_above_i64_max() {
        assert_eq!(count_value_as_sum(0).unwrap(), 0);
        assert_eq!(count_value_as_sum(8).unwrap(), 8);
        assert_eq!(count_value_as_sum(i64::MAX as u64).unwrap(), i64::MAX);
        let err = count_value_as_sum(i64::MAX as u64 + 1)
            .expect_err("one past i64::MAX must fail closed");
        assert!(err.to_string().contains("cannot be mirrored"), "{err}");
        count_value_as_sum(u64::MAX).expect_err("u64::MAX must fail closed");
    }
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
        make_axis_secondary_key, max_item_key_len_for_axis, MAX_AVG_INDEXED_ITEM_KEY_LEN,
        MAX_CIDX_ITEM_KEY_LEN,
    };
    use crate::Error;

    #[test]
    fn count_secondary_keys_round_trip_and_sort_by_count() {
        let key = make_axis_secondary_key(IndexAxis::Count, 258, 0, b"row");
        assert_eq!(
            key,
            vec![0, 0, 0, 0, 0, 0, 1, 2, b'r', b'o', b'w'],
            "count prefix must be 8-byte big-endian followed by the item key"
        );
        assert_eq!(
            decode_secondary_key(&key),
            Some((258u64, b"row".to_vec())),
            "decode must invert the count-axis key builder"
        );
        // Big-endian is what makes byte order equal count order.
        assert!(
            make_axis_secondary_key(IndexAxis::Count, 2, 0, b"a")
                < make_axis_secondary_key(IndexAxis::Count, 10, 0, b"a")
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
mod direct_axis_mirror_tests {
    //! Direct-drive tests for `mirror_indexed_axis_to_secondary`.
    //!
    //! Two regression families live here:
    //!
    //! **BUG 2** — the mirror must not early-return on the Avg axis when the
    //! sort key is unchanged but the carried sum differs. The avg sort key is
    //! `floor(sum * 10^19 / count)` while the row carries the raw `sum`, so
    //! `(1, 5)` and `(2, 10)` share a key yet carry sums `5` and `10`. The old
    //! key-only early-return left the stale hash-committed `5` behind.
    //!
    //! **Commitment refresh** — a canonical row binds the primary node's
    //! committed value hash, so a transition whose key AND carried sum are
    //! both unchanged still has to rewrite the row when that hash moves. This
    //! is the case a `(count, sum)`-only mirror is structurally blind to, and
    //! it is reachable in production through a value-only update, a deep
    //! mutation that changes a child subtree's root, and a `RefreshReference`
    //! on a reference-shaped primary.
    //!
    //! Both drive the module-private mirror directly against a real secondary
    //! Merk: the transitions involved are not all reachable through the
    //! public dedicated APIs (each child contributes count 0 or 1, so no
    //! public path produces a `(1, 5) -> (2, 10)` move for one item key).

    use grovedb_costs::OperationCost;
    use grovedb_element::indexed::{compute_avg_fixed_point, IndexAxis};
    use grovedb_merk::{
        element::{costs::ElementCostExtensions, get::ElementFetchFromStorageExtensions},
        tree::AggregateData,
    };
    use grovedb_path::SubtreePath;
    use grovedb_storage::StorageBatch;
    use grovedb_version::version::GroveVersion;

    use super::{
        axis_row_reference, make_axis_secondary_key, mirror_indexed_axis_to_secondary,
        IndexedEntryState,
    };
    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb,
    };

    /// A distinct stand-in commitment per byte, so a test can move the bound
    /// target hash without needing a real primary entry behind it.
    fn target_hash(seed: u8) -> grovedb_merk::CryptoHash {
        [seed; 32]
    }

    fn state(count: u64, sum: i64, seed: u8) -> IndexedEntryState {
        IndexedEntryState {
            count,
            sum,
            value_hash: target_hash(seed),
        }
    }

    /// Set up a PCPSIT with a single configured axis and hand back an open,
    /// empty secondary Merk for it plus the pieces that must outlive it.
    fn open_axis_secondary<'db>(
        db: &'db GroveDb,
        axis: IndexAxis,
        grove_version: &GroveVersion,
    ) -> (crate::Transaction<'db>, StorageBatch) {
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(axis.tag(), None)];
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
        (db.start_transaction(), StorageBatch::new())
    }

    /// Sanity: `(1, 5)` and `(2, 10)` share an avg sort key but produce
    /// different carried sums — the precondition that made the old key-only
    /// early-return unsound.
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
        // The rows differ in their carried sum — the hash-committed value the
        // mirror stores.
        assert_ne!(
            axis_row_reference(IndexAxis::Avg, item_key, 1, 5).unwrap(),
            axis_row_reference(IndexAxis::Avg, item_key, 2, 10).unwrap(),
            "carried sums 5 and 10 must produce different rows"
        );
    }

    /// Drive the mirror directly: first write `(1, 5)`, then transition to
    /// `(2, 10)` (same avg key). The stored carried sum must update to `10`;
    /// the old key-only early-return would have left the stale `5`.
    #[test]
    fn avg_axis_mirror_updates_stale_payload_when_key_unchanged() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let (tx, batch) = open_axis_secondary(&db, IndexAxis::Avg, grove_version);

        let item_key = b"row";
        let shared_key = make_axis_secondary_key(IndexAxis::Avg, 1, 5, item_key);
        assert_eq!(
            shared_key,
            make_axis_secondary_key(IndexAxis::Avg, 2, 10, item_key),
            "test setup: keys must collide"
        );

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
            Some(state(1, 5, 0xAA)),
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
            "carried sum after (1,5) must be 5"
        );

        mirror_indexed_axis_to_secondary(
            &mut secondary,
            IndexAxis::Avg,
            item_key,
            Some(state(1, 5, 0xAA)),
            Some(state(2, 10, 0xBB)),
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
            "carried sum must update to 10 even though the avg sort key did not \
             move (BUG 2 regression: old key-only early-return left stale 5)"
        );
    }

    /// The commitment-refresh case: key unchanged, carried sum unchanged, but
    /// the primary node's value hash moved. The row bytes are identical, so
    /// only the stored value hash can show the difference — and it must.
    ///
    /// This is the transition a `(count, sum)`-only mirror cannot see, and it
    /// is exactly what a value-only primary update produces.
    #[test]
    fn mirror_refreshes_the_row_when_only_the_target_hash_moves() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let (tx, batch) = open_axis_secondary(&db, IndexAxis::Count, grove_version);

        let item_key = b"row";
        let path_segments: [&[u8]; 2] = [TEST_LEAF, b"pcpsit".as_ref()];
        let path: SubtreePath<_> = (&path_segments).into();
        let mut secondary = db
            .open_indexed_secondary_at_path(
                path,
                IndexAxis::Count,
                None,
                &tx,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("open empty count secondary");

        let key = make_axis_secondary_key(IndexAxis::Count, 1, 0, item_key);

        mirror_indexed_axis_to_secondary(
            &mut secondary,
            IndexAxis::Count,
            item_key,
            None,
            Some(state(1, 0, 0x11)),
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let (root_before, ..) = secondary
            .root_hash_key_and_aggregate_data()
            .unwrap()
            .expect("secondary root state");
        let committed_before = secondary
            .get_value_hash(
                key.as_slice(),
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .unwrap()
            .expect("value hash read")
            .expect("row present");

        // Same count, same sum, same row bytes — only the bound commitment
        // moves, as it does when a primary entry's value changes without
        // touching its aggregates.
        mirror_indexed_axis_to_secondary(
            &mut secondary,
            IndexAxis::Count,
            item_key,
            Some(state(1, 0, 0x11)),
            Some(state(1, 0, 0x22)),
            grove_version,
        )
        .unwrap()
        .expect("refresh");

        let committed_after = secondary
            .get_value_hash(
                key.as_slice(),
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .unwrap()
            .expect("value hash read")
            .expect("row present");
        let (root_after, ..) = secondary
            .root_hash_key_and_aggregate_data()
            .unwrap()
            .expect("secondary root state");

        assert_ne!(
            committed_before, committed_after,
            "a moved target hash must move the row's committed value hash — \
             otherwise the row still authenticates a value that is no longer there"
        );
        assert_ne!(
            root_before, root_after,
            "the refreshed commitment must reach the secondary root, or the \
             staleness never becomes visible to a proof"
        );
        // The row BYTES are unchanged: this drift is invisible to any check
        // that compares serialized rows alone.
        let row = Element::get(&secondary, key.as_slice(), true, grove_version)
            .unwrap()
            .expect("row present");
        assert_eq!(
            row,
            axis_row_reference(IndexAxis::Count, item_key, 1, 0).unwrap(),
            "the row bytes must be the canonical ones, unchanged by the refresh"
        );
    }

    /// The fast path must still short-circuit for a genuine no-op — key,
    /// carried sum AND bound target hash all unchanged.
    #[test]
    fn avg_axis_mirror_noop_when_key_payload_and_target_unchanged() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let (tx, batch) = open_axis_secondary(&db, IndexAxis::Avg, grove_version);

        let item_key = b"row";
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
            Some(state(2, 10, 0x33)),
            grove_version,
        )
        .unwrap()
        .expect("insert (2,10)");

        // Identical transition: key, row AND target hash unchanged.
        // Assert on the COST, not just the stored value — a delete+reinsert
        // would leave the same value behind, so a value-only assertion passes
        // even with the fast path deleted entirely.
        let mut noop_cost = OperationCost::default();
        mirror_indexed_axis_to_secondary(
            &mut secondary,
            IndexAxis::Avg,
            item_key,
            Some(state(2, 10, 0x33)),
            Some(state(2, 10, 0x33)),
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
        assert_eq!(
            entry.sum_value_or_default(),
            10,
            "carried sum must remain 10"
        );
    }

    /// A delete reaches the mirror as a `None` new state, which must resolve
    /// to "no new key" and leave only the removal. The row has to disappear
    /// from the axis secondary — not merely stop being findable — or the
    /// secondary's own aggregate keeps counting it.
    #[test]
    fn avg_axis_mirror_removes_the_row_when_the_new_state_is_absent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let (tx, batch) = open_axis_secondary(&db, IndexAxis::Avg, grove_version);

        let item_key = b"row";
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
            Some(state(2, 10, 0x44)),
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
            Some(state(2, 10, 0x44)),
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
