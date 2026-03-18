//! Proof operations

#[cfg(feature = "minimal")]
mod generate;
/// Utility functions for proof display and conversion.
pub mod util;
mod verify;

use std::{collections::BTreeMap, fmt};

/// Maximum allowed recursion depth for proof generation and verification.
///
/// This limit prevents stack overflow from deeply nested subqueries or
/// maliciously crafted proofs with excessive `LayerProof` nesting. A depth
/// of 128 is well beyond any practical GroveDB tree hierarchy while still
/// fitting comfortably within typical stack sizes.
pub const MAX_PROOF_DEPTH: usize = 128;

use bincode::{
    de::{BorrowDecoder, Decoder as BincodeDecoder},
    error::DecodeError,
    BorrowDecode, Decode, Encode,
};
use grovedb_bulk_append_tree::BulkAppendTreeProof;
use grovedb_dense_fixed_sized_merkle_tree::DenseTreeProof;
use grovedb_merk::{
    proofs::{
        query::{Key, VerifyOptions},
        Decoder as MerkDecoder, Node, Op,
    },
    CryptoHash,
};
use grovedb_merkle_mountain_range::MmrTreeProof;
use grovedb_version::version::GroveVersion;

use crate::{
    operations::proof::util::{element_hex_to_ascii, hex_to_ascii, ProvedPathKeyValues},
    query_result_type::PathKeyOptionalElementTrio,
    Error, GroveDb, PathQuery,
};

/// Options controlling proof generation behavior.
///
/// # Security note (V0 proofs only)
///
/// In [`GroveDBProofV0`], `ProveOptions` is serialized as part of the proof
/// via `bincode::Encode` / `bincode::Decode`. During verification the
/// verifier deserializes these options from the proof bytes, which means
/// **the values come from the (potentially untrusted) prover**. A malicious
/// prover could craft a proof with `decrease_limit_on_empty_sub_query_result`
/// set to `true` even when the original query used `false`, causing the
/// verifier to consume its result limit faster and therefore return fewer
/// results than actually exist.
///
/// [`GroveDBProofV1`] does **not** embed `ProveOptions`. The verifier uses
/// [`ProveOptions::default()`] instead, closing this attack vector.
#[derive(Debug, Clone, Copy, Encode, Decode)]
pub struct ProveOptions {
    /// This tells the proof system to decrease the available limit of the query
    /// by 1 in the case of empty subtrees. Generally this should be set to
    /// true. The case where this could be set to false is if there is a
    /// known structure where we know that there are only a few empty
    /// subtrees.
    ///
    /// # Warning
    ///
    /// Be very careful: if this is set to `false` then you must be sure that
    /// the sub queries do not match many trees. Otherwise you could crash the
    /// system as the proof system goes through millions of subtrees and
    /// eventually runs out of memory.
    ///
    /// # Security note (V0 proofs only)
    ///
    /// In V0 proofs this field is embedded in the serialized proof and
    /// deserialized by the verifier. Because it originates from the prover, it
    /// must be treated as **untrusted input**. V1 proofs do not embed this
    /// field; the verifier uses [`ProveOptions::default()`] instead.
    pub decrease_limit_on_empty_sub_query_result: bool,
}

impl fmt::Display for ProveOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ProveOptions {{ decrease_limit_on_empty_sub_query_result: {} }}",
            self.decrease_limit_on_empty_sub_query_result
        )
    }
}

impl Default for ProveOptions {
    fn default() -> Self {
        ProveOptions {
            decrease_limit_on_empty_sub_query_result: true,
        }
    }
}

/// A single layer of a legacy (v0) GroveDB proof containing only merk proofs.
///
/// Uses a custom `Decode` implementation that enforces [`MAX_PROOF_DEPTH`]
/// during deserialization to prevent stack overflow from deeply nested proofs.
#[derive(Encode)]
pub struct MerkOnlyLayerProof {
    /// Encoded merk proof bytes for this layer.
    pub merk_proof: Vec<u8>,
    /// Proofs for child subtrees keyed by their key in the parent tree.
    pub lower_layers: BTreeMap<Key, MerkOnlyLayerProof>,
}

