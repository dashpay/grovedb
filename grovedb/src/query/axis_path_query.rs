//! Query vocabulary for **axis-ordered** reads of an indexed tree.
//!
//! An ordinary [`PathQuery`](crate::PathQuery) selects keys: its items
//! name keys or key ranges in the Merk a path points at. That vocabulary
//! cannot describe the other thing an indexed tree can answer — "the
//! best `k` groups by aggregate" — because that ordering lives in the
//! tree's per-axis **secondary**, which is keyed by
//! `sort_key ‖ original_key` and is not a path-addressable subtree. It
//! is an internal structure of the element, with its own storage
//! prefix, so no path (and therefore no `PathQuery`, merged or not)
//! names it.
//!
//! The consequence, before this module existed, was that a caller
//! wanting an axis-ordered answer left the query language entirely and
//! called one of a dozen bespoke `indexed_{count,sum,avg}_*` methods,
//! each with its own argument list, and hand-built the secondary's Merk
//! query when it wanted bounds. [`AxisPathQuery`] gives that capability
//! the same shape everything else has: a path plus a description of
//! what to read, one entry point to execute it, one to prove it, one to
//! verify it.
//!
//! ## What it does not (yet) do
//!
//! This is vocabulary and dispatch, not a new proof shape. Executing an
//! [`AxisPathQuery`] routes to the existing indexed-axis primitives and
//! produces the existing envelopes, byte for byte. Two follow-ups are
//! deliberately out of scope:
//!
//! - **Merging sibling axis queries.** N axis path queries whose paths
//!   differ at one segment are exactly the branched-proof case; merging
//!   them into one envelope belongs with that shape.
//! - **Embedding axis queries inside a `PathQuery`'s subquery
//!   branches**, so one proof could mix key-selected and axis-ordered
//!   layers. That needs the general proof generator to carry a
//!   secondary proof where it currently carries only a secondary root
//!   attestation.

use std::fmt;

use bincode::{Decode, Encode};
use grovedb_element::indexed::{
    encode_avg_sort_key, encode_count_sort_key, encode_sum_sort_key, IndexAxis,
};
use grovedb_merk::proofs::{query::QueryItem as MerkQueryItem, Query as MerkQuery};

use crate::{operations::proof::util::hex_to_ascii, Error};

/// How an [`AxisQuery`] walks the secondary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AxisTraversal {
    /// The `k` entries starting at rank `offset` in the walk direction —
    /// the "top-k" reading (`offset = 0` for the first page).
    ///
    /// Skipping is attested from the secondary's counted subtree
    /// commitments rather than walked, so a large `offset` costs the
    /// same as a small one.
    TopK {
        /// Number of entries to return.
        k: u16,
        /// Rank the returned page starts at.
        offset: u64,
    },
    /// Every entry whose aggregate falls in the **inclusive** range
    /// `[lo, hi]`, up to `limit` entries, in the walk direction.
    ///
    /// Bounds are carried as `i128` for every axis — the same
    /// convention the aggregate-range entry points use — and are
    /// validated against the axis's own domain.
    Bounded {
        /// Inclusive lower bound on the aggregate.
        lo: i128,
        /// Inclusive upper bound on the aggregate.
        hi: i128,
        /// Maximum entries to return.
        limit: u16,
    },
}

/// What to read from one indexed tree's per-axis secondary: which axis,
/// how to walk it, and in which direction.
///
/// The axis-ordered counterpart of [`Query`](crate::Query).
///
/// `bincode` is implemented by hand rather than derived because
/// [`IndexAxis`] is a plain element-crate enum with no codec derives:
/// the axis travels as its canonical tag byte (the same byte the
/// indexed-tree element and every proof envelope use), so the encoding
/// stays stable if the enum ever gains variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// A path to an indexed tree plus the axis-ordered read to perform on
/// it — the axis counterpart of [`PathQuery`](crate::PathQuery).
///
/// The path's last segment must be an indexed-tree element; every
/// entry point fails with a typed error otherwise, rather than
/// answering from some other structure.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AxisPathQuery {
    /// Path to the indexed tree.
    pub path: Vec<Vec<u8>>,
    /// The axis-ordered read.
    pub query: AxisQuery,
}

