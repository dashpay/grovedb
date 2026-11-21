//! What a commit that was made reports.

use lignin_format::{wal::LogPlace, CommitVersion, PageId};
use lignin_store::{table::LogDurable, values::ValueStoreError};
use lignin_tree::directory::{Directory, Dropped};

/// How a commit ended that reported no error.
#[derive(Debug)]
pub enum Committed {
    /// The write set was empty: nothing was appended and no version was
    /// taken (`transactions.md` CV2).
    Empty,
    /// The commit is published.
    Published(Box<Published>),
}

impl Committed {
    /// The version the commit took, if it took one.
    pub fn version(&self) -> Option<CommitVersion> {
        match self {
            Self::Empty => None,
            Self::Published(published) => Some(published.version),
        }
    }

    /// The commit, if it took a version.
    pub fn published(&self) -> Option<&Published> {
        match self {
            Self::Empty => None,
            Self::Published(published) => Some(published),
        }
    }
}

/// One commit that was published: its version, where its group is in the
/// log, and what it did to the pages of the store.
#[derive(Debug)]
pub struct Published {
    /// The version of the commit, which is the latest version from its
    /// publication on (CM4).
    pub version: CommitVersion,
    /// Where the group of the commit begins in the log.
    pub at: LogPlace,
    /// Where it ends: the end of the log when it was appended.
    pub end: LogPlace,
    /// How long the group is, in bytes.
    pub group_len: u64,
    /// How far the log was durable after the sync that the sync policy
    /// asked of this commit, and `None` if it asked for none (CM7, DU1).
    pub durable: Option<LogDurable>,
    /// The pages the commit made, in ascending order: their ids were
    /// published with it (ID1).
    pub created: Vec<PageId>,
    /// How many pages that were there before got a new version.
    pub changed: usize,
    /// The pages the commit freed, in ascending order (ID2).
    pub freed: Vec<PageId>,
    /// How many of the freed pages no segment ever held under their id's
    /// present life: the page table had the free link of an earlier page
    /// of that id as its newest, and was given no second one.
    pub freed_unlinked: usize,
    /// How many private copies of tree pages the commit was handed.
    pub tree_copies: usize,
    /// How many of them were as their pages were, and were discarded: a
    /// write plans on a copy of every page it reads
    /// (`lignin_tree::fragment`, "One writer").
    pub unchanged_tree: usize,
    /// How many private copies of flat pages were as their pages were, and
    /// were discarded: a flat write copies a page before it concludes
    /// anything from it (`lignin_tree::flat`).
    pub unchanged_flat: usize,
    /// How many tree pages have cells whose values the group carries: new
    /// values out of line, and those of cells that were carried to another
    /// page (FL7).
    pub value_pages: usize,
    /// The bytes of the value records of the group, of tree cells and of
    /// flat entries.
    pub value_bytes: u64,
    /// How many value pointers the commit's cells stopped holding, of
    /// those a count of live values included (PV6). A pointer that a load
    /// of the same transaction gave a cell is not among them.
    pub released: usize,
    /// How many value pointers the commit's cells started to hold: the
    /// values that its loads put into the value log and that its cells
    /// hold (`log-format.md` LG16). Zero for a commit with no load.
    pub acquired: usize,
    /// How many nodes had a stale hash that the commit evaluated (CM3
    /// step 5).
    pub evaluated: usize,
    /// What each Merk that the commit dropped or cleared left behind: the
    /// reclaimer's work, which may begin when no pin is below
    /// [`Self::version`] (CL6).
    pub dropped: Vec<Dropped>,
    /// The subtree directory of [`Self::version`]
    /// (`lignin_tree::directory`, "A handle is of the pages it was loaded
    /// from").
    pub directory: Directory,
    /// The first value pointer that the counts of live values could not
    /// take as the commit released or acquired it. The whole change was
    /// applied all the same, and the commit went on
    /// ([`ValueStore::apply`](lignin_store::values::ValueStore::apply)).
    pub miscounted: Option<ValueStoreError>,
}
