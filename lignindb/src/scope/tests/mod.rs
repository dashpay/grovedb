//! The tests of the scopes' own structures. The tests on a store are a part
//! of the transaction's tests, whose store and model they use
//! (`scope/tests/engine.rs`, named by `txn/tests/mod.rs`).

#[cfg(not(loom))]
mod claims;
mod model;
#[cfg(not(loom))]
mod set;
