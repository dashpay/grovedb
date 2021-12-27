//! Storage implementation using RocksDB
use std::{path::Path, rc::Rc};

pub use rocksdb::{checkpoint::Checkpoint, Error, DB};
use rocksdb::{ColumnFamily, ColumnFamilyDescriptor, DBRawIterator, WriteBatch};

use crate::{Batch, RawIterator, Storage};

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
pub fn default_rocksdb(path: &Path) -> Rc<rocksdb::DB> {
    Rc::new(
        rocksdb::DB::open_cf_descriptors(&default_db_opts(), &path, column_families())
            .expect("cannot create rocksdb"),
    )
}

fn make_prefixed_key(prefix: Vec<u8>, key: &[u8]) -> Vec<u8> {
    let mut prefixed_key = prefix.clone();
    prefixed_key.extend_from_slice(key);
    prefixed_key
}

/// RocksDB wrapper to store items with prefixes
pub struct PrefixedRocksDbStorage {
    db: Rc<rocksdb::DB>,
    prefix: Vec<u8>,
}

#[derive(thiserror::Error, Debug)]
pub enum PrefixedRocksDbStorageError {
    #[error("column family not found: {0}")]
    ColumnFamilyNotFound(&'static str),
    #[error(transparent)]
    RocksDbError(#[from] rocksdb::Error),
}

impl PrefixedRocksDbStorage {
    /// Wraps RocksDB to prepend prefixes to each operation
    pub fn new(db: Rc<rocksdb::DB>, prefix: Vec<u8>) -> Result<Self, PrefixedRocksDbStorageError> {
        Ok(PrefixedRocksDbStorage { prefix, db })
    }

    /// Get auxiliary data column family
    fn cf_aux(&self) -> Result<&rocksdb::ColumnFamily, PrefixedRocksDbStorageError> {
        self.db
            .cf_handle(AUX_CF_NAME)
            .ok_or(PrefixedRocksDbStorageError::ColumnFamilyNotFound(
                AUX_CF_NAME,
            ))
    }

    /// Get trees roots data column family
    fn cf_roots(&self) -> Result<&rocksdb::ColumnFamily, PrefixedRocksDbStorageError> {
        self.db
            .cf_handle(ROOTS_CF_NAME)
            .ok_or(PrefixedRocksDbStorageError::ColumnFamilyNotFound(
                ROOTS_CF_NAME,
            ))
    }

    /// Get metadata column family
    fn cf_meta(&self) -> Result<&rocksdb::ColumnFamily, PrefixedRocksDbStorageError> {
        self.db
            .cf_handle(META_CF_NAME)
            .ok_or(PrefixedRocksDbStorageError::ColumnFamilyNotFound(
                META_CF_NAME,
            ))
    }
}

impl Storage for PrefixedRocksDbStorage {
    type Batch<'a> = PrefixedRocksDbBatch<'a>;
    type Error = PrefixedRocksDbStorageError;
    type RawIterator<'a> = RawPrefixedIterator<'a>;

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        self.db
            .put(make_prefixed_key(self.prefix.clone(), key), value)?;
        Ok(())
    }

    fn put_aux(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        self.db.put_cf(
            self.cf_aux()?,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )?;
        Ok(())
    }

    fn put_root(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        self.db.put_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )?;
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), Self::Error> {
        self.db
            .delete(make_prefixed_key(self.prefix.clone(), key))?;
        Ok(())
    }

    fn delete_aux(&self, key: &[u8]) -> Result<(), Self::Error> {
        self.db
            .delete_cf(self.cf_aux()?, make_prefixed_key(self.prefix.clone(), key))?;
        Ok(())
    }

