//! Prints, for a few common operations, the cost GroveDB reports on the
//! cost model and the storage rules that were applied to produce it:
//! the table of section 11.1 of `docs/lignindb/cost-model.md`.
//!
//! `check.sh profile [GROVE_VERSION]` builds and runs it.

use std::collections::BTreeMap;

use grovedb::{
    operations::delete::DeleteOptions, query_result_type::QueryResultType, Element, GroveDb,
    PathQuery, Query, SizedQuery, Transaction,
};
use grovedb_costs::{CostResult, OperationCost};
use grovedb_storage::cost_model::rules::hits;
use grovedb_version::version::{
    v1::GROVE_V1, v2::GROVE_V2, v3::GROVE_V3, v4::GROVE_V4, GroveVersion,
};

const TREE: &[u8] = b"tree";

fn applied() -> BTreeMap<&'static str, u64> {
    let mut by_rule = BTreeMap::new();
    for (rule, count) in hits() {
        *by_rule.entry(rule.section()).or_default() += count;
    }
    by_rule
}

fn row<T, E: std::fmt::Debug>(name: &str, run: impl FnOnce() -> CostResult<T, E>) -> Option<T> {
    let before = applied();
    let result = run();
    let events: Vec<String> = applied()
        .into_iter()
        .filter(|(rule, _)| !rule.starts_with('N'))
        .filter_map(|(rule, count)| {
            let times = count - before.get(rule).copied().unwrap_or(0);
            (times > 0).then(|| format!("{rule}×{times}"))
        })
        .collect();
    let OperationCost {
        seek_count,
        storage_cost,
        storage_loaded_bytes,
        hash_node_calls,
        ..
    } = &result.cost;
    println!(
        "| {name} | {seek_count} | {storage_loaded_bytes} | {} | {} | {} | {hash_node_calls} | {} |",
        storage_cost.added_bytes,
        storage_cost.replaced_bytes,
        storage_cost.removed_bytes.total_removed_bytes(),
        events.join(" ")
    );
    result.value.ok()
}

fn item(byte: u8, len: usize) -> Element {
    Element::new_item(vec![byte; len])
}

fn insert<'db>(
    db: &'db GroveDb,
    tx: &'db Transaction<'db>,
    path: &'db [&'db [u8]],
    key: &'db [u8],
    element: Element,
    version: &'db GroveVersion,
) -> impl FnOnce() -> CostResult<(), grovedb::Error> + 'db {
    move || db.insert(path, key, element, None, Some(tx), version)
}

fn main() {
    let version = match std::env::args().nth(1).as_deref() {
        Some("1") => &GROVE_V1,
        Some("2") => &GROVE_V2,
        Some("3") => &GROVE_V3,
        Some("4") => &GROVE_V4,
        _ => GroveVersion::latest(),
    };
    let directory = tempfile::tempdir().expect("a temporary directory");
    let db = GroveDb::open(directory.path()).expect("a database");
    let tx = db.start_transaction();
    let root: &[&[u8]] = &[];
    let below: &[&[u8]] = &[TREE];

    println!(
        "| Operation | Seeks | Loaded | Added | Replaced | Removed | Hashes | Storage rules applied |"
    );
    println!("| --- | --- | --- | --- | --- | --- | --- | --- |");

    row(
        "insert an empty tree into the empty root tree",
        insert(&db, &tx, root, TREE, Element::empty_tree(), version),
    );
    row(
        "insert a second empty tree next to it",
        insert(&db, &tx, root, b"tree2", Element::empty_tree(), version),
    );
    row(
        "insert a 100-byte item into the first tree, which is empty",
        insert(&db, &tx, below, b"key1", item(7, 100), version),
    );
    for key in [b"key2", b"key3", b"key4", b"key5", b"key6", b"key7"] {
        db.insert(below, key, item(7, 100), None, Some(&tx), version)
            .value
            .expect("an insert");
    }
    row(
        "insert an eighth item",
        insert(&db, &tx, below, b"key8", item(7, 100), version),
    );
    row(
        "replace an item by one 20 bytes longer",
        insert(&db, &tx, below, b"key4", item(9, 120), version),
    );
    row("get the item at the root of its tree", || {
        db.get(below, b"key4", Some(&tx), version)
    });
    row("get an item further down", || {
        db.get(below, b"key7", Some(&tx), version)
    });
    row("get a key that is not there", || {
        db.get(below, b"none", Some(&tx), version)
    });
    row("delete an item", || {
        db.delete(below, b"key2", None, Some(&tx), version)
    });
    let everything = PathQuery::new(
        vec![TREE.to_vec()],
        SizedQuery::new(Query::new_range_full(), None, None),
    );
    row("read the seven items with a range query", || {
        db.query_raw(
            &everything,
            true,
            true,
            true,
            QueryResultType::QueryElementResultType,
            Some(&tx),
            version,
        )
    });
    row("commit the transaction", || db.commit_transaction(tx));
    row("prove the same range query", || {
        db.prove_query(&everything, None, version)
    });
    let tx = db.start_transaction();
    let with_its_items = || {
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: false,
            ..Default::default()
        })
    };
    row("delete the tree with its seven committed items", || {
        db.delete(root, TREE, with_its_items(), Some(&tx), version)
    });
    let fresh: &[&[u8]] = &[b"tree3"];
    db.insert(
        root,
        b"tree3",
        Element::empty_tree(),
        None,
        Some(&tx),
        version,
    )
    .value
    .expect("an insert");
    for key in [
        b"key1", b"key2", b"key3", b"key4", b"key5", b"key6", b"key7",
    ] {
        db.insert(fresh, key, item(7, 100), None, Some(&tx), version)
            .value
            .expect("an insert");
    }
    row(
        "delete a tree with seven items the same transaction wrote",
        || db.delete(root, b"tree3", with_its_items(), Some(&tx), version),
    );
}
