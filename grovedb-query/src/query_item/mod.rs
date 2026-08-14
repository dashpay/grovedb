/// Range set intersection and set-theoretic operations on query items.
pub mod intersect;
mod merge;

use std::{
    cmp,
    cmp::Ordering,
    fmt,
    hash::Hash,
    ops::{Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive},
};

use bincode::{enc::write::Writer, error::DecodeError, BorrowDecode, Decode, Encode};
#[cfg(feature = "blockchain")]
use grovedb_costs::{CostContext, CostsExt, OperationCost};
#[cfg(feature = "blockchain")]
use grovedb_storage::RawIterator;
#[cfg(feature = "serde")]
use serde::de::VariantAccess;
#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{error::Error, hex_to_ascii};

/// A `QueryItem` represents a key or a range of keys to be included in a proof.
///
/// This enum allows specifying different ways of selecting keys, including
/// exact matches, open-ended ranges, and boundary-based selections.
///
/// # Variants:
/// - `Key(Vec<u8>)` → A specific key.
/// - `Range(Range<Vec<u8>>)` → A range of keys (exclusive upper bound).
/// - `RangeInclusive(RangeInclusive<Vec<u8>>)` → A range of keys (inclusive
///   upper bound).
/// - `RangeFull(RangeFull)` → A full range, including all keys.
/// - `RangeFrom(RangeFrom<Vec<u8>>)` → A range starting from a key (inclusive).
/// - `RangeTo(RangeTo<Vec<u8>>)` → A range up to a key (exclusive).
/// - `RangeToInclusive(RangeToInclusive<Vec<u8>>)` → A range up to a key
///   (inclusive).
/// - `RangeAfter(RangeFrom<Vec<u8>>)` → A range starting after a key
///   (exclusive).
/// - `RangeAfterTo(Range<Vec<u8>>)` → A range between two keys, starting after
///   the lower bound.
/// - `RangeAfterToInclusive(RangeInclusive<Vec<u8>>)` → A range between two
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub enum QueryItem {
    /// A specific key to be included in the proof.
    Key(Vec<u8>),

    /// A range of keys, **excluding** the upper bound (`start..end`).
    Range(Range<Vec<u8>>),

    /// A range of keys, **including** the upper bound (`start..=end`).
    RangeInclusive(RangeInclusive<Vec<u8>>),

    /// Represents a **full range**, covering **all** possible keys.
    RangeFull(RangeFull),

    /// A range starting **from** a key, **inclusive** (`start..`).
    RangeFrom(RangeFrom<Vec<u8>>),

    /// A range **up to** a key, **exclusive** (`..end`).
    RangeTo(RangeTo<Vec<u8>>),

    /// A range **up to** a key, **inclusive** (`..=end`).
    RangeToInclusive(RangeToInclusive<Vec<u8>>),

    /// A range starting **after** a specific key, **exclusive** (`(key, ∞)`).
    RangeAfter(RangeFrom<Vec<u8>>),

    /// A range starting **after** a key and extending to another key,
    /// **exclusive**.
    RangeAfterTo(Range<Vec<u8>>),

    /// A range starting **after** a key and extending to another key,
    /// **inclusive**.
    RangeAfterToInclusive(RangeInclusive<Vec<u8>>),

    /// A count-only meta-query that wraps another `QueryItem` describing the
    /// range to count.
    ///
    /// When this variant appears in a `Query`, the query is interpreted as
    /// "return the **number of elements** matched by the inner range" instead
    /// of returning the elements themselves. The proof is shaped accordingly:
    /// boundary nodes are emitted as `KVDigestCount`, fully-inside subtree
    /// roots as `KVHashCount`, and fully-outside subtrees as opaque `Hash`.
    ///
    /// This variant is only valid against `ProvableCountTree` /
    /// `ProvableCountSumTree` (and their `NonCounted*` wrapper variants), and
    /// it must be the **only** item in the surrounding `Query` (no subqueries,
    /// no pagination, no other range items). The inner `QueryItem` may not be
    /// `Key`, `RangeFull`, or another `AggregateCountOnRange`.
    AggregateCountOnRange(Box<QueryItem>),

    /// A sum-only meta-query that wraps another `QueryItem` describing the
    /// range to sum.
    ///
    /// When this variant appears in a `Query`, the query is interpreted as
    /// "return the **total signed sum** of children with keys in the inner
    /// range" instead of returning the elements themselves. The proof is
    /// shaped accordingly: boundary nodes are emitted as `KVDigestSum`,
    /// fully-inside subtree roots as `HashWithSum`, and fully-outside subtree
    /// roots also as `HashWithSum` so the parent's `own_sum` derivation is
    /// hash-bound (same self-verifying compression pattern as count).
    ///
    /// This variant is only valid against `ProvableSumTree` (and its
    /// `NotSummed` wrapper variant), and it must be the **only** item in the
    /// surrounding `Query` (no subqueries, no pagination, no other range
    /// items). The inner `QueryItem` may not be `Key`, `RangeFull`, another
    /// `AggregateCountOnRange`, or another `AggregateSumOnRange`. Sum values
    /// are signed `i64`; the verifier uses an `i128` accumulator and narrows
    /// to `i64` at the end to detect overflow on adversarial inputs.
    AggregateSumOnRange(Box<QueryItem>),

    /// A combined-aggregate meta-query that wraps another `QueryItem`
    /// describing the range to aggregate over and returns BOTH a `u64`
    /// count AND a signed `i64` sum from a single proof.
    ///
    /// When this variant appears in a `Query`, the query is interpreted as
    /// "return the (count, sum) pair of children with keys in the inner
    /// range" in one proof. The proof shape is byte-identical to
    /// `AggregateCountOnRange` against a `ProvableCountProvableSumTree`
    /// host: emitter emits `HashWithCountAndSum(kv, l, r, count, sum)` for
    /// fully-inside / fully-outside collapsed subtrees and
    /// `KVDigestCountSum(key, value_hash, count, sum)` for boundary
    /// nodes — both already needed to reconstruct
    /// `node_hash_with_count_and_sum`. The verifier walks BOTH the count
    /// and sum axes in parallel against the same op stream.
    ///
    /// This variant is **only** valid against
    /// `ProvableCountProvableSumTree` (PCPS) — the single-axis hosts
    /// (`ProvableCountTree`, `ProvableSumTree`, `ProvableCountSumTree`)
    /// cannot host this query because their node hashes don't bind both
    /// aggregates. It must be the only item in the surrounding `Query`
    /// (no subqueries, no pagination, no other range items). The inner
    /// `QueryItem` may not be `Key`, `RangeFull`, or any aggregate
    /// variant (including itself).
    AggregateCountAndSumOnRange(Box<QueryItem>),
}

#[cfg(feature = "serde")]
impl Serialize for QueryItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Variant tags are emitted in snake_case so that textual formats
        // (JSON, YAML, TOML) round-trip through the matching Deserialize
        // impl, which uses `#[serde(field_identifier, rename_all =
        // "snake_case")]` on its `Field` enum. Binary formats that
        // identify variants by index (bincode, postcard) ignore the
        // string tag, so this change is transparent to them.
        match self {
            QueryItem::Key(key) => serializer.serialize_newtype_variant("QueryItem", 0, "key", key),
            QueryItem::Range(range) => {
                serializer.serialize_newtype_variant("QueryItem", 1, "range", &range)
            }
            QueryItem::RangeInclusive(range) => {
                serializer.serialize_newtype_variant("QueryItem", 2, "range_inclusive", range)
            }
            QueryItem::RangeFull(_) => {
                serializer.serialize_unit_variant("QueryItem", 3, "range_full")
            }
            QueryItem::RangeFrom(range_from) => {
                serializer.serialize_newtype_variant("QueryItem", 4, "range_from", range_from)
            }
            QueryItem::RangeTo(range_to) => {
                serializer.serialize_newtype_variant("QueryItem", 5, "range_to", range_to)
            }
            QueryItem::RangeToInclusive(range_to_inclusive) => serializer
                .serialize_newtype_variant(
                    "QueryItem",
                    6,
                    "range_to_inclusive",
                    &range_to_inclusive.end,
                ),
            QueryItem::RangeAfter(range_after) => {
                serializer.serialize_newtype_variant("QueryItem", 7, "range_after", range_after)
            }
            QueryItem::RangeAfterTo(range_after_to) => serializer.serialize_newtype_variant(
                "QueryItem",
                8,
                "range_after_to",
                range_after_to,
            ),
            QueryItem::RangeAfterToInclusive(range_after_to_inclusive) => serializer
                .serialize_newtype_variant(
                    "QueryItem",
                    9,
                    "range_after_to_inclusive",
                    range_after_to_inclusive,
                ),
            QueryItem::AggregateCountOnRange(inner) => serializer.serialize_newtype_variant(
                "QueryItem",
                10,
                "aggregate_count_on_range",
                inner,
            ),
            QueryItem::AggregateSumOnRange(inner) => serializer.serialize_newtype_variant(
                "QueryItem",
                11,
                "aggregate_sum_on_range",
                inner,
            ),
            QueryItem::AggregateCountAndSumOnRange(inner) => serializer.serialize_newtype_variant(
                "QueryItem",
                12,
                "aggregate_count_and_sum_on_range",
                inner,
            ),
        }
    }
}

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for QueryItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            Key,
            Range,
            RangeInclusive,
            RangeFull,
            RangeFrom,
            RangeTo,
            RangeToInclusive,
            RangeAfter,
            RangeAfterTo,
            RangeAfterToInclusive,
            AggregateCountOnRange,
            AggregateSumOnRange,
            AggregateCountAndSumOnRange,
        }

        struct QueryItemVisitor;

        impl<'de> serde::de::Visitor<'de> for QueryItemVisitor {
            type Value = QueryItem;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("enum QueryItem")
            }

            fn visit_enum<A>(self, data: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::EnumAccess<'de>,
            {
                let (variant, variant_access) = data.variant()?;

                match variant {
                    Field::Key => {
                        let key = variant_access.newtype_variant()?;
                        Ok(QueryItem::Key(key))
                    }
                    Field::Range => {
                        let range = variant_access.newtype_variant()?;
                        Ok(QueryItem::Range(range))
                    }
                    Field::RangeInclusive => {
                        let range_inclusive = variant_access.newtype_variant()?;
                        Ok(QueryItem::RangeInclusive(range_inclusive))
                    }
                    Field::RangeFull => Ok(QueryItem::RangeFull(RangeFull)),
                    Field::RangeFrom => {
                        let range_from = variant_access.newtype_variant()?;
                        Ok(QueryItem::RangeFrom(range_from))
                    }
                    Field::RangeTo => {
                        let range_to = variant_access.newtype_variant()?;
                        Ok(QueryItem::RangeTo(range_to))
                    }
                    Field::RangeToInclusive => {
                        // Deserialize the `Vec<u8>` for the `end` of the range
                        let end = variant_access.newtype_variant()?;
                        Ok(QueryItem::RangeToInclusive(..=end))
                    }
                    Field::RangeAfter => {
                        let range_after = variant_access.newtype_variant()?;
                        Ok(QueryItem::RangeAfter(range_after))
                    }
                    Field::RangeAfterTo => {
                        let range_after_to = variant_access.newtype_variant()?;
                        Ok(QueryItem::RangeAfterTo(range_after_to))
                    }
                    Field::RangeAfterToInclusive => {
                        let range_after_to_inclusive = variant_access.newtype_variant()?;
                        Ok(QueryItem::RangeAfterToInclusive(range_after_to_inclusive))
                    }
                    Field::AggregateCountOnRange => {
                        // Deserialize the inner via a wrapper that rejects
                        // the `AggregateCountOnRange` tag *before* recursing.
                        // This is the serde counterpart to the bincode
                        // depth-bounded decode + nested-rejection added in
                        // `Self::decode_with_depth`. Without it, a
                        // `serde`-feature client could send arbitrarily
                        // deep nested AggregateCountOnRange payloads and
                        // exhaust the stack inside `QueryItem::deserialize`
                        // before any validation runs.
                        let NonAggregateInner(inner) = variant_access.newtype_variant()?;
                        Ok(QueryItem::AggregateCountOnRange(Box::new(inner)))
                    }
                    Field::AggregateSumOnRange => {
                        // Same defense-in-depth as AggregateCountOnRange: the
                        // inner is deserialized through `NonAggregateInner`
                        // whose field set excludes both aggregate-variant
                        // tags, so any nested aggregate payload is rejected
                        // immediately by serde without recursing through
                        // `QueryItem::deserialize`. Keeps `AggregateSumOnRange`
                        // and `AggregateCountOnRange` orthogonal — one cannot
                        // wrap the other.
                        let NonAggregateInner(inner) = variant_access.newtype_variant()?;
                        Ok(QueryItem::AggregateSumOnRange(Box::new(inner)))
                    }
                    Field::AggregateCountAndSumOnRange => {
                        // Same defense-in-depth as the single-axis aggregate
                        // variants: the inner is deserialized through
                        // `NonAggregateInner`, whose field set excludes ALL
                        // three aggregate-variant tags, so any nested
                        // aggregate payload is rejected immediately by serde
                        // without recursing through `QueryItem::deserialize`.
                        // Keeps `AggregateCountAndSumOnRange` orthogonal to
                        // both `AggregateCountOnRange` and
                        // `AggregateSumOnRange` — none can wrap the other.
                        let NonAggregateInner(inner) = variant_access.newtype_variant()?;
                        Ok(QueryItem::AggregateCountAndSumOnRange(Box::new(inner)))
                    }
                }
            }
        }

        const VARIANTS: &[&str] = &[
            "Key",
            "Range",
            "RangeInclusive",
            "RangeFull",
            "RangeFrom",
            "RangeTo",
            "RangeToInclusive",
            "RangeAfter",
            "RangeAfterTo",
            "RangeAfterToInclusive",
            "AggregateCountOnRange",
            "AggregateSumOnRange",
            "AggregateCountAndSumOnRange",
        ];

        deserializer.deserialize_enum("QueryItem", VARIANTS, QueryItemVisitor)
    }
}

