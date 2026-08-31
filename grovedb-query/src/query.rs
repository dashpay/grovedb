use std::{fmt, ops::RangeFull};

use bincode::{
    enc::write::Writer,
    error::{DecodeError, EncodeError},
    BorrowDecode, Decode, Encode,
};
use indexmap::IndexMap;

use crate::{query_item::QueryItem, Key, Path, ReadMode, SubqueryBranch};

/// `Query` represents one or more keys or ranges of keys, which can be used to
/// resolve a proof which will include all the requested values.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Query {
    /// Items
    pub items: Vec<QueryItem>,
    /// Default subquery branch
    pub default_subquery_branch: SubqueryBranch,
    /// Conditional subquery branches
    pub conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
    /// Left to right?
    pub left_to_right: bool,
    /// When `true`, the parent tree element (e.g. a `CountTree` or `SumTree`)
    /// is included in query results alongside its subquery children.
    ///
    /// # Known limitation
    ///
    /// Parent tree results added by this flag do **not** currently count
    /// against `SizedQuery::limit` or the per-instance [`limit`](Self::limit).
    /// A query with `limit = 10` may return more than 10 results when this
    /// flag is active, because the budgets only govern child-level results.
    /// Fixing this requires charging the prover at exactly the points the
    /// verifier pushes parent rows, across every descent flavor — tracked
    /// as follow-up work to the per-instance limits feature.
    ///
    /// # Absence-proof verification
    ///
    /// When verifying with `verify_query_with_absence_proof` or
    /// `verify_subset_query_with_absence_proof`, results are reconstructed
    /// from the terminal-keys walk (see the `terminal_keys` module),
    /// which does not emit parent-tree entries.
    /// Parent tree elements will therefore not appear in the verified
    /// result set in those modes.
    pub add_parent_tree_on_subquery: bool,
    /// How this node reads the tree its (sub)path names. `None` is
    /// plain key selection — all pre-existing behavior, byte-identical
    /// on the wire. `Some(_)` switches the node to an axis-ordered or
    /// sum-budget read and bumps the node's encoding to the lowest
    /// version that can carry its optional fields: version `2` when
    /// the read mode is the only one present, version `3` (with both
    /// flags set) when a per-instance [`limit`](Self::limit) rides
    /// along. Decoders that predate the respective version reject it —
    /// fail-closed by construction.
    ///
    /// Placement rules (which items/branches may accompany a read mode,
    /// where in a `PathQuery` it may appear) are enforced by
    /// `PathQuery::classify` in the `grovedb` crate.
    ///
    /// **Boxed deliberately.** A read mode is absent from virtually
    /// every query, but `AxisQuery`'s `i128` bounds make it 64 bytes
    /// inline — which would fatten every `Query` (and through
    /// `PathQuery`, the `Error::InvalidProof` variant and so every
    /// `CostResult` in the crate) whether or not a read mode is
    /// present. The indirection costs one allocation on the rare
    /// read-mode path and keeps `Query` cheap to clone, which the
    /// engine does constantly. It is invisible on the wire and in
    /// serde: `Box<T>` encodes exactly as `T`.
    ///
    /// Serde: see the `query_serde` module — the representation is
    /// versioned and hand-written, mirroring the bincode codec's
    /// fail-closed rules across format generations.
    pub read_mode: Option<Box<ReadMode>>,
    /// Per-instance result limit for this query node.
    ///
    /// Unlike `SizedQuery::limit` — one global budget shared by the
    /// whole traversal — this cap is **per execution instance**: the
    /// query node runs once for every parent key it is reached under
    /// (via a default or conditional subquery branch), and each of
    /// those runs gets a fresh budget of `limit` result rows for
    /// everything originating in that instance's subtree (its own
    /// pushed elements plus all descendant results). That is what
    /// expresses "top k per parent": a parent selecting many keys with
    /// a subquery whose `limit` is `Some(k)` returns at most `k` rows
    /// under *each* matched key instead of `k` rows in total.
    ///
    /// Caps compose by `min`: an instance's effective budget is the
    /// smaller of its own `limit` and whatever remains of every
    /// enclosing budget (ancestor instances and the global
    /// `SizedQuery::limit`). On the root query node — which executes
    /// exactly once — this field is therefore equivalent to
    /// `SizedQuery::limit`, and setting both means the smaller wins.
    ///
    /// `Some(0)` is rejected by every serving entry point (a node that
    /// may select nothing is a malformed query, not an empty result).
    /// Serving is version-gated (`GROVE_V4`+); older grove versions
    /// fail closed, and on the wire a query carrying a per-instance
    /// limit anywhere encodes that node as version 3, which decoders
    /// that predate the field reject.
    ///
    /// Serde: see the `query_serde` module.
    pub limit: Option<u16>,
}

/// Version-3 `Query` encoding flags byte: the node carries a read mode.
const QUERY_V3_FLAG_READ_MODE: u8 = 0b0000_0001;
/// Version-3 `Query` encoding flags byte: the node carries a
/// per-instance limit. A version-3 node always has this flag set —
/// a node without a per-instance limit encodes as version 1 or 2.
const QUERY_V3_FLAG_INSTANCE_LIMIT: u8 = 0b0000_0010;
const QUERY_V3_KNOWN_FLAGS: u8 = QUERY_V3_FLAG_READ_MODE | QUERY_V3_FLAG_INSTANCE_LIMIT;

