//! A cursor through a write transaction: the committed records of the
//! moment it was opened, merged with the transaction's own writes.

use std::{cmp::Ordering, fmt, ops::Bound};

use lignin_tree::flat::{self, Committed};

use super::{
    error::TxnError,
    frames::stored_key,
    key::Unit,
    txn::WriteTxn,
    write_set::{Own, WriteSet},
};
use crate::snapshot::VersionPin;

/// The committed side of a cursor of a write transaction
/// (`transactions.md` RD6): an ordered cursor over the records of one unit
/// as one committed version has them.
///
/// [`FlatSide`] is the one for flat records. The one for the nodes of a
/// Merk is the ordered cursor of the tree layer (TREE-06, MERK-05).
pub trait CommittedSide {
    /// Goes to the first key at or after `key`.
    fn seek(&mut self, key: &[u8]) -> Result<(), TxnError>;

    /// Goes to the last key at or before `key`.
    fn seek_for_prev(&mut self, key: &[u8]) -> Result<(), TxnError>;

    /// Goes to the first key.
    fn seek_to_first(&mut self) -> Result<(), TxnError>;

    /// Goes to the last key.
    fn seek_to_last(&mut self) -> Result<(), TxnError>;

    /// Goes to the next key. Past the last one the side has no key.
    fn next(&mut self) -> Result<(), TxnError>;

    /// Goes to the key before. Before the first one the side has no key.
    fn prev(&mut self) -> Result<(), TxnError>;

    /// The key the side stands on, if it stands on one.
    fn key(&self) -> Option<&[u8]>;

    /// Appends the value of the record under `key` to `out`, and says
    /// whether the committed version has one. `key` is mostly the key the
    /// side stands on.
    fn value(&mut self, key: &[u8], out: &mut Vec<u8>) -> Result<bool, TxnError>;
}

/// The committed flat records of one unit at one version.
pub struct FlatSide<'s> {
    pages: Committed<'s>,
    tree: flat::FlatTree,
    cursor: flat::Cursor,
    unit: Unit,
    key: Vec<u8>,
}

impl fmt::Debug for FlatSide<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlatSide")
            .field("unit", &self.unit)
            .field("at", &self.pages.at())
            .finish_non_exhaustive()
    }
}

impl CommittedSide for FlatSide<'_> {
    fn seek(&mut self, key: &[u8]) -> Result<(), TxnError> {
        Ok(self.cursor.seek(&self.pages, key)?)
    }

    fn seek_for_prev(&mut self, key: &[u8]) -> Result<(), TxnError> {
        Ok(self.cursor.seek_for_prev(&self.pages, key)?)
    }

    fn seek_to_first(&mut self) -> Result<(), TxnError> {
        Ok(self.cursor.seek_to_first(&self.pages)?)
    }

    fn seek_to_last(&mut self) -> Result<(), TxnError> {
        Ok(self.cursor.seek_to_last(&self.pages)?)
    }

    fn next(&mut self) -> Result<(), TxnError> {
        Ok(self.cursor.next(&self.pages)?)
    }

    fn prev(&mut self) -> Result<(), TxnError> {
        Ok(self.cursor.prev(&self.pages)?)
    }

    fn key(&self) -> Option<&[u8]> {
        self.cursor.key()
    }

    fn value(&mut self, key: &[u8], out: &mut Vec<u8>) -> Result<bool, TxnError> {
        if self.cursor.key() == Some(key) {
            return Ok(self.cursor.value(&self.pages, out)?);
        }
        stored_key(&mut self.key, &self.unit.subtree, key);
        Ok(self.tree.get(&self.pages, &self.key, out)?)
    }
}

/// Which way a cursor last moved, which says where its committed side
/// stands: at the first committed key at or after the cursor's key, or at
/// the last one at or before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Way {
    Forward,
    Back,
}

