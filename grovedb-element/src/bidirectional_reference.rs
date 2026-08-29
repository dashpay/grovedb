//! Bidirectional reference definitions.
//!
//! A bidirectional reference behaves like [`crate::Element::Reference`] on
//! reads, but additionally registers itself in its target's backward-reference
//! meta storage so that updates to the target propagate back along the
//! reference chain (or cascade-delete it). Only elements that opt into
//! backward references (`ItemWithBackwardsReferences`,
//! `SumItemWithBackwardsReferences`, or another `BidirectionalReference`)
//! may be targeted.
//!
//! The definitions live here (rather than in the `grovedb` crate that hosts
//! the propagation machinery) because the `Element` enum embeds
//! [`BidirectionalReference`] directly.

use bincode::{Decode, Encode};

use crate::{
    element::{ElementFlags, MaxReferenceHop},
    reference_path::ReferencePathType,
};

/// Index of a backward reference inside its target's 32-slot meta bitvec.
pub type SlotIdx = usize;

/// Flag to indicate whether the bidirectional reference should be deleted when
/// the pointed-to item no longer exists or becomes incompatible. When unset,
/// such an update is refused with an error instead.
pub type CascadeOnUpdate = bool;

/// Payload of [`crate::Element::BidirectionalReference`].
#[derive(Clone, Debug, Encode, Decode, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BidirectionalReference {
    /// Where this reference points to, like a regular reference.
    pub forward_reference_path: ReferencePathType,
    /// Slot (0..32) occupied in the target's backward-references bitvec.
    /// Assigned on insertion; the value supplied by the caller is
    /// overwritten.
    pub backward_reference_slot: SlotIdx,
    /// Whether overwriting/deleting the target may cascade-delete this
    /// reference (otherwise such an update errors).
    pub cascade_on_update: CascadeOnUpdate,
    /// Maximum number of reference hops allowed when following the chain.
    pub max_hop: MaxReferenceHop,
    /// Optional per-element metadata.
    pub flags: Option<ElementFlags>,
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use super::*;
    use crate::{Element, ElementType, ProofNodeType};

    fn sibling_ref() -> ReferencePathType {
        ReferencePathType::SiblingReference(b"target".to_vec())
    }

    fn bidi(flags: Option<ElementFlags>) -> Element {
        Element::BidirectionalReference(BidirectionalReference {
            forward_reference_path: sibling_ref(),
            backward_reference_slot: 3,
            cascade_on_update: true,
            max_hop: Some(5),
            flags,
        })
    }

    #[test]
    fn constructors_produce_expected_shapes() {
        assert_eq!(
            Element::new_item_allowing_bidirectional_references(b"v".to_vec()),
            Element::ItemWithBackwardsReferences(b"v".to_vec(), None)
        );
        assert_eq!(
            Element::new_item_allowing_bidirectional_references_with_flags(
                b"v".to_vec(),
                Some(vec![1])
            ),
            Element::ItemWithBackwardsReferences(b"v".to_vec(), Some(vec![1]))
        );
        assert_eq!(
            Element::new_sum_item_allowing_bidirectional_references(7),
            Element::SumItemWithBackwardsReferences(7, None)
        );
        assert_eq!(
            Element::new_sum_item_allowing_bidirectional_references_with_flags(7, Some(vec![2])),
            Element::SumItemWithBackwardsReferences(7, Some(vec![2]))
        );

        let Element::BidirectionalReference(plain) =
            Element::new_bidirectional_reference(sibling_ref())
        else {
            panic!("expected a bidirectional reference");
        };
        assert_eq!(plain.forward_reference_path, sibling_ref());
        assert_eq!(plain.backward_reference_slot, 0);
        assert!(!plain.cascade_on_update);
        assert_eq!(plain.max_hop, None);
        assert_eq!(plain.flags, None);

        let Element::BidirectionalReference(full) =
            Element::new_bidirectional_reference_with_options(
                sibling_ref(),
                Some(2),
                true,
                Some(vec![9]),
            )
        else {
            panic!("expected a bidirectional reference");
        };
        assert_eq!(full.max_hop, Some(2));
        assert!(full.cascade_on_update);
        assert_eq!(full.flags, Some(vec![9]));
    }

    #[test]
    fn display_covers_backward_references_family() {
        let shown = format!("{}", bidi(Some(vec![1])));
        assert!(shown.contains("BidirectionalReference"), "{shown}");
        assert!(shown.contains("max_hop: 5"), "{shown}");
        assert!(shown.contains("cascade: true"), "{shown}");
        assert!(shown.contains("flags"), "{shown}");

        let shown = format!(
            "{}",
            Element::ItemWithBackwardsReferences(b"abc".to_vec(), None)
        );
        assert!(shown.contains("ItemWithBackwardsReferences"), "{shown}");
        assert!(shown.contains("abc"), "{shown}");

        let shown = format!("{}", Element::SumItemWithBackwardsReferences(-4, None));
        assert!(shown.contains("SumItemWithBackwardsReferences"), "{shown}");
        assert!(shown.contains("-4"), "{shown}");
    }

    #[test]
    fn classification_of_backward_references_family() {
        let bidi = bidi(None);
        let item = Element::ItemWithBackwardsReferences(b"v".to_vec(), None);
        let sum_item = Element::SumItemWithBackwardsReferences(-3, None);

        assert!(bidi.is_reference());
        assert!(!bidi.is_any_item());
        assert!(!bidi.is_any_tree());
        assert!(item.is_any_item());
        assert!(!item.is_sum_item());
        assert!(sum_item.is_any_item());
        assert!(sum_item.is_sum_item());
        assert!(sum_item.is_sum_bearing_child());
        assert!(!item.is_sum_bearing_child());

        assert_eq!(bidi.element_type(), ElementType::BidirectionalReference);
        assert_eq!(
            item.element_type(),
            ElementType::ItemWithBackwardsReferences
        );
        assert_eq!(
            sum_item.element_type(),
            ElementType::SumItemWithBackwardsReferences
        );
        assert_eq!(bidi.type_str(), "bidirectional reference");
        assert_eq!(item.type_str(), "item with backwards references");
        assert_eq!(sum_item.type_str(), "sum item with backwards references");

        // Proof shapes: the reference gets the combined-hash family, the
        // items the simple-hash family.
        assert_eq!(
            ElementType::BidirectionalReference.proof_node_type(Some(ElementType::Tree)),
            ProofNodeType::KvRefValueHash
        );
        assert_eq!(
            ElementType::ItemWithBackwardsReferences.proof_node_type(Some(ElementType::Tree)),
            ProofNodeType::Kv
        );
    }

    #[test]
    fn value_helpers_cover_backward_references_family() {
        let bidi = bidi(None);
        let item = Element::ItemWithBackwardsReferences(b"v".to_vec(), None);
        let sum_item = Element::SumItemWithBackwardsReferences(-3, None);

        assert_eq!(sum_item.sum_value_or_default(), -3);
        assert_eq!(item.sum_value_or_default(), 0);
        assert_eq!(bidi.sum_value_or_default(), 0);
        assert_eq!(sum_item.count_value_or_default(), 1);
        assert_eq!(sum_item.count_sum_value_or_default(), (1, -3));
        assert_eq!(item.count_sum_value_or_default(), (1, 0));
        assert_eq!(sum_item.big_sum_value_or_default(), -3i128);

        assert_eq!(sum_item.as_sum_item_value().unwrap(), -3);
        assert_eq!(sum_item.clone().into_sum_item_value().unwrap(), -3);
        assert_eq!(item.as_item_bytes().unwrap(), b"v");
        assert_eq!(item.clone().into_item_bytes().unwrap(), b"v".to_vec());
        assert_eq!(
            bidi.clone().into_reference_path_type().unwrap(),
            sibling_ref()
        );
    }

    #[test]
    fn flags_accessors_cover_backward_references_family() {
        let mut elements = [
            bidi(Some(vec![1])),
            Element::ItemWithBackwardsReferences(b"v".to_vec(), Some(vec![2])),
            Element::SumItemWithBackwardsReferences(5, Some(vec![3])),
        ];
        for (i, element) in elements.iter_mut().enumerate() {
            let expected = Some(vec![(i + 1) as u8]);
            assert_eq!(element.get_flags(), &expected);
            assert_eq!(element.clone().get_flags_owned(), expected);
            assert_eq!(element.get_flags_mut(), &mut expected.clone());
            element.set_flags(Some(vec![42]));
            assert_eq!(element.get_flags(), &Some(vec![42]));
        }
    }

    #[test]
    fn bidirectional_reference_converts_to_absolute() {
        let grove_version = GroveVersion::latest();
        let element = bidi(None);
        let converted = element
            .convert_if_reference_to_absolute_reference(&[b"root", b"sub"], Some(b"self_key"))
            .expect("conversion works");
        let Element::BidirectionalReference(reference) = &converted else {
            panic!("variant preserved");
        };
        assert_eq!(
            reference.forward_reference_path,
            ReferencePathType::AbsolutePathReference(vec![
                b"root".to_vec(),
                b"sub".to_vec(),
                b"target".to_vec()
            ])
        );
        // Slot/cascade/max_hop survive the conversion.
        assert_eq!(reference.backward_reference_slot, 3);
        assert!(reference.cascade_on_update);
        assert_eq!(reference.max_hop, Some(5));

        // An already-absolute forward path passes through unchanged, and the
        // whole element round-trips the codec.
        let again = converted
            .clone()
            .convert_if_reference_to_absolute_reference(&[b"other"], None)
            .expect("absolute passthrough");
        assert_eq!(again, converted);
        let bytes = again.serialize(grove_version).expect("serializes");
        assert_eq!(
            Element::deserialize(&bytes, grove_version).expect("deserializes"),
            again
        );
    }
}
