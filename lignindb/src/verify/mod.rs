//! The physical invariant checker: whether the pages of a store are what
//! the layout says they are.
//!
//! - [`check`] reads one version of a [`Store`] and returns a [`Report`].
//! - A [`Finding`] is one [`Rule`] that does not hold, at one [`Place`].
//!   A store with no finding is sound ([`Report::is_sound`]).
//! - [`Unreached`] counts the cells that nothing reaches. They are space
//!   and no state, and no finding.
//! - [`Census`] is what the run counted, and [`Phases`] how long it took.
//!
//! The checker changes nothing and takes no lock of the store. It reads as
//! any reader does: pages through the store's loader at the version it is
//! given, under the operation guard of its caller. It knows no Merk rule:
//! the functions that make a hash are given to it
//! ([`HashRules`](lignin_tree::hash::HashRules)), as they are to the tree
//! layer.
//!
//! No example runs here: a [`Store`] is made of the structures of an open
//! store, which the engine's open makes (TXN-07). The tests make one over
//! the store of this crate's tests (`txn::testing`), and that store runs
//! the checker when it is dropped in a debug build.
//!
//! # What is checked
//!
//! Every page of the version is read once, whether or not anything reaches
//! it. Then the trees are walked from their roots: each flat keyspace from
//! its root page, and each Merk from the root that the subtree directory
//! names. What the walks did not reach is looked at last.
//!
//! | Rule | Specified in | Findings |
//! | --- | --- | --- |
//! | A page is read, is well formed and holds its own id, flat pages too. | `tree-page.md` TP2, `flat-page.md` P14 | [`Rule::PageUnreadable`], [`Rule::TreePageFormat`], [`Rule::FlatPageFormat`], [`Rule::PageId`] |
//! | Every node has one cell: every cell of a Merk in use is reached once from its root. | `tree-layout.md` C1 | [`Rule::CellNamedTwice`] |
//! | Every stored address names the cell it means: a slot with a cell, whose key is in its place in the order of its Merk, has the length its link states, and has the heights its link states. | S1, S3, C2 | [`Rule::DanglingAddress`], [`Rule::KeyOrder`], [`Rule::LinkKeyLength`], [`Rule::LinkHeights`] |
//! | The fragment-root table equals what the links imply. | F1, F3 | [`Rule::Fragments`] |
//! | No cell has two stored addresses besides the directory. | C5 | [`Rule::CellNamedTwice`], [`Rule::Fragments`] |
//! | Every stored hash equals its recomputation, the value hash among them. | `lignin_tree::hash::check` | [`Rule::Hash`], [`Rule::HashUnchecked`] |
//! | Every value pointer leads to a record of its length that names the page of its cell as the owner, and no two cells hold one pointer. A pending value has its bytes. This holds for every cell and every flat record of the version: in use, left by a dropped Merk or a range delete, or reached by nothing. | `value-log-format.md` O2, O5; X1 | [`Rule::ValueUnreadable`], [`Rule::ValueOwner`], [`Rule::ValueShared`], [`Rule::PendingBytes`] |
//! | The directory's records agree with each other. | `lignin_tree::directory` | [`Rule::Directory`] |
//! | Every Merk in use has an entry that agrees with the cells: the entry names the root's cell and key, the host is a node of the Merk it is said to be in, and the host's pointer is the entry's root. No pointer of a host in use is without its entry. | plan, draft 17 | [`Rule::EntryRoot`], [`Rule::EntryHost`], [`Rule::HostPointer`], [`Rule::HostedUnentered`], [`Rule::HostUnnamed`] |
//! | The flat trees are trees: every page is reached once, by one tree, a child is one level below its branch, and the keys are in order between the separators above them. A bucket of long keys is followed into its tree. | `flat-page.md` P6, P8; TREE-07 | [`Rule::FlatDanglingPage`], [`Rule::FlatPageNamedTwice`], [`Rule::FlatLevel`], [`Rule::FlatKeyOrder`], [`Rule::FlatBucket`], [`Rule::FlatPageUnreached`] |
//! | No flat record stands under a prefix that the directory has no entry for. | `tree-layout.md` D6 | [`Rule::FlatRecordWithoutEntry`] |
//! | A page that a lazy range delete left is a flat page that no tree reaches. | `lignin_tree::drop` | [`Rule::FlatDropRecord`], [`Rule::FlatPageNamedTwice`] |
//!
//! Where the caller vouches that the store is still ([`Scope::Still`]),
//! what the store keeps beside its pages is compared with them:
//!
//! | Rule | Specified in | Findings |
//! | --- | --- | --- |
//! | The live page ids are exactly the pages of the latest version. | `page-alloc.md` | [`Rule::IdNotLive`], [`Rule::IdWithoutPage`] |
//! | The counts of live values are the pointers the pages hold. | `value-log-format.md` O6 | [`Rule::ValueCount`] |
//! | Every link of the page table leads to its block, a chain keeps the rules of chains, and no block is named twice. | `page-table.md` | [`Rule::Chain`], [`Rule::Block`], [`Rule::BlockNamedTwice`] |
//! | No page version that a segment makes durable holds a pending value: no base image, and no page after a run. | `page-delta.md` PD16 | [`Rule::PendingInSegment`] |
//! | Where a record placed a value, the pointer has the pending value's length. | `page-delta.md`, record 21 | [`Rule::PlacedLength`] |
//!
//! # Cells that nothing reaches
//!
//! The record of a dropped Merk goes before its cells do, so that no walk
//! begins a second time (`tree-layout.md` D2). While its cells go, and for
//! good after a crash at that time, the rest of them are in their pages
//! with nothing that leads to them. They are in no state and no read
//! returns them. Only a walk of every page finds them, and this is that
//! walk: [`Unreached`] says how many there are, how much room they take and
//! where the first of them are. That nothing reaches a cell is no finding,
//! because nothing in the pages tells such a cell from what a reclaiming
//! leaves while it runs.
//!
//! What such a cell holds can still be a finding, as for any other cell:
//! one that names a cell in use gives that cell a second stored address
//! ([`Rule::CellNamedTwice`]), and a value pointer in it is read like every
//! other.
//!
//! The cells that a dropped Merk left and that its record still leads to
//! are walked as a tree of their own. Their addresses, keys and fragments
//! are checked, and their hashes are not: they are no state.
//!
//! **Every value out of line is read**, whatever holds its place: a cell in
//! use, a cell that a dropped Merk left, a cell that nothing reaches, and a
//! record of any flat leaf of the version, in a keyspace, left by a range
//! delete, or reached by nothing. The reclaimer releases the pointer it
//! finds in what it takes out, and a pointer that nothing reaches stays
//! counted as live.
//!
//! # What a page that is not read costs
//!
//! A page that is not read, or is not well formed, is a finding, and no
//! walk enters it. Most rules are of one page, one cell or one pointer,
//! and are checked wherever the page that holds it is read. A few say
//! that something is *not* there, and those need every page that could
//! hold it. Where a walk was pointed at a page and did not read it, they
//! are not reported, because the page may hold what is missed:
//!
//! - **A flat page that no walk reached** ([`Rule::FlatPageUnreached`]):
//!   the page that is not read may be the branch above it, and a record of
//!   a range delete that is not read may name it.
//! - **A host that is no node of its Merk** ([`Rule::EntryHost`]): not
//!   said of a Merk whose walk did not enter every cell it was pointed at.
//! - **A cell that nothing reaches and that names a cell in use**
//!   ([`Rule::CellNamedTwice`]): the cell may be below the page that is
//!   not read, and be the parent or the host of the cell it names. The
//!   cells are still counted, and [`Unreached::uncertain`] says that not
//!   all of them are known to be space.
//! - **The counts of live values** ([`Rule::ValueCount`]): not compared
//!   while any page of the version is not read, since it may hold
//!   pointers.
//! - **The directory**: with a keyspace that is no tree, or records that
//!   are not all read, no Merk is walked and no flat record is compared
//!   with its entries.
//!
//! So one page that is not read gives the finding of that page, and the
//! findings of what was read; the rest waits for a run over a store whose
//! pages are all read. A pointer that was read and names the wrong place
//! is another matter: what it should have named is reached by no reader
//! either, and is reported as it is found.
//!
//! # What is not checked
//!
//! - **The policy of the layout** (`tree-layout.md`, sections 5 to 8, and
//!   F5): where a cell is, how full a page is, how large a fragment is.
//!   Any placement is valid.
//! - **The aggregate in a link, and whether a Merk is balanced.** They are
//!   Merk's rules, and nothing here knows them. A link's heights are
//!   compared with the links of the child, which is the page's own
//!   statement twice.
//! - **The prefix of a flat page.** A write gives a page the longest prefix
//!   its keys share, and a page step that takes entries out leaves the
//!   prefix as it was (`lignin_tree::drop`), so a shorter one is valid.
//! - **The log.** A version is checked as the pages have it, in the buffer
//!   pool or in segments. Whether the log leads to it is recovery's
//!   (STORE-07, VER-03).
//! - **Dead space in segments and value-log files.** [`Census::segments`]
//!   and [`Census::values`] say what the pages name of each file. What the
//!   cleaner keeps of that is its own (STORE-08).
//!
//! # What the callers do
//!
//! - **The caller holds an operation guard** for the whole run, and gives
//!   a version it has pinned, or the latest version read under the guard
//!   (`transactions.md` PN5). A run reads every page, so it is one long
//!   operation: it holds back the reuse of page ids and the deletion of
//!   files for as long as it takes.
//! - **[`Scope::Still`] is a promise.** Nothing commits, no page step is
//!   made and no write-back runs while the check does, the version is the
//!   latest one, and no write transaction holds a page id. A database that
//!   was just opened, a checkpoint, and a store at the end of a test are
//!   still. Under [`Scope::Reader`] the run asks nothing of the store that
//!   a reader does not ask, and can run beside writers.
//! - **The rules that make a hash answer for every node.** A value hash
//!   that no rule can make from the pages, as a reference's is, is the
//!   rules' to return as it is stored. A rule that refuses is a finding
//!   ([`Rule::HashUnchecked`]). With no rules, no hash is recomputed.
//! - **No reclaimer of dropped subtrees works under a run.** A step of the
//!   reclaimer takes cells and records out of the pages of the version
//!   itself (`lignin_tree::drop`), so a run beside one can read the record
//!   of a dropped Merk and then find its cells gone, and reports that.
//!   Whoever runs background work holds the reclaimer back for the run.
//!   Write-back may run under [`Scope::Reader`]: what a run reads of one
//!   cell, it reads from one frame.
//! - **The pool has room for a few pages beside what is pinned.** The run
//!   holds the pages of the step it is in, and a few of the steps before.
//!
//! # Choices made here that the plan left open
//!
//! 1. **A run returns every finding**, up to a limit, and does not stop at
//!    the first. One damage can give several findings: a link that names
//!    the wrong cell is a key out of order, a cell named twice and a hash
//!    that differs.
//! 2. **The store is described by its parts** ([`Store`]), because the
//!    engine's facade does not exist yet (TXN-07). The facade and the
//!    `lignin` command (OPS-01) make a [`Store`] of an open database.
//! 3. **A store with no directory is checked from the roots it is given**
//!    ([`Grove::Roots`]): the pages a bulk load wrote (TREE-10) are a
//!    grove before any directory exists.
//! 4. **Two scopes** ([`Scope`]). What a reader can see is checked at any
//!    version beside writers. The page ids, the counts of live values and
//!    the page table are compared with the pages only where the caller
//!    says the store is still, since each of them is of the latest moment
//!    and not of a version.
//! 5. **Cells that nothing reaches are reported apart**, and so are tree
//!    pages with no cell.
//! 6. **The heights in a link are checked against the child's links, and
//!    the aggregate is not checked.** The first needs no rule of Merk. The
//!    second needs Merk's rule for each feature type, which the hash rules
//!    do not carry.

