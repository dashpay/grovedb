//! Proofs encoding

use std::io::{Read, Write};

use ed::{Decode, Encode, Error as EdError, Terminated};

use super::{Node, Op};
use crate::{
    error::Error,
    proofs::{TreeFeatureType, HASH_LENGTH},
};

/// Maximum allowed value length for large value variants (64MB).
/// This prevents DoS attacks via malicious proofs specifying unreasonably large
/// allocations.
const MAX_VALUE_LEN: u32 = 64 * 1024 * 1024;

impl Encode for Op {
    // Note: `key.len() as u8` casts below are safe because GroveDB enforces a
    // 255-byte maximum key length at insertion time (both direct insert and
    // batch paths). The `debug_assert!` guards serve as development-time
    // verification of this invariant. A runtime check here is unnecessary
    // since keys exceeding 255 bytes can never be stored in the database.
    fn encode_into<W: Write>(&self, dest: &mut W) -> ed::Result<()> {
        match self {
            // Push
            Op::Push(Node::Hash(hash)) => {
                dest.write_all(&[0x01])?;
                dest.write_all(hash)?;
            }
            Op::Push(Node::KVHash(kv_hash)) => {
                dest.write_all(&[0x02])?;
                dest.write_all(kv_hash)?;
            }
            Op::Push(Node::KV(key, value)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x03, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                } else {
                    dest.write_all(&[0x20, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                }
            }
            Op::Push(Node::KVValueHash(key, value, value_hash)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x04, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                } else {
                    dest.write_all(&[0x21, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                }
            }
            Op::Push(Node::KVDigest(key, value_hash)) => {
                debug_assert!(key.len() < 256);

                dest.write_all(&[0x05, key.len() as u8])?;
                dest.write_all(key)?;
                dest.write_all(value_hash)?;
            }
            Op::Push(Node::KVRefValueHash(key, value, value_hash)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x06, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                } else {
                    dest.write_all(&[0x22, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                }
            }
            Op::Push(Node::KVValueHashFeatureType(key, value, value_hash, feature_type)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x07, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    feature_type.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x23, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    feature_type.encode_into(dest)?;
                }
            }
            Op::Push(Node::KVCount(key, value, count)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x14, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    count.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x24, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    count.encode_into(dest)?;
                }
            }
            Op::Push(Node::KVHashCount(kv_hash, count)) => {
                dest.write_all(&[0x15])?;
                dest.write_all(kv_hash)?;
                count.encode_into(dest)?;
            }
            Op::Push(Node::KVRefValueHashCount(key, value, value_hash, count)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x18, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    count.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x25, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    count.encode_into(dest)?;
                }
            }
            Op::Push(Node::KVDigestCount(key, value_hash, count)) => {
                debug_assert!(key.len() < 256);

                dest.write_all(&[0x1a, key.len() as u8])?;
                dest.write_all(key)?;
                dest.write_all(value_hash)?;
                count.encode_into(dest)?;
            }
            Op::Push(Node::HashWithCount(kv_hash, left_child_hash, right_child_hash, count)) => {
                dest.write_all(&[0x1e])?;
                dest.write_all(kv_hash)?;
                dest.write_all(left_child_hash)?;
                dest.write_all(right_child_hash)?;
                count.encode_into(dest)?;
            }
            Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
                key,
                value,
                value_hash,
                feature_type,
                child_hash,
            )) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x1c, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    feature_type.encode_into(dest)?;
                    dest.write_all(child_hash)?;
                } else {
                    dest.write_all(&[0x2e, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    feature_type.encode_into(dest)?;
                    dest.write_all(child_hash)?;
                }
            }

            // PushInverted
            Op::PushInverted(Node::Hash(hash)) => {
                dest.write_all(&[0x08])?;
                dest.write_all(hash)?;
            }
            Op::PushInverted(Node::KVHash(kv_hash)) => {
                dest.write_all(&[0x09])?;
                dest.write_all(kv_hash)?;
            }
            Op::PushInverted(Node::KV(key, value)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x0a, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                } else {
                    dest.write_all(&[0x28, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                }
            }
            Op::PushInverted(Node::KVValueHash(key, value, value_hash)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x0b, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                } else {
                    dest.write_all(&[0x29, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                }
            }
            Op::PushInverted(Node::KVDigest(key, value_hash)) => {
                debug_assert!(key.len() < 256);

                dest.write_all(&[0x0c, key.len() as u8])?;
                dest.write_all(key)?;
                dest.write_all(value_hash)?;
            }
            Op::PushInverted(Node::KVRefValueHash(key, value, value_hash)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x0d, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                } else {
                    dest.write_all(&[0x2a, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                }
            }
            Op::PushInverted(Node::KVValueHashFeatureType(
                key,
                value,
                value_hash,
                feature_type,
            )) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x0e, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    feature_type.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x2b, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    feature_type.encode_into(dest)?;
                }
            }
            Op::PushInverted(Node::KVCount(key, value, count)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x16, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    count.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x2c, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    count.encode_into(dest)?;
                }
            }
            Op::PushInverted(Node::KVHashCount(kv_hash, count)) => {
                dest.write_all(&[0x17])?;
                dest.write_all(kv_hash)?;
                count.encode_into(dest)?;
            }
            Op::PushInverted(Node::KVRefValueHashCount(key, value, value_hash, count)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x19, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    count.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x2d, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    count.encode_into(dest)?;
                }
            }
            Op::PushInverted(Node::KVDigestCount(key, value_hash, count)) => {
                debug_assert!(key.len() < 256);

                dest.write_all(&[0x1b, key.len() as u8])?;
                dest.write_all(key)?;
                dest.write_all(value_hash)?;
                count.encode_into(dest)?;
            }
            Op::PushInverted(Node::HashWithCount(
                kv_hash,
                left_child_hash,
                right_child_hash,
                count,
            )) => {
                dest.write_all(&[0x1f])?;
                dest.write_all(kv_hash)?;
                dest.write_all(left_child_hash)?;
                dest.write_all(right_child_hash)?;
                count.encode_into(dest)?;
            }
            Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
                key,
                value,
                value_hash,
                feature_type,
                child_hash,
            )) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x1d, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    feature_type.encode_into(dest)?;
                    dest.write_all(child_hash)?;
                } else {
                    dest.write_all(&[0x2f, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    feature_type.encode_into(dest)?;
                    dest.write_all(child_hash)?;
                }
            }

            // ProvableSumTree proof variants. Tag bytes 0x30..=0x3D
            // (0x3E and 0x3F intentionally reserved). Layout mirrors the
            // corresponding Count variants verbatim; only the encoded
            // aggregate type changes (i64 sum via varint instead of u64
            // count). The sum field uses varint for wire compactness — the
            // hash recomputation in `node_hash_with_sum` uses the fixed
            // big-endian byte form, which is independent of the wire
            // encoding.

            // Push: ProvableSumTree variants
            Op::Push(Node::KVSum(key, value, sum)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x30, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    sum.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x31, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    sum.encode_into(dest)?;
                }
            }
            Op::Push(Node::KVHashSum(kv_hash, sum)) => {
                dest.write_all(&[0x32])?;
                dest.write_all(kv_hash)?;
                sum.encode_into(dest)?;
            }
            Op::Push(Node::KVRefValueHashSum(key, value, value_hash, sum)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x33, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    sum.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x34, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    sum.encode_into(dest)?;
                }
            }
            Op::Push(Node::KVDigestSum(key, value_hash, sum)) => {
                debug_assert!(key.len() < 256);

                dest.write_all(&[0x35, key.len() as u8])?;
                dest.write_all(key)?;
                dest.write_all(value_hash)?;
                sum.encode_into(dest)?;
            }
            Op::Push(Node::HashWithSum(kv_hash, left_child_hash, right_child_hash, sum)) => {
                dest.write_all(&[0x36])?;
                dest.write_all(kv_hash)?;
                dest.write_all(left_child_hash)?;
                dest.write_all(right_child_hash)?;
                sum.encode_into(dest)?;
            }

            // PushInverted: ProvableSumTree variants
            Op::PushInverted(Node::KVSum(key, value, sum)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x37, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    sum.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x38, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    sum.encode_into(dest)?;
                }
            }
            Op::PushInverted(Node::KVHashSum(kv_hash, sum)) => {
                dest.write_all(&[0x39])?;
                dest.write_all(kv_hash)?;
                sum.encode_into(dest)?;
            }
            Op::PushInverted(Node::KVRefValueHashSum(key, value, value_hash, sum)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x3a, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    sum.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x3b, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    sum.encode_into(dest)?;
                }
            }
            Op::PushInverted(Node::KVDigestSum(key, value_hash, sum)) => {
                debug_assert!(key.len() < 256);

                dest.write_all(&[0x3c, key.len() as u8])?;
                dest.write_all(key)?;
                dest.write_all(value_hash)?;
                sum.encode_into(dest)?;
            }
            Op::PushInverted(Node::HashWithSum(
                kv_hash,
                left_child_hash,
                right_child_hash,
                sum,
            )) => {
                dest.write_all(&[0x3d])?;
                dest.write_all(kv_hash)?;
                dest.write_all(left_child_hash)?;
                dest.write_all(right_child_hash)?;
                sum.encode_into(dest)?;
            }

            // ProvableCountProvableSumTree proof variants. Tag bytes
            // 0x40..=0x4D mirror the ProvableSumTree layout (0x30..=0x3D)
            // but carry BOTH a varint u64 count AND a varint i64 sum
            // immediately after the value-bearing fields. The hash
            // recomputation in `node_hash_with_count_and_sum` uses the
            // fixed 8-byte big-endian byte form of each aggregate, which
            // is independent of the wire encoding.

            // Push: ProvableCountProvableSumTree variants
            Op::Push(Node::KVCountSum(key, value, count, sum)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x40, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    count.encode_into(dest)?;
                    sum.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x41, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    count.encode_into(dest)?;
                    sum.encode_into(dest)?;
                }
            }
            Op::Push(Node::KVHashCountSum(kv_hash, count, sum)) => {
                dest.write_all(&[0x42])?;
                dest.write_all(kv_hash)?;
                count.encode_into(dest)?;
                sum.encode_into(dest)?;
            }
            Op::Push(Node::KVRefValueHashCountSum(key, value, value_hash, count, sum)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x43, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    count.encode_into(dest)?;
                    sum.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x44, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    count.encode_into(dest)?;
                    sum.encode_into(dest)?;
                }
            }
            Op::Push(Node::KVDigestCountSum(key, value_hash, count, sum)) => {
                debug_assert!(key.len() < 256);

                dest.write_all(&[0x45, key.len() as u8])?;
                dest.write_all(key)?;
                dest.write_all(value_hash)?;
                count.encode_into(dest)?;
                sum.encode_into(dest)?;
            }
            Op::Push(Node::HashWithCountAndSum(
                kv_hash,
                left_child_hash,
                right_child_hash,
                count,
                sum,
            )) => {
                dest.write_all(&[0x46])?;
                dest.write_all(kv_hash)?;
                dest.write_all(left_child_hash)?;
                dest.write_all(right_child_hash)?;
                count.encode_into(dest)?;
                sum.encode_into(dest)?;
            }

            // PushInverted: ProvableCountProvableSumTree variants
            Op::PushInverted(Node::KVCountSum(key, value, count, sum)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x47, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    count.encode_into(dest)?;
                    sum.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x48, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    count.encode_into(dest)?;
                    sum.encode_into(dest)?;
                }
            }
            Op::PushInverted(Node::KVHashCountSum(kv_hash, count, sum)) => {
                dest.write_all(&[0x49])?;
                dest.write_all(kv_hash)?;
                count.encode_into(dest)?;
                sum.encode_into(dest)?;
            }
            Op::PushInverted(Node::KVRefValueHashCountSum(key, value, value_hash, count, sum)) => {
                debug_assert!(key.len() < 256);
                if value.len() < 65536 {
                    dest.write_all(&[0x4a, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u16).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    count.encode_into(dest)?;
                    sum.encode_into(dest)?;
                } else {
                    dest.write_all(&[0x4b, key.len() as u8])?;
                    dest.write_all(key)?;
                    (value.len() as u32).encode_into(dest)?;
                    dest.write_all(value)?;
                    dest.write_all(value_hash)?;
                    count.encode_into(dest)?;
                    sum.encode_into(dest)?;
                }
            }
            Op::PushInverted(Node::KVDigestCountSum(key, value_hash, count, sum)) => {
                debug_assert!(key.len() < 256);

                dest.write_all(&[0x4c, key.len() as u8])?;
                dest.write_all(key)?;
                dest.write_all(value_hash)?;
                count.encode_into(dest)?;
                sum.encode_into(dest)?;
            }
            Op::PushInverted(Node::HashWithCountAndSum(
                kv_hash,
                left_child_hash,
                right_child_hash,
                count,
                sum,
            )) => {
                dest.write_all(&[0x4d])?;
                dest.write_all(kv_hash)?;
                dest.write_all(left_child_hash)?;
                dest.write_all(right_child_hash)?;
                count.encode_into(dest)?;
                sum.encode_into(dest)?;
            }

            Op::Parent => dest.write_all(&[0x10])?,
            Op::Child => dest.write_all(&[0x11])?,
            Op::ParentInverted => dest.write_all(&[0x12])?,
            Op::ChildInverted => dest.write_all(&[0x13])?,
        };
        Ok(())
    }

    fn encoding_length(&self) -> ed::Result<usize> {
        Ok(match self {
            Op::Push(Node::Hash(_)) => 1 + HASH_LENGTH,
            Op::Push(Node::KVHash(_)) => 1 + HASH_LENGTH,
            Op::Push(Node::KVDigest(key, _)) => 2 + key.len() + HASH_LENGTH,
            Op::Push(Node::KV(key, value)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len()
            }
            Op::Push(Node::KVValueHash(key, value, _)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + HASH_LENGTH
            }
            Op::Push(Node::KVRefValueHash(key, value, _)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + HASH_LENGTH
            }
            Op::Push(Node::KVValueHashFeatureType(key, value, _, feature_type)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + HASH_LENGTH + feature_type.encoding_length()?
            }
            Op::Push(Node::KVCount(key, value, count)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + count.encoding_length()?
            }
            Op::Push(Node::KVHashCount(_, count)) => 1 + HASH_LENGTH + count.encoding_length()?,
            Op::Push(Node::KVRefValueHashCount(key, value, _, count)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + HASH_LENGTH + count.encoding_length()?
            }
            Op::Push(Node::KVDigestCount(key, _, count)) => {
                2 + key.len() + HASH_LENGTH + count.encoding_length()?
            }
            Op::Push(Node::HashWithCount(_, _, _, count)) => {
                1 + 3 * HASH_LENGTH + count.encoding_length()?
            }
            Op::Push(Node::KVValueHashFeatureTypeWithChildHash(key, value, _, feature_type, _)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header
                    + key.len()
                    + value.len()
                    + HASH_LENGTH
                    + feature_type.encoding_length()?
                    + HASH_LENGTH
            }
            Op::PushInverted(Node::Hash(_)) => 1 + HASH_LENGTH,
            Op::PushInverted(Node::KVHash(_)) => 1 + HASH_LENGTH,
            Op::PushInverted(Node::KVDigest(key, _)) => 2 + key.len() + HASH_LENGTH,
            Op::PushInverted(Node::KV(key, value)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len()
            }
            Op::PushInverted(Node::KVValueHash(key, value, _)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + HASH_LENGTH
            }
            Op::PushInverted(Node::KVRefValueHash(key, value, _)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + HASH_LENGTH
            }
            Op::PushInverted(Node::KVValueHashFeatureType(key, value, _, feature_type)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + HASH_LENGTH + feature_type.encoding_length()?
            }
            Op::PushInverted(Node::KVCount(key, value, count)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + count.encoding_length()?
            }
            Op::PushInverted(Node::KVHashCount(_, count)) => {
                1 + HASH_LENGTH + count.encoding_length()?
            }
            Op::PushInverted(Node::KVRefValueHashCount(key, value, _, count)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + HASH_LENGTH + count.encoding_length()?
            }
            Op::PushInverted(Node::KVDigestCount(key, _, count)) => {
                2 + key.len() + HASH_LENGTH + count.encoding_length()?
            }
            Op::PushInverted(Node::HashWithCount(_, _, _, count)) => {
                1 + 3 * HASH_LENGTH + count.encoding_length()?
            }
            Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
                key,
                value,
                _,
                feature_type,
                _,
            )) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header
                    + key.len()
                    + value.len()
                    + HASH_LENGTH
                    + feature_type.encoding_length()?
                    + HASH_LENGTH
            }
            // ProvableSumTree variants — Push (sum is i64 varint)
            Op::Push(Node::KVSum(key, value, sum)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + sum.encoding_length()?
            }
            Op::Push(Node::KVHashSum(_, sum)) => 1 + HASH_LENGTH + sum.encoding_length()?,
            Op::Push(Node::KVRefValueHashSum(key, value, _, sum)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + HASH_LENGTH + sum.encoding_length()?
            }
            Op::Push(Node::KVDigestSum(key, _, sum)) => {
                2 + key.len() + HASH_LENGTH + sum.encoding_length()?
            }
            Op::Push(Node::HashWithSum(_, _, _, sum)) => {
                1 + 3 * HASH_LENGTH + sum.encoding_length()?
            }
            // ProvableSumTree variants — PushInverted
            Op::PushInverted(Node::KVSum(key, value, sum)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + sum.encoding_length()?
            }
            Op::PushInverted(Node::KVHashSum(_, sum)) => 1 + HASH_LENGTH + sum.encoding_length()?,
            Op::PushInverted(Node::KVRefValueHashSum(key, value, _, sum)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header + key.len() + value.len() + HASH_LENGTH + sum.encoding_length()?
            }
            Op::PushInverted(Node::KVDigestSum(key, _, sum)) => {
                2 + key.len() + HASH_LENGTH + sum.encoding_length()?
            }
            Op::PushInverted(Node::HashWithSum(_, _, _, sum)) => {
                1 + 3 * HASH_LENGTH + sum.encoding_length()?
            }
            // ProvableCountProvableSumTree variants — Push
            Op::Push(Node::KVCountSum(key, value, count, sum)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header
                    + key.len()
                    + value.len()
                    + count.encoding_length()?
                    + sum.encoding_length()?
            }
            Op::Push(Node::KVHashCountSum(_, count, sum)) => {
                1 + HASH_LENGTH + count.encoding_length()? + sum.encoding_length()?
            }
            Op::Push(Node::KVRefValueHashCountSum(key, value, _, count, sum)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header
                    + key.len()
                    + value.len()
                    + HASH_LENGTH
                    + count.encoding_length()?
                    + sum.encoding_length()?
            }
            Op::Push(Node::KVDigestCountSum(key, _, count, sum)) => {
                2 + key.len() + HASH_LENGTH + count.encoding_length()? + sum.encoding_length()?
            }
            Op::Push(Node::HashWithCountAndSum(_, _, _, count, sum)) => {
                1 + 3 * HASH_LENGTH + count.encoding_length()? + sum.encoding_length()?
            }
            // ProvableCountProvableSumTree variants — PushInverted
            Op::PushInverted(Node::KVCountSum(key, value, count, sum)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header
                    + key.len()
                    + value.len()
                    + count.encoding_length()?
                    + sum.encoding_length()?
            }
            Op::PushInverted(Node::KVHashCountSum(_, count, sum)) => {
                1 + HASH_LENGTH + count.encoding_length()? + sum.encoding_length()?
            }
            Op::PushInverted(Node::KVRefValueHashCountSum(key, value, _, count, sum)) => {
                let header = if value.len() < 65536 { 4 } else { 6 };
                header
                    + key.len()
                    + value.len()
                    + HASH_LENGTH
                    + count.encoding_length()?
                    + sum.encoding_length()?
            }
            Op::PushInverted(Node::KVDigestCountSum(key, _, count, sum)) => {
                2 + key.len() + HASH_LENGTH + count.encoding_length()? + sum.encoding_length()?
            }
            Op::PushInverted(Node::HashWithCountAndSum(_, _, _, count, sum)) => {
                1 + 3 * HASH_LENGTH + count.encoding_length()? + sum.encoding_length()?
            }
            Op::Parent => 1,
            Op::Child => 1,
            Op::ParentInverted => 1,
            Op::ChildInverted => 1,
        })
    }
}

