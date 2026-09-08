use grovedb_query::{error::Error, PathKey, Query, QueryItem, SubqueryBranch};
use indexmap::IndexMap;

fn branch(path: Option<Vec<Vec<u8>>>, key: Option<u8>) -> SubqueryBranch {
    SubqueryBranch {
        subquery_path: path,
        subquery: key.map(|key| Box::new(Query::new_single_key(vec![key]))),
    }
}

fn assert_terminals(query: &Query, expected: &[PathKey]) {
    for terminal_keys in [Query::terminal_keys_v0, Query::terminal_keys_v1] {
        let mut actual = vec![];
        terminal_keys(query, vec![], 100, &mut actual).expect("valid terminal selection");
        actual.sort();
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(actual, expected);
    }
}

fn assert_empty_path_body_combinations(
    merge: impl Fn(SubqueryBranch, SubqueryBranch) -> SubqueryBranch,
) {
    for left_key in [None, Some(10)] {
        for right_key in [None, Some(20)] {
            let empty_path = branch(Some(vec![]), left_key);
            let no_path = branch(None, right_key);
            for (left, right) in [(empty_path.clone(), no_path.clone()), (no_path, empty_path)] {
                let merged = merge(left, right);
                assert_eq!(merged.subquery_path, Some(vec![]));
                let mut query = Query::new_single_key(vec![1]);
                query.default_subquery_branch = merged;
                if left_key.is_none() && right_key.is_none() {
                    // An empty path without a body is not a parent selection.
                    // Keep the existing terminal error instead of rewriting it to None.
                    for terminal_keys in [Query::terminal_keys_v0, Query::terminal_keys_v1] {
                        assert!(matches!(
                            terminal_keys(&query, vec![], 100, &mut vec![]),
                            Err(Error::CorruptedCodeExecution(_))
                        ));
                    }
                } else {
                    let expected: Vec<_> = left_key
                        .into_iter()
                        .chain(right_key)
                        .map(|key| (vec![vec![1]], vec![key]))
                        .collect();
                    assert_terminals(&query, &expected);
                }
            }
        }
    }
}

#[test]
fn empty_path_branch_merge_preserves_bodies_in_both_orders() {
    assert_empty_path_body_combinations(|left, right| left.merge(&right).expect("branch merge"));
}

#[test]
fn empty_path_default_merge_preserves_bodies_in_both_orders() {
    assert_empty_path_body_combinations(|left, right| {
        let mut query = Query::new_single_key(vec![1]);
        query.default_subquery_branch = left;
        query
            .merge_default_subquery_branch(right)
            .expect("default merge");
        assert!(query.conditional_subquery_branches.is_none());
        query.default_subquery_branch
    });
}

#[test]
fn empty_path_conditional_overlap_preserves_each_selected_branch() {
    let empty_path = (
        QueryItem::RangeInclusive(vec![1]..=vec![3]),
        branch(Some(vec![]), Some(10)),
    );
    let no_path = (QueryItem::Key(vec![2]), branch(None, Some(20)));
    for (left, right) in [(empty_path.clone(), no_path.clone()), (no_path, empty_path)] {
        let existing = Some(IndexMap::from([left.clone()]));
        let merged = Query::merge_conditional_subquery_branches_with_new_at_query_item(
            existing.clone(),
            right.0.clone(),
            right.1.clone(),
        )
        .expect("conditional map merge");
        let mut query = Query::new();
        query.insert_range_inclusive(vec![1]..=vec![3]);
        query.conditional_subquery_branches = existing;
        query
            .merge_conditional_boxed_subquery(right.0, right.1)
            .expect("conditional merge");
        assert_eq!(query.conditional_subquery_branches, Some(merged));
        // Splitting ranges can create exclusive bounds that terminal_keys
        // cannot enumerate. Resolve each known key's branch before walking it.
        for key in 1..=3 {
            let selected = query
                .conditional_subquery_branches
                .as_ref()
                .unwrap()
                .iter()
                .find(|(item, _)| item.contains(&[key]))
                .expect("selected branch")
                .1;
            let mut selection = Query::new_single_key(vec![key]);
            selection.default_subquery_branch = selected.clone();
            let mut expected = vec![(vec![vec![key]], vec![10])];
            if key == 2 {
                expected.push((vec![vec![key]], vec![20]));
            }
            assert_terminals(&selection, &expected);
        }
    }
}