mod finding;
mod flat;
mod grove;
mod pages;
mod read;
mod report;
mod still;

#[cfg(all(test, not(loom)))]
mod tests;

use std::{
    collections::{BTreeMap, HashMap},
    time::Instant,
};

use lignin_format::{
    value_log::ValuePointer, CellAddress, CommitVersion, FileId, PageId, PageSize,
};
use lignin_store::{
    alloc::PageAllocator,
    cache::PageRef,
    load::{LoadError, PageLoader},
    segment::SegmentStore,
    table::PageTable,
    values::ValueStore,
};
use lignin_tree::{
    drop::{Column, FlatDrops},
    flat::FlatTree,
    hash::HashRules,
    node::Lease,
    OperationGuard,
};

use finding::Findings;
pub use finding::{Finding, Place, Rule};
pub use report::{Census, Phases, Report, SegmentUse, Unreached, ValueUse};

use self::{
    pages::Seen,
    read::{Frames, Shared, TreeRead, ValueRead},
};

/// The structures of one open store, as the checker reads them. The caller
/// vouches that all of them are of one store.
#[derive(Debug, Clone, Copy)]
pub struct Store<'s, 'c> {
    /// The read path of the store: its buffer pool over its page table and
    /// its segments.
    pub loader: PageLoader<'s>,
    /// The page table that `loader` reads.
    pub table: &'s PageTable,
    /// The segments that `loader` reads.
    pub segments: &'s SegmentStore,
    /// The value log.
    pub values: &'s ValueStore,
    /// The size of the store's pages.
    pub page_size: PageSize,
    /// The page id allocator, if the store has one. With none, the pages of
    /// the version are those the page table has a link for.
    pub ids: Option<&'s PageAllocator>,
    /// Where the Merks are found from.
    pub grove: Grove<'c>,
    /// The keyspace of the pages that lazy range deletes left, if the store
    /// keeps one (`lignin_tree::drop::FlatDrops`).
    pub flat_drops: Option<FlatDrops>,
    /// Every other flat keyspace of the store: one that holds no record of
    /// a subtree.
    pub flat: &'c [FlatTree],
    /// Whether the store's counts of live values were kept by the change
    /// points of its pages (`lignin_store::values`). A store that was
    /// written beside them, as a bulk load writes its pages, has none to
    /// compare.
    pub values_counted: bool,
}

