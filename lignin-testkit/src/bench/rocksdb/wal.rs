//! Reads what RocksDB appends to its write-ahead log.
//!
//! GroveDB opens RocksDB with statistics off and does not expose the
//! database handle, so the bytes a commit hands to RocksDB are read back
//! from the log files themselves. The format is RocksDB's `log::Writer`:
//! 32 KiB blocks of physical records, each a 7-byte header (checksum,
//! length, type) or an 11-byte one for recycled logs, carrying fragments of
//! one logical record. A logical record is a `WriteBatch`: an 8-byte
//! sequence number, a 4-byte entry count and the entries.
//!
//! [`LogTail`] reads one file incrementally, from where it stopped last
//! time, so the harness can read every commit before RocksDB deletes the
//! file after a flush.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The size of a log block.
pub const BLOCK_SIZE: u64 = 32 * 1024;
const HEADER_SIZE: usize = 7;
const RECYCLABLE_HEADER_SIZE: usize = 11;
const BATCH_HEADER_SIZE: usize = 12;

/// Totals over the write batches read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalTotals {
    /// Bytes of log file consumed: headers, payload and block padding.
    pub file_bytes: u64,
    /// Write batches read.
    pub batches: u64,
    /// Entries per column family id. Id 0 is the default family.
    pub families: BTreeMap<u32, FamilyTotals>,
    /// The sequence number the next write batch must carry: each batch
    /// takes one per entry, so a batch the reader missed shows as a gap.
    pub next_sequence: Option<u64>,
}

impl WalTotals {
    /// Totals of the default column family, where Merk keeps its nodes.
    pub fn default_family(&self) -> FamilyTotals {
        self.families.get(&0).copied().unwrap_or_default()
    }

    /// Totals of every other column family.
    pub fn other_families(&self) -> FamilyTotals {
        self.families
            .iter()
            .filter(|(id, _)| **id != 0)
            .fold(FamilyTotals::default(), |sum, (_, family)| sum.plus(family))
    }
}

/// Entries of one column family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyTotals {
    /// Puts.
    pub puts: u64,
    /// Key and value bytes of the puts.
    pub put_bytes: u64,
    /// Deletes, single deletes, range deletes and merges.
    pub others: u64,
    /// Key and value bytes of those.
    pub other_bytes: u64,
}

impl FamilyTotals {
    fn plus(self, other: &FamilyTotals) -> FamilyTotals {
        FamilyTotals {
            puts: self.puts + other.puts,
            put_bytes: self.put_bytes + other.put_bytes,
            others: self.others + other.others,
            other_bytes: self.other_bytes + other.other_bytes,
        }
    }
}

/// Why a log could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalError {
    /// A physical record of a type this reader does not know.
    RecordType(u8),
    /// Fragments out of order: a middle or last fragment with no first, or
    /// a first or full record while a fragment was pending.
    Fragment(u64),
    /// A write batch entry this reader does not know.
    EntryTag(u8),
    /// A write batch that ends inside an entry, or shorter than its header.
    Truncated,
    /// A write batch whose sequence number is not the one the batch read
    /// before it leads to: a batch was missed.
    Gap {
        /// The sequence number expected.
        expected: u64,
        /// The sequence number found.
        found: u64,
    },
    /// A write batch whose entry count disagrees with its entries.
    Count {
        /// The count in the batch header.
        header: u32,
        /// The entries found.
        found: u32,
    },
}

impl std::fmt::Display for WalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalError::RecordType(kind) => write!(f, "unknown log record type {kind}"),
            WalError::Fragment(offset) => write!(f, "log fragment out of order at {offset}"),
            WalError::EntryTag(tag) => write!(f, "unknown write batch entry tag {tag:#x}"),
            WalError::Truncated => write!(f, "write batch truncated"),
            WalError::Gap { expected, found } => write!(
                f,
                "write batch with sequence {found} where {expected} was next: a batch was missed"
            ),
            WalError::Count { header, found } => write!(
                f,
                "write batch header counts {header} entries, {found} found"
            ),
        }
    }
}