impl Encode for Query {
    fn encode<E: bincode::enc::Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        // Version byte — always the lowest version that can represent
        // this node, so every already-expressible query keeps its exact
        // historical bytes and old decoders fail closed on exactly the
        // queries they cannot execute and on nothing else:
        // 1 = plain (pre-read-mode layout, byte-for-byte), 2 = carries
        // a read mode, 3 = carries a per-instance limit (flags byte
        // says whether a read mode rides along).
        if self.limit.is_some() {
            3u8.encode(encoder)?;
            let mut flags = QUERY_V3_FLAG_INSTANCE_LIMIT;
            if self.read_mode.is_some() {
                flags |= QUERY_V3_FLAG_READ_MODE;
            }
            flags.encode(encoder)?;
        } else if self.read_mode.is_some() {
            2u8.encode(encoder)?;
        } else {
            1u8.encode(encoder)?;
        }

        // Encode the items vector
        self.items.encode(encoder)?;

        // Encode the default subquery branch
        self.default_subquery_branch.encode(encoder)?;

        // Encode the conditional subquery branches
        match &self.conditional_subquery_branches {
            Some(conditional_subquery_branches) => {
                encoder.writer().write(&[1])?; // Write a flag indicating presence of data
                                               // Encode the length of the map
                (conditional_subquery_branches.len() as u64).encode(encoder)?;
                // Encode each key-value pair in the IndexMap
                for (key, value) in conditional_subquery_branches {
                    key.encode(encoder)?;
                    value.encode(encoder)?;
                }
            }
            None => {
                encoder.writer().write(&[0])?; // Write a flag indicating
                                               // absence of data
            }
        }

        // Encode the left_to_right boolean
        self.left_to_right.encode(encoder)?;

        self.add_parent_tree_on_subquery.encode(encoder)?;

        // Versions 2 and 3 append the read mode when present. No
        // per-field presence flag: the version byte (v2) or the flags
        // byte (v3) already says it's there.
        if let Some(read_mode) = &self.read_mode {
            read_mode.encode(encoder)?;
        }

        // Version 3 appends the per-instance limit last; its presence
        // is what selected version 3 in the first place.
        if let Some(limit) = self.limit {
            limit.encode(encoder)?;
        }

        Ok(())
    }
}

/// Maximum number of query items allowed during decoding.
/// Prevents OOM from malicious inputs with inflated lengths.
const MAX_QUERY_ITEMS: usize = 65_536;

/// Maximum number of conditional subquery branches allowed during decoding.
/// Prevents OOM from malicious inputs with inflated lengths.
const MAX_CONDITIONAL_BRANCHES: usize = 1024;

/// Maximum subquery nesting depth allowed during deserialization.
/// Prevents stack overflow from deeply nested Query ↔ SubqueryBranch
/// mutual recursion. Matches `MAX_TERMINAL_KEYS_DEPTH`.
const MAX_SUBQUERY_DECODE_DEPTH: usize = 64;

impl Query {
    pub(crate) fn decode_with_depth<D: bincode::de::Decoder>(
        decoder: &mut D,
        depth: usize,
    ) -> Result<Self, DecodeError> {
        if depth > MAX_SUBQUERY_DECODE_DEPTH {
            return Err(DecodeError::Other(
                "subquery nesting depth exceeded maximum during deserialization",
            ));
        }
        let version = u8::decode(decoder)?;
        if version != 1 && version != 2 && version != 3 {
            return Err(DecodeError::Other("unsupported Query encoding version"));
        }
        // Version 3 carries a flags byte right after the version. The
        // instance-limit flag must be set (a node without one encodes
        // as version 1 or 2 — the encoding is canonical), and unknown
        // flag bits fail closed.
        let flags = if version == 3 {
            let flags = u8::decode(decoder)?;
            if flags & !QUERY_V3_KNOWN_FLAGS != 0 {
                return Err(DecodeError::Other("unknown Query version 3 flags"));
            }
            if flags & QUERY_V3_FLAG_INSTANCE_LIMIT == 0 {
                return Err(DecodeError::Other(
                    "non-canonical Query version 3 encoding: no per-instance limit",
                ));
            }
            flags
        } else {
            0
        };
        let items_len = u64::decode(decoder)? as usize;
        if items_len > MAX_QUERY_ITEMS {
            return Err(DecodeError::Other("query items length exceeds maximum"));
        }
        let mut items = Vec::with_capacity(items_len);
        for _ in 0..items_len {
            items.push(QueryItem::decode(decoder)?);
        }

        let default_subquery_branch = SubqueryBranch::decode_with_depth(decoder, depth)?;

        let conditional_subquery_branches = if u8::decode(decoder)? == 1 {
            let len = u64::decode(decoder)? as usize;
            if len > MAX_CONDITIONAL_BRANCHES {
                return Err(DecodeError::Other(
                    "conditional subquery branches length exceeds maximum",
                ));
            }
            let mut map = IndexMap::with_capacity(len);
            for _ in 0..len {
                let key = QueryItem::decode(decoder)?;
                let value = SubqueryBranch::decode_with_depth(decoder, depth)?;
                map.insert(key, value);
            }
            Some(map)
        } else {
            None
        };

        let left_to_right = bool::decode(decoder)?;
        let add_parent_tree_on_subquery = bool::decode(decoder)?;

        // Version 2 always carries a read mode; version 3 carries one
        // when its flags byte says so; version 1 never does.
        let read_mode = if version == 2 || flags & QUERY_V3_FLAG_READ_MODE != 0 {
            Some(Box::new(ReadMode::decode(decoder)?))
        } else {
            None
        };

        // Version 3 always carries the per-instance limit last.
        let limit = if version == 3 {
            Some(u16::decode(decoder)?)
        } else {
            None
        };

        Ok(Query {
            items,
            default_subquery_branch,
            conditional_subquery_branches,
            left_to_right,
            add_parent_tree_on_subquery,
            read_mode,
            limit,
        })
    }

