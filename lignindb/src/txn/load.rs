//! A load: change sets that are whole Merks, laid out by the bulk loader,
//! with their values out of line in the value log before the commit
//! (`crate::txn`, "A load").

use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
};

use lignin_format::{
    tree_page::Hash,
    value_log::{checked_value_len, PendingValue, ValuePlace, ValuePointer},
    FileId, FormatVersion, PageId, SlotId,
};
use lignin_store::{
    cache::ValueId,
    segment::ManifestStore,
    values::{ValueHold, ValueStore, ValueStoreError, ValueWriter},
};
use lignin_tree::{
    bulk::{LoadError, LoadValues},
    fragment::{FragmentError, PageMut, Pages, Values},
    hash::{HashRules, Refusal},
    node::{NodeError, NodeHandle, NodePage, NodeRef},
    value::InlinePolicy,
};

use super::{
    error::{BatchError, TxnError, WholeFault},
    key::Subtree,
    pages::TreePages,
    write::{Batch, Hosted, MerkWrite, RootPut},
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the change set `merk` is laid out by the loader: it says it is
/// the whole Merk, and has a node.
pub(crate) fn is_load(merk: &MerkWrite<'_>) -> bool {
    merk.whole && !merk.nodes.is_empty()
}

/// Whether the change set `merk` writes anything.
fn writes(merk: &MerkWrite<'_>) -> bool {
    !merk.nodes.is_empty() || !merk.removed.is_empty() || merk.root != RootPut::Keep
}

/// The loads of `batch`, by their place among its change sets, in an order
/// that has a Merk before the load whose node hosts it: the host's cell is
/// written with the address of the root.
///
/// A node of a load hosts no Merk that the batch writes node by node: the
/// root of such a Merk has its place only once the loads are in their
/// pages. And no two loads host each other.
pub(crate) fn load_order(batch: &Batch<'_>) -> Result<Vec<usize>, BatchError> {
    let merks = batch.merks;
    let by_subtree: HashMap<Subtree, usize> = (merks.iter().enumerate())
        .map(|(merk, write)| (write.subtree, merk))
        .collect();
    // For each load, the loads its nodes host.
    let mut hosts: Vec<Vec<usize>> = vec![Vec::new(); merks.len()];
    for (merk, write) in merks.iter().enumerate().filter(|(_, merk)| is_load(merk)) {
        let subtree = write.subtree;
        for node in write.nodes {
            let Hosted::Set(hosted) = node.hosted else {
                continue;
            };
            for hosted in hosted.iter().filter_map(|hosted| by_subtree.get(hosted)) {
                match &merks[*hosted] {
                    other if is_load(other) => hosts[merk].push(*hosted),
                    other if writes(other) => {
                        let fault = WholeFault::HostsWritten;
                        return Err(BatchError::NotWhole { subtree, fault });
                    }
                    _ => {}
                }
            }
        }
    }
    // A walk that leaves a load after every load it hosts.
    #[derive(Clone, Copy, PartialEq)]
    enum Seen {
        No,
        Below,
        Done,
    }
    let mut seen = vec![Seen::No; merks.len()];
    let mut order = Vec::new();
    let mut walk: Vec<(usize, usize)> = Vec::new();
    for start in (0..merks.len()).filter(|merk| is_load(&merks[*merk])) {
        if seen[start] != Seen::No {
            continue;
        }
        seen[start] = Seen::Below;
        walk.push((start, 0));
        while let Some((merk, next)) = walk.last_mut() {
            let merk = *merk;
            match hosts[merk].get(*next) {
                Some(&hosted) => {
                    *next += 1;
                    match seen[hosted] {
                        Seen::Done => {}
                        Seen::Below => {
                            let (subtree, fault) = (merks[merk].subtree, WholeFault::HostsItself);
                            return Err(BatchError::NotWhole { subtree, fault });
                        }
                        Seen::No => {
                            seen[hosted] = Seen::Below;
                            walk.push((hosted, 0));
                        }
                    }
                }
                None => {
                    seen[merk] = Seen::Done;
                    order.push(merk);
                    walk.pop();
                }
            }
        }
    }
    Ok(order)
}

/// Where the bytes of a value are that a load put into the value log: the
/// value's pointer, and the page its record names as its owner.
///
/// It is what the write set keeps of such a value in the place of its
/// bytes. The record is written once and is never changed, so the bytes
/// are read from here for as long as the transaction holds the file
/// ([`Loaded`]), whichever page the node's cell is in by then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StoredValue {
    pub(crate) pointer: ValuePointer,
    pub(crate) owner: PageId,
}

