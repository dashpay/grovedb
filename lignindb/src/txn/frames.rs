//! The private frames of a write transaction, and how a batch is applied
//! to them by key.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

use lignin_format::{
    tree_page::{HashField, Side},
    CellAddress, CommitVersion,
};
use lignin_store::alloc::PageAllocator;
use lignin_tree::{
    directory::{
        Directory, DirectoryError, Dropped, Entry, Host, MerkId, Prefix, Rekey, RekeyPlan,
        Restated, Root, RootChange,
    },
    drop::{self, DropError, Flats, Leaving as Left},
    flat::Private,
    fragment::{
        FragmentError, Fragments, HostedWrite, LinkWrite, NodeWrite, Report, RootWrite, Target,
        ValueWrite, Write,
    },
    hash::{Hashes, Stale},
    node::{Lease, NodeError, NodeHandle},
    value::{TreeValues, ValueBuf, ValueSource},
};

use super::{
    error::{BatchError, Differs, HostName, TxnError},
    key::{Column, Subtree},
    pages::{find, follow, NodePages, TreePages},
    path::{FlatWriter, Idle, WritePath, IDLE},
    unlinked,
    write::{
        Applied, Batch, Entering, Hosted, LinkPut, MerkWrite, Moving, RecordWrite, RootPut,
        SubtreeWrite, ValuePut,
    },
};
use crate::snapshot::OperationGuard;

/// Why a batch was not applied to the frames.
#[derive(Debug)]
pub(crate) struct Refused {
    pub(crate) error: TxnError,
    /// Whether the frames may hold a part of the batch. They are then of no
    /// use, and are made again from the write set (WT3).
    pub(crate) dirty: bool,
}

impl Refused {
    /// A refusal that changed no page.
    fn clean(error: impl Into<TxnError>) -> Self {
        Self {
            error: error.into(),
            dirty: false,
        }
    }

    /// A failure after which the frames are in no state to go on from.
    fn dirty(error: impl Into<TxnError>) -> Self {
        Self {
            error: error.into(),
            dirty: true,
        }
    }

    /// A failure of a drop or of an enter (`lignin_tree::drop`), after the
    /// pages may have changed.
    fn dropping(error: DropError) -> Self {
        Self::dirty(match error {
            DropError::Directory(error) => TxnError::Directory(error),
            DropError::Flat(error) => TxnError::Flat(error),
            DropError::Fragment(error) => TxnError::Fragment(error),
            DropError::Format(error) => TxnError::Format(error),
            DropError::Load(error) => TxnError::Load(error),
            DropError::Values(error) => TxnError::Values(error),
            DropError::Damaged { what, .. } | DropError::Record(what) => TxnError::Damaged(what),
            // The numbers of the records of unlinked pages ran out, which
            // is no damage: it is said as the directory says it of the ids
            // it gives out.
            DropError::IdsExhausted => TxnError::Directory(DirectoryError::IdsExhausted),
            // A Merk that is named with one it is below is refused by the
            // frames before the drop is asked.
            DropError::Below { prefix } => {
                let subtree = Subtree::new(prefix);
                TxnError::Batch(BatchError::NotEntered { subtree })
            }
            // The others are of reclaiming, which no write makes.
            DropError::Store(_) | DropError::Broken { .. } | DropError::GivenUp => {
                TxnError::Damaged("an error of reclaiming, where nothing is reclaimed")
            }
        })
    }
}

/// Whether `error` is a refusal of the directory, which changes no page:
/// every error but a failed or a damaged flat keyspace
/// ([`DirectoryError`]).
fn refuses(error: &DirectoryError) -> bool {
    !matches!(
        error,
        DirectoryError::Flat(_) | DirectoryError::Damaged { .. }
    )
}

/// What the tests arm and count in the write of an operation, on their own
/// thread.
#[cfg(all(test, not(loom)))]
pub(crate) mod probe {
    use std::cell::Cell;

    use lignin_format::CellAddress;
    use lignin_tree::directory::RootChange;

    thread_local! {
        static PANIC: Cell<bool> = const { Cell::new(false) };
        static TAKEN: Cell<u64> = const { Cell::new(0) };
        static CHAINED: Cell<u64> = const { Cell::new(0) };
    }

    /// The next write panics once the tree pages have taken its batch,
    /// and a drop of the batch is made, before the subtrees of the batch
    /// are entered and the directory is told the roots.
    pub(crate) fn panic_next() {
        PANIC.set(true);
    }

    pub(super) fn strike() {
        assert!(!PANIC.replace(false), "the write was told to panic");
    }

    /// Counts what one write states of its roots.
    pub(super) fn count(changes: &[RootChange<'_>]) {
        let left = |address: CellAddress| {
            (changes.iter()).any(
                |change| matches!(change, RootChange::Carried { from, .. } if *from == address),
            )
        };
        for change in changes {
            match change {
                RootChange::Named {
                    root: Some(root), ..
                } if left(root.address) => TAKEN.set(TAKEN.get() + 1),
                RootChange::Carried { to, .. } if left(*to) => CHAINED.set(CHAINED.get() + 1),
                _ => {}
            }
        }
    }

    /// How many roots this thread's writes named in a cell that a carried
    /// root left in the same write, and how many carried roots came to
    /// such a cell.
    pub(crate) fn counts() -> (u64, u64) {
        (TAKEN.get(), CHAINED.get())
    }
}

/// The key a record of `subtree` is stored under in the flat tree of its
/// column: the subtree's 32 bytes, then the record's key.
pub(crate) fn stored_key(out: &mut Vec<u8>, subtree: &Subtree, key: &[u8]) {
    out.clear();
    out.extend_from_slice(subtree.as_bytes());
    out.extend_from_slice(key);
}

/// The first stored key after every key of `subtree`, or `None` if its keys
/// go to the end of the keyspace.
pub(crate) fn subtree_end(subtree: &Subtree) -> Option<Vec<u8>> {
    let mut end = subtree.as_bytes().to_vec();
    while let Some(last) = end.pop() {
        if last < u8::MAX {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

/// The private frames of a write transaction (`transactions.md` WT3, WT4):
/// its copies of tree pages and of flat pages, over the committed pages of
/// its base version, which it keeps pinned (PN1).
///
/// They are derived from the write set: applying the write set's
/// operations, in order, to the committed state of the base gives them.
/// Nothing is kept here that the write set does not say, so they can be
/// given up and made again at any time: for a rebase, a rollback to a
/// mark, or after an operation that failed half way.
///
/// They are bound to the guard of the operation that reads through them
/// (PN5): [`Awake`] in an operation, and [`Parked`] between two, where
/// nothing is read. The base pin keeps the version from one operation to
/// the next, which is what taking the flat pages along asks for.
pub(crate) struct Frames<'s: 'g, 'g, G: lignin_tree::OperationGuard> {
    base: CommitVersion,
    pub(crate) tree: TreePages<'s>,
    /// The values of the transaction's cells that are not in their cells,
    /// and where those are read from that are not in its memory.
    pub(crate) values: TreeValues,
    pub(crate) source: ValueSource<'s>,
    pub(crate) flat: FlatFrames<'s, 'g, G>,
    /// The subtree directory as these pages have it: a copy of the handle
    /// that is kept for their version, taken when the frames are made
    /// ([`WritePath::directory_at`]), and told every root, every Merk that
    /// is entered
    /// and every one that leaves by the writes that are applied to them
    /// (`lignin_tree::directory`). It is made again with them.
    pub(crate) directory: Directory,
    /// What each Merk that the transaction dropped or cleared left for the
    /// reclaimer.
    pub(crate) dropped: Vec<Dropped>,
    /// Which hashes of the frames' cells are stale: marked by the writes
    /// that left them so, and evaluated by the commit (`lignin_tree::hash`).
    /// They are marks of these pages, so they are made again with them.
    pub(crate) hashes: Hashes,
    fragments: Fragments,
    key: Vec<u8>,
    /// For each Merk of the batch that was applied last, whether it had no
    /// root before.
    empty: Vec<bool>,
    /// For the subtrees it was looked up for: whether the data column
    /// holds a record under the subtree, which for a Merk is an unlinked
    /// record (`super::unlinked`). It is of these pages, so it is made
    /// again with them.
    unlinked: HashMap<Subtree, bool>,
    /// The stored key of each unlinked record that a write applied to
    /// these frames left where it is: the transaction's own node under
    /// that key, whose value a later write of the node keeps. A record
    /// that the committed state left is not among them. A flat write
    /// meets none of them: the transaction writes no flat record into the
    /// data column of a subtree it has written a node into since it last
    /// dropped or cleared the subtree (ST1), and a drop and a clear take
    /// the subtree's keys out. The keys of a subtree that went with one
    /// above it stay until a subtree is entered under its prefix again or
    /// a move brings one there, and nothing is read or written under it
    /// until then. The keys of a Merk that is moved go with it to its
    /// new prefix.
    own: HashSet<Vec<u8>>,
    /// The subtrees that left with the moves of the batch that was applied
    /// last.
    left: Vec<Subtree>,
    /// For each Merk that a clear applied to these frames entered again
    /// under another id (`Directory::clear`), the id it had before every
    /// such clear, by the id it has since.
    renewed: HashMap<MerkId, MerkId>,
}

/// The most cells a search by key passes before the pages are taken for
/// damaged. A Merk of 2^64 nodes is at most 93 levels deep.
const MAX_DEPTH: usize = 128;

/// The frames of a transaction between two of its operations.
pub(crate) type Parked<'s> = Frames<'s, 's, Idle>;

/// The frames of a transaction in the operation that holds the guard.
pub(crate) type Awake<'s, 'g> = Frames<'s, 'g, OperationGuard<'s>>;

/// The flat pages of the frames. When they are dropped, their copies are
/// given up.
pub(crate) struct FlatFrames<'s: 'g, 'g, G: lignin_tree::OperationGuard>(
    Option<Private<'s, 'g, G, &'s PageAllocator>>,
);

impl<'s: 'g, 'g, G: lignin_tree::OperationGuard> FlatFrames<'s, 'g, G> {
    /// The same pages, with all they hold, bound to `guard`.
    fn under<'q, H>(mut self, guard: &'q H) -> FlatFrames<'s, 'q, H>
    where
        's: 'q,
        H: lignin_tree::OperationGuard,
    {
        FlatFrames(self.0.take().map(|writer| writer.under(guard)))
    }

