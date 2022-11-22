use crate::HASH_LENGTH_U32;
use crate::tree::kv::KV;

pub mod average_case_costs;
pub mod worst_case_costs;

/// The cost of a subtree layer
pub const LAYER_COST_SIZE: u32 = 3;

impl KV {
    fn encoded_kv_node_size(element_size: u32) -> u32 {
        // KV holds the state of a node
        // 32 bytes to encode the hash of the node
        // 32 bytes to encode the value hash
        // max_element_size to encode the worst case value size
        HASH_LENGTH_U32 + HASH_LENGTH_U32 + element_size
    }
}
