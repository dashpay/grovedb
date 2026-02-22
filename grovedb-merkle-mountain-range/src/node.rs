//! MMR node types and Blake3 merge implementation.
//!
//! Hash domain separation:
//! - Leaf nodes:     `blake3(0x00 || value)`
//! - Internal nodes: `blake3(0x01 || left_hash || right_hash)`
//!
//! The 0x00/0x01 domain tags prevent second-preimage attacks where a crafted
//! value could produce the same hash as an internal merge.

use crate::{Error, Result};

/// Domain tag prepended to leaf hash inputs: `blake3(LEAF_TAG || value)`.
const LEAF_TAG: u8 = 0x00;
/// Domain tag prepended to internal merge inputs: `blake3(INTERNAL_TAG || left
/// || right)`.
const INTERNAL_TAG: u8 = 0x01;

/// An MMR node: leaf nodes carry full values, internal nodes carry only hashes.
///
/// `PartialEq` and `Eq` compare only the `hash` field, because the MMR
/// library's proof verifier compares nodes by equality and a leaf node
/// (value = Some) must equal an internal reconstruction (value = None) when
/// their hashes match.
#[derive(Clone, Debug)]
pub struct MmrNode {
    hash: [u8; 32],
    value: Option<Vec<u8>>,
    is_data_leaf: bool,
}

impl PartialEq for MmrNode {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

impl Eq for MmrNode {}

impl MmrNode {
    /// Create a leaf node: `hash = blake3(0x00 || value)`.
    pub fn leaf(value: Vec<u8>) -> Self {
        let hash = leaf_hash(&value);
        MmrNode {
            hash,
            value: Some(value),
            is_data_leaf: false,
        }
    }

    /// Create an internal node (hash only, no value).
    pub fn internal(hash: [u8; 32]) -> Self {
        MmrNode {
            hash,
            value: None,
            is_data_leaf: false,
        }
    }

    /// Create a data leaf node with an externally-computed hash.
    ///
    /// Unlike `leaf()` which computes `hash = blake3(0x00 || value)`, this
    /// stores an arbitrary hash alongside the data. Used by BulkAppendTree
    /// where the hash is a dense Merkle root of the chunk entries.
    pub fn data_leaf(hash: [u8; 32], data: Vec<u8>) -> Self {
        MmrNode {
            hash,
            value: Some(data),
            is_data_leaf: true,
        }
    }

    /// The 32-byte Blake3 hash identifying this node.
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }

    /// The raw value for leaf nodes; `None` for internal (hash-only) nodes.
    pub fn value(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }

    /// Consume this node and return the raw value, if any.
    pub fn into_value(self) -> Option<Vec<u8>> {
        self.value
    }

    /// Merge two sibling nodes into a parent: `blake3(0x01 || left.hash ||
    /// right.hash)`.
    ///
    /// Also used for bagging peaks when computing the MMR root.
    pub fn merge(left: &MmrNode, right: &MmrNode) -> MmrNode {
        MmrNode::internal(blake3_merge(&left.hash, &right.hash))
    }

    /// The serialized size in bytes.
    ///
    /// Internal nodes: 33 bytes (1 flag + 32 hash).
    /// Leaf/data-leaf nodes: 37 + value length (1 flag + 32 hash + 4 length +
    /// value).
    pub fn serialized_size(&self) -> u64 {
        match &self.value {
            None => 33,
            Some(val) => 37 + val.len() as u64,
        }
    }

    /// Serialize this node to bytes.
    ///
    /// Format: `flag(1) + hash(32) [+ value_len(4 BE) + value_bytes]`
    /// - flag 0x00 = internal node (no value)
    /// - flag 0x01 = leaf node (hash = blake3(0x00 || value))
    /// - flag 0x02 = data leaf node (hash is external, no validation on
    ///   deserialize)
    pub fn serialize(&self) -> Result<Vec<u8>> {
        match &self.value {
            None => {
                let mut buf = Vec::with_capacity(33);
                buf.push(0x00);
                buf.extend_from_slice(&self.hash);
                Ok(buf)
            }
            Some(val) => {
                if val.len() > u32::MAX as usize {
                    return Err(Error::InvalidData(format!(
                        "MmrNode value length {} exceeds u32::MAX",
                        val.len()
                    )));
                }
                let flag = if self.is_data_leaf { 0x02 } else { 0x01 };
                let mut buf = Vec::with_capacity(37 + val.len());
                buf.push(flag);
                buf.extend_from_slice(&self.hash);
                buf.extend_from_slice(&(val.len() as u32).to_be_bytes());
                buf.extend_from_slice(val);
                Ok(buf)
            }
        }
    }

