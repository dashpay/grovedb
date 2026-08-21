use grovedb_query::{error::Error, Query, QueryItem, SubqueryBranch};

fn k(v: u8) -> Vec<u8> {
    vec![v]
}

fn p(v: &[u8]) -> Vec<u8> {
    v.to_vec()
}

/// Both terminal-key versions, for behavior shared between them.
type TerminalKeysFn =
    fn(&Query, Vec<Vec<u8>>, usize, &mut Vec<(Vec<Vec<u8>>, Vec<u8>)>) -> Result<usize, Error>;
const TERMINAL_KEYS_VERSIONS: [TerminalKeysFn; 2] =
    [Query::terminal_keys_v0, Query::terminal_keys_v1];

#[test]
fn terminal_keys_plain_and_subquery_path_forms() {
    for terminal_keys in TERMINAL_KEYS_VERSIONS {
        let mut plain = Query::new();
        plain.insert_keys(vec![k(1), k(2)]);

        let mut result = vec![];
        let added = terminal_keys(&plain, vec![k(99)], 10, &mut result).expect("terminal keys");
        assert_eq!(added, 2);
        assert_eq!(result.len(), 2);
        assert!(result.contains(&(vec![k(99)], k(1))));
        assert!(result.contains(&(vec![k(99)], k(2))));

        let mut with_path = Query::new_single_key(k(7));
        with_path.set_subquery_path(vec![k(8), k(9)]);

        let mut result = vec![];
        let added = terminal_keys(&with_path, vec![k(1)], 10, &mut result).expect("terminal keys");
        assert_eq!(added, 1);
        assert_eq!(result, vec![(vec![k(1), k(7), k(8)], k(9))]);
    }
}

#[test]
fn terminal_keys_recursive_and_deduplicated() {
    // The queried key has a matching conditional branch; both versions must
    // resolve it to the conditional branch exactly once (v0 via its dedup
    // set, v1 via per-item branch resolution).
    for terminal_keys in TERMINAL_KEYS_VERSIONS {
        let mut leaf = Query::new();
        leaf.insert_key(k(3));

        let mut root = Query::new_single_key(k(1));
        root.set_subquery_path(vec![k(2)]);
        root.set_subquery(leaf);

        root.add_conditional_subquery(
            QueryItem::Key(k(1)),
            Some(vec![k(4), k(5)]),
            Some(Query::new_single_key(k(6))),
        );

        let mut result = vec![];
        let added = terminal_keys(&root, vec![], 10, &mut result).expect("terminal keys");

        assert_eq!(added, 1);
        assert_eq!(result, vec![(vec![k(1), k(4), k(5)], k(6))]);
    }
}

#[test]
fn terminal_keys_v1_conditionals_only_apply_to_queried_items() {
    let mut query = Query::new_single_key(k(1));
    query.add_conditional_subquery(
        QueryItem::Key(k(2)),
        Some(vec![k(3)]),
        Some(Query::new_single_key(k(4))),
    );

    let mut result = vec![];
    let added = query
        .terminal_keys_v1(vec![], 10, &mut result)
        .expect("terminal keys");

    assert_eq!(added, 1);
    assert_eq!(result, vec![(vec![], k(1))]);
}

#[test]
fn terminal_keys_v0_applies_conditionals_before_items() {
    let mut query = Query::new_single_key(k(1));
    query.add_conditional_subquery(
        QueryItem::Key(k(2)),
        Some(vec![k(3)]),
        Some(Query::new_single_key(k(4))),
    );

    let mut result = vec![];
    let added = query
        .terminal_keys_v0(vec![], 10, &mut result)
        .expect("terminal keys");

    assert_eq!(added, 2);
    assert_eq!(result, vec![(vec![k(2), k(3)], k(4)), (vec![], k(1))]);
}

#[test]
fn terminal_keys_v1_empty_conditional_branch_keeps_matching_key_terminal() {
    let mut query = Query::new_single_key(k(1));
    query.add_conditional_subquery(QueryItem::Key(k(1)), None, None);

    let mut result = vec![];
    let added = query
        .terminal_keys_v1(vec![], 10, &mut result)
        .expect("terminal keys");

    assert_eq!(added, 1);
    assert_eq!(result, vec![(vec![], k(1))]);
}

#[test]
fn terminal_keys_v0_empty_conditional_branch_drops_matching_key() {
    // Frozen v0 quirk (issue #689): a (None, None) conditional override marks
    // the key as already added without pushing a terminal key for it.
    let mut query = Query::new_single_key(k(1));
    query.add_conditional_subquery(QueryItem::Key(k(1)), None, None);

    let mut result = vec![];
    let added = query
        .terminal_keys_v0(vec![], 10, &mut result)
        .expect("terminal keys");

    assert_eq!(added, 0);
    assert!(result.is_empty());
}

