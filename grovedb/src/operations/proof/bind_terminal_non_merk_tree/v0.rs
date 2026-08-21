//! `bind_terminal_non_merk_tree` — **v0** (released behaviour,
//! `GROVE_V1`..`GROVE_V3`).
//!
//! Does nothing. The proof node for a terminally-reported non-Merk tree is left
//! exactly as the prover emitted it — a bare `Node::KVValueHash`, which hashes
//! only `(key, value_hash)` and leaves the serialized element bytes unbound.
//!
//! This is a **known soundness gap**, not an oversight to fix in place: a
//! prover can serve a forged entry count alongside the genuine `value_hash` and
//! still reconstruct the correct root hash. It is preserved here because
//! `GROVE_V3` is live — closing it changes both an accepted/rejected outcome
//! and the prover's tracked cost, so nodes carrying the fix would diverge from
//! nodes that do not. [`super::v1`] closes it from `GROVE_V4` onward; the hole
//! shuts when that protocol version activates.
//!
//! Deliberately takes the same arguments as [`super::v1`] and ignores them, so
//! the dispatch in [`super`][`mod@super`] stays a plain version match.

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::proofs::Node;

use crate::{Element, Error, GroveDb, Transaction};

impl GroveDb {
    /// `bind_terminal_non_merk_tree` v0 — see the module documentation.
    pub(crate) fn bind_terminal_non_merk_tree_v0(
        &self,
        _node: &mut Node,
        _element: &Element,
        _parent_path: &[&[u8]],
        _tx: &Transaction,
    ) -> CostResult<(), Error> {
        Ok(()).wrap_with_cost(OperationCost::default())
    }
}
