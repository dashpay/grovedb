//! A V1 proof layer that has subquery branches under a per-instance cap
//! (`Query::limit`), issue #962.
//!
//! The cap budgets the rows below the layer's keys, so through `GROVE_V4`
//! the layer is proven under the global budget only: with no global limit,
//! the normal case for a merged page, it carries every key of its range.
//! From `GROVE_V5` (`proof.instance_cap_layer_truncation`) the layer stops
//! after the key whose rows spent the budget, and the verifier accepts the
//! early stop only once it has itself seen the budget spent.
//!
//! The fixture is an ordered index, `index/<key>/doc`, the shape of a
//! document feed page: each key of the index level is a subtree holding the
//! page's rows. Some tests leave subtrees empty, which spend no instance
//! budget.

use grovedb_merk::proofs::{Decoder, Node, Op};
use grovedb_version::version::{v4::GROVE_V4, v5::GROVE_V5, GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp,
    operations::proof::{GroveDBProof, LayerProof, ProofBytes},
    query_result_type::QueryResultType,
    reference_path::ReferencePathType,
    tests::{make_test_grovedb, TempGroveDb},
    Element, Error, GroveDb, PathQuery, Query, QueryItem, SizedQuery,
};

const INDEX: &[u8] = b"index";
const OTHER: &[u8] = b"other";
const DOC: &[u8] = b"doc";
const PAGE: u16 = 5;

/// The index key of position `i`: two big-endian bytes, so keys order as
/// their positions do.
fn index_key(i: u16) -> Vec<u8> {
    i.to_be_bytes().to_vec()
}

fn doc_key(doc: u8) -> Vec<u8> {
    [DOC, &[doc][..]].concat()
}

fn insert(db: &TempGroveDb, path: &[&[u8]], key: &[u8], element: Element, v: &GroveVersion) {
    db.insert(path, key, element, None, None, v)
        .unwrap()
        .expect("insert should succeed");
}

/// `index/<0..n>` subtrees, each holding `docs_under(i)` items, plus the
/// disjoint `other/x` item a page is merged with.
fn feed(n: u16, docs_under: impl Fn(u16) -> u8, v: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(v);
    insert(&db, &[], INDEX, Element::empty_tree(), v);
    insert(&db, &[], OTHER, Element::empty_tree(), v);
    insert(&db, &[OTHER], b"x", Element::new_item(vec![9]), v);
    for i in 0..n {
        let key = index_key(i);
        insert(&db, &[INDEX], &key, Element::empty_tree(), v);
        for doc in 0..docs_under(i) {
            insert(
                &db,
                &[INDEX, key.as_slice()],
                &doc_key(doc),
                Element::new_item(vec![doc]),
                v,
            );
        }
    }
    db
}

fn range_full(left_to_right: bool) -> Query {
    let mut query = Query::new_with_direction(left_to_right);
    query.insert_all();
    query
}

/// The page: every index key in order, every row below each, at most `cap`
/// rows. The cap sits on the query node as a per-instance cap, the form a
/// merge carries it in.
fn page(cap: u16, left_to_right: bool) -> PathQuery {
    let mut body = range_full(left_to_right);
    body.set_subquery(range_full(left_to_right));
    body.limit = Some(cap);
    PathQuery::new_unsized(vec![INDEX.to_vec()], body)
}

/// The page merged with a disjoint branch that has no limit.
fn merged_page(cap: u16, left_to_right: bool, v: &GroveVersion) -> PathQuery {
    let mut other = Query::new_with_direction(left_to_right);
    other.insert_key(b"x".to_vec());
    let other = PathQuery::new_unsized(vec![OTHER.to_vec()], other);
    PathQuery::merge(vec![&page(cap, left_to_right), &other], v).expect("the inputs merge")
}

type Rows = Vec<(Vec<Vec<u8>>, Vec<u8>)>;

fn trusted_rows(db: &TempGroveDb, path_query: &PathQuery, v: &GroveVersion) -> Rows {
    let (elements, _) = db
        .query_raw(
            path_query,
            true,
            true,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            v,
        )
        .unwrap()
        .expect("the trusted read should succeed");
    elements
        .to_path_key_elements()
        .into_iter()
        .map(|(path, key, _)| (path, key))
        .collect()
}

fn verified_rows(proof: &[u8], path_query: &PathQuery, v: &GroveVersion) -> Result<Rows, Error> {
    GroveDb::verify_query(proof, path_query, v).map(|(_, rows)| {
        rows.into_iter()
            .map(|(path, key, element)| {
                assert!(element.is_some(), "every proved row carries its element");
                (path, key)
            })
            .collect()
    })
}

