//! The records of the subtree directory: their keys and their bytes.
//!
//! They are for whoever reads the records without the directory's own
//! calls: the invariant checker, the tools, and a reclaimer that takes
//! left-over records out in page steps. The
//! [module documentation](super) says what each record means.
//!
//! Every integer is little-endian and of a fixed width, and every record
//! has one encoding (`format.md` B1, B8). A decoder never panics (B10).

use std::{fmt, num::NonZeroU64};

use lignin_format::CellAddress;

use super::error::{DirectoryError, Malformed};

/// The length of a subtree's prefix.
pub const PREFIX_LEN: usize = 32;

/// What names a subtree: the 32-byte hash of its path, or a prefix derived
/// from one (`transactions.md` ST1). The directory takes it as it is given.
pub type Prefix = [u8; PREFIX_LEN];

/// Names one Merk for as long as the nodes it holds are in use: from the
/// call that enters the Merk, or that clears it, to the one that drops or
/// clears it.
///
/// A 64-bit number from 1 up, given out in ascending order and never
/// twice along the history that survives. A Merk keeps its id when it is
/// moved.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MerkId(NonZeroU64);

impl MerkId {
    /// Bytes the id takes in a record.
    pub const ENCODED_LEN: usize = 8;
    /// The first id, `1`.
    pub const FIRST: Self = Self(NonZeroU64::MIN);

    /// The id with this number, or `None` for zero, which is not an id.
    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    /// The id's number.
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// The bytes of the id in a record.
    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        self.get().to_le_bytes()
    }

    /// Reads an id. Zero is none.
    pub const fn decode(bytes: [u8; Self::ENCODED_LEN]) -> Option<Self> {
        Self::new(u64::from_le_bytes(bytes))
    }
}

impl fmt::Debug for MerkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "merk#{}", self.get())
    }
}

impl fmt::Display for MerkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.get(), f)
    }
}

/// What a record of the directory is. The code is the first byte of the
/// record's key, so the records of one kind are one range of the keyspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RecordKind {
    /// The one record that holds the next id to give out. Its key is the
    /// code alone.
    Header = 1,
    /// The entry of a Merk, under the Merk's prefix.
    Entry = 2,
    /// A Merk as its host has it: under the id of the Merk the host is in,
    /// the position among the host's Merks and the host's key.
    Child = 3,
    /// The Merk that has its root in a cell, under the cell's address.
    Root = 4,
    /// What a dropped Merk left, under its id.
    Dropped = 5,
}

impl RecordKind {
    /// Every kind.
    pub const ALL: [Self; 5] = [
        Self::Header,
        Self::Entry,
        Self::Child,
        Self::Root,
        Self::Dropped,
    ];