impl StoredValue {
    /// The bytes of one in the arena of a write set.
    pub(crate) const LEN: usize = ValuePointer::ENCODED_LEN + 4;

    pub(crate) fn encode(&self) -> [u8; Self::LEN] {
        let mut bytes = [0; Self::LEN];
        let (pointer, owner) = bytes.split_at_mut(ValuePointer::ENCODED_LEN);
        pointer.copy_from_slice(&self.pointer.encode());
        owner.copy_from_slice(&self.owner.get().to_le_bytes());
        bytes
    }

    /// One that [`Self::encode`] wrote.
    pub(crate) fn decode(bytes: &[u8]) -> Self {
        let whole = "the bytes of a stored value were put in whole";
        let (pointer, owner) = bytes.split_at(ValuePointer::ENCODED_LEN);
        let pointer = ValuePointer::decode(pointer.try_into().expect(whole)).expect(whole);
        let owner = u32::from_le_bytes(owner.try_into().expect(whole));
        Self {
            pointer,
            owner: PageId::new(owner).expect(whole),
        }
    }

    /// The value's bytes, read from the value log: one positional read.
    /// `records` and `joined` are what the read reads into.
    pub(crate) fn read<'a>(
        &self,
        values: &ValueStore,
        records: &'a mut Vec<u8>,
        joined: &'a mut Vec<u8>,
    ) -> Result<&'a [u8], ValueStoreError> {
        let value = values.read(self.pointer, self.owner, records)?;
        if let Some(bytes) = value.contiguous() {
            return Ok(bytes);
        }
        joined.clear();
        value
            .parts()
            .for_each(|part| joined.extend_from_slice(part));
        Ok(joined)
    }
}

/// The value-log file that the loads of one open store append to: one
/// file at a time for all of them, so that a store has no file for each
/// transaction that loads.
///
/// A load appends under the transaction's lock (LT7 level 2), and this
/// lock is taken under it and before the manifest's (level 3), which is
/// held for the one replacement that names a new file. Nothing is waited
/// for under it but the file layer.
#[derive(Debug, Default)]
pub struct LoadFiles {
    writer: Mutex<Option<ValueWriter>>,
}

impl LoadFiles {
    /// The files of a store that no load has written to in this session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `value` for a cell of the page `owner`, under `hold`. A
    /// file that is full is closed first, and the next one is created and
    /// named by a manifest (`lignin_store::values`, "The life of a file").
    fn append(
        &self,
        values: &ValueStore,
        manifest: &Mutex<ManifestStore>,
        hold: &mut ValueHold,
        owner: PageId,
        value: &[u8],
    ) -> Result<ValuePointer, ValueStoreError> {
        let mut writer = lock(&self.writer);
        if let Some(full) = writer.take_if(|writer| writer.is_full()) {
            full.finish()?;
        }
        let file = match &mut *writer {
            Some(file) => file,
            empty => {
                let created = values.create()?;
                values.commit(&mut lock(manifest))?;
                empty.insert(created)
            }
        };
        let appended = file.append(hold, owner, value);
        if matches!(&appended, Err(error) if !matches!(error, ValueStoreError::Format { .. })) {
            // The file may take no value any more: the next one goes to
            // another file. What it took before is made durable here, if
            // it can be, since no later sync of this writer comes.
            let _ = file.sync();
            *writer = None;
        }
        appended
    }

