//! A run of records for one page, and what a chain of runs makes of a page.

use std::iter::FusedIterator;

use super::{
    record::{write_create, Reader, Record},
    Delta, DeltaError,
};
use crate::{
    error::FormatError,
    header::FormatVersion,
    ids::PageId,
    tree_page::{TreePage, TreePageMut, TreePageSize},
};

/// The records of one page, in the order the changes were made: what a
/// delta block of a segment holds, and what a log group holds for a page.
///
/// A run names its page. It has no checksum and no commit version of its
/// own: the block or the log group it lies in has the checksum, and the
/// page table's link or the log group has the version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaRun<'a> {
    bytes: &'a [u8],
    page: PageId,
    created: Option<u64>,
    changes: &'a [u8],
    change_count: usize,
    version: FormatVersion,
}

impl<'a> DeltaRun<'a> {
    /// Bytes of a run before its first record: the page id.
    pub const HEADER_LEN: usize = PageId::ENCODED_LEN;

    /// Reads and fully checks the run that is exactly `bytes`, written in
    /// format `version`.
    ///
    /// A run carries no checksum, so `bytes` must come from a record whose
    /// checksum was verified. The checks run in a fixed order: the page id,
    /// that there is a record, then each record in turn, and that a create
    /// record is the first.
    pub fn decode(bytes: &'a [u8], version: FormatVersion) -> Result<Self, FormatError> {
        let mut reader = Reader::new(bytes);
        let page = PageId::decode(*reader.array()?).ok_or(FormatError::MissingId)?;
        if reader.is_empty() {
            return Err(DeltaError::EmptyRun.into());
        }
        let (created, changes_at, mut change_count) = match reader.record(version)? {
            Record::Create { departures } => (Some(departures), reader.position(), 0),
            Record::Change(_) => (None, Self::HEADER_LEN, 1),
        };
        while !reader.is_empty() {
            match reader.record(version)? {
                Record::Create { .. } => return Err(DeltaError::MisplacedCreate.into()),
                Record::Change(_) => change_count += 1,
            }
        }
        Ok(Self {
            bytes,
            page,
            created,
            changes: bytes.get(changes_at..).unwrap_or_default(),
            change_count,
            version,
        })
    }

    /// The run's bytes.
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Bytes of the run.
    pub const fn encoded_len(&self) -> usize {
        self.bytes.len()
    }

    /// The format version the run was read under.
    pub const fn version(&self) -> FormatVersion {
        self.version
    }

    /// The page the run changes.
    pub const fn page(&self) -> PageId {
        self.page
    }

    /// Whether the run creates its page, and then the count of departed
    /// cells the new page starts at. Such a run needs no page to apply to.
    pub const fn created(&self) -> Option<u64> {
        self.created
    }

    /// The changes to the cells of the page, in order.
    pub const fn deltas(&self) -> Deltas<'a> {
        Deltas {
            reader: Reader::new(self.changes),
            remaining: self.change_count,
            version: self.version,
        }
    }

    /// Applies the run to the page at the start of `frame`, a buffer of the
    /// store's page size that holds a page in format `version`.
    ///
    /// A run that creates its page starts an empty page in `frame`, in the
    /// current format version, whatever `frame` held. Any other run must
    /// name the page that `frame` holds.
    ///
    /// The page must be valid, as [`TreePageMut`] asks, and must be the
    /// page as the change before the run's first left it: a run is applied
    /// once, in its place in the chain. When a change is refused, the ones
    /// before it stay made and the page is as the refused change found it.
    pub fn apply(
        &self,
        frame: &mut [u8],
        size: TreePageSize,
        version: FormatVersion,
    ) -> Result<(), FormatError> {
        match (self.version, version) {
            (FormatVersion::V1, FormatVersion::V1) => {}
        }
        let mut page = match self.created {
            Some(departures) => TreePageMut::init(frame, size, self.page, departures)?,
            None => {
                let page = TreePageMut::open(frame, size, version)?;
                let found = page.page().id();
                if found != self.page {
                    return Err(DeltaError::WrongPage {
                        run: self.page,
                        page: found,
                    }
                    .into());
                }
                page
            }
        };
        self.deltas().try_for_each(|delta| delta.apply(&mut page))
    }
}

