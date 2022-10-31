use std::{
    borrow::BorrowMut,
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

/// A BTreeMap mapping identities to the storage they removed by epoch
pub type StorageRemovalPerEpochByIdentifier = BTreeMap<Identifier, IntMap<u32>>;

/// Removal bytes
#[derive(Debug, PartialEq, Clone, Eq)]
pub enum StorageRemovedBytes {
    /// No storage removal
    NoStorageRemoval,
    /// Basic storage removal
    BasicStorageRemoval(u32),
    /// Storage removal is given as sections
    SectionedStorageRemoval(StorageRemovalPerEpochByIdentifier),
}

impl Default for StorageRemovedBytes {
    fn default() -> Self {
        NoStorageRemoval
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
                BasicStorageRemoval(r) => BasicStorageRemoval(s + r),
                SectionedStorageRemoval(mut map) => {
                    let default = Identifier::default();
                    if map.contains_key(&default) {
                        let mut old_section_map = map.remove(&default).unwrap_or_default();
                        if let Some(old_value) = old_section_map.remove(u64::MAX) {
                            old_section_map.insert(u64::MAX, old_value + s);
                        } else {
                            old_section_map.insert(u64::MAX, s);
                        }
                    } else {
                        let mut new_map = IntMap::new();
                        new_map.insert(u64::MAX, s);
                        map.insert(default, new_map);
                    }
                    SectionedStorageRemoval(map)
                }
            },
            SectionedStorageRemoval(mut smap) => match rhs {
                NoStorageRemoval => SectionedStorageRemoval(smap),
                BasicStorageRemoval(r) => {
                    let default = Identifier::default();
                    if smap.contains_key(&default) {
                        let mut old_section_map = smap.remove(&default).unwrap_or_default();
                        if let Some(old_value) = old_section_map.remove(u64::MAX) {
                            old_section_map.insert(u64::MAX, old_value + r);
                        } else {
                            old_section_map.insert(u64::MAX, r);
                        }
                    } else {
                        let mut new_map = IntMap::new();
                        new_map.insert(u64::MAX, r);
                        smap.insert(default, new_map);
                    }
                    SectionedStorageRemoval(smap)
                }
                SectionedStorageRemoval(rmap) => {
                    rmap.into_iter().for_each(|(identifier, mut int_map_b)| {
                        let to_insert_int_map =
                            if let Some(mut sint_map_a) = smap.remove(&identifier) {
                                // other has an int_map with the same identifier
                                let intersection = sint_map_a
                                    .into_iter()
                                    .map(|(k, v)| {
                                        let combined = if let Some(value_b) = int_map_b.remove(k) {
                                            v + value_b
                                        } else {
                                            v
                                        };
                                        (k, combined)
                                    })
                                    .collect::<IntMap<u32>>();
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
                BasicStorageRemoval(r) => *s += r,
                SectionedStorageRemoval(mut map) => {
                    let default = Identifier::default();
                    if let Some(mut old_int_map) = map.remove(&default) {
                        if old_int_map.contains_key(u64::MAX) {
                            let old_value = old_int_map.remove(u64::MAX).unwrap_or_default();
                            old_int_map.insert(u64::MAX, old_value + *s);
                        } else {
                            old_int_map.insert(u64::MAX, *s);
                        }
                    } else {
                        let mut new_map = IntMap::new();
                        new_map.insert(u64::MAX, *s);
                        map.insert(default, new_map);
                    }
                    *self = SectionedStorageRemoval(map)
                }
            },
            SectionedStorageRemoval(smap) => match rhs {
                NoStorageRemoval => {}
                BasicStorageRemoval(r) => {
                    let default = Identifier::default();
                    let map_to_insert = if let Some(mut old_int_map) = smap.remove(&default) {
                        if old_int_map.contains_key(u64::MAX) {
                            let old_value = old_int_map.remove(u64::MAX).unwrap_or_default();
                            old_int_map.insert(u64::MAX, old_value + r);
                        } else {
                            old_int_map.insert(u64::MAX, r);
                        }
                        old_int_map
                    } else {
                        let mut new_map = IntMap::new();
                        new_map.insert(u64::MAX, r);
                        new_map
                    };
                    smap.insert(default, map_to_insert);
                }
                SectionedStorageRemoval(rmap) => {
                    rmap.into_iter().for_each(|(identifier, mut int_map_b)| {
                        let to_insert_int_map =
                            if let Some(mut sint_map_a) = smap.remove(&identifier) {
                                // other has an int_map with the same identifier
                                let intersection = sint_map_a
                                    .into_iter()
                                    .map(|(k, v)| {
                                        let combined = if let Some(value_b) = int_map_b.remove(k) {
                                            v + value_b
                                        } else {
                                            v
                                        };
                                        (k, combined)
                                    })
                                    .collect::<IntMap<u32>>();
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
                .iter()
                .map(|(_, int_map)| int_map.iter().map(|(_, r)| *r).sum::<u32>())
                .sum(),
        }
    }
}
