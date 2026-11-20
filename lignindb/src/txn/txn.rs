//! The write transaction and its scope.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use lignin_format::{tree_page::Side, CellAddress, CommitVersion};
use lignin_store::{
    load::LoadBuffers,
    stats::{OpenWrite, Stats},
};
use lignin_tree::{
    directory::{Directory, DirectoryError, MerkId},
    fragment::FragmentError,
    node::{Lease, NodeError, NodeHandle},
    value::{read_value, ValueBuf},
};

use super::{
    check::check,
    commits::Watch,
    error::{BatchError, TxnError},
    frames::{stored_key, Awake, Frames, Parked, Woken},
    key::{Column, KeySet, Subtree, Unit},
    load::{cover, is_load, Loaded},
    opened::Opened,
    pages::{child_keys, find, follow, CommittedPages, NodePages},
    path::WritePath,
    reads::{Reads, Since},
    unlinked,
    view::{ChildKeys, NodeView, SubtreeView},
    write::{Applied, Batch, Hosted, Moving, SubtreeWrite, ValuePut},
    write_set::{EntryId, Mark, NodeValue, Own, OwnSubtree, Restated, Said, WriteSet},
};
use crate::{
    scope::table::{Scopes, Who},
    snapshot::{OperationGuard, VersionPin},
};

/// What a write transaction holds at one moment: for whoever bounds its
/// memory, and for the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TxnStats {
    /// The writes in the write set (`transactions.md` ST5).
    pub writes: usize,
    /// The bytes the write set's arenas hold.
    pub write_set_bytes: usize,
    /// How many of the writes a commit outside the transaction has met: the
    /// overlap set (WT7). The transaction's commit fails while it is not
    /// zero (CF2).
    pub overlapped: usize,
    /// The version the private frames are built over, if the transaction
    /// has frames.
    pub base: Option<CommitVersion>,
    /// The version the transaction keeps pinned, if it pins one (PN1).
    pub pinned: Option<CommitVersion>,
    /// The private copies of tree pages the transaction holds in the
    /// buffer pool. They cannot be evicted (WT4).
    pub tree_pages: usize,
    /// The private copies of flat pages it holds.
    pub flat_pages: usize,
    /// The tree pages whose cells hold values out of line that the
    /// transaction's commit writes: its own new values, and those of cells
    /// it carried to another page.
    pub value_pages: usize,
    /// The nodes of its frames that have a stale hash, for its commit to
    /// evaluate. None on existing GroveVersions.
    pub stale_nodes: usize,
    /// How many times the frames were made again from the write set: after
    /// a commit outside the transaction (WT7), a rollback to a mark, or an
    /// operation that failed half way.
    pub rebuilds: u64,
    /// How many reads of a node went through a handle that was still good
    /// (PN8): no keyed lookup was made for them.
    pub handles_followed: u64,
    /// How many reads of a node found it by its key: in the write set, or
    /// down from the root of its Merk.
    pub keyed_reads: u64,
}

/// What a transaction keeps between its operations.
struct State<'s> {
    set: WriteSet,
    /// The version the write set's kept values are read at (WT3): pinned
    /// from the transaction's first write until the frames are made over
    /// another version, or until the transaction has its own copy of every
    /// committed byte its write set refers to.
    base: Option<VersionPin<'s>>,
    /// The private frames, as they are kept between two operations.
    frames: Option<Parked<'s>>,
    /// The values that the transaction's loads put into the value log:
    /// the hold on their files, from the first of them until the commit
    /// has counted those its cells hold, or until the write set is empty
    /// again (`super::load`).
    loaded: Loaded,
    /// The watch on the key sets of the commits: from the transaction's
    /// first write or the first node it is shown, whichever comes first,
    /// until it has no write and remembers no lease.
    watch: Option<Watch<'s>>,
    /// The versions of the leases the transaction has shown nodes under,
    /// and the key sets of the commits it took in since: what a write that
    /// keeps a value is checked against (`super::reads`).
    reads: Reads,
    /// The lease that committed nodes are shown under where they are asked
    /// for as committed ([`Self::committed_node_in`]), and the version it
    /// is for.
    ///
    /// [`Self::committed_node_in`]: WriteTxn::committed_node_in
    committed: Option<(Lease, CommitVersion)>,
    /// The transaction among the open write transactions of the store's
    /// counts, from its first write until its write set is empty again:
    /// while it can hold private copies, and with them the log
    /// (`lignin_store::stats`, "What an operator watches").
    open: Option<OpenWrite<'s>>,
    /// The lease committed pages are read under where the transaction has
    /// no frames, and the version it is for (PN8).
    lease: Lease,
    lease_at: Option<CommitVersion>,
    /// The subtree directory of the committed version that was read last,
    /// with the subtrees that were opened through it: for the committed
    /// records the transaction reads beside its own.
    directory: Option<Opened>,
    /// The version and the number of operations of the write set at which
    /// the frames could not be made: the write set names what the
    /// committed state does not hold.
    stuck: Option<(CommitVersion, usize)>,
    /// The scopes that are open, what each has claimed, and the pending
    /// marks (`crate::scope`).
    scopes: Scopes,
    /// For each subtree the transaction has written under: the Merks it
    /// is below in the committed version the transaction last looked at,
    /// or none where that version has not entered it (CF5,
    /// `WriteTxn::meet_below`).
    under: HashMap<Subtree, Option<Vec<MerkId>>>,
    /// What each enter if it is absent that the transaction made stated
    /// of where its subtree hangs (CF5, `WriteTxn::meet_below`).
    above: Vec<Above>,
    /// The commits whose key sets were taken and that were not looked at
    /// for what their drops cover (`WriteTxn::meet_below`): the look that
    /// was owed them failed on a read, and the next one makes it.
    owed: Vec<(CommitVersion, Arc<KeySet>)>,
    /// The next look at what the drops of commits cover fails once it has
    /// read where the subtrees hang, as a read of the directory does: for
    /// a test of a read that fails there.
    #[cfg(test)]
    refuse_look: bool,
    /// The transaction is rolled back where the next read of a record or
    /// a node through a cursor comes to take its lock: for a test of a
    /// rollback by another thread at that moment.
    #[cfg(test)]
    rolls_back: bool,
    /// The version up to which the transaction last took the commits in
    /// ([`WriteTxn::take_in`]), for a test's model of what it saw there.
    #[cfg(all(test, not(loom)))]
    looked: Option<CommitVersion>,
    /// The savepoints that are set, oldest first, each the place of the
    /// write set at which it was set (WT8, `crate::savepoint`).
    savepoints: Vec<Mark>,
    buffers: LoadBuffers,
    key: Vec<u8>,
    rebuilds: u64,
    followed: u64,
    keyed: u64,
}

impl State<'_> {
    /// Gives up everything a transaction holds beside its write set.
    fn release(&mut self) {
        self.frames = None;
        self.base = None;
        self.stuck = None;
        if self.set.is_empty() {
            self.unwatch();
            self.open = None;
            self.under.clear();
            self.above.clear();
            self.owed.clear();
            // No write is left that could name a value of a load: what the
            // loads put into the value log is dead space from here on.
            self.loaded = Loaded::default();
        }
    }

    /// Gives the watch on the commits up where nothing needs it: the
    /// transaction has no write, remembers no lease, and no read of a
    /// node is under way that a cursor makes of its own version
    /// ([`Reads::any`]). A lease is remembered from when a cursor over
    /// the nodes of a Merk is opened with it ([`WriteTxn::pin_shown`]),
    /// whether or not a node was shown under it since, until the
    /// transaction ends or the key sets it holds reach back to the
    /// lease's version no more ([`Reads::took`]). The handle of a node
    /// that was shown can still come back in a write that keeps the
    /// node's value, and the commits are watched for that: with no
    /// write, for the newest key sets only, which are all that its reads
    /// are dated with ([`Commits`](super::Commits)).
    fn unwatch(&mut self) {
        if !self.set.is_empty() {
            return;
        }
        match &mut self.watch {
            Some(watch) if self.reads.any() => watch.hold_newest(),
            _ => self.watch = None,
        }
    }

    /// Ends every node handle the transaction issued (PN8).
    fn renew_lease(&mut self) {
        self.lease = Lease::take();
        self.lease_at = None;
    }

    /// Ends the transaction's writes: the write set, the savepoints, and
    /// everything that is held for them (WT9).
    ///
    /// The handles of the nodes it showed end with it, and it forgets when
    /// it showed them: a write that keeps a value by a handle of a lease
    /// it has shown no node under since is taken to be met
    /// (`super::reads`). That holds of the lease of a cursor too, also
    /// while a read of a node that the cursor makes of its own version
    /// is under way: such a read dates the lease when it finds a node,
    /// and again when it ends, if it found one ([`Reads::ended`]). The
    /// key sets that the transaction holds are not given up at its end
    /// while such a read is under way ([`Reads::clear`]).
    fn end(&mut self) {
        self.set.clear();
        self.savepoints.clear();
        self.scopes.clear_marks();
        self.reads.clear();
        self.committed = None;
        self.release();
        self.renew_lease();
    }

    /// The place after the transaction's last write, and after the last
    /// change of its pending marks.
    fn mark(&self) -> Mark {
        let mut mark = self.set.mark();
        mark.pending = self.scopes.marks_len();
        mark
    }

    /// Forgets what the enters of operations noted that the write set
    /// has no more.
    fn forget_above(&mut self) {
        (self.above).retain(|above| self.set.has_op(above.serial));
    }

    /// Takes out the writes after `mark`, and says whether `mark` is a
    /// place of the write set still.
    fn rewind(&mut self, mark: Mark) -> bool {
        if !self.set.holds(mark) {
            return false;
        }
        // The pending marks that were set or taken off after the mark go
        // with the writes after it (WT8). The frames know the cells of the
        // marked elements, so they are made again with them.
        let marks = self.scopes.rewind_marks(mark.pending);
        if self.set.rewind(mark) || marks {
            // The frames are made again from what is left, over the latest
            // version of that moment (WT8). The base stays pinned until
            // then: what is left may keep committed values.
            self.frames = None;
            self.stuck = None;
            self.renew_lease();
            self.forget_above();
            if self.set.is_empty() {
                self.release();
            }
        }
        true
    }
}

/// What a read of the transaction's own node is given beside the key.
enum Beside<'c> {
    /// A handle of the node, which is followed while it is good (PN8).
    Hint(Option<NodeHandle>),
    /// Where the keys of the children of a cell go, for a cursor that
    /// reads them with the node.
    Children(Option<&'c mut ChildKeys>),
}

/// What a batch keeps of the committed state, and what is known of the
/// reads it kept it from (`transactions.md` CF2a; `crate::txn`, "A kept
/// value and the read it is of").
///
/// The places are those of the batch's nodes among the entries the write
/// set makes of it: the nodes and then the deletes of each Merk, in order.
struct Kept<'b> {
    /// The nodes whose kept value a commit outside the transaction wrote
    /// since its writer read it, or may have: their entries are met.
    met: Vec<(EntryId, Unit, &'b [u8])>,
    /// The first node that keeps a value and names no read, where the
    /// transaction has no write of the key: with a node there, the batch
    /// is refused for it, and with none for a value that is not there.
    unread: Option<(EntryId, Subtree, &'b [u8])>,
}

impl<'b> Kept<'b> {
    /// Looks at every node of `batch` that keeps its value, for a write at
    /// the version `at` that has taken in every commit up to it. `current`
    /// is the lease a handle of the operation's own view is of.
    ///
    /// A keep whose handle is of `current` costs one comparison: no commit
    /// was taken in since its node was read. Only the others are looked up.
    fn of(
        set: &WriteSet,
        reads: &Reads,
        at: CommitVersion,
        current: Lease,
        batch: &Batch<'b>,
    ) -> Self {
        let mut kept = Self {
            met: Vec::new(),
            unread: None,
        };
        let mut entry: EntryId = 0;
        for merk in batch.merks {
            let unit = Unit::data(merk.subtree);
            for node in merk.nodes {
                entry += 1;
                let lease = node.at.map(|handle| handle.lease());
                if node.value != ValuePut::Keep || lease == Some(current) {
                    continue;
                }
                let since = lease.map(|lease| reads.since(lease, at, unit, node.key));
                if since == Some(Since::Unwritten) || batch.leaves(merk.subtree) {
                    // What the batch itself drops or clears has no value:
                    // it is refused for that where it is applied.
                    continue;
                }
                match (set.own(unit, node.key), since) {
                    // The committed value, and no read of it is named.
                    (Own::Unwritten, None) => {
                        _ = kept
                            .unread
                            .get_or_insert((entry - 1, merk.subtree, node.key))
                    }
                    // The committed value, which a commit wrote since the
                    // read that is named, or may have.
                    (Own::Unwritten, Some(_)) => kept.met.push((entry - 1, unit, node.key)),
                    // The transaction wrote the node and stated no value
                    // for it: it has the committed value still, and the
                    // read that is named is as far back as any.
                    (Own::Node(id), Some(_)) if !set.states_value(id) => {
                        kept.met.push((entry - 1, unit, node.key))
                    }
                    // A value the transaction stated is its own to keep.
                    // A node it wrote before is shown to it as its record,
                    // which has no handle: a keep that names none is of
                    // that record, which is there only since the write
                    // that the conflict rule looks from. What it deleted
                    // has no value, and is refused where it is applied.
                    _ => {}
                }
            }
            entry += merk.removed.len() as EntryId;
        }
        kept
    }
}

/// How many subtrees one inside the other the hosts of a transaction's own
/// subtrees are followed through: the depth the directory takes.
const MAX_DEPTH: usize = 1 << 16;

/// One operation of a transaction: its guard, and the one version it reads
/// committed records at (RD4).
struct Op<'s> {
    guard: OperationGuard<'s>,
    at: CommitVersion,
}

