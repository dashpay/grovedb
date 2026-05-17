//! Tree feature type for Merk nodes

use std::io::{Read, Write};

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use ed::{Decode, Encode, Terminated};
#[cfg(feature = "blockchain")]
use grovedb_costs::TreeCostType;
use integer_encoding::{VarInt, VarIntReader, VarIntWriter};

use self::TreeFeatureType::{
    BasicMerkNode, BigSummedMerkNode, CountedMerkNode, CountedSummedMerkNode,
    ProvableCountedAndProvableSummedMerkNode, ProvableCountedMerkNode, ProvableSummedMerkNode,
    SummedMerkNode,
};
use crate::proofs::TreeFeatureType::ProvableCountedSummedMerkNode;

/// Node type classification for Merk tree nodes

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum NodeType {
    /// Normal node (no aggregation)
    NormalNode,
    /// Sum node (i64 sum)
    SumNode,
    /// Big sum node (i128 sum)
    BigSumNode,
    /// Count node (u64 count)
    CountNode,
    /// Count + sum node
    CountSumNode,
    /// Provable count node (count included in hash)
    ProvableCountNode,
    /// Provable count + sum node (count included in hash)
    ProvableCountSumNode,
    /// Provable sum node (sum included in hash). Mirrors `SumNode`'s
    /// encoding layout (i64 varint, 9-byte feature length), but the hash
    /// computation includes the sum so the sum participates in the node
    /// hash (unlike `SumNode`, which only tracks the sum alongside).
    ProvableSumNode,
    /// Provable count + provable sum node. Both the u64 count AND the i64
    /// sum are included in the node hash via
    /// `node_hash_with_count_and_sum`. Mirrors `CountSumNode`'s encoding
    /// layout (count varint + sum varint, 17-byte feature length) but
    /// extends the hash to bind both aggregates.
    ProvableCountProvableSumNode,
}

impl NodeType {
    /// The byte length of the feature data for this node type
    pub const fn feature_len(&self) -> u32 {
        match self {
            NodeType::NormalNode => 1,
            NodeType::SumNode => 9,
            NodeType::BigSumNode => 17,
            NodeType::CountNode => 9,
            NodeType::CountSumNode => 17,
            NodeType::ProvableCountNode => 9,
            NodeType::ProvableCountSumNode => 17,
            NodeType::ProvableSumNode => 9,
            NodeType::ProvableCountProvableSumNode => 17,
        }
    }

