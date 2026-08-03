//! Apply multiple GroveDB operations atomically.

mod batch_structure;

/// Indexed-tree helpers for the batch apply pipeline (pre-apply
/// aggregate capture + post-apply secondary mirror) — covers
/// `ProvableCountIndexedTree`, `ProvableSumIndexedTree`, and
/// `ProvableCountProvableSumIndexedTree`. Extracted from `mod.rs` to
/// keep the propagation pattern self-contained.
mod indexed_tree;

#[cfg(feature = "estimated_costs")]
pub mod estimated_costs;

pub mod key_info;

mod mode;
#[cfg(test)]
mod multi_insert_cost_tests;

#[cfg(test)]
mod just_in_time_cost_tests;
/// Just-in-time reference update handling for batch operations.
pub mod just_in_time_reference_update;
mod options;
mod refresh_reference_mode;
#[cfg(test)]
mod single_deletion_cost_tests;
#[cfg(test)]
mod single_insert_cost_tests;
#[cfg(test)]
mod single_sum_item_deletion_cost_tests;
#[cfg(test)]
mod single_sum_item_insert_cost_tests;

use core::fmt;
use std::{
    cmp::Ordering,
    collections::{btree_map::Entry, hash_map::Entry as HashMapEntry, BTreeMap, HashMap, HashSet},
    hash::{Hash, Hasher},
    ops::{Add, AddAssign},
    slice::Iter,
    vec::IntoIter,
};

#[cfg(feature = "estimated_costs")]
use estimated_costs::{
    average_case_costs::AverageCaseTreeCacheKnownPaths,
    worst_case_costs::WorstCaseTreeCacheKnownPaths,
};
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_into_no_add,
    cost_return_on_error_no_add,
    storage_cost::{
        removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
        StorageCost,
    },
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, delete::ElementDeleteFromStorageExtensions,
        exists::ElementExistsInStorageExtensions, get::ElementFetchFromStorageExtensions,
        insert::ElementInsertToStorageExtensions, tree_type::ElementTreeTypeExtensions,
    },
    tree::{
        kv::ValueDefinedCostType::{LayeredValueDefinedCost, SpecializedValueDefinedCost},
        value_hash, AggregateData, NULL_HASH,
    },
    tree_type::{CostSize, TreeType, SUM_ITEM_COST_SIZE},
    CryptoHash, Error as MerkError, Merk, MerkType, OldValueDisposition, Op,
    RootHashKeyAndAggregateData,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::PrefixedRocksDbTransactionContext, Storage, StorageBatch, StorageContext,
};
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};
use grovedb_visualize::{Drawer, Visualize};
use integer_encoding::VarInt;
use itertools::Itertools;
use key_info::{KeyInfo, KeyInfo::KnownKey};
pub use options::BatchApplyOptions;
pub use refresh_reference_mode::RefreshReferenceMode;

pub use crate::batch::batch_structure::{OpsByLevelPath, OpsByPath};
#[cfg(feature = "estimated_costs")]
use crate::batch::estimated_costs::EstimatedCostsType;
use crate::{
    batch::{batch_structure::BatchStructure, mode::BatchRunMode},
    element::{MaxReferenceHop, SumValue},
    operations::{delete::DeleteOptions, get::MAX_REFERENCE_HOPS, proof::util::hex_to_ascii},
    reference_path::{
        path_from_reference_path_type, path_from_reference_qualified_path_type, ReferencePathType,
    },
    util::TxRef,
    Element, ElementFlags, Error, GroveDb, Transaction, TransactionArg,
};

/// Controls how a `DeleteTree` operation handles non-empty subtrees.
///
/// This enum is attached to each `DeleteTree` operation individually,
/// replacing the old batch-level `allow_deleting_non_empty_trees` /
/// `deleting_non_empty_trees_returns_error` flags on `BatchApplyOptions`.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum SubelementsDeletionBehavior {
    /// Do not check whether the subtree is empty before deleting, and skip
    /// post-apply storage cleanup. The tree element is removed from the
    /// parent Merk unconditionally but no child subtree storage is cleared.
    /// Callers use this when they have already ensured the subtree is empty
    /// and want to avoid the I/O cost of both the emptiness check and the
    /// cleanup phase.
    DontCheckWithNoCleanup,
    /// Check emptiness. If the subtree is non-empty, return
    /// `Error::DeletingNonEmptyTree`.
    Error,
    /// Do not check whether the subtree is empty before deleting, but
    /// still perform post-apply storage cleanup to remove the child
    /// subtree's storage (and any nested subtrees). Use this when the
    /// subtree may contain children that should be recursively cleaned up.
    DeleteChildren,
    /// Check emptiness. If the subtree is non-empty, silently skip this
    /// `DeleteTree` operation (no error, no deletion).
    Skip,
}

/// Metadata for non-Merk tree types, carrying tree-type-specific state
/// through the batch system.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum NonMerkTreeMeta {
    /// CommitmentTree state: total_count and chunk_power.
    CommitmentTree {
        /// Total number of entries appended so far.
        total_count: u64,
        /// Power-of-2 chunk size for the bulk append layer.
        chunk_power: u8,
    },
    /// MmrTree state: mmr_size.
    MmrTree {
        /// MMR size (number of nodes, not leaves).
        mmr_size: u64,
    },
    /// BulkAppendTree state: total_count and chunk_power.
    BulkAppendTree {
        /// Total number of entries appended so far.
        total_count: u64,
        /// Power-of-2 chunk size for epochs.
        chunk_power: u8,
    },
    /// DenseAppendOnlyFixedSizeTree state: count and height.
    DenseTree {
        /// Number of entries inserted so far.
        count: u16,
        /// Fixed height of the dense Merkle tree.
        height: u8,
    },
}

impl NonMerkTreeMeta {
    /// Returns the `TreeType` corresponding to this metadata.
    pub fn to_tree_type(&self) -> TreeType {
        match self {
            NonMerkTreeMeta::CommitmentTree { chunk_power, .. } => {
                TreeType::CommitmentTree(*chunk_power)
            }
            NonMerkTreeMeta::MmrTree { .. } => TreeType::MmrTree,
            NonMerkTreeMeta::BulkAppendTree { chunk_power, .. } => {
                TreeType::BulkAppendTree(*chunk_power)
            }
            NonMerkTreeMeta::DenseTree { height, .. } => {
                TreeType::DenseAppendOnlyFixedSizeTree(*height)
            }
        }
    }

    /// Constructs an `Element` from this metadata with the given flags.
    pub fn to_element(&self, flags: Option<ElementFlags>) -> Element {
        match self {
            NonMerkTreeMeta::CommitmentTree {
                total_count,
                chunk_power,
            } => Element::new_commitment_tree(*total_count, *chunk_power, flags),
            NonMerkTreeMeta::MmrTree { mmr_size } => Element::new_mmr_tree(*mmr_size, flags),
            NonMerkTreeMeta::BulkAppendTree {
                total_count,
                chunk_power,
            } => Element::new_bulk_append_tree(*total_count, *chunk_power, flags),
            NonMerkTreeMeta::DenseTree { count, height } => {
                Element::new_dense_tree(*count, *height, flags)
            }
        }
    }

    /// Extracts the count field used in `execute_ops_on_path`.
    pub fn count(&self) -> u64 {
        match self {
            NonMerkTreeMeta::CommitmentTree { total_count, .. } => *total_count,
            NonMerkTreeMeta::MmrTree { mmr_size } => *mmr_size,
            NonMerkTreeMeta::BulkAppendTree { total_count, .. } => *total_count,
            NonMerkTreeMeta::DenseTree { count, .. } => *count as u64,
        }
    }
}

/// Operations for batch processing.
///
/// User-facing variants: `InsertWithKnownToNotAlreadyExist`, `InsertIfNotExists`,
/// `InsertOrReplace`, `Replace`, `Patch`, `RefreshReference`, `Delete`,
/// `DeleteTree`, `CommitmentTreeInsert`, `MmrTreeAppend`, `BulkAppend`,
/// `DenseTreeInsert`.
///
/// Internal variants (`ReplaceTreeRootKey`, `InsertTreeWithRootHash`,
/// `ReplaceNonMerkTreeRoot`, `InsertNonMerkTree`) are marked
/// `#[non_exhaustive]` so they **cannot be constructed by external crates**.
/// They are produced solely by batch propagation / preprocessing within
/// this crate.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum GroveOp {
    /// **Internal only — do not construct directly.**
    /// Replace tree root key for standard Merk trees.
    ///
    /// Used by propagation to update an existing Merk tree's root hash
    /// and aggregate data. For non-Merk trees, see `ReplaceNonMerkTreeRoot`.
    ///
    /// This variant is `#[non_exhaustive]` and cannot be constructed outside
    /// of this crate.
    #[non_exhaustive]
    ReplaceTreeRootKey {
        /// Hash
        hash: [u8; 32],
        /// Root key
        root_key: Option<Vec<u8>>,
        /// Aggregate data
        aggregate_data: AggregateData,
    },
    /// Inserts an element that the caller knows does not yet exist.
    /// This is a performance optimization hint — no existence check is
    /// performed. The caller asserts the key is new.
    InsertWithKnownToNotAlreadyExist {
        /// Element
        element: Element,
    },
    /// Inserts an element only if the key does not already exist.
    /// An existence check is performed; if the key is found the behaviour
    /// depends on `error_if_exists`:
    /// - `true`: the operation is rejected with an error (default).
    /// - `false`: the insert is silently skipped.
    InsertIfNotExists {
        /// Element
        element: Element,
        /// If true, return an error when the key already exists.
        /// If false, silently skip the insert.
        error_if_exists: bool,
    },
    /// Inserts or Replaces an element
    InsertOrReplace {
        /// Element
        element: Element,
    },
    /// Replace
    Replace {
        /// Element
        element: Element,
    },
    /// Patch
    Patch {
        /// Element
        element: Element,
        /// Byte change
        change_in_bytes: i32,
    },
    /// **Internal only — do not construct directly.**
    /// Insert tree with root hash for standard Merk trees.
    ///
    /// Created during batch propagation from an `InsertOrReplace`/`InsertWithKnownToNotAlreadyExist`/`InsertIfNotExists`
    /// occupied entry when a child subtree's root hash is propagated upward.
    /// For non-Merk trees, see `InsertNonMerkTree`.
    ///
    /// This variant is `#[non_exhaustive]` and cannot be constructed outside
    /// of this crate.
    #[non_exhaustive]
    InsertTreeWithRootHash {
        /// Hash
        hash: [u8; 32],
        /// Root key
        root_key: Option<Vec<u8>>,
        /// Flags
        flags: Option<ElementFlags>,
        /// Aggregate Data such as sum
        aggregate_data: AggregateData,
        /// True if the original element was wrapped in `Element::NonCounted`.
        /// Set during propagation; on execution the reconstructed tree
        /// element is re-wrapped so the on-disk bytes preserve the wrapper
        /// and the parent count tree's aggregate excludes the subtree.
        non_counted: bool,
        /// True if the original element was wrapped in `Element::NotSummed`.
        /// Set during propagation; on execution the reconstructed
        /// sum-tree element is re-wrapped so the on-disk bytes preserve
        /// the wrapper and the parent sum tree's running sum excludes
        /// the subtree.
        not_summed: bool,
        /// True if the original element was wrapped in
        /// `Element::NotCountedOrSummed`. Set during propagation; on
        /// execution the reconstructed sum-bearing-tree element is
        /// re-wrapped so the on-disk bytes preserve the wrapper, and the
        /// parent's running count AND sum both exclude the subtree.
        not_counted_or_summed: bool,
    },
    /// **Internal only — do not construct directly.**
    /// Replace root hash for a non-Merk tree (CommitmentTree, MmrTree,
    /// BulkAppendTree, DenseTree). Produced by preprocessing functions.
    ///
    /// This variant is `#[non_exhaustive]` and cannot be constructed outside
    /// of this crate.
    #[non_exhaustive]
    ReplaceNonMerkTreeRoot {
        /// New root hash (sinsemilla root, MMR root, state root, dense root).
        hash: [u8; 32],
        /// Tree-type-specific metadata (count, chunk_power, height, etc.).
        meta: NonMerkTreeMeta,
    },
    /// **Internal only — do not construct directly.**
    /// Insert a non-Merk tree with root hash during propagation.
    ///
    /// Created when propagation encounters an occupied entry that is a
    /// non-Merk tree element (CommitmentTree, MmrTree, BulkAppendTree,
    /// DenseTree).
    ///
    /// This variant is `#[non_exhaustive]` and cannot be constructed outside
    /// of this crate.
    #[non_exhaustive]
    InsertNonMerkTree {
        /// Hash
        hash: [u8; 32],
        /// Root key
        root_key: Option<Vec<u8>>,
        /// Flags
        flags: Option<ElementFlags>,
        /// Aggregate data (always NoAggregateData for non-Merk trees)
        aggregate_data: AggregateData,
        /// Tree-type-specific metadata.
        meta: NonMerkTreeMeta,
        /// True if the original element was wrapped in `Element::NonCounted`.
        /// On execution the reconstructed element is re-wrapped to preserve
        /// the wrapper byte on disk.
        non_counted: bool,
    },
    /// **Internal only — do not construct directly.**
    /// Replace both primary and secondary root keys for an aggregate-
    /// indexed tree element (e.g. `CountIndexedTree`, and the planned
    /// `SumIndexedTree` / other aggregate-indexed shapes) after batch ops
    /// mutated its primary Merk. Carries both child Merks' new
    /// `(root_hash, root_key)` plus the primary's aggregate value; the
    /// parent merk node uses these in `combine_hash_three` (H1-A) to
    /// recompute its value_hash via
    /// `Op::ReplaceLayeredCountIndexedReference` (and its future
    /// aggregate-shape siblings).
    ///
    /// The op is aggregate-agnostic: `primary_aggregate_data` is an
    /// `AggregateData` enum, so the same propagation op works for Count,
    /// ProvableCount, and any future Sum / BigSum / ProvableSum / etc.
    /// secondary-indexed variants. Today this is produced only by
    /// `execute_ops_on_path` when a level's path resolves to a
    /// CountIndexedTree primary; consumed at the parent level to update
    /// the cidx (or future aggregate-cidx) element bytes consistently
    /// with both child Merks.
    ///
    /// This variant is `#[non_exhaustive]` and cannot be constructed outside
    /// of this crate.
    #[non_exhaustive]
    ReplaceAggregateIndexedTreeRootKeys {
        /// Primary Merk's new root hash.
        primary_hash: [u8; 32],
        /// Primary Merk's new root key.
        primary_root_key: Option<Vec<u8>>,
        /// Primary Merk's new aggregate, whichever the indexed variant
        /// carries — count for PCIT, sum for PSIT, count-and-sum for PCPSIT.
        primary_aggregate_data: AggregateData,
        /// Every configured axis's new state, as
        /// `(axis_tag, root_hash, root_key)`, in the element's canonical axis
        /// order. One entry for the single-axis variants (PCIT, PSIT); up to
        /// three for a PCPSIT indexing count, sum and avg.
        ///
        /// The consumer rebuilds the element from these and derives the second
        /// input to the H1-A `combine_hash_three`: the single axis's root hash
        /// directly for PCIT/PSIT, or `axes_digest` over all of them for
        /// PCPSIT.
        axes: Vec<(u8, [u8; 32], Option<Vec<u8>>)>,
    },
    /// Insert-side counterpart of [`Self::ReplaceAggregateIndexedTreeRootKeys`]
    /// for an indexed primary CREATED in this same batch: there is no stored
    /// element to read during propagation, so the op carries the caller's
    /// element itself (root keys unset — enforced by the rootless-aggregate
    /// rule) alongside the computed primary and per-axis secondary state.
    /// Internal: emitted by the bubble-up when a deeper level finishes under
    /// a freshly-inserted indexed element; rejected in user-supplied batches.
    InsertAggregateIndexedTreeRootKeys {
        /// The freshly created indexed element as the caller supplied it.
        element: Element,
        /// Root hash of the indexed primary Merk after this batch's ops.
        primary_hash: CryptoHash,
        /// Root key of the indexed primary Merk after this batch's ops.
        primary_root_key: Option<Vec<u8>>,
        /// The primary's aggregate data after this batch's ops.
        primary_aggregate_data: AggregateData,
        /// Post-mirror per-axis state, in the element's canonical axis
        /// order: `(axis_tag, secondary_root_hash, secondary_root_key)`.
        axes: Vec<(u8, CryptoHash, Option<Vec<u8>>)>,
    },
    /// Refresh a reference. The full op shape (which on-disk variant,
    /// trust mode, sum-update behavior) lives in `mode` — see
    /// [`RefreshReferenceMode`] for the per-variant contract.
    ///
    /// `non_counted` declares whether the rebuilt element is wrapped
    /// in `NonCounted` (suppresses the count contribution in a
    /// count-bearing parent). Under trusted variants it is written at
    /// face value; under untrusted variants it is cross-checked
    /// against on-disk and a mismatch is rejected (a silent wrapper
    /// drop would corrupt the parent's count aggregate).
    ///
    /// Under trusted variants, the apply path writes the op's payload
    /// (`reference_path_type`, `max_reference_hop`, `flags`, and the
    /// mode's contained sum if any) verbatim. If on-disk has a
    /// different variant or wrapper, it gets silently coerced — the
    /// parent's count/sum aggregate may become inconsistent, caller's
    /// responsibility.
    ///
    /// Under untrusted variants, the apply path reads on-disk,
    /// cross-checks variant and wrapper, and writes back with the
    /// on-disk path / max-hop / flags / wrapper. For
    /// `SumItemReferenceUntrustedValueUpdate(v)` the op's `v`
    /// overrides the on-disk sum; the other untrusted variants write
    /// the on-disk element back verbatim. Op fields
    /// `reference_path_type`, `max_reference_hop`, and `flags` are
    /// used only for the average / worst case cost models in
    /// untrusted mode.
    RefreshReference {
        /// The reference path written under trusted variants. Under
        /// untrusted variants the on-disk path is preserved; this
        /// field is consulted only for the cost estimate.
        reference_path_type: ReferencePathType,
        /// Max hops written under trusted variants. Same trust-mode
        /// semantics as `reference_path_type`.
        max_reference_hop: MaxReferenceHop,
        /// Fully specifies the op: on-disk variant, trust mode, and
        /// sum-update behavior. See [`RefreshReferenceMode`].
        mode: RefreshReferenceMode,
        /// Element flags written under trusted variants. Same
        /// trust-mode semantics as `reference_path_type`.
        flags: Option<ElementFlags>,
        /// Declares whether the rebuilt element is wrapped in
        /// `NonCounted`. Trusted variants write at face value;
        /// untrusted cross-check against on-disk.
        non_counted: bool,
    },
    /// Delete
    Delete,
    /// Delete tree
    DeleteTree(TreeType, SubelementsDeletionBehavior),
    /// Insert a note commitment + payload into a CommitmentTree
    CommitmentTreeInsert {
        /// 32-byte note commitment (must be a valid Pallas field element)
        cmx: [u8; 32],
        /// 32-byte nullifier (rho) of the spent note
        rho: [u8; 32],
        /// 32-byte value commitment (cv_net) of the note. Stored unencrypted;
        /// required for outgoing-note (OVK) recovery.
        cv_net: [u8; 32],
        /// Payload data (typically encrypted note)
        payload: Vec<u8>,
    },
    /// Append a value to an MmrTree
    MmrTreeAppend {
        /// Value to append (will be Blake3-hashed for the leaf)
        value: Vec<u8>,
    },
    /// Append a value to a BulkAppendTree
    BulkAppend {
        /// Value to append
        value: Vec<u8>,
    },
    /// Insert a value into a DenseAppendOnlyFixedSizeTree
    DenseTreeInsert {
        /// Value to insert
        value: Vec<u8>,
    },
}

impl GroveOp {
    /// Stable per-variant sort tag used by [`Ord::cmp`] and exposed
    /// `pub(crate)` so tests can pin the exact value (not just relative
    /// ordering). Changing any of these numbers is observable to
    /// downstream sort-order assumptions in the batch pipeline; the
    /// associated tests are intentionally strict.
    pub(crate) fn to_u8(&self) -> u8 {
        match self {
            GroveOp::DeleteTree(..) => 0,
            // 1 used to be used for the DeleteSumTree
            GroveOp::Delete => 2,
            GroveOp::InsertTreeWithRootHash { .. } => 3,
            GroveOp::ReplaceTreeRootKey { .. } => 4,
            GroveOp::RefreshReference { .. } => 5,
            GroveOp::Replace { .. } => 6,
            GroveOp::Patch { .. } => 7,
            GroveOp::InsertOrReplace { .. } => 8,
            GroveOp::InsertWithKnownToNotAlreadyExist { .. } => 9,
            GroveOp::InsertIfNotExists { .. } => 10,
            GroveOp::CommitmentTreeInsert { .. } => 11,
            GroveOp::MmrTreeAppend { .. } => 12,
            GroveOp::BulkAppend { .. } => 13,
            GroveOp::DenseTreeInsert { .. } => 14,
            GroveOp::ReplaceNonMerkTreeRoot { .. } => 15,
            GroveOp::InsertNonMerkTree { .. } => 16,
            GroveOp::ReplaceAggregateIndexedTreeRootKeys { .. } => 17,
            GroveOp::InsertAggregateIndexedTreeRootKeys { .. } => 18,
        }
    }

    /// True iff this op, when applied at a cidx primary's path, can
    /// change the `count_value` (or absence) of the element at the
    /// op's key — and therefore requires the cidx primary's secondary
    /// mirror to be updated for that key.
    ///
    /// Used by `execute_ops_on_path`'s pre-state capture pass: only
    /// ops that may change a key's count_value are read from disk
    /// before apply, so the post-apply mirror knows the (old, new)
    /// count delta.
    ///
    /// # Single source of truth — keep in sync with `GroveOp` variants
    ///
    /// This method uses an **exhaustive `match`** with no wildcard
    /// arm. Adding a new `GroveOp` variant forces an explicit decision
    /// here; the compiler will refuse to compile until the variant is
    /// classified. This is the guard that prevents the nested-cidx
    /// mirror-bug class (commit a8bb34fb) from recurring: that bug
    /// existed because the original inline `matches!()` was a
    /// non-exhaustive check, so the newly added
    /// `ReplaceAggregateIndexedTreeRootKeys` variant silently fell through
    /// to "doesn't mutate" and the outer's secondary stayed stale.
    pub(crate) fn can_mutate_child_count(&self) -> bool {
        match self {
            // User-facing leaf-level mutations: insert/replace/patch
            // all set a new (or unchanged) count_value on the affected
            // key; delete removes it. All require secondary mirror.
            GroveOp::InsertWithKnownToNotAlreadyExist { .. }
            | GroveOp::InsertIfNotExists { .. }
            | GroveOp::InsertOrReplace { .. }
            | GroveOp::Replace { .. }
            | GroveOp::Patch { .. }
            | GroveOp::Delete
            | GroveOp::DeleteTree(..)
            | GroveOp::RefreshReference { .. } => true,

            // Bubble-up ops emitted by propagation. Each updates the
            // child element's bytes at the parent level — for
            // count-bearing trees this changes the aggregated
            // count_value, which is the secondary's sort key. Without
            // this arm, nested-cidx bubble-up silently leaves the
            // outer's secondary stale (commit a8bb34fb).
            GroveOp::ReplaceTreeRootKey { .. }
            | GroveOp::InsertTreeWithRootHash { .. }
            | GroveOp::ReplaceNonMerkTreeRoot { .. }
            | GroveOp::InsertNonMerkTree { .. }
            | GroveOp::ReplaceAggregateIndexedTreeRootKeys { .. }
            | GroveOp::InsertAggregateIndexedTreeRootKeys { .. } => true,

            // Non-Merk-tree leaf inserts (commitment/MMR/bulk-append/
            // dense) don't change count_value for their own entries
            // because these trees use non-Merk storage and don't
            // contribute counts to a parent cidx the same way. Their
            // own internal aggregation is handled by the tree-specific
            // propagation; they do NOT need a per-entry cidx secondary
            // mirror at the leaf level.
            GroveOp::CommitmentTreeInsert { .. }
            | GroveOp::MmrTreeAppend { .. }
            | GroveOp::BulkAppend { .. }
            | GroveOp::DenseTreeInsert { .. } => false,
        }
    }
}

impl PartialOrd for GroveOp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for GroveOp {
    fn cmp(&self, other: &Self) -> Ordering {
        self.to_u8().cmp(&other.to_u8())
    }
}

/// Known keys path
#[derive(Eq, Clone, Debug)]
pub struct KnownKeysPath(Vec<Vec<u8>>);

impl Hash for KnownKeysPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

impl PartialEq for KnownKeysPath {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl PartialEq<KeyInfoPath> for KnownKeysPath {
    fn eq(&self, other: &KeyInfoPath) -> bool {
        self.0 == other.to_path_refs()
    }
}

impl PartialEq<Vec<Vec<u8>>> for KnownKeysPath {
    fn eq(&self, other: &Vec<Vec<u8>>) -> bool {
        self.0 == other.as_slice()
    }
}

/// Key info path
#[derive(PartialOrd, Ord, Eq, Clone, Debug, Default)]
pub struct KeyInfoPath(pub Vec<KeyInfo>);

impl Hash for KeyInfoPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

impl PartialEq for KeyInfoPath {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl PartialEq<Vec<Vec<u8>>> for KeyInfoPath {
    fn eq(&self, other: &Vec<Vec<u8>>) -> bool {
        if self.len() != other.len() as u32 {
            return false;
        }
        self.0.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl PartialEq<Vec<&[u8]>> for KeyInfoPath {
    fn eq(&self, other: &Vec<&[u8]>) -> bool {
        if self.len() != other.len() as u32 {
            return false;
        }
        self.0.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl<const N: usize> PartialEq<[&[u8]; N]> for KeyInfoPath {
    fn eq(&self, other: &[&[u8]; N]) -> bool {
        if self.len() != N as u32 {
            return false;
        }
        self.0.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl Visualize for KeyInfoPath {
    fn visualize<W: std::io::Write>(&self, mut drawer: Drawer<W>) -> std::io::Result<Drawer<W>> {
        drawer.write(b"path: ")?;
        let mut path_out = Vec::new();
        let mut path_drawer = Drawer::new(&mut path_out);
        for k in &self.0 {
            path_drawer = k.visualize(path_drawer)?;
            path_drawer.write(b" ")?;
        }
        drawer.write(path_out.as_slice())?;
        Ok(drawer)
    }
}

impl KeyInfoPath {
    /// From a vector
    pub fn from_vec(vec: Vec<KeyInfo>) -> Self {
        KeyInfoPath(vec)
    }

    /// From a known path
    pub fn from_known_path<'p, P>(path: P) -> Self
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        KeyInfoPath(path.into_iter().map(|k| KnownKey(k.to_vec())).collect())
    }

    /// From a known owned path
    pub fn from_known_owned_path<P>(path: P) -> Self
    where
        P: IntoIterator<Item = Vec<u8>>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        KeyInfoPath(path.into_iter().map(KnownKey).collect())
    }

    /// To a path and consume
    pub fn to_path_consume(self) -> Vec<Vec<u8>> {
        self.0.into_iter().map(|k| k.get_key()).collect()
    }

    /// To a path
    pub fn to_path(&self) -> Vec<Vec<u8>> {
        self.0.iter().map(|k| k.get_key_clone()).collect()
    }

    /// Compare with a byte-vector path without allocating
    pub fn eq_path_vec(&self, other: &[Vec<u8>]) -> bool {
        self.0.len() == other.len()
            && self
                .0
                .iter()
                .zip(other.iter())
                .all(|(a, b)| a.as_slice() == b.as_slice())
    }

    /// To a path of refs
    pub fn to_path_refs(&self) -> Vec<&[u8]> {
        self.0.iter().map(|k| k.as_slice()).collect()
    }

    /// Return the last and all the other elements split
    pub fn split_last(&self) -> Option<(&KeyInfo, &[KeyInfo])> {
        self.0.split_last()
    }

    /// Return the last element
    pub fn last(&self) -> Option<&KeyInfo> {
        self.0.last()
    }

    /// As vector
    pub fn as_vec(&self) -> &Vec<KeyInfo> {
        &self.0
    }

    /// Check if it's empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Return length
    pub fn len(&self) -> u32 {
        self.0.len() as u32
    }

    /// Push a KeyInfo to self
    pub fn push(&mut self, k: KeyInfo) {
        self.0.push(k);
    }

    /// Iterate KeyInfo
    pub fn iterator(&self) -> Iter<'_, KeyInfo> {
        self.0.iter()
    }

    /// Into iterator
    pub fn into_iterator(self) -> IntoIter<KeyInfo> {
        self.0.into_iter()
    }
}

/// Batch operation
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct QualifiedGroveDbOp {
    /// Path to a subtree - subject to an operation
    pub path: KeyInfoPath,
    /// Key of an element in the subtree.
    /// `None` for append-only tree ops (CommitmentTreeInsert, MmrTreeAppend,
    /// BulkAppend, DenseTreeInsert) where the tree key is the last segment
    /// of `path` instead.
    pub key: Option<KeyInfo>,
    /// Operation to perform on the key
    pub op: GroveOp,
}

impl fmt::Debug for QualifiedGroveDbOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut path_out = Vec::new();
        let path_drawer = Drawer::new(&mut path_out);
        self.path.visualize(path_drawer).unwrap();
        let key_display = if let Some(ref key) = self.key {
            let mut key_out = Vec::new();
            let key_drawer = Drawer::new(&mut key_out);
            key.visualize(key_drawer).unwrap();
            String::from_utf8_lossy(&key_out).into_owned()
        } else {
            "(keyless)".to_string()
        };

        let op_dbg = match &self.op {
            GroveOp::InsertOrReplace { element } => format!("Insert Or Replace {:?}", element),
            GroveOp::InsertWithKnownToNotAlreadyExist { element } => {
                format!("Insert With Known To Not Already Exist {:?}", element)
            }
            GroveOp::InsertIfNotExists {
                element,
                error_if_exists,
            } => {
                if *error_if_exists {
                    format!("Insert If Not Exists (error on existing) {:?}", element)
                } else {
                    format!("Insert If Not Exists (skip on existing) {:?}", element)
                }
            }
            GroveOp::Replace { element } => format!("Replace {:?}", element),
            GroveOp::Patch { element, .. } => format!("Patch {:?}", element),
            GroveOp::RefreshReference {
                reference_path_type,
                max_reference_hop,
                mode,
                non_counted,
                ..
            } => {
                let (label, mode_render) = match mode {
                    RefreshReferenceMode::PlainReferenceTrusted => {
                        ("Refresh Reference", "PlainReferenceTrusted".to_string())
                    }
                    RefreshReferenceMode::PlainReferenceUntrusted => {
                        ("Refresh Reference", "PlainReferenceUntrusted".to_string())
                    }
                    RefreshReferenceMode::SumItemReferenceTrusted(sum) => (
                        "Refresh Reference With Sum Item",
                        format!("SumItemReferenceTrusted({sum})"),
                    ),
                    RefreshReferenceMode::SumItemReferenceUntrustedValueUpdate(sum) => (
                        "Refresh Reference With Sum Item",
                        format!("SumItemReferenceUntrustedValueUpdate({sum})"),
                    ),
                    RefreshReferenceMode::SumItemReferenceUntrustedNoValueUpdate => (
                        "Refresh Reference With Sum Item",
                        "SumItemReferenceUntrustedNoValueUpdate".to_string(),
                    ),
                };
                format!(
                    "{label}: path {:?}, max_hop {:?}, mode {}, non_counted {} ",
                    reference_path_type, max_reference_hop, mode_render, non_counted,
                )
            }
            GroveOp::Delete => "Delete".to_string(),
            GroveOp::DeleteTree(tree_type, check) => {
                format!("Delete Tree {} ({:?})", tree_type, check)
            }
            GroveOp::ReplaceTreeRootKey { .. } => "Replace Tree Hash and Root Key".to_string(),
            GroveOp::InsertTreeWithRootHash { .. } => "Insert Tree Hash and Root Key".to_string(),
            GroveOp::ReplaceNonMerkTreeRoot { meta, .. } => {
                format!("Replace Non-Merk Tree Root ({:?})", meta)
            }
            GroveOp::InsertNonMerkTree { meta, .. } => {
                format!("Insert Non-Merk Tree ({:?})", meta)
            }
            GroveOp::CommitmentTreeInsert { cmx, rho, .. } => {
                format!(
                    "Commitment Tree Insert (cmx={}, rho={})",
                    hex::encode(&cmx[..4]),
                    hex::encode(&rho[..4])
                )
            }
            GroveOp::MmrTreeAppend { .. } => "MMR Tree Append".to_string(),
            GroveOp::BulkAppend { .. } => "Bulk Append".to_string(),
            GroveOp::DenseTreeInsert { .. } => "Dense Tree Insert".to_string(),
            GroveOp::ReplaceAggregateIndexedTreeRootKeys { .. } => {
                "Replace CountIndexedTree primary+secondary roots".to_string()
            }
            GroveOp::InsertAggregateIndexedTreeRootKeys { .. } => {
                "Insert fresh indexed tree with primary+secondary roots".to_string()
            }
        };

        f.debug_struct("GroveDbOp")
            .field("path", &String::from_utf8_lossy(&path_out))
            .field("key", &key_display)
            .field("op", &op_dbg)
            .finish()
    }
}

impl QualifiedGroveDbOp {
    /// An insert op using a known owned path and known key.
    /// The caller asserts the key is new — no existence check is performed.
    /// This is a performance optimization hint.
    pub fn insert_only_known_to_not_already_exist_op(
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        element: Element,
    ) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::InsertWithKnownToNotAlreadyExist { element },
        }
    }

