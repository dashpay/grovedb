//! The write set of a write transaction: the ordered list of its writes,
//! and an index of them by record key (`transactions.md` WT2, WT3).

use std::{
    collections::{BTreeMap, HashMap},
    ops::Bound,
};

use lignin_format::{tree_page::Hash, CommitVersion};
use lignin_tree::hash::Stale;

use super::{
    arena::{Arena, ArenaMark, Span},
    error::BatchError,
    key::{in_range, ranges_meet, Column, KeySet, Subtree, Unit},
    write::{
        Batch, HostPut, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite, RootPut, SubtreeWrite,
        ValuePut,
    },
};

/// The position of an entry in the write set. A later write has a higher
/// one.
pub(crate) type EntryId = u32;

/// A link of a written node.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LinkEntry {
    pub(crate) key: Span,
    pub(crate) hash: Span,
    pub(crate) heights: (u8, u8),
    pub(crate) aggregate: Span,
}

/// The value of a written node.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ValueEntry {
    /// The value of the entry before this one for the same key, or with
    /// none before it, the committed value.
    Keep,
    /// The value the write stated.
    Value(Span),
    /// The transaction's own copy of the committed value the entry kept, in
    /// the arena of kept values (WT3, WT7).
    Kept(Span),
}

/// The Merks a written node hosts.
#[derive(Debug, Clone, Copy)]
pub(crate) enum HostedEntry {
    Keep,
    /// The subtrees, 32 bytes each.
    Set(Span),
}

/// A written node.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NodeEntry {
    pub(crate) feature: Span,
    pub(crate) value_hash: Span,
    pub(crate) kv_hash: Span,
    pub(crate) left: Option<LinkEntry>,
    pub(crate) right: Option<LinkEntry>,
    pub(crate) hosted: HostedEntry,
    pub(crate) value: ValueEntry,
    /// The hashes the write left stale.
    pub(crate) stale: Stale,
}

/// What an entry does to its key.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Kind {
    /// A put of a flat record.
    Put(Span),
    /// A delete of a flat record.
    Delete,
    /// A put of a Merk node.
    Node(NodeEntry),
    /// A delete of a Merk node.
    Remove,
    /// A delete of the keys from the entry's key to before `end`.
    Range { end: Option<Span> },
    /// A drop of the entry's subtree: a delete of every key of every
    /// column under it (`transactions.md` ST4, CV9).
    ///
    /// The Merks below go with it, and the drop names none of them. Those
    /// among them that the transaction had written are named in the bytes
    /// of the entry's key, 32 each, and the entry deletes every key of
    /// every column under each: what it wrote there is gone with the drop.
    Dropped,
    /// A clear of the entry's subtree: a delete of every key of its data
    /// column, and of what the transaction wrote below it, as for a drop.
    Cleared,
}

impl Kind {
    /// The columns of its subtree that a drop or a clear deletes as a
    /// whole, and none for any other write.
    fn leaves(&self) -> &'static [Column] {
        match self {
            Self::Dropped => &Column::ALL,
            Self::Cleared => &[Column::Data],
            _ => &[],
        }
    }

    /// Whether the entry deletes keys without naming each: it is among the
    /// ranges of the units it covers, and under no key.
    fn is_range(&self) -> bool {
        matches!(self, Self::Range { .. } | Self::Dropped | Self::Cleared)
    }
}

/// One write (ST5), with its first-write version (WT5).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Entry {
    pub(crate) unit: Unit,
    pub(crate) key: Span,
    pub(crate) kind: Kind,
    /// The latest version the entry's operation read.
    pub(crate) first_write: CommitVersion,
    /// Whether a commit outside the transaction wrote a key of the entry
    /// after `first_write`: the entry is in the overlap set (WT7, CF2).
    pub(crate) met: bool,
    /// The entry before this one for the same record key.
    pub(crate) prev: Option<EntryId>,
}

/// What an operation did to the root of one Merk.
#[derive(Debug, Clone, Copy)]
struct MerkEntry {
    subtree: Subtree,
    /// `None` keeps the root.
    root: Option<Option<Span>>,
    anchored: bool,
    /// Whether the operation gave a root to a Merk that had none in the
    /// transaction's view.
    made: bool,
    nodes: u32,
    removed: u32,
}

/// What an operation did to a subtree as a whole.
#[derive(Debug, Clone, Copy)]
enum SubtreeEntry {
    Enter {
        subtree: Subtree,
        tree_type: Span,
        /// The host's subtree, its key and the position.
        host: Option<(Subtree, Span, u8)>,
    },
    Drop(Subtree),
    Clear(Subtree),
}

/// Where the entries of one operation begin.
#[derive(Debug, Clone, Copy)]
struct Op {
    entries: EntryId,
    merks: u32,
    subtrees: u32,
    /// A number no other operation of the set has, or had.
    serial: u64,
    /// The open scope that made the operation, or zero for an operation
    /// that is the transaction's: its scope has merged (SC6).
    scope: u32,
    /// Whether the transaction's frames show the operation. A scope that
    /// was opened beside another lays its operations out when it merges
    /// (`lignindb::scope`).
    laid: bool,
    /// The first-write version of the operation's entries (WT5).
    first_write: CommitVersion,
    /// The place of the arena before the operation's bytes.
    arena: ArenaMark,
}

/// What the data column of a subtree holds in a write set (ST1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    Nodes,
    Flat,
}

/// The newest write of a transaction that covers a record key (RD3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Own<'a> {
    /// The transaction has no write that covers the key.
    Unwritten,
    /// A delete, or a range delete that contains the key.
    Deleted,
    /// A put of a flat record, with its value.
    Put(&'a [u8]),
    /// A put of a Merk node.
    Node(EntryId),
}

/// The writes of one unit, by key.
#[derive(Debug, Clone, Default)]
struct UnitIndex {
    /// The newest entry of each key that was put or deleted.
    keys: BTreeMap<Box<[u8]>, EntryId>,
    /// The range deletes, oldest first.
    ranges: Vec<EntryId>,
}

/// A place in a write set: the writes after it can be taken out again, which
/// is what a savepoint and a scope are (WT8, SC6).
///
/// It is a place of the set for as long as every write before it stays. A
/// rollback behind it ends it, and it is then refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    entries: usize,
    ops: usize,
    merks: usize,
    subtrees: usize,
    arena: ArenaMark,
    /// The serial of the operation before the mark, or zero.
    last: u64,
    /// How many changes the transaction's pending marks had taken (SC9):
    /// the changes after the mark are undone with the writes after it.
    pub(crate) pending: usize,
}

