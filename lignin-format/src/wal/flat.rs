//! A run of changes to one flat page: what a commit group holds for a page
//! of the flat keyspace.

use std::{cmp::Ordering, iter::FusedIterator};

use super::{record::Fields, WalError};
use crate::{
    error::FormatError,
    flat_page::{FlatBranchBuilder, FlatKey, FlatLeafBuilder, FlatPage, FlatValue},
    header::FormatVersion,
    ids::PageId,
    value_log::ValuePlace,
};

const IMAGE: u8 = 1;
const INSERT: u8 = 2;
const REPLACE: u8 = 3;
const REMOVE: u8 = 4;
const REMOVE_RANGE: u8 = 5;
const INSERT_CHILD: u8 = 6;
const SET_PREFIX: u8 = 7;

const OUT_OF_LINE: u8 = 1;

/// One change to a flat page.
///
/// A key is the whole key, with the page's prefix. What a change borrows
/// comes from the caller when it is written and from the run's bytes when
/// it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatChange<'a> {
    /// The page becomes this image, whatever it was: a new page, or one
    /// that is written whole. Only the first change of a run, and an image
    /// of the run's page.
    Image(&'a [u8]),
    /// A leaf gets an entry under a key it does not hold.
    Insert {
        /// The key.
        key: &'a [u8],
        /// The value, or the 16 bytes of the place of a value out of line.
        value: FlatValue<'a>,
    },
    /// The value under a key of a leaf is replaced.
    Replace {
        /// The key.
        key: &'a [u8],
        /// The value, or the 16 bytes of the place of a value out of line.
        value: FlatValue<'a>,
    },
    /// The entry of a leaf under a key leaves, or the separator of a
    /// branch with the child to its right.
    Remove {
        /// The key.
        key: &'a [u8],
    },
    /// Several entries leave, one after another in key order: the delete
    /// of a range of keys, and the half of a page that a split carries
    /// away.
    RemoveRange {
        /// The key of the first entry that leaves.
        key: &'a [u8],
        /// How many entries leave, at least 2.
        count: u16,
    },
    /// A branch gets a separator, with the child that holds the keys from
    /// it upward.
    InsertChild {
        /// The separator.
        key: &'a [u8],
        /// The child.
        child: PageId,
    },
    /// The page stores another prefix. Its keys stay as they are.
    SetPrefix {
        /// The prefix, which every key of the page starts with.
        prefix: &'a [u8],
    },
}

fn check_place(value: FlatValue<'_>) -> Result<(), FormatError> {
    let FlatValue::OutOfLine(place) = value else {
        return Ok(());
    };
    let place: [u8; ValuePlace::ENCODED_LEN] = place
        .try_into()
        .map_err(|_| WalError::PlaceLength { len: place.len() })?;
    ValuePlace::decode(place).map(|_| ())
}

fn check_page(found: PageId, expected: PageId) -> Result<(), FormatError> {
    if found != expected {
        return Err(WalError::WrongPage { found, expected }.into());
    }
    Ok(())
}

fn check_image(image: &[u8], page: PageId, version: FormatVersion) -> Result<(), FormatError> {
    let read = FlatPage::decode(image, version)?;
    if read.as_bytes().len() != image.len() {
        return Err(WalError::ImageLength {
            stated: read.as_bytes().len(),
            found: image.len(),
        }
        .into());
    }
    check_page(read.id(), page)
}

fn put_field(out: &mut Vec<u8>, field: &[u8]) -> Result<(), FormatError> {
    let len =
        u16::try_from(field.len()).map_err(|_| WalError::FlatFieldTooLong { len: field.len() })?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(field);
    Ok(())
}

fn put_entry(
    out: &mut Vec<u8>,
    kind: u8,
    key: &[u8],
    value: FlatValue<'_>,
) -> Result<(), FormatError> {
    check_place(value)?;
    out.push(kind);
    put_field(out, key)?;
    out.push(if value.is_inline() { 0 } else { OUT_OF_LINE });
    put_field(out, value.bytes())
}