    fn delete_root(&self, key: &[u8]) -> Result<(), Self::Error> {
        self.db.delete_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
        )?;
        Ok(())
    }

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.db.get(make_prefixed_key(self.prefix.clone(), key))?)
    }

    fn get_aux(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self
            .db
            .get_cf(self.cf_aux()?, make_prefixed_key(self.prefix.clone(), key))?)
    }

    fn get_root(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.db.get_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
        )?)
    }

    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        Ok(self.db.put_cf(self.cf_meta()?, key, value)?)
    }

    fn delete_meta(&self, key: &[u8]) -> Result<(), Self::Error> {
        Ok(self.db.delete_cf(self.cf_meta()?, key)?)
    }

    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.db.get_cf(self.cf_meta()?, key)?)
    }

    fn new_batch<'a>(&'a self) -> Result<Self::Batch<'a>, Self::Error> {
        Ok(PrefixedRocksDbBatch {
            prefix: self.prefix.clone(),
            batch: WriteBatch::default(),
            cf_aux: self.cf_aux()?,
            cf_roots: self.cf_roots()?,
        })
    }

    fn commit_batch<'a>(&'a self, batch: Self::Batch<'a>) -> Result<(), Self::Error> {
        self.db.write(batch.batch)?;
        Ok(())
    }

    fn flush(&self) -> Result<(), Self::Error> {
        self.db.flush()?;
        Ok(())
    }

    fn raw_iter<'a>(&'a self) -> Self::RawIterator<'a> {
        RawPrefixedIterator {
            rocksdb_iterator: self.db.raw_iterator(),
            prefix: &self.prefix,
        }
    }
}

pub struct RawPrefixedIterator<'a> {
    rocksdb_iterator: DBRawIterator<'a>,
    prefix: &'a [u8],
}

impl RawIterator for RawPrefixedIterator<'_> {
    fn seek_to_first(&mut self) {
        self.rocksdb_iterator.seek(self.prefix);
    }

    fn seek(&mut self, key: &[u8]) {
        self.rocksdb_iterator
            .seek(make_prefixed_key(self.prefix.to_vec(), key));
    }

    fn next(&mut self) {
        self.rocksdb_iterator.next();
    }

    fn value(&self) -> Option<&[u8]> {
        if self.valid() {
            self.rocksdb_iterator.value()
        } else {
            None
        }
    }

    fn key(&self) -> Option<&[u8]> {
        if self.valid() {
            self.rocksdb_iterator
                .key()
                .map(|k| k.split_at(self.prefix.len()).1)
        } else {
            None
        }
    }

    fn valid(&self) -> bool {
        self.rocksdb_iterator
            .key()
            .map(|k| k.starts_with(self.prefix))
            .unwrap_or(false)
    }
}

/// Wrapper to RocksDB batch
pub struct PrefixedRocksDbBatch<'a> {
    prefix: Vec<u8>,
    batch: rocksdb::WriteBatch,
    cf_aux: &'a ColumnFamily,
    cf_roots: &'a ColumnFamily,
}

impl<'a> Batch for PrefixedRocksDbBatch<'a> {
    fn put(&mut self, key: &[u8], value: &[u8]) {
        self.batch
            .put(make_prefixed_key(self.prefix.clone(), key), value)
    }

    fn put_aux(&mut self, key: &[u8], value: &[u8]) {
        self.batch.put_cf(
            self.cf_aux,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )
    }

    fn put_root(&mut self, key: &[u8], value: &[u8]) {
        self.batch.put_cf(
            self.cf_roots,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )
    }

    fn delete(&mut self, key: &[u8]) {
        self.batch
            .delete(make_prefixed_key(self.prefix.clone(), key))
    }

    fn delete_aux(&mut self, key: &[u8]) {
        self.batch
            .delete_cf(self.cf_aux, make_prefixed_key(self.prefix.clone(), key))
    }

    fn delete_root(&mut self, key: &[u8]) {
        self.batch
            .delete_cf(self.cf_roots, make_prefixed_key(self.prefix.clone(), key))
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
        let another_storage_after = PrefixedRocksDbStorage::new(db, b"zanothersomeprefix".to_vec())
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

        let mut iter = storage.raw_iter();
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
