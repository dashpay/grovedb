use indexmap::IndexMap;

use grovedb_query::{Query, QueryItem, SubqueryBranch};

fn k(v: u8) -> Vec<u8> {
    vec![v]
}

// ───────────────────────────────────────────────────────────────────────
// SubqueryBranch::merge — (None, None) paths, subquery combinations
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_branch_both_paths_none_both_subqueries_none() {
    let a = SubqueryBranch {
        subquery_path: None,
        subquery: None,
    };
    let b = SubqueryBranch {
        subquery_path: None,
        subquery: None,
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, None);
    assert_eq!(merged.subquery, None);
}

#[test]
fn merge_branch_both_paths_none_self_has_subquery() {
    let sq = Query::new_single_key(k(1));
    let a = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(sq.clone())),
    };
    let b = SubqueryBranch {
        subquery_path: None,
        subquery: None,
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, None);
    assert_eq!(merged.subquery, Some(Box::new(sq)));
}

#[test]
fn merge_branch_both_paths_none_other_has_subquery() {
    let sq = Query::new_single_key(k(2));
    let a = SubqueryBranch {
        subquery_path: None,
        subquery: None,
    };
    let b = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(sq.clone())),
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, None);
    assert_eq!(merged.subquery, Some(Box::new(sq)));
}

#[test]
fn merge_branch_both_paths_none_both_have_subqueries() {
    let sq_a = Query::new_single_key(k(1));
    let sq_b = Query::new_single_key(k(2));
    let a = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(sq_a)),
    };
    let b = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(sq_b)),
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, None);
    let mq = merged.subquery.expect("merged subquery should exist");
    // Both keys should be present in the merged query items
    assert!(mq.items.contains(&QueryItem::Key(k(1))));
    assert!(mq.items.contains(&QueryItem::Key(k(2))));
}

// ───────────────────────────────────────────────────────────────────────
// SubqueryBranch::merge — (Some, Some) identical paths
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_branch_same_paths_merges_subqueries() {
    let path = vec![k(10), k(20)];
    let a = SubqueryBranch {
        subquery_path: Some(path.clone()),
        subquery: Some(Box::new(Query::new_single_key(k(1)))),
    };
    let b = SubqueryBranch {
        subquery_path: Some(path.clone()),
        subquery: Some(Box::new(Query::new_single_key(k(2)))),
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, Some(path));
    let mq = merged.subquery.unwrap();
    assert!(mq.items.contains(&QueryItem::Key(k(1))));
    assert!(mq.items.contains(&QueryItem::Key(k(2))));
}

// ───────────────────────────────────────────────────────────────────────
// SubqueryBranch::merge — (Some, Some) divergent paths, both leftovers
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_branch_divergent_paths_both_have_leftovers() {
    // common=[10], left leftover=[20], right leftover=[30]
    let a = SubqueryBranch {
        subquery_path: Some(vec![k(10), k(20)]),
        subquery: Some(Box::new(Query::new_single_key(k(1)))),
    };
    let b = SubqueryBranch {
        subquery_path: Some(vec![k(10), k(30)]),
        subquery: Some(Box::new(Query::new_single_key(k(2)))),
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, Some(vec![k(10)]));
    let mq = merged.subquery.unwrap();
    // Should have conditional subquery branches for keys 20 and 30
    let conds = mq.conditional_subquery_branches.as_ref().unwrap();
    assert!(conds.contains_key(&QueryItem::Key(k(20))));
    assert!(conds.contains_key(&QueryItem::Key(k(30))));
}

#[test]
fn merge_branch_divergent_paths_multi_segment_leftovers() {
    // common=[10], left leftover=[20, 21], right leftover=[30]
    // Left has multi-segment leftovers — exercises maybe_left_path_leftovers = Some(...)
    let a = SubqueryBranch {
        subquery_path: Some(vec![k(10), k(20), k(21)]),
        subquery: Some(Box::new(Query::new_single_key(k(1)))),
    };
    let b = SubqueryBranch {
        subquery_path: Some(vec![k(10), k(30)]),
        subquery: Some(Box::new(Query::new_single_key(k(2)))),
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, Some(vec![k(10)]));
    let mq = merged.subquery.unwrap();
    let conds = mq.conditional_subquery_branches.as_ref().unwrap();
    // Key 20's branch should have remaining path [21]
    let branch_20 = conds.get(&QueryItem::Key(k(20))).unwrap();
    assert_eq!(branch_20.subquery_path, Some(vec![k(21)]));
    // Key 30's branch should have no remaining path (single element leftover)
    let branch_30 = conds.get(&QueryItem::Key(k(30))).unwrap();
    assert_eq!(branch_30.subquery_path, None);
}

#[test]
fn merge_branch_divergent_paths_no_common_prefix() {
    // No common path — exercises subquery_path = None case
    let a = SubqueryBranch {
        subquery_path: Some(vec![k(1)]),
        subquery: Some(Box::new(Query::new_single_key(k(10)))),
    };
    let b = SubqueryBranch {
        subquery_path: Some(vec![k(2)]),
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    };
    let merged = a.merge(&b);
    // No common path
    assert_eq!(merged.subquery_path, None);
    let mq = merged.subquery.unwrap();
    let conds = mq.conditional_subquery_branches.as_ref().unwrap();
    assert!(conds.contains_key(&QueryItem::Key(k(1))));
    assert!(conds.contains_key(&QueryItem::Key(k(2))));
}

