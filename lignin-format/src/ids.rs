//! Identifiers shared by every LigninDB file.
//!
//! See "Identifiers" in `docs/lignindb/format.md`.

use std::{fmt, num::NonZeroU32};

macro_rules! nonzero_u32_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(NonZeroU32);

        impl $name {
            /// Bytes the id takes on disk.
            pub const ENCODED_LEN: usize = 4;
            /// The smallest id, `1`.
            pub const MIN: Self = Self(NonZeroU32::MIN);
            /// The largest id, `2^32 - 1`.
            pub const MAX: Self = Self(NonZeroU32::MAX);

            /// The id with this number, or `None` for zero, which is not an id.
            pub const fn new(raw: u32) -> Option<Self> {
                match NonZeroU32::new(raw) {
                    Some(raw) => Some(Self(raw)),
                    None => None,
                }
            }

            /// The id's number.
            pub const fn get(self) -> u32 {
                self.0.get()
            }

            /// The four bytes written to disk.
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
                Self::new(u32::from_le_bytes(bytes))
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

nonzero_u32_id!(
    /// Names one logical page in the page table, whatever the page holds.
    ///
    /// A 32-bit number from 1 up. Zero on disk means "no page".
    PageId,
    "page#"
);

nonzero_u32_id!(
    /// The number in a numbered file's name: `seg/00000310.seg` is file 310 of
    /// its kind.
    ///
    /// A 32-bit number from 1 up. Zero on disk means "no file".
    FileId,
    "file#"
);

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
