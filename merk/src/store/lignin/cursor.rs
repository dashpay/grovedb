//! The cursor of a Merk on LigninDB.

use std::cell::{OnceCell, RefCell};

use ed::Encode;
use grovedb_costs::{CostContext, CostsExt};
use grovedb_storage::Error as StorageError;

use super::{
    access::{Access, Prefix, Seek, StoredNode, Walk},
    hash, node_inner, MODEL,
};
use crate::{store::NodeCursor, Error};

/// The cursor of a [`LigninNodeStore`](super::LigninNodeStore): the nodes
/// of one Merk in key order, each as the record Merk encodes of it
/// (`node-store.md`, "Cursor").
///
/// - **What it shows** is what the handle's reads show: the committed
///   nodes of the moment the cursor was opened, which it keeps pinned,
///   with the transaction's own writes as they are at each call
///   (`transactions.md` RD6). It shows nothing that is staged in a batch.
/// - **It stands on a key.** A move takes the key it comes to, and the
///   record under it is built when [`NodeCursor::value`] first asks for
///   it: from the node's cell, and from the cell of each child for the
///   child's key, which a cell does not hold (`tree-page.md` TP10). A walk
///   that reads keys only, as [`Merk::clear`](crate::Merk::clear) does,
///   reads no value and no child. The record is read once for a place: a
///   write to the key after `value` was asked shows after the next move.
/// - **It holds no lock and no page between two calls.** Each move, and
///   the read of a record, is one operation of the transaction. On a
///   writer's handle it takes the writer's lock for that long.
/// - **It is charged as a cursor of a storage context** (`cost-model.md`
///   C1 to C3), with the sizes Merk's record has.
/// - **A move the transaction refuses leaves it on no key.** A read of a
///   record that fails leaves the cursor on its key, with no value, and so
///   does a node with a hash that is not made yet, which has no record.
///   Either way [`NodeCursor::status`] gives the error until the cursor is
///   sought again. A walk that ends is at the end of the nodes only if
///   `status` says so.
pub struct LigninCursor<'db> {
    walk: Option<RefCell<Box<dyn Walk + 'db>>>,
    /// The key the cursor stands on, where it stands on one.
    key: Vec<u8>,
    on_key: bool,
    /// The record under the key, once it was asked for.
    record: OnceCell<Option<Vec<u8>>>,
    failure: RefCell<Option<String>>,
}

impl std::fmt::Debug for LigninCursor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LigninCursor")
            .field("key", &self.on_key.then_some(&self.key))
            .field("failure", &self.failure.borrow())
            .finish_non_exhaustive()
    }
}

/// The record Merk encodes of `node`. A node with a stale hash has none:
/// the bytes its cell stores there are no hash, and a record would give
/// them out as one (`node-store.md`, "Hashes").
fn record(node: StoredNode) -> Result<Vec<u8>, Error> {
    hash::refuse_stale(node.stale)?;
    // A record does not hold the key of its own node.
    let inner = node_inner(Vec::new(), node)?;
    Encode::encode(&inner).map_err(Error::EdError)
}

impl<'db> LigninCursor<'db> {
    pub(super) fn open(access: &'db (dyn Access + 'db), subtree: &Prefix) -> Self {
        let (walk, failure) = match access.cursor(subtree) {
            Ok(walk) => (Some(RefCell::new(walk)), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            walk,
            key: Vec::new(),
            on_key: false,
            record: OnceCell::new(),
            failure: RefCell::new(failure),
        }
    }

    /// Moves the cursor. A seek starts again after a failure, and a step
    /// from no key goes nowhere.
    fn go(&mut self, to: Seek<'_>) -> CostContext<()> {
        let seeks = !matches!(to, Seek::Next | Seek::Prev);
        self.record = OnceCell::new();
        if let Some(walk) = self.walk.as_mut().filter(|_| seeks || self.on_key) {
            match walk.get_mut().go(to, &mut self.key) {
                Ok(on_key) => {
                    self.on_key = on_key;
                    if seeks {
                        *self.failure.get_mut() = None;
                    }
                }
                Err(error) => {
                    self.on_key = false;
                    *self.failure.get_mut() = Some(error.to_string());
                }
            }
        }
        ().wrap_with_cost(MODEL.cursor_move())
    }

    /// The length of the stored key the cursor stands on.
    fn stored_key_len(&self) -> Option<usize> {
        (self.on_key).then(|| MODEL.stored_key_len(self.key.len()))
    }

    /// Builds the record under the cursor's key.
    fn read(&self) -> Option<Vec<u8>> {
        let mut walk = self.walk.as_ref()?.borrow_mut();
        let read = walk.node().and_then(|node| node.map(record).transpose());
        read.unwrap_or_else(|error| {
            *self.failure.borrow_mut() = Some(error.to_string());
            None
        })
    }
}

impl NodeCursor for LigninCursor<'_> {
    fn seek_to_first(&mut self) -> CostContext<()> {
        self.go(Seek::First)
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        self.go(Seek::Last)
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.go(Seek::From(key.as_ref()))
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.go(Seek::Back(key.as_ref()))
    }

    fn next(&mut self) -> CostContext<()> {
        self.go(Seek::Next)
    }

    fn prev(&mut self) -> CostContext<()> {
        self.go(Seek::Prev)
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        let Some(key_len) = self.stored_key_len() else {
            return None.wrap_with_cost(MODEL.cursor_key(None));
        };
        match self.record.get_or_init(|| self.read()) {
            Some(record) => Some(record.as_slice())
                .wrap_with_cost(MODEL.cursor_value(Some((key_len, record.len())))),
            None => None.wrap_with_cost(MODEL.cursor_key(Some(key_len))),
        }
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        let key = self.on_key.then_some(self.key.as_slice());
        key.wrap_with_cost(MODEL.cursor_key(self.stored_key_len()))
    }

    fn valid(&self) -> CostContext<bool> {
        (self.on_key).wrap_with_cost(MODEL.cursor_key(self.stored_key_len()))
    }

    fn status(&self) -> Result<(), StorageError> {
        match &*self.failure.borrow() {
            None => Ok(()),
            Some(failure) => Err(StorageError::StorageError(failure.clone())),
        }
    }
}