// ───────────────────────────────────────────────────────────────────────
// SubqueryBranch::merge — (Some, Some) one path longer than the other
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_branch_our_path_longer_right_empty_leftovers() {
    // our=[10,20,30], their=[10,20] => common=[10,20], left=[30], right=[]
    let a = SubqueryBranch {
        subquery_path: Some(vec![k(10), k(20), k(30)]),
        subquery: Some(Box::new(Query::new_single_key(k(1)))),
    };
    let b = SubqueryBranch {
        subquery_path: Some(vec![k(10), k(20)]),
        subquery: Some(Box::new(Query::new_single_key(k(2)))),
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, Some(vec![k(10), k(20)]));
    let mq = merged.subquery.unwrap();
    let conds = mq.conditional_subquery_branches.as_ref().unwrap();
    assert!(conds.contains_key(&QueryItem::Key(k(30))));
}

#[test]
fn merge_branch_their_path_longer_left_empty_leftovers() {
    // our=[10,20], their=[10,20,40] => common=[10,20], left=[], right=[40]
    let a = SubqueryBranch {
        subquery_path: Some(vec![k(10), k(20)]),
        subquery: Some(Box::new(Query::new_single_key(k(1)))),
    };
    let b = SubqueryBranch {
        subquery_path: Some(vec![k(10), k(20), k(40)]),
        subquery: Some(Box::new(Query::new_single_key(k(2)))),
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, Some(vec![k(10), k(20)]));
    let mq = merged.subquery.unwrap();
    let conds = mq.conditional_subquery_branches.as_ref().unwrap();
    assert!(conds.contains_key(&QueryItem::Key(k(40))));
}

// ───────────────────────────────────────────────────────────────────────
// SubqueryBranch::merge — (Some, None) and (None, Some) arms
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_branch_ours_has_path_theirs_none() {
    let a = SubqueryBranch {
        subquery_path: Some(vec![k(5), k(6)]),
        subquery: Some(Box::new(Query::new_single_key(k(10)))),
    };
    let b = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    };
    let merged = a.merge(&b);
    // Path should be None (dropped to the shorter level)
    assert_eq!(merged.subquery_path, None);
    let mq = merged.subquery.unwrap();
    // Key 5 should be a conditional branch with remaining path [6]
    let conds = mq.conditional_subquery_branches.as_ref().unwrap();
    let branch = conds.get(&QueryItem::Key(k(5))).unwrap();
    assert_eq!(branch.subquery_path, Some(vec![k(6)]));
}

#[test]
fn merge_branch_ours_none_theirs_has_path() {
    let a = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(k(10)))),
    };
    let b = SubqueryBranch {
        subquery_path: Some(vec![k(7), k(8)]),
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, None);
    let mq = merged.subquery.unwrap();
    let conds = mq.conditional_subquery_branches.as_ref().unwrap();
    let branch = conds.get(&QueryItem::Key(k(7))).unwrap();
    assert_eq!(branch.subquery_path, Some(vec![k(8)]));
}

#[test]
fn merge_branch_ours_has_single_segment_path_theirs_none() {
    // Single-segment path exercises maybe_our_subquery_path = None
    let a = SubqueryBranch {
        subquery_path: Some(vec![k(5)]),
        subquery: Some(Box::new(Query::new_single_key(k(10)))),
    };
    let b = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    };
    let merged = a.merge(&b);
    assert_eq!(merged.subquery_path, None);
    let mq = merged.subquery.unwrap();
    let conds = mq.conditional_subquery_branches.as_ref().unwrap();
    let branch = conds.get(&QueryItem::Key(k(5))).unwrap();
    // Single segment — no remaining path
    assert_eq!(branch.subquery_path, None);
}

// ───────────────────────────────────────────────────────────────────────
// Query::merge_default_subquery_branch — (None, None) arm
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_default_branch_both_none_paths_both_have_subqueries() {
    let mut q = Query::new_single_key(k(1));
    q.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(k(10)))),
    };
    q.merge_default_subquery_branch(SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    });
    let sq = q.default_subquery_branch.subquery.unwrap();
    assert!(sq.items.contains(&QueryItem::Key(k(10))));
    assert!(sq.items.contains(&QueryItem::Key(k(20))));
}

#[test]
fn merge_default_branch_both_none_paths_self_has_no_subquery() {
    let mut q = Query::new_single_key(k(1));
    // default_subquery_branch starts with None subquery
    q.merge_default_subquery_branch(SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    });
    let sq = q.default_subquery_branch.subquery.unwrap();
    assert!(sq.items.contains(&QueryItem::Key(k(20))));
}

