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
    /// Counted Merk Tree Node
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

    /// Force the count component of this feature type to 0, leaving sum
    /// components untouched. No-op for variants that don't carry a count.
    ///
    /// Used by the `Element::NonCounted` wrapper: when computing the parent
    /// tree's feature type for a non-counted child, we use the inner element's
    /// feature type but zero out its count so the parent's aggregate
    /// excludes it.
    pub fn zero_count(&mut self) {
        match self {
            CountedMerkNode(count) | ProvableCountedMerkNode(count) => *count = 0,
            CountedSummedMerkNode(count, _) | ProvableCountedSummedMerkNode(count, _) => *count = 0,
            BasicMerkNode | SummedMerkNode(_) | BigSummedMerkNode(_) => {}
        }
    }

    /// Force the sum component of this feature type to 0, leaving count
    /// components untouched. No-op for variants that don't carry a sum.
    ///
    /// Used by the `Element::NotSummed` wrapper: when computing the parent
    /// tree's feature type for a not-summed child, we use the inner sum-tree
    /// element's feature type but zero out its sum so the parent's
    /// aggregate excludes it.
    pub fn zero_sum(&mut self) {
        match self {
            SummedMerkNode(sum) => *sum = 0,
            BigSummedMerkNode(sum) => *sum = 0,
            CountedSummedMerkNode(_, sum) | ProvableCountedSummedMerkNode(_, sum) => *sum = 0,
            BasicMerkNode | CountedMerkNode(_) | ProvableCountedMerkNode(_) => {}
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
            [b] => Err(ed::Error::UnexpectedByte(b)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_count_only_zeros_count() {
        let mut basic = BasicMerkNode;
        basic.zero_count();
        assert_eq!(basic, BasicMerkNode);

        let mut summed = SummedMerkNode(42);
        summed.zero_count();
        assert_eq!(summed, SummedMerkNode(42));

        let mut big_summed = BigSummedMerkNode(42);
        big_summed.zero_count();
        assert_eq!(big_summed, BigSummedMerkNode(42));

        let mut counted = CountedMerkNode(7);
        counted.zero_count();
        assert_eq!(counted, CountedMerkNode(0));

        let mut count_sum = CountedSummedMerkNode(7, 42);
        count_sum.zero_count();
        assert_eq!(count_sum, CountedSummedMerkNode(0, 42));

        let mut prov_counted = ProvableCountedMerkNode(7);
        prov_counted.zero_count();
        assert_eq!(prov_counted, ProvableCountedMerkNode(0));

        let mut prov_count_sum = ProvableCountedSummedMerkNode(7, 42);
        prov_count_sum.zero_count();
        assert_eq!(prov_count_sum, ProvableCountedSummedMerkNode(0, 42));
    }

    #[test]
    fn zero_sum_only_zeros_sum() {
        let mut basic = BasicMerkNode;
        basic.zero_sum();
        assert_eq!(basic, BasicMerkNode);

        let mut counted = CountedMerkNode(7);
        counted.zero_sum();
        assert_eq!(counted, CountedMerkNode(7));

        let mut prov_counted = ProvableCountedMerkNode(7);
        prov_counted.zero_sum();
        assert_eq!(prov_counted, ProvableCountedMerkNode(7));

        let mut summed = SummedMerkNode(42);
        summed.zero_sum();
        assert_eq!(summed, SummedMerkNode(0));

        let mut big_summed = BigSummedMerkNode(42);
        big_summed.zero_sum();
        assert_eq!(big_summed, BigSummedMerkNode(0));

        let mut count_sum = CountedSummedMerkNode(7, 42);
        count_sum.zero_sum();
        assert_eq!(count_sum, CountedSummedMerkNode(7, 0));

        let mut prov_count_sum = ProvableCountedSummedMerkNode(7, 42);
        prov_count_sum.zero_sum();
        assert_eq!(prov_count_sum, ProvableCountedSummedMerkNode(7, 0));
    }

    #[test]
    fn count_helper_returns_some_only_for_count_bearing() {
        assert_eq!(BasicMerkNode.count(), None);
        assert_eq!(SummedMerkNode(42).count(), None);
        assert_eq!(BigSummedMerkNode(42).count(), None);
        assert_eq!(CountedMerkNode(7).count(), Some(7));
        assert_eq!(CountedSummedMerkNode(7, 42).count(), Some(7));
        assert_eq!(ProvableCountedMerkNode(7).count(), Some(7));
        assert_eq!(ProvableCountedSummedMerkNode(7, 42).count(), Some(7));
    }
}