/// Proves `path_query`, verifies the proof against the database root and
/// checks its rows against the trusted read. Returns the proof and the rows.
fn prove_and_verify(db: &TempGroveDb, path_query: &PathQuery, v: &GroveVersion) -> (Vec<u8>, Rows) {
    let proof = db
        .prove_query(path_query, None, v)
        .unwrap()
        .expect("proving should succeed");
    let (root, _) = GroveDb::verify_query(&proof, path_query, v).expect("the proof verifies");
    assert_eq!(root, db.root_hash(None, v).unwrap().unwrap());
    let rows = verified_rows(&proof, path_query, v).expect("the proof verifies");
    assert_eq!(
        rows,
        trusted_rows(db, path_query, v),
        "proved rows must be the trusted read's"
    );
    (proof, rows)
}

fn proof_config() -> impl bincode::config::Config {
    bincode::config::standard()
        .with_big_endian()
        .with_no_limit()
}

fn decode(proof: &[u8]) -> GroveDBProof {
    bincode::decode_from_slice(proof, proof_config())
        .expect("the proof decodes")
        .0
}

fn encode(proof: &GroveDBProof) -> Vec<u8> {
    bincode::encode_to_vec(proof, proof_config()).expect("the proof encodes")
}

fn index_layer(proof: &mut GroveDBProof) -> &mut LayerProof {
    let GroveDBProof::V1(proof) = proof else {
        panic!("expected a V1 proof envelope");
    };
    proof
        .root_layer
        .lower_layers
        .get_mut(INDEX)
        .expect("the proof descends into the index")
}

/// The nodes the index layer's Merk proof pushes, in order.
fn index_layer_nodes(proof: &[u8]) -> Vec<Node> {
    let mut proof = decode(proof);
    let ProofBytes::Merk(bytes) = &index_layer(&mut proof).merk_proof else {
        panic!("the index layer is a Merk proof");
    };
    Decoder::new(bytes)
        .filter_map(|op| match op.expect("the op decodes") {
            Op::Push(node) | Op::PushInverted(node) => Some(node),
            _ => None,
        })
        .collect()
}

/// The keys the index layer reveals together with a value, in the order the
/// proof pushes them.
fn index_layer_rows(proof: &[u8]) -> Vec<Vec<u8>> {
    index_layer_nodes(proof)
        .iter()
        .filter_map(|node| node.key_and_value().map(|(key, _)| key.to_vec()))
        .collect()
}

/// Every key the index layer reveals: its rows, and the range bounds that
/// prove a queried key absent.
fn index_layer_keys(proof: &[u8]) -> Vec<Vec<u8>> {
    index_layer_nodes(proof)
        .iter()
        .filter_map(|node| node.key().map(<[u8]>::to_vec))
        .collect()
}

fn index_keys(positions: impl IntoIterator<Item = u16>) -> Vec<Vec<u8>> {
    positions.into_iter().map(index_key).collect()
}

fn doc_rows(positions: impl IntoIterator<Item = u16>) -> Rows {
    positions
        .into_iter()
        .map(|i| (vec![INDEX.to_vec(), index_key(i)], doc_key(0)))
        .collect()
}

fn assert_stops_before_its_budget(result: Result<Rows, Error>) {
    match result {
        Err(Error::InvalidProof(_, message)) => assert!(
            message.contains("stops before the limit is spent"),
            "unexpected refusal: {message}"
        ),
        other => panic!("a layer that stops with budget left must be refused, got {other:?}"),
    }
}

/// The issue's regression: an ordered range page merged with a disjoint
/// limit-free branch. The proof returns the page and its size does not
/// follow the number of keys past the page.
#[test]
fn merged_page_proof_does_not_grow_with_the_range() {
    let v = &GROVE_V5;
    let mut sizes = vec![];
    for n in [10u16, 200] {
        let db = feed(n, |_| 1, v);
        let (proof, rows) = prove_and_verify(&db, &merged_page(PAGE, true, v), v);

        let mut expected = doc_rows(0..PAGE);
        expected.push((vec![OTHER.to_vec()], b"x".to_vec()));
        assert_eq!(rows, expected, "the first {PAGE} rows and the other branch");
        assert_eq!(
            index_layer_rows(&proof),
            index_keys(0..PAGE),
            "the index layer reveals the keys of the page and nothing after"
        );
        sizes.push(proof.len());

        // The page alone reads its rows out of the merged proof, the way a
        // client reads one component before it verifies the whole.
        let (_, page_rows) = GroveDb::verify_subset_query(&proof, &page(PAGE, true), v)
            .expect("the page verifies as a subset of the merged proof");
        assert_eq!(page_rows.len(), PAGE as usize);
        // A larger page is not a subset of it: the layer hides the keys the
        // extra row would come from.
        assert!(GroveDb::verify_subset_query(&proof, &page(PAGE + 1, true), v).is_err());
    }
    // The deeper tree adds a few hashes on the way down to the page, about
    // 70 bytes a level.
    assert!(
        sizes[1] <= sizes[0] + 512,
        "200 keys prove in {} bytes, 10 keys in {}",
        sizes[1],
        sizes[0]
    );
}

