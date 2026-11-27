//! [`EitherEngine`]: the RocksDB engine or the LigninDB engine, chosen when
//! the database is opened.
//!
//! The type is an [`Engine`] that holds one of the two, by an enum. Its
//! transaction, handles, cursors, change sets and moves are [`OneOf`] the
//! two engines' own. A call that reads or writes a database, given what is
//! of the engine that is held, is that engine's call, with its answer and
//! its `OperationCost`. What is of the other engine is refused by the type
//! itself, and the type opens no write scope of RocksDB
//! ([`Engine::open_write_scope`] on [`EitherEngine`]).
//!
//! [`EitherEngine::open_with`] is told the engine by an [`EngineChoice`].
//! Opens and path-only removals classify the directory before calling an
//! engine ([`EitherEngine::found_at`] says how a directory is told). Where
//! no entry says an engine, a RocksDB open of a path whose last name ends
//! in `.creating` is refused. And a creation or a removal of a checkpoint
//! at `p` is not handed to LigninDB where the type, when it looks, finds
//! that the entries of `p.creating` say RocksDB: LigninDB builds the
//! checkpoint at `p` in a directory of that name, and leaves it alone if
//! it lists a RocksDB entry. The type refuses the call before invoking
//! LigninDB.
//!
//! `docs/lignindb/engine.md`, "An engine chosen at the open", is the
//! contract.

#[cfg(test)]
mod tests;

use std::{
    cell::Cell,
    ffi::{OsStr, OsString},
    fmt,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostResult, CostsExt,
    OperationCost,
};
use grovedb_merk::{
    store::{NodeChange, NodeChangeSet, NodeStore, NodeWriteCost},
    tree::{kv::ValueDefinedCostType, TreeNode, TreeNodeInner},
    Error as MerkError, Link, TreeType,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    flat::{PendingPrefixDropRecord, PendingPrefixDrops, SubtreePrefix},
    rocksdb_storage::Tx,
    Error, FlatStore, RawIterator,
};
use grovedb_version::version::GroveVersion;
use lignin_io::{EntryKind, Vfs};
/// The LigninDB crate, for the options an [`EngineChoice`] carries
/// ([`lignindb::Options`]).
pub use lignindb;
/// When a commit of LigninDB makes its log durable: the type of
/// [`lignindb::Options::sync`].
pub use lignindb::options::SyncPolicy;
use lignindb::{
    checkpoint::{CREATING_SUFFIX, DELETING_FILE_NAME},
    Options,
};

use super::{
    lignin::{LigninTransaction, LigninWriteScope},
    Engine, EngineBatch, LigninEngine, RocksDbEngine, SubtreeCursor, SubtreeHandle, SubtreeMove,
    WriteGroup,
};

/// Which of the two engines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EngineKind {
    /// The RocksDB engine, [`RocksDbEngine`].
    RocksDb,
    /// The LigninDB engine, [`LigninEngine`].
    Lignin,
}

impl EngineKind {
    /// The other engine.
    pub fn other(self) -> Self {
        match self {
            EngineKind::RocksDb => EngineKind::Lignin,
            EngineKind::Lignin => EngineKind::RocksDb,
        }
    }
}

impl fmt::Display for EngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EngineKind::RocksDb => "RocksDB",
            EngineKind::Lignin => "LigninDB",
        })
    }
}

/// The engine a database is opened on, with that engine's own options.
#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum EngineChoice {
    /// The RocksDB engine, opened as [`RocksDbEngine`] opens a database:
    /// it takes no options.
    RocksDb,
    /// The LigninDB engine, opened with these options, which are handed to
    /// it whole.
    Lignin(Options),
}

impl EngineChoice {
    /// The engine that is chosen.
    pub fn kind(&self) -> EngineKind {
        match self {
            EngineChoice::RocksDb => EngineKind::RocksDb,
            EngineChoice::Lignin(_) => EngineKind::Lignin,
        }
    }
}

impl fmt::Debug for EngineChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EngineChoice({})", self.kind())
    }
}

/// A thing of the RocksDB engine or the same thing of the LigninDB engine:
/// a transaction, a subtree handle, a cursor, a change set, a move.
///
/// Where both are a [`NodeStore`], a [`FlatStore`], a [`SubtreeHandle`], a
/// cursor ([`RawIterator`]), a [`NodeChangeSet`] or a [`SubtreeMove`], so is
/// this type, and its methods hand what is of the same engine on to the
/// one inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneOf<R, L> {
    /// The RocksDB engine's.
    RocksDb(R),
    /// The LigninDB engine's.
    Lignin(L),
}

impl<R, L> OneOf<R, L> {
    /// Which engine's it is.
    pub fn kind(&self) -> EngineKind {
        match self {
            OneOf::RocksDb(_) => EngineKind::RocksDb,
            OneOf::Lignin(_) => EngineKind::Lignin,
        }
    }

    /// The RocksDB engine's, where it is that.
    pub fn rocksdb(&self) -> Option<&R> {
        match self {
            OneOf::RocksDb(held) => Some(held),
            OneOf::Lignin(_) => None,
        }
    }

    /// The LigninDB engine's, where it is that.
    pub fn lignin(&self) -> Option<&L> {
        match self {
            OneOf::RocksDb(_) => None,
            OneOf::Lignin(held) => Some(held),
        }
    }
}

/// The call on the one inside.
macro_rules! held {
    ($one:expr, $inside:ident => $call:expr) => {
        match $one {
            OneOf::RocksDb($inside) => $call,
            OneOf::Lignin($inside) => $call,
        }
    };
}

/// The call on the one inside, whose result is the same one of the two.
macro_rules! held_as_one {
    ($one:expr, $inside:ident => $call:expr) => {
        match $one {
            OneOf::RocksDb($inside) => OneOf::RocksDb($call),
            OneOf::Lignin($inside) => OneOf::Lignin($call),
        }
    };
}

/// What a store of one engine says of a change set or a prepared node of
/// the other.
fn of_the_other_store(what: &str) -> MerkError {
    MerkError::StorageError(Error::NotSupported(format!(
        "{what} of the other engine was handed to a store of an engine that is one of two"
    )))
}

impl<R: RawIterator, L: RawIterator> RawIterator for OneOf<R, L> {
    fn seek_to_first(&mut self) -> CostContext<()> {
        held!(self, cursor => cursor.seek_to_first())
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        held!(self, cursor => cursor.seek_to_last())
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        held!(self, cursor => cursor.seek(key))
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        held!(self, cursor => cursor.seek_for_prev(key))
    }

    fn next(&mut self) -> CostContext<()> {
        held!(self, cursor => cursor.next())
    }

    fn prev(&mut self) -> CostContext<()> {
        held!(self, cursor => cursor.prev())
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        held!(self, cursor => cursor.value())
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        held!(self, cursor => cursor.key())
    }

    fn valid(&self) -> CostContext<bool> {
        held!(self, cursor => cursor.valid())
    }

    fn status(&self) -> Result<(), Error> {
        held!(self, cursor => cursor.status())
    }
}

impl<R: NodeChangeSet, L: NodeChangeSet> NodeChangeSet for OneOf<R, L> {
    type Prepared = OneOf<R::Prepared, L::Prepared>;

    fn prepare_node(&mut self, node: &TreeNode, change: NodeChange) -> Self::Prepared {
        held_as_one!(self, changes => changes.prepare_node(node, change))
    }

