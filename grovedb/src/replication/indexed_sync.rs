//! State-sync support for indexed trees (`ProvableCountIndexedTree`,
//! `ProvableSumIndexedTree`, `ProvableCountProvableSumIndexedTree`).
//!
//! An indexed subtree is one primary Merk at the ordinary path prefix plus
//! one ordinary secondary Merk per configured axis at the derived prefix
//! `Blake3(primary_prefix ‖ axis_tag)`. Three properties shape the
//! protocol:
//!
//! - The parent element commits
//!   `combine_hash_three(value_hash(element_bytes), primary_root_hash,
//!   secondary_slot)` where `secondary_slot` is the single secondary root
//!   hash (PCIT / PSIT) or the canonical `axes_digest` (PCPSIT). The
//!   element itself stores root KEYS only, never root hashes, so the
//!   target cannot derive any per-Merk expected root hash from what it
//!   has restored — those hashes must cross the wire as an **indexed
//!   header**.
//! - A secondary's root hash commits to its AVL shape, which is
//!   write-history-dependent — the target cannot rebuild it locally from
//!   the primary (see `operations/indexed_tree.rs`). Secondaries are
//!   chunk-transferred like ordinary Merks, addressed by prefix.
//! - The header is untrusted (any peer can claim any hashes). Per-chunk
//!   verification against the header is early-abort DoS protection only;
//!   the security boundary is the **unconditional finalize-time joint
//!   check** ([`verify_indexed_binding`]): once the primary and every
//!   secondary of a group are fully restored, their *actual* recomputed
//!   root hashes are combined and compared against the parent-bound
//!   element value hash, which is itself protected by the already
//!   verified parent chunk chain up to the app hash.
//!
//! Wire flow (target-driven, like the rest of the protocol):
//!
//! 1. The target discovers an indexed child, opens its group, and requests
//!    the primary with a single [`IndexedHeaderRequest`] local chunk id —
//!    marker-prefixed so it can never be confused with a Merk traversal
//!    instruction (whose bytes are only `0x00` / `0x01`) — carrying the
//!    axis tags and secondary root keys from the target's hash-verified
//!    element (the source cannot recover them from a one-way prefix).
//! 2. The source answers with `pack([header, root_chunk_ops])`: the
//!    [`IndexedHeader`] (primary root hash + per-axis secondary root
//!    hashes) and the primary's root chunk (empty for an empty primary).
//! 3. The target constructs the primary `Restorer` with
//!    `Restorer::new(merk, header.primary_root_hash, None)` (direct
//!    root-hash comparison — the three-input parent binding is checked at
//!    group finalize instead), activates one ordinary Merk chunk restore
//!    per axis against the header's secondary hashes, and requests them by
//!    prefix. Subsequent primary chunks are ordinary Merk chunks.
//! 4. As each group member completes, its actual root hash is recorded;
//!    when the last lands, [`verify_indexed_binding`] accepts or rejects
//!    the whole group.

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    tree::{
        hash::{axes_digest, combine_hash_three},
        CryptoHash,
    },
    tree_type::TreeType,
    ChunkProducer,
};
use grovedb_storage::rocksdb_storage::RocksDbStorage;

use crate::{
    operations::indexed_tree::axis_secondary_tree_type,
    replication::utils::{encode_vec_ops, pack_nested_bytes},
    Element, Error, GroveDb, SubtreePrefix, Transaction,
};

/// Marker byte opening an [`IndexedHeaderRequest`] local chunk id.
///
/// Merk traversal-instruction chunk ids consist solely of `0x00` / `0x01`
/// bytes (`vec_bytes_as_traversal_instruction` rejects anything else), so
/// this byte makes the header request unambiguous among an indexed
/// primary's local chunk ids.
const INDEXED_HEADER_REQUEST_MARKER: u8 = 0xFE;

/// Length of one `(tag, root_key_len)` fixed part in a header request.
const REQUEST_AXIS_FIXED_LEN: usize = 1 + 2;

/// Returns true when a local chunk id addressed to an indexed primary is a
/// header request rather than a Merk traversal instruction.
pub(crate) fn is_indexed_header_request(chunk_id: &[u8]) -> bool {
    chunk_id.first() == Some(&INDEXED_HEADER_REQUEST_MARKER)
}

