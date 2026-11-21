//! A seam for failing a storage read, the way a storage fault would,
//! without touching the database underneath.
//!
//! In test builds `open_merk` reads a subtree's parent element through
//! [`FaultableReads`], and a test can open a Merk over one to fail the read
//! of one of its nodes.
//!
//! [`RefusedReads`] is an engine whose deferred handles are all such
//! handles, so that a read fails inside an operation of `GroveDb`: the
//! fetch of a node by a walk, where [`with_one_failed_fetch`] arms it,
//! which in an insert is the walk of its write or of the read that
//! observes the old value before it; and a move of a cursor over the
//! nodes of a subtree, or the read of the record a move comes to, where
//! [`with_one_refused_move`] and [`with_one_unread_record`] arm them, which
//! are the reads inside a scan.

use std::{cell::RefCell, path::Path};

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostResult, CostsExt,
    OperationCost,
};
use grovedb_merk::{
    store::{NodeChange, NodeStore},
    tree::{kv::ValueDefinedCostType, TreeNode, TreeNodeInner},
    Error, Link,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    flat::{PendingPrefixDropRecord, PendingPrefixDrops, SubtreePrefix},
    Error as StorageError, FlatStore, RawIterator,
};
use grovedb_version::version::GroveVersion;

use crate::engine::{Engine, SubtreeCursor, SubtreeHandle};

/// The message of the injected failure.
pub(crate) const INJECTED_READ_FAILURE: &str = "injected read failure";

/// Which reads of a key fail.
struct ArmedFault {
    key: Vec<u8>,
    /// Reads of `key` let through before the first failure.
    passes_left: usize,
    /// Failures still to inject; `None` fails every read from then on.
    failures_left: Option<usize>,
    /// Whether only the fetch of a node through a link fails, which is the
    /// read a walk makes through a link, and no read of the node by its
    /// key.
    fetches_only: bool,
    /// Whether a read has failed.
    fired: bool,
}

impl ArmedFault {
    fn fails(&mut self, key: &[u8], fetch: bool) -> bool {
        if key != self.key || (self.fetches_only && !fetch) {
            return false;
        }
        if self.passes_left > 0 {
            self.passes_left -= 1;
            return false;
        }
        match &mut self.failures_left {
            Some(0) => return false,
            Some(left) => *left -= 1,
            None => {}
        }
        self.fired = true;
        true
    }
}

thread_local! {
    static ARMED: RefCell<Option<ArmedFault>> = const { RefCell::new(None) };
}

/// Runs `body` with `fault` armed and returns whether a read failed. The
/// fault armed before is put back afterwards, even when `body` panics, so a
/// scope nested in another leaves the outer one armed.
fn with_fault<T>(fault: ArmedFault, body: impl FnOnce() -> T) -> (T, bool) {
    struct Disarm(Option<ArmedFault>);

    impl Drop for Disarm {
        fn drop(&mut self) {
            ARMED.with(|armed| armed.replace(self.0.take()));
        }
    }

    let outer = Disarm(ARMED.with(|armed| armed.replace(Some(fault))));
    let result = body();
    let fired = ARMED.with(|armed| armed.borrow().as_ref().is_some_and(|fault| fault.fired));
    drop(outer);
    (result, fired)
}

/// Runs `body` with every read of `key` through a [`FaultableReads`] context
/// failing with a storage error. For `open_merk` that is the read of a
/// parent element stored under `key`.
pub(crate) fn with_failed_parent_read<T>(key: &[u8], body: impl FnOnce() -> T) -> T {
    with_fault(
        ArmedFault {
            key: key.to_vec(),
            passes_left: 0,
            failures_left: None,
            fetches_only: false,
            fired: false,
        },
        body,
    )
    .0
}

/// Runs `body` with one read of `key` failing: the one after `passes` reads
/// of it went through. Reads after the failed one succeed again. Also
/// returns whether `body` got as far as the failing read.
pub(crate) fn with_one_failed_parent_read<T>(
    key: &[u8],
    passes: usize,
    body: impl FnOnce() -> T,
) -> (T, bool) {
    with_fault(
        ArmedFault {
            key: key.to_vec(),
            passes_left: passes,
            failures_left: Some(1),
            fetches_only: false,
            fired: false,
        },
        body,
    )
}

