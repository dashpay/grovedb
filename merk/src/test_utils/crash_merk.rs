use std::{
    ops::{Deref, DerefMut},
    rc::Rc,
};

use anyhow::Result;
use storage::rocksdb_storage::{default_rocksdb, PrefixedRocksDbStorage};
use tempdir::TempDir;

use crate::Merk;

/// Wraps a Merk instance and drops it without flushing once it goes out of
/// scope.
pub struct CrashMerk {
    merk: Merk<PrefixedRocksDbStorage>,
    path: Option<TempDir>,
    _db: Rc<rocksdb::OptimisticTransactionDB>,
}

impl CrashMerk {
    /// Opens a `CrashMerk` at the given file path, creating a new one if it
    /// does not exist.
    pub fn open() -> Result<CrashMerk> {
        let path = TempDir::new("db").expect("cannot create tempdir");
        let db = default_rocksdb(path.path());
        let merk = Merk::open(PrefixedRocksDbStorage::new(db.clone(), Vec::new()).unwrap())?;
        Ok(CrashMerk {
            merk,
            path: Some(path),
            _db: db,
        })
    }

    pub fn crash(&mut self) {
        if let Some(a) = self.path.take() {
            drop(a)
        }
    }
}

impl Deref for CrashMerk {
    type Target = Merk<PrefixedRocksDbStorage>;

    fn deref(&self) -> &Merk<PrefixedRocksDbStorage> {
        &self.merk
    }
}

impl DerefMut for CrashMerk {
    fn deref_mut(&mut self) -> &mut Merk<PrefixedRocksDbStorage> {
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
        merk.apply::<_, Vec<u8>>(&[(vec![1, 2, 3], Op::Put(vec![4, 5, 6]))], &[], None)
            .expect("apply failed");

        merk.crash();

        assert_eq!(merk.get(&[1, 2, 3]).expect("failed to get"), None);
    }
}
