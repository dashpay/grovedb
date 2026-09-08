//! Bidirectional reference definitions.
//!
//! A bidirectional reference behaves like [`crate::Element::Reference`] on
//! reads, but additionally registers itself in its target's backward-reference
//! list so that updates to the target propagate back along the reference
//! chain (or cascade-delete it). Only elements that opt into backward
//! references (`ItemWithBackwardsReferences`, `SumItemWithBackwardsReferences`,
//! or another `BidirectionalReference`) may be targeted.
//!
//! # Storage & hashing model
//!
//! Backward references live **on the target element itself** and are covered
//! by the node hash through a two-layer scheme:
//!
//! ```text
//! inner_hash      = H(serialize(element with backward_references = []))
//! backrefs_hash   = H(serialize(backward_references))
//! node value_hash = combine(inner_hash, backrefs_hash)            // items
//!                 = combine3(inner_hash, target_inner_hash,
//!                            backrefs_hash)                       // bidi refs
//! ```
//!
//! Forward references (and every member of a reference chain) commit to the
//! target's **inner** hash, so registering or removing a referrer changes
//! only the target's own node hash — never the hashes stored by other
//! referrers. Proofs carry the stripped (inner) serialization plus the
//! 32-byte `backrefs_hash`, so the referrer set is authenticated without
//! bloating or leaking into result sets.

use bincode::{Decode, DecodeUntrusted, Encode};

use crate::{element::MaxReferenceHop, reference_path::ReferencePathType};

/// The protocol ceiling on the referrer capacity a backward-references
/// ITEM (`ItemWithBackwardsReferences` / `SumItemWithBackwardsReferences` /
/// `ItemWithSumItemWithBackwardsReferences`) may declare, and therefore on
/// the number of backward references it can ever carry.
///
/// Each item declares its own capacity ([`BackwardReferences::max_incoming`])
/// at or below this ceiling; the ceiling is what bounds the cascade work of
/// an operation that cannot see the stored element it displaces (a delete,
/// or a plain payload landing on a registered element) and what bounds the
/// node growth registrations can inflict on a target.
pub const MAX_BACKWARD_REFERENCES: usize = 256;

/// The referrer capacity the convenience constructors declare when the
/// caller does not choose one — the flat per-item limit the family shipped
/// with. Callers that know their fan-out (an item indexed four ways
/// declares four) should declare it explicitly: declared capacity is what
/// the batch cost estimator charges for a write.
pub const DEFAULT_BACKWARD_REFERENCES_CAPACITY: u16 = 32;

/// The maximum number of backward references a `BidirectionalReference`
/// itself may carry (chains do not branch, keeping worst-case propagation
/// linear in the target's declared capacity: at most
/// `capacity × MAX_REFERENCE_HOPS` nodes are affected).
pub const MAX_BACKWARD_REFERENCES_ON_REFERENCE: usize = 1;

/// Flag to indicate whether the bidirectional reference should be deleted when
/// the pointed-to item no longer exists or becomes incompatible. When unset,
/// such an update is refused with an error instead.
pub type CascadeOnUpdate = bool;

/// One registered referrer of a backward-references-capable element: the
/// inverse path leading back to the referring `BidirectionalReference`,
/// plus its cascade policy.
///
/// A referrer is identified by its `inverted_reference` (which is derived
/// from the referrer's position, so it is unique per referrer); there are
/// no slot indices.
#[derive(Clone, Debug, Encode, Decode, PartialEq, Eq, Hash, DecodeUntrusted)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BackwardReference {
    /// Path leading back to the referring `BidirectionalReference`.
    pub inverted_reference: ReferencePathType,
    /// Whether the referrer may be cascade-deleted when this element is
    /// removed or becomes incompatible.
    pub cascade_on_update: bool,
}