    /// Makes every value durable that a load appended before the call. A
    /// file whose sync fails takes no more values, and what it holds past
    /// its last sync is covered by no manifest.
    pub(crate) fn sync(&self) -> Result<(), ValueStoreError> {
        let mut writer = lock(&self.writer);
        let synced = writer.as_mut().map(ValueWriter::sync);
        if let Some(Err(error)) = synced {
            *writer = None;
            return Err(error);
        }
        Ok(())
    }
}

/// Has a durable manifest cover every value that the loads of a
/// transaction put into the value log, where one does not yet: the file
/// they went to is synced, and the manifest is replaced
/// (`lignin_store::values`, "When a value is durable").
///
/// It is what the commit of a load does before its group names the
/// pointers (`log-format.md` LG16). A value that still is not covered
/// afterwards is in a file whose sync failed, and is not committed.
pub(crate) fn cover(path: &super::path::WritePath<'_>, loaded: &Loaded) -> Result<(), TxnError> {
    if loaded.is_empty() || loaded.covered(path.values) {
        return Ok(());
    }
    path.load_files.sync()?;
    path.values.commit(&mut lock(path.manifest))?;
    Ok(())
}

/// Values of one file that follow each other with nothing between them.
#[derive(Debug, Clone, Copy)]
struct Run {
    file: FileId,
    from: u64,
    to: u64,
    /// The last value of the run: a manifest that covers it covers all.
    last: ValuePointer,
}

/// What a write transaction holds of the values its loads put into the
/// value log: the hold that keeps their files, and which bytes of those
/// files are its own.
///
/// No count of live values includes one of them before the transaction's
/// commit, which acquires those that cells of its frames hold then
/// (`log-format.md` LG16). The others are dead space: the copies that
/// frames held which were made again since, and the values of nodes the
/// transaction wrote over or took out. When this is dropped, by a
/// rollback or with the transaction, all of them are.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    hold: ValueHold,
    /// In the order of their appends: by file, and in a file by offset.
    runs: Vec<Run>,
}

impl Loaded {
    fn note(&mut self, pointer: ValuePointer) {
        let (file, from) = (pointer.file(), pointer.offset());
        let to = pointer.end(FormatVersion::CURRENT);
        match self.runs.last_mut() {
            Some(run) if run.file == file && run.to == from => {
                (run.to, run.last) = (to, pointer);
            }
            _ => self.runs.push(Run {
                file,
                from,
                to,
                last: pointer,
            }),
        }
    }

    /// Whether no load of the transaction has put a value into the value
    /// log.
    pub(crate) fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Whether `pointer` names a value a load of the transaction appended.
    pub(crate) fn owns(&self, pointer: &ValuePointer) -> bool {
        let place = (pointer.file(), pointer.offset());
        let after = self
            .runs
            .partition_point(|run| (run.file, run.from) <= place);
        let run = after.checked_sub(1).map(|at| &self.runs[at]);
        run.is_some_and(|run| run.file == pointer.file() && pointer.offset() < run.to)
    }

    /// Whether a durable manifest covers every value of the transaction's
    /// loads.
    pub(crate) fn covered(&self, values: &ValueStore) -> bool {
        self.runs.iter().all(|run| values.covers(run.last))
    }

    /// How many runs of values the transaction keeps: one for each stretch
    /// of a file that no other transaction's load appended into.
    #[cfg(test)]
    pub(crate) fn runs(&self) -> usize {
        self.runs.len()
    }
}

/// The bit the names of a load's values begin with, as those of a scope's
/// new values do (`lignin_tree::value`): no address of the log has it.
const NEW_VALUE: u64 = 1 << 63;

