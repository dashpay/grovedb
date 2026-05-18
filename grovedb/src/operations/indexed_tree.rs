//! Direct operations for indexed-tree elements (`ProvableCountIndexedTree`,
//! `ProvableSumIndexedTree`, `ProvableCountProvableSumIndexedTree`).
//!
//! These dedicated APIs handle the two-Merk machinery for the single-axis
//! variants (primary + one count-/sum-ordered secondary) and the
//! multi-axis PCPSIT (primary + 1..=3 axis-specific secondaries). Direct
//! mutations against an indexed-tree primary must go through these APIs
//! (`insert_into_indexed_tree`, `delete_from_indexed_tree`); the
//! level-by-level batch path fails closed for direct indexed-primary
//! mutations until full batch integration lands. Deep ops *under* a
//! sub-tree of a cidx primary propagate correctly through the existing
//! `propagate_changes_with_transaction_with_initial_deferred` machinery.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::indexed::{
    encode_avg_sort_key, encode_count_sort_key, encode_sum_sort_key, IndexAxis,
};
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, decode::ElementDecodeExtensions,
        delete::ElementDeleteFromStorageExtensions, get::ElementFetchFromStorageExtensions,
        insert::ElementInsertToStorageExtensions, reconstruct::ElementReconstructExtensions,
        ElementExt,
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
fn axis_secondary_tree_type(axis: IndexAxis) -> TreeType {
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
fn make_axis_secondary_key(axis: IndexAxis, count: u64, sum: i64, item_key: &[u8]) -> Vec<u8> {
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

    /// Backward-compatible alias for callers still using the
    /// count-only entrypoint. Forwards to [`open_indexed_secondary_at_path`]
    /// with `IndexAxis::Count`.
    pub(crate) fn open_count_indexed_secondary_at_path<'db, 'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        secondary_root_key: Option<Vec<u8>>,
        tx: &'db Transaction,
        batch: Option<&'db StorageBatch>,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.open_indexed_secondary_at_path(
            path,
            IndexAxis::Count,
            secondary_root_key,
            tx,
            batch,
            grove_version,
        )
    }

    /// Helper used by the batch path: open the secondary Merk for the cidx
    /// primary at `path`. Reads the parent merk's cidx element to discover
    /// the secondary's current root_key, then opens the secondary at the
    /// derived prefix sharing the supplied storage batch and transaction.
    pub(crate) fn open_count_indexed_secondary_for_batch<'db, 'b, B>(
        &'db self,
        path: SubtreePath<'b, B>,
        batch: &'db StorageBatch,
        tx: &'db Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        let mut cost = OperationCost::default();
        let (parent_path, cidx_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot open cidx secondary at root path".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };
        let parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(parent_path, tx, Some(batch), grove_version)
        );
        let element = cost_return_on_error!(
            &mut cost,
            Element::get(&parent_merk, cidx_key, true, grove_version).map_err(Error::MerkError)
        );
        let secondary_root_key = match element.underlying() {
            Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
            _ => {
                return Err(Error::CorruptedData(
                    "open_count_indexed_secondary_for_batch: parent element is not a \
                     ProvableCountIndexedTree"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };
        self.open_count_indexed_secondary_at_path(
            path,
            secondary_root_key,
            tx,
            Some(batch),
            grove_version,
        )
        .add_cost(cost)
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
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        cost_return_on_error!(
            &mut cost,
            self.insert_into_count_indexed_tree_on_transaction(
                path,
                item_key,
                item,
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

    pub(crate) fn insert_into_count_indexed_tree_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        item_key: &[u8],
        item: Element,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        // Bound the item key length so the derived secondary key
        // (count_be ‖ item_key) stays under Merk's 256-byte limit.
        cost_return_on_error_no_add!(cost, validate_cidx_item_key_len(item_key));

        let (parent_path, count_indexed_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot insert into count-indexed tree at the root path".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        // 1. Open primary at path. Path resolution reads the parent's
        //    CountIndexedTree element to get primary_root_key.
        let mut primary_merk = cost_return_on_error!(
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
                "insert_into_count_indexed_tree requires the path's last segment to be a \
                 CountIndexedTree or ProvableCountIndexedTree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }
        let is_provable_primary =
            matches!(primary_merk.tree_type, TreeType::ProvableCountIndexedTree);

        // 2. Open the parent merk and read the CountIndexedTree element so
        //    we know the secondary's current root_key.
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

        // 3. Read existing primary entry at item_key (if any) for the count
        //    delta the secondary mirror needs.
        let existing_item = cost_return_on_error!(
            &mut cost,
            Element::get_optional_from_storage(&primary_merk.storage, item_key, grove_version)
                .map_err(Error::MerkError)
        );
        let old_count_for_secondary = existing_item.as_ref().map(|e| e.count_value_or_default());
        let new_count_for_secondary = item.count_value_or_default();

        // 3a. Tree-overwrite cleanup.
        //
        // If the existing entry at `item_key` is a tree (Tree, SumTree,
        // CountTree, cidx, etc.), replacing it with a new element
        // would orphan the old tree's child storage:
        //   - Replace existing tree with non-tree (Item/Ref): old
        //     children stay in storage but are unreachable.
        //   - Replace existing tree with empty new tree (root_key=None):
        //     same orphan; new tree starts fresh while old data stays
        //     at the same storage prefix.
        //   - Replace existing cidx with anything: the secondary
        //     namespace at Blake3(prefix ‖ 0x01) also needs cleanup.
        //
        // For cidx-overwrite specifically, the batch path uses the
        // same SAFE-SUBSET semantics: cidx → non-empty cidx is
        // rejected because the new element's root_keys could refer
        // to on-disk data while the cleanup pass also clears it.
        //
        // Apply identical semantics here:
        //   - Existing is tree, new is non-tree OR empty tree (or
        //     non-cidx tree): ALLOW + cleanup
        //   - Existing is cidx, new is non-empty cidx: REJECT
        // UNCONDITIONAL new-element validation.
        //
        // This API short-circuits to NULL_HASH child roots when
        // inserting any tree-typed element (the new entry's content
        // is owned by callers using insert_into_count_indexed_tree
        // recursively to populate; cidx is the dedicated path). A
        // non-empty tree or cidx in `item` would claim on-disk data
        // (root_keys / count > 0) that we then DON'T validate against
        // — the merk write uses NULL_HASH regardless. The result is a
        // serialized element whose stored root_keys/count don't match
        // the actual child-merk root hashes.
        //
        // Generic db.insert validates non-empty cidx claims by opening
        // the existing primary/secondary Merks and comparing root
        // keys (see grovedb/src/operations/insert/mod.rs). The cidx
        // dedicated API doesn't perform that on-disk read because it
        // creates the cidx fresh — so reject non-empty claims here
        // regardless of whether anything currently exists at item_key.
        //
        // Applies to BRAND-NEW keys, replacements of non-tree keys
        // (e.g., Item → Tree(Some)), and overwrites of trees alike.
        match item.underlying() {
            Element::ProvableCountIndexedTree(p, s, c, _)
                if p.is_some() || s.is_some() || *c != 0 =>
            {
                return Err(Error::NotSupported(
                    "insert_into_count_indexed_tree only accepts an EMPTY cidx \
                     element (primary_root_key = None, secondary_root_key = None, \
                     count_value = 0). Non-empty cidx claims must use generic \
                     db.insert which validates root keys against on-disk state, \
                     or insert via this API then populate with subsequent \
                     insert_into_count_indexed_tree calls"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
            Element::Tree(Some(_), _)
            | Element::SumTree(Some(_), ..)
            | Element::BigSumTree(Some(_), ..)
            | Element::CountTree(Some(_), ..)
            | Element::CountSumTree(Some(_), ..)
            | Element::ProvableCountTree(Some(_), ..)
            | Element::ProvableCountSumTree(Some(_), ..) => {
                return Err(Error::NotSupported(
                    "insert_into_count_indexed_tree only accepts EMPTY tree elements \
                     (root_key = None) for tree variants. The dedicated cidx insert \
                     short-circuits to NULL_HASH child roots; a non-None root_key \
                     would persist a mismatched chain. Use generic db.insert for \
                     non-empty tree claims, or insert empty here then populate"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
            _ => {}
        }

        let existing_is_tree = existing_item
            .as_ref()
            .map(|e| e.is_any_tree())
            .unwrap_or(false);
        if existing_is_tree {
            let existing_is_cidx = matches!(
                existing_item.as_ref().map(|e| e.underlying()),
                Some(Element::ProvableCountIndexedTree(..))
            );

            // The unconditional check above already guarantees the new
            // element is either non-tree OR an empty tree/cidx. Both
            // are safe to allow with cleanup (existing tree's child
            // storage cleared; cidx secondary cleared when applicable).

            // ALLOW + cleanup. Walk find_subtrees on the existing
            // entry's path and clear each subtree's storage. For cidx
            // existing, also clear the secondary namespace.
            let entry_path = path.derive_owned_with_child(item_key.to_vec());
            let entry_path_ref = SubtreePath::from(&entry_path);
            let subtrees_paths = cost_return_on_error!(
                &mut cost,
                self.find_subtrees(&entry_path_ref, Some(transaction), grove_version)
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
                            "unable to clean up old subtree storage in \
                             insert_into_count_indexed_tree overwrite: {e}",
                        ))
                    })
                );
            }
            if existing_is_cidx {
                let primary_prefix =
                    RocksDbStorage::build_prefix(entry_path_ref.clone()).unwrap_add_cost(&mut cost);
                let secondary_prefix =
                    RocksDbStorage::secondary_prefix_for(&primary_prefix, IndexAxis::Count.tag())
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
                            "unable to clean up nested cidx secondary in \
                             insert_into_count_indexed_tree overwrite: {e}",
                        ))
                    })
                );
            }
        }

        // 4. Insert into primary. Dispatch on element kind so tree
        //    subtree entries take the layered (combine_hash) path; using
        //    `Element::insert` for tree elements would set the merk
        //    node's value_hash to value_hash(serialized) without the
        //    combine_hash composition, breaking the merkle invariant
        //    for the cidx primary until a deep insert later updates it.
        match item.underlying() {
            Element::Item(..) | Element::SumItem(..) | Element::ItemWithSumItem(..) => {
                cost_return_on_error!(
                    &mut cost,
                    item.insert(&mut primary_merk, item_key, None, grove_version)
                        .map_err(Error::MerkError)
                );
            }
            Element::Reference(reference_path_type, ..)
            | Element::ReferenceWithSumItem(reference_path_type, ..) => {
                // Resolve the reference, fetch the target's value_hash,
                // and insert via Element::insert_reference so the merk
                // node carries combine_hash(value_hash(serialized),
                // referenced_value_hash). NonCounted is unwrapped above
                // by underlying(); the outer `item` still owns the
                // wrapper byte that goes to storage.
                let cidx_primary_path_vec = path.to_vec();
                let resolved_path = cost_return_on_error_no_add!(
                    cost,
                    grovedb_element::reference_path::path_from_reference_path_type(
                        reference_path_type.clone(),
                        &cidx_primary_path_vec,
                        Some(item_key)
                    )
                    .map_err(Error::from)
                );
                let referenced_item = cost_return_on_error!(
                    &mut cost,
                    self.follow_reference(
                        resolved_path.as_slice().into(),
                        false,
                        Some(transaction),
                        grove_version,
                    )
                );
                let referenced_value_hash = cost_return_on_error!(
                    &mut cost,
                    referenced_item
                        .value_hash(grove_version)
                        .map_err(Error::from)
                );
                cost_return_on_error!(
                    &mut cost,
                    item.insert_reference(
                        &mut primary_merk,
                        item_key,
                        referenced_value_hash,
                        None,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
                );
            }
            Element::Tree(..)
            | Element::SumTree(..)
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
                cost_return_on_error!(
                    &mut cost,
                    item.insert_subtree(
                        &mut primary_merk,
                        item_key,
                        grovedb_merk::tree::NULL_HASH,
                        None,
                        grove_version
                    )
                    .map_err(Error::MerkError)
                );
            }
            Element::ProvableCountIndexedTree(..) => {
                // Nested cidx creation: must use the dedicated cidx
                // subtree insert (Op::PutLayeredCountIndexedReference)
                // so the parent's merk node uses H1-A
                // (combine_hash_three) over the inner cidx's primary
                // and secondary root hashes — both NULL_HASH for an
                // empty inner cidx.
                cost_return_on_error!(
                    &mut cost,
                    item.insert_count_indexed_subtree(
                        &mut primary_merk,
                        item_key,
                        grovedb_merk::tree::NULL_HASH,
                        grovedb_merk::tree::NULL_HASH,
                        None,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
                );
            }
            // Phase 1 stub: PSIT / PCPSIT nested under a PCIT primary
            // are deliberately not supported yet — the insertion path
            // for those variants is a Phase 2 concern.
            Element::ProvableSumIndexedTree(..) => {
                return Err(Error::NotSupported(
                    "inserting a ProvableSumIndexedTree element via \
                     insert_into_count_indexed_tree is not yet supported (Phase 2)"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
            Element::ProvableCountProvableSumIndexedTree(..) => {
                return Err(Error::NotSupported(
                    "inserting a ProvableCountProvableSumIndexedTree element via \
                     insert_into_count_indexed_tree is not yet supported (Phase 2)"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
            Element::NonCounted(_) | Element::NotSummed(_) | Element::NotCountedOrSummed(_) => {
                unreachable!("underlying() unwraps wrappers")
            }
        };

        // 5. Open secondary and apply the mirror update.
        let mut secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_count_indexed_secondary_at_path(
                path,
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
                item_key,
                old_count_for_secondary,
                new_count_for_secondary,
                grove_version,
            )
        );

        // 6. Snapshot both Merks' new states.
        let (primary_root_hash, primary_root_key, primary_aggregate_data) = cost_return_on_error!(
            &mut cost,
            primary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(Error::MerkError)
        );
        let (secondary_root_hash, secondary_root_key, _secondary_aggregate) = cost_return_on_error!(
            &mut cost,
            secondary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(Error::MerkError)
        );

        // 7. Reconstruct the parent's CountIndexedTree element with the new
        //    root keys + aggregate count.
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
        match (is_provable_primary, reconstructed.underlying()) {
            (true, Element::ProvableCountIndexedTree(..)) => {}
            _ => {
                return Err(Error::CorruptedCodeExecution(
                    "reconstructed element kind does not match primary tree type",
                ))
                .wrap_with_cost(cost);
            }
        }
        // OLD count of `count_indexed_key` in parent_merk before the
        // rewrite — captured here so we can mirror parent_merk's
        // secondary if parent_merk is itself a CountIndexedTree primary
        // (nested case).
        let old_count_in_parent = count_indexed_element.count_value_or_default();
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

        // 7b. If parent_merk (where the CountIndexedTree element lives)
        //     is itself a CountIndexedTree primary (nested case), the
        //     count_value of the just-modified CountIndexedTree element
        //     in parent_merk's primary changed. Mirror this in
        //     parent_merk's secondary, capture the post-mirror state,
        //     and seed `deferred_secondary` for the upstream propagate.
        let initial_deferred_secondary = if parent_merk.tree_type.is_count_indexed_primary() {
            let new_count_in_parent = primary_aggregate_data.as_count_u64();
            let (gp_path, parent_cidx_key) = match parent_path.derive_parent() {
                Some(p) => p,
                None => {
                    return Err(Error::CorruptedCodeExecution(
                        "nested CountIndexedTree primary requires a grandparent",
                    ))
                    .wrap_with_cost(cost);
                }
            };
            let gp_merk = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(
                    gp_path,
                    transaction,
                    Some(batch),
                    grove_version,
                )
            );
            let gp_element = cost_return_on_error!(
                &mut cost,
                Element::get(&gp_merk, parent_cidx_key, true, grove_version)
                    .map_err(Error::MerkError)
            );
            let parent_secondary_root_key_before = match gp_element.underlying() {
                Element::ProvableCountIndexedTree(_, sec, ..) => sec.clone(),
                _ => {
                    return Err(Error::CorruptedData(
                        "expected ProvableCountIndexedTree element in grandparent for nested \
                         mirror"
                            .to_string(),
                    ))
                    .wrap_with_cost(cost);
                }
            };
            let mut parent_secondary_merk = cost_return_on_error!(
                &mut cost,
                self.open_count_indexed_secondary_at_path(
                    parent_path.clone(),
                    parent_secondary_root_key_before,
                    transaction,
                    Some(batch),
                    grove_version,
                )
            );
            cost_return_on_error!(
                &mut cost,
                mirror_to_secondary(
                    &mut parent_secondary_merk,
                    count_indexed_key,
                    Some(old_count_in_parent),
                    new_count_in_parent,
                    grove_version,
                )
            );
            let (sh, sk, _) = cost_return_on_error!(
                &mut cost,
                parent_secondary_merk
                    .root_hash_key_and_aggregate_data()
                    .map_err(Error::MerkError)
            );
            Some((sh, sk))
        } else {
            None
        };

        // 8. Hand off to `propagate_changes_with_transaction` from
        //    `parent_path`. The shared propagation logic understands
        //    nested CountIndexedTree levels — if the path traverses
        //    another CountIndexedTree above, it will use the three-input
        //    combine and mirror to that higher level's secondary as well.
        let mut merk_cache: std::collections::HashMap<
            SubtreePath<B>,
            Merk<PrefixedRocksDbTransactionContext>,
        > = std::collections::HashMap::default();
        merk_cache.insert(parent_path.clone(), parent_merk);
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction_with_initial_deferred(
                merk_cache,
                parent_path,
                initial_deferred_secondary,
                transaction,
                batch,
                grove_version,
            )
        );

        Ok(()).wrap_with_cost(cost)
    }

    /// Rebuild the count-ordered secondary index for a `CountIndexedTree`
    /// element from scratch by walking its primary Merk.
    ///
    /// **When you need this:**
    ///
    /// The dedicated [`Self::insert_into_count_indexed_tree`] /
    /// [`Self::delete_from_count_indexed_tree`] APIs maintain the
    /// secondary inline. The regular [`Self::insert`] / batch paths do
    /// **not** know about the secondary, so if the application uses
    /// those paths to mutate a sub-element whose `count_value` lives
    /// inside a CountIndexedTree primary (for example by inserting into
    /// a sub-`CountTree` stored as an item in the primary, which causes
    /// the sub-tree's aggregate count to propagate up into the primary's
    /// element bytes), the secondary will fall out of sync. After such
    /// updates, call this method to bring the secondary back in sync.
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
            self.open_count_indexed_secondary_at_path(
                path.clone(),
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
        let desired_keys: std::collections::HashSet<Vec<u8>> = entries
            .iter()
            .map(|(count, key)| make_secondary_key(*count, key))
            .collect();

        // 5. Delete entries that should not be present.
        let existing_set: std::collections::HashSet<Vec<u8>> =
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

    /// Iterate the secondary index in count-order and return the
    /// **top `k`** entries by `count_value`. When `descending` is `true`
    /// (the typical "highest first" use case) entries are walked
    /// right-to-left through the secondary's keyspace; ties on `count`
    /// are broken in descending lex order of the original key.
    ///
    /// Each returned entry is `(count, original_key)`. Resolving the
    /// primary value is the caller's responsibility (use
    /// `db.get(path, original_key, ...)`).
    ///
    /// For a verifiable variant, see [`Self::prove_count_indexed_top_k`]
    /// and [`Self::verify_count_indexed_top_k`].
    pub fn count_indexed_top_k<'b, B, P>(
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
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_root_key = cost_return_on_error!(
            &mut cost,
            self.read_count_indexed_secondary_root_key(path.clone(), tx_ref, None, grove_version)
        );

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_count_indexed_secondary_at_path(
                path,
                secondary_root_key,
                tx_ref,
                None,
                grove_version,
            )
        );

        let mut all_query = Query::new();
        all_query.left_to_right = !descending;
        all_query.insert_all();

        let mut iter = KVIterator::new(secondary_merk.storage.raw_iter(), &all_query)
            .unwrap_add_cost(&mut cost);

        let mut results = Vec::with_capacity(k as usize);
        while results.len() < k as usize {
            match iter.next_kv().unwrap_add_cost(&mut cost) {
                Some((secondary_key, _value_bytes)) => {
                    if let Some(decoded) = decode_secondary_key(&secondary_key) {
                        results.push(decoded);
                    } else {
                        return Err(Error::CorruptedData(format!(
                            "secondary key in count-indexed-tree is shorter than 8 bytes: {:?}",
                            secondary_key
                        )))
                        .wrap_with_cost(cost);
                    }
                }
                None => break,
            }
        }

        Ok(results).wrap_with_cost(cost)
    }

    /// Same as [`Self::count_indexed_top_k`] but skips the first
    /// `offset` entries in the directional scan before collecting up to
    /// `k` results. Used to implement paginated top-K views over the
    /// cidx secondary (e.g. "show me page 3 of the leaderboard").
    ///
    /// `offset = 0` is equivalent to plain `count_indexed_top_k` (no
    /// skip). The skip is performed at the secondary's storage iterator
    /// level — this is not a verifiable / proof-bounded skip; for the
    /// provable variant use [`Self::prove_count_indexed_top_k_paginated`]
    /// which relies on the merk-level count-offset proof to commit the
    /// skipped count via `HashWithCount`.
    pub fn count_indexed_top_k_paginated<'b, B, P>(
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
        let path: SubtreePath<B> = path.into();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_root_key = cost_return_on_error!(
            &mut cost,
            self.read_count_indexed_secondary_root_key(path.clone(), tx_ref, None, grove_version)
        );

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_count_indexed_secondary_at_path(
                path,
                secondary_root_key,
                tx_ref,
                None,
                grove_version,
            )
        );

        let mut all_query = Query::new();
        all_query.left_to_right = !descending;
        all_query.insert_all();

        let mut iter = KVIterator::new(secondary_merk.storage.raw_iter(), &all_query)
            .unwrap_add_cost(&mut cost);

        // Skip `offset` entries. The iterator burns the same merk-storage
        // seek count whether we skip or surface, so this is honestly
        // O(offset) at the merk level — for paginated UIs this is the
        // expected trade against a full scan + post-slice.
        let mut skipped: u64 = 0;
        while skipped < offset {
            match iter.next_kv().unwrap_add_cost(&mut cost) {
                Some((secondary_key, _)) => {
                    // Defensive decode: a malformed secondary key here
                    // (< 8 bytes) is a storage corruption indicator that
                    // we want to surface even during the skip phase, not
                    // mask by silently dropping into the limit window.
                    if decode_secondary_key(&secondary_key).is_none() {
                        return Err(Error::CorruptedData(format!(
                            "secondary key in count-indexed-tree is shorter than 8 bytes: {:?}",
                            secondary_key
                        )))
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
                Some((secondary_key, _value_bytes)) => {
                    if let Some(decoded) = decode_secondary_key(&secondary_key) {
                        results.push(decoded);
                    } else {
                        return Err(Error::CorruptedData(format!(
                            "secondary key in count-indexed-tree is shorter than 8 bytes: {:?}",
                            secondary_key
                        )))
                        .wrap_with_cost(cost);
                    }
                }
                None => break,
            }
        }

        Ok(results).wrap_with_cost(cost)
    }

    /// Iterate the secondary index over a count range `[lo_count,
    /// hi_count_inclusive]` and return matching `(count, original_key)`
    /// entries up to `limit`. Direction is controlled by `descending`.
    ///
    /// Bounds are inclusive on both sides; passing `(0, u64::MAX, false,
    /// limit)` is equivalent to a full scan.
    pub fn count_indexed_count_range<'b, B, P>(
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
        let mut cost = OperationCost::default();
        if lo_count > hi_count {
            return Ok(Vec::new()).wrap_with_cost(cost);
        }
        let path: SubtreePath<B> = path.into();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let secondary_root_key = cost_return_on_error!(
            &mut cost,
            self.read_count_indexed_secondary_root_key(path.clone(), tx_ref, None, grove_version)
        );

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_count_indexed_secondary_at_path(
                path,
                secondary_root_key,
                tx_ref,
                None,
                grove_version,
            )
        );

        // Seek directly to the encoded count bounds in the secondary's
        // keyspace instead of doing a full scan with post-filtering. The
        // secondary keys are `count_be_bytes ‖ original_key`; we build a
        // `RangeInclusive` query that brackets all encodings whose count
        // falls in `[lo_count, hi_count]`. The lower bound is
        // `lo_count_be ‖ <empty>`; the upper bound is
        // `hi_count_be ‖ 0xFF*` — we use `(hi_count + 1)_be` (or
        // `RangeFrom`-equivalent at the high end if `hi_count == u64::MAX`)
        // to make the upper boundary exclusive on the next count, which
        // is equivalent to inclusive on `hi_count` for any original_key
        // suffix.
        let mut lo_bytes = lo_count.to_be_bytes().to_vec();
        // Lower bound has no original_key suffix → smallest possible key
        // for `count == lo_count`.
        let upper_bytes = if hi_count == u64::MAX {
            // No representable next-count; use unbounded upper end.
            None
        } else {
            // Exclusive upper bound at (hi_count + 1) ‖ <empty>; this
            // includes every entry with count <= hi_count (any suffix).
            Some((hi_count + 1).to_be_bytes().to_vec())
        };

        let mut q = Query::new();
        q.left_to_right = !descending;
        match upper_bytes {
            Some(upper) => q.insert_range(lo_bytes..upper),
            None => {
                lo_bytes.shrink_to_fit();
                q.insert_range_from(lo_bytes..);
            }
        }

        let mut iter =
            KVIterator::new(secondary_merk.storage.raw_iter(), &q).unwrap_add_cost(&mut cost);

        let mut results = Vec::new();
        while results.len() < limit as usize {
            match iter.next_kv().unwrap_add_cost(&mut cost) {
                Some((secondary_key, _value_bytes)) => {
                    let Some((count, original_key)) = decode_secondary_key(&secondary_key) else {
                        return Err(Error::CorruptedData(format!(
                            "secondary key in count-indexed-tree is shorter than 8 bytes: {:?}",
                            secondary_key
                        )))
                        .wrap_with_cost(cost);
                    };
                    // Range bounds already filter; this is a defensive
                    // check that catches encoding bugs without affecting
                    // performance in the happy path.
                    debug_assert!(count >= lo_count && count <= hi_count);
                    results.push((count, original_key));
                }
                None => break,
            }
        }

        Ok(results).wrap_with_cost(cost)
    }

    /// Count the number of cidx entries whose `count_value` falls in
    /// `[lo_count, hi_count]`, without returning the entries
    /// themselves.
    ///
    /// Wraps `Merk::count_aggregate_on_range` against the cidx
    /// secondary (which is a `ProvableCountTree`); the merk walks the
    /// secondary in O(log n + boundary) using each internal node's
    /// stored count to short-circuit fully-inside / fully-outside
    /// subtrees. Use this when the caller only needs the *count* of
    /// matching entries — answering questions like "how many users
    /// have a score in `[100, 500]`?" — rather than the list of
    /// matching keys. For the listing form use
    /// [`Self::count_indexed_count_range`].
    ///
    /// `lo_count > hi_count` returns `Ok(0)` (degenerate range).
    /// `lo_count == 0 && hi_count == u64::MAX` is equivalent to "how
    /// many entries does this cidx have?". This call has no
    /// cryptographic guarantee — the returned count is whatever the
    /// merk reports. For a verifiable count, use
    /// [`Self::prove_count_indexed_count_range_aggregate`] +
    /// [`Self::verify_count_indexed_count_range_aggregate`].
    pub fn count_indexed_count_range_aggregate<'b, B, P>(
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

        let secondary_root_key = cost_return_on_error!(
            &mut cost,
            self.read_count_indexed_secondary_root_key(path.clone(), tx_ref, None, grove_version)
        );

        let secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_count_indexed_secondary_at_path(
                path,
                secondary_root_key,
                tx_ref,
                None,
                grove_version,
            )
        );

        // Build the secondary inner-range query. Secondary keys are
        // `count_be ‖ original_key`; an entry is in [lo_count, hi_count]
        // iff its key falls in `[lo_count_be ‖ <empty>, (hi_count+1)_be ‖
        // <empty>)` — exclusive on the upper, inclusive on the lower. The
        // empty key on each side anchors against any original_key suffix.
        // (Mirrors the bound construction in
        // [`Self::count_indexed_count_range`]; kept inline rather than
        // shared because the two callers want different `QueryItem`
        // shapes.)
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
                .map_err(|e| Error::CorruptedData(format!("cidx aggregate count on range: {e}")))
        );

        Ok(count).wrap_with_cost(cost)
    }

    /// Read the `secondary_root_key` field from a CountIndexedTree element
    /// at the given path. Returns an error if the path's last segment is
    /// not a count-indexed-tree element.
    fn read_count_indexed_secondary_root_key<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        transaction: &'db Transaction,
        batch: Option<&'db StorageBatch>,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<Vec<u8>>, Error> {
        let mut cost = OperationCost::default();
        let (parent_path, count_indexed_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot query a count-indexed tree at the root path".to_string(),
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
            Element::get(&parent_merk, count_indexed_key, true, grove_version)
                .map_err(Error::MerkError)
        );
        match element.underlying() {
            Element::ProvableCountIndexedTree(_, secondary, ..) => {
                Ok(secondary.clone()).wrap_with_cost(cost)
            }
            _ => Err(Error::InvalidPath(
                "path's last segment is not a ProvableCountIndexedTree element".to_string(),
            ))
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
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let removed = cost_return_on_error!(
            &mut cost,
            self.delete_from_count_indexed_tree_on_transaction(
                path,
                item_key,
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

        cost_return_on_error_no_add!(cost, tx.commit_local());
        Ok(removed).wrap_with_cost(cost)
    }

    fn delete_from_count_indexed_tree_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        item_key: &[u8],
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        let mut cost = OperationCost::default();

        let (parent_path, count_indexed_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot delete from count-indexed tree at the root path".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        let mut primary_merk = cost_return_on_error!(
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
                "delete_from_count_indexed_tree requires the path's last segment to be a \
                 CountIndexedTree or ProvableCountIndexedTree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }

        // Read existing item to determine the secondary entry to remove.
        let existing_item = cost_return_on_error!(
            &mut cost,
            Element::get_optional_from_storage(&primary_merk.storage, item_key, grove_version)
                .map_err(Error::MerkError)
        );
        let Some(existing) = existing_item else {
            return Ok(false).wrap_with_cost(cost);
        };
        let old_count = existing.count_value_or_default();

        let in_tree_type = primary_merk.tree_type;

        // Open parent for later element rewrite.
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

        // Determine the layered-or-not deletion shape based on what we're
        // deleting from the primary.
        let is_layered_target = existing.is_any_tree();

        cost_return_on_error!(
            &mut cost,
            Element::delete(
                &mut primary_merk,
                item_key,
                None,
                is_layered_target,
                in_tree_type,
                grove_version,
            )
            .map_err(Error::MerkError)
        );

        // Storage cleanup for tree entries.
        //
        // If the deleted cidx entry is a tree (CountTree, SumTree, etc.),
        // its child storage at `cidx_primary_path + item_key` is now
        // orphaned — the entry is gone from the primary Merk but its
        // children still occupy the storage namespace. Without cleanup,
        // a future insert at the same item_key would observe stale data
        // (orphaned entries surface to `verify_grovedb`'s raw_iter pass
        // even though they're invisible to the Merk tree at the new
        // root). Same class of bug as the direct `db.delete` cidx
        // primary cleanup (commit 6b7ec21d).
        //
        // For nested cidx entries (the deleted item is itself a cidx
        // primary), we also need to clear the secondary's storage at
        // the derived prefix.
        //
        // NOTE: we do NOT drop+reopen primary_merk around this block —
        // dropping the merk without explicit apply would lose the
        // staged Element::delete. The cleanup calls below only use
        // `&self` (via find_subtrees and get_transactional_storage_*),
        // which coexist with the owned primary_merk.
        if is_layered_target {
            let entry_path = path.derive_owned_with_child(item_key.to_vec());
            let entry_path_ref = SubtreePath::from(&entry_path);

            // Detect whether the deleted entry was itself a cidx
            // primary (nested cidx). Use the existing element snapshot.
            let deleted_was_cidx_primary =
                matches!(existing.underlying(), Element::ProvableCountIndexedTree(..));

            // Recursively clear all primary subtree storage under
            // `entry_path` via the same find_subtrees walk used by
            // db.delete.
            let subtrees_paths = cost_return_on_error!(
                &mut cost,
                self.find_subtrees(&entry_path_ref, Some(transaction), grove_version)
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
                            "unable to cleanup subtree storage during cidx delete: {e}",
                        ))
                    })
                );
            }

            // Nested cidx: also clear the deleted entry's secondary
            // storage namespace at Blake3(primary_prefix ‖ count_tag).
            if deleted_was_cidx_primary {
                let primary_prefix =
                    grovedb_storage::rocksdb_storage::RocksDbStorage::build_prefix(
                        entry_path_ref.clone(),
                    )
                    .unwrap_add_cost(&mut cost);
                let secondary_prefix =
                    grovedb_storage::rocksdb_storage::RocksDbStorage::secondary_prefix_for(
                        &primary_prefix,
                        IndexAxis::Count.tag(),
                    )
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
                            "unable to cleanup nested cidx secondary during \
                             delete_from_count_indexed_tree: {e}",
                        ))
                    })
                );
            }
        }

        // Open and update secondary.
        let mut secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_count_indexed_secondary_at_path(
                path,
                secondary_root_key_before,
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let old_secondary_key = make_secondary_key(old_count, item_key);
        cost_return_on_error!(
            &mut cost,
            Element::delete(
                &mut secondary_merk,
                old_secondary_key.as_slice(),
                None,
                false,
                TreeType::ProvableCountTree,
                grove_version,
            )
            .map_err(Error::MerkError)
        );

        // Snapshot both merks' new states.
        let (primary_root_hash, primary_root_key, primary_aggregate_data) = cost_return_on_error!(
            &mut cost,
            primary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(Error::MerkError)
        );
        let (secondary_root_hash, secondary_root_key, _secondary_aggregate) = cost_return_on_error!(
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
        // Capture parent_merk's element's OLD count for the
        // count_indexed_key BEFORE the rewrite, so we can mirror to
        // parent_merk's secondary if parent_merk is itself a cidx
        // primary (nested case).
        let old_count_in_parent = count_indexed_element.count_value_or_default();
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

        // 7b (nested case). Mirror parent's secondary if parent_merk is
        //     itself a CountIndexedTree primary, then seed
        //     `deferred_secondary` for upstream propagate.
        let initial_deferred_secondary = if parent_merk.tree_type.is_count_indexed_primary() {
            let new_count_in_parent = primary_aggregate_data.as_count_u64();
            let (gp_path, parent_cidx_key) = match parent_path.derive_parent() {
                Some(p) => p,
                None => {
                    return Err(Error::CorruptedCodeExecution(
                        "nested CountIndexedTree primary requires a grandparent",
                    ))
                    .wrap_with_cost(cost);
                }
            };
            let gp_merk = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(
                    gp_path,
                    transaction,
                    Some(batch),
                    grove_version,
                )
            );
            let gp_element = cost_return_on_error!(
                &mut cost,
                Element::get(&gp_merk, parent_cidx_key, true, grove_version)
                    .map_err(Error::MerkError)
            );
            let parent_secondary_root_key_before = match gp_element.underlying() {
                Element::ProvableCountIndexedTree(_, sec, ..) => sec.clone(),
                _ => {
                    return Err(Error::CorruptedData(
                        "expected ProvableCountIndexedTree element in grandparent for nested \
                         mirror"
                            .to_string(),
                    ))
                    .wrap_with_cost(cost);
                }
            };
            let mut parent_secondary_merk = cost_return_on_error!(
                &mut cost,
                self.open_count_indexed_secondary_at_path(
                    parent_path.clone(),
                    parent_secondary_root_key_before,
                    transaction,
                    Some(batch),
                    grove_version,
                )
            );
            cost_return_on_error!(
                &mut cost,
                mirror_to_secondary(
                    &mut parent_secondary_merk,
                    count_indexed_key,
                    Some(old_count_in_parent),
                    new_count_in_parent,
                    grove_version,
                )
            );
            let (sh, sk, _) = cost_return_on_error!(
                &mut cost,
                parent_secondary_merk
                    .root_hash_key_and_aggregate_data()
                    .map_err(Error::MerkError)
            );
            Some((sh, sk))
        } else {
            None
        };

        // Hand off to shared propagation (CountIndexedTree-aware).
        let mut merk_cache: std::collections::HashMap<
            SubtreePath<B>,
            Merk<PrefixedRocksDbTransactionContext>,
        > = std::collections::HashMap::default();
        merk_cache.insert(parent_path.clone(), parent_merk);
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction_with_initial_deferred(
                merk_cache,
                parent_path,
                initial_deferred_secondary,
                transaction,
                batch,
                grove_version,
            )
        );

        Ok(true).wrap_with_cost(cost)
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
    /// `path` is the path to the PSIT element (its primary Merk's
    /// path). The child element must be sum-bearing (see
    /// [`Element::is_sum_bearing_child`]); otherwise an
    /// `InvalidInputError` is returned.
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
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        cost_return_on_error!(
            &mut cost,
            self.insert_into_psit_on_transaction(
                path,
                item_key,
                item,
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

    fn insert_into_psit_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        item_key: &[u8],
        item: Element,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        // PSIT secondary key = sum_sort_key (8 bytes) ‖ item_key, so
        // the same 247-byte ceiling as PCIT applies.
        cost_return_on_error_no_add!(cost, validate_cidx_item_key_len(item_key));

        // Reject non-sum-bearing children up front. PSIT's primary is
        // a sum-bearing tree; inserting a non-sum-bearing item would
        // contribute 0 to the sum but make the secondary entry
        // meaningless.
        if !item.is_sum_bearing_child() {
            return Err(Error::InvalidInput(
                "ProvableSumIndexedTree only accepts sum-bearing children (SumItem, \
                 ItemWithSumItem, ReferenceWithSumItem, or any sum-bearing tree variant)",
            ))
            .wrap_with_cost(cost);
        }

        let (parent_path, psit_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot insert into ProvableSumIndexedTree at the root path".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        let mut primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        if !matches!(primary_merk.tree_type, TreeType::ProvableSumIndexedTree) {
            return Err(Error::InvalidPath(
                "insert_into_provable_sum_indexed_tree requires the path's last segment to be a \
                 ProvableSumIndexedTree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }

        let mut parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                parent_path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        let psit_element = cost_return_on_error!(
            &mut cost,
            Element::get(&parent_merk, psit_key, true, grove_version).map_err(Error::MerkError)
        );
        let secondary_root_key_before = match psit_element.underlying() {
            Element::ProvableSumIndexedTree(_, sec, ..) => sec.clone(),
            _ => {
                return Err(Error::CorruptedData(
                    "parent element at PSIT key is not a ProvableSumIndexedTree".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        // Read existing primary entry's old sum for the secondary
        // mirror.
        let existing_item = cost_return_on_error!(
            &mut cost,
            Element::get_optional_from_storage(&primary_merk.storage, item_key, grove_version)
                .map_err(Error::MerkError)
        );
        let old_sum_for_secondary = existing_item.as_ref().map(|e| e.sum_value_or_default());
        let new_sum_for_secondary = item.sum_value_or_default();

        // Insert into primary. For PSIT we only accept sum-bearing
        // items (sum item, item-with-sum, references, or sum-bearing
        // trees). The merk Element::insert / insert_reference /
        // insert_subtree dispatch covers all cases.
        match item.underlying() {
            Element::SumItem(..) | Element::ItemWithSumItem(..) => {
                cost_return_on_error!(
                    &mut cost,
                    item.insert(&mut primary_merk, item_key, None, grove_version)
                        .map_err(Error::MerkError)
                );
            }
            Element::ReferenceWithSumItem(reference_path_type, ..) => {
                let psit_primary_path_vec = path.to_vec();
                let resolved_path = cost_return_on_error_no_add!(
                    cost,
                    grovedb_element::reference_path::path_from_reference_path_type(
                        reference_path_type.clone(),
                        &psit_primary_path_vec,
                        Some(item_key)
                    )
                    .map_err(Error::from)
                );
                let referenced_item = cost_return_on_error!(
                    &mut cost,
                    self.follow_reference(
                        resolved_path.as_slice().into(),
                        false,
                        Some(transaction),
                        grove_version,
                    )
                );
                let referenced_value_hash = cost_return_on_error!(
                    &mut cost,
                    referenced_item
                        .value_hash(grove_version)
                        .map_err(Error::from)
                );
                cost_return_on_error!(
                    &mut cost,
                    item.insert_reference(
                        &mut primary_merk,
                        item_key,
                        referenced_value_hash,
                        None,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
                );
            }
            // Sum-bearing trees: write with NULL_HASH (empty subtree
            // assumption); same restriction as the PCIT path.
            Element::SumTree(..)
            | Element::BigSumTree(..)
            | Element::CountSumTree(..)
            | Element::ProvableCountSumTree(..)
            | Element::ProvableSumTree(..)
            | Element::ProvableCountProvableSumTree(..) => {
                cost_return_on_error!(
                    &mut cost,
                    item.insert_subtree(
                        &mut primary_merk,
                        item_key,
                        grovedb_merk::tree::NULL_HASH,
                        None,
                        grove_version
                    )
                    .map_err(Error::MerkError)
                );
            }
            _ => {
                return Err(Error::InvalidInput(
                    "ProvableSumIndexedTree: unsupported child element",
                ))
                .wrap_with_cost(cost);
            }
        }

        // Open + mirror to the sum-axis secondary.
        let mut secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_indexed_secondary_at_path(
                path,
                IndexAxis::Sum,
                secondary_root_key_before,
                transaction,
                Some(batch),
                grove_version,
            )
        );
        cost_return_on_error!(
            &mut cost,
            mirror_psit_to_secondary(
                &mut secondary_merk,
                item_key,
                old_sum_for_secondary,
                new_sum_for_secondary,
                grove_version,
            )
        );

        // Snapshot both merks.
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
            psit_element
                .reconstruct_with_two_root_keys(
                    primary_root_key,
                    secondary_root_key,
                    primary_aggregate_data,
                )
                .ok_or(Error::CorruptedCodeExecution(
                    "reconstruct_with_two_root_keys returned None for a PSIT element",
                ))
        );
        cost_return_on_error!(
            &mut cost,
            reconstructed
                .insert_count_indexed_subtree(
                    &mut parent_merk,
                    psit_key,
                    primary_root_hash,
                    secondary_root_hash,
                    None,
                    grove_version,
                )
                .map_err(Error::MerkError)
        );

        // Propagate parent changes via the shared propagation path. We
        // do NOT need an initial_deferred_secondary here because PSIT
        // primaries do not yet nest under another indexed-tree primary
        // (PCIT nesting under PCIT is the only currently exercised
        // nested case). When PSIT/PCPSIT under another indexed-tree
        // primary lands, generalize this branch the way the PCIT path
        // does (with a deferred-secondary capture).
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
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let removed = cost_return_on_error!(
            &mut cost,
            self.delete_from_psit_on_transaction(path, item_key, tx_ref, &batch, grove_version,)
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx_ref))
                .map_err(Into::into)
        );

        cost_return_on_error_no_add!(cost, tx.commit_local());
        Ok(removed).wrap_with_cost(cost)
    }

    fn delete_from_psit_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        item_key: &[u8],
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        let mut cost = OperationCost::default();

        let (parent_path, psit_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot delete from ProvableSumIndexedTree at the root path".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        let mut primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        if !matches!(primary_merk.tree_type, TreeType::ProvableSumIndexedTree) {
            return Err(Error::InvalidPath(
                "delete_from_provable_sum_indexed_tree requires the path's last segment to be a \
                 ProvableSumIndexedTree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }

        let existing_item = cost_return_on_error!(
            &mut cost,
            Element::get_optional_from_storage(&primary_merk.storage, item_key, grove_version)
                .map_err(Error::MerkError)
        );
        let Some(existing) = existing_item else {
            return Ok(false).wrap_with_cost(cost);
        };
        let old_sum = existing.sum_value_or_default();

        let in_tree_type = primary_merk.tree_type;
        let is_layered_target = existing.is_any_tree();

        let mut parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                parent_path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        let psit_element = cost_return_on_error!(
            &mut cost,
            Element::get(&parent_merk, psit_key, true, grove_version).map_err(Error::MerkError)
        );
        let secondary_root_key_before = match psit_element.underlying() {
            Element::ProvableSumIndexedTree(_, sec, ..) => sec.clone(),
            _ => {
                return Err(Error::CorruptedData(
                    "parent element at PSIT key is not a ProvableSumIndexedTree".to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        cost_return_on_error!(
            &mut cost,
            Element::delete(
                &mut primary_merk,
                item_key,
                None,
                is_layered_target,
                in_tree_type,
                grove_version,
            )
            .map_err(Error::MerkError)
        );

        // Delete corresponding secondary entry.
        let mut secondary_merk = cost_return_on_error!(
            &mut cost,
            self.open_indexed_secondary_at_path(
                path,
                IndexAxis::Sum,
                secondary_root_key_before,
                transaction,
                Some(batch),
                grove_version,
            )
        );
        let old_secondary_key = make_axis_secondary_key(IndexAxis::Sum, 0, old_sum, item_key);
        cost_return_on_error!(
            &mut cost,
            Element::delete(
                &mut secondary_merk,
                old_secondary_key.as_slice(),
                None,
                false,
                TreeType::ProvableSumTree,
                grove_version,
            )
            .map_err(Error::MerkError)
        );

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
            psit_element
                .reconstruct_with_two_root_keys(
                    primary_root_key,
                    secondary_root_key,
                    primary_aggregate_data,
                )
                .ok_or(Error::CorruptedCodeExecution(
                    "reconstruct_with_two_root_keys returned None for a PSIT element"
                ))
        );
        cost_return_on_error!(
            &mut cost,
            reconstructed
                .insert_count_indexed_subtree(
                    &mut parent_merk,
                    psit_key,
                    primary_root_hash,
                    secondary_root_hash,
                    None,
                    grove_version,
                )
                .map_err(Error::MerkError)
        );

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

        Ok(true).wrap_with_cost(cost)
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
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        cost_return_on_error!(
            &mut cost,
            self.insert_into_pcpsit_on_transaction(
                path,
                item_key,
                item,
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

    fn insert_into_pcpsit_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        item_key: &[u8],
        item: Element,
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        cost_return_on_error_no_add!(cost, validate_cidx_item_key_len(item_key));

        if !item.is_count_and_sum_bearing_child() {
            return Err(Error::InvalidInput(
                "ProvableCountProvableSumIndexedTree only accepts children that contribute \
                 both count and sum (ItemWithSumItem, ReferenceWithSumItem, CountSumTree, \
                 ProvableCountSumTree, ProvableCountProvableSumTree, or a nested PCPSIT)",
            ))
            .wrap_with_cost(cost);
        }

        let (parent_path, pcpsit_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot insert into ProvableCountProvableSumIndexedTree at the root path"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        let mut primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        if !matches!(
            primary_merk.tree_type,
            TreeType::ProvableCountProvableSumIndexedTree
        ) {
            return Err(Error::InvalidPath(
                "insert_into_provable_count_provable_sum_indexed_tree requires the path's last \
                 segment to be a ProvableCountProvableSumIndexedTree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }

        let mut parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                parent_path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        let pcpsit_element = cost_return_on_error!(
            &mut cost,
            Element::get(&parent_merk, pcpsit_key, true, grove_version).map_err(Error::MerkError)
        );
        let axes_before = match pcpsit_element.underlying() {
            Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _) => axes.clone(),
            _ => {
                return Err(Error::CorruptedData(
                    "parent element at PCPSIT key is not a ProvableCountProvableSumIndexedTree"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };
        if axes_before.is_empty() {
            return Err(Error::InvalidInput(
                "ProvableCountProvableSumIndexedTree has no axes configured; cannot mirror to \
                 any secondary index. Configure 1..=3 axes when first inserting the PCPSIT \
                 element",
            ))
            .wrap_with_cost(cost);
        }

        // Read existing primary entry for (old_count, old_sum).
        let existing_item = cost_return_on_error!(
            &mut cost,
            Element::get_optional_from_storage(&primary_merk.storage, item_key, grove_version)
                .map_err(Error::MerkError)
        );
        let (old_count, old_sum) = existing_item
            .as_ref()
            .map(|e| e.count_sum_value_or_default())
            .map(|(c, s)| (Some(c), Some(s)))
            .unwrap_or((None, None));
        let (new_count, new_sum) = item.count_sum_value_or_default();

        // Insert into primary.
        match item.underlying() {
            Element::ItemWithSumItem(..) => {
                cost_return_on_error!(
                    &mut cost,
                    item.insert(&mut primary_merk, item_key, None, grove_version)
                        .map_err(Error::MerkError)
                );
            }
            Element::ReferenceWithSumItem(reference_path_type, ..) => {
                let primary_path_vec = path.to_vec();
                let resolved_path = cost_return_on_error_no_add!(
                    cost,
                    grovedb_element::reference_path::path_from_reference_path_type(
                        reference_path_type.clone(),
                        &primary_path_vec,
                        Some(item_key)
                    )
                    .map_err(Error::from)
                );
                let referenced_item = cost_return_on_error!(
                    &mut cost,
                    self.follow_reference(
                        resolved_path.as_slice().into(),
                        false,
                        Some(transaction),
                        grove_version,
                    )
                );
                let referenced_value_hash = cost_return_on_error!(
                    &mut cost,
                    referenced_item
                        .value_hash(grove_version)
                        .map_err(Error::from)
                );
                cost_return_on_error!(
                    &mut cost,
                    item.insert_reference(
                        &mut primary_merk,
                        item_key,
                        referenced_value_hash,
                        None,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
                );
            }
            Element::CountSumTree(..)
            | Element::ProvableCountSumTree(..)
            | Element::ProvableCountProvableSumTree(..) => {
                cost_return_on_error!(
                    &mut cost,
                    item.insert_subtree(
                        &mut primary_merk,
                        item_key,
                        grovedb_merk::tree::NULL_HASH,
                        None,
                        grove_version
                    )
                    .map_err(Error::MerkError)
                );
            }
            _ => {
                return Err(Error::InvalidInput(
                    "ProvableCountProvableSumIndexedTree: unsupported child element",
                ))
                .wrap_with_cost(cost);
            }
        }

        // For each configured axis, open the secondary and apply the
        // delete-then-insert mirror. Collect each axis's
        // (root_hash, root_key) for the new axes TLV.
        let mut new_axes: Vec<(u8, Option<Vec<u8>>)> = Vec::with_capacity(axes_before.len());
        let mut axis_root_hashes: Vec<(u8, grovedb_merk::CryptoHash)> =
            Vec::with_capacity(axes_before.len());
        for (tag, sec_root_key_before) in &axes_before {
            let axis = cost_return_on_error_no_add!(
                cost,
                IndexAxis::try_from_tag(*tag).map_err(|e| {
                    Error::CorruptedData(format!("invalid axis tag in PCPSIT element: {e}"))
                })
            );
            let mut secondary_merk = cost_return_on_error!(
                &mut cost,
                self.open_indexed_secondary_at_path(
                    path.clone(),
                    axis,
                    sec_root_key_before.clone(),
                    transaction,
                    Some(batch),
                    grove_version,
                )
            );
            cost_return_on_error!(
                &mut cost,
                mirror_pcpsit_axis_to_secondary(
                    &mut secondary_merk,
                    axis,
                    item_key,
                    old_count,
                    old_sum,
                    Some(new_count),
                    Some(new_sum),
                    grove_version,
                )
            );
            let (sec_hash, sec_root_key, _) = cost_return_on_error!(
                &mut cost,
                secondary_merk
                    .root_hash_key_and_aggregate_data()
                    .map_err(Error::MerkError)
            );
            new_axes.push((*tag, sec_root_key));
            axis_root_hashes.push((*tag, sec_hash));
        }

        let (primary_root_hash, primary_root_key, primary_aggregate_data) = cost_return_on_error!(
            &mut cost,
            primary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(Error::MerkError)
        );
        let axes_digest_value =
            grovedb_merk::tree::axes_digest(&axis_root_hashes).unwrap_add_cost(&mut cost);

        let reconstructed = cost_return_on_error_no_add!(
            cost,
            pcpsit_element
                .reconstruct_with_axes(primary_root_key, primary_aggregate_data, new_axes)
                .ok_or(Error::CorruptedCodeExecution(
                    "reconstruct_with_axes returned None for a PCPSIT element"
                ))
        );
        cost_return_on_error!(
            &mut cost,
            reconstructed
                .insert_count_indexed_subtree(
                    &mut parent_merk,
                    pcpsit_key,
                    primary_root_hash,
                    axes_digest_value,
                    None,
                    grove_version,
                )
                .map_err(Error::MerkError)
        );

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
        let mut cost = OperationCost::default();
        let path: SubtreePath<B> = path.into();
        let batch = StorageBatch::new();
        let tx = TxRef::new(&self.db, transaction);
        let tx_ref = tx.as_ref();

        let removed = cost_return_on_error!(
            &mut cost,
            self.delete_from_pcpsit_on_transaction(path, item_key, tx_ref, &batch, grove_version,)
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx_ref))
                .map_err(Into::into)
        );

        cost_return_on_error_no_add!(cost, tx.commit_local());
        Ok(removed).wrap_with_cost(cost)
    }

    fn delete_from_pcpsit_on_transaction<'db, 'b, B: AsRef<[u8]>>(
        &'db self,
        path: SubtreePath<'b, B>,
        item_key: &[u8],
        transaction: &'db Transaction,
        batch: &'db StorageBatch,
        grove_version: &GroveVersion,
    ) -> CostResult<bool, Error> {
        let mut cost = OperationCost::default();

        let (parent_path, pcpsit_key) = match path.derive_parent() {
            Some(p) => p,
            None => {
                return Err(Error::InvalidPath(
                    "cannot delete from ProvableCountProvableSumIndexedTree at the root path"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        let mut primary_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        if !matches!(
            primary_merk.tree_type,
            TreeType::ProvableCountProvableSumIndexedTree
        ) {
            return Err(Error::InvalidPath(
                "delete_from_provable_count_provable_sum_indexed_tree requires the path's \
                 last segment to be a ProvableCountProvableSumIndexedTree element"
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }

        let existing_item = cost_return_on_error!(
            &mut cost,
            Element::get_optional_from_storage(&primary_merk.storage, item_key, grove_version)
                .map_err(Error::MerkError)
        );
        let Some(existing) = existing_item else {
            return Ok(false).wrap_with_cost(cost);
        };
        let (old_count, old_sum) = existing.count_sum_value_or_default();

        let in_tree_type = primary_merk.tree_type;
        let is_layered_target = existing.is_any_tree();

        let mut parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                parent_path.clone(),
                transaction,
                Some(batch),
                grove_version
            )
        );
        let pcpsit_element = cost_return_on_error!(
            &mut cost,
            Element::get(&parent_merk, pcpsit_key, true, grove_version).map_err(Error::MerkError)
        );
        let axes_before = match pcpsit_element.underlying() {
            Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _) => axes.clone(),
            _ => {
                return Err(Error::CorruptedData(
                    "parent element at PCPSIT key is not a ProvableCountProvableSumIndexedTree"
                        .to_string(),
                ))
                .wrap_with_cost(cost);
            }
        };

        cost_return_on_error!(
            &mut cost,
            Element::delete(
                &mut primary_merk,
                item_key,
                None,
                is_layered_target,
                in_tree_type,
                grove_version,
            )
            .map_err(Error::MerkError)
        );

        let mut new_axes: Vec<(u8, Option<Vec<u8>>)> = Vec::with_capacity(axes_before.len());
        let mut axis_root_hashes: Vec<(u8, grovedb_merk::CryptoHash)> =
            Vec::with_capacity(axes_before.len());
        for (tag, sec_root_key_before) in &axes_before {
            let axis = cost_return_on_error_no_add!(
                cost,
                IndexAxis::try_from_tag(*tag).map_err(|e| {
                    Error::CorruptedData(format!("invalid axis tag in PCPSIT element: {e}"))
                })
            );
            let mut secondary_merk = cost_return_on_error!(
                &mut cost,
                self.open_indexed_secondary_at_path(
                    path.clone(),
                    axis,
                    sec_root_key_before.clone(),
                    transaction,
                    Some(batch),
                    grove_version,
                )
            );
            cost_return_on_error!(
                &mut cost,
                mirror_pcpsit_axis_to_secondary(
                    &mut secondary_merk,
                    axis,
                    item_key,
                    Some(old_count),
                    Some(old_sum),
                    None,
                    None,
                    grove_version,
                )
            );
            let (sec_hash, sec_root_key, _) = cost_return_on_error!(
                &mut cost,
                secondary_merk
                    .root_hash_key_and_aggregate_data()
                    .map_err(Error::MerkError)
            );
            new_axes.push((*tag, sec_root_key));
            axis_root_hashes.push((*tag, sec_hash));
        }

        let (primary_root_hash, primary_root_key, primary_aggregate_data) = cost_return_on_error!(
            &mut cost,
            primary_merk
                .root_hash_key_and_aggregate_data()
                .map_err(Error::MerkError)
        );
        let axes_digest_value =
            grovedb_merk::tree::axes_digest(&axis_root_hashes).unwrap_add_cost(&mut cost);

        let reconstructed = cost_return_on_error_no_add!(
            cost,
            pcpsit_element
                .reconstruct_with_axes(primary_root_key, primary_aggregate_data, new_axes)
                .ok_or(Error::CorruptedCodeExecution(
                    "reconstruct_with_axes returned None for a PCPSIT element"
                ))
        );
        cost_return_on_error!(
            &mut cost,
            reconstructed
                .insert_count_indexed_subtree(
                    &mut parent_merk,
                    pcpsit_key,
                    primary_root_hash,
                    axes_digest_value,
                    None,
                    grove_version,
                )
                .map_err(Error::MerkError)
        );

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

        Ok(true).wrap_with_cost(cost)
    }
}

/// Apply the sum-axis secondary mirror for a PSIT insert/update. The
/// secondary stores entries at `(sum_sort_key ‖ item_key)` whose value
/// is a `SumItem(sum)` so the secondary's sum aggregate matches the
/// primary's.
fn mirror_psit_to_secondary<'db, S: StorageContext<'db>>(
    secondary: &mut Merk<S>,
    item_key: &[u8],
    old_sum: Option<i64>,
    new_sum: i64,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    if let Some(old) = old_sum
        && old == new_sum
    {
        return Ok(()).wrap_with_cost(cost);
    }
    if let Some(old) = old_sum {
        let old_secondary_key = make_axis_secondary_key(IndexAxis::Sum, 0, old, item_key);
        cost_return_on_error!(
            &mut cost,
            Element::delete(
                secondary,
                old_secondary_key.as_slice(),
                None,
                false,
                TreeType::ProvableSumTree,
                grove_version,
            )
            .map_err(Error::MerkError)
        );
    }
    let new_secondary_key = make_axis_secondary_key(IndexAxis::Sum, 0, new_sum, item_key);
    let secondary_entry = Element::new_sum_item(new_sum);
    cost_return_on_error!(
        &mut cost,
        secondary_entry
            .insert(secondary, new_secondary_key.as_slice(), None, grove_version)
            .map_err(Error::MerkError)
    );
    Ok(()).wrap_with_cost(cost)
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
fn mirror_pcpsit_axis_to_secondary<'db, S: StorageContext<'db>>(
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

    // Compute old and new sort keys. Either may be None (no entry).
    let old_key = match (old_count, old_sum) {
        (Some(c), Some(s)) => Some(make_axis_secondary_key(axis, c, s, item_key)),
        _ => None,
    };
    let new_key = match (new_count, new_sum) {
        (Some(c), Some(s)) => Some(make_axis_secondary_key(axis, c, s, item_key)),
        _ => None,
    };

    if old_key == new_key && new_key.is_some() {
        // The sort key didn't move and there's still an entry there;
        // the previous insert already wrote the correct value, so
        // nothing more is needed.
        return Ok(()).wrap_with_cost(cost);
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
        let entry = match axis {
            // Count axis: secondary uses a ProvableCountTree, every
            // entry contributes count = 1; a plain Item is sufficient.
            IndexAxis::Count => Element::new_item(Vec::new()),
            // Sum axis: secondary uses a ProvableSumTree, each entry
            // contributes its own sum value.
            IndexAxis::Sum => Element::new_sum_item(new_sum_val),
            // Avg axis: secondary uses a ProvableCountProvableSumTree;
            // an ItemWithSumItem(empty, sum) contributes (1, sum).
            IndexAxis::Avg => Element::new_item_with_sum_item(Vec::new(), new_sum_val),
        };
        cost_return_on_error!(
            &mut cost,
            entry
                .insert(secondary, nk.as_slice(), None, grove_version)
                .map_err(Error::MerkError)
        );
    }
    Ok(()).wrap_with_cost(cost)
}

/// Apply the secondary-mirror update for a primary mutation in the
/// generic batch path. Handles all four cases (insert / update / delete
/// / no-op) by combining `old_count` and `new_count` Options:
/// - `(None, None)`: no-op (key was absent before and after).
/// - `(None, Some(c))`: insert at count `c`.
/// - `(Some(c), None)`: delete the secondary entry at count `c`.
/// - `(Some(o), Some(n))`: update — delete entry at `o` and insert at `n`
///   (skips both if `o == n`).
pub(crate) fn mirror_to_secondary_for_batch<'db, S: StorageContext<'db>>(
    secondary: &mut Merk<S>,
    item_key: &[u8],
    old_count: Option<u64>,
    new_count: Option<u64>,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    if old_count == new_count {
        // (None, None) and (Some(o) == Some(o)) are no-ops.
        return Ok(()).wrap_with_cost(cost);
    }
    if let Some(old) = old_count {
        let old_secondary_key = make_secondary_key(old, item_key);
        cost_return_on_error!(
            &mut cost,
            Element::delete(
                secondary,
                old_secondary_key.as_slice(),
                None,
                false,
                TreeType::ProvableCountTree,
                grove_version,
            )
            .map_err(Error::MerkError)
        );
    }
    if let Some(new) = new_count {
        let new_secondary_key = make_secondary_key(new, item_key);
        let secondary_entry = Element::new_item(Vec::new());
        cost_return_on_error!(
            &mut cost,
            secondary_entry
                .insert(secondary, new_secondary_key.as_slice(), None, grove_version)
                .map_err(Error::MerkError)
        );
    }
    Ok(()).wrap_with_cost(cost)
}

/// Apply the secondary-mirror update for a primary insert/update.
///
/// `old_count` is `None` for a fresh insert, `Some(c)` for an update.
pub(crate) fn mirror_to_secondary<'db, S: StorageContext<'db>>(
    secondary: &mut Merk<S>,
    item_key: &[u8],
    old_count: Option<u64>,
    new_count: u64,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();

    if let Some(old) = old_count
        && old == new_count
    {
        // The secondary entry is already at the right position.
        return Ok(()).wrap_with_cost(cost);
    }

    if let Some(old) = old_count {
        let old_secondary_key = make_secondary_key(old, item_key);
        cost_return_on_error!(
            &mut cost,
            Element::delete(
                secondary,
                old_secondary_key.as_slice(),
                None,
                false,
                TreeType::ProvableCountTree,
                grove_version,
            )
            .map_err(Error::MerkError)
        );
    }

    let new_secondary_key = make_secondary_key(new_count, item_key);
    let secondary_entry = Element::new_item(Vec::new());
    cost_return_on_error!(
        &mut cost,
        secondary_entry
            .insert(secondary, new_secondary_key.as_slice(), None, grove_version)
            .map_err(Error::MerkError)
    );

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

/// Returns `Err(Error::InvalidInput)` if `item_key.len() > 247`.
/// Used by every cidx primary write path to bound the secondary key.
#[inline]
pub(crate) fn validate_cidx_item_key_len(item_key: &[u8]) -> Result<(), Error> {
    if item_key.len() > MAX_CIDX_ITEM_KEY_LEN {
        return Err(Error::InvalidInput(
            "item key for a CountIndexedTree primary must be at most 247 bytes (the \
             secondary index prepends an 8-byte count, and Merk requires keys < 256 bytes)",
        ));
    }
    Ok(())
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
