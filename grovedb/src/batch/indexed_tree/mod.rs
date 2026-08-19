//! Indexed-tree (PCIT / PSIT / PCPSIT) helpers for the batch apply pipeline.
//!
//! The two-Merk propagation pattern is self-contained here — kept out of
//! `batch/mod.rs` so that file stays focused on the generic pipeline — and
//! shared by all three indexed variants. One file per phase, in the order
//! the pipeline runs them:
//!
//! - [`preflight`] runs in the `apply_batch*` entry points before any merk
//!   is touched: [`reject_indexed_overwrite_with_descendants`] rejects
//!   batches that OVERWRITE an existing element with an indexed tree while
//!   also writing under it (the post-apply cleanup of the old element's
//!   storage would clear the new writes). Genuine creation plus population
//!   in one batch is supported — the level executor opens the fresh primary
//!   and secondaries from the in-batch element, and the bubble-up emits
//!   `InsertAggregateIndexedTreeRootKeys`.
//! - [`overwrite`] classifies overwrite-capable ops that displaced an
//!   existing element (V4+ only, gated by
//!   `overwrite_indexed_cleanup_inspection`). [`classify_cidx_overwrite`]
//!   runs against the OLD element bytes the merk apply already fetched —
//!   surfaced through the merk old-value observer — so it performs no
//!   storage read of its own and V4 costs match V1..V3 exactly. Indexed →
//!   non-indexed and indexed → empty indexed are allowed and schedule the
//!   old tree's storage for cleanup; indexed → non-empty indexed is
//!   rejected as ambiguous.
//! - [`delete_tree`] holds the other V4 gate's check:
//!   [`validate_delete_tree_type`] treats the tree type a `DeleteTree` op
//!   carries as a checked claim rather than storage-ownership authority, so
//!   cleanup namespaces are selected from what is actually stored. It takes
//!   the already-loaded stored element (from the emptiness pre-scan's own
//!   read or the merk old-value observer) and performs no read itself.
//! - [`pre_state`] runs against an indexed primary's level just before the
//!   merk apply: [`capture_indexed_pre_state`] validates the level's ops
//!   against the indexed-primary rules and reads each mutated key's *old*
//!   `(count, sum, value_hash)` state so the mirror can compute exact old → new
//!   transitions.
//! - [`mirror`] runs once per configured axis after the primary merk's
//!   `apply_with_specialized_costs` returns:
//!   [`apply_indexed_secondary_mirror_post_apply`] re-reads each captured
//!   key's new aggregates, assembles one atomic Merk batch of row moves for
//!   that axis's secondary, applies it, and returns the secondary's
//!   post-mirror `(root_hash, root_key)` for the bubble-up to fold into the
//!   parent's H1-A composition — directly for the single-axis variants,
//!   through `axes_digest` for PCPSIT.
//!
//! What lives in this file is what more than one phase needs: the entry-state
//! type aliases and [`read_entry_state`], the primary-entry read that the
//! capture ("pre") and the mirror ("post") both perform.

mod delete_tree;
mod mirror;
mod overwrite;
mod pre_state;
mod preflight;

pub(crate) use delete_tree::validate_delete_tree_type;
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{element::costs::ElementCostExtensions, CryptoHash, Merk};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;
pub(crate) use mirror::{apply_indexed_secondary_mirror_post_apply, read_post_apply_transitions};
pub(crate) use overwrite::classify_cidx_overwrite;
pub(crate) use pre_state::capture_indexed_pre_state;
pub(crate) use preflight::reject_indexed_overwrite_with_descendants;

use crate::{Element, Error};

/// Complete state that decides one indexed secondary row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct IndexedEntryState {
    count: u64,
    sum: i64,
    value_hash: CryptoHash,
}

/// A primary entry's state, `None` when the entry does not exist on that side
/// of the transition.
pub(super) type MaybeEntryState = Option<IndexedEntryState>;

/// One captured key's transition: `(item_key, old state, new state)`.
type AggregateTransition = (Vec<u8>, MaybeEntryState, MaybeEntryState);

/// Read one primary entry's current aggregate and commitment state. `phase`
/// labels error messages ("pre" before the primary apply, "post" after it).
fn read_entry_state<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    key: &[u8],
    phase: &str,
    grove_version: &GroveVersion,
) -> CostResult<MaybeEntryState, Error> {
    let mut cost = OperationCost::default();
    let maybe = cost_return_on_error!(
        &mut cost,
        primary_merk
            .get_value_and_value_hash(
                key,
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| Error::CorruptedData(format!(
                "indexed {phase}-state read for key {}: {e}",
                hex::encode(key)
            )))
    );
    let state = if let Some((bytes, value_hash)) = maybe {
        let elem = cost_return_on_error_no_add!(
            cost,
            Element::deserialize(bytes.as_slice(), grove_version).map_err(|e| {
                Error::CorruptedData(format!("indexed {phase}-state deserialize: {e}"))
            })
        );
        let (count, sum) = elem.count_sum_value_or_default();
        Some(IndexedEntryState {
            count,
            sum,
            value_hash,
        })
    } else {
        None
    };
    Ok(state).wrap_with_cost(cost)
}
