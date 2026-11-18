//! Proof primitives: Op, Node, encoding, and TreeFeatureType.
//!
//! These types define the proof operators and node variants used to construct
//! and verify Merkle proofs in GroveDB/Merk.

mod encoding;

mod tree_feature_type;

pub use encoding::{encode_into, Decoder};
pub use tree_feature_type::{NodeType, TreeFeatureType};

use crate::hex_to_ascii;

/// The length of a `Hash` (in bytes).
pub const HASH_LENGTH: usize = 32;

/// A zero-filled `Hash`.
pub const NULL_HASH: CryptoHash = [0; HASH_LENGTH];

/// A cryptographic hash digest.
pub type CryptoHash = [u8; HASH_LENGTH];

/// A proof operator, executed to verify the data in a Merkle proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Pushes a node on the stack.
    /// Signifies ascending node keys
    Push(Node),

    /// Pushes a node on the stack
    /// Signifies descending node keys
    PushInverted(Node),

    /// Pops the top stack item as `parent`. Pops the next top stack item as
    /// `child`. Attaches `child` as the left child of `parent`. Pushes the
    /// updated `parent` back on the stack.
    Parent,

    /// Pops the top stack item as `child`. Pops the next top stack item as
    /// `parent`. Attaches `child` as the right child of `parent`. Pushes the
    /// updated `parent` back on the stack.
    Child,

    /// Pops the top stack item as `parent`. Pops the next top stack item as
    /// `child`. Attaches `child` as the right child of `parent`. Pushes the
    /// updated `parent` back on the stack.
    ParentInverted,

    /// Pops the top stack item as `child`. Pops the next top stack item as
    /// `parent`. Attaches `child` as the left child of `parent`. Pushes the
    /// updated `parent` back on the stack.
    ChildInverted,
}

