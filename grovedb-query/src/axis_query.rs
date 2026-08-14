//! Vocabulary for **axis-ordered** reads of an indexed tree.
//!
//! An ordinary [`Query`](crate::Query) selects keys: its items name keys
//! or key ranges in the Merk a path points at. That vocabulary cannot
//! describe the other thing an indexed tree can answer — "the best `k`
//! groups by aggregate" — because that ordering lives in the tree's
//! per-axis **secondary**, which is keyed by `sort_key ‖ original_key`
//! and is not a path-addressable subtree. [`AxisQuery`] is the missing
//! vocabulary: which axis to read, how to walk it, and in which
//! direction. It travels inside a `Query` as a
//! [`ReadMode`](crate::ReadMode), never as a `QueryItem` — an axis read
//! has no key-range meaning, so it does not participate in the item
//! algebra (merge, intersect, ordering).
//!
//! Wire stability: every tag in this module (the [`IndexAxis`] tag
//! byte, the [`AxisTraversal`] variant tags) is frozen on first
//! release. The `bincode` implementations are written by hand so the
//! encoding cannot drift if variants are reordered or added.

use std::fmt;

use bincode::{
    de::{BorrowDecoder, Decoder},
    enc::Encoder,
    error::{DecodeError, EncodeError},
    BorrowDecode, Decode, Encode,
};

use crate::error::Error;

/// Axis tag for an indexed tree's per-axis secondary.
///
/// The numeric values are on-disk / on-wire tag bytes shared by the
/// indexed-tree element encoding, every axis proof envelope, and the
/// [`AxisQuery`] encoding — they must never change:
///
/// - `0` = `Count`: secondary keyed by `(count_be ‖ original_key)`.
/// - `1` = `Sum`:   secondary keyed by `(sum_sortable_be ‖ original_key)`.
/// - `2` = `Avg`:   secondary keyed by `(avg_sortable_be ‖ original_key)`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum IndexAxis {
    /// Count axis. Secondary entries are ordered by aggregate count.
    Count = 0,
    /// Sum axis. Secondary entries are ordered by aggregate sum (signed).
    Sum = 1,
    /// Average axis. Secondary entries are ordered by the fixed-point
    /// average `floor(sum * SCALE / count)`.
    Avg = 2,
}

/// A tag byte that does not name any [`IndexAxis`]. Carries the
/// offending byte so error surfaces can report it; converts into the
/// element crate's error type at that boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownAxisTag(pub u8);

impl fmt::Display for UnknownAxisTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown axis tag {}", self.0)
    }
}

impl std::error::Error for UnknownAxisTag {}

impl IndexAxis {
    /// On-disk / on-wire tag byte for this axis.
    #[inline]
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// Inverse of [`Self::tag`]: parse a tag byte into the axis. Returns
    /// [`UnknownAxisTag`] for any byte outside the `0..=2` range.
    #[inline]
    pub const fn try_from_tag(b: u8) -> Result<Self, UnknownAxisTag> {
        match b {
            0 => Ok(IndexAxis::Count),
            1 => Ok(IndexAxis::Sum),
            2 => Ok(IndexAxis::Avg),
            other => Err(UnknownAxisTag(other)),
        }
    }
}

/// Maximum length of the key carried by [`AxisTraversal::RankOfKey`].
/// Matches the element layer's key-length limit; enforced at decode
/// time so a hostile payload cannot smuggle an oversized allocation
/// through the traversal.
pub const MAX_RANK_OF_KEY_LEN: usize = 255;

