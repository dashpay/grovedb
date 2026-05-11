//! Element type enum for efficient type checking from serialized bytes.

use crate::error::ElementError;

/// Bincode discriminant byte for `Element::NonCounted`. Tied to the
/// declaration order of the `Element` enum (0-indexed, 16th variant).
///
/// `ElementType` does NOT have a direct variant at this byte — when
/// `from_serialized_value` sees this discriminant, it reads the next byte to
/// resolve the inner type and returns a synthetic `NonCountedXxx` variant
/// (high bit set). The `test_element_serialization_discriminants_match_element_type`
/// test pins this constant to the actual bincode encoding.
pub const NON_COUNTED_WRAPPER_DISCRIMINANT: u8 = 15;

/// High bit set on every `NonCountedXxx` discriminant. The base type can be
/// recovered with `disc & NON_COUNTED_BASE_MASK`, and "is non-counted" is
/// `disc & NON_COUNTED_FLAG != 0`.
pub const NON_COUNTED_FLAG: u8 = 0x80;

/// Mask to recover the base type discriminant from a `NonCountedXxx`
/// discriminant.
pub const NON_COUNTED_BASE_MASK: u8 = 0x7F;

/// Bincode discriminant byte for `Element::NotSummed`. Tied to the
/// declaration order of the `Element` enum (0-indexed, 17th variant).
///
/// Like `NON_COUNTED_WRAPPER_DISCRIMINANT`, this byte has no direct
/// `ElementType` variant. `from_serialized_value` reads the next byte and
/// resolves to one of the four `NotSummedXxx` synthetic twins. Only the four
/// sum-tree base discriminants are legal as the inner byte.
pub const NOT_SUMMED_WRAPPER_DISCRIMINANT: u8 = 16;

/// Twin-discriminant prefix for `NotSummedXxx` types: every twin is encoded
/// as `NOT_SUMMED_TWIN_PREFIX | base`. The prefix uses the top three bits
/// (`1010_xxxxx`) so the family spans `0xA0..=0xBF`, leaving the low 5 bits
/// for the base discriminant. Detection is an upper-3-bit compare:
/// `disc & 0xe0 == 0xa0`. This keeps the NotSummed range disjoint from the
/// NonCounted range (`0x80..=0x9F`, matched via `disc & 0xe0 == 0x80`).
///
/// The earlier revision used a 4-bit prefix `0xb0` with a 4-bit base mask
/// `0x0F`, which could only encode bases 0..=15. Widening to 5 bits made
/// room for `NotSummedProvableSumTree` (base 17, twin 0xB1 = 177) without
/// having to introduce a second prefix range.
pub const NOT_SUMMED_TWIN_PREFIX: u8 = 0xa0;

/// Mask to recover the base type discriminant from a `NotSummedXxx`
/// discriminant. Base discriminants reach up to 17 (ProvableSumTree) so
/// 5 bits are required; pre-Phase-1.5 this was `0x0F`.
pub const NOT_SUMMED_BASE_MASK: u8 = 0x1F;

/// Indicates which type of proof node should be used when generating proofs.
///
/// This determines whether the verifier will recompute the value hash (secure)
/// or trust the provided value hash (required for combined hashes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofNodeType {
    /// Use `Node::KV` - the verifier will compute `value_hash = H(value)`.
    ///
    /// This is secure because any tampering with the value bytes will cause
    /// the computed hash to differ, failing verification.
    ///
    /// Used for: Item, SumItem, ItemWithSumItem (in regular trees)
    Kv,

    /// Use `Node::KVValueHash` - the verifier trusts the provided value_hash.
    ///
    /// Required because `value_hash = combine_hash(H(value), other_hash)` and
    /// the verifier doesn't have access to `other_hash` at the merk level.
    ///
    /// Security comes from GroveDB's multi-layer proof structure.
    ///
    /// Used for: All tree types (Tree, SumTree, BigSumTree, CountTree,
    ///           CountSumTree, ProvableCountTree) when NOT inside a
    ///           ProvableCountTree parent
    KvValueHash,

    /// Use `Node::KVRefValueHash` - like KVValueHash but for references.
    ///
    /// At the merk layer, this generates `KVValueHash` (since merk doesn't
    /// know about references). GroveDB post-processes these nodes to
    /// `Node::KVRefValueHash` with the dereferenced value.
    ///
    /// Required for references in regular trees because:
    /// 1. They need combined hash for reference resolution
    /// 2. GroveDB needs to identify them for post-processing
    ///
    /// Used for: Reference (in regular trees, not ProvableCountTree)
    KvRefValueHash,

    /// Use `Node::KVCount` - the verifier will compute `value_hash = H(value)`
    /// and include the count in the node hash calculation.
    ///
    /// This is secure because:
    /// 1. Tampering with value bytes causes hash mismatch (like KV)
    /// 2. Tampering with count causes hash mismatch (count is in node_hash)
    ///
    /// Used for: Item, SumItem, ItemWithSumItem (inside ProvableCountTree)
    KvCount,

    /// Use `Node::KVValueHashFeatureType` - like KVValueHash but includes the
    /// feature type (count) in the node hash calculation.
    ///
    /// Required for subtrees inside ProvableCountTree because:
    /// 1. They need combined hash (like KVValueHash) for subtree root hash
    /// 2. They need count included in node_hash for tamper resistance
    ///
    /// Used for: Tree, SumTree, BigSumTree, CountTree, CountSumTree,
    ///           ProvableCountTree (inside ProvableCountTree)
    KvValueHashFeatureType,

    /// Use `Node::KVRefValueHashCount` - like KVRefValueHash but includes
    /// the count in the node hash calculation.
    ///
    /// At the merk layer, this generates `KVValueHashFeatureType` (since merk
    /// doesn't know about references). GroveDB post-processes these nodes to
    /// `Node::KVRefValueHashCount` with the dereferenced value.
    ///
    /// Required for references inside ProvableCountTree because:
    /// 1. They need combined hash (like KVRefValueHash) for reference
    ///    resolution
    /// 2. They need count included in node_hash for tamper resistance
    ///
    /// Used for: Reference (inside ProvableCountTree)
    KvRefValueHashCount,

    /// Use `Node::KVSum` - sum analogue of `KvCount`. The verifier
    /// recomputes `value_hash = H(value)` and includes the i64 sum in the
    /// node hash via `node_hash_with_sum`. Phase 2.
    ///
    /// Used for: Item, SumItem, ItemWithSumItem (inside ProvableSumTree)
    KvSum,

    /// Use `Node::KVRefValueHashSum` - sum analogue of `KvRefValueHashCount`.
    /// At the merk layer, this generates `KVValueHashFeatureType` (since
    /// merk doesn't know about references). GroveDB post-processes these
    /// nodes to `Node::KVRefValueHashSum` with the dereferenced value.
    ///
    /// Used for: Reference (inside ProvableSumTree)
    KvRefValueHashSum,
}