/// The write set (WT2): every write the transaction made, in order, with
/// its bytes in one arena, and beside it an index by record key.
///
/// The list is the truth (WT3). The index is derived from it: it gives the
/// newest write that covers a key, and the written keys of a unit in order.
#[derive(Debug, Clone, Default)]
pub(crate) struct WriteSet {
    entries: Vec<Entry>,
    ops: Vec<Op>,
    merks: Vec<MerkEntry>,
    subtrees: Vec<SubtreeEntry>,
    /// The drops and the clears among the entries, oldest first.
    leaving: Vec<EntryId>,
    /// How many times `leaving` changed. It never goes back.
    left: u64,
    arena: Arena,
    /// The copies of committed values that entries kept. They are not taken
    /// out with a mark: an entry from before the mark can have its copy
    /// made after it.
    kept: Arena,
    units: HashMap<Unit, UnitIndex>,
    met: usize,
    /// The serial of the operation that was added last. It never goes back.
    serial: u64,
}

/// The Merks that the nodes of one operation host, a list for each node in
/// the order of the operation's entries. [`WriteSet::batch`] borrows it.
pub(crate) type HostedLists = Vec<Vec<Subtree>>;

/// One operation of the write set as the batch it was.
#[derive(Debug)]
pub(crate) struct OpBatch<'a> {
    nodes: Vec<Vec<NodePut<'a>>>,
    removed: Vec<Vec<&'a [u8]>>,
    merks: Vec<MerkEntry>,
    subtrees: &'a [SubtreeEntry],
    records: Vec<RecordWrite<'a>>,
    arena: &'a Arena,
}

impl<'a> OpBatch<'a> {
    /// The change sets of the Merks.
    pub(crate) fn merks(&'a self) -> Vec<MerkWrite<'a>> {
        (self.merks.iter().zip(&self.nodes).zip(&self.removed))
            .map(|((merk, nodes), removed)| MerkWrite {
                subtree: merk.subtree,
                nodes,
                removed,
                root: match merk.root {
                    None => RootPut::Keep,
                    Some(root) => RootPut::Set(root.map(|key| self.arena.get(key))),
                },
                anchored: merk.anchored,
            })
            .collect()
    }

    /// The writes of flat records.
    pub(crate) fn records(&self) -> &[RecordWrite<'a>] {
        &self.records
    }

    /// What the operation did to subtrees as a whole.
    pub(crate) fn subtrees(&self) -> Vec<SubtreeWrite<'a>> {
        (self.subtrees.iter())
            .map(|entry| match *entry {
                SubtreeEntry::Enter {
                    subtree,
                    tree_type,
                    host,
                } => SubtreeWrite::Enter {
                    subtree,
                    tree_type: self.arena.get(tree_type),
                    host: host.map(|(subtree, key, position)| HostPut {
                        subtree,
                        key: self.arena.get(key),
                        position,
                    }),
                },
                SubtreeEntry::Drop(subtree) => SubtreeWrite::Drop { subtree },
                SubtreeEntry::Clear(subtree) => SubtreeWrite::Clear { subtree },
            })
            .collect()
    }
}

/// The 32 bytes of a hash that was put in the arena.
pub(crate) fn hash_of(bytes: &[u8]) -> &Hash {
    bytes.try_into().expect("a hash was put in whole")
}

impl WriteSet {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether the transaction has made no write.
    pub(crate) fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// How many writes the set holds.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// How many operations added to the set.
    pub(crate) fn ops(&self) -> usize {
        self.ops.len()
    }

    /// The bytes the arenas hold.
    pub(crate) fn bytes(&self) -> usize {
        self.arena.bytes() + self.kept.bytes()
    }

    /// How many entries are in the overlap set.
    pub(crate) fn met(&self) -> usize {
        self.met
    }

    pub(crate) fn entry(&self, id: EntryId) -> &Entry {
        &self.entries[id as usize]
    }

    /// The key of the entry `id`.
    pub(crate) fn key(&self, id: EntryId) -> &[u8] {
        self.arena.get(self.entry(id).key)
    }

    /// The bytes at `span` of the arena of writes.
    pub(crate) fn bytes_at(&self, span: Span) -> &[u8] {
        self.arena.get(span)
    }

    /// The place after the last write.
    pub(crate) fn mark(&self) -> Mark {
        Mark {
            entries: self.entries.len(),
            ops: self.ops.len(),
            merks: self.merks.len(),
            subtrees: self.subtrees.len(),
            arena: self.arena.mark(),
            last: self.ops.last().map_or(0, |op| op.serial),
            pending: 0,
        }
    }

    /// Whether `mark` is a place of the set still: every operation that was
    /// in the set when the mark was given is in it now.
    pub(crate) fn holds(&self, mark: Mark) -> bool {
        match mark.ops.checked_sub(1) {
            None => true,
            Some(last) => (self.ops.get(last)).is_some_and(|op| op.serial == mark.last),
        }
    }

    /// Takes out every write that was made after `mark`, newest first, with
    /// its first-write version and its place in the overlap set (WT8, CF4),
    /// and says whether there was one. A mark that is no place of the set
    /// any more takes nothing out.
    pub(crate) fn rewind(&mut self, mark: Mark) -> bool {
        if !self.holds(mark) || self.ops.len() == mark.ops {
            return false;
        }
        while self.entries.len() > mark.entries {
            let id = (self.entries.len() - 1) as EntryId;
            let entry = self.entries.pop().expect("more entries than the mark");
            if entry.met {
                self.met -= 1;
            }
            for unit in Self::covered(&self.arena, &entry) {
                let index = (self.units.get_mut(&unit)).expect("the unit of an entry is indexed");
                if entry.kind.is_range() {
                    let last = index.ranges.pop();
                    debug_assert_eq!(last, Some(id), "the newest range is the last");
                } else {
                    let key = self.arena.get(entry.key);
                    match entry.prev {
                        Some(prev) => {
                            *index.keys.get_mut(key).expect("the key is indexed") = prev;
                        }
                        None => {
                            index.keys.remove(key);
                        }
                    }
                }
                if index.keys.is_empty() && index.ranges.is_empty() {
                    self.units.remove(&unit);
                }
            }
            if self.leaving.last() == Some(&id) {
                self.leaving.pop();
                self.left += 1;
            }
        }
        self.ops.truncate(mark.ops);
        self.merks.truncate(mark.merks);
        self.subtrees.truncate(mark.subtrees);
        self.arena.rewind(mark.arena);
        if self.ops.is_empty() {
            self.kept.clear();
        }
        true
    }

