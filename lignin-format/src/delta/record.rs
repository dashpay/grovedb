//! One change to a tree page, and its bytes.

use super::{MAX_CELL_LEN, MAX_INLINE_VALUE_LEN};
use crate::{
    error::FormatError,
    header::FormatVersion,
    ids::{CellAddress, SlotId},
    tree_page::{
        CellField, CellValue, Hash, HashField, Side, TreeCell, TreeLink, TreePageError,
        TreePageMut, HASH_LEN,
    },
    value_log::ValuePointer,
};

const ADDRESS_LEN: usize = CellAddress::ENCODED_LEN;
const CELL_LEN_LEN: usize = 2;
const INLINE_LEN_LEN: usize = 2;

/// The kind code of the record that creates a page.
const CREATE: u8 = 1;

/// What a record that changes a cell is, by its first byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Insert,
    Replace,
    Remove,
    Hash(HashField),
    Child(Side),
    Link(Side),
    NoLink(Side),
    FragmentRoot(bool),
    HostedRoot,
    HostedRoots,
    Feature,
    Value,
    ValuePointer,
}

impl Kind {
    const fn code(self) -> u8 {
        match self {
            Self::Insert => 2,
            Self::Replace => 3,
            Self::Remove => 4,
            Self::Hash(HashField::Value) => 5,
            Self::Hash(HashField::KeyValue) => 6,
            Self::Hash(HashField::Child(Side::Left)) => 7,
            Self::Hash(HashField::Child(Side::Right)) => 8,
            Self::Child(Side::Left) => 9,
            Self::Child(Side::Right) => 10,
            Self::Link(Side::Left) => 11,
            Self::Link(Side::Right) => 12,
            Self::NoLink(Side::Left) => 13,
            Self::NoLink(Side::Right) => 14,
            Self::FragmentRoot(true) => 15,
            Self::FragmentRoot(false) => 16,
            Self::HostedRoot => 17,
            Self::HostedRoots => 18,
            Self::Feature => 19,
            Self::Value => 20,
            Self::ValuePointer => 21,
        }
    }

    const fn from_code(code: u8) -> Result<Self, FormatError> {
        Ok(match code {
            2 => Self::Insert,
            3 => Self::Replace,
            4 => Self::Remove,
            5 => Self::Hash(HashField::Value),
            6 => Self::Hash(HashField::KeyValue),
            7 => Self::Hash(HashField::Child(Side::Left)),
            8 => Self::Hash(HashField::Child(Side::Right)),
            9 => Self::Child(Side::Left),
            10 => Self::Child(Side::Right),
            11 => Self::Link(Side::Left),
            12 => Self::Link(Side::Right),
            13 => Self::NoLink(Side::Left),
            14 => Self::NoLink(Side::Right),
            15 => Self::FragmentRoot(true),
            16 => Self::FragmentRoot(false),
            17 => Self::HostedRoot,
            18 => Self::HostedRoots,
            19 => Self::Feature,
            20 => Self::Value,
            21 => Self::ValuePointer,
            code => {
                return Err(FormatError::UnknownRecordKind { code: code as u32 });
            }
        })
    }
}

const fn field_too_long(field: CellField, len: usize, max: usize) -> FormatError {
    FormatError::TreePage(TreePageError::FieldTooLong { field, len, max })
}

const fn check_cell_len(len: usize) -> Result<(), FormatError> {
    if len > MAX_CELL_LEN {
        return Err(FormatError::TreePage(TreePageError::CellTooLarge {
            len,
            max: MAX_CELL_LEN,
        }));
    }
    Ok(())
}

const fn check_inline_len(len: usize) -> Result<(), FormatError> {
    if len > MAX_INLINE_VALUE_LEN {
        return Err(field_too_long(
            CellField::InlineValue,
            len,
            MAX_INLINE_VALUE_LEN,
        ));
    }
    Ok(())
}

/// The hosted-root pointers a record gives a cell, by position. `None` is a
/// hosted Merk with no root.
///
/// A writer makes one from a slice. A record that was read lends its own
/// bytes.
#[derive(Debug, Clone, Copy)]
pub struct RootList<'a>(Roots<'a>);

