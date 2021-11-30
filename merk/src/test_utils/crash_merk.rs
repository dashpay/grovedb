use std::{
    ops::{Deref, DerefMut},
    rc::Rc,
};

use storage::rocksdb_storage::PrefixedRocksDbStorage;
use tempdir::TempDir;

use crate::{Merk, Result};

/// Wraps a Merk instance and drops it without flushing once it goes out of
/// scope.
pub struct CrashMerk {
    merk: Merk<PrefixedRocksDbStorage>,
    path: Option<TempDir>,
    _db: Rc<rocksdb::DB>,
}

impl CrashMerk {
    /// Opens a `CrashMerk` at the given file path, creating a new one if it
    /// does not exist.
    pub fn open() -> Result<CrashMerk> {
        let path = TempDir::new("db").expect("cannot create tempdir");
        let db = super::default_rocksdb(path.path());
        let merk = Merk::open(PrefixedRocksDbStorage::new(db.clone(), Vec::new()).unwrap())?;
        Ok(CrashMerk {
            merk,
            path: Some(path),
            _db: db,
        })
    }

    pub fn crash(&mut self) {
        self.path.take().map(|x| drop(x));
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
        merk.apply(&[(vec![1, 2, 3], Op::Put(vec![4, 5, 6]))], &[])
            .expect("apply failed");

        merk.crash();

        assert_eq!(merk.get(&[1, 2, 3]).expect("failed to get"), None);
    }
}