/// Newtype wrapper used internally by the serde `Deserialize` impl when
/// deserializing the *inner* item of an `AggregateCountOnRange` or
/// `AggregateSumOnRange`. The wrapper's `Deserialize` impl mirrors
/// `QueryItem::deserialize` but rejects both aggregate field tags immediately
/// — without recursing — so nested aggregate payloads cannot exhaust the
/// stack via repeated variant-10/11 recursion through
/// `QueryItem::deserialize`. Reused for both aggregate wrappers so
/// `AggregateCountOnRange` and `AggregateSumOnRange` stay orthogonal:
/// neither can wrap the other (or itself).
///
/// Defense-in-depth: nested aggregate variants are also rejected by
/// `Query::validate_aggregate_count_on_range` /
/// `Query::validate_aggregate_sum_on_range`, but enforcing it at decode time
/// matches the bincode side and prevents the DoS class on its own.
#[cfg(feature = "serde")]
struct NonAggregateInner(QueryItem);

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for NonAggregateInner {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Field set excludes all three aggregate variants
        // (`AggregateCountOnRange`, `AggregateSumOnRange`,
        // `AggregateCountAndSumOnRange`); encountering any tag produces a
        // serde "unknown variant" error before any inner recursion can
        // happen.
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            Key,
            Range,
            RangeInclusive,
            RangeFull,
            RangeFrom,
            RangeTo,
            RangeToInclusive,
            RangeAfter,
            RangeAfterTo,
            RangeAfterToInclusive,
        }

        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = NonAggregateInner;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("non-aggregate QueryItem variant")
            }

            fn visit_enum<A>(self, data: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::EnumAccess<'de>,
            {
                let (variant, va) = data.variant()?;
                let inner = match variant {
                    Field::Key => QueryItem::Key(va.newtype_variant()?),
                    Field::Range => QueryItem::Range(va.newtype_variant()?),
                    Field::RangeInclusive => QueryItem::RangeInclusive(va.newtype_variant()?),
                    Field::RangeFull => {
                        va.unit_variant()?;
                        QueryItem::RangeFull(RangeFull)
                    }
                    Field::RangeFrom => QueryItem::RangeFrom(va.newtype_variant()?),
                    Field::RangeTo => QueryItem::RangeTo(va.newtype_variant()?),
                    Field::RangeToInclusive => {
                        let end: Vec<u8> = va.newtype_variant()?;
                        QueryItem::RangeToInclusive(..=end)
                    }
                    Field::RangeAfter => QueryItem::RangeAfter(va.newtype_variant()?),
                    Field::RangeAfterTo => QueryItem::RangeAfterTo(va.newtype_variant()?),
                    Field::RangeAfterToInclusive => {
                        QueryItem::RangeAfterToInclusive(va.newtype_variant()?)
                    }
                };
                Ok(NonAggregateInner(inner))
            }
        }

        // The list excludes "AggregateCountOnRange" so a serde format that
        // surfaces unknown variants by name (most do) gives a precise error
        // for the nested case.
        const NON_AGGREGATE_VARIANTS: &[&str] = &[
            "Key",
            "Range",
            "RangeInclusive",
            "RangeFull",
            "RangeFrom",
            "RangeTo",
            "RangeToInclusive",
            "RangeAfter",
            "RangeAfterTo",
            "RangeAfterToInclusive",
        ];

        deserializer.deserialize_enum("QueryItem", NON_AGGREGATE_VARIANTS, V)
    }
}

impl Encode for QueryItem {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), bincode::error::EncodeError> {
        match self {
            QueryItem::Key(key) => {
                encoder.writer().write(&[0])?;
                key.encode(encoder)
            }
            QueryItem::Range(range) => {
                encoder.writer().write(&[1])?;
                range.start.encode(encoder)?;
                range.end.encode(encoder)
            }
            QueryItem::RangeInclusive(range) => {
                encoder.writer().write(&[2])?;
                range.start().encode(encoder)?;
                range.end().encode(encoder)
            }
            QueryItem::RangeFull(_) => {
                encoder.writer().write(&[3])?;
                Ok(())
            }
            QueryItem::RangeFrom(range) => {
                encoder.writer().write(&[4])?;
                range.start.encode(encoder)
            }
            QueryItem::RangeTo(range) => {
                encoder.writer().write(&[5])?;
                range.end.encode(encoder)
            }
            QueryItem::RangeToInclusive(range) => {
                encoder.writer().write(&[6])?;
                range.end.encode(encoder)
            }
            QueryItem::RangeAfter(range) => {
                encoder.writer().write(&[7])?;
                range.start.encode(encoder)
            }
            QueryItem::RangeAfterTo(range) => {
                encoder.writer().write(&[8])?;
                range.start.encode(encoder)?;
                range.end.encode(encoder)
            }
            QueryItem::RangeAfterToInclusive(range) => {
                encoder.writer().write(&[9])?;
                range.start().encode(encoder)?;
                range.end().encode(encoder)
            }
            QueryItem::AggregateCountOnRange(inner) => {
                encoder.writer().write(&[10])?;
                inner.as_ref().encode(encoder)
            }
            QueryItem::AggregateSumOnRange(inner) => {
                encoder.writer().write(&[11])?;
                inner.as_ref().encode(encoder)
            }
            QueryItem::AggregateCountAndSumOnRange(inner) => {
                encoder.writer().write(&[12])?;
                inner.as_ref().encode(encoder)
            }
        }
    }
}

/// Maximum recursion depth allowed when decoding a `QueryItem` from bincode.
///
/// The only recursive variant today is `AggregateCountOnRange(Box<QueryItem>)`
/// (variant 10). A malicious payload made of repeated variant-10 bytes
/// would otherwise recurse arbitrarily deep before any validation runs and
/// can stack-overflow the decoder. Since nested `AggregateCountOnRange` is
/// always rejected by `Query::validate_aggregate_count_on_range` anyway,
/// the only legal nesting depth here is **one** (the outer wrapper plus its
/// non-aggregate inner range). We keep a small safety margin.
pub(crate) const MAX_QUERY_ITEM_DECODE_DEPTH: usize = 4;

impl<Context> Decode<Context> for QueryItem {
    fn decode<D: bincode::de::Decoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode_with_depth(decoder, 0)
    }
}

