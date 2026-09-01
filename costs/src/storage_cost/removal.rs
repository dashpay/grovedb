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
    borrow::BorrowMut,
    cell::Cell,
    cmp::Ordering,
    collections::BTreeMap,
    ops::{Add, AddAssign},
};

use intmap::IntMap;

use crate::storage_cost::removal::StorageRemovedBytes::{
    BasicStorageRemoval, NoStorageRemoval, SectionedStorageRemoval,
};

/// An identifier using 32 bytes
pub type Identifier = [u8; 32];

/// Unknown Epoch
pub const UNKNOWN_EPOCH: u16 = u16::MAX;

/// A BTreeMap mapping identities to the storage they removed by epoch
pub type StorageRemovalPerEpochByIdentifier = BTreeMap<Identifier, IntMap<u16, u32>>;

/// Removal bytes
#[derive(Debug, PartialEq, Clone, Eq, Default)]
pub enum StorageRemovedBytes {
    /// No storage removal
    #[default]
    NoStorageRemoval,
    /// Basic storage removal
    BasicStorageRemoval(u32),
    /// Storage removal is given as sections
    SectionedStorageRemoval(StorageRemovalPerEpochByIdentifier),
}

// Version selector for the basic-into-sectioned storage-removal arithmetic.
//
// Why a thread-local instead of an explicit parameter: the version-sensitive
// combination happens inside the `Add`/`AddAssign` operator overloads for
// `StorageRemovedBytes`, which are reached through `StorageCost` and
// `OperationCost` aggregation at hundreds of version-less call sites (every
// `add_cost` / `cost_return_on_error!`). Those operator signatures cannot carry
// a `grove_version`, and `grovedb-costs` does not depend on `grovedb-version`.
// The version is only known at the GroveDB apply / delete / batch entry points,
// which install a guard ([`use_basic_sectioned_removal_addition_version`] /
// [`with_basic_sectioned_removal_addition_version`]) for the duration of the
// operation.
//
// The default is `0` (legacy / shipped v1..v3 behavior). That default is the safe
// one: an un-guarded caller reproduces historical behavior rather than silently
// "upgrading" to the fixed arithmetic and diverging from the rest of the
// network. Only an explicit guard set from a v4+ context enables the fix.
//
// Note: this selector only affects the three historically-buggy arms. The
// `Sectioned += Basic` arm was always correct and bypasses the selector
// entirely (see [`AddAssign`]).
thread_local! {
    static BASIC_SECTIONED_REMOVAL_ADDITION_VERSION: Cell<u16> = const { Cell::new(0) };
}

/// Guard that restores the previous storage-removal arithmetic version when
/// dropped.
pub struct BasicSectionedRemovalAdditionVersionGuard {
    previous_version: u16,
}

impl Drop for BasicSectionedRemovalAdditionVersionGuard {
    fn drop(&mut self) {
        BASIC_SECTIONED_REMOVAL_ADDITION_VERSION.with(|current_version| {
            current_version.set(self.previous_version);
        });
    }
}

/// Use a storage-removal arithmetic version until the returned guard is
/// dropped.
pub fn use_basic_sectioned_removal_addition_version(
    version: u16,
) -> BasicSectionedRemovalAdditionVersionGuard {
    let previous_version = BASIC_SECTIONED_REMOVAL_ADDITION_VERSION
        .with(|current_version| current_version.replace(version));
    BasicSectionedRemovalAdditionVersionGuard { previous_version }
}

/// Run storage-removal arithmetic using a specific version.
pub fn with_basic_sectioned_removal_addition_version<T>(version: u16, f: impl FnOnce() -> T) -> T {
    let _guard = use_basic_sectioned_removal_addition_version(version);
    f()
}

fn basic_sectioned_removal_addition_version() -> u16 {
    BASIC_SECTIONED_REMOVAL_ADDITION_VERSION.with(Cell::get)
}

/// Correct behavior: fold the basic removal into the default identifier's
/// `UNKNOWN_EPOCH` entry while preserving the rest of the default section. Used
/// unconditionally for the always-correct `Sectioned += Basic` arm, and for the
/// fixed (v4+) path of the three historically-buggy arms.
fn add_basic_removal_to_sectioned_map(
    map: &mut StorageRemovalPerEpochByIdentifier,
    removed_bytes: u32,
) {
    let epoch_map = map.entry(Identifier::default()).or_default();
    let old_value = epoch_map.remove(UNKNOWN_EPOCH).unwrap_or_default();
    epoch_map.insert(UNKNOWN_EPOCH, old_value.saturating_add(removed_bytes));
}

/// Buggy shipped (v1..v3) behavior, preserved verbatim for replay
/// compatibility: when the default identifier already exists it is removed,
/// mutated, and then **dropped** instead of reinserted, losing the rest of the
/// default section. Only reachable through the legacy (v0) path of the three
/// historically-buggy arms.
fn legacy_add_basic_removal_to_sectioned_map(
    map: &mut StorageRemovalPerEpochByIdentifier,
    removed_bytes: u32,
) {
    let default = Identifier::default();
    if let std::collections::btree_map::Entry::Vacant(e) = map.entry(default) {
        let mut new_map = IntMap::new();
        new_map.insert(UNKNOWN_EPOCH, removed_bytes);
        e.insert(new_map);
    } else {
        let mut old_section_map = map.remove(&default).unwrap_or_default();
        if let Some(old_value) = old_section_map.remove(UNKNOWN_EPOCH) {
            old_section_map.insert(UNKNOWN_EPOCH, old_value.saturating_add(removed_bytes));
        } else {
            old_section_map.insert(UNKNOWN_EPOCH, removed_bytes);
        }
    }
}

