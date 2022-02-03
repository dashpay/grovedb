//! Storage implementation using RocksDB
use std::{path::Path, rc::Rc};

pub use rocksdb::{checkpoint::Checkpoint, Error, OptimisticTransactionDB};
use rocksdb::{ColumnFamilyDescriptor, DBRawIteratorWithThreadMode};

use crate::{DBTransaction, RawIterator};

mod batch;
mod storage;
mod transaction;

pub use batch::PrefixedRocksDbBatch;
pub use transaction::PrefixedRocksDbTransaction;

pub use self::storage::{PrefixedRocksDbStorage, PrefixedRocksDbStorageError};

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

pub type OptimisticTransactionDBTransaction<'a> = rocksdb::Transaction<'a, OptimisticTransactionDB>;

impl<'a> DBTransaction<'a> for OptimisticTransactionDBTransaction<'a> {}

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
        rocksdb::OptimisticTransactionDB::open_cf_descriptors(
            &default_db_opts(),
            &path,
            column_families(),
        )
        .expect("cannot create rocksdb"),
    )
}

fn make_prefixed_key<K: AsRef<[u8]>>(mut prefix: Vec<u8>, key: K) -> Vec<u8> {
    prefix.extend_from_slice(key.as_ref());
    prefix
}

// There is no public API to abstract over raw iterators yet
enum RawIteratorVariant<'a> {
    StorageIterator(DBRawIteratorWithThreadMode<'a, OptimisticTransactionDB>),
    TransactionIterator(
        DBRawIteratorWithThreadMode<'a, rocksdb::Transaction<'a, OptimisticTransactionDB>>,
    ),
}

pub struct RawPrefixedTransactionalIterator<'a> {
    rocksdb_iterator: RawIteratorVariant<'a>,
    prefix: &'a [u8],
}

macro_rules! iterator_call {
    (mut $self:ident, $($call:tt)+) => {
        iterator_call!(@branches &mut $self.rocksdb_iterator, $($call)+)
    };

    ($self:ident, $($call:tt)+) => {
        iterator_call!(@branches &$self.rocksdb_iterator, $($call)+)
    };

    (@branches $iter:expr, $($call:tt)+) => {
        match $iter {
            RawIteratorVariant::StorageIterator(i) => i.$($call)+,
            RawIteratorVariant::TransactionIterator(i) => i.$($call)+
        }
    }
}