impl QueryItem {
    /// Recursive bincode decode with an explicit depth counter. Used to bound
    /// nested `AggregateCountOnRange` payloads (which would otherwise allow
    /// stack exhaustion via repeated variant-10 bytes).
    pub(crate) fn decode_with_depth<D: bincode::de::Decoder>(
        decoder: &mut D,
        depth: usize,
    ) -> Result<Self, DecodeError> {
        if depth > MAX_QUERY_ITEM_DECODE_DEPTH {
            return Err(DecodeError::Other(
                "QueryItem nesting depth exceeded maximum during deserialization",
            ));
        }
        let variant_id = u8::decode(decoder)?;

        match variant_id {
            0 => {
                let key = Vec::<u8>::decode(decoder)?;
                Ok(QueryItem::Key(key))
            }
            1 => {
                let start = Vec::<u8>::decode(decoder)?;
                let end = Vec::<u8>::decode(decoder)?;
                Ok(QueryItem::Range(start..end))
            }
            2 => {
                let start = Vec::<u8>::decode(decoder)?;
                let end = Vec::<u8>::decode(decoder)?;
                Ok(QueryItem::RangeInclusive(start..=end))
            }
            3 => Ok(QueryItem::RangeFull(RangeFull)),
            4 => {
                let start = Vec::<u8>::decode(decoder)?;
                Ok(QueryItem::RangeFrom(start..))
            }
            5 => {
                let end = Vec::<u8>::decode(decoder)?;
                Ok(QueryItem::RangeTo(..end))
            }
            6 => {
                let end = Vec::<u8>::decode(decoder)?;
                Ok(QueryItem::RangeToInclusive(..=end))
            }
            7 => {
                let start = Vec::<u8>::decode(decoder)?;
                Ok(QueryItem::RangeAfter(start..))
            }
            8 => {
                let start = Vec::<u8>::decode(decoder)?;
                let end = Vec::<u8>::decode(decoder)?;
                Ok(QueryItem::RangeAfterTo(start..end))
            }
            9 => {
                let start = Vec::<u8>::decode(decoder)?;
                let end = Vec::<u8>::decode(decoder)?;
                Ok(QueryItem::RangeAfterToInclusive(start..=end))
            }
            10 => {
                let inner = QueryItem::decode_with_depth(decoder, depth + 1)?;
                // Defense-in-depth: nested AggregateCountOnRange is invalid
                // by validation rules, so we also reject it at decode time.
                // The depth guard above remains the primary stack-overflow
                // mitigation for malicious deeper nesting. Also reject
                // `AggregateSumOnRange` and `AggregateCountAndSumOnRange`
                // to keep the three aggregate variants orthogonal.
                if matches!(
                    inner,
                    QueryItem::AggregateCountOnRange(_)
                        | QueryItem::AggregateSumOnRange(_)
                        | QueryItem::AggregateCountAndSumOnRange(_)
                ) {
                    return Err(DecodeError::Other(
                        "AggregateCountOnRange must not wrap another aggregate variant",
                    ));
                }
                Ok(QueryItem::AggregateCountOnRange(Box::new(inner)))
            }
            11 => {
                let inner = QueryItem::decode_with_depth(decoder, depth + 1)?;
                // Same defense-in-depth as variant 10. `AggregateSumOnRange`
                // may not wrap another aggregate variant (whether sum,
                // count, or count+sum) — keeps the three orthogonal and
                // the depth guard primary mitigation against
                // stack-exhaustion.
                if matches!(
                    inner,
                    QueryItem::AggregateSumOnRange(_)
                        | QueryItem::AggregateCountOnRange(_)
                        | QueryItem::AggregateCountAndSumOnRange(_)
                ) {
                    return Err(DecodeError::Other(
                        "AggregateSumOnRange must not wrap another aggregate variant",
                    ));
                }
                Ok(QueryItem::AggregateSumOnRange(Box::new(inner)))
            }
            12 => {
                let inner = QueryItem::decode_with_depth(decoder, depth + 1)?;
                // Same defense-in-depth as variants 10 and 11.
                // `AggregateCountAndSumOnRange` may not wrap any
                // aggregate variant (including itself) — keeps the three
                // orthogonal and the depth guard primary mitigation
                // against stack-exhaustion.
                if matches!(
                    inner,
                    QueryItem::AggregateCountAndSumOnRange(_)
                        | QueryItem::AggregateCountOnRange(_)
                        | QueryItem::AggregateSumOnRange(_)
                ) {
                    return Err(DecodeError::Other(
                        "AggregateCountAndSumOnRange must not wrap another aggregate variant",
                    ));
                }
                Ok(QueryItem::AggregateCountAndSumOnRange(Box::new(inner)))
            }
            _ => Err(DecodeError::UnexpectedVariant {
                type_name: "QueryItem",
                allowed: &bincode::error::AllowedEnumVariants::Range { min: 0, max: 12 },
                found: variant_id as u32,
            }),
        }
    }
}

impl<'de, Context> BorrowDecode<'de, Context> for QueryItem {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::borrow_decode_with_depth(decoder, 0)
    }
}

impl QueryItem {
    /// Recursive bincode borrow-decode with an explicit depth counter.
    /// Mirrors [`Self::decode_with_depth`] for the borrowed-decoder path; same
    /// `MAX_QUERY_ITEM_DECODE_DEPTH` and same nested-`AggregateCountOnRange`
    /// rejection apply.
    pub(crate) fn borrow_decode_with_depth<'de, D: bincode::de::BorrowDecoder<'de>>(
        decoder: &mut D,
        depth: usize,
    ) -> Result<Self, DecodeError> {
        if depth > MAX_QUERY_ITEM_DECODE_DEPTH {
            return Err(DecodeError::Other(
                "QueryItem nesting depth exceeded maximum during deserialization",
            ));
        }
        let variant_id = u8::decode(decoder)?;

        match variant_id {
            0 => {
                let key = Vec::<u8>::borrow_decode(decoder)?;
                Ok(QueryItem::Key(key))
            }
            1 => {
                let start = Vec::<u8>::borrow_decode(decoder)?;
                let end = Vec::<u8>::borrow_decode(decoder)?;
                Ok(QueryItem::Range(start..end))
            }
            2 => {
                let start = Vec::<u8>::borrow_decode(decoder)?;
                let end = Vec::<u8>::borrow_decode(decoder)?;
                Ok(QueryItem::RangeInclusive(start..=end))
            }
            3 => Ok(QueryItem::RangeFull(RangeFull)),
            4 => {
                let start = Vec::<u8>::borrow_decode(decoder)?;
                Ok(QueryItem::RangeFrom(start..))
            }
            5 => {
                let end = Vec::<u8>::borrow_decode(decoder)?;
                Ok(QueryItem::RangeTo(..end))
            }
            6 => {
                let end = Vec::<u8>::borrow_decode(decoder)?;
                Ok(QueryItem::RangeToInclusive(..=end))
            }
            7 => {
                let start = Vec::<u8>::borrow_decode(decoder)?;
                Ok(QueryItem::RangeAfter(start..))
            }
            8 => {
                let start = Vec::<u8>::borrow_decode(decoder)?;
                let end = Vec::<u8>::borrow_decode(decoder)?;
                Ok(QueryItem::RangeAfterTo(start..end))
            }
            9 => {
                let start = Vec::<u8>::borrow_decode(decoder)?;
                let end = Vec::<u8>::borrow_decode(decoder)?;
                Ok(QueryItem::RangeAfterToInclusive(start..=end))
            }
            10 => {
                let inner = QueryItem::borrow_decode_with_depth(decoder, depth + 1)?;
                if matches!(
                    inner,
                    QueryItem::AggregateCountOnRange(_)
                        | QueryItem::AggregateSumOnRange(_)
                        | QueryItem::AggregateCountAndSumOnRange(_)
                ) {
                    return Err(DecodeError::Other(
                        "AggregateCountOnRange must not wrap another aggregate variant",
                    ));
                }
                Ok(QueryItem::AggregateCountOnRange(Box::new(inner)))
            }
            11 => {
                let inner = QueryItem::borrow_decode_with_depth(decoder, depth + 1)?;
                if matches!(
                    inner,
                    QueryItem::AggregateSumOnRange(_)
                        | QueryItem::AggregateCountOnRange(_)
                        | QueryItem::AggregateCountAndSumOnRange(_)
                ) {
                    return Err(DecodeError::Other(
                        "AggregateSumOnRange must not wrap another aggregate variant",
                    ));
                }
                Ok(QueryItem::AggregateSumOnRange(Box::new(inner)))
            }
            12 => {
                let inner = QueryItem::borrow_decode_with_depth(decoder, depth + 1)?;
                if matches!(
                    inner,
                    QueryItem::AggregateCountAndSumOnRange(_)
                        | QueryItem::AggregateCountOnRange(_)
                        | QueryItem::AggregateSumOnRange(_)
                ) {
                    return Err(DecodeError::Other(
                        "AggregateCountAndSumOnRange must not wrap another aggregate variant",
                    ));
                }
                Ok(QueryItem::AggregateCountAndSumOnRange(Box::new(inner)))
            }
            _ => Err(DecodeError::UnexpectedVariant {
                type_name: "QueryItem",
                allowed: &bincode::error::AllowedEnumVariants::Range { min: 0, max: 12 },
                found: variant_id as u32,
            }),
        }
    }
}

impl fmt::Display for QueryItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryItem::Key(key) => write!(f, "Key({})", hex_to_ascii(key)),
            QueryItem::Range(range) => write!(
                f,
                "Range({} .. {})",
                hex_to_ascii(&range.start),
                hex_to_ascii(&range.end)
            ),
            QueryItem::RangeInclusive(range) => write!(
                f,
                "RangeInclusive({} ..= {})",
                hex_to_ascii(range.start()),
                hex_to_ascii(range.end())
            ),
            QueryItem::RangeFull(_) => write!(f, "RangeFull"),
            QueryItem::RangeFrom(range) => {
                write!(f, "RangeFrom({} ..)", hex_to_ascii(&range.start))
            }
            QueryItem::RangeTo(range) => write!(f, "RangeTo(.. {})", hex_to_ascii(&range.end)),
            QueryItem::RangeToInclusive(range) => {
                write!(f, "RangeToInclusive(..= {})", hex_to_ascii(&range.end))
            }
            QueryItem::RangeAfter(range) => {
                write!(f, "RangeAfter({} <..)", hex_to_ascii(&range.start))
            }
            QueryItem::RangeAfterTo(range) => write!(
                f,
                "RangeAfterTo({} <.. {})",
                hex_to_ascii(&range.start),
                hex_to_ascii(&range.end)
            ),
            QueryItem::RangeAfterToInclusive(range) => write!(
                f,
                "RangeAfterToInclusive({} <..= {})",
                hex_to_ascii(range.start()),
                hex_to_ascii(range.end())
            ),
            QueryItem::AggregateCountOnRange(inner) => {
                write!(f, "AggregateCountOnRange({})", inner)
            }
            QueryItem::AggregateSumOnRange(inner) => {
                write!(f, "AggregateSumOnRange({})", inner)
            }
            QueryItem::AggregateCountAndSumOnRange(inner) => {
                write!(f, "AggregateCountAndSumOnRange({})", inner)
            }
        }
    }
}

