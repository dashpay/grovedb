//! Prefixed storage raw iterator implementation for RocksDB backend.
use rocksdb::DBRawIteratorWithThreadMode;

use super::make_prefixed_key;
use crate::{
    rocksdb_storage::storage::{Db, Tx},
    RawIterator,
};

/// Raw iterator over prefixed storage.
pub struct PrefixedRocksDbRawIterator<I> {
    pub(super) prefix: Vec<u8>,
    pub(super) raw_iterator: I,
}

impl<'a> RawIterator for PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'a, Db>> {
    fn seek_to_first(&mut self) {
        self.raw_iterator.seek(&self.prefix)
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
        self.raw_iterator.seek_for_prev(prefix_vec)
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) {
        self.raw_iterator
            .seek(make_prefixed_key(self.prefix.to_vec(), key))
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) {
        self.raw_iterator
            .seek_for_prev(make_prefixed_key(self.prefix.to_vec(), key))
    }

    fn next(&mut self) {
        self.raw_iterator.next()
    }

    fn prev(&mut self) {
        self.raw_iterator.prev()
    }

    fn value(&self) -> Option<&[u8]> {
        if self.valid() {
            self.raw_iterator.value()
        } else {
            None
        }
    }

    fn key(&self) -> Option<&[u8]> {
        if self.valid() {
            self.raw_iterator
                .key()
                .map(|k| k.split_at(self.prefix.len()).1)
        } else {
            None
        }
    }

    fn valid(&self) -> bool {
        self.raw_iterator
            .key()
            .map(|k| k.starts_with(&self.prefix))
            .unwrap_or(false)
    }
}

impl<'a> RawIterator for PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'a, Tx<'a>>> {
    fn seek_to_first(&mut self) {
        self.raw_iterator.seek(&self.prefix)
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
        self.raw_iterator.seek_for_prev(prefix_vec)
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) {
        self.raw_iterator
            .seek(make_prefixed_key(self.prefix.to_vec(), key))
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) {
        self.raw_iterator
            .seek_for_prev(make_prefixed_key(self.prefix.to_vec(), key))
    }

    fn next(&mut self) {
        self.raw_iterator.next()
    }

    fn prev(&mut self) {
        self.raw_iterator.prev()
    }

    fn value(&self) -> Option<&[u8]> {
        if self.valid() {
            self.raw_iterator.value()
        } else {
            None
        }
    }

    fn key(&self) -> Option<&[u8]> {
        if self.valid() {
            self.raw_iterator
                .key()
                .map(|k| k.split_at(self.prefix.len()).1)
        } else {
            None
        }
    }

    fn valid(&self) -> bool {
        self.raw_iterator
            .key()
            .map(|k| k.starts_with(&self.prefix))
            .unwrap_or(false)
    }
}
