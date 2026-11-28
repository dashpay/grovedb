//! Walking the values of one value-log file in file order.

use std::sync::Arc;

use lignin_format::{
    value_log::{
        extent_len, read_value, record_len, Value, ValuePointer, ValueRecord, FIRST_RECORD_OFFSET,
        MAX_PART_LEN,
    },
    FormatError, RecordHeader,
};

use super::{error::ValueStoreError, file::ValueFile};

const WALK_WINDOW_LEN: u64 = 256 * 1024;

/// A walk over the values of one value-log file, in file order, from
/// [`ValueStore::walk`](super::ValueStore::walk).
///
/// Every byte it passes must belong to a whole value: the first one that
/// does not ends the walk with an error.
#[derive(Debug)]
pub struct ValueWalk {
    file: Arc<ValueFile>,
    at: u64,
    end: u64,
    window: Vec<u8>,
    window_at: u64,
}

/// One value as a walk found it.
pub(super) struct Walked<'a> {
    pub(super) pointer: ValuePointer,
    pub(super) value: Value<'a>,
    /// The value's records as they are in the file.
    pub(super) records: &'a [u8],
}

impl ValueWalk {
    pub(super) fn new(file: Arc<ValueFile>) -> Self {
        let end = file.end();
        Self {
            file,
            at: FIRST_RECORD_OFFSET,
            end,
            window: Vec::new(),
            window_at: FIRST_RECORD_OFFSET,
        }
    }

    /// The file offset of the value [`Self::next_value`] reads next, which is
    /// where the walk ends once it has returned `None`.
    pub fn offset(&self) -> u64 {
        self.at
    }

    /// Reads the next value, with every record of it verified, or `None`
    /// after the last one. The pointer is the one the value's owner must
    /// hold for the value to be live.
    pub fn next_value(&mut self) -> Result<Option<(ValuePointer, Value<'_>)>, ValueStoreError> {
        Ok(self
            .next_walked()?
            .map(|walked| (walked.pointer, walked.value)))
    }

    pub(super) fn next_walked(&mut self) -> Result<Option<Walked<'_>>, ValueStoreError> {
        self.end = self.end.min(self.file.end());
        if self.at >= self.end {
            return Ok(None);
        }
        let offset = self.at;
        let len = self.load_value()?;
        let at = self.window_offset();
        let records = &self.window[at..at + len];
        let (value, rest) =
            read_value(records, self.file.version()).map_err(self.file.format_error(offset))?;
        let records = &records[..len - rest.len()];
        self.at += records.len() as u64;
        let pointer = ValuePointer::new(self.file.id(), offset, value.value_len())
            .map_err(self.file.format_error(offset))?;
        Ok(Some(Walked {
            pointer,
            value,
            records,
        }))
    }

    fn window_offset(&self) -> usize {
        (self.at - self.window_at) as usize
    }

    fn truncated(&self, needed: u64, left: u64) -> ValueStoreError {
        self.file.format_error(self.at)(FormatError::Truncated {
            needed: usize::try_from(needed).unwrap_or(usize::MAX),
            available: left as usize,
        })
    }

    /// Brings the records of the next value into the window and returns
    /// their length. Only the frame's own length is used before the record
    /// it belongs to is verified.
    fn load_value(&mut self) -> Result<usize, ValueStoreError> {
        let version = self.file.version();
        let left = self.end - self.at;
        if left < RecordHeader::ENCODED_LEN as u64 {
            return Err(self.truncated(RecordHeader::ENCODED_LEN as u64, left));
        }
        self.fill(RecordHeader::ENCODED_LEN)?;
        let at = self.window_offset();
        let first = record_len(&self.window[at..at + RecordHeader::ENCODED_LEN], version)
            .map_err(self.file.format_error(self.at))?;
        if first as u64 > left {
            return Err(self.truncated(first as u64, left));
        }
        self.fill(first)?;

        // Every record of a value but its last is full, so a first record
        // that is not is the whole value.
        if (first as u64) < extent_len(MAX_PART_LEN as u32, version) {
            return Ok(first);
        }
        let at = self.window_offset();
        let record = &self.window[at..at + first];
        let (verified, _) =
            ValueRecord::decode(record, version).map_err(self.file.format_error(self.at))?;
        let extent = extent_len(verified.value_len(), version);
        if extent > left {
            return Err(self.truncated(extent, left));
        }
        let len = usize::try_from(extent).map_err(|_| ValueStoreError::ValueTooLong { extent })?;
        self.fill(len)?;
        Ok(len)
    }

    fn fill(&mut self, len: usize) -> Result<(), ValueStoreError> {
        if self.window.len() - self.window_offset() >= len {
            return Ok(());
        }
        let left = self.end - self.at;
        let window_len = left.min(WALK_WINDOW_LEN.max(len as u64)) as usize;
        self.window.clear();
        self.window_at = self.at;
        self.window.resize(window_len, 0);
        let read = self.file.read_exact(&mut self.window, self.at);
        if read.is_err() {
            self.window.clear();
        }
        read
    }
}
