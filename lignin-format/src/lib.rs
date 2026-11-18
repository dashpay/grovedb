//! LigninDB on-disk byte formats: page images, delta records, log records, segments, manifest and value log
//!
//! Every format here is a pure function over bytes. The crate knows nothing
//! about files, threads or Merk. The rules all formats share are specified in
//! `docs/lignindb/format.md`.

#![forbid(unsafe_code)]

pub mod checksum;
pub mod error;
pub mod flat_page;
pub mod header;
pub mod ids;
pub mod manifest;
pub mod segment;
pub mod table;
pub mod tree_page;
pub mod value_log;

pub use checksum::Checksum;
pub use error::FormatError;
pub use header::{FileHeader, FileKind, FormatVersion, RecordHeader};
pub use ids::{CellAddress, CommitVersion, FileId, PageId, SlotId};

mod wire;

#[cfg(test)]
mod test_util {
    pub(crate) struct SplitMix64(u64);

    impl SplitMix64 {
        pub(crate) fn new(seed: u64) -> Self {
            Self(seed)
        }

        pub(crate) fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        pub(crate) fn bytes(&mut self, len: usize) -> Vec<u8> {
            (0..len).map(|_| self.next_u64() as u8).collect()
        }
    }
}
