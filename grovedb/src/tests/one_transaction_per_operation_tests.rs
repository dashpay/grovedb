//! An operation that is given no transaction starts one, and only one.
//!
//! [`PinnedReads`] is the RocksDB engine with one difference: the transaction
//! of a read that was given none is a snapshot read transaction, as on an
//! engine that pins such a read to one state. It counts the transactions it
//! starts. A read that started two would read two states on such an engine,
//! and a read that wrote would be refused.
//!
//! These tests name that engine, so they run the same whichever engine the
//! suite is on.

use std::sync::atomic::{AtomicUsize, Ordering};

use grovedb_element::indexed::IndexAxis;
use grovedb_merk::proofs::{query::QueryItem, Decoder, Op, Query};
use grovedb_storage::Error as StorageError;
use grovedb_version::version::{v4::GROVE_V4, GroveVersion};
use tempfile::TempDir;

use crate::{
    batch::QualifiedGroveDbOp,
    operations::proof::{GroveDBProof, ProofBytes},
    query_result_type::QueryResultType,
    reference_path::ReferencePathType,
    tests::{
        faultable_reads::{with_failed_parent_read, INJECTED_READ_FAILURE},
        pinned_reads::PinnedReads,
    },
    AggregateSumPathQuery, Element, Engine, Error, GroveDb, PathQuery, SizedQuery,
};

const TREE: &[u8] = b"tree";
const INNER: &[u8] = b"inner";
const SUMS: &[u8] = b"sums";
const RANKED: &[u8] = b"ranked";
const COUNTED: &[u8] = b"counted";
const INDEX: &[u8] = b"index";

fn item(value: &[u8]) -> Element {
    Element::new_item(value.to_vec())
}

/// `tree` holds two items, a reference to the first, and the tree `inner`
/// with two items. `sums` is a sum tree with two sum items.
fn database(grove_version: &GroveVersion) -> (TempDir, GroveDb<PinnedReads>) {
    let dir = TempDir::new().expect("temporary directory");
    let db = GroveDb::<PinnedReads>::open_with_engine(dir.path()).expect("open");
    let tree: &[&[u8]] = &[TREE];
    let inner: &[&[u8]] = &[TREE, INNER];
    let sums: &[&[u8]] = &[SUMS];
    let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
        TREE.to_vec(),
        b"k1".to_vec(),
    ]));
    let elements: Vec<(&[&[u8]], &[u8], Element)> = vec![
        (&[], TREE, Element::empty_tree()),
        (&[], SUMS, Element::empty_sum_tree()),
        (tree, b"k1", item(b"one")),
        (tree, b"k2", item(b"two")),
        (tree, b"ref", reference),
        (tree, INNER, Element::empty_tree()),
        (inner, b"i1", item(b"inner one")),
        (inner, b"i2", item(b"inner two")),
        (sums, b"s1", Element::new_sum_item(5)),
        (sums, b"s2", Element::new_sum_item(7)),
    ];
    for (path, key, element) in elements {
        db.insert(path, key, element, None, None, grove_version)
            .unwrap()
            .expect("insert");
    }
    (dir, db)
}

/// Runs `operation` and returns its result with the number of read
/// transactions, of write transactions and of snapshot read transactions it
/// started.
fn started_with_snapshots<T>(
    db: &GroveDb<PinnedReads>,
    operation: impl FnOnce() -> T,
) -> (T, usize, usize, usize) {
    let engine = db.engine();
    let count = |counter: &AtomicUsize| counter.load(Ordering::Relaxed);
    let reads = count(&engine.reads_started);
    let writes = count(&engine.writes_started);
    let snapshots = count(&engine.snapshots_started);
    let result = operation();
    (
        result,
        count(&engine.reads_started) - reads,
        count(&engine.writes_started) - writes,
        count(&engine.snapshots_started) - snapshots,
    )
}

/// Runs `operation`, which may start no snapshot read transaction, and
/// returns its result with the number of read transactions and of write
/// transactions it started.
fn started<T>(db: &GroveDb<PinnedReads>, operation: impl FnOnce() -> T) -> (T, usize, usize) {
    let (result, reads, writes, snapshots) = started_with_snapshots(db, operation);
    assert_eq!(snapshots, 0, "snapshot read transactions started");
    (result, reads, writes)
}

