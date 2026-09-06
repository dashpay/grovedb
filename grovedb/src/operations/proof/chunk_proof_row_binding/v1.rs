//! Chunk-proof row binding — **v1** (`GROVE_V4`+).
//!
//! The prover rewrites every composite row of the chunk ops — a tree of any
//! kind, or a reference — to `KVValueHashFeatureTypeWithChildHash` through the
//! same per-row binder the sum-budget window uses
//! (`GroveDb::bind_composite_row`): a Merk tree carries its child root
//! (`NULL_HASH` when empty), a non-Merk tree its own state root, a reference
//! the referenced element's value hash. Each is exactly the second input of
//! the `combine_hash` the parent committed, so the merk-level
//! `combine_hash(H(value), child_hash) == value_hash` check now covers the
//! element bytes. Item rows are left as the merk chunk emitted them; their
//! bytes are hashed by the verifier directly.
//!
//! The verifier requires that node for every tree or reference row and no
//! longer waives anything: a `KVValueHash` / `KVValueHashFeatureType` row must
//! satisfy `H(value) == value_hash` and must not deserialize as a tree or
//! reference, and a plain `KV`-family row must not either. Indexed trees
//! commit `combine_hash_three(H(value), primary_root, attestation)`, a
//! three-input hash no proof node carries, so the prover refuses to serve a
//! chunk containing one and the verifier rejects any such row rather than
//! return its metadata as verified.
//!
//! This differs from [`super::v0`] in that the prover reads storage (one child
//! Merk open per tree row, a reference resolution per reference row) and
//! mutates the ops, and the verifier rejects rows the released verifier
//! returned. Those are why it cannot apply to the released versions; see the
//! module docs in [`super`][`mod@super`].

#[cfg(feature = "minimal")]
use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
#[cfg(feature = "minimal")]
use grovedb_merk::proofs::Op;
use grovedb_merk::{
    proofs::Node,
    tree::{combine_hash, value_hash},
};

#[cfg(feature = "minimal")]
use crate::Transaction;
use crate::{Element, Error, GroveDb, PathQuery, Query};

impl GroveDb {
    /// `bind_chunk_proof_rows` v1 — see the module documentation.
    #[cfg(feature = "minimal")]
    pub(crate) fn bind_chunk_proof_rows_v1(
        &self,
        ops: &mut [Op],
        target_path: &[&[u8]],
        tx: &Transaction,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        for op in ops.iter_mut() {
            let node = match op {
                Op::Push(node) | Op::PushInverted(node) => node,
                _ => continue,
            };
            cost_return_on_error!(
                &mut cost,
                self.bind_composite_row(
                    node,
                    target_path,
                    tx,
                    grove_version,
                    "trunk/branch chunk proof: the row",
                )
            );
        }

        Ok(()).wrap_with_cost(cost)
    }

    /// `check_chunk_proof_row` v1 — see the module documentation.
    pub(crate) fn check_chunk_proof_row_v1(
        node: &Node,
        key: &[u8],
        value: &[u8],
        element: &Element,
    ) -> Result<(), Error> {
        let reject = |message: String| {
            Err(Error::InvalidProof(
                PathQuery::new_unsized(Vec::new(), Query::default()),
                message,
            ))
        };

        // No proof node can carry an indexed tree's three-input binding, so
        // its metadata can never be returned as verified from a chunk proof.
        if element.is_indexed_tree() {
            return reject(format!(
                "trunk/branch proof row at key {} is an indexed tree, whose three-input \
                 binding no proof node can carry; its metadata cannot be verified",
                hex::encode(key),
            ));
        }

        let composite = element.is_any_tree() || element.is_reference();

        match node {
            Node::KVValueHashFeatureTypeWithChildHash(_, _, node_value_hash, _, child_hash) => {
                let element_vh = value_hash(value).value().to_owned();
                let computed_vh = combine_hash(&element_vh, child_hash).value().to_owned();
                if computed_vh != *node_value_hash {
                    return reject(format!(
                        "trunk/branch proof value/child hash mismatch at key {}: \
                         combine_hash(H(value), child_hash) = {} but value_hash = {}",
                        hex::encode(key),
                        hex::encode(computed_vh),
                        hex::encode(node_value_hash),
                    ));
                }
                // The composition checked out, so the row is what its Merk
                // committed. Only a tree or reference is ever committed
                // through a combine_hash; anything else here is a prover
                // bug, and its bytes must not be returned as an element of
                // a shape the row cannot have.
                if !composite {
                    return reject(format!(
                        "trunk/branch proof row at key {} carries a child hash but is neither \
                         a tree nor a reference",
                        hex::encode(key),
                    ));
                }
                Ok(())
            }
            Node::KVValueHash(_, _, node_value_hash)
            | Node::KVValueHashFeatureType(_, _, node_value_hash, _) => {
                if composite {
                    // The committed value_hash of a tree or reference is a
                    // combine_hash the value bytes alone cannot reproduce;
                    // without the child hash the metadata would be unbound.
                    return reject(format!(
                        "trunk/branch proof composite row at key {} must carry its child hash \
                         (KVValueHashFeatureTypeWithChildHash); the tree metadata would \
                         otherwise be unbound",
                        hex::encode(key),
                    ));
                }
                let computed_vh = value_hash(value).value().to_owned();
                if computed_vh != *node_value_hash {
                    return reject(format!(
                        "trunk/branch proof value hash mismatch at key {}: H(value) = {} but \
                         embedded value_hash = {}",
                        hex::encode(key),
                        hex::encode(computed_vh),
                        hex::encode(node_value_hash),
                    ));
                }
                Ok(())
            }
            Node::KVRefValueHash(..)
            | Node::KVRefValueHashCount(..)
            | Node::KVRefValueHashSum(..)
            | Node::KVRefValueHashCountSum(..) => reject(format!(
                "trunk/branch proof contains unexpected KVRefValueHash node at key {}",
                hex::encode(key),
            )),
            // KV, KVCount, KVSum, KVCountSum: the merk verifier hashes the
            // value bytes itself, which binds an item. A tree or reference
            // is never committed that way, so one here is a forgery attempt
            // that happens to have survived the root check — or a prover
            // bug; either way its metadata must not be returned.
            _ => {
                if composite {
                    return reject(format!(
                        "trunk/branch proof composite row at key {} must carry its child hash \
                         (KVValueHashFeatureTypeWithChildHash), not a plain key/value node",
                        hex::encode(key),
                    ));
                }
                Ok(())
            }
        }
    }
}