    /// The cost in bytes of the feature data (excluding the 1-byte tag)
    pub const fn cost(&self) -> u32 {
        match self {
            NodeType::NormalNode => 0,
            NodeType::SumNode => 8,
            NodeType::BigSumNode => 16,
            NodeType::CountNode => 8,
            NodeType::CountSumNode => 16,
            NodeType::ProvableCountNode => 8,
            NodeType::ProvableCountSumNode => 16,
            NodeType::ProvableSumNode => 8,
            NodeType::ProvableCountProvableSumNode => 16,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
/// Basic or summed
pub enum TreeFeatureType {
    /// Basic Merk Tree Node
    BasicMerkNode,
    /// Summed Merk Tree Node
    SummedMerkNode(i64),
    /// Big Summed Merk Tree Node
    BigSummedMerkNode(i128),
    /// Counted Merk Tree Node
    CountedMerkNode(u64),
    /// Counted and summed Merk Tree None
    CountedSummedMerkNode(u64, i64),
    /// Provable Counted Merk Tree Node
    ProvableCountedMerkNode(u64),
    /// Provable Counted and Summed Merk Tree Node (count in hash, sum tracked)
    ProvableCountedSummedMerkNode(u64, i64),
    /// Provable Summed Merk Tree Node (sum included in hash).
    /// Mirrors `SummedMerkNode` for encoding/cost purposes, but the hash
    /// computation includes the sum so the sum participates in the node hash.
    ProvableSummedMerkNode(i64),
    /// Provable Counted AND Provable Summed Merk Tree Node — both the u64
    /// count AND the i64 sum are baked into the node hash via
    /// `node_hash_with_count_and_sum`. Mirrors
    /// `ProvableCountedSummedMerkNode` on the wire (tag byte 8, two
    /// varints) but the hash computation binds both aggregates rather
    /// than just the count.
    ProvableCountedAndProvableSummedMerkNode(u64, i64),
}

impl TreeFeatureType {
    /// Returns the count of elements in this subtree, if available.
    /// Returns Some(count) for CountedMerkNode, ProvableCountedMerkNode,
    /// CountedSummedMerkNode, and ProvableCountedSummedMerkNode variants.
    /// Returns None for BasicMerkNode, SummedMerkNode, BigSummedMerkNode.
    pub fn count(&self) -> Option<u64> {
        match self {
            CountedMerkNode(count)
            | ProvableCountedMerkNode(count)
            | CountedSummedMerkNode(count, _)
            | ProvableCountedSummedMerkNode(count, _)
            | ProvableCountedAndProvableSummedMerkNode(count, _) => Some(*count),
            BasicMerkNode
            | SummedMerkNode(_)
            | BigSummedMerkNode(_)
            | ProvableSummedMerkNode(_) => None,
        }
    }

    /// Force the count component of this feature type to 0, leaving sum
    /// components untouched. No-op for variants that don't carry a count.
    ///
    /// Used by the `Element::NonCounted` wrapper: when computing the parent
    /// tree's feature type for a non-counted child, we use the inner element's
    /// feature type but zero out its count so the parent's aggregate
    /// excludes it.
    pub fn zero_count(&mut self) {
        match self {
            CountedMerkNode(count) | ProvableCountedMerkNode(count) => *count = 0,
            CountedSummedMerkNode(count, _)
            | ProvableCountedSummedMerkNode(count, _)
            | ProvableCountedAndProvableSummedMerkNode(count, _) => *count = 0,
            BasicMerkNode
            | SummedMerkNode(_)
            | BigSummedMerkNode(_)
            | ProvableSummedMerkNode(_) => {}
        }
    }

    /// Force the sum component of this feature type to 0, leaving count
    /// components untouched. No-op for variants that don't carry a sum.
    ///
    /// Mirrors `zero_count` for the `Element::NotSummed` wrapper: when
    /// computing the parent sum tree's feature type for a not-summed child,
    /// we use the inner element's feature type but zero out its sum so the
    /// parent's aggregate excludes it.
    pub fn zero_sum(&mut self) {
        match self {
            SummedMerkNode(sum) | ProvableSummedMerkNode(sum) => *sum = 0,
            BigSummedMerkNode(sum) => *sum = 0,
            CountedSummedMerkNode(_, sum)
            | ProvableCountedSummedMerkNode(_, sum)
            | ProvableCountedAndProvableSummedMerkNode(_, sum) => *sum = 0,
            BasicMerkNode | CountedMerkNode(_) | ProvableCountedMerkNode(_) => {}
        }
    }

    /// Get the NodeType for this feature type
    pub fn node_type(&self) -> NodeType {
        match self {
            BasicMerkNode => NodeType::NormalNode,
            SummedMerkNode(_) => NodeType::SumNode,
            BigSummedMerkNode(_) => NodeType::BigSumNode,
            CountedMerkNode(_) => NodeType::CountNode,
            CountedSummedMerkNode(..) => NodeType::CountSumNode,
            ProvableCountedMerkNode(_) => NodeType::ProvableCountNode,
            ProvableCountedSummedMerkNode(..) => NodeType::ProvableCountSumNode,
            ProvableSummedMerkNode(_) => NodeType::ProvableSumNode,
            ProvableCountedAndProvableSummedMerkNode(..) => NodeType::ProvableCountProvableSumNode,
        }
    }

    /// Get encoding cost of self
    #[inline]
    pub fn encoding_cost(&self) -> usize {
        match self {
            BasicMerkNode => 1,
            SummedMerkNode(_) => 9,
            BigSummedMerkNode(_) => 17,
            CountedMerkNode(_) => 9,
            CountedSummedMerkNode(..) => 17,
            ProvableCountedMerkNode(_) => 9,
            ProvableCountedSummedMerkNode(..) => 17,
            ProvableSummedMerkNode(_) => 9,
            ProvableCountedAndProvableSummedMerkNode(..) => 17,
        }
    }
}

/// Methods that depend on grovedb-costs (behind `blockchain` feature)
#[cfg(feature = "blockchain")]
impl TreeFeatureType {
    /// Get length of encoded feature type with TreeCostType
    #[inline]
    pub fn tree_feature_specialized_type_and_length(&self) -> Option<(TreeCostType, u32)> {
        match self {
            BasicMerkNode => None,
            SummedMerkNode(m) => Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
                m.encode_var_vec().len() as u32,
            )),
            BigSummedMerkNode(_) => Some((TreeCostType::TreeFeatureUses16Bytes, 16)),
            CountedMerkNode(m) => Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
                m.encode_var_vec().len() as u32,
            )),
            CountedSummedMerkNode(count, sum) => Some((
                TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes,
                count.encode_var_vec().len() as u32 + sum.encode_var_vec().len() as u32,
            )),
            ProvableCountedMerkNode(m) => Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
                m.encode_var_vec().len() as u32,
            )),
            ProvableCountedSummedMerkNode(count, sum) => Some((
                TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes,
                count.encode_var_vec().len() as u32 + sum.encode_var_vec().len() as u32,
            )),
            ProvableSummedMerkNode(m) => Some((
                TreeCostType::TreeFeatureUsesVarIntCostAs8Bytes,
                m.encode_var_vec().len() as u32,
            )),
            ProvableCountedAndProvableSummedMerkNode(count, sum) => Some((
                TreeCostType::TreeFeatureUsesTwoVarIntsCostAs16Bytes,
                count.encode_var_vec().len() as u32 + sum.encode_var_vec().len() as u32,
            )),
        }
    }
}

