//! Prefixed storage raw iterator implementation for RocksDB backend.
use crate::RawIterator;

/// Raw iterator over prefixed storage.
pub struct PrefixedRocksDbRawIterator;

impl RawIterator for PrefixedRocksDbRawIterator {
    fn seek_to_first(&mut self) {
        todo!()
    }

    fn seek_to_last(&mut self) {
        todo!()
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) {
        todo!()
    }

    fn next(&mut self) {
        todo!()
    }

    fn prev(&mut self) {
        todo!()
    }

    fn value(&self) -> Option<&[u8]> {
        todo!()
    }

    fn key(&self) -> Option<&[u8]> {
        todo!()
    }

    fn valid(&self) -> bool {
        todo!()
    }
}
