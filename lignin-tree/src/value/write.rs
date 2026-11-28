//! The values of the tree cells that one write scope holds out of line.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use lignin_format::{
    tree_page::{CellValue, TreePage},
    value_log::{checked_value_len, PendingValue, ValuePlace, ValuePointer},
    wal::GroupWriter,
    CellAddress, PageId, SlotId,
};
use lignin_store::{
    cache::{ExclusiveLatch, PageRef, ValueId},
    load::FRAME_FORMAT,
    values::ValueChange,
};

use super::{
    error::ValueError,
    policy::InlinePolicy,
    read::{ValueBuf, ValueSource},
};
use crate::{
    fragment::{Pages, ValueWrite, Values},
    hash,
    node::{NodeError, NodeRef},
};

/// The bit that the names of a scope's new values begin with: the address
/// of a value in the log has it only in a log of more than 2^63 bytes.
const NEW_VALUE: u64 = 1 << 63;

/// Whether `name` is one that a scope gave a value of its own. Every other
/// name is the address of a value in the log, which an earlier commit gave.
fn is_new(name: ValueId) -> bool {
    u64::from_le_bytes(name) & NEW_VALUE != 0
}

/// Where the bytes of a pending value of a scope are.
#[derive(Debug, Clone)]
enum Source {
    /// In memory.
    Bytes(Arc<[u8]>),
    /// In the value log, at the pointer the cell held before it was
    /// carried, in a record that names `owner`.
    Stored {
        pointer: ValuePointer,
        owner: PageId,
    },
    /// With the frame of the committed page `page`, under the name
    /// `pending`: the value was pending there when its cell was carried.
    Inherited { page: PageId, pending: PendingValue },
}

impl Source {
    fn value_len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Stored { pointer, .. } => pointer.value_len() as usize,
            Self::Inherited { pending, .. } => pending.value_len() as usize,
        }
    }

    /// The value's bytes, read from where they are.
    fn read<'a>(
        &'a self,
        source: &ValueSource<'_>,
        buf: &'a mut ValueBuf,
    ) -> Result<&'a [u8], ValueError> {
        match self {
            Self::Bytes(bytes) => Ok(bytes),
            Self::Stored { pointer, owner } => source.placed(*pointer, *owner, buf),
            Self::Inherited { page, pending } => source.pending(*page, *pending, buf),
        }
    }
}

/// The values that a commit's group repeats because their cells or entries
/// were carried to another page (`value-log-format.md` O5, `log-format.md`
/// LG23): how many, and their bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CarriedValues {
    /// The values repeated.
    pub values: u64,
    /// Their bytes.
    pub bytes: u64,
}

impl CarriedValues {
    pub(crate) fn add(&mut self, len: usize) {
        self.values += 1;
        self.bytes += len as u64;
    }
}

/// The values that the tree cells of one write scope hold out of line and
/// that the scope's commit writes: the tree side of what
/// [`Private`](crate::flat::Private) is for flat leaves.
///
/// It is what a scope hands to
/// [`Fragments::apply`](crate::fragment::Fragments::apply) as its
/// [`Values`]. From what it is told there it keeps
///
/// - every pending value that the scope gave a cell, by the page that owns
///   the value and the eight bytes that name it: a new value, and the value
///   of a cell that was carried to another page
/// - every value pointer a cell stopped holding
///
/// and at the commit it writes those values into the log group, gives each
/// cell the name its value has there, and reports the pointers. See the
/// [module documentation](super) for the steps of a commit and for what
/// the caller keeps.
///
/// A value that was pending in a page when the scope copied it, and whose
/// cell has stayed there, is not the scope's: an earlier commit logged it
/// for that page, and its bytes stay with the frame.
///
/// It holds no page and no latch. [`Self::log_values`] reads the store
/// only for the values of carried cells, and [`Self::read`] as any read of
/// a value does. A clone is what a savepoint keeps of it: the bytes are
/// shared, not copied. A pending value that [`Self::write`] handed out
/// after the clone was taken is given up with what the scope goes back
/// over: the names start again from the clone's.
#[derive(Debug, Clone, Default)]
pub struct TreeValues {
    policy: InlinePolicy,
    pages: BTreeMap<PageId, HashMap<ValueId, Source>>,
    /// New values that no cell holds yet.
    unplaced: HashMap<ValueId, Arc<[u8]>>,
    released: Vec<ValuePointer>,
    next: u64,
    /// The page of the first cell that was said to hold a pending value of
    /// the scope's own whose bytes are not kept here.
    lost: Option<PageId>,
    carried: CarriedValues,
}