    /// The first byte of the keys of this kind.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// The kind of the record under `key`.
    pub fn of(key: &[u8]) -> Option<Self> {
        let code = *key.first()?;
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// The key of the header.
pub const HEADER_KEY: [u8; 1] = [RecordKind::Header.code()];

/// The key of the entry of the Merk under `prefix`.
pub fn entry_key(prefix: &Prefix) -> [u8; 1 + PREFIX_LEN] {
    let mut key = [RecordKind::Entry.code(); 1 + PREFIX_LEN];
    key[1..].copy_from_slice(prefix);
    key
}

/// What the keys of the child records of the Merk `parent` begin with.
pub fn children_key(parent: MerkId) -> [u8; 1 + MerkId::ENCODED_LEN] {
    let mut key = [RecordKind::Child.code(); 1 + MerkId::ENCODED_LEN];
    key[1..].copy_from_slice(&parent.encode());
    key
}

/// The key of the child record of the Merk that the node `key` of the Merk
/// `parent` hosts at `position`.
pub fn child_key(parent: MerkId, position: u8, key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + MerkId::ENCODED_LEN + key.len());
    out.extend_from_slice(&children_key(parent));
    out.push(position);
    out.extend_from_slice(key);
    out
}

/// The key of the record of the Merk whose root is the cell at `address`.
pub fn root_key(address: CellAddress) -> [u8; 1 + CellAddress::ENCODED_LEN] {
    let mut key = [RecordKind::Root.code(); 1 + CellAddress::ENCODED_LEN];
    key[1..].copy_from_slice(&address.encode());
    key
}

/// The key of the record of what the dropped Merk `id` left.
pub fn dropped_key(id: MerkId) -> [u8; 1 + MerkId::ENCODED_LEN] {
    let mut key = [RecordKind::Dropped.code(); 1 + MerkId::ENCODED_LEN];
    key[1..].copy_from_slice(&id.encode());
    key
}

/// The header's bytes: the next id to give out.
pub const fn encode_header(next: MerkId) -> [u8; MerkId::ENCODED_LEN] {
    next.encode()
}

/// Reads the header: the next id to give out.
pub fn decode_header(bytes: &[u8]) -> Result<MerkId, Malformed> {
    let bytes = bytes.try_into().ok();
    bytes
        .and_then(MerkId::decode)
        .ok_or(Malformed("a header that is not an id"))
}

/// The bytes of a child record: the prefix and the id of the hosted Merk.
pub fn encode_child(prefix: &Prefix, id: MerkId) -> [u8; PREFIX_LEN + MerkId::ENCODED_LEN] {
    let mut out = [0; PREFIX_LEN + MerkId::ENCODED_LEN];
    out[..PREFIX_LEN].copy_from_slice(prefix);
    out[PREFIX_LEN..].copy_from_slice(&id.encode());
    out
}

/// Reads a child record: the prefix and the id of the hosted Merk.
pub fn decode_child(bytes: &[u8]) -> Result<(Prefix, MerkId), Malformed> {
    let wrong = Malformed("a child record that is not a prefix and an id");
    let (prefix, id) = bytes.split_at_checked(PREFIX_LEN).ok_or(wrong)?;
    let id = id.try_into().ok().and_then(MerkId::decode);
    Ok((decode_prefix(prefix).map_err(|_| wrong)?, id.ok_or(wrong)?))
}

/// What comes after the parent's id in the key of a child record: the
/// position among the host's Merks, and the host's key.
pub fn decode_child_key(rest: &[u8]) -> Result<(u8, &[u8]), Malformed> {
    match rest.split_first() {
        Some((position, key)) if key.len() <= usize::from(u8::MAX) => Ok((*position, key)),
        _ => Err(Malformed("a child record under no position and key")),
    }
}

/// Reads a prefix: the bytes of a root record, or what comes after the
/// code in the key of an entry.
pub fn decode_prefix(bytes: &[u8]) -> Result<Prefix, Malformed> {
    bytes.try_into().map_err(|_| Malformed("not a prefix"))
}

/// The bytes of the record of a dropped Merk: the address its root had, or
/// zero for a Merk that had none.
pub const fn encode_dropped(root: Option<CellAddress>) -> [u8; CellAddress::ENCODED_LEN] {
    CellAddress::encode_optional(root)
}

/// Reads the record of a dropped Merk: the address its root had.
pub fn decode_dropped(bytes: &[u8]) -> Result<Option<CellAddress>, Malformed> {
    let wrong = Malformed("a dropped Merk's record that is not an address");
    let bytes = bytes.try_into().map_err(|_| wrong)?;
    CellAddress::decode(bytes).map_err(|_| wrong)
}

/// Reads what comes after the code in the key of a dropped Merk's record,
/// or after the code in the key of the child records of one Merk: an id.
pub fn decode_id(bytes: &[u8]) -> Result<MerkId, Malformed> {
    let id = bytes.try_into().ok().and_then(MerkId::decode);
    id.ok_or(Malformed("not an id"))
}

/// The root of a Merk: the cell of its root node, and that node's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Root<'a> {
    /// Where the root's cell is.
    pub address: CellAddress,
    /// The root node's key.
    pub key: &'a [u8],
}

/// The element that hosts a Merk: the back pointer of an entry.
///
/// It names the element as the records do (`transactions.md` ST1, SC9): by
/// the subtree it is in and its key there. It is no cell address, so
/// carrying the host's cell changes no entry (`tree-layout.md` C5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Host<'a> {
    /// The prefix of the Merk the element is in.
    pub parent: &'a Prefix,
    /// The element's key in that Merk.
    pub key: &'a [u8],
    /// The position of the Merk among those the element hosts.
    pub position: u8,
}

