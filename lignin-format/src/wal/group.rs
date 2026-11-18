//! A commit group: its parts, their entries, and the writer.

use std::iter::FusedIterator;

use super::{
    flat::{FlatRun, FlatRunWriter},
    place::LogAddress,
    record::{Fields, PART},
    value::{self, write_value, CarriedValue, LoggedValue},
    Limits, WalError,
};
use crate::{
    checksum::Checksum,
    delta::{DeltaRun, DeltaRunWriter},
    error::FormatError,
    header::{FormatVersion, RecordHeader},
    ids::{CommitVersion, PageId},
    value_log::{PendingValue, ValuePointer},
};

pub(super) const PART_FIELDS_LEN: usize = 1 + 1 + CommitVersion::ENCODED_LEN + 4;
pub(super) const RUN_ENTRY_LEN: usize = 1 + 4;

const PART_HEADER_LEN: usize = RecordHeader::ENCODED_LEN + PART_FIELDS_LEN;
const FLAGS_AT: usize = RecordHeader::ENCODED_LEN + 1;
const LAST: u8 = 1;

const TREE: u8 = 1;
const FLAT: u8 = 2;
const FREED: u8 = 3;
const RELEASED: u8 = 4;
const ACQUIRED: u8 = 5;

/// Where an entry stands in the order of its group: the pages first, by
/// page id, then the released pointers, then the acquired ones, each by
/// file and offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct OrderKey {
    rank: u8,
    major: u32,
    minor: u64,
}

impl OrderKey {
    const fn page(page: PageId) -> Self {
        Self {
            rank: 0,
            major: page.get(),
            minor: 0,
        }
    }

    pub(super) const fn pointer(acquired: bool, pointer: &ValuePointer) -> Self {
        Self {
            rank: if acquired { 2 } else { 1 },
            major: pointer.file().get(),
            minor: pointer.offset(),
        }
    }
}

/// One thing a commit group says the commit did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry<'a> {
    /// The changes to a tree page, as a run of page deltas. A run that
    /// creates its page says that the commit made the page.
    Tree(DeltaRun<'a>),
    /// The changes to a flat page.
    Flat(FlatRun<'a>),
    /// The commit freed the page.
    Freed(PageId),
    /// A cell stopped holding this value pointer.
    Released(ValuePointer),
    /// A cell started to hold this value pointer.
    Acquired(ValuePointer),
}

impl<'a> Entry<'a> {
    /// The page the entry is about: the page a run changes, or the page
    /// that was freed.
    pub const fn page(&self) -> Option<PageId> {
        match self {
            Self::Tree(run) => Some(run.page()),
            Self::Flat(run) => Some(run.page()),
            Self::Freed(page) => Some(*page),
            Self::Released(_) | Self::Acquired(_) => None,
        }
    }

    const fn key(&self) -> OrderKey {
        match self {
            Self::Tree(run) => OrderKey::page(run.page()),
            Self::Flat(run) => OrderKey::page(run.page()),
            Self::Freed(page) => OrderKey::page(*page),
            Self::Released(pointer) => OrderKey::pointer(false, pointer),
            Self::Acquired(pointer) => OrderKey::pointer(true, pointer),
        }
    }

    fn read(fields: &mut Fields<'a>, version: FormatVersion) -> Result<Self, FormatError> {
        let run = |fields: &mut Fields<'a>| {
            let len = fields.u32()? as usize;
            fields.take(len)
        };
        Ok(match fields.byte()? {
            TREE => Self::Tree(DeltaRun::decode(run(fields)?, version)?),
            FLAT => Self::Flat(FlatRun::decode(run(fields)?, version)?),
            FREED => Self::Freed(fields.page()?),
            RELEASED => Self::Released(fields.pointer()?),
            ACQUIRED => Self::Acquired(fields.pointer()?),
            code => return Err(FormatError::UnknownRecordKind { code: code.into() }),
        })
    }
}

/// The entries of a commit group, or of one of its parts, in order.
#[derive(Debug, Clone)]
pub struct Entries<'a> {
    records: &'a [u8],
    part: Fields<'a>,
    version: FormatVersion,
}

