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

//! Useful utilities for testing.

use std::{cell::RefCell, ops::Deref};

use tempfile::TempDir;

use super::*;

/// RocksDb storage with self-cleanup
pub struct TempStorage {
    dir: RefCell<TempDir>,
    storage: RocksDbStorage,
}

impl TempStorage {
    /// Create new `TempStorage`
    pub fn new() -> Self {
        let dir = TempDir::new().expect("cannot create tempir");
        let storage = RocksDbStorage::default_primary_rocksdb(dir.path())
            .expect("cannot open rocksdb storage");
        TempStorage {
            dir: RefCell::new(dir),
            storage,
        }
    }

    /// Create secondary storage
    pub fn secondary(&self) -> RocksDbStorage {
        let dir = TempDir::new().expect("cannot create tempir");

        let primary_dir = self.dir.borrow();

        RocksDbStorage::default_secondary_rocksdb(primary_dir.path(), dir.path())
            .expect("cannot open rocksdb storage")
    }

    /// Simulate storage crash
    pub fn crash(&self) {
        drop(
            self.dir
                .replace(TempDir::new().expect("cannot create tempdir")),
        )
    }
}

impl Default for TempStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for TempStorage {
    type Target = RocksDbStorage;

    fn deref(&self) -> &Self::Target {
        &self.storage
    }
}