/// Which aggregate an [`AxisTraversal::AggregateOverValueRange`] folds
/// over the entries the value band selects. Wire bytes are explicit and
/// frozen: `Population = 0`, `Total = 1`.
///
/// The fold is EXPLICIT because the two readings genuinely differ and
/// the "obvious" one flips per axis: over counts `[3, 1, 5]`, the band
/// `[2, 10]` selects the `3` and the `5`, so `Population` answers **2**
/// (each selected entry contributes 1) while `Total` answers **8** (the
/// selected values are summed). Making the caller say which they mean
/// removes the ambiguity that an axis-default fold invited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AggregateFold {
    /// How many entries the band selects; each contributes 1.
    /// Entries, not distinct values: two entries sharing the same axis
    /// value are two nodes of the secondary (it is keyed
    /// `sort_key ‖ original_key`) and count as 2. Answered by the
    /// secondary's count aggregate, so it needs a count-bearing
    /// secondary.
    Population,
    /// The sum of the selected entries' axis values. Answered by the
    /// secondary's sum aggregate, so it needs a sum-bearing secondary.
    Total,
}

impl AggregateFold {
    /// The frozen wire byte.
    #[inline]
    pub const fn tag(&self) -> u8 {
        match self {
            AggregateFold::Population => 0,
            AggregateFold::Total => 1,
        }
    }

    /// Inverse of [`Self::tag`]; any byte outside `0..=1` is an error.
    #[inline]
    pub const fn try_from_tag(b: u8) -> Result<Self, u8> {
        match b {
            0 => Ok(AggregateFold::Population),
            1 => Ok(AggregateFold::Total),
            other => Err(other),
        }
    }
}

impl fmt::Display for AggregateFold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AggregateFold::Population => write!(f, "population"),
            AggregateFold::Total => write!(f, "total"),
        }
    }
}