impl TreeValues {
    /// The values of a scope that has written nothing, under `policy`.
    pub fn new(policy: InlinePolicy) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    /// The policy new values are written under.
    pub const fn policy(&self) -> InlinePolicy {
        self.policy
    }

    /// What a node whose value is `value` is written with
    /// ([`NodeWrite::value`](crate::fragment::NodeWrite::value)): the value
    /// itself up to the inline threshold, and above it a pending value,
    /// whose bytes are kept here until a write puts it into a cell
    /// ([`Values::placed`]).
    ///
    /// A pending value that no write placed, because its write was refused
    /// or never made, is given up with [`Self::forget`].
    pub fn write<'a>(&mut self, value: &'a [u8]) -> Result<ValueWrite<'a>, ValueError> {
        if self.policy.is_inline(value.len()) {
            return Ok(ValueWrite::Inline(value));
        }
        let len =
            checked_value_len(value.len()).map_err(|_| ValueError::TooLong { len: value.len() })?;
        let name = self.name();
        self.unplaced.insert(name, Arc::from(value));
        Ok(ValueWrite::Pending(PendingValue::new(name, len)?))
    }

    /// Gives up a pending value of [`Self::write`] that no cell holds.
    /// Returns whether there was one.
    pub fn forget(&mut self, pending: PendingValue) -> bool {
        self.unplaced.remove(&pending.id()).is_some()
    }

    /// The value of `node`, a node of a page the scope has copied or made.
    ///
    /// A value in the cell is returned from the frame. A value in the value
    /// log is read as [`read_value`](super::read_value) reads it: the copy
    /// holds the pointers its page had when it was copied, and no page step
    /// has changed them since (`transactions.md` PV7). A pending value of
    /// the scope's own is returned from the bytes kept here, or read from
    /// where its cell was carried from. A value that was pending in the
    /// page when the scope copied it is with the frame of the committed
    /// page.
    ///
    /// A page the scope has not copied is a committed page, and
    /// [`read_value`](super::read_value) reads its values.
    pub fn read<'a>(
        &'a self,
        source: &ValueSource<'_>,
        node: NodeRef<'a>,
        buf: &'a mut ValueBuf,
    ) -> Result<&'a [u8], ValueError> {
        let page = node.address().page();
        match node.value() {
            CellValue::Inline(bytes) => Ok(bytes),
            CellValue::OutOfLine(pointer) => source.placed(pointer, page, buf),
            CellValue::Pending(pending) => self.read_pending(source, pending, page, buf),
        }
    }

    /// The value of `node`, a node of any page the scope is shown.
    ///
    /// For a page the scope has copied or made, `committed` is `None` and
    /// this is [`Self::read`]. For a committed page that the scope has
    /// not copied, `committed` is the frame the caller read the node
    /// from, which it holds, and the value is read as
    /// [`read_value`](super::read_value) reads it, from that frame.
    ///
    /// [`Self::read`] is not for such a page. It looks for the bytes of a
    /// pending value with the frame the store has for the page when it
    /// asks, and write-back can have placed the value since the node was
    /// read and put a frame there that has the pointer and not the
    /// bytes: the scope holds no private copy of the page, so it does not
    /// itself hold that step back (`transactions.md` PV7).
    pub fn read_shown<'a>(
        &'a self,
        source: &ValueSource<'_>,
        node: NodeRef<'a>,
        committed: Option<&'a PageRef<'_>>,
        buf: &'a mut ValueBuf,
    ) -> Result<&'a [u8], ValueError> {
        match committed {
            Some(frame) => source.framed(frame, node, buf),
            None => self.read(source, node, buf),
        }
    }

    /// The value at `place`, which a cell of `page` holds, in a page as the
    /// scope has it: the read of [`Self::read`] for a caller that has the
    /// place of a value and not its node, as an evaluation of stale hashes
    /// has between two page steps ([`hash::ValueReader`]).
    pub fn read_at<'a>(
        &'a self,
        source: &ValueSource<'_>,
        place: ValuePlace,
        page: PageId,
        buf: &'a mut ValueBuf,
    ) -> Result<&'a [u8], ValueError> {
        match place {
            ValuePlace::Placed(pointer) => source.placed(pointer, page, buf),
            ValuePlace::Pending(pending) => self.read_pending(source, pending, page, buf),
        }
    }

    /// The values of the scope as an evaluation of stale hashes reads
    /// them, with `source` for those that are not in the scope's memory.
    pub fn reader<'a, 's>(&'a self, source: &'a ValueSource<'s>) -> ScopeReader<'a, 's> {
        ScopeReader {
            values: self,
            source,
            buf: ValueBuf::new(),
        }
    }

    /// The bytes of the pending value `pending`, which a cell of `page`
    /// holds: the scope's own, or one that an earlier commit left with the
    /// frame of the committed page.
    fn read_pending<'a>(
        &'a self,
        source: &ValueSource<'_>,
        pending: PendingValue,
        page: PageId,
        buf: &'a mut ValueBuf,
    ) -> Result<&'a [u8], ValueError> {
        match own_value(self.pages.get(&page), page, pending)? {
            Some(own) => own.read(source, buf),
            None => source.pending(page, pending, buf),
        }
    }

    /// The pages whose cells hold pending values that the scope's commit
    /// writes, in ascending order.
    pub fn pages(&self) -> impl Iterator<Item = PageId> + '_ {
        self.pages.keys().copied()
    }

    /// How many such values the cells of `page` hold.
    #[cfg(test)]
    pub(super) fn pending_in(&self, page: PageId) -> usize {
        self.pages.get(&page).map_or(0, HashMap::len)
    }

    /// The value pointers that cells of the scope stopped holding, in
    /// ascending order of file and offset: what the commit's group lists as
    /// released (`log-format.md` LG14), merged with those of the flat
    /// leaves.
    pub fn released_pointers(&mut self) -> &[ValuePointer] {
        self.released
            .sort_unstable_by_key(|pointer| (pointer.file(), pointer.offset()));
        &self.released
    }

    /// What the scope's commit does to the counts of live values: one fewer
    /// for every pointer a cell stopped holding. It acquires nothing: a new
    /// value has no pointer before write-back places it (`transactions.md`
    /// PV6, FL7).
    ///
    /// A commit applies one change, with the pointers its flat leaves
    /// released added to this one.
    pub fn change(&self) -> ValueChange {
        let mut change = ValueChange::new();
        for &pointer in &self.released {
            change.release(pointer);
        }
        change
    }

    /// Fails if a write told of a pending value of the scope's own whose
    /// bytes are not kept here: one that [`Self::write`] did not make.
    pub fn check(&self) -> Result<(), ValueError> {
        match self.lost {
            Some(page) => Err(ValueError::NoValue { page }),
            None => Ok(()),
        }
    }

    /// Writes every value of the scope into `group`, each for the page of
    /// its cell, and gives each cell the pending value that names the
    /// value's place in that group (`log-format.md` LG20 to LG22). The
    /// pages come in ascending order, and the values of a page in slot
    /// order.
    ///
    /// A value whose cell was carried is repeated: its bytes go into the
    /// group again, with the page the cell is in now as their owner
    /// (LG23). They are read here, once: from the value log if the cell
    /// held a value pointer, and from the frame of the committed page it
    /// left if the value was pending there.
    ///
    /// The cells are changed through `pages`, so each new name is told to
    /// its page's sink as the record that puts it there (record 21 of
    /// `page-delta.md`, PD16).
    ///
    /// A commit calls this before it writes its pages into the group. A
    /// group is written for one place (LG22): a commit that writes its
    /// group again for another place calls this again, and every call
    /// writes all of the scope's values and names them anew. After a call
    /// that failed the scope is given up, as after a write that failed.
    pub fn log_values<P: Pages>(
        &mut self,
        pages: &mut P,
        source: &ValueSource<'_>,
        group: &mut GroupWriter<'_>,
    ) -> Result<(), ValueError> {
        self.check()?;
        let mut buf = ValueBuf::new();
        for (&page, held) in &mut self.pages {
            let cells = pages.read(page, |view| {
                view.tree_page().pending_values().collect::<Vec<_>>()
            })?;
            let mut logged = Vec::with_capacity(held.len());
            for (slot, _, value) in own_values(page, &cells, held)? {
                let bytes = match value {
                    Source::Bytes(bytes) => Arc::clone(bytes),
                    carried => {
                        let bytes: Arc<[u8]> = Arc::from(carried.read(source, &mut buf)?);
                        self.carried.add(bytes.len());
                        bytes
                    }
                };
                logged.push((slot, group.value(page, &bytes)?, bytes));
            }
            held.clear();
            pages.write(page, |nodes| {
                for &(slot, named, _) in &logged {
                    let address = CellAddress::new(page, slot);
                    nodes
                        .node_mut(slot)
                        .ok_or(NodeError::NoNode { address })?
                        .set_value(CellValue::Pending(named))?;
                }
                Ok(())
            })?;
            for (_, named, bytes) in logged {
                held.insert(named.id(), Source::Bytes(bytes));
            }
        }
        Ok(())
    }

    /// The values of carried cells that [`Self::log_values`] has repeated.
    /// Each is counted by the first call that writes it: a call for another
    /// place finds its bytes in memory and counts it no more.
    pub const fn values_carried(&self) -> CarriedValues {
        self.carried
    }

    /// Puts the bytes of the scope's values of a page with its frame, each
    /// under the name its cell holds, and takes off what no cell names:
    /// `latch` is on the scope's copy of the page.
    ///
    /// A commit calls this for every tree page it installs, after
    /// [`Self::log_values`]: from the commit on, a reader finds a pending
    /// value with the frame of its page (`transactions.md` FL7). The bytes
    /// of a value that an earlier commit left pending in the page came
    /// with the copy and stay, as they are.
    ///
    /// For a page of [`Self::pages`] it fails only for what
    /// [`Self::log_values`] fails for as well, so a commit that has logged
    /// its values, and has changed no page since, installs those pages
    /// without an error from here. For any other page it fails only if a
    /// cell holds a pending value under a name of a scope's own, which no
    /// write that this was told of gave it. On an error the frame is as it
    /// was.
    pub fn attach(&self, latch: &mut ExclusiveLatch<'_>) -> Result<(), ValueError> {
        let view = TreePage::from_validated(latch, FRAME_FORMAT)?;
        let page = view.id();
        let cells: Vec<(SlotId, PendingValue)> = view.pending_values().collect();
        let none = HashMap::new();
        let held = self.pages.get(&page).unwrap_or(&none);
        let mut own = Vec::with_capacity(held.len());
        for (_, name, value) in own_values(page, &cells, held)? {
            let Source::Bytes(bytes) = value else {
                return Err(ValueError::NoValue { page });
            };
            own.push((name, Arc::clone(bytes)));
        }
        let named = |name: &ValueId| cells.iter().any(|(_, pending)| pending.id() == *name);
        let stale: Vec<ValueId> = latch
            .attachments()
            .map(|(name, _)| name)
            .filter(|name| !named(name))
            .collect();
        for name in stale {
            latch.detach(name);
        }
        for (name, bytes) in own {
            latch.attach(name, bytes);
        }
        Ok(())
    }

    fn name(&mut self) -> ValueId {
        self.next += 1;
        (NEW_VALUE | self.next).to_le_bytes()
    }

    fn take(&mut self, page: PageId, name: ValueId) -> Option<Source> {
        let held = self.pages.get_mut(&page)?;
        let value = held.remove(&name);
        if held.is_empty() {
            self.pages.remove(&page);
        }
        value
    }

    fn hold(&mut self, page: PageId, name: ValueId, source: Source) {
        self.pages.entry(page).or_default().insert(name, source);
    }
}