/// The same page under `GROVE_V4`, where the layer is proven in full: the
/// proof carries every key of the range, as GroveDB 6.1.1 emits it.
#[test]
fn v4_proves_the_layer_in_full() {
    let n = 200;
    let v4_db = feed(n, |_| 1, &GROVE_V4);
    let query = merged_page(PAGE, true, &GROVE_V4);
    let (v4_proof, rows) = prove_and_verify(&v4_db, &query, &GROVE_V4);
    assert_eq!(rows.len(), PAGE as usize + 1);
    assert_eq!(index_layer_rows(&v4_proof), index_keys(0..n));

    // A global limit on the page is lifted by the V4 merge onto the same
    // cap, so the issue's own shape is this query.
    let mut sized = page(PAGE, true);
    sized.query.query.limit = None;
    sized.query.limit = Some(PAGE);
    let mut other = Query::new();
    other.insert_key(b"x".to_vec());
    let other = PathQuery::new_unsized(vec![OTHER.to_vec()], other);
    assert_eq!(
        PathQuery::merge(vec![&sized, &other], &GROVE_V4).expect("the limit lifts"),
        query
    );

    // Pinned to the bytes the prover emitted for this fixture before the
    // truncation existed: this test passes with the same constants against
    // the prover and verifier that preceded it.
    assert_eq!(v4_proof.len(), V4_PROOF_LEN);
    assert_eq!(
        hex::encode(blake3::hash(&v4_proof).as_bytes()),
        V4_PROOF_DIGEST
    );
    // And to the work it took: a V4 prover that walked the layer a second
    // time and threw the result away would emit the same bytes.
    let cost = v4_db.prove_query(&query, None, &GROVE_V4).cost;
    assert_eq!(
        (
            cost.seek_count,
            cost.storage_loaded_bytes,
            cost.hash_node_calls
        ),
        V4_PROOF_COST
    );
}

/// The two versions side by side on the same data: the same rows, and a
/// V5 proof that is a fraction of the V4 one.
#[test]
fn v5_proof_is_a_fraction_of_the_v4_proof() {
    let n = 200;
    let v4_db = feed(n, |_| 1, &GROVE_V4);
    let (v4_proof, v4_rows) =
        prove_and_verify(&v4_db, &merged_page(PAGE, true, &GROVE_V4), &GROVE_V4);
    let v5_db = feed(n, |_| 1, &GROVE_V5);
    let (v5_proof, v5_rows) =
        prove_and_verify(&v5_db, &merged_page(PAGE, true, &GROVE_V5), &GROVE_V5);
    assert_eq!(v4_rows, v5_rows);
    assert!(
        v5_proof.len() * 8 < v4_proof.len(),
        "V5 proves the page in {} bytes, V4 in {}",
        v5_proof.len(),
        v4_proof.len()
    );
}

const V4_PROOF_LEN: usize = 9684;
/// `(seek_count, storage_loaded_bytes, hash_node_calls)` of proving it.
const V4_PROOF_COST: (u32, u64, u32) = (217, 23878, 12);
const V4_PROOF_DIGEST: &str = "dc5ccb80920c7fefef137472c102caa52021b7f61f233db841241c00fe48e732";

/// A descending page takes the last keys of the range and stops below them.
#[test]
fn descending_page_stops_below_its_last_row() {
    let v = &GROVE_V5;
    let n = 200;
    let db = feed(n, |_| 1, v);
    let (proof, rows) = prove_and_verify(&db, &merged_page(PAGE, false, v), v);

    let last = (n - PAGE..n).rev();
    let mut expected = vec![(vec![OTHER.to_vec()], b"x".to_vec())];
    expected.extend(doc_rows(last.clone()));
    assert_eq!(rows, expected);
    assert_eq!(index_layer_rows(&proof), index_keys(last));

    let small = feed(10, |_| 1, v);
    let (small_proof, _) = prove_and_verify(&small, &merged_page(PAGE, false, v), v);
    assert!(proof.len() <= small_proof.len() + 512);
}

/// Empty subtrees spend no instance budget, so the walk passes them: the
/// layer reveals every key up to the one that spends the budget, the empty
/// ones included, and hides the rest whatever they hold.
#[test]
fn empty_children_are_walked_until_the_budget_is_spent() {
    let v = &GROVE_V5;
    // Two empty subtrees, then a populated one, repeated.
    let db = feed(60, |i| u8::from(i % 3 == 2), v);

    let (proof, rows) = prove_and_verify(&db, &merged_page(PAGE, true, v), v);
    let mut expected = doc_rows([2, 5, 8, 11, 14]);
    expected.push((vec![OTHER.to_vec()], b"x".to_vec()));
    assert_eq!(rows, expected);
    assert_eq!(index_layer_rows(&proof), index_keys(0..=14));

    // Descending, the walk starts on a populated key (59).
    let (proof, rows) = prove_and_verify(&db, &merged_page(PAGE, false, v), v);
    assert_eq!(rows[1..], doc_rows([59, 56, 53, 50, 47]));
    assert_eq!(index_layer_rows(&proof), index_keys((47..60).rev()));

    // A page the data cannot fill leaves the budget unspent, so no key may
    // be hidden: the layer is whole.
    let sparse = feed(40, |i| u8::from([1, 7, 30].contains(&i)), v);
    let (proof, rows) = prove_and_verify(&sparse, &merged_page(PAGE, true, v), v);
    assert_eq!(rows[..3], doc_rows([1, 7, 30]));
    assert_eq!(index_layer_rows(&proof), index_keys(0..40));

    // The budget is spent on the last key of the range: nothing to hide.
    let exact = feed(8, |i| u8::from(i >= 3), v);
    let (proof, rows) = prove_and_verify(&exact, &merged_page(PAGE, true, v), v);
    assert_eq!(rows[..5], doc_rows(3..8));
    assert_eq!(index_layer_rows(&proof), index_keys(0..8));
}