/// Element type discriminants.
///
/// Base types (0..=14, 17) match the bincode serialization order of the
/// `Element` enum. The `Element` enum has indices 15 and 16 reserved for the
/// `NonCounted` and `NotSummed` wrapper variants respectively (neither has a
/// direct `ElementType` variant — they synthesize twin discriminants by
/// reading the inner element's byte).
///
/// Non-counted twins are synthetic — they encode "this is a `NonCounted`
/// wrapper around an inner element of base type `disc & ...`". Twins for
/// base discriminants 0..=14 live in 128..=142 (`0x80 | base`). The
/// twin for `ProvableSumTree` (base 17) is placed at 145 (`0x80 | 17 =
/// 0x91`). All NonCounted twins satisfy `disc & 0xe0 == 0x80` — the upper
/// three bits identify them. The on-disk representation of
/// `Element::NonCounted` still uses the wrapper byte
/// `NON_COUNTED_WRAPPER_DISCRIMINANT` (15) followed by the inner element's
/// bytes; `from_serialized_value` synthesizes the `NonCountedXxx` variant by
/// peeking at the second byte.
///
/// Not-summed twins follow a similar scheme but use the prefix `0xa0` and
/// cover the five sum-tree base discriminants (4, 5, 7, 10, 17), placing
/// them at `164, 165, 167, 170, 177`. The wrapper byte is
/// `NOT_SUMMED_WRAPPER_DISCRIMINANT` (16). The two families are matched by
/// the top three bits: NonCounted occupies `0x80..=0x9F` (`disc & 0xe0 ==
/// 0x80`) and NotSummed lives in `0xA0..=0xBF` (`disc & 0xe0 == 0xa0`).
/// The two wrappers are mutually exclusive — the constructors and
/// (de)serializers reject any nesting in either direction.
///
/// IMPORTANT: Base values (0..=14, 17) must match the order of variants in
/// the `Element` enum. The
/// `test_element_serialization_discriminants_match_element_type` test
/// catches drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ElementType {
    /// An ordinary value - discriminant 0
    Item = 0,
    /// A reference to an object by its path - discriminant 1
    Reference = 1,
    /// A subtree container - discriminant 2
    Tree = 2,
    /// Signed integer value for sum trees - discriminant 3
    SumItem = 3,
    /// Sum tree - discriminant 4
    SumTree = 4,
    /// Big sum tree (i128) - discriminant 5
    BigSumTree = 5,
    /// Count tree - discriminant 6
    CountTree = 6,
    /// Count and sum tree combined - discriminant 7
    CountSumTree = 7,
    /// Provable count tree - discriminant 8
    ProvableCountTree = 8,
    /// Item with sum value - discriminant 9
    ItemWithSumItem = 9,
    /// Provable count sum tree - discriminant 10
    ProvableCountSumTree = 10,
    /// Orchard-style commitment tree - discriminant 11
    CommitmentTree = 11,
    /// MMR (Merkle Mountain Range) tree - discriminant 12
    MmrTree = 12,
    /// Bulk-append tree - discriminant 13
    BulkAppendTree = 13,
    /// Dense fixed-sized Merkle tree - discriminant 14
    DenseAppendOnlyFixedSizeTree = 14,
    // 15 is reserved as the NonCounted wrapper byte and has no direct
    // ElementType variant.
    // 16 is reserved as the NotSummed wrapper byte and has no direct
    // ElementType variant.
    /// Provable sum tree - discriminant 17 (sums baked into node hashes)
    ProvableSumTree = 17,
    /// Non-counted wrapper around `Item` - discriminant 128
    NonCountedItem = 128,
    /// Non-counted wrapper around `Reference` - discriminant 129
    NonCountedReference = 129,
    /// Non-counted wrapper around `Tree` - discriminant 130
    NonCountedTree = 130,
    /// Non-counted wrapper around `SumItem` - discriminant 131
    NonCountedSumItem = 131,
    /// Non-counted wrapper around `SumTree` - discriminant 132
    NonCountedSumTree = 132,
    /// Non-counted wrapper around `BigSumTree` - discriminant 133
    NonCountedBigSumTree = 133,
    /// Non-counted wrapper around `CountTree` - discriminant 134
    NonCountedCountTree = 134,
    /// Non-counted wrapper around `CountSumTree` - discriminant 135
    NonCountedCountSumTree = 135,
    /// Non-counted wrapper around `ProvableCountTree` - discriminant 136
    NonCountedProvableCountTree = 136,
    /// Non-counted wrapper around `ItemWithSumItem` - discriminant 137
    NonCountedItemWithSumItem = 137,
    /// Non-counted wrapper around `ProvableCountSumTree` - discriminant 138
    NonCountedProvableCountSumTree = 138,
    /// Non-counted wrapper around `CommitmentTree` - discriminant 139
    NonCountedCommitmentTree = 139,
    /// Non-counted wrapper around `MmrTree` - discriminant 140
    NonCountedMmrTree = 140,
    /// Non-counted wrapper around `BulkAppendTree` - discriminant 141
    NonCountedBulkAppendTree = 141,
    /// Non-counted wrapper around `DenseAppendOnlyFixedSizeTree` - discriminant 142
    NonCountedDenseAppendOnlyFixedSizeTree = 142,
    /// Non-counted wrapper around `ProvableSumTree` - discriminant 145 (`0x80 | 17`)
    NonCountedProvableSumTree = 145,
    /// Not-summed wrapper around `SumTree` - discriminant 164 (`0xa0 | 4`)
    NotSummedSumTree = 164,
    /// Not-summed wrapper around `BigSumTree` - discriminant 165 (`0xa0 | 5`)
    NotSummedBigSumTree = 165,
    /// Not-summed wrapper around `CountSumTree` - discriminant 167 (`0xa0 | 7`)
    NotSummedCountSumTree = 167,
    /// Not-summed wrapper around `ProvableCountSumTree` - discriminant 170 (`0xa0 | 10`)
    NotSummedProvableCountSumTree = 170,
    /// Not-summed wrapper around `ProvableSumTree` - discriminant 177 (`0xa0 | 17`)
    NotSummedProvableSumTree = 177,
}

impl ElementType {
    /// Get the ElementType from a serialized Element's leading bytes.
    ///
    /// Reads byte 0 in the common case. When byte 0 is the
    /// `NON_COUNTED_WRAPPER_DISCRIMINANT`, also reads byte 1 to resolve the
    /// inner type and returns the corresponding `NonCountedXxx` variant
    /// (with bit 7 set).
    ///
    /// # Arguments
    /// * `serialized_value` - The serialized Element bytes
    ///
    /// # Returns
    /// * `Ok(ElementType)` - The element type
    /// * `Err(ElementError)` - If the value is empty, truncated (wrapper
    ///   without inner byte), or has an unknown discriminant
    pub fn from_serialized_value(serialized_value: &[u8]) -> Result<Self, ElementError> {
        let first_byte = *serialized_value.first().ok_or_else(|| {
            ElementError::CorruptedData("Cannot get element type from empty value".to_string())
        })?;

        if first_byte == NON_COUNTED_WRAPPER_DISCRIMINANT {
            let inner_byte = *serialized_value.get(1).ok_or_else(|| {
                ElementError::CorruptedData(
                    "NonCounted wrapper has no inner element discriminant byte".to_string(),
                )
            })?;
            // The inner discriminant must be a base type. Legal base bytes
            // are 0..=14 plus the new 17 (`ProvableSumTree`). Bytes 15 and
            // 16 are wrapper bytes (nested wrappers forbidden in either
            // direction). 18..=127 are unallocated. 128..=255 are synthetic
            // twin discriminants that never appear on disk; without these
            // checks, the bitwise OR below would collapse
            // `0x80 | inner_byte` into `inner_byte` and a payload like
            // `[15, 128, ...]` would silently parse as `NonCountedItem`.
            let inner_is_legal_base = inner_byte < NON_COUNTED_WRAPPER_DISCRIMINANT
                || inner_byte == 17 /* ProvableSumTree */;
            if !inner_is_legal_base {
                return Err(ElementError::CorruptedData(format!(
                    "NonCounted inner discriminant must be a base type (0..=14 or 17), got {}",
                    inner_byte
                )));
            }
            Self::try_from(NON_COUNTED_FLAG | inner_byte)
        } else if first_byte == NOT_SUMMED_WRAPPER_DISCRIMINANT {
            let inner_byte = *serialized_value.get(1).ok_or_else(|| {
                ElementError::CorruptedData(
                    "NotSummed wrapper has no inner element discriminant byte".to_string(),
                )
            })?;
            // Only the five sum-tree base discriminants are legal here.
            // Anything else — including the wrapper bytes 15/16, the
            // synthetic twin ranges, and the unrelated base types — is
            // rejected so that round-tripping `from_serialized_value` always
            // yields a valid `NotSummedXxx` twin.
            match inner_byte {
                4 | 5 | 7 | 10 | 17 => Self::try_from(NOT_SUMMED_TWIN_PREFIX | inner_byte),
                _ => Err(ElementError::CorruptedData(format!(
                    "NotSummed inner discriminant must be a sum-tree base type \
                     (4=SumTree, 5=BigSumTree, 7=CountSumTree, 10=ProvableCountSumTree, \
                     17=ProvableSumTree), got {}",
                    inner_byte
                ))),
            }
        } else {
            Self::try_from(first_byte)
        }
    }

