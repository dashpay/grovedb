// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

use std::{
    cmp::Ordering,
    ops::{Add, AddAssign},
};

use integer_encoding::VarInt;

use crate::{
    storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
    BasicStorageRemoval, StorageRemovedBytes,
};

/// Storage only operation costs separated by key and value
#[derive(Debug, PartialEq, Clone, Eq, Default)]
pub struct KeyValueStorageCost {
    /// Key storage_cost costs
    pub key_storage_cost: StorageCost,
    /// Value storage_cost costs
    pub value_storage_cost: StorageCost,
    /// Is this a new node
    pub new_node: bool,
    /// Should we verify this at storage time
    pub needs_value_verification: bool,
    /// The put was billed by its owner in advance — bytes, key and the
    /// write itself — so the commit path charges it nothing, the seek
    /// included. Set only by [`prepaid`](Self::prepaid); `Default` is NOT
    /// prepaid (a default cost info still pays its seek).
    pub prepaid: bool,
}

impl KeyValueStorageCost {
    /// Convenience method for getting the cost of updating the key of the root
    /// of each merk
    pub fn for_updated_root_cost(old_tree_key_len: Option<u32>, tree_key_len: u32) -> Self {
        if let Some(old_tree_key_len) = old_tree_key_len {
            let key_storage_cost = StorageCost {
                added_bytes: 0,
                replaced_bytes: 34, // prefix + 1 for 'r' + 1 required space
                removed_bytes: NoStorageRemoval,
            };
            let new_bytes = tree_key_len + tree_key_len.required_space() as u32;
            let value_storage_cost = match tree_key_len.cmp(&old_tree_key_len) {
                Ordering::Less => {
                    // we removed bytes
                    let old_bytes = old_tree_key_len + old_tree_key_len.required_space() as u32;
                    StorageCost {
                        added_bytes: 0,
                        replaced_bytes: new_bytes,
                        removed_bytes: BasicStorageRemoval(old_bytes - new_bytes),
                    }
                }
                Ordering::Equal => StorageCost {
                    added_bytes: 0,
                    replaced_bytes: new_bytes,
                    removed_bytes: NoStorageRemoval,
                },
                Ordering::Greater => {
                    let old_bytes = old_tree_key_len + old_tree_key_len.required_space() as u32;
                    StorageCost {
                        added_bytes: new_bytes - old_bytes,
                        replaced_bytes: old_bytes,
                        removed_bytes: NoStorageRemoval,
                    }
                }
            };
            KeyValueStorageCost {
                key_storage_cost,
                value_storage_cost,
                new_node: false,
                needs_value_verification: false,
                prepaid: false,
            }
        } else {
            KeyValueStorageCost {
                key_storage_cost: StorageCost {
                    added_bytes: 34, // prefix + 1 for 'r' + 1 required space
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                value_storage_cost: StorageCost {
                    added_bytes: tree_key_len + tree_key_len.required_space() as u32,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                new_node: true,
                needs_value_verification: false,
                prepaid: false,
            }
        }
    }

    /// Cost of rewriting an existing value in place — `previous_len` bytes
    /// stored under a key that already exists, overwritten with `new_len`
    /// bytes — with no refund semantics.
    ///
    /// The paid size of a value is its length plus the varint encoding that
    /// length, which is what the commit path verifies `added + replaced`
    /// against. The key is not charged (it exists); `replaced_bytes` is the
    /// smaller of the previous and the new paid size; `added_bytes` is the
    /// growth beyond the previous size, if any; a shrink is NOT credited as
    /// removed bytes — used for rolling data (a reused buffer slot, a
    /// frontier rewritten on every append) where a refund would mean paying
    /// someone back for bytes nobody owns.
    pub fn for_in_place_value_rewrite(previous_len: u32, new_len: u32) -> Self {
        let paid_previous = previous_len + previous_len.required_space() as u32;
        let paid_new = new_len + new_len.required_space() as u32;
        KeyValueStorageCost {
            key_storage_cost: StorageCost::default(),
            value_storage_cost: StorageCost {
                added_bytes: paid_new.saturating_sub(paid_previous),
                replaced_bytes: paid_new.min(paid_previous),
                removed_bytes: NoStorageRemoval,
            },
            new_node: false,
            needs_value_verification: true,
            prepaid: false,
        }
    }

    /// The cost information of a put whose owner has already charged
    /// everything about it — its key and value bytes AND the write itself —
    /// in advance (the bulk-append tree prepays each append's share of the
    /// chunk blob, the MMR nodes and the commit-time puts that compaction
    /// issues, amortized over the epoch). The commit path bills such a put
    /// nothing: no added or replaced bytes, no key, no value verification,
    /// and no seek.
    pub fn prepaid() -> Self {
        KeyValueStorageCost {
            key_storage_cost: StorageCost::default(),
            value_storage_cost: StorageCost::default(),
            new_node: false,
            needs_value_verification: false,
            prepaid: true,
        }
    }

    /// Whether this is the cost information of a fully prepaid put (see
    /// [`prepaid`](Self::prepaid)): nothing to bill at commit, the seek
    /// included. An explicit marker — a zero-cost `Default` is not prepaid.
    pub fn is_prepaid(&self) -> bool {
        self.prepaid
    }

    /// Returns the total removed bytes between the key removed bytes and the
    /// value removed bytes
    pub fn combined_removed_bytes(self) -> StorageRemovedBytes {
        self.key_storage_cost.removed_bytes + self.value_storage_cost.removed_bytes
    }
}

impl Add for KeyValueStorageCost {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            key_storage_cost: self.key_storage_cost + rhs.key_storage_cost,
            value_storage_cost: self.value_storage_cost + rhs.value_storage_cost,
            new_node: self.new_node & rhs.new_node,
            needs_value_verification: self.needs_value_verification & rhs.needs_value_verification,
            prepaid: self.prepaid & rhs.prepaid,
        }
    }
}