/// The values of the cells a load builds. A value out of line is kept from
/// when its node arrives until its cell gets its page, and is appended to
/// the value log then, for that page: the cell is written with the
/// pointer.
pub(super) struct Placing<'a> {
    pub(super) values: &'a ValueStore,
    pub(super) manifest: &'a Mutex<ManifestStore>,
    pub(super) files: &'a LoadFiles,
    pub(super) loaded: &'a mut Loaded,
    pub(super) policy: InlinePolicy,
    /// The values of the nodes whose cells are in no page yet, each with
    /// the place of its node among the entries of the batch.
    held: HashMap<ValueId, (u32, Box<[u8]>)>,
    next: u64,
    /// The place of the node that is handed to the loader, among the
    /// entries of the batch.
    pub(super) entry: u32,
    /// Where each value out of line went, by the place of its node.
    pub(super) stored: Vec<(u32, StoredValue)>,
    /// Why the value log refused a value.
    pub(super) failed: Option<TxnError>,
}

impl<'a> Placing<'a> {
    pub(super) fn new(
        values: &'a ValueStore,
        manifest: &'a Mutex<ManifestStore>,
        files: &'a LoadFiles,
        loaded: &'a mut Loaded,
        policy: InlinePolicy,
    ) -> Self {
        Self {
            values,
            manifest,
            files,
            loaded,
            policy,
            held: HashMap::new(),
            next: 0,
            entry: 0,
            stored: Vec::new(),
            failed: None,
        }
    }

    fn refuse(&mut self, error: impl Into<TxnError>) -> Refusal {
        let error = error.into();
        let said = error.to_string();
        self.failed.get_or_insert(error);
        said.into()
    }
}

/// A load calls [`LoadValues::hold`] and [`LoadValues::place`] and nothing
/// else: it carries no cell and takes none out.
impl Values for Placing<'_> {
    fn released(&mut self, _: ValuePlace, _: PageId) {
        self.refuse(TxnError::Damaged("a load released a value"));
    }

    fn placed(&mut self, _: PendingValue, _: PageId) {
        self.refuse(TxnError::Damaged("a load left a value pending"));
    }

    fn carried(&mut self, place: ValuePlace, _: PageId, _: PageId) -> PendingValue {
        self.refuse(TxnError::Damaged("a load carried a cell"));
        self.next += 1;
        PendingValue::new((NEW_VALUE | self.next).to_le_bytes(), place.value_len())
            .expect("a name that is not zero, and the length of a value out of line")
    }
}

impl LoadValues for Placing<'_> {
    fn hold(&mut self, value: &[u8]) -> Result<Option<PendingValue>, Refusal> {
        if self.policy.is_inline(value.len()) {
            return Ok(None);
        }
        let len = checked_value_len(value.len()).map_err(|error| self.refuse(error))?;
        self.next += 1;
        let name = (NEW_VALUE | self.next).to_le_bytes();
        self.held.insert(name, (self.entry, value.into()));
        let pending = PendingValue::new(name, len).map_err(|error| self.refuse(error))?;
        Ok(Some(pending))
    }

    fn place(&mut self, pending: PendingValue, page: PageId) -> Result<ValuePlace, Refusal> {
        let Some((entry, value)) = self.held.remove(&pending.id()) else {
            return Err(self.refuse(TxnError::Damaged("a load placed a value it was not given")));
        };
        let (values, hold) = (self.values, &mut self.loaded.hold);
        let appended = (self.files).append(values, self.manifest, hold, page, &value);
        let pointer = appended.map_err(|error| self.refuse(error))?;
        self.loaded.note(pointer);
        let stored = StoredValue {
            pointer,
            owner: page,
        };
        self.stored.push((entry, stored));
        Ok(ValuePlace::Placed(pointer))
    }
}

/// The hashes of one node of a load, as its change set states them. The
/// loader asks for each hash of a node when the node arrives, and a change
/// set states them all: the node's own in the link of its parent, which
/// comes after it.
pub(super) struct Stated<'a> {
    /// The node's key-value hash.
    pub(super) kv: &'a Hash,
    /// The node's hash, or zeros for the root, whose hash no node of its
    /// Merk states and no cell holds.
    pub(super) own: Hash,
}

