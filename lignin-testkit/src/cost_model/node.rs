//! A Merk node as its logical fields, and the length its record encodes to
//! as a function of their sizes.
//!
//! Today's engine keeps a node as one record and charges a fetch by the
//! length of that record. The model keeps the fields and works the length
//! out ([`LogicalNode::record_len`]), which is what an engine that stores
//! no such record has to do. The layout is written here from
//! `docs/lignindb/cost-model.md`, not taken from Merk, so that the tests can
//! check one against the other.

use super::rules::{signed_varint_len, varint_len, zigzag};

/// The length of a hash.
pub const HASH_LEN: usize = 32;

/// The aggregate a node contributes (its feature) or a link carries for the
/// subtree below it. Both are written as one tag byte and a payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    /// Tag 0: nothing.
    None,
    /// Tag 1: a sum, as a signed varint.
    Sum(i64),
    /// Tag 2: a sum of 16 bytes.
    BigSum(i128),
    /// Tag 3: a count, as a varint.
    Count(u64),
    /// Tag 4: a count and a sum, as two varints.
    CountAndSum(u64, i64),
    /// Tag 5: a count that is hashed into the node, as a varint.
    ProvableCount(u64),
    /// Tag 6: a hashed count and a sum, as two varints.
    ProvableCountAndSum(u64, i64),
    /// Tag 7: a sum that is hashed into the node, as a signed varint.
    ProvableSum(i64),
    /// Tag 8: a hashed count and a hashed sum, as two varints.
    ProvableCountAndProvableSum(u64, i64),
    /// Tag 9: a hashed count as a varint and a hashed sum of 16 bytes.
    ProvableCountAndProvableBigSum(u64, i128),
}

/// The fixed-size class an aggregate is charged in, whatever its varints
/// take: `TreeCostType` without the engine's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostClass {
    /// One varint, charged as 8 bytes.
    OneVarint,
    /// Two varints, charged as 16 bytes.
    TwoVarints,
    /// 16 bytes, charged as 16.
    Wide,
    /// A varint and 16 bytes, charged as 24.
    VarintAndWide,
}

impl CostClass {
    /// The bytes the class is charged as.
    pub fn size(self) -> u32 {
        match self {
            CostClass::OneVarint => 8,
            CostClass::TwoVarints | CostClass::Wide => 16,
            CostClass::VarintAndWide => 24,
        }
    }
}

impl Aggregate {
    /// The tag byte.
    pub fn tag(&self) -> u8 {
        match self {
            Aggregate::None => 0,
            Aggregate::Sum(_) => 1,
            Aggregate::BigSum(_) => 2,
            Aggregate::Count(_) => 3,
            Aggregate::CountAndSum(..) => 4,
            Aggregate::ProvableCount(_) => 5,
            Aggregate::ProvableCountAndSum(..) => 6,
            Aggregate::ProvableSum(_) => 7,
            Aggregate::ProvableCountAndProvableSum(..) => 8,
            Aggregate::ProvableCountAndProvableBigSum(..) => 9,
        }
    }

    /// The length of the payload after the tag byte.
    pub fn payload_len(&self) -> u32 {
        match self {
            Aggregate::None => 0,
            Aggregate::Sum(sum) | Aggregate::ProvableSum(sum) => signed_varint_len(*sum),
            Aggregate::BigSum(_) => 16,
            Aggregate::Count(count) | Aggregate::ProvableCount(count) => varint_len(*count),
            Aggregate::CountAndSum(count, sum)
            | Aggregate::ProvableCountAndSum(count, sum)
            | Aggregate::ProvableCountAndProvableSum(count, sum) => {
                varint_len(*count) + signed_varint_len(*sum)
            }
            Aggregate::ProvableCountAndProvableBigSum(count, _) => varint_len(*count) + 16,
        }
    }

    /// The class the aggregate is charged in, and none for no aggregate.
    pub fn cost_class(&self) -> Option<CostClass> {
        match self {
            Aggregate::None => None,
            Aggregate::Sum(_)
            | Aggregate::Count(_)
            | Aggregate::ProvableCount(_)
            | Aggregate::ProvableSum(_) => Some(CostClass::OneVarint),
            Aggregate::CountAndSum(..)
            | Aggregate::ProvableCountAndSum(..)
            | Aggregate::ProvableCountAndProvableSum(..) => Some(CostClass::TwoVarints),
            Aggregate::BigSum(_) => Some(CostClass::Wide),
            Aggregate::ProvableCountAndProvableBigSum(..) => Some(CostClass::VarintAndWide),
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.tag());
        match self {
            Aggregate::None => {}
            Aggregate::Sum(sum) | Aggregate::ProvableSum(sum) => put_varint(out, zigzag(*sum)),
            Aggregate::BigSum(sum) => out.extend_from_slice(&sum.to_be_bytes()),
            Aggregate::Count(count) | Aggregate::ProvableCount(count) => put_varint(out, *count),
            Aggregate::CountAndSum(count, sum)
            | Aggregate::ProvableCountAndSum(count, sum)
            | Aggregate::ProvableCountAndProvableSum(count, sum) => {
                put_varint(out, *count);
                put_varint(out, zigzag(*sum));
            }
            Aggregate::ProvableCountAndProvableBigSum(count, sum) => {
                put_varint(out, *count);
                out.extend_from_slice(&sum.to_be_bytes());
            }
        }
    }

