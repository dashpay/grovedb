//! The record of a background change: a change to one page that takes no
//! commit version, with what it does to the counts of the value log.

use std::iter::FusedIterator;

use super::{
    flat::{self, FlatRun},
    group::OrderKey,
    record::{Fields, CHANGE},
    WalError,
};
use crate::{
    delta::{Delta, DeltaRun},
    error::FormatError,
    header::{write_record, FormatVersion},
    ids::{CommitVersion, PageId, SlotId},
    tree_page::{CellValue, TreePage},
    value_log::{ValuePlace, ValuePointer},
};

const TREE_VALUE: u8 = 1;
const FLAT_VALUE: u8 = 2;
const RECLAIM: u8 = 3;

const FREED: u8 = 1;
const TREE_RUN: u8 = 2;
const FLAT_RUN: u8 = 4;

const PLACE_LEN: usize = ValuePlace::ENCODED_LEN;
const FLAT_VALUE_LEN: usize = PageId::ENCODED_LEN + 2 * PLACE_LEN;
const TREE_VALUE_LEN: usize = FLAT_VALUE_LEN + SlotId::ENCODED_LEN;

/// A background change, as the log records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change<'a> {
    /// A value got a place in the value log, or another place there.
    Value(ValueMove),
    /// What a dropped subtree left behind in a page was removed.
    Reclaim(Reclaim<'a>),
}

impl<'a> Change<'a> {
    /// The page the change is made to.
    pub const fn page(&self) -> PageId {
        match self {
            Self::Value(value) => value.page(),
            Self::Reclaim(reclaim) => reclaim.page(),
        }
    }

    /// Appends the change's record to `out`.
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), FormatError> {
        match self {
            Self::Value(value) => value.write(out),
            Self::Reclaim(reclaim) => write_reclaim(
                reclaim.page,
                reclaim.removed,
                reclaim.freed,
                reclaim.released(),
                out,
            ),
        }
    }

    pub(super) fn decode(
        mut fields: Fields<'a>,
        version: FormatVersion,
    ) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => {}
        }
        let what = fields.byte().map_err(|_| WalError::RecordLength {
            kind: CHANGE,
            len: 1,
        })?;
        match what {
            TREE_VALUE => {
                fields.expect_len(CHANGE, TREE_VALUE_LEN)?;
                let page = fields.page()?;
                let slot = SlotId::decode(*fields.array()?).ok_or(FormatError::MissingId)?;
                ValueMove::in_tree(page, slot, fields.place()?, fields.pointer()?).map(Self::Value)
            }
            FLAT_VALUE => {
                fields.expect_len(CHANGE, FLAT_VALUE_LEN)?;
                let page = fields.page()?;
                ValueMove::in_flat_leaf(page, fields.place()?, fields.pointer()?).map(Self::Value)
            }
            RECLAIM => Reclaim::decode(fields, version).map(Self::Reclaim),
            code => Err(FormatError::UnknownRecordKind { code: code.into() }),
        }
    }
}

/// The page step that gives the value of one cell a place in the value
/// log: the placing of a pending value at write-back, or the move of a
/// value to another place.
///
/// It says what the cell held before, so that a replay can tell a page
/// that has the change from one that lacks it: the change is still to be
/// made exactly when the cell holds [`Self::from`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueMove {
    page: PageId,
    slot: Option<SlotId>,
    from: ValuePlace,
    to: ValuePointer,
}

impl ValueMove {
    fn checked(self) -> Result<Self, FormatError> {
        if self.from.value_len() != self.to.value_len() || self.from == ValuePlace::Placed(self.to)
        {
            return Err(WalError::InvalidChange.into());
        }
        Ok(self)
    }

    /// The cell in `slot` of the tree page `page`, which holds the place
    /// `from`, holds `to` from this step on.
    ///
    /// Both name a value of one length, and they are not the same place.
    pub fn in_tree(
        page: PageId,
        slot: SlotId,
        from: ValuePlace,
        to: ValuePointer,
    ) -> Result<Self, FormatError> {
        Self {
            page,
            slot: Some(slot),
            from,
            to,
        }
        .checked()
    }

    /// The entry of the flat leaf `page` whose value is at the place
    /// `from` has it at `to` from this step on.
    pub fn in_flat_leaf(
        page: PageId,
        from: ValuePlace,
        to: ValuePointer,
    ) -> Result<Self, FormatError> {
        Self {
            page,
            slot: None,
            from,
            to,
        }
        .checked()
    }

    /// The page the cell is in.
    pub const fn page(&self) -> PageId {
        self.page
    }

    /// The slot of the cell in a tree page, or `None` for an entry of a
    /// flat leaf, which is found by the place it holds.
    pub const fn slot(&self) -> Option<SlotId> {
        self.slot
    }

