//! Bidirectional references management module.
//!
//! The type definitions ([`BidirectionalReference`] and friends) live in the
//! `grovedb-element` crate because the `Element` enum embeds them; this
//! module hosts the propagation machinery: backward-reference meta storage
//! bookkeeping, hash propagation along reference chains, and cascade
//! deletion. See `adr/bidirectional_references.md`.

mod handling;
pub(crate) mod semantics;

pub use grovedb_element::{BackwardReference, BidirectionalReference};
pub(crate) use handling::*;

/// Maximum Grove path depth (number of subtree levels) of any position
/// participating in a bidirectional edge — the referrer's own position and
/// its resolved target. Enforced at registration time by the shared
/// semantic core, so every later derived write (propagation rewrite,
/// cascade deletion, registration cleanup) is guaranteed to land at a
/// bounded depth. Estimation relies on this: each derived foreign-subtree
/// propagation charges up to this many ancestor updates, which would be
/// unboundable otherwise (a referrer parked arbitrarily deep would make
/// its propagation cost exceed any fixed estimate).
pub const MAX_BACKWARD_REFERENCES_GROVE_DEPTH: usize = 32;
