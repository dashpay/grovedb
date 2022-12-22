#[cfg(any(feature = "full", feature = "verify"))]
use std::io::{Read, Write};

#[cfg(feature = "full")]
use ed::Terminated;
#[cfg(any(feature = "full", feature = "verify"))]
use ed::{Decode, Encode};
#[cfg(any(feature = "full", feature = "verify"))]
use integer_encoding::{VarInt, VarIntReader, VarIntWriter};

#[cfg(any(feature = "full", feature = "verify"))]
use crate::tree::tree_feature_type::TreeFeatureType::{BasicMerk, SummedMerk};

#[cfg(any(feature = "full", feature = "verify"))]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TreeFeatureType {
    BasicMerk,
    SummedMerk(i64),
}

#[cfg(feature = "full")]
impl TreeFeatureType {
    #[inline]
    pub fn sum_length(&self) -> Option<u32> {
        match self {
            BasicMerk => None,
            SummedMerk(m) => Some(m.encode_var_vec().len() as u32),
        }
    }

    #[inline]
    pub fn is_sum_feature(&self) -> bool {
        matches!(self, SummedMerk(_))
    }

    #[inline]
    pub(crate) fn encoding_cost(&self) -> usize {
        match self {
            BasicMerk => 1,
            SummedMerk(_sum) => 9,
        }
    }
}

#[cfg(feature = "full")]
impl Terminated for TreeFeatureType {}

impl Encode for TreeFeatureType {
    #[inline]
    fn encode_into<W: Write>(&self, dest: &mut W) -> ed::Result<()> {
        match self {
            BasicMerk => {
                dest.write_all(&[0])?;
                Ok(())
            }
            SummedMerk(sum) => {
                dest.write_all(&[1])?;
                dest.write_varint(sum.to_owned())?;
                Ok(())
            }
        }
    }

    #[inline]
    fn encoding_length(&self) -> ed::Result<usize> {
        match self {
            BasicMerk => Ok(1),
            SummedMerk(sum) => {
                let encoded_sum = sum.encode_var_vec();
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
            [0] => Ok(BasicMerk),
            [1] => {
                let encoded_sum: i64 = input.read_varint()?;
                Ok(SummedMerk(encoded_sum))
            }
            _ => Err(ed::Error::UnexpectedByte(55)),
        }
    }
}
