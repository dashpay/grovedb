use crate::proofs::{
    query::{common_path::CommonPathResult, query_item::QueryItem, Path, SubqueryBranch},
    Query,
};

#[cfg(any(feature = "full", feature = "verify"))]
impl Query {
    fn merge_default_subquerys_branch_subquery(&mut self, other_default_branch_subquery: Option<Box<Query>>) {
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
        match (&self.default_subquery_branch.subquery_path, &other_default_subquery_branch.subquery_path) {
            (None, None) => {
                // they both just have subqueries without paths
                self.merge_default_subquerys_branch_subquery(other_default_subquery_branch.subquery);
            }
            (Some(our_subquery_path), Some(their_subquery_path)) => {
                // They both have subquery paths

                if our_subquery_path.eq(their_subquery_path) {
                    // The subquery paths are the same
                    // We just need to merge the subqueries together
                    self.merge_default_subquerys_branch_subquery(other_default_subquery_branch.subquery);
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
                        let maybe_left_path_leftovers = if left_path_leftovers.is_empty() { None } else { Some(left_path_leftovers)};
                        self.add_conditional_boxed_subquery(
                            QueryItem::Key(left_top_key),
                            maybe_left_path_leftovers,
                            self.default_subquery_branch.subquery.clone(),
                        );
                        let right_top_key = right_path_leftovers.remove(0);
                        let maybe_right_path_leftovers = if right_path_leftovers.is_empty() { None } else { Some(right_path_leftovers)};

                        self.add_conditional_boxed_subquery(
                            QueryItem::Key(right_top_key),
                            maybe_right_path_leftovers,
                            other_default_subquery_branch.subquery.clone(),
                        );
                    } else if right_path_leftovers.is_empty() {

                        let left_subquery = self.default_subquery_branch.subquery.clone();
                        // this means our subquery path was longer
                        // which means we need to set the default to the right (other)
                        self.default_subquery_branch.subquery =
                            other_default_subquery_branch.subquery.clone();
                        let first_key = left_path_leftovers.remove(0);
                        let maybe_left_path_leftovers = if left_path_leftovers.is_empty() { None } else { Some(left_path_leftovers)};

                        // our subquery stays the same as we didn't change level
                        // add a conditional subquery for other
                        self.add_conditional_boxed_subquery(
                            QueryItem::Key(first_key),
                            maybe_left_path_leftovers,
                            left_subquery,
                        );
                    } else if left_path_leftovers.is_empty() {
                        // this means our subquery path shorter
                        // we should keep our subquery
                        let other_first = right_path_leftovers.remove(0);

                        let maybe_right_path_leftovers = if right_path_leftovers.is_empty() { None } else { Some(right_path_leftovers)};
                        // our subquery stays the same as we didn't change level
                        // add a conditional subquery for other
                        self.add_conditional_boxed_subquery(
                            QueryItem::Key(other_first),
                            maybe_right_path_leftovers,
                            other_default_subquery_branch.subquery.clone(),
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
                self.default_subquery_branch.subquery = other_default_subquery_branch.subquery.clone();
                // We need to add a conditional subquery for ours

                let our_top_key = our_subquery_path.remove(0);

                let maybe_our_subquery_path = if our_subquery_path.is_empty() { None } else { Some(our_subquery_path)};
                // our subquery stays the same as we didn't change level
                // add a conditional subquery for other
                self.add_conditional_boxed_subquery(
                    QueryItem::Key(our_top_key),
                    maybe_our_subquery_path,
                    other_default_subquery_branch.subquery.clone(),
                );
            }
            (None, Some(their_subquery_path)) => {
                // They have a subquery path, we does not.
                // We set the subquery path to None

                let mut their_subquery_path = their_subquery_path.clone();

                // The subquery_path is already set to None, no need to set it again

                let their_top_key = their_subquery_path.remove(0);

                let maybe_their_subquery_path = if their_subquery_path.is_empty() { None } else { Some(their_subquery_path)};
                // our subquery stays the same as we didn't change level
                // add a conditional subquery for other
                self.add_conditional_boxed_subquery(
                    QueryItem::Key(their_top_key),
                    maybe_their_subquery_path,
                    other_default_subquery_branch.subquery.clone(),
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
            for (item, conditional_subquery_branch) in conditional_subquery_branches {
                merged_query.merge_conditional_subquery(
                    item.clone(),
                    conditional_subquery_branch.subquery_path.clone(),
                    conditional_subquery_branch
                        .subquery
                        .as_ref()
                        .map(|query| *query.clone()),
                )
            }
        }
        merged_query
    }

    pub fn merge_with(&mut self, other: Query) {
        let Query {
            items,
            default_subquery_branch,
            conditional_subquery_branches,
            left_to_right,
        } = other;
        // merge query items as they point to the same context
        for item in items {
            self.insert_item(item)
        }

        // TODO: deal with default subquery branch
        //  this is not needed currently for path_query merge as we enforce
        //  non-subset paths, but might be useful in the future
        //  Need to create a stretching function for queries that expands default
        //  subqueries  to conditional subqueries.

        // merge conditional query branches.
        for (query_item, subquery_branch) in conditional_subquery_branches.into_iter() {
            let subquery_branch_option = self.conditional_subquery_branches.get_mut(&query_item);
            if let Some(subquery_branch_old) = subquery_branch_option {
                (subquery_branch_old.subquery.as_mut().unwrap())
                    .merge_with(*subquery_branch.subquery.unwrap());
            } else {
                // we don't have that branch just assign the query
                self.conditional_subquery_branches
                    .insert(query_item, subquery_branch);
            }
        }
    }

    /// Adds a conditional subquery. A conditional subquery replaces the default
    /// subquery and subquery_path if the item matches for the key. If
    /// multiple conditional subquery items match, then the first one that
    /// matches is used (in order that they were added).
    pub fn merge_conditional_subquery(
        &mut self,
        item: QueryItem,
        subquery_path: Option<Path>,
        subquery: Option<Self>,
    ) {
        self.conditional_subquery_branches.insert(
            item,
            SubqueryBranch {
                subquery_path,
                subquery: subquery.map(Box::new),
            },
        );
    }
}