impl HashRules for Stated<'_> {
    fn value_hash(
        &mut self,
        _: NodeRef<'_>,
        _: &[u8],
        _: &[Option<Hash>],
    ) -> Result<Hash, Refusal> {
        Err("a load states the value hash of every node".into())
    }

    fn kv_hash(&mut self, _: &[u8], _: &Hash) -> Result<Hash, Refusal> {
        Ok(*self.kv)
    }

    fn node_hash(&mut self, _: NodeRef<'_>) -> Result<Hash, Refusal> {
        Ok(self.own)
    }
}

/// The tree pages of a transaction as a load writes them: every cell that
/// goes into a page is noted with its handle, by the place its key has in
/// the change set that is being loaded.
///
/// A load writes the pages it started and no other, a fragment in one
/// write, and its cells take the slots of a page in order
/// (`lignin_tree::bulk`, "What the callers do"). The cells of one write
/// are of one Merk: no Merk is given to the loader inside its host.
pub(super) struct LoadPages<'a, 's> {
    pub(super) tree: &'a mut TreePages<'s>,
    /// How many cells each page the load fills holds.
    cells: HashMap<PageId, usize>,
    /// The nodes of the change set that is being loaded, by key.
    pub(super) keys: &'a HashMap<&'a [u8], usize>,
    /// The handle of each node of that change set whose cell is in a page.
    pub(super) handles: Vec<Option<NodeHandle>>,
}

impl<'a, 's> LoadPages<'a, 's> {
    pub(super) fn new(tree: &'a mut TreePages<'s>, keys: &'a HashMap<&'a [u8], usize>) -> Self {
        Self {
            tree,
            cells: HashMap::new(),
            keys,
            handles: Vec::new(),
        }
    }

    /// Goes on to the next change set: `keys` has its nodes. Returns the
    /// handles of the nodes of the one before.
    pub(super) fn next_merk(
        &mut self,
        keys: &'a HashMap<&'a [u8], usize>,
    ) -> Vec<Option<NodeHandle>> {
        self.keys = keys;
        std::mem::replace(&mut self.handles, vec![None; keys.len()])
    }
}

impl Pages for LoadPages<'_, '_> {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        self.tree.read(page, read)
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        let from = self.cells.get(&page).copied().unwrap_or(0);
        let (keys, handles) = (self.keys, &mut self.handles);
        let mut to = from;
        let changed = self.tree.write(page, |nodes| {
            let changed = change(nodes)?;
            let view = nodes.view();
            while let Some(node) = SlotId::from_index(to).and_then(|slot| view.node(slot)) {
                if let Some(place) = keys.get(node.key()) {
                    handles[*place] = Some(node.handle());
                }
                to += 1;
            }
            Ok(changed)
        })?;
        self.cells.insert(page, to);
        Ok(changed)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        self.tree.create()
    }

    fn host_is_behind(&mut self, host: lignin_format::CellAddress) -> bool {
        self.tree.host_is_behind(host)
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        self.tree.free(page)
    }
}

/// The error of a load as an operation of a transaction returns it.
/// `failed` is why the value log refused a value, if it did.
pub(super) fn load_error(error: LoadError, failed: Option<TxnError>, subtree: Subtree) -> TxnError {
    let not_whole = |fault| BatchError::NotWhole { subtree, fault }.into();
    match error {
        LoadError::Pages(error) => error.into(),
        LoadError::Node(error) => error.into(),
        LoadError::Value(_) => failed.unwrap_or(TxnError::Damaged("a load refused a value")),
        // The batch was checked before: these say that the check let a
        // change set through that is no tree.
        LoadError::NotLoaded { .. } | LoadError::Shape(_) => not_whole(WholeFault::ChildLater),
        LoadError::Unfinished { .. } => not_whole(WholeFault::NotBelowRoot),
        LoadError::Slot { .. } => TxnError::Damaged("a page of a load was written beside it"),
        LoadError::NoRoot { .. } | LoadError::Rule(_) => {
            TxnError::Damaged("a load asked for a hash its change set states")
        }
        LoadError::Fill { .. } | LoadError::Failed => {
            TxnError::Damaged("a load went on after it had failed")
        }
    }
}
