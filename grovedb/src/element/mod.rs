//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.

#[cfg(feature = "full")]
mod insert;
#[cfg(feature = "full")]
mod delete;
#[cfg(any(feature = "full", feature = "verify"))]
mod serialize;
#[cfg(feature = "full")]
mod get;
#[cfg(feature = "full")]
mod query;
#[cfg(feature = "full")]
mod constructor;
#[cfg(feature = "full")]
pub(crate) mod helpers;
#[cfg(feature = "full")]
mod exists;

#[cfg(feature = "full")]
use core::fmt;





#[cfg(feature = "full")]
use merk::{
    estimated_costs::{LAYER_COST_SIZE, SUM_LAYER_COST_SIZE},
};
#[cfg(any(feature = "full", feature = "verify"))]
use serde::{Deserialize, Serialize};

#[cfg(feature = "full")]
use visualize::visualize_to_vec;


#[cfg(any(feature = "full", feature = "verify"))]
use crate::{reference_path::ReferencePathType};


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
#[cfg(feature = "full")]
/// The cost of a tree
pub const SUM_ITEM_COST_SIZE: u32 = 10;
#[cfg(feature = "full")]
/// The cost of a sum tree
pub const SUM_TREE_COST_SIZE: u32 = SUM_LAYER_COST_SIZE; // 11

#[cfg(any(feature = "full", feature = "verify"))]
/// int 64 sum value
pub type SumValue = i64;

#[cfg(any(feature = "full", feature = "verify"))]
/// Variants of GroveDB stored entities
/// ONLY APPEND TO THIS LIST!!! Because
/// of how serialization works.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Element {
    /// An ordinary value
    Item(Vec<u8>, Option<ElementFlags>),
    /// A reference to an object by its path
    Reference(ReferencePathType, MaxReferenceHop, Option<ElementFlags>),
    /// A subtree, contains the a prefixed key representing the root of the
    /// subtree.
    Tree(Option<Vec<u8>>, Option<ElementFlags>),
    /// Signed integer value that can be totaled in a sum tree
    SumItem(SumValue, Option<ElementFlags>),
    /// Same as Element::Tree but underlying Merk sums value of it's summable
    /// nodes
    SumTree(Option<Vec<u8>>, SumValue, Option<ElementFlags>),
}

#[cfg(feature = "full")]
impl fmt::Debug for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut v = Vec::new();
        visualize_to_vec(&mut v, self);

        f.write_str(&String::from_utf8_lossy(&v))
    }
}
