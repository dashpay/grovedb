//! The errors of a write transaction.

use std::{error::Error, fmt};

use lignin_format::{FormatError, PageId};
use lignin_store::{
    alloc::AllocError, cache::CacheError, load::LoadError, values::ValueStoreError,
};
use lignin_tree::{
    cursor::CursorError, directory::DirectoryError, flat::FlatError, fragment::FragmentError,
    node::NodeError, value::ValueError,
};

use super::key::{Subtree, Unit};

/// The element that hosts a subtree, as an error names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostName {
    /// The subtree of the Merk the element is a node of.
    pub subtree: Subtree,
    /// The element's key in that Merk.
    pub key: Box<[u8]>,
    /// The position of the hosted subtree among those the element hosts.
    pub position: u8,
}

impl fmt::Display for HostName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            subtree,
            key,
            position,
        } = self;
        write!(f, "the key {} of {subtree}, at {position}", Hex(key))
    }
}

/// What a subtree is entered with and what an enter of it states, where
/// the two are not the same ([`BatchError::EnteredOtherwise`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Differs {
    /// The tree types.
    TreeType {
        /// The tree type the subtree is entered with.
        entered: Box<[u8]>,
        /// The tree type the enter states.
        stated: Box<[u8]>,
    },
    /// The elements that host the subtree, where the tree types are the
    /// same.
    Host {
        /// The element the subtree is entered under, or none.
        entered: Option<HostName>,
        /// The element the enter states, or none.
        stated: Option<HostName>,
    },
}

struct Hex<'a>(&'a [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("0x")?;
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl fmt::Display for Differs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = |host: &Option<HostName>| match host {
            Some(host) => host.to_string(),
            None => "no element".to_owned(),
        };
        match self {
            Self::TreeType { entered, stated } => write!(
                f,
                "as a tree of the type {}, and the enter states the type {}",
                Hex(entered),
                Hex(stated)
            ),
            Self::Host { entered, stated } => write!(
                f,
                "under {}, and the enter states {}",
                host(entered),
                host(stated)
            ),
        }
    }
}

