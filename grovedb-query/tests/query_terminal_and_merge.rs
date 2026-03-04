use grovedb_query::{error::Error, Query, QueryItem, SubqueryBranch};

fn k(v: u8) -> Vec<u8> {
    vec![v]
}

fn p(v: &[u8]) -> Vec<u8> {
    v.to_vec()
}

#[test]
fn terminal_keys_plain_and_subquery_path_forms() {
    let mut plain = Query::new();
    plain.insert_keys(vec![k(1), k(2)]);

    let mut result = vec![];
    let added = plain
        .terminal_keys(vec![k(99)], 10, &mut result)
        .expect("terminal keys");
    assert_eq!(added, 2);
    assert_eq!(result.len(), 2);
    assert!(result.contains(&(vec![k(99)], k(1))));
    assert!(result.contains(&(vec![k(99)], k(2))));

    let mut with_path = Query::new_single_key(k(7));
    with_path.set_subquery_path(vec![k(8), k(9)]);

    let mut result = vec![];
    let added = with_path
        .terminal_keys(vec![k(1)], 10, &mut result)
        .expect("terminal keys");
    assert_eq!(added, 1);
    assert_eq!(result, vec![(vec![k(1), k(7), k(8)], k(9))]);
}

#[test]
fn terminal_keys_recursive_and_deduplicated() {
    let mut leaf = Query::new();
    leaf.insert_key(k(3));

    let mut root = Query::new_single_key(k(1));
    root.set_subquery_path(vec![k(2)]);
    root.set_subquery(leaf);

    // Duplicate key in conditional branch must not be added from top-level items twice.
    root.add_conditional_subquery(
        QueryItem::Key(k(1)),
        Some(vec![k(4), k(5)]),
        Some(Query::new_single_key(k(6))),
    );

    let mut result = vec![];
    let added = root
        .terminal_keys(vec![], 10, &mut result)
        .expect("terminal keys");

    assert_eq!(added, 1);
    assert_eq!(result, vec![(vec![k(1), k(4), k(5)], k(6))]);
}

#[test]
fn terminal_keys_error_paths_are_reported() {
    let mut with_unbounded = Query::new();
    with_unbounded.insert_all();
    let mut out = vec![];
    let err = with_unbounded
        .terminal_keys(vec![], 10, &mut out)
        .expect_err("must fail on unbounded item");
    assert!(matches!(err, Error::NotSupported(_)));

    let mut conditional_unbounded = Query::new_single_key(k(1));
    conditional_unbounded.add_conditional_subquery(QueryItem::RangeFull(..), None, None);
    let mut out = vec![];
    let err = conditional_unbounded
        .terminal_keys(vec![], 10, &mut out)
        .expect_err("must fail on unbounded conditional item");
    assert!(matches!(err, Error::NotSupported(_)));

    let mut exceeding = Query::new();
    exceeding.insert_keys(vec![k(1), k(2)]);
    let mut out = vec![];
    let err = exceeding
        .terminal_keys(vec![], 1, &mut out)
        .expect_err("must fail on max results");
    assert!(matches!(err, Error::RequestAmountExceeded(_)));

    let mut corrupted_path = Query::new_single_key(k(1));
    corrupted_path.set_subquery_path(vec![]);
    let mut out = vec![];
    let err = corrupted_path
        .terminal_keys(vec![], 10, &mut out)
        .expect_err("must fail on empty subquery path");
    assert!(matches!(err, Error::CorruptedCodeExecution(_)));
}

#[test]
fn merge_apis_cover_default_and_conditional_paths() {
    let mut base = Query::new();
    base.insert_key(k(1));

    let mut other = Query::new();
    other.insert_key(k(2));
    other.set_subquery_path(vec![k(9)]);

    base.merge_with(other);
    assert_eq!(base.items.len(), 2);
    assert!(base.items.contains(&QueryItem::Key(k(1))));
    assert!(base.items.contains(&QueryItem::Key(k(2))));

    let conditional = base
        .conditional_subquery_branches
        .as_ref()
        .expect("conditional branch should be created");
    assert!(conditional.contains_key(&QueryItem::Key(k(2))));

    let merged_empty = Query::merge_multiple(vec![]);
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
    );

    assert_eq!(merged.len(), 3);
    assert!(merged.contains_key(&QueryItem::Range(k(1)..k(3))));
    assert!(merged.contains_key(&QueryItem::Range(k(3)..k(5))));
    assert!(merged.contains_key(&QueryItem::Range(k(5)..k(7))));
}
