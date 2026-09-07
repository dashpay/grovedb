//! Golden-byte pins for the `Query` bincode encoding.
//!
//! The `Query` encoding is a public compatibility surface: external
//! callers (rs-drive) round-trip serialized queries, and the verifier
//! interprets the same query the prover used, so the byte layout of
//! every already-expressible query must never change. These tests pin
//! the exact bytes of representative queries; if one fails, the
//! encoding changed for queries that predate the change — which is a
//! wire break, not a refactor.

use bincode::config;
use grovedb_query::{AxisQuery, IndexAxis, Query, QueryItem, ReadMode, SumBudgetRead};

fn encode(query: &Query) -> Vec<u8> {
    bincode::encode_to_vec(query, config::standard()).expect("query must encode")
}

fn decode(bytes: &[u8]) -> Query {
    let (query, consumed): (Query, usize) =
        bincode::decode_from_slice(bytes, config::standard()).expect("query must decode");
    assert_eq!(consumed, bytes.len(), "no trailing bytes");
    query
}

fn simple_query() -> Query {
    let mut query = Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
    query.insert_key(b"0".to_vec());
    query
}

fn nested_query() -> Query {
    let mut inner = Query::new_single_key(b"leaf".to_vec());
    inner.left_to_right = false;

    let mut query = Query::new_single_query_item(QueryItem::RangeFrom(b"m".to_vec()..));
    query.set_subquery_path(vec![b"sub".to_vec()]);
    query.set_subquery(inner);
    query.add_conditional_subquery(
        QueryItem::Key(b"cond".to_vec()),
        Some(vec![b"p".to_vec()]),
        Some(Query::new_single_key(b"ck".to_vec())),
    );
    query.add_parent_tree_on_subquery = true;
    query
}

#[test]
fn print_golden_bytes() {
    // Helper to (re)generate the literals below; keep it running so the
    // encode path stays exercised even when the pins are up to date.
    println!("simple: {:?}", encode(&simple_query()));
    println!("nested: {:?}", encode(&nested_query()));
}

#[test]
fn simple_query_bytes_are_pinned() {
    let bytes = encode(&simple_query());
    assert_eq!(bytes, GOLDEN_SIMPLE, "simple Query encoding changed");
    assert_eq!(decode(&bytes), simple_query());
}

#[test]
fn nested_query_bytes_are_pinned() {
    let bytes = encode(&nested_query());
    assert_eq!(bytes, GOLDEN_NESTED, "nested Query encoding changed");
    assert_eq!(decode(&bytes), nested_query());
}

#[test]
fn queries_without_read_mode_stay_on_version_1() {
    // The version byte is the first encoded byte; every query that was
    // expressible before read modes must keep encoding as version 1.
    assert_eq!(encode(&simple_query())[0], 1);
    assert_eq!(encode(&nested_query())[0], 1);
    assert_eq!(encode(&Query::new())[0], 1);
}

#[test]
fn read_mode_queries_use_version_2_and_round_trip() {
    let mut axis_query = Query::new();
    axis_query.read_mode = Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
        IndexAxis::Sum,
        10,
        5,
        true,
    ))));
    let bytes = encode(&axis_query);
    assert_eq!(bytes[0], 2, "read-mode queries encode as version 2");
    assert_eq!(decode(&bytes), axis_query);

    let mut budget_query = Query::new_single_query_item(QueryItem::RangeFull(..));
    budget_query.read_mode = Some(Box::new(ReadMode::SumBudget(SumBudgetRead {
        sum_limit: 500,
        match_limit: Some(100),
    })));
    let bytes = encode(&budget_query);
    assert_eq!(bytes[0], 2);
    assert_eq!(decode(&bytes), budget_query);

    // A nested query carrying a read mode in its terminal subquery: the
    // outer node stays version 1, the inner node is version 2, and the
    // whole payload round-trips.
    let mut outer = Query::new_single_key(b"branch".to_vec());
    outer.set_subquery_path(vec![b"suffix".to_vec()]);
    outer.set_subquery(axis_query);
    let bytes = encode(&outer);
    assert_eq!(
        bytes[0], 1,
        "outer node without a read mode stays version 1"
    );
    assert_eq!(decode(&bytes), outer);
}

#[test]
fn version_2_payload_without_read_mode_bytes_is_rejected() {
    // Take a version-1 encoding and flip the version byte to 2: the
    // decoder now expects read-mode bytes that aren't there.
    let mut bytes = encode(&simple_query());
    bytes[0] = 2;
    let result: Result<(Query, usize), _> = bincode::decode_from_slice(&bytes, config::standard());
    assert!(
        result.is_err(),
        "version-2 payload missing its read mode must be rejected"
    );
}

