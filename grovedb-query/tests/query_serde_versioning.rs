//! Cross-generation safety of `Query`'s versioned serde representation.
//!
//! `BaseQuery` below reproduces the pre-limit derived layout (the one
//! shipped before per-instance limits): flat fields through
//! `read_mode`, which carries `default` + `skip_serializing_if`. The
//! contract under test, in both directions:
//!
//! - base-writer payloads a base reader could serve still decode at
//!   head (self-describing formats), or fail *cleanly* (positional
//!   formats, which the base layout never round-tripped reliably);
//! - head-writer payloads carrying a limit — which a base reader
//!   cannot serve — hard-fail at base instead of silently decoding as
//!   an unlimited query;
//! - head/head round-trips work in both format families, and
//!   non-canonical version claims are rejected.

#![cfg(feature = "serde")]

use bincode::config;
use grovedb_query::{Query, QueryItem, ReadMode, SubqueryBranch, SumBudgetRead};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// The pre-limit derived serde layout, byte-for-byte as it shipped.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct BaseQuery {
    items: Vec<QueryItem>,
    default_subquery_branch: SubqueryBranch,
    conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
    left_to_right: bool,
    add_parent_tree_on_subquery: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    read_mode: Option<Box<ReadMode>>,
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
fn base_writer_payloads_stay_readable_at_head_in_json() {
    for with_read_mode in [false, true] {
        let base = BaseQuery {
            items: vec![QueryItem::Key(b"k".to_vec())],
            default_subquery_branch: SubqueryBranch::default(),
            conditional_subquery_branches: None,
            left_to_right: true,
            add_parent_tree_on_subquery: false,
            read_mode: with_read_mode.then(read_mode),
        };
        let json = serde_json::to_string(&base).expect("base json encode");
        let decoded: Query = serde_json::from_str(&json).expect("head must read base payloads");
        assert_eq!(decoded.items, base.items);
        assert_eq!(decoded.read_mode.is_some(), with_read_mode);
        assert_eq!(decoded.limit, None);
    }
}

#[test]
fn head_flat_json_without_a_limit_stays_readable_at_base() {
    // Versions 1 and 2 keep the flat layout (plus a `version` key the
    // base derive ignores), so every query a base reader can serve
    // keeps flowing to it.
    for with_read_mode in [false, true] {
        let query = head_query(with_read_mode, None);
        let json = serde_json::to_string(&query).expect("head json encode");
        let decoded: BaseQuery =
            serde_json::from_str(&json).expect("base must read flat head payloads");
        assert_eq!(decoded.items, query.items);
        assert_eq!(decoded.read_mode.is_some(), with_read_mode);
    }
}

#[test]
fn limited_head_json_fails_closed_at_base() {
    // The limit-bearing form nests under `body`, so a base reader —
    // which would silently drop an unknown `limit` key in a flat map —
    // hard-fails on the missing flat fields instead of decoding an
    // unlimited query.
    let query = head_query(false, Some(3));
    let json = serde_json::to_string(&query).expect("head json encode");
    assert!(
        serde_json::from_str::<BaseQuery>(&json).is_err(),
        "a base reader must not silently decode a limited query"
    );
}

#[test]
fn positional_payloads_fail_cleanly_across_generations() {
    // Positional base payloads (the read-mode-bearing shape was the
    // only one the base layout round-tripped) fail with a decode error
    // at head — never a silent misread.
    let base = BaseQuery {
        items: vec![QueryItem::Key(b"k".to_vec())],
        default_subquery_branch: SubqueryBranch::default(),
        conditional_subquery_branches: None,
        left_to_right: true,
        add_parent_tree_on_subquery: false,
        read_mode: Some(read_mode()),
    };
    let base_bytes =
        bincode::serde::encode_to_vec(&base, config::standard()).expect("base positional encode");
    assert!(
        bincode::serde::decode_from_slice::<Query, _>(&base_bytes, config::standard()).is_err(),
        "head must fail closed on base positional payloads"
    );

    // And head positional payloads fail at base likewise.
    let head_bytes = bincode::serde::encode_to_vec(head_query(false, Some(3)), config::standard())
        .expect("head positional encode");
    assert!(
        bincode::serde::decode_from_slice::<BaseQuery, _>(&head_bytes, config::standard()).is_err(),
        "base must fail closed on head positional payloads"
    );
}

#[test]
fn non_canonical_version_claims_are_rejected() {
    // Flat JSON claiming version 3 (limits only travel nested).
    let flat_v3 = r#"{"version":3,"items":[],"default_subquery_branch":{"subquery_path":null,"subquery":null},"conditional_subquery_branches":null,"left_to_right":true,"add_parent_tree_on_subquery":false}"#;
    assert!(serde_json::from_str::<Query>(flat_v3).is_err());

    // Flat JSON claiming version 1 while carrying a read mode.
    let query = head_query(true, None);
    let json = serde_json::to_string(&query).expect("encode");
    let json = json.replace("\"version\":2", "\"version\":1");
    assert!(serde_json::from_str::<Query>(&json).is_err());

    // An unknown flat key — the silent-drop vector — is refused.
    let unknown_key = r#"{"items":[],"default_subquery_branch":{"subquery_path":null,"subquery":null},"conditional_subquery_branches":null,"left_to_right":true,"add_parent_tree_on_subquery":false,"limit":3}"#;
    assert!(serde_json::from_str::<Query>(unknown_key).is_err());

    // A version-3 positional payload whose limit slot is None.
    let query = head_query(false, Some(3));
    let mut bytes =
        bincode::serde::encode_to_vec(&query, config::standard()).expect("positional encode");
    // version byte leads the positional layout; downgrade the claim.
    assert_eq!(bytes[0], 3);
    bytes[0] = 1;
    assert!(
        bincode::serde::decode_from_slice::<Query, _>(&bytes, config::standard()).is_err(),
        "positional canonicality must hold too"
    );
}