/// How an [`AxisQuery`] walks the secondary. Wire tags are explicit and
/// frozen: `RankedPage = 0`, `Bounded = 1`, `RankOfKey = 2`,
/// `AggregateOverValueRange = 3`.
///
/// # Cost
///
/// Each variant documents best / average / worst prover work, which is
/// also the shape of the proof and so of the verifier's work. Throughout,
/// `n` is the number of entries on the queried axis; an empty secondary
/// short-circuits every traversal to `O(1)`.
///
/// The costs are worth reading before choosing a shape: none of them
/// scale with how *deep* into the ordering the answer sits, because
/// every axis secondary binds an aggregate count into its node hashes.
/// Skipping and counting are read off subtree commitments rather than
/// walked entry by entry.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AxisTraversal {
    /// The `k` entries starting at rank `offset` **in the walk
    /// direction** (`offset = 0` for the first page).
    ///
    /// The direction is [`AxisQuery::descending`], not part of this
    /// variant, so one shape serves both readings:
    ///
    /// - `descending: true` → the `k` **largest** by aggregate (top-k),
    /// - `descending: false` → the `k` **smallest** (bottom-k).
    ///
    /// Named for what it is — a page at a rank — rather than "top-k",
    /// which would read as a contradiction in the ascending case.
    ///
    /// Skipping is attested from the secondary's counted subtree
    /// commitments rather than walked, so a large `offset` costs the
    /// same as a small one.
    ///
    /// **Cost** — best `O(log n)` (single-entry page), average and
    /// worst `O(log n + k)`: descend to the page start, then emit `k`
    /// entries. **No term in `offset`**: each skipped subtree collapses
    /// to one counted commitment, so the skip is `O(log n)` rather than
    /// `O(offset)` — page 10 000 costs what page 1 costs.
    RankedPage {
        /// Number of entries to return.
        k: u16,
        /// Rank the returned page starts at.
        offset: u64,
    },
    /// Every entry whose aggregate falls in the **inclusive** range
    /// `[lo, hi]`, up to `limit` entries, in the walk direction.
    ///
    /// Bounds are carried as `i128` for every axis — the convention the
    /// aggregate-range entry points use — and are validated against the
    /// axis's own domain.
    ///
    /// **Cost** — with `m` the entries actually inside `[lo, hi]`: best
    /// `O(log n)` (the range matches nothing), average
    /// `O(log n + min(limit, m))`, worst `O(log n + limit)`. Unlike
    /// [`Self::RankedPage`] this walks the matched entries, so `limit`
    /// is the real bound on work — an unbounded-looking range is only
    /// as expensive as the `limit` you set.
    Bounded {
        /// Inclusive lower bound on the aggregate.
        lo: i128,
        /// Inclusive upper bound on the aggregate.
        hi: i128,
        /// Maximum entries to return.
        limit: u16,
    },
    /// The rank of `key` in the directional walk — "where does this
    /// entry place?". Served as an `offset = rank, k = 1` page whose
    /// verifier additionally checks the yielded key equals `key`.
    ///
    /// The rank is *derived*, never searched for: the entry's position
    /// is a pure function of its aggregate and its key (the secondary
    /// is keyed `sort_key ‖ original_key`), so one point read of the
    /// primary reconstructs its secondary key, and the entries before
    /// it are counted off the subtree commitments.
    ///
    /// **Cost** — `O(log n)` in every case: one primary point read,
    /// one counted-range count, one single-entry page proof. **No term
    /// in the rank itself** — ranking 5-millionth costs what ranking
    /// 5th costs. Errors with `PathKeyNotFound` when `key` is absent
    /// from the primary: this answers where an entry *does* place, not
    /// where a hypothetical one would.
    RankOfKey {
        /// The original (primary) key whose rank is requested.
        key: Vec<u8>,
    },
    /// `[lo, hi]` selects the entries by their own axis value; `fold`
    /// says which aggregate over exactly those entries is the answer.
    /// Count and Sum axes only — the Avg axis has no meaningful
    /// aggregate-of-averages.
    ///
    /// The fold is explicit because both readings are meaningful on
    /// both axes and the "obvious" one flips per axis. Over counts
    /// `[3, 1, 5]`, the band `[2, 10]` selects the `3` and the `5`:
    ///
    /// * [`AggregateFold::Population`] answers **2** — how many entries
    ///   fall in the band, each contributing 1.
    /// * [`AggregateFold::Total`] answers **8** — the selected values
    ///   summed.
    ///
    /// The same pair over sums `[40, -10, 25]` with band `[0, 100]`
    /// selects `40` and `25`: `Population` answers `2`, `Total` answers
    /// `65`.
    ///
    /// **Currently unsupported: `Total` on the count axis.** The query
    /// validates and serializes (the vocabulary is stable), but every
    /// execution surface — trusted read, embedded prover/verifier,
    /// standalone prover/verifier — refuses it with a typed
    /// `NotSupported` naming issue #806 until the count secondary
    /// becomes sum-bearing (#806 part 2, which removes this paragraph).
    /// The other three (axis, fold) cells are served today.
    ///
    /// **Cost** — `O(log n)` in every case, either fold. The walk
    /// classifies each subtree as fully Contained, Disjoint, or Partial
    /// and folds a Contained subtree's stored aggregate in one step,
    /// descending only along the two range boundaries. **No term in the
    /// number of matched entries** — aggregating a million in-range
    /// entries costs what aggregating one costs, which is what makes
    /// this preferable to [`Self::Bounded`] whenever only the scalar is
    /// wanted.
    AggregateOverValueRange {
        /// Inclusive lower bound on the entry's own axis VALUE (its
        /// count on the count axis, its sum on the sum axis) — not on
        /// the aggregate this traversal returns.
        lo: i128,
        /// Inclusive upper bound on the entry's own axis value. See
        /// [`Self::AggregateOverValueRange::lo`].
        hi: i128,
        /// The aggregate to fold over the selected entries.
        fold: AggregateFold,
    },
}