impl MerkOnlyLayerProof {
    fn decode_with_depth<D: BincodeDecoder>(
        decoder: &mut D,
        depth: usize,
    ) -> Result<Self, DecodeError> {
        if depth > MAX_PROOF_DEPTH {
            return Err(DecodeError::Other(
                "proof layer nesting depth exceeded maximum",
            ));
        }
        let merk_proof = Vec::<u8>::decode(decoder)?;
        let len = u64::decode(decoder)? as usize;
        if len > MAX_PROOF_DEPTH {
            return Err(DecodeError::Other("proof layer has too many children"));
        }
        let mut lower_layers = BTreeMap::new();
        for _ in 0..len {
            let key = Key::decode(decoder)?;
            let value = Self::decode_with_depth(decoder, depth + 1)?;
            lower_layers.insert(key, value);
        }
        Ok(MerkOnlyLayerProof {
            merk_proof,
            lower_layers,
        })
    }

    fn borrow_decode_with_depth<'de, D: BorrowDecoder<'de>>(
        decoder: &mut D,
        depth: usize,
    ) -> Result<Self, DecodeError> {
        if depth > MAX_PROOF_DEPTH {
            return Err(DecodeError::Other(
                "proof layer nesting depth exceeded maximum",
            ));
        }
        let merk_proof = Vec::<u8>::borrow_decode(decoder)?;
        let len = u64::borrow_decode(decoder)? as usize;
        if len > MAX_PROOF_DEPTH {
            return Err(DecodeError::Other("proof layer has too many children"));
        }
        let mut lower_layers = BTreeMap::new();
        for _ in 0..len {
            let key = Key::borrow_decode(decoder)?;
            let value = Self::borrow_decode_with_depth(decoder, depth + 1)?;
            lower_layers.insert(key, value);
        }
        Ok(MerkOnlyLayerProof {
            merk_proof,
            lower_layers,
        })
    }
}

impl<Context> Decode<Context> for MerkOnlyLayerProof {
    fn decode<D: BincodeDecoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        Self::decode_with_depth(decoder, 0)
    }
}

impl<'de, Context> BorrowDecode<'de, Context> for MerkOnlyLayerProof {
    fn borrow_decode<D: BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::borrow_decode_with_depth(decoder, 0)
    }
}

/// Encoded proof bytes for different tree backing store types.
#[derive(Encode, Decode)]
pub enum ProofBytes {
    /// Merk (Merkle AVL) tree proof bytes.
    Merk(Vec<u8>),
    /// Merkle Mountain Range tree proof bytes.
    MMR(Vec<u8>),
    /// Bulk-append tree proof bytes.
    BulkAppendTree(Vec<u8>),
    /// Dense fixed-size Merkle tree proof bytes.
    DenseTree(Vec<u8>),
    /// CommitmentTree proof: `sinsemilla_root (32 bytes) || bulk_append_proof`.
    /// Binds the Orchard anchor to the GroveDB root hash.
    CommitmentTree(Vec<u8>),
}

/// A single layer of a v1 GroveDB proof supporting multiple tree types.
///
/// Uses a custom `Decode` implementation that enforces [`MAX_PROOF_DEPTH`]
/// during deserialization to prevent stack overflow from deeply nested proofs.
#[derive(Encode)]
pub struct LayerProof {
    /// Proof bytes for this layer (may be any supported tree type).
    pub merk_proof: ProofBytes,
    /// Proofs for child subtrees keyed by their key in the parent tree.
    pub lower_layers: BTreeMap<Key, LayerProof>,
}

