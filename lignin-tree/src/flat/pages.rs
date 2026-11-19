//! What a flat keyspace asks of whoever holds its pages.

use std::ops::Deref;

use lignin_format::{
    value_log::{PendingValue, ValuePlace},
    CommitVersion, PageId, PageSize,
};

use super::error::FlatError;

/// The sizes a flat keyspace is laid out with. They come from the store's
/// page size and are the same for every page of a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatConfig {
    capacity: usize,
    max_inline: usize,
}

impl FlatConfig {
    /// The smallest [`Self::max_inline`]: a value's place takes 16 bytes of
    /// a page, so a shorter value gains nothing by leaving it.
    pub const MIN_INLINE: usize = ValuePlace::ENCODED_LEN;

    /// The sizes for a store of pages of `page_size`: a value stays in its
    /// page up to an eighth of a page.
    pub const fn new(page_size: PageSize) -> Self {
        let capacity = page_size.image_capacity();
        Self {
            capacity,
            max_inline: capacity / 8,
        }
    }

    /// The same sizes with values kept in their page up to `max_inline`
    /// bytes, from [`Self::MIN_INLINE`] to a quarter of a page.
    pub const fn with_max_inline(self, max_inline: usize) -> Result<Self, FlatError> {
        let max = self.capacity / 4;
        if max_inline < Self::MIN_INLINE || max_inline > max {
            return Err(FlatError::InlineLimit {
                limit: max_inline,
                min: Self::MIN_INLINE,
                max,
            });
        }
        Ok(Self {
            capacity: self.capacity,
            max_inline,
        })
    }

    /// The longest image of a page.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// The longest key a page stores as it is: an eighth of a page. A longer
    /// key is kept in the bucket of its first [`Self::key_limit`] bytes.
    pub const fn key_limit(&self) -> usize {
        self.capacity / 8
    }

    /// The longest value kept in its page. A longer one is out of line.
    pub const fn max_inline(&self) -> usize {
        self.max_inline
    }

    /// The image length below which a page looks for a neighbour to merge
    /// with.
    pub(super) const fn underfull(&self) -> usize {
        self.capacity / 4
    }

    /// The longest image a merge may give.
    pub(super) const fn merge_limit(&self) -> usize {
        self.capacity / 4 * 3
    }
}

/// The pages of a flat keyspace, as one reader sees them: a snapshot at one
/// version, or a write transaction with its own changes.
pub trait FlatPages {
    /// A page that is held while it is read. Its bytes do not change, and
    /// they start with the page's image.
    type Page<'a>: Deref<Target = [u8]>
    where
        Self: 'a;

    /// The flat page `page`.
    fn page(&self, page: PageId) -> Result<Self::Page<'_>, FlatError>;

    /// Appends to `out` the bytes of the value out of line that the entry
    /// in position `slot` of the leaf `page` holds at `place`.
    ///
    /// `held` is the page as the caller read the entry from it, and still
    /// holds. A pending value is found with that page, not with the page
    /// as it is stored now: write-back can have placed the value since,
    /// and taken its bytes off the frame that replaced this one.
    fn read_value(
        &self,
        held: &Self::Page<'_>,
        page: PageId,
        slot: usize,
        place: ValuePlace,
        out: &mut Vec<u8>,
    ) -> Result<(), FlatError>;

    /// What names these pages as they are: another revision after every
    /// change to a page of a writer, and another one for every reader and
    /// writer that does not read the same pages. A [`Cursor`](super::Cursor)
    /// that is given pages of another revision goes on from its key.
    ///
    /// A background step that takes an entry out of a leaf of a reader's
    /// version changes no revision: a cursor finds that by itself.
    fn revision(&self) -> Revision;
}

/// What tells the pages of a reader or a writer apart from every other
/// pages a cursor could be given ([`FlatPages::revision`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revision {
    /// The committed pages at one version: readers of one version read the
    /// same pages.
    Committed(CommitVersion),
    /// One state of the pages of one writer, by a number that no other
    /// state of any writer of the process has.
    Written(u64),
}

/// The pages of a flat keyspace, as the one writer of a write transaction
/// changes them.
///
/// The tree calls these in an order an implementation can rely on:
///
/// - a page is taken ([`Self::take`]) or allocated before it is written,
///   freed, or has a value held, dropped or carried for it
/// - what the tree does to a leaf it decides from the image it took, never
///   from the page as it read it before
/// - a page that entries are carried out of is written or freed after the
///   pages they are carried to
/// - a page is freed after every value of its entries was dropped or
///   carried
/// - the tree holds one page of [`FlatPages::page`] at a time, and none
///   across a call of this trait
pub trait FlatPagesMut: FlatPages {
    /// The id of a new page, which only this writer sees until it commits.
    fn allocate(&mut self) -> Result<PageId, FlatError>;

    /// The image of `page`, for a change. From this call until the writer
    /// commits or gives up, nothing else changes how the page is stored: a
    /// value pointer of it is not rewritten, no pending value of it is
    /// placed, and no entry that a dropped subtree left is taken out of it
    /// (`transactions.md` PV7).
    fn take(&mut self, page: PageId) -> Result<Vec<u8>, FlatError>;

    /// The page `page` becomes `image`.
    fn write(&mut self, page: PageId, image: &[u8]) -> Result<(), FlatError>;

    /// The page `page` leaves the keyspace.
    fn free(&mut self, page: PageId) -> Result<(), FlatError>;

    /// Takes a new value for an entry of the leaf `page`, and returns what
    /// the entry holds for it until the commit gives the value its place in
    /// the log.
    fn hold_value(&mut self, page: PageId, value: &[u8]) -> Result<PendingValue, FlatError>;

    /// An entry of the leaf `page` stopped holding the value at `place`:
    /// the entry was removed or was given another value.
    fn drop_value(&mut self, page: PageId, place: ValuePlace) -> Result<(), FlatError>;

    /// The entry in position `slot` of the leaf `from`, whose value is at
    /// `place`, goes to the leaf `to`. Returns what it holds there.
    ///
    /// This is the move rule of the value log (`value-log-format.md` O5):
    /// the value is given again to the new page, as a new value of it, and
    /// the old place is dropped.
    fn carry_value(
        &mut self,
        from: PageId,
        slot: usize,
        to: PageId,
        place: ValuePlace,
    ) -> Result<PendingValue, FlatError> {
        let mut value = Vec::new();
        let held = self.page(from)?;
        self.read_value(&held, from, slot, place, &mut value)?;
        drop(held);
        let held = self.hold_value(to, &value)?;
        self.drop_value(from, place)?;
        Ok(held)
    }
}

/// The length of `value`, which is at most what the value log holds
/// (`value-log-format.md` V9).
pub(super) fn value_len(value: &[u8]) -> Result<u32, FlatError> {
    u32::try_from(value.len()).map_err(|_| FlatError::ValueTooLong { len: value.len() })
}
