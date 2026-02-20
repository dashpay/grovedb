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

/// The cost of a commitment tree (32 bytes sinsemilla_root + 9 bytes
/// total_count (u64 varint worst case) + 1 byte chunk_power (u8) + 2 bytes
/// overhead)
pub const COMMITMENT_TREE_COST_SIZE: u32 = 32 + 9 + 1 + 2; // 44

/// The cost of an MMR tree (32 bytes for mmr_root + 9 bytes for mmr_size
/// (u64 varint worst case) + 2 bytes overhead)
pub const MMR_TREE_COST_SIZE: u32 = 32 + 9 + 2; // 43

/// The cost of a bulk-append tree (32 bytes state_root + 9 bytes total_count
/// (u64 varint worst case) + 1 byte chunk_power (u8) + 2 bytes overhead)
pub const BULK_APPEND_TREE_COST_SIZE: u32 = 32 + 9 + 1 + 2; // 44

/// The cost of a dense tree (32 bytes root_hash + 3 bytes count (u16 varint
/// worst case) + 1 byte height (u8) + 2 bytes overhead)
pub const DENSE_TREE_COST_SIZE: u32 = 32 + 3 + 1 + 2; // 38

pub trait CostSize {
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
            TreeType::CommitmentTree => COMMITMENT_TREE_COST_SIZE,
            TreeType::MmrTree => MMR_TREE_COST_SIZE,
            TreeType::BulkAppendTree => BULK_APPEND_TREE_COST_SIZE,
            TreeType::DenseAppendOnlyFixedSizeTree => DENSE_TREE_COST_SIZE,
        }
    }
}
