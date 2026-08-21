//! Versioned hash charges for the MMR operations that merge internally.
//!
//! `push`, `get_root` and `gen_proof` all compute blake3 merges — collapsing
//! peaks on a push, folding peaks into a root or a proof. The shipped
//! accounting billed the storage reads those merges consume but not the
//! merges themselves.
//!
//! Correcting that changes `hash_node_calls`, and costs become fees, so the
//! correction cannot simply replace the old behaviour: a node replaying a
//! historical block has to charge what that block was admitted under. Each
//! charge is therefore version-dispatched, v0 being the shipped (uncharged)
//! accounting and v1 the corrected one. The values the operations return are
//! bit-identical either way.

mod v0;
mod v1;

use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::Error;

/// Hashes to charge for the peak collapses a `push` performs.
pub(crate) fn push_merge_hashes(merges: u32, grove_version: &GroveVersion) -> Result<u32, Error> {
    match grove_version.mmr_versions.cost.push {
        0 => Ok(v0::merge_hashes(merges)),
        1 => Ok(v1::merge_hashes(merges)),
        version => Err(Error::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "MMR::push hash charge".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}

/// Hashes to charge for the peak bagging a `get_root` performs.
pub(crate) fn get_root_bagging_hashes(
    peaks: usize,
    grove_version: &GroveVersion,
) -> Result<u32, Error> {
    match grove_version.mmr_versions.cost.get_root {
        0 => Ok(v0::bagging_hashes(peaks)),
        1 => Ok(v1::bagging_hashes(peaks)),
        version => Err(Error::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "MMR::get_root hash charge".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}

/// Hashes to charge for the peak bagging a `gen_proof` performs.
pub(crate) fn gen_proof_bagging_hashes(
    peaks: usize,
    grove_version: &GroveVersion,
) -> Result<u32, Error> {
    match grove_version.mmr_versions.cost.gen_proof {
        0 => Ok(v0::bagging_hashes(peaks)),
        1 => Ok(v1::bagging_hashes(peaks)),
        version => Err(Error::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "MMR::gen_proof hash charge".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}

/// Hashes a CALLER must charge for a push, on top of what
/// [`MMR::push`](crate::MMR::push) charges itself.
///
/// A caller that hashes the leaf before calling `push` — the ops layer does —
/// has to make up whatever `push` does not bill for the version in play:
///
/// - v0: `push` charges no merges, so the caller owes the leaf hash AND the
///   collapses, i.e. `hash_count_for_push`
/// - v1: `push` charges the merges, so the caller owes only the leaf hash
///
/// The invariant across both is `call_site + push == 1 + merges`, which is
/// why an MmrTree push costs the same under either version. Getting this
/// wrong in either direction double-charges or under-charges every merge.
pub fn push_call_site_hashes(leaf_count: u64, grove_version: &GroveVersion) -> Result<u32, Error> {
    match grove_version.mmr_versions.cost.push {
        0 => Ok(v0::call_site_hashes(leaf_count)),
        1 => Ok(v1::call_site_hashes(leaf_count)),
        version => Err(Error::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "MMR push call-site hash charge".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}
