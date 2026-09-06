//! Trunk / branch chunk-proof composite-row binding — versioned dispatch
//! (#859).
//!
//! A merk chunk proof (`prove_trunk_chunk` / `prove_branch_chunk`) binds an
//! item row through `KV`, whose value bytes the verifier hashes itself, but a
//! tree or reference row through `KVValueHash` / `KVValueHashFeatureType`,
//! whose embedded `value_hash` the merk verifier treats as opaque. That hash is
//! `combine_hash(H(value), child_root)` for a tree and
//! `combine_hash(H(value), H(referenced))` for a reference — a composition the
//! value bytes alone cannot reproduce. Checking the target root and the
//! ancestor chain therefore says nothing about the returned tree element's own
//! metadata: its type, aggregate count or sum, and root key.
//!
//! These functions share one gate, `proof.chunk_proof_row_binding`, so prover
//! and verifier move together at the protocol boundary:
//!
//! * [`GroveDb::start_chunk_proof_transaction`] pins V4 generation to one
//!   snapshot, including the chunk, row bindings, and ancestor layers.
//! * [`GroveDb::bind_chunk_proof_rows`] (prover) rewrites every composite row
//!   of the chunk ops to `KVValueHashFeatureTypeWithChildHash`.
//! * [`GroveDb::check_chunk_proof_row`] (verifier) decides, per extracted
//!   row, whether the node form binds the element bytes it carries.
//!
//! * **[v0]** — released behaviour, `GROVE_V1`..`GROVE_V3`. The prover leaves
//!   the rows as the merk chunk emitted them and the verifier waives the
//!   `H(value) == value_hash` check for any row that deserializes as a tree.
//!   The returned tree metadata is unbound: a prover can substitute any
//!   tree-family element, or disguise an item as a tree, under a genuine root
//!   hash. Reference rows are not waived and never verify.
//! * **[v1]** — `GROVE_V4`+. Every tree or reference row carries its child
//!   hash and the verifier requires it, so the merk-level
//!   `combine_hash(H(value), child_hash) == value_hash` check closes the loop.
//!   Indexed trees commit a three-input hash no proof node can carry: the
//!   prover refuses to serve a chunk containing one and the verifier rejects
//!   any such row rather than return its metadata as verified.
//!
//! The split cannot be applied unconditionally: it flips an accepted/rejected
//! outcome, and deriving each child root costs the prover storage reads and
//! hash calls the released path never paid — cost feeds fees. See
//! `grovedb-version`'s `v4.rs` for the landing-zone rationale.
//!
//! [v0]: self::v0
//! [v1]: self::v1

mod v0;
mod v1;

#[cfg(feature = "minimal")]
use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_merk::proofs::Node;
#[cfg(feature = "minimal")]
use grovedb_merk::proofs::Op;
use grovedb_version::version::GroveVersion;

#[cfg(feature = "minimal")]
use crate::Transaction;
use crate::{Element, Error, GroveDb};

impl GroveDb {
    /// Use one committed state for the chunk and every storage read needed to
    /// bind it. A plain transaction can pair a pre-commit parent value hash
    /// with a post-commit child root or reference target. Preserve the released
    /// transaction behavior when composite-row binding is disabled.
    #[cfg(feature = "minimal")]
    pub(crate) fn start_chunk_proof_transaction(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<Transaction<'_>, Error> {
        match grove_version
            .grovedb_versions
            .operations
            .proof
            .chunk_proof_row_binding
        {
            0 => Ok(self.start_transaction()),
            1 => Ok(self.start_snapshot_read_transaction()),
            version => Err(Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "start_chunk_proof_transaction".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            )),
        }
    }

    /// Bind every composite row of a trunk / branch chunk proof to the
    /// `value_hash` its Merk commits to, if the grove version calls for it.
    ///
    /// `ops` are the chunk ops as the merk `trunk_query` / `branch_query`
    /// emitted them; `target_path` is the path of the Merk they were taken
    /// from, so a tree row's own data lives one level below under the row's
    /// key.
    #[cfg(feature = "minimal")]
    pub(crate) fn bind_chunk_proof_rows(
        &self,
        ops: &mut [Op],
        target_path: &[&[u8]],
        tx: &Transaction,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .proof
            .chunk_proof_row_binding
        {
            0 => Self::bind_chunk_proof_rows_v0(ops),
            1 => self.bind_chunk_proof_rows_v1(ops, target_path, tx, grove_version),
            version => Err(Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "bind_chunk_proof_rows".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }

    /// Decide whether a trunk / branch chunk-proof row binds the element
    /// bytes it carries, per the grove version.
    ///
    /// `node` is the proof node the row was extracted from, `key` / `value`
    /// its key and value bytes, and `element` the already-deserialized value.
    /// Runs after the merk root hash (and, for a trunk, the ancestor chain)
    /// has been checked, so a row only reaches here under a genuine root.
    pub(crate) fn check_chunk_proof_row(
        node: &Node,
        key: &[u8],
        value: &[u8],
        element: &Element,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .proof
            .chunk_proof_row_binding
        {
            0 => Self::check_chunk_proof_row_v0(node, key, value, element),
            1 => Self::check_chunk_proof_row_v1(node, key, value, element),
            version => Err(Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "check_chunk_proof_row".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            )),
        }
    }
}
