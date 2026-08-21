//! Versioned cost accounting for the bulk-append tree.
//!
//! Only the reported hash count is versioned; chunk bytes, roots and stored
//! state are identical under every version. This gate reaches a live fee —
//! `CommitmentTree` adds the compaction hash count straight into its own
//! `hash_node_calls` — so the corrected figure arrives as a new version rather
//! than replacing the old one.

mod v0;
mod v1;

use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::BulkAppendError;

/// Hashes to report for a compacting append.
///
/// `leaf_count` is the MMR leaf count BEFORE the push (what
/// `hash_count_for_push` expects); `mmr_size_after_push` is the size the MMR
/// reached, which determines how many peaks the compaction's `get_root` had
/// to fold.
pub(crate) fn compaction_hash_count(
    leaf_count: u64,
    mmr_size_after_push: u64,
    grove_version: &GroveVersion,
) -> Result<u32, BulkAppendError> {
    match grove_version
        .bulk_append_tree_versions
        .cost
        .compaction_hash_count
    {
        0 => Ok(v0::compaction_hash_count(leaf_count)),
        1 => Ok(v1::compaction_hash_count(leaf_count, mmr_size_after_push)),
        version => Err(BulkAppendError::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "BulkAppendTree compaction hash count".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}