/// Where the Merks of a store are found from.
#[derive(Debug, Clone, Copy)]
pub enum Grove<'c> {
    /// The store has a subtree directory.
    Directory {
        /// The keyspace of the directory.
        tree: FlatTree,
        /// **Every** flat keyspace that holds records of subtrees, with
        /// what stands before a subtree's prefix in its keys.
        columns: &'c [Column<'c>],
    },
    /// The store has no directory. These are the root cells of the Merks
    /// that no element hosts, and every hosted-root pointer below them is
    /// followed.
    Roots(&'c [CellAddress]),
}

/// How much of a store a run may look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// What a reader of the version sees: the pages, the trees, the
    /// directory, the values. It may run beside writers.
    #[default]
    Reader,
    /// Also what the store keeps beside its pages: the page ids, the counts
    /// of live values, and the page table with its segments. The caller
    /// vouches that the store is still and that the version is the latest
    /// one (see the [module documentation](self)).
    Still,
}

/// What one run checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The version to check: one the caller has pinned, or the latest
    /// version read under its guard.
    pub at: CommitVersion,
    /// How much of the store the run may look at.
    pub scope: Scope,
    /// The most findings that are kept. The others are counted
    /// ([`Report::more_findings`]).
    pub max_findings: usize,
}

impl Options {
    /// The findings that are kept unless the caller says otherwise.
    pub const MAX_FINDINGS: usize = 1_000;