// ───────────────────────────────────────────────────────────────────────
// Query::merge_default_subquery_branch — (Some, Some) same paths
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_default_branch_same_paths() {
    let mut q = Query::new_single_key(k(1));
    q.default_subquery_branch = SubqueryBranch {
        subquery_path: Some(vec![k(5), k(6)]),
        subquery: Some(Box::new(Query::new_single_key(k(10)))),
    };
    q.merge_default_subquery_branch(SubqueryBranch {
        subquery_path: Some(vec![k(5), k(6)]),
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    });
    assert_eq!(
        q.default_subquery_branch.subquery_path,
        Some(vec![k(5), k(6)])
    );
    let sq = q.default_subquery_branch.subquery.unwrap();
    assert!(sq.items.contains(&QueryItem::Key(k(10))));
    assert!(sq.items.contains(&QueryItem::Key(k(20))));
}

// ───────────────────────────────────────────────────────────────────────
// Query::merge_default_subquery_branch — (Some, Some) no common prefix
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_default_branch_no_common_prefix() {
    let mut q = Query::new_single_key(k(1));
    q.default_subquery_branch = SubqueryBranch {
        subquery_path: Some(vec![k(1)]),
        subquery: Some(Box::new(Query::new_single_key(k(10)))),
    };
    q.merge_default_subquery_branch(SubqueryBranch {
        subquery_path: Some(vec![k(2)]),
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    });
    // No common prefix => subquery_path set to None
    assert_eq!(q.default_subquery_branch.subquery_path, None);
    let conds = q.conditional_subquery_branches.as_ref().unwrap();
    assert!(conds.contains_key(&QueryItem::Key(k(1))));
    assert!(conds.contains_key(&QueryItem::Key(k(2))));
}

// ───────────────────────────────────────────────────────────────────────
// Query::merge_default_subquery_branch — left path longer (right empty leftovers)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_default_branch_left_path_longer() {
    let mut q = Query::new_single_key(k(1));
    q.default_subquery_branch = SubqueryBranch {
        subquery_path: Some(vec![k(10), k(20), k(30)]),
        subquery: Some(Box::new(Query::new_single_key(k(1)))),
    };
    q.merge_default_subquery_branch(SubqueryBranch {
        subquery_path: Some(vec![k(10), k(20)]),
        subquery: Some(Box::new(Query::new_single_key(k(2)))),
    });
    // Common path is [10, 20]
    assert_eq!(
        q.default_subquery_branch.subquery_path,
        Some(vec![k(10), k(20)])
    );
    // Default subquery should now be the other's (shorter path's) subquery
    let sq = q.default_subquery_branch.subquery.as_ref().unwrap();
    assert!(sq.items.contains(&QueryItem::Key(k(2))));
    // And key 30 should be a conditional branch
    let conds = q.conditional_subquery_branches.as_ref().unwrap();
    assert!(conds.contains_key(&QueryItem::Key(k(30))));
}

// ───────────────────────────────────────────────────────────────────────
// Query::merge_default_subquery_branch — right path longer (left empty leftovers)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_default_branch_right_path_longer() {
    let mut q = Query::new_single_key(k(1));
    q.default_subquery_branch = SubqueryBranch {
        subquery_path: Some(vec![k(10), k(20)]),
        subquery: Some(Box::new(Query::new_single_key(k(1)))),
    };
    q.merge_default_subquery_branch(SubqueryBranch {
        subquery_path: Some(vec![k(10), k(20), k(40)]),
        subquery: Some(Box::new(Query::new_single_key(k(2)))),
    });
    assert_eq!(
        q.default_subquery_branch.subquery_path,
        Some(vec![k(10), k(20)])
    );
    // Self's subquery should be preserved (shorter path)
    let sq = q.default_subquery_branch.subquery.as_ref().unwrap();
    assert!(sq.items.contains(&QueryItem::Key(k(1))));
    let conds = q.conditional_subquery_branches.as_ref().unwrap();
    assert!(conds.contains_key(&QueryItem::Key(k(40))));
}

// ───────────────────────────────────────────────────────────────────────
// Query::merge_default_subquery_branch — (Some, None) and (None, Some)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_default_branch_ours_has_path_theirs_none() {
    let mut q = Query::new_single_key(k(1));
    q.default_subquery_branch = SubqueryBranch {
        subquery_path: Some(vec![k(5), k(6)]),
        subquery: Some(Box::new(Query::new_single_key(k(10)))),
    };
    q.merge_default_subquery_branch(SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    });
    // Path drops to None
    assert_eq!(q.default_subquery_branch.subquery_path, None);
    // Default subquery becomes the other's
    let conds = q.conditional_subquery_branches.as_ref().unwrap();
    let branch = conds.get(&QueryItem::Key(k(5))).unwrap();
    assert_eq!(branch.subquery_path, Some(vec![k(6)]));
}

#[test]
fn merge_default_branch_ours_none_theirs_has_path() {
    let mut q = Query::new_single_key(k(1));
    q.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(k(10)))),
    };
    q.merge_default_subquery_branch(SubqueryBranch {
        subquery_path: Some(vec![k(7), k(8)]),
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    });
    // Path stays None
    assert_eq!(q.default_subquery_branch.subquery_path, None);
    let conds = q.conditional_subquery_branches.as_ref().unwrap();
    let branch = conds.get(&QueryItem::Key(k(7))).unwrap();
    assert_eq!(branch.subquery_path, Some(vec![k(8)]));
}