    /// Returns true if this is a `NonCountedXxx` discriminant.
    ///
    /// The mask checks the top three bits (`& 0xe0 == 0x80`) so the
    /// NonCounted family spans `0x80..=0x9F`. This is wider than the
    /// `0xf0 == 0x80` compare used in earlier revisions in order to make
    /// room for `NonCountedProvableSumTree = 0x91` (twin of base
    /// discriminant 17). The NotSummed family lives at `0xB0..=0xBF`
    /// (`& 0xf0 == 0xb0`) and so is not caught here.
    #[inline]
    pub const fn is_non_counted(self) -> bool {
        (self as u8) & 0xe0 == NON_COUNTED_FLAG
    }

    /// Returns true if this is a `NotSummedXxx` discriminant.
    ///
    /// The mask checks the top three bits (`& 0xe0 == 0xa0`) so the
    /// NotSummed family spans `0xA0..=0xBF`. This is wider than the
    /// `& 0xf0 == 0xb0` compare used pre-Phase-1.5 in order to make room
    /// for `NotSummedProvableSumTree = 0xB1` (twin of base discriminant
    /// 17). NonCounted lives at `0x80..=0x9F` so the two ranges stay
    /// disjoint.
    #[inline]
    pub const fn is_not_summed(self) -> bool {
        (self as u8) & 0xe0 == NOT_SUMMED_TWIN_PREFIX
    }

    /// Returns the underlying base ElementType, stripping any wrapper flag
    /// bits. For base types, returns `self` unchanged.
    ///
    /// The two wrapper twin ranges share bit 7 but are distinguished by the
    /// upper nibble (`0x80` for `NonCounted`, `0xb0` for `NotSummed`).
    /// Constructors and (de)serializers reject any wrapper nesting, so only
    /// one wrapper status is ever set on any valid `ElementType` instance.
    #[inline]
    pub fn base(self) -> ElementType {
        let disc = self as u8;
        if self.is_non_counted() {
            // Safe: every NonCountedXxx is constructed from a valid base
            // discriminant 0..=14, so masking the high bit yields a valid
            // base discriminant.
            ElementType::try_from(disc & NON_COUNTED_BASE_MASK)
                .expect("NonCounted twin always has a valid base")
        } else if self.is_not_summed() {
            // Safe: every NotSummedXxx is constructed from one of the four
            // sum-tree base discriminants {4, 5, 7, 10}.
            ElementType::try_from(disc & NOT_SUMMED_BASE_MASK)
                .expect("NotSummed twin always has a valid base")
        } else {
            self
        }
    }

    /// Returns the type of proof node that should be used for this element
    /// type, given the parent tree type.
    ///
    /// ## Regular trees (Tree, SumTree, BigSumTree, CountTree, CountSumTree)
    ///
    /// All use `node_hash(kv_hash, left, right)` — feature_type is NOT hashed.
    ///
    /// | Role                               | Node type (V0)  | Node type (V1)                       |
    /// |------------------------------------|-----------------|--------------------------------------|
    /// | Queried item                       | `Kv`            | `Kv`                                 |
    /// | Queried non-empty tree (no subqry) | `KvValueHash`   | `KvValueHashFeatureTypeWithChildHash`|
    /// | Queried empty tree                 | `KvValueHash`   | `KvValueHash`                        |
    /// | Queried ref                        | `KvRefValueHash`| `KvRefValueHash`                     |
    ///
    /// Note: non-empty trees WITH a subquery descend into the child layer;
    /// the tree node appears as `KvValueHash` in the parent layer proof.
    ///
    /// ## ProvableCountTree / ProvableCountSumTree
    ///
    /// Use `node_hash_with_count(kv_hash, left, right, count)` — the count
    /// IS included in the hash, so every proof node must carry it.
    ///
    /// | Role                               | Node type (V0)           | Node type (V1)                       |
    /// |------------------------------------|--------------------------|--------------------------------------|
    /// | Queried item                       | `KvCount`                | `KvCount`                            |
    /// | Queried non-empty tree (no subqry) | `KvValueHashFeatureType` | `KvValueHashFeatureTypeWithChildHash`|
    /// | Queried empty tree                 | `KvValueHashFeatureType` | `KvValueHashFeatureType`             |
    /// | Queried ref                        | `KvRefValueHashCount`    | `KvRefValueHashCount`                |
    ///
    /// Non-queried and boundary nodes are handled separately in the merk
    /// proof generation code (`KVHash`/`KVHashCount`, `KVDigest`/`KVDigestCount`).
    ///
    /// See also: docs/book/src/proof-system.md "Proof Node Types by Tree Type"
    ///
    /// # Arguments
    /// * `parent_tree_type` - The type of tree containing this element, or
    ///   `None` for root-level elements
    ///
    /// The `NonCounted` wrapper is transparent for proof-node-type
    /// selection: both `self` and `parent_tree_type` are normalized via
    /// `base()` before dispatch. The proof shape is determined by the inner
    /// element type, not by the wrapper.
    #[inline]
    pub fn proof_node_type(&self, parent_tree_type: Option<ElementType>) -> ProofNodeType {
        let parent_base = parent_tree_type.map(|t| t.base());
        // "Provable aggregate parents" are those that bake the per-node
        // aggregate into the node hash. The count family
        // (`ProvableCountTree`, `ProvableCountSumTree`) hashes the count;
        // the sum family (`ProvableSumTree`, Phase 2) hashes the sum.
        //
        // Phase 2: the dispatch now distinguishes the two families. Item /
        // Reference proof variants diverge (KvSum / KvRefValueHashSum vs
        // KvCount / KvRefValueHashCount). Subtrees inside either family
        // still use `KvValueHashFeatureType` — the feature_type field on
        // that variant carries both the count and sum in their respective
        // tagged TreeFeatureType variants, so a single proof-node variant
        // suffices for the subtree case.
        let is_provable_count_tree = matches!(
            parent_base,
            Some(ElementType::ProvableCountTree) | Some(ElementType::ProvableCountSumTree)
        );
        let is_provable_sum_tree = matches!(parent_base, Some(ElementType::ProvableSumTree));
        let is_provable_aggregate_tree = is_provable_count_tree || is_provable_sum_tree;

        let base = self.base();
        if base.has_simple_value_hash() {
            // Items (Item, SumItem, ItemWithSumItem)
            if is_provable_count_tree {
                ProofNodeType::KvCount
            } else if is_provable_sum_tree {
                ProofNodeType::KvSum
            } else {
                ProofNodeType::Kv
            }
        } else if base.is_reference() {
            // References need combined hash (for reference resolution).
            // In ProvableCountTree they additionally need the count in
            // node_hash; in ProvableSumTree they need the sum.
            // GroveDB post-processes these to KVRefValueHash /
            // KVRefValueHashCount / KVRefValueHashSum.
            if is_provable_count_tree {
                ProofNodeType::KvRefValueHashCount
            } else if is_provable_sum_tree {
                ProofNodeType::KvRefValueHashSum
            } else {
                ProofNodeType::KvRefValueHash
            }
        } else {
            // Subtrees (Tree, SumTree, BigSumTree, CountTree, CountSumTree,
            // ProvableCountTree, ProvableSumTree). KvValueHashFeatureType
            // works for both Count and Sum families because the embedded
            // `TreeFeatureType` carries the aggregate.
            if is_provable_aggregate_tree {
                ProofNodeType::KvValueHashFeatureType
            } else {
                ProofNodeType::KvValueHash
            }
        }
    }

    /// Returns true if this element type uses a simple value hash (H(value)).
    ///
    /// Item types have `value_hash = H(serialized_element)`.
    /// These can safely use `Node::KV` in proofs because the verifier
    /// can recompute the hash from the value. Looks through the
    /// `NonCounted` wrapper.
    #[inline]
    pub fn has_simple_value_hash(&self) -> bool {
        matches!(
            self.base(),
            ElementType::Item | ElementType::SumItem | ElementType::ItemWithSumItem
        )
    }