impl RawIterator for RawPrefixedTransactionalIterator<'_> {
    fn seek_to_first(&mut self) {
        iterator_call!(mut self, seek(self.prefix));
    }

    fn seek_to_last(&mut self) {
        let mut prefix_vec = self.prefix.to_vec();
        for i in (0..prefix_vec.len()).rev() {
            prefix_vec[i] += 1;
            if prefix_vec[i] != 0 {
                // if it is == 0 then we need to go to next bit
                break;
            }
        }
        iterator_call!(mut self, seek_for_prev(prefix_vec));
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) {
        iterator_call!(mut self, seek(make_prefixed_key(self.prefix.to_vec(), key)));
    }

    fn next(&mut self) {
        iterator_call!(mut self, next());
    }

    fn prev(&mut self) {
        iterator_call!(mut self, prev());
    }

    fn value(&self) -> Option<&[u8]> {
        if self.valid() {
            iterator_call!(self, value())
        } else {
            None
        }
    }

    fn key(&self) -> Option<&[u8]> {
        if self.valid() {
            iterator_call!(self, key().map(|k| k.split_at(self.prefix.len()).1))
        } else {
            None
        }
    }

    fn valid(&self) -> bool {
        iterator_call!(
            self,
            key().map(|k| k.starts_with(self.prefix)).unwrap_or(false)
        )
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Deref;

    use tempdir::TempDir;

    use super::*;
    use crate::{Batch, Storage, Transaction};

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
        let mut batch = storage.new_batch(None).expect("cannot create batch");
        batch
            .put(b"key1", b"value1")
            .expect("cannot put into batch");
        batch
            .put(b"key2", b"value2")
            .expect("cannot put into batch");
        batch
            .put_root(b"root", b"yeet")
            .expect("cannot put into batch");
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
    fn test_raw_iterator() {
        let tmp_dir = TempDir::new("test_raw_iterator").expect("unable to open a tempdir");
        let db = default_rocksdb(tmp_dir.path());

        let storage = PrefixedRocksDbStorage::new(db.clone(), b"someprefix".to_vec())
            .expect("cannot create a prefixed storage");
        storage
            .put(b"key1", b"value1")
            .expect("expected successful insertion");
        storage
            .put(b"key0", b"value0")
            .expect("expected successful insertion");
        storage
            .put(b"key3", b"value3")
            .expect("expected successful insertion");
        storage
            .put(b"key2", b"value2")
            .expect("expected successful insertion");

        // Other storages are required to put something into rocksdb with other prefix
        // to see if there will be any conflicts and boundaries are met
        let another_storage_before =
            PrefixedRocksDbStorage::new(db.clone(), b"anothersomeprefix".to_vec())
                .expect("cannot create a prefixed storage");
        another_storage_before
            .put(b"key1", b"value1")
            .expect("expected successful insertion");
        another_storage_before
            .put(b"key5", b"value5")
            .expect("expected successful insertion");
        let another_storage_after =
            PrefixedRocksDbStorage::new(db.clone(), b"zanothersomeprefix".to_vec())
                .expect("cannot create a prefixed storage");
        another_storage_after
            .put(b"key1", b"value1")
            .expect("expected successful insertion");
        another_storage_after
            .put(b"key5", b"value5")
            .expect("expected successful insertion");

        let expected: [(&'static [u8], &'static [u8]); 4] = [
            (b"key0", b"value0"),
            (b"key1", b"value1"),
            (b"key2", b"value2"),
            (b"key3", b"value3"),
        ];
        let mut expected_iter = expected.into_iter();

        // Test iterator goes forward

        let mut iter = storage.raw_iter(None);
        iter.seek_to_first();
        while iter.valid() {
            assert_eq!(
                (iter.key().unwrap(), iter.value().unwrap()),
                expected_iter.next().unwrap()
            );
            iter.next();
        }
        assert!(expected_iter.next().is_none());

        // Test `seek_to_last` on a storage with elements

        let mut iter = storage.raw_iter(None);
        iter.seek_to_last();
        assert_eq!(
            (iter.key().unwrap(), iter.value().unwrap()),
            expected.last().unwrap().clone(),
        );
        iter.next();
        assert!(!iter.valid());

        // Test `seek_to_last` on empty storage
        let empty_storage = PrefixedRocksDbStorage::new(db, b"notexist".to_vec())
            .expect("cannot create a prefixed storage");
        let mut iter = empty_storage.raw_iter(None);
        iter.seek_to_last();
        assert!(!iter.valid());
        iter.next();
        assert!(!iter.valid());
    }

    #[test]
    fn test_raw_iterator_with_transaction() {
        let tmp_dir = TempDir::new("test_raw_iterator").expect("unable to open a tempdir");
        let db = default_rocksdb(tmp_dir.path());

        let storage = PrefixedRocksDbStorage::new(db.clone(), b"someprefix".to_vec())
            .expect("cannot create a prefixed storage");
        storage
            .put(b"key1", b"value1")
            .expect("expected successful insertion");
        storage
            .put(b"key0", b"value0")
            .expect("expected successful insertion");

        let db_transaction = db.transaction();
        let transaction = storage.transaction(&db_transaction);

        transaction
            .put(b"key3", b"value3")
            .expect("expected successful insertion with transaction");
        transaction
            .put(b"key2", b"value2")
            .expect("expected successful insertion with transaction");

        let expected: [(&'static [u8], &'static [u8]); 4] = [
            (b"key0", b"value0"),
            (b"key1", b"value1"),
            (b"key2", b"value2"),
            (b"key3", b"value3"),
        ];
        let mut expected_iter = expected.into_iter();

        // Test iterator on transactional data
        let mut iter_transaction = storage.raw_iter(Some(&db_transaction));
        iter_transaction.seek_to_first();
        while iter_transaction.valid() {
            assert_eq!(
                (
                    iter_transaction.key().unwrap(),
                    iter_transaction.value().unwrap()
                ),
                expected_iter.next().unwrap()
            );
            iter_transaction.next();
        }
        assert!(expected_iter.next().is_none());
        drop(iter_transaction);

        // Test iterator on commited data
        let expected: [(&'static [u8], &'static [u8]); 2] =
            [(b"key0", b"value0"), (b"key1", b"value1")];
        let mut expected_iter = expected.into_iter();

        let mut iter = storage.raw_iter(None);
        iter.seek_to_first();
        while iter.valid() {
            assert_eq!(
                (iter.key().unwrap(), iter.value().unwrap()),
                expected_iter.next().unwrap()
            );
            iter.next();
        }
        assert!(expected_iter.next().is_none());

        // Commit data and test iterator again
        db_transaction
            .commit()
            .expect("cannot commit the transaction");

        let expected: [(&'static [u8], &'static [u8]); 4] = [
            (b"key0", b"value0"),
            (b"key1", b"value1"),
            (b"key2", b"value2"),
            (b"key3", b"value3"),
        ];
        let mut expected_iter = expected.into_iter();

        let mut iter = storage.raw_iter(None);
        iter.seek_to_first();
        while iter.valid() {
            assert_eq!(
                (iter.key().unwrap(), iter.value().unwrap()),
                expected_iter.next().unwrap()
            );
            iter.next();
        }
        assert!(expected_iter.next().is_none());
    }
}
