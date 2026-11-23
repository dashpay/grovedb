//! The stream that a subtree is dumped to and loaded from.
//!
//! It names no engine: what it holds is the records of one subtree as the
//! seams show them (`docs/lignindb/engine.md`, "Dump and load"). All
//! integers are big-endian.
//!
//! ```text
//! magic            8 bytes   "GRVSUBT\0"
//! format version   u32       1
//! element length   u32
//! element          the element that hosts the subtree, as GroveDB serializes it
//! root             32 bytes  the root that the subtree's records give (see `subtree_root_as`)
//! records          each: keyspace u8 (1 data, 2 aux, 3 roots, 4 meta),
//!                        key length u32, key, value length u32, value
//! end              u8        0
//! record count     u64
//! checksum         32 bytes  Blake3 of every byte before it
//! ```
//!
//! The records stand in the order of their keyspaces and, inside one, in
//! the ascending order of their keys, each key once. The roots keyspace
//! holds at most one record, the recorded key of the root node of a Merk
//! that keeps one, under the empty key.

use std::io::{self, Read, Write};

use super::DumpLoadError;

pub(super) const MAGIC: [u8; 8] = *b"GRVSUBT\0";
pub(super) const FORMAT_VERSION: u32 = 1;
const END: u8 = 0;

/// The keyspaces of a subtree, in the order a dump holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Keyspace {
    Data = 1,
    Aux = 2,
    Roots = 3,
    Meta = 4,
}

impl Keyspace {
    fn of(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Data),
            2 => Some(Self::Aux),
            3 => Some(Self::Roots),
            4 => Some(Self::Meta),
            _ => None,
        }
    }
}

/// How many records of each keyspace a stream holds, and its length.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Counts {
    pub(super) records: [u64; 4],
    pub(super) bytes: u64,
}

impl Counts {
    fn all(&self) -> u64 {
        self.records.iter().sum()
    }
}

fn length(of: &[u8]) -> Result<[u8; 4], DumpLoadError> {
    u32::try_from(of.len())
        .map(u32::to_be_bytes)
        .map_err(|_| DumpLoadError::Malformed("a key or a value is longer than the format holds"))
}

/// Writes one dump to `out`.
pub(super) struct DumpWriter<W: Write> {
    out: W,
    hasher: blake3::Hasher,
    counts: Counts,
}

impl<W: Write> DumpWriter<W> {
    /// Writes the header.
    pub(super) fn new(out: W, element: &[u8], root: &[u8; 32]) -> Result<Self, DumpLoadError> {
        let mut writer = Self {
            out,
            hasher: blake3::Hasher::new(),
            counts: Counts::default(),
        };
        writer.put(&MAGIC)?;
        writer.put(&FORMAT_VERSION.to_be_bytes())?;
        writer.put(&length(element)?)?;
        writer.put(element)?;
        writer.put(root)?;
        Ok(writer)
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.hasher.update(bytes);
        self.counts.bytes += bytes.len() as u64;
        self.out.write_all(bytes)
    }

    /// Writes one record. The caller keeps the order of the format.
    pub(super) fn record(
        &mut self,
        keyspace: Keyspace,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), DumpLoadError> {
        let (key_length, value_length) = (length(key)?, length(value)?);
        self.put(&[keyspace as u8])?;
        self.put(&key_length)?;
        self.put(key)?;
        self.put(&value_length)?;
        self.put(value)?;
        self.counts.records[keyspace as usize - 1] += 1;
        Ok(())
    }

    /// Writes the end of the dump and flushes `out`.
    pub(super) fn finish(mut self) -> Result<Counts, DumpLoadError> {
        self.put(&[END])?;
        let all = self.counts.all();
        self.put(&all.to_be_bytes())?;
        let checksum = *self.hasher.finalize().as_bytes();
        self.out.write_all(&checksum)?;
        self.counts.bytes += checksum.len() as u64;
        self.out.flush()?;
        Ok(self.counts)
    }
}

/// Reads one dump from `input`, and no byte after it.
pub(super) struct DumpReader<R: Read> {
    input: R,
    hasher: blake3::Hasher,
    counts: Counts,
    last: Option<(Keyspace, Vec<u8>)>,
}

impl<R: Read> DumpReader<R> {
    /// Reads the header, and gives the bytes of the element and the root.
    pub(super) fn open(input: R) -> Result<(Self, Vec<u8>, [u8; 32]), DumpLoadError> {
        let mut reader = Self {
            input,
            hasher: blake3::Hasher::new(),
            counts: Counts::default(),
            last: None,
        };
        if reader.fixed::<8>()? != MAGIC {
            return Err(DumpLoadError::Malformed("not the dump of a subtree"));
        }
        if u32::from_be_bytes(reader.fixed()?) != FORMAT_VERSION {
            return Err(DumpLoadError::Malformed(
                "a dump of another version of the format",
            ));
        }
        let element = reader.block()?;
        let root = reader.fixed()?;
        Ok((reader, element, root))
    }

    fn fixed<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut bytes = [0u8; N];
        self.input.read_exact(&mut bytes)?;
        self.hasher.update(&bytes);
        self.counts.bytes += N as u64;
        Ok(bytes)
    }

    /// A length and that many bytes. Memory is taken for the bytes that
    /// are there, not for the length the stream states.
    fn block(&mut self) -> io::Result<Vec<u8>> {
        let length = u64::from(u32::from_be_bytes(self.fixed()?));
        let mut bytes = Vec::new();
        (&mut self.input).take(length).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != length {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.hasher.update(&bytes);
        self.counts.bytes += length;
        Ok(bytes)
    }

    /// The next record, or `None` at the end of a dump that is whole: its
    /// count and its checksum are what the stream gave.
    pub(super) fn next(&mut self) -> Result<Option<(Keyspace, Vec<u8>, Vec<u8>)>, DumpLoadError> {
        let [tag] = self.fixed()?;
        if tag == END {
            if u64::from_be_bytes(self.fixed()?) != self.counts.all() {
                return Err(DumpLoadError::Malformed(
                    "the dump holds another number of records than it states",
                ));
            }
            let expected = *self.hasher.finalize().as_bytes();
            let mut stated = [0u8; 32];
            self.input.read_exact(&mut stated)?;
            self.counts.bytes += stated.len() as u64;
            if stated != expected {
                return Err(DumpLoadError::Malformed(
                    "the checksum of the dump is not the one of its bytes",
                ));
            }
            return Ok(None);
        }
        let Some(keyspace) = Keyspace::of(tag) else {
            return Err(DumpLoadError::Malformed("a record of no keyspace"));
        };
        let key = self.block()?;
        let value = self.block()?;
        if keyspace == Keyspace::Roots && !key.is_empty() {
            return Err(DumpLoadError::Malformed(
                "a record of the roots keyspace under a key",
            ));
        }
        let place = (keyspace, key);
        if self.last.as_ref().is_some_and(|last| *last >= place) {
            return Err(DumpLoadError::Malformed(
                "the records are not in the order of the format",
            ));
        }
        self.counts.records[keyspace as usize - 1] += 1;
        let key = place.1.clone();
        self.last = Some(place);
        Ok(Some((keyspace, key, value)))
    }

    /// What was read.
    pub(super) fn counts(&self) -> Counts {
        self.counts
    }
}