    /// Deserialize a node from bytes.
    pub fn deserialize(data: &[u8]) -> Result<Self> {
        if data.len() < 33 {
            return Err(Error::InvalidData("data too short for MmrNode".into()));
        }
        let flag = data[0];
        let hash: [u8; 32] = data[1..33]
            .try_into()
            .map_err(|_| Error::InvalidData("bad hash bytes".into()))?;
        match flag {
            0x00 => {
                if data.len() != 33 {
                    return Err(Error::InvalidData(format!(
                        "internal node has {} trailing bytes",
                        data.len() - 33
                    )));
                }
                Ok(MmrNode {
                    hash,
                    value: None,
                    is_data_leaf: false,
                })
            }
            0x01 | 0x02 => {
                if data.len() < 37 {
                    return Err(Error::InvalidData("truncated leaf value length".into()));
                }
                let val_len = u32::from_be_bytes(
                    data[33..37]
                        .try_into()
                        .map_err(|_| Error::InvalidData("bad value length".into()))?,
                ) as usize;
                if data.len() != 37 + val_len {
                    return Err(Error::InvalidData(format!(
                        "leaf node expected {} bytes, got {}",
                        37 + val_len,
                        data.len()
                    )));
                }
                let value = data[37..37 + val_len].to_vec();
                // Flag 0x01: verify hash-value binding (standard leaf).
                // Flag 0x02: skip validation (data leaf with external hash).
                if flag == 0x01 {
                    let expected_hash = leaf_hash(&value);
                    if hash != expected_hash {
                        return Err(Error::InvalidData(
                            "leaf hash does not match blake3(0x00 || value)".into(),
                        ));
                    }
                }
                Ok(MmrNode {
                    hash,
                    value: Some(value),
                    is_data_leaf: flag == 0x02,
                })
            }
            _ => Err(Error::InvalidData(format!("unknown flag: 0x{:02x}", flag))),
        }
    }
}

/// Compute the domain-separated leaf hash: `blake3(0x00 || value)`.
pub fn leaf_hash(value: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[LEAF_TAG]);
    hasher.update(value);
    *hasher.finalize().as_bytes()
}