impl QueryItem {
    /// Returns an approximate byte size of this query item for cost estimation.
    pub fn processing_footprint(&self) -> u32 {
        match self {
            QueryItem::Key(key) => key.len() as u32,
            QueryItem::RangeFull(_) => 0u32,
            QueryItem::AggregateCountOnRange(inner) => inner.processing_footprint(),
            QueryItem::AggregateSumOnRange(inner) => inner.processing_footprint(),
            QueryItem::AggregateCountAndSumOnRange(inner) => inner.processing_footprint(),
            _ => {
                self.lower_bound().0.map_or(0u32, |x| x.len() as u32)
                    + self.upper_bound().0.map_or(0u32, |x| x.len() as u32)
            }
        }
    }

    /// Returns the lower bound of this query item as `(bound, is_exclusive)`.
    /// `None` means the lower bound is unbounded.
    pub fn lower_bound(&self) -> (Option<&[u8]>, bool) {
        match self {
            QueryItem::Key(key) => (Some(key.as_slice()), false),
            QueryItem::Range(range) => (Some(range.start.as_ref()), false),
            QueryItem::RangeInclusive(range) => (Some(range.start().as_ref()), false),
            QueryItem::RangeFull(_) => (None, false),
            QueryItem::RangeFrom(range) => (Some(range.start.as_ref()), false),
            QueryItem::RangeTo(_) => (None, false),
            QueryItem::RangeToInclusive(_) => (None, false),
            QueryItem::RangeAfter(range) => (Some(range.start.as_ref()), true),
            QueryItem::RangeAfterTo(range) => (Some(range.start.as_ref()), true),
            QueryItem::RangeAfterToInclusive(range) => (Some(range.start().as_ref()), true),
            QueryItem::AggregateCountOnRange(inner) => inner.lower_bound(),
            QueryItem::AggregateSumOnRange(inner) => inner.lower_bound(),
            QueryItem::AggregateCountAndSumOnRange(inner) => inner.lower_bound(),
        }
    }

    /// Returns `true` if this query item has no lower bound (extends to -inf).
    pub fn lower_unbounded(&self) -> bool {
        match self {
            QueryItem::Key(_) => false,
            QueryItem::Range(_) => false,
            QueryItem::RangeInclusive(_) => false,
            QueryItem::RangeFull(_) => true,
            QueryItem::RangeFrom(_) => false,
            QueryItem::RangeTo(_) => true,
            QueryItem::RangeToInclusive(_) => true,
            QueryItem::RangeAfter(_) => false,
            QueryItem::RangeAfterTo(_) => false,
            QueryItem::RangeAfterToInclusive(_) => false,
            QueryItem::AggregateCountOnRange(inner) => inner.lower_unbounded(),
            QueryItem::AggregateSumOnRange(inner) => inner.lower_unbounded(),
            QueryItem::AggregateCountAndSumOnRange(inner) => inner.lower_unbounded(),
        }
    }

    /// Returns the upper bound of this query item as `(bound, is_inclusive)`.
    /// `None` means the upper bound is unbounded.
    pub fn upper_bound(&self) -> (Option<&[u8]>, bool) {
        match self {
            QueryItem::Key(key) => (Some(key.as_slice()), true),
            QueryItem::Range(range) => (Some(range.end.as_ref()), false),
            QueryItem::RangeInclusive(range) => (Some(range.end().as_ref()), true),
            QueryItem::RangeFull(_) => (None, true),
            QueryItem::RangeFrom(_) => (None, true),
            QueryItem::RangeTo(range) => (Some(range.end.as_ref()), false),
            QueryItem::RangeToInclusive(range) => (Some(range.end.as_ref()), true),
            QueryItem::RangeAfter(_) => (None, true),
            QueryItem::RangeAfterTo(range) => (Some(range.end.as_ref()), false),
            QueryItem::RangeAfterToInclusive(range) => (Some(range.end().as_ref()), true),
            QueryItem::AggregateCountOnRange(inner) => inner.upper_bound(),
            QueryItem::AggregateSumOnRange(inner) => inner.upper_bound(),
            QueryItem::AggregateCountAndSumOnRange(inner) => inner.upper_bound(),
        }
    }

    /// Returns `true` if this query item has no upper bound (extends to +inf).
    pub fn upper_unbounded(&self) -> bool {
        match self {
            QueryItem::Key(_) => false,
            QueryItem::Range(_) => false,
            QueryItem::RangeInclusive(_) => false,
            QueryItem::RangeFull(_) => true,
            QueryItem::RangeFrom(_) => true,
            QueryItem::RangeTo(_) => false,
            QueryItem::RangeToInclusive(_) => false,
            QueryItem::RangeAfter(_) => true,
            QueryItem::RangeAfterTo(_) => false,
            QueryItem::RangeAfterToInclusive(_) => false,
            QueryItem::AggregateCountOnRange(inner) => inner.upper_unbounded(),
            QueryItem::AggregateSumOnRange(inner) => inner.upper_unbounded(),
            QueryItem::AggregateCountAndSumOnRange(inner) => inner.upper_unbounded(),
        }
    }

    /// Returns `true` if the given key falls within this query item's bounds.
    pub fn contains(&self, key: &[u8]) -> bool {
        let (lower_bound, lower_bound_non_inclusive) = self.lower_bound();
        let (upper_bound, upper_bound_inclusive) = self.upper_bound();
        (self.lower_unbounded()
            || Some(key) > lower_bound
            || (Some(key) == lower_bound && !lower_bound_non_inclusive))
            && (self.upper_unbounded()
                || Some(key) < upper_bound
                || (Some(key) == upper_bound && upper_bound_inclusive))
    }

    /// Returns a numeric discriminant for this query item variant.
    pub fn enum_value(&self) -> u32 {
        match self {
            QueryItem::Key(_) => 0,
            QueryItem::Range(_) => 1,
            QueryItem::RangeInclusive(_) => 2,
            QueryItem::RangeFull(_) => 3,
            QueryItem::RangeFrom(_) => 4,
            QueryItem::RangeTo(_) => 5,
            QueryItem::RangeToInclusive(_) => 6,
            QueryItem::RangeAfter(_) => 7,
            QueryItem::RangeAfterTo(_) => 8,
            QueryItem::RangeAfterToInclusive(_) => 9,
            QueryItem::AggregateCountOnRange(_) => 10,
            QueryItem::AggregateSumOnRange(_) => 11,
            QueryItem::AggregateCountAndSumOnRange(_) => 12,
        }
    }

    /// Returns `true` if this query item is a single key lookup.
    pub const fn is_key(&self) -> bool {
        matches!(self, QueryItem::Key(_))
    }

    /// Returns `true` if this query item is any kind of range (not a single
    /// key). `AggregateCountOnRange`, `AggregateSumOnRange`, and
    /// `AggregateCountAndSumOnRange` count as ranges — they describe a
    /// range to aggregate over.
    pub const fn is_range(&self) -> bool {
        matches!(
            self,
            QueryItem::Range(_)
                | QueryItem::RangeInclusive(_)
                | QueryItem::RangeFull(_)
                | QueryItem::RangeFrom(_)
                | QueryItem::RangeTo(_)
                | QueryItem::RangeToInclusive(_)
                | QueryItem::RangeAfter(_)
                | QueryItem::RangeAfterTo(_)
                | QueryItem::RangeAfterToInclusive(_)
                | QueryItem::AggregateCountOnRange(_)
                | QueryItem::AggregateSumOnRange(_)
                | QueryItem::AggregateCountAndSumOnRange(_)
        )
    }

    /// Returns `true` if this query item matches exactly one key.
    pub const fn is_single(&self) -> bool {
        matches!(self, QueryItem::Key(_))
    }

    /// Returns `true` if this query item is a range with at least one unbounded
    /// end (e.g., `RangeFull`, `RangeFrom`, `RangeTo`, etc.). For
    /// `AggregateCountOnRange` and `AggregateSumOnRange`, delegates to the
    /// inner item.
    pub fn is_unbounded_range(&self) -> bool {
        match self {
            QueryItem::AggregateCountOnRange(inner) => inner.is_unbounded_range(),
            QueryItem::AggregateSumOnRange(inner) => inner.is_unbounded_range(),
            QueryItem::AggregateCountAndSumOnRange(inner) => inner.is_unbounded_range(),
            _ => !matches!(
                self,
                QueryItem::Key(_) | QueryItem::Range(_) | QueryItem::RangeInclusive(_)
            ),
        }
    }

    /// Returns `true` if this query item is any aggregate meta-variant
    /// (`AggregateCountOnRange`, `AggregateSumOnRange`, or
    /// `AggregateCountAndSumOnRange`).
    ///
    /// Aggregate meta-variants change the *interpretation* of the wrapped
    /// range (return a count/sum instead of the elements), so they must
    /// never be range-merged with other query items — merging would erase
    /// the wrapper and silently turn the query into a plain range query.
    pub const fn is_aggregate(&self) -> bool {
        matches!(
            self,
            QueryItem::AggregateCountOnRange(_)
                | QueryItem::AggregateSumOnRange(_)
                | QueryItem::AggregateCountAndSumOnRange(_)
        )
    }

    /// Returns `true` if this query item is the count-only meta-variant.
    pub const fn is_aggregate_count_on_range(&self) -> bool {
        matches!(self, QueryItem::AggregateCountOnRange(_))
    }

    /// Returns `true` if this query item is the sum-only meta-variant.
    pub const fn is_aggregate_sum_on_range(&self) -> bool {
        matches!(self, QueryItem::AggregateSumOnRange(_))
    }

    /// Returns `true` if this query item is the combined count+sum
    /// meta-variant.
    pub const fn is_aggregate_count_and_sum_on_range(&self) -> bool {
        matches!(self, QueryItem::AggregateCountAndSumOnRange(_))
    }

    /// If this is `AggregateCountOnRange`, returns a reference to the inner
    /// `QueryItem` describing the range to count. Otherwise returns `None`.
    pub fn aggregate_count_inner(&self) -> Option<&QueryItem> {
        match self {
            QueryItem::AggregateCountOnRange(inner) => Some(inner.as_ref()),
            _ => None,
        }
    }

    /// If this is `AggregateSumOnRange`, returns a reference to the inner
    /// `QueryItem` describing the range to sum. Otherwise returns `None`.
    pub fn aggregate_sum_inner(&self) -> Option<&QueryItem> {
        match self {
            QueryItem::AggregateSumOnRange(inner) => Some(inner.as_ref()),
            _ => None,
        }
    }

    /// If this is `AggregateCountAndSumOnRange`, returns a reference to
    /// the inner `QueryItem` describing the range to aggregate over.
    /// Otherwise returns `None`.
    pub fn aggregate_count_and_sum_inner(&self) -> Option<&QueryItem> {
        match self {
            QueryItem::AggregateCountAndSumOnRange(inner) => Some(inner.as_ref()),
            _ => None,
        }
    }

