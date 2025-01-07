//! Merk tree feature type

#[cfg(any(feature = "full", feature = "verify"))]
use std::io::{Read, Write};
use byteorder::{BigEndian, ByteOrder, LittleEndian, ReadBytesExt, WriteBytesExt};
#[cfg(feature = "full")]
use ed::Terminated;
#[cfg(any(feature = "full", feature = "verify"))]
use ed::{Decode, Encode};
#[cfg(any(feature = "full", feature = "verify"))]
use integer_encoding::{VarInt, VarIntReader, VarIntWriter};

#[cfg(any(feature = "full", feature = "verify"))]
use crate::tree::tree_feature_type::TreeFeatureType::{BasicMerkNode, SummedMerkNode};
use crate::TreeFeatureType::{BigSummedMerkNode, CountedMerkNode};

#[cfg(any(feature = "full", feature = "verify"))]
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
}

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AggregateData {
    NoAggregateData,
    Sum(i64),
    BigSum(i128),
    Count(u64),
}

impl From<TreeFeatureType> for AggregateData {
    fn from(value: TreeFeatureType) -> Self {
        match value {
            BasicMerkNode => AggregateData::NoAggregateData,
            SummedMerkNode(val) => AggregateData::Sum(val),
            BigSummedMerkNode(val) => AggregateData::BigSum(val),
            CountedMerkNode(val) => AggregateData::Count(val),
        }
    }
}

#[cfg(feature = "full")]
impl TreeFeatureType {
    #[inline]
    /// Get length of encoded SummedMerk
    pub fn sum_length(&self) -> Option<u32> {
        match self {
            BasicMerkNode => None,
            SummedMerkNode(m) => Some(m.encode_var_vec().len() as u32),
            BigSummedMerkNode(_) => Some(16),
            CountedMerkNode(m) => Some(m.encode_var_vec().len() as u32),
        }
    }

    #[inline]
    /// Is sum feature?
    pub fn is_sum_feature(&self) -> bool {
        matches!(self, SummedMerkNode(_))
    }

    #[inline]
    /// Get encoding cost of self
    pub(crate) fn encoding_cost(&self) -> usize {
        match self {
            BasicMerkNode => 1,
            SummedMerkNode(_sum) => 9,
            BigSummedMerkNode(_) => 17,
            CountedMerkNode(_) => 9,
        }
    }
}

#[cfg(feature = "full")]
impl Terminated for TreeFeatureType {}

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
        }
    }

    #[inline]
    fn encoding_length(&self) -> ed::Result<usize> {
        match self {
            BasicMerkNode => Ok(1),
            SummedMerkNode(sum) => {
                let encoded_sum = sum.encode_var_vec();
                // 1 for the enum type
                // encoded_sum.len() for the length of the encoded vector
                Ok(1 + encoded_sum.len())
            }
            BigSummedMerkNode(_) => {
                Ok(17)
            }
            CountedMerkNode(count) => {
                let encoded_sum = count.encode_var_vec();
                // 1 for the enum type
                // encoded_sum.len() for the length of the encoded vector
                Ok(1 + encoded_sum.len())
            }
        }
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
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
            _ => Err(ed::Error::UnexpectedByte(55)),
        }
    }
}
