//! The feature of a node and the aggregate of a link, as the bytes a tree
//! page holds (`docs/lignindb/tree-page.md`, TP12): the tag Merk itself
//! gives the variant, then the variant's numbers in the order Merk declares
//! them, each at its full width, little-endian. There is no varint, so the
//! length depends on the variant alone.
//!
//! A tree page stores these bytes and does not interpret them. This module
//! is their one writer and their one reader. A variant Merk gains is one
//! arm in each of the four functions, and the compiler names them.

use crate::{
    tree::AggregateData,
    Error,
    TreeFeatureType::{self, *},
};

/// The most bytes a feature or an aggregate takes: a tag, a count and a
/// 128-bit sum.
const MAX_LEN: usize = 25;

/// The bytes of one feature or aggregate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Tp12 {
    len: u8,
    bytes: [u8; MAX_LEN],
}

impl Tp12 {
    fn tagged(tag: u8) -> Self {
        let mut bytes = [0; MAX_LEN];
        bytes[0] = tag;
        Self { len: 1, bytes }
    }

    fn with(mut self, number: &[u8]) -> Self {
        let start = usize::from(self.len);
        self.bytes[start..start + number.len()].copy_from_slice(number);
        self.len += number.len() as u8;
        self
    }

    fn count(self, count: u64) -> Self {
        self.with(&count.to_le_bytes())
    }

    fn sum(self, sum: i64) -> Self {
        self.with(&sum.to_le_bytes())
    }

    fn big_sum(self, sum: i128) -> Self {
        self.with(&sum.to_le_bytes())
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// The numbers after a tag, read in order.
struct Numbers<'a> {
    rest: &'a [u8],
    what: &'static str,
}

impl Numbers<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (number, rest) = self
            .rest
            .split_first_chunk::<N>()
            .ok_or_else(|| damaged(self.what, "is cut short"))?;
        self.rest = rest;
        Ok(*number)
    }

    fn count(&mut self) -> Result<u64, Error> {
        self.take().map(u64::from_le_bytes)
    }

    fn sum(&mut self) -> Result<i64, Error> {
        self.take().map(i64::from_le_bytes)
    }

    fn big_sum(&mut self) -> Result<i128, Error> {
        self.take().map(i128::from_le_bytes)
    }
}

fn damaged(what: &'static str, how: &'static str) -> Error {
    Error::CorruptedData(format!("the {what} of a stored node {how}"))
}