/// Runs `body` with the first fetch of the node `key` through a link
/// failing, in whichever subtree a [`FaultableReads`] handle makes it:
/// `NodeStore::fetch_node` or `fetch_node_without_value`, the calls by
/// which a walk comes to a node that is not in memory. `get_node`,
/// `get_node_for_read` and `has_node` of the key go through, and so does
/// every fetch after the failed one. Also returns whether `body` got as far
/// as the failing fetch.
pub(crate) fn with_one_failed_fetch<T>(key: &[u8], body: impl FnOnce() -> T) -> (T, bool) {
    with_fault(
        ArmedFault {
            key: key.to_vec(),
            passes_left: 0,
            failures_left: Some(1),
            fetches_only: true,
            fired: false,
        },
        body,
    )
}

/// Which move of a cursor fails, and how.
struct ArmedMove {
    /// Moves let through before the one that fails.
    passes_left: usize,
    /// Whether the move is made and the record it comes to is withheld,
    /// where otherwise the move itself is refused.
    withholds: bool,
    /// Whether the fault was injected. It is injected once.
    fired: bool,
}

thread_local! {
    static ARMED_MOVE: RefCell<Option<ArmedMove>> = const { RefCell::new(None) };
}

/// Runs `body` with one move of a cursor refused: the move after `passes`
/// moves went through, counted over every cursor over the nodes of a
/// subtree that a [`FaultableReads`] handle gives while `body` runs. A move
/// is a seek or a step. The refused move is not made: its cursor stands on
/// no record from then on, and its status is the injected failure, until it
/// is sought again ([`RefusedMoves`]). Also returns whether `body` got as
/// far as the refused move.
pub(crate) fn with_one_refused_move<T>(passes: usize, body: impl FnOnce() -> T) -> (T, bool) {
    with_armed_move(passes, false, body)
}

/// Runs `body` with the record under one key withheld from a cursor: the
/// key that the first move after `passes` moves comes to, of the moves
/// that come to a key, counted as [`with_one_refused_move`] counts them.
/// The cursor stands on the key and shows no record under it, and its
/// status is the injected failure, until it is moved again. Also returns
/// whether `body` got as far as that key.
pub(crate) fn with_one_unread_record<T>(passes: usize, body: impl FnOnce() -> T) -> (T, bool) {
    with_armed_move(passes, true, body)
}

fn with_armed_move<T>(passes: usize, withholds: bool, body: impl FnOnce() -> T) -> (T, bool) {
    struct Disarm(Option<ArmedMove>);

    impl Drop for Disarm {
        fn drop(&mut self) {
            ARMED_MOVE.with(|armed| *armed.borrow_mut() = self.0.take());
        }
    }

    let armed = ArmedMove {
        passes_left: passes,
        withholds,
        fired: false,
    };
    let outer = Disarm(ARMED_MOVE.with(|slot| slot.replace(Some(armed))));
    let result = body();
    let fired = ARMED_MOVE.with(|armed| armed.borrow().as_ref().is_some_and(|armed| armed.fired));
    drop(outer);
    (result, fired)
}

/// The cursor of a [`FaultableReads`] handle over the nodes of its
/// subtree: the cursor it wraps, except for the one fault that
/// [`with_one_refused_move`] or [`with_one_unread_record`] arms.
///
/// A refused move is charged as a move and leaves the cursor as a cursor
/// of an engine that refused it: on no record, with nothing to show and
/// the refusal as its status. A step from there goes nowhere, and a seek
/// starts again. A record that is withheld leaves the cursor on its key,
/// with no value and the failure as its status, until the next move.
pub(crate) struct RefusedMoves<C> {
    cursor: C,
    refused: bool,
    unread: bool,
}

/// What is armed for the move that is about to be made: nothing, a refusal
/// of the move, or the withholding of the record it comes to.
fn armed_for_this_move() -> Option<bool> {
    ARMED_MOVE.with(|armed| {
        let mut armed = armed.borrow_mut();
        let armed = armed.as_mut().filter(|armed| !armed.fired)?;
        match armed.passes_left.checked_sub(1) {
            Some(left) => {
                armed.passes_left = left;
                None
            }
            None => Some(armed.withholds),
        }
    })
}