/// Blake3 merge with domain separation: `blake3(0x01 || left || right)`.
pub fn blake3_merge(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut input = [0u8; 65];
    input[0] = INTERNAL_TAG;
    input[1..33].copy_from_slice(left);
    input[33..65].copy_from_slice(right);
    *blake3::hash(&input).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_serialize_roundtrip_internal() {
        let node = MmrNode::internal([42u8; 32]);
        let bytes = node.serialize().expect("serialize internal node");
        let decoded = MmrNode::deserialize(&bytes).expect("deserialize internal node");
        assert_eq!(node, decoded);
        assert!(decoded.value().is_none());
    }

    #[test]
    fn test_node_serialize_roundtrip_leaf() {
        let node = MmrNode::leaf(b"test data".to_vec());
        let bytes = node.serialize().expect("serialize leaf node");
        let decoded = MmrNode::deserialize(&bytes).expect("deserialize leaf node");
        assert_eq!(node, decoded);
        assert_eq!(
            decoded.value().expect("leaf should have value"),
            b"test data"
        );
    }

    #[test]
    fn test_merge() {
        let left = MmrNode::leaf(b"left".to_vec());
        let right = MmrNode::leaf(b"right".to_vec());
        let merged = MmrNode::merge(&left, &right);
        assert!(merged.value().is_none());

        let merged2 = MmrNode::merge(&left, &right);
        assert_eq!(merged.hash(), merged2.hash());

        let merged_rev = MmrNode::merge(&right, &left);
        assert_ne!(merged.hash(), merged_rev.hash());
    }

    #[test]
    fn test_deserialize_too_short() {
        assert!(MmrNode::deserialize(&[0u8; 10]).is_err());
    }

    #[test]
    fn test_deserialize_unknown_flag() {
        let mut data = vec![0xFF];
        data.extend_from_slice(&[0u8; 32]);
        assert!(MmrNode::deserialize(&data).is_err());
    }

    #[test]
    fn test_deserialize_internal_trailing_bytes() {
        let node = MmrNode::internal([1u8; 32]);
        let mut bytes = node.serialize().expect("serialize internal node");
        bytes.push(0x00); // trailing byte
        assert!(MmrNode::deserialize(&bytes).is_err());
    }

    #[test]
    fn test_deserialize_leaf_trailing_bytes() {
        let node = MmrNode::leaf(b"data".to_vec());
        let mut bytes = node.serialize().expect("serialize leaf node");
        bytes.push(0x00); // trailing byte
        assert!(MmrNode::deserialize(&bytes).is_err());
    }

    #[test]
    fn test_deserialize_leaf_truncated_value() {
        let node = MmrNode::leaf(b"data".to_vec());
        let bytes = node.serialize().expect("serialize leaf node");
        // Truncate the value portion
        assert!(MmrNode::deserialize(&bytes[..bytes.len() - 2]).is_err());
    }

    #[test]
    fn test_deserialize_leaf_truncated_length() {
        // Flag + hash but missing value length
        let mut data = vec![0x01];
        data.extend_from_slice(&[0u8; 32]);
        assert!(MmrNode::deserialize(&data).is_err());
    }

    #[test]
    fn test_leaf_hash_uses_domain_tag() {
        // Verify leaf hash is blake3(0x00 || value), not plain blake3(value)
        let value = b"test value";
        let node = MmrNode::leaf(value.to_vec());

        // Manual domain-tagged hash
        let mut hasher = blake3::Hasher::new();
        hasher.update(&[0x00]);
        hasher.update(value);
        let expected = *hasher.finalize().as_bytes();

        assert_eq!(
            node.hash(),
            expected,
            "leaf hash should use 0x00 domain tag"
        );

        // Must NOT equal plain blake3(value)
        let plain = *blake3::hash(value).as_bytes();
        assert_ne!(
            node.hash(),
            plain,
            "leaf hash must differ from plain blake3(value)"
        );
    }

    #[test]
    fn test_merge_uses_domain_tag() {
        // Verify merge is blake3(0x01 || left || right), not blake3(left || right)
        let left = [0xAAu8; 32];
        let right = [0xBBu8; 32];
        let merged = blake3_merge(&left, &right);

        // Manual domain-tagged hash
        let mut input = [0u8; 65];
        input[0] = 0x01;
        input[1..33].copy_from_slice(&left);
        input[33..65].copy_from_slice(&right);
        let expected = *blake3::hash(&input).as_bytes();

        assert_eq!(merged, expected, "merge hash should use 0x01 domain tag");

        // Must NOT equal plain blake3(left || right)
        let mut plain_input = [0u8; 64];
        plain_input[..32].copy_from_slice(&left);
        plain_input[32..].copy_from_slice(&right);
        let plain = *blake3::hash(&plain_input).as_bytes();
        assert_ne!(
            merged, plain,
            "merge hash must differ from plain blake3(left || right)"
        );
    }

    #[test]
    fn test_deserialize_rejects_tampered_leaf_hash() {
        let node = MmrNode::leaf(b"real data".to_vec());
        let mut bytes = node.serialize().expect("serialize leaf node");
        bytes[1] ^= 0x01;
        let result = MmrNode::deserialize(&bytes);
        assert!(
            result.is_err(),
            "deserialize should reject tampered leaf hash"
        );
        let err = result.expect_err("should be an error for tampered hash");
        let err_msg = format!("{}", err);
        assert!(
            err_msg.contains("does not match"),
            "error should mention hash mismatch: {}",
            err_msg
        );
    }

    #[test]
    fn test_data_leaf_serialize_roundtrip() {
        let external_hash = [0xABu8; 32];
        let data = b"chunk blob data here".to_vec();
        let node = MmrNode::data_leaf(external_hash, data.clone());

        assert_eq!(node.hash(), external_hash);
        assert_eq!(
            node.value(),
            Some(data.as_slice()),
            "data_leaf should carry the data"
        );

        let bytes = node.serialize().expect("serialize data_leaf");
        assert_eq!(bytes[0], 0x02, "data_leaf should use flag 0x02");

        let decoded = MmrNode::deserialize(&bytes).expect("deserialize data_leaf");
        assert_eq!(decoded.hash(), external_hash);
        assert_eq!(decoded.value().expect("data_leaf should have value"), data);
    }

    #[test]
    fn test_data_leaf_flag_differs_from_leaf() {
        // A standard leaf uses flag 0x01; data_leaf with external hash uses 0x02
        let value = b"test".to_vec();
        let leaf = MmrNode::leaf(value.clone());
        let leaf_bytes = leaf.serialize().expect("serialize leaf");
        assert_eq!(leaf_bytes[0], 0x01);

        let data_leaf = MmrNode::data_leaf([0xFFu8; 32], value);
        let data_bytes = data_leaf.serialize().expect("serialize data_leaf");
        assert_eq!(data_bytes[0], 0x02);
    }

    #[test]
    fn test_data_leaf_with_matching_hash_still_uses_flag_02() {
        // data_leaf always uses flag 0x02 regardless of hash match
        let value = b"some value".to_vec();
        let expected_hash = leaf_hash(&value);
        let node = MmrNode::data_leaf(expected_hash, value);
        let bytes = node.serialize().expect("serialize");
        assert_eq!(bytes[0], 0x02, "data_leaf should always use flag 0x02");
    }

    #[test]
    fn test_data_leaf_trailing_bytes_rejected() {
        let node = MmrNode::data_leaf([0xCDu8; 32], b"payload".to_vec());
        let mut bytes = node.serialize().expect("serialize data_leaf");
        bytes.push(0x00);
        assert!(
            MmrNode::deserialize(&bytes).is_err(),
            "trailing bytes should be rejected for data_leaf"
        );
    }

    #[test]
    fn test_serialized_size_matches_serialize_internal() {
        let node = MmrNode::internal([0xABu8; 32]);
        let bytes = node.serialize().expect("serialize internal");
        assert_eq!(
            node.serialized_size(),
            bytes.len() as u64,
            "serialized_size must match actual serialize output for internal nodes"
        );
    }

    #[test]
    fn test_serialized_size_matches_serialize_leaf() {
        let node = MmrNode::leaf(b"test data for size check".to_vec());
        let bytes = node.serialize().expect("serialize leaf");
        assert_eq!(
            node.serialized_size(),
            bytes.len() as u64,
            "serialized_size must match actual serialize output for leaf nodes"
        );
    }

    #[test]
    fn test_serialized_size_matches_serialize_data_leaf() {
        let node = MmrNode::data_leaf([0xCDu8; 32], b"chunk blob payload".to_vec());
        let bytes = node.serialize().expect("serialize data_leaf");
        assert_eq!(
            node.serialized_size(),
            bytes.len() as u64,
            "serialized_size must match actual serialize output for data_leaf nodes"
        );
    }

    #[test]
    fn test_serialized_size_matches_serialize_empty_leaf() {
        let node = MmrNode::leaf(vec![]);
        let bytes = node.serialize().expect("serialize empty leaf");
        assert_eq!(
            node.serialized_size(),
            bytes.len() as u64,
            "serialized_size must match actual serialize output for empty leaf nodes"
        );
    }

    #[test]
    fn test_serialized_size_matches_serialize_large_leaf() {
        let node = MmrNode::leaf(vec![0xFFu8; 10_000]);
        let bytes = node.serialize().expect("serialize large leaf");
        assert_eq!(
            node.serialized_size(),
            bytes.len() as u64,
            "serialized_size must match actual serialize output for large leaf nodes"
        );
    }
}