impl Decode for Op {
    fn decode<R: Read>(mut input: R) -> ed::Result<Self> {
        let variant: u8 = Decode::decode(&mut input)?;

        Ok(match variant {
            0x01 => {
                let mut hash = [0; HASH_LENGTH];
                input.read_exact(&mut hash)?;
                Self::Push(Node::Hash(hash))
            }
            0x02 => {
                let mut hash = [0; HASH_LENGTH];
                input.read_exact(&mut hash)?;
                Self::Push(Node::KVHash(hash))
            }
            0x03 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                Self::Push(Node::KV(key, value))
            }
            0x04 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                Self::Push(Node::KVValueHash(key, value, value_hash))
            }
            0x05 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                Self::Push(Node::KVDigest(key, value_hash))
            }
            0x06 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                Self::Push(Node::KVRefValueHash(key, value, value_hash))
            }
            0x07 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let tree_feature_type = TreeFeatureType::decode(input)?;
                Self::Push(Node::KVValueHashFeatureType(
                    key,
                    value,
                    value_hash,
                    tree_feature_type,
                ))
            }
            0x08 => {
                let mut hash = [0; HASH_LENGTH];
                input.read_exact(&mut hash)?;
                Self::PushInverted(Node::Hash(hash))
            }
            0x09 => {
                let mut hash = [0; HASH_LENGTH];
                input.read_exact(&mut hash)?;
                Self::PushInverted(Node::KVHash(hash))
            }
            0x0a => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                Self::PushInverted(Node::KV(key, value))
            }
            0x0b => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                Self::PushInverted(Node::KVValueHash(key, value, value_hash))
            }
            0x0c => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                Self::PushInverted(Node::KVDigest(key, value_hash))
            }
            0x0d => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                Self::PushInverted(Node::KVRefValueHash(key, value, value_hash))
            }
            0x0e => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let tree_feature_type = TreeFeatureType::decode(input)?;
                Self::PushInverted(Node::KVValueHashFeatureType(
                    key,
                    value,
                    value_hash,
                    tree_feature_type,
                ))
            }
            0x14 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let count: u64 = Decode::decode(&mut input)?;

                Self::Push(Node::KVCount(key, value, count))
            }
            0x15 => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let count: u64 = Decode::decode(&mut input)?;

                Self::Push(Node::KVHashCount(kv_hash, count))
            }
            0x16 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let count: u64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::KVCount(key, value, count))
            }
            0x17 => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let count: u64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::KVHashCount(kv_hash, count))
            }
            0x18 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                Self::Push(Node::KVRefValueHashCount(key, value, value_hash, count))
            }
            0x19 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                Self::PushInverted(Node::KVRefValueHashCount(key, value, value_hash, count))
            }
            0x1a => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                Self::Push(Node::KVDigestCount(key, value_hash, count))
            }
            0x1b => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                Self::PushInverted(Node::KVDigestCount(key, value_hash, count))
            }
            0x1c => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let tree_feature_type = TreeFeatureType::decode(&mut input)?;

                let mut child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut child_hash)?;

                Self::Push(Node::KVValueHashFeatureTypeWithChildHash(
                    key,
                    value,
                    value_hash,
                    tree_feature_type,
                    child_hash,
                ))
            }
            0x1e => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let mut left_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut left_child_hash)?;
                let mut right_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut right_child_hash)?;
                let count: u64 = Decode::decode(&mut input)?;

                Self::Push(Node::HashWithCount(
                    kv_hash,
                    left_child_hash,
                    right_child_hash,
                    count,
                ))
            }
            0x1f => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let mut left_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut left_child_hash)?;
                let mut right_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut right_child_hash)?;
                let count: u64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::HashWithCount(
                    kv_hash,
                    left_child_hash,
                    right_child_hash,
                    count,
                ))
            }
            0x1d => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let tree_feature_type = TreeFeatureType::decode(&mut input)?;

                let mut child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut child_hash)?;

                Self::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
                    key,
                    value,
                    value_hash,
                    tree_feature_type,
                    child_hash,
                ))
            }

            // Large value variants (value_len as u32)
            // Push large variants: 0x20-0x25
            0x20 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x20));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                Self::Push(Node::KV(key, value))
            }
            0x21 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x21));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                Self::Push(Node::KVValueHash(key, value, value_hash))
            }
            0x22 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x22));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                Self::Push(Node::KVRefValueHash(key, value, value_hash))
            }
            0x23 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x23));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let tree_feature_type = TreeFeatureType::decode(input)?;
                Self::Push(Node::KVValueHashFeatureType(
                    key,
                    value,
                    value_hash,
                    tree_feature_type,
                ))
            }
            0x24 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x24));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let count: u64 = Decode::decode(&mut input)?;

                Self::Push(Node::KVCount(key, value, count))
            }
            0x25 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x25));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                Self::Push(Node::KVRefValueHashCount(key, value, value_hash, count))
            }

            // PushInverted large variants: 0x28-0x2d
            0x28 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x28));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                Self::PushInverted(Node::KV(key, value))
            }
            0x29 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x29));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                Self::PushInverted(Node::KVValueHash(key, value, value_hash))
            }
            0x2a => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x2a));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                Self::PushInverted(Node::KVRefValueHash(key, value, value_hash))
            }
            0x2b => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x2b));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let tree_feature_type = TreeFeatureType::decode(input)?;
                Self::PushInverted(Node::KVValueHashFeatureType(
                    key,
                    value,
                    value_hash,
                    tree_feature_type,
                ))
            }
            0x2c => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x2c));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let count: u64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::KVCount(key, value, count))
            }
            0x2d => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x2d));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                Self::PushInverted(Node::KVRefValueHashCount(key, value, value_hash, count))
            }
            0x2e => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x2e));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let tree_feature_type = TreeFeatureType::decode(&mut input)?;

                let mut child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut child_hash)?;

                Self::Push(Node::KVValueHashFeatureTypeWithChildHash(
                    key,
                    value,
                    value_hash,
                    tree_feature_type,
                    child_hash,
                ))
            }
            0x2f => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x2f));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let tree_feature_type = TreeFeatureType::decode(&mut input)?;

                let mut child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut child_hash)?;

                Self::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
                    key,
                    value,
                    value_hash,
                    tree_feature_type,
                    child_hash,
                ))
            }

            // ProvableSumTree decoder arms. Mirror the Count family layout
            // exactly; only the aggregate type differs (i64 sum via varint
            // instead of u64 count).
            0x30 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let sum: i64 = Decode::decode(&mut input)?;

                Self::Push(Node::KVSum(key, value, sum))
            }
            0x31 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x31));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let sum: i64 = Decode::decode(&mut input)?;

                Self::Push(Node::KVSum(key, value, sum))
            }
            0x32 => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::Push(Node::KVHashSum(kv_hash, sum))
            }
            0x33 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let sum: i64 = Decode::decode(&mut input)?;
                Self::Push(Node::KVRefValueHashSum(key, value, value_hash, sum))
            }
            0x34 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x34));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let sum: i64 = Decode::decode(&mut input)?;
                Self::Push(Node::KVRefValueHashSum(key, value, value_hash, sum))
            }
            0x35 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let sum: i64 = Decode::decode(&mut input)?;
                Self::Push(Node::KVDigestSum(key, value_hash, sum))
            }
            0x36 => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let mut left_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut left_child_hash)?;
                let mut right_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut right_child_hash)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::Push(Node::HashWithSum(
                    kv_hash,
                    left_child_hash,
                    right_child_hash,
                    sum,
                ))
            }
            0x37 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let sum: i64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::KVSum(key, value, sum))
            }
            0x38 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x38));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let sum: i64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::KVSum(key, value, sum))
            }
            0x39 => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::KVHashSum(kv_hash, sum))
            }
            0x3a => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let sum: i64 = Decode::decode(&mut input)?;
                Self::PushInverted(Node::KVRefValueHashSum(key, value, value_hash, sum))
            }
            0x3b => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x3b));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let sum: i64 = Decode::decode(&mut input)?;
                Self::PushInverted(Node::KVRefValueHashSum(key, value, value_hash, sum))
            }
            0x3c => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let sum: i64 = Decode::decode(&mut input)?;
                Self::PushInverted(Node::KVDigestSum(key, value_hash, sum))
            }
            0x3d => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let mut left_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut left_child_hash)?;
                let mut right_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut right_child_hash)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::HashWithSum(
                    kv_hash,
                    left_child_hash,
                    right_child_hash,
                    sum,
                ))
            }

            // ProvableCountProvableSumTree decoder arms. Mirror the
            // Count and Sum families' layouts; each variant carries the
            // count (varint u64) followed by the sum (varint i64).
            0x40 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::Push(Node::KVCountSum(key, value, count, sum))
            }
            0x41 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x41));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::Push(Node::KVCountSum(key, value, count, sum))
            }
            0x42 => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::Push(Node::KVHashCountSum(kv_hash, count, sum))
            }
            0x43 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;
                Self::Push(Node::KVRefValueHashCountSum(
                    key, value, value_hash, count, sum,
                ))
            }
            0x44 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x44));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;
                Self::Push(Node::KVRefValueHashCountSum(
                    key, value, value_hash, count, sum,
                ))
            }
            0x45 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;
                Self::Push(Node::KVDigestCountSum(key, value_hash, count, sum))
            }
            0x46 => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let mut left_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut left_child_hash)?;
                let mut right_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut right_child_hash)?;
                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::Push(Node::HashWithCountAndSum(
                    kv_hash,
                    left_child_hash,
                    right_child_hash,
                    count,
                    sum,
                ))
            }
            0x47 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::KVCountSum(key, value, count, sum))
            }
            0x48 => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x48));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::KVCountSum(key, value, count, sum))
            }
            0x49 => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::KVHashCountSum(kv_hash, count, sum))
            }
            0x4a => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u16 = Decode::decode(&mut input)?;
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;
                Self::PushInverted(Node::KVRefValueHashCountSum(
                    key, value, value_hash, count, sum,
                ))
            }
            0x4b => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let value_len: u32 = Decode::decode(&mut input)?;
                if value_len > MAX_VALUE_LEN {
                    return Err(ed::Error::UnexpectedByte(0x4b));
                }
                let mut value = vec![0; value_len as usize];
                input.read_exact(value.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;
                Self::PushInverted(Node::KVRefValueHashCountSum(
                    key, value, value_hash, count, sum,
                ))
            }
            0x4c => {
                let key_len: u8 = Decode::decode(&mut input)?;
                let mut key = vec![0; key_len as usize];
                input.read_exact(key.as_mut_slice())?;

                let mut value_hash = [0; HASH_LENGTH];
                input.read_exact(&mut value_hash)?;

                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;
                Self::PushInverted(Node::KVDigestCountSum(key, value_hash, count, sum))
            }
            0x4d => {
                let mut kv_hash = [0; HASH_LENGTH];
                input.read_exact(&mut kv_hash)?;
                let mut left_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut left_child_hash)?;
                let mut right_child_hash = [0; HASH_LENGTH];
                input.read_exact(&mut right_child_hash)?;
                let count: u64 = Decode::decode(&mut input)?;
                let sum: i64 = Decode::decode(&mut input)?;

                Self::PushInverted(Node::HashWithCountAndSum(
                    kv_hash,
                    left_child_hash,
                    right_child_hash,
                    count,
                    sum,
                ))
            }

            0x10 => Self::Parent,
            0x11 => Self::Child,
            0x12 => Self::ParentInverted,
            0x13 => Self::ChildInverted,
            _ => return Err(ed::Error::UnexpectedByte(variant)),
        })
    }
}