impl Encode for AxisTraversal {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        match self {
            AxisTraversal::RankedPage { k, offset } => {
                0u8.encode(encoder)?;
                k.encode(encoder)?;
                offset.encode(encoder)
            }
            AxisTraversal::Bounded { lo, hi, limit } => {
                1u8.encode(encoder)?;
                lo.encode(encoder)?;
                hi.encode(encoder)?;
                limit.encode(encoder)
            }
            AxisTraversal::RankOfKey { key } => {
                2u8.encode(encoder)?;
                key.encode(encoder)
            }
            AxisTraversal::AggregateOverValueRange { lo, hi, fold } => {
                3u8.encode(encoder)?;
                lo.encode(encoder)?;
                hi.encode(encoder)?;
                fold.tag().encode(encoder)
            }
        }
    }
}

impl<Context> Decode<Context> for AxisTraversal {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        match u8::decode(decoder)? {
            0 => Ok(AxisTraversal::RankedPage {
                k: u16::decode(decoder)?,
                offset: u64::decode(decoder)?,
            }),
            1 => Ok(AxisTraversal::Bounded {
                lo: i128::decode(decoder)?,
                hi: i128::decode(decoder)?,
                limit: u16::decode(decoder)?,
            }),
            2 => {
                let key = Vec::<u8>::decode(decoder)?;
                if key.len() > MAX_RANK_OF_KEY_LEN {
                    return Err(DecodeError::Other(
                        "rank-of-key key exceeds the maximum key length",
                    ));
                }
                Ok(AxisTraversal::RankOfKey { key })
            }
            3 => Ok(AxisTraversal::AggregateOverValueRange {
                lo: i128::decode(decoder)?,
                hi: i128::decode(decoder)?,
                fold: AggregateFold::try_from_tag(u8::decode(decoder)?)
                    .map_err(|_| DecodeError::Other("unknown aggregate fold tag"))?,
            }),
            _ => Err(DecodeError::Other("unknown axis traversal tag")),
        }
    }
}

impl<'de, Context> BorrowDecode<'de, Context> for AxisTraversal {
    fn borrow_decode<D: BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode(decoder)
    }
}

/// What to read from one indexed tree's per-axis secondary: which axis,
/// how to walk it, and in which direction. The axis-ordered counterpart
/// of a key-selecting [`Query`](crate::Query).
///
/// `bincode` is implemented by hand: the axis travels as its canonical
/// tag byte (the same byte the indexed-tree element and every proof
/// envelope use), so the encoding stays stable if the enum ever gains
/// variants.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AxisQuery {
    /// Which per-axis secondary to read. The indexed tree must carry
    /// this axis; it is authenticated in the proof, so a query naming
    /// an axis the element does not configure fails rather than
    /// silently reading another one.
    pub axis: IndexAxis,
    /// How to walk it.
    pub traversal: AxisTraversal,
    /// `true` walks from the largest aggregate down ("best first");
    /// `false` from the smallest up. Ties break by the entry's original
    /// key **in the direction of the walk** — a property of the
    /// directional scan over `sort_key ‖ original_key`, not a separate
    /// rule.
    pub descending: bool,
}

impl Encode for AxisQuery {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        self.axis.tag().encode(encoder)?;
        self.traversal.encode(encoder)?;
        self.descending.encode(encoder)
    }
}

impl<Context> Decode<Context> for AxisQuery {
    fn decode<D: Decoder<Context = Context>>(decoder: &mut D) -> Result<Self, DecodeError> {
        let tag = u8::decode(decoder)?;
        let axis = IndexAxis::try_from_tag(tag)
            .map_err(|_| DecodeError::Other("unknown index axis tag"))?;
        Ok(Self {
            axis,
            traversal: AxisTraversal::decode(decoder)?,
            descending: bool::decode(decoder)?,
        })
    }
}

impl<'de, Context> BorrowDecode<'de, Context> for AxisQuery {
    fn borrow_decode<D: BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode(decoder)
    }
}

impl AxisQuery {
    /// A page of `k` entries on `axis`, starting at rank `offset`.
    ///
    /// `descending` chooses which end the ranking starts from: `true`
    /// gives the `k` largest by aggregate (top-k), `false` the `k`
    /// smallest — for which [`Self::bottom_k`] is the clearer spelling.
    /// See [`AxisTraversal::RankedPage`] for the shape and its cost.
    pub const fn top_k(axis: IndexAxis, k: u16, offset: u64, descending: bool) -> Self {
        Self {
            axis,
            traversal: AxisTraversal::RankedPage { k, offset },
            descending,
        }
    }