fn fire() {
    ARMED_MOVE.with(|armed| {
        if let Some(armed) = armed.borrow_mut().as_mut() {
            armed.fired = true;
        }
    });
}

impl<C: RawIterator> RefusedMoves<C> {
    fn new(cursor: C) -> Self {
        Self {
            cursor,
            refused: false,
            unread: false,
        }
    }

    /// Makes the move `go`, unless it is the armed one, or a step of a
    /// cursor whose move was refused.
    fn go(&mut self, seeks: bool, go: impl FnOnce(&mut C) -> CostContext<()>) -> CostContext<()> {
        let armed = armed_for_this_move();
        self.unread = false;
        self.refused = match armed {
            Some(false) => {
                fire();
                true
            }
            _ => self.refused && !seeks,
        };
        if self.refused {
            return ().wrap_with_cost(OperationCost::with_seek_count(1));
        }
        let moved = go(&mut self.cursor);
        // A record is withheld under a key: a move that comes to none
        // leaves the fault armed for the next one.
        if armed == Some(true) && self.cursor.valid().value {
            fire();
            self.unread = true;
        }
        moved
    }

    /// Hands the wrapped cursor to `retarget`, which points it at another
    /// subtree. A cursor that is pointed elsewhere has no failure to show.
    fn retargeted(self, retarget: impl FnOnce(C) -> C) -> Self {
        Self::new(retarget(self.cursor))
    }
}

impl<C: RawIterator> RawIterator for RefusedMoves<C> {
    fn seek_to_first(&mut self) -> CostContext<()> {
        self.go(true, C::seek_to_first)
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        self.go(true, C::seek_to_last)
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.go(true, |cursor| cursor.seek(key))
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.go(true, |cursor| cursor.seek_for_prev(key))
    }

    fn next(&mut self) -> CostContext<()> {
        self.go(false, C::next)
    }

    fn prev(&mut self) -> CostContext<()> {
        self.go(false, C::prev)
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        match self.refused || self.unread {
            true => None.wrap_with_cost(OperationCost::default()),
            false => self.cursor.value(),
        }
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        match self.refused {
            true => None.wrap_with_cost(OperationCost::default()),
            false => self.cursor.key(),
        }
    }

    fn valid(&self) -> CostContext<bool> {
        match self.refused {
            true => false.wrap_with_cost(OperationCost::default()),
            false => self.cursor.valid(),
        }
    }

    fn status(&self) -> Result<(), StorageError> {
        match self.refused || self.unread {
            true => Err(StorageError::StorageError(INJECTED_READ_FAILURE.to_owned())),
            false => self.cursor.status(),
        }
    }
}

/// A subtree handle that is the one it wraps, except that a node read of an
/// armed key fails, and that an armed move of its cursor over the nodes
/// fails.
pub(crate) struct FaultableReads<S>(pub(crate) S);

/// The failed read of `key`, if one is armed: the seek is paid for, as on
/// the real handle, and nothing is loaded.
fn injected_failure<T>(key: &[u8], fetch: bool) -> Option<CostResult<T, Error>> {
    ARMED
        .with(|armed| {
            armed
                .borrow_mut()
                .as_mut()
                .is_some_and(|fault| fault.fails(key, fetch))
        })
        .then(|| {
            Err(Error::StorageError(StorageError::StorageError(
                INJECTED_READ_FAILURE.to_owned(),
            )))
            .wrap_with_cost(OperationCost::with_seek_count(1))
        })
}