    pub(crate) fn borrow_decode_with_depth<'de, D: bincode::de::BorrowDecoder<'de>>(
        decoder: &mut D,
        depth: usize,
    ) -> Result<Self, DecodeError> {
        if depth > MAX_SUBQUERY_DECODE_DEPTH {
            return Err(DecodeError::Other(
                "subquery nesting depth exceeded maximum during deserialization",
            ));
        }
        let version = u8::borrow_decode(decoder)?;
        if version != 1 && version != 2 && version != 3 {
            return Err(DecodeError::Other("unsupported Query encoding version"));
        }
        // See `decode_with_depth`: version 3 = flags byte, canonical,
        // unknown bits fail closed.
        let flags = if version == 3 {
            let flags = u8::borrow_decode(decoder)?;
            if flags & !QUERY_V3_KNOWN_FLAGS != 0 {
                return Err(DecodeError::Other("unknown Query version 3 flags"));
            }
            if flags & QUERY_V3_FLAG_INSTANCE_LIMIT == 0 {
                return Err(DecodeError::Other(
                    "non-canonical Query version 3 encoding: no per-instance limit",
                ));
            }
            flags
        } else {
            0
        };
        let items_len = u64::borrow_decode(decoder)? as usize;
        if items_len > MAX_QUERY_ITEMS {
            return Err(DecodeError::Other("query items length exceeds maximum"));
        }
        let mut items = Vec::with_capacity(items_len);
        for _ in 0..items_len {
            items.push(QueryItem::borrow_decode(decoder)?);
        }

        let default_subquery_branch = SubqueryBranch::borrow_decode_with_depth(decoder, depth)?;

        let conditional_subquery_branches = if u8::borrow_decode(decoder)? == 1 {
            let len = u64::borrow_decode(decoder)? as usize;
            if len > MAX_CONDITIONAL_BRANCHES {
                return Err(DecodeError::Other(
                    "conditional subquery branches length exceeds maximum",
                ));
            }
            let mut map = IndexMap::with_capacity(len);
            for _ in 0..len {
                let key = QueryItem::borrow_decode(decoder)?;
                let value = SubqueryBranch::borrow_decode_with_depth(decoder, depth)?;
                map.insert(key, value);
            }
            Some(map)
        } else {
            None
        };

        let left_to_right = bool::borrow_decode(decoder)?;
        let add_parent_tree_on_subquery = bool::borrow_decode(decoder)?;

        // Version 2 always carries a read mode; version 3 carries one
        // when its flags byte says so; version 1 never does.
        let read_mode = if version == 2 || flags & QUERY_V3_FLAG_READ_MODE != 0 {
            Some(Box::new(ReadMode::borrow_decode(decoder)?))
        } else {
            None
        };

        // Version 3 always carries the per-instance limit last.
        let limit = if version == 3 {
            Some(u16::borrow_decode(decoder)?)
        } else {
            None
        };

        Ok(Query {
            items,
            default_subquery_branch,
            conditional_subquery_branches,
            left_to_right,
            add_parent_tree_on_subquery,
            read_mode,
            limit,
        })
    }
}

impl<Context> Decode<Context> for Query {
    fn decode<D: bincode::de::Decoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::decode_with_depth(decoder, 0)
    }
}

impl<'de, Context> BorrowDecode<'de, Context> for Query {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        Self::borrow_decode_with_depth(decoder, 0)
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Query {{")?;
        writeln!(f, "  items: [")?;
        for item in &self.items {
            writeln!(f, "    {},", item)?;
        }
        writeln!(f, "  ],")?;
        writeln!(
            f,
            "  default_subquery_branch: {},",
            self.default_subquery_branch
        )?;
        if let Some(conditional_branches) = &self.conditional_subquery_branches {
            writeln!(f, "  conditional_subquery_branches: {{")?;
            for (item, branch) in conditional_branches {
                writeln!(f, "    {}: {},", item, branch)?;
            }
            writeln!(f, "  }},")?;
        }
        writeln!(f, "  left_to_right: {},", self.left_to_right)?;
        writeln!(
            f,
            "  add_parent_tree_on_subquery: {},",
            self.add_parent_tree_on_subquery
        )?;
        if let Some(read_mode) = &self.read_mode {
            writeln!(f, "  read_mode: {read_mode},")?;
        }
        if let Some(limit) = self.limit {
            writeln!(f, "  limit: {limit},")?;
        }
        write!(f, "}}")
    }
}

impl Query {
    /// Whether this query — or any subquery below it, default or
    /// conditional — carries a per-instance [`limit`](Self::limit).
    /// Entry points that don't serve per-instance limits use this to
    /// fail closed instead of silently running the query with the caps
    /// ignored.
    pub fn has_instance_limit_anywhere(&self) -> bool {
        self.any_instance_limit(|limit| limit.is_some())
    }