/// Receives each put as its column family, key and value.
type Visit<'a> = dyn FnMut(u32, &[u8], &[u8]) + 'a;

/// How far one log file has been read.
#[derive(Clone, Debug, Default)]
pub struct LogTail {
    offset: u64,
    fragment: Vec<u8>,
    in_fragment: bool,
}

impl LogTail {
    /// A tail that starts reading at `offset`, which must be where a
    /// physical record starts: the end of the file when it is opened.
    pub fn starting_at(offset: u64) -> Self {
        Self {
            offset,
            ..Default::default()
        }
    }

    /// Where the next read starts.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads the complete records in `data`, the file's bytes from
    /// [`LogTail::offset`] on, into `totals`. A record that is not complete
    /// yet, or the zeroes of space not written yet, are left for the next
    /// call. A record is complete once the commit that wrote it has
    /// returned, so the caller reads after commits, never during one.
    pub fn read(&mut self, data: &[u8], totals: &mut WalTotals) -> Result<(), WalError> {
        self.read_visiting(data, totals, &mut |_, _, _| {})
    }

    /// [`LogTail::read`], also handing every put to `visit` as its column
    /// family, key and value.
    pub fn read_visiting(
        &mut self,
        data: &[u8],
        totals: &mut WalTotals,
        visit: &mut Visit,
    ) -> Result<(), WalError> {
        let mut position = 0usize;
        loop {
            let absolute = self.offset + position as u64;
            let left_in_block = (BLOCK_SIZE - absolute % BLOCK_SIZE) as usize;
            if left_in_block < HEADER_SIZE {
                // The writer pads a block's tail when it writes the record
                // that follows, so the tail is passed only once that record
                // is there; the offset never runs ahead of what is written.
                let next = position + left_in_block;
                match data.get(next..next + HEADER_SIZE) {
                    Some(header) if header[6] != 0 => {
                        position = next;
                        continue;
                    }
                    _ => break,
                }
            }
            let Some(header) = data.get(position..position + HEADER_SIZE) else {
                break;
            };
            let length = usize::from(u16::from_le_bytes([header[4], header[5]]));
            let kind = header[6];
            if kind == 0 {
                break;
            }
            let header_size = match kind {
                1..=4 | 10 | 11 => HEADER_SIZE,
                5..=8 | 12 | 13 => RECYCLABLE_HEADER_SIZE,
                _ => return Err(WalError::RecordType(kind)),
            };
            let end = position + header_size + length;
            if end > data.len() {
                break;
            }
            let payload = &data[position + header_size..end];
            match kind {
                1 | 5 => {
                    if self.in_fragment {
                        return Err(WalError::Fragment(absolute));
                    }
                    read_batch(payload, totals, visit)?;
                }
                2 | 6 => {
                    if self.in_fragment {
                        return Err(WalError::Fragment(absolute));
                    }
                    self.in_fragment = true;
                    self.fragment.clear();
                    self.fragment.extend_from_slice(payload);
                }
                3 | 7 | 4 | 8 => {
                    if !self.in_fragment {
                        return Err(WalError::Fragment(absolute));
                    }
                    self.fragment.extend_from_slice(payload);
                    if matches!(kind, 4 | 8) {
                        self.in_fragment = false;
                        let batch = std::mem::take(&mut self.fragment);
                        read_batch(&batch, totals, visit)?;
                    }
                }
                _ => {}
            }
            position = end;
        }
        self.offset += position as u64;
        totals.file_bytes += position as u64;
        Ok(())
    }
}

