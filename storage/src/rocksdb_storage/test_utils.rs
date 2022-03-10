use std::{cell::Cell, ops::Deref};

use tempfile::TempDir;

use super::*;

pub struct TempStorage {
    dir: Cell<TempDir>,
    storage: RocksDbStorage,
}

impl TempStorage {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("cannot create tempir");
        let storage = RocksDbStorage::default_rocksdb_with_path(dir.path())
            .expect("cannot open RocksDB storage");
        TempStorage {
            dir: Cell::new(dir),
            storage,
        }
    }

    pub fn crash(&self) {
        drop(
            self.dir
                .replace(TempDir::new().expect("cannot create tempdir")),
        )
    }
}

impl Default for TempStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for TempStorage {
    type Target = RocksDbStorage;

    fn deref(&self) -> &Self::Target {
        &self.storage
    }
}
