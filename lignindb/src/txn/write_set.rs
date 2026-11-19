//! The write set of a write transaction: the ordered list of its writes,
//! and an index of them by record key (`transactions.md` WT2, WT3).

use std::{
    collections::{BTreeMap, HashMap},
    ops::Bound,
};

use lignin_format::{tree_page::Hash, CommitVersion};

use super::{
    arena::{Arena, ArenaMark, Span},
    key::{in_range, ranges_meet, KeySet, Subtree, Unit},
    write::{Batch, Hosted, LinkPut, MerkWrite, NodePut, RecordWrite, RootPut, ValuePut},
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
    /// A value in the node's cell.
    Inline(Span),
    /// A value out of line.
    OutOfLine(Span),
    /// The transaction's own copy of the committed value the entry kept, in
    /// the arena of kept values (WT3, WT7).
    Kept { span: Span, out_of_line: bool },
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
    /// Whether the Merk had no root when the operation wrote it, and the
    /// transaction had not written it before.
    made: bool,
    nodes: u32,
    removed: u32,
}

/// Where the entries of one operation begin.
#[derive(Debug, Clone, Copy)]
struct Op {
    entries: EntryId,
    merks: u32,
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
#[derive(Debug, Default)]
struct UnitIndex {
    /// The newest entry of each key that was put or deleted.
    keys: BTreeMap<Box<[u8]>, EntryId>,
    /// The range deletes, oldest first.
    ranges: Vec<EntryId>,
}

/// A place in a write set: the writes after it can be taken out again, which
/// is what a savepoint and a scope are (WT8, SC6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    entries: usize,
    ops: usize,
    merks: usize,
    arena: ArenaMark,
}

/// The write set (WT2): every write the transaction made, in order, with
/// its bytes in one arena, and beside it an index by record key.
///
/// The list is the truth (WT3). The index is derived from it: it gives the
/// newest write that covers a key, and the written keys of a unit in order.
#[derive(Debug, Default)]
pub(crate) struct WriteSet {
    entries: Vec<Entry>,
    ops: Vec<Op>,
    merks: Vec<MerkEntry>,
    arena: Arena,
    /// The copies of committed values that entries kept. They are not taken
    /// out with a mark: an entry from before the mark can have its copy
    /// made after it.
    kept: Arena,
    units: HashMap<Unit, UnitIndex>,
    met: usize,
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
}

fn hash_of(bytes: &[u8]) -> &Hash {
    bytes.try_into().expect("a hash was put in whole")
}

