//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.

#[cfg(feature = "full")]
mod constructor;
#[cfg(feature = "full")]
mod delete;
#[cfg(feature = "full")]
mod exists;
#[cfg(feature = "full")]
mod get;
#[cfg(any(feature = "full", feature = "verify"))]
pub(crate) mod helpers;
#[cfg(feature = "full")]
mod insert;
#[cfg(any(feature = "full", feature = "verify"))]
mod query;
#[cfg(any(feature = "full", feature = "verify"))]
pub use query::QueryOptions;
#[cfg(any(feature = "full", feature = "verify"))]
mod serialize;
#[cfg(any(feature = "full", feature = "verify"))]
use std::fmt;

use bincode::{Decode, Encode};
#[cfg(any(feature = "full", feature = "verify"))]
use grovedb_merk::estimated_costs::SUM_VALUE_EXTRA_COST;
#[cfg(feature = "full")]
use grovedb_merk::estimated_costs::{LAYER_COST_SIZE, SUM_LAYER_COST_SIZE};
#[cfg(feature = "full")]
use grovedb_visualize::visualize_to_vec;
pub(crate) use insert::Delta;

#[cfg(any(feature = "full", feature = "verify"))]
use crate::reference_path::ReferencePathType;
#[cfg(feature = "full")]
use crate::OperationCost;
use crate::{
    bidirectional_references::BidirectionalReference, operations::proof::util::hex_to_ascii,
};

#[cfg(any(feature = "full", feature = "verify"))]
/// Optional meta-data to be stored per element
pub type ElementFlags = Vec<u8>;

#[cfg(any(feature = "full", feature = "verify"))]
/// Optional single byte to represent the maximum number of reference hop to
/// base element
pub type MaxReferenceHop = Option<u8>;

#[cfg(feature = "full")]
/// The cost of a tree
pub const TREE_COST_SIZE: u32 = LAYER_COST_SIZE; // 3
#[cfg(any(feature = "full", feature = "verify"))]
/// The cost of a sum item
///
/// It is 11 because we have 9 bytes for the sum value
/// 1 byte for the item type
/// 1 byte for the flags option
pub const SUM_ITEM_COST_SIZE: u32 = SUM_VALUE_EXTRA_COST + 2; // 11
#[cfg(feature = "full")]
/// The cost of a sum tree
pub const SUM_TREE_COST_SIZE: u32 = SUM_LAYER_COST_SIZE; // 12

/// int 64 sum value
pub type SumValue = i64;

#[cfg(any(feature = "full", feature = "verify"))]
/// Variants of GroveDB stored entities
///
/// ONLY APPEND TO THIS LIST!!! Because
/// of how serialization works.
#[derive(Clone, Encode, Decode, PartialEq, Eq, Hash)]
#[cfg_attr(not(any(feature = "full", feature = "visualize")), derive(Debug))]
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
    /// A reference to an object by its path
    BidirectionalReference(BidirectionalReference),
    /// An ordinary value that has a backwards reference
    ItemWithBackwardsReferences(Vec<u8>, Option<ElementFlags>),
    /// Signed integer value that can be totaled in a sum tree that has a
    /// backwards reference
    SumItemWithBackwardsReferences(SumValue, Option<ElementFlags>),
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
            Element::BidirectionalReference(BidirectionalReference {
                forward_reference_path,
                cascade_on_update,
                max_hop,
                flags,
                ..
            }) => {
                // TODO: print something on backward_references
                write!(
                    f,
                    "BidirectionalReference({forward_reference_path}, max_hop: {}{}, cascade: \
                     {cascade_on_update})",
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
            Element::ItemWithBackwardsReferences(data, flags) => write!(
                f,
                "ItemWithBackwardReferences({}{})",
                hex_to_ascii(data),
                flags
                    .as_ref()
                    .map_or(String::new(), |f| format!(", flags: {:?}", f))
            ),
            Element::SumItemWithBackwardsReferences(sum_value, flags) => write!(
                f,
                "SumItemWithBackwardReferences({}{})",
                sum_value,
                flags
                    .as_ref()
                    .map_or(String::new(), |f| format!(", flags: {:?}", f))
            ),
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
            Element::BidirectionalReference(..) => "bidirectional reference",
            Element::ItemWithBackwardsReferences(..) => "item with backwards references",
            Element::SumItemWithBackwardsReferences(..) => "sum item with backwards references",
        }
    }

    #[cfg(feature = "full")]
    pub(crate) fn value_hash(
        &self,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> grovedb_costs::CostResult<grovedb_merk::CryptoHash, crate::Error> {
        let bytes = grovedb_costs::cost_return_on_error_default!(self.serialize(grove_version));
        crate::value_hash(&bytes).map(Result::Ok)
    }
}

#[cfg(any(feature = "full", feature = "visualize"))]
impl fmt::Debug for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut v = Vec::new();
        visualize_to_vec(&mut v, self);

        f.write_str(&String::from_utf8_lossy(&v))
    }
}
