//! Preflight guard run by the `apply_batch*` entry points before any merk
//! is touched.
//!
//! Creating an indexed primary and populating it in the same batch is
//! supported; what a batch may not do is OVERWRITE an existing element
//! with an indexed tree while also writing under it — the post-apply
//! cleanup of the old element's storage namespaces would clear the new
//! writes at the same derived prefixes.

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
    Element, Error, GroveDb, Transaction,
};

/// The qualified path (`op.path + op.key`) of every indexed-tree primary
/// this batch creates or overwrites, via any insert-style op carrying an
/// indexed element.
///
/// All three indexed variants, not just PCIT: the limitation is the same for
/// each — the bubble-up has to read the indexed element from the parent merk
/// to learn its secondary root keys (and, for PCPSIT, its axes), and a
/// freshly-inserted element has not been flushed there yet. Before this
/// covered PSIT/PCPSIT the batch failed later and less clearly, with a
/// PathKeyNotFound from the secondary opener.
fn freshly_created_indexed_paths(ops: &[QualifiedGroveDbOp]) -> Vec<Vec<Vec<u8>>> {
    let mut fresh_cidx_paths: Vec<Vec<Vec<u8>>> = Vec::new();
    for op in ops {
        let elem = match &op.op {
            GroveOp::InsertOrReplace { element }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::Replace { element }
            | GroveOp::Patch { element, .. } => element,
            _ => continue,
        };
        if matches!(
            elem.underlying(),
            Element::ProvableCountIndexedTree(..)
                | Element::ProvableSumIndexedTree(..)
                | Element::ProvableCountProvableSumIndexedTree(..)
        ) && let Some(key) = &op.key
        {
            let mut cidx_path = op.path.to_path();
            cidx_path.push(key.get_key_clone());
            fresh_cidx_paths.push(cidx_path);
        }
    }
    fresh_cidx_paths
}

/// Preflight check: reject any batch that both **overwrites an
/// existing element with an indexed tree** AND contains other ops
/// targeting paths inside that indexed tree in the same batch.
///
/// Genuine CREATION plus population in one batch is supported: the
/// level executor opens the fresh primary and its per-axis secondaries
/// from the in-batch element, and the bubble-up emits
/// `InsertAggregateIndexedTreeRootKeys` to write the element with its
/// computed root state. What stays rejected is the OVERWRITE variant —
/// replacing an element that already exists on disk while writing under
/// it — because the post-apply cleanup of the old element's storage
/// namespaces runs after the new writes land on the same derived
/// prefixes, and would silently clear them.
///
/// Existence is checked against the transaction state (one read per
/// indexed-creation-with-descendants path, so batches without the
/// pattern pay nothing), treating a missing parent path as absent — a
/// parent created in this same batch means the element cannot exist on
/// disk either.
pub(crate) fn reject_indexed_overwrite_with_descendants(
    db: &GroveDb,
    ops: &[QualifiedGroveDbOp],
    transaction: &Transaction,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    let fresh_cidx_paths = freshly_created_indexed_paths(ops);
    if fresh_cidx_paths.is_empty() {
        return Ok(()).wrap_with_cost(cost);
    }
    // Which of the candidate paths have a write strictly under them in
    // this batch? The effective target path is `op.path + op.key`
    // (keyless ops use just `op.path`); the creation op itself does not
    // trigger (its target equals the candidate path exactly).
    for cidx_path in &fresh_cidx_paths {
        let has_descendant_write = ops.iter().any(|op| {
            let mut op_target = op.path.to_path();
            if let Some(key) = &op.key {
                op_target.push(key.get_key_clone());
            }
            op_target.len() > cidx_path.len() && op_target[..cidx_path.len()] == cidx_path[..]
        });
        if !has_descendant_write {
            continue;
        }
        let (key, parent_path) = cidx_path
            .split_last()
            .expect("qualified path carries at least the key");
        let existing = cost_return_on_error!(
            &mut cost,
            db.get_raw_optional_on_transaction_caching_optional(
                parent_path.into(),
                key,
                true,
                transaction,
                grove_version,
            )
        );
        if existing.is_some() {
            return Err(Error::NotSupported(
                "overwriting an EXISTING element with an indexed tree \
                 (ProvableCountIndexedTree / ProvableSumIndexedTree / \
                 ProvableCountProvableSumIndexedTree) while also writing under \
                 it in the same batch is not supported: the post-apply cleanup \
                 of the old element's storage would clear the new writes. \
                 DeleteTree the old element first, then create and populate \
                 the indexed tree in a follow-up batch (same transaction is \
                 fine)."
                    .to_string(),
            ))
            .wrap_with_cost(cost);
        }
    }
    Ok(()).wrap_with_cost(cost)
}