/// A subtree with more rows than the budget has left spends it in the
/// middle of its own layer; the index layer stops right after that subtree.
#[test]
fn a_subtree_larger_than_the_budget_ends_the_layer() {
    let v = &GROVE_V5;
    let db = feed(50, |_| 3, v);
    let (proof, rows) = prove_and_verify(&db, &merged_page(7, true, v), v);
    assert_eq!(rows.len(), 7 + 1);
    // 3 + 3 + 1 rows: the third subtree is cut inside.
    assert_eq!(index_layer_rows(&proof), index_keys(0..3));
}

/// A layer whose keys are answered in different ways: references and
/// subtrees that are rows themselves, and subtrees a conditional branch
/// descends into. The nodes the walk rewrote for the rows it keeps
/// (resolved references, child hashes) are carried into the truncated layer.
#[test]
fn conditional_branches_and_rewritten_rows_survive_the_cut() {
    let v = &GROVE_V5;
    let db = make_test_grovedb(v);
    insert(&db, &[], INDEX, Element::empty_tree(), v);
    insert(&db, &[], OTHER, Element::empty_tree(), v);
    insert(&db, &[OTHER], b"x", Element::new_item(vec![9]), v);
    for i in 0..48u16 {
        let key = index_key(i);
        if i < 4 {
            // A reference: a row, proven as its resolved target.
            let target =
                ReferencePathType::AbsolutePathReference(vec![OTHER.to_vec(), b"x".to_vec()]);
            insert(&db, &[INDEX], &key, Element::new_reference(target), v);
            continue;
        }
        // 4..16 are rows themselves (no branch selects them), 16.. are
        // descended into.
        insert(&db, &[INDEX], &key, Element::empty_tree(), v);
        for doc in 0..2u8 {
            insert(
                &db,
                &[INDEX, key.as_slice()],
                &doc_key(doc),
                Element::new_item(vec![doc]),
                v,
            );
        }
    }

    let query_with_cap = |cap: u16| {
        let mut body = range_full(true);
        body.add_conditional_subquery(
            QueryItem::RangeFrom(index_key(16)..),
            None,
            Some(range_full(true)),
        );
        body.limit = Some(cap);
        PathQuery::new_unsized(vec![INDEX.to_vec()], body)
    };

    for (cap, last_key) in [
        // Within the references.
        (3u16, 2u16),
        // Past them, into the subtrees reported as rows.
        (9, 8),
        // The 16 rows, then two descents of two rows each.
        (20, 17),
        // One row more: the next descent is cut inside.
        (21, 18),
    ] {
        let (proof, rows) = prove_and_verify(&db, &query_with_cap(cap), v);
        assert_eq!(rows.len(), cap as usize, "cap {cap}");
        assert_eq!(
            index_layer_rows(&proof),
            index_keys(0..=last_key),
            "cap {cap}"
        );
    }
}

/// With a global limit as well, the layer is first bounded by it, as in
/// every version, and then stops where either budget is spent.
#[test]
fn a_global_limit_composes_with_the_cut() {
    let v = &GROVE_V5;
    let sized = |cap: u16, global: u16| {
        let mut query = page(cap, true);
        query.query.limit = Some(global);
        query
    };

    let db = feed(200, |_| 1, v);
    // The global limit is the tighter one: it truncates the layer itself.
    let (proof, rows) = prove_and_verify(&db, &sized(PAGE, 3), v);
    assert_eq!(rows, doc_rows(0..3));
    assert_eq!(index_layer_rows(&proof), index_keys(0..3));
    // The cap is the tighter one: the layer stops at the page, not at the
    // 50 keys the global limit allows.
    let (proof, rows) = prove_and_verify(&db, &sized(PAGE, 50), v);
    assert_eq!(rows, doc_rows(0..PAGE));
    assert_eq!(index_layer_rows(&proof), index_keys(0..PAGE));

    // Empty subtrees are charged to the global limit and not to the cap.
    let gappy = feed(60, |i| u8::from(i % 3 == 2), v);
    let (proof, rows) = prove_and_verify(&gappy, &sized(PAGE, 8), v);
    assert_eq!(rows, doc_rows([2, 5]));
    assert_eq!(index_layer_rows(&proof), index_keys(0..8));
    let (proof, rows) = prove_and_verify(&gappy, &sized(PAGE, 40), v);
    assert_eq!(rows, doc_rows([2, 5, 8, 11, 14]));
    assert_eq!(index_layer_rows(&proof), index_keys(0..=14));
}