/// The changes of a run, in order.
#[derive(Debug, Clone)]
pub struct Deltas<'a> {
    reader: Reader<'a>,
    remaining: usize,
    version: FormatVersion,
}

impl<'a> Iterator for Deltas<'a> {
    type Item = Delta<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.remaining = self.remaining.checked_sub(1)?;
        match self.reader.record(self.version) {
            Ok(Record::Change(delta)) => Some(delta),
            _ => {
                self.remaining = 0;
                None
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for Deltas<'_> {}

impl FusedIterator for Deltas<'_> {}

/// Writes one run to the end of a byte string.
///
/// Until [`Self::finish`] returns, the byte string ends with an unfinished
/// run.
#[derive(Debug)]
pub struct DeltaRunWriter<'o> {
    out: &'o mut Vec<u8>,
    start: usize,
    records: usize,
}

impl<'o> DeltaRunWriter<'o> {
    /// Starts a run of changes to the page `page`, at the end of `out`.
    pub fn new(page: PageId, out: &'o mut Vec<u8>) -> Self {
        let start = out.len();
        out.extend_from_slice(&page.encode());
        Self {
            out,
            start,
            records: 0,
        }
    }

    /// Starts a run that creates the page `page`, at the end of `out`. The
    /// new page's count of departed cells starts at `departures`, as
    /// [`TreePageMut::init`] takes it.
    pub fn new_page(page: PageId, departures: u64, out: &'o mut Vec<u8>) -> Self {
        let mut run = Self::new(page, out);
        write_create(departures, run.out);
        run.records = 1;
        run
    }

    /// Appends the record of `delta`. On an error the run is as it was.
    pub fn push(&mut self, delta: &Delta<'_>) -> Result<(), FormatError> {
        delta.encode_into(self.out)?;
        self.records += 1;
        Ok(())
    }

    /// Bytes of the run so far.
    pub fn encoded_len(&self) -> usize {
        self.out.len() - self.start
    }

    /// Ends the run and returns its length in bytes.
    ///
    /// A run with no record is not a run: it is taken off the byte string
    /// again, and the call fails with [`DeltaError::EmptyRun`].
    pub fn finish(self) -> Result<usize, FormatError> {
        if self.records == 0 {
            self.out.truncate(self.start);
            return Err(DeltaError::EmptyRun.into());
        }
        Ok(self.encoded_len())
    }
}

/// Applies a chain of runs, oldest first, to the page at the start of
/// `frame`: a base image that was copied there, or a page that earlier runs
/// were applied to. `version` is the format version of that page.
///
/// See [`DeltaRun::apply`].
pub fn apply_chain<'a, 'r: 'a>(
    frame: &mut [u8],
    size: TreePageSize,
    version: FormatVersion,
    chain: impl IntoIterator<Item = &'a DeltaRun<'r>>,
) -> Result<(), FormatError> {
    chain
        .into_iter()
        .try_for_each(|run| run.apply(frame, size, version))
}

/// Consolidates a chain into a base image: appends to `out` the image of
/// the page that `base` and the runs of `chain`, oldest first, give. On an
/// error `out` is unchanged.
///
/// `base` is the page's base image, in format `version`, from a record whose
/// checksum was verified. It is `None` for a chain whose first run creates
/// the page. `size` is the store's page size.
pub fn consolidate<'a, 'r: 'a>(
    base: Option<&[u8]>,
    chain: impl IntoIterator<Item = &'a DeltaRun<'r>>,
    version: FormatVersion,
    size: TreePageSize,
    out: &mut Vec<u8>,
) -> Result<(), FormatError> {
    let mut chain = chain.into_iter().peekable();
    let mut frame = vec![0; size.image_capacity()];
    match base {
        Some(base) => {
            let image = TreePage::decode(base, version, size)?.as_bytes();
            frame[..image.len()].copy_from_slice(image);
        }
        None if chain.peek().is_some_and(|run| run.created().is_some()) => {}
        None => return Err(DeltaError::MissingBase.into()),
    }
    apply_chain(&mut frame, size, version, chain)?;
    TreePage::from_validated(&frame, version)?.write_image(out);
    Ok(())
}
