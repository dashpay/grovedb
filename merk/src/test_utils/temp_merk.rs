use std::{
    ops::{Deref, DerefMut},
    rc::Rc,
    sync::Arc,
};

use storage::rocksdb_storage::{default_rocksdb, PrefixedRocksDbStorage};
use tempdir::TempDir;

use crate::Merk;

/// Wraps a Merk instance and deletes it from disk it once it goes out of scope.
pub struct TempMerk {
    pub inner: Merk<PrefixedRocksDbStorage>,
    pub path: TempDir,
    _db: Arc<rocksdb::OptimisticTransactionDB>,
}

impl TempMerk {
    /// Opens a `TempMerk` at an autogenerated, temporary file path.
    pub fn new() -> TempMerk {
        let path = TempDir::new("db").expect("cannot create tempdir");
        let db = default_rocksdb(path.path());
        let inner = PrefixedRocksDbStorage::new(db.clone(), Vec::new())
            .expect("cannot create prefixed storage");
        TempMerk {
            inner: Merk::open(inner).expect("cannot open Merk"),
            path,
            _db: db,
        }
    }
}

impl Default for TempMerk {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for TempMerk {
    type Target = Merk<PrefixedRocksDbStorage>;

    fn deref(&self) -> &Merk<PrefixedRocksDbStorage> {
        &self.inner
    }
}

impl DerefMut for TempMerk {
    fn deref_mut(&mut self) -> &mut Merk<PrefixedRocksDbStorage> {
        &mut self.inner
    }
}