impl<'a> Entries<'a> {
    fn stop(&mut self) {
        self.records = &[];
        self.part = Fields::new(&[], 0);
    }
}

impl<'a> Iterator for Entries<'a> {
    type Item = Entry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.part.is_empty() {
            let frame: &[u8; 4] = self.records.first_chunk()?;
            let len = RecordHeader::ENCODED_LEN + u32::from_le_bytes(*frame) as usize;
            let Some((record, rest)) = self.records.split_at_checked(len) else {
                self.stop();
                return None;
            };
            self.records = rest;
            self.part = Fields::new(
                record.get(PART_HEADER_LEN..).unwrap_or_default(),
                PART_HEADER_LEN,
            );
        }
        let entry = Entry::read(&mut self.part, self.version).ok();
        if entry.is_none() {
            self.stop();
        }
        entry
    }
}

impl FusedIterator for Entries<'_> {}

/// One part of a commit group: a record that holds some of the group's
/// entries. A group that fits one record has one part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupPart<'a> {
    commit: CommitVersion,
    part: u32,
    last: bool,
    entries: &'a [u8],
    payload_len: usize,
    first: Option<(OrderKey, usize)>,
    last_key: Option<OrderKey>,
    version: FormatVersion,
}

impl<'a> GroupPart<'a> {
    /// The commit version of the group.
    pub const fn version(&self) -> CommitVersion {
        self.commit
    }

    /// The part's place among the parts of its group, from 0.
    pub const fn part(&self) -> u32 {
        self.part
    }

    /// Whether this is the last part of its group.
    pub const fn is_last(&self) -> bool {
        self.last
    }

    /// Whether the part holds no entry.
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries of this part, in order.
    pub const fn entries(&self) -> Entries<'a> {
        Entries {
            records: &[],
            part: Fields::new(self.entries, PART_HEADER_LEN),
            version: self.version,
        }
    }

    pub(super) const fn payload_len(&self) -> usize {
        self.payload_len
    }

    /// The place in the group's order of the part's first entry, and that
    /// entry's length.
    pub(super) const fn first(&self) -> Option<(OrderKey, usize)> {
        self.first
    }

    pub(super) const fn last_key(&self) -> Option<OrderKey> {
        self.last_key
    }

    pub(super) fn decode(
        mut fields: Fields<'a>,
        payload_len: usize,
        version: FormatVersion,
    ) -> Result<Self, FormatError> {
        match version {
            FormatVersion::V1 => {}
        }
        if payload_len < PART_FIELDS_LEN {
            return Err(WalError::RecordLength {
                kind: PART,
                len: payload_len,
            }
            .into());
        }
        let flags = fields.byte()?;
        if flags & !LAST != 0 {
            return Err(FormatError::NonZeroReserved);
        }
        let last = flags & LAST != 0;
        let commit = CommitVersion::decode(*fields.array()?).ok_or(FormatError::MissingId)?;
        if commit == CommitVersion::EMPTY {
            return Err(WalError::NotACommit.into());
        }
        let part = fields.u32()?;
        let entries = fields.rest();
        let mut walk = Fields::new(entries, PART_HEADER_LEN);
        let mut first = None;
        let mut last_key = None;
        while !walk.is_empty() {
            let at = walk.position();
            let key = Entry::read(&mut walk, version)?.key();
            if last_key.is_some_and(|before| key <= before) {
                return Err(WalError::EntriesOutOfOrder.into());
            }
            first = first.or(Some((key, walk.position() - at)));
            last_key = Some(key);
        }
        if entries.is_empty() && (!last || part != 0) {
            return Err(WalError::EmptyPart.into());
        }
        Ok(Self {
            commit,
            part,
            last,
            entries,
            payload_len,
            first,
            last_key,
            version,
        })
    }
}

/// A whole commit group, as a scan of a log file found it: the values the
/// commit wrote, and what it did to pages and to the counts of the value
/// log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Group<'a> {
    bytes: &'a [u8],
    values_len: usize,
    commit: CommitVersion,
    address: LogAddress,
    version: FormatVersion,
    limits: Limits,
}

