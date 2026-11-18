//! Identifiers shared by every LigninDB file.
//!
//! See "Identifiers" in `docs/lignindb/format.md`.

use std::{
    fmt,
    num::{NonZeroU16, NonZeroU32},
};

use crate::error::FormatError;

macro_rules! nonzero_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal, $raw:ty, $nonzero:ty, $max:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($nonzero);

        impl $name {
            /// Bytes the id takes on disk.
            pub const ENCODED_LEN: usize = size_of::<$raw>();
            /// The smallest id, `1`.
            pub const MIN: Self = Self(<$nonzero>::MIN);
            #[doc = concat!("The largest id, `", $max, "`.")]
            pub const MAX: Self = Self(<$nonzero>::MAX);

            /// The id with this number, or `None` for zero, which is not an id.
            pub const fn new(raw: $raw) -> Option<Self> {
                match <$nonzero>::new(raw) {
                    Some(raw) => Some(Self(raw)),
                    None => None,
                }
            }

            /// The id's number.
            pub const fn get(self) -> $raw {
                self.0.get()
            }

            /// The bytes written to disk.
            pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
                self.get().to_le_bytes()
            }

            /// The bytes of a field that may hold no id: zero for `None`.
            pub const fn encode_optional(id: Option<Self>) -> [u8; Self::ENCODED_LEN] {
                match id {
                    Some(id) => id.encode(),
                    None => [0; Self::ENCODED_LEN],
                }
            }

            /// Reads an id field. Zero decodes to `None`.
            pub const fn decode(bytes: [u8; Self::ENCODED_LEN]) -> Option<Self> {
                Self::new(<$raw>::from_le_bytes(bytes))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.get())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.get(), f)
            }
        }
    };
}

nonzero_id!(
    /// Names one logical page in the page table, whatever the page holds.
    ///
    /// A 32-bit number from 1 up. Zero on disk means "no page".
    PageId,
    "page#",
    u32,
    NonZeroU32,
    "2^32 - 1"
);

nonzero_id!(
    /// The number in a numbered file's name: `seg/00000310.seg` is file 310 of
    /// its kind.
    ///
    /// A 32-bit number from 1 up. Zero on disk means "no file".
    FileId,
    "file#",
    u32,
    NonZeroU32,
    "2^32 - 1"
);

nonzero_id!(
    /// Names one cell inside a tree page: its entry in the page's slot
    /// directory.
    ///
    /// A 16-bit number from 1 up. Zero on disk means "no slot". A slot number
    /// names the same cell from the commit that puts the cell into the page
    /// to the commit that takes it out.
    SlotId,
    "slot#",
    u16,
    NonZeroU16,
    "2^16 - 1"
);

impl SlotId {
    /// The slot's position in the slot directory, counted from 0.
    pub const fn index(self) -> usize {
        self.get() as usize - 1
    }

    /// The slot at position `index` of the slot directory, or `None` past
    /// the last slot number.
    pub const fn from_index(index: usize) -> Option<Self> {
        if index < u16::MAX as usize {
            Self::new(index as u16 + 1)
        } else {
            None
        }
    }
}

/// Where a cell is: its page and its slot in that page.
///
/// Six bytes on disk, the page id followed by the slot. All zero means "no
/// cell".
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellAddress {
    page: PageId,
    slot: SlotId,
}

impl CellAddress {
    /// Bytes the address takes on disk.
    pub const ENCODED_LEN: usize = PageId::ENCODED_LEN + SlotId::ENCODED_LEN;

    /// The address of the cell in `slot` of `page`.
    pub const fn new(page: PageId, slot: SlotId) -> Self {
        Self { page, slot }
    }

    /// The page the cell is in.
    pub const fn page(self) -> PageId {
        self.page
    }

    /// The cell's slot in that page.
    pub const fn slot(self) -> SlotId {
        self.slot
    }