/// The target-encoded first request for an indexed primary: the configured
/// axis tags and their secondary root keys, read from the target's
/// hash-verified element. The source needs them to open the secondary
/// Merks (their prefixes are one-way hashes and layered Merks do not
/// persist their own root keys).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexedHeaderRequest {
    /// `(axis_tag, secondary_root_key)` in canonical element order.
    pub axes: Vec<(u8, Option<Vec<u8>>)>,
}

impl IndexedHeaderRequest {
    /// Layout: `MARKER ‖ n(1) ‖ (tag(1) ‖ root_key_len(2 BE) ‖ root_key)*n`.
    /// A missing root key (empty secondary) encodes as length 0.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = vec![INDEXED_HEADER_REQUEST_MARKER, self.axes.len() as u8];
        for (tag, root_key) in &self.axes {
            out.push(*tag);
            let key = root_key.as_deref().unwrap_or_default();
            out.extend_from_slice(&(key.len() as u16).to_be_bytes());
            out.extend_from_slice(key);
        }
        out
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let err =
            |what: &str| Error::CorruptedData(format!("malformed indexed header request: {what}"));
        let mut rest = bytes
            .strip_prefix(&[INDEXED_HEADER_REQUEST_MARKER][..])
            .ok_or_else(|| err("missing marker byte"))?;
        let (&count, tail) = rest
            .split_first()
            .ok_or_else(|| err("missing axis count"))?;
        rest = tail;
        if !(1..=3).contains(&count) {
            return Err(err("axis count must be 1..=3"));
        }
        let mut axes = Vec::with_capacity(count as usize);
        for _ in 0..count {
            if rest.len() < REQUEST_AXIS_FIXED_LEN {
                return Err(err("truncated axis entry"));
            }
            let tag = rest[0];
            let key_len = u16::from_be_bytes([rest[1], rest[2]]) as usize;
            rest = &rest[REQUEST_AXIS_FIXED_LEN..];
            if rest.len() < key_len {
                return Err(err("truncated root key"));
            }
            let (key, tail) = rest.split_at(key_len);
            rest = tail;
            axes.push((tag, (!key.is_empty()).then(|| key.to_vec())));
        }
        if !rest.is_empty() {
            return Err(err("trailing bytes"));
        }
        Ok(IndexedHeaderRequest { axes })
    }
}

/// The source's answer to an [`IndexedHeaderRequest`]: the root hashes the
/// element does not carry. A HINT ONLY — per-chunk verification against it
/// bounds how much garbage a byzantine source can make the target chew,
/// but acceptance is decided solely by [`verify_indexed_binding`] over the
/// actually restored Merks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexedHeader {
    /// Root hash of the primary Merk.
    pub primary_root_hash: CryptoHash,
    /// `(axis_tag, secondary_root_hash)` in canonical element order. An
    /// empty secondary reports `NULL_HASH` (the empty Merk root).
    pub axes: Vec<(u8, CryptoHash)>,
}

impl IndexedHeader {
    /// Layout: `primary_root_hash(32) ‖ n(1) ‖ (tag(1) ‖ hash(32))*n`.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(33 + 33 * self.axes.len());
        out.extend_from_slice(&self.primary_root_hash);
        out.push(self.axes.len() as u8);
        for (tag, hash) in &self.axes {
            out.push(*tag);
            out.extend_from_slice(hash);
        }
        out
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let err = |what: &str| Error::CorruptedData(format!("malformed indexed header: {what}"));
        if bytes.len() < 33 {
            return Err(err("too short for primary root hash and axis count"));
        }
        let primary_root_hash: CryptoHash = bytes[0..32].try_into().expect("checked length");
        let count = bytes[32] as usize;
        if !(1..=3).contains(&count) {
            return Err(err("axis count must be 1..=3"));
        }
        let rest = &bytes[33..];
        if rest.len() != count * 33 {
            return Err(err("axis section length mismatch"));
        }
        let axes = rest
            .as_chunks::<33>()
            .0
            .iter()
            .map(|chunk| (chunk[0], chunk[1..33].try_into().expect("checked length")))
            .collect();
        Ok(IndexedHeader {
            primary_root_hash,
            axes,
        })
    }
}

