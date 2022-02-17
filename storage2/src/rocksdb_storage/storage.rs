//! Impementation for a storage abstraction over RocksDB.
use std::path::Path;

use lazy_static::lazy_static;
use rocksdb::{ColumnFamilyDescriptor, Error, OptimisticTransactionDB, Transaction};

use crate::Storage;

/// Name of column family used to store auxiliary data
const AUX_CF_NAME: &str = "aux";
/// Name of column family used to store subtrees roots data
const ROOTS_CF_NAME: &str = "roots";
/// Name of column family used to store metadata
const META_CF_NAME: &str = "meta";

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
}

impl<'a> Storage<'a> for RocksDbStorage {
    type Error = Error;
    type Transaction = Transaction<'a, OptimisticTransactionDB>;

    fn start_transaction(&'a self) -> Self::Transaction {
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
