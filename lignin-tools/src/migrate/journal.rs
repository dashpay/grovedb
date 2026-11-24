//! The journal of a migration: where its load stands, kept in the target
//! and committed with what it loaded.
//!
//! The journal is one aux record of the target's root subtree, under
//! [`JOURNAL_KEY`]. Every commit of the load writes it with the place the
//! load has reached once that commit is made, so after a stop at any
//! point the target holds the writes of some number of commits and the
//! journal that says exactly which. The commit that ends the load removes
//! it: **a target has a journal exactly while its load is not complete.**
//!
//! It names the source it was made from ([`Journal::identity`],
//! [`Journal::sequence`], [`Journal::root_hash`]): a load goes on only
//! from the source it was begun on, in the state it was begun on.
//!
//! # The record
//!
//! All numbers are little-endian; `bytes` is a length in four bytes and
//! the bytes.
//!
//! ```text
//! magic     "LIGNMIGR" and the format, one byte: 1
//! source    identity (bytes), sequence number (8), root hash (32)
//! phase     1: 0 the grove, 1 the root-key records, 2 the pending drops
//! frames    count (4), then for each subtree that is entered and not left,
//!           from the root Merk down: key (bytes), position (1), and the last
//!           subtree below it that was left: 0, or 1 with key (bytes) and
//!           position (1)
//! roots     0, or 1 and the stored key (bytes) of the last root-key record
//!           that was carried
//! carried   ten counts (8 each), in the order of the fields of `Carried`
//! ```

use lignin_format::tree_page::Hash;

use super::walk::Frame;
use crate::ToolError;

/// The key of the journal, in the aux keyspace of the target's root
/// subtree.
pub(crate) const JOURNAL_KEY: &[u8] = b"lignin_migration_in_progress";

const MAGIC: &[u8; 8] = b"LIGNMIGR";
const FORMAT: u8 = 1;

/// The part of the load that is under way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Phase {
    /// The subtrees that elements lead to: their data, aux and meta
    /// records.
    Grove,
    /// The records of root keys, under every prefix.
    Roots,
    /// The pending-drop records, with the marks of what they left.
    Drops,
}

/// What a load has carried so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Carried {
    /// The subtrees that were entered.
    pub(crate) subtrees: u64,
    /// The Merks that were handed over whole.
    pub(crate) merks: u64,
    /// The nodes of those Merks.
    pub(crate) nodes: u64,
    /// The node records that no tree links.
    pub(crate) unlinked: u64,
    /// The flat records of the data keyspace.
    pub(crate) flat: u64,
    /// The aux records.
    pub(crate) aux: u64,
    /// The meta records, the pending-drop records among them.
    pub(crate) meta: u64,
    /// The records of root keys.
    pub(crate) roots: u64,
    /// The marks of what flat drops left behind.
    pub(crate) marks: u64,
    /// The bytes of the keys and values of all of these.
    pub(crate) bytes: u64,
}

impl Carried {
    fn fields(&self) -> [u64; 10] {
        [
            self.subtrees,
            self.merks,
            self.nodes,
            self.unlinked,
            self.flat,
            self.aux,
            self.meta,
            self.roots,
            self.marks,
            self.bytes,
        ]
    }

    /// Every record that was carried, of every kind.
    pub(crate) fn records(&self) -> u64 {
        self.nodes + self.unlinked + self.flat + self.aux + self.meta + self.roots + self.marks
    }
}

