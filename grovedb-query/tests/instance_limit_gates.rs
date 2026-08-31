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
fn merge_default_subquery_branch_refuses_limits_reachable_through_promotion() {
    // The unchecked body can PROMOTE a default branch into a
    // conditional when the two defaults' subquery paths diverge, and
    // that promotion merges with an already-existing conditional. Here
    // the receiver's default has path `[k]`, its existing `Key(k)`
    // conditional carries an instance limit, and the incoming default
    // is pathless — a defaults-only scan sees no limit, but the merge
    // would conflate the limited conditional's result set. The gate
    // must scan the whole receiver.
    let mut receiver = Query::new_range_full();
    receiver.set_subquery_path(vec![b"k".to_vec()]);
    receiver.set_subquery(Query::new_range_full());
    receiver.add_conditional_subquery(QueryItem::Key(b"k".to_vec()), None, Some(limited_query(2)));

    match receiver.merge_default_subquery_branch(branch_with(Some(Query::new_range_full()))) {
        Err(Error::NotSupported(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => {
            panic!("a limit reachable through conditional promotion must be refused, got {other:?}")
        }
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
fn version_3_round_trips_through_both_decode_paths() {
    // `decode_from_slice` and `decode_from_std_read` exercise the owned
    // `Decode` impl; `borrow_decode_from_slice` exercises the separate
    // `BorrowDecode` impl — both must speak version 3 and reject its
    // malformed flags.
    let mut nested = Query::new_range_full();
    nested.set_subquery(limited_query(2));
    nested.limit = Some(7);
    let bytes = bincode::encode_to_vec(&nested, config::standard()).expect("encode");

    let mut reader = Cursor::new(bytes.clone());
    let decoded: Query =
        bincode::decode_from_std_read(&mut reader, config::standard()).expect("owned decode");
    assert_eq!(decoded, nested);

    let (borrow_decoded, consumed): (Query, usize) =
        bincode::borrow_decode_from_slice(&bytes, config::standard()).expect("borrowed decode");
    assert_eq!(consumed, bytes.len());
    assert_eq!(borrow_decoded, nested);

    // A version-3 node carrying BOTH flags decodes through the borrowed
    // path too (read mode then limit, in that order).
    let mut both = limited_query(9);
    both.read_mode = Some(Box::new(grovedb_query::ReadMode::SumBudget(
        grovedb_query::SumBudgetRead {
            sum_limit: 500,
            match_limit: Some(100),
        },
    )));
    let both_bytes = bincode::encode_to_vec(&both, config::standard()).expect("encode");
    assert_eq!(both_bytes[0], 3);
    assert_eq!(both_bytes[1], 0b11);
    let (borrow_decoded, _): (Query, usize) =
        bincode::borrow_decode_from_slice(&both_bytes, config::standard())
            .expect("borrowed decode of both flags");
    assert_eq!(borrow_decoded, both);

    // Both decoders reject the malformed flags.
    for bad_flags in [0b00u8, 0b01, 0b110] {
        let mut malformed = bytes.clone();
        assert_eq!(malformed[0], 3);
        malformed[1] = bad_flags;
        let mut reader = Cursor::new(malformed.clone());
        assert!(
            bincode::decode_from_std_read::<Query, _, _>(&mut reader, config::standard()).is_err(),
            "owned decode must reject version 3 with flags {bad_flags:#b}"
        );
        assert!(
            bincode::borrow_decode_from_slice::<Query, _>(&malformed, config::standard()).is_err(),
            "borrowed decode must reject version 3 with flags {bad_flags:#b}"
        );
    }
}

/// The serde representation must stay positional-format-safe: both
/// optional fields are always serialized (with `default` tolerated on
/// deserialization), because a `skip_serializing_if` field has no
/// presence tag in a non-self-describing serializer — the remaining
/// fields shift and the payload decodes wrongly.
#[cfg(feature = "serde")]
#[test]
fn serde_bincode_round_trips_every_option_combination() {
    let read_mode = || {
        Box::new(grovedb_query::ReadMode::SumBudget(
            grovedb_query::SumBudgetRead {
                sum_limit: 500,
                match_limit: Some(100),
            },
        ))
    };
    let combos: [(Option<Box<grovedb_query::ReadMode>>, Option<u16>); 4] = [
        (None, None),
        (Some(read_mode()), None),
        (None, Some(3)),
        (Some(read_mode()), Some(3)),
    ];
    for (read_mode, limit) in combos {
        let mut query = Query::new_range_full();
        query.set_subquery(limited_query(2));
        query.read_mode = read_mode;
        query.limit = limit;

        let bytes = bincode::serde::encode_to_vec(&query, config::standard())
            .expect("serde-bincode encode");
        let (decoded, consumed): (Query, usize) =
            bincode::serde::decode_from_slice(&bytes, config::standard())
                .expect("serde-bincode decode");
        assert_eq!(consumed, bytes.len(), "no trailing bytes");
        assert_eq!(decoded, query, "serde-bincode round trip");
    }
}

#[test]
fn display_prints_the_instance_limit() {
    let rendered = format!("{}", limited_query(3));
    assert!(rendered.contains("limit: 3"), "got: {rendered}");
    let plain = format!("{}", Query::new_range_full());
    assert!(!plain.contains("limit:"), "got: {plain}");
}