    #[deprecated(
        note = "use insert_only_known_to_not_already_exist_op or insert_if_not_exists_op instead"
    )]
    /// Deprecated: use `insert_only_known_to_not_already_exist_op` instead.
    pub fn insert_only_op(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        Self::insert_only_known_to_not_already_exist_op(path, key, element)
    }

    /// An insert op that checks if the key already exists and rejects
    /// the operation if it does, enforcing uniqueness.
    pub fn insert_if_not_exists_op(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::InsertIfNotExists {
                element,
                error_if_exists: true,
            },
        }
    }

    /// An insert op that checks if the key already exists and silently
    /// skips the insert when it does (no error).
    pub fn insert_if_not_exists_or_skip_op(
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        element: Element,
    ) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::InsertIfNotExists {
                element,
                error_if_exists: false,
            },
        }
    }

    /// An insert op using a known owned path and known key
    pub fn insert_or_replace_op(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::InsertOrReplace { element },
        }
    }

    /// An insert op
    pub fn insert_estimated_op(path: KeyInfoPath, key: KeyInfo, element: Element) -> Self {
        Self {
            path,
            key: Some(key),
            op: GroveOp::InsertOrReplace { element },
        }
    }

    /// A replace op using a known owned path and known key
    pub fn replace_op(path: Vec<Vec<u8>>, key: Vec<u8>, element: Element) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::Replace { element },
        }
    }

    /// A replace op
    pub fn replace_estimated_op(path: KeyInfoPath, key: KeyInfo, element: Element) -> Self {
        Self {
            path,
            key: Some(key),
            op: GroveOp::Replace { element },
        }
    }

    /// A patch op using a known owned path and known key
    pub fn patch_op(
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        element: Element,
        change_in_bytes: i32,
    ) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::Patch {
                element,
                change_in_bytes,
            },
        }
    }

    /// A patch op
    pub fn patch_estimated_op(
        path: KeyInfoPath,
        key: KeyInfo,
        element: Element,
        change_in_bytes: i32,
    ) -> Self {
        Self {
            path,
            key: Some(key),
            op: GroveOp::Patch {
                element,
                change_in_bytes,
            },
        }
    }

    /// Construct a [`GroveOp::RefreshReference`] op for a plain
    /// [`Element::Reference`] (no carried sum-item). Thin wrapper
    /// that builds the unified `GroveOp::RefreshReference` with
    /// `mode = PlainReferenceTrusted` or `PlainReferenceUntrusted`
    /// based on `trust_refresh_reference`.
    ///
    /// `non_counted` declares whether the rebuilt element is wrapped
    /// in `Element::NonCounted` (suppresses the count contribution
    /// in a count-bearing parent). Under trusted mode it's written
    /// at face value; under untrusted mode it's cross-checked
    /// against the on-disk wrapper and a mismatch is rejected.
    ///
    /// See the [`RefreshReferenceMode`] doc for the trust-mode
    /// contract.
    ///
    /// For sum-item-carrying references, use
    /// [`Self::refresh_reference_with_sum_item_op`] (override the
    /// sum) or [`Self::refresh_reference_with_sum_item_keep_sum_op`]
    /// (preserve the on-disk sum).
    pub fn refresh_reference_op(
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        reference_path_type: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
        flags: Option<ElementFlags>,
        non_counted: bool,
        trust_refresh_reference: bool,
    ) -> Self {
        let mode = if trust_refresh_reference {
            RefreshReferenceMode::PlainReferenceTrusted
        } else {
            RefreshReferenceMode::PlainReferenceUntrusted
        };
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::RefreshReference {
                reference_path_type,
                max_reference_hop,
                mode,
                flags,
                non_counted,
            },
        }
    }

    /// Construct a [`GroveOp::RefreshReference`] op for an
    /// [`Element::ReferenceWithSumItem`] that **overrides** the
    /// carried sum with the given `sum_value`. Thin wrapper that
    /// builds the unified `GroveOp::RefreshReference` with `mode =
    /// SumItemReferenceTrusted(sum_value)` or
    /// `SumItemReferenceUntrustedValueUpdate(sum_value)` based on
    /// `trust_refresh_reference`.
    ///
    /// See the [`RefreshReferenceMode`] doc for the trust-mode
    /// contract.
    ///
    /// To refresh a `ReferenceWithSumItem`'s `value_hash` *without*
    /// changing its carried sum, use
    /// [`Self::refresh_reference_with_sum_item_keep_sum_op`].
    pub fn refresh_reference_with_sum_item_op(
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        reference_path_type: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
        sum_value: SumValue,
        flags: Option<ElementFlags>,
        non_counted: bool,
        trust_refresh_reference: bool,
    ) -> Self {
        let mode = if trust_refresh_reference {
            RefreshReferenceMode::SumItemReferenceTrusted(sum_value)
        } else {
            RefreshReferenceMode::SumItemReferenceUntrustedValueUpdate(sum_value)
        };
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::RefreshReference {
                reference_path_type,
                max_reference_hop,
                mode,
                flags,
                non_counted,
            },
        }
    }

    /// Construct a [`GroveOp::RefreshReference`] op for an
    /// [`Element::ReferenceWithSumItem`] that **preserves** the
    /// on-disk carried sum (no value update). Thin wrapper that
    /// builds the unified `GroveOp::RefreshReference` with `mode =
    /// SumItemReferenceUntrustedNoValueUpdate`.
    ///
    /// This op is **always untrusted** — under trusted mode the
    /// apply path would have no sum to write without reading disk,
    /// so the type system makes that combination unrepresentable
    /// (there is no `SumItemReferenceTrustedNoValueUpdate` variant).
    /// The on-disk element must be a `ReferenceWithSumItem`
    /// (verified at apply); a plain `Reference` or any other variant
    /// is rejected.
    ///
    /// Use this for the "I want to refresh my value_hash, leaving
    /// the carried sum alone" case — caller doesn't need to know the
    /// current sum.
    pub fn refresh_reference_with_sum_item_keep_sum_op(
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        reference_path_type: ReferencePathType,
        max_reference_hop: MaxReferenceHop,
        flags: Option<ElementFlags>,
        non_counted: bool,
    ) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::RefreshReference {
                reference_path_type,
                max_reference_hop,
                mode: RefreshReferenceMode::SumItemReferenceUntrustedNoValueUpdate,
                flags,
                non_counted,
            },
        }
    }

    /// A delete op using a known owned path and known key
    pub fn delete_op(path: Vec<Vec<u8>>, key: Vec<u8>) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::Delete,
        }
    }

    /// A delete tree op using a known owned path and known key
    pub fn delete_tree_op(
        path: Vec<Vec<u8>>,
        key: Vec<u8>,
        tree_type: TreeType,
        subelements_deletion_behavior: SubelementsDeletionBehavior,
    ) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: Some(KnownKey(key)),
            op: GroveOp::DeleteTree(tree_type, subelements_deletion_behavior),
        }
    }

    /// A delete op
    pub fn delete_estimated_op(path: KeyInfoPath, key: KeyInfo) -> Self {
        Self {
            path,
            key: Some(key),
            op: GroveOp::Delete,
        }
    }

    /// A delete tree op
    pub fn delete_estimated_tree_op(
        path: KeyInfoPath,
        key: KeyInfo,
        tree_type: TreeType,
        subelements_deletion_behavior: SubelementsDeletionBehavior,
    ) -> Self {
        Self {
            path,
            key: Some(key),
            op: GroveOp::DeleteTree(tree_type, subelements_deletion_behavior),
        }
    }

    /// A commitment tree insert op. `path` includes the tree key as its last
    /// segment (e.g. `vec![b"pool".to_vec()]` for a tree at key `b"pool"` in
    /// the root subtree).
    pub fn commitment_tree_insert_op(
        path: Vec<Vec<u8>>,
        cmx: [u8; 32],
        rho: [u8; 32],
        cv_net: [u8; 32],
        payload: Vec<u8>,
    ) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: None,
            op: GroveOp::CommitmentTreeInsert {
                cmx,
                rho,
                cv_net,
                payload,
            },
        }
    }

    /// A typed commitment tree insert op. Serializes the ciphertext and
    /// delegates to
    /// [`commitment_tree_insert_op`](Self::commitment_tree_insert_op).
    pub fn commitment_tree_insert_op_typed<M: grovedb_commitment_tree::MemoSize>(
        path: Vec<Vec<u8>>,
        cmx: [u8; 32],
        rho: [u8; 32],
        cv_net: [u8; 32],
        ciphertext: &grovedb_commitment_tree::TransmittedNoteCiphertext<M>,
    ) -> Self {
        let payload = grovedb_commitment_tree::serialize_ciphertext(ciphertext);
        Self::commitment_tree_insert_op(path, cmx, rho, cv_net, payload)
    }

    /// An MMR tree append op. `path` includes the tree key as its last segment.
    pub fn mmr_tree_append_op(path: Vec<Vec<u8>>, value: Vec<u8>) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: None,
            op: GroveOp::MmrTreeAppend { value },
        }
    }

    /// A bulk append op. `path` includes the tree key as its last segment.
    pub fn bulk_append_op(path: Vec<Vec<u8>>, value: Vec<u8>) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: None,
            op: GroveOp::BulkAppend { value },
        }
    }

    /// A dense tree insert op. `path` includes the tree key as its last
    /// segment.
    pub fn dense_tree_insert_op(path: Vec<Vec<u8>>, value: Vec<u8>) -> Self {
        let path = KeyInfoPath::from_known_owned_path(path);
        Self {
            path,
            key: None,
            op: GroveOp::DenseTreeInsert { value },
        }
    }

    /// Verify consistency of operations
    pub fn verify_consistency_of_operations(
        ops: &[QualifiedGroveDbOp],
    ) -> GroveDbOpConsistencyResults {
        // Reject internal-only ops that should never appear in user-submitted
        // batches. These are produced by preprocessing or propagation only.
        let internal_only_ops: Vec<(QualifiedGroveDbOp, u16)> = ops
            .iter()
            .filter(|op| {
                matches!(
                    op.op,
                    GroveOp::ReplaceTreeRootKey { .. }
                        | GroveOp::ReplaceNonMerkTreeRoot { .. }
                        | GroveOp::InsertTreeWithRootHash { .. }
                        | GroveOp::InsertNonMerkTree { .. }
                )
            })
            .map(|op| (op.clone(), 1))
            .collect();

        // operations should not have any duplicates — O(n) via HashMap
        let mut repeated_ops = internal_only_ops;
        {
            let mut op_counts: HashMap<&QualifiedGroveDbOp, u16> = HashMap::new();
            for op in ops.iter() {
                *op_counts.entry(op).or_insert(0) += 1;
            }
            for (op, count) in op_counts {
                if count > 1 {
                    repeated_ops.push((op.clone(), count));
                }
            }
        }

        // No double insert or delete of same key in same path — O(n) via HashMap.
        // Keyless ops (append-only tree ops) can't conflict — skip them.
        let mut same_path_key_ops = vec![];
        {
            let mut path_key_ops: HashMap<(&KeyInfoPath, &KeyInfo), Vec<&GroveOp>> = HashMap::new();
            for op in ops.iter() {
                if let Some(ref key) = op.key {
                    path_key_ops
                        .entry((&op.path, key))
                        .or_default()
                        .push(&op.op);
                }
            }
            for ((path, key), op_list) in path_key_ops {
                if op_list.len() > 1 {
                    same_path_key_ops.push((
                        path.clone(),
                        Some(key.clone()),
                        op_list.into_iter().cloned().collect(),
                    ));
                }
            }
        }

        let mut append_keyed_conflicts = vec![];
        {
            // Detect conflicts between keyless append ops and keyed ops
            // targeting the same tree element. Keyless ops encode the tree as
            // the last path segment; keyed ops use (parent_path, key).
            // Collect (parent_path, tree_key) for every keyless op.
            let mut keyless_tree_ids: HashMap<(Vec<Vec<u8>>, Vec<u8>), usize> = HashMap::new();
            for (i, op) in ops.iter().enumerate() {
                if op.key.is_none() {
                    let full = op.path.to_path();
                    if let Some((tree_key, parent)) = full.split_last() {
                        keyless_tree_ids
                            .entry((parent.to_vec(), tree_key.clone()))
                            .or_insert(i);
                    }
                }
            }
            // Check all keyed ops for conflicts.
            for op in ops.iter() {
                if let Some(ref key_info) = op.key {
                    let parent = op.path.to_path();
                    let key = key_info.get_key_clone();
                    if let Some(&idx) = keyless_tree_ids.get(&(parent, key)) {
                        append_keyed_conflicts.push((ops[idx].clone(), op.clone()));
                    }
                }
            }
        }

        // No inserts under a deleted path
        // Build a map of deleted_qualified_path -> indices of delete ops
        let mut deleted_path_to_op_indices: HashMap<KeyInfoPath, Vec<usize>> = HashMap::new();
        for (idx, op) in ops.iter().enumerate() {
            if matches!(op.op, GroveOp::Delete | GroveOp::DeleteTree(..)) {
                let Some(ref key) = op.key else {
                    continue;
                };
                let mut qualified_path = op.path.clone();
                qualified_path.push(key.clone());
                deleted_path_to_op_indices
                    .entry(qualified_path)
                    .or_default()
                    .push(idx);
            }
        }

        // For each insert, check if any prefix of its path is a deleted path
        let mut conflicts: HashMap<KeyInfoPath, Vec<usize>> = HashMap::new();
        for (idx, op) in ops.iter().enumerate() {
            match op.op {
                GroveOp::InsertWithKnownToNotAlreadyExist { .. }
                | GroveOp::InsertIfNotExists { .. }
                | GroveOp::InsertOrReplace { .. }
                | GroveOp::Replace { .. }
                | GroveOp::Patch { .. } => {}
                _ => continue,
            }
            for prefix_len in 1..=op.path.len() as usize {
                let prefix = KeyInfoPath(op.path.iterator().take(prefix_len).cloned().collect());
                if deleted_path_to_op_indices.contains_key(&prefix) {
                    conflicts.entry(prefix).or_default().push(idx);
                    break;
                }
            }
        }

        // Build output (clone only conflicting ops)
        let mut insert_ops_below_deleted_ops = Vec::new();
        for (deleted_path, insert_indices) in conflicts {
            let inserts: Vec<QualifiedGroveDbOp> =
                insert_indices.iter().map(|&i| ops[i].clone()).collect();
            for &del_idx in &deleted_path_to_op_indices[&deleted_path] {
                insert_ops_below_deleted_ops.push((ops[del_idx].clone(), inserts.clone()));
            }
        }

        GroveDbOpConsistencyResults {
            repeated_ops,
            same_path_key_ops,
            insert_ops_below_deleted_ops,
            append_keyed_conflicts,
        }
    }
}

/// Results of a consistency check on an operation batch
#[derive(Debug)]
pub struct GroveDbOpConsistencyResults {
    /// Repeated Ops, the second u16 element represents the count
    repeated_ops: Vec<(QualifiedGroveDbOp, u16)>,
    /// The same path key ops
    same_path_key_ops: Vec<(KeyInfoPath, Option<KeyInfo>, Vec<GroveOp>)>,
    /// This shows issues when we delete a tree but insert under the deleted
    /// tree Deleted ops are first, with inserts under them in a tree
    insert_ops_below_deleted_ops: Vec<(QualifiedGroveDbOp, Vec<QualifiedGroveDbOp>)>,
    /// Conflicts between keyless append ops and keyed ops targeting the same
    /// tree element. Tuple is (append_op, keyed_op).
    append_keyed_conflicts: Vec<(QualifiedGroveDbOp, QualifiedGroveDbOp)>,
}

impl GroveDbOpConsistencyResults {
    /// Check if results are empty
    pub fn is_empty(&self) -> bool {
        self.repeated_ops.is_empty()
            && self.same_path_key_ops.is_empty()
            && self.insert_ops_below_deleted_ops.is_empty()
            && self.append_keyed_conflicts.is_empty()
    }
}

/// Cache for Merk trees by their paths.
struct TreeCacheMerkByPath<S, F, F2> {
    merks: HashMap<Vec<Vec<u8>>, Merk<S>>,
    get_merk_fn: F,
    /// Opens EVERY configured axis secondary for an indexed primary, given
    /// the primary's path. Used when ops mutate an indexed primary so the
    /// mirrors stay in sync at apply time. The closure reads the indexed
    /// element from the parent merk to learn the configured axes and each
    /// axis's current root key, then opens one secondary per axis at its
    /// derived prefix. PCIT/PSIT yield one; a PCPSIT indexing count+sum+avg
    /// yields three.
    get_secondary_merks_fn: F2,
    /// Per-indexed-primary captured secondary state after apply, keyed by the
    /// primary's path, as `(axis_tag, root_hash, root_key)` in the element's
    /// canonical axis order. Populated by `execute_ops_on_path` when the
    /// path's merk is an indexed primary; consumed by the bubble-up code so a
    /// `ReplaceAggregateIndexedTreeRootKeys` op can be emitted on the parent
    /// level carrying the primary plus every axis's state — one entry for
    /// PCIT/PSIT, up to three for PCPSIT.
    indexed_secondary_after_apply: HashMap<Vec<Vec<u8>>, Vec<(u8, CryptoHash, Option<Vec<u8>>)>>,
    /// Cidx primary paths whose old storage (primary subtree + secondary
    /// namespace) must be cleaned up at apply_batch's post-apply phase
    /// because an InsertOrReplace / Replace / Patch op replaced the
    /// cidx with either a non-cidx element OR an empty cidx. These are
    /// the SAFE-SUBSET overwrites — cidx → non-empty cidx is rejected
    /// at validation time because the storage-pointer semantics are
    /// ambiguous. See the cidx-overwrite handling in
    /// `execute_ops_on_path`.
    cidx_overwrite_cleanup_paths: Vec<Vec<Vec<u8>>>,
    /// Qualified path → ACTUAL stored tree type of every `DeleteTree` target
    /// this apply deleted, captured (V4+ only) from the old element bytes the
    /// merk delete surfaces through the old-value observer. Consumed by
    /// `apply_batch`'s post-apply phase to select cleanup namespaces from
    /// what was really stored rather than what the op declared.
    deleted_tree_actual_types: Vec<(Vec<Vec<u8>>, TreeType)>,
}

impl<S, F, F2> fmt::Debug for TreeCacheMerkByPath<S, F, F2> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheMerkByPath").finish()
    }
}

/// V4 cleanup data collected while the batch body applied, handed from
/// `apply_batch_structure` back to the outer `apply_batch*` functions, which
/// run the corresponding post-apply storage cleanup passes. Both vecs are
/// empty on V1..V3.
#[derive(Default)]
struct BatchApplyCaptures {
    /// Cidx primary paths displaced by a safe-subset overwrite; their old
    /// primary subtree storage + per-axis secondary namespaces get cleared.
    cidx_overwrite_cleanup_paths: Vec<Vec<Vec<u8>>>,
    /// `(qualified_path, ACTUAL stored tree type)` of every `DeleteTree`
    /// target that was really deleted; cleanup namespaces are selected from
    /// the actual type, not the declared one.
    deleted_tree_actual_types: Vec<(Vec<Vec<u8>>, TreeType)>,
}

/// Result of the pre-apply `DeleteTree` scan shared by
/// `apply_batch_with_element_flags_update` and
/// `apply_partial_batch_with_element_flags_update`.
#[derive(Default)]
struct DeleteTreePreScan {
    /// Deleted non-merk-tree paths whose data namespace gets cleared
    /// post-apply. Filled pre-apply from DECLARED types on V1..V3 only; on
    /// V4+ it starts empty and `classify_captured_delete_trees` fills it
    /// from the captured ACTUAL types.
    non_merk_delete_paths: Vec<Vec<Vec<u8>>>,
    /// Deleted merk-tree paths for the recursive `find_subtrees` clear.
    /// Same V1..V3 / V4+ split as above.
    merk_delete_paths: Vec<Vec<Vec<u8>>>,
    /// Deleted indexed-primary paths for the per-axis secondary sweep.
    /// Same V1..V3 / V4+ split as above.
    cidx_primary_delete_paths: Vec<Vec<Vec<u8>>>,
    /// Paths whose `Skip`-behavior `DeleteTree` found a non-empty tree;
    /// their ops are filtered out of the batch before `apply_body`.
    skipped_delete_paths: HashSet<Vec<Vec<u8>>>,
    /// V4+ only: qualified path → deletion behavior of every `DeleteTree`
    /// op, so `classify_captured_delete_trees` can honor the behavior when
    /// folding captured actual types into the cleanup lists.
    delete_tree_behaviors: HashMap<Vec<Vec<u8>>, SubelementsDeletionBehavior>,
}

/// V4+ post-apply classification: fold the `(qualified_path, ACTUAL stored
/// tree type)` pairs captured by the merk old-value observer into the
/// cleanup lists, honoring each op's deletion behavior. On V1..V3 the
/// captures are empty and this is a no-op — the lists were already built
/// pre-apply from the declared types, exactly as released.
fn classify_captured_delete_trees(
    captures: Vec<(Vec<Vec<u8>>, TreeType)>,
    behaviors: &HashMap<Vec<Vec<u8>>, SubelementsDeletionBehavior>,
    non_merk_delete_paths: &mut Vec<Vec<Vec<u8>>>,
    merk_delete_paths: &mut Vec<Vec<Vec<u8>>>,
    cidx_primary_delete_paths: &mut Vec<Vec<Vec<u8>>>,
) {
    for (qualified_path, actual_tree_type) in captures {
        // Ops the pre-scan did not register (e.g. add-on DeleteTree ops
        // returned by a partial batch's callback) keep their released
        // no-cleanup behaviour.
        let Some(behavior) = behaviors.get(&qualified_path) else {
            continue;
        };
        match behavior {
            SubelementsDeletionBehavior::DontCheckWithNoCleanup => {
                // No primary storage cleanup — but an indexed primary still
                // needs its secondary namespaces cleared, because they live
                // outside the primary's prefix and are invisible to
                // find_subtrees. is_indexed_primary() (not
                // is_count_indexed_primary): PSIT and PCPSIT must also queue
                // for the all-axis sweep, which clears all three axis tags
                // unconditionally and so is correct for every variant.
                if actual_tree_type.is_indexed_primary() {
                    cidx_primary_delete_paths.push(qualified_path);
                }
            }
            SubelementsDeletionBehavior::DeleteChildren
            | SubelementsDeletionBehavior::Error
            | SubelementsDeletionBehavior::Skip => {
                if actual_tree_type.uses_non_merk_data_storage() {
                    non_merk_delete_paths.push(qualified_path);
                } else {
                    // is_indexed_primary(): PSIT/PCPSIT primaries also need
                    // their path queued for the all-axis secondary sweep.
                    if actual_tree_type.is_indexed_primary() {
                        cidx_primary_delete_paths.push(qualified_path.clone());
                    }
                    merk_delete_paths.push(qualified_path);
                }
            }
        }
    }
}

#[allow(dead_code)] // get_batch_run_mode is defined for future use
trait TreeCache<G, SR> {
    fn insert(
        &mut self,
        path: &KeyInfoPath,
        key: &KeyInfo,
        tree_type: TreeType,
    ) -> CostResult<(), Error>;

    fn get_batch_run_mode(&self) -> BatchRunMode;

    /// We will also be returning an op mode, this is to be used in propagation
    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, GroveOp>,
        ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, GroveOp>,
        batch_apply_options: &BatchApplyOptions,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
        grove_version: &GroveVersion,
    ) -> CostResult<RootHashKeyAndAggregateData, Error>;

    fn update_base_merk_root_key(
        &mut self,
        root_key: Option<Vec<u8>>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>;

    /// After a level's `execute_ops_on_path` returns, the bubble-up code
    /// calls this to retrieve the cidx-secondary state captured by that
    /// level (if the level was a cidx primary). Default impl returns
    /// `None` for caches that do not support cidx primary mutations.
    fn take_cidx_secondary_after_apply(
        &mut self,
        _path: &[Vec<u8>],
    ) -> Option<Vec<(u8, CryptoHash, Option<Vec<u8>>)>> {
        None
    }

    /// After all level processing completes, `apply_batch` calls this
    /// to retrieve the list of cidx primary paths whose OLD storage
    /// (primary subtree + secondary namespace) must be cleaned up
    /// because a safe-subset overwrite replaced them with a non-cidx
    /// element or an empty cidx. Default impl returns an empty Vec.
    fn take_cidx_overwrite_cleanup_paths(&mut self) -> Vec<Vec<Vec<u8>>> {
        Vec::new()
    }

    /// After all level processing completes, `apply_batch` calls this to
    /// retrieve the `(qualified_path, actual_tree_type)` pairs captured
    /// (V4+ only) for the `DeleteTree` targets that were really deleted, so
    /// the post-apply cleanup can classify namespaces by the ACTUAL stored
    /// type. Default impl returns an empty Vec.
    fn take_deleted_tree_actual_types(&mut self) -> Vec<(Vec<Vec<u8>>, TreeType)> {
        Vec::new()
    }
}