    /// What the cell held before the step.
    pub const fn from(&self) -> ValuePlace {
        self.from
    }

    /// What the cell holds from the step on.
    pub const fn to(&self) -> ValuePointer {
        self.to
    }

    /// The pointer that the step takes off its file's count of live
    /// values: the place the value is moved from. A pending value has no
    /// pointer, and placing it releases nothing.
    pub const fn released(&self) -> Option<ValuePointer> {
        self.from.pointer()
    }

    /// The pointer that the step adds to its file's count of live values.
    pub const fn acquired(&self) -> ValuePointer {
        self.to
    }

    /// The change to a tree page, as the page delta that makes it.
    pub const fn delta(&self) -> Option<Delta<'static>> {
        match self.slot {
            Some(slot) => Some(Delta::Value {
                slot,
                value: CellValue::OutOfLine(self.to),
            }),
            None => None,
        }
    }

    /// Whether the change is still to be made to the tree page `page`: it
    /// is the page the step names, and the cell still holds
    /// [`Self::from`].
    pub fn is_due_in_tree(&self, page: &TreePage<'_>) -> bool {
        let cell = self.slot.and_then(|slot| page.cell(slot));
        page.id() == self.page && cell.is_some_and(|cell| cell.value_place() == Some(self.from))
    }

    /// Whether the change is still to be made to the flat leaf whose image
    /// is `image`: it is the page the step names, and an entry of it still
    /// holds [`Self::from`].
    pub fn is_due_in_flat_leaf(
        &self,
        image: &[u8],
        version: FormatVersion,
    ) -> Result<bool, FormatError> {
        Ok(self.slot.is_none()
            && flat::holds_place(image, self.page, version, &self.from.encode())?)
    }

    /// Makes the change to the flat leaf whose image is `image`, which is
    /// the page the step names, and appends the image it gives to `out`. On
    /// an error `out` is unchanged. `capacity` is the longest image the
    /// store's page size allows.
    pub fn apply_to_flat_leaf(
        &self,
        image: &[u8],
        version: FormatVersion,
        capacity: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), FormatError> {
        if self.slot.is_some() {
            return Err(WalError::WrongPageKind.into());
        }
        let (from, to) = (self.from.encode(), self.to.encode());
        flat::repoint(image, self.page, version, capacity, &from, &to, out)
    }

    /// Appends the step's record to `out`.
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), FormatError> {
        let mut payload = Vec::with_capacity(2 + TREE_VALUE_LEN);
        payload.push(CHANGE);
        payload.push(if self.slot.is_some() {
            TREE_VALUE
        } else {
            FLAT_VALUE
        });
        payload.extend_from_slice(&self.page.encode());
        if let Some(slot) = self.slot {
            payload.extend_from_slice(&slot.encode());
        }
        payload.extend_from_slice(&self.from.encode());
        payload.extend_from_slice(&self.to.encode());
        write_record(&payload, out)
    }
}

/// What a reclaiming step took out of its page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removed<'a> {
    /// Cells of a tree page: a run of remove records.
    Tree(DeltaRun<'a>),
    /// Entries of a flat page: a run of removes.
    Flat(FlatRun<'a>),
}

impl Removed<'_> {
    /// The page the cells or entries were in.
    pub const fn page(&self) -> PageId {
        match self {
            Self::Tree(run) => run.page(),
            Self::Flat(run) => run.page(),
        }
    }

    const fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Tree(run) => run.as_bytes(),
            Self::Flat(run) => run.as_bytes(),
        }
    }

    fn check(&self, page: PageId) -> Result<(), FormatError> {
        if self.page() != page {
            return Err(WalError::WrongPage {
                found: self.page(),
                expected: page,
            }
            .into());
        }
        let only_removes = match self {
            Self::Tree(run) => {
                run.created().is_none()
                    && run
                        .deltas()
                        .all(|delta| matches!(delta, Delta::Remove { .. }))
            }
            Self::Flat(run) => flat::only_removes(run),
        };
        if !only_removes {
            return Err(WalError::InvalidChange.into());
        }
        Ok(())
    }
}

/// The page step that removes what a dropped subtree left behind in one
/// page: cells of a tree page or entries of a flat page. It can free the
/// page, and it names the value pointers that the removed cells held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reclaim<'a> {
    page: PageId,
    freed: Option<CommitVersion>,
    released: &'a [[u8; PLACE_LEN]],
    removed: Option<Removed<'a>>,
}

impl<'a> Reclaim<'a> {
    /// The page the step is made to.
    pub const fn page(&self) -> PageId {
        self.page
    }