/// A cursor through a write transaction over the records of one unit, in
/// key order (`transactions.md` RD6).
///
/// - **The committed side is fixed** at the latest committed version of the
///   moment the cursor was opened, which the cursor keeps pinned (PN1). A
///   commit that is published afterwards is not seen by this cursor, and is
///   seen by the next one.
/// - **The transaction's own writes are live.** A write the transaction
///   makes after the cursor was opened is seen when the cursor reaches its
///   key, and a delete hides the committed record. A rollback takes the
///   writes out again in the same way.
/// - **The cursor stands on a key, not on a record.** After a write to the
///   key it stands on, [`Self::key`] is still that key, [`Self::value`]
///   reads what is under it now, and a step goes on from the key. So the
///   delete of the record the cursor stands on, followed by a step, is
///   defined (`flat-store.md`, rule 7), and so is every other write under
///   an open cursor.
///
/// Each call is one operation of the transaction, and holds its lock for
/// the length of the call.
pub struct Cursor<'t, 's, C = FlatSide<'s>> {
    txn: &'t WriteTxn<'s>,
    scoped: bool,
    unit: Unit,
    pin: VersionPin<'s>,
    side: C,
    key: Option<Vec<u8>>,
    way: Way,
}

impl<'t, 's> Cursor<'t, 's> {
    pub(crate) fn flat(txn: &'t WriteTxn<'s>, scoped: bool, unit: Unit) -> Self {
        let path = txn.path();
        // The guard is held while the version is pinned and its pages are
        // first named (PN5); the pin keeps them from then on.
        let guard = path.versions().guard();
        let pin = path.versions().pin();
        let tree = path.keyspaces().of(unit.column);
        let side = FlatSide {
            pages: path.committed(pin.version()),
            cursor: tree.cursor_within(unit.subtree.as_bytes()),
            tree,
            unit,
            key: Vec::new(),
        };
        drop(guard);
        Self::over(txn, scoped, unit, pin, side)
    }
}

impl<'t, 's, C: CommittedSide> Cursor<'t, 's, C> {
    /// A cursor whose committed side is `side`, which reads the version
    /// `pin` holds.
    pub(crate) fn over(
        txn: &'t WriteTxn<'s>,
        scoped: bool,
        unit: Unit,
        pin: VersionPin<'s>,
        side: C,
    ) -> Self {
        Self {
            txn,
            scoped,
            unit,
            pin,
            side,
            key: None,
            way: Way::Forward,
        }
    }

    /// The committed version the cursor reads beside the transaction's
    /// writes.
    pub fn version(&self) -> lignin_format::CommitVersion {
        self.pin.version()
    }

    /// Whether the cursor stands on a key.
    pub fn valid(&self) -> bool {
        self.key.is_some()
    }

    /// The key the cursor stands on.
    pub fn key(&self) -> Option<&[u8]> {
        self.key.as_deref()
    }

    /// Appends the value under the key the cursor stands on to `out`, and
    /// says whether there is one: none if the cursor stands on no key, or
    /// if the transaction has deleted the record since the cursor came to
    /// it.
    pub fn value(&mut self, out: &mut Vec<u8>) -> Result<bool, TxnError> {
        let Some(key) = &self.key else {
            return Ok(false);
        };
        let unit = self.unit;
        let own = self
            .txn
            .with_set(self.scoped, |set| match set.own(unit, key) {
                Own::Put(value) => {
                    out.extend_from_slice(value);
                    Ok(Some(true))
                }
                Own::Deleted => Ok(Some(false)),
                Own::Unwritten => Ok(None),
                Own::Node(_) => Err(TxnError::Damaged("a Merk node under a cursor of records")),
            })??;
        match own {
            Some(found) => Ok(found),
            None => self.side.value(key, out),
        }
    }

    /// Goes to the first key at or after `key`.
    pub fn seek(&mut self, key: &[u8]) -> Result<(), TxnError> {
        self.side.seek(key)?;
        self.settle(Bound::Included(key.to_vec()), Way::Forward)
    }

    /// Goes to the last key at or before `key`.
    pub fn seek_for_prev(&mut self, key: &[u8]) -> Result<(), TxnError> {
        self.side.seek_for_prev(key)?;
        self.settle(Bound::Included(key.to_vec()), Way::Back)
    }

    /// Goes to the first key.
    pub fn seek_to_first(&mut self) -> Result<(), TxnError> {
        self.side.seek_to_first()?;
        self.settle(Bound::Unbounded, Way::Forward)
    }