    /// The six bytes written to disk.
    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let [p0, p1, p2, p3] = self.page.encode();
        let [s0, s1] = self.slot.encode();
        [p0, p1, p2, p3, s0, s1]
    }

    /// The bytes of a field that may hold no address: zero for `None`.
    pub const fn encode_optional(address: Option<Self>) -> [u8; Self::ENCODED_LEN] {
        match address {
            Some(address) => address.encode(),
            None => [0; Self::ENCODED_LEN],
        }
    }

    /// Reads an address field. All zero decodes to `None`. A page with no
    /// slot, or a slot with no page, is an error.
    pub const fn decode(bytes: [u8; Self::ENCODED_LEN]) -> Result<Option<Self>, FormatError> {
        let [p0, p1, p2, p3, s0, s1] = bytes;
        match (PageId::decode([p0, p1, p2, p3]), SlotId::decode([s0, s1])) {
            (Some(page), Some(slot)) => Ok(Some(Self { page, slot })),
            (None, None) => Ok(None),
            _ => Err(FormatError::MissingId),
        }
    }
}

impl fmt::Debug for CellAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}/{:?}", self.page, self.slot)
    }
}

impl fmt::Display for CellAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.page, self.slot)
    }
}

/// Counts the commits of a database: the empty database is at version 0, and
/// each committed write transaction moves it to the next version.
///
/// A 64-bit number. `u64::MAX` is never a version; on disk it means "no
/// version".
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitVersion(u64);

impl CommitVersion {
    /// Bytes the version takes on disk.
    pub const ENCODED_LEN: usize = 8;
    /// The version of a database with no commit.
    pub const EMPTY: Self = Self(0);
    /// The largest version, `2^64 - 2`.
    pub const MAX: Self = Self(u64::MAX - 1);

    /// The version with this number, or `None` for `u64::MAX`, which is not a
    /// version.
    pub const fn new(raw: u64) -> Option<Self> {
        if raw == u64::MAX {
            None
        } else {
            Some(Self(raw))
        }
    }

    /// The version's number.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The version the next commit produces, or `None` after [`Self::MAX`].
    pub const fn next(self) -> Option<Self> {
        Self::new(self.0 + 1)
    }

    /// The eight bytes written to disk.
    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        self.0.to_le_bytes()
    }

    /// The bytes of a field that may hold no version: all ones for `None`.
    pub const fn encode_optional(version: Option<Self>) -> [u8; Self::ENCODED_LEN] {
        match version {
            Some(version) => version.encode(),
            None => [0xFF; Self::ENCODED_LEN],
        }
    }

    /// Reads a version field. All ones decodes to `None`.
    pub const fn decode(bytes: [u8; Self::ENCODED_LEN]) -> Option<Self> {
        Self::new(u64::from_le_bytes(bytes))
    }
}

impl fmt::Debug for CommitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

