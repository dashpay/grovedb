//! A record under a key of a Merk's subtree that no node of the Merk's
//! tree is under: an unlinked record.
//!
//! An engine that keeps one record for each key holds a node whether or
//! not a link of its tree leads to it, and the released versions of
//! GroveDB leave such records behind: a storage batch keeps a put over a
//! later delete of the same key inside one operation, so a node that one
//! commit of a Merk rewrote and the next removed stays in storage, in no
//! tree. It is read by its key and by a cursor, billed and removed by its
//! key, and is in no hash. A node of a batch that the tree does not reach
//! once the batch is applied is such a record
//! ([`MerkWrite`](super::MerkWrite), "Unlinked records").
//!
//! The record is no cell of a tree page. It is a flat record in the data
//! column of its subtree, under the subtree's 32 bytes and the node's key,
//! so it follows the subtree's entry in the directory as every flat record
//! does (`tree-layout.md` D6): a drop and a clear take it, and an enter
//! starts without it. Its value is the node as its writer stated it:
//!
//! ```text
//! 1    the form, 1
//! 1    the links the node has: bit 0 the left, bit 1 the right
//! 1    the length of the feature
//! ..   the feature (`tree-page.md` TP12)
//! 32   the value hash
//! 32   the key-value hash
//! for each link, the left first:
//! 1    the length of the child's key
//! ..   the child's key
//! 32   the child's hash
//! 2    the heights of the child's left and right subtree
//! 1    the length of the aggregate
//! ..   the aggregate (TP12)
//! then, to the end of the record:
//! ..   the value
//! ```
//!
//! A link names its child by key, as the record of the engine that left it
//! does: the child need not be there. Nothing reads the hashes as hashes of
//! anything, and no hash is made of the record.
//!
//! **One record for a key.** A key of a Merk has a node of the tree, or an
//! unlinked record, or neither. The write that links a node under a key
//! takes the unlinked record under it, the write that unlinks a node takes
//! its cell, and a delete of the key takes whichever is there.
//!
//! **It is no node.** It has no handle, and no read of it is dated: no
//! write keeps the value of a committed unlinked record. A record that
//! the transaction itself left is its own node as it last wrote it, and a
//! later write of that node keeps its value
//! ([`MerkWrite`](super::MerkWrite), "Unlinked records").
//!
//! **A Merk with no unlinked record pays a lookup for them.** Whether the
//! data column holds a record under a subtree is one lookup, which reads
//! no record. Who keeps what it found:
//!
//! - The frames of a transaction, while they stand: the writes that are
//!   applied to them keep it right or leave it not known, and what is not
//!   known is looked up when it is next asked.
//! - The transaction, for the committed version it reads, while it reads
//!   that version. A cursor that pins another version looks when it is
//!   opened.
//! - A reader of a snapshot, with what it keeps of the subtree
//!   (`snapshot::Kept`).
//!
//! A read that finds no node, a cursor that is opened, and a write that
//! makes a node, deletes a key or leaves a node out of its tree are what
//! ask. Where the answer is none, a read that finds no node reads nothing
//! more, and a write that leaves none changes no flat page.

use std::cmp::Ordering;

use lignin_format::{
    tree_page::{CellField, Hash, Side, TreeCell, TreeLink, TreePageError},
    value_log::MAX_VALUE_LEN,
    FormatError,
};
use lignin_tree::flat::{FlatError, FlatPages, FlatTree};

use super::{
    error::{BatchError, TxnError},
    frames::stored_key,
    key::Subtree,
    write::{LinkPut, NodePut},
};

/// The first byte of an unlinked record.
const FORM: u8 = 1;

const LEFT: u8 = 1;
const RIGHT: u8 = 2;

const HASH_LEN: usize = 32;

/// A link of an unlinked record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UnlinkedLink<'a> {
    pub(crate) key: &'a [u8],
    pub(crate) hash: &'a Hash,
    pub(crate) heights: (u8, u8),
    pub(crate) aggregate: &'a [u8],
}

/// An unlinked record as it is read: its key, and the parts its bytes hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unlinked<'a> {
    pub(crate) key: &'a [u8],
    pub(crate) feature: &'a [u8],
    pub(crate) value_hash: &'a Hash,
    pub(crate) kv_hash: &'a Hash,
    pub(crate) left: Option<UnlinkedLink<'a>>,
    pub(crate) right: Option<UnlinkedLink<'a>>,
    pub(crate) value: &'a [u8],
}