#[derive(Debug, Clone, Copy)]
enum Roots<'a> {
    Given(&'a [Option<CellAddress>]),
    Read(&'a [[u8; ADDRESS_LEN]]),
}

impl<'a> RootList<'a> {
    /// How many pointers there are.
    pub const fn len(&self) -> usize {
        match self.0 {
            Roots::Given(roots) => roots.len(),
            Roots::Read(fields) => fields.len(),
        }
    }

    /// Whether there is no pointer.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn at(&self, index: usize) -> Option<CellAddress> {
        match self.0 {
            Roots::Given(roots) => roots.get(index).copied().flatten(),
            Roots::Read(fields) => fields
                .get(index)
                .and_then(|field| CellAddress::decode(*field).ok().flatten()),
        }
    }

    /// The pointers, by position.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = Option<CellAddress>> + use<'a> {
        let list = *self;
        (0..list.len()).map(move |index| list.at(index))
    }
}

impl<'a> From<&'a [Option<CellAddress>]> for RootList<'a> {
    fn from(roots: &'a [Option<CellAddress>]) -> Self {
        Self(Roots::Given(roots))
    }
}

impl PartialEq for RootList<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

impl Eq for RootList<'_> {}

/// One change to the cells of a tree page. Each is one call of
/// [`TreePageMut`], and [`Self::apply`] makes that call.
///
/// What a change borrows comes from the caller when it is written and from
/// the run's bytes when it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delta<'a> {
    /// A cell is put into a free slot: a new node, or a cell that was
    /// carried from another page.
    Insert {
        /// The slot the cell takes.
        slot: SlotId,
        /// The cell.
        cell: TreeCell<'a>,
    },
    /// The cell in a slot is replaced by another cell of the same node.
    Replace {
        /// The cell's slot.
        slot: SlotId,
        /// The cell that takes its place. It has the same key.
        cell: TreeCell<'a>,
    },
    /// The cell in a slot leaves the page: its node was deleted, or it was
    /// carried to another page.
    Remove {
        /// The cell's slot.
        slot: SlotId,
    },
    /// One of the hashes a cell stores changes.
    Hash {
        /// The cell's slot.
        slot: SlotId,
        /// Which hash.
        field: HashField,
        /// The hash.
        hash: &'a Hash,
    },
    /// The cell of a child is somewhere else: only the address in the link
    /// changes.
    Child {
        /// The slot of the cell that holds the link.
        slot: SlotId,
        /// The side of the link.
        side: Side,
        /// The child's new address.
        child: CellAddress,
    },
    /// The link on a side is set, replaced or, with `None`, removed.
    Link {
        /// The slot of the cell that holds the link.
        slot: SlotId,
        /// The side of the link.
        side: Side,
        /// The link, or `None` for a node that has no child there any more.
        link: Option<TreeLink<'a>>,
    },
    /// The mark that says whether a cell is the root of a fragment changes.
    FragmentRoot {
        /// The cell's slot.
        slot: SlotId,
        /// Whether the cell is the root of a fragment.
        fragment_root: bool,
    },
    /// One hosted-root pointer of a cell changes.
    HostedRoot {
        /// The cell's slot.
        slot: SlotId,
        /// The pointer's position.
        index: u8,
        /// The root's address, or `None` for a hosted Merk with no root.
        root: Option<CellAddress>,
    },
    /// All hosted-root pointers of a cell are replaced, which may change
    /// their number.
    HostedRoots {
        /// The cell's slot.
        slot: SlotId,
        /// The pointers.
        roots: RootList<'a>,
    },
    /// The feature of a cell is replaced.
    Feature {
        /// The cell's slot.
        slot: SlotId,
        /// The feature.
        feature: &'a [u8],
    },
    /// The value of a cell is replaced, by a value or by a pointer to one.
    /// With a pointer this is the record that installs a value's place in
    /// the value log.
    Value {
        /// The cell's slot.
        slot: SlotId,
        /// The value, or the pointer to it.
        value: CellValue<'a>,
    },
}

