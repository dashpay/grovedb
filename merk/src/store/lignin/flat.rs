//! The flat records of a subtree on LigninDB (`flat-store.md`): its data
//! keyspace where the tree keeps no Merk there, and its aux and meta
//! keyspaces.
//!
//! A flat record is a record of its subtree's column in the transaction
//! (`transactions.md` ST1). A handle reads one from its view and writes
//! one as it writes a node: staged in the batch of a deferred handle, and
//! made at once on an immediate one, at the charges of a storage context
//! (`cost-model.md` R1, R2, W1, M1, C1 to C3).
//!
//! The cursor of a writer's handle ([`LigninWriter`](super::LigninWriter))
//! keeps no cursor of the transaction: a cursor of a scope borrows the
//! scope, which the handles of a writer share behind a lock, and the
//! transaction offers no parked cursor over flat records as it does over
//! nodes. It keeps the key it stands on with its value, and each move
//! takes the lock, seeks to that key in a cursor of the scope and steps
//! from there. So it reads the committed state of each move, as the point
//! reads of a scope do, and not that of the moment it was opened (RD6):
//! a commit outside the transaction that lands between two moves shows in
//! the walk. The writes of its own scope it sees as any cursor of the
//! scope does. GroveDB opens no write scope before GROVE-12, which is
//! where a cursor of a scope that keeps its state has to come from the
//! transaction.

use std::cell::{OnceCell, RefCell};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostResult, CostsExt,
    OperationCost,
};
use grovedb_storage::{Error as StorageError, FlatStore, RawIterator};
use lignindb::txn::{Cursor, StandIns, Subtree, Unit, WriteTxn};

use super::{
    access::{column, fault, Failure, Prefix, Seek},
    left::Left,
    place::{Marked, Place},
    LigninChangeSet, LigninNodeStore, LigninWriter, Mode, MODEL,
};
use crate::{
    cost_model::Keyspace,
    store::{NodeChangeSet, NodeStore},
    Error,
};

/// A cursor over the flat records of one column of one subtree, in key
/// order, as the store uses it.
pub(super) trait FlatWalk {
    /// Moves the cursor. `key` is emptied and takes the key the cursor then
    /// stands on, and the answer is whether it stands on one.
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error>;

    /// How many records the cursor's column holds in the subtrees after
    /// the cursor's own in the order of their prefixes, or with `back` in
    /// those before it, as a cursor of each subtree shows them: counted
    /// from the nearest subtree on and up to `limit`. In the data column
    /// the nodes of a Merk count as the flat records do. A step from no
    /// key asks this, and no other move ([`Place`](super::place::Place)).
    /// With `stand_ins`, as
    /// [`Cursor::beside_with`](lignindb::txn::Cursor::beside_with) counts.
    fn beside(
        &mut self,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, Error>;

    /// Appends the value under the key the cursor stands on to `out`, and
    /// says whether there is one.
    fn value(&mut self, out: &mut Vec<u8>) -> Result<bool, Error>;
}

/// The flat records of a view.
pub(super) trait FlatAccess {
    /// A cursor over the flat records of `keyspace` of `subtree`, which
    /// stands on no record yet.
    fn records<'a>(
        &'a self,
        keyspace: Keyspace,
        subtree: &Prefix,
    ) -> Result<Box<dyn FlatWalk + 'a>, Error>;
}

/// A cursor of a write transaction, outside any scope (RD6).
struct TxnRecords<'t, 's>(Cursor<'t, 's>);

impl FlatWalk for TxnRecords<'_, '_> {
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
        key.clear();
        let cursor = &mut self.0;
        match to {
            Seek::First => cursor.seek_to_first(),
            Seek::Last => cursor.seek_to_last(),
            Seek::From(key) => cursor.seek(key),
            Seek::Back(key) => cursor.seek_for_prev(key),
            Seek::Next => cursor.next(),
            Seek::Prev => cursor.prev(),
        }
        .map_err(fault)?;
        key.extend_from_slice(cursor.key().unwrap_or_default());
        Ok(cursor.valid())
    }

    fn beside(
        &mut self,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, Error> {
        (self.0.beside_with(back, limit, stand_ins)).map_err(fault)
    }

    fn value(&mut self, out: &mut Vec<u8>) -> Result<bool, Error> {
        self.0.value(out).map_err(fault)
    }
}

