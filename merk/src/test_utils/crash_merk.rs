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

//! Crash Merk

#[cfg(feature = "full")]
use std::ops::{Deref, DerefMut};

use path::SubtreePath;
#[cfg(feature = "full")]
use storage::{
    rocksdb_storage::{test_utils::TempStorage, PrefixedRocksDbStorageContext},
    Storage,
};

#[cfg(feature = "full")]
use crate::{error::Error, Merk};

#[cfg(feature = "full")]
/// Wraps a Merk instance and drops it without flushing once it goes out of
/// scope.
pub struct CrashMerk {
    storage: &'static TempStorage,
    merk: Merk<PrefixedRocksDbStorageContext<'static>>,
}

#[cfg(feature = "full")]
impl CrashMerk {
    /// Opens a `CrashMerk` at the given file path, creating a new one if it
    /// does not exist.
    pub fn open_base() -> Result<CrashMerk, Error> {
        let storage = Box::leak(Box::new(TempStorage::new()));
        let context = storage
            .get_storage_context(&SubtreePath::new())
            .unwrap();
        let merk = Merk::open_base(context, false).unwrap().unwrap();
        Ok(CrashMerk { merk, storage })
    }

    /// Crash
    pub fn crash(&self) {
        self.storage.crash()
    }
}

#[cfg(feature = "full")]
impl Drop for CrashMerk {
    fn drop(&mut self) {
        unsafe {
            drop(Box::from_raw(self.storage as *const _ as *mut TempStorage));
        }
    }
}

#[cfg(feature = "full")]
impl Deref for CrashMerk {
    type Target = Merk<PrefixedRocksDbStorageContext<'static>>;

    fn deref(&self) -> &Merk<PrefixedRocksDbStorageContext<'static>> {
        &self.merk
    }
}

#[cfg(feature = "full")]
impl DerefMut for CrashMerk {
    fn deref_mut(&mut self) -> &mut Merk<PrefixedRocksDbStorageContext<'static>> {
        &mut self.merk
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use super::CrashMerk;
    use crate::{Op, TreeFeatureType::BasicMerk};

    #[test]
    #[ignore] // currently this still works because we enabled the WAL
    fn crash() {
        let mut merk = CrashMerk::open_base().expect("failed to open merk");
        merk.apply::<_, Vec<u8>>(
            &[(vec![1, 2, 3], Op::Put(vec![4, 5, 6], BasicMerk))],
            &[],
            None,
        )
        .unwrap()
        .expect("apply failed");

        merk.crash();

        assert_eq!(
            merk.get(&[1, 2, 3], true).unwrap().expect("failed to get"),
            None
        );
    }
}
