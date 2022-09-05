use std::ops::{Add, AddAssign};

use integer_encoding::VarInt;

use crate::storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost};

/// Storage only Operation Costs separated by key and value
#[derive(PartialEq, Clone, Eq)]
pub struct KeyValueStorageCost {
    /// Key storage_cost costs
    pub key_storage_cost: StorageCost,
    /// Value storage_cost costs
    pub value_storage_cost: StorageCost,
    /// Is this a new node
    pub new_node: bool,
}

impl KeyValueStorageCost {
    /// Convenience method for getting the cost of updating the key of the root
    /// of each merk
    pub fn for_updated_root_cost(tree_key_len: u32) -> Self {
        KeyValueStorageCost {
            key_storage_cost: StorageCost {
                added_bytes: 0,
                replaced_bytes: 34, // prefix + 1 for 'r' + 1 required space
                removed_bytes: NoStorageRemoval,
            },
            value_storage_cost: StorageCost {
                added_bytes: 0,
                replaced_bytes: tree_key_len + tree_key_len.required_space() as u32,
                removed_bytes: NoStorageRemoval,
            },
            new_node: false,
        }
    }
}

impl Add for KeyValueStorageCost {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            key_storage_cost: self.key_storage_cost + rhs.key_storage_cost,
            value_storage_cost: self.value_storage_cost + rhs.value_storage_cost,
            new_node: self.new_node & rhs.new_node,
        }
    }
}

impl AddAssign for KeyValueStorageCost {
    fn add_assign(&mut self, rhs: Self) {
        self.key_storage_cost += rhs.key_storage_cost;
        self.value_storage_cost += rhs.value_storage_cost;
        self.new_node &= rhs.new_node;
    }
}
