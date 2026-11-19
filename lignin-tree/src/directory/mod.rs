//! The subtree directory: where the root of each Merk is.
//!
//! - [`Directory`] is the directory of one store: a handle on the flat
//!   keyspace its records are in ([`flat`](crate::flat)), given the pages
//!   of a reader or of a write transaction with every call.
//! - [`Entry`] is what it holds of a Merk: the address of the root's cell,
//!   the root's key, the tree type, and the element that hosts the Merk
//!   ([`Host`]).
//! - [`RootChange`] is how a write states the roots it changed, and
//!   [`Dropped`] what a dropped Merk leaves for the reclaimer.
//! - [`record`] has the keys and the bytes of the records.
//!
//! The directory knows no path, no hash and no element. A Merk is named by
//! its [`Prefix`], the 32 bytes that GroveDB derives from the path, and
//! the tree type is bytes that are given and returned. What it holds is no
//! record of the state (`transactions.md` ST3): it follows from the tree
//! pages and the elements, and it is written through the flat pages of the
//! write transaction that changes them, so it is in that transaction's
//! commit and in no other.
//!
//! ```
//! use std::sync::Arc;
//!
//! use lignin_format::{manifest::Manifest, CellAddress, CommitVersion, PageId, PageSize, SlotId};
//! use lignin_io::{StdVfs, Vfs};
//! use lignin_store::{
//!     alloc::PageAllocator,
//!     cache::{BufferPool, OwnerId, PoolConfig},
//!     load::PageLoader,
//!     segment::{ManifestStore, SegmentStore},
//!     table::PageTable,
//!     values::ValueStore,
//! };
//! use lignin_tree::{
//!     directory::{Directory, Host, Root, RootChange},
//!     flat::{Committed, FlatConfig, Private},
//!     OperationGuard,
//! };
//!
//! struct Guard;
//! impl OperationGuard for Guard {}
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
//! let size = PageSize::DEFAULT;
//! let empty = Manifest::new(size, Default::default(), None, Vec::new())?;
//! let manifest = ManifestStore::create(vfs, dir.path(), empty)?;
//! let segments = SegmentStore::open(&manifest)?;
//! let values = ValueStore::open(&manifest, &[])?;
//! let (table, ids) = (PageTable::new(), PageAllocator::new());
//! let pool = BufferPool::new(PoolConfig {
//!     page_size: size,
//!     capacity: 8 << 20,
//! })?;
//! let loader = PageLoader::new(&pool, &table, &segments, size)?;
//! let guard = Guard;
//! let base = Committed::new(loader, &values, &guard, CommitVersion::EMPTY);
//! let mut pages = Private::new(base, &pool, OwnerId::new(1), &ids);
//!
//! // The Merk at the root of the grove, and a Merk that the element
//! // `tokens` of it hosts.
//! let (grove, tokens) = ([0; 32], [7; 32]);
//! let mut directory = Directory::create(&mut pages, FlatConfig::new(size))?;
//! directory.add(&mut pages, &grove, b"tree", None)?;
//! let cell = |page, slot| Some(CellAddress::new(PageId::new(page)?, SlotId::new(slot)?));
//! let root = Root {
//!     address: cell(3, 1).expect("an address"),
//!     key: b"tokens",
//! };
//! let named = RootChange::Named {
//!     prefix: &grove,
//!     root: Some(root),
//! };
//! directory.set_roots(&mut pages, &[named])?;
//! let host = Host {
//!     parent: &grove,
//!     key: b"tokens",
//!     position: 0,
//! };
//! directory.add(&mut pages, &tokens, b"sum tree", Some(host))?;
//!
//! // The open by prefix: one lookup.
//! let entry = directory.open(&pages, &tokens)?.expect("the Merk");
//! assert_eq!((entry.root(), entry.tree_type()), (None, &b"sum tree"[..]));
//! assert_eq!(entry.host(), Some(host));
//!
//! // A dropped Merk takes the Merks below it out of every lookup, and
//! // writes nothing of theirs.
//! directory.unlink(&mut pages, &grove)?;
//! assert!(directory.open(&pages, &tokens)?.is_none());
//! assert!(directory.stored(&pages, &tokens)?.is_some());
//! # Ok(())
//! # }
//! ```
//!
//! # The records
//!
//! The directory is one flat keyspace, and its records are of five kinds,
//! told apart by the first byte of the key ([`RecordKind`]).
//!
//! | Record | Key | Value |
//! | --- | --- | --- |
//! | entry | the Merk's prefix | [`Entry`] |
//! | child | the id of the Merk the host is in, the position among the host's Merks, the host's key | the hosted Merk's prefix and id |
//! | root | the address of a cell | the prefix of the Merk whose root the cell is |
//! | dropped | the id of a dropped or cleared Merk | the address its root had |
//! | header | none | the next id to give out |
//!
//! The entry is what an open reads. The other records lead to entries
//! from where the prefix is not known:
//!
//! - **The child records are the Merks below a Merk.** A path hash says
//!   nothing about the hashes of the paths below it, so the Merks nested
//!   in a Merk are found through its child records and through no scan:
//!   by the reclaimer of a dropped subtree (TREE-09), by a move
//!   ([`Directory::rekey`]) and by the invariant checker.
//! - **The root records find a Merk by its root.** A write carries the
//!   cells of Merks it never opened: a fragment travels with its root, and
//!   a small hosted Merk with its host (`tree-layout.md` P5). The report
//!   of the write names such a root by the address it had, and a cell does
//!   not say which Merk it is of (T1). So the directory is asked by the
//!   address ([`RootChange::Carried`]).
//!
//! No key is longer than 265 bytes, so none is the bucket of a long key,
//! on any page size.
//!
//! # Ids, and how a lookup tells a left-over entry
//!
//! A drop writes one record of the directory's and reads none of the Merks
//! below the dropped one, however many there are (`tree-layout.md` D1,
//! D2). Their entries stay until the reclaimer has come, and from the
//! drop's commit on no open returns one of them, by prefix or by path
//! (`transactions.md` CV9). A lookup knows by two things.
//!
//! **Every Merk has an id** ([`MerkId`]), which no other Merk has had or
//! will have. A Merk gets its id when it is entered, keeps it when it is
//! moved, and gets another when its nodes are cleared: the id stands for
//! the nodes the Merk holds, and for the Merks they host. **An entry holds the ids
//! of the Merks its Merk is below**, from the outermost to the Merk of its
//! host. That is eight bytes for each level of the path.
//!
//! **A dropped Merk leaves its id** in a record of its own, which holds
//! the address its root had, for the walk of its cells
//! (`tree-layout.md` D2). So does a Merk whose nodes are cleared: a Merk
//! with no node has nothing to clear, and keeps its id. A handle has the ids that are left
//! ([`Directory::load`]), and an entry is live if none of the ids in it is
//! among them ([`Directory::is_live`]). So:
//!
//! - an open is one lookup, of the entry, and nothing else is read for it;
//! - a Merk that is entered again under the prefix of a left-over entry
//!   takes the entry's place and has another id, so it starts with no
//!   root, and what is entered below it afterwards is live;
//! - a left-over entry that was not replaced stays out of every lookup,
//!   whether or not a Merk is entered again under the prefix of the
//!   dropped one above it;
//! - a snapshot that was pinned before the drop reads the pages of its
//!   version: they have every entry and no record of the drop, so it opens
//!   the whole subtree for as long as it is held.
//!
//! The ids come from a count in the header, which a write reads and
//! writes when it enters or clears a Merk.
//!
//! # A handle is of the pages it was loaded from
//!
//! [`Directory::load`] reads the dropped Merks' records, one walk that
//! finds nothing in a store with nothing to reclaim. A handle then answers
//! for those pages, and for what it wrote to them itself. It is loaded
//! again:
//!
//! - for a reader of another version, if a drop or a clear was committed,
//!   or what one left was reclaimed, between the two. An engine keeps the
//!   handle of the latest version and gives each reader a copy, which
//!   shares the ids;
//! - for a write transaction after a commit of another landed
//!   (`transactions.md` WT7), and after it rolled back to a savepoint or
//!   rolled a scope back.
//!
//! An id that a handle still has after the reclaimer took the record does
//! no harm: no entry holds it any more.
//!
//! # What a write does
//!
//! Every call that writes is given the flat pages of its write
//! transaction, and belongs to the operation whose change it states. A
//! call that is refused has changed no page ([`DirectoryError`]).
//!
//! - **A Merk is entered** ([`Directory::add`]) with the element that
//!   hosts it, when that element is written. It has no root then.
//! - **The roots a write changed are stated together**
//!   ([`Directory::set_roots`]), from the report of the write
//!   ([`fragment::Report`](crate::fragment::Report)): the Merks of the
//!   write by their prefix, with the address the report gives, and every
//!   other carried root by the address it had. This is rule S3 of
//!   `tree-layout.md` for the directory: the entry names the root's cell
//!   after every write.
//! - **The entry belongs to its Merk's unit** (`transactions.md` SC2): the
//!   scope that writes the Merk states its root, in the same operation.
//!   The entry is therefore exact while the pointer in the host's cell is
//!   behind, and it is the address to open the Merk by while the host's
//!   element is marked pending (`tree-layout.md` T3, SC9). An open never
//!   reads the host: [`Entry::host`] is the subtree and the key that such
//!   a mark is kept under.
//! - **A Merk is dropped** ([`Directory::unlink`]) or **cleared**
//!   ([`Directory::clear`]) in the operation that names its root as
//!   dropped to the tree pages
//!   ([`Write::dropped`](crate::fragment::Write::dropped)).
//! - **A Merk is moved** ([`Directory::rekey`]) with every Merk below it:
//!   one entry for each is written under its new prefix, and no cell is
//!   read (`tree-layout.md` D5).
//!
//! # The reclaimer
//!
//! What a dropped Merk left in the directory is found from its record
//! ([`Directory::drops`]) through the child records under its id
//! ([`Directory::children`]), and under the ids of the Merks found there.
//! These are also the prefixes and the ids of everything that was nested
//! in the subtree, a Merk or not, for whatever else is kept by them.
//!
//! The records go in this order, and each step leaves a directory that
//! every reader reads right:
//!
//! 1. for a Merk that was below, after the Merks below it: the record of
//!    its root, its entry, the record of its host
//!    ([`Directory::left_by`], [`Directory::reclaim_child`]);
//! 2. the record of the dropped Merk, last ([`Directory::reclaimed`]).
//!
//! **The records of a Merk go before its root's cell does.** A cell that
//! is gone leaves its slot to the next cell, which may be a root, and no
//! two Merks have a record for one address.
//!
//! The calls here are writes of a flat keyspace. A reclaimer that takes a
//! record out in a page step (`transactions.md` CL6) takes the same keys.
//!
//! # What the callers do
//!
//! - **Every subtree that a node hosts is entered**, a Merk with no node
//!   and a tree that keeps its data elsewhere among them: what is not
//!   entered is not found below a dropped Merk.
//! - **A Merk that loses its last node has no Merk below it**: each was
//!   dropped by the operation that deleted its host.
//!   [`Directory::unlink`] refuses a Merk with no root that has one, and
//!   [`Directory::check`] reports one.
//! - **A write names every root it changed.** A root that the directory
//!   does not have right is not found by the address the next write
//!   reports.
//! - **One writer.** The calls of one write transaction are made one after
//!   the other, as for any flat keyspace.
//!
//! # Choices made here that the plan left open
//!
//! 1. **The back pointer is the host's subtree and key**, not a cell
//!    address: `tree-layout.md` C5 allows no third stored address of a
//!    cell, and a host that is carried changes no entry. With it an entry
//!    holds the position of its Merk among the host's.
//! 2. **An entry is found by its root's address as well**, through a
//!    record of its own. A root that moves writes its entry, takes one
//!    root record out and puts one in. The root records of the roots in
//!    one page are beside each other, so a fragment that takes many roots
//!    along writes few pages for them.
//! 3. **A lookup knows a left-over entry by the ids above it**, which
//!    every entry holds, and by the ids of the dropped Merks, which a
//!    handle holds. The alternatives read more for an open: the entry of
//!    every Merk above, or the records of the drops.
//! 4. **A drop always leaves a record**, also for a Merk with no node,
//!    which leaves nothing in the tree pages: the engine may keep other
//!    data of a subtree by its prefix or its id, and the record is what
//!    says that it waits to be reclaimed. Only the ids of the Merks that
//!    had a root are held by a handle.
//! 5. **A clear of a Merk with nodes gives it another id**, so what is
//!    kept under the id of a Merk is not kept across a clear.
//! 6. **The tree type is opaque**, up to 255 bytes.
//! 7. **The entry's bytes are defined here**, above the flat page format,
//!    as the bucket of a long key is. They are part of format version 1.