fn instance_limited_query() -> Query {
    let mut query = Query::new_single_query_item(QueryItem::RangeFull(..));
    query.limit = Some(3);
    query
}

fn nested_instance_limited_query() -> Query {
    // The common real shape: an unlimited outer selection whose subquery
    // caps each parent instance ("top 2 per parent").
    let mut inner = Query::new_range_full();
    inner.limit = Some(2);
    let mut outer = Query::new_single_query_item(QueryItem::RangeFull(..));
    outer.set_subquery(inner);
    outer
}

#[test]
fn print_golden_v3_bytes() {
    // Helper to (re)generate the version-3 literals below.
    println!("limited: {:?}", encode(&instance_limited_query()));
    println!(
        "nested_limited: {:?}",
        encode(&nested_instance_limited_query())
    );
}

#[test]
fn instance_limit_queries_use_version_3_and_round_trip() {
    let bytes = encode(&instance_limited_query());
    assert_eq!(bytes[0], 3, "instance-limited queries encode as version 3");
    assert_eq!(bytes[1], 0b10, "flags byte carries only the limit flag");
    assert_eq!(bytes, GOLDEN_LIMITED, "instance-limited encoding changed");
    assert_eq!(decode(&bytes), instance_limited_query());

    // A nested query carrying the limit only in its subquery: the outer
    // node stays version 1, the inner node is version 3, and the whole
    // payload round-trips.
    let outer = nested_instance_limited_query();
    let bytes = encode(&outer);
    assert_eq!(bytes[0], 1, "outer node without a limit stays version 1");
    assert_eq!(bytes, GOLDEN_NESTED_LIMITED);
    assert_eq!(decode(&bytes), outer);

    // Limit and read mode together: version 3 with both flags.
    let mut both = Query::new();
    both.limit = Some(7);
    both.read_mode = Some(Box::new(ReadMode::SumBudget(SumBudgetRead {
        sum_limit: 500,
        match_limit: Some(100),
    })));
    let bytes = encode(&both);
    assert_eq!(bytes[0], 3);
    assert_eq!(bytes[1], 0b11, "flags byte carries both flags");
    assert_eq!(decode(&bytes), both);
}

#[test]
fn version_3_flags_are_canonical_and_fail_closed() {
    // A version-3 node whose flags byte lacks the instance-limit flag is
    // non-canonical (it was expressible as version 1 or 2) — rejected.
    let mut bytes = encode(&instance_limited_query());
    bytes[1] = 0b00;
    assert!(
        bincode::decode_from_slice::<Query, _>(&bytes, config::standard()).is_err(),
        "version 3 without the limit flag must be rejected"
    );
    let mut bytes = encode(&instance_limited_query());
    bytes[1] = 0b01;
    assert!(
        bincode::decode_from_slice::<Query, _>(&bytes, config::standard()).is_err(),
        "version 3 with only the read-mode flag must be rejected"
    );
    // Unknown flag bits fail closed.
    let mut bytes = encode(&instance_limited_query());
    bytes[1] = 0b110;
    assert!(
        bincode::decode_from_slice::<Query, _>(&bytes, config::standard()).is_err(),
        "unknown version 3 flag bits must be rejected"
    );
}

#[test]
fn version_3_payload_missing_limit_bytes_is_rejected() {
    // Drop the trailing limit bytes: the decoder now expects a u16 that
    // isn't there.
    let full = encode(&instance_limited_query());
    let truncated = &full[..full.len() - 1];
    assert!(
        bincode::decode_from_slice::<Query, _>(truncated, config::standard()).is_err(),
        "version-3 payload missing its limit must be rejected"
    );
}

// Captured from the encoding as of develop @ a2791bbd (pre-read_mode).
const GOLDEN_SIMPLE: &[u8] = &[1, 2, 0, 1, 48, 1, 1, 97, 1, 122, 0, 0, 0, 1, 0];
const GOLDEN_NESTED: &[u8] = &[
    1, 1, 4, 1, 109, 1, 1, 3, 115, 117, 98, 1, 1, 1, 0, 4, 108, 101, 97, 102, 0, 0, 0, 0, 0, 1, 1,
    0, 4, 99, 111, 110, 100, 1, 1, 1, 112, 1, 1, 1, 0, 2, 99, 107, 0, 0, 0, 1, 0, 1, 1,
];
// Version-3 pins, captured at introduction of the per-instance limit.
const GOLDEN_LIMITED: &[u8] = &[3, 2, 1, 3, 0, 0, 0, 1, 0, 3];
const GOLDEN_NESTED_LIMITED: &[u8] = &[1, 1, 3, 0, 1, 3, 2, 1, 3, 0, 0, 0, 1, 0, 2, 0, 1, 0];