impl<'a> Delta<'a> {
    /// The slot of the cell the change is made to.
    pub const fn slot(&self) -> SlotId {
        match *self {
            Self::Insert { slot, .. }
            | Self::Replace { slot, .. }
            | Self::Remove { slot }
            | Self::Hash { slot, .. }
            | Self::Child { slot, .. }
            | Self::Link { slot, .. }
            | Self::FragmentRoot { slot, .. }
            | Self::HostedRoot { slot, .. }
            | Self::HostedRoots { slot, .. }
            | Self::Feature { slot, .. }
            | Self::Value { slot, .. } => slot,
        }
    }

    const fn kind(&self) -> Kind {
        match *self {
            Self::Insert { .. } => Kind::Insert,
            Self::Replace { .. } => Kind::Replace,
            Self::Remove { .. } => Kind::Remove,
            Self::Hash { field, .. } => Kind::Hash(field),
            Self::Child { side, .. } => Kind::Child(side),
            Self::Link {
                side,
                link: Some(_),
                ..
            } => Kind::Link(side),
            Self::Link {
                side, link: None, ..
            } => Kind::NoLink(side),
            Self::FragmentRoot { fragment_root, .. } => Kind::FragmentRoot(fragment_root),
            Self::HostedRoot { .. } => Kind::HostedRoot,
            Self::HostedRoots { .. } => Kind::HostedRoots,
            Self::Feature { .. } => Kind::Feature,
            Self::Value {
                value: CellValue::Inline(_),
                ..
            } => Kind::Value,
            Self::Value {
                value: CellValue::OutOfLine(_),
                ..
            } => Kind::ValuePointer,
        }
    }

    /// Bytes of the change's record.
    pub const fn encoded_len(&self) -> usize {
        let fields = match self {
            Self::Insert { cell, .. } | Self::Replace { cell, .. } => {
                CELL_LEN_LEN + cell.encoded_len()
            }
            Self::Remove { .. } | Self::FragmentRoot { .. } | Self::Link { link: None, .. } => 0,
            Self::Hash { .. } => HASH_LEN,
            Self::Child { .. } => ADDRESS_LEN,
            Self::Link {
                link: Some(link), ..
            } => link.encoded_len(),
            Self::HostedRoot { .. } => 1 + ADDRESS_LEN,
            Self::HostedRoots { roots, .. } => 1 + roots.len() * ADDRESS_LEN,
            Self::Feature { feature, .. } => 1 + feature.len(),
            Self::Value { value, .. } => match value {
                CellValue::Inline(value) => INLINE_LEN_LEN + value.len(),
                CellValue::OutOfLine(_) => ValuePointer::ENCODED_LEN,
            },
        };
        1 + SlotId::ENCODED_LEN + fields
    }

    fn check(&self) -> Result<(), FormatError> {
        match self {
            Self::Insert { cell, .. } | Self::Replace { cell, .. } => {
                check_cell_len(cell.encoded_len())?;
                TreeCell::decode(cell.as_bytes(), FormatVersion::CURRENT).map(drop)
            }
            Self::Link {
                link: Some(link), ..
            } if link.aggregate.len() > TreeLink::MAX_AGGREGATE_LEN => Err(field_too_long(
                CellField::Aggregate,
                link.aggregate.len(),
                TreeLink::MAX_AGGREGATE_LEN,
            )),
            Self::HostedRoots { roots, .. } if roots.len() > TreeCell::MAX_HOSTED_ROOTS => {
                Err(field_too_long(
                    CellField::HostedRoots,
                    roots.len(),
                    TreeCell::MAX_HOSTED_ROOTS,
                ))
            }
            Self::Feature { feature, .. } if feature.len() > TreeCell::MAX_FEATURE_LEN => Err(
                field_too_long(CellField::Feature, feature.len(), TreeCell::MAX_FEATURE_LEN),
            ),
            Self::Value {
                value: CellValue::Inline(value),
                ..
            } => check_inline_len(value.len()),
            _ => Ok(()),
        }
    }