impl Encode for TreeFeatureType {
    #[inline]
    fn encode_into<W: Write>(&self, dest: &mut W) -> ed::Result<()> {
        match self {
            BasicMerkNode => {
                dest.write_all(&[0])?;
                Ok(())
            }
            SummedMerkNode(sum) => {
                dest.write_all(&[1])?;
                dest.write_varint(*sum)?;
                Ok(())
            }
            BigSummedMerkNode(sum) => {
                dest.write_all(&[2])?;
                dest.write_i128::<BigEndian>(*sum)?;
                Ok(())
            }
            CountedMerkNode(count) => {
                dest.write_all(&[3])?;
                dest.write_varint(*count)?;
                Ok(())
            }
            CountedSummedMerkNode(count, sum) => {
                dest.write_all(&[4])?;
                dest.write_varint(*count)?;
                dest.write_varint(*sum)?;
                Ok(())
            }
            ProvableCountedMerkNode(count) => {
                dest.write_all(&[5])?;
                dest.write_varint(*count)?;
                Ok(())
            }
            ProvableCountedSummedMerkNode(count, sum) => {
                dest.write_all(&[6])?;
                dest.write_varint(*count)?;
                dest.write_varint(*sum)?;
                Ok(())
            }
            ProvableSummedMerkNode(sum) => {
                dest.write_all(&[7])?;
                dest.write_varint(*sum)?;
                Ok(())
            }
            ProvableCountedAndProvableSummedMerkNode(count, sum) => {
                dest.write_all(&[8])?;
                dest.write_varint(*count)?;
                dest.write_varint(*sum)?;
                Ok(())
            }
        }
    }

