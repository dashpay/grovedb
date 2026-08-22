//! Versioned cost accounting for the commitment tree.
//!
//! Only cost reporting is versioned; the frontier bytes, anchors and state
//! roots are identical under every version. The gate reaches a live fee —
//! every shielded append rewrites the frontier — so the corrected figure
//! arrives as a new version rather than replacing the old one.
//!
//! - `frontier_save_storage_accounting` — how [`CommitmentTree::save`] reports
//!   the `__ct_data__` rewrite (issue #822). v0 issues the put with no cost
//!   information, so the key and the whole frontier are charged as new
//!   storage on every append. v1 reports it as a replacement of the bytes
//!   loaded at open, with only growth added.
//!
//! [`CommitmentTree::save`]: super::CommitmentTree::save

mod v0;
mod v1;

use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::CommitmentTreeError;

/// Cost information to attach to the frontier put.
///
/// `persisted_len` is the serialized size of the frontier as loaded at open
/// (`None` when the key did not exist), `new_len` the size being written.
pub(crate) fn frontier_save_cost_info(
    persisted_len: Option<u32>,
    new_len: u32,
    grove_version: &GroveVersion,
) -> Result<Option<KeyValueStorageCost>, CommitmentTreeError> {
    match grove_version
        .commitment_tree_versions
        .cost
        .frontier_save_storage_accounting
    {
        0 => Ok(v0::frontier_save_cost_info()),
        1 => Ok(v1::frontier_save_cost_info(persisted_len, new_len)),
        version => Err(CommitmentTreeError::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "CommitmentTree frontier save storage accounting".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::{v1::GROVE_V1, v3::GROVE_V3, v4::GROVE_V4};

    use super::*;

    #[test]
    fn v0_attaches_no_cost_info() {
        for version in [&GROVE_V1, &GROVE_V3] {
            assert!(frontier_save_cost_info(None, 74, version)
                .unwrap()
                .is_none());
            assert!(frontier_save_cost_info(Some(74), 106, version)
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn v1_first_save_is_new_storage_and_rewrites_replace_with_growth_added() {
        // No committed frontier: the key is created, everything is added.
        assert!(frontier_save_cost_info(None, 74, &GROVE_V4)
            .unwrap()
            .is_none());
        // 74 -> 106 bytes (one more ommer): paid 75 replaced, 32 added.
        let c = frontier_save_cost_info(Some(74), 106, &GROVE_V4)
            .unwrap()
            .unwrap();
        assert_eq!(c.value_storage_cost.replaced_bytes, 75);
        assert_eq!(c.value_storage_cost.added_bytes, 32);
        assert!(!c.new_node);
        // 1066 -> 74 bytes (position 2^k): replaced at the new size, nothing credited.
        let c = frontier_save_cost_info(Some(1066), 74, &GROVE_V4)
            .unwrap()
            .unwrap();
        assert_eq!(c.value_storage_cost.replaced_bytes, 75);
        assert_eq!(c.value_storage_cost.added_bytes, 0);
        assert_eq!(
            c.value_storage_cost.removed_bytes,
            grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval
        );
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mut bad = GROVE_V4.clone();
        bad.commitment_tree_versions
            .cost
            .frontier_save_storage_accounting = 99;
        assert!(matches!(
            frontier_save_cost_info(Some(74), 74, &bad),
            Err(CommitmentTreeError::VersionError(_))
        ));
    }
}