    /// Goes to the last key.
    pub fn seek_to_last(&mut self) -> Result<(), TxnError> {
        self.side.seek_to_last()?;
        self.settle(Bound::Unbounded, Way::Back)
    }

    /// Goes to the next key. Past the last key, and from no key, the
    /// cursor stands on none.
    pub fn next(&mut self) -> Result<(), TxnError> {
        let Some(key) = self.key.take() else {
            return Ok(());
        };
        // The committed side goes to the first committed key after `key`.
        if self.way != Way::Forward {
            self.side.seek(&key)?;
        }
        if self.side.key() == Some(&key[..]) {
            self.side.next()?;
        }
        self.settle(Bound::Excluded(key), Way::Forward)
    }

    /// Goes to the key before. Before the first key, and from no key, the
    /// cursor stands on none.
    pub fn prev(&mut self) -> Result<(), TxnError> {
        let Some(key) = self.key.take() else {
            return Ok(());
        };
        if self.way != Way::Back {
            self.side.seek_for_prev(&key)?;
        }
        if self.side.key() == Some(&key[..]) {
            self.side.prev()?;
        }
        self.settle(Bound::Excluded(key), Way::Back)
    }

    /// Finds the first key within `bound`, going `way`, that has a record
    /// in the transaction's view, and stands on it. The committed side
    /// stands at its first key within the bound.
    fn settle(&mut self, mut bound: Bound<Vec<u8>>, way: Way) -> Result<(), TxnError> {
        self.way = way;
        self.key = None;
        let back = way == Way::Back;
        let unit = self.unit;
        loop {
            let committed = self.side.key();
            // The next key the transaction wrote, and whether it is a
            // record now.
            let own = self.txn.with_set(self.scoped, |set| {
                let within = match (&bound, back) {
                    (Bound::Unbounded, _) => (Bound::Unbounded, Bound::Unbounded),
                    (Bound::Included(key), false) => (Bound::Included(&key[..]), Bound::Unbounded),
                    (Bound::Excluded(key), false) => (Bound::Excluded(&key[..]), Bound::Unbounded),
                    (Bound::Included(key), true) => (Bound::Unbounded, Bound::Included(&key[..])),
                    (Bound::Excluded(key), true) => (Bound::Unbounded, Bound::Excluded(&key[..])),
                };
                let written = set.written_key(unit, within, back);
                let first = first_of(written, committed, back);
                first.map(|(key, written)| (key.to_vec(), visible(set, unit, key, written)))
            })?;
            let Some((key, shown)) = own else {
                return Ok(());
            };
            if shown {
                self.key = Some(key);
                return Ok(());
            }
            // The key has no record in the transaction's view: it goes on
            // behind it.
            if self.side.key() == Some(&key[..]) {
                match way {
                    Way::Forward => self.side.next()?,
                    Way::Back => self.side.prev()?,
                }
            }
            bound = Bound::Excluded(key);
        }
    }
}

/// The key that comes first, going back or forward, of the next key the
/// transaction wrote and the next committed key, with whether the
/// transaction wrote it.
fn first_of<'a>(
    written: Option<&'a [u8]>,
    committed: Option<&'a [u8]>,
    back: bool,
) -> Option<(&'a [u8], bool)> {
    match (written, committed) {
        (None, None) => None,
        (Some(key), None) => Some((key, true)),
        (None, Some(key)) => Some((key, false)),
        (Some(written), Some(committed)) => {
            let order = match back {
                false => written.cmp(committed),
                true => committed.cmp(written),
            };
            match order {
                Ordering::Less | Ordering::Equal => Some((written, true)),
                Ordering::Greater => Some((committed, false)),
            }
        }
    }
}

/// Whether `key` has a record in the transaction's view: its own put, or a
/// committed record that none of its deletes covers (RD3).
fn visible(set: &WriteSet, unit: Unit, key: &[u8], written: bool) -> bool {
    match written {
        true => matches!(set.own(unit, key), Own::Put(_) | Own::Node(_)),
        false => !set.hides(unit, key),
    }
}

impl<C> fmt::Debug for Cursor<'_, '_, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cursor")
            .field("unit", &self.unit)
            .field("version", &self.pin.version())
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}