    #[inline]
    fn encoding_length(&self) -> ed::Result<usize> {
        match self {
            BasicMerkNode => Ok(1),
            SummedMerkNode(sum) => {
                let encoded_sum = sum.encode_var_vec();
                Ok(1 + encoded_sum.len())
            }
            BigSummedMerkNode(_) => Ok(17),
            CountedMerkNode(count) => {
                let encoded_sum = count.encode_var_vec();
                Ok(1 + encoded_sum.len())
            }
            CountedSummedMerkNode(count, sum) => {
                let encoded_lengths = count.encode_var_vec().len() + sum.encode_var_vec().len();
                Ok(1 + encoded_lengths)
            }
            ProvableCountedMerkNode(count) => {
                let encoded_sum = count.encode_var_vec();
                Ok(1 + encoded_sum.len())
            }
            ProvableCountedSummedMerkNode(count, sum) => {
                let encoded_lengths = count.encode_var_vec().len() + sum.encode_var_vec().len();
                Ok(1 + encoded_lengths)
            }
            ProvableSummedMerkNode(sum) => {
                let encoded_sum = sum.encode_var_vec();
                Ok(1 + encoded_sum.len())
            }
            ProvableCountedAndProvableSummedMerkNode(count, sum) => {
                let encoded_lengths = count.encode_var_vec().len() + sum.encode_var_vec().len();
                Ok(1 + encoded_lengths)
            }
        }
    }
}

impl Terminated for TreeFeatureType {}