// ───────────────────────────────────────────────────────────────────────
// Query::merge_multiple — non-trivial multi-query merges
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_multiple_two_queries_with_items_and_conditionals() {
    let mut q1 = Query::new();
    q1.insert_key(k(1));
    q1.insert_key(k(2));
    q1.set_subquery_path(vec![k(10)]);

    let mut q2 = Query::new();
    q2.insert_key(k(3));
    q2.insert_key(k(4));
    q2.add_conditional_subquery(
        QueryItem::Key(k(3)),
        Some(vec![k(20)]),
        Some(Query::new_single_key(k(50))),
    );

    let merged = Query::merge_multiple(vec![q1, q2]);
    // All four keys should be present
    assert!(merged.items.contains(&QueryItem::Key(k(1))));
    assert!(merged.items.contains(&QueryItem::Key(k(2))));
    assert!(merged.items.contains(&QueryItem::Key(k(3))));
    assert!(merged.items.contains(&QueryItem::Key(k(4))));
    // Should have conditional subquery branches
    assert!(merged.conditional_subquery_branches.is_some());
}

#[test]
fn merge_multiple_three_queries() {
    let mut q1 = Query::new_single_key(k(1));
    q1.set_subquery_path(vec![k(10)]);

    let mut q2 = Query::new_single_key(k(2));
    q2.set_subquery_path(vec![k(20)]);

    let mut q3 = Query::new_single_key(k(3));
    q3.set_subquery_path(vec![k(30)]);

    let merged = Query::merge_multiple(vec![q1, q2, q3]);
    assert_eq!(merged.items.len(), 3);
    assert!(merged.items.contains(&QueryItem::Key(k(1))));
    assert!(merged.items.contains(&QueryItem::Key(k(2))));
    assert!(merged.items.contains(&QueryItem::Key(k(3))));
}

// ───────────────────────────────────────────────────────────────────────
// merge_conditional_boxed_subquery — empty branch guard (no-op)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_conditional_boxed_subquery_noop_on_empty_branch() {
    let mut q = Query::new_single_key(k(1));
    // Both subquery and subquery_path are None — should be a no-op
    q.merge_conditional_boxed_subquery(
        QueryItem::Key(k(1)),
        SubqueryBranch {
            subquery_path: None,
            subquery: None,
        },
    );
    assert!(q.conditional_subquery_branches.is_none());
}

// ───────────────────────────────────────────────────────────────────────
// merge_conditional_subquery_branches_with_new_at_query_item
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_conditional_branches_none_existing_direct_insert() {
    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        None,
        QueryItem::Key(k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(10)]),
            subquery: None,
        },
    );
    assert_eq!(merged.len(), 1);
    assert!(merged.contains_key(&QueryItem::Key(k(5))));
}

#[test]
fn merge_conditional_branches_exact_overlap_no_leftovers() {
    // Existing: Range(1..5), incoming: Range(1..5) — exact match
    let mut existing = IndexMap::new();
    existing.insert(
        QueryItem::Range(k(1)..k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(10)]),
            subquery: None,
        },
    );

    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Range(k(1)..k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(20)]),
            subquery: None,
        },
    );
    // Exact overlap — single merged entry
    assert_eq!(merged.len(), 1);
    assert!(merged.contains_key(&QueryItem::Range(k(1)..k(5))));
}

#[test]
fn merge_conditional_branches_ours_extends_left_only() {
    // Existing: Range(1..5), incoming: Range(3..5) — ours extends left [1..3)
    let mut existing = IndexMap::new();
    existing.insert(
        QueryItem::Range(k(1)..k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(10)]),
            subquery: None,
        },
    );

    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Range(k(3)..k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(20)]),
            subquery: None,
        },
    );
    assert_eq!(merged.len(), 2);
    assert!(merged.contains_key(&QueryItem::Range(k(1)..k(3))));
    assert!(merged.contains_key(&QueryItem::Range(k(3)..k(5))));
}

#[test]
fn merge_conditional_branches_ours_extends_right_only() {
    // Existing: Range(1..5), incoming: Range(1..3)
    let mut existing = IndexMap::new();
    existing.insert(
        QueryItem::Range(k(1)..k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(10)]),
            subquery: None,
        },
    );

    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Range(k(1)..k(3)),
        SubqueryBranch {
            subquery_path: Some(vec![k(20)]),
            subquery: None,
        },
    );
    assert_eq!(merged.len(), 2);
    assert!(merged.contains_key(&QueryItem::Range(k(1)..k(3))));
}

#[test]
fn merge_conditional_branches_ours_contains_theirs() {
    // Existing: Range(1..10), incoming: Key(5) — ours extends both sides
    let mut existing = IndexMap::new();
    existing.insert(
        QueryItem::Range(k(1)..k(10)),
        SubqueryBranch {
            subquery_path: Some(vec![k(10)]),
            subquery: None,
        },
    );

    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Key(k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(20)]),
            subquery: None,
        },
    );
    // Should have left piece, intersection, and right piece
    assert!(merged.len() >= 2);
    assert!(merged.contains_key(&QueryItem::Key(k(5))));
}