    /// The `k` **smallest** entries on `axis` by aggregate, starting at
    /// rank `offset` — the ascending reading of
    /// [`AxisTraversal::RankedPage`].
    ///
    /// Identical to [`Self::top_k`] with `descending: false`, but says
    /// which end it starts from in the name rather than in a boolean
    /// argument, where `top_k(.., false)` reads as a contradiction.
    /// Same cost: `O(log n + k)`, with no term in `offset`.
    pub const fn bottom_k(axis: IndexAxis, k: u16, offset: u64) -> Self {
        Self {
            axis,
            traversal: AxisTraversal::RankedPage { k, offset },
            descending: false,
        }
    }

    /// Every entry whose aggregate is in `[lo, hi]`, up to `limit`.
    pub const fn bounded(
        axis: IndexAxis,
        lo: i128,
        hi: i128,
        limit: u16,
        descending: bool,
    ) -> Self {
        Self {
            axis,
            traversal: AxisTraversal::Bounded { lo, hi, limit },
            descending,
        }
    }

    /// The rank of `key` in the directional walk over `axis`.
    pub const fn rank_of_key(axis: IndexAxis, key: Vec<u8>, descending: bool) -> Self {
        Self {
            axis,
            traversal: AxisTraversal::RankOfKey { key },
            descending,
        }
    }

    /// A single `fold` aggregate over the entries whose axis value is
    /// in `[lo, hi]` — [`AggregateFold::Population`] for how many,
    /// [`AggregateFold::Total`] for the sum of their values. Direction
    /// does not affect the answer; constructors set
    /// `descending = false`.
    pub const fn aggregate_over_value_range(
        axis: IndexAxis,
        lo: i128,
        hi: i128,
        fold: AggregateFold,
    ) -> Self {
        Self {
            axis,
            traversal: AxisTraversal::AggregateOverValueRange { lo, hi, fold },
            descending: false,
        }
    }

    /// Reject a query that cannot describe any answer, so a caller
    /// error surfaces as a query error rather than an empty result that
    /// looks like real absence.
    ///
    /// Checked in one place so the read, prove, and verify paths cannot
    /// disagree on what is well-formed.
    pub fn validate(&self) -> Result<(), Error> {
        match &self.traversal {
            AxisTraversal::RankedPage { k, .. } => {
                if *k == 0 {
                    return Err(Error::InvalidOperation(
                        "axis query: `k` must be at least 1; a zero-length page selects nothing",
                    ));
                }
            }
            AxisTraversal::Bounded { lo, hi, limit } => {
                if *limit == 0 {
                    return Err(Error::InvalidOperation(
                        "axis query: `limit` must be at least 1; a zero-length page selects \
                         nothing",
                    ));
                }
                self.validate_bounds(*lo, *hi)?;
            }
            AxisTraversal::RankOfKey { key } => {
                if key.is_empty() {
                    return Err(Error::InvalidOperation(
                        "axis query: rank-of-key requires a non-empty key",
                    ));
                }
                if key.len() > MAX_RANK_OF_KEY_LEN {
                    return Err(Error::InvalidOperation(
                        "axis query: rank-of-key key exceeds the maximum key length",
                    ));
                }
            }
            AxisTraversal::AggregateOverValueRange { lo, hi, fold: _ } => {
                // Both folds are rejected on the Avg axis: a total of
                // averages is not meaningful, and a population over the
                // avg ordering is served by the other two axes' bands
                // in every use case seen so far — permit it later if
                // one appears (additive).
                if self.axis == IndexAxis::Avg {
                    return Err(Error::InvalidOperation(
                        "axis query: the Avg axis has no value-range aggregate — an \
                         aggregate of averages is not meaningful",
                    ));
                }
                self.validate_bounds(*lo, *hi)?;
            }
        }
        Ok(())
    }