impl FlatAccess for WriteTxn<'_> {
    fn records<'a>(
        &'a self,
        keyspace: Keyspace,
        subtree: &Prefix,
    ) -> Result<Box<dyn FlatWalk + 'a>, Error> {
        let unit = Unit::new(column(keyspace), Subtree::new(*subtree));
        Ok(Box::new(TxnRecords(self.cursor(unit))))
    }
}

/// A cursor over the flat records of a writer's scope: the key it stands
/// on, from which each move starts again in a cursor of the scope.
struct WriterRecords<'a, 't, 's> {
    writer: &'a LigninWriter<'t, 's>,
    unit: Unit,
    /// The key the cursor stands on with the value under it, both as the
    /// move that came to the key read them.
    at: Option<(Vec<u8>, Vec<u8>)>,
}

impl FlatWalk for WriterRecords<'_, '_, '_> {
    fn go(&mut self, to: Seek<'_>, key: &mut Vec<u8>) -> Result<bool, Error> {
        key.clear();
        let at = self.at.take();
        let at = at.as_ref().map(|(key, _)| key.as_slice());
        let scope = self.writer.scope();
        let mut cursor = scope.cursor(self.unit);
        match (to, at) {
            (Seek::First, _) => cursor.seek_to_first(),
            (Seek::Last, _) => cursor.seek_to_last(),
            (Seek::From(key), _) => cursor.seek(key),
            (Seek::Back(key), _) => cursor.seek_for_prev(key),
            // The key the cursor stood on may be gone: the seek is then
            // on the key the step goes to.
            (Seek::Next, Some(at)) => {
                cursor
                    .seek(at)
                    .and_then(|()| match cursor.key() == Some(at) {
                        true => cursor.next(),
                        false => Ok(()),
                    })
            }
            (Seek::Prev, Some(at)) => {
                (cursor.seek_for_prev(at)).and_then(|()| match cursor.key() == Some(at) {
                    true => cursor.prev(),
                    false => Ok(()),
                })
            }
            (Seek::Next | Seek::Prev, None) => Ok(()),
        }
        .map_err(fault)?;
        key.extend_from_slice(cursor.key().unwrap_or_default());
        if cursor.valid() {
            let mut value = Vec::new();
            cursor.value(&mut value).map_err(fault)?;
            self.at = Some((key.clone(), value));
        }
        Ok(self.at.is_some())
    }

    /// Asked of a cursor of the scope that is opened for the question, as
    /// one is for each move: at the committed state of this moment, with
    /// the writes the scope sees.
    fn beside(
        &mut self,
        back: bool,
        limit: usize,
        stand_ins: Option<StandIns<'_>>,
    ) -> Result<usize, Error> {
        let scope = self.writer.scope();
        let mut cursor = scope.cursor(self.unit);
        (cursor.beside_with(back, limit, stand_ins)).map_err(fault)
    }

    fn value(&mut self, out: &mut Vec<u8>) -> Result<bool, Error> {
        if let Some((_, value)) = &self.at {
            out.extend_from_slice(value);
        }
        Ok(self.at.is_some())
    }
}

impl FlatAccess for LigninWriter<'_, '_> {
    fn records<'a>(
        &'a self,
        keyspace: Keyspace,
        subtree: &Prefix,
    ) -> Result<Box<dyn FlatWalk + 'a>, Error> {
        Ok(Box::new(WriterRecords {
            writer: self,
            unit: Unit::new(column(keyspace), Subtree::new(*subtree)),
            at: None,
        }))
    }
}

