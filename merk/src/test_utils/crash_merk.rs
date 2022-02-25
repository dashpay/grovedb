use std::ops::{Deref, DerefMut};

use anyhow::Result;
use storage::rocksdb_storage::{test_utils::TempStorage, PrefixedRocksDbStorageContext};

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
    pub fn open() -> Result<CrashMerk> {
        let storage = Box::leak(Box::new(TempStorage::new()));
        let context = storage.get_prefixed_context(b"".to_vec());
        let merk = Merk::open(context).unwrap();
        Ok(CrashMerk { merk, storage })
    }

    pub fn crash(&self) {
        self.storage.crash()
    }
}

impl Drop for CrashMerk {
    fn drop(&mut self) {
        unsafe {
            Box::from_raw(self.storage as *const _ as *mut TempStorage);
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
        let mut merk = CrashMerk::open().expect("failed to open merk");
        merk.apply::<_, Vec<u8>>(&[(vec![1, 2, 3], Op::Put(vec![4, 5, 6]))], &[])
            .expect("apply failed");

        merk.crash();

        assert_eq!(merk.get(&[1, 2, 3]).expect("failed to get"), None);
    }
}
