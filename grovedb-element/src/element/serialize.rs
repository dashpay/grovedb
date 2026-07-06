//! Serialize
//! Implements serialization functions in Element

use bincode::config;
use grovedb_version::{check_grovedb_v0, version::GroveVersion};

use crate::{
    element::Element,
    element_type::{
        NON_COUNTED_WRAPPER_DISCRIMINANT, NOT_COUNTED_OR_SUMMED_WRAPPER_DISCRIMINANT,
        NOT_SUMMED_WRAPPER_DISCRIMINANT,
    },
    error::ElementError,
};

impl Element {
    /// Serializes self. Returns vector of u8s.
    ///
    /// Rejects:
    /// - Any wrapper nesting in any combination — `NonCounted`, `NotSummed`,
    ///   and `NotCountedOrSummed` are mutually exclusive.
    /// - `NotSummed(x)` / `NotCountedOrSummed(x)` where `x` is not one of
    ///   the six sum-bearing tree variants (`SumTree`, `BigSumTree`,
    ///   `CountSumTree`, `ProvableCountSumTree`, `ProvableSumTree`,
    ///   `ProvableCountProvableSumTree`).
    ///
    /// Constructed via the `new_non_counted` / `new_not_summed` /
    /// `new_not_counted_or_summed` constructors these are impossible, but a
    /// caller could build them directly.
    pub fn serialize(&self, grove_version: &GroveVersion) -> Result<Vec<u8>, ElementError> {
        // AUDIT NOTE (issue #717 — intentional, do not re-flag): the
        // `element.serialize` feature version is `0` on every protocol version.
        // Element bincode encoding is protocol-independent (append-only
        // discriminants), so newer variants encode identically across versions
        // — the constant is a cost selector, not a wire-format gate. See the
        // doc on `GroveDBElementMethodVersions::serialize` for the full
        // rationale.
        check_grovedb_v0!(
            "Element::serialize",
            grove_version.grovedb_versions.element.serialize
        );
        if let Element::NonCounted(inner) = self
            && matches!(
                **inner,
                Element::NonCounted(_) | Element::NotSummed(_) | Element::NotCountedOrSummed(_)
            )
        {
            return Err(ElementError::CorruptedData(
                "NonCounted cannot wrap another wrapper".to_string(),
            ));
        }
        if let Element::NotSummed(inner) = self {
            match **inner {
                Element::SumTree(..)
                | Element::BigSumTree(..)
                | Element::CountSumTree(..)
                | Element::ProvableCountSumTree(..)
                | Element::ProvableSumTree(..)
                | Element::ProvableCountProvableSumTree(..) => {}
                _ => {
                    return Err(ElementError::CorruptedData(
                        "NotSummed inner must be a sum-tree variant".to_string(),
                    ));
                }
            }
        }
        if let Element::NotCountedOrSummed(inner) = self {
            match **inner {
                Element::SumTree(..)
                | Element::BigSumTree(..)
                | Element::CountSumTree(..)
                | Element::ProvableCountSumTree(..)
                | Element::ProvableSumTree(..)
                | Element::ProvableCountProvableSumTree(..) => {}
                _ => {
                    return Err(ElementError::CorruptedData(
                        "NotCountedOrSummed inner must be a sum-bearing tree variant".to_string(),
                    ));
                }
            }
        }
        let config = config::standard().with_big_endian().with_no_limit();
        bincode::encode_to_vec(self, config)
            .map_err(|e| ElementError::CorruptedData(format!("unable to serialize element {}", e)))
    }

    /// Serializes self. Returns usize.
    pub fn serialized_size(&self, grove_version: &GroveVersion) -> Result<usize, ElementError> {
        check_grovedb_v0!(
            "Element::serialized_size",
            grove_version.grovedb_versions.element.serialized_size
        );
        self.serialize(grove_version)
            .map(|serialized| serialized.len())
    }

