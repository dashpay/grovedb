//! `instance_cap_layer_truncation` **v0** (released behaviour: `GROVE_V4`,
//! the only earlier version that serves per-instance caps; `GROVE_V1`..
//! `GROVE_V3` refuse such a query before any proof code runs).
//!
//! A layer with subquery branches under a per-instance cap is proven under
//! the global budget only and left as the walk produced it: with no global
//! limit it carries every key its query selects (issue #962). The verifier
//! reads it at exactly that limit, so a layer that hides a selected key
//! before the limit runs out never reaches the check below; Merk refuses it.
//!
//! Kept because `GROVE_V4` is live: its proofs have to generate and verify
//! byte for byte as GroveDB 6.1.1 does. [`super::v1`] truncates the layer
//! from `GROVE_V5`.

#[cfg(feature = "minimal")]
use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::proofs::query::ProofLimitMode;

use crate::{Error, GroveDb};

impl GroveDb {
    /// `truncate_instance_capped_layer` v0: the layer stays whole.
    #[cfg(feature = "minimal")]
    pub(super) fn truncate_instance_capped_layer_v0() -> CostResult<(), Error> {
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    /// `instance_capped_layer_limit_mode` v0: the prover's exact limit.
    pub(super) fn instance_capped_layer_limit_mode_v0() -> ProofLimitMode {
        ProofLimitMode::Exact
    }

    /// `check_instance_capped_layer_stop` v0: nothing to check, an exact
    /// read never returns a layer that stopped before its limit.
    pub(super) fn check_instance_capped_layer_stop_v0() -> Result<(), Error> {
        Ok(())
    }
}