/// The referrer list of a backward-references ITEM together with the
/// capacity the element declares for it.
///
/// `max_incoming` is authenticated with the element: it sits in the
/// STRIPPED (inner) bytes, so every referrer's commitment covers it and
/// every node enforces the same value. `entries` are the registered
/// referrers, excluded from the inner hash (see the module docs); there
/// are never more of them than `max_incoming`, and `max_incoming` never
/// exceeds [`MAX_BACKWARD_REFERENCES`].
///
/// Declaring a capacity allocates nothing: the list stores only actual
/// registrations. Raising the capacity on an update is always accepted;
/// lowering it below the number of registered referrers is refused.
///
/// The type dereferences to its entries so the list can be read and
/// maintained like the plain `Vec` it replaced.
#[derive(Clone, Debug, Encode, Decode, PartialEq, Eq, Hash, DecodeUntrusted)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BackwardReferences {
    /// How many referrers this element accepts (at most
    /// [`MAX_BACKWARD_REFERENCES`]). Part of the inner hash.
    pub max_incoming: u16,
    /// The registered referrers, at most `max_incoming` of them.
    pub entries: Vec<BackwardReference>,
}

impl BackwardReferences {
    /// An empty list declaring `max_incoming` as its capacity.
    pub fn with_max_incoming(max_incoming: u16) -> Self {
        Self {
            max_incoming,
            entries: Vec::new(),
        }
    }

    /// A list with the given entries declaring `max_incoming` as its
    /// capacity. Budgets are enforced by
    /// `Element::validate_backward_references_limits`, not here.
    pub fn new(max_incoming: u16, entries: Vec<BackwardReference>) -> Self {
        Self {
            max_incoming,
            entries,
        }
    }

    /// The declared capacity.
    pub fn max_incoming(&self) -> u16 {
        self.max_incoming
    }

    /// Whether another referrer can register.
    pub fn is_full(&self) -> bool {
        self.entries.len() >= self.max_incoming as usize
    }
}

impl Default for BackwardReferences {
    /// Empty, declaring [`DEFAULT_BACKWARD_REFERENCES_CAPACITY`].
    fn default() -> Self {
        Self::with_max_incoming(DEFAULT_BACKWARD_REFERENCES_CAPACITY)
    }
}

impl From<Vec<BackwardReference>> for BackwardReferences {
    /// The entries under [`DEFAULT_BACKWARD_REFERENCES_CAPACITY`].
    fn from(entries: Vec<BackwardReference>) -> Self {
        Self::new(DEFAULT_BACKWARD_REFERENCES_CAPACITY, entries)
    }
}

impl std::ops::Deref for BackwardReferences {
    type Target = Vec<BackwardReference>;

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl std::ops::DerefMut for BackwardReferences {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}

/// Payload of [`crate::Element::BidirectionalReference`].
#[derive(Clone, Debug, Encode, Decode, PartialEq, Eq, Hash, DecodeUntrusted)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BidirectionalReference {
    /// Where this reference points to, like a regular reference.
    pub forward_reference_path: ReferencePathType,
    /// Whether overwriting/deleting the target may cascade-delete this
    /// reference (otherwise such an update errors).
    pub cascade_on_update: CascadeOnUpdate,
    /// Maximum number of reference hops allowed when following the chain.
    pub max_hop: MaxReferenceHop,
    /// Referrers registered on THIS reference (it can itself be targeted by
    /// at most [`MAX_BACKWARD_REFERENCES_ON_REFERENCE`] other bidirectional
    /// references). Excluded from the inner hash; see the module docs.
    pub backward_references: Vec<BackwardReference>,
}

/// Canonical serialization of a backward-references list (bincode, big
/// endian, no limit — the same codec configuration `Element` uses). The
/// 32-byte hash of these bytes is the `backrefs_hash` half of the node's
/// combined value hash, so this encoding is consensus-critical.
pub fn serialize_backward_references(
    backward_references: &[BackwardReference],
) -> Result<Vec<u8>, crate::error::ElementError> {
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    bincode::encode_to_vec(backward_references, config).map_err(|e| {
        crate::error::ElementError::CorruptedData(format!(
            "unable to serialize backward references: {e}"
        ))
    })
}