mod check;
mod error;
mod read;
mod reclaim;
pub mod record;
mod rekey;
mod write;

#[cfg(test)]
mod tests;

use std::{collections::BTreeSet, sync::Arc};

use lignin_format::PageId;

pub use check::Census;
pub use error::{DirectoryError, Malformed};
pub use read::{Children, Drops, Entries};
pub use record::{Child, Dropped, Entry, Host, MerkId, Prefix, RecordKind, Root};
pub use rekey::Step;
pub use write::RootChange;

use crate::flat::{FlatConfig, FlatPages, FlatPagesMut, FlatTree};

/// The subtree directory of a store.
///
/// It is the root page of its keyspace and the ids of the dropped Merks
/// whose records are not reclaimed. It holds no page. A copy shares the
/// ids until one of the two changes them. See the
/// [module documentation](self).
#[derive(Debug, Clone)]
pub struct Directory {
    tree: FlatTree,
    dropped: Arc<BTreeSet<MerkId>>,
}

impl Directory {
    /// Makes an empty directory: a keyspace with the header.
    pub fn create<P: FlatPagesMut + ?Sized>(
        pages: &mut P,
        config: FlatConfig,
    ) -> Result<Self, DirectoryError> {
        let tree = FlatTree::create(pages, config)?;
        let header = record::encode_header(MerkId::FIRST);
        tree.put(pages, &record::HEADER_KEY, &header)?;
        Ok(Self {
            tree,
            dropped: Arc::default(),
        })
    }

    /// The directory whose keyspace has the root page `root`, as `pages`
    /// have it: with the Merks that are dropped there and not reclaimed.
    pub fn load<P: FlatPages + ?Sized>(
        pages: &P,
        root: PageId,
        config: FlatConfig,
    ) -> Result<Self, DirectoryError> {
        let mut directory = Self {
            tree: FlatTree::open(root, config),
            dropped: Arc::default(),
        };
        let mut dropped = BTreeSet::new();
        let mut drops = directory.drops();
        while let Some(drop) = drops.next(pages)? {
            if drop.root.is_some() {
                dropped.insert(drop.id);
            }
        }
        directory.dropped = Arc::new(dropped);
        Ok(directory)
    }

    /// The root page of the directory's keyspace: what a store keeps to
    /// find its directory again. It is the same page for good.
    pub const fn root_page(&self) -> PageId {
        self.tree.root()
    }

    /// The keyspace the records are in.
    pub const fn tree(&self) -> &FlatTree {
        &self.tree
    }
}