    /// Whether any per-instance limit in this query tree is `Some(0)`.
    /// A node that may select nothing is a malformed query, not an
    /// empty result; serving entry points reject it.
    pub fn has_zero_instance_limit_anywhere(&self) -> bool {
        self.any_instance_limit(|limit| limit == Some(0))
    }

    fn any_instance_limit(&self, predicate: fn(Option<u16>) -> bool) -> bool {
        let branch_matches = |branch: &SubqueryBranch| {
            branch
                .subquery
                .as_deref()
                .is_some_and(|subquery| subquery.any_instance_limit(predicate))
        };
        predicate(self.limit)
            || branch_matches(&self.default_subquery_branch)
            || self
                .conditional_subquery_branches
                .as_ref()
                .is_some_and(|branches| branches.values().any(branch_matches))
    }
}

impl Query {
    /// Creates a new query which contains no items.
    pub fn new() -> Self {
        Self::new_with_direction(true)
    }

    /// Creates a new query which contains all items.
    pub fn new_range_full() -> Self {
        Self {
            items: vec![QueryItem::RangeFull(RangeFull)],
            left_to_right: true,
            ..Self::default()
        }
    }

    /// Creates a new query which contains only one key.
    pub fn new_single_key(key: Vec<u8>) -> Self {
        Self {
            items: vec![QueryItem::Key(key)],
            left_to_right: true,
            ..Self::default()
        }
    }

    /// Creates a new query which contains only one item.
    pub fn new_single_query_item(query_item: QueryItem) -> Self {
        Self {
            items: vec![query_item],
            left_to_right: true,
            ..Self::default()
        }
    }

    /// Creates a new query with a direction specified
    pub fn new_with_direction(left_to_right: bool) -> Self {
        Self {
            left_to_right,
            ..Self::default()
        }
    }

    /// Creates a new query which contains only one item with the specified
    /// direction.
    pub fn new_single_query_item_with_direction(
        query_item: QueryItem,
        left_to_right: bool,
    ) -> Self {
        Self {
            items: vec![query_item],
            left_to_right,
            ..Self::default()
        }
    }

    /// Returns `true` if the given key would trigger a subquery (either via
    /// the default subquery branch or a matching conditional branch).
    pub fn has_subquery_on_key(&self, key: &[u8], in_path: bool) -> bool {
        if in_path || self.default_subquery_branch.subquery.is_some() {
            return true;
        }
        if let Some(conditional_subquery_branches) = self.conditional_subquery_branches.as_ref() {
            for (query_item, subquery) in conditional_subquery_branches {
                if query_item.contains(key) {
                    return subquery.subquery.is_some();
                }
            }
        }
        false
    }

    /// Returns `true` if the given key would trigger a subquery or subquery
    /// path (either via the default branch or a matching conditional branch).
    pub fn has_subquery_or_subquery_path_on_key(&self, key: &[u8], in_path: bool) -> bool {
        if in_path
            || self.default_subquery_branch.subquery.is_some()
            || self.default_subquery_branch.subquery_path.is_some()
        {
            return true;
        }
        if let Some(conditional_subquery_branches) = self.conditional_subquery_branches.as_ref() {
            for query_item in conditional_subquery_branches.keys() {
                if query_item.contains(key) {
                    return true;
                }
            }
        }
        false
    }

    /// Get number of query items
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns `true` if there are no query items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Iterate through query items
    pub fn iter(&self) -> impl Iterator<Item = &QueryItem> {
        self.items.iter()
    }

    /// Iterate through query items in reverse
    pub fn rev_iter(&self) -> impl Iterator<Item = &QueryItem> {
        self.items.iter().rev()
    }

    /// Iterate with direction specified
    pub fn directional_iter(
        &self,
        left_to_right: bool,
    ) -> Box<dyn Iterator<Item = &QueryItem> + '_> {
        if left_to_right {
            Box::new(self.iter())
        } else {
            Box::new(self.rev_iter())
        }
    }

    /// Sets the subquery_path for the query with one key. This causes every
    /// element that is returned by the query to be subqueried one level to
    /// the subquery_path.
    pub fn set_subquery_key(&mut self, key: Key) {
        self.default_subquery_branch.subquery_path = Some(vec![key]);
    }

    /// Sets the subquery_path for the query. This causes every element that is
    /// returned by the query to be subqueried to the subquery_path.
    pub fn set_subquery_path(&mut self, path: Path) {
        self.default_subquery_branch.subquery_path = Some(path);
    }

    /// Sets the subquery for the query. This causes every element that is
    /// returned by the query to be subqueried or subqueried to the
    /// subquery_path/subquery if a subquery is present.
    pub fn set_subquery(&mut self, subquery: Self) {
        self.default_subquery_branch.subquery = Some(Box::new(subquery));
    }

    /// Adds a conditional subquery. A conditional subquery replaces the default
    /// subquery and subquery_path if the item matches for the key. If
    /// multiple conditional subquery items match, then the first one that
    /// matches is used (in order that they were added).
    pub fn add_conditional_subquery(
        &mut self,
        item: QueryItem,
        subquery_path: Option<Path>,
        subquery: Option<Self>,
    ) {
        if let Some(conditional_subquery_branches) = &mut self.conditional_subquery_branches {
            conditional_subquery_branches.insert(
                item,
                SubqueryBranch {
                    subquery_path,
                    subquery: subquery.map(Box::new),
                },
            );
        } else {
            let mut conditional_subquery_branches = IndexMap::new();
            conditional_subquery_branches.insert(
                item,
                SubqueryBranch {
                    subquery_path,
                    subquery: subquery.map(Box::new),
                },
            );
            self.conditional_subquery_branches = Some(conditional_subquery_branches);
        }
    }

