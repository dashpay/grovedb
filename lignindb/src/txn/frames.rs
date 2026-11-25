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
    bulk::{HostedLoad, LinkLoad, LoadCounters, LoadParams, Loader, NodeLoad},
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
    value::{read_placed, TreeValues, ValueBuf, ValueSource},
};

use super::{
    below::Below,
    error::{BatchError, Differs, HostName, TxnError, WholeFault},
    key::{Column, Subtree},
    load::{
        entry_firsts, is_load, load_error, load_order, stated_hashes, LoadPages, Loaded, Placing,
        Source, Stated, StoredValue,
    },
    pages::{find, follow, NodePages, TreePages},
    path::{FlatWriter, Idle, WritePath, IDLE},
    unlinked,
    view::FlatKinds,
    write::{
        Applied, Batch, Entering, HostNode, HostPut, Hosted, LinkPut, MerkWrite, Moving,
        RecordWrite, RootPut, SubtreeWrite, ValuePut,
    },
    write_set::{Again, WriteSet},
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

/// How often what [`Frames::below`] found by the index of the frames was
/// held to what every written subtree says, for the tests of this thread.
#[cfg(all(test, not(loom)))]
pub(crate) mod held {
    use std::cell::Cell;

    /// The calls that were held, on this thread.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub(crate) struct Compared {
        /// Every call that was held.
        pub(crate) calls: u64,
        /// Those of a batch that takes a Merk with a node: the index was
        /// asked.
        pub(crate) asked: u64,
        /// Those that found a subtree below.
        pub(crate) found: u64,
        /// The subtrees that were found below, over all the calls.
        pub(crate) subtrees: u64,
    }

    thread_local! {
        static COMPARED: Cell<Compared> = const {
            Cell::new(Compared {
                calls: 0,
                asked: 0,
                found: 0,
                subtrees: 0,
            })
        };
    }

    pub(super) fn count(asked: bool, subtrees: usize) {
        let mut compared = COMPARED.get();
        compared.calls += 1;
        compared.asked += u64::from(asked);
        compared.found += u64::from(subtrees > 0);
        compared.subtrees += subtrees as u64;
        COMPARED.set(compared);
    }

    /// What was held on this thread since it began.
    pub(crate) fn compared() -> Compared {
        COMPARED.get()
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
    /// The tree types that keep flat records, as the batch that is
    /// applied was given them, or `None` for a batch that was given
    /// nothing: whoever applies a batch says it before
    /// ([`Self::apply`]).
    pub(crate) kinds: Option<FlatKinds>,
    /// The subtrees of whose Merk an operation that is laid out on these
    /// frames wrote a node or removed one (`noded`), and those into whose
    /// data column one put a flat record or deleted one (`recorded`),
    /// each since a batch last named the subtree in an enter that made
    /// its entry, a clear or a drop. What a batch removes under a subtree
    /// if it is entered (`SubtreeWrite::RemoveIfEntered`) is in neither:
    /// it is removed from whatever the subtree holds. A flat record of one of the first,
    /// and a change set of one of the second, is judged by the tree type
    /// of the subtree ([`Self::apply`]). What is known of a Merk that a
    /// move takes goes with it to its new prefix.
    ///
    /// A subtree that went with one above it is not named by the batch
    /// that took it, and what is kept of it stays here. It refuses no
    /// batch while the subtree is not entered, and the batch that enters
    /// a subtree under its prefix again is not asked ([`Self::apply`]).
    /// The two are of these pages, so they are made again with them.
    noded: HashSet<Subtree>,
    recorded: HashSet<Subtree>,
    /// Whether the batch that was applied last was taken by the tree type
    /// of a subtree: the pages asked the tree type for a flat record or
    /// for a change set of it ([`Self::apply`]). The write set is told of
    /// an operation that is taken so when it is added
    /// (`WriteSet::tell_both`).
    pub(crate) both: bool,
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
    /// What the commit holds to its host: the subtrees whose entries an
    /// enter if absent under an element made in these frames
    /// ([`Self::unhosted`]), and those that a clear applied to them
    /// cleared ([`Self::pointed_at`]), each until a batch drops the
    /// subtree or makes its entry anew. A subtree that went with one
    /// above it stays here, and is passed while it is not entered. What
    /// is kept of a Merk that a move takes goes with it to its new
    /// prefix. The two are of these pages, so they are made again with
    /// them.
    if_absent: HashSet<Subtree>,
    cleared: HashSet<Subtree>,
    /// The subtrees that left with the moves of the batch that was applied
    /// last.
    left: Vec<Subtree>,
    /// For each Merk that a clear applied to these frames entered again
    /// under another id (`Directory::clear`), the id it had before every
    /// such clear, by the id it has since.
    renewed: HashMap<MerkId, MerkId>,
    /// The values that the loads of the batch that was applied last put
    /// into the value log, each by the place of its node among the entries
    /// of the batch's change sets, in ascending order: for the write set,
    /// which takes them. A batch that is applied again leaves none here.
    stored: Vec<(u32, StoredValue)>,
    /// The subtrees that the batch which was applied last dropped because
    /// it found them entered ([`SubtreeWrite::DropIfEntered`]).
    took: Vec<Subtree>,
    /// Which of the subtrees that the write set has an entry under are
    /// asked whether they are below a Merk that a batch takes
    /// ([`Self::below`], `super::below`). It is of the directory of these
    /// frames, so it is made again with them, and starts empty.
    pub(crate) written_below: Below,
}

/// A batch as these frames lay it out ([`Frames::lay`]): with a plain drop
/// for each drop that names a node and holds, and for each drop of a
/// subtree if it is entered that takes one, and without what it removes
/// under a subtree that is not entered.
struct Laid<'a> {
    subtrees: Vec<SubtreeWrite<'a>>,
    merks: Vec<MerkWrite<'a>>,
    records: Vec<RecordWrite<'a>>,
    /// The subtrees that a drop of a subtree if it is entered takes.
    took: Vec<Subtree>,
    /// For each Merk of the batch as it was stated, whether its change
    /// set is laid out.
    kept: Vec<bool>,
}

impl Laid<'_> {
    fn batch(&self) -> Batch<'_> {
        Batch {
            records: &self.records,
            merks: &self.merks,
            subtrees: &self.subtrees,
        }
    }
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
    /// among the nodes of that change set.
    written: Vec<HashMap<&'a [u8], usize>>,
    by_subtree: HashMap<Subtree, usize>,
    /// For each node of the batch, its place among the nodes of the tree
    /// write, or none for a node that the batch leaves unlinked and for a
    /// node of a load, which the loader writes.
    places: Vec<Option<usize>>,
    /// The root of each Merk that the batch loads, where the loader put
    /// it, and nothing for a batch with no load. The nodes of such a Merk
    /// are in their pages before any other name of the batch is looked
    /// for, and none of them is written again.
    loaded: Vec<Option<CellAddress>>,
}