    /// What the step took out of the page, if it says so. A step that
    /// frees a page whose cells are all left-overs need not list them.
    pub const fn removed(&self) -> Option<Removed<'a>> {
        self.removed
    }

    /// The commit version the page was freed with, if the step freed it.
    pub const fn freed(&self) -> Option<CommitVersion> {
        self.freed
    }

    /// The value pointers that the removed cells held, in ascending order
    /// of file and offset: what the step takes off the counts of live
    /// values.
    pub fn released(&self) -> Released<'a> {
        Released(self.released.iter())
    }

    fn decode(mut fields: Fields<'a>, version: FormatVersion) -> Result<Self, FormatError> {
        let flags = fields.byte()?;
        if flags & !(FREED | TREE_RUN | FLAT_RUN) != 0 {
            return Err(FormatError::NonZeroReserved);
        }
        if flags & TREE_RUN != 0 && flags & FLAT_RUN != 0 {
            return Err(WalError::InvalidChange.into());
        }
        let page = fields.page()?;
        let freed = if flags & FREED != 0 {
            let version = CommitVersion::decode(*fields.array()?).ok_or(FormatError::MissingId)?;
            if version == CommitVersion::EMPTY {
                return Err(WalError::NotACommit.into());
            }
            Some(version)
        } else {
            None
        };
        let count = usize::from(fields.u16()?);
        let (released, rest) = fields.take(count * PLACE_LEN)?.as_chunks();
        debug_assert!(rest.is_empty());
        check_released(
            released
                .iter()
                .map(|pointer| ValuePointer::decode(*pointer)),
        )?;
        let removed = if flags & TREE_RUN != 0 {
            Some(Removed::Tree(DeltaRun::decode(
                fields.take_rest(),
                version,
            )?))
        } else if flags & FLAT_RUN != 0 {
            Some(Removed::Flat(FlatRun::decode(fields.take_rest(), version)?))
        } else {
            fields.expect_len(CHANGE, 0)?;
            None
        };
        match removed {
            Some(removed) => removed.check(page)?,
            None if freed.is_none() => return Err(WalError::InvalidChange.into()),
            None => {}
        }
        Ok(Self {
            page,
            freed,
            released,
            removed,
        })
    }
}

/// The value pointers a reclaiming step released, in order.
#[derive(Debug, Clone)]
pub struct Released<'a>(std::slice::Iter<'a, [u8; PLACE_LEN]>);

impl Iterator for Released<'_> {
    type Item = ValuePointer;

    fn next(&mut self) -> Option<Self::Item> {
        let pointer = ValuePointer::decode(*self.0.next()?).ok();
        if pointer.is_none() {
            self.0 = [].iter();
        }
        pointer
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl ExactSizeIterator for Released<'_> {}

impl FusedIterator for Released<'_> {}

fn check_released(
    pointers: impl Iterator<Item = Result<ValuePointer, FormatError>>,
) -> Result<(), FormatError> {
    let mut before = None;
    for pointer in pointers {
        let key = OrderKey::pointer(false, &pointer?);
        if before.is_some_and(|before| key <= before) {
            return Err(WalError::EntriesOutOfOrder.into());
        }
        before = Some(key);
    }
    Ok(())
}

/// Appends to `out` the record of the page step that removes what a
/// dropped subtree left behind in the page `page`.
///
/// - `removed` is what the step takes out: a run of remove records of
///   that page.
/// - `freed` is the commit version the page is freed with, if the step
///   frees it: the version of the drop, or a later one that a commit
///   before the record has.
/// - `released` are the value pointers the removed cells held, in
///   ascending order of file and offset, each once.
///
/// A step removes something or frees its page.
pub fn write_reclaim(
    page: PageId,
    removed: Option<Removed<'_>>,
    freed: Option<CommitVersion>,
    released: impl ExactSizeIterator<Item = ValuePointer> + Clone,
    out: &mut Vec<u8>,
) -> Result<(), FormatError> {
    match removed {
        Some(removed) => removed.check(page)?,
        None if freed.is_none() => return Err(WalError::InvalidChange.into()),
        None => {}
    }
    if freed == Some(CommitVersion::EMPTY) {
        return Err(WalError::NotACommit.into());
    }
    check_released(released.clone().map(Ok))?;
    let count = u16::try_from(released.len()).map_err(|_| WalError::InvalidChange)?;
    let flags = match removed {
        Some(Removed::Tree(_)) => TREE_RUN,
        Some(Removed::Flat(_)) => FLAT_RUN,
        None => 0,
    } | if freed.is_some() { FREED } else { 0 };

    let mut payload = vec![CHANGE, RECLAIM, flags];
    payload.extend_from_slice(&page.encode());
    if let Some(version) = freed {
        payload.extend_from_slice(&version.encode());
    }
    payload.extend_from_slice(&count.to_le_bytes());
    released.for_each(|pointer| payload.extend_from_slice(&pointer.encode()));
    if let Some(removed) = &removed {
        payload.extend_from_slice(removed.as_bytes());
    }
    write_record(&payload, out)
}