/// Every item of `tree` and, under `inner`, every item of that tree.
fn query_with_a_subquery() -> PathQuery {
    let mut query = Query::new();
    query.insert_all();
    let mut subquery = Query::new();
    subquery.insert_all();
    query.set_subquery(subquery);
    PathQuery::new(vec![TREE.to_vec()], SizedQuery::new(query, None, None))
}

fn query_all(path: &[&[u8]]) -> PathQuery {
    let mut query = Query::new();
    query.insert_all();
    PathQuery::new(
        path.iter().map(|segment| segment.to_vec()).collect(),
        SizedQuery::new(query, None, None),
    )
}

macro_rules! assert_one_read_transaction {
    ($db:expr, $name:literal, $operation:expr) => {{
        let (result, reads, writes) = started($db, || $operation);
        assert_eq!(
            (reads, writes),
            (1, 0),
            "{}: read and write transactions started",
            $name
        );
        result
    }};
}

#[test]
fn a_read_that_is_given_no_transaction_starts_one_read_transaction() {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = database(grove_version);
    let tree: &[&[u8]] = &[TREE];
    let elements = QueryResultType::QueryElementResultType;

    // A reference is read and then followed: two reads, one state.
    let followed =
        assert_one_read_transaction!(&db, "get", db.get(tree, b"ref", None, grove_version));
    assert_eq!(followed.unwrap().expect("get"), item(b"one"));
    assert_one_read_transaction!(
        &db,
        "get_raw",
        db.get_raw(tree.into(), b"k1", None, grove_version)
    )
    .unwrap()
    .expect("get_raw");
    assert!(assert_one_read_transaction!(
        &db,
        "has_raw",
        db.has_raw(tree, b"k1", None, grove_version)
    )
    .unwrap()
    .expect("has_raw"));
    assert!(!assert_one_read_transaction!(
        &db,
        "is_empty_tree",
        db.is_empty_tree(tree, None, grove_version)
    )
    .unwrap()
    .expect("is_empty_tree"));
    assert_one_read_transaction!(&db, "root_hash", db.root_hash(None, grove_version))
        .unwrap()
        .expect("root_hash");
    assert_one_read_transaction!(
        &db,
        "find_subtrees",
        db.find_subtrees(&tree.into(), None, grove_version)
    )
    .unwrap()
    .expect("find_subtrees");
    assert_one_read_transaction!(&db, "get_aux", db.get_aux(b"absent", None))
        .unwrap()
        .expect("get_aux");
    assert!(assert_one_read_transaction!(
        &db,
        "verify_grovedb",
        db.verify_grovedb(None, true, false, grove_version)
    )
    .expect("verify_grovedb")
    .is_empty());

    // A query walks one frame for the tree and one for each tree its
    // subquery enters, and follows the references it finds.
    let nested = query_with_a_subquery();
    let (found, _) = assert_one_read_transaction!(
        &db,
        "query",
        db.query(&nested, true, true, true, elements, None, grove_version)
    )
    .unwrap()
    .expect("query");
    assert_eq!(
        found.to_elements(),
        [
            item(b"inner one"),
            item(b"inner two"),
            item(b"one"),
            item(b"two"),
            item(b"one")
        ]
    );
    assert_one_read_transaction!(
        &db,
        "query_raw",
        db.query_raw(&nested, true, true, true, elements, None, grove_version)
    )
    .unwrap()
    .expect("query_raw");
    assert_one_read_transaction!(
        &db,
        "query_many_raw",
        db.query_many_raw(
            &[&nested, &query_all(&[SUMS])],
            true,
            true,
            true,
            elements,
            None,
            grove_version
        )
    )
    .unwrap()
    .expect("query_many_raw");
    assert_one_read_transaction!(
        &db,
        "query_item_value",
        db.query_item_value(&nested, true, true, true, None, grove_version)
    )
    .unwrap()
    .expect("query_item_value");
    let (sums, _) = assert_one_read_transaction!(
        &db,
        "query_sums",
        db.query_sums(&query_all(&[SUMS]), true, true, true, None, grove_version)
    )
    .unwrap()
    .expect("query_sums");
    assert_eq!(sums, [5, 7]);
    assert_one_read_transaction!(
        &db,
        "run_path_query",
        db.run_path_query(&nested, true, true, true, elements, None, grove_version)
    )
    .unwrap()
    .expect("run_path_query");
    // A proof reads its layers through a snapshot the prover pins, and
    // follows the reference of a row in a read transaction of its own.
    let (proved, reads, writes, snapshots) =
        started_with_snapshots(&db, || db.prove_query(&nested, None, grove_version));
    proved.unwrap().expect("prove_query");
    assert_eq!((snapshots, reads, writes), (1, 1, 0), "prove_query");
    let sums_up_to_a_limit = AggregateSumPathQuery::new_single_query_item(
        vec![SUMS.to_vec()],
        QueryItem::RangeFull(..),
        100,
        None,
    );
    assert_one_read_transaction!(
        &db,
        "query_aggregate_sums",
        db.query_aggregate_sums(&sums_up_to_a_limit, true, true, None, grove_version)
    )
    .unwrap()
    .expect("query_aggregate_sums");
}