    fn put_node(
        &mut self,
        key: &[u8],
        node: Self::Prepared,
        cost: Option<NodeWriteCost>,
    ) -> Result<(), MerkError> {
        match (self, node) {
            (OneOf::RocksDb(changes), OneOf::RocksDb(node)) => changes.put_node(key, node, cost),
            (OneOf::Lignin(changes), OneOf::Lignin(node)) => changes.put_node(key, node, cost),
            _ => Err(of_the_other_store("a prepared node")),
        }
    }

    fn delete_node(&mut self, key: &[u8], cost: Option<KeyValueStorageCost>) {
        held!(self, changes => changes.delete_node(key, cost))
    }

    fn state_root(&mut self, root_key: Option<&[u8]>) {
        held!(self, changes => changes.state_root(root_key))
    }

    fn state_whole_merk(&mut self) {
        held!(self, changes => changes.state_whole_merk())
    }

    fn put_root_key(
        &mut self,
        root_key: &[u8],
        cost: Option<KeyValueStorageCost>,
    ) -> Result<(), MerkError> {
        held!(self, changes => changes.put_root_key(root_key, cost))
    }

    fn delete_root_key(&mut self, cost: Option<KeyValueStorageCost>) {
        held!(self, changes => changes.delete_root_key(cost))
    }

    fn put_aux_record(
        &mut self,
        key: &[u8],
        value: &[u8],
        cost: Option<KeyValueStorageCost>,
    ) -> Result<(), MerkError> {
        held!(self, changes => changes.put_aux_record(key, value, cost))
    }

    fn delete_aux_record(&mut self, key: &[u8], cost: Option<KeyValueStorageCost>) {
        held!(self, changes => changes.delete_aux_record(key, cost))
    }
}

impl<'db, R: NodeStore<'db>, L: NodeStore<'db>> NodeStore<'db> for OneOf<R, L> {
    type ChangeSet = OneOf<R::ChangeSet, L::ChangeSet>;
    type Cursor = OneOf<R::Cursor, L::Cursor>;

    fn get_node(
        &self,
        key: &[u8],
        value_defined_cost_fn: Option<
            impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<Option<TreeNode>, MerkError> {
        held!(self, store => store.get_node(key, value_defined_cost_fn, grove_version))
    }

    fn get_node_for_read(&self, key: &[u8]) -> CostResult<Option<TreeNodeInner>, MerkError> {
        held!(self, store => store.get_node_for_read(key))
    }

    fn has_node(&self, key: &[u8]) -> CostResult<bool, MerkError> {
        held!(self, store => store.has_node(key))
    }

    fn fetch_node(
        &self,
        link: &Link,
        value_defined_cost_fn: Option<
            &impl Fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, MerkError> {
        held!(self, store => store.fetch_node(link, value_defined_cost_fn, grove_version))
    }

    fn fetch_node_without_value(
        &self,
        link: &Link,
        grove_version: &GroveVersion,
    ) -> CostResult<TreeNode, MerkError> {
        held!(self, store => store.fetch_node_without_value(link, grove_version))
    }

    fn load_node_value(&self, node: &mut TreeNode) -> CostResult<(), MerkError> {
        held!(self, store => store.load_node_value(node))
    }

    fn takes_links_to_absent_nodes(&self) -> bool {
        held!(self, store => store.takes_links_to_absent_nodes())
    }

    fn write_node(&self, node: &TreeNode, change: NodeChange) -> CostResult<(), MerkError> {
        held!(self, store => store.write_node(node, change))
    }

    fn state_tree_type(&self, tree_type: TreeType) -> Result<(), MerkError> {
        held!(self, store => store.state_tree_type(tree_type))
    }

    fn root_key(&self) -> CostResult<Option<Vec<u8>>, MerkError> {
        held!(self, store => store.root_key())
    }

    fn set_root_key(&self, root_key: Option<&[u8]>) -> CostResult<(), MerkError> {
        held!(self, store => store.set_root_key(root_key))
    }

    fn cursor(&self) -> Self::Cursor {
        held_as_one!(self, store => store.cursor())
    }

    fn change_set(&self) -> Self::ChangeSet {
        held_as_one!(self, store => store.change_set())
    }

    fn commit(&self, change_set: Self::ChangeSet) -> CostResult<(), MerkError> {
        match (self, change_set) {
            (OneOf::RocksDb(store), OneOf::RocksDb(changes)) => store.commit(changes),
            (OneOf::Lignin(store), OneOf::Lignin(changes)) => store.commit(changes),
            _ => Err(of_the_other_store("a change set")).wrap_with_cost(OperationCost::default()),
        }
    }

    fn get_aux_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, MerkError> {
        held!(self, store => store.get_aux_record(key))
    }

    fn get_meta_record(&self, key: &[u8]) -> CostResult<Option<Vec<u8>>, MerkError> {
        held!(self, store => store.get_meta_record(key))
    }

    fn put_meta_record(&self, key: &[u8], value: &[u8]) -> CostResult<(), MerkError> {
        held!(self, store => store.put_meta_record(key, value))
    }

    fn delete_meta_record(&self, key: &[u8]) -> CostResult<(), MerkError> {
        held!(self, store => store.delete_meta_record(key))
    }
}

impl<R: FlatStore, L: FlatStore> FlatStore for OneOf<R, L> {
    type Cursor<'a>
        = OneOf<R::Cursor<'a>, L::Cursor<'a>>
    where
        Self: 'a;

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        held!(self, store => store.get(key))
    }

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        held!(self, store => store.put(key, value, cost_info))
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        held!(self, store => store.delete(key, cost_info))
    }

    fn cursor(&self) -> Self::Cursor<'_> {
        held_as_one!(self, store => store.cursor())
    }

    fn clear(&self) -> CostResult<(), Error> {
        held!(self, store => store.clear())
    }
}

impl<'db, R: SubtreeHandle<'db>, L: SubtreeHandle<'db>> SubtreeHandle<'db> for OneOf<R, L> {
    type Aux<'a>
        = OneOf<R::Aux<'a>, L::Aux<'a>>
    where
        Self: 'a;
    type Meta<'a>
        = OneOf<R::Meta<'a>, L::Meta<'a>>
    where
        Self: 'a;

    fn aux(&self) -> Self::Aux<'_> {
        held_as_one!(self, handle => handle.aux())
    }

    fn meta(&self) -> Self::Meta<'_> {
        held_as_one!(self, handle => handle.meta())
    }
}

impl<R: SubtreeMove, L: SubtreeMove> SubtreeMove for OneOf<R, L> {
    fn move_next<T>(
        &mut self,
        inspect: impl FnOnce(&[u8], &[u8]) -> Result<T, crate::Error>,
        bill: impl FnOnce(&[u8], &[u8]) -> KeyValueStorageCost,
    ) -> CostResult<Option<T>, crate::Error> {
        held!(self, records => records.move_next(inspect, bill))
    }
}

/// A transaction of an [`EitherEngine`]: the transaction of the engine it
/// holds.
///
/// The two are named by their types and not as the transactions of their
/// engines (`<RocksDbEngine as Engine>::Transaction<'db>`): a type that
/// holds one by that name, as the write scope does, would keep the
/// lifetime of its database as it was given, where a caller holds it with
/// a shorter one.
pub type EitherTransaction<'db> = OneOf<Tx<'db>, LigninTransaction<'db>>;

