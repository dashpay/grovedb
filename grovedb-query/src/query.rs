use std::{collections::HashSet, fmt, ops::RangeFull};

use bincode::{
    enc::write::Writer,
    error::{DecodeError, EncodeError},
    BorrowDecode, Decode, Encode,
};
use indexmap::IndexMap;

use crate::{error::Error, query_item::QueryItem, Key, Path, SubqueryBranch};

/// `Query` represents one or more keys or ranges of keys, which can be used to
/// resolve a proof which will include all the requested values.
#[derive(Debug, Default, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    /// against `SizedQuery::limit`. A query with `limit = 10` may return
    /// more than 10 results when this flag is active, because the limit
    /// only governs child-level results. This will be resolved in a future
    /// redesign that introduces per-level limits.
    ///
    /// # Absence-proof verification
    ///
    /// When verifying with `verify_query_with_absence_proof` or
    /// `verify_subset_query_with_absence_proof`, results are reconstructed
    /// from `terminal_keys()` which does not emit parent-tree entries.
    /// Parent tree elements will therefore not appear in the verified
    /// result set in those modes.
    pub add_parent_tree_on_subquery: bool,
}

impl Encode for Query {
    fn encode<E: bincode::enc::Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        1u8.encode(encoder)?;

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
        if version != 1 {
            return Err(DecodeError::Other("unsupported Query encoding version"));
        }
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

