//! Helpers
//! Implements helper functions in Element

#[cfg(feature = "full")]
use grovedb_merk::tree::kv::{
    ValueDefinedCostType,
    ValueDefinedCostType::{LayeredValueDefinedCost, SpecializedValueDefinedCost},
};
#[cfg(feature = "full")]
use grovedb_merk::{
    tree::{kv::KV, TreeNode},
    TreeFeatureType,
    TreeFeatureType::{BasicMerkNode, SummedMerkNode},
};
use grovedb_version::{check_grovedb_v0, error::GroveVersionError, version::GroveVersion};
#[cfg(feature = "full")]
use integer_encoding::VarInt;

#[cfg(feature = "full")]
use crate::reference_path::path_from_reference_path_type;
#[cfg(any(feature = "full", feature = "verify"))]
use crate::reference_path::ReferencePathType;
#[cfg(feature = "full")]
use crate::{
    element::{SUM_ITEM_COST_SIZE, SUM_TREE_COST_SIZE, TREE_COST_SIZE},
    ElementFlags,
};
#[cfg(any(feature = "full", feature = "verify"))]
use crate::{Element, Error};

impl Element {
    #[cfg(any(feature = "full", feature = "verify"))]
    /// Decoded the integer value in the SumItem element type, returns 0 for
    /// everything else
    pub fn sum_value_or_default(&self) -> i64 {
        match self {
            Element::SumItem(sum_value, _) | Element::SumTree(_, sum_value, _) => *sum_value,
            _ => 0,
        }
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Decoded the integer value in the SumItem element type
    pub fn as_sum_item_value(&self) -> Result<i64, Error> {
        match self {
            Element::SumItem(value, _) => Ok(*value),
            _ => Err(Error::WrongElementType("expected a sum item")),
        }
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Decoded the integer value in the SumItem element type
    pub fn into_sum_item_value(self) -> Result<i64, Error> {
        match self {
            Element::SumItem(value, _) => Ok(value),
            _ => Err(Error::WrongElementType("expected a sum item")),
        }
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Decoded the integer value in the SumTree element type
    pub fn as_sum_tree_value(&self) -> Result<i64, Error> {
        match self {
            Element::SumTree(_, value, _) => Ok(*value),
            _ => Err(Error::WrongElementType("expected a sum tree")),
        }
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Decoded the integer value in the SumTree element type
    pub fn into_sum_tree_value(self) -> Result<i64, Error> {
        match self {
            Element::SumTree(_, value, _) => Ok(value),
            _ => Err(Error::WrongElementType("expected a sum tree")),
        }
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Gives the item value in the Item element type
    pub fn as_item_bytes(&self) -> Result<&[u8], Error> {
        match self {
            Element::Item(value, _) => Ok(value),
            _ => Err(Error::WrongElementType("expected an item")),
        }
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Gives the item value in the Item element type
    pub fn into_item_bytes(self) -> Result<Vec<u8>, Error> {
        match self {
            Element::Item(value, _) => Ok(value),
            _ => Err(Error::WrongElementType("expected an item")),
        }
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Gives the reference path type in the Reference element type
    pub fn into_reference_path_type(self) -> Result<ReferencePathType, Error> {
        match self {
            Element::Reference(value, ..) => Ok(value),
            _ => Err(Error::WrongElementType("expected a reference")),
        }
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Check if the element is a sum tree
    pub fn is_sum_tree(&self) -> bool {
        matches!(self, Element::SumTree(..))
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Check if the element is a tree but not a sum tree
    pub fn is_basic_tree(&self) -> bool {
        matches!(self, Element::Tree(..))
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Check if the element is a tree
    pub fn is_any_tree(&self) -> bool {
        matches!(self, Element::SumTree(..) | Element::Tree(..))
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Check if the element is a reference
    pub fn is_reference(&self) -> bool {
        matches!(self, Element::Reference(..))
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Check if the element is an item
    pub fn is_any_item(&self) -> bool {
        matches!(self, Element::Item(..) | Element::SumItem(..))
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Check if the element is an item
    pub fn is_basic_item(&self) -> bool {
        matches!(self, Element::Item(..))
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Check if the element is a sum item
    pub fn is_sum_item(&self) -> bool {
        matches!(self, Element::SumItem(..))
    }

    #[cfg(feature = "full")]
    /// Get the tree feature type
    pub fn get_feature_type(&self, parent_is_sum_tree: bool) -> Result<TreeFeatureType, Error> {
        match parent_is_sum_tree {
            true => Ok(SummedMerkNode(self.sum_value_or_default())),
            false => Ok(BasicMerkNode),
        }
    }

    #[cfg(feature = "full")]
    /// Grab the optional flag stored in an element
    pub fn get_flags(&self) -> &Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::SumItem(_, flags) => flags,
        }
    }

    #[cfg(feature = "full")]
    /// Grab the optional flag stored in an element
    pub fn get_flags_owned(self) -> Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::SumItem(_, flags) => flags,
        }
    }

    #[cfg(feature = "full")]
    /// Grab the optional flag stored in an element as mutable
    pub fn get_flags_mut(&mut self) -> &mut Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::SumItem(_, flags) => flags,
        }
    }

    #[cfg(feature = "full")]
    /// Sets the optional flag stored in an element
    pub fn set_flags(&mut self, new_flags: Option<ElementFlags>) {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::SumItem(_, flags) => *flags = new_flags,
        }
    }

    #[cfg(feature = "full")]
    /// Get the required item space
    pub fn required_item_space(
        len: u32,
        flag_len: u32,
        grove_version: &GroveVersion,
    ) -> Result<u32, Error> {
        check_grovedb_v0!(
            "required_item_space",
            grove_version.grovedb_versions.element.required_item_space
        );
        Ok(len + len.required_space() as u32 + flag_len + flag_len.required_space() as u32 + 1)
    }

    #[cfg(feature = "full")]
    /// Convert the reference to an absolute reference
    pub(crate) fn convert_if_reference_to_absolute_reference(
        self,
        path: &[&[u8]],
        key: Option<&[u8]>,
    ) -> Result<Element, Error> {
        // Convert any non-absolute reference type to an absolute one
        // we do this here because references are aggregated first then followed later
        // to follow non-absolute references, we need the path they are stored at
        // this information is lost during the aggregation phase.
        Ok(match &self {
            Element::Reference(reference_path_type, ..) => match reference_path_type {
                ReferencePathType::AbsolutePathReference(..) => self,
                _ => {
                    // Element is a reference and is not absolute.
                    // build the stored path for this reference
                    let absolute_path =
                        path_from_reference_path_type(reference_path_type.clone(), path, key)?;
                    // return an absolute reference that contains this info
                    Element::Reference(
                        ReferencePathType::AbsolutePathReference(absolute_path),
                        None,
                        None,
                    )
                }
            },
            _ => self,
        })
    }

    #[cfg(feature = "full")]
    /// Get tree costs for a key value
    pub fn specialized_costs_for_key_value(
        key: &Vec<u8>,
        value: &[u8],
        is_sum_node: bool,
        grove_version: &GroveVersion,
    ) -> Result<u32, Error> {
        check_grovedb_v0!(
            "specialized_costs_for_key_value",
            grove_version
                .grovedb_versions
                .element
                .specialized_costs_for_key_value
        );
        // todo: we actually don't need to deserialize the whole element
        let element = Element::deserialize(value, grove_version)?;
        let cost = match element {
            Element::Tree(_, flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = TREE_COST_SIZE + flags_len;
                let key_len = key.len() as u32;
                KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key_len,
                    value_len,
                    is_sum_node,
                )
            }
            Element::SumTree(_, _sum_value, flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = SUM_TREE_COST_SIZE + flags_len;
                let key_len = key.len() as u32;
                KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key_len,
                    value_len,
                    is_sum_node,
                )
            }
            Element::SumItem(.., flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = SUM_ITEM_COST_SIZE + flags_len;
                let key_len = key.len() as u32;
                KV::node_value_byte_cost_size(key_len, value_len, is_sum_node)
            }
            _ => KV::node_value_byte_cost_size(key.len() as u32, value.len() as u32, is_sum_node),
        };
        Ok(cost)
    }

    #[cfg(feature = "full")]
    /// Get tree cost for the element
    pub fn get_specialized_cost(&self, grove_version: &GroveVersion) -> Result<u32, Error> {
        check_grovedb_v0!(
            "get_specialized_cost",
            grove_version.grovedb_versions.element.get_specialized_cost
        );
        match self {
            Element::Tree(..) => Ok(TREE_COST_SIZE),
            Element::SumTree(..) => Ok(SUM_TREE_COST_SIZE),
            Element::SumItem(..) => Ok(SUM_ITEM_COST_SIZE),
            _ => Err(Error::CorruptedCodeExecution(
                "trying to get tree cost from non tree element",
            )),
        }
    }

    #[cfg(feature = "full")]
    /// Get the value defined cost for a serialized value
    pub fn value_defined_cost(&self, grove_version: &GroveVersion) -> Option<ValueDefinedCostType> {
        let Some(value_cost) = self.get_specialized_cost(grove_version).ok() else {
            return None;
        };

        let cost = value_cost
            + self.get_flags().as_ref().map_or(0, |flags| {
                let flags_len = flags.len() as u32;
                flags_len + flags_len.required_space() as u32
            });
        match self {
            Element::Tree(..) => Some(LayeredValueDefinedCost(cost)),
            Element::SumTree(..) => Some(LayeredValueDefinedCost(cost)),
            Element::SumItem(..) => Some(SpecializedValueDefinedCost(cost)),
            _ => None,
        }
    }

    #[cfg(feature = "full")]
    /// Get the value defined cost for a serialized value
    pub fn value_defined_cost_for_serialized_value(
        value: &[u8],
        grove_version: &GroveVersion,
    ) -> Option<ValueDefinedCostType> {
        let element = Element::deserialize(value, grove_version).ok()?;
        element.value_defined_cost(grove_version)
    }
}

#[cfg(feature = "full")]
/// Decode from bytes
pub fn raw_decode(bytes: &[u8], grove_version: &GroveVersion) -> Result<Element, Error> {
    let tree = TreeNode::decode_raw(
        bytes,
        vec![],
        Some(Element::value_defined_cost_for_serialized_value),
        grove_version,
    )
    .map_err(|e| Error::CorruptedData(e.to_string()))?;
    let element: Element = Element::deserialize(tree.value_as_slice(), grove_version)?;
    Ok(element)
}
