use indexmap::IndexMap;

use crate::{
    common_path::CommonPathResult, query_item::QueryItem, Query, QueryItemIntersectionResult,
    SubqueryBranch,
};

impl SubqueryBranch {
    fn merge_subquery(
        &self,
        other_default_branch_subquery: Option<Box<Query>>,
    ) -> Option<Box<Query>> {
        match (&self.subquery, other_default_branch_subquery) {
            (None, None) => None,
            (Some(subquery), None) => Some(subquery.clone()),
            (None, Some(subquery)) => Some(subquery),
            (Some(subquery), Some(other_subquery)) => {
                let mut merged_subquery = subquery.clone();
                merged_subquery.merge_with_unchecked(*other_subquery);
                Some(merged_subquery)
            }
        }
    }

    /// Whether either side of a branch merge carries a read mode
    /// anywhere in its subquery. Branch merges combine subqueries by
    /// field, which cannot express "both of these are axis reads" — so
    /// the public entry points refuse rather than drop the mode.
    fn branch_merge_carries_read_mode(&self, other: &Self) -> bool {
        let carries = |branch: &Self| {
            branch
                .subquery
                .as_deref()
                .is_some_and(|subquery| subquery.has_read_mode_anywhere())
        };
        carries(self) || carries(other)
    }

    /// Merges two subquery branches, combining their subquery paths and
    /// subqueries. When paths differ, creates conditional subqueries to
    /// preserve both branches.
    ///
    /// Errors if either side carries a [`ReadMode`](crate::ReadMode)
    /// anywhere: merging combines subqueries field by field, and the
    /// mode is not one of the combined fields, so a merge would answer
    /// key selection where the caller asked for an axis or sum-budget
    /// read.
    pub fn merge(&self, other: &Self) -> Result<Self, crate::error::Error> {
        if self.branch_merge_carries_read_mode(other) {
            return Err(crate::error::Error::NotSupported(
                "can not merge subquery branches carrying read modes (axis / sum-budget reads)"
                    .to_string(),
            ));
        }
        Ok(self.merge_unchecked(other))
    }