impl<'db, S, F, F2> TreeCacheMerkByPath<S, F, F2>
where
    F: FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
    F2: FnMut(
        &[Vec<u8>],
        Option<&Element>,
    ) -> CostResult<Vec<(grovedb_element::indexed::IndexAxis, Merk<S>)>, Error>,
    S: StorageContext<'db>,
{
    /// Processes a reference, determining whether it can be retrieved from a
    /// batch operation.
    ///
    /// This function performs the processing for a reference when it does not
    /// change in the same batch. It distinguishes between two cases:
    ///
    /// 1. When the hop count is exactly 1, it tries to directly extract the
    ///    value hash from the reference element.
    ///
    /// 2. When the hop count is greater than 1, it retrieves the referenced
    ///    element and then determines the next step based on the type of the
    ///    element.
    ///
    /// # Arguments
    ///
    /// * `qualified_path`: The path to the referenced element. It should be
    ///   already checked to be a valid path.
    /// * `recursions_allowed`: The maximum allowed hop count to reach the
    ///   target element.
    ///
    /// # Returns
    ///
    /// * `Ok(CryptoHash)`: Returns the crypto hash of the referenced element
    ///   wrapped in the associated cost, if successful.
    ///
    /// * `Err(Error)`: Returns an error if there is an issue with the
    ///   operation, such as missing reference, corrupted data, or invalid batch
    ///   operation.
    ///
    /// # Errors
    ///
    /// This function will return `Err(Error)` if there are any issues
    /// encountered while processing the reference. Possible errors include:
    ///
    /// * `Error::MissingReference`: If a direct or indirect reference to the
    ///   target element is missing in the batch.
    /// * `Error::CorruptedData`: If there is an issue while retrieving or
    ///   deserializing the referenced element.
    /// * `Error::InvalidBatchOperation`: If the referenced element points to a
    ///   tree being updated.
    fn process_reference<'a, G, SR>(
        &'a mut self,
        qualified_path: &[Vec<u8>],
        ops_by_qualified_paths: &'a BTreeMap<Vec<Vec<u8>>, GroveOp>,
        recursions_allowed: u8,
        intermediate_reference_info: Option<&'a ReferencePathType>,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
        visited: &mut HashSet<Vec<Vec<u8>>>,
        grove_version: &GroveVersion,
    ) -> CostResult<CryptoHash, Error>
    where
        G: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<bool, Error>,
        SR: FnMut(
            &mut ElementFlags,
            u32,
            u32,
        ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        let mut cost = OperationCost::default();
        let (key, reference_path) = qualified_path
            .split_last()
            .expect("path validated non-empty above");

        // Fast path: `recursions_allowed == 1` means the user-declared
        // `max_reference_hop` budget allows exactly one more hop. Under
        // the well-formed-user contract, that one hop must land on an
        // `Item` (or `SumItem` / `ItemWithSumItem`) terminal — pointing
        // at another `Reference` would violate the user's own budget.
        //
        // For an `Item` terminal the merk-stored `value_hash` IS the
        // terminal's simple hash `H(serialize(item))`, which is exactly
        // what `insert_reference` bakes into the dependent ref via
        // `Op::PutCombinedReference`. So we can skip a full element
        // decode and read the value_hash directly.
        //
        // Ill-formed input (`max_hop = 1` pointing at a `Reference`)
        // is out of scope: this fast path would return the target's
        // merk-combined hash as if it were a simple hash, producing a
        // hash mismatch that `verify_grovedb` later reports. The
        // contract is the user's to uphold; we don't pay the price of
        // an extra dispatch on every well-formed hop=1 ref.
        if recursions_allowed == 1 {
            let merk = match self.merks.entry(reference_path.to_vec()) {
                HashMapEntry::Occupied(o) => o.into_mut(),
                HashMapEntry::Vacant(v) => v.insert(cost_return_on_error!(
                    &mut cost,
                    (self.get_merk_fn)(reference_path, false)
                )),
            };

            let referenced_element_value_hash_opt = cost_return_on_error!(
                &mut cost,
                merk.get_value_hash(
                    key.as_ref(),
                    true,
                    Some(Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(|e| Error::CorruptedData(e.to_string()))
            );

            let referenced_element_value_hash = cost_return_on_error!(
                &mut cost,
                referenced_element_value_hash_opt
                    .ok_or({
                        let reference_string = reference_path
                            .iter()
                            .map(hex::encode)
                            .collect::<Vec<String>>()
                            .join("/");
                        Error::MissingReference(format!(
                            "direct reference to path:`{}` key:`{}` in batch is missing",
                            reference_string,
                            hex::encode(key)
                        ))
                    })
                    .wrap_with_cost(OperationCost::default())
            );

            return Ok(referenced_element_value_hash).wrap_with_cost(cost);
        }

        // Slow path: `recursions_allowed > 1`. Dispatch on whether the
        // target is being modified in this same batch. Neither branch
        // needs the merk handle here — the helpers open (or reuse the
        // cached) merk themselves via `self.merks.entry(..)`.
        if let Some(referenced_path) = intermediate_reference_info {
            // Target is in batch (refresh). Hop through the op's new
            // path; budget decrements by one for this hop.
            let path = cost_return_on_error_into_no_add!(
                cost,
                path_from_reference_qualified_path_type(referenced_path.clone(), qualified_path)
            );
            self.follow_reference_get_value_hash(
                path.as_slice(),
                ops_by_qualified_paths,
                recursions_allowed - 1,
                flags_update,
                split_removal_bytes,
                visited,
                grove_version,
            )
        } else {
            // Target is not in batch. Read the on-disk element and
            // dispatch by type (Item terminals return their simple
            // hash; References recurse).
            self.process_reference_with_hop_count_greater_than_one(
                key,
                reference_path,
                qualified_path,
                ops_by_qualified_paths,
                recursions_allowed,
                flags_update,
                split_removal_bytes,
                visited,
                grove_version,
            )
        }
    }

    /// Retrieves and deserializes the referenced element from the Merk tree.
    ///
    /// This function is responsible for fetching the referenced element using
    /// the provided key and reference path, deserializing it into an
    /// `Element`. It handles potential errors that can occur during these
    /// operations, such as missing references or corrupted data.
    ///
    /// # Arguments
    ///
    /// * `key` - The key associated with the referenced element within the Merk
    ///   tree.
    /// * `reference_path` - The path to the referenced element, used to locate
    ///   it in the Merk tree.
    /// * `grove_version` - The current version of the GroveDB being used for
    ///   serialization and deserialization operations.
    ///
    /// # Returns
    ///
    /// * `Ok((Element, Vec<u8>, TreeType))` - Returns the deserialized
    ///   `Element` and the serialized counterpart if the retrieval and
    ///   deserialization are successful, wrapped in the associated cost. Also
    ///   returns if the merk of the element is a sum tree as a TreeType.
    /// * `Err(Error)` - Returns an error if any issue occurs during the
    ///   retrieval or deserialization of the referenced element.
    ///
    /// # Errors
    ///
    /// This function may return the following errors:
    ///
    /// * `Error::MissingReference` - If the referenced element is missing from
    ///   the Merk tree.
    /// * `Error::CorruptedData` - If the referenced element cannot be
    ///   deserialized due to corrupted data.
    fn get_and_deserialize_referenced_element(
        &mut self,
        key: &[u8],
        reference_path: &[Vec<u8>],
        grove_version: &GroveVersion,
    ) -> CostResult<Option<(Element, Vec<u8>, TreeType)>, Error> {
        let mut cost = OperationCost::default();

        let merk = match self.merks.entry(reference_path.to_vec()) {
            HashMapEntry::Occupied(o) => o.into_mut(),
            HashMapEntry::Vacant(v) => v.insert(cost_return_on_error!(
                &mut cost,
                (self.get_merk_fn)(reference_path, false)
            )),
        };

        let referenced_element = cost_return_on_error!(
            &mut cost,
            merk.get(
                key.as_ref(),
                true,
                Some(Element::value_defined_cost_for_serialized_value),
                grove_version
            )
            .map_err(|e| Error::CorruptedData(e.to_string()))
        );

        let tree_type = merk.tree_type;

        if let Some(referenced_element) = referenced_element {
            let element = cost_return_on_error_no_add!(
                cost,
                Element::deserialize(referenced_element.as_slice(), grove_version).map_err(|e| {
                    Error::CorruptedData(format!("unable to deserialize element: {e}"))
                })
            );

            Ok(Some((element, referenced_element, tree_type))).wrap_with_cost(cost)
        } else {
            Ok(None).wrap_with_cost(cost)
        }
    }

    /// Processes a reference with a hop count greater than one, handling the
    /// retrieval and further processing of the referenced element.
    ///
    /// This function is used when the hop count is greater than 1, meaning that
    /// the reference points to another element that may also be a reference.
    /// It handles retrieving the referenced element, deserializing it, and
    /// determining the appropriate action based on the type of the element.
    ///
    /// # Arguments
    ///
    /// * `key` - The key corresponding to the referenced element in the current
    ///   Merk tree.
    /// * `reference_path` - The path to the referenced element within the
    ///   current batch of operations.
    /// * `qualified_path` - The fully qualified path to the reference, already
    ///   validated as a valid path.
    /// * `ops_by_qualified_paths` - A map of qualified paths to their
    ///   corresponding batch operations. Used to track and manage updates
    ///   within the batch.
    /// * `recursions_allowed` - The maximum allowed hop count to reach the
    ///   final target element. Each recursive call reduces this by one.
    /// * `flags_update` - A mutable closure that handles updating element flags
    ///   during the processing of the reference.
    /// * `split_removal_bytes` - A mutable closure that handles splitting and
    ///   managing the removal of bytes during the processing of the reference.
    /// * `grove_version` - The current version of the GroveDB being used for
    ///   serialization and deserialization operations.
    ///
    /// # Returns
    ///
    /// * `Ok(CryptoHash)` - Returns the crypto hash of the referenced element
    ///   if successful, wrapped in the associated cost.
    /// * `Err(Error)` - Returns an error if there is an issue with the
    ///   operation, such as a missing reference, corrupted data, or an invalid
    ///   batch operation.
    ///
    /// # Errors
    ///
    /// This function will return `Err(Error)` if any issues are encountered
    /// during the processing of the reference. Possible errors include:
    ///
    /// * `Error::MissingReference` - If a direct or indirect reference to the
    ///   target element is missing in the batch.
    /// * `Error::CorruptedData` - If there is an issue while retrieving or
    ///   deserializing the referenced element.
    /// * `Error::InvalidBatchOperation` - If the referenced element points to a
    ///   tree being updated, which is not allowed.
    fn process_reference_with_hop_count_greater_than_one<'a, G, SR>(
        &'a mut self,
        key: &[u8],
        reference_path: &[Vec<u8>],
        qualified_path: &[Vec<u8>],
        ops_by_qualified_paths: &'a BTreeMap<Vec<Vec<u8>>, GroveOp>,
        recursions_allowed: u8,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
        visited: &mut HashSet<Vec<Vec<u8>>>,
        grove_version: &GroveVersion,
    ) -> CostResult<CryptoHash, Error>
    where
        G: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<bool, Error>,
        SR: FnMut(
            &mut ElementFlags,
            u32,
            u32,
        ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        let mut cost = OperationCost::default();

        let Some((element, ..)) = cost_return_on_error!(
            &mut cost,
            self.get_and_deserialize_referenced_element(key, reference_path, grove_version)
        ) else {
            let reference_string = reference_path
                .iter()
                .map(hex::encode)
                .collect::<Vec<String>>()
                .join("/");
            return Err(Error::MissingReference(format!(
                "reference to path:`{}` key:`{}` in batch is missing",
                reference_string,
                hex::encode(key)
            )))
            .wrap_with_cost(cost);
        };

        // Dispatch on the underlying element type but compute the value hash
        // from the OUTER element's serialized bytes. Storage keeps the
        // wrapper byte; the on-disk value hash must reflect that.
        match element.underlying() {
            Element::Item(..) | Element::SumItem(..) | Element::ItemWithSumItem(..) => {
                let serialized =
                    cost_return_on_error_into_no_add!(cost, element.serialize(grove_version));
                let val_hash = value_hash(&serialized).unwrap_add_cost(&mut cost);
                Ok(val_hash).wrap_with_cost(cost)
            }
            // Both reference variants follow the same chain-resolution path
            // to compute their effective value hash.
            Element::Reference(path, ..) | Element::ReferenceWithSumItem(path, ..) => {
                let path = cost_return_on_error_into_no_add!(
                    cost,
                    path_from_reference_qualified_path_type(path.clone(), qualified_path)
                );
                self.follow_reference_get_value_hash(
                    path.as_slice(),
                    ops_by_qualified_paths,
                    recursions_allowed - 1,
                    flags_update,
                    split_removal_bytes,
                    visited,
                    grove_version,
                )
            }
            Element::Tree(..)
            | Element::SumTree(..)
            | Element::BigSumTree(..)
            | Element::CountTree(..)
            | Element::CountSumTree(..)
            | Element::ProvableCountTree(..)
            | Element::ProvableCountSumTree(..)
            | Element::ProvableSumTree(..)
            | Element::ProvableCountProvableSumTree(..)
            | Element::CommitmentTree(..)
            | Element::MmrTree(..)
            | Element::BulkAppendTree(..)
            | Element::DenseAppendOnlyFixedSizeTree(..)
            | Element::ProvableSumIndexedTree(..)
            | Element::ProvableCountIndexedTree(..)
            | Element::ProvableCountProvableSumIndexedTree(..) => Err(
                Error::InvalidBatchOperation("references can not point to trees being updated"),
            )
            .wrap_with_cost(cost),
            // underlying() unwraps a single level; the constructor and
            // (de)serializer reject nested wrappers, so these are
            // unreachable by construction.
            Element::NonCounted(_) | Element::NotSummed(_) | Element::NotCountedOrSummed(_) => {
                unreachable!("wrappers may not nest")
            }
        }
    }

    /// A reference assumes the value hash of the base item it points to.
    /// In a reference chain base_item -> ref_1 -> ref_2 e.t.c.
    /// all references in that chain (ref_1, ref_2) assume the value hash of the
    /// base_item. The goal of this function is to figure out what the
    /// value_hash of a reference chain is. If we want to insert ref_3 to the
    /// chain above and nothing else changes, we can get the value_hash from
    /// ref_2. But when dealing with batches, you can have an operation to
    /// insert ref_3 and another operation to change something in the
    /// reference chain in the same batch.
    /// All these has to be taken into account.
    fn follow_reference_get_value_hash<'a, G, SR>(
        &'a mut self,
        qualified_path: &[Vec<u8>],
        ops_by_qualified_paths: &'a BTreeMap<Vec<Vec<u8>>, GroveOp>,
        recursions_allowed: u8,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
        visited: &mut HashSet<Vec<Vec<u8>>>,
        grove_version: &GroveVersion,
    ) -> CostResult<CryptoHash, Error>
    where
        G: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<bool, Error>,
        SR: FnMut(
            &mut ElementFlags,
            u32,
            u32,
        ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        let mut cost = OperationCost::default();
        // Cap recursion depth to MAX_REFERENCE_HOPS to prevent excessive stack
        // depth even if the user-provided element_max_reference_hop is larger.
        let recursions_allowed = recursions_allowed.min(MAX_REFERENCE_HOPS as u8);
        if recursions_allowed == 0 {
            return Err(Error::ReferenceLimit).wrap_with_cost(cost);
        }
        let path_vec = qualified_path.to_vec();
        if !visited.insert(path_vec) {
            return Err(Error::CyclicReference).wrap_with_cost(cost);
        }
        // If the element being referenced changes in the same batch
        // we need to set the value_hash based on the new change and not the old state.

        // However the operation might either be merged or unmerged, if it is unmerged
        // we need to merge it with the state first
        if let Some(op) = ops_by_qualified_paths.get(qualified_path) {
            // the path is being modified, inserted or deleted in the batch of operations
            match op {
                GroveOp::ReplaceTreeRootKey { .. }
                | GroveOp::InsertTreeWithRootHash { .. }
                | GroveOp::ReplaceNonMerkTreeRoot { .. }
                | GroveOp::InsertNonMerkTree { .. }
                | GroveOp::ReplaceAggregateIndexedTreeRootKeys { .. }
                | GroveOp::InsertAggregateIndexedTreeRootKeys { .. }
                | GroveOp::CommitmentTreeInsert { .. }
                | GroveOp::MmrTreeAppend { .. }
                | GroveOp::BulkAppend { .. }
                | GroveOp::DenseTreeInsert { .. } => Err(Error::InvalidBatchOperation(
                    "references can not point to trees being updated",
                ))
                .wrap_with_cost(cost),
                GroveOp::InsertOrReplace { element }
                | GroveOp::Replace { element }
                | GroveOp::Patch { element, .. } => {
                    // Look through NonCounted for dispatch; serialize the outer
                    // wrapper for hashing so the value hash matches storage.
                    match element.underlying() {
                        Element::Item(..) | Element::SumItem(..) | Element::ItemWithSumItem(..) => {
                            let serialized = cost_return_on_error_into_no_add!(
                                cost,
                                element.serialize(grove_version)
                            );
                            if element.get_flags().is_none() {
                                // There are no storage flags, we can just hash new element
                                let val_hash = value_hash(&serialized).unwrap_add_cost(&mut cost);
                                Ok(val_hash).wrap_with_cost(cost)
                            } else {
                                let mut new_element = element.clone();

                                // it can be unmerged, let's get the value on disk
                                let (key, reference_path) = qualified_path
                                    .split_last()
                                    .expect("path validated non-empty above");
                                let serialized_element_result = cost_return_on_error!(
                                    &mut cost,
                                    self.get_and_deserialize_referenced_element(
                                        key,
                                        reference_path,
                                        grove_version
                                    )
                                );
                                if let Some((old_element, old_serialized_element, is_in_sum_tree)) =
                                    serialized_element_result
                                {
                                    let value_hash = cost_return_on_error!(
                                        &mut cost,
                                        Self::process_old_element_flags(
                                            key,
                                            &serialized,
                                            &mut new_element,
                                            old_element,
                                            &old_serialized_element,
                                            is_in_sum_tree,
                                            flags_update,
                                            split_removal_bytes,
                                            grove_version,
                                        )
                                    );
                                    Ok(value_hash).wrap_with_cost(cost)
                                } else {
                                    let value_hash =
                                        value_hash(&serialized).unwrap_add_cost(&mut cost);
                                    Ok(value_hash).wrap_with_cost(cost)
                                }
                            }
                        }
                        // Both reference variants follow the same chain.
                        Element::Reference(path, ..) | Element::ReferenceWithSumItem(path, ..) => {
                            let path = cost_return_on_error_into_no_add!(
                                cost,
                                path_from_reference_qualified_path_type(
                                    path.clone(),
                                    qualified_path
                                )
                            );
                            self.follow_reference_get_value_hash(
                                path.as_slice(),
                                ops_by_qualified_paths,
                                recursions_allowed - 1,
                                flags_update,
                                split_removal_bytes,
                                visited,
                                grove_version,
                            )
                        }
                        Element::Tree(..)
                        | Element::SumTree(..)
                        | Element::BigSumTree(..)
                        | Element::CountTree(..)
                        | Element::CountSumTree(..)
                        | Element::ProvableCountTree(..)
                        | Element::ProvableCountSumTree(..)
                        | Element::ProvableSumTree(..)
                        | Element::ProvableCountProvableSumTree(..)
                        | Element::CommitmentTree(..)
                        | Element::MmrTree(..)
                        | Element::BulkAppendTree(..)
                        | Element::DenseAppendOnlyFixedSizeTree(..)
                        | Element::ProvableSumIndexedTree(..)
                        | Element::ProvableCountIndexedTree(..)
                        | Element::ProvableCountProvableSumIndexedTree(..) => {
                            Err(Error::InvalidBatchOperation(
                                "references can not point to trees being updated",
                            ))
                            .wrap_with_cost(cost)
                        }
                        // Wrappers are unwrapped via underlying() above.
                        Element::NonCounted(_)
                        | Element::NotSummed(_)
                        | Element::NotCountedOrSummed(_) => {
                            unreachable!("unwrapped above")
                        }
                    }
                }
                GroveOp::InsertWithKnownToNotAlreadyExist { element }
                | GroveOp::InsertIfNotExists { element, .. } => match element.underlying() {
                    Element::Item(..) | Element::SumItem(..) | Element::ItemWithSumItem(..) => {
                        let serialized = cost_return_on_error_into_no_add!(
                            cost,
                            element.serialize(grove_version)
                        );
                        let val_hash = value_hash(&serialized).unwrap_add_cost(&mut cost);
                        Ok(val_hash).wrap_with_cost(cost)
                    }
                    Element::Reference(path, ..) | Element::ReferenceWithSumItem(path, ..) => {
                        let path = cost_return_on_error_into_no_add!(
                            cost,
                            path_from_reference_qualified_path_type(path.clone(), qualified_path)
                        );
                        self.follow_reference_get_value_hash(
                            path.as_slice(),
                            ops_by_qualified_paths,
                            recursions_allowed - 1,
                            flags_update,
                            split_removal_bytes,
                            visited,
                            grove_version,
                        )
                    }
                    Element::Tree(..)
                    | Element::SumTree(..)
                    | Element::BigSumTree(..)
                    | Element::CountTree(..)
                    | Element::CountSumTree(..)
                    | Element::ProvableCountTree(..)
                    | Element::ProvableCountSumTree(..)
                    | Element::ProvableSumTree(..)
                    | Element::ProvableCountProvableSumTree(..)
                    | Element::CommitmentTree(..)
                    | Element::MmrTree(..)
                    | Element::BulkAppendTree(..)
                    | Element::DenseAppendOnlyFixedSizeTree(..)
                    | Element::ProvableSumIndexedTree(..)
                    | Element::ProvableCountIndexedTree(..)
                    | Element::ProvableCountProvableSumIndexedTree(..) => {
                        Err(Error::InvalidBatchOperation(
                            "references can not point to trees being updated",
                        ))
                        .wrap_with_cost(cost)
                    }
                    // Wrappers are unwrapped via underlying() above.
                    Element::NonCounted(_)
                    | Element::NotSummed(_)
                    | Element::NotCountedOrSummed(_) => {
                        unreachable!("unwrapped above")
                    }
                },
                GroveOp::RefreshReference {
                    reference_path_type,
                    mode,
                    ..
                } => {
                    // We are pointing towards a reference that will
                    // be refreshed in this batch. The dependent
                    // ref's value hash must be computed against
                    // whatever the apply path will write — which
                    // depends on the trust mode encoded in `mode`:
                    //
                    // * Trusted variants: apply writes the op's
                    //   payload (`reference_path_type`). Thread it
                    //   through so dependent refs resolve against
                    //   the post-batch path.
                    //
                    // * Untrusted variants: apply keeps the on-disk
                    //   path (sum-item updates only override the
                    //   carried sum; the path is preserved). Pass
                    //   `None` so `process_reference` resolves
                    //   through the (unchanged) on-disk path.
                    let reference_info = if mode.is_trusted() {
                        Some(reference_path_type)
                    } else {
                        None
                    };
                    self.process_reference(
                        qualified_path,
                        ops_by_qualified_paths,
                        recursions_allowed,
                        reference_info,
                        flags_update,
                        split_removal_bytes,
                        visited,
                        grove_version,
                    )
                }
                GroveOp::Delete | GroveOp::DeleteTree(..) => Err(Error::InvalidBatchOperation(
                    "references can not point to something currently being deleted",
                ))
                .wrap_with_cost(cost),
            }
        } else {
            self.process_reference(
                qualified_path,
                ops_by_qualified_paths,
                recursions_allowed,
                None,
                flags_update,
                split_removal_bytes,
                visited,
                grove_version,
            )
        }
    }
}