/// Where a load stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Journal {
    /// RocksDB's identity of the source.
    pub(crate) identity: Vec<u8>,
    /// The sequence number of the source's last write.
    pub(crate) sequence: u64,
    /// The root hash of the source's grove.
    pub(crate) root_hash: Hash,
    /// The part that is under way.
    pub(crate) phase: Phase,
    /// The place of the walk of the grove.
    pub(crate) frames: Vec<Frame>,
    /// The stored key of the last root-key record that was carried.
    pub(crate) roots: Option<Vec<u8>>,
    /// What was carried so far.
    pub(crate) carried: Carried,
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(taken)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn byte(&mut self) -> Option<u8> {
        self.array::<1>().map(|[byte]| byte)
    }

    fn number(&mut self) -> Option<u64> {
        self.array().map(u64::from_le_bytes)
    }

    fn bytes(&mut self) -> Option<Vec<u8>> {
        let len = u32::from_le_bytes(self.array()?);
        self.take(len as usize).map(<[u8]>::to_vec)
    }

    fn flag(&mut self) -> Option<bool> {
        match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

impl Journal {
    /// The bytes of the record.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.push(FORMAT);
        put_bytes(&mut out, &self.identity);
        out.extend_from_slice(&self.sequence.to_le_bytes());
        out.extend_from_slice(&self.root_hash);
        out.push(self.phase as u8);
        out.extend_from_slice(&(self.frames.len() as u32).to_le_bytes());
        for frame in &self.frames {
            put_bytes(&mut out, &frame.key);
            out.push(frame.position);
            match &frame.done {
                None => out.push(0),
                Some((key, position)) => {
                    out.push(1);
                    put_bytes(&mut out, key);
                    out.push(*position);
                }
            }
        }
        match &self.roots {
            None => out.push(0),
            Some(key) => {
                out.push(1);
                put_bytes(&mut out, key);
            }
        }
        for count in self.carried.fields() {
            out.extend_from_slice(&count.to_le_bytes());
        }
        out
    }

    fn read(record: &mut In<'_>) -> Option<Self> {
        let identity = record.bytes()?;
        let sequence = record.number()?;
        let root_hash = record.array()?;
        let phase = match record.byte()? {
            0 => Phase::Grove,
            1 => Phase::Roots,
            2 => Phase::Drops,
            _ => return None,
        };
        let count = u32::from_le_bytes(record.array()?);
        let mut frames = Vec::new();
        for _ in 0..count {
            let (key, position) = (record.bytes()?, record.byte()?);
            let done = match record.flag()? {
                false => None,
                true => Some((record.bytes()?, record.byte()?)),
            };
            frames.push(Frame {
                key,
                position,
                done,
            });
        }
        let roots = match record.flag()? {
            false => None,
            true => Some(record.bytes()?),
        };
        let mut counts = [0; 10];
        for count in &mut counts {
            *count = record.number()?;
        }
        let [subtrees, merks, nodes, unlinked, flat, aux, meta, roots_carried, marks, bytes] =
            counts;
        Some(Self {
            identity,
            sequence,
            root_hash,
            phase,
            frames,
            roots,
            carried: Carried {
                subtrees,
                merks,
                nodes,
                unlinked,
                flat,
                aux,
                meta,
                roots: roots_carried,
                marks,
                bytes,
            },
        })
    }

    /// The journal that `record` is the bytes of.
    pub(crate) fn decode(record: &[u8]) -> Result<Self, ToolError> {
        let refused = |what: &str| {
            ToolError::Migrate(format!(
                "the journal of the target {what}: the directory was not made by this build of \
                 the command, or is damaged. Remove it and migrate anew"
            ))
        };
        let mut record = In(record);
        if record.array::<8>() != Some(*MAGIC) {
            return Err(refused("is no journal of a migration"));
        }
        match record.byte() {
            Some(FORMAT) => {}
            Some(other) => {
                return Err(refused(&format!(
                    "has the format {other}, and this build reads {FORMAT}"
                )));
            }
            None => return Err(refused("is cut short")),
        }
        let journal = Self::read(&mut record).ok_or_else(|| refused("is cut short"))?;
        match record.0.is_empty() {
            true => Ok(journal),
            false => Err(refused("has bytes after its end")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal() -> Journal {
        Journal {
            identity: b"an identity".to_vec(),
            sequence: 0x0102_0304_0506_0708,
            root_hash: [7; 32],
            phase: Phase::Roots,
            frames: vec![
                Frame {
                    key: Vec::new(),
                    position: 0,
                    done: Some((b"contracts".to_vec(), 0)),
                },
                Frame {
                    key: b"identities".to_vec(),
                    position: 2,
                    done: None,
                },
            ],
            roots: Some(vec![9; 33]),
            carried: Carried {
                subtrees: 1,
                merks: 2,
                nodes: 3,
                unlinked: 4,
                flat: 5,
                aux: 6,
                meta: 7,
                roots: 8,
                marks: 9,
                bytes: 10,
            },
        }
    }

    #[test]
    fn a_journal_is_read_back_as_it_was_written() {
        let journal = journal();
        assert_eq!(Journal::decode(&journal.encode()).unwrap(), journal);
        assert_eq!(journal.carried.records(), 3 + 4 + 5 + 6 + 7 + 8 + 9);
        for phase in [Phase::Grove, Phase::Drops] {
            let other = Journal {
                phase,
                frames: Vec::new(),
                roots: None,
                ..journal.clone()
            };
            assert_eq!(Journal::decode(&other.encode()).unwrap(), other);
        }
    }

    #[test]
    fn bytes_that_are_no_journal_are_refused() {
        let message = |record: &[u8]| match Journal::decode(record) {
            Err(ToolError::Migrate(message)) => message,
            other => panic!("{other:?}"),
        };
        let record = journal().encode();
        assert!(message(b"some other record").contains("is no journal of a migration"));
        assert!(message(&record[..4]).contains("is no journal of a migration"));
        for cut in 8..record.len() {
            assert!(message(&record[..cut]).contains("is cut short"), "{cut}");
        }
        let mut longer = record.clone();
        longer.push(0);
        assert!(message(&longer).contains("has bytes after its end"));
        let mut other_format = record.clone();
        other_format[8] = 2;
        assert!(message(&other_format).contains("has the format 2"));
        // A phase and a flag that are none.
        let phase_at = 9 + 4 + b"an identity".len() + 8 + 32;
        let mut no_phase = record.clone();
        no_phase[phase_at] = 3;
        assert!(message(&no_phase).contains("is cut short"));
        let mut no_flag = record;
        let flag_at = phase_at + 1 + 4 + 4 + 1;
        assert_eq!(
            no_flag[flag_at], 1,
            "the flag of the root frame's last subtree"
        );
        no_flag[flag_at] = 2;
        assert!(message(&no_flag).contains("is cut short"));
    }
}