impl<'a> FlatChange<'a> {
    fn write(&self, page: PageId, out: &mut Vec<u8>) -> Result<(), FormatError> {
        match *self {
            Self::Image(image) => {
                check_image(image, page, FormatVersion::CURRENT)?;
                out.push(IMAGE);
                put_field(out, image)
            }
            Self::Insert { key, value } => put_entry(out, INSERT, key, value),
            Self::Replace { key, value } => put_entry(out, REPLACE, key, value),
            Self::Remove { key } => {
                out.push(REMOVE);
                put_field(out, key)
            }
            Self::RemoveRange { key, count } => {
                if count < 2 {
                    return Err(WalError::InvalidRange.into());
                }
                out.push(REMOVE_RANGE);
                put_field(out, key)?;
                out.extend_from_slice(&count.to_le_bytes());
                Ok(())
            }
            Self::InsertChild { key, child } => {
                out.push(INSERT_CHILD);
                put_field(out, key)?;
                out.extend_from_slice(&child.encode());
                Ok(())
            }
            Self::SetPrefix { prefix } => {
                out.push(SET_PREFIX);
                put_field(out, prefix)
            }
        }
    }

    /// Appends the change's record to `out`. On an error `out` is as it
    /// was.
    fn encode_into(&self, page: PageId, out: &mut Vec<u8>) -> Result<(), FormatError> {
        let start = out.len();
        self.write(page, out).inspect_err(|_| out.truncate(start))
    }

    fn read(
        fields: &mut Fields<'a>,
        page: PageId,
        version: FormatVersion,
    ) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => {}
        }
        let field = |fields: &mut Fields<'a>| {
            let len = usize::from(fields.u16()?);
            fields.take(len)
        };
        Ok(match fields.byte()? {
            IMAGE => {
                let image = field(fields)?;
                check_image(image, page, version)?;
                Self::Image(image)
            }
            kind @ (INSERT | REPLACE) => {
                let key = field(fields)?;
                let flags = fields.byte()?;
                if flags & !OUT_OF_LINE != 0 {
                    return Err(FormatError::NonZeroReserved);
                }
                let bytes = field(fields)?;
                let value = if flags == 0 {
                    FlatValue::Inline(bytes)
                } else {
                    FlatValue::OutOfLine(bytes)
                };
                check_place(value)?;
                if kind == INSERT {
                    Self::Insert { key, value }
                } else {
                    Self::Replace { key, value }
                }
            }
            REMOVE => Self::Remove {
                key: field(fields)?,
            },
            REMOVE_RANGE => {
                let key = field(fields)?;
                let count = fields.u16()?;
                if count < 2 {
                    return Err(WalError::InvalidRange.into());
                }
                Self::RemoveRange { key, count }
            }
            INSERT_CHILD => Self::InsertChild {
                key: field(fields)?,
                child: fields.page()?,
            },
            SET_PREFIX => Self::SetPrefix {
                prefix: field(fields)?,
            },
            code => return Err(FormatError::UnknownRecordKind { code: code.into() }),
        })
    }
}

/// The changes of one flat page, in the order they were made: what a
/// commit group holds for a page of the flat keyspace.
///
/// A run names its page. It has no checksum and no commit version of its
/// own: the log group it lies in has both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlatRun<'a> {
    bytes: &'a [u8],
    page: PageId,
    count: usize,
    image: Option<&'a [u8]>,
    version: FormatVersion,
}

impl<'a> FlatRun<'a> {
    /// Bytes of a run before its first record: the page id.
    pub const HEADER_LEN: usize = PageId::ENCODED_LEN;