    /// A run at the version `at` with the scope `scope`.
    pub const fn new(at: CommitVersion, scope: Scope) -> Self {
        Self {
            at,
            scope,
            max_findings: Self::MAX_FINDINGS,
        }
    }
}

/// Checks the version `options.at` of `store` and returns what was found.
///
/// `guard` is the guard of the caller's operation, held for the whole run.
/// `rules` are the functions that make the hashes of the store's nodes;
/// with `None` no stored hash is recomputed. See the
/// [module documentation](self) for what is checked and what the caller
/// vouches for.
///
/// The run returns its findings and never fails: a page that cannot be
/// read is a finding, and what could only be reached through it is not
/// looked at. So with a directory whose keyspace is no tree, or whose
/// records are not all read, no Merk is walked and no flat record is
/// compared with its entries: the cells are then counted as reached by
/// nothing, beside the finding that says why.
pub fn check<G: OperationGuard + ?Sized>(
    store: &Store<'_, '_>,
    guard: &G,
    options: Options,
    mut rules: Option<&mut dyn HashRules>,
) -> Report {
    let mut run = Run::new(store, options);
    let mut phases = Phases::default();
    let mut started = Instant::now();
    let mut lap = |phase: &mut std::time::Duration| {
        *phase = started.elapsed();
        started = Instant::now();
    };

    run.read_every_page();
    lap(&mut phases.pages);
    let directory = run.walk_flat_trees(guard);
    lap(&mut phases.flat);
    run.walk_grove(directory.as_ref(), &mut rules);
    lap(&mut phases.merks);
    run.count_unreached();
    if run.still {
        run.compare_ids();
        run.compare_value_counts();
    }
    lap(&mut phases.census);
    if run.still {
        run.check_chains();
    }
    lap(&mut phases.segments);
    run.finish(phases)
}