    /// Appends the change's record to `out`, in the current format version.
    /// On an error `out` is unchanged.
    ///
    /// A cell is checked as [`TreeCell::decode`] checks it, and a part that
    /// is too long for its length field is refused, so what is written here
    /// reads back.
    pub fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), FormatError> {
        self.check()?;
        out.reserve(self.encoded_len());
        out.push(self.kind().code());
        out.extend_from_slice(&self.slot().encode());
        match *self {
            Self::Insert { cell, .. } | Self::Replace { cell, .. } => {
                out.extend_from_slice(&(cell.encoded_len() as u16).to_le_bytes());
                out.extend_from_slice(cell.as_bytes());
            }
            Self::Remove { .. } | Self::FragmentRoot { .. } | Self::Link { link: None, .. } => {}
            Self::Hash { hash, .. } => out.extend_from_slice(hash),
            Self::Child { child, .. } => out.extend_from_slice(&child.encode()),
            Self::Link {
                link: Some(link), ..
            } => {
                out.extend_from_slice(&link.child.encode());
                out.extend_from_slice(link.hash);
                out.extend_from_slice(&[
                    link.heights.0,
                    link.heights.1,
                    link.child_key_len,
                    link.aggregate.len() as u8,
                ]);
                out.extend_from_slice(link.aggregate);
            }
            Self::HostedRoot { index, root, .. } => {
                out.push(index);
                out.extend_from_slice(&CellAddress::encode_optional(root));
            }
            Self::HostedRoots { roots, .. } => {
                out.push(roots.len() as u8);
                for root in roots.iter() {
                    out.extend_from_slice(&CellAddress::encode_optional(root));
                }
            }
            Self::Feature { feature, .. } => {
                out.push(feature.len() as u8);
                out.extend_from_slice(feature);
            }
            Self::Value { value, .. } => match value {
                CellValue::Inline(value) => {
                    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
                    out.extend_from_slice(value);
                }
                CellValue::OutOfLine(pointer) => out.extend_from_slice(&pointer.encode()),
            },
        }
        Ok(())
    }

    /// Makes the change to `page`.
    ///
    /// The errors are those of the [`TreePageMut`] call the change is. When
    /// one is returned, the page is as it was.
    pub fn apply(&self, page: &mut TreePageMut<'_>) -> Result<(), FormatError> {
        match *self {
            Self::Insert { slot, cell } => page.insert_encoded(slot, cell),
            Self::Replace { slot, cell } => page.replace_encoded(slot, cell),
            Self::Remove { slot } => page.remove(slot),
            Self::Hash { slot, field, hash } => page.set_hash(slot, field, hash),
            Self::Child { slot, side, child } => page.set_child(slot, side, child),
            Self::Link { slot, side, link } => page.set_link(slot, side, link.as_ref()),
            Self::FragmentRoot {
                slot,
                fragment_root,
            } => page.set_fragment_root(slot, fragment_root),
            Self::HostedRoot { slot, index, root } => {
                page.set_hosted_root(slot, usize::from(index), root)
            }
            Self::HostedRoots { slot, roots } => match roots.0 {
                Roots::Given(roots) => page.set_hosted_roots(slot, roots),
                Roots::Read(_) => {
                    let mut buffer = [None; TreeCell::MAX_HOSTED_ROOTS];
                    let mut count = 0;
                    for (place, root) in buffer.iter_mut().zip(roots.iter()) {
                        *place = root;
                        count += 1;
                    }
                    page.set_hosted_roots(slot, &buffer[..count])
                }
            },
            Self::Feature { slot, feature } => page.set_feature(slot, feature),
            Self::Value { slot, value } => page.set_value(slot, value),
        }
    }
}

/// Appends the record that creates a page whose count of departed cells
/// starts at `departures`.
pub(super) fn write_create(departures: u64, out: &mut Vec<u8>) {
    out.push(CREATE);
    out.extend_from_slice(&departures.to_le_bytes());
}

/// A record as it was read.
#[derive(Debug, Clone, Copy)]
pub(super) enum Record<'a> {
    Create { departures: u64 },
    Change(Delta<'a>),
}