/// The value of a scope that a cell of `page` names with `pending`, among
/// the values `held` for the page: `None` for a value that an earlier
/// commit left pending, which has a name that commit gave. A name of a
/// scope's own that is not held, and a cell that states another length
/// than its value has, are errors.
fn own_value(
    held: Option<&HashMap<ValueId, Source>>,
    page: PageId,
    pending: PendingValue,
) -> Result<Option<&Source>, ValueError> {
    match held.and_then(|held| held.get(&pending.id())) {
        Some(value) if value.value_len() == pending.value_len() as usize => Ok(Some(value)),
        None if !is_new(pending.id()) => Ok(None),
        _ => Err(ValueError::NoValue { page }),
    }
}

/// The values `held` for `page`, each with the slot of the cell that holds
/// it and its name, in the order of `cells`, the pending values the cells
/// of the page hold. Each value of `held` is held by one cell, and no two
/// cells hold one name.
fn own_values<'h>(
    page: PageId,
    cells: &[(SlotId, PendingValue)],
    held: &'h HashMap<ValueId, Source>,
) -> Result<Vec<(SlotId, ValueId, &'h Source)>, ValueError> {
    let mut names = HashSet::with_capacity(cells.len());
    let mut own = Vec::with_capacity(held.len());
    for &(slot, pending) in cells {
        if let Some(value) = own_value(Some(held), page, pending)? {
            own.push((slot, pending.id(), value));
        }
        if !names.insert(pending.id()) {
            return Err(ValueError::Shared { page });
        }
    }
    if own.len() != held.len() {
        return Err(ValueError::NoCell { page });
    }
    Ok(own)
}