/// The state of one run.
struct Run<'a, 's, 'c> {
    store: &'a Store<'s, 'c>,
    at: CommitVersion,
    still: bool,
    lease: Lease,
    /// The frames that the step of the run which is under way has read.
    frames: Shared<'s>,
    found: Findings,
    /// Every page of the version that was read, with what the walks
    /// reached of it.
    pages: BTreeMap<PageId, Seen>,
    /// The cell or the flat record that holds each value pointer.
    holders: HashMap<ValuePointer, Place>,
    /// The pointers into each value-log file: how many, and their bytes.
    files: BTreeMap<FileId, (u64, u64)>,
    /// The tree pages as a hash check reads them, and the values.
    tree: TreeRead<'s>,
    values: ValueRead<'s>,
    census: Census,
    unreached: Unreached,
    cut: Cut,
}

/// What a walk was pointed at and did not read. Each such place has its
/// finding. What needs every page below it is then not known, and is not
/// reported (see "What a page that is not read costs" in the
/// [module documentation](self)).
#[derive(Debug, Clone, Copy, Default)]
struct Cut {
    /// A walk of cells did not enter a page, or the directory was not read
    /// and no walk of cells began.
    cells: bool,
    /// A walk of flat pages did not enter a page, or the pages that range
    /// deletes left were not walked.
    flat: bool,
}

impl<'a, 's, 'c> Run<'a, 's, 'c> {
    fn new(store: &'a Store<'s, 'c>, options: Options) -> Self {
        let lease = Lease::take();
        let frames = Frames::new(store.loader, options.at);
        Self {
            store,
            at: options.at,
            still: options.scope == Scope::Still,
            lease,
            found: Findings::new(options.max_findings),
            pages: BTreeMap::new(),
            holders: HashMap::new(),
            files: BTreeMap::new(),
            tree: TreeRead::new(frames.clone(), lease),
            values: ValueRead::new(store.values, frames.clone()),
            frames,
            census: Census::default(),
            unreached: Unreached::default(),
            cut: Cut::default(),
        }
    }

    /// Begins a step of the run: one page of the census, or one cell or
    /// one page of a walk. What the step reads of a page, it reads from one
    /// frame ([`Frames`]).
    fn begin_step(&mut self) {
        self.frames.borrow_mut().begin();
        self.values.begin();
    }

    /// The page `page` as the version has it, pinned.
    fn frame(&mut self, page: PageId) -> Result<PageRef<'s>, LoadError> {
        self.frames.borrow_mut().get(page)
    }

    /// The page `page`, which the run read before, for a walk to enter.
    /// Where it is not read now, that is reported once: from then on it is
    /// a page that is not read, as if the run had never read it.
    fn enter(&mut self, page: PageId) -> Option<PageRef<'s>> {
        match self.frame(page) {
            Ok(frame) => Some(frame),
            Err(error) => {
                self.add(Rule::PageUnreadable, Place::Page(page), || {
                    error.to_string()
                });
                self.pages.insert(page, Seen::Bad);
                None
            }
        }
    }

    fn add(&mut self, rule: Rule, place: Place, detail: impl FnOnce() -> String) {
        self.found.add(rule, place, detail);
    }

    fn finish(mut self, phases: Phases) -> Report {
        self.census.values = (self.files.iter())
            .map(|(file, (values, bytes))| ValueUse {
                file: *file,
                values: *values,
                bytes: *bytes,
            })
            .collect();
        Report {
            findings: self.found.list,
            more_findings: self.found.dropped,
            unreached: self.unreached,
            census: self.census,
            phases,
        }
    }
}
