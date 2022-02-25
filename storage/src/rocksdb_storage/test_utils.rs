use std::ops::Deref;

use tempfile::TempDir;

use super::*;

pub struct TempStorage {
    _dir: TempDir,
    storage: RocksDbStorage,
}

impl TempStorage {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("cannot create tempir");
        let storage = RocksDbStorage::default_rocksdb_with_path(dir.path())
            .expect("cannot open RocksDB storage");
        TempStorage { _dir: dir, storage }
    }
}

impl Deref for TempStorage {
    type Target = RocksDbStorage;

    fn deref(&self) -> &Self::Target {
        &self.storage
    }
}