impl<'a> Unlinked<'a> {
    pub(crate) const fn link(&self, side: Side) -> Option<UnlinkedLink<'a>> {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
    }
}

/// What is wrong with the bytes of a flat record that is read as an
/// unlinked record.
const NOT_ONE: TxnError =
    TxnError::Damaged("a flat record among the nodes of a Merk is no unlinked record");

/// The length of a part that a record has one byte for the length of, as
/// a cell has: a part that no cell takes is refused as a cell refuses it.
fn short(field: CellField, bytes: &[u8], max: usize) -> Result<u8, TxnError> {
    let len = bytes.len();
    let short = u8::try_from(len).ok().filter(|_| len <= max);
    short.ok_or_else(|| {
        let long = TreePageError::FieldTooLong { field, len, max };
        TxnError::Format(FormatError::TreePage(long))
    })
}

/// Checks that `node` can be kept as an unlinked record: each part has a
/// length that a cell would take.
pub(crate) fn check(node: &NodePut<'_>) -> Result<(), TxnError> {
    short(CellField::Key, node.key, TreeCell::MAX_KEY_LEN)?;
    short(CellField::Feature, node.feature, TreeCell::MAX_FEATURE_LEN)?;
    for link in [&node.left, &node.right].into_iter().flatten() {
        short(CellField::Key, link.key, TreeCell::MAX_KEY_LEN)?;
        short(
            CellField::Aggregate,
            link.aggregate,
            TreeLink::MAX_AGGREGATE_LEN,
        )?;
    }
    Ok(())
}

/// Checks that the unlinked record of `node` with a value of `value_len`
/// bytes is no longer than the value of a flat record may be: the value
/// shares that length with the node's other parts.
pub(crate) fn room(node: &NodePut<'_>, value_len: usize) -> Result<(), TxnError> {
    let link = |link: &Option<LinkPut<'_>>| {
        (link.as_ref()).map_or(0, |link| {
            1 + link.key.len() + HASH_LEN + 2 + 1 + link.aggregate.len()
        })
    };
    let parts = 3 + node.feature.len() + 2 * HASH_LEN + link(&node.left) + link(&node.right);
    match parts.checked_add(value_len) {
        Some(len) if len <= MAX_VALUE_LEN => Ok(()),
        _ => Err(BatchError::ValueTooLong { len: value_len }.into()),
    }
}

fn put_link(out: &mut Vec<u8>, link: &LinkPut<'_>) -> Result<(), TxnError> {
    out.push(short(CellField::Key, link.key, TreeCell::MAX_KEY_LEN)?);
    out.extend_from_slice(link.key);
    out.extend_from_slice(link.hash);
    out.extend_from_slice(&[link.heights.0, link.heights.1]);
    out.push(short(
        CellField::Aggregate,
        link.aggregate,
        TreeLink::MAX_AGGREGATE_LEN,
    )?);
    out.extend_from_slice(link.aggregate);
    Ok(())
}

/// Appends to `out` the bytes of the unlinked record of `node`, whose
/// value is `value`.
pub(crate) fn encode(out: &mut Vec<u8>, node: &NodePut<'_>, value: &[u8]) -> Result<(), TxnError> {
    out.push(FORM);
    let links = [(LEFT, &node.left), (RIGHT, &node.right)];
    let has = |(bit, link): &(u8, &Option<LinkPut<'_>>)| link.map_or(0, |_| *bit);
    out.push(links.iter().map(has).sum());
    out.push(short(
        CellField::Feature,
        node.feature,
        TreeCell::MAX_FEATURE_LEN,
    )?);
    out.extend_from_slice(node.feature);
    out.extend_from_slice(node.value_hash);
    out.extend_from_slice(node.kv_hash);
    for (_, link) in links {
        if let Some(link) = link {
            put_link(out, link)?;
        }
    }
    out.extend_from_slice(value);
    Ok(())
}

/// Reads bytes from the front of a record.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], TxnError> {
        let (taken, rest) = self.0.split_at_checked(len).ok_or(NOT_ONE)?;
        self.0 = rest;
        Ok(taken)
    }

    fn byte(&mut self) -> Result<u8, TxnError> {
        Ok(self.take(1)?[0])
    }

    /// A byte string that has its length in one byte before it.
    fn string(&mut self) -> Result<&'a [u8], TxnError> {
        let len = self.byte()?;
        self.take(usize::from(len))
    }

    fn hash(&mut self) -> Result<&'a Hash, TxnError> {
        self.take(HASH_LEN)?.try_into().map_err(|_| NOT_ONE)
    }

    fn link(&mut self) -> Result<UnlinkedLink<'a>, TxnError> {
        let key = self.string()?;
        let hash = self.hash()?;
        let heights = (self.byte()?, self.byte()?);
        let aggregate = self.string()?;
        Ok(UnlinkedLink {
            key,
            hash,
            heights,
            aggregate,
        })
    }
}

