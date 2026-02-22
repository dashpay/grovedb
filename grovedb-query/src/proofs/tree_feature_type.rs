//! Tree feature type for Merk nodes

use std::io::{Read, Write};

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use ed::{Decode, Encode, Terminated};
#[cfg(feature = "blockchain")]
use grovedb_costs::TreeCostType;
use integer_encoding::{VarInt, VarIntReader, VarIntWriter};

use self::TreeFeatureType::{
    BasicMerkNode, BigSummedMerkNode, CountedMerkNode, CountedSummedMerkNode,
    ProvableCountedMerkNode, SummedMerkNode,
};
use crate::proofs::TreeFeatureType::ProvableCountedSummedMerkNode;

/// Node type classification for Merk tree nodes

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum NodeType {
    /// Normal node (no aggregation)
    NormalNode,
    /// Sum node (i64 sum)
    SumNode,
    /// Big sum node (i128 sum)
    BigSumNode,
    /// Count node (u64 count)
    CountNode,
    /// Count + sum node
    CountSumNode,
    /// Provable count node (count included in hash)
    ProvableCountNode,
    /// Provable count + sum node (count included in hash)
    ProvableCountSumNode,
}

impl NodeType {
    /// The byte length of the feature data for this node type
    pub const fn feature_len(&self) -> u32 {
        match self {
            NodeType::NormalNode => 1,
            NodeType::SumNode => 9,
            NodeType::BigSumNode => 17,
            NodeType::CountNode => 9,
            NodeType::CountSumNode => 17,
            NodeType::ProvableCountNode => 9,
            NodeType::ProvableCountSumNode => 17,
        }
    }

    /// The cost in bytes of the feature data (excluding the 1-byte tag)
    pub const fn cost(&self) -> u32 {
        match self {
            NodeType::NormalNode => 0,
            NodeType::SumNode => 8,
            NodeType::BigSumNode => 16,
            NodeType::CountNode => 8,
            NodeType::CountSumNode => 16,
            NodeType::ProvableCountNode => 8,
            NodeType::ProvableCountSumNode => 16,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
/// Basic or summed
pub enum TreeFeatureType {
    /// Basic Merk Tree Node
    BasicMerkNode,
    /// Summed Merk Tree Node
    SummedMerkNode(i64),
    /// Big Summed Merk Tree Node
    BigSummedMerkNode(i128),
    /// Counted Merk Tree None
    CountedMerkNode(u64),
    /// Counted and summed Merk Tree None
    CountedSummedMerkNode(u64, i64),
    /// Provable Counted Merk Tree Node
    ProvableCountedMerkNode(u64),
    /// Provable Counted and Summed Merk Tree Node (count in hash, sum tracked)
    ProvableCountedSummedMerkNode(u64, i64),
}

impl TreeFeatureType {
    /// Returns the count of elements in this subtree, if available.
    /// Returns Some(count) for CountedMerkNode, ProvableCountedMerkNode,
    /// CountedSummedMerkNode, and ProvableCountedSummedMerkNode variants.
    /// Returns None for BasicMerkNode, SummedMerkNode, BigSummedMerkNode.
    pub fn count(&self) -> Option<u64> {
        match self {
            CountedMerkNode(count)
            | ProvableCountedMerkNode(count)
            | CountedSummedMerkNode(count, _)
            | ProvableCountedSummedMerkNode(count, _) => Some(*count),
            BasicMerkNode | SummedMerkNode(_) | BigSummedMerkNode(_) => None,
        }
    }

    /// Get the NodeType for this feature type
    pub fn node_type(&self) -> NodeType {
        match self {
            BasicMerkNode => NodeType::NormalNode,
            SummedMerkNode(_) => NodeType::SumNode,
            BigSummedMerkNode(_) => NodeType::BigSumNode,
            CountedMerkNode(_) => NodeType::CountNode,
            CountedSummedMerkNode(..) => NodeType::CountSumNode,
            ProvableCountedMerkNode(_) => NodeType::ProvableCountNode,
            ProvableCountedSummedMerkNode(..) => NodeType::ProvableCountSumNode,
        }
    }

    /// Get encoding cost of self
    #[inline]
    pub fn encoding_cost(&self) -> usize {
        match self {
            BasicMerkNode => 1,
            SummedMerkNode(_) => 9,
            BigSummedMerkNode(_) => 17,
            CountedMerkNode(_) => 9,
            CountedSummedMerkNode(..) => 17,
            ProvableCountedMerkNode(_) => 9,
            ProvableCountedSummedMerkNode(..) => 17,
        }
    }
}

/// Methods that depend on grovedb-costs (behind `blockchain` feature)
#[cfg(feature = "blockchain")]
impl TreeFeatureType {
    /// Get length of encoded feature type with TreeCostType
    #[inline]
    pub fn tree_feature_specialized_type_and_length(&self) -> Option<(TreeCostType, u32)> {
        match self {
            BasicMerkNode => None,
            SummedMerkNode(m) => Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
                m.encode_var_vec().len() as u32,
            )),
            BigSummedMerkNode(_) => Some((TreeCostType::TreeFeatureUses16Bytes, 16)),
            CountedMerkNode(m) => Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
                m.encode_var_vec().len() as u32,
            )),
            CountedSummedMerkNode(count, sum) => Some((
                TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes,
                count.encode_var_vec().len() as u32 + sum.encode_var_vec().len() as u32,
            )),
            ProvableCountedMerkNode(m) => Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
                m.encode_var_vec().len() as u32,
            )),
            ProvableCountedSummedMerkNode(count, sum) => Some((
                TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes,
                count.encode_var_vec().len() as u32 + sum.encode_var_vec().len() as u32,
            )),
        }
    }
}