/// A proof built for a smaller page hides keys the larger page still owes
/// rows from. Verified against the larger page it reconstructs the same
/// root, so only the budget check stands between it and a short result.
#[test]
fn a_layer_cut_for_a_smaller_page_is_refused() {
    let v = &GROVE_V5;
    let db = feed(200, |_| 1, v);
    for left_to_right in [true, false] {
        let (short_proof, _) = prove_and_verify(&db, &merged_page(3, left_to_right, v), v);
        assert_stops_before_its_budget(verified_rows(
            &short_proof,
            &merged_page(PAGE, left_to_right, v),
            v,
        ));
    }
}

/// The same forgery made by hand: the honest proof of the page with its
/// index layer replaced by one that stops two keys early, and the lower
/// layers of the hidden keys dropped.
#[test]
fn a_hand_abridged_layer_is_refused() {
    let v = &GROVE_V5;
    let db = feed(200, |_| 1, v);
    let query = merged_page(PAGE, true, v);
    let (honest, _) = prove_and_verify(&db, &query, v);
    let (short, _) = prove_and_verify(&db, &merged_page(3, true, v), v);

    let mut forged = decode(&honest);
    let mut short = decode(&short);
    let layer = index_layer(&mut forged);
    layer.merk_proof = index_layer(&mut short).merk_proof.clone();
    for hidden in [3, 4] {
        assert!(layer.lower_layers.remove(&index_key(hidden)).is_some());
    }
    let forged = encode(&forged);

    assert_eq!(index_layer_rows(&forged), index_keys(0..3));
    assert_stops_before_its_budget(verified_rows(&forged, &query, v));
    // The forgery is well formed: it is the honest proof of the 3-row page.
    assert_eq!(
        verified_rows(&forged, &merged_page(3, true, v), v).expect("verifies as the 3-row page"),
        trusted_rows(&db, &merged_page(3, true, v), v)
    );

    // Keeping the lower layers of the hidden keys does not help either.
    let mut with_layers = decode(&honest);
    index_layer(&mut with_layers).merk_proof = index_layer(&mut short).merk_proof.clone();
    assert!(verified_rows(&encode(&with_layers), &query, v).is_err());
}

/// A cut placed right after an empty subtree, with budget left. The layer
/// is one a global limit truncated honestly; read under the cap alone it
/// would hide the populated key that follows.
#[test]
fn a_cut_after_an_empty_child_with_budget_left_is_refused() {
    let v = &GROVE_V5;
    let db = feed(60, |i| u8::from(i % 3 == 2), v);

    // 13 charges: 4 rows (keys 2, 5, 8, 11) and 9 empty subtrees, the last
    // of them key 12.
    let mut globally_limited = page(PAGE, true);
    globally_limited.query.limit = Some(13);
    let (proof, rows) = prove_and_verify(&db, &globally_limited, v);
    assert_eq!(rows, doc_rows([2, 5, 8, 11]));
    assert_eq!(index_layer_rows(&proof), index_keys(0..=12));

    // Under the cap alone the fifth row (key 14) is owed.
    assert_eq!(
        trusted_rows(&db, &page(PAGE, true), v),
        doc_rows([2, 5, 8, 11, 14])
    );
    assert_stops_before_its_budget(verified_rows(&proof, &page(PAGE, true), v));
    // And under a wider global limit too.
    let mut wider = page(PAGE, true);
    wider.query.limit = Some(40);
    assert_stops_before_its_budget(verified_rows(&proof, &wider, v));
}

/// A `GROVE_V4` verifier refuses the truncated layer, and a `GROVE_V5`
/// verifier still accepts a layer proven in full: one that stops later than
/// it must hides nothing.
#[test]
fn the_two_versions_read_each_others_layers() {
    let db = feed(50, |_| 1, &GROVE_V5);
    let query = page(PAGE, true);

    let v5_proof = db
        .prove_query(&query, None, &GROVE_V5)
        .unwrap()
        .expect("V5 proves");
    let v4_proof = db
        .prove_query(&query, None, &GROVE_V4)
        .unwrap()
        .expect("V4 proves");
    assert_eq!(index_layer_rows(&v5_proof), index_keys(0..PAGE));
    assert_eq!(index_layer_rows(&v4_proof), index_keys(0..50));

    let refusal = verified_rows(&v5_proof, &query, &GROVE_V4);
    assert!(
        matches!(&refusal, Err(Error::InvalidProof(_, message)) if message.contains("missing data")),
        "a V4 verifier must refuse the truncated layer, got {refusal:?}"
    );
    assert_eq!(
        verified_rows(&v4_proof, &query, &GROVE_V5).expect("V5 reads a whole layer"),
        doc_rows(0..PAGE)
    );
}