/// The cursor of a flat keyspace of a subtree on LigninDB: its records in
/// key order, as the handle's reads show them (`flat-store.md`).
///
/// It stands on a key, and reads the value under it when
/// [`RawIterator::value`] first asks. It is charged as a cursor of a
/// storage context (`cost-model.md` C1 to C3). A move that the view
/// refuses leaves it on no key, and [`RawIterator::status`] gives the
/// error until the cursor is sought again.
///
/// A move that leaves the subtree's keys leaves the cursor where a cursor
/// of RocksDB stands then, and a step goes on from there
/// ([`Place`], `flat-store.md` rule 6).
pub struct LigninFlatCursor<'db> {
    walk: Option<RefCell<Box<dyn FlatWalk + 'db>>>,
    place: Place,
    /// What the cursor knows of the marks of flat drops, if its handle
    /// was told where they are kept.
    left: Option<Left>,
    /// The value under the key, once it was asked for.
    value: OnceCell<Option<Vec<u8>>>,
    failure: RefCell<Option<Failure>>,
}

impl std::fmt::Debug for LigninFlatCursor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LigninFlatCursor")
            .field("key", &self.place.key())
            .field("failure", &self.failure.borrow())
            .finish_non_exhaustive()
    }
}

impl<'db> LigninFlatCursor<'db> {
    fn open(walk: Result<Box<dyn FlatWalk + 'db>, Error>, left: Option<Left>) -> Self {
        let (walk, failure) = match walk {
            Ok(walk) => (Some(RefCell::new(walk)), None),
            Err(error) => (None, Some(error.into())),
        };
        Self {
            walk,
            place: Place::default(),
            left,
            value: OnceCell::new(),
            failure: RefCell::new(failure),
        }
    }

    fn go(&mut self, to: Seek<'_>) -> CostContext<()> {
        let seeks = !matches!(to, Seek::Next | Seek::Prev);
        self.value = OnceCell::new();
        if let Some(walk) = self.walk.as_mut() {
            let mut line = Marked {
                walk: &mut **walk.get_mut(),
                left: self.left,
            };
            match self.place.go(&mut line, to) {
                Ok(()) if seeks => *self.failure.get_mut() = None,
                Ok(()) => {}
                Err(error) => *self.failure.get_mut() = Some(error.into()),
            }
        }
        ().wrap_with_cost(MODEL.cursor_move())
    }

    fn stored_key_len(&self) -> Option<usize> {
        let key = self.place.key();
        key.map(|key| MODEL.stored_key_len(key.len()))
    }

    fn read(&self) -> Option<Vec<u8>> {
        let mut walk = self.walk.as_ref()?.borrow_mut();
        let mut value = Vec::new();
        match walk.value(&mut value) {
            Ok(found) => found.then_some(value),
            Err(error) => {
                *self.failure.borrow_mut() = Some(error.into());
                None
            }
        }
    }
}

impl RawIterator for LigninFlatCursor<'_> {
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
        match self.value.get_or_init(|| self.read()) {
            Some(value) => Some(value.as_slice())
                .wrap_with_cost(MODEL.cursor_value(Some((key_len, value.len())))),
            None => None.wrap_with_cost(MODEL.cursor_key(Some(key_len))),
        }
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        let key = self.place.key();
        key.wrap_with_cost(MODEL.cursor_key(self.stored_key_len()))
    }

    fn valid(&self) -> CostContext<bool> {
        let valid = self.place.key().is_some();
        valid.wrap_with_cost(MODEL.cursor_key(self.stored_key_len()))
    }

    fn status(&self) -> Result<(), StorageError> {
        match &*self.failure.borrow() {
            None => Ok(()),
            Some(failure) => Err(failure.error()),
        }
    }
}

