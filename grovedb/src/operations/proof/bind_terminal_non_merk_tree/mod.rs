//! `bind_terminal_non_merk_tree` — versioned dispatch.
//!
//! Binds the serialized element bytes of a **terminally-reported non-Merk
//! tree** — `CommitmentTree`, `MmrTree`, `BulkAppendTree`,
//! `DenseAppendOnlyFixedSizeTree` — to the `value_hash` its parent Merk
//! commits to. "Terminal" means the query targets the tree element itself and
//! the prover emits no lower layer, so there is no child layer to chain
//! through.
//!
//! These four types have no child Merk. Their parent entry is written by
//! `insert_subtree`, which commits `combine_hash(H(value), state_root)` — the
//! same two-input form that `Node::KVValueHashFeatureTypeWithChildHash` is
//! verified with. Carrying the state root in the node is therefore enough for
//! the merk verifier to close the loop; no new proof node type is needed.
//!
//! Whether it is carried is **consensus-critical** and version-gated on
//! `proof.terminal_non_merk_tree_child_hash`:
//!
//! * **[v0]** — released behaviour, `GROVE_V1`..`GROVE_V3`. The node is left
//!   exactly as the prover emitted it (a bare `Node::KVValueHash`), which
//!   hashes only `(key, value_hash)`. The element bytes are unbound: a prover
//!   can serve a forged entry count — an inflated or deflated `CommitmentTree`
//!   `total_count`, a different MMR size — alongside the genuine `value_hash`
//!   and still reconstruct the correct root hash.
//! * **[v1]** — `GROVE_V4`+. The tree's state root is derived from storage and
//!   the node is rewritten to `KVValueHashFeatureTypeWithChildHash`, so the
//!   merk verifier's `combine_hash(H(value), child_hash) == value_hash` check
//!   catches forged bytes. The matching verifier gate in
//!   [`verify`](super::verify) requires the node from the same version.
//!
//! The split cannot be applied unconditionally on two counts: it flips an
//! accepted/rejected outcome, and deriving the state root costs the prover
//! storage reads and hash calls that the released versions never paid — cost
//! feeds fees. See `grovedb-version`'s `v4.rs` for the landing-zone rationale.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::proofs::Node;
use grovedb_version::version::GroveVersion;

use crate::{Element, Error, GroveDb, Transaction};

impl GroveDb {
    /// Bind a terminally-reported non-Merk tree's element bytes to the
    /// parent-committed `value_hash`, if the grove version calls for it.
    ///
    /// `node` is the proof node standing for the tree element; `element` is
    /// that node's already-deserialized (and `NonCounted`-unwrapped) value, and
    /// must be one of the four non-Merk tree types. `parent_path` is the path
    /// of the Merk holding the element — the tree's own data lives one level
    /// below, under the node's key, which the versioned implementations append
    /// themselves.
    pub(crate) fn bind_terminal_non_merk_tree(
        &self,
        node: &mut Node,
        element: &Element,
        parent_path: &[&[u8]],
        tx: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .proof
            .terminal_non_merk_tree_child_hash
        {
            0 => self.bind_terminal_non_merk_tree_v0(node, element, parent_path, tx),
            1 => self.bind_terminal_non_merk_tree_v1(node, element, parent_path, tx),
            version => Err(Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "bind_terminal_non_merk_tree".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }
}