/// A selected piece of data about a single tree node, to be contained in a
/// `Push` operator in a proof.
///
/// Each variant carries different amounts of information, allowing proofs to
/// include only what's necessary for verification while minimizing proof size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// The node hash only. Used for sibling/cousin nodes not on the query path.
    ///
    /// Contains: `node_hash`
    Hash(CryptoHash),

    /// The key-value hash only. Used for non-queried nodes on the path.
    ///
    /// Contains: `kv_hash` (hash of key concatenated with value_hash)
    KVHash(CryptoHash),

    /// Key and value_hash. Used for proving key existence/absence at
    /// boundaries.
    ///
    /// Contains: `(key, value_hash)`
    KVDigest(Vec<u8>, CryptoHash),

    /// Key and value. Suitable for items where value_hash = H(value).
    ///
    /// Contains: `(key, value)`
    KV(Vec<u8>, Vec<u8>),

    /// Key, value, and value_hash. Standard node for queried items.
    ///
    /// Contains: `(key, value, value_hash)`
    KVValueHash(Vec<u8>, Vec<u8>, CryptoHash),

    /// Key, value, value_hash, and feature type. For trees with special
    /// features.
    ///
    /// Contains: `(key, value, value_hash, feature_type)`
    KVValueHashFeatureType(Vec<u8>, Vec<u8>, CryptoHash, TreeFeatureType),

    /// Key, referenced value, and hash of serialized Reference element.
    /// For GroveDB references.
    ///
    /// Contains: `(key, referenced_value, reference_element_hash)`
    KVRefValueHash(Vec<u8>, Vec<u8>, CryptoHash),

    /// Key, the element's STRIPPED serialization (backward-references list
    /// emptied), and the 32-byte hash of the serialized backward-references
    /// list. For GroveDB's backward-references-capable elements
    /// (`ItemWithBackwardsReferences` / `SumItemWithBackwardsReferences` /
    /// `ItemWithSumItemWithBackwardsReferences`), whose node value hash is
    /// `combine_hash(H(stripped_value), backrefs_hash)`.
    ///
    /// The verifier RECOMPUTES that combination, so the payload bytes are
    /// bound by the proof (unlike `KVValueHash`, whose value bytes are
    /// carried on trust) while the referrer set itself stays out of the
    /// proof — only its hash travels.
    ///
    /// Contains: `(key, stripped_value, backward_references_hash)`
    KVBackwardsReferencesValueHash(Vec<u8>, Vec<u8>, CryptoHash),

    /// Key, value, and count. For queried Items in ProvableCountTree.
    ///
    /// Contains: `(key, value, count)`
    KVCount(Vec<u8>, Vec<u8>, u64),

    /// KV hash and count. For non-queried nodes in ProvableCountTree.
    ///
    /// Contains: `(kv_hash, count)`
    KVHashCount(CryptoHash, u64),

    /// Key, referenced value, reference element hash, and count.
    /// For queried References in ProvableCountTree.
    ///
    /// Contains: `(key, referenced_value, reference_element_hash, count)`
    KVRefValueHashCount(Vec<u8>, Vec<u8>, CryptoHash, u64),

    /// Key, value_hash, and count. For proving absence in ProvableCountTree.
    ///
    /// Contains: `(key, value_hash, count)`
    KVDigestCount(Vec<u8>, CryptoHash, u64),

    /// Key, value, value_hash, feature type, and child tree root hash.
    /// Used for non-empty tree elements in the result set that have no
    /// subquery (no lower-layer proof). The child_hash allows the verifier
    /// to check `combine_hash(H(value), child_hash) == value_hash` without
    /// needing a full lower-layer proof.
    ///
    /// Contains: `(key, value, value_hash, feature_type, child_hash)`
    KVValueHashFeatureTypeWithChildHash(Vec<u8>, Vec<u8>, CryptoHash, TreeFeatureType, CryptoHash),

    /// A self-verifying compressed subtree for `AggregateCountOnRange` proofs
    /// against a `ProvableCountTree` / `ProvableCountSumTree`.
    ///
    /// Encodes the subtree's *root* node as `(kv_hash, left_child_hash,
    /// right_child_hash, count)`. The verifier reconstructs the subtree's
    /// root `node_hash` as
    /// `node_hash_with_count(kv_hash, left_child_hash, right_child_hash, count)`
    /// and uses that hash exactly as `Hash(...)` would. Because `count` is
    /// part of that recomputation, a forged count produces a different hash
    /// and the parent's Merkle-root check fails — the count is therefore
    /// cryptographically committed by the parent's hash chain, not just
    /// trusted on faith.
    ///
    /// Used to collapse an entire fully-inside subtree into a single proof
    /// node: the verifier doesn't need any per-key information (the parent
    /// boundary nodes already established that every key under here is
    /// in-range), so we hand it the four hashes plus the count.
    ///
    /// `left_child_hash` / `right_child_hash` are the all-zero `NULL_HASH`
    /// when the subtree's root has no left / right child respectively.
    ///
    /// Contains: `(kv_hash, left_child_hash, right_child_hash, count)`
    HashWithCount(CryptoHash, CryptoHash, CryptoHash, u64),

    /// Key, value, and sum. For queried Items in ProvableSumTree.
    ///
    /// Sum analogue of `KVCount`: the verifier recomputes
    /// `node_hash = node_hash_with_sum(kv_hash, left, right, sum)` so a
    /// forged sum produces a hash divergence at the parent boundary.
    ///
    /// Contains: `(key, value, sum)`
    KVSum(Vec<u8>, Vec<u8>, i64),

    /// KV hash and sum. For non-queried nodes in ProvableSumTree.
    ///
    /// Sum analogue of `KVHashCount`.
    ///
    /// Contains: `(kv_hash, sum)`
    KVHashSum(CryptoHash, i64),

    /// Key, referenced value, reference element hash, and sum.
    /// For queried References in ProvableSumTree.
    ///
    /// Sum analogue of `KVRefValueHashCount`.
    ///
    /// Contains: `(key, referenced_value, reference_element_hash, sum)`
    KVRefValueHashSum(Vec<u8>, Vec<u8>, CryptoHash, i64),

    /// Key, value_hash, and sum. For proving absence in ProvableSumTree.
    ///
    /// Sum analogue of `KVDigestCount`.
    ///
    /// Contains: `(key, value_hash, sum)`
    KVDigestSum(Vec<u8>, CryptoHash, i64),

    /// A self-verifying compressed subtree for `AggregateSumOnRange` proofs
    /// against a `ProvableSumTree`.
    ///
    /// Sum analogue of `HashWithCount` — encodes the subtree's *root* node as
    /// `(kv_hash, left_child_hash, right_child_hash, sum)`. The verifier
    /// reconstructs the subtree's root `node_hash` as
    /// `node_hash_with_sum(kv_hash, left_child_hash, right_child_hash, sum)`
    /// and uses that hash exactly as `Hash(...)` would. The sum is
    /// cryptographically committed by the parent's hash chain.
    ///
    /// Contains: `(kv_hash, left_child_hash, right_child_hash, sum)`
    HashWithSum(CryptoHash, CryptoHash, CryptoHash, i64),

    /// Key, value, count, and sum. For queried Items in
    /// `ProvableCountProvableSumTree`.
    ///
    /// Combined analogue of `KVCount` and `KVSum`: the verifier recomputes
    /// `node_hash = node_hash_with_count_and_sum(kv_hash, left, right, count, sum)`
    /// so a forged count OR forged sum produces a hash divergence at the
    /// parent boundary.
    ///
    /// Contains: `(key, value, count, sum)`
    KVCountSum(Vec<u8>, Vec<u8>, u64, i64),

    /// KV hash, count, and sum. For non-queried nodes in
    /// `ProvableCountProvableSumTree`.
    ///
    /// Combined analogue of `KVHashCount` and `KVHashSum`.
    ///
    /// Contains: `(kv_hash, count, sum)`
    KVHashCountSum(CryptoHash, u64, i64),

    /// Key, referenced value, reference element hash, count, and sum.
    /// For queried References in `ProvableCountProvableSumTree`.
    ///
    /// Combined analogue of `KVRefValueHashCount` and `KVRefValueHashSum`.
    ///
    /// Contains: `(key, referenced_value, reference_element_hash, count, sum)`
    KVRefValueHashCountSum(Vec<u8>, Vec<u8>, CryptoHash, u64, i64),

    /// Key, value_hash, count, and sum. For proving absence in
    /// `ProvableCountProvableSumTree`.
    ///
    /// Combined analogue of `KVDigestCount` and `KVDigestSum`.
    ///
    /// Contains: `(key, value_hash, count, sum)`
    KVDigestCountSum(Vec<u8>, CryptoHash, u64, i64),

    /// A self-verifying compressed subtree for both `AggregateCountOnRange`
    /// AND `AggregateSumOnRange` proofs against a
    /// `ProvableCountProvableSumTree`.
    ///
    /// Combined analogue of `HashWithCount` and `HashWithSum` — encodes
    /// the subtree's *root* node as
    /// `(kv_hash, left_child_hash, right_child_hash, count, sum)`. The
    /// verifier reconstructs the subtree's root `node_hash` as
    /// `node_hash_with_count_and_sum(kv_hash, left, right, count, sum)`
    /// and uses that hash exactly as `Hash(...)` would. Both the count and
    /// the sum are cryptographically committed by the parent's hash chain.
    ///
    /// Contains: `(kv_hash, left_child_hash, right_child_hash, count, sum)`
    HashWithCountAndSum(CryptoHash, CryptoHash, CryptoHash, u64, i64),

    /// Key, value, count, and big sum. For queried Items in
    /// `ProvableCountProvableBigSumTree`.
    ///
    /// The wide counterpart of `KVCountSum`: the verifier recomputes
    /// `node_hash = node_hash_with_count_and_big_sum(kv_hash, left, right,
    /// count, sum)`, so a forged count OR forged sum produces a hash
    /// divergence at the parent boundary.
    ///
    /// Contains: `(key, value, count, sum)`
    KVCountBigSum(Vec<u8>, Vec<u8>, u64, i128),

    /// KV hash, count, and big sum. For non-queried nodes in
    /// `ProvableCountProvableBigSumTree`.
    ///
    /// The wide counterpart of `KVHashCountSum`.
    ///
    /// Contains: `(kv_hash, count, sum)`
    KVHashCountBigSum(CryptoHash, u64, i128),

    /// Key, referenced value, reference element hash, count, and big sum.
    /// For queried References in `ProvableCountProvableBigSumTree`.
    ///
    /// The wide counterpart of `KVRefValueHashCountSum`.
    ///
    /// Contains: `(key, referenced_value, reference_element_hash, count, sum)`
    KVRefValueHashCountBigSum(Vec<u8>, Vec<u8>, CryptoHash, u64, i128),

    /// Key, value_hash, count, and big sum. For proving absence in
    /// `ProvableCountProvableBigSumTree`.
    ///
    /// The wide counterpart of `KVDigestCountSum`.
    ///
    /// Contains: `(key, value_hash, count, sum)`
    KVDigestCountBigSum(Vec<u8>, CryptoHash, u64, i128),

    /// A self-verifying compressed subtree for aggregate range proofs
    /// against a `ProvableCountProvableBigSumTree`.
    ///
    /// The wide counterpart of `HashWithCountAndSum`: encodes the
    /// subtree's *root* node as
    /// `(kv_hash, left_child_hash, right_child_hash, count, sum)`. The
    /// verifier reconstructs the subtree's root `node_hash` as
    /// `node_hash_with_count_and_big_sum(kv_hash, left, right, count, sum)`
    /// and uses that hash exactly as `Hash(...)` would.
    ///
    /// Contains: `(kv_hash, left_child_hash, right_child_hash, count, sum)`
    HashWithCountAndBigSum(CryptoHash, CryptoHash, CryptoHash, u64, i128),
}

