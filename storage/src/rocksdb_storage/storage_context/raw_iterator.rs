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

use grovedb_costs::{CostContext, CostsExt};
use rocksdb::{DBAccess, DBRawIteratorWithThreadMode};

use super::make_prefixed_key;
use crate::{rocksdb_storage::storage::SubtreePrefix, Error, RawIterator, COST_MODEL};

/// Raw iterator over prefixed storage_cost.
pub struct PrefixedRocksDbRawIterator<I> {
    pub(super) prefix: SubtreePrefix,
    pub(super) raw_iterator: I,
}

impl<I> PrefixedRocksDbRawIterator<I> {
    /// Re-scope this iterator to a different subtree prefix while keeping
    /// the underlying RocksDB iterator — and therefore its pinned snapshot
    /// view — alive. A multi-subtree read that must be internally
    /// consistent (e.g. discovering a subtree's root key in one subtree
    /// and then traversing that subtree) can hop scopes without giving up
    /// the view; consuming `self` makes the hand-off explicit, so one
    /// pinned view serves one subtree at a time, in sequence.
    pub fn retarget(self, prefix: SubtreePrefix) -> Self {
        Self {
            prefix,
            raw_iterator: self.raw_iterator,
        }
    }
}

impl<D: DBAccess> PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'_, D>> {
    /// The stored key the iterator stands on, when it is a key of this
    /// iterator's subtree.
    ///
    /// The underlying storage is a stack of blocks, one per prefix. A key
    /// of the next block, and the end of the last block, are both "not a
    /// record of this subtree": the charge for looking at either is a fixed
    /// one, so that it does not depend on which blocks are neighbours.
    fn stored_key(&self) -> Option<&[u8]> {
        self.raw_iterator
            .key()
            .filter(|key| key.starts_with(&self.prefix))
    }
}

impl<D: DBAccess> RawIterator for PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'_, D>> {
    fn seek_to_first(&mut self) -> CostContext<()> {
        self.raw_iterator.seek(self.prefix);
        ().wrap_with_cost(COST_MODEL.cursor_move())
    }

    /// Seeks to the last key with this prefix by incrementing the prefix
    /// and using `seek_for_prev`. If every byte of the prefix is 0xFF, the
    /// increment would wrap to all zeros, which would be incorrect. However,
    /// this cannot happen in practice: prefixes are 32-byte Blake3 hashes,
    /// so an all-0xFF prefix has probability 1/2^256 — effectively zero.
    fn seek_to_last(&mut self) -> CostContext<()> {
        let mut prefix_vec = self.prefix.to_vec();
        for i in (0..prefix_vec.len()).rev() {
            prefix_vec[i] = prefix_vec[i].wrapping_add(1);
            if prefix_vec[i] != 0 {
                // if it is == 0 then we need to go to next bit
                break;
            }
        }
        self.raw_iterator.seek_for_prev(prefix_vec);
        ().wrap_with_cost(COST_MODEL.cursor_move())
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.raw_iterator.seek(make_prefixed_key(&self.prefix, key));
        ().wrap_with_cost(COST_MODEL.cursor_move())
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.raw_iterator
            .seek_for_prev(make_prefixed_key(&self.prefix, key));
        ().wrap_with_cost(COST_MODEL.cursor_move())
    }

    fn next(&mut self) -> CostContext<()> {
        self.raw_iterator.next();
        ().wrap_with_cost(COST_MODEL.cursor_move())
    }

    fn prev(&mut self) -> CostContext<()> {
        self.raw_iterator.prev();
        ().wrap_with_cost(COST_MODEL.cursor_move())
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        let stored_key = self.stored_key();
        let value = stored_key.and_then(|_| self.raw_iterator.value());
        let cost = match (stored_key, value) {
            (Some(key), Some(value)) => COST_MODEL.cursor_value(Some((key.len(), value.len()))),
            (key, _) => COST_MODEL.cursor_key(key.map(<[u8]>::len)),
        };
        value.wrap_with_cost(cost)
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        let stored_key = self.stored_key();
        stored_key
            .map(|key| key.split_at(self.prefix.len()).1)
            .wrap_with_cost(COST_MODEL.cursor_key(stored_key.map(<[u8]>::len)))
    }

    fn valid(&self) -> CostContext<bool> {
        let stored_key = self.stored_key();
        stored_key
            .is_some()
            .wrap_with_cost(COST_MODEL.cursor_key(stored_key.map(<[u8]>::len)))
    }

    fn status(&self) -> Result<(), Error> {
        self.raw_iterator.status().map_err(Error::RocksDBError)
    }
}