    /// How many private copies of flat pages the transaction holds: of the
    /// pages it changed or made, and of those it freed.
    pub(crate) fn copies(&self) -> usize {
        (self.0.as_ref()).map_or(0, |writer| {
            writer.written().count() + writer.freed().count()
        })
    }
}

impl<'s> FlatFrames<'s, 's, Idle> {
    /// The flat pages for the commit, which logs them in the operation
    /// that holds `guard`. They are still given up when they are dropped.
    pub(crate) fn for_commit<'g>(
        self,
        guard: &'g OperationGuard<'s>,
    ) -> FlatFrames<'s, 'g, OperationGuard<'s>> {
        self.under(guard)
    }
}

impl<'s: 'g, 'g> FlatFrames<'s, 'g, OperationGuard<'s>> {
    /// Ends the flat pages and hands their copies to the commit, which
    /// installs them (`lignin_tree::flat`, "What a commit writes"). From
    /// here on nothing gives them up but the commit.
    pub(crate) fn into_parts(mut self) -> lignin_tree::flat::Parts<'s> {
        let writer = self.0.take();
        writer
            .expect("the writer is there until it is taken")
            .into_parts()
    }
}

impl<'s: 'g, 'g> std::ops::Deref for FlatFrames<'s, 'g, OperationGuard<'s>> {
    type Target = FlatWriter<'s, 'g>;

    fn deref(&self) -> &Self::Target {
        self.0
            .as_ref()
            .expect("the writer is there until it is taken")
    }
}

impl<'s: 'g, 'g> std::ops::DerefMut for FlatFrames<'s, 'g, OperationGuard<'s>> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
            .as_mut()
            .expect("the writer is there until it is taken")
    }
}

impl<'s: 'g, 'g, G: lignin_tree::OperationGuard> Drop for FlatFrames<'s, 'g, G> {
    fn drop(&mut self) {
        if let Some(writer) = &mut self.0 {
            // What the pool will not let go of now cannot be given up
            // later either: nothing is left that could.
            let _ = writer.roll_back();
        }
    }
}

/// The frames of a transaction, awake for one of its operations. When this
/// is dropped they are parked again where the transaction keeps them, on
/// every way out of the operation.
pub(crate) struct Woken<'a, 's: 'g, 'g> {
    home: &'a mut Option<Parked<'s>>,
    frames: Option<Awake<'s, 'g>>,
}

impl<'a, 's: 'g, 'g> Woken<'a, 's, 'g> {
    /// Takes the frames that `home` keeps, if it keeps any, for the
    /// operation that holds `guard`.
    pub(crate) fn wake(
        home: &'a mut Option<Parked<'s>>,
        guard: &'g OperationGuard<'s>,
    ) -> Option<Self> {
        let frames = home.take()?.under(guard);
        Some(Self {
            home,
            frames: Some(frames),
        })
    }

    /// Gives the frames up: they hold a part of a batch.
    pub(crate) fn discard(mut self) {
        self.frames = None;
    }
}

impl<'s: 'g, 'g> std::ops::Deref for Woken<'_, 's, 'g> {
    type Target = Awake<'s, 'g>;

    fn deref(&self) -> &Self::Target {
        (self.frames.as_ref()).expect("the frames are there until they are parked")
    }
}

impl<'s: 'g, 'g> std::ops::DerefMut for Woken<'_, 's, 'g> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        (self.frames.as_mut()).expect("the frames are there until they are parked")
    }
}

impl<'a, 's: 'g, 'g> Drop for Woken<'a, 's, 'g> {
    fn drop(&mut self) {
        *self.home = self.frames.take().map(Frames::park);
    }
}

/// A Merk that a batch drops or clears, as the directory has it before.
struct Leaving {
    subtree: Subtree,
    entry: Entry,
    root: Option<CellAddress>,
    /// Whether it is dropped, and not cleared.
    dropped: bool,
}

/// What one batch does to subtrees as a whole.
#[derive(Default)]
struct Subtrees {
    leaving: Vec<Leaving>,
    /// The subtrees the batch makes an entry for: not one that is entered
    /// if it is absent and was found entered.
    entering: HashSet<Subtree>,
    /// What the moves of the batch do to the directory, if it has any
    /// ([`Directory::plan_rekey`]).
    rekey: Option<RekeyPlan>,
    /// The subtrees that a move brings a Merk to, each with the root of
    /// that Merk.
    moved: HashMap<Subtree, Option<CellAddress>>,
    /// The subtrees that a move takes a Merk from: those it gives another
    /// prefix, and those that leave with it.
    taken: HashSet<Subtree>,
}

/// The refusal of a plan for the moves of a batch, as the batch is told
/// it.
fn unplanned(error: DirectoryError) -> TxnError {
    let subtree = Subtree::new;
    let refused = match error {
        DirectoryError::NotFound { prefix } => BatchError::NotEntered {
            subtree: subtree(prefix),
        },
        DirectoryError::Exists { prefix } => BatchError::Entered {
            subtree: subtree(prefix),
        },
        DirectoryError::Twice { prefix } => BatchError::SubtreeTwice {
            subtree: subtree(prefix),
        },
        DirectoryError::Unnamed { prefix } => BatchError::MoveUnnamed {
            subtree: subtree(prefix),
        },
        DirectoryError::NoHost { prefix } => BatchError::MoveHost {
            subtree: subtree(prefix),
        },
        error => return error.into(),
    };
    refused.into()
}

/// Checks that `entry`, which the subtree of `enter` is entered with, has
/// the tree type and the host the enter states
/// ([`SubtreeWrite::EnterIfAbsent`]).
fn same(entry: &Entry, enter: &Entering<'_>) -> Result<(), BatchError> {
    let subtree = enter.subtree;
    let differs = |differs| {
        let differs = Box::new(differs);
        Err(BatchError::EnteredOtherwise { subtree, differs })
    };
    if entry.tree_type() != enter.tree_type {
        return differs(Differs::TreeType {
            entered: entry.tree_type().into(),
            stated: enter.tree_type.into(),
        });
    }
    let entered = entry.host().map(|host| HostName {
        subtree: Subtree::new(*host.parent),
        key: host.key.into(),
        position: host.position,
    });
    let stated = enter.host.map(|host| HostName {
        subtree: host.subtree,
        key: host.key.into(),
        position: host.position,
    });
    if entered == stated {
        return Ok(());
    }
    differs(Differs::Host { entered, stated })
}

/// Where each node of a batch is before the batch, and whether the tree of
/// its Merk reaches it once the batch is applied, both by the node's place
/// among the nodes of the batch. A node that its tree does not reach is
/// kept as an unlinked record (`super::unlinked`).
struct Reach {
    ats: Vec<Option<CellAddress>>,
    linked: Vec<bool>,
}

/// What the change sets of one batch do to the unlinked records.
#[derive(Default)]
struct UnlinkedWrites {
    /// The stored keys whose records go, each with its subtree: a node is
    /// linked under the key, or the key is deleted.
    taken: Vec<(Subtree, Vec<u8>)>,
    /// The records of the nodes that the trees do not reach, by stored
    /// key.
    kept: Vec<(Vec<u8>, Vec<u8>)>,
    /// The subtrees that hold an unlinked record afterwards.
    holding: Vec<Subtree>,
    /// The value of each node that is new to its tree and keeps the value
    /// of the record the transaction left under its key, by the node's
    /// place among the nodes of the batch.
    relinked: HashMap<usize, Vec<u8>>,
}

impl Subtrees {
    /// Whether the Merk of `entry` is below one that the batch drops or
    /// clears. A Merk with no node hosts none, and a clear of it leaves
    /// the directory as it is.
    fn below_leaving(&self, entry: &Entry) -> bool {
        let leaves = |id: MerkId| {
            (self.leaving.iter()).any(|left| left.root.is_some() && left.entry.id() == id)
        };
        entry.lineage().any(leaves)
    }

    /// Whether the batch ends every node handle that was read before it:
    /// it drops, clears or moves a Merk, whose cells stay in their pages
    /// (PN8).
    fn ends_handles(&self) -> bool {
        !self.leaving.is_empty() || self.rekey.is_some()
    }
}