impl EitherTransaction<'_> {
    /// Whether the transaction is a snapshot read transaction.
    pub fn is_snapshot_read(&self) -> bool {
        held!(self, transaction => transaction.is_snapshot_read())
    }
}

/// The batch of an [`EitherEngine`]: a batch of each engine, since a batch
/// is made with no engine in hand.
///
/// A default batch starts unbound. A handle, move, or merge from a bound
/// batch binds it, to RocksDB or to LigninDB, and
/// [`EngineBatch::take_pending`] copies the binding. A handle stages its
/// writes in the part of the engine the batch is bound to. An
/// `EitherEngine` that holds the other engine opens no handle with a bound
/// batch, and does not settle or apply it.
#[derive(Default)]
pub struct EitherBatch {
    rocksdb: <RocksDbEngine as Engine>::Batch,
    lignin: <LigninEngine as Engine>::Batch,
    /// The batch's engine, once bound.
    of: Cell<Option<EngineKind>>,
}

impl EitherBatch {
    /// Whether the batch is of `kind`. One that is of neither engine yet
    /// becomes of `kind` here.
    fn taken_for(&self, kind: EngineKind) -> bool {
        let of = self.of.get().unwrap_or(kind);
        self.of.set(Some(of));
        of == kind
    }

    /// Whether the batch is of the engine other than `kind`.
    fn is_of_the_other(&self, kind: EngineKind) -> bool {
        self.of.get() == Some(kind.other())
    }
}

impl EngineBatch for EitherBatch {
    fn next_operation(&self) {
        EngineBatch::next_operation(&self.rocksdb);
        EngineBatch::next_operation(&self.lignin);
    }

    fn take_pending(&self) -> Self {
        Self {
            rocksdb: EngineBatch::take_pending(&self.rocksdb),
            lignin: EngineBatch::take_pending(&self.lignin),
            of: self.of.clone(),
        }
    }

    /// Merges `other` into this batch, which is of the engine `other` is
    /// of where it was of neither.
    ///
    /// # Panics
    ///
    /// Where the two are of different engines.
    fn merge_overwriting(&self, other: Self) {
        match (self.of.get(), other.of.get()) {
            (Some(of), Some(merged)) if of != merged => panic!(
                "a batch of {merged} was merged into a batch of {of}, of an engine that is one \
                 of two"
            ),
            (None, merged) => self.of.set(merged),
            _ => {}
        }
        EngineBatch::merge_overwriting(&self.rocksdb, other.rocksdb);
        EngineBatch::merge_overwriting(&self.lignin, other.lignin);
    }
}

/// The write group of an [`EitherEngine`]: the group of the engine that a
/// settle of a batch into it was first handed to, whether or not that
/// settle succeeded, and none before. A settle that the type refuses is
/// handed to no engine. What a settle that failed left in the group is
/// its engine's to say.
#[derive(Default)]
pub struct EitherWriteGroup {
    settled:
        Option<OneOf<<RocksDbEngine as Engine>::WriteGroup, <LigninEngine as Engine>::WriteGroup>>,
    /// The pending cost of a group that is of no engine yet: no cost.
    nothing: OperationCost,
}

impl WriteGroup for EitherWriteGroup {
    fn pending_cost(&self) -> &OperationCost {
        match &self.settled {
            Some(group) => held!(group, group => group.pending_cost()),
            None => &self.nothing,
        }
    }
}

/// A write scope of an [`EitherEngine`] that holds LigninDB. One that holds
/// RocksDB opens none.
pub struct EitherWriteScope<'db> {
    /// The scope of LigninDB, as the transaction it shows.
    view: EitherTransaction<'db>,
}

impl fmt::Debug for EitherWriteScope<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EitherWriteScope").finish_non_exhaustive()
    }
}

/// The engine that [`Engine::open`] of the type calls for a directory in
/// which no entry says an engine: RocksDB, and LigninDB in the unit tests
/// of this crate that are built for the suite engine `either-lignin`,
/// whatever the cfg of the default engine is.
const WHERE_NONE_IS_SAID: EngineKind = match cfg!(all(test, grovedb_test_engine = "either-lignin"))
{
    true => EngineKind::Lignin,
    false => EngineKind::RocksDb,
};

/// The engine whose database an entry of this name belongs to, if the
/// name is one that only that engine gives an entry of its database
/// directory.
fn engine_of_entry(name: &OsStr) -> Option<EngineKind> {
    let name = name.to_str()?;
    let rocksdb = matches!(name, "CURRENT" | "IDENTITY")
        || name.starts_with("MANIFEST-")
        || name.starts_with("OPTIONS-")
        || name.ends_with(".sst");
    let lignin =
        name == "MANIFEST" || name == DELETING_FILE_NAME || LIGNIN_DIRECTORIES.contains(&name);
    match (rocksdb, lignin) {
        (true, _) => Some(EngineKind::RocksDb),
        (false, true) => Some(EngineKind::Lignin),
        (false, false) => None,
    }
}

/// The directories LigninDB keeps its files in, inside its database
/// directory: the log, the segments, the value log and the page table.
const LIGNIN_DIRECTORIES: [&str; 4] = ["wal", "seg", "val", "table"];

/// What the entries of a directory say it holds: an engine, with the first
/// entry that says so.
fn held_among(
    dir: &Path,
    entries: impl IntoIterator<Item = OsString>,
) -> Result<Option<(EngineKind, OsString)>, Error> {
    let mut found: Option<(EngineKind, OsString)> = None;
    for entry in entries {
        let Some(kind) = engine_of_entry(&entry) else {
            continue;
        };
        let Some((first, of_first)) = &found else {
            found = Some((kind, entry));
            continue;
        };
        if kind != *first {
            return Err(Error::NotSupported(format!(
                "the directory {} holds entries of both engines (`{}` is {first}'s and `{}` is \
                 {kind}'s): it is opened as neither",
                dir.display(),
                of_first.to_string_lossy(),
                entry.to_string_lossy(),
            )));
        }
    }
    Ok(found)
}

fn unlisted(dir: &Path, error: std::io::Error) -> Error {
    Error::StorageError(format!(
        "the directory {} could not be listed to see which engine's database it holds: {error}",
        dir.display()
    ))
}

/// Lists `dir` on the file system of the process and says what it holds.
fn look(dir: &Path) -> Result<Option<(EngineKind, OsString)>, Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unlisted(dir, error)),
    };
    let mut names = entries
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| unlisted(dir, error))?;
    names.sort();
    held_among(dir, names)
}

