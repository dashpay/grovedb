//! Indexed-tree (PCIT / PSIT / PCPSIT) helpers for the batch apply pipeline.
//!
//! The two-Merk propagation pattern is self-contained here — kept out of
//! `batch/mod.rs` so that file stays focused on the generic pipeline — and
//! shared by all three indexed variants. One file per phase, in the order
//! the pipeline runs them:
//!
//! - [`preflight`] runs in the `apply_batch*` entry points before any merk
//!   is touched: [`reject_freshly_inserted_cidx_with_descendants`] rejects
//!   batches that both create an indexed primary AND write under it in the
//!   same batch — there is no `InsertAggregateIndexedTreeWithRootKeys` op,
//!   so the H1-A propagation cannot read the just-created element from a
//!   parent merk that has not been flushed yet.
//! - [`overwrite`] runs inside the per-op loop when tree-override protection
//!   is OFF and the op could overwrite an existing element (V4+ only — the
//!   stored-element read [`inspect_cidx_overwrite`] starts with is gated by
//!   `overwrite_indexed_cleanup_inspection` so released versions keep their
//!   cost shape). Indexed → non-indexed and indexed → empty indexed are
//!   allowed and schedule the old tree's storage for cleanup; indexed →
//!   non-empty indexed is rejected as ambiguous.
//! - [`delete_tree`] holds the other V4-gated stored-element read:
//!   [`validate_delete_tree_type`] treats the tree type a `DeleteTree` op
//!   carries as a checked claim rather than storage-ownership authority, so
//!   cleanup namespaces are selected from what is actually stored.
//! - [`pre_state`] runs against an indexed primary's level just before the
//!   merk apply: [`capture_indexed_pre_state`] validates the level's ops
//!   against the indexed-primary rules and reads each mutated key's *old*
//!   `(count, sum)` pair so the mirror can compute old → new transitions.
//! - [`mirror`] runs once per configured axis after the primary merk's
//!   `apply_with_specialized_costs` returns:
//!   [`apply_indexed_secondary_mirror_post_apply`] re-reads each captured
//!   key's new aggregates, assembles one atomic Merk batch of row moves for
//!   that axis's secondary, applies it, and returns the secondary's
//!   post-mirror `(root_hash, root_key)` for the bubble-up to fold into the
//!   parent's H1-A composition — directly for the single-axis variants,
//!   through `axes_digest` for PCPSIT.
//!
//! What lives in this file is what more than one phase needs: the aggregate
//! type aliases and [`read_entry_aggregates`], the primary-entry read that
//! the capture ("pre") and the mirror ("post") both perform.

mod delete_tree;
mod mirror;
mod overwrite;
mod pre_state;
mod preflight;

pub(crate) use delete_tree::validate_delete_tree_type;
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{element::costs::ElementCostExtensions, Merk};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;
pub(crate) use mirror::apply_indexed_secondary_mirror_post_apply;
pub(crate) use overwrite::inspect_cidx_overwrite;
pub(crate) use pre_state::capture_indexed_pre_state;
pub(crate) use preflight::reject_freshly_inserted_cidx_with_descendants;

use crate::{Element, Error};

/// A primary entry's `(count, sum)` aggregate pair, `None` when the entry
/// does not exist on that side of the transition.
type AggregatePair = Option<(u64, i64)>;

/// One captured key's aggregate transition: `(item_key, old, new)`.
type AggregateTransition = (Vec<u8>, AggregatePair, AggregatePair);

/// Read one primary entry's current `(count, sum)` pair, `None` if the key
/// does not exist. `phase` labels error messages ("pre" for the capture
/// before the primary apply, "post" for the delta read after it).
fn read_entry_aggregates<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    key: &[u8],
    phase: &str,
    grove_version: &GroveVersion,
) -> CostResult<AggregatePair, Error> {
    let mut cost = OperationCost::default();
    let maybe_bytes = cost_return_on_error!(
        &mut cost,
        primary_merk
            .get(
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
    let aggregates = if let Some(bytes) = maybe_bytes {
        let elem = cost_return_on_error_no_add!(
            cost,
            Element::deserialize(bytes.as_slice(), grove_version).map_err(|e| {
                Error::CorruptedData(format!("indexed {phase}-state deserialize: {e}"))
            })
        );
        Some(elem.count_sum_value_or_default())
    } else {
        None
    };
    Ok(aggregates).wrap_with_cost(cost)
}
