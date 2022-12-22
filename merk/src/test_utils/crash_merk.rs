#[cfg(feature = "full")]
use std::{
    iter::empty,
    ops::{Deref, DerefMut},
};

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
        let context = storage.get_storage_context(empty()).unwrap();
        let merk = Merk::open_base(context, false).unwrap().unwrap();
        Ok(CrashMerk { merk, storage })
    }

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

        assert_eq!(merk.get(&[1, 2, 3]).unwrap().expect("failed to get"), None);
    }
}