    /// The merge body, private: every public path checks read modes
    /// first, and the recursive internals only ever see branches whose
    /// descendants passed that check.
    fn merge_unchecked(&self, other: &Self) -> Self {
        match (&self.subquery_path, &other.subquery_path) {
            (None, None) => {
                // they both just have subqueries without paths
                let subquery = self.merge_subquery(other.subquery.clone());
                SubqueryBranch {
                    subquery_path: None,
                    subquery,
                }
            }
            (Some(our_subquery_path), Some(their_subquery_path)) => {
                // They both have subquery paths

                if our_subquery_path.eq(their_subquery_path) {
                    // The subquery paths are the same
                    // We just need to merge the subqueries together
                    let subquery = self.merge_subquery(other.subquery.clone());
                    SubqueryBranch {
                        subquery_path: Some(our_subquery_path.clone()),
                        subquery,
                    }
                } else {
                    // We need to find the common path between the two subqueries
                    let CommonPathResult {
                        common_path,
                        mut left_path_leftovers,
                        mut right_path_leftovers,
                    } = CommonPathResult::from_paths(our_subquery_path, their_subquery_path);

                    let subquery_path = if common_path.is_empty() {
                        // There is no common path
                        // We set the subquery path to be None
                        None
                    } else {
                        // There is a common path
                        // We can use this common path as a common root
                        Some(common_path)
                    };

                    if !left_path_leftovers.is_empty() && !right_path_leftovers.is_empty() {
                        // Both left and right split but still have paths below them
                        // We take the top element from the left path leftovers and add a
                        // conditional subquery for each key

                        // We need to create a new subquery that will hold the conditional
                        // subqueries
                        let mut merged_query = Query::new();

                        // The key is also removed from the path as it is no needed in the subquery
                        let left_top_key = left_path_leftovers.remove(0);
                        let maybe_left_path_leftovers = if left_path_leftovers.is_empty() {
                            None
                        } else {
                            Some(left_path_leftovers)
                        };
                        merged_query.insert_key(left_top_key.clone());
                        merged_query.merge_conditional_boxed_subquery_unchecked(
                            QueryItem::Key(left_top_key),
                            SubqueryBranch {
                                subquery_path: maybe_left_path_leftovers,
                                subquery: self.subquery.clone(),
                            },
                        );
                        let right_top_key = right_path_leftovers.remove(0);
                        let maybe_right_path_leftovers = if right_path_leftovers.is_empty() {
                            None
                        } else {
                            Some(right_path_leftovers)
                        };

                        merged_query.insert_key(right_top_key.clone());
                        merged_query.merge_conditional_boxed_subquery_unchecked(
                            QueryItem::Key(right_top_key),
                            SubqueryBranch {
                                subquery_path: maybe_right_path_leftovers,
                                subquery: other.subquery.clone(),
                            },
                        );
                        SubqueryBranch {
                            subquery_path,
                            subquery: Some(Box::new(merged_query)),
                        }
                    } else if right_path_leftovers.is_empty() {
                        // this means our subquery path was longer
                        // which means we need to set the default to the right (other)
                        let mut merged_query = other.subquery.clone().unwrap_or_default();
                        let first_key = left_path_leftovers.remove(0);
                        let maybe_left_path_leftovers = if left_path_leftovers.is_empty() {
                            None
                        } else {
                            Some(left_path_leftovers)
                        };

                        merged_query.insert_key(first_key.clone());
                        // our subquery stays the same as we didn't change level
                        // add a conditional subquery for other
                        merged_query.merge_conditional_boxed_subquery_unchecked(
                            QueryItem::Key(first_key),
                            SubqueryBranch {
                                subquery_path: maybe_left_path_leftovers,
                                subquery: self.subquery.clone(),
                            },
                        );
                        SubqueryBranch {
                            subquery_path,
                            subquery: Some(merged_query),
                        }
                    } else if left_path_leftovers.is_empty() {
                        let mut merged_query = self.subquery.clone().unwrap_or_default();
                        // this means our subquery path shorter
                        // we should keep our subquery
                        let other_first = right_path_leftovers.remove(0);

                        let maybe_right_path_leftovers = if right_path_leftovers.is_empty() {
                            None
                        } else {
                            Some(right_path_leftovers)
                        };
                        merged_query.insert_key(other_first.clone());
                        // our subquery stays the same as we didn't change level
                        // add a conditional subquery for other
                        merged_query.merge_conditional_boxed_subquery_unchecked(
                            QueryItem::Key(other_first),
                            SubqueryBranch {
                                subquery_path: maybe_right_path_leftovers,
                                subquery: other.subquery.clone(),
                            },
                        );
                        SubqueryBranch {
                            subquery_path,
                            subquery: Some(merged_query),
                        }
                    } else {
                        unreachable!("Unreachable as both paths being equal already covered");
                    }
                }
            }
            (Some(our_subquery_path), None) => {
                // Ours has a subquery path, theirs does not.
                // We set the subquery path to None

                let mut our_subquery_path = our_subquery_path.clone();

                // take their subquery as it will be on a topmost layer
                let mut merged_subquery = other.subquery.clone().unwrap_or_default();

                // We need to add a conditional subquery for ours

                let our_top_key = our_subquery_path.remove(0);

                merged_subquery.insert_key(our_top_key.clone());

                let maybe_our_subquery_path = if our_subquery_path.is_empty() {
                    None
                } else {
                    Some(our_subquery_path)
                };
                // our subquery stays the same as we didn't change level
                // add a conditional subquery for other
                merged_subquery.merge_conditional_boxed_subquery_unchecked(
                    // there are no conditional subquery branches yes
                    QueryItem::Key(our_top_key),
                    SubqueryBranch {
                        subquery_path: maybe_our_subquery_path,
                        subquery: self.subquery.clone(),
                    },
                );

                SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(merged_subquery),
                }
            }
            (None, Some(their_subquery_path)) => {
                // They have a subquery path, we does not.
                // We set the subquery path to None

                let mut their_subquery_path = their_subquery_path.clone();

                // take our subquery as it will be on a topmost layer
                let mut merged_subquery = self.subquery.clone().unwrap_or_default();

                // The subquery_path is already set to None, no need to set it again

                let their_top_key = their_subquery_path.remove(0);

                merged_subquery.insert_key(their_top_key.clone());

                let maybe_their_subquery_path = if their_subquery_path.is_empty() {
                    None
                } else {
                    Some(their_subquery_path)
                };
                // their subquery stays the same as we didn't change level
                // add a conditional subquery for other
                merged_subquery.merge_conditional_boxed_subquery_unchecked(
                    // there are no conditional subquery branches yes
                    QueryItem::Key(their_top_key),
                    SubqueryBranch {
                        subquery_path: maybe_their_subquery_path,
                        subquery: other.subquery.clone(),
                    },
                );

                SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(merged_subquery),
                }
            }
        }
    }
}

