//! The scope of a write transaction, and what the transaction does for
//! its scopes: claims, laying a scope's writes out, merge and rollback,
//! pending marks, and the conflict rule under a dropped subtree.
//!
//! This file is a part of the transaction's own module (`txn/txn.rs`
//! names it), because it works on the state that the transaction keeps
//! under its lock. [`crate::scope`] is its documentation.

use std::{cell::Cell, collections::HashSet, fmt, marker::PhantomData, sync::Arc};

use lignin_tree::directory::MerkId;

use super::*;
use crate::{
    scope::table::{Lineage, MarkChange, MarkKey, Refused, ScopeId},
    txn::{key::KeySet, Hosted, MerkSide, ParkedCursor, RecordWrite, Resumed},
};

/// The handle one thread writes a transaction through (`transactions.md`
/// SC1). It reads its own writes, then what the scopes before it merged,
/// then the committed state (SC4), and never a write of another scope that
/// is still open.
///
/// - **Claims.** Its first read of a unit takes a read claim and its first
///   write a write claim, which it keeps until it ends (SC3). A call that
///   another open scope's claim stands against is refused at once with
///   [`TxnError::ScopeOverlap`], changes nothing, and leaves the scope
///   usable.
/// - **It ends by merging**, which makes its writes and its pending marks
///   the transaction's, **or by rolling back**, which leaves the
///   transaction as if the scope had never been opened (SC6). A scope that
///   is dropped is rolled back.
///
/// Any number of scopes of one transaction may be open, each on a thread
/// of its own. See [`crate::scope`] for what that costs and for what a
/// scope that was opened beside another does differently.
pub struct Scope<'t, 's> {
    txn: &'t WriteTxn<'s>,
    id: ScopeId,
    open: bool,
    /// A scope is one thread's at a time (SC1): it is sent, not shared.
    one_thread: PhantomData<Cell<()>>,
}

impl<'t, 's> Scope<'t, 's> {
    fn txn(&self) -> &'t WriteTxn<'s> {
        self.txn
    }

    fn who(&self) -> Who {
        Who::scope(self.id)
    }

    /// The transaction the scope writes.
    pub fn transaction(&self) -> &'t WriteTxn<'s> {
        self.txn
    }

    reads!();

    /// Applies `batch` to the transaction as one operation: all of it or
    /// nothing (WT10). Every write of the batch gets the latest version
    /// that this call reads as its first-write version (WT5, SC5).
    ///
    /// The batch takes a write claim on the data column of every Merk it
    /// has a change set for or enters, on the unit of every flat record,
    /// and on every column of a subtree it drops with everything below it
    /// (for a clear: its data column, with everything below). It takes a
    /// read claim on the data column of every Merk that one of its nodes
    /// names as hosted and that it has no change set for. If another
    /// open scope holds one of them the batch is refused with
    /// [`TxnError::ScopeOverlap`] and has no effect (SC3). A claim that was
    /// granted is kept, also where the batch is then refused for what it
    /// says.
    ///
    /// A batch that is refused with [`TxnError::Batch`], and one that the
    /// tree layer or the subtree directory refuses before a page is
    /// changed, leave everything as it was. After an error that came with
    /// a page changed, the private frames are made again from the write
    /// set, which the batch was never added to, and every node handle read
    /// through the transaction has ended.
    ///
    /// A scope that was opened beside another keeps the batch in the write
    /// set and changes no page: it is given no handle of the nodes it
    /// wrote, and what only the pages can refuse is refused when its writes
    /// are laid out ([`Self::lay_out`]).
    ///
    /// A cursor of the scope may be open: it sees the writes (RD6).
    pub fn apply(&self, batch: &Batch<'_>) -> Result<Applied, TxnError> {
        self.txn.apply_scoped(self.id, batch)
    }