    /// Reads and fully checks the run that is exactly `bytes`, written in
    /// format `version`.
    ///
    /// A run carries no checksum, so `bytes` must come from a record whose
    /// checksum was verified. The checks run in a fixed order: the page id,
    /// that there is a record, then each record in turn, of which only the
    /// first may be an image.
    pub fn decode(bytes: &'a [u8], version: FormatVersion) -> Result<Self, FormatError> {
        let mut fields = Fields::new(bytes, 0);
        let page = fields.page()?;
        if fields.is_empty() {
            return Err(WalError::EmptyFlatRun.into());
        }
        let mut count = 0;
        let mut image = None;
        while !fields.is_empty() {
            match FlatChange::read(&mut fields, page, version)? {
                FlatChange::Image(first) if count == 0 => image = Some(first),
                FlatChange::Image(_) => return Err(WalError::MisplacedImage.into()),
                _ => {}
            }
            count += 1;
        }
        Ok(Self {
            bytes,
            page,
            count,
            image,
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

    /// The image the run starts from, if its first record is one. Such a
    /// run needs no page to apply to.
    pub const fn image(&self) -> Option<&'a [u8]> {
        self.image
    }

    /// The changes, in order.
    pub fn changes(&self) -> FlatChanges<'a> {
        FlatChanges {
            fields: Fields::new(self.bytes.get(Self::HEADER_LEN..).unwrap_or_default(), 0),
            remaining: self.count,
            page: self.page,
            version: self.version,
        }
    }

    /// Applies the run to the page whose image is `base`, in format
    /// `version`, and appends the image of the page it gives to `out`. On
    /// an error `out` is unchanged.
    ///
    /// `base` comes from a record whose checksum was verified, and is the
    /// image of the page the run names. It is not read by a run that starts
    /// from an image of its own, and may then be `None`. `capacity` is the
    /// longest image the store's page size allows.
    ///
    /// A change that does not fit the page it finds is refused: a key that
    /// is there where it must not be, or the other way round. On a run
    /// written by a correct writer, applied once and in its place, none is.
    pub fn apply(
        &self,
        base: Option<&[u8]>,
        version: FormatVersion,
        capacity: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), FormatError> {
        let mut changes = self.changes();
        let mut content = match (self.image, base) {
            (Some(image), _) => {
                changes.next();
                Content::read(image, self.version)?
            }
            (None, Some(base)) => Content::read(base, version)?,
            (None, None) => return Err(WalError::NoPage.into()),
        };
        check_page(content.page, self.page)?;
        changes.try_for_each(|change| content.apply(change))?;
        out.extend_from_slice(&content.build(capacity)?);
        Ok(())
    }
}

/// The changes of a run, in order.
#[derive(Debug, Clone)]
pub struct FlatChanges<'a> {
    fields: Fields<'a>,
    remaining: usize,
    page: PageId,
    version: FormatVersion,
}

impl<'a> Iterator for FlatChanges<'a> {
    type Item = FlatChange<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.remaining = self.remaining.checked_sub(1)?;
        let change = FlatChange::read(&mut self.fields, self.page, self.version).ok();
        if change.is_none() {
            self.remaining = 0;
        }
        change
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for FlatChanges<'_> {}

impl FusedIterator for FlatChanges<'_> {}

/// Writes one run to the end of a byte string.
///
/// Until [`Self::finish`] returns, the byte string ends with an unfinished
/// run.
#[derive(Debug)]
pub struct FlatRunWriter<'o> {
    out: &'o mut Vec<u8>,
    start: usize,
    page: PageId,
}

impl<'o> FlatRunWriter<'o> {
    /// Starts a run for the page `page`, at the end of `out`.
    pub fn new(page: PageId, out: &'o mut Vec<u8>) -> Self {
        let start = out.len();
        out.extend_from_slice(&page.encode());
        Self { out, start, page }
    }

    /// Appends the record of `change`. On an error the run is as it was.
    ///
    /// An image is the first record of its run or none, and is an image of
    /// the run's page. A change that would not read back is not written.
    pub fn push(&mut self, change: &FlatChange<'_>) -> Result<(), FormatError> {
        if matches!(change, FlatChange::Image(_)) && self.encoded_len() != FlatRun::HEADER_LEN {
            return Err(WalError::MisplacedImage.into());
        }
        change.encode_into(self.page, self.out)
    }