/// Version-selecting helper for the three historically-buggy arms only
/// (Add: Basic+Sectioned, Add: Sectioned+Basic, AddAssign: Basic+=Sectioned).
/// v0 keeps the buggy shipped behavior; v4+ uses the corrected behavior. The
/// always-correct `Sectioned += Basic` arm must NOT use this — it calls
/// [`add_basic_removal_to_sectioned_map`] directly.
fn add_basic_removal_to_sectioned_map_for_current_version(
    map: &mut StorageRemovalPerEpochByIdentifier,
    removed_bytes: u32,
) {
    if basic_sectioned_removal_addition_version() >= 1 {
        add_basic_removal_to_sectioned_map(map, removed_bytes);
    } else {
        legacy_add_basic_removal_to_sectioned_map(map, removed_bytes);
    }
}

impl Add for StorageRemovedBytes {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        match self {
            NoStorageRemoval => match rhs {
                NoStorageRemoval => NoStorageRemoval,
                BasicStorageRemoval(r) => BasicStorageRemoval(r),
                SectionedStorageRemoval(map) => SectionedStorageRemoval(map),
            },
            BasicStorageRemoval(s) => match rhs {
                NoStorageRemoval => BasicStorageRemoval(s),
                BasicStorageRemoval(r) => BasicStorageRemoval(s.saturating_add(r)),
                SectionedStorageRemoval(mut map) => {
                    add_basic_removal_to_sectioned_map_for_current_version(&mut map, s);
                    SectionedStorageRemoval(map)
                }
            },
            SectionedStorageRemoval(mut smap) => match rhs {
                NoStorageRemoval => SectionedStorageRemoval(smap),
                BasicStorageRemoval(r) => {
                    add_basic_removal_to_sectioned_map_for_current_version(&mut smap, r);
                    SectionedStorageRemoval(smap)
                }
                SectionedStorageRemoval(rmap) => {
                    rmap.into_iter().for_each(|(identifier, mut int_map_b)| {
                        let to_insert_int_map = if let Some(sint_map_a) = smap.remove(&identifier) {
                            // other has an int_map with the same identifier
                            let intersection = sint_map_a
                                .into_iter()
                                .map(|(k, v)| {
                                    let combined = if let Some(value_b) = int_map_b.remove(k) {
                                        v.saturating_add(value_b)
                                    } else {
                                        v
                                    };
                                    (k, combined)
                                })
                                .collect::<IntMap<u16, u32>>();
                            intersection.into_iter().chain(int_map_b).collect()
                        } else {
                            int_map_b
                        };
                        smap.insert(identifier, to_insert_int_map);
                    });
                    SectionedStorageRemoval(smap)
                }
            },
        }
    }
}

impl AddAssign for StorageRemovedBytes {
    fn add_assign(&mut self, rhs: Self) {
        match self.borrow_mut() {
            NoStorageRemoval => *self = rhs,
            BasicStorageRemoval(s) => match rhs {
                NoStorageRemoval => {}
                BasicStorageRemoval(r) => *s = s.saturating_add(r),
                SectionedStorageRemoval(mut map) => {
                    add_basic_removal_to_sectioned_map_for_current_version(&mut map, *s);
                    *self = SectionedStorageRemoval(map)
                }
            },
            SectionedStorageRemoval(smap) => match rhs {
                NoStorageRemoval => {}
                BasicStorageRemoval(r) => {
                    // `Sectioned += Basic` reinserted the default section
                    // correctly in EVERY shipped version, so it is intentionally
                    // NOT version-gated — always use the default-section-
                    // preserving helper. The three historically-buggy arms
                    // (Add: Basic+Sectioned, Add: Sectioned+Basic, AddAssign:
                    // Basic+=Sectioned) are gated; this one was never broken, so
                    // routing it through the version selector would *regress*
                    // shipped v1..v3 output (drop the default section under v0).
                    add_basic_removal_to_sectioned_map(smap, r);
                }
                SectionedStorageRemoval(rmap) => {
                    rmap.into_iter().for_each(|(identifier, mut int_map_b)| {
                        let to_insert_int_map = if let Some(sint_map_a) = smap.remove(&identifier) {
                            // other has an int_map with the same identifier
                            let intersection = sint_map_a
                                .into_iter()
                                .map(|(k, v)| {
                                    let combined = if let Some(value_b) = int_map_b.remove(k) {
                                        v.saturating_add(value_b)
                                    } else {
                                        v
                                    };
                                    (k, combined)
                                })
                                .collect::<IntMap<u16, u32>>();
                            intersection.into_iter().chain(int_map_b).collect()
                        } else {
                            int_map_b
                        };
                        // reinsert the now combined intmap
                        smap.insert(identifier, to_insert_int_map);
                    });
                }
            },
        }
    }
}

impl PartialOrd for StorageRemovedBytes {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.total_removed_bytes().cmp(&other.total_removed_bytes()))
    }
}

impl StorageRemovedBytes {
    /// Were any bytes removed?
    pub fn has_removal(&self) -> bool {
        match self {
            NoStorageRemoval => false,
            BasicStorageRemoval(r) => *r != 0,
            SectionedStorageRemoval(m) => m
                .iter()
                .any(|(_, int_map)| int_map.iter().any(|(_, r)| *r != 0)),
        }
    }

    /// The total number of removed bytes
    pub fn total_removed_bytes(&self) -> u32 {
        match self {
            NoStorageRemoval => 0,
            BasicStorageRemoval(r) => *r,
            SectionedStorageRemoval(m) => m
                .values()
                .map(|int_map| int_map.values().copied().sum::<u32>())
                .sum(),
        }
    }
}