    fn parse(input: &mut &[u8]) -> Option<Self> {
        Some(match take_byte(input)? {
            0 => Aggregate::None,
            1 => Aggregate::Sum(take_signed_varint(input)?),
            2 => Aggregate::BigSum(take_wide(input)?),
            3 => Aggregate::Count(take_varint(input)?),
            4 => Aggregate::CountAndSum(take_varint(input)?, take_signed_varint(input)?),
            5 => Aggregate::ProvableCount(take_varint(input)?),
            6 => Aggregate::ProvableCountAndSum(take_varint(input)?, take_signed_varint(input)?),
            7 => Aggregate::ProvableSum(take_signed_varint(input)?),
            8 => Aggregate::ProvableCountAndProvableSum(
                take_varint(input)?,
                take_signed_varint(input)?,
            ),
            9 => Aggregate::ProvableCountAndProvableBigSum(take_varint(input)?, take_wide(input)?),
            _ => return None,
        })
    }
}

/// What a node holds of one child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicalLink {
    /// The key of the child.
    pub key: Vec<u8>,
    /// The hash of the child.
    pub hash: [u8; HASH_LEN],
    /// The height of the child's left subtree.
    pub left_height: u8,
    /// The height of the child's right subtree.
    pub right_height: u8,
    /// The aggregate of the subtree under the child.
    pub aggregate: Aggregate,
}

impl LogicalLink {
    /// The bytes the link takes in a record: a key length byte, the key, the
    /// hash, the two heights, and the aggregate with its tag.
    pub fn encoded_len(&self) -> u64 {
        link_len(self.key.len(), self.aggregate.payload_len())
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.key.len() as u8);
        out.extend_from_slice(&self.key);
        out.extend_from_slice(&self.hash);
        out.push(self.left_height);
        out.push(self.right_height);
        self.aggregate.encode_into(out);
    }

    fn parse(input: &mut &[u8]) -> Option<Self> {
        let key_len = usize::from(take_byte(input)?);
        let key = take(input, key_len)?.to_vec();
        let hash = take_hash(input)?;
        let left_height = take_byte(input)?;
        let right_height = take_byte(input)?;
        let aggregate = Aggregate::parse(input)?;
        Some(LogicalLink {
            key,
            hash,
            left_height,
            right_height,
            aggregate,
        })
    }
}

/// The bytes a link takes in a record, from the length of the child's key
/// and of its aggregate's payload.
pub fn link_len(key_len: usize, aggregate_payload_len: u32) -> u64 {
    1 + key_len as u64 + HASH_LEN as u64 + 2 + 1 + u64::from(aggregate_payload_len)
}

/// The sizes a node's record length is a function of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeSizes {
    /// The length of the value.
    pub value_len: usize,
    /// The length of the payload of the node's own aggregate.
    pub feature_payload_len: u32,
    /// For each child the node has, left then right: the length of its key
    /// and of the payload of its aggregate.
    pub children: [Option<(usize, u32)>; 2],
}

impl NodeSizes {
    /// The length of the record today's engine stores for a node of these
    /// sizes, and so what a fetch of the node loads: one option byte and the
    /// link for each side, the feature with its tag, the two hashes of the
    /// node, and the value.
    pub fn record_len(&self) -> u64 {
        let links: u64 = self
            .children
            .iter()
            .map(|child| {
                1 + child.map_or(0, |(key_len, aggregate_len)| {
                    link_len(key_len, aggregate_len)
                })
            })
            .sum();
        links
            + 1
            + u64::from(self.feature_payload_len)
            + 2 * HASH_LEN as u64
            + self.value_len as u64
    }
}

/// A Merk node, as the fields an engine has to keep of it. Its key is what
/// it is stored under and is not part of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicalNode {
    /// The left child, if any.
    pub left: Option<LogicalLink>,
    /// The right child, if any.
    pub right: Option<LogicalLink>,
    /// What the node itself contributes to the aggregate of its tree.
    pub feature: Aggregate,
    /// The hash of the key and the value hash.
    pub kv_hash: [u8; HASH_LEN],
    /// The hash of the value, combined with what the value refers to.
    pub value_hash: [u8; HASH_LEN],
    /// The value.
    pub value: Vec<u8>,
}

impl LogicalNode {
    /// The sizes of the node.
    pub fn sizes(&self) -> NodeSizes {
        let child = |link: &Option<LogicalLink>| {
            link.as_ref()
                .map(|link| (link.key.len(), link.aggregate.payload_len()))
        };
        NodeSizes {
            value_len: self.value.len(),
            feature_payload_len: self.feature.payload_len(),
            children: [child(&self.left), child(&self.right)],
        }
    }