impl Query {
    fn merge_default_subquerys_branch_subquery(
        &mut self,
        other_default_branch_subquery: Option<Box<Query>>,
    ) {
        if let Some(current_subquery) = self.default_subquery_branch.subquery.as_mut() {
            if let Some(other_subquery) = other_default_branch_subquery {
                current_subquery.merge_with_unchecked(*other_subquery);
            }
        } else {
            // None existed yet
            self.default_subquery_branch.subquery = other_default_branch_subquery.clone();
        }
    }

    /// Merges the subquery for the query with the current subquery. Subqueries
    /// causes every element that is returned by the query to be subqueried
    /// or subqueried to the subquery_path/subquery if a subquery is
    /// present. Merging involves creating conditional subqueries in the
    /// subqueries subqueries and paths.
    ///
    /// Errors if either side carries a [`ReadMode`](crate::ReadMode)
    /// anywhere — see [`SubqueryBranch::merge`] for why a branch merge
    /// cannot carry one through.
    pub fn merge_default_subquery_branch(
        &mut self,
        other_default_subquery_branch: SubqueryBranch,
    ) -> Result<(), crate::error::Error> {
        let carries = |branch: &SubqueryBranch| {
            branch
                .subquery
                .as_deref()
                .is_some_and(|subquery| subquery.has_read_mode_anywhere())
        };
        if carries(&self.default_subquery_branch) || carries(&other_default_subquery_branch) {
            return Err(crate::error::Error::NotSupported(
                "can not merge a default subquery branch carrying a read mode (axis / \
                 sum-budget read)"
                    .to_string(),
            ));
        }
        self.merge_default_subquery_branch_unchecked(other_default_subquery_branch);
        Ok(())
    }

    /// The merge body, private: see [`SubqueryBranch::merge_unchecked`].
    fn merge_default_subquery_branch_unchecked(
        &mut self,
        other_default_subquery_branch: SubqueryBranch,
    ) {
        match (
            &self.default_subquery_branch.subquery_path,
            &other_default_subquery_branch.subquery_path,
        ) {
            (None, None) => {
                // they both just have subqueries without paths
                self.merge_default_subquerys_branch_subquery(
                    other_default_subquery_branch.subquery,
                );
            }
            (Some(our_subquery_path), Some(their_subquery_path)) => {
                // They both have subquery paths

                if our_subquery_path.eq(their_subquery_path) {
                    // The subquery paths are the same
                    // We just need to merge the subqueries together
                    self.merge_default_subquerys_branch_subquery(
                        other_default_subquery_branch.subquery,
                    );
                } else {
                    // We need to find the common path between the two subqueries
                    let CommonPathResult {
                        common_path,
                        mut left_path_leftovers,
                        mut right_path_leftovers,
                    } = CommonPathResult::from_paths(our_subquery_path, their_subquery_path);

                    if common_path.is_empty() {
                        // There is no common path
                        // We set the subquery path to be None
                        self.default_subquery_branch.subquery_path = None;
                    } else {
                        // There is a common path
                        // We can use this common path as a common root
                        self.default_subquery_branch.subquery_path = Some(common_path)
                    }

                    if !left_path_leftovers.is_empty() && !right_path_leftovers.is_empty() {
                        // Both left and right split but still have paths below them
                        // We take the top element from the left path leftovers and add a
                        // conditional subquery for each key
                        // The key is also removed from the path as it is no needed in the subquery
                        let left_top_key = left_path_leftovers.remove(0);
                        let maybe_left_path_leftovers = if left_path_leftovers.is_empty() {
                            None
                        } else {
                            Some(left_path_leftovers)
                        };
                        self.merge_conditional_boxed_subquery_unchecked(
                            QueryItem::Key(left_top_key),
                            SubqueryBranch {
                                subquery_path: maybe_left_path_leftovers,
                                subquery: self.default_subquery_branch.subquery.clone(),
                            },
                        );
                        let right_top_key = right_path_leftovers.remove(0);
                        let maybe_right_path_leftovers = if right_path_leftovers.is_empty() {
                            None
                        } else {
                            Some(right_path_leftovers)
                        };

                        self.merge_conditional_boxed_subquery_unchecked(
                            QueryItem::Key(right_top_key),
                            SubqueryBranch {
                                subquery_path: maybe_right_path_leftovers,
                                subquery: other_default_subquery_branch.subquery.clone(),
                            },
                        );
                    } else if right_path_leftovers.is_empty() {
                        let left_subquery = self.default_subquery_branch.subquery.clone();
                        // this means our subquery path was longer
                        // which means we need to set the default to the right (other)
                        self.default_subquery_branch.subquery =
                            other_default_subquery_branch.subquery.clone();
                        let first_key = left_path_leftovers.remove(0);
                        let maybe_left_path_leftovers = if left_path_leftovers.is_empty() {
                            None
                        } else {
                            Some(left_path_leftovers)
                        };

                        // our subquery stays the same as we didn't change level
                        // add a conditional subquery for other
                        self.merge_conditional_boxed_subquery_unchecked(
                            QueryItem::Key(first_key),
                            SubqueryBranch {
                                subquery_path: maybe_left_path_leftovers,
                                subquery: left_subquery,
                            },
                        );
                    } else if left_path_leftovers.is_empty() {
                        // this means our subquery path shorter
                        // we should keep our subquery
                        let other_first = right_path_leftovers.remove(0);

                        let maybe_right_path_leftovers = if right_path_leftovers.is_empty() {
                            None
                        } else {
                            Some(right_path_leftovers)
                        };
                        // our subquery stays the same as we didn't change level
                        // add a conditional subquery for other
                        self.merge_conditional_boxed_subquery_unchecked(
                            QueryItem::Key(other_first),
                            SubqueryBranch {
                                subquery_path: maybe_right_path_leftovers,
                                subquery: other_default_subquery_branch.subquery.clone(),
                            },
                        );
                    } else {
                        unreachable!("Unreachable as both paths being equal already covered");
                    }
                }
            }
            (Some(our_subquery_path), None) => {
                // Ours has a subquery path, theirs does not.
                // We set the subquery path to None

                let mut our_subquery_path = our_subquery_path.clone();

                // Save our subquery before overwriting with theirs
                let our_subquery = self.default_subquery_branch.subquery.clone();

                self.default_subquery_branch.subquery_path = None;
                self.default_subquery_branch.subquery =
                    other_default_subquery_branch.subquery.clone();
                // We need to add a conditional subquery for ours

                let our_top_key = our_subquery_path.remove(0);

                let maybe_our_subquery_path = if our_subquery_path.is_empty() {
                    None
                } else {
                    Some(our_subquery_path)
                };
                // our subquery stays the same as we didn't change level
                // add a conditional subquery for other
                self.merge_conditional_boxed_subquery_unchecked(
                    QueryItem::Key(our_top_key),
                    SubqueryBranch {
                        subquery_path: maybe_our_subquery_path,
                        subquery: our_subquery,
                    },
                );
            }
            (None, Some(their_subquery_path)) => {
                // They have a subquery path, we does not.
                // We set the subquery path to None

                let mut their_subquery_path = their_subquery_path.clone();

                // The subquery_path is already set to None, no need to set it again

                let their_top_key = their_subquery_path.remove(0);

                let maybe_their_subquery_path = if their_subquery_path.is_empty() {
                    None
                } else {
                    Some(their_subquery_path)
                };
                // our subquery stays the same as we didn't change level
                // add a conditional subquery for other
                self.merge_conditional_boxed_subquery_unchecked(
                    QueryItem::Key(their_top_key),
                    SubqueryBranch {
                        subquery_path: maybe_their_subquery_path,
                        subquery: other_default_subquery_branch.subquery.clone(),
                    },
                );
            }
        }
    }

