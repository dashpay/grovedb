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

/// Twin-discriminant family marker for `NotSummedXxx` types. Every twin
/// lives in the range `0xB0..=0xBF` and is matched by an upper-nibble
/// compare: `disc & 0xf0 == 0xb0`. This keeps the NotSummed family
/// disjoint from NonCounted (`0x80..=0xAF`) and NotCountedOrSummed
/// (`0xC0..=0xCF`).
///
/// **No bitwise OR formula is used to compute the twin discriminant from
/// the base.** Each twin is assigned a specific value out of the 16 slots
/// in the family range, and resolution in both directions
/// (`from_serialized_value`'s `inner_byte → twin` and `base()`'s
/// `twin → base`) is done by an explicit per-variant match. This avoids
/// the constraint that the previous "`prefix | base`" formula imposed
/// (base discriminants had to fit in the low nibble), so a new
/// sum-tree base at e.g. discriminant 19 can have an arbitrary twin slot
/// like `0xB1 = 177` without colliding with the formula's collapsed
/// `0xb0 | 19 → 0xb3 → base 3 (SumItem)` interpretation.
pub const NOT_SUMMED_TWIN_PREFIX: u8 = 0xb0;

/// Bincode discriminant byte for `Element::NotCountedOrSummed`. Tied to the
/// declaration order of the `Element` enum (0-indexed, 17th variant).
/// Matches develop's positioning (this branch preserves that ordering and
/// places its own `ProvableSumTree` addition at slot 19 instead).
///
/// Like the other two wrapper discriminants, this byte has no direct
/// `ElementType` variant. `from_serialized_value` reads the next byte and
/// resolves to one of the synthetic `NotCountedOrSummedXxx` twins. Only
/// the sum-bearing tree base discriminants are legal as the inner byte.
pub const NOT_COUNTED_OR_SUMMED_WRAPPER_DISCRIMINANT: u8 = 17;

/// Twin-discriminant prefix for `NotCountedOrSummedXxx` types: every twin is
/// encoded as `NOT_COUNTED_OR_SUMMED_TWIN_PREFIX | base`. The prefix has the
/// high bit set (so all wrappers cluster in `0x80..` range) plus bit 6, which
/// distinguishes it from `NON_COUNTED_FLAG`'s `0x80` upper-nibble and
/// `NOT_SUMMED_TWIN_PREFIX`'s `0xb0` upper-nibble. Detection is therefore
/// an upper-nibble compare: `disc & 0xf0 == 0xc0`.
pub const NOT_COUNTED_OR_SUMMED_TWIN_PREFIX: u8 = 0xc0;
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
    /// node hash via `node_hash_with_sum`.
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

    /// Use `Node::KVCountSum` - combined analogue of `KvCount` and `KvSum`.
    /// The verifier recomputes `value_hash = H(value)` and includes BOTH
    /// the u64 count AND the i64 sum in the node hash via
    /// `node_hash_with_count_and_sum`.
    ///
    /// Used for: Item, SumItem, ItemWithSumItem (inside
    /// ProvableCountProvableSumTree)
    KvCountSum,

    /// Use `Node::KVRefValueHashCountSum` - combined analogue of
    /// `KvRefValueHashCount` and `KvRefValueHashSum`. At the merk layer,
    /// this generates `KVValueHashFeatureType` (since merk doesn't know
    /// about references). GroveDB post-processes these nodes to
    /// `Node::KVRefValueHashCountSum` with the dereferenced value.
    ///
    /// Used for: Reference (inside ProvableCountProvableSumTree)
    KvRefValueHashCountSum,
}

