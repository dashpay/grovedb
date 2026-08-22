//! Versioned root maintenance: how an insert leaves the tree able to produce
//! its root, and how a root read derives it.
//!
//! The root VALUE is the same under every version — every node hashes as
//! `blake3(H(value) || H(left) || H(right))` with `[0; 32]` for empty
//! children, positions fill in BFS order — so nothing here can move a
//! committed root. What the version selects is the *work*:
//!
//! - [`v0`] keeps no intermediate hashes. Every insert re-derives the root by
//!   walking every filled position out of storage (one read and two blake3
//!   calls each), and so does every root read. This is what GROVE_V1..V3
//!   shipped; it is locked — those versions are live and a replayed block
//!   must be charged what it was admitted under.
//! - [`v1`] writes one [`PathRecord`] per insert (the inserted position's
//!   value hash and the node hash of every position on its ancestor path)
//!   and derives an insert's ancestor hashes from the records of earlier
//!   inserts: O(height) record reads, one blake3 per level (two for the
//!   leaf) and ONE record write. The root is the last insert's record. Every
//!   insert is charged a fixed figure for the tree's height
//!   ([`v1_insert_model_cost`]), not the work of its particular position.
//!   Records that are absent (a buffer filled under `v0`) or tagged with an
//!   earlier generation (an earlier epoch over the same slot keys) are never
//!   trusted: the subtree is recomputed from its values, exactly as `v0`
//!   would, and recorded so the next insert is O(height) again.
//!
//! Selected by `grove_version.dense_tree_versions.root_maintenance`.
//!
//! [`PathRecord`]: super::PathRecord

mod v0;
mod v1;

pub use v1::{v1_insert_model_cost, V1InsertModel};

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_storage::StorageContext;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};

use super::{DenseFixedSizedMerkleTree, SlotWriteAccounting};
use crate::DenseMerkleError;

/// The root-maintenance strategy a grove version selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootMaintenance {
    /// Version 0: no intermediate hashes; the root is recomputed from every
    /// filled position on each insert and each root read.
    RecomputeFromValues,
    /// Version 1: a hash record per position; an insert updates its ancestor
    /// path and the root is the record at position 0.
    PerPositionRecords,
}

/// The strategy `grove_version` selects.
pub(crate) fn root_maintenance(
    grove_version: &GroveVersion,
) -> Result<RootMaintenance, DenseMerkleError> {
    match grove_version.dense_tree_versions.root_maintenance {
        0 => Ok(RootMaintenance::RecomputeFromValues),
        1 => Ok(RootMaintenance::PerPositionRecords),
        version => Err(DenseMerkleError::VersionError(
            GroveVersionError::UnknownVersionMismatch {
                method: "DenseFixedSizedMerkleTree root maintenance".to_string(),
                known_versions: vec![0, 1],
                received: version,
            }
            .to_string(),
        )),
    }
}