    /// Returns true if this element type uses a combined value hash.
    ///
    /// Subtrees and References have `value_hash = combine_hash(H(value),
    /// other_hash)`.
    /// - For subtrees: `other_hash` is the child merk root hash
    /// - For references: `other_hash` is the referenced element's value hash
    ///
    /// These must use `Node::KVValueHash` in proofs because the verifier
    /// cannot recompute the combined hash without additional information.
    #[inline]
    pub fn has_combined_value_hash(&self) -> bool {
        !self.has_simple_value_hash()
    }

    /// Returns true if this element type is any kind of tree (subtree).
    /// Looks through the `NonCounted` wrapper.
    #[inline]
    pub fn is_tree(&self) -> bool {
        matches!(
            self.base(),
            ElementType::Tree
                | ElementType::SumTree
                | ElementType::BigSumTree
                | ElementType::CountTree
                | ElementType::CountSumTree
                | ElementType::ProvableCountTree
                | ElementType::ProvableCountSumTree
                | ElementType::ProvableSumTree
                | ElementType::CommitmentTree
                | ElementType::MmrTree
                | ElementType::BulkAppendTree
                | ElementType::DenseAppendOnlyFixedSizeTree
        )
    }

    /// Returns true if this element type is a reference. Looks through the
    /// `NonCounted` wrapper.
    #[inline]
    pub fn is_reference(&self) -> bool {
        matches!(self.base(), ElementType::Reference)
    }

    /// Returns true if this element type is any kind of item (not a tree or
    /// reference). Looks through the `NonCounted` wrapper.
    #[inline]
    pub fn is_item(&self) -> bool {
        matches!(
            self.base(),
            ElementType::Item | ElementType::SumItem | ElementType::ItemWithSumItem
        )
    }

    /// Returns a human-readable string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            ElementType::Item => "item",
            ElementType::Reference => "reference",
            ElementType::Tree => "tree",
            ElementType::SumItem => "sum item",
            ElementType::SumTree => "sum tree",
            ElementType::BigSumTree => "big sum tree",
            ElementType::CountTree => "count tree",
            ElementType::CountSumTree => "count sum tree",
            ElementType::ProvableCountTree => "provable count tree",
            ElementType::ItemWithSumItem => "item with sum item",
            ElementType::ProvableCountSumTree => "provable count sum tree",
            ElementType::CommitmentTree => "commitment tree",
            ElementType::MmrTree => "mmr tree",
            ElementType::BulkAppendTree => "bulk_append_tree",
            ElementType::DenseAppendOnlyFixedSizeTree => "dense_tree",
            ElementType::ProvableSumTree => "provable sum tree",
            ElementType::NonCountedItem => "non_counted item",
            ElementType::NonCountedReference => "non_counted reference",
            ElementType::NonCountedTree => "non_counted tree",
            ElementType::NonCountedSumItem => "non_counted sum item",
            ElementType::NonCountedSumTree => "non_counted sum tree",
            ElementType::NonCountedBigSumTree => "non_counted big sum tree",
            ElementType::NonCountedCountTree => "non_counted count tree",
            ElementType::NonCountedCountSumTree => "non_counted count sum tree",
            ElementType::NonCountedProvableCountTree => "non_counted provable count tree",
            ElementType::NonCountedItemWithSumItem => "non_counted item with sum item",
            ElementType::NonCountedProvableCountSumTree => "non_counted provable count sum tree",
            ElementType::NonCountedCommitmentTree => "non_counted commitment tree",
            ElementType::NonCountedMmrTree => "non_counted mmr tree",
            ElementType::NonCountedBulkAppendTree => "non_counted bulk_append_tree",
            ElementType::NonCountedDenseAppendOnlyFixedSizeTree => "non_counted dense_tree",
            ElementType::NonCountedProvableSumTree => "non_counted provable sum tree",
            ElementType::NotSummedSumTree => "not_summed sum tree",
            ElementType::NotSummedBigSumTree => "not_summed big sum tree",
            ElementType::NotSummedCountSumTree => "not_summed count sum tree",
            ElementType::NotSummedProvableCountSumTree => "not_summed provable count sum tree",
            ElementType::NotSummedProvableSumTree => "not_summed provable sum tree",
        }
    }
}

impl TryFrom<u8> for ElementType {
    type Error = ElementError;

    /// Maps a discriminant byte to an `ElementType`.
    ///
    /// `NON_COUNTED_WRAPPER_DISCRIMINANT` (15) on its own is rejected: it is
    /// the on-disk wrapper byte and must be paired with the inner element's
    /// discriminant byte. Use `from_serialized_value` for that.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ElementType::Item),
            1 => Ok(ElementType::Reference),
            2 => Ok(ElementType::Tree),
            3 => Ok(ElementType::SumItem),
            4 => Ok(ElementType::SumTree),
            5 => Ok(ElementType::BigSumTree),
            6 => Ok(ElementType::CountTree),
            7 => Ok(ElementType::CountSumTree),
            8 => Ok(ElementType::ProvableCountTree),
            9 => Ok(ElementType::ItemWithSumItem),
            10 => Ok(ElementType::ProvableCountSumTree),
            11 => Ok(ElementType::CommitmentTree),
            12 => Ok(ElementType::MmrTree),
            13 => Ok(ElementType::BulkAppendTree),
            14 => Ok(ElementType::DenseAppendOnlyFixedSizeTree),
            // 15 is the raw NonCounted wrapper byte; from_serialized_value
            // resolves it by reading the inner discriminant.
            // 16 is the raw NotSummed wrapper byte.
            17 => Ok(ElementType::ProvableSumTree),
            128 => Ok(ElementType::NonCountedItem),
            129 => Ok(ElementType::NonCountedReference),
            130 => Ok(ElementType::NonCountedTree),
            131 => Ok(ElementType::NonCountedSumItem),
            132 => Ok(ElementType::NonCountedSumTree),
            133 => Ok(ElementType::NonCountedBigSumTree),
            134 => Ok(ElementType::NonCountedCountTree),
            135 => Ok(ElementType::NonCountedCountSumTree),
            136 => Ok(ElementType::NonCountedProvableCountTree),
            137 => Ok(ElementType::NonCountedItemWithSumItem),
            138 => Ok(ElementType::NonCountedProvableCountSumTree),
            139 => Ok(ElementType::NonCountedCommitmentTree),
            140 => Ok(ElementType::NonCountedMmrTree),
            141 => Ok(ElementType::NonCountedBulkAppendTree),
            142 => Ok(ElementType::NonCountedDenseAppendOnlyFixedSizeTree),
            145 => Ok(ElementType::NonCountedProvableSumTree),
            164 => Ok(ElementType::NotSummedSumTree),
            165 => Ok(ElementType::NotSummedBigSumTree),
            167 => Ok(ElementType::NotSummedCountSumTree),
            170 => Ok(ElementType::NotSummedProvableCountSumTree),
            177 => Ok(ElementType::NotSummedProvableSumTree),
            _ => Err(ElementError::CorruptedData(format!(
                "Unknown element type discriminant: {}",
                value
            ))),
        }
    }
}

