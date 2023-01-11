// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Prefixed storage_cost raw iterator implementation for RocksDB backend.

use costs::{CostContext, CostsExt, OperationCost};
use rocksdb::DBRawIteratorWithThreadMode;

use super::make_prefixed_key;
use crate::{
    rocksdb_storage::storage::{Db, Tx},
    RawIterator,
};

/// Raw iterator over prefixed storage_cost.
pub struct PrefixedRocksDbRawIterator<I> {
    pub(super) prefix: Vec<u8>,
    pub(super) raw_iterator: I,
}

impl<'a> RawIterator for PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'a, Db>> {
    fn seek_to_first(&mut self) -> CostContext<()> {
        self.raw_iterator.seek(&self.prefix);
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        let mut prefix_vec = self.prefix.to_vec();
        for i in (0..prefix_vec.len()).rev() {
            prefix_vec[i] += 1;
            if prefix_vec[i] != 0 {
                // if it is == 0 then we need to go to next bit
                break;
            }
        }
        self.raw_iterator.seek_for_prev(prefix_vec);
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.raw_iterator
            .seek(make_prefixed_key(self.prefix.to_vec(), key));
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.raw_iterator
            .seek_for_prev(make_prefixed_key(self.prefix.to_vec(), key));
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn next(&mut self) -> CostContext<()> {
        self.raw_iterator.next();
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn prev(&mut self) -> CostContext<()> {
        self.raw_iterator.prev();
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        let mut cost = OperationCost::default();

        let value = if self.valid().unwrap_add_cost(&mut cost) {
            self.raw_iterator.value().map(|v| {
                cost.storage_loaded_bytes += v.len() as u32;
                v
            })
        } else {
            None
        };

        value.wrap_with_cost(cost)
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        let mut cost = OperationCost::default();

        let value = self.raw_iterator.key().and_then(|k| {
            // Even if we truncate prefix, loaded cost should be maximum for the whole
            // function
            cost.storage_loaded_bytes += k.len() as u32;
            if k.starts_with(&self.prefix) {
                Some(k.split_at(self.prefix.len()).1)
            } else {
                None
            }
        });

        value.wrap_with_cost(cost)
    }

    fn valid(&self) -> CostContext<bool> {
        let mut cost = OperationCost::default();

        self.raw_iterator
            .key()
            .map(|k| {
                cost.storage_loaded_bytes += k.len() as u32;
                k.starts_with(&self.prefix)
            })
            .unwrap_or(false)
            .wrap_with_cost(cost)
    }
}

impl<'a> RawIterator for PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'a, Tx<'a>>> {
    fn seek_to_first(&mut self) -> CostContext<()> {
        self.raw_iterator.seek(&self.prefix);
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        let mut prefix_vec = self.prefix.to_vec();
        for i in (0..prefix_vec.len()).rev() {
            prefix_vec[i] += 1;
            if prefix_vec[i] != 0 {
                // if it is == 0 then we need to go to next bit
                break;
            }
        }
        self.raw_iterator.seek_for_prev(prefix_vec);
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.raw_iterator
            .seek(make_prefixed_key(self.prefix.to_vec(), key));
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.raw_iterator
            .seek_for_prev(make_prefixed_key(self.prefix.to_vec(), key));
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn next(&mut self) -> CostContext<()> {
        self.raw_iterator.next();
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn prev(&mut self) -> CostContext<()> {
        self.raw_iterator.prev();
        ().wrap_with_cost(OperationCost::with_seek_count(1))
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        let mut cost = OperationCost::default();

        let value = if self.valid().unwrap_add_cost(&mut cost) {
            self.raw_iterator.value().map(|v| {
                cost.storage_loaded_bytes += v.len() as u32;
                v
            })
        } else {
            None
        };

        value.wrap_with_cost(cost)
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        let mut cost = OperationCost::default();

        let value = self.raw_iterator.key().and_then(|k| {
            // Even if we truncate prefix, loaded cost should be maximum for the whole
            // function
            cost.storage_loaded_bytes += k.len() as u32;
            if k.starts_with(&self.prefix) {
                Some(k.split_at(self.prefix.len()).1)
            } else {
                None
            }
        });

        value.wrap_with_cost(cost)
    }

    fn valid(&self) -> CostContext<bool> {
        let mut cost = OperationCost::default();

        self.raw_iterator
            .key()
            .map(|k| {
                cost.storage_loaded_bytes += k.len() as u32;
                k.starts_with(&self.prefix)
            })
            .unwrap_or(false)
            .wrap_with_cost(cost)
    }
}
