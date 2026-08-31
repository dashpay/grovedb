//! Gate coverage for per-instance limits (`Query::limit`) at the
//! `grovedb-query` level: every public merge entry point refuses
//! limit-carrying nodes (the `_unchecked` internals destructure the
//! field away by design), the version-3 encoding decodes through the
//! owned-`Decode` path too, and the helpers walk every branch flavor.

use std::io::Cursor;

use bincode::config;
use grovedb_query::{error::Error, Query, QueryItem, SubqueryBranch};

fn limited_query(limit: u16) -> Query {
    let mut query = Query::new_range_full();
    query.limit = Some(limit);
    query
}

fn branch_with(subquery: Option<Query>) -> SubqueryBranch {
    SubqueryBranch {
        subquery_path: None,
        subquery: subquery.map(Box::new),
    }
}

#[test]
fn instance_limit_walkers_cover_every_branch_flavor() {
    let plain = Query::new_range_full();
    assert!(!plain.has_instance_limit_anywhere());
    assert!(!plain.has_zero_instance_limit_anywhere());

    // Direct.
    assert!(limited_query(1).has_instance_limit_anywhere());
    assert!(limited_query(0).has_zero_instance_limit_anywhere());
    assert!(!limited_query(1).has_zero_instance_limit_anywhere());

    // In the default branch.
    let mut in_default = Query::new_range_full();
    in_default.set_subquery(limited_query(2));
    assert!(in_default.has_instance_limit_anywhere());

    // In a conditional branch, nested one level down.
    let mut nested = Query::new_range_full();
    nested.set_subquery(limited_query(0));
    let mut in_conditional = Query::new_range_full();
    in_conditional.add_conditional_subquery(QueryItem::Key(b"k".to_vec()), None, Some(nested));
    assert!(in_conditional.has_instance_limit_anywhere());
    assert!(in_conditional.has_zero_instance_limit_anywhere());
}

#[test]
fn subquery_branch_merge_refuses_instance_limits_on_either_side() {
    let limited = branch_with(Some(limited_query(1)));
    let plain = branch_with(Some(Query::new_range_full()));

    for (ours, theirs) in [(&limited, &plain), (&plain, &limited)] {
        match ours.merge(theirs) {
            Err(Error::NotSupported(message)) => {
                assert!(message.contains("per-instance"), "got: {message}")
            }
            other => panic!("branch merge must refuse instance limits, got {other:?}"),
        }
    }
    // Limit-free branches still merge.
    plain.merge(&plain).expect("plain branches merge");
}

#[test]
fn merge_default_subquery_branch_refuses_instance_limits_on_either_side() {
    let mut carrying = Query::new_range_full();
    carrying.set_subquery(limited_query(1));
    let mut plain = Query::new_range_full();
    plain.set_subquery(Query::new_range_full());

    match carrying.merge_default_subquery_branch(branch_with(Some(Query::new_range_full()))) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("must refuse a carrying receiver, got {other:?}"),
    }
    match plain.merge_default_subquery_branch(branch_with(Some(limited_query(1)))) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("must refuse a carrying incomer, got {other:?}"),
    }
}

#[test]
fn merge_conditional_entry_points_refuse_instance_limits_on_either_side() {
    let mut carrying_receiver = Query::new_range_full();
    carrying_receiver.limit = Some(1);
    match carrying_receiver.merge_conditional_boxed_subquery(
        QueryItem::Key(b"k".to_vec()),
        branch_with(Some(Query::new_range_full())),
    ) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("must refuse a carrying receiver, got {other:?}"),
    }

    let mut plain_receiver = Query::new_range_full();
    match plain_receiver.merge_conditional_boxed_subquery(
        QueryItem::Key(b"k".to_vec()),
        branch_with(Some(limited_query(1))),
    ) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("must refuse a carrying incomer, got {other:?}"),
    }

    // The static map-level entry point: existing side and incoming side.
    let mut existing = indexmap::IndexMap::new();
    existing.insert(
        QueryItem::Key(b"a".to_vec()),
        branch_with(Some(limited_query(1))),
    );
    match Query::merge_conditional_subquery_branches_with_new_at_query_item(
        Some(existing),
        QueryItem::Key(b"b".to_vec()),
        branch_with(Some(Query::new_range_full())),
    ) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("must refuse a carrying existing map, got {other:?}"),
    }
    match Query::merge_conditional_subquery_branches_with_new_at_query_item(
        None,
        QueryItem::Key(b"b".to_vec()),
        branch_with(Some(limited_query(1))),
    ) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("must refuse a carrying incomer, got {other:?}"),
    }
}

#[test]
fn whole_query_merges_refuse_instance_limits() {
    match Query::merge_multiple(vec![Query::new_range_full(), limited_query(1)]) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("merge_multiple must refuse instance limits, got {other:?}"),
    }
    match Query::merge_multiple_directional(vec![Query::new_range_full(), limited_query(1)]) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("merge_multiple_directional must refuse instance limits, got {other:?}"),
    }
    let mut target = Query::new_range_full();
    match target.merge_with(limited_query(1)) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("merge_with must refuse instance limits, got {other:?}"),
    }
    let mut carrying_target = limited_query(1);
    match carrying_target.merge_with(Query::new_range_full()) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("merge_with must refuse a carrying receiver, got {other:?}"),
    }
}

#[test]
fn version_3_round_trips_through_the_owned_decode_path_too() {
    // `decode_from_slice` exercises `BorrowDecode`; a reader exercises
    // the owned `Decode` impl — both must speak version 3.
    let mut nested = Query::new_range_full();
    nested.set_subquery(limited_query(2));
    nested.limit = Some(7);
    let bytes = bincode::encode_to_vec(&nested, config::standard()).expect("encode");

    let mut reader = Cursor::new(bytes.clone());
    let decoded: Query =
        bincode::decode_from_std_read(&mut reader, config::standard()).expect("owned decode");
    assert_eq!(decoded, nested);

    // Owned decode rejects the malformed flags too.
    let mut flagless = bytes;
    assert_eq!(flagless[0], 3);
    flagless[1] = 0b01;
    let mut reader = Cursor::new(flagless);
    assert!(
        bincode::decode_from_std_read::<Query, _, _>(&mut reader, config::standard()).is_err(),
        "owned decode must reject version 3 without the limit flag"
    );
}

#[test]
fn display_prints_the_instance_limit() {
    let rendered = format!("{}", limited_query(3));
    assert!(rendered.contains("limit: 3"), "got: {rendered}");
    let plain = format!("{}", Query::new_range_full());
    assert!(!plain.contains("limit:"), "got: {plain}");
}
