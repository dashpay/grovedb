//! Impementation for a storage abstraction over RocksDB.
use std::path::Path;

use lazy_static::lazy_static;
use rocksdb::{ColumnFamilyDescriptor, Error, OptimisticTransactionDB, Transaction};

use super::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext};
use crate::Storage;

/// Name of column family used to store auxiliary data
pub(super) const AUX_CF_NAME: &str = "aux";
/// Name of column family used to store subtrees roots data
pub(super) const ROOTS_CF_NAME: &str = "roots";
/// Name of column family used to store metadata
pub(super) const META_CF_NAME: &str = "meta";

lazy_static! {
    static ref DEFAULT_OPTS: rocksdb::Options = {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.increase_parallelism(num_cpus::get() as i32);
        opts.set_allow_mmap_writes(true);
        opts.set_allow_mmap_reads(true);
        opts.create_missing_column_families(true);
        opts.set_atomic_flush(true);
        opts
    };
}

/// Storage which uses RocksDB as its backend.
pub struct RocksDbStorage {
    db: OptimisticTransactionDB,
}

impl RocksDbStorage {
    pub fn default_rocksdb_with_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = rocksdb::OptimisticTransactionDB::open_cf_descriptors(
            &DEFAULT_OPTS,
            &path,
            [
                ColumnFamilyDescriptor::new(AUX_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(ROOTS_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(META_CF_NAME, DEFAULT_OPTS.clone()),
            ],
        )?;

        Ok(RocksDbStorage { db })
    }

    pub fn get_prefixed_context(&self, prefix: Vec<u8>) -> PrefixedRocksDbStorageContext {
        PrefixedRocksDbStorageContext::new(&self.db, prefix)
    }

    pub fn get_prefixed_context_from_path<'p, P>(&self, path: P) -> PrefixedRocksDbStorageContext
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        let prefix = Self::build_prefix(path);
        PrefixedRocksDbStorageContext::new(&self.db, prefix)
    }

    pub fn get_prefixed_transactional_context<'a>(
        &'a self,
        prefix: Vec<u8>,
        transaction: &'a <Self as Storage>::Transaction,
    ) -> PrefixedRocksDbTransactionContext {
        PrefixedRocksDbTransactionContext::new(&self.db, transaction, prefix)
    }

    pub fn get_prefixed_transactional_context_from_path<'a, 'p, P>(
        &'a self,
        path: P,
        transaction: &'a <Self as Storage>::Transaction,
    ) -> PrefixedRocksDbTransactionContext
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        let prefix = Self::build_prefix(path);
        PrefixedRocksDbTransactionContext::new(&self.db, transaction, prefix)
    }

    /// A helper method to build a prefix to rocksdb keys or identify a subtree
    /// in `subtrees` map by tree path;
    pub fn build_prefix<'a, P>(path: P) -> Vec<u8>
    where
        P: IntoIterator<Item = &'a [u8]>,
    {
        let segments_iter = path.into_iter();
        let mut segments_count: usize = 0;
        let mut res = Vec::new();
        let mut lengthes = Vec::new();

        for s in segments_iter {
            segments_count += 1;
            res.extend_from_slice(s);
            lengthes.extend(s.len().to_ne_bytes());
        }

        res.extend(segments_count.to_ne_bytes());
        res.extend(lengthes);
        res = blake3::hash(&res).as_bytes().to_vec();
        res
    }
}

impl<'db> Storage<'db> for RocksDbStorage {
    type Error = Error;
    type Transaction = Transaction<'db, OptimisticTransactionDB>;

    fn start_transaction(&'db self) -> Self::Transaction {
        self.db.transaction()
    }

    fn commit_transaction(&self, transaction: Self::Transaction) -> Result<(), Self::Error> {
        transaction.commit()
    }

    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Self::Error> {
        transaction.rollback()
    }

    fn flush(&self) -> Result<(), Self::Error> {
        self.db.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_prefix() {
        let path_a = [b"aa".as_ref(), b"b"];
        let path_b = [b"a".as_ref(), b"ab"];
        assert_ne!(
            RocksDbStorage::build_prefix(path_a),
            RocksDbStorage::build_prefix(path_b),
        );
        assert_eq!(
            RocksDbStorage::build_prefix(path_a),
            RocksDbStorage::build_prefix(path_a),
        );
    }
}