    /// Deserializes given bytes and sets as self.
    ///
    /// Pre-checks the leading bytes for any wrapper-byte combination and
    /// rejects before invoking bincode. This closes a stack-exhaustion
    /// vector: a hostile payload of repeated wrapper bytes would otherwise
    /// cause bincode to recursively decode the `Box<Element>` chain (and
    /// overflow the stack) before any post-decode check could fire. The
    /// pre-check is O(1) — only the first two bytes matter.
    ///
    /// The rejected leading byte pairs are every ordered combination of
    /// the three wrapper discriminants (15 NonCounted, 16 NotSummed,
    /// 17 NotCountedOrSummed) — nine in total.
    pub fn deserialize(bytes: &[u8], grove_version: &GroveVersion) -> Result<Self, ElementError> {
        check_grovedb_v0!(
            "Element::deserialize",
            grove_version.grovedb_versions.element.deserialize
        );
        // Pre-check: if the wire starts with a wrapper discriminant, the
        // very next byte must NOT be ANY wrapper discriminant. This bounds
        // the recursion bincode will attempt.
        if let [outer, inner, ..] = bytes
            && matches!(
                *outer,
                NON_COUNTED_WRAPPER_DISCRIMINANT
                    | NOT_SUMMED_WRAPPER_DISCRIMINANT
                    | NOT_COUNTED_OR_SUMMED_WRAPPER_DISCRIMINANT
            )
            && matches!(
                *inner,
                NON_COUNTED_WRAPPER_DISCRIMINANT
                    | NOT_SUMMED_WRAPPER_DISCRIMINANT
                    | NOT_COUNTED_OR_SUMMED_WRAPPER_DISCRIMINANT
            )
        {
            return Err(ElementError::CorruptedData(
                "deserialized wrapper wrapping another wrapper".to_string(),
            ));
        }
        let config = config::standard().with_big_endian().with_no_limit();
        let (elem, consumed): (Element, usize) = bincode::decode_from_slice(bytes, config)
            .map_err(|e| {
                ElementError::CorruptedData(format!("unable to deserialize element {}", e))
            })?;
        if consumed != bytes.len() {
            return Err(ElementError::CorruptedData(format!(
                "element deserialization did not consume all bytes: consumed {}, total {}",
                consumed,
                bytes.len()
            )));
        }
        // Defensive belt-and-braces post-check (guards against future
        // bincode/discriminant changes that could let a nested wrapper
        // sneak past the pre-check).
        if let Element::NonCounted(inner) = &elem
            && matches!(
                **inner,
                Element::NonCounted(_) | Element::NotSummed(_) | Element::NotCountedOrSummed(_)
            )
        {
            return Err(ElementError::CorruptedData(
                "deserialized NonCounted wrapping another wrapper".to_string(),
            ));
        }
        if let Element::NotSummed(inner) = &elem {
            match **inner {
                Element::SumTree(..)
                | Element::BigSumTree(..)
                | Element::CountSumTree(..)
                | Element::ProvableCountSumTree(..)
                | Element::ProvableSumTree(..)
                | Element::ProvableCountProvableSumTree(..) => {}
                _ => {
                    return Err(ElementError::CorruptedData(
                        "deserialized NotSummed with non-sum-tree inner".to_string(),
                    ));
                }
            }
        }
        if let Element::NotCountedOrSummed(inner) = &elem {
            match **inner {
                Element::SumTree(..)
                | Element::BigSumTree(..)
                | Element::CountSumTree(..)
                | Element::ProvableCountSumTree(..)
                | Element::ProvableSumTree(..)
                | Element::ProvableCountProvableSumTree(..) => {}
                _ => {
                    return Err(ElementError::CorruptedData(
                        "deserialized NotCountedOrSummed with non-sum-bearing-tree inner"
                            .to_string(),
                    ));
                }
            }
        }
        Ok(elem)
    }
}

#[cfg(test)]
mod tests {
    use integer_encoding::VarInt;

    use super::*;
    use crate::reference_path::ReferencePathType;