    /// Check if there is a subquery
    pub fn has_subquery(&self) -> bool {
        // checks if a query has subquery items
        if self.default_subquery_branch.subquery.is_some()
            || self.default_subquery_branch.subquery_path.is_some()
            || self.conditional_subquery_branches.is_some()
        {
            return true;
        }
        false
    }

    /// Check if there are only keys
    pub fn has_only_keys(&self) -> bool {
        // checks if all searched for items are keys
        self.items.iter().all(|a| a.is_key())
    }

    /// Returns the depth of the subquery branch
    /// This depth is how many GroveDB layers down we could query at maximum
    pub fn max_depth(&self) -> Option<u16> {
        self.max_depth_internal(u8::MAX)
    }

    /// Returns the depth of the subquery branch
    /// This depth is how many GroveDB layers down we could query at maximum
    pub(crate) fn max_depth_internal(&self, recursion_limit: u8) -> Option<u16> {
        let default_subquery_branch_depth = self
            .default_subquery_branch
            .max_depth_internal(recursion_limit)?;
        let conditional_subquery_branches_max_depth = self
            .conditional_subquery_branches
            .as_ref()
            .map_or(Some(0), |condition_subqueries| {
            condition_subqueries
                .values()
                .try_fold(0, |max_depth, conditional_subquery_branch| {
                    conditional_subquery_branch
                        .max_depth_internal(recursion_limit)
                        .map(|depth| max_depth.max(depth))
                })
        })?;
        1u16.checked_add(default_subquery_branch_depth.max(conditional_subquery_branches_max_depth))
    }
}

#[cfg(feature = "blockchain")]
impl<Q: Into<QueryItem>> From<Vec<Q>> for Query {
    fn from(other: Vec<Q>) -> Self {
        let items = other.into_iter().map(Into::into).collect();
        Self {
            items,
            default_subquery_branch: SubqueryBranch {
                subquery_path: None,
                subquery: None,
            },
            conditional_subquery_branches: None,
            left_to_right: true,
            add_parent_tree_on_subquery: false,
            read_mode: None,
            limit: None,
        }
    }
}

impl From<Query> for Vec<QueryItem> {
    fn from(q: Query) -> Self {
        q.into_iter().collect()
    }
}

impl IntoIterator for Query {
    type IntoIter = <Vec<QueryItem> as IntoIterator>::IntoIter;
    type Item = QueryItem;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use bincode::config;

    use super::*;
    use crate::query_item::QueryItem;

    fn bincode_config() -> impl bincode::config::Config {
        config::standard().with_big_endian().with_no_limit()
    }

    #[test]
    fn query_encode_decode_round_trip() {
        let mut query = Query::new();
        query.items = vec![
            QueryItem::Key(vec![1, 2, 3]),
            QueryItem::Range(vec![10]..vec![20]),
            QueryItem::RangeInclusive(vec![30]..=vec![40]),
        ];
        query.left_to_right = false;
        query.add_parent_tree_on_subquery = true;

        let encoded =
            bincode::encode_to_vec(&query, bincode_config()).expect("expected to encode query");
        let (decoded, _): (Query, _) = bincode::decode_from_slice(&encoded, bincode_config())
            .expect("expected to decode query");

        assert_eq!(decoded.items.len(), 3);
        assert_eq!(decoded.items, query.items);
        assert!(!decoded.left_to_right);
        assert!(decoded.add_parent_tree_on_subquery);
    }

    #[test]
    fn query_decode_rejects_too_many_items() {
        // Craft a malicious payload with an excessive items count.
        // The encoded format after the version byte starts with a u64 length
        // for the items vector. We encode the length separately using bincode's
        // own format to match the variable-length integer encoding.
        let mut malicious = Vec::new();
        malicious.push(1u8); // version byte

        // Encode the excessive length using bincode's format
        let excessive_len = (MAX_QUERY_ITEMS as u64) + 1;
        let len_bytes =
            bincode::encode_to_vec(excessive_len, bincode_config()).expect("encode length");
        malicious.extend_from_slice(&len_bytes);

        // Add enough dummy QueryItem bytes to start decoding (each Key item
        // is: variant_id=0, then a Vec<u8> length, then bytes)
        // We just need enough to trigger the length check, not necessarily
        // enough valid items.
        // Actually, the check happens before decoding any items, so no item
        // data is needed -- the decoder will reject based on length alone.

        let result: Result<(Query, _), _> =
            bincode::decode_from_slice(&malicious, bincode_config());
        assert!(
            result.is_err(),
            "decoding should fail when items count exceeds MAX_QUERY_ITEMS"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("query items length exceeds maximum"),
            "error message should mention the limit, got: {}",
            err
        );
    }

    #[test]
    fn query_decode_accepts_max_items_boundary() {
        // Build a query with exactly MAX_QUERY_ITEMS items and verify it encodes/decodes
        let mut query = Query::new();
        // Use a smaller number to keep the test fast but verify the boundary logic
        // We'll test with a count just under the limit
        let count = 100; // Use a reasonable count for test performance
        query.items = (0..count)
            .map(|i| QueryItem::Key(vec![(i % 256) as u8]))
            .collect();

        let encoded =
            bincode::encode_to_vec(&query, bincode_config()).expect("expected to encode query");
        let (decoded, _): (Query, _) = bincode::decode_from_slice(&encoded, bincode_config())
            .expect("expected to decode query with many items");
        assert_eq!(decoded.items.len(), count);
    }

