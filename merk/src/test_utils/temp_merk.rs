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

//! Temp merk test utils

#[cfg(feature = "full")]
use std::ops::{Deref, DerefMut};

use grovedb_path::SubtreePath;
use grovedb_storage::StorageBatch;
#[cfg(feature = "full")]
use grovedb_storage::{
    rocksdb_storage::{test_utils::TempStorage, PrefixedRocksDbStorageContext},
    Storage,
};
use grovedb_version::version::GroveVersion;

#[cfg(feature = "full")]
use crate::Merk;
use crate::{merk::TreeType, tree::kv::ValueDefinedCostType};

#[cfg(feature = "full")]
/// Wraps a Merk instance and deletes it from disk it once it goes out of scope.
pub struct TempMerk {
    storage: &'static TempStorage,
    batch: &'static StorageBatch,
    merk: Merk<PrefixedRocksDbStorageContext<'static>>,
}

#[cfg(feature = "full")]
impl TempMerk {
    /// Opens a `TempMerk` at the given file path, creating a new one if it
    /// does not exist.
    pub fn new(grove_version: &GroveVersion) -> Self {
        let storage = Box::leak(Box::new(TempStorage::new()));
        let batch = Box::leak(Box::new(StorageBatch::new()));

        let context = storage
            .get_storage_context(SubtreePath::empty(), Some(batch))
            .unwrap();

        let merk = Merk::open_base(
            context,
            TreeType::NormalTree,
            None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .unwrap()
        .unwrap();
        TempMerk {
            storage,
            merk,
            batch,
        }
    }

    /// Commits pending batch operations.
    pub fn commit(&mut self, grove_version: &GroveVersion) {
        let batch = unsafe { Box::from_raw(self.batch as *const _ as *mut StorageBatch) };
        self.storage
            .commit_multi_context_batch(*batch, None)
            .unwrap()
            .expect("unable to commit batch");
        self.batch = Box::leak(Box::new(StorageBatch::new()));
        let context = self
            .storage
            .get_storage_context(SubtreePath::empty(), Some(self.batch))
            .unwrap();
        self.merk = Merk::open_base(
            context,
            TreeType::NormalTree,
            None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .unwrap()
        .unwrap();
    }
}

#[cfg(feature = "full")]
impl Drop for TempMerk {
    fn drop(&mut self) {
        unsafe {
            let batch = Box::from_raw(self.batch as *const _ as *mut StorageBatch);
            let _ = self.storage.commit_multi_context_batch(*batch, None);
            drop(Box::from_raw(self.storage as *const _ as *mut TempStorage));
        }
    }
}

#[cfg(feature = "full")]
impl Default for TempMerk {
    fn default() -> Self {
        Self::new(GroveVersion::latest())
    }
}

#[cfg(feature = "full")]
impl Deref for TempMerk {
    type Target = Merk<PrefixedRocksDbStorageContext<'static>>;

    fn deref(&self) -> &Merk<PrefixedRocksDbStorageContext<'static>> {
        &self.merk
    }
}

#[cfg(feature = "full")]
impl DerefMut for TempMerk {
    fn deref_mut(&mut self) -> &mut Merk<PrefixedRocksDbStorageContext<'static>> {
        &mut self.merk
    }
}