/// The roots that one write changed, as the directory is told them: all in
/// one statement, which it resolves against the entries as they were before
/// the write (`Directory::set_roots`). The cell that one root left can be
/// the cell another root is in afterwards, so no root is told by itself.
///
/// `named` has, for each Merk of the batch, its change set and the address
/// of its root before the write and after it. `hosted` has the carried
/// roots of hosted Merks, as the write's report gives them
/// ([`RootChange::carried`]).
///
/// - A Merk whose change set sets its root is named with the root's key.
/// - A root that is the node it was and was carried is told by its address,
///   as is every carried root of a hosted Merk that the write did not open
///   (`tree-layout.md` S3, P5). One that is both is told once.
pub(crate) fn root_changes<'a>(
    named: impl Iterator<Item = (&'a MerkWrite<'a>, Option<CellAddress>, Option<CellAddress>)>,
    hosted: impl Iterator<Item = RootChange<'a>>,
) -> Vec<RootChange<'a>> {
    let mut changes = Vec::new();
    let mut told = HashSet::new();
    for (write, was, now) in named {
        match write.root {
            RootPut::Keep => {
                if let (Some(from), Some(to)) = (was, now)
                    && from != to
                    && told.insert(from)
                {
                    changes.push(RootChange::Carried { from, to });
                }
            }
            // A Merk that had no node and has none is as it was.
            RootPut::Set(None) if was.is_none() => {}
            RootPut::Set(key) => {
                let root = key.zip(now).map(|(key, address)| Root { address, key });
                let prefix = write.subtree.as_bytes();
                changes.push(RootChange::Named { prefix, root });
            }
        }
    }
    for change in hosted {
        if matches!(change, RootChange::Carried { from, .. } if told.insert(from)) {
            changes.push(change);
        }
    }
    changes
}

/// The nodes that the keys of one batch name, as the tree layer names them.
struct Resolver<'a> {
    merks: &'a [MerkWrite<'a>],
    /// The root of each Merk before the batch.
    roots: Vec<Option<CellAddress>>,
    /// The place of each Merk's first node among the nodes of the batch.
    offsets: Vec<usize>,
    /// The nodes each Merk's change set writes, by key: each by its place
    /// among the nodes of the batch.
    written: Vec<HashMap<&'a [u8], usize>>,
    by_subtree: HashMap<Subtree, usize>,
    /// For each node of the batch, its place among the nodes of the tree
    /// write, or none for a node that the batch leaves unlinked.
    places: Vec<Option<usize>>,
}

impl Resolver<'_> {
    /// The node under `key` in the Merk `merk`: a node of the batch, or a
    /// cell that is there.
    fn node(
        &self,
        tree: &mut TreePages<'_>,
        merk: usize,
        key: &[u8],
    ) -> Result<Option<Target>, TxnError> {
        if let Some(node) = self.written[merk].get(key) {
            // A node that the batch leaves unlinked is no node of a tree.
            return Ok(self.places[*node].map(Target::Node));
        }
        Ok(find(tree, self.roots[merk], key)?.map(Target::At))
    }

    /// The node of the Merk `merk` that the batch writes under `key`, by
    /// its place among the nodes of that Merk's change set.
    fn local(&self, merk: usize, key: &[u8]) -> Option<usize> {
        let node = self.written[merk].get(key)?;
        Some(node - self.offsets[merk])
    }

    /// The root of the Merk `merk` after the batch.
    fn root(&self, tree: &mut TreePages<'_>, merk: usize) -> Result<Option<Target>, TxnError> {
        match self.merks[merk].root {
            RootPut::Keep => Ok(self.roots[merk].map(Target::At)),
            RootPut::Set(None) => Ok(None),
            RootPut::Set(Some(key)) => {
                let subtree = self.merks[merk].subtree;
                let root = self.node(tree, merk, key)?;
                Ok(Some(root.ok_or(BatchError::NoRoot { subtree })?))
            }
        }
    }

    fn link<'l>(
        &self,
        tree: &mut TreePages<'_>,
        merk: usize,
        link: &LinkPut<'l>,
    ) -> Result<LinkWrite<'l>, TxnError> {
        let subtree = self.merks[merk].subtree;
        let child = self.node(tree, merk, link.key)?;
        Ok(LinkWrite {
            child: child.ok_or(BatchError::NoChild { subtree })?,
            hash: link.hash,
            heights: link.heights,
            child_key_len: link.key.len() as u8,
            aggregate: link.aggregate,
        })
    }
}

/// One node of a batch with its names resolved.
struct Resolved<'a> {
    at: Option<CellAddress>,
    left: Option<LinkWrite<'a>>,
    right: Option<LinkWrite<'a>>,
    /// The position of the node's list of hosted roots.
    hosted: Option<usize>,
    value: ValueWrite<'a>,
}

/// What every name of a batch stands for, as [`Frames::resolve`] finds it.
struct ResolvedBatch<'a> {
    nodes: Vec<Resolved<'a>>,
    hosted: Vec<Vec<Option<Target>>>,
    removed: Vec<CellAddress>,
    root_writes: Vec<Option<RootWrite>>,
    root_targets: Vec<Option<Target>>,
}

impl<'s: 'g, 'g, G: lignin_tree::OperationGuard> Frames<'s, 'g, G> {
    /// The version the frames are built over: *B(T)*.
    pub(crate) fn base(&self) -> CommitVersion {
        self.base
    }

    /// The same frames, with all they hold, bound to `guard`.
    fn under<'q, H>(self, guard: &'q H) -> Frames<'s, 'q, H>
    where
        's: 'q,
        H: lignin_tree::OperationGuard,
    {
        Frames {
            base: self.base,
            tree: self.tree,
            values: self.values,
            source: self.source,
            flat: self.flat.under(guard),
            directory: self.directory,
            dropped: self.dropped,
            hashes: self.hashes,
            fragments: self.fragments,
            key: self.key,
            empty: self.empty,
            unlinked: self.unlinked,
            own: self.own,
            left: self.left,
            renewed: self.renewed,
        }
    }
}