impl<'a> Group<'a> {
    pub(super) const fn new(
        bytes: &'a [u8],
        values_len: usize,
        commit: CommitVersion,
        address: LogAddress,
        version: FormatVersion,
        limits: Limits,
    ) -> Self {
        Self {
            bytes,
            values_len,
            commit,
            address,
            version,
            limits,
        }
    }

    /// The commit version of the group.
    pub const fn version(&self) -> CommitVersion {
        self.commit
    }

    /// The log address of the group's first byte.
    pub const fn address(&self) -> LogAddress {
        self.address
    }

    /// The group's bytes: its records, one after another.
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Bytes of the group.
    pub const fn encoded_len(&self) -> usize {
        self.bytes.len()
    }

    /// The values the group carries, each with the log address of its
    /// first record: the address the cell of that value holds while the
    /// value is pending.
    pub fn values(&self) -> GroupValues<'a> {
        GroupValues {
            rest: self.bytes.get(..self.values_len).unwrap_or_default(),
            address: self.address,
            version: self.version,
            limits: self.limits,
        }
    }

    /// The entries of the group, in order, across all of its parts.
    pub fn entries(&self) -> Entries<'a> {
        Entries {
            records: self.bytes.get(self.values_len..).unwrap_or_default(),
            part: Fields::new(&[], 0),
            version: self.version,
        }
    }
}

/// The values of a commit group, in the order they stand in the log.
#[derive(Debug, Clone)]
pub struct GroupValues<'a> {
    rest: &'a [u8],
    address: LogAddress,
    version: FormatVersion,
    limits: Limits,
}

impl<'a> Iterator for GroupValues<'a> {
    type Item = (LogAddress, LoggedValue<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let read = value::read_with(self.rest, self.version, self.limits).ok();
        let next = read.and_then(|(value, len)| {
            let after = self.address.after(len as u64).ok()?;
            Some((value, self.rest.get(len..)?, after))
        });
        let Some((value, rest, after)) = next else {
            self.rest = &[];
            return None;
        };
        let address = self.address;
        self.rest = rest;
        self.address = after;
        Some((address, value))
    }
}

impl FusedIterator for GroupValues<'_> {}

/// Writes one commit group to the end of a byte string: first the values,
/// then the entries.
///
/// Until [`Self::finish`] returns, the byte string ends with an unfinished
/// group, which no reader takes.
#[derive(Debug)]
pub struct GroupWriter<'o> {
    out: &'o mut Vec<u8>,
    start: usize,
    address: LogAddress,
    version: CommitVersion,
    limits: Limits,
    part_at: Option<usize>,
    part: u32,
    last_key: Option<OrderKey>,
}

impl<'o> GroupWriter<'o> {
    /// Starts the group of the commit `version` at the end of `out`.
    ///
    /// `address` is the log address the group's first byte will have: the
    /// values of the group are named by their place in the log, so a group
    /// is written for one place and for no other.
    pub fn new(
        version: CommitVersion,
        address: LogAddress,
        out: &'o mut Vec<u8>,
    ) -> Result<Self, FormatError> {
        Self::with_limits(version, address, Limits::FORMAT, out)
    }

    pub(crate) fn with_limits(
        version: CommitVersion,
        address: LogAddress,
        limits: Limits,
        out: &'o mut Vec<u8>,
    ) -> Result<Self, FormatError> {
        if version == CommitVersion::EMPTY {
            return Err(WalError::NotACommit.into());
        }
        Ok(Self {
            start: out.len(),
            out,
            address,
            version,
            limits,
            part_at: None,
            part: 0,
            last_key: None,
        })
    }

    /// Bytes of the group so far.
    pub fn encoded_len(&self) -> usize {
        self.out.len() - self.start
    }

    fn next_value(&self) -> Result<LogAddress, FormatError> {
        if self.part_at.is_some() {
            return Err(WalError::ValueAfterEntries.into());
        }
        self.address.after(self.encoded_len() as u64)
    }