/// Adds one write batch to `totals`.
fn read_batch(batch: &[u8], totals: &mut WalTotals, visit: &mut Visit) -> Result<(), WalError> {
    if batch.len() < BATCH_HEADER_SIZE {
        return Err(WalError::Truncated);
    }
    let header = u32::from_le_bytes([batch[8], batch[9], batch[10], batch[11]]);
    let mut cursor = Cursor(&batch[BATCH_HEADER_SIZE..]);
    let mut found = 0u32;
    let mut families: BTreeMap<u32, FamilyTotals> = BTreeMap::new();
    while let Some(tag) = cursor.byte() {
        let (family, put, fields) = match tag {
            0x1 => (0, true, 2),
            0x5 => (cursor.varint()?, true, 2),
            0x0 | 0x7 => (0, false, 1),
            0x4 | 0x8 => (cursor.varint()?, false, 1),
            0x2 | 0xF => (0, false, 2),
            0x6 | 0xE => (cursor.varint()?, false, 2),
            0x3 | 0xA | 0xB | 0xC => {
                cursor.slice()?;
                continue;
            }
            0x9 | 0xD | 0x12 | 0x13 => continue,
            _ => return Err(WalError::EntryTag(tag)),
        };
        let key = cursor.slice()?;
        let value = if fields == 2 { cursor.slice()? } else { &[] };
        let bytes = (key.len() + value.len()) as u64;
        let entry = families.entry(family).or_default();
        if put {
            visit(family, key, value);
            entry.puts += 1;
            entry.put_bytes += bytes;
        } else {
            entry.others += 1;
            entry.other_bytes += bytes;
        }
        found += 1;
    }
    if found != header {
        return Err(WalError::Count { header, found });
    }
    let mut sequence = [0u8; 8];
    sequence.copy_from_slice(&batch[..8]);
    let sequence = u64::from_le_bytes(sequence);
    if let Some(expected) = totals
        .next_sequence
        .filter(|expected| *expected != sequence)
    {
        return Err(WalError::Gap {
            expected,
            found: sequence,
        });
    }
    totals.next_sequence = Some(sequence + u64::from(header));
    totals.batches += 1;
    for (id, family) in families {
        let sum = totals.families.entry(id).or_default();
        *sum = sum.plus(&family);
    }
    Ok(())
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn byte(&mut self) -> Option<u8> {
        let (&first, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(first)
    }

    fn varint(&mut self) -> Result<u32, WalError> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.byte().ok_or(WalError::Truncated)?;
            value |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(WalError::Truncated)
    }

    fn slice(&mut self) -> Result<&'a [u8], WalError> {
        let length = self.varint()? as usize;
        if length > self.0.len() {
            return Err(WalError::Truncated);
        }
        let (slice, rest) = self.0.split_at(length);
        self.0 = rest;
        Ok(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut value: u32, out: &mut Vec<u8>) {
        while value >= 0x80 {
            out.push(value as u8 | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn slice(bytes: &[u8], out: &mut Vec<u8>) {
        varint(bytes.len() as u32, out);
        out.extend_from_slice(bytes);
    }

    /// A batch of `puts` default-family puts of a 3-byte key and a value of
    /// `value_len` bytes, a put to family 2 and a delete in family 1.
    fn batch(puts: u32, value_len: usize) -> Vec<u8> {
        let mut out = vec![0u8; 8];
        out.extend_from_slice(&(puts + 2).to_le_bytes());
        for _ in 0..puts {
            out.push(0x1);
            slice(b"key", &mut out);
            slice(&vec![7u8; value_len], &mut out);
        }
        out.push(0x5);
        varint(2, &mut out);
        slice(b"root", &mut out);
        slice(b"v", &mut out);
        out.push(0x4);
        varint(1, &mut out);
        slice(b"gone", &mut out);
        out.push(0x3);
        slice(b"log data is not an entry", &mut out);
        out
    }

    /// Frames `records` the way RocksDB's log writer does, numbering the
    /// batches as RocksDB does from sequence 1.
    fn frame(records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut sequence = 1u64;
        for record in records {
            let mut record = record.clone();
            if record.len() >= BATCH_HEADER_SIZE {
                record[..8].copy_from_slice(&sequence.to_le_bytes());
                sequence += u64::from(u32::from_le_bytes(record[8..12].try_into().unwrap()));
            }
            let mut left = record.as_slice();
            let mut first = true;
            loop {
                let room = BLOCK_SIZE as usize - out.len() % BLOCK_SIZE as usize;
                if room < HEADER_SIZE {
                    out.extend(std::iter::repeat_n(0u8, room));
                    continue;
                }
                let take = left.len().min(room - HEADER_SIZE);
                let last = take == left.len();
                let kind = match (first, last) {
                    (true, true) => 1,
                    (true, false) => 2,
                    (false, false) => 3,
                    (false, true) => 4,
                };
                out.extend_from_slice(&[0xAB; 4]);
                out.extend_from_slice(&(take as u16).to_le_bytes());
                out.push(kind);
                out.extend_from_slice(&left[..take]);
                left = &left[take..];
                first = false;
                if last {
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn whole_and_fragmented_batches_are_counted() {
        let records = vec![batch(2, 10), batch(40, 1000), batch(1, 0)];
        let file = frame(&records);
        let mut totals = WalTotals::default();
        let mut tail = LogTail::default();
        tail.read(&file, &mut totals).unwrap();
        assert_eq!(tail.offset(), file.len() as u64);
        assert_eq!(totals.file_bytes, file.len() as u64);
        assert_eq!(totals.batches, 3);
        assert_eq!(
            totals.default_family(),
            FamilyTotals {
                puts: 43,
                put_bytes: 2 * 13 + 40 * 1003 + 3,
                others: 0,
                other_bytes: 0,
            }
        );
        assert_eq!(
            totals.other_families(),
            FamilyTotals {
                puts: 3,
                put_bytes: 15,
                others: 3,
                other_bytes: 12,
            }
        );
    }

    #[test]
    fn a_file_read_in_pieces_gives_the_same_totals() {
        let records: Vec<_> = (0..30).map(|n| batch(n, 2_000)).collect();
        let file = frame(&records);
        let mut whole = WalTotals::default();
        LogTail::default().read(&file, &mut whole).unwrap();

        let mut pieces = WalTotals::default();
        let mut tail = LogTail::default();
        let mut written = 0usize;
        while written < file.len() {
            written = (written + 4_099).min(file.len());
            tail.read(&file[tail.offset() as usize..written], &mut pieces)
                .unwrap();
        }
        assert_eq!(tail.offset(), file.len() as u64);
        assert_eq!(pieces, whole);
    }

    #[test]
    fn a_missed_batch_is_a_gap() {
        let file = frame(&[batch(2, 10), batch(3, 10), batch(1, 10)]);
        let mut totals = WalTotals::default();
        LogTail::default().read(&file, &mut totals).unwrap();
        assert_eq!(totals.next_sequence, Some(1 + 4 + 5 + 3));

        let first = frame(&[batch(2, 10)]).len();
        let second = frame(&[batch(2, 10), batch(3, 10)]).len();
        let mut skipped = WalTotals::default();
        let mut tail = LogTail::default();
        tail.read(&file[..first], &mut skipped).unwrap();
        let mut later = LogTail::starting_at(second as u64);
        assert_eq!(
            later.read(&file[second..], &mut skipped),
            Err(WalError::Gap {
                expected: 5,
                found: 10
            })
        );
    }

    #[test]
    fn every_put_is_visited() {
        let file = frame(&[batch(2, 10)]);
        let mut puts = Vec::new();
        LogTail::default()
            .read_visiting(
                &file,
                &mut WalTotals::default(),
                &mut |family, key, value| puts.push((family, key.to_vec(), value.len())),
            )
            .unwrap();
        assert_eq!(
            puts,
            vec![
                (0, b"key".to_vec(), 10),
                (0, b"key".to_vec(), 10),
                (2, b"root".to_vec(), 1)
            ]
        );
    }

    #[test]
    fn space_not_written_yet_is_left_for_later() {
        let mut file = frame(&[batch(3, 10)]);
        let end = file.len() as u64;
        file.extend(std::iter::repeat_n(0u8, 1_000));
        let mut totals = WalTotals::default();
        let mut tail = LogTail::default();
        tail.read(&file, &mut totals).unwrap();
        assert_eq!(tail.offset(), end);
        assert_eq!(totals.batches, 1);
    }

    #[test]
    fn a_block_tail_is_skipped_only_once_a_record_follows() {
        let filling = (0..4_000)
            .map(|value_len| batch(1, value_len + 30_000))
            .find(|record| record.len() + HEADER_SIZE == BLOCK_SIZE as usize - 3)
            .expect("a batch that leaves three bytes of its block");
        let one = frame(std::slice::from_ref(&filling));
        assert_eq!(one.len(), BLOCK_SIZE as usize - 3);

        let mut grown = one.clone();
        grown.extend(std::iter::repeat_n(0u8, 4_096));
        let mut totals = WalTotals::default();
        let mut tail = LogTail::default();
        tail.read(&grown, &mut totals).unwrap();
        assert_eq!(tail.offset(), one.len() as u64, "stops before the tail");

        let two = frame(&[filling, batch(2, 10)]);
        assert_eq!(two[one.len()..BLOCK_SIZE as usize], [0, 0, 0]);
        tail.read(&two[tail.offset() as usize..], &mut totals)
            .unwrap();
        assert_eq!(tail.offset(), two.len() as u64);
        assert_eq!(totals.batches, 2);
    }

    #[test]
    fn a_tail_starting_mid_file_reads_from_there() {
        let first = frame(&[batch(5, 100)]);
        let file = frame(&[batch(5, 100), batch(1, 1)]);
        let mut totals = WalTotals::default();
        let mut tail = LogTail::starting_at(first.len() as u64);
        tail.read(&file[first.len()..], &mut totals).unwrap();
        assert_eq!(totals.batches, 1);
        assert_eq!(totals.default_family().puts, 1);
    }

    #[test]
    fn malformed_logs_are_refused() {
        let mut bad_type = frame(&[batch(1, 1)]);
        bad_type[6] = 9;
        assert_eq!(
            LogTail::default().read(&bad_type, &mut WalTotals::default()),
            Err(WalError::RecordType(9))
        );

        let mut orphan = frame(&[batch(1, 1)]);
        orphan[6] = 4;
        assert_eq!(
            LogTail::default().read(&orphan, &mut WalTotals::default()),
            Err(WalError::Fragment(0))
        );

        let mut first = frame(&[batch(1, 1)]);
        first[6] = 2;
        let twice = [first.clone(), first].concat();
        assert!(matches!(
            LogTail::default().read(&twice, &mut WalTotals::default()),
            Err(WalError::Fragment(_))
        ));

        let mut unknown = batch(1, 1);
        unknown.push(0x40);
        assert_eq!(
            LogTail::default().read(&frame(&[unknown]), &mut WalTotals::default()),
            Err(WalError::EntryTag(0x40))
        );

        let mut miscounted = batch(1, 1);
        miscounted[8] = 9;
        assert_eq!(
            LogTail::default().read(&frame(&[miscounted]), &mut WalTotals::default()),
            Err(WalError::Count {
                header: 9,
                found: 3
            })
        );

        let mut cut = batch(1, 1);
        cut.truncate(cut.len() - 3);
        assert_eq!(
            LogTail::default().read(&frame(&[cut]), &mut WalTotals::default()),
            Err(WalError::Truncated)
        );
        assert_eq!(
            LogTail::default().read(&frame(&[vec![0; 5]]), &mut WalTotals::default()),
            Err(WalError::Truncated)
        );
        let mut long_varint = vec![0u8; 12];
        long_varint.push(0x5);
        long_varint.extend_from_slice(&[0xFF; 6]);
        assert_eq!(
            LogTail::default().read(&frame(&[long_varint]), &mut WalTotals::default()),
            Err(WalError::Truncated)
        );
    }

    #[test]
    fn errors_say_what_went_wrong() {
        for (error, text) in [
            (WalError::RecordType(9), "unknown log record type 9"),
            (WalError::Fragment(7), "log fragment out of order at 7"),
            (
                WalError::EntryTag(0x40),
                "unknown write batch entry tag 0x40",
            ),
            (WalError::Truncated, "write batch truncated"),
            (
                WalError::Gap {
                    expected: 5,
                    found: 9,
                },
                "write batch with sequence 9 where 5 was next: a batch was missed",
            ),
            (
                WalError::Count {
                    header: 2,
                    found: 1,
                },
                "write batch header counts 2 entries, 1 found",
            ),
        ] {
            assert_eq!(error.to_string(), text);
        }
    }
}