/// Splits `bytes` into its tag and its numbers, hands them to `variant`,
/// and refuses bytes that are left over.
fn decode<T>(
    bytes: &[u8],
    what: &'static str,
    variant: impl FnOnce(u8, &mut Numbers<'_>) -> Result<Option<T>, Error>,
) -> Result<T, Error> {
    let (tag, rest) = bytes
        .split_first()
        .ok_or_else(|| damaged(what, "is empty"))?;
    let mut numbers = Numbers { rest, what };
    let decoded = variant(*tag, &mut numbers)?.ok_or_else(|| damaged(what, "has no known tag"))?;
    if !numbers.rest.is_empty() {
        return Err(damaged(what, "is too long"));
    }
    Ok(decoded)
}

/// The bytes a cell holds for `feature`.
pub(super) fn encode_feature(feature: &TreeFeatureType) -> Tp12 {
    match *feature {
        BasicMerkNode => Tp12::tagged(0),
        SummedMerkNode(sum) => Tp12::tagged(1).sum(sum),
        BigSummedMerkNode(sum) => Tp12::tagged(2).big_sum(sum),
        CountedMerkNode(count) => Tp12::tagged(3).count(count),
        CountedSummedMerkNode(count, sum) => Tp12::tagged(4).count(count).sum(sum),
        ProvableCountedMerkNode(count) => Tp12::tagged(5).count(count),
        ProvableCountedSummedMerkNode(count, sum) => Tp12::tagged(6).count(count).sum(sum),
        ProvableSummedMerkNode(sum) => Tp12::tagged(7).sum(sum),
        ProvableCountedAndProvableSummedMerkNode(count, sum) => {
            Tp12::tagged(8).count(count).sum(sum)
        }
        ProvableCountedAndProvableBigSummedMerkNode(count, sum) => {
            Tp12::tagged(9).count(count).big_sum(sum)
        }
        CountedBigSummedMerkNode(count, sum) => Tp12::tagged(10).count(count).big_sum(sum),
        ProvableBigSummedMerkNode(sum) => Tp12::tagged(11).big_sum(sum),
    }
}

/// The feature a cell holds as `bytes`.
pub(super) fn decode_feature(bytes: &[u8]) -> Result<TreeFeatureType, Error> {
    decode(bytes, "feature", |tag, numbers| {
        Ok(Some(match tag {
            0 => BasicMerkNode,
            1 => SummedMerkNode(numbers.sum()?),
            2 => BigSummedMerkNode(numbers.big_sum()?),
            3 => CountedMerkNode(numbers.count()?),
            4 => CountedSummedMerkNode(numbers.count()?, numbers.sum()?),
            5 => ProvableCountedMerkNode(numbers.count()?),
            6 => ProvableCountedSummedMerkNode(numbers.count()?, numbers.sum()?),
            7 => ProvableSummedMerkNode(numbers.sum()?),
            8 => ProvableCountedAndProvableSummedMerkNode(numbers.count()?, numbers.sum()?),
            9 => ProvableCountedAndProvableBigSummedMerkNode(numbers.count()?, numbers.big_sum()?),
            10 => CountedBigSummedMerkNode(numbers.count()?, numbers.big_sum()?),
            11 => ProvableBigSummedMerkNode(numbers.big_sum()?),
            _ => return Ok(None),
        }))
    })
}

/// The bytes a link holds for `aggregate`.
pub(super) fn encode_aggregate(aggregate: &AggregateData) -> Tp12 {
    use AggregateData::*;
    match *aggregate {
        NoAggregateData => Tp12::tagged(0),
        Sum(sum) => Tp12::tagged(1).sum(sum),
        BigSum(sum) => Tp12::tagged(2).big_sum(sum),
        Count(count) => Tp12::tagged(3).count(count),
        CountAndSum(count, sum) => Tp12::tagged(4).count(count).sum(sum),
        ProvableCount(count) => Tp12::tagged(5).count(count),
        ProvableCountAndSum(count, sum) => Tp12::tagged(6).count(count).sum(sum),
        ProvableSum(sum) => Tp12::tagged(7).sum(sum),
        ProvableCountAndProvableSum(count, sum) => Tp12::tagged(8).count(count).sum(sum),
        ProvableCountAndProvableBigSum(count, sum) => Tp12::tagged(9).count(count).big_sum(sum),
        CountAndBigSum(count, sum) => Tp12::tagged(10).count(count).big_sum(sum),
        ProvableBigSum(sum) => Tp12::tagged(11).big_sum(sum),
    }
}

/// The aggregate a link holds as `bytes`.
pub(super) fn decode_aggregate(bytes: &[u8]) -> Result<AggregateData, Error> {
    use AggregateData::*;
    decode(bytes, "aggregate", |tag, numbers| {
        Ok(Some(match tag {
            0 => NoAggregateData,
            1 => Sum(numbers.sum()?),
            2 => BigSum(numbers.big_sum()?),
            3 => Count(numbers.count()?),
            4 => CountAndSum(numbers.count()?, numbers.sum()?),
            5 => ProvableCount(numbers.count()?),
            6 => ProvableCountAndSum(numbers.count()?, numbers.sum()?),
            7 => ProvableSum(numbers.sum()?),
            8 => ProvableCountAndProvableSum(numbers.count()?, numbers.sum()?),
            9 => ProvableCountAndProvableBigSum(numbers.count()?, numbers.big_sum()?),
            10 => CountAndBigSum(numbers.count()?, numbers.big_sum()?),
            11 => ProvableBigSum(numbers.big_sum()?),
            _ => return Ok(None),
        }))
    })
}

#[cfg(test)]
mod tests {
    use ed::Encode;

    use super::*;
    use crate::Link;

    const COUNTS: [u64; 4] = [0, 1, 0x0102_0304_0506_0708, u64::MAX];
    const SUMS: [i64; 5] = [0, -1, 0x0102_0304_0506_0708, i64::MAX, i64::MIN];
    const BIG_SUMS: [i128; 4] = [0, -1, i128::MAX, i128::MIN];

    fn pairs<S: Copy>(sums: &[S]) -> Vec<(u64, S)> {
        COUNTS
            .iter()
            .flat_map(|count| sums.iter().map(move |sum| (*count, *sum)))
            .collect()
    }

    /// Every variant, with numbers at the ends of their ranges. The match
    /// names each kind, so a kind Merk gains does not compile until it is
    /// given values here.
    fn features() -> Vec<TreeFeatureType> {
        let mut all = Vec::new();
        let mut kind = Some(BasicMerkNode);
        while let Some(this) = kind {
            let (values, next): (Vec<TreeFeatureType>, _) = match this {
                BasicMerkNode => (vec![BasicMerkNode], Some(SummedMerkNode(0))),
                SummedMerkNode(_) => (
                    SUMS.map(SummedMerkNode).to_vec(),
                    Some(BigSummedMerkNode(0)),
                ),
                BigSummedMerkNode(_) => (
                    BIG_SUMS.map(BigSummedMerkNode).to_vec(),
                    Some(CountedMerkNode(0)),
                ),
                CountedMerkNode(_) => (
                    COUNTS.map(CountedMerkNode).to_vec(),
                    Some(CountedSummedMerkNode(0, 0)),
                ),
                CountedSummedMerkNode(..) => (
                    (pairs(&SUMS).into_iter())
                        .map(|(count, sum)| CountedSummedMerkNode(count, sum))
                        .collect(),
                    Some(ProvableCountedMerkNode(0)),
                ),
                ProvableCountedMerkNode(_) => (
                    COUNTS.map(ProvableCountedMerkNode).to_vec(),
                    Some(ProvableCountedSummedMerkNode(0, 0)),
                ),
                ProvableCountedSummedMerkNode(..) => (
                    (pairs(&SUMS).into_iter())
                        .map(|(count, sum)| ProvableCountedSummedMerkNode(count, sum))
                        .collect(),
                    Some(ProvableSummedMerkNode(0)),
                ),
                ProvableSummedMerkNode(_) => (
                    SUMS.map(ProvableSummedMerkNode).to_vec(),
                    Some(ProvableCountedAndProvableSummedMerkNode(0, 0)),
                ),
                ProvableCountedAndProvableSummedMerkNode(..) => (
                    (pairs(&SUMS).into_iter())
                        .map(|(count, sum)| ProvableCountedAndProvableSummedMerkNode(count, sum))
                        .collect(),
                    Some(ProvableCountedAndProvableBigSummedMerkNode(0, 0)),
                ),
                ProvableCountedAndProvableBigSummedMerkNode(..) => (
                    (pairs(&BIG_SUMS).into_iter())
                        .map(|(count, sum)| ProvableCountedAndProvableBigSummedMerkNode(count, sum))
                        .collect(),
                    Some(CountedBigSummedMerkNode(0, 0)),
                ),
                CountedBigSummedMerkNode(..) => (
                    (pairs(&BIG_SUMS).into_iter())
                        .map(|(count, sum)| CountedBigSummedMerkNode(count, sum))
                        .collect(),
                    Some(ProvableBigSummedMerkNode(0)),
                ),
                ProvableBigSummedMerkNode(_) => {
                    (BIG_SUMS.map(ProvableBigSummedMerkNode).to_vec(), None)
                }
            };
            all.extend(values);
            kind = next;
        }
        all
    }

    /// The tag Merk's own encoding gives the aggregate of a link: the byte
    /// after the key, the hash and the two heights.
    fn merk_aggregate_tag(aggregate: AggregateData) -> u8 {
        let link = Link::Reference {
            hash: [0; 32],
            child_heights: (0, 0),
            key: vec![],
            aggregate_data: aggregate,
        };
        link.encode().unwrap()[1 + 32 + 2]
    }

    #[test]
    fn a_feature_and_its_aggregate_are_stored_under_merks_tag_at_full_width() {
        let features = features();
        assert_eq!(
            features.len(),
            1 + 5 + 4 + 4 + 20 + 4 + 20 + 5 + 20 + 16 + 16 + 4
        );
        for feature in features {
            let aggregate = AggregateData::from(feature);
            let stored = encode_feature(&feature);
            let linked = encode_aggregate(&aggregate);

            assert_eq!(stored.as_slice(), linked.as_slice(), "{feature:?}");
            assert_eq!(stored.as_slice()[0], feature.encode().unwrap()[0]);
            assert_eq!(linked.as_slice()[0], merk_aggregate_tag(aggregate));

            let numbers: usize = [feature.count().map(|_| 8), big_or_sum_width(&feature)]
                .into_iter()
                .flatten()
                .sum();
            assert_eq!(stored.as_slice().len(), 1 + numbers, "{feature:?}");

            assert_eq!(decode_feature(stored.as_slice()).unwrap(), feature);
            assert_eq!(decode_aggregate(linked.as_slice()).unwrap(), aggregate);
        }
    }

    fn big_or_sum_width(feature: &TreeFeatureType) -> Option<usize> {
        match feature {
            BasicMerkNode | CountedMerkNode(_) | ProvableCountedMerkNode(_) => None,
            SummedMerkNode(_)
            | CountedSummedMerkNode(..)
            | ProvableCountedSummedMerkNode(..)
            | ProvableSummedMerkNode(_)
            | ProvableCountedAndProvableSummedMerkNode(..) => Some(8),
            BigSummedMerkNode(_)
            | ProvableCountedAndProvableBigSummedMerkNode(..)
            | CountedBigSummedMerkNode(..)
            | ProvableBigSummedMerkNode(_) => Some(16),
        }
    }

    #[test]
    fn the_numbers_are_little_endian_in_the_order_merk_declares_them() {
        let stored = encode_feature(&CountedBigSummedMerkNode(0x0102, -2));
        let mut expected = vec![10, 0x02, 0x01, 0, 0, 0, 0, 0, 0, 0xFE];
        expected.extend([0xFF; 15]);
        assert_eq!(stored.as_slice(), expected);

        let linked = encode_aggregate(&AggregateData::CountAndSum(3, 0x0405));
        assert_eq!(
            linked.as_slice(),
            [4, 3, 0, 0, 0, 0, 0, 0, 0, 0x05, 0x04, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn bytes_that_are_no_feature_are_refused() {
        let message = |error: Error| match error {
            Error::CorruptedData(message) => message,
            other => panic!("{other:?}"),
        };
        let whole = encode_feature(&CountedSummedMerkNode(7, 9));
        let bytes = whole.as_slice();

        assert!(
            message(decode_feature(&[]).unwrap_err()).contains("feature of a stored node is empty")
        );
        assert!(message(decode_aggregate(&[]).unwrap_err()).contains("aggregate"));
        for cut in 1..bytes.len() {
            let error = message(decode_feature(&bytes[..cut]).unwrap_err());
            assert!(error.contains("is cut short"), "{error}");
            assert!(decode_aggregate(&bytes[..cut]).is_err());
        }
        let mut longer = bytes.to_vec();
        longer.push(0);
        assert!(message(decode_feature(&longer).unwrap_err()).contains("is too long"));
        assert!(message(decode_aggregate(&longer).unwrap_err()).contains("is too long"));
        assert!(message(decode_feature(&[12]).unwrap_err()).contains("has no known tag"));
        assert!(message(decode_aggregate(&[255]).unwrap_err()).contains("has no known tag"));
    }
}