impl LayerProof {
    fn decode_with_depth<D: BincodeDecoder>(
        decoder: &mut D,
        depth: usize,
    ) -> Result<Self, DecodeError> {
        if depth > MAX_PROOF_DEPTH {
            return Err(DecodeError::Other(
                "proof layer nesting depth exceeded maximum",
            ));
        }
        let merk_proof = ProofBytes::decode(decoder)?;
        let len = u64::decode(decoder)? as usize;
        if len > MAX_PROOF_DEPTH {
            return Err(DecodeError::Other("proof layer has too many children"));
        }
        let mut lower_layers = BTreeMap::new();
        for _ in 0..len {
            let key = Key::decode(decoder)?;
            let value = Self::decode_with_depth(decoder, depth + 1)?;
            lower_layers.insert(key, value);
        }
        Ok(LayerProof {
            merk_proof,
            lower_layers,
        })
    }

    fn borrow_decode_with_depth<'de, D: BorrowDecoder<'de>>(
        decoder: &mut D,
        depth: usize,
    ) -> Result<Self, DecodeError> {
        if depth > MAX_PROOF_DEPTH {
            return Err(DecodeError::Other(
                "proof layer nesting depth exceeded maximum",
            ));
        }
        let merk_proof = ProofBytes::borrow_decode(decoder)?;
        let len = u64::borrow_decode(decoder)? as usize;
        if len > MAX_PROOF_DEPTH {
            return Err(DecodeError::Other("proof layer has too many children"));
        }
        let mut lower_layers = BTreeMap::new();
        for _ in 0..len {
            let key = Key::borrow_decode(decoder)?;
            let value = Self::borrow_decode_with_depth(decoder, depth + 1)?;
            lower_layers.insert(key, value);
        }
        Ok(LayerProof {
            merk_proof,
            lower_layers,
        })
    }
}

impl<Context> Decode<Context> for LayerProof {
    fn decode<D: BincodeDecoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        Self::decode_with_depth(decoder, 0)
    }
}

impl<'de, Context> BorrowDecode<'de, Context> for LayerProof {
    fn borrow_decode<D: BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::borrow_decode_with_depth(decoder, 0)
    }
}

/// A versioned GroveDB proof that can be verified against a path query.
#[derive(Encode, Decode)]
pub enum GroveDBProof {
    /// Legacy proof format using only merk proofs.
    V0(GroveDBProofV0),
    /// Current proof format supporting multiple tree backing store types.
    V1(GroveDBProofV1),
}

impl GroveDBProof {
    /// Verifies a query with options using the proof and returns the root hash
    /// and the query result.
    pub fn verify_with_options(
        &self,
        query: &PathQuery,
        options: VerifyOptions,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        GroveDb::verify_proof_internal(self, query, options, grove_version)
            .map(|(root_hash, _, results)| (root_hash, results))
    }