/// Reads fields off the front of a byte string, and counts what a missing
/// one would have needed from the string's first byte.
#[derive(Debug, Clone, Copy)]
pub(super) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(super) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub(super) const fn position(&self) -> usize {
        self.at
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn rest(&self) -> &'a [u8] {
        self.bytes.get(self.at..).unwrap_or_default()
    }

    fn truncated(&self, len: usize) -> FormatError {
        FormatError::Truncated {
            needed: self.at.saturating_add(len),
            available: self.bytes.len(),
        }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], FormatError> {
        let part = self.rest().get(..len).ok_or_else(|| self.truncated(len))?;
        self.at += len;
        Ok(part)
    }

    pub(super) fn array<const N: usize>(&mut self) -> Result<&'a [u8; N], FormatError> {
        let array = self.rest().first_chunk().ok_or_else(|| self.truncated(N))?;
        self.at += N;
        Ok(array)
    }

    fn byte(&mut self) -> Result<u8, FormatError> {
        self.array().map(|&[byte]| byte)
    }

    fn slot(&mut self) -> Result<SlotId, FormatError> {
        SlotId::decode(*self.array()?).ok_or(FormatError::MissingId)
    }

    fn address(&mut self) -> Result<Option<CellAddress>, FormatError> {
        CellAddress::decode(*self.array()?)
    }

    fn cell(&mut self, version: FormatVersion) -> Result<TreeCell<'a>, FormatError> {
        let len = usize::from(u16::from_le_bytes(*self.array()?));
        check_cell_len(len)?;
        TreeCell::decode(self.take(len)?, version).map_err(|error| match error {
            FormatError::Truncated { needed, available } => TreePageError::CellLengthMismatch {
                expected: needed,
                found: available,
            }
            .into(),
            error => error,
        })
    }

    fn link(&mut self) -> Result<TreeLink<'a>, FormatError> {
        let child = self.address()?.ok_or(FormatError::MissingId)?;
        let hash = self.array()?;
        let &[left_height, right_height, child_key_len, aggregate_len] = self.array()?;
        Ok(TreeLink {
            child,
            hash,
            heights: (left_height, right_height),
            child_key_len,
            aggregate: self.take(usize::from(aggregate_len))?,
        })
    }

    fn roots(&mut self) -> Result<RootList<'a>, FormatError> {
        let count = usize::from(self.byte()?);
        let (fields, _) = self.take(count * ADDRESS_LEN)?.as_chunks();
        for field in fields {
            CellAddress::decode(*field)?;
        }
        Ok(RootList(Roots::Read(fields)))
    }

    /// Reads and fully checks the next record, written in format `version`.
    ///
    /// The kind is checked first, then each field in the order it stands:
    /// that its bytes are there, then that they are valid.
    pub(super) fn record(&mut self, version: FormatVersion) -> Result<Record<'a>, FormatError> {
        match version {
            FormatVersion::V1 => {}
        }
        let code = self.byte()?;
        if code == CREATE {
            let departures = u64::from_le_bytes(*self.array()?);
            return Ok(Record::Create { departures });
        }
        let kind = Kind::from_code(code)?;
        let slot = self.slot()?;
        Ok(Record::Change(match kind {
            Kind::Remove => Delta::Remove { slot },
            Kind::Insert => Delta::Insert {
                slot,
                cell: self.cell(version)?,
            },
            Kind::Replace => Delta::Replace {
                slot,
                cell: self.cell(version)?,
            },
            Kind::Hash(field) => Delta::Hash {
                slot,
                field,
                hash: self.array()?,
            },
            Kind::Child(side) => Delta::Child {
                slot,
                side,
                child: self.address()?.ok_or(FormatError::MissingId)?,
            },
            Kind::Link(side) => Delta::Link {
                slot,
                side,
                link: Some(self.link()?),
            },
            Kind::NoLink(side) => Delta::Link {
                slot,
                side,
                link: None,
            },
            Kind::FragmentRoot(fragment_root) => Delta::FragmentRoot {
                slot,
                fragment_root,
            },
            Kind::HostedRoot => Delta::HostedRoot {
                slot,
                index: self.byte()?,
                root: self.address()?,
            },
            Kind::HostedRoots => Delta::HostedRoots {
                slot,
                roots: self.roots()?,
            },
            Kind::Feature => {
                let len = usize::from(self.byte()?);
                Delta::Feature {
                    slot,
                    feature: self.take(len)?,
                }
            }
            Kind::Value => {
                let len = usize::from(u16::from_le_bytes(*self.array()?));
                check_inline_len(len)?;
                Delta::Value {
                    slot,
                    value: CellValue::Inline(self.take(len)?),
                }
            }
            Kind::ValuePointer => Delta::Value {
                slot,
                value: CellValue::OutOfLine(ValuePointer::decode(*self.array()?)?),
            },
        }))
    }
}
