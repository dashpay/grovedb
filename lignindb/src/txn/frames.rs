//! The private frames of a write transaction, and how a batch is applied
//! to them by key.

use std::collections::{HashMap, HashSet};

use lignin_format::{
    tree_page::{HashField, Side},
    CellAddress, CommitVersion,
};
use lignin_store::alloc::PageAllocator;
use lignin_tree::{
    directory::{Directory, DirectoryError, Dropped, Entry, Host, MerkId, Root, RootChange},
    drop::{self, DropError, Flats, Leaving as Left},
    flat::Private,
    fragment::{
        FragmentError, Fragments, HostedWrite, LinkWrite, NodeWrite, Report, RootWrite, Target,
        ValueWrite, Write,
    },
    hash::{Hashes, Stale},
    node::{Lease, NodeError, NodeHandle},
    value::{TreeValues, ValueSource},
};

use super::{
    error::{BatchError, TxnError},
    key::{Column, Subtree},
    pages::{find, follow, NodePages, TreePages},
    path::{FlatWriter, Idle, WritePath, IDLE},
    write::{
        Applied, Batch, Hosted, LinkPut, MerkWrite, RecordWrite, RootPut, SubtreeWrite, ValuePut,
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
}

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
    entering: HashSet<Subtree>,
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
    /// The nodes each Merk's change set writes, by key.
    written: Vec<HashMap<&'a [u8], usize>>,
    by_subtree: HashMap<Subtree, usize>,
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
            return Ok(Some(Target::Node(*node)));
        }
        Ok(find(tree, self.roots[merk], key)?.map(Target::At))
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
                SubtreeWrite::Enter { .. } => continue,
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
        for write in batch.subtrees {
            let SubtreeWrite::Enter { subtree, host, .. } = write else {
                continue;
            };
            if self.entered(&view, subtree)?.is_some() {
                return Err(BatchError::Entered { subtree: *subtree }.into());
            }
            if let Some(host) = host
                && self.entered(&view, &host.subtree)?.is_none()
            {
                let subtree = host.subtree;
                return Err(BatchError::NotEntered { subtree }.into());
            }
            view.entering.insert(*subtree);
        }
        Ok(view)
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

    /// Applies `batch` to the frames: the change sets of its Merks as one
    /// write of the tree layer, in which the root of each Merk that leaves
    /// is named as dropped, then what the write did to the subtree
    /// directory, and then its records.
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
        let applied = self.apply_merks(path, batch, &view, made)?;
        // The flat records of a dropped subtree went with its drop, in
        // every column. A cleared one keeps its own (`tree-layout.md` D6)
        // but for its data, which a clear takes: the same lazy range
        // delete, which reads and writes the two ends of the range and
        // leaves the pages between to the reclaimer.
        for left in view.leaving.iter().filter(|left| !left.dropped) {
            let data = path.keyspaces.of(Column::Data);
            let prefix = left.subtree.as_bytes();
            (path.flat_drops)
                .unlink_prefix(&mut *self.flat, &data, prefix, self.base)
                .map_err(Refused::dropping)?;
        }
        for record in batch.records {
            let tree = path.keyspaces.of(record.unit().column);
            let subtree = record.unit().subtree;
            let done = match *record {
                RecordWrite::Put { key, value, .. } => {
                    stored_key(&mut self.key, &subtree, key);
                    tree.put(&mut *self.flat, &self.key, value)
                }
                RecordWrite::Delete { key, .. } => {
                    stored_key(&mut self.key, &subtree, key);
                    tree.delete(&mut *self.flat, &self.key).map(|_| ())
                }
                RecordWrite::DeleteRange { start, end, .. } => {
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

    fn apply_merks(
        &mut self,
        path: &WritePath<'s>,
        batch: &Batch<'_>,
        view: &Subtrees,
        made: Option<&[bool]>,
    ) -> Result<Applied, Refused> {
        self.empty.clear();
        let merks = batch.merks;
        if merks.is_empty() && batch.subtrees.is_empty() {
            return Ok(Applied::default());
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
        let resolver = Resolver {
            merks,
            roots,
            written,
            by_subtree,
        };

        // What every name of the batch stands for is found before anything
        // is changed, so a batch that names what is not there is refused
        // with every page as it was.
        let mut held = Vec::new();
        let resolved = self.resolve(view, &resolver, total, &mut held);
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
        let node_writes: Vec<NodeWrite<'_>> = (merks.iter().flat_map(|merk| merk.nodes))
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
        // into them would still be followed. Every handle that was read
        // before ends here: the write is made under a new lease, and
        // reports its nodes under that one (PN8).
        let lease = self.tree.lease();
        if !view.leaving.is_empty() {
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
        let puts = merks.iter().flat_map(|merk| merk.nodes);
        let marks = !self.hashes.is_empty() || puts.clone().any(|put| !put.stale.is_empty());
        for (put, node) in puts.zip(&report.nodes).filter(|_| marks) {
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

        // The directory: the Merks that are entered, each so that it starts
        // with no flat record, and then every root the write changed. A
        // subtree that the directory refuses has changed no page: where
        // nothing of the batch was written before it, the frames are as
        // they were.
        let mut untouched = merks.is_empty() && view.leaving.is_empty();
        for write in batch.subtrees {
            let SubtreeWrite::Enter {
                subtree,
                tree_type,
                host,
            } = write
            else {
                continue;
            };
            let host = host.as_ref().map(|host| Host {
                parent: host.subtree.as_bytes(),
                key: host.key,
                position: host.position,
            });
            let prefix = subtree.as_bytes();
            match drop::enter(&self.directory, &mut flats, prefix, tree_type, host) {
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
        Ok(Applied {
            carried: (report.carried.iter())
                .map(|carried| (carried.from, carried.to))
                .collect(),
            nodes: report.nodes,
        })
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

    /// Finds what every key of the batch names: the cell of each written
    /// and each deleted node, the child of each link, and the root of each
    /// Merk, by a hint that is still good or by the key (PN8).
    fn resolve<'a>(
        &mut self,
        view: &Subtrees,
        resolver: &Resolver<'a>,
        total: usize,
        held: &mut Vec<lignin_format::value_log::PendingValue>,
    ) -> Result<ResolvedBatch<'a>, TxnError> {
        let merks = resolver.merks;
        let mut root_targets = Vec::with_capacity(merks.len());
        for merk in 0..merks.len() {
            root_targets.push(resolver.root(&mut self.tree, merk)?);
        }
        let mut nodes = Vec::with_capacity(total);
        let mut hosted: Vec<Vec<Option<Target>>> = Vec::new();
        let mut removed = Vec::new();
        let mut hosted_here = vec![false; merks.len()];
        for (merk, write) in merks.iter().enumerate() {
            let subtree = write.subtree;
            for put in write.nodes {
                // No hint is followed in a batch that drops or clears a
                // Merk: its cells stay in their pages, and a handle from
                // before would name one of them.
                let hint = put.at.filter(|_| view.leaving.is_empty());
                let hinted = hint.and_then(|handle| self.follow(handle, put.key));
                let at = match hinted {
                    Some(at) => Some(at),
                    None => find(&mut self.tree, resolver.roots[merk], put.key)?,
                };
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
                let value = match put.value {
                    ValuePut::Keep if at.is_none() => {
                        return Err(BatchError::NoValue { subtree }.into())
                    }
                    ValuePut::Keep => ValueWrite::Keep,
                    // The length alone says where a value goes, and a
                    // value of no bytes is never out of line.
                    ValuePut::Value(value) => {
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