impl Encode for TreeFeatureType {
    #[inline]
    fn encode_into<W: Write>(&self, dest: &mut W) -> ed::Result<()> {
        match self {
            BasicMerkNode => {
                dest.write_all(&[0])?;
                Ok(())
            }
            SummedMerkNode(sum) => {
                dest.write_all(&[1])?;
                dest.write_varint(*sum)?;
                Ok(())
            }
            BigSummedMerkNode(sum) => {
                dest.write_all(&[2])?;
                dest.write_i128::<BigEndian>(*sum)?;
                Ok(())
            }
            CountedMerkNode(count) => {
                dest.write_all(&[3])?;
                dest.write_varint(*count)?;
                Ok(())
            }
            CountedSummedMerkNode(count, sum) => {
                dest.write_all(&[4])?;
                dest.write_varint(*count)?;
                dest.write_varint(*sum)?;
                Ok(())
            }
            ProvableCountedMerkNode(count) => {
                dest.write_all(&[5])?;
                dest.write_varint(*count)?;
                Ok(())
            }
            ProvableCountedSummedMerkNode(count, sum) => {
                dest.write_all(&[6])?;
                dest.write_varint(*count)?;
                dest.write_varint(*sum)?;
                Ok(())
            }
        }
    }

    #[inline]
    fn encoding_length(&self) -> ed::Result<usize> {
        match self {
            BasicMerkNode => Ok(1),
            SummedMerkNode(sum) => {
                let encoded_sum = sum.encode_var_vec();
                Ok(1 + encoded_sum.len())
            }
            BigSummedMerkNode(_) => Ok(17),
            CountedMerkNode(count) => {
                let encoded_sum = count.encode_var_vec();
                Ok(1 + encoded_sum.len())
            }
            CountedSummedMerkNode(count, sum) => {
                let encoded_lengths = count.encode_var_vec().len() + sum.encode_var_vec().len();
                Ok(1 + encoded_lengths)
            }
            ProvableCountedMerkNode(count) => {
                let encoded_sum = count.encode_var_vec();
                Ok(1 + encoded_sum.len())
            }
            ProvableCountedSummedMerkNode(count, sum) => {
                let encoded_lengths = count.encode_var_vec().len() + sum.encode_var_vec().len();
                Ok(1 + encoded_lengths)
            }
        }
    }
}

impl Terminated for TreeFeatureType {}

impl Decode for TreeFeatureType {
    #[inline]
    fn decode<R: Read>(mut input: R) -> ed::Result<Self> {
        let mut feature_type: [u8; 1] = [0];
        input.read_exact(&mut feature_type)?;
        match feature_type {
            [0] => Ok(BasicMerkNode),
            [1] => {
                let encoded_sum: i64 = input.read_varint()?;
                Ok(SummedMerkNode(encoded_sum))
            }
            [2] => {
                let encoded_sum: i128 = input.read_i128::<BigEndian>()?;
                Ok(BigSummedMerkNode(encoded_sum))
            }
            [3] => {
                let encoded_count: u64 = input.read_varint()?;
                Ok(CountedMerkNode(encoded_count))
            }
            [4] => {
                let encoded_count: u64 = input.read_varint()?;
                let encoded_sum: i64 = input.read_varint()?;
                Ok(CountedSummedMerkNode(encoded_count, encoded_sum))
            }
            [5] => {
                let encoded_count: u64 = input.read_varint()?;
                Ok(ProvableCountedMerkNode(encoded_count))
            }
            [6] => {
                let encoded_count: u64 = input.read_varint()?;
                let encoded_sum: i64 = input.read_varint()?;
                Ok(ProvableCountedSummedMerkNode(encoded_count, encoded_sum))
            }
            _ => Err(ed::Error::UnexpectedByte(55)),
        }
    }
}
