//! `instance_cap_layer_truncation`: versioned dispatch.
//!
//! How much of a layer a V1 proof carries when the layer has subquery
//! branches and a per-instance cap (`Query::limit`) is in force there
//! (issue #962). Gated on `proof.instance_cap_layer_truncation`; the prover
//! and the verifier read the same slot.
//!
//! The cap budgets the rows found below the layer's keys, not the keys. A key
//! whose subtree is empty spends none of it, and a key whose subtree is large
//! can spend all of it, so the number of keys the walk needs is known only
//! once the descents have run. That is why the layer cannot be proven under
//! the cap the way a layer without subqueries is.
//!
//! * **[v0]**: released behaviour, `GROVE_V4` (per-instance caps are not
//!   served before it, so `GROVE_V1`..`GROVE_V3` never reach this code). The
//!   layer is proven under the global budget only. With no global limit,
//!   which is the normal case for a merged page, it carries every key its
//!   query selects and only the descents stop, so the proof grows with the
//!   range. The verifier reads the layer at exactly that limit.
//! * **[v1]**: `GROVE_V5`. Once the descents have spent the instance budget,
//!   the prover proves the layer again, truncated after the key that spent
//!   it.
//!   The verifier reads the layer as a gap-free prefix of its query, and
//!   accepts one that stops early only after it has itself seen the budget
//!   spent by the rows below the keys the layer reveals.
//!
//! # Why a truncated layer cannot drop an owed row
//!
//! The verifier reads the layer through Merk's
//! [`ProofLimitMode::UpperBound`]: the keys it gets are the first keys the
//! layer query selects in the walk's direction, with none missing between
//! them, whichever direction that is and however many query items the layer
//! has. Nothing may be revealed after the point where the layer stops.
//!
//! It then processes every revealed key exactly as it does in v0: a key with
//! no subquery is a row, a populated subtree must carry a lower layer (which
//! is verified by these same rules), and an empty subtree is bound to its
//! empty root and spends no instance budget. The budget it keeps is its own,
//! computed from the rows it verified, never read from the proof.
//!
//! A layer that stopped before its limit is accepted only if, after that, the
//! frame's instance budget or the global budget is zero. A frame whose
//! instance budget is zero owes no further row: either its own cap is spent,
//! or an enclosing cap is, and the enclosing frame settles to zero by the same
//! rows. A zero global budget ends the whole query. So every key the layer
//! hides is one whose rows the query does not ask for.
//!
//! A prover that stops earlier leaves budget unspent and is refused. One that
//! stops later only reveals keys the verifier never reads. A layer that stops
//! exactly where the global budget truncates it is the v0 shape and is
//! accepted as before.
//!
//! # When the v1 prover keeps the layer whole
//!
//! The cut relies on prover and verifier counting the same rows against the
//! instance budget. Under `GROVE_V5` they do: the one row the two sides used
//! to count differently, an empty plain `Tree` that is itself a result row
//! and that a default verifier does not report, is charged by the verifier
//! too from the same version (`proof.unreported_empty_tree_row_charge`). The
//! truncation is meant to be selected together with that slot.
//!
//! The prover keeps a layer whole when the instance budget outlasts it, when
//! the global budget ends the walk first (the global limit already bounds
//! that layer, and the two sides do not charge it identically for every
//! empty descent), and when the rows before the cut exceed what a Merk limit
//! (a `u16`) can express.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

#[cfg(feature = "minimal")]
use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::proofs::query::ProofLimitMode;
#[cfg(feature = "minimal")]
use grovedb_merk::{proofs::query::QueryItem, store::NodeStore, Merk, ProofWithoutEncodingResult};
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

#[cfg(feature = "minimal")]
use crate::Engine;
use crate::{Error, GroveDb, PathQuery};

fn unknown_version(received: u16) -> Error {
    Error::VersionError(GroveVersionError::UnknownVersionMismatch {
        method: "instance_cap_layer_truncation".to_string(),
        known_versions: vec![0, 1],
        received,
    })
}

#[cfg(feature = "minimal")]
impl<E: Engine> GroveDb<E> {
    /// Prover side. Called once the walk of a layer with subquery branches
    /// under a per-instance cap has run its descents.
    ///
    /// `merk_proof` is the layer as it was walked, with the nodes of the rows
    /// the walk processed already rewritten.
    /// `first_op_past_the_instance_budget` is the index of the first op the
    /// walk reached with a spent instance budget, `None` when that budget
    /// outlasted the layer or the global budget ended the walk first. From v1
    /// the layer is replaced by one that reveals nothing after the last row
    /// within the budget.
    pub(crate) fn truncate_instance_capped_layer<'a, S>(
        &self,
        subtree: &Merk<S>,
        query_items: &[QueryItem],
        left_to_right: bool,
        merk_proof: &mut ProofWithoutEncodingResult,
        first_op_past_the_instance_budget: Option<usize>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        S: NodeStore<'a> + 'a,
    {
        match grove_version
            .grovedb_versions
            .operations
            .proof
            .instance_cap_layer_truncation
        {
            0 => GroveDb::truncate_instance_capped_layer_v0(),
            1 => self.truncate_instance_capped_layer_v1(
                subtree,
                query_items,
                left_to_right,
                merk_proof,
                first_op_past_the_instance_budget,
                grove_version,
            ),
            version => Err(unknown_version(version)).wrap_with_cost(OperationCost::default()),
        }
    }
}

impl GroveDb {
    /// Verifier side. How the Merk proof of a layer with subquery branches
    /// under a per-instance cap is read: at exactly the layer's limit (v0),
    /// or as a gap-free prefix that may stop before it (v1).
    pub(crate) fn instance_capped_layer_limit_mode(
        grove_version: &GroveVersion,
    ) -> Result<ProofLimitMode, Error> {
        match grove_version
            .grovedb_versions
            .operations
            .proof
            .instance_cap_layer_truncation
        {
            0 => Ok(Self::instance_capped_layer_limit_mode_v0()),
            1 => Ok(Self::instance_capped_layer_limit_mode_v1()),
            version => Err(unknown_version(version)),
        }
    }

    /// Verifier side. Called after the rows of a layer with subquery branches
    /// under a per-instance cap have been verified, descents included.
    ///
    /// `stopped_before_its_limit` says the layer's Merk proof stopped
    /// revealing its query before the limit it was read at ran out.
    /// `budget_spent` is the verifier's own accounting after those rows: the
    /// frame's instance budget or the global budget is zero.
    pub(crate) fn check_instance_capped_layer_stop(
        query: &PathQuery,
        stopped_before_its_limit: bool,
        budget_spent: bool,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .proof
            .instance_cap_layer_truncation
        {
            0 => Self::check_instance_capped_layer_stop_v0(),
            1 => Self::check_instance_capped_layer_stop_v1(
                query,
                stopped_before_its_limit,
                budget_spent,
            ),
            version => Err(unknown_version(version)),
        }
    }
}
