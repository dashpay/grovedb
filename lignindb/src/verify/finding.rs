//! What the checker reports: the rule that does not hold, and where.

use std::fmt;

use lignin_format::{tree_page::HashField, CellAddress, FileId, PageId};
use lignin_store::segment::BlockLocation;
use lignin_tree::directory::Prefix;

/// A rule of the physical layout that the checker tests.
///
/// Each names one way a store can be wrong. The text of a rule
/// ([`Rule::text`]) says what holds in a sound store, with the place it is
/// specified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    /// A page that the version holds cannot be read: a block of its chain
    /// is refused, or its bytes are no page.
    PageUnreadable,
    /// A page holds the id of another page (`tree-page.md` TP2,
    /// `flat-page.md` P14).
    PageId,
    /// A tree page is not well formed: a cell is not valid, two cells share
    /// bytes, or the header's counts are not the page's (`tree-page.md`).
    TreePageFormat,
    /// The cells of one tree page disagree: an address into the page names
    /// no cell, a cell is named twice, a link states another key length
    /// than its child has, or a fragment-root mark is not what the pointers
    /// imply (`tree-layout.md` F1, F3; `TreePage::check_fragments`).
    Fragments,
    /// A stored address names a slot that holds no cell, or a page that is
    /// no tree page of the version (`tree-layout.md` S1, S3).
    DanglingAddress,
    /// A cell has two stored addresses besides the directory: two links or
    /// hosted-root pointers name it, or the links form a circle
    /// (`tree-layout.md` C1, C5).
    CellNamedTwice,
    /// A link states another key length than the child's key has
    /// (`tree-layout.md` C2).
    LinkKeyLength,
    /// A link states other heights than the child's own links give
    /// (`tree-layout.md` C2).
    LinkHeights,
    /// A node's key is not between the keys its place in its Merk allows:
    /// the address leads to another node than it means (`tree-layout.md`
    /// S1).
    KeyOrder,
    /// A stored hash differs from its recomputation
    /// (`lignin_tree::hash::check`).
    Hash(HashField),
    /// The hashes of a cell could not be recomputed: a rule refused, or
    /// what a hash is made from could not be read.
    HashUnchecked,
    /// The value of a cell or of a flat record is not read through its
    /// pointer: no value is there, or it has another length.
    ValueUnreadable,
    /// A value pointer leads to a record that names another page as its
    /// owner: a cell was carried without its value (`value-log-format.md`
    /// O2, O5; `tree-layout.md` X1).
    ValueOwner,
    /// Two cells hold the same value pointer (`value-log-format.md` O2).
    ValueShared,
    /// A pending value has no bytes with the frame of its page, or bytes
    /// of another length (`transactions.md` FL7).
    PendingBytes,
    /// A page version that a segment makes durable holds a pending value
    /// (`page-delta.md` PD16, `flat-page.md` P15).
    PendingInSegment,
    /// A record placed a value under a pointer of another length than the
    /// pending value had (`page-delta.md`, record 21).
    PlacedLength,
    /// The store's count of the live values of a value-log file is not the
    /// number of pointers the pages hold into it (`value-log-format.md`
    /// O6).
    ValueCount,
    /// The records of the subtree directory disagree with each other
    /// (`lignin_tree::directory`, `Directory::check`).
    Directory,
    /// The root that an entry names is no cell, or a cell with another key
    /// than the entry states (`tree-layout.md` S3).
    EntryRoot,
    /// The element that an entry names as the host of its Merk is no node
    /// of the Merk it is said to be in.
    EntryHost,
    /// The pointer in the cell of a host is not the root that the entry of
    /// the hosted Merk names (`tree-layout.md` C3).
    HostPointer,
    /// A hosted-root pointer of a cell in use belongs to no entry: the
    /// directory does not know the Merk (plan, draft 17).
    HostedUnentered,
    /// A cell in use points at the root of a Merk whose entry does not
    /// name that cell's node as its host: the entry names no host, or
    /// another one.
    HostUnnamed,
    /// A flat record stands under a prefix that the directory has no entry
    /// for (`tree-layout.md` D6).
    FlatRecordWithoutEntry,
    /// A flat record in the data column of a subtree whose entry is of a
    /// tree type that keeps a Merk is no node that no node links
    /// (`transactions.md` ST1; `lignindb::txn`, "Unlinked records").
    RecordKind,
    /// A key of a subtree holds a node of the subtree's Merk and a flat
    /// record of the data column (`transactions.md` ST1).
    KeyHeldTwice,
    /// An entry of a tree type that keeps flat records is hosted in
    /// another place than the first, or its host hosts another Merk in
    /// use.
    FlatTreeHost,
    /// A flat page is not well formed, or is of the wrong kind where it
    /// stands.
    FlatPageFormat,
    /// A branch or a bucket of long keys names a page that is no flat page
    /// of the version.
    FlatDanglingPage,
    /// A flat page is reached twice: from two trees, from two branches, or
    /// from a tree and from what a range delete left (`flat-page.md`, the
    /// walk of TREE-07).
    FlatPageNamedTwice,
    /// A page below a branch is not one level down (`flat-page.md` P8).
    FlatLevel,
    /// The keys of a flat page are not in order, or not between the
    /// separators of the branches above it (`flat-page.md` P6).
    FlatKeyOrder,
    /// A bucket of long keys is not what the tree writes: its key is not
    /// the head and a zero byte, or its value names no page.
    FlatBucket,
    /// A flat page of the version is reached from no tree and was left by
    /// no range delete.
    FlatPageUnreached,
    /// A record of a lazy range delete is not read, or names a page that
    /// is no flat page.
    FlatDropRecord,
    /// A page of the latest version has an id that the allocator does not
    /// hold as live (`page-alloc.md`).
    IdNotLive,
    /// The allocator holds an id as live that names no page of the latest
    /// version (`page-alloc.md`).
    IdWithoutPage,
    /// The chain of a page in the page table breaks a rule of chains
    /// (`page-table.md`).
    Chain,
    /// A block that the page table names is not read from its segment, or
    /// is not what its link says.
    Block,
    /// Two links of the page table name one block.
    BlockNamedTwice,
}