/// What is wrong with a [`Batch`](super::Batch). A batch that is refused
/// with one of these has changed nothing (`transactions.md` WT10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchError {
    /// A record key is written twice in one batch. A batch holds one
    /// outcome for a key (GM0).
    KeyTwice {
        /// The unit of the key.
        unit: Unit,
    },
    /// A Merk has two change sets in one batch.
    MerkTwice {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// The key of a node, or of a child a link names, is longer than a
    /// cell holds.
    KeyTooLong {
        /// The Merk's subtree.
        subtree: Subtree,
        /// The length of the key.
        len: usize,
    },
    /// A node hosts more Merks than a cell has pointers for.
    TooManyHosted {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A value is longer than the value log holds, or a node that is kept
    /// as an unlinked record is longer with its value than a flat record
    /// may be: the value shares that length with the node's other parts.
    ValueTooLong {
        /// The length of the value.
        len: usize,
    },
    /// A link names a child that the Merk does not hold in the
    /// transaction's view.
    NoChild {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// The root that the change set names is a key with no node: the Merk
    /// holds none under it in the transaction's view, or the change set
    /// itself deletes it.
    NoRoot {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A node keeps a value, and the transaction's view has no node under
    /// its key.
    NoValue {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A node keeps a value of the committed state, and its write does not
    /// say which read the value is kept from: it names no handle
    /// ([`NodePut::at`](super::NodePut::at)). The transaction could not
    /// say whether a commit wrote the node's key since (`crate::txn`, "A
    /// kept value and the read it is of"). A node the transaction has
    /// written itself is kept with no handle.
    NoRead {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A node keeps a value of the committed state, and a commit outside
    /// the transaction took the node away after the read the write names
    /// ([`NodePut::at`](super::NodePut::at)): it deleted the node, or
    /// dropped or cleared its Merk, and the committed state has no node
    /// under the key. It is what [`Self::NoValue`] or [`Self::NotEntered`]
    /// would say, where the transaction can tell that such commits are the
    /// one cause: for every node of the Merk that the batch has no value
    /// to keep of, the read is one the transaction can date, the key set
    /// of a commit since holds the node's key, and nothing the transaction
    /// wrote itself took the node away (`crate::txn`, "A kept value and
    /// the read it is of"). The handle a keep names is taken to be of the
    /// node it is named for.
    ///
    /// **It is a conflict** ([`Self::is_conflict`]), and not something the
    /// batch got wrong. The batch has changed nothing and the transaction
    /// is as it was (WT10), so a caller that takes it as a conflict rolls
    /// the transaction back and makes its reads and its writes again.
    ValueTaken {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// The transaction made the first node of a Merk that was empty, and a
    /// commit outside it has made one since, with no key in common. The
    /// two roots cannot both stand, and the conflict rule did not see it:
    /// whoever writes the first node of a Merk also writes a record that
    /// every such writer writes, which on existing GroveVersions is the
    /// element that hosts the Merk or the base root key (GM1, GM3).
    RootTaken {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A change set sets the root of a Merk, and writes and deletes no node
    /// of it. A root changes only with a node, and the root of a Merk that
    /// is as it was is kept ([`RootPut::Keep`](super::RootPut::Keep)).
    RootAlone {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A flat record is written, or a range deleted, in the data column of
    /// a subtree that holds Merk nodes there: in the batch, in the
    /// transaction's write set, or in the state it reads. A node is written
    /// through the change set of its Merk, and a Merk is dropped, not
    /// deleted by range (ST1, CV9, TREE-09).
    NotFlat {
        /// The unit.
        unit: Unit,
    },
    /// A change set names a Merk in whose data column the transaction has
    /// written flat records (ST1).
    NotMerk {
        /// The Merk's subtree.
        subtree: Subtree,
    },
    /// A node is written that keeps the Merks it hosts
    /// ([`Hosted::Keep`](super::Hosted::Keep)), and the batch holds the
    /// change set of one of them. The write that holds a Merk and its host
    /// names the Merk in the host.
    HostKeeps {
        /// The subtree of the hosted Merk.
        subtree: Subtree,
    },
    /// A change set, a drop or a clear names a subtree that is not entered
    /// in the transaction's view, or a subtree is entered whose host is in
    /// one that is not. A subtree is entered by the batch that writes the
    /// element which hosts it
    /// ([`SubtreeWrite::Enter`](super::SubtreeWrite::Enter)).
    NotEntered {
        /// The subtree that is not entered.
        subtree: Subtree,
    },
    /// A node that the tree of its Merk does not reach once the batch is
    /// applied is said to host a Merk
    /// ([`Hosted::Set`](super::Hosted::Set)). Such a node is kept as an
    /// unlinked record, which hosts nothing: the batch that leaves it
    /// drops what its element hosted, and states that it hosts none.
    HostUnlinked {
        /// The subtree of the Merk the node is written in.
        subtree: Subtree,
    },
    /// A node that the tree of its Merk does not reach once the batch is
    /// applied leaves a hash to be made
    /// ([`NodePut::stale`](super::NodePut::stale)). Such a node is kept as
    /// an unlinked record, and nobody makes a hash of one: its writer
    /// states them all.
    StaleUnlinked {
        /// The subtree of the Merk the node is written in.
        subtree: Subtree,
    },
    /// A subtree is entered that is entered already in the transaction's
    /// view, and that the batch does not drop.
    Entered {
        /// The subtree.
        subtree: Subtree,
    },
    /// A subtree is entered if it is absent
    /// ([`SubtreeWrite::EnterIfAbsent`](super::SubtreeWrite::EnterIfAbsent)),
    /// and it is entered already as a tree of another type or under
    /// another element. That is no race with another writer: whoever
    /// enters one subtree in two places says the same of it in both.
    EnteredOtherwise {
        /// The subtree.
        subtree: Subtree,
        /// What it is entered with, and what the enter states.
        differs: Box<Differs>,
    },
    /// A subtree is dropped or cleared that an earlier operation of the
    /// same staged batch entered if it was absent
    /// ([`StagedBatch::enter_if_absent`](crate::batch::StagedBatch::enter_if_absent)),
    /// and the batch holds no drop and no clear of it from before that
    /// enter. It does not know then whether the transaction has the
    /// subtree to leave, and holds one outcome for it.
    LeavesIfEntered {
        /// The subtree.
        subtree: Subtree,
    },
    /// A batch enters one subtree twice, drops or clears it twice, or
    /// states it anew twice, or beside its enter, its drop or its clear,
    /// or its moves take a subtree from one prefix twice or bring two to
    /// one.
    SubtreeTwice {
        /// The subtree.
        subtree: Subtree,
    },
    /// A subtree is moved ([`SubtreeWrite::Move`](super::SubtreeWrite::Move))
    /// to a host it cannot have: the subtree has no host; the host that is
    /// stated is in another Merk or at another position than the one it
    /// has; the batch does not put that node; the node names the
    /// subtree's new prefix among the Merks it hosts at another position;
    /// it does not name that prefix at the position where the cell of the
    /// node that hosts the subtree before hosts a Merk there; or its cell
    /// hosts a Merk at the position where that cell hosts none there.
    MoveHost {
        /// The subtree, by the prefix it is moved from.
        subtree: Subtree,
    },
    /// A subtree is moved, and a subtree below it has no prefix named for
    /// it and does not leave with the move: it has a node, or a subtree
    /// below it is named.
    MoveUnnamed {
        /// The subtree below the moved one.
        subtree: Subtree,
    },
    /// A batch that moves a subtree states something else of a subtree
    /// its moves name: an enter, a clear, a change set, a record, a drop
    /// of one that is taken from its prefix, or the host of a move in it.
    /// Or it drops or clears a subtree below one that is moved, or one
    /// that the moved subtree's host is in or below. Or a move names a
    /// new prefix for a subtree that is entered and is not below the one
    /// it moves. The one statement a batch makes beside a move of a prefix
    /// the move names is the drop of the subtree that is entered where a
    /// subtree is moved to.
    Moved {
        /// The subtree the other statement names.
        subtree: Subtree,
    },
    /// A scope that keeps its writes in the write set states a move,
    /// reads or writes a subtree that a move of the transaction decides,
    /// or drops or clears a subtree while the transaction's write set
    /// holds a move (`transactions.md` MV7, MV8). Or a move is stated
    /// while a pending mark stands under a prefix it names, or a call
    /// that set such a mark or took one off is among the writes that the
    /// transaction's frames show (MV8). Or a cursor reads a subtree that
    /// a move of the transaction decides after a commit was published
    /// that the cursor's version does not hold, and the transaction's
    /// writes are laid out over the latest version (MV7).
    MoveBeside {
        /// The subtree the refused move takes from its prefix, the first
        /// subtree the refused batch drops or clears, or the subtree
        /// whose read or write was refused.
        subtree: Subtree,
    },
    /// The transaction serves its records by key because a commit outside
    /// it met one of its writes (WT7b), and the operation states a move,
    /// reads or writes a subtree that a move in its write set decides, or
    /// drops or clears a subtree while its write set holds a move
    /// (`transactions.md` MV7): its frames would say what is there, and
    /// it has none.
    ///
    /// **It is a conflict** ([`Self::is_conflict`]): the transaction's
    /// commit fails with one unless a rollback to a savepoint takes the
    /// met writes out, after which it has frames again where the writes
    /// that are left can be laid out. Nothing was changed, and the
    /// transaction is still open.
    MovedByKey {
        /// The subtree that was read or written, the first one the
        /// refused batch drops or clears, or the one a move was stated
        /// for.
        subtree: Subtree,
    },
    /// The tree type of an entered subtree is longer than the subtree
    /// directory keeps.
    TreeTypeTooLong {
        /// The subtree.
        subtree: Subtree,
        /// The length of the tree type.
        len: usize,
    },
}

impl fmt::Display for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyTwice { unit } => write!(f, "a key of {unit} is written twice"),
            Self::MerkTwice { subtree } => {
                write!(f, "the Merk {subtree} has two change sets")
            }
            Self::KeyTooLong { subtree, len } => {
                write!(f, "a node key of {len} bytes in the Merk {subtree}")
            }
            Self::TooManyHosted { subtree } => {
                write!(f, "a node of the Merk {subtree} hosts too many Merks")
            }
            Self::ValueTooLong { len } => write!(f, "a value of {len} bytes"),
            Self::NoChild { subtree } => {
                write!(f, "a link names a child the Merk {subtree} does not hold")
            }
            Self::NoRoot { subtree } => {
                write!(f, "the root of the Merk {subtree} is a key with no node")
            }
            Self::NoValue { subtree } => {
                write!(
                    f,
                    "a node of the Merk {subtree} keeps a value it does not have"
                )
            }
            Self::NoRead { subtree } => {
                write!(
                    f,
                    "a node of the Merk {subtree} keeps a committed value and names no handle of \
                     the read it kept it from"
                )
            }
            Self::ValueTaken { subtree } => {
                write!(
                    f,
                    "a node of the Merk {subtree} keeps a value that a commit outside the \
                     transaction took away after the node was read"
                )
            }
            Self::RootTaken { subtree } => {
                write!(
                    f,
                    "the empty Merk {subtree} was given a root outside the transaction"
                )
            }
            Self::RootAlone { subtree } => {
                write!(f, "the root of the Merk {subtree} is set with no node")
            }
            Self::NotFlat { unit } => {
                write!(f, "a flat record is written among the Merk nodes in {unit}")
            }
            Self::NotMerk { subtree } => {
                write!(f, "the Merk {subtree} is written among flat records")
            }
            Self::HostKeeps { subtree } => {
                write!(
                    f,
                    "the host of the Merk {subtree} keeps its pointers in a batch that writes the Merk"
                )
            }
            Self::HostUnlinked { subtree } => {
                write!(
                    f,
                    "a node of the Merk {subtree} that its tree does not reach hosts a Merk"
                )
            }
            Self::StaleUnlinked { subtree } => {
                write!(
                    f,
                    "a node of the Merk {subtree} that its tree does not reach leaves a hash to \
                     be made"
                )
            }
            Self::NotEntered { subtree } => write!(f, "the subtree {subtree} is not entered"),
            Self::Entered { subtree } => {
                write!(f, "the subtree {subtree} is entered already")
            }
            Self::EnteredOtherwise { subtree, differs } => {
                write!(f, "the subtree {subtree} is entered {differs}")
            }
            Self::LeavesIfEntered { subtree } => {
                write!(
                    f,
                    "the subtree {subtree} leaves in a batch that entered it if it was absent"
                )
            }
            Self::SubtreeTwice { subtree } => {
                write!(
                    f,
                    "the subtree {subtree} is entered, leaves, is stated anew, or is on one side of a move, twice"
                )
            }
            Self::MoveHost { subtree } => {
                write!(f, "the subtree {subtree} is moved to a host it cannot have")
            }
            Self::MoveUnnamed { subtree } => {
                write!(
                    f,
                    "the subtree {subtree} is below a moved one, and no prefix is named for it"
                )
            }
            Self::Moved { subtree } => {
                write!(
                    f,
                    "the subtree {subtree} is stated otherwise in a batch whose move takes it"
                )
            }
            Self::MoveBeside { subtree } => {
                write!(
                    f,
                    "the subtree {subtree} is moved where the move cannot be laid out"
                )
            }
            Self::MovedByKey { subtree } => {
                write!(
                    f,
                    "the subtree {subtree} is of a move, in a transaction that a commit has met"
                )
            }
            Self::TreeTypeTooLong { subtree, len } => {
                write!(f, "a tree type of {len} bytes for the subtree {subtree}")
            }
        }
    }
}

impl Error for BatchError {}

impl BatchError {
    /// Whether the refusal is [`Self::ValueTaken`] or
    /// [`Self::MovedByKey`]: those that a caller answers as it answers a
    /// commit that fails with a conflict.
    pub const fn is_conflict(&self) -> bool {
        matches!(self, Self::ValueTaken { .. } | Self::MovedByKey { .. })
    }
}

/// Why an operation of a write transaction failed. The transaction's write
/// set is what it was before the call (`transactions.md` WT10).
#[derive(Debug)]
pub enum TxnError {
    /// The batch was refused.
    Batch(BatchError),
    /// A scope of the transaction is open, and the call needs every scope
    /// closed: a rollback of the transaction, a rollback to a mark, or its
    /// commit (`transactions.md` SC8, CM1; the specification's
    /// `ScopesOpen`). The transaction is as it was.
    ScopeOpen,
    /// The operation touches a unit that another open scope of the
    /// transaction holds: it writes where that scope has read or written,
    /// or reads where that scope has written (SC3). A claim never waits.
    /// The operation had no effect, and its scope stays open and usable.
    ScopeOverlap {
        /// The unit that the other scope holds: the one the operation
        /// names, or a subtree above it that the other scope has dropped
        /// or cleared.
        unit: Unit,
    },
    /// The transaction has a pending mark left: its tree is not finalized
    /// up to the root, and it is not committed so (SC9, CM1). The
    /// transaction is as it was.
    NotFinalized,
    /// The tree layer did not apply a write.
    Fragment(FragmentError),
    /// A tree page was not read.
    Node(NodeError),
    /// The committed side of a cursor over the nodes of a Merk did not
    /// move.
    Cursor(CursorError),
    /// A flat keyspace was not read or not written.
    Flat(FlatError),
    /// The buffer pool refused a private copy.
    Cache(CacheError),
    /// No page id was given, or one was not taken back.
    Alloc(AllocError),
    /// A page was not loaded.
    Load(LoadError),
    /// A value was not read from the value log.
    Values(ValueStoreError),
    /// A value of a node that is not in its cell was not kept or not read
    /// (`lignin_tree::value`).
    TreeValue(ValueError),
    /// Bytes that should be a page or a value are not.
    Format(FormatError),
    /// The subtree directory did not answer, or refused a write
    /// (`lignin_tree::directory`).
    Directory(DirectoryError),
    /// A tree page that the transaction freed was asked for again.
    PageFreed {
        /// The page.
        page: PageId,
    },
    /// The pages do not hold what every committed state holds.
    Damaged(&'static str),
}

impl fmt::Display for TxnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Batch(error) => write!(f, "batch: {error}"),
            Self::ScopeOpen => f.write_str("a scope of the transaction is open"),
            Self::ScopeOverlap { unit } => {
                write!(f, "another open scope of the transaction holds {unit}")
            }
            Self::NotFinalized => f.write_str("the transaction has a pending mark left"),
            Self::Fragment(error) => write!(f, "tree pages: {error}"),
            Self::Node(error) => write!(f, "node: {error}"),
            Self::Cursor(error) => write!(f, "cursor: {error}"),
            Self::Flat(error) => write!(f, "flat keyspace: {error}"),
            Self::Cache(error) => write!(f, "buffer pool: {error}"),
            Self::Alloc(error) => write!(f, "page ids: {error}"),
            Self::Load(error) => write!(f, "page load: {error}"),
            Self::Values(error) => write!(f, "value log: {error}"),
            Self::TreeValue(error) => write!(f, "node value: {error}"),
            Self::Format(error) => write!(f, "format: {error}"),
            Self::Directory(error) => write!(f, "subtree directory: {error}"),
            Self::PageFreed { page } => write!(f, "{page} was freed by the transaction"),
            Self::Damaged(what) => write!(f, "damaged pages: {what}"),
        }
    }
}

impl Error for TxnError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Batch(error) => Some(error),
            Self::Fragment(error) => Some(error),
            Self::Node(error) => Some(error),
            Self::Cursor(error) => Some(error),
            Self::Flat(error) => Some(error),
            Self::Cache(error) => Some(error),
            Self::Alloc(error) => Some(error),
            Self::Load(error) => Some(error),
            Self::Values(error) => Some(error),
            Self::TreeValue(error) => Some(error),
            Self::Format(error) => Some(error),
            Self::Directory(error) => Some(error),
            Self::ScopeOpen
            | Self::ScopeOverlap { .. }
            | Self::NotFinalized
            | Self::PageFreed { .. }
            | Self::Damaged(_) => None,
        }
    }
}

