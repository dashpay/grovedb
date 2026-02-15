//! MMR node types and Blake3 merge implementation.

use ckb_merkle_mountain_range::{Merge, Result as MmrResult};

use crate::MmrError;

/// An MMR node: leaf nodes carry full values, internal nodes carry only hashes.
///
/// `PartialEq` and `Eq` compare only the `hash` field, because the ckb MMR
/// library's proof verifier compares nodes by equality and a leaf node
/// (value = Some) must equal an internal reconstruction (value = None) when
/// their hashes match.
#[derive(Clone, Debug)]
pub struct MmrNode {
    pub hash: [u8; 32],
    pub value: Option<Vec<u8>>,
}

impl PartialEq for MmrNode {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

impl Eq for MmrNode {}

impl MmrNode {
    /// Create a leaf node by hashing the value.
    pub fn leaf(value: Vec<u8>) -> Self {
        let hash = blake3::hash(&value);
        MmrNode {
            hash: *hash.as_bytes(),
            value: Some(value),
        }
    }

    /// Create an internal node (hash only, no value).
    pub fn internal(hash: [u8; 32]) -> Self {
        MmrNode { hash, value: None }
    }

    /// Serialize this node to bytes.
    ///
    /// Format: `flag(1) + hash(32) [+ value_len(4 BE) + value_bytes]`
    /// - flag 0x00 = internal node (no value)
    /// - flag 0x01 = leaf node (has value)
    pub fn serialize(&self) -> Vec<u8> {
        match &self.value {
            None => {
                let mut buf = Vec::with_capacity(33);
                buf.push(0x00);
                buf.extend_from_slice(&self.hash);
                buf
            }
            Some(val) => {
                let mut buf = Vec::with_capacity(37 + val.len());
                buf.push(0x01);
                buf.extend_from_slice(&self.hash);
                buf.extend_from_slice(&(val.len() as u32).to_be_bytes());
                buf.extend_from_slice(val);
                buf
            }
        }
    }

    /// Deserialize a node from bytes.
    pub fn deserialize(data: &[u8]) -> Result<Self, MmrError> {
        if data.len() < 33 {
            return Err(MmrError::InvalidData("data too short for MmrNode".into()));
        }
        let flag = data[0];
        let hash: [u8; 32] = data[1..33]
            .try_into()
            .map_err(|_| MmrError::InvalidData("bad hash bytes".into()))?;
        match flag {
            0x00 => {
                if data.len() != 33 {
                    return Err(MmrError::InvalidData(format!(
                        "internal node has {} trailing bytes",
                        data.len() - 33
                    )));
                }
                Ok(MmrNode { hash, value: None })
            }
            0x01 => {
                if data.len() < 37 {
                    return Err(MmrError::InvalidData("truncated leaf value length".into()));
                }
                let val_len = u32::from_be_bytes(
                    data[33..37]
                        .try_into()
                        .map_err(|_| MmrError::InvalidData("bad value length".into()))?,
                ) as usize;
                if data.len() != 37 + val_len {
                    return Err(MmrError::InvalidData(format!(
                        "leaf node expected {} bytes, got {}",
                        37 + val_len,
                        data.len()
                    )));
                }
                Ok(MmrNode {
                    hash,
                    value: Some(data[37..37 + val_len].to_vec()),
                })
            }
            _ => Err(MmrError::InvalidData(format!(
                "unknown flag: 0x{:02x}",
                flag
            ))),
        }
    }
}

impl Default for MmrNode {
    fn default() -> Self {
        MmrNode {
            hash: [0u8; 32],
            value: None,
        }
    }
}

/// Blake3 hash of two 32-byte inputs concatenated: `blake3(left || right)`.
pub fn blake3_merge(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut input = [0u8; 64];
    input[..32].copy_from_slice(left);
    input[32..].copy_from_slice(right);
    *blake3::hash(&input).as_bytes()
}

/// Blake3-based merge for ckb MMR.
///
/// `merge(left, right)` = `blake3(left.hash || right.hash)` → internal node.
#[derive(Clone)]
pub struct MergeBlake3;

impl Merge for MergeBlake3 {
    type Item = MmrNode;

    fn merge(left: &Self::Item, right: &Self::Item) -> MmrResult<Self::Item> {
        Ok(MmrNode::internal(blake3_merge(&left.hash, &right.hash)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_serialize_roundtrip_internal() {
        let node = MmrNode::internal([42u8; 32]);
        let bytes = node.serialize();
        let decoded = MmrNode::deserialize(&bytes).expect("deserialize internal node");
        assert_eq!(node, decoded);
        assert!(decoded.value.is_none());
    }

    #[test]
    fn test_node_serialize_roundtrip_leaf() {
        let node = MmrNode::leaf(b"test data".to_vec());
        let bytes = node.serialize();
        let decoded = MmrNode::deserialize(&bytes).expect("deserialize leaf node");
        assert_eq!(node, decoded);
        assert_eq!(decoded.value.expect("leaf should have value"), b"test data");
    }

    #[test]
    fn test_merge_blake3() {
        let left = MmrNode::leaf(b"left".to_vec());
        let right = MmrNode::leaf(b"right".to_vec());
        let merged = MergeBlake3::merge(&left, &right).expect("merge left+right");
        assert!(merged.value.is_none());

        let merged2 = MergeBlake3::merge(&left, &right).expect("merge left+right again");
        assert_eq!(merged.hash, merged2.hash);

        let merged_rev = MergeBlake3::merge(&right, &left).expect("merge right+left");
        assert_ne!(merged.hash, merged_rev.hash);
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
        let mut bytes = node.serialize();
        bytes.push(0x00); // trailing byte
        assert!(MmrNode::deserialize(&bytes).is_err());
    }

    #[test]
    fn test_deserialize_leaf_trailing_bytes() {
        let node = MmrNode::leaf(b"data".to_vec());
        let mut bytes = node.serialize();
        bytes.push(0x00); // trailing byte
        assert!(MmrNode::deserialize(&bytes).is_err());
    }

    #[test]
    fn test_deserialize_leaf_truncated_value() {
        let node = MmrNode::leaf(b"data".to_vec());
        let bytes = node.serialize();
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
}
