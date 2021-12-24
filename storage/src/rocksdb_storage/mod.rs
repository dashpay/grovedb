//! Storage implementation using RocksDB
use std::{path::Path, rc::Rc};
pub use rocksdb::{checkpoint::Checkpoint, Error, OptimisticTransactionDB};
use rocksdb::{ColumnFamilyDescriptor, DBRawIterator};
use crate::{RawIterator};

mod transaction;
mod batch;
mod storage;

pub use batch::PrefixedRocksDbBatch;
pub use transaction::PrefixedRocksDbTransaction;
pub use storage::{PrefixedRocksDbStorage, PrefixedRocksDbStorageError};

const AUX_CF_NAME: &str = "aux";
const ROOTS_CF_NAME: &str = "roots";
const META_CF_NAME: &str = "meta";

/// RocksDB options
pub fn default_db_opts() -> rocksdb::Options {
    let mut opts = rocksdb::Options::default();
    opts.create_if_missing(true);
    opts.increase_parallelism(num_cpus::get() as i32);
    opts.set_allow_mmap_writes(true);
    opts.set_allow_mmap_reads(true);
    opts.create_missing_column_families(true);
    opts.set_atomic_flush(true);
    opts
}

/// RocksDB column families
pub fn column_families() -> Vec<ColumnFamilyDescriptor> {
    vec![
        ColumnFamilyDescriptor::new(AUX_CF_NAME, default_db_opts()),
        ColumnFamilyDescriptor::new(ROOTS_CF_NAME, default_db_opts()),
        ColumnFamilyDescriptor::new(META_CF_NAME, default_db_opts()),
    ]
}

/// Create RocksDB with default settings
pub fn default_rocksdb(path: &Path) -> Rc<rocksdb::OptimisticTransactionDB> {
    Rc::new(
        rocksdb::OptimisticTransactionDB::open_cf_descriptors(&default_db_opts(), &path, column_families())
            .expect("cannot create rocksdb"),
    )
}

fn make_prefixed_key(prefix: Vec<u8>, key: &[u8]) -> Vec<u8> {
    let mut prefixed_key = prefix.clone();
    prefixed_key.extend_from_slice(key);
    prefixed_key
}

pub type DBRawTransactionIterator<'a> = rocksdb::DBRawIteratorWithThreadMode<'a, OptimisticTransactionDB>;

impl RawIterator for DBRawTransactionIterator<'_> {
    fn seek_to_first(&mut self) {
        DBRawTransactionIterator::seek_to_first(self)
    }

    fn seek(&mut self, key: &[u8]) {
        DBRawTransactionIterator::seek(self, key)
    }

    fn next(&mut self) {
        DBRawTransactionIterator::next(self)
    }

    fn value(&self) -> Option<&[u8]> {
        DBRawTransactionIterator::value(self)
    }

    fn key(&self) -> Option<&[u8]> {
        DBRawTransactionIterator::key(self)
    }

    fn valid(&self) -> bool {
        DBRawTransactionIterator::valid(self)
    }
}

