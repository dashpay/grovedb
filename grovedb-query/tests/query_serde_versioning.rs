//! Cross-generation safety of `Query`'s versioned serde representation.
//!
//! `ReleasedQuery` reproduces the **released** v5.0.1 derived layout —
//! the actual compatibility surface: five flat fields, no `read_mode`,
//! no `limit`, unknown keys silently ignored by its derive. The
//! contract under test, in both directions:
//!
//! - flat head payloads (version 1 — the only content a released
//!   reader can serve) keep flowing to released readers, and released
//!   payloads keep decoding at head (self-describing formats);
//! - head payloads carrying features a released reader cannot serve —
//!   a read mode or a limit — hard-fail there instead of silently
//!   decoding with the feature dropped;
//! - positional payloads never cross generations silently: the framed
//!   layout's magic decodes as an absurd `items` length under the
//!   released unframed layout (a bare leading version byte would have
//!   been consumed as a small length and reinterpreted), and the
//!   framed reader requires the magic exactly;
//! - head/head round-trips work in both format families, and
//!   non-canonical version claims are rejected.

#![cfg(feature = "serde")]

use bincode::config;
use grovedb_query::{Query, QueryItem, ReadMode, SubqueryBranch, SumBudgetRead};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// The released v5.0.1 derived serde layout, byte-for-byte as shipped.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ReleasedQuery {
    items: Vec<QueryItem>,
    default_subquery_branch: SubqueryBranch,
    conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
    left_to_right: bool,
    add_parent_tree_on_subquery: bool,
}

fn read_mode() -> Box<ReadMode> {
    Box::new(ReadMode::SumBudget(SumBudgetRead {
        sum_limit: 500,
        match_limit: Some(100),
    }))
}

fn head_query(with_read_mode: bool, limit: Option<u16>) -> Query {
    let mut inner = Query::new_single_key(b"leaf".to_vec());
    inner.limit = limit.map(|_| 2); // nested cap rides along when capped
    let mut query = Query::new_single_query_item(QueryItem::RangeFull(..));
    query.set_subquery(inner);
    if with_read_mode {
        query.read_mode = Some(read_mode());
    }
    query.limit = limit;
    query
}

fn released_query() -> ReleasedQuery {
    ReleasedQuery {
        items: vec![QueryItem::Key(b"k".to_vec())],
        default_subquery_branch: SubqueryBranch::default(),
        conditional_subquery_branches: None,
        left_to_right: true,
        add_parent_tree_on_subquery: false,
    }
}

#[test]
fn head_round_trips_every_combination_in_both_format_families() {
    for (with_read_mode, limit) in [
        (false, None),
        (true, None),
        (false, Some(3)),
        (true, Some(3)),
    ] {
        let query = head_query(with_read_mode, limit);

        let json = serde_json::to_string(&query).expect("json encode");
        let decoded: Query = serde_json::from_str(&json).expect("json decode");
        assert_eq!(decoded, query, "json round trip");

        let bytes =
            bincode::serde::encode_to_vec(&query, config::standard()).expect("positional encode");
        let (decoded, consumed): (Query, usize) =
            bincode::serde::decode_from_slice(&bytes, config::standard())
                .expect("positional decode");
        assert_eq!(consumed, bytes.len());
        assert_eq!(decoded, query, "positional round trip");
    }
}

#[test]
fn released_json_payloads_stay_readable_at_head() {
    let released = released_query();
    let json = serde_json::to_string(&released).expect("released json encode");
    let decoded: Query = serde_json::from_str(&json).expect("head must read released payloads");
    assert_eq!(decoded.items, released.items);
    assert_eq!(decoded.read_mode, None);
    assert_eq!(decoded.limit, None);
}

#[test]
fn plain_head_json_stays_readable_at_released() {
    // Version 1 keeps the flat layout (plus a `version` key the
    // released derive ignores), so every query a released reader can
    // serve keeps flowing to it.
    let query = head_query(false, None);
    let json = serde_json::to_string(&query).expect("head json encode");
    let decoded: ReleasedQuery =
        serde_json::from_str(&json).expect("released must read flat head payloads");
    assert_eq!(decoded.items, query.items);
}

