//! The propagation rules of grove version 6
//! ([`PropagationCostModel::V1`](super::PropagationCostModel::V1)): a write
//! stops at its stop points and carries nothing above them, and a
//! finalize carries every marked subtree once
//! (`docs/lignindb/cost-model.md`, section 16).

use grovedb_costs::OperationCost;

use super::{Carrier, FinalizeCharge};
use crate::{tree_type::TreeType, Error};

pub(super) fn carrier() -> Carrier {
    Carrier::Finalize
}

/// SP2: nothing, whether or not the code makes the read.
pub(super) fn host_read_at_open(_read: OperationCost) -> OperationCost {
    OperationCost::default()
}

/// SP5: the topmost primary of an indexed tree on the path, and the changed
/// subtree itself where there is none.
///
/// Carrying a subtree into an entry of an indexed tree writes the entry's
/// rows again, which adds and removes bytes, and every change under an
/// entry reaches the entry. So a write that changes anything under an
/// indexed tree carries it up to the primary, and under indexed trees one
/// inside the other up to the outermost.
pub(super) fn stop_point(merks_on_the_path: &[TreeType]) -> usize {
    merks_on_the_path
        .iter()
        .position(TreeType::is_indexed_primary)
        .unwrap_or(merks_on_the_path.len().saturating_sub(1))
}

/// SP3: a mark is free.
pub(super) fn mark() -> Result<OperationCost, Error> {
    Ok(OperationCost::default())
}

/// SP6: so is taking the marks under a host away with the host.
pub(super) fn marks_under_a_removed_host() -> Result<OperationCost, Error> {
    Ok(OperationCost::default())
}

/// FZ1: the charge of a finalize, with nothing in it yet.
pub(super) fn finalize() -> Result<FinalizeCharge, Error> {
    Ok(FinalizeCharge::new())
}