impl AddAssign for KeyValueStorageCost {
    fn add_assign(&mut self, rhs: Self) {
        self.key_storage_cost += rhs.key_storage_cost;
        self.value_storage_cost += rhs.value_storage_cost;
        self.new_node &= rhs.new_node;
        self.needs_value_verification &= rhs.needs_value_verification;
        self.prepaid &= rhs.prepaid;
    }
}

#[cfg(test)]
mod prepaid_tests {
    use super::*;

    #[test]
    fn prepaid_is_the_only_shape_that_bills_nothing() {
        assert!(KeyValueStorageCost::prepaid().is_prepaid());
        // The marker is explicit: a zero-cost default still pays its seek.
        assert!(!KeyValueStorageCost::default().is_prepaid());
        assert_ne!(
            KeyValueStorageCost::prepaid(),
            KeyValueStorageCost::default()
        );
        assert!(!KeyValueStorageCost::for_in_place_value_rewrite(0, 0).is_prepaid());
        assert!(!KeyValueStorageCost::for_in_place_value_rewrite(8, 8).is_prepaid());
        assert!(!KeyValueStorageCost::for_updated_root_cost(None, 1).is_prepaid());
        assert!(!KeyValueStorageCost::for_updated_root_cost(Some(1), 1).is_prepaid());
        let mut unmarked = KeyValueStorageCost::prepaid();
        unmarked.prepaid = false;
        assert!(!unmarked.is_prepaid());
        let mut replaced = KeyValueStorageCost::prepaid();
        replaced.value_storage_cost.replaced_bytes = 1;
        // Bytes on a prepaid put are still added by the commit path; only the
        // seek is waived. The marker, not the shape, decides.
        assert!(replaced.is_prepaid());
        let mut sum = KeyValueStorageCost::prepaid();
        sum += KeyValueStorageCost::default();
        assert!(
            !sum.is_prepaid(),
            "a sum with an unprepaid part is not prepaid"
        );
    }
}

#[cfg(test)]
mod in_place_rewrite_tests {
    use super::*;

    #[test]
    fn same_size_is_fully_replaced() {
        let c = KeyValueStorageCost::for_in_place_value_rewrite(312, 312);
        assert_eq!(c.value_storage_cost.replaced_bytes, 314);
        assert_eq!(c.value_storage_cost.added_bytes, 0);
        assert_eq!(c.value_storage_cost.removed_bytes, NoStorageRemoval);
        assert_eq!(c.key_storage_cost, StorageCost::default());
        assert!(!c.new_node);
        assert!(c.needs_value_verification);
        // Verifies against the paid size of the new value.
        assert!(c.value_storage_cost.verify(314).is_ok());
    }

    #[test]
    fn growth_is_added_on_top_of_the_previous_size() {
        // 74 -> 106 bytes (a frontier gaining one ommer).
        let c = KeyValueStorageCost::for_in_place_value_rewrite(74, 106);
        assert_eq!(c.value_storage_cost.replaced_bytes, 75);
        assert_eq!(c.value_storage_cost.added_bytes, 32);
        assert!(c.value_storage_cost.verify(107).is_ok());
    }

    #[test]
    fn shrink_is_replaced_at_the_new_size_and_not_credited() {
        let c = KeyValueStorageCost::for_in_place_value_rewrite(1066, 74);
        assert_eq!(c.value_storage_cost.replaced_bytes, 75);
        assert_eq!(c.value_storage_cost.added_bytes, 0);
        assert_eq!(c.value_storage_cost.removed_bytes, NoStorageRemoval);
        assert!(c.value_storage_cost.verify(75).is_ok());
    }

    #[test]
    fn varint_width_change_counts_as_growth() {
        // 127 -> 128 bytes crosses a varint boundary: paid 128 -> 130.
        let c = KeyValueStorageCost::for_in_place_value_rewrite(127, 128);
        assert_eq!(c.value_storage_cost.replaced_bytes, 128);
        assert_eq!(c.value_storage_cost.added_bytes, 2);
    }
}
