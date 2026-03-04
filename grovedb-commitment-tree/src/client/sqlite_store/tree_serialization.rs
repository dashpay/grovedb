//! Binary serialization of `PrunableTree<MerkleHashOrchard>` nodes.
//!
//! Format:
//! - `Nil`:    `[0x00]`
//! - `Leaf`:   `[0x01][hash: 32][flags: 1]`
//! - `Parent`: `[0x02][has_ann: 1][ann?: 32][left][right]`

use std::sync::Arc;

use orchard::tree::MerkleHashOrchard;
use shardtree::{Node, PrunableTree, RetentionFlags, Tree};

use super::SqliteShardStoreError;
use crate::commitment_frontier::merkle_hash_from_bytes;

/// Binary format tags for tree nodes.
const TAG_NIL: u8 = 0x00;
const TAG_LEAF: u8 = 0x01;
const TAG_PARENT: u8 = 0x02;

/// Maximum recursion depth for deserialization.
///
/// A binary tree of depth 32 has at most 32 levels of nesting in its
/// serialization. We allow a small margin above the shard height.
const MAX_DESERIALIZE_DEPTH: usize = 64;

/// Serialize a `PrunableTree<MerkleHashOrchard>` to bytes.
pub(crate) fn serialize_tree(tree: &PrunableTree<MerkleHashOrchard>) -> Vec<u8> {
    let mut buf = Vec::new();
    serialize_tree_inner(tree, &mut buf);
    buf
}

fn serialize_tree_inner(tree: &PrunableTree<MerkleHashOrchard>, buf: &mut Vec<u8>) {
    match &**tree {
        Node::Nil => {
            buf.push(TAG_NIL);
        }
        Node::Leaf {
            value: (hash, flags),
        } => {
            buf.push(TAG_LEAF);
            buf.extend_from_slice(&hash.to_bytes());
            buf.push(flags.bits());
        }
        Node::Parent { ann, left, right } => {
            buf.push(TAG_PARENT);
            match ann {
                Some(arc_hash) => {
                    buf.push(0x01);
                    buf.extend_from_slice(&arc_hash.to_bytes());
                }
                None => {
                    buf.push(0x00);
                }
            }
            serialize_tree_inner(left, buf);
            serialize_tree_inner(right, buf);
        }
    }
}

/// Deserialize a `PrunableTree<MerkleHashOrchard>` from bytes.
pub(crate) fn deserialize_tree(
    data: &[u8],
    pos: &mut usize,
) -> Result<PrunableTree<MerkleHashOrchard>, SqliteShardStoreError> {
    deserialize_tree_bounded(data, pos, 0)
}