/// The unconditional finalize-time joint verification for one indexed
/// group — the security boundary of indexed-tree state sync.
///
/// `primary_root` and `secondary_roots` are the ACTUAL root hashes
/// recomputed from the fully restored Merks (never the header's claims).
/// Recomputes the element's three-input binding —
/// `combine_hash_three(value_hash, primary_root, secondary_root)` for the
/// single-axis variants, `combine_hash_three(value_hash, primary_root,
/// axes_digest(axes))` for PCPSIT (an empty secondary contributes
/// `NULL_HASH`, exactly as the write path does) — and requires it to equal
/// the element value hash bound into the restored, hash-verified parent.
pub(crate) fn verify_indexed_binding(
    element: &Element,
    actual_value_hash: &CryptoHash,
    elem_value_hash: &CryptoHash,
    primary_root: &CryptoHash,
    secondary_roots: &[(u8, CryptoHash)],
) -> Result<(), Error> {
    let secondary_slot = match element.underlying() {
        Element::ProvableCountIndexedTree(..) | Element::ProvableSumIndexedTree(..) => {
            match secondary_roots {
                [(_, hash)] => *hash,
                other => {
                    return Err(Error::InternalError(format!(
                        "single-axis indexed group finalized with {} secondary roots",
                        other.len()
                    )));
                }
            }
        }
        Element::ProvableCountProvableSumIndexedTree(..) => axes_digest(secondary_roots).unwrap(),
        other => {
            return Err(Error::InternalError(format!(
                "verify_indexed_binding called on a non-indexed element: {}",
                other.type_str()
            )));
        }
    };
    let combined = combine_hash_three(actual_value_hash, primary_root, &secondary_slot).unwrap();
    if combined != *elem_value_hash {
        return Err(Error::CorruptedData(format!(
            "indexed subtree joint verification failed: combined hash {} does not match the \
             parent binding {}",
            hex::encode(combined),
            hex::encode(elem_value_hash),
        )));
    }
    Ok(())
}

// ── Source side ─────────────────────────────────────────────────────────