/// An unknown value of the slot is a version error on both sides, never a
/// silent choice of one behaviour.
#[test]
fn an_unknown_slot_value_is_a_version_error() {
    let db = feed(10, |_| 1, &GROVE_V5);
    let query = page(PAGE, true);
    let proof = db
        .prove_query(&query, None, &GROVE_V5)
        .unwrap()
        .expect("V5 proves");

    let mut unknown = GROVE_V5.clone();
    unknown
        .grovedb_versions
        .operations
        .proof
        .instance_cap_layer_truncation = 2;
    assert!(matches!(
        db.prove_query(&query, None, &unknown).unwrap(),
        Err(Error::VersionError(_))
    ));
    assert!(matches!(
        GroveDb::verify_query(&proof, &query, &unknown),
        Err(Error::VersionError(_))
    ));
}

/// A query without a per-instance cap is untouched: its proof is the same
/// bytes under both versions.
#[test]
fn queries_without_a_cap_prove_the_same_bytes() {
    let db = feed(30, |i| u8::from(i % 2 == 0), &GROVE_V5);
    let mut uncapped = page(PAGE, true);
    uncapped.query.query.limit = None;
    let mut globally_limited = uncapped.clone();
    globally_limited.query = SizedQuery::new(globally_limited.query.query, Some(PAGE), None);

    for query in [uncapped, globally_limited] {
        let v4 = db.prove_query(&query, None, &GROVE_V4).unwrap().unwrap();
        let v5 = db.prove_query(&query, None, &GROVE_V5).unwrap().unwrap();
        assert_eq!(v4, v5);
    }
}

/// `index/<keys>` empty subtrees written in one batch.
fn insert_empty_subtrees(db: &TempGroveDb, keys: impl Iterator<Item = Vec<u8>>, v: &GroveVersion) {
    let ops = keys
        .map(|key| {
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![INDEX.to_vec()],
                key,
                Element::empty_tree(),
            )
        })
        .collect();
    db.apply_batch(ops, None, None, v)
        .unwrap()
        .expect("the batch applies");
}

/// Merk refuses a layer proof of more than 50,000 ops, about 25,000 keys.
/// A layer proven in full therefore stops verifying at all once the range
/// is that long, however small the page. The truncated layer does not.
#[test]
fn a_page_over_a_range_too_long_to_prove_in_full_verifies() {
    let db = feed(PAGE, |_| 1, &GROVE_V5);
    insert_empty_subtrees(&db, (PAGE..26_000).map(index_key), &GROVE_V5);
    let query = page(PAGE, true);

    let v4_proof = db
        .prove_query(&query, None, &GROVE_V4)
        .unwrap()
        .expect("V4 proves");
    let refusal = verified_rows(&v4_proof, &query, &GROVE_V4);
    assert!(
        matches!(&refusal, Err(Error::InvalidProof(_, message)) if message.contains("maximum operation count")),
        "the V4 proof carries the whole layer, got {refusal:?}"
    );

    let (v5_proof, rows) = prove_and_verify(&db, &query, &GROVE_V5);
    assert_eq!(rows, doc_rows(0..PAGE));
    assert_eq!(index_layer_rows(&v5_proof), index_keys(0..PAGE));
    assert!(v5_proof.len() < 2_000, "{} bytes", v5_proof.len());
}

/// A Merk limit is a `u16`. A layer that needs more rows than that before
/// its budget is spent cannot be cut at a limit, so the prover leaves it
/// whole, as V4 does. Such a layer is past Merk's op count either way.
#[test]
fn a_layer_longer_than_a_merk_limit_stays_whole() {
    let v = &GROVE_V5;
    let db = make_test_grovedb(v);
    insert(&db, &[], INDEX, Element::empty_tree(), v);

    // 65,536 empty subtrees, then the one populated subtree, then one more
    // key for the layer to hide if it could.
    let populated = [0xff, 0xff, 0x00].to_vec();
    let last = [0xff, 0xff, 0x01].to_vec();
    insert_empty_subtrees(
        &db,
        (0..=u16::MAX)
            .map(index_key)
            .chain([populated.clone(), last]),
        v,
    );
    insert(
        &db,
        &[INDEX, populated.as_slice()],
        &doc_key(0),
        Element::new_item(vec![0]),
        v,
    );

    let query = page(1, true);
    let proof = db
        .prove_query(&query, None, v)
        .unwrap()
        .expect("proving should succeed");
    assert_eq!(index_layer_rows(&proof).len(), usize::from(u16::MAX) + 3);
    let refusal = verified_rows(&proof, &query, v);
    assert!(
        matches!(&refusal, Err(Error::InvalidProof(_, message)) if message.contains("maximum operation count")),
        "a layer of this length is refused by Merk's op count, got {refusal:?}"
    );
}

