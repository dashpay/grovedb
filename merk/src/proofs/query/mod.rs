//! Query proofs

#[cfg(feature = "minimal")]
mod map;

#[cfg(any(feature = "minimal", feature = "verify"))]
mod common_path;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod insert;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod merge;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub mod query_item;
#[cfg(any(feature = "minimal", feature = "verify"))]
mod verify;

#[cfg(feature = "minimal")]
use std::cmp::Ordering;
use std::{collections::HashSet, fmt, ops::RangeFull};

#[cfg(any(feature = "minimal", feature = "verify"))]
use bincode::{
    enc::write::Writer,
    error::{DecodeError, EncodeError},
    BorrowDecode, Decode, Encode,
};
#[cfg(feature = "minimal")]
use grovedb_costs::{cost_return_on_error, CostContext, CostResult, CostsExt, OperationCost};
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;
#[cfg(any(feature = "minimal", feature = "verify"))]
use indexmap::IndexMap;
#[cfg(feature = "minimal")]
pub use map::*;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use query_item::intersect::QueryItemIntersectionResult;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use query_item::QueryItem;
#[cfg(feature = "minimal")]
use verify::ProofAbsenceLimit;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use verify::VerifyOptions;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use verify::{ProofVerificationResult, ProvedKeyOptionalValue, ProvedKeyValue};
#[cfg(feature = "minimal")]
use {super::Op, std::collections::LinkedList};

#[cfg(feature = "minimal")]
use super::Node;
#[cfg(any(feature = "minimal", feature = "verify"))]
use crate::error::Error;
use crate::proofs::hex_to_ascii;
#[cfg(feature = "minimal")]
use crate::tree::kv::ValueDefinedCostType;
#[cfg(feature = "minimal")]
use crate::tree::{Fetch, Link, RefWalker};

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Type alias for a path.
pub type Path = Vec<Vec<u8>>;

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Type alias for a Key.
pub type Key = Vec<u8>;

#[cfg(any(feature = "minimal", feature = "verify"))]
/// Type alias for path-key common pattern.
pub type PathKey = (Path, Key);

#[cfg(any(feature = "minimal", feature = "verify"))]
#[derive(Debug, Default, Clone, PartialEq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
/// Subquery branch
pub struct SubqueryBranch {
    /// Subquery path
    pub subquery_path: Option<Path>,
    /// Subquery
    pub subquery: Option<Box<Query>>,
}

impl SubqueryBranch {
    /// Returns the depth of the subquery branch
    /// This depth is how many GroveDB layers down we could query at maximum
    #[inline]
    pub fn max_depth(&self) -> Option<u16> {
        self.max_depth_internal(u8::MAX)
    }

    /// Returns the depth of the subquery branch
    /// This depth is how many GroveDB layers down we could query at maximum
    #[inline]
    fn max_depth_internal(&self, recursion_limit: u8) -> Option<u16> {
        if recursion_limit == 0 {
            return None;
        }
        let subquery_path_depth = self.subquery_path.as_ref().map_or(Some(0), |path| {
            let path_len = path.len();
            if path_len > u16::MAX as usize {
                None
            } else {
                Some(path_len as u16)
            }
        })?;
        let subquery_depth = self.subquery.as_ref().map_or(Some(0), |query| {
            query.max_depth_internal(recursion_limit - 1)
        })?;
        subquery_path_depth.checked_add(subquery_depth)
    }
}

#[cfg(any(feature = "minimal", feature = "verify"))]
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
}

#[cfg(any(feature = "minimal", feature = "verify"))]
impl Encode for Query {
    fn encode<E: bincode::enc::Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
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

        Ok(())
    }
}

#[cfg(any(feature = "minimal", feature = "verify"))]
impl Decode for Query {
    fn decode<D: bincode::de::Decoder>(decoder: &mut D) -> Result<Self, DecodeError> {
        // Decode the items vector
        let items = Vec::<QueryItem>::decode(decoder)?;

        // Decode the default subquery branch
        let default_subquery_branch = SubqueryBranch::decode(decoder)?;

        // Decode the conditional subquery branches
        let conditional_subquery_branches = if u8::decode(decoder)? == 1 {
            let len = u64::decode(decoder)? as usize;
            let mut map = IndexMap::with_capacity(len);
            for _ in 0..len {
                let key = QueryItem::decode(decoder)?;
                let value = SubqueryBranch::decode(decoder)?;
                map.insert(key, value);
            }
            Some(map)
        } else {
            None
        };

        // Decode the left_to_right boolean
        let left_to_right = bool::decode(decoder)?;

        Ok(Query {
            items,
            default_subquery_branch,
            conditional_subquery_branches,
            left_to_right,
        })
    }
}

#[cfg(any(feature = "minimal", feature = "verify"))]
impl<'de> BorrowDecode<'de> for Query {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        // Borrow-decode the items vector
        let items = Vec::<QueryItem>::borrow_decode(decoder)?;

        // Borrow-decode the default subquery branch
        let default_subquery_branch = SubqueryBranch::borrow_decode(decoder)?;

        // Borrow-decode the conditional subquery branches
        let conditional_subquery_branches = if u8::borrow_decode(decoder)? == 1 {
            let len = u64::borrow_decode(decoder)? as usize;
            let mut map = IndexMap::with_capacity(len);
            for _ in 0..len {
                let key = QueryItem::borrow_decode(decoder)?;
                let value = SubqueryBranch::borrow_decode(decoder)?;
                map.insert(key, value);
            }
            Some(map)
        } else {
            None
        };

        // Borrow-decode the left_to_right boolean
        let left_to_right = bool::borrow_decode(decoder)?;

        Ok(Query {
            items,
            default_subquery_branch,
            conditional_subquery_branches,
            left_to_right,
        })
    }
}

#[cfg(any(feature = "minimal", feature = "verify"))]
impl fmt::Display for SubqueryBranch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SubqueryBranch {{ ")?;
        if let Some(path) = &self.subquery_path {
            write!(f, "subquery_path: [")?;
            for (i, path_part) in path.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?
                }
                write!(f, "{}", hex_to_ascii(path_part))?;
            }
            write!(f, "], ")?;
        } else {
            write!(f, "subquery_path: None ")?;
        }
        if let Some(subquery) = &self.subquery {
            write!(f, "subquery: {} ", subquery)?;
        } else {
            write!(f, "subquery: None ")?;
        }
        write!(f, "}}")
    }
}

#[cfg(any(feature = "minimal", feature = "verify"))]
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
        write!(f, "}}")
    }
}

#[cfg(any(feature = "minimal", feature = "verify"))]
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
                            let added_here = subquery.terminal_keys(path, max_results, result)?;
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
                        let added_here = subquery.terminal_keys(path, max_results, result)?;
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
                        let added_here = subquery.terminal_keys(path, max_results, result)?;
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
                    let added_here = subquery.terminal_keys(path, max_results, result)?;
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
    pub(crate) fn len(&self) -> usize {
        self.items.len()
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

#[cfg(feature = "minimal")]
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
        }
    }
}

#[cfg(feature = "minimal")]
impl From<Query> for Vec<QueryItem> {
    fn from(q: Query) -> Self {
        q.into_iter().collect()
    }
}