/// Reads the unlinked record under `key` from its bytes. Bytes that are
/// not of its form are refused, and so is a key that is longer than a
/// node's.
pub(crate) fn decode<'a>(key: &'a [u8], bytes: &'a [u8]) -> Result<Unlinked<'a>, TxnError> {
    let mut read = Reader(bytes);
    let (form, links) = (read.byte()?, read.byte()?);
    if form != FORM || links & !(LEFT | RIGHT) != 0 || key.len() > TreeCell::MAX_KEY_LEN {
        return Err(NOT_ONE);
    }
    let feature = read.string()?;
    let (value_hash, kv_hash) = (read.hash()?, read.hash()?);
    let mut link = |bit: u8| match links & bit {
        0 => Ok(None),
        _ => read.link().map(Some),
    };
    let (left, right) = (link(LEFT)?, link(RIGHT)?);
    Ok(Unlinked {
        key,
        feature,
        value_hash,
        kv_hash,
        left,
        right,
        value: read.0,
    })
}

/// Which of two cursors over one subtree stand on the nearer key: the one
/// over the nodes of the tree, which stands on `node`, and the one over the
/// unlinked records, which stands on `record`. Both were last moved
/// backwards if `back` says so, and each then stands at its last key at or
/// before the place, and otherwise at its first key at or after it.
pub(crate) fn nearer(node: Option<&[u8]>, record: Option<&[u8]>, back: bool) -> (bool, bool) {
    match (node, record) {
        (None, None) => (false, false),
        (Some(_), None) => (true, false),
        (None, Some(_)) => (false, true),
        (Some(node), Some(record)) => {
            let nearer = match back {
                false => node.cmp(record),
                true => record.cmp(node),
            };
            (nearer != Ordering::Greater, nearer != Ordering::Less)
        }
    }
}

/// Whether the data column `data` holds a record under `subtree` in
/// `pages`: one lookup, which reads no record.
pub(crate) fn any<P: FlatPages + ?Sized>(
    data: &FlatTree,
    pages: &P,
    subtree: &Subtree,
) -> Result<bool, FlatError> {
    #[cfg(all(test, not(loom)))]
    probe::count();
    let mut cursor = data.cursor_within(subtree.as_bytes());
    cursor.seek_to_first(pages)?;
    Ok(cursor.valid())
}

/// What the tests count of the lookups of [`any`] and of the reads of
/// [`get`], on their own thread.
#[cfg(all(test, not(loom)))]
pub(crate) mod probe {
    use std::cell::Cell;

    thread_local! {
        static LOOKUPS: Cell<u64> = const { Cell::new(0) };
        static READS: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) fn count() {
        LOOKUPS.set(LOOKUPS.get() + 1);
    }

    pub(super) fn count_read() {
        READS.set(READS.get() + 1);
    }

    /// How many times this thread has read the flat keyspace for an
    /// unlinked record under a key.
    pub(crate) fn reads() -> u64 {
        READS.get()
    }

    /// How many times this thread has looked up whether a subtree holds
    /// unlinked records.
    pub(crate) fn lookups() -> u64 {
        LOOKUPS.get()
    }
}

