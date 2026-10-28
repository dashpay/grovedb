use bincode::{config::standard, decode_from_slice, encode_to_vec};
use grovedb_query::Query;

#[path = "support/query_v3.rs"]
mod query_v3;

#[test]
fn current_v3_wire_reader_rejects_offsets_even_in_dormant_branches() {
    for offset in [0, 7] {
        for limit in [None, Some(2)] {
            let mut leaf = Query::new_range_full();
            leaf.offset = Some(offset);
            leaf.limit = limit;
            let mut root = Query::new_single_key(b"unused".to_vec());
            root.add_conditional_subquery(
                grovedb_query::QueryItem::Key(b"dormant".to_vec()),
                None,
                Some(leaf.clone()),
            );
            for query in [leaf, root] {
                let bytes = encode_to_vec(query, standard()).unwrap();
                let error =
                    decode_from_slice::<query_v3::Query, _>(&bytes, standard()).unwrap_err();
                assert!(error.to_string().contains("unknown Query version 3 flags"));
            }
        }
    }
}

#[test]
fn local_offset_wire_round_trip_preserves_zero_and_positive_skips() {
    for offset in [0u16, 7, u16::MAX] {
        let plain = encode_to_vec(Query::new_range_full(), standard()).unwrap();
        let mut bytes = vec![3, 4];
        bytes.extend_from_slice(&plain[1..]);
        bytes.extend(encode_to_vec(offset, standard()).unwrap());
        let (query, consumed): (Query, usize) = decode_from_slice(&bytes, standard())
            .expect("a count-unit offset must survive Query decoding");
        assert_eq!(consumed, bytes.len());
        assert_eq!(encode_to_vec(&query, standard()).unwrap(), bytes);
        assert_eq!(query.offset, Some(offset));
        assert_eq!(
            bincode::decode_from_slice_untrusted::<Query, _>(&bytes, standard())
                .unwrap()
                .0,
            query
        );
        assert_eq!(
            bincode::borrow_decode_from_slice::<Query, _>(&bytes, standard())
                .unwrap()
                .0,
            query
        );
        assert_eq!(
            bincode::borrow_decode_from_slice_untrusted::<Query, _>(&bytes, standard())
                .unwrap()
                .0,
            query
        );
    }
}

#[test]
fn local_offset_merges_and_terminal_projections_fail_closed() {
    let mut leaf = Query::new_range_full();
    leaf.offset = Some(0);
    let mut root = Query::new_single_key(b"unused".to_vec());
    root.add_conditional_subquery(
        grovedb_query::QueryItem::Key(b"dormant".to_vec()),
        None,
        Some(leaf),
    );
    assert!(root.has_instance_offset_anywhere());
    assert!(Query::merge_multiple(vec![root.clone()]).is_err());
    assert!(root.merge_with(Query::new()).is_err());
    assert!(root.terminal_keys_v1(vec![], 10, &mut vec![]).is_err());
    assert!(root.terminal_keys_v0(vec![], 10, &mut vec![]).is_err());
}

#[test]
fn offset_only_merge_refusals_name_both_per_instance_fields() {
    use grovedb_query::{error::Error, QueryItem, SubqueryBranch};

    let branch = |query| SubqueryBranch {
        subquery_path: None,
        subquery: Some(Box::new(query)),
    };
    for offset in [0, 7] {
        let mut carrying = Query::new_range_full();
        carrying.offset = Some(offset);
        let plain = Query::new_range_full();
        for (ours, theirs) in [
            (carrying.clone(), plain.clone()),
            (plain.clone(), carrying.clone()),
        ] {
            let mut default = Query::new_range_full();
            default.set_subquery(ours.clone());
            let mut conditional = ours.clone();
            let mut existing = indexmap::IndexMap::new();
            existing.insert(QueryItem::Key(b"a".to_vec()), branch(ours.clone()));
            let errors = [
                branch(ours.clone())
                    .merge(&branch(theirs.clone()))
                    .unwrap_err(),
                Query::merge_multiple(vec![ours.clone(), theirs.clone()]).unwrap_err(),
                Query::merge_multiple_directional(vec![ours.clone(), theirs.clone()]).unwrap_err(),
                ours.clone().merge_with(theirs.clone()).unwrap_err(),
                default
                    .merge_default_subquery_branch(branch(theirs.clone()))
                    .unwrap_err(),
                conditional
                    .merge_conditional_boxed_subquery(
                        QueryItem::Key(b"b".to_vec()),
                        branch(theirs.clone()),
                    )
                    .unwrap_err(),
                Query::merge_conditional_subquery_branches_with_new_at_query_item(
                    Some(existing),
                    QueryItem::Key(b"b".to_vec()),
                    branch(theirs),
                )
                .unwrap_err(),
            ];
            for error in errors {
                let Error::NotSupported(message) = error else {
                    panic!("expected a merge refusal, got {error:?}");
                };
                assert!(message.contains("Query::limit"), "{message}");
                assert!(message.contains("Query::offset"), "{message}");
            }
        }
    }
}