#[cfg(feature = "minimal")]
impl IntoIterator for Query {
    type IntoIter = <Vec<QueryItem> as IntoIterator>::IntoIter;
    type Item = QueryItem;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

#[cfg(feature = "minimal")]
impl Link {
    /// Creates a `Node::Hash` from this link. Panics if the link is of variant
    /// `Link::Modified` since its hash has not yet been computed.
    #[cfg(feature = "minimal")]
    const fn to_hash_node(&self) -> Node {
        let hash = match self {
            Link::Reference { hash, .. } => hash,
            Link::Modified { .. } => {
                panic!("Cannot convert Link::Modified to proof hash node");
            }
            Link::Uncommitted { hash, .. } => hash,
            Link::Loaded { hash, .. } => hash,
        };
        Node::Hash(*hash)
    }
}

#[cfg(feature = "minimal")]
impl<S> RefWalker<'_, S>
where
    S: Fetch + Sized + Clone,
{
    #[allow(dead_code)]
    /// Creates a `Node::KV` from the key/value pair of the root node.
    pub(crate) fn to_kv_node(&self) -> Node {
        Node::KV(
            self.tree().key().to_vec(),
            self.tree().value_as_slice().to_vec(),
        )
    }

    /// Creates a `Node::KVValueHash` from the key/value pair of the root node.
    pub(crate) fn to_kv_value_hash_node(&self) -> Node {
        Node::KVValueHash(
            self.tree().key().to_vec(),
            self.tree().value_ref().to_vec(),
            *self.tree().value_hash(),
        )
    }

    /// Creates a `Node::KVValueHashFeatureType` from the key/value pair of the
    /// root node
    pub(crate) fn to_kv_value_hash_feature_type_node(&self) -> Node {
        Node::KVValueHashFeatureType(
            self.tree().key().to_vec(),
            self.tree().value_ref().to_vec(),
            *self.tree().value_hash(),
            self.tree().feature_type(),
        )
    }

    /// Creates a `Node::KVHash` from the hash of the key/value pair of the root
    /// node.
    pub(crate) fn to_kvhash_node(&self) -> Node {
        Node::KVHash(*self.tree().kv_hash())
    }

    /// Creates a `Node::KVDigest` from the key/value_hash pair of the root
    /// node.
    pub(crate) fn to_kvdigest_node(&self) -> Node {
        Node::KVDigest(self.tree().key().to_vec(), *self.tree().value_hash())
    }

    /// Creates a `Node::Hash` from the hash of the node.
    pub(crate) fn to_hash_node(&self) -> CostContext<Node> {
        self.tree().hash().map(Node::Hash)
    }

    /// Generates a proof for the list of queried keys. Returns a tuple
    /// containing the generated proof operators, and a tuple representing if
    /// any keys were queried were less than the left edge or greater than the
    /// right edge, respectively.
    ///
    /// TODO: Generalize logic and get code to better represent logic
    #[cfg(feature = "minimal")]
    pub(crate) fn create_proof(
        &mut self,
        query: &[QueryItem],
        limit: Option<u16>,
        left_to_right: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<ProofAbsenceLimit, Error> {
        let mut cost = OperationCost::default();

        // TODO: don't copy into vec, support comparing QI to byte slice
        let node_key = QueryItem::Key(self.tree().key().to_vec());
        let mut search = query.binary_search_by(|key| {
            // TODO: change to contains more efficient
            //  left here to catch potential errors with the intersect function
            if key.collides_with(&node_key) {
                // if key.contains(self.tree().key()) {
                Ordering::Equal
            } else {
                key.cmp(&node_key)
            }
        });

        let current_node_in_query: bool;
        let mut node_on_non_inclusive_bounds = false;

        let (mut left_items, mut right_items) = match search {
            Ok(index) => {
                current_node_in_query = true;
                let item = &query[index];
                let (left_bound, left_not_inclusive) = item.lower_bound();
                let (right_bound, right_inclusive) = item.upper_bound();

                if left_bound.is_some()
                    && left_bound.unwrap() == self.tree().key()
                    && left_not_inclusive
                    || right_bound.is_some()
                        && right_bound.unwrap() == self.tree().key()
                        && !right_inclusive
                {
                    node_on_non_inclusive_bounds = true;
                }

                // if range starts before this node's key, include it in left
                // child's query
                let left_query = if left_bound.is_none() || left_bound < Some(self.tree().key()) {
                    &query[..=index]
                } else {
                    &query[..index]
                };

                // if range ends after this node's key, include it in right
                // child's query
                let right_query = if right_bound.is_none() || right_bound > Some(self.tree().key())
                {
                    &query[index..]
                } else {
                    &query[index + 1..]
                };

                (left_query, right_query)
            }
            Err(index) => {
                current_node_in_query = false;
                (&query[..index], &query[index..])
            }
        };

        // when the limit hits zero, the rest of the query batch should be cleared
        // so empty the left, right query batch, and set the current node to not found
        if let Some(current_limit) = limit {
            if current_limit == 0 {
                left_items = &[];
                search = Err(Default::default());
                right_items = &[];
            }
        }

        let proof_direction = left_to_right; // signifies what direction the DFS should go
        let (mut proof, left_absence, mut new_limit) = if left_to_right {
            cost_return_on_error!(
                &mut cost,
                self.create_child_proof(
                    proof_direction,
                    left_items,
                    limit,
                    left_to_right,
                    grove_version
                )
            )
        } else {
            cost_return_on_error!(
                &mut cost,
                self.create_child_proof(
                    proof_direction,
                    right_items,
                    limit,
                    left_to_right,
                    grove_version
                )
            )
        };

        if let Some(current_limit) = new_limit {
            // if after generating proof for the left subtree, the limit becomes 0
            // clear the current node and clear the right batch
            if current_limit == 0 {
                if left_to_right {
                    right_items = &[];
                } else {
                    left_items = &[];
                }
                search = Err(Default::default());
            } else if current_node_in_query && !node_on_non_inclusive_bounds {
                // if limit is not zero, reserve a limit slot for the current node
                // before generating proof for the right subtree
                new_limit = Some(current_limit - 1);
                // if after limit slot reservation, limit becomes 0, right query
                // should be cleared
                if current_limit - 1 == 0 {
                    if left_to_right {
                        right_items = &[];
                    } else {
                        left_items = &[];
                    }
                }
            }
        }

        let proof_direction = !proof_direction; // search the opposite path on second pass
        let (mut right_proof, right_absence, new_limit) = if left_to_right {
            cost_return_on_error!(
                &mut cost,
                self.create_child_proof(
                    proof_direction,
                    right_items,
                    new_limit,
                    left_to_right,
                    grove_version
                )
            )
        } else {
            cost_return_on_error!(
                &mut cost,
                self.create_child_proof(
                    proof_direction,
                    left_items,
                    new_limit,
                    left_to_right,
                    grove_version
                )
            )
        };

        let (has_left, has_right) = (!proof.is_empty(), !right_proof.is_empty());

        proof.push_back(match search {
            Ok(_) => {
                if node_on_non_inclusive_bounds {
                    if left_to_right {
                        Op::Push(self.to_kvdigest_node())
                    } else {
                        Op::PushInverted(self.to_kvdigest_node())
                    }
                } else if left_to_right {
                    Op::Push(self.to_kv_value_hash_node())
                } else {
                    Op::PushInverted(self.to_kv_value_hash_node())
                }
            }
            Err(_) => {
                if left_absence.1 || right_absence.0 {
                    if left_to_right {
                        Op::Push(self.to_kvdigest_node())
                    } else {
                        Op::PushInverted(self.to_kvdigest_node())
                    }
                } else if left_to_right {
                    Op::Push(self.to_kvhash_node())
                } else {
                    Op::PushInverted(self.to_kvhash_node())
                }
            }
        });

        if has_left {
            if left_to_right {
                proof.push_back(Op::Parent);
            } else {
                proof.push_back(Op::ParentInverted);
            }
        }

        if has_right {
            proof.append(&mut right_proof);
            if left_to_right {
                proof.push_back(Op::Child);
            } else {
                proof.push_back(Op::ChildInverted);
            }
        }

        Ok((proof, (left_absence.0, right_absence.1), new_limit)).wrap_with_cost(cost)
    }

    /// Similar to `create_proof`. Recurses into the child on the given side and
    /// generates a proof for the queried keys.
    #[cfg(feature = "minimal")]
    fn create_child_proof(
        &mut self,
        left: bool,
        query: &[QueryItem],
        limit: Option<u16>,
        left_to_right: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<ProofAbsenceLimit, Error> {
        if !query.is_empty() {
            self.walk(
                left,
                None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                grove_version,
            )
            .flat_map_ok(|child_opt| {
                if let Some(mut child) = child_opt {
                    child.create_proof(query, limit, left_to_right, grove_version)
                } else {
                    Ok((LinkedList::new(), (true, true), limit)).wrap_with_cost(Default::default())
                }
            })
        } else if let Some(link) = self.tree().link(left) {
            let mut proof = LinkedList::new();
            proof.push_back(if left_to_right {
                Op::Push(link.to_hash_node())
            } else {
                Op::PushInverted(link.to_hash_node())
            });
            Ok((proof, (false, false), limit)).wrap_with_cost(Default::default())
        } else {
            Ok((LinkedList::new(), (false, false), limit)).wrap_with_cost(Default::default())
        }
    }
}

#[cfg(feature = "minimal")]
#[allow(deprecated)]
#[cfg(test)]
mod test {

    macro_rules! compare_result_tuples_not_optional {
        ($result_set:expr, $expected_result_set:expr) => {
            assert_eq!(
                $expected_result_set.len(),
                $result_set.len(),
                "Result set lengths do not match"
            );
            for i in 0..$expected_result_set.len() {
                assert_eq!(
                    $expected_result_set[i].0, $result_set[i].key,
                    "Key mismatch at index {}",
                    i
                );
                assert_eq!(
                    &$expected_result_set[i].1,
                    $result_set[i].value.as_ref().expect("expected value"),
                    "Value mismatch at index {}",
                    i
                );
            }
        };
    }

    use super::{
        super::{encoding::encode_into, *},
        *,
    };
    use crate::{
        proofs::query::verify,
        test_utils::make_tree_seq,
        tree::{NoopCommit, PanicSource, RefWalker, TreeNode},
        TreeFeatureType::BasicMerkNode,
    };

    fn make_3_node_tree() -> TreeNode {
        let mut tree = TreeNode::new(vec![5], vec![5], None, BasicMerkNode)
            .unwrap()
            .attach(
                true,
                Some(TreeNode::new(vec![3], vec![3], None, BasicMerkNode).unwrap()),
            )
            .attach(
                false,
                Some(TreeNode::new(vec![7], vec![7], None, BasicMerkNode).unwrap()),
            );
        tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
            .unwrap()
            .expect("commit failed");
        tree
    }

    fn make_6_node_tree() -> TreeNode {
        let two_tree = TreeNode::new(vec![2], vec![2], None, BasicMerkNode).unwrap();
        let four_tree = TreeNode::new(vec![4], vec![4], None, BasicMerkNode).unwrap();
        let mut three_tree = TreeNode::new(vec![3], vec![3], None, BasicMerkNode)
            .unwrap()
            .attach(true, Some(two_tree))
            .attach(false, Some(four_tree));
        three_tree
            .commit(&mut NoopCommit {}, &|_, _| Ok(0))
            .unwrap()
            .expect("commit failed");

        let seven_tree = TreeNode::new(vec![7], vec![7], None, BasicMerkNode).unwrap();
        let mut eight_tree = TreeNode::new(vec![8], vec![8], None, BasicMerkNode)
            .unwrap()
            .attach(true, Some(seven_tree));
        eight_tree
            .commit(&mut NoopCommit {}, &|_, _| Ok(0))
            .unwrap()
            .expect("commit failed");

        let mut root_tree = TreeNode::new(vec![5], vec![5], None, BasicMerkNode)
            .unwrap()
            .attach(true, Some(three_tree))
            .attach(false, Some(eight_tree));
        root_tree
            .commit(&mut NoopCommit {}, &|_, _| Ok(0))
            .unwrap()
            .expect("commit failed");

        root_tree
    }

    fn verify_keys_test(keys: Vec<Vec<u8>>, expected_result: Vec<Option<Vec<u8>>>) {
        let grove_version = GroveVersion::latest();
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let (proof, ..) = walker
            .create_proof(
                keys.clone()
                    .into_iter()
                    .map(QueryItem::Key)
                    .collect::<Vec<_>>()
                    .as_slice(),
                None,
                true,
                grove_version,
            )
            .unwrap()
            .expect("failed to create proof");
        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);

        let expected_hash = [
            148, 227, 127, 84, 149, 54, 117, 188, 32, 85, 176, 25, 96, 127, 170, 90, 148, 196, 218,
            30, 5, 109, 112, 3, 120, 138, 194, 28, 27, 49, 119, 125,
        ];

        let mut query = Query::new();
        for key in keys.iter() {
            query.insert_key(key.clone());
        }

        let result = query
            .verify_proof(bytes.as_slice(), None, true, expected_hash)
            .unwrap()
            .expect("verify failed");

        let mut values = std::collections::HashMap::new();
        for proved_value in result.result_set {
            assert!(values
                .insert(proved_value.key, proved_value.value)
                .is_none());
        }

        for (key, expected_value) in keys.iter().zip(expected_result.iter()) {
            assert_eq!(
                values.get(key).and_then(|a| a.as_ref()),
                expected_value.as_ref()
            );
        }
    }

    #[test]
    fn test_query_merge_single_key() {
        // single key test
        let mut query_one = Query::new();
        query_one.insert_key(b"a".to_vec());
        let mut query_two = Query::new();
        query_two.insert_key(b"b".to_vec());
        query_one.merge_with(query_two);
        let mut expected_query = Query::new();
        expected_query.insert_key(b"a".to_vec());
        expected_query.insert_key(b"b".to_vec());
        assert_eq!(query_one, expected_query);
    }

    #[test]
    fn test_query_merge_range() {
        // range test
        let mut query_one = Query::new();
        query_one.insert_range(b"a".to_vec()..b"c".to_vec());
        let mut query_two = Query::new();
        query_two.insert_key(b"b".to_vec());
        query_one.merge_with(query_two);
        let mut expected_query = Query::new();
        expected_query.insert_range(b"a".to_vec()..b"c".to_vec());
        assert_eq!(query_one, expected_query);
    }

    #[test]
    fn test_query_merge_conditional_query() {
        // conditional query test
        let mut query_one = Query::new();
        query_one.insert_key(b"a".to_vec());
        let mut insert_all_query = Query::new();
        insert_all_query.insert_all();
        query_one.add_conditional_subquery(
            QueryItem::Key(b"a".to_vec()),
            None,
            Some(insert_all_query),
        );

        let mut query_two = Query::new();
        query_two.insert_key(b"b".to_vec());
        query_one.merge_with(query_two);

        let mut expected_query = Query::new();
        expected_query.insert_key(b"a".to_vec());
        expected_query.insert_key(b"b".to_vec());
        let mut insert_all_query = Query::new();
        insert_all_query.insert_all();
        expected_query.add_conditional_subquery(
            QueryItem::Key(b"a".to_vec()),
            None,
            Some(insert_all_query),
        );
        assert_eq!(query_one, expected_query);
    }

    #[test]
    fn test_query_merge_deep_conditional_query() {
        // deep conditional query
        // [a, b, c]
        // [a, c, d]
        let mut query_one = Query::new();
        query_one.insert_key(b"a".to_vec());
        let mut query_one_b = Query::new();
        query_one_b.insert_key(b"b".to_vec());
        let mut query_one_c = Query::new();
        query_one_c.insert_key(b"c".to_vec());
        query_one_b.add_conditional_subquery(
            QueryItem::Key(b"b".to_vec()),
            None,
            Some(query_one_c),
        );
        query_one.add_conditional_subquery(QueryItem::Key(b"a".to_vec()), None, Some(query_one_b));

        let mut query_two = Query::new();
        query_two.insert_key(b"a".to_vec());
        let mut query_two_c = Query::new();
        query_two_c.insert_key(b"c".to_vec());
        let mut query_two_d = Query::new();
        query_two_d.insert_key(b"d".to_vec());
        query_two_c.add_conditional_subquery(
            QueryItem::Key(b"c".to_vec()),
            None,
            Some(query_two_d),
        );
        query_two.add_conditional_subquery(QueryItem::Key(b"a".to_vec()), None, Some(query_two_c));
        query_one.merge_with(query_two);

        let mut expected_query = Query::new();
        expected_query.insert_key(b"a".to_vec());
        let mut query_b_c = Query::new();
        query_b_c.insert_key(b"b".to_vec());
        query_b_c.insert_key(b"c".to_vec());
        let mut query_c = Query::new();
        query_c.insert_key(b"c".to_vec());
        let mut query_d = Query::new();
        query_d.insert_key(b"d".to_vec());

        query_b_c.add_conditional_subquery(QueryItem::Key(b"b".to_vec()), None, Some(query_c));
        query_b_c.add_conditional_subquery(QueryItem::Key(b"c".to_vec()), None, Some(query_d));

        expected_query.add_conditional_subquery(
            QueryItem::Key(b"a".to_vec()),
            None,
            Some(query_b_c),
        );
        assert_eq!(query_one, expected_query);
    }

    #[test]
    fn root_verify() {
        verify_keys_test(vec![vec![5]], vec![Some(vec![5])]);
    }

    #[test]
    fn single_verify() {
        verify_keys_test(vec![vec![3]], vec![Some(vec![3])]);
    }

    #[test]
    fn double_verify() {
        verify_keys_test(vec![vec![3], vec![5]], vec![Some(vec![3]), Some(vec![5])]);
    }

    #[test]
    fn double_verify_2() {
        verify_keys_test(vec![vec![3], vec![7]], vec![Some(vec![3]), Some(vec![7])]);
    }

    #[test]
    fn triple_verify() {
        verify_keys_test(
            vec![vec![3], vec![5], vec![7]],
            vec![Some(vec![3]), Some(vec![5]), Some(vec![7])],
        );
    }

    #[test]
    fn left_edge_absence_verify() {
        verify_keys_test(vec![vec![2]], vec![None]);
    }

    #[test]
    fn right_edge_absence_verify() {
        verify_keys_test(vec![vec![8]], vec![None]);
    }

    #[test]
    fn inner_absence_verify() {
        verify_keys_test(vec![vec![6]], vec![None]);
    }

    #[test]
    fn absent_and_present_verify() {
        verify_keys_test(vec![vec![5], vec![6]], vec![Some(vec![5]), None]);
    }

    #[test]
    fn node_variant_conversion() {
        let mut tree = make_6_node_tree();
        let walker = RefWalker::new(&mut tree, PanicSource {});

        assert_eq!(walker.to_kv_node(), Node::KV(vec![5], vec![5]));
        assert_eq!(
            walker.to_kvhash_node(),
            Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])
        );
        assert_eq!(
            walker.to_kvdigest_node(),
            Node::KVDigest(
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            ),
        );
        assert_eq!(
            walker.to_hash_node().unwrap(),
            Node::Hash([
                47, 88, 45, 83, 28, 53, 123, 233, 238, 140, 130, 174, 250, 220, 210, 37, 3, 215,
                82, 177, 190, 30, 154, 156, 35, 214, 144, 79, 40, 41, 218, 142
            ])
        );
    }