    /// A cursor over the nodes of the Merk `subtree`, as
    /// [`Self::merk_cursor`] gives one, for a holder that cannot keep a
    /// borrow of the scope from one call to the next: the handles of a
    /// writer share its scope behind a lock, which they take for each
    /// call.
    ///
    /// The cursor is parked ([`ParkedCursor`]): it is moved and read
    /// through [`Self::resume`] only, so by no one once this scope has
    /// ended.
    pub fn parked_merk_cursor(
        &self,
        subtree: Subtree,
    ) -> Result<ParkedCursor<'t, 's, MerkSide<'s>>, TxnError> {
        let cursor = crate::txn::Cursor::merk(self.txn, self.who(), subtree)?;
        Ok(ParkedCursor::new(cursor))
    }

    /// The cursor `parked` holds, for calls through this scope, which
    /// opened it. Each call is a read of the scope like any other: it
    /// reads the scope's writes as they are then (RD6), and it is refused
    /// with [`TxnError::ScopeOverlap`] where another open scope holds the
    /// Merk for writing (SC3).
    ///
    /// `None` for a cursor that another scope opened, which stays as it
    /// was. No scope has the number of one that has ended, so a parked
    /// cursor is resumed by no one once its scope has merged or was rolled
    /// back.
    pub fn resume<'a, C>(
        &'a self,
        parked: &'a mut ParkedCursor<'t, 's, C>,
    ) -> Option<Resumed<'a, 't, 's, C>> {
        parked.resume(self.txn, self.who())
    }

    /// Whether the scope's writes are laid out on the transaction's pages
    /// as it makes them: it was opened while no other scope was open, or
    /// [`Self::lay_out`] has laid them out.
    pub fn is_laid_out(&self) -> bool {
        self.txn.lock().scopes.laid(self.id)
    }

    /// Lays the writes of a scope that was opened beside another out on
    /// the transaction's pages, each applied by key through the tree layer
    /// as the transaction's pages are now (`transactions.md` LT4), and has
    /// the scope's later writes laid out as they are made.
    ///
    /// If a write is refused, the scope is as it was and usable: its
    /// writes are in the write set, not laid out, and the caller rolls it
    /// back or merges it all the same. A merge lays out what is not, and a
    /// scope whose writes cannot be laid out leaves a transaction that
    /// cannot be either, until a rollback takes them out.
    pub fn lay_out(&self) -> Result<(), TxnError> {
        let mut state = self.txn.lock();
        self.txn.lay(&mut state, self.id)
    }

    /// Sets the pending mark on the element under `key` of `subtree`
    /// (`transactions.md` SC9): a write of this scope stopped below that
    /// element, and the pointers it holds to the roots it hosts are not
    /// followed until the mark is taken off (`tree-layout.md` T3). The
    /// transaction is not committed while a mark stands (CM1).
    ///
    /// A mark is not a claim, and the element's Merk is not read for the
    /// caller. It goes with the scope if the scope is rolled back.
    pub fn mark_pending(&self, subtree: Subtree, key: &[u8]) -> Result<(), TxnError> {
        (self.txn).set_pending(self.id, true, (subtree, key.into()))
    }

    /// Takes the pending mark off the element under `key` of `subtree`:
    /// the write that names the element's roots again has been made
    /// (finalize, GROVE-11). The mark may be one that a merged scope set.
    pub fn clear_pending(&self, subtree: Subtree, key: &[u8]) -> Result<(), TxnError> {
        (self.txn).set_pending(self.id, false, (subtree, key.into()))
    }

    /// Whether the element under `key` of `subtree` is marked pending as
    /// this scope is shown it: by the marks it has set and taken off, and
    /// otherwise by those the merged scopes left (SC9). A mark of another
    /// scope that is still open is not shown.
    pub fn is_pending(&self, subtree: Subtree, key: &[u8]) -> bool {
        let state = self.txn.lock();
        state.scopes.is_pending(self.who(), &(subtree, key.into()))
    }

    /// Ends the scope and makes its writes and its pending marks the
    /// transaction's (SC6). It is one step for every other scope.
    ///
    /// Writes that are not laid out yet are laid out now
    /// ([`Self::lay_out`]). Where that is refused they are the
    /// transaction's all the same: its write set then cannot be laid out,
    /// it serves its records by key, and its commit is refused with what
    /// refused them until a rollback takes them out.
    pub fn merge(mut self) {
        if std::mem::take(&mut self.open) {
            self.txn.merge_scope(self.id);
        }
    }

    /// Ends the scope and takes its writes out of the transaction, with
    /// the first-write versions they introduced, the pending marks it set
    /// or took off and the page ids it allocated (SC6). The transaction is
    /// as if the scope had never been opened.
    pub fn rollback(mut self) {
        self.undo();
    }

    fn undo(&mut self) {
        if std::mem::take(&mut self.open) {
            self.txn.rollback_scope(self.id);
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
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// The subtrees that `batch` enters, each with the subtree of its host and
/// whether the subtree can be there already: the batch enters it if it is
/// absent, and does not drop it first.
fn entering(batch: &Batch<'_>) -> Vec<(Subtree, Option<Subtree>, bool)> {
    let dropped = |subtree: Subtree| {
        let mut writes = batch.subtrees.iter();
        writes.any(|write| matches!(write, SubtreeWrite::Drop { .. }) && write.subtree() == subtree)
    };
    let enters = batch.subtrees.iter().filter_map(SubtreeWrite::entering);
    enters
        .map(|enter| {
            let host = enter.host.map(|host| host.subtree);
            let kept = enter.if_absent && !dropped(enter.subtree);
            (enter.subtree, host, kept)
        })
        .collect()
}

/// Which subtree is below which for a transaction, asked in two ways.
///
/// **Where a subtree hangs once the batch that asks is applied**
/// ([`Self::host`]): by the hosts of the subtrees that the batch enters,
/// then of those the transaction entered, and from there up by the
/// subtree directory of its frames, or of the committed version it reads.
/// A subtree that is entered if it is absent hangs where that enter
/// states only where the enter makes it. One that the transaction has
/// already, its own or the committed one, hangs where that one does: as
/// its frames have it where they show every write of the transaction, and
/// otherwise as its write set says over the committed directory
/// ([`Standing`]).
///
/// **Whether a subtree is below another for the claims of scopes**
/// ([`Lineage::below`], SC3): by every place the subtree hangs, or hung
/// while a scope that is open could hold a claim on it. That is where it
/// hung before the first operation of a scope that is open, by the
/// operations before that one over the committed directory
/// ([`ByKey::standing`]), where an enter of an operation from that one
/// on states it, and where the batch that asks states it. A batch can
/// take a subtree out with one above it and enter it again elsewhere,
/// and a claim of another scope is on the subtree as it was before. So
/// between those operations a subtree is taken to be below every subtree
/// it is or was below, which refuses a batch that the claims would allow
/// where a scope that is open moved a subtree, and allows none that they
/// would refuse. Where an operation of the transaction itself moved a
/// subtree before them, the subtree hangs where that operation left it.
struct Hosts<'a, 's> {
    path: &'a WritePath<'s>,
    /// The subtrees the batch enters ([`entering`]).
    entering: &'a [(Subtree, Option<Subtree>, bool)],
    /// The subtrees the batch drops and clears, before it enters any.
    leaving: &'a [Subtree],
    /// The enters if absent that the transaction made (`State::above`).
    noted: &'a [Above],
    /// Whether every operation of the write set is laid out, once that
    /// was asked: the frames then show every write of the transaction.
    laid: Option<bool>,
    set: &'a WriteSet,
    frames: &'a mut Option<Parked<'s>>,
    directory: &'a mut Option<Opened>,
    guard: &'a OperationGuard<'s>,
    at: CommitVersion,
}

impl Hosts<'_, '_> {
    /// The subtree of the element that hosts `subtree`.
    fn host(&mut self, subtree: &Subtree) -> Result<Option<Subtree>, TxnError> {
        let mut entering = self.entering.iter().rev();
        let stated = match entering.find(|(entered, ..)| entered == subtree) {
            Some(&(_, host, false)) => return Ok(host),
            stated => stated.map(|&(_, host, _)| host),
        };
        if stated.is_some() || self.noted.iter().any(|noted| noted.subtree == *subtree) {
            if let Some(host) = self.kept(subtree, stated)? {
                return Ok(host);
            }
        } else if let Some(host) = self.set.host_of(subtree) {
            // No enter if it is absent names the subtree, so an enter of
            // the write set is of a subtree that is new.
            return Ok(host);
        }
        if let Some(frames) = Woken::wake(&mut *self.frames, self.guard) {
            // An entry that a drop left is asked as well: the drop can be
            // the write of a scope that is still open.
            let entry = (frames.directory).stored(&*frames.flat, subtree.as_bytes())?;
            let host = entry.as_ref().and_then(|entry| entry.host());
            return Ok(host.map(|host| Subtree::new(*host.parent)));
        }
        let opened = Opened::at(self.directory, self.path, self.guard, self.at)?;
        opened.host(self.path, self.guard, subtree)
    }

    /// The subtree of the element that hosts `subtree`, of which an enter
    /// if it is absent is stated: by the batch that asks, with the host
    /// `stated`, or by the write set. `None` is for a subtree that the
    /// directory is asked for: the committed one, and one that the
    /// transaction's frames have, its own as well.
    fn kept(
        &mut self,
        subtree: &Subtree,
        stated: Option<Option<Subtree>>,
    ) -> Result<Option<Option<Subtree>>, TxnError> {
        let set = self.set;
        let laid = *(self.laid).get_or_insert_with(|| (0..set.ops()).all(|op| set.laid(op)));
        if laid && let Some(frames) = Woken::wake(&mut *self.frames, self.guard) {
            // The frames show every write of the transaction: a subtree
            // they have is kept, unless the batch that asks takes it with
            // one above it.
            let Some(entry) = (frames.directory).open(&*frames.flat, subtree.as_bytes())? else {
                return Ok(Some(
                    stated.unwrap_or_else(|| set.host_of(subtree).flatten()),
                ));
            };
            let mut taken = false;
            for left in self.leaving {
                // A Merk with no node hosts none.
                let above = frames.directory.open(&*frames.flat, left.as_bytes())?;
                let above = above.filter(|above| above.root().is_some());
                taken |= above.is_some_and(|above| entry.lineage().any(|id| id == above.id()));
            }
            return Ok(stated.filter(|_| taken));
        }
        let ops = set.ops();
        let mut by_key = ByKey::new(self.path, self.guard, self.at, set, self.directory)?;
        Ok(match by_key.standing(*subtree, ops)? {
            // Its own, where the enter that made it says. The batch that
            // asks makes it again where a subtree above it left since.
            Standing::Own(entered) => {
                if let Some(host) = stated {
                    let left = [set.left_after(entered).as_slice(), self.leaving].concat();
                    if !left.is_empty() && by_key.below(*subtree, &left, Some((entered, ops)))? {
                        return Ok(Some(host));
                    }
                }
                Some(set.host_in(subtree, entered))
            }
            // It left: the batch that asks makes it again. With no such
            // batch it hangs where its last enter said, for the scope
            // that took it.
            Standing::Gone => Some(stated.unwrap_or_else(|| set.host_of(subtree).flatten())),
            // The committed one, where the committed version has it and
            // the transaction took no subtree above it. Where it does
            // not, the batch that asks makes it.
            Standing::Committed => {
                let opened = by_key.opened;
                let entry = opened.seen_entry(self.path, self.guard, set, subtree)?;
                let there = entry.is_some()
                    && !opened.below(self.path, self.guard, subtree, self.leaving)?;
                stated.filter(|_| !there)
            }
        })
    }
}

impl Lineage for Hosts<'_, '_> {
    type Error = TxnError;

    fn below(&mut self, subtree: &Subtree, above: &Subtree) -> Result<bool, TxnError> {
        let set = self.set;
        let since = set.first_open();
        let mut by_key = ByKey::new(self.path, self.guard, self.at, set, self.directory)?;
        let mut seen = HashSet::from([*subtree]);
        let mut reached = vec![*subtree];
        while let Some(at) = reached.pop() {
            // Every place the subtree hangs or hung since `since`: where
            // the batch that asks states it, where an enter from then on
            // states it, and where it hung before.
            let stated = self.entering.iter().filter(|(entered, ..)| *entered == at);
            let mut hosts: Vec<Subtree> = stated.filter_map(|(_, host, _)| *host).collect();
            hosts.extend(set.hosts_stated_from(&at, since));
            match by_key.standing(at, since)? {
                Standing::Gone => {}
                Standing::Own(entered) => hosts.extend(set.host_in(&at, entered)),
                Standing::Committed => {
                    hosts.extend(by_key.opened.host(self.path, self.guard, &at)?);
                }
            }
            for host in hosts {
                if host == *above {
                    return Ok(true);
                }
                if seen.insert(host) {
                    reached.push(host);
                }
            }
        }
        Ok(false)
    }
}

fn overlap(refused: Refused<TxnError>) -> TxnError {
    match refused {
        Refused::Overlap(unit) => TxnError::ScopeOverlap { unit },
        Refused::Lineage(error) => error,
    }
}

/// The cell of the element a pending mark is on, as `frames` have it.
fn marked_cell(
    frames: &mut Awake<'_, '_>,
    (subtree, key): &MarkKey,
) -> Result<Option<CellAddress>, TxnError> {
    let root = frames.root(subtree)?;
    find(&mut frames.tree, root, key)
}

/// The cells of hosts that are behind in `frames` and that `batch`
/// deletes, where `marked` says which elements may be marked. Once the
/// batch is applied nothing is behind there, whichever node takes the
/// cell.
fn behind_deleted(
    frames: &mut Awake<'_, '_>,
    batch: &Batch<'_>,
    marked: impl Fn(&MarkKey) -> bool,
) -> Result<Vec<CellAddress>, TxnError> {
    let mut deleted = Vec::new();
    if frames.tree.behind.is_empty() {
        return Ok(deleted);
    }
    for merk in batch.merks {
        let removed = merk.removed.iter();
        for key in removed.filter(|key| marked(&(merk.subtree, (**key).into()))) {
            let root = frames.root(&merk.subtree)?;
            let cell = find(&mut frames.tree, root, key)?;
            deleted.extend(cell.filter(|cell| frames.tree.behind.contains(cell)));
        }
    }
    Ok(deleted)
}

/// What the write `applied` did to the hosts that are behind in `frames`:
/// those it deleted are behind no more, and those it carried are behind
/// where they are now.
fn move_behind(frames: &mut Awake<'_, '_>, deleted: &[CellAddress], applied: &Applied) {
    let behind = &mut frames.tree.behind;
    for cell in deleted {
        behind.remove(cell);
    }
    let carried = applied.carried.iter();
    let moved: Vec<_> = carried.filter(|(from, _)| behind.contains(from)).collect();
    for (from, _) in &moved {
        behind.remove(from);
    }
    behind.extend(moved.iter().map(|(_, to)| to.address()));
}

/// Has the element that `mark` is on be behind in `frames`, or behind no
/// more, where they have its node.
fn set_behind(frames: &mut Awake<'_, '_>, set: bool, mark: &MarkKey) -> Result<(), TxnError> {
    if let Some(cell) = marked_cell(frames, mark)? {
        match set {
            true => frames.tree.behind.insert(cell),
            false => frames.tree.behind.remove(&cell),
        };
    }
    Ok(())
}

impl<'s> WriteTxn<'s> {
    pub(super) fn who(&self) -> Who {
        Who::OUTSIDE
    }

    /// Opens a scope: the handle a thread writes the transaction through
    /// (`transactions.md` SC1). Any number may be open.
    ///
    /// A scope that is opened while no other is open has its writes laid
    /// out on the transaction's pages as it makes them. One that is opened
    /// beside another keeps them in the write set until they are laid out,
    /// at its merge or before ([`Scope::lay_out`]).
    pub fn scope(&self) -> Result<Scope<'_, 's>, TxnError> {
        let id = self.lock().scopes.open();
        Ok(Scope {
            txn: self,
            id,
            open: true,
            one_thread: PhantomData,
        })
    }

    /// Whether a scope of the transaction is open. A savepoint, a rollback
    /// of the transaction and its commit need every scope closed (SC8).
    pub fn scopes_open(&self) -> bool {
        self.lock().scopes.any_open()
    }

    /// Whether the element under `key` of `subtree` is marked pending by a
    /// scope that has merged (`transactions.md` SC9). A mark of a scope
    /// that is still open is not shown.
    pub fn is_pending(&self, subtree: Subtree, key: &[u8]) -> bool {
        let state = self.lock();
        (state.scopes).is_pending(Who::OUTSIDE, &(subtree, key.into()))
    }

    /// The cells of the hosts that are behind in the transaction's frames,
    /// in order, each as the pages answer the tree layer for it
    /// ([`Pages::host_is_behind`](lignin_tree::fragment::Pages::host_is_behind)),
    /// and what they answer for `other`. None without frames.
    #[cfg(test)]
    pub(crate) fn behind(&self, other: CellAddress) -> Option<(Vec<CellAddress>, bool)> {
        use lignin_tree::fragment::Pages;
        let mut state = self.lock();
        let guard = self.path.versions.guard();
        let mut frames = Woken::wake(&mut state.frames, &guard)?;
        let mut cells: Vec<CellAddress> = frames.tree.behind.iter().copied().collect();
        cells.sort_unstable();
        cells.retain(|cell| frames.tree.host_is_behind(*cell));
        let other = frames.tree.host_is_behind(other);
        Some((cells, other))
    }

    /// Takes the read claim of `who` on `unit` (SC3), or checks it for a
    /// caller outside any scope.
    pub(super) fn claim_read(
        &self,
        state: &mut State<'s>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
        who: Who,
        unit: Unit,
    ) -> Result<(), TxnError> {
        if !state.scopes.any_open() {
            return Ok(());
        }
        let mut hosts = Hosts {
            path: &self.path,
            entering: &[],
            leaving: &[],
            noted: &state.above,
            laid: None,
            set: &state.set,
            frames: &mut state.frames,
            directory: &mut state.directory,
            guard,
            at,
        };
        (state.scopes.read(who, unit, &mut hosts)).map_err(overlap)
    }

    /// As [`Self::claim_read`], for a call that begins no operation.
    pub(super) fn claim_read_now(
        &self,
        state: &mut State<'s>,
        who: Who,
        unit: Unit,
    ) -> Result<(), TxnError> {
        if !state.scopes.any_open() {
            return Ok(());
        }
        let guard = self.path.versions.guard();
        let at = guard.latest();
        self.claim_read(state, &guard, at, who, unit)
    }

    /// Takes the claims of `batch` for the scope `id` (SC3): all of them,
    /// or none.
    ///
    /// A node that names a Merk it hosts has its cell point at that Merk's
    /// root, which is read from the Merk's entry in the subtree directory
    /// where the batch has no change set for the Merk: the batch reads the
    /// Merk's unit (SC2). What is below what is asked with the subtrees the
    /// batch enters under their hosts.
    ///
    /// An enter claims the unit of its subtree as a write, of either kind:
    /// one that is stated if the subtree is absent is judged again each
    /// time the write is laid out, and can make the entry then where it
    /// found one before. So of two scopes that are open together only one
    /// states an enter of a subtree, also of one that is entered.
    pub(super) fn claim_batch(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        id: ScopeId,
        batch: &Batch<'_>,
    ) -> Result<(), TxnError> {
        let mut units: Vec<Unit> = (batch.merks.iter())
            .map(|merk| Unit::data(merk.subtree))
            .collect();
        units.extend(batch.records.iter().map(RecordWrite::unit));
        let leaving = batch.leaving();
        let mut whole = leaving.clone();
        let entering = entering(batch);
        for write in batch.subtrees {
            // The entry of a Merk in the subtree directory is of that
            // Merk's unit (SC2).
            let subtree = write.subtree();
            let every = |subtree| Column::ALL.map(|column| Unit::new(column, subtree));
            match write {
                SubtreeWrite::Enter { .. } | SubtreeWrite::EnterIfAbsent { .. } => {
                    units.push(Unit::data(subtree));
                }
                SubtreeWrite::Clear { .. } => units.push(Unit::data(subtree)),
                SubtreeWrite::Drop { .. } => units.extend(every(subtree)),
                SubtreeWrite::Convert { .. } => units.push(Unit::data(subtree)),
                SubtreeWrite::Move { .. } => {}
            }
            // A move claims every prefix it names, on either side, whole
            // and in every column, with what is below each.
            for named in write.moving().iter().flat_map(|moving| moving.named()) {
                units.extend(every(named));
                whole.push(named);
            }
        }
        let nodes = batch.merks.iter().flat_map(|merk| merk.nodes);
        let mut reads: Vec<Unit> = nodes
            .filter_map(|node| match node.hosted {
                Hosted::Set(hosted) => Some(hosted),
                Hosted::Keep => None,
            })
            .flatten()
            .map(|hosted| Unit::data(*hosted))
            .filter(|unit| !units.contains(unit))
            .collect();
        reads.sort_unstable();
        reads.dedup();
        let mut hosts = Hosts {
            path: &self.path,
            entering: &entering,
            leaving: &leaving,
            noted: &state.above,
            laid: None,
            set: &state.set,
            frames: &mut state.frames,
            directory: &mut state.directory,
            guard: &op.guard,
            at: op.at,
        };
        (state.scopes.write(id, &units, &reads, &whole, &mut hosts)).map_err(overlap)
    }

    /// Frames that show, applied in order to the committed state of the
    /// version `at`, every operation of `set` that is laid out (WT3): each
    /// is applied again through the tree layer, which finds every cell by
    /// its key (WT7, LT4). The operations of a scope that keeps its writes
    /// in the write set are passed over.
    ///
    /// The cells of the elements that are marked pending are found again
    /// at the end. While the operations are applied, every host whose key
    /// is that of an element that was marked at some point is taken to be
    /// behind.
    pub(super) fn build<'g>(
        path: &WritePath<'s>,
        guard: &'g OperationGuard<'s>,
        at: CommitVersion,
        set: &WriteSet,
        scopes: &Scopes,
        loaded: &mut Loaded,
    ) -> Result<Parked<'s>, TxnError> {
        // No cell of the frames that are made here holds a value that a
        // load appended for the frames before.
        loaded.begin_layout();
        let mut frames = Frames::new(path, guard, at)?;
        frames.tree.behind_keys = (scopes.ever_marked().into_iter())
            .map(|(_, key)| key)
            .collect();
        for op in (0..set.ops()).filter(|op| set.laid(*op)) {
            set.with_batch(op, |batch, again| {
                frames.apply(path, batch, Some(again), loaded)
            })
            .map_err(|refused| refused.error)?;
        }
        frames.tree.behind_keys.clear();
        for mark in scopes.laid_marks() {
            let cell = marked_cell(&mut frames, &mark)?;
            frames.tree.behind.extend(cell);
        }
        Ok(frames.park())
    }

    pub(super) fn apply_scoped(&self, id: ScopeId, batch: &Batch<'_>) -> Result<Applied, TxnError> {
        check(batch)?;
        // A batch that writes nothing adds nothing (GM0), and claims
        // nothing.
        if batch.is_empty() {
            return Ok(Applied::default());
        }
        let mut state = self.lock();
        let state = &mut *state;
        let applied = (self.begin(state, true)).and_then(|op| {
            let laid = state.scopes.laid(id);
            // What a move decides is not written by key, and a move is
            // laid out on the frames when it is stated: asked first, so
            // that a batch which is refused for it has claimed nothing.
            self.guard_batch(state, &op, Who::scope(id), batch)?;
            for moving in batch.moves() {
                // And a move knows nothing of the marks of pending
                // elements.
                let subtree = moving.from;
                let marked = state.scopes.ever_marked();
                if (moving.named()).any(|named| marked.iter().any(|(at, _)| *at == named)) {
                    return Err(BatchError::MoveBeside { subtree }.into());
                }
            }
            self.claim_batch(state, &op, id, batch)?;
            self.note_under(state, &op, batch)?;
            let above = self.above(state, &op, batch)?;
            let applied = match laid {
                true => {
                    let scopes = &state.scopes;
                    let deleted = match Woken::wake(&mut state.frames, &op.guard) {
                        Some(mut frames) => {
                            behind_deleted(&mut frames, batch, |key| scopes.marked_on_pages(key))?
                        }
                        None => Vec::new(),
                    };
                    let applied = self.write(state, &op, batch)?;
                    if let Some(mut frames) = Woken::wake(&mut state.frames, &op.guard) {
                        move_behind(&mut frames, &deleted, &applied);
                    }
                    applied
                }
                false => {
                    state.set.admits(batch)?;
                    self.write_by_key(state, &op, batch)?
                }
            };
            state.set.tag_last(id, laid);
            let serial = state.set.last_serial();
            let above = above.into_iter().map(|(subtree, names)| Above {
                serial,
                subtree,
                names,
            });
            state.above.extend(above);
            Ok(applied)
        });
        if state.set.is_empty() {
            // Nothing was added, so nothing is held (PN1).
            state.release();
        }
        applied
    }

    /// The value of the node under `key` of the Merk `subtree` as the
    /// frames have it, for a write that keeps it and is kept in the write
    /// set: the node is one of an operation that is laid out, which did not
    /// state the value either. It is a cell of its tree, or the unlinked
    /// record that operation left of it.
    pub(super) fn framed_value(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        subtree: &Subtree,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, TxnError> {
        let Some(mut frames) = Woken::wake(&mut state.frames, &op.guard) else {
            return Ok(None);
        };
        let root = frames.root(subtree)?;
        let at = find(&mut frames.tree, root, key)?;
        if at.is_none() {
            // The node left its tree in an operation that is laid out:
            // the frames have it as an unlinked record.
            return frames.own_unlinked_value(&self.path, subtree, key);
        }
        show_framed(&mut frames, at, |view| {
            let mut buf = ValueBuf::new();
            view.map(|view| view.value(&mut buf).map(<[u8]>::to_vec))
                .transpose()
        })?
    }

    /// Shows `read` the node under `key` as a scope reads it whose writes
    /// are kept in the write set: its own node, or that it deleted the
    /// node, where its newest write of the key is not laid out, and no
    /// node where the Merk is below one that such a write drops or clears
    /// (CV9). For every other key `read` is handed back: the node is then
    /// the one the transaction's pages show.
    pub(super) fn unlaid_node<R, F>(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        who: Who,
        subtree: Subtree,
        key: &[u8],
        read: F,
    ) -> Result<Result<R, F>, TxnError>
    where
        F: FnOnce(Option<NodeView<'_>>) -> R,
    {
        if who.id().is_none_or(|id| state.scopes.laid(id)) {
            return Ok(Err(read));
        }
        let unit = Unit::data(subtree);
        if (state.set.newest(unit, key)).is_some_and(|newest| state.set.unlaid(newest)) {
            state.keyed += 1;
            return match state.set.own(unit, key) {
                Own::Node(id) => {
                    let record = NodeView::record(&state.set, id, self.path.values);
                    Ok(Ok(read(Some(record))))
                }
                Own::Put(_) => Err(TxnError::Damaged("a flat record is read as a Merk node")),
                Own::Deleted | Own::Unwritten => Ok(Ok(read(None))),
            };
        }
        match self.below_unlaid(state, op, &subtree)? {
            true => Ok(Ok(read(None))),
            false => Ok(Err(read)),
        }
    }

    /// Whether `subtree` is, in the frames, below a Merk that a write
    /// which is not laid out drops or clears: the frames still show what
    /// is there, and the scope that made the write reads none of it. With
    /// no frames the records are read by key, where every drop of the
    /// write set is asked ([`Opened::seen`]).
    pub(super) fn below_unlaid(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        subtree: &Subtree,
    ) -> Result<bool, TxnError> {
        let leaving = state.set.unlaid_leaving();
        if leaving.is_empty() {
            return Ok(false);
        }
        let Some(frames) = Woken::wake(&mut state.frames, &op.guard) else {
            return Ok(false);
        };
        let Some(entry) = frames.directory.open(&*frames.flat, subtree.as_bytes())? else {
            return Ok(false);
        };
        for left in leaving {
            let above = frames.directory.open(&*frames.flat, left.as_bytes())?;
            // A Merk with no node hosts none.
            let above = above.filter(|above| above.root().is_some());
            if above.is_some_and(|above| entry.lineage().any(|id| id == above.id())) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether a scope whose writes are kept in the write set reads the
    /// nodes of `unit` by their keys: it has written the Merk, or dropped
    /// or cleared a subtree, so a cell of the transaction's pages need not
    /// be the node it names.
    pub(super) fn reads_by_key(state: &State<'s>, who: Who, unit: Unit) -> bool {
        who.id().is_some_and(|id| {
            !state.scopes.laid(id)
                && (state.scopes.writes(id, unit) || !state.set.unlaid_leaving().is_empty())
        })
    }

    /// Lays the writes of the scope `id` out on the frames, if they are
    /// not: each of its operations is applied by key, in order, to the
    /// pages as the scopes before it left them (LT4). With no frames
    /// nothing is applied: they are made from the write set, which has the
    /// operations.
    pub(super) fn lay(&self, state: &mut State<'s>, id: ScopeId) -> Result<(), TxnError> {
        if state.scopes.laid(id) {
            return Ok(());
        }
        let ops = state.set.ops_of(id);
        if !ops.is_empty() || !state.scopes.marks_of(id).is_empty() {
            let op = self.begin(state, false)?;
            let laid = match Woken::wake(&mut state.frames, &op.guard) {
                None => Ok(()),
                Some(mut frames) => {
                    let marks = state.scopes.marks_of(id);
                    let (set, loaded) = (&state.set, &mut state.loaded);
                    let laid = Self::lay_on(&self.path, &mut frames, set, &ops, marks, loaded);
                    if matches!(laid, Err((_, true))) {
                        // The frames hold a part of the scope's writes.
                        // They are made again from the operations that are
                        // laid out, which these are not.
                        frames.discard();
                    }
                    laid
                }
            };
            if let Err((error, dirty)) = laid {
                if dirty {
                    state.renew_lease();
                }
                return Err(error);
            }
        }
        state.set.lay_scope(id);
        state.scopes.set_laid(id);
        Ok(())
    }

    /// Applies the operations `ops` of `set` to `frames`, with the changes
    /// `marks` of pending marks that their scope made. On an error, says
    /// whether the frames may hold a part of them.
    pub(super) fn lay_on(
        path: &WritePath<'s>,
        frames: &mut Awake<'s, '_>,
        set: &WriteSet,
        ops: &[usize],
        marks: &[MarkChange],
        loaded: &mut Loaded,
    ) -> Result<(), (TxnError, bool)> {
        let mut dirty = false;
        // When each mark was set among the operations is not kept: a host
        // that the scope marked at some point is behind through all of
        // them, which keeps its pointers as they are.
        for (_, mark) in marks.iter().filter(|(set, _)| *set) {
            let cell = marked_cell(frames, mark).map_err(|error| (error, dirty))?;
            dirty |= cell.is_some_and(|cell| frames.tree.behind.insert(cell));
        }
        for op in ops {
            set.with_batch(*op, |batch, again| {
                // A host that is behind here may be marked: every mark of
                // the scope is of a host that was found above.
                let deleted =
                    behind_deleted(frames, batch, |_| true).map_err(|error| (error, dirty))?;
                let applied = (frames.apply(path, batch, Some(again), loaded))
                    .map_err(|refused| (refused.error, dirty || refused.dirty))?;
                move_behind(frames, &deleted, &applied);
                Ok(())
            })?;
            dirty = true;
        }
        for (set, mark) in marks {
            set_behind(frames, *set, mark).map_err(|error| (error, true))?;
        }
        Ok(())
    }

    pub(super) fn merge_scope(&self, id: ScopeId) {
        let mut state = self.lock();
        let state = &mut *state;
        if self.lay(state, id).is_err() {
            // The writes are the transaction's all the same (SC6). Its
            // frames do not show them: they are made again from the write
            // set, and where that is refused the transaction serves its
            // records by key and is not committed.
            state.frames = None;
            state.renew_lease();
        }
        let closed = state.scopes.close(id).expect("the scope is open");
        state.set.merge_scope(id);
        state.scopes.merge_marks(closed.marks);
        if state.set.is_empty() {
            state.release();
        }
    }

    pub(super) fn rollback_scope(&self, id: ScopeId) {
        let mut state = self.lock();
        let state = &mut *state;
        let Some(closed) = state.scopes.close(id) else {
            return;
        };
        let removed = state.set.remove_scope(id);
        if removed {
            state.stuck = None;
            state.forget_above();
            // A look may have seen what the scope wrote.
            state.looks.clear();
        }
        if closed.laid && (removed || !closed.marks.is_empty()) {
            // The frames show the scope's writes: they are made again from
            // what is left. The base stays pinned until then.
            state.frames = None;
            state.renew_lease();
        }
        if state.set.is_empty() {
            state.release();
        }
    }

    pub(super) fn set_pending(
        &self,
        id: ScopeId,
        set: bool,
        mark: MarkKey,
    ) -> Result<(), TxnError> {
        let mut state = self.lock();
        let state = &mut *state;
        if state.scopes.laid(id) && state.frames.is_some() {
            // The frames show this scope's writes: the cell is behind, or
            // behind no more, from here on.
            let op = self.begin(state, false)?;
            if let Some(mut frames) = Woken::wake(&mut state.frames, &op.guard) {
                set_behind(&mut frames, set, &mark)?;
            }
        }
        state.scopes.mark(id, set, mark);
        Ok(())
    }

    /// Notes, for every subtree that `batch` writes under and the
    /// transaction has not written under before, which Merks it is below
    /// in the committed version of this operation
    /// ([`Self::meet_below`]).
    pub(super) fn note_under(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        batch: &Batch<'_>,
    ) -> Result<(), TxnError> {
        let merks = batch.merks.iter().map(|merk| merk.subtree);
        let records = batch.records.iter().map(|record| record.unit().subtree);
        // A move is held against what a commit takes above the subtree
        // it moves, as a drop is.
        let whole =
            (batch.subtrees.iter()).filter(|write| write.leaves() || write.moving().is_some());
        for subtree in merks.chain(records).chain(whole.map(SubtreeWrite::subtree)) {
            if !state.under.contains_key(&subtree) {
                let opened = Opened::at(&mut state.directory, &self.path, &op.guard, op.at)?;
                let above = opened.lineage(&self.path, &op.guard, &subtree)?;
                state.under.insert(subtree, above);
            }
        }
        Ok(())
    }

    /// For each subtree that `batch` enters if it is absent, the subtrees
    /// that the element which is stated as its host is in or below, by
    /// their names and the nearest first, as the transaction has them
    /// with the batch ([`Hosts`]). It is what [`Self::meet_below`] holds
    /// the commits of others against, and is noted once the batch is
    /// taken.
    fn above(
        &self,
        state: &mut State<'s>,
        op: &Op<'s>,
        batch: &Batch<'_>,
    ) -> Result<Vec<(Subtree, Vec<Subtree>)>, TxnError> {
        let enters = batch.subtrees.iter().filter_map(SubtreeWrite::entering);
        let hosted: Vec<_> = enters
            .filter(|enter| enter.if_absent)
            .map(|enter| (enter.subtree, enter.host.map(|host| host.subtree)))
            .collect();
        if hosted.is_empty() {
            return Ok(Vec::new());
        }
        let entering = entering(batch);
        let leaving = batch.leaving();
        let mut hosts = Hosts {
            path: &self.path,
            entering: &entering,
            leaving: &leaving,
            noted: &state.above,
            laid: None,
            set: &state.set,
            frames: &mut state.frames,
            directory: &mut state.directory,
            guard: &op.guard,
            at: op.at,
        };
        let mut noted = Vec::new();
        for (subtree, host) in hosted {
            let (mut above, mut at) = (Vec::new(), host);
            // A batch can state hosts in a circle, which the frames
            // refuse: the walk ends where it comes to a subtree again.
            while let Some(host) = at.filter(|host| !above.contains(host)) {
                above.push(host);
                at = hosts.host(&host)?;
            }
            noted.push((subtree, above));
        }
        Ok(noted)
    }

    /// The conflict rule for what a drop covers without naming it
    /// (`transactions.md` CF5, CV9): a drop deletes every record under
    /// every subtree below the dropped one, and its key set names the
    /// dropped subtree and, of the subtrees below it, those that its
    /// transaction had written under. `commits` were published since the
    /// transaction last looked, up to the version `at`.
    ///
    /// - **A commit dropped or cleared a subtree above one the transaction
    ///   wrote under.** The subtree is then entered no more, or it is
    ///   below other Merks than it was: every write of the transaction
    ///   under it is met. A subtree that was itself dropped or cleared is
    ///   named by the key set of that commit, which meets what it covers.
    /// - **A commit dropped or cleared a subtree that was above the host
    ///   of a subtree which the transaction entered if it was absent.**
    ///   Where the committed version did not have such a subtree at the
    ///   transaction's write, the Merks it is below are not known then: a
    ///   commit beside the transaction can enter it, and a later one take
    ///   it with a subtree above, between two looks of the transaction.
    ///   So each commit is held against the names of the subtrees that
    ///   the stated host was in or below as the transaction had them at
    ///   the write ([`Self::above`]), by its own key set. A commit that
    ///   took one of them meets the writes under the subtree that the
    ///   transaction made from that enter on, until it entered the
    ///   subtree again, and before the commit
    ///   ([`WriteSet::meet_entered`]).
    ///
    /// The first rule goes by the Merks that the transaction saw a
    /// subtree below at a look, and the second by the names that an enter
    /// if it is absent recorded. Neither sees the following, so there the
    /// outcome can depend on whether the transaction looked between two
    /// commits: a subtree is not entered at the transaction's first write
    /// under it, a commit beside the transaction enters it, and a later
    /// one takes it with a subtree above.
    ///
    /// - With an enter of a subtree that is new ([`SubtreeWrite::Enter`]),
    ///   which records no names, that is every such pair of commits.
    /// - With an enter if it is absent it is a pair whose later commit
    ///   takes the subtree with one that is not among the recorded names:
    ///   for one, the first entered it under an element of a Merk beside
    ///   the stated host's, and the later one drops that Merk.
    /// - **The transaction dropped or cleared a subtree, and a commit
    ///   wrote, dropped or cleared one below it.** The drop is met.
    ///
    /// A look that fails on a read keeps nothing of where it found the
    /// subtrees: the next one holds the commits against what the last
    /// look that was made saw.
    pub(super) fn meet_below(
        &self,
        state: &mut State<'s>,
        guard: &OperationGuard<'s>,
        at: CommitVersion,
        commits: &[(CommitVersion, Arc<KeySet>)],
    ) -> Result<(), TxnError> {
        if commits.is_empty() || state.set.is_empty() {
            return Ok(());
        }
        #[cfg(test)]
        let refused = std::mem::take(&mut state.refuse_look);
        let State {
            directory,
            set,
            under,
            above,
            ..
        } = state;
        for noted in above.iter() {
            for (version, keys) in commits {
                if noted.names.iter().any(|name| keys.takes(name)) {
                    set.meet_entered(noted.serial, noted.subtree, *version);
                }
            }
        }
        let opened = Opened::at(directory, &self.path, guard, at)?;
        // Where the subtrees hang now is read before anything is kept of
        // it: a look that fails on a read has seen none of them.
        let mut seen = Vec::with_capacity(under.len());
        for subtree in under.keys() {
            let now: Option<Vec<MerkId>> = opened.lineage(&self.path, guard, subtree)?;
            seen.push((*subtree, now));
        }
        let mut met = Vec::new();
        let mut leaving: Vec<Subtree> = set.leaving().collect();
        leaving.sort_unstable();
        leaving.dedup();
        for above in leaving {
            for (version, keys) in commits {
                let units = keys.units.keys().map(|unit| unit.subtree);
                for subtree in units.chain(keys.subtrees.iter().copied()) {
                    // The hosts are followed through the entries that are
                    // stored, also one that the commit's own drop left.
                    let mut at = subtree;
                    let mut below = false;
                    for _ in 0..MAX_DEPTH {
                        match opened.host(&self.path, guard, &at)? {
                            Some(host) if host == above => below = true,
                            Some(host) => {
                                at = host;
                                continue;
                            }
                            None => {}
                        }
                        break;
                    }
                    if below {
                        met.push((above, *version));
                        break;
                    }
                }
            }
        }
        #[cfg(test)]
        if refused {
            return Err(TxnError::Damaged("a look that a test refuses"));
        }
        for (subtree, now) in seen {
            let was = under.get_mut(&subtree).expect("a subtree that was read");
            if was.is_some() && now != *was {
                set.meet_subtree(subtree, at);
            }
            *was = now;
        }
        for (above, version) in met {
            set.meet_leaving(above, version);
        }
        Ok(())
    }
}