impl Node {
    /// The key and the value of a node that reveals both: a row. A node
    /// that reveals a key without a value (a `KVDigest*` range bound) or no
    /// key at all (a hash) is `None`.
    ///
    /// Every variant is listed in one of the two accessors' arms, so a new
    /// one has to be classified here, in one place.
    pub fn key_and_value(&self) -> Option<(&[u8], &[u8])> {
        match self {
            Node::KV(key, value)
            | Node::KVValueHash(key, value, ..)
            | Node::KVBackwardsReferencesValueHash(key, value, ..)
            | Node::KVValueHashFeatureType(key, value, ..)
            | Node::KVValueHashFeatureTypeWithChildHash(key, value, ..)
            | Node::KVCount(key, value, ..)
            | Node::KVRefValueHash(key, value, ..)
            | Node::KVRefValueHashCount(key, value, ..)
            | Node::KVSum(key, value, ..)
            | Node::KVRefValueHashSum(key, value, ..)
            | Node::KVCountBigSum(key, value, ..)
            | Node::KVCountSum(key, value, ..)
            | Node::KVRefValueHashCountBigSum(key, value, ..)
            | Node::KVRefValueHashCountSum(key, value, ..) => {
                Some((key.as_slice(), value.as_slice()))
            }
            Node::KVDigest(..)
            | Node::KVDigestCount(..)
            | Node::KVDigestSum(..)
            | Node::KVDigestCountBigSum(..)
            | Node::KVDigestCountSum(..)
            | Node::Hash(_)
            | Node::KVHash(_)
            | Node::KVHashCount(..)
            | Node::HashWithCount(..)
            | Node::KVHashSum(..)
            | Node::HashWithSum(..)
            | Node::KVHashCountBigSum(..)
            | Node::KVHashCountSum(..)
            | Node::HashWithCountAndBigSum(..)
            | Node::HashWithCountAndSum(..) => None,
        }
    }

    /// The key of a node that reveals one, with a value (a row) or without
    /// (a `KVDigest*` range bound). A node that reveals only hashes is
    /// `None`.
    pub fn key(&self) -> Option<&[u8]> {
        match self {
            Node::KVDigest(key, ..)
            | Node::KVDigestCount(key, ..)
            | Node::KVDigestSum(key, ..)
            | Node::KVDigestCountBigSum(key, ..)
            | Node::KVDigestCountSum(key, ..) => Some(key.as_slice()),
            Node::KV(..)
            | Node::KVValueHash(..)
            | Node::KVBackwardsReferencesValueHash(..)
            | Node::KVValueHashFeatureType(..)
            | Node::KVValueHashFeatureTypeWithChildHash(..)
            | Node::KVCount(..)
            | Node::KVRefValueHash(..)
            | Node::KVRefValueHashCount(..)
            | Node::KVSum(..)
            | Node::KVRefValueHashSum(..)
            | Node::KVCountBigSum(..)
            | Node::KVCountSum(..)
            | Node::KVRefValueHashCountBigSum(..)
            | Node::KVRefValueHashCountSum(..)
            | Node::Hash(_)
            | Node::KVHash(_)
            | Node::KVHashCount(..)
            | Node::HashWithCount(..)
            | Node::KVHashSum(..)
            | Node::HashWithSum(..)
            | Node::KVHashCountBigSum(..)
            | Node::KVHashCountSum(..)
            | Node::HashWithCountAndBigSum(..)
            | Node::HashWithCountAndSum(..) => self.key_and_value().map(|(key, _)| key),
        }
    }
}