/// A Merk as the record of its host has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    /// The position of the Merk among those its host has.
    pub position: u8,
    /// The host's key.
    pub key: Vec<u8>,
    /// The prefix the Merk is, or was, entered under.
    pub prefix: Prefix,
    /// The Merk's id.
    pub id: MerkId,
}

/// What a dropped or cleared Merk left to be reclaimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dropped {
    /// The id the Merk had. The Merks below it are entered under it.
    pub id: MerkId,
    /// The address of its root, which the walk of its cells starts at
    /// (`tree-layout.md` D2), or `None` for a Merk that had no node.
    pub root: Option<CellAddress>,
}

const ROOT_AT: usize = MerkId::ENCODED_LEN;
const ROOT_KEY_AT: usize = ROOT_AT + CellAddress::ENCODED_LEN + 1;
const DEPTH_LEN: usize = 2;

/// The entry of a Merk.
///
/// ```text
/// 8   the Merk's id
/// 6   the address of the root's cell, zero for a Merk with no node
/// 1   the length of the root's key, zero with no root
/// ..  the root's key
/// 1   the length of the tree type
/// ..  the tree type
/// 2   how many Merks the Merk is below, zero for one with no host
/// with a host:
/// 1   the position among the host's Merks
/// 32  the prefix of the Merk the host is in
/// 1   the length of the host's key
/// ..  the host's key
/// 8   the id of each Merk it is below, the outermost first
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    raw: Vec<u8>,
    id: MerkId,
    root: Option<CellAddress>,
    root_key_end: usize,
    type_end: usize,
    /// Where the host's key ends, or zero with no host.
    host_key_end: usize,
}

/// Reads an entry from its start, and ends with an error where it ends
/// early.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    const SHORT: Malformed = Malformed("an entry that ends early");

    fn skip(&mut self, len: usize) -> Result<(), Malformed> {
        let end = self
            .at
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len());
        self.at = end.ok_or(Self::SHORT)?;
        Ok(())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Malformed> {
        let start = self.at;
        self.skip(N)?;
        self.bytes[start..self.at]
            .try_into()
            .map_err(|_| Self::SHORT)
    }

    fn byte(&mut self) -> Result<u8, Malformed> {
        let [byte] = self.array()?;
        Ok(byte)
    }

    /// Passes a byte string that has its length in one byte before it, and
    /// returns where it ends.
    fn string(&mut self) -> Result<usize, Malformed> {
        let len = self.byte()?;
        self.skip(usize::from(len))?;
        Ok(self.at)
    }
}

impl Entry {
    /// Reads the bytes of an entry.
    pub fn decode(raw: Vec<u8>) -> Result<Self, Malformed> {
        let mut read = Reader { bytes: &raw, at: 0 };
        let id = MerkId::decode(read.array()?).ok_or(Malformed("an entry with no id"))?;
        let root = CellAddress::decode(read.array()?)
            .map_err(|_| Malformed("a root address that is not one"))?;
        let root_key_end = read.string()?;
        if root.is_none() && root_key_end != ROOT_KEY_AT {
            return Err(Malformed("a root key with no root"));
        }
        let type_end = read.string()?;
        let depth = u16::from_le_bytes(read.array()?);
        let mut host_key_end = 0;
        if depth > 0 {
            read.skip(1 + PREFIX_LEN)?;
            host_key_end = read.string()?;
            for _ in 0..depth {
                MerkId::decode(read.array()?).ok_or(Malformed("an ancestor with no id"))?;
            }
        }
        if read.at != raw.len() {
            return Err(Malformed("bytes after the entry"));
        }
        Ok(Self {
            raw,
            id,
            root,
            root_key_end,
            type_end,
            host_key_end,
        })
    }

    /// The Merk's id.
    pub const fn id(&self) -> MerkId {
        self.id
    }