/// The values of one write scope, read by place: what
/// [`Hashes::evaluate`](crate::hash::Hashes::evaluate) and the calls beside
/// it are given for the values that are not in their cells
/// ([`TreeValues::reader`]).
#[derive(Debug)]
pub struct ScopeReader<'a, 's> {
    values: &'a TreeValues,
    source: &'a ValueSource<'s>,
    buf: ValueBuf,
}

impl hash::ValueReader for ScopeReader<'_, '_> {
    fn read(
        &mut self,
        place: ValuePlace,
        page: PageId,
        into: &mut Vec<u8>,
    ) -> Result<(), hash::Refusal> {
        let bytes = (self.values).read_at(self.source, place, page, &mut self.buf)?;
        into.extend_from_slice(bytes);
        Ok(())
    }
}

impl Values for TreeValues {
    fn released(&mut self, place: ValuePlace, page: PageId) {
        match place {
            ValuePlace::Placed(pointer) => self.released.push(pointer),
            ValuePlace::Pending(pending) => {
                self.take(page, pending.id());
            }
        }
    }

    fn placed(&mut self, pending: PendingValue, page: PageId) {
        match self.unplaced.remove(&pending.id()) {
            Some(bytes) => self.hold(page, pending.id(), Source::Bytes(bytes)),
            None => {
                self.lost.get_or_insert(page);
            }
        }
    }

    fn carried(&mut self, place: ValuePlace, from: PageId, to: PageId) -> PendingValue {
        let source = match place {
            ValuePlace::Placed(pointer) => {
                self.released.push(pointer);
                Some(Source::Stored {
                    pointer,
                    owner: from,
                })
            }
            ValuePlace::Pending(pending) => self.take(from, pending.id()).or_else(|| {
                let inherited = Source::Inherited {
                    page: from,
                    pending,
                };
                (!is_new(pending.id())).then_some(inherited)
            }),
        };
        let name = self.name();
        match source {
            Some(source) => self.hold(to, name, source),
            None => {
                self.lost.get_or_insert(from);
            }
        }
        PendingValue::new(name, place.value_len())
            .expect("a name that is not zero, and the length of a value out of line")
    }
}
