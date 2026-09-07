//! Flat-subtree drop (issue #848): O(1) consensus removal of a populated
//! subtree that is declared to contain no child subtrees, with storage
//! reclaimed outside consensus via range tombstones.
//!
//! # The primitive
//!
//! [`GroveDb::drop_flat_subtree`] deletes a subtree's element from its
//! parent Merk exactly like deleting any single element — the parent's root
//! hash (and therefore the grove root) is immediately correct, absence is
//! provable, and the cost is a deterministic function of the element bytes,
//! its key, the parent Merk's shape, and the path. The subtree's contents
//! are **never opened, checked, or metered**, which is what makes the cost
//! O(1) in the subtree's size: dropping a tree of ten million entries
//! costs the same as dropping a tree of ten.
//!
//! Atomically with the delete, a durable redo record
//! ([`PendingPrefixDropRecord`]) is committed into a reserved namespace of
//! the meta column family, listing every storage prefix that just became
//! unreachable: the subtree's own path-derived prefix and, for indexed
//! primaries, the three per-axis secondary prefixes. Reclamation is then a
//! handful of DB-level range tombstones per record — O(1) writes that
//! RocksDB compaction turns into physically reclaimed disk in the
//! background:
//!
//! - when GroveDB owns the transaction (the caller passed `None`), the
//!   record is drained immediately after the commit, in the same call;
//! - when the caller owns the transaction, records become visible (and
//!   drainable) only once the caller commits; the host calls
//!   [`GroveDb::flush_pending_prefix_drops`] afterwards — or at any later
//!   point, including after a crash and restart: records survive in the
//!   database, checkpoints carry them, and draining is idempotent.
//!
//! Range tombstones cannot ride RocksDB optimistic transactions, which is
//! why reclamation is a separate, non-transactional step. It is safe
//! because the tombstoned prefixes are unreachable from the live element
//! graph — nothing else ever reads or writes them — and it never
//! contributes to the operation's returned cost (consistent with callers
//! that exclude such drops from storage-refund accounting).
//!
//! # The flatness contract
//!
//! The caller declares that the dropped subtree contains **no child
//! subtrees**. GroveDB cannot verify this without an O(contents) scan, so
//! it is a contract, like reference-lifecycle management on `delete`. For
//! most tree types it holds by construction (non-Merk data trees cannot
//! contain children; indexed primaries reject generic child writes); only
//! a plain Merk tree relies on the caller's word. A false declaration
//! leaks the children's storage — their prefixes are cryptographically
//! unrelated to the parent's, so the tombstones never touch them and they
//! remain on disk, unreachable and invisible to hashes, proofs, state
//! sync, and `verify_grovedb` — but it never corrupts state.
//!
//! # Path reuse
//!
//! Storage prefixes derive from paths alone, so re-creating a subtree at a
//! dropped path re-derives the identical prefix. Callers must not
//! re-create an element at a dropped path before its record has been
//! drained. As defense in depth the drain re-resolves each record's path
//! against the live element graph first and skips (rather than tombstones)
//! any record whose path resolves to a live element — a contract violation
//! degrades to a bounded, reported storage leak, never to destruction of
//! live data.
//!
//! # Dangling references
//!
//! As with every delete operation, incoming references to the dropped
//! subtree (or anything inside it) become dangling; following one returns
//! a corrupted-reference error rather than incorrect data. Reference
//! lifecycle is the caller's responsibility.

use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add,
    storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval, CostResult, CostsExt,
    OperationCost,
};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::{delete::ElementDeleteFromStorageExtensions, tree_type::ElementTreeTypeExtensions},
    Merk, MerkOptions,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::{
        pending_prefix_drops_namespace, PendingPrefixDropRecord, PrefixedRocksDbTransactionContext,
        RocksDbStorage,
    },
    Storage, StorageBatch, StorageContext,
};
use grovedb_version::{
    error::GroveVersionError,
    version::{FeatureVersion, GroveVersion},
};

use crate::{util::TxRef, Element, Error, GroveDb, TransactionArg};

/// Reject a flat-drop entry point whose capability slot is not active.
///
/// Slot `0` (every version before `GROVE_V4`) means the operation is
/// unavailable; slot `1` is the active v1 implementation. The comparison is
/// an EXACT match — accepting `slot > 1` would silently run v1 code under a
/// future protocol version that assigned the slot new semantics.
pub(crate) fn check_flat_drop_enabled(method: &str, slot: FeatureVersion) -> Result<(), Error> {
    if slot != 1 {
        return Err(GroveVersionError::UnknownVersionMismatch {
            method: method.to_string(),
            known_versions: vec![1],
            received: slot,
        }
        .into());
    }
    Ok(())
}

