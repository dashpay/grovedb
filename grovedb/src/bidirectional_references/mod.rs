//! Bidirectional references management module.
//!
//! The type definitions ([`BidirectionalReference`] and friends) live in the
//! `grovedb-element` crate because the `Element` enum embeds them; this
//! module hosts the propagation machinery: backward-reference meta storage
//! bookkeeping, hash propagation along reference chains, and cascade
//! deletion. See `adr/bidirectional_references.md`.

mod handling;

pub use grovedb_element::{BidirectionalReference, SlotIdx};
pub(crate) use handling::*;

/// Namespace inside a subtree's meta storage under which backward
/// references are recorded (extended with the target key's length and
/// bytes, then the slot index).
const META_BACKWARD_REFERENCES_PREFIX: &[u8] = b"refs";
