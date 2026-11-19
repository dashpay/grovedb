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

//! Temp merk test utils

use std::ops::{Deref, DerefMut};

use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::engine::{SuiteBatch, SuiteSubtree, SuiteTransaction, TempEngine, TestEngine};
use crate::{tree::kv::ValueDefinedCostType, Merk, TreeType};

/// Wraps a Merk instance and deletes it from disk it once it goes out of scope.
///
/// The Merk is on the engine the suite runs on
/// ([`SuiteEngine`](super::SuiteEngine)).
pub struct TempMerk {
    engine: &'static TempEngine,
    batch: &'static SuiteBatch,
    merk: Merk<SuiteSubtree<'static>>,
    tx: &'static SuiteTransaction<'static>,
    /// The tree type to preserve across commits
    pub tree_type: TreeType,
}

impl TempMerk {
    /// Opens a `TempMerk` at the given file path, creating a new one if it
    /// does not exist.
    pub fn new(grove_version: &GroveVersion) -> Self {
        Self::new_with_tree_type(grove_version, TreeType::NormalTree)
    }

    /// Opens a `TempMerk` with a specific tree type.
    pub fn new_with_tree_type(grove_version: &GroveVersion, tree_type: TreeType) -> Self {
        let engine = Box::leak(Box::new(TempEngine::new()));
        let batch = Box::leak(Box::new(SuiteBatch::default()));
        let tx = Box::leak(Box::new(engine.start_transaction()));

        let context = engine
            .subtree(SubtreePath::empty(), Some(batch), tx)
            .unwrap();

        let merk = Merk::open_base(
            context,
            tree_type,
            None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .unwrap()
        .unwrap();
        TempMerk {
            engine,
            merk,
            batch,
            tx,
            tree_type,
        }
    }

    /// Commits pending batch operations.
    pub fn commit(&mut self, grove_version: &GroveVersion) {
        let batch: Box<SuiteBatch> =
            unsafe { Box::from_raw(self.batch as *const _ as *mut SuiteBatch) };
        let tx: Box<SuiteTransaction<'static>> =
            unsafe { Box::from_raw(self.tx as *const _ as *mut SuiteTransaction<'static>) };
        self.engine
            .apply_batch(*batch, self.tx)
            .unwrap()
            .expect("unable to commit batch");
        self.engine
            .commit_transaction(*tx)
            .unwrap()
            .expect("unable to commit transaction");
        self.batch = Box::leak(Box::new(SuiteBatch::default()));
        self.tx = Box::leak(Box::new(self.engine.start_transaction()));
        let context = self
            .engine
            .subtree(SubtreePath::empty(), Some(self.batch), self.tx)
            .unwrap();
        self.merk = Merk::open_base(
            context,
            self.tree_type,
            None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .unwrap()
        .unwrap();
    }
}

impl Drop for TempMerk {
    fn drop(&mut self) {
        unsafe {
            let batch = Box::from_raw(self.batch as *const _ as *mut SuiteBatch);

            let tx: Box<SuiteTransaction<'static>> =
                Box::from_raw(self.tx as *const _ as *mut SuiteTransaction<'static>);

            let _ = self.engine.apply_batch(*batch, &tx);
            let _ = self.engine.commit_transaction(*tx).unwrap();
            drop(Box::from_raw(self.engine as *const _ as *mut TempEngine));
        }
    }
}

impl Default for TempMerk {
    fn default() -> Self {
        Self::new(GroveVersion::latest())
    }
}

impl Deref for TempMerk {
    type Target = Merk<SuiteSubtree<'static>>;

    fn deref(&self) -> &Merk<SuiteSubtree<'static>> {
        &self.merk
    }
}

impl DerefMut for TempMerk {
    fn deref_mut(&mut self) -> &mut Merk<SuiteSubtree<'static>> {
        &mut self.merk
    }
}