#[test]
fn merge_conditional_branches_theirs_contains_ours() {
    // Existing: Key(5), incoming: Range(1..10) — theirs spans ours completely
    let mut existing = IndexMap::new();
    existing.insert(
        QueryItem::Key(k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(10)]),
            subquery: None,
        },
    );

    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Range(k(1)..k(10)),
        SubqueryBranch {
            subquery_path: Some(vec![k(20)]),
            subquery: None,
        },
    );
    // Should have left piece from theirs, intersection at Key(5), right piece from theirs
    assert!(merged.len() >= 2);
    assert!(merged.contains_key(&QueryItem::Key(k(5))));
}

#[test]
fn merge_conditional_branches_theirs_extends_right_only() {
    // Existing: Range(3..5), incoming: Range(3..7) — theirs extends right
    let mut existing = IndexMap::new();
    existing.insert(
        QueryItem::Range(k(3)..k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(10)]),
            subquery: None,
        },
    );

    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Range(k(3)..k(7)),
        SubqueryBranch {
            subquery_path: Some(vec![k(20)]),
            subquery: None,
        },
    );
    assert_eq!(merged.len(), 2);
    assert!(merged.contains_key(&QueryItem::Range(k(3)..k(5))));
    assert!(merged.contains_key(&QueryItem::Range(k(5)..k(7))));
}

#[test]
fn merge_conditional_branches_theirs_extends_left_only() {
    // Existing: Range(3..5), incoming: Range(1..5) — theirs extends left
    let mut existing = IndexMap::new();
    existing.insert(
        QueryItem::Range(k(3)..k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(10)]),
            subquery: None,
        },
    );

    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Range(k(1)..k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![k(20)]),
            subquery: None,
        },
    );
    assert_eq!(merged.len(), 2);
    assert!(merged.contains_key(&QueryItem::Range(k(3)..k(5))));
    assert!(merged.contains_key(&QueryItem::Range(k(1)..k(3))));
}

#[test]
fn merge_conditional_branches_no_overlap_appends() {
    // Existing: Range(1..3), incoming: Range(5..7) — no overlap
    let mut existing = IndexMap::new();
    existing.insert(
        QueryItem::Range(k(1)..k(3)),
        SubqueryBranch {
            subquery_path: Some(vec![k(10)]),
            subquery: None,
        },
    );

    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Range(k(5)..k(7)),
        SubqueryBranch {
            subquery_path: Some(vec![k(20)]),
            subquery: None,
        },
    );
    assert_eq!(merged.len(), 2);
    assert!(merged.contains_key(&QueryItem::Range(k(1)..k(3))));
    assert!(merged.contains_key(&QueryItem::Range(k(5)..k(7))));
}

#[test]
fn merge_conditional_branches_incoming_spans_multiple_existing() {
    // Two existing non-overlapping branches, incoming spans across both
    let mut existing = IndexMap::new();
    existing.insert(
        QueryItem::Range(k(1)..k(3)),
        SubqueryBranch {
            subquery_path: Some(vec![k(10)]),
            subquery: None,
        },
    );
    existing.insert(
        QueryItem::Range(k(5)..k(7)),
        SubqueryBranch {
            subquery_path: Some(vec![k(20)]),
            subquery: None,
        },
    );

    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Range(k(0)..k(10)),
        SubqueryBranch {
            subquery_path: Some(vec![k(30)]),
            subquery: None,
        },
    );
    // Should have entries for: the overlap with first range, the overlap with
    // second range, and the leftover pieces from the incoming range
    assert!(merged.len() >= 3);
}

// ───────────────────────────────────────────────────────────────────────
// Query::merge_with — exercises the full merge_with pipeline
// ───────────────────────────────────────────────────────────────────────

#[test]
fn merge_multiple_preserves_add_parent_tree_on_subquery() {
    let mut q1 = Query::new_single_key(k(1));
    q1.add_parent_tree_on_subquery = false;

    let mut q2 = Query::new_single_key(k(2));
    q2.add_parent_tree_on_subquery = true;

    let merged = Query::merge_multiple(vec![q1, q2]);
    assert!(
        merged.add_parent_tree_on_subquery,
        "merge_multiple should preserve add_parent_tree_on_subquery when any query sets it"
    );
}

#[test]
fn merge_with_preserves_add_parent_tree_on_subquery() {
    let mut q1 = Query::new_single_key(k(1));
    q1.add_parent_tree_on_subquery = false;

    let mut q2 = Query::new_single_key(k(2));
    q2.add_parent_tree_on_subquery = true;

    q1.merge_with(q2);
    assert!(
        q1.add_parent_tree_on_subquery,
        "merge_with should preserve add_parent_tree_on_subquery when other query sets it"
    );
}

#[test]
fn merge_with_both_have_conditional_branches() {
    let mut q1 = Query::new();
    q1.insert_key(k(1));
    q1.add_conditional_subquery(
        QueryItem::Key(k(1)),
        Some(vec![k(10)]),
        Some(Query::new_single_key(k(100))),
    );

    let mut q2 = Query::new();
    q2.insert_key(k(2));
    q2.add_conditional_subquery(
        QueryItem::Key(k(2)),
        Some(vec![k(20)]),
        Some(Query::new_single_key(k(200))),
    );

    q1.merge_with(q2);
    assert!(q1.items.contains(&QueryItem::Key(k(1))));
    assert!(q1.items.contains(&QueryItem::Key(k(2))));
    let conds = q1.conditional_subquery_branches.as_ref().unwrap();
    assert!(conds.contains_key(&QueryItem::Key(k(1))));
    assert!(conds.contains_key(&QueryItem::Key(k(2))));
}

