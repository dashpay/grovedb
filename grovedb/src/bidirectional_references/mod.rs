//! Bidirectional references management module.
//!
//! The type definitions ([`BidirectionalReference`] and friends) live in the
//! `grovedb-element` crate because the `Element` enum embeds them; this
//! module hosts the propagation machinery: backward-reference meta storage
//! bookkeeping, hash propagation along reference chains, and cascade
//! deletion. See `adr/bidirectional_references.md`.

mod handling;

pub use grovedb_element::{BackwardReference, BidirectionalReference};
pub(crate) use handling::*;