/// What a cursor brings to a call into its transaction.
pub(crate) struct Pinned<'a, 's> {
    /// The subtree directory of the version the cursor reads, which the
    /// cursor keeps from call to call.
    pub(crate) opened: &'a mut Option<Opened>,
    /// The guard of the cursor's operation.
    pub(crate) guard: &'a OperationGuard<'s>,
    /// The version the cursor reads.
    pub(crate) at: CommitVersion,
    /// The answer the cursor has of its committed side: the count of
    /// [`WriteSet::left`] it is of, and whether the side is read. It is
    /// asked again once the count is another.
    pub(crate) seen: Option<(u64, bool)>,
    /// The lease under which a cursor over the nodes of a Merk reads the
    /// nodes of its own version.
    pub(crate) lease: Option<Lease>,
}

/// What a cursor has as its committed side
/// ([`WriteTxn::committed_side`]).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sided {
    /// The count of [`WriteSet::left`] that the answer is of.
    pub(crate) left: u64,
    /// Whether the side is read.
    pub(crate) reads: bool,
    /// The prefix it is read under, where that was asked again.
    pub(crate) under: Option<Subtree>,
}

/// Whether `error` says that a batch names what is not there: the tree
/// pages or the subtree directory refused it, and no page is damaged.
fn is_refusal(error: &TxnError) -> bool {
    match error {
        TxnError::Batch(_)
        | TxnError::Fragment(FragmentError::Write(_) | FragmentError::NoRoom { .. }) => true,
        TxnError::Directory(error) => !matches!(
            error,
            DirectoryError::Flat(_) | DirectoryError::Damaged { .. }
        ),
        _ => false,
    }
}

/// Where an enter if it is absent put its subtree, as the transaction
/// had the subtrees when it made the enter.
#[derive(Debug)]
struct Above {
    /// The serial of the operation that made the enter.
    serial: u64,
    subtree: Subtree,
    /// The subtrees that the host the enter states was in or below, by
    /// their names, the nearest first.
    names: Vec<Subtree>,
}

/// How a transaction with no frames has a subtree once the operations of
/// its write set before one are made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// It has none: it dropped the subtree, alone or with one above it
    /// that it had written under, and has not entered it since.
    Gone,
    /// The one the committed version has, if that has one: no write of
    /// the transaction made the subtree.
    Committed,
    /// Its own, which its operation of this number entered.
    Own(usize),
}

/// An answer of [`ByKey`], or the standing it rests on that is not worked
/// out yet: of a subtree, once the operations before one are made.
enum Step<T> {
    Done(T),
    Needs((Subtree, usize)),
}

/// What a write set says of the subtrees over one committed version, for
/// a transaction that has no frames to ask, and for a scope whose writes
/// are kept in the write set beside them.
///
/// **A subtree is the committed one, the transaction's own, or gone**
/// ([`Standing`]). An enter of a subtree that is new makes it the
/// transaction's own. An enter if it is absent
/// ([`SubtreeWrite::EnterIfAbsent`]) does so where the subtree was not
/// there before it: the transaction had dropped it, a subtree above it had
/// left since it was entered, or the committed version does not have it.
/// The drops and the clears of the operation's own batch come before its
/// enter. Where the subtree was there, it stays the one it was, whatever
/// the enter states of it: by key no write to a subtree is judged
/// ([`SubtreeWrite`]).
///
/// **Where a subtree hangs follows from which one it is.** The
/// transaction's own hangs on the host that the enter which made it
/// states, and the committed one where the committed directory has it.
///
/// **One answer can rest on another**: what a subtree is on what its host
/// was when the subtree was entered, up a line of hosts. An answer that
/// another rests on is worked out first and kept, one after another and
/// not one inside another, so a line as long as the directory takes
/// (`MAX_DEPTH`) is followed.
struct ByKey<'a, 's> {
    path: &'a WritePath<'s>,
    guard: &'a OperationGuard<'s>,
    set: &'a WriteSet,
    opened: &'a mut Opened,
    /// What was worked out already, by subtree and operation.
    stands: HashMap<(Subtree, usize), Standing>,
}

impl<'a, 's> ByKey<'a, 's> {
    /// The answers of `set` over the committed version `at`, whose
    /// directory is kept in `slot`.
    fn new(
        path: &'a WritePath<'s>,
        guard: &'a OperationGuard<'s>,
        at: CommitVersion,
        set: &'a WriteSet,
        slot: &'a mut Option<Opened>,
    ) -> Result<Self, TxnError> {
        Ok(Self {
            opened: Opened::at(slot, path, guard, at)?,
            path,
            guard,
            set,
            stands: HashMap::new(),
        })
    }

    /// How the transaction has `subtree` once its operations before `end`
    /// are made. A subtree that it entered and wrote nothing into is its
    /// own here also where a subtree above it left since: no drop names
    /// it, and [`Self::below`] says whether one took it.
    fn standing(&mut self, subtree: Subtree, end: usize) -> Result<Standing, TxnError> {
        // What is asked, and under it what each answer waits for. An
        // answer waits only for answers of fewer operations, so the line
        // ends.
        let mut wanted = vec![(subtree, end)];
        while let Some(&(subtree, end)) = wanted.last() {
            if self.stands.contains_key(&(subtree, end)) {
                wanted.pop();
                continue;
            }
            match self.try_standing(subtree, end)? {
                Step::Done(stands) => {
                    self.stands.insert((subtree, end), stands);
                    wanted.pop();
                }
                Step::Needs(first) => wanted.push(first),
            }
        }
        Ok(self.stands[&(subtree, end)])
    }

    /// [`Self::standing`], from the answers that are worked out.
    fn try_standing(&mut self, subtree: Subtree, end: usize) -> Result<Step<Standing>, TxnError> {
        let set = self.set;
        let Said { base, if_absent } = set.subtree(&subtree, end);
        let mut stands = match base {
            OwnSubtree::Gone => Standing::Gone,
            OwnSubtree::Entered { op, .. } => Standing::Own(op),
            OwnSubtree::Unwritten { .. } => Standing::Committed,
        };
        for (_, at) in if_absent.iter().rev() {
            // Whether the subtree was there when the operation `at` came
            // to its enters: the leaves of that operation come first.
            let there = match stands {
                Standing::Own(since) => {
                    let left = set.left_in(since + 1..at + 1);
                    left.is_empty()
                        || match self.try_below(subtree, &left, Some((since, at + 1)))? {
                            Step::Done(gone) => !gone,
                            Step::Needs(first) => return Ok(Step::Needs(first)),
                        }
                }
                Standing::Committed => {
                    let left = set.taking_in(0..at + 1);
                    let entered = (self.opened.root(self.path, self.guard, &subtree)?).is_some();
                    entered && !(self.opened).below_taken(self.path, self.guard, &subtree, &left)?
                }
                Standing::Gone => false,
            };
            if !there {
                stands = Standing::Own(*at);
            }
        }
        Ok(Step::Done(stands))
    }

    /// Whether `subtree` is below one of `leaving`: by the hosts of the
    /// subtrees that are the transaction's own, and by the committed
    /// directory from the first that is the committed one.
    ///
    /// With `since`, `subtree` is the transaction's own, entered in the
    /// first of the two operations, and `leaving` left in the operations
    /// after it and before the second. A host that was entered again in
    /// them, as a subtree that is new, left in between, with what it
    /// hosted, whatever it is below now: a subtree is entered so only
    /// after it left. Each host is taken as it stood when the subtree it
    /// hosts was entered: one that was entered again later, if it was
    /// absent, left with a subtree above it, which is found from where it
    /// hung then. With no `since`, `subtree` is taken as it stands after
    /// every operation.
    ///
    /// That holds for the write sets the frames take. One answer is not
    /// the frames': a clear of a Merk with no node leaves the directory as
    /// it is, and here it takes what the transaction entered below that
    /// Merk. An element hosts a subtree, so no writer of a grove enters
    /// one below a Merk with no node.
    fn below(
        &mut self,
        subtree: Subtree,
        leaving: &[Subtree],
        since: Option<(usize, usize)>,
    ) -> Result<bool, TxnError> {
        loop {
            match self.try_below(subtree, leaving, since)? {
                Step::Done(below) => return Ok(below),
                Step::Needs((host, end)) => drop(self.standing(host, end)?),
            }
        }
    }

    /// [`Self::below`], from the answers that are worked out.
    fn try_below(
        &mut self,
        subtree: Subtree,
        leaving: &[Subtree],
        since: Option<(usize, usize)>,
    ) -> Result<Step<bool>, TxnError> {
        let set = self.set;
        let again = |host: &Subtree| {
            since.is_some_and(|(entered, end)| set.enters_anew(host, entered + 1..end))
        };
        let (mut at, mut end) = (subtree, set.ops());
        let mut stands = match since {
            Some((entered, _)) => Some(Standing::Own(entered)),
            None => self.stands.get(&(subtree, end)).copied(),
        };
        for _ in 0..MAX_DEPTH {
            let entered = match stands {
                None => return Ok(Step::Needs((at, end))),
                Some(Standing::Gone) => return Ok(Step::Done(false)),
                Some(Standing::Committed) => {
                    let below = (self.opened).below(self.path, self.guard, &at, leaving)?;
                    return Ok(Step::Done(below));
                }
                Some(Standing::Own(entered)) => entered,
            };
            let Some(host) = set.host_in(&at, entered) else {
                return Ok(Step::Done(false));
            };
            if leaving.contains(&host) || again(&host) {
                return Ok(Step::Done(true));
            }
            (at, end) = (host, entered + 1);
            stands = self.stands.get(&(at, end)).copied();
        }
        Ok(Step::Done(false))
    }
}

/// A write transaction (`transactions.md` section 7).
///
/// - **It reads its own writes, and for everything else the latest
///   committed state at the moment of each read** (RD3). It has no
///   snapshot: a commit made outside it while it is open is seen by its
///   next read of every key it has not written.
/// - **Its write set is the truth** (WT3). Its private frames, in the
///   buffer pool, are derived from it and are made again whenever a commit
///   lands outside it (WT7), and after a rollback to a mark.
/// - **Nobody else sees its writes** (RD8): they are in private copies and
///   in its own memory until a commit installs them.
/// - **It holds no page while it has written nothing** (PN1), and dropping
///   it is a rollback (WT9). Once it has been shown a node it watches the
///   commits, for a write that keeps the node's value (`crate::txn`, "A
///   kept value and the read it is of").
///
/// It is shared between threads (WT11). Its writes go through a [`Scope`];
/// reads may also be made outside one. Each call is one operation (RD4)
/// and holds the transaction's own lock for its length.
pub struct WriteTxn<'s> {
    path: WritePath<'s>,
    state: Mutex<State<'s>>,
}

const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    fn moved<T: Send>() {}
    shared::<WriteTxn<'static>>();
    moved::<Scope<'static, 'static>>();
};

impl<'s> WriteTxn<'s> {
    pub(crate) fn new(path: WritePath<'s>) -> Self {
        Self {
            path,
            state: Mutex::new(State {
                set: WriteSet::new(),
                base: None,
                frames: None,
                loaded: Loaded::default(),
                watch: None,
                reads: Reads::default(),
                committed: None,
                open: None,
                lease: Lease::take(),
                lease_at: None,
                directory: None,
                stuck: None,
                scopes: Scopes::default(),
                under: HashMap::new(),
                above: Vec::new(),
                owed: Vec::new(),
                #[cfg(test)]
                refuse_look: false,
                #[cfg(test)]
                rolls_back: false,
                #[cfg(all(test, not(loom)))]
                looked: None,
                savepoints: Vec::new(),
                buffers: LoadBuffers::new(),
                key: Vec::new(),
                rebuilds: 0,
                followed: 0,
                keyed: 0,
            }),
        }
    }