    /// Verifies a raw query using the proof and returns the root hash and the
    /// query result.
    pub fn verify_raw(
        &self,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, ProvedPathKeyValues), Error> {
        GroveDb::verify_proof_raw_internal(
            self,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: true,
            },
            grove_version,
        )
        .map(|(root_hash, _, results)| (root_hash, results))
    }

    /// Verifies a query using the proof and returns the root hash and the query
    /// result. Rejects proofs containing extra data beyond what the query
    /// requires (succinctness check enabled).
    pub fn verify(
        &self,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        GroveDb::verify_proof_internal(
            self,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: true,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .map(|(root_hash, _, results)| (root_hash, results))
    }

    /// Verifies a query with an absence proof and returns the root hash and the
    /// query result. Rejects proofs containing extra data beyond what the
    /// query requires (succinctness check enabled).
    pub fn verify_with_absence_proof(
        &self,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        GroveDb::verify_proof_internal(
            self,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: true,
                verify_proof_succinctness: true,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .map(|(root_hash, _, results)| (root_hash, results))
    }

    /// Verifies a subset query using the proof and returns the root hash and
    /// the query result.
    pub fn verify_subset(
        &self,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        GroveDb::verify_proof_internal(
            self,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: false,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .map(|(root_hash, _, results)| (root_hash, results))
    }

    /// Verifies a subset query with an absence proof using the proof and
    /// returns the root hash and the query result.
    pub fn verify_subset_with_absence_proof(
        &self,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(CryptoHash, Vec<PathKeyOptionalElementTrio>), Error> {
        GroveDb::verify_proof_internal(
            self,
            query,
            VerifyOptions {
                absence_proofs_for_non_existing_searched_keys: true,
                verify_proof_succinctness: false,
                include_empty_trees_in_result: false,
            },
            grove_version,
        )
        .map(|(root_hash, _, results)| (root_hash, results))
    }

    /// Checks whether a key exists as a boundary element (`KVDigest` or
    /// `KVDigestCount`) at the specified path in this proof.
    ///
    /// Boundary elements prove a key exists in the tree without revealing
    /// its value. They appear in exclusive range queries (e.g.
    /// `RangeAfter(10)`) where the boundary key anchors the range but is
    /// not part of the result set.
    ///
    /// **Important:** This performs a syntactic scan of proof nodes. It
    /// provides no cryptographic guarantee on its own — the proof should
    /// be verified against a trusted root hash first.
    ///
    /// The `path` identifies which layer of the proof to inspect, and
    /// `key` is the boundary key to look for.
    pub fn key_exists_as_boundary(&self, path: &[&[u8]], key: &[u8]) -> Result<bool, Error> {
        match self {
            GroveDBProof::V0(v0) => Self::find_boundary_in_merk_layer(&v0.root_layer, path, 0, key),
            GroveDBProof::V1(v1) => Self::find_boundary_in_layer(&v1.root_layer, path, 0, key),
        }
    }

    /// Returns all boundary keys (`KVDigest` and `KVDigestCount` nodes)
    /// found at the specified path in this proof.
    ///
    /// **Important:** This performs a syntactic scan of proof nodes. It
    /// provides no cryptographic guarantee on its own — the proof should
    /// be verified against a trusted root hash first.
    pub fn boundaries(&self, path: &[&[u8]]) -> Result<Vec<Vec<u8>>, Error> {
        match self {
            GroveDBProof::V0(v0) => Self::boundaries_in_merk_layer(&v0.root_layer, path, 0),
            GroveDBProof::V1(v1) => Self::boundaries_in_layer(&v1.root_layer, path, 0),
        }
    }

    fn boundaries_in_merk_layer(
        layer: &MerkOnlyLayerProof,
        path: &[&[u8]],
        depth: usize,
    ) -> Result<Vec<Vec<u8>>, Error> {
        if depth == path.len() {
            return grovedb_merk::proofs::query::boundaries_in_proof(&layer.merk_proof)
                .map_err(Into::into);
        }
        let segment = path[depth];
        match layer.lower_layers.get(segment) {
            Some(child_layer) => Self::boundaries_in_merk_layer(child_layer, path, depth + 1),
            None => Err(Error::InvalidInput("path segment not found in proof layer")),
        }
    }

    fn boundaries_in_layer(
        layer: &LayerProof,
        path: &[&[u8]],
        depth: usize,
    ) -> Result<Vec<Vec<u8>>, Error> {
        if depth == path.len() {
            let merk_bytes = match &layer.merk_proof {
                ProofBytes::Merk(bytes) => bytes,
                _ => {
                    return Err(Error::NotSupported(
                        "boundary check only supported for merk proofs".to_string(),
                    ))
                }
            };
            return grovedb_merk::proofs::query::boundaries_in_proof(merk_bytes)
                .map_err(Into::into);
        }
        let segment = path[depth];
        match layer.lower_layers.get(segment) {
            Some(child_layer) => Self::boundaries_in_layer(child_layer, path, depth + 1),
            None => Err(Error::InvalidInput("path segment not found in proof layer")),
        }
    }

    fn find_boundary_in_merk_layer(
        layer: &MerkOnlyLayerProof,
        path: &[&[u8]],
        depth: usize,
        key: &[u8],
    ) -> Result<bool, Error> {
        if depth == path.len() {
            return grovedb_merk::proofs::query::key_exists_as_boundary_in_proof(
                &layer.merk_proof,
                key,
            )
            .map_err(Into::into);
        }
        let segment = path[depth];
        match layer.lower_layers.get(segment) {
            Some(child_layer) => {
                Self::find_boundary_in_merk_layer(child_layer, path, depth + 1, key)
            }
            None => Err(Error::InvalidInput("path segment not found in proof layer")),
        }
    }

    fn find_boundary_in_layer(
        layer: &LayerProof,
        path: &[&[u8]],
        depth: usize,
        key: &[u8],
    ) -> Result<bool, Error> {
        if depth == path.len() {
            let merk_bytes = match &layer.merk_proof {
                ProofBytes::Merk(bytes) => bytes,
                _ => {
                    return Err(Error::NotSupported(
                        "boundary check only supported for merk proofs".to_string(),
                    ))
                }
            };
            return grovedb_merk::proofs::query::key_exists_as_boundary_in_proof(merk_bytes, key)
                .map_err(Into::into);
        }
        let segment = path[depth];
        match layer.lower_layers.get(segment) {
            Some(child_layer) => Self::find_boundary_in_layer(child_layer, path, depth + 1, key),
            None => Err(Error::InvalidInput("path segment not found in proof layer")),
        }
    }
}

/// Legacy (v0) GroveDB proof containing only merk layer proofs.
#[derive(Encode, Decode)]
pub struct GroveDBProofV0 {
    /// The root layer proof for the top-level tree.
    pub root_layer: MerkOnlyLayerProof,
    /// Options that were used when generating this proof.
    pub prove_options: ProveOptions,
}

/// Current (v1) GroveDB proof supporting multiple tree backing store types.
///
/// Unlike [`GroveDBProofV0`], V1 proofs do **not** embed [`ProveOptions`].
/// The verifier uses [`ProveOptions::default()`] instead of trusting
/// prover-supplied options, which closes the result-truncation attack
/// vector described in the [`ProveOptions`] security note.
#[derive(Encode, Decode)]
pub struct GroveDBProofV1 {
    /// The root layer proof for the top-level tree.
    pub root_layer: LayerProof,
}

impl fmt::Display for MerkOnlyLayerProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "LayerProof {{")?;
        writeln!(f, "  merk_proof: {}", decode_merk_proof(&self.merk_proof)?)?;
        if !self.lower_layers.is_empty() {
            writeln!(f, "  lower_layers: {{")?;
            for (key, layer_proof) in &self.lower_layers {
                writeln!(f, "    {} => {{", hex_to_ascii(key))?;
                for line in format!("{}", layer_proof).lines() {
                    writeln!(f, "      {}", line)?;
                }
                writeln!(f, "    }}")?;
            }
            writeln!(f, "  }}")?;
        }
        write!(f, "}}")
    }
}