impl RawIterator for rocksdb::DBRawIterator<'_> {
    fn seek_to_first(&mut self) {
        DBRawIterator::seek_to_first(self)
    }

    fn seek(&mut self, key: &[u8]) {
        DBRawIterator::seek(self, key)
    }

    fn next(&mut self) {
        DBRawIterator::next(self)
    }

    fn value(&self) -> Option<&[u8]> {
        DBRawIterator::value(self)
    }

    fn key(&self) -> Option<&[u8]> {
        DBRawIterator::key(self)
    }

    fn valid(&self) -> bool {
        DBRawIterator::valid(self)
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Deref;

    use tempdir::TempDir;

    use super::*;

    struct TempPrefixedStorage {
        storage: PrefixedRocksDbStorage,
        _tmp_dir: TempDir,
    }

    impl Deref for TempPrefixedStorage {
        type Target = PrefixedRocksDbStorage;

        fn deref(&self) -> &Self::Target {
            &self.storage
        }
    }

    impl TempPrefixedStorage {
        fn new() -> Self {
            let tmp_dir = TempDir::new("db").expect("cannot open tempdir");
            TempPrefixedStorage {
                storage: PrefixedRocksDbStorage::new(
                    default_rocksdb(tmp_dir.path()),
                    b"test".to_vec(),
                )
                    .expect("cannot create prefixed rocksdb storage"),
                _tmp_dir: tmp_dir,
            }
        }
    }

    #[test]
    fn test_get_put() {
        let storage = TempPrefixedStorage::new();
        storage
            .put(b"key", b"value")
            .expect("cannot put into storage");
        assert_eq!(
            storage.get(b"key").expect("cannot get by key").unwrap(),
            b"value"
        );
        assert_eq!(
            storage
                .db
                .get(b"testkey")
                .expect("cannot get by prefixed key")
                .unwrap(),
            b"value"
        );
    }

    #[test]
    fn test_get_put_aux() {
        let storage = TempPrefixedStorage::new();
        storage
            .put_aux(b"key", b"value")
            .expect("cannot put into aux storage");
        assert_eq!(
            storage.get_aux(b"key").expect("cannot get by key").unwrap(),
            b"value"
        );
        assert_eq!(
            storage
                .db
                .get_cf(&storage.db.cf_handle(AUX_CF_NAME).unwrap(), b"testkey")
                .expect("cannot get by prefixed key")
                .unwrap(),
            b"value"
        );
    }

    #[test]
    fn test_get_put_root() {
        let storage = TempPrefixedStorage::new();
        storage
            .put_root(b"key", b"value")
            .expect("cannot put into roots storage");
        assert_eq!(
            storage
                .get_root(b"key")
                .expect("cannot get by key")
                .unwrap(),
            b"value"
        );
        assert_eq!(
            storage
                .db
                .get_cf(&storage.db.cf_handle(ROOTS_CF_NAME).unwrap(), b"testkey")
                .expect("cannot get by prefixed key")
                .unwrap(),
            b"value"
        );
    }

    #[test]
    fn test_get_put_meta() {
        let storage = TempPrefixedStorage::new();
        storage
            .put_meta(b"key", b"value")
            .expect("cannot put into metadata storage");
        assert_eq!(
            storage
                .get_meta(b"key")
                .expect("cannot get by key")
                .unwrap(),
            b"value"
        );

        // Note that metadata storage requires no prefixes

        assert!(storage
            .db
            .get_cf(&storage.db.cf_handle(META_CF_NAME).unwrap(), b"testkey")
            .expect("cannot get by prefixed key")
            .is_none());
        assert_eq!(
            storage
                .db
                .get_cf(&storage.db.cf_handle(META_CF_NAME).unwrap(), b"key")
                .expect("cannot get by prefixed key")
                .unwrap(),
            b"value"
        );
    }

    #[test]
    fn test_delete() {
        let storage = TempPrefixedStorage::new();
        storage
            .put(b"key", b"value")
            .expect("cannot put into storage");
        storage.delete(b"key").expect("cannot delete from storage");
        assert!(storage
            .db
            .get(b"testkey")
            .expect("cannot get by prefixed key")
            .is_none());
    }

    #[test]
    fn test_delete_aux() {
        let storage = TempPrefixedStorage::new();
        storage
            .put_aux(b"key", b"value")
            .expect("cannot put into aux storage");
        storage
            .delete_aux(b"key")
            .expect("cannot delete from storage");
        assert!(storage
            .db
            .get_cf(&storage.db.cf_handle(AUX_CF_NAME).unwrap(), b"testkey")
            .expect("cannot get by prefixed key")
            .is_none());
    }

    #[test]
    fn test_delete_root() {
        let storage = TempPrefixedStorage::new();
        storage
            .put_root(b"key", b"value")
            .expect("cannot put into storage");
        storage
            .delete_root(b"key")
            .expect("cannot delete from storage");
        assert!(storage
            .db
            .get_cf(&storage.db.cf_handle(ROOTS_CF_NAME).unwrap(), b"testkey")
            .expect("cannot get by prefixed key")
            .is_none());
    }

    #[test]
    fn test_delete_meta() {
        let storage = TempPrefixedStorage::new();
        storage
            .put_meta(b"key", b"value")
            .expect("cannot put into storage");
        storage
            .delete_meta(b"key")
            .expect("cannot delete from storage");
        assert!(storage
            .db
            .get_cf(&storage.db.cf_handle(META_CF_NAME).unwrap(), b"key")
            .expect("cannot get by prefixed key")
            .is_none());
    }

    #[test]
    fn test_batch() {
        let storage = TempPrefixedStorage::new();
        let mut batch = storage.new_batch().expect("cannot create batch");
        batch.put(b"key1", b"value1");
        batch.put(b"key2", b"value2");
        batch.put_root(b"root", b"yeet");
        storage.commit_batch(batch).expect("cannot commit batch");
        assert_eq!(
            storage
                .get(b"key1")
                .expect("cannot get a value by key1")
                .unwrap(),
            b"value1"
        );
        assert_eq!(
            storage
                .get(b"key2")
                .expect("cannot get a value by key2")
                .unwrap(),
            b"value2"
        );
        assert_eq!(
            storage
                .get_root(b"root")
                .expect("cannot get a root value")
                .unwrap(),
            b"yeet"
        );
    }

    #[test]
    fn transaction_commit_should_work() {
        let storage = TempPrefixedStorage::new();
        let mut transaction = storage.transaction();
        transaction.put(b"key1", b"value1");
        transaction.put(b"key2", b"value2");
        transaction.put_root(b"root", b"yeet");
    }
}