/// Compute every storage prefix a dropped subtree owns: its path-derived
/// primary prefix plus, when the element is an indexed primary, the three
/// per-axis secondary prefixes (all three unconditionally, matching the
/// existing delete-path sweep — tombstoning an unused axis namespace is a
/// no-op).
pub(crate) fn doomed_prefixes_for_drop<B: AsRef<[u8]>>(
    subtree_path: SubtreePath<B>,
    is_indexed_primary: bool,
    cost: &mut OperationCost,
) -> Vec<[u8; 32]> {
    let primary_prefix = RocksDbStorage::build_prefix(subtree_path).unwrap_add_cost(cost);
    let mut doomed = Vec::with_capacity(if is_indexed_primary { 4 } else { 1 });
    doomed.push(primary_prefix);
    if is_indexed_primary {
        for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            doomed.push(
                RocksDbStorage::secondary_prefix_for(&primary_prefix, axis.tag())
                    .unwrap_add_cost(cost),
            );
        }
    }
    doomed
}

/// Add the deterministic cost of staging one flat-drop redo record to an
/// estimated-cost accumulation. Average and worst case share the formula:
/// the record's size is a function of the path's maximum segment lengths
/// and the declared tree type, both known up front. Mirrors the real cost
/// charged by `stage_flat_drop_records` / `drop_flat_subtree`: the prefix
/// derivation hash calls plus the meta put charged at commit
/// (`AbstractBatchOperation::PutMeta` with `cost_info: None` — one seek
/// and `paid_key_len + paid_value_len` added bytes).
#[cfg(feature = "estimated_costs")]
pub(crate) fn add_flat_drop_record_put_estimate(
    cost: &mut OperationCost,
    path: &crate::batch::KeyInfoPath,
    key: &crate::batch::key_info::KeyInfo,
    tree_type: &grovedb_merk::tree_type::TreeType,
) {
    use grovedb_storage::worst_case_costs::WorstKeyLength;
    use integer_encoding::VarInt;

    let is_indexed = tree_type.is_indexed_primary();
    let doomed_count: u32 = if is_indexed { 4 } else { 1 };
    let segment_lengths: Vec<u32> = path
        .0
        .iter()
        .map(|segment| segment.max_length() as u32)
        .chain(std::iter::once(key.max_length() as u32))
        .collect();
    // `build_prefix` of the dropped subtree's full path: one Blake3 call
    // per 64-byte block of the path body (segment bytes, the native usize
    // segment count, one length byte per segment).
    let body_len: u32 = segment_lengths.iter().sum::<u32>()
        + std::mem::size_of::<usize>() as u32
        + segment_lengths.len() as u32;
    let blocks = if body_len == 0 {
        1
    } else {
        1 + (body_len - 1) / 64
    };
    // Plus one single-block hash per axis-secondary prefix derivation.
    cost.hash_node_calls += blocks + if is_indexed { 3 } else { 0 };
    // The meta put: key is `namespace ‖ primary_prefix` (64 bytes), value
    // is the record encoding — version byte, doomed count, the doomed
    // prefixes, segment count, then length-prefixed segments.
    let key_len: u32 = 64;
    let value_len: u32 =
        2 + 32 * doomed_count + 1 + segment_lengths.iter().map(|len| 1 + len).sum::<u32>();
    cost.seek_count += 1;
    cost.storage_cost.added_bytes +=
        key_len + key_len.required_space() as u32 + value_len + value_len.required_space() as u32;
}

/// Outcome of one [`GroveDb::flush_pending_prefix_drops`] pass, for
/// telemetry only — never consensus.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PendingPrefixDropsReport {
    /// Records whose doomed prefixes were tombstoned and which were then
    /// removed from the queue.
    pub reclaimed_records: u64,
    /// Records skipped because their path resolves to a live element — a
    /// dropped path was re-created before its record drained (a caller
    /// contract violation). Their storage stays leaked and the records
    /// remain, so the condition keeps being reported.
    pub skipped_live: u64,
}

impl GroveDb {
    /// Drop the subtree element at `path`/`key` in O(1), without opening
    /// or sweeping its contents, and stage its storage prefixes for
    /// reclamation. See the [module documentation](self) for the full
    /// contract: the caller declares the subtree contains **no child
    /// subtrees**, incoming references dangle, and the dropped path must
    /// not be re-created before its record drains.
    ///
    /// The returned cost covers the parent-Merk delete, upward hash
    /// propagation, and the redo record's bytes — a deterministic function
    /// of the element, key, parent shape, and path, independent of the
    /// subtree's contents. Reclamation itself is unmetered.
    pub fn drop_flat_subtree<'b, B, P>(
        &self,
        path: P,
        key: &[u8],
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        let path: SubtreePath<B> = path.into();
        let mut cost = OperationCost::default();

        cost_return_on_error_no_add!(
            cost,
            check_flat_drop_enabled(
                "drop_flat_subtree",
                grove_version
                    .grovedb_versions
                    .operations
                    .flat_drop
                    .drop_flat_subtree,
            )
        );