#[test]
fn merge_with_overlapping_conditional_items_intersect() {
    let mut q1 = Query::new();
    q1.insert_range(k(1)..k(10));
    q1.add_conditional_subquery(
        QueryItem::Range(k(1)..k(5)),
        Some(vec![k(10)]),
        Some(Query::new_single_key(k(100))),
    );

    let mut q2 = Query::new();
    q2.insert_range(k(3)..k(8));
    q2.add_conditional_subquery(
        QueryItem::Range(k(3)..k(8)),
        Some(vec![k(20)]),
        Some(Query::new_single_key(k(200))),
    );

    q1.merge_with(q2);
    // Items should be merged (ranges unioned)
    assert!(!q1.items.is_empty());
    // Conditional branches should exist covering the overlapping and
    // non-overlapping portions
    assert!(q1.conditional_subquery_branches.is_some());
}

// ───────────────────────────────────────────────────────────────────────
// M6 fix: merge_multiple / merge_with correctly handle overlapping items
// with different default_subquery_branch values
// ───────────────────────────────────────────────────────────────────────

/// Overlapping items between two queries should get BOTH queries' default
/// subquery branches merged together.
#[test]
fn merge_multiple_overlapping_items_get_both_defaults() {
    // Query A: items=[Key(1), Key(2)], default subquery selects key "a"
    let mut query_a = Query::new();
    query_a.insert_key(k(1));
    query_a.insert_key(k(2));
    query_a.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'a']))),
    };

    // Query B: items=[Key(2), Key(3)], default subquery selects key "b"
    let mut query_b = Query::new();
    query_b.insert_key(k(2));
    query_b.insert_key(k(3));
    query_b.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'b']))),
    };

    let merged = Query::merge_multiple(vec![query_a, query_b]);

    // All three keys should be present
    assert!(merged.items.contains(&QueryItem::Key(k(1))));
    assert!(merged.items.contains(&QueryItem::Key(k(2))));
    assert!(merged.items.contains(&QueryItem::Key(k(3))));

    let conds = merged
        .conditional_subquery_branches
        .as_ref()
        .expect("should have conditional branches");

    // Key(2) is in BOTH queries → should have merged subquery with "a" AND "b"
    let key2_branch = conds
        .get(&QueryItem::Key(k(2)))
        .expect("Key(2) should have a conditional branch");
    let key2_subquery = key2_branch
        .subquery
        .as_ref()
        .expect("Key(2) branch should have a subquery");
    assert!(
        key2_subquery.items.contains(&QueryItem::Key(vec![b'a'])),
        "Key(2) should contain 'a' from query A's default, got: {:?}",
        key2_subquery.items
    );
    assert!(
        key2_subquery.items.contains(&QueryItem::Key(vec![b'b'])),
        "Key(2) should contain 'b' from query B's default, got: {:?}",
        key2_subquery.items
    );

    // Key(3) is only in query B → should have only "b"
    let key3_branch = conds
        .get(&QueryItem::Key(k(3)))
        .expect("Key(3) should have a conditional branch");
    let key3_subquery = key3_branch
        .subquery
        .as_ref()
        .expect("Key(3) branch should have a subquery");
    assert!(key3_subquery.items.contains(&QueryItem::Key(vec![b'b'])));
    assert!(!key3_subquery.items.contains(&QueryItem::Key(vec![b'a'])));

    // Key(1) is only in query A → uses default (A's) → selects "a"
    assert!(!conds.contains_key(&QueryItem::Key(k(1))));
    assert!(merged
        .default_subquery_branch
        .subquery
        .as_ref()
        .unwrap()
        .items
        .contains(&QueryItem::Key(vec![b'a'])));
}

/// Non-overlapping items: each query's items should only get their own default.
#[test]
fn merge_multiple_non_overlapping_items_get_own_defaults() {
    let mut query_a = Query::new();
    query_a.insert_key(k(1));
    query_a.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'a']))),
    };

    let mut query_b = Query::new();
    query_b.insert_key(k(2));
    query_b.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'b']))),
    };

    let merged = Query::merge_multiple(vec![query_a, query_b]);

    // Key(1): only in A → uses default "a" (no conditional needed)
    // Key(2): only in B → conditional "b"
    let default_sq = merged
        .default_subquery_branch
        .subquery
        .as_ref()
        .expect("default subquery should exist");
    assert!(default_sq.items.contains(&QueryItem::Key(vec![b'a'])));

    let conds = merged
        .conditional_subquery_branches
        .as_ref()
        .expect("should have conditional branches for B's items");
    let key2_branch = conds
        .get(&QueryItem::Key(k(2)))
        .expect("Key(2) should have a conditional");
    let key2_sq = key2_branch.subquery.as_ref().unwrap();
    assert!(key2_sq.items.contains(&QueryItem::Key(vec![b'b'])));
    assert!(!key2_sq.items.contains(&QueryItem::Key(vec![b'a'])));
}