    /// The length of the node's record, from its sizes alone.
    pub fn record_len(&self) -> u64 {
        self.sizes().record_len()
    }

    /// The record today's engine stores for the node.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.record_len() as usize);
        for link in [&self.left, &self.right] {
            match link {
                None => out.push(0),
                Some(link) => {
                    out.push(1);
                    link.encode_into(&mut out);
                }
            }
        }
        self.feature.encode_into(&mut out);
        out.extend_from_slice(&self.kv_hash);
        out.extend_from_slice(&self.value_hash);
        out.extend_from_slice(&self.value);
        out
    }

    /// The node `record` is the record of. `None` when the bytes are not a
    /// node record, or are one that [`LogicalNode::encode`] would write
    /// differently.
    pub fn parse(record: &[u8]) -> Option<Self> {
        let mut input = record;
        let left = Self::parse_side(&mut input)?;
        let right = Self::parse_side(&mut input)?;
        let feature = Aggregate::parse(&mut input)?;
        let kv_hash = take_hash(&mut input)?;
        let value_hash = take_hash(&mut input)?;
        let node = LogicalNode {
            left,
            right,
            feature,
            kv_hash,
            value_hash,
            value: input.to_vec(),
        };
        (node.encode() == record).then_some(node)
    }

    fn parse_side(input: &mut &[u8]) -> Option<Option<LogicalLink>> {
        match take_byte(input)? {
            0 => Some(None),
            1 => LogicalLink::parse(input).map(Some),
            _ => None,
        }
    }
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    let (head, tail) = input.split_at_checked(len)?;
    *input = tail;
    Some(head)
}

fn take_byte(input: &mut &[u8]) -> Option<u8> {
    take(input, 1).map(|bytes| bytes[0])
}

fn take_hash(input: &mut &[u8]) -> Option<[u8; HASH_LEN]> {
    take(input, HASH_LEN)?.try_into().ok()
}

fn take_wide(input: &mut &[u8]) -> Option<i128> {
    Some(i128::from_be_bytes(take(input, 16)?.try_into().ok()?))
}

fn take_varint(input: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = take_byte(input)?;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn take_signed_varint(input: &mut &[u8]) -> Option<i64> {
    let value = take_varint(input)?;
    Some((value >> 1) as i64 ^ -((value & 1) as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(key_len: usize, aggregate: Aggregate) -> LogicalLink {
        LogicalLink {
            key: vec![0xAB; key_len],
            hash: [key_len as u8; HASH_LEN],
            left_height: 3,
            right_height: 4,
            aggregate,
        }
    }

    #[test]
    fn bytes_that_are_not_a_canonical_node_record_are_not_parsed() {
        let node = LogicalNode {
            left: Some(link(2, Aggregate::Count(5))),
            right: None,
            feature: Aggregate::Sum(-3),
            kv_hash: [1; HASH_LEN],
            value_hash: [2; HASH_LEN],
            value: b"value".to_vec(),
        };
        let record = node.encode();
        assert_eq!(LogicalNode::parse(&record), Some(node));

        for len in 0..record.len() - 5 {
            assert_eq!(LogicalNode::parse(&record[..len]), None, "{len}");
        }
        let mut bad_option = record.clone();
        bad_option[0] = 2;
        assert_eq!(LogicalNode::parse(&bad_option), None);
        let mut bad_tag = record.clone();
        let feature_at = 1 + 1 + 2 + 32 + 2 + 2 + 1;
        assert_eq!(bad_tag[feature_at], 1);
        bad_tag[feature_at] = 10;
        assert_eq!(LogicalNode::parse(&bad_tag), None);

        let mut padded_varint = vec![0, 0, 3, 0x80, 0x00];
        padded_varint.extend_from_slice(&[0; 2 * HASH_LEN]);
        assert_eq!(LogicalNode::parse(&padded_varint), None);
        let mut endless_varint = vec![0, 0, 3];
        endless_varint.extend_from_slice(&[0xFF; 11]);
        endless_varint.extend_from_slice(&[0; 2 * HASH_LEN]);
        assert_eq!(LogicalNode::parse(&endless_varint), None);
        assert_eq!(LogicalNode::parse(b"a flat record"), None);
    }

    #[test]
    fn the_record_length_is_a_sum_of_sizes() {
        let leaf = NodeSizes {
            value_len: 1,
            feature_payload_len: 0,
            children: [None, None],
        };
        assert_eq!(leaf.record_len(), 68);
        let full = NodeSizes {
            value_len: 10,
            feature_payload_len: 3,
            children: [Some((4, 0)), Some((32, 17))],
        };
        assert_eq!(
            full.record_len(),
            (1 + 40) + (1 + 68 + 17) + (1 + 3) + 64 + 10
        );
        assert_eq!(link_len(0, 0), 36);
    }
}
