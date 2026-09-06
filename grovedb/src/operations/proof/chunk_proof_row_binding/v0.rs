//! Chunk-proof row binding — **v0** (released behaviour,
//! `GROVE_V1`..`GROVE_V3`).
//!
//! The prover leaves the chunk ops exactly as the merk `trunk_query` /
//! `branch_query` emitted them: an item as `KV`, a tree or reference as a bare
//! `KVValueHash` (or `KVValueHashFeatureType` in a `Provable*` host), which
//! hashes only `(key, value_hash)`.
//!
//! The verifier checks `H(value) == value_hash` for those two node forms but
//! **waives the check when the value deserializes as a tree**, because a
//! tree's committed `value_hash` is `combine_hash(H(value), child_root)` and
//! the child root travels nowhere in the proof. The waiver is the hole: the
//! merk root check only ever covers `value_hash`, so a prover can serve any
//! tree-family element — a different type, an inflated count or sum, a
//! phantom root key — or disguise an item as a tree, and the row is returned
//! as verified. A reference row is not waived, so an honest chunk that
//! contains one never verifies under these versions.
//!
//! This is a **known soundness gap**, not an oversight to fix in place: it is
//! preserved here because `GROVE_V3` is live — closing it changes both an
//! accepted/rejected outcome and the prover's tracked cost. [`super::v1`]
//! closes it from `GROVE_V4` onward.
//!
//! Deliberately takes the same arguments as [`super::v1`] and ignores those it
//! does not need, so the dispatch in [`super`][`mod@super`] stays a plain
//! version match.

#[cfg(feature = "minimal")]
use grovedb_costs::{CostResult, CostsExt, OperationCost};
#[cfg(feature = "minimal")]
use grovedb_merk::proofs::Op;
use grovedb_merk::{
    proofs::Node,
    tree::{combine_hash, value_hash},
};

use crate::{Element, Error, GroveDb, PathQuery, Query};

impl GroveDb {
    /// `bind_chunk_proof_rows` v0 — see the module documentation.
    #[cfg(feature = "minimal")]
    pub(crate) fn bind_chunk_proof_rows_v0(_ops: &mut [Op]) -> CostResult<(), Error> {
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    /// `check_chunk_proof_row` v0 — see the module documentation.
    pub(crate) fn check_chunk_proof_row_v0(
        node: &Node,
        key: &[u8],
        value: &[u8],
        element: &Element,
    ) -> Result<(), Error> {
        match node {
            Node::KVValueHash(_, _, node_value_hash)
            | Node::KVValueHashFeatureType(_, _, node_value_hash, _) => {
                let computed_vh = value_hash(value).value().to_owned();
                if computed_vh != *node_value_hash {
                    // For tree elements, value_hash = combine_hash(H(value),
                    // child_root_hash). The child root travels nowhere in a
                    // v0 chunk, so the check is waived — leaving the tree's
                    // metadata unbound (see the module docs).
                    if !element.is_any_tree() {
                        return Err(Error::InvalidProof(
                            PathQuery::new_unsized(Vec::new(), Query::default()),
                            format!(
                                "trunk/branch proof value hash mismatch at key {}: H(value) = \
                                 {} but embedded value_hash = {}",
                                hex::encode(key),
                                hex::encode(computed_vh),
                                hex::encode(node_value_hash),
                            ),
                        ));
                    }
                }
                Ok(())
            }
            Node::KVValueHashFeatureTypeWithChildHash(_, _, node_value_hash, _, child_hash) => {
                let element_vh = value_hash(value).value().to_owned();
                let computed_vh = combine_hash(&element_vh, child_hash).value().to_owned();
                if computed_vh != *node_value_hash {
                    return Err(Error::InvalidProof(
                        PathQuery::new_unsized(Vec::new(), Query::default()),
                        format!(
                            "trunk/branch proof value/child hash mismatch at key {}: \
                             combine_hash(H(value), child_hash) = {} but value_hash = {}",
                            hex::encode(key),
                            hex::encode(computed_vh),
                            hex::encode(node_value_hash),
                        ),
                    ));
                }
                Ok(())
            }
            Node::KVRefValueHash(..)
            | Node::KVRefValueHashCount(..)
            | Node::KVRefValueHashSum(..)
            | Node::KVRefValueHashCountSum(..) => {
                // KVRefValueHash{,Count,Sum,CountSum} carries an opaque
                // node_value_hash that cannot be recomputed from the value
                // bytes alone — the hash is `combine_hash(node_value_hash,
                // value_hash(referenced_value))`, and the verifier never
                // gets to see the referenced_value at this layer. Without
                // this rejection, a forged value could ride along in a
                // KVRefValueHashSum / KVRefValueHashCountSum trunk/branch
                // node while the merk-level hash chain still appears
                // valid, because the embedded opaque hash is treated as
                // authoritative. These node types should never appear in
                // trunk/branch chunk proofs.
                Err(Error::InvalidProof(
                    PathQuery::new_unsized(Vec::new(), Query::default()),
                    format!(
                        "trunk/branch proof contains unexpected KVRefValueHash node at key {}",
                        hex::encode(key),
                    ),
                ))
            }
            // KV, KVCount, KVSum, KVCountSum: the value is used directly in
            // the hash computation — bound.
            _ => Ok(()),
        }
    }
}
