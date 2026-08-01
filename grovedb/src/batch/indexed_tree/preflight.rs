//! Preflight guard run by the `apply_batch*` entry points before any merk
//! is touched.
//!
//! A batch may not both CREATE an indexed primary and write under it: the
//! H1-A propagation has to read the indexed element from the parent merk to
//! learn its secondary root keys (and, for PCPSIT, its axes), and a
//! freshly-inserted element has not been flushed there yet.

use crate::{
    batch::{GroveOp, QualifiedGroveDbOp},
    Element, Error,
};

/// The qualified path (`op.path + op.key`) of every indexed-tree primary
/// this batch CREATES, via any insert-style op carrying an indexed element.
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

/// Preflight check: reject any batch that both **creates** a
/// `CountIndexedTree` / `ProvableCountIndexedTree` element AND
/// contains other ops targeting paths inside the freshly-created
/// cidx in the same batch.
///
/// Why: cidx propagation needs both primary and secondary root state
/// to bubble up via the H1-A `combine_hash_three` composition. There
/// is no `InsertAggregateIndexedTreeWithRootKeys` counterpart to
/// `ReplaceAggregateIndexedTreeRootKeys`, and the secondary merk
/// cannot be opened during propagation because the parent's cidx
/// element bytes aren't on disk yet. Without this preflight, callers
/// hit a confusing `MerkError(PathKeyNotFound)` mid-batch as the
/// secondary-merk closure tries to read the cidx element from a
/// parent merk that doesn't yet contain it.
///
/// Workaround: split into two batches. First batch creates the
/// empty cidx; second batch populates it (or call
/// `db.insert_into_count_indexed_tree` directly for individual
/// items).
pub(crate) fn reject_freshly_inserted_cidx_with_descendants(
    ops: &[QualifiedGroveDbOp],
) -> Result<(), Error> {
    let fresh_cidx_paths = freshly_created_indexed_paths(ops);
    if fresh_cidx_paths.is_empty() {
        return Ok(());
    }
    // Reject any op whose effective target path is strictly under one
    // of the fresh cidx paths. The effective path is
    // `op.path + op.key` (keyless ops use just `op.path`). The
    // cidx-creation op itself doesn't trigger (its target equals the
    // cidx path exactly).
    for op in ops {
        let mut op_target = op.path.to_path();
        if let Some(key) = &op.key {
            op_target.push(key.get_key_clone());
        }
        for cidx_path in &fresh_cidx_paths {
            if op_target.len() > cidx_path.len() && op_target[..cidx_path.len()] == cidx_path[..] {
                return Err(Error::NotSupported(
                    "populating a freshly-inserted indexed tree (ProvableCountIndexedTree \
                     / ProvableSumIndexedTree / ProvableCountProvableSumIndexedTree) in the \
                     same batch as its creation is not \
                     supported (no Insert variant for aggregate-indexed two-Merk \
                     propagation exists, and the secondary merk cannot be opened from \
                     stale parent state during bubble-up). Split into two batches: \
                     insert the empty cidx first, then populate it via \
                     `db.insert_into_count_indexed_tree` or a follow-up batch."
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}