    /// The write path the transaction was begun from.
    pub(crate) fn path(&self) -> &WritePath<'s> {
        &self.path
    }

    /// The counts of the store the transaction is of, for the layer above
    /// to tell what the engine cannot see: the hashes it computes itself
    /// ([`Stats::count_hashes`]).
    pub fn counts(&self) -> &'s Stats {
        self.path.stats
    }

    fn lock(&self) -> MutexGuard<'_, State<'s>> {
        self.state.lock().unwrap_or_else(|poisoned| {
            // An operation panicked, and its frames may hold a part of a
            // batch. The write set does not (WT10): the frames are made
            // again from it.
            let mut state = PoisonError::into_inner(poisoned);
            state.frames = None;
            state.stuck = None;
            state.renew_lease();
            if state.set.is_empty() {
                // The operation that panicked may have been a first write,
                // which pins a version and starts to watch the commits
                // before it adds anything. Nothing was added, so nothing
                // is held (PN1).
                state.release();
            }
            self.state.clear_poison();
            state
        })
    }

    /// The state, for a call that needs every scope closed: a rollback of
    /// the transaction and its commit (SC8, CM1).
    fn closed(&self) -> Result<MutexGuard<'_, State<'s>>, TxnError> {
        let state = self.lock();
        if state.scopes.any_open() {
            return Err(TxnError::ScopeOpen);
        }
        Ok(state)
    }

    /// What the transaction holds now.
    pub fn stats(&self) -> TxnStats {
        let state = self.lock();
        let frames = state.frames.as_ref();
        TxnStats {
            writes: state.set.len(),
            write_set_bytes: state.set.bytes(),
            overlapped: state.set.met(),
            base: frames.map(|frames| frames.base()),
            pinned: state.base.as_ref().map(VersionPin::version),
            tree_pages: frames.map_or(0, |frames| frames.tree.copies()),
            flat_pages: frames.map_or(0, |frames| frames.flat.copies()),
            value_pages: frames.map_or(0, |frames| frames.values.pages().count()),
            stale_nodes: frames.map_or(0, |frames| frames.hashes.len()),
            rebuilds: state.rebuilds,
            handles_followed: state.followed,
            keyed_reads: state.keyed,
        }
    }

    /// Rolls the transaction back (WT9): its write set, its private frames
    /// and its version pin go, the page ids it allocated return to the
    /// allocator, and every node handle read through it ends. It stays
    /// usable, and writes made through it afterwards start fresh.
    ///
    /// It is refused while a scope is open.
    pub fn rollback(&self) -> Result<(), TxnError> {
        self.closed()?.end();
        Ok(())
    }

    /// Holds the transaction for one call on its marks and its savepoints
    /// (`crate::savepoint`).
    pub(crate) fn hold(&self) -> Held<'_, 's> {
        Held(self.lock())
    }

    /// The place after the transaction's last write: what a savepoint
    /// remembers (WT8).
    #[cfg_attr(
        any(not(test), loom),
        expect(dead_code, reason = "a savepoint takes its mark through `Held`")
    )]
    pub(crate) fn mark(&self) -> Mark {
        self.lock().mark()
    }

    /// Takes out every write that was made after `mark`, with its
    /// first-write version and its place in the overlap set (WT8, CF4). The
    /// frames are made again from what is left, and every node handle read
    /// through the transaction ends (PN8).
    ///
    /// A mark is a place of the write set until a write from before it is
    /// taken out: by a rollback, by the transaction's commit, or by going
    /// back to an earlier mark. For a mark that is one no more, nothing is
    /// changed and `false` is returned.
    #[cfg_attr(
        any(not(test), loom),
        expect(dead_code, reason = "a savepoint goes back to its mark through `Held`")
    )]
    pub(crate) fn rewind(&self, mark: Mark) -> bool {
        let mut state = self.lock();
        // A scope's writes are taken out with the scope, and with nothing
        // else, while it is open (SC8).
        !state.scopes.any_open() && state.rewind(mark)
    }

    /// Takes in the key sets of the commits that were published up to `at`
    /// since the transaction last looked: the entries they meet go into
    /// the overlap set (WT7, CF3).
    ///
    /// What a drop covers without naming it is met as well: the subtrees
    /// below a dropped one (CF5, [`Self::meet_below`]).
    fn take_in(
        &self,
        state: &mut State<'s>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
    ) -> Result<(), TxnError> {
        if let Some(watch) = &mut state.watch {
            let taken = state.owed.len();
            watch.take(at, &mut state.owed);
            for (version, keys) in &state.owed[taken..] {
                state.set.meet(*version, keys);
                state.reads.took(*version, keys);
            }
        }
        // The key sets are taken once. A look that fails on a read leaves
        // its commits owed, so that no drop goes unmet.
        let owed: Vec<_> = std::mem::take(&mut state.owed);
        let looked = self.meet_below(state, guard, at, &owed);
        if looked.is_err() {
            state.owed = owed;
        }
        #[cfg(all(test, not(loom)))]
        if looked.is_ok() {
            state.looked = Some(at);
        }
        looked
    }

    /// The version up to which the transaction last took the commits in
    /// ([`Self::take_in`]), if it has done so.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn looked_at(&self) -> Option<CommitVersion> {
        self.lock().looked
    }

    /// Whether the transaction watches the key sets of the commits.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn watches(&self) -> bool {
        self.lock().watch.is_some()
    }

    /// Has the transaction's next look at what the drops of commits cover
    /// fail once it has read where the subtrees hang, as a page read
    /// after that does.
    #[cfg(test)]
    pub(crate) fn refuse_look(&self) {
        self.lock().refuse_look = true;
    }

    /// Brings the transaction to the commits that were published up to
    /// `at` (WT7): the entries they meet go into the overlap set, and
    /// frames that are built over another version go.
    fn observe(
        &self,
        state: &mut State<'s>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
    ) -> Result<(), TxnError> {
        self.take_in(state, guard, at)?;
        if state.set.met() > 0 {
            // Before it gives up the version it read them at, the
            // transaction takes its own copy of every committed byte its
            // write set refers to (WT3, WT7). From here on it serves its
            // records by key, and pins nothing.
            if let Some(base) = state.base.as_ref().map(VersionPin::version) {
                self.keep_values(state, guard, base)?;
                state.base = None;
            }
            state.frames = None;
        } else if (state.frames.as_ref()).is_some_and(|frames| frames.base() != at) {
            // The frames are made again over `at`. The base stays pinned
            // until they are.
            state.frames = None;
        }
        Ok(())
    }

    /// Gives every written node that keeps a committed value the
    /// transaction's own copy of it, read at `version` under `guard`.
    fn keep_values(
        &self,
        state: &mut State<'s>,
        guard: &OperationGuard<'s>,
        version: CommitVersion,
    ) -> Result<(), TxnError> {
        let keeping = state.set.keeping();
        if keeping.is_empty() {
            return Ok(());
        }
        let path = &self.path;
        let lease = Lease::take();
        for id in keeping {
            // Where the transaction moved the node's subtree before the
            // write, the committed state has the node under the prefix
            // the subtree was taken from.
            let gone = TxnError::Damaged("a node whose value is kept is gone");
            let subtree = state.set.committed_under_at(id).ok_or(gone)?;
            let root = self.committed_root(state, guard, version, &subtree)?;
            let mut pages = path.committed_pages(guard, version, lease, &mut state.buffers);
            // No commit wrote the key since the transaction did, or the
            // entry would be met: the node it kept the value of is there.
            let address = find(&mut pages, root, state.set.key(id))?
                .ok_or(TxnError::Damaged("a node whose value is kept is gone"))?;
            let value = Self::read_value(&mut pages, address)?;
            state.set.keep(id, &value);
        }
        Ok(())
    }

    /// Begins an operation (RD4, WT7): takes its guard, reads the latest
    /// version, and brings the transaction to it. Afterwards one of these
    /// holds:
    ///
    /// - the transaction has frames over the version the operation reads
    ///   at, which show its write set applied to that version (WT7a)
    /// - it has none, and serves its records by key: its write set is
    ///   empty, or a commit outside it has met one of its writes (WT7b), or
    ///   its write set names what the committed state does not hold
    fn begin(&self, state: &mut State<'s>, writing: bool) -> Result<Op<'s>, TxnError> {
        let path = &self.path;
        // The guard is taken before the latest version is read (PN5), and
        // the watch before the version a first write is dated with (WT5).
        let guard = path.versions.guard();
        if writing {
            // The write is dated with the version that is read below, and
            // every commit after it is compared with it (CF3).
            self.watch(state).hold_all();
        }
        if writing && state.open.is_none() {
            state.open = Some(path.stats.write_opened());
        }
        let mut at = guard.latest();
        loop {
            self.observe(state, &guard, at)?;
            let framed = state.frames.is_some();
            let by_key = state.set.met() > 0 || state.stuck == Some((at, state.set.ops()));
            if framed || by_key || (state.set.is_empty() && !writing) {
                break;
            }
            // The frames are made over a version the transaction pins. If a
            // commit was published since the latest version was read, that
            // one is taken in first.
            let pin = path.versions.pin();
            if pin.version() != at {
                at = pin.version();
                continue;
            }
            let built = Self::build(
                path,
                &guard,
                at,
                &state.set,
                &state.scopes,
                &mut state.loaded,
            );
            match built {
                Ok(frames) => {
                    if !state.set.is_empty() {
                        state.rebuilds += 1;
                    }
                    state.frames = Some(frames);
                    state.base = Some(pin);
                    // Whatever stopped a layout over an earlier version
                    // stops this one no more.
                    state.stuck = None;
                }
                Err(error) if is_refusal(&error) => {
                    // The values the write set keeps are read at the
                    // version it kept them at, while that is pinned: at
                    // `at` a commit that met no key of the transaction may
                    // have dropped a subtree above one it wrote.
                    let kept = state.base.as_ref().map_or(at, VersionPin::version);
                    self.keep_values(state, &guard, kept)?;
                    state.base = None;
                    state.stuck = Some((at, state.set.ops()));
                }
                Err(error) => return Err(error),
            }
            break;
        }
        if state.lease_at != Some(at) {
            state.lease = Lease::take();
            state.lease_at = Some(at);
        }
        Ok(Op { guard, at })
    }

    /// Begins an operation that shows a node. The node's handle may come
    /// back in a write that keeps the node's value, and the transaction
    /// then has to say what the commits did to the node's key since
    /// (CF2a). So it watches them from here on, as it does from its first
    /// write: the watch stands before the operation reads the version the
    /// node is shown at, and no commit after that version goes unseen.
    ///
    /// Whoever shows the node then says under which lease
    /// (`Reads::note`). A read of the transaction's own view (`own`) is
    /// refused first where a move decides what is under the subtree and
    /// the reader has no frames that show it ([`Self::guard_moved`]); the
    /// committed read is never refused for a move (RD10). Then the read
    /// is claimed for `who` (SC2). An operation that does not begin, or
    /// whose read is refused, gives the watch up again where nothing else
    /// needs it ([`State::unwatch`]), and one that is refused for a move
    /// has claimed nothing.
    fn begin_shown(
        &self,
        state: &mut State<'s>,
        who: Who,
        unit: Unit,
        own: bool,
    ) -> Result<Op<'s>, TxnError> {
        self.watch(state);
        let begun = self.begin(state, false).and_then(|op| {
            if own {
                self.guard_moved(state, &op, who, unit.subtree)?;
            }
            self.claim_read(state, &op.guard, op.at, who, unit)?;
            Ok(op)
        });
        if begun.is_err() {
            state.unwatch();
        }
        begun
    }

    /// Starts to watch the key sets of the commits, if the transaction
    /// does not watch them yet, and gives its watch.
    fn watch<'a>(&self, state: &'a mut State<'s>) -> &'a mut Watch<'s> {
        let commits = &self.path.commits;
        state.watch.get_or_insert_with(|| commits.watch())
    }

    /// Takes a guard and pins the latest version for a cursor over the
    /// nodes of a Merk, which shows them under its own `lease`. The
    /// transaction watches from before the version is pinned, as for any
    /// node it shows ([`Self::begin_shown`]), and dates the lease with it.
    ///
    /// All of it is done under the transaction's lock, so that no
    /// operation of another thread takes a key set in between the two.
    pub(crate) fn pin_shown(&self, lease: Lease) -> (OperationGuard<'s>, VersionPin<'s>) {
        let mut state = self.lock();
        self.watch(&mut state);
        // The guard is taken before the latest version is read (PN5).
        let guard = self.path.versions.guard();
        let pin = self.path.versions.pin();
        state.reads.note(lease, pin.version());
        (guard, pin)
    }

    fn write(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        batch: &Batch<'_>,
    ) -> Result<Applied, TxnError> {
        let path = &self.path;
        state.set.admits(batch)?;
        let Some(mut frames) = Woken::wake(&mut state.frames, &op.guard) else {
            return self.write_by_key(state, op, batch);
        };
        Self::date_moves(&state.set, &state.reads, op.at, &mut frames, batch)?;
        // A value that is kept is one its writer read: when, and what the
        // commits did to its key since (CF2a).
        let kept = Kept::of(&state.set, &state.reads, op.at, frames.lease(), batch);
        if let Some((_, subtree, key)) = kept.unread {
            // A keep that names no read of a node the frames have. One of
            // a node they do not have is refused below, as that.
            let root = frames.root(&subtree)?;
            if find(&mut frames.tree, root, key)?.is_some() {
                return Err(BatchError::NoRead { subtree }.into());
            }
        }
        // What the transaction wrote below a Merk that the batch drops or
        // clears goes with it: the directory says which, before the batch.
        let cut = match batch.has_leaves() {
            true => match frames.below(batch, state.set.written()) {
                Ok(cut) => cut,
                // The batch is refused before anything is applied: the
                // refusal is told as one of the application is.
                Err(refused) => {
                    drop(frames);
                    return Err(self.told(state, op, batch, refused));
                }
            },
            false => Vec::new(),
        };
        match frames.apply(path, batch, None, &mut state.loaded) {
            Ok(applied) => {
                let (empty, left) = (frames.empty_before(), frames.left_with_moves());
                let first = state.set.add(batch, op.at, empty, &cut, left, frames.stored());
                if !kept.met.is_empty() {
                    for (offset, unit, key) in &kept.met {
                        state.set.meet_kept(first + offset, *unit, key);
                    }
                    // The write hands its nodes back by their handles. That
                    // of a met node names bytes its writer never read, under
                    // the hashes it stated, at this write's version: no
                    // read of the committed state. It would pass for one
                    // once a rollback has taken the write out. So the lease
                    // of these handles is not dated: a keep by one of them
                    // is taken to be met. The frames go at the next
                    // operation, and show no node before that.
                    state.reads.forget_frames(frames.lease());
                } else if !applied.nodes.is_empty() || !applied.carried.is_empty() {
                    state.reads.note_frames(frames.lease(), op.at);
                }
                Ok(applied)
            }
            Err(refused) => {
                if refused.dirty {
                    // The frames hold a part of the batch. They are made
                    // again from the write set, which does not (WT10).
                    frames.discard();
                    state.renew_lease();
                    let (set, scopes) = (&state.set, &state.scopes);
                    let frames =
                        Self::build(path, &op.guard, op.at, set, scopes, &mut state.loaded);
                    if let Ok(frames) = frames {
                        state.frames = Some(frames);
                        state.rebuilds += 1;
                        state.stuck = None;
                    }
                } else {
                    drop(frames);
                }
                Err(self.told(state, op, batch, refused.error))
            }
        }
    }

    /// Dates each move of `batch` at the read of the node that hosts the
    /// moved subtree before the batch, as a kept value is dated at the
    /// read it is kept from (CF2a, [`Kept::of`]): the host's value hash
    /// commits to the Merk as the mover read it, and the move carries the
    /// cells as they are now.
    ///
    /// The batch is refused, with nothing changed, where a commit outside
    /// the transaction wrote that node's key since the read the move
    /// names, or may have ([`BatchError::ValueTaken`]), and where the
    /// node is the committed state's and the move names no read
    /// ([`BatchError::NoRead`]). A node the transaction wrote itself with
    /// its value stated needs no date: a commit that wrote its key since
    /// meets that write.
    fn date_moves(
        set: &WriteSet,
        reads: &Reads,
        at: CommitVersion,
        frames: &mut Awake<'s, '_>,
        batch: &Batch<'_>,
    ) -> Result<(), TxnError> {
        for moving in batch.moves() {
            // A subtree that is not entered, or has no host, is refused
            // for that where the batch is applied.
            let Some(entry) = frames.entry(&moving.from)? else {
                continue;
            };
            let Some(host) = entry.host() else {
                continue;
            };
            let subtree = Subtree::new(*host.parent);
            let unit = Unit::data(subtree);
            let lease = moving.read.map(|handle| handle.lease());
            if lease == Some(frames.lease()) {
                continue;
            }
            let since = lease.map(|lease| reads.since(lease, at, unit, host.key));
            match (set.own(unit, host.key), since) {
                (_, Some(Since::Unwritten)) => {}
                (Own::Unwritten, None) => {
                    let root = frames.root(&subtree)?;
                    if find(&mut frames.tree, root, host.key)?.is_some() {
                        return Err(BatchError::NoRead { subtree }.into());
                    }
                }
                (Own::Unwritten, Some(_)) => {
                    return Err(BatchError::ValueTaken { subtree }.into());
                }
                (Own::Node(id), Some(_)) if !set.states_value(id) => {
                    return Err(BatchError::ValueTaken { subtree }.into());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Why a read or a write is not answered for where a move decides:
    /// where a commit has met a write of the transaction, that is a
    /// conflict ([`BatchError::MovedByKey`]); where its writes cannot be
    /// laid out
    /// over the version it reads, it is the refusal of that layout; and
    /// for a scope that keeps its writes in the write set beside the
    /// frames, and for a cursor that reads another version than the
    /// frames are laid out over, [`BatchError::MoveBeside`]. In that
    /// order: the first that holds is the answer.
    fn unlaid(&self, state: &State<'s>, guard: &OperationGuard<'s>, subtree: Subtree) -> TxnError {
        if state.set.met() > 0 {
            return BatchError::MovedByKey { subtree }.into();
        }
        if let Some((at, _)) = state.stuck.filter(|(_, ops)| *ops == state.set.ops()) {
            let mut loaded = Loaded::default();
            let (set, scopes) = (&state.set, &state.scopes);
            let frames = Self::build(&self.path, guard, at, set, scopes, &mut loaded);
            if let Err(refused) = frames {
                return refused;
            }
        }
        BatchError::MoveBeside { subtree }.into()
    }

    /// Whether `who` is answered from the write set over the committed
    /// state, and not from frames that show the transaction's writes: a
    /// commit has met one of them, they cannot be laid out, or `who` is a
    /// scope whose writes are kept in the write set.
    fn by_key(state: &State<'s>, who: Who) -> bool {
        state.set.met() > 0
            || state.stuck.is_some_and(|(_, ops)| ops == state.set.ops())
            || who.id().is_some_and(|id| !state.scopes.laid(id))
    }

    /// Refuses the read or the write of `subtree` by `who` where a move
    /// of the transaction decides what is under it ([`Opened::moved`])
    /// and `who` has no frames that show the move over the committed
    /// version `at` that it reads: `who` is answered from the write set
    /// ([`Self::by_key`]), or the frames are laid out over another
    /// version, which is so for a cursor that was opened before a commit
    /// that the transaction has taken in since. The refusal is that of
    /// [`Self::unlaid`]. `opened` keeps the directory of `at`.
    fn refuse_moved(
        &self,
        state: &State<'s>,
        opened: &mut Option<Opened>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
        who: Who,
        subtree: Subtree,
    ) -> Result<(), TxnError> {
        if !state.set.has_moves() {
            return Ok(());
        }
        let beside = (state.frames.as_ref()).is_some_and(|frames| frames.base() != at);
        if !beside && !Self::by_key(state, who) {
            return Ok(());
        }
        let opened = Opened::at(opened, &self.path, guard, at)?;
        match opened.moved(&self.path, guard, &state.set, &subtree)? {
            true => Err(self.unlaid(state, guard, subtree)),
            false => Ok(()),
        }
    }

    /// [`Self::refuse_moved`], in an operation of the transaction.
    fn guard_moved(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        who: Who,
        subtree: Subtree,
    ) -> Result<(), TxnError> {
        let mut opened = state.directory.take();
        let refused = self.refuse_moved(state, &mut opened, &op.guard, op.at, who, subtree);
        state.directory = opened;
        refused
    }

    /// Refuses `batch`, which `who` writes, where `who` has no frames
    /// that show the transaction's moves ([`Self::by_key`]) and
    ///
    /// - the batch states a move: a move is laid out when it is stated,
    ///   or not made;
    /// - the write set holds a move and the batch drops or clears a
    ///   subtree, whichever one: what such a write takes is asked of the
    ///   committed directory and of the transaction's own enters
    ///   ([`ByKey`]), and those do not have a moved subtree where it is;
    /// - or the batch writes or reads a subtree that a move decides
    ///   ([`Self::refuse_moved`]): one it changes, puts a record under,
    ///   enters, states anew, enters a subtree under, or names as one
    ///   that a node it puts hosts, whose entry the batch reads (SC2).
    ///
    /// The refusal is that of [`Self::unlaid`]. Where that is
    /// [`BatchError::MovedByKey`] or [`BatchError::MoveBeside`], it names
    /// the first subtree in that order: the prefix the first move takes a
    /// subtree from, the first subtree that is dropped or cleared, or the
    /// written subtree. The refusal of a layout that stopped is that
    /// layout's own, as it is. It is asked before the batch takes a claim
    /// (SC3) and
    /// before it is judged by what the write set holds
    /// ([`WriteSet::admits`]): a batch that is refused here has claimed
    /// nothing.
    fn guard_batch(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        who: Who,
        batch: &Batch<'_>,
    ) -> Result<(), TxnError> {
        if !Self::by_key(state, who) {
            return Ok(());
        }
        if let Some(moving) = batch.moves().next() {
            return Err(self.unlaid(state, &op.guard, moving.from));
        }
        if !state.set.has_moves() {
            return Ok(());
        }
        if let Some(first) = batch.leaving().first() {
            return Err(self.unlaid(state, &op.guard, *first));
        }
        let merks = batch.merks.iter().map(|merk| merk.subtree);
        let records = batch.records.iter().map(|record| record.unit().subtree);
        let hosts = batch.subtrees.iter().filter_map(|write| {
            let host = write.entering()?.host?;
            Some(host.subtree)
        });
        let named = batch.subtrees.iter().map(SubtreeWrite::subtree);
        let nodes = batch.merks.iter().flat_map(|merk| merk.nodes);
        let hosted = nodes.flat_map(|node| match node.hosted {
            Hosted::Set(hosted) => hosted,
            Hosted::Keep => &[],
        });
        let written = merks.chain(records).chain(hosts).chain(named);
        // Each subtree is asked once, the first the batch names first.
        let mut asked = HashSet::new();
        let written: Vec<Subtree> = (written.chain(hosted.copied()))
            .filter(|subtree| asked.insert(*subtree))
            .collect();
        for subtree in written {
            self.guard_moved(state, op, who, subtree)?;
        }
        Ok(())
    }

    /// The prefix under which the committed version `at`, whose directory
    /// `opened` keeps, has the records that the transaction reads as
    /// those of `subtree` beside its own (RD3), or none where it reads no
    /// committed record there (CV9).
    ///
    /// For a subtree that no move of the transaction names or took out,
    /// that is the subtree's own prefix, where the version has the
    /// subtree entered, the transaction has not taken it with a subtree
    /// above it that it dropped or cleared ([`Opened::seen`]), and it
    /// does not leave with a move ([`Opened::leaves_with_a_move`]). All
    /// of that is asked of the write set and of the directory of `at`,
    /// and none of it of the transaction's frames: it is answered alike
    /// with frames and without, and for a cursor of whichever version.
    ///
    /// For one that a move names, it is the prefix that the moves took
    /// the subtree from ([`WriteSet::committed_under`]), where the
    /// transaction's own writes left the Merk the one it was: the Merk
    /// that its frames have under `subtree` is the Merk that the version
    /// has under that prefix. The frames show every move, drop, clear and
    /// enter that is laid out. A Merk keeps its id through a move. A
    /// clear of a Merk with a node gives it another id and leaves it the
    /// subtree it was, with its flat records but those of its data, which
    /// the entry of the clear deletes: the frames remember the id it had
    /// ([`Awake::before_clears`]). A subtree that the transaction
    /// dropped, alone or with one above it, and entered again is another
    /// Merk, under another id, and so is one that the transaction entered
    /// itself where the version has none. For such a subtree the frames
    /// are laid out over `at`: whoever has no frames that show the moves
    /// over the version it reads was refused the read before this is
    /// asked ([`Self::refuse_moved`]).
    fn committed_side(
        &self,
        state: &mut State<'s>,
        opened: &mut Option<Opened>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
        subtree: Subtree,
    ) -> Result<Option<Subtree>, TxnError> {
        let path = &self.path;
        let State { set, frames, .. } = state;
        let opened = Opened::at(opened, path, guard, at)?;
        if !set.moves(&subtree) {
            let reads = !opened.leaves_with_a_move(path, guard, set, &subtree)?
                && opened.seen(path, guard, set, &subtree)?.is_some();
            return Ok(reads.then_some(subtree));
        }
        let Some(under) = set.committed_under(subtree) else {
            return Ok(None);
        };
        let shown = Woken::wake(frames, guard).filter(|frames| frames.base() == at);
        let Some(frames) = shown else {
            return Err(TxnError::Damaged(
                "a move is read with no frames that show it",
            ));
        };
        let own = frames.entry(&subtree)?;
        let own = own.map(|entry| frames.before_clears(entry.id()));
        let committed = opened.id(path, guard, &under)?;
        Ok((own.is_some() && own == committed).then_some(under))
    }

    /// What a cursor of `who` over `unit` has as its committed side
    /// ([`Self::sided`]), for a seek or a step of the cursor.
    ///
    /// While the write set holds a move, the call begins as an operation
    /// of the transaction does ([`Self::begin`]), at the latest version.
    /// So a commit that met the transaction, and writes that cannot be
    /// laid out over the version at which the call begins, are known to
    /// a cursor that is the first to call after the commit. Whoever has
    /// no frames that show the moves over the cursor's version is then
    /// refused where a move decides what is under the subtree
    /// ([`Self::refuse_moved`]), by the directory of that version. The
    /// read is claimed after that (SC3).
    pub(crate) fn cursor_side(
        &self,
        who: Who,
        unit: Unit,
        pinned: Pinned<'_, 's>,
    ) -> Result<Sided, TxnError> {
        let mut state = self.lock();
        let state = &mut *state;
        let Pinned {
            opened,
            guard,
            at,
            seen,
            ..
        } = pinned;
        if state.set.has_moves() {
            self.begin(state, false)?;
            self.refuse_moved(state, opened, guard, at, who, unit.subtree)?;
        }
        self.claim_read_now(state, who, unit)?;
        self.sided(state, opened, guard, at, seen, unit.subtree)
    }

    /// The read of the record under the key `key` that a cursor of `who`
    /// over `unit` stands on, as one look at what the transaction holds:
    /// the refusal and the claim of [`Self::cursor_side`], then the
    /// transaction's own record, which is appended to `out`, or that it
    /// deleted the record, and otherwise the cursor's committed side
    /// ([`Self::sided`]), under which the cursor then reads the record of
    /// its own version (RD6).
    pub(crate) fn cursor_record(
        &self,
        who: Who,
        unit: Unit,
        key: &[u8],
        pinned: Pinned<'_, 's>,
        out: &mut Vec<u8>,
    ) -> Result<Result<bool, Sided>, TxnError> {
        #[cfg(test)]
        self.interleave();
        let mut state = self.lock();
        let state = &mut *state;
        let Pinned {
            opened,
            guard,
            at,
            seen,
            ..
        } = pinned;
        if state.set.has_moves() {
            self.begin(state, false)?;
            self.refuse_moved(state, opened, guard, at, who, unit.subtree)?;
        }
        self.claim_read_now(state, who, unit)?;
        match state.set.own(unit, key) {
            Own::Put(value) => {
                out.extend_from_slice(value);
                return Ok(Ok(true));
            }
            Own::Deleted => return Ok(Ok(false)),
            Own::Node(_) => return Err(TxnError::Damaged("a Merk node under a cursor of records")),
            Own::Unwritten => {}
        }
        let side = self.sided(state, opened, guard, at, seen, unit.subtree)?;
        Ok(Err(side))
    }

    /// The committed side of a cursor over `subtree` that reads the
    /// version `at` through `opened`: the answer the cursor has (`seen`)
    /// while the count of [`WriteSet::left`] stands, and otherwise what
    /// [`Self::committed_side`] says now.
    fn sided(
        &self,
        state: &mut State<'s>,
        opened: &mut Option<Opened>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
        seen: Option<(u64, bool)>,
        subtree: Subtree,
    ) -> Result<Sided, TxnError> {
        let left = state.set.left();
        if let Some((_, reads)) = seen.filter(|(seen, _)| *seen == left) {
            let under = None;
            return Ok(Sided { left, reads, under });
        }
        let under = self.committed_side(state, opened, guard, at, subtree)?;
        let reads = under.is_some();
        Ok(Sided { left, reads, under })
    }

    /// Has the transaction rolled back, as by another thread, at the
    /// moment the next read of a record or a node through a cursor comes
    /// to take the transaction's lock.
    #[cfg(test)]
    pub(crate) fn roll_back_before_cursor_read(&self) {
        self.lock().rolls_back = true;
    }

    #[cfg(test)]
    fn interleave(&self) {
        let rolls_back = std::mem::take(&mut self.lock().rolls_back);
        if rolls_back {
            self.rollback().expect("no scope is open");
        }
    }

    /// The refusal `error` of `batch`, which says so where a commit outside
    /// the transaction is its cause ([`BatchError::ValueTaken`], CF2a): a
    /// node of the batch has no value to keep, or its Merk is not entered,
    /// because such a commit took the node away after the read the keep
    /// names. Every other refusal is as it was, and so is this one where a
    /// read that would tell fails.
    fn told(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        batch: &Batch<'_>,
        error: TxnError,
    ) -> TxnError {
        let TxnError::Batch(refused) = &error else {
            return error;
        };
        if let Some(subtree) = Self::taken_from_move(&state.reads, batch, refused) {
            return BatchError::ValueTaken { subtree }.into();
        }
        let (subtree, not_entered) = match refused {
            BatchError::NoValue { subtree } => (*subtree, false),
            BatchError::NotEntered { subtree } => (*subtree, true),
            _ => return error,
        };
        if matches!(self.taken(state, op, batch, subtree), Ok(true)) {
            return BatchError::ValueTaken { subtree }.into();
        }
        // A subtree that the batch writes into, or enters a subtree under,
        // and that a commit which the transaction took in dropped or
        // moved: the key set of that commit names it.
        let written = !batch.leaves(subtree) && batch.moves().all(|moving| moving.from != subtree);
        let named = |keys: &KeySet| keys.subtrees.contains(&subtree);
        match not_entered && written && state.reads.any_since(None, named) {
            true => BatchError::ValueTaken { subtree }.into(),
            false => error,
        }
    }

    /// The subtree that a batch with a move was refused for, where a
    /// commit that the transaction took in since the read the move names
    /// explains the refusal ([`Reads::any_since`]):
    ///
    /// - the moved subtree is not entered, and such a commit wrote every
    ///   key of the Merk its host is in, or took a subtree out, whichever;
    /// - a prefix the move brings a subtree to has one, and such a commit
    ///   wrote the key of the node that hosts the moved subtree
    ///   afterwards, or a key under a prefix the move brings a subtree to.
    ///
    /// A commit that wrote the key of the node which hosts the moved
    /// subtree before is told before the batch is applied
    /// ([`Self::date_moves`]).
    fn taken_from_move(reads: &Reads, batch: &Batch<'_>, refused: &BatchError) -> Option<Subtree> {
        let (subtree, moving) = match refused {
            BatchError::NotEntered { subtree } => (
                *subtree,
                batch.moves().find(|moving| moving.from == *subtree)?,
            ),
            BatchError::Entered { subtree } => {
                let mut moves = batch.moves();
                let brings = |moving: &Moving<'_>| moving.targets().any(|to| to == *subtree);
                (*subtree, moves.find(brings)?)
            }
            _ => return None,
        };
        let lease = moving.read.map(|handle| handle.lease());
        let host = moving.host;
        let explains = |keys: &KeySet| match refused {
            BatchError::NotEntered { .. } => {
                keys.covers_unit(Unit::data(host.subtree)) || keys.takes_any()
            }
            _ => {
                let under = |to: Subtree| {
                    Column::ALL
                        .iter()
                        .any(|column| keys.touches(Unit::new(*column, to)))
                };
                keys.meets_key(Unit::data(host.subtree), host.key) || moving.targets().any(under)
            }
        };
        reads.any_since(lease, explains).then_some(subtree)
    }

    /// Whether commits outside the transaction are why `batch` has no
    /// value to keep in the Merk `subtree`: every node of that Merk whose
    /// value the batch keeps and that is not there is one such a commit
    /// took away, and there is one. A commit took a node away where the
    /// keep names a read the transaction can date, the key set of a commit
    /// since that read holds the node's key, and the committed version the
    /// operation reads has no node under it.
    ///
    /// What is not there for another reason leaves the refusal what it
    /// was: a node the transaction deleted itself, a Merk that it dropped
    /// or cleared itself or that is below one it did, a keep that names no
    /// read, and a read it cannot date. Where the Merk is not entered in
    /// the committed version and the transaction has dropped or cleared
    /// anything, it does not say whose drop it was. A drop of a Merk above
    /// this one by a commit outside is not told either: its key set does
    /// not name this subtree.
    ///
    /// Only a refusal asks, so no write that is taken pays for it.
    fn taken(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        batch: &Batch<'_>,
        subtree: Subtree,
    ) -> Result<bool, TxnError> {
        if batch.leaves(subtree) {
            return Ok(false);
        }
        let Some(merk) = batch.merks.iter().find(|merk| merk.subtree == subtree) else {
            return Ok(false);
        };
        let (path, guard) = (&self.path, &op.guard);
        let State {
            directory,
            set,
            reads,
            buffers,
            lease,
            ..
        } = state;
        // What the committed version has of the Merk, and whether what the
        // transaction dropped or cleared itself hides it.
        let opened = Opened::at(directory, path, guard, op.at)?;
        let entered = opened.root(path, guard, &subtree)?;
        let hidden = match entered {
            Some(_) => opened.seen(path, guard, set, &subtree)?.is_none(),
            None => set.leaving().next().is_some(),
        };
        if hidden {
            return Ok(false);
        }
        let unit = Unit::data(subtree);
        let mut pages = path.committed_pages(guard, op.at, *lease, buffers);
        let mut taken = false;
        for node in merk.nodes {
            if node.value != ValuePut::Keep {
                continue;
            }
            match set.own(unit, node.key) {
                Own::Unwritten => {}
                // Its own node is there, with a value to keep.
                Own::Node(_) => continue,
                Own::Deleted | Own::Put(_) => return Ok(false),
            }
            if find(&mut pages, entered.flatten(), node.key)?.is_some() {
                continue;
            }
            let since = node
                .at
                .map(|handle| reads.since(handle.lease(), op.at, unit, node.key));
            if since != Some(Since::Written) {
                return Ok(false);
            }
            taken = true;
        }
        Ok(taken)
    }

    /// Adds a batch to a write set that is served by key: no page is
    /// changed, and every value the batch keeps is copied now. What a
    /// move decides is refused before ([`Self::guard_batch`]).
    fn write_by_key(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        batch: &Batch<'_>,
    ) -> Result<Applied, TxnError> {
        let path = &self.path;
        for record in batch.records {
            // The transaction has written no node of the subtree since it
            // last dropped or cleared it, or the write set would not admit
            // the record: the subtree holds nodes if it has a root.
            let unit = record.unit();
            let flat = unit.column != Column::Data || batch.leaves(unit.subtree);
            if !flat && self.rooted(state, op, &unit.subtree)? {
                return Err(BatchError::NotFlat { unit }.into());
            }
        }
        // The lease a handle of this operation's view is of: the frames',
        // for a scope that keeps its writes in the write set beside them.
        let current = (state.frames.as_ref()).map_or(state.lease, |frames| frames.tree.lease());
        let read = Kept::of(&state.set, &state.reads, op.at, current, batch);
        let mut kept = Vec::new();
        let mut empty = Vec::with_capacity(batch.merks.len());
        let mut entry: EntryId = 0;
        for merk in batch.merks {
            let subtree = merk.subtree;
            let left = batch.leaves(subtree);
            let rootless = left || !self.rooted(state, op, &subtree)?;
            if is_load(merk) && !rootless {
                // A load is the whole of a Merk that had no root.
                let fault = super::error::WholeFault::HasRoot;
                return Err(BatchError::NotWhole { subtree, fault }.into());
            }
            empty.push(rootless);
            for node in merk.nodes {
                entry += 1;
                if node.value != ValuePut::Keep {
                    continue;
                }
                let unread = read.unread.is_some_and(|(at, ..)| at == entry - 1);
                let value = match state.set.own(Unit::data(subtree), node.key) {
                    // What the batch itself drops or clears is gone before
                    // its nodes are written, the transaction's own node
                    // with it.
                    _ if left => None,
                    Own::Node(id) => match state.set.node_value(id) {
                        Some(NodeValue::Bytes(value)) => Some(value.to_vec()),
                        // A value that a load put into the value log.
                        Some(NodeValue::Stored(stored)) => {
                            let (mut records, mut joined) = (Vec::new(), Vec::new());
                            let value = stored.read(path.values, &mut records, &mut joined)?;
                            Some(value.to_vec())
                        }
                        // The node is one of the frames, which hold its
                        // value: the batch is one that is not laid out.
                        None => self.framed_value(state, op, &subtree, node.key)?,
                    },
                    Own::Deleted | Own::Put(_) => None,
                    Own::Unwritten => {
                        let seen = self.seen(state, &op.guard, op.at, &subtree)?;
                        let root = seen.flatten();
                        let mut pages =
                            path.committed_pages(&op.guard, op.at, state.lease, &mut state.buffers);
                        match find(&mut pages, root, node.key)? {
                            // An unlinked record under the key is no
                            // node, and has no value to keep.
                            None => None,
                            // A committed value, and no read of it is
                            // named: refused before the value is read.
                            Some(_) if unread => {
                                return Err(BatchError::NoRead { subtree }.into());
                            }
                            Some(address) => Some(Self::read_value(&mut pages, address)?),
                        }
                    }
                };
                let Some(value) = value else {
                    let refused = BatchError::NoValue { subtree }.into();
                    return Err(self.told(state, op, batch, refused));
                };
                kept.push((entry - 1, value));
            }
            entry += merk.removed.len() as EntryId;
        }
        let cut = self.cut(state, op, batch)?;
        if batch.has_leaves() {
            // The cells of a Merk that leaves stay in their pages: every
            // handle that was read before ends (PN8).
            state.renew_lease();
            state.lease_at = Some(op.at);
        }
        // A load that is taken by key has no page: its values stay with
        // the write set until its frames are made.
        let first = state.set.add(batch, op.at, &empty, &cut, &[], &[]);
        for (offset, value) in kept {
            state.set.keep(first + offset, &value);
        }
        // No handle is handed back: the records have no cells.
        for (offset, unit, key) in &read.met {
            state.set.meet_kept(first + offset, *unit, key);
        }
        Ok(Applied::default())
    }

    /// The bytes of the value of the committed cell at `address`, wherever
    /// the value is.
    fn read_value(
        pages: &mut CommittedPages<'_, 's>,
        address: CellAddress,
    ) -> Result<Vec<u8>, TxnError> {
        let node = pages.pinned(address)?;
        let mut buf = ValueBuf::new();
        Ok(read_value(pages.values, &node, &mut buf)?.to_vec())
    }

    fn get_in(
        &self,
        who: Who,
        unit: Unit,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError> {
        let mut state = self.lock();
        let state = &mut *state;
        let op = self.begin(state, false)?;
        self.guard_moved(state, &op, who, unit.subtree)?;
        self.claim_read(state, &op.guard, op.at, who, unit)?;
        match state.set.own(unit, key) {
            Own::Put(value) => {
                out.extend_from_slice(value);
                Ok(true)
            }
            Own::Deleted => Ok(false),
            Own::Node(_) => Err(TxnError::Damaged("a Merk node is read as a flat record")),
            // No record is read under a subtree that is not entered, or
            // that went with one the transaction dropped or moved. The
            // committed state has the records of a subtree that the
            // transaction moved under the prefix it took the subtree from.
            Own::Unwritten => {
                let mut opened = state.directory.take();
                let under = self.committed_side(state, &mut opened, &op.guard, op.at, unit.subtree);
                state.directory = opened;
                match under? {
                    Some(under) => {
                        let unit = Unit::new(unit.column, under);
                        self.committed_record(state, &op, unit, key, out)
                    }
                    None => Ok(false),
                }
            }
        }
    }

    fn committed_record(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        unit: Unit,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError> {
        let path = &self.path;
        stored_key(&mut state.key, &unit.subtree, key);
        let tree = path.keyspaces.of(unit.column);
        Ok(tree.get(&path.committed(&op.guard, op.at), &state.key, out)?)
    }

    /// Appends to `out` the bytes of the unlinked record that the
    /// committed version `at` holds under `key` of `subtree`, and says
    /// whether it holds one (`super::unlinked`). The caller has found no
    /// node of the Merk's tree under the key.
    ///
    /// No record is read under a subtree without its entry. Whether the
    /// subtree holds unlinked records at all is looked up once for the
    /// version ([`Opened::has_unlinked`]), and where it holds none,
    /// nothing else is read.
    fn committed_unlinked(
        &self,
        state: &mut State<'s>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
        subtree: &Subtree,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError> {
        let path = &self.path;
        let opened = Opened::at(&mut state.directory, path, guard, at)?;
        let entered = opened.root(path, guard, subtree)?.is_some();
        if !entered || !opened.has_unlinked(path, guard, subtree)? {
            return Ok(false);
        }
        let pages = path.committed(guard, at);
        Ok(unlinked::get(
            &path.keyspaces.data,
            &pages,
            subtree,
            key,
            out,
        )?)
    }

    /// Whether the committed version `at` holds unlinked records under
    /// `subtree`, for a cursor that reads that version beside the
    /// transaction's writes. The cursor pins `at` and has found the subtree
    /// entered there. What the transaction found for the version is not
    /// looked up again.
    pub(crate) fn committed_has_unlinked(
        &self,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        let mut state = self.lock();
        let path = &self.path;
        // The transaction keeps the directory of one version, the one it
        // read last: a cursor of another version asks for itself.
        if (state.directory.as_ref()).is_some_and(|opened| opened.version() != at) {
            let pages = path.committed(guard, at);
            return Ok(unlinked::any(&path.keyspaces.data, &pages, subtree)?);
        }
        let opened = Opened::at(&mut state.directory, path, guard, at)?;
        opened.has_unlinked(path, guard, subtree)
    }

    fn committed_get_in(
        &self,
        who: Who,
        unit: Unit,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError> {
        let mut state = self.lock();
        let state = &mut *state;
        let op = self.begin(state, false)?;
        self.claim_read(state, &op.guard, op.at, who, unit)?;
        let opened = Opened::at(&mut state.directory, &self.path, &op.guard, op.at)?;
        match opened.root(&self.path, &op.guard, &unit.subtree)? {
            Some(_) => self.committed_record(state, &op, unit, key, out),
            None => Ok(false),
        }
    }

    /// The root cell of the Merk `subtree` in the committed version `at`,
    /// read under `guard`: none for a Merk with no node, and for a subtree
    /// that is not entered there.
    fn committed_root(
        &self,
        state: &mut State<'s>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
        subtree: &Subtree,
    ) -> Result<Option<CellAddress>, TxnError> {
        let opened = Opened::at(&mut state.directory, &self.path, guard, at)?;
        Ok(opened.root(&self.path, guard, subtree)?.flatten())
    }

    /// Whether the committed version `at` has records of `subtree` for the
    /// transaction to read beside its own, and where the root of its Merk
    /// is: `None` for a subtree that is not entered there, or that is
    /// below one the transaction dropped or cleared ([`Opened::seen`]).
    fn seen(
        &self,
        state: &mut State<'s>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
        subtree: &Subtree,
    ) -> Result<Option<Option<CellAddress>>, TxnError> {
        let State { directory, set, .. } = state;
        let opened = Opened::at(directory, &self.path, guard, at)?;
        opened.seen(&self.path, guard, set, subtree)
    }

    /// Whether the Merk `subtree` has a root as a transaction with no
    /// frames sees it: the one its writes gave it last, and otherwise the
    /// committed one.
    fn rooted(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        if state.set.enters_if_absent(subtree) {
            // Whether such an enter found the subtree, with the root it
            // had, is what a read of the subtree asks.
            return self.said(state, op, *subtree, |view| {
                view.is_some_and(|view| view.root_key.is_some())
            });
        }
        match state.set.rooted(subtree) {
            Some(rooted) => Ok(rooted),
            None => {
                let seen = self.seen(state, &op.guard, op.at, subtree)?;
                Ok(seen.flatten().is_some())
            }
        }
    }

    /// Shows `read` what the subtree directory holds of `subtree` as `who`
    /// sees it, or `None` for a subtree that is not entered: the frames'
    /// directory, which has the enters, drops, clears and roots of every
    /// write that is laid out, and otherwise what the write set says of
    /// the subtree over the entry of the committed version the operation
    /// reads.
    ///
    /// The write set answers where the transaction has no frames, and for
    /// a scope whose writes are kept in it (`crate::scope`): the frames do
    /// not show those. Every write of the set is asked, laid out or not.
    /// A unit that `who` reads has no write of another open scope, and no
    /// other open scope has taken a subtree above it (SC3).
    ///
    /// The entry of a Merk is of the Merk's unit (SC2): the read claims
    /// its data column.
    fn subtree_in<R>(
        &self,
        who: Who,
        subtree: Subtree,
        read: impl FnOnce(Option<SubtreeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        let mut state = self.lock();
        let state = &mut *state;
        let op = self.begin(state, false)?;
        self.guard_moved(state, &op, who, subtree)?;
        self.claim_read(state, &op.guard, op.at, who, Unit::data(subtree))?;
        if who.id().is_none_or(|id| state.scopes.laid(id))
            && let Some(frames) = Woken::wake(&mut state.frames, &op.guard)
        {
            let entry = frames.entry(&subtree)?;
            return Ok(read(entry.as_ref().map(SubtreeView::of)));
        }
        self.said(state, &op, subtree, read)
    }

    /// Shows `read` what the transaction's write set says of `subtree`
    /// over the committed version that `op` reads, or `None` for a subtree
    /// that is not entered: what a transaction with no frames answers.
    ///
    /// The subtree is the committed one, the transaction's own, or gone
    /// ([`ByKey::standing`]). The committed one is shown as the committed
    /// version has it, with the root the transaction's writes gave it and
    /// not where the transaction took a subtree above it. Its own is
    /// shown as the enter that made it states it.
    fn said<R>(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        subtree: Subtree,
        read: impl FnOnce(Option<SubtreeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        let State { directory, set, .. } = state;
        let (set, ops) = (&*set, set.ops());
        let mut by_key = ByKey::new(&self.path, &op.guard, op.at, set, directory)?;
        let Said { base, if_absent } = set.subtree(&subtree, ops);
        let entered = match by_key.standing(subtree, ops)? {
            Standing::Gone => return Ok(read(None)),
            Standing::Committed => {
                let (root, stated) = match base {
                    OwnSubtree::Unwritten { root, stated } => (root, stated),
                    OwnSubtree::Entered { .. } | OwnSubtree::Gone => (None, Restated::default()),
                };
                let opened = by_key.opened;
                let entry = opened.seen_entry(&self.path, &op.guard, set, &subtree)?;
                return Ok(read(entry.map(|entry| {
                    let view = SubtreeView::of(entry);
                    stated.of(SubtreeView {
                        root_key: root.unwrap_or(view.root_key),
                        ..view
                    })
                })));
            }
            Standing::Own(entered) => entered,
        };
        let view = match base {
            OwnSubtree::Entered { view, op } if op == entered => Some(view),
            _ => (if_absent.iter()).find_map(|(view, at)| (*at == entered).then_some(*view)),
        };
        // A subtree that was entered and not written is named by no drop:
        // it went with a subtree above it that left since.
        let left = set.left_after(entered);
        let gone = !left.is_empty() && by_key.below(subtree, &left, Some((entered, ops)))?;
        Ok(read(view.filter(|_| !gone)))
    }

    /// Those of the subtrees the transaction has written that are below
    /// one that `batch` drops or clears, where the transaction has no
    /// frames to ask ([`ByKey::below`]).
    fn cut(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        batch: &Batch<'_>,
    ) -> Result<Vec<Subtree>, TxnError> {
        let leaving = batch.leaving();
        if leaving.is_empty() {
            return Ok(Vec::new());
        }
        let State { directory, set, .. } = state;
        let mut by_key = ByKey::new(&self.path, &op.guard, op.at, set, directory)?;
        let mut cut = Vec::new();
        for written in set.written().filter(|written| !leaving.contains(written)) {
            if by_key.below(written, &leaving, None)? {
                cut.push(written);
            }
        }
        Ok(cut)
    }

    /// Shows `read` the node under `key` as the transaction has it: in its
    /// frames, which show its whole view, or as the record it wrote. Where
    /// it serves its records by key and has written none under `key`,
    /// `read` is handed back: the node is then a committed one.
    fn own_node<R, F>(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        subtree: Subtree,
        key: &[u8],
        beside: Beside<'_>,
        read: F,
    ) -> Result<Result<R, F>, TxnError>
    where
        F: FnOnce(Option<NodeView<'_>>) -> R,
    {
        let (hint, children) = match beside {
            Beside::Hint(hint) => (hint, None),
            Beside::Children(children) => (None, children),
        };
        if let Some(mut frames) = Woken::wake(&mut state.frames, &op.guard) {
            // The frames show the transaction's view (WT3): the node is
            // where a good handle says, or where its key leads to.
            let followed = hint.and_then(|handle| frames.follow(handle, key));
            let at = match followed {
                Some(at) => {
                    state.followed += 1;
                    Some(at)
                }
                None => {
                    state.keyed += 1;
                    let entry = frames.entry(&subtree)?;
                    let root = entry.as_ref().and_then(|entry| entry.root());
                    let found = find(&mut frames.tree, root.map(|root| root.address), key)?;
                    if found.is_none() && entry.is_some() {
                        // No node of the tree is under the key, which can
                        // hold an unlinked record.
                        let mut record = Vec::new();
                        if frames.unlinked(&self.path, &subtree, key, &mut record)? {
                            let node = unlinked::decode(key, &record)?;
                            return Ok(Ok(read(Some(NodeView::unlinked(node)))));
                        }
                    }
                    found
                }
            };
            if at.is_some() {
                state.reads.note_frames(frames.lease(), op.at);
            }
            if let (Some(at), Some(children)) = (at, children) {
                child_keys(&mut frames.tree, at, children)?;
            }
            return show_framed(&mut frames, at, read).map(Ok);
        }
        let own = state.set.own(Unit::data(subtree), key);
        if own != Own::Unwritten {
            state.keyed += 1;
        }
        match own {
            Own::Node(id) => {
                let record = NodeView::record(&state.set, id, self.path.values);
                Ok(Ok(read(Some(record))))
            }
            Own::Deleted => Ok(Ok(read(None))),
            Own::Put(_) => Err(TxnError::Damaged("a flat record is read as a Merk node")),
            Own::Unwritten => Ok(Err(read)),
        }
    }

    fn node_in<R>(
        &self,
        who: Who,
        subtree: Subtree,
        key: &[u8],
        hint: Option<NodeHandle>,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        let mut state = self.lock();
        let state = &mut *state;
        let op = self.begin_shown(state, who, Unit::data(subtree), true)?;
        let read = match self.unlaid_node(state, &op, who, subtree, key, read)? {
            Ok(shown) => return Ok(shown),
            Err(read) => read,
        };
        let read = match self.own_node(state, &op, subtree, key, Beside::Hint(hint), read)? {
            Ok(shown) => return Ok(shown),
            Err(read) => read,
        };
        // What the transaction dropped hides the Merks below it, also from
        // a handle.
        let seen = self.seen(state, &op.guard, op.at, &subtree)?;
        let root = seen.flatten();
        let lease = state.lease;
        let mut pages = (self.path).committed_pages(&op.guard, op.at, lease, &mut state.buffers);
        let followed = (hint.filter(|_| root.is_some()))
            .and_then(|handle| follow(&mut pages, lease, handle, key));
        let at = match followed {
            Some(at) => {
                state.followed += 1;
                Some(at)
            }
            None => {
                state.keyed += 1;
                find(&mut pages, root, key)?
            }
        };
        if at.is_none() {
            // No node of the tree is shown: there is no handle to watch
            // the commits for. The key can hold an unlinked record, which
            // has none.
            state.unwatch();
            let (guard, mut record) = (&op.guard, Vec::new());
            let held = seen.is_some()
                && self.committed_unlinked(state, guard, op.at, &subtree, key, &mut record)?;
            let node = held.then(|| unlinked::decode(key, &record)).transpose()?;
            return Ok(read(node.map(NodeView::unlinked)));
        }
        state.reads.note(lease, op.at);
        show_committed(&mut pages, at, read)
    }

    /// The read of the node under the key `key` that a cursor of `who`
    /// over the Merk `subtree` stands on, as one operation of the
    /// transaction: what it holds is looked at once, under its lock, and
    /// every answer is of that moment.
    ///
    /// It begins as an operation does, and watches from here on, as one
    /// that may show a node; a call that fails gives the watch up again
    /// where nothing else needs it ([`State::unwatch`]). Whoever has no
    /// frames that show the transaction's moves over the cursor's version
    /// is refused where a move decides what is under the subtree, by the
    /// directory of that version, as a seek is ([`Self::cursor_side`]).
    /// Then `read`
    /// is shown the transaction's own node, or that it deleted the node,
    /// if it has a write that covers the key. With none, `read` is handed
    /// back with the cursor's committed side ([`Self::sided`]), and
    /// nothing committed is read: the cursor then reads the node of its
    /// own version (RD6) under the prefix it was told, under its own
    /// lease, and says what it read ([`Self::cursor_read`]). From here
    /// until it has, the read is under way ([`Reads::begun`]): the watch
    /// stays, and the key sets that are taken in meanwhile are kept, as
    /// many as are kept for a lease, whatever else the transaction gives
    /// up, also where it ends ([`State::end`]). It answers for no keep
    /// before it has found a node ([`Self::cursor_shown`]).
    ///
    /// `children`, where it is given, takes the key of each child of a
    /// node that is shown from a cell of the frames, in the same operation.
    /// A record of the write set names its children itself.
    pub(crate) fn cursor_node<R, F>(
        &self,
        who: Who,
        subtree: Subtree,
        key: &[u8],
        children: Option<&mut ChildKeys>,
        pinned: Pinned<'_, 's>,
        read: F,
    ) -> Result<Result<R, (F, Sided)>, TxnError>
    where
        F: FnOnce(Option<NodeView<'_>>) -> R,
    {
        #[cfg(test)]
        self.interleave();
        let mut state = self.lock();
        let state = &mut *state;
        let Pinned {
            opened,
            guard,
            at,
            seen,
            lease,
        } = pinned;
        let unit = Unit::data(subtree);
        self.watch(state);
        let shown: Result<Result<R, (F, Sided)>, TxnError> = (|| {
            let op = self.begin(state, false)?;
            self.refuse_moved(state, opened, guard, at, who, subtree)?;
            self.claim_read(state, &op.guard, op.at, who, unit)?;
            let read = match self.unlaid_node(state, &op, who, subtree, key, read)? {
                Ok(shown) => return Ok(Ok(shown)),
                Err(read) => read,
            };
            let read = match state.set.own(unit, key) == Own::Unwritten {
                true => read,
                false => {
                    let beside = Beside::Children(children);
                    match self.own_node(state, &op, subtree, key, beside, read)? {
                        Ok(shown) => return Ok(Ok(shown)),
                        Err(read) => read,
                    }
                }
            };
            let side = self.sided(state, opened, guard, at, seen, subtree)?;
            Ok(Err((read, side)))
        })();
        match (&shown, lease) {
            // The cursor's own read is under way from here on, until the
            // cursor says what it read.
            (Ok(Err(_)), Some(lease)) => state.reads.begun(lease),
            (Ok(_), _) => {}
            (Err(_), _) => state.unwatch(),
        }
        shown
    }

    /// A cursor found a node that has a handle in the read of its own
    /// version, `pinned`, under its `lease`, that [`Self::cursor_node`]
    /// handed back to it, and is about to give the node to its caller. An
    /// unlinked record has no handle, and is not told. The lease is dated at
    /// `pinned`, if the transaction does not remember it, and the
    /// transaction watches, as when the read has ended
    /// ([`Self::cursor_read`]): the caller can hand the node's handle to
    /// another thread before that.
    pub(crate) fn cursor_shown(&self, lease: Lease, pinned: CommitVersion) {
        let mut state = self.lock();
        let state = &mut *state;
        self.watch(state);
        state.reads.note(lease, pinned);
    }

    /// A cursor ended the read of a node of its own version, `pinned`,
    /// under its `lease`, that [`Self::cursor_node`] handed back to it:
    /// `shown` says whether it was shown a node that has a handle, which
    /// an unlinked record has not. The transaction's lock
    /// was not held over that read, so what the transaction holds is
    /// looked at here, as it is now.
    ///
    /// A node that was shown dates the lease at `pinned` here as well, if
    /// the transaction does not remember it: it may have ended since the
    /// node was found. The transaction watches from here on: a keep by
    /// the node's handle is answered for where the key sets it holds
    /// reach back to `pinned`, and is taken to be met otherwise
    /// ([`Reads::since`]). With no node shown, the watch is given up
    /// where nothing else needs it ([`State::unwatch`]).
    pub(crate) fn cursor_read(&self, lease: Lease, pinned: CommitVersion, shown: bool) {
        let mut state = self.lock();
        let state = &mut *state;
        if shown {
            self.watch(state);
        }
        state.reads.ended(lease, pinned, shown);
        if !shown {
            state.unwatch();
        }
    }

    fn committed_node_in<R>(
        &self,
        who: Who,
        subtree: Subtree,
        key: &[u8],
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        let mut state = self.lock();
        let state = &mut *state;
        let op = self.begin_shown(state, who, Unit::data(subtree), false)?;
        let root = self.committed_root(state, &op.guard, op.at, &subtree)?;
        // The committed node is found by its key, in the committed tree:
        // the transaction may have rebuilt the tree around it (RD10). It is
        // read under a lease that no other read uses, so no handle to it
        // is followed into the transaction's frames. The lease is one for
        // each version.
        let lease = match state.committed {
            Some((lease, at)) if at == op.at => lease,
            _ => {
                let lease = Lease::take();
                state.committed = Some((lease, op.at));
                lease
            }
        };
        let mut pages = (self.path).committed_pages(&op.guard, op.at, lease, &mut state.buffers);
        let at = find(&mut pages, root, key)?;
        if at.is_none() {
            state.unwatch();
            // The committed record under the key can be an unlinked one:
            // a delete with no cost is charged from it as from a node.
            let (guard, mut record) = (&op.guard, Vec::new());
            let held = self.committed_unlinked(state, guard, op.at, &subtree, key, &mut record)?;
            let node = held.then(|| unlinked::decode(key, &record)).transpose()?;
            return Ok(read(node.map(NodeView::unlinked)));
        }
        state.reads.note(lease, op.at);
        show_committed(&mut pages, at, read)
    }

    fn child_in<R>(
        &self,
        who: Who,
        subtree: Subtree,
        parent: NodeHandle,
        side: Side,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<Option<R>, TxnError> {
        let mut state = self.lock();
        let state = &mut *state;
        let op = self.begin_shown(state, who, Unit::data(subtree), true)?;
        if Self::reads_by_key(state, who, Unit::data(subtree)) {
            state.unwatch();
            return Ok(None);
        }
        if let Some(mut frames) = Woken::wake(&mut state.frames, &op.guard) {
            if parent.lease() != frames.lease() {
                return Ok(None);
            }
            let child = frames.tree.page(parent.page(), |page| {
                let node = page.resolve(parent)?;
                Some(node.link(side).map(|link| link.child))
            });
            let Ok(Some(child)) = child else {
                return Ok(None);
            };
            state.followed += 1;
            if child.is_some() {
                state.reads.note_frames(frames.lease(), op.at);
            }
            return Ok(Some(show_framed(&mut frames, child, read)?));
        }
        if parent.lease() != state.lease {
            state.unwatch();
            return Ok(None);
        }
        // The parent's handle is of this lease, so the lease is dated
        // already: by the read that showed the parent.
        let unit = Unit::data(subtree);
        let lease = state.lease;
        let mut pages = (self.path).committed_pages(&op.guard, op.at, lease, &mut state.buffers);
        let mut key = std::mem::take(&mut state.key);
        key.clear();
        let child = pages.page(parent.page(), |page| {
            let node = page.resolve(parent)?;
            key.extend_from_slice(node.key());
            Some(node.link(side).map(|link| link.child))
        });
        // A node the transaction has written since is its record now, and
        // that is read by its key (RD5).
        let stale = !matches!(state.set.own(unit, &key), Own::Unwritten);
        let (Ok(Some(child)), false) = (child, stale) else {
            state.key = key;
            return Ok(None);
        };
        state.followed += 1;
        let shown = match child {
            None => Ok(read(None)),
            Some(address) => {
                // The overlay is by record (RD5): the child is the
                // transaction's node under the child's key if it wrote one.
                key.clear();
                let found = pages.page(address.page(), |page| {
                    let node = page.node(address.slot())?;
                    key.extend_from_slice(node.key());
                    Some(())
                })?;
                found.ok_or(NodeError::NoNode { address })?;
                match state.set.own(unit, &key) {
                    Own::Unwritten => show_committed(&mut pages, Some(address), read),
                    Own::Node(id) => {
                        let record = NodeView::record(&state.set, id, self.path.values);
                        Ok(read(Some(record)))
                    }
                    Own::Deleted => Ok(read(None)),
                    Own::Put(_) => Err(TxnError::Damaged("a flat record is read as a Merk node")),
                }
            }
        };
        state.key = key;
        shown.map(Some)
    }

    /// Whether the transaction's newest write of the record key hides or
    /// replaces the committed record, and with what.
    pub(crate) fn with_set<R>(
        &self,
        who: Who,
        unit: Unit,
        read: impl FnOnce(&WriteSet) -> R,
    ) -> Result<R, TxnError> {
        let mut state = self.lock();
        self.claim_read_now(&mut state, who, unit)?;
        Ok(read(&state.set))
    }

    /// Commits a transaction that has written nothing, and says whether it
    /// was one: such a commit succeeds and takes no version (CV2), so it
    /// needs no commit lock (CM3 step 1). What [`Self::seal`] does for an
    /// empty write set is done here: the transaction is ended as by any
    /// commit. Its savepoints go (WT8), its handles end, and it gives up
    /// its watch on the commits and what it remembers of its reads.
    ///
    /// It is refused while a scope is open or a pending mark stands (CM1),
    /// as the commit is.
    pub(crate) fn seal_empty(&self) -> Result<bool, TxnError> {
        let mut state = self.closed()?;
        if !state.scopes.finalized() {
            return Err(TxnError::NotFinalized);
        }
        if !state.set.is_empty() {
            return Ok(false);
        }
        state.end();
        Ok(true)
    }

    /// Hands the transaction to its commit (`transactions.md` CM3 steps 1
    /// to 4). The caller holds the commit lock, so the latest version does
    /// not move.
    ///
    /// The commits published since the transaction last looked are taken
    /// in. If one of them met a write of the transaction, the commit fails
    /// with a conflict (CF2) and the transaction is ended, its writes
    /// discarded (CM5). Otherwise its frames are made over the latest
    /// version, if they are not (WT7a), and handed over with the key set
    /// of the commit: the transaction is empty afterwards.
    ///
    /// It is refused while a scope is open (CM1), and for a write set that
    /// cannot be laid out over the committed state. The transaction is
    /// then as it was.
    pub(crate) fn seal(&self) -> Result<Sealed<'s>, TxnError> {
        let mut state = self.closed()?;
        let state = &mut *state;
        if !state.scopes.finalized() {
            return Err(TxnError::NotFinalized);
        }
        if state.set.is_empty() {
            // The savepoints of a transaction that commits are ignored
            // (WT8), also where it commits nothing, and it is ended as by
            // any commit.
            state.end();
            return Ok(Sealed::Empty);
        }
        // The conflict is decided before anything else is read, so that no
        // error of a read the transaction has no more use for stands in
        // its place (CF6). What a drop covers below the subtree it names
        // is read from the subtree directory: a write that is met without
        // it is the conflict, whatever that read came to.
        let taken = {
            let guard = self.path.versions.guard();
            self.take_in(state, &guard, guard.latest())
        };
        if state.set.met() > 0 {
            state.end();
            return Ok(Sealed::Conflict);
        }
        taken?;
        let op = self.begin(state, false)?;
        let Some(frames) = state.frames.take() else {
            if state.set.met() > 0 {
                // A commit was published since: the caller does not hold
                // the commit lock.
                state.end();
                return Ok(Sealed::Conflict);
            }
            // The write set names what the committed state does not hold.
            let (set, scopes) = (&state.set, &state.scopes);
            let built = Self::build(&self.path, &op.guard, op.at, set, scopes, &mut state.loaded);
            return Err(match built {
                Err(error) => error,
                Ok(_) => TxnError::Damaged("frames that could not be made are made"),
            });
        };
        let keys = state.set.key_set();
        let pin = state.base.take();
        // The hold on the values of its loads goes to the commit, which
        // counts those that cells hold (`log-format.md` LG16).
        let loaded = std::mem::take(&mut state.loaded);
        state.end();
        let Frames {
            tree,
            values,
            hashes,
            flat,
            directory,
            dropped,
            ..
        } = frames;
        Ok(Sealed::Ready(Box::new(Parts {
            base: op.at,
            pin,
            tree,
            values,
            hashes,
            flat,
            directory,
            dropped,
            keys,
            loaded,
        })))
    }

    /// Has a durable manifest cover the values that the transaction's
    /// loads put into the value log, if it has any that none covers: the
    /// part of a load's commit that syncs a file and replaces the
    /// manifest, made before the commit lock is taken ([`cover`]).
    pub(crate) fn cover_loaded(&self) -> Result<(), TxnError> {
        let state = self.lock();
        cover(&self.path, &state.loaded)
    }

    /// How many stretches of value-log files hold the values of the
    /// transaction's loads.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn loaded_runs(&self) -> usize {
        self.lock().loaded.runs()
    }
}

/// A transaction that is held for one call on its marks and its savepoints
/// (`crate::savepoint`): the call has the transaction's own lock, so the
/// write set and the scope are what it reads them to be until it returns.
pub(crate) struct Held<'t, 's>(MutexGuard<'t, State<'s>>);

impl Held<'_, '_> {
    /// Whether a scope is open.
    pub(crate) fn scope_open(&self) -> bool {
        self.0.scopes.any_open()
    }

    /// The savepoints that are set, oldest first.
    pub(crate) fn savepoints(&mut self) -> &mut Vec<Mark> {
        &mut self.0.savepoints
    }

    /// [`WriteTxn::mark`].
    pub(crate) fn mark(&self) -> Mark {
        self.0.mark()
    }

    /// [`WriteTxn::rewind`].
    pub(crate) fn rewind(&mut self, mark: Mark) -> bool {
        self.0.rewind(mark)
    }
}

impl fmt::Debug for WriteTxn<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteTxn")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

/// What a write transaction is when it is handed to its commit.
pub(crate) enum Sealed<'s> {
    /// The write set is empty: the commit succeeds and takes no version
    /// (CV2).
    Empty,
    /// A commit outside the transaction wrote a key after the transaction
    /// first wrote it (CF2). The transaction is ended.
    Conflict,
    /// The transaction's frames over the latest version, to install.
    Ready(Box<Parts<'s>>),
}

/// The frames of a transaction that commits (CM3 steps 6 and 7).
///
/// Parts that are dropped and not installed give up their private copies
/// and the page ids they took, as a rollback does (CM5).
pub(crate) struct Parts<'s> {
    /// The latest version, which the frames are built over.
    pub(crate) base: CommitVersion,
    /// The pin on it. The commit keeps it until it has installed.
    pub(crate) pin: Option<VersionPin<'s>>,
    /// The private tree pages, with the run of each.
    pub(crate) tree: super::pages::TreePages<'s>,
    /// The values of tree cells that are out of line: those the commit
    /// logs, and the pointers it releases.
    pub(crate) values: lignin_tree::value::TreeValues,
    /// The hashes that are stale in the tree pages. The commit evaluates
    /// them all before it logs a page (CM3 step 5), with the rules of its
    /// caller and [`TreeValues::reader`](lignin_tree::value::TreeValues::reader)
    /// for the values.
    pub(crate) hashes: lignin_tree::hash::Hashes,
    /// The flat pages (`lignin_tree::flat`, "What a commit writes"). The
    /// commit takes the writer out, under its own guard, when it installs
    /// them.
    pub(crate) flat: super::frames::FlatFrames<'s, 's, super::path::Idle>,
    /// The subtree directory as the flat pages have it: the handle of the
    /// version the commit publishes, for the readers after it
    /// (`lignin_tree::directory`, "A handle is of the pages it was loaded
    /// from").
    pub(crate) directory: Directory,
    /// What each Merk that the transaction dropped or cleared left: the
    /// reclaimer's work (TREE-09), which begins when no pin can read it
    /// (CL6). The same is in the directory's own records
    /// ([`Directory::drops`]).
    pub(crate) dropped: Vec<lignin_tree::directory::Dropped>,
    /// The key set of the commit (CF1).
    pub(crate) keys: super::key::KeySet,
    /// The values that the transaction's loads put into the value log,
    /// with the hold on their files: the commit acquires the pointers that
    /// cells of the tree pages hold, and gives the hold up when they are
    /// counted (`super::load`).
    pub(crate) loaded: Loaded,
}

/// Shows the cell at `at` in the frames, or no node.
fn show_framed<R>(
    frames: &mut Awake<'_, '_>,
    at: Option<CellAddress>,
    read: impl FnOnce(Option<NodeView<'_>>) -> R,
) -> Result<R, TxnError> {
    let Some(address) = at else {
        return Ok(read(None));
    };
    let Frames {
        tree,
        values,
        source,
        hashes,
        ..
    } = frames;
    let stale = hashes.stale(address);
    let shown = tree.shown(address.page(), |page, committed| {
        let node = page.node(address.slot())?;
        let view = NodeView::framed(node, values, source, committed, stale);
        Some(read(Some(view)))
    })?;
    Ok(shown.ok_or(NodeError::NoNode { address })?)
}

/// Shows the committed cell at `at`, or no node.
pub(crate) fn show_committed<R>(
    pages: &mut CommittedPages<'_, '_>,
    at: Option<CellAddress>,
    read: impl FnOnce(Option<NodeView<'_>>) -> R,
) -> Result<R, TxnError> {
    let Some(address) = at else {
        return Ok(read(None));
    };
    let node = pages.pinned(address)?;
    Ok(read(Some(NodeView::committed(&node, pages.values))))
}

macro_rules! reads {
    () => {
        /// Appends the value of the flat record under `key` in `unit` to
        /// `out`, and says whether there is one: the transaction's own
        /// write if it has one that covers the key, and otherwise the
        /// record of the latest committed state (RD3).
        pub fn get(&self, unit: Unit, key: &[u8], out: &mut Vec<u8>) -> Result<bool, TxnError> {
            self.txn().get_in(self.who(), unit, key, out)
        }

        /// The committed read of the flat record under `key` in `unit`
        /// (RD10): the record of the latest committed state, whatever the
        /// transaction has written there. A delete with no cost is charged
        /// from it (`cost-model.md` D2).
        pub fn committed_get(
            &self,
            unit: Unit,
            key: &[u8],
            out: &mut Vec<u8>,
        ) -> Result<bool, TxnError> {
            self.txn().committed_get_in(self.who(), unit, key, out)
        }

        /// Shows `read` the node under `key` in the Merk `subtree`, or
        /// `None` if there is none (RD3, RD5): the transaction's own node
        /// if it wrote one under that key, and otherwise the committed
        /// node.
        ///
        /// This is the read by key. `read` runs inside the operation: it
        /// must not call the transaction.
        pub fn node<R>(
            &self,
            subtree: Subtree,
            key: &[u8],
            read: impl FnOnce(Option<NodeView<'_>>) -> R,
        ) -> Result<R, TxnError> {
            self.txn().node_in(self.who(), subtree, key, None, read)
        }

        /// As [`Self::node`], through `handle` while that is still good
        /// (PN8): the same node, found without a keyed lookup. A handle
        /// that has ended is not followed, and the node is found by `key`.
        ///
        /// A handle is good for the cell it names, and a cell does not say
        /// which Merk it is in. So `handle` is one that was read for
        /// `subtree`: a good handle of another Merk whose node has the same
        /// key would be followed.
        pub fn follow<R>(
            &self,
            subtree: Subtree,
            handle: NodeHandle,
            key: &[u8],
            read: impl FnOnce(Option<NodeView<'_>>) -> R,
        ) -> Result<R, TxnError> {
            self.txn()
                .node_in(self.who(), subtree, key, Some(handle), read)
        }

        /// Shows `read` the child on `side` of the node `parent` names, as
        /// the parent's cell links it, or `None` if no node is there: the
        /// descent by page pointer.
        ///
        /// The outer `None` says that `parent` is no longer good, or names
        /// a node the transaction has written since: the caller then reads
        /// the child by its key ([`Self::node`]). Where the transaction
        /// serves its records by key (WT7b), the child is its own node
        /// under the child's key if it wrote one (RD5).
        pub fn child<R>(
            &self,
            subtree: Subtree,
            parent: NodeHandle,
            side: Side,
            read: impl FnOnce(Option<NodeView<'_>>) -> R,
        ) -> Result<Option<R>, TxnError> {
            self.txn().child_in(self.who(), subtree, parent, side, read)
        }

        /// The committed read of the node under `key` in the Merk
        /// `subtree` (RD10): the node of the latest committed state,
        /// whatever the transaction has written there, or `None` if the
        /// key has no committed node. It shows what a delete with no cost
        /// is charged from: the length of the value, the feature, and the
        /// key length and aggregate of each child, all as committed.
        ///
        /// A handle taken from it is not followed by the other reads.
        pub fn committed_node<R>(
            &self,
            subtree: Subtree,
            key: &[u8],
            read: impl FnOnce(Option<NodeView<'_>>) -> R,
        ) -> Result<R, TxnError> {
            self.txn().committed_node_in(self.who(), subtree, key, read)
        }

        /// Shows `read` what the subtree directory holds of `subtree`
        /// (`lignin_tree::directory`), or `None` for a subtree that is not
        /// entered: the key of the root of its Merk, its tree type and the
        /// element that hosts it. It is the open by prefix: one lookup in
        /// the directory of the transaction's frames, or with no write of
        /// its own in the committed one.
        ///
        /// The transaction's own writes are in it: a subtree it entered,
        /// the root its change sets left a Merk with, and `None` for a
        /// subtree it dropped and for one below a subtree it dropped or
        /// cleared. For a subtree it has not written it is the entry of the
        /// latest committed state (RD3). A transaction that serves its
        /// records by key (WT7b) answers from its write set, and also
        /// looks up the committed entry of each subtree it took out. So
        /// does a scope whose writes are not laid out (`crate::scope`).
        ///
        /// The read claims the data column of `subtree` (SC2, SC3): it is
        /// refused with [`TxnError::ScopeOverlap`] where another open
        /// scope writes that unit, or has dropped or cleared a subtree
        /// above it.
        ///
        /// `read` runs inside the operation: it must not call the
        /// transaction.
        pub fn subtree<R>(
            &self,
            subtree: Subtree,
            read: impl FnOnce(Option<SubtreeView<'_>>) -> R,
        ) -> Result<R, TxnError> {
            self.txn().subtree_in(self.who(), subtree, read)
        }

        /// A cursor over the flat records of `unit`, not positioned (RD6).
        /// Its committed side is the latest committed state of this
        /// moment, which it keeps pinned.
        pub fn cursor(&self, unit: Unit) -> $crate::txn::Cursor<'_, 's> {
            $crate::txn::Cursor::flat(self.txn(), self.who(), unit)
        }

        /// A cursor over the nodes of the Merk `subtree`, in key order, not
        /// positioned (RD6). Its committed side is the Merk as the latest
        /// committed state of this moment has it, which the cursor keeps
        /// pinned, and it reads that under a lease of its own (PN1).
        pub fn merk_cursor(
            &self,
            subtree: Subtree,
        ) -> Result<$crate::txn::MerkCursor<'_, 's>, TxnError> {
            $crate::txn::Cursor::merk(self.txn(), self.who(), subtree)
        }
    };
}

impl<'s> WriteTxn<'s> {
    fn txn(&self) -> &Self {
        self
    }

    reads!();
}

#[path = "../scope/engine.rs"]
mod scopes;

pub use scopes::Scope;
