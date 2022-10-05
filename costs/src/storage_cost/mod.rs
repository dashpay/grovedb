use std::ops::{Add, AddAssign};

use crate::{
    error::Error,
    storage_cost::removal::{StorageRemovedBytes, StorageRemovedBytes::NoStorageRemoval},
};

/// Key Value Storage Costs
pub mod key_value_cost;
/// Storage Removal
pub mod removal;
/// Costs to Transitions
pub mod transition;

/// Storage only Operation Costs
#[derive(Debug, PartialEq, Clone, Eq)]
pub struct StorageCost {
    /// How many bytes are said to be added on hard drive.
    pub added_bytes: u32,
    /// How many bytes are said to be replaced on hard drive.
    pub replaced_bytes: u32,
    /// How many bytes are said to be removed on hard drive.
    pub removed_bytes: StorageRemovedBytes,
}

impl Add for StorageCost {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            added_bytes: self.added_bytes + rhs.added_bytes,
            replaced_bytes: self.replaced_bytes + rhs.replaced_bytes,
            removed_bytes: self.removed_bytes + rhs.removed_bytes,
        }
    }
}

impl AddAssign for StorageCost {
    fn add_assign(&mut self, rhs: Self) {
        self.added_bytes += rhs.added_bytes;
        self.replaced_bytes += rhs.replaced_bytes;
        self.removed_bytes += rhs.removed_bytes;
    }
}

impl StorageCost {
    /// Verify that the len of the item matches the given storage_cost cost
    pub fn verify(&self, len: u32) -> Result<(), Error> {
        // from the length we first need to remove 2 bytes for the left and right
        // optional links we then should add the parent link
        let size = self.added_bytes + self.replaced_bytes;
        match size == len {
            true => Ok(()),
            false => Err(Error::StorageCostMismatch {
                expected: self.clone(),
                actual_total_bytes: len,
            }),
        }
    }

    /// Verifies the len of a key item only if the node is new
    /// doesn't need to verify for the update case since the key never changes
    pub fn verify_key_storage_cost(&self, len: u32, new_node: bool) -> Result<(), Error> {
        if new_node {
            self.verify(len)
        } else {
            Ok(())
        }
    }

    /// worse_or_eq_than means worse for things that would cost resources
    /// storage_freed_bytes is worse when it is lower instead
    pub fn worse_or_eq_than(&self, other: &Self) -> bool {
        self.replaced_bytes >= other.replaced_bytes
            && self.added_bytes >= other.added_bytes
            && self.removed_bytes <= other.removed_bytes
    }
}

impl Default for StorageCost {
    fn default() -> Self {
        Self {
            added_bytes: 0,
            replaced_bytes: 0,
            removed_bytes: NoStorageRemoval,
        }
    }
}