/// The prover charges an empty plain `Tree` that is itself a result row; a
/// verifier that leaves empty trees out of its result (the default) does
/// not. After such a row the two budgets differ, and a layer cut by the
/// prover's count would look to the verifier like one that stops with
/// budget left. The prover therefore leaves the layer whole, and the proof
/// verifies to the same rows as under V4.
#[test]
fn an_empty_tree_row_keeps_the_layer_whole() {
    // The empty tree is a row below the capped layer: `index/a/t`.
    let db = make_test_grovedb(&GROVE_V5);
    insert(&db, &[], INDEX, Element::empty_tree(), &GROVE_V5);
    for key in [b"a", b"b", b"c"] {
        insert(&db, &[INDEX], key, Element::empty_tree(), &GROVE_V5);
    }
    insert(
        &db,
        &[INDEX, b"a"],
        b"d",
        Element::new_item(vec![1]),
        &GROVE_V5,
    );
    insert(&db, &[INDEX, b"a"], b"t", Element::empty_tree(), &GROVE_V5);
    let query = page(2, true);
    for v in [&GROVE_V4, &GROVE_V5] {
        let proof = db.prove_query(&query, None, v).unwrap().expect("proves");
        assert_eq!(index_layer_rows(&proof).len(), 3, "v{}", v.protocol_version);
        assert_eq!(
            verified_rows(&proof, &query, v).expect("the proof verifies"),
            vec![(vec![INDEX.to_vec(), b"a".to_vec()], b"d".to_vec())],
            "v{}",
            v.protocol_version
        );
    }

    // The empty tree is a row of the capped layer itself: no branch selects
    // `a`, so it is the first row of the page for the prover, and no row at
    // all for the verifier, which takes `c` as the page.
    let db = make_test_grovedb(&GROVE_V5);
    insert(&db, &[], INDEX, Element::empty_tree(), &GROVE_V5);
    insert(&db, &[INDEX], b"a", Element::empty_tree(), &GROVE_V5);
    insert(&db, &[INDEX], b"c", Element::new_item(vec![1]), &GROVE_V5);
    insert(&db, &[INDEX], b"d", Element::new_item(vec![2]), &GROVE_V5);
    let mut body = range_full(true);
    body.add_conditional_subquery(
        QueryItem::RangeFrom(b"z".to_vec()..),
        None,
        Some(range_full(true)),
    );
    body.limit = Some(1);
    let query = PathQuery::new_unsized(vec![INDEX.to_vec()], body);
    for v in [&GROVE_V4, &GROVE_V5] {
        let proof = db.prove_query(&query, None, v).unwrap().expect("proves");
        assert_eq!(index_layer_rows(&proof).len(), 3, "v{}", v.protocol_version);
        assert_eq!(
            verified_rows(&proof, &query, v).expect("the proof verifies"),
            vec![(vec![INDEX.to_vec()], b"c".to_vec())],
            "v{}",
            v.protocol_version
        );
    }
}

/// Past the cut the layer hides range bounds as well as rows. A query that
/// names its keys one by one proves each absent key by its neighbours; once
/// the page is full those witnesses are not owed either, and under V4 they
/// stay in the proof.
#[test]
fn absence_witnesses_past_the_cut_are_hidden() {
    let db = make_test_grovedb(&GROVE_V5);
    insert(&db, &[], INDEX, Element::empty_tree(), &GROVE_V5);
    for i in [0u16, 50, 150, 300] {
        let key = index_key(i);
        insert(&db, &[INDEX], &key, Element::empty_tree(), &GROVE_V5);
        insert(
            &db,
            &[INDEX, key.as_slice()],
            &doc_key(0),
            Element::new_item(vec![0]),
            &GROVE_V5,
        );
    }

    // Key 0 fills the page. Keys 100..120 are absent, between 50 and 150.
    let mut body = Query::new();
    body.insert_key(index_key(0));
    for absent in 100..120 {
        body.insert_key(index_key(absent));
    }
    body.set_subquery(range_full(true));
    body.limit = Some(1);
    let query = PathQuery::new_unsized(vec![INDEX.to_vec()], body);

    let (v4_proof, v4_rows) = prove_and_verify(&db, &query, &GROVE_V4);
    assert_eq!(v4_rows, doc_rows([0]));
    assert_eq!(index_layer_keys(&v4_proof), index_keys([0, 50, 150]));

    let (v5_proof, v5_rows) = prove_and_verify(&db, &query, &GROVE_V5);
    assert_eq!(v5_rows, doc_rows([0]));
    assert_eq!(index_layer_keys(&v5_proof), index_keys([0]));
}

/// Only the instance budget cuts a layer. When the global budget ends the
/// walk with cap to spare, the layer keeps the shape V4 gives it, which the
/// global limit already bounds: the proof is the same bytes.
#[test]
fn the_global_budget_alone_does_not_cut_the_layer() {
    let db = feed(50, |_| 3, &GROVE_V5);
    // 3 rows, then 1: the global limit of 4 is spent inside the second
    // subtree, with 3 of the cap of 7 left.
    let mut query = page(7, true);
    query.query.limit = Some(4);

    let (proof, rows) = prove_and_verify(&db, &query, &GROVE_V5);
    assert_eq!(rows.len(), 4);
    assert_eq!(index_layer_rows(&proof), index_keys(0..4));
    assert_eq!(
        proof,
        db.prove_query(&query, None, &GROVE_V4).unwrap().unwrap()
    );
}

