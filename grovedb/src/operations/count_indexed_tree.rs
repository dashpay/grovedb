//! `CountIndexedTree` direct operations.
//!
//! These dedicated APIs handle the two-Merk machinery (primary +
//! count-ordered secondary) for direct (non-batch) access. Batch-style
//! integration with the existing batch-propagation pass is a follow-up
//! effort.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, decode::ElementDecodeExtensions,
        delete::ElementDeleteFromStorageExtensions, get::ElementFetchFromStorageExtensions,
        insert::ElementInsertToStorageExtensions, reconstruct::ElementReconstructExtensions,
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

impl GroveDb {
    /// Open the secondary Merk for a `CountIndexedTree` /
    /// `ProvableCountIndexedTree` element at `path`. The secondary lives at
    /// `Blake3(primary_prefix ‖ 0x01)` per the S2-B prefix derivation.
    ///
    /// `secondary_root_key` is read from the parent's
    /// `Element::CountIndexedTree(_, secondary_root_key, ..)` field.
    ///
    /// The Merk is opened with `TreeType::ProvableCountTree` regardless of
    /// whether the parent was `CountIndexedTree` or
    /// `ProvableCountIndexedTree` — the secondary is always a provable
    /// count tree (each entry contributes count = 1).
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
        let mut cost = OperationCost::default();
        let primary_prefix = RocksDbStorage::build_prefix(path).unwrap_add_cost(&mut cost);
        let secondary_prefix =
            RocksDbStorage::secondary_prefix_for(&primary_prefix).unwrap_add_cost(&mut cost);
        let storage = self
            .db
            .get_transactional_storage_context_by_subtree_prefix(secondary_prefix, batch, tx)
            .unwrap_add_cost(&mut cost);
        if secondary_root_key.is_some() {
            Merk::open_layered_with_root_key(
                storage,
                secondary_root_key,
                TreeType::ProvableCountTree,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "cannot open count-indexed-tree secondary by prefix with given root key: {e}"
                ))
            })
            .add_cost(cost)
        } else {
            Merk::open_base(
                storage,
                TreeType::ProvableCountTree,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "cannot open empty count-indexed-tree secondary by prefix: {e}"
                ))
            })
            .add_cost(cost)
        }
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
            Element::CountIndexedTree(_, secondary, ..)
            | Element::ProvableCountIndexedTree(_, secondary, ..) => secondary.clone(),
            _ => {
                return Err(Error::CorruptedData(
                    "parent element at count-indexed key is not a CountIndexedTree".to_string(),
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

        // 4. Insert into primary.
        cost_return_on_error!(
            &mut cost,
            item.insert(&mut primary_merk, item_key, None, grove_version)
                .map_err(Error::MerkError)
        );

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
            (true, Element::ProvableCountIndexedTree(..))
            | (false, Element::CountIndexedTree(..)) => {}
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
                Element::CountIndexedTree(_, sec, ..)
                | Element::ProvableCountIndexedTree(_, sec, ..) => sec.clone(),
                _ => {
                    return Err(Error::CorruptedData(
                        "expected CountIndexedTree element in grandparent for nested mirror"
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
            Element::CountIndexedTree(_, secondary, ..)
            | Element::ProvableCountIndexedTree(_, secondary, ..) => secondary.clone(),
            _ => {
                return Err(Error::CorruptedData(
                    "parent element at count-indexed key is not a CountIndexedTree".to_string(),
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
    /// Reads do not yet produce verifiable proofs — the proof system
    /// integration for `CountIndexedQuery` is a follow-up.
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
            Element::CountIndexedTree(_, secondary, ..)
            | Element::ProvableCountIndexedTree(_, secondary, ..) => {
                Ok(secondary.clone()).wrap_with_cost(cost)
            }
            _ => Err(Error::InvalidPath(
                "path's last segment is not a CountIndexedTree element".to_string(),
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
            Element::CountIndexedTree(_, secondary, ..)
            | Element::ProvableCountIndexedTree(_, secondary, ..) => secondary.clone(),
            _ => {
                return Err(Error::CorruptedData(
                    "parent element at count-indexed key is not a CountIndexedTree".to_string(),
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
                Element::CountIndexedTree(_, sec, ..)
                | Element::ProvableCountIndexedTree(_, sec, ..) => sec.clone(),
                _ => {
                    return Err(Error::CorruptedData(
                        "expected CountIndexedTree element in grandparent for nested mirror"
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