        let tx = TxRef::new(&self.db, transaction);
        let batch = StorageBatch::new();

        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw(path.clone(), key, Some(tx.as_ref()), grove_version)
        );
        let Some(dropped_tree_type) = element.tree_type() else {
            return Err(Error::InvalidInput(
                "drop_flat_subtree: the element at the given path/key is not a tree",
            ))
            .wrap_with_cost(cost);
        };

        let mut parent_merk = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(
                path.clone(),
                tx.as_ref(),
                Some(&batch),
                grove_version
            )
        );
        // A generic delete out of an indexed primary cannot mirror the
        // removed child's ordering value into the secondary index — same
        // rejection as `delete`.
        cost_return_on_error_no_add!(
            cost,
            crate::operations::indexed_tree::reject_generic_write_into_indexed_primary(
                parent_merk.tree_type,
                "drop_flat_subtree",
            )
        );
        let parent_tree_type = parent_merk.tree_type;

        // Stage the redo record in the same atomic commit as the delete.
        let subtree_path_owned = path.derive_owned_with_child(key);
        let subtree_path_ref = SubtreePath::from(&subtree_path_owned);
        let doomed_prefixes = doomed_prefixes_for_drop(
            subtree_path_ref,
            dropped_tree_type.is_indexed_primary(),
            &mut cost,
        );
        let record = PendingPrefixDropRecord {
            primary_prefix: doomed_prefixes[0],
            path: {
                let mut segments = path.to_vec();
                segments.push(key.to_vec());
                segments
            },
            doomed_prefixes,
        };
        let record_value =
            cost_return_on_error_no_add!(cost, record.encode().map_err(Error::StorageError));
        let namespace_ctx = self
            .db
            .get_transactional_storage_context_by_subtree_prefix(
                *pending_prefix_drops_namespace(),
                Some(&batch),
                tx.as_ref(),
            )
            .unwrap_add_cost(&mut cost);
        cost_return_on_error!(
            &mut cost,
            namespace_ctx
                .put_meta(record.primary_prefix, &record_value, None)
                .map_err(Error::StorageError)
        );

        // The O(1) detach: an ordinary layered delete from the parent Merk.
        // The child subtree is never opened.
        cost_return_on_error_into!(
            &mut cost,
            Element::delete_with_sectioned_removal_bytes(
                &mut parent_merk,
                key,
                Some(MerkOptions {
                    base_root_storage_is_free: true
                }),
                true,
                parent_tree_type,
                &mut |_, removed_key_bytes, removed_value_bytes| {
                    Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    ))
                },
                grove_version,
            )
        );

        let mut merk_cache: HashMap<SubtreePath<B>, Merk<PrefixedRocksDbTransactionContext>> =
            HashMap::default();
        merk_cache.insert(path.clone(), parent_merk);
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction(
                merk_cache,
                path,
                tx.as_ref(),
                &batch,
                grove_version,
            )
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        let owns_tx = tx.is_owned();
        cost_return_on_error_no_add!(cost, tx.commit_local());

        if owns_tx {
            // Best-effort immediate reclamation. On failure the record
            // persists and the next flush retries — nothing is lost, so the
            // committed drop is still reported as success.
            let _ = self.flush_pending_prefix_drops(grove_version);
        }

        Ok(()).wrap_with_cost(cost)
    }

    /// Drain every committed pending-prefix-drop redo record: range-delete
    /// each record's doomed prefixes across all column families, then
    /// remove the record. Idempotent and crash-safe — a record is removed
    /// only after its tombstones are written, and re-tombstoning is a
    /// no-op. Never touches the root hash; the returned counts are
    /// telemetry, not consensus.
    ///
    /// Records staged inside a still-open caller transaction are invisible
    /// here (and roll back with it), so a drop can never be reclaimed
    /// before it is durably committed.
    ///
    /// Deliberately not version-gated: it only reclaims storage that a
    /// gated operation already orphaned, and is a no-op when no records
    /// exist. Hosts should call it after committing a transaction that
    /// contained drops, and once at startup to finish reclamation
    /// interrupted by a crash.
    pub fn flush_pending_prefix_drops(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<PendingPrefixDropsReport, Error> {
        let mut report = PendingPrefixDropsReport::default();
        for record in self.db.pending_prefix_drop_records()? {
            // Liveness guard: never tombstone a prefix whose path resolves
            // to a live element (the dropped path was re-created — a caller
            // contract violation; leak instead of destroying live data).
            let live = match record.path.split_last() {
                // A record with an empty path cannot be produced by
                // `drop_flat_subtree`; treat it as unsafe to act on.
                None => true,
                Some((key, parent_segments)) => {
                    let parent_path: SubtreePath<Vec<u8>> = parent_segments.into();
                    match self.get_raw(parent_path, key, None, grove_version).unwrap() {
                        Ok(_) => true,
                        Err(
                            Error::PathKeyNotFound(_)
                            | Error::PathNotFound(_)
                            | Error::PathParentLayerNotFound(_)
                            | Error::InvalidParentLayerPath(_),
                        ) => false,
                        Err(e) => return Err(e),
                    }
                }
            };
            if live {
                report.skipped_live += 1;
                continue;
            }
            self.db.delete_prefix_ranges(&record.doomed_prefixes)?;
            self.db
                .remove_pending_prefix_drop_record(&record.primary_prefix)?;
            report.reclaimed_records += 1;
        }
        Ok(report)
    }
}