#[test]
fn aggregate_validators_reject_own_and_nested_count_unit_offsets() {
    use grovedb_query::{error::Error, QueryItem};
    type Validator = for<'a> fn(&'a Query) -> Result<&'a QueryItem, Error>;
    let families = [
        (
            Query::new_aggregate_count_on_range as fn(QueryItem) -> Query,
            Query::validate_aggregate_count_on_range as Validator,
            Query::validate_leaf_aggregate_count_on_range as Validator,
            Query::validate_carrier_aggregate_count_on_range as Validator,
        ),
        (
            Query::new_aggregate_sum_on_range as fn(QueryItem) -> Query,
            Query::validate_aggregate_sum_on_range as Validator,
            Query::validate_leaf_aggregate_sum_on_range as Validator,
            Query::validate_carrier_aggregate_sum_on_range as Validator,
        ),
        (
            Query::new_aggregate_count_and_sum_on_range as fn(QueryItem) -> Query,
            Query::validate_aggregate_count_and_sum_on_range as Validator,
            Query::validate_leaf_aggregate_count_and_sum_on_range as Validator,
            Query::validate_carrier_aggregate_count_and_sum_on_range as Validator,
        ),
    ];
    for (construct, dispatch, validate_leaf, validate_carrier) in families {
        let leaf = construct(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let mut carrier = Query::new_single_key(b"doc".to_vec());
        carrier.set_subquery(leaf.clone());
        assert!(dispatch(&leaf).is_ok());
        assert!(validate_leaf(&leaf).is_ok());
        assert!(dispatch(&carrier).is_ok());
        assert!(validate_carrier(&carrier).is_ok());
        for offset in [0, 7] {
            let mut leaf_own = leaf.clone();
            leaf_own.offset = Some(offset);
            let mut carrier_own = carrier.clone();
            carrier_own.offset = Some(offset);
            let mut carrier_nested = carrier.clone();
            carrier_nested.set_subquery(leaf_own.clone());
            for (query, direct) in [
                (leaf_own, validate_leaf),
                (carrier_own, validate_carrier),
                (carrier_nested, validate_carrier),
            ] {
                for validate in [dispatch, direct] {
                    let error =
                        validate(&query).expect_err("aggregates cannot ignore count-unit offsets");
                    assert!(error.to_string().contains("offset"), "{error}");
                }
            }
        }
    }
}

#[cfg(feature = "serde")]
#[test]
fn local_offset_serde_round_trips_with_a_distinct_outer_version() {
    for offset in [0u16, 6, u16::MAX] {
        for limit in [None, Some(2)] {
            let mut leaf = Query::new_range_full();
            leaf.offset = Some(offset);
            leaf.limit = limit;
            let mut root = Query::new_single_key(b"doc".to_vec());
            root.set_subquery(leaf);
            let json = serde_json::to_value(&root).unwrap();
            assert_eq!(json["version"], 1);
            assert_eq!(json["default_subquery_branch"]["subquery"]["version"], 4);
            assert_eq!(serde_json::from_value::<Query>(json).unwrap(), root);
            let bytes = bincode::serde::encode_to_vec(&root, standard()).unwrap();
            let (decoded, consumed): (Query, _) =
                bincode::serde::decode_from_slice(&bytes, standard()).unwrap();
            assert_eq!(consumed, bytes.len());
            assert_eq!(decoded, root);
        }
    }
}