    fn merge_default_subquery_branch_for_remaining_items(
        &mut self,
        old_items: Vec<QueryItem>,
        items: Vec<QueryItem>,
        default_subquery_branch: SubqueryBranch,
    ) {
        if items.is_empty() {
            return;
        }

        // Start with old items, then subtract any that already have conditional
        // subquery branches. Those items were not using the default.
        let mut old_default_items = old_items;
        if let Some(existing_conditionals) = self.conditional_subquery_branches.as_ref() {
            for conditional_item in existing_conditionals.keys().cloned() {
                let remaining = QueryItem::intersect_many_ordered(
                    &mut old_default_items,
                    vec![conditional_item],
                );
                old_default_items = remaining.ours.unwrap_or_default();
                if old_default_items.is_empty() {
                    break;
                }
            }
        }

        if old_default_items.is_empty() {
            // No old items were using the default, or no old items existed. Just
            // apply the incoming default directly.
            for item in items {
                self.merge_conditional_boxed_subquery_unchecked(
                    item,
                    default_subquery_branch.clone(),
                );
            }
            return;
        }

        let intersection = QueryItem::intersect_many_ordered(&mut old_default_items, items);

        // Items in both queries that were using the default: promote our
        // default, then merge with the incoming default.
        if let Some(in_both) = intersection.in_both {
            let existing_default = self.default_subquery_branch.clone();
            for item in in_both {
                if existing_default.subquery.is_some() || existing_default.subquery_path.is_some() {
                    self.merge_conditional_boxed_subquery_unchecked(
                        item.clone(),
                        existing_default.clone(),
                    );
                }
                self.merge_conditional_boxed_subquery_unchecked(
                    item,
                    default_subquery_branch.clone(),
                );
            }
        }

        // Items only in the incoming query, or overlapping old conditional
        // items, just get the incoming default.
        if let Some(theirs_only) = intersection.theirs {
            for item in theirs_only {
                self.merge_conditional_boxed_subquery_unchecked(
                    item,
                    default_subquery_branch.clone(),
                );
            }
        }
    }