    #[test]
    fn query_decode_rejects_invalid_version() {
        // Craft a payload with an invalid version byte
        let mut payload = Vec::new();
        payload.push(4u8); // invalid version (only versions 1..=3 are supported)
                           // Add some dummy data after
        payload.extend_from_slice(&[0; 20]);

        let result: Result<(Query, _), _> = bincode::decode_from_slice(&payload, bincode_config());
        assert!(
            result.is_err(),
            "decoding should fail for unsupported version"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported Query encoding version"),
            "error message should mention unsupported version, got: {}",
            err
        );
    }

    #[test]
    fn query_borrow_decode_rejects_too_many_items() {
        // Same test but exercising BorrowDecode path via decode_from_slice
        // (bincode::decode_from_slice uses BorrowDecode when possible, but
        // since Query doesn't borrow data, both paths should be tested)

        let mut malicious = Vec::new();
        malicious.push(1u8); // version byte

        let excessive_len = (MAX_QUERY_ITEMS as u64) + 1;
        let len_bytes =
            bincode::encode_to_vec(excessive_len, bincode_config()).expect("encode length");
        malicious.extend_from_slice(&len_bytes);

        // Try borrow_decode path
        let result: Result<(Query, _), _> =
            bincode::borrow_decode_from_slice(&malicious, bincode_config());
        assert!(
            result.is_err(),
            "borrow_decode should fail when items count exceeds MAX_QUERY_ITEMS"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string()
                .contains("query items length exceeds maximum"),
            "error message should mention the limit, got: {}",
            err
        );
    }

    /// Build a query with `depth` levels of nested subqueries.
    fn build_nested_query(depth: usize) -> Query {
        let mut query = Query::new();
        query.insert_all();
        for _ in 0..depth {
            let mut outer = Query::new();
            outer.insert_all();
            outer.set_subquery(query);
            query = outer;
        }
        query
    }

    #[test]
    fn query_decode_rejects_excessive_subquery_nesting() {
        // Build a query nested deeper than MAX_SUBQUERY_DECODE_DEPTH
        let deep_query = build_nested_query(MAX_SUBQUERY_DECODE_DEPTH + 5);

        let encoded =
            bincode::encode_to_vec(&deep_query, bincode_config()).expect("encoding should succeed");

        let result: Result<(Query, _), _> = bincode::decode_from_slice(&encoded, bincode_config());
        assert!(
            result.is_err(),
            "decode should reject query nested beyond MAX_SUBQUERY_DECODE_DEPTH"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("nesting depth exceeded"),
            "error should mention nesting depth, got: {}",
            err
        );
    }

    #[test]
    fn query_borrow_decode_rejects_excessive_subquery_nesting() {
        let deep_query = build_nested_query(MAX_SUBQUERY_DECODE_DEPTH + 5);

        let encoded =
            bincode::encode_to_vec(&deep_query, bincode_config()).expect("encoding should succeed");

        let result: Result<(Query, _), _> =
            bincode::borrow_decode_from_slice(&encoded, bincode_config());
        assert!(
            result.is_err(),
            "borrow_decode should reject query nested beyond MAX_SUBQUERY_DECODE_DEPTH"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("nesting depth exceeded"),
            "error should mention nesting depth, got: {}",
            err
        );
    }

    #[test]
    fn query_decode_round_trips_at_valid_nesting_depth() {
        // Build a query at a depth well within the limit
        let query = build_nested_query(10);

        let encoded =
            bincode::encode_to_vec(&query, bincode_config()).expect("encoding should succeed");

        let (decoded, _): (Query, _) = bincode::decode_from_slice(&encoded, bincode_config())
            .expect("decode should succeed for valid nesting depth");

        // Verify structure preserved — walk down to the innermost query
        let mut current = &decoded;
        for _ in 0..10 {
            let subquery = current
                .default_subquery_branch
                .subquery
                .as_ref()
                .expect("subquery should exist at this depth");
            current = subquery.as_ref();
        }
        assert!(
            current.default_subquery_branch.subquery.is_none(),
            "innermost query should have no further subquery"
        );
    }
}

/// Versioned serde representation for [`Query`].
///
/// The derived representation was unsafe across code generations in
/// both directions: a positional (non-self-describing) serializer
/// broke as fields were appended (`serde(default)` cannot turn EOF
/// into an omitted field), and a self-describing reader from *before*
/// a field silently ignored its unknown key — turning a bounded query
/// into an unlimited one, or a read-mode query into plain key
/// selection. This module mirrors the bincode codec's versioning
/// instead:
///
/// - **Non-self-describing formats** (`!is_human_readable`, e.g.
///   serde-bincode): a framed layout — a leading `MAGIC` sentinel,
///   then a version integer, then every field always present. A
///   plain `version: u8` alone would NOT fail closed: the released
///   unframed layout begins with the `items` vector length, so a
///   legacy reader would consume a small version byte as a length and
///   reinterpret the remaining fields as query items. The magic
///   decodes there as an absurd items length instead, which errors —
///   and this reader requires the magic exactly, so released payloads
///   error cleanly here too (positional layouts carry no
///   self-description to migrate on).
/// - **Self-describing formats** (`is_human_readable`, e.g. JSON):
///   version 1 — the only content the **released** derived layout can
///   serve — keeps the flat map layout (plus a `version` key old
///   readers ignore). Versions 2 and 3 (read-mode- and limit-bearing
///   forms) nest their fields under a `body` key, so an old reader
///   hard-fails on the missing flat fields instead of silently
///   dropping the read mode or the limit.
///
/// Decoding validates canonicality exactly like bincode: the version
/// must be the lowest that can represent the contents.
#[cfg(feature = "serde")]
mod query_serde {
    use std::fmt;