/// Element type discriminants.
///
/// Base types (0..=14, 18..=20) match the bincode serialization order of
/// the `Element` enum. The `Element` enum has indices 15, 16, and 17
/// reserved for the `NonCounted`, `NotSummed`, and `NotCountedOrSummed`
/// wrapper variants respectively (none has a direct `ElementType`
/// variant — each synthesizes twin discriminants by reading the inner
/// element's byte). Slot 18 is `ReferenceWithSumItem` and slot 19 is
/// `ProvableSumTree`.
///
/// Non-counted twins are synthetic — they encode "this is a `NonCounted`
/// wrapper around an inner element of base type `disc & ...`". Twins for
/// base discriminants 0..=14 live in 128..=142 (`0x80 | base`). The twin
/// for `ReferenceWithSumItem` (base 18) is placed at 146 (`0x80 | 18 =
/// 0x92`), and the twin for `ProvableSumTree` (base 19) is placed at 147
/// (`0x80 | 19 = 0x93`). All NonCounted twins satisfy the range check
/// `0x80 <= disc < 0xB0` — the lower range distinguishes them from the
/// NotSummed family that starts at `0xB0`. The on-disk representation of
/// `Element::NonCounted` still uses the wrapper byte
/// `NON_COUNTED_WRAPPER_DISCRIMINANT` (15) followed by the inner
/// element's bytes; `from_serialized_value` synthesizes the
/// `NonCountedXxx` variant by peeking at the second byte.
///
/// Not-summed twins use the prefix `0xb0` over the five sum-bearing tree
/// bases (4, 5, 7, 10, 19), placing them at `180, 181, 183, 186, 177`.
/// The wrapper byte is `NOT_SUMMED_WRAPPER_DISCRIMINANT` (16).
/// Not-counted-or-summed twins use the prefix `0xc0` over the same five
/// sum-bearing tree bases, placing them at `196, 197, 199, 202, 193`.
/// The wrapper byte is `NOT_COUNTED_OR_SUMMED_WRAPPER_DISCRIMINANT`
/// (17).
///
/// All three wrapper twin ranges have bit 7 set, so wrappers cluster in
/// `0x80..`, and the upper nibble distinguishes them: `0x80..=0xAF` is
/// `NonCounted`, `0xB0..=0xBF` is `NotSummed`, `0xC0..=0xCF` is
/// `NotCountedOrSummed`. The three wrappers are mutually exclusive —
/// the constructors and (de)serializers reject any nesting in either
/// direction.
///
/// Not-summed twins cluster in the `0xB0..=0xBF` family range (matched
/// via `disc & 0xf0 == 0xb0`). Unlike NonCounted, twin slots are
/// **assigned explicitly per variant** rather than computed via a
/// `prefix | base` formula. The five legal sum-bearing tree inner types
/// are mapped 1-to-1:
///   SumTree (base 4)              -> 180
///   BigSumTree (base 5)           -> 181
///   CountSumTree (base 7)         -> 183
///   ProvableCountSumTree (base 10)-> 186
///   ProvableSumTree (base 19)     -> 177 (hand-assigned; base overflows
///                                          the low nibble of the formula)
/// The wrapper byte on disk is `NOT_SUMMED_WRAPPER_DISCRIMINANT` (16),
/// and `from_serialized_value` resolves `[16, inner_byte]` to the matching
/// twin via an explicit `inner_byte → twin` match.
///
/// IMPORTANT: Base values (0..=14, 18..=20) must match the order of
/// variants in the `Element` enum. The
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
    // 15 is reserved as the NonCounted on-disk wrapper byte, 16 is
    // reserved as the NotSummed on-disk wrapper byte, and 17 is
    // reserved as the NotCountedOrSummed on-disk wrapper byte. None of
    // these have a direct ElementType variant.
    /// Reference that also carries an explicit `SumValue` - discriminant 18.
    /// Resolves like `Reference` on `get()` and propagates `sum_value` into
    /// sum-bearing parents like `SumItem` / `ItemWithSumItem`. See
    /// `Element::ReferenceWithSumItem` for full semantics.
    ReferenceWithSumItem = 18,
    /// Provable sum tree - discriminant 19 (sums baked into node hashes).
    ProvableSumTree = 19,
    /// Provable count + provable sum tree - discriminant 20.
    /// BOTH count and sum baked into node hashes. Mirrors
    /// `ProvableCountTree` and `ProvableSumTree` simultaneously so
    /// `AggregateCountOnRange` AND `AggregateSumOnRange` proofs are both
    /// verifiable against the same tree.
    ProvableCountProvableSumTree = 20,
    /// Provable sum-indexed tree (primary `ProvableSumTree` + secondary
    /// sum-ordered index) - discriminant 21. Takes the slot previously
    /// held by the now-removed non-provable `CountIndexedTree`.
    ProvableSumIndexedTree = 21,
    /// Provable count-indexed tree (primary `ProvableCountTree` + secondary
    /// count-ordered index) - discriminant 22
    ProvableCountIndexedTree = 22,
    /// Provable count + provable sum indexed tree (primary
    /// `ProvableCountProvableSumTree` + 1..=3 secondaries keyed by axis
    /// sort-prefix) - discriminant 23. The new dual/triple-axis variant.
    ProvableCountProvableSumIndexedTree = 23,
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
    /// Non-counted wrapper around `ReferenceWithSumItem` - discriminant 146 (`0x80 | 18`)
    NonCountedReferenceWithSumItem = 146,
    /// Non-counted wrapper around `ProvableSumTree` - discriminant 147 (`0x80 | 19`)
    NonCountedProvableSumTree = 147,
    /// Non-counted wrapper around `ProvableCountProvableSumTree` - discriminant
    /// 148 (`0x80 | 20`).
    NonCountedProvableCountProvableSumTree = 148,
    /// Non-counted wrapper around `ProvableSumIndexedTree` - discriminant 149
    /// (`0x80 | 21`)
    NonCountedProvableSumIndexedTree = 149,
    /// Non-counted wrapper around `ProvableCountIndexedTree` - discriminant
    /// 150 (`0x80 | 22`)
    NonCountedProvableCountIndexedTree = 150,
    /// Non-counted wrapper around `ProvableCountProvableSumIndexedTree` -
    /// discriminant 151 (`0x80 | 23`)
    NonCountedProvableCountProvableSumIndexedTree = 151,
    /// Not-summed wrapper around `SumTree` - discriminant 180 (`0xB4`)
    NotSummedSumTree = 180,
    /// Not-summed wrapper around `BigSumTree` - discriminant 181 (`0xB5`)
    NotSummedBigSumTree = 181,
    /// Not-summed wrapper around `CountSumTree` - discriminant 183 (`0xB7`)
    NotSummedCountSumTree = 183,
    /// Not-summed wrapper around `ProvableCountSumTree` - discriminant 186 (`0xBA`)
    NotSummedProvableCountSumTree = 186,
    /// Not-summed wrapper around `ProvableSumTree` - discriminant 177 (`0xB1`),
    /// assigned explicitly out of the `0xB0..=0xBF` family range. Not derived
    /// from any formula — see the doc comment on `NOT_SUMMED_TWIN_PREFIX`.
    NotSummedProvableSumTree = 177,
    /// Not-summed wrapper around `ProvableCountProvableSumTree` - discriminant
    /// 178 (`0xB2`), assigned explicitly out of the `0xB0..=0xBF` family range.
    /// Hand-assigned because base 20 overflows the low nibble and `0xB4`
    /// would collide with `NotSummedSumTree`.
    NotSummedProvableCountProvableSumTree = 178,
    /// Not-counted-or-summed wrapper around `SumTree` - discriminant 196 (`0xc0 | 4`)
    NotCountedOrSummedSumTree = 196,
    /// Not-counted-or-summed wrapper around `BigSumTree` - discriminant 197 (`0xc0 | 5`)
    NotCountedOrSummedBigSumTree = 197,
    /// Not-counted-or-summed wrapper around `CountSumTree` - discriminant 199 (`0xc0 | 7`)
    NotCountedOrSummedCountSumTree = 199,
    /// Not-counted-or-summed wrapper around `ProvableCountSumTree` - discriminant 202 (`0xc0 | 10`)
    NotCountedOrSummedProvableCountSumTree = 202,
    /// Not-counted-or-summed wrapper around `ProvableSumTree` - discriminant
    /// 193 (`0xC1`), assigned explicitly out of the `0xC0..=0xCF` family
    /// range. Like `NotSummedProvableSumTree` it can't use the
    /// `prefix | base` formula because base 19 overflows the low nibble.
    NotCountedOrSummedProvableSumTree = 193,
    /// Not-counted-or-summed wrapper around `ProvableCountProvableSumTree`
    /// - discriminant 194 (`0xC2`), hand-assigned out of the `0xC0..=0xCF`
    ///   family range. Like `NotSummedProvableCountProvableSumTree` (0xB2)
    ///   it can't use the `prefix | base` formula because base 20 overflows
    ///   the low nibble.
    NotCountedOrSummedProvableCountProvableSumTree = 194,
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
            // The inner discriminant must be a legal base type. Today
            // those are `0..=14`, `18` (ReferenceWithSumItem), `19`
            // (ProvableSumTree), `20` (ProvableCountProvableSumTree),
            // `21` (ProvableSumIndexedTree), `22` (ProvableCountIndexedTree),
            // and `23` (ProvableCountProvableSumIndexedTree).
            // Bytes 15, 16, and 17 are the wrapper bytes themselves
            // (nested wrappers forbidden in either direction); 24..=127
            // are unallocated; 128..=151 are the synthetic NonCountedXxx
            // twins which never appear on disk.
            // Without this check, the bitwise OR below would collapse
            // `0x80 | inner_byte` into `inner_byte` and a payload like
            // `[15, 128, ...]` would silently parse as `NonCountedItem`.
            //
            // Use an explicit allowlist so the next base-variant addition
            // is a one-line edit here and the check stays robust to new
            // variants landing without updating the guard.
            if !matches!(inner_byte, 0..=14 | 18 | 19 | 20 | 21 | 22 | 23) {
                return Err(ElementError::CorruptedData(format!(
                    "NonCounted inner discriminant must be a base type \
                     (0..=14, 18, 19, 20, 21, 22, or 23), got {}",
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
            // Only the five sum-bearing tree base discriminants are legal
            // here. Each is mapped explicitly to its assigned NotSummed
            // twin — no `prefix | inner_byte` formula is used, because
            // some twin slots are hand-assigned (see NOT_SUMMED_TWIN_PREFIX
            // doc). Anything else is rejected so that round-tripping
            // `from_serialized_value` always yields a valid `NotSummedXxx`.
            match inner_byte {
                4 => Ok(ElementType::NotSummedSumTree),
                5 => Ok(ElementType::NotSummedBigSumTree),
                7 => Ok(ElementType::NotSummedCountSumTree),
                10 => Ok(ElementType::NotSummedProvableCountSumTree),
                19 => Ok(ElementType::NotSummedProvableSumTree),
                20 => Ok(ElementType::NotSummedProvableCountProvableSumTree),
                _ => Err(ElementError::CorruptedData(format!(
                    "NotSummed inner discriminant must be a sum-bearing tree base type \
                     (4=SumTree, 5=BigSumTree, 7=CountSumTree, 10=ProvableCountSumTree, \
                     19=ProvableSumTree, 20=ProvableCountProvableSumTree), got {}",
                    inner_byte
                ))),
            }
        } else if first_byte == NOT_COUNTED_OR_SUMMED_WRAPPER_DISCRIMINANT {
            let inner_byte = *serialized_value.get(1).ok_or_else(|| {
                ElementError::CorruptedData(
                    "NotCountedOrSummed wrapper has no inner element discriminant byte".to_string(),
                )
            })?;
            // Same whitelist as NotSummed: only the five sum-bearing tree
            // base discriminants are legal here. The new wrapper additionally
            // suppresses the parent count contribution (the implicit +1 for
            // SumTree/BigSumTree/ProvableSumTree, or the explicit count for
            // CountSumTree / ProvableCountSumTree). `ProvableSumTree` (base
            // 19) is mapped explicitly because its twin slot 0xC1 doesn't
            // fall out of the bitwise `prefix | base` formula.
            match inner_byte {
                4 => Ok(ElementType::NotCountedOrSummedSumTree),
                5 => Ok(ElementType::NotCountedOrSummedBigSumTree),
                7 => Ok(ElementType::NotCountedOrSummedCountSumTree),
                10 => Ok(ElementType::NotCountedOrSummedProvableCountSumTree),
                19 => Ok(ElementType::NotCountedOrSummedProvableSumTree),
                20 => Ok(ElementType::NotCountedOrSummedProvableCountProvableSumTree),
                _ => Err(ElementError::CorruptedData(format!(
                    "NotCountedOrSummed inner discriminant must be a sum-bearing tree base \
                     type (4=SumTree, 5=BigSumTree, 7=CountSumTree, \
                     10=ProvableCountSumTree, 19=ProvableSumTree, \
                     20=ProvableCountProvableSumTree), got {}",
                    inner_byte
                ))),
            }
        } else {
            Self::try_from(first_byte)
        }
    }

    /// Returns true if this is a `NonCountedXxx` discriminant. Range check:
    /// `NonCountedXxx` lives in `0x80..=0xaf`, `NotSummedXxx` in
    /// `0xb0..=0xbf`, `NotCountedOrSummedXxx` in `0xc0..=0xcf`. All three
    /// ranges have bit 7 set but only NonCounted's upper nibble stays
    /// below `0xb`. The widened upper bound (was `0x9F` in earlier
    /// revisions) makes room for synthetic twins of higher-numbered base
    /// discriminants like `NonCountedProvableSumTree = 0x93` (twin of
    /// base 19) and `NonCountedReferenceWithSumItem = 0x92` (twin of
    /// base 18).
    #[inline]
    pub const fn is_non_counted(self) -> bool {
        let disc = self as u8;
        disc >= NON_COUNTED_FLAG && disc < NOT_SUMMED_TWIN_PREFIX
    }

    /// Returns true if this is a `NotSummedXxx` discriminant. The
    /// NotSummed family spans `0xB0..=0xBF` (16 explicit slots, no
    /// formula); matched via `disc & 0xf0 == 0xb0`. NonCounted lives at
    /// `0x80..=0xAF` so the two families stay disjoint.
    #[inline]
    pub const fn is_not_summed(self) -> bool {
        (self as u8) & 0xf0 == NOT_SUMMED_TWIN_PREFIX
    }

    /// Returns true if this is a `NotCountedOrSummedXxx` discriminant.
    #[inline]
    pub const fn is_not_counted_or_summed(self) -> bool {
        (self as u8) & 0xf0 == NOT_COUNTED_OR_SUMMED_TWIN_PREFIX
    }

    /// Returns the underlying base ElementType, stripping any wrapper flag
    /// bits. For base types, returns `self` unchanged.
    ///
    /// The three wrapper families occupy disjoint ranges: `NonCounted` at
    /// `0x80..=0x9F` (`& 0xe0 == 0x80`), `NotSummed` at `0xB0..=0xBF`
    /// (`& 0xf0 == 0xb0`), and `NotCountedOrSummed` at `0xC0..=0xCF`
    /// (`& 0xf0 == 0xc0`). Constructors and (de)serializers reject any
    /// wrapper nesting, so only one wrapper status is ever set on any
    /// valid `ElementType` instance.
    ///
    /// `NonCounted` uses the bitwise formula `base | 0x80` (all base
    /// discriminants fit in the low 5 bits), so its inverse mask works
    /// uniformly. `NotSummed` and `NotCountedOrSummed` use **explicit
    /// per-variant mapping** because their twin slots are hand-assigned
    /// rather than computed — `NotSummedProvableSumTree = 0xB1` and
    /// `NotCountedOrSummedProvableSumTree = 0xC1` would collide with the
    /// `disc & 0x0F → base 1 (Reference)` interpretation if a bitwise
    /// inverse were used.
    #[inline]
    pub fn base(self) -> ElementType {
        if self.is_non_counted() {
            // Safe: every NonCountedXxx is constructed from a valid base
            // discriminant whose low 5 bits fit cleanly under 0x80.
            ElementType::try_from((self as u8) & NON_COUNTED_BASE_MASK)
                .expect("NonCounted twin always has a valid base")
        } else {
            match self {
                ElementType::NotSummedSumTree => ElementType::SumTree,
                ElementType::NotSummedBigSumTree => ElementType::BigSumTree,
                ElementType::NotSummedCountSumTree => ElementType::CountSumTree,
                ElementType::NotSummedProvableCountSumTree => ElementType::ProvableCountSumTree,
                ElementType::NotSummedProvableSumTree => ElementType::ProvableSumTree,
                ElementType::NotSummedProvableCountProvableSumTree => {
                    ElementType::ProvableCountProvableSumTree
                }
                ElementType::NotCountedOrSummedSumTree => ElementType::SumTree,
                ElementType::NotCountedOrSummedBigSumTree => ElementType::BigSumTree,
                ElementType::NotCountedOrSummedCountSumTree => ElementType::CountSumTree,
                ElementType::NotCountedOrSummedProvableCountSumTree => {
                    ElementType::ProvableCountSumTree
                }
                ElementType::NotCountedOrSummedProvableSumTree => ElementType::ProvableSumTree,
                ElementType::NotCountedOrSummedProvableCountProvableSumTree => {
                    ElementType::ProvableCountProvableSumTree
                }
                other => other,
            }
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
        // aggregate into the node hash. The dispatch distinguishes three
        // mutually-exclusive families:
        //   - count-only: ProvableCountTree, ProvableCountSumTree
        //     (only the count is hashed; sum is tracked but unauthenticated)
        //   - sum-only: ProvableSumTree
        //   - count-AND-sum: ProvableCountProvableSumTree
        //
        // Item / Reference proof variants diverge across families
        // (`KvCount`/`KvSum`/`KvCountSum` and `KvRefValueHash*` analogues).
        // Subtrees inside any of the three families still use
        // `KvValueHashFeatureType` — the embedded `TreeFeatureType` carries
        // the per-node aggregate(s) so a single proof-node variant suffices
        // for the subtree case in every family.
        // Indexed-tree primaries dispatch with their own node family:
        // PCIT primaries use count-only nodes, PSIT primaries use
        // sum-only nodes, and PCPSIT primaries use count-and-sum nodes
        // (mirroring `TreeType::inner_node_type`).
        let is_provable_count_only_tree = matches!(
            parent_base,
            Some(ElementType::ProvableCountTree)
                | Some(ElementType::ProvableCountSumTree)
                | Some(ElementType::ProvableCountIndexedTree)
        );
        let is_provable_sum_only_tree = matches!(
            parent_base,
            Some(ElementType::ProvableSumTree) | Some(ElementType::ProvableSumIndexedTree)
        );
        let is_provable_count_and_provable_sum_tree = matches!(
            parent_base,
            Some(ElementType::ProvableCountProvableSumTree)
                | Some(ElementType::ProvableCountProvableSumIndexedTree)
        );
        let is_provable_aggregate_tree = is_provable_count_only_tree
            || is_provable_sum_only_tree
            || is_provable_count_and_provable_sum_tree;

        let base = self.base();
        if base.has_simple_value_hash() {
            // Items (Item, SumItem, ItemWithSumItem)
            if is_provable_count_and_provable_sum_tree {
                ProofNodeType::KvCountSum
            } else if is_provable_count_only_tree {
                ProofNodeType::KvCount
            } else if is_provable_sum_only_tree {
                ProofNodeType::KvSum
            } else {
                ProofNodeType::Kv
            }
        } else if base.is_reference() {
            // References need combined hash (for reference resolution).
            // Inside provable-aggregate parents they additionally need the
            // hashed aggregate(s) in node_hash. GroveDB post-processes
            // these to KVRefValueHash / KVRefValueHashCount /
            // KVRefValueHashSum / KVRefValueHashCountSum.
            if is_provable_count_and_provable_sum_tree {
                ProofNodeType::KvRefValueHashCountSum
            } else if is_provable_count_only_tree {
                ProofNodeType::KvRefValueHashCount
            } else if is_provable_sum_only_tree {
                ProofNodeType::KvRefValueHashSum
            } else {
                ProofNodeType::KvRefValueHash
            }
        } else {
            // Subtrees (Tree, SumTree, BigSumTree, CountTree, CountSumTree,
            // ProvableCountTree, ProvableSumTree, ProvableCountProvableSumTree).
            // KvValueHashFeatureType works for every family because the
            // embedded `TreeFeatureType` carries the aggregate(s).
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
                | ElementType::ProvableCountProvableSumTree
                | ElementType::CommitmentTree
                | ElementType::MmrTree
                | ElementType::BulkAppendTree
                | ElementType::DenseAppendOnlyFixedSizeTree
                | ElementType::ProvableSumIndexedTree
                | ElementType::ProvableCountIndexedTree
                | ElementType::ProvableCountProvableSumIndexedTree
        )
    }

    /// Returns true if this element type is a count-indexed tree (the
    /// single-axis provable count case). Kept for callers that
    /// specifically need the count-only case; new code that needs to
    /// detect any indexed-tree primary should prefer
    /// [`is_indexed_tree`]. Looks through the `NonCounted` wrapper.
    #[inline]
    pub fn is_count_indexed_tree(&self) -> bool {
        matches!(self.base(), ElementType::ProvableCountIndexedTree)
    }

    /// Returns true if this element type is any of the three indexed-tree
    /// variants (`ProvableSumIndexedTree`, `ProvableCountIndexedTree`,
    /// `ProvableCountProvableSumIndexedTree`). Looks through the
    /// `NonCounted` wrapper. This is the broad predicate that callers
    /// touching "indexed-tree primaries" generically should use.
    #[inline]
    pub fn is_indexed_tree(&self) -> bool {
        matches!(
            self.base(),
            ElementType::ProvableSumIndexedTree
                | ElementType::ProvableCountIndexedTree
                | ElementType::ProvableCountProvableSumIndexedTree
        )
    }

    /// Returns true if this element type is a reference. Looks through the
    /// `NonCounted` wrapper. Both `Reference` and `ReferenceWithSumItem` are
    /// references — they share the combined-value-hash proof shape and are
    /// resolved by the same `follow_reference` chain.
    #[inline]
    pub fn is_reference(&self) -> bool {
        matches!(
            self.base(),
            ElementType::Reference | ElementType::ReferenceWithSumItem
        )
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
            ElementType::ReferenceWithSumItem => "reference with sum item",
            ElementType::ProvableSumTree => "provable sum tree",
            ElementType::ProvableCountProvableSumTree => "provable count provable sum tree",
            ElementType::ProvableSumIndexedTree => "provable sum indexed tree",
            ElementType::ProvableCountIndexedTree => "provable count indexed tree",
            ElementType::ProvableCountProvableSumIndexedTree => {
                "provable count provable sum indexed tree"
            }
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
            ElementType::NonCountedReferenceWithSumItem => "non_counted reference with sum item",
            ElementType::NonCountedProvableSumTree => "non_counted provable sum tree",
            ElementType::NonCountedProvableCountProvableSumTree => {
                "non_counted provable count provable sum tree"
            }
            ElementType::NonCountedProvableSumIndexedTree => {
                "non_counted provable sum indexed tree"
            }
            ElementType::NonCountedProvableCountIndexedTree => {
                "non_counted provable count indexed tree"
            }
            ElementType::NonCountedProvableCountProvableSumIndexedTree => {
                "non_counted provable count provable sum indexed tree"
            }
            ElementType::NotSummedSumTree => "not_summed sum tree",
            ElementType::NotSummedBigSumTree => "not_summed big sum tree",
            ElementType::NotSummedCountSumTree => "not_summed count sum tree",
            ElementType::NotSummedProvableCountSumTree => "not_summed provable count sum tree",
            ElementType::NotSummedProvableSumTree => "not_summed provable sum tree",
            ElementType::NotSummedProvableCountProvableSumTree => {
                "not_summed provable count provable sum tree"
            }
            ElementType::NotCountedOrSummedSumTree => "not_counted_or_summed sum tree",
            ElementType::NotCountedOrSummedBigSumTree => "not_counted_or_summed big sum tree",
            ElementType::NotCountedOrSummedCountSumTree => "not_counted_or_summed count sum tree",
            ElementType::NotCountedOrSummedProvableCountSumTree => {
                "not_counted_or_summed provable count sum tree"
            }
            ElementType::NotCountedOrSummedProvableSumTree => {
                "not_counted_or_summed provable sum tree"
            }
            ElementType::NotCountedOrSummedProvableCountProvableSumTree => {
                "not_counted_or_summed provable count provable sum tree"
            }
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
            // 16 is the raw NotSummed wrapper byte; same treatment.
            // 17 is the raw NotCountedOrSummed wrapper byte; same treatment.
            18 => Ok(ElementType::ReferenceWithSumItem),
            19 => Ok(ElementType::ProvableSumTree),
            20 => Ok(ElementType::ProvableCountProvableSumTree),
            21 => Ok(ElementType::ProvableSumIndexedTree),
            22 => Ok(ElementType::ProvableCountIndexedTree),
            23 => Ok(ElementType::ProvableCountProvableSumIndexedTree),
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
            146 => Ok(ElementType::NonCountedReferenceWithSumItem),
            147 => Ok(ElementType::NonCountedProvableSumTree),
            148 => Ok(ElementType::NonCountedProvableCountProvableSumTree),
            149 => Ok(ElementType::NonCountedProvableSumIndexedTree),
            150 => Ok(ElementType::NonCountedProvableCountIndexedTree),
            151 => Ok(ElementType::NonCountedProvableCountProvableSumIndexedTree),
            // NotSummed twins occupy the 0xB0..=0xBF family range; slots
            // are assigned explicitly per variant.
            177 => Ok(ElementType::NotSummedProvableSumTree),
            178 => Ok(ElementType::NotSummedProvableCountProvableSumTree),
            180 => Ok(ElementType::NotSummedSumTree),
            181 => Ok(ElementType::NotSummedBigSumTree),
            183 => Ok(ElementType::NotSummedCountSumTree),
            186 => Ok(ElementType::NotSummedProvableCountSumTree),
            // NotCountedOrSummed twins occupy the 0xC0..=0xCF family range.
            // Bases 4/5/7/10 use the bitwise `0xC0 | base` formula; bases 19
            // (ProvableSumTree) and 20 (ProvableCountProvableSumTree) are
            // hand-assigned to 0xC1 (193) and 0xC2 (194) because they
            // overflow the low nibble and would otherwise collide under a
            // uniform mask.
            193 => Ok(ElementType::NotCountedOrSummedProvableSumTree),
            194 => Ok(ElementType::NotCountedOrSummedProvableCountProvableSumTree),
            196 => Ok(ElementType::NotCountedOrSummedSumTree),
            197 => Ok(ElementType::NotCountedOrSummedBigSumTree),
            199 => Ok(ElementType::NotCountedOrSummedCountSumTree),
            202 => Ok(ElementType::NotCountedOrSummedProvableCountSumTree),
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
        // 15 (NonCounted), 16 (NotSummed), and 17 (NotCountedOrSummed)
        // are raw wrapper bytes that are rejected by TryFrom; they have
        // no direct ElementType variant (use from_serialized_value).
        assert!(ElementType::try_from(15).is_err());
        // 16 is the raw NotSummed wrapper byte; same treatment.
        assert!(ElementType::try_from(16).is_err());
        // 17 is the raw NotCountedOrSummed wrapper byte; same treatment.
        assert!(ElementType::try_from(17).is_err());

        // Base discriminants 18..=23: ReferenceWithSumItem, ProvableSumTree,
        // ProvableCountProvableSumTree, ProvableSumIndexedTree,
        // ProvableCountIndexedTree, ProvableCountProvableSumIndexedTree.
        assert_eq!(
            ElementType::try_from(18).unwrap(),
            ElementType::ReferenceWithSumItem
        );
        assert_eq!(
            ElementType::try_from(19).unwrap(),
            ElementType::ProvableSumTree
        );
        assert_eq!(
            ElementType::try_from(20).unwrap(),
            ElementType::ProvableCountProvableSumTree
        );
        assert_eq!(
            ElementType::try_from(21).unwrap(),
            ElementType::ProvableSumIndexedTree
        );
        assert_eq!(
            ElementType::try_from(22).unwrap(),
            ElementType::ProvableCountIndexedTree
        );
        assert_eq!(
            ElementType::try_from(23).unwrap(),
            ElementType::ProvableCountProvableSumIndexedTree
        );
        // 24..=127 are unallocated and invalid.
        assert!(ElementType::try_from(24).is_err());
        assert!(ElementType::try_from(100).is_err());

        // NonCounted twins (0x80 | base): 128..142, plus 146 (= 0x80|18 =
        // ReferenceWithSumItem twin), 147 (= 0x80|19 = ProvableSumTree
        // twin), and 148 (= 0x80|20 = ProvableCountProvableSumTree twin).
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
            ElementType::try_from(146).unwrap(),
            ElementType::NonCountedReferenceWithSumItem
        );
        assert_eq!(
            ElementType::try_from(147).unwrap(),
            ElementType::NonCountedProvableSumTree
        );
        assert_eq!(
            ElementType::try_from(148).unwrap(),
            ElementType::NonCountedProvableCountProvableSumTree
        );
        assert_eq!(
            ElementType::try_from(149).unwrap(),
            ElementType::NonCountedProvableSumIndexedTree
        );
        assert_eq!(
            ElementType::try_from(150).unwrap(),
            ElementType::NonCountedProvableCountIndexedTree
        );
        assert_eq!(
            ElementType::try_from(151).unwrap(),
            ElementType::NonCountedProvableCountProvableSumIndexedTree
        );
        // Bytes between the base and NonCounted-twin ranges are invalid.
        assert!(ElementType::try_from(127).is_err());
        // 143 (= 0x80|15), 144 (= 0x80|16), 145 (= 0x80|17): wrapper bytes
        // have no base, so these would synthesize a wrapper-on-wrapper twin.
        assert!(ElementType::try_from(143).is_err());
        assert!(ElementType::try_from(144).is_err());
        assert!(ElementType::try_from(145).is_err());
        // 152..=176 (between NonCounted-twin and NotSummed-twin ranges) are
        // invalid (with the exception of 177 = NotSummedProvableSumTree and
        // 178 = NotSummedProvableCountProvableSumTree).
        assert!(ElementType::try_from(152).is_err());
        assert!(ElementType::try_from(176).is_err());

        // NotSummed twins live in 0xB0..=0xBF with explicit per-variant
        // slot assignments — not a formula. Six slots are populated:
        //   SumTree                       -> 180 (0xB4)
        //   BigSumTree                    -> 181 (0xB5)
        //   CountSumTree                  -> 183 (0xB7)
        //   ProvableCountSumTree          -> 186 (0xBA)
        //   ProvableSumTree               -> 177 (0xB1)
        //   ProvableCountProvableSumTree  -> 178 (0xB2)
        assert_eq!(
            ElementType::try_from(180).unwrap(),
            ElementType::NotSummedSumTree
        );
        assert_eq!(
            ElementType::try_from(181).unwrap(),
            ElementType::NotSummedBigSumTree
        );
        assert_eq!(
            ElementType::try_from(183).unwrap(),
            ElementType::NotSummedCountSumTree
        );
        assert_eq!(
            ElementType::try_from(186).unwrap(),
            ElementType::NotSummedProvableCountSumTree
        );
        assert_eq!(
            ElementType::try_from(177).unwrap(),
            ElementType::NotSummedProvableSumTree
        );
        assert_eq!(
            ElementType::try_from(178).unwrap(),
            ElementType::NotSummedProvableCountProvableSumTree
        );
        // All unallocated slots in 0xB0..=0xBF are invalid.
        for bad in [
            0xb0u8, // wrapper byte 16, never a twin
            0xb3, 0xb6, 0xb8, 0xb9, 0xbb, 0xbc, 0xbd, 0xbe, 0xbf,
        ] {
            assert!(
                ElementType::try_from(bad).is_err(),
                "{:#x} should be rejected",
                bad
            );
        }
        // Bytes between NotSummed and NotCountedOrSummed twin ranges are invalid.
        assert!(ElementType::try_from(187).is_err());
        assert!(ElementType::try_from(195).is_err());

        // NotCountedOrSummed twins: the six sum-bearing tree bases
        // {4, 5, 7, 10, 19, 20} are legal → discriminants {196, 197, 199,
        // 202, 193, 194}. `0xc0 | base` only works for bases 4/5/7/10;
        // ProvableSumTree (base 19) gets an explicit hand-assigned slot
        // at 0xC1 (193), and ProvableCountProvableSumTree (base 20) at
        // 0xC2 (194), same shape as the corresponding NotSummed twins.
        assert_eq!(
            ElementType::try_from(196).unwrap(),
            ElementType::NotCountedOrSummedSumTree
        );
        assert_eq!(
            ElementType::try_from(197).unwrap(),
            ElementType::NotCountedOrSummedBigSumTree
        );
        assert_eq!(
            ElementType::try_from(199).unwrap(),
            ElementType::NotCountedOrSummedCountSumTree
        );
        assert_eq!(
            ElementType::try_from(202).unwrap(),
            ElementType::NotCountedOrSummedProvableCountSumTree
        );
        assert_eq!(
            ElementType::try_from(193).unwrap(),
            ElementType::NotCountedOrSummedProvableSumTree
        );
        assert_eq!(
            ElementType::try_from(194).unwrap(),
            ElementType::NotCountedOrSummedProvableCountProvableSumTree
        );
        // Other bytes in 0xc0..=0xcf (non-sum-bearing-tree bases) are invalid.
        for bad in [0xc0u8, 0xc3, 0xc6, 0xc8, 0xc9, 0xcb, 0xcc, 0xcd, 0xce, 0xcf] {
            assert!(
                ElementType::try_from(bad).is_err(),
                "{:#x} should be rejected",
                bad
            );
        }
        // Bytes past the highest NotCountedOrSummed twin are invalid.
        assert!(ElementType::try_from(203).is_err());
        assert!(ElementType::try_from(255).is_err());
    }

    #[test]
    fn test_non_counted_helpers() {
        // is_non_counted: range check against 0x80..=0xAF.
        assert!(!ElementType::Item.is_non_counted());
        assert!(!ElementType::Tree.is_non_counted());
        assert!(!ElementType::ProvableSumTree.is_non_counted());
        assert!(!ElementType::ReferenceWithSumItem.is_non_counted());
        assert!(ElementType::NonCountedItem.is_non_counted());
        assert!(ElementType::NonCountedTree.is_non_counted());
        assert!(ElementType::NonCountedDenseAppendOnlyFixedSizeTree.is_non_counted());
        // The NonCounted twin of ProvableSumTree (base 19) lives at 147
        // (0x93). The widened range check (0x80..=0xAF) must still
        // classify it correctly.
        assert!(ElementType::NonCountedProvableSumTree.is_non_counted());
        // Same for the NonCounted twin of ReferenceWithSumItem (base 18)
        // at 146 (0x92).
        assert!(ElementType::NonCountedReferenceWithSumItem.is_non_counted());

        // All three wrapper twin ranges share bit 7, but only NonCounted has
        // upper-nibble 0x80. NotSummed (upper-nibble 0xb0) and
        // NotCountedOrSummed (upper-nibble 0xc0) must NOT be counted as
        // NonCounted.
        assert!(!ElementType::NotSummedSumTree.is_non_counted());
        assert!(!ElementType::NotSummedProvableCountSumTree.is_non_counted());
        assert!(!ElementType::NotSummedProvableSumTree.is_non_counted());
        assert!(!ElementType::NotCountedOrSummedSumTree.is_non_counted());
        assert!(!ElementType::NotCountedOrSummedProvableCountSumTree.is_non_counted());
        assert!(!ElementType::NotCountedOrSummedProvableSumTree.is_non_counted());

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
        // is_not_summed: upper-nibble compare against 0xb0
        // (range 0xB0..=0xBF).
        assert!(!ElementType::Item.is_not_summed());
        assert!(!ElementType::SumTree.is_not_summed());
        assert!(!ElementType::NonCountedSumTree.is_not_summed());
        assert!(!ElementType::NotCountedOrSummedSumTree.is_not_summed());
        assert!(ElementType::NotSummedSumTree.is_not_summed());
        assert!(ElementType::NotSummedBigSumTree.is_not_summed());
        assert!(ElementType::NotSummedCountSumTree.is_not_summed());
        assert!(ElementType::NotSummedProvableCountSumTree.is_not_summed());
        // The new ProvableSumTree NotSummed twin lives at 177 (0xB1)
        // — an explicit slot in the 0xB0..=0xBF window. The mask
        // (& 0xf0 == 0xb0) classifies it correctly.
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

        // Twin slots are explicit, not formula-derived. Pin each one.
        // The first four are historical (formula `base | 0xb0` happens to
        // match), the last is a hand-assigned slot.
        assert_eq!(ElementType::NotSummedSumTree as u8, 180);
        assert_eq!(ElementType::NotSummedBigSumTree as u8, 181);
        assert_eq!(ElementType::NotSummedCountSumTree as u8, 183);
        assert_eq!(ElementType::NotSummedProvableCountSumTree as u8, 186);
        assert_eq!(ElementType::NotSummedProvableSumTree as u8, 177);

        // The whole family fits inside the 0xB0..=0xBF window.
        for t in [
            ElementType::NotSummedSumTree,
            ElementType::NotSummedBigSumTree,
            ElementType::NotSummedCountSumTree,
            ElementType::NotSummedProvableCountSumTree,
            ElementType::NotSummedProvableSumTree,
        ] {
            let d = t as u8;
            assert!(
                d & 0xf0 == NOT_SUMMED_TWIN_PREFIX,
                "{:?} = {:#x} outside NotSummed family",
                t,
                d
            );
        }
    }

    #[test]
    fn test_not_counted_or_summed_helpers() {
        // is_not_counted_or_summed: upper-nibble compare against 0xc0.
        assert!(!ElementType::Item.is_not_counted_or_summed());
        assert!(!ElementType::SumTree.is_not_counted_or_summed());
        assert!(!ElementType::NonCountedSumTree.is_not_counted_or_summed());
        assert!(!ElementType::NotSummedSumTree.is_not_counted_or_summed());
        assert!(ElementType::NotCountedOrSummedSumTree.is_not_counted_or_summed());
        assert!(ElementType::NotCountedOrSummedBigSumTree.is_not_counted_or_summed());
        assert!(ElementType::NotCountedOrSummedCountSumTree.is_not_counted_or_summed());
        assert!(ElementType::NotCountedOrSummedProvableCountSumTree.is_not_counted_or_summed());

        // The three wrapper twin ranges have distinct upper nibbles, so the
        // three predicates are mutually exclusive on any single discriminant.
        assert!(!ElementType::NotCountedOrSummedSumTree.is_non_counted());
        assert!(!ElementType::NotCountedOrSummedSumTree.is_not_summed());

        // base() strips the wrapper and returns the underlying type.
        assert_eq!(
            ElementType::NotCountedOrSummedSumTree.base(),
            ElementType::SumTree
        );
        assert_eq!(
            ElementType::NotCountedOrSummedBigSumTree.base(),
            ElementType::BigSumTree
        );
        assert_eq!(
            ElementType::NotCountedOrSummedCountSumTree.base(),
            ElementType::CountSumTree
        );
        assert_eq!(
            ElementType::NotCountedOrSummedProvableCountSumTree.base(),
            ElementType::ProvableCountSumTree
        );

        // The discriminant relationship: twin = base | 0xc0.
        assert_eq!(
            ElementType::NotCountedOrSummedSumTree as u8,
            ElementType::SumTree as u8 | NOT_COUNTED_OR_SUMMED_TWIN_PREFIX
        );
        assert_eq!(
            ElementType::NotCountedOrSummedProvableCountSumTree as u8,
            ElementType::ProvableCountSumTree as u8 | NOT_COUNTED_OR_SUMMED_TWIN_PREFIX
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
        assert!(ElementType::ReferenceWithSumItem.has_combined_value_hash());
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
        assert!(ElementType::NonCountedReferenceWithSumItem.has_combined_value_hash());
    }

    #[test]
    fn test_as_str_for_provable_sum_tree_variants() {
        // Cover the as_str / Display path for the ProvableSumTree variant and
        // its synthetic NonCountedProvableSumTree / NotSummed twins. The
        // Display impl delegates to `as_str`, so we go through it to make the
        // test resilient.
        assert_eq!(ElementType::ProvableSumTree.as_str(), "provable sum tree");
        assert_eq!(
            ElementType::NonCountedProvableSumTree.as_str(),
            "non_counted provable sum tree"
        );
        assert_eq!(
            ElementType::NotSummedProvableSumTree.as_str(),
            "not_summed provable sum tree"
        );
        // Display delegation.
        assert_eq!(
            format!("{}", ElementType::NonCountedProvableSumTree),
            "non_counted provable sum tree"
        );
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
        // ReferenceWithSumItem shares the reference proof shape.
        assert_eq!(
            ElementType::ReferenceWithSumItem.proof_node_type(None),
            ProofNodeType::KvRefValueHash
        );
        assert_eq!(
            ElementType::ReferenceWithSumItem.proof_node_type(Some(ElementType::SumTree)),
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
        // ReferenceWithSumItem shares the reference proof shape inside
        // ProvableCountTree parents.
        assert_eq!(
            ElementType::ReferenceWithSumItem.proof_node_type(pct),
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
    fn test_proof_node_type_provable_sum_tree() {
        // Inside a ProvableSumTree parent, items map to KvSum and references
        // map to KvRefValueHashSum. Subtrees still use KvValueHashFeatureType
        // (the embedded TreeFeatureType carries the aggregate). This
        // exercises the `is_provable_sum_tree` branches in `proof_node_type`.
        use super::ProofNodeType;

        let pst = Some(ElementType::ProvableSumTree);

        assert_eq!(ElementType::Item.proof_node_type(pst), ProofNodeType::KvSum);
        assert_eq!(
            ElementType::SumItem.proof_node_type(pst),
            ProofNodeType::KvSum
        );
        assert_eq!(
            ElementType::ItemWithSumItem.proof_node_type(pst),
            ProofNodeType::KvSum
        );

        assert_eq!(
            ElementType::Reference.proof_node_type(pst),
            ProofNodeType::KvRefValueHashSum
        );

        // Subtrees still go through KvValueHashFeatureType.
        assert_eq!(
            ElementType::Tree.proof_node_type(pst),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::SumTree.proof_node_type(pst),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::ProvableSumTree.proof_node_type(pst),
            ProofNodeType::KvValueHashFeatureType
        );

        // NotSummed-wrapped ProvableSumTree parents normalize to the same
        // base — the wrapper is transparent here.
        assert_eq!(
            ElementType::Item.proof_node_type(Some(ElementType::NotSummedProvableSumTree)),
            ProofNodeType::KvSum
        );
    }

    /// Inside a `ProvableCountProvableSumTree` parent, items map to
    /// `KvCountSum` (the new dual-axis variant) and references map to
    /// `KvRefValueHashCountSum`. Subtrees still use
    /// `KvValueHashFeatureType` — the embedded `TreeFeatureType`
    /// (`ProvableCountedAndProvableSummedMerkNode`) carries both
    /// aggregates. Wrappers normalize through `base()` so a
    /// `NonCounted` / `NotSummed` / `NotCountedOrSummed` wrap of the
    /// new variant produces the same dispatch.
    #[test]
    fn test_proof_node_type_provable_count_provable_sum_tree() {
        use super::ProofNodeType;

        let pcps = Some(ElementType::ProvableCountProvableSumTree);

        assert_eq!(
            ElementType::Item.proof_node_type(pcps),
            ProofNodeType::KvCountSum
        );
        assert_eq!(
            ElementType::SumItem.proof_node_type(pcps),
            ProofNodeType::KvCountSum
        );
        assert_eq!(
            ElementType::ItemWithSumItem.proof_node_type(pcps),
            ProofNodeType::KvCountSum
        );

        assert_eq!(
            ElementType::Reference.proof_node_type(pcps),
            ProofNodeType::KvRefValueHashCountSum
        );
        assert_eq!(
            ElementType::ReferenceWithSumItem.proof_node_type(pcps),
            ProofNodeType::KvRefValueHashCountSum
        );

        // Subtrees still go through KvValueHashFeatureType. The embedded
        // feature_type carries both axes via the
        // ProvableCountedAndProvableSummedMerkNode variant.
        assert_eq!(
            ElementType::Tree.proof_node_type(pcps),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::SumTree.proof_node_type(pcps),
            ProofNodeType::KvValueHashFeatureType
        );
        assert_eq!(
            ElementType::ProvableCountProvableSumTree.proof_node_type(pcps),
            ProofNodeType::KvValueHashFeatureType
        );

        // Wrappers around PCPS parents normalize transparently to the
        // same dual-axis dispatch.
        assert_eq!(
            ElementType::Item
                .proof_node_type(Some(ElementType::NonCountedProvableCountProvableSumTree)),
            ProofNodeType::KvCountSum
        );
        assert_eq!(
            ElementType::Item
                .proof_node_type(Some(ElementType::NotSummedProvableCountProvableSumTree)),
            ProofNodeType::KvCountSum
        );
        assert_eq!(
            ElementType::Item.proof_node_type(Some(
                ElementType::NotCountedOrSummedProvableCountProvableSumTree
            )),
            ProofNodeType::KvCountSum
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
        // Cross-nesting with NotSummed wrapper byte is rejected.
        assert!(ElementType::from_serialized_value(&[15, 16]).is_err());
        // Wrapper with unknown inner discriminant is rejected.
        assert!(ElementType::from_serialized_value(&[15, 200]).is_err());
        // Wrapper whose inner byte is itself a synthetic twin discriminant
        // (high bit set) is rejected — only base discriminants 0..=14, 18,
        // 19, 20, 21, 22, 23 are legal on-disk inner bytes. Without this
        // guard, `0x80 | 128 == 128` would silently parse as `NonCountedItem`.
        assert!(ElementType::from_serialized_value(&[15, 128]).is_err());
        assert!(ElementType::from_serialized_value(&[15, 142]).is_err());
        // Wrapper with an unallocated mid-range inner byte (16, 17,
        // 24..=127) is also rejected, even though it has no high bit
        // set.
        assert!(ElementType::from_serialized_value(&[15, 16]).is_err());
        assert!(ElementType::from_serialized_value(&[15, 17]).is_err());
        assert!(ElementType::from_serialized_value(&[15, 24]).is_err());
        assert!(ElementType::from_serialized_value(&[15, 100]).is_err());

        // Inner byte 18 (ReferenceWithSumItem) IS a legal base; resolves to
        // the synthetic NonCountedReferenceWithSumItem twin.
        assert_eq!(
            ElementType::from_serialized_value(&[15, 18]).unwrap(),
            ElementType::NonCountedReferenceWithSumItem
        );
        // Inner byte 20 (ProvableCountProvableSumTree) IS also a legal
        // base; resolves to NonCountedProvableCountProvableSumTree
        // (twin slot 148 = 0x80|20).
        assert_eq!(
            ElementType::from_serialized_value(&[15, 20]).unwrap(),
            ElementType::NonCountedProvableCountProvableSumTree
        );
        // Inner byte 19 (ProvableSumTree) IS also a legal base; resolves to
        // NonCountedProvableSumTree (twin slot 147 = 0x80|19).
        assert_eq!(
            ElementType::from_serialized_value(&[15, 19]).unwrap(),
            ElementType::NonCountedProvableSumTree
        );
        // Inner bytes 21, 22, and 23 (ProvableSumIndexedTree,
        // ProvableCountIndexedTree, ProvableCountProvableSumIndexedTree)
        // resolve to their NonCounted twins at slots 149, 150, 151.
        assert_eq!(
            ElementType::from_serialized_value(&[15, 21]).unwrap(),
            ElementType::NonCountedProvableSumIndexedTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[15, 22]).unwrap(),
            ElementType::NonCountedProvableCountIndexedTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[15, 23]).unwrap(),
            ElementType::NonCountedProvableCountProvableSumIndexedTree
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
        assert!(ElementType::ProvableSumIndexedTree.is_tree());
        assert!(ElementType::ProvableCountIndexedTree.is_tree());
        assert!(ElementType::ProvableCountProvableSumIndexedTree.is_tree());
        // ReferenceWithSumItem is a reference, not a tree and not an item.
        assert!(!ElementType::ReferenceWithSumItem.is_tree());
        assert!(ElementType::ReferenceWithSumItem.is_reference());
        assert!(!ElementType::ReferenceWithSumItem.is_item());

        // The wrapper is transparent: NonCountedTree is a tree, NonCountedItem is not.
        assert!(!ElementType::NonCountedItem.is_tree());
        assert!(!ElementType::NonCountedReference.is_tree());
        assert!(ElementType::NonCountedTree.is_tree());
        assert!(ElementType::NonCountedSumTree.is_tree());
        assert!(ElementType::NonCountedProvableCountTree.is_tree());
        assert!(ElementType::NonCountedDenseAppendOnlyFixedSizeTree.is_tree());
        assert!(ElementType::NonCountedProvableSumIndexedTree.is_tree());
        assert!(ElementType::NonCountedProvableCountIndexedTree.is_tree());
        assert!(ElementType::NonCountedProvableCountProvableSumIndexedTree.is_tree());

        // is_count_indexed_tree only matches the (provable) count-indexed
        // variant. PSIT and PCPSIT are NOT count-indexed in the
        // legacy sense, so they must fall outside this predicate.
        assert!(ElementType::ProvableCountIndexedTree.is_count_indexed_tree());
        assert!(ElementType::NonCountedProvableCountIndexedTree.is_count_indexed_tree());
        assert!(!ElementType::ProvableSumIndexedTree.is_count_indexed_tree());
        assert!(!ElementType::NonCountedProvableSumIndexedTree.is_count_indexed_tree());
        assert!(!ElementType::ProvableCountProvableSumIndexedTree.is_count_indexed_tree());
        assert!(!ElementType::NonCountedProvableCountProvableSumIndexedTree.is_count_indexed_tree());
        assert!(!ElementType::Tree.is_count_indexed_tree());
        assert!(!ElementType::ProvableCountTree.is_count_indexed_tree());

        // is_indexed_tree is the broad predicate: all three indexed-tree
        // variants (and their NonCounted twins) match.
        assert!(ElementType::ProvableSumIndexedTree.is_indexed_tree());
        assert!(ElementType::ProvableCountIndexedTree.is_indexed_tree());
        assert!(ElementType::ProvableCountProvableSumIndexedTree.is_indexed_tree());
        assert!(ElementType::NonCountedProvableSumIndexedTree.is_indexed_tree());
        assert!(ElementType::NonCountedProvableCountIndexedTree.is_indexed_tree());
        assert!(ElementType::NonCountedProvableCountProvableSumIndexedTree.is_indexed_tree());
        assert!(!ElementType::Tree.is_indexed_tree());
        assert!(!ElementType::ProvableCountTree.is_indexed_tree());

        // is_item / is_reference also see through the wrapper.
        assert!(ElementType::NonCountedItem.is_item());
        assert!(ElementType::NonCountedSumItem.is_item());
        assert!(ElementType::NonCountedReference.is_reference());
        assert!(ElementType::NonCountedReferenceWithSumItem.is_reference());
        assert!(!ElementType::NonCountedReferenceWithSumItem.is_item());
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

        // Build vector of (Element, ElementType, variant_name) for all base variants.
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
            // discriminant 18 (15, 16, 17 are wrapper bytes — no base variants)
            (
                Element::ReferenceWithSumItem(
                    ReferencePathType::AbsolutePathReference(vec![vec![1]]),
                    None,
                    42,
                    None,
                ),
                ElementType::ReferenceWithSumItem,
                "ReferenceWithSumItem",
            ),
            // discriminant 19
            (
                Element::ProvableSumTree(None, 0, None),
                ElementType::ProvableSumTree,
                "ProvableSumTree",
            ),
            // discriminant 20
            (
                Element::ProvableCountProvableSumTree(None, 0, 0, None),
                ElementType::ProvableCountProvableSumTree,
                "ProvableCountProvableSumTree",
            ),
            // discriminant 21
            (
                Element::ProvableSumIndexedTree(None, None, 0, None),
                ElementType::ProvableSumIndexedTree,
                "ProvableSumIndexedTree",
            ),
            // discriminant 22
            (
                Element::ProvableCountIndexedTree(None, None, 0, None),
                ElementType::ProvableCountIndexedTree,
                "ProvableCountIndexedTree",
            ),
            // discriminant 23
            (
                Element::ProvableCountProvableSumIndexedTree(None, 0, 0, vec![(0, None)], None),
                ElementType::ProvableCountProvableSumIndexedTree,
                "ProvableCountProvableSumIndexedTree",
            ),
        ];

        // Verify we're testing all 21 base discriminants: 0..=14, 18, 19,
        // 20, 21, 22, 23. (15 = NonCounted wrapper byte, 16 = NotSummed
        // wrapper byte, 17 = NotCountedOrSummed wrapper byte — none has
        // a base ElementType variant.)
        assert_eq!(
            test_cases.len(),
            21,
            "Expected 21 base Element variants in test, got {}",
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

        use crate::{element::Element, reference_path::ReferencePathType};

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
            (
                Element::NonCounted(Box::new(Element::ReferenceWithSumItem(
                    ReferencePathType::AbsolutePathReference(vec![vec![1]]),
                    None,
                    42,
                    None,
                ))),
                ElementType::NonCountedReferenceWithSumItem,
                18,
                "NonCounted(ReferenceWithSumItem)",
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

        // Tuple: (Element, expected twin, expected_inner_disc_on_wire,
        //         expected_twin_disc_assignment, name)
        let cases: Vec<(Element, ElementType, u8, u8, &str)> = vec![
            (
                Element::NotSummed(Box::new(Element::SumTree(None, 0, None))),
                ElementType::NotSummedSumTree,
                4,
                180,
                "NotSummed(SumTree)",
            ),
            (
                Element::NotSummed(Box::new(Element::BigSumTree(None, 0, None))),
                ElementType::NotSummedBigSumTree,
                5,
                181,
                "NotSummed(BigSumTree)",
            ),
            (
                Element::NotSummed(Box::new(Element::CountSumTree(None, 0, 0, None))),
                ElementType::NotSummedCountSumTree,
                7,
                183,
                "NotSummed(CountSumTree)",
            ),
            (
                Element::NotSummed(Box::new(Element::ProvableCountSumTree(None, 0, 0, None))),
                ElementType::NotSummedProvableCountSumTree,
                10,
                186,
                "NotSummed(ProvableCountSumTree)",
            ),
            (
                Element::NotSummed(Box::new(Element::ProvableSumTree(None, 0, None))),
                ElementType::NotSummedProvableSumTree,
                19,
                177,
                "NotSummed(ProvableSumTree)",
            ),
        ];

        for (element, expected_type, expected_inner_disc, expected_twin_disc, name) in cases {
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
            // Twin discriminants are explicit slot assignments — pin each.
            assert_eq!(
                parsed as u8, expected_twin_disc,
                "{}: NotSummedXxx slot mismatch (no formula — see \
                 NOT_SUMMED_TWIN_PREFIX doc)",
                name
            );
        }
    }

    /// Round-trip the `ProvableSumTree` discriminant. The base byte is
    /// 19 (this branch places `ProvableSumTree` at the end of the
    /// `Element` enum to preserve develop's `NotCountedOrSummed = 17`
    /// and `ReferenceWithSumItem = 18` wire encoding), and the
    /// NonCounted twin lives at 147 (= 0x80 | 19). The
    /// `NonCounted(ProvableSumTree)` shape is allowed.
    #[test]
    fn test_provable_sum_tree_discriminant_round_trip() {
        use grovedb_version::version::GroveVersion;

        use crate::element::Element;

        let grove_version = GroveVersion::latest();

        // Base form serializes with leading byte 19.
        let element = Element::ProvableSumTree(None, 0, None);
        let serialized = element
            .serialize(grove_version)
            .expect("serialize ProvableSumTree");
        assert_eq!(serialized[0], 19);
        assert_eq!(
            ElementType::from_serialized_value(&serialized).unwrap(),
            ElementType::ProvableSumTree
        );

        // NonCounted(ProvableSumTree) serializes with the wrapper byte 15
        // followed by the inner discriminant 19, and resolves to the
        // NonCountedProvableSumTree synthetic twin (= 147).
        let nc = Element::NonCounted(Box::new(Element::ProvableSumTree(None, 0, None)));
        let nc_serialized = nc.serialize(grove_version).expect("serialize NC PST");
        assert_eq!(nc_serialized[0], NON_COUNTED_WRAPPER_DISCRIMINANT);
        assert_eq!(nc_serialized[1], 19);
        assert_eq!(
            ElementType::from_serialized_value(&nc_serialized).unwrap(),
            ElementType::NonCountedProvableSumTree
        );
        assert_eq!(ElementType::NonCountedProvableSumTree as u8, 147);
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
            ElementType::from_serialized_value(&[16, 19]).unwrap(),
            ElementType::NotSummedProvableSumTree
        );

        // All other inner bytes are rejected: non-sum-bearing base types,
        // wrapper bytes (15/16/17), the new ReferenceWithSumItem base
        // (18 — a reference, not a sum-tree), synthetic NonCounted
        // twins (128..142, 146, 147), synthetic NotSummed twins
        // (177, 180..186), synthetic NotCountedOrSummed twins (193,
        // 196..202), and unallocated ranges. Note: `19`
        // (= ProvableSumTree base) IS accepted as inner — see the
        // explicit-success assertion above.
        for bad in [
            0u8, 1, 2, 3, 6, 8, 9, 11, 12, 13, 14, 15, 16, 17, 18, 100, 128, 142, 146, 147, 180,
            186, 193, 196, 202, 255,
        ] {
            assert!(
                ElementType::from_serialized_value(&[16, bad]).is_err(),
                "[16, {}] should be rejected",
                bad
            );
        }
    }

    /// Validate the resolver paths around byte 17 (NotCountedOrSummed
    /// wrapper). Mirrors `test_from_serialized_value_not_summed_paths`.
    #[test]
    fn test_from_serialized_value_not_counted_or_summed_paths() {
        // Truncated wrapper (no inner byte) is rejected.
        assert!(ElementType::from_serialized_value(&[17]).is_err());

        // Each of the five legal inner discriminants resolves to the right
        // synthetic twin.
        assert_eq!(
            ElementType::from_serialized_value(&[17, 4]).unwrap(),
            ElementType::NotCountedOrSummedSumTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[17, 5]).unwrap(),
            ElementType::NotCountedOrSummedBigSumTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[17, 7]).unwrap(),
            ElementType::NotCountedOrSummedCountSumTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[17, 10]).unwrap(),
            ElementType::NotCountedOrSummedProvableCountSumTree
        );
        assert_eq!(
            ElementType::from_serialized_value(&[17, 19]).unwrap(),
            ElementType::NotCountedOrSummedProvableSumTree
        );

        // All other inner bytes are rejected: non-sum-bearing base types,
        // wrapper bytes (15/16/17), the ReferenceWithSumItem base (18 — a
        // reference, not a sum-tree), synthetic NonCounted/NotSummed/
        // NotCountedOrSummed twins, and unallocated ranges.
        for bad in [
            0u8, 1, 2, 3, 6, 8, 9, 11, 12, 13, 14, 15, 16, 17, 18, 100, 128, 142, 146, 147, 177,
            180, 186, 193, 196, 202, 255,
        ] {
            assert!(
                ElementType::from_serialized_value(&[17, bad]).is_err(),
                "[17, {}] should be rejected",
                bad
            );
        }
    }
}
