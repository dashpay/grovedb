//! The budget slot of an unreported empty tree row: **v0** (released
//! behaviour, `GROVE_V1`..`GROVE_V4`).
//!
//! The row is bound and skipped, and no budget moves. The prover charged it
//! one row, so from here on the verifier's budgets stand one slot above the
//! prover's. This is a known mismatch, kept because `GROVE_V4` is live: a
//! verifier that charged the row would accept proofs a released verifier
//! refuses and return fewer rows for some it accepts. [`super::v1`] charges
//! it from `GROVE_V5`.

use crate::GroveDb;

impl GroveDb {
    /// `charge_unreported_empty_tree_row` v0: charges nothing and never stops
    /// the walk. See the module documentation.
    pub(super) fn charge_unreported_empty_tree_row_v0() -> bool {
        false
    }
}