    #[test]
    fn empty_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let (proof, absence, ..) = walker
            .create_proof(vec![].as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169,
                82, 205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                171, 95, 191, 1, 198, 99, 138, 43, 233, 158, 239, 50, 56, 86, 221, 125, 213, 84,
                143, 196, 177, 139, 135, 144, 4, 86, 197, 9, 92, 30, 65, 41
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let res = Query::new()
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        assert!(res.result_set.is_empty());
    }

    #[test]
    fn root_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::Key(vec![5])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169,
                82, 205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![5],
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                171, 95, 191, 1, 198, 99, 138, 43, 233, 158, 239, 50, 56, 86, 221, 125, 213, 84,
                143, 196, 177, 139, 135, 144, 4, 86, 197, 9, 92, 30, 65, 41
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![5], vec![5])]);
    }

    #[test]
    fn leaf_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::Key(vec![3])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![3],
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                171, 95, 191, 1, 198, 99, 138, 43, 233, 158, 239, 50, 56, 86, 221, 125, 213, 84,
                143, 196, 177, 139, 135, 144, 4, 86, 197, 9, 92, 30, 65, 41
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![3], vec![3])]);
    }

    #[test]
    fn double_leaf_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::Key(vec![3]), QueryItem::Key(vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![3],
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![7],
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![3], vec![3]), (vec![7], vec![7])]
        );
    }

    #[test]
    fn all_nodes_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![
            QueryItem::Key(vec![3]),
            QueryItem::Key(vec![5]),
            QueryItem::Key(vec![7]),
        ];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![3],
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![5],
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![7],
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![3], vec![3]), (vec![5], vec![5]), (vec![7], vec![7])]
        );
    }

    #[test]
    fn global_edge_absence_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::Key(vec![8])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169,
                82, 205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, Vec::<(Vec<u8>, Vec<u8>)>::new());
    }

    #[test]
    fn absence_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::Key(vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                139, 162, 218, 27, 213, 199, 221, 8, 110, 173, 94, 78, 254, 231, 225, 61, 122, 169,
                82, 205, 81, 207, 60, 90, 166, 78, 184, 53, 134, 79, 66, 255
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, Vec::<(Vec<u8>, Vec<u8>)>::new());
    }

    #[test]
    fn doc_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = TreeNode::new(vec![5], vec![5], None, BasicMerkNode)
            .unwrap()
            .attach(
                true,
                Some(
                    TreeNode::new(vec![2], vec![2], None, BasicMerkNode)
                        .unwrap()
                        .attach(
                            true,
                            Some(TreeNode::new(vec![1], vec![1], None, BasicMerkNode).unwrap()),
                        )
                        .attach(
                            false,
                            Some(
                                TreeNode::new(vec![4], vec![4], None, BasicMerkNode)
                                    .unwrap()
                                    .attach(
                                        true,
                                        Some(
                                            TreeNode::new(vec![3], vec![3], None, BasicMerkNode)
                                                .unwrap(),
                                        ),
                                    ),
                            ),
                        ),
                ),
            )
            .attach(
                false,
                Some(
                    TreeNode::new(vec![9], vec![9], None, BasicMerkNode)
                        .unwrap()
                        .attach(
                            true,
                            Some(
                                TreeNode::new(vec![7], vec![7], None, BasicMerkNode)
                                    .unwrap()
                                    .attach(
                                        true,
                                        Some(
                                            TreeNode::new(vec![6], vec![6], None, BasicMerkNode)
                                                .unwrap(),
                                        ),
                                    )
                                    .attach(
                                        false,
                                        Some(
                                            TreeNode::new(vec![8], vec![8], None, BasicMerkNode)
                                                .unwrap(),
                                        ),
                                    ),
                            ),
                        )
                        .attach(
                            false,
                            Some(
                                TreeNode::new(vec![11], vec![11], None, BasicMerkNode)
                                    .unwrap()
                                    .attach(
                                        true,
                                        Some(
                                            TreeNode::new(vec![10], vec![10], None, BasicMerkNode)
                                                .unwrap(),
                                        ),
                                    ),
                            ),
                        ),
                ),
            );
        tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
            .unwrap()
            .unwrap();

        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![
            QueryItem::Key(vec![1]),
            QueryItem::Key(vec![2]),
            QueryItem::Key(vec![3]),
            QueryItem::Key(vec![4]),
        ];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![1],
                vec![1],
                [
                    32, 34, 236, 157, 87, 27, 167, 116, 207, 158, 131, 208, 25, 73, 98, 245, 209,
                    227, 170, 26, 72, 212, 134, 166, 126, 39, 98, 166, 199, 149, 144, 21
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![2],
                vec![2],
                [
                    183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190, 166, 110, 16,
                    139, 136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96, 178
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![3],
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![4],
                vec![4],
                [
                    198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51,
                    227, 215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                12, 156, 232, 212, 220, 65, 226, 32, 91, 101, 248, 64, 225, 206, 63, 12, 153, 191,
                183, 10, 233, 251, 249, 76, 184, 200, 88, 57, 219, 2, 250, 113
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        assert_eq!(
            bytes,
            vec![
                4, 1, 1, 0, 1, 1, 32, 34, 236, 157, 87, 27, 167, 116, 207, 158, 131, 208, 25, 73,
                98, 245, 209, 227, 170, 26, 72, 212, 134, 166, 126, 39, 98, 166, 199, 149, 144, 21,
                4, 1, 2, 0, 1, 2, 183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190,
                166, 110, 16, 139, 136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96,
                178, 16, 4, 1, 3, 0, 1, 3, 210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81,
                192, 139, 153, 104, 205, 4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169,
                129, 231, 144, 4, 1, 4, 0, 1, 4, 198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146,
                71, 4, 16, 82, 205, 89, 51, 227, 215, 139, 195, 237, 202, 159, 191, 209, 172, 156,
                38, 239, 192, 16, 17, 2, 61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44,
                165, 68, 87, 7, 52, 238, 68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88,
                197, 16, 1, 12, 156, 232, 212, 220, 65, 226, 32, 91, 101, 248, 64, 225, 206, 63,
                12, 153, 191, 183, 10, 233, 251, 249, 76, 184, 200, 88, 57, 219, 2, 250, 113, 17
            ]
        );

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![1], vec![1]),
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
            ]
        );
    }

    #[test]
    fn query_item_merge() {
        let mine = QueryItem::Range(vec![10]..vec![30]);
        let other = QueryItem::Range(vec![15]..vec![20]);
        assert_eq!(mine.merge(&other), QueryItem::Range(vec![10]..vec![30]));

        let mine = QueryItem::RangeInclusive(vec![10]..=vec![30]);
        let other = QueryItem::Range(vec![20]..vec![30]);
        assert_eq!(
            mine.merge(&other),
            QueryItem::RangeInclusive(vec![10]..=vec![30])
        );

        let mine = QueryItem::Key(vec![5]);
        let other = QueryItem::Range(vec![1]..vec![10]);
        assert_eq!(mine.merge(&other), QueryItem::Range(vec![1]..vec![10]));

        let mine = QueryItem::Key(vec![10]);
        let other = QueryItem::RangeInclusive(vec![1]..=vec![10]);
        assert_eq!(
            mine.merge(&other),
            QueryItem::RangeInclusive(vec![1]..=vec![10])
        );
    }

    #[test]
    fn query_insert() {
        let mut query = Query::new();
        query.insert_key(vec![2]);
        query.insert_range(vec![3]..vec![5]);
        query.insert_range_inclusive(vec![5]..=vec![7]);
        query.insert_range(vec![4]..vec![6]);
        query.insert_key(vec![5]);

        let mut iter = query.items.iter();
        assert_eq!(format!("{:?}", iter.next()), "Some(Key([2]))");
        assert_eq!(
            format!("{:?}", iter.next()),
            "Some(RangeInclusive([3]..=[7]))"
        );
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn range_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_tree_seq(10, grove_version);
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 0, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        )];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
                197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123,
                117, 31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172,
                237, 19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![0, 0, 0, 0, 0, 0, 0, 5],
                vec![123; 60],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![0, 0, 0, 0, 0, 0, 0, 6],
                vec![123; 60],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![0, 0, 0, 0, 0, 0, 0, 7],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
                188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
            ]
        );
        assert_eq!(res.limit, None);

        // right to left test
        let mut tree = make_tree_seq(10, grove_version);
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 0, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        )];
        let (proof, ..) = walker
            .create_proof(query_items.as_slice(), None, false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new_with_direction(false);
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
            ]
        );
    }

    #[test]
    fn range_proof_inclusive() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_tree_seq(10, grove_version);
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeInclusive(
            vec![0, 0, 0, 0, 0, 0, 0, 5]..=vec![0, 0, 0, 0, 0, 0, 0, 7],
        )];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
                197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123,
                117, 31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172,
                237, 19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![0, 0, 0, 0, 0, 0, 0, 5],
                vec![123; 60],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![0, 0, 0, 0, 0, 0, 0, 6],
                vec![123; 60],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![0, 0, 0, 0, 0, 0, 0, 7],
                vec![123; 60],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
                188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 7], vec![123; 60]),
            ]
        );
        assert_eq!(res.limit, None);

        // right_to_left proof
        let mut tree = make_tree_seq(10, grove_version);
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeInclusive(
            vec![0, 0, 0, 0, 0, 0, 0, 5]..=vec![0, 0, 0, 0, 0, 0, 0, 7],
        )];
        let (proof, ..) = walker
            .create_proof(query_items.as_slice(), None, false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
            .unwrap()
            .unwrap();

        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 7], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
            ]
        );
    }

    #[test]
    fn range_from_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFrom(vec![5]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                85, 217, 56, 226, 204, 53, 103, 145, 201, 33, 178, 80, 207, 194, 104, 128, 199,
                145, 156, 208, 152, 255, 209, 24, 140, 222, 204, 193, 211, 26, 118, 58
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![5],
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![7],
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![8],
                vec![8],
                [
                    205, 24, 196, 78, 21, 130, 132, 58, 44, 29, 21, 175, 68, 254, 158, 189, 49,
                    158, 250, 151, 137, 22, 160, 107, 216, 238, 129, 230, 199, 251, 197, 51
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![5], vec![5]), (vec![7], vec![7]), (vec![8], vec![8])]
        );
        assert_eq!(res.limit, None);

        // Limit result set to 1 item
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFrom(vec![5]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(1), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::Key(vec![5])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![5], vec![5])]);
        assert_eq!(res.limit, Some(0));

        // Limit result set to 2 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFrom(vec![5]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(2), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![
            QueryItem::Key(vec![5]),
            QueryItem::Key(vec![6]),
            QueryItem::Key(vec![7]),
        ];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![5], vec![5]), (vec![7], vec![7])]
        );
        assert_eq!(res.limit, Some(0));

        // Limit result set to 100 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFrom(vec![5]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(100), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeFrom(vec![5]..)];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![5], vec![5]), (vec![7], vec![7]), (vec![8], vec![8])]
        );
        assert_eq!(res.limit, Some(97));

        // right_to_left test
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFrom(vec![5]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (true, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![8], vec![8]), (vec![7], vec![7]), (vec![5], vec![5])]
        );
    }

    #[test]
    fn range_to_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeTo(..vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![2],
                vec![2],
                [
                    183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190, 166, 110, 16,
                    139, 136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96, 178
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![3],
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![4],
                vec![4],
                [
                    198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51,
                    227, 215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![5],
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250,
                165, 180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (true, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
                (vec![5], vec![5]),
            ]
        );
        assert_eq!(res.limit, None);

        // Limit result set to 1 item
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeTo(..vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(1), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![2])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![2], vec![2])]);
        assert_eq!(res.limit, Some(0));

        // Limit result set to 2 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeTo(..vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(2), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![3])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![2], vec![2]), (vec![3], vec![3])]
        );
        assert_eq!(res.limit, Some(0));

        // Limit result set to 100 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeTo(..vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(100), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeTo(..vec![6])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
                (vec![5], vec![5]),
            ]
        );
        assert_eq!(res.limit, Some(96));

        // right_to_left proof
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeTo(..vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (false, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![5], vec![5]),
                (vec![4], vec![4]),
                (vec![3], vec![3]),
                (vec![2], vec![2]),
            ]
        );

        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeTo(..vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(2), false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(2), false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![5], vec![5]), (vec![4], vec![4])]
        );
        assert_eq!(res.limit, Some(0));
    }

    #[test]
    fn range_to_proof_inclusive() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![2],
                vec![2],
                [
                    183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190, 166, 110, 16,
                    139, 136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96, 178
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![3],
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![4],
                vec![4],
                [
                    198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51,
                    227, 215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![5],
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250,
                165, 180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (true, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
                (vec![5], vec![5]),
            ]
        );
        assert_eq!(res.limit, None);

        // Limit result set to 1 item
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(1), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![2])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![2], vec![2])]);
        assert_eq!(res.limit, Some(0));

        // Limit result set to 2 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(2), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![3])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![2], vec![2]), (vec![3], vec![3])]
        );
        assert_eq!(res.limit, Some(0));

        // Limit result set to 100 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(100), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
                (vec![5], vec![5]),
            ]
        );
        assert_eq!(res.limit, Some(96));

        // right_to_left proof
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (false, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![5], vec![5]),
                (vec![4], vec![4]),
                (vec![3], vec![3]),
                (vec![2], vec![2]),
            ]
        );

        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeToInclusive(..=vec![6])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(1), false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(1), false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![5], vec![5])]);
        assert_eq!(res.limit, Some(0));
    }

    #[test]
    fn range_after_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4,
                241, 127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![4],
                vec![4],
                [
                    198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51,
                    227, 215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![5],
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![7],
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![8],
                vec![8],
                [
                    205, 24, 196, 78, 21, 130, 132, 58, 44, 29, 21, 175, 68, 254, 158, 189, 49,
                    158, 250, 151, 137, 22, 160, 107, 216, 238, 129, 230, 199, 251, 197, 51
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![4], vec![4]),
                (vec![5], vec![5]),
                (vec![7], vec![7]),
                (vec![8], vec![8]),
            ]
        );
        assert_eq!(res.limit, None);

        // Limit result set to 1 item
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(1), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![4])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![4], vec![4])]);
        assert_eq!(res.limit, Some(0));

        // Limit result set to 2 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(2), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![5])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![4], vec![4]), (vec![5], vec![5])]
        );
        assert_eq!(res.limit, Some(0));

        // Limit result set to 100 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(100), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeAfter(vec![3]..)];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![4], vec![4]),
                (vec![5], vec![5]),
                (vec![7], vec![7]),
                (vec![8], vec![8]),
            ]
        );
        assert_eq!(res.limit, Some(96));

        // right_to_left proof
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (true, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![8], vec![8]),
                (vec![7], vec![7]),
                (vec![5], vec![5]),
                (vec![4], vec![4]),
            ]
        );

        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfter(vec![3]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(3), false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (true, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(3), false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![8], vec![8]), (vec![7], vec![7]), (vec![5], vec![5])]
        );
        assert_eq!(res.limit, Some(0));
    }

    #[test]
    fn range_after_to_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4,
                241, 127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![4],
                vec![4],
                [
                    198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51,
                    227, 215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![5],
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250,
                165, 180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![4], vec![4]), (vec![5], vec![5])]
        );
        assert_eq!(res.limit, None);

        // Limit result set to 1 item
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(1), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![4])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![4], vec![4])]);
        assert_eq!(res.limit, Some(0));

        // Limit result set to 2 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(2), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![5])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![4], vec![4]), (vec![5], vec![5])]
        );
        assert_eq!(res.limit, Some(0));

        // Limit result set to 100 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(100), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![4], vec![4]), (vec![5], vec![5])]
        );
        assert_eq!(res.limit, Some(98));

        // right_to_left
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![5], vec![5]), (vec![4], vec![4])]
        );

        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterTo(vec![3]..vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(300), false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(300), false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![5], vec![5]), (vec![4], vec![4])]
        );
        assert_eq!(res.limit, Some(298));
    }

    #[test]
    fn range_after_to_proof_inclusive() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        iter.next();
        Some(&Op::Push(Node::Hash([
            121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4,
            241, 127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30,
        ])));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![4],
                vec![4],
                [
                    198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51,
                    227, 215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![5],
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![7],
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                236, 141, 96, 8, 244, 103, 232, 110, 117, 105, 162, 111, 148, 9, 59, 195, 2, 250,
                165, 180, 215, 137, 202, 221, 38, 98, 93, 247, 54, 180, 242, 116
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![4], vec![4]), (vec![5], vec![5]), (vec![7], vec![7])]
        );
        assert_eq!(res.limit, None);

        // Limit result set to 1 item
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(1), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![4])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![4], vec![4])]);
        assert_eq!(res.limit, Some(0));

        // Limit result set to 2 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(2), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![5])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![4], vec![4]), (vec![5], vec![5])]
        );
        assert_eq!(res.limit, Some(0));

        // Limit result set to 100 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(100), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![4], vec![4]), (vec![5], vec![5]), (vec![7], vec![7])]
        );
        assert_eq!(res.limit, Some(97));

        // right_to_left proof
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeAfterToInclusive(vec![3]..=vec![7])];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![7], vec![7]), (vec![5], vec![5]), (vec![4], vec![4])]
        );
    }

    #[test]
    fn range_full_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFull(..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![2],
                vec![2],
                [
                    183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190, 166, 110, 16,
                    139, 136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96, 178
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![3],
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![4],
                vec![4],
                [
                    198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51,
                    227, 215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![5],
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![7],
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![8],
                vec![8],
                [
                    205, 24, 196, 78, 21, 130, 132, 58, 44, 29, 21, 175, 68, 254, 158, 189, 49,
                    158, 250, 151, 137, 22, 160, 107, 216, 238, 129, 230, 199, 251, 197, 51
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(iter.next(), Some(&Op::Child));

        assert!(iter.next().is_none());
        assert_eq!(absence, (true, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
                (vec![5], vec![5]),
                (vec![7], vec![7]),
                (vec![8], vec![8]),
            ]
        );
        assert_eq!(res.limit, None);

        // Limit result set to 1 item
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFull(..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(1), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![2])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![2], vec![2])]);
        assert_eq!(res.limit, Some(0));

        // Limit result set to 2 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFull(..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(2), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeToInclusive(..=vec![3])];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(2), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![2], vec![2]), (vec![3], vec![3])]
        );
        assert_eq!(res.limit, Some(0));

        // Limit result set to 100 items
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFull(..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(100), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let equivalent_query_items = vec![QueryItem::RangeFull(..)];
        let (equivalent_proof, equivalent_absence, ..) = walker
            .create_proof(equivalent_query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(proof, equivalent_proof);
        assert_eq!(absence, equivalent_absence);

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(100), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![2], vec![2]),
                (vec![3], vec![3]),
                (vec![4], vec![4]),
                (vec![5], vec![5]),
                (vec![7], vec![7]),
                (vec![8], vec![8]),
            ]
        );
        assert_eq!(res.limit, Some(94));

        // right_to_left proof
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFull(..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (true, true));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![8], vec![8]),
                (vec![7], vec![7]),
                (vec![5], vec![5]),
                (vec![4], vec![4]),
                (vec![3], vec![3]),
                (vec![2], vec![2]),
            ]
        );

        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFull(..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), Some(2), false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        assert_eq!(absence, (true, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(2), false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![8], vec![8]), (vec![7], vec![7])]
        );
        assert_eq!(res.limit, Some(0));
    }

    #[test]
    fn proof_with_limit() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFrom(vec![2]..)];
        let (proof, _, limit) = walker
            .create_proof(query_items.as_slice(), Some(1), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        // TODO: Add this test for other range types
        assert_eq!(limit, Some(0));

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![2],
                vec![2],
                [
                    183, 215, 112, 4, 15, 120, 14, 157, 239, 246, 188, 3, 138, 190, 166, 110, 16,
                    139, 136, 208, 152, 209, 109, 36, 205, 116, 134, 235, 103, 16, 96, 178
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                126, 128, 159, 241, 207, 26, 88, 61, 163, 18, 218, 189, 45, 220, 124, 96, 118, 68,
                61, 95, 230, 75, 145, 218, 178, 227, 63, 137, 79, 153, 182, 12
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                56, 181, 68, 232, 233, 83, 180, 104, 74, 123, 143, 25, 174, 80, 132, 201, 61, 108,
                131, 89, 204, 90, 128, 199, 164, 25, 3, 146, 39, 127, 12, 105
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                61, 233, 169, 61, 231, 15, 78, 53, 219, 99, 131, 45, 44, 165, 68, 87, 7, 52, 238,
                68, 142, 211, 110, 161, 111, 220, 108, 11, 17, 31, 88, 197
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                133, 188, 175, 131, 60, 89, 221, 135, 133, 53, 205, 110, 58, 56, 128, 58, 1, 227,
                75, 122, 83, 20, 125, 44, 149, 44, 62, 130, 252, 134, 105, 200
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), Some(1), true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(res.result_set, vec![(vec![2], vec![2])]);
        assert_eq!(res.limit, Some(0));
    }

    #[test]
    fn right_to_left_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_6_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::RangeFrom(vec![3]..)];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, false, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::PushInverted(Node::KVValueHash(
                vec![8],
                vec![8],
                [
                    205, 24, 196, 78, 21, 130, 132, 58, 44, 29, 21, 175, 68, 254, 158, 189, 49,
                    158, 250, 151, 137, 22, 160, 107, 216, 238, 129, 230, 199, 251, 197, 51
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::PushInverted(Node::KVValueHash(
                vec![7],
                vec![7],
                [
                    63, 193, 78, 215, 236, 222, 32, 58, 144, 66, 94, 225, 145, 233, 219, 89, 102,
                    51, 109, 115, 127, 3, 152, 236, 147, 183, 100, 81, 123, 109, 244, 0
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::ChildInverted));
        assert_eq!(
            iter.next(),
            Some(&Op::PushInverted(Node::KVValueHash(
                vec![5],
                vec![5],
                [
                    116, 30, 0, 135, 25, 118, 86, 14, 12, 107, 215, 214, 133, 122, 48, 45, 180, 21,
                    158, 223, 88, 148, 181, 149, 189, 65, 121, 19, 81, 118, 11, 106
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::ParentInverted));
        assert_eq!(
            iter.next(),
            Some(&Op::PushInverted(Node::KVValueHash(
                vec![4],
                vec![4],
                [
                    198, 129, 51, 156, 134, 199, 7, 21, 172, 89, 146, 71, 4, 16, 82, 205, 89, 51,
                    227, 215, 139, 195, 237, 202, 159, 191, 209, 172, 156, 38, 239, 192
                ]
            )))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::PushInverted(Node::KVValueHash(
                vec![3],
                vec![3],
                [
                    210, 173, 26, 11, 185, 253, 244, 69, 11, 216, 113, 81, 192, 139, 153, 104, 205,
                    4, 107, 218, 102, 84, 170, 189, 186, 36, 48, 176, 169, 129, 231, 144
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::ParentInverted));
        assert_eq!(
            iter.next(),
            Some(&Op::PushInverted(Node::Hash([
                121, 235, 207, 195, 143, 58, 159, 120, 166, 33, 151, 45, 178, 124, 91, 233, 201, 4,
                241, 127, 41, 198, 197, 228, 19, 190, 36, 173, 183, 73, 104, 30
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::ChildInverted));
        assert_eq!(iter.next(), Some(&Op::ChildInverted));
        assert_eq!(iter.next(), None);

        assert_eq!(absence, (true, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new_with_direction(false);
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, false, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![8], vec![8]),
                (vec![7], vec![7]),
                (vec![5], vec![5]),
                (vec![4], vec![4]),
                (vec![3], vec![3]),
            ]
        );
    }

    #[test]
    fn range_proof_missing_upper_bound() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_tree_seq(10, grove_version);
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 0, 5]..vec![0, 0, 0, 0, 0, 0, 0, 6, 5],
        )];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
                197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123,
                117, 31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172,
                237, 19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![0, 0, 0, 0, 0, 0, 0, 5],
                vec![123; 60],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![0, 0, 0, 0, 0, 0, 0, 6],
                vec![123; 60],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![0, 0, 0, 0, 0, 0, 0, 7],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
                188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
            ]
        );
    }

    #[test]
    fn range_proof_missing_lower_bound() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_tree_seq(10, grove_version);
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let query_items = vec![
            // 7 is not inclusive
            QueryItem::Range(vec![0, 0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7]),
        ];
        let (proof, absence, ..) = walker
            .create_proof(query_items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut iter = proof.iter();
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                15, 191, 194, 224, 193, 134, 156, 159, 52, 166, 27, 230, 63, 93, 135, 17, 255, 154,
                197, 27, 14, 205, 136, 199, 234, 59, 188, 241, 187, 239, 117, 93
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVHash([
                95, 245, 207, 74, 17, 152, 55, 24, 246, 112, 233, 61, 187, 164, 177, 44, 203, 123,
                117, 31, 98, 233, 121, 106, 202, 39, 49, 163, 56, 243, 123, 176
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                41, 224, 141, 252, 95, 145, 96, 170, 95, 214, 144, 222, 239, 139, 144, 77, 172,
                237, 19, 147, 70, 9, 109, 145, 10, 54, 165, 205, 249, 140, 29, 180
            ])))
        );
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![0, 0, 0, 0, 0, 0, 0, 5],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVValueHash(
                vec![0, 0, 0, 0, 0, 0, 0, 6],
                vec![123; 60],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::KVDigest(
                vec![0, 0, 0, 0, 0, 0, 0, 7],
                [
                    18, 20, 146, 3, 255, 218, 128, 82, 50, 175, 125, 255, 248, 14, 221, 175, 220,
                    56, 190, 183, 81, 241, 201, 175, 242, 210, 209, 100, 99, 235, 119, 243
                ]
            )))
        );
        assert_eq!(iter.next(), Some(&Op::Parent));
        assert_eq!(
            iter.next(),
            Some(&Op::Push(Node::Hash([
                161, 130, 183, 198, 179, 212, 6, 233, 106, 118, 142, 222, 33, 98, 197, 61, 120, 14,
                188, 1, 146, 86, 114, 147, 90, 50, 135, 7, 213, 112, 77, 72
            ])))
        );
        assert_eq!(iter.next(), Some(&Op::Child));
        assert_eq!(iter.next(), Some(&Op::Child));
        assert!(iter.next().is_none());
        assert_eq!(absence, (false, false));

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);
        let mut query = Query::new();
        for item in query_items {
            query.insert_item(item);
        }
        let res = query
            .verify_proof(bytes.as_slice(), None, true, tree.hash().unwrap())
            .unwrap()
            .unwrap();
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60])]
        );
    }

    #[test]
    fn subset_proof() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_tree_seq(10, grove_version);
        let expected_hash = tree.hash().unwrap().to_owned();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        // 1..10 prove range full, subset 7
        let mut query = Query::new();
        query.insert_all();

        let (proof, ..) = walker
            .create_proof(query.items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);

        // subset query
        let mut query = Query::new();
        query.insert_key(vec![0, 0, 0, 0, 0, 0, 0, 6]);

        let res = query
            .verify_proof(bytes.as_slice(), None, true, expected_hash)
            .unwrap()
            .unwrap();

        assert_eq!(res.result_set.len(), 1);
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![(vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60])]
        );

        // 1..10 prove (2..=5, 7..10) subset (3..=4, 7..=8)
        let mut query = Query::new();
        query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 2]..=vec![0, 0, 0, 0, 0, 0, 0, 5]);
        query.insert_range(vec![0, 0, 0, 0, 0, 0, 0, 7]..vec![0, 0, 0, 0, 0, 0, 0, 10]);
        let (proof, ..) = walker
            .create_proof(query.items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);

        let mut query = Query::new();
        query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 3]..=vec![0, 0, 0, 0, 0, 0, 0, 4]);
        query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 7]..=vec![0, 0, 0, 0, 0, 0, 0, 8]);
        let res = query
            .verify_proof(bytes.as_slice(), None, true, expected_hash)
            .unwrap()
            .unwrap();

        assert_eq!(res.result_set.len(), 4);
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 3], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 4], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 7], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 8], vec![123; 60]),
            ]
        );

        // 1..10 prove (2..=5, 6..10) subset (4..=8)
        let mut query = Query::new();
        query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 2]..=vec![0, 0, 0, 0, 0, 0, 0, 5]);
        query.insert_range(vec![0, 0, 0, 0, 0, 0, 0, 6]..vec![0, 0, 0, 0, 0, 0, 0, 10]);
        let (proof, ..) = walker
            .create_proof(query.items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);

        let mut query = Query::new();
        query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 4]..=vec![0, 0, 0, 0, 0, 0, 0, 8]);
        let res = query
            .verify_proof(bytes.as_slice(), None, true, expected_hash)
            .unwrap()
            .unwrap();

        assert_eq!(res.result_set.len(), 5);
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 4], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 6], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 7], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 8], vec![123; 60]),
            ]
        );

        // 1..10 prove (1..=3, 2..=5) subset (1..=5)
        let mut query = Query::new();
        query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 1]..=vec![0, 0, 0, 0, 0, 0, 0, 3]);
        query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 2]..=vec![0, 0, 0, 0, 0, 0, 0, 5]);
        let (proof, ..) = walker
            .create_proof(query.items.as_slice(), None, true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);

        let mut query = Query::new();
        query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 1]..=vec![0, 0, 0, 0, 0, 0, 0, 5]);
        let res = query
            .verify_proof(bytes.as_slice(), None, true, expected_hash)
            .unwrap()
            .unwrap();

        assert_eq!(res.result_set.len(), 5);
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 1], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 2], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 3], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 4], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
            ]
        );

        // 1..10 prove full (..) limit to 5, subset (1..=5)
        let mut query = Query::new();
        query.insert_range_from(vec![0, 0, 0, 0, 0, 0, 0, 1]..);
        let (proof, ..) = walker
            .create_proof(query.items.as_slice(), Some(5), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);

        let mut query = Query::new();
        query.insert_range_inclusive(vec![0, 0, 0, 0, 0, 0, 0, 1]..=vec![0, 0, 0, 0, 0, 0, 0, 5]);
        let res = query
            .verify_proof(bytes.as_slice(), Some(5), true, expected_hash)
            .unwrap()
            .unwrap();

        assert_eq!(res.result_set.len(), 5);
        compare_result_tuples_not_optional!(
            res.result_set,
            vec![
                (vec![0, 0, 0, 0, 0, 0, 0, 1], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 2], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 3], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 4], vec![123; 60]),
                (vec![0, 0, 0, 0, 0, 0, 0, 5], vec![123; 60]),
            ]
        );
    }

    #[test]
    fn break_subset_proof() {
        let grove_version = GroveVersion::latest();
        // TODO: move this to where you'd set the constraints for this definition
        // goal is to show that ones limit and offset values are involved
        // whether a query is subset or not now also depends on the state
        // queries essentially highlight parts of the tree, a query
        // is a subset of another query if all the nodes it highlights
        // are also highlighted by the original query
        // with limit and offset the nodes a query highlights now depends on state
        // hence it's impossible to know if something is subset at definition time

        let mut tree = make_tree_seq(10, grove_version);
        let expected_hash = tree.hash().unwrap().to_owned();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        // 1..10 prove full (..) limit to 3, subset (1..=3)
        let mut query = Query::new();
        query.insert_range_from(vec![0, 0, 0, 0, 0, 0, 0, 1]..);
        let (proof, ..) = walker
            .create_proof(query.items.as_slice(), Some(3), true, grove_version)
            .unwrap()
            .expect("create_proof errored");

        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);

        // Try to query 4
        let mut query = Query::new();
        query.insert_key(vec![0, 0, 0, 0, 0, 0, 0, 4]);
        assert!(query
            .verify_proof(bytes.as_slice(), Some(3), true, expected_hash)
            .unwrap()
            .is_err());

        // if limit offset parameters are different from generation then proof
        // verification returns an error Try superset proof with increased limit
        let mut query = Query::new();
        query.insert_range_from(vec![0, 0, 0, 0, 0, 0, 0, 1]..);
        assert!(query
            .verify_proof(bytes.as_slice(), Some(4), true, expected_hash)
            .unwrap()
            .is_err());

        // Try superset proof with less limit
        let mut query = Query::new();
        query.insert_range_from(vec![0, 0, 0, 0, 0, 0, 0, 1]..);
        assert!(query
            .verify_proof(bytes.as_slice(), Some(2), true, expected_hash)
            .unwrap()
            .is_err());
    }

    #[test]
    fn query_from_vec() {
        let query_items = vec![QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        )];
        let query = Query::from(query_items);

        let mut expected = Vec::new();
        expected.push(QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        ));
        assert_eq!(query.items, expected);
    }

    #[test]
    fn query_into_vec() {
        let mut query = Query::new();
        query.insert_item(QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        ));
        let query_vec: Vec<QueryItem> = query.into();
        let expected = [QueryItem::Range(
            vec![0, 0, 0, 0, 0, 0, 5, 5]..vec![0, 0, 0, 0, 0, 0, 0, 7],
        )];
        assert_eq!(
            query_vec.first().unwrap().lower_bound(),
            expected.first().unwrap().lower_bound()
        );
        assert_eq!(
            query_vec.first().unwrap().upper_bound(),
            expected.first().unwrap().upper_bound()
        );
    }

    #[test]
    fn query_item_from_vec_u8() {
        let query_items: Vec<u8> = vec![42];
        let query = QueryItem::from(query_items);

        let expected = QueryItem::Key(vec![42]);
        assert_eq!(query, expected);
    }

    #[test]
    fn verify_ops() {
        let grove_version = GroveVersion::latest();
        let mut tree = TreeNode::new(vec![5], vec![5], None, BasicMerkNode).unwrap();
        tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
            .unwrap()
            .expect("commit failed");

        let root_hash = tree.hash().unwrap();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let (proof, ..) = walker
            .create_proof(
                vec![QueryItem::Key(vec![5])].as_slice(),
                None,
                true,
                grove_version,
            )
            .unwrap()
            .expect("failed to create proof");
        let mut bytes = vec![];

        encode_into(proof.iter(), &mut bytes);

        let map = verify::verify(&bytes, root_hash).unwrap().unwrap();
        assert_eq!(
            map.get(vec![5].as_slice()).unwrap().unwrap(),
            vec![5].as_slice()
        );
    }

    #[test]
    #[should_panic(expected = "verify failed")]
    fn verify_ops_mismatched_hash() {
        let grove_version = GroveVersion::latest();
        let mut tree = TreeNode::new(vec![5], vec![5], None, BasicMerkNode).unwrap();
        tree.commit(&mut NoopCommit {}, &|_, _| Ok(0))
            .unwrap()
            .expect("commit failed");

        let mut walker = RefWalker::new(&mut tree, PanicSource {});

        let (proof, ..) = walker
            .create_proof(
                vec![QueryItem::Key(vec![5])].as_slice(),
                None,
                true,
                grove_version,
            )
            .unwrap()
            .expect("failed to create proof");
        let mut bytes = vec![];

        encode_into(proof.iter(), &mut bytes);

        let _map = verify::verify(&bytes, [42; 32])
            .unwrap()
            .expect("verify failed");
    }

    #[test]
    #[should_panic(expected = "verify failed")]
    fn verify_query_mismatched_hash() {
        let grove_version = GroveVersion::latest();
        let mut tree = make_3_node_tree();
        let mut walker = RefWalker::new(&mut tree, PanicSource {});
        let keys = vec![vec![5], vec![7]];
        let (proof, ..) = walker
            .create_proof(
                keys.clone()
                    .into_iter()
                    .map(QueryItem::Key)
                    .collect::<Vec<_>>()
                    .as_slice(),
                None,
                true,
                grove_version,
            )
            .unwrap()
            .expect("failed to create proof");
        let mut bytes = vec![];
        encode_into(proof.iter(), &mut bytes);

        let mut query = Query::new();
        for key in keys.iter() {
            query.insert_key(key.clone());
        }

        let _result = query
            .verify_proof(bytes.as_slice(), None, true, [42; 32])
            .unwrap()
            .expect("verify failed");
    }
}