    /// Merges multiple queries into a single query. Items are unioned and
    /// conditional subquery branches are merged where they intersect.
    ///
    /// Errors if any input carries a [`ReadMode`](crate::ReadMode) at
    /// any nesting level — axis and sum-budget reads have no defined
    /// merge semantics, and merging them silently as key selection
    /// would change what the query means.
    ///
    /// The **direction** (`left_to_right`) of every query after the
    /// first is discarded — the merged query keeps the first query's
    /// direction, at every nesting level. Use
    /// [`Self::merge_multiple_directional`] to require agreement
    /// instead of silently keeping the first.
    pub fn merge_multiple(queries: Vec<Query>) -> Result<Self, crate::error::Error> {
        for query in &queries {
            if query.has_read_mode_anywhere() {
                return Err(crate::error::Error::NotSupported(
                    "cannot merge queries carrying read modes (axis / sum-budget reads)"
                        .to_string(),
                ));
            }
        }
        Ok(Self::merge_multiple_unchecked(queries))
    }

    /// [`Self::merge_multiple`] with **direction agreement**: every
    /// input's top-level `left_to_right` must match, and the merged
    /// query carries it, instead of silently keeping the first query's
    /// direction. (Directions at deeper nesting levels still follow the
    /// first-branch-wins behavior of the underlying merge.)
    pub fn merge_multiple_directional(queries: Vec<Query>) -> Result<Self, crate::error::Error> {
        if let Some(first) = queries.first() {
            let direction = first.left_to_right;
            if queries.iter().any(|query| query.left_to_right != direction) {
                return Err(crate::error::Error::NotSupported(
                    "cannot merge queries with conflicting directions (left_to_right differs)"
                        .to_string(),
                ));
            }
        }
        Self::merge_multiple(queries)
    }

    /// The merge body, private. The `_unchecked` family is the whole
    /// recursive machinery; the invariant that keeps it sound is that
    /// **every** public entry point rejects read modes before calling
    /// in — the whole-query merges (`merge_multiple`,
    /// `merge_multiple_directional`, `merge_with`) and the branch
    /// merges alike (`SubqueryBranch::merge`,
    /// `merge_default_subquery_branch`,
    /// `merge_conditional_boxed_subquery`,
    /// `merge_conditional_subquery_branches_with_new_at_query_item`).
    /// A new public wrapper that skips the check reintroduces the
    /// silent-drop bug, because these bodies destructure `read_mode`
    /// away by design.
    fn merge_multiple_unchecked(mut queries: Vec<Query>) -> Self {
        if queries.is_empty() {
            return Query::new();
        }
        // slight performance increase with swap remove as we don't care about the
        // ordering
        let mut merged_query = queries.swap_remove(0);
        for query in queries {
            let Query {
                mut items,
                default_subquery_branch,
                conditional_subquery_branches,
                left_to_right: _,
                add_parent_tree_on_subquery,
                // Checked by the public wrappers; read-mode-free here.
                read_mode: _,
            } = query;
            // Preserve add_parent_tree_on_subquery if any query requests it
            if add_parent_tree_on_subquery {
                merged_query.add_parent_tree_on_subquery = true;
            }

            // Save pre-merge items so we can detect overlapping items later
            let old_items = merged_query.items.clone();

            // the searched for items are the union of all items
            merged_query.insert_items(items.clone());

            if let Some(conditional_subquery_branches) = conditional_subquery_branches {
                // if there are conditional subqueries
                // we need to remove from our items the conditional items

                for (conditional_item, conditional_subquery_branch) in conditional_subquery_branches
                {
                    merged_query.merge_conditional_boxed_subquery_unchecked(
                        conditional_item.clone(),
                        conditional_subquery_branch,
                    );
                    if !items.is_empty() {
                        let intersection_result =
                            QueryItem::intersect_many_ordered(&mut items, vec![conditional_item]);
                        items = intersection_result.ours.unwrap_or_default();
                    }
                }
            }

            merged_query.merge_default_subquery_branch_for_remaining_items(
                old_items,
                items,
                default_subquery_branch,
            );
        }
        merged_query
    }

