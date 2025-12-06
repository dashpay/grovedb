//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.

#[cfg(feature = "minimal")]
mod constructor;
#[cfg(feature = "minimal")]
mod delete;
#[cfg(feature = "minimal")]
mod exists;
#[cfg(feature = "minimal")]
mod get;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub(crate) mod helpers;
#[cfg(feature = "minimal")]
mod insert;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod query;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use query::QueryOptions;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod serialize;
#[cfg(any(feature = "minimal", feature = "verify"))]
use std::fmt;

use bincode::{Decode, Encode};
#[cfg(feature = "minimal")]
use grovedb_merk::estimated_costs::SUM_AND_COUNT_LAYER_COST_SIZE;
#[cfg(feature = "minimal")]
use grovedb_merk::estimated_costs::SUM_VALUE_EXTRA_COST;
#[cfg(feature = "minimal")]
use grovedb_merk::estimated_costs::{
    BIG_SUM_LAYER_COST_SIZE, LAYER_COST_SIZE, SUM_LAYER_COST_SIZE,
};
#[cfg(feature = "minimal")]
use grovedb_merk::tree_type::TreeType;
#[cfg(feature = "minimal")]
use grovedb_visualize::visualize_to_vec;

use crate::operations::proof::util::hex_to_ascii;
#[cfg(any(feature = "minimal", feature = "verify"))]
use crate::reference_path::ReferencePathType;
#[cfg(feature = "minimal")]
use crate::OperationCost;

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Optional meta-data to be stored per element
pub type ElementFlags = Vec<u8>;

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Optional single byte to represent the maximum number of reference hop to
/// base element
pub type MaxReferenceHop = Option<u8>;

#[cfg(feature = "minimal")]
/// The cost of a tree
pub const TREE_COST_SIZE: u32 = LAYER_COST_SIZE; // 3
#[cfg(feature = "minimal")]
/// The cost of a sum item
///
/// It is 11 because we have 9 bytes for the sum value
/// 1 byte for the item type
/// 1 byte for the flags option
pub const SUM_ITEM_COST_SIZE: u32 = SUM_VALUE_EXTRA_COST + 2; // 11
#[cfg(feature = "minimal")]
/// The cost of a sum tree
pub const SUM_TREE_COST_SIZE: u32 = SUM_LAYER_COST_SIZE; // 12

#[cfg(feature = "minimal")]
/// The cost of a big sum tree
pub const BIG_SUM_TREE_COST_SIZE: u32 = BIG_SUM_LAYER_COST_SIZE; // 19

#[cfg(feature = "minimal")]
/// The cost of a count tree
pub const COUNT_TREE_COST_SIZE: u32 = SUM_LAYER_COST_SIZE; // 12

#[cfg(feature = "minimal")]
/// The cost of a count tree
pub const COUNT_SUM_TREE_COST_SIZE: u32 = SUM_AND_COUNT_LAYER_COST_SIZE; // 21

#[cfg(any(feature = "minimal", feature = "verify"))]
/// int 64 sum value
pub type SumValue = i64;

#[cfg(any(feature = "minimal", feature = "verify"))]
/// int 128 sum value
pub type BigSumValue = i128;

#[cfg(any(feature = "minimal", feature = "verify"))]
/// int 64 count value
pub type CountValue = u64;

#[cfg(feature = "minimal")]
pub trait CostSize {
    fn cost_size(&self) -> u32;
}

#[cfg(feature = "minimal")]
impl CostSize for TreeType {
    fn cost_size(&self) -> u32 {
        match self {
            TreeType::NormalTree => TREE_COST_SIZE,
            TreeType::SumTree => SUM_TREE_COST_SIZE,
            TreeType::BigSumTree => BIG_SUM_TREE_COST_SIZE,
            TreeType::CountTree => COUNT_TREE_COST_SIZE,
            TreeType::CountSumTree => COUNT_SUM_TREE_COST_SIZE,
            TreeType::ProvableCountTree => COUNT_TREE_COST_SIZE,
        }
    }
}

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Variants of GroveDB stored entities
///
/// ONLY APPEND TO THIS LIST!!! Because
/// of how serialization works.
#[derive(Clone, Encode, Decode, PartialEq, Eq, Hash)]
#[cfg_attr(not(any(feature = "minimal", feature = "visualize")), derive(Debug))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
        }
    }
}

impl Element {
    pub fn type_str(&self) -> &str {
        match self {
            Element::Item(..) => "item",
            Element::Reference(..) => "reference",
            Element::Tree(..) => "tree",
            Element::SumItem(..) => "sum item",
            Element::SumTree(..) => "sum tree",
            Element::BigSumTree(..) => "big sum tree",
            Element::CountTree(..) => "count tree",
            Element::CountSumTree(..) => "count sum tree",
            Element::ProvableCountTree(..) => "provable count tree",
            Element::ItemWithSumItem(..) => "item with sum item",
        }
    }

    #[cfg(feature = "minimal")]
    pub(crate) fn value_hash(
        &self,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<grovedb_merk::CryptoHash, crate::Error> {
        let bytes = grovedb_costs::cost_return_on_error_default!(self.serialize(grove_version));
        crate::value_hash(&bytes).map(Result::Ok)
    }
}

#[cfg(any(feature = "minimal", feature = "visualize"))]
impl fmt::Debug for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut v = Vec::new();
        visualize_to_vec(&mut v, self);

        f.write_str(&String::from_utf8_lossy(&v))
    }
}
