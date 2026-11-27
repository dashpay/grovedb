//! The write set of a write transaction: the ordered list of its writes,
//! and an index of them by record key (`transactions.md` WT2, WT3).

use std::{
    collections::{hash_map, BTreeMap, BTreeSet, HashMap, HashSet},
    ops::Bound,
};

use lignin_format::{tree_page::Hash, CommitVersion};
use lignin_tree::hash::Stale;

use super::{
    arena::{Arena, ArenaMark, Span},
    error::BatchError,
    key::{in_range, ranges_meet, Column, KeyRange, KeySet, Subtree, Unit, UnitKeys},
    load::StoredValue,
    view::{FlatKinds, SubtreeView},
    write::{
        marked, Batch, Entering, HostNode, HostPut, Hosted, LinkPut, MerkWrite, Moving, NodePut,
        RecordWrite, RootPut, SubtreeWrite, ValuePut,
    },
};
use crate::scope::table::ScopeId;

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
    /// The value the write stated, which a load put into the value log:
    /// where it is ([`StoredValue`]), in the place of its bytes.
    Stored(Span),
}

/// The value of a written node, as the write set has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeValue<'a> {
    /// The value's bytes.
    Bytes(&'a [u8]),
    /// A value that a load put into the value log.
    Stored(StoredValue),
}

impl NodeValue<'_> {
    /// The length of the value in bytes.
    pub(crate) fn len(&self) -> u32 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u32,
            Self::Stored(stored) => stored.pointer.value_len(),
        }
    }
}

/// What an operation that is applied again is given beside its batch
/// ([`WriteSet::with_batch`]).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Again<'a> {
    /// For each Merk of the batch, whether the operation gave it a root
    /// when it had none ([`WriteSet::made`]).
    pub(crate) made: &'a [bool],
    /// The values of the batch that a load put into the value log, each by
    /// the place of its node among the entries of the batch's change sets,
    /// in ascending order. The batch states no bytes for them.
    pub(crate) stored: &'a [(u32, StoredValue)],
    /// The subtrees that the batch drops if they are entered
    /// ([`SubtreeWrite::DropIfEntered`]) and that it took in the
    /// operation that took it ([`WriteSet::add`]).
    pub(crate) took: &'a [Subtree],
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
    /// The hashes the write left stale, as its writer stated them
    /// ([`NodePut::stale`]): what a node that is shown from its entry
    /// shows.
    pub(crate) stale: Stale,
}

impl NodeEntry {
    /// Whether the write left a hash of its node stale as the pages mark
    /// them ([`marked`]): a mark on the hash of a child that the node
    /// does not have leaves none.
    fn leaves_stale(&self) -> bool {
        let linked = [self.left.is_some(), self.right.is_some()];
        !marked(self.stale, linked).is_empty()
    }
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
    /// The moves of one operation, which are made together: a write of
    /// every key of every column under each prefix they name, on either
    /// side, and under the prefix of each subtree below a moved one that
    /// left with the moves when they were stated (`transactions.md`
    /// CF10). The entry's key holds them all ([`Names`]).
    ///
    /// It deletes nothing by itself. What the transaction reads under a
    /// prefix after it follows from where the move took each subtree
    /// ([`WriteSet::stretches`]).
    Moved,
}

/// What the key of a [`Kind::Moved`] entry holds: how many pairs the moves
/// of its operation name, the prefixes the subtrees are taken from, the
/// prefixes they are brought to in the same order, and then the prefixes
/// of the subtrees that left with the moves.
struct Names<'a> {
    sources: &'a [[u8; Subtree::LEN]],
    targets: &'a [[u8; Subtree::LEN]],
    left: &'a [[u8; Subtree::LEN]],
}

impl<'a> Names<'a> {
    fn encode(moves: &[Moving<'_>], left: &[Subtree]) -> Vec<u8> {
        let sources: Vec<Subtree> = moves.iter().flat_map(Moving::sources).collect();
        let targets: Vec<Subtree> = moves.iter().flat_map(Moving::targets).collect();
        let mut bytes = (sources.len() as u32).to_le_bytes().to_vec();
        bytes.extend(sources.concat_bytes());
        bytes.extend(targets.concat_bytes());
        bytes.extend(left.concat_bytes());
        bytes
    }

    fn of(bytes: &'a [u8]) -> Self {
        let (count, rest) = bytes.split_first_chunk::<4>().expect("a count");
        let (all, _) = rest.as_chunks::<{ Subtree::LEN }>();
        let (sources, rest) = all.split_at(u32::from_le_bytes(*count) as usize);
        let (targets, left) = rest.split_at(sources.len());
        Self {
            sources,
            targets,
            left,
        }
    }

    /// Every prefix the moves name, on either side.
    fn named(&self) -> impl Iterator<Item = Subtree> + 'a {
        let named = self.sources.iter().chain(self.targets);
        named.copied().map(Subtree::new)
    }

    /// Every prefix the entry covers: those the moves name, and those of
    /// the subtrees that left with them.
    fn covered(&self) -> impl Iterator<Item = Subtree> + 'a {
        let left = self.left.iter().copied().map(Subtree::new);
        self.named().chain(left)
    }

    /// What the moves did under `at`: brought a subtree there, from the
    /// prefix that is returned, or took one from there and brought none
    /// (`Some(None)`). `None` for a prefix they did nothing under.
    ///
    /// The set keeps this of each prefix where the move is added
    /// (`WriteSet::covered`); what it keeps is held to this.
    #[cfg(any(test, feature = "held_reads"))]
    fn place(&self, at: &Subtree) -> Option<Option<Subtree>> {
        let at = at.as_bytes();
        if let Some(brought) = self.targets.iter().position(|to| to == at) {
            return Some(Some(Subtree::new(self.sources[brought])));
        }
        let taken = self.sources.iter().chain(self.left).any(|from| from == at);
        taken.then_some(None)
    }
}

/// One stretch of the writes that a transaction which moved subtrees made
/// of one subtree, as that subtree is read at some point: those under the
/// prefix `subtree` that are newer than the move `after` and older than
/// `before`.
#[derive(Debug, Clone, Copy)]
struct Stretch {
    subtree: Subtree,
    after: Option<EntryId>,
    before: Option<EntryId>,
}

impl Stretch {
    fn holds(&self, id: EntryId) -> bool {
        self.after.is_none_or(|after| id > after) && self.before.is_none_or(|before| id < before)
    }
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

    /// Whether the entry writes keys without naming each: it is among the
    /// ranges of the units it covers, and under no key.
    fn is_range(&self) -> bool {
        matches!(
            self,
            Self::Range { .. } | Self::Dropped | Self::Cleared | Self::Moved
        )
    }

    /// Whether the entry is of what its operation did to a subtree as a
    /// whole: these come first among the entries of an operation.
    fn is_whole(&self) -> bool {
        matches!(self, Self::Dropped | Self::Cleared | Self::Moved)
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
    /// Whether the change set said that it is the whole Merk.
    whole: bool,
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
        /// Whether the subtree is entered only where it is absent
        /// ([`SubtreeWrite::EnterIfAbsent`]): the set keeps what was
        /// stated, and each layout of it finds the subtree entered or
        /// not.
        if_absent: bool,
    },
    Drop(Subtree),
    /// A drop that names the node its subtree is entered under
    /// ([`SubtreeWrite::DropUnder`]): the node's subtree and key. The set
    /// keeps what was stated, and each layout of it holds the subtree to
    /// that node.
    DropUnder {
        subtree: Subtree,
        host: (Subtree, Span),
    },
    /// A drop of the subtree if it is entered under a node
    /// ([`SubtreeWrite::DropIfEntered`]): the node's subtree and key.
    /// `took` says whether it took the subtree in the operation that took
    /// its batch: it is a drop to the set then, with the entry of one,
    /// and nothing otherwise. The set keeps what was stated beside it: a
    /// layout of the set holds a drop that took nothing to that.
    DropIfEntered {
        subtree: Subtree,
        host: (Subtree, Span),
        took: bool,
    },
    /// What the operation removes under the subtree is removed if the
    /// subtree is entered ([`SubtreeWrite::RemoveIfEntered`]).
    RemoveIfEntered(Subtree),
    /// The subtree is not entered under a node
    /// ([`SubtreeWrite::NotUnder`]): the node's subtree and key. The set
    /// keeps what was stated, and each layout of it holds the subtree to
    /// that.
    NotUnder {
        subtree: Subtree,
        host: (Subtree, Span),
    },
    Clear(Subtree),
    /// The subtree is stated anew: its tree type, and its position among
    /// the subtrees of its host where it takes another.
    Convert {
        subtree: Subtree,
        tree_type: Span,
        position: Option<u8>,
    },
    Move {
        from: Subtree,
        to: Subtree,
        /// The host's subtree, its key and the position.
        host: (Subtree, Span, u8),
        /// The pairs of the subtrees below: the prefix before and the
        /// prefix afterwards, 32 bytes each.
        below: Span,
    },
}

impl SubtreeEntry {
    /// The subtree that the operation dropped by this, if it did: a drop
    /// of a subtree if it is entered that took none is none.
    fn dropped(&self) -> Option<Subtree> {
        match *self {
            Self::Drop(subtree) | Self::DropUnder { subtree, .. } => Some(subtree),
            Self::DropIfEntered { subtree, took, .. } => took.then_some(subtree),
            Self::Enter { .. }
            | Self::RemoveIfEntered(_)
            | Self::NotUnder { .. }
            | Self::Clear(_)
            | Self::Convert { .. }
            | Self::Move { .. } => None,
        }
    }

    /// The subtree that the operation dropped or cleared by this, if it
    /// did.
    fn left(&self) -> Option<Subtree> {
        match *self {
            Self::Clear(subtree) => Some(subtree),
            _ => self.dropped(),
        }
    }

    /// The subtree that the entry says something of as a whole, for what
    /// the transaction's own writes say of a subtree
    /// ([`WriteSet::subtree`], [`WriteSet::rooted_before`]): the one it
    /// enters, states anew, clears or drops. A move is not asked there,
    /// and the other entries change no subtree.
    fn says_of(&self) -> Option<Subtree> {
        match *self {
            Self::Enter { subtree, .. } | Self::Convert { subtree, .. } => Some(subtree),
            Self::Drop(_)
            | Self::DropUnder { .. }
            | Self::DropIfEntered { .. }
            | Self::Clear(_) => self.left(),
            Self::RemoveIfEntered(_) | Self::NotUnder { .. } | Self::Move { .. } => None,
        }
    }
}

/// The subtrees that `bytes` hold, 32 bytes each.
fn subtrees_of(bytes: &[u8]) -> impl Iterator<Item = Subtree> + '_ {
    let (subtrees, _) = bytes.as_chunks::<{ Subtree::LEN }>();
    subtrees.iter().copied().map(Subtree::new)
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
    scope: ScopeId,
    /// Whether the transaction's frames show the operation. A scope that
    /// was opened beside another lays its operations out when it merges
    /// (`lignindb::scope`).
    laid: bool,
    /// The first-write version of the operation's entries (WT5).
    first_write: CommitVersion,
    /// The place of the arena before the operation's bytes.
    arena: ArenaMark,
    /// The tree types that keep flat records, as the operation's batch
    /// was given them: it is judged by them wherever it is laid out.
    kinds: Option<FlatKinds>,
    /// Whether the pages took the operation by the tree type of a
    /// subtree when it was added ([`Frames::both`](super::frames::Frames)):
    /// while the set holds such an operation, [`WriteSet::admits`] looks
    /// for both kinds among all that the transaction wrote. It is told
    /// when the operation is added to pages and by no layout after that.
    both: bool,
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

/// What a transaction's own writes say of a subtree as a whole.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OwnSubtree<'a> {
    /// No write of the transaction entered the subtree or took it out: it
    /// is what the committed state has. `root` is the key of the root its
    /// writes left its Merk with, if one of them set the root, or cleared
    /// the subtree. `stated` is what its writes stated the subtree anew
    /// with.
    Unwritten {
        root: Option<Option<&'a [u8]>>,
        stated: Restated<'a>,
    },
    /// The transaction entered the subtree in its operation `op`, and
    /// this is what it holds of it since, unless a later operation took a
    /// subtree above it ([`WriteSet::left_after`]).
    Entered { view: SubtreeView<'a>, op: usize },
    /// The transaction dropped the subtree, or dropped or cleared one
    /// above it after it had written this one.
    Gone,
}

/// What a transaction's own writes say of a subtree ([`WriteSet::subtree`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Said<'a> {
    /// What they come to up to the last of them that entered the subtree
    /// as one that is new, or took it out.
    pub(crate) base: OwnSubtree<'a>,
    /// The operations after that which entered the subtree if it was
    /// absent ([`SubtreeWrite::EnterIfAbsent`]), newest first: each with
    /// what it states and the root that the writes from it on left the
    /// Merk with, as in [`OwnSubtree::Entered`]. Whether the subtree was
    /// absent then is not in the write set alone: it was unless `base`,
    /// or an enter before, left it entered, and no subtree above it left
    /// since.
    pub(crate) if_absent: Vec<(SubtreeView<'a>, usize)>,
}

/// What a transaction's writes stated a subtree anew with, since it was
/// entered: the tree type its newest such write gave it, and the position
/// the newest that gave one put it at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Restated<'a> {
    pub(crate) tree_type: Option<&'a [u8]>,
    pub(crate) position: Option<u8>,
}

impl<'a> Restated<'a> {
    /// `view` as these statements leave it.
    pub(crate) fn of(&self, view: SubtreeView<'a>) -> SubtreeView<'a> {
        SubtreeView {
            tree_type: self.tree_type.unwrap_or(view.tree_type),
            host: view.host.map(|host| HostPut {
                position: self.position.unwrap_or(host.position),
                ..host
            }),
            ..view
        }
    }
}

/// The writes of one unit, by key.
#[derive(Debug, Clone, Default)]
struct UnitIndex {
    /// The newest entry of each key that was put or deleted.
    keys: BTreeMap<Box<[u8]>, EntryId>,
    /// The range deletes, oldest first.
    ranges: Vec<EntryId>,
    /// The puts of nodes that left a hash stale and that no later write
    /// of the same node has passed ([`WriteSet::unmade`]).
    stale: BTreeSet<EntryId>,
}

/// A place in a write set: the writes after it can be taken out again, which
/// is what a savepoint is (WT8). The writes of a scope are taken out by the
/// scope they name (SC6).
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

/// The operations that say something of one subtree as a whole, in two
/// lists, each oldest first and with each operation once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Says {
    /// Those that change the subtree as a whole: one that enters it,
    /// states it anew, drops it or clears it, and one whose drop or
    /// clear takes it with a subtree above it.
    whole: Vec<usize>,
    /// Those with a change set that states the root of its Merk: a key,
    /// or none. A transaction can state the root of one Merk in each of
    /// its operations, so this list can be as long as the transaction.
    rooted: Vec<usize>,
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
    /// For each entry of `leaving`: whether it takes the subtrees below
    /// its own. A drop does. A clear does where its Merk had a node, as
    /// the writes of the transaction from before the clear say
    /// ([`Self::rooted_before`]): `None` where they say nothing of the
    /// Merk's root.
    taking: Vec<Option<bool>>,
    /// How many times `leaving` changed. It never goes back.
    left: u64,
    /// The moves among the entries, oldest first.
    moved: Vec<EntryId>,
    /// The prefixes that the moves took a subtree from, each with how
    /// many of the moves' pairs name it so ([`Self::took_from`]).
    sources: HashMap<Subtree, u32>,
    /// For each prefix that a move covers, the moves that cover it,
    /// oldest first, each with what it did there: it brought a subtree
    /// there, from the prefix that is kept, or took one from there and
    /// brought none.
    covered: HashMap<Subtree, Vec<(EntryId, Option<Subtree>)>>,
    /// For each subtree that `leaving` names, what its drops and its
    /// clears say of the subtrees below it (`taking`), counted
    /// ([`Self::takes_below`]).
    taken: HashMap<Subtree, Taken>,
    /// Every prefix that a move names, on either side, each with how
    /// many times ([`Self::named_by_a_move`]).
    named: HashMap<Subtree, u32>,
    /// For each subtree, the enters of it among `subtrees`, oldest
    /// first: where each stands there, and the subtree of the host
    /// that it states ([`Self::host_of`]).
    enters: HashMap<Subtree, Vec<(u32, Option<Subtree>)>>,
    /// For each subtree, the operations that say something of it as a
    /// whole ([`Says`], [`Self::said_before`]). No subtree is kept with
    /// no operation.
    said: HashMap<Subtree, Says>,
    /// How many times operations were taken out of the set: by a
    /// rollback to a mark, with a scope that was taken out, and at the
    /// end of the writes, each where there was an operation to take. It
    /// never goes back ([`Self::taken_out`]).
    taken_out: u64,
    /// For each open scope, its operations, oldest first
    /// ([`Self::ops_of`]). No scope is kept with no operation.
    scoped: HashMap<ScopeId, Vec<usize>>,
    arena: Arena,
    /// The copies of committed values that entries kept. They are not taken
    /// out with a mark: an entry from before the mark can have its copy
    /// made after it.
    kept: Arena,
    units: HashMap<Unit, UnitIndex>,
    /// The subtrees the set has an entry under, in the order of their
    /// prefixes, each with how many of its units are in `units` and with
    /// the number it was given when it came ([`Self::written`]). A subtree
    /// comes in with its first unit and goes with its last, where the
    /// writes after a mark are taken out.
    written: BTreeMap<Subtree, Written>,
    /// The same subtrees by the number each was given when it came
    /// ([`Self::written_after`]).
    came: BTreeMap<u64, Subtree>,
    /// How many times a subtree came to be one the set has an entry
    /// under: the number of the one that came last. It never goes back.
    first_writes: u64,
    met: usize,
    /// How many entries the units list as puts that left a hash stale
    /// (`UnitIndex::stale`), and whether one was listed since the set was
    /// last cleared.
    unmade: usize,
    listed: bool,
    /// The serial of the operation that was added last. It never goes back.
    serial: u64,
}