/// Inverse of [`serialize_backward_references`].
pub fn deserialize_backward_references(
    bytes: &[u8],
) -> Result<Vec<BackwardReference>, crate::error::ElementError> {
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    bincode::decode_from_slice_untrusted(bytes, config)
        .map_err(|e| {
            crate::error::ElementError::CorruptedData(format!(
                "unable to deserialize backward references: {e}"
            ))
        })
        .and_then(|(v, consumed)| {
            // Trailing bytes would let two different byte strings decode to
            // the same list; the codec is consensus-critical, so reject
            // them like `Element::deserialize` does.
            if consumed == bytes.len() {
                Ok(v)
            } else {
                Err(crate::error::ElementError::CorruptedData(
                    "trailing bytes after backward references".to_string(),
                ))
            }
        })
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use super::*;
    use crate::{element::ElementFlags, Element, ElementType, ProofNodeType};

    fn sibling_ref() -> ReferencePathType {
        ReferencePathType::SiblingReference(b"target".to_vec())
    }

    fn backref(tag: &[u8]) -> BackwardReference {
        BackwardReference {
            inverted_reference: ReferencePathType::SiblingReference(tag.to_vec()),
            cascade_on_update: true,
        }
    }

    fn bidi(flags: Option<ElementFlags>) -> Element {
        Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: sibling_ref(),
                cascade_on_update: true,
                max_hop: Some(5),
                backward_references: Vec::new(),
            },
            flags,
        )
    }

    #[test]
    fn constructors_produce_expected_shapes() {
        assert_eq!(
            Element::new_item_allowing_bidirectional_references(b"v".to_vec()),
            Element::ItemWithBackwardsReferences(b"v".to_vec(), Default::default(), None)
        );
        assert_eq!(
            Element::new_sum_item_allowing_bidirectional_references_with_flags(7, Some(vec![2])),
            Element::SumItemWithBackwardsReferences(7, Default::default(), Some(vec![2]))
        );
        assert_eq!(
            Element::new_item_allowing_bidirectional_references_with_flags(
                b"v".to_vec(),
                Some(vec![3])
            ),
            Element::ItemWithBackwardsReferences(b"v".to_vec(), Default::default(), Some(vec![3]))
        );
        let Element::BidirectionalReference(full, full_flags) =
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
        assert!(full.backward_references.is_empty());
        assert_eq!(full_flags, Some(vec![9]));
    }

    #[test]
    fn classification_of_backward_references_family() {
        let bidi = bidi(None);
        let item = Element::ItemWithBackwardsReferences(b"v".to_vec(), Default::default(), None);
        let sum_item = Element::SumItemWithBackwardsReferences(-3, Default::default(), None);

        assert!(bidi.is_reference());
        assert!(item.is_any_item());
        assert!(sum_item.is_sum_item());
        assert!(sum_item.is_sum_bearing_child());
        assert!(item.supports_backward_references());
        assert!(bidi.supports_backward_references());
        assert!(!Element::new_item(b"x".to_vec()).supports_backward_references());

        assert_eq!(bidi.element_type(), ElementType::BidirectionalReference);
        assert_eq!(sum_item.sum_value_or_default(), -3);
        assert_eq!(item.as_item_bytes().unwrap(), b"v");
        assert_eq!(
            bidi.clone().into_reference_path_type().unwrap(),
            sibling_ref()
        );

        // Proof shapes: the reference resolves through KvRefValueHash; the
        // items use the dedicated recombining node kind.
        assert_eq!(
            ElementType::BidirectionalReference.proof_node_type(Some(ElementType::Tree)),
            ProofNodeType::KvRefValueHash
        );
        assert_eq!(
            ElementType::ItemWithBackwardsReferences.proof_node_type(Some(ElementType::Tree)),
            ProofNodeType::KvBackwardsReferencesValueHash
        );
    }

    #[test]
    fn stripping_and_limits() {
        let grove_version = GroveVersion::latest();

        let mut item = Element::ItemWithBackwardsReferences(
            b"v".to_vec(),
            vec![backref(b"a"), backref(b"b")].into(),
            Some(vec![1]),
        );
        let stripped = item.stripped_of_backward_references();
        assert_eq!(
            stripped,
            Element::ItemWithBackwardsReferences(b"v".to_vec(), Default::default(), Some(vec![1]))
        );
        // Stripped form round-trips the codec (it IS a valid element).
        let bytes = stripped.serialize(grove_version).unwrap();
        assert_eq!(
            Element::deserialize(&bytes, grove_version).unwrap(),
            stripped
        );

        // Registering a referrer never changes the stripped form.
        item.backward_references_mut().unwrap().push(backref(b"c"));
        assert_eq!(item.stripped_of_backward_references(), stripped);

        // Budgets: 32 for items...
        let full_list: Vec<_> = (0..32u8).map(|i| backref(&[i])).collect();
        let at_limit =
            Element::ItemWithBackwardsReferences(b"v".to_vec(), full_list.clone().into(), None);
        assert!(at_limit.validate_backward_references_limits().is_ok());
        assert!(at_limit.serialize(grove_version).is_ok());
        let mut over = full_list;
        over.push(backref(b"!"));
        let over_limit = Element::ItemWithBackwardsReferences(b"v".to_vec(), over.into(), None);
        assert!(over_limit.validate_backward_references_limits().is_err());
        assert!(over_limit.serialize(grove_version).is_err());

        // ...and 1 for references.
        let Element::BidirectionalReference(mut reference, _) = bidi(None) else {
            unreachable!()
        };
        reference.backward_references = vec![backref(b"a"), backref(b"b")];
        let over_ref = Element::BidirectionalReference(reference, None);
        assert!(over_ref.validate_backward_references_limits().is_err());
        assert!(over_ref.serialize(grove_version).is_err());
    }

    #[test]
    fn display_and_type_names_for_the_family() {
        let bidi_with = Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: sibling_ref(),
                cascade_on_update: false,
                max_hop: None,
                backward_references: Vec::new(),
            },
            Some(vec![7]),
        );
        let s = format!("{}", bidi_with);
        assert!(
            s.starts_with("BidirectionalReference(") && s.contains("flags"),
            "got: {s}"
        );
        let s = format!("{}", bidi(None));
        assert!(s.contains("max_hop: 5") && !s.contains("flags"), "got: {s}");

        let item =
            Element::ItemWithBackwardsReferences(b"v".to_vec(), Default::default(), Some(vec![1]));
        let s = format!("{}", item);
        assert!(s.starts_with("ItemWithBackwardsReferences("), "got: {s}");
        let sum = Element::SumItemWithBackwardsReferences(-2, Default::default(), None);
        let s = format!("{}", sum);
        assert!(
            s.starts_with("SumItemWithBackwardsReferences(-2"),
            "got: {s}"
        );

        assert_eq!(
            item.element_type(),
            ElementType::ItemWithBackwardsReferences
        );
        assert_eq!(
            sum.element_type(),
            ElementType::SumItemWithBackwardsReferences
        );
        assert_eq!(
            ElementType::BidirectionalReference.as_str(),
            "bidirectional reference"
        );
        assert_eq!(
            ElementType::ItemWithBackwardsReferences.as_str(),
            "item with backwards references"
        );
        assert_eq!(
            ElementType::SumItemWithBackwardsReferences.as_str(),
            "sum item with backwards references"
        );
    }

    #[test]
    fn aggregation_wrappers_reject_the_family() {
        for element in [
            bidi(None),
            Element::ItemWithBackwardsReferences(b"v".to_vec(), Default::default(), None),
            Element::SumItemWithBackwardsReferences(1, Default::default(), None),
        ] {
            assert!(
                Element::new_non_counted(element.clone()).is_err(),
                "NonCounted must reject {element}"
            );
            let hand_built = Element::NonCounted(Box::new(element));
            assert!(hand_built.validate_wrapper_invariants().is_err());
        }
    }

    #[test]
    fn value_accessors_see_through_the_family() {
        let item = Element::ItemWithBackwardsReferences(b"v".to_vec(), Default::default(), None);
        assert_eq!(item.clone().into_item_bytes().unwrap(), b"v".to_vec());
        let sum = Element::SumItemWithBackwardsReferences(-4, Default::default(), None);
        assert_eq!(sum.as_sum_item_value().unwrap(), -4);
        assert_eq!(sum.clone().into_sum_item_value().unwrap(), -4);
    }

    #[test]
    fn flags_accessors_cover_the_family() {
        for mut element in [
            bidi(Some(vec![1])),
            Element::ItemWithBackwardsReferences(b"v".to_vec(), Default::default(), Some(vec![1])),
            Element::SumItemWithBackwardsReferences(1, Default::default(), Some(vec![1])),
        ] {
            assert_eq!(element.get_flags(), &Some(vec![1]));
            *element.get_flags_mut() = Some(vec![2]);
            element.set_flags(Some(vec![3]));
            assert_eq!(element.clone().get_flags_owned(), Some(vec![3]));
        }
    }

    #[test]
    fn bidirectional_forward_path_can_be_made_absolute() {
        let element = bidi(None);
        let absolute = element
            .convert_if_reference_to_absolute_reference(
                &[b"root".as_slice(), b"leaf".as_slice()],
                Some(b"me".as_slice()),
            )
            .unwrap();
        let Element::BidirectionalReference(reference, _) = absolute else {
            panic!("expected a bidirectional reference");
        };
        assert_eq!(
            reference.forward_reference_path,
            ReferencePathType::AbsolutePathReference(vec![
                b"root".to_vec(),
                b"leaf".to_vec(),
                b"target".to_vec()
            ])
        );
        // An already-absolute forward path is returned unchanged.
        let element = Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                    b"x".to_vec()
                ]),
                cascade_on_update: false,
                max_hop: None,
                backward_references: Vec::new(),
            },
            None,
        );
        assert_eq!(
            element
                .clone()
                .convert_if_reference_to_absolute_reference(&[b"a".as_slice()], None)
                .unwrap(),
            element
        );
    }

    #[test]
    fn corrupt_and_over_limit_bytes_are_rejected() {
        let grove_version = GroveVersion::latest();

        // Garbage referrer-list bytes fail the standalone codec.
        assert!(deserialize_backward_references(&[0xff, 0xff, 0xff]).is_err());

        // Raw element bytes with an over-limit referrer list (crafted by
        // encoding the enum directly, bypassing `serialize`'s validation)
        // are rejected on deserialize.
        let over = Element::ItemWithBackwardsReferences(
            b"v".to_vec(),
            (0..33u8).map(|i| backref(&[i])).collect::<Vec<_>>().into(),
            None,
        );
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let bytes = bincode::encode_to_vec(&over, config).unwrap();
        assert!(Element::deserialize(&bytes, grove_version).is_err());
    }

    #[test]
    fn item_with_sum_item_twin_matches_family_semantics() {
        let grove_version = GroveVersion::latest();
        let element =
            Element::new_item_with_sum_item_allowing_bidirectional_references(b"v".to_vec(), 7);
        assert_eq!(
            element,
            Element::ItemWithSumItemWithBackwardsReferences(
                b"v".to_vec(),
                7,
                Default::default(),
                None
            )
        );
        assert_eq!(
            Element::new_item_with_sum_item_allowing_bidirectional_references_with_flags(
                b"v".to_vec(),
                7,
                Some(vec![1])
            ),
            Element::ItemWithSumItemWithBackwardsReferences(
                b"v".to_vec(),
                7,
                Default::default(),
                Some(vec![1])
            )
        );

        // Classification mirrors ItemWithSumItem plus the family flags.
        assert!(element.is_any_item());
        assert!(element.is_sum_item());
        assert!(element.is_item_with_sum_item());
        assert!(element.has_basic_item());
        assert!(element.is_sum_bearing_child());
        assert!(element.is_count_and_sum_bearing_child());
        assert!(element.supports_backward_references());
        assert_eq!(element.sum_value_or_default(), 7);
        assert_eq!(element.as_item_bytes().unwrap(), b"v");
        assert_eq!(element.as_sum_item_value().unwrap(), 7);
        assert_eq!(element.clone().into_item_bytes().unwrap(), b"v".to_vec());
        assert_eq!(element.clone().into_sum_item_value().unwrap(), 7);
        assert_eq!(
            element.element_type(),
            ElementType::ItemWithSumItemWithBackwardsReferences
        );
        assert_eq!(
            ElementType::ItemWithSumItemWithBackwardsReferences.as_str(),
            "item with sum item with backwards references"
        );
        assert_eq!(
            ElementType::ItemWithSumItemWithBackwardsReferences
                .proof_node_type(Some(ElementType::Tree)),
            ProofNodeType::KvBackwardsReferencesValueHash
        );
        let shown = format!("{element}");
        assert!(
            shown.starts_with("ItemWithSumItemWithBackwardsReferences("),
            "got: {shown}"
        );

        // Wrapper rejection.
        assert!(Element::new_non_counted(element.clone()).is_err());

        // Codec: roundtrip, stripping, and the 32-entry budget.
        let with_refs = Element::ItemWithSumItemWithBackwardsReferences(
            b"v".to_vec(),
            7,
            vec![backref(b"a"), backref(b"b")].into(),
            Some(vec![2]),
        );
        let bytes = with_refs.serialize(grove_version).unwrap();
        assert_eq!(
            Element::deserialize(&bytes, grove_version).unwrap(),
            with_refs
        );
        assert_eq!(bytes[0], 28, "wire discriminant is pinned");
        assert_eq!(
            with_refs.stripped_of_backward_references(),
            Element::ItemWithSumItemWithBackwardsReferences(
                b"v".to_vec(),
                7,
                Default::default(),
                Some(vec![2])
            )
        );
        let over = Element::ItemWithSumItemWithBackwardsReferences(
            b"v".to_vec(),
            7,
            (0..33u8).map(|i| backref(&[i])).collect::<Vec<_>>().into(),
            None,
        );
        assert!(over.validate_backward_references_limits().is_err());
        assert!(over.serialize(grove_version).is_err());
    }

    #[test]
    fn backward_references_codec_round_trips() {
        let list = vec![backref(b"a"), backref(b"zz")];
        let bytes = serialize_backward_references(&list).unwrap();
        assert_eq!(deserialize_backward_references(&bytes).unwrap(), list);
        // Trailing bytes are rejected — one list, one byte string.
        let mut padded = bytes.clone();
        padded.push(0);
        assert!(deserialize_backward_references(&padded).is_err());
        // The empty list has a stable 1-byte encoding (its hash is a
        // protocol constant).
        assert_eq!(serialize_backward_references(&[]).unwrap().len(), 1);

        // A declared list length without encoded entries is rejected before
        // the decoder reserves space for the declaration.
        let unbacked_length = [vec![252], u32::MAX.to_be_bytes().to_vec()].concat();
        assert!(deserialize_backward_references(&unbacked_length).is_err());
    }
    /// The declared capacity bounds registrations, the protocol ceiling
    /// bounds declarations, and decode enforces both.
    #[test]
    fn declared_capacity_bounds_registrations_and_the_ceiling_bounds_declarations() {
        let grove_version = GroveVersion::latest();
        let entries = |n: u8| (0..n).map(|i| backref(&[i])).collect::<Vec<_>>();

        let within = Element::ItemWithBackwardsReferences(
            b"v".to_vec(),
            BackwardReferences::new(2, entries(2)),
            None,
        );
        assert!(within.validate_backward_references_limits().is_ok());
        assert_eq!(within.max_incoming_references(), Some(2));

        let over_capacity = Element::ItemWithBackwardsReferences(
            b"v".to_vec(),
            BackwardReferences::new(2, entries(3)),
            None,
        );
        assert!(over_capacity.validate_backward_references_limits().is_err());

        let at_ceiling = Element::SumItemWithBackwardsReferences(
            1,
            BackwardReferences::with_max_incoming(MAX_BACKWARD_REFERENCES as u16),
            None,
        );
        assert!(at_ceiling.validate_backward_references_limits().is_ok());

        let over_ceiling = Element::ItemWithSumItemWithBackwardsReferences(
            b"v".to_vec(),
            1,
            BackwardReferences::with_max_incoming(MAX_BACKWARD_REFERENCES as u16 + 1),
            None,
        );
        assert!(over_ceiling.validate_backward_references_limits().is_err());

        // Serialization refuses both shapes …
        assert!(over_capacity.serialize(grove_version).is_err());
        assert!(over_ceiling.serialize(grove_version).is_err());
        // … and decode enforces the same rules on bytes a peer hands over.
        // Layout of `ItemWithBackwardsReferences(b"v", refs, None)`:
        // discriminant, value length, value byte, then `max_incoming`.
        let bytes = within.serialize(grove_version).unwrap();
        assert_eq!(Element::deserialize(&bytes, grove_version).unwrap(), within);
        assert_eq!(bytes[3], 2, "the declared capacity (one-byte varint)");
        let mut over_capacity_bytes = bytes.clone();
        over_capacity_bytes[3] = 1;
        assert!(Element::deserialize(&over_capacity_bytes, grove_version).is_err());
        // At the ceiling the capacity is a three-byte varint (marker, then
        // the big-endian u16); bumping it one past the ceiling must fail.
        let ceiling_bytes = at_ceiling.serialize(grove_version).unwrap();
        assert_eq!(
            ceiling_bytes[0], 27,
            "SumItemWithBackwardsReferences discriminant"
        );
        assert_eq!(
            ceiling_bytes[2..5],
            [0xFB, 0x01, 0x00],
            "sum value varint then the ceiling as a three-byte varint"
        );
        let mut over_ceiling_bytes = ceiling_bytes.clone();
        over_ceiling_bytes[3..5]
            .copy_from_slice(&(MAX_BACKWARD_REFERENCES as u16 + 1).to_be_bytes());
        assert!(Element::deserialize(&over_ceiling_bytes, grove_version).is_err());
        assert_eq!(
            Element::deserialize(&ceiling_bytes, grove_version).unwrap(),
            at_ceiling
        );

        // The plain constructors declare the historical default; the
        // explicit ones declare what they are given.
        assert_eq!(
            Element::new_item_allowing_bidirectional_references(b"v".to_vec())
                .max_incoming_references(),
            Some(DEFAULT_BACKWARD_REFERENCES_CAPACITY)
        );
        assert_eq!(
            Element::new_item_allowing_bidirectional_references_with_capacity(b"v".to_vec(), 4)
                .max_incoming_references(),
            Some(4)
        );
        assert!(BackwardReferences::new(1, entries(1)).is_full());
        assert!(!BackwardReferences::with_max_incoming(1).is_full());
    }

    /// The capacity is part of the INNER (stripped) bytes — every referrer
    /// commits to it — while the entries stay outside them.
    #[test]
    fn capacity_is_in_the_inner_bytes_and_entries_are_not() {
        let grove_version = GroveVersion::latest();
        let a = Element::ItemWithBackwardsReferences(
            b"v".to_vec(),
            BackwardReferences::new(4, vec![backref(b"x")]),
            None,
        );
        let b = Element::ItemWithBackwardsReferences(
            b"v".to_vec(),
            BackwardReferences::new(4, vec![backref(b"y"), backref(b"z")]),
            None,
        );
        let c = Element::ItemWithBackwardsReferences(
            b"v".to_vec(),
            BackwardReferences::new(5, vec![backref(b"x")]),
            None,
        );
        let stripped = |e: &Element| {
            e.stripped_of_backward_references()
                .serialize(grove_version)
                .unwrap()
        };
        assert_eq!(
            stripped(&a),
            stripped(&b),
            "entries are outside the inner bytes"
        );
        assert_ne!(
            stripped(&a),
            stripped(&c),
            "capacity is inside the inner bytes"
        );
        assert_eq!(
            a.stripped_of_backward_references()
                .max_incoming_references(),
            Some(4),
            "stripping keeps the declaration"
        );
        assert!(a
            .stripped_of_backward_references()
            .backward_references()
            .unwrap()
            .is_empty());
    }
}