/// Lists `dir` on the file system `vfs` of a LigninDB engine and says what
/// it holds.
fn look_on(vfs: &dyn Vfs, dir: &Path) -> Result<Option<(EngineKind, OsString)>, Error> {
    match vfs.read_dir(dir) {
        Ok(entries) => held_among(dir, entries.into_iter().map(|entry| entry.name)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(unlisted(dir, error)),
    }
}

/// Whether the last name of `path` ends in the suffix of the directory
/// LigninDB builds a checkpoint in, in any case of its ASCII letters:
/// the paths LigninDB opens no database at.
fn is_reserved(path: &Path) -> bool {
    let suffix = CREATING_SUFFIX.as_bytes();
    path.file_name().is_some_and(|name| {
        let name = name.as_encoded_bytes();
        (name.len().checked_sub(suffix.len()))
            .is_some_and(|at| name[at..].eq_ignore_ascii_case(suffix))
    })
}

/// Checkpoint creation by RocksDB rejects this suffix, ignoring ASCII
/// case, and so does a RocksDB open for the caller where no entry of the
/// directory says an engine ([`listed_for_rocksdb`]). LigninDB builds at
/// the literal sibling `p.creating`.
fn refuse_a_reserved_name(path: &Path) -> Result<(), Error> {
    match is_reserved(path) {
        true => Err(Error::NotSupported(format!(
            "the path {} ends in `{CREATING_SUFFIX}`; it cannot be used for a RocksDB open for \
             the caller or for checkpoint creation",
            path.display()
        ))),
        false => Ok(()),
    }
}

/// What a call on a checkpoint says of a directory in which no entry says
/// an engine, where the call is not handed to LigninDB. RocksDB's open of
/// a checkpoint can leave a directory, a `LOCK` and a `LOG` where it finds
/// no database, so it is called only where an entry says RocksDB.
fn no_checkpoint(dir: &Path) -> Error {
    Error::StorageError(format!(
        "the directory {} has no entry of a database of either engine, so it is no checkpoint: \
         no engine was called for it",
        dir.display()
    ))
}

/// Lists `dir` on the file system `choice` opens its database on, and
/// refuses a listing that has an entry of the other engine's. Otherwise it
/// says whether an entry says the chosen engine.
fn refuse_the_other(dir: &Path, choice: &EngineChoice) -> Result<bool, Error> {
    let found = match choice {
        EngineChoice::RocksDb => look(dir)?,
        EngineChoice::Lignin(options) => look_on(&*options.vfs, dir)?,
    };
    match found {
        Some((kind, entry)) if kind != choice.kind() => Err(Error::NotSupported(format!(
            "the directory {} holds a database of {kind} (its entry `{}`) and {} was chosen for \
             it: it is not opened, and nothing was written to it",
            dir.display(),
            entry.to_string_lossy(),
            choice.kind(),
        ))),
        found => Ok(found.is_some()),
    }
}

/// [`refuse_the_other`] for an open that names RocksDB. Where no entry
/// says an engine, a last name of `dir` that ends in `.creating` is
/// refused: LigninDB may be building a checkpoint there, and a directory
/// it has just made has no entry yet.
fn listed_for_rocksdb(dir: &Path) -> Result<bool, Error> {
    let says_rocksdb = refuse_the_other(dir, &EngineChoice::RocksDb)?;
    if !says_rocksdb {
        refuse_a_reserved_name(dir)?;
    }
    Ok(says_rocksdb)
}

/// A call that is handed to LigninDB for a checkpoint at a path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OfLignin {
    Creation,
    Removal,
}

/// The directory beside `target` that LigninDB looks for, and takes for
/// what a creation of a checkpoint there left where it lists nothing but
/// what a creation puts there, where `call` at `target` on the file
/// system `vfs` would look for one: the one under the name of `target`
/// with `.creating` added, where the directory above lists that name,
/// byte for byte, as a directory of its own. That is how
/// `lignindb::checkpoint` finds it.
///
/// `None` where LigninDB looks for none. It makes no checkpoint at a target
/// whose last name ends in `.creating`, in any case of its ASCII letters,
/// and its removal of a path whose last name ends so, as it is written,
/// is of that directory and of nothing beside it. A name that the
/// directory above lists in another spelling, as a link or as a file is
/// not looked into, and neither is anything where there is no directory
/// above.
///
/// A listing of the directory above that fails otherwise is the error:
/// LigninDB lists that directory again itself, and its listing can
/// succeed where this one failed.
fn taken_beside(vfs: &dyn Vfs, target: &Path, call: OfLignin) -> Result<Option<PathBuf>, Error> {
    let (Some(name), Some(above)) = (target.file_name(), target.parent()) else {
        return Ok(None);
    };
    let its_own = match call {
        OfLignin::Creation => is_reserved(target),
        OfLignin::Removal => (name.as_encoded_bytes()).ends_with(CREATING_SUFFIX.as_bytes()),
    };
    if its_own {
        return Ok(None);
    }
    let mut beside = name.to_os_string();
    beside.push(CREATING_SUFFIX);
    let above = match above.as_os_str().is_empty() {
        true => Path::new("."),
        false => above,
    };
    let entries = match vfs.read_dir(above) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unlisted(above, error)),
    };
    let listed = (entries.iter()).any(|entry| entry.name == beside && entry.kind == EntryKind::Dir);
    Ok(listed.then(|| target.with_file_name(beside)))
}

/// Refuses to hand `call` for a checkpoint at `target` to LigninDB, which
/// would act on the file system `vfs`, where an entry of the directory it
/// would look for beside `target` ([`taken_beside`]) says RocksDB.
/// LigninDB builds the checkpoint at `target` in that directory, and
/// takes one that is there for what a creation left only where it lists
/// nothing but what a creation puts there: a creation at `target` then
/// removes what was built in it, and a removal at `target` removes it.
/// One with an entry of RocksDB's it neither builds in nor removes
/// (`lignindb::checkpoint`). The type refuses the call before invoking
/// LigninDB. The opens of this type hand a database of RocksDB under
/// such a name to RocksDB ([`listed_for_rocksdb`]).
///
/// Where LigninDB would look for no directory, and where no entry of the
/// one it would look for says an engine or one says LigninDB, the call
/// goes on. A
/// listing of that directory with entries of both engines, and a listing
/// of it or of the directory above that fails otherwise than for a
/// directory that is not there, is the listing's error.
fn keep_from_lignindb(vfs: &dyn Vfs, target: &Path, call: OfLignin) -> Result<(), Error> {
    let Some(beside) = taken_beside(vfs, target, call)? else {
        return Ok(());
    };
    match look_on(vfs, &beside)? {
        Some((EngineKind::RocksDb, entry)) => Err(Error::NotSupported(format!(
            "the directory {} beside the checkpoint path {} has an entry of RocksDB's (`{}`); \
             LigninDB builds a checkpoint at the path in a directory of that name and leaves \
             it alone if it lists a RocksDB entry, so LigninDB was not called and nothing was \
             changed",
            beside.display(),
            target.display(),
            entry.to_string_lossy(),
        ))),
        _ => Ok(()),
    }
}

/// Why no checkpoint is made at `target` by a LigninDB engine that was
/// built on a database its caller opened, and does not say which file
/// system it is on.
fn not_looked_beside(target: &Path) -> Error {
    Error::NotSupported(format!(
        "no checkpoint was made at {}: the LigninDB engine was built on a database that its \
         caller opened, so the file system it builds on is not known here and the directory \
         beside the path, in which LigninDB builds the checkpoint, was not looked at; open the \
         database with GroveDB's own open of the engine to make checkpoints through it",
        target.display()
    ))
}

/// The RocksDB engine or the LigninDB engine, chosen when the database is
/// opened ([`Self::open_with`]).
///
/// It is an [`Engine`], so a `GroveDb<EitherEngine>` is a database on the
/// engine that was chosen, and code that names `GroveDb<EitherEngine>`
/// runs on either with no change.
///
/// A transaction, a batch, a write group or a write scope of an
/// `EitherEngine` that holds the other engine is not this engine's. A call
/// that is handed one returns [`Error::NotSupported`], or panics if it
/// returns no `Result`.
pub struct EitherEngine {
    held: OneOf<RocksDbEngine, LigninEngine>,
}