    /// Bytes of the run so far.
    pub fn encoded_len(&self) -> usize {
        self.out.len() - self.start
    }

    /// Ends the run and returns its length in bytes.
    ///
    /// A run with no record is not a run: it is taken off the byte string
    /// again, and the call fails with [`WalError::EmptyFlatRun`].
    pub fn finish(self) -> Result<usize, FormatError> {
        if self.encoded_len() == FlatRun::HEADER_LEN {
            self.out.truncate(self.start);
            return Err(WalError::EmptyFlatRun.into());
        }
        Ok(self.encoded_len())
    }
}

#[derive(Debug, Clone, Copy)]
enum Key<'a> {
    Page(FlatKey<'a>),
    Whole(&'a [u8]),
}

impl Key<'_> {
    fn cmp_bytes(&self, other: &[u8]) -> Ordering {
        match self {
            Self::Page(key) => key.cmp_bytes(other),
            Self::Whole(key) => (*key).cmp(other),
        }
    }
}

type Keyed<'a, T> = Vec<(Key<'a>, T)>;

fn insert<'a, T>(entries: &mut Keyed<'a, T>, key: &'a [u8], value: T) -> Result<(), FormatError> {
    match entries.binary_search_by(|(entry, _)| entry.cmp_bytes(key)) {
        Ok(_) => Err(WalError::KeyPresent.into()),
        Err(at) => {
            entries.insert(at, (Key::Whole(key), value));
            Ok(())
        }
    }
}

fn find<T>(entries: &Keyed<'_, T>, key: &[u8]) -> Result<usize, FormatError> {
    entries
        .binary_search_by(|(entry, _)| entry.cmp_bytes(key))
        .map_err(|_| WalError::KeyAbsent.into())
}

fn remove<T>(entries: &mut Keyed<'_, T>, key: &[u8], count: usize) -> Result<(), FormatError> {
    let at = find(entries, key)?;
    let end = at + count;
    if end > entries.len() {
        return Err(WalError::RangeTooLong.into());
    }
    entries.drain(at..end);
    Ok(())
}

#[derive(Debug)]
enum Entries<'a> {
    Leaf(Keyed<'a, FlatValue<'a>>),
    Branch {
        level: u8,
        first: PageId,
        separators: Keyed<'a, PageId>,
    },
}

impl Entries<'_> {
    fn remove(&mut self, key: &[u8], count: usize) -> Result<(), FormatError> {
        match self {
            Self::Leaf(entries) => remove(entries, key, count),
            Self::Branch { separators, .. } => remove(separators, key, count),
        }
    }
}

/// What a flat page holds, taken apart so that it can be changed.
#[derive(Debug)]
struct Content<'a> {
    page: PageId,
    prefix: &'a [u8],
    entries: Entries<'a>,
}

impl<'a> Content<'a> {
    fn read(image: &'a [u8], version: FormatVersion) -> Result<Self, FormatError> {
        let page = FlatPage::decode(image, version)?;
        let entries = match page {
            FlatPage::Leaf(leaf) => Entries::Leaf(
                leaf.iter()
                    .map(|(key, value)| (Key::Page(key), value))
                    .collect(),
            ),
            FlatPage::Branch(branch) => Entries::Branch {
                level: branch.level(),
                first: branch.child(0).ok_or(FormatError::MissingId)?,
                separators: (0..branch.len())
                    .filter_map(|slot| {
                        Some((Key::Page(branch.separator(slot)?), branch.child(slot + 1)?))
                    })
                    .collect(),
            },
        };
        Ok(Self {
            page: page.id(),
            prefix: page.prefix(),
            entries,
        })
    }

