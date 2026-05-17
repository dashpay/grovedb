//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.

#[cfg(feature = "constructor")]
mod constructor;

pub(crate) mod helpers;
mod serialize;

#[cfg(feature = "visualize")]
mod visualize;

use std::fmt;

use bincode::{Decode, Encode};

use crate::{element_type::ElementType, reference_path::ReferencePathType};

/// Optional meta-data to be stored per element
pub type ElementFlags = Vec<u8>;

/// Optional single byte to represent the maximum number of reference hop to
/// base element
pub type MaxReferenceHop = Option<u8>;

/// int 64 sum value
pub type SumValue = i64;

/// int 128 sum value
pub type BigSumValue = i128;

/// int 64 count value
pub type CountValue = u64;

#[cfg(feature = "verify")]
pub trait ElementCostSizeExtension {
    fn cost_size(&self) -> u32;
}

/// Variants of GroveDB stored entities
///
/// ONLY APPEND TO THIS LIST!!! Because
/// of how serialization works.
///
/// `serde::Deserialize` is implemented manually (under the `serde` feature)
/// so it can enforce the same wrapper invariants as `Element::deserialize`:
/// `NonCounted`, `NotSummed`, and `NotCountedOrSummed` may not nest in any
/// combination, and the sum-bearing wrappers (`NotSummed`,
/// `NotCountedOrSummed`) may only wrap a sum-tree variant.
/// `serde::Serialize` is derived; serialization of valid `Element` values is
/// always safe.
#[derive(Clone, Encode, Decode, PartialEq, Eq, Hash)]
#[cfg_attr(not(feature = "visualize"), derive(Debug))]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Element {
    /// An ordinary value
    Item(Vec<u8>, Option<ElementFlags>),
    /// A reference to an object by its path
    Reference(ReferencePathType, MaxReferenceHop, Option<ElementFlags>),
    /// A subtree, contains the prefixed key representing the root of the
    /// subtree.
    Tree(Option<Vec<u8>>, Option<ElementFlags>),
    /// Signed integer value that can be totaled in a sum tree
    SumItem(SumValue, Option<ElementFlags>),
    /// Same as Element::Tree but underlying Merk sums value of it's summable
    /// nodes
    SumTree(Option<Vec<u8>>, SumValue, Option<ElementFlags>),
    /// Same as Element::Tree but underlying Merk sums value of it's summable
    /// nodes in big form i128
    /// The big sum tree is valuable if you have a big sum tree of sum trees
    BigSumTree(Option<Vec<u8>>, BigSumValue, Option<ElementFlags>),
    /// Same as Element::Tree but underlying Merk counts value of its countable
    /// nodes
    CountTree(Option<Vec<u8>>, CountValue, Option<ElementFlags>),
    /// Combines Element::SumTree and Element::CountTree
    CountSumTree(Option<Vec<u8>>, CountValue, SumValue, Option<ElementFlags>),
    /// Same as Element::CountTree but includes counts in cryptographic state
    ProvableCountTree(Option<Vec<u8>>, CountValue, Option<ElementFlags>),
    /// An ordinary value with a sum value
    ItemWithSumItem(Vec<u8>, SumValue, Option<ElementFlags>),
    /// Same as Element::CountSumTree but includes counts in cryptographic state
    /// (sum is tracked but NOT included in hash, only count is)
    ProvableCountSumTree(Option<Vec<u8>>, CountValue, SumValue, Option<ElementFlags>),
    /// Orchard-style commitment tree: combines a BulkAppendTree (for efficient
    /// append-only storage of cmx||encrypted_note payloads with epoch
    /// compaction) and a Sinsemilla Frontier (for anchor computation).
    /// Items are stored in the data namespace via BulkAppendTree;
    /// the frontier is stored in data storage.
    ///
    /// Fields: `(total_count, chunk_power, flags)`
    /// - `total_count`: Number of notes appended so far.
    /// - `chunk_power`: Log2 of the chunk size (actual size = `1 <<
    ///   chunk_power`).
    /// - `flags`: Optional per-element metadata.
    ///
    /// The Sinsemilla frontier root hash is not stored in the Element; it
    /// is persisted in data storage and verified by opening the frontier.
    /// The BulkAppendTree state_root flows as the Merk child hash.
    CommitmentTree(u64, u8, Option<ElementFlags>),
    /// MMR (Merkle Mountain Range) tree: append-only authenticated data
    /// structure with zero rotations, O(N) total hashes, sequential I/O.
    ///
    /// The MMR root hash is stored as the Merk child hash (not in the Element).
    ///
    /// Fields: `(mmr_size, flags)`
    /// - `mmr_size`: Total number of MMR nodes (internal + leaves).
    /// - `flags`: Optional per-element metadata.
    MmrTree(u64, Option<ElementFlags>),
    /// Bulk-append tree: two-level structure with a dense Merkle buffer that
    /// compacts into chunk blobs stored in an MMR.
    ///
    /// Fields: `(total_count, chunk_power, flags)`
    /// - `total_count`: Number of items appended so far.
    /// - `chunk_power`: Log2 of the chunk size (actual size = `1 <<
    ///   chunk_power`).
    /// - `flags`: Optional per-element metadata.
    ///
    /// The state root (`blake3(mmr_root || buffer_hash)`) flows through the
    /// Merk child hash mechanism (`insert_subtree`'s `subtree_root_hash`
    /// parameter).
    BulkAppendTree(u64, u8, Option<ElementFlags>),
    /// Dense fixed-sized Merkle tree: a complete binary tree of height h with
    /// 2^h - 1 positions. Nodes are filled sequentially in level-order (BFS).
    /// Root hash flows through the Merk child hash mechanism (insert_subtree's
    /// subtree_root_hash parameter).
    ///
    /// Fields: `(count, height, flags)`
    /// - `count`: Number of values inserted so far.
    /// - `height`: Tree height h; the tree has 2^h - 1 positions.
    /// - `flags`: Optional per-element metadata.
    DenseAppendOnlyFixedSizeTree(u16, u8, Option<ElementFlags>),
    /// Non-counted wrapper: contains any other Element type and behaves
    /// identically to it for storage, hashing, and internal aggregation, but
    /// contributes 0 to its parent count tree's aggregate count when inserted.
    /// Sums still propagate to parent sum trees.
    ///
    /// May only be inserted into count-bearing trees (`CountTree`,
    /// `ProvableCountTree`, `CountSumTree`, `ProvableCountSumTree`).
    ///
    /// Invariant: a `NonCounted` may not wrap another `NonCounted`. Enforced
    /// at construction and at deserialization.
    NonCounted(Box<Element>),
    /// Not-summed wrapper: contains a sum-bearing tree variant (`SumTree`,
    /// `BigSumTree`, `CountSumTree`, `ProvableCountSumTree`) and behaves
    /// identically to it for storage, hashing, and its own internal sum
    /// aggregate, but contributes 0 to its parent sum tree's running sum
    /// when inserted. Counts still propagate.
    ///
    /// May only be inserted into sum-bearing trees (`SumTree`, `BigSumTree`,
    /// `CountSumTree`, `ProvableCountSumTree`).
    ///
    /// Invariants (enforced at construction, serialization, and
    /// deserialization):
    /// - The inner element MUST be one of the four sum-tree variants above.
    /// - A `NotSummed` may not wrap another `NotSummed`, a `NonCounted`,
    ///   `NotCountedOrSummed`, or any non-tree element.
    NotSummed(Box<Element>),
    /// Not-counted-or-summed wrapper: contains a sum-bearing tree variant
    /// (`SumTree`, `BigSumTree`, `CountSumTree`, `ProvableCountSumTree`) and
    /// behaves identically to it for storage, hashing, and its own internal
    /// aggregates, but contributes 0 to BOTH its parent's running sum AND
    /// the parent's count when inserted.
    ///
    /// May only be inserted into count-AND-sum-bearing trees (`CountSumTree`,
    /// `ProvableCountSumTree`) — the only tree types where suppressing both
    /// axes is meaningful. Any other parent rejects the wrapper at insert.
    ///
    /// For `SumTree` / `BigSumTree` inners, this wrapper suppresses the
    /// implicit count-of-one a subtree would contribute to a parent count
    /// tree (it stores a sum internally, but counts as a single element).
    /// For `CountSumTree` / `ProvableCountSumTree` inners, it suppresses
    /// the explicit count value as well as the sum.
    ///
    /// Invariants (enforced at construction, serialization, and
    /// deserialization):
    /// - The inner element MUST be one of the four sum-tree variants above.
    /// - A `NotCountedOrSummed` may not wrap any other wrapper or any
    ///   non-tree element.
    NotCountedOrSummed(Box<Element>),
    /// A reference that simultaneously carries an explicit `SumValue`.
    ///
    /// Resolves like `Element::Reference` on `get()` / `follow_reference()`
    /// (hop-limited, cycle-detected, combined value hash) AND contributes
    /// `sum_value` to any sum-bearing parent tree like `Element::SumItem` /
    /// `Element::ItemWithSumItem`. The `sum_value` is **independent of the
    /// resolved target's value** — it is the caller-supplied weight or amount
    /// associated with the link itself.
    ///
    /// Use case: ranked / sortable index entries where the key encodes the
    /// rank, the reference points to a canonical record elsewhere, and the
    /// sum is the entry's monetary weight that aggregates into the parent's
    /// total.
    ///
    /// May be inserted into any parent tree type. In non-sum parents (Tree,
    /// CountTree, ProvableCountTree) the `sum_value` is silently ignored,
    /// the same rule `Element::ItemWithSumItem` follows.
    ///
    /// Wrapper compatibility:
    /// - **May** be wrapped in `NonCounted` to opt out of count propagation.
    /// - **May NOT** be wrapped in `NotSummed` or `NotCountedOrSummed` —
    ///   those whitelists accept only the four sum-tree variants, not
    ///   item-like or reference-like base variants.
    ReferenceWithSumItem(
        ReferencePathType,
        MaxReferenceHop,
        SumValue,
        Option<ElementFlags>,
    ),
    /// Count-indexed tree: a `CountTree`-style primary Merk paired with a
    /// secondary Merk keyed by `(count_be ‖ original_key)` for ordered
    /// count-based queries. Both Merks contribute to the element's
    /// `combined_value_hash` via the three-input hash composition.
    ///
    /// Fields: `(primary_root_key, secondary_root_key, count_value, flags)`
    /// - `primary_root_key`: root key of the primary count Merk.
    /// - `secondary_root_key`: root key of the secondary count-ordered Merk.
    /// - `count_value`: aggregated count of the primary Merk.
    /// - `flags`: optional per-element metadata.
    ///
    /// Variant order in this enum determines bincode's variant-index
    /// encoding on disk; `CountIndexedTree` is placed AFTER
    /// `ReferenceWithSumItem` (variant index 18). This variant gets
    /// index 19, matching `ElementType::CountIndexedTree`.
    CountIndexedTree(
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        CountValue,
        Option<ElementFlags>,
    ),
    /// Provable count-indexed tree: same as `CountIndexedTree` but the
    /// primary Merk uses `ProvableCountedMerkNode` (count baked into node
    /// hash). The secondary Merk's per-node feature type is
    /// `ProvableCountedMerkNode(1)` in both variants — this lets aggregate
    /// count queries against the secondary work uniformly.
    ///
    /// Fields: `(primary_root_key, secondary_root_key, count_value, flags)`
    ProvableCountIndexedTree(
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        CountValue,
        Option<ElementFlags>,
    ),
}