impl std::fmt::Display for ElementType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_element_type_from_discriminant() {
        assert_eq!(ElementType::try_from(0).unwrap(), ElementType::Item);
        assert_eq!(ElementType::try_from(1).unwrap(), ElementType::Reference);
        assert_eq!(ElementType::try_from(2).unwrap(), ElementType::Tree);
        assert_eq!(ElementType::try_from(3).unwrap(), ElementType::SumItem);
        assert_eq!(ElementType::try_from(4).unwrap(), ElementType::SumTree);
        assert_eq!(ElementType::try_from(5).unwrap(), ElementType::BigSumTree);
        assert_eq!(ElementType::try_from(6).unwrap(), ElementType::CountTree);
        assert_eq!(ElementType::try_from(7).unwrap(), ElementType::CountSumTree);
        assert_eq!(
            ElementType::try_from(8).unwrap(),
            ElementType::ProvableCountTree
        );
        assert_eq!(
            ElementType::try_from(9).unwrap(),
            ElementType::ItemWithSumItem
        );
        assert_eq!(
            ElementType::try_from(10).unwrap(),
            ElementType::ProvableCountSumTree
        );
        assert_eq!(
            ElementType::try_from(11).unwrap(),
            ElementType::CommitmentTree
        );
        assert_eq!(ElementType::try_from(12).unwrap(), ElementType::MmrTree);
        assert_eq!(
            ElementType::try_from(13).unwrap(),
            ElementType::BulkAppendTree
        );
        assert_eq!(
            ElementType::try_from(14).unwrap(),
            ElementType::DenseAppendOnlyFixedSizeTree
        );
        // 15 is the raw NonCounted wrapper byte and is rejected by TryFrom;
        // it has no direct ElementType variant (use from_serialized_value).
        assert!(ElementType::try_from(15).is_err());
        // 16 is the raw NotSummed wrapper byte.
        assert!(ElementType::try_from(16).is_err());
        // 17 is ProvableSumTree (Phase 1 addition).
        assert_eq!(
            ElementType::try_from(17).unwrap(),
            ElementType::ProvableSumTree
        );

        // NonCounted twins (0x80 | base): 128..142, plus 145 (= 0x80 | 17)
        assert_eq!(
            ElementType::try_from(128).unwrap(),
            ElementType::NonCountedItem
        );
        assert_eq!(
            ElementType::try_from(129).unwrap(),
            ElementType::NonCountedReference
        );
        assert_eq!(
            ElementType::try_from(142).unwrap(),
            ElementType::NonCountedDenseAppendOnlyFixedSizeTree
        );
        assert_eq!(
            ElementType::try_from(145).unwrap(),
            ElementType::NonCountedProvableSumTree
        );
        // Bytes between the base and NonCounted-twin ranges are invalid.
        assert!(ElementType::try_from(127).is_err());
        // Byte 143 is between the contiguous NonCounted block (128..=142)
        // and the new NonCountedProvableSumTree at 145, so it remains
        // invalid.
        assert!(ElementType::try_from(143).is_err());
        assert!(ElementType::try_from(144).is_err());
        // Bytes between NonCounted-twin and NotSummed-twin ranges are invalid.
        assert!(ElementType::try_from(146).is_err());
        assert!(ElementType::try_from(163).is_err());