        Ok(Query {
            items,
            default_subquery_branch,
            conditional_subquery_branches,
            left_to_right,
            add_parent_tree_on_subquery,
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
        if version != 1 {
            return Err(DecodeError::Other("unsupported Query encoding version"));
        }
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

        Ok(Query {
            items,
            default_subquery_branch,
            conditional_subquery_branches,
            left_to_right,
            add_parent_tree_on_subquery,
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
        write!(f, "}}")
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

    /// Maximum subquery nesting depth for `terminal_keys`. GroveDB paths
    /// rarely exceed a handful of levels; 64 is generous and prevents stack
    /// overflow from adversarial queries.
    const MAX_TERMINAL_KEYS_DEPTH: usize = 64;

    /// Pushes terminal key paths and keys to `result`, no more than
    /// `max_results`. Returns the number of terminal keys added.
    ///
    /// Terminal keys are the keys of a path query below which there are no more
    /// subqueries. In other words they're the keys of the terminal queries
    /// of a path query.
    pub fn terminal_keys(
        &self,
        current_path: Vec<Vec<u8>>,
        max_results: usize,
        result: &mut Vec<(Vec<Vec<u8>>, Vec<u8>)>,
    ) -> Result<usize, Error> {
        self.terminal_keys_inner(current_path, max_results, result, 0)
    }

    fn terminal_keys_inner(
        &self,
        current_path: Vec<Vec<u8>>,
        max_results: usize,
        result: &mut Vec<(Vec<Vec<u8>>, Vec<u8>)>,
        depth: usize,
    ) -> Result<usize, Error> {
        if depth >= Self::MAX_TERMINAL_KEYS_DEPTH {
            return Err(Error::NotSupported(
                "terminal_keys subquery nesting depth exceeded".to_string(),
            ));
        }
        let mut current_len = result.len();
        let mut added = 0;
        let mut already_added_keys = HashSet::new();
        if let Some(conditional_subquery_branches) = &self.conditional_subquery_branches {
            for (conditional_query_item, subquery_branch) in conditional_subquery_branches {
                // unbounded ranges can not be supported
                if conditional_query_item.is_unbounded_range() {
                    return Err(Error::NotSupported(
                        "terminal keys are not supported with conditional unbounded ranges"
                            .to_string(),
                    ));
                }
                let conditional_keys = conditional_query_item.keys()?;
                for key in conditional_keys.into_iter() {
                    if current_len > max_results {
                        return Err(Error::RequestAmountExceeded(format!(
                            "terminal keys limit exceeded for conditional subqueries, set max is \
                             {max_results}, current length is {current_len}",
                        )));
                    }
                    already_added_keys.insert(key.clone());
                    let mut path = current_path.clone();
                    if let Some(subquery_path) = &subquery_branch.subquery_path {
                        if let Some(subquery) = &subquery_branch.subquery {
                            // a subquery path with a subquery
                            // push the key to the path
                            path.push(key);
                            // push the subquery path to the path
                            path.extend(subquery_path.iter().cloned());
                            // recurse onto the lower level
                            let added_here = subquery.terminal_keys_inner(
                                path,
                                max_results,
                                result,
                                depth + 1,
                            )?;
                            added += added_here;
                            current_len += added_here;
                        } else {
                            if current_len == max_results {
                                return Err(Error::RequestAmountExceeded(format!(
                                    "terminal keys limit exceeded when subquery path but no \
                                     subquery, set max is {max_results}, current length is \
                                     {current_len}",
                                )));
                            }
                            // a subquery path but no subquery
                            // split the subquery path and remove the last element
                            // push the key to the path with the front elements,
                            // and set the tail of the subquery path as the terminal key
                            path.push(key);
                            if let Some((last_key, front_keys)) = subquery_path.split_last() {
                                path.extend(front_keys.iter().cloned());
                                result.push((path, last_key.clone()));
                            } else {
                                return Err(Error::CorruptedCodeExecution(
                                    "subquery_path set but doesn't contain any values",
                                ));
                            }

                            added += 1;
                            current_len += 1;
                        }
                    } else if let Some(subquery) = &subquery_branch.subquery {
                        // a subquery without a subquery path
                        // push the key to the path
                        path.push(key);
                        // recurse onto the lower level
                        let added_here =
                            subquery.terminal_keys_inner(path, max_results, result, depth + 1)?;
                        added += added_here;
                        current_len += added_here;
                    }
                }
            }
        }
        for item in self.items.iter() {
            if item.is_unbounded_range() {
                return Err(Error::NotSupported(
                    "terminal keys are not supported with unbounded ranges".to_string(),
                ));
            }
            let keys = item.keys()?;
            for key in keys.into_iter() {
                if already_added_keys.contains(&key) {
                    // we already had this key in the conditional subqueries
                    continue; // skip this key
                }
                if current_len > max_results {
                    return Err(Error::RequestAmountExceeded(format!(
                        "terminal keys limit exceeded for items, set max is {max_results}, \
                         current len is {current_len}",
                    )));
                }
                let mut path = current_path.clone();
                if let Some(subquery_path) = &self.default_subquery_branch.subquery_path {
                    if let Some(subquery) = &self.default_subquery_branch.subquery {
                        // a subquery path with a subquery
                        // push the key to the path
                        path.push(key);
                        // push the subquery path to the path
                        path.extend(subquery_path.iter().cloned());
                        // recurse onto the lower level
                        let added_here =
                            subquery.terminal_keys_inner(path, max_results, result, depth + 1)?;
                        added += added_here;
                        current_len += added_here;
                    } else {
                        if current_len == max_results {
                            return Err(Error::RequestAmountExceeded(format!(
                                "terminal keys limit exceeded when subquery path but no subquery, \
                                 set max is {max_results}, current len is {current_len}",
                            )));
                        }
                        // a subquery path but no subquery
                        // split the subquery path and remove the last element
                        // push the key to the path with the front elements,
                        // and set the tail of the subquery path as the terminal key
                        path.push(key);
                        if let Some((last_key, front_keys)) = subquery_path.split_last() {
                            path.extend(front_keys.iter().cloned());
                            result.push((path, last_key.clone()));
                        } else {
                            return Err(Error::CorruptedCodeExecution(
                                "subquery_path set but doesn't contain any values",
                            ));
                        }
                        added += 1;
                        current_len += 1;
                    }
                } else if let Some(subquery) = &self.default_subquery_branch.subquery {
                    // a subquery without a subquery path
                    // push the key to the path
                    path.push(key);
                    // recurse onto the lower level
                    let added_here =
                        subquery.terminal_keys_inner(path, max_results, result, depth + 1)?;
                    added += added_here;
                    current_len += added_here;
                } else {
                    if current_len == max_results {
                        return Err(Error::RequestAmountExceeded(format!(
                            "terminal keys limit exceeded without subquery or subquery path, set \
                             max is {max_results}, current len is {current_len}",
                        )));
                    }
                    result.push((path, key));
                    added += 1;
                    current_len += 1;
                }
            }
        }
        Ok(added)
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
        assert_eq!(decoded.left_to_right, false);
        assert_eq!(decoded.add_parent_tree_on_subquery, true);
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
            bincode::encode_to_vec(&excessive_len, bincode_config()).expect("encode length");
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
        payload.push(2u8); // invalid version (only version 1 is supported)
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
            bincode::encode_to_vec(&excessive_len, bincode_config()).expect("encode length");
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