pub fn hex_to_ascii(hex_value: &[u8]) -> String {
    // Define the set of allowed characters
    const ALLOWED_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ\
                                  abcdefghijklmnopqrstuvwxyz\
                                  0123456789_-/\\[]@";

    // Check if all characters in hex_value are allowed
    if hex_value.iter().all(|&c| ALLOWED_CHARS.contains(&c)) {
        // Try to convert to UTF-8
        String::from_utf8(hex_value.to_vec())
            .unwrap_or_else(|_| format!("0x{}", hex::encode(hex_value)))
    } else {
        // Hex encode and prepend "0x"
        format!("0x{}", hex::encode(hex_value))
    }
}

impl fmt::Display for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Element::Item(data, flags) => {
                write!(
                    f,
                    "Item({}{})",
                    hex_to_ascii(data),
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::Reference(path, max_hop, flags) => {
                write!(
                    f,
                    "Reference({}, max_hop: {}{})",
                    path,
                    max_hop.map_or("None".to_string(), |h| h.to_string()),
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::Tree(root_key, flags) => {
                write!(
                    f,
                    "Tree({}{})",
                    root_key.as_ref().map_or("None".to_string(), hex::encode),
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::SumItem(sum_value, flags) => {
                write!(
                    f,
                    "SumItem({}{})",
                    sum_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::SumTree(root_key, sum_value, flags) => {
                write!(
                    f,
                    "SumTree({}, {}{})",
                    root_key.as_ref().map_or("None".to_string(), hex::encode),
                    sum_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::BigSumTree(root_key, sum_value, flags) => {
                write!(
                    f,
                    "BigSumTree({}, {}{})",
                    root_key.as_ref().map_or("None".to_string(), hex::encode),
                    sum_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::CountTree(root_key, count_value, flags) => {
                write!(
                    f,
                    "CountTree({}, {}{})",
                    root_key.as_ref().map_or("None".to_string(), hex::encode),
                    count_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::CountSumTree(root_key, count_value, sum_value, flags) => {
                write!(
                    f,
                    "CountSumTree({}, {}, {}{})",
                    root_key.as_ref().map_or("None".to_string(), hex::encode),
                    count_value,
                    sum_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::ProvableCountTree(root_key, count_value, flags) => {
                write!(
                    f,
                    "ProvableCountTree({}, {}{})",
                    root_key.as_ref().map_or("None".to_string(), hex::encode),
                    count_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::ProvableCountSumTree(root_key, count_value, sum_value, flags) => {
                write!(
                    f,
                    "ProvableCountSumTree({}, {}, {}{})",
                    root_key.as_ref().map_or("None".to_string(), hex::encode),
                    count_value,
                    sum_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::ItemWithSumItem(data, sum_value, flags) => {
                write!(
                    f,
                    "ItemWithSumItem({} , {}{})",
                    hex_to_ascii(data),
                    sum_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::CommitmentTree(total_count, chunk_power, flags) => {
                write!(
                    f,
                    "CommitmentTree(count: {}, chunk_power: {}{})",
                    total_count,
                    chunk_power,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::MmrTree(mmr_size, flags) => {
                write!(
                    f,
                    "MmrTree(mmr_size: {}{})",
                    mmr_size,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::BulkAppendTree(total_count, chunk_power, flags) => {
                write!(
                    f,
                    "BulkAppendTree(total_count: {}, chunk_power: {}{})",
                    total_count,
                    chunk_power,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::DenseAppendOnlyFixedSizeTree(count, height, flags) => {
                write!(
                    f,
                    "DenseAppendOnlyFixedSizeTree(count: {}, height: {}{})",
                    count,
                    height,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::NonCounted(inner) => {
                write!(f, "NonCounted({})", inner)
            }
            Element::CountIndexedTree(primary_root_key, secondary_root_key, count_value, flags) => {
                write!(
                    f,
                    "CountIndexedTree(primary={}, secondary={}, count={}{})",
                    primary_root_key
                        .as_ref()
                        .map_or("None".to_string(), hex::encode),
                    secondary_root_key
                        .as_ref()
                        .map_or("None".to_string(), hex::encode),
                    count_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::ProvableCountIndexedTree(
                primary_root_key,
                secondary_root_key,
                count_value,
                flags,
            ) => {
                write!(
                    f,
                    "ProvableCountIndexedTree(primary={}, secondary={}, count={}{})",
                    primary_root_key
                        .as_ref()
                        .map_or("None".to_string(), hex::encode),
                    secondary_root_key
                        .as_ref()
                        .map_or("None".to_string(), hex::encode),
                    count_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::NotSummed(inner) => {
                write!(f, "NotSummed({})", inner)
            }
            Element::NotCountedOrSummed(inner) => {
                write!(f, "NotCountedOrSummed({})", inner)
            }
            Element::ReferenceWithSumItem(path, max_hop, sum_value, flags) => {
                write!(
                    f,
                    "ReferenceWithSumItem({}, max_hop: {}, sum: {}{})",
                    path,
                    max_hop.map_or("None".to_string(), |h| h.to_string()),
                    sum_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
        }
    }
}

impl Element {
    /// Returns the ElementType for this element.
    ///
    /// For `Element::NonCounted(inner)`, returns the matching `NonCountedXxx`
    /// twin of the inner element's type (high bit set). Nested wrappers are
    /// disallowed at construction and serialization, so the inner is always
    /// a base element.
    pub fn element_type(&self) -> ElementType {
        match self {
            Element::Item(..) => ElementType::Item,
            Element::Reference(..) => ElementType::Reference,
            Element::Tree(..) => ElementType::Tree,
            Element::SumItem(..) => ElementType::SumItem,
            Element::SumTree(..) => ElementType::SumTree,
            Element::BigSumTree(..) => ElementType::BigSumTree,
            Element::CountTree(..) => ElementType::CountTree,
            Element::CountSumTree(..) => ElementType::CountSumTree,
            Element::ProvableCountTree(..) => ElementType::ProvableCountTree,
            Element::ProvableCountSumTree(..) => ElementType::ProvableCountSumTree,
            Element::ItemWithSumItem(..) => ElementType::ItemWithSumItem,
            Element::CommitmentTree(..) => ElementType::CommitmentTree,
            Element::MmrTree(..) => ElementType::MmrTree,
            Element::BulkAppendTree(..) => ElementType::BulkAppendTree,
            Element::DenseAppendOnlyFixedSizeTree(..) => ElementType::DenseAppendOnlyFixedSizeTree,
            Element::ReferenceWithSumItem(..) => ElementType::ReferenceWithSumItem,
            Element::CountIndexedTree(..) => ElementType::CountIndexedTree,
            Element::ProvableCountIndexedTree(..) => ElementType::ProvableCountIndexedTree,
            Element::NonCounted(inner) => match inner.element_type() {
                ElementType::Item => ElementType::NonCountedItem,
                ElementType::Reference => ElementType::NonCountedReference,
                ElementType::Tree => ElementType::NonCountedTree,
                ElementType::SumItem => ElementType::NonCountedSumItem,
                ElementType::SumTree => ElementType::NonCountedSumTree,
                ElementType::BigSumTree => ElementType::NonCountedBigSumTree,
                ElementType::CountTree => ElementType::NonCountedCountTree,
                ElementType::CountSumTree => ElementType::NonCountedCountSumTree,
                ElementType::ProvableCountTree => ElementType::NonCountedProvableCountTree,
                ElementType::ItemWithSumItem => ElementType::NonCountedItemWithSumItem,
                ElementType::ProvableCountSumTree => ElementType::NonCountedProvableCountSumTree,
                ElementType::CommitmentTree => ElementType::NonCountedCommitmentTree,
                ElementType::MmrTree => ElementType::NonCountedMmrTree,
                ElementType::BulkAppendTree => ElementType::NonCountedBulkAppendTree,
                ElementType::DenseAppendOnlyFixedSizeTree => {
                    ElementType::NonCountedDenseAppendOnlyFixedSizeTree
                }
                ElementType::ReferenceWithSumItem => ElementType::NonCountedReferenceWithSumItem,
                ElementType::CountIndexedTree => ElementType::NonCountedCountIndexedTree,
                ElementType::ProvableCountIndexedTree => {
                    ElementType::NonCountedProvableCountIndexedTree
                }
                // Inner is always a base type — nested wrappers are
                // forbidden at construction and (de)serialization.
                already_non_counted => already_non_counted,
            },
            Element::NotSummed(inner) => match inner.element_type() {
                ElementType::SumTree => ElementType::NotSummedSumTree,
                ElementType::BigSumTree => ElementType::NotSummedBigSumTree,
                ElementType::CountSumTree => ElementType::NotSummedCountSumTree,
                ElementType::ProvableCountSumTree => ElementType::NotSummedProvableCountSumTree,
                // Inner is always one of the 4 sum-tree variants above —
                // construction and (de)serialization forbid anything else.
                // Returning the inner type is the safest fallback for the
                // unreachable case.
                other => other,
            },
            Element::NotCountedOrSummed(inner) => match inner.element_type() {
                ElementType::SumTree => ElementType::NotCountedOrSummedSumTree,
                ElementType::BigSumTree => ElementType::NotCountedOrSummedBigSumTree,
                ElementType::CountSumTree => ElementType::NotCountedOrSummedCountSumTree,
                ElementType::ProvableCountSumTree => {
                    ElementType::NotCountedOrSummedProvableCountSumTree
                }
                // Inner is always one of the 4 sum-tree variants above —
                // see comment on the NotSummed arm above.
                other => other,
            },
        }
    }

    pub fn type_str(&self) -> &str {
        self.element_type().as_str()
    }

    /// Verify the wrapper invariants for `self`:
    /// - `NonCounted`, `NotSummed`, and `NotCountedOrSummed` may not nest
    ///   in any combination.
    /// - `NotSummed` and `NotCountedOrSummed` may only wrap one of the four
    ///   sum-tree variants (`SumTree`, `BigSumTree`, `CountSumTree`,
    ///   `ProvableCountSumTree`).
    ///
    /// Constructors and the `serialize`/`deserialize` paths already enforce
    /// these rules; this helper exists so external callers (most importantly
    /// the manual `serde::Deserialize` impl) can apply the same checks.
    ///
    /// Only the immediate wrapper layer is checked — wrapper nesting is
    /// forbidden by these very rules, so deeper recursion is not needed.
    pub fn validate_wrapper_invariants(&self) -> Result<(), crate::error::ElementError> {
        match self {
            Element::NonCounted(inner) => {
                if matches!(
                    **inner,
                    Element::NonCounted(_) | Element::NotSummed(_) | Element::NotCountedOrSummed(_)
                ) {
                    return Err(crate::error::ElementError::InvalidInput(
                        "NonCounted cannot wrap another wrapper",
                    ));
                }
            }
            Element::NotSummed(inner) => match **inner {
                Element::SumTree(..)
                | Element::BigSumTree(..)
                | Element::CountSumTree(..)
                | Element::ProvableCountSumTree(..) => {}
                _ => {
                    return Err(crate::error::ElementError::InvalidInput(
                        "NotSummed inner element must be a sum-tree variant (SumTree, \
                         BigSumTree, CountSumTree, or ProvableCountSumTree)",
                    ));
                }
            },
            Element::NotCountedOrSummed(inner) => match **inner {
                Element::SumTree(..)
                | Element::BigSumTree(..)
                | Element::CountSumTree(..)
                | Element::ProvableCountSumTree(..) => {}
                _ => {
                    return Err(crate::error::ElementError::InvalidInput(
                        "NotCountedOrSummed inner element must be a sum-tree variant \
                         (SumTree, BigSumTree, CountSumTree, or ProvableCountSumTree)",
                    ));
                }
            },
            _ => {}
        }
        Ok(())
    }
}

#[cfg(feature = "serde")]
mod serde_impl {
    //! Manual `serde::Deserialize` for [`Element`] that enforces the same
    //! wrapper invariants as [`Element::deserialize`]. Without this, a serde
    //! payload could construct invalid `NotSummed(Item)` or cross-wrapper
    //! values that the rest of the system assumes do not exist.
    //!
    //! ### Why a shadow enum?
    //!
    //! Serde does not provide a derive-and-validate passthrough for whole
    //! types. The relevant attributes don't fit:
    //! - `#[serde(try_from = "...")]` needs a *separate* source type — you
    //!   can't point it back at `Self` (infinite recursion at the type
    //!   level).
    //! - `#[serde(remote = "...")]` is only for foreign types you don't own.
    //! - `#[serde(deserialize_with = "...")]` is field-level, not type-level.
    //! - `#[serde(transparent)]` / `#[serde(flatten)]` are for single-field
    //!   structs and field embedding respectively.
    //!
    //! That leaves three real options: (1) duplicate the variants in a
    //! shadow type and convert via `From`, (2) write a manual `Visitor`
    //! that walks all 17 variants by hand, or (3) drop the `Deserialize`
    //! derive entirely. The shadow is the shortest of (1) and (2), and
    //! we keep `Deserialize` because external tooling consumers may rely
    //! on it.
    //!
    //! ### Approach
    //!
    //! A private shadow enum (`ElementShadow`) mirrors `Element` verbatim
    //! with `#[serde(rename = "Element")]` so the wire format is identical.
    //! The derived `Deserialize` on the shadow handles the recursive
    //! descent; we then convert to `Element` and call
    //! [`Element::check_recursive_wrapper_invariants`].
    //!
    //! Each `Box<ElementShadow>` field deserializes through the shadow's
    //! own `Deserialize` impl, so validation fires at every level of the
    //! tree as the conversion unwinds.

    use serde::de::Error as _;

    use super::{BigSumValue, CountValue, Element, ElementFlags, MaxReferenceHop, SumValue};
    use crate::reference_path::ReferencePathType;

    #[derive(serde::Deserialize)]
    #[serde(rename = "Element")]
    enum ElementShadow {
        Item(Vec<u8>, Option<ElementFlags>),
        Reference(ReferencePathType, MaxReferenceHop, Option<ElementFlags>),
        Tree(Option<Vec<u8>>, Option<ElementFlags>),
        SumItem(SumValue, Option<ElementFlags>),
        SumTree(Option<Vec<u8>>, SumValue, Option<ElementFlags>),
        BigSumTree(Option<Vec<u8>>, BigSumValue, Option<ElementFlags>),
        CountTree(Option<Vec<u8>>, CountValue, Option<ElementFlags>),
        CountSumTree(Option<Vec<u8>>, CountValue, SumValue, Option<ElementFlags>),
        ProvableCountTree(Option<Vec<u8>>, CountValue, Option<ElementFlags>),
        ItemWithSumItem(Vec<u8>, SumValue, Option<ElementFlags>),
        ProvableCountSumTree(Option<Vec<u8>>, CountValue, SumValue, Option<ElementFlags>),
        CommitmentTree(u64, u8, Option<ElementFlags>),
        MmrTree(u64, Option<ElementFlags>),
        BulkAppendTree(u64, u8, Option<ElementFlags>),
        DenseAppendOnlyFixedSizeTree(u16, u8, Option<ElementFlags>),
        NonCounted(Box<ElementShadow>),
        NotSummed(Box<ElementShadow>),
        NotCountedOrSummed(Box<ElementShadow>),
        ReferenceWithSumItem(
            ReferencePathType,
            MaxReferenceHop,
            SumValue,
            Option<ElementFlags>,
        ),
        CountIndexedTree(
            Option<Vec<u8>>,
            Option<Vec<u8>>,
            CountValue,
            Option<ElementFlags>,
        ),
        ProvableCountIndexedTree(
            Option<Vec<u8>>,
            Option<Vec<u8>>,
            CountValue,
            Option<ElementFlags>,
        ),
    }

    impl From<ElementShadow> for Element {
        fn from(s: ElementShadow) -> Self {
            match s {
                ElementShadow::Item(v, f) => Element::Item(v, f),
                ElementShadow::Reference(p, h, f) => Element::Reference(p, h, f),
                ElementShadow::Tree(k, f) => Element::Tree(k, f),
                ElementShadow::SumItem(v, f) => Element::SumItem(v, f),
                ElementShadow::SumTree(k, s, f) => Element::SumTree(k, s, f),
                ElementShadow::BigSumTree(k, s, f) => Element::BigSumTree(k, s, f),
                ElementShadow::CountTree(k, c, f) => Element::CountTree(k, c, f),
                ElementShadow::CountSumTree(k, c, s, f) => Element::CountSumTree(k, c, s, f),
                ElementShadow::ProvableCountTree(k, c, f) => Element::ProvableCountTree(k, c, f),
                ElementShadow::ItemWithSumItem(v, s, f) => Element::ItemWithSumItem(v, s, f),
                ElementShadow::ProvableCountSumTree(k, c, s, f) => {
                    Element::ProvableCountSumTree(k, c, s, f)
                }
                ElementShadow::CommitmentTree(c, p, f) => Element::CommitmentTree(c, p, f),
                ElementShadow::MmrTree(s, f) => Element::MmrTree(s, f),
                ElementShadow::BulkAppendTree(c, p, f) => Element::BulkAppendTree(c, p, f),
                ElementShadow::DenseAppendOnlyFixedSizeTree(c, h, f) => {
                    Element::DenseAppendOnlyFixedSizeTree(c, h, f)
                }
                ElementShadow::NonCounted(inner) => {
                    Element::NonCounted(Box::new(Element::from(*inner)))
                }
                ElementShadow::NotSummed(inner) => {
                    Element::NotSummed(Box::new(Element::from(*inner)))
                }
                ElementShadow::NotCountedOrSummed(inner) => {
                    Element::NotCountedOrSummed(Box::new(Element::from(*inner)))
                }
                ElementShadow::ReferenceWithSumItem(p, h, s, f) => {
                    Element::ReferenceWithSumItem(p, h, s, f)
                }
                ElementShadow::CountIndexedTree(pk, sk, c, f) => {
                    Element::CountIndexedTree(pk, sk, c, f)
                }
                ElementShadow::ProvableCountIndexedTree(pk, sk, c, f) => {
                    Element::ProvableCountIndexedTree(pk, sk, c, f)
                }
            }
        }
    }

    impl<'de> serde::Deserialize<'de> for Element {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let shadow = ElementShadow::deserialize(deserializer)?;
            let element = Element::from(shadow);
            // Validate immediate wrapper invariants. Inner elements were
            // built by recursive `From<ElementShadow>` calls, so the check
            // at each level catches a violation at any depth.
            Self::check_recursive_wrapper_invariants(&element).map_err(D::Error::custom)?;
            Ok(element)
        }
    }

    impl Element {
        /// Walk the element tree and run `validate_wrapper_invariants` at
        /// every level. Used by the manual `serde::Deserialize`; bincode
        /// goes through `Element::deserialize` which already validates the
        /// outer wrapper plus a leading-byte pre-check that rejects
        /// nesting before recursion.
        pub(super) fn check_recursive_wrapper_invariants(
            element: &Element,
        ) -> Result<(), crate::error::ElementError> {
            element.validate_wrapper_invariants()?;
            if let Element::NonCounted(inner)
            | Element::NotSummed(inner)
            | Element::NotCountedOrSummed(inner) = element
            {
                Self::check_recursive_wrapper_invariants(inner)?;
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Round-trip a valid `Element` through serde JSON and back.
        #[test]
        fn serde_round_trip_valid_elements() {
            let cases = vec![
                Element::Item(b"abc".to_vec(), None),
                Element::SumTree(Some(b"r".to_vec()), 42, None),
                Element::new_non_counted(Element::Item(b"x".to_vec(), None)).unwrap(),
                Element::new_not_summed(Element::SumTree(None, 100, None)).unwrap(),
                Element::new_not_counted_or_summed(Element::CountSumTree(None, 3, 100, None))
                    .unwrap(),
                Element::new_not_counted_or_summed(Element::SumTree(None, 50, None)).unwrap(),
            ];
            for original in cases {
                let json = serde_json::to_string(&original).expect("serialize");
                let back: Element = serde_json::from_str(&json).expect("deserialize");
                assert_eq!(back, original, "round trip mismatch for {:?}", original);
            }
        }

        /// `NotCountedOrSummed(NotCountedOrSummed(_))` and cross-nestings
        /// involving the new wrapper must be rejected.
        #[test]
        fn serde_rejects_not_counted_or_summed_nesting() {
            // Self-nest.
            let payload =
                r#"{"NotCountedOrSummed":{"NotCountedOrSummed":{"SumTree":[null,0,null]}}}"#;
            assert!(serde_json::from_str::<Element>(payload).is_err());

            // Cross with NonCounted (both directions).
            let payload = r#"{"NotCountedOrSummed":{"NonCounted":{"SumTree":[null,0,null]}}}"#;
            assert!(serde_json::from_str::<Element>(payload).is_err());
            let payload = r#"{"NonCounted":{"NotCountedOrSummed":{"SumTree":[null,0,null]}}}"#;
            assert!(serde_json::from_str::<Element>(payload).is_err());

            // Cross with NotSummed (both directions).
            let payload = r#"{"NotCountedOrSummed":{"NotSummed":{"SumTree":[null,0,null]}}}"#;
            assert!(serde_json::from_str::<Element>(payload).is_err());
            let payload = r#"{"NotSummed":{"NotCountedOrSummed":{"SumTree":[null,0,null]}}}"#;
            assert!(serde_json::from_str::<Element>(payload).is_err());
        }

        /// `NotCountedOrSummed(non_sum_tree)` must be rejected for every
        /// illegal inner type.
        #[test]
        fn serde_rejects_not_counted_or_summed_with_non_sum_tree_inner() {
            let illegal_inners = [
                r#"{"Item":[[120],null]}"#,
                r#"{"SumItem":[7,null]}"#,
                r#"{"Tree":[null,null]}"#,
                r#"{"CountTree":[null,0,null]}"#,
                r#"{"ProvableCountTree":[null,0,null]}"#,
                r#"{"MmrTree":[0,null]}"#,
            ];
            for inner in illegal_inners {
                let payload = format!(r#"{{"NotCountedOrSummed":{}}}"#, inner);
                let result: Result<Element, _> = serde_json::from_str(&payload);
                assert!(
                    result.is_err(),
                    "NotCountedOrSummed({}) must be rejected; got {:?}",
                    inner,
                    result
                );
            }
        }

        /// A serde payload that constructs a nested `NonCounted(NonCounted)`
        /// must be rejected.
        #[test]
        fn serde_rejects_nested_non_counted() {
            // Wire payload: NonCounted(NonCounted(Item))
            let json = r#"{"NonCounted":{"NonCounted":{"Item":[[120],null]}}}"#;
            let result: Result<Element, _> = serde_json::from_str(json);
            assert!(
                result.is_err(),
                "nested NonCounted must be rejected, got {:?}",
                result
            );
        }

        /// `NotSummed(NotSummed(_))` must be rejected.
        #[test]
        fn serde_rejects_nested_not_summed() {
            let json = r#"{"NotSummed":{"NotSummed":{"SumTree":[null,0,null]}}}"#;
            let result: Result<Element, _> = serde_json::from_str(json);
            assert!(result.is_err(), "got {:?}", result);
        }

        /// `NonCounted(NotSummed(_))` and `NotSummed(NonCounted(_))` must
        /// both be rejected — wrappers are mutually exclusive.
        #[test]
        fn serde_rejects_cross_wrapper_nesting() {
            let cross_a = r#"{"NonCounted":{"NotSummed":{"SumTree":[null,0,null]}}}"#;
            let result: Result<Element, _> = serde_json::from_str(cross_a);
            assert!(
                result.is_err(),
                "NonCounted(NotSummed) must be rejected; got {:?}",
                result
            );

            let cross_b = r#"{"NotSummed":{"NonCounted":{"SumTree":[null,0,null]}}}"#;
            let result: Result<Element, _> = serde_json::from_str(cross_b);
            assert!(
                result.is_err(),
                "NotSummed(NonCounted) must be rejected; got {:?}",
                result
            );
        }

        /// `NotSummed(non_sum_tree)` must be rejected for every illegal
        /// inner type.
        #[test]
        fn serde_rejects_not_summed_with_non_sum_tree_inner() {
            let illegal_inners = [
                r#"{"Item":[[120],null]}"#,
                r#"{"SumItem":[7,null]}"#,
                r#"{"Tree":[null,null]}"#,
                r#"{"CountTree":[null,0,null]}"#,
                r#"{"ProvableCountTree":[null,0,null]}"#,
                r#"{"MmrTree":[0,null]}"#,
            ];
            for inner in illegal_inners {
                let payload = format!(r#"{{"NotSummed":{}}}"#, inner);
                let result: Result<Element, _> = serde_json::from_str(&payload);
                assert!(
                    result.is_err(),
                    "NotSummed({}) must be rejected; got {:?}",
                    inner,
                    result
                );
            }
        }

        /// Deeply-nested wrapper payloads must be rejected (recursion bound).
        /// This pairs with the bincode pre-check; for serde we rely on the
        /// recursive `From<ElementShadow>` calls hitting validation at each
        /// level. With the immediate-level check at every step, the top-level
        /// `NonCounted(NonCounted(...))` rejects without recursing through
        /// the rest.
        #[test]
        fn serde_rejects_deeply_nested_wrapper_chain() {
            // Build NonCounted(NonCounted(NonCounted(...(Item)))).
            let depth = 64;
            let mut payload = r#"{"Item":[[120],null]}"#.to_string();
            for _ in 0..depth {
                payload = format!(r#"{{"NonCounted":{}}}"#, payload);
            }
            let result: Result<Element, _> = serde_json::from_str(&payload);
            assert!(result.is_err(), "depth-{} chain must be rejected", depth);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_renders_not_summed_wrapper() {
        let inner = Element::SumTree(Some(b"r".to_vec()), 100, None);
        let wrapped = Element::new_not_summed(inner).expect("wrap ok");
        let s = format!("{}", wrapped);
        assert!(s.starts_with("NotSummed("), "got: {}", s);
        assert!(s.contains("SumTree"), "got: {}", s);
    }

    #[test]
    fn element_type_resolves_not_summed_twins() {
        // The four sum-tree variants each map to their NotSummed twin.
        let cases: [(Element, ElementType); 4] = [
            (
                Element::NotSummed(Box::new(Element::SumTree(None, 0, None))),
                ElementType::NotSummedSumTree,
            ),
            (
                Element::NotSummed(Box::new(Element::BigSumTree(None, 0, None))),
                ElementType::NotSummedBigSumTree,
            ),
            (
                Element::NotSummed(Box::new(Element::CountSumTree(None, 0, 0, None))),
                ElementType::NotSummedCountSumTree,
            ),
            (
                Element::NotSummed(Box::new(Element::ProvableCountSumTree(None, 0, 0, None))),
                ElementType::NotSummedProvableCountSumTree,
            ),
        ];
        for (element, expected) in cases {
            assert_eq!(element.element_type(), expected);
            assert_eq!(element.type_str(), expected.as_str());
        }
    }

    #[test]
    fn element_type_resolves_non_counted_twins() {
        // Spot-check a few NonCounted twins to lock in the dispatch.
        assert_eq!(
            Element::NonCounted(Box::new(Element::Item(b"x".to_vec(), None))).element_type(),
            ElementType::NonCountedItem
        );
        assert_eq!(
            Element::NonCounted(Box::new(Element::SumTree(None, 0, None))).element_type(),
            ElementType::NonCountedSumTree
        );
        assert_eq!(
            Element::NonCounted(Box::new(Element::ProvableCountSumTree(None, 0, 0, None)))
                .element_type(),
            ElementType::NonCountedProvableCountSumTree
        );
    }

    #[test]
    fn display_renders_non_counted_wrapper() {
        let inner = Element::Item(b"abc".to_vec(), None);
        let wrapped = Element::new_non_counted(inner).expect("wrap ok");
        let s = format!("{}", wrapped);
        assert!(s.starts_with("NonCounted("), "got: {}", s);
    }

    #[test]
    fn display_renders_not_counted_or_summed_wrapper() {
        let inner = Element::CountSumTree(Some(b"r".to_vec()), 3, 100, None);
        let wrapped = Element::new_not_counted_or_summed(inner).expect("wrap ok");
        let s = format!("{}", wrapped);
        assert!(s.starts_with("NotCountedOrSummed("), "got: {}", s);
        assert!(s.contains("CountSumTree"), "got: {}", s);
    }

    #[test]
    fn element_type_resolves_not_counted_or_summed_twins() {
        // Each of the four sum-tree variants maps to its
        // NotCountedOrSummed twin.
        let cases: [(Element, ElementType); 4] = [
            (
                Element::NotCountedOrSummed(Box::new(Element::SumTree(None, 0, None))),
                ElementType::NotCountedOrSummedSumTree,
            ),
            (
                Element::NotCountedOrSummed(Box::new(Element::BigSumTree(None, 0, None))),
                ElementType::NotCountedOrSummedBigSumTree,
            ),
            (
                Element::NotCountedOrSummed(Box::new(Element::CountSumTree(None, 0, 0, None))),
                ElementType::NotCountedOrSummedCountSumTree,
            ),
            (
                Element::NotCountedOrSummed(Box::new(Element::ProvableCountSumTree(
                    None, 0, 0, None,
                ))),
                ElementType::NotCountedOrSummedProvableCountSumTree,
            ),
        ];
        for (element, expected) in cases {
            assert_eq!(element.element_type(), expected);
            assert_eq!(element.type_str(), expected.as_str());
        }
    }

    #[test]
    fn validate_wrapper_invariants_covers_all_arms() {
        // Valid: bare elements pass.
        assert!(Element::Item(b"x".to_vec(), None)
            .validate_wrapper_invariants()
            .is_ok());

        // Valid NonCounted/NotSummed/NotCountedOrSummed pass.
        let nc = Element::NonCounted(Box::new(Element::Item(b"x".to_vec(), None)));
        assert!(nc.validate_wrapper_invariants().is_ok());
        let ns = Element::NotSummed(Box::new(Element::SumTree(None, 0, None)));
        assert!(ns.validate_wrapper_invariants().is_ok());
        let ncos = Element::NotCountedOrSummed(Box::new(Element::SumTree(None, 0, None)));
        assert!(ncos.validate_wrapper_invariants().is_ok());

        // Invalid: NonCounted wrapping another wrapper.
        let bad = Element::NonCounted(Box::new(Element::NotCountedOrSummed(Box::new(
            Element::SumTree(None, 0, None),
        ))));
        assert!(bad.validate_wrapper_invariants().is_err());

        // Invalid: NotSummed with non-sum-tree inner.
        let bad = Element::NotSummed(Box::new(Element::Item(b"x".to_vec(), None)));
        assert!(bad.validate_wrapper_invariants().is_err());

        // Invalid: NotCountedOrSummed with non-sum-tree inner.
        let bad = Element::NotCountedOrSummed(Box::new(Element::Item(b"x".to_vec(), None)));
        assert!(bad.validate_wrapper_invariants().is_err());
    }
}

#[cfg(test)]
mod cidx_tests;