/// Appends the bytes of the unlinked record under `key` of `subtree` to
/// `out`, and says whether `pages` hold one.
pub(crate) fn get<P: FlatPages + ?Sized>(
    data: &FlatTree,
    pages: &P,
    subtree: &Subtree,
    key: &[u8],
    out: &mut Vec<u8>,
) -> Result<bool, FlatError> {
    #[cfg(all(test, not(loom)))]
    probe::count_read();
    let mut stored = Vec::with_capacity(Subtree::LEN + key.len());
    stored_key(&mut stored, subtree, key);
    data.get(pages, &stored, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txn::write::{Hosted, ValuePut};

    fn node<'a>(left: Option<LinkPut<'a>>, right: Option<LinkPut<'a>>) -> NodePut<'a> {
        NodePut {
            key: b"key",
            feature: &[3, 1, 2],
            value_hash: &[7; 32],
            kv_hash: &[8; 32],
            left,
            right,
            hosted: Hosted::Keep,
            value: ValuePut::Keep,
            stale: lignin_tree::hash::Stale::NONE,
            at: None,
        }
    }

    const LINK: LinkPut<'static> = LinkPut {
        key: b"child",
        hash: &[9; 32],
        heights: (3, 4),
        aggregate: &[1, 0, 0, 0, 0, 0, 0, 0, 5],
    };

    /// The room a record has for its value is what a flat record's value
    /// may be long, less the parts that `encode` puts before the value.
    #[test]
    fn a_record_has_room_for_a_value_beside_its_parts() {
        for (left, right) in [(None, None), (Some(LINK), None), (Some(LINK), Some(LINK))] {
            let node = node(left, right);
            let mut parts = Vec::new();
            encode(&mut parts, &node, b"").expect("encoded");
            let most = MAX_VALUE_LEN - parts.len();
            room(&node, 0).expect("no value");
            room(&node, most).expect("the longest value");
            for len in [most + 1, MAX_VALUE_LEN, usize::MAX] {
                let refused = room(&node, len).expect_err("no room");
                assert!(
                    matches!(refused, TxnError::Batch(BatchError::ValueTooLong { len: at }) if at == len),
                    "{refused}"
                );
            }
        }
    }

    /// A record gives back every part it was made of, with each side's
    /// link on its side, and every record that ends early or begins with
    /// another form is refused.
    #[test]
    fn a_record_gives_back_what_it_was_made_of() {
        let other = LinkPut {
            key: b"",
            aggregate: &[],
            ..LINK
        };
        let sides = [
            (None, None),
            (Some(LINK), None),
            (None, Some(other)),
            (Some(other), Some(LINK)),
        ];
        for (left, right) in sides {
            for value in [&b""[..], b"v", &[5; 700]] {
                let node = node(left, right);
                check(&node).expect("a node a cell would take");
                let mut bytes = Vec::new();
                encode(&mut bytes, &node, value).expect("encoded");
                let read = decode(b"key", &bytes).expect("decoded");
                assert_eq!(
                    (read.key, read.feature, read.value),
                    (&b"key"[..], node.feature, value)
                );
                assert_eq!((read.value_hash, read.kv_hash), (&[7; 32], &[8; 32]));
                for (side, link) in [(Side::Left, left), (Side::Right, right)] {
                    let found = read.link(side).map(|link| LinkPut {
                        key: link.key,
                        hash: link.hash,
                        heights: link.heights,
                        aggregate: link.aggregate,
                    });
                    assert_eq!(found, link);
                }
                // The value is whatever follows, so only a cut before it
                // is seen.
                let fixed = bytes.len() - value.len();
                for cut in 0..fixed {
                    assert!(decode(b"key", &bytes[..cut]).is_err(), "cut at {cut}");
                }
                let mut wrong = bytes.clone();
                wrong[0] = 2;
                assert!(decode(b"key", &wrong).is_err());
                wrong[0] = FORM;
                wrong[1] |= 4;
                assert!(decode(b"key", &wrong).is_err());
            }
        }
    }

    #[test]
    fn a_part_that_is_too_long_for_a_cell_is_refused() {
        let long = [0; 256];
        let mut bytes = Vec::new();
        let feature = NodePut {
            feature: &long,
            ..node(None, None)
        };
        assert!(check(&feature).is_err());
        assert!(encode(&mut bytes, &feature, b"").is_err());
        let aggregate = LinkPut {
            aggregate: &long,
            ..LINK
        };
        assert!(check(&node(None, Some(aggregate))).is_err());
        assert!(encode(&mut bytes, &node(Some(aggregate), None), b"").is_err());
        let key = LinkPut { key: &long, ..LINK };
        assert!(check(&node(Some(key), None)).is_err());
        assert!(encode(&mut bytes, &node(None, Some(key)), b"").is_err());
        let own = NodePut {
            key: &long,
            ..node(None, None)
        };
        assert!(check(&own).is_err());
        let error = check(&own).expect_err("refused").to_string();
        assert!(error.contains("key of 256 bytes"), "{error}");
    }
}
