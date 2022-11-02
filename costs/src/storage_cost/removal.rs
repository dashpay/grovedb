use std::{
    borrow::BorrowMut,
    cmp::Ordering,
    ops::{Add, AddAssign},
};

use intmap::IntMap;

use crate::storage_cost::removal::StorageRemovedBytes::{
    BasicStorageRemoval, NoStorageRemoval, SectionedStorageRemoval,
};

/// Removal bytes
#[derive(Debug, PartialEq, Clone, Eq)]
pub enum StorageRemovedBytes {
    /// No storage removal
    NoStorageRemoval,
    /// Basic storage removal
    BasicStorageRemoval(u32),
    /// Storage removal is given as sections
    SectionedStorageRemoval(IntMap<u32>),
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
                    if map.contains_key(u64::MAX) {
                        let old_value = map.remove(u64::MAX).unwrap_or_default();
                        map.insert(u64::MAX, old_value + s);
                    } else {
                        map.insert(u64::MAX, s);
                    }
                    SectionedStorageRemoval(map)
                }
            },
            SectionedStorageRemoval(mut smap) => match rhs {
                NoStorageRemoval => SectionedStorageRemoval(smap),
                BasicStorageRemoval(r) => {
                    if smap.contains_key(u64::MAX) {
                        let old_value = smap.remove(u64::MAX).unwrap_or_default();
                        smap.insert(u64::MAX, old_value + r);
                    } else {
                        smap.insert(u64::MAX, r);
                    }
                    SectionedStorageRemoval(smap)
                }
                SectionedStorageRemoval(rmap) => {
                    rmap.into_iter().for_each(|(k, v)| {
                        if smap.contains_key(k) {
                            let old_value = smap.remove(k).unwrap_or_default();
                            smap.insert(k, old_value + v);
                        } else {
                            smap.insert(k, v);
                        }
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
                    if map.contains_key(u64::MAX) {
                        let old_value = map.remove(u64::MAX).unwrap_or_default();
                        map.insert(u64::MAX, old_value + *s);
                    } else {
                        map.insert(u64::MAX, *s);
                    }
                    *self = SectionedStorageRemoval(map)
                }
            },
            SectionedStorageRemoval(smap) => match rhs {
                NoStorageRemoval => {}
                BasicStorageRemoval(r) => {
                    if smap.contains_key(u64::MAX) {
                        let old_value = smap.remove(u64::MAX).unwrap_or_default();
                        smap.insert(u64::MAX, old_value + r);
                    } else {
                        smap.insert(u64::MAX, r);
                    }
                }
                SectionedStorageRemoval(rmap) => {
                    rmap.into_iter().for_each(|(k, v)| {
                        if smap.contains_key(k) {
                            let old_value = smap.remove(k).unwrap_or_default();
                            smap.insert(k, old_value + v);
                        } else {
                            smap.insert(k, v);
                        }
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
            SectionedStorageRemoval(m) => m.iter().any(|(_, r)| *r != 0),
        }
    }

    /// The total number of removed bytes
    pub fn total_removed_bytes(&self) -> u32 {
        match self {
            NoStorageRemoval => 0,
            BasicStorageRemoval(r) => *r,
            SectionedStorageRemoval(m) => m.iter().map(|(_, r)| *r).sum(),
        }
    }
}