    /// Merges another query into this one, combining items and conditional
    /// subquery branches.
    ///
    /// Errors if either side carries a [`ReadMode`](crate::ReadMode) at
    /// any nesting level — see [`Self::merge_multiple`]. `other`'s
    /// direction is discarded (this query's is kept), matching the
    /// long-standing merge behavior.
    pub fn merge_with(&mut self, other: Query) -> Result<(), crate::error::Error> {
        if self.has_read_mode_anywhere() || other.has_read_mode_anywhere() {
            return Err(crate::error::Error::NotSupported(
                "cannot merge queries carrying read modes (axis / sum-budget reads)".to_string(),
            ));
        }
        self.merge_with_unchecked(other);
        Ok(())
    }

    /// The merge-with body, private: see [`Self::merge_multiple_unchecked`].
    fn merge_with_unchecked(&mut self, other: Query) {
        let Query {
            mut items,
            default_subquery_branch,
            conditional_subquery_branches,
            left_to_right: _,
            add_parent_tree_on_subquery,
            // Checked by the public wrappers; read-mode-free here.
            read_mode: _,
        } = other;
        // Preserve add_parent_tree_on_subquery if either query requests it
        if add_parent_tree_on_subquery {
            self.add_parent_tree_on_subquery = true;
        }

        // Save pre-merge items so we can detect overlapping items later
        let old_items = self.items.clone();

        self.insert_items(items.clone());

        if let Some(conditional_subquery_branches) = conditional_subquery_branches {
            for (conditional_item, conditional_subquery_branch) in conditional_subquery_branches {
                self.merge_conditional_boxed_subquery_unchecked(
                    conditional_item.clone(),
                    conditional_subquery_branch,
                );

                if !items.is_empty() {
                    let intersection_result =
                        QueryItem::intersect_many_ordered(&mut items, vec![conditional_item]);
                    items = intersection_result.ours.unwrap_or_default();
                }
            }
        }

        self.merge_default_subquery_branch_for_remaining_items(
            old_items,
            items,
            default_subquery_branch,
        );
    }

    /// Adds a conditional subquery. A conditional subquery replaces the default
    /// subquery and subquery_path if the item matches for the key. If
    /// multiple conditional subquery items match, then the first one that
    /// matches is used (in order that they were added).
    ///
    /// Errors if EITHER side carries a [`ReadMode`](crate::ReadMode)
    /// anywhere — the receiving query's existing conditional branches
    /// as well as the incoming one. A one-sided check is not enough:
    /// merging an overlapping item splits both sides' branches and
    /// recombines them through
    /// [`merge_with_unchecked`](Query::merge_with_unchecked), so an
    /// existing read mode can end up governing a different key range
    /// than the caller established. See [`SubqueryBranch::merge`] for
    /// why a branch merge cannot carry one through.
    pub fn merge_conditional_boxed_subquery(
        &mut self,
        query_item_merging_in: QueryItem,
        subquery_branch_merging_in: SubqueryBranch,
    ) -> Result<(), crate::error::Error> {
        if self.has_read_mode_anywhere()
            || subquery_branch_merging_in
                .subquery
                .as_deref()
                .is_some_and(|subquery| subquery.has_read_mode_anywhere())
        {
            return Err(crate::error::Error::NotSupported(
                "can not merge a conditional subquery branch carrying a read mode (axis / \
                 sum-budget read)"
                    .to_string(),
            ));
        }
        self.merge_conditional_boxed_subquery_unchecked(
            query_item_merging_in,
            subquery_branch_merging_in,
        );
        Ok(())
    }

    /// The merge body, private: see [`SubqueryBranch::merge_unchecked`].
    fn merge_conditional_boxed_subquery_unchecked(
        &mut self,
        query_item_merging_in: QueryItem,
        subquery_branch_merging_in: SubqueryBranch,
    ) {
        if subquery_branch_merging_in.subquery.is_some()
            || subquery_branch_merging_in.subquery_path.is_some()
        {
            self.conditional_subquery_branches = Some(
                Self::merge_conditional_subquery_branches_with_new_at_query_item_unchecked(
                    self.conditional_subquery_branches.take(),
                    query_item_merging_in,
                    subquery_branch_merging_in,
                ),
            );
        }
    }