impl fmt::Display for GroveDBProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GroveDBProof::V0(proof) => write!(f, "{}", proof),
            GroveDBProof::V1(proof) => write!(f, "{}", proof),
        }
    }
}

impl fmt::Display for GroveDBProofV0 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "GroveDBProofV0 {{")?;
        for line in format!("{}", self.root_layer).lines() {
            writeln!(f, "  {}", line)?;
        }
        write!(f, "}}")
    }
}

impl fmt::Display for ProofBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProofBytes::Merk(bytes) => {
                write!(f, "Merk({})", decode_merk_proof(bytes)?)
            }
            ProofBytes::MMR(bytes) => {
                write!(f, "MMR({})", decode_mmr_proof(bytes))
            }
            ProofBytes::BulkAppendTree(bytes) => {
                write!(f, "BulkAppendTree({})", decode_bulk_append_proof(bytes))
            }
            ProofBytes::DenseTree(bytes) => {
                write!(f, "DenseTree({})", decode_dense_proof(bytes))
            }
            ProofBytes::CommitmentTree(bytes) => {
                if bytes.len() >= 32 {
                    write!(
                        f,
                        "CommitmentTree(sinsemilla={}, bulk={})",
                        hex::encode(&bytes[..32]),
                        decode_bulk_append_proof(&bytes[32..])
                    )
                } else {
                    write!(f, "CommitmentTree(<invalid: {} bytes>)", bytes.len())
                }
            }
        }
    }
}

