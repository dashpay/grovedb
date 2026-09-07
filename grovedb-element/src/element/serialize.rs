//! Serialize
//! Implements serialization functions in Element

use bincode::config;
use grovedb_version::{check_grovedb_v0, version::GroveVersion};

use crate::{element::Element, error::ElementError};

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
        if let Element::NonCounted(inner) = self
            && matches!(
                **inner,
                Element::BidirectionalReference(..)
                    | Element::ItemWithBackwardsReferences(..)
                    | Element::SumItemWithBackwardsReferences(..)
                    | Element::ItemWithSumItemWithBackwardsReferences(..)
            )
        {
            return Err(ElementError::CorruptedData(
                "NonCounted cannot wrap backward-references elements".to_string(),
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
        // A PrivateDocumentStore's committed config is bound into its state
        // root; an unusable config must never reach disk. The checked
        // constructors and insert paths already enforce this — the codec
        // check closes the caller-built-element gap.
        if let Err(e) = self.validate_private_document_store_config() {
            return Err(ElementError::CorruptedData(format!(
                "invalid private document store config: {}",
                e
            )));
        }
        // The backward-references budgets bound worst-case propagation cost;
        // an over-limit list must never reach disk.
        if let Err(e) = self.validate_backward_references_limits() {
            return Err(ElementError::CorruptedData(format!(
                "invalid backward references: {}",
                e
            )));
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
    /// The manual bincode decoder validates wrapper nesting from decoded
    /// discriminants before descending and grows collections only after their
    /// encoded contents have been read.
    pub fn deserialize(bytes: &[u8], grove_version: &GroveVersion) -> Result<Self, ElementError> {
        check_grovedb_v0!(
            "Element::deserialize",
            grove_version.grovedb_versions.element.deserialize
        );
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
        // Keep this public-boundary validation so its error remains an
        // ElementError even if another decoder implementation is introduced.
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
        if let Element::NonCounted(inner) = &elem
            && matches!(
                **inner,
                Element::BidirectionalReference(..)
                    | Element::ItemWithBackwardsReferences(..)
                    | Element::SumItemWithBackwardsReferences(..)
                    | Element::ItemWithSumItemWithBackwardsReferences(..)
            )
        {
            return Err(ElementError::CorruptedData(
                "deserialized NonCounted wrapping a backward-references element".to_string(),
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
        // Reject a PrivateDocumentStore with an unusable committed config
        // (entry_size 0 or chunk_power outside 1..=16). No such bytes can
        // legitimately exist — serialization and every insert path enforce
        // the same bound — so this cannot reject previously-valid data;
        // it makes the invalid configuration unrepresentable, mirroring
        // the wrapper-invariant checks above.
        if let Err(e) = elem.validate_private_document_store_config() {
            return Err(ElementError::CorruptedData(format!(
                "deserialized private document store with invalid config: {}",
                e
            )));
        }
        if let Err(e) = elem.validate_backward_references_limits() {
            return Err(ElementError::CorruptedData(format!(
                "deserialized element with invalid backward references: {}",
                e
            )));
        }
        Ok(elem)
    }
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::GROVE_VERSIONS;
    use integer_encoding::VarInt;

    use super::*;
    use crate::reference_path::ReferencePathType;

    fn every_element_variant() -> Vec<Element> {
        let flags = || Some(Vec::new());
        vec![
            Element::Item(vec![1], flags()),
            Element::Reference(
                ReferencePathType::AbsolutePathReference(vec![vec![2]]),
                Some(3),
                flags(),
            ),
            Element::Tree(Some(vec![3]), flags()),
            Element::SumItem(-4, flags()),
            Element::SumTree(Some(vec![5]), -5, flags()),
            Element::BigSumTree(Some(vec![6]), -6, flags()),
            Element::CountTree(Some(vec![7]), 7, flags()),
            Element::CountSumTree(Some(vec![8]), 8, -8, flags()),
            Element::ProvableCountTree(Some(vec![9]), 9, flags()),
            Element::ItemWithSumItem(vec![10], -10, flags()),
            Element::ProvableCountSumTree(Some(vec![11]), 11, -11, flags()),
            Element::CommitmentTree(12, 4, flags()),
            Element::MmrTree(13, flags()),
            Element::BulkAppendTree(14, 4, flags()),
            Element::DenseAppendOnlyFixedSizeTree(15, 4, flags()),
            Element::NonCounted(Box::new(Element::Item(vec![16], flags()))),
            Element::NotSummed(Box::new(Element::SumTree(Some(vec![17]), -17, flags()))),
            Element::NotCountedOrSummed(Box::new(Element::SumTree(Some(vec![18]), -18, flags()))),
            Element::ReferenceWithSumItem(
                ReferencePathType::SiblingReference(vec![19]),
                Some(4),
                -19,
                flags(),
            ),
            Element::ProvableSumTree(Some(vec![20]), -20, flags()),
            Element::ProvableCountProvableSumTree(Some(vec![21]), 21, -21, flags()),
            Element::ProvableSumIndexedTree(Some(vec![22]), Some(vec![23]), -22, flags()),
            Element::ProvableCountIndexedTree(Some(vec![24]), Some(vec![25]), 24, flags()),
            Element::ProvableCountProvableSumIndexedTree(
                Some(vec![26]),
                25,
                -25,
                vec![(0, Some(vec![27])), (1, None)],
                flags(),
            ),
            Element::PrivateDocumentStore(26, 32, 4, flags()),
            Element::BidirectionalReference(
                crate::BidirectionalReference {
                    forward_reference_path: ReferencePathType::SiblingReference(vec![28]),
                    cascade_on_update: true,
                    max_hop: Some(5),
                    backward_references: Vec::new(),
                },
                flags(),
            ),
            Element::ItemWithBackwardsReferences(
                vec![29],
                crate::BackwardReferences::default(),
                flags(),
            ),
            Element::SumItemWithBackwardsReferences(
                -30,
                crate::BackwardReferences::default(),
                flags(),
            ),
            Element::ItemWithSumItemWithBackwardsReferences(
                vec![31],
                -31,
                crate::BackwardReferences::default(),
                flags(),
            ),
        ]
    }

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

    /// The backward-references family occupies wire discriminants 25/26/27
    /// (bincode variant indices, append-only). Pin them: a reorder of the
    /// enum would silently change the on-disk format.
    #[test]
    fn backward_references_family_wire_discriminants_are_pinned() {
        let grove_version = GroveVersion::latest();

        let bidi = Element::new_bidirectional_reference(
            crate::reference_path::ReferencePathType::AbsolutePathReference(vec![b"a".to_vec()]),
        );
        let item = Element::ItemWithBackwardsReferences(b"v".to_vec(), Default::default(), None);
        let sum_item = Element::SumItemWithBackwardsReferences(7, Default::default(), None);

        assert_eq!(bidi.serialize(grove_version).unwrap()[0], 25);
        assert_eq!(item.serialize(grove_version).unwrap()[0], 26);
        assert_eq!(sum_item.serialize(grove_version).unwrap()[0], 27);

        // And they round-trip.
        for element in [bidi, item, sum_item] {
            let bytes = element.serialize(grove_version).unwrap();
            assert_eq!(
                Element::deserialize(&bytes, grove_version).unwrap(),
                element
            );
        }
    }

    #[test]
    fn noncanonical_nested_wrappers_are_rejected_without_recursing() {
        // `fb 00 0f` is bincode's accepted, non-minimal u16 form of the
        // NonCounted discriminant. It bypassed the former raw-byte precheck.
        let mut bytes = vec![251, 0, 15];
        bytes.extend(std::iter::repeat_n(15, 100_000));
        bytes.extend([0, 0, 0]);

        let config = config::standard().with_big_endian().with_no_limit();
        let direct = bincode::decode_from_slice::<Element, _>(&bytes, config);
        assert!(
            matches!(direct, Err(bincode::error::DecodeError::Other(message)) if message.contains("nested Element wrappers"))
        );
        let borrowed = bincode::borrow_decode_from_slice::<Element, _>(&bytes, config);
        assert!(
            matches!(borrowed, Err(bincode::error::DecodeError::Other(message)) if message.contains("nested Element wrappers"))
        );

        let result = std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || Element::deserialize(&bytes, GroveVersion::latest()))
            .expect("spawn decoder on a small stack")
            .join()
            .expect("decoder must not overflow its stack");

        assert!(
            matches!(result, Err(ElementError::CorruptedData(message)) if message.contains("nested Element wrappers")),
            "nested wrappers should return a structured decode error"
        );
    }

    #[test]
    fn every_vec_bearing_variant_rejects_an_unbacked_length_without_panicking() {
        for element in every_element_variant() {
            let name = element.type_str();
            let mut bytes = element
                .serialize(GroveVersion::latest())
                .expect("serialize test element");

            // Every Element variant has a trailing flags Option<Vec<u8>>.
            // Replace its empty vector length with bincode's u64 marker and
            // the largest possible declaration, without supplying contents.
            assert_eq!(bytes.pop(), Some(0));
            assert_eq!(bytes[bytes.len() - 1], 1);
            bytes.push(252);
            bytes.extend(u32::MAX.to_be_bytes());

            let error = match Element::deserialize(&bytes, GroveVersion::latest()) {
                Ok(_) => panic!("{name} accepted an unbacked vector length"),
                Err(error) => error,
            };
            assert!(matches!(&error, ElementError::CorruptedData(_)));
        }
    }

    #[test]
    fn unlimited_public_decoders_do_not_reserve_an_unbacked_length() {
        let bytes = [vec![0, 253], u64::MAX.to_be_bytes().to_vec()].concat();
        let config = config::standard().with_big_endian().with_no_limit();

        assert!(bincode::decode_from_slice::<Element, _>(&bytes, config).is_err());
        assert!(bincode::borrow_decode_from_slice::<Element, _>(&bytes, config).is_err());
    }

    /// `NonCounted` may not wrap the backward-references family — enforced
    /// at construction, serialization, AND deserialization (fail closed
    /// symmetric in both directions).
    #[test]
    fn non_counted_rejects_backward_references_family() {
        let grove_version = GroveVersion::latest();

        let inners = [
            Element::new_bidirectional_reference(
                crate::reference_path::ReferencePathType::AbsolutePathReference(
                    vec![b"a".to_vec()],
                ),
            ),
            Element::ItemWithBackwardsReferences(b"v".to_vec(), Default::default(), None),
            Element::SumItemWithBackwardsReferences(7, Default::default(), None),
        ];

        for inner in inners {
            // Constructor refuses.
            assert!(Element::new_non_counted(inner.clone()).is_err());

            // A hand-built wrapper refuses to serialize.
            let wrapped = Element::NonCounted(Box::new(inner.clone()));
            assert!(wrapped.serialize(grove_version).is_err());

            // Hand-built wire bytes refuse to deserialize.
            let mut bytes = vec![15u8];
            bytes.extend(inner.serialize(grove_version).unwrap());
            assert!(Element::deserialize(&bytes, grove_version).is_err());

            // The type-classifier guard also rejects the pair.
            assert!(crate::ElementType::from_serialized_value(&bytes).is_err());
        }
    }

    #[test]
    fn every_variant_round_trips_on_all_supported_versions() {
        for grove_version in GROVE_VERSIONS {
            for element in every_element_variant() {
                let bytes = element
                    .serialize(grove_version)
                    .expect("serialize valid element");
                let decoded =
                    Element::deserialize(&bytes, grove_version).expect("deserialize valid element");
                assert_eq!(decoded, element);
            }
        }
    }
}
