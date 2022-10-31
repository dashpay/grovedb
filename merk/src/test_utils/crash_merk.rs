use std::{
    iter::empty,
    ops::{Deref, DerefMut},
};

use anyhow::Result;
use storage::{
    rocksdb_storage::{test_utils::TempStorage, PrefixedRocksDbStorageContext},
    Storage,
};

use crate::Merk;

/// Wraps a Merk instance and drops it without flushing once it goes out of
/// scope.
pub struct CrashMerk {
    storage: &'static TempStorage,
    merk: Merk<PrefixedRocksDbStorageContext<'static>>,
}

impl CrashMerk {
    /// Opens a `CrashMerk` at the given file path, creating a new one if it
    /// does not exist.
    pub fn open_base() -> Result<CrashMerk> {
        let storage = Box::leak(Box::new(TempStorage::new()));
        let context = storage.get_storage_context(empty()).unwrap();
        let merk = Merk::open_base(context).unwrap().unwrap();
        Ok(CrashMerk { merk, storage })
    }

    pub fn crash(&self) {
        self.storage.crash()
    }
}

impl Drop for CrashMerk {
    fn drop(&mut self) {
        unsafe {
            drop(Box::from_raw(self.storage as *const _ as *mut TempStorage));
        }
    }
}

impl Deref for CrashMerk {
    type Target = Merk<PrefixedRocksDbStorageContext<'static>>;

    fn deref(&self) -> &Merk<PrefixedRocksDbStorageContext<'static>> {
        &self.merk
    }
}

impl DerefMut for CrashMerk {
    fn deref_mut(&mut self) -> &mut Merk<PrefixedRocksDbStorageContext<'static>> {
        &mut self.merk
    }
}

#[cfg(test)]
mod tests {
    use super::CrashMerk;
    use crate::Op;

    #[test]
    #[ignore] // currently this still works because we enabled the WAL
    fn crash() {
        let mut merk = CrashMerk::open_base().expect("failed to open merk");
        merk.apply::<_, Vec<u8>>(&[(vec![1, 2, 3], Op::Put(vec![4, 5, 6]))], &[], None)
            .unwrap()
            .expect("apply failed");

        merk.crash();

        assert_eq!(merk.get(&[1, 2, 3]).unwrap().expect("failed to get"), None);
    }
}