#[test]
fn an_append_only_tree_is_read_in_the_transaction_of_its_element() {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = database(grove_version);
    let root: &[&[u8]] = &[];
    db.insert(
        root,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert the tree");

    // An append reads the element of its tree as a read of its own, and
    // then writes: two operations, as before the seam.
    let (appended, reads, writes) = started(&db, || {
        db.mmr_tree_append(root, b"mmr", b"leaf".to_vec(), None, grove_version)
    });
    appended.unwrap().expect("append");
    assert_eq!((reads, writes), (1, 1), "mmr_tree_append");

    assert_one_read_transaction!(
        &db,
        "mmr_tree_root_hash",
        db.mmr_tree_root_hash(root, b"mmr", None, grove_version)
    )
    .unwrap()
    .expect("root hash");
    let leaf = assert_one_read_transaction!(
        &db,
        "mmr_tree_get_value",
        db.mmr_tree_get_value(root, b"mmr", 0, None, grove_version)
    )
    .unwrap()
    .expect("leaf");
    assert_eq!(leaf, Some(b"leaf".to_vec()));
}

#[test]
fn a_failed_parent_read_is_a_storage_error_on_a_second_engine() {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = database(grove_version);
    let inner: &[&[u8]] = &[TREE, INNER];
    let read = || {
        db.get_raw_optional(inner.into(), b"i1", None, grove_version)
            .unwrap()
    };

    // Opening `inner` reads its element in `tree`. The read fails in the
    // handle of this engine, behind the node store.
    let failed = with_failed_parent_read(INNER, read);
    assert!(
        matches!(
            &failed,
            Err(Error::StorageError(StorageError::StorageError(message)))
                if message == INJECTED_READ_FAILURE
        ),
        "got {failed:?}"
    );
    assert_eq!(read().expect("a healthy read"), Some(item(b"inner one")));
    // A parent that is not there is still absence.
    let absent: &[&[u8]] = &[TREE, b"absent"];
    let below_nothing = with_failed_parent_read(INNER, || {
        db.get_raw_optional(absent.into(), b"i1", None, grove_version)
            .unwrap()
    });
    assert_eq!(below_nothing.expect("absence"), None);
}

#[test]
fn a_write_that_is_given_no_transaction_starts_one_write_transaction() {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = database(grove_version);
    let tree: &[&[u8]] = &[TREE];

    let (inserted, reads, writes) = started(&db, || {
        db.insert(tree, b"k3", item(b"three"), None, None, grove_version)
    });
    inserted.unwrap().expect("insert");
    assert_eq!((reads, writes), (0, 1), "insert");

    let (deleted, reads, writes) =
        started(&db, || db.delete(tree, b"k3", None, None, grove_version));
    deleted.unwrap().expect("delete");
    assert_eq!((reads, writes), (0, 1), "delete");

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![TREE.to_vec()],
        b"k4".to_vec(),
        item(b"four"),
    )];
    let (applied, reads, writes) = started(&db, || db.apply_batch(ops, None, None, grove_version));
    applied.unwrap().expect("apply_batch");
    assert_eq!((reads, writes), (0, 1), "apply_batch");

    let (put, reads, writes) = started(&db, || db.put_aux(b"aux", b"value", None, None));
    put.unwrap().expect("put_aux");
    assert_eq!((reads, writes), (0, 1), "put_aux");
}

