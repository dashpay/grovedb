use crate::{
    estimated_costs::{
        BIG_SUM_LAYER_COST_SIZE, LAYER_COST_SIZE, SUM_AND_COUNT_LAYER_COST_SIZE,
        SUM_LAYER_COST_SIZE, SUM_VALUE_EXTRA_COST,
    },
    TreeType,
};

/// The cost of a tree
pub const TREE_COST_SIZE: u32 = LAYER_COST_SIZE; // 3

/// The cost of a sum item
///
/// It is 11 because we have 9 bytes for the sum value
/// 1 byte for the item type
/// 1 byte for the flags option
pub const SUM_ITEM_COST_SIZE: u32 = SUM_VALUE_EXTRA_COST + 2; // 11

/// The cost of a sum tree
pub const SUM_TREE_COST_SIZE: u32 = SUM_LAYER_COST_SIZE; // 12

/// The cost of a big sum tree
pub const BIG_SUM_TREE_COST_SIZE: u32 = BIG_SUM_LAYER_COST_SIZE; // 19

/// The cost of a count tree
pub const COUNT_TREE_COST_SIZE: u32 = SUM_LAYER_COST_SIZE; // 12

/// The cost of a count sum tree
pub const COUNT_SUM_TREE_COST_SIZE: u32 = SUM_AND_COUNT_LAYER_COST_SIZE; // 21

/// The cost of a commitment tree (9 bytes total_count (u64 varint worst case)
/// + 1 byte chunk_power (u8) + 2 bytes overhead)
pub const COMMITMENT_TREE_COST_SIZE: u32 = 9 + 1 + 2; // 12

/// The cost of an MMR tree (9 bytes for mmr_size (u64 varint worst case) +
/// 2 bytes overhead). The MMR root hash is stored as the Merk child hash,
/// not in the Element.
pub const MMR_TREE_COST_SIZE: u32 = 9 + 2; // 11

/// The cost of a bulk-append tree (9 bytes total_count (u64 varint worst case)
/// + 1 byte chunk_power (u8) + 2 bytes overhead)
pub const BULK_APPEND_TREE_COST_SIZE: u32 = 9 + 1 + 2; // 12

/// The cost of a dense tree (3 bytes count (u16 varint worst case) + 1 byte
/// height (u8) + 2 bytes overhead)
pub const DENSE_TREE_COST_SIZE: u32 = 3 + 1 + 2; // 6

/// The cost of a private document store (9 bytes total_count (u64 varint
/// worst case) + 5 bytes entry_size (u32 varint worst case) + 1 byte
/// chunk_power (u8) + 2 bytes overhead). Same shape as
/// `BULK_APPEND_TREE_COST_SIZE` plus the committed entry size field.
pub const PRIVATE_DOCUMENT_STORE_COST_SIZE: u32 = 9 + 5 + 1 + 2; // 17

/// The cost of a count-indexed tree element. Same shape as `COUNT_TREE_COST_SIZE`
/// but with one extra byte of overhead for the second `Option<root_key>` field
/// (the secondary root key).
pub const COUNT_INDEXED_TREE_COST_SIZE: u32 = COUNT_TREE_COST_SIZE + 1; // 13

/// The cost of a provable-count + provable-sum indexed tree element.
///
/// Unlike the single-axis indexed trees this carries BOTH aggregates plus a
/// variable-length axes TLV, so it is sized as the count+sum layer cost plus
/// the TLV's worst case: one length byte, and for each of the maximum three
/// axes one tag byte and one `Option<root_key>` discriminant byte. Sizing it
/// like the single-axis trees put it *below* its own minimum serialized
/// payload, and `merk::tree::Tree` disables `StorageCost::verify` for layered
/// nodes so nothing caught the shortfall.
pub const PROVABLE_COUNT_PROVABLE_SUM_INDEXED_TREE_COST_SIZE: u32 =
    SUM_AND_COUNT_LAYER_COST_SIZE + 1 + 3 * 2; // 28

/// Provides the serialized cost size in bytes for a tree type.
pub trait CostSize {
    /// Returns the cost size in bytes for this value.
    fn cost_size(&self) -> u32;
}

impl CostSize for TreeType {
    fn cost_size(&self) -> u32 {
        match self {
            TreeType::NormalTree => TREE_COST_SIZE,
            TreeType::SumTree => SUM_TREE_COST_SIZE,
            TreeType::BigSumTree => BIG_SUM_TREE_COST_SIZE,
            TreeType::CountTree => COUNT_TREE_COST_SIZE,
            TreeType::CountSumTree => COUNT_SUM_TREE_COST_SIZE,
            TreeType::ProvableCountTree => COUNT_TREE_COST_SIZE,
            TreeType::ProvableCountSumTree => COUNT_SUM_TREE_COST_SIZE,
            TreeType::CommitmentTree(_) => COMMITMENT_TREE_COST_SIZE,
            TreeType::MmrTree => MMR_TREE_COST_SIZE,
            TreeType::BulkAppendTree(_) => BULK_APPEND_TREE_COST_SIZE,
            TreeType::DenseAppendOnlyFixedSizeTree(_) => DENSE_TREE_COST_SIZE,
            // ProvableSumTree mirrors SumTree's cost.
            TreeType::ProvableSumTree => SUM_TREE_COST_SIZE,
            // ProvableCountProvableSumTree carries both a count and a sum
            // like ProvableCountSumTree, so reuse its cost size.
            TreeType::ProvableCountProvableSumTree => COUNT_SUM_TREE_COST_SIZE,
            TreeType::ProvableSumIndexedTree | TreeType::ProvableCountIndexedTree => {
                COUNT_INDEXED_TREE_COST_SIZE
            }
            // PCPSIT carries both aggregates and a variable-length axes TLV,
            // so it gets its own worst-case constant rather than the
            // single-axis indexed cost, which was smaller than the element's
            // own minimum payload.
            TreeType::ProvableCountProvableSumIndexedTree => {
                PROVABLE_COUNT_PROVABLE_SUM_INDEXED_TREE_COST_SIZE
            }
            TreeType::PrivateDocumentStore(_) => PRIVATE_DOCUMENT_STORE_COST_SIZE,
        }
    }
}