/// One flat keyspace of the subtree of a handle: its data, its aux or its
/// meta keyspace, in the transaction and the write mode of the handle.
#[derive(Debug, Clone, Copy)]
pub struct LigninKeyspace<'a, 'db> {
    store: &'a LigninNodeStore<'db>,
    keyspace: Keyspace,
}

impl<'db> LigninNodeStore<'db> {
    fn keyspace(&self, keyspace: Keyspace) -> LigninKeyspace<'_, 'db> {
        LigninKeyspace {
            store: self,
            keyspace,
        }
    }

    /// The aux keyspace of the handle's subtree.
    pub fn aux(&self) -> LigninKeyspace<'_, 'db> {
        self.keyspace(Keyspace::Aux)
    }

    /// The meta keyspace of the handle's subtree.
    pub fn meta(&self) -> LigninKeyspace<'_, 'db> {
        self.keyspace(Keyspace::Meta)
    }

    fn data(&self) -> LigninKeyspace<'_, 'db> {
        self.keyspace(Keyspace::Data)
    }
}

pub(super) fn storage_error(error: Error) -> StorageError {
    match error {
        Error::StorageError(error) => error,
        error => StorageError::StorageError(error.to_string()),
    }
}

impl<'db> LigninKeyspace<'_, 'db> {
    fn write(
        &self,
        key: &[u8],
        value: Option<&[u8]>,
        cost: Option<KeyValueStorageCost>,
    ) -> CostResult<(), StorageError> {
        let subtree = self.store.subtree.clone();
        let change_set = LigninChangeSet::of_record(subtree, self.keyspace, key, value, cost);
        (self.store.submit(change_set, "a write", false)).map_err(storage_error)
    }
}

impl<'db> FlatStore for LigninKeyspace<'_, 'db> {
    type Cursor<'c>
        = LigninFlatCursor<'db>
    where
        Self: 'c;

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, StorageError> {
        (self.store.read_record(self.keyspace, key.as_ref())).map_err(storage_error)
    }

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), StorageError> {
        let key = key.as_ref();
        // A put into the data keyspace leaves the stored key out of the
        // cost of a new record, as a put of a node does.
        let cost_info = match self.keyspace {
            Keyspace::Data => {
                MODEL.complete_new_key_cost(cost_info, MODEL.stored_key_len(key.len()))
            }
            Keyspace::Aux | Keyspace::Roots | Keyspace::Meta => cost_info,
        };
        self.write(key, Some(value), cost_info)
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), StorageError> {
        self.write(key.as_ref(), None, cost_info)
    }

    fn cursor(&self) -> Self::Cursor<'_> {
        let store = self.store;
        let own = &store.subtree.prefix;
        let left = Left::of(store.left_behind, self.keyspace, own);
        LigninFlatCursor::open(store.access.records(self.keyspace, own), left)
    }

    /// Removes every record of the keyspace: the walk of the trait, at its
    /// cost. A walk that the view refuses or that fails on its way has not
    /// come to every record, and the clear fails with its error then.
    fn clear(&self) -> CostResult<(), StorageError> {
        let mut cost = OperationCost::default();
        let mut cursor = self.cursor();
        cursor.seek_to_first().unwrap_add_cost(&mut cost);
        while cursor.valid().unwrap_add_cost(&mut cost) {
            if let Some(key) = cursor.key().unwrap_add_cost(&mut cost) {
                cost_return_on_error!(&mut cost, self.delete(key, None));
            }
            cursor.next().unwrap_add_cost(&mut cost);
        }
        cursor.status().wrap_with_cost(cost)
    }
}

