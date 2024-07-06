// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

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
#[cfg(feature = "full")]
use core::fmt;

use bincode::{Decode, Encode};
#[cfg(any(feature = "full", feature = "verify"))]
use grovedb_merk::estimated_costs::SUM_VALUE_EXTRA_COST;
#[cfg(feature = "full")]
use grovedb_merk::estimated_costs::{LAYER_COST_SIZE, SUM_LAYER_COST_SIZE};
#[cfg(feature = "full")]
use grovedb_visualize::visualize_to_vec;

#[cfg(any(feature = "full", feature = "verify"))]
use crate::reference_path::ReferencePathType;

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

#[cfg(any(feature = "full", feature = "verify"))]
/// int 64 sum value
pub type SumValue = i64;

#[cfg(any(feature = "full", feature = "verify"))]
/// Variants of GroveDB stored entities
///
/// ONLY APPEND TO THIS LIST!!! Because
/// of how serialization works.
#[derive(Clone, Encode, Decode, PartialEq, Eq, Hash)]
#[cfg_attr(not(any(feature = "full", feature = "visualize")), derive(Debug))]
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
                    root_key
                        .as_ref()
                        .map_or("None".to_string(), |k| hex::encode(k)),
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::SumItem(sum_value, flags) => {
                write!(
                    f,
                    "SumItem({}{}",
                    sum_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
            Element::SumTree(root_key, sum_value, flags) => {
                write!(
                    f,
                    "SumTree({}, {}{}",
                    root_key
                        .as_ref()
                        .map_or("None".to_string(), |k| hex::encode(k)),
                    sum_value,
                    flags
                        .as_ref()
                        .map_or(String::new(), |f| format!(", flags: {:?}", f))
                )
            }
        }
    }
}

fn hex_to_ascii(hex_value: &[u8]) -> String {
    if hex_value.len() == 1 && hex_value[0] < b"0"[0] {
        hex::encode(&hex_value)
    } else {
        String::from_utf8(hex_value.to_vec()).unwrap_or_else(|_| hex::encode(&hex_value))
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
        }
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