impl Encode for AxisQuery {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), bincode::error::EncodeError> {
        self.axis.tag().encode(encoder)?;
        self.traversal.encode(encoder)?;
        self.descending.encode(encoder)
    }
}

impl<Context> Decode<Context> for AxisQuery {
    fn decode<D: bincode::de::Decoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        let tag = u8::decode(decoder)?;
        let axis = IndexAxis::try_from_tag(tag).map_err(|_| {
            bincode::error::DecodeError::OtherString(format!("unknown index axis tag {tag}"))
        })?;
        Ok(Self {
            axis,
            traversal: AxisTraversal::decode(decoder)?,
            descending: bool::decode(decoder)?,
        })
    }
}

impl<'de, Context> bincode::BorrowDecode<'de, Context> for AxisQuery {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        Self::decode(decoder)
    }
}

impl fmt::Display for AxisTraversal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AxisTraversal::TopK { k, offset } => {
                write!(f, "TopK {{ k: {k}, offset: {offset} }}")
            }
            AxisTraversal::Bounded { lo, hi, limit } => {
                write!(f, "Bounded {{ lo: {lo}, hi: {hi}, limit: {limit} }}")
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

impl fmt::Display for AxisPathQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AxisPathQuery {{ path: [")?;
        for (i, path_element) in self.path.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", hex_to_ascii(path_element))?;
        }
        write!(f, "], query: {} }}", self.query)
    }
}

impl AxisQuery {
    /// The `k` best entries on `axis`, starting at rank `offset`.
    pub const fn top_k(axis: IndexAxis, k: u16, offset: u64, descending: bool) -> Self {
        Self {
            axis,
            traversal: AxisTraversal::TopK { k, offset },
            descending,
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

    /// Reject a query that cannot describe any answer, so a caller
    /// error surfaces as a query error rather than an empty result that
    /// looks like real absence.
    ///
    /// Checked here rather than at each entry point so that the read,
    /// prove, and verify paths cannot disagree on what is well-formed —
    /// the same reason the bounds lowering below is shared.
    pub fn validate(&self) -> Result<(), Error> {
        match self.traversal {
            AxisTraversal::TopK { k, .. } => {
                if k == 0 {
                    return Err(Error::InvalidInput(
                        "axis query: `k` must be at least 1; a zero-length page selects nothing",
                    ));
                }
            }
            AxisTraversal::Bounded { lo, hi, limit } => {
                if limit == 0 {
                    return Err(Error::InvalidInput(
                        "axis query: `limit` must be at least 1; a zero-length page selects \
                         nothing",
                    ));
                }
                if lo > hi {
                    return Err(Error::InvalidInput(
                        "axis query: the bounds are inverted (`lo > hi`), so they can match \
                         nothing",
                    ));
                }
                if self.bounds_out_of_domain(lo, hi) {
                    return Err(Error::InvalidInput(
                        "axis query: the bounds fall entirely outside the axis's value \
                         domain, so they can match nothing",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whether `[lo, hi]` lies wholly outside what this axis can hold.
    /// A partial overlap is fine — it clamps in [`Self::merk_query`].
    fn bounds_out_of_domain(&self, lo: i128, hi: i128) -> bool {
        match self.axis {
            IndexAxis::Count => hi < 0 || lo > u64::MAX as i128,
            IndexAxis::Sum => hi < i64::MIN as i128 || lo > i64::MAX as i128,
            // The avg axis is ordered by the fixed-point average, whose
            // domain is the whole i128 range; nothing is out of domain.
            IndexAxis::Avg => false,
        }
    }

    /// Lower a [`AxisTraversal::Bounded`] query into the secondary's own
    /// Merk query.
    ///
    /// This is prover/verifier agreement material: both sides build the
    /// range from the request through *this* function, so they cannot
    /// drift on which secondary entries the proof is about. (Before this
    /// module, each caller carried its own copy of this lowering —
    /// exactly the kind of duplication that makes a proof format
    /// disagree with itself.)
    ///
    /// The secondary's keys are `sort_key ‖ original_key`, so an
    /// inclusive bound on the aggregate becomes a byte range that
    /// brackets every key-suffix at the boundary sort key: inclusive at
    /// `lo`, exclusive at the *successor* of `hi`. When `hi` is already
    /// the axis maximum there is no successor, and the range is
    /// open-ended instead.
    pub fn merk_query(&self) -> Result<MerkQuery, Error> {
        let AxisTraversal::Bounded { lo, hi, .. } = self.traversal else {
            return Err(Error::InvalidInput(
                "axis query: only a bounded traversal lowers to a secondary Merk query; a \
                 top-k traversal is served by the paginated primitives",
            ));
        };
        self.validate()?;

        let (lo_bytes, hi_exclusive) = match self.axis {
            IndexAxis::Count => {
                let lo = lo.max(0) as u64;
                let hi = hi.min(u64::MAX as i128) as u64;
                (
                    encode_count_sort_key(lo).to_vec(),
                    hi.checked_add(1).map(|h| encode_count_sort_key(h).to_vec()),
                )
            }
            IndexAxis::Sum => {
                let lo = lo.max(i64::MIN as i128) as i64;
                let hi = hi.min(i64::MAX as i128) as i64;
                (
                    encode_sum_sort_key(lo).to_vec(),
                    hi.checked_add(1).map(|h| encode_sum_sort_key(h).to_vec()),
                )
            }
            IndexAxis::Avg => (
                encode_avg_sort_key(lo).to_vec(),
                hi.checked_add(1).map(|h| encode_avg_sort_key(h).to_vec()),
            ),
        };

        let mut query = MerkQuery::new();
        match hi_exclusive {
            Some(hi_bytes) => query.insert_item(MerkQueryItem::Range(lo_bytes..hi_bytes)),
            None => query.insert_item(MerkQueryItem::RangeFrom(lo_bytes..)),
        }
        query.left_to_right = !self.descending;
        Ok(query)
    }

    /// The entry cap this query asks for, whichever traversal it is.
    pub const fn limit(&self) -> u16 {
        match self.traversal {
            AxisTraversal::TopK { k, .. } => k,
            AxisTraversal::Bounded { limit, .. } => limit,
        }
    }
}

impl AxisPathQuery {
    /// A path plus an axis-ordered read.
    pub const fn new(path: Vec<Vec<u8>>, query: AxisQuery) -> Self {
        Self { path, query }
    }

    /// The `k` best entries on `axis` at `path`, starting at rank
    /// `offset`.
    pub const fn top_k(
        path: Vec<Vec<u8>>,
        axis: IndexAxis,
        k: u16,
        offset: u64,
        descending: bool,
    ) -> Self {
        Self::new(path, AxisQuery::top_k(axis, k, offset, descending))
    }

    /// Every entry at `path` whose `axis` aggregate is in `[lo, hi]`,
    /// up to `limit`.
    pub const fn bounded(
        path: Vec<Vec<u8>>,
        axis: IndexAxis,
        lo: i128,
        hi: i128,
        limit: u16,
        descending: bool,
    ) -> Self {
        Self::new(path, AxisQuery::bounded(axis, lo, hi, limit, descending))
    }

    /// Borrowed path segments, the shape the execution entry points
    /// take.
    pub fn path_refs(&self) -> Vec<&[u8]> {
        self.path.iter().map(|segment| segment.as_slice()).collect()
    }

    /// See [`AxisQuery::validate`]; additionally rejects an empty path,
    /// which cannot name an indexed tree.
    pub fn validate(&self) -> Result<(), Error> {
        if self.path.is_empty() {
            return Err(Error::InvalidPath(
                "an axis path query's path cannot be empty: its last segment must be an \
                 indexed-tree element"
                    .to_string(),
            ));
        }
        self.query.validate()
    }
}