impl Rule {
    /// What holds in a sound store.
    pub const fn text(&self) -> &'static str {
        match self {
            Self::PageUnreadable => "every page of the version is read",
            Self::PageId => "a page holds its own id",
            Self::TreePageFormat => "a tree page is well formed",
            Self::Fragments => "the cells of a page agree, and its fragment roots are marked",
            Self::DanglingAddress => "every stored address names a cell",
            Self::CellNamedTwice => "a cell has one stored address besides the directory",
            Self::LinkKeyLength => "a link states the length of its child's key",
            Self::LinkHeights => "a link states the heights of its child's subtrees",
            Self::KeyOrder => "an address names the node it means: the keys of a Merk are in order",
            Self::Hash(HashField::Value) => "a value hash is the hash of its value",
            Self::Hash(HashField::KeyValue) => {
                "a key-value hash is made from its key and value hash"
            }
            Self::Hash(HashField::Child(_)) => "the hash in a link is the hash of the child",
            Self::HashUnchecked => "every stored hash can be recomputed",
            Self::ValueUnreadable => "every value pointer leads to its value",
            Self::ValueOwner => {
                "a value's record names the page of the cell that holds its pointer"
            }
            Self::ValueShared => "no two cells hold one value pointer",
            Self::PendingBytes => "a pending value has its bytes with its page",
            Self::PendingInSegment => "no page version in a segment holds a pending value",
            Self::PlacedLength => "a value is placed under a pointer of its own length",
            Self::ValueCount => "the counts of live values are the pointers the pages hold",
            Self::Directory => "the records of the directory agree with each other",
            Self::EntryRoot => "an entry names the cell and the key of its Merk's root",
            Self::EntryHost => "an entry names a node as the host of its Merk",
            Self::HostPointer => "a host points at the root its Merk's entry names",
            Self::HostedUnentered => "every hosted root has its entry in the directory",
            Self::HostUnnamed => "the entry of a hosted Merk names its host",
            Self::FlatRecordWithoutEntry => "a flat record stands under a prefix with an entry",
            Self::RecordKind => {
                "a flat record in the data of a Merk's subtree is a node that no node links"
            }
            Self::KeyHeldTwice => "a key of a subtree holds a node or a flat record, not both",
            Self::FlatTreeHost => {
                "a tree that keeps flat records hosts one Merk, in the first place"
            }
            Self::FlatPageFormat => "a flat page is well formed",
            Self::FlatDanglingPage => "a branch or a bucket names a flat page",
            Self::FlatPageNamedTwice => "a flat page is reached once",
            Self::FlatLevel => "the children of a branch are one level down",
            Self::FlatKeyOrder => "the keys of a flat tree are in order",
            Self::FlatBucket => "a bucket of long keys is the head, a zero byte and a page",
            Self::FlatPageUnreached => "every flat page is reached from a tree or a range delete",
            Self::FlatDropRecord => "the records of the range deletes are read",
            Self::IdNotLive => "a page of the latest version has a live id",
            Self::IdWithoutPage => "a live id names a page",
            Self::Chain => "a chain of the page table keeps the rules of chains",
            Self::Block => "every link of the page table leads to its block",
            Self::BlockNamedTwice => "a block is named by one link",
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// Where a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Place {
    /// The store as a whole.
    Store,
    /// A page.
    Page(PageId),
    /// A cell of a tree page.
    Cell(CellAddress),
    /// The entry in position `slot` of a flat leaf.
    FlatEntry {
        /// The leaf.
        page: PageId,
        /// The entry's position.
        slot: usize,
    },
    /// The Merk entered under a prefix.
    Merk(Prefix),
    /// A block of a segment.
    Block(BlockLocation),
    /// A value-log file.
    ValueFile(FileId),
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store => f.write_str("the store"),
            Self::Page(page) => write!(f, "page {page}"),
            Self::Cell(address) => write!(f, "cell {address}"),
            Self::FlatEntry { page, slot } => write!(f, "entry {slot} of flat page {page}"),
            Self::Merk(prefix) => {
                f.write_str("Merk ")?;
                prefix.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
            }
            Self::Block(location) => write!(
                f,
                "block at {} of segment {}, {} bytes",
                location.offset, location.file, location.len
            ),
            Self::ValueFile(file) => write!(f, "value-log file {file}"),
        }
    }
}

/// One thing the checker found wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The rule that does not hold.
    pub rule: Rule,
    /// Where.
    pub place: Place,
    /// What was found there.
    pub detail: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({})", self.place, self.detail, self.rule)
    }
}

/// The findings of a run, up to a limit.
#[derive(Debug)]
pub(super) struct Findings {
    pub(super) list: Vec<Finding>,
    pub(super) dropped: u64,
    limit: usize,
}

impl Findings {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            list: Vec::new(),
            dropped: 0,
            limit,
        }
    }

    /// The findings so far, those above the limit included.
    pub(super) fn count(&self) -> u64 {
        self.list.len() as u64 + self.dropped
    }

    /// Adds a finding. `detail` is made only if the finding is kept.
    pub(super) fn add(&mut self, rule: Rule, place: Place, detail: impl FnOnce() -> String) {
        if self.list.len() < self.limit {
            let detail = detail();
            self.list.push(Finding {
                rule,
                place,
                detail,
            });
        } else {
            self.dropped += 1;
        }
    }
}
