mod merge;
mod insert;

use std::fmt;
use std::ops::RangeFull;
use bincode::{Decode, Encode};
use crate::proofs::query::{Key, QueryItem};

/// `AggregateSumQuery` represents one or more keys or ranges of keys, which can be used to
/// resolve a proof which will include all the requested values
#[derive(Debug, Default, Clone, PartialEq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AggregateSumQuery {
    /// Items
    pub items: Vec<QueryItem>,
    /// Left to right?
    pub left_to_right: bool,
    /// The amount above which we should stop
    /// For example if we have sum nodes with 5 and 10, and we have 15, we should stop looking
    /// At elements when we get to 15
    pub sum_limit: u64,
    /// The max amount of nodes we should check
    pub limit_of_items_to_check: Option<u16>,
}

impl fmt::Display for AggregateSumQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let direction = if self.left_to_right { "→" } else { "←" };
        writeln!(f, "AggregateSumQuery [direction: {}, sum_limit: {}]", direction, self.sum_limit)?;
        writeln!(f, "Items:")?;
        for item in &self.items {
            writeln!(f, "  - {}", item)?;
        }
        Ok(())
    }
}

impl AggregateSumQuery {
    /// Creates a new query which contains all items.
    pub fn new(sum_limit: u64, limit_of_items_to_check: Option<u16>) -> Self {
        Self::new_range_full(sum_limit, limit_of_items_to_check)
    }

    /// Creates a new query which contains all items and ordered by keys descending
    pub fn new_descending(sum_limit: u64, limit_of_items_to_check: Option<u16>) -> Self {
        Self::new_range_full_descending(sum_limit, limit_of_items_to_check)
    }


    /// Creates a new query which contains all items.
    pub fn new_range_full(sum_limit: u64, limit_of_items_to_check: Option<u16>) -> Self {
        Self {
            items: vec![QueryItem::RangeFull(RangeFull)],
            left_to_right: true,
            sum_limit,
            limit_of_items_to_check,
        }
    }

    /// Creates a new query which contains all items and ordered by keys descending
    pub fn new_range_full_descending(sum_limit: u64, limit_of_items_to_check: Option<u16>) -> Self {
        Self {
            items: vec![QueryItem::RangeFull(RangeFull)],
            left_to_right: false,
            sum_limit,
            limit_of_items_to_check,
        }
    }

    /// Creates a new query which contains only one key.
    /// We will basically only check this key to see if we are hitting the sum limit in one element
    pub fn new_single_key(key: Vec<u8>, sum_limit: u64) -> Self {
        Self {
            items: vec![QueryItem::Key(key)],
            left_to_right: true,
            sum_limit,
            limit_of_items_to_check: Some(1),
        }
    }

    /// Creates a new query which contains only one item.
    pub fn new_single_query_item(query_item: QueryItem, sum_limit: u64, limit_of_items_to_check: Option<u16>) -> Self {
        Self {
            items: vec![query_item],
            left_to_right: true,
            sum_limit,
            limit_of_items_to_check,
        }
    }

    /// Creates a new query which contains multiple items.
    pub fn new_with_query_items(query_items: Vec<QueryItem>, sum_limit: u64, limit_of_items_to_check: Option<u16>) -> Self {
        Self {
            items: query_items,
            left_to_right: true,
            sum_limit,
            limit_of_items_to_check,
        }
    }

    /// Creates a new query which contains multiple items.
    pub fn new_with_keys(keys: Vec<Key>, sum_limit: u64, limit_of_items_to_check: Option<u16>) -> Self {
        Self {
            items: keys.into_iter().map(QueryItem::Key).collect(),
            left_to_right: true,
            sum_limit,
            limit_of_items_to_check,
        }
    }

    /// Creates a new query which contains multiple keys.
    pub fn new_with_keys_reversed(keys: Vec<Key>, sum_limit: u64, limit_of_items_to_check: Option<u16>) -> Self {
        Self {
            items: keys.into_iter().map(QueryItem::Key).collect(),
            left_to_right: false,
            sum_limit,
            limit_of_items_to_check,
        }
    }

    /// Creates a new query which contains only one item with the specified
    /// direction.
    pub fn new_single_query_item_with_direction(
        query_item: QueryItem,
        left_to_right: bool,
        sum_limit: u64,
        limit_of_items_to_check: Option<u16>,
    ) -> Self {
        Self {
            items: vec![query_item],
            left_to_right,
            sum_limit,
            limit_of_items_to_check,
        }
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

    /// Check if there are only keys
    pub fn has_only_keys(&self) -> bool {
        // checks if all searched for items are keys
        self.items.iter().all(|a| a.is_key())
    }
}