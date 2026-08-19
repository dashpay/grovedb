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