/// A handle is the flat keyspace of its subtree's data, for a tree that
/// keeps no Merk there.
impl<'db> FlatStore for LigninNodeStore<'db> {
    type Cursor<'c>
        = LigninFlatCursor<'db>
    where
        Self: 'c;

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, StorageError> {
        self.data().get(key)
    }

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), StorageError> {
        self.data().put(key, value, cost_info)
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), StorageError> {
        self.data().delete(key, cost_info)
    }

    fn cursor(&self) -> Self::Cursor<'_> {
        let own = &self.subtree.prefix;
        let left = Left::of(self.left_behind, Keyspace::Data, own);
        LigninFlatCursor::open(self.access.records(Keyspace::Data, own), left)
    }

    /// Removes every record of the subtree's data, in one of two ways:
    /// as the nodes of its Merk, where it keeps one that has a node, and
    /// as flat records otherwise.
    ///
    /// - **Flat records** go by the walk of their keyspace
    ///   ([`LigninKeyspace`]), at its cost.
    /// - **The nodes of a Merk** go as the records they are on RocksDB:
    ///   the walk of the node cursor, which shows each node as the record
    ///   Merk encodes of it, at a cursor's cost, and for each key a
    ///   removal that states no cost, staged in the handle's batch as one
    ///   change set that leaves the Merk with no root. The cursor shows
    ///   the unlinked records of the subtree among the nodes, so the key
    ///   of each of them is removed with them. The batch charges
    ///   each removal when it is settled, by rule D2 of `cost-model.md`.
    ///   Applied, the change set empties the Merk.
    ///   Where the element that hosts the subtree leaves in the same group
    ///   of batches, the subtree is dropped with it and the change set is
    ///   not applied ([`LigninSettled::leave_out`](super::LigninSettled::leave_out)).
    ///   A handle that does not stage refuses it with
    ///   [`StorageError::NotSupported`].
    ///
    /// Which of the two ways it goes is read from the subtree's entry
    /// before the walk. A walk that the view refuses or that fails on its way has
    /// not come to every record: the clear fails with its error then and
    /// stages nothing of a Merk. After the walk of flat records the entry
    /// is read again, and a Merk that has a node by then, from a commit
    /// outside the transaction or a write scope that merged into it, is
    /// the same refusal: the caller gives up the batch.
    fn clear(&self) -> CostResult<(), StorageError> {
        let has_nodes = || match self.entry() {
            Ok(entry) => Ok(entry.is_some_and(|entry| entry.root_key.is_some())),
            Err(error) => Err(storage_error(error)),
        };
        let mut cost = OperationCost::default();
        match has_nodes() {
            Ok(false) => {}
            Ok(true) => return self.clear_nodes(),
            Err(error) => return Err(error).wrap_with_cost(cost),
        }
        cost_return_on_error!(&mut cost, self.data().clear());
        match has_nodes() {
            Ok(false) => Ok(()),
            Ok(true) => Err(StorageError::NotSupported(
                "lignindb: the removal of the flat records of a subtree that got a Merk meanwhile"
                    .to_owned(),
            )),
            Err(error) => Err(error),
        }
        .wrap_with_cost(cost)
    }
}

impl LigninNodeStore<'_> {
    /// Removes the nodes of the subtree's Merk as the records of its data
    /// keyspace ([`FlatStore::clear`]).
    fn clear_nodes(&self) -> CostResult<(), StorageError> {
        let mut cost = OperationCost::default();
        if !matches!(self.mode, Mode::Deferred(Some(_))) {
            return Err(StorageError::NotSupported(
                "lignindb: the removal of the nodes of a Merk as the records of a flat keyspace, \
                 through a handle that does not stage its writes"
                    .to_owned(),
            ))
            .wrap_with_cost(cost);
        }
        let mut cursor = NodeStore::cursor(self);
        let mut removed = NodeStore::change_set(self);
        cursor.seek_to_first().unwrap_add_cost(&mut cost);
        while cursor.valid().unwrap_add_cost(&mut cost) {
            if let Some(key) = cursor.key().unwrap_add_cost(&mut cost) {
                removed.delete_node(key, None);
            }
            cursor.next().unwrap_add_cost(&mut cost);
        }
        cost_return_on_error_no_add!(cost, cursor.status());
        removed.state_root(None);
        NodeStore::commit(self, removed)
            .map_err(storage_error)
            .add_cost(cost)
    }
}
