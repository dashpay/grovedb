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
    cache::{ExclusiveLatch, ValueId},
    load::FRAME_FORMAT,
    values::ValueChange,
};

use super::{
    error::ValueError,
    policy::InlinePolicy,
    read::{pending_bytes, ValueBuf, ValueSource},
};
use crate::{
    fragment::{Pages, ValueWrite, Values},
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
/// It holds no page and no latch, and reads the store only for the value
/// of a carried cell, when that value is read or logged. A clone is what a
/// savepoint keeps of it: the bytes are shared, not copied.
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
        let pending = match node.value() {
            CellValue::Inline(bytes) => return Ok(bytes),
            CellValue::OutOfLine(pointer) => return source.placed(pointer, page, buf),
            CellValue::Pending(pending) => pending,
        };
        let held = self
            .pages
            .get(&page)
            .and_then(|held| held.get(&pending.id()));
        match held {
            Some(held) if held.value_len() == pending.value_len() as usize => {
                held.read(source, buf)
            }
            None if !is_new(pending.id()) => source.pending(page, pending, buf),
            _ => Err(ValueError::NoValue { page }),
        }
    }

    /// The pages whose cells hold pending values that the scope's commit
    /// writes, in ascending order.
    pub fn pages(&self) -> impl Iterator<Item = PageId> + '_ {
        self.pages.keys().copied()
    }

    /// How many such values the cells of `page` hold.
    pub fn pending_in(&self, page: PageId) -> usize {
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
            check_cells(page, &cells, held)?;
            let mut logged = Vec::with_capacity(held.len());
            for (slot, pending) in cells {
                let Some(value) = held.get(&pending.id()) else {
                    continue;
                };
                let bytes = match value {
                    Source::Bytes(bytes) => Arc::clone(bytes),
                    carried => Arc::from(carried.read(source, &mut buf)?),
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

    /// Puts the bytes of the scope's values of a page with its frame, each
    /// under the name its cell holds, and takes off what no cell names:
    /// `latch` is on the scope's copy of the page.
    ///
    /// A commit calls this for every tree page it installs, after
    /// [`Self::log_values`]: from the commit on, a reader finds a pending
    /// value with the frame of its page (`transactions.md` FL7). The bytes
    /// of a value that an earlier commit left pending in the page came
    /// with the copy and stay.
    ///
    /// On an error the frame is as it was.
    pub fn attach(&self, latch: &mut ExclusiveLatch<'_>) -> Result<(), ValueError> {
        let view = TreePage::from_validated(latch, FRAME_FORMAT)?;
        let page = view.id();
        let cells: Vec<(SlotId, PendingValue)> = view.pending_values().collect();
        let none = HashMap::new();
        let held = self.pages.get(&page).unwrap_or(&none);
        check_cells(page, &cells, held)?;
        let mut own = Vec::with_capacity(held.len());
        for (_, pending) in &cells {
            let name = pending.id();
            match held.get(&name) {
                Some(Source::Bytes(bytes)) => own.push((name, Arc::clone(bytes))),
                Some(_) => return Err(ValueError::NoValue { page }),
                None => drop(pending_bytes(latch.attached(name), *pending, page)?),
            }
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

/// Checks the pending values that the cells of `page` hold against the
/// values `held` for it: each value of `held` is held by one cell, which
/// states its length, and every other pending value has a name that an
/// earlier commit gave.
fn check_cells(
    page: PageId,
    cells: &[(SlotId, PendingValue)],
    held: &HashMap<ValueId, Source>,
) -> Result<(), ValueError> {
    let mut names = HashSet::with_capacity(cells.len());
    let mut own = 0;
    for (_, pending) in cells {
        let name = pending.id();
        match held.get(&name) {
            Some(value) if value.value_len() == pending.value_len() as usize => own += 1,
            None if !is_new(name) => {}
            _ => return Err(ValueError::NoValue { page }),
        }
        if !names.insert(name) {
            return Err(ValueError::Shared { page });
        }
    }
    if own != held.len() {
        return Err(ValueError::NoCell { page });
    }
    Ok(())
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
                let owner = from;
                Some(Source::Stored { pointer, owner })
            }
            ValuePlace::Pending(pending) => self.take(from, pending.id()).or_else(|| {
                let page = from;
                (!is_new(pending.id())).then_some(Source::Inherited { page, pending })
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