#[test]
fn feature_bearing_head_json_fails_closed_at_released() {
    // Read-mode and limit-bearing forms nest under `body`: the
    // released derive — which silently ignores unknown keys in a flat
    // map — hard-fails on the missing flat fields instead of decoding
    // the query with the feature dropped.
    for (with_read_mode, limit) in [(true, None), (false, Some(3)), (true, Some(3))] {
        let query = head_query(with_read_mode, limit);
        let json = serde_json::to_string(&query).expect("head json encode");
        assert!(
            serde_json::from_str::<ReleasedQuery>(&json).is_err(),
            "a released reader must not silently decode a feature-bearing query"
        );
    }
}

#[test]
fn flat_json_carrying_features_is_refused_at_head() {
    // A flat map smuggling `read_mode` or `limit` would mean different
    // things to the two released surfaces (the released derive drops
    // both keys silently), so the head reader refuses it outright.
    let with_flat_read_mode = r#"{"items":[],"default_subquery_branch":{"subquery_path":null,"subquery":null},"conditional_subquery_branches":null,"left_to_right":true,"add_parent_tree_on_subquery":false,"read_mode":{"SumBudget":{"sum_limit":5,"match_limit":null}}}"#;
    assert!(serde_json::from_str::<Query>(with_flat_read_mode).is_err());

    let with_flat_limit = r#"{"items":[],"default_subquery_branch":{"subquery_path":null,"subquery":null},"conditional_subquery_branches":null,"left_to_right":true,"add_parent_tree_on_subquery":false,"limit":3}"#;
    assert!(serde_json::from_str::<Query>(with_flat_limit).is_err());
}

#[test]
fn positional_payloads_fail_cleanly_across_generations() {
    // Released positional payloads are unframed; the framed reader
    // requires the magic and errors cleanly.
    let released_bytes = bincode::serde::encode_to_vec(released_query(), config::standard())
        .expect("released positional encode");
    assert!(
        bincode::serde::decode_from_slice::<Query, _>(&released_bytes, config::standard()).is_err(),
        "head must fail closed on released positional payloads"
    );

    // Head positional payloads must not decode under the released
    // layout — not even by fully consuming the bytes as reinterpreted
    // fields. The leading magic reads there as an absurd `items`
    // length, which errors.
    for (with_read_mode, limit) in [
        (false, None),
        (true, None),
        (false, Some(3)),
        (true, Some(3)),
    ] {
        let head_bytes =
            bincode::serde::encode_to_vec(head_query(with_read_mode, limit), config::standard())
                .expect("head positional encode");
        assert!(
            bincode::serde::decode_from_slice::<ReleasedQuery, _>(&head_bytes, config::standard())
                .is_err(),
            "released must fail closed on head positional payloads"
        );
    }
}

#[test]
fn non_canonical_version_claims_are_rejected() {
    // Flat JSON claiming version 2 or 3 (features only travel nested).
    for version in [2u8, 3] {
        let flat = format!(
            r#"{{"version":{version},"items":[],"default_subquery_branch":{{"subquery_path":null,"subquery":null}},"conditional_subquery_branches":null,"left_to_right":true,"add_parent_tree_on_subquery":false}}"#
        );
        assert!(serde_json::from_str::<Query>(&flat).is_err());
    }

    // A nested body whose version claim does not match its contents.
    let query = head_query(true, None);
    let json = serde_json::to_string(&query).expect("encode");
    assert!(json.contains("\"version\":2"));
    let downgraded = json.replace("\"version\":2", "\"version\":1");
    assert!(serde_json::from_str::<Query>(&downgraded).is_err());
    let upgraded = json.replace("\"version\":2", "\"version\":3");
    assert!(serde_json::from_str::<Query>(&upgraded).is_err());

    // A framed positional payload with a downgraded version claim.
    let query = head_query(false, Some(3));
    let mut bytes =
        bincode::serde::encode_to_vec(&query, config::standard()).expect("positional encode");
    // The magic's varint encoding leads; the version byte follows it.
    let magic_len =
        bincode::serde::encode_to_vec(u64::from_be_bytes(*b"grvquery"), config::standard())
            .expect("encode magic")
            .len();
    assert_eq!(bytes[magic_len], 3);
    bytes[magic_len] = 1;
    assert!(
        bincode::serde::decode_from_slice::<Query, _>(&bytes, config::standard()).is_err(),
        "positional canonicality must hold too"
    );
}
