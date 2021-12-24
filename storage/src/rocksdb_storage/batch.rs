use rocksdb::ColumnFamily;
use crate::Batch;
use super::make_prefixed_key;

/// Wrapper to RocksDB batch
pub struct PrefixedRocksDbBatch<'a> {
    pub prefix: Vec<u8>,
    pub batch: rocksdb::WriteBatchWithTransaction<true>,
    pub cf_aux: &'a ColumnFamily,
    pub cf_roots: &'a ColumnFamily,
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