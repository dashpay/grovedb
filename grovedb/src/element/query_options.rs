use std::fmt;

/// Options controlling how an element query is executed.
#[derive(Copy, Clone, Debug)]
pub struct QueryOptions {
    /// If true, allows fetching raw (unprocessed) elements.
    pub allow_get_raw: bool,
    /// If true, allows reading from cache instead of forcing fresh disk reads.
    pub allow_cache: bool,
    /// Should we decrease the limit of elements found when we have no
    /// subelements in the subquery? This should generally be set to true,
    /// as having it false could mean very expensive queries. The queries
    /// would be expensive because we could go through many many trees where the
    /// sub elements have no matches, hence the limit would not decrease and
    /// hence we would continue on the increasingly expensive query.
    pub decrease_limit_on_range_with_no_sub_elements: bool,
    /// If true (default), returns an error when an intermediate path tree does
    /// not exist. When false, a missing intermediate tree is silently treated
    /// as empty.
    pub error_if_intermediate_path_tree_not_present: bool,
    /// Whether the empty-subtree charges governed by
    /// `decrease_limit_on_range_with_no_sub_elements` also consume
    /// per-instance budgets (`Query::limit`). By default (`false`) those
    /// charges consume only the global `SizedQuery::limit` — the budget
    /// whose exhaustion bounds a walk across many empty subtrees — and
    /// per-instance caps count result rows only. Has no effect on a
    /// query without per-instance limits, and no effect when
    /// `decrease_limit_on_range_with_no_sub_elements` is off.
    pub decrease_instance_limits_on_range_with_no_sub_elements: bool,
}

impl fmt::Display for QueryOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "QueryOptions {{")?;
        writeln!(f, "  allow_get_raw: {}", self.allow_get_raw)?;
        writeln!(f, "  allow_cache: {}", self.allow_cache)?;
        writeln!(
            f,
            "  decrease_limit_on_range_with_no_sub_elements: {}",
            self.decrease_limit_on_range_with_no_sub_elements
        )?;
        writeln!(
            f,
            "  error_if_intermediate_path_tree_not_present: {}",
            self.error_if_intermediate_path_tree_not_present
        )?;
        writeln!(
            f,
            "  decrease_instance_limits_on_range_with_no_sub_elements: {}",
            self.decrease_instance_limits_on_range_with_no_sub_elements
        )?;
        write!(f, "}}")
    }
}

impl Default for QueryOptions {
    fn default() -> Self {
        QueryOptions {
            allow_get_raw: false,
            allow_cache: true,
            decrease_limit_on_range_with_no_sub_elements: true,
            error_if_intermediate_path_tree_not_present: true,
            decrease_instance_limits_on_range_with_no_sub_elements: false,
        }
    }
}
