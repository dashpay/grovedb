//! Helpers
//! Implements helper functions in Element

#[cfg(feature = "minimal")]
use grovedb_merk::{
    merk::NodeType,
    tree::{
        kv::{
            ValueDefinedCostType,
            ValueDefinedCostType::{LayeredValueDefinedCost, SpecializedValueDefinedCost},
            KV,
        },
        TreeNode,
    },
};
#[cfg(any(feature = "minimal", feature = "verify"))]
use grovedb_merk::{
    tree_type::{MaybeTree, TreeType},
    TreeFeatureType,
    TreeFeatureType::{
        BasicMerkNode, BigSummedMerkNode, CountedMerkNode, CountedSummedMerkNode, SummedMerkNode,
    },
};
#[cfg(feature = "minimal")]
use grovedb_version::{check_grovedb_v0, version::GroveVersion};
#[cfg(feature = "minimal")]
use integer_encoding::VarInt;

#[cfg(feature = "minimal")]
use crate::element::{
    BIG_SUM_TREE_COST_SIZE, COUNT_SUM_TREE_COST_SIZE, COUNT_TREE_COST_SIZE, SUM_ITEM_COST_SIZE,
    SUM_TREE_COST_SIZE, TREE_COST_SIZE,
};
#[cfg(feature = "minimal")]
use crate::reference_path::path_from_reference_path_type;
#[cfg(any(feature = "minimal", feature = "verify"))]
use crate::reference_path::ReferencePathType;
#[cfg(any(feature = "minimal", feature = "verify"))]
use crate::{Element, ElementFlags, Error};