    /// Adds a conditional subquery. A conditional subquery replaces the default
    /// subquery and subquery_path if the item matches for the key. If
    /// multiple conditional subquery items match, then the first one that
    /// matches is used (in order that they were added).
    ///
    /// Errors if EITHER the existing branch map or the incoming branch
    /// carries a [`ReadMode`](crate::ReadMode) anywhere — see
    /// [`Query::merge_conditional_boxed_subquery`] for why the existing
    /// side has to be inspected too.
    pub fn merge_conditional_subquery_branches_with_new_at_query_item(
        conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
        query_item_merging_in: QueryItem,
        subquery_branch_merging_in: SubqueryBranch,
    ) -> Result<IndexMap<QueryItem, SubqueryBranch>, crate::error::Error> {
        let existing_carries = conditional_subquery_branches
            .as_ref()
            .is_some_and(|branches| {
                branches.values().any(|branch| {
                    branch
                        .subquery
                        .as_deref()
                        .is_some_and(|subquery| subquery.has_read_mode_anywhere())
                })
            });
        if existing_carries
            || subquery_branch_merging_in
                .subquery
                .as_deref()
                .is_some_and(|subquery| subquery.has_read_mode_anywhere())
        {
            return Err(crate::error::Error::NotSupported(
                "can not merge a conditional subquery branch carrying a read mode (axis / \
                 sum-budget read)"
                    .to_string(),
            ));
        }
        Ok(
            Self::merge_conditional_subquery_branches_with_new_at_query_item_unchecked(
                conditional_subquery_branches,
                query_item_merging_in,
                subquery_branch_merging_in,
            ),
        )
    }