    /// Shared bound rules for [`AxisTraversal::Bounded`] and
    /// [`AxisTraversal::AggregateOverValueRange`].
    fn validate_bounds(&self, lo: i128, hi: i128) -> Result<(), Error> {
        if lo > hi {
            return Err(Error::InvalidOperation(
                "axis query: the bounds are inverted (`lo > hi`), so they can match nothing",
            ));
        }
        if self.bounds_out_of_domain(lo, hi) {
            return Err(Error::InvalidOperation(
                "axis query: the bounds fall entirely outside the axis's value domain, so \
                 they can match nothing",
            ));
        }
        Ok(())
    }

    /// Whether `[lo, hi]` lies wholly outside what this axis can hold.
    /// A partial overlap is fine — execution clamps it to the domain.
    fn bounds_out_of_domain(&self, lo: i128, hi: i128) -> bool {
        match self.axis {
            IndexAxis::Count => hi < 0 || lo > u64::MAX as i128,
            IndexAxis::Sum => hi < i64::MIN as i128 || lo > i64::MAX as i128,
            // The avg axis is ordered by the fixed-point average, whose
            // domain is the whole i128 range; nothing is out of domain.
            IndexAxis::Avg => false,
        }
    }

    /// The number of entries this query can return, when that is a
    /// fixed property of the traversal (`None` for
    /// [`AxisTraversal::AggregateOverValueRange`], which returns one scalar, not
    /// entries).
    pub const fn entry_cap(&self) -> Option<u16> {
        match &self.traversal {
            AxisTraversal::RankedPage { k, .. } => Some(*k),
            AxisTraversal::Bounded { limit, .. } => Some(*limit),
            AxisTraversal::RankOfKey { .. } => Some(1),
            AxisTraversal::AggregateOverValueRange { .. } => None,
        }
    }
}

impl fmt::Display for AxisTraversal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AxisTraversal::RankedPage { k, offset } => {
                write!(f, "RankedPage {{ k: {k}, offset: {offset} }}")
            }
            AxisTraversal::Bounded { lo, hi, limit } => {
                write!(f, "Bounded {{ lo: {lo}, hi: {hi}, limit: {limit} }}")
            }
            AxisTraversal::RankOfKey { key } => {
                write!(f, "RankOfKey {{ key: {} }}", crate::hex_to_ascii(key))
            }
            AxisTraversal::AggregateOverValueRange { lo, hi, fold } => {
                write!(
                    f,
                    "AggregateOverValueRange {{ lo: {lo}, hi: {hi}, fold: {fold} }}"
                )
            }
        }
    }
}

