//! The write transaction and its scope.

use std::{
    cell::Cell,
    fmt,
    marker::PhantomData,
    sync::{Mutex, MutexGuard, PoisonError},
};

use lignin_format::{tree_page::Side, CellAddress, CommitVersion};
use lignin_store::load::LoadBuffers;
use lignin_tree::{
    directory::{Directory, DirectoryError},
    fragment::FragmentError,
    node::{Lease, NodeError, NodeHandle},
    value::{read_value, ValueBuf},
};

use super::{
    check::check,
    commits::Watch,
    error::{BatchError, TxnError},
    frames::{stored_key, Awake, Frames, Parked, Woken},
    key::{Column, Subtree, Unit},
    opened::Opened,
    pages::{find, follow, CommittedPages, NodePages},
    path::WritePath,
    view::NodeView,
    write::{Applied, Batch, SubtreeWrite, ValuePut},
    write_set::{EntryId, Mark, Own, WriteSet},
};
use crate::snapshot::{OperationGuard, VersionPin};

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
    watch: Option<Watch<'s>>,
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
    /// Whether a scope is open.
    scope: bool,
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
            self.watch = None;
        }
    }

    /// Ends every node handle the transaction issued (PN8).
    fn renew_lease(&mut self) {
        self.lease = Lease::take();
        self.lease_at = None;
    }

    /// Ends the transaction's writes: the write set, and everything that
    /// is held for it (WT9).
    fn end(&mut self) {
        self.set.clear();
        self.release();
        self.renew_lease();
    }

    /// Takes out the writes after `mark`, and says whether `mark` is a
    /// place of the write set still.
    fn rewind(&mut self, mark: Mark) -> bool {
        if !self.set.holds(mark) {
            return false;
        }
        if self.set.rewind(mark) {
            // The frames are made again from what is left, over the latest
            // version of that moment (WT8). The base stays pinned until
            // then: what is left may keep committed values.
            self.frames = None;
            self.stuck = None;
            self.renew_lease();
            if self.set.is_empty() {
                self.release();
            }
        }
        true
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
/// - **It holds nothing while it has written nothing** (PN1), and dropping
///   it is a rollback (WT9).
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
                watch: None,
                lease: Lease::take(),
                lease_at: None,
                directory: None,
                stuck: None,
                scope: false,
                buffers: LoadBuffers::new(),
                key: Vec::new(),
                rebuilds: 0,
                followed: 0,
                keyed: 0,
            }),
        }
    }

    /// The write path the transaction was begun from.
    pub fn path(&self) -> &WritePath<'s> {
        &self.path
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

    /// The state, for an operation that is made outside any scope, or
    /// through the one that is open.
    fn enter(&self, scoped: bool) -> Result<MutexGuard<'_, State<'s>>, TxnError> {
        let state = self.lock();
        if state.scope && !scoped {
            return Err(TxnError::ScopeOpen);
        }
        Ok(state)
    }

    /// Opens a scope: the handle a thread writes the transaction through
    /// (SC1). Until parallel scopes exist (TXN-09) a transaction has one at
    /// a time, and a second is refused with [`TxnError::ScopeOpen`].
    pub fn scope(&self) -> Result<Scope<'_, 's>, TxnError> {
        let mut state = self.enter(false)?;
        state.scope = true;
        Ok(Scope {
            txn: self,
            mark: state.set.mark(),
            open: true,
            one_thread: PhantomData,
        })
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
        self.enter(false)?.end();
        Ok(())
    }

    /// The place after the transaction's last write: what a savepoint and a
    /// scope remember (WT8, SC6).
    #[cfg_attr(
        any(not(test), loom),
        expect(dead_code, reason = "savepoints (TXN-04) are its callers")
    )]
    pub(crate) fn mark(&self) -> Mark {
        self.lock().set.mark()
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
        expect(dead_code, reason = "savepoints (TXN-04) are its callers")
    )]
    pub(crate) fn rewind(&self, mark: Mark) -> bool {
        self.lock().rewind(mark)
    }

    /// Takes in the key sets of the commits that were published up to `at`
    /// since the transaction last looked: the entries they meet go into
    /// the overlap set (WT7, CF3).
    fn take_in(state: &mut State<'s>, at: CommitVersion) {
        if let Some(watch) = &mut state.watch {
            let mut commits = Vec::new();
            watch.take(at, &mut commits);
            for (version, keys) in commits {
                state.set.meet(version, &keys);
            }
        }
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
        Self::take_in(state, at);
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
            let subtree = state.set.entry(id).unit.subtree;
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
        if writing && state.watch.is_none() {
            state.watch = Some(path.commits.watch());
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
            match Frames::build(path, &guard, at, &state.set) {
                Ok(frames) => {
                    if !state.set.is_empty() {
                        state.rebuilds += 1;
                    }
                    state.frames = Some(frames);
                    state.base = Some(pin);
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

    fn apply_in(&self, batch: &Batch<'_>) -> Result<Applied, TxnError> {
        check(batch)?;
        // A batch that writes nothing adds nothing (GM0).
        if batch.is_empty() {
            return Ok(Applied::default());
        }
        let mut state = self.enter(true)?;
        let state = &mut *state;
        let applied = (self.begin(state, true)).and_then(|op| self.write(state, &op, batch));
        if state.set.is_empty() {
            // Nothing was added, so nothing is held (PN1).
            state.release();
        }
        applied
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
        // What the transaction wrote below a Merk that the batch drops or
        // clears goes with it: the directory says which, before the batch.
        let cut = match batch.subtrees.iter().any(|write| !write.enters()) {
            true => frames.below(batch, state.set.written())?,
            false => Vec::new(),
        };
        match frames.apply(path, batch, None) {
            Ok(applied) => {
                state.set.add(batch, op.at, frames.empty_before(), &cut);
                Ok(applied)
            }
            Err(refused) => {
                if refused.dirty {
                    // The frames hold a part of the batch. They are made
                    // again from the write set, which does not (WT10).
                    frames.discard();
                    state.renew_lease();
                    if let Ok(frames) = Frames::build(path, &op.guard, op.at, &state.set) {
                        state.frames = Some(frames);
                        state.rebuilds += 1;
                    }
                }
                Err(refused.error)
            }
        }
    }

    /// Adds a batch to a write set that is served by key: no page is
    /// changed, and every value the batch keeps is copied now.
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
        let mut kept = Vec::new();
        let mut empty = Vec::with_capacity(batch.merks.len());
        let mut entry: EntryId = 0;
        for merk in batch.merks {
            let subtree = merk.subtree;
            let left = batch.leaves(subtree);
            empty.push(left || !self.rooted(state, op, &subtree)?);
            for node in merk.nodes {
                entry += 1;
                if node.value != ValuePut::Keep {
                    continue;
                }
                let value = match state.set.own(Unit::data(subtree), node.key) {
                    Own::Node(id) => state.set.node_value(id).map(<[u8]>::to_vec),
                    Own::Deleted | Own::Put(_) => None,
                    Own::Unwritten if left => None,
                    Own::Unwritten => {
                        let seen = self.seen(state, &op.guard, op.at, &subtree)?;
                        let root = seen.flatten();
                        let mut pages =
                            path.committed_pages(&op.guard, op.at, state.lease, &mut state.buffers);
                        match find(&mut pages, root, node.key)? {
                            None => None,
                            Some(address) => Some(Self::read_value(&mut pages, address)?),
                        }
                    }
                };
                kept.push((entry - 1, value.ok_or(BatchError::NoValue { subtree })?));
            }
            entry += merk.removed.len() as EntryId;
        }
        let cut = self.cut(state, op, batch)?;
        if batch.subtrees.iter().any(|write| !write.enters()) {
            // The cells of a Merk that leaves stay in their pages: every
            // handle that was read before ends (PN8).
            state.renew_lease();
            state.lease_at = Some(op.at);
        }
        let first = state.set.add(batch, op.at, &empty, &cut);
        for (offset, value) in kept {
            state.set.keep(first + offset, &value);
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
        scoped: bool,
        unit: Unit,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError> {
        let mut state = self.enter(scoped)?;
        let state = &mut *state;
        let op = self.begin(state, false)?;
        match state.set.own(unit, key) {
            Own::Put(value) => {
                out.extend_from_slice(value);
                Ok(true)
            }
            Own::Deleted => Ok(false),
            Own::Node(_) => Err(TxnError::Damaged("a Merk node is read as a flat record")),
            // No record is read under a subtree that is not entered, or
            // that went with one the transaction dropped.
            Own::Unwritten => match self.seen(state, &op.guard, op.at, &unit.subtree)? {
                Some(_) => self.committed_record(state, &op, unit, key, out),
                None => Ok(false),
            },
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

    fn committed_get_in(
        &self,
        scoped: bool,
        unit: Unit,
        key: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<bool, TxnError> {
        let mut state = self.enter(scoped)?;
        let state = &mut *state;
        let op = self.begin(state, false)?;
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
        match state.set.rooted(subtree) {
            Some(rooted) => Ok(rooted),
            None => {
                let seen = self.seen(state, &op.guard, op.at, subtree)?;
                Ok(seen.flatten().is_some())
            }
        }
    }

    /// Those of the subtrees the transaction has written that are below
    /// one that `batch` drops or clears, where the transaction has no
    /// frames to ask: by the hosts of the subtrees it entered itself, and
    /// by the committed directory from there up.
    fn cut(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        batch: &Batch<'_>,
    ) -> Result<Vec<Subtree>, TxnError> {
        let leaves = (batch.subtrees.iter()).filter(|write| !write.enters());
        let leaving: Vec<Subtree> = leaves.map(SubtreeWrite::subtree).collect();
        if leaving.is_empty() {
            return Ok(Vec::new());
        }
        let State { directory, set, .. } = state;
        let opened = Opened::at(directory, &self.path, &op.guard, op.at)?;
        let mut cut = Vec::new();
        for written in set.written().filter(|written| !leaving.contains(written)) {
            let (mut at, mut below) = (written, false);
            for _ in 0..MAX_DEPTH {
                let Some(Some(host)) = set.host_of(&at) else {
                    break;
                };
                below = leaving.contains(&host);
                if below {
                    break;
                }
                at = host;
            }
            if !below && set.host_of(&at).is_none() {
                below = opened.below(&self.path, &op.guard, &at, &leaving)?;
            }
            if below {
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
        hint: Option<NodeHandle>,
        read: F,
    ) -> Result<Result<R, F>, TxnError>
    where
        F: FnOnce(Option<NodeView<'_>>) -> R,
    {
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
                    let root = frames.root(&subtree)?;
                    find(&mut frames.tree, root, key)?
                }
            };
            return show_framed(&mut frames, at, read).map(Ok);
        }
        let own = state.set.own(Unit::data(subtree), key);
        if own != Own::Unwritten {
            state.keyed += 1;
        }
        match own {
            Own::Node(id) => Ok(Ok(read(Some(NodeView::record(&state.set, id))))),
            Own::Deleted => Ok(Ok(read(None))),
            Own::Put(_) => Err(TxnError::Damaged("a flat record is read as a Merk node")),
            Own::Unwritten => Ok(Err(read)),
        }
    }

    fn node_in<R>(
        &self,
        scoped: bool,
        subtree: Subtree,
        key: &[u8],
        hint: Option<NodeHandle>,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        let mut state = self.enter(scoped)?;
        let state = &mut *state;
        let op = self.begin(state, false)?;
        let read = match self.own_node(state, &op, subtree, key, hint, read)? {
            Ok(shown) => return Ok(shown),
            Err(read) => read,
        };
        // What the transaction dropped hides the Merks below it, also from
        // a handle.
        let root = (self.seen(state, &op.guard, op.at, &subtree)?).flatten();
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
        show_committed(&mut pages, at, read)
    }

    /// Shows `read` the transaction's own node under `key`, or that it
    /// deleted the node, if it has a write that covers the key. With none,
    /// `read` is handed back, and nothing committed is read: a cursor then
    /// shows the node of its own version (RD6).
    pub(crate) fn own_node_in<R, F>(
        &self,
        scoped: bool,
        subtree: Subtree,
        key: &[u8],
        read: F,
    ) -> Result<Result<R, F>, TxnError>
    where
        F: FnOnce(Option<NodeView<'_>>) -> R,
    {
        let mut state = self.enter(scoped)?;
        let state = &mut *state;
        let op = self.begin(state, false)?;
        if state.set.own(Unit::data(subtree), key) == Own::Unwritten {
            return Ok(Err(read));
        }
        self.own_node(state, &op, subtree, key, None, read)
    }

    fn committed_node_in<R>(
        &self,
        scoped: bool,
        subtree: Subtree,
        key: &[u8],
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<R, TxnError> {
        let mut state = self.enter(scoped)?;
        let state = &mut *state;
        let op = self.begin(state, false)?;
        let root = self.committed_root(state, &op.guard, op.at, &subtree)?;
        // The committed node is found by its key, in the committed tree:
        // the transaction may have rebuilt the tree around it (RD10). It is
        // read under a lease of its own, so no handle to it is followed
        // into the transaction's frames.
        let mut pages =
            self.path
                .committed_pages(&op.guard, op.at, Lease::take(), &mut state.buffers);
        let at = find(&mut pages, root, key)?;
        show_committed(&mut pages, at, read)
    }

    fn child_in<R>(
        &self,
        scoped: bool,
        subtree: Subtree,
        parent: NodeHandle,
        side: Side,
        read: impl FnOnce(Option<NodeView<'_>>) -> R,
    ) -> Result<Option<R>, TxnError> {
        let mut state = self.enter(scoped)?;
        let state = &mut *state;
        let op = self.begin(state, false)?;
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
            return show_framed(&mut frames, child, read).map(Some);
        }
        if parent.lease() != state.lease {
            return Ok(None);
        }
        let unit = Unit::data(subtree);
        let mut pages =
            self.path
                .committed_pages(&op.guard, op.at, state.lease, &mut state.buffers);
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
                    Own::Node(id) => Ok(read(Some(NodeView::record(&state.set, id)))),
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
        scoped: bool,
        read: impl FnOnce(&WriteSet) -> R,
    ) -> Result<R, TxnError> {
        let state = self.enter(scoped)?;
        Ok(read(&state.set))
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
    #[cfg_attr(
        any(not(test), loom),
        expect(dead_code, reason = "the commit (TXN-05) is its one caller")
    )]
    pub(crate) fn seal(&self) -> Result<Sealed<'s>, TxnError> {
        let mut state = self.enter(false)?;
        let state = &mut *state;
        if state.set.is_empty() {
            return Ok(Sealed::Empty);
        }
        // The conflict is decided before anything else is read, so that no
        // error of a read the transaction has no more use for stands in
        // its place (CF6).
        {
            let guard = self.path.versions.guard();
            Self::take_in(state, guard.latest());
        }
        if state.set.met() > 0 {
            state.end();
            return Ok(Sealed::Conflict);
        }
        let op = self.begin(state, false)?;
        let Some(frames) = state.frames.take() else {
            if state.set.met() > 0 {
                // A commit was published since: the caller does not hold
                // the commit lock.
                state.end();
                return Ok(Sealed::Conflict);
            }
            // The write set names what the committed state does not hold.
            return Err(
                match Frames::build(&self.path, &op.guard, op.at, &state.set) {
                    Err(error) => error,
                    Ok(_) => TxnError::Damaged("frames that could not be made are made"),
                },
            );
        };
        let keys = state.set.key_set();
        let pin = state.base.take();
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
        })))
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
#[cfg_attr(
    any(not(test), loom),
    expect(dead_code, reason = "the commit (TXN-05) is its one reader")
)]
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
#[cfg_attr(
    any(not(test), loom),
    expect(dead_code, reason = "the commit (TXN-05) is its one reader")
)]
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
    let shown = tree.page(address.page(), |page| {
        let node = page.node(address.slot())?;
        Some(read(Some(NodeView::framed(node, values, source, stale))))
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
    ($scoped:expr) => {
        /// Appends the value of the flat record under `key` in `unit` to
        /// `out`, and says whether there is one: the transaction's own
        /// write if it has one that covers the key, and otherwise the
        /// record of the latest committed state (RD3).
        pub fn get(&self, unit: Unit, key: &[u8], out: &mut Vec<u8>) -> Result<bool, TxnError> {
            self.txn().get_in($scoped, unit, key, out)
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
            self.txn().committed_get_in($scoped, unit, key, out)
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
            self.txn().node_in($scoped, subtree, key, None, read)
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
                .node_in($scoped, subtree, key, Some(handle), read)
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
            self.txn().child_in($scoped, subtree, parent, side, read)
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
            self.txn().committed_node_in($scoped, subtree, key, read)
        }

        /// A cursor over the flat records of `unit`, not positioned (RD6).
        /// Its committed side is the latest committed state of this
        /// moment, which it keeps pinned.
        pub fn cursor(&self, unit: Unit) -> super::Cursor<'_, 's> {
            super::Cursor::flat(self.txn(), $scoped, unit)
        }

        /// A cursor over the nodes of the Merk `subtree`, in key order, not
        /// positioned (RD6). Its committed side is the Merk as the latest
        /// committed state of this moment has it, which the cursor keeps
        /// pinned, and it reads that under a lease of its own (PN1).
        pub fn merk_cursor(&self, subtree: Subtree) -> Result<super::MerkCursor<'_, 's>, TxnError> {
            super::Cursor::merk(self.txn(), $scoped, subtree)
        }
    };
}

impl<'s> WriteTxn<'s> {
    fn txn(&self) -> &Self {
        self
    }

    reads!(false);
}

/// The handle one thread writes a transaction through (`transactions.md`
/// SC1). It reads what the transaction reads, with its own writes (SC4).
///
/// It ends by merging, which makes its writes the transaction's, or by
/// rolling back, which leaves the transaction as if the scope had never
/// been opened (SC6). A scope that is dropped is rolled back.
///
/// A transaction has one scope at a time here. Claims, scopes that run at
/// the same time and their reconciliation are TXN-09's.
pub struct Scope<'t, 's> {
    txn: &'t WriteTxn<'s>,
    mark: Mark,
    open: bool,
    /// A scope is one thread's at a time (SC1): it is sent, not shared.
    one_thread: PhantomData<Cell<()>>,
}

impl<'t, 's> Scope<'t, 's> {
    fn txn(&self) -> &'t WriteTxn<'s> {
        self.txn
    }

    /// The transaction the scope writes.
    pub fn transaction(&self) -> &'t WriteTxn<'s> {
        self.txn
    }

    reads!(true);

    /// Applies `batch` to the transaction as one operation: all of it or
    /// nothing (WT10). Every write of the batch gets the latest version
    /// that this call reads as its first-write version (WT5).
    ///
    /// A batch that is refused with [`TxnError::Batch`], and one that the
    /// tree layer or the subtree directory refuses before a page is
    /// changed, leave everything as it was. After an error that came with
    /// a page changed, the private frames are made again from the write
    /// set, which the batch was never added to, and every node handle read
    /// through the transaction has ended.
    ///
    /// A cursor of the scope may be open: it sees the writes (RD6).
    pub fn apply(&self, batch: &Batch<'_>) -> Result<Applied, TxnError> {
        self.txn.apply_in(batch)
    }

    /// Ends the scope and makes its writes the transaction's (SC6).
    pub fn merge(mut self) {
        self.close();
    }

    /// Ends the scope and takes its writes out of the transaction, with
    /// the first-write versions they introduced and the page ids it
    /// allocated (SC6).
    pub fn rollback(mut self) {
        self.undo();
    }

    fn close(&mut self) {
        if std::mem::take(&mut self.open) {
            self.txn.lock().scope = false;
        }
    }

    fn undo(&mut self) {
        if std::mem::take(&mut self.open) {
            let mut state = self.txn.lock();
            state.rewind(self.mark);
            state.scope = false;
        }
    }
}

impl Drop for Scope<'_, '_> {
    fn drop(&mut self) {
        self.undo();
    }
}

impl fmt::Debug for Scope<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope")
            .field("mark", &self.mark)
            .finish_non_exhaustive()
    }
}
