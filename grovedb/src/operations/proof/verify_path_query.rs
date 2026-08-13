//! The unified verify entry point: one function that verifies a proof
//! for every provable [`PathQuery`] shape.
//!
//! [`GroveDb::verify_path_query`] classifies the query once
//! ([`PathQuery::classify`]) and routes it to the verifier that serves
//! that shape, returning a [`VerifiedPathQuery`] whose variant mirrors
//! the shape — the proved counterpart of `run_path_query`.
//!
//! Axis shapes verify **GroveDBProof V1** envelopes carrying
//! [`ProofBytes::IndexedTreeAxisDescent`](crate::operations::proof::ProofBytes)
//! layers, gated on `proof.axis_descent_in_v1_envelope` (GROVE_V4+).
//! Sum-budget shapes verify V1 envelopes carrying
//! [`ProofBytes::SumBudgetWindow`](crate::operations::proof::ProofBytes)
//! layers, gated on `proof.sum_budget_in_v1_envelope` (GROVE_V4+), with
//! the stop condition attested by the verifier's fold replay.
//! Key-selection and aggregate shapes route to the existing verifiers
//! unchanged.

use grovedb_merk::{proofs::query::AxisTraversal, CryptoHash};
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::{
        decode_grovedb_proof_canonical,
        indexed_axis::AxisEntries,
        verify::{AxisWalkOutcome, AxisWalkResult},
        GroveDBProof,
    },
    query::{AggregateKind, PathQueryShape},
    query_result_type::PathKeyOptionalElementTrio,
    Error, GroveDb, PathQuery,
};

pub use crate::operations::proof::verify::SumBudgetStop;

/// The verified answer to any provable [`PathQuery`] shape — one
/// variant per shape family. Every variant carries the reconstructed
/// GroveDB root hash; compare it against the root you trust —
/// verification alone proves internal consistency, not that the proof
/// is about the state you meant.
#[derive(Debug)]
pub enum VerifiedPathQuery {
    /// Key-selection shapes: the regular trio result set.
    Elements {
        /// Reconstructed GroveDB root hash.
        root_hash: CryptoHash,
        /// The proved `(path, key, element)` rows.
        elements: Vec<PathKeyOptionalElementTrio>,
    },
    /// Leaf `AggregateCountOnRange`.
    AggregateCount {
        /// Reconstructed GroveDB root hash.
        root_hash: CryptoHash,
        /// The proved count.
        count: u64,
    },
    /// Leaf `AggregateSumOnRange`.
    AggregateSum {
        /// Reconstructed GroveDB root hash.
        root_hash: CryptoHash,
        /// The proved signed sum.
        sum: i64,
    },
    /// Leaf `AggregateCountAndSumOnRange`.
    AggregateCountAndSum {
        /// Reconstructed GroveDB root hash.
        root_hash: CryptoHash,
        /// The proved count.
        count: u64,
        /// The proved signed sum.
        sum: i64,
    },
    /// Carrier aggregates: one value per matched outer key.
    AggregatePerKey {
        /// Reconstructed GroveDB root hash.
        root_hash: CryptoHash,
        /// Per outer key: the proved `(count, sum)` — count-only
        /// carriers report `sum = None`, sum-only carriers report
        /// `count = None`.
        per_key: Vec<(Vec<u8>, Option<u64>, Option<i64>)>,
    },
    /// Single-path axis read (`TopK` / `Bounded`): the proved entries.
    AxisEntries {
        /// Reconstructed GroveDB root hash.
        root_hash: CryptoHash,
        /// The proved entries, in walk order.
        entries: AxisEntries,
        /// Count-commitment-attested skip for paginated traversals;
        /// `None` for bounded ones.
        skipped: Option<u64>,
    },
    /// Branched axis read: per branch key, in query order, the proved
    /// entries — or `None` for a branch key whose absence the
    /// branching-level Merk proof authenticates.
    BranchedAxisEntries {
        /// Reconstructed GroveDB root hash.
        root_hash: CryptoHash,
        /// `(branch_key, entries)` per branch, `None` = proven absent.
        branches: Vec<(Vec<u8>, Option<AxisEntries>)>,
    },
    /// `RankOfKey`: the attested 0-based rank of the queried key in the
    /// directional walk.
    AxisRank {
        /// Reconstructed GroveDB root hash.
        root_hash: CryptoHash,
        /// The attested rank.
        rank: u64,
    },
    /// `RangeAggregate`: the attested aggregate over the value range.
    AxisAggregate {
        /// Reconstructed GroveDB root hash.
        root_hash: CryptoHash,
        /// The attested aggregate: a count (`>= 0`) for the count axis,
        /// a signed sum for the sum axis.
        value: i128,
    },
    /// Sum-budget read: the proved window's matched sum items, their
    /// net total, and the replay-attested stop condition.
    SumBudget {
        /// Reconstructed GroveDB root hash.
        root_hash: CryptoHash,
        /// The matched `(key, value)` pairs, in walk order.
        matches: Vec<(Vec<u8>, i64)>,
        /// Net total of the matched values (saturating).
        total: i64,
        /// Why the walk stopped, attested by the fold replay.
        stop: SumBudgetStop,
    },
}

