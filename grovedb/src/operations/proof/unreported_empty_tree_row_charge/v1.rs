//! The budget slot of an unreported empty tree row: **v1** (`GROVE_V5`+).
//!
//! The row takes one slot of the global budget and of the frame's
//! per-instance budget, as it does in the prover and in the trusted read,
//! and the walk stops once either is spent, as it does after every reported
//! row. The verifier's budgets therefore match the prover's whether or not
//! the caller asked for empty trees in the results.
//!
//! This cannot make the verifier drop a row the query owes. The row is
//! charged only after `verify_empty_tree_binding` tied its bytes to the
//! committed value hash, so it is a key the state holds and the query
//! selects, and the trusted read spends a slot on it as well: the rows after
//! the stop are rows of a later page.

use crate::{operations::proof::V1LimitState, GroveDb};

impl GroveDb {
    /// `charge_unreported_empty_tree_row` v1: charges one row and reports
    /// whether a budget is now spent. See the module documentation.
    pub(super) fn charge_unreported_empty_tree_row_v1(
        limit_state: &mut V1LimitState,
        frame_instance: &mut Option<u16>,
    ) -> bool {
        limit_state.charge_row_with_instance(frame_instance);
        limit_state.is_exhausted(*frame_instance)
    }
}