impl Terminated for Op {}

impl Op {
    fn encode_into_with_error<W: std::io::Write>(&self, dest: &mut W) -> Result<(), Error> {
        Encode::encode_into(self, dest).map_err(|e| match e {
            EdError::UnexpectedByte(byte) => Error::ProofCreationError(format!(
                "failed to encode a proofs::Op structure (UnexpectedByte: {byte})"
            )),
            EdError::IOError(error) => Error::ProofCreationError(format!(
                "failed to encode a proofs::Op structure ({error})"
            )),
        })
    }

    /// Get the encoding length of this Op
    pub fn encoding_length(&self) -> usize {
        Encode::encoding_length(self).expect("encoding length should not fail")
    }

    /// Decode an Op from bytes
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        Decode::decode(bytes).map_err(|e| match e {
            EdError::UnexpectedByte(byte) => Error::InvalidProofError(format!(
                "failed to decode an proofs::Op structure (UnexpectedByte: {byte})"
            )),
            EdError::IOError(error) => Error::InvalidProofError(format!(
                "failed to decode an proofs::Op structure ({error})"
            )),
        })
    }
}

/// Encode a sequence of Ops into a byte vector.
///
/// # Panics
/// Panics if encoding fails — this is unreachable when writing to a `Vec<u8>`
/// since `Vec<u8>: Write` never returns IO errors. The only theoretical error
/// source is `ed::Error::UnexpectedByte`, which would indicate a bug in the
/// encoding logic itself.
pub fn encode_into<'a, T: Iterator<Item = &'a Op>>(ops: T, output: &mut Vec<u8>) {
    for op in ops {
        op.encode_into_with_error(output)
            .expect("encoding into Vec<u8> is infallible");
    }
}

