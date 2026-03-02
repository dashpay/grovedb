use bincode::{
    borrow_decode_from_slice, config::standard, decode_from_slice, encode_into_std_write,
    encode_to_vec,
};
use indexmap::IndexMap;

use grovedb_query::{Query, QueryItem, SubqueryBranch};

fn k(v: u8) -> Vec<u8> {
    vec![v]
}

#[test]
fn query_subquery_flags_and_conditional_first_match_work() {
    let mut q = Query::new();
    q.insert_key(k(1));
    assert!(!q.has_subquery());
    assert!(q.has_only_keys());

    q.set_subquery_key(k(9));
    assert!(q.has_subquery());
    assert!(q.has_subquery_or_subquery_path_on_key(&k(1), false));
    assert!(!q.has_subquery_on_key(&k(1), false));

    let mut nested = Query::new();
    nested.insert_key(k(7));
    q.set_subquery(nested);
    assert!(q.has_subquery_on_key(&k(1), false));

    let mut cond = Query::new();
    cond.insert_key(k(5));
    cond.add_conditional_subquery(QueryItem::Range(k(0)..k(10)), None, None);
    cond.add_conditional_subquery(
        QueryItem::RangeInclusive(k(4)..=k(6)),
        None,
        Some(Query::new_single_key(k(8))),
    );

    assert!(cond.has_subquery());
    // First matching conditional branch wins (the first has no subquery).
    assert!(!cond.has_subquery_on_key(&k(5), false));
    assert!(cond.has_subquery_or_subquery_path_on_key(&k(5), false));
    assert!(cond.has_subquery_on_key(&k(99), true));
}

#[test]
fn query_encode_decode_and_borrow_decode_round_trip() {
    let mut q = Query::new_single_key(k(1));
    q.left_to_right = false;
    q.add_parent_tree_on_subquery = true;
    q.set_subquery_path(vec![k(9), k(10)]);

    let mut branch_map = IndexMap::new();
    branch_map.insert(
        QueryItem::RangeInclusive(k(2)..=k(3)),
        SubqueryBranch {
            subquery_path: Some(vec![k(11)]),
            subquery: Some(Box::new(Query::new_single_key(k(12)))),
        },
    );
    q.conditional_subquery_branches = Some(branch_map);

    let encoded = encode_to_vec(&q, standard()).expect("encode query");

    let (decoded, consumed): (Query, usize) =
        decode_from_slice(&encoded, standard()).expect("decode query");
    assert_eq!(consumed, encoded.len());
    assert_eq!(decoded, q);

    let (borrow_decoded, borrow_consumed): (Query, usize) =
        borrow_decode_from_slice(&encoded, standard()).expect("borrow decode query");
    assert_eq!(borrow_consumed, encoded.len());
    assert_eq!(borrow_decoded, q);
}

#[test]
fn query_decode_rejects_unsupported_version() {
    let err = decode_from_slice::<Query, _>(&[2_u8], standard()).expect_err("must fail");
    assert!(err
        .to_string()
        .contains("unsupported Query encoding version"));
}

#[test]
fn query_decode_rejects_too_many_conditional_branches() {
    let mut bytes = Vec::new();
    let cfg = standard();

    encode_into_std_write(1_u8, &mut bytes, cfg).expect("encode version");
    encode_into_std_write(Vec::<QueryItem>::new(), &mut bytes, cfg).expect("encode items");
    encode_into_std_write(SubqueryBranch::default(), &mut bytes, cfg)
        .expect("encode default branch");
    encode_into_std_write(1_u8, &mut bytes, cfg).expect("encode conditional flag");
    encode_into_std_write(1025_u64, &mut bytes, cfg).expect("encode oversized len");
    encode_into_std_write(true, &mut bytes, cfg).expect("encode ltr");
    encode_into_std_write(false, &mut bytes, cfg).expect("encode add parent");

    let err = decode_from_slice::<Query, _>(&bytes, cfg).expect_err("must fail");
    assert!(err
        .to_string()
        .contains("conditional subquery branches length exceeds maximum"));
}
