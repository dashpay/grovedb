// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Tests for replication utility functions

#[cfg(test)]
mod tests {
    use grovedb_merk::{
        proofs::{Node, Op},
        tree_type::TreeType,
    };
    use grovedb_version::version::GroveVersion;

    use crate::{
        replication::{
            utils::{
                decode_global_chunk_id, decode_vec_ops, encode_global_chunk_id, encode_vec_ops,
                pack_nested_bytes, path_to_string, unpack_nested_bytes,
            },
            CURRENT_STATE_SYNC_VERSION,
        },
        tests::make_test_grovedb,
        Element,
    };

    // -----------------------------------------------------------------------
    // path_to_string
    // -----------------------------------------------------------------------

    #[test]
    fn path_to_string_utf8() {
        let path = vec![b"root".to_vec(), b"child".to_vec(), b"leaf".to_vec()];
        let result = path_to_string(&path);
        assert_eq!(result, vec!["root", "child", "leaf"]);
    }

    #[test]
    fn path_to_string_non_utf8() {
        // 0xFF 0xFE is not valid UTF-8
        let path = vec![vec![0xFF, 0xFE], b"valid".to_vec()];
        let result = path_to_string(&path);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "<NON_UTF8_PATH>");
        assert_eq!(result[1], "valid");
    }

    #[test]
    fn path_to_string_empty_path() {
        let path: Vec<Vec<u8>> = vec![];
        let result = path_to_string(&path);
        assert!(result.is_empty(), "empty path should produce empty result");
    }

    #[test]
    fn path_to_string_empty_segment() {
        let path = vec![b"".to_vec()];
        let result = path_to_string(&path);
        assert_eq!(result, vec![""]);
    }

    // -----------------------------------------------------------------------
    // pack_nested_bytes / unpack_nested_bytes
    // -----------------------------------------------------------------------

    #[test]
    fn pack_unpack_nested_bytes_round_trip() {
        let input = vec![
            b"hello".to_vec(),
            b"world".to_vec(),
            vec![0x00, 0x01, 0x02, 0xFF],
        ];
        let packed = pack_nested_bytes(input.clone());
        let unpacked = unpack_nested_bytes(&packed).expect("should unpack valid packed data");
        assert_eq!(unpacked, input);
    }

    #[test]
    fn pack_unpack_nested_bytes_single_element() {
        let input = vec![b"only_one".to_vec()];
        let packed = pack_nested_bytes(input.clone());
        let unpacked =
            unpack_nested_bytes(&packed).expect("should unpack single-element packed data");
        assert_eq!(unpacked, input);
    }

    #[test]
    fn pack_unpack_nested_bytes_empty_inner_vecs() {
        // Packing a list that contains empty byte vectors
        let input = vec![vec![], vec![], b"data".to_vec()];
        let packed = pack_nested_bytes(input.clone());
        let unpacked =
            unpack_nested_bytes(&packed).expect("should unpack data with empty inner vecs");
        assert_eq!(unpacked, input);
    }

    #[test]
    fn pack_unpack_empty() {
        let input: Vec<Vec<u8>> = vec![];
        let packed = pack_nested_bytes(input.clone());
        let unpacked = unpack_nested_bytes(&packed).expect("should unpack empty nested bytes");
        assert_eq!(unpacked, input);
    }

    #[test]
    fn unpack_truncated_bytes_error_empty_input() {
        // Completely empty input should fail
        let result = unpack_nested_bytes(&[]);
        assert!(result.is_err(), "empty input should return an error");
    }

    #[test]
    fn unpack_truncated_bytes_error_missing_element_data() {
        // Header says 1 element, but no length/data follows
        let mut packed = vec![];
        packed.extend_from_slice(&1u16.to_be_bytes()); // num_elements = 1
                                                       // No length bytes or data follow
        let result = unpack_nested_bytes(&packed);
        assert!(
            result.is_err(),
            "truncated data (missing element length) should return an error"
        );
    }

    #[test]
    fn unpack_truncated_bytes_error_short_element_content() {
        // Header says 1 element of length 10, but only 3 bytes of content provided
        let mut packed = vec![];
        packed.extend_from_slice(&1u16.to_be_bytes()); // num_elements = 1
        packed.extend_from_slice(&10u32.to_be_bytes()); // element length = 10
        packed.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // only 3 bytes
        let result = unpack_nested_bytes(&packed);
        assert!(
            result.is_err(),
            "truncated element content should return an error"
        );
    }

    #[test]
    fn unpack_nested_bytes_extra_trailing_data_error() {
        // Valid packed data followed by extra trailing bytes should fail
        let input = vec![b"test".to_vec()];
        let mut packed = pack_nested_bytes(input);
        packed.push(0xFF); // extra byte
        let result = unpack_nested_bytes(&packed);
        assert!(
            result.is_err(),
            "extra trailing bytes should cause an error"
        );
    }

    // -----------------------------------------------------------------------
    // encode_global_chunk_id / decode_global_chunk_id
    // -----------------------------------------------------------------------

    #[test]
    fn encode_decode_global_chunk_id_root() {
        // When the global chunk id equals the app_hash, decode should return
        // the root prefix ([0u8; 32]), None root key, NormalTree, and empty
        // chunk ids.
        let app_hash = [0x42u8; 32];

        // For root, global_chunk_id == app_hash
        let (prefix, root_key, tree_type, chunk_ids) =
            decode_global_chunk_id(&app_hash, &app_hash).expect("should decode root chunk id");

        assert_eq!(prefix, [0u8; 32], "root prefix should be all zeros");
        assert!(root_key.is_none(), "root chunk should have no root key");
        assert_eq!(tree_type, TreeType::NormalTree);
        assert!(
            chunk_ids.is_empty(),
            "root chunk should have no nested chunk ids"
        );
    }

    #[test]
    fn encode_decode_global_chunk_id_non_root_no_root_key() {
        let subtree_prefix = [0xABu8; 32];
        let app_hash = [0x00u8; 32];
        let tree_type = TreeType::NormalTree;
        let chunk_ids = vec![b"chunk1".to_vec(), b"chunk2".to_vec()];

        let encoded = encode_global_chunk_id(subtree_prefix, None, tree_type, chunk_ids.clone());

        let (dec_prefix, dec_root_key, dec_tree_type, dec_chunk_ids) =
            decode_global_chunk_id(&encoded, &app_hash)
                .expect("should decode non-root chunk id without root key");

        assert_eq!(dec_prefix, subtree_prefix);
        assert!(dec_root_key.is_none(), "root key should be None");
        assert_eq!(dec_tree_type, TreeType::NormalTree);
        assert_eq!(dec_chunk_ids, chunk_ids);
    }

    #[test]
    fn encode_decode_global_chunk_id_non_root_with_root_key() {
        let subtree_prefix = [0xCDu8; 32];
        let app_hash = [0x00u8; 32];
        let root_key = b"my_root_key".to_vec();
        let tree_type = TreeType::SumTree;
        let chunk_ids = vec![b"id1".to_vec()];

        let encoded = encode_global_chunk_id(
            subtree_prefix,
            Some(root_key.clone()),
            tree_type,
            chunk_ids.clone(),
        );

        let (dec_prefix, dec_root_key, dec_tree_type, dec_chunk_ids) =
            decode_global_chunk_id(&encoded, &app_hash)
                .expect("should decode non-root chunk id with root key");

        assert_eq!(dec_prefix, subtree_prefix);
        assert_eq!(dec_root_key.expect("root key should be Some"), root_key);
        // Note: SumTree discriminant is 1, but TryFrom<u8> for TreeType maps 1 -> SumTree
        assert_eq!(dec_tree_type, TreeType::SumTree);
        assert_eq!(dec_chunk_ids, chunk_ids);
    }

    #[test]
    fn encode_decode_global_chunk_id_empty_chunk_ids() {
        let subtree_prefix = [0x11u8; 32];
        let app_hash = [0x00u8; 32];
        let tree_type = TreeType::CountTree;

        let encoded = encode_global_chunk_id(subtree_prefix, None, tree_type, vec![]);

        let (dec_prefix, dec_root_key, dec_tree_type, dec_chunk_ids) =
            decode_global_chunk_id(&encoded, &app_hash)
                .expect("should decode chunk id with empty chunk ids");

        assert_eq!(dec_prefix, subtree_prefix);
        assert!(dec_root_key.is_none());
        assert_eq!(dec_tree_type, TreeType::CountTree);
        assert!(dec_chunk_ids.is_empty());
    }

    #[test]
    fn decode_global_chunk_id_too_short_error() {
        let app_hash = [0x00u8; 32];
        // Less than 32 bytes
        let short_data = vec![0x01; 16];
        let result = decode_global_chunk_id(&short_data, &app_hash);
        assert!(
            result.is_err(),
            "data shorter than 32 bytes should return an error"
        );
    }

    #[test]
    fn decode_global_chunk_id_missing_root_key_size_error() {
        let app_hash = [0x00u8; 32];
        // Exactly 32 bytes but NOT matching app_hash, so it tries to read root_key_size
        // but there is nothing after the prefix
        let data = [0xFFu8; 32];
        let result = decode_global_chunk_id(&data, &app_hash);
        assert!(
            result.is_err(),
            "32-byte data not matching app_hash should fail (no root key size byte)"
        );
    }

    #[test]
    fn decode_global_chunk_id_truncated_root_key_error() {
        let app_hash = [0x00u8; 32];
        let mut data = vec![0xAA; 32]; // prefix
        data.push(10); // root_key_size = 10
        data.extend_from_slice(&[0x01, 0x02]); // only 2 bytes of root key (need 10)
        let result = decode_global_chunk_id(&data, &app_hash);
        assert!(
            result.is_err(),
            "truncated root key data should return an error"
        );
    }

    #[test]
    fn decode_global_chunk_id_missing_tree_type_error() {
        let app_hash = [0x00u8; 32];
        let mut data = vec![0xBBu8; 32]; // prefix
        data.push(0); // root_key_size = 0 (no root key)
                      // Missing tree type byte
        let result = decode_global_chunk_id(&data, &app_hash);
        assert!(
            result.is_err(),
            "missing tree type byte should return an error"
        );
    }

    // -----------------------------------------------------------------------
    // encode_vec_ops / decode_vec_ops
    // -----------------------------------------------------------------------

    #[test]
    fn encode_decode_vec_ops_round_trip() {
        let hash = [0x42u8; 32];
        let ops = vec![Op::Push(Node::Hash(hash)), Op::Parent, Op::Child];
        let encoded = encode_vec_ops(ops.clone()).expect("should encode ops");
        let decoded = decode_vec_ops(&encoded).expect("should decode ops");
        assert_eq!(decoded.len(), ops.len());
        // Verify each op matches
        for (original, decoded_op) in ops.iter().zip(decoded.iter()) {
            assert_eq!(
                format!("{:?}", original),
                format!("{:?}", decoded_op),
                "decoded op should match original"
            );
        }
    }

    #[test]
    fn encode_decode_vec_ops_with_kv_node() {
        let ops = vec![
            Op::Push(Node::KV(b"key1".to_vec(), b"value1".to_vec())),
            Op::Push(Node::KVHash([0xAA; 32])),
            Op::Parent,
        ];
        let encoded = encode_vec_ops(ops.clone()).expect("should encode KV ops");
        let decoded = decode_vec_ops(&encoded).expect("should decode KV ops");
        assert_eq!(decoded.len(), ops.len());
        for (original, decoded_op) in ops.iter().zip(decoded.iter()) {
            assert_eq!(
                format!("{:?}", original),
                format!("{:?}", decoded_op),
                "decoded KV op should match original"
            );
        }
    }

    #[test]
    fn encode_decode_vec_ops_empty() {
        let ops: Vec<Op> = vec![];
        let encoded = encode_vec_ops(ops).expect("should encode empty ops");
        assert!(
            encoded.is_empty(),
            "encoding empty ops should produce empty bytes"
        );
        let decoded = decode_vec_ops(&encoded).expect("should decode empty ops");
        assert!(
            decoded.is_empty(),
            "decoding empty bytes should produce empty ops"
        );
    }

    #[test]
    fn decode_vec_ops_garbage_error() {
        // Random garbage bytes that do not form a valid Op encoding
        let garbage = vec![0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA];
        let result = decode_vec_ops(&garbage);
        assert!(
            result.is_err(),
            "garbage input should return a decode error"
        );
    }

    #[test]
    fn encode_decode_vec_ops_inverted_ops() {
        let ops = vec![
            Op::PushInverted(Node::Hash([0x01; 32])),
            Op::ParentInverted,
            Op::ChildInverted,
        ];
        let encoded = encode_vec_ops(ops.clone()).expect("should encode inverted ops");
        let decoded = decode_vec_ops(&encoded).expect("should decode inverted ops");
        assert_eq!(decoded.len(), ops.len());
        for (original, decoded_op) in ops.iter().zip(decoded.iter()) {
            assert_eq!(
                format!("{:?}", original),
                format!("{:?}", decoded_op),
                "decoded inverted op should match original"
            );
        }
    }

    // -----------------------------------------------------------------------
    // fetch_chunk
    // -----------------------------------------------------------------------

    #[test]
    fn fetch_chunk_version_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert something so the DB has a valid root hash
        db.insert(
            [crate::tests::TEST_LEAF].as_ref(),
            b"key1",
            Element::new_item(b"value1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        // Use an unsupported version (0 is not CURRENT_STATE_SYNC_VERSION which is 1)
        let result = db.fetch_chunk(&root_hash, None, 0, grove_version);
        assert!(
            result.is_err(),
            "fetch_chunk with unsupported version should fail"
        );
        let err_msg = format!("{:?}", result.expect_err("should be error"));
        assert!(
            err_msg.contains("Unsupported state sync protocol version"),
            "error should mention unsupported version, got: {}",
            err_msg
        );
    }

    #[test]
    fn fetch_chunk_with_valid_version() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item so the tree is not empty
        db.insert(
            [crate::tests::TEST_LEAF].as_ref(),
            b"key1",
            Element::new_item(b"value1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        // Use the current supported version
        let result = db.fetch_chunk(&root_hash, None, CURRENT_STATE_SYNC_VERSION, grove_version);
        assert!(
            result.is_ok(),
            "fetch_chunk with valid version should succeed, got: {:?}",
            result.err()
        );
        let chunk_data = result.expect("should have chunk data");
        assert!(
            !chunk_data.is_empty(),
            "fetched chunk data should not be empty for a non-empty tree"
        );
    }

    #[test]
    fn fetch_chunk_with_future_version() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let root_hash = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        // Use a hypothetical future version
        let result = db.fetch_chunk(
            &root_hash,
            None,
            CURRENT_STATE_SYNC_VERSION + 1,
            grove_version,
        );
        assert!(
            result.is_err(),
            "fetch_chunk with future version should fail"
        );
    }

    // -----------------------------------------------------------------------
    // Round-trip integration: encode_global_chunk_id -> pack -> unpack -> decode
    // -----------------------------------------------------------------------

    #[test]
    fn global_chunk_id_pack_unpack_integration() {
        // Encode a global chunk id, pack it into nested bytes, unpack, and decode
        let subtree_prefix = [0x77u8; 32];
        let app_hash = [0x00u8; 32];
        let root_key = Some(b"rk".to_vec());
        let tree_type = TreeType::NormalTree;
        let chunk_ids = vec![b"c1".to_vec(), b"c2".to_vec()];

        let encoded = encode_global_chunk_id(
            subtree_prefix,
            root_key.clone(),
            tree_type,
            chunk_ids.clone(),
        );

        // Pack into a nested bytes structure
        let packed = pack_nested_bytes(vec![encoded.clone()]);
        let unpacked = unpack_nested_bytes(&packed).expect("should unpack nested global chunk id");
        assert_eq!(unpacked.len(), 1);
        assert_eq!(unpacked[0], encoded);

        // Decode back
        let (dec_prefix, dec_root_key, dec_tree_type, dec_chunk_ids) =
            decode_global_chunk_id(&unpacked[0], &app_hash)
                .expect("should decode global chunk id from unpacked data");
        assert_eq!(dec_prefix, subtree_prefix);
        assert_eq!(dec_root_key, root_key);
        assert_eq!(dec_tree_type, tree_type);
        assert_eq!(dec_chunk_ids, chunk_ids);
    }
}