#[test]
fn a_transaction_the_caller_gives_is_the_only_one_an_operation_uses() {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = database(grove_version);
    let tree: &[&[u8]] = &[TREE];
    let transaction = db.start_transaction();

    let (results, reads, writes) = started(&db, || {
        db.insert(
            tree,
            b"k3",
            item(b"three"),
            None,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("insert");
        db.query(
            &query_with_a_subquery(),
            true,
            true,
            true,
            QueryResultType::QueryElementResultType,
            Some(&transaction),
            grove_version,
        )
        .unwrap()
        .expect("query")
    });
    assert_eq!((reads, writes), (0, 0));
    // The query read through the caller's transaction: it sees the insert.
    assert!(results.0.to_elements().contains(&item(b"three")));
}

/// The indexed trees with a 128-bit sum on an engine that is not the default
/// one: a write through their entry points starts one write transaction, and
/// a read of the big sum axis one read transaction.
#[test]
fn the_big_sum_axis_is_written_and_read_in_one_transaction_each() {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = database(grove_version);
    let root: &[&[u8]] = &[];
    let ranked: &[&[u8]] = &[RANKED];
    let counted: &[&[u8]] = &[COUNTED];
    let count_and_big_sum = Element::empty_provable_count_provable_big_sum_indexed_tree(vec![
        (IndexAxis::Count.tag(), None),
        (IndexAxis::BigSum.tag(), None),
    ])
    .expect("canonical axes");
    for (key, tree) in [
        (RANKED, Element::empty_provable_big_sum_indexed_tree()),
        (COUNTED, count_and_big_sum),
    ] {
        db.insert(root, key, tree, None, None, grove_version)
            .unwrap()
            .expect("insert");
    }

    let largest = i64::MAX;
    for (key, sum) in [(b"a", 5), (b"b", largest)] {
        let entry = Element::new_sum_item(sum);
        let (inserted, reads, writes) = started(&db, || {
            db.insert_into_provable_big_sum_indexed_tree(
                ranked,
                key,
                entry.clone(),
                None,
                grove_version,
            )
        });
        inserted
            .unwrap()
            .expect("insert_into_provable_big_sum_indexed_tree");
        assert_eq!((reads, writes), (0, 1), "insert into the big sum tree");
        let (inserted, reads, writes) = started(&db, || {
            db.insert_into_provable_count_provable_big_sum_indexed_tree(
                counted,
                key,
                entry,
                None,
                grove_version,
            )
        });
        inserted
            .unwrap()
            .expect("insert_into_provable_count_provable_big_sum_indexed_tree");
        assert_eq!(
            (reads, writes),
            (0, 1),
            "insert into the count and big sum tree"
        );
    }

    // An entry whose sum is past the range of an `i64`: a tree with a
    // 128-bit sum that holds the largest sum item twice.
    db.insert_into_provable_big_sum_indexed_tree(
        ranked,
        b"c",
        Element::empty_big_sum_tree(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert_into_provable_big_sum_indexed_tree");
    db.insert_into_provable_count_provable_big_sum_indexed_tree(
        counted,
        b"c",
        Element::empty_count_big_sum_tree(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert_into_provable_count_provable_big_sum_indexed_tree");
    for tree in [RANKED, COUNTED] {
        let entry: &[&[u8]] = &[tree, b"c"];
        for key in [b"0", b"1"] {
            let item = Element::new_sum_item(largest);
            db.insert(entry, key, item, None, None, grove_version)
                .unwrap()
                .expect("insert");
        }
    }
    let widest = largest as i128 * 2;

    let highest_first = [
        (widest, b"c".to_vec()),
        (largest as i128, b"b".to_vec()),
        (5, b"a".to_vec()),
    ];
    let lowest_first = [
        (5, b"a".to_vec()),
        (largest as i128, b"b".to_vec()),
        (widest, b"c".to_vec()),
    ];
    let page = assert_one_read_transaction!(
        &db,
        "indexed_big_sum_top_k_paginated",
        db.indexed_big_sum_top_k_paginated(ranked, 10, 0, true, None, grove_version)
    )
    .unwrap()
    .expect("indexed_big_sum_top_k_paginated");
    let ranks: Vec<_> = page.entries.into_iter().map(|e| e.key_pair()).collect();
    assert_eq!(ranks, highest_first);
    // An offset is skipped by count, through a cursor the read points at the
    // secondary of the axis.
    let page = assert_one_read_transaction!(
        &db,
        "indexed_big_sum_top_k_paginated after an offset",
        db.indexed_big_sum_top_k_paginated(ranked, 10, 1, true, None, grove_version)
    )
    .unwrap()
    .expect("indexed_big_sum_top_k_paginated");
    assert_eq!(page.skipped, 1);
    let ranks: Vec<_> = page.entries.into_iter().map(|e| e.key_pair()).collect();
    assert_eq!(ranks, highest_first[1..]);
    let band = assert_one_read_transaction!(
        &db,
        "indexed_big_sum_range",
        db.indexed_big_sum_range(ranked, 0, i128::MAX, false, 10, None, grove_version)
    )
    .unwrap()
    .expect("indexed_big_sum_range");
    let ranks: Vec<_> = band.into_iter().map(|e| e.key_pair()).collect();
    assert_eq!(ranks, lowest_first);
    let population = assert_one_read_transaction!(
        &db,
        "indexed_big_sum_population_over_value_range",
        db.indexed_big_sum_population_over_value_range(ranked, 6, i128::MAX, None, grove_version)
    )
    .unwrap()
    .expect("indexed_big_sum_population_over_value_range");
    assert_eq!(population, 2);
    let keys = assert_one_read_transaction!(
        &db,
        "indexed_big_sum_top_k_paginated_keys",
        db.indexed_big_sum_top_k_paginated_keys(counted, 10, 0, true, None, grove_version)
    )
    .unwrap()
    .expect("indexed_big_sum_top_k_paginated_keys");
    assert_eq!(keys.entries, highest_first);
    let keys = assert_one_read_transaction!(
        &db,
        "indexed_big_sum_top_k_paginated_keys after an offset",
        db.indexed_big_sum_top_k_paginated_keys(counted, 10, 2, true, None, grove_version)
    )
    .unwrap()
    .expect("indexed_big_sum_top_k_paginated_keys");
    assert_eq!((keys.skipped, &keys.entries[..]), (2, &highest_first[2..]));
    let keys = assert_one_read_transaction!(
        &db,
        "indexed_big_sum_range_keys",
        db.indexed_big_sum_range_keys(counted, 0, i128::MAX, false, 10, None, grove_version)
    )
    .unwrap()
    .expect("indexed_big_sum_range_keys");
    assert_eq!(keys, lowest_first);

    let (deleted, reads, writes) = started(&db, || {
        db.delete_from_provable_big_sum_indexed_tree(ranked, b"a", None, grove_version)
    });
    assert!(deleted
        .unwrap()
        .expect("delete_from_provable_big_sum_indexed_tree"));
    assert_eq!((reads, writes), (0, 1), "delete from the big sum tree");
    let (deleted, reads, writes) = started(&db, || {
        db.delete_from_provable_count_provable_big_sum_indexed_tree(
            counted,
            b"a",
            None,
            grove_version,
        )
    });
    assert!(deleted
        .unwrap()
        .expect("delete_from_provable_count_provable_big_sum_indexed_tree"));
    assert_eq!(
        (reads, writes),
        (0, 1),
        "delete from the count and big sum tree"
    );
}

/// A conversion to a 128-bit sum reads every node of the tree and writes it
/// again, and for an indexed tree rebuilds the rows of its axes. A repair
/// rebuilds them too, here of a ranking that has a row to lose. On an engine
/// that is not the default one each starts one write transaction, and none
/// when the caller gives one.
#[test]
fn a_conversion_and_a_repair_are_one_write_transaction_each() {
    let grove_version = GroveVersion::latest();
    let (_dir, db) = database(grove_version);
    let root: &[&[u8]] = &[];
    let ranked: &[&[u8]] = &[RANKED];
    let sums: &[&[u8]] = &[SUMS];
    db.insert(
        root,
        RANKED,
        Element::empty_provable_sum_indexed_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");
    for (key, sum) in [(b"a", 5), (b"b", -3), (b"c", 40)] {
        db.insert_into_provable_sum_indexed_tree(
            ranked,
            key,
            Element::new_sum_item(sum),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert_into_provable_sum_indexed_tree");
    }

    // A ranking that holds the row of an entry that is gone, as the removal
    // of the entry at the root of this tree leaves it on grove version 4:
    // the repair has a row to remove.
    let drifted: &[&[u8]] = &[INDEX];
    db.insert(
        root,
        INDEX,
        Element::empty_provable_count_indexed_tree(),
        None,
        None,
        &GROVE_V4,
    )
    .unwrap()
    .expect("insert");
    for key in [b"k0", b"k2", b"k3", b"k4", b"k1"] {
        db.insert_into_count_indexed_tree(drifted, key, item(b"v"), None, &GROVE_V4)
            .unwrap()
            .expect("insert_into_count_indexed_tree");
    }
    db.delete_from_count_indexed_tree(drifted, b"k2", None, &GROVE_V4)
        .unwrap()
        .expect("delete_from_count_indexed_tree");
    let ranking = || db.indexed_count_top_k_paginated(drifted, 10, 0, true, None, grove_version);
    assert!(
        matches!(ranking().unwrap(), Err(Error::CorruptedData(_))),
        "the ranking names an entry that is gone"
    );

    let (repaired, reads, writes) = started(&db, || {
        db.reconcile_indexed_tree_secondaries(drifted, None, grove_version)
    });
    repaired.unwrap().expect("reconcile");
    assert_eq!((reads, writes), (0, 1), "a repair that removes a row");
    let (page, reads, writes) = started(&db, ranking);
    assert_eq!((reads, writes), (1, 0), "the ranking of the repaired tree");
    assert_eq!(page.unwrap().expect("the ranking reads").entries.len(), 4);

    let (converted, reads, writes) = started(&db, || {
        db.convert_to_big_sum_tree(root, SUMS, None, grove_version)
    });
    converted.unwrap().expect("convert a sum tree");
    assert_eq!((reads, writes), (0, 1), "a sum tree");
    assert!(matches!(
        db.get(root, SUMS, None, grove_version)
            .unwrap()
            .expect("get"),
        Element::BigSumTree(_, 12, _)
    ));
    db.insert(
        sums,
        b"s3",
        Element::new_sum_item(i64::MAX),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("the converted tree takes a sum past an i64");

    let (converted, reads, writes) = started(&db, || {
        db.convert_to_big_sum_tree(root, RANKED, None, grove_version)
    });
    converted.unwrap().expect("convert an indexed tree");
    assert_eq!((reads, writes), (0, 1), "an indexed tree");
    let (page, reads, writes) = started(&db, || {
        db.indexed_big_sum_top_k_paginated(ranked, 10, 0, true, None, grove_version)
    });
    let page = page.unwrap().expect("indexed_big_sum_top_k_paginated");
    assert_eq!((reads, writes), (1, 0), "the ranking of the converted tree");
    let keys: Vec<&[u8]> = page
        .entries
        .iter()
        .map(|entry| entry.primary_key.as_slice())
        .collect();
    assert_eq!(keys, [b"c".as_slice(), b"a", b"b"]);

    db.insert(
        root,
        COUNTED,
        Element::empty_count_sum_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");
    let transaction = db.start_transaction();
    let (converted, reads, writes) = started(&db, || {
        db.convert_to_big_sum_tree(root, COUNTED, Some(&transaction), grove_version)
    });
    converted.unwrap().expect("convert in a transaction");
    assert_eq!((reads, writes), (0, 0), "in the caller's transaction");
    let (repaired, reads, writes) = started(&db, || {
        db.reconcile_indexed_tree_secondaries(ranked, Some(&transaction), grove_version)
    });
    repaired.unwrap().expect("reconcile in a transaction");
    assert_eq!((reads, writes), (0, 0), "in the caller's transaction");
    assert!(matches!(
        db.get(root, COUNTED, None, grove_version)
            .unwrap()
            .expect("get"),
        Element::CountSumTree(..)
    ));
    db.commit_transaction(transaction).unwrap().expect("commit");
    assert!(matches!(
        db.get(root, COUNTED, None, grove_version)
            .unwrap()
            .expect("get"),
        Element::CountBigSumTree(..)
    ));
}

/// `index` holds six trees with one item each.
fn insert_index<E: Engine>(db: &GroveDb<E>, grove_version: &GroveVersion) {
    let root: &[&[u8]] = &[];
    db.insert(
        root,
        INDEX,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");
    for position in 0u8..6 {
        let index: &[&[u8]] = &[INDEX];
        db.insert(
            index,
            &[position],
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let entry: &[&[u8]] = &[INDEX, &[position]];
        db.insert(entry, b"doc", item(&[position]), None, None, grove_version)
            .unwrap()
            .expect("insert");
    }
}

/// How many keys the proof of the `index` layer reveals.
fn keys_revealed_in_the_index(proof: &[u8]) -> usize {
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let (proof, _): (GroveDBProof, usize) =
        bincode::decode_from_slice(proof, config).expect("the proof decodes");
    let GroveDBProof::V1(proof) = proof else {
        panic!("expected a V1 proof envelope");
    };
    let layer = proof
        .root_layer
        .lower_layers
        .get(INDEX)
        .expect("the proof descends into the index");
    let ProofBytes::Merk(bytes) = &layer.merk_proof else {
        panic!("the index layer is a Merk proof");
    };
    Decoder::new(bytes)
        .filter(|op| match op.as_ref().expect("the op decodes") {
            Op::Push(node) | Op::PushInverted(node) => node.key().is_some(),
            _ => false,
        })
        .count()
}

/// A layer under a per-instance cap is proven a second time, cut where the
/// cap is spent. Both proofs of the layer read the one snapshot the prover
/// pins, and the bytes are those the default engine proves.
#[test]
fn a_capped_layer_is_cut_in_the_snapshot_of_its_proof() {
    let grove_version = GroveVersion::latest();
    let pinned_dir = TempDir::new().expect("temporary directory");
    let pinned = GroveDb::<PinnedReads>::open_with_engine(pinned_dir.path()).expect("open");
    let default_dir = TempDir::new().expect("temporary directory");
    let default = GroveDb::open(default_dir.path()).expect("open");
    insert_index(&pinned, grove_version);
    insert_index(&default, grove_version);

    let mut rows = Query::new();
    rows.insert_all();
    let mut page = Query::new();
    page.insert_all();
    page.set_subquery(rows);
    page.limit = Some(2);
    let page = PathQuery::new_unsized(vec![INDEX.to_vec()], page);

    let (proof, reads, writes, snapshots) =
        started_with_snapshots(&pinned, || pinned.prove_query(&page, None, grove_version));
    let proof = proof.unwrap().expect("prove_query");
    assert_eq!((snapshots, reads, writes), (1, 0, 0));
    let (root, proved) = GroveDb::verify_query(&proof, &page, grove_version).expect("verify");
    assert_eq!(
        root,
        pinned.root_hash(None, grove_version).unwrap().unwrap()
    );
    assert_eq!(proved.len(), 2);

    let on_the_default_engine = default
        .prove_query(&page, None, grove_version)
        .unwrap()
        .expect("prove_query");
    assert_eq!(proof, on_the_default_engine);
    // The layer ends after the two keys whose rows fill the page. GROVE_V4
    // proves it whole: every key of the index.
    assert_eq!(keys_revealed_in_the_index(&proof), 2);
    let whole = pinned
        .prove_query(&page, None, &GROVE_V4)
        .unwrap()
        .expect("prove_query");
    assert_eq!(keys_revealed_in_the_index(&whole), 6);
}