impl fmt::Display for CommitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::SplitMix64;

    fn u32_samples() -> Vec<u32> {
        let mut rng = SplitMix64::new(0x1D5);
        let mut samples = vec![1, 2, 255, 256, 65_535, 65_536, 0x0102_0304, u32::MAX - 1];
        samples.push(u32::MAX);
        samples.extend(
            (0..10_000)
                .map(|_| rng.next_u64() as u32)
                .filter(|&raw| raw != 0),
        );
        samples
    }

    macro_rules! nonzero_u32_id_tests {
        ($module:ident, $name:ident, $debug:literal) => {
            mod $module {
                use super::*;

                #[test]
                fn round_trips() {
                    for raw in u32_samples() {
                        let id = $name::new(raw).expect("non-zero");
                        assert_eq!(id.get(), raw);
                        assert_eq!(id.encode(), raw.to_le_bytes());
                        assert_eq!($name::decode(id.encode()), Some(id));
                        assert_eq!($name::encode_optional(Some(id)), id.encode());
                    }
                }

                #[test]
                fn every_byte_string_decodes_to_the_value_that_encodes_back_to_it() {
                    for raw in u32_samples().into_iter().chain([0]) {
                        let bytes = raw.to_le_bytes();
                        assert_eq!($name::encode_optional($name::decode(bytes)), bytes);
                    }
                }

                #[test]
                fn zero_is_no_id() {
                    assert_eq!($name::new(0), None);
                    assert_eq!($name::decode([0; 4]), None);
                    assert_eq!($name::encode_optional(None), [0; 4]);
                }

                #[test]
                fn bounds_and_byte_order() {
                    assert_eq!($name::MIN.get(), 1);
                    assert_eq!($name::MAX.get(), u32::MAX);
                    assert_eq!($name::ENCODED_LEN, 4);
                    let id = $name::new(0x0102_0304).expect("non-zero");
                    assert_eq!(id.encode(), [0x04, 0x03, 0x02, 0x01]);
                }

                #[test]
                fn orders_by_number_and_prints_it() {
                    let low = $name::new(9).expect("non-zero");
                    let high = $name::new(10).expect("non-zero");
                    assert!(low < high);
                    assert_eq!(low.to_string(), "9");
                    assert_eq!(format!("{high:?}"), concat!($debug, "10"));
                }
            }
        };
    }

    nonzero_u32_id_tests!(page_id, PageId, "page#");
    nonzero_u32_id_tests!(file_id, FileId, "file#");

    #[test]
    fn slot_id_round_trips_for_every_number() {
        for raw in 1..=u16::MAX {
            let slot = SlotId::new(raw).expect("non-zero");
            assert_eq!(slot.get(), raw);
            assert_eq!(slot.encode(), raw.to_le_bytes());
            assert_eq!(SlotId::decode(slot.encode()), Some(slot));
            assert_eq!(SlotId::encode_optional(Some(slot)), slot.encode());
            assert_eq!(slot.index(), usize::from(raw) - 1);
            assert_eq!(SlotId::from_index(slot.index()), Some(slot));
        }
    }

    #[test]
    fn zero_is_no_slot_id() {
        assert_eq!(SlotId::new(0), None);
        assert_eq!(SlotId::decode([0; 2]), None);
        assert_eq!(SlotId::encode_optional(None), [0; 2]);
        assert_eq!(SlotId::from_index(usize::from(u16::MAX)), None);
        assert_eq!(SlotId::from_index(usize::MAX), None);
    }

    #[test]
    fn slot_id_bounds_byte_order_and_printing() {
        assert_eq!(SlotId::MIN.get(), 1);
        assert_eq!(SlotId::MAX.get(), u16::MAX);
        assert_eq!(SlotId::ENCODED_LEN, 2);
        assert_eq!(SlotId::MIN.index(), 0);
        let low = SlotId::new(0x0102).expect("non-zero");
        assert_eq!(low.encode(), [0x02, 0x01]);
        assert!(low < SlotId::MAX);
        assert_eq!(low.to_string(), "258");
        assert_eq!(format!("{low:?}"), "slot#258");
    }

    fn address(page: u32, slot: u16) -> CellAddress {
        CellAddress::new(
            PageId::new(page).expect("non-zero"),
            SlotId::new(slot).expect("non-zero"),
        )
    }

    #[test]
    fn cell_address_round_trips() {
        let mut rng = SplitMix64::new(0xCE11);
        for _ in 0..10_000 {
            let (page, slot) = (rng.next_u64() as u32 | 1, rng.next_u64() as u16 | 1);
            let address = address(page, slot);
            assert_eq!(address.page().get(), page);
            assert_eq!(address.slot().get(), slot);
            assert_eq!(CellAddress::decode(address.encode()), Ok(Some(address)));
            assert_eq!(
                CellAddress::encode_optional(Some(address)),
                address.encode()
            );
        }
    }

    #[test]
    fn cell_address_bytes_are_the_page_then_the_slot() {
        assert_eq!(CellAddress::ENCODED_LEN, 6);
        let address = address(0x0102_0304, 0x0506);
        assert_eq!(address.encode(), [0x04, 0x03, 0x02, 0x01, 0x06, 0x05]);
        assert_eq!(address.to_string(), "16909060/1286");
        assert_eq!(format!("{address:?}"), "page#16909060/slot#1286");
        assert!(address < self::address(0x0102_0304, 0x0507));
        assert!(address < self::address(0x0102_0305, 1));
    }

    #[test]
    fn all_zero_is_no_cell_address_and_half_an_address_is_an_error() {
        assert_eq!(CellAddress::decode([0; 6]), Ok(None));
        assert_eq!(CellAddress::encode_optional(None), [0; 6]);
        assert_eq!(
            CellAddress::decode([7, 0, 0, 0, 0, 0]),
            Err(FormatError::MissingId)
        );
        assert_eq!(
            CellAddress::decode([0, 0, 0, 0, 3, 0]),
            Err(FormatError::MissingId)
        );
    }

    #[test]
    fn every_cell_address_field_decodes_to_what_encodes_back_to_it() {
        let mut rng = SplitMix64::new(0xADD2);
        for _ in 0..10_000 {
            let mut bytes = [0; 6];
            bytes.copy_from_slice(&rng.bytes(6));
            if rng.next_u64().is_multiple_of(4) {
                bytes[..4].fill(0);
            }
            if rng.next_u64().is_multiple_of(4) {
                bytes[4..].fill(0);
            }
            if let Ok(address) = CellAddress::decode(bytes) {
                assert_eq!(CellAddress::encode_optional(address), bytes);
            }
        }
    }

    fn u64_samples() -> Vec<u64> {
        let mut rng = SplitMix64::new(0xC0);
        let mut samples = vec![0, 1, 255, 256, u64::from(u32::MAX), 1 << 32, u64::MAX - 1];
        samples.push(0x0102_0304_0506_0708);
        samples.extend(
            (0..10_000)
                .map(|_| rng.next_u64())
                .filter(|&raw| raw != u64::MAX),
        );
        samples
    }

    #[test]
    fn commit_version_round_trips() {
        for raw in u64_samples() {
            let version = CommitVersion::new(raw).expect("not reserved");
            assert_eq!(version.get(), raw);
            assert_eq!(version.encode(), raw.to_le_bytes());
            assert_eq!(CommitVersion::decode(version.encode()), Some(version));
            assert_eq!(
                CommitVersion::encode_optional(Some(version)),
                version.encode()
            );
        }
    }

    #[test]
    fn every_byte_string_decodes_to_the_commit_version_that_encodes_back_to_it() {
        for raw in u64_samples().into_iter().chain([u64::MAX]) {
            let bytes = raw.to_le_bytes();
            assert_eq!(
                CommitVersion::encode_optional(CommitVersion::decode(bytes)),
                bytes
            );
        }
    }

    #[test]
    fn all_ones_is_no_commit_version() {
        assert_eq!(CommitVersion::new(u64::MAX), None);
        assert_eq!(CommitVersion::decode([0xFF; 8]), None);
        assert_eq!(CommitVersion::encode_optional(None), [0xFF; 8]);
    }

    #[test]
    fn commit_version_bounds_and_byte_order() {
        assert_eq!(CommitVersion::EMPTY.get(), 0);
        assert_eq!(CommitVersion::EMPTY.encode(), [0; 8]);
        assert_eq!(CommitVersion::MAX.get(), u64::MAX - 1);
        assert_eq!(CommitVersion::ENCODED_LEN, 8);
        let version = CommitVersion::new(0x0102_0304_0506_0708).expect("not reserved");
        assert_eq!(
            version.encode(),
            [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
    }

    #[test]
    fn commit_versions_advance_by_one_and_stop_at_the_maximum() {
        let first = CommitVersion::EMPTY.next().expect("has a successor");
        assert_eq!(first.get(), 1);
        assert!(CommitVersion::EMPTY < first);
        let last = CommitVersion::new(u64::MAX - 2)
            .and_then(CommitVersion::next)
            .expect("has a successor");
        assert_eq!(last, CommitVersion::MAX);
        assert_eq!(CommitVersion::MAX.next(), None);
        assert_eq!(first.to_string(), "1");
        assert_eq!(format!("{first:?}"), "v1");
    }
}