impl fmt::Debug for EitherEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EitherEngine({})", self.kind())
    }
}

impl From<RocksDbEngine> for EitherEngine {
    /// The engine holding a RocksDB engine that its caller opened.
    fn from(engine: RocksDbEngine) -> Self {
        Self {
            held: OneOf::RocksDb(engine),
        }
    }
}

impl From<LigninEngine> for EitherEngine {
    /// The engine holding a LigninDB engine that its caller opened.
    ///
    /// An engine that [`LigninEngine`] opened, by
    /// [`LigninEngine::open_with`] or by an open of [`Engine`], keeps the
    /// file system it is on, and makes its checkpoints through this type
    /// as one that this type opened does. One that was built on a
    /// database its caller opened ([`LigninEngine::new`]) does not say
    /// which file system it is on. The type cannot then look at the
    /// directory beside a checkpoint path as [`Engine::create_checkpoint`]
    /// of the type does, and makes no checkpoint through such an engine:
    /// the call is refused with [`Error::NotSupported`].
    fn from(engine: LigninEngine) -> Self {
        Self {
            held: OneOf::Lignin(engine),
        }
    }
}

impl EitherEngine {
    /// Opens the database at `path` on the engine `choice` names, and
    /// creates it if there is none, as that engine does: RocksDB always,
    /// LigninDB unless its options say otherwise
    /// ([`Options::create_if_missing`]).
    ///
    /// The directory is listed ([`Self::found_at`]), for LigninDB on
    /// the file system of its options. A listing with an entry of the
    /// other engine's, or with entries of both, is refused with
    /// [`Error::NotSupported`], and the chosen engine is not called. The
    /// listing and the open are two steps: a refusal, and the admission of
    /// a name below, are of what the directory held when it was listed.
    /// The listing goes by the names of the entries and looks at no lock.
    ///
    /// For RocksDB a path whose last name ends in `.creating`, in any case
    /// of its ASCII letters, is opened only where the listing says RocksDB
    /// and only RocksDB: a database of RocksDB that is there is opened as
    /// under any other name. Where there is no directory, or no entry
    /// says an engine, the path is refused for its name with
    /// [`Error::NotSupported`], and nothing is made. LigninDB builds the
    /// checkpoint at `p` at the literal sibling `p.creating`
    /// (`lignindb::checkpoint`), and a directory it has just made has no
    /// entry yet. A directory that is there under that name with an
    /// entry of a name of RocksDB's and none of LigninDB's is handed to
    /// RocksDB. LigninDB does not build in such a directory: a creation
    /// through this type is refused for it before LigninDB is called
    /// ([`Engine::create_checkpoint`] of the type), and one by LigninDB's
    /// own engine is refused by LigninDB.
    pub fn open_with<P: AsRef<Path>>(path: P, choice: &EngineChoice) -> Result<Self, Error> {
        let path = path.as_ref();
        match choice {
            EngineChoice::RocksDb => {
                listed_for_rocksdb(path)?;
                RocksDbEngine::open(path).map(Self::from)
            }
            EngineChoice::Lignin(options) => {
                refuse_the_other(path, choice)?;
                LigninEngine::open_with(path, options).map(Self::from)
            }
        }
    }

    /// Opens a checkpoint at `path` on the engine `choice` names, and
    /// makes no database where there is none: RocksDB as
    /// [`Engine::open_checkpoint`] of [`RocksDbEngine`] does, and LigninDB
    /// with the options of the choice, [`Options::create_if_missing`]
    /// taken as off.
    ///
    /// What [`Self::open_with`] refuses is refused here, a path whose last
    /// name ends in `.creating` by the same rule: where RocksDB is chosen
    /// and the path is refused for its name, with [`Error::NotSupported`].
    /// Where RocksDB is chosen, a listing of a path of any other name in
    /// which no entry says an engine is refused by this type too, with
    /// [`Error::StorageError`] and no engine called; where LigninDB is
    /// chosen it is LigninDB's to answer. An admitted RocksDB-only listing
    /// selects the native checkpoint open, which can write files before
    /// failing.
    pub fn open_checkpoint_with<P: AsRef<Path>>(
        path: P,
        choice: &EngineChoice,
    ) -> Result<Self, Error> {
        let path = path.as_ref();
        match choice {
            EngineChoice::RocksDb => match listed_for_rocksdb(path)? {
                true => RocksDbEngine::open_checkpoint(path).map(Self::from),
                false => Err(no_checkpoint(path)),
            },
            EngineChoice::Lignin(options) => {
                refuse_the_other(path, choice)?;
                let options = Options {
                    create_if_missing: false,
                    ..options.clone()
                };
                LigninEngine::open_with(path, &options).map(Self::from)
            }
        }
    }

    /// Which engine the entries of the directory at `path` say, by their
    /// names. The directory is listed on the file system of the process,
    /// and no entry of it is opened, read or changed. The listing looks at
    /// the names that are UTF-8, whatever kind of entry has them, and at
    /// nothing an entry holds.
    ///
    /// - RocksDB: an entry named `CURRENT` or `IDENTITY`, or one whose name
    ///   starts with `MANIFEST-` or `OPTIONS-` or ends in `.sst`.
    /// - LigninDB: an entry named `MANIFEST`, the name a removal gives that
    ///   manifest ([`lignindb::checkpoint::DELETING_FILE_NAME`]), or `wal`,
    ///   `seg`, `val` or `table`, the names of its directories.
    /// - `None`: there is no directory, or it has no such entry. A lock
    ///   file says nothing: both engines keep one named `LOCK`.
    ///
    /// A listing with names of both engines returns
    /// [`Error::NotSupported`], which names one entry of each. A listing
    /// that fails otherwise than for a directory that is not there returns
    /// [`Error::StorageError`].
    pub fn found_at<P: AsRef<Path>>(path: P) -> Result<Option<EngineKind>, Error> {
        Ok(look(path.as_ref())?.map(|(kind, _)| kind))
    }

    /// Removes the checkpoint at `path` as the engine `kind` removes one
    /// ([`Engine::delete_checkpoint`] of that engine), for a caller that
    /// knows which engine made it.
    ///
    /// [`Engine::delete_checkpoint`] of this type goes by the engine the
    /// entries of the directory say, and refuses one in which none says an
    /// engine. LigninDB's removal does more than remove a database: it
    /// finishes a removal that was cut short after the last name of the
    /// database was gone, and removes what a creation left beside a
    /// checkpoint that is not there. With [`EngineKind::Lignin`] this call
    /// hands such a directory to LigninDB. With [`EngineKind::RocksDb`] it
    /// is refused, with no engine called.
    ///
    /// A listing with an entry of the other engine's, or with entries of
    /// both, is refused with no engine called.
    ///
    /// With [`EngineKind::Lignin`], the directory that LigninDB's removal
    /// would look for with the checkpoint, beside `path` under its name
    /// with `.creating` added, is listed too before LigninDB is called, as
    /// [`Engine::create_checkpoint`] of the type looks for it: where an entry
    /// of it says RocksDB, the removal is refused with
    /// [`Error::NotSupported`] and nothing is changed. A `path` whose last
    /// name ends in `.creating` as it is written has no such directory:
    /// its removal is of the directory at `path` alone.
    ///
    /// The directory is listed and removed on the file system of the
    /// process. A checkpoint of LigninDB on another one
    /// ([`Options::vfs`]) is removed with [`lignindb::checkpoint::delete`]
    /// and that file system.
    pub fn delete_checkpoint_of<P: AsRef<Path>>(
        kind: EngineKind,
        path: P,
    ) -> Result<(), crate::Error> {
        let path = path.as_ref();
        match look(path)? {
            Some((found, entry)) if found != kind => Err(Error::NotSupported(format!(
                "the directory {} holds a database of {found} (its entry `{}`) and was to be \
                 removed as a checkpoint of {kind}: no engine was called for it",
                path.display(),
                entry.to_string_lossy(),
            ))
            .into()),
            None if kind == EngineKind::RocksDb => Err(no_checkpoint(path).into()),
            _ => match kind {
                EngineKind::RocksDb => RocksDbEngine::delete_checkpoint(path),
                EngineKind::Lignin => {
                    keep_from_lignindb(&*Options::default().vfs, path, OfLignin::Removal)?;
                    LigninEngine::delete_checkpoint(path)
                }
            },
        }
    }

