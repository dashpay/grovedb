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
