#[cfg(feature = "full")]
use integer_encoding::VarInt;
#[cfg(feature = "full")]
use merk::{
    tree::{kv::KV, Tree},
    TreeFeatureType,
    TreeFeatureType::{BasicMerk, SummedMerk},
};

#[cfg(feature = "full")]
use crate::{
    element::{SUM_TREE_COST_SIZE, TREE_COST_SIZE},
    reference_path::{path_from_reference_path_type, ReferencePathType},
    Element, ElementFlags, Error,
};

impl Element {
    #[cfg(feature = "full")]
    /// Decoded the integer value in the SumItem element type, returns 0 for
    /// everything else
    pub fn sum_value(&self) -> Option<i64> {
        match self {
            Element::SumItem(sum_value, _) | Element::SumTree(_, sum_value, _) => Some(*sum_value),
            _ => Some(0),
        }
    }

    #[cfg(feature = "full")]
    pub fn is_sum_tree(&self) -> bool {
        matches!(self, Element::SumTree(..))
    }

    #[cfg(feature = "full")]
    pub fn is_tree(&self) -> bool {
        matches!(self, Element::SumTree(..) | Element::Tree(..))
    }

    #[cfg(feature = "full")]
    pub fn is_sum_item(&self) -> bool {
        matches!(self, Element::SumItem(..))
    }

    #[cfg(feature = "full")]
    pub fn get_feature_type(&self, parent_is_sum_tree: bool) -> Result<TreeFeatureType, Error> {
        match parent_is_sum_tree {
            true => {
                let sum_value = self.sum_value();
                match sum_value {
                    Some(sum) => Ok(SummedMerk(sum)),
                    None => Err(Error::CorruptedData(String::from(
                        "cannot decode sum item to i64",
                    ))),
                }
            }
            false => Ok(BasicMerk),
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
    /// Get the size of an element in bytes
    #[deprecated]
    pub fn byte_size(&self) -> u32 {
        match self {
            Element::Item(item, element_flag) => {
                if let Some(flag) = element_flag {
                    flag.len() as u32 + item.len() as u32
                } else {
                    item.len() as u32
                }
            }
            Element::SumItem(item, element_flag) => {
                if let Some(flag) = element_flag {
                    flag.len() as u32 + item.required_space() as u32
                } else {
                    item.required_space() as u32
                }
            }
            Element::Reference(path_reference, _, element_flag) => {
                let path_length = path_reference.serialized_size() as u32;

                if let Some(flag) = element_flag {
                    flag.len() as u32 + path_length
                } else {
                    path_length
                }
            }
            Element::Tree(_, element_flag) => {
                if let Some(flag) = element_flag {
                    flag.len() as u32 + 32
                } else {
                    32
                }
            }
            Element::SumTree(_, _, element_flag) => {
                if let Some(flag) = element_flag {
                    flag.len() as u32 + 32 + 8
                } else {
                    32 + 8
                }
            }
        }
    }

    #[cfg(feature = "full")]
    pub fn required_item_space(len: u32, flag_len: u32) -> u32 {
        len + len.required_space() as u32 + flag_len + flag_len.required_space() as u32 + 1
    }

    #[cfg(feature = "full")]
    pub(crate) fn convert_if_reference_to_absolute_reference(
        self,
        path: &[&[u8]],
        key: Option<&[u8]>,
    ) -> Result<Element, Error> {
        // Convert any non absolute reference type to an absolute one
        // we do this here because references are aggregated first then followed later
        // to follow non absolute references, we need the path they are stored at
        // this information is lost during the aggregation phase.
        Ok(match &self {
            Element::Reference(reference_path_type, ..) => match reference_path_type {
                ReferencePathType::AbsolutePathReference(..) => self,
                _ => {
                    // Element is a reference and is not absolute.
                    // build the stored path for this reference
                    let current_path = <&[&[u8]]>::clone(&path).to_vec();
                    let absolute_path = path_from_reference_path_type(
                        reference_path_type.clone(),
                        current_path,
                        key,
                    )?;
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
    pub fn tree_costs_for_key_value(
        key: &Vec<u8>,
        value: &[u8],
        is_sum_node: bool,
    ) -> Result<u32, Error> {
        let element = Element::deserialize(value)?;
        match element {
            Element::Tree(_, flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = TREE_COST_SIZE + flags_len;
                let key_len = key.len() as u32;
                Ok(KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key_len,
                    value_len,
                    is_sum_node,
                ))
            }
            Element::SumTree(_, _sum_value, flags) => {
                let flags_len = flags.map_or(0, |flags| {
                    let flags_len = flags.len() as u32;
                    flags_len + flags_len.required_space() as u32
                });
                let value_len = TREE_COST_SIZE + flags_len + 8;
                let key_len = key.len() as u32;
                Ok(KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    key_len,
                    value_len,
                    is_sum_node,
                ))
            }
            _ => Err(Error::CorruptedCodeExecution(
                "only trees are supported for specialized costs",
            )),
        }
    }

    #[cfg(feature = "full")]
    pub fn get_tree_cost(&self) -> Result<u32, Error> {
        match self {
            Element::Tree(..) => Ok(TREE_COST_SIZE),
            Element::SumTree(..) => Ok(SUM_TREE_COST_SIZE),
            _ => Err(Error::CorruptedCodeExecution(
                "trying to get tree cost from non tree element",
            )),
        }
    }
}

#[cfg(feature = "full")]
pub fn raw_decode(bytes: &[u8]) -> Result<Element, Error> {
    let tree = Tree::decode_raw(bytes, vec![]).map_err(|e| Error::CorruptedData(e.to_string()))?;
    let element: Element = Element::deserialize(tree.value_as_slice())?;
    Ok(element)
}