#[test]
fn terminal_keys_error_paths_are_reported() {
    for terminal_keys in TERMINAL_KEYS_VERSIONS {
        let mut with_unbounded = Query::new();
        with_unbounded.insert_all();
        let mut out = vec![];
        let err = terminal_keys(&with_unbounded, vec![], 10, &mut out)
            .expect_err("must fail on unbounded item");
        assert!(matches!(err, Error::NotSupported(_)));

        let mut conditional_unbounded = Query::new_single_key(k(1));
        conditional_unbounded.add_conditional_subquery(QueryItem::RangeFull(..), None, None);
        let mut out = vec![];
        let err = terminal_keys(&conditional_unbounded, vec![], 10, &mut out)
            .expect_err("must fail on unbounded conditional item");
        assert!(matches!(err, Error::NotSupported(_)));

        let mut exceeding = Query::new();
        exceeding.insert_keys(vec![k(1), k(2)]);
        let mut out = vec![];
        let err =
            terminal_keys(&exceeding, vec![], 1, &mut out).expect_err("must fail on max results");
        assert!(matches!(err, Error::RequestAmountExceeded(_)));

        let mut corrupted_path = Query::new_single_key(k(1));
        corrupted_path.set_subquery_path(vec![]);
        let mut out = vec![];
        let err = terminal_keys(&corrupted_path, vec![], 10, &mut out)
            .expect_err("must fail on empty subquery path");
        assert!(matches!(err, Error::CorruptedCodeExecution(_)));

        let mut exceeding_subquery_path = Query::new_single_key(k(1));
        exceeding_subquery_path.set_subquery_path(vec![k(2)]);
        let mut out = vec![(vec![], k(0))];
        let err = terminal_keys(&exceeding_subquery_path, vec![], 1, &mut out)
            .expect_err("must fail when subquery path result would exceed limit");
        assert!(matches!(err, Error::RequestAmountExceeded(_)));
    }
}

#[test]
fn terminal_keys_rejects_excessive_nesting_depth() {
    for terminal_keys in TERMINAL_KEYS_VERSIONS {
        // Build a chain of 65 nested subqueries (limit is 64)
        let mut inner = Query::new_single_key(k(1));
        for _ in 0..65 {
            let mut outer = Query::new_single_key(k(1));
            outer.set_subquery(inner);
            inner = outer;
        }
        let mut out = vec![];
        let err = terminal_keys(&inner, vec![], 1000, &mut out)
            .expect_err("must fail on excessive depth");
        assert!(matches!(err, Error::NotSupported(_)));
    }
}

#[test]
fn terminal_keys_v0_conditional_subquery_skips_duplicate_item_key() {
    let mut query = Query::new_single_key(k(1));
    query.add_conditional_subquery(
        QueryItem::Key(k(1)),
        None,
        Some(Query::new_single_key(k(2))),
    );

    let mut result = vec![];
    let added = query
        .terminal_keys_v0(vec![], 10, &mut result)
        .expect("terminal keys");

    assert_eq!(added, 1);
    assert_eq!(result, vec![(vec![k(1)], k(2))]);
}

#[test]
fn terminal_keys_v0_default_branch_recurses_with_subquery_path() {
    let mut query = Query::new_single_key(k(1));
    query.set_subquery_path(vec![k(2)]);
    query.set_subquery(Query::new_single_key(k(3)));

    let mut result = vec![];
    let added = query
        .terminal_keys_v0(vec![], 10, &mut result)
        .expect("terminal keys");

    assert_eq!(added, 1);
    assert_eq!(result, vec![(vec![k(1), k(2)], k(3))]);
}

#[test]
fn terminal_keys_v0_default_branch_recurses_without_subquery_path() {
    let mut query = Query::new_single_key(k(4));
    query.set_subquery(Query::new_single_key(k(5)));

    let mut result = vec![];
    let added = query
        .terminal_keys_v0(vec![], 10, &mut result)
        .expect("terminal keys");

    assert_eq!(added, 1);
    assert_eq!(result, vec![(vec![k(4)], k(5))]);
}

#[test]
fn merge_apis_cover_default_and_conditional_paths() {
    let mut base = Query::new();
    base.insert_key(k(1));

    let mut other = Query::new();
    other.insert_key(k(2));
    other.set_subquery_path(vec![k(9)]);

    base.merge_with(other).expect("merge must succeed");
    assert_eq!(base.items.len(), 2);
    assert!(base.items.contains(&QueryItem::Key(k(1))));
    assert!(base.items.contains(&QueryItem::Key(k(2))));

    let conditional = base
        .conditional_subquery_branches
        .as_ref()
        .expect("conditional branch should be created");
    assert!(conditional.contains_key(&QueryItem::Key(k(2))));

    let merged_empty = Query::merge_multiple(vec![]).expect("merge must succeed");
    assert!(merged_empty.items.is_empty());

    let mut left = Query::new_single_key(k(10));
    left.default_subquery_branch = SubqueryBranch {
        subquery_path: Some(vec![k(1), k(2)]),
        subquery: Some(Box::new(Query::new_single_key(k(20)))),
    };

    left.merge_default_subquery_branch(SubqueryBranch {
        subquery_path: Some(vec![k(1), k(3)]),
        subquery: Some(Box::new(Query::new_single_key(k(30)))),
    });

    assert_eq!(left.default_subquery_branch.subquery_path, Some(vec![k(1)]));
    let merged_conditionals = left
        .conditional_subquery_branches
        .as_ref()
        .expect("expected merged conditionals from split path");
    assert!(merged_conditionals.contains_key(&QueryItem::Key(k(2))));
    assert!(merged_conditionals.contains_key(&QueryItem::Key(k(3))));
}

#[test]
fn merge_conditional_subquery_branches_splits_intersections() {
    use indexmap::IndexMap;

    let mut existing = IndexMap::new();
    existing.insert(
        QueryItem::Range(k(1)..k(5)),
        SubqueryBranch {
            subquery_path: Some(vec![p(b"a")]),
            subquery: None,
        },
    );

    let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Range(k(3)..k(7)),
        SubqueryBranch {
            subquery_path: Some(vec![p(b"a")]),
            subquery: None,
        },
    )
    .expect("no read modes involved");

    assert_eq!(merged.len(), 3);
    assert!(merged.contains_key(&QueryItem::Range(k(1)..k(3))));
    assert!(merged.contains_key(&QueryItem::Range(k(3)..k(5))));
    assert!(merged.contains_key(&QueryItem::Range(k(5)..k(7))));
}