    /// Which engine is held.
    pub fn kind(&self) -> EngineKind {
        self.held.kind()
    }

    /// The RocksDB engine, where that is held.
    pub fn rocksdb(&self) -> Option<&RocksDbEngine> {
        self.held.rocksdb()
    }

    /// The LigninDB engine, where that is held.
    pub fn lignin(&self) -> Option<&LigninEngine> {
        self.held.lignin()
    }

    fn refused(&self, what: &str) -> Error {
        Error::NotSupported(format!(
            "{what} of {} was handed to an engine that is one of two and holds {}",
            self.kind().other(),
            self.kind(),
        ))
    }

    fn refuse<T>(&self, what: &str) -> CostResult<T, Error> {
        Err(self.refused(what)).wrap_with_cost(OperationCost::default())
    }

    /// Refuses a batch of the other engine: its writes are staged in that
    /// engine's part, and this engine would settle its own part and drop
    /// them.
    fn its_own(&self, batch: &EitherBatch) -> Result<(), Error> {
        match batch.is_of_the_other(self.kind()) {
            true => Err(self.refused("a batch")),
            false => Ok(()),
        }
    }

    /// The batch a handle of this engine is opened with, which is of this
    /// engine from here on.
    ///
    /// # Panics
    ///
    /// Where the batch is of the other engine.
    #[track_caller]
    fn its_batch<'a>(&self, batch: &'a EitherBatch) -> &'a EitherBatch {
        match batch.taken_for(self.kind()) {
            true => batch,
            false => panic!("{}", self.refused("a batch")),
        }
    }

    #[track_caller]
    fn no_handle_on(&self) -> ! {
        panic!("{}", self.refused("a transaction"))
    }
}

impl PendingPrefixDrops for EitherEngine {
    fn pending_prefix_drop_records(&self) -> Result<Vec<PendingPrefixDropRecord>, Error> {
        held!(&self.held, engine => engine.pending_prefix_drop_records())
    }

    fn delete_prefix_ranges(&self, prefixes: &[SubtreePrefix]) -> Result<(), Error> {
        held!(&self.held, engine => engine.delete_prefix_ranges(prefixes))
    }

    fn remove_pending_prefix_drop_record(
        &self,
        primary_prefix: &SubtreePrefix,
    ) -> Result<(), Error> {
        held!(&self.held, engine => engine.remove_pending_prefix_drop_record(primary_prefix))
    }
}

/// The call on the engine that is held, with the transaction that is its
/// own, or `$other` where the transaction is of the other engine.
macro_rules! with_its_own {
    ($self:ident, $transaction:expr, |$engine:ident, $own:ident| $call:expr, $other:expr) => {
        match (&$self.held, $transaction) {
            (OneOf::RocksDb($engine), OneOf::RocksDb($own)) => $call,
            (OneOf::Lignin($engine), OneOf::Lignin($own)) => $call,
            _ => $other,
        }
    };
}

impl Engine for EitherEngine {
    type Applied = OneOf<<RocksDbEngine as Engine>::Applied, <LigninEngine as Engine>::Applied>;
    type Batch = EitherBatch;
    type ImmediateSubtree<'db> = OneOf<
        <RocksDbEngine as Engine>::ImmediateSubtree<'db>,
        <LigninEngine as Engine>::ImmediateSubtree<'db>,
    >;
    type Subtree<'db> =
        OneOf<<RocksDbEngine as Engine>::Subtree<'db>, <LigninEngine as Engine>::Subtree<'db>>;
    type SubtreeMove<'db> = OneOf<
        <RocksDbEngine as Engine>::SubtreeMove<'db>,
        <LigninEngine as Engine>::SubtreeMove<'db>,
    >;
    type Transaction<'db> = EitherTransaction<'db>;
    type WriteGroup = EitherWriteGroup;
    type WriteScope<'db> = EitherWriteScope<'db>;