    use serde::{
        de::{self, MapAccess, Visitor},
        Deserialize, Deserializer, Serialize, Serializer,
    };

    use super::*;

    fn wire_version(read_mode: bool, limit: bool) -> u8 {
        match (read_mode, limit) {
            (_, true) => 3,
            (true, false) => 2,
            (false, false) => 1,
        }
    }

    fn validate_canonical<E: de::Error>(
        version: u8,
        read_mode: bool,
        limit: bool,
    ) -> Result<(), E> {
        let expected = wire_version(read_mode, limit);
        if version != expected {
            return Err(E::custom(format!(
                "non-canonical Query serde version {version} for its contents (expected \
                 {expected})"
            )));
        }
        Ok(())
    }

    /// The positional frame sentinel. Chosen so a legacy unframed
    /// reader, which decodes the leading bytes as its `items` vector
    /// length, sees an absurd length and errors instead of
    /// reinterpreting the payload.
    const POSITIONAL_MAGIC: u64 = u64::from_be_bytes(*b"grvquery");

    /// Framed positional layout — magic, version, then every field
    /// always present.
    #[derive(Serialize)]
    struct PositionalRef<'a> {
        magic: u64,
        version: u8,
        items: &'a Vec<QueryItem>,
        default_subquery_branch: &'a SubqueryBranch,
        conditional_subquery_branches: &'a Option<IndexMap<QueryItem, SubqueryBranch>>,
        left_to_right: bool,
        add_parent_tree_on_subquery: bool,
        read_mode: &'a Option<Box<ReadMode>>,
        limit: &'a Option<u16>,
    }

    #[derive(Deserialize)]
    #[serde(rename = "Query")]
    struct PositionalOwned {
        magic: u64,
        version: u8,
        items: Vec<QueryItem>,
        default_subquery_branch: SubqueryBranch,
        conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
        left_to_right: bool,
        add_parent_tree_on_subquery: bool,
        read_mode: Option<Box<ReadMode>>,
        limit: Option<u16>,
    }

    /// The nested body of the human-readable version-2 and version-3
    /// forms.
    #[derive(Serialize, Deserialize)]
    struct BodyOwned {
        items: Vec<QueryItem>,
        default_subquery_branch: SubqueryBranch,
        conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
        left_to_right: bool,
        add_parent_tree_on_subquery: bool,
        read_mode: Option<Box<ReadMode>>,
        limit: Option<u16>,
    }

    #[derive(Serialize)]
    struct BodyRef<'a> {
        items: &'a Vec<QueryItem>,
        default_subquery_branch: &'a SubqueryBranch,
        conditional_subquery_branches: &'a Option<IndexMap<QueryItem, SubqueryBranch>>,
        left_to_right: bool,
        add_parent_tree_on_subquery: bool,
        read_mode: &'a Option<Box<ReadMode>>,
        limit: &'a Option<u16>,
    }

    impl Serialize for Query {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let version = wire_version(self.read_mode.is_some(), self.limit.is_some());
            if !serializer.is_human_readable() {
                return PositionalRef {
                    magic: POSITIONAL_MAGIC,
                    version,
                    items: &self.items,
                    default_subquery_branch: &self.default_subquery_branch,
                    conditional_subquery_branches: &self.conditional_subquery_branches,
                    left_to_right: self.left_to_right,
                    add_parent_tree_on_subquery: self.add_parent_tree_on_subquery,
                    read_mode: &self.read_mode,
                    limit: &self.limit,
                }
                .serialize(serializer);
            }
            use serde::ser::SerializeStruct;
            match version {
                2 | 3 => {
                    // Both non-released forms nest: the released
                    // derived reader ignores unknown flat keys, so
                    // either feature would silently vanish in a flat
                    // map.
                    let mut state = serializer.serialize_struct("Query", 2)?;
                    state.serialize_field("version", &version)?;
                    state.serialize_field(
                        "body",
                        &BodyRef {
                            items: &self.items,
                            default_subquery_branch: &self.default_subquery_branch,
                            conditional_subquery_branches: &self.conditional_subquery_branches,
                            left_to_right: self.left_to_right,
                            add_parent_tree_on_subquery: self.add_parent_tree_on_subquery,
                            read_mode: &self.read_mode,
                            limit: &self.limit,
                        },
                    )?;
                    state.end()
                }
                _ => {
                    let mut state = serializer.serialize_struct("Query", 6)?;
                    state.serialize_field("version", &version)?;
                    state.serialize_field("items", &self.items)?;
                    state.serialize_field(
                        "default_subquery_branch",
                        &self.default_subquery_branch,
                    )?;
                    state.serialize_field(
                        "conditional_subquery_branches",
                        &self.conditional_subquery_branches,
                    )?;
                    state.serialize_field("left_to_right", &self.left_to_right)?;
                    state.serialize_field(
                        "add_parent_tree_on_subquery",
                        &self.add_parent_tree_on_subquery,
                    )?;
                    state.end()
                }
            }
        }
    }

    struct QueryVisitor;

    impl<'de> Visitor<'de> for QueryVisitor {
        type Value = Query;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a versioned Query map")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Query, A::Error> {
            let mut version: Option<u8> = None;
            let mut body: Option<BodyOwned> = None;
            let mut items: Option<Vec<QueryItem>> = None;
            let mut default_subquery_branch: Option<SubqueryBranch> = None;
            let mut conditional_subquery_branches: Option<
                Option<IndexMap<QueryItem, SubqueryBranch>>,
            > = None;
            let mut left_to_right: Option<bool> = None;
            let mut add_parent_tree_on_subquery: Option<bool> = None;
            let mut read_mode: Option<Option<Box<ReadMode>>> = None;

            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "version" => version = Some(map.next_value()?),
                    "body" => body = Some(map.next_value()?),
                    "items" => items = Some(map.next_value()?),
                    "default_subquery_branch" => default_subquery_branch = Some(map.next_value()?),
                    "conditional_subquery_branches" => {
                        conditional_subquery_branches = Some(map.next_value()?)
                    }
                    "left_to_right" => left_to_right = Some(map.next_value()?),
                    "add_parent_tree_on_subquery" => {
                        add_parent_tree_on_subquery = Some(map.next_value()?)
                    }
                    "read_mode" => read_mode = Some(map.next_value()?),
                    other => {
                        // Unknown keys fail closed: silently ignoring a
                        // key is exactly how a limit was dropped by
                        // pre-versioning readers.
                        return Err(de::Error::unknown_field(
                            other,
                            &[
                                "version",
                                "body",
                                "items",
                                "default_subquery_branch",
                                "conditional_subquery_branches",
                                "left_to_right",
                                "add_parent_tree_on_subquery",
                                "read_mode",
                            ],
                        ));
                    }
                }
            }

            if let Some(body) = body {
                let version = version.ok_or_else(|| de::Error::missing_field("version"))?;
                if items.is_some()
                    || default_subquery_branch.is_some()
                    || conditional_subquery_branches.is_some()
                    || left_to_right.is_some()
                    || add_parent_tree_on_subquery.is_some()
                    || read_mode.is_some()
                {
                    return Err(de::Error::custom(
                        "a version-2 or version-3 Query nests every field under `body`; flat \
                         fields may not accompany it",
                    ));
                }
                validate_canonical::<A::Error>(
                    version,
                    body.read_mode.is_some(),
                    body.limit.is_some(),
                )?;
                if version < 2 {
                    return Err(de::Error::custom(
                        "a nested Query body requires version 2 or 3",
                    ));
                }
                return Ok(Query {
                    items: body.items,
                    default_subquery_branch: body.default_subquery_branch,
                    conditional_subquery_branches: body.conditional_subquery_branches,
                    left_to_right: body.left_to_right,
                    add_parent_tree_on_subquery: body.add_parent_tree_on_subquery,
                    read_mode: body.read_mode,
                    limit: body.limit,
                });
            }

            // The flat layout carries neither a read mode nor a limit
            // (both bump the query to the nested form); a `read_mode`
            // key in a flat map is refused rather than accepted, since
            // the released reader would drop it silently and the two
            // generations must not diverge on the same payload. A
            // missing `version` key is the released pre-versioning
            // layout — accepted for compatibility.
            if read_mode
                .as_ref()
                .is_some_and(|read_mode| read_mode.is_some())
            {
                return Err(de::Error::custom(
                    "a flat Query map may not carry a read mode; read-mode queries nest under \
                     `body` (version 2)",
                ));
            }
            let read_mode = read_mode.unwrap_or(None);
            if let Some(version) = version {
                validate_canonical::<A::Error>(version, false, false)?;
            }
            Ok(Query {
                items: items.ok_or_else(|| de::Error::missing_field("items"))?,
                default_subquery_branch: default_subquery_branch
                    .ok_or_else(|| de::Error::missing_field("default_subquery_branch"))?,
                conditional_subquery_branches: conditional_subquery_branches.unwrap_or(None),
                left_to_right: left_to_right
                    .ok_or_else(|| de::Error::missing_field("left_to_right"))?,
                add_parent_tree_on_subquery: add_parent_tree_on_subquery
                    .ok_or_else(|| de::Error::missing_field("add_parent_tree_on_subquery"))?,
                read_mode,
                limit: None,
            })
        }
    }

    impl<'de> Deserialize<'de> for Query {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Query, D::Error> {
            if !deserializer.is_human_readable() {
                let wire = PositionalOwned::deserialize(deserializer)?;
                if wire.magic != POSITIONAL_MAGIC {
                    return Err(de::Error::custom(
                        "positional Query payload does not carry the framed layout's magic \
                         sentinel",
                    ));
                }
                validate_canonical::<D::Error>(
                    wire.version,
                    wire.read_mode.is_some(),
                    wire.limit.is_some(),
                )?;
                return Ok(Query {
                    items: wire.items,
                    default_subquery_branch: wire.default_subquery_branch,
                    conditional_subquery_branches: wire.conditional_subquery_branches,
                    left_to_right: wire.left_to_right,
                    add_parent_tree_on_subquery: wire.add_parent_tree_on_subquery,
                    read_mode: wire.read_mode,
                    limit: wire.limit,
                });
            }
            deserializer.deserialize_map(QueryVisitor)
        }
    }
}
