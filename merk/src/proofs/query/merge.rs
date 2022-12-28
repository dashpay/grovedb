use indexmap::IndexMap;

use crate::proofs::{
    query::{
        common_path::CommonPathResult, query_item::QueryItem, Path, QueryItemIntersectionResult,
        SubqueryBranch,
    },
    Query,
};

#[cfg(any(feature = "full", feature = "verify"))]
impl Query {
    fn merge_default_subquerys_branch_subquery(
        &mut self,
        other_default_branch_subquery: Option<Box<Query>>,
    ) {
        if let Some(current_subquery) = self.default_subquery_branch.subquery.as_mut() {
            if let Some(other_subquery) = other_default_branch_subquery {
                current_subquery.merge_with(*other_subquery);
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
    pub fn merge_default_subquery_branch(&mut self, other_default_subquery_branch: SubqueryBranch) {
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
                        self.merge_conditional_boxed_subquery(
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

                        self.merge_conditional_boxed_subquery(
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
                        self.merge_conditional_boxed_subquery(
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
                        self.merge_conditional_boxed_subquery(
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
                self.merge_conditional_boxed_subquery(
                    QueryItem::Key(our_top_key),
                    SubqueryBranch {
                        subquery_path: maybe_our_subquery_path,
                        subquery: other_default_subquery_branch.subquery.clone(),
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
                self.merge_conditional_boxed_subquery(
                    QueryItem::Key(their_top_key),
                    SubqueryBranch {
                        subquery_path: maybe_their_subquery_path,
                        subquery: other_default_subquery_branch.subquery.clone(),
                    },
                );
            }
        }
    }

    pub fn merge_multiple(queries: Vec<Query>) -> Self {
        let mut merged_query = Query::new();
        for query in queries {
            let Query {
                items,
                default_subquery_branch,
                conditional_subquery_branches,
                left_to_right,
            } = query;
            // merge query items as they point to the same context
            for item in items {
                merged_query.insert_item(item);
            }
            merged_query.merge_default_subquery_branch(default_subquery_branch);
            if let Some(conditional_subquery_branches) = conditional_subquery_branches {
                for (item, conditional_subquery_branch) in conditional_subquery_branches {
                    merged_query
                        .merge_conditional_boxed_subquery(item.clone(), conditional_subquery_branch)
                }
            }
        }
        merged_query
    }

    pub fn merge_with(&mut self, other: Query) {
        let Query {
            items,
            default_subquery_branch,
            conditional_subquery_branches,
            ..
        } = other;
        // merge query items as they point to the same context
        for item in items {
            self.insert_item(item)
        }

        self.merge_default_subquery_branch(default_subquery_branch);
        if let Some(conditional_subquery_branches) = conditional_subquery_branches {
            for (item, conditional_subquery_branch) in conditional_subquery_branches {
                self.merge_conditional_boxed_subquery(item.clone(), conditional_subquery_branch)
            }
        }
    }

    /// Adds a conditional subquery. A conditional subquery replaces the default
    /// subquery and subquery_path if the item matches for the key. If
    /// multiple conditional subquery items match, then the first one that
    /// matches is used (in order that they were added).
    pub fn merge_conditional_boxed_subquery(
        &mut self,
        query_item_merging_in: QueryItem,
        subquery_branch_merging_in: SubqueryBranch,
    ) {
        self.conditional_subquery_branches = Some(
            Self::merge_conditional_subquery_branches_with_new_at_query_item(
                self.conditional_subquery_branches.take(),
                query_item_merging_in,
                subquery_branch_merging_in,
            ),
        );
    }

    /// Adds a conditional subquery. A conditional subquery replaces the default
    /// subquery and subquery_path if the item matches for the key. If
    /// multiple conditional subquery items match, then the first one that
    /// matches is used (in order that they were added).
    pub fn merge_conditional_subquery_branches_with_new_at_query_item(
        conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
        query_item_merging_in: QueryItem,
        subquery_branch_merging_in: SubqueryBranch,
    ) -> IndexMap<QueryItem, SubqueryBranch> {
        let mut merged_items: IndexMap<QueryItem, SubqueryBranch> = IndexMap::new();
        if let Some(conditional_subquery_branches) = conditional_subquery_branches {
            for (query_item, subquery_branch) in conditional_subquery_branches {
                let QueryItemIntersectionResult {
                    in_both,
                    ours_left,
                    ours_right,
                    theirs_left,
                    theirs_right,
                } = query_item.intersect(&query_item_merging_in);
                if let Some(in_both) = in_both {
                    // todo: for the part that they are in both we need to construct a common
                    // conditional subquery

                    match (ours_left, ours_right, theirs_left, theirs_right) {
                        (None, None, None, None) => {}
                        (Some(ours_left), None, None, None) => {
                            merged_items.insert(ours_left, subquery_branch);
                        }
                        (None, Some(ours_right), None, None) => {
                            merged_items.insert(ours_right, subquery_branch);
                        }
                        (Some(ours_left), Some(ours_right), None, None) => {
                            merged_items.insert(ours_left, subquery_branch.clone());
                            merged_items.insert(ours_right, subquery_branch);
                        }
                        (None, None, Some(theirs_left), None) => {
                            merged_items.insert(theirs_left, subquery_branch_merging_in.clone());
                        }
                        (Some(ours_left), None, Some(theirs_left), None) => {
                            merged_items.insert(ours_left, subquery_branch);
                            merged_items.insert(theirs_left, subquery_branch_merging_in.clone());
                        }
                        (None, Some(ours_right), Some(theirs_left), None) => {
                            merged_items.insert(ours_right, subquery_branch);
                            merged_items.insert(theirs_left, subquery_branch_merging_in.clone());
                        }
                        (Some(ours_left), Some(ours_right), Some(theirs_left), None) => {
                            merged_items.insert(ours_left, subquery_branch.clone());
                            merged_items.insert(ours_right, subquery_branch);
                            merged_items.insert(theirs_left, subquery_branch_merging_in.clone());
                        }
                        (None, None, None, Some(theirs_right)) => {
                            merged_items.insert(theirs_right, subquery_branch_merging_in.clone());
                        }
                        (Some(ours_left), None, None, Some(theirs_right)) => {
                            merged_items.insert(ours_left, subquery_branch.clone());
                            merged_items.insert(theirs_right, subquery_branch_merging_in.clone());
                        }
                        (None, Some(ours_right), None, Some(theirs_right)) => {
                            merged_items.insert(ours_right, subquery_branch);
                            merged_items.insert(theirs_right, subquery_branch_merging_in.clone());
                        }
                        (Some(ours_left), Some(ours_right), None, Some(theirs_right)) => {
                            merged_items.insert(ours_left, subquery_branch.clone());
                            merged_items.insert(ours_right, subquery_branch);
                            merged_items.insert(theirs_right, subquery_branch_merging_in.clone());
                        }
                        (None, None, Some(theirs_left), Some(theirs_right)) => {
                            merged_items.insert(theirs_left, subquery_branch_merging_in.clone());
                            merged_items.insert(theirs_right, subquery_branch_merging_in.clone());
                        }

                        (Some(ours_left), None, Some(theirs_left), Some(theirs_right)) => {
                            merged_items.insert(ours_left, subquery_branch);
                            merged_items.insert(theirs_left, subquery_branch_merging_in.clone());
                            merged_items.insert(theirs_right, subquery_branch_merging_in.clone());
                        }
                        (None, Some(ours_right), Some(theirs_left), Some(theirs_right)) => {
                            merged_items.insert(ours_right, subquery_branch);
                            merged_items.insert(theirs_left, subquery_branch_merging_in.clone());
                            merged_items.insert(theirs_right, subquery_branch_merging_in.clone());
                        }
                        (
                            Some(ours_left),
                            Some(ours_right),
                            Some(theirs_left),
                            Some(theirs_right),
                        ) => {
                            merged_items.insert(ours_left, subquery_branch.clone());
                            merged_items.insert(ours_right, subquery_branch);
                            merged_items.insert(theirs_left, subquery_branch_merging_in.clone());
                            merged_items.insert(theirs_right, subquery_branch_merging_in.clone());
                        }
                    }
                } else {
                    // there was no overlap
                    // readd to merged_items
                    merged_items.insert(query_item, subquery_branch);
                }
            }
        } else {
            merged_items.insert(query_item_merging_in, subquery_branch_merging_in);
        }
        merged_items
    }
}