    /// Enumerates all distinct keys in this query item. Only works for `Key`,
    /// `Range`, and `RangeInclusive` with single-byte boundaries; returns an
    /// error for unbounded ranges.
    pub fn keys(&self) -> Result<Vec<Vec<u8>>, Error> {
        match self {
            QueryItem::Key(key) => Ok(vec![key.clone()]),
            QueryItem::Range(Range { start, end }) => {
                let mut keys = vec![];
                if start.len() > 1 || end.len() != 1 {
                    return Err(Error::InvalidOperation(
                        "distinct keys are not available for ranges using more or less than 1 byte",
                    ));
                }
                let start = *start.first().unwrap_or_else(|| {
                    keys.push(vec![]);
                    &0
                });
                if let Some(end) = end.first() {
                    let end = *end;
                    for i in start..end {
                        keys.push(vec![i]);
                    }
                }
                Ok(keys)
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                let start = range_inclusive.start();
                let end = range_inclusive.end();
                let mut keys = vec![];
                if start.len() > 1 || end.len() != 1 {
                    return Err(Error::InvalidOperation(
                        "distinct keys are not available for ranges using more or less than 1 byte",
                    ));
                }
                let start = *start.first().unwrap_or_else(|| {
                    keys.push(vec![]);
                    &0
                });
                if let Some(end) = end.first() {
                    let end = *end;
                    for i in start..=end {
                        keys.push(vec![i]);
                    }
                }
                Ok(keys)
            }
            _ => Err(Error::InvalidOperation(
                "distinct keys are not available for unbounded ranges",
            )),
        }
    }

    /// Like [`keys`](Self::keys) but consumes `self`, avoiding clones.
    pub fn keys_consume(self) -> Result<Vec<Vec<u8>>, Error> {
        match self {
            QueryItem::Key(key) => Ok(vec![key]),
            QueryItem::Range(Range { start, end }) => {
                let mut keys = vec![];
                if start.len() > 1 || end.len() != 1 {
                    return Err(Error::InvalidOperation(
                        "distinct keys are not available for ranges using more or less than 1 byte",
                    ));
                }
                let start = *start.first().unwrap_or_else(|| {
                    keys.push(vec![]);
                    &0
                });
                if let Some(end) = end.first() {
                    let end = *end;
                    for i in start..end {
                        keys.push(vec![i]);
                    }
                }
                Ok(keys)
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                let start = range_inclusive.start();
                let end = range_inclusive.end();
                let mut keys = vec![];
                if start.len() > 1 || end.len() != 1 {
                    return Err(Error::InvalidOperation(
                        "distinct keys are not available for ranges using more or less than 1 byte",
                    ));
                }
                let start = *start.first().unwrap_or_else(|| {
                    keys.push(vec![]);
                    &0
                });
                if let Some(end) = end.first() {
                    let end = *end;
                    for i in start..=end {
                        keys.push(vec![i]);
                    }
                }
                Ok(keys)
            }
            _ => Err(Error::InvalidOperation(
                "distinct keys are not available for unbounded ranges",
            )),
        }
    }

    /// Positions a storage iterator to the start of this query item, seeking
    /// forward or backward based on `left_to_right`.
    #[cfg(feature = "blockchain")]
    pub fn seek_for_iter<I: RawIterator>(
        &self,
        iter: &mut I,
        left_to_right: bool,
    ) -> CostContext<()> {
        match self {
            QueryItem::Key(start) => iter.seek(start),
            QueryItem::Range(Range { start, end }) => {
                if left_to_right {
                    iter.seek(start)
                } else {
                    iter.seek(end).flat_map(|_| iter.prev())
                }
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                if left_to_right {
                    iter.seek(range_inclusive.start())
                } else {
                    iter.seek_for_prev(range_inclusive.end())
                }
            }
            QueryItem::RangeFull(..) => {
                if left_to_right {
                    iter.seek_to_first()
                } else {
                    iter.seek_to_last()
                }
            }
            QueryItem::RangeFrom(RangeFrom { start }) => {
                if left_to_right {
                    iter.seek(start)
                } else {
                    iter.seek_to_last()
                }
            }
            QueryItem::RangeTo(RangeTo { end }) => {
                if left_to_right {
                    iter.seek_to_first()
                } else {
                    iter.seek(end).flat_map(|_| iter.prev())
                }
            }
            QueryItem::RangeToInclusive(RangeToInclusive { end }) => {
                if left_to_right {
                    iter.seek_to_first()
                } else {
                    iter.seek_for_prev(end)
                }
            }
            QueryItem::RangeAfter(RangeFrom { start }) => {
                if left_to_right {
                    let mut cost = OperationCost::default();
                    iter.seek(start).unwrap_add_cost(&mut cost);
                    if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
                        // if the key is the same as start we should go to next
                        if key == start {
                            iter.next().unwrap_add_cost(&mut cost)
                        }
                    }
                    ().wrap_with_cost(cost)
                } else {
                    iter.seek_to_last()
                }
            }
            QueryItem::RangeAfterTo(Range { start, end }) => {
                if left_to_right {
                    let mut cost = OperationCost::default();
                    iter.seek(start).unwrap_add_cost(&mut cost);
                    if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
                        // if the key is the same as start we тshould go to next
                        if key == start {
                            iter.next().unwrap_add_cost(&mut cost);
                        }
                    }
                    ().wrap_with_cost(cost)
                } else {
                    iter.seek(end).flat_map(|_| iter.prev())
                }
            }
            QueryItem::RangeAfterToInclusive(range_inclusive) => {
                if left_to_right {
                    let mut cost = OperationCost::default();
                    let start = range_inclusive.start();
                    iter.seek(start).unwrap_add_cost(&mut cost);
                    if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
                        // if the key is the same as start we тshould go to next
                        if key == start {
                            iter.next().unwrap_add_cost(&mut cost);
                        }
                    }
                    ().wrap_with_cost(cost)
                } else {
                    let end = range_inclusive.end();
                    iter.seek_for_prev(end)
                }
            }
            QueryItem::AggregateCountOnRange(inner) => inner.seek_for_iter(iter, left_to_right),
            QueryItem::AggregateSumOnRange(inner) => inner.seek_for_iter(iter, left_to_right),
            QueryItem::AggregateCountAndSumOnRange(inner) => {
                inner.seek_for_iter(iter, left_to_right)
            }
        }
    }

    /// Lexicographically compares two byte slices.
    pub fn compare(a: &[u8], b: &[u8]) -> cmp::Ordering {
        for (ai, bi) in a.iter().zip(b.iter()) {
            match ai.cmp(bi) {
                Ordering::Equal => continue,
                ord => return ord,
            }
        }

        // if every single element was equal, compare length
        a.len().cmp(&b.len())
    }

    /// Returns `true` if the iterator is currently positioned at a valid key
    /// within this query item's bounds, respecting direction and limit.
    #[cfg(feature = "blockchain")]
    pub fn iter_is_valid_for_type<I: RawIterator>(
        &self,
        iter: &I,
        limit: Option<u16>,
        aggregate_limit: Option<i64>,
        left_to_right: bool,
    ) -> CostContext<bool> {
        let mut cost = OperationCost::default();

        // Check that if limit is set it's greater than 0 and iterator points to a valid
        // place.
        let basic_valid = limit.map(|l| l > 0).unwrap_or(true)
            && aggregate_limit.map(|l| l > 0).unwrap_or(true)
            && iter.valid().unwrap_add_cost(&mut cost);

        if !basic_valid {
            return false.wrap_with_cost(cost);
        }

        // Key should also be something, otherwise terminate early.
        // Aggregate-wrapper variants delegate directly to their inner item.
        // Returning before the `iter.key()` read below avoids
        //   (a) double-charging the `iter.key()` cost when the inner item
        //       reads it again, and
        //   (b) duplicating the raw-iter read itself.
        // The depth-bounded decoder + nested-aggregate validators already
        // prevent wrapper-of-wrapper shapes, so recursion here is bounded.
        if let QueryItem::AggregateCountOnRange(inner)
        | QueryItem::AggregateSumOnRange(inner)
        | QueryItem::AggregateCountAndSumOnRange(inner) = self
        {
            return inner.iter_is_valid_for_type(iter, limit, aggregate_limit, left_to_right);
        }

        let key = if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
            key
        } else {
            return false.wrap_with_cost(cost);
        };

        let is_valid = match self {
            QueryItem::Key(start) => key == start,
            QueryItem::Range(Range { start, end }) => {
                if left_to_right {
                    key < end
                } else {
                    key >= start
                }
            }
            QueryItem::RangeInclusive(range_inclusive) => {
                if left_to_right {
                    key <= range_inclusive.end()
                } else {
                    key >= range_inclusive.start()
                }
            }
            QueryItem::RangeFull(..) => {
                true // requires only basic validation which is done above
            }
            QueryItem::RangeFrom(RangeFrom { start }) => left_to_right || key >= start,
            QueryItem::RangeTo(RangeTo { end }) => !left_to_right || key < end,
            QueryItem::RangeToInclusive(RangeToInclusive { end }) => !left_to_right || key <= end,
            QueryItem::RangeAfter(RangeFrom { start }) => left_to_right || key > start,
            QueryItem::RangeAfterTo(Range { start, end }) => {
                if left_to_right {
                    key < end
                } else {
                    key > start
                }
            }
            QueryItem::RangeAfterToInclusive(range_inclusive) => {
                if left_to_right {
                    let end = range_inclusive.end().as_slice();
                    match Self::compare(key, end) {
                        Ordering::Less => true,
                        Ordering::Equal => true,
                        Ordering::Greater => false,
                    }
                } else {
                    let start = range_inclusive.start().as_slice();
                    match Self::compare(key, start) {
                        Ordering::Less => false,
                        Ordering::Equal => false,
                        Ordering::Greater => true,
                    }
                }
            }
            QueryItem::AggregateCountOnRange(_)
            | QueryItem::AggregateSumOnRange(_)
            | QueryItem::AggregateCountAndSumOnRange(_) => {
                unreachable!("aggregate-wrapper variants short-circuit at the top of the function")
            }
        };

        is_valid.wrap_with_cost(cost)
    }

    /// Returns `true` if this query item overlaps with `other` in any way.
    pub fn collides_with(&self, other: &Self) -> bool {
        self.intersect(other).in_both.is_some()
    }
}

impl PartialEq<&[u8]> for QueryItem {
    fn eq(&self, other: &&[u8]) -> bool {
        matches!(self.partial_cmp(other), Some(Ordering::Equal))
    }
}