/// What the set keeps of a subtree it has an entry under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Written {
    /// How many units of the subtree the set has an entry in.
    units: u32,
    /// The number the subtree was given when the set came to have an
    /// entry under it ([`WriteSet::first_writes`]). A subtree that the
    /// set stopped having an entry under, and has one under again, has
    /// another.
    came: u64,
}

/// What the drops and the clears of one subtree say of the subtrees below
/// it, counted: how many take them, how many leave them, and how many say
/// nothing, so that the version which is read says
/// ([`WriteSet::taking`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Taken {
    takes: u32,
    leaves: u32,
    unsaid: u32,
}

impl Taken {
    /// The count of the drops and clears that say `rooted`.
    fn said(&mut self, rooted: Option<bool>) -> &mut u32 {
        match rooted {
            Some(true) => &mut self.takes,
            Some(false) => &mut self.leaves,
            None => &mut self.unsaid,
        }
    }
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
    /// The pairs of each move among `subtrees`, in their order.
    below: Vec<Vec<(Subtree, Subtree)>>,
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
                whole: merk.whole,
            })
            .collect()
    }

    /// The writes of flat records.
    pub(crate) fn records(&self) -> &[RecordWrite<'a>] {
        &self.records
    }

    /// What the operation did to subtrees as a whole. A move names no
    /// read: it was dated when it was stated.
    pub(crate) fn subtrees(&'a self) -> Vec<SubtreeWrite<'a>> {
        let mut below = self.below.iter();
        (self.subtrees.iter())
            .map(|entry| match *entry {
                SubtreeEntry::Enter {
                    subtree,
                    tree_type,
                    host,
                    if_absent,
                } => Entering {
                    subtree,
                    tree_type: self.arena.get(tree_type),
                    host: host.map(|(subtree, key, position)| HostPut {
                        subtree,
                        key: self.arena.get(key),
                        position,
                    }),
                    if_absent,
                }
                .write(),
                SubtreeEntry::Drop(subtree) => SubtreeWrite::Drop { subtree },
                // A drop names no read, as a move names none.
                SubtreeEntry::DropUnder {
                    subtree,
                    host: (above, key),
                } => SubtreeWrite::DropUnder {
                    subtree,
                    host: HostNode {
                        subtree: above,
                        key: self.arena.get(key),
                    },
                    read: None,
                },
                SubtreeEntry::DropIfEntered {
                    subtree,
                    host: (above, key),
                    ..
                } => SubtreeWrite::DropIfEntered {
                    subtree,
                    host: HostNode {
                        subtree: above,
                        key: self.arena.get(key),
                    },
                },
                SubtreeEntry::RemoveIfEntered(subtree) => SubtreeWrite::RemoveIfEntered { subtree },
                SubtreeEntry::NotUnder {
                    subtree,
                    host: (above, key),
                } => SubtreeWrite::NotUnder {
                    subtree,
                    host: HostNode {
                        subtree: above,
                        key: self.arena.get(key),
                    },
                },
                SubtreeEntry::Clear(subtree) => SubtreeWrite::Clear { subtree },
                SubtreeEntry::Convert {
                    subtree,
                    tree_type,
                    position,
                } => SubtreeWrite::Convert {
                    subtree,
                    tree_type: self.arena.get(tree_type),
                    position,
                },
                SubtreeEntry::Move {
                    from,
                    to,
                    host: (subtree, key, position),
                    ..
                } => SubtreeWrite::Move {
                    from,
                    to,
                    host: HostPut {
                        subtree,
                        key: self.arena.get(key),
                        position,
                    },
                    read: None,
                    below: below.next().expect("the pairs of each move"),
                },
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

    /// Whether no operation of the set writes anything: it has no entry,
    /// and all that its operations state of subtrees is drops if entered
    /// that took nothing, removals if entered, which have no entry
    /// here, and subtrees that are not under a node. Such a set holds
    /// what its operations found, which a layout
    /// judges again, and nothing for a commit to publish.
    pub(crate) fn writes_nothing(&self) -> bool {
        let nothing = |entry: &SubtreeEntry| {
            matches!(
                entry,
                SubtreeEntry::DropIfEntered { took: false, .. }
                    | SubtreeEntry::RemoveIfEntered(_)
                    | SubtreeEntry::NotUnder { .. }
            )
        };
        self.entries.is_empty() && self.subtrees.iter().all(nothing)
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
            self.unpass(id);
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
                    let written = self.written.get_mut(&unit.subtree);
                    let written = written.expect("the subtree of a unit is counted");
                    written.units -= 1;
                    if written.units == 0 {
                        self.came.remove(&written.came);
                        self.written.remove(&unit.subtree);
                    }
                }
            }
            if self.leaving.last() == Some(&id) {
                for cut in Self::cut(&self.arena, &entry) {
                    Self::unsay(&mut self.said, cut, mark.ops);
                }
                self.leaving.pop();
                let rooted = self
                    .taking
                    .pop()
                    .expect("each drop and clear says what it takes");
                let subtree = entry.unit.subtree;
                let taken = self.taken.get_mut(&subtree);
                let taken = taken.expect("a drop or a clear is counted");
                *taken.said(rooted) -= 1;
                if *taken == Taken::default() {
                    self.taken.remove(&subtree);
                }
                self.left += 1;
            }
            if self.moved.last() == Some(&id) {
                self.moved.pop();
                self.left += 1;
                let names = Names::of(self.arena.get(entry.key));
                for covered in names.covered() {
                    // A prefix that the move covers twice has it once, and
                    // is passed twice here.
                    let Some(moves) = self.covered.get_mut(&covered) else {
                        continue;
                    };
                    if moves.last().is_some_and(|(last, _)| *last == id) {
                        moves.pop();
                    }
                    if moves.is_empty() {
                        self.covered.remove(&covered);
                    }
                }
                for source in names.sources {
                    let source = Subtree::new(*source);
                    let named = self.sources.get_mut(&source);
                    let named = named.expect("a source of a move is counted");
                    *named -= 1;
                    if *named == 0 {
                        self.sources.remove(&source);
                    }
                }
                for name in names.named() {
                    let named = self.named.get_mut(&name);
                    let named = named.expect("a name of a move is counted");
                    *named -= 1;
                    if *named == 0 {
                        self.named.remove(&name);
                    }
                }
            }
        }
        for at in (mark.subtrees..self.subtrees.len()).rev() {
            if let SubtreeEntry::Enter { subtree, .. } = self.subtrees[at] {
                let enters = self.enters.get_mut(&subtree);
                let enters = enters.expect("an enter is indexed");
                let last = enters.pop();
                debug_assert_eq!(last.map(|(last, _)| last as usize), Some(at));
                if enters.is_empty() {
                    self.enters.remove(&subtree);
                }
            }
        }
        for entry in &self.subtrees[mark.subtrees..] {
            if let Some(named) = entry.says_of() {
                Self::unsay(&mut self.said, named, mark.ops);
            }
        }
        for merk in self.merks[mark.merks..]
            .iter()
            .filter(|merk| merk.root.is_some())
        {
            Self::unsay(&mut self.said, merk.subtree, mark.ops);
        }
        for at in (mark.ops..self.ops.len()).rev() {
            self.untag(at);
        }
        self.taken_out += 1;
        self.ops.truncate(mark.ops);
        self.merks.truncate(mark.merks);
        self.subtrees.truncate(mark.subtrees);
        self.arena.rewind(mark.arena);
        if self.ops.is_empty() {
            self.kept.clear();
        }
        #[cfg(any(test, feature = "held_reads"))]
        self.hold_said();
        #[cfg(any(test, feature = "held_reads"))]
        self.hold_unmade();
        true
    }

    /// Notes that the operation that is being added says something of
    /// `subtree` as a whole: with `rooted`, that it states the root of
    /// its Merk, and otherwise that it changes the subtree.
    fn note_said(&mut self, subtree: Subtree, rooted: bool) {
        let op = self.ops.len() - 1;
        let says = self.said.entry(subtree).or_default();
        let ops = match rooted {
            true => &mut says.rooted,
            false => &mut says.whole,
        };
        if ops.last() != Some(&op) {
            ops.push(op);
        }
    }

    /// Takes the operations from `from` on out of what `said` has for
    /// `subtree`: they are the last of each of its lists.
    fn unsay(said: &mut HashMap<Subtree, Says>, subtree: Subtree, from: usize) {
        let Some(says) = said.get_mut(&subtree) else {
            return;
        };
        for ops in [&mut says.whole, &mut says.rooted] {
            while ops.last().is_some_and(|op| *op >= from) {
                ops.pop();
            }
        }
        if says.whole.is_empty() && says.rooted.is_empty() {
            said.remove(&subtree);
        }
    }

    /// Takes the operation `at` out of the operations of the open scope
    /// it is of. It is the last of those: the operations of a scope are
    /// taken out from the newest back.
    fn untag(&mut self, at: usize) {
        let scope = self.ops[at].scope;
        if scope == 0 {
            return;
        }
        let ops = self.scoped.get_mut(&scope);
        let ops = ops.expect("the operation of an open scope is listed");
        let last = ops.pop();
        debug_assert_eq!(last, Some(at), "the newest of its scope");
        if ops.is_empty() {
            self.scoped.remove(&scope);
        }
    }

    /// The operations before `end` that [`Self::subtree`] and
    /// [`Self::rooted_before`] walk, newest first: every one that
    /// changes `subtree` as a whole, and the newest that states the
    /// root of its Merk.
    ///
    /// A walk over these answers as a walk over every operation before
    /// `end` does. An operation that neither changes the subtree nor
    /// states the root of its Merk is passed over by both bodies. A
    /// statement of the root that is older than another is read by
    /// neither. A walk that comes to the newer one has answered there
    /// (the walk of the root), or has its root from there or from a
    /// clear that is newer still, and takes none once it has one (the
    /// walk of the subtree). And a walk that ends before it comes to
    /// the newer one, at an operation that changes the subtree, does
    /// not come to an older one.
    fn said_before(&self, subtree: &Subtree, end: usize) -> impl Iterator<Item = usize> + '_ {
        let says = self.said.get(subtree);
        // Of a list whose last operation is before `end`, all is taken,
        // with no search for where to end it: so it is where a walk is
        // asked of every operation of the set.
        let before = |ops: &[usize]| match ops.last() {
            Some(last) if *last >= end => ops.partition_point(|op| *op < end),
            _ => ops.len(),
        };
        let whole = says.map_or(&[][..], |says| &says.whole[..before(&says.whole)]);
        let rooted = says.map_or(&[][..], |says| &says.rooted[..before(&says.rooted)]);
        let mut whole = whole.iter().rev().copied().peekable();
        let mut newest = rooted.last().copied();
        std::iter::from_fn(move || {
            // The newest statement of the root goes in at its place among
            // the operations that change the subtree, once.
            let next = match (whole.peek().copied(), newest) {
                (Some(changes), Some(states)) if states >= changes => {
                    newest = None;
                    if states == changes {
                        whole.next();
                    }
                    states
                }
                (Some(_), _) => whole.next()?,
                (None, states) => {
                    newest = None;
                    states?
                }
            };
            Some(next)
        })
    }

    /// How many times operations were taken out of the set. Whoever
    /// keeps what it learned of the subtrees the set has an entry under
    /// ([`Self::written_after`]) learns it again when this changed: a
    /// subtree that a drop or an enter of the transaction covered can be
    /// as the committed state has it again.
    pub(crate) fn taken_out(&self) -> u64 {
        self.taken_out
    }

    /// The subtrees that an operation of the set enters, in no order.
    pub(crate) fn entered(&self) -> impl Iterator<Item = Subtree> + '_ {
        self.enters.keys().copied()
    }

    /// Takes out every write (WT9).
    pub(crate) fn clear(&mut self) {
        if !self.ops.is_empty() {
            self.taken_out += 1;
        }
        self.entries.clear();
        self.ops.clear();
        self.merks.clear();
        self.subtrees.clear();
        self.leaving.clear();
        self.taking.clear();
        self.left += 1;
        self.moved.clear();
        self.sources.clear();
        self.covered.clear();
        self.taken.clear();
        self.named.clear();
        self.enters.clear();
        self.said.clear();
        self.scoped.clear();
        self.arena.clear();
        self.kept.clear();
        self.units.clear();
        self.written.clear();
        self.came.clear();
        self.met = 0;
        self.unmade = 0;
        self.listed = false;
    }

    /// The subtrees below the one a drop or a clear names that the
    /// transaction had written, and that the entry covers.
    fn cut<'a>(arena: &'a Arena, entry: &Entry) -> impl Iterator<Item = Subtree> + 'a {
        let cut: &[u8] = match entry.kind.leaves() {
            [] => &[],
            _ => arena.get(entry.key),
        };
        subtrees_of(cut)
    }

    /// The units an entry is indexed in: its own, or for a drop every
    /// column of its subtree, and every column of each subtree it cut.
    fn covered(arena: &Arena, entry: &Entry) -> Vec<Unit> {
        let subtree = entry.unit.subtree;
        if matches!(entry.kind, Kind::Moved) {
            let mut seen = HashSet::new();
            let covered = Names::of(arena.get(entry.key)).covered();
            let named = covered.filter(|subtree| seen.insert(*subtree));
            let units = |subtree| Column::ALL.map(|column| Unit::new(column, subtree));
            return named.flat_map(units).collect();
        }
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

    /// The index of `unit`, which is made where the set has none: the
    /// unit's subtree is counted among those the set has an entry under
    /// (`written`), and is numbered where this is its first unit.
    fn index_of(&mut self, unit: Unit) -> &mut UnitIndex {
        match self.units.entry(unit) {
            hash_map::Entry::Occupied(index) => index.into_mut(),
            hash_map::Entry::Vacant(free) => {
                match self.written.get_mut(&unit.subtree) {
                    Some(written) => written.units += 1,
                    None => {
                        self.first_writes += 1;
                        let came = self.first_writes;
                        self.written
                            .insert(unit.subtree, Written { units: 1, came });
                        self.came.insert(came, unit.subtree);
                    }
                }
                free.insert(UnitIndex::default())
            }
        }
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
                self.index_of(unit).ranges.push(id);
            }
        } else {
            let index = self.index_of(unit);
            entry.prev = match index.keys.get_mut(key) {
                Some(newest) => Some(std::mem::replace(newest, id)),
                None => {
                    index.keys.insert(key.into(), id);
                    None
                }
            };
        }
        self.entries.push(entry);
        if self.unmade > 0 && Self::passes(unit, &kind) {
            // The write of the same record before this one is passed.
            if let Some(prior) = self.before(id) {
                self.set_unmade(prior, false);
            }
        }
        if matches!(kind, Kind::Node(node) if node.leaves_stale()) {
            self.set_unmade(id, true);
        }
        id
    }

    /// Whether a write of `kind` in `unit` passes the write of its record
    /// before it: a put or a delete of one key in a data column, where
    /// the nodes are. A write of another column passes no put of a node.
    fn passes(unit: Unit, kind: &Kind) -> bool {
        unit.column == Column::Data && !kind.is_range()
    }

    /// The write of the same record before the entry `id`, which is the
    /// newest of the set: what [`Self::prior`] answers. The moves of the
    /// record's subtree are gone through only where one stands between
    /// the two: for the first write of a record under a prefix since a
    /// move named the prefix. That costs what a read of the record costs
    /// then ([`Self::own`]).
    fn before(&self, id: EntryId) -> Option<EntryId> {
        let entry = self.entry(id);
        let moved = self.covered.get(&entry.unit.subtree);
        match (moved.and_then(|moves| moves.last()), entry.prev) {
            (None, prev) => prev,
            (Some((moved, _)), Some(prev)) if prev > *moved => Some(prev),
            (Some(_), _) => self.prior(id),
        }
    }

    /// Lists the entry `id` among the puts of its unit that left a hash
    /// stale, or takes it from them.
    fn set_unmade(&mut self, id: EntryId, listed: bool) {
        let unit = self.entry(id).unit;
        let Some(index) = self.units.get_mut(&unit) else {
            return;
        };
        match listed {
            true if index.stale.insert(id) => {
                self.unmade += 1;
                self.listed = true;
            }
            false if index.stale.remove(&id) => self.unmade -= 1,
            _ => {}
        }
    }

    /// The entry `id`, the newest, is about to be taken out: it is listed
    /// no more, and the write of the same record that it passed is listed
    /// again if that left a hash stale.
    fn unpass(&mut self, id: EntryId) {
        let entry = self.entry(id);
        if !self.listed || !Self::passes(entry.unit, &entry.kind) {
            return;
        }
        self.set_unmade(id, false);
        let passed = self.before(id).filter(
            |prior| matches!(self.entry(*prior).kind, Kind::Node(node) if node.leaves_stale()),
        );
        if let Some(passed) = passed {
            self.set_unmade(passed, true);
        }
    }

    /// The puts of nodes of the Merk `subtree` that left a hash stale and
    /// are the transaction's newest write of their record, each with its
    /// key, oldest first within each prefix the Merk was read under.
    ///
    /// The list that is kept for this holds each such put from when it is
    /// added: a later write of the same record takes it out, as the
    /// transaction read the record's subtree then ([`Self::prior`]), and
    /// puts it back where that write is taken out. What a drop or a clear
    /// took is passed here with no step for each. A range delete covers a
    /// put only if it is newer, and only those are asked.
    ///
    /// So a call costs the prefixes the Merk was read under, as a read of
    /// one record of it does ([`Self::own`]); for each of them a search
    /// among its range deletes, and a step for each range delete since
    /// its newest drop or clear; and for each put that is returned a
    /// search among those, and a comparison with each that is newer than
    /// the put.
    ///
    /// A put of a node that is in no tree left no hash marked on the
    /// pages, and is returned all the same.
    pub(crate) fn unmade(&self, subtree: Subtree) -> Vec<(EntryId, &[u8])> {
        let mut puts = Vec::new();
        if self.unmade > 0 {
            self.list_unmade(subtree, &mut puts);
        }
        #[cfg(any(test, feature = "held_reads"))]
        {
            let listed: BTreeSet<EntryId> = puts.iter().map(|(id, _)| *id).collect();
            let every = self.unmade_by_every_key(subtree);
            assert_eq!(listed, every, "the puts that left a hash stale");
        }
        puts
    }

    /// Adds to `puts` what [`Self::unmade`] returns for `subtree`.
    fn list_unmade<'a>(&'a self, subtree: Subtree, puts: &mut Vec<(EntryId, &'a [u8])>) {
        let stretches = match self.moved.is_empty() {
            true => vec![Stretch {
                subtree,
                after: None,
                before: None,
            }],
            false => self.stretches(subtree, None).0,
        };
        // The range deletes of the stretches that were passed, which are
        // the newer ones: each is newer than every entry of the stretch
        // at hand.
        let mut later: Vec<EntryId> = Vec::new();
        for (at, stretch) in stretches.iter().enumerate() {
            let Some(index) = self.units.get(&Unit::data(stretch.subtree)) else {
                continue;
            };
            // The puts that the stretch lists, oldest first.
            let low = match stretch.after {
                Some(id) => Bound::Excluded(id),
                None => Bound::Unbounded,
            };
            let listed = index.stale.range((low, Bound::Unbounded));
            let listed = listed.take_while(|id| stretch.before.is_none_or(|before| **id < before));
            let mut listed = listed.copied().peekable();
            // After the last stretch nothing older is asked for. So what
            // is older than the oldest put it lists is not looked at: a
            // drop or a clear from before a put did not take it, and a
            // range delete from before it does not cover it.
            let oldest = match (at + 1 == stretches.len(), listed.peek()) {
                (true, None) => break,
                (true, Some(oldest)) => Some(*oldest),
                (false, _) => None,
            };
            // The ranges that the stretch holds. They are in the order of
            // their ids, and no move is among them: a move under the
            // prefix is where a stretch ends.
            let ranges = &index.ranges[..];
            let older = |than: EntryId| ranges.partition_point(|id| *id < than);
            let from = stretch
                .after
                .map_or(0, |after| older(after.saturating_add(1)));
            let from = oldest.map_or(from, |oldest| from.max(older(oldest)));
            let to = stretch.before.map_or(ranges.len(), older);
            let held = ranges.get(from..to).unwrap_or(&[]);
            let whole = |id: &EntryId| {
                #[cfg(test)]
                counted::cut();
                matches!(self.entry(*id).kind, Kind::Dropped | Kind::Cleared)
            };
            let cut = held.iter().rposition(whole);
            let deletes = &held[cut.map_or(0, |cut| cut + 1)..];
            let taken = cut.map(|cut| held[cut]);
            for id in listed.skip_while(|id| taken.is_some_and(|taken| *id < taken)) {
                let key = self.key(id);
                let newer = &deletes[deletes.partition_point(|range| *range < id)..];
                let mut newer = newer.iter().chain(&later);
                let covers = |range: &EntryId| {
                    #[cfg(test)]
                    counted::range();
                    self.range_holds(*range, key)
                };
                if !newer.any(covers) {
                    puts.push((id, key));
                }
            }
            if cut.is_some() {
                // What is older went with the drop or the clear.
                break;
            }
            if at + 1 < stretches.len() {
                later.extend_from_slice(deletes);
            }
        }
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
    /// clear is one entry each (CF5), and they come first, and then one
    /// entry for the moves. Then, in the order of the batch, for each
    /// Merk its nodes and then its deletes, and the records last. A node
    /// is one entry under its key whether or not its tree reaches it: an
    /// unlinked record is a record of its key (`super::unlinked`). A
    /// subtree that is entered or stated anew is no entry: it writes no
    /// record. The batch has passed
    /// [`check`](super::check::check).
    ///
    /// `empty` says of each Merk of the batch whether it had no root in the
    /// transaction's view before the operation. `cut` has the subtrees the
    /// transaction had written that are below one the batch drops or
    /// clears: the first of those entries covers them. `left` has the
    /// subtrees that left with the moves of the batch, which their entry
    /// keeps. `stored` has the values that a load of the batch put into
    /// the value log, as [`Again::stored`] has them: the entry of such a
    /// node keeps where the value is, and not its bytes. `took` has the
    /// subtrees that the batch drops if they are entered
    /// ([`SubtreeWrite::DropIfEntered`]) and that it took where it was
    /// taken: each is a drop to the set, and the others are no entry.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add(
        &mut self,
        batch: &Batch<'_>,
        first_write: CommitVersion,
        empty: &[bool],
        cut: &[Subtree],
        left: &[Subtree],
        stored: &[(u32, StoredValue)],
        took: &[Subtree],
    ) -> EntryId {
        let took: HashSet<Subtree> = took.iter().copied().collect();
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
            kinds: None,
            both: false,
        });
        for write in batch.subtrees {
            let entry = match *write {
                SubtreeWrite::Enter {
                    subtree,
                    tree_type,
                    host,
                }
                | SubtreeWrite::EnterIfAbsent {
                    subtree,
                    tree_type,
                    host,
                } => SubtreeEntry::Enter {
                    subtree,
                    tree_type: self.arena.put(tree_type),
                    host: host.map(|host| (host.subtree, self.arena.put(host.key), host.position)),
                    if_absent: matches!(write, SubtreeWrite::EnterIfAbsent { .. }),
                },
                SubtreeWrite::Drop { subtree } => SubtreeEntry::Drop(subtree),
                SubtreeWrite::DropUnder { subtree, host, .. } => SubtreeEntry::DropUnder {
                    subtree,
                    host: (host.subtree, self.arena.put(host.key)),
                },
                SubtreeWrite::DropIfEntered { subtree, host } => SubtreeEntry::DropIfEntered {
                    subtree,
                    host: (host.subtree, self.arena.put(host.key)),
                    took: took.contains(&subtree),
                },
                SubtreeWrite::RemoveIfEntered { subtree } => SubtreeEntry::RemoveIfEntered(subtree),
                SubtreeWrite::NotUnder { subtree, host } => SubtreeEntry::NotUnder {
                    subtree,
                    host: (host.subtree, self.arena.put(host.key)),
                },
                SubtreeWrite::Clear { subtree } => SubtreeEntry::Clear(subtree),
                SubtreeWrite::Convert {
                    subtree,
                    tree_type,
                    position,
                } => SubtreeEntry::Convert {
                    subtree,
                    tree_type: self.arena.put(tree_type),
                    position,
                },
                SubtreeWrite::Move {
                    from,
                    to,
                    host,
                    below,
                    ..
                } => {
                    let pairs: Vec<Subtree> =
                        (below.iter()).flat_map(|(from, to)| [*from, *to]).collect();
                    SubtreeEntry::Move {
                        from,
                        to,
                        host: (host.subtree, self.arena.put(host.key), host.position),
                        below: self.arena.put(&pairs.concat_bytes()),
                    }
                }
            };
            if let SubtreeEntry::Enter { subtree, host, .. } = entry {
                let at = self.subtrees.len() as u32;
                let host = host.map(|(host, ..)| host);
                self.enters.entry(subtree).or_default().push((at, host));
            }
            if let Some(named) = entry.says_of() {
                self.note_said(named, false);
            }
            self.subtrees.push(entry);
        }
        let mut cut = cut.concat_bytes();
        for write in batch.subtrees {
            let kind = match write {
                SubtreeWrite::Drop { .. } | SubtreeWrite::DropUnder { .. } => Kind::Dropped,
                SubtreeWrite::DropIfEntered { subtree, .. } if took.contains(subtree) => {
                    Kind::Dropped
                }
                SubtreeWrite::Clear { .. } => Kind::Cleared,
                SubtreeWrite::Enter { .. }
                | SubtreeWrite::EnterIfAbsent { .. }
                | SubtreeWrite::DropIfEntered { .. }
                | SubtreeWrite::RemoveIfEntered { .. }
                | SubtreeWrite::NotUnder { .. }
                | SubtreeWrite::Convert { .. }
                | SubtreeWrite::Move { .. } => continue,
            };
            let below = std::mem::take(&mut cut);
            let subtree = write.subtree();
            let id = self.push(Unit::data(subtree), &below, kind, first_write);
            // What the entry cut went with its subtree.
            for cut in subtrees_of(&below) {
                self.note_said(cut, false);
            }
            // A drop takes what is below its subtree (CV9). A clear takes
            // it where its Merk has a node, which the operations before
            // this one say: a clear comes before the change sets of its
            // own operation.
            let rooted = match kind {
                Kind::Cleared => self.rooted_before(&subtree, self.ops.len() - 1, true),
                _ => Some(true),
            };
            self.leaving.push(id);
            self.taking.push(rooted);
            *self.taken.entry(subtree).or_default().said(rooted) += 1;
            self.left += 1;
        }
        // The moves of a batch are made together, and are one entry.
        let moves: Vec<Moving<'_>> = batch.moves().collect();
        if let Some(first) = moves.first() {
            let names = Names::encode(&moves, left);
            let id = self.push(Unit::data(first.from), &names, Kind::Moved, first_write);
            self.moved.push(id);
            self.left += 1;
            for source in moves.iter().flat_map(Moving::sources) {
                *self.sources.entry(source).or_default() += 1;
            }
            // What the move did under each prefix it covers: a prefix
            // that a subtree is brought to is that, whatever else
            // names it.
            let names = Names::of(self.arena.get(self.entries[id as usize].key));
            for name in names.named() {
                *self.named.entry(name).or_default() += 1;
            }
            let brought = (names.targets.iter().zip(names.sources))
                .map(|(to, from)| (Subtree::new(*to), Some(Subtree::new(*from))));
            let taken =
                (names.sources.iter().chain(names.left)).map(|from| (Subtree::new(*from), None));
            for (at, place) in brought.chain(taken) {
                let covering = self.covered.entry(at).or_default();
                if covering.last().is_none_or(|(last, _)| *last != id) {
                    covering.push((id, place));
                }
            }
        }
        let first = self.entries.len() as EntryId;
        let mut stored = stored.iter().peekable();
        for (merk, empty) in batch.merks.iter().zip(empty) {
            let unit = Unit::data(merk.subtree);
            for node in merk.nodes {
                let place = self.entries.len() as EntryId - first;
                let in_log = stored.next_if(|(at, _)| *at == place);
                let hosted = match node.hosted {
                    Hosted::Keep => HostedEntry::Keep,
                    Hosted::Set(subtrees) => {
                        let bytes = subtrees.concat_bytes();
                        HostedEntry::Set(self.arena.put(&bytes))
                    }
                };
                let value = match (node.value, in_log) {
                    (ValuePut::Keep, _) => ValueEntry::Keep,
                    (ValuePut::Value(_), Some((_, stored))) => {
                        ValueEntry::Stored(self.arena.put(&stored.encode()))
                    }
                    (ValuePut::Value(value), None) => ValueEntry::Value(self.arena.put(value)),
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
            if root.is_some() {
                self.note_said(merk.subtree, true);
            }
            self.merks.push(MerkEntry {
                subtree: merk.subtree,
                root,
                anchored: merk.anchored,
                made: *empty && matches!(merk.root, RootPut::Set(Some(_))),
                whole: merk.whole,
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
    pub(crate) fn tag_last(&mut self, scope: ScopeId, laid: bool) {
        let at = self
            .ops
            .len()
            .checked_sub(1)
            .expect("an operation was added");
        self.untag(at);
        (self.ops[at].scope, self.ops[at].laid) = (scope, laid);
        if scope != 0 {
            self.scoped.entry(scope).or_default().push(at);
        }
    }

    /// Whether the transaction's frames show the operation `op`. Frames
    /// that are made from the set pass over an operation they do not show.
    pub(crate) fn laid(&self, op: usize) -> bool {
        self.ops[op].laid
    }

    /// The operations of the open scope `scope`, oldest first. Zero is
    /// no open scope: it names the transaction's own operations, which
    /// are found among all of them.
    pub(crate) fn ops_of(&self, scope: ScopeId) -> Vec<usize> {
        if scope == 0 {
            return self.ops_among_all(0);
        }
        let ops = self.scoped.get(&scope).cloned().unwrap_or_default();
        #[cfg(any(test, feature = "held_reads"))]
        assert_eq!(ops, self.ops_among_all(scope), "the operations of a scope");
        ops
    }

    /// The operations that name `scope`, oldest first, found among all
    /// of the set: [`Self::ops_of`] as it was made before the set kept
    /// the operations of each open scope.
    fn ops_among_all(&self, scope: ScopeId) -> Vec<usize> {
        let ops = self.ops.iter().enumerate();
        ops.filter_map(|(at, op)| (op.scope == scope).then_some(at))
            .collect()
    }

    /// Makes the operations of the scope `scope` the transaction's (SC6),
    /// all of them laid out.
    pub(crate) fn merge_scope(&mut self, scope: ScopeId) {
        #[cfg(any(test, feature = "held_reads"))]
        self.hold_scoped();
        for op in self.scoped.remove(&scope).unwrap_or_default() {
            let op = &mut self.ops[op];
            (op.scope, op.laid) = (0, true);
        }
    }

    /// The operations of the open scope `scope` are laid out: the
    /// transaction's frames show them from here on.
    pub(crate) fn lay_scope(&mut self, scope: ScopeId) {
        #[cfg(any(test, feature = "held_reads"))]
        self.hold_scoped();
        for op in self.scoped.get(&scope).into_iter().flatten() {
            self.ops[*op].laid = true;
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

    /// Takes out the operation `op` and every one after it.
    fn rewind_before(&mut self, op: usize) {
        let this = self.ops[op];
        let before = Mark {
            entries: this.entries as usize,
            ops: op,
            merks: this.merks as usize,
            subtrees: this.subtrees as usize,
            arena: this.arena,
            last: op
                .checked_sub(1)
                .map_or(0, |before| self.ops[before].serial),
            pending: 0,
        };
        let removed = self.rewind(before);
        debug_assert!(removed, "the place before an operation of the set");
    }

    /// Calls `apply` with the operation `op` as the batch it was, and with
    /// what [`Self::made`] says of its Merks and where the values are that
    /// a load put into the value log ([`Again`]): for applying it again by
    /// key.
    pub(crate) fn with_batch<R>(
        &self,
        op: usize,
        apply: impl FnOnce(&Batch<'_>, Again<'_>) -> R,
    ) -> R {
        let hosted = self.hosted_lists(op);
        let batch = self.batch(op, &hosted);
        let (merks, subtrees) = (batch.merks(), batch.subtrees());
        let batch = Batch {
            records: batch.records(),
            merks: &merks,
            subtrees: &subtrees,
        };
        let (made, stored, took) = (self.made(op), self.stored(op), self.op_took(op));
        apply(
            &batch,
            Again {
                made: &made,
                stored: &stored,
                took: &took,
            },
        )
    }

    /// Whether an operation of the set holds a load: a change set with a
    /// node that said it is the whole Merk.
    pub(crate) fn has_loads(&self) -> bool {
        self.merks.iter().any(|merk| merk.whole && merk.nodes > 0)
    }

    /// The loads of the set: the subtree of each, and the entry of the
    /// first node its change set states, which has the first-write
    /// version of the load's operation.
    fn loads(&self) -> Vec<(Subtree, EntryId)> {
        let mut loads = Vec::new();
        if !self.has_loads() {
            return loads;
        }
        for op in 0..self.ops.len() {
            let (from, to, merks_from, merks_to) = self.op_bounds(op);
            // What an operation did to subtrees as a whole stands before
            // the entries of its change sets.
            let whole = |entry: &&Entry| entry.kind.is_whole();
            let mut first = from + self.entries[from..to].iter().take_while(whole).count();
            for merk in &self.merks[merks_from..merks_to] {
                if merk.whole && merk.nodes > 0 {
                    loads.push((merk.subtree, first as EntryId));
                }
                first += (merk.nodes + merk.removed) as usize;
            }
        }
        loads
    }

    /// The values of the operation `op` that a load put into the value
    /// log, each by the place of its node among the entries of the
    /// operation's change sets ([`Again::stored`]).
    fn stored(&self, op: usize) -> Vec<(u32, StoredValue)> {
        let (from, to, ..) = self.op_bounds(op);
        let entries = self.entries[from..to].iter();
        let entries = entries.skip_while(|entry| entry.kind.is_whole());
        let stored = entries
            .enumerate()
            .filter_map(|(place, entry)| match entry.kind {
                Kind::Node(NodeEntry {
                    value: ValueEntry::Stored(span),
                    ..
                }) => Some((place as u32, StoredValue::decode(self.arena.get(span)))),
                _ => None,
            });
        stored.collect()
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
    /// overlap set it had, and a copy it took of a committed value is that
    /// copy again. That is done in a copy of the set, which takes the
    /// set's place when all of them are in: the cost is that of the whole
    /// write set, once.
    pub(crate) fn remove_scope(&mut self, scope: ScopeId) -> bool {
        #[cfg(any(test, feature = "held_reads"))]
        self.hold_scoped();
        let Some(own) = self.scoped.get(&scope) else {
            return false;
        };
        let first = own[0];
        if own.len() == self.ops.len() - first {
            self.rewind_before(first);
            return true;
        }
        let mut kept = self.clone();
        kept.rewind_before(first);
        for op in (first..self.ops.len()).filter(|op| self.ops[*op].scope != scope) {
            let was = self.ops[op];
            // A Merk that the operation gave its first root is stated as
            // one that had none, and every other root is stated as it was
            // kept: both come out as they went in.
            let start = kept.entries.len();
            self.with_batch(op, |batch, again| {
                let (cut, left) = (self.op_cut(op), self.op_left(op));
                kept.add(
                    batch,
                    was.first_write,
                    again.made,
                    &cut,
                    &left,
                    again.stored,
                    &self.op_took(op),
                );
            });
            kept.tag_last(was.scope, was.laid);
            let added = kept.ops.last_mut().expect("it was added");
            added.serial = was.serial;
            (added.kinds, added.both) = (was.kinds, was.both);
            let (from, to, ..) = self.op_bounds(op);
            for (id, was) in (start..).zip(&self.entries[from..to]) {
                kept.entries[id].met = was.met;
                kept.met += usize::from(was.met);
                // A copy of a committed value went in as a value that was
                // stated. It is the copy again: the transaction did not
                // state it ([`Self::states_value`]).
                if let Kind::Node(NodeEntry {
                    value: ValueEntry::Kept(span),
                    ..
                }) = was.kind
                {
                    kept.keep(id as EntryId, self.kept.get(span));
                }
            }
        }
        kept.serial = self.serial;
        *self = kept;
        #[cfg(any(test, feature = "held_reads"))]
        self.hold_said();
        #[cfg(any(test, feature = "held_reads"))]
        self.hold_unmade();
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

    /// The subtrees that the operation `op` dropped because it found
    /// them entered ([`SubtreeWrite::DropIfEntered`]): what [`Self::add`]
    /// was given as `took`.
    fn op_took(&self, op: usize) -> Vec<Subtree> {
        let took = self.op_subtrees(op).iter().filter_map(|entry| match entry {
            SubtreeEntry::DropIfEntered { took: true, .. } => entry.dropped(),
            _ => None,
        });
        took.collect()
    }

    /// Whether the operation of the entry `id` removes under the subtree
    /// of the entry if the subtree is entered
    /// ([`SubtreeWrite::RemoveIfEntered`]).
    fn removed_if_entered(&self, id: EntryId) -> bool {
        let subtree = self.entry(id).unit.subtree;
        let removes = |entry: &SubtreeEntry| matches!(entry, SubtreeEntry::RemoveIfEntered(named) if *named == subtree);
        self.op_subtrees(self.op_of(id)).iter().any(removes)
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
        // The drops, the clears and the moves are applied from what the
        // operation did to subtrees, and their entries are passed over.
        let entries = self.entries[from..to].iter();
        let mut entries = entries.skip_while(|entry| entry.kind.is_whole());
        let pairs = |entry: &SubtreeEntry| match entry {
            SubtreeEntry::Move { below, .. } => {
                let mut named = subtrees_of(self.arena.get(*below));
                let mut pairs = Vec::new();
                while let (Some(from), Some(to)) = (named.next(), named.next()) {
                    pairs.push((from, to));
                }
                Some(pairs)
            }
            _ => None,
        };
        let below = subtrees.iter().filter_map(pairs).collect();
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
                        // The bytes are in the value log, and whoever
                        // applies the batch is told where ([`Again`]).
                        ValueEntry::Stored(_) => ValuePut::Value(&[]),
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
                Kind::Node(_) | Kind::Remove | Kind::Dropped | Kind::Cleared | Kind::Moved => {
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
            below,
            arena: &self.arena,
        }
    }

    /// What the transaction has written in `unit` since it last dropped or
    /// cleared the unit's subtree: whether Merk nodes or removals of
    /// them, and whether flat records, their deletes among them. With
    /// `all` both are looked for, among every write and not the newest
    /// under each key alone. Without it the first kind that is found
    /// among the newest is the answer, which is all there is where no
    /// operation was taken by the tree type of a subtree
    /// ([`Self::admits`]).
    ///
    /// What it wrote under the prefix a move took the subtree from, before
    /// the move, is the subtree's still ([`Self::stretches`]).
    fn held(&self, unit: Unit, all: bool) -> (bool, bool) {
        let kind = |id: EntryId| match self.entry(id).kind {
            // What is removed under a subtree if it is entered is removed
            // from whatever the subtree holds then.
            Kind::Remove | Kind::Delete | Kind::Range { .. } if self.removed_if_entered(id) => None,
            Kind::Node(_) | Kind::Remove => Some(Held::Nodes),
            Kind::Put(_) | Kind::Delete | Kind::Range { .. } => Some(Held::Flat),
            Kind::Dropped | Kind::Cleared | Kind::Moved => None,
        };
        let stretches = match self.moved.is_empty() {
            true => vec![Stretch {
                subtree: unit.subtree,
                after: None,
                before: None,
            }],
            false => self.stretches(unit.subtree, None).0,
        };
        let (mut nodes, mut flat) = (false, false);
        for stretch in &stretches {
            let Some(index) = self.units.get(&Unit::new(unit.column, stretch.subtree)) else {
                continue;
            };
            // What the subtree held when it was dropped or cleared went
            // with that, and so did all that is older.
            let leaves =
                |id: &&EntryId| stretch.holds(**id) && !self.entry(**id).kind.leaves().is_empty();
            let since = index.ranges.iter().rev().find(leaves).copied();
            let later = |id: &EntryId| since.is_none_or(|since| *id > since);
            let ranges = (index.ranges.iter().copied()).filter(|id| stretch.holds(*id));
            // What is removed under a key if the subtree is entered says
            // nothing of what the subtree holds: the write under the key
            // before it does.
            let points = (index.keys.keys()).filter_map(|key| {
                let mut point = self.point_in(index, key, stretch);
                while let Some(id) = point.filter(|id| self.removed_if_entered(*id)) {
                    point = self.entry(id).prev.filter(|prev| stretch.holds(*prev));
                }
                point
            });
            for newest in ranges.chain(points).filter(later) {
                let mut entry = Some(newest);
                while let Some(id) = entry {
                    match kind(id) {
                        Some(Held::Nodes) => nodes = true,
                        Some(Held::Flat) => flat = true,
                        None => {}
                    }
                    if (nodes || flat) && (!all || (nodes && flat)) {
                        return (nodes, flat);
                    }
                    // With `all`, what was written under the key before
                    // was written too: a read takes the newest, and this
                    // asks for every one.
                    entry = match all {
                        true => {
                            (self.entry(id).prev).filter(|prev| stretch.holds(*prev) && later(prev))
                        }
                        false => None,
                    };
                }
            }
            if since.is_some() {
                break;
            }
        }
        (nodes, flat)
    }

    /// Whether an operation of the set moves a subtree.
    pub(crate) fn has_moves(&self) -> bool {
        !self.moved.is_empty()
    }

    /// What the move `id` holds in its key.
    fn names(&self, id: EntryId) -> Names<'_> {
        Names::of(self.key(id))
    }

    /// Whether a move of the set names `subtree`, on either side, or took
    /// it out as one that left with it.
    pub(crate) fn moves(&self, subtree: &Subtree) -> bool {
        let moves = self.covered.contains_key(subtree);
        #[cfg(any(test, feature = "held_reads"))]
        assert_eq!(moves, self.moves_by_every_name(subtree), "{subtree:?}");
        moves
    }

    /// The newest move among those older than `before`, or of all,
    /// that did something under `at`, with what it did: brought a
    /// subtree there, from the prefix that is given, or took one from
    /// there and brought none.
    fn moved_under(
        &self,
        at: &Subtree,
        before: Option<EntryId>,
    ) -> Option<(EntryId, Option<Subtree>)> {
        // The moves that cover a prefix are in the order of their ids.
        let covering = self
            .covered
            .get(at)
            .map_or(&[][..], |covering| &covering[..]);
        let older = match before {
            Some(before) => covering.partition_point(|(id, _)| {
                #[cfg(test)]
                counted::moved();
                *id < before
            }),
            None => covering.len(),
        };
        let moved = older.checked_sub(1).map(|newest| covering[newest]);
        #[cfg(any(test, feature = "held_reads"))]
        assert_eq!(moved, self.moved_under_by_every_name(at, before), "{at:?}");
        moved
    }

    /// Whether a move of the set took a subtree from `prefix`: whether
    /// [`Self::moved_from`] has it, asked with one lookup.
    pub(crate) fn took_from(&self, prefix: &Subtree) -> bool {
        self.sources.contains_key(prefix)
    }

    /// The prefixes that the moves of the set took a subtree from.
    pub(crate) fn moved_from(&self) -> Vec<Subtree> {
        let sources = (self.moved.iter()).flat_map(|id| self.names(*id).sources);
        sources.copied().map(Subtree::new).collect()
    }

    /// Whether a move of the set names `prefix`, on either side: whether
    /// [`Self::moved_names`] has it, asked with one lookup.
    pub(crate) fn named_by_a_move(&self, prefix: &Subtree) -> bool {
        self.named.contains_key(prefix)
    }

    /// Every prefix that the moves of the set name, on either side.
    pub(crate) fn moved_names(&self) -> Vec<Subtree> {
        let named = |id: &EntryId| self.names(*id).named().collect::<Vec<_>>();
        self.moved.iter().flat_map(named).collect()
    }

    /// The subtrees that left with the moves of the operation `op`: what
    /// [`Self::add`] was given as `left`.
    fn op_left(&self, op: usize) -> Vec<Subtree> {
        let (from, to, ..) = self.op_bounds(op);
        let moved = (from..to).filter(|id| matches!(self.entries[*id].kind, Kind::Moved));
        let left = moved.flat_map(|id| self.names(id as EntryId).left);
        left.copied().map(Subtree::new).collect()
    }

    /// Where the transaction's writes of `subtree` are, as the subtree is
    /// read before the entry `before`, or now with none: the stretches of
    /// its entries, newest first, and the prefix the committed state has
    /// the subtree's records under, if it has them at all.
    ///
    /// A move that brought a subtree to a prefix ends a stretch: what is
    /// older is under the prefix that subtree had before, up to that
    /// move. A move that took a subtree from a prefix and brought none
    /// ends the last stretch: nothing older is read under that prefix,
    /// and nothing of the committed state.
    fn stretches(
        &self,
        subtree: Subtree,
        before: Option<EntryId>,
    ) -> (Vec<Stretch>, Option<Subtree>) {
        #[cfg(test)]
        counted::stretches();
        let mut stretches = Vec::new();
        let (mut at, mut before) = (subtree, before);
        loop {
            let moved = self.moved_under(&at, before);
            let after = moved.map(|(id, _)| id);
            stretches.push(Stretch {
                subtree: at,
                after,
                before,
            });
            match moved {
                None => return (stretches, Some(at)),
                Some((_, None)) => return (stretches, None),
                Some((id, Some(from))) => (at, before) = (from, Some(id)),
            }
        }
    }

    /// The prefix the committed state has the records of `subtree` under,
    /// as the transaction reads the subtree now: the subtree's own where
    /// no move of the transaction named it, the prefix that the moves
    /// took it from, and `None` where a move took a subtree from the
    /// prefix and none was brought there since.
    ///
    /// Whether the transaction reads those records is not said here: it
    /// may have dropped the subtree since, alone or with one above it
    /// (`WriteTxn::committed_side`).
    pub(crate) fn committed_under(&self, subtree: Subtree) -> Option<Subtree> {
        match self.moved.is_empty() {
            true => Some(subtree),
            false => self.stretches(subtree, None).1,
        }
    }

    /// The prefix of [`Self::committed_under`], for the subtree of the
    /// entry `id` as it was read when that entry was written.
    pub(crate) fn committed_under_at(&self, id: EntryId) -> Option<Subtree> {
        let subtree = self.entry(id).unit.subtree;
        match self.moved.is_empty() {
            true => Some(subtree),
            false => self.stretches(subtree, Some(id)).1,
        }
    }

    /// The newest entry of the record key `key` among those of `index`
    /// that `stretch` holds.
    fn point_in(&self, index: &UnitIndex, key: &[u8], stretch: &Stretch) -> Option<EntryId> {
        let mut point = index.keys.get(key).copied();
        while let Some(id) = point.filter(|id| !stretch.holds(*id)) {
            // What is older than the stretch is older than all it holds.
            point = self
                .entry(id)
                .prev
                .filter(|_| stretch.after.is_none_or(|after| id > after));
        }
        point
    }

    /// The newest range delete among those of `index` that `stretch` holds
    /// and that contains `key`.
    fn range_in(&self, index: &UnitIndex, key: &[u8], stretch: &Stretch) -> Option<EntryId> {
        let held = index
            .ranges
            .iter()
            .rev()
            .copied()
            .filter(|id| stretch.holds(*id));
        held.into_iter().find(|range| self.range_holds(*range, key))
    }

    /// The entry before `id` for the same record, as the transaction read
    /// the record's subtree when it wrote `id`.
    fn prior(&self, id: EntryId) -> Option<EntryId> {
        let entry = self.entry(id);
        if self.moved.is_empty() {
            return entry.prev;
        }
        let (stretches, _) = self.stretches(entry.unit.subtree, Some(id));
        let key = self.key(id);
        stretches.iter().find_map(|stretch| {
            let unit = Unit::new(entry.unit.column, stretch.subtree);
            self.point_in(self.units.get(&unit)?, key, stretch)
        })
    }

    /// The subtrees the transaction dropped or cleared, oldest first.
    pub(crate) fn leaving(&self) -> impl Iterator<Item = Subtree> + '_ {
        (self.leaving.iter()).map(|id| self.entry(*id).unit.subtree)
    }

    /// [`Self::leaving`], each with whether it takes the subtrees below
    /// its own. A drop does. A clear of a Merk with a node does, and one
    /// of a Merk with no node leaves them entered
    /// (`lignin_tree::directory`): which of the two it is, the writes of
    /// the transaction from before the clear say where they say
    /// something of the Merk's root. An enter of the subtree if it was
    /// absent says nothing of it: it leaves a Merk that is there as it
    /// is. `None` is for a clear of a Merk of whose root those writes say
    /// nothing: the committed version that is read says there, where it
    /// has the subtree (`Opened::below_taken`).
    pub(crate) fn taking(&self) -> Vec<(Subtree, Option<bool>)> {
        let taking = self.leaving.iter().zip(&self.taking);
        taking
            .map(|(id, rooted)| (self.entry(*id).unit.subtree, *rooted))
            .collect()
    }

    /// Whether the transaction dropped or cleared a subtree.
    pub(crate) fn has_leaves(&self) -> bool {
        !self.leaving.is_empty()
    }

    /// Whether a drop or a clear of `subtree` that the set holds takes
    /// the subtrees below it, where `rooted` says whether the version
    /// that is read has the Merk under `subtree` with a node: whether
    /// [`Self::taking`] has the subtree as one that takes them, or as
    /// one of which that version says and `rooted`. It is asked with
    /// one lookup.
    pub(crate) fn takes_below(&self, subtree: &Subtree, rooted: bool) -> bool {
        let taken = self.taken.get(subtree);
        taken.is_some_and(|taken| taken.takes > 0 || (rooted && taken.unsaid > 0))
    }

    /// [`Self::taking`], of the operations `ops` only.
    pub(crate) fn taking_in(&self, ops: std::ops::Range<usize>) -> Vec<(Subtree, Option<bool>)> {
        let taking = self.leaving.iter().zip(&self.taking);
        let taking = taking.filter(|(id, _)| ops.contains(&self.op_of(**id)));
        taking
            .map(|(id, rooted)| (self.entry(*id).unit.subtree, *rooted))
            .collect()
    }

    /// A number that is another whenever the transaction drops or clears a
    /// subtree, or takes such a write out: whoever keeps what it found
    /// open asks again when it changed.
    pub(crate) fn left(&self) -> u64 {
        self.left
    }

    /// The subtrees the transaction has an entry under, in any column,
    /// in the order of their prefixes. The set keeps them in that order:
    /// nothing is sorted for the call, which costs what it yields. A
    /// build that holds them to the old way (`held`) makes them that way
    /// as well, at each call. Outside tests they are asked for only
    /// where an answer is held to what every written subtree says
    /// (`Frames::below`, `ByKey::cut`).
    #[cfg_attr(
        not(any(test, feature = "held_reads")),
        expect(
            dead_code,
            reason = "what is held to every written subtree asks for them"
        )
    )]
    pub(crate) fn written(&self) -> impl Iterator<Item = Subtree> + '_ {
        #[cfg(any(test, feature = "held_reads"))]
        self.hold_written();
        self.written.keys().copied()
    }

    /// Whether the transaction has an entry under `subtree`, in any
    /// column: one lookup.
    pub(crate) fn writes_under(&self, subtree: &Subtree) -> bool {
        self.written.contains_key(subtree)
    }

    /// How many times a subtree came to be one the set has an entry
    /// under, since the set was made. It never goes back: not where
    /// writes are taken out, and not where the transaction ends its
    /// writes. Whoever keeps what it made of the subtrees of
    /// [`Self::written`] keeps this number beside it, and asks for those
    /// that came since ([`Self::written_after`]).
    pub(crate) fn first_writes(&self) -> u64 {
        self.first_writes
    }

    /// Those of [`Self::written`] that came to be subtrees the set has an
    /// entry under after [`Self::first_writes`] was `count`, in the order
    /// they came. A subtree that the set stopped having an entry under
    /// since, and has one under again, is among them. The call costs a
    /// lookup and what it yields.
    pub(crate) fn written_after(&self, count: u64) -> impl Iterator<Item = Subtree> + '_ {
        let after = (Bound::Excluded(count), Bound::Unbounded);
        self.came.range(after).map(|(_, subtree)| *subtree)
    }

    /// The nearest subtree after `from` in the order of the prefixes, or
    /// with `back` the nearest before it, that the transaction has an
    /// entry under, in any column, or that a move of it names.
    pub(crate) fn written_beside(&self, from: &Subtree, back: bool) -> Option<Subtree> {
        let written = self.units.keys().map(|unit| unit.subtree);
        let written = written.chain(self.moved_names());
        match back {
            false => written.filter(|subtree| subtree > from).min(),
            true => written.filter(|subtree| subtree < from).max(),
        }
    }

    /// The subtree of the element that the last enter of `subtree` among
    /// `entries` states as its host, of either kind: `Some(None)` for an
    /// enter with no host.
    fn last_host(entries: &[SubtreeEntry], subtree: &Subtree) -> Option<Option<Subtree>> {
        entries.iter().rev().find_map(|entry| match entry {
            SubtreeEntry::Enter {
                subtree: entered,
                host,
                ..
            } if entered == subtree => Some(host.map(|(host, ..)| host)),
            _ => None,
        })
    }

    /// The subtree of the element that hosts `subtree`, as the last enter
    /// of the subtree states it, of either kind, if the transaction has
    /// one: `Some(None)` for an enter with no host. Where the subtree
    /// stands, and so which enter made it, is not said here
    /// (`ByKey::standing`).
    pub(crate) fn host_of(&self, subtree: &Subtree) -> Option<Option<Subtree>> {
        let enters = self.enters.get(subtree);
        let host = enters
            .and_then(|enters| enters.last())
            .map(|(_, host)| *host);
        #[cfg(any(test, feature = "held_reads"))]
        assert_eq!(
            host,
            Self::last_host(&self.subtrees, subtree),
            "{subtree:?}"
        );
        host
    }

    /// The place of the first operation of a scope that is open, or the
    /// number of operations where there is none: the operations before it
    /// are the transaction's.
    pub(crate) fn first_open(&self) -> usize {
        #[cfg(any(test, feature = "held_reads"))]
        self.hold_scoped();
        let open = self.scoped.values().map(|ops| ops[0]).min();
        open.unwrap_or(self.ops.len())
    }

    /// Every subtree that an enter of `subtree` states the host to be in,
    /// of either kind of enter, in the operations from `since` on.
    pub(crate) fn hosts_stated_from<'a>(
        &'a self,
        subtree: &'a Subtree,
        since: usize,
    ) -> impl Iterator<Item = Subtree> + 'a {
        let from = self
            .ops
            .get(since)
            .map_or(self.subtrees.len(), |op| op.subtrees as usize);
        self.subtrees[from..]
            .iter()
            .filter_map(move |entry| match entry {
                SubtreeEntry::Enter {
                    subtree: entered,
                    host,
                    ..
                } if entered == subtree => host.map(|(host, ..)| host),
                _ => None,
            })
    }

    /// The serial of the operation that was added last, or zero.
    pub(crate) fn last_serial(&self) -> u64 {
        self.ops.last().map_or(0, |op| op.serial)
    }

    /// The place of the operation with `serial`, if the set has it still.
    fn op_with(&self, serial: u64) -> Option<usize> {
        (self.ops.binary_search_by_key(&serial, |op| op.serial)).ok()
    }

    /// Whether the set has the operation with `serial` still.
    pub(crate) fn has_op(&self, serial: u64) -> bool {
        self.op_with(serial).is_some()
    }

    /// The subtree of the element that the enter of `subtree` in the
    /// operation `op` states as its host, of either kind: `None` for an
    /// enter with no host, and for an operation that does not enter it.
    pub(crate) fn host_in(&self, subtree: &Subtree, op: usize) -> Option<Subtree> {
        Self::last_host(self.op_subtrees(op), subtree).flatten()
    }

    /// Whether one of the operations `ops` entered `subtree` as one that
    /// is new ([`SubtreeWrite::Enter`]): the subtree was not entered
    /// before that operation. An enter of a subtree that may be entered
    /// already says nothing of that.
    pub(crate) fn enters_anew(&self, subtree: &Subtree, ops: std::ops::Range<usize>) -> bool {
        let enters = |entry: &SubtreeEntry| matches!(entry, SubtreeEntry::Enter { subtree: entered, if_absent: false, .. } if entered == subtree);
        ops.flat_map(|op| self.op_subtrees(op)).any(enters)
    }

    /// Checks `batch` against what the transaction has written, as a
    /// batch that is given no tree types is held to it: no change set of
    /// a subtree in whose data column the transaction wrote a flat
    /// record, and no flat record in the data column of one it wrote a
    /// node of (ST1).
    ///
    /// A subtree that the batch drops or clears holds nothing when its
    /// change sets and records are applied.
    pub(crate) fn admits(&self, batch: &Batch<'_>) -> Result<(), BatchError> {
        // Once the pages took an operation by the tree type of a subtree,
        // a data column can hold both kinds: each is looked for among all
        // that the transaction wrote there.
        let all = self.ops.iter().any(|op| op.both);
        for merk in batch.merks {
            let subtree = merk.subtree;
            let (_, flat) = self.held(Unit::data(subtree), all);
            if flat && !batch.leaves(subtree) {
                return Err(BatchError::NotMerk { subtree });
            }
        }
        for record in batch.records {
            let unit = record.unit();
            let nodes = unit.column == Column::Data && self.held(unit, all).0;
            if nodes && !batch.leaves(unit.subtree) {
                return Err(BatchError::NotFlat { unit });
            }
        }
        Ok(())
    }

    /// Says that the operation that was added last was given `kinds`.
    pub(crate) fn tell_kinds(&mut self, kinds: Option<FlatKinds>) {
        if let Some(op) = self.ops.last_mut() {
            op.kinds = kinds;
        }
    }

    /// Says whether the pages took the operation that was added last by
    /// the tree type of a subtree ([`Op::both`]).
    pub(crate) fn tell_both(&mut self, both: bool) {
        if let Some(op) = self.ops.last_mut() {
            op.both = both;
        }
    }

    /// The tree types that keep flat records, as the batch of the
    /// operation `op` was given them.
    pub(crate) fn kinds(&self, op: usize) -> Option<FlatKinds> {
        self.ops[op].kinds
    }

    /// Whether an operation of the transaction entered `subtree` if it
    /// was absent ([`SubtreeWrite::EnterIfAbsent`]).
    pub(crate) fn enters_if_absent(&self, subtree: &Subtree) -> bool {
        let if_absent = |(at, _): &(u32, Option<Subtree>)| {
            matches!(
                self.subtrees[*at as usize],
                SubtreeEntry::Enter {
                    if_absent: true,
                    ..
                }
            )
        };
        let enters = (self.enters.get(subtree)).is_some_and(|enters| enters.iter().any(if_absent));
        #[cfg(any(test, feature = "held_reads"))]
        assert_eq!(
            enters,
            self.enters_if_absent_by_every_entry(subtree),
            "an enter if absent of {subtree}"
        );
        enters
    }

    /// Whether the Merk `subtree` has a root after the transaction's
    /// writes, if one of them set its root, or entered, dropped or cleared
    /// the subtree, which leaves it with none. It is asked of a subtree
    /// that no write entered if it was absent
    /// ([`Self::enters_if_absent`]): whether such a write left the subtree
    /// with the root it had is in [`Self::subtree`].
    pub(crate) fn rooted(&self, subtree: &Subtree) -> Option<bool> {
        self.rooted_before(subtree, self.ops.len(), false)
    }

    /// What the transaction's writes say of the root of the Merk
    /// `subtree`, an enter of the subtree if it was absent aside, which
    /// says nothing of it: [`Self::rooted`] otherwise, and `None` where no
    /// write set the root, entered the subtree as a new one, dropped or
    /// cleared it, or took it with a subtree above it.
    pub(crate) fn states_root(&self, subtree: &Subtree) -> Option<bool> {
        self.rooted_before(subtree, self.ops.len(), true)
    }

    /// [`Self::rooted`], after the operations before `end` only. With
    /// `found`, an enter of the subtree if it was absent says nothing of
    /// the root, whether it found the subtree or entered it: what is
    /// older says, and where nothing does, `None`. The version that is
    /// read is then asked, which may have no such subtree.
    fn rooted_before(&self, subtree: &Subtree, end: usize, found: bool) -> Option<bool> {
        let rooted = self.rooted_in(subtree, self.said_before(subtree, end), found);
        #[cfg(any(test, feature = "held_reads"))]
        assert_eq!(
            rooted,
            self.rooted_in(subtree, (0..end).rev(), found),
            "the root of {subtree} before the operation {end}: by the operations that say something of \
             it, and by every operation"
        );
        rooted
    }

    /// [`Self::rooted_before`] from the operations `ops`, newest first:
    /// those that say something of the subtree ([`Self::said_before`]),
    /// or every operation before one.
    fn rooted_in(
        &self,
        subtree: &Subtree,
        ops: impl Iterator<Item = usize>,
        found: bool,
    ) -> Option<bool> {
        for op in ops {
            // The change sets of an operation come after what it does to
            // subtrees as a whole.
            let (entries_from, entries_to, merks_from, merks_to) = self.op_bounds(op);
            let set = |merk: &MerkEntry| (merk.subtree == *subtree).then_some(merk.root).flatten();
            if let Some(root) = self.merks[merks_from..merks_to].iter().rev().find_map(set) {
                return Some(root.is_some());
            }
            // What a move says of a subtree is not asked here: a
            // transaction with no frames reads none that a move names.
            let named = |entry: &SubtreeEntry| match entry {
                SubtreeEntry::Enter {
                    subtree: named,
                    if_absent,
                    ..
                } => named == subtree && !(found && *if_absent),
                SubtreeEntry::Convert { .. }
                | SubtreeEntry::Move { .. }
                | SubtreeEntry::RemoveIfEntered(_)
                | SubtreeEntry::NotUnder { .. } => false,
                left => left.left() == Some(*subtree),
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

    /// What the operations of the transaction before `end` say of
    /// `subtree` as a whole: whether they entered it or took it out, and
    /// the root they left its Merk with. It is what a transaction with no
    /// frames answers from, as [`Self::rooted`] is.
    pub(crate) fn subtree(&self, subtree: &Subtree, end: usize) -> Said<'_> {
        let end = end.min(self.ops.len());
        let said = self.subtree_in(subtree, self.said_before(subtree, end));
        #[cfg(any(test, feature = "held_reads"))]
        assert_eq!(
            said,
            self.subtree_in(subtree, (0..end).rev()),
            "{subtree} before the operation {end}: by the operations that say something of it, and \
             by every operation"
        );
        said
    }

    /// [`Self::subtree`] from the operations `ops`, newest first: those
    /// that say something of the subtree ([`Self::said_before`]), or
    /// every operation before one.
    fn subtree_in(&self, subtree: &Subtree, ops: impl Iterator<Item = usize>) -> Said<'_> {
        let mut root: Option<Option<&[u8]>> = None;
        // The operations that entered the subtree if it was absent, newest
        // first, after the write that says of the subtree what does not
        // depend on that.
        let mut if_absent: Vec<(SubtreeView<'_>, usize)> = Vec::new();
        let mut stated = Restated::default();
        for op in ops {
            // In one operation the change sets come last, the enters
            // before them, and the drops and the clears first.
            let (entries_from, entries_to, merks_from, merks_to) = self.op_bounds(op);
            if root.is_none() {
                let set =
                    |merk: &MerkEntry| (merk.subtree == *subtree).then_some(merk.root).flatten();
                let stated = self.merks[merks_from..merks_to].iter().rev().find_map(set);
                root = stated.map(|root| root.map(|key| self.arena.get(key)));
            }
            let named = self.op_subtrees(op);
            // A subtree is stated anew by an operation after the one that
            // entered it: the newest statement stands.
            for entry in named {
                if let SubtreeEntry::Convert {
                    subtree: named,
                    tree_type,
                    position,
                } = entry
                    && named == subtree
                {
                    stated.tree_type = stated.tree_type.or(Some(self.arena.get(*tree_type)));
                    stated.position = stated.position.or(*position);
                }
            }
            let entered = named.iter().rev().find_map(|entry| match entry {
                SubtreeEntry::Enter {
                    subtree: entered,
                    tree_type,
                    host,
                    if_absent,
                } if entered == subtree => Some((*tree_type, *host, *if_absent)),
                _ => None,
            });
            if let Some((tree_type, host, absent)) = entered {
                let view = SubtreeView {
                    root_key: root.flatten(),
                    tree_type: self.arena.get(tree_type),
                    host: host.map(|(subtree, key, position)| HostPut {
                        subtree,
                        key: self.arena.get(key),
                        position,
                    }),
                };
                let view = stated.of(view);
                if !absent {
                    let base = OwnSubtree::Entered { view, op };
                    return Said { base, if_absent };
                }
                if_absent.push((view, op));
            }
            // What the operation wrote of a subtree below one that it
            // dropped or cleared went with it: the entry of the drop or the
            // clear names it.
            let before = |bound: usize| self.leaving.partition_point(|id| (*id as usize) < bound);
            let mut cut = (self.leaving[before(entries_from)..before(entries_to)].iter())
                .flat_map(|id| Self::cut(&self.arena, &self.entries[*id as usize]));
            let dropped = |entry: &SubtreeEntry| entry.dropped() == Some(*subtree);
            if named.iter().any(dropped) || cut.any(|cut| cut == *subtree) {
                // A drop comes before the enters of its operation.
                let base = OwnSubtree::Gone;
                return Said { base, if_absent };
            }
            let cleared = |entry: &SubtreeEntry| matches!(entry, SubtreeEntry::Clear(named) if named == subtree);
            if named.iter().any(cleared) {
                // A cleared Merk has no node until a later write gives it
                // one.
                root = root.or(Some(None));
            }
        }
        let base = OwnSubtree::Unwritten { root, stated };
        Said { base, if_absent }
    }

    /// The subtrees that the operations after `op` dropped or cleared,
    /// oldest first.
    ///
    /// A subtree that the transaction entered and has not written is in
    /// the entry of no drop: whether it went with a subtree above it is
    /// asked of these.
    pub(crate) fn left_after(&self, op: usize) -> Vec<Subtree> {
        self.left_in(op + 1..self.ops.len())
    }

    /// The subtrees that the operations `ops` dropped or cleared, oldest
    /// first.
    pub(crate) fn left_in(&self, ops: std::ops::Range<usize>) -> Vec<Subtree> {
        let named = ops.flat_map(|op| self.op_subtrees(op));
        named.filter_map(SubtreeEntry::left).collect()
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

    /// Whether the range delete `id` contains `key`. A move deletes
    /// nothing.
    fn range_holds(&self, id: EntryId, key: &[u8]) -> bool {
        let (start, end) = self.range_bounds(id);
        !matches!(self.entry(id).kind, Kind::Moved) && in_range(key, start, end)
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
    ///
    /// Where the transaction moved the unit's subtree, the writes it made
    /// of the record under the prefix the subtree had before are its
    /// writes of it ([`Self::stretches`]). [`Own::Unwritten`] then says
    /// that the record is the committed one, which the committed state
    /// has under the prefix of [`Self::committed_under`].
    pub(crate) fn own(&self, unit: Unit, key: &[u8]) -> Own<'_> {
        if self.moved.is_empty() {
            let whole = Stretch {
                subtree: unit.subtree,
                after: None,
                before: None,
            };
            return self.own_in(unit, key, &whole).unwrap_or(Own::Unwritten);
        }
        let (stretches, _) = self.stretches(unit.subtree, None);
        let own = stretches.iter().find_map(|stretch| {
            let unit = Unit::new(unit.column, stretch.subtree);
            self.own_in(unit, key, stretch)
        });
        own.unwrap_or(Own::Unwritten)
    }

    /// The newest write that covers the record key `key` among those of
    /// `unit` that `stretch` holds, or `None` if none of them does.
    fn own_in(&self, unit: Unit, key: &[u8], stretch: &Stretch) -> Option<Own<'_>> {
        let index = self.units.get(&unit)?;
        let point = self.point_in(index, key, stretch);
        let range = self.range_in(index, key, stretch);
        Some(match (point, range) {
            (None, None) => return None,
            (None, Some(_)) => Own::Deleted,
            (Some(point), Some(range)) if range > point => Own::Deleted,
            (Some(point), _) => match self.entry(point).kind {
                Kind::Put(value) => Own::Put(self.arena.get(value)),
                Kind::Node(_) => Own::Node(point),
                Kind::Delete | Kind::Remove => Own::Deleted,
                Kind::Range { .. } | Kind::Dropped | Kind::Cleared | Kind::Moved => {
                    unreachable!("a range is not indexed by its key")
                }
            },
        })
    }

    /// Whether a range delete of the transaction contains the record key
    /// `key` of `unit`: a committed record under it is hidden, whatever
    /// else the transaction wrote there (RD3).
    pub(crate) fn hides(&self, unit: Unit, key: &[u8]) -> bool {
        if self.moved.is_empty() {
            let index = self.units.get(&unit);
            return index.is_some_and(|index| {
                (index.ranges.iter()).any(|range| self.range_holds(*range, key))
            });
        }
        let (stretches, _) = self.stretches(unit.subtree, None);
        stretches.iter().any(|stretch| {
            let index = self.units.get(&Unit::new(unit.column, stretch.subtree));
            index.is_some_and(|index| self.range_in(index, key, stretch).is_some())
        })
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
        if self.moved.is_empty() {
            let keys = &self.units.get(&unit)?.keys;
            let mut range = keys.range::<[u8], _>(bounds);
            let found = if back {
                range.next_back()
            } else {
                range.next()
            };
            return found.map(|(key, _)| &**key);
        }
        // The first key of each stretch that an entry of the stretch
        // wrote, and of those the one that comes first.
        let (stretches, _) = self.stretches(unit.subtree, None);
        let firsts = stretches.iter().filter_map(|stretch| {
            let index = self.units.get(&Unit::new(unit.column, stretch.subtree))?;
            let written = |key: &[u8]| self.point_in(index, key, stretch).is_some();
            let mut range = index.keys.range::<[u8], _>(bounds);
            let found = match back {
                true => range.rfind(|(key, _)| written(key)),
                false => range.find(|(key, _)| written(key)),
            };
            found.map(|(key, _)| &**key)
        });
        match back {
            true => firsts.max(),
            false => firsts.min(),
        }
    }

    /// The value of the written node `id`: its own, that of an earlier
    /// write of the same node, or the transaction's copy of the committed
    /// one. `None` says that it is the committed value and that no copy was
    /// taken, which is the case only while the transaction's frames hold
    /// it.
    ///
    /// A value that a load put into the value log is told by where it is:
    /// the write set does not hold its bytes.
    pub(crate) fn node_value(&self, id: EntryId) -> Option<NodeValue<'_>> {
        let mut id = id;
        loop {
            let entry = self.entry(id);
            let Kind::Node(node) = entry.kind else {
                return None;
            };
            match node.value {
                ValueEntry::Value(span) => return Some(NodeValue::Bytes(self.arena.get(span))),
                ValueEntry::Kept(span) => return Some(NodeValue::Bytes(self.kept.get(span))),
                ValueEntry::Stored(span) => {
                    let stored = StoredValue::decode(self.arena.get(span));
                    return Some(NodeValue::Stored(stored));
                }
                ValueEntry::Keep => id = self.prior(id)?,
            }
        }
    }

    /// Whether the value of the written node `id` is one the transaction
    /// stated: in that write, or in an earlier write of the same node whose
    /// value the later ones kept. Where no write of the node stated a
    /// value, every one of them kept the committed value, and that is what
    /// the node has, whether or not the transaction took a copy of it.
    pub(crate) fn states_value(&self, id: EntryId) -> bool {
        let mut id = id;
        loop {
            let entry = self.entry(id);
            let Kind::Node(node) = entry.kind else {
                return false;
            };
            match (node.value, self.prior(id)) {
                (ValueEntry::Value(_) | ValueEntry::Stored(_), _) => return true,
                (_, Some(prev)) => id = prev,
                (_, None) => return false,
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
                let before = || self.prior(*id).map(|prev| self.entry(prev).kind);
                keeps && !matches!(before(), Some(Kind::Node(_)))
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

    /// Puts the written node `id`, the node under `key` of `unit`, into the
    /// overlap set: it keeps a value of the committed state, and a commit outside
    /// the transaction wrote its key after the writer read the value and
    /// before this write, or may have (CF2a). The commit is not after the
    /// entry's first-write version, so [`Self::meet`] does not see it.
    ///
    /// The entry is met like any other from here on: a rollback to a place
    /// before it takes it out of the overlap set with the entry (CF4), and
    /// a scope that is taken out from between leaves it met.
    pub(crate) fn meet_kept(&mut self, id: EntryId, unit: Unit, key: &[u8]) {
        debug_assert_eq!(
            (self.entry(id).unit, self.key(id)),
            (unit, key),
            "the entry of the node that is met"
        );
        let entry = &mut self.entries[id as usize];
        debug_assert!(matches!(entry.kind, Kind::Node(_)), "a node keeps a value");
        if !entry.met {
            entry.met = true;
            self.met += 1;
        }
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
    /// `version` (CF2, CF3). A load is met where the commit holds any key
    /// or range of the nodes of its subtree: the entry of its first node
    /// goes into the overlap set.
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
        // A load is the whole of a Merk that had no root, so it is a range
        // over the nodes of its subtree (`transactions.md` LD6): a commit
        // that wrote or deleted any of them meets it.
        for (subtree, first) in self.loads() {
            // A range that ends where it starts holds no key.
            let holds_keys = |theirs: &UnitKeys| {
                let holds = |(from, to): &KeyRange| ranges_meet((from, to.as_deref()), (&[], None));
                !theirs.keys.is_empty() || theirs.ranges.iter().any(holds)
            };
            if keys.units.get(&Unit::data(subtree)).is_some_and(holds_keys) {
                met.push(first);
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
        let met: Vec<EntryId> = (0..self.entries.len() as EntryId)
            .filter(|id| self.covers_subtree(*id, subtree))
            .collect();
        for id in met {
            self.mark_met(id, version);
        }
    }

    /// Whether the entry `id` covers a key of `subtree`, in any column.
    fn covers_subtree(&self, id: EntryId, subtree: Subtree) -> bool {
        let units = Self::covered(&self.arena, self.entry(id));
        units.iter().any(|unit| unit.subtree == subtree)
    }

    /// As [`Self::meet_subtree`], for the writes under `subtree` as the
    /// operation with `serial` entered it, or found it: those of that
    /// operation and of the ones after it, up to the next operation that
    /// enters the subtree. A drop and a clear of that operation itself
    /// come before its enter, and are none of them. It does nothing where
    /// the set has the operation no more.
    pub(crate) fn meet_entered(&mut self, serial: u64, subtree: Subtree, version: CommitVersion) {
        let Some(first) = self.op_with(serial) else {
            return;
        };
        let enters = |op: &usize| Self::last_host(self.op_subtrees(*op), &subtree).is_some();
        let later = (first + 1..self.ops.len()).find(enters);
        let end = later.unwrap_or(self.ops.len());
        // Where the entries of an operation begin, or all of them end.
        let begins = |op: usize| match self.ops.get(op) {
            Some(op) => op.entries,
            None => self.entries.len() as EntryId,
        };
        let own = begins(first + 1);
        let met: Vec<EntryId> = (begins(first)..own)
            .filter(|id| self.entry(*id).kind.leaves().is_empty())
            .chain(own..begins(end))
            .filter(|id| self.covers_subtree(*id, subtree))
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
    /// deleted, every range it deleted, and the range of the nodes of
    /// each Merk it loaded, each once.
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
                    Kind::Cleared => {
                        set.cleared(unit.subtree);
                    }
                    // Every column under every prefix the move names:
                    // what it took from one side and wrote on the other.
                    Kind::Moved => {
                        set.subtree(unit.subtree);
                    }
                    _ => {
                        let (start, end) = self.range_bounds(*range);
                        set.range(*unit, start, end);
                    }
                }
            }
        }
        // Every node of the subtree of a load (LD6).
        for (subtree, _) in self.loads() {
            set.range(Unit::data(subtree), &[], None);
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

/// What the write set did on this thread for the list of the puts that
/// left a hash stale, counted for the tests: how often the prefixes of a
/// subtree were asked for through its moves, how many range deletes a
/// put was compared with, and how many moves an entry was compared with.
#[cfg(test)]
mod counted {
    use std::cell::Cell;

    thread_local! {
        static STRETCHES: Cell<u64> = const { Cell::new(0) };
        static RANGES: Cell<u64> = const { Cell::new(0) };
        static MOVES: Cell<u64> = const { Cell::new(0) };
        static CUTS: Cell<u64> = const { Cell::new(0) };
    }

    /// An entry was asked whether it is a drop or a clear, to find the
    /// newest that took the puts before it.
    pub(super) fn cut() {
        CUTS.with(|count| count.set(count.get() + 1));
    }

    /// How many entries were asked that since the count was last taken.
    pub(super) fn take_cut() -> u64 {
        CUTS.with(|count| count.replace(0))
    }

    /// A move was compared with an entry, to find the newest before it.
    pub(super) fn moved() {
        MOVES.with(|count| count.set(count.get() + 1));
    }

    /// How many moves were compared since the count was last taken.
    pub(super) fn take_moved() -> u64 {
        MOVES.with(|count| count.replace(0))
    }

    pub(super) fn stretches() {
        STRETCHES.with(|count| count.set(count.get() + 1));
    }

    pub(super) fn range() {
        RANGES.with(|count| count.set(count.get() + 1));
    }

    /// Both counts since they were last taken.
    pub(super) fn take() -> (u64, u64) {
        (
            STRETCHES.with(|count| count.replace(0)),
            RANGES.with(|count| count.replace(0)),
        )
    }
}

#[cfg(any(test, feature = "held_reads"))]
mod held {
    //! What the index of the prefixes that moves cover answers is held,
    //! at every call, to the answers that were made before it: the
    //! names of every move of the set are looked through. So is the
    //! host that the index of the enters gives for a subtree, to the
    //! last enter of it among all that the set holds
    //! ([`WriteSet::host_of`]). The counts of what the drops and the
    //! clears take, and of the names of the moves, are held to the
    //! lists they are counted from wherever an answer of the directory
    //! is held that is made from them (`opened::held`). And the
    //! subtrees that the set keeps in order are held, at every call of
    //! [`WriteSet::written`], to those of its units, sorted, as that
    //! call made them before, and the numbers they were given when they
    //! came to each other. That is so in a test build of this crate,
    //! and in a build with the feature `held_reads`.

    use super::*;

    impl WriteSet {
        /// The subtrees the set has an entry under, as
        /// [`WriteSet::written`] made them before the set kept them in
        /// order: those of every unit, sorted, each once.
        fn written_by_sort(&self) -> Vec<Subtree> {
            let mut subtrees: Vec<Subtree> = self.units.keys().map(|unit| unit.subtree).collect();
            subtrees.sort_unstable();
            subtrees.dedup();
            subtrees
        }

        /// Holds the subtrees that [`WriteSet::written`] gives to
        /// those it gave before ([`Self::written_by_sort`]), in their
        /// order, the count of each to its units, and the number each
        /// was given when it came to the subtrees by their numbers
        /// ([`WriteSet::written_after`]): each subtree has one number,
        /// which no other has and which is none after the last that
        /// was given.
        pub(super) fn hold_written(&self) {
            let kept: Vec<Subtree> = self.written.keys().copied().collect();
            assert_eq!(
                kept,
                self.written_by_sort(),
                "the subtrees the set has an entry under"
            );
            let mut counted: BTreeMap<Subtree, u32> = BTreeMap::new();
            for unit in self.units.keys() {
                *counted.entry(unit.subtree).or_default() += 1;
            }
            let units: BTreeMap<Subtree, u32> = (self.written.iter())
                .map(|(subtree, written)| (*subtree, written.units))
                .collect();
            assert_eq!(units, counted, "the units of each subtree");
            let came: BTreeMap<u64, Subtree> = (self.written.iter())
                .map(|(subtree, written)| (written.came, *subtree))
                .collect();
            assert_eq!(self.came, came, "the subtrees by when they came");
            assert_eq!(came.len(), self.written.len(), "one number each");
            let last = came.keys().next_back().copied().unwrap_or(0);
            assert!(last <= self.first_writes, "no number after the last");
        }

        /// The operations by subtree, made anew from every operation
        /// of the set as [`WriteSet::add`] notes them.
        fn said_by_every_op(&self) -> HashMap<Subtree, Says> {
            let mut said: HashMap<Subtree, Says> = HashMap::new();
            let note = |ops: &mut Vec<usize>, op: usize| {
                if ops.last() != Some(&op) {
                    ops.push(op);
                }
            };
            for op in 0..self.ops.len() {
                let (entries_from, entries_to, merks_from, merks_to) = self.op_bounds(op);
                let named = self
                    .op_subtrees(op)
                    .iter()
                    .filter_map(SubtreeEntry::says_of);
                let cut = (self.entries[entries_from..entries_to].iter())
                    .flat_map(|entry| Self::cut(&self.arena, entry));
                let rooted = self.merks[merks_from..merks_to].iter();
                let rooted = rooted.filter(|merk| merk.root.is_some());
                for subtree in named.chain(cut) {
                    note(&mut said.entry(subtree).or_default().whole, op);
                }
                for merk in rooted {
                    note(&mut said.entry(merk.subtree).or_default().rooted, op);
                }
            }
            said
        }

        /// The operations of each open scope, from every operation.
        fn scoped_by_every_op(&self) -> HashMap<ScopeId, Vec<usize>> {
            let mut scoped: HashMap<ScopeId, Vec<usize>> = HashMap::new();
            for (at, op) in self.ops.iter().enumerate().filter(|(_, op)| op.scope != 0) {
                scoped.entry(op.scope).or_default().push(at);
            }
            scoped
        }

        /// The puts that [`WriteSet::unmade`] returns for `subtree`,
        /// from every key the set has under each prefix the Merk was read
        /// under: those that are the newest write of their record and
        /// left a hash stale.
        pub(super) fn unmade_by_every_key(&self, subtree: Subtree) -> BTreeSet<EntryId> {
            let unit = Unit::data(subtree);
            let (stretches, _) = self.stretches(subtree, None);
            let mut puts = BTreeSet::new();
            for stretch in &stretches {
                let at = Unit::data(stretch.subtree);
                let Some(index) = self.units.get(&at) else {
                    continue;
                };
                for key in index.keys.keys() {
                    let Own::Node(id) = self.own(unit, key) else {
                        continue;
                    };
                    let Kind::Node(node) = self.entry(id).kind else {
                        unreachable!("a node is a put of one");
                    };
                    if node.leaves_stale() {
                        puts.insert(id);
                    }
                }
            }
            puts
        }

        /// The keys of the nodes of the Merk `subtree` that the
        /// transaction put and that are its newest write of their
        /// record, from every key the set has under each prefix the
        /// Merk was read under.
        pub(crate) fn nodes_by_every_key(&self, subtree: Subtree) -> Vec<&[u8]> {
            let unit = Unit::data(subtree);
            let (stretches, _) = self.stretches(subtree, None);
            let indexes = stretches
                .iter()
                .filter_map(|stretch| self.units.get(&Unit::data(stretch.subtree)));
            let keys = indexes.flat_map(|index| index.keys.keys());
            let nodes = keys.filter(|key| matches!(self.own(unit, key), Own::Node(_)));
            nodes.map(|key| &**key).collect()
        }

        /// Holds the puts that the units list as having left a hash
        /// stale to every entry of the set, oldest first: each lists
        /// itself if it is such a put, and takes out the write of its
        /// record before it.
        ///
        /// A set in which no put left a hash stale lists nothing, which
        /// is seen by going through its entries and its units once: the
        /// write before a write is not looked for, which costs a search
        /// for each where the set holds a move.
        pub(super) fn hold_unmade(&self) {
            let leaves =
                |entry: &Entry| matches!(entry.kind, Kind::Node(node) if node.leaves_stale());
            if !self.entries.iter().any(leaves) {
                let listed = self.units.values().any(|index| !index.stale.is_empty());
                assert!(!listed, "no put left a hash stale, and one is listed");
                assert_eq!(self.unmade, 0, "and none is counted");
                return;
            }
            let mut every: BTreeSet<(Unit, EntryId)> = BTreeSet::new();
            for (id, entry) in (0..).zip(&self.entries) {
                if !Self::passes(entry.unit, &entry.kind) {
                    continue;
                }
                if let Some(prior) = self.prior(id) {
                    every.remove(&(self.entry(prior).unit, prior));
                }
                if matches!(entry.kind, Kind::Node(node) if node.leaves_stale()) {
                    every.insert((entry.unit, id));
                }
            }
            let listed = self.units.iter();
            let listed: BTreeSet<(Unit, EntryId)> = listed
                .flat_map(|(unit, index)| index.stale.iter().map(|id| (*unit, *id)))
                .collect();
            assert_eq!(listed, every, "the puts that left a hash stale");
            assert_eq!(listed.len(), self.unmade, "and how many they are");
            assert!(self.listed || listed.is_empty(), "one was listed");
        }

        /// Holds the operations that the set keeps by subtree and by
        /// open scope to those of every operation
        /// ([`Self::said_by_every_op`], [`Self::scoped_by_every_op`]):
        /// after a rollback to a mark and after a scope was taken out,
        /// the lists are what the operations that stay make them.
        pub(super) fn hold_said(&self) {
            assert_eq!(
                self.said,
                self.said_by_every_op(),
                "the operations by subtree"
            );
            self.hold_scoped();
        }

        /// Holds the operations that the set keeps by open scope to
        /// those of every operation.
        pub(super) fn hold_scoped(&self) {
            assert_eq!(
                self.scoped,
                self.scoped_by_every_op(),
                "the operations by scope"
            );
        }

        /// [`WriteSet::enters_if_absent`] as it was made before: the
        /// entries of subtrees as a whole are asked, oldest first, until
        /// one is such an enter of the subtree.
        pub(super) fn enters_if_absent_by_every_entry(&self, subtree: &Subtree) -> bool {
            let enters = |entry: &SubtreeEntry| matches!(entry, SubtreeEntry::Enter { subtree: entered, if_absent: true, .. } if entered == subtree);
            self.subtrees.iter().any(enters)
        }

        pub(crate) fn moves_by_every_name(&self, subtree: &Subtree) -> bool {
            (self.moved.iter()).any(|id| self.names(*id).place(subtree).is_some())
        }

        pub(super) fn moved_under_by_every_name(
            &self,
            at: &Subtree,
            before: Option<EntryId>,
        ) -> Option<(EntryId, Option<Subtree>)> {
            let older = |id: &EntryId| before.is_none_or(|before| *id < before);
            let mut moves = self.moved.iter().rev().copied().filter(older);
            moves.find_map(|id| Some((id, self.names(id).place(at)?)))
        }

        /// Holds the counts that [`WriteSet::takes_below`] answers
        /// from to the list of the drops and the clears
        /// ([`WriteSet::taking`]), and its answer for each subtree
        /// of the list to what the list says of it: one of its
        /// entries takes what is below, or one leaves that to the
        /// version, which has the Merk with a node or with none.
        pub(crate) fn hold_taken(&self) {
            let mut taken: HashMap<Subtree, Taken> = HashMap::new();
            // For each subtree, whether the list takes what is
            // below it: where the version has its Merk with no
            // node, and where it has it with one.
            let mut listed: HashMap<Subtree, [bool; 2]> = HashMap::new();
            for (subtree, rooted) in self.taking() {
                *taken.entry(subtree).or_default().said(rooted) += 1;
                let takes = listed.entry(subtree).or_default();
                takes[0] |= rooted.unwrap_or(false);
                takes[1] |= rooted.unwrap_or(true);
            }
            assert_eq!(self.taken, taken, "what the drops and the clears take");
            for (subtree, takes) in listed {
                let answered = [false, true].map(|rooted| self.takes_below(&subtree, rooted));
                assert_eq!(answered, takes, "{subtree:?}");
            }
        }

        /// Holds the counts that [`WriteSet::named_by_a_move`] answers
        /// from to the list of the names ([`WriteSet::moved_names`]).
        pub(crate) fn hold_named(&self) {
            let mut named: HashMap<Subtree, u32> = HashMap::new();
            for name in self.moved_names() {
                *named.entry(name).or_default() += 1;
            }
            assert_eq!(self.named, named, "the names of the moves");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use lignin_format::tree_page::{HashField, Side};

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
                        set.add(&batch, version(latest), &[], &[], &[], &[], &[]);
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
                // The subtrees with an entry, in the order of their
                // prefixes: those of the writes that are left.
                let mut written: Vec<Subtree> = (model.iter())
                    .map(|(write, ..)| write.write().unit().subtree)
                    .collect();
                written.sort_unstable();
                written.dedup();
                assert_eq!(set.written().collect::<Vec<_>>(), written, "seed {seed}");
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

    /// The subtrees the set has an entry under are kept in the order of
    /// their prefixes: each comes in with its first unit, whichever write
    /// makes it, and goes with its last, through a drop that covers a
    /// subtree written below it, a move, rollbacks to a mark, and the end
    /// of the writes.
    #[test]
    fn the_written_subtrees_are_kept_in_order() {
        let subtree = |name: u8| Subtree::new([name; 32]);
        let put = |set: &mut WriteSet, name: u8, column: Column| {
            let records = [RecordWrite::Put {
                unit: Unit::new(column, subtree(name)),
                key: b"k",
                value: b"v",
            }];
            let batch = Batch {
                records: &records,
                ..Batch::default()
            };
            set.add(&batch, version(1), &[], &[], &[], &[], &[]);
        };
        let whole = |set: &mut WriteSet, write: SubtreeWrite<'_>, cut: &[u8], left: &[u8]| {
            let (cut, left): (Vec<Subtree>, Vec<Subtree>) = (
                cut.iter().copied().map(subtree).collect(),
                left.iter().copied().map(subtree).collect(),
            );
            let subtrees = [write];
            let batch = Batch {
                subtrees: &subtrees,
                ..Batch::default()
            };
            set.add(&batch, version(1), &[], &cut, &left, &[], &[]);
        };
        let written = |set: &WriteSet| -> Vec<Subtree> { set.written().collect() };
        let names = |names: &[u8]| -> Vec<Subtree> { names.iter().copied().map(subtree).collect() };
        let after = |set: &WriteSet, count: u64| -> Vec<Subtree> {
            // Every one of them is a subtree the set has an entry under.
            let after: Vec<Subtree> = set.written_after(count).collect();
            assert!(after.iter().all(|subtree| set.writes_under(subtree)));
            after
        };

        let mut set = WriteSet::new();
        assert_eq!(written(&set), names(&[]));
        assert_eq!((set.first_writes(), after(&set, 0)), (0, names(&[])));
        // Two columns of one subtree are one subtree, and the order is
        // that of the prefixes, not of the writes.
        put(&mut set, 9, Column::Aux);
        put(&mut set, 3, Column::Aux);
        put(&mut set, 9, Column::Meta);
        assert_eq!(written(&set), names(&[3, 9]));
        // By when they came, the order is that of the writes.
        assert_eq!((set.first_writes(), after(&set, 0)), (2, names(&[9, 3])));
        assert_eq!(after(&set, 1), names(&[3]));
        assert!(set.writes_under(&subtree(3)) && !set.writes_under(&subtree(5)));
        let first = set.mark();
        // A drop is an entry under its own subtree, which nothing was
        // written under, and under the one it cuts.
        let dropped = SubtreeWrite::Drop {
            subtree: subtree(5),
        };
        whole(&mut set, dropped, &[3], &[]);
        assert_eq!(written(&set), names(&[3, 5, 9]));
        // The subtree that the drop cut had an entry before: it did not
        // come again.
        assert_eq!((set.first_writes(), after(&set, 2)), (3, names(&[5])));
        let second = set.mark();
        put(&mut set, 1, Column::Roots);
        put(&mut set, 5, Column::Aux);
        // A clear, of the data column of its subtree alone.
        let cleared = SubtreeWrite::Clear {
            subtree: subtree(7),
        };
        whole(&mut set, cleared, &[], &[]);
        assert_eq!(written(&set), names(&[1, 3, 5, 7, 9]));
        // The writes after the second mark go: the drop keeps the
        // subtree it names, which the put after it was under as well.
        assert_eq!((set.first_writes(), after(&set, 3)), (5, names(&[1, 7])));
        assert!(set.rewind(second));
        assert_eq!(written(&set), names(&[3, 5, 9]));
        // What went is among none of them, and the count stays.
        assert_eq!((set.first_writes(), after(&set, 3)), (5, names(&[])));
        // The moves of an operation are an entry under every prefix they
        // name, on either side, and under each that left with them.
        let moved = SubtreeWrite::Move {
            from: subtree(4),
            to: subtree(8),
            host: HostPut {
                subtree: subtree(9),
                key: b"host",
                position: 0,
            },
            read: None,
            below: &[(subtree(2), subtree(6))],
        };
        whole(&mut set, moved, &[], &[11]);
        assert_eq!(written(&set), names(&[2, 3, 4, 5, 6, 8, 9, 11]));
        // The drop and the move go: the subtree that the drop named has
        // no unit left, and the one it cut keeps its own.
        let moved = set.first_writes();
        assert_eq!(after(&set, 5), names(&[4, 2, 8, 6, 11]));
        assert!(set.rewind(first));
        assert_eq!(written(&set), names(&[3, 9]));
        assert_eq!((set.first_writes(), after(&set, 2)), (moved, names(&[])));
        // A subtree that comes again has another number: whoever took
        // the subtrees up to the count as it was is given it again.
        put(&mut set, 5, Column::Data);
        assert_eq!(written(&set), names(&[3, 5, 9]));
        assert_eq!(
            (set.first_writes(), after(&set, moved)),
            (moved + 1, names(&[5]))
        );
        // The end of the writes takes every subtree out and leaves the
        // count where it is.
        set.clear();
        assert_eq!(written(&set), names(&[]));
        assert_eq!(
            (set.first_writes(), after(&set, 0)),
            (moved + 1, names(&[]))
        );
        put(&mut set, 2, Column::Aux);
        assert_eq!(written(&set), names(&[2]));
        assert_eq!(after(&set, moved + 1), names(&[2]));
    }

    /// The operations that say something of a subtree as a whole are kept
    /// by subtree, and those of an open scope by scope: through writes
    /// that say something and writes that say nothing, a clear that cuts a
    /// subtree, a rollback to a mark, a scope that merges, and a scope
    /// that is taken out from between the operations of others. What the
    /// set says of each subtree before each operation is asked all along,
    /// which holds the lists to the walk over every operation.
    #[test]
    fn the_operations_by_subtree_and_by_scope_are_kept_with_the_set() {
        let subtree = |name: u8| Subtree::new([name; 32]);
        let whole = |set: &mut WriteSet, writes: &[SubtreeWrite<'_>], cut: &[u8]| {
            let cut: Vec<Subtree> = cut.iter().copied().map(subtree).collect();
            let batch = Batch {
                subtrees: writes,
                ..Batch::default()
            };
            set.add(&batch, version(1), &[], &cut, &[], &[], &[]);
        };
        let rooted = |set: &mut WriteSet, name: u8, root: RootPut<'_>| {
            let merks = [MerkWrite {
                subtree: subtree(name),
                nodes: &[],
                removed: &[],
                root,
                anchored: false,
                whole: false,
            }];
            let batch = Batch {
                merks: &merks,
                ..Batch::default()
            };
            set.add(&batch, version(1), &[false], &[], &[], &[], &[]);
        };
        let put = |set: &mut WriteSet, name: u8| {
            let records = [RecordWrite::Put {
                unit: Unit::new(Column::Aux, subtree(name)),
                key: b"k",
                value: b"v",
            }];
            let batch = Batch {
                records: &records,
                ..Batch::default()
            };
            set.add(&batch, version(1), &[], &[], &[], &[], &[]);
        };
        let enter = |name: u8, if_absent: bool| match if_absent {
            false => SubtreeWrite::Enter {
                subtree: subtree(name),
                tree_type: b"t",
                host: None,
            },
            true => SubtreeWrite::EnterIfAbsent {
                subtree: subtree(name),
                tree_type: b"t",
                host: None,
            },
        };
        // The operations of a subtree, of both lists, oldest first.
        let said = |set: &WriteSet, name: u8| -> Vec<usize> {
            let says = set.said.get(&subtree(name)).cloned().unwrap_or_default();
            let mut ops = [says.whole, says.rooted].concat();
            ops.sort_unstable();
            ops.dedup();
            ops
        };
        let walked = |set: &WriteSet, name: u8, end: usize| -> Vec<usize> {
            set.said_before(&subtree(name), end).collect()
        };
        // Every answer that the lists give, before every operation: each
        // is held to the walk over every operation where it is made.
        let asked = |set: &WriteSet| {
            for name in 0..10 {
                for end in 0..=set.ops.len() {
                    let _ = set.subtree(&subtree(name), end);
                    for found in [false, true] {
                        let _ = set.rooted_before(&subtree(name), end, found);
                    }
                }
                let _ = set.enters_if_absent(&subtree(name));
            }
            set.hold_said();
        };

        let mut set = WriteSet::new();
        // A record under a subtree says nothing of the subtree.
        put(&mut set, 1);
        assert_eq!(said(&set, 1), []);
        // An enter does, and so does a change set that states the root.
        // One that keeps the root says nothing.
        whole(&mut set, &[enter(1, false)], &[]);
        rooted(&mut set, 1, RootPut::Set(Some(b"r")));
        rooted(&mut set, 1, RootPut::Keep);
        assert_eq!(said(&set, 1), [1, 2]);
        assert_eq!(set.rooted(&subtree(1)), Some(true));
        // A second statement of the root: the newest before `end` is taken
        // with every operation that changes the subtree. It goes again
        // with a rollback, which the set counts.
        let taken_out = set.taken_out();
        let again = set.mark();
        rooted(&mut set, 1, RootPut::Set(None));
        assert_eq!(set.said[&subtree(1)].rooted, [2, 4]);
        assert_eq!(
            (walked(&set, 1, 5), walked(&set, 1, 4)),
            (vec![4, 1], vec![2, 1])
        );
        assert_eq!((walked(&set, 1, 2), walked(&set, 1, 1)), (vec![1], vec![]));
        assert_eq!(set.rooted(&subtree(1)), Some(false));
        assert!(set.rewind(again));
        assert_eq!(
            (set.taken_out(), walked(&set, 1, 5)),
            (taken_out + 1, vec![2, 1])
        );
        // One operation clears a subtree, which cuts another, and enters
        // a third if it is absent: it is one operation of each, once.
        let cleared = SubtreeWrite::Clear {
            subtree: subtree(2),
        };
        whole(&mut set, &[cleared, enter(3, true)], &[1]);
        assert_eq!(
            (said(&set, 1), said(&set, 2), said(&set, 3)),
            (vec![1, 2, 4], vec![4], vec![4])
        );
        assert_eq!(set.rooted(&subtree(1)), Some(false));
        assert!(set.enters_if_absent(&subtree(3)) && !set.enters_if_absent(&subtree(1)));
        asked(&set);
        let mark = set.mark();
        // A drop, a statement of a subtree anew, and a move, which is not
        // asked of the subtrees it names.
        let dropped = SubtreeWrite::Drop {
            subtree: subtree(3),
        };
        whole(&mut set, &[dropped], &[]);
        let stated = SubtreeWrite::Convert {
            subtree: subtree(1),
            tree_type: b"u",
            position: None,
        };
        whole(&mut set, &[stated], &[]);
        let moved = SubtreeWrite::Move {
            from: subtree(4),
            to: subtree(8),
            host: HostPut {
                subtree: subtree(9),
                key: b"host",
                position: 0,
            },
            read: None,
            below: &[],
        };
        whole(&mut set, &[moved], &[]);
        assert_eq!(
            (said(&set, 1), said(&set, 3), said(&set, 4), said(&set, 8)),
            (vec![1, 2, 4, 6], vec![4, 5], vec![], vec![])
        );
        asked(&set);
        // The rollback takes the three operations: the lists lose the
        // two of them that they had, and had nothing of the move.
        assert!(set.rewind(mark));
        assert_eq!((said(&set, 1), said(&set, 3)), (vec![1, 2, 4], vec![4]));
        asked(&set);

        // Two open scopes, the operations of one around one of the other.
        put(&mut set, 5);
        set.tag_last(7, false);
        whole(&mut set, &[enter(6, false)], &[]);
        set.tag_last(9, false);
        rooted(&mut set, 6, RootPut::Set(Some(b"r")));
        set.tag_last(7, false);
        assert_eq!((set.ops_of(7), set.ops_of(9)), (vec![5, 7], vec![6]));
        assert_eq!((set.ops_of(8), set.first_open()), (vec![], 5));
        assert_eq!(said(&set, 6), [6, 7]);
        // The one merges: its operation is the transaction's, and laid.
        set.lay_scope(9);
        assert!(set.laid(6) && !set.laid(5));
        set.merge_scope(9);
        assert_eq!((set.ops_of(7), set.ops_of(9)), (vec![5, 7], vec![]));
        assert_eq!((set.ops[6].scope, set.first_open()), (0, 5));
        asked(&set);
        // The other is taken out. The operation that stood between its
        // two is added again behind what stays, as operation 5.
        assert!(set.remove_scope(7));
        assert_eq!((set.ops.len(), said(&set, 6)), (6, vec![5]));
        assert_eq!((set.ops_of(7), set.first_open()), (vec![], 6));
        assert!(set.scoped.is_empty() && !set.remove_scope(7));
        asked(&set);
        // A scope whose operations are the last goes as a rollback does.
        whole(&mut set, &[enter(4, true)], &[]);
        set.tag_last(3, true);
        assert_eq!((said(&set, 4), set.first_open()), (vec![6], 6));
        assert!(set.remove_scope(3));
        assert_eq!((said(&set, 4), set.ops.len()), (vec![], 6));
        asked(&set);
        // The end of the writes empties both, and is counted where there
        // was an operation to take.
        let taken_out = set.taken_out();
        set.clear();
        assert!(set.said.is_empty() && set.scoped.is_empty());
        assert_eq!(set.taken_out(), taken_out + 1);
        set.clear();
        assert_eq!(set.taken_out(), taken_out + 1);
        asked(&set);
        // An operation that is tagged a second time is of the scope it
        // was tagged with last, and of no other.
        put(&mut set, 5);
        set.tag_last(7, false);
        set.tag_last(9, true);
        assert_eq!((set.ops_of(7), set.ops_of(9)), (vec![], vec![0]));
        assert_eq!((set.first_open(), set.laid(0)), (0, true));
    }

    /// The list of the puts that left a hash stale ([`WriteSet::unmade`]):
    /// what it has after each kind of write, and after each is taken out
    /// again. Every answer is held to every key of the Merk where it is
    /// made, and the list to every entry of the set.
    #[test]
    fn the_puts_that_left_a_hash_stale_are_listed_with_the_set() {
        let subtree = |name: u8| Subtree::new([name; 32]);
        let hash = [7; 32];
        let node = |set: &mut WriteSet, name: u8, key: &[u8], stale: Stale, linked: bool| {
            let link = LinkPut {
                key: b"below",
                hash: &hash,
                heights: (1, 0),
                aggregate: &[],
            };
            let nodes = [NodePut {
                key,
                feature: b"f",
                value_hash: &hash,
                kv_hash: &hash,
                left: linked.then_some(link),
                right: None,
                hosted: Hosted::Keep,
                value: ValuePut::Value(b"v"),
                stale,
                at: None,
            }];
            let merks = [MerkWrite {
                subtree: subtree(name),
                nodes: &nodes,
                removed: &[],
                root: RootPut::Keep,
                anchored: false,
                whole: false,
            }];
            let batch = Batch {
                merks: &merks,
                ..Batch::default()
            };
            set.add(&batch, version(1), &[false], &[], &[], &[], &[]);
        };
        let remove = |set: &mut WriteSet, name: u8, key: &[u8]| {
            let merks = [MerkWrite {
                subtree: subtree(name),
                nodes: &[],
                removed: &[key],
                root: RootPut::Keep,
                anchored: false,
                whole: false,
            }];
            let batch = Batch {
                merks: &merks,
                ..Batch::default()
            };
            set.add(&batch, version(1), &[false], &[], &[], &[], &[]);
        };
        let whole = |set: &mut WriteSet, write: SubtreeWrite<'_>| {
            let batch = Batch {
                subtrees: &[write],
                ..Batch::default()
            };
            set.add(&batch, version(1), &[], &[], &[], &[], &[]);
        };
        let listed = |set: &WriteSet, name: u8| -> Vec<Vec<u8>> {
            set.hold_unmade();
            let puts = set.unmade(subtree(name));
            let mut keys: Vec<Vec<u8>> = puts.iter().map(|(_, key)| key.to_vec()).collect();
            keys.sort_unstable();
            keys
        };
        let keys =
            |keys: &[&[u8]]| -> Vec<Vec<u8>> { keys.iter().map(|key| key.to_vec()).collect() };
        let value = Stale::of(HashField::Value);
        let child = Stale::of(HashField::Child(Side::Left));

        let mut set = WriteSet::new();
        // A set whose writers make every hash lists nothing.
        node(&mut set, 1, b"x", Stale::NONE, true);
        assert_eq!(
            (listed(&set, 1), set.unmade, set.listed),
            (keys(&[]), 0, false)
        );
        // A put that leaves a hash is listed. The hash of a child that
        // the node does not have is none, and one that it has is one.
        node(&mut set, 1, b"a", value, false);
        node(&mut set, 1, b"b", child, false);
        node(&mut set, 1, b"c", child, true);
        assert_eq!(listed(&set, 1), keys(&[b"a", b"c"]));
        assert_eq!(listed(&set, 2), keys(&[]));
        // An entry has the hashes as its writer stated them, which is
        // what a node that is shown from its entry shows. The list goes
        // by what the pages mark.
        let kept = |set: &WriteSet, key: &[u8]| {
            let id = set.newest(Unit::data(subtree(1)), key).expect("an entry");
            match set.entry(id).kind {
                Kind::Node(node) => node.stale,
                other => panic!("a put of a node, and not {other:?}"),
            }
        };
        assert_eq!(kept(&set, b"a"), value);
        assert_eq!((kept(&set, b"b"), kept(&set, b"c")), (child, child));

        // A later put of the node passes it, as that of a call does, and
        // so does a delete. Each is taken out again, and the put that it
        // passed is listed again.
        let made = set.mark();
        node(&mut set, 1, b"a", Stale::NONE, false);
        assert_eq!(listed(&set, 1), keys(&[b"c"]));
        remove(&mut set, 1, b"c");
        assert_eq!((listed(&set, 1), set.unmade), (keys(&[]), 0));
        node(&mut set, 1, b"c", value, false);
        assert_eq!(listed(&set, 1), keys(&[b"c"]));
        assert!(set.rewind(made));
        assert_eq!((listed(&set, 1), set.unmade), (keys(&[b"a", b"c"]), 2));

        // A range delete that covers the key of a put: the put is the
        // newest write of its record no more, and is not given.
        let ranged = set.mark();
        let range = [RecordWrite::DeleteRange {
            unit: Unit::data(subtree(1)),
            start: b"b",
            end: None,
        }];
        let batch = Batch {
            records: &range,
            ..Batch::default()
        };
        set.add(&batch, version(1), &[], &[], &[], &[], &[]);
        assert_eq!((listed(&set, 1), set.unmade), (keys(&[b"a"]), 2));
        assert!(set.rewind(ranged));
        assert_eq!(listed(&set, 1), keys(&[b"a", b"c"]));

        // A clear covers the puts from before it: a call is given those
        // from after it. They are listed still, and are given again once
        // the clear is taken out.
        let cleared = set.mark();
        whole(
            &mut set,
            SubtreeWrite::Clear {
                subtree: subtree(1),
            },
        );
        assert_eq!((listed(&set, 1), set.unmade), (keys(&[]), 2));
        node(&mut set, 1, b"d", value, false);
        assert_eq!(listed(&set, 1), keys(&[b"d"]));
        assert!(set.rewind(cleared));
        assert_eq!(listed(&set, 1), keys(&[b"a", b"c"]));

        // A move: the Merk is read under its new prefix, with the puts
        // that were made under the old one, and a put under the new
        // prefix passes one of those. A Merk that is entered under the
        // old prefix afterwards is another.
        let moved = set.mark();
        let to = SubtreeWrite::Move {
            from: subtree(1),
            to: subtree(2),
            host: HostPut {
                subtree: subtree(9),
                key: b"host",
                position: 0,
            },
            read: None,
            below: &[],
        };
        whole(&mut set, to);
        assert_eq!(listed(&set, 2), keys(&[b"a", b"c"]));
        assert_eq!(listed(&set, 1), keys(&[]));
        // A range delete under the new prefix covers a put that was made
        // under the old one.
        let passed = set.mark();
        let range = [RecordWrite::DeleteRange {
            unit: Unit::data(subtree(2)),
            start: b"b",
            end: None,
        }];
        let batch = Batch {
            records: &range,
            ..Batch::default()
        };
        set.add(&batch, version(1), &[], &[], &[], &[], &[]);
        assert_eq!(listed(&set, 2), keys(&[b"a"]));
        assert!(set.rewind(passed));
        assert_eq!(listed(&set, 2), keys(&[b"a", b"c"]));
        let passed = set.mark();
        node(&mut set, 2, b"a", Stale::NONE, false);
        assert_eq!((listed(&set, 2), set.unmade), (keys(&[b"c"]), 1));
        node(&mut set, 2, b"e", value, false);
        let entered = SubtreeWrite::Enter {
            subtree: subtree(1),
            tree_type: b"t",
            host: None,
        };
        whole(&mut set, entered);
        node(&mut set, 1, b"c", Stale::NONE, false);
        node(&mut set, 1, b"a", value, false);
        assert_eq!(listed(&set, 1), keys(&[b"a"]));
        assert_eq!(listed(&set, 2), keys(&[b"c", b"e"]));
        assert!(set.rewind(passed));
        assert_eq!(listed(&set, 2), keys(&[b"a", b"c"]));
        assert!(set.rewind(moved));
        assert_eq!(
            (listed(&set, 1), listed(&set, 2)),
            (keys(&[b"a", b"c"]), keys(&[]))
        );

        // A scope that is taken out: the operations that stay are added
        // again, and the list is what they make it.
        node(&mut set, 1, b"a", Stale::NONE, false);
        set.tag_last(7, true);
        node(&mut set, 3, b"f", value, false);
        set.tag_last(9, true);
        node(&mut set, 1, b"c", Stale::NONE, false);
        set.tag_last(7, true);
        assert_eq!(
            (listed(&set, 1), listed(&set, 3)),
            (keys(&[]), keys(&[b"f"]))
        );
        assert!(set.remove_scope(7));
        assert_eq!(listed(&set, 1), keys(&[b"a", b"c"]));
        assert_eq!((listed(&set, 3), set.unmade), (keys(&[b"f"]), 3));

        // A drop covers the puts, and the end of the writes has none.
        whole(
            &mut set,
            SubtreeWrite::Drop {
                subtree: subtree(1),
            },
        );
        assert_eq!(
            (listed(&set, 1), listed(&set, 3)),
            (keys(&[]), keys(&[b"f"]))
        );
        set.clear();
        set.hold_unmade();
        assert_eq!(
            (listed(&set, 3), set.unmade, set.listed),
            (keys(&[]), 0, false)
        );
    }

    /// What the list costs, by counts: a write of another column asks
    /// for no write before it, however often its subtree was moved, and a
    /// put that is asked for is compared with the range deletes that are
    /// newer than it, and with no other.
    #[test]
    fn the_list_of_stale_puts_asks_for_what_can_pass_and_cover_a_put() {
        let subtree = |name: u8| Subtree::new([name; 32]);
        let hash = [7; 32];
        let node = |set: &mut WriteSet, name: u8, key: &[u8], stale: Stale| {
            let nodes = [NodePut {
                key,
                feature: b"f",
                value_hash: &hash,
                kv_hash: &hash,
                left: None,
                right: None,
                hosted: Hosted::Keep,
                value: ValuePut::Value(b"v"),
                stale,
                at: None,
            }];
            let merks = [MerkWrite {
                subtree: subtree(name),
                nodes: &nodes,
                removed: &[],
                root: RootPut::Keep,
                anchored: false,
                whole: false,
            }];
            let batch = Batch {
                merks: &merks,
                ..Batch::default()
            };
            set.add(&batch, version(1), &[false], &[], &[], &[], &[]);
        };
        let record = |set: &mut WriteSet, write: RecordWrite<'_>| {
            let batch = Batch {
                records: &[write],
                ..Batch::default()
            };
            set.add(&batch, version(1), &[], &[], &[], &[], &[]);
        };
        let moved = |set: &mut WriteSet, from: u8, to: u8| {
            let moves = [SubtreeWrite::Move {
                from: subtree(from),
                to: subtree(to),
                host: HostPut {
                    subtree: subtree(9),
                    key: b"host",
                    position: 0,
                },
                read: None,
                below: &[],
            }];
            let batch = Batch {
                subtrees: &moves,
                ..Batch::default()
            };
            set.add(&batch, version(1), &[], &[], &[], &[], &[]);
        };
        let value = Stale::of(HashField::Value);

        // One put elsewhere is listed, and a subtree goes to and fro.
        let mut set = WriteSet::new();
        node(&mut set, 5, b"s", value);
        for _ in 0..20 {
            moved(&mut set, 1, 2);
            moved(&mut set, 2, 1);
        }
        counted::take();
        let aux = Unit::new(Column::Aux, subtree(1));
        let put = RecordWrite::Put {
            unit: aux,
            key: b"k",
            value: b"v",
        };
        record(&mut set, put);
        assert_eq!(counted::take(), (0, 0), "a write of another column");
        // A put of a node is a write that can pass one. The first under
        // the prefix since a move named it asks where the subtree was
        // before, once: each of the forty moves is a step, and the newest
        // move before a step is searched for among the moves of its
        // prefix, not gone through.
        counted::take_moved();
        node(&mut set, 1, b"k", Stale::NONE);
        assert_eq!(counted::take(), (1, 0), "a put of a node");
        let compared = counted::take_moved();
        assert!(
            (41..=41 * 7).contains(&compared),
            "{compared} moves compared, of forty under each of two prefixes"
        );
        // The next put of the node has the first before it, under the
        // same prefix since the newest move: no move is asked.
        node(&mut set, 1, b"k", Stale::NONE);
        assert_eq!((counted::take(), counted::take_moved()), ((0, 0), 0));

        // Range deletes, then as many puts with keys outside them: no put
        // is compared with a range delete that is older than it.
        let mut set = WriteSet::new();
        let data = Unit::data(subtree(6));
        let outside = RecordWrite::DeleteRange {
            unit: data,
            start: b"z",
            end: None,
        };
        for _ in 0..40 {
            record(&mut set, outside);
        }
        let keys: Vec<Vec<u8>> = (0..40u8).map(|index| vec![b'n', index]).collect();
        for key in &keys {
            node(&mut set, 6, key, value);
        }
        counted::take();
        set.list_unmade(subtree(6), &mut Vec::new());
        assert_eq!(counted::take(), (0, 0), "older range deletes");
        // Three that are newer: each put is compared with each of them.
        for _ in 0..3 {
            record(&mut set, outside);
        }
        let mut puts = Vec::new();
        counted::take();
        set.list_unmade(subtree(6), &mut puts);
        assert_eq!((puts.len(), counted::take()), (40, (0, 120)));
        // One that covers a put ends the comparisons of that put.
        let covers = RecordWrite::DeleteRange {
            unit: data,
            start: b"n\x27",
            end: None,
        };
        record(&mut set, covers);
        assert_eq!(set.unmade(subtree(6)).len(), 39);

        // What is older than the oldest put that is listed is not looked
        // at for a drop or a clear: here two hundred range deletes and a
        // clear from before the one put. Nor is anything looked at for a
        // Merk that lists no put.
        let mut set = WriteSet::new();
        let data = Unit::data(subtree(7));
        let outside = RecordWrite::DeleteRange {
            unit: data,
            start: b"z",
            end: None,
        };
        for _ in 0..100 {
            record(&mut set, outside);
        }
        let clear = [SubtreeWrite::Clear {
            subtree: subtree(7),
        }];
        let cleared = Batch {
            subtrees: &clear,
            ..Batch::default()
        };
        set.add(&cleared, version(1), &[], &[], &[], &[], &[]);
        for _ in 0..100 {
            record(&mut set, outside);
        }
        node(&mut set, 8, b"elsewhere", value);
        counted::take_cut();
        assert_eq!(set.unmade(subtree(7)).len(), 0);
        assert_eq!(counted::take_cut(), 0, "no put is listed in the Merk");
        node(&mut set, 7, b"s", value);
        counted::take();
        assert_eq!(set.unmade(subtree(7)).len(), 1);
        assert_eq!((counted::take_cut(), counted::take().1), (0, 0));
        // A range delete and a clear that are newer are looked at.
        record(&mut set, outside);
        assert_eq!(set.unmade(subtree(7)).len(), 1);
        assert_eq!((counted::take_cut(), counted::take().1), (1, 1));
        set.add(&cleared, version(1), &[], &[], &[], &[], &[]);
        node(&mut set, 7, b"t", value);
        record(&mut set, outside);
        let after: Vec<&[u8]> = (set.unmade(subtree(7)).iter())
            .map(|(_, key)| *key)
            .collect();
        assert_eq!(after, [&b"t"[..]]);
        // From the newest entry back to the clear: the range delete
        // after it, and the clear.
        assert_eq!(counted::take_cut(), 2);
    }

    /// A rollback to a mark in a set whose writers made every hash looks
    /// for no write before a write: the list of the puts that left a hash
    /// stale is held to the entries with one pass over them, however
    /// often one key was written and although the set holds a move. With
    /// one such put in the set the list is held to each entry's write
    /// before it, which is asked for once for each.
    #[test]
    fn a_rollback_in_a_set_with_no_stale_hash_looks_for_no_write_before_a_write() {
        const WRITES: u64 = 300;
        let subtree = |name: u8| Subtree::new([name; 32]);
        let hash = [7; 32];
        let node = |set: &mut WriteSet, name: u8, key: &[u8], stale: Stale| {
            let nodes = [NodePut {
                key,
                feature: b"f",
                value_hash: &hash,
                kv_hash: &hash,
                left: None,
                right: None,
                hosted: Hosted::Keep,
                value: ValuePut::Value(b"v"),
                stale,
                at: None,
            }];
            let merks = [MerkWrite {
                subtree: subtree(name),
                nodes: &nodes,
                removed: &[],
                root: RootPut::Keep,
                anchored: false,
                whole: false,
            }];
            let batch = Batch {
                merks: &merks,
                ..Batch::default()
            };
            set.add(&batch, version(1), &[false], &[], &[], &[], &[]);
        };
        let moves = [SubtreeWrite::Move {
            from: subtree(1),
            to: subtree(2),
            host: HostPut {
                subtree: subtree(9),
                key: b"host",
                position: 0,
            },
            read: None,
            below: &[],
        }];
        let moved = Batch {
            subtrees: &moves,
            ..Batch::default()
        };
        // A move of another subtree, and one key that is written again
        // and again.
        let mut set = WriteSet::new();
        set.add(&moved, version(1), &[], &[], &[], &[], &[]);
        for _ in 0..WRITES {
            node(&mut set, 5, b"k", Stale::NONE);
        }
        let last = set.mark();
        node(&mut set, 5, b"k", Stale::NONE);
        counted::take();
        assert!(set.rewind(last));
        assert_eq!(counted::take().0, 0, "no put left a hash stale");

        // One put that left a hash stale: every write is held to the
        // write before it.
        node(&mut set, 6, b"s", Stale::of(HashField::Value));
        let last = set.mark();
        node(&mut set, 5, b"k", Stale::NONE);
        counted::take();
        assert!(set.rewind(last));
        let asked = counted::take().0;
        assert!((WRITES..WRITES + 8).contains(&asked), "{asked}");
        set.hold_unmade();
        assert_eq!(set.unmade(subtree(6)).len(), 1);
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
            set.add(&batch, version(1), &[], &[], &[], &[], &[]);
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
            set.add(&batch, version(3), &[], &[], &[], &[], &[]);
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