impl Element {
    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Decoded the integer value in the SumItem element type, returns 0 for
    /// everything else
    pub fn sum_value_or_default(&self) -> i64 {
        match self {
            Element::SumItem(sum_value, _)
            | Element::ItemWithSumItem(_, sum_value, _)
            | Element::SumTree(_, sum_value, _) => *sum_value,
            _ => 0,
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Decoded the integer value in the CountTree element type, returns 1 for
    /// everything else
    pub fn count_value_or_default(&self) -> u64 {
        match self {
            Element::CountTree(_, count_value, _)
            | Element::ProvableCountTree(_, count_value, _) => *count_value,
            _ => 1,
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Decoded the integer value in the CountTree element type, returns 1 for
    /// everything else
    pub fn count_sum_value_or_default(&self) -> (u64, i64) {
        match self {
            Element::SumItem(sum_value, _)
            | Element::ItemWithSumItem(_, sum_value, _)
            | Element::SumTree(_, sum_value, _) => (1, *sum_value),
            Element::CountTree(_, count_value, _) => (*count_value, 0),
            Element::CountSumTree(_, count_value, sum_value, _) => (*count_value, *sum_value),
            Element::ProvableCountTree(_, count_value, _) => (*count_value, 0),
            _ => (1, 0),
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Decoded the integer value in the SumItem element type, returns 0 for
    /// everything else
    pub fn big_sum_value_or_default(&self) -> i128 {
        match self {
            Element::SumItem(sum_value, _)
            | Element::ItemWithSumItem(_, sum_value, _)
            | Element::SumTree(_, sum_value, _) => *sum_value as i128,
            Element::BigSumTree(_, sum_value, _) => *sum_value,
            _ => 0,
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Decoded the integer value in the SumItem element type
    pub fn as_sum_item_value(&self) -> Result<i64, Error> {
        match self {
            Element::SumItem(value, _) => Ok(*value),
            Element::ItemWithSumItem(_, value, _) => Ok(*value),
            _ => Err(Error::WrongElementType("expected a sum item")),
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Decoded the integer value in the SumItem element type
    pub fn into_sum_item_value(self) -> Result<i64, Error> {
        match self {
            Element::SumItem(value, _) => Ok(value),
            Element::ItemWithSumItem(_, value, _) => Ok(value),
            _ => Err(Error::WrongElementType("expected a sum item")),
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Decoded the integer value in the SumTree element type
    pub fn as_sum_tree_value(&self) -> Result<i64, Error> {
        match self {
            Element::SumTree(_, value, _) => Ok(*value),
            _ => Err(Error::WrongElementType("expected a sum tree")),
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Decoded the integer value in the SumTree element type
    pub fn into_sum_tree_value(self) -> Result<i64, Error> {
        match self {
            Element::SumTree(_, value, _) => Ok(value),
            _ => Err(Error::WrongElementType("expected a sum tree")),
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Gives the item value in the Item element type
    pub fn as_item_bytes(&self) -> Result<&[u8], Error> {
        match self {
            Element::Item(value, _) => Ok(value),
            Element::ItemWithSumItem(value, ..) => Ok(value),
            _ => Err(Error::WrongElementType("expected an item")),
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Gives the item value in the Item element type
    pub fn into_item_bytes(self) -> Result<Vec<u8>, Error> {
        match self {
            Element::Item(value, _) => Ok(value),
            Element::ItemWithSumItem(value, ..) => Ok(value),
            _ => Err(Error::WrongElementType("expected an item")),
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Gives the reference path type in the Reference element type
    pub fn into_reference_path_type(self) -> Result<ReferencePathType, Error> {
        match self {
            Element::Reference(value, ..) => Ok(value),
            _ => Err(Error::WrongElementType("expected a reference")),
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a sum tree
    pub fn is_sum_tree(&self) -> bool {
        matches!(self, Element::SumTree(..))
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a tree and return the root_tree info and tree
    /// type
    pub fn root_key_and_tree_type_owned(self) -> Option<(Option<Vec<u8>>, TreeType)> {
        match self {
            Element::Tree(root_key, _) => Some((root_key, TreeType::NormalTree)),
            Element::SumTree(root_key, ..) => Some((root_key, TreeType::SumTree)),
            Element::BigSumTree(root_key, ..) => Some((root_key, TreeType::BigSumTree)),
            Element::CountTree(root_key, ..) => Some((root_key, TreeType::CountTree)),
            Element::CountSumTree(root_key, ..) => Some((root_key, TreeType::CountSumTree)),
            Element::ProvableCountTree(root_key, ..) => {
                Some((root_key, TreeType::ProvableCountTree))
            }
            _ => None,
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a tree and return the root_tree info and the
    /// tree type
    pub fn root_key_and_tree_type(&self) -> Option<(&Option<Vec<u8>>, TreeType)> {
        match self {
            Element::Tree(root_key, _) => Some((root_key, TreeType::NormalTree)),
            Element::SumTree(root_key, ..) => Some((root_key, TreeType::SumTree)),
            Element::BigSumTree(root_key, ..) => Some((root_key, TreeType::BigSumTree)),
            Element::CountTree(root_key, ..) => Some((root_key, TreeType::CountTree)),
            Element::CountSumTree(root_key, ..) => Some((root_key, TreeType::CountSumTree)),
            Element::ProvableCountTree(root_key, ..) => {
                Some((root_key, TreeType::ProvableCountTree))
            }
            _ => None,
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a tree and return the flags and the tree type
    pub fn tree_flags_and_type(&self) -> Option<(&Option<ElementFlags>, TreeType)> {
        match self {
            Element::Tree(_, flags) => Some((flags, TreeType::NormalTree)),
            Element::SumTree(_, _, flags) => Some((flags, TreeType::SumTree)),
            Element::BigSumTree(_, _, flags) => Some((flags, TreeType::BigSumTree)),
            Element::CountTree(_, _, flags) => Some((flags, TreeType::CountTree)),
            Element::CountSumTree(.., flags) => Some((flags, TreeType::CountSumTree)),
            Element::ProvableCountTree(_, _, flags) => Some((flags, TreeType::ProvableCountTree)),
            _ => None,
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a tree and return the tree type
    pub fn tree_type(&self) -> Option<TreeType> {
        match self {
            Element::Tree(..) => Some(TreeType::NormalTree),
            Element::SumTree(..) => Some(TreeType::SumTree),
            Element::BigSumTree(..) => Some(TreeType::BigSumTree),
            Element::CountTree(..) => Some(TreeType::CountTree),
            Element::CountSumTree(..) => Some(TreeType::CountSumTree),
            Element::ProvableCountTree(..) => Some(TreeType::ProvableCountTree),
            _ => None,
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a tree and return the aggregate of elements in
    /// the tree
    pub fn tree_feature_type(&self) -> Option<TreeFeatureType> {
        match self {
            Element::Tree(..) => Some(BasicMerkNode),
            Element::SumTree(_, value, _) => Some(SummedMerkNode(*value)),
            Element::BigSumTree(_, value, _) => Some(BigSummedMerkNode(*value)),
            Element::CountTree(_, value, _) => Some(CountedMerkNode(*value)),
            Element::CountSumTree(_, count, sum, _) => Some(CountedSummedMerkNode(*count, *sum)),
            Element::ProvableCountTree(_, value, _) => {
                Some(TreeFeatureType::ProvableCountedMerkNode(*value))
            }
            _ => None,
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a tree and return the tree type
    pub fn maybe_tree_type(&self) -> MaybeTree {
        match self {
            Element::Tree(..) => MaybeTree::Tree(TreeType::NormalTree),
            Element::SumTree(..) => MaybeTree::Tree(TreeType::SumTree),
            Element::BigSumTree(..) => MaybeTree::Tree(TreeType::BigSumTree),
            Element::CountTree(..) => MaybeTree::Tree(TreeType::CountTree),
            Element::CountSumTree(..) => MaybeTree::Tree(TreeType::CountSumTree),
            Element::ProvableCountTree(..) => MaybeTree::Tree(TreeType::ProvableCountTree),
            _ => MaybeTree::NotTree,
        }
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a big sum tree
    pub fn is_big_sum_tree(&self) -> bool {
        matches!(self, Element::BigSumTree(..))
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a tree but not a sum tree
    pub fn is_basic_tree(&self) -> bool {
        matches!(self, Element::Tree(..))
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a tree
    pub fn is_any_tree(&self) -> bool {
        matches!(
            self,
            Element::SumTree(..)
                | Element::Tree(..)
                | Element::BigSumTree(..)
                | Element::CountTree(..)
                | Element::CountSumTree(..)
                | Element::ProvableCountTree(..)
        )
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a reference
    pub fn is_reference(&self) -> bool {
        matches!(self, Element::Reference(..))
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is an item
    pub fn is_any_item(&self) -> bool {
        matches!(
            self,
            Element::Item(..) | Element::SumItem(..) | Element::ItemWithSumItem(..)
        )
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is an item
    pub fn is_basic_item(&self) -> bool {
        matches!(self, Element::Item(..))
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is an item
    pub fn has_basic_item(&self) -> bool {
        matches!(self, Element::Item(..) | Element::ItemWithSumItem(..))
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a sum item
    pub fn is_sum_item(&self) -> bool {
        matches!(self, Element::SumItem(..) | Element::ItemWithSumItem(..))
    }

    #[cfg(any(feature = "minimal", feature = "verify"))]
    /// Check if the element is a sum item
    pub fn is_item_with_sum_item(&self) -> bool {
        matches!(self, Element::ItemWithSumItem(..))
    }

    #[cfg(feature = "minimal")]
    /// Get the tree feature type
    pub fn get_feature_type(&self, parent_tree_type: TreeType) -> Result<TreeFeatureType, Error> {
        match parent_tree_type {
            TreeType::NormalTree => Ok(BasicMerkNode),
            TreeType::SumTree => Ok(SummedMerkNode(self.sum_value_or_default())),
            TreeType::BigSumTree => Ok(BigSummedMerkNode(self.big_sum_value_or_default())),
            TreeType::CountTree => Ok(CountedMerkNode(self.count_value_or_default())),
            TreeType::CountSumTree => {
                let v = self.count_sum_value_or_default();
                Ok(CountedSummedMerkNode(v.0, v.1))
            }
            TreeType::ProvableCountTree => Ok(TreeFeatureType::ProvableCountedMerkNode(
                self.count_value_or_default(),
            )),
        }
    }

    #[cfg(feature = "minimal")]
    /// Grab the optional flag stored in an element
    pub fn get_flags(&self) -> &Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::BigSumTree(.., flags)
            | Element::CountTree(.., flags)
            | Element::SumItem(_, flags)
            | Element::CountSumTree(.., flags)
            | Element::ProvableCountTree(.., flags) => flags,
            Element::ItemWithSumItem(.., flags) => flags,
        }
    }

    #[cfg(feature = "minimal")]
    /// Grab the optional flag stored in an element
    pub fn get_flags_owned(self) -> Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::BigSumTree(.., flags)
            | Element::CountTree(.., flags)
            | Element::SumItem(_, flags)
            | Element::CountSumTree(.., flags)
            | Element::ProvableCountTree(.., flags) => flags,
            Element::ItemWithSumItem(.., flags) => flags,
        }
    }

    #[cfg(feature = "minimal")]
    /// Grab the optional flag stored in an element as mutable
    pub fn get_flags_mut(&mut self) -> &mut Option<ElementFlags> {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::BigSumTree(.., flags)
            | Element::CountTree(.., flags)
            | Element::SumItem(_, flags)
            | Element::CountSumTree(.., flags)
            | Element::ProvableCountTree(.., flags) => flags,
            Element::ItemWithSumItem(.., flags) => flags,
        }
    }

    #[cfg(feature = "minimal")]
    /// Sets the optional flag stored in an element
    pub fn set_flags(&mut self, new_flags: Option<ElementFlags>) {
        match self {
            Element::Tree(_, flags)
            | Element::Item(_, flags)
            | Element::Reference(_, _, flags)
            | Element::SumTree(.., flags)
            | Element::BigSumTree(.., flags)
            | Element::CountTree(.., flags)
            | Element::SumItem(_, flags)
            | Element::CountSumTree(.., flags)
            | Element::ProvableCountTree(.., flags) => *flags = new_flags,
            Element::ItemWithSumItem(.., flags) => *flags = new_flags,
        }
    }

    #[cfg(feature = "minimal")]
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

    #[cfg(feature = "minimal")]
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

    #[cfg(feature = "minimal")]
    /// Get tree costs for a key value
    pub fn specialized_costs_for_key_value(
        key: &[u8],
        value: &[u8],
        node_type: NodeType,
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
                    key_len, value_len, node_type,
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
                    key_len, value_len, node_type,
                )
            }
            Element::BigSumTree(_, _sum_value, flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = BIG_SUM_TREE_COST_SIZE + flags_len;
                let key_len = key.len() as u32;
                KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key_len, value_len, node_type,
                )
            }
            Element::CountTree(_, _count_value, flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = COUNT_TREE_COST_SIZE + flags_len;
                let key_len = key.len() as u32;
                KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key_len, value_len, node_type,
                )
            }
            Element::CountSumTree(.., flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = COUNT_SUM_TREE_COST_SIZE + flags_len;
                let key_len = key.len() as u32;
                KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key_len, value_len, node_type,
                )
            }
            Element::ProvableCountTree(_, _count_value, flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = COUNT_TREE_COST_SIZE + flags_len;
                let key_len = key.len() as u32;
                KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key_len, value_len, node_type,
                )
            }
            Element::SumItem(.., flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = SUM_ITEM_COST_SIZE + flags_len;
                let key_len = key.len() as u32;
                KV::node_value_byte_cost_size(key_len, value_len, node_type)
            }
            Element::ItemWithSumItem(item_value, _, flags) => {
                let item_value_len = item_value.len() as u32;
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = item_value_len
                    + item_value_len.required_space() as u32
                    + SUM_ITEM_COST_SIZE
                    + flags_len;
                let key_len = key.len() as u32;
                KV::node_value_byte_cost_size(key_len, value_len, node_type)
            }
            _ => KV::node_value_byte_cost_size(key.len() as u32, value.len() as u32, node_type),
        };
        Ok(cost)
    }

    #[cfg(feature = "minimal")]
    /// Get tree cost for the element
    fn get_specialized_cost(&self, grove_version: &GroveVersion) -> Result<u32, Error> {
        check_grovedb_v0!(
            "get_specialized_cost",
            grove_version.grovedb_versions.element.get_specialized_cost
        );
        match self {
            Element::Tree(..) => Ok(TREE_COST_SIZE),
            Element::SumTree(..) => Ok(SUM_TREE_COST_SIZE),
            Element::BigSumTree(..) => Ok(BIG_SUM_TREE_COST_SIZE),
            Element::SumItem(..) | Element::ItemWithSumItem(..) => Ok(SUM_ITEM_COST_SIZE),
            Element::CountTree(..) => Ok(COUNT_TREE_COST_SIZE),
            Element::CountSumTree(..) => Ok(COUNT_SUM_TREE_COST_SIZE),
            Element::ProvableCountTree(..) => Ok(COUNT_TREE_COST_SIZE),
            _ => Err(Error::CorruptedCodeExecution(
                "trying to get tree cost from non tree element",
            )),
        }
    }

    #[cfg(feature = "minimal")]
    /// Get the value defined cost for a serialized value item with sum item or
    /// sum item
    pub fn specialized_value_defined_cost(&self, grove_version: &GroveVersion) -> Option<u32> {
        let value_cost = self.get_specialized_cost(grove_version).ok()?;

        let cost = value_cost
            + self.get_flags().as_ref().map_or(0, |flags| {
                let flags_len = flags.len() as u32;
                flags_len + flags_len.required_space() as u32
            });
        match self {
            Element::SumItem(..) => Some(cost),
            Element::ItemWithSumItem(item, ..) => {
                let item_len = item.len() as u32;
                Some(cost + item_len + item_len.required_space() as u32)
            }
            _ => None,
        }
    }

    #[cfg(feature = "minimal")]
    /// Get the value defined cost for a serialized value item with a tree
    pub fn layered_value_defined_cost(&self, grove_version: &GroveVersion) -> Option<u32> {
        let value_cost = self.get_specialized_cost(grove_version).ok()?;

        let cost = value_cost
            + self.get_flags().as_ref().map_or(0, |flags| {
                let flags_len = flags.len() as u32;
                flags_len + flags_len.required_space() as u32
            });
        match self {
            Element::Tree(..)
            | Element::SumTree(..)
            | Element::BigSumTree(..)
            | Element::CountTree(..)
            | Element::CountSumTree(..) => Some(cost),
            _ => None,
        }
    }

    #[cfg(feature = "minimal")]
    /// Get the value defined cost for a serialized value
    pub fn value_defined_cost(&self, grove_version: &GroveVersion) -> Option<ValueDefinedCostType> {
        let value_cost = self.get_specialized_cost(grove_version).ok()?;

        let cost = value_cost
            + self.get_flags().as_ref().map_or(0, |flags| {
                let flags_len = flags.len() as u32;
                flags_len + flags_len.required_space() as u32
            });
        match self {
            Element::Tree(..) => Some(LayeredValueDefinedCost(cost)),
            Element::SumTree(..) => Some(LayeredValueDefinedCost(cost)),
            Element::BigSumTree(..) => Some(LayeredValueDefinedCost(cost)),
            Element::CountTree(..) => Some(LayeredValueDefinedCost(cost)),
            Element::CountSumTree(..) => Some(LayeredValueDefinedCost(cost)),
            Element::ProvableCountTree(..) => Some(LayeredValueDefinedCost(cost)),
            Element::SumItem(..) => Some(SpecializedValueDefinedCost(cost)),
            Element::ItemWithSumItem(item, ..) => {
                let item_len = item.len() as u32;
                Some(SpecializedValueDefinedCost(
                    cost + item_len + item_len.required_space() as u32,
                ))
            }
            _ => None,
        }
    }

    #[cfg(feature = "minimal")]
    /// Get the value defined cost for a serialized value
    pub fn value_defined_cost_for_serialized_value(
        value: &[u8],
        grove_version: &GroveVersion,
    ) -> Option<ValueDefinedCostType> {
        let element = Element::deserialize(value, grove_version).ok()?;
        element.value_defined_cost(grove_version)
    }
}

#[cfg(feature = "minimal")]
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

#[cfg(test)]
mod tests {
    use grovedb_merk::tree::kv::ValueDefinedCostType::SpecializedValueDefinedCost;
    use grovedb_version::version::GroveVersion;

    use super::*;

    #[test]
    fn item_with_sum_item_helpers_cover_all_behaviors() {
        let grove_version = GroveVersion::latest();
        let flags = Some(vec![1, 2, 3]);
        let element = Element::ItemWithSumItem(b"payload".to_vec(), 42, flags.clone());

        assert!(element.is_any_item());
        assert!(element.has_basic_item());
        assert!(element.is_sum_item());
        assert!(element.is_item_with_sum_item());
        assert_eq!(element.sum_value_or_default(), 42);
        assert_eq!(element.count_sum_value_or_default(), (1, 42));
        assert_eq!(element.big_sum_value_or_default(), 42);
        assert_eq!(element.as_item_bytes().unwrap(), b"payload");
        assert_eq!(
            element.clone().into_item_bytes().unwrap(),
            b"payload".to_vec()
        );
        assert_eq!(element.as_sum_item_value().unwrap(), 42);
        assert_eq!(element.clone().into_sum_item_value().unwrap(), 42);
        assert_eq!(element.get_flags(), &flags);

        let serialized = element.serialize(grove_version).expect("serialize element");
        let deserialized =
            Element::deserialize(&serialized, grove_version).expect("deserialize element");
        assert_eq!(deserialized, element);

        let explicit_cost = element.value_defined_cost(grove_version).unwrap();
        let derived_cost =
            Element::value_defined_cost_for_serialized_value(&serialized, grove_version)
                .expect("cost for serialized element");
        match (explicit_cost, derived_cost) {
            (SpecializedValueDefinedCost(explicit), SpecializedValueDefinedCost(derived)) => {
                assert!(explicit > 0);
                assert_eq!(explicit, derived);
            }
            _ => panic!("unexpected cost type"),
        }
    }
}
