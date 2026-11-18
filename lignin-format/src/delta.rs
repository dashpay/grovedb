//! Page delta records: the changes to a tree page, one record for each.
//!
//! See `docs/lignindb/page-delta.md`.
//!
//! A run is the page id (4 bytes) followed by one or more records. A record
//! is one byte, its kind, the slot of the cell it changes (2 bytes), and the
//! fields of its kind. The create record has no slot.
//!
//! ```text
//! code  record                 after the kind byte                     bytes
//!    1  create the page        departures (8)                              9
//!    2  insert a cell          slot, length (2), the cell           5 + cell
//!    3  replace a cell         slot, length (2), the cell           5 + cell
//!    4  remove a cell          slot                                        3
//!    5  value hash             slot, hash (32)                            35
//!    6  key-value hash         slot, hash (32)                            35
//!    7  left child's hash      slot, hash (32)                            35
//!    8  right child's hash     slot, hash (32)                            35
//!    9  left child's address   slot, cell address (6)                      9
//!   10  right child's address  slot, cell address (6)                      9
//!   11  left link              slot, the link                 45 + aggregate
//!   12  right link             slot, the link                 45 + aggregate
//!   13  no left link           slot                                        3
//!   14  no right link          slot                                        3
//!   15  fragment root          slot                                        3
//!   16  not a fragment root    slot                                        3
//!   17  one hosted root        slot, position (1), address or 0 (6)       10
//!   18  all hosted roots       slot, count (1), address or 0 (6) each
//!                                                           4 + 6 for each
//!   19  feature                slot, length (1), the feature     4 + feature
//!   20  value                  slot, length (2), the value         5 + value
//!   21  value pointer          slot, the value pointer (16)               19
//! ```
//!
//! A link in a record is the child's cell address (6), the child's hash
//! (32), the heights of the child's two subtrees (2), the length of the
//! child's key (1), the length of the aggregate (1) and the aggregate.
//!
//! # Example
//!
//! ```
//! use lignin_format::{
//!     delta::{consolidate, Delta, DeltaRun, DeltaRunWriter, HASH_RECORD_LEN},
//!     tree_page::{CellContent, CellValue, HashField, TreePage, TreePageMut, TreePageSize},
//!     FormatVersion, PageId, SlotId,
//! };
//!
//! # fn main() -> Result<(), lignin_format::FormatError> {
//! let size = TreePageSize::DEFAULT;
//! let version = FormatVersion::CURRENT;
//! let mut frame = vec![0; size.get()];
//! let mut page = TreePageMut::init(&mut frame, size, PageId::MIN, 0)?;
//! page.insert(
//!     SlotId::MIN,
//!     &CellContent {
//!         key: b"key",
//!         feature: &[],
//!         value_hash: &[1; 32],
//!         kv_hash: &[2; 32],
//!         left: None,
//!         right: None,
//!         hosted_roots: &[],
//!         value: CellValue::Inline(b"value"),
//!         fragment_root: true,
//!     },
//! )?;
//! let base = page.page().to_image();
//!
//! // A changed hash is one record of 35 bytes, in a run that names the page.
//! let mut bytes = Vec::new();
//! let mut writer = DeltaRunWriter::new(PageId::MIN, None, &mut bytes);
//! writer.push(&Delta::Hash {
//!     slot: SlotId::MIN,
//!     field: HashField::Value,
//!     hash: &[9; 32],
//! })?;
//! assert_eq!(writer.finish()?, DeltaRun::HEADER_LEN + HASH_RECORD_LEN);
//!
//! // The base image and the run give the page's new base image.
//! let run = DeltaRun::decode(&bytes, version)?;
//! let mut image = Vec::new();
//! consolidate(PageId::MIN, Some(&base), [&run], version, size, &mut image)?;
//! let page = TreePage::decode(&image, version, size)?;
//! let cell = page.cell(SlotId::MIN).expect("the cell");
//! assert_eq!(cell.value_hash(), &[9; 32]);
//! assert_eq!(cell.value(), CellValue::Inline(b"value"));
//! # Ok(())
//! # }
//! ```

use std::fmt;

use crate::{
    error::FormatError,
    ids::{PageId, SlotId},
    tree_page::{TreeCell, TreePageSize, HASH_LEN},
};

mod record;
mod run;

#[cfg(test)]
mod tests;

pub use record::{Delta, RootList};
pub use run::{apply_chain, consolidate, DeltaRun, DeltaRunWriter, Deltas};

/// Bytes of the record of one changed hash: the kind, the slot and the hash.
pub const HASH_RECORD_LEN: usize = 1 + SlotId::ENCODED_LEN + HASH_LEN;

/// The longest cell a record carries: the longest cell of the largest page.
pub const MAX_CELL_LEN: usize = TreePageSize::MAX.max_cell_len();

/// The longest value a record puts into a cell: the longest value the
/// longest cell holds.
pub const MAX_INLINE_VALUE_LEN: usize = MAX_CELL_LEN - TreeCell::FIXED_LEN;

/// Why a byte string is not a run of page deltas, or why a run does not
/// apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaError {
    /// A run holds no record.
    EmptyRun,
    /// A create record that is not the first record of its run.
    MisplacedCreate,
    /// A run, or the base image of a chain, is of another page than the one
    /// it was applied to.
    WrongPage {
        /// The page the run names, or the page the base image is of.
        found: PageId,
        /// The page it was applied to.
        expected: PageId,
    },
    /// A chain that does not create its page was given no base image.
    MissingBase,
}

impl fmt::Display for DeltaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRun => write!(f, "a run with no record"),
            Self::MisplacedCreate => {
                write!(f, "a create record that is not the first of its run")
            }
            Self::WrongPage { found, expected } => write!(
                f,
                "a run or a base image of page {found} applied to page {expected}"
            ),
            Self::MissingBase => write!(
                f,
                "a chain that does not create its page, and no base image"
            ),
        }
    }
}

impl std::error::Error for DeltaError {}

impl From<DeltaError> for FormatError {
    fn from(error: DeltaError) -> Self {
        Self::Delta(error)
    }
}
