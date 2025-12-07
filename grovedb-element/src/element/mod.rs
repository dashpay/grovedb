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

use crate::reference_path::ReferencePathType;

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
#[derive(Clone, Encode, Decode, PartialEq, Eq, Hash)]
#[cfg_attr(not(feature = "visualize"), derive(Debug))]
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
}