        // NotSummed twins (0xa0 | base): only the five sum-tree bases
        // {4, 5, 7, 10, 17} are legal → discriminants {164, 165, 167, 170, 177}.
        assert_eq!(
            ElementType::try_from(164).unwrap(),
            ElementType::NotSummedSumTree
        );
        assert_eq!(
            ElementType::try_from(165).unwrap(),
            ElementType::NotSummedBigSumTree
        );
        assert_eq!(
            ElementType::try_from(167).unwrap(),
            ElementType::NotSummedCountSumTree
        );
        assert_eq!(
            ElementType::try_from(170).unwrap(),
            ElementType::NotSummedProvableCountSumTree
        );
        assert_eq!(
            ElementType::try_from(177).unwrap(),
            ElementType::NotSummedProvableSumTree
        );
        // Other bytes in 0xa0..=0xbf (non-sum-tree bases) are invalid.
        for bad in [
            0xa0u8, // base 0 (Item) — not a sum-tree variant
            0xa1,   // base 1 (Reference)
            0xa2,   // base 2 (Tree)
            0xa3,   // base 3 (SumItem) — leaf, not a tree
            0xa6,   // base 6 (CountTree)
            0xa8,   // base 8 (ProvableCountTree)
            0xa9,   // base 9 (ItemWithSumItem)
            0xab,   // base 11 (CommitmentTree)
            0xac,   // base 12 (MmrTree)
            0xad,   // base 13 (BulkAppendTree)
            0xae,   // base 14 (DenseAppendOnlyFixedSizeTree)
            0xaf,   // unallocated base 15 — coincides with the on-disk
            // NonCounted wrapper byte; not a sum-tree variant
            0xb0, // unallocated base 16 — coincides with the on-disk
            // NotSummed wrapper byte; not a sum-tree variant
            0xb2, // base 18 — unallocated
            0xbf, // base 31 — unallocated, top of the NotSummed range
        ] {
            assert!(
                ElementType::try_from(bad).is_err(),
                "{:#x} should be rejected",
                bad
            );
        }
        // Bytes past the NotSummed range are invalid.
        assert!(ElementType::try_from(0xc0).is_err());
        assert!(ElementType::try_from(255).is_err());
    }

    #[test]
    fn test_non_counted_helpers() {
        // is_non_counted: upper-three-bit compare against 0x80
        // (range 0x80..=0x9F).
        assert!(!ElementType::Item.is_non_counted());
        assert!(!ElementType::Tree.is_non_counted());
        assert!(!ElementType::ProvableSumTree.is_non_counted());
        assert!(ElementType::NonCountedItem.is_non_counted());
        assert!(ElementType::NonCountedTree.is_non_counted());
        assert!(ElementType::NonCountedDenseAppendOnlyFixedSizeTree.is_non_counted());
        // The new ProvableSumTree NonCounted twin lives at 145 (0x91), in
        // the upper half of the 0x80..=0x9F window. The widened mask
        // (& 0xe0 == 0x80) must still classify it correctly.
        assert!(ElementType::NonCountedProvableSumTree.is_non_counted());

        // The two wrapper twin ranges share bit 7. NonCounted occupies
        // 0x80..=0x9F, NotSummed occupies 0xB0..=0xBF, so NotSummed must
        // NOT be counted as NonCounted.
        assert!(!ElementType::NotSummedSumTree.is_non_counted());
        assert!(!ElementType::NotSummedProvableCountSumTree.is_non_counted());

        // base() strips the wrapper and returns the underlying type.
        assert_eq!(ElementType::Item.base(), ElementType::Item);
        assert_eq!(ElementType::NonCountedItem.base(), ElementType::Item);
        assert_eq!(ElementType::NonCountedTree.base(), ElementType::Tree);
        assert_eq!(
            ElementType::NonCountedProvableCountTree.base(),
            ElementType::ProvableCountTree
        );
        assert_eq!(
            ElementType::NonCountedProvableSumTree.base(),
            ElementType::ProvableSumTree
        );

        // The discriminant relationship: twin = base | 0x80
        assert_eq!(
            ElementType::NonCountedItem as u8,
            ElementType::Item as u8 | NON_COUNTED_FLAG
        );
        assert_eq!(
            ElementType::NonCountedDenseAppendOnlyFixedSizeTree as u8,
            ElementType::DenseAppendOnlyFixedSizeTree as u8 | NON_COUNTED_FLAG
        );
        assert_eq!(
            ElementType::NonCountedProvableSumTree as u8,
            ElementType::ProvableSumTree as u8 | NON_COUNTED_FLAG
        );
    }

    #[test]
    fn test_not_summed_helpers() {
        // is_not_summed: upper-three-bit compare against 0xa0
        // (range 0xA0..=0xBF).
        assert!(!ElementType::Item.is_not_summed());
        assert!(!ElementType::SumTree.is_not_summed());
        assert!(!ElementType::NonCountedSumTree.is_not_summed());
        assert!(ElementType::NotSummedSumTree.is_not_summed());
        assert!(ElementType::NotSummedBigSumTree.is_not_summed());
        assert!(ElementType::NotSummedCountSumTree.is_not_summed());
        assert!(ElementType::NotSummedProvableCountSumTree.is_not_summed());
        // The new ProvableSumTree NotSummed twin lives at 177 (0xB1),
        // in the upper half of the 0xA0..=0xBF window. The widened mask
        // (& 0xe0 == 0xa0) must still classify it correctly.
        assert!(ElementType::NotSummedProvableSumTree.is_not_summed());

        // NonCounted twins (0x80..=0x9F) must NOT match.
        assert!(!ElementType::NonCountedProvableSumTree.is_not_summed());

        // base() strips the wrapper and returns the underlying type.
        assert_eq!(ElementType::NotSummedSumTree.base(), ElementType::SumTree);
        assert_eq!(
            ElementType::NotSummedBigSumTree.base(),
            ElementType::BigSumTree
        );
        assert_eq!(
            ElementType::NotSummedCountSumTree.base(),
            ElementType::CountSumTree
        );
        assert_eq!(
            ElementType::NotSummedProvableCountSumTree.base(),
            ElementType::ProvableCountSumTree
        );
        assert_eq!(
            ElementType::NotSummedProvableSumTree.base(),
            ElementType::ProvableSumTree
        );

        // The discriminant relationship: twin = base | 0xa0.
        assert_eq!(
            ElementType::NotSummedSumTree as u8,
            ElementType::SumTree as u8 | NOT_SUMMED_TWIN_PREFIX
        );
        assert_eq!(
            ElementType::NotSummedProvableCountSumTree as u8,
            ElementType::ProvableCountSumTree as u8 | NOT_SUMMED_TWIN_PREFIX
        );
        assert_eq!(
            ElementType::NotSummedProvableSumTree as u8,
            ElementType::ProvableSumTree as u8 | NOT_SUMMED_TWIN_PREFIX
        );
    }

    #[test]
    fn test_simple_vs_combined_hash() {
        // Items have simple hash
        assert!(ElementType::Item.has_simple_value_hash());
        assert!(ElementType::SumItem.has_simple_value_hash());
        assert!(ElementType::ItemWithSumItem.has_simple_value_hash());

        // Trees and references have combined hash
        assert!(ElementType::Reference.has_combined_value_hash());
        assert!(ElementType::Tree.has_combined_value_hash());
        assert!(ElementType::SumTree.has_combined_value_hash());
        assert!(ElementType::BigSumTree.has_combined_value_hash());
        assert!(ElementType::CountTree.has_combined_value_hash());
        assert!(ElementType::CountSumTree.has_combined_value_hash());
        assert!(ElementType::ProvableCountTree.has_combined_value_hash());
        assert!(ElementType::ProvableSumTree.has_combined_value_hash());

        // The wrapper is transparent: NonCountedItem still hashes simply.
        assert!(ElementType::NonCountedItem.has_simple_value_hash());
        assert!(ElementType::NonCountedSumItem.has_simple_value_hash());
        assert!(ElementType::NonCountedTree.has_combined_value_hash());
        assert!(ElementType::NonCountedReference.has_combined_value_hash());
    }

    #[test]
    fn test_proof_node_type_regular_tree() {
        use super::ProofNodeType;

        // In regular trees (or None parent), items should use Kv
        assert_eq!(ElementType::Item.proof_node_type(None), ProofNodeType::Kv);
        assert_eq!(
            ElementType::SumItem.proof_node_type(Some(ElementType::Tree)),
            ProofNodeType::Kv
        );
        assert_eq!(
            ElementType::ItemWithSumItem.proof_node_type(Some(ElementType::SumTree)),
            ProofNodeType::Kv
        );

        // References should use KvRefValueHash (verifier trusts hash, GroveDB
        // post-processes)
        assert_eq!(
            ElementType::Reference.proof_node_type(None),
            ProofNodeType::KvRefValueHash
        );

        // Trees should use KvValueHash (verifier trusts hash)
        assert_eq!(
            ElementType::Tree.proof_node_type(None),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::SumTree.proof_node_type(Some(ElementType::Tree)),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::BigSumTree.proof_node_type(None),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::CountTree.proof_node_type(None),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::CountSumTree.proof_node_type(None),
            ProofNodeType::KvValueHash
        );
        assert_eq!(
            ElementType::ProvableCountTree.proof_node_type(None),
            ProofNodeType::KvValueHash
        );
    }

    #[test]
    fn test_proof_node_type_provable_count_tree() {
        use super::ProofNodeType;

        let pct = Some(ElementType::ProvableCountTree);

        // In ProvableCountTree, items should use KvCount (count in hash)
        assert_eq!(
            ElementType::Item.proof_node_type(pct),
            ProofNodeType::KvCount
        );
        assert_eq!(
            ElementType::SumItem.proof_node_type(pct),
            ProofNodeType::KvCount
        );
        assert_eq!(
            ElementType::ItemWithSumItem.proof_node_type(pct),
            ProofNodeType::KvCount
        );

        // References use KvRefValueHashCount (combined hash + count)
        // GroveDB post-processes these with dereferenced values
        assert_eq!(
            ElementType::Reference.proof_node_type(pct),
            ProofNodeType::KvRefValueHashCount
        );

        // Subtrees use KvValueHashFeatureType (combined hash + count)
        assert_eq!(
            ElementType::Tree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::SumTree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::BigSumTree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::CountTree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::CountSumTree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::ProvableCountTree.proof_node_type(pct),
            ProofNodeType::KvValueHashFeatureType
        );
    }

    #[test]
    fn test_proof_node_type_provable_count_sum_tree() {
        use super::ProofNodeType;

        let pcst = Some(ElementType::ProvableCountSumTree);

        // Items use KvCount in ProvableCountSumTree (same as ProvableCountTree)
        assert_eq!(
            ElementType::Item.proof_node_type(pcst),
            ProofNodeType::KvCount
        );

        // References use KvRefValueHashCount
        assert_eq!(
            ElementType::Reference.proof_node_type(pcst),
            ProofNodeType::KvRefValueHashCount
        );

        // Subtrees use KvValueHashFeatureType
        assert_eq!(
            ElementType::Tree.proof_node_type(pcst),
            ProofNodeType::KvValueHashFeatureType
        );
    }

    #[test]
    fn test_proof_node_type_through_non_counted_wrapper() {
        use super::ProofNodeType;

        // Wrapping doesn't change proof shape — both self and parent fall
        // back to base() before dispatch.
        assert_eq!(
            ElementType::NonCountedItem.proof_node_type(None),
            ProofNodeType::Kv,
        );
        assert_eq!(
            ElementType::NonCountedItem
                .proof_node_type(Some(ElementType::NonCountedProvableCountTree)),
            ProofNodeType::KvCount,
        );
        assert_eq!(
            ElementType::NonCountedReference
                .proof_node_type(Some(ElementType::ProvableCountSumTree)),
            ProofNodeType::KvRefValueHashCount,
        );
        assert_eq!(
            ElementType::NonCountedTree.proof_node_type(Some(ElementType::ProvableCountTree)),
            ProofNodeType::KvValueHashFeatureType,
        );
    }

    #[test]
    fn test_from_serialized_value() {
        // Test with valid first bytes
        assert_eq!(
            ElementType::from_serialized_value(&[0, 1, 2, 3]).unwrap(),
            ElementType::Item
        );
        assert_eq!(
            ElementType::from_serialized_value(&[2, 0, 0]).unwrap(),
            ElementType::Tree
        );

        // Test with empty value
        assert!(ElementType::from_serialized_value(&[]).is_err());

        // Test with unknown discriminant
        assert!(ElementType::from_serialized_value(&[255]).is_err());

        // NonCounted wrapper: the leading byte is the wrapper discriminant
        // (15) and the next byte is the inner type's discriminant. The
        // returned type is the synthetic NonCountedXxx with bit 7 set.
        assert_eq!(
            ElementType::from_serialized_value(&[15, 0, 1, 2, 3]).unwrap(),
            ElementType::NonCountedItem
        );
        assert_eq!(
            ElementType::from_serialized_value(&[15, 6]).unwrap(),
            ElementType::NonCountedCountTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[15, 14]).unwrap(),
            ElementType::NonCountedDenseAppendOnlyFixedSizeTree
        );
        // Truncated wrapper (no inner byte) is rejected.
        assert!(ElementType::from_serialized_value(&[15]).is_err());
        // Nested NonCounted is rejected.
        assert!(ElementType::from_serialized_value(&[15, 15]).is_err());
        // Wrapper with unknown inner discriminant is rejected.
        assert!(ElementType::from_serialized_value(&[15, 200]).is_err());
        // Wrapper whose inner byte is itself a synthetic twin discriminant
        // (high bit set) is rejected — only base discriminants 0..=14 are
        // legal on-disk inner bytes. Without this guard, `0x80 | 128 == 128`
        // would silently parse as `NonCountedItem`.
        assert!(ElementType::from_serialized_value(&[15, 128]).is_err());
        assert!(ElementType::from_serialized_value(&[15, 142]).is_err());
        // Wrapper with an unallocated mid-range inner byte (16, 18..=127)
        // is also rejected, even though it has no high bit set.
        assert!(ElementType::from_serialized_value(&[15, 16]).is_err());
        assert!(ElementType::from_serialized_value(&[15, 18]).is_err());
        assert!(ElementType::from_serialized_value(&[15, 100]).is_err());
        // Byte 17 IS a valid base discriminant (ProvableSumTree), so
        // `[15, 17, ...]` resolves to NonCountedProvableSumTree.
        assert_eq!(
            ElementType::from_serialized_value(&[15, 17]).unwrap(),
            ElementType::NonCountedProvableSumTree
        );
    }

    #[test]
    fn test_is_tree() {
        // Base types
        assert!(!ElementType::Item.is_tree());
        assert!(!ElementType::Reference.is_tree());
        assert!(ElementType::Tree.is_tree());
        assert!(!ElementType::SumItem.is_tree());
        assert!(ElementType::SumTree.is_tree());
        assert!(ElementType::BigSumTree.is_tree());
        assert!(ElementType::CountTree.is_tree());
        assert!(ElementType::CountSumTree.is_tree());
        assert!(ElementType::ProvableCountTree.is_tree());
        assert!(!ElementType::ItemWithSumItem.is_tree());
        assert!(ElementType::CommitmentTree.is_tree());
        assert!(ElementType::MmrTree.is_tree());
        assert!(ElementType::BulkAppendTree.is_tree());
        assert!(ElementType::DenseAppendOnlyFixedSizeTree.is_tree());
        assert!(ElementType::ProvableSumTree.is_tree());
        assert!(ElementType::NonCountedProvableSumTree.is_tree());

        // The wrapper is transparent: NonCountedTree is a tree, NonCountedItem is not.
        assert!(!ElementType::NonCountedItem.is_tree());
        assert!(!ElementType::NonCountedReference.is_tree());
        assert!(ElementType::NonCountedTree.is_tree());
        assert!(ElementType::NonCountedSumTree.is_tree());
        assert!(ElementType::NonCountedProvableCountTree.is_tree());
        assert!(ElementType::NonCountedDenseAppendOnlyFixedSizeTree.is_tree());

        // is_item / is_reference also see through the wrapper.
        assert!(ElementType::NonCountedItem.is_item());
        assert!(ElementType::NonCountedSumItem.is_item());
        assert!(ElementType::NonCountedReference.is_reference());
    }

    /// Verifies that serialized Element discriminants match ElementType
    /// constants.
    ///
    /// This test ensures that the ElementType enum values stay in sync with
    /// the actual bincode serialization of Element variants. If the Element
    /// enum order changes, this test will catch the drift.
    #[test]
    fn test_element_serialization_discriminants_match_element_type() {
        use grovedb_version::version::GroveVersion;

        use crate::{element::Element, reference_path::ReferencePathType};

        let grove_version = GroveVersion::latest();

        // Build vector of (Element, ElementType, variant_name) for all 10 variants
        let test_cases: Vec<(Element, ElementType, &str)> = vec![
            // discriminant 0
            (
                Element::Item(vec![1, 2, 3], None),
                ElementType::Item,
                "Item",
            ),
            // discriminant 1
            (
                Element::Reference(
                    ReferencePathType::AbsolutePathReference(vec![vec![1]]),
                    None,
                    None,
                ),
                ElementType::Reference,
                "Reference",
            ),
            // discriminant 2
            (Element::Tree(None, None), ElementType::Tree, "Tree"),
            // discriminant 3
            (Element::SumItem(42, None), ElementType::SumItem, "SumItem"),
            // discriminant 4
            (
                Element::SumTree(None, 0, None),
                ElementType::SumTree,
                "SumTree",
            ),
            // discriminant 5
            (
                Element::BigSumTree(None, 0, None),
                ElementType::BigSumTree,
                "BigSumTree",
            ),
            // discriminant 6
            (
                Element::CountTree(None, 0, None),
                ElementType::CountTree,
                "CountTree",
            ),
            // discriminant 7
            (
                Element::CountSumTree(None, 0, 0, None),
                ElementType::CountSumTree,
                "CountSumTree",
            ),
            // discriminant 8
            (
                Element::ProvableCountTree(None, 0, None),
                ElementType::ProvableCountTree,
                "ProvableCountTree",
            ),
            // discriminant 9
            (
                Element::ItemWithSumItem(vec![1, 2, 3], 42, None),
                ElementType::ItemWithSumItem,
                "ItemWithSumItem",
            ),
            // discriminant 10
            (
                Element::ProvableCountSumTree(None, 0, 0, None),
                ElementType::ProvableCountSumTree,
                "ProvableCountSumTree",
            ),
            // discriminant 11
            (
                Element::CommitmentTree(0, 10, None),
                ElementType::CommitmentTree,
                "CommitmentTree",
            ),
            // discriminant 12
            (Element::MmrTree(0, None), ElementType::MmrTree, "MmrTree"),
            // discriminant 13
            (
                Element::BulkAppendTree(0, 2, None),
                ElementType::BulkAppendTree,
                "BulkAppendTree",
            ),
            // discriminant 14
            (
                Element::DenseAppendOnlyFixedSizeTree(0, 1, None),
                ElementType::DenseAppendOnlyFixedSizeTree,
                "DenseAppendOnlyFixedSizeTree",
            ),
            // discriminant 17 (15 = NonCounted wrapper, 16 = NotSummed wrapper)
            (
                Element::ProvableSumTree(None, 0, None),
                ElementType::ProvableSumTree,
                "ProvableSumTree",
            ),
        ];

        // Verify we're testing all 16 base discriminants (0-14 plus 17;
        // 15 and 16 are reserved wrapper bytes with no direct ElementType
        // variant).
        assert_eq!(
            test_cases.len(),
            16,
            "Expected 16 base Element variants in test, got {}",
            test_cases.len()
        );

        for (element, expected_type, variant_name) in test_cases {
            let serialized = element
                .serialize(grove_version)
                .unwrap_or_else(|e| panic!("Failed to serialize {}: {:?}", variant_name, e));

            // Verify serialized buffer is non-empty
            assert!(
                !serialized.is_empty(),
                "Serialized {} should not be empty",
                variant_name
            );

            // Verify first byte matches ElementType discriminant
            let first_byte = serialized[0];
            let expected_discriminant = expected_type as u8;

            assert_eq!(
                first_byte, expected_discriminant,
                "Element::{} serialized with discriminant {}, but ElementType::{} = {}. The \
                 Element enum order may have changed!",
                variant_name, first_byte, variant_name, expected_discriminant
            );

            // Also verify round-trip through ElementType::from_serialized_value
            let parsed_type = ElementType::from_serialized_value(&serialized).unwrap_or_else(|e| {
                panic!(
                    "Failed to parse ElementType from serialized {}: {:?}",
                    variant_name, e
                )
            });

            assert_eq!(
                parsed_type, expected_type,
                "ElementType::from_serialized_value for {} returned {:?}, expected {:?}",
                variant_name, parsed_type, expected_type
            );
        }
    }

    /// Pins the bincode discriminant for `Element::NonCounted` to
    /// `NON_COUNTED_WRAPPER_DISCRIMINANT`. If someone reorders the `Element`
    /// enum and pushes `NonCounted` to a different position, this catches it
    /// — `from_serialized_value` reads byte 1 specifically when byte 0 is
    /// this constant.
    #[test]
    fn test_non_counted_wrapper_discriminant_pinned() {
        use grovedb_version::version::GroveVersion;

        use crate::element::Element;

        let grove_version = GroveVersion::latest();

        // Pick one inner element per category to verify the wrapper byte +
        // inner byte resolve to the right NonCountedXxx.
        let cases: Vec<(Element, ElementType, u8, &str)> = vec![
            (
                Element::NonCounted(Box::new(Element::Item(vec![1, 2, 3], None))),
                ElementType::NonCountedItem,
                0,
                "NonCounted(Item)",
            ),
            (
                Element::NonCounted(Box::new(Element::SumItem(7, None))),
                ElementType::NonCountedSumItem,
                3,
                "NonCounted(SumItem)",
            ),
            (
                Element::NonCounted(Box::new(Element::CountTree(None, 5, None))),
                ElementType::NonCountedCountTree,
                6,
                "NonCounted(CountTree)",
            ),
            (
                Element::NonCounted(Box::new(Element::ProvableCountTree(None, 5, None))),
                ElementType::NonCountedProvableCountTree,
                8,
                "NonCounted(ProvableCountTree)",
            ),
        ];

        for (element, expected_type, expected_inner_disc, name) in cases {
            let serialized = element
                .serialize(grove_version)
                .unwrap_or_else(|e| panic!("Failed to serialize {}: {:?}", name, e));

            assert!(
                serialized.len() >= 2,
                "Serialized {} should have at least 2 bytes",
                name
            );
            assert_eq!(
                serialized[0], NON_COUNTED_WRAPPER_DISCRIMINANT,
                "{}: first byte should be the wrapper discriminant (15)",
                name
            );
            assert_eq!(
                serialized[1], expected_inner_disc,
                "{}: second byte should match the inner element's discriminant",
                name
            );

            let parsed = ElementType::from_serialized_value(&serialized)
                .unwrap_or_else(|e| panic!("Failed to parse {}: {:?}", name, e));
            assert_eq!(
                parsed, expected_type,
                "{}: from_serialized_value returned {:?}, expected {:?}",
                name, parsed, expected_type
            );
            // And the synthetic discriminant follows the 0x80|base rule.
            assert_eq!(
                parsed as u8,
                expected_inner_disc | NON_COUNTED_FLAG,
                "{}: NonCountedXxx = inner_disc | 0x80",
                name
            );
        }
    }

    /// Pins the bincode discriminant for `Element::NotSummed` to
    /// `NOT_SUMMED_WRAPPER_DISCRIMINANT` and the five allowed inner
    /// discriminants. Mirrors `test_non_counted_wrapper_discriminant_pinned`.
    #[test]
    fn test_not_summed_wrapper_discriminant_pinned() {
        use grovedb_version::version::GroveVersion;

        use crate::element::Element;

        let grove_version = GroveVersion::latest();

        let cases: Vec<(Element, ElementType, u8, &str)> = vec![
            (
                Element::NotSummed(Box::new(Element::SumTree(None, 0, None))),
                ElementType::NotSummedSumTree,
                4,
                "NotSummed(SumTree)",
            ),
            (
                Element::NotSummed(Box::new(Element::BigSumTree(None, 0, None))),
                ElementType::NotSummedBigSumTree,
                5,
                "NotSummed(BigSumTree)",
            ),
            (
                Element::NotSummed(Box::new(Element::CountSumTree(None, 0, 0, None))),
                ElementType::NotSummedCountSumTree,
                7,
                "NotSummed(CountSumTree)",
            ),
            (
                Element::NotSummed(Box::new(Element::ProvableCountSumTree(None, 0, 0, None))),
                ElementType::NotSummedProvableCountSumTree,
                10,
                "NotSummed(ProvableCountSumTree)",
            ),
            (
                Element::NotSummed(Box::new(Element::ProvableSumTree(None, 0, None))),
                ElementType::NotSummedProvableSumTree,
                17,
                "NotSummed(ProvableSumTree)",
            ),
        ];

        for (element, expected_type, expected_inner_disc, name) in cases {
            let serialized = element
                .serialize(grove_version)
                .unwrap_or_else(|e| panic!("Failed to serialize {}: {:?}", name, e));

            assert!(
                serialized.len() >= 2,
                "Serialized {} should have at least 2 bytes",
                name
            );
            assert_eq!(
                serialized[0], NOT_SUMMED_WRAPPER_DISCRIMINANT,
                "{}: first byte should be the wrapper discriminant (16)",
                name
            );
            assert_eq!(
                serialized[1], expected_inner_disc,
                "{}: second byte should match the inner element's discriminant",
                name
            );

            let parsed = ElementType::from_serialized_value(&serialized)
                .unwrap_or_else(|e| panic!("Failed to parse {}: {:?}", name, e));
            assert_eq!(
                parsed, expected_type,
                "{}: from_serialized_value returned {:?}, expected {:?}",
                name, parsed, expected_type
            );
            // The synthetic discriminant follows the 0xa0|base rule.
            assert_eq!(
                parsed as u8,
                expected_inner_disc | NOT_SUMMED_TWIN_PREFIX,
                "{}: NotSummedXxx = inner_disc | 0xa0",
                name
            );
        }
    }

    /// Round-trip the new `ProvableSumTree` discriminant. The base byte is
    /// 17, and the NonCounted twin lives at 145 (= 0x80 | 17). The
    /// `NonCounted(ProvableSumTree)` shape is allowed; the
    /// `NotSummed(ProvableSumTree)` shape is allowed at the Element level
    /// but does NOT produce a twin ElementType (see the TODO in the
    /// ElementType doc-comment).
    #[test]
    fn test_provable_sum_tree_discriminant_round_trip() {
        use grovedb_version::version::GroveVersion;

        use crate::element::Element;

        let grove_version = GroveVersion::latest();

        // Base form serializes with leading byte 17.
        let element = Element::ProvableSumTree(None, 0, None);
        let serialized = element
            .serialize(grove_version)
            .expect("serialize ProvableSumTree");
        assert_eq!(serialized[0], 17);
        assert_eq!(
            ElementType::from_serialized_value(&serialized).unwrap(),
            ElementType::ProvableSumTree
        );

        // NonCounted(ProvableSumTree) serializes with the wrapper byte 15
        // followed by the inner discriminant 17, and resolves to the new
        // NonCountedProvableSumTree synthetic twin (= 145).
        let nc = Element::NonCounted(Box::new(Element::ProvableSumTree(None, 0, None)));
        let nc_serialized = nc.serialize(grove_version).expect("serialize NC PST");
        assert_eq!(nc_serialized[0], NON_COUNTED_WRAPPER_DISCRIMINANT);
        assert_eq!(nc_serialized[1], 17);
        assert_eq!(
            ElementType::from_serialized_value(&nc_serialized).unwrap(),
            ElementType::NonCountedProvableSumTree
        );
        assert_eq!(ElementType::NonCountedProvableSumTree as u8, 145);
    }

    /// Validate the new resolver paths around byte 16 (NotSummed wrapper).
    #[test]
    fn test_from_serialized_value_not_summed_paths() {
        // Truncated wrapper (no inner byte) is rejected.
        assert!(ElementType::from_serialized_value(&[16]).is_err());

        // Each of the five legal inner discriminants resolves to the right
        // synthetic twin.
        assert_eq!(
            ElementType::from_serialized_value(&[16, 4]).unwrap(),
            ElementType::NotSummedSumTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[16, 5]).unwrap(),
            ElementType::NotSummedBigSumTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[16, 7]).unwrap(),
            ElementType::NotSummedCountSumTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[16, 10]).unwrap(),
            ElementType::NotSummedProvableCountSumTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[16, 17]).unwrap(),
            ElementType::NotSummedProvableSumTree
        );

        // All other inner bytes are rejected: non-sum-tree base types,
        // wrapper bytes, synthetic NonCounted twins (128..145), synthetic
        // NotSummed twins (164..177), and unallocated ranges.
        for bad in [
            0u8, 1, 2, 3, 6, 8, 9, 11, 12, 13, 14, 15, 16, 18, 100, 128, 142, 145, 164, 170, 177,
            200, 255,
        ] {
            assert!(
                ElementType::from_serialized_value(&[16, bad]).is_err(),
                "[16, {}] should be rejected",
                bad
            );
        }
    }
}
