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

//! Serialize
//! Implements serialization functions in Element

#[cfg(any(feature = "full", feature = "verify"))]
use bincode::Options;

#[cfg(any(feature = "full", feature = "verify"))]
use crate::{Element, Error};

impl Element {
    #[cfg(feature = "full")]
    /// Serializes self. Returns vector of u8s.
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        bincode::DefaultOptions::default()
            .with_varint_encoding()
            .reject_trailing_bytes()
            .serialize(self)
            .map_err(|_| Error::CorruptedData(String::from("unable to serialize element")))
    }

    #[cfg(feature = "full")]
    /// Serializes self. Returns usize.
    pub fn serialized_size(&self) -> usize {
        bincode::DefaultOptions::default()
            .with_varint_encoding()
            .reject_trailing_bytes()
            .serialized_size(self)
            .unwrap() as usize // this should not be able to error
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    /// Deserializes given bytes and sets as self
    pub fn deserialize(bytes: &[u8]) -> Result<Self, Error> {
        bincode::DefaultOptions::default()
            .with_varint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use integer_encoding::VarInt;

    use super::*;
    use crate::reference_path::ReferencePathType;

    #[test]
    fn test_serialization() {
        let empty_tree = Element::empty_tree();
        let serialized = empty_tree.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 3);
        assert_eq!(serialized.len(), empty_tree.serialized_size());
        // The tree is fixed length 32 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(hex::encode(serialized), "020000");

        let empty_tree = Element::new_tree_with_flags(None, Some(vec![5]));
        let serialized = empty_tree.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 5);
        assert_eq!(serialized.len(), empty_tree.serialized_size());
        assert_eq!(hex::encode(serialized), "0200010105");

        let item = Element::new_item(hex::decode("abcdef").expect("expected to decode"));
        let serialized = item.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 6);
        assert_eq!(serialized.len(), item.serialized_size());
        // The item is variable length 3 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(hex::encode(serialized), "0003abcdef00");

        assert_eq!(hex::encode(5.encode_var_vec()), "0a");

        let item = Element::new_sum_item(5);
        let serialized = item.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 3);
        assert_eq!(serialized.len(), item.serialized_size());
        // The item is variable length 3 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(hex::encode(serialized), "030a00");

        let item = Element::new_item_with_flags(
            hex::decode("abcdef").expect("expected to decode"),
            Some(vec![1]),
        );
        let serialized = item.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 8);
        assert_eq!(serialized.len(), item.serialized_size());
        assert_eq!(hex::encode(serialized), "0003abcdef010101");

        let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            vec![0],
            hex::decode("abcd").expect("expected to decode"),
            vec![5],
        ]));
        let serialized = reference.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 12);
        assert_eq!(serialized.len(), reference.serialized_size());
        // The item is variable length 2 bytes, so it's enum 1 then 1 byte for length,
        // then 1 byte for 0, then 1 byte 02 for abcd, then 1 byte '1' for 05
        assert_eq!(hex::encode(serialized), "010003010002abcd01050000");

        let reference = Element::new_reference_with_flags(
            ReferencePathType::AbsolutePathReference(vec![
                vec![0],
                hex::decode("abcd").expect("expected to decode"),
                vec![5],
            ]),
            Some(vec![1, 2, 3]),
        );
        let serialized = reference.serialize().expect("expected to serialize");
        assert_eq!(serialized.len(), 16);
        assert_eq!(serialized.len(), reference.serialized_size());
        assert_eq!(hex::encode(serialized), "010003010002abcd0105000103010203");
    }
}
