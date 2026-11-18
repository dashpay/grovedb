//! The budget slot of an empty tree row the verifier does not report:
//! versioned dispatch.
//!
//! An empty plain `Tree` that no subquery selects is a result row: the
//! trusted read returns it and counts it against the limit, and the V1 prover
//! charges it one row (the "Empty trees without subquery" arm of
//! `prove_subqueries_v1`). A verifier run with
//! `include_empty_trees_in_result` false, the default of `verify_query` and
//! `verify_subset_query`, binds that row and leaves it out of its results.
//! Whether it also takes the row's slot from the budgets is gated on
//! `proof.unreported_empty_tree_row_charge`:
//!
//! * **[v0]**: released behaviour, `GROVE_V1`..`GROVE_V4`. Nothing is
//!   charged, so the verifier's budgets stand one slot above the prover's for
//!   every such row. Where the prover's budget is spent the verifier goes on:
//!   it refuses the honest proof for the lower layer the prover rightly left
//!   out, or reports an item past the page.
//! * **[v1]**: `GROVE_V5`+. The row is charged against the global budget and
//!   the frame's per-instance budget, exactly as the prover charges it, and
//!   the walk stops where the budget is spent.
//!
//! Gated because it flips an accepted/rejected outcome and changes the rows a
//! verifier returns for a proof it accepted before.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use super::V1LimitState;
use crate::{Error, GroveDb};

impl GroveDb {
    /// Account for a bound, empty plain `Tree` row that the verifier leaves
    /// out of its results, per the grove version.
    ///
    /// Returns whether the layer walk must stop at this row because a budget
    /// is spent.
    pub(crate) fn charge_unreported_empty_tree_row(
        limit_state: &mut V1LimitState,
        frame_instance: &mut Option<u16>,
        grove_version: &GroveVersion,
    ) -> Result<bool, Error> {
        match grove_version
            .grovedb_versions
            .operations
            .proof
            .unreported_empty_tree_row_charge
        {
            0 => Ok(Self::charge_unreported_empty_tree_row_v0()),
            1 => Ok(Self::charge_unreported_empty_tree_row_v1(
                limit_state,
                frame_instance,
            )),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "charge_unreported_empty_tree_row".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            )),
        }
    }
}