impl WriteSet {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether the transaction has made no write.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
            arena: self.arena.mark(),
        }
    }

    /// Whether writes were made after `mark`.
    pub(crate) fn is_after(&self, mark: Mark) -> bool {
        self.entries.len() > mark.entries
    }

    /// Takes out every write that was made after `mark`, newest first, with
    /// its first-write version and its place in the overlap set (WT8, CF4).
    ///
    /// `mark` is one this set gave and that no earlier call has gone back
    /// behind.
    pub(crate) fn rewind(&mut self, mark: Mark) {
        debug_assert!(mark.entries <= self.entries.len(), "a mark of this set");
        while self.entries.len() > mark.entries {
            let id = (self.entries.len() - 1) as EntryId;
            let entry = self.entries.pop().expect("more entries than the mark");
            if entry.met {
                self.met -= 1;
            }
            let index = self
                .units
                .get_mut(&entry.unit)
                .expect("the unit of an entry is indexed");
            match entry.kind {
                Kind::Range { .. } => {
                    let last = index.ranges.pop();
                    debug_assert_eq!(last, Some(id), "the newest range is the last");
                }
                _ => {
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
            }
            if index.keys.is_empty() && index.ranges.is_empty() {
                self.units.remove(&entry.unit);
            }
        }
        self.ops.truncate(mark.ops);
        self.merks.truncate(mark.merks);
        self.arena.rewind(mark.arena);
        if self.entries.is_empty() {
            self.kept.clear();
        }
    }

    /// Takes out every write (WT9).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.ops.clear();
        self.merks.clear();
        self.arena.clear();
        self.kept.clear();
        self.units.clear();
        self.met = 0;
    }

    fn push(&mut self, unit: Unit, key: &[u8], kind: Kind, first_write: CommitVersion) -> EntryId {
        let id = EntryId::try_from(self.entries.len()).expect("fewer than 2^32 writes");
        let index = self.units.entry(unit).or_default();
        let prev = match kind {
            Kind::Range { .. } => {
                index.ranges.push(id);
                None
            }
            _ => match index.keys.get_mut(key) {
                Some(newest) => Some(std::mem::replace(newest, id)),
                None => {
                    index.keys.insert(key.into(), id);
                    None
                }
            },
        };
        let key = self.arena.put(key);
        self.entries.push(Entry {
            unit,
            key,
            kind,
            first_write,
            met: false,
            prev,
        });
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
    /// read (WT5). Returns the entry of the batch's first write.
    ///
    /// The entries come in the order of [`Applied::nodes`](super::Applied):
    /// for each Merk its nodes and then its deletes, and the records last.
    /// The batch has passed [`check`](super::check::check).
    ///
    /// `empty` says of each Merk of the batch whether it had no root before
    /// the operation.
    pub(crate) fn add(
        &mut self,
        batch: &Batch<'_>,
        first_write: CommitVersion,
        empty: &[bool],
    ) -> EntryId {
        let first = self.entries.len() as EntryId;
        self.ops.push(Op {
            entries: first,
            merks: self.merks.len() as u32,
        });
        for (merk, empty) in batch.merks.iter().zip(empty) {
            let unit = Unit::data(merk.subtree);
            let made = *empty && !self.units.contains_key(&unit);
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
                    ValuePut::Inline(value) => ValueEntry::Inline(self.arena.put(value)),
                    ValuePut::OutOfLine(value) => ValueEntry::OutOfLine(self.arena.put(value)),
                };
                let entry = NodeEntry {
                    feature: self.arena.put(node.feature),
                    value_hash: self.arena.put(node.value_hash),
                    kv_hash: self.arena.put(node.kv_hash),
                    left: self.link(node.left),
                    right: self.link(node.right),
                    hosted,
                    value,
                };
                self.push(unit, node.key, Kind::Node(entry), first_write);
            }
            for key in merk.removed {
                self.push(unit, key, Kind::Remove, first_write);
            }
            let root = match merk.root {
                RootPut::Keep => None,
                RootPut::Set(root) => Some(root.map(|key| self.arena.put(key))),
            };
            self.merks.push(MerkEntry {
                subtree: merk.subtree,
                root,
                anchored: merk.anchored,
                made,
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

    /// For each Merk of the operation `op`, whether the transaction made
    /// its first node with that operation: the Merk had no root then.
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
                    HostedEntry::Set(span) => self
                        .arena
                        .get(span)
                        .chunks_exact(Subtree::LEN)
                        .map(|bytes| Subtree::new(bytes.try_into().expect("32 bytes")))
                        .collect(),
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
        let mut entries = self.entries[from..to].iter();
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
                        ValueEntry::Inline(span) => ValuePut::Inline(self.arena.get(span)),
                        ValueEntry::OutOfLine(span) => ValuePut::OutOfLine(self.arena.get(span)),
                        ValueEntry::Kept { span, out_of_line } => match out_of_line {
                            false => ValuePut::Inline(self.kept.get(span)),
                            true => ValuePut::OutOfLine(self.kept.get(span)),
                        },
                    },
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
                Kind::Node(_) | Kind::Remove => unreachable!("the records come last"),
            }
        });
        OpBatch {
            records: records.collect(),
            nodes,
            removed,
            merks,
            arena: &self.arena,
        }
    }

    fn range_holds(&self, id: EntryId, key: &[u8]) -> bool {
        let entry = self.entry(id);
        let Kind::Range { end } = entry.kind else {
            unreachable!("the ranges of a unit are range deletes");
        };
        in_range(
            key,
            self.arena.get(entry.key),
            end.map(|end| self.arena.get(end)),
        )
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
                Kind::Range { .. } => unreachable!("a range is not indexed by its key"),
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
    pub(crate) fn node_value(&self, id: EntryId) -> Option<(&[u8], bool)> {
        let mut id = id;
        loop {
            let entry = self.entry(id);
            let Kind::Node(node) = entry.kind else {
                return None;
            };
            match node.value {
                ValueEntry::Inline(span) => return Some((self.arena.get(span), false)),
                ValueEntry::OutOfLine(span) => return Some((self.arena.get(span), true)),
                ValueEntry::Kept { span, out_of_line } => {
                    return Some((self.kept.get(span), out_of_line))
                }
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
    pub(crate) fn keep(&mut self, id: EntryId, value: &[u8], out_of_line: bool) {
        let span = self.kept.put(value);
        let Kind::Node(node) = &mut self.entries[id as usize].kind else {
            unreachable!("a node keeps a value");
        };
        node.value = ValueEntry::Kept { span, out_of_line };
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
            let bounds = |id: EntryId| {
                let entry = self.entry(id);
                let Kind::Range { end } = entry.kind else {
                    unreachable!("the ranges of a unit are range deletes");
                };
                (
                    self.arena.get(entry.key),
                    end.map(|end| self.arena.get(end)),
                )
            };
            for key in &theirs.keys {
                if let Some(id) = index.keys.get(key) {
                    chain(*id, &mut met);
                }
            }
            for range in &index.ranges {
                let (start, end) = bounds(*range);
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
                let Kind::Range { end } = entry.kind else {
                    unreachable!("the ranges of a unit are range deletes");
                };
                let end = end.map(|end| self.arena.get(end));
                set.range(*unit, self.arena.get(entry.key), end);
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
