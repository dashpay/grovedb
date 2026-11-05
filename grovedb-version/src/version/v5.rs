//! Grove protocol version 5.
//!
//! Introduced identical to [`GROVE_V4`](super::v4::GROVE_V4) apart from its
//! `protocol_version`. No slot is flipped yet.
//!
//! It exists so that changes made after GroveDB 6.1.1 have somewhere to
//! land. GROVE_V4 as 6.1.1 shipped it is what Dash Platform 5.0 (protocol
//! version 14) runs, so a change that alters an accepted/rejected outcome, a
//! committed root hash, a tracked cost or a cost estimate cannot be made to
//! V4: nodes carrying it would diverge from nodes that do not. Such a change
//! bumps the relevant slot here and branches on it, leaving V1..V4
//! untouched.
//!
//! When adding a gate, list it here and say what changes in the slot
//! comment, the way v4.rs does. `GroveVersion::latest()` returns the last
//! entry of `GROVE_VERSIONS`, so it resolves to this version: anything
//! defaulting to "latest" (tests, benchmarks, tools) exercises every gate
//! listed here, and a test about V4 behaviour names `GROVE_V4`.

use super::{v4::GROVE_V4, GroveVersion};

pub const GROVE_V5: GroveVersion = GroveVersion {
    protocol_version: 5,
    ..GROVE_V4
};