    /// The merge body, private: see [`SubqueryBranch::merge_unchecked`].
    fn merge_conditional_subquery_branches_with_new_at_query_item_unchecked(
        conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
        query_item_merging_in: QueryItem,
        subquery_branch_merging_in: SubqueryBranch,
    ) -> IndexMap<QueryItem, SubqueryBranch> {
        let mut merged_items: IndexMap<QueryItem, SubqueryBranch> = IndexMap::new();
        // first we need to check if there are already conditional subquery branches
        // because if there are none then we just assign the new conditional subquery
        // branch instead of merging it in
        if let Some(conditional_subquery_branches) = conditional_subquery_branches {
            // There were conditional subquery branches
            // We create a vector of the query item we are merging in
            // On the first loop this is a continuous query item (for example a range)
            // However as we find things that intersect with it, it might break
            // Example:
            // *On first pass:
            // **Current Subqueries:                   -----------------      --------
            // ----- **Conditional Subquery merging in:
            // ------------------------------------ **After first query:
            // --*****************----------------- We then feed back in
            // *On second pass:
            // **Current Subqueries:                   -----------------      --------
            // ----- **Conditional Subquery merging in:    --
            // ----------------- Lets say M is the one we are merging in and 1,
            // 2 and 3 are the previous conditional Suqueries
            // In the end we will have:              MM11111111111111111MMMMMM22222222MMM
            // 33333
            let mut sub_query_items_merging_in_vec = vec![query_item_merging_in];
            for (original_query_item, subquery_branch) in conditional_subquery_branches {
                let mut new_query_items_merging_in_vec = vec![];
                let mut hit = false;
                for sub_query_item_merging_in in sub_query_items_merging_in_vec {
                    let QueryItemIntersectionResult {
                        in_both,
                        ours_left,
                        ours_right,
                        theirs_left,
                        theirs_right,
                    } = original_query_item.intersect(&sub_query_item_merging_in);
                    if let Some(in_both) = in_both {
                        if !hit {
                            hit = true;
                        }
                        // merge the overlapping subquery branches
                        let merged_subquery_branch =
                            subquery_branch.merge_unchecked(&subquery_branch_merging_in);
                        merged_items.insert(in_both, merged_subquery_branch);

                        match (ours_left, ours_right, theirs_left, theirs_right) {
                            (None, None, None, None) => {}
                            (Some(ours_left), None, None, None) => {
                                merged_items.insert(ours_left, subquery_branch.clone());
                            }
                            (None, Some(ours_right), None, None) => {
                                merged_items.insert(ours_right, subquery_branch.clone());
                            }
                            (Some(ours_left), Some(ours_right), None, None) => {
                                merged_items.insert(ours_left, subquery_branch.clone());
                                merged_items.insert(ours_right, subquery_branch.clone());
                            }
                            (None, None, Some(theirs_left), None) => {
                                new_query_items_merging_in_vec.push(theirs_left);
                            }
                            (Some(ours_left), None, Some(theirs_left), None) => {
                                merged_items.insert(ours_left, subquery_branch.clone());
                                new_query_items_merging_in_vec.push(theirs_left);
                            }
                            (None, Some(ours_right), Some(theirs_left), None) => {
                                merged_items.insert(ours_right, subquery_branch.clone());
                                new_query_items_merging_in_vec.push(theirs_left);
                            }
                            (Some(ours_left), Some(ours_right), Some(theirs_left), None) => {
                                merged_items.insert(ours_left, subquery_branch.clone());
                                merged_items.insert(ours_right, subquery_branch.clone());
                                new_query_items_merging_in_vec.push(theirs_left);
                            }
                            (None, None, None, Some(theirs_right)) => {
                                new_query_items_merging_in_vec.push(theirs_right);
                            }
                            (Some(ours_left), None, None, Some(theirs_right)) => {
                                merged_items.insert(ours_left, subquery_branch.clone());
                                new_query_items_merging_in_vec.push(theirs_right);
                            }
                            (None, Some(ours_right), None, Some(theirs_right)) => {
                                merged_items.insert(ours_right, subquery_branch.clone());
                                new_query_items_merging_in_vec.push(theirs_right);
                            }
                            (Some(ours_left), Some(ours_right), None, Some(theirs_right)) => {
                                merged_items.insert(ours_left, subquery_branch.clone());
                                merged_items.insert(ours_right, subquery_branch.clone());
                                new_query_items_merging_in_vec.push(theirs_right);
                            }
                            (None, None, Some(theirs_left), Some(theirs_right)) => {
                                new_query_items_merging_in_vec.push(theirs_left);
                                new_query_items_merging_in_vec.push(theirs_right);
                            }

                            (Some(ours_left), None, Some(theirs_left), Some(theirs_right)) => {
                                merged_items.insert(ours_left, subquery_branch.clone());
                                new_query_items_merging_in_vec.push(theirs_left);
                                new_query_items_merging_in_vec.push(theirs_right);
                            }
                            (None, Some(ours_right), Some(theirs_left), Some(theirs_right)) => {
                                merged_items.insert(ours_right, subquery_branch.clone());
                                new_query_items_merging_in_vec.push(theirs_left);
                                new_query_items_merging_in_vec.push(theirs_right);
                            }
                            (
                                Some(ours_left),
                                Some(ours_right),
                                Some(theirs_left),
                                Some(theirs_right),
                            ) => {
                                merged_items.insert(ours_left, subquery_branch.clone());
                                merged_items.insert(ours_right, subquery_branch.clone());
                                new_query_items_merging_in_vec.push(theirs_left);
                                new_query_items_merging_in_vec.push(theirs_right);
                            }
                        }
                    } else {
                        // there was no overlap
                        // re-add to merged_items
                        new_query_items_merging_in_vec.push(sub_query_item_merging_in);
                    }
                }
                if !hit {
                    merged_items.insert(original_query_item, subquery_branch.clone());
                }
                sub_query_items_merging_in_vec = new_query_items_merging_in_vec;
            }
            for split_item in sub_query_items_merging_in_vec {
                merged_items.insert(split_item, subquery_branch_merging_in.clone());
            }
        } else {
            merged_items.insert(query_item_merging_in, subquery_branch_merging_in);
        }
        merged_items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Query;

    /// Demonstrates that merging a query with a subquery_path into one without
    /// must preserve the original (self) subquery in the conditional branch,
    /// not duplicate the other's subquery.
    #[test]
    fn merge_default_subquery_branch_preserves_self_subquery() {
        // "self" query: has subquery_path ["a"] and its own subquery selecting key "self_key"
        let mut self_query = Query::new();
        let mut self_subquery = Query::new();
        self_subquery.insert_key(b"self_key".to_vec());

        self_query.default_subquery_branch = SubqueryBranch {
            subquery_path: Some(vec![b"a".to_vec()]),
            subquery: Some(Box::new(self_subquery.clone())),
        };

        // "other" branch: no subquery_path, has its own subquery selecting key "other_key"
        let mut other_subquery = Query::new();
        other_subquery.insert_key(b"other_key".to_vec());

        let other_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(other_subquery.clone())),
        };

        self_query.merge_default_subquery_branch(other_branch);

        // After merge:
        // - default subquery should be other's (no path, so it applies broadly)
        assert_eq!(self_query.default_subquery_branch.subquery_path, None);
        assert_eq!(
            self_query.default_subquery_branch.subquery,
            Some(Box::new(other_subquery.clone())),
            "default subquery should be other's subquery"
        );

        // - conditional branch at key "a" should have self's ORIGINAL subquery
        let conditionals = self_query
            .conditional_subquery_branches
            .expect("should have conditional branches");
        let branch_a = conditionals
            .get(&QueryItem::Key(b"a".to_vec()))
            .expect("should have conditional branch for key 'a'");

        // This is the critical assertion: the conditional branch must contain
        // self's original subquery (selecting "self_key"), NOT other's subquery.
        assert_eq!(
            branch_a.subquery,
            Some(Box::new(self_subquery)),
            "conditional branch should preserve self's original subquery, not other's"
        );
    }
}