impl Resolver<'_> {
    /// The place in the batch of the change set of `subtree`, if the batch
    /// loads that Merk.
    fn load(&self, subtree: &Subtree) -> Option<usize> {
        let merk = self.by_subtree.get(subtree).copied();
        merk.filter(|merk| is_load(&self.merks[*merk]))
    }

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
            return Ok(self.places[self.offsets[merk] + node].map(Target::Node));
        }
        Ok(find(tree, self.roots[merk], key)?.map(Target::At))
    }

    /// The node of the Merk `merk` that the batch writes under `key`, by
    /// its place among the nodes of that Merk's change set.
    fn local(&self, merk: usize, key: &[u8]) -> Option<usize> {
        self.written[merk].get(key).copied()
    }

    /// Where the loader put the root of the Merk `merk`, if the batch
    /// loads that Merk.
    fn loaded(&self, merk: usize) -> Option<CellAddress> {
        self.loaded.get(merk).copied().flatten()
    }

    /// The root of the Merk `merk` after the batch.
    fn root(&self, tree: &mut TreePages<'_>, merk: usize) -> Result<Option<Target>, TxnError> {
        if let Some(root) = self.loaded(merk) {
            return Ok(Some(Target::At(root)));
        }
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

/// What the loads of one batch need before a page is started
/// ([`Frames::plan_load`]).
struct LoadPlan {
    /// The loads, by their place in the batch, a hosted one before its
    /// host.
    order: Vec<usize>,
    /// The root of each Merk that a node of a load hosts and that the
    /// batch does not load.
    outside: HashMap<Subtree, Option<CellAddress>>,
    loader: Loader,
}

/// What the loads of one batch came to ([`Frames::load`]).
struct Load {
    /// The root of each Merk of the batch that was loaded.
    roots: Vec<Option<CellAddress>>,
    /// The handles of the nodes of each load, in the order of its change
    /// set.
    handles: Vec<Vec<NodeHandle>>,
    /// The bytes in use in the emptiest page the loader filled and left
    /// before the loads ended ([`Applied::least_filled`]).
    least_filled: Option<usize>,
    counters: LoadCounters,
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

    /// Whether the frames keep a subtree that the commit holds to its
    /// host ([`Awake::unhosted`], [`Awake::pointed_at`]).
    pub(crate) fn holds_to_hosts(&self) -> bool {
        !self.if_absent.is_empty() || !self.cleared.is_empty()
    }

    /// The same frames, with all they hold, bound to `guard`.
    fn under<'q, H>(self, guard: &'q H) -> Frames<'s, 'q, H>
    where
        's: 'q,
        H: lignin_tree::OperationGuard,
    {
        Frames {
            kinds: self.kinds,
            noded: self.noded,
            recorded: self.recorded,
            both: self.both,
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
            if_absent: self.if_absent,
            cleared: self.cleared,
            left: self.left,
            renewed: self.renewed,
            stored: self.stored,
            took: self.took,
            written_below: self.written_below,
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
            kinds: None,
            noded: HashSet::new(),
            recorded: HashSet::new(),
            both: false,
            dropped: Vec::new(),
            flat: FlatFrames(Some(flat)),
            hashes: Hashes::new(),
            fragments: Fragments::new(path.layout),
            key: Vec::new(),
            empty: Vec::new(),
            unlinked: HashMap::new(),
            own: HashSet::new(),
            if_absent: HashSet::new(),
            cleared: HashSet::new(),
            left: Vec::new(),
            renewed: HashMap::new(),
            stored: Vec::new(),
            took: Vec::new(),
            written_below: Below::default(),
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

    /// The subtrees that the batch which was applied last dropped because
    /// it found them entered under the node it names
    /// ([`SubtreeWrite::DropIfEntered`]).
    pub(crate) fn took(&self) -> &[Subtree] {
        &self.took
    }

    /// The id that the Merk which has `id` in these frames had before the
    /// clears that were applied to them: its own, where none of them gave
    /// it another.
    pub(crate) fn before_clears(&self, id: MerkId) -> MerkId {
        self.renewed.get(&id).copied().unwrap_or(id)
    }

    /// Takes the places of the values that the loads of the batch that
    /// was applied last put into the value log, as the write set keeps
    /// them ([`Again::stored`]). The frames keep none of them.
    pub(crate) fn take_stored(&mut self) -> Vec<(u32, StoredValue)> {
        std::mem::take(&mut self.stored)
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

    /// The first, in the order of their prefixes, of the subtrees whose
    /// entries an enter if absent under an element made in these frames
    /// that is entered under a node which a search of its host's Merk
    /// for the host's key, from the root, does not find. A record that
    /// the tree of that Merk does not reach is not found.
    ///
    /// `wrote` says whether the newest write of the transaction under a
    /// key of the data column of a subtree is a node. The search is left
    /// out for a host of which it says so, unless these frames kept that
    /// node as an unlinked record. A cell that the tree does not reach,
    /// and that these frames keep no unlinked record for, is not told
    /// from one in the tree so.
    pub(crate) fn unhosted(
        &mut self,
        wrote: impl Fn(&Subtree, &[u8]) -> bool,
    ) -> Result<Option<Subtree>, TxnError> {
        let mut subtrees: Vec<Subtree> = self.if_absent.iter().copied().collect();
        subtrees.sort_unstable();
        for subtree in subtrees {
            let Some(entry) = self.entry(&subtree)? else {
                continue;
            };
            let Some(host) = entry.host() else {
                continue;
            };
            let above = Subtree::new(*host.parent);
            stored_key(&mut self.key, &above, host.key);
            if wrote(&above, host.key) && !self.own.contains(&self.key) {
                continue;
            }
            let root = self.root(&above)?;
            if find(&mut self.tree, root, host.key)?.is_none() {
                return Ok(Some(subtree));
            }
        }
        Ok(None)
    }

    /// The first, in the order of their prefixes, of the subtrees that a
    /// clear applied to these frames cleared that is entered with no
    /// node, under an element whose cell still has a pointer at the
    /// subtree's position, or under a node that is none. One keyed search
    /// of the host's Merk and a read of the host's cell are made for
    /// each that is entered so. A subtree that has a node is passed: its
    /// pointer is its writers' to state.
    pub(crate) fn pointed_at(&mut self) -> Result<Option<Subtree>, TxnError> {
        let mut subtrees: Vec<Subtree> = self.cleared.iter().copied().collect();
        subtrees.sort_unstable();
        for subtree in subtrees {
            let Some(entry) = self.entry(&subtree)? else {
                continue;
            };
            if entry.root().is_some() {
                continue;
            }
            let Some(host) = entry.host() else {
                continue;
            };
            let root = self.root(&Subtree::new(*host.parent))?;
            let Some(at) = find(&mut self.tree, root, host.key)? else {
                return Ok(Some(subtree));
            };
            let position = usize::from(host.position);
            let pointer = self.tree.page(at.page(), |page| {
                let node = page.node(at.slot())?;
                Some(node.hosted_roots().nth(position).flatten())
            })?;
            if pointer.ok_or(NodeError::NoNode { address: at })?.is_some() {
                return Ok(Some(subtree));
            }
        }
        Ok(None)
    }

    /// What `batch` comes to where it is laid out on these frames, with
    /// nothing changed, or `None` for a batch that states nothing which
    /// is judged here ([`Batch::is_judged`]).
    ///
    /// - A drop that names a node ([`SubtreeWrite::DropUnder`]) is
    ///   refused where its subtree is not entered, and where it is
    ///   entered under another node or under none. Otherwise it is a
    ///   drop.
    /// - A drop of a subtree if it is entered
    ///   ([`SubtreeWrite::DropIfEntered`]) is a drop where the subtree is
    ///   entered under the node it names and is below no Merk that the
    ///   batch takes what is below of: one that it drops, one that it
    ///   clears and that has a node, and one that another such drop
    ///   finds entered. Otherwise it is nothing.
    /// - What the batch removes under a subtree that is not entered once
    ///   its subtrees have left, are moved and are entered
    ///   ([`SubtreeWrite::RemoveIfEntered`]) is left out.
    ///
    /// `took` is given for a batch that is applied again: the subtrees
    /// that its drops of a subtree if it is entered took in the operation
    /// that took the batch. Such a drop that took nothing then and finds
    /// its subtree entered under the node now refuses the batch
    /// ([`BatchError::EnteredSince`]): a commit beside the transaction
    /// entered the subtree since.
    fn lay<'a>(
        &self,
        batch: &Batch<'a>,
        took: Option<&[Subtree]>,
    ) -> Result<Option<Laid<'a>>, TxnError> {
        if !batch.is_judged() {
            return Ok(None);
        }
        self.not_under(batch)?;
        let hangs = |entry: &Entry, host: &HostNode<'_>| {
            let under = |at: Host<'_>| at.parent == host.subtree.as_bytes() && at.key == host.key;
            entry.host().is_some_and(under)
        };
        // The Merks that the batch takes what is below of, whatever is
        // found here, and those that a drop of a subtree if it is
        // entered finds: each by its place among the writes.
        let mut above: Vec<MerkId> = Vec::new();
        let mut found: Vec<(usize, Entry)> = Vec::new();
        let mut subtrees: Vec<Option<SubtreeWrite<'a>>> = Vec::with_capacity(batch.subtrees.len());
        for (place, write) in batch.subtrees.iter().enumerate() {
            let laid = match *write {
                SubtreeWrite::Drop { subtree } | SubtreeWrite::Clear { subtree } => {
                    let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
                    let cleared = matches!(write, SubtreeWrite::Clear { .. });
                    let takes = |entry: &Entry| !cleared || entry.root().is_some();
                    above.extend(entry.filter(takes).map(|entry| entry.id()));
                    Some(*write)
                }
                SubtreeWrite::DropUnder { subtree, host, .. } => {
                    let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
                    let entry = entry.ok_or(BatchError::NotEntered { subtree })?;
                    if !hangs(&entry, &host) {
                        return Err(BatchError::HungOtherwise { subtree }.into());
                    }
                    above.push(entry.id());
                    Some(SubtreeWrite::Drop { subtree })
                }
                SubtreeWrite::DropIfEntered { subtree, host } => {
                    let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
                    found.extend(
                        entry
                            .filter(|entry| hangs(entry, &host))
                            .map(|entry| (place, entry)),
                    );
                    None
                }
                SubtreeWrite::RemoveIfEntered { .. } => None,
                SubtreeWrite::NotUnder { .. } => None,
                SubtreeWrite::Enter { .. }
                | SubtreeWrite::EnterIfAbsent { .. }
                | SubtreeWrite::Convert { .. }
                | SubtreeWrite::Move { .. } => Some(*write),
            };
            subtrees.push(laid);
        }
        // The Merks that take what is below them: those of `above`, and
        // those that were found.
        let taking: HashSet<MerkId> = (above.iter().copied())
            .chain(found.iter().map(|(_, entry)| entry.id()))
            .collect();
        let before: Option<HashSet<Subtree>> = took.map(|took| took.iter().copied().collect());
        let mut took = Vec::new();
        for (place, entry) in &found {
            if entry.lineage().any(|id| taking.contains(&id)) {
                continue;
            }
            let subtree = batch.subtrees[*place].subtree();
            if (before.as_ref()).is_some_and(|before| !before.contains(&subtree)) {
                return Err(BatchError::EnteredSince { subtree }.into());
            }
            subtrees[*place] = Some(SubtreeWrite::Drop { subtree });
            took.push(subtree);
        }
        let subtrees: Vec<SubtreeWrite<'a>> = subtrees.into_iter().flatten().collect();
        let (mut merks, mut records) = (batch.merks.to_vec(), batch.records.to_vec());
        let mut kept = vec![true; merks.len()];
        if batch.removing_if_entered().next().is_some() {
            let view = self.subtrees(&Batch {
                subtrees: &subtrees,
                ..Batch::default()
            })?;
            let mut absent = HashSet::new();
            for subtree in batch.removing_if_entered() {
                if self.entered(&view, &subtree)?.is_none() {
                    absent.insert(subtree);
                }
            }
            kept = (merks.iter())
                .map(|merk| !absent.contains(&merk.subtree))
                .collect();
            merks.retain(|merk| !absent.contains(&merk.subtree));
            records.retain(|record| !absent.contains(&record.unit().subtree));
        }
        Ok(Some(Laid {
            subtrees,
            merks,
            records,
            took,
            kept,
        }))
    }

    /// Refuses `batch` where a subtree that it states as not entered
    /// under a node ([`SubtreeWrite::NotUnder`]) is entered under that
    /// node ([`BatchError::EnteredSince`]), as the directory has it before
    /// the batch.
    pub(crate) fn not_under(&self, batch: &Batch<'_>) -> Result<(), TxnError> {
        for (subtree, host) in batch.not_under() {
            let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
            let under = |at: Host<'_>| at.parent == host.subtree.as_bytes() && at.key == host.key;
            if entry.is_some_and(|entry| entry.host().is_some_and(under)) {
                return Err(BatchError::EnteredSince { subtree }.into());
            }
        }
        Ok(())
    }

    /// What `batch` does to subtrees as a whole, checked against the
    /// directory with nothing changed. The batch is one as it is laid out
    /// ([`Self::lay`]).
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
                SubtreeWrite::DropUnder { .. }
                | SubtreeWrite::DropIfEntered { .. }
                | SubtreeWrite::RemoveIfEntered { .. }
                | SubtreeWrite::NotUnder { .. } => {
                    return Err(TxnError::Damaged(
                        "a batch is applied that was not laid out",
                    ));
                }
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
        let leaving: HashSet<MerkId> = (view.leaving.iter()).map(|left| left.entry.id()).collect();
        for left in &view.leaving {
            if left.entry.lineage().any(|id| leaving.contains(&id)) {
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

    /// Those of the subtrees that `set` has an entry under
    /// ([`WriteSet::written`]) that are below a Merk which `batch` drops
    /// or clears, as the directory has them before the batch, in the
    /// order of their prefixes: what the transaction wrote there goes
    /// with the drop.
    ///
    /// Only a Merk with a node takes what is below it
    /// ([`Subtrees::below_leaving`]): a clear of one with no node leaves
    /// what is entered below it, and a drop of one is refused where a
    /// subtree is. So for a batch that takes no Merk with a node the
    /// answer is none, and nothing is asked for it. For one that takes
    /// such a Merk, the index of these frames is brought up to date
    /// ([`Self::list_written`]), and of the subtrees that it lists under
    /// the ids of the Merks which the batch takes, each that `set` has
    /// an entry under is asked of the directory, once ([`Below`]). One
    /// that `set` has no entry under any more is forgotten, unopened.
    /// `opened` counts the entries of the directory that are opened for
    /// the two: one for each subtree that is asked.
    pub(crate) fn below(
        &mut self,
        batch: &Batch<'_>,
        set: &WriteSet,
        opened: &mut u64,
    ) -> Result<Vec<Subtree>, TxnError> {
        let laid = self.lay(batch, None)?;
        let laid = laid.as_ref().map(Laid::batch);
        let view = self.subtrees(laid.as_ref().unwrap_or(batch))?;
        let below = self.below_listed(&view, set, opened)?;
        #[cfg(any(test, feature = "held_reads"))]
        self.held_to_every_written(&view, set, &below)?;
        Ok(below)
    }

    /// [`Self::below`], for a batch whose drops and clears are `view`'s.
    fn below_listed(
        &mut self,
        view: &Subtrees,
        set: &WriteSet,
        opened: &mut u64,
    ) -> Result<Vec<Subtree>, TxnError> {
        let taking = view.leaving.iter().filter(|left| left.root.is_some());
        let taking: Vec<MerkId> = taking.map(|left| left.entry.id()).collect();
        if taking.is_empty() {
            return Ok(Vec::new());
        }
        self.list_written(set, opened)?;
        let index = &self.written_below;
        let mut asked: Vec<Subtree> = taking.iter().flat_map(|id| index.under(*id)).collect();
        asked.sort_unstable();
        asked.dedup();
        let mut below = Vec::new();
        for subtree in asked {
            // A scope that was not laid out took its writes with it: the
            // set has no entry under the subtree any more.
            if !set.writes_under(&subtree) {
                self.written_below.forget(&subtree);
                continue;
            }
            let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
            *opened += 1;
            if entry.is_some_and(|entry| view.below_leaving(&entry)) {
                below.push(subtree);
            }
        }
        Ok(below)
    }

    /// Brings the index of what the transaction wrote below a Merk up to
    /// date with the directory as it is ([`Below`]): the subtrees that
    /// `set` has an entry under and numbered after the count the index
    /// last took are asked, and then the subtrees that the batches which
    /// were applied since said to ask again. A read that fails ends the
    /// call, and leaves what was not asked to the next one.
    fn list_written(&mut self, set: &WriteSet, opened: &mut u64) -> Result<(), TxnError> {
        for subtree in set.written_after(self.written_below.taken()) {
            self.ask_below(set, subtree, opened)?;
        }
        self.written_below.take(set.first_writes());
        for subtree in self.written_below.again() {
            self.ask_below(set, subtree, opened)?;
        }
        Ok(())
    }

    /// Asks the directory for `subtree` and lists it in the index under
    /// the ids of the Merks that its entry has it below, or under
    /// nothing where no entry in use is under its prefix. A subtree that
    /// `set` has no entry under is forgotten, and nothing is opened for
    /// it.
    fn ask_below(
        &mut self,
        set: &WriteSet,
        subtree: Subtree,
        opened: &mut u64,
    ) -> Result<(), TxnError> {
        if !set.writes_under(&subtree) {
            self.written_below.forget(&subtree);
            return Ok(());
        }
        let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
        *opened += 1;
        let above = entry.as_ref().map(Entry::lineage);
        self.written_below
            .list(subtree, above.into_iter().flatten());
        Ok(())
    }

    /// Says to the index of what the transaction wrote below a Merk what
    /// a batch that was applied did to the directory ([`Below`], "When it
    /// changes"): which of the subtrees it has asked are asked again.
    /// `view` is what the batch did to subtrees as a whole, as it was
    /// found before the batch.
    fn ask_below_again(&mut self, view: &Subtrees) {
        let index = &mut self.written_below;
        for left in &view.leaving {
            if left.root.is_some() {
                index.left(left.entry.id());
            }
            if left.dropped {
                index.ask_again(&left.subtree);
            }
        }
        let moved = view.moved.keys().chain(&view.taken);
        for subtree in moved.chain(&view.entering) {
            index.ask_again(subtree);
        }
    }

    /// [`Self::below`] as it was made before the frames had an index:
    /// every subtree of `written` is opened in the directory and asked.
    #[cfg(any(test, feature = "held_reads"))]
    fn below_of_every_written(
        &self,
        view: &Subtrees,
        written: impl Iterator<Item = Subtree>,
    ) -> Result<Vec<Subtree>, TxnError> {
        let mut below = Vec::new();
        for subtree in written {
            let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
            if entry.is_some_and(|entry| view.below_leaving(&entry)) {
                below.push(subtree);
            }
        }
        Ok(below)
    }

    /// Holds `below`, what [`Self::below`] found by the index, to what
    /// every subtree that `set` has an entry under says
    /// ([`Self::below_of_every_written`]): the same subtrees in the same
    /// order, or a panic that shows both. That is so in a test build of
    /// this crate, and in a build with the feature `held_reads`. A read
    /// of the directory that fails here ends the call with its error
    /// before the two are compared.
    #[cfg(any(test, feature = "held_reads"))]
    fn held_to_every_written(
        &self,
        view: &Subtrees,
        set: &WriteSet,
        below: &[Subtree],
    ) -> Result<(), TxnError> {
        let every = self.below_of_every_written(view, set.written())?;
        assert_eq!(
            below, every,
            "what the transaction wrote below a Merk that a batch takes: by the index of the \
             frames, and by every subtree the write set has an entry under"
        );
        #[cfg(all(test, not(loom)))]
        held::count(
            view.leaving.iter().any(|left| left.root.is_some()),
            below.len(),
        );
        Ok(())
    }

    /// Whether `subtree`, once the subtrees of `batch` have left, are
    /// entered and are stated anew, is of a tree type that keeps its data
    /// as flat records, by what the batch was given ([`Self::kinds`]): the
    /// tree type that the batch states for it, or the one of its entry.
    /// An enter that finds the subtree entered states the tree type of
    /// its entry ([`same`]). A batch that was given nothing is answered
    /// no.
    fn keeps_flat(&self, batch: &Batch<'_>, subtree: &Subtree) -> Result<bool, TxnError> {
        let Some(kinds) = self.kinds else {
            return Ok(false);
        };
        let stated = batch.subtrees.iter().rev().find_map(|write| match *write {
            SubtreeWrite::Convert {
                subtree: named,
                tree_type,
                ..
            } if named == *subtree => Some(tree_type),
            _ => {
                let entering = write.entering();
                let entering = entering.filter(|entering| entering.subtree == *subtree);
                entering.map(|entering| entering.tree_type)
            }
        });
        if let Some(tree_type) = stated {
            return Ok(kinds(tree_type));
        }
        let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
        Ok(entry.is_some_and(|entry| kinds(entry.tree_type())))
    }

    /// Checks that `record`, a flat record of the data column of a
    /// subtree that holds nodes and takes flat records beside them, is
    /// one that goes beside them: a put or a delete of one key, under
    /// which the Merk, whose root is `root` before the change sets of the
    /// batch are applied, has no node, and under which these frames keep
    /// no node of the transaction as an unlinked record ([`Self::own`]).
    /// A key holds one record (ST1). A range delete is refused as a flat
    /// record among the nodes of a Merk is.
    ///
    /// `anew` says that the batch drops or clears the subtree, or enters
    /// it as one that is new: the unlinked records that these frames keep
    /// under its prefix are of the subtree that was there before.
    fn beside_nodes(
        &mut self,
        record: &RecordWrite<'_>,
        root: Option<CellAddress>,
        anew: bool,
    ) -> Result<(), TxnError> {
        let (unit, key) = match *record {
            RecordWrite::Put { unit, key, .. } | RecordWrite::Delete { unit, key } => (unit, key),
            RecordWrite::DeleteRange { .. } => {
                let unit = record.unit();
                return Err(BatchError::NotFlat { unit }.into());
            }
        };
        stored_key(&mut self.key, &unit.subtree, key);
        let unlinked = !anew && self.own.contains(&self.key);
        if unlinked || find(&mut self.tree, root, key)?.is_some() {
            return Err(BatchError::RecordUnderNode { unit }.into());
        }
        Ok(())
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
        let read = tree.shown(address.page(), |page, committed| {
            let node = page.node(address.slot())?;
            let mut buf = ValueBuf::new();
            let value = values.read_shown(source, node, committed, &mut buf);
            Some(value.map(<[u8]>::to_vec))
        })?;
        Ok(read.ok_or(NodeError::NoNode { address })??)
    }

    /// Applies `batch` to the frames: the change sets of its Merks as one
    /// write of the tree layer, in which the root of each Merk that leaves
    /// is named as dropped, then what the write did to the subtree
    /// directory, then the unlinked records its change sets leave and
    /// take, and then its records.
    ///
    /// The batch has passed [`check`](super::check::check). `again` is
    /// given for a batch that is applied again (WT7): it says of each Merk
    /// whether the transaction made its first node with this batch. Such a
    /// Merk has to be empty still, and the batch is refused if it is not
    /// (GM3). It also says where the values are that a load put into the
    /// value log when the batch was first applied.
    ///
    /// The change sets that are whole Merks are laid out first, by the
    /// loader, in pages of their own ([`Self::load`]), and `loaded` is
    /// told the values they put into the value log. A refusal after that
    /// leaves a part of the batch in the frames.
    ///
    /// What the batch states of a subtree as its writer found it is
    /// judged first, against the directory of these frames
    /// ([`Self::lay`]), and the batch is applied as that leaves it.
    pub(crate) fn apply(
        &mut self,
        path: &WritePath<'s>,
        batch: &Batch<'_>,
        again: Option<Again<'_>>,
        loaded: &mut Loaded,
    ) -> Result<Applied, Refused> {
        self.took.clear();
        let took = again.map(|again| again.took);
        let Some(laid) = self.lay(batch, took).map_err(Refused::clean)? else {
            return self.apply_laid(path, batch, &[], again, loaded);
        };
        // What is said of each Merk of the batch is said of those that
        // are laid out.
        let made: Vec<bool> = again.map_or_else(Vec::new, |again| {
            let made = again.made.iter().zip(&laid.kept);
            made.filter_map(|(made, kept)| kept.then_some(*made))
                .collect()
        });
        let again = again.map(|again| Again {
            made: &made,
            ..again
        });
        let removing: Vec<Subtree> = batch.removing_if_entered().collect();
        let applied = self.apply_laid(path, &laid.batch(), &removing, again, loaded)?;
        // A Merk whose change set was left out is one with no root to
        // the write set: it says nothing of the root.
        let mut empty = std::mem::take(&mut self.empty).into_iter();
        let empty = laid.kept.iter().map(|kept| match kept {
            true => empty.next().unwrap_or(true),
            false => true,
        });
        self.empty = empty.collect();
        self.took = laid.took;
        Ok(applied)
    }

    /// [`Self::apply`], for a batch as it is laid out ([`Self::lay`]).
    /// `removing` are the subtrees under which the batch removes if they
    /// are entered: what it removes there says nothing of what the
    /// subtree holds ([`Self::noded`]).
    fn apply_laid(
        &mut self,
        path: &WritePath<'s>,
        batch: &Batch<'_>,
        removing: &[Subtree],
        again: Option<Again<'_>>,
        loaded: &mut Loaded,
    ) -> Result<Applied, Refused> {
        let view = self.subtrees(batch).map_err(Refused::clean)?;
        self.hosts_moved(batch).map_err(Refused::clean)?;
        self.both = false;
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
            if unit.column != Column::Data {
                continue;
            }
            // The subtree holds nodes: its Merk has a root, the batch
            // holds a change set of it, or an operation that is laid out
            // wrote a node of it or removed one. The tree type of the
            // subtree says whether a flat record goes beside them (ST1),
            // and the record is one that does. What an earlier operation
            // wrote is not of a subtree that the batch drops, clears or
            // enters as one that is new.
            let anew = batch.leaves(subtree) || view.entering.contains(&subtree);
            let own = batch.merks.iter().any(|merk| merk.subtree == subtree);
            let wrote = !anew && self.noded.contains(&subtree);
            if root.is_some() || own || wrote {
                let kept = self.keeps_flat(batch, &subtree);
                if !kept.map_err(Refused::clean)? {
                    return Err(Refused::clean(BatchError::NotFlat { unit }));
                }
                (self.beside_nodes(record, root, anew)).map_err(Refused::clean)?;
                self.both = true;
            }
        }
        for merk in batch.merks {
            // A change set where an operation that is laid out wrote a
            // flat record into the data column of the subtree as the
            // batch has it: the tree type says that as well.
            let subtree = merk.subtree;
            let anew = batch.leaves(subtree) || view.entering.contains(&subtree);
            if !anew && self.recorded.contains(&subtree) {
                // What is known is of a subtree that is entered. One that
                // is not entered is not judged here: its change set goes
                // on to where the change sets are applied.
                let entered = self.entered(&view, &subtree).map_err(Refused::clean)?;
                if entered.is_none() {
                    continue;
                }
                let kept = self.keeps_flat(batch, &subtree);
                if !kept.map_err(Refused::clean)? {
                    return Err(Refused::clean(BatchError::NotMerk { subtree }));
                }
                self.both = true;
            }
        }
        let (applied, unlinked) = self.apply_merks(path, batch, &view, again, loaded)?;
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
                SubtreeWrite::Convert { .. }
                | SubtreeWrite::Move { .. }
                | SubtreeWrite::DropUnder { .. }
                | SubtreeWrite::DropIfEntered { .. }
                | SubtreeWrite::RemoveIfEntered { .. }
                | SubtreeWrite::NotUnder { .. } => continue,
            }
            // What the transaction left under a subtree goes with it, and
            // so does what is known of what it wrote there.
            let prefix = write.subtree();
            (self.own).retain(|key| !key.starts_with(prefix.as_bytes()));
            self.noded.remove(&prefix);
            self.recorded.remove(&prefix);
        }
        // What the commit holds to its host. The drops and the clears of
        // a batch come before its moves, and its enters after them.
        for write in batch.subtrees {
            match write {
                SubtreeWrite::Drop { subtree } => {
                    self.if_absent.remove(subtree);
                    self.cleared.remove(subtree);
                }
                SubtreeWrite::Clear { subtree } => drop(self.cleared.insert(*subtree)),
                _ => {}
            }
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
            // And so does what is known of what the transaction wrote
            // into it, of either kind.
            let hosted = [&mut self.if_absent, &mut self.cleared];
            for written in [&mut self.noded, &mut self.recorded]
                .into_iter()
                .chain(hosted)
            {
                let taken: Vec<(Subtree, bool)> = (moved.iter())
                    .map(|(from, to)| (*to, written.remove(from)))
                    .collect();
                for left in plan.left() {
                    written.remove(&Subtree::new(*left));
                }
                for (to, was) in taken {
                    match was {
                        true => drop(written.insert(to)),
                        false => drop(written.remove(&to)),
                    }
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
        // An entry that the batch made is no cleared one, and is held
        // to its host where an enter if absent under an element made it.
        for enter in batch.subtrees.iter().filter_map(SubtreeWrite::entering) {
            if !view.entering.contains(&enter.subtree) {
                continue;
            }
            self.cleared.remove(&enter.subtree);
            match enter.if_absent && enter.host.is_some() {
                true => drop(self.if_absent.insert(enter.subtree)),
                false => drop(self.if_absent.remove(&enter.subtree)),
            }
        }
        for subtree in unlinked.holding {
            self.unlinked.insert(subtree, true);
        }
        for (_, key) in &unlinked.taken {
            self.own.remove(key);
        }
        (self.own).extend(unlinked.kept.into_iter().map(|(key, _)| key));
        // A change set that writes no node and removes none says nothing
        // of what the subtree holds.
        let written = |merk: &&MerkWrite<'_>| !merk.nodes.is_empty() || !merk.removed.is_empty();
        let written = batch.merks.iter().filter(written);
        let said = |subtree: &Subtree| !removing.contains(subtree);
        (self.noded).extend(written.map(|merk| merk.subtree).filter(said));
        for record in batch.records {
            let tree = path.keyspaces.of(record.unit().column);
            let subtree = record.unit().subtree;
            if record.unit().column == Column::Data && said(&subtree) {
                self.recorded.insert(subtree);
            }
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
        self.ask_below_again(&view);
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
        again: Option<Again<'_>>,
        loaded: &mut Loaded,
    ) -> Result<(Applied, UnlinkedWrites), Refused> {
        self.empty.clear();
        self.left.clear();
        self.stored.clear();
        let merks = batch.merks;
        if merks.is_empty() && batch.subtrees.is_empty() {
            return Ok(Default::default());
        }
        let made = again.map(|again| again.made);
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
        for (merk, root) in merks.iter().zip(&roots).filter(|(merk, _)| is_load(merk)) {
            // A load is the whole of a Merk that had no root.
            if root.is_some() {
                let (subtree, fault) = (merk.subtree, WholeFault::HasRoot);
                return Err(Refused::clean(BatchError::NotWhole { subtree, fault }));
            }
        }
        let written = (merks.iter())
            .map(|merk| {
                let nodes = merk.nodes.iter().enumerate();
                nodes.map(|(node, put)| (put.key, node)).collect()
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
            loaded: Vec::new(),
        };

        // What every name of the batch stands for is found before anything
        // is changed, so a batch that names what is not there is refused
        // with every page as it was. First, where each node is and whether
        // the tree of its Merk reaches it once the batch is applied: a
        // node it does not reach is no node of the tree write, and neither
        // is a node of a load.
        let reach = self.reach(view, &resolver).map_err(Refused::clean)?;
        let mut place = 0;
        let loads =
            (merks.iter()).flat_map(|merk| std::iter::repeat_n(is_load(merk), merk.nodes.len()));
        let places = reach.linked.iter().zip(loads).map(|(linked, load)| {
            (*linked && !load).then(|| {
                place += 1;
                place - 1
            })
        });
        resolver.places = places.collect();
        let loose = (self.loose(path, view, &resolver, &reach)).map_err(Refused::clean)?;
        // A Merk that leaves keeps its cells in their pages, so a handle
        // into them would still be followed, and so would one into a Merk
        // that has another prefix afterwards. Every handle that was read
        // before ends here: the loads and the write are made under a new
        // lease, and report their nodes under that one (PN8).
        let lease = self.tree.lease();
        if view.ends_handles() {
            self.tree.set_lease(Lease::take());
        }
        // The loads, before the names of the other change sets are looked
        // for: a host of a loaded Merk is written with the cell of its
        // root.
        let mut handles: Vec<Vec<NodeHandle>> = Vec::new();
        let mut load = None;
        if merks.iter().any(is_load) {
            let stored = again.map_or(&[][..], |again| again.stored);
            let plan = match self.plan_load(path, batch, view, &resolver) {
                Ok(plan) => plan,
                Err(error) => {
                    self.tree.set_lease(lease);
                    return Err(Refused::clean(error));
                }
            };
            let done = self.load(path, batch, &resolver, plan, stored, loaded);
            let done = done.map_err(Refused::dirty)?;
            if again.is_some() {
                // The write set has the places of a batch that is applied
                // again: nobody takes these.
                self.stored = Vec::new();
            }
            (resolver.loaded, handles) = (done.roots, done.handles);
            load = Some((done.counters, done.least_filled));
        }
        // A refusal from here on leaves the pages of the loads in the
        // frames, whatever else it changed.
        let laden = load.is_some();
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
                self.tree.set_lease(lease);
                return Err(Refused {
                    dirty: laden,
                    ..Refused::clean(error)
                });
            }
        };
        // The nodes of the tree write: those the trees reach, but for the
        // nodes of the loads, which are in their pages.
        let linked = || {
            let puts = merks.iter().flat_map(|merk| merk.nodes);
            puts.zip(&resolver.places)
                .filter_map(|(put, place)| place.is_some().then_some(put))
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
                return Err(Refused {
                    dirty: laden,
                    ..Refused::clean(error)
                });
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
        // The root of a load is named with the cell it is in after the
        // write, so it is not told as carried as well: the directory had
        // no root for it before.
        let loaded_roots = &resolver.loaded;
        let hosted = (report.carried.iter().filter_map(RootChange::carried)).filter(|change| {
            !matches!(change, RootChange::Carried { from, .. } if loaded_roots.contains(&Some(*from)))
        });
        let changes = root_changes(named, hosted);
        #[cfg(all(test, not(loom)))]
        probe::count(&changes);
        if !changes.is_empty() {
            (self.directory.set_roots(&mut *self.flat, &changes)).map_err(Refused::dirty)?;
        }
        let unlinked = (0..total).filter(|node| !reach.linked[*node]).collect();
        // The handles in the order of the batch: those the write reports,
        // and for a batch with a load the nodes of each load among them,
        // where its change set stands. A cell of a load that the write
        // carried is where the write says.
        let nodes = match load {
            None => report.nodes,
            Some(_) => {
                let now: HashMap<CellAddress, NodeHandle> = (report.carried.iter())
                    .map(|carried| (carried.from, carried.to))
                    .collect();
                let mut reported = report.nodes.into_iter();
                let mut nodes = Vec::with_capacity(total);
                for ((merk, loaded), offset) in merks.iter().zip(handles).zip(&resolver.offsets) {
                    if is_load(merk) {
                        let now = |handle: NodeHandle| {
                            now.get(&handle.address()).copied().unwrap_or(handle)
                        };
                        nodes.extend(loaded.into_iter().map(now));
                        continue;
                    }
                    let places = &resolver.places[*offset..offset + merk.nodes.len()];
                    nodes.extend(reported.by_ref().take(places.iter().flatten().count()));
                }
                nodes
            }
        };
        let applied = Applied {
            carried: (report.carried.iter())
                .map(|carried| (carried.from, carried.to))
                .collect(),
            nodes,
            unlinked,
            load: load.map(|(counters, _)| counters),
            least_filled: load.and_then(|(_, least_filled)| least_filled),
        };
        Ok((applied, loose))
    }

    /// Whether the entry of `subtree` names `host` as the element that
    /// hosts it, once the subtrees of `batch` have left, are moved, are
    /// stated anew and are entered: `None` for a subtree that is not
    /// entered then.
    ///
    /// A subtree that a move of the batch brings a Merk to names no node
    /// of a load. The element that hosts the moved subtree is in the Merk
    /// its host was in before, and a Merk that is loaded had no node: the
    /// element that takes such a host's place hosts no Merk
    /// (`transactions.md` MV3). A subtree below the moved one is hosted
    /// in a Merk that goes with the move, which no change set of the
    /// batch writes.
    fn names_host(
        &self,
        batch: &Batch<'_>,
        view: &Subtrees,
        subtree: &Subtree,
        host: HostPut<'_>,
    ) -> Result<Option<bool>, TxnError> {
        let mut enters = batch.subtrees.iter().filter_map(SubtreeWrite::entering);
        if view.entering.contains(subtree)
            && let Some(enter) = enters.find(|enter| enter.subtree == *subtree)
        {
            return Ok(Some(enter.host == Some(host)));
        }
        if view.moved.contains_key(subtree) {
            return Ok(Some(false));
        }
        if view.taken.contains(subtree) {
            return Ok(None);
        }
        // The position that the batch states anew for the subtree, which
        // it then neither enters nor drops nor clears.
        let restated = batch.subtrees.iter().find_map(|write| match write {
            SubtreeWrite::Convert {
                subtree: stated,
                position,
                ..
            } if stated == subtree => *position,
            _ => None,
        });
        let names = |entry: &Entry| {
            entry.host().is_some_and(|named| {
                let position = restated.unwrap_or(named.position);
                (named.parent, named.key, position)
                    == (host.subtree.as_bytes(), host.key, host.position)
            })
        };
        if let Some(left) = view.leaving.iter().find(|left| left.subtree == *subtree) {
            return Ok((!left.dropped).then(|| names(&left.entry)));
        }
        let entry = self.directory.open(&*self.flat, subtree.as_bytes())?;
        let entry = entry.filter(|entry| !view.below_leaving(entry));
        Ok(entry.as_ref().map(names))
    }

    /// What the loads of `batch` need that is found before a page is
    /// started: the order they are laid out in, the roots of the Merks
    /// their nodes host that the batch does not load, and a loader. It
    /// changes nothing.
    ///
    /// A node of a load hosts a Merk only where the entry of the Merk
    /// names that node as its host (LD1): the cell of the node is written
    /// with the address of the Merk's root, and a root that the directory
    /// gives another host, or none, would have a pointer the directory
    /// does not know. A subtree that is not entered has no root to name.
    fn plan_load(
        &self,
        path: &WritePath<'s>,
        batch: &Batch<'_>,
        view: &Subtrees,
        resolver: &Resolver<'_>,
    ) -> Result<LoadPlan, TxnError> {
        let merks = batch.merks;
        let order = load_order(batch)?;
        let mut outside = HashMap::new();
        for &merk in &order {
            let write = &merks[merk];
            let not_its_host = || BatchError::NotWhole {
                subtree: write.subtree,
                fault: WholeFault::NotItsHost,
            };
            for node in write.nodes {
                let Hosted::Set(hosted) = node.hosted else {
                    continue;
                };
                for (position, subtree) in hosted.iter().enumerate() {
                    let host = HostPut {
                        subtree: write.subtree,
                        key: node.key,
                        position: u8::try_from(position).map_err(|_| not_its_host())?,
                    };
                    if self.names_host(batch, view, subtree, host)? == Some(false) {
                        return Err(not_its_host().into());
                    }
                    if resolver.load(subtree).is_none() && !outside.contains_key(subtree) {
                        let root = self.entered(view, subtree)?.flatten();
                        outside.insert(*subtree, root);
                    }
                }
            }
        }
        let first = merks[*order.first().expect("a batch with a load")].subtree;
        let loader = Loader::new(path.layout, LoadParams::DEFAULT);
        Ok(LoadPlan {
            order,
            outside,
            loader: loader.map_err(|error| load_error(error, None, first))?,
        })
    }

    /// Lays out the loads of `batch`: its change sets that are whole
    /// Merks, each a Merk that had no root (`crate::txn`, "A load").
    ///
    /// The loader is given the nodes of each, in the order of its change
    /// set, and puts their cells into pages it starts, all loads of the
    /// batch through the same pages. Nothing of the directory is changed
    /// and no page that was there is written: where the roots went is
    /// returned, and the write that follows names them, in the hosts and
    /// to the directory.
    ///
    /// - **The hashes are the ones the change set states**, which is what
    ///   the loader is answered with ([`Stated`]): no rule is known here.
    /// - **A value out of line goes into the value log** when its cell
    ///   gets its page ([`Placing`]), and the cell is written with the
    ///   pointer. `again` says where the values of a batch that is applied
    ///   again are: each is read from there and appended anew, for the
    ///   page its cell is in now.
    /// - **A Merk that a node of a load hosts** is one of the loads, which
    ///   is laid out before it, or a Merk the batch does not write, whose
    ///   root is where it was. Either is named by the address of its
    ///   root, and none is laid out inside its host: the cells of one
    ///   write of the loader are of one Merk ([`LoadPages`]).
    ///
    /// Whatever it returns, the frames may hold pages of the loads.
    fn load(
        &mut self,
        path: &WritePath<'s>,
        batch: &Batch<'_>,
        resolver: &Resolver<'_>,
        plan: LoadPlan,
        again: &[(u32, StoredValue)],
        loaded: &mut Loaded,
    ) -> Result<Load, TxnError> {
        let merks = batch.merks;
        let LoadPlan {
            order,
            outside,
            mut loader,
        } = plan;
        let firsts = entry_firsts(merks);
        let not_whole = |subtree, fault| TxnError::from(BatchError::NotWhole { subtree, fault });
        let mut placing = Placing::new(path, loaded);
        let mut pages = LoadPages::new(&mut self.tree);
        let mut roots = vec![None; merks.len()];
        let mut handles: Vec<Vec<NodeHandle>> = vec![Vec::new(); merks.len()];
        let mut buf = ValueBuf::new();
        let mut hosted_roots = Vec::new();
        for &merk in &order {
            let write = &merks[merk];
            let subtree = write.subtree;
            let keys = &resolver.written[merk];
            pages.start(keys);
            let hashes = stated_hashes(write, keys);
            let mut subtrees = vec![None; write.nodes.len()];
            for (at, node) in write.nodes.iter().enumerate() {
                let entry = firsts[merk] + at as u32;
                let stored = again.binary_search_by_key(&entry, |(place, _)| *place);
                let (value, source) = match (stored, node.value) {
                    (Ok(stored), _) => {
                        let stored = again[stored].1;
                        let value =
                            read_placed(path.values, stored.pointer, stored.owner, &mut buf)?;
                        (value, Source::Stored(stored))
                    }
                    (Err(_), ValuePut::Value(value)) => (value, Source::Batch(value)),
                    (Err(_), ValuePut::Keep) => {
                        return Err(not_whole(subtree, WholeFault::KeepsValue));
                    }
                };
                let mut links = [None, None];
                for (load, link) in links.iter_mut().zip([&node.left, &node.right]) {
                    let Some(link) = link else {
                        continue;
                    };
                    let child = keys.get(link.key).and_then(|child| subtrees[*child].take());
                    *load = Some(LinkLoad {
                        child: child.ok_or_else(|| not_whole(subtree, WholeFault::ChildLater))?,
                        heights: link.heights,
                        aggregate: link.aggregate,
                    });
                }
                let [left, right] = links;
                hosted_roots.clear();
                if let Hosted::Set(hosted) = node.hosted {
                    hosted_roots.extend(hosted.iter().map(|subtree| {
                        let root = match resolver.load(subtree) {
                            Some(merk) => roots[merk],
                            None => outside.get(subtree).copied().flatten(),
                        };
                        root.map_or(HostedLoad::Empty, HostedLoad::At)
                    }));
                }
                let mut rules = Stated {
                    kv: node.kv_hash,
                    own: hashes[at].copied().unwrap_or([0; 32]),
                };
                placing.node(entry, source);
                let node = NodeLoad {
                    key: node.key,
                    feature: node.feature,
                    value,
                    value_hash: Some(node.value_hash),
                    left,
                    right,
                    hosted: &hosted_roots,
                };
                let taken = loader.node(&mut pages, &mut placing, &mut rules, &node);
                let taken =
                    taken.map_err(|error| load_error(error, placing.failed.take(), subtree));
                subtrees[at] = Some(taken?);
            }
            let top = match write.root {
                RootPut::Set(Some(root)) => keys.get(root).and_then(|at| subtrees[*at].take()),
                _ => None,
            };
            let top = top.ok_or_else(|| not_whole(subtree, WholeFault::NoRoot))?;
            let root = loader.root(&mut pages, &mut placing, top);
            let root = root.map_err(|error| load_error(error, placing.failed.take(), subtree))?;
            roots[merk] = Some(root.address);
            // The root is placed, so every cell of the Merk is in a page.
            let in_no_page = TxnError::Damaged("a node of a load is in no page");
            handles[merk] = pages.take_handles().ok_or(in_no_page)?;
        }
        let first = merks[order[0]].subtree;
        // The pages the loader has left by now are the ones it filled: each
        // level fills one more, which holds what the load ends with.
        let mut least_filled = None;
        for page in loader.drain_pages() {
            self.fragments.hints_mut().note(page.page, page.used);
            least_filled =
                Some(least_filled.map_or(page.used, |least: usize| least.min(page.used)));
        }
        let report = (loader.finish()).map_err(|error| load_error(error, None, first))?;
        for page in &report.pages {
            self.fragments.hints_mut().note(page.page, page.used);
        }
        let mut stored = placing.stored;
        stored.sort_unstable_by_key(|(entry, _)| *entry);
        self.stored = stored;
        Ok(Load {
            roots,
            handles,
            least_filled,
            counters: report.counters,
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
            if is_load(write) {
                // The Merk of a load had no node, and every node of its
                // change set is below the root it states
                // ([`check`](super::check::check)).
                ats.resize(offset + nodes.len(), None);
                linked.resize(offset + nodes.len(), true);
                continue;
            }
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
            if resolver.loaded(merk).is_some() {
                // The nodes of a load are in their pages, and those that
                // host a Merk of the batch hold the pointer to its root:
                // it is no pointer that the directory alone holds.
                let sets = write.nodes.iter().filter_map(|node| match node.hosted {
                    Hosted::Set(hosted) => Some(hosted),
                    Hosted::Keep => None,
                });
                for hosted in sets.flatten() {
                    if let Some(&hosted) = resolver.by_subtree.get(hosted) {
                        hosted_here[hosted] = true;
                    }
                }
                continue;
            }
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