macro_rules! from {
    ($($source:ty => $variant:ident),* $(,)?) => {$(
        impl From<$source> for TxnError {
            fn from(error: $source) -> Self {
                Self::$variant(error)
            }
        }
    )*};
}

from! {
    BatchError => Batch,
    FragmentError => Fragment,
    NodeError => Node,
    CursorError => Cursor,
    FlatError => Flat,
    CacheError => Cache,
    AllocError => Alloc,
    LoadError => Load,
    ValueStoreError => Values,
    ValueError => TreeValue,
    FormatError => Format,
    DirectoryError => Directory,
}

impl TxnError {
    /// Whether the operation was refused with [`BatchError::ValueTaken`]
    /// or [`BatchError::MovedByKey`] ([`BatchError::is_conflict`]). The
    /// transaction is as it was, and still open.
    pub const fn is_conflict(&self) -> bool {
        matches!(self, Self::Batch(refused) if refused.is_conflict())
    }

    /// The error as the tree layer takes one from the pages of a write
    /// (`FragmentError::Pages`).
    pub(crate) fn into_pages(self) -> FragmentError {
        match self {
            Self::Fragment(error) => error,
            Self::Node(error) => FragmentError::Node(error),
            other => FragmentError::Pages(Box::new(other)),
        }
    }
}