    /// Takes out every write (WT9).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.ops.clear();
        self.merks.clear();
        self.subtrees.clear();
        self.leaving.clear();
        self.left += 1;
        self.arena.clear();
        self.kept.clear();
        self.units.clear();
        self.met = 0;
    }

    /// The subtrees below the one a drop or a clear names that the
    /// transaction had written, and that the entry covers.
    fn cut<'a>(arena: &'a Arena, entry: &Entry) -> impl Iterator<Item = Subtree> + 'a {
        let cut: &[u8] = match entry.kind.leaves() {
            [] => &[],
            _ => arena.get(entry.key),
        };
        let (subtrees, _) = cut.as_chunks::<{ Subtree::LEN }>();
        subtrees.iter().copied().map(Subtree::new)
    }

    /// The units an entry is indexed in: its own, or for a drop every
    /// column of its subtree, and every column of each subtree it cut.
    fn covered(arena: &Arena, entry: &Entry) -> Vec<Unit> {
        let subtree = entry.unit.subtree;
        let own = match entry.kind.leaves() {
            [] => vec![entry.unit],
            columns => (columns.iter())
                .map(|column| Unit::new(*column, subtree))
                .collect(),
        };
        let below = Self::cut(arena, entry)
            .flat_map(|subtree| Column::ALL.map(|column| Unit::new(column, subtree)));
        own.into_iter().chain(below).collect()
    }

    fn push(&mut self, unit: Unit, key: &[u8], kind: Kind, first_write: CommitVersion) -> EntryId {
        let id = EntryId::try_from(self.entries.len()).expect("fewer than 2^32 writes");
        let mut entry = Entry {
            unit,
            key: self.arena.put(key),
            kind,
            first_write,
            met: false,
            prev: None,
        };
        if kind.is_range() {
            for unit in Self::covered(&self.arena, &entry) {
                self.units.entry(unit).or_default().ranges.push(id);
            }
        } else {
            let index = self.units.entry(unit).or_default();
            entry.prev = match index.keys.get_mut(key) {
                Some(newest) => Some(std::mem::replace(newest, id)),
                None => {
                    index.keys.insert(key.into(), id);
                    None
                }
            };
        }
        self.entries.push(entry);
        id
    }

    fn link(&mut self, link: Option<LinkPut<'_>>) -> Option<LinkEntry> {
        link.map(|link| LinkEntry {
            key: self.arena.put(link.key),
            hash: self.arena.put(link.hash),
            heights: link.heights,
            aggregate: self.arena.put(link.aggregate),
        })
    }

    /// Adds the writes of `batch` as one operation, each with the
    /// first-write version `first_write`: the latest version the operation
    /// read (WT5). Returns the entry of the batch's first node.
    ///
    /// The entries come in the order the batch is applied in: a drop or a
    /// clear is one entry each (CF5), and they come first. Then, in the
    /// order of [`Applied::nodes`](super::Applied), for each Merk its nodes
    /// and then its deletes, and the records last. A subtree that is
    /// entered is no entry: it writes no record. The batch has passed
    /// [`check`](super::check::check).
    ///
    /// `empty` says of each Merk of the batch whether it had no root in the
    /// transaction's view before the operation. `cut` has the subtrees the
    /// transaction had written that are below one the batch drops or
    /// clears: the first of those entries covers them.
    pub(crate) fn add(
        &mut self,
        batch: &Batch<'_>,
        first_write: CommitVersion,
        empty: &[bool],
        cut: &[Subtree],
    ) -> EntryId {
        self.serial += 1;
        self.ops.push(Op {
            entries: self.entries.len() as EntryId,
            merks: self.merks.len() as u32,
            subtrees: self.subtrees.len() as u32,
            serial: self.serial,
            scope: 0,
            laid: true,
            first_write,
            arena: self.arena.mark(),
        });
        for write in batch.subtrees {
            let entry = match *write {
                SubtreeWrite::Enter {
                    subtree,
                    tree_type,
                    host,
                } => SubtreeEntry::Enter {
                    subtree,
                    tree_type: self.arena.put(tree_type),
                    host: host.map(|host| (host.subtree, self.arena.put(host.key), host.position)),
                },
                SubtreeWrite::Drop { subtree } => SubtreeEntry::Drop(subtree),
                SubtreeWrite::Clear { subtree } => SubtreeEntry::Clear(subtree),
            };
            self.subtrees.push(entry);
        }
        let mut cut = cut.concat_bytes();
        for write in batch.subtrees {
            let kind = match write {
                SubtreeWrite::Drop { .. } => Kind::Dropped,
                SubtreeWrite::Clear { .. } => Kind::Cleared,
                SubtreeWrite::Enter { .. } => continue,
            };
            let below = std::mem::take(&mut cut);
            let id = self.push(Unit::data(write.subtree()), &below, kind, first_write);
            self.leaving.push(id);
            self.left += 1;
        }
        let first = self.entries.len() as EntryId;
        for (merk, empty) in batch.merks.iter().zip(empty) {
            let unit = Unit::data(merk.subtree);
            for node in merk.nodes {
                let hosted = match node.hosted {
                    Hosted::Keep => HostedEntry::Keep,
                    Hosted::Set(subtrees) => {
                        let bytes = subtrees.concat_bytes();
                        HostedEntry::Set(self.arena.put(&bytes))
                    }
                };
                let value = match node.value {
                    ValuePut::Keep => ValueEntry::Keep,
                    ValuePut::Value(value) => ValueEntry::Value(self.arena.put(value)),
                };
                let entry = NodeEntry {
                    feature: self.arena.put(node.feature),
                    value_hash: self.arena.put(node.value_hash),
                    kv_hash: self.arena.put(node.kv_hash),
                    left: self.link(node.left),
                    right: self.link(node.right),
                    hosted,
                    value,
                    stale: node.stale,
                };
                self.push(unit, node.key, Kind::Node(entry), first_write);
            }
            for key in merk.removed {
                self.push(unit, key, Kind::Remove, first_write);
            }
            let root = match merk.root {
                RootPut::Keep => None,
                // A Merk that had no root and is left with none is as it
                // was. Applied again over a root that a commit outside made
                // since, the operation leaves that root alone (WT7).
                RootPut::Set(None) if *empty => None,
                RootPut::Set(root) => Some(root.map(|key| self.arena.put(key))),
            };
            self.merks.push(MerkEntry {
                subtree: merk.subtree,
                root,
                anchored: merk.anchored,
                made: *empty && matches!(merk.root, RootPut::Set(Some(_))),
                nodes: merk.nodes.len() as u32,
                removed: merk.removed.len() as u32,
            });
        }
        for record in batch.records {
            match *record {
                RecordWrite::Put { unit, key, value } => {
                    let value = self.arena.put(value);
                    self.push(unit, key, Kind::Put(value), first_write);
                }
                RecordWrite::Delete { unit, key } => {
                    self.push(unit, key, Kind::Delete, first_write);
                }
                RecordWrite::DeleteRange { unit, start, end } => {
                    let end = end.map(|end| self.arena.put(end));
                    self.push(unit, start, Kind::Range { end }, first_write);
                }
            }
        }
        first
    }

    /// Says that the open scope `scope` made the operation that was added
    /// last, and whether the transaction's frames show it.
    pub(crate) fn tag_last(&mut self, scope: u32, laid: bool) {
        let op = self.ops.last_mut().expect("an operation was added");
        (op.scope, op.laid) = (scope, laid);
    }

    /// Whether the transaction's frames show the operation `op`. Frames
    /// that are made from the set pass over an operation they do not show.
    pub(crate) fn laid(&self, op: usize) -> bool {
        self.ops[op].laid
    }

    /// The operations of the open scope `scope`, oldest first.
    pub(crate) fn ops_of(&self, scope: u32) -> Vec<usize> {
        let ops = self.ops.iter().enumerate();
        ops.filter_map(|(at, op)| (op.scope == scope).then_some(at))
            .collect()
    }

    /// Makes the operations of the scope `scope` the transaction's (SC6),
    /// all of them laid out.
    pub(crate) fn merge_scope(&mut self, scope: u32) {
        for op in self.ops.iter_mut().filter(|op| op.scope == scope) {
            (op.scope, op.laid) = (0, true);
        }
    }

    /// The operations of the open scope `scope` are laid out: the
    /// transaction's frames show them from here on.
    pub(crate) fn lay_scope(&mut self, scope: u32) {
        for op in self.ops.iter_mut().filter(|op| op.scope == scope) {
            op.laid = true;
        }
    }

    /// The operation that the entry `id` is of.
    fn op_of(&self, id: EntryId) -> usize {
        self.ops.partition_point(|op| op.entries <= id) - 1
    }

    /// Whether the entry `id` is of an operation the transaction's frames
    /// do not show.
    pub(crate) fn unlaid(&self, id: EntryId) -> bool {
        !self.ops[self.op_of(id)].laid
    }

    /// The subtrees that operations which are not laid out drop or clear,
    /// oldest first.
    pub(crate) fn unlaid_leaving(&self) -> Vec<Subtree> {
        let unlaid = self.leaving.iter().filter(|id| self.unlaid(**id));
        unlaid.map(|id| self.entry(*id).unit.subtree).collect()
    }

    /// The place before the operation `op`.
    fn mark_at(&self, op: usize) -> Mark {
        let this = self.ops[op];
        Mark {
            entries: this.entries as usize,
            ops: op,
            merks: this.merks as usize,
            subtrees: this.subtrees as usize,
            arena: this.arena,
            last: op
                .checked_sub(1)
                .map_or(0, |before| self.ops[before].serial),
            pending: 0,
        }
    }

    /// The subtrees below the one it drops or clears that the operation
    /// `op` covers: what [`Self::add`] was given as `cut`.
    fn op_cut(&self, op: usize) -> Vec<Subtree> {
        let (from, to, ..) = self.op_bounds(op);
        let first = self.entries[from..to].first();
        first.map_or_else(Vec::new, |entry| Self::cut(&self.arena, entry).collect())
    }

    /// Takes out every write of the open scope `scope`, with its
    /// first-write version and its place in the overlap set (SC6, CF4),
    /// and says whether there was one. The writes of every other scope and
    /// of the transaction stay as they are, in their order.
    ///
    /// Where the scope's operations are the last of the set, this is a
    /// rollback to the place before them. Where operations of others came
    /// after one of them, those are added again behind what stays, each
    /// with the first-write version, the serial and the place in the
    /// overlap set it had: the set is copied for it, once.
    pub(crate) fn remove_scope(&mut self, scope: u32) -> bool {
        let Some(first) = self.ops.iter().position(|op| op.scope == scope) else {
            return false;
        };
        let last = self.ops[first..].iter().all(|op| op.scope == scope);
        let old = (!last).then(|| self.clone());
        let removed = self.rewind(self.mark_at(first));
        debug_assert!(removed, "the place before an operation of the set");
        let Some(old) = old else {
            return true;
        };
        for op in (first..old.ops.len()).filter(|op| old.ops[*op].scope != scope) {
            let was = old.ops[op];
            let hosted = old.hosted_lists(op);
            let batch = old.batch(op, &hosted);
            let (merks, subtrees) = (batch.merks(), batch.subtrees());
            let batch = Batch {
                records: batch.records(),
                merks: &merks,
                subtrees: &subtrees,
            };
            // A Merk that the operation gave its first root is stated as
            // one that had none, and every other root is stated as it was
            // kept: both come out as they went in.
            let start = self.entries.len();
            self.add(&batch, was.first_write, &old.made(op), &old.op_cut(op));
            let added = self.ops.last_mut().expect("it was added");
            (added.serial, added.scope, added.laid) = (was.serial, was.scope, was.laid);
            let (from, to, ..) = old.op_bounds(op);
            for (entry, was) in self.entries[start..].iter_mut().zip(&old.entries[from..to]) {
                entry.met = was.met;
                self.met += usize::from(was.met);
            }
        }
        self.serial = old.serial;
        true
    }

    fn op_bounds(&self, op: usize) -> (usize, usize, usize, usize) {
        let this = self.ops[op];
        let (entries_end, merks_end) = self
            .ops
            .get(op + 1)
            .map_or((self.entries.len(), self.merks.len()), |next| {
                (next.entries as usize, next.merks as usize)
            });
        (
            this.entries as usize,
            entries_end,
            this.merks as usize,
            merks_end,
        )
    }

    /// What the operation `op` did to subtrees as a whole.
    fn op_subtrees(&self, op: usize) -> &[SubtreeEntry] {
        let from = self.ops[op].subtrees as usize;
        let to = (self.ops.get(op + 1)).map_or(self.subtrees.len(), |next| next.subtrees as usize);
        &self.subtrees[from..to]
    }

    /// For each Merk of the operation `op`, whether the operation gave it a
    /// root when it had none.
    pub(crate) fn made(&self, op: usize) -> Vec<bool> {
        let (.., from, to) = self.op_bounds(op);
        self.merks[from..to].iter().map(|merk| merk.made).collect()
    }

    /// The Merks that the nodes of the operation `op` host, for
    /// [`Self::batch`].
    pub(crate) fn hosted_lists(&self, op: usize) -> HostedLists {
        let (from, to, ..) = self.op_bounds(op);
        self.entries[from..to]
            .iter()
            .filter_map(|entry| match entry.kind {
                Kind::Node(node) => Some(match node.hosted {
                    HostedEntry::Keep => Vec::new(),
                    HostedEntry::Set(span) => {
                        let (subtrees, _) = self.arena.get(span).as_chunks::<{ Subtree::LEN }>();
                        subtrees.iter().copied().map(Subtree::new).collect()
                    }
                }),
                _ => None,
            })
            .collect()
    }

    fn link_put(&self, link: Option<LinkEntry>) -> Option<LinkPut<'_>> {
        link.map(|link| LinkPut {
            key: self.arena.get(link.key),
            hash: hash_of(self.arena.get(link.hash)),
            heights: link.heights,
            aggregate: self.arena.get(link.aggregate),
        })
    }

    /// The operation `op` as the batch it was added as, for applying it
    /// again by key (WT7). A value the transaction took its own copy of is
    /// stated in full, and no node carries a hint.
    pub(crate) fn batch<'a>(&'a self, op: usize, hosted: &'a HostedLists) -> OpBatch<'a> {
        let (from, to, merks_from, merks_to) = self.op_bounds(op);
        let merks = self.merks[merks_from..merks_to].to_vec();
        let subtrees = self.op_subtrees(op);
        // The drops and the clears are applied from what the operation did
        // to subtrees, and their entries are passed over.
        let entries = self.entries[from..to].iter();
        let mut entries = entries.skip_while(|entry| !entry.kind.leaves().is_empty());
        let mut hosted = hosted.iter();
        let (mut nodes, mut removed) = (Vec::new(), Vec::new());
        for merk in &merks {
            let puts = entries.by_ref().take(merk.nodes as usize).map(|entry| {
                let Kind::Node(node) = entry.kind else {
                    unreachable!("the nodes of a Merk come first");
                };
                let hosts = hosted.next().expect("a list for each node");
                NodePut {
                    key: self.arena.get(entry.key),
                    feature: self.arena.get(node.feature),
                    value_hash: hash_of(self.arena.get(node.value_hash)),
                    kv_hash: hash_of(self.arena.get(node.kv_hash)),
                    left: self.link_put(node.left),
                    right: self.link_put(node.right),
                    hosted: match node.hosted {
                        HostedEntry::Keep => Hosted::Keep,
                        HostedEntry::Set(_) => Hosted::Set(hosts),
                    },
                    value: match node.value {
                        ValueEntry::Keep => ValuePut::Keep,
                        ValueEntry::Value(span) => ValuePut::Value(self.arena.get(span)),
                        ValueEntry::Kept(span) => ValuePut::Value(self.kept.get(span)),
                    },
                    stale: node.stale,
                    at: None,
                }
            });
            nodes.push(puts.collect());
            let keys = entries.by_ref().take(merk.removed as usize);
            removed.push(keys.map(|entry| self.arena.get(entry.key)).collect());
        }
        let records = entries.map(|entry| {
            let key = self.arena.get(entry.key);
            let unit = entry.unit;
            match entry.kind {
                Kind::Put(value) => RecordWrite::Put {
                    unit,
                    key,
                    value: self.arena.get(value),
                },
                Kind::Delete => RecordWrite::Delete { unit, key },
                Kind::Range { end } => RecordWrite::DeleteRange {
                    unit,
                    start: key,
                    end: end.map(|end| self.arena.get(end)),
                },
                Kind::Node(_) | Kind::Remove | Kind::Dropped | Kind::Cleared => {
                    unreachable!("the records come last")
                }
            }
        });
        OpBatch {
            records: records.collect(),
            nodes,
            removed,
            merks,
            subtrees,
            arena: &self.arena,
        }
    }

    /// What the transaction has written in `unit` since it last dropped or
    /// cleared the unit's subtree: Merk nodes, flat records, or nothing. It
    /// is never both ([`Self::admits`]).
    fn held(&self, unit: Unit) -> Option<Held> {
        let index = self.units.get(&unit)?;
        let left = |id: &&EntryId| !self.entry(**id).kind.leaves().is_empty();
        let since = index.ranges.iter().rev().find(left).copied();
        let later = |id: &&EntryId| since.is_none_or(|since| **id > since);
        let entry =
            (index.ranges.iter().find(later)).or_else(|| index.keys.values().find(later))?;
        match self.entry(*entry).kind {
            Kind::Node(_) | Kind::Remove => Some(Held::Nodes),
            Kind::Put(_) | Kind::Delete | Kind::Range { .. } => Some(Held::Flat),
            Kind::Dropped | Kind::Cleared => None,
        }
    }

    /// The subtrees the transaction dropped or cleared, oldest first.
    pub(crate) fn leaving(&self) -> impl Iterator<Item = Subtree> + '_ {
        (self.leaving.iter()).map(|id| self.entry(*id).unit.subtree)
    }

    /// A number that is another whenever the transaction drops or clears a
    /// subtree, or takes such a write out: whoever keeps what it found
    /// open asks again when it changed.
    pub(crate) fn left(&self) -> u64 {
        self.left
    }

    /// The subtrees the transaction has an entry under, in any column.
    pub(crate) fn written(&self) -> impl Iterator<Item = Subtree> + '_ {
        let mut subtrees: Vec<Subtree> = self.units.keys().map(|unit| unit.subtree).collect();
        subtrees.sort_unstable();
        subtrees.dedup();
        subtrees.into_iter()
    }

    /// The subtree of the element that hosts `subtree`, if the transaction
    /// entered it: `Some(None)` for one it entered with no host.
    pub(crate) fn host_of(&self, subtree: &Subtree) -> Option<Option<Subtree>> {
        self.subtrees.iter().rev().find_map(|entry| match entry {
            SubtreeEntry::Enter {
                subtree: entered,
                host,
                ..
            } if entered == subtree => Some(host.map(|(host, ..)| host)),
            _ => None,
        })
    }

    /// Checks `batch` against what the transaction has written: the data
    /// column of a subtree holds the nodes of a Merk or flat records, and
    /// not both (ST1).
    ///
    /// A subtree that the batch drops or clears holds nothing when its
    /// change sets and records are applied.
    pub(crate) fn admits(&self, batch: &Batch<'_>) -> Result<(), BatchError> {
        for merk in batch.merks {
            let subtree = merk.subtree;
            if !batch.leaves(subtree) && self.held(Unit::data(subtree)) == Some(Held::Flat) {
                return Err(BatchError::NotMerk { subtree });
            }
        }
        for record in batch.records {
            let unit = record.unit();
            let nodes = unit.column == Column::Data && self.held(unit) == Some(Held::Nodes);
            if nodes && !batch.leaves(unit.subtree) {
                return Err(BatchError::NotFlat { unit });
            }
        }
        Ok(())
    }

    /// Whether the Merk `subtree` has a root after the transaction's
    /// writes, if one of them set its root, or entered, dropped or cleared
    /// the subtree, which leaves it with none.
    pub(crate) fn rooted(&self, subtree: &Subtree) -> Option<bool> {
        for op in (0..self.ops.len()).rev() {
            // The change sets of an operation come after what it does to
            // subtrees as a whole.
            let (entries_from, entries_to, merks_from, merks_to) = self.op_bounds(op);
            let set = |merk: &MerkEntry| (merk.subtree == *subtree).then_some(merk.root).flatten();
            if let Some(root) = self.merks[merks_from..merks_to].iter().rev().find_map(set) {
                return Some(root.is_some());
            }
            let named = |entry: &SubtreeEntry| match entry {
                SubtreeEntry::Enter { subtree: named, .. }
                | SubtreeEntry::Drop(named)
                | SubtreeEntry::Clear(named) => named == subtree,
            };
            // A Merk below one that the operation dropped or cleared went
            // with it.
            let mut cut = (self.entries[entries_from..entries_to].iter())
                .flat_map(|entry| Self::cut(&self.arena, entry));
            if self.op_subtrees(op).iter().any(named) || cut.any(|cut| cut == *subtree) {
                return Some(false);
            }
        }
        None
    }

    /// The first key of the range delete `id`, and the key it ends before.
    ///
    /// A drop and a clear delete every key of the units they cover.
    fn range_bounds(&self, id: EntryId) -> (&[u8], Option<&[u8]>) {
        let entry = self.entry(id);
        match entry.kind {
            Kind::Range { end } => (
                self.arena.get(entry.key),
                end.map(|end| self.arena.get(end)),
            ),
            _ => (&[], None),
        }
    }

    fn range_holds(&self, id: EntryId, key: &[u8]) -> bool {
        let (start, end) = self.range_bounds(id);
        in_range(key, start, end)
    }

    /// The entry of the transaction's newest write that covers the record
    /// key `key` of `unit`, if it has one: what [`Self::own`] answers
    /// from.
    pub(crate) fn newest(&self, unit: Unit, key: &[u8]) -> Option<EntryId> {
        let index = self.units.get(&unit)?;
        let point = index.keys.get(key).copied();
        let range =
            (index.ranges.iter().rev().copied()).find(|range| self.range_holds(*range, key));
        point.max(range)
    }

    /// The transaction's newest write that covers the record key `key` of
    /// `unit` (RD3).
    pub(crate) fn own(&self, unit: Unit, key: &[u8]) -> Own<'_> {
        let Some(index) = self.units.get(&unit) else {
            return Own::Unwritten;
        };
        let point = index.keys.get(key).copied();
        let range =
            (index.ranges.iter().rev().copied()).find(|range| self.range_holds(*range, key));
        match (point, range) {
            (None, None) => Own::Unwritten,
            (None, Some(_)) => Own::Deleted,
            (Some(point), Some(range)) if range > point => Own::Deleted,
            (Some(point), _) => match self.entry(point).kind {
                Kind::Put(value) => Own::Put(self.arena.get(value)),
                Kind::Node(_) => Own::Node(point),
                Kind::Delete | Kind::Remove => Own::Deleted,
                Kind::Range { .. } | Kind::Dropped | Kind::Cleared => {
                    unreachable!("a range is not indexed by its key")
                }
            },
        }
    }

    /// Whether a range delete of the transaction contains the record key
    /// `key` of `unit`: a committed record under it is hidden, whatever
    /// else the transaction wrote there (RD3).
    pub(crate) fn hides(&self, unit: Unit, key: &[u8]) -> bool {
        self.units
            .get(&unit)
            .is_some_and(|index| (index.ranges.iter()).any(|range| self.range_holds(*range, key)))
    }

    /// The first key of `unit` that the transaction put or deleted and that
    /// lies within `bounds`, from the low end, or from the high end with
    /// `back`. A range delete is not a key.
    pub(crate) fn written_key(
        &self,
        unit: Unit,
        bounds: (Bound<&[u8]>, Bound<&[u8]>),
        back: bool,
    ) -> Option<&[u8]> {
        let keys = &self.units.get(&unit)?.keys;
        let empty = match bounds {
            (Bound::Included(low), Bound::Included(high)) => low > high,
            (
                Bound::Included(low) | Bound::Excluded(low),
                Bound::Included(high) | Bound::Excluded(high),
            ) => low >= high,
            _ => false,
        };
        if empty {
            return None;
        }
        let mut range = keys.range::<[u8], _>(bounds);
        let found = if back {
            range.next_back()
        } else {
            range.next()
        };
        found.map(|(key, _)| &**key)
    }

    /// The value of the written node `id`: its own, that of an earlier
    /// write of the same node, or the transaction's copy of the committed
    /// one. `None` says that it is the committed value and that no copy was
    /// taken, which is the case only while the transaction's frames hold
    /// it.
    pub(crate) fn node_value(&self, id: EntryId) -> Option<&[u8]> {
        let mut id = id;
        loop {
            let entry = self.entry(id);
            let Kind::Node(node) = entry.kind else {
                return None;
            };
            match node.value {
                ValueEntry::Value(span) => return Some(self.arena.get(span)),
                ValueEntry::Kept(span) => return Some(self.kept.get(span)),
                ValueEntry::Keep => id = entry.prev?,
            }
        }
    }

    /// The written nodes that keep a committed value the transaction has no
    /// copy of: those with no earlier write of the same node to take it
    /// from. They are what [`Self::keep`] is called for before the
    /// transaction gives up the version it read them at (WT7).
    pub(crate) fn keeping(&self) -> Vec<EntryId> {
        (0..self.entries.len() as EntryId)
            .filter(|id| {
                let entry = self.entry(*id);
                let keeps = matches!(
                    entry.kind,
                    Kind::Node(NodeEntry {
                        value: ValueEntry::Keep,
                        ..
                    })
                );
                let before = entry.prev.map(|prev| self.entry(prev).kind);
                keeps && !matches!(before, Some(Kind::Node(_)))
            })
            .collect()
    }

    /// Gives the written node `id`, which kept its value, the
    /// transaction's own copy of that value.
    pub(crate) fn keep(&mut self, id: EntryId, value: &[u8]) {
        let span = self.kept.put(value);
        let Kind::Node(node) = &mut self.entries[id as usize].kind else {
            unreachable!("a node keeps a value");
        };
        node.value = ValueEntry::Kept(span);
    }

    fn mark_met(&mut self, id: EntryId, version: CommitVersion) {
        let entry = &mut self.entries[id as usize];
        if !entry.met && entry.first_write < version {
            entry.met = true;
            self.met += 1;
        }
    }

    /// Compares the key set of a commit that was published outside the
    /// transaction with the write set, and puts into the overlap set every
    /// entry whose keys it meets and whose first-write version is before
    /// `version` (CF2, CF3).
    ///
    /// It does not matter that the transaction wrote a key again later: the
    /// earlier entry is met (case A9).
    pub(crate) fn meet(&mut self, version: CommitVersion, keys: &KeySet) {
        let mut met = Vec::new();
        for (unit, index) in &self.units {
            let chain = |mut id: EntryId, met: &mut Vec<EntryId>| loop {
                met.push(id);
                match self.entry(id).prev {
                    Some(prev) => id = prev,
                    None => break,
                }
            };
            if keys.covers_unit(*unit) {
                index.keys.values().for_each(|id| chain(*id, &mut met));
                met.extend(&index.ranges);
                continue;
            }
            let Some(theirs) = keys.units.get(unit) else {
                continue;
            };
            for key in &theirs.keys {
                if let Some(id) = index.keys.get(key) {
                    chain(*id, &mut met);
                }
            }
            for range in &index.ranges {
                let (start, end) = self.range_bounds(*range);
                let meets = theirs.keys_in(start, end).next().is_some()
                    || (theirs.ranges.iter())
                        .any(|(from, to)| ranges_meet((from, to.as_deref()), (start, end)));
                if meets {
                    met.push(*range);
                }
            }
            for (start, end) in &theirs.ranges {
                let end = end.as_deref();
                if end.is_some_and(|end| end <= &**start) {
                    continue;
                }
                let upper = end.map_or(Bound::Unbounded, Bound::Excluded);
                let inside = index
                    .keys
                    .range::<[u8], _>((Bound::Included(&**start), upper));
                inside.for_each(|(_, id)| chain(*id, &mut met));
            }
        }
        for id in met {
            self.mark_met(id, version);
        }
    }

    /// Puts into the overlap set every entry that covers a key of
    /// `subtree`, in any column, and whose first-write version is before
    /// `version`: a commit outside the transaction dropped the subtree, or
    /// one it is below, with every record under it (CF5, CV9).
    pub(crate) fn meet_subtree(&mut self, subtree: Subtree, version: CommitVersion) {
        let covers = |entry: &Entry| {
            let units = Self::covered(&self.arena, entry);
            units.iter().any(|unit| unit.subtree == subtree)
        };
        let met: Vec<EntryId> = (0..self.entries.len() as EntryId)
            .filter(|id| covers(self.entry(*id)))
            .collect();
        for id in met {
            self.mark_met(id, version);
        }
    }

    /// Puts into the overlap set every drop and clear of `subtree` whose
    /// first-write version is before `version`: a commit outside the
    /// transaction wrote a record below the subtree, which the drop
    /// deletes (CF5).
    pub(crate) fn meet_leaving(&mut self, subtree: Subtree, version: CommitVersion) {
        let named = |id: &&EntryId| self.entry(**id).unit.subtree == subtree;
        let met: Vec<EntryId> = self.leaving.iter().filter(named).copied().collect();
        for id in met {
            self.mark_met(id, version);
        }
    }

    /// The key set of the transaction's commit (CF1): every key it put or
    /// deleted, and every range it deleted, each once.
    pub(crate) fn key_set(&self) -> KeySet {
        let mut set = KeySet::new();
        for (unit, index) in &self.units {
            for key in index.keys.keys() {
                set.key(*unit, key);
            }
            for range in &index.ranges {
                let entry = self.entry(*range);
                match entry.kind {
                    // Every column of the dropped subtree, and of each one
                    // below it that the entry covers.
                    Kind::Dropped => {
                        set.subtree(unit.subtree);
                    }
                    Kind::Cleared if unit.subtree != entry.unit.subtree => {
                        set.subtree(unit.subtree);
                    }
                    _ => {
                        let (start, end) = self.range_bounds(*range);
                        set.range(*unit, start, end);
                    }
                }
            }
        }
        set
    }
}