    /// Appends the bytes of a value that the commit wrote for a cell of
    /// the page `owner`, and returns what that cell holds until the value
    /// has a place in the value log.
    ///
    /// Values come before every entry. On an error the group is as it was.
    pub fn value(&mut self, owner: PageId, value: &[u8]) -> Result<PendingValue, FormatError> {
        let address = self.next_value()?;
        let at = self.out.len();
        write_value(owner, value, self.limits, self.out)
            .and_then(|len| address.pending(len))
            .inspect_err(|_| self.out.truncate(at))
    }

    /// Appends the name of a carried value, and returns what its cell
    /// holds until the value has a place of its own in the value log.
    ///
    /// Values come before every entry. On an error the group is as it was.
    pub fn carried(&mut self, carried: &CarriedValue) -> Result<PendingValue, FormatError> {
        let address = self.next_value()?;
        let at = self.out.len();
        carried
            .write(self.out)
            .and_then(|()| address.pending(carried.value_len()))
            .inspect_err(|_| self.out.truncate(at))
    }

    fn open_part(&mut self) -> usize {
        let at = self.out.len();
        self.out.extend_from_slice(&[0; RecordHeader::ENCODED_LEN]);
        self.out.push(PART);
        self.out.push(0);
        self.out.extend_from_slice(&self.version.encode());
        self.out.extend_from_slice(&self.part.to_le_bytes());
        self.part_at = Some(at);
        at
    }

    fn close_part(&mut self, at: usize, last: bool) {
        self.out[at + FLAGS_AT] = if last { LAST } else { 0 };
        let payload = &self.out[at + RecordHeader::ENCODED_LEN..];
        let payload_len = (payload.len() as u32).to_le_bytes();
        let checksum = Checksum::of(&[&payload_len, payload]).encode();
        self.out[at..at + 4].copy_from_slice(&payload_len);
        self.out[at + 4..at + RecordHeader::ENCODED_LEN].copy_from_slice(&checksum);
    }

    /// Makes the entry that was written at `entry_at` the last of the
    /// part at `part_at`, or the first of a new part when that one has no
    /// room for it.
    fn fit(&mut self, part_at: usize, entry_at: usize) -> Result<(), FormatError> {
        let entry_len = self.out.len() - entry_at;
        if entry_len > self.limits.max_entries() {
            return Err(WalError::RunTooLong {
                len: entry_len.saturating_sub(RUN_ENTRY_LEN),
                max: self.limits.max_run(),
            }
            .into());
        }
        if self.out.len() - part_at - RecordHeader::ENCODED_LEN <= self.limits.max_payload {
            return Ok(());
        }
        let next = self.part.checked_add(1).ok_or(WalError::TooManyParts)?;
        let entry = self.out.split_off(entry_at);
        self.close_part(part_at, false);
        self.part = next;
        self.open_part();
        self.out.extend_from_slice(&entry);
        Ok(())
    }

    fn entry(
        &mut self,
        key: OrderKey,
        write: impl FnOnce(&mut Vec<u8>) -> Result<(), FormatError>,
    ) -> Result<(), FormatError> {
        if self.last_key.is_some_and(|before| key <= before) {
            return Err(WalError::EntriesOutOfOrder.into());
        }
        let (len_before, part_at_before, part_before) = (self.out.len(), self.part_at, self.part);
        let part_at = match self.part_at {
            Some(at) => at,
            None => self.open_part(),
        };
        let entry_at = self.out.len();
        let written = write(self.out).and_then(|()| self.fit(part_at, entry_at));
        match written {
            Ok(()) => self.last_key = Some(key),
            Err(_) => {
                self.out.truncate(len_before);
                self.part_at = part_at_before;
                self.part = part_before;
            }
        }
        written
    }

    fn run(
        &mut self,
        kind: u8,
        page: PageId,
        write: impl FnOnce(&mut Vec<u8>) -> Result<usize, FormatError>,
    ) -> Result<(), FormatError> {
        let max = self.limits.max_run();
        self.entry(OrderKey::page(page), |out| {
            out.push(kind);
            let len_at = out.len();
            out.extend_from_slice(&[0; 4]);
            let len = write(out)?;
            let field = u32::try_from(len).map_err(|_| WalError::RunTooLong { len, max })?;
            out[len_at..len_at + 4].copy_from_slice(&field.to_le_bytes());
            Ok(())
        })
    }

