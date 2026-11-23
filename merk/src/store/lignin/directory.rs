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
    access::Write,
    change_set::{converting, entering},
    refused, LigninBatch, LigninHost, LigninNodeStore, LigninSubtree, Mode,
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
    /// The tree type, as the bytes the directory holds: those the subtree
    /// was entered with, or was last stated anew with
    /// ([`LigninBatch::convert`]).
    pub tree_type: Vec<u8>,
    /// The element that hosts the subtree, or `None` for the root Merk of
    /// a grove and for a Merk that stands alone.
    pub host: Option<LigninHosted>,
}

/// Whether `tree_type`, the bytes a subtree is entered with, is of a tree
/// that keeps its data as flat records and no Merk
/// ([`TreeType::uses_non_merk_data_storage`]). A flat record of such a
/// subtree is read as RocksDB reads it wherever a node is asked for: as
/// the bytes it is stored with (`node-store.md`, "Reads" and "The cursor"
/// of the LigninDB store). The engine is told with this which subtrees
/// those are (`lignindb::txn::FlatKinds`), and so is a check of its
/// database that holds the data column of each subtree to the tree type
/// of its entry (`lignindb::Db::check_kinds_by`).
pub fn keeps_flat_records(tree_type: &[u8]) -> bool {
    tree_type_from_bytes(tree_type).is_ok_and(|tree_type| tree_type.uses_non_merk_data_storage())
}

impl LigninEntry {
    /// The tree type of the subtree, if the directory holds the bytes of
    /// one for it ([`tree_type_bytes`]).
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

    /// States `subtree` anew as a tree of `tree_type`: [`Self::convert`]
    /// with the bytes of that type ([`tree_type_bytes`]).
    pub fn convert_tree(&self, subtree: &LigninSubtree, tree_type: TreeType) -> Result<(), Error> {
        self.convert(subtree, &tree_type_bytes(tree_type))
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

    /// States the handle's subtree anew in the subtree directory as a tree
    /// of `tree_type`, at the place the handle names among the subtrees of
    /// its host ([`LigninSubtree::host`]): staged on a deferred handle, in
    /// the batch it was opened with, and made at once on an immediate one.
    ///
    /// It is what a conversion in place states beside the nodes it writes
    /// again: in the same batch on a deferred handle, and as a write of
    /// its own on an immediate one. The subtree is entered already. It is
    /// no write of a record, so it is charged nothing.
    ///
    /// The cell of the host names its Merks in the new order when a batch
    /// writes the host's node and states the Merks it hosts with it, as
    /// the batch of a deferred handle does. A statement that is made at
    /// once writes no node: after one that gives the Merk another place,
    /// the host's cell has the old order until a write states what the
    /// host hosts.
    pub fn convert(&self, tree_type: TreeType) -> Result<(), Error> {
        match self.mode {
            Mode::ReadOnly | Mode::Deferred(None) => {
                Err(refused("a conversion through a handle that only reads"))
            }
            Mode::Deferred(Some(batch)) => batch.convert_tree(&self.subtree, tree_type),
            Mode::Immediate => {
                let convert = converting(&self.subtree, &tree_type_bytes(tree_type))?;
                let write = Write {
                    subtrees: vec![convert],
                    ..Write::default()
                };
                self.apply(&write)
            }
        }
    }

    /// Puts the node under `key` of the handle's Merk again as the
    /// transaction holds it, and names in it the Merks its element hosts,
    /// by position: the node's cell then points at the root that each of
    /// them has (`tree-layout.md` C3), and at none for one with no node.
    ///
    /// It is for a Merk that an immediate handle committed by itself, as a
    /// restore hands one over: the subtree directory names its root, and
    /// its host does not point at it until a write names the Merk there.
    /// The node keeps its value, and its feature, its hashes and its links
    /// are stated as they were read, so nothing that a Merk reads of the
    /// node changes and no hash is made. Like an enter it is no write of a
    /// record for the cost rules, and is charged nothing.
    ///
    /// The cell of the node grows by each Merk it comes to name, and the
    /// tree pages carry a cell only for a write that holds the cell which
    /// points at it. So the node above the node is put again with it, in
    /// the same way (`node-store.md`, "A batch that stops below an
    /// element"). A node that is the root of its Merk has none above it:
    /// `above` are the elements of the handle's Merk and of the Merks
    /// above it, innermost first, each with every Merk it hosts, and each
    /// is named so where the node below it is the root of its Merk.
    ///
    /// The handle is an immediate one, of the subtree the node is in. A
    /// key that holds no node of the Merk is refused, and so is an
    /// element whose first Merk is not entered under it.
    pub fn name_hosted(
        &self,
        key: &[u8],
        hosted: &[[u8; 32]],
        above: &[LigninHost],
    ) -> Result<(), Error> {
        if !matches!(self.mode, Mode::Immediate) {
            return Err(refused(
                "the Merks of a host are named through a handle that does not write at once",
            ));
        }
        let element = LigninHost {
            subtree: self.subtree.prefix,
            key: key.to_vec(),
            hosted: hosted.to_vec(),
        };
        let hosts: Vec<LigninHost> = std::iter::once(element)
            .chain(above.iter().cloned())
            .collect();
        self.access.name_hosted(&hosts)
    }
}