impl fmt::Display for LayerProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "LayerProof {{")?;
        writeln!(f, "  proof: {}", self.merk_proof)?;
        if !self.lower_layers.is_empty() {
            writeln!(f, "  lower_layers: {{")?;
            for (key, layer_proof) in &self.lower_layers {
                writeln!(f, "    {} => {{", hex_to_ascii(key))?;
                for line in format!("{}", layer_proof).lines() {
                    writeln!(f, "      {}", line)?;
                }
                writeln!(f, "    }}")?;
            }
            writeln!(f, "  }}")?;
        }
        write!(f, "}}")
    }
}

impl fmt::Display for GroveDBProofV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "GroveDBProofV1 {{")?;
        for line in format!("{}", self.root_layer).lines() {
            writeln!(f, "  {}", line)?;
        }
        write!(f, "}}")
    }
}

fn decode_merk_proof(proof: &[u8]) -> Result<String, fmt::Error> {
    let mut result = String::new();
    let ops = MerkDecoder::new(proof);

    for (i, op) in ops.enumerate() {
        match op {
            Ok(op) => {
                result.push_str(&format!("\n    {}: {}", i, op_to_string(&op)?));
            }
            Err(e) => {
                result.push_str(&format!("\n    {}: Error decoding op: {}", i, e));
            }
        }
    }

    Ok(result)
}

fn op_to_string(op: &Op) -> Result<String, fmt::Error> {
    let s = match op {
        Op::Push(node) => format!("Push({})", node_to_string(node)?),
        Op::PushInverted(node) => format!("PushInverted({})", node_to_string(node)?),
        Op::Parent => "Parent".to_string(),
        Op::Child => "Child".to_string(),
        Op::ParentInverted => "ParentInverted".to_string(),
        Op::ChildInverted => "ChildInverted".to_string(),
    };
    Ok(s)
}

fn node_to_string(node: &Node) -> Result<String, fmt::Error> {
    let s = match node {
        Node::Hash(hash) => format!("Hash(HASH[{}])", hex::encode(hash)),
        Node::KVHash(kv_hash) => format!("KVHash(HASH[{}])", hex::encode(kv_hash)),
        Node::KV(key, value) => {
            format!(
                "KV({}, {})",
                hex_to_ascii(key),
                element_hex_to_ascii(value)?
            )
        }
        Node::KVValueHash(key, value, value_hash) => format!(
            "KVValueHash({}, {}, HASH[{}])",
            hex_to_ascii(key),
            element_hex_to_ascii(value)?,
            hex::encode(value_hash)
        ),
        Node::KVDigest(key, value_hash) => format!(
            "KVDigest({}, HASH[{}])",
            hex_to_ascii(key),
            hex::encode(value_hash)
        ),
        Node::KVDigestCount(key, value_hash, count) => format!(
            "KVDigestCount({}, HASH[{}], {})",
            hex_to_ascii(key),
            hex::encode(value_hash),
            count
        ),
        Node::KVRefValueHash(key, value, value_hash) => format!(
            "KVRefValueHash({}, {}, HASH[{}])",
            hex_to_ascii(key),
            element_hex_to_ascii(value)?,
            hex::encode(value_hash)
        ),
        Node::KVValueHashFeatureType(key, value, value_hash, feature_type) => format!(
            "KVValueHashFeatureType({}, {}, HASH[{}], {:?})",
            hex_to_ascii(key),
            element_hex_to_ascii(value)?,
            hex::encode(value_hash),
            feature_type
        ),
        Node::KVCount(key, value, count) => format!(
            "KVCount({}, {}, {})",
            hex_to_ascii(key),
            element_hex_to_ascii(value)?,
            count
        ),
        Node::KVHashCount(kv_hash, count) => {
            format!("KVHashCount(HASH[{}], {})", hex::encode(kv_hash), count)
        }
        Node::KVRefValueHashCount(key, value, value_hash, count) => format!(
            "KVRefValueHashCount({}, {}, HASH[{}], {})",
            hex_to_ascii(key),
            element_hex_to_ascii(value)?,
            hex::encode(value_hash),
            count
        ),
        Node::KVValueHashFeatureTypeWithChildHash(
            key,
            value,
            value_hash,
            feature_type,
            child_hash,
        ) => format!(
            "KVValueHashFeatureTypeWithChildHash({}, {}, HASH[{}], {:?}, HASH[{}])",
            hex_to_ascii(key),
            element_hex_to_ascii(value)?,
            hex::encode(value_hash),
            feature_type,
            hex::encode(child_hash)
        ),
    };
    Ok(s)
}