    /// Appends the changes the commit made to a tree page. A run that
    /// creates its page says that the commit made the page.
    ///
    /// The pages of a group come in ascending order of page id, each once,
    /// and before every pointer. On an error the group is as it was.
    pub fn tree_run(&mut self, run: &DeltaRun<'_>) -> Result<(), FormatError> {
        match run.version() {
            FormatVersion::V1 => {}
        }
        self.run(TREE, run.page(), |out| {
            out.extend_from_slice(run.as_bytes());
            Ok(run.encoded_len())
        })
    }

    /// Appends the changes the commit made to the tree page `page`, which
    /// `write` pushes into a run that is written straight into the group.
    /// With `created`, the run creates the page, as
    /// [`DeltaRunWriter::new`] takes it.
    pub fn tree_run_with(
        &mut self,
        page: PageId,
        created: Option<u64>,
        write: impl FnOnce(&mut DeltaRunWriter<'_>) -> Result<(), FormatError>,
    ) -> Result<(), FormatError> {
        self.run(TREE, page, |out| {
            let mut run = DeltaRunWriter::new(page, created, out);
            write(&mut run)?;
            run.finish()
        })
    }

    /// Appends the changes the commit made to a flat page.
    ///
    /// The pages of a group come in ascending order of page id, each once,
    /// and before every pointer. On an error the group is as it was.
    pub fn flat_run(&mut self, run: &FlatRun<'_>) -> Result<(), FormatError> {
        match run.version() {
            FormatVersion::V1 => {}
        }
        self.run(FLAT, run.page(), |out| {
            out.extend_from_slice(run.as_bytes());
            Ok(run.encoded_len())
        })
    }

    /// Appends the changes the commit made to the flat page `page`, which
    /// `write` pushes into a run that is written straight into the group.
    pub fn flat_run_with(
        &mut self,
        page: PageId,
        write: impl FnOnce(&mut FlatRunWriter<'_>) -> Result<(), FormatError>,
    ) -> Result<(), FormatError> {
        self.run(FLAT, page, |out| {
            let mut run = FlatRunWriter::new(page, out);
            write(&mut run)?;
            run.finish()
        })
    }

    /// Says that the commit freed the page `page`. A page that is freed
    /// has no run in the group.
    pub fn freed(&mut self, page: PageId) -> Result<(), FormatError> {
        self.entry(OrderKey::page(page), |out| {
            out.push(FREED);
            out.extend_from_slice(&page.encode());
            Ok(())
        })
    }

    fn pointer(&mut self, kind: u8, pointer: &ValuePointer) -> Result<(), FormatError> {
        self.entry(OrderKey::pointer(kind == ACQUIRED, pointer), |out| {
            out.push(kind);
            out.extend_from_slice(&pointer.encode());
            Ok(())
        })
    }

    /// Says that a cell stopped holding `pointer` at this commit.
    ///
    /// The released pointers come after the pages, in ascending order of
    /// file and offset, each once.
    pub fn released(&mut self, pointer: &ValuePointer) -> Result<(), FormatError> {
        self.pointer(RELEASED, pointer)
    }

    /// Says that a cell started to hold `pointer` at this commit.
    ///
    /// The acquired pointers come last, in ascending order of file and
    /// offset, each once.
    pub fn acquired(&mut self, pointer: &ValuePointer) -> Result<(), FormatError> {
        self.pointer(ACQUIRED, pointer)
    }

    /// Ends the group and returns its length in bytes.
    pub fn finish(mut self) -> usize {
        let at = match self.part_at {
            Some(at) => at,
            None => self.open_part(),
        };
        self.close_part(at, true);
        self.encoded_len()
    }

    /// Gives the group up: takes its bytes off the byte string again.
    pub fn abandon(self) {
        self.out.truncate(self.start);
    }
}