    /// The Merk's root, or `None` for a Merk with no node.
    pub fn root(&self) -> Option<Root<'_>> {
        let address = self.root?;
        let key = &self.raw[ROOT_KEY_AT..self.root_key_end];
        Some(Root { address, key })
    }

    /// The tree type, as it was given.
    pub fn tree_type(&self) -> &[u8] {
        &self.raw[self.root_key_end + 1..self.type_end]
    }

    /// The element that hosts the Merk, or `None` for a Merk with no host.
    pub fn host(&self) -> Option<Host<'_>> {
        let at = self.type_end + DEPTH_LEN;
        let (position, rest) = self.raw.get(at..self.host_key_end)?.split_first()?;
        let (parent, key) = rest.split_first_chunk::<PREFIX_LEN>()?;
        let key = key.get(1..)?;
        Some(Host {
            parent,
            key,
            position: *position,
        })
    }

    /// How many Merks the Merk is below.
    pub fn depth(&self) -> usize {
        self.lineage_bytes().len() / MerkId::ENCODED_LEN
    }

    /// The ids of the Merks the Merk is below, the outermost first. The
    /// last is the Merk its host is in.
    pub fn lineage(&self) -> impl DoubleEndedIterator<Item = MerkId> + '_ {
        let (ids, _) = self.lineage_bytes().as_chunks();
        ids.iter().filter_map(|id| MerkId::decode(*id))
    }

    /// The id of the Merk the host is in.
    pub fn parent_id(&self) -> Option<MerkId> {
        self.lineage().next_back()
    }

    /// The entry's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }

    pub(super) fn lineage_bytes(&self) -> &[u8] {
        match self.host_key_end {
            0 => &[],
            end => &self.raw[end..],
        }
    }

    pub(super) fn fields(&self) -> Fields<'_> {
        Fields {
            id: self.id,
            root: self.root(),
            tree_type: self.tree_type(),
            host: self.host(),
            lineage: self.lineage_bytes(),
        }
    }
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("id", &self.id)
            .field("root", &self.root())
            .field("tree_type", &self.tree_type())
            .field("host", &self.host())
            .field("lineage", &self.lineage().collect::<Vec<_>>())
            .finish()
    }
}

/// What an entry holds, for the bytes to be made of.
#[derive(Debug, Clone, Copy)]
pub(super) struct Fields<'a> {
    pub id: MerkId,
    pub root: Option<Root<'a>>,
    pub tree_type: &'a [u8],
    pub host: Option<Host<'a>>,
    /// The ids of the Merks above, as an entry has them. Empty exactly
    /// with no host.
    pub lineage: &'a [u8],
}

/// The length of `bytes` in one byte.
fn short(what: &'static str, bytes: &[u8]) -> Result<u8, DirectoryError> {
    let len = bytes.len();
    u8::try_from(len).map_err(|_| DirectoryError::TooLong { what, len })
}

impl Fields<'_> {
    /// The entry that holds these, or why there is none: a part is too
    /// long.
    pub(super) fn encode(&self) -> Result<Entry, DirectoryError> {
        let root_key = self.root.map_or(&[][..], |root| root.key);
        let depth = self.lineage.len() / MerkId::ENCODED_LEN;
        let depth = u16::try_from(depth).map_err(|_| DirectoryError::TooDeep)?;
        debug_assert_eq!(self.host.is_some(), depth > 0, "a host, and Merks above");

        let mut raw =
            Vec::with_capacity(ROOT_KEY_AT + root_key.len() + 1 + self.tree_type.len() + DEPTH_LEN);
        raw.extend_from_slice(&self.id.encode());
        let address = self.root.map(|root| root.address);
        raw.extend_from_slice(&CellAddress::encode_optional(address));
        raw.push(short("a root key", root_key)?);
        raw.extend_from_slice(root_key);
        let root_key_end = raw.len();
        raw.push(short("a tree type", self.tree_type)?);
        raw.extend_from_slice(self.tree_type);
        let type_end = raw.len();
        raw.extend_from_slice(&depth.to_le_bytes());
        let mut host_key_end = 0;
        if let Some(host) = self.host {
            raw.push(host.position);
            raw.extend_from_slice(host.parent);
            raw.push(short("a host's key", host.key)?);
            raw.extend_from_slice(host.key);
            host_key_end = raw.len();
            raw.extend_from_slice(self.lineage);
        }
        Ok(Entry {
            raw,
            id: self.id,
            root: address,
            root_key_end,
            type_end,
            host_key_end,
        })
    }
}