/// merge_with has the same fix: overlapping items get both defaults.
#[test]
fn merge_with_overlapping_items_get_both_defaults() {
    let mut query_a = Query::new();
    query_a.insert_key(k(1));
    query_a.insert_key(k(2));
    query_a.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'a']))),
    };

    let mut query_b = Query::new();
    query_b.insert_key(k(2));
    query_b.insert_key(k(3));
    query_b.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'b']))),
    };

    query_a.merge_with(query_b);

    let conds = query_a
        .conditional_subquery_branches
        .as_ref()
        .expect("should have conditional branches");

    // Key(2) overlaps → merged subquery should have both "a" and "b"
    let key2_branch = conds
        .get(&QueryItem::Key(k(2)))
        .expect("Key(2) should have a conditional branch");
    let key2_sq = key2_branch.subquery.as_ref().unwrap();
    assert!(
        key2_sq.items.contains(&QueryItem::Key(vec![b'a'])),
        "Key(2) should contain 'a' from query A, got: {:?}",
        key2_sq.items
    );
    assert!(
        key2_sq.items.contains(&QueryItem::Key(vec![b'b'])),
        "Key(2) should contain 'b' from query B, got: {:?}",
        key2_sq.items
    );
}

/// Three queries with pairwise overlaps: Key(2) in A+B, Key(3) in B+C.
#[test]
fn merge_multiple_three_queries_pairwise_overlaps() {
    let mut qa = Query::new();
    qa.insert_key(k(1));
    qa.insert_key(k(2));
    qa.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'a']))),
    };

    let mut qb = Query::new();
    qb.insert_key(k(2));
    qb.insert_key(k(3));
    qb.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'b']))),
    };

    let mut qc = Query::new();
    qc.insert_key(k(3));
    qc.insert_key(k(4));
    qc.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'c']))),
    };

    let merged = Query::merge_multiple(vec![qa, qb, qc]);

    assert_eq!(merged.items.len(), 4);

    let conds = merged
        .conditional_subquery_branches
        .as_ref()
        .expect("should have conditional branches");

    // Key(2): in A + B → should have "a" and "b"
    let key2_sq = conds
        .get(&QueryItem::Key(k(2)))
        .unwrap()
        .subquery
        .as_ref()
        .unwrap();
    assert!(key2_sq.items.contains(&QueryItem::Key(vec![b'a'])));
    assert!(key2_sq.items.contains(&QueryItem::Key(vec![b'b'])));

    // Key(3): in B + C → should have "b" and "c"
    let key3_sq = conds
        .get(&QueryItem::Key(k(3)))
        .unwrap()
        .subquery
        .as_ref()
        .unwrap();
    assert!(
        key3_sq.items.contains(&QueryItem::Key(vec![b'b'])),
        "Key(3) should have 'b', got: {:?}",
        key3_sq.items
    );
    assert!(
        key3_sq.items.contains(&QueryItem::Key(vec![b'c'])),
        "Key(3) should have 'c', got: {:?}",
        key3_sq.items
    );

    // Key(4): only in C → should have just "c"
    let key4_sq = conds
        .get(&QueryItem::Key(k(4)))
        .unwrap()
        .subquery
        .as_ref()
        .unwrap();
    assert!(key4_sq.items.contains(&QueryItem::Key(vec![b'c'])));
    assert!(!key4_sq.items.contains(&QueryItem::Key(vec![b'a'])));
    assert!(!key4_sq.items.contains(&QueryItem::Key(vec![b'b'])));
}

/// Overlapping range items: partial overlap should correctly split.
#[test]
fn merge_multiple_overlapping_ranges_get_correct_defaults() {
    // Query A: Range(3..8), default selects "a"
    let mut query_a = Query::new();
    query_a.insert_range(k(3)..k(8));
    query_a.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'a']))),
    };

    // Query B: Range(1..5), default selects "b"
    let mut query_b = Query::new();
    query_b.insert_range(k(1)..k(5));
    query_b.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'b']))),
    };

    let merged = Query::merge_multiple(vec![query_a, query_b]);

    let conds = merged
        .conditional_subquery_branches
        .as_ref()
        .expect("should have conditional branches");

    // The overlapping portion Range(3..5) should have both "a" and "b"
    let overlap_branch = conds
        .get(&QueryItem::Range(k(3)..k(5)))
        .expect("Range(3..5) overlap should have a conditional");
    let overlap_sq = overlap_branch.subquery.as_ref().unwrap();
    assert!(
        overlap_sq.items.contains(&QueryItem::Key(vec![b'a'])),
        "Overlap range should contain 'a', got: {:?}",
        overlap_sq.items
    );
    assert!(
        overlap_sq.items.contains(&QueryItem::Key(vec![b'b'])),
        "Overlap range should contain 'b', got: {:?}",
        overlap_sq.items
    );

    // Range(1..3) is only in B → should have just "b"
    let b_only_branch = conds
        .get(&QueryItem::Range(k(1)..k(3)))
        .expect("Range(1..3) (B-only) should have a conditional");
    let b_only_sq = b_only_branch.subquery.as_ref().unwrap();
    assert!(b_only_sq.items.contains(&QueryItem::Key(vec![b'b'])));
    assert!(
        !b_only_sq.items.contains(&QueryItem::Key(vec![b'a'])),
        "Range(1..3) should NOT contain 'a' (only in B)"
    );
}