    /// Opens the database at `path` on the engine that the entries of the
    /// directory say ([`Self::found_at`]), with that engine's default
    /// options. On an unmarked ordinary path, `EitherEngine::open` selects
    /// RocksDB except in the `either-lignin` unit suite, which selects
    /// LigninDB. Where no entry says an engine, the path would go to
    /// RocksDB and its last name ends in `.creating`, it is refused as by
    /// [`Self::open_with`]. Where an entry says RocksDB, the name is not
    /// looked at.
    fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let path = path.as_ref();
        let found = Self::found_at(path)?;
        match found.unwrap_or(WHERE_NONE_IS_SAID) {
            EngineKind::RocksDb => {
                if found.is_none() {
                    refuse_a_reserved_name(path)?;
                }
                RocksDbEngine::open(path).map(Self::from)
            }
            EngineKind::Lignin => LigninEngine::open(path).map(Self::from),
        }
    }

    /// Opens the checkpoint at `path` with the engine that the entries of
    /// the directory say. A listing in which none says an engine is
    /// refused with no engine called, whatever the name of the path. An
    /// admitted RocksDB-only listing selects the native checkpoint open,
    /// also under a last name that ends in `.creating`, which can write
    /// files before failing.
    fn open_checkpoint<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let path = path.as_ref();
        match Self::found_at(path)? {
            Some(EngineKind::RocksDb) => RocksDbEngine::open_checkpoint(path).map(Self::from),
            Some(EngineKind::Lignin) => LigninEngine::open_checkpoint(path).map(Self::from),
            None => Err(no_checkpoint(path)),
        }
    }

    /// Makes a checkpoint with the engine that is held. Where RocksDB is
    /// held, a `path` whose last name ends in `.creating`, in any case of
    /// its ASCII letters, is refused, whatever is there.
    ///
    /// Where LigninDB is held, the directory it would build in, beside
    /// `path` under its name with `.creating` added, is looked for first.
    /// LigninDB takes a directory that is there under that name for what
    /// a creation left only where it lists nothing but what a creation
    /// puts there, and the opens of this type hand a database of RocksDB
    /// under such a name to RocksDB. It is looked for as LigninDB finds
    /// it: the directory above has to list the name, byte for byte, as a
    /// directory of its own. One that it lists in another spelling, as a
    /// link or as a file is not LigninDB's to build in, and LigninDB is
    /// called as before; so it
    /// is for a `path` that LigninDB makes no checkpoint at for its name.
    /// A directory that is found is listed, and its entries are told as
    /// [`Self::found_at`] tells them: where one says RocksDB, the creation
    /// is refused with [`Error::NotSupported`], LigninDB is not called and
    /// nothing is changed, and a listing with entries of both engines is
    /// the listing's error. So is a listing that fails, of that directory
    /// or of the one above, though there is a directory to list: LigninDB
    /// is not called on what this type could not look at.
    ///
    /// The look is at the file system the engine is on, which is where it
    /// builds, and the type asks the engine it holds for it
    /// ([`Options::vfs`] of the options [`LigninEngine`] opened it with:
    /// those of the choice, or the default ones for an open that names no
    /// engine). So it is for an engine that this type opened and for one
    /// that its caller opened by [`LigninEngine`]'s opens and handed over
    /// ([`From`]). An engine that was built on a database its caller
    /// opened ([`LigninEngine::new`]) does not say which file system it is
    /// on. The type does not guess: the creation is then refused with
    /// [`Error::NotSupported`], and LigninDB is not called.
    ///
    /// The look and the creation are two steps. The type refuses what is
    /// there when it looks, and holds nothing: what is put under that
    /// name afterwards is LigninDB's to answer, by its own rule for such
    /// a directory (`lignindb::checkpoint`).
    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        let path = path.as_ref();
        match &self.held {
            OneOf::RocksDb(engine) => {
                refuse_a_reserved_name(path)?;
                engine.create_checkpoint(path)
            }
            OneOf::Lignin(engine) => {
                let Some(vfs) = engine.file_system() else {
                    return Err(not_looked_beside(path));
                };
                keep_from_lignindb(vfs, path, OfLignin::Creation)?;
                engine.create_checkpoint(path)
            }
        }
    }

    /// Removes the checkpoint at `path` as the engine that the entries of
    /// the directory say removes one. A listing in which none says an
    /// engine is refused with no engine called:
    /// [`Self::delete_checkpoint_of`] names the engine for what a removal
    /// or a creation of LigninDB left. Where the removal is LigninDB's,
    /// the directory beside `path` under its name with `.creating` added
    /// is looked for and listed first, as [`Self::create_checkpoint`] does
    /// it: LigninDB's removal removes that directory with the checkpoint
    /// where a creation left it, and where an entry of it says RocksDB
    /// the removal is refused with [`Error::NotSupported`] and nothing is
    /// changed.
    fn delete_checkpoint<P: AsRef<Path>>(path: P) -> Result<(), crate::Error> {
        let path = path.as_ref();
        match Self::found_at(path)? {
            Some(EngineKind::RocksDb) => RocksDbEngine::delete_checkpoint(path),
            Some(EngineKind::Lignin) => {
                keep_from_lignindb(&*Options::default().vfs, path, OfLignin::Removal)?;
                LigninEngine::delete_checkpoint(path)
            }
            None => Err(no_checkpoint(path).into()),
        }
    }

    fn flush(&self) -> Result<(), Error> {
        held!(&self.held, engine => engine.flush())
    }

    fn wipe(&self) -> Result<(), Error> {
        held!(&self.held, engine => engine.wipe())
    }

    fn start_transaction(&self) -> EitherTransaction<'_> {
        held_as_one!(&self.held, engine => engine.start_transaction())
    }

    fn start_snapshot_read_transaction(&self) -> EitherTransaction<'_> {
        held_as_one!(&self.held, engine => engine.start_snapshot_read_transaction())
    }

    fn start_read_transaction(&self) -> EitherTransaction<'_> {
        held_as_one!(&self.held, engine => engine.start_read_transaction())
    }

    fn commit_transaction(&self, transaction: EitherTransaction<'_>) -> CostResult<(), Error> {
        with_its_own!(
            self,
            transaction,
            |engine, own| engine.commit_transaction(own),
            self.refuse("a transaction")
        )
    }

    fn rollback_transaction(&self, transaction: &EitherTransaction<'_>) -> Result<(), Error> {
        with_its_own!(
            self,
            transaction,
            |engine, own| engine.rollback_transaction(own),
            Err(self.refused("a transaction"))
        )
    }

    fn set_savepoint(&self, transaction: &EitherTransaction<'_>) -> Result<(), Error> {
        with_its_own!(
            self,
            transaction,
            |engine, own| engine.set_savepoint(own),
            Err(self.refused("a transaction"))
        )
    }

    fn rollback_to_savepoint(&self, transaction: &EitherTransaction<'_>) -> Result<(), Error> {
        with_its_own!(
            self,
            transaction,
            |engine, own| engine.rollback_to_savepoint(own),
            Err(self.refused("a transaction"))
        )
    }

    /// Opens a write scope where LigninDB is held, as that engine does.
    ///
    /// Where RocksDB is held no scope is opened: a snapshot read
    /// transaction is refused as RocksDB refuses it, and a write
    /// transaction with [`Error::NotSupported`], where RocksDB grants one
    /// scope at a time. The engine inside is asked for no scope, so the
    /// refusal leaves its one scope as it was. That scope shows its
    /// transaction by a shared reference, and a transaction of this engine
    /// that could hold one could not move to another thread, as a
    /// transaction of RocksDB can. RocksDB rolls no scope back
    /// ([`Self::rolls_back_write_scopes`]), so no call of `GroveDb` asks
    /// it for one.
    fn open_write_scope<'a>(
        &'a self,
        transaction: &'a EitherTransaction<'_>,
    ) -> Result<EitherWriteScope<'a>, Error> {
        match (&self.held, transaction) {
            (OneOf::RocksDb(_), OneOf::RocksDb(own)) if own.is_snapshot_read() => {
                Err(Error::SnapshotReadOnlyTransaction("write scope"))
            }
            (OneOf::RocksDb(_), OneOf::RocksDb(_)) => Err(Error::NotSupported(
                "a write scope of RocksDB through an engine that is one of two: write to \
                     the transaction, which is where a scope of RocksDB writes"
                    .to_owned(),
            )),
            (OneOf::Lignin(engine), OneOf::Lignin(own)) => {
                let scope = engine.open_write_scope(own)?;
                Ok(EitherWriteScope {
                    view: OneOf::Lignin(scope.into_view()),
                })
            }
            _ => Err(self.refused("a transaction")),
        }
    }

    fn scope_transaction<'s, 'a: 's>(scope: &'s EitherWriteScope<'a>) -> &'s EitherTransaction<'a> {
        &scope.view
    }

    fn merge_write_scope(&self, scope: EitherWriteScope<'_>) -> Result<(), Error> {
        match (&self.held, scope.view) {
            (OneOf::Lignin(engine), OneOf::Lignin(view)) => {
                engine.merge_write_scope(LigninWriteScope::from_view(view))
            }
            _ => Err(self.refused("a write scope")),
        }
    }

    fn rollback_write_scope(&self, scope: EitherWriteScope<'_>) -> Result<(), Error> {
        match (&self.held, scope.view) {
            (OneOf::Lignin(engine), OneOf::Lignin(view)) => {
                engine.rollback_write_scope(LigninWriteScope::from_view(view))
            }
            _ => Err(self.refused("a write scope")),
        }
    }

    fn rolls_back_write_scopes(&self) -> bool {
        held!(&self.held, engine => engine.rolls_back_write_scopes())
    }

    fn subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        batch: Option<&'a EitherBatch>,
        transaction: &'a EitherTransaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        match (&self.held, transaction) {
            (OneOf::RocksDb(engine), OneOf::RocksDb(own)) => engine
                .subtree(path, batch.map(|batch| &self.its_batch(batch).rocksdb), own)
                .map(OneOf::RocksDb),
            (OneOf::Lignin(engine), OneOf::Lignin(own)) => engine
                .subtree(path, batch.map(|batch| &self.its_batch(batch).lignin), own)
                .map(OneOf::Lignin),
            _ => self.no_handle_on(),
        }
    }

    fn subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        batch: Option<&'a EitherBatch>,
        transaction: &'a EitherTransaction<'_>,
    ) -> CostContext<Self::Subtree<'a>> {
        match (&self.held, transaction) {
            (OneOf::RocksDb(engine), OneOf::RocksDb(own)) => engine
                .subtree_by_prefix(
                    prefix,
                    batch.map(|batch| &self.its_batch(batch).rocksdb),
                    own,
                )
                .map(OneOf::RocksDb),
            (OneOf::Lignin(engine), OneOf::Lignin(own)) => engine
                .subtree_by_prefix(
                    prefix,
                    batch.map(|batch| &self.its_batch(batch).lignin),
                    own,
                )
                .map(OneOf::Lignin),
            _ => self.no_handle_on(),
        }
    }

    fn subtree_beside<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        prefix: SubtreePrefix,
        batch: Option<&'a EitherBatch>,
        transaction: &'a EitherTransaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        match (&self.held, transaction) {
            (OneOf::RocksDb(engine), OneOf::RocksDb(own)) => engine
                .subtree_beside(
                    path,
                    prefix,
                    batch.map(|batch| &self.its_batch(batch).rocksdb),
                    own,
                )
                .map(OneOf::RocksDb),
            (OneOf::Lignin(engine), OneOf::Lignin(own)) => engine
                .subtree_beside(
                    path,
                    prefix,
                    batch.map(|batch| &self.its_batch(batch).lignin),
                    own,
                )
                .map(OneOf::Lignin),
            _ => self.no_handle_on(),
        }
    }

    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a EitherTransaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        match (&self.held, transaction) {
            (OneOf::RocksDb(engine), OneOf::RocksDb(own)) => {
                engine.immediate_subtree(path, own).map(OneOf::RocksDb)
            }
            (OneOf::Lignin(engine), OneOf::Lignin(own)) => {
                engine.immediate_subtree(path, own).map(OneOf::Lignin)
            }
            _ => self.no_handle_on(),
        }
    }

    fn immediate_subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        transaction: &'a EitherTransaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>> {
        match (&self.held, transaction) {
            (OneOf::RocksDb(engine), OneOf::RocksDb(own)) => engine
                .immediate_subtree_by_prefix(prefix, own)
                .map(OneOf::RocksDb),
            (OneOf::Lignin(engine), OneOf::Lignin(own)) => engine
                .immediate_subtree_by_prefix(prefix, own)
                .map(OneOf::Lignin),
            _ => self.no_handle_on(),
        }
    }

    fn adopt_restored_subtrees<'b, B>(
        &self,
        path: SubtreePath<'b, B>,
        transaction: &EitherTransaction<'_>,
    ) -> Result<(), Error>
    where
        B: AsRef<[u8]> + 'b,
    {
        with_its_own!(
            self,
            transaction,
            |engine, own| engine.adopt_restored_subtrees(path, own),
            Err(self.refused("a transaction"))
        )
    }

    fn settle_batch(
        &self,
        group: &mut EitherWriteGroup,
        batch: EitherBatch,
    ) -> CostResult<(), Error> {
        if let Err(refused) = self.its_own(&batch) {
            return Err(refused).wrap_with_cost(OperationCost::default());
        }
        match &self.held {
            OneOf::RocksDb(engine) => {
                match (group.settled).get_or_insert_with(|| OneOf::RocksDb(Default::default())) {
                    OneOf::RocksDb(group) => engine.settle_batch(group, batch.rocksdb),
                    OneOf::Lignin(_) => self.refuse("a write group"),
                }
            }
            OneOf::Lignin(engine) => {
                match (group.settled).get_or_insert_with(|| OneOf::Lignin(Default::default())) {
                    OneOf::Lignin(group) => engine.settle_batch(group, batch.lignin),
                    OneOf::RocksDb(_) => self.refuse("a write group"),
                }
            }
        }
    }

    fn apply_write_group(
        &self,
        group: EitherWriteGroup,
        transaction: &EitherTransaction<'_>,
    ) -> CostResult<Self::Applied, Error> {
        match (&self.held, transaction, group.settled) {
            (OneOf::RocksDb(engine), OneOf::RocksDb(own), None) => engine
                .apply_write_group(Default::default(), own)
                .map_ok(OneOf::RocksDb),
            (OneOf::RocksDb(engine), OneOf::RocksDb(own), Some(OneOf::RocksDb(group))) => {
                engine.apply_write_group(group, own).map_ok(OneOf::RocksDb)
            }
            (OneOf::Lignin(engine), OneOf::Lignin(own), None) => engine
                .apply_write_group(Default::default(), own)
                .map_ok(OneOf::Lignin),
            (OneOf::Lignin(engine), OneOf::Lignin(own), Some(OneOf::Lignin(group))) => {
                engine.apply_write_group(group, own).map_ok(OneOf::Lignin)
            }
            (held, transaction, _) if held.kind() != transaction.kind() => {
                self.refuse("a transaction")
            }
            _ => self.refuse("a write group"),
        }
    }

    fn apply_batch(
        &self,
        batch: EitherBatch,
        transaction: &EitherTransaction<'_>,
    ) -> CostResult<Self::Applied, Error> {
        if let Err(refused) = self.its_own(&batch) {
            return Err(refused).wrap_with_cost(OperationCost::default());
        }
        match (&self.held, transaction) {
            (OneOf::RocksDb(engine), OneOf::RocksDb(own)) => engine
                .apply_batch(batch.rocksdb, own)
                .map_ok(OneOf::RocksDb),
            (OneOf::Lignin(engine), OneOf::Lignin(own)) => {
                engine.apply_batch(batch.lignin, own).map_ok(OneOf::Lignin)
            }
            _ => self.refuse("a transaction"),
        }
    }

    fn retarget_cursor<'db>(
        cursor: SubtreeCursor<'db, Self>,
        prefix: SubtreePrefix,
    ) -> SubtreeCursor<'db, Self> {
        match cursor {
            OneOf::RocksDb(cursor) => {
                OneOf::RocksDb(RocksDbEngine::retarget_cursor(cursor, prefix))
            }
            OneOf::Lignin(cursor) => OneOf::Lignin(LigninEngine::retarget_cursor(cursor, prefix)),
        }
    }

    fn move_subtree<'a>(
        &'a self,
        from: SubtreePrefix,
        to: SubtreePrefix,
        batch: &'a EitherBatch,
        transaction: &'a EitherTransaction<'_>,
    ) -> CostContext<Self::SubtreeMove<'a>> {
        match (&self.held, transaction) {
            (OneOf::RocksDb(engine), OneOf::RocksDb(own)) => engine
                .move_subtree(from, to, &self.its_batch(batch).rocksdb, own)
                .map(OneOf::RocksDb),
            (OneOf::Lignin(engine), OneOf::Lignin(own)) => engine
                .move_subtree(from, to, &self.its_batch(batch).lignin, own)
                .map(OneOf::Lignin),
            _ => self.no_handle_on(),
        }
    }
}
