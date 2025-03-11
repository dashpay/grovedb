//! Bidirectional references management module.

// Bidirectional references definitions shall be visible in `Element` thus
// they're left out of feature gate, the implementation though is not required
// for `verify`.
#[cfg(feature = "minimal")]
mod handling;

use bincode::{Decode, Encode};
#[cfg(feature = "minimal")]
pub(crate) use handling::*;

use crate::{element::MaxReferenceHop, reference_path::ReferencePathType, ElementFlags};

const META_BACKWARD_REFERENCES_PREFIX: &[u8] = b"refs";

pub type SlotIdx = usize;

/// Flag to indicate whether the bidirectional reference should be deleted when
/// the pointing-to item no longer exists or becomes incompatible.
pub type CascadeOnUpdate = bool;

#[derive(Clone, Encode, Decode, PartialEq, Eq, Hash)]
#[cfg_attr(not(any(feature = "full", feature = "visualize")), derive(Debug))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BidirectionalReference {
    pub forward_reference_path: ReferencePathType,
    pub backward_reference_slot: SlotIdx,
    pub cascade_on_update: CascadeOnUpdate,
    pub max_hop: MaxReferenceHop,
    pub flags: Option<ElementFlags>,
}