impl Decode for TreeFeatureType {
    #[inline]
    fn decode<R: Read>(mut input: R) -> ed::Result<Self> {
        let mut feature_type: [u8; 1] = [0];
        input.read_exact(&mut feature_type)?;
        match feature_type {
            [0] => Ok(BasicMerkNode),
            [1] => {
                let encoded_sum: i64 = input.read_varint()?;
                Ok(SummedMerkNode(encoded_sum))
            }
            [2] => {
                let encoded_sum: i128 = input.read_i128::<BigEndian>()?;
                Ok(BigSummedMerkNode(encoded_sum))
            }
            [3] => {
                let encoded_count: u64 = input.read_varint()?;
                Ok(CountedMerkNode(encoded_count))
            }
            [4] => {
                let encoded_count: u64 = input.read_varint()?;
                let encoded_sum: i64 = input.read_varint()?;
                Ok(CountedSummedMerkNode(encoded_count, encoded_sum))
            }
            [5] => {
                let encoded_count: u64 = input.read_varint()?;
                Ok(ProvableCountedMerkNode(encoded_count))
            }
            [6] => {
                let encoded_count: u64 = input.read_varint()?;
                let encoded_sum: i64 = input.read_varint()?;
                Ok(ProvableCountedSummedMerkNode(encoded_count, encoded_sum))
            }
            [7] => {
                let encoded_sum: i64 = input.read_varint()?;
                Ok(ProvableSummedMerkNode(encoded_sum))
            }
            [8] => {
                let encoded_count: u64 = input.read_varint()?;
                let encoded_sum: i64 = input.read_varint()?;
                Ok(ProvableCountedAndProvableSummedMerkNode(
                    encoded_count,
                    encoded_sum,
                ))
            }
            [b] => Err(ed::Error::UnexpectedByte(b)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_count_only_zeros_count() {
        let mut basic = BasicMerkNode;
        basic.zero_count();
        assert_eq!(basic, BasicMerkNode);

        let mut summed = SummedMerkNode(42);
        summed.zero_count();
        assert_eq!(summed, SummedMerkNode(42));

        let mut big_summed = BigSummedMerkNode(42);
        big_summed.zero_count();
        assert_eq!(big_summed, BigSummedMerkNode(42));

        let mut counted = CountedMerkNode(7);
        counted.zero_count();
        assert_eq!(counted, CountedMerkNode(0));

        let mut count_sum = CountedSummedMerkNode(7, 42);
        count_sum.zero_count();
        assert_eq!(count_sum, CountedSummedMerkNode(0, 42));

        let mut prov_counted = ProvableCountedMerkNode(7);
        prov_counted.zero_count();
        assert_eq!(prov_counted, ProvableCountedMerkNode(0));

        let mut prov_count_sum = ProvableCountedSummedMerkNode(7, 42);
        prov_count_sum.zero_count();
        assert_eq!(prov_count_sum, ProvableCountedSummedMerkNode(0, 42));
    }

    #[test]
    fn count_helper_returns_some_only_for_count_bearing() {
        assert_eq!(BasicMerkNode.count(), None);
        assert_eq!(SummedMerkNode(42).count(), None);
        assert_eq!(BigSummedMerkNode(42).count(), None);
        assert_eq!(CountedMerkNode(7).count(), Some(7));
        assert_eq!(CountedSummedMerkNode(7, 42).count(), Some(7));
        assert_eq!(ProvableCountedMerkNode(7).count(), Some(7));
        assert_eq!(ProvableCountedSummedMerkNode(7, 42).count(), Some(7));
        // ProvableSummedMerkNode carries a sum, not a count.
        assert_eq!(ProvableSummedMerkNode(42).count(), None);
    }

    /// `zero_sum` mirrors `zero_count`: it zeroes the sum component on
    /// every sum-bearing variant and is a no-op elsewhere.
    #[test]
    fn zero_sum_only_zeros_sum() {
        let mut basic = BasicMerkNode;
        basic.zero_sum();
        assert_eq!(basic, BasicMerkNode);

        let mut summed = SummedMerkNode(42);
        summed.zero_sum();
        assert_eq!(summed, SummedMerkNode(0));

        let mut big_summed = BigSummedMerkNode(42);
        big_summed.zero_sum();
        assert_eq!(big_summed, BigSummedMerkNode(0));

        let mut counted = CountedMerkNode(7);
        counted.zero_sum();
        assert_eq!(counted, CountedMerkNode(7));

        let mut count_sum = CountedSummedMerkNode(7, 42);
        count_sum.zero_sum();
        assert_eq!(count_sum, CountedSummedMerkNode(7, 0));

        let mut prov_counted = ProvableCountedMerkNode(7);
        prov_counted.zero_sum();
        assert_eq!(prov_counted, ProvableCountedMerkNode(7));

        let mut prov_count_sum = ProvableCountedSummedMerkNode(7, 42);
        prov_count_sum.zero_sum();
        assert_eq!(prov_count_sum, ProvableCountedSummedMerkNode(7, 0));

        let mut prov_summed = ProvableSummedMerkNode(42);
        prov_summed.zero_sum();
        assert_eq!(prov_summed, ProvableSummedMerkNode(0));
    }

    /// `ProvableSummedMerkNode` round-trips through `Encode`/`Decode` with
    /// tag byte 7 followed by a varint i64.
    #[test]
    fn provable_summed_round_trip() {
        for &sum in &[0i64, 1, -1, 42, -42, i64::MAX, i64::MIN] {
            let original = ProvableSummedMerkNode(sum);
            let mut buf = Vec::new();
            original.encode_into(&mut buf).expect("encode");
            // First byte must be the tag.
            assert_eq!(buf[0], 7);
            // Encoded length matches the variable-length `encoding_length`
            // (1 tag byte + varint i64). `encoding_cost` is the storage
            // worst-case (9 = 1 + 8) and does NOT have to match the
            // serialized length on the wire — it intentionally over-counts
            // so cost accounting is consistent across all sum values.
            assert_eq!(
                buf.len(),
                original.encoding_length().expect("encoding_length")
            );
            let back = TreeFeatureType::decode(&buf[..]).expect("decode");
            assert_eq!(back, original);
        }
    }

    /// The new `ProvableSumNode` carries the same feature length / cost as
    /// `SumNode` so cost accounting remains consistent.
    #[test]
    fn provable_sum_node_matches_sum_node_layout() {
        assert_eq!(
            NodeType::ProvableSumNode.feature_len(),
            NodeType::SumNode.feature_len()
        );
        assert_eq!(NodeType::ProvableSumNode.cost(), NodeType::SumNode.cost());
        assert_eq!(
            ProvableSummedMerkNode(0).node_type(),
            NodeType::ProvableSumNode
        );
    }

    /// `ProvableCountedAndProvableSummedMerkNode` round-trips through
    /// `Encode`/`Decode` with tag byte 8 followed by two varints
    /// (u64 count + i64 sum), mirroring `ProvableCountedSummedMerkNode`'s
    /// wire layout. Covers the count/sum extremes.
    #[test]
    fn provable_counted_and_provable_summed_round_trip() {
        for &(count, sum) in &[
            (0u64, 0i64),
            (1, 1),
            (1, -1),
            (42, -42),
            (u64::MAX, i64::MAX),
            (u64::MAX, i64::MIN),
            (7, 0),
            (0, -7),
        ] {
            let original = ProvableCountedAndProvableSummedMerkNode(count, sum);
            let mut buf = Vec::new();
            original.encode_into(&mut buf).expect("encode");
            assert_eq!(buf[0], 8, "tag byte for the dual-axis feature type");
            assert_eq!(
                buf.len(),
                original.encoding_length().expect("encoding_length"),
                "encoding_length must match the actual byte count"
            );
            let back = TreeFeatureType::decode(&buf[..]).expect("decode");
            assert_eq!(back, original);
        }
    }

    /// The new `ProvableCountProvableSumNode` mirrors `CountSumNode`'s
    /// feature length / cost for accounting consistency. The variant tag
    /// flows through `node_type()`.
    #[test]
    fn provable_count_provable_sum_node_matches_count_sum_layout() {
        assert_eq!(
            NodeType::ProvableCountProvableSumNode.feature_len(),
            NodeType::CountSumNode.feature_len()
        );
        assert_eq!(
            NodeType::ProvableCountProvableSumNode.cost(),
            NodeType::CountSumNode.cost()
        );
        assert_eq!(
            ProvableCountedAndProvableSummedMerkNode(0, 0).node_type(),
            NodeType::ProvableCountProvableSumNode
        );
    }

    /// `count()` returns the count for the dual-axis variant and `None`
    /// for pure-sum variants. Complements the existing
    /// `count_helper_returns_some_only_for_count_bearing` test by
    /// covering the new variant.
    #[test]
    fn count_helper_pulls_count_from_dual_axis_variant() {
        assert_eq!(
            ProvableCountedAndProvableSummedMerkNode(7, -42).count(),
            Some(7)
        );
        assert_eq!(
            ProvableCountedAndProvableSummedMerkNode(0, i64::MAX).count(),
            Some(0)
        );
        assert_eq!(
            ProvableCountedAndProvableSummedMerkNode(u64::MAX, 0).count(),
            Some(u64::MAX)
        );
    }

    /// `zero_count` zeroes the count component on the dual-axis variant
    /// and leaves the sum untouched. Symmetric: `zero_sum` zeroes the
    /// sum and leaves the count untouched.
    #[test]
    fn zero_count_and_zero_sum_only_zero_their_axis_for_dual_axis() {
        let mut n = ProvableCountedAndProvableSummedMerkNode(7, -42);
        n.zero_count();
        assert_eq!(n, ProvableCountedAndProvableSummedMerkNode(0, -42));

        let mut n = ProvableCountedAndProvableSummedMerkNode(7, -42);
        n.zero_sum();
        assert_eq!(n, ProvableCountedAndProvableSummedMerkNode(7, 0));
    }

    /// Decoder rejects an unknown leading tag byte. Tag byte 9 is the
    /// next unallocated slot above 8 (`ProvableCountedAndProvableSummed`),
    /// so it exercises the byte-mismatch path without colliding with any
    /// existing variant.
    #[test]
    fn decode_rejects_tag_byte_9() {
        let buf = vec![9u8, 0];
        let res = TreeFeatureType::decode(&buf[..]);
        assert!(res.is_err(), "decode must reject unknown tag byte");
    }
}
