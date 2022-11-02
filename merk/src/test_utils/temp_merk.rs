use std::{
    iter::empty,
    ops::{Deref, DerefMut},
};

use storage::{
    rocksdb_storage::{test_utils::TempStorage, PrefixedRocksDbStorageContext},
    Storage,
};

use crate::Merk;

/// Wraps a Merk instance and deletes it from disk it once it goes out of scope.
pub struct TempMerk {
    storage: &'static TempStorage,
    merk: Merk<PrefixedRocksDbStorageContext<'static>>,
}

impl TempMerk {
    /// Opens a `TempMerk` at the given file path, creating a new one if it
    /// does not exist.
    pub fn new() -> Self {
        let storage = Box::leak(Box::new(TempStorage::new()));
        let context = storage.get_storage_context(empty()).unwrap();
        let merk = Merk::open_base(context).unwrap().unwrap();
        TempMerk { storage, merk }
    }
}

impl Drop for TempMerk {
    fn drop(&mut self) {
        unsafe {
            drop(Box::from_raw(self.storage as *const _ as *mut TempStorage));
        }
    }
}

impl Default for TempMerk {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for TempMerk {
    type Target = Merk<PrefixedRocksDbStorageContext<'static>>;

    fn deref(&self) -> &Merk<PrefixedRocksDbStorageContext<'static>> {
        &self.merk
    }
}

impl DerefMut for TempMerk {
    fn deref_mut(&mut self) -> &mut Merk<PrefixedRocksDbStorageContext<'static>> {
        &mut self.merk
    }
}
