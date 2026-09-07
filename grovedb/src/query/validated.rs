//! Borrowed validation shared by the read, prove and verify dispatchers.
//!
//! Shape grammar lives in `PathQuery::classify`; this module adds only the
//! consumer's version/envelope eligibility. It performs no storage access,
//! serialization, query rewriting, or cost accounting. Tree types and proof
//! contents must still be checked by the execution/verification walkers.
//!
//! The legacy result-specific APIs retain their narrower contracts: scalar
//! aggregate APIs require leaves, element APIs cannot return read-mode
//! results, ordinary reads permit offsets that proofs cannot attest, and
//! absence/parent-info APIs have their own limit/offset requirements. Those
//! restrictions are not additional query shapes.

#[cfg(feature = "minimal")]
use grovedb_version::error::GroveVersionError;
use grovedb_version::version::GroveVersion;

use super::{AggregateKind, PathQueryShape};
use crate::{operations::proof::GroveDBProof, Error, PathQuery};

/// A query and its validated, allocation-free shape at a particular version.
/// Aggregate kind and inner range are retained in `shape`, not rediscovered
/// from the query by consumers. This type is deliberately not serialized.
pub(crate) struct ValidatedPathQuery<'q> {
    query: &'q PathQuery,
    shape: PathQueryShape<'q>,
    grove_version: &'q GroveVersion,
}

impl<'q> ValidatedPathQuery<'q> {
    pub(crate) fn new(
        query: &'q PathQuery,
        grove_version: &'q GroveVersion,
    ) -> Result<Self, Error> {
        Ok(Self {
            query,
            shape: query.classify()?,
            grove_version,
        })
    }

    #[cfg(feature = "minimal")]
    pub(crate) fn for_read(
        query: &'q PathQuery,
        grove_version: &'q GroveVersion,
    ) -> Result<Self, Error> {
        let validated = Self::new(query, grove_version)?;
        if matches!(
            validated.shape,
            PathQueryShape::AxisRead { .. }
                | PathQueryShape::BranchedAxisRead { .. }
                | PathQueryShape::SumBudget { .. }
        ) {
            match grove_version.grovedb_versions.path_query_methods.unified_read_mode {
                0 => return Err(Error::NotSupported(
                    "read-mode (axis / sum-budget) path queries are not served at this grove version"
                        .to_string(),
                )),
                1 => {}
                received => return Err(Error::VersionError(
                    GroveVersionError::UnknownVersionMismatch {
                        method: "run_path_query (unified_read_mode)".to_string(),
                        known_versions: vec![0, 1],
                        received,
                    },
                )),
            }
        }
        Ok(validated)
    }

    #[cfg(feature = "minimal")]
    pub(crate) fn for_proof(
        query: &'q PathQuery,
        grove_version: &'q GroveVersion,
    ) -> Result<Self, Error> {
        // Preserve the current prover's per-instance gate before shape
        // validation, including its historical error precedence.
        query.reject_unserved_per_instance_limits(grove_version)?;
        let prove_version = grove_version
            .grovedb_versions
            .operations
            .proof
            .prove_query_non_serialized;
        let shape = query.classify_with_offset_gate(|| match prove_version {
            // V0 rejected offsets before inspecting pagination syntax.
            0 => {
                query.reject_per_instance_limits("the V0 prover")?;
                Err(Error::InvalidQuery(
                    "proved path queries can not have offsets",
                ))
            }
            1 => Ok(()),
            received => Err(unknown_prove_version(received)),
        })?;
        let validated = Self {
            query,
            shape,
            grove_version,
        };
        if prove_version == 0 {
            let message = match shape {
                PathQueryShape::AxisRead { .. } | PathQueryShape::BranchedAxisRead { .. } => Some(
                    "axis-ordered path queries require V1 proof envelopes; upgrade the grove version producing the proof",
                ),
                PathQueryShape::SumBudget { .. } => Some(
                    "sum-budget path queries require V1 proof envelopes; upgrade the grove version producing the proof",
                ),
                _ => match shape.aggregate_kind() {
                    Some(AggregateKind::Count) => Some("AggregateCountOnRange proofs require V1 proof envelopes; upgrade the grove version producing the proof"),
                    Some(AggregateKind::Sum) => Some("AggregateSumOnRange proofs require V1 proof envelopes; upgrade the grove version producing the proof"),
                    Some(AggregateKind::CountAndSum) => Some("AggregateCountAndSumOnRange proofs require V1 proof envelopes; upgrade the grove version producing the proof"),
                    None => None,
                },
            };
            if let Some(message) = message {
                return Err(Error::NotSupported(message.to_string()));
            }
        }
        validated.check_proof_capabilities(true)?;
        if prove_version == 0 {
            query.reject_per_instance_limits("the V0 prover")?;
        }
        if prove_version > 1 {
            return Err(unknown_prove_version(prove_version));
        }
        // limit == 0 is checked by the prover AFTER the count-offset
        // target-tree open. Moving it here would change operation costs.
        Ok(validated)
    }