    #[test]
    fn test_serialization() {
        let grove_version = GroveVersion::latest();
        let empty_tree = Element::empty_tree();
        let serialized = empty_tree
            .serialize(grove_version)
            .expect("expected to serialize");
        assert_eq!(serialized.len(), 3);
        assert_eq!(
            serialized.len(),
            empty_tree.serialized_size(grove_version).unwrap()
        );
        // The tree is fixed length 32 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(hex::encode(serialized), "020000");

        let empty_tree = Element::new_tree_with_flags(None, Some(vec![5]));
        let serialized = empty_tree
            .serialize(grove_version)
            .expect("expected to serialize");
        assert_eq!(serialized.len(), 5);
        assert_eq!(
            serialized.len(),
            empty_tree.serialized_size(grove_version).unwrap()
        );
        assert_eq!(hex::encode(serialized), "0200010105");

        let item = Element::new_item(hex::decode("abcdef").expect("expected to decode"));
        let serialized = item
            .serialize(grove_version)
            .expect("expected to serialize");
        assert_eq!(serialized.len(), 6);
        assert_eq!(
            serialized.len(),
            item.serialized_size(grove_version).unwrap()
        );
        // The item is variable length 3 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(hex::encode(serialized), "0003abcdef00");

        assert_eq!(hex::encode(5.encode_var_vec()), "0a");

        let item = Element::new_sum_item(5);
        let serialized = item
            .serialize(grove_version)
            .expect("expected to serialize");
        assert_eq!(serialized.len(), 3);
        assert_eq!(
            serialized.len(),
            item.serialized_size(grove_version).unwrap()
        );
        // The item is variable length 3 bytes, so it's enum 2 then 32 bytes of zeroes
        assert_eq!(hex::encode(serialized), "030a00");

        let item = Element::new_item_with_sum_item("abc".as_bytes().to_vec(), 7);
        let serialized = item
            .serialize(grove_version)
            .expect("expected to serialize ItemWithSumItem");
        assert_eq!(
            Element::deserialize(&serialized, grove_version)
                .expect("should deserialize ItemWithSumItem"),
            item
        );

        let item = Element::new_item_with_sum_item_with_flags(
            hex::decode("abcd").expect("expected to decode"),
            -3,
            Some(vec![9, 8, 7]),
        );
        let serialized = item
            .serialize(grove_version)
            .expect("expected to serialize ItemWithSumItem");
        assert_eq!(
            Element::deserialize(&serialized, grove_version)
                .expect("should deserialize ItemWithSumItem"),
            item
        );

        let item = Element::new_item_with_flags(
            hex::decode("abcdef").expect("expected to decode"),
            Some(vec![1]),
        );
        let serialized = item
            .serialize(grove_version)
            .expect("expected to serialize");
        assert_eq!(serialized.len(), 8);
        assert_eq!(
            serialized.len(),
            item.serialized_size(grove_version).unwrap()
        );
        assert_eq!(hex::encode(serialized), "0003abcdef010101");

        let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            vec![0],
            hex::decode("abcd").expect("expected to decode"),
            vec![5],
        ]));
        let serialized = reference
            .serialize(grove_version)
            .expect("expected to serialize");
        assert_eq!(serialized.len(), 12);
        assert_eq!(
            serialized.len(),
            reference.serialized_size(grove_version).unwrap()
        );
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
        let serialized = reference
            .serialize(grove_version)
            .expect("expected to serialize");
        assert_eq!(serialized.len(), 16);
        assert_eq!(
            serialized.len(),
            reference.serialized_size(grove_version).unwrap()
        );
        assert_eq!(hex::encode(serialized), "010003010002abcd0105000103010203");
    }

    #[test]
    fn deserialize_rejects_trailing_bytes() {
        let grove_version = GroveVersion::latest();
        let elements = [
            Element::new_item(b"abc".to_vec()),
            Element::empty_tree(),
            Element::new_sum_item(5),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                b"root".to_vec(),
                b"leaf".to_vec(),
            ])),
        ];

        for element in elements {
            let mut serialized = element.serialize(grove_version).expect("serialize element");
            serialized.push(0xff);

            let err = Element::deserialize(&serialized, grove_version)
                .expect_err("trailing bytes must be rejected");
            assert!(
                matches!(&err, ElementError::CorruptedData(message) if message.contains("did not consume all bytes")),
                "unexpected error: {err:?}"
            );
        }
    }
}