/// Depth-bounded deserialization to prevent stack overflow from malicious
/// input.
fn deserialize_tree_bounded(
    data: &[u8],
    pos: &mut usize,
    depth: usize,
) -> Result<PrunableTree<MerkleHashOrchard>, SqliteShardStoreError> {
    if depth > MAX_DESERIALIZE_DEPTH {
        return Err(SqliteShardStoreError::Serialization(format!(
            "tree exceeds maximum nesting depth of {}",
            MAX_DESERIALIZE_DEPTH
        )));
    }

    if *pos >= data.len() {
        return Err(SqliteShardStoreError::Serialization(
            "unexpected end of data".to_string(),
        ));
    }

    let tag = data[*pos];
    *pos += 1;

    match tag {
        TAG_NIL => Ok(Tree::empty()),
        TAG_LEAF => {
            if *pos + 33 > data.len() {
                return Err(SqliteShardStoreError::Serialization(
                    "truncated leaf data".to_string(),
                ));
            }
            let hash_bytes: [u8; 32] = data[*pos..*pos + 32]
                .try_into()
                .map_err(|_| SqliteShardStoreError::Serialization("bad hash".to_string()))?;
            *pos += 32;
            let flags_byte = data[*pos];
            *pos += 1;

            let hash = merkle_hash_from_bytes(&hash_bytes).ok_or_else(|| {
                SqliteShardStoreError::Serialization(
                    "invalid Pallas field element in leaf".to_string(),
                )
            })?;
            let flags = RetentionFlags::from_bits(flags_byte).ok_or_else(|| {
                SqliteShardStoreError::Serialization(format!(
                    "invalid retention flags: 0x{flags_byte:02x}"
                ))
            })?;
            Ok(Tree::leaf((hash, flags)))
        }
        TAG_PARENT => {
            if *pos >= data.len() {
                return Err(SqliteShardStoreError::Serialization(
                    "truncated parent annotation flag".to_string(),
                ));
            }
            let has_ann = data[*pos];
            *pos += 1;

            let ann: Option<Arc<MerkleHashOrchard>> = match has_ann {
                0x00 => None,
                0x01 => {
                    if *pos + 32 > data.len() {
                        return Err(SqliteShardStoreError::Serialization(
                            "truncated parent annotation".to_string(),
                        ));
                    }
                    let ann_bytes: [u8; 32] = data[*pos..*pos + 32]
                        .try_into()
                        .map_err(|_| SqliteShardStoreError::Serialization("bad ann".to_string()))?;
                    *pos += 32;
                    let hash = merkle_hash_from_bytes(&ann_bytes).ok_or_else(|| {
                        SqliteShardStoreError::Serialization(
                            "invalid Pallas field element in annotation".to_string(),
                        )
                    })?;
                    Some(Arc::new(hash))
                }
                other => {
                    return Err(SqliteShardStoreError::Serialization(format!(
                        "invalid parent annotation flag: 0x{other:02x}"
                    )));
                }
            };

            let left = deserialize_tree_bounded(data, pos, depth + 1)?;
            let right = deserialize_tree_bounded(data, pos, depth + 1)?;
            Ok(Tree::parent(ann, left, right))
        }
        other => Err(SqliteShardStoreError::Serialization(format!(
            "unknown tree node tag: 0x{other:02x}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_exceeding_max_depth() {
        // Build a deeply nested Parent chain: TAG_PARENT, no annotation, left=Nil,
        // right=recurse
        let mut data = Vec::new();
        for _ in 0..MAX_DESERIALIZE_DEPTH + 2 {
            data.push(TAG_PARENT);
            data.push(0x00); // no annotation
            data.push(TAG_NIL); // left = nil
            // right continues with next Parent
        }
        data.push(TAG_NIL); // terminal

        let mut pos = 0;
        let result = deserialize_tree(&data, &mut pos);
        assert!(result.is_err(), "should reject trees exceeding max depth");
        let msg = format!("{}", result.expect_err("should be depth error"));
        assert!(
            msg.contains("maximum nesting depth"),
            "error should mention depth limit: {msg}"
        );
    }

    #[test]
    fn test_deserialize_empty_data() {
        let mut pos = 0;
        let result = deserialize_tree(&[], &mut pos);
        assert!(result.is_err());
        let msg = format!("{}", result.expect_err("should be error"));
        assert!(
            msg.contains("unexpected end of data"),
            "error should mention end of data: {msg}"
        );
    }

    #[test]
    fn test_deserialize_unknown_tag() {
        let data = [0x99];
        let mut pos = 0;
        let result = deserialize_tree(&data, &mut pos);
        assert!(result.is_err());
        let msg = format!("{}", result.expect_err("should be error"));
        assert!(
            msg.contains("unknown tree node tag: 0x99"),
            "error should mention unknown tag: {msg}"
        );
    }

    #[test]
    fn test_deserialize_truncated_leaf() {
        // TAG_LEAF followed by only 10 bytes (needs 33: 32 hash + 1 flags)
        let mut data = vec![TAG_LEAF];
        data.extend_from_slice(&[0u8; 10]);
        let mut pos = 0;
        let result = deserialize_tree(&data, &mut pos);
        assert!(result.is_err());
        let msg = format!("{}", result.expect_err("should be error"));
        assert!(
            msg.contains("truncated leaf data"),
            "error should mention truncated leaf: {msg}"
        );
    }

    #[test]
    fn test_deserialize_invalid_leaf_field_element() {
        // TAG_LEAF + 32 bytes of 0xFF (not a valid Pallas element) + flags
        let mut data = vec![TAG_LEAF];
        data.extend_from_slice(&[0xFF; 32]);
        data.push(0x00); // flags
        let mut pos = 0;
        let result = deserialize_tree(&data, &mut pos);
        assert!(result.is_err());
        let msg = format!("{}", result.expect_err("should be error"));
        assert!(
            msg.contains("invalid Pallas field element in leaf"),
            "error should mention invalid leaf: {msg}"
        );
    }

    #[test]
    fn test_deserialize_invalid_retention_flags() {
        // TAG_LEAF + valid hash + invalid flags byte (0xFF has undefined bits)
        let valid_hash = crate::commitment_frontier::merkle_hash_from_bytes(&[0u8; 32])
            .expect("zero is valid Pallas element");
        let mut data = vec![TAG_LEAF];
        data.extend_from_slice(&valid_hash.to_bytes());
        data.push(0xFF); // invalid flags (only low 3 bits are defined)
        let mut pos = 0;
        let result = deserialize_tree(&data, &mut pos);
        assert!(result.is_err());
        let msg = format!("{}", result.expect_err("should be error"));
        assert!(
            msg.contains("invalid retention flags: 0xff"),
            "error should mention invalid flags: {msg}"
        );
    }

    #[test]
    fn test_deserialize_truncated_parent_annotation_flag() {
        // TAG_PARENT but no annotation flag byte follows
        let data = [TAG_PARENT];
        let mut pos = 0;
        let result = deserialize_tree(&data, &mut pos);
        assert!(result.is_err());
        let msg = format!("{}", result.expect_err("should be error"));
        assert!(
            msg.contains("truncated parent annotation flag"),
            "error should mention truncated annotation flag: {msg}"
        );
    }

    #[test]
    fn test_deserialize_truncated_parent_annotation_hash() {
        // TAG_PARENT + has_ann=0x01 but only 10 bytes of hash (needs 32)
        let mut data = vec![TAG_PARENT, 0x01];
        data.extend_from_slice(&[0u8; 10]);
        let mut pos = 0;
        let result = deserialize_tree(&data, &mut pos);
        assert!(result.is_err());
        let msg = format!("{}", result.expect_err("should be error"));
        assert!(
            msg.contains("truncated parent annotation"),
            "error should mention truncated annotation: {msg}"
        );
    }

    #[test]
    fn test_deserialize_invalid_annotation_field_element() {
        // TAG_PARENT + has_ann=0x01 + 32 bytes of 0xFF (invalid) + left=Nil + right=Nil
        let mut data = vec![TAG_PARENT, 0x01];
        data.extend_from_slice(&[0xFF; 32]);
        data.push(TAG_NIL);
        data.push(TAG_NIL);
        let mut pos = 0;
        let result = deserialize_tree(&data, &mut pos);
        assert!(result.is_err());
        let msg = format!("{}", result.expect_err("should be error"));
        assert!(
            msg.contains("invalid Pallas field element in annotation"),
            "error should mention invalid annotation: {msg}"
        );
    }

    #[test]
    fn test_deserialize_invalid_annotation_flag() {
        // TAG_PARENT + has_ann=0x42 (neither 0x00 nor 0x01)
        let data = [TAG_PARENT, 0x42];
        let mut pos = 0;
        let result = deserialize_tree(&data, &mut pos);
        assert!(result.is_err());
        let msg = format!("{}", result.expect_err("should be error"));
        assert!(
            msg.contains("invalid parent annotation flag: 0x42"),
            "error should mention invalid flag: {msg}"
        );
    }
}