/// Empty default_subquery_branch on one side should not cause issues.
#[test]
fn merge_multiple_one_empty_default_no_panic() {
    let mut query_a = Query::new();
    query_a.insert_key(k(1));
    query_a.insert_key(k(2));
    // A has no default subquery branch (both None)

    let mut query_b = Query::new();
    query_b.insert_key(k(2));
    query_b.insert_key(k(3));
    query_b.default_subquery_branch = SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(Query::new_single_key(vec![b'b']))),
    };

    let merged = Query::merge_multiple(vec![query_a, query_b]);

    // Key(2) overlaps, but A has empty default → Key(2) should just get B's default
    let conds = merged.conditional_subquery_branches.as_ref().unwrap();
    let key2_branch = conds.get(&QueryItem::Key(k(2))).unwrap();
    let key2_sq = key2_branch.subquery.as_ref().unwrap();
    assert!(key2_sq.items.contains(&QueryItem::Key(vec![b'b'])));
    assert_eq!(key2_sq.items.len(), 1);
}

// ───────────────────────────────────────────────────────────────────────
// QueryItem::merge — covers merge.rs uncovered branches
// ───────────────────────────────────────────────────────────────────────

#[test]
fn query_item_merge_range_with_range_produces_larger_range() {
    let a = QueryItem::Range(k(1)..k(5));
    let b = QueryItem::Range(k(3)..k(8));
    let merged = a.merge(&b);
    assert_eq!(merged, QueryItem::Range(k(1)..k(8)));
}

#[test]
fn query_item_merge_key_with_range_produces_range_inclusive() {
    let a = QueryItem::Key(k(5));
    let b = QueryItem::Range(k(3)..k(5));
    let merged = a.merge(&b);
    assert_eq!(
        merged,
        QueryItem::RangeInclusive(std::ops::RangeInclusive::new(k(3), k(5)))
    );
}

#[test]
fn query_item_merge_range_after_with_range_to() {
    // RangeAfter(3..) is unbounded on the right, RangeTo(..10) is unbounded on the left
    // Merging produces RangeFull since together they cover all keys
    let a = QueryItem::RangeAfter(k(3)..);
    let b = QueryItem::RangeTo(..k(10));
    let merged = a.merge(&b);
    assert_eq!(merged, QueryItem::RangeFull(std::ops::RangeFull));
}

#[test]
fn query_item_merge_range_after_with_range_to_inclusive() {
    // Same as above — both together cover all keys
    let a = QueryItem::RangeAfter(k(3)..);
    let b = QueryItem::RangeToInclusive(..=k(10));
    let merged = a.merge(&b);
    assert_eq!(merged, QueryItem::RangeFull(std::ops::RangeFull));
}

#[test]
fn query_item_merge_range_after_to_with_range_after_to_inclusive() {
    // Both have exclusive start — exercises the start_non_inclusive path
    // with bounded ends
    let a = QueryItem::RangeAfterTo(k(1)..k(5));
    let b = QueryItem::RangeAfterToInclusive(k(1)..=k(8));
    let merged = a.merge(&b);
    assert_eq!(merged, QueryItem::RangeAfterToInclusive(k(1)..=k(8)));
}

#[test]
fn query_item_merge_range_after_with_range_from() {
    // Both unbounded on the right — produces RangeAfter (smaller exclusive start)
    let a = QueryItem::RangeAfter(k(3)..);
    let b = QueryItem::RangeFrom(k(5)..);
    let merged = a.merge(&b);
    assert_eq!(merged, QueryItem::RangeAfter(k(3)..));
}

#[test]
fn query_item_merge_range_to_with_range_from() {
    // Unbounded on both sides — produces RangeFull
    let a = QueryItem::RangeTo(..k(10));
    let b = QueryItem::RangeFrom(k(1)..);
    let merged = a.merge(&b);
    assert_eq!(merged, QueryItem::RangeFull(std::ops::RangeFull));
}

#[test]
fn query_item_merge_range_to_with_range_to_inclusive() {
    let a = QueryItem::RangeTo(..k(5));
    let b = QueryItem::RangeToInclusive(..=k(8));
    let merged = a.merge(&b);
    assert_eq!(merged, QueryItem::RangeToInclusive(..=k(8)));
}

#[test]
fn query_item_merge_range_from_with_range_inclusive() {
    let a = QueryItem::RangeFrom(k(3)..);
    let b = QueryItem::RangeInclusive(k(1)..=k(5));
    let merged = a.merge(&b);
    assert_eq!(merged, QueryItem::RangeFrom(k(1)..));
}

#[test]
fn query_item_merge_range_inclusive_with_range() {
    let a = QueryItem::RangeInclusive(k(1)..=k(5));
    let b = QueryItem::Range(k(3)..k(8));
    let merged = a.merge(&b);
    assert_eq!(merged, QueryItem::Range(k(1)..k(8)));
}