impl GroveDb {
    /// Serve the header page for an indexed primary: the
    /// [`IndexedHeader`] plus the primary's root chunk, packed as
    /// `pack([header, root_chunk_ops])` (`root_chunk_ops` is empty for an
    /// empty primary).
    ///
    /// Every field of `request` is peer-controlled: an invalid axis tag or
    /// a root key that does not open a Merk produces a bounded descriptive
    /// error, and a wrong-but-openable request only yields hashes the
    /// target's joint verification will reject.
    pub(crate) fn serve_indexed_header_page(
        &self,
        chunk_prefix: SubtreePrefix,
        root_key: Option<Vec<u8>>,
        tree_type: TreeType,
        request_bytes: &[u8],
        transaction: &Transaction,
        grove_version: &grovedb_version::version::GroveVersion,
    ) -> Result<Vec<u8>, Error> {
        let request = IndexedHeaderRequest::decode(request_bytes)?;

        let merk = self
            .open_transactional_merk_by_prefix(
                chunk_prefix,
                root_key,
                tree_type,
                transaction,
                None,
                grove_version,
            )
            .value
            .map_err(|e| {
                Error::CorruptedData(format!(
                    "failed to open indexed primary by prefix {}: {e}",
                    hex::encode(chunk_prefix)
                ))
            })?;
        let primary_root_hash = merk.root_hash().unwrap();

        let mut axes = Vec::with_capacity(request.axes.len());
        for (tag, secondary_root_key) in &request.axes {
            let axis = IndexAxis::try_from_tag(*tag).map_err(|e| {
                Error::CorruptedData(format!("invalid axis tag in indexed header request: {e}"))
            })?;
            let secondary_prefix =
                RocksDbStorage::secondary_prefix_for(&chunk_prefix, *tag).unwrap();
            let secondary_merk = self
                .open_transactional_merk_by_prefix(
                    secondary_prefix,
                    secondary_root_key.clone(),
                    axis_secondary_tree_type(axis),
                    transaction,
                    None,
                    grove_version,
                )
                .value
                .map_err(|e| {
                    Error::CorruptedData(format!(
                        "failed to open indexed secondary (axis {axis:?}) by prefix: {e}"
                    ))
                })?;
            axes.push((*tag, secondary_merk.root_hash().unwrap()));
        }

        let header = IndexedHeader {
            primary_root_hash,
            axes,
        };

        let root_chunk_ops = if merk.is_empty_tree().unwrap() {
            Vec::new()
        } else {
            let mut chunk_producer = ChunkProducer::new(&merk).map_err(|e| {
                Error::CorruptedData(format!(
                    "failed to create indexed primary chunk producer: {e}"
                ))
            })?;
            let (chunk, _) = chunk_producer.chunk(&[], grove_version).map_err(|e| {
                Error::CorruptedData(format!("failed to produce indexed primary root chunk: {e}"))
            })?;
            encode_vec_ops(chunk)?
        };

        pack_nested_bytes(vec![header.encode(), root_chunk_ops])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_header_request_roundtrip() {
        let request = IndexedHeaderRequest {
            axes: vec![
                (0, Some(b"count_root".to_vec())),
                (1, None),
                (2, Some(vec![0xFF; 40])),
            ],
        };
        let encoded = request.encode();
        assert!(is_indexed_header_request(&encoded));
        assert_eq!(IndexedHeaderRequest::decode(&encoded).unwrap(), request);

        let single = IndexedHeaderRequest {
            axes: vec![(1, None)],
        };
        assert_eq!(
            IndexedHeaderRequest::decode(&single.encode()).unwrap(),
            single
        );
    }

    #[test]
    fn indexed_header_request_rejects_malformed() {
        // Empty / wrong marker / bad counts.
        assert!(IndexedHeaderRequest::decode(&[]).is_err());
        assert!(IndexedHeaderRequest::decode(&[0x00, 1]).is_err());
        assert!(IndexedHeaderRequest::decode(&[INDEXED_HEADER_REQUEST_MARKER]).is_err());
        assert!(IndexedHeaderRequest::decode(&[INDEXED_HEADER_REQUEST_MARKER, 0]).is_err());
        assert!(IndexedHeaderRequest::decode(&[INDEXED_HEADER_REQUEST_MARKER, 4]).is_err());
        // Truncated axis entry and truncated root key.
        assert!(IndexedHeaderRequest::decode(&[INDEXED_HEADER_REQUEST_MARKER, 1, 0]).is_err());
        assert!(
            IndexedHeaderRequest::decode(&[INDEXED_HEADER_REQUEST_MARKER, 1, 0, 0, 5, 1, 2])
                .is_err()
        );
        // Trailing bytes.
        let mut encoded = IndexedHeaderRequest {
            axes: vec![(0, None)],
        }
        .encode();
        encoded.push(0);
        assert!(IndexedHeaderRequest::decode(&encoded).is_err());
        // A traversal instruction is never mistaken for a header request.
        assert!(!is_indexed_header_request(&[0x01, 0x00, 0x01]));
        assert!(!is_indexed_header_request(&[]));
    }

    #[test]
    fn indexed_header_roundtrip_and_malformed() {
        let header = IndexedHeader {
            primary_root_hash: [7u8; 32],
            axes: vec![(0, [1u8; 32]), (1, [2u8; 32]), (2, [3u8; 32])],
        };
        assert_eq!(IndexedHeader::decode(&header.encode()).unwrap(), header);

        let single = IndexedHeader {
            primary_root_hash: [9u8; 32],
            axes: vec![(1, [4u8; 32])],
        };
        assert_eq!(IndexedHeader::decode(&single.encode()).unwrap(), single);

        assert!(IndexedHeader::decode(&[]).is_err());
        assert!(IndexedHeader::decode(&[0u8; 32]).is_err());
        // Zero axes.
        let mut zero = vec![0u8; 33];
        zero[32] = 0;
        assert!(IndexedHeader::decode(&zero).is_err());
        // Axis section length mismatch.
        let mut short = single.encode();
        short.pop();
        assert!(IndexedHeader::decode(&short).is_err());
        let mut long = single.encode();
        long.push(0);
        assert!(IndexedHeader::decode(&long).is_err());
    }
}