#[cfg(feature = "serde")]
fn compact_frame_tokens(offset: Option<u16>) -> Vec<serde_test::Token> {
    use serde_test::Token;
    let mut tokens = vec![
        Token::Map {
            len: Some(if offset.is_some() { 10 } else { 9 }),
        },
        Token::Str("magic"),
        Token::U64(u64::from_be_bytes(*b"grvquery")),
        Token::Str("version"),
        Token::U8(if offset.is_some() { 4 } else { 1 }),
        Token::Str("items"),
        Token::Seq { len: Some(0) },
        Token::SeqEnd,
        Token::Str("default_subquery_branch"),
        Token::Struct {
            name: "SubqueryBranch",
            len: 2,
        },
        Token::Str("subquery_path"),
        Token::None,
        Token::Str("subquery"),
        Token::None,
        Token::StructEnd,
        Token::Str("conditional_subquery_branches"),
        Token::None,
        Token::Str("left_to_right"),
        Token::Bool(true),
        Token::Str("add_parent_tree_on_subquery"),
        Token::Bool(false),
        Token::Str("read_mode"),
        Token::None,
        Token::Str("limit"),
        Token::None,
    ];
    if let Some(offset) = offset {
        tokens.extend([Token::Str("offset"), Token::Some, Token::U16(offset)]);
    }
    tokens.push(Token::MapEnd);
    tokens
}

#[cfg(feature = "serde")]
#[test]
fn compact_map_frames_preserve_existing_queries_and_local_offsets() {
    use serde_test::{assert_de_tokens, Configure};
    let query = Query::new();
    let bytes = encode_to_vec(&query, standard()).unwrap();
    let frozen = decode_from_slice::<query_v3::Query, _>(&bytes, standard())
        .unwrap()
        .0;
    assert_de_tokens(&frozen.compact(), &compact_frame_tokens(None));
    for offset in [None, Some(0), Some(7)] {
        let mut query = Query::new();
        query.offset = offset;
        assert_de_tokens(&query.compact(), &compact_frame_tokens(offset));
    }
}

#[cfg(feature = "serde")]
#[test]
fn current_v3_serde_reader_rejects_top_level_and_nested_offsets() {
    for offset in [0, 7] {
        for limit in [None, Some(2)] {
            let mut leaf = Query::new_range_full();
            leaf.limit = limit;
            leaf.offset = Some(offset);
            let mut root = Query::new_single_key(b"doc".to_vec());
            root.set_subquery(leaf.clone());
            for query in [leaf, root] {
                let json = serde_json::to_string(&query).unwrap();
                let error = serde_json::from_str::<query_v3::Query>(&json).unwrap_err();
                assert!(error
                    .to_string()
                    .contains("non-canonical Query serde version 4"));
                let bytes = bincode::serde::encode_to_vec(&query, standard()).unwrap();
                let error =
                    bincode::serde::decode_from_slice::<query_v3::Query, _>(&bytes, standard())
                        .unwrap_err();
                assert!(error
                    .to_string()
                    .contains("non-canonical Query serde version 4"));
            }
        }
    }
}

#[cfg(feature = "serde")]
#[test]
fn offset_free_queries_keep_current_v3_wire_and_serde_bytes() {
    for read_mode in [false, true] {
        for limit in [None, Some(2)] {
            let mut leaf = Query::new_range_full();
            leaf.limit = limit;
            if read_mode {
                leaf.read_mode = Some(Box::new(grovedb_query::ReadMode::SumBudget(
                    grovedb_query::SumBudgetRead {
                        sum_limit: 100,
                        match_limit: Some(3),
                    },
                )));
            }
            let mut root = Query::new_single_key(b"doc".to_vec());
            root.set_subquery(leaf.clone());
            let mut conditional = root.clone();
            conditional.add_conditional_subquery(
                grovedb_query::QueryItem::Key(b"other".to_vec()),
                None,
                Some(leaf.clone()),
            );
            for query in [leaf, root, conditional] {
                let bytes = encode_to_vec(&query, standard()).unwrap();
                let (frozen, consumed) =
                    decode_from_slice::<query_v3::Query, _>(&bytes, standard()).unwrap();
                assert_eq!(consumed, bytes.len());
                assert_eq!(encode_to_vec(&frozen, standard()).unwrap(), bytes);
                if query.conditional_subquery_branches.is_none() {
                    assert_eq!(
                        serde_json::to_vec(&frozen).unwrap(),
                        serde_json::to_vec(&query).unwrap()
                    );
                    assert_eq!(
                        serde_json::from_slice::<Query>(&serde_json::to_vec(&frozen).unwrap())
                            .unwrap(),
                        query
                    );
                }
                let frozen_bytes = bincode::serde::encode_to_vec(&frozen, standard()).unwrap();
                assert_eq!(
                    bincode::serde::encode_to_vec(&query, standard()).unwrap(),
                    frozen_bytes
                );
                let (decoded, consumed): (Query, _) =
                    bincode::serde::decode_from_slice(&frozen_bytes, standard()).unwrap();
                assert_eq!(consumed, frozen_bytes.len());
                assert_eq!(decoded, query);
            }
        }
    }
}