impl Ord for QueryItem {
    fn cmp(&self, other: &Self) -> Ordering {
        let self_as_range_set = self.to_range_set();
        let other_as_range_set = other.to_range_set();

        let compare_start = self_as_range_set.start.cmp(&other_as_range_set.start);

        // if start is equal then use the size of the set to compare
        // the smaller set is considered less
        if compare_start == Ordering::Equal {
            self_as_range_set.end.cmp(&other_as_range_set.end)
        } else {
            compare_start
        }
    }
}

impl PartialOrd for QueryItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialOrd<&[u8]> for QueryItem {
    fn partial_cmp(&self, other: &&[u8]) -> Option<Ordering> {
        let other = Self::Key(other.to_vec());
        Some(self.cmp(&other))
    }
}

impl From<Vec<u8>> for QueryItem {
    fn from(key: Vec<u8>) -> Self {
        Self::Key(key)
    }
}

#[cfg(test)]
mod test {
    use crate::query_item::QueryItem;

    #[test]
    fn query_item_collides() {
        assert!(!QueryItem::Key(vec![10]).collides_with(&QueryItem::Key(vec![20])));
        assert!(QueryItem::Key(vec![10]).collides_with(&QueryItem::Key(vec![10])));
        assert!(!QueryItem::Key(vec![20]).collides_with(&QueryItem::Key(vec![10])));

        assert!(!QueryItem::Key(vec![10]).collides_with(&QueryItem::Range(vec![20]..vec![30])));
        assert!(QueryItem::Key(vec![10]).collides_with(&QueryItem::Range(vec![10]..vec![20])));
        assert!(QueryItem::Key(vec![15]).collides_with(&QueryItem::Range(vec![10]..vec![20])));
        assert!(!QueryItem::Key(vec![20]).collides_with(&QueryItem::Range(vec![10]..vec![20])));
        assert!(
            QueryItem::Key(vec![20]).collides_with(&QueryItem::RangeInclusive(vec![10]..=vec![20]))
        );
        assert!(!QueryItem::Key(vec![30]).collides_with(&QueryItem::Range(vec![10]..vec![20])));

        assert!(!QueryItem::Range(vec![10]..vec![20])
            .collides_with(&QueryItem::Range(vec![30]..vec![40])));
        assert!(!QueryItem::Range(vec![10]..vec![20])
            .collides_with(&QueryItem::Range(vec![20]..vec![30])));
        assert!(QueryItem::RangeInclusive(vec![10]..=vec![20])
            .collides_with(&QueryItem::Range(vec![20]..vec![30])));
        assert!(QueryItem::Range(vec![15]..vec![25])
            .collides_with(&QueryItem::Range(vec![20]..vec![30])));
        assert!(!QueryItem::Range(vec![20]..vec![30])
            .collides_with(&QueryItem::Range(vec![10]..vec![20])));
        assert!(QueryItem::RangeFrom(vec![2]..).collides_with(&QueryItem::Key(vec![5])));
    }

    #[test]
    fn query_item_cmp() {
        assert!(QueryItem::Key(vec![10]) < QueryItem::Key(vec![20]));
        assert_eq!(QueryItem::Key(vec![10]), QueryItem::Key(vec![10]));
        assert!(QueryItem::Key(vec![20]) > QueryItem::Key(vec![10]));

        assert!(QueryItem::Key(vec![10]) < QueryItem::Range(vec![20]..vec![30]));
        assert_ne!(
            QueryItem::Key(vec![10]),
            QueryItem::Range(vec![10]..vec![20])
        );
        assert_ne!(
            QueryItem::Key(vec![15]),
            QueryItem::Range(vec![10]..vec![20])
        );
        assert!(QueryItem::Key(vec![20]) > QueryItem::Range(vec![10]..vec![20]));
        assert_ne!(
            QueryItem::Key(vec![20]),
            QueryItem::RangeInclusive(vec![10]..=vec![20])
        );
        assert!(QueryItem::Key(vec![30]) > QueryItem::Range(vec![10]..vec![20]));

        assert!(QueryItem::Range(vec![10]..vec![20]) < QueryItem::Range(vec![30]..vec![40]));
        assert!(QueryItem::Range(vec![10]..vec![20]) < QueryItem::Range(vec![20]..vec![30]));
        assert_ne!(
            QueryItem::RangeInclusive(vec![10]..=vec![20]),
            QueryItem::Range(vec![20]..vec![30])
        );
        assert_ne!(
            QueryItem::Range(vec![15]..vec![25]),
            QueryItem::Range(vec![20]..vec![30])
        );
        assert!(QueryItem::Range(vec![20]..vec![30]) > QueryItem::Range(vec![10]..vec![20]));
    }

    // ---------- decode-depth + nested-AggregateCountOnRange rejection ----------

    use super::MAX_QUERY_ITEM_DECODE_DEPTH;

    fn bincode_config() -> bincode::config::Configuration<
        bincode::config::BigEndian,
        bincode::config::Fixint,
        bincode::config::NoLimit,
    > {
        bincode::config::standard()
            .with_big_endian()
            .with_fixed_int_encoding()
            .with_no_limit()
    }

