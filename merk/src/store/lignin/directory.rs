//! What the subtree directory of LigninDB holds of a subtree, and how a
//! subtree is entered in it: what the open of a Merk needs (MERK-04).
//!
//! A subtree is in the directory from the write that enters it: the batch
//! that writes the element which hosts it ([`LigninBatch::enter`]), and for
//! a Merk with no host, the root Merk of a grove and a Merk that stands
//! alone, whoever makes it ([`LigninNodeStore::enter`]). The transaction
//! reads nothing of a subtree that is not entered, so a Merk opened there
//! is empty, and it refuses every write to one.
//!
//! The directory keeps three things of an entered subtree, and
//! [`LigninNodeStore::entry`] reads them:
//!
//! - **The key of the root node** of its Merk, which every commit states
//!   (`NodeChangeSet::state_root`). A Merk under a parent element is opened
//!   with the root key that element holds, and the directory names the
//!   same node. A Merk with no host keeps its root key in a record of its
//!   own beside it, in the flat keyspace `roots`
//!   ([`NodeStore::root_key`](crate::store::NodeStore::root_key)).
//! - **The tree type**, as bytes ([`tree_type_bytes`]).
//! - **The element that hosts it** ([`LigninHosted`]).

use super::{
    access::Write, change_set::entering, refused, LigninBatch, LigninNodeStore, LigninSubtree, Mode,
};
use crate::{Error, TreeType};

/// The parameter a tree type carries beside its kind, if it has one.
const fn parameter(tree_type: TreeType) -> Option<u8> {
    match tree_type {
        TreeType::CommitmentTree(parameter)
        | TreeType::BulkAppendTree(parameter)
        | TreeType::DenseAppendOnlyFixedSizeTree(parameter)
        | TreeType::PrivateDocumentStore(parameter) => Some(parameter),
        TreeType::NormalTree
        | TreeType::SumTree
        | TreeType::BigSumTree
        | TreeType::CountTree
        | TreeType::CountSumTree
        | TreeType::ProvableCountTree
        | TreeType::ProvableCountSumTree
        | TreeType::MmrTree
        | TreeType::ProvableSumTree
        | TreeType::ProvableCountProvableSumTree
        | TreeType::ProvableSumIndexedTree
        | TreeType::ProvableBigSumIndexedTree
        | TreeType::ProvableCountIndexedTree
        | TreeType::ProvableCountProvableSumIndexedTree
        | TreeType::ProvableCountProvableBigSumIndexedTree
        | TreeType::ProvableCountProvableBigSumTree
        | TreeType::CountBigSumTree
        | TreeType::ProvableBigSumTree => None,
    }
}

/// The bytes the subtree directory keeps for `tree_type`: its discriminant,
/// and after it the parameter of a tree type that has one.
pub fn tree_type_bytes(tree_type: TreeType) -> Vec<u8> {
    let mut bytes = vec![tree_type.discriminant()];
    bytes.extend(parameter(tree_type));
    bytes
}

/// The tree type that [`tree_type_bytes`] gave `bytes` for. Other bytes are
/// those of no tree type.
pub fn tree_type_from_bytes(bytes: &[u8]) -> Result<TreeType, Error> {
    let unknown = || Error::UnknownTreeType(format!("the bytes {bytes:?} of a subtree directory"));
    let (discriminant, rest) = bytes.split_first().ok_or_else(unknown)?;
    let kind = TreeType::try_from(*discriminant)?;
    match (kind, rest) {
        (TreeType::CommitmentTree(_), [parameter]) => Ok(TreeType::CommitmentTree(*parameter)),
        (TreeType::BulkAppendTree(_), [parameter]) => Ok(TreeType::BulkAppendTree(*parameter)),
        (TreeType::DenseAppendOnlyFixedSizeTree(_), [parameter]) => {
            Ok(TreeType::DenseAppendOnlyFixedSizeTree(*parameter))
        }
        (TreeType::PrivateDocumentStore(_), [parameter]) => {
            Ok(TreeType::PrivateDocumentStore(*parameter))
        }
        (kind, []) if parameter(kind).is_none() => Ok(kind),
        _ => Err(unknown()),
    }
}

/// The element that hosts a subtree, as the subtree directory names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LigninHosted {
    /// The subtree of the Merk that holds the element.
    pub subtree: [u8; 32],
    /// The element's key in that Merk.
    pub key: Vec<u8>,
    /// The position of the subtree among those the element hosts.
    pub position: u8,
}

/// What the subtree directory holds of an entered subtree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LigninEntry {
    /// The key of the root node of the subtree's Merk, or `None` for a
    /// Merk with no node and for a tree that keeps its data flat.
    pub root_key: Option<Vec<u8>>,
    /// The tree type, as the bytes the subtree was entered with.
    pub tree_type: Vec<u8>,
    /// The element that hosts the subtree, or `None` for the root Merk of
    /// a grove and for a Merk that stands alone.
    pub host: Option<LigninHosted>,
}

impl LigninEntry {
    /// The tree type of the subtree, if it was entered with the bytes of
    /// one ([`tree_type_bytes`]).
    pub fn tree_type(&self) -> Result<TreeType, Error> {
        tree_type_from_bytes(&self.tree_type)
    }
}

impl LigninBatch {
    /// Enters `subtree` as a tree of `tree_type`: [`Self::enter`] with the
    /// bytes of that type ([`tree_type_bytes`]), which is what a Merk is
    /// opened from the directory by.
    pub fn enter_tree(&self, subtree: &LigninSubtree, tree_type: TreeType) -> Result<(), Error> {
        self.enter(subtree, &tree_type_bytes(tree_type))
    }
}

impl LigninNodeStore<'_> {
    /// What the subtree directory holds of the handle's subtree as its
    /// transaction shows it, or `None` if the subtree is not entered: the
    /// open by prefix, which is one lookup in the directory
    /// (`WriteTxn::subtree`).
    ///
    /// It is the transaction's view, as every read of the handle is: its
    /// own enters, drops and roots are in it, and nothing that a batch has
    /// staged. Like a read of a node it is refused where another open
    /// scope of the transaction writes the subtree's Merk. It has no
    /// counterpart in the cost rules and is charged nothing.
    pub fn entry(&self) -> Result<Option<LigninEntry>, Error> {
        self.access.subtree(&self.subtree.prefix)
    }

    /// Enters the handle's subtree in the subtree directory as a tree of
    /// `tree_type`, with no node: staged on a deferred handle, in the batch
    /// it was opened with, and made at once on an immediate one.
    ///
    /// A subtree is entered once, before anything is written in it. The
    /// same batch may write it: a batch enters its subtrees before it
    /// applies a change set or a record. An enter is no write of a record,
    /// so it is charged nothing.
    pub fn enter(&self, tree_type: TreeType) -> Result<(), Error> {
        match self.mode {
            Mode::ReadOnly | Mode::Deferred(None) => {
                Err(refused("an enter through a handle that only reads"))
            }
            Mode::Deferred(Some(batch)) => batch.enter_tree(&self.subtree, tree_type),
            Mode::Immediate => {
                let enter = entering(&self.subtree, &tree_type_bytes(tree_type), false)?;
                let write = Write {
                    subtrees: vec![enter],
                    ..Write::default()
                };
                self.apply(&write)
            }
        }
    }
}
