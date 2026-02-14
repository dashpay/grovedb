//! MMR node types and Blake3 merge implementation.

use ckb_merkle_mountain_range::{Merge, Result as MmrResult};

use crate::MmrError;

/// An MMR node: leaf nodes carry full values, internal nodes carry only hashes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MmrNode {
    pub hash: [u8; 32],
    pub value: Option<Vec<u8>>,
}

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
            0x00 => Ok(MmrNode { hash, value: None }),
            0x01 => {
                if data.len() < 37 {
                    return Err(MmrError::InvalidData("truncated leaf value length".into()));
                }
                let val_len = u32::from_be_bytes(
                    data[33..37]
                        .try_into()
                        .map_err(|_| MmrError::InvalidData("bad value length".into()))?,
                ) as usize;
                if data.len() < 37 + val_len {
                    return Err(MmrError::InvalidData("truncated leaf value".into()));
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

/// Blake3-based merge for ckb MMR.
///
/// `merge(left, right)` = `blake3(left.hash || right.hash)` → internal node.
#[derive(Clone)]
pub struct MergeBlake3;

impl Merge for MergeBlake3 {
    type Item = MmrNode;

    fn merge(left: &Self::Item, right: &Self::Item) -> MmrResult<Self::Item> {
        let mut input = [0u8; 64];
        input[..32].copy_from_slice(&left.hash);
        input[32..].copy_from_slice(&right.hash);
        let hash = blake3::hash(&input);
        Ok(MmrNode::internal(*hash.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_serialize_roundtrip_internal() {
        let node = MmrNode::internal([42u8; 32]);
        let bytes = node.serialize();
        let decoded = MmrNode::deserialize(&bytes).unwrap();
        assert_eq!(node, decoded);
        assert!(decoded.value.is_none());
    }

    #[test]
    fn test_node_serialize_roundtrip_leaf() {
        let node = MmrNode::leaf(b"test data".to_vec());
        let bytes = node.serialize();
        let decoded = MmrNode::deserialize(&bytes).unwrap();
        assert_eq!(node, decoded);
        assert_eq!(decoded.value.unwrap(), b"test data");
    }

    #[test]
    fn test_merge_blake3() {
        let left = MmrNode::leaf(b"left".to_vec());
        let right = MmrNode::leaf(b"right".to_vec());
        let merged = MergeBlake3::merge(&left, &right).unwrap();
        assert!(merged.value.is_none());

        let merged2 = MergeBlake3::merge(&left, &right).unwrap();
        assert_eq!(merged.hash, merged2.hash);

        let merged_rev = MergeBlake3::merge(&right, &left).unwrap();
        assert_ne!(merged.hash, merged_rev.hash);
    }
}