    #[test]
    fn decode_rejects_nested_aggregate_count_on_range() {
        // A two-level nest: AggregateCountOnRange(AggregateCountOnRange(Range)).
        let nested = QueryItem::AggregateCountOnRange(Box::new(QueryItem::AggregateCountOnRange(
            Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
        )));
        let bytes = bincode::encode_to_vec(&nested, bincode_config()).expect("encode succeeds");
        let result: Result<(QueryItem, _), _> =
            bincode::decode_from_slice(&bytes, bincode_config());
        let err = result.expect_err("nested AggregateCountOnRange must be rejected at decode");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("AggregateCountOnRange") || msg.contains("nesting depth"),
            "expected nested-rejection message, got: {msg}"
        );
    }

    #[test]
    fn decode_caps_depth_for_malicious_payload() {
        // Construct a raw byte payload of (MAX_QUERY_ITEM_DECODE_DEPTH + 2)
        // copies of the AggregateCountOnRange variant byte (10) followed by
        // a base item. This bypasses the constructor-level nested rejection
        // but should hit the depth guard. We use Range as the eventual base
        // (variants 0..=9 don't recurse). Since variant 10 reads the next
        // byte as a recursive QueryItem, repeated 10s recurse without
        // bound — exactly the stack-exhaustion case the depth guard
        // prevents.
        let depth_to_try = MAX_QUERY_ITEM_DECODE_DEPTH + 2;
        let mut payload: Vec<u8> = Vec::new();
        for _ in 0..depth_to_try {
            payload.push(10u8); // AggregateCountOnRange variant tag
        }
        // Innermost: Range(b"a", b"z"). Variant tag 1, then encoded start +
        // end Vec<u8>s in big-endian fixed-int config.
        payload.push(1u8);
        let inner = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
        let inner_bytes = bincode::encode_to_vec(&inner, bincode_config()).unwrap();
        // inner_bytes already starts with the variant tag (1), strip it.
        payload.extend_from_slice(&inner_bytes[1..]);

        let result: Result<(QueryItem, _), _> =
            bincode::decode_from_slice(&payload, bincode_config());
        let err = result.expect_err("payload exceeding max depth must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("nesting depth") || msg.contains("AggregateCountOnRange"),
            "expected depth-rejection message, got: {msg}"
        );
    }

    #[test]
    fn decode_accepts_valid_one_level_aggregate_count_on_range() {
        // Single-level wrap with a non-aggregate inner. This is the only
        // legal shape after validation; decoding must succeed.
        let q = QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let bytes = bincode::encode_to_vec(&q, bincode_config()).unwrap();
        let (decoded, _): (QueryItem, _) = bincode::decode_from_slice(&bytes, bincode_config())
            .expect("single-level wrap must decode");
        assert_eq!(q, decoded);
    }

    // ---------- serde-feature: nested AggregateCountOnRange rejection ----------
    //
    // The bincode path is depth-bounded above. Mirror the same defense for the
    // serde path so serde-feature clients can't bypass the protection — the
    // inner item is deserialized through `NonAggregateInner`, whose enum
    // field set excludes `AggregateCountOnRange`, so any nested payload is
    // rejected immediately by serde without recursion through
    // `QueryItem::deserialize`.
    //
    // We use `serde_test`'s token-level driver here for symmetry with the
    // rejection contract — the inner field set's snake_case tag (e.g.
    // `aggregate_count_on_range`) matches both the Serialize impl (which
    // emits snake_case variant tags) and the Deserialize impl
    // (`#[serde(field_identifier, rename_all = "snake_case")]`).

    #[cfg(feature = "serde")]
    #[test]
    fn serde_decode_rejects_nested_aggregate_count_on_range() {
        // Replay the token sequence for an outer AggregateCountOnRange whose
        // inner is itself an AggregateCountOnRange. The outer dispatch
        // selects the AggregateCountOnRange variant and tries to deserialize
        // the inner via `NonAggregateInner`, which does not list
        // `aggregate_count_on_range` in its field set — serde_test surfaces
        // this as an "unknown variant" error.
        use serde_test::{assert_de_tokens_error, Token};
        assert_de_tokens_error::<QueryItem>(
            &[
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_count_on_range",
                },
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_count_on_range",
                },
            ],
            // Exact wording comes from serde's `field_identifier`
            // dispatcher rejecting an out-of-set tag — the field set lives
            // in `NonAggregateInner`'s `Field` enum, which deliberately
            // omits `aggregate_count_on_range`.
            "unknown field `aggregate_count_on_range`, expected one of \
             `key`, `range`, `range_inclusive`, `range_full`, `range_from`, \
             `range_to`, `range_to_inclusive`, `range_after`, `range_after_to`, \
             `range_after_to_inclusive`",
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_decode_accepts_valid_one_level_aggregate_count_on_range() {
        // Outer `AggregateCountOnRange` wrapping a non-aggregate `Range`
        // succeeds: the inner dispatch goes through `NonAggregateInner`,
        // finds `range`, and the resulting Range is wrapped back up.
        use serde_test::{assert_de_tokens, Token};
        let expected = QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        assert_de_tokens(
            &expected,
            &[
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_count_on_range",
                },
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "range",
                },
                Token::Struct {
                    name: "Range",
                    len: 2,
                },
                Token::Str("start"),
                Token::Seq { len: Some(1) },
                Token::U8(b'a'),
                Token::SeqEnd,
                Token::Str("end"),
                Token::Seq { len: Some(1) },
                Token::U8(b'z'),
                Token::SeqEnd,
                Token::StructEnd,
            ],
        );
    }

    // ---------- AggregateSumOnRange: mirrors of the AggregateCountOnRange tests ----------
    //
    // These exist to drive coverage of the variant-11 dispatch in encode,
    // decode_with_depth, borrow_decode_with_depth, Display, and the helper
    // accessors. Each test targets a specific arm previously not exercised.

    #[test]
    fn decode_rejects_nested_aggregate_sum_on_range() {
        // AggregateSumOnRange(AggregateSumOnRange(Range)): the inner nested
        // aggregate must be rejected by the variant-11 dispatch's matches!
        // guard (or by the depth guard).
        let nested = QueryItem::AggregateSumOnRange(Box::new(QueryItem::AggregateSumOnRange(
            Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
        )));
        let bytes = bincode::encode_to_vec(&nested, bincode_config()).expect("encode succeeds");
        let result: Result<(QueryItem, _), _> =
            bincode::decode_from_slice(&bytes, bincode_config());
        let err = result.expect_err("nested AggregateSumOnRange must be rejected at decode");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("AggregateSumOnRange") || msg.contains("nesting depth"),
            "expected nested-rejection message, got: {msg}"
        );
    }

    #[test]
    fn decode_rejects_aggregate_sum_wrapping_aggregate_count() {
        // Orthogonality check: AggregateSumOnRange cannot wrap
        // AggregateCountOnRange (and vice versa) — this is the explicit
        // matches! guard in variant 11.
        let mixed = QueryItem::AggregateSumOnRange(Box::new(QueryItem::AggregateCountOnRange(
            Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
        )));
        let bytes = bincode::encode_to_vec(&mixed, bincode_config()).expect("encode succeeds");
        let result: Result<(QueryItem, _), _> =
            bincode::decode_from_slice(&bytes, bincode_config());
        let err = result
            .expect_err("AggregateSumOnRange wrapping AggregateCountOnRange must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("AggregateSumOnRange") || msg.contains("aggregate"),
            "expected nested-rejection message, got: {msg}"
        );
    }

    #[test]
    fn decode_rejects_aggregate_count_wrapping_aggregate_sum() {
        // The other direction: AggregateCountOnRange cannot wrap
        // AggregateSumOnRange — variant 10's matches! guard.
        let mixed = QueryItem::AggregateCountOnRange(Box::new(QueryItem::AggregateSumOnRange(
            Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
        )));
        let bytes = bincode::encode_to_vec(&mixed, bincode_config()).expect("encode succeeds");
        let result: Result<(QueryItem, _), _> =
            bincode::decode_from_slice(&bytes, bincode_config());
        let err = result
            .expect_err("AggregateCountOnRange wrapping AggregateSumOnRange must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("AggregateCountOnRange") || msg.contains("aggregate"),
            "expected nested-rejection message, got: {msg}"
        );
    }

    #[test]
    fn decode_accepts_valid_one_level_aggregate_sum_on_range() {
        let q = QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let bytes = bincode::encode_to_vec(&q, bincode_config()).unwrap();
        let (decoded, _): (QueryItem, _) = bincode::decode_from_slice(&bytes, bincode_config())
            .expect("single-level wrap must decode");
        assert_eq!(q, decoded);
    }

    #[test]
    fn decode_caps_depth_for_malicious_sum_payload() {
        // Mirrors the count payload depth test but with the variant-11
        // (AggregateSumOnRange) tag. Hits the depth guard inside
        // decode_with_depth on the sum branch.
        let depth_to_try = MAX_QUERY_ITEM_DECODE_DEPTH + 2;
        let mut payload: Vec<u8> = Vec::new();
        for _ in 0..depth_to_try {
            payload.push(11u8); // AggregateSumOnRange variant tag
        }
        // Innermost: Range. Variant tag 1, then start/end Vec<u8> bytes.
        payload.push(1u8);
        let inner = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
        let inner_bytes = bincode::encode_to_vec(&inner, bincode_config()).unwrap();
        payload.extend_from_slice(&inner_bytes[1..]);

        let result: Result<(QueryItem, _), _> =
            bincode::decode_from_slice(&payload, bincode_config());
        let err = result.expect_err("payload exceeding max depth must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("nesting depth") || msg.contains("AggregateSumOnRange"),
            "expected depth-rejection message, got: {msg}"
        );
    }

    #[test]
    fn decode_unknown_variant_rejected() {
        // Variant byte 13 is unknown (max = 12 after the combined
        // variant was added). Verifies the trailing UnexpectedVariant
        // arm in decode_with_depth.
        let payload = vec![13u8];
        let result: Result<(QueryItem, _), _> =
            bincode::decode_from_slice(&payload, bincode_config());
        let err = result.expect_err("unknown variant must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("UnexpectedVariant") || msg.contains("QueryItem"),
            "expected unknown-variant error, got: {msg}"
        );
    }

    #[test]
    fn borrow_decode_round_trips_aggregate_sum_on_range() {
        // BorrowDecode path: exercises borrow_decode_with_depth's variant-11
        // branch.
        let q = QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let bytes = bincode::encode_to_vec(&q, bincode_config()).unwrap();
        let (decoded, _): (QueryItem, _) =
            bincode::borrow_decode_from_slice(&bytes, bincode_config()).expect("borrow decode");
        assert_eq!(q, decoded);
    }

    #[test]
    fn borrow_decode_rejects_nested_aggregate_sum_on_range() {
        let nested = QueryItem::AggregateSumOnRange(Box::new(QueryItem::AggregateSumOnRange(
            Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
        )));
        let bytes = bincode::encode_to_vec(&nested, bincode_config()).expect("encode");
        let result: Result<(QueryItem, _), _> =
            bincode::borrow_decode_from_slice(&bytes, bincode_config());
        let err = result.expect_err("must reject nested aggregate sum via borrow decode");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("AggregateSumOnRange") || msg.contains("nesting depth"),
            "got: {msg}"
        );
    }

    #[test]
    fn borrow_decode_rejects_count_wrapping_sum() {
        let mixed = QueryItem::AggregateCountOnRange(Box::new(QueryItem::AggregateSumOnRange(
            Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
        )));
        let bytes = bincode::encode_to_vec(&mixed, bincode_config()).expect("encode");
        let result: Result<(QueryItem, _), _> =
            bincode::borrow_decode_from_slice(&bytes, bincode_config());
        let err = result.expect_err("must reject count wrapping sum via borrow decode");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("AggregateCountOnRange") || msg.contains("aggregate"),
            "got: {msg}"
        );
    }

    #[test]
    fn borrow_decode_rejects_sum_wrapping_count() {
        let mixed = QueryItem::AggregateSumOnRange(Box::new(QueryItem::AggregateCountOnRange(
            Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
        )));
        let bytes = bincode::encode_to_vec(&mixed, bincode_config()).expect("encode");
        let result: Result<(QueryItem, _), _> =
            bincode::borrow_decode_from_slice(&bytes, bincode_config());
        let err = result.expect_err("must reject sum wrapping count via borrow decode");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("AggregateSumOnRange") || msg.contains("aggregate"),
            "got: {msg}"
        );
    }

    #[test]
    fn borrow_decode_unknown_variant_rejected() {
        // Variant byte 13 is unknown (max = 12 after the combined
        // variant was added).
        let payload = vec![13u8];
        let result: Result<(QueryItem, _), _> =
            bincode::borrow_decode_from_slice(&payload, bincode_config());
        let err = result.expect_err("unknown variant must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("UnexpectedVariant") || msg.contains("QueryItem"),
            "got: {msg}"
        );
    }

    #[test]
    fn display_aggregate_sum_on_range_formats() {
        // Drives the Display arm at line ~717.
        let q = QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
            b"aa".to_vec()..b"zz".to_vec(),
        )));
        let s = format!("{}", q);
        assert!(s.starts_with("AggregateSumOnRange("), "got: {s}");
        assert!(s.contains("Range("), "got: {s}");
    }

    #[test]
    fn display_aggregate_count_on_range_formats() {
        // Drives the Display arm at line ~714 (also currently uncovered).
        let q = QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
            b"aa".to_vec()..b"zz".to_vec(),
        )));
        let s = format!("{}", q);
        assert!(s.starts_with("AggregateCountOnRange("), "got: {s}");
    }

    #[test]
    fn aggregate_sum_helpers_and_bounds() {
        // Hits processing_footprint, lower_bound, lower_unbounded,
        // upper_bound, upper_unbounded, enum_value, is_range, is_single,
        // is_unbounded_range, is_aggregate_*, aggregate_*_inner for the
        // sum variant.
        let inner = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
        let q = QueryItem::AggregateSumOnRange(Box::new(inner.clone()));

        assert_eq!(q.processing_footprint(), inner.processing_footprint());
        assert_eq!(q.lower_bound(), inner.lower_bound());
        assert_eq!(q.upper_bound(), inner.upper_bound());
        assert_eq!(q.lower_unbounded(), inner.lower_unbounded());
        assert_eq!(q.upper_unbounded(), inner.upper_unbounded());
        assert_eq!(q.enum_value(), 11);
        assert!(q.is_range());
        assert!(!q.is_single());
        assert!(!q.is_key());
        assert!(!q.is_aggregate_count_on_range());
        assert!(q.is_aggregate_sum_on_range());
        assert!(q.aggregate_count_inner().is_none());
        assert_eq!(q.aggregate_sum_inner(), Some(&inner));
        // unbounded delegation: inner is bounded -> false
        assert!(!q.is_unbounded_range());

        // Now wrap an unbounded inner and verify delegation flips.
        let q_unbound =
            QueryItem::AggregateSumOnRange(Box::new(QueryItem::RangeFrom(b"a".to_vec()..)));
        assert!(q_unbound.is_unbounded_range());
    }

    #[test]
    fn aggregate_count_helpers_and_bounds() {
        // Mirror the helpers for the count variant — covers count arms
        // for the same accessors (some of which were missed by the
        // existing tests).
        let inner = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
        let q = QueryItem::AggregateCountOnRange(Box::new(inner.clone()));

        assert_eq!(q.processing_footprint(), inner.processing_footprint());
        assert_eq!(q.lower_bound(), inner.lower_bound());
        assert_eq!(q.upper_bound(), inner.upper_bound());
        assert_eq!(q.lower_unbounded(), inner.lower_unbounded());
        assert_eq!(q.upper_unbounded(), inner.upper_unbounded());
        assert_eq!(q.enum_value(), 10);
        assert!(q.is_range());
        assert!(q.is_aggregate_count_on_range());
        assert!(!q.is_aggregate_sum_on_range());
        assert_eq!(q.aggregate_count_inner(), Some(&inner));
        assert!(q.aggregate_sum_inner().is_none());
        assert!(!q.is_unbounded_range());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_decode_rejects_nested_aggregate_sum_on_range() {
        // Mirrors the serde nested-rejection test for the sum variant
        // (covers the serde dispatcher's variant-11 path).
        use serde_test::{assert_de_tokens_error, Token};
        assert_de_tokens_error::<QueryItem>(
            &[
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_sum_on_range",
                },
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_sum_on_range",
                },
            ],
            "unknown field `aggregate_sum_on_range`, expected one of \
             `key`, `range`, `range_inclusive`, `range_full`, `range_from`, \
             `range_to`, `range_to_inclusive`, `range_after`, `range_after_to`, \
             `range_after_to_inclusive`",
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_decode_accepts_valid_one_level_aggregate_sum_on_range() {
        // Covers the serde Field::AggregateSumOnRange dispatch arm and
        // the NonAggregateInner inner deserialization.
        use serde_test::{assert_de_tokens, Token};
        let expected = QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        assert_de_tokens(
            &expected,
            &[
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_sum_on_range",
                },
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "range",
                },
                Token::Struct {
                    name: "Range",
                    len: 2,
                },
                Token::Str("start"),
                Token::Seq { len: Some(1) },
                Token::U8(b'a'),
                Token::SeqEnd,
                Token::Str("end"),
                Token::Seq { len: Some(1) },
                Token::U8(b'z'),
                Token::SeqEnd,
                Token::StructEnd,
            ],
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_decode_rejects_aggregate_sum_wrapping_count() {
        // The NonAggregateInner field set excludes both variants — verify
        // that explicitly for the sum-wrapping-count combination.
        use serde_test::{assert_de_tokens_error, Token};
        assert_de_tokens_error::<QueryItem>(
            &[
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_sum_on_range",
                },
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_count_on_range",
                },
            ],
            "unknown field `aggregate_count_on_range`, expected one of \
             `key`, `range`, `range_inclusive`, `range_full`, `range_from`, \
             `range_to`, `range_to_inclusive`, `range_after`, `range_after_to`, \
             `range_after_to_inclusive`",
        );
    }

    /// Regression: the Serialize impl emits variant tags in snake_case
    /// so the round-trip with the snake_case `Field` enum on the
    /// Deserialize side works for textual formats (JSON, YAML).
    ///
    /// Before this was fixed, the Serialize impl emitted PascalCase
    /// (`"AggregateSumOnRange"`) while the Deserialize side expected
    /// snake_case (`"aggregate_sum_on_range"`), so JSON round-trip
    /// failed silently for every QueryItem variant. Bincode round-trip
    /// was unaffected because it identifies variants by index, not by
    /// the textual tag.
    #[cfg(feature = "serde")]
    #[test]
    fn serde_round_trip_aggregate_sum_on_range_uses_snake_case_tag() {
        use serde_test::{assert_tokens, Token};

        let qi = QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        assert_tokens(
            &qi,
            &[
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_sum_on_range",
                },
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "range",
                },
                Token::Struct {
                    name: "Range",
                    len: 2,
                },
                Token::Str("start"),
                Token::Seq { len: Some(1) },
                Token::U8(b'a'),
                Token::SeqEnd,
                Token::Str("end"),
                Token::Seq { len: Some(1) },
                Token::U8(b'z'),
                Token::SeqEnd,
                Token::StructEnd,
            ],
        );
    }

    // ---------- AggregateCountAndSumOnRange (variant 12) bincode coverage ----------

    #[test]
    fn decode_accepts_valid_one_level_aggregate_count_and_sum_on_range() {
        let q = QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let bytes = bincode::encode_to_vec(&q, bincode_config()).unwrap();
        let (decoded, _): (QueryItem, _) = bincode::decode_from_slice(&bytes, bincode_config())
            .expect("single-level combined wrap must decode");
        assert_eq!(q, decoded);
    }

    #[test]
    fn borrow_decode_round_trips_aggregate_count_and_sum_on_range() {
        let q = QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let bytes = bincode::encode_to_vec(&q, bincode_config()).unwrap();
        let (decoded, _): (QueryItem, _) =
            bincode::borrow_decode_from_slice(&bytes, bincode_config())
                .expect("borrow decode combined");
        assert_eq!(q, decoded);
    }

    #[test]
    fn decode_rejects_nested_aggregate_count_and_sum_on_range() {
        // Combined wrapping combined.
        let nested = QueryItem::AggregateCountAndSumOnRange(Box::new(
            QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ))),
        ));
        let bytes = bincode::encode_to_vec(&nested, bincode_config()).expect("encode");
        let result: Result<(QueryItem, _), _> =
            bincode::decode_from_slice(&bytes, bincode_config());
        let err =
            result.expect_err("nested AggregateCountAndSumOnRange must be rejected at decode time");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("AggregateCountAndSumOnRange") || msg.contains("nesting depth"),
            "got: {msg}"
        );
    }

    #[test]
    fn decode_rejects_combined_wrapping_count_or_sum() {
        // Combined wrapping ACOR.
        let mixed_count =
            QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::AggregateCountOnRange(
                Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
            )));
        let bytes = bincode::encode_to_vec(&mixed_count, bincode_config()).expect("encode");
        bincode::decode_from_slice::<QueryItem, _>(&bytes, bincode_config())
            .expect_err("combined wrapping count must be rejected");

        // Combined wrapping ASOR.
        let mixed_sum =
            QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::AggregateSumOnRange(
                Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
            )));
        let bytes = bincode::encode_to_vec(&mixed_sum, bincode_config()).expect("encode");
        bincode::decode_from_slice::<QueryItem, _>(&bytes, bincode_config())
            .expect_err("combined wrapping sum must be rejected");

        // ACOR / ASOR wrapping combined (orthogonality, reverse direction).
        let count_wrapping =
            QueryItem::AggregateCountOnRange(Box::new(QueryItem::AggregateCountAndSumOnRange(
                Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
            )));
        let bytes = bincode::encode_to_vec(&count_wrapping, bincode_config()).expect("encode");
        bincode::decode_from_slice::<QueryItem, _>(&bytes, bincode_config())
            .expect_err("count wrapping combined must be rejected");

        let sum_wrapping =
            QueryItem::AggregateSumOnRange(Box::new(QueryItem::AggregateCountAndSumOnRange(
                Box::new(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
            )));
        let bytes = bincode::encode_to_vec(&sum_wrapping, bincode_config()).expect("encode");
        bincode::decode_from_slice::<QueryItem, _>(&bytes, bincode_config())
            .expect_err("sum wrapping combined must be rejected");
    }

    #[test]
    fn decode_unknown_variant_rejected_now_12_is_max() {
        // Variant byte 13 is unknown (max = 12 after this PR).
        let payload = vec![13u8];
        let result: Result<(QueryItem, _), _> =
            bincode::decode_from_slice(&payload, bincode_config());
        let err = result.expect_err("unknown variant must be rejected");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("UnexpectedVariant") || msg.contains("QueryItem"),
            "got: {msg}"
        );
    }

    #[test]
    fn aggregate_count_and_sum_helpers_and_bounds() {
        // Hits processing_footprint, lower_bound, lower_unbounded,
        // upper_bound, upper_unbounded, enum_value, is_range,
        // is_aggregate_*, aggregate_*_inner for the combined variant.
        let inner = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
        let q = QueryItem::AggregateCountAndSumOnRange(Box::new(inner.clone()));

        assert_eq!(q.processing_footprint(), inner.processing_footprint());
        assert_eq!(q.lower_bound(), inner.lower_bound());
        assert_eq!(q.upper_bound(), inner.upper_bound());
        assert_eq!(q.lower_unbounded(), inner.lower_unbounded());
        assert_eq!(q.upper_unbounded(), inner.upper_unbounded());
        assert_eq!(q.enum_value(), 12);
        assert!(q.is_range());
        assert!(!q.is_single());
        assert!(!q.is_key());
        assert!(!q.is_aggregate_count_on_range());
        assert!(!q.is_aggregate_sum_on_range());
        assert!(q.is_aggregate_count_and_sum_on_range());
        assert!(q.aggregate_count_inner().is_none());
        assert!(q.aggregate_sum_inner().is_none());
        assert_eq!(q.aggregate_count_and_sum_inner(), Some(&inner));
        assert!(!q.is_unbounded_range());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_round_trip_aggregate_count_and_sum_on_range_uses_snake_case_tag() {
        use serde_test::{assert_tokens, Token};

        let qi = QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        assert_tokens(
            &qi,
            &[
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_count_and_sum_on_range",
                },
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "range",
                },
                Token::Struct {
                    name: "Range",
                    len: 2,
                },
                Token::Str("start"),
                Token::Seq { len: Some(1) },
                Token::U8(b'a'),
                Token::SeqEnd,
                Token::Str("end"),
                Token::Seq { len: Some(1) },
                Token::U8(b'z'),
                Token::SeqEnd,
                Token::StructEnd,
            ],
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_decode_rejects_nested_combined_aggregate() {
        use serde_test::{assert_de_tokens_error, Token};
        // Combined wrapping combined.
        assert_de_tokens_error::<QueryItem>(
            &[
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_count_and_sum_on_range",
                },
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_count_and_sum_on_range",
                },
            ],
            "unknown field `aggregate_count_and_sum_on_range`, expected one of \
             `key`, `range`, `range_inclusive`, `range_full`, `range_from`, \
             `range_to`, `range_to_inclusive`, `range_after`, `range_after_to`, \
             `range_after_to_inclusive`",
        );
    }

    #[test]
    fn display_aggregate_count_and_sum_on_range_formats() {
        let q = QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::Range(
            b"aa".to_vec()..b"zz".to_vec(),
        )));
        let s = format!("{}", q);
        assert!(s.starts_with("AggregateCountAndSumOnRange("), "got: {s}");
    }

    /// Mirror of the sum test: count side round-trips through
    /// snake_case too. Pins the contract so both aggregate variants
    /// stay in lockstep on the Serialize side.
    #[cfg(feature = "serde")]
    #[test]
    fn serde_round_trip_aggregate_count_on_range_uses_snake_case_tag() {
        use serde_test::{assert_tokens, Token};

        let qi = QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        assert_tokens(
            &qi,
            &[
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "aggregate_count_on_range",
                },
                Token::NewtypeVariant {
                    name: "QueryItem",
                    variant: "range",
                },
                Token::Struct {
                    name: "Range",
                    len: 2,
                },
                Token::Str("start"),
                Token::Seq { len: Some(1) },
                Token::U8(b'a'),
                Token::SeqEnd,
                Token::Str("end"),
                Token::Seq { len: Some(1) },
                Token::U8(b'z'),
                Token::SeqEnd,
                Token::StructEnd,
            ],
        );
    }
}