impl<'db, S: StorageContext<'db>> DenseFixedSizedMerkleTree<S> {
    /// Insert a value at the next available position.
    ///
    /// Returns `(root_hash, position)` where position is the 0-based index
    /// where the value was inserted. Storage and hash costs are tracked in the
    /// returned `OperationCost`. The write is reported as new storage; see
    /// [`try_insert_with_accounting`](Self::try_insert_with_accounting) for a
    /// slot that already holds a committed value.
    ///
    /// What the insert reads, hashes and writes to leave the root derivable
    /// is selected by `grove_version` — see [`root_maintenance`](self).
    pub fn insert(
        &mut self,
        value: &[u8],
        grove_version: &GroveVersion,
    ) -> CostResult<([u8; 32], u16), DenseMerkleError> {
        let cost = OperationCost::default();
        if self.count() >= self.capacity() {
            return Err(DenseMerkleError::TreeFull {
                capacity: self.capacity(),
                count: self.count(),
            })
            .wrap_with_cost(cost);
        }
        let mode = match root_maintenance(grove_version) {
            Ok(m) => m,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        match mode {
            RootMaintenance::RecomputeFromValues => {
                v0::insert_next(self, value, SlotWriteAccounting::AsNew)
            }
            RootMaintenance::PerPositionRecords => {
                v1::insert_next(self, value, SlotWriteAccounting::AsNew)
            }
        }
    }

    /// Try to insert a value at the next available position.
    ///
    /// Returns `None` if the tree is full, otherwise returns
    /// `Some((root_hash, position))`. The write is reported as new storage;
    /// see [`try_insert_with_accounting`](Self::try_insert_with_accounting)
    /// for a slot that already holds a committed value.
    pub fn try_insert(
        &mut self,
        value: &[u8],
        grove_version: &GroveVersion,
    ) -> CostResult<Option<([u8; 32], u16)>, DenseMerkleError> {
        self.try_insert_with_accounting(value, SlotWriteAccounting::AsNew, grove_version)
    }

    /// [`try_insert`](Self::try_insert) with an explicit
    /// [`SlotWriteAccounting`] for the slot write.
    pub fn try_insert_with_accounting(
        &mut self,
        value: &[u8],
        accounting: SlotWriteAccounting,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<([u8; 32], u16)>, DenseMerkleError> {
        let cost = OperationCost::default();
        if self.count() >= self.capacity() {
            return Ok(None).wrap_with_cost(cost);
        }
        let mode = match root_maintenance(grove_version) {
            Ok(m) => m,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        match mode {
            RootMaintenance::RecomputeFromValues => v0::insert_next(self, value, accounting),
            RootMaintenance::PerPositionRecords => v1::insert_next(self, value, accounting),
        }
        .map_ok(Some)
    }

    /// Insert a value **without** returning the root hash.
    ///
    /// Same storage effect as [`try_insert`](Self::try_insert) — the value is
    /// written at the next free position and `count` is incremented — for
    /// callers that only need the FINAL root of a run of inserts, recovered
    /// once at the end with [`root_hash`](Self::root_hash). Returns the
    /// position, or `None` when the tree is already full.
    ///
    /// Under root-maintenance version 0 this skips the O(count) root walk
    /// [`try_insert`](Self::try_insert) performs, so a run of n inserts is
    /// O(n) instead of O(n²) in hash calls. Under version 1 the path record
    /// is written exactly as [`try_insert`](Self::try_insert) does — the
    /// records are the tree's state, not a cache that may be skipped — so
    /// the two cost the same (the fixed model) and differ only in what is
    /// returned.
    pub fn try_insert_no_root(
        &mut self,
        value: &[u8],
        grove_version: &GroveVersion,
    ) -> CostResult<Option<u16>, DenseMerkleError> {
        self.try_insert_no_root_with_accounting(value, SlotWriteAccounting::AsNew, grove_version)
    }

    /// [`try_insert_no_root`](Self::try_insert_no_root) with an explicit
    /// [`SlotWriteAccounting`] for the slot write.
    pub fn try_insert_no_root_with_accounting(
        &mut self,
        value: &[u8],
        accounting: SlotWriteAccounting,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<u16>, DenseMerkleError> {
        let cost = OperationCost::default();
        if self.count() >= self.capacity() {
            return Ok(None).wrap_with_cost(cost);
        }
        let mode = match root_maintenance(grove_version) {
            Ok(m) => m,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        match mode {
            RootMaintenance::RecomputeFromValues => {
                v0::insert_next_no_root(self, value, accounting).map_ok(Some)
            }
            RootMaintenance::PerPositionRecords => {
                v1::insert_next(self, value, accounting).map_ok(|(_root, position)| Some(position))
            }
        }
    }

    /// The root hash of the tree.
    ///
    /// Returns `[0u8; 32]` if the tree is empty. Under root-maintenance
    /// version 0 this walks every filled position; under version 1 it reads
    /// the last insert's path record (falling back to the walk for a buffer
    /// that has no current record there).
    pub fn root_hash(
        &self,
        grove_version: &GroveVersion,
    ) -> CostResult<[u8; 32], DenseMerkleError> {
        match root_maintenance(grove_version) {
            Ok(RootMaintenance::RecomputeFromValues) => v0::root_hash(self),
            Ok(RootMaintenance::PerPositionRecords) => v1::root_hash(self),
            Err(e) => Err(e).wrap_with_cost(OperationCost::default()),
        }
    }
}
