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
//! - `frontier_cost_model` — whether the frontier's per-append cost is the
//!   actual work of the position (v0: `32 + trailing_ones(position)`
//!   Sinsemilla hashes, the frontier's actual size loaded and saved) or a
//!   fixed model derived from the depth (v1: `depth + 1` hashes, a
//!   `42 + 32 · depth/2`-byte frontier), so every append costs the same.
//!   When set it also decides the save's storage accounting — a replacement
//!   of the model size — taking precedence over the gate above.
//!
//! [`CommitmentTree::save`]: super::CommitmentTree::save

mod v0;
mod v1;

use grovedb_costs::storage_cost::key_value_cost::KeyValueStorageCost;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use crate::CommitmentTreeError;

/// Whether `grove_version` charges the frontier at its fixed model.
pub(crate) fn frontier_cost_model(
    grove_version: &GroveVersion,
) -> Result<bool, CommitmentTreeError> {
    match grove_version
        .commitment_tree_versions
        .cost
        .frontier_cost_model
    {
        0 => Ok(false),
        1 => Ok(true),
        version => Err(CommitmentTreeError::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "CommitmentTree frontier cost model".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}

/// Cost information to attach to the frontier put.
///
/// `persisted_len` is the serialized size of the frontier as loaded at open
/// (`None` when the key did not exist), `new_len` the size being written.
/// Under the fixed frontier cost model the put is a replacement of the model
/// size whatever was there; otherwise the save accounting gate decides.
pub(crate) fn frontier_save_cost_info(
    persisted_len: Option<u32>,
    new_len: u32,
    grove_version: &GroveVersion,
) -> Result<Option<KeyValueStorageCost>, CommitmentTreeError> {
    let actual = match grove_version
        .commitment_tree_versions
        .cost
        .frontier_save_storage_accounting
    {
        0 => v0::frontier_save_cost_info(),
        1 => v1::frontier_save_cost_info(persisted_len, new_len),
        version => {
            return Err(CommitmentTreeError::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "CommitmentTree frontier save storage accounting".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                }
                .to_string(),
            ))
        }
    };
    if frontier_cost_model(grove_version)? {
        // A replacement of the model size, whatever the bytes written — so
        // the commit path must not verify the figure against them.
        // 554 bytes: a two-byte length varint.
        let paid = crate::MODEL_FRONTIER_SERIALIZED_LEN + 2;
        return Ok(Some(KeyValueStorageCost {
            key_storage_cost: grovedb_costs::storage_cost::StorageCost::default(),
            value_storage_cost: grovedb_costs::storage_cost::StorageCost {
                added_bytes: 0,
                replaced_bytes: paid,
                removed_bytes:
                    grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
            },
            new_node: false,
            needs_value_verification: false,
        }));
    }
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::{v1::GROVE_V1, v2::GROVE_V2, v3::GROVE_V3, v4::GROVE_V4};

    use super::*;

    #[test]
    fn v0_attaches_no_cost_info() {
        for version in [&GROVE_V1, &GROVE_V2, &GROVE_V3] {
            assert!(frontier_save_cost_info(None, 74, version)
                .unwrap()
                .is_none());
            assert!(frontier_save_cost_info(Some(74), 106, version)
                .unwrap()
                .is_none());
        }
    }

    /// GROVE_V4 with the fixed frontier cost model switched off: the
    /// save-accounting gate alone.
    fn v4_actual_frontier() -> GroveVersion {
        let mut version = GROVE_V4.clone();
        version.commitment_tree_versions.cost.frontier_cost_model = 0;
        version
    }

    #[test]
    fn save_accounting_v1_first_save_is_new_storage_and_rewrites_replace_with_growth_added() {
        let version = v4_actual_frontier();
        // No committed frontier: the key is created, everything is added.
        assert!(frontier_save_cost_info(None, 74, &version)
            .unwrap()
            .is_none());
        // 74 -> 106 bytes (one more ommer): paid 75 replaced, 32 added.
        let c = frontier_save_cost_info(Some(74), 106, &version)
            .unwrap()
            .unwrap();
        assert_eq!(c.value_storage_cost.replaced_bytes, 75);
        assert_eq!(c.value_storage_cost.added_bytes, 32);
        assert!(!c.new_node);
        // 1066 -> 74 bytes (position 2^k): replaced at the new size, nothing credited.
        let c = frontier_save_cost_info(Some(1066), 74, &version)
            .unwrap()
            .unwrap();
        assert_eq!(c.value_storage_cost.replaced_bytes, 75);
        assert_eq!(c.value_storage_cost.added_bytes, 0);
        assert_eq!(
            c.value_storage_cost.removed_bytes,
            grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval
        );
    }

    /// GROVE_V4: the fixed frontier cost model — every save, the first
    /// included, is a replacement of the model size, whatever was loaded and
    /// whatever is written.
    #[test]
    fn v4_saves_replace_the_model_size() {
        let paid = crate::MODEL_FRONTIER_SERIALIZED_LEN + 2;
        for (persisted, new_len) in [(None, 74), (Some(74), 106), (Some(1066), 74)] {
            let c = frontier_save_cost_info(persisted, new_len, &GROVE_V4)
                .unwrap()
                .unwrap();
            assert_eq!(c.value_storage_cost.replaced_bytes, paid);
            assert_eq!(c.value_storage_cost.added_bytes, 0);
            assert!(!c.new_node);
        }
        assert_eq!(crate::MODEL_FRONTIER_SERIALIZED_LEN, 554);
        assert_eq!(crate::MODEL_FRONTIER_APPEND_SINSEMILLA_HASHES, 33);
    }

    #[test]
    fn unknown_version_is_rejected() {
        let mut bad = v4_actual_frontier();
        bad.commitment_tree_versions
            .cost
            .frontier_save_storage_accounting = 99;
        assert!(matches!(
            frontier_save_cost_info(Some(74), 74, &bad),
            Err(CommitmentTreeError::VersionError(_))
        ));
        let mut bad = GROVE_V4.clone();
        bad.commitment_tree_versions.cost.frontier_cost_model = 99;
        assert!(matches!(
            frontier_save_cost_info(Some(74), 74, &bad),
            Err(CommitmentTreeError::VersionError(_))
        ));
        assert!(matches!(
            frontier_cost_model(&bad),
            Err(CommitmentTreeError::VersionError(_))
        ));
    }
}