impl<'db, S, F, F2, G, SR> TreeCache<G, SR> for TreeCacheMerkByPath<S, F, F2>
where
    G: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<bool, Error>,
    SR: FnMut(
        &mut ElementFlags,
        u32,
        u32,
    ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    F: FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
    F2: FnMut(
        &[Vec<u8>],
        Option<&Element>,
    ) -> CostResult<Vec<(grovedb_element::indexed::IndexAxis, Merk<S>)>, Error>,
    S: StorageContext<'db>,
{
    fn insert(
        &mut self,
        path: &KeyInfoPath,
        key: &KeyInfo,
        tree_type: TreeType,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let mut inserted_path = path.to_path();
        inserted_path.push(key.get_key_clone());
        if let HashMapEntry::Vacant(e) = self.merks.entry(inserted_path.clone()) {
            let mut merk =
                cost_return_on_error!(&mut cost, (self.get_merk_fn)(&inserted_path, true));
            merk.tree_type = tree_type;
            e.insert(merk);
        }

        Ok(()).wrap_with_cost(cost)
    }

    fn take_cidx_secondary_after_apply(
        &mut self,
        path: &[Vec<u8>],
    ) -> Option<Vec<(u8, CryptoHash, Option<Vec<u8>>)>> {
        self.indexed_secondary_after_apply.remove(path)
    }

    fn take_cidx_overwrite_cleanup_paths(&mut self) -> Vec<Vec<Vec<u8>>> {
        std::mem::take(&mut self.cidx_overwrite_cleanup_paths)
    }

    fn take_deleted_tree_actual_types(&mut self) -> Vec<(Vec<Vec<u8>>, TreeType)> {
        std::mem::take(&mut self.deleted_tree_actual_types)
    }

    fn update_base_merk_root_key(
        &mut self,
        root_key: Option<Vec<u8>>,
        _grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let base_path = vec![];

        let merk = match self.merks.entry(base_path.clone()) {
            HashMapEntry::Occupied(o) => o.into_mut(),
            HashMapEntry::Vacant(v) => v.insert(cost_return_on_error!(
                &mut cost,
                (self.get_merk_fn)(&base_path, false)
            )),
        };

        merk.set_base_root_key(root_key)
            .add_cost(cost)
            .map_err(|e| Error::InternalError(format!("unable to set base root key: {e}")))
    }

    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, GroveOp>,
        ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, GroveOp>,
        batch_apply_options: &BatchApplyOptions,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
        grove_version: &GroveVersion,
    ) -> CostResult<RootHashKeyAndAggregateData, Error> {
        let mut cost = OperationCost::default();
        // todo: fix this
        let p = path.to_path();
        let path = &p;

        // This also populates Merk trees cache
        let in_tree_type = {
            let merk = match self.merks.entry(path.to_vec()) {
                HashMapEntry::Occupied(o) => o.into_mut(),
                HashMapEntry::Vacant(v) => v.insert(cost_return_on_error!(
                    &mut cost,
                    (self.get_merk_fn)(path, false)
                )),
            };
            merk.tree_type
        };

        // For cidx primaries, capture the pre-apply count value of every
        // key that this level's ops will mutate. After
        // `merk.apply_with_specialized_costs` runs we re-read each
        // key's post-apply element and use the (old, new) count pair to
        // mirror the change in the secondary Merk. Then we store the
        // post-mirror secondary state in `cidx_secondary_after_apply`
        // so the bubble-up code can emit
        // `ReplaceAggregateIndexedTreeRootKeys` instead of the standard
        // `ReplaceTreeRootKey`. `ReplaceTreeRootKey` ops on a cidx
        // primary level represent a child subtree's bubble-up — the
        // child's element bytes have a new aggregate count, so its
        // secondary entry needs to move; we capture it here too.
        let indexed_pre_state: Option<BTreeMap<Vec<u8>, Option<(u64, i64)>>> = if in_tree_type
            .is_indexed_primary()
        {
            let merk = self.merks.get(path).expect("the Merk is cached");
            Some(cost_return_on_error!(
                &mut cost,
                indexed_tree::capture_indexed_pre_state(merk, &ops_at_path_by_key, grove_version,)
            ))
        } else {
            None
        };

        // V4 gates: keys whose ops need the OLD element they displace. The
        // merk apply surfaces those bytes for free through the old-value
        // observer below — the walker fetched the node anyway to rewrite or
        // delete it — so no dedicated stored-element read (and no extra
        // tracked cost) is issued. On V1..V3 both maps stay empty.
        let mut pending_overwrite_inspections: BTreeMap<Vec<u8>, Element> = BTreeMap::new();
        let mut pending_delete_tree_checks: BTreeMap<Vec<u8>, TreeType> = BTreeMap::new();

        let mut batch_operations: Vec<(Vec<u8>, Op)> = vec![];
        for (key_info, op) in ops_at_path_by_key.into_iter() {
            match op {
                op_ref @ (GroveOp::InsertWithKnownToNotAlreadyExist { .. }
                | GroveOp::InsertIfNotExists { .. }
                | GroveOp::InsertOrReplace { .. }
                | GroveOp::Replace { .. }
                | GroveOp::Patch { .. }) => {
                    let (is_insert_if_not_exists, error_if_exists) = match &op_ref {
                        GroveOp::InsertIfNotExists {
                            error_if_exists, ..
                        } => (true, *error_if_exists),
                        _ => (false, false),
                    };
                    let op_could_overwrite = matches!(
                        &op_ref,
                        GroveOp::InsertOrReplace { .. }
                            | GroveOp::Replace { .. }
                            | GroveOp::Patch { .. }
                    );
                    let element = match op_ref {
                        GroveOp::InsertWithKnownToNotAlreadyExist { element }
                        | GroveOp::InsertIfNotExists { element, .. }
                        | GroveOp::InsertOrReplace { element }
                        | GroveOp::Replace { element }
                        | GroveOp::Patch { element, .. } => element,
                        // Structurally unreachable: the enclosing arm already
                        // constrained op_ref to exactly these five variants.
                        // Fail gracefully instead of panicking if a refactor
                        // ever widens the outer pattern.
                        _ => {
                            return Err(Error::CorruptedCodeExecution(
                                "execute_ops_on_path: op matched an insert/replace/patch \
                                 variant in the outer arm but not when extracting its element",
                            ))
                            .wrap_with_cost(cost);
                        }
                    };

                    // Check tree-override protection for all non-reference elements.
                    // `is_reference()` looks through `NonCounted` and recognizes
                    // both `Element::Reference` and `Element::ReferenceWithSumItem`,
                    // so wrapped or sum-bearing references receive the same
                    // exemption as plain references.
                    if batch_apply_options.validate_insertion_does_not_override_tree
                        && !element.is_reference()
                    {
                        let merk = self.merks.get_mut(path).expect("the Merk is cached");
                        let maybe_existing = cost_return_on_error_into!(
                            &mut cost,
                            merk.get(
                                key_info.get_key_clone().as_slice(),
                                true,
                                Some(&Element::value_defined_cost_for_serialized_value,),
                                grove_version,
                            )
                            .map_err(|e| {
                                Error::CorruptedData(format!(
                                    "unable to check for existing tree: {e}"
                                ))
                            })
                        );
                        if let Some(existing_bytes) = maybe_existing {
                            let existing_element = cost_return_on_error_no_add!(
                                cost,
                                Element::deserialize(existing_bytes.as_slice(), grove_version)
                                    .map_err(|_| {
                                        Error::CorruptedData(
                                            "unable to deserialize existing element".to_string(),
                                        )
                                    })
                            );
                            if existing_element.is_any_tree() {
                                return Err(Error::InvalidBatchOperation(
                                    "attempting to overwrite a tree",
                                ))
                                .wrap_with_cost(cost);
                            }
                        }
                    } else if op_could_overwrite
                        && grove_version
                            .grovedb_versions
                            .apply_batch
                            .overwrite_indexed_cleanup_inspection
                            >= 1
                    {
                        // Register the key so the merk old-value observer can
                        // classify what this op displaces (safe subset →
                        // schedule cleanup, ambiguous → err, non-indexed →
                        // no-op). The observer only fires when the key
                        // actually exists, and the bytes it sees are the node
                        // the merk walk fetched anyway — so unlike the
                        // pre-V4 shape of this gate, no dedicated
                        // stored-element read is issued and V4 charges
                        // exactly the V1..V3 cost for every
                        // overwrite-capable op.
                        //
                        // Bare `Reference` overwrites are included: with the
                        // read gone there is no cost argument for leaving a
                        // reference that overwrites an indexed tree unswept.
                        pending_overwrite_inspections
                            .insert(key_info.get_key_clone(), element.clone());
                    }

                    // Mirror the per-merk insert guard: wrapper children are
                    // only valid inside the matching aggregate-bearing parents.
                    // Without these checks, batch users could persist
                    // wrapped elements into the wrong tree types and silently
                    // violate the wrapper invariant.
                    if element.is_non_counted() && !in_tree_type.accepts_non_counted_children() {
                        return Err(Error::InvalidBatchOperation(
                            "non-counted elements may only be inserted into non-provable \
                             count-bearing trees (CountTree or CountSumTree); Provable* count \
                             trees commit the count cryptographically and cannot host \
                             NonCounted children",
                        ))
                        .wrap_with_cost(cost);
                    }
                    if element.is_not_summed() && !in_tree_type.is_sum_bearing() {
                        return Err(Error::InvalidBatchOperation(
                            "not-summed elements may only be inserted into sum-bearing trees",
                        ))
                        .wrap_with_cost(cost);
                    }
                    if element.is_not_counted_or_summed()
                        && !in_tree_type.accepts_not_counted_or_summed_children()
                    {
                        return Err(Error::InvalidBatchOperation(
                            "not-counted-or-summed elements may only be inserted into \
                             CountSumTree; ProvableCountSumTree commits the count \
                             cryptographically and cannot host NotCountedOrSummed children",
                        ))
                        .wrap_with_cost(cost);
                    }
                    // Look through NonCounted; methods called on `element`
                    // (serialize, get_feature_type, insert_*_into_batch_operations,
                    // element_at_key_already_exists) are wrapper-aware via the
                    // helper methods updated in grovedb-element.
                    match element.underlying() {
                        // Both reference variants share this batch-insert
                        // path. `ReferenceWithSumItem` has a 4-tuple shape
                        // (path, max_hop, sum_value, flags); we only bind
                        // the path and the max-hop here — the sum value is
                        // included in the element's serialized bytes via
                        // `insert_reference_into_batch_operations` and is
                        // picked up by `get_feature_type` for the parent's
                        // sum aggregation.
                        Element::Reference(path_reference, element_max_reference_hop, _)
                        | Element::ReferenceWithSumItem(
                            path_reference,
                            element_max_reference_hop,
                            _,
                            _,
                        ) => {
                            // Check existence for InsertIfNotExists on references
                            if is_insert_if_not_exists
                                || batch_apply_options.validate_insertion_does_not_override
                            {
                                let merk = self.merks.get_mut(path).expect("the Merk is cached");
                                let existing = cost_return_on_error_into!(
                                    &mut cost,
                                    element.element_at_key_already_exists(
                                        merk,
                                        key_info.get_key_clone().as_slice(),
                                        grove_version,
                                    )
                                );
                                if existing {
                                    if error_if_exists
                                        || batch_apply_options.validate_insertion_does_not_override
                                    {
                                        return Err(Error::InvalidBatchOperation(
                                            "attempting to insert reference that already exists",
                                        ))
                                        .wrap_with_cost(cost);
                                    }
                                    continue;
                                }
                            }

                            let merk_feature_type = cost_return_on_error_into!(
                                &mut cost,
                                element
                                    .get_feature_type(in_tree_type)
                                    .wrap_with_cost(OperationCost::default())
                            );
                            let path_reference = cost_return_on_error_into!(
                                &mut cost,
                                path_from_reference_path_type(
                                    path_reference.clone(),
                                    path,
                                    Some(key_info.as_slice())
                                )
                                .wrap_with_cost(OperationCost::default())
                            );
                            if path_reference.is_empty() {
                                return Err(Error::InvalidBatchOperation(
                                    "attempting to insert an empty reference",
                                ))
                                .wrap_with_cost(cost);
                            }

                            let referenced_element_value_hash = cost_return_on_error!(
                                &mut cost,
                                self.follow_reference_get_value_hash(
                                    path_reference.as_slice(),
                                    ops_by_qualified_paths,
                                    element_max_reference_hop.unwrap_or(MAX_REFERENCE_HOPS as u8),
                                    flags_update,
                                    split_removal_bytes,
                                    &mut HashSet::new(),
                                    grove_version,
                                )
                            );

                            cost_return_on_error_into!(
                                &mut cost,
                                element.insert_reference_into_batch_operations(
                                    key_info.get_key_clone(),
                                    referenced_element_value_hash,
                                    &mut batch_operations,
                                    merk_feature_type,
                                    grove_version,
                                )
                            );
                        }
                        // CountIndexedTree / ProvableCountIndexedTree own two
                        // child Merks (primary + secondary). For the batch
                        // path we accept only the empty-creation case here:
                        // both root keys = None, count = 0. This is
                        // sufficient to create the element bytes correctly
                        // (with the H1-A three-input combine and both
                        // child hashes = NULL_HASH); subsequent item-level
                        // mutations into the primary still need the
                        // dedicated `insert_into_count_indexed_tree` /
                        // `delete_from_count_indexed_tree` APIs because
                        // the batch propagation pass does not yet cascade
                        // through the secondary.
                        Element::ProvableCountIndexedTree(primary, secondary, count_value, _) => {
                            if primary.is_some() || secondary.is_some() || *count_value != 0 {
                                return Err(Error::InvalidBatchOperation(
                                    "a CountIndexedTree must be empty at the moment of batch \
                                     insertion (both primary_root_key and secondary_root_key \
                                     must be None and count = 0); item-level mutations require \
                                     the dedicated insert_into_count_indexed_tree API",
                                ))
                                .wrap_with_cost(cost);
                            }
                            // Check existence for InsertIfNotExists.
                            if is_insert_if_not_exists
                                || batch_apply_options.validate_insertion_does_not_override
                            {
                                let merk = self.merks.get_mut(path).expect("the Merk is cached");
                                let existing = cost_return_on_error_into!(
                                    &mut cost,
                                    element.element_at_key_already_exists(
                                        merk,
                                        key_info.get_key_clone().as_slice(),
                                        grove_version,
                                    )
                                );
                                if existing {
                                    if error_if_exists
                                        || batch_apply_options.validate_insertion_does_not_override
                                    {
                                        return Err(Error::InvalidBatchOperation(
                                            "attempting to insert CountIndexedTree element that \
                                             already exists",
                                        ))
                                        .wrap_with_cost(cost);
                                    }
                                    continue;
                                }
                            }

                            let merk_feature_type = cost_return_on_error_into!(
                                &mut cost,
                                element
                                    .get_feature_type(in_tree_type)
                                    .wrap_with_cost(OperationCost::default())
                            );
                            cost_return_on_error_into!(
                                &mut cost,
                                element.insert_count_indexed_subtree_into_batch_operations(
                                    key_info.get_key_clone(),
                                    NULL_HASH,
                                    NULL_HASH,
                                    false,
                                    &mut batch_operations,
                                    merk_feature_type,
                                    grove_version,
                                )
                            );
                        }
                        // ProvableSumIndexedTree empty-creation: same
                        // shape as PCIT — both root keys must be None
                        // and sum_value = 0, then we write the element
                        // via insert_count_indexed_subtree_into_batch_operations
                        // (the merk-side helper now accepts all three
                        // indexed-tree variants).
                        Element::ProvableSumIndexedTree(primary, secondary, sum_value, _) => {
                            if primary.is_some() || secondary.is_some() || *sum_value != 0 {
                                return Err(Error::InvalidBatchOperation(
                                    "a ProvableSumIndexedTree must be empty at the moment of \
                                     batch insertion (both primary_root_key and \
                                     secondary_root_key must be None and sum = 0); item-level \
                                     mutations require the dedicated insert_into_indexed_tree \
                                     API",
                                ))
                                .wrap_with_cost(cost);
                            }
                            if is_insert_if_not_exists
                                || batch_apply_options.validate_insertion_does_not_override
                            {
                                let merk = self.merks.get_mut(path).expect("the Merk is cached");
                                let existing = cost_return_on_error_into!(
                                    &mut cost,
                                    element.element_at_key_already_exists(
                                        merk,
                                        key_info.get_key_clone().as_slice(),
                                        grove_version,
                                    )
                                );
                                if existing {
                                    if error_if_exists
                                        || batch_apply_options.validate_insertion_does_not_override
                                    {
                                        return Err(Error::InvalidBatchOperation(
                                            "attempting to insert ProvableSumIndexedTree element \
                                             that already exists",
                                        ))
                                        .wrap_with_cost(cost);
                                    }
                                    continue;
                                }
                            }
                            let merk_feature_type = cost_return_on_error_into!(
                                &mut cost,
                                element
                                    .get_feature_type(in_tree_type)
                                    .wrap_with_cost(OperationCost::default())
                            );
                            cost_return_on_error_into!(
                                &mut cost,
                                element.insert_count_indexed_subtree_into_batch_operations(
                                    key_info.get_key_clone(),
                                    NULL_HASH,
                                    NULL_HASH,
                                    false,
                                    &mut batch_operations,
                                    merk_feature_type,
                                    grove_version,
                                )
                            );
                        }
                        // ProvableCountProvableSumIndexedTree empty-creation:
                        // primary_root_key = None, count = 0, sum = 0,
                        // and every axis slot's secondary_root_key =
                        // None (the axes list carries the configured
                        // schema and is non-empty). Each axis slot
                        // contributes NULL_HASH to the axes_digest.
                        Element::ProvableCountProvableSumIndexedTree(
                            primary,
                            count_value,
                            sum_value,
                            axes,
                            _,
                        ) => {
                            let axes_all_empty = axes.iter().all(|(_, sk)| sk.is_none());
                            if primary.is_some()
                                || *count_value != 0
                                || *sum_value != 0
                                || !axes_all_empty
                            {
                                return Err(Error::InvalidBatchOperation(
                                    "a ProvableCountProvableSumIndexedTree must be empty at the \
                                     moment of batch insertion (primary_root_key = None, count \
                                     = 0, sum = 0, every axis secondary_root_key = None); \
                                     item-level mutations require the dedicated \
                                     insert_into_indexed_tree API",
                                ))
                                .wrap_with_cost(cost);
                            }
                            // Full canonical-axes validation (1..=3 entries,
                            // sorted ascending by tag, no duplicates, known
                            // tags) — reuses the same check the Element
                            // constructors run. Without this, a batch caller
                            // could persist an empty PCPSIT whose axes are
                            // unsorted / duplicated / out-of-range (the
                            // axes_digest is computed over whatever TLV is
                            // supplied; it does not validate).
                            cost_return_on_error_no_add!(
                                cost,
                                Element::validate_pcpsit_axes(axes).map_err(|_| {
                                    Error::InvalidBatchOperation(
                                        "a ProvableCountProvableSumIndexedTree must have \
                                         canonical axes (1..=3 entries, sorted ascending by \
                                         tag, no duplicates, tags in 0..=2)",
                                    )
                                })
                            );
                            if is_insert_if_not_exists
                                || batch_apply_options.validate_insertion_does_not_override
                            {
                                let merk = self.merks.get_mut(path).expect("the Merk is cached");
                                let existing = cost_return_on_error_into!(
                                    &mut cost,
                                    element.element_at_key_already_exists(
                                        merk,
                                        key_info.get_key_clone().as_slice(),
                                        grove_version,
                                    )
                                );
                                if existing {
                                    if error_if_exists
                                        || batch_apply_options.validate_insertion_does_not_override
                                    {
                                        return Err(Error::InvalidBatchOperation(
                                            "attempting to insert \
                                             ProvableCountProvableSumIndexedTree element that \
                                             already exists",
                                        ))
                                        .wrap_with_cost(cost);
                                    }
                                    continue;
                                }
                            }
                            let merk_feature_type = cost_return_on_error_into!(
                                &mut cost,
                                element
                                    .get_feature_type(in_tree_type)
                                    .wrap_with_cost(OperationCost::default())
                            );
                            // For an empty PCPSIT each configured axis
                            // contributes NULL_HASH to the digest. The
                            // merk-side helper takes a single second
                            // hash (axes_digest in this case).
                            let zero_axes: Vec<(u8, grovedb_merk::CryptoHash)> =
                                axes.iter().map(|(t, _)| (*t, NULL_HASH)).collect();
                            let empty_axes_digest = grovedb_merk::tree::axes_digest(&zero_axes)
                                .unwrap_add_cost(&mut cost);
                            cost_return_on_error_into!(
                                &mut cost,
                                element.insert_count_indexed_subtree_into_batch_operations(
                                    key_info.get_key_clone(),
                                    NULL_HASH,
                                    empty_axes_digest,
                                    false,
                                    &mut batch_operations,
                                    merk_feature_type,
                                    grove_version,
                                )
                            );
                        }
                        Element::Tree(..)
                        | Element::SumTree(..)
                        | Element::BigSumTree(..)
                        | Element::CountTree(..)
                        | Element::CountSumTree(..)
                        | Element::ProvableCountTree(..)
                        | Element::ProvableCountSumTree(..)
                        | Element::ProvableSumTree(..)
                        | Element::ProvableCountProvableSumTree(..)
                        | Element::MmrTree(..)
                        | Element::BulkAppendTree(..)
                        | Element::DenseAppendOnlyFixedSizeTree(..) => {
                            // Check existence for InsertIfNotExists on subtrees
                            if is_insert_if_not_exists
                                || batch_apply_options.validate_insertion_does_not_override
                            {
                                let merk = self.merks.get_mut(path).expect("the Merk is cached");
                                let existing = cost_return_on_error_into!(
                                    &mut cost,
                                    element.element_at_key_already_exists(
                                        merk,
                                        key_info.get_key_clone().as_slice(),
                                        grove_version,
                                    )
                                );
                                if existing {
                                    if error_if_exists
                                        || batch_apply_options.validate_insertion_does_not_override
                                    {
                                        return Err(Error::InvalidBatchOperation(
                                            "attempting to insert subtree that already exists",
                                        ))
                                        .wrap_with_cost(cost);
                                    }
                                    continue;
                                }
                            }

                            let merk_feature_type = cost_return_on_error_into!(
                                &mut cost,
                                element
                                    .get_feature_type(in_tree_type)
                                    .wrap_with_cost(OperationCost::default())
                            );
                            cost_return_on_error_into!(
                                &mut cost,
                                element.insert_subtree_into_batch_operations(
                                    key_info.get_key_clone(),
                                    NULL_HASH,
                                    false,
                                    &mut batch_operations,
                                    merk_feature_type,
                                    grove_version,
                                )
                            );
                        }
                        Element::CommitmentTree(..) => {
                            let merk_feature_type = cost_return_on_error_into!(
                                &mut cost,
                                element
                                    .get_feature_type(in_tree_type)
                                    .wrap_with_cost(OperationCost::default())
                            );
                            cost_return_on_error_into!(
                                &mut cost,
                                element.insert_subtree_into_batch_operations(
                                    key_info.get_key_clone(),
                                    grovedb_commitment_tree::EMPTY_COMMITMENT_TREE_STATE_ROOT,
                                    false,
                                    &mut batch_operations,
                                    merk_feature_type,
                                    grove_version,
                                )
                            );
                        }
                        Element::Item(..) | Element::SumItem(..) | Element::ItemWithSumItem(..) => {
                            let merk_feature_type = cost_return_on_error_into!(
                                &mut cost,
                                element
                                    .get_feature_type(in_tree_type)
                                    .wrap_with_cost(OperationCost::default())
                            );
                            if is_insert_if_not_exists
                                || batch_apply_options.validate_insertion_does_not_override
                            {
                                let merk = self.merks.get_mut(path).expect("the Merk is cached");

                                let inserted = cost_return_on_error_into!(
                                    &mut cost,
                                    element.insert_if_not_exists_into_batch_operations(
                                        merk,
                                        key_info.get_key(),
                                        &mut batch_operations,
                                        merk_feature_type,
                                        grove_version,
                                    )
                                );
                                if !inserted
                                    && (error_if_exists
                                        || batch_apply_options.validate_insertion_does_not_override)
                                {
                                    return Err(Error::InvalidBatchOperation(
                                        "attempting to insert element that already exists",
                                    ))
                                    .wrap_with_cost(cost);
                                }
                            } else {
                                cost_return_on_error_into!(
                                    &mut cost,
                                    element.insert_into_batch_operations(
                                        key_info.get_key(),
                                        &mut batch_operations,
                                        merk_feature_type,
                                        grove_version,
                                    )
                                );
                            }
                        }
                        // Wrappers are unwrapped via underlying() above.
                        Element::NonCounted(_)
                        | Element::NotSummed(_)
                        | Element::NotCountedOrSummed(_) => {
                            unreachable!("unwrapped above")
                        }
                    }
                }
                GroveOp::RefreshReference {
                    reference_path_type,
                    max_reference_hop,
                    mode,
                    flags,
                    non_counted,
                } => {
                    // Five-way dispatch on the mode variant. Trust
                    // mode is encoded in the variant name — see
                    // `RefreshReferenceMode` for the per-variant
                    // contract.
                    let wrap_if_non_counted = |inner: Element| -> Result<Element, Error> {
                        if non_counted {
                            Element::new_non_counted(inner).map_err(|e| {
                                Error::CorruptedData(format!(
                                    "failed to wrap refreshed reference in NonCounted: {e}"
                                ))
                            })
                        } else {
                            Ok(inner)
                        }
                    };
                    let element = match mode {
                        // ---------- Trusted variants ----------
                        // Build the element from op fields verbatim,
                        // no disk read. If on-disk has a different
                        // variant or wrapper, it gets silently
                        // coerced — caller-asserted shape.
                        RefreshReferenceMode::PlainReferenceTrusted => {
                            let inner =
                                Element::Reference(reference_path_type, max_reference_hop, flags);
                            cost_return_on_error_no_add!(cost, wrap_if_non_counted(inner))
                        }
                        RefreshReferenceMode::SumItemReferenceTrusted(sum) => {
                            let inner = Element::ReferenceWithSumItem(
                                reference_path_type,
                                max_reference_hop,
                                sum,
                                flags,
                            );
                            cost_return_on_error_no_add!(cost, wrap_if_non_counted(inner))
                        }
                        // ---------- Untrusted variants ----------
                        // Read on-disk, cross-check variant +
                        // wrapper, then either write back verbatim
                        // or override the sum.
                        RefreshReferenceMode::PlainReferenceUntrusted
                        | RefreshReferenceMode::SumItemReferenceUntrustedValueUpdate(_)
                        | RefreshReferenceMode::SumItemReferenceUntrustedNoValueUpdate => {
                            let merk = self.merks.get(path).expect("the Merk is cached");
                            let value = cost_return_on_error!(
                                &mut cost,
                                merk.get(
                                    key_info.as_slice(),
                                    true,
                                    Some(Element::value_defined_cost_for_serialized_value),
                                    grove_version
                                )
                                .map(|result_value| result_value
                                    .map_err(Error::MerkError)
                                    .and_then(|maybe_value| maybe_value.ok_or(
                                        Error::InvalidInput(
                                            "trying to refresh a non existing reference",
                                        )
                                    )))
                            );
                            let on_disk = cost_return_on_error_no_add!(
                                cost,
                                Element::deserialize(value.as_slice(), grove_version).map_err(
                                    |e| {
                                        Error::CorruptedData(format!(
                                            "unable to deserialize element: {e}"
                                        ))
                                    }
                                )
                            );
                            if on_disk.is_non_counted() != non_counted {
                                return Err(Error::InvalidInput(
                                    "RefreshReference non_counted flag disagrees with on-disk \
                                     wrapper",
                                ))
                                .wrap_with_cost(cost);
                            }
                            match (mode, on_disk.underlying()) {
                                (
                                    RefreshReferenceMode::PlainReferenceUntrusted,
                                    Element::Reference(..),
                                )
                                | (
                                    RefreshReferenceMode::SumItemReferenceUntrustedNoValueUpdate,
                                    Element::ReferenceWithSumItem(..),
                                ) => on_disk,
                                (
                                    RefreshReferenceMode::SumItemReferenceUntrustedValueUpdate(sum),
                                    Element::ReferenceWithSumItem(
                                        disk_path,
                                        disk_max_hop,
                                        _disk_sum,
                                        disk_flags,
                                    ),
                                ) => {
                                    let rebuilt_inner = Element::ReferenceWithSumItem(
                                        disk_path.clone(),
                                        *disk_max_hop,
                                        sum,
                                        disk_flags.clone(),
                                    );
                                    cost_return_on_error_no_add!(
                                        cost,
                                        wrap_if_non_counted(rebuilt_inner)
                                    )
                                }
                                (RefreshReferenceMode::PlainReferenceUntrusted, _) => {
                                    return Err(Error::InvalidInput(
                                        "RefreshReference PlainReferenceUntrusted applied to \
                                         non-plain-Reference on disk",
                                    ))
                                    .wrap_with_cost(cost);
                                }
                                (
                                    RefreshReferenceMode::SumItemReferenceUntrustedValueUpdate(_),
                                    _,
                                )
                                | (
                                    RefreshReferenceMode::SumItemReferenceUntrustedNoValueUpdate,
                                    _,
                                ) => {
                                    return Err(Error::InvalidInput(
                                        "RefreshReference SumItem-untrusted mode applied to \
                                         non-RefWithSumItem on disk",
                                    ))
                                    .wrap_with_cost(cost);
                                }
                                // Trusted variants are handled in
                                // the outer match and never reach
                                // this point.
                                (
                                    RefreshReferenceMode::PlainReferenceTrusted
                                    | RefreshReferenceMode::SumItemReferenceTrusted(_),
                                    _,
                                ) => unreachable!("trusted modes handled in outer match"),
                            }
                        }
                    };

                    // Mirror the per-merk wrapper invariant enforced
                    // for direct inserts: a NonCounted-wrapped element
                    // may only live in a non-provable count-bearing
                    // parent (`CountTree` or `CountSumTree`). Without
                    // this guard a trusted refresh with
                    // `non_counted=true` could persist a NonCounted
                    // wrapper into a parent that doesn't accept it —
                    // including any `Provable*` count tree where the
                    // count is cryptographically committed.
                    if element.is_non_counted() && !in_tree_type.accepts_non_counted_children() {
                        return Err(Error::InvalidBatchOperation(
                            "RefreshReference with non_counted=true requires a non-provable \
                             count-bearing parent (CountTree or CountSumTree)",
                        ))
                        .wrap_with_cost(cost);
                    }

                    let (path_reference, max_reference_hop) = match element.underlying() {
                        Element::Reference(path, max_hop, _) => (path.clone(), *max_hop),
                        Element::ReferenceWithSumItem(path, max_hop, _, _) => {
                            (path.clone(), *max_hop)
                        }
                        _ => {
                            // Unreachable: branches above always
                            // produce one of these two variants
                            // (possibly NonCounted-wrapped).
                            return Err(Error::InvalidInput(
                                "internal: refresh did not produce a reference variant",
                            ))
                            .wrap_with_cost(cost);
                        }
                    };

                    let merk_feature_type = cost_return_on_error_into!(
                        &mut cost,
                        element
                            .get_feature_type(in_tree_type)
                            .wrap_with_cost(OperationCost::default())
                    );

                    let path_reference = cost_return_on_error_into!(
                        &mut cost,
                        path_from_reference_path_type(
                            path_reference,
                            path,
                            Some(key_info.as_slice())
                        )
                        .wrap_with_cost(OperationCost::default())
                    );
                    if path_reference.is_empty() {
                        return Err(Error::CorruptedReferencePathNotFound(
                            "attempting to refresh an empty reference".to_string(),
                        ))
                        .wrap_with_cost(cost);
                    }

                    let referenced_element_value_hash = cost_return_on_error!(
                        &mut cost,
                        self.follow_reference_get_value_hash(
                            path_reference.as_slice(),
                            ops_by_qualified_paths,
                            max_reference_hop.unwrap_or(MAX_REFERENCE_HOPS as u8),
                            flags_update,
                            split_removal_bytes,
                            &mut HashSet::new(),
                            grove_version
                        )
                    );

                    cost_return_on_error_into!(
                        &mut cost,
                        element.insert_reference_into_batch_operations(
                            key_info.get_key_clone(),
                            referenced_element_value_hash,
                            &mut batch_operations,
                            merk_feature_type,
                            grove_version
                        )
                    );
                }
                GroveOp::Delete => {
                    cost_return_on_error_into!(
                        &mut cost,
                        Element::delete_into_batch_operations(
                            key_info.get_key(),
                            false,
                            in_tree_type, /* we are in a sum tree, this might or might not be a
                                           * sum item */
                            &mut batch_operations,
                            grove_version
                        )
                    );
                }
                GroveOp::DeleteTree(tree_type, _) => {
                    // CountIndexedTree owns two child Merks (primary +
                    // secondary). The standard DeleteTree path runs
                    // find_subtrees on the primary's prefix and clears
                    // each subtree's storage, but it doesn't know about
                    // the cidx secondary's storage namespace
                    // (Blake3(primary_prefix ‖ 0x01)). The dedicated
                    // post-apply secondary cleanup pass that runs in
                    // apply_batch detects cidx primary deletes by
                    // tree_type and clears the secondary prefix there;
                    // here we just emit the merk-level delete the same
                    // way as for any other tree.
                    //
                    // On V4+ the declared type is a checked claim, not
                    // authority: register the key so the old-value observer
                    // can validate the declaration against the stored
                    // element the merk delete surfaces, and capture the
                    // ACTUAL type for the post-apply cleanup-namespace
                    // classification.
                    if grove_version
                        .grovedb_versions
                        .apply_batch
                        .delete_tree_cleanup_type_source
                        >= 1
                    {
                        pending_delete_tree_checks.insert(key_info.get_key_clone(), tree_type);
                    }
                    cost_return_on_error_into!(
                        &mut cost,
                        Element::delete_into_batch_operations(
                            key_info.get_key(),
                            true,
                            in_tree_type, /* use parent tree type, not the deleted subtree's type */
                            &mut batch_operations,
                            grove_version
                        )
                    );
                }
                GroveOp::ReplaceTreeRootKey {
                    hash,
                    root_key,
                    aggregate_data,
                } => {
                    let merk = self.merks.get(path).expect("the Merk is cached");
                    cost_return_on_error!(
                        &mut cost,
                        GroveDb::update_tree_item_preserve_flag_into_batch_operations(
                            merk,
                            key_info.get_key(),
                            root_key,
                            hash,
                            aggregate_data,
                            &mut batch_operations,
                            grove_version
                        )
                    );
                }
                GroveOp::ReplaceNonMerkTreeRoot { hash, meta } => {
                    // Read existing element to preserve flags
                    let merk = self.merks.get(path).expect("the Merk is cached");
                    let existing_flags = cost_return_on_error!(
                        &mut cost,
                        GroveDb::get_element_from_subtree(merk, key_info.as_slice(), grove_version)
                    )
                    .get_flags_owned();

                    let element = meta.to_element(existing_flags);
                    let merk_feature_type = cost_return_on_error_into_no_add!(
                        cost,
                        element.get_feature_type(in_tree_type)
                    );

                    cost_return_on_error_into!(
                        &mut cost,
                        element.insert_subtree_into_batch_operations(
                            key_info.get_key_clone(),
                            hash,
                            true,
                            &mut batch_operations,
                            merk_feature_type,
                            grove_version
                        )
                    );
                }
                GroveOp::InsertTreeWithRootHash {
                    hash,
                    root_key,
                    flags,
                    aggregate_data,
                    non_counted,
                    not_summed,
                    not_counted_or_summed,
                } => {
                    // Standard Merk trees — infer element from aggregate_data
                    let element = match aggregate_data {
                        AggregateData::NoAggregateData => {
                            Element::new_tree_with_flags(root_key, flags)
                        }
                        AggregateData::Sum(sum_value) => {
                            Element::new_sum_tree_with_flags_and_sum_value(
                                root_key, sum_value, flags,
                            )
                        }
                        AggregateData::BigSum(sum_value) => {
                            Element::new_big_sum_tree_with_flags_and_sum_value(
                                root_key, sum_value, flags,
                            )
                        }
                        AggregateData::Count(count_value) => {
                            Element::new_count_tree_with_flags_and_count_value(
                                root_key,
                                count_value,
                                flags,
                            )
                        }
                        AggregateData::CountAndSum(count_value, sum_value) => {
                            Element::new_count_sum_tree_with_flags_and_sum_and_count_value(
                                root_key,
                                count_value,
                                sum_value,
                                flags,
                            )
                        }
                        AggregateData::ProvableCount(count_value) => {
                            Element::new_provable_count_tree_with_flags_and_count_value(
                                root_key,
                                count_value,
                                flags,
                            )
                        }
                        AggregateData::ProvableCountAndSum(count_value, sum_value) => {
                            Element::ProvableCountSumTree(root_key, count_value, sum_value, flags)
                        }
                        AggregateData::ProvableSum(sum_value) => {
                            Element::new_provable_sum_tree_with_flags_and_sum_value(
                                root_key, sum_value, flags,
                            )
                        }
                        AggregateData::ProvableCountAndProvableSum(count_value, sum_value) => {
                            Element::new_provable_count_provable_sum_tree_with_flags_and_sum_and_count_value(
                                root_key,
                                count_value,
                                sum_value,
                                flags,
                            )
                        }
                    };
                    // Re-wrap if the original element was wrapped, so the
                    // on-disk bytes preserve the wrapper and the parent's
                    // aggregate excludes this subtree from the right
                    // dimension. The three flags are mutually exclusive —
                    // set only one during propagation. The `element` here is
                    // a freshly-constructed bare tree built from
                    // `aggregate_data` above, so the conditional wrappers
                    // should never see a pre-existing wrapper input — but
                    // surface a typed error rather than panic if the
                    // invariant is ever violated by a future change.
                    let element = if non_counted {
                        element.into_non_counted().map_err(|_| {
                            Error::CorruptedCodeExecution(
                                "into_non_counted called on a wrapped element during \
                                 InsertTreeWithRootHash propagation",
                            )
                        })
                    } else if not_summed {
                        element.into_not_summed().map_err(|_| {
                            Error::CorruptedCodeExecution(
                                "into_not_summed called on a non-sum-tree or wrapped element \
                                 during InsertTreeWithRootHash propagation",
                            )
                        })
                    } else if not_counted_or_summed {
                        element.into_not_counted_or_summed().map_err(|_| {
                            Error::CorruptedCodeExecution(
                                "into_not_counted_or_summed called on a non-sum-tree or \
                                 wrapped element during InsertTreeWithRootHash propagation",
                            )
                        })
                    } else {
                        Ok(element)
                    };
                    let element = cost_return_on_error_no_add!(cost, element);
                    let merk_feature_type = cost_return_on_error_into_no_add!(
                        cost,
                        element.get_feature_type(in_tree_type)
                    );

                    cost_return_on_error_into!(
                        &mut cost,
                        element.insert_subtree_into_batch_operations(
                            key_info.get_key_clone(),
                            hash,
                            false,
                            &mut batch_operations,
                            merk_feature_type,
                            grove_version
                        )
                    );
                }
                GroveOp::InsertNonMerkTree {
                    hash,
                    flags,
                    meta,
                    non_counted,
                    ..
                } => {
                    let element = meta.to_element(flags);
                    // Re-wrap as above for the non-Merk tree path. `element`
                    // is freshly built from `meta.to_element(...)` so it is
                    // never a pre-existing wrapper — surface a typed error
                    // if a future change ever violates that.
                    let element = if non_counted {
                        let wrapped = element.into_non_counted().map_err(|_| {
                            Error::CorruptedCodeExecution(
                                "into_non_counted called on a wrapped element during \
                                 InsertNonMerkTree propagation",
                            )
                        });
                        cost_return_on_error_no_add!(cost, wrapped)
                    } else {
                        element
                    };
                    let merk_feature_type = cost_return_on_error_into_no_add!(
                        cost,
                        element.get_feature_type(in_tree_type)
                    );

                    cost_return_on_error_into!(
                        &mut cost,
                        element.insert_subtree_into_batch_operations(
                            key_info.get_key_clone(),
                            hash,
                            false,
                            &mut batch_operations,
                            merk_feature_type,
                            grove_version
                        )
                    );
                }
                GroveOp::CommitmentTreeInsert { .. } => {
                    return Err(Error::InvalidBatchOperation(
                        "CommitmentTreeInsert should have been preprocessed before batch execution",
                    ))
                    .wrap_with_cost(cost);
                }
                GroveOp::MmrTreeAppend { .. } => {
                    return Err(Error::InvalidBatchOperation(
                        "MmrTreeAppend should have been preprocessed before batch execution",
                    ))
                    .wrap_with_cost(cost);
                }
                GroveOp::BulkAppend { .. } => {
                    return Err(Error::InvalidBatchOperation(
                        "BulkAppend should have been preprocessed before batch execution",
                    ))
                    .wrap_with_cost(cost);
                }
                GroveOp::DenseTreeInsert { .. } => {
                    return Err(Error::InvalidBatchOperation(
                        "DenseTreeInsert should have been preprocessed before batch execution",
                    ))
                    .wrap_with_cost(cost);
                }
                GroveOp::ReplaceAggregateIndexedTreeRootKeys {
                    primary_hash,
                    primary_root_key,
                    primary_aggregate_data,
                    axes,
                } => {
                    // Bubble-up from an indexed primary's level. The `path`
                    // here is the parent merk where the indexed ELEMENT lives;
                    // key_info points at that element. Recompute its
                    // value_hash via H1-A from the primary's new root hash and
                    // the axes' new state — the single axis's root hash for
                    // PCIT/PSIT, the axes digest for PCPSIT.
                    let merk = self.merks.get(path).expect("the Merk is cached");
                    cost_return_on_error!(
                        &mut cost,
                        GroveDb::update_indexed_tree_item_preserve_flag_into_batch_operations(
                            merk,
                            key_info.get_key(),
                            primary_root_key,
                            axes,
                            primary_aggregate_data,
                            primary_hash,
                            &mut batch_operations,
                            grove_version,
                        )
                    );
                }
                GroveOp::InsertAggregateIndexedTreeRootKeys {
                    element,
                    primary_hash,
                    primary_root_key,
                    primary_aggregate_data,
                    axes,
                } => {
                    // Bubble-up from an indexed primary CREATED in this same
                    // batch: there is no stored element to read, so the op
                    // carries it. Build the parent-merk INSERT with the same
                    // H1-A value hash composition the replace arm uses.
                    cost_return_on_error!(
                        &mut cost,
                        GroveDb::insert_indexed_tree_item_into_batch_operations(
                            element,
                            in_tree_type,
                            key_info.get_key(),
                            primary_root_key,
                            axes,
                            primary_aggregate_data,
                            primary_hash,
                            &mut batch_operations,
                            grove_version,
                        )
                    );
                }
            }
        }

        let merk = self.merks.get_mut(path).expect("the Merk is cached");

        // V4 gate results collected by the old-value observer while the merk
        // apply runs. The observer is infallible, so rejections are stashed
        // here and returned right after the apply — nothing has been
        // committed at that point (the level's writes only live in the
        // pending storage batch, which the caller discards on error).
        let mut old_value_gate_error: Option<Error> = None;
        let mut cidx_overwrite_cleanups: Vec<Vec<Vec<u8>>> = vec![];
        let mut deleted_tree_captures: Vec<(Vec<u8>, TreeType)> = vec![];
        let mut old_value_observer =
            |key: &[u8], old_value: &[u8], disposition: OldValueDisposition| {
                if old_value_gate_error.is_some() {
                    return;
                }
                match disposition {
                    OldValueDisposition::Replaced => {
                        let Some(new_element) = pending_overwrite_inspections.get(key) else {
                            return;
                        };
                        match indexed_tree::classify_cidx_overwrite(
                            old_value,
                            path,
                            key,
                            new_element,
                            ops_by_qualified_paths,
                            grove_version,
                        ) {
                            Ok(Some(cidx_path)) => cidx_overwrite_cleanups.push(cidx_path),
                            Ok(None) => {}
                            Err(e) => old_value_gate_error = Some(e),
                        }
                    }
                    OldValueDisposition::Deleted => {
                        let Some(declared_tree_type) = pending_delete_tree_checks.get(key) else {
                            return;
                        };
                        let outcome = Element::deserialize(old_value, grove_version)
                            .map_err(|_| {
                                Error::CorruptedData(
                                    "unable to deserialize deleted element".to_string(),
                                )
                            })
                            .and_then(|stored_element| {
                                indexed_tree::validate_delete_tree_type(
                                    &stored_element,
                                    *declared_tree_type,
                                )
                            });
                        match outcome {
                            Ok(actual_tree_type) => {
                                deleted_tree_captures.push((key.to_vec(), actual_tree_type))
                            }
                            Err(e) => old_value_gate_error = Some(e),
                        }
                    }
                }
            };

        cost_return_on_error!(
            &mut cost,
            merk.apply_unchecked_with_old_value_observer::<_, Vec<u8>, _, _, _, _, _, _>(
                &batch_operations,
                &[],
                Some(batch_apply_options.as_merk_options()),
                &|key, value| {
                    Element::specialized_costs_for_key_value(
                        key,
                        value,
                        in_tree_type.inner_node_type(),
                        grove_version,
                    )
                    .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
                },
                Some(&Element::value_defined_cost_for_serialized_value),
                &|old_value, new_value| {
                    let old_element = Element::deserialize(old_value.as_slice(), grove_version)
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                    let maybe_old_flags = old_element.get_flags_owned();
                    if maybe_old_flags.is_some() {
                        let mut new_element =
                            Element::deserialize(new_value.as_slice(), grove_version)
                                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                        new_element.set_flags(maybe_old_flags);
                        new_element
                            .serialize(grove_version)
                            .map(Some)
                            .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
                    } else {
                        Ok(None)
                    }
                },
                &mut |storage_costs, old_value, new_value| {
                    // todo: change the flags without full deserialization
                    let old_element = Element::deserialize(old_value.as_slice(), grove_version)
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                    let maybe_old_flags = old_element.get_flags_owned();

                    let mut new_element = Element::deserialize(new_value.as_slice(), grove_version)
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                    let maybe_new_flags = new_element.get_flags_mut();
                    match maybe_new_flags {
                        None => Ok((false, None)),
                        Some(new_flags) => {
                            let changed = (flags_update)(storage_costs, maybe_old_flags, new_flags)
                                .map_err(|e| match e {
                                    Error::JustInTimeElementFlagsClientError(_) => {
                                        MerkError::ClientCorruptionError(e.to_string())
                                    }
                                    _ => MerkError::ClientCorruptionError(
                                        "non client error".to_string(),
                                    ),
                                })?;
                            if changed {
                                let flags_len = new_flags.len() as u32;
                                new_value.clone_from(
                                    &new_element.serialize(grove_version).map_err(|e| {
                                        MerkError::ClientCorruptionError(e.to_string())
                                    })?,
                                );
                                // we need to give back the value defined cost in the case that the
                                // new element is a tree.
                                //
                                // Look through wrapper variants for the cost
                                // path (the wrapper byte costs +1 over the
                                // bare type, mirroring `wrapper_overhead`
                                // in `merk/src/element/costs.rs`).
                                let wrapper_overhead =
                                    if new_element.is_wrapped() { 1u32 } else { 0 };
                                match new_element.underlying() {
                                    Element::Tree(..)
                                    | Element::SumTree(..)
                                    | Element::BigSumTree(..)
                                    | Element::CountTree(..)
                                    | Element::CountSumTree(..)
                                    | Element::ProvableCountTree(..)
                                    | Element::ProvableCountSumTree(..)
                                    | Element::ProvableSumTree(..)
                                    | Element::ProvableCountProvableSumTree(..)
                                    | Element::CommitmentTree(..)
                                    | Element::MmrTree(..)
                                    | Element::BulkAppendTree(..)
                                    | Element::DenseAppendOnlyFixedSizeTree(..)
                                    | Element::ProvableSumIndexedTree(..)
                                    | Element::ProvableCountIndexedTree(..)
                                    | Element::ProvableCountProvableSumIndexedTree(..) => {
                                        let tree_type = new_element
                                            .tree_type()
                                            .expect("tree_type guaranteed by match arm");
                                        let tree_cost_size = tree_type.cost_size();
                                        let tree_value_cost = tree_cost_size
                                            + flags_len
                                            + flags_len.required_space() as u32
                                            + wrapper_overhead;
                                        Ok((true, Some(LayeredValueDefinedCost(tree_value_cost))))
                                    }
                                    Element::SumItem(..) => {
                                        let sum_item_value_cost = SUM_ITEM_COST_SIZE
                                            + flags_len
                                            + flags_len.required_space() as u32
                                            + wrapper_overhead;
                                        Ok((
                                            true,
                                            Some(SpecializedValueDefinedCost(sum_item_value_cost)),
                                        ))
                                    }
                                    Element::ItemWithSumItem(item_value, ..) => {
                                        let item_len = item_value.len() as u32;
                                        let sum_item_value_cost = SUM_ITEM_COST_SIZE
                                            + flags_len
                                            + flags_len.required_space() as u32
                                            + item_len
                                            + item_len.required_space() as u32
                                            + wrapper_overhead;
                                        Ok((
                                            true,
                                            Some(SpecializedValueDefinedCost(sum_item_value_cost)),
                                        ))
                                    }
                                    _ => Ok((true, None)),
                                }
                            } else {
                                Ok((false, None))
                            }
                        }
                    }
                },
                &mut |value, removed_key_bytes, removed_value_bytes| {
                    let mut element = Element::deserialize(value.as_slice(), grove_version)
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                    let maybe_flags = element.get_flags_mut();
                    match maybe_flags {
                        None => Ok((
                            BasicStorageRemoval(removed_key_bytes),
                            BasicStorageRemoval(removed_value_bytes),
                        )),
                        Some(flags) => {
                            (split_removal_bytes)(flags, removed_key_bytes, removed_value_bytes)
                                .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
                        }
                    }
                },
                &mut old_value_observer,
                grove_version,
            )
            .map_err(|e| Error::CorruptedData(e.to_string()))
        );

        // Surface any V4 gate rejection the observer recorded. The apply's
        // cost has been charged (the batch fails as a whole, and none of its
        // storage writes commit), which mirrors how every other mid-apply
        // rejection behaves.
        if let Some(gate_error) = old_value_gate_error {
            return Err(gate_error).wrap_with_cost(cost);
        }
        self.cidx_overwrite_cleanup_paths
            .extend(cidx_overwrite_cleanups);
        for (key, actual_tree_type) in deleted_tree_captures {
            let mut qualified_path = path.to_vec();
            qualified_path.push(key);
            self.deleted_tree_actual_types
                .push((qualified_path, actual_tree_type));
        }

        // Post-apply: if this level was a cidx primary, mirror each
        // mutation to the secondary and capture the secondary's
        // post-mirror state into `cidx_secondary_after_apply` so the
        // bubble-up can emit `ReplaceAggregateIndexedTreeRootKeys`.
        if let Some(pre) = indexed_pre_state {
            // Open every configured axis secondary via the closure (it does the
            // parent merk lookup to learn the axes and their current root
            // keys), then mirror each one from the same captured pre-state.
            // A freshly-created indexed primary has no stored element for the
            // opener to read — the batch op that creates it carries the
            // element, so hand it over. `None` for an existing primary keeps
            // the read-from-parent path. Overwrite-with-descendants is
            // rejected in preflight, so an in-batch insert op at this exact
            // path means the element is fresh.
            let fresh_indexed_element = ops_by_qualified_paths.get(path).and_then(|op| match op {
                GroveOp::InsertOrReplace { element }
                | GroveOp::InsertWithKnownToNotAlreadyExist { element }
                | GroveOp::InsertIfNotExists { element, .. }
                | GroveOp::Replace { element }
                | GroveOp::Patch { element, .. }
                    if element.is_indexed_tree() =>
                {
                    Some(element)
                }
                _ => None,
            });
            let secondaries = cost_return_on_error!(
                &mut cost,
                (self.get_secondary_merks_fn)(path, fresh_indexed_element)
            );
            let primary_merk = self.merks.get(path).expect("the Merk is cached");
            // One post-state read per captured key, shared by every axis: the
            // (count, sum) transitions are axis-independent, only the sort-key
            // encoding differs. Reading inside the per-axis call charged a
            // PCPSIT batch three primary reads per key where one suffices.
            let transitions = cost_return_on_error!(
                &mut cost,
                indexed_tree::read_post_apply_transitions(primary_merk, &pre, grove_version)
            );
            let mut per_axis = Vec::with_capacity(secondaries.len());
            for (axis, mut secondary_merk) in secondaries {
                let (sec_hash, sec_root_key) = cost_return_on_error!(
                    &mut cost,
                    indexed_tree::apply_indexed_secondary_mirror_post_apply(
                        &transitions,
                        axis,
                        &mut secondary_merk,
                        grove_version,
                    )
                );
                per_axis.push((axis.tag(), sec_hash, sec_root_key));
            }
            self.indexed_secondary_after_apply
                .insert(path.to_vec(), per_axis);
        }

        let merk = self.merks.get_mut(path).expect("the Merk is cached");
        merk.root_hash_key_and_aggregate_data()
            .add_cost(cost)
            .map_err(Error::MerkError)
    }

    fn get_batch_run_mode(&self) -> BatchRunMode {
        BatchRunMode::Execute
    }
}