use std::fmt;

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let node_string = match self {
            Node::Hash(hash) => format!("Hash(HASH[{}])", hex::encode(hash)),
            Node::KVHash(kv_hash) => format!("KVHash(HASH[{}])", hex::encode(kv_hash)),
            Node::KV(key, value) => {
                format!("KV({}, {})", hex_to_ascii(key), hex_to_ascii(value))
            }
            Node::KVValueHash(key, value, value_hash) => format!(
                "KVValueHash({}, {}, HASH[{}])",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash)
            ),
            Node::KVBackwardsReferencesValueHash(key, value, backrefs_hash) => format!(
                "KVBackwardsReferencesValueHash({}, {}, HASH[{}])",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(backrefs_hash)
            ),
            Node::KVDigest(key, value_hash) => format!(
                "KVDigest({}, HASH[{}])",
                hex_to_ascii(key),
                hex::encode(value_hash)
            ),
            Node::KVRefValueHash(key, value, value_hash) => format!(
                "KVRefValueHash({}, {}, HASH[{}])",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash)
            ),
            Node::KVValueHashFeatureType(key, value, value_hash, feature_type) => format!(
                "KVValueHashFeatureType({}, {}, HASH[{}], {:?})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash),
                feature_type
            ),
            Node::KVCount(key, value, count) => format!(
                "KVCount({}, {}, {})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                count
            ),
            Node::KVHashCount(kv_hash, count) => {
                format!("KVHashCount(HASH[{}], {})", hex::encode(kv_hash), count)
            }
            Node::KVRefValueHashCount(key, value, value_hash, count) => format!(
                "KVRefValueHashCount({}, {}, HASH[{}], {})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash),
                count
            ),
            Node::KVDigestCount(key, value_hash, count) => format!(
                "KVDigestCount({}, HASH[{}], {})",
                hex_to_ascii(key),
                hex::encode(value_hash),
                count
            ),
            Node::HashWithCount(kv_hash, left_child_hash, right_child_hash, count) => format!(
                "HashWithCount(kv_hash=HASH[{}], left=HASH[{}], right=HASH[{}], count={})",
                hex::encode(kv_hash),
                hex::encode(left_child_hash),
                hex::encode(right_child_hash),
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
                hex_to_ascii(value),
                hex::encode(value_hash),
                feature_type,
                hex::encode(child_hash)
            ),
            Node::KVSum(key, value, sum) => format!(
                "KVSum({}, {}, {})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                sum
            ),
            Node::KVHashSum(kv_hash, sum) => {
                format!("KVHashSum(HASH[{}], {})", hex::encode(kv_hash), sum)
            }
            Node::KVRefValueHashSum(key, value, value_hash, sum) => format!(
                "KVRefValueHashSum({}, {}, HASH[{}], {})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash),
                sum
            ),
            Node::KVDigestSum(key, value_hash, sum) => format!(
                "KVDigestSum({}, HASH[{}], {})",
                hex_to_ascii(key),
                hex::encode(value_hash),
                sum
            ),
            Node::HashWithSum(kv_hash, left_child_hash, right_child_hash, sum) => format!(
                "HashWithSum(kv_hash=HASH[{}], left=HASH[{}], right=HASH[{}], sum={})",
                hex::encode(kv_hash),
                hex::encode(left_child_hash),
                hex::encode(right_child_hash),
                sum
            ),
            Node::KVCountSum(key, value, count, sum) => format!(
                "KVCountSum({}, {}, count={}, sum={})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                count,
                sum
            ),
            Node::KVHashCountSum(kv_hash, count, sum) => format!(
                "KVHashCountSum(HASH[{}], count={}, sum={})",
                hex::encode(kv_hash),
                count,
                sum
            ),
            Node::KVRefValueHashCountSum(key, value, value_hash, count, sum) => format!(
                "KVRefValueHashCountSum({}, {}, HASH[{}], count={}, sum={})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash),
                count,
                sum
            ),
            Node::KVDigestCountSum(key, value_hash, count, sum) => format!(
                "KVDigestCountSum({}, HASH[{}], count={}, sum={})",
                hex_to_ascii(key),
                hex::encode(value_hash),
                count,
                sum
            ),
            Node::HashWithCountAndSum(kv_hash, left_child_hash, right_child_hash, count, sum) => {
                format!(
                    "HashWithCountAndSum(kv_hash=HASH[{}], left=HASH[{}], right=HASH[{}], \
                     count={}, sum={})",
                    hex::encode(kv_hash),
                    hex::encode(left_child_hash),
                    hex::encode(right_child_hash),
                    count,
                    sum
                )
            }
            Node::KVCountBigSum(key, value, count, sum) => format!(
                "KVCountBigSum({}, {}, count={}, sum={})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                count,
                sum
            ),
            Node::KVHashCountBigSum(kv_hash, count, sum) => format!(
                "KVHashCountBigSum(HASH[{}], count={}, sum={})",
                hex::encode(kv_hash),
                count,
                sum
            ),
            Node::KVRefValueHashCountBigSum(key, value, value_hash, count, sum) => format!(
                "KVRefValueHashCountBigSum({}, {}, HASH[{}], count={}, sum={})",
                hex_to_ascii(key),
                hex_to_ascii(value),
                hex::encode(value_hash),
                count,
                sum
            ),
            Node::KVDigestCountBigSum(key, value_hash, count, sum) => format!(
                "KVDigestCountBigSum({}, HASH[{}], count={}, sum={})",
                hex_to_ascii(key),
                hex::encode(value_hash),
                count,
                sum
            ),
            Node::HashWithCountAndBigSum(
                kv_hash,
                left_child_hash,
                right_child_hash,
                count,
                sum,
            ) => {
                format!(
                    "HashWithCountAndBigSum(kv_hash=HASH[{}], left=HASH[{}], right=HASH[{}], \
                     count={}, sum={})",
                    hex::encode(kv_hash),
                    hex::encode(left_child_hash),
                    hex::encode(right_child_hash),
                    count,
                    sum
                )
            }
        };
        write!(f, "{}", node_string)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn backwards_references_node_display_and_codec() {
        use crate::proofs::encoding::encode_into;

        let small = Node::KVBackwardsReferencesValueHash(
            b"key".to_vec(),
            b"stripped-value".to_vec(),
            [7; 32],
        );
        let shown = format!("{}", small);
        assert!(
            shown.contains("KVBackwardsReferencesValueHash("),
            "got: {shown}"
        );

        // A value over u16::MAX bytes selects the wide length encoding.
        let large =
            Node::KVBackwardsReferencesValueHash(b"key".to_vec(), vec![0xAB; 70_000], [9; 32]);

        for node in [small, large] {
            for op in [Op::Push(node.clone()), Op::PushInverted(node.clone())] {
                let mut bytes = Vec::new();
                encode_into([op.clone()].iter(), &mut bytes);
                assert_eq!(bytes.len(), ed::Encode::encoding_length(&op).unwrap());
                let decoded: Vec<Op> = Decoder::new(&bytes)
                    .collect::<Result<_, _>>()
                    .expect("decode");
                assert_eq!(decoded, vec![op]);
            }
        }
    }

    use super::*;

    #[test]
    fn display_kv_value_hash_feature_type_with_child_hash() {
        let key = b"mykey".to_vec();
        let value = b"myval".to_vec();
        let value_hash = [0xAB; HASH_LENGTH];
        let child_hash = [0xCD; HASH_LENGTH];
        let feature_type = TreeFeatureType::BasicMerkNode;

        let node = Node::KVValueHashFeatureTypeWithChildHash(
            key,
            value,
            value_hash,
            feature_type,
            child_hash,
        );
        let display = node.to_string();

        assert!(
            display.starts_with("KVValueHashFeatureTypeWithChildHash("),
            "Expected prefix, got: {}",
            display
        );
        assert!(display.contains("mykey"), "Expected key in output");
        assert!(display.contains("myval"), "Expected value in output");
        assert!(
            display.contains(&hex::encode(value_hash)),
            "Expected value_hash in output"
        );
        assert!(
            display.contains("BasicMerkNode"),
            "Expected feature_type in output"
        );
        assert!(
            display.contains(&hex::encode(child_hash)),
            "Expected child_hash in output"
        );
    }

    #[test]
    fn display_kv_value_hash_feature_type_with_child_hash_non_ascii_key() {
        let key = vec![0xFF, 0xFE, 0x01];
        let value = vec![0x00, 0x80];
        let value_hash = [0x11; HASH_LENGTH];
        let child_hash = [0x22; HASH_LENGTH];
        let feature_type = TreeFeatureType::SummedMerkNode(42);

        let node = Node::KVValueHashFeatureTypeWithChildHash(
            key,
            value,
            value_hash,
            feature_type,
            child_hash,
        );
        let display = node.to_string();

        assert!(
            display.starts_with("KVValueHashFeatureTypeWithChildHash("),
            "Expected prefix, got: {}",
            display
        );
        // Non-ASCII keys should be hex-encoded by hex_to_ascii
        assert!(
            display.contains("SummedMerkNode(42)"),
            "Expected feature_type in output, got: {}",
            display
        );
    }

    // Display tests for the ProvableSumTree proof-node variants. Each
    // variant has its own match arm in the Display impl, so we exercise them
    // individually to ensure they don't accidentally fall through to a
    // wildcard that would mask future drift.

    #[test]
    fn display_kv_sum() {
        let node = Node::KVSum(b"k".to_vec(), b"v".to_vec(), -7);
        let display = node.to_string();
        assert!(display.starts_with("KVSum("), "got: {}", display);
        assert!(
            display.contains("-7"),
            "expected sum in output: {}",
            display
        );
    }

    #[test]
    fn display_kv_hash_sum() {
        let node = Node::KVHashSum([0xAB; HASH_LENGTH], 123);
        let display = node.to_string();
        assert!(display.starts_with("KVHashSum("), "got: {}", display);
        assert!(display.contains("123"), "expected sum: {}", display);
        assert!(
            display.contains(&hex::encode([0xAB; HASH_LENGTH])),
            "expected kv_hash hex: {}",
            display
        );
    }

    #[test]
    fn display_kv_ref_value_hash_sum() {
        let node =
            Node::KVRefValueHashSum(b"k".to_vec(), b"v".to_vec(), [0xCD; HASH_LENGTH], i64::MIN);
        let display = node.to_string();
        assert!(
            display.starts_with("KVRefValueHashSum("),
            "got: {}",
            display
        );
        assert!(
            display.contains(&i64::MIN.to_string()),
            "expected i64::MIN: {}",
            display
        );
    }

    #[test]
    fn display_kv_digest_sum() {
        let node = Node::KVDigestSum(b"k".to_vec(), [0xEF; HASH_LENGTH], i64::MAX);
        let display = node.to_string();
        assert!(display.starts_with("KVDigestSum("), "got: {}", display);
        assert!(
            display.contains(&i64::MAX.to_string()),
            "expected i64::MAX: {}",
            display
        );
    }

    #[test]
    fn display_hash_with_sum() {
        let node = Node::HashWithSum(
            [0x11; HASH_LENGTH],
            [0x22; HASH_LENGTH],
            [0x33; HASH_LENGTH],
            -42,
        );
        let display = node.to_string();
        assert!(display.starts_with("HashWithSum("), "got: {}", display);
        assert!(display.contains("sum=-42"), "expected sum=-42: {}", display);
        assert!(
            display.contains(&hex::encode([0x11; HASH_LENGTH])),
            "expected kv_hash hex: {}",
            display
        );
        assert!(
            display.contains(&hex::encode([0x22; HASH_LENGTH])),
            "expected left hex: {}",
            display
        );
        assert!(
            display.contains(&hex::encode([0x33; HASH_LENGTH])),
            "expected right hex: {}",
            display
        );
    }

    // Display tests for the ProvableCountProvableSumTree dual-axis proof
    // nodes. Each variant has its own Display arm; testing them
    // individually keeps any future drift from being masked by a
    // wildcard match.

    #[test]
    fn display_kv_count_sum() {
        let node = Node::KVCountSum(b"k".to_vec(), b"v".to_vec(), 3, -7);
        let display = node.to_string();
        assert!(display.starts_with("KVCountSum("), "got: {}", display);
        assert!(display.contains("count=3"), "expected count=3: {}", display);
        assert!(display.contains("sum=-7"), "expected sum=-7: {}", display);
    }

    #[test]
    fn display_kv_hash_count_sum() {
        let node = Node::KVHashCountSum([0xAB; HASH_LENGTH], 5, 100);
        let display = node.to_string();
        assert!(display.starts_with("KVHashCountSum("), "got: {}", display);
        assert!(display.contains("count=5"), "expected count: {}", display);
        assert!(display.contains("sum=100"), "expected sum=100: {}", display);
    }

    #[test]
    fn display_kv_ref_value_hash_count_sum() {
        let node = Node::KVRefValueHashCountSum(
            b"k".to_vec(),
            b"v".to_vec(),
            [0xCD; HASH_LENGTH],
            u64::MAX,
            i64::MIN,
        );
        let display = node.to_string();
        assert!(
            display.starts_with("KVRefValueHashCountSum("),
            "got: {}",
            display
        );
        assert!(
            display.contains(&u64::MAX.to_string()),
            "expected u64::MAX: {}",
            display
        );
        assert!(
            display.contains(&i64::MIN.to_string()),
            "expected i64::MIN: {}",
            display
        );
    }

    #[test]
    fn display_kv_digest_count_sum() {
        let node = Node::KVDigestCountSum(b"k".to_vec(), [0xEF; HASH_LENGTH], 7, i64::MAX);
        let display = node.to_string();
        assert!(display.starts_with("KVDigestCountSum("), "got: {}", display);
        assert!(display.contains("count=7"), "expected count=7: {}", display);
        assert!(
            display.contains(&i64::MAX.to_string()),
            "expected i64::MAX: {}",
            display
        );
    }

    #[test]
    fn display_hash_with_count_and_sum() {
        let node = Node::HashWithCountAndSum(
            [0x11; HASH_LENGTH],
            [0x22; HASH_LENGTH],
            [0x33; HASH_LENGTH],
            42,
            -100,
        );
        let display = node.to_string();
        assert!(
            display.starts_with("HashWithCountAndSum("),
            "got: {}",
            display
        );
        assert!(
            display.contains("count=42"),
            "expected count=42: {}",
            display
        );
        assert!(
            display.contains("sum=-100"),
            "expected sum=-100: {}",
            display
        );
        assert!(
            display.contains(&hex::encode([0x11; HASH_LENGTH])),
            "expected kv_hash hex: {}",
            display
        );
    }

    // Encoding round-trip tests for the dual-axis Node variants. These
    // exercise the Push + PushInverted Encode arms (tag bytes 0x40..=0x4D)
    // and the matching Decode arms; without these tests the encoding.rs
    // additions show up as ~263 uncovered lines.

    /// Rows reveal a key and a value, range bounds a key only, and hash
    /// nodes neither, in every node family.
    #[test]
    fn key_and_value_accessors_classify_every_family() {
        let key = vec![7u8];
        let value = vec![8u8, 9];
        let hash = [3u8; 32];
        let feature = TreeFeatureType::BasicMerkNode;

        let rows = [
            Node::KV(key.clone(), value.clone()),
            Node::KVValueHash(key.clone(), value.clone(), hash),
            Node::KVBackwardsReferencesValueHash(key.clone(), value.clone(), hash),
            Node::KVValueHashFeatureType(key.clone(), value.clone(), hash, feature),
            Node::KVValueHashFeatureTypeWithChildHash(
                key.clone(),
                value.clone(),
                hash,
                feature,
                hash,
            ),
            Node::KVCount(key.clone(), value.clone(), 1),
            Node::KVRefValueHash(key.clone(), value.clone(), hash),
            Node::KVRefValueHashCount(key.clone(), value.clone(), hash, 1),
            Node::KVSum(key.clone(), value.clone(), 1),
            Node::KVRefValueHashSum(key.clone(), value.clone(), hash, 1),
            Node::KVCountSum(key.clone(), value.clone(), 1, 1),
            Node::KVRefValueHashCountSum(key.clone(), value.clone(), hash, 1, 1),
            Node::KVCountBigSum(key.clone(), value.clone(), 1, 1),
            Node::KVRefValueHashCountBigSum(key.clone(), value.clone(), hash, 1, 1),
        ];
        for node in rows {
            assert_eq!(
                node.key_and_value(),
                Some((key.as_slice(), value.as_slice())),
                "{node}"
            );
            assert_eq!(node.key(), Some(key.as_slice()), "{node}");
        }

        let bounds = [
            Node::KVDigest(key.clone(), hash),
            Node::KVDigestCount(key.clone(), hash, 1),
            Node::KVDigestSum(key.clone(), hash, 1),
            Node::KVDigestCountSum(key.clone(), hash, 1, 1),
            Node::KVDigestCountBigSum(key.clone(), hash, 1, 1),
        ];
        for node in bounds {
            assert_eq!(node.key_and_value(), None, "{node}");
            assert_eq!(node.key(), Some(key.as_slice()), "{node}");
        }

        let hidden = [
            Node::Hash(hash),
            Node::KVHash(hash),
            Node::KVHashCount(hash, 1),
            Node::KVHashSum(hash, 1),
            Node::KVHashCountSum(hash, 1, 1),
            Node::KVHashCountBigSum(hash, 1, 1),
            Node::HashWithCount(hash, hash, hash, 1),
            Node::HashWithSum(hash, hash, hash, 1),
            Node::HashWithCountAndSum(hash, hash, hash, 1, 1),
            Node::HashWithCountAndBigSum(hash, hash, hash, 1, 1),
        ];
        for node in hidden {
            assert_eq!(node.key_and_value(), None, "{node}");
            assert_eq!(node.key(), None, "{node}");
        }
    }

    fn round_trip_push(node: Node) {
        use ed::Encode;
        let op = Op::Push(node.clone());
        let mut buf = Vec::new();
        op.encode_into(&mut buf).expect("encode push");
        let decoded = Op::decode(&buf[..]).expect("decode push");
        match decoded {
            Op::Push(n) => assert_eq!(n, node, "Push round trip"),
            other => panic!("expected Push, got {:?}", other),
        }
        // PushInverted round trip uses a different tag byte; verify it too.
        let op_inv = Op::PushInverted(node.clone());
        let mut buf_inv = Vec::new();
        op_inv.encode_into(&mut buf_inv).expect("encode pushinv");
        let decoded_inv = Op::decode(&buf_inv[..]).expect("decode pushinv");
        match decoded_inv {
            Op::PushInverted(n) => assert_eq!(n, node, "PushInverted round trip"),
            other => panic!("expected PushInverted, got {:?}", other),
        }
        // Tag bytes must differ between Push and PushInverted.
        assert_ne!(
            buf[0], buf_inv[0],
            "Push and PushInverted must use distinct tag bytes for this Node variant"
        );
    }

    #[test]
    fn round_trip_kv_count_sum_small_value() {
        round_trip_push(Node::KVCountSum(b"k".to_vec(), b"value".to_vec(), 3, -7));
    }

    #[test]
    fn round_trip_kv_count_sum_large_value() {
        // value.len() >= 65536 triggers the alternate large-value tag byte
        // (0x41 / 0x48 for Push / PushInverted respectively).
        let large_value = vec![0xAA; 70_000];
        round_trip_push(Node::KVCountSum(
            b"k".to_vec(),
            large_value,
            u64::MAX,
            i64::MAX,
        ));
    }

    #[test]
    fn round_trip_kv_hash_count_sum() {
        round_trip_push(Node::KVHashCountSum([0xAB; HASH_LENGTH], 42, 100));
    }

    #[test]
    fn round_trip_kv_ref_value_hash_count_sum_small_value() {
        round_trip_push(Node::KVRefValueHashCountSum(
            b"k".to_vec(),
            b"v".to_vec(),
            [0xCD; HASH_LENGTH],
            7,
            -3,
        ));
    }

    #[test]
    fn round_trip_kv_ref_value_hash_count_sum_large_value() {
        let large_value = vec![0xBB; 70_000];
        round_trip_push(Node::KVRefValueHashCountSum(
            b"k".to_vec(),
            large_value,
            [0xCD; HASH_LENGTH],
            0,
            i64::MIN,
        ));
    }

    #[test]
    fn round_trip_kv_digest_count_sum() {
        round_trip_push(Node::KVDigestCountSum(
            b"k".to_vec(),
            [0xEF; HASH_LENGTH],
            u64::MAX,
            i64::MIN,
        ));
    }

    #[test]
    fn round_trip_hash_with_count_and_sum_extremes() {
        // Cover several extreme value combinations so the varint encoding
        // for both axes is exercised for u64 and i64 boundary cases.
        for &(count, sum) in &[
            (0u64, 0i64),
            (1, 1),
            (1, -1),
            (u64::MAX, i64::MAX),
            (u64::MAX, i64::MIN),
            (42, -100),
        ] {
            round_trip_push(Node::HashWithCountAndSum(
                [0x11; HASH_LENGTH],
                [0x22; HASH_LENGTH],
                [0x33; HASH_LENGTH],
                count,
                sum,
            ));
        }
    }

    /// `encoding_length` must agree with the actual byte length produced
    /// by `encode_into` for every dual-axis variant — otherwise size
    /// estimates feed wrong buffer allocations downstream. The check
    /// covers Push and PushInverted plus small/large value variants
    /// where applicable.
    #[test]
    fn encoding_length_matches_actual_byte_length_for_dual_axis() {
        use ed::Encode;

        let dual_axis_nodes = [
            Node::KVCountSum(b"k".to_vec(), b"v".to_vec(), 3, -7),
            Node::KVCountSum(b"k".to_vec(), vec![0xAA; 70_000], u64::MAX, i64::MAX),
            Node::KVHashCountSum([0xAB; HASH_LENGTH], 42, 100),
            Node::KVRefValueHashCountSum(b"k".to_vec(), b"v".to_vec(), [0xCD; HASH_LENGTH], 7, -3),
            Node::KVRefValueHashCountSum(
                b"k".to_vec(),
                vec![0xBB; 70_000],
                [0xCD; HASH_LENGTH],
                0,
                i64::MIN,
            ),
            Node::KVDigestCountSum(b"k".to_vec(), [0xEF; HASH_LENGTH], u64::MAX, i64::MIN),
            Node::HashWithCountAndSum(
                [0x11; HASH_LENGTH],
                [0x22; HASH_LENGTH],
                [0x33; HASH_LENGTH],
                100,
                200,
            ),
        ];

        for node in dual_axis_nodes {
            for op in [Op::Push(node.clone()), Op::PushInverted(node.clone())] {
                let mut buf = Vec::new();
                <Op as Encode>::encode_into(&op, &mut buf).expect("encode");
                let predicted = <Op as Encode>::encoding_length(&op).expect("encoding_length");
                assert_eq!(
                    predicted,
                    buf.len(),
                    "encoding_length predicted {} but encode produced {} for {:?}",
                    predicted,
                    buf.len(),
                    op
                );
            }
        }
    }

    /// The five wide count and sum nodes round-trip as `Push` and
    /// `PushInverted`, with small and large values and sums outside the
    /// `i64` range, under tag bytes of their own (0x60..=0x6D), and their
    /// predicted encoding length is the encoded length.
    #[test]
    fn wide_count_and_sum_nodes_round_trip() {
        use ed::Encode;

        let past_i64 = i64::MAX as i128 * 3;
        let below_i64 = i64::MIN as i128 * 3;
        let nodes = [
            Node::KVCountBigSum(b"k".to_vec(), b"v".to_vec(), 3, -7),
            Node::KVCountBigSum(b"k".to_vec(), vec![0xAA; 70_000], u64::MAX, past_i64),
            Node::KVHashCountBigSum([0xAB; HASH_LENGTH], 42, below_i64),
            Node::KVRefValueHashCountBigSum(
                b"k".to_vec(),
                b"v".to_vec(),
                [0xCD; HASH_LENGTH],
                7,
                past_i64,
            ),
            Node::KVRefValueHashCountBigSum(
                b"k".to_vec(),
                vec![0xBB; 70_000],
                [0xCD; HASH_LENGTH],
                0,
                i128::MIN,
            ),
            Node::KVDigestCountBigSum(b"k".to_vec(), [0xEF; HASH_LENGTH], u64::MAX, i128::MAX),
            Node::HashWithCountAndBigSum(
                [0x11; HASH_LENGTH],
                [0x22; HASH_LENGTH],
                [0x33; HASH_LENGTH],
                100,
                past_i64,
            ),
        ];

        for node in nodes {
            round_trip_push(node.clone());
            for op in [Op::Push(node.clone()), Op::PushInverted(node.clone())] {
                let mut buf = Vec::new();
                <Op as Encode>::encode_into(&op, &mut buf).expect("encode");
                assert!(
                    (0x60..=0x6D).contains(&buf[0]),
                    "{op:?} must use a wide tag byte, got {:#x}",
                    buf[0]
                );
                assert_eq!(
                    buf.len(),
                    <Op as Encode>::encoding_length(&op).expect("encoding_length"),
                    "encoding_length must match the encoded length of {op:?}"
                );
            }
        }
    }

    /// Each wide node is displayed under its own name, with its count and
    /// sum.
    #[test]
    fn wide_count_and_sum_nodes_display() {
        let sum = i64::MAX as i128 * 3;
        for (node, name) in [
            (
                Node::KVCountBigSum(b"k".to_vec(), b"v".to_vec(), 3, sum),
                "KVCountBigSum(",
            ),
            (
                Node::KVHashCountBigSum([0xAB; HASH_LENGTH], 3, sum),
                "KVHashCountBigSum(",
            ),
            (
                Node::KVRefValueHashCountBigSum(
                    b"k".to_vec(),
                    b"v".to_vec(),
                    [0xCD; HASH_LENGTH],
                    3,
                    sum,
                ),
                "KVRefValueHashCountBigSum(",
            ),
            (
                Node::KVDigestCountBigSum(b"k".to_vec(), [0xEF; HASH_LENGTH], 3, sum),
                "KVDigestCountBigSum(",
            ),
            (
                Node::HashWithCountAndBigSum(
                    [0x11; HASH_LENGTH],
                    [0x22; HASH_LENGTH],
                    [0x33; HASH_LENGTH],
                    3,
                    sum,
                ),
                "HashWithCountAndBigSum(",
            ),
        ] {
            let display = format!("{node}");
            assert!(display.starts_with(name), "got: {display}");
            assert!(display.contains("count=3"), "got: {display}");
            assert!(display.contains(&format!("sum={sum}")), "got: {display}");
        }
    }
}
