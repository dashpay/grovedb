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

//! Storage Errors File

/// Storage and underlying errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Storage Error
    #[error("storage error: {0}")]
    StorageError(String),
    /// Cost Error
    #[error("cost error: {0}")]
    CostError(grovedb_costs::error::Error),
    /// A write or commit was attempted through a snapshot read
    /// transaction. Snapshot read transactions
    /// (`start_snapshot_read_transaction`) exist to pin multi-operation
    /// READS to one committed state; writing through one is refused
    /// because `set_snapshot` arms commit-time conflict detection, which
    /// can fail such a commit with `Busy` where a plain transaction's
    /// would have succeeded. The payload names the refused operation.
    #[error("snapshot read transaction is read-only: refused {0}")]
    SnapshotReadOnlyTransaction(&'static str),
    /// Rocks DB error
    #[error("rocksDB error: {0}")]
    #[cfg(feature = "rocksdb_storage")]
    RocksDBError(#[from] rocksdb::Error),
}