/// The bytes of a list of subtrees, one after another.
trait ConcatBytes {
    fn concat_bytes(&self) -> Vec<u8>;
}

impl ConcatBytes for [Subtree] {
    fn concat_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.len() * Subtree::LEN);
        for subtree in self {
            bytes.extend_from_slice(subtree.as_bytes());
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn version(raw: u64) -> CommitVersion {
        CommitVersion::new(raw).expect("a version")
    }

    fn unit(name: u8) -> Unit {
        Unit::new(Column::Aux, Subtree::new([name; 32]))
    }

    /// SplitMix64.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) % bound as u64) as usize
        }
    }

    /// One write, as the model keeps it.
    #[derive(Debug, Clone, PartialEq)]
    enum Model {
        Put(Unit, Vec<u8>, Vec<u8>),
        Delete(Unit, Vec<u8>),
        Range(Unit, Vec<u8>, Option<Vec<u8>>),
    }

    impl Model {
        fn covers(&self, of: Unit, key: &[u8]) -> bool {
            match self {
                Self::Put(unit, at, _) | Self::Delete(unit, at) => *unit == of && at == key,
                Self::Range(unit, start, end) => {
                    *unit == of && in_range(key, start, end.as_deref())
                }
            }
        }

        fn write(&self) -> RecordWrite<'_> {
            match self {
                Self::Put(unit, key, value) => RecordWrite::Put {
                    unit: *unit,
                    key,
                    value,
                },
                Self::Delete(unit, key) => RecordWrite::Delete { unit: *unit, key },
                Self::Range(unit, start, end) => RecordWrite::DeleteRange {
                    unit: *unit,
                    start,
                    end: end.as_deref(),
                },
            }
        }
    }

    fn key(rng: &mut Rng) -> Vec<u8> {
        vec![b'a' + rng.below(12) as u8]
    }

    fn draw(rng: &mut Rng) -> Model {
        let unit = unit(rng.below(2) as u8);
        match rng.below(10) {
            0..=5 => Model::Put(unit, key(rng), vec![rng.below(250) as u8; rng.below(5)]),
            6..=7 => Model::Delete(unit, key(rng)),
            _ => {
                let start = key(rng);
                let end = (rng.below(3) > 0).then(|| key(rng));
                Model::Range(unit, start, end)
            }
        }
    }

    /// The write set answers as a list of its writes does: the newest write
    /// that covers a key, the written keys in order, the key set, and the
    /// overlap set of the conflict rule, through any number of rollbacks to
    /// a mark.
    #[test]
    fn the_index_agrees_with_the_list_of_writes() {
        for seed in 0..40 {
            let mut rng = Rng(seed);
            let mut set = WriteSet::new();
            // Each write with its first-write version and whether it is met.
            let mut model: Vec<(Model, u64, bool)> = Vec::new();
            let mut marks = vec![(set.mark(), 0)];
            let mut latest = 1;
            for _ in 0..200 {
                match rng.below(10) {
                    0..=5 => {
                        let write = draw(&mut rng);
                        let records = [write.write()];
                        let batch = Batch {
                            records: &records,
                            merks: &[],
                            ..Batch::default()
                        };
                        set.add(&batch, version(latest), &[], &[]);
                        model.push((write, latest, false));
                    }
                    6 => marks.push((set.mark(), model.len())),
                    7 => {
                        let to = rng.below(marks.len());
                        let (mark, len) = marks[to];
                        let above = marks.split_off(to + 1);
                        assert!(set.holds(mark));
                        assert_eq!(set.rewind(mark), model.len() > len);
                        model.truncate(len);
                        // A mark from after a write that went is no place
                        // of the set any more, whatever is written next.
                        for (stale, at) in above {
                            assert_eq!(set.holds(stale), at == len, "seed {seed}");
                        }
                    }
                    _ => {
                        // A commit outside, of one write.
                        latest += 1;
                        let foreign = draw(&mut rng);
                        let mut keys = KeySet::new();
                        match &foreign {
                            Model::Put(unit, key, _) | Model::Delete(unit, key) => {
                                keys.key(*unit, key);
                            }
                            Model::Range(unit, start, end) => {
                                keys.range(*unit, start, end.as_deref());
                            }
                        }
                        set.meet(version(latest), &keys);
                        for (own, first_write, met) in &mut model {
                            let meets = match (&*own, &foreign) {
                                (Model::Range(unit, start, end), Model::Range(of, from, to)) => {
                                    unit == of
                                        && ranges_meet(
                                            (start, end.as_deref()),
                                            (from, to.as_deref()),
                                        )
                                }
                                (
                                    range @ Model::Range(..),
                                    Model::Put(of, at, _) | Model::Delete(of, at),
                                )
                                | (Model::Put(of, at, _) | Model::Delete(of, at), range) => {
                                    range.covers(*of, at)
                                }
                            };
                            *met |= meets && *first_write < latest;
                        }
                    }
                }
                assert_eq!(set.len(), model.len(), "seed {seed}");
                assert_eq!(set.is_empty(), model.is_empty());
                assert_eq!(set.ops(), model.len());
                let met = model.iter().filter(|(.., met)| *met).count();
                assert_eq!(set.met(), met, "seed {seed}: the overlap set");
                let mut expected_keys = KeySet::new();
                for name in 0..2 {
                    let unit = unit(name);
                    let mut written = BTreeMap::new();
                    for byte in b'a'..b'a' + 12 {
                        let key = [byte];
                        let newest = model
                            .iter()
                            .rev()
                            .find(|(write, ..)| write.covers(unit, &key));
                        let expected = match newest.map(|(write, ..)| write) {
                            None => Own::Unwritten,
                            Some(Model::Put(_, _, value)) => Own::Put(value),
                            Some(_) => Own::Deleted,
                        };
                        assert_eq!(set.own(unit, &key), expected, "seed {seed}: {key:?}");
                        let hidden = model.iter().any(|(write, ..)| {
                            matches!(write, Model::Range(..)) && write.covers(unit, &key)
                        });
                        assert_eq!(set.hides(unit, &key), hidden);
                        let point = model.iter().any(|(write, ..)| {
                            !matches!(write, Model::Range(..)) && write.covers(unit, &key)
                        });
                        if point {
                            written.insert(key.to_vec(), ());
                            expected_keys.key(unit, &key);
                        }
                    }
                    for (write, ..) in &model {
                        if let Model::Range(of, start, end) = write
                            && *of == unit
                        {
                            expected_keys.range(unit, start, end.as_deref());
                        }
                    }
                    // The written keys, from both ends and from the middle.
                    let keys: Vec<&Vec<u8>> = written.keys().collect();
                    let all = (Bound::Unbounded, Bound::Unbounded);
                    assert_eq!(
                        set.written_key(unit, all, false),
                        keys.first().map(|key| &key[..])
                    );
                    assert_eq!(
                        set.written_key(unit, all, true),
                        keys.last().map(|key| &key[..])
                    );
                    let from = (Bound::Excluded(&b"f"[..]), Bound::Unbounded);
                    let next = keys.iter().find(|key| key[..] > b"f"[..]);
                    assert_eq!(set.written_key(unit, from, false), next.map(|key| &key[..]));
                    let none = (Bound::Excluded(&b"f"[..]), Bound::Excluded(&b"f"[..]));
                    assert_eq!(set.written_key(unit, none, false), None);
                    let inverted = (Bound::Included(&b"g"[..]), Bound::Included(&b"f"[..]));
                    assert_eq!(set.written_key(unit, inverted, true), None);
                }
                let mut ours = set.key_set();
                for keys in ours.units.values_mut() {
                    keys.ranges.sort();
                }
                for keys in expected_keys.units.values_mut() {
                    keys.ranges.sort();
                    keys.ranges.dedup();
                }
                assert_eq!(ours, expected_keys, "seed {seed}: the key set");
            }
            // Every operation is the batch it was added as.
            for (op, (write, first_write, _)) in model.iter().enumerate() {
                let hosted = set.hosted_lists(op);
                let batch = set.batch(op, &hosted);
                assert_eq!(batch.records(), [write.write()]);
                assert!(batch.merks().is_empty() && set.made(op).is_empty());
                assert_eq!(set.entry(op as EntryId).first_write, version(*first_write));
            }
            set.clear();
            assert!(set.is_empty() && set.met() == 0 && set.bytes() == 0);
            assert_eq!(set.own(unit(0), b"a"), Own::Unwritten);
        }
    }

    /// A mark is refused once a write from before it went, whatever was
    /// written since: the place it names is in the middle of other
    /// operations.
    #[test]
    fn a_mark_is_a_place_until_a_write_before_it_goes() {
        let mut set = WriteSet::new();
        let add = |set: &mut WriteSet, keys: &[&[u8]]| {
            let records: Vec<RecordWrite<'_>> = (keys.iter())
                .map(|key| RecordWrite::Put {
                    unit: unit(1),
                    key,
                    value: b"v",
                })
                .collect();
            let batch = Batch {
                records: &records,
                merks: &[],
                ..Batch::default()
            };
            set.add(&batch, version(1), &[], &[]);
        };
        let empty = set.mark();
        add(&mut set, &[b"a"]);
        let first = set.mark();
        add(&mut set, &[b"b", b"c"]);
        let second = set.mark();
        add(&mut set, &[b"d"]);
        assert!(set.holds(empty) && set.holds(first) && set.holds(second));
        assert!(set.rewind(first));
        assert!(!set.rewind(first), "nothing is after it");
        // Two operations of one write each stand where one of two stood.
        add(&mut set, &[b"e"]);
        add(&mut set, &[b"f"]);
        assert!(set.holds(first) && !set.holds(second));
        assert!(!set.rewind(second));
        assert_eq!((set.len(), set.ops()), (3, 3));
        assert_eq!(set.own(unit(1), b"f"), Own::Put(b"v"));
        set.clear();
        add(&mut set, &[b"g", b"h"]);
        assert!(!set.holds(first) && !set.rewind(first));
        assert!(set.holds(empty) && set.rewind(empty));
        assert!(set.is_empty() && set.bytes() == 0);
    }

    /// A commit that drops a subtree or wipes the database meets every
    /// write under it, of every kind (CF5).
    #[test]
    fn a_dropped_subtree_and_a_wipe_meet_every_write_under_them() {
        let mut set = WriteSet::new();
        let records = [
            RecordWrite::Put {
                unit: unit(1),
                key: b"k",
                value: b"1",
            },
            RecordWrite::Put {
                unit: unit(1),
                key: b"k",
                value: b"2",
            },
            RecordWrite::DeleteRange {
                unit: unit(1),
                start: b"a",
                end: None,
            },
            RecordWrite::Delete {
                unit: unit(2),
                key: b"k",
            },
        ];
        for record in &records {
            let batch = Batch {
                records: std::slice::from_ref(record),
                merks: &[],
                ..Batch::default()
            };
            set.add(&batch, version(3), &[], &[]);
        }
        let mut dropped = KeySet::new();
        dropped.subtree(unit(1).subtree);
        // A commit at the first-write version, or before it, is not after
        // it.
        set.meet(version(3), &dropped);
        assert_eq!(set.met(), 0);
        set.meet(version(4), &dropped);
        assert_eq!(set.met(), 3, "both puts of the key and the range");
        let mut wipe = KeySet::new();
        wipe.everything();
        set.meet(version(5), &wipe);
        assert_eq!(set.met(), 4);
    }
}
