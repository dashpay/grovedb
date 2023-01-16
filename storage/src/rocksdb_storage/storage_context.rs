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

//! Implementation of prefixed storage context.

mod batch;
mod context_batch_no_tx;
mod context_batch_tx;
mod context_no_tx;
mod context_tx;
mod raw_iterator;

pub use batch::PrefixedRocksDbBatch;
pub use context_batch_no_tx::PrefixedRocksDbBatchStorageContext;
pub use context_batch_tx::PrefixedRocksDbBatchTransactionContext;
pub use context_no_tx::PrefixedRocksDbStorageContext;
pub use context_tx::PrefixedRocksDbTransactionContext;
pub use raw_iterator::PrefixedRocksDbRawIterator;

/// Make prefixed key
pub fn make_prefixed_key<K: AsRef<[u8]>>(mut prefix: Vec<u8>, key: K) -> Vec<u8> {
    prefix.extend_from_slice(key.as_ref());
    prefix
}