impl<'s: 'g, 'g> Awake<'s, 'g> {
    /// Frames with no copy, over the committed pages of the version `at`,
    /// which the transaction keeps pinned for as long as it has them.
    ///
    /// Their handle on the subtree directory is a copy of the one the
    /// engine keeps for `at` ([`WritePath::directory_at`]), so frames that
    /// are made again have it as their pages do: after a commit of another
    /// landed, and after a rollback.
    pub(crate) fn new(
        path: &WritePath<'s>,
        guard: &'g OperationGuard<'s>,
        at: CommitVersion,
    ) -> Result<Self, TxnError> {
        let owner = WritePath::next_owner();
        let lease = Lease::take();
        let size = path.layout.page_size();
        let flat = FlatWriter::new(path.committed(guard, at), path.pool, owner, path.ids);
        Ok(Self {
            tree: TreePages::new(path.loader, path.pool, path.ids, owner, at, lease, size)
                .counted_in(path.stats),
            values: TreeValues::new(path.inline),
            source: ValueSource::new(path.values, path.loader, at),
            directory: path.directory_at(at)?,
            dropped: Vec::new(),
            flat: FlatFrames(Some(flat)),
            hashes: Hashes::new(),
            fragments: Fragments::new(path.layout),
            key: Vec::new(),
            empty: Vec::new(),
            unlinked: HashMap::new(),
            own: HashSet::new(),
            left: Vec::new(),
            renewed: HashMap::new(),
            base: at,
        })
    }

    /// The frames as the transaction keeps them until its next operation.
    pub(crate) fn park(self) -> Parked<'s> {
        self.under(&IDLE)
    }

    /// For each Merk of the batch that was applied last, whether it had no
    /// root before.
    pub(crate) fn empty_before(&self) -> &[bool] {
        &self.empty
    }

    /// The subtrees below a moved one that left with the moves of the
    /// batch that was applied last: those the moves named no prefix for.
    pub(crate) fn left_with_moves(&self) -> &[Subtree] {
        &self.left
    }

    /// The id that the Merk which has `id` in these frames had before the
    /// clears that were applied to them: its own, where none of them gave
    /// it another.
    pub(crate) fn before_clears(&self, id: MerkId) -> MerkId {
        self.renewed.get(&id).copied().unwrap_or(id)
    }

    /// The lease the nodes of the frames are read under. It ends with the
    /// frames (PN8).
    pub(crate) fn lease(&self) -> Lease {
        self.tree.lease()
    }

    /// The root cell of the Merk `subtree`, as the transaction sees it:
    /// none for a Merk with no node, and for a subtree that is not entered,
    /// or that is below a Merk the transaction dropped or cleared
    /// (`transactions.md` CV9).
    pub(crate) fn root(&self, subtree: &Subtree) -> Result<Option<CellAddress>, TxnError> {
        let entry = self.entry(subtree)?;
        Ok(entry.and_then(|entry| entry.root().map(|root| root.address)))
    }

    /// What the directory holds of `subtree`, as the transaction sees it:
    /// nothing for a subtree that is not entered, or that is below a Merk
    /// the transaction dropped or cleared.
    pub(crate) fn entry(&self, subtree: &Subtree) -> Result<Option<Entry>, TxnError> {
        Ok(self.directory.open(&*self.flat, subtree.as_bytes())?)
    }

    /// What `batch` does to subtrees as a whole, checked against the
    /// directory with nothing changed.
    fn subtrees(&self, batch: &Batch<'_>) -> Result<Subtrees, TxnError> {
        let mut view = Subtrees::default();
        for write in batch.subtrees {
            let (subtree, dropped) = match *write {
                SubtreeWrite::Drop { subtree } => (subtree, true),
                SubtreeWrite::Clear { subtree } => (subtree, false),
                SubtreeWrite::Enter { .. }
                | SubtreeWrite::EnterIfAbsent { .. }
                | SubtreeWrite::Convert { .. }
                | SubtreeWrite::Move { .. } => continue,
            };
            let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
            let entry = entry.ok_or(BatchError::NotEntered { subtree })?;
            let root = entry.root().map(|root| root.address);
            // A Merk with no node hosts none: the directory would refuse
            // the drop after the pages took the batch.
            if dropped && root.is_none() {
                let below = self.directory.children(entry.id()).next(&*self.flat)?;
                if below.is_some() {
                    let prefix = *subtree.as_bytes();
                    return Err(DirectoryError::NotEmpty { prefix }.into());
                }
            }
            view.leaving.push(Leaving {
                subtree,
                entry,
                root,
                dropped,
            });
        }
        // A drop takes the Merks below with it: none of them is named,
        // also below a Merk that has no node.
        for left in &view.leaving {
            let above = |id: MerkId| view.leaving.iter().any(|left| left.entry.id() == id);
            if left.entry.lineage().any(above) {
                let subtree = left.subtree;
                return Err(BatchError::NotEntered { subtree }.into());
            }
        }
        let moves: Vec<Moving<'_>> = batch.moves().collect();
        if !moves.is_empty() {
            self.plan_moves(&moves, &mut view)?;
        }
        for write in batch.subtrees {
            let Some(enter) = write.entering() else {
                continue;
            };
            let subtree = enter.subtree;
            if self.entered(&view, &subtree)?.is_some() {
                // An enter of a subtree that is absent finds it entered:
                // by a commit, by an earlier write of the transaction, or
                // kept by a clear of this batch. It is satisfied where
                // that is the subtree it states.
                let held = match enter.if_absent {
                    true => self.kept(&view, &subtree)?,
                    false => None,
                };
                let Some(held) = held else {
                    return Err(BatchError::Entered { subtree }.into());
                };
                same(&held, &enter)?;
                continue;
            }
            if let Some(host) = enter.host
                && self.entered(&view, &host.subtree)?.is_none()
            {
                let subtree = host.subtree;
                return Err(BatchError::NotEntered { subtree }.into());
            }
            view.entering.insert(subtree);
        }
        // A subtree that is stated anew stays as it is: it is entered, and
        // the batch neither enters it nor takes it out.
        for write in batch.subtrees {
            let SubtreeWrite::Convert { subtree, .. } = write else {
                continue;
            };
            let named = view.entering.contains(subtree)
                || view.leaving.iter().any(|left| left.subtree == *subtree);
            if named {
                return Err(BatchError::SubtreeTwice { subtree: *subtree }.into());
            }
            if self.entered(&view, subtree)?.is_none() {
                return Err(BatchError::NotEntered { subtree: *subtree }.into());
            }
        }
        Ok(view)
    }

    /// Checks the moves of a batch against the directory as the drops and
    /// the clears of the batch leave it, and notes in `view` where each
    /// Merk goes and which leave, with nothing changed.
    ///
    /// Each moved subtree is entered and stays in the Merk its host is in,
    /// at its position. No drop and no clear of the batch names it, a
    /// subtree below it, or one that its host is in or below.
    fn plan_moves(&self, moves: &[Moving<'_>], view: &mut Subtrees) -> Result<(), TxnError> {
        let mut pairs: Vec<Vec<(Prefix, Prefix)>> = Vec::with_capacity(moves.len());
        for moving in moves {
            let subtree = moving.from;
            let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
            let entry = entry.ok_or(BatchError::NotEntered { subtree })?;
            let stated = moving.host;
            let stays = entry.host().is_some_and(|host| {
                host.parent == stated.subtree.as_bytes() && host.position == stated.position
            });
            if !stays {
                return Err(BatchError::MoveHost { subtree }.into());
            }
            for left in &view.leaving {
                let (moved, leaves) = (entry.id(), left.entry.id());
                if moved == leaves
                    || left.entry.lineage().any(|id| id == moved)
                    || entry.lineage().any(|id| id == leaves)
                {
                    let subtree = left.subtree;
                    return Err(BatchError::Moved { subtree }.into());
                }
            }
            let below = moving.below.iter();
            pairs.push(
                below
                    .map(|(from, to)| (*from.as_bytes(), *to.as_bytes()))
                    .collect(),
            );
        }
        let rekeys: Vec<Rekey<'_>> = (moves.iter().zip(&pairs))
            .map(|(moving, below)| Rekey {
                from: moving.from.as_bytes(),
                to: moving.to.as_bytes(),
                key: moving.host.key,
                below,
            })
            .collect();
        // What the batch drops, and what is below a Merk it drops or
        // clears, is out of the directory before the moves are made.
        let leaves = |entry: &Entry| {
            let dropped = |left: &Leaving| left.dropped && left.entry.id() == entry.id();
            view.leaving.iter().any(dropped) || view.below_leaving(entry)
        };
        let plan = self.directory.plan_rekey(&*self.flat, &rekeys, &leaves);
        let plan = plan.map_err(unplanned)?;
        // A pair names a subtree below the moved one, or one that is not
        // entered. One that is entered elsewhere would stay where it is,
        // under a prefix the move is taken to have emptied. And the
        // prefix that such a pair states for a subtree is free as every
        // other one: the transaction reads under it what the pair says.
        let taken: HashSet<&Prefix> = plan.moved().map(|(from, ..)| from).collect();
        let gone: HashSet<&Prefix> = taken.iter().copied().chain(plan.left()).collect();
        for (from, to) in pairs
            .iter()
            .flatten()
            .filter(|(from, _)| !taken.contains(from))
        {
            let entry = self.directory.open(&*self.flat, from)?;
            if entry.is_some_and(|entry| !leaves(&entry)) {
                let subtree = Subtree::new(*from);
                return Err(BatchError::Moved { subtree }.into());
            }
            let there = self.directory.open(&*self.flat, to)?;
            if there.is_some_and(|there| !leaves(&there)) && !gone.contains(to) {
                let subtree = Subtree::new(*to);
                return Err(BatchError::Entered { subtree }.into());
            }
        }
        for (from, to, was) in plan.moved() {
            let root = was.root().map(|root| root.address);
            view.moved.insert(Subtree::new(*to), root);
            view.taken.insert(Subtree::new(*from));
        }
        (view.taken).extend(plan.left().iter().map(|prefix| Subtree::new(*prefix)));
        view.rekey = Some(plan);
        Ok(())
    }

    /// The entry `subtree` keeps through the batch: of a subtree that is
    /// entered before it and that the batch does not drop, alone or with
    /// one above it. A clear keeps the entry, without its root.
    fn kept(&self, view: &Subtrees, subtree: &Subtree) -> Result<Option<Entry>, TxnError> {
        if let Some(left) = view.leaving.iter().find(|left| left.subtree == *subtree) {
            return Ok((!left.dropped).then(|| left.entry.clone()));
        }
        let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
        Ok(entry.filter(|entry| !view.below_leaving(entry)))
    }

    /// Those of `written` that are below a Merk that `batch` drops or
    /// clears, as the directory has them before the batch: what the
    /// transaction wrote there goes with the drop.
    pub(crate) fn below(
        &self,
        batch: &Batch<'_>,
        written: impl Iterator<Item = Subtree>,
    ) -> Result<Vec<Subtree>, TxnError> {
        let view = self.subtrees(batch)?;
        let mut below = Vec::new();
        for subtree in written {
            let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
            if entry.is_some_and(|entry| view.below_leaving(&entry)) {
                below.push(subtree);
            }
        }
        Ok(below)
    }

    /// Whether `subtree` is entered once the subtrees of the batch have
    /// left and are entered, and with which root: `None` for a subtree that
    /// is not entered then, `Some(None)` for one with no node.
    fn entered(
        &self,
        view: &Subtrees,
        subtree: &Subtree,
    ) -> Result<Option<Option<CellAddress>>, TxnError> {
        if view.entering.contains(subtree) {
            return Ok(Some(None));
        }
        // A Merk that a move brings there is entered, whatever was there
        // before, and one that a move takes away is not.
        if let Some(root) = view.moved.get(subtree) {
            return Ok(Some(*root));
        }
        if view.taken.contains(subtree) {
            return Ok(None);
        }
        if let Some(left) = view.leaving.iter().find(|left| left.subtree == *subtree) {
            return Ok((!left.dropped).then_some(None));
        }
        let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
        let entry = entry.filter(|entry| !view.below_leaving(entry));
        Ok(entry.map(|entry| entry.root().map(|root| root.address)))
    }

    /// The address `handle` names, if the handle is still good in these
    /// frames and its cell has the key `key`.
    pub(crate) fn follow(&mut self, handle: NodeHandle, key: &[u8]) -> Option<CellAddress> {
        let lease = self.lease();
        follow(&mut self.tree, lease, handle, key)
    }

    /// Whether the data column holds a record under `subtree` in these
    /// frames: for a subtree whose data are the nodes of a Merk, whether
    /// it holds unlinked records (`super::unlinked`). It is one lookup,
    /// whose answer is kept for the frames, and kept right by the writes
    /// that are applied to them or forgotten: the next call then looks
    /// again. The caller has found the subtree entered.
    pub(crate) fn has_unlinked(
        &mut self,
        path: &WritePath<'s>,
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        if let Some(known) = self.unlinked.get(subtree) {
            return Ok(*known);
        }
        let has = unlinked::any(&path.keyspaces.data, &*self.flat, subtree)?;
        self.unlinked.insert(*subtree, has);
        Ok(has)
    }

    /// Appends the bytes of the unlinked record under `key` of `subtree`
    /// to `out`, and says whether the frames hold one. The caller has
    /// found the subtree entered, and no node of its tree under the key.
    pub(crate) fn unlinked(
        &mut self,
        path: &WritePath<'s>,
        subtree: &Subtree,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError> {
        if !self.has_unlinked(path, subtree)? {
            return Ok(false);
        }
        let data = &path.keyspaces.data;
        Ok(unlinked::get(data, &*self.flat, subtree, key, out)?)
    }

    /// The value of the unlinked record under `key` of `subtree`, if a
    /// write that was applied to these frames left the record there: the
    /// transaction's own node, as it last wrote it. `None` for a key with
    /// no record and for a record the committed state left.
    pub(crate) fn own_unlinked_value(
        &mut self,
        path: &WritePath<'s>,
        subtree: &Subtree,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, TxnError> {
        stored_key(&mut self.key, subtree, key);
        if !self.own.contains(&self.key) {
            return Ok(None);
        }
        let mut record = Vec::new();
        let data = &path.keyspaces.data;
        if !unlinked::get(data, &*self.flat, subtree, key, &mut record)? {
            return Ok(None);
        }
        Ok(Some(unlinked::decode(key, &record)?.value.to_vec()))
    }

    /// The bytes of the value of the cell at `address`, wherever the value
    /// is.
    fn cell_value(&mut self, address: CellAddress) -> Result<Vec<u8>, TxnError> {
        let Frames {
            tree,
            values,
            source,
            ..
        } = self;
        let read = tree.page(address.page(), |page| {
            let node = page.node(address.slot())?;
            let mut buf = ValueBuf::new();
            Some(values.read(source, node, &mut buf).map(<[u8]>::to_vec))
        })?;
        Ok(read.ok_or(NodeError::NoNode { address })??)
    }

    /// Applies `batch` to the frames: the change sets of its Merks as one
    /// write of the tree layer, in which the root of each Merk that leaves
    /// is named as dropped, then what the write did to the subtree
    /// directory, then the unlinked records its change sets leave and
    /// take, and then its records.
    ///
    /// The batch has passed [`check`](super::check::check). `made` is given
    /// for a batch that is applied again (WT7): it says of each Merk
    /// whether the transaction made its first node with this batch. Such a
    /// Merk has to be empty still, and the batch is refused if it is not
    /// (GM3).
    pub(crate) fn apply(
        &mut self,
        path: &WritePath<'s>,
        batch: &Batch<'_>,
        made: Option<&[bool]>,
    ) -> Result<Applied, Refused> {
        let view = self.subtrees(batch).map_err(Refused::clean)?;
        self.hosts_moved(batch).map_err(Refused::clean)?;
        let mut entered = HashMap::new();
        for record in batch.records {
            // A record is of a subtree that is entered, in every column,
            // and the nodes of a Merk are not written through a flat tree.
            let unit = record.unit();
            let subtree = unit.subtree;
            let root = match entered.get(&subtree) {
                Some(root) => *root,
                None => {
                    let root = self.entered(&view, &subtree).map_err(Refused::clean)?;
                    entered.insert(subtree, root);
                    root
                }
            };
            let Some(root) = root else {
                return Err(Refused::clean(BatchError::NotEntered { subtree }));
            };
            if unit.column == Column::Data && root.is_some() {
                return Err(Refused::clean(BatchError::NotFlat { unit }));
            }
        }
        let (applied, unlinked) = self.apply_merks(path, batch, &view, made)?;
        // The flat records of a dropped subtree went with its drop, in
        // every column. A cleared one keeps its own (`tree-layout.md` D6)
        // but for its data, which a clear takes: the same lazy range
        // delete, which reads and writes the two ends of the range and
        // leaves the pages between to the reclaimer.
        let data = path.keyspaces.of(Column::Data);
        for left in view.leaving.iter().filter(|left| !left.dropped) {
            let prefix = left.subtree.as_bytes();
            (path.flat_drops)
                .unlink_prefix(&mut *self.flat, &data, prefix, self.base)
                .map_err(Refused::dropping)?;
            // The subtree is the one it was, with those records, under
            // whichever id the clear left it.
            let cleared = self.directory.open(&*self.flat, prefix);
            let cleared = cleared.map_err(Refused::dirty)?;
            let was = left.entry.id();
            if let Some(now) = cleared.map(|entry| entry.id()).filter(|now| *now != was) {
                self.renewed.insert(now, self.before_clears(was));
            }
        }
        // The unlinked records: those under the keys that the change sets
        // link a node under or delete go, and the nodes that the trees do
        // not reach are kept as records. A subtree that is entered or
        // cleared starts with none, and the memory of what each subtree
        // holds follows.
        for (subtree, key) in &unlinked.taken {
            let taken = data.delete(&mut *self.flat, key);
            if taken.map_err(Refused::dirty)? {
                // Whether the subtree holds another one is not known any
                // more: it is looked up when it is next asked.
                self.unlinked.remove(subtree);
            }
        }
        for (key, record) in &unlinked.kept {
            let kept = data.put(&mut *self.flat, key, record);
            kept.map_err(Refused::dirty)?;
        }
        for write in batch.subtrees {
            match write {
                SubtreeWrite::Enter { subtree, .. } | SubtreeWrite::Clear { subtree } => {
                    self.unlinked.insert(*subtree, false);
                }
                // A subtree that was found entered keeps its records.
                SubtreeWrite::EnterIfAbsent { subtree, .. } => {
                    if !view.entering.contains(subtree) {
                        continue;
                    }
                    self.unlinked.insert(*subtree, false);
                }
                SubtreeWrite::Drop { subtree } => {
                    self.unlinked.remove(subtree);
                }
                SubtreeWrite::Convert { .. } | SubtreeWrite::Move { .. } => continue,
            }
            // What the transaction left under a subtree goes with it.
            let prefix = write.subtree();
            (self.own).retain(|key| !key.starts_with(prefix.as_bytes()));
        }
        // A Merk that is moved has its flat records under its new prefix,
        // the unlinked ones among them: what is known of whether it holds
        // any goes there with it, and so do the keys of the records that
        // the transaction left. The moves of a batch are made together,
        // and what leaves with them takes its own out. A key that was
        // left under a prefix by a subtree which went with one above it
        // is not of the Merk that a move brings there.
        if let Some(plan) = &view.rekey {
            let moved: Vec<(Subtree, Subtree)> = (plan.moved())
                .map(|(from, to, _)| (Subtree::new(*from), Subtree::new(*to)))
                .collect();
            let known: Vec<(Subtree, Option<bool>)> = (moved.iter())
                .map(|(from, to)| (*to, self.unlinked.remove(from)))
                .collect();
            for left in plan.left() {
                self.unlinked.remove(&Subtree::new(*left));
            }
            for (to, known) in known {
                match known {
                    Some(known) => drop(self.unlinked.insert(to, known)),
                    None => drop(self.unlinked.remove(&to)),
                }
            }
            // Each key is asked first of the prefixes that a Merk is taken
            // from: the key of a Merk that leaves a prefix which another
            // comes to goes with its Merk.
            let goes: HashMap<&[u8], &Subtree> = (moved.iter())
                .map(|(from, to)| (&from.as_bytes()[..], to))
                .collect();
            let mut gone: HashSet<&[u8]> = plan.left().iter().map(|left| &left[..]).collect();
            gone.extend(moved.iter().map(|(_, to)| &to.as_bytes()[..]));
            let own = std::mem::take(&mut self.own);
            let rekeyed = own.into_iter().filter_map(|key| {
                let (prefix, rest) = key.split_at(Subtree::LEN.min(key.len()));
                if let Some(to) = goes.get(prefix) {
                    return Some([&to.as_bytes()[..], rest].concat());
                }
                (!gone.contains(prefix)).then_some(key)
            });
            self.own = rekeyed.collect();
        }
        for subtree in unlinked.holding {
            self.unlinked.insert(subtree, true);
        }
        for (_, key) in &unlinked.taken {
            self.own.remove(key);
        }
        (self.own).extend(unlinked.kept.into_iter().map(|(key, _)| key));
        for record in batch.records {
            let tree = path.keyspaces.of(record.unit().column);
            let subtree = record.unit().subtree;
            // A delete in the data column can take the last record out:
            // whether the subtree holds one is looked up when it is next
            // asked.
            let forget = |known: &mut HashMap<Subtree, bool>| {
                if record.unit().column == Column::Data {
                    known.remove(&subtree);
                }
            };
            let done = match *record {
                RecordWrite::Put { key, value, .. } => {
                    // What is known of the data column of the subtree
                    // stays true.
                    if record.unit().column == Column::Data
                        && let Some(holds) = self.unlinked.get_mut(&subtree)
                    {
                        *holds = true;
                    }
                    stored_key(&mut self.key, &subtree, key);
                    tree.put(&mut *self.flat, &self.key, value)
                }
                RecordWrite::Delete { key, .. } => {
                    forget(&mut self.unlinked);
                    stored_key(&mut self.key, &subtree, key);
                    tree.delete(&mut *self.flat, &self.key).map(|_| ())
                }
                RecordWrite::DeleteRange { start, end, .. } => {
                    forget(&mut self.unlinked);
                    stored_key(&mut self.key, &subtree, start);
                    let end = match end {
                        Some(end) => {
                            let mut stored = Vec::new();
                            stored_key(&mut stored, &subtree, end);
                            Some(stored)
                        }
                        None => subtree_end(&subtree),
                    };
                    tree.delete_range(&mut *self.flat, &self.key, end.as_deref())
                }
            };
            done.map_err(Refused::dirty)?;
        }
        Ok(applied)
    }

    /// Whether the cell of the node under `key`, in the Merk whose root is
    /// `root`, hosts a Merk at `position`: the cell has a pointer there,
    /// to a root or to none. Where no node is under the key, none does.
    fn hosts_at(
        &mut self,
        root: Option<CellAddress>,
        key: &[u8],
        position: usize,
    ) -> Result<bool, TxnError> {
        let Some(at) = find(&mut self.tree, root, key)? else {
            return Ok(false);
        };
        let hosts = self.tree.page(at.page(), |page| {
            Some(page.node(at.slot())?.hosted_roots().count() > position)
        })?;
        Ok(hosts.ok_or(NodeError::NoNode { address: at })?)
    }

    /// Checks, for each subtree that `batch` moves, that the cell of the
    /// node which the batch puts as its host points at the moved Merk as
    /// the cell of the node that hosts it before the batch does
    /// (`transactions.md` MV3):
    ///
    /// - where the old host's cell hosts a Merk at the subtree's
    ///   position, the new host names the subtree's new prefix there;
    /// - where it hosts none there, or no node is under the key that the
    ///   subtree's entry names, the new host hosts no Merk at the
    ///   position: it names none there, or it keeps what it hosts and its
    ///   cell hosts none there.
    ///
    /// The entry of a subtree names its host whether or not the host's
    /// cell points at a Merk: the element of a tree that keeps its data
    /// flat hosts none (`tree-layout.md`, section 2), and a move leaves
    /// that as it is. Nothing is changed.
    fn hosts_moved(&mut self, batch: &Batch<'_>) -> Result<(), TxnError> {
        for moving in batch.moves() {
            let subtree = moving.from;
            // A subtree that is not entered, or that has no host, was
            // refused where the moves were planned.
            let Some(entry) = self.directory.open(&*self.flat, subtree.as_bytes())? else {
                continue;
            };
            let Some(host) = entry.host() else {
                continue;
            };
            let position = usize::from(host.position);
            // The host stays in its Merk, at its position.
            let root = self.root(&Subtree::new(*host.parent))?;
            let hosted = self.hosts_at(root, host.key, position)?;
            let stated = moving.host;
            let merk = (batch.merks.iter()).find(|merk| merk.subtree == stated.subtree);
            let node = merk.and_then(|merk| merk.nodes.iter().find(|node| node.key == stated.key));
            let fits = match node.map(|node| node.hosted) {
                Some(Hosted::Set(named)) => match hosted {
                    true => named.get(position) == Some(&moving.to),
                    false => named.len() <= position,
                },
                // A node that keeps what it hosts names nothing.
                Some(Hosted::Keep) | None => {
                    !hosted && !self.hosts_at(root, stated.key, position)?
                }
            };
            if !fits {
                return Err(BatchError::MoveHost { subtree }.into());
            }
        }
        Ok(())
    }

    fn apply_merks(
        &mut self,
        path: &WritePath<'s>,
        batch: &Batch<'_>,
        view: &Subtrees,
        made: Option<&[bool]>,
    ) -> Result<(Applied, UnlinkedWrites), Refused> {
        self.empty.clear();
        self.left.clear();
        let merks = batch.merks;
        if merks.is_empty() && batch.subtrees.is_empty() {
            return Ok(Default::default());
        }
        let mut offsets = Vec::with_capacity(merks.len());
        let mut total = 0;
        for merk in merks {
            offsets.push(total);
            total += merk.nodes.len();
        }
        let mut roots = Vec::with_capacity(merks.len());
        for merk in merks {
            let subtree = merk.subtree;
            let root = self.entered(view, &subtree).map_err(Refused::clean)?;
            roots.push(root.ok_or_else(|| Refused::clean(BatchError::NotEntered { subtree }))?);
        }
        self.empty.extend(roots.iter().map(Option::is_none));
        let made = made.unwrap_or_default().iter();
        for ((merk, made), empty) in merks.iter().zip(made).zip(&self.empty) {
            // The transaction made the first node of this Merk, and it has
            // a root now that is not the transaction's.
            if *made && !*empty {
                let subtree = merk.subtree;
                return Err(Refused::clean(BatchError::RootTaken { subtree }));
            }
        }
        let written = (merks.iter().zip(&offsets))
            .map(|(merk, offset)| {
                let nodes = merk.nodes.iter().enumerate();
                nodes.map(|(node, put)| (put.key, offset + node)).collect()
            })
            .collect();
        let by_subtree = (merks.iter().enumerate())
            .map(|(merk, write)| (write.subtree, merk))
            .collect();
        let mut resolver = Resolver {
            merks,
            roots,
            offsets,
            written,
            by_subtree,
            places: Vec::new(),
        };

        // What every name of the batch stands for is found before anything
        // is changed, so a batch that names what is not there is refused
        // with every page as it was. First, where each node is and whether
        // the tree of its Merk reaches it once the batch is applied: a
        // node it does not reach is no node of the tree write.
        let reach = self.reach(view, &resolver).map_err(Refused::clean)?;
        let mut place = 0;
        let places = reach.linked.iter().map(|linked| {
            linked.then(|| {
                place += 1;
                place - 1
            })
        });
        resolver.places = places.collect();
        let loose = (self.loose(path, view, &resolver, &reach)).map_err(Refused::clean)?;
        let mut held = Vec::new();
        let resolved = self.resolve(view, &resolver, &reach, &loose.relinked, &mut held);
        let ResolvedBatch {
            nodes,
            hosted,
            removed,
            root_writes,
            root_targets,
        } = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                self.forget(&held);
                return Err(Refused::clean(error));
            }
        };
        let linked = || {
            let puts = merks.iter().flat_map(|merk| merk.nodes);
            puts.zip(&reach.linked)
                .filter_map(|(put, linked)| linked.then_some(put))
        };
        let node_writes: Vec<NodeWrite<'_>> = linked()
            .zip(&nodes)
            .map(|(put, node)| NodeWrite {
                at: node.at,
                key: put.key,
                feature: put.feature,
                value_hash: put.value_hash,
                kv_hash: put.kv_hash,
                left: node.left,
                right: node.right,
                hosted: match node.hosted {
                    None => HostedWrite::Keep,
                    Some(list) => HostedWrite::Set(&hosted[list]),
                },
                value: node.value,
            })
            .collect();
        let roots: Vec<RootWrite> = root_writes.iter().flatten().copied().collect();
        let write = Write {
            nodes: &node_writes,
            removed: &removed,
            dropped: &[],
            roots: &roots,
        };
        // The flat side of a drop and of an enter: every column, the
        // keyspace of the pages that are unlinked, and the version the
        // frames are built over. Frames that are made again over another
        // base make these calls again with that base, because a record of
        // unlinked pages says which version its writer read at
        // (`lignin_tree::drop`, "What the callers do").
        let columns = path.keyspaces.columns();
        let mut flats = Flats {
            pages: &mut *self.flat,
            columns: &columns,
            drops: &path.flat_drops,
            at: self.base,
        };
        // A Merk that leaves keeps its cells in their pages, so a handle
        // into them would still be followed, and so would one into a Merk
        // that has another prefix afterwards. Every handle that was read
        // before ends here: the write is made under a new lease, and
        // reports its nodes under that one (PN8).
        let lease = self.tree.lease();
        if view.ends_handles() {
            self.tree.set_lease(Lease::take());
        }
        let report = match (merks.is_empty(), view.leaving.is_empty()) {
            (true, true) => Ok(Report::default()),
            (false, true) => (self.fragments)
                .apply(&mut self.tree, &mut self.values, &write)
                .map_err(DropError::Fragment),
            // The drop: the one write to the tree pages, in which the root
            // of each Merk that leaves is named as dropped, the Merk's
            // records in the directory, and the flat records of each
            // dropped Merk in every column. Nothing of the Merks is read
            // but their entries (`tree-layout.md` D1, D4).
            (_, false) => {
                let leaving: Vec<Left<'_>> = (view.leaving.iter())
                    .map(|left| match left.dropped {
                        true => Left::Dropped(left.subtree.as_bytes()),
                        false => Left::Cleared(left.subtree.as_bytes()),
                    })
                    .collect();
                drop::unlink(
                    &mut self.fragments,
                    &mut self.tree,
                    &mut self.values,
                    &mut self.directory,
                    &mut flats,
                    &write,
                    &leaving,
                )
                .map(|unlinked| {
                    self.dropped.extend(unlinked.left);
                    unlinked.report
                })
            }
        };
        let report = match report {
            Ok(report) => report,
            Err(DropError::Fragment(
                error @ (FragmentError::Write(_) | FragmentError::NoRoom { .. }),
            )) => {
                self.forget(&held);
                self.tree.set_lease(lease);
                return Err(Refused::clean(error));
            }
            Err(error) => return Err(Refused::dropping(error)),
        };
        self.values.check().map_err(Refused::dirty)?;
        // The marks follow the cells the write moved. Then each node of
        // the write has the marks its writer states, under the address the
        // report gives: a hash the writer made is stale no more. Where
        // nothing is or was stale, nothing is done.
        self.hashes.follow(&write, &report);
        let marks = !self.hashes.is_empty() || linked().any(|put| !put.stale.is_empty());
        for (put, node) in linked().zip(&report.nodes).filter(|_| marks) {
            let links = [(Side::Left, &put.left), (Side::Right, &put.right)];
            let missing = links.iter().filter(|(_, link)| link.is_none());
            let stale = missing.fold(put.stale, |stale, (side, _)| {
                stale.without(HashField::Child(*side))
            });
            // The key-value hash is made of the value hash: it is stale
            // with it, whatever the writer states.
            let stale = match stale.has(HashField::Value) {
                true => stale.with(HashField::KeyValue),
                false => stale,
            };
            for field in Stale::ALL.fields() {
                match stale.has(field) {
                    true => self.hashes.mark(node.address(), field),
                    false => self.hashes.unmark(node.address(), field),
                }
            }
        }

        #[cfg(all(test, not(loom)))]
        probe::strike();

        // The moves, after the drops and before the enters: the records
        // of the directory for every Merk that has another prefix
        // afterwards, and its flat records in every column. The roots are
        // told below, under the prefixes the Merks have then.
        let rekeyed = match &view.rekey {
            Some(plan) => {
                let left = drop::rekey(&mut self.directory, &mut flats, plan);
                self.dropped.extend(left.map_err(Refused::dropping)?);
                (self.left).extend(plan.left().iter().map(|prefix| Subtree::new(*prefix)));
                true
            }
            None => false,
        };

        // The directory: the Merks that are stated anew, the Merks that are
        // entered, each so that it starts with no flat record, and then
        // every root the write changed. A
        // subtree that the directory refuses has changed no page: where
        // nothing of the batch was written before it, the frames are as
        // they were. A subtree that is entered if it is absent, and was
        // found entered, keeps its entry and its records: nothing is done
        // for it.
        let mut untouched = merks.is_empty() && view.leaving.is_empty() && !rekeyed;
        let restated: Vec<Restated<'_>> = (batch.subtrees.iter())
            .filter_map(|write| match write {
                SubtreeWrite::Convert {
                    subtree,
                    tree_type,
                    position,
                } => Some(Restated {
                    prefix: subtree.as_bytes(),
                    tree_type,
                    position: *position,
                }),
                _ => None,
            })
            .collect();
        if !restated.is_empty() {
            match self.directory.restate(flats.pages, &restated) {
                Ok(()) => untouched = false,
                Err(error) if untouched && refuses(&error) => return Err(Refused::clean(error)),
                Err(error) => return Err(Refused::dirty(error)),
            }
        }
        let enters = batch.subtrees.iter().filter_map(SubtreeWrite::entering);
        for enter in enters.filter(|enter| view.entering.contains(&enter.subtree)) {
            let host = enter.host.as_ref().map(|host| Host {
                parent: host.subtree.as_bytes(),
                key: host.key,
                position: host.position,
            });
            let prefix = enter.subtree.as_bytes();
            match drop::enter(&self.directory, &mut flats, prefix, enter.tree_type, host) {
                Ok(_) => untouched = false,
                Err(DropError::Directory(error)) if untouched && refuses(&error) => {
                    return Err(Refused::clean(error));
                }
                Err(error) => return Err(Refused::dropping(error)),
            }
        }
        let moved: HashMap<CellAddress, CellAddress> = (report.carried.iter())
            .map(|carried| (carried.from, carried.to.address()))
            .collect();
        let mut pointer = 0;
        let named = merks.iter().enumerate().map(|(merk, write)| {
            let root = match root_writes[merk] {
                Some(_) => {
                    pointer += 1;
                    report.roots[pointer - 1]
                }
                None => match root_targets[merk] {
                    None | Some(Target::Kept) => None,
                    Some(Target::Node(node)) => Some(report.nodes[node].address()),
                    Some(Target::At(at)) => Some(*moved.get(&at).unwrap_or(&at)),
                },
            };
            (write, resolver.roots[merk], root)
        });
        let hosted = report.carried.iter().filter_map(RootChange::carried);
        let changes = root_changes(named, hosted);
        #[cfg(all(test, not(loom)))]
        probe::count(&changes);
        if !changes.is_empty() {
            (self.directory.set_roots(&mut *self.flat, &changes)).map_err(Refused::dirty)?;
        }
        let unlinked = (0..total).filter(|node| !reach.linked[*node]).collect();
        let applied = Applied {
            carried: (report.carried.iter())
                .map(|carried| (carried.from, carried.to))
                .collect(),
            nodes: report.nodes,
            unlinked,
        };
        Ok((applied, loose))
    }

    /// Gives up the pending values of a write that was refused.
    fn forget(&mut self, held: &[lignin_format::value_log::PendingValue]) {
        for pending in held {
            self.values.forget(*pending);
        }
    }

    /// Checks that the cell at `host`, which is written and keeps the Merks
    /// it hosts, hosts none that the batch has a change set for: the root
    /// of such a Merk is the host's to name, and a host that keeps its
    /// pointers names nothing.
    fn keeps_no_root(
        &mut self,
        resolver: &Resolver<'_>,
        host: CellAddress,
    ) -> Result<(), TxnError> {
        let roots = &resolver.roots;
        let hosted = self.tree.page(host.page(), |page| {
            let pointers = page.node(host.slot())?.hosted_roots();
            Some(
                (pointers.flatten()).find_map(|root| roots.iter().position(|at| *at == Some(root))),
            )
        })?;
        match hosted.ok_or(NodeError::NoNode { address: host })? {
            Some(merk) if !resolver.merks[merk].anchored => {
                let subtree = resolver.merks[merk].subtree;
                Err(BatchError::HostKeeps { subtree }.into())
            }
            _ => Ok(()),
        }
    }

    /// Finds the cell of each node the batch writes, by a hint that is
    /// still good or by the key (PN8), and which of the nodes the tree of
    /// their Merk reaches once the batch is applied.
    ///
    /// A node is reached from the root its change set states, through the
    /// links the batch states. That finds every node of a commit of a
    /// Merk, which writes the path from the root to each node it writes,
    /// with no page read. What it does not find is asked of the pages:
    ///
    /// - a node with a cell can hang below a node the batch does not
    ///   write, so the tree is asked for it by its key ([`Self::hangs`]);
    /// - under a root that is kept, a node with a cell stays where the
    ///   tree has it;
    /// - a node with no cell that no reached node links is in no tree.
    fn reach(&mut self, view: &Subtrees, resolver: &Resolver<'_>) -> Result<Reach, TxnError> {
        let mut ats = Vec::new();
        let mut linked = Vec::new();
        for (merk, write) in resolver.merks.iter().enumerate() {
            let (offset, nodes) = (resolver.offsets[merk], write.nodes);
            for put in nodes {
                // No hint is followed in a batch that drops, clears or
                // moves a Merk: its cells stay in their pages, and a
                // handle from before would name one of them.
                let hint = put.at.filter(|_| !view.ends_handles());
                let hinted = hint.and_then(|handle| self.follow(handle, put.key));
                ats.push(match hinted {
                    Some(at) => Some(at),
                    None => find(&mut self.tree, resolver.roots[merk], put.key)?,
                });
            }
            linked.resize(offset + nodes.len(), false);
            let (ats, linked) = (&ats[offset..], &mut linked[offset..]);
            let index = |key: &[u8]| resolver.local(merk, key);
            let mut ahead: Vec<usize> = match write.root {
                RootPut::Keep => (0..nodes.len())
                    .filter(|node| ats[*node].is_some())
                    .collect(),
                RootPut::Set(None) => Vec::new(),
                RootPut::Set(Some(root)) => index(root).into_iter().collect(),
            };
            let spread = |ahead: &mut Vec<usize>, linked: &mut [bool]| {
                while let Some(node) = ahead.pop() {
                    if std::mem::replace(&mut linked[node], true) {
                        continue;
                    }
                    let links = [&nodes[node].left, &nodes[node].right];
                    ahead.extend(
                        links
                            .into_iter()
                            .flatten()
                            .filter_map(|link| index(link.key)),
                    );
                }
            };
            spread(&mut ahead, linked);
            if linked.iter().all(|linked| *linked) || !matches!(write.root, RootPut::Set(Some(_))) {
                continue;
            }
            let cells: HashMap<CellAddress, usize> = (ats.iter().enumerate())
                .filter_map(|(node, at)| Some(((*at)?, node)))
                .collect();
            for node in 0..nodes.len() {
                if linked[node] || ats[node].is_none() {
                    continue;
                }
                if self.hangs(resolver, merk, &cells, nodes[node].key)? {
                    ahead.push(node);
                    spread(&mut ahead, linked);
                }
            }
        }
        Ok(Reach { ats, linked })
    }

    /// Whether the tree of the Merk `merk` holds a node under `key` once
    /// the batch is applied: asked by the key, from the root the change
    /// set states, through the links the batch states for the nodes it
    /// writes and the links the cells have for all others. `cells` has the
    /// written nodes of the Merk by their cells.
    fn hangs(
        &mut self,
        resolver: &Resolver<'_>,
        merk: usize,
        cells: &HashMap<CellAddress, usize>,
        key: &[u8],
    ) -> Result<bool, TxnError> {
        /// A node of the tree: one the batch writes, or the cell of another.
        enum At {
            Written(usize),
            Cell(CellAddress),
        }
        let write = &resolver.merks[merk];
        let RootPut::Set(Some(root)) = write.root else {
            return Ok(false);
        };
        let before = resolver.roots[merk];
        let named = |tree: &mut TreePages<'_>, key: &[u8]| match resolver.local(merk, key) {
            Some(node) => Ok::<_, TxnError>(Some(At::Written(node))),
            None => Ok(find(tree, before, key)?.map(At::Cell)),
        };
        let mut at = named(&mut self.tree, root)?;
        for _ in 0..MAX_DEPTH {
            let node = match at {
                None => return Ok(false),
                Some(At::Written(node)) => node,
                Some(At::Cell(cell)) => match cells.get(&cell) {
                    Some(node) => *node,
                    None => {
                        let step = self.tree.page(cell.page(), |page| {
                            let node = page.node(cell.slot())?;
                            let side = match key.cmp(node.key()) {
                                Ordering::Equal => return Some(None),
                                Ordering::Less => Side::Left,
                                Ordering::Greater => Side::Right,
                            };
                            Some(Some(node.link(side).map(|link| link.child)))
                        })?;
                        match step.ok_or(NodeError::NoNode { address: cell })? {
                            None => return Ok(true),
                            Some(child) => at = child.map(At::Cell),
                        }
                        continue;
                    }
                },
            };
            let put = &write.nodes[node];
            let link = match key.cmp(put.key) {
                Ordering::Equal => return Ok(true),
                Ordering::Less => &put.left,
                Ordering::Greater => &put.right,
            };
            at = match link {
                None => None,
                Some(link) => named(&mut self.tree, link.key)?,
            };
        }
        Err(TxnError::Damaged("a Merk deeper than any tree of nodes"))
    }

    /// What the change sets of a batch do to the unlinked records, found
    /// before anything is changed: the records they take, and the nodes
    /// they leave as records.
    ///
    /// A node that leaves its tree and keeps its value takes the value of
    /// its cell with it. A node with no cell that keeps its value keeps
    /// the value of the record the transaction left under its key, and is
    /// refused where it left none: a record that the committed state left
    /// is no node, and has no value to keep.
    ///
    /// A change set whose nodes all have a cell that stays, and that
    /// deletes none, asks nothing. Any other, in a subtree that the batch
    /// neither enters nor drops nor clears, asks whether its subtree holds
    /// unlinked records, where the frames do not know
    /// ([`Self::has_unlinked`]), and where it holds none and leaves none,
    /// nothing more.
    fn loose(
        &mut self,
        path: &WritePath<'s>,
        view: &Subtrees,
        resolver: &Resolver<'_>,
        reach: &Reach,
    ) -> Result<UnlinkedWrites, TxnError> {
        let mut writes = UnlinkedWrites::default();
        let data = path.keyspaces.of(Column::Data);
        for (merk, write) in resolver.merks.iter().enumerate() {
            let (offset, subtree) = (resolver.offsets[merk], write.subtree);
            let state = |node: usize| (reach.linked[offset + node], reach.ats[offset + node]);
            let states = || (0..write.nodes.len()).map(state);
            let leaves = states().any(|(linked, _)| !linked);
            let news = states().any(|(linked, at)| linked && at.is_none());
            if !leaves && !news && write.removed.is_empty() {
                continue;
            }
            // A subtree that the batch enters, drops or clears holds no
            // record when its change sets are applied.
            let fresh = view.entering.contains(&subtree)
                || view.leaving.iter().any(|left| left.subtree == subtree);
            let holds = !fresh && self.has_unlinked(path, &subtree)?;
            if !leaves && !holds {
                continue;
            }
            let stored = |key: &[u8]| {
                let mut stored = Vec::with_capacity(Subtree::LEN + key.len());
                stored_key(&mut stored, &subtree, key);
                stored
            };
            for (node, put) in write.nodes.iter().enumerate() {
                let (linked, at) = state(node);
                if linked {
                    // A key has a node of the tree or an unlinked record,
                    // so only a node that is new to its tree can have a
                    // record under its key, which it takes the place of.
                    let mut record = Vec::new();
                    if holds
                        && at.is_none()
                        && unlinked::get(&data, &*self.flat, &subtree, put.key, &mut record)?
                    {
                        let stored = stored(put.key);
                        if put.value == ValuePut::Keep && self.own.contains(&stored) {
                            let value = unlinked::decode(put.key, &record)?.value.to_vec();
                            writes.relinked.insert(offset + node, value);
                        }
                        writes.taken.push((subtree, stored));
                    }
                    continue;
                }
                unlinked::check(put)?;
                // A record hosts nothing: what the element of such a node
                // hosted is dropped by its batch.
                if matches!(put.hosted, Hosted::Set(hosted) if !hosted.is_empty()) {
                    return Err(BatchError::HostUnlinked { subtree }.into());
                }
                // And nobody makes a hash of one. The hash of a child the
                // node does not have is none to make.
                let missing = [(Side::Left, &put.left), (Side::Right, &put.right)];
                let missing = missing.into_iter().filter(|(_, link)| link.is_none());
                let stale = missing.fold(put.stale, |stale, (side, _)| {
                    stale.without(HashField::Child(side))
                });
                if !stale.is_empty() {
                    return Err(BatchError::StaleUnlinked { subtree }.into());
                }
                let kept;
                let value = match (put.value, at) {
                    (ValuePut::Value(value), _) => value,
                    (ValuePut::Keep, Some(at)) => {
                        kept = self.cell_value(at)?;
                        &kept
                    }
                    (ValuePut::Keep, None) => {
                        let own = match holds {
                            true => self.own_unlinked_value(path, &subtree, put.key)?,
                            false => None,
                        };
                        kept = own.ok_or(BatchError::NoValue { subtree })?;
                        &kept
                    }
                };
                unlinked::room(put, value.len())?;
                let mut bytes = Vec::new();
                unlinked::encode(&mut bytes, put, value)?;
                writes.kept.push((stored(put.key), bytes));
            }
            if leaves {
                writes.holding.push(subtree);
            }
            if holds {
                (writes.taken).extend(write.removed.iter().map(|key| (subtree, stored(key))));
            }
        }
        Ok(writes)
    }

    /// Finds what every key of the batch names for the tree write: the
    /// child of each link, the root of each Merk, the Merks each node
    /// hosts and the cell of each deleted node. The nodes are those the
    /// trees reach ([`Self::reach`]); the cell of a node that leaves its
    /// tree is taken out with the deleted ones.
    fn resolve<'a>(
        &mut self,
        view: &Subtrees,
        resolver: &Resolver<'a>,
        reach: &Reach,
        relinked: &'a HashMap<usize, Vec<u8>>,
        held: &mut Vec<lignin_format::value_log::PendingValue>,
    ) -> Result<ResolvedBatch<'a>, TxnError> {
        let merks = resolver.merks;
        let mut root_targets = Vec::with_capacity(merks.len());
        for merk in 0..merks.len() {
            root_targets.push(resolver.root(&mut self.tree, merk)?);
        }
        let mut nodes = Vec::with_capacity(reach.linked.len());
        let mut hosted: Vec<Vec<Option<Target>>> = Vec::new();
        let mut removed = Vec::new();
        let mut hosted_here = vec![false; merks.len()];
        for (merk, write) in merks.iter().enumerate() {
            let subtree = write.subtree;
            let offset = resolver.offsets[merk];
            for (node, put) in write.nodes.iter().enumerate() {
                let at = reach.ats[offset + node];
                if !reach.linked[offset + node] {
                    // The node is in no tree: its cell goes, if it has
                    // one, and its record is kept beside the tree.
                    removed.extend(at);
                    continue;
                }
                let link = |tree: &mut TreePages<'_>, link: &Option<LinkPut<'a>>| {
                    (link.as_ref())
                        .map(|link| resolver.link(tree, merk, link))
                        .transpose()
                };
                let left = link(&mut self.tree, &put.left)?;
                let right = link(&mut self.tree, &put.right)?;
                let hosts = match put.hosted {
                    Hosted::Keep => {
                        // A host and the Merk it hosts are two Merks.
                        if let Some(at) = at.filter(|_| merks.len() > 1) {
                            self.keeps_no_root(resolver, at)?;
                        }
                        None
                    }
                    Hosted::Set(subtrees) => {
                        let mut targets = Vec::with_capacity(subtrees.len());
                        for hosted in subtrees {
                            targets.push(match resolver.by_subtree.get(hosted) {
                                Some(&merk) => {
                                    hosted_here[merk] = true;
                                    root_targets[merk]
                                }
                                None => self.entered(view, hosted)?.flatten().map(Target::At),
                            });
                        }
                        hosted.push(targets);
                        Some(hosted.len() - 1)
                    }
                };
                let stated = match put.value {
                    ValuePut::Value(value) => Some(value),
                    // A node that is new to its tree keeps the value of
                    // the record the transaction left under its key, and
                    // has none to keep otherwise.
                    ValuePut::Keep if at.is_none() => {
                        let own = relinked.get(&(offset + node));
                        Some(own.ok_or(BatchError::NoValue { subtree })?.as_slice())
                    }
                    ValuePut::Keep => None,
                };
                let value = match stated {
                    None => ValueWrite::Keep,
                    // The length alone says where a value goes, and a
                    // value of no bytes is never out of line.
                    Some(value) => {
                        let write = self.values.write(value)?;
                        if let ValueWrite::Pending(pending) = write {
                            held.push(pending);
                        }
                        write
                    }
                };
                nodes.push(Resolved {
                    at,
                    left,
                    right,
                    hosted: hosts,
                    value,
                });
            }
            for key in write.removed {
                // A delete of a key with no node is a write of the key and
                // changes no page (ST5).
                removed.extend(find(&mut self.tree, resolver.roots[merk], key)?);
            }
        }
        // The root of a Merk that no node of the batch hosts is named by a
        // pointer the directory alone holds.
        let root_writes = (0..merks.len())
            .map(|merk| {
                (!hosted_here[merk]).then(|| RootWrite {
                    was: resolver.roots[merk],
                    now: root_targets[merk],
                    anchored: merks[merk].anchored,
                })
            })
            .collect();
        Ok(ResolvedBatch {
            nodes,
            hosted,
            removed,
            root_writes,
            root_targets,
        })
    }
}