/// Decoder iterates over proof bytes, yielding Op values
pub struct Decoder<'a> {
    offset: usize,
    bytes: &'a [u8],
}

impl<'a> Decoder<'a> {
    /// Create a new Decoder from proof bytes
    pub const fn new(proof_bytes: &'a [u8]) -> Self {
        Decoder {
            offset: 0,
            bytes: proof_bytes,
        }
    }

    /// Returns the number of bytes not yet consumed by the decoder.
    pub const fn remaining_bytes(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }
}

impl Iterator for Decoder<'_> {
    type Item = Result<Op, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.bytes.len() {
            return None;
        }

        Some((|| {
            let bytes = &self.bytes[self.offset..];
            let op = Op::decode(bytes)?;
            self.offset += op.encoding_length();
            Ok(op)
        })())
    }
}

#[cfg(test)]
mod test {
    use ed::Encode;

    use super::{Decoder, Node, Op};
    use crate::proofs::{
        TreeFeatureType::{BasicMerkNode, SummedMerkNode},
        HASH_LENGTH,
    };

    #[test]
    fn encode_push_hash() {
        let op = Op::Push(Node::Hash([123; HASH_LENGTH]));
        assert_eq!(op.encoding_length(), 1 + HASH_LENGTH);

        let mut bytes = vec![];
        op.encode_into_with_error(&mut bytes)
            .expect("encode failed");
        assert_eq!(
            bytes,
            vec![
                0x01, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123
            ]
        );
    }