    pub(crate) fn for_verification(
        query: &'q PathQuery,
        grove_version: &'q GroveVersion,
    ) -> Result<Self, Error> {
        let validated = Self::new(query, grove_version)?;
        validated.check_proof_capabilities(false)?;
        Ok(validated)
    }

    fn check_proof_capabilities(&self, generating: bool) -> Result<(), Error> {
        let versions = &self.grove_version.grovedb_versions.operations.proof;
        let message = match self.shape {
            PathQueryShape::AxisRead { .. } | PathQueryShape::BranchedAxisRead { .. }
                if versions.axis_descent_in_v1_envelope != 1 =>
            {
                Some(if generating {
                    "axis-ordered descents in the V1 proof envelope are not emitted at this grove version"
                } else {
                    "axis-ordered descents in the V1 proof envelope are not accepted at this grove version"
                })
            }
            PathQueryShape::SumBudget { .. } if versions.sum_budget_in_v1_envelope != 1 => {
                Some(if generating {
                    "sum-budget windows in the V1 proof envelope are not emitted at this grove version"
                } else {
                    "sum-budget windows in the V1 proof envelope are not accepted at this grove version"
                })
            }
            _ => None,
        };
        match message {
            Some(message) => Err(Error::NotSupported(message.to_string())),
            None => Ok(()),
        }
    }

    /// Called after decoding; actual layer/tree eligibility is still the
    /// verifier's responsibility. Keep the existing per-family error types.
    pub(crate) fn check_envelope(&self, proof: &GroveDBProof) -> Result<(), Error> {
        if matches!(proof, GroveDBProof::V1(_)) {
            return Ok(());
        }
        let message = match self.shape {
            PathQueryShape::KeySelection => return Ok(()),
            PathQueryShape::CountOffsetPaginated { .. } => {
                "offsets in path queries are not supported for proofs"
            }
            PathQueryShape::AxisRead { .. } | PathQueryShape::BranchedAxisRead { .. } => {
                "axis-ordered path queries require V1 proof envelopes"
            }
            PathQueryShape::SumBudget { .. } => {
                "sum-budget path queries require V1 proof envelopes"
            }
            PathQueryShape::AggregateLeaf { kind, .. }
            | PathQueryShape::AggregateCarrier { kind, .. } => {
                let name = match kind {
                    AggregateKind::Count => "AggregateCountOnRange",
                    AggregateKind::Sum => "AggregateSumOnRange",
                    AggregateKind::CountAndSum => "AggregateCountAndSumOnRange",
                };
                return Err(Error::InvalidProof(self.query.clone(), format!(
                    "{name} proofs require V1 proof envelopes; V0 envelopes predate this feature and cannot legitimately carry such a proof"
                )));
            }
        };
        Err(Error::NotSupported(message.to_string()))
    }

    pub(crate) fn query(&self) -> &'q PathQuery {
        self.query
    }

    pub(crate) fn shape(&self) -> PathQueryShape<'q> {
        self.shape
    }

    /// The aggregate terminal's depth is fixed by the leaf/carrier grammar.
    /// The prover still opens and checks the actual tree at this depth.
    #[cfg(feature = "minimal")]
    pub(crate) fn aggregate_at_depth(
        &self,
        depth: usize,
    ) -> Option<(AggregateKind, &'q grovedb_merk::proofs::query::QueryItem)> {
        match self.shape {
            PathQueryShape::AggregateLeaf { kind, inner } if depth == self.query.path.len() => {
                Some((kind, inner))
            }
            PathQueryShape::AggregateCarrier { kind, inner }
                if depth
                    == self.query.path.len()
                        + 1
                        + self
                            .query
                            .query
                            .query
                            .default_subquery_branch
                            .subquery_path
                            .as_ref()
                            .map_or(0, Vec::len) =>
            {
                Some((kind, inner))
            }
            _ => None,
        }
    }

    #[cfg(feature = "minimal")]
    pub(crate) fn proof_version(&self) -> u16 {
        self.grove_version
            .grovedb_versions
            .operations
            .proof
            .prove_query_non_serialized
    }
}

#[cfg(feature = "minimal")]
fn unknown_prove_version(received: u16) -> Error {
    Error::VersionError(GroveVersionError::UnknownVersionMismatch {
        method: "prove_query_non_serialized".to_string(),
        known_versions: vec![0, 1],
        received,
    })
}