impl GroveDb {
    /// Method to propagate updated subtree root hashes up to GroveDB root
    /// If the stop level is set in the apply options the remaining operations
    /// are returned
    /// Runs the level-by-level batch propagation.
    ///
    /// Returns `(leftover_ops, captures)`:
    ///   - `leftover_ops` is `Some(...)` only if a `batch_pause_height`
    ///     was set and pruning paused before reaching the root.
    ///   - `captures` is the [`BatchApplyCaptures`] collected while the
    ///     body applied, for the caller's post-apply cleanup passes:
    ///     `cidx_overwrite_cleanup_paths` lists the cidx primary paths
    ///     whose old storage (primary subtree + secondary namespaces)
    ///     must be cleared because of a safe-subset cidx-overwrite (see
    ///     `execute_ops_on_path`), and `deleted_tree_actual_types` maps
    ///     each really-deleted `DeleteTree` target to its ACTUAL stored
    ///     tree type so cleanup namespaces follow the truth rather than
    ///     the op's declaration. Both are empty on V1..V3.
    fn apply_batch_structure<C: TreeCache<F, SR>, F, SR>(
        batch_structure: BatchStructure<C, F, SR>,
        batch_apply_options: Option<BatchApplyOptions>,
        grove_version: &GroveVersion,
    ) -> CostResult<(Option<OpsByLevelPath>, BatchApplyCaptures), Error>
    where
        F: FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<bool, Error>,
        SR: FnMut(
            &mut ElementFlags,
            u32,
            u32,
        ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
    {
        check_grovedb_v0_with_cost!(
            "apply_batch_structure",
            grove_version
                .grovedb_versions
                .apply_batch
                .apply_batch_structure
        );
        let mut cost = OperationCost::default();
        let BatchStructure {
            mut ops_by_level_paths,
            ops_by_qualified_paths,
            mut merk_tree_cache,
            mut flags_update,
            mut split_removal_bytes,
            last_level,
        } = batch_structure;
        let mut current_level = last_level;

        let batch_apply_options = batch_apply_options.unwrap_or_default();
        let stop_level = batch_apply_options.batch_pause_height.unwrap_or_default() as u32;

        // We will update up the tree
        while let Some(ops_at_level) = ops_by_level_paths.remove(current_level) {
            for (path, ops_at_path) in ops_at_level.into_iter() {
                if current_level == 0 {
                    // execute the ops at this path
                    // ignoring sum as root tree cannot be summed
                    let (_root_hash, calculated_root_key, _sum) = cost_return_on_error!(
                        &mut cost,
                        merk_tree_cache.execute_ops_on_path(
                            &path,
                            ops_at_path,
                            &ops_by_qualified_paths,
                            &batch_apply_options,
                            &mut flags_update,
                            &mut split_removal_bytes,
                            grove_version,
                        )
                    );
                    if batch_apply_options.base_root_storage_is_free {
                        // the base root is free
                        let mut update_root_cost = cost_return_on_error_no_add!(
                            cost,
                            merk_tree_cache
                                .update_base_merk_root_key(calculated_root_key, grove_version)
                                .cost_as_result()
                        );
                        update_root_cost.storage_cost = StorageCost::default();
                        cost.add_assign(update_root_cost);
                    } else {
                        cost_return_on_error!(
                            &mut cost,
                            merk_tree_cache
                                .update_base_merk_root_key(calculated_root_key, grove_version)
                        );
                    }
                } else {
                    let (root_hash, calculated_root_key, aggregate_data) = cost_return_on_error!(
                        &mut cost,
                        merk_tree_cache.execute_ops_on_path(
                            &path,
                            ops_at_path,
                            &ops_by_qualified_paths,
                            &batch_apply_options,
                            &mut flags_update,
                            &mut split_removal_bytes,
                            grove_version,
                        )
                    );

                    // If the just-finished level was a cidx primary,
                    // pull the post-mirror secondary state from the
                    // side-channel set by execute_ops_on_path so the
                    // bubble-up can carry it.
                    let cidx_secondary_state =
                        merk_tree_cache.take_cidx_secondary_after_apply(&path.to_path());
                    if current_level > 0 {
                        // We need to propagate up this root hash, this means adding grove_db
                        // operations up for the level above
                        if let Some((key, parent_path)) = path.split_last() {
                            if let Some(ops_at_level_above) =
                                ops_by_level_paths.get_mut(current_level - 1)
                            {
                                // todo: fix this hack
                                let parent_path = KeyInfoPath(parent_path.to_vec());
                                if let Some(ops_on_path) = ops_at_level_above.get_mut(&parent_path)
                                {
                                    match ops_on_path.entry(key.clone()) {
                                        Entry::Vacant(vacant_entry) => {
                                            if let Some(axes) = cidx_secondary_state {
                                                vacant_entry.insert(
                                                    GroveOp::ReplaceAggregateIndexedTreeRootKeys {
                                                        primary_hash: root_hash,
                                                        primary_root_key: calculated_root_key,
                                                        primary_aggregate_data: aggregate_data,
                                                        axes,
                                                    },
                                                );
                                            } else {
                                                vacant_entry.insert(GroveOp::ReplaceTreeRootKey {
                                                    hash: root_hash,
                                                    root_key: calculated_root_key,
                                                    aggregate_data,
                                                });
                                            }
                                        }
                                        Entry::Occupied(occupied_entry) => {
                                            let mutable_occupied_entry = occupied_entry.into_mut();
                                            match mutable_occupied_entry {
                                                GroveOp::ReplaceTreeRootKey {
                                                    hash,
                                                    root_key,
                                                    aggregate_data: aggregate_data_entry,
                                                } => {
                                                    if let Some(axes) = cidx_secondary_state {
                                                        // Upgrade to the indexed variant so
                                                        // the parent merk's value_hash
                                                        // is recomputed via H1-A.
                                                        *mutable_occupied_entry =
                                                            GroveOp::ReplaceAggregateIndexedTreeRootKeys {
                                                                primary_hash: root_hash,
                                                                primary_root_key:
                                                                    calculated_root_key,
                                                                primary_aggregate_data:
                                                                    aggregate_data,
                                                                axes,
                                                            };
                                                    } else {
                                                        *hash = root_hash;
                                                        *root_key = calculated_root_key;
                                                        *aggregate_data_entry = aggregate_data;
                                                    }
                                                }
                                                GroveOp::ReplaceNonMerkTreeRoot {
                                                    hash, ..
                                                } => {
                                                    // Non-Merk tree root update: just
                                                    // update the hash, meta is preserved
                                                    // from preprocessing.
                                                    *hash = root_hash;
                                                }
                                                GroveOp::InsertTreeWithRootHash { .. }
                                                | GroveOp::InsertNonMerkTree { .. }
                                                | GroveOp::ReplaceAggregateIndexedTreeRootKeys {
                                                    ..
                                                }
                                                | GroveOp::InsertAggregateIndexedTreeRootKeys {
                                                    ..
                                                } => {
                                                    return Err(Error::CorruptedCodeExecution(
                                                        "we can not do this operation twice",
                                                    ))
                                                    .wrap_with_cost(cost);
                                                }
                                                GroveOp::InsertOrReplace { element }
                                                | GroveOp::InsertWithKnownToNotAlreadyExist {
                                                    element,
                                                }
                                                | GroveOp::InsertIfNotExists { element, .. }
                                                | GroveOp::Replace { element }
                                                | GroveOp::Patch { element, .. } => {
                                                    // Look through wrappers: a wrapped tree
                                                    // still needs to be converted into the
                                                    // appropriate InsertTreeWithRootHash /
                                                    // InsertNonMerkTree variant during
                                                    // upward propagation. Capture wrapper
                                                    // status so execution can re-wrap the
                                                    // reconstructed element — otherwise the
                                                    // wrapper byte would be silently dropped
                                                    // from storage and the parent's aggregate
                                                    // would include a value it should not.
                                                    // The three wrappers are mutually
                                                    // exclusive (constructors reject
                                                    // nesting), so at most one flag is true
                                                    // here.
                                                    let non_counted = element.is_non_counted();
                                                    let not_summed = element.is_not_summed();
                                                    let not_counted_or_summed =
                                                        element.is_not_counted_or_summed();
                                                    let element = element.underlying();
                                                    // Standard Merk trees
                                                    if let Element::Tree(_, flags) = element {
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data:
                                                                    AggregateData::NoAggregateData,
                                                                non_counted,
                                                                not_summed,
                                                                not_counted_or_summed,
                                                            }
                                                    } else if let Element::SumTree(.., flags) =
                                                        element
                                                    {
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                non_counted,
                                                                not_summed,
                                                                not_counted_or_summed,
                                                            }
                                                    } else if let Element::BigSumTree(.., flags) =
                                                        element
                                                    {
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                non_counted,
                                                                not_summed,
                                                                not_counted_or_summed,
                                                            }
                                                    } else if let Element::CountTree(.., flags) =
                                                        element
                                                    {
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                non_counted,
                                                                not_summed,
                                                                not_counted_or_summed,
                                                            }
                                                    } else if let Element::CountSumTree(.., flags) =
                                                        element
                                                    {
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                non_counted,
                                                                not_summed,
                                                                not_counted_or_summed,
                                                            }
                                                    } else if let Element::ProvableCountTree(
                                                        ..,
                                                        flags,
                                                    ) = element
                                                    {
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                non_counted,
                                                                not_summed,
                                                                not_counted_or_summed,
                                                            }
                                                    } else if let Element::ProvableCountSumTree(
                                                        ..,
                                                        flags,
                                                    ) = element
                                                    {
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                non_counted,
                                                                not_summed,
                                                                not_counted_or_summed,
                                                            }
                                                    } else if let Element::ProvableSumTree(
                                                        ..,
                                                        flags,
                                                    ) = element
                                                    {
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                non_counted,
                                                                not_summed,
                                                                not_counted_or_summed,
                                                            }
                                                    } else if let
                                                        Element::ProvableCountProvableSumTree(
                                                            ..,
                                                            flags,
                                                        ) = element
                                                    {
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertTreeWithRootHash {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                non_counted,
                                                                not_summed,
                                                                not_counted_or_summed,
                                                            }
                                                    // Non-Merk trees → InsertNonMerkTree
                                                    // (none of these can be NotSummed or
                                                    // NotCountedOrSummed — they aren't
                                                    // sum-tree variants.)
                                                    } else if let Element::CommitmentTree(
                                                        total_count,
                                                        chunk_power,
                                                        flags,
                                                    ) = element
                                                    {
                                                        let meta = NonMerkTreeMeta::CommitmentTree {
                                                            total_count: *total_count,
                                                            chunk_power: *chunk_power,
                                                        };
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertNonMerkTree {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                meta,
                                                                non_counted,
                                                            }
                                                    } else if let Element::MmrTree(
                                                        mmr_size,
                                                        flags,
                                                    ) = element
                                                    {
                                                        let meta = NonMerkTreeMeta::MmrTree {
                                                            mmr_size: *mmr_size,
                                                        };
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertNonMerkTree {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                meta,
                                                                non_counted,
                                                            }
                                                    } else if let Element::BulkAppendTree(
                                                        total_count,
                                                        chunk_power,
                                                        flags,
                                                    ) = element
                                                    {
                                                        let meta = NonMerkTreeMeta::BulkAppendTree {
                                                            total_count: *total_count,
                                                            chunk_power: *chunk_power,
                                                        };
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertNonMerkTree {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                meta,
                                                                non_counted,
                                                            }
                                                    } else if let
                                                        Element::DenseAppendOnlyFixedSizeTree(
                                                            count,
                                                            height,
                                                            flags,
                                                        ) = element
                                                    {
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertNonMerkTree {
                                                                hash: root_hash,
                                                                root_key: calculated_root_key,
                                                                flags: flags.clone(),
                                                                aggregate_data,
                                                                meta: NonMerkTreeMeta::DenseTree {
                                                                    count: *count,
                                                                    height: *height,
                                                                },
                                                                non_counted,
                                                            }
                                                    // A freshly-inserted indexed primary
                                                    // needs BOTH primary and secondary root
                                                    // state to propagate via the H1-A
                                                    // composition, and there is no stored
                                                    // element to read — so the op carries the
                                                    // caller's element itself alongside the
                                                    // level's computed state. The per-axis
                                                    // state comes from the mirror this level
                                                    // just ran (the fresh secondaries were
                                                    // opened from the in-batch element).
                                                    } else if matches!(
                                                        element,
                                                        Element::ProvableSumIndexedTree(..)
                                                            | Element::ProvableCountIndexedTree(..)
                                                            | Element::ProvableCountProvableSumIndexedTree(..)
                                                    ) {
                                                        if non_counted
                                                            || not_summed
                                                            || not_counted_or_summed
                                                        {
                                                            return Err(Error::InvalidBatchOperation(
                                                                "indexed-tree elements cannot be \
                                                                 wrapped in NonCounted / NotSummed \
                                                                 / NotCountedOrSummed",
                                                            ))
                                                            .wrap_with_cost(cost);
                                                        }
                                                        // The real TreeCache always reports
                                                        // per-axis state for an indexed level;
                                                        // only the worst-case ESTIMATOR cache
                                                        // cannot (its layer information carries
                                                        // no tree type), and estimation never
                                                        // applies the op — the apply arm's
                                                        // empty-axes check guards the real path.
                                                        *mutable_occupied_entry =
                                                            GroveOp::InsertAggregateIndexedTreeRootKeys {
                                                                element: element.clone(),
                                                                primary_hash: root_hash,
                                                                primary_root_key:
                                                                    calculated_root_key,
                                                                primary_aggregate_data:
                                                                    aggregate_data,
                                                                axes: cidx_secondary_state
                                                                    .unwrap_or_default(),
                                                            };
                                                    } else {
                                                        return Err(Error::InvalidBatchOperation(
                                                            "insertion of element under a non tree",
                                                        ))
                                                        .wrap_with_cost(cost);
                                                    }
                                                }
                                                GroveOp::RefreshReference { .. } => {
                                                    return Err(Error::InvalidBatchOperation(
                                                        "insertion of element under a refreshed \
                                                         reference",
                                                    ))
                                                    .wrap_with_cost(cost);
                                                }
                                                GroveOp::Delete | GroveOp::DeleteTree(..) => {
                                                    if calculated_root_key.is_some() {
                                                        return Err(Error::InvalidBatchOperation(
                                                            "modification of tree when it will be \
                                                             deleted",
                                                        ))
                                                        .wrap_with_cost(cost);
                                                    }
                                                }
                                                GroveOp::CommitmentTreeInsert { .. } => {
                                                    return Err(Error::InvalidBatchOperation(
                                                        "CommitmentTree ops should have been \
                                                         preprocessed",
                                                    ))
                                                    .wrap_with_cost(cost);
                                                }
                                                GroveOp::MmrTreeAppend { .. } => {
                                                    return Err(Error::InvalidBatchOperation(
                                                        "MmrTree ops should have been preprocessed",
                                                    ))
                                                    .wrap_with_cost(cost);
                                                }
                                                GroveOp::BulkAppend { .. } => {
                                                    return Err(Error::InvalidBatchOperation(
                                                        "BulkAppend ops should have been \
                                                         preprocessed",
                                                    ))
                                                    .wrap_with_cost(cost);
                                                }
                                                GroveOp::DenseTreeInsert { .. } => {
                                                    return Err(Error::InvalidBatchOperation(
                                                        "DenseTreeInsert ops should have been \
                                                         preprocessed",
                                                    ))
                                                    .wrap_with_cost(cost);
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    let mut ops_on_path: BTreeMap<KeyInfo, GroveOp> =
                                        BTreeMap::new();
                                    let new_op = if let Some(axes) = cidx_secondary_state {
                                        GroveOp::ReplaceAggregateIndexedTreeRootKeys {
                                            primary_hash: root_hash,
                                            primary_root_key: calculated_root_key,
                                            primary_aggregate_data: aggregate_data,
                                            axes,
                                        }
                                    } else {
                                        GroveOp::ReplaceTreeRootKey {
                                            hash: root_hash,
                                            root_key: calculated_root_key,
                                            aggregate_data,
                                        }
                                    };
                                    ops_on_path.insert(key.clone(), new_op);
                                    ops_at_level_above.insert(parent_path, ops_on_path);
                                }
                            } else {
                                let mut ops_on_path: BTreeMap<KeyInfo, GroveOp> = BTreeMap::new();
                                let new_op = if let Some(axes) = cidx_secondary_state {
                                    GroveOp::ReplaceAggregateIndexedTreeRootKeys {
                                        primary_hash: root_hash,
                                        primary_root_key: calculated_root_key,
                                        primary_aggregate_data: aggregate_data,
                                        axes,
                                    }
                                } else {
                                    GroveOp::ReplaceTreeRootKey {
                                        hash: root_hash,
                                        root_key: calculated_root_key,
                                        aggregate_data,
                                    }
                                };
                                ops_on_path.insert(key.clone(), new_op);
                                let mut ops_on_level: BTreeMap<
                                    KeyInfoPath,
                                    BTreeMap<KeyInfo, GroveOp>,
                                > = BTreeMap::new();
                                ops_on_level.insert(KeyInfoPath(parent_path.to_vec()), ops_on_path);
                                ops_by_level_paths.insert(current_level - 1, ops_on_level);
                            }
                        }
                    }
                }
            }
            if current_level == stop_level {
                // we need to pause the batch execution
                let captures = BatchApplyCaptures {
                    cidx_overwrite_cleanup_paths: merk_tree_cache
                        .take_cidx_overwrite_cleanup_paths(),
                    deleted_tree_actual_types: merk_tree_cache.take_deleted_tree_actual_types(),
                };
                return Ok((Some(ops_by_level_paths), captures)).wrap_with_cost(cost);
            }
            current_level = current_level.saturating_sub(1);
        }
        let captures = BatchApplyCaptures {
            cidx_overwrite_cleanup_paths: merk_tree_cache.take_cidx_overwrite_cleanup_paths(),
            deleted_tree_actual_types: merk_tree_cache.take_deleted_tree_actual_types(),
        };
        Ok((None, captures)).wrap_with_cost(cost)
    }

    /// Method to propagate updated subtree root hashes up to GroveDB root
    /// If the pause height is set in the batch apply options
    /// Then return the list of leftover operations
    fn apply_body<'db, S: StorageContext<'db>>(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        update_element_flags_function: impl FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> Result<bool, Error>,
        split_removed_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        get_merk_fn: impl FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
        get_secondary_merks_fn: impl FnMut(
            &[Vec<u8>],
            Option<&Element>,
        ) -> CostResult<
            Vec<(grovedb_element::indexed::IndexAxis, Merk<S>)>,
            Error,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<(Option<OpsByLevelPath>, BatchApplyCaptures), Error> {
        check_grovedb_v0_with_cost!(
            "apply_body",
            grove_version.grovedb_versions.apply_batch.apply_body
        );
        let mut cost = OperationCost::default();
        let batch_structure = cost_return_on_error!(
            &mut cost,
            BatchStructure::from_ops(
                ops,
                update_element_flags_function,
                split_removed_bytes_function,
                TreeCacheMerkByPath {
                    merks: Default::default(),
                    get_merk_fn,
                    get_secondary_merks_fn,
                    indexed_secondary_after_apply: Default::default(),
                    cidx_overwrite_cleanup_paths: Default::default(),
                    deleted_tree_actual_types: Default::default(),
                }
            )
        );
        Self::apply_batch_structure(batch_structure, batch_apply_options, grove_version)
            .add_cost(cost)
    }

    /// Method to propagate updated subtree root hashes up to GroveDB root
    /// If the pause height is set in the batch apply options
    /// Then return the list of leftover operations
    fn continue_partial_apply_body<'db, S: StorageContext<'db>>(
        &self,
        previous_leftover_operations: Option<OpsByLevelPath>,
        additional_ops: Vec<QualifiedGroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        update_element_flags_function: impl FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> Result<bool, Error>,
        split_removed_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        get_merk_fn: impl FnMut(&[Vec<u8>], bool) -> CostResult<Merk<S>, Error>,
        get_secondary_merks_fn: impl FnMut(
            &[Vec<u8>],
            Option<&Element>,
        ) -> CostResult<
            Vec<(grovedb_element::indexed::IndexAxis, Merk<S>)>,
            Error,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<(Option<OpsByLevelPath>, BatchApplyCaptures), Error> {
        check_grovedb_v0_with_cost!(
            "continue_partial_apply_body",
            grove_version
                .grovedb_versions
                .apply_batch
                .continue_partial_apply_body
        );
        let mut cost = OperationCost::default();
        let batch_structure = cost_return_on_error!(
            &mut cost,
            BatchStructure::continue_from_ops(
                previous_leftover_operations,
                additional_ops,
                update_element_flags_function,
                split_removed_bytes_function,
                TreeCacheMerkByPath {
                    merks: Default::default(),
                    get_merk_fn,
                    get_secondary_merks_fn,
                    indexed_secondary_after_apply: Default::default(),
                    cidx_overwrite_cleanup_paths: Default::default(),
                    deleted_tree_actual_types: Default::default(),
                }
            )
        );
        Self::apply_batch_structure(batch_structure, batch_apply_options, grove_version)
            .add_cost(cost)
    }

    /// Applies operations on GroveDB one at a time, without batching.
    ///
    /// # Warning -- not atomic
    ///
    /// Unlike [`apply_batch`](Self::apply_batch), this method processes each
    /// operation individually and applies its side-effects to the current
    /// storage context immediately. If an operation in the middle of the list
    /// fails, all preceding operations will have already been applied and
    /// **will not be rolled back** within this method. (Note: when a
    /// `transaction` is supplied, the caller can still roll back the entire
    /// transaction; the non-atomicity refers to the inability to undo
    /// *individual* operations within the list.)
    /// This means:
    ///
    /// * The storage context may be left in a partially-updated state on
    ///   failure.
    /// * Root hashes may differ from the result of applying the same
    ///   operations via `apply_batch`, because batch application propagates
    ///   root hashes in a single pass whereas this method updates trees
    ///   one-by-one.
    ///
    /// Use this method **only** for testing, debugging, or situations where
    /// partial application is explicitly acceptable. For production workloads
    /// that require atomicity, use [`apply_batch`](Self::apply_batch) instead.
    pub fn apply_operations_without_batching(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        options: Option<BatchApplyOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "apply_operations_without_batching",
            grove_version
                .grovedb_versions
                .apply_batch
                .apply_operations_without_batching
        );
        let mut cost = OperationCost::default();
        for op in ops.into_iter() {
            match op.op {
                GroveOp::InsertOrReplace { element } | GroveOp::Replace { element } => {
                    // TODO: paths in batches is something to think about
                    let path_slices: Vec<&[u8]> =
                        op.path.iterator().map(|p| p.as_slice()).collect();
                    let key = cost_return_on_error_no_add!(
                        cost,
                        op.key
                            .as_ref()
                            .ok_or(Error::InvalidBatchOperation("insert op is missing a key"))
                    );
                    cost_return_on_error!(
                        &mut cost,
                        self.insert(
                            path_slices.as_slice(),
                            key.as_slice(),
                            element.to_owned(),
                            options.clone().map(|o| o.as_insert_options()),
                            transaction,
                            grove_version,
                        )
                    );
                }
                GroveOp::InsertWithKnownToNotAlreadyExist { element } => {
                    let path_slices: Vec<&[u8]> =
                        op.path.iterator().map(|p| p.as_slice()).collect();
                    let key = cost_return_on_error_no_add!(
                        cost,
                        op.key.as_ref().ok_or(Error::InvalidBatchOperation(
                            "insert_only_known_to_not_already_exist op is missing a key",
                        ))
                    );
                    cost_return_on_error!(
                        &mut cost,
                        self.insert(
                            path_slices.as_slice(),
                            key.as_slice(),
                            element.to_owned(),
                            options.clone().map(|o| o.as_insert_options()),
                            transaction,
                            grove_version,
                        )
                    );
                }
                GroveOp::InsertIfNotExists {
                    element,
                    error_if_exists,
                } => {
                    let path_slices: Vec<&[u8]> =
                        op.path.iterator().map(|p| p.as_slice()).collect();
                    let key = cost_return_on_error_no_add!(
                        cost,
                        op.key.as_ref().ok_or(Error::InvalidBatchOperation(
                            "insert_if_not_exists op is missing a key",
                        ))
                    );
                    if error_if_exists {
                        let mut insert_options = options
                            .clone()
                            .map(|o| o.as_insert_options())
                            .unwrap_or_default();
                        insert_options.validate_insertion_does_not_override = true;
                        cost_return_on_error!(
                            &mut cost,
                            self.insert(
                                path_slices.as_slice(),
                                key.as_slice(),
                                element.to_owned(),
                                Some(insert_options),
                                transaction,
                                grove_version,
                            )
                        );
                    } else {
                        cost_return_on_error!(
                            &mut cost,
                            self.insert_if_not_exists(
                                path_slices.as_slice(),
                                key.as_slice(),
                                element.to_owned(),
                                transaction,
                                grove_version,
                            )
                        );
                    }
                }
                GroveOp::Delete => {
                    let path_slices: Vec<&[u8]> =
                        op.path.iterator().map(|p| p.as_slice()).collect();
                    let key = cost_return_on_error_no_add!(
                        cost,
                        op.key
                            .as_ref()
                            .ok_or(Error::InvalidBatchOperation("delete op is missing a key"))
                    );
                    cost_return_on_error!(
                        &mut cost,
                        self.delete(
                            path_slices.as_slice(),
                            key.as_slice(),
                            options.clone().map(|o| o.as_delete_options()),
                            transaction,
                            grove_version
                        )
                    );
                }
                GroveOp::DeleteTree(_, subelements_deletion_behavior) => {
                    let path_slices: Vec<&[u8]> =
                        op.path.iterator().map(|p| p.as_slice()).collect();
                    let key = cost_return_on_error_no_add!(
                        cost,
                        op.key
                            .as_ref()
                            .ok_or(Error::InvalidBatchOperation("delete op is missing a key"))
                    );
                    // Map the per-op enum to the lower-level DeleteOptions.
                    // DontCheckWithNoCleanup and DeleteChildren both set
                    // allow_deleting_non_empty_trees = true because the
                    // single-op `delete()` already performs recursive child
                    // subtree cleanup when that flag is true.  Skip maps to
                    // allow=false + error=false, which makes `delete()`
                    // silently return Ok(false) for non-empty trees.
                    let delete_options = DeleteOptions {
                        allow_deleting_non_empty_trees: matches!(
                            subelements_deletion_behavior,
                            SubelementsDeletionBehavior::DontCheckWithNoCleanup
                                | SubelementsDeletionBehavior::DeleteChildren
                        ),
                        deleting_non_empty_trees_returns_error: matches!(
                            subelements_deletion_behavior,
                            SubelementsDeletionBehavior::Error
                        ),
                        base_root_storage_is_free: options
                            .as_ref()
                            .is_none_or(|o| o.base_root_storage_is_free),
                        validate_tree_at_path_exists: false,
                    };
                    cost_return_on_error!(
                        &mut cost,
                        self.delete(
                            path_slices.as_slice(),
                            key.as_slice(),
                            Some(delete_options),
                            transaction,
                            grove_version
                        )
                    );
                }
                GroveOp::CommitmentTreeInsert {
                    cmx,
                    rho,
                    cv_net,
                    payload,
                } => {
                    let mut path_vec: Vec<Vec<u8>> = op.path.to_path();
                    let key = cost_return_on_error_no_add!(
                        cost,
                        path_vec.pop().ok_or(Error::InvalidBatchOperation(
                            "append op path must include tree key"
                        ))
                    );
                    let path_slices: Vec<&[u8]> = path_vec.iter().map(|p| p.as_slice()).collect();
                    cost_return_on_error!(
                        &mut cost,
                        self.commitment_tree_insert_raw(
                            path_slices.as_slice(),
                            &key,
                            cmx,
                            rho,
                            cv_net,
                            payload.clone(),
                            transaction,
                            grove_version,
                        )
                    );
                }
                GroveOp::MmrTreeAppend { value } => {
                    let mut path_vec: Vec<Vec<u8>> = op.path.to_path();
                    let key = cost_return_on_error_no_add!(
                        cost,
                        path_vec.pop().ok_or(Error::InvalidBatchOperation(
                            "append op path must include tree key"
                        ))
                    );
                    let path_slices: Vec<&[u8]> = path_vec.iter().map(|p| p.as_slice()).collect();
                    cost_return_on_error!(
                        &mut cost,
                        self.mmr_tree_append(
                            path_slices.as_slice(),
                            &key,
                            value.clone(),
                            transaction,
                            grove_version,
                        )
                    );
                }
                GroveOp::BulkAppend { value } => {
                    let mut path_vec: Vec<Vec<u8>> = op.path.to_path();
                    let key = cost_return_on_error_no_add!(
                        cost,
                        path_vec.pop().ok_or(Error::InvalidBatchOperation(
                            "append op path must include tree key"
                        ))
                    );
                    let path_slices: Vec<&[u8]> = path_vec.iter().map(|p| p.as_slice()).collect();
                    cost_return_on_error!(
                        &mut cost,
                        self.bulk_append(
                            path_slices.as_slice(),
                            &key,
                            value.clone(),
                            transaction,
                            grove_version,
                        )
                    );
                }
                GroveOp::DenseTreeInsert { value } => {
                    let mut path_vec: Vec<Vec<u8>> = op.path.to_path();
                    let key = cost_return_on_error_no_add!(
                        cost,
                        path_vec.pop().ok_or(Error::InvalidBatchOperation(
                            "append op path must include tree key"
                        ))
                    );
                    let path_slices: Vec<&[u8]> = path_vec.iter().map(|p| p.as_slice()).collect();
                    cost_return_on_error!(
                        &mut cost,
                        self.dense_tree_insert(
                            path_slices.as_slice(),
                            &key,
                            value.clone(),
                            transaction,
                            grove_version,
                        )
                    );
                }
                GroveOp::Patch { .. } | GroveOp::RefreshReference { .. } => {
                    return Err(Error::NotSupported(
                        "Patch and RefreshReference are batch-only operations".to_string(),
                    ))
                    .wrap_with_cost(cost);
                }
                GroveOp::ReplaceTreeRootKey { .. }
                | GroveOp::InsertTreeWithRootHash { .. }
                | GroveOp::ReplaceNonMerkTreeRoot { .. }
                | GroveOp::InsertNonMerkTree { .. }
                | GroveOp::ReplaceAggregateIndexedTreeRootKeys { .. }
                | GroveOp::InsertAggregateIndexedTreeRootKeys { .. } => {
                    return Err(Error::NotSupported(
                        "internal tree ops not supported in apply_operations_without_batching"
                            .to_string(),
                    ))
                    .wrap_with_cost(cost);
                }
            }
        }
        Ok(()).wrap_with_cost(cost)
    }

    /// Applies batch on GroveDB
    pub fn apply_batch(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "apply_batch",
            grove_version.grovedb_versions.apply_batch.apply_batch
        );
        self.apply_batch_with_element_flags_update(
            ops,
            batch_apply_options,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
            transaction,
            grove_version,
        )
    }

    /// Applies batch on GroveDB
    pub fn apply_partial_batch(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        cost_based_add_on_operations: impl FnMut(
            &OperationCost,
            &Option<OpsByLevelPath>,
        ) -> Result<Vec<QualifiedGroveDbOp>, Error>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "apply_partial_batch",
            grove_version
                .grovedb_versions
                .apply_batch
                .apply_partial_batch
        );
        self.apply_partial_batch_with_element_flags_update(
            ops,
            batch_apply_options,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
            cost_based_add_on_operations,
            transaction,
            grove_version,
        )
    }

    /// Opens transactional merk at path with given storage batch context.
    /// Returns CostResult.
    pub fn open_batch_transactional_merk_at_path<'db, B: AsRef<[u8]>>(
        &'db self,
        storage_batch: &'db StorageBatch,
        path: SubtreePath<B>,
        tx: &'db Transaction,
        new_merk: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error> {
        check_grovedb_v0_with_cost!(
            "open_batch_transactional_merk_at_path",
            grove_version
                .grovedb_versions
                .apply_batch
                .open_batch_transactional_merk_at_path
        );
        let mut cost = OperationCost::default();
        let storage = self
            .db
            .get_transactional_storage_context(path.clone(), Some(storage_batch), tx)
            .unwrap_add_cost(&mut cost);

        if let Some((parent_path, parent_key)) = path.derive_parent() {
            if new_merk {
                // TODO: can this be a sum tree
                Ok(Merk::open_empty(
                    storage,
                    MerkType::LayeredMerk,
                    TreeType::NormalTree,
                ))
                .wrap_with_cost(cost)
            } else {
                let parent_storage = self
                    .db
                    .get_transactional_storage_context(parent_path.clone(), Some(storage_batch), tx)
                    .unwrap_add_cost(&mut cost);
                let element = cost_return_on_error!(
                    &mut cost,
                    Element::get_from_storage(&parent_storage, parent_key, grove_version).map_err(
                        |_| {
                            Error::InvalidPath(format!(
                                "could not get key for parent of subtree for batch at path [{}] \
                                 for key {}",
                                parent_path
                                    .to_vec()
                                    .into_iter()
                                    .map(|v| hex_to_ascii(&v))
                                    .join("/"),
                                hex_to_ascii(parent_key)
                            ))
                        }
                    )
                );
                if let Some((root_key, tree_type)) = element.root_key_and_tree_type_owned() {
                    Merk::open_layered_with_root_key(
                        storage,
                        root_key,
                        tree_type,
                        Some(&Element::value_defined_cost_for_serialized_value),
                        grove_version,
                    )
                    .map_err(|e| {
                        Error::CorruptedData(format!(
                            "cannot open a subtree with given root key: {e}"
                        ))
                    })
                    .add_cost(cost)
                } else {
                    Err(Error::CorruptedPath(
                        "cannot open a subtree as parent exists but is not a tree".to_string(),
                    ))
                    .wrap_with_cost(OperationCost::default())
                }
            }
        } else if new_merk {
            Ok(Merk::open_empty(
                storage,
                MerkType::BaseMerk,
                TreeType::NormalTree,
            ))
            .wrap_with_cost(cost)
        } else {
            Merk::open_base(
                storage,
                TreeType::NormalTree,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| Error::CorruptedData(format!("cannot open the root subtree: {e}")))
            .add_cost(cost)
        }
    }

    /// Like [`open_batch_transactional_merk_at_path`]
    /// (Self::open_batch_transactional_merk_at_path) with `new_merk: false`,
    /// for a caller that has ALREADY read (and paid for) the parent element
    /// at `path`: the open's own parent fetch is skipped so the read is
    /// charged exactly once. Only valid for non-root paths.
    fn open_batch_transactional_merk_with_parent_element<'db, B: AsRef<[u8]>>(
        &'db self,
        storage_batch: &'db StorageBatch,
        path: SubtreePath<B>,
        tx: &'db Transaction,
        parent_element: Element,
        grove_version: &GroveVersion,
    ) -> CostResult<Merk<PrefixedRocksDbTransactionContext<'db>>, Error> {
        let mut cost = OperationCost::default();
        if path.derive_parent().is_none() {
            return Err(Error::CorruptedCodeExecution(
                "open_batch_transactional_merk_with_parent_element requires a non-root path",
            ))
            .wrap_with_cost(cost);
        }
        let storage = self
            .db
            .get_transactional_storage_context(path, Some(storage_batch), tx)
            .unwrap_add_cost(&mut cost);
        if let Some((root_key, tree_type)) = parent_element.root_key_and_tree_type_owned() {
            Merk::open_layered_with_root_key(
                storage,
                root_key,
                tree_type,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| {
                Error::CorruptedData(format!("cannot open a subtree with given root key: {e}"))
            })
            .add_cost(cost)
        } else {
            Err(Error::CorruptedPath(
                "cannot open a subtree as parent exists but is not a tree".to_string(),
            ))
            .wrap_with_cost(cost)
        }
    }

    /// Pre-apply scan over a batch's `DeleteTree` ops, shared by
    /// `apply_batch_with_element_flags_update` and
    /// `apply_partial_batch_with_element_flags_update`.
    ///
    /// On V1..V3 this is the released behaviour verbatim: the DECLARED tree
    /// type is taken at face value, driving both the emptiness checks and
    /// the pre-apply classification of cleanup paths.
    ///
    /// On V4+ (`delete_tree_cleanup_type_source >= 1`) cleanup namespaces
    /// must follow the ACTUAL stored type instead — but reading the stored
    /// element here would add a charged read per op, which is exactly what
    /// this gate used to cost. So classification moves to after
    /// `apply_body`, driven by the old element bytes the merk delete
    /// surfaces for free through the old-value observer (which also rejects
    /// declared/stored mismatches involving an indexed tree). The only work
    /// left here is what must happen before the apply: the Error/Skip
    /// emptiness checks. Those already read the stored element on V1..V3 —
    /// directly for declared non-merk types, or inside the child-merk open
    /// for merk types — so V4 does that single read up front, derives the
    /// ACTUAL type from it, and hands the element to the open. Same read
    /// count, same bytes: V4 charges exactly the released cost.
    fn scan_delete_tree_ops<'db>(
        &'db self,
        ops: &[QualifiedGroveDbOp],
        storage_batch: &'db StorageBatch,
        tx: &'db Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<DeleteTreePreScan, Error> {
        let mut cost = OperationCost::default();
        let mut scan = DeleteTreePreScan::default();
        let capture_actual_types = grove_version
            .grovedb_versions
            .apply_batch
            .delete_tree_cleanup_type_source
            >= 1;
        for op in ops.iter() {
            if let GroveOp::DeleteTree(tree_type, subelements_deletion_behavior) = &op.op
                && let Some(key) = op.key.as_ref()
            {
                let mut child_path = op.path.to_path();
                child_path.push(key.as_slice().to_vec());

                if capture_actual_types {
                    scan.delete_tree_behaviors
                        .insert(child_path.clone(), *subelements_deletion_behavior);
                    match subelements_deletion_behavior {
                        SubelementsDeletionBehavior::DontCheckWithNoCleanup
                        | SubelementsDeletionBehavior::DeleteChildren => {
                            // Nothing to check pre-apply; cleanup paths come
                            // from the captured actual types after apply.
                        }
                        SubelementsDeletionBehavior::Error | SubelementsDeletionBehavior::Skip => {
                            let parent_path_vec = op.path.to_path();
                            let parent_path: SubtreePath<Vec<u8>> =
                                parent_path_vec.as_slice().into();
                            let parent_storage = self
                                .db
                                .get_transactional_storage_context(
                                    parent_path,
                                    Some(storage_batch),
                                    tx,
                                )
                                .unwrap_add_cost(&mut cost);
                            let stored_element = cost_return_on_error!(
                                &mut cost,
                                Element::get_from_storage(
                                    &parent_storage,
                                    key.as_slice(),
                                    grove_version,
                                )
                                .map_err(|e| {
                                    Error::CorruptedData(format!(
                                        "unable to get element for delete tree emptiness \
                                         check: {e}"
                                    ))
                                })
                            );
                            let actual_tree_type = cost_return_on_error_no_add!(
                                cost,
                                indexed_tree::validate_delete_tree_type(
                                    &stored_element,
                                    *tree_type
                                )
                            );
                            let is_empty = if actual_tree_type.uses_non_merk_data_storage() {
                                // Non-Merk trees: element-level entry count,
                                // read off the element loaded above.
                                stored_element.non_merk_entry_count().unwrap_or(0) == 0
                            } else {
                                // Standard Merk trees: use is_empty_tree_except
                                // to account for other delete ops in the same
                                // batch.
                                //
                                // Exclude DeleteTree ops with Skip policy —
                                // those might not execute if their target is
                                // non-empty, so we cannot assume they will
                                // delete their key.
                                let batch_deleted_keys = ops
                                    .iter()
                                    .filter_map(|other_op| match &other_op.op {
                                        GroveOp::Delete => {
                                            if other_op.path.to_path() == child_path {
                                                Some(other_op.key.as_ref()?.as_slice().to_vec())
                                            } else {
                                                None
                                            }
                                        }
                                        GroveOp::DeleteTree(
                                            _,
                                            SubelementsDeletionBehavior::Skip,
                                        ) => None,
                                        GroveOp::DeleteTree(..) => {
                                            if other_op.path.to_path() == child_path {
                                                Some(other_op.key.as_ref()?.as_slice().to_vec())
                                            } else {
                                                None
                                            }
                                        }
                                        _ => None,
                                    })
                                    .collect::<Vec<Vec<u8>>>();
                                let batch_deleted_keys_refs: std::collections::BTreeSet<&[u8]> =
                                    batch_deleted_keys.iter().map(|k| k.as_slice()).collect();

                                let child_merk = cost_return_on_error!(
                                    &mut cost,
                                    self.open_batch_transactional_merk_with_parent_element(
                                        storage_batch,
                                        child_path.as_slice().into(),
                                        tx,
                                        stored_element,
                                        grove_version,
                                    )
                                );

                                child_merk
                                    .is_empty_tree_except(batch_deleted_keys_refs)
                                    .unwrap_add_cost(&mut cost)
                            };

                            if !is_empty {
                                match subelements_deletion_behavior {
                                    SubelementsDeletionBehavior::Error => {
                                        return Err(Error::DeletingNonEmptyTree(
                                            "trying to do a batch delete operation for a non \
                                             empty tree, but options not allowing this",
                                        ))
                                        .wrap_with_cost(cost);
                                    }
                                    SubelementsDeletionBehavior::Skip => {
                                        scan.skipped_delete_paths.insert(child_path);
                                    }
                                    SubelementsDeletionBehavior::DontCheckWithNoCleanup
                                    | SubelementsDeletionBehavior::DeleteChildren => {
                                        return Err(Error::CorruptedCodeExecution(
                                            "batch delete: DontCheckWithNoCleanup / \
                                             DeleteChildren behaviors are handled before the \
                                             non-empty-tree check and must not reach this \
                                             match arm",
                                        ))
                                        .wrap_with_cost(cost);
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }

                // V1..V3: released behaviour, byte for byte — the declared
                // tree type is taken at face value.
                //
                // Per-op emptiness check based on the
                // SubelementsDeletionBehavior policy.
                match subelements_deletion_behavior {
                    SubelementsDeletionBehavior::DontCheckWithNoCleanup => {
                        // No emptiness check and no post-apply storage cleanup.
                        // The caller guarantees the subtree is already empty.
                        // Cidx still needs the secondary cleared even when
                        // primary cleanup is skipped, because the cidx's
                        // secondary metadata lives in a different
                        // namespace and is invisible to find_subtrees.
                        // is_indexed_primary() (not is_count_indexed_primary):
                        // PSIT and PCPSIT DeleteTree ops must also queue their
                        // primary path for the all-axis secondary sweep below,
                        // otherwise their sum/avg secondary namespaces survive
                        // the DeleteTree. The sweep clears all three axis tags
                        // unconditionally, so this is correct for every variant.
                        if tree_type.is_indexed_primary() {
                            scan.cidx_primary_delete_paths.push(child_path);
                        }
                        continue;
                    }
                    SubelementsDeletionBehavior::DeleteChildren => {
                        // No emptiness check, but still perform post-apply
                        // storage cleanup to remove child subtree storage.
                    }
                    SubelementsDeletionBehavior::Error | SubelementsDeletionBehavior::Skip => {
                        let is_empty = if tree_type.uses_non_merk_data_storage() {
                            // Non-Merk trees: check element-level entry count.
                            let parent_path_vec = op.path.to_path();
                            let parent_path: SubtreePath<Vec<u8>> =
                                parent_path_vec.as_slice().into();
                            let parent_storage = self
                                .db
                                .get_transactional_storage_context(
                                    parent_path,
                                    Some(storage_batch),
                                    tx,
                                )
                                .unwrap_add_cost(&mut cost);
                            let element = cost_return_on_error!(
                                &mut cost,
                                Element::get_from_storage(
                                    &parent_storage,
                                    key.as_slice(),
                                    grove_version,
                                )
                                .map_err(|e| {
                                    Error::CorruptedData(format!(
                                        "unable to get element for delete tree emptiness \
                                         check: {e}"
                                    ))
                                })
                            );
                            element.non_merk_entry_count().unwrap_or(0) == 0
                        } else {
                            // Standard Merk trees: use is_empty_tree_except to
                            // account for other delete ops in the same batch.
                            //
                            // Exclude DeleteTree ops with Skip policy — those
                            // might not execute if their target is non-empty,
                            // so we cannot assume they will delete their key.
                            let batch_deleted_keys = ops
                                .iter()
                                .filter_map(|other_op| match &other_op.op {
                                    GroveOp::Delete => {
                                        if other_op.path.to_path() == child_path {
                                            Some(other_op.key.as_ref()?.as_slice().to_vec())
                                        } else {
                                            None
                                        }
                                    }
                                    GroveOp::DeleteTree(_, SubelementsDeletionBehavior::Skip) => {
                                        None
                                    }
                                    GroveOp::DeleteTree(..) => {
                                        if other_op.path.to_path() == child_path {
                                            Some(other_op.key.as_ref()?.as_slice().to_vec())
                                        } else {
                                            None
                                        }
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<Vec<u8>>>();
                            let batch_deleted_keys_refs: std::collections::BTreeSet<&[u8]> =
                                batch_deleted_keys.iter().map(|k| k.as_slice()).collect();

                            let child_merk = cost_return_on_error!(
                                &mut cost,
                                self.open_batch_transactional_merk_at_path(
                                    storage_batch,
                                    child_path.as_slice().into(),
                                    tx,
                                    false,
                                    grove_version,
                                )
                            );

                            child_merk
                                .is_empty_tree_except(batch_deleted_keys_refs)
                                .unwrap_add_cost(&mut cost)
                        };

                        if !is_empty {
                            match subelements_deletion_behavior {
                                SubelementsDeletionBehavior::Error => {
                                    return Err(Error::DeletingNonEmptyTree(
                                        "trying to do a batch delete operation for a non \
                                         empty tree, but options not allowing this",
                                    ))
                                    .wrap_with_cost(cost);
                                }
                                SubelementsDeletionBehavior::Skip => {
                                    scan.skipped_delete_paths.insert(child_path);
                                    continue;
                                }
                                // DontCheckWithNoCleanup / DeleteChildren never
                                // reach the emptiness-check block above (they
                                // either skip the check or delete children
                                // unconditionally). Return a graceful error
                                // rather than panicking if that invariant is
                                // ever broken.
                                SubelementsDeletionBehavior::DontCheckWithNoCleanup
                                | SubelementsDeletionBehavior::DeleteChildren => {
                                    return Err(Error::CorruptedCodeExecution(
                                        "batch delete: DontCheckWithNoCleanup / DeleteChildren \
                                         behaviors are handled before the non-empty-tree check \
                                         and must not reach this match arm",
                                    ))
                                    .wrap_with_cost(cost);
                                }
                            }
                        }
                    }
                }

                if tree_type.uses_non_merk_data_storage() {
                    scan.non_merk_delete_paths.push(child_path);
                } else {
                    // is_indexed_primary(): PSIT/PCPSIT primaries also need
                    // their path queued for the all-axis secondary sweep.
                    if tree_type.is_indexed_primary() {
                        scan.cidx_primary_delete_paths.push(child_path.clone());
                    }
                    scan.merk_delete_paths.push(child_path);
                }
            }
        }
        Ok(scan).wrap_with_cost(cost)
    }

    /// Applies batch of operations on GroveDB
    pub fn apply_batch_with_element_flags_update(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        update_element_flags_function: impl FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> Result<bool, Error>,
        split_removal_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "apply_batch_with_element_flags_update",
            grove_version
                .grovedb_versions
                .apply_batch
                .apply_batch_with_element_flags_update
        );
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
        }

        // Check batch operation consistency BEFORE preprocessing so that
        // conflicting ops (e.g., CommitmentTreeInsert + Delete on the same
        // path/key) are caught before any work is done.
        let check_batch_operation_consistency = batch_apply_options
            .as_ref()
            .map(|batch_options| !batch_options.disable_operation_consistency_check)
            .unwrap_or(true);

        if check_batch_operation_consistency {
            let consistency_result = QualifiedGroveDbOp::verify_consistency_of_operations(&ops);
            if !consistency_result.is_empty() {
                return Err(Error::InvalidBatchOperation(
                    "batch operations fail consistency checks",
                ))
                .wrap_with_cost(cost);
            }
        }

        cost_return_on_error!(
            &mut cost,
            indexed_tree::reject_indexed_overwrite_with_descendants(
                self,
                &ops,
                tx.as_ref(),
                grove_version,
            )
        );

        // `StorageBatch` collects all operations (preprocessing + apply_body)
        // for a single atomic commit at the end.
        let storage_batch = StorageBatch::new();

        // Preprocess CommitmentTreeInsert ops: execute Sinsemilla operations
        // then convert to ReplaceTreeRootKey ops
        let ops = cost_return_on_error!(
            &mut cost,
            self.preprocess_commitment_tree_ops(ops, tx.as_ref(), &storage_batch, grove_version)
        );

        // Preprocess MmrTreeAppend ops: execute MMR operations
        // then convert to ReplaceTreeRootKey ops
        let ops = cost_return_on_error!(
            &mut cost,
            self.preprocess_mmr_tree_ops(ops, tx.as_ref(), &storage_batch, grove_version)
        );

        // Preprocess BulkAppend ops: execute bulk append operations
        // then convert to ReplaceTreeRootKey ops
        let ops = cost_return_on_error!(
            &mut cost,
            self.preprocess_bulk_append_ops(ops, tx.as_ref(), &storage_batch, grove_version)
        );

        // Preprocess DenseTreeInsert ops: execute dense tree operations
        // then convert to ReplaceTreeRootKey ops
        let ops = cost_return_on_error!(
            &mut cost,
            self.preprocess_dense_tree_ops(ops, tx.as_ref(), &storage_batch, grove_version)
        );

        // Collect paths of subtrees being deleted (so their storage can be
        // cleaned up after apply_body) and run the pre-apply emptiness
        // checks / Skip filtering. On V1..V3 the cleanup lists are filled
        // here from the DECLARED tree types; on V4+ they stay empty and are
        // filled after apply_body from the ACTUAL stored types captured by
        // the merk old-value observer. See `scan_delete_tree_ops`.
        let DeleteTreePreScan {
            mut non_merk_delete_paths,
            mut merk_delete_paths,
            mut cidx_primary_delete_paths,
            skipped_delete_paths,
            delete_tree_behaviors,
        } = cost_return_on_error!(
            &mut cost,
            self.scan_delete_tree_ops(&ops, &storage_batch, tx.as_ref(), grove_version)
        );

        // Filter out DeleteTree ops that were skipped due to
        // SubelementsDeletionBehavior::Skip on non-empty trees.
        let ops = if !skipped_delete_paths.is_empty() {
            ops.into_iter()
                .filter(|op| {
                    if let GroveOp::DeleteTree(..) = &op.op
                        && let Some(key) = op.key.as_ref()
                    {
                        let mut child_path = op.path.to_path();
                        child_path.push(key.as_slice().to_vec());
                        return !skipped_delete_paths.contains(&child_path);
                    }
                    true
                })
                .collect()
        } else {
            ops
        };

        // With the only one difference (if there is a transaction) do the following:
        // 2. If nothing left to do and we were on a non-leaf subtree or we're done with
        //    one subtree and moved to another then add propagation operation to the
        //    operations tree and drop Merk handle;
        // 3. Take Merk from temp subtrees or open a new one with batched storage_cost
        //    context;
        // 4. Apply operation to the Merk;
        // 5. Remove operation from the tree, repeat until there are operations to do;
        // 6. Add root leaves save operation to the batch
        // 7. Apply storage_cost batch
        let (_leftover, batch_apply_captures) = cost_return_on_error!(
            &mut cost,
            self.apply_body(
                ops,
                batch_apply_options,
                update_element_flags_function,
                split_removal_bytes_function,
                |path, new_merk| {
                    self.open_batch_transactional_merk_at_path(
                        &storage_batch,
                        path.into(),
                        tx.as_ref(),
                        new_merk,
                        grove_version,
                    )
                },
                |primary_path: &[Vec<u8>], fresh_element: Option<&Element>| {
                    let primary_refs: Vec<&[u8]> =
                        primary_path.iter().map(|v| v.as_slice()).collect();
                    let cidx_path: SubtreePath<&[u8]> = primary_refs.as_slice().into();
                    self.open_indexed_secondaries_for_batch(
                        cidx_path,
                        fresh_element,
                        &storage_batch,
                        tx.as_ref(),
                        grove_version,
                    )
                },
                grove_version
            )
        );

        let BatchApplyCaptures {
            cidx_overwrite_cleanup_paths,
            deleted_tree_actual_types,
        } = batch_apply_captures;

        // V4+: fold the `(path, ACTUAL stored type)` pairs captured during
        // the apply into the cleanup lists (no-op on V1..V3, where the
        // captures are empty and the lists were already built pre-apply from
        // the declared types).
        classify_captured_delete_trees(
            deleted_tree_actual_types,
            &delete_tree_behaviors,
            &mut non_merk_delete_paths,
            &mut merk_delete_paths,
            &mut cidx_primary_delete_paths,
        );

        // Clean up data storage for deleted non-Merk trees.
        for child_path in &non_merk_delete_paths {
            let child_subtree_path: SubtreePath<Vec<u8>> = child_path.as_slice().into();
            // Clear data namespace for all non-Merk tree types
            let mut storage = self
                .db
                .get_transactional_storage_context(
                    child_subtree_path,
                    Some(&storage_batch),
                    tx.as_ref(),
                )
                .unwrap_add_cost(&mut cost);
            cost_return_on_error!(
                &mut cost,
                storage.clear().map_err(|e| {
                    Error::CorruptedData(format!(
                        "unable to clean up non-merk tree data in batch delete: {e}",
                    ))
                })
            );
        }

        // Clean up storage for deleted standard Merk subtrees.
        // The parent key has been removed from the parent Merk by apply_body,
        // but the child subtree's storage (and any nested subtrees) remains.
        // We use find_subtrees to recursively discover all nested subtrees
        // and clear their storage, matching the non-batch delete behavior.
        //
        // NOTE: find_subtrees reads from the committed transaction state
        // (without the pending storage_batch), so any subtrees *inserted*
        // by this same batch are invisible to it.  This is safe because
        // verify_consistency_of_operations (enabled by default) rejects
        // batches that insert under a path being deleted.  If the caller
        // disables the consistency check, inserts under deleted paths can
        // cause orphaned storage prefixes.  See the doc comment on
        // BatchApplyOptions::disable_operation_consistency_check.
        for child_path in &merk_delete_paths {
            let child_subtree_path: SubtreePath<Vec<u8>> = child_path.as_slice().into();
            let subtrees_paths = cost_return_on_error!(
                &mut cost,
                self.find_subtrees(&child_subtree_path, Some(tx.as_ref()), grove_version)
            );
            for subtree_path in subtrees_paths {
                let p: SubtreePath<_> = subtree_path.as_slice().into();
                let mut storage = self
                    .db
                    .get_transactional_storage_context(p, Some(&storage_batch), tx.as_ref())
                    .unwrap_add_cost(&mut cost);
                cost_return_on_error!(
                    &mut cost,
                    storage.clear().map_err(|e| {
                        Error::CorruptedData(format!(
                            "unable to clean up merk subtree storage in batch delete: {e}",
                        ))
                    })
                );
            }
        }

        // Indexed-tree secondary cleanup. find_subtrees walks the
        // primary's storage namespace via path-derived prefixes and
        // does not see the per-axis secondaries at
        // Blake3(primary_prefix ‖ axis_tag). Clear all three axis
        // tags unconditionally so the secondary data does not survive
        // a DeleteTree on its primary regardless of which
        // indexed-tree variant the primary was. (Clears on empty
        // namespaces are no-ops, so the sweep is safe.)
        for primary_path in &cidx_primary_delete_paths {
            let cidx_subtree_path: SubtreePath<Vec<u8>> = primary_path.as_slice().into();
            let primary_prefix =
                grovedb_storage::rocksdb_storage::RocksDbStorage::build_prefix(cidx_subtree_path)
                    .unwrap_add_cost(&mut cost);
            for axis in [
                grovedb_element::indexed::IndexAxis::Count,
                grovedb_element::indexed::IndexAxis::Sum,
                grovedb_element::indexed::IndexAxis::Avg,
            ] {
                let secondary_prefix =
                    grovedb_storage::rocksdb_storage::RocksDbStorage::secondary_prefix_for(
                        &primary_prefix,
                        axis.tag(),
                    )
                    .unwrap_add_cost(&mut cost);
                let mut secondary_storage = self
                    .db
                    .get_transactional_storage_context_by_subtree_prefix(
                        secondary_prefix,
                        Some(&storage_batch),
                        tx.as_ref(),
                    )
                    .unwrap_add_cost(&mut cost);
                cost_return_on_error!(
                    &mut cost,
                    secondary_storage.clear().map_err(|e| {
                        Error::CorruptedData(format!(
                            "unable to clean up indexed-tree secondary (axis {:?}) storage \
                             in batch delete: {e}",
                            axis
                        ))
                    })
                );
            }
        }

        // Cidx safe-subset OVERWRITE cleanup. When a batch op replaced
        // an existing cidx element with a non-cidx element or an empty
        // cidx, the OLD cidx's primary subtree storage AND secondary
        // namespace must be cleared. Cleanup paths were collected by
        // execute_ops_on_path and surfaced via apply_body's tuple
        // return. Each path is the cidx primary's full path
        // (parent_path + cidx_key).
        for cidx_path in &cidx_overwrite_cleanup_paths {
            let cidx_subtree_path: SubtreePath<Vec<u8>> = cidx_path.as_slice().into();
            // Clear all primary subtree storage recursively via
            // find_subtrees (same walk as DeleteTree cleanup above).
            let subtrees_paths = cost_return_on_error!(
                &mut cost,
                self.find_subtrees(&cidx_subtree_path, Some(tx.as_ref()), grove_version)
            );
            for subtree_path in subtrees_paths {
                let p: SubtreePath<_> = subtree_path.as_slice().into();
                let mut storage = self
                    .db
                    .get_transactional_storage_context(p, Some(&storage_batch), tx.as_ref())
                    .unwrap_add_cost(&mut cost);
                cost_return_on_error!(
                    &mut cost,
                    storage.clear().map_err(|e| {
                        Error::CorruptedData(format!(
                            "unable to clean up cidx primary subtree storage in batch \
                             overwrite: {e}",
                        ))
                    })
                );
            }
            // Clear the per-axis secondary namespaces at
            // Blake3(primary ‖ axis_tag). Sweep all three axes — clear
            // on empty is a no-op, so this also works for PCIT-only
            // overwrites (the sum / avg slots are empty).
            let primary_prefix = grovedb_storage::rocksdb_storage::RocksDbStorage::build_prefix(
                cidx_subtree_path.clone(),
            )
            .unwrap_add_cost(&mut cost);
            for axis in [
                grovedb_element::indexed::IndexAxis::Count,
                grovedb_element::indexed::IndexAxis::Sum,
                grovedb_element::indexed::IndexAxis::Avg,
            ] {
                let secondary_prefix =
                    grovedb_storage::rocksdb_storage::RocksDbStorage::secondary_prefix_for(
                        &primary_prefix,
                        axis.tag(),
                    )
                    .unwrap_add_cost(&mut cost);
                let mut secondary_storage = self
                    .db
                    .get_transactional_storage_context_by_subtree_prefix(
                        secondary_prefix,
                        Some(&storage_batch),
                        tx.as_ref(),
                    )
                    .unwrap_add_cost(&mut cost);
                cost_return_on_error!(
                    &mut cost,
                    secondary_storage.clear().map_err(|e| {
                        Error::CorruptedData(format!(
                            "unable to clean up indexed-tree secondary (axis {:?}) storage \
                             in batch overwrite: {e}",
                            axis
                        ))
                    })
                );
            }
        }

        // TODO: compute batch costs
        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(storage_batch, Some(tx.as_ref()))
                .map_err(|e| e.into())
        );

        // Keep this commented for easy debugging in the future.
        // let issues = self
        //     .visualize_verify_grovedb(Some(tx), true,
        // &Default::default())     .unwrap();
        // if issues.len() > 0 {
        //     println!(
        //         "tx_issues: {}",
        //         issues
        //             .iter()
        //             .map(|(hash, (a, b, c))| format!("{}: {} {} {}",
        // hash, a, b, c))             .collect::<Vec<_>>()
        //             .join(" | ")
        //     );
        // }

        tx.commit_local().wrap_with_cost(cost)
    }

    /// Applies a partial batch of operations on GroveDB
    /// The batch is not committed
    /// Clients should set the Batch Apply Options batch pause height
    /// If it is not set we default to pausing at the root tree
    pub fn apply_partial_batch_with_element_flags_update(
        &self,
        ops: Vec<QualifiedGroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        mut update_element_flags_function: impl FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> Result<bool, Error>,
        mut split_removal_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        mut add_on_operations: impl FnMut(
            &OperationCost,
            &Option<OpsByLevelPath>,
        ) -> Result<Vec<QualifiedGroveDbOp>, Error>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "apply_partial_batch_with_element_flags_update",
            grove_version
                .grovedb_versions
                .apply_batch
                .apply_partial_batch_with_element_flags_update
        );
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
        }

        // Check batch operation consistency BEFORE preprocessing so that
        // conflicting ops (e.g., CommitmentTreeInsert + Delete on the same
        // path/key) are caught before any work is done.
        let check_batch_operation_consistency = batch_apply_options
            .as_ref()
            .map(|batch_options| !batch_options.disable_operation_consistency_check)
            .unwrap_or(true);

        if check_batch_operation_consistency {
            let consistency_result = QualifiedGroveDbOp::verify_consistency_of_operations(&ops);
            if !consistency_result.is_empty() {
                return Err(Error::InvalidBatchOperation(
                    "batch operations fail consistency checks",
                ))
                .wrap_with_cost(cost);
            }
        }

        cost_return_on_error!(
            &mut cost,
            indexed_tree::reject_indexed_overwrite_with_descendants(
                self,
                &ops,
                tx.as_ref(),
                grove_version,
            )
        );

        // `StorageBatch` collects all operations (preprocessing + apply_body)
        // for a single atomic commit at the end.
        let storage_batch = StorageBatch::new();

        // Preprocess CommitmentTreeInsert ops
        let ops = cost_return_on_error!(
            &mut cost,
            self.preprocess_commitment_tree_ops(ops, tx.as_ref(), &storage_batch, grove_version)
        );

        // Preprocess MmrTreeAppend ops
        let ops = cost_return_on_error!(
            &mut cost,
            self.preprocess_mmr_tree_ops(ops, tx.as_ref(), &storage_batch, grove_version)
        );

        // Preprocess BulkAppend ops
        let ops = cost_return_on_error!(
            &mut cost,
            self.preprocess_bulk_append_ops(ops, tx.as_ref(), &storage_batch, grove_version)
        );

        // Preprocess DenseTreeInsert ops
        let ops = cost_return_on_error!(
            &mut cost,
            self.preprocess_dense_tree_ops(ops, tx.as_ref(), &storage_batch, grove_version)
        );

        let mut batch_apply_options = batch_apply_options.unwrap_or_default();

        // Collect paths of subtrees being deleted (so their storage can be
        // cleaned up after apply_body) and run the pre-apply emptiness
        // checks / Skip filtering. On V1..V3 the cleanup lists are filled
        // here from the DECLARED tree types; on V4+ they stay empty and are
        // filled after apply_body from the ACTUAL stored types captured by
        // the merk old-value observer. See `scan_delete_tree_ops`.
        let DeleteTreePreScan {
            mut non_merk_delete_paths,
            mut merk_delete_paths,
            mut cidx_primary_delete_paths,
            skipped_delete_paths,
            delete_tree_behaviors,
        } = cost_return_on_error!(
            &mut cost,
            self.scan_delete_tree_ops(&ops, &storage_batch, tx.as_ref(), grove_version)
        );

        // Filter out DeleteTree ops that were skipped due to
        // SubelementsDeletionBehavior::Skip on non-empty trees.
        let ops = if !skipped_delete_paths.is_empty() {
            ops.into_iter()
                .filter(|op| {
                    if let GroveOp::DeleteTree(..) = &op.op
                        && let Some(key) = op.key.as_ref()
                    {
                        let mut child_path = op.path.to_path();
                        child_path.push(key.as_slice().to_vec());
                        return !skipped_delete_paths.contains(&child_path);
                    }
                    true
                })
                .collect()
        } else {
            ops
        };
        if batch_apply_options.batch_pause_height.is_none() {
            // we default to pausing at the root tree, which is the most common case
            batch_apply_options.batch_pause_height = Some(1);
        }

        // With the only one difference (if there is a transaction) do the following:
        // 2. If nothing left to do and we were on a non-leaf subtree or we're done with
        //    one subtree and moved to another then add propagation operation to the
        //    operations tree and drop Merk handle;
        // 3. Take Merk from temp subtrees or open a new one with batched storage_cost
        //    context;
        // 4. Apply operation to the Merk;
        // 5. Remove operation from the tree, repeat until there are operations to do;
        // 6. Add root leaves save operation to the batch
        // 7. Apply storage_cost batch
        let (left_over_operations, partial_captures) = cost_return_on_error!(
            &mut cost,
            self.apply_body(
                ops,
                Some(batch_apply_options.clone()),
                &mut update_element_flags_function,
                &mut split_removal_bytes_function,
                |path, new_merk| {
                    self.open_batch_transactional_merk_at_path(
                        &storage_batch,
                        path.into(),
                        tx.as_ref(),
                        new_merk,
                        grove_version,
                    )
                },
                |primary_path: &[Vec<u8>], fresh_element: Option<&Element>| {
                    let primary_refs: Vec<&[u8]> =
                        primary_path.iter().map(|v| v.as_slice()).collect();
                    let cidx_path: SubtreePath<&[u8]> = primary_refs.as_slice().into();
                    self.open_indexed_secondaries_for_batch(
                        cidx_path,
                        fresh_element,
                        &storage_batch,
                        tx.as_ref(),
                        grove_version,
                    )
                },
                grove_version,
            )
        );
        // if we paused at the root height, the left over operations would be to replace
        // a lot of leaf nodes in the root tree

        // let's build the write batch
        let (mut write_batch, mut pending_costs) = cost_return_on_error!(
            &mut cost,
            self.db
                .build_write_batch(storage_batch)
                .map_err(|e| e.into())
        );

        let total_current_costs = cost.clone().add(pending_costs.clone());

        // todo: estimate root costs

        // at this point we need to send the pending costs back
        // we will get GroveDB a new set of GroveDBOps

        let new_operations = cost_return_on_error_no_add!(
            cost,
            add_on_operations(&total_current_costs, &left_over_operations)
        );

        // Validate the add-on operations for consistency. The callback is
        // caller-provided, so the returned operations could contain duplicates,
        // internal-only ops, or inserts under paths being deleted. Apply the
        // same consistency gate used for the initial batch.
        //
        // Limitation: add-on DeleteTree ops bypass the
        // SubelementsDeletionBehavior preflight (emptiness check, Skip
        // filtering, cleanup path collection) that runs on the initial
        // batch.  They go straight into continue_partial_apply_body →
        // apply_body, where DeleteTree is a simple layered Merk delete
        // with no emptiness enforcement.  In practice this is safe because
        // partial-batch callers (Platform) control the callback and only
        // return root-level propagation ops, not new DeleteTree ops.  If
        // add-on DeleteTree support is needed in the future, the preflight
        // must be extended to cover new_operations as well.
        if check_batch_operation_consistency && !new_operations.is_empty() {
            let consistency_result =
                QualifiedGroveDbOp::verify_consistency_of_operations(&new_operations);
            if !consistency_result.is_empty() {
                return Err(Error::InvalidBatchOperation(
                    "add-on operations from callback fail consistency checks",
                ))
                .wrap_with_cost(cost);
            }
        }

        // we are trying to finalize
        batch_apply_options.batch_pause_height = None;

        let continue_storage_batch = StorageBatch::new();

        let (_leftover_unused, continue_captures) = cost_return_on_error!(
            &mut cost,
            self.continue_partial_apply_body(
                left_over_operations,
                new_operations,
                Some(batch_apply_options),
                update_element_flags_function,
                split_removal_bytes_function,
                |path, new_merk| {
                    self.open_batch_transactional_merk_at_path(
                        &continue_storage_batch,
                        path.into(),
                        tx.as_ref(),
                        new_merk,
                        grove_version,
                    )
                },
                |primary_path: &[Vec<u8>], fresh_element: Option<&Element>| {
                    let primary_refs: Vec<&[u8]> =
                        primary_path.iter().map(|v| v.as_slice()).collect();
                    let cidx_path: SubtreePath<&[u8]> = primary_refs.as_slice().into();
                    self.open_indexed_secondaries_for_batch(
                        cidx_path,
                        fresh_element,
                        &continue_storage_batch,
                        tx.as_ref(),
                        grove_version,
                    )
                },
                grove_version
            )
        );

        let BatchApplyCaptures {
            cidx_overwrite_cleanup_paths: partial_cidx_overwrite_cleanup_paths,
            deleted_tree_actual_types: partial_deleted_tree_actual_types,
        } = partial_captures;
        let BatchApplyCaptures {
            cidx_overwrite_cleanup_paths: continue_cidx_overwrite_cleanup_paths,
            deleted_tree_actual_types: continue_deleted_tree_actual_types,
        } = continue_captures;

        // V4+: fold captures from BOTH applies into the cleanup lists
        // (no-op on V1..V3). The overwrite-cleanup paths are unioned below.
        classify_captured_delete_trees(
            partial_deleted_tree_actual_types
                .into_iter()
                .chain(continue_deleted_tree_actual_types)
                .collect(),
            &delete_tree_behaviors,
            &mut non_merk_delete_paths,
            &mut merk_delete_paths,
            &mut cidx_primary_delete_paths,
        );

        // Clean up data storage for deleted non-Merk trees.
        for child_path in &non_merk_delete_paths {
            let child_subtree_path: SubtreePath<Vec<u8>> = child_path.as_slice().into();
            let mut storage = self
                .db
                .get_transactional_storage_context(
                    child_subtree_path,
                    Some(&continue_storage_batch),
                    tx.as_ref(),
                )
                .unwrap_add_cost(&mut cost);
            cost_return_on_error!(
                &mut cost,
                storage.clear().map_err(|e| {
                    Error::CorruptedData(format!(
                        "unable to clean up non-merk tree data in batch delete: {e}",
                    ))
                })
            );
        }

        // Clean up storage for deleted standard Merk subtrees (same as
        // apply_batch_with_element_flags_update).
        //
        // NOTE: find_subtrees reads from the committed transaction state
        // (without the pending storage_batch), so any subtrees *inserted*
        // by this same batch are invisible to it.  This is safe because
        // verify_consistency_of_operations (enabled by default) rejects
        // batches that insert under a path being deleted.  If the caller
        // disables the consistency check, inserts under deleted paths can
        // cause orphaned storage prefixes.  See the doc comment on
        // BatchApplyOptions::disable_operation_consistency_check.
        for child_path in &merk_delete_paths {
            let child_subtree_path: SubtreePath<Vec<u8>> = child_path.as_slice().into();
            let subtrees_paths = cost_return_on_error!(
                &mut cost,
                self.find_subtrees(&child_subtree_path, Some(tx.as_ref()), grove_version)
            );
            for subtree_path in subtrees_paths {
                let p: SubtreePath<_> = subtree_path.as_slice().into();
                let mut storage = self
                    .db
                    .get_transactional_storage_context(
                        p,
                        Some(&continue_storage_batch),
                        tx.as_ref(),
                    )
                    .unwrap_add_cost(&mut cost);
                cost_return_on_error!(
                    &mut cost,
                    storage.clear().map_err(|e| {
                        Error::CorruptedData(format!(
                            "unable to clean up merk subtree storage in batch delete: {e}",
                        ))
                    })
                );
            }
        }

        // Indexed-tree secondary cleanup (parallels the
        // apply_batch_with_element_flags_update pass). Sweep all
        // three axes.
        for primary_path in &cidx_primary_delete_paths {
            let cidx_subtree_path: SubtreePath<Vec<u8>> = primary_path.as_slice().into();
            let primary_prefix =
                grovedb_storage::rocksdb_storage::RocksDbStorage::build_prefix(cidx_subtree_path)
                    .unwrap_add_cost(&mut cost);
            for axis in [
                grovedb_element::indexed::IndexAxis::Count,
                grovedb_element::indexed::IndexAxis::Sum,
                grovedb_element::indexed::IndexAxis::Avg,
            ] {
                let secondary_prefix =
                    grovedb_storage::rocksdb_storage::RocksDbStorage::secondary_prefix_for(
                        &primary_prefix,
                        axis.tag(),
                    )
                    .unwrap_add_cost(&mut cost);
                let mut secondary_storage = self
                    .db
                    .get_transactional_storage_context_by_subtree_prefix(
                        secondary_prefix,
                        Some(&continue_storage_batch),
                        tx.as_ref(),
                    )
                    .unwrap_add_cost(&mut cost);
                cost_return_on_error!(
                    &mut cost,
                    secondary_storage.clear().map_err(|e| {
                        Error::CorruptedData(format!(
                            "unable to clean up indexed-tree secondary (axis {:?}) storage \
                             in batch delete: {e}",
                            axis
                        ))
                    })
                );
            }
        }

        // Cidx safe-subset OVERWRITE cleanup (parallels the
        // apply_batch_with_element_flags_update pass). Two passes
        // contribute paths: the initial apply_body and the
        // continue_partial_apply_body. Union them.
        let all_cidx_overwrite_paths: Vec<&Vec<Vec<u8>>> = partial_cidx_overwrite_cleanup_paths
            .iter()
            .chain(continue_cidx_overwrite_cleanup_paths.iter())
            .collect();
        for cidx_path in all_cidx_overwrite_paths {
            let cidx_subtree_path: SubtreePath<Vec<u8>> = cidx_path.as_slice().into();
            let subtrees_paths = cost_return_on_error!(
                &mut cost,
                self.find_subtrees(&cidx_subtree_path, Some(tx.as_ref()), grove_version)
            );
            for subtree_path in subtrees_paths {
                let p: SubtreePath<_> = subtree_path.as_slice().into();
                let mut storage = self
                    .db
                    .get_transactional_storage_context(
                        p,
                        Some(&continue_storage_batch),
                        tx.as_ref(),
                    )
                    .unwrap_add_cost(&mut cost);
                cost_return_on_error!(
                    &mut cost,
                    storage.clear().map_err(|e| {
                        Error::CorruptedData(format!(
                            "unable to clean up cidx primary subtree storage in batch \
                             overwrite: {e}",
                        ))
                    })
                );
            }
            let primary_prefix = grovedb_storage::rocksdb_storage::RocksDbStorage::build_prefix(
                cidx_subtree_path.clone(),
            )
            .unwrap_add_cost(&mut cost);
            for axis in [
                grovedb_element::indexed::IndexAxis::Count,
                grovedb_element::indexed::IndexAxis::Sum,
                grovedb_element::indexed::IndexAxis::Avg,
            ] {
                let secondary_prefix =
                    grovedb_storage::rocksdb_storage::RocksDbStorage::secondary_prefix_for(
                        &primary_prefix,
                        axis.tag(),
                    )
                    .unwrap_add_cost(&mut cost);
                let mut secondary_storage = self
                    .db
                    .get_transactional_storage_context_by_subtree_prefix(
                        secondary_prefix,
                        Some(&continue_storage_batch),
                        tx.as_ref(),
                    )
                    .unwrap_add_cost(&mut cost);
                cost_return_on_error!(
                    &mut cost,
                    secondary_storage.clear().map_err(|e| {
                        Error::CorruptedData(format!(
                            "unable to clean up indexed-tree secondary (axis {:?}) storage \
                             in batch overwrite: {e}",
                            axis
                        ))
                    })
                );
            }
        }

        // let's build the write batch
        let continued_pending_costs = cost_return_on_error!(
            &mut cost,
            self.db
                .continue_write_batch(&mut write_batch, continue_storage_batch)
                .map_err(|e| e.into())
        );

        pending_costs.add_assign(continued_pending_costs);

        // TODO: compute batch costs
        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_db_write_batch(write_batch, pending_costs, Some(tx.as_ref()))
                .map_err(|e| e.into())
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    #[cfg(feature = "estimated_costs")]
    /// Returns the estimated average or worst case cost for an entire batch of
    /// ops
    pub fn estimated_case_operations_for_batch(
        estimated_costs_type: EstimatedCostsType,
        ops: Vec<QualifiedGroveDbOp>,
        batch_apply_options: Option<BatchApplyOptions>,
        update_element_flags_function: impl FnMut(
            &StorageCost,
            Option<ElementFlags>,
            &mut ElementFlags,
        ) -> Result<bool, Error>,
        split_removal_bytes_function: impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        check_grovedb_v0_with_cost!(
            "estimated_case_operations_for_batch",
            grove_version
                .grovedb_versions
                .apply_batch
                .estimated_case_operations_for_batch
        );
        let mut cost = OperationCost::default();

        if ops.is_empty() {
            return Ok(()).wrap_with_cost(cost);
        }

        match estimated_costs_type {
            EstimatedCostsType::AverageCaseCostsType(estimated_layer_information) => {
                let batch_structure = cost_return_on_error!(
                    &mut cost,
                    BatchStructure::from_ops(
                        ops,
                        update_element_flags_function,
                        split_removal_bytes_function,
                        AverageCaseTreeCacheKnownPaths::new_with_estimated_layer_information(
                            estimated_layer_information
                        )
                    )
                );
                cost_return_on_error!(
                    &mut cost,
                    Self::apply_batch_structure(
                        batch_structure,
                        batch_apply_options,
                        grove_version
                    )
                );
            }

            EstimatedCostsType::WorstCaseCostsType(worst_case_layer_information) => {
                let batch_structure = cost_return_on_error!(
                    &mut cost,
                    BatchStructure::from_ops(
                        ops,
                        update_element_flags_function,
                        split_removal_bytes_function,
                        WorstCaseTreeCacheKnownPaths::new_with_worst_case_layer_information(
                            worst_case_layer_information
                        )
                    )
                );
                cost_return_on_error!(
                    &mut cost,
                    Self::apply_batch_structure(
                        batch_structure,
                        batch_apply_options,
                        grove_version
                    )
                );
            }
        }

        Ok(()).wrap_with_cost(cost)
    }
}

#[cfg(test)]
mod tests {
    use grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;
    use grovedb_merk::proofs::Query;

    use super::*;
    use crate::{
        reference_path::ReferencePathType,
        tests::{
            common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF,
        },
        PathQuery,
    };

    #[test]
    fn test_batch_validation_ok() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"key2".to_vec(),
                element2.clone(),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("cannot apply batch");

        // visualize_stderr(&db);
        db.get(EMPTY_PATH, b"key1", None, grove_version)
            .unwrap()
            .expect("cannot get element");
        db.get([b"key1".as_ref()].as_ref(), b"key2", None, grove_version)
            .unwrap()
            .expect("cannot get element");
        db.get(
            [b"key1".as_ref(), b"key2"].as_ref(),
            b"key3",
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");
        db.get(
            [b"key1".as_ref(), b"key2", b"key3"].as_ref(),
            b"key4",
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        assert_eq!(
            db.get(
                [b"key1".as_ref(), b"key2", b"key3"].as_ref(),
                b"key4",
                None,
                grove_version
            )
            .unwrap()
            .expect("cannot get element"),
            element
        );
        assert_eq!(
            db.get([TEST_LEAF, b"key1"].as_ref(), b"key2", None, grove_version)
                .unwrap()
                .expect("cannot get element"),
            element2
        );
    }

    #[test]
    fn test_batch_operation_consistency_checker() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // No two operations should be the same
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"a".to_vec()],
                b"b".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"a".to_vec()],
                b"b".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(matches!(
            db.apply_batch(ops, None, None, grove_version).unwrap(),
            Err(Error::InvalidBatchOperation(
                "batch operations fail consistency checks"
            ))
        ));

        // Can't perform 2 or more operations on the same node
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"a".to_vec()],
                b"b".to_vec(),
                Element::new_item(vec![1]),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"a".to_vec()],
                b"b".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(matches!(
            db.apply_batch(ops, None, None, grove_version).unwrap(),
            Err(Error::InvalidBatchOperation(
                "batch operations fail consistency checks"
            ))
        ));

        // Can't insert under a deleted path
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                Element::new_item(vec![1]),
            ),
            QualifiedGroveDbOp::delete_op(vec![], TEST_LEAF.to_vec()),
        ];
        assert!(matches!(
            db.apply_batch(ops, None, None, grove_version).unwrap(),
            Err(Error::InvalidBatchOperation(
                "batch operations fail consistency checks"
            ))
        ));

        // Should allow invalid operations pass when disable option is set to true
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_insertion_does_not_override: false,
                    validate_insertion_does_not_override_tree: true,
                    disable_operation_consistency_check: true,
                    base_root_storage_is_free: true,
                    batch_pause_height: None,
                }),
                None,
                grove_version
            )
            .unwrap()
            .is_ok());
    }

    #[test]
    fn test_batch_validation_ok_on_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"keyb",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");

        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"key2".to_vec(),
                element2.clone(),
            ),
        ];
        db.apply_batch(ops, None, Some(&tx), grove_version)
            .unwrap()
            .expect("cannot apply batch");
        db.get(EMPTY_PATH, b"keyb", None, grove_version)
            .unwrap()
            .expect_err("we should not get an element");
        db.get(EMPTY_PATH, b"keyb", Some(&tx), grove_version)
            .unwrap()
            .expect("we should get an element");

        db.get(EMPTY_PATH, b"key1", None, grove_version)
            .unwrap()
            .expect_err("we should not get an element");
        db.get(EMPTY_PATH, b"key1", Some(&tx), grove_version)
            .unwrap()
            .expect("cannot get element");
        db.get(
            [b"key1".as_ref()].as_ref(),
            b"key2",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");
        db.get(
            [b"key1".as_ref(), b"key2"].as_ref(),
            b"key3",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");
        db.get(
            [b"key1".as_ref(), b"key2", b"key3"].as_ref(),
            b"key4",
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("cannot get element");

        assert_eq!(
            db.get(
                [b"key1".as_ref(), b"key2", b"key3"].as_ref(),
                b"key4",
                Some(&tx),
                grove_version
            )
            .unwrap()
            .expect("cannot get element"),
            element
        );
        assert_eq!(
            db.get(
                [TEST_LEAF, b"key1"].as_ref(),
                b"key2",
                Some(&tx),
                grove_version
            )
            .unwrap()
            .expect("cannot get element"),
            element2
        );
    }

    #[test]
    fn test_batch_add_other_element_in_sub_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();
        // let's start by inserting a tree structure
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(vec![], b"1".to_vec(), Element::empty_tree()),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"1".to_vec()],
                b"my_contract".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"1".to_vec(), b"my_contract".to_vec()],
                b"0".to_vec(),
                Element::new_item(b"this is the contract".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"1".to_vec(), b"my_contract".to_vec()],
                b"1".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"1".to_vec(), b"my_contract".to_vec(), b"1".to_vec()],
                b"person".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                ],
                b"0".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                ],
                b"message".to_vec(),
                Element::empty_tree(),
            ),
        ];

        db.apply_batch_with_element_flags_update(
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to do tree form insert");

        let some_element_flags = Some(vec![0]);

        // now let's add an item
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"0".to_vec(),
                ],
                b"sam".to_vec(),
                Element::new_item_with_flags(
                    b"Samuel Westrich".to_vec(),
                    some_element_flags.clone(),
                ),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                ],
                b"my apples are safe".to_vec(),
                Element::empty_tree_with_flags(some_element_flags.clone()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                    b"my apples are safe".to_vec(),
                ],
                b"0".to_vec(),
                Element::empty_tree_with_flags(some_element_flags.clone()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                    b"my apples are safe".to_vec(),
                    b"0".to_vec(),
                ],
                b"sam".to_vec(),
                Element::new_reference_with_max_hops_and_flags(
                    ReferencePathType::UpstreamRootHeightReference(
                        4,
                        vec![b"0".to_vec(), b"sam".to_vec()],
                    ),
                    Some(2),
                    some_element_flags.clone(),
                ),
            ),
        ];

        db.apply_batch_with_element_flags_update(
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("expected to do first insert");

        // now let's add an item
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"0".to_vec(),
                ],
                b"wisdom".to_vec(),
                Element::new_item_with_flags(b"Wisdom Ogwu".to_vec(), some_element_flags.clone()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                ],
                b"canteloupe!".to_vec(),
                Element::empty_tree_with_flags(some_element_flags.clone()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                    b"canteloupe!".to_vec(),
                ],
                b"0".to_vec(),
                Element::empty_tree_with_flags(some_element_flags.clone()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![
                    b"1".to_vec(),
                    b"my_contract".to_vec(),
                    b"1".to_vec(),
                    b"person".to_vec(),
                    b"message".to_vec(),
                    b"canteloupe!".to_vec(),
                    b"0".to_vec(),
                ],
                b"wisdom".to_vec(),
                Element::new_reference_with_max_hops_and_flags(
                    ReferencePathType::UpstreamRootHeightReference(
                        4,
                        vec![b"0".to_vec(), b"wisdom".to_vec()],
                    ),
                    Some(2),
                    some_element_flags,
                ),
            ),
        ];

        db.apply_batch_with_element_flags_update(
            ops,
            None,
            |cost, _old_flags, _new_flags| {
                // we should only either have nodes that are completely replaced (inner_trees)
                // or added
                assert!((cost.added_bytes > 0) ^ (cost.replaced_bytes > 0));
                Ok(false)
            },
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("successful batch apply");
    }

    #[test]
    fn test_batch_validation_broken_chain() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(ops, None, None, grove_version)
            .unwrap()
            .is_err());
        assert!(db
            .get([b"key1".as_ref()].as_ref(), b"key2", None, grove_version)
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_batch_validation_broken_chain_aborts_whole_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
                b"key2".to_vec(),
                element.clone(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(ops, None, None, grove_version)
            .unwrap()
            .is_err());
        assert!(db
            .get([b"key1".as_ref()].as_ref(), b"key2", None, grove_version)
            .unwrap()
            .is_err());
        assert!(db
            .get([TEST_LEAF, b"key1"].as_ref(), b"key2", None, grove_version)
            .unwrap()
            .is_err(),);
    }

    #[test]
    fn test_batch_validation_deletion_brokes_chain() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());

        db.insert(
            EMPTY_PATH,
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert a subtree");
        db.insert(
            [b"key1".as_ref()].as_ref(),
            b"key2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert a subtree");

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::delete_op(vec![b"key1".to_vec()], b"key2".to_vec()),
        ];
        assert!(db
            .apply_batch(ops, None, None, grove_version)
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_batch_validation_insertion_under_deleted_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::delete_op(vec![b"key1".to_vec()], b"key2".to_vec()),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect_err("insertion of element under a deleted tree should not be allowed");
        db.get(
            [b"key1".as_ref(), b"key2", b"key3"].as_ref(),
            b"key4",
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("nothing should have been inserted");
    }

    #[test]
    fn test_batch_validation_insert_into_existing_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());

        db.insert(
            [TEST_LEAF].as_ref(),
            b"invalid",
            element.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert value");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"valid",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert value");

        // Insertion into scalar is invalid
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"invalid".to_vec()],
            b"key1".to_vec(),
            element.clone(),
        )];
        assert!(db
            .apply_batch(ops, None, None, grove_version)
            .unwrap()
            .is_err());

        // Insertion into a tree is correct
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"valid".to_vec()],
            b"key1".to_vec(),
            element.clone(),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("cannot apply batch");
        assert_eq!(
            db.get([TEST_LEAF, b"valid"].as_ref(), b"key1", None, grove_version)
                .unwrap()
                .expect("cannot get element"),
            element
        );
    }

    #[test]
    fn test_batch_validation_nested_subtree_overwrite() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key_subtree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert a subtree");
        db.insert(
            [TEST_LEAF, b"key_subtree"].as_ref(),
            b"key2",
            element,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert an item");

        // TEST_LEAF can not be overwritten
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(vec![], TEST_LEAF.to_vec(), element2),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"key_subtree".to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_insertion_does_not_override: true,
                    validate_insertion_does_not_override_tree: true,
                    disable_operation_consistency_check: false,
                    base_root_storage_is_free: true,
                    batch_pause_height: None,
                }),
                None,
                grove_version
            )
            .unwrap()
            .is_err());

        // TEST_LEAF will be deleted so you can not insert underneath it
        let ops = vec![
            QualifiedGroveDbOp::delete_op(vec![], TEST_LEAF.to_vec()),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(ops, None, None, grove_version)
            .unwrap()
            .is_err());

        // TEST_LEAF will be deleted so you can not insert underneath it
        // We are testing with the batch apply option
        // validate_tree_insertion_does_not_override set to true
        let ops = vec![
            QualifiedGroveDbOp::delete_op(vec![], TEST_LEAF.to_vec()),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    disable_operation_consistency_check: false,
                    validate_insertion_does_not_override_tree: true,
                    validate_insertion_does_not_override: true,
                    base_root_storage_is_free: true,
                    batch_pause_height: None,
                }),
                None,
                grove_version
            )
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_batch_validation_root_leaf_removal() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                TEST_LEAF.to_vec(),
                Element::new_item(b"ayy".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::empty_tree(),
            ),
        ];
        assert!(db
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    validate_insertion_does_not_override: true,
                    validate_insertion_does_not_override_tree: true,
                    disable_operation_consistency_check: false,
                    base_root_storage_is_free: true,
                    batch_pause_height: None,
                }),
                None,
                grove_version
            )
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_merk_data_is_deleted() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let element = Element::new_item(b"ayy".to_vec());

        db.insert(
            [TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert a subtree");
        db.insert(
            [TEST_LEAF, b"key1"].as_ref(),
            b"key2",
            element.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert an item");
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"key1".to_vec(),
            Element::new_item(b"ayy2".to_vec()),
        )];

        assert_eq!(
            db.get([TEST_LEAF, b"key1"].as_ref(), b"key2", None, grove_version)
                .unwrap()
                .expect("cannot get item"),
            element
        );
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("cannot apply batch");
        assert!(db
            .get([TEST_LEAF, b"key1"].as_ref(), b"key2", None, grove_version)
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_multi_tree_insertion_deletion_with_propagation_no_tx() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            EMPTY_PATH,
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert root leaf");
        db.insert(
            EMPTY_PATH,
            b"key2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert root leaf");
        db.insert(
            [ANOTHER_TEST_LEAF].as_ref(),
            b"key1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cannot insert root leaf");

        let hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("cannot get root hash");
        let element = Element::new_item(b"ayy".to_vec());
        let element2 = Element::new_item(b"ayy2".to_vec());

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                element.clone(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key2".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key".to_vec(),
                element2.clone(),
            ),
            QualifiedGroveDbOp::delete_op(vec![ANOTHER_TEST_LEAF.to_vec()], b"key1".to_vec()),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("cannot apply batch");

        assert!(db
            .get([ANOTHER_TEST_LEAF].as_ref(), b"key1", None, grove_version)
            .unwrap()
            .is_err());

        assert_eq!(
            db.get(
                [b"key1".as_ref(), b"key2", b"key3"].as_ref(),
                b"key4",
                None,
                grove_version
            )
            .unwrap()
            .expect("cannot get element"),
            element
        );
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), b"key", None, grove_version)
                .unwrap()
                .expect("cannot get element"),
            element2
        );
        assert_ne!(
            db.root_hash(None, grove_version)
                .unwrap()
                .expect("cannot get root hash"),
            hash
        );

        // verify root leaves
        assert!(db
            .get(EMPTY_PATH, TEST_LEAF, None, grove_version)
            .unwrap()
            .is_ok());
        assert!(db
            .get(EMPTY_PATH, ANOTHER_TEST_LEAF, None, grove_version)
            .unwrap()
            .is_ok());
        assert!(db
            .get(EMPTY_PATH, b"key1", None, grove_version)
            .unwrap()
            .is_ok());
        assert!(db
            .get(EMPTY_PATH, b"key2", None, grove_version)
            .unwrap()
            .is_ok());
        assert!(db
            .get(EMPTY_PATH, b"key3", None, grove_version)
            .unwrap()
            .is_err());
    }

    #[test]
    fn test_nested_batch_insertion_corrupts_state() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let full_path = vec![
            b"leaf1".to_vec(),
            b"sub1".to_vec(),
            b"sub2".to_vec(),
            b"sub3".to_vec(),
            b"sub4".to_vec(),
            b"sub5".to_vec(),
        ];
        let mut acc_path: Vec<Vec<u8>> = vec![];
        for p in full_path.into_iter() {
            db.insert(
                acc_path.as_slice(),
                &p,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected to insert");
            acc_path.push(p);
        }

        let element = Element::new_item(b"ayy".to_vec());
        let batch = vec![QualifiedGroveDbOp::insert_or_replace_op(
            acc_path.clone(),
            b"key".to_vec(),
            element.clone(),
        )];
        db.apply_batch(batch, None, None, grove_version)
            .unwrap()
            .expect("cannot apply batch");

        let batch = vec![QualifiedGroveDbOp::insert_or_replace_op(
            acc_path,
            b"key".to_vec(),
            element,
        )];
        db.apply_batch(batch, None, None, grove_version)
            .unwrap()
            .expect("cannot apply same batch twice");
    }

    #[test]
    fn test_apply_sorted_pre_validated_batch_propagation() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let full_path = vec![b"leaf1".to_vec(), b"sub1".to_vec()];
        let mut acc_path: Vec<Vec<u8>> = vec![];
        for p in full_path.into_iter() {
            db.insert(
                acc_path.as_slice(),
                &p,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("expected to insert");
            acc_path.push(p);
        }

        let root_hash = db.root_hash(None, grove_version).unwrap().unwrap();

        let element = Element::new_item(b"ayy".to_vec());
        let batch = vec![QualifiedGroveDbOp::insert_or_replace_op(
            acc_path.clone(),
            b"key".to_vec(),
            element,
        )];
        db.apply_batch(batch, None, None, grove_version)
            .unwrap()
            .expect("cannot apply batch");

        assert_ne!(
            db.root_hash(None, grove_version).unwrap().unwrap(),
            root_hash
        );
    }

    #[test]
    fn test_references() {
        let grove_version = GroveVersion::latest();
        // insert reference that points to non-existent item
        let db = make_test_grovedb(grove_version);
        let batch = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"key1".to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"invalid_path".to_vec(),
            ])),
        )];
        assert!(matches!(
            db.apply_batch(batch, None, None, grove_version).unwrap(),
            Err(Error::MissingReference(String { .. }))
        ));

        // insert reference with item it points to in the same batch
        let db = make_test_grovedb(grove_version);
        let elem = Element::new_item(b"ayy".to_vec());
        let batch = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"invalid_path".to_vec(),
                ])),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"invalid_path".to_vec(),
                elem.clone(),
            ),
        ];
        assert!(db
            .apply_batch(batch, None, None, grove_version)
            .unwrap()
            .is_ok());
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), b"key1", None, grove_version)
                .unwrap()
                .unwrap(),
            elem
        );

        // should successfully prove reference as the value hash is valid
        let mut reference_key_query = Query::new();
        reference_key_query.insert_key(b"key1".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], reference_key_query);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");
        let verification_result = GroveDb::verify_query_raw(&proof, &path_query, grove_version);
        assert!(verification_result.is_ok());

        // Hit reference limit when you specify max reference hop, lower than actual hop
        // count
        let db = make_test_grovedb(grove_version);
        let elem = Element::new_item(b"ayy".to_vec());
        let batch = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key2".to_vec(),
                Element::new_reference_with_hops(
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"key1".to_vec(),
                    ]),
                    Some(1),
                ),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::new_reference_with_hops(
                    ReferencePathType::AbsolutePathReference(vec![
                        TEST_LEAF.to_vec(),
                        b"invalid_path".to_vec(),
                    ]),
                    Some(1),
                ),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"invalid_path".to_vec(),
                elem,
            ),
        ];
        assert!(matches!(
            db.apply_batch(batch, None, None, grove_version).unwrap(),
            Err(Error::ReferenceLimit)
        ));
    }

    #[test]
    fn test_batch_replace_item_with_sum_item_flags_update() {
        // Exercises the Element::ItemWithSumItem branch in MerkCache's
        // flags update closure (line ~2136).
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        // Create a sum tree that can hold ItemWithSumItem elements.
        db.insert(
            EMPTY_PATH,
            b"sum_tree",
            Element::empty_sum_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");

        // Insert an ItemWithSumItem with flags.
        db.insert(
            [b"sum_tree".as_ref()].as_ref(),
            b"key1",
            Element::new_item_with_sum_item_with_flags(b"hello".to_vec(), 42, Some(b"f1".to_vec())),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert item_with_sum_item");

        // Replace via batch with flags update returning true.
        let ops = vec![QualifiedGroveDbOp::replace_op(
            vec![b"sum_tree".to_vec()],
            b"key1".to_vec(),
            Element::new_item_with_sum_item_with_flags(
                b"world".to_vec(),
                100,
                Some(b"f2".to_vec()),
            ),
        )];

        db.apply_batch_with_element_flags_update(
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(true),
            |_flags, removed_key_bytes, removed_value_bytes| {
                Ok((
                    StorageRemovedBytes::BasicStorageRemoval(removed_key_bytes),
                    StorageRemovedBytes::BasicStorageRemoval(removed_value_bytes),
                ))
            },
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("batch replace item_with_sum_item");

        // Verify the element was updated.
        let elem = db
            .get(
                [b"sum_tree".as_ref()].as_ref(),
                b"key1",
                Some(&tx),
                grove_version,
            )
            .unwrap()
            .expect("get replaced element");
        assert_eq!(
            elem,
            Element::new_item_with_sum_item_with_flags(
                b"world".to_vec(),
                100,
                Some(b"f2".to_vec()),
            )
        );
    }

    #[test]
    fn test_batch_delete_non_merk_tree_cleans_data_storage() {
        // Exercises the non-Merk delete path collection (line ~3057)
        // and data storage cleanup (line ~3101).
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        // Insert a CommitmentTree.
        db.insert(
            EMPTY_PATH,
            b"ct",
            Element::empty_commitment_tree(4).expect("valid chunk_power"),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree");

        // Write actual data to populate data storage (frontier + bulk data).
        // Payload must be ciphertext_payload_size::<DashMemo>() = 32+104+80 = 216 bytes.
        db.commitment_tree_insert_raw(
            EMPTY_PATH,
            b"ct",
            [1u8; 32],
            [2u8; 32],
            [5u8; 32],
            vec![0u8; 216],
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree data");

        // Delete it via batch.  The tree is non-empty (has one entry).
        // Use DeleteChildren to skip the emptiness check but still perform
        // post-apply storage cleanup.
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"ct".to_vec(),
            grovedb_merk::tree_type::TreeType::CommitmentTree(4),
            SubelementsDeletionBehavior::DeleteChildren,
        )];

        let batch_options = Some(BatchApplyOptions::default());

        db.apply_batch(ops, batch_options, Some(&tx), grove_version)
            .unwrap()
            .expect("batch delete non-merk tree");

        // Verify element is gone.
        assert!(db
            .get(EMPTY_PATH, b"ct", Some(&tx), grove_version)
            .unwrap()
            .is_err());

        // Recreate the tree and insert fresh data. CommitmentTree::open
        // validates that the stored frontier's tree_size matches total_count.
        // If cleanup failed, stale frontier (tree_size=1) would conflict with
        // total_count=0 and cause an error — proving data-storage cleanup.
        db.insert(
            EMPTY_PATH,
            b"ct",
            Element::empty_commitment_tree(4).expect("valid chunk_power"),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("recreate commitment tree");

        db.commitment_tree_insert_raw(
            EMPTY_PATH,
            b"ct",
            [3u8; 32],
            [4u8; 32],
            [6u8; 32],
            vec![0u8; 216],
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert into recreated commitment tree");

        // Verify the recreated tree has exactly 1 note (fresh start).
        let elem = db
            .get(EMPTY_PATH, b"ct", Some(&tx), grove_version)
            .unwrap()
            .expect("get recreated ct");
        match elem {
            Element::CommitmentTree(count, _, _) => {
                assert_eq!(count, 1, "recreated tree should have count 1");
            }
            _ => panic!("expected CommitmentTree element"),
        }
    }

    #[test]
    fn test_batch_delete_mmr_tree_cleans_data_storage() {
        // Exercises non-Merk data-storage cleanup for MmrTree.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert mmr tree");

        // Populate data storage with MMR nodes.
        for i in 0..3u8 {
            db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![i], Some(&tx), grove_version)
                .unwrap()
                .expect("append mmr value");
        }

        // The tree is non-empty (has 3 entries). Use DeleteChildren to skip
        // the emptiness check but still perform post-apply storage cleanup.
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"mmr".to_vec(),
            grovedb_merk::tree_type::TreeType::MmrTree,
            SubelementsDeletionBehavior::DeleteChildren,
        )];

        let batch_options = Some(BatchApplyOptions::default());

        db.apply_batch(ops, batch_options, Some(&tx), grove_version)
            .unwrap()
            .expect("batch delete mmr tree");

        // Verify element is gone.
        assert!(db
            .get(EMPTY_PATH, b"mmr", Some(&tx), grove_version)
            .unwrap()
            .is_err());

        // Recreate and verify fresh start.
        db.insert(
            EMPTY_PATH,
            b"mmr",
            Element::empty_mmr_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("recreate mmr tree");

        db.mmr_tree_append(
            EMPTY_PATH,
            b"mmr",
            b"fresh".to_vec(),
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("append to recreated mmr");

        let count = db
            .mmr_tree_leaf_count(EMPTY_PATH, b"mmr", Some(&tx), grove_version)
            .unwrap()
            .expect("leaf count");
        assert_eq!(count, 1, "recreated MMR should have 1 leaf");
    }

    #[test]
    fn test_partial_batch_delete_non_merk_tree_cleans_data_storage() {
        // Exercises the non-Merk delete cleanup in
        // apply_partial_batch_with_element_flags_update (line ~3239, ~3337).
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        // Insert a DenseAppendOnlyFixedSizeTree.
        db.insert(
            EMPTY_PATH,
            b"dense",
            Element::empty_dense_tree(3),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert dense tree");

        // Populate data storage with dense tree values.
        for i in 0..3u8 {
            db.dense_tree_insert(EMPTY_PATH, b"dense", vec![i; 32], Some(&tx), grove_version)
                .unwrap()
                .expect("insert dense tree value");
        }

        // The tree is non-empty (has 3 entries). Use DeleteChildren to skip
        // the emptiness check but still perform post-apply storage cleanup.
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"dense".to_vec(),
            grovedb_merk::tree_type::TreeType::DenseAppendOnlyFixedSizeTree(3),
            SubelementsDeletionBehavior::DeleteChildren,
        )];

        let batch_options = Some(BatchApplyOptions::default());

        db.apply_partial_batch(
            ops,
            batch_options,
            |_cost, _left_over_ops| Ok(vec![]),
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("partial batch delete non-merk tree");

        // Verify element is gone.
        assert!(db
            .get(EMPTY_PATH, b"dense", Some(&tx), grove_version)
            .unwrap()
            .is_err());

        // Recreate and verify fresh start.
        db.insert(
            EMPTY_PATH,
            b"dense",
            Element::empty_dense_tree(3),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("recreate dense tree");

        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            vec![99u8; 32],
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert into recreated dense tree");

        let elem = db
            .get(EMPTY_PATH, b"dense", Some(&tx), grove_version)
            .unwrap()
            .expect("get recreated dense tree");
        match elem {
            Element::DenseAppendOnlyFixedSizeTree(count, _, _) => {
                assert_eq!(count, 1, "recreated dense tree should have count 1");
            }
            _ => panic!("expected DenseAppendOnlyFixedSizeTree element"),
        }
    }

    // ===================================================================
    // InsertIfNotExists and InsertWithKnownToNotAlreadyExist tests
    // ===================================================================

    #[test]
    fn test_batch_insert_if_not_exists_succeeds_for_new_key() {
        // InsertIfNotExists should succeed when the key does not exist.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![QualifiedGroveDbOp::insert_if_not_exists_op(
            vec![TEST_LEAF.to_vec()],
            b"new_key".to_vec(),
            Element::new_item(b"value".to_vec()),
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("insert_if_not_exists should succeed for new key");

        let result = db
            .get([TEST_LEAF].as_ref(), b"new_key", None, grove_version)
            .unwrap()
            .expect("get inserted item");
        assert_eq!(result, Element::new_item(b"value".to_vec()));
    }

    #[test]
    fn test_batch_insert_if_not_exists_errors_when_key_exists() {
        // InsertIfNotExists (with error_if_exists=true) should fail when key exists.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"existing",
            Element::new_item(b"original".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert original");

        // Try to insert at the same key with insert_if_not_exists
        let ops = vec![QualifiedGroveDbOp::insert_if_not_exists_op(
            vec![TEST_LEAF.to_vec()],
            b"existing".to_vec(),
            Element::new_item(b"new_value".to_vec()),
        )];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();

        assert!(
            result.is_err(),
            "insert_if_not_exists should fail when key exists, got: {:?}",
            result,
        );

        // Original value should be preserved
        let val = db
            .get([TEST_LEAF].as_ref(), b"existing", None, grove_version)
            .unwrap()
            .expect("get existing");
        assert_eq!(val, Element::new_item(b"original".to_vec()));
    }

    #[test]
    fn test_batch_insert_if_not_exists_or_skip_silently_skips() {
        // InsertIfNotExists with error_if_exists=false should silently skip
        // when the key already exists.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"existing",
            Element::new_item(b"original".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert original");

        // Use insert_if_not_exists_or_skip (error_if_exists=false)
        let ops = vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
            vec![TEST_LEAF.to_vec()],
            b"existing".to_vec(),
            Element::new_item(b"new_value".to_vec()),
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("insert_if_not_exists_or_skip should succeed (skip)");

        // Original value should be preserved
        let val = db
            .get([TEST_LEAF].as_ref(), b"existing", None, grove_version)
            .unwrap()
            .expect("get existing");
        assert_eq!(val, Element::new_item(b"original".to_vec()));
    }

    #[test]
    fn test_batch_insert_only_known_to_not_already_exist() {
        // InsertWithKnownToNotAlreadyExist should succeed for a new key.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![
            QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op(
                vec![TEST_LEAF.to_vec()],
                b"brand_new".to_vec(),
                Element::new_item(b"data".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("insert_only_known_to_not_already_exist should succeed");

        let result = db
            .get([TEST_LEAF].as_ref(), b"brand_new", None, grove_version)
            .unwrap()
            .expect("get inserted item");
        assert_eq!(result, Element::new_item(b"data".to_vec()));
    }

    #[test]
    fn test_batch_insert_if_not_exists_with_flags_update() {
        // Test InsertIfNotExists through the apply_batch_with_element_flags_update
        // code path (with element flags function).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let tx = db.start_transaction();

        // Insert an item first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"flagged",
            Element::new_item(b"original".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert original");

        // Try insert_if_not_exists with element flags (uses the flags update path)
        let ops = vec![QualifiedGroveDbOp::insert_if_not_exists_op(
            vec![TEST_LEAF.to_vec()],
            b"flagged".to_vec(),
            Element::new_item(b"new_value".to_vec()),
        )];

        let batch_options = Some(BatchApplyOptions {
            validate_insertion_does_not_override: false,
            ..Default::default()
        });

        let result = db
            .apply_batch_with_element_flags_update(
                ops,
                batch_options,
                |_cost, _old_flags, _new_flags| Ok(false),
                |_flags, _removed_key_bytes, _removed_value_bytes| {
                    Ok((NoStorageRemoval, NoStorageRemoval))
                },
                Some(&tx),
                grove_version,
            )
            .unwrap();

        assert!(
            result.is_err(),
            "insert_if_not_exists via flags update should fail when key exists: {:?}",
            result,
        );

        // Original should be preserved
        let val = db
            .get([TEST_LEAF].as_ref(), b"flagged", Some(&tx), grove_version)
            .unwrap()
            .expect("get existing");
        assert_eq!(val, Element::new_item(b"original".to_vec()));
    }

    #[test]
    fn test_batch_insert_if_not_exists_or_skip_with_flags_update() {
        // Test InsertIfNotExists (skip mode) through the flags update path.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let tx = db.start_transaction();

        // Insert an item first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"flagged2",
            Element::new_item(b"original".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert original");

        // Use insert_if_not_exists_or_skip with element flags
        let ops = vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
            vec![TEST_LEAF.to_vec()],
            b"flagged2".to_vec(),
            Element::new_item(b"new_value".to_vec()),
        )];

        let batch_options = Some(BatchApplyOptions {
            validate_insertion_does_not_override: false,
            ..Default::default()
        });

        db.apply_batch_with_element_flags_update(
            ops,
            batch_options,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert_if_not_exists_or_skip via flags update should succeed (skip)");

        // Original should be preserved
        let val = db
            .get([TEST_LEAF].as_ref(), b"flagged2", Some(&tx), grove_version)
            .unwrap()
            .expect("get existing");
        assert_eq!(val, Element::new_item(b"original".to_vec()));
    }

    #[test]
    fn test_batch_insert_if_not_exists_new_key_with_flags_update() {
        // Test InsertIfNotExists for a new key through the flags update path.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let tx = db.start_transaction();

        let ops = vec![QualifiedGroveDbOp::insert_if_not_exists_op(
            vec![TEST_LEAF.to_vec()],
            b"new_flagged".to_vec(),
            Element::new_item(b"fresh".to_vec()),
        )];

        let batch_options = Some(BatchApplyOptions {
            validate_insertion_does_not_override: false,
            ..Default::default()
        });

        db.apply_batch_with_element_flags_update(
            ops,
            batch_options,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert_if_not_exists should succeed for new key via flags update");

        let val = db
            .get(
                [TEST_LEAF].as_ref(),
                b"new_flagged",
                Some(&tx),
                grove_version,
            )
            .unwrap()
            .expect("get new item");
        assert_eq!(val, Element::new_item(b"fresh".to_vec()));
    }

    // ===================================================================
    // Debug formatting tests for new op variants
    // ===================================================================

    #[test]
    fn test_debug_format_insert_if_not_exists_ops() {
        // Verify Debug formatting covers the new InsertIfNotExists variants
        let op_error = QualifiedGroveDbOp::insert_if_not_exists_op(
            vec![b"path".to_vec()],
            b"key".to_vec(),
            Element::new_item(b"val".to_vec()),
        );
        let debug_str = format!("{:?}", op_error);
        assert!(
            debug_str.contains("Insert If Not Exists (error on existing)"),
            "unexpected debug format: {}",
            debug_str,
        );

        let op_skip = QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
            vec![b"path".to_vec()],
            b"key".to_vec(),
            Element::new_item(b"val".to_vec()),
        );
        let debug_str = format!("{:?}", op_skip);
        assert!(
            debug_str.contains("Insert If Not Exists (skip on existing)"),
            "unexpected debug format: {}",
            debug_str,
        );

        let op_known = QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op(
            vec![b"path".to_vec()],
            b"key".to_vec(),
            Element::new_item(b"val".to_vec()),
        );
        let debug_str = format!("{:?}", op_known);
        assert!(
            debug_str.contains("Insert With Known To Not Already Exist"),
            "unexpected debug format: {}",
            debug_str,
        );
    }

    #[test]
    fn test_batch_rejects_key_longer_than_255_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        // Create a key that is 256 bytes long (one byte over the limit)
        let oversized_key = vec![b'x'; 256];
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            oversized_key,
            Element::new_item(b"value".to_vec()),
        )];

        let result = db.apply_batch(ops, None, Some(&tx), grove_version).unwrap();
        assert!(
            result.is_err(),
            "batch with oversized key should be rejected"
        );
        match result {
            Err(Error::InvalidInput(msg)) => {
                assert!(
                    msg.contains("255"),
                    "error should mention the 255 byte limit, got: {msg}"
                );
            }
            Err(other) => panic!("expected InvalidInput error, got: {:?}", other),
            Ok(_) => unreachable!(),
        }

        // Verify that a key of exactly 255 bytes is accepted
        let max_key = vec![b'y'; 255];
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            max_key,
            Element::new_item(b"value".to_vec()),
        )];
        db.apply_batch(ops, None, Some(&tx), grove_version)
            .unwrap()
            .expect("batch with 255-byte key should succeed");
    }

    #[test]
    fn test_apply_operations_without_batching_is_not_atomic() {
        // Demonstrates that apply_operations_without_batching is NOT atomic:
        // if the second operation fails, the first operation's side-effects
        // are still committed to the database.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let tx = db.start_transaction();

        // Op 1: Insert a valid item under TEST_LEAF -- this should succeed.
        // Op 2: Insert an item under a non-existent subtree -- this should fail.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"key1".to_vec(),
                Element::new_item(b"value1".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"nonexistent_subtree".to_vec()],
                b"key2".to_vec(),
                Element::new_item(b"value2".to_vec()),
            ),
        ];

        // The overall call should fail because the second op targets a
        // subtree that does not exist.
        let result = db.apply_operations_without_batching(ops, None, Some(&tx), grove_version);
        assert!(
            result.unwrap().is_err(),
            "should fail because the second op targets a non-existent subtree"
        );

        // Despite the failure, the first operation was already committed
        // (non-atomic behavior). We can observe this by reading key1.
        let element = db
            .get([TEST_LEAF].as_ref(), b"key1", Some(&tx), grove_version)
            .unwrap()
            .expect("first op should have been committed despite later failure");
        assert_eq!(element, Element::new_item(b"value1".to_vec()));
    }

    #[test]
    fn test_batch_reference_hop_count_capped_to_max() {
        // Audit L2: Verify that even if a reference specifies
        // max_reference_hop = Some(255), the effective recursion depth is
        // capped to MAX_REFERENCE_HOPS (10).
        //
        // We build a chain of MAX_REFERENCE_HOPS + 1 references (11 hops)
        // ending at an item.  With the cap enforced, this should fail with
        // ReferenceLimit because 10 < 11.  Without the cap, 255 >= 11 would
        // allow all hops to succeed.
        use crate::operations::get::MAX_REFERENCE_HOPS;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let chain_len = MAX_REFERENCE_HOPS + 1; // 11 references before the item
        let mut batch = Vec::new();

        // Insert the base item that the chain ultimately points to.
        batch.push(QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"item".to_vec(),
            Element::new_item(b"value".to_vec()),
        ));

        // Build chain: ref_0 -> ref_1 -> ... -> ref_{chain_len-1} -> item
        // ref_{chain_len-1} points to "item".
        // ref_i points to ref_{i+1} for i < chain_len-1.
        for i in (0..chain_len).rev() {
            let key = format!("ref_{}", i).into_bytes();
            let target_key = if i == chain_len - 1 {
                b"item".to_vec()
            } else {
                format!("ref_{}", i + 1).into_bytes()
            };

            // Only the first reference in the chain (ref_0) carries the
            // user-specified hop limit of 255.  The others use None
            // (which defaults to MAX_REFERENCE_HOPS inside the batch
            // resolution code).
            let max_hops = if i == 0 { Some(255u8) } else { None };

            batch.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                key,
                Element::new_reference_with_hops(
                    ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), target_key]),
                    max_hops,
                ),
            ));
        }

        // With the cap in place, the batch should fail because 11 hops
        // exceed the capped limit of MAX_REFERENCE_HOPS (10).
        let result = db.apply_batch(batch, None, None, grove_version).unwrap();
        assert!(
            matches!(result, Err(Error::ReferenceLimit)),
            "expected ReferenceLimit error due to hop cap, got: {:?}",
            result,
        );

        // Verify that a chain of exactly MAX_REFERENCE_HOPS still succeeds
        // with max_reference_hop = Some(255), proving the cap allows up to
        // MAX_REFERENCE_HOPS hops.
        let db = make_test_grovedb(grove_version);
        let ok_chain_len = MAX_REFERENCE_HOPS; // 10 references before the item
        let mut batch = Vec::new();

        batch.push(QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"ok_item".to_vec(),
            Element::new_item(b"ok_value".to_vec()),
        ));

        for i in (0..ok_chain_len).rev() {
            let key = format!("ok_ref_{}", i).into_bytes();
            let target_key = if i == ok_chain_len - 1 {
                b"ok_item".to_vec()
            } else {
                format!("ok_ref_{}", i + 1).into_bytes()
            };

            let max_hops = if i == 0 { Some(255u8) } else { None };

            batch.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                key,
                Element::new_reference_with_hops(
                    ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), target_key]),
                    max_hops,
                ),
            ));
        }

        db.apply_batch(batch, None, None, grove_version)
            .unwrap()
            .expect("chain of exactly MAX_REFERENCE_HOPS with hop cap should succeed");
    }
}