impl<'db, S: NodeStore<'db>> NodeStore<'db> for FaultableReads<S> {
    type ChangeSet = S::ChangeSet;
    type Cursor = RefusedMoves<S::Cursor>;

    fn get_node(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<TreeNode>, Error> {
        injected_failure(key, false)
            .unwrap_or_else(|| self.0.get_node(key, value_defined_cost_fn, grove_version))
    }

    fn get_node_for_read(&self, key: &[u8]) -> CostResult<Option<TreeNodeInner>, Error> {
        injected_failure(key, false).unwrap_or_else(|| self.0.get_node_for_read(key))
    }

    fn has_node(&self, key: &[u8]) -> CostResult<bool, Error> {
        injected_failure(key, false).unwrap_or_else(|| self.0.has_node(key))
    }

    fn fetch_node(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        injected_failure(link.key(), true).unwrap_or_else(|| {
            self.0
                .fetch_node(link, value_defined_cost_fn, grove_version)
        })
    }

    fn fetch_node_without_value(
        &self,
        link: &Link,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, Error> {
        injected_failure(link.key(), true)
            .unwrap_or_else(|| self.0.fetch_node_without_value(link, grove_version))
    }

    fn load_node_value(&self, node: &mut TreeNode) -> CostResult<(), Error> {
        self.0.load_node_value(node)
    }

    fn takes_links_to_absent_nodes(&self) -> bool {
        self.0.takes_links_to_absent_nodes()
    }

    fn write_node(&self, node: &TreeNode, change: NodeChange) -> CostResult<(), Error> {
        self.0.write_node(node, change)
    }

    fn root_key(&self) -> CostResult<Option<Vec<u8>>, Error> {
        self.0.root_key()
    }

    fn set_root_key(&self, root_key: Option<&[u8]>) -> CostResult<(), Error> {
        self.0.set_root_key(root_key)
    }

    fn cursor(&self) -> Self::Cursor {
        RefusedMoves::new(self.0.cursor())
    }

    fn change_set(&self) -> Self::ChangeSet {
        self.0.change_set()
    }

    fn commit(&self, change_set: Self::ChangeSet) -> CostResult<(), Error> {
        self.0.commit(change_set)
    }

    fn get_aux_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        self.0.get_aux_record(key)
    }

    fn get_meta_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        self.0.get_meta_record(key)
    }

    fn put_meta_record(&self, key: &[u8], value: &[u8]) -> CostResult<(), Error> {
        self.0.put_meta_record(key, value)
    }

    fn delete_meta_record(&self, key: &[u8]) -> CostResult<(), Error> {
        self.0.delete_meta_record(key)
    }
}

impl<S: FlatStore> FlatStore for FaultableReads<S> {
    type Cursor<'a>
        = S::Cursor<'a>
    where
        Self: 'a;

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, StorageError> {
        self.0.get(key)
    }

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), StorageError> {
        self.0.put(key, value, cost_info)
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), StorageError> {
        self.0.delete(key, cost_info)
    }

    fn cursor(&self) -> Self::Cursor<'_> {
        FlatStore::cursor(&self.0)
    }

    fn clear(&self) -> CostResult<(), StorageError> {
        self.0.clear()
    }
}

impl<'db, S: SubtreeHandle<'db>> SubtreeHandle<'db> for FaultableReads<S> {
    type Aux<'a>
        = S::Aux<'a>
    where
        Self: 'a;
    type Meta<'a>
        = S::Meta<'a>
    where
        Self: 'a;

    fn aux(&self) -> Self::Aux<'_> {
        self.0.aux()
    }

    fn meta(&self) -> Self::Meta<'_> {
        self.0.meta()
    }
}

/// The engine `E` with one difference: its deferred subtree handles are
/// [`FaultableReads`], so a node read of an armed key through one of them
/// fails, in whichever operation of `GroveDb` makes it, and an armed move
/// of a cursor over the nodes of one of them fails. Its immediate
/// handles, and the flat records of every handle with their cursors, are
/// `E`'s.
pub(crate) struct RefusedReads<E>(pub(crate) E);

impl<E: Engine> PendingPrefixDrops for RefusedReads<E> {
    fn pending_prefix_drop_records(&self) -> Result<Vec<PendingPrefixDropRecord>, StorageError> {
        self.0.pending_prefix_drop_records()
    }

    fn delete_prefix_ranges(&self, prefixes: &[SubtreePrefix]) -> Result<(), StorageError> {
        self.0.delete_prefix_ranges(prefixes)
    }

    fn remove_pending_prefix_drop_record(
        &self,
        primary_prefix: &SubtreePrefix,
    ) -> Result<(), StorageError> {
        self.0.remove_pending_prefix_drop_record(primary_prefix)
    }
}