fn decode_mmr_proof(bytes: &[u8]) -> String {
    match MmrTreeProof::decode_from_slice(bytes) {
        Ok(proof) => {
            let mut s = format!(
                "\n    mmr_size: {}, leaves: {}, proof_items: {}",
                proof.mmr_size(),
                proof.leaves().len(),
                proof.proof_items().len(),
            );
            for (i, (idx, value)) in proof.leaves().iter().enumerate() {
                s.push_str(&format!(
                    "\n    leaf[{}]: index={}, value={}",
                    i,
                    idx,
                    hex_to_ascii(value),
                ));
            }
            for (i, hash) in proof.proof_items().iter().enumerate() {
                s.push_str(&format!(
                    "\n    sibling[{}]: HASH[{}]",
                    i,
                    hex::encode(hash),
                ));
            }
            s
        }
        Err(e) => format!("Error decoding MMR proof: {}", e),
    }
}

fn decode_bulk_append_proof(bytes: &[u8]) -> String {
    match BulkAppendTreeProof::decode_from_slice(bytes) {
        Ok(proof) => {
            let mut s = format!(
                "\n    chunk_proof: mmr_size={}, leaves={}, proof_items={}",
                proof.chunk_proof.mmr_size(),
                proof.chunk_proof.leaves().len(),
                proof.chunk_proof.proof_items().len(),
            );
            s.push_str(&format!(
                "\n    buffer_proof: entries={}, node_value_hashes={}, node_hashes={}",
                proof.buffer_proof.entries.len(),
                proof.buffer_proof.node_value_hashes.len(),
                proof.buffer_proof.node_hashes.len(),
            ));
            for (i, (pos, data)) in proof.chunk_proof.leaves().iter().enumerate() {
                s.push_str(&format!(
                    "\n    mmr_leaf[{}]: pos={}, {} bytes",
                    i,
                    pos,
                    data.len(),
                ));
            }
            for (i, (pos, value)) in proof.buffer_proof.entries.iter().enumerate() {
                s.push_str(&format!(
                    "\n    buffer[{}]: pos={}, {}",
                    i,
                    pos,
                    hex_to_ascii(value),
                ));
            }
            s
        }
        Err(e) => {
            format!("Error decoding BulkAppendTree proof: {}", e)
        }
    }
}

fn decode_dense_proof(bytes: &[u8]) -> String {
    match DenseTreeProof::decode_from_slice(bytes) {
        Ok(proof) => {
            let mut s = format!(
                "\n    entries: {}, node_value_hashes: {}, node_hashes: {}",
                proof.entries.len(),
                proof.node_value_hashes.len(),
                proof.node_hashes.len(),
            );
            for (i, (pos, value)) in proof.entries.iter().enumerate() {
                s.push_str(&format!(
                    "\n    entry[{}]: pos={}, value={}",
                    i,
                    pos,
                    hex_to_ascii(value),
                ));
            }
            for (i, (pos, hash)) in proof.node_value_hashes.iter().enumerate() {
                s.push_str(&format!(
                    "\n    value_hash[{}]: pos={}, HASH[{}]",
                    i,
                    pos,
                    hex::encode(hash),
                ));
            }
            for (i, (pos, hash)) in proof.node_hashes.iter().enumerate() {
                s.push_str(&format!(
                    "\n    hash[{}]: pos={}, HASH[{}]",
                    i,
                    pos,
                    hex::encode(hash),
                ));
            }
            s
        }
        Err(e) => format!("Error decoding DenseTree proof: {}", e),
    }
}