    #[test]
    fn encode_push_kvhash() {
        let op = Op::Push(Node::KVHash([123; HASH_LENGTH]));
        assert_eq!(op.encoding_length(), 1 + HASH_LENGTH);

        let mut bytes = vec![];
        op.encode_into_with_error(&mut bytes)
            .expect("encode failed");
        assert_eq!(
            bytes,
            vec![
                0x02, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123
            ]
        );
    }

    #[test]
    fn encode_push_kvdigest() {
        let op = Op::Push(Node::KVDigest(vec![1, 2, 3], [123; HASH_LENGTH]));
        assert_eq!(op.encoding_length(), 5 + HASH_LENGTH);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x05, 3, 1, 2, 3, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123, 123, 123
            ]
        );
    }

    #[test]
    fn encode_push_kv() {
        let op = Op::Push(Node::KV(vec![1, 2, 3], vec![4, 5, 6]));
        assert_eq!(op.encoding_length(), 10);

        let mut bytes = vec![];
        op.encode_into_with_error(&mut bytes)
            .expect("encode failed");
        assert_eq!(bytes, vec![0x03, 3, 1, 2, 3, 0, 3, 4, 5, 6]);
    }

    #[test]
    fn encode_push_kvvaluehash() {
        let op = Op::Push(Node::KVValueHash(vec![1, 2, 3], vec![4, 5, 6], [0; 32]));
        assert_eq!(op.encoding_length(), 42);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x04, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        )
    }

    #[test]
    fn encode_push_kvvaluerefhash() {
        let op = Op::Push(Node::KVRefValueHash(vec![1, 2, 3], vec![4, 5, 6], [0; 32]));
        assert_eq!(op.encoding_length(), 42);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x06, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        )
    }

    #[test]
    fn encode_push_kvvalue_hash_feature_type() {
        let op = Op::Push(Node::KVValueHashFeatureType(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            BasicMerkNode,
        ));
        assert_eq!(op.encoding_length(), 43);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x07, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );

        let op = Op::Push(Node::KVValueHashFeatureType(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            SummedMerkNode(6),
        ));
        assert_eq!(op.encoding_length(), 44);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x07, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 12
            ]
        )
    }

    #[test]
    fn encode_push_inverted_hash() {
        let op = Op::PushInverted(Node::Hash([123; HASH_LENGTH]));
        assert_eq!(op.encoding_length(), 1 + HASH_LENGTH);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x08, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123
            ]
        );
    }

    #[test]
    fn encode_push_inverted_kvhash() {
        let op = Op::PushInverted(Node::KVHash([123; HASH_LENGTH]));
        assert_eq!(op.encoding_length(), 1 + HASH_LENGTH);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x09, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123
            ]
        );
    }

    #[test]
    fn encode_push_inverted_kvdigest() {
        let op = Op::PushInverted(Node::KVDigest(vec![1, 2, 3], [123; HASH_LENGTH]));
        assert_eq!(op.encoding_length(), 5 + HASH_LENGTH);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x0c, 3, 1, 2, 3, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
                123, 123, 123
            ]
        );
    }

    #[test]
    fn encode_push_inverted_kv() {
        let op = Op::PushInverted(Node::KV(vec![1, 2, 3], vec![4, 5, 6]));
        assert_eq!(op.encoding_length(), 10);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes, vec![0x0a, 3, 1, 2, 3, 0, 3, 4, 5, 6]);
    }

    #[test]
    fn encode_push_inverted_kvvaluehash() {
        let op = Op::PushInverted(Node::KVValueHash(vec![1, 2, 3], vec![4, 5, 6], [0; 32]));
        assert_eq!(op.encoding_length(), 42);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x0b, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        )
    }

    #[test]
    fn encode_push_inverted_kvvalue_hash_feature_type() {
        let op = Op::PushInverted(Node::KVValueHashFeatureType(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            BasicMerkNode,
        ));
        assert_eq!(op.encoding_length(), 43);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x0e, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        );

        let op = Op::PushInverted(Node::KVValueHashFeatureType(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            SummedMerkNode(5),
        ));
        assert_eq!(op.encoding_length(), 44);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x0e, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 10
            ]
        );
    }

    #[test]
    fn encode_push_inverted_kvvaluerefhash() {
        let op = Op::PushInverted(Node::KVRefValueHash(vec![1, 2, 3], vec![4, 5, 6], [0; 32]));
        assert_eq!(op.encoding_length(), 42);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x0d, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ]
        )
    }

    #[test]
    fn encode_parent() {
        let op = Op::Parent;
        assert_eq!(op.encoding_length(), 1);

        let mut bytes = vec![];
        op.encode_into_with_error(&mut bytes)
            .expect("encode failed");
        assert_eq!(bytes, vec![0x10]);
    }

    #[test]
    fn encode_child() {
        let op = Op::Child;
        assert_eq!(op.encoding_length(), 1);

        let mut bytes = vec![];
        op.encode_into_with_error(&mut bytes)
            .expect("encode failed");
        assert_eq!(bytes, vec![0x11]);
    }

    #[test]
    fn encode_parent_inverted() {
        let op = Op::ParentInverted;
        assert_eq!(op.encoding_length(), 1);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes, vec![0x12]);
    }

    #[test]
    fn encode_child_inverted() {
        let op = Op::ChildInverted;
        assert_eq!(op.encoding_length(), 1);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes, vec![0x13]);
    }

    #[test]
    #[should_panic]
    fn encode_push_kv_long_key() {
        let op = Op::Push(Node::KV(vec![123; 300], vec![4, 5, 6]));
        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
    }

    #[test]
    fn decode_push_hash() {
        let bytes = [
            0x01, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
            123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(op, Op::Push(Node::Hash([123; HASH_LENGTH])));
    }

    #[test]
    fn decode_push_kvhash() {
        let bytes = [
            0x02, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
            123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(op, Op::Push(Node::KVHash([123; HASH_LENGTH])));
    }

    #[test]
    fn decode_push_kvdigest() {
        let bytes = [
            0x05, 3, 1, 2, 3, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
            123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
            123,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::Push(Node::KVDigest(vec![1, 2, 3], [123; HASH_LENGTH]))
        );
    }

    #[test]
    fn decode_push_kv() {
        let bytes = [0x03, 3, 1, 2, 3, 0, 3, 4, 5, 6];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(op, Op::Push(Node::KV(vec![1, 2, 3], vec![4, 5, 6])));
    }

    #[test]
    fn decode_push_kvvaluehash() {
        let bytes = [
            0x04, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::Push(Node::KVValueHash(vec![1, 2, 3], vec![4, 5, 6], [0; 32]))
        );
    }

    #[test]
    fn decode_push_kvvaluerefhash() {
        let bytes = [
            0x06, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::Push(Node::KVRefValueHash(vec![1, 2, 3], vec![4, 5, 6], [0; 32]))
        );
    }

    #[test]
    fn decode_push_kvvalue_hash_feature_type() {
        let bytes = [
            0x07, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::Push(Node::KVValueHashFeatureType(
                vec![1, 2, 3],
                vec![4, 5, 6],
                [0; 32],
                BasicMerkNode
            ))
        );

        let bytes = [
            0x07, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 12,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::Push(Node::KVValueHashFeatureType(
                vec![1, 2, 3],
                vec![4, 5, 6],
                [0; 32],
                SummedMerkNode(6)
            ))
        );
    }

    #[test]
    fn decode_push_inverted_hash() {
        let bytes = [
            0x08, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
            123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(op, Op::PushInverted(Node::Hash([123; HASH_LENGTH])));
    }

    #[test]
    fn decode_push_inverted_kvhash() {
        let bytes = [
            0x09, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
            123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(op, Op::PushInverted(Node::KVHash([123; HASH_LENGTH])));
    }

    #[test]
    fn decode_push_inverted_kvdigest() {
        let bytes = [
            0x0c, 3, 1, 2, 3, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
            123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
            123,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::PushInverted(Node::KVDigest(vec![1, 2, 3], [123; HASH_LENGTH]))
        );
    }

    #[test]
    fn decode_push_inverted_kv() {
        let bytes = [0x0a, 3, 1, 2, 3, 0, 3, 4, 5, 6];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(op, Op::PushInverted(Node::KV(vec![1, 2, 3], vec![4, 5, 6])));
    }

    #[test]
    fn decode_push_inverted_kvvaluehash() {
        let bytes = [
            0x0b, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::PushInverted(Node::KVValueHash(vec![1, 2, 3], vec![4, 5, 6], [0; 32]))
        );
    }

    #[test]
    fn decode_push_inverted_kvvaluerefhash() {
        let bytes = [
            0x0d, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::PushInverted(Node::KVRefValueHash(vec![1, 2, 3], vec![4, 5, 6], [0; 32]))
        );
    }

    #[test]
    fn decode_push_inverted_kvvalue_hash_feature_type() {
        let bytes = [
            0x0e, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::PushInverted(Node::KVValueHashFeatureType(
                vec![1, 2, 3],
                vec![4, 5, 6],
                [0; 32],
                BasicMerkNode
            ))
        );

        let bytes = [
            0x0e, 3, 1, 2, 3, 0, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 12,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::PushInverted(Node::KVValueHashFeatureType(
                vec![1, 2, 3],
                vec![4, 5, 6],
                [0; 32],
                SummedMerkNode(6)
            ))
        );
    }

    #[test]
    fn decode_parent() {
        let bytes = [0x10];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(op, Op::Parent);
    }

    #[test]
    fn decode_child() {
        let bytes = [0x11];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(op, Op::Child);
    }

    #[test]
    fn decode_multiple_child() {
        let bytes = [0x11, 0x11, 0x11, 0x10];
        let decoder = Decoder {
            bytes: &bytes,
            offset: 0,
        };

        let mut vecop = vec![];
        for op in decoder {
            match op {
                Ok(op) => vecop.push(op),
                Err(e) => eprintln!("Error decoding: {:?}", e),
            }
        }
        assert_eq!(vecop, vec![Op::Child, Op::Child, Op::Child, Op::Parent]);
    }

    #[test]
    fn decode_parent_inverted() {
        let bytes = [0x12];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(op, Op::ParentInverted);
    }

    #[test]
    fn decode_child_inverted() {
        let bytes = [0x13];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(op, Op::ChildInverted);
    }

    #[test]
    fn decode_unknown() {
        let bytes = [0x88];
        assert!(Op::decode(&bytes[..]).is_err());
    }

    #[test]
    fn encode_decode_push_kvcount() {
        let op = Op::Push(Node::KVCount(vec![1, 2, 3], vec![4, 5, 6], 42));
        // 1 opcode + 1 key_len + key + 2 value_len + value + 8 count
        let expected_length = 4 + 3 + 3 + 8;
        assert_eq!(op.encoding_length(), expected_length);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes.len(), expected_length);
        assert_eq!(bytes[0], 0x14); // Check opcode

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn encode_decode_push_kvhashcount() {
        let op = Op::Push(Node::KVHashCount([123; HASH_LENGTH], 42));
        let expected_length = 1 + HASH_LENGTH + 8; // 1 opcode + 32 hash + 8 count
        assert_eq!(op.encoding_length(), expected_length);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes.len(), expected_length);
        assert_eq!(bytes[0], 0x15); // Check opcode

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn encode_decode_push_inverted_kvcount() {
        let op = Op::PushInverted(Node::KVCount(vec![1, 2, 3], vec![4, 5, 6], 42));
        // 1 opcode + 1 key_len + key + 2 value_len + value + 8 count
        let expected_length = 4 + 3 + 3 + 8;
        assert_eq!(op.encoding_length(), expected_length);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes.len(), expected_length);
        assert_eq!(bytes[0], 0x16); // Check opcode

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn encode_decode_push_inverted_kvhashcount() {
        let op = Op::PushInverted(Node::KVHashCount([123; HASH_LENGTH], 42));
        let expected_length = 1 + HASH_LENGTH + 8; // 1 opcode + 32 hash + 8 count
        assert_eq!(op.encoding_length(), expected_length);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes.len(), expected_length);
        assert_eq!(bytes[0], 0x17); // Check opcode

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn decoder_with_count_nodes() {
        let ops = vec![
            Op::Push(Node::KVCount(vec![1, 2, 3], vec![4, 5, 6], 42)),
            Op::Push(Node::KVHashCount([123; HASH_LENGTH], 100)),
            Op::Child,
            Op::PushInverted(Node::KVCount(vec![7, 8, 9], vec![10, 11, 12], 200)),
            Op::Parent,
        ];

        let mut encoded = vec![];
        for op in &ops {
            op.encode_into(&mut encoded).unwrap();
        }

        let decoder = Decoder::new(&encoded);
        let decoded_ops: Result<Vec<Op>, _> = decoder.collect();
        assert!(decoded_ops.is_ok());
        assert_eq!(decoded_ops.unwrap(), ops);
    }

    #[test]
    fn encode_decode_push_kvrefvaluehash_count() {
        let op = Op::Push(Node::KVRefValueHashCount(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            42,
        ));
        // 1 opcode + 1 key_len + key + 2 value_len + value + 32 hash + 8 count
        let expected_length = 4 + 3 + 3 + HASH_LENGTH + 8;
        assert_eq!(op.encoding_length(), expected_length);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes.len(), expected_length);
        assert_eq!(bytes[0], 0x18); // Check opcode

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn encode_decode_push_inverted_kvrefvaluehash_count() {
        let op = Op::PushInverted(Node::KVRefValueHashCount(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            100,
        ));
        // 1 opcode + 1 key_len + key + 2 value_len + value + 32 hash + 8 count
        let expected_length = 4 + 3 + 3 + HASH_LENGTH + 8;
        assert_eq!(op.encoding_length(), expected_length);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes.len(), expected_length);
        assert_eq!(bytes[0], 0x19); // Check opcode

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn encode_decode_push_kvvalue_hash_feature_type() {
        let op = Op::Push(Node::KVValueHashFeatureType(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            BasicMerkNode,
        ));
        assert_eq!(op.encoding_length(), 43);

        let mut bytes = vec![];
        op.encode_into_with_error(&mut bytes)
            .expect("encode failed");

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);

        let op2 = Op::Push(Node::KVValueHashFeatureType(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            SummedMerkNode(6),
        ));
        let mut bytes2 = vec![];
        op2.encode_into_with_error(&mut bytes2)
            .expect("encode failed");
        let decoded2 = Op::decode(&bytes2[..]).expect("decode failed");
        assert_eq!(decoded2, op2);
    }

    #[test]
    fn decoder_multiple_ops() {
        let ops = vec![
            Op::Push(Node::KVCount(vec![1, 2, 3], vec![4, 5, 6], 42)),
            Op::Push(Node::KVHashCount([123; HASH_LENGTH], 100)),
            Op::Child,
            Op::Parent,
        ];

        let mut encoded = vec![];
        for op in &ops {
            op.encode_into_with_error(&mut encoded)
                .expect("encode failed");
        }

        let decoder = Decoder::new(&encoded);
        let decoded_ops: Result<Vec<Op>, _> = decoder.collect();
        assert_eq!(decoded_ops.expect("decode failed"), ops);
    }

    #[test]
    fn decoder_remaining_bytes_zero_after_full_consumption() {
        let ops = vec![
            Op::Push(Node::Hash([1; HASH_LENGTH])),
            Op::Push(Node::KV(vec![2, 3], vec![4, 5])),
            Op::Parent,
        ];

        let mut encoded = vec![];
        for op in &ops {
            op.encode_into(&mut encoded).unwrap();
        }

        let mut decoder = Decoder::new(&encoded);
        let decoded: Vec<Op> = decoder.by_ref().collect::<Result<_, _>>().unwrap();
        assert_eq!(decoded, ops);
        assert_eq!(decoder.remaining_bytes(), 0);
    }

    #[test]
    fn decoder_remaining_bytes_detects_trailing_data() {
        let op = Op::Push(Node::Hash([1; HASH_LENGTH]));

        let mut encoded = vec![];
        op.encode_into(&mut encoded).unwrap();
        // Append trailing garbage bytes
        encoded.extend_from_slice(&[0xFF, 0xFE, 0xFD]);

        let mut decoder = Decoder::new(&encoded);
        // First op decodes fine
        let first = decoder.next().unwrap().unwrap();
        assert_eq!(first, op);
        // Remaining bytes include the trailing garbage (and possibly the next
        // attempted decode will fail, but remaining_bytes is nonzero either way)
        assert!(decoder.remaining_bytes() > 0);
    }

    #[test]
    fn encode_push_kvvalue_hash_feature_type_with_child_hash() {
        let op = Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            BasicMerkNode,
            [42; 32],
        ));
        // header(4) + key(3) + value(3) + value_hash(32) + feature_type(1) + child_hash(32) = 75
        assert_eq!(op.encoding_length(), 75);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x1c, 3, 1, 2, 3, 0, 3, 4, 5, 6, // value_hash: 32 zero bytes
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, // feature_type: BasicMerkNode
                0, // child_hash: 32 bytes of 42
                42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
                42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42
            ]
        );

        let op = Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            SummedMerkNode(6),
            [42; 32],
        ));
        // header(4) + key(3) + value(3) + value_hash(32) + feature_type(2) + child_hash(32) = 76
        assert_eq!(op.encoding_length(), 76);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x1c, 3, 1, 2, 3, 0, 3, 4, 5, 6, // value_hash: 32 zero bytes
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, // feature_type: SummedMerkNode(6)
                1, 12, // child_hash: 32 bytes of 42
                42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
                42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42
            ]
        );
    }

    #[test]
    fn encode_push_inverted_kvvalue_hash_feature_type_with_child_hash() {
        let op = Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            BasicMerkNode,
            [42; 32],
        ));
        assert_eq!(op.encoding_length(), 75);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x1d, 3, 1, 2, 3, 0, 3, 4, 5, 6, // value_hash: 32 zero bytes
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, // feature_type: BasicMerkNode
                0, // child_hash: 32 bytes of 42
                42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
                42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42
            ]
        );

        let op = Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            SummedMerkNode(6),
            [42; 32],
        ));
        assert_eq!(op.encoding_length(), 76);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x1d, 3, 1, 2, 3, 0, 3, 4, 5, 6, // value_hash: 32 zero bytes
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, // feature_type: SummedMerkNode(6)
                1, 12, // child_hash: 32 bytes of 42
                42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
                42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42
            ]
        );
    }

    #[test]
    fn decode_push_kvvalue_hash_feature_type_with_child_hash() {
        let bytes = [
            0x1c, 3, 1, 2, 3, 0, 3, 4, 5, 6, // value_hash: 32 zero bytes
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, // feature_type: BasicMerkNode
            0, // child_hash: 32 bytes of 42
            42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
            42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
                vec![1, 2, 3],
                vec![4, 5, 6],
                [0; 32],
                BasicMerkNode,
                [42; 32]
            ))
        );

        let bytes = [
            0x1c, 3, 1, 2, 3, 0, 3, 4, 5, 6, // value_hash: 32 zero bytes
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, // feature_type: SummedMerkNode(6)
            1, 12, // child_hash: 32 bytes of 42
            42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
            42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
                vec![1, 2, 3],
                vec![4, 5, 6],
                [0; 32],
                SummedMerkNode(6),
                [42; 32]
            ))
        );
    }

    #[test]
    fn decode_push_inverted_kvvalue_hash_feature_type_with_child_hash() {
        let bytes = [
            0x1d, 3, 1, 2, 3, 0, 3, 4, 5, 6, // value_hash: 32 zero bytes
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, // feature_type: BasicMerkNode
            0, // child_hash: 32 bytes of 42
            42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
            42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
                vec![1, 2, 3],
                vec![4, 5, 6],
                [0; 32],
                BasicMerkNode,
                [42; 32]
            ))
        );

        let bytes = [
            0x1d, 3, 1, 2, 3, 0, 3, 4, 5, 6, // value_hash: 32 zero bytes
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, // feature_type: SummedMerkNode(6)
            1, 12, // child_hash: 32 bytes of 42
            42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
            42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
        ];
        let op = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(
            op,
            Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
                vec![1, 2, 3],
                vec![4, 5, 6],
                [0; 32],
                SummedMerkNode(6),
                [42; 32]
            ))
        );
    }

    #[test]
    fn encode_decode_roundtrip_kvvalue_hash_feature_type_with_child_hash() {
        // Push with BasicMerkNode
        let op = Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            BasicMerkNode,
            [42; 32],
        ));
        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);

        // Push with SummedMerkNode
        let op = Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            SummedMerkNode(6),
            [42; 32],
        ));
        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);

        // PushInverted with BasicMerkNode
        let op = Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            BasicMerkNode,
            [42; 32],
        ));
        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);

        // PushInverted with SummedMerkNode
        let op = Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            vec![4, 5, 6],
            [0; 32],
            SummedMerkNode(6),
            [42; 32],
        ));
        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn encode_push_kvvalue_hash_feature_type_with_child_hash_large_value() {
        let large_value = vec![0xAB; 70000];
        let op = Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            large_value,
            [0; 32],
            BasicMerkNode,
            [42; 32],
        ));
        // 1 opcode + 1 key_len + 3 key + 4 value_len_u32 + 70000 value
        // + 32 value_hash + 1 feature_type + 32 child_hash = 70074
        assert_eq!(op.encoding_length(), 70074);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes[0], 0x2e); // large-value Push opcode
    }

    #[test]
    fn encode_push_inverted_kvvalue_hash_feature_type_with_child_hash_large_value() {
        let large_value = vec![0xAB; 70000];
        let op = Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            large_value,
            [0; 32],
            BasicMerkNode,
            [42; 32],
        ));
        // 1 opcode + 1 key_len + 3 key + 4 value_len_u32 + 70000 value
        // + 32 value_hash + 1 feature_type + 32 child_hash = 70074
        assert_eq!(op.encoding_length(), 70074);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes[0], 0x2f); // large-value PushInverted opcode
    }

    #[test]
    fn decode_push_kvvalue_hash_feature_type_with_child_hash_large_value() {
        let large_value = vec![0xAB; 70000];
        let op = Op::Push(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            large_value,
            [0; 32],
            BasicMerkNode,
            [42; 32],
        ));

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn decode_push_inverted_kvvalue_hash_feature_type_with_child_hash_large_value() {
        let large_value = vec![0xAB; 70000];
        let op = Op::PushInverted(Node::KVValueHashFeatureTypeWithChildHash(
            vec![1, 2, 3],
            large_value,
            [0; 32],
            BasicMerkNode,
            [42; 32],
        ));

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn encode_decode_push_hash_with_count() {
        // (kv_hash, left_child_hash, right_child_hash, count) — the
        // self-verifying compressed-subtree variant for AggregateCountOnRange.
        let op = Op::Push(Node::HashWithCount(
            [0xAA; HASH_LENGTH],
            [0xBB; HASH_LENGTH],
            [0xCC; HASH_LENGTH],
            42,
        ));
        // 1 opcode + 3 * 32 hashes + varint(42) = 1 + 96 + 1 = 98
        let expected_length = 1 + 3 * HASH_LENGTH + ed::Encode::encoding_length(&42u64).unwrap();
        assert_eq!(op.encoding_length(), expected_length);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes.len(), expected_length);
        assert_eq!(bytes[0], 0x1e); // Push HashWithCount opcode

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn encode_decode_push_inverted_hash_with_count() {
        let op = Op::PushInverted(Node::HashWithCount(
            [0x11; HASH_LENGTH],
            [0x22; HASH_LENGTH],
            [0x33; HASH_LENGTH],
            u64::MAX,
        ));
        let expected_length = 1 + 3 * HASH_LENGTH + ed::Encode::encoding_length(&u64::MAX).unwrap();
        assert_eq!(op.encoding_length(), expected_length);

        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes.len(), expected_length);
        assert_eq!(bytes[0], 0x1f); // PushInverted HashWithCount opcode

        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn encode_decode_hash_with_count_zero_count_zero_children() {
        // count = 0 (encodes to a 1-byte varint), all-zero hashes — represents
        // a leaf-shaped collapsed subtree with no children.
        let op = Op::Push(Node::HashWithCount(
            [0u8; HASH_LENGTH],
            [0u8; HASH_LENGTH],
            [0u8; HASH_LENGTH],
            0,
        ));
        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes[0], 0x1e);
        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn decoder_with_hash_with_count_mixed_with_other_count_nodes() {
        // Round-trip a small Op stream containing HashWithCount alongside the
        // existing count-bearing variants — exercises the Decoder iterator
        // boundary handling for the new variants.
        let ops = vec![
            Op::Push(Node::HashWithCount(
                [1; HASH_LENGTH],
                [2; HASH_LENGTH],
                [3; HASH_LENGTH],
                7,
            )),
            Op::Push(Node::KVDigestCount(vec![0xAB], [4; HASH_LENGTH], 1)),
            Op::Parent,
            Op::Push(Node::Hash([5; HASH_LENGTH])),
            Op::Child,
            Op::PushInverted(Node::HashWithCount(
                [6; HASH_LENGTH],
                [7; HASH_LENGTH],
                [8; HASH_LENGTH],
                12345,
            )),
        ];

        let mut encoded = vec![];
        for op in &ops {
            op.encode_into(&mut encoded).unwrap();
        }

        let decoder = Decoder::new(&encoded);
        let decoded_ops: Result<Vec<Op>, _> = decoder.collect();
        assert_eq!(decoded_ops.unwrap(), ops);
    }

    // ProvableSumTree proof-node round-trip tests. Each variant must
    // round-trip through both `Op::Push` and `Op::PushInverted`, and through
    // the full numeric range of i64 sums (incl. negatives and boundaries).
    fn round_trip_op(op: Op) {
        let mut encoded = vec![];
        op.encode_into(&mut encoded).unwrap();
        // encoding_length must match the actual encoded byte length.
        assert_eq!(encoded.len(), op.encoding_length());
        let mut decoder = Decoder::new(&encoded);
        let decoded = decoder.next().unwrap().unwrap();
        assert_eq!(decoded, op);
        assert_eq!(decoder.remaining_bytes(), 0);
    }

    fn round_trip_sum_variants_with(sum: i64) {
        // Push family
        round_trip_op(Op::Push(Node::KVSum(vec![1, 2, 3], vec![4, 5], sum)));
        round_trip_op(Op::Push(Node::KVHashSum([0xAB; HASH_LENGTH], sum)));
        round_trip_op(Op::Push(Node::KVRefValueHashSum(
            vec![9, 8],
            vec![7, 6, 5],
            [0xCD; HASH_LENGTH],
            sum,
        )));
        round_trip_op(Op::Push(Node::KVDigestSum(
            vec![10, 11],
            [0xEF; HASH_LENGTH],
            sum,
        )));
        round_trip_op(Op::Push(Node::HashWithSum(
            [1; HASH_LENGTH],
            [2; HASH_LENGTH],
            [3; HASH_LENGTH],
            sum,
        )));
        // PushInverted family
        round_trip_op(Op::PushInverted(Node::KVSum(
            vec![1, 2, 3],
            vec![4, 5],
            sum,
        )));
        round_trip_op(Op::PushInverted(Node::KVHashSum([0xAB; HASH_LENGTH], sum)));
        round_trip_op(Op::PushInverted(Node::KVRefValueHashSum(
            vec![9, 8],
            vec![7, 6, 5],
            [0xCD; HASH_LENGTH],
            sum,
        )));
        round_trip_op(Op::PushInverted(Node::KVDigestSum(
            vec![10, 11],
            [0xEF; HASH_LENGTH],
            sum,
        )));
        round_trip_op(Op::PushInverted(Node::HashWithSum(
            [1; HASH_LENGTH],
            [2; HASH_LENGTH],
            [3; HASH_LENGTH],
            sum,
        )));
    }

    #[test]
    fn sum_node_variants_round_trip_at_zero() {
        round_trip_sum_variants_with(0);
    }

    #[test]
    fn sum_node_variants_round_trip_at_positive() {
        round_trip_sum_variants_with(1);
        round_trip_sum_variants_with(42);
        round_trip_sum_variants_with(i64::MAX);
    }

    #[test]
    fn sum_node_variants_round_trip_at_negative() {
        round_trip_sum_variants_with(-1);
        round_trip_sum_variants_with(-42);
        round_trip_sum_variants_with(i64::MIN);
    }

    #[test]
    fn sum_node_variants_use_new_tag_bytes() {
        // Sanity check: each new variant writes its expected tag byte as the
        // first byte of the encoded form. This guards against tag drift if
        // someone refactors the encoder.
        let cases: &[(Op, u8)] = &[
            (Op::Push(Node::KVSum(vec![1], vec![2], 5)), 0x30),
            (Op::Push(Node::KVHashSum([0; HASH_LENGTH], 5)), 0x32),
            (
                Op::Push(Node::KVRefValueHashSum(
                    vec![1],
                    vec![2],
                    [0; HASH_LENGTH],
                    5,
                )),
                0x33,
            ),
            (
                Op::Push(Node::KVDigestSum(vec![1], [0; HASH_LENGTH], 5)),
                0x35,
            ),
            (
                Op::Push(Node::HashWithSum(
                    [0; HASH_LENGTH],
                    [0; HASH_LENGTH],
                    [0; HASH_LENGTH],
                    5,
                )),
                0x36,
            ),
            (Op::PushInverted(Node::KVSum(vec![1], vec![2], 5)), 0x37),
            (Op::PushInverted(Node::KVHashSum([0; HASH_LENGTH], 5)), 0x39),
            (
                Op::PushInverted(Node::KVRefValueHashSum(
                    vec![1],
                    vec![2],
                    [0; HASH_LENGTH],
                    5,
                )),
                0x3a,
            ),
            (
                Op::PushInverted(Node::KVDigestSum(vec![1], [0; HASH_LENGTH], 5)),
                0x3c,
            ),
            (
                Op::PushInverted(Node::HashWithSum(
                    [0; HASH_LENGTH],
                    [0; HASH_LENGTH],
                    [0; HASH_LENGTH],
                    5,
                )),
                0x3d,
            ),
        ];
        for (op, expected_tag) in cases {
            let mut bytes = vec![];
            op.encode_into(&mut bytes).unwrap();
            assert_eq!(bytes[0], *expected_tag, "wrong tag byte for {:?}", op);
        }
    }

    // Large-value (>= 65536 bytes) round-trip tests for ProvableSumTree
    // proof-node variants. Each KV-style variant has a "small value" (u16 length)
    // and a "large value" (u32 length) encoding path. The small-value path is
    // exercised by `sum_node_variants_round_trip_at_*` above; here we cover
    // the large-value path for the four KV variants that carry a value field
    // (`KVSum`, `KVRefValueHashSum` in both Push and PushInverted directions).

    /// Helper: encode → decode → assert byte-for-byte and structural equality.
    fn large_value_round_trip(op: Op, expected_tag: u8) {
        let mut bytes = vec![];
        op.encode_into(&mut bytes).unwrap();
        assert_eq!(bytes[0], expected_tag, "wrong tag byte for {:?}", op);
        assert_eq!(bytes.len(), op.encoding_length());
        let decoded = Op::decode(&bytes[..]).expect("decode failed");
        assert_eq!(decoded, op);
    }

    #[test]
    fn kvsum_push_large_value_round_trip() {
        // 0x31 = Push KVSum with u32 value length (value.len() >= 65536).
        let large_value = vec![0xAB; 70_000];
        let op = Op::Push(Node::KVSum(vec![1, 2, 3], large_value, 42));
        large_value_round_trip(op, 0x31);
    }

    #[test]
    fn kvsum_pushinverted_large_value_round_trip() {
        // 0x38 = PushInverted KVSum with u32 value length.
        let large_value = vec![0xCD; 70_000];
        let op = Op::PushInverted(Node::KVSum(vec![9, 8, 7], large_value, -99));
        large_value_round_trip(op, 0x38);
    }

    #[test]
    fn kvrefvaluehashsum_push_large_value_round_trip() {
        // 0x34 = Push KVRefValueHashSum with u32 value length.
        let large_value = vec![0xEF; 70_000];
        let op = Op::Push(Node::KVRefValueHashSum(
            vec![1, 2, 3],
            large_value,
            [0x55; HASH_LENGTH],
            i64::MAX,
        ));
        large_value_round_trip(op, 0x34);
    }

    #[test]
    fn kvrefvaluehashsum_pushinverted_large_value_round_trip() {
        // 0x3b = PushInverted KVRefValueHashSum with u32 value length.
        let large_value = vec![0x12; 70_000];
        let op = Op::PushInverted(Node::KVRefValueHashSum(
            vec![4, 5, 6],
            large_value,
            [0x77; HASH_LENGTH],
            i64::MIN,
        ));
        large_value_round_trip(op, 0x3b);
    }
}