impl VerifiedPathQuery {
    /// The reconstructed GroveDB root hash, whichever shape this is.
    pub fn root_hash(&self) -> &CryptoHash {
        match self {
            VerifiedPathQuery::Elements { root_hash, .. }
            | VerifiedPathQuery::AggregateCount { root_hash, .. }
            | VerifiedPathQuery::AggregateSum { root_hash, .. }
            | VerifiedPathQuery::AggregateCountAndSum { root_hash, .. }
            | VerifiedPathQuery::AggregatePerKey { root_hash, .. }
            | VerifiedPathQuery::AxisEntries { root_hash, .. }
            | VerifiedPathQuery::BranchedAxisEntries { root_hash, .. }
            | VerifiedPathQuery::AxisRank { root_hash, .. }
            | VerifiedPathQuery::AxisAggregate { root_hash, .. }
            | VerifiedPathQuery::SumBudget { root_hash, .. } => root_hash,
        }
    }
}

impl GroveDb {
    /// Verify `proof` against `path_query`, whatever shape the query
    /// is, returning the shape's typed verified answer. The unified
    /// counterpart of the per-shape `verify_*` entry points and the
    /// proved counterpart of `run_path_query`.
    pub fn verify_path_query(
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<VerifiedPathQuery, Error> {
        match path_query.classify()? {
            PathQueryShape::KeySelection | PathQueryShape::CountOffsetPaginated { .. } => {
                let (root_hash, elements) = Self::verify_query(proof, path_query, grove_version)?;
                Ok(VerifiedPathQuery::Elements {
                    root_hash,
                    elements,
                })
            }
            PathQueryShape::AggregateLeaf { kind, .. } => match kind {
                AggregateKind::Count => {
                    let (root_hash, count) =
                        Self::verify_aggregate_count_query(proof, path_query, grove_version)?;
                    Ok(VerifiedPathQuery::AggregateCount { root_hash, count })
                }
                AggregateKind::Sum => {
                    let (root_hash, sum) =
                        Self::verify_aggregate_sum_query(proof, path_query, grove_version)?;
                    Ok(VerifiedPathQuery::AggregateSum { root_hash, sum })
                }
                AggregateKind::CountAndSum => {
                    let (root_hash, count, sum) = Self::verify_aggregate_count_and_sum_query(
                        proof,
                        path_query,
                        grove_version,
                    )?;
                    Ok(VerifiedPathQuery::AggregateCountAndSum {
                        root_hash,
                        count,
                        sum,
                    })
                }
            },
            PathQueryShape::AggregateCarrier { kind, .. } => match kind {
                AggregateKind::Count => {
                    let (root_hash, per_key) = Self::verify_aggregate_count_query_per_key(
                        proof,
                        path_query,
                        grove_version,
                    )?;
                    Ok(VerifiedPathQuery::AggregatePerKey {
                        root_hash,
                        per_key: per_key
                            .into_iter()
                            .map(|(key, count)| (key, Some(count), None))
                            .collect(),
                    })
                }
                AggregateKind::Sum => {
                    let (root_hash, per_key) =
                        Self::verify_aggregate_sum_query_per_key(proof, path_query, grove_version)?;
                    Ok(VerifiedPathQuery::AggregatePerKey {
                        root_hash,
                        per_key: per_key
                            .into_iter()
                            .map(|(key, sum)| (key, None, Some(sum)))
                            .collect(),
                    })
                }
                AggregateKind::CountAndSum => {
                    let (root_hash, per_key) = Self::verify_aggregate_count_and_sum_query_per_key(
                        proof,
                        path_query,
                        grove_version,
                    )?;
                    Ok(VerifiedPathQuery::AggregatePerKey {
                        root_hash,
                        per_key: per_key
                            .into_iter()
                            .map(|(key, count, sum)| (key, Some(count), Some(sum)))
                            .collect(),
                    })
                }
            },
            PathQueryShape::AxisRead { axis } => {
                let (root_hash, _, outcomes) =
                    Self::verify_axis_shape_walk(proof, path_query, grove_version)?;
                let [outcome]: [AxisWalkOutcome; 1] =
                    outcomes.try_into().map_err(|outcomes: Vec<_>| {
                        Error::InvalidProof(
                            path_query.clone(),
                            format!(
                                "a single-path axis read must verify exactly one axis layer, \
                                 got {}",
                                outcomes.len()
                            ),
                        )
                    })?;
                if outcome.path != path_query.path {
                    return Err(Error::InvalidProof(
                        path_query.clone(),
                        "the verified axis layer is not at the queried path".to_string(),
                    ));
                }
                Self::outcome_into_verified(root_hash, outcome.result, &axis.traversal, path_query)
            }
            PathQueryShape::BranchedAxisRead {
                branch_items,
                suffix,
                axis,
            } => {
                let (root_hash, trios, outcomes) =
                    Self::verify_axis_shape_walk(proof, path_query, grove_version)?;

                // Index outcomes by their branch key (the segment right
                // after the branching prefix).
                let prefix_len = path_query.path.len();
                let mut by_branch_key: std::collections::BTreeMap<Vec<u8>, AxisWalkResult> =
                    std::collections::BTreeMap::new();
                for outcome in outcomes {
                    let expected_len = prefix_len + 1 + suffix.len();
                    if outcome.path.len() != expected_len
                        || outcome.path[..prefix_len]
                            .iter()
                            .zip(&path_query.path)
                            .any(|(a, b)| a != b)
                        || outcome.path[prefix_len + 1..]
                            .iter()
                            .zip(suffix)
                            .any(|(a, b)| a != b)
                    {
                        return Err(Error::InvalidProof(
                            path_query.clone(),
                            "a verified axis layer sits outside the branched query's \
                             prefix/branch/suffix structure"
                                .to_string(),
                        ));
                    }
                    let branch_key = outcome.path[prefix_len].clone();
                    if by_branch_key.insert(branch_key, outcome.result).is_some() {
                        return Err(Error::InvalidProof(
                            path_query.clone(),
                            "duplicate axis layers for one branch key".to_string(),
                        ));
                    }
                }

                // Present-element trios anywhere under a branch key that
                // produced no outcome would mean the prover proved the
                // branch present but omitted its axis layer — hiding
                // entries behind a fake absence. Reject.
                let mut branches = Vec::with_capacity(branch_items.len());
                for item in branch_items {
                    let grovedb_merk::proofs::query::query_item::QueryItem::Key(branch_key) = item
                    else {
                        return Err(Error::CorruptedCodeExecution(
                            "branched axis read classified with a non-Key branch item",
                        ));
                    };
                    match by_branch_key.remove(branch_key) {
                        Some(AxisWalkResult::Entries { entries, .. }) => {
                            branches.push((branch_key.clone(), Some(entries)));
                        }
                        Some(_) => {
                            return Err(Error::InvalidProof(
                                path_query.clone(),
                                "branched axis reads verify entry-listing traversals only"
                                    .to_string(),
                            ));
                        }
                        None => {
                            // No axis layer for this branch: the
                            // branching-level Merk proof must have
                            // authenticated the key's ABSENCE. Any
                            // present-element trio for the branch key
                            // itself — or anywhere under its subtree —
                            // means the proof shows the branch present
                            // while withholding its axis layer, i.e. a
                            // hidden branch dressed up as absence.
                            let present = trios.iter().any(|(trio_path, trio_key, element)| {
                                if element.is_none() {
                                    return false;
                                }
                                // The branch key at the branching level…
                                (trio_path == &path_query.path && trio_key == branch_key)
                                    // …or anything below prefix/branch_key.
                                    || (trio_path.len() > path_query.path.len()
                                        && trio_path[..path_query.path.len()] == path_query.path
                                        && trio_path[path_query.path.len()] == *branch_key)
                            });
                            if present {
                                return Err(Error::InvalidProof(
                                    path_query.clone(),
                                    format!(
                                        "branch key {} is proven present but its axis layer \
                                         is missing from the proof",
                                        hex::encode(branch_key),
                                    ),
                                ));
                            }
                            branches.push((branch_key.clone(), None));
                        }
                    }
                }
                if !by_branch_key.is_empty() {
                    return Err(Error::InvalidProof(
                        path_query.clone(),
                        "the proof carries axis layers for branch keys the query does not name"
                            .to_string(),
                    ));
                }
                // The branched grammar admits only entry-listing
                // traversals downstream; reject aggregates/ranks here
                // for symmetry with the prover.
                if matches!(
                    axis.traversal,
                    AxisTraversal::RankOfKey { .. } | AxisTraversal::RangeAggregate { .. }
                ) {
                    return Err(Error::NotSupported(
                        "branched axis reads serve entry-listing traversals (TopK / Bounded) \
                         only"
                            .to_string(),
                    ));
                }
                Ok(VerifiedPathQuery::BranchedAxisEntries {
                    root_hash,
                    branches,
                })
            }
            PathQueryShape::SumBudget { .. } => {
                if grove_version
                    .grovedb_versions
                    .operations
                    .proof
                    .sum_budget_in_v1_envelope
                    != 1
                {
                    return Err(Error::NotSupported(
                        "sum-budget windows in the V1 proof envelope are not accepted at this \
                         grove version"
                            .to_string(),
                    ));
                }
                let decoded = decode_grovedb_proof_canonical(proof)?;
                let GroveDBProof::V1(proof_v1) = decoded else {
                    return Err(Error::NotSupported(
                        "sum-budget path queries require V1 proof envelopes".to_string(),
                    ));
                };
                let (root_hash, _, outcomes) =
                    Self::verify_proof_v1_with_axis_outcomes(&proof_v1, path_query, grove_version)?;
                let [outcome]: [AxisWalkOutcome; 1] =
                    outcomes.try_into().map_err(|outcomes: Vec<_>| {
                        Error::InvalidProof(
                            path_query.clone(),
                            format!(
                                "a sum-budget read must verify exactly one window layer, got {}",
                                outcomes.len()
                            ),
                        )
                    })?;
                if outcome.path != path_query.path {
                    return Err(Error::InvalidProof(
                        path_query.clone(),
                        "the verified sum-budget window is not at the queried path".to_string(),
                    ));
                }
                let AxisWalkResult::SumBudget {
                    matches,
                    total,
                    stop,
                } = outcome.result
                else {
                    return Err(Error::InvalidProof(
                        path_query.clone(),
                        "the verified outcome does not match the query's sum-budget shape"
                            .to_string(),
                    ));
                };
                Ok(VerifiedPathQuery::SumBudget {
                    root_hash,
                    matches,
                    total,
                    stop,
                })
            }
        }
    }

    /// Decode + envelope-gate + walk for the axis shapes.
    fn verify_axis_shape_walk(
        proof: &[u8],
        path_query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<
        (
            CryptoHash,
            Vec<PathKeyOptionalElementTrio>,
            Vec<AxisWalkOutcome>,
        ),
        Error,
    > {
        if grove_version
            .grovedb_versions
            .operations
            .proof
            .axis_descent_in_v1_envelope
            != 1
        {
            return Err(Error::NotSupported(
                "axis-ordered descents in the V1 proof envelope are not accepted at this \
                 grove version"
                    .to_string(),
            ));
        }
        let decoded = decode_grovedb_proof_canonical(proof)?;
        match decoded {
            GroveDBProof::V0(_) => Err(Error::NotSupported(
                "axis-ordered path queries require V1 proof envelopes".to_string(),
            )),
            GroveDBProof::V1(proof_v1) => {
                Self::verify_proof_v1_with_axis_outcomes(&proof_v1, path_query, grove_version)
            }
        }
    }

    /// Map a single-path axis outcome into the public result, checking
    /// the outcome family matches the traversal.
    fn outcome_into_verified(
        root_hash: CryptoHash,
        result: AxisWalkResult,
        traversal: &AxisTraversal,
        path_query: &PathQuery,
    ) -> Result<VerifiedPathQuery, Error> {
        match (result, traversal) {
            (
                AxisWalkResult::Entries { entries, skipped },
                AxisTraversal::TopK { .. } | AxisTraversal::Bounded { .. },
            ) => Ok(VerifiedPathQuery::AxisEntries {
                root_hash,
                entries,
                skipped,
            }),
            (AxisWalkResult::Rank { rank }, AxisTraversal::RankOfKey { .. }) => {
                Ok(VerifiedPathQuery::AxisRank { root_hash, rank })
            }
            (AxisWalkResult::Aggregate { value }, AxisTraversal::RangeAggregate { .. }) => {
                Ok(VerifiedPathQuery::AxisAggregate { root_hash, value })
            }
            _ => Err(Error::InvalidProof(
                path_query.clone(),
                "the verified axis outcome does not match the query's traversal".to_string(),
            )),
        }
    }
}