/// Prover and verifier do not charge the global budget identically for
/// every empty descent: a subtree whose only content is an empty MMR costs
/// the prover an empty-layer slot and the verifier none. That predates the
/// truncation, and the truncation must not turn it into a refusal: with the
/// instance budget unspent the layer is not cut, and the proof verifies to
/// the same rows as under V4.
#[test]
fn an_uneven_global_charge_does_not_cut_the_layer() {
    const MMR: &[u8] = b"m";
    let db = make_test_grovedb(&GROVE_V5);
    let v = &GROVE_V5;
    insert(&db, &[], INDEX, Element::empty_tree(), v);
    // Three subtrees `index/<k>/x/m`, the MMR holding 0, 2 and 1 leaves,
    // then an empty subtree.
    for (i, leaves) in [(0u16, 0u8), (1, 2), (2, 1)] {
        let key = index_key(i);
        insert(&db, &[INDEX], &key, Element::empty_tree(), v);
        insert(&db, &[INDEX, &key], b"x", Element::empty_tree(), v);
        insert(&db, &[INDEX, &key, b"x"], MMR, Element::empty_mmr_tree(), v);
        for leaf in 0..leaves {
            db.mmr_tree_append(
                [INDEX, key.as_slice(), b"x"].as_ref(),
                MMR,
                vec![leaf],
                None,
                v,
            )
            .unwrap()
            .expect("the leaf appends");
        }
    }
    insert(&db, &[INDEX], &index_key(3), Element::empty_tree(), v);

    let mut positions = Query::new();
    positions.insert_range_inclusive(0u64.to_be_bytes().to_vec()..=9u64.to_be_bytes().to_vec());
    let mut mmr = Query::new();
    mmr.insert_key(MMR.to_vec());
    mmr.set_subquery(positions);
    let mut level = range_full(true);
    level.set_subquery(mmr);
    let mut body = range_full(true);
    body.set_subquery(level);
    body.limit = Some(5);
    let query = PathQuery::new(vec![INDEX.to_vec()], SizedQuery::new(body, Some(4), None));

    let v4_proof = db.prove_query(&query, None, &GROVE_V4).unwrap().unwrap();
    let v4_rows = verified_rows(&v4_proof, &query, &GROVE_V4).expect("V4 verifies");
    assert_eq!(v4_rows.len(), 3);

    let v5_proof = db.prove_query(&query, None, &GROVE_V5).unwrap().unwrap();
    assert_eq!(v5_proof, v4_proof);
    assert_eq!(
        verified_rows(&v5_proof, &query, &GROVE_V5).expect("V5 verifies"),
        v4_rows
    );
}

/// A global limit that a merge lifts into a cap cannot land on a layer this
/// change truncates. The V5 merge refuses to lift one onto a body that has
/// subqueries, and a body without subqueries is a layer of result rows,
/// proven under the cap itself in every version. So the original, globally
/// limited component still verifies as a subset of the merged V5 proof.
#[test]
fn a_lifted_global_limit_never_lands_on_a_truncated_layer() {
    let v = &GROVE_V5;
    let mut other = Query::new();
    other.insert_key(b"x".to_vec());
    let other = PathQuery::new_unsized(vec![OTHER.to_vec()], other);

    // A body with subqueries: the lift is refused.
    let mut with_subquery = range_full(true);
    with_subquery.set_subquery(range_full(true));
    let sized = PathQuery::new(
        vec![INDEX.to_vec()],
        SizedQuery::new(with_subquery, Some(PAGE), None),
    );
    assert!(matches!(
        PathQuery::merge(vec![&sized, &other], v),
        Err(Error::NotSupported(_))
    ));

    // A body of rows: the lift is allowed, and the layer is terminal.
    let db = make_test_grovedb(v);
    insert(&db, &[], INDEX, Element::empty_tree(), v);
    insert(&db, &[], OTHER, Element::empty_tree(), v);
    insert(&db, &[OTHER], b"x", Element::new_item(vec![9]), v);
    for i in 0..40u16 {
        insert(&db, &[INDEX], &index_key(i), Element::new_item(vec![1]), v);
    }
    let component = PathQuery::new(
        vec![INDEX.to_vec()],
        SizedQuery::new(range_full(true), Some(PAGE), None),
    );
    let merged = PathQuery::merge(vec![&component, &other], v).expect("the limit lifts");
    let (proof, rows) = prove_and_verify(&db, &merged, v);
    assert_eq!(rows.len(), PAGE as usize + 1);
    let (_, component_rows) = GroveDb::verify_subset_query(&proof, &component, v)
        .expect("the globally limited component is a subset of the merged proof");
    assert_eq!(component_rows.len(), PAGE as usize);
}
