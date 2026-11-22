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

use std::{
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
};

use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use super::engine::{SuiteBatch, SuiteSubtree, SuiteTransaction, TempEngine, TestEngine};
use crate::{tree::kv::ValueDefinedCostType, Merk, TreeType};

/// A value on the heap that is lent out for `'static` and is freed when
/// this is dropped, or handed back by [`Self::take`].
///
/// What [`TempMerk`] guarantees with it is an order: the Merk it holds is
/// dropped before the batch and the transaction that Merk borrows, and
/// those before the engine. It does not bound what a caller takes out of
/// the Merk. `TempMerk` hands its Merk out as one over a `'static`
/// handle, as it did before it had this guard. A caller that moves that
/// Merk out of the fixture, or keeps a cursor of its handle, holds what
/// the fixture frees: the batch and the transaction at the next
/// [`TempMerk::commit`] and when the fixture is dropped, and the engine
/// when the fixture is dropped.
struct Lent<T: 'static>(*mut T);

impl<T: 'static> Lent<T> {
    fn new(value: T) -> Self {
        Lent(Box::into_raw(Box::new(value)))
    }

    /// The value, for as long as the caller says.
    ///
    /// # Safety
    ///
    /// Nothing reads through the reference, or through anything that
    /// borrows from it, once this `Lent` is dropped or taken.
    unsafe fn lend(&self) -> &'static T {
        // SAFETY: the pointer is the one `Box::into_raw` gave, and the
        // box is freed by `drop` or `take` alone, after which the caller
        // reads nothing through the reference.
        unsafe { &*self.0 }
    }

    /// The value itself. What was lent is not read again.
    fn take(self) -> T {
        let lent = ManuallyDrop::new(self);
        // SAFETY: the pointer is the one `Box::into_raw` gave, and
        // `ManuallyDrop` keeps `drop` from freeing it a second time.
        *unsafe { Box::from_raw(lent.0) }
    }
}

impl<T: 'static> Drop for Lent<T> {
    fn drop(&mut self) {
        // SAFETY: the pointer is the one `Box::into_raw` gave, and it is
        // freed here or in `take`, never in both.
        drop(unsafe { Box::from_raw(self.0) });
    }
}

/// A Merk with the batch its handle stages in and the transaction it is
/// opened in. The fields are dropped in their order: the Merk, which
/// borrows the other two, goes first.
struct Open {
    merk: Merk<SuiteSubtree<'static>>,
    batch: Lent<SuiteBatch>,
    tx: Lent<SuiteTransaction<'static>>,
}

impl Open {
    /// A Merk of the root subtree of `engine`, in a new transaction and
    /// with a new batch. A panic on the way frees both.
    fn new(engine: &'static TempEngine, tree_type: TreeType, grove_version: &GroveVersion) -> Self {
        let batch = Lent::new(SuiteBatch::default());
        let tx = Lent::new(engine.start_transaction());
        // SAFETY: the Merk that borrows the two is kept beside them in
        // `Open`, which drops the Merk before either. That nothing else
        // borrows from them once they are taken or dropped is left to the
        // callers of `TempMerk`, as `Lent` says.
        let (lent_batch, lent_tx) = unsafe { (batch.lend(), tx.lend()) };
        let context = engine
            .subtree(SubtreePath::empty(), Some(lent_batch), lent_tx)
            .unwrap();
        let merk = Merk::open_base(
            context,
            tree_type,
            None::<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
            grove_version,
        )
        .unwrap()
        .unwrap();
        Open { merk, batch, tx }
    }
}

/// Wraps a Merk instance and deletes it from disk it once it goes out of scope.
///
/// The Merk is on the engine the suite runs on
/// ([`SuiteEngine`](super::SuiteEngine)).
pub struct TempMerk {
    /// The Merk, but while a commit hands its batch and its transaction
    /// to the engine, and for good after a commit that panicked there.
    /// It is declared, and so dropped, before the engine.
    open: Option<Open>,
    engine: Lent<TempEngine>,
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
        let engine = Lent::new(TempEngine::new());
        // SAFETY: the Merk, the batch and the transaction that borrow the
        // engine are in `open`, which is dropped before the engine is.
        let open = Open::new(unsafe { engine.lend() }, tree_type, grove_version);
        TempMerk {
            open: Some(open),
            engine,
            tree_type,
        }
    }

    /// Commits pending batch operations.
    ///
    /// A batch or a transaction that the engine refuses is a panic. The
    /// `TempMerk` then holds no Merk: a dereference of it and another
    /// commit panic.
    pub fn commit(&mut self, grove_version: &GroveVersion) {
        let Open { merk, batch, tx } = self.open.take().expect(NO_MERK);
        drop(merk);
        // SAFETY: the transaction that borrows the engine is a local of
        // this function from here on, and what `Open::new` makes below is
        // kept in `open`: both are dropped before the engine is.
        let engine = unsafe { self.engine.lend() };
        let tx = tx.take();
        engine
            .apply_batch(batch.take(), &tx)
            .unwrap()
            .expect("unable to commit batch");
        engine
            .commit_transaction(tx)
            .unwrap()
            .expect("unable to commit transaction");
        self.open = Some(Open::new(engine, self.tree_type, grove_version));
    }
}

const NO_MERK: &str = "an earlier commit of this TempMerk failed";

impl Drop for TempMerk {
    fn drop(&mut self) {
        let Some(Open { merk, batch, tx }) = self.open.take() else {
            return;
        };
        drop(merk);
        // SAFETY: the transaction that borrows the engine is a local of
        // this function, and the engine is dropped after the function
        // returns or unwinds, as a field of `self`.
        let engine = unsafe { self.engine.lend() };
        let tx = tx.take();
        let _ = engine.apply_batch(batch.take(), &tx);
        let _ = engine.commit_transaction(tx).unwrap();
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
        &self.open.as_ref().expect(NO_MERK).merk
    }
}

impl DerefMut for TempMerk {
    fn deref_mut(&mut self) -> &mut Merk<SuiteSubtree<'static>> {
        &mut self.open.as_mut().expect(NO_MERK).merk
    }
}
