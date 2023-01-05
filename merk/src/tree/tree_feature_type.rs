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

//! Merk tree feature type

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
/// Basic or summed
pub enum TreeFeatureType {
    BasicMerk,
    SummedMerk(i64),
}

#[cfg(feature = "full")]
impl TreeFeatureType {
    #[inline]
    /// Get length of encoded SummedMerk
    pub fn sum_length(&self) -> Option<u32> {
        match self {
            BasicMerk => None,
            SummedMerk(m) => Some(m.encode_var_vec().len() as u32),
        }
    }

    #[inline]
    /// Is sum feature?
    pub fn is_sum_feature(&self) -> bool {
        matches!(self, SummedMerk(_))
    }

    #[inline]
    /// Get encoding cost of self
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