    fn apply(&mut self, change: FlatChange<'a>) -> Result<(), FormatError> {
        match (change, &mut self.entries) {
            (FlatChange::Image(_), _) => Err(WalError::MisplacedImage.into()),
            (FlatChange::SetPrefix { prefix }, _) => {
                self.prefix = prefix;
                Ok(())
            }
            (FlatChange::Insert { key, value }, Entries::Leaf(entries)) => {
                insert(entries, key, value)
            }
            (FlatChange::Replace { key, value }, Entries::Leaf(entries)) => {
                let at = find(entries, key)?;
                entries[at].1 = value;
                Ok(())
            }
            (FlatChange::InsertChild { key, child }, Entries::Branch { separators, .. }) => {
                insert(separators, key, child)
            }
            (FlatChange::Remove { key }, entries) => entries.remove(key, 1),
            (FlatChange::RemoveRange { key, count }, entries) => entries.remove(key, count.into()),
            (FlatChange::Insert { .. } | FlatChange::Replace { .. }, Entries::Branch { .. })
            | (FlatChange::InsertChild { .. }, Entries::Leaf(_)) => {
                Err(WalError::WrongPageKind.into())
            }
        }
    }

    /// Makes the entry that holds the place `from` hold the place `to`.
    fn repoint(&mut self, from: &[u8], to: &'a [u8]) -> Result<(), FormatError> {
        let Entries::Leaf(entries) = &mut self.entries else {
            return Err(WalError::WrongPageKind.into());
        };
        let entry = entries
            .iter_mut()
            .find(|(_, value)| *value == FlatValue::OutOfLine(from))
            .ok_or(WalError::NoSuchPlace)?;
        entry.1 = FlatValue::OutOfLine(to);
        Ok(())
    }

    fn build(&self, capacity: usize) -> Result<Vec<u8>, FormatError> {
        match &self.entries {
            Entries::Leaf(entries) => {
                let mut page = FlatLeafBuilder::new(self.page, self.prefix, capacity)?;
                for &(key, value) in entries {
                    match key {
                        Key::Page(key) => page.push_key(key, value)?,
                        Key::Whole(key) => page.push(key, value)?,
                    }
                }
                Ok(page.finish())
            }
            Entries::Branch {
                level,
                first,
                separators,
            } => {
                let mut page =
                    FlatBranchBuilder::new(self.page, self.prefix, capacity, *level, *first)?;
                for &(key, child) in separators {
                    match key {
                        Key::Page(key) => page.push_key(key, child)?,
                        Key::Whole(key) => page.push(key, child)?,
                    }
                }
                Ok(page.finish())
            }
        }
    }
}

/// Appends to `out` the image of the leaf `image` with the place `from` of
/// one of its values replaced by `to`.
pub(super) fn repoint(
    image: &[u8],
    page: PageId,
    version: FormatVersion,
    capacity: usize,
    from: &[u8; ValuePlace::ENCODED_LEN],
    to: &[u8; ValuePlace::ENCODED_LEN],
    out: &mut Vec<u8>,
) -> Result<(), FormatError> {
    let mut content = Content::read(image, version)?;
    check_page(content.page, page)?;
    content.repoint(from, to)?;
    out.extend_from_slice(&content.build(capacity)?);
    Ok(())
}

/// Whether `image` is the image of the leaf `page` and has an entry whose
/// value is out of line at the place `place`.
pub(super) fn holds_place(
    image: &[u8],
    page: PageId,
    version: FormatVersion,
    place: &[u8; ValuePlace::ENCODED_LEN],
) -> Result<bool, FormatError> {
    let read = FlatPage::decode(image, version)?;
    Ok(read.id() == page
        && read.leaf().is_some_and(|leaf| {
            leaf.iter()
                .any(|(_, value)| value == FlatValue::OutOfLine(place))
        }))
}

/// Whether every change of `run` only takes entries out of its page.
pub(super) fn only_removes(run: &FlatRun<'_>) -> bool {
    run.changes().all(|change| {
        matches!(
            change,
            FlatChange::Remove { .. } | FlatChange::RemoveRange { .. }
        )
    })
}

const _: () = assert!(u16::MAX as usize == FlatPage::MAX_LEN);