impl<E: Engine> Engine for RefusedReads<E> {
    type Applied = E::Applied;
    type Batch = E::Batch;
    type ImmediateSubtree<'db> = E::ImmediateSubtree<'db>;
    type Subtree<'db> = FaultableReads<E::Subtree<'db>>;
    type SubtreeMove<'db> = E::SubtreeMove<'db>;
    type Transaction<'db> = E::Transaction<'db>;
    type WriteGroup = E::WriteGroup;
    type WriteScope<'db> = E::WriteScope<'db>;

    fn open<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        E::open(path).map(Self)
    }

    fn open_checkpoint<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        E::open_checkpoint(path).map(Self)
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), StorageError> {
        self.0.create_checkpoint(path)
    }

    fn flush(&self) -> Result<(), StorageError> {
        self.0.flush()
    }

    fn wipe(&self) -> Result<(), StorageError> {
        self.0.wipe()
    }

    fn start_transaction(&self) -> Self::Transaction<'_> {
        self.0.start_transaction()
    }

    fn start_snapshot_read_transaction(&self) -> Self::Transaction<'_> {
        self.0.start_snapshot_read_transaction()
    }

    fn start_read_transaction(&self) -> Self::Transaction<'_> {
        self.0.start_read_transaction()
    }

    fn commit_transaction(
        &self,
        transaction: Self::Transaction<'_>,
    ) -> CostResult<(), StorageError> {
        self.0.commit_transaction(transaction)
    }

    fn rollback_transaction(
        &self,
        transaction: &Self::Transaction<'_>,
    ) -> Result<(), StorageError> {
        self.0.rollback_transaction(transaction)
    }

    fn set_savepoint(&self, transaction: &Self::Transaction<'_>) -> Result<(), StorageError> {
        self.0.set_savepoint(transaction)
    }

    fn rollback_to_savepoint(
        &self,
        transaction: &Self::Transaction<'_>,
    ) -> Result<(), StorageError> {
        self.0.rollback_to_savepoint(transaction)
    }

    fn open_write_scope<'a>(
        &'a self,
        transaction: &'a Self::Transaction<'_>,
    ) -> Result<Self::WriteScope<'a>, StorageError> {
        self.0.open_write_scope(transaction)
    }

    fn scope_transaction<'s, 'a: 's>(scope: &'s Self::WriteScope<'a>) -> &'s Self::Transaction<'a> {
        E::scope_transaction(scope)
    }

    fn merge_write_scope(&self, scope: Self::WriteScope<'_>) -> Result<(), StorageError> {
        self.0.merge_write_scope(scope)
    }

    fn rollback_write_scope(&self, scope: Self::WriteScope<'_>) -> Result<(), StorageError> {
        self.0.rollback_write_scope(scope)
    }

    fn subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        batch: Option<&'a Self::Batch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.0.subtree(path, batch, transaction).map(FaultableReads)
    }

    fn subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        batch: Option<&'a Self::Batch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>> {
        self.0
            .subtree_by_prefix(prefix, batch, transaction)
            .map(FaultableReads)
    }

    fn subtree_beside<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        prefix: SubtreePrefix,
        batch: Option<&'a Self::Batch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.0
            .subtree_beside(path, prefix, batch, transaction)
            .map(FaultableReads)
    }

    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.0.immediate_subtree(path, transaction)
    }

    fn immediate_subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>> {
        self.0.immediate_subtree_by_prefix(prefix, transaction)
    }

    fn settle_batch(
        &self,
        group: &mut Self::WriteGroup,
        batch: Self::Batch,
    ) -> CostResult<(), StorageError> {
        self.0.settle_batch(group, batch)
    }

    fn apply_write_group(
        &self,
        group: Self::WriteGroup,
        transaction: &Self::Transaction<'_>,
    ) -> CostResult<Self::Applied, StorageError> {
        self.0.apply_write_group(group, transaction)
    }

    fn retarget_cursor<'db>(
        cursor: SubtreeCursor<'db, Self>,
        prefix: SubtreePrefix,
    ) -> SubtreeCursor<'db, Self> {
        cursor.retargeted(|cursor| E::retarget_cursor(cursor, prefix))
    }

    fn move_subtree<'a>(
        &'a self,
        from: SubtreePrefix,
        to: SubtreePrefix,
        batch: &'a Self::Batch,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::SubtreeMove<'a>> {
        self.0.move_subtree(from, to, batch, transaction)
    }
}
