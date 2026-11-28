//! The propagation rules of grove versions 1 to 5
//! ([`PropagationCostModel::V0`](super::PropagationCostModel::V0)): a write
//! carries every subtree it changed to the root of the grove before it
//! returns and is charged every level on the way. There is no stop point
//! below the root Merk, no mark and no finalize.

use grovedb_costs::OperationCost;

use super::{Carrier, FinalizeCharge};
use crate::{tree_type::TreeType, Error};

pub(super) fn carrier() -> Carrier {
    Carrier::Write
}

/// The read of the host is the write's, charged as it is made.
pub(super) fn host_read_at_open(read: OperationCost) -> OperationCost {
    read
}

/// A write carries to the root Merk, whatever is on the way.
pub(super) fn stop_point(_merks_on_the_path: &[TreeType]) -> usize {
    0
}

pub(super) fn mark() -> Result<OperationCost, Error> {
    Err(no_marks("mark"))
}

pub(super) fn marks_under_a_removed_host() -> Result<OperationCost, Error> {
    Err(no_marks("marks_under_a_removed_host"))
}

pub(super) fn finalize() -> Result<FinalizeCharge, Error> {
    Err(no_marks("finalize"))
}

fn no_marks(rule: &str) -> Error {
    Error::NotSupported(format!(
        "{rule}: a grove version whose writes carry to the root of the grove leaves no mark"
    ))
}