#[test]
fn empty_path_nested_whole_query_merges_preserve_terminal_union() {
    for conditional in [false, true] {
        let mut left = Query::new_single_key(vec![1]);
        let mut inner = Query::new_single_key(vec![2]);
        // Exercise the supported setter as well as a conditional branch.
        if conditional {
            inner.add_conditional_subquery(
                QueryItem::Key(vec![2]),
                Some(vec![]),
                Some(Query::new_single_key(vec![10])),
            );
        } else {
            inner.set_subquery_path(vec![]);
            inner.set_subquery(Query::new_single_key(vec![10]));
        }
        left.set_subquery(inner);
        let mut right = Query::new_single_key(vec![1]);
        let mut inner = Query::new_single_key(vec![2]);
        if conditional {
            inner.add_conditional_subquery(
                QueryItem::Key(vec![2]),
                None,
                Some(Query::new_single_key(vec![20])),
            );
        } else {
            inner.set_subquery(Query::new_single_key(vec![20]));
        }
        right.set_subquery(inner);
        for (left, right) in [(left.clone(), right.clone()), (right, left)] {
            let multiple =
                Query::merge_multiple(vec![left.clone(), right.clone()]).expect("multiple merge");
            let directional = Query::merge_multiple_directional(vec![left.clone(), right.clone()])
                .expect("directional merge");
            let mut with = left;
            with.merge_with(right).expect("merge with");
            for query in [multiple, directional, with] {
                assert_terminals(
                    &query,
                    &[
                        (vec![vec![1], vec![2]], vec![10]),
                        (vec![vec![1], vec![2]], vec![20]),
                    ],
                );
            }
        }
    }
}

#[test]
fn empty_path_decoded_query_keeps_wire_representation_and_merges() {
    let mut query = Query::new_single_key(vec![1]);
    query.set_subquery_path(vec![]);
    query.set_subquery(Query::new_single_key(vec![10]));
    let config = bincode::config::standard().with_big_endian();
    let encoded = bincode::encode_to_vec(&query, config).expect("encode");
    let (decoded, consumed): (Query, _) =
        bincode::decode_from_slice(&encoded, config).expect("decode");
    assert_eq!(consumed, encoded.len());
    assert_eq!(decoded, query);
    assert_eq!(
        bincode::encode_to_vec(&decoded, config).expect("reencode"),
        encoded
    );
    let mut other = Query::new_single_key(vec![1]);
    other.set_subquery(Query::new_single_key(vec![20]));
    for queries in [vec![decoded.clone(), other.clone()], vec![other, decoded]] {
        let merged = Query::merge_multiple(queries).expect("decoded merge");
        assert_terminals(
            &merged,
            &[(vec![vec![1]], vec![10]), (vec![vec![1]], vec![20])],
        );
    }
}

#[test]
fn empty_path_and_nonempty_path_merge_keeps_descent() {
    // A path containing an empty key has one component, unlike an empty path.
    for path in [vec![vec![30]], vec![vec![]], vec![vec![30], vec![40]]] {
        let empty = branch(Some(vec![]), Some(10));
        let nonempty = branch(Some(path.clone()), Some(20));
        for (left, right) in [(empty.clone(), nonempty.clone()), (nonempty, empty)] {
            let mut query = Query::new_single_key(vec![1]);
            query.default_subquery_branch = left.merge(&right).expect("merge with path");
            let mut descended = vec![vec![1]];
            descended.extend(path.clone());
            assert_terminals(&query, &[(vec![vec![1]], vec![10]), (descended, vec![20])]);
        }
    }
}