impl fmt::Display for AxisQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AxisQuery {{ axis: {:?}, {}, {} }}",
            self.axis,
            self.traversal,
            if self.descending {
                "descending"
            } else {
                "ascending"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use bincode::config;

    use super::*;

    #[test]
    fn axis_tag_round_trip() {
        for a in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            assert_eq!(IndexAxis::try_from_tag(a.tag()).unwrap(), a);
        }
    }

    #[test]
    fn axis_tag_rejects_unknown_byte() {
        assert_eq!(IndexAxis::try_from_tag(3), Err(UnknownAxisTag(3)));
        assert_eq!(format!("{}", UnknownAxisTag(255)), "unknown axis tag 255");
    }

    #[test]
    fn axis_ordering_is_canonical() {
        // The canonical order required by the on-disk TLV: Count < Sum < Avg.
        assert!(IndexAxis::Count < IndexAxis::Sum);
        assert!(IndexAxis::Sum < IndexAxis::Avg);
    }

    fn all_traversals() -> Vec<AxisTraversal> {
        vec![
            AxisTraversal::RankedPage { k: 5, offset: 100 },
            AxisTraversal::Bounded {
                lo: -7,
                hi: 12,
                limit: 3,
            },
            AxisTraversal::RankOfKey {
                key: b"alice".to_vec(),
            },
            AxisTraversal::AggregateOverValueRange {
                lo: 0,
                hi: 50,
                fold: AggregateFold::Population,
            },
            AxisTraversal::AggregateOverValueRange {
                lo: 0,
                hi: 50,
                fold: AggregateFold::Total,
            },
        ]
    }

    #[test]
    fn axis_query_round_trips_every_traversal_and_axis() {
        for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            for traversal in all_traversals() {
                for descending in [false, true] {
                    let q = AxisQuery {
                        axis,
                        traversal: traversal.clone(),
                        descending,
                    };
                    let bytes = bincode::encode_to_vec(&q, config::standard()).unwrap();
                    let (decoded, consumed): (AxisQuery, usize) =
                        bincode::decode_from_slice(&bytes, config::standard()).unwrap();
                    assert_eq!(consumed, bytes.len());
                    assert_eq!(decoded, q);
                }
            }
        }
    }

    #[test]
    fn traversal_wire_tags_are_frozen() {
        // First byte of each traversal encoding is its frozen tag.
        let tags: Vec<u8> = all_traversals()
            .into_iter()
            .map(|t| bincode::encode_to_vec(&t, config::standard()).unwrap()[0])
            .collect();
        assert_eq!(tags, vec![0, 1, 2, 3, 3]);
        // First byte of an AxisQuery encoding is the axis tag byte.
        let q = AxisQuery::top_k(IndexAxis::Avg, 1, 0, true);
        let bytes = bincode::encode_to_vec(&q, config::standard()).unwrap();
        assert_eq!(bytes[0], 2);
    }

    #[test]
    fn decode_rejects_unknown_tags_and_oversized_rank_key() {
        // Unknown traversal tag.
        let err = bincode::decode_from_slice::<AxisTraversal, _>(&[9u8], config::standard());
        assert!(err.is_err());
        // Unknown axis tag at the head of an AxisQuery.
        let err =
            bincode::decode_from_slice::<AxisQuery, _>(&[7u8, 0, 1, 0, 0], config::standard());
        assert!(err.is_err());
        // Oversized rank-of-key key.
        let mut q = AxisQuery::rank_of_key(IndexAxis::Count, vec![0u8; 256], false);
        let bytes = bincode::encode_to_vec(&q, config::standard()).unwrap();
        assert!(bincode::decode_from_slice::<AxisQuery, _>(&bytes, config::standard()).is_err());
        // At the cap it round-trips.
        q = AxisQuery::rank_of_key(IndexAxis::Count, vec![0u8; 255], false);
        let bytes = bincode::encode_to_vec(&q, config::standard()).unwrap();
        assert!(bincode::decode_from_slice::<AxisQuery, _>(&bytes, config::standard()).is_ok());
    }

    #[test]
    fn decode_rejects_unknown_fold_bytes() {
        // tag 3, lo = 0, hi = 0 (i128s), then a fold byte outside 0..=1:
        // fail closed, exactly like an unknown traversal tag.
        let good = bincode::encode_to_vec(
            &AxisTraversal::AggregateOverValueRange {
                lo: 0,
                hi: 0,
                fold: AggregateFold::Total,
            },
            config::standard(),
        )
        .unwrap();
        let mut bad = good.clone();
        *bad.last_mut().unwrap() = 2;
        assert!(
            bincode::decode_from_slice::<AxisTraversal, _>(&bad, config::standard()).is_err(),
            "an unknown fold byte must not decode"
        );
        // Sanity: the honest bytes decode, and the last byte IS the fold.
        assert_eq!(*good.last().unwrap(), 1, "Total = 1 on the wire");
        assert!(bincode::decode_from_slice::<AxisTraversal, _>(&good, config::standard()).is_ok());
    }

    #[test]
    fn validate_rejects_unanswerable_queries() {
        // k = 0.
        assert!(AxisQuery::top_k(IndexAxis::Count, 0, 0, true)
            .validate()
            .is_err());
        // limit = 0.
        assert!(AxisQuery::bounded(IndexAxis::Sum, 0, 10, 0, true)
            .validate()
            .is_err());
        // Inverted bounds.
        assert!(AxisQuery::bounded(IndexAxis::Sum, 10, 0, 1, true)
            .validate()
            .is_err());
        // Wholly out of domain: negative counts.
        assert!(AxisQuery::bounded(IndexAxis::Count, -10, -1, 1, true)
            .validate()
            .is_err());
        // Wholly out of domain: beyond i64 for sums.
        assert!(AxisQuery::aggregate_over_value_range(
            IndexAxis::Sum,
            i64::MAX as i128 + 1,
            i128::MAX,
            AggregateFold::Total
        )
        .validate()
        .is_err());
        // Aggregate over the value range on Avg — both folds.
        for fold in [AggregateFold::Population, AggregateFold::Total] {
            assert!(
                AxisQuery::aggregate_over_value_range(IndexAxis::Avg, 0, 10, fold)
                    .validate()
                    .is_err()
            );
        }
        // Empty rank key.
        assert!(AxisQuery::rank_of_key(IndexAxis::Count, vec![], true)
            .validate()
            .is_err());
        // Partial domain overlap is fine.
        assert!(AxisQuery::bounded(IndexAxis::Count, -5, 5, 1, false)
            .validate()
            .is_ok());
        // Avg accepts the full i128 range for Bounded.
        assert!(
            AxisQuery::bounded(IndexAxis::Avg, i128::MIN, i128::MAX, 1, false)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn bottom_k_is_the_ascending_ranked_page() {
        // `bottom_k` is exactly `top_k` with the walk reversed — same
        // traversal, same wire bytes, only the direction differs.
        let bottom = AxisQuery::bottom_k(IndexAxis::Sum, 5, 10);
        assert_eq!(bottom, AxisQuery::top_k(IndexAxis::Sum, 5, 10, false));
        assert!(!bottom.descending);
        assert_eq!(
            bottom.traversal,
            AxisTraversal::RankedPage { k: 5, offset: 10 }
        );
        bottom.validate().expect("a bottom-k page is well formed");

        // ...and is the mirror of the descending page, not a different
        // shape: the two differ in exactly one byte on the wire.
        let top = AxisQuery::top_k(IndexAxis::Sum, 5, 10, true);
        let bottom_bytes = bincode::encode_to_vec(&bottom, config::standard()).unwrap();
        let top_bytes = bincode::encode_to_vec(&top, config::standard()).unwrap();
        assert_eq!(bottom_bytes.len(), top_bytes.len());
        assert_eq!(
            bottom_bytes
                .iter()
                .zip(&top_bytes)
                .filter(|(a, b)| a != b)
                .count(),
            1,
            "only the descending flag distinguishes the two directions"
        );
    }

    #[test]
    fn entry_caps() {
        assert_eq!(
            AxisQuery::top_k(IndexAxis::Count, 7, 0, true).entry_cap(),
            Some(7)
        );
        assert_eq!(
            AxisQuery::bottom_k(IndexAxis::Count, 7, 0).entry_cap(),
            Some(7)
        );
        assert_eq!(
            AxisQuery::bounded(IndexAxis::Sum, 0, 1, 9, true).entry_cap(),
            Some(9)
        );
        assert_eq!(
            AxisQuery::rank_of_key(IndexAxis::Sum, b"k".to_vec(), true).entry_cap(),
            Some(1)
        );
        for fold in [AggregateFold::Population, AggregateFold::Total] {
            assert_eq!(
                AxisQuery::aggregate_over_value_range(IndexAxis::Sum, 0, 1, fold).entry_cap(),
                None
            );
        }
    }
}
