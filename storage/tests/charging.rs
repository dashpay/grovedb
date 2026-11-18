//! The RocksDB engine works out no charge of its own: every
//! `OperationCost` it reports is the result of a function of
//! `grovedb_costs::cost_model`.
//!
//! The test reads the engine's sources and fails on any line outside a test
//! module that builds a cost, reads or writes a field of one, or does the
//! arithmetic of a rule. Passing a cost on (`wrap_with_cost`, a `CostResult`
//! in a signature) is not a charge.

const SOURCES: [(&str, &str); 7] = [
    (
        "rocksdb_storage/storage.rs",
        include_str!("../src/rocksdb_storage/storage.rs"),
    ),
    (
        "rocksdb_storage/storage_context.rs",
        include_str!("../src/rocksdb_storage/storage_context.rs"),
    ),
    (
        "rocksdb_storage/storage_context/batch.rs",
        include_str!("../src/rocksdb_storage/storage_context/batch.rs"),
    ),
    (
        "rocksdb_storage/storage_context/context_immediate.rs",
        include_str!("../src/rocksdb_storage/storage_context/context_immediate.rs"),
    ),
    (
        "rocksdb_storage/storage_context/context_tx.rs",
        include_str!("../src/rocksdb_storage/storage_context/context_tx.rs"),
    ),
    (
        "rocksdb_storage/storage_context/flat.rs",
        include_str!("../src/rocksdb_storage/storage_context/flat.rs"),
    ),
    (
        "rocksdb_storage/storage_context/raw_iterator.rs",
        include_str!("../src/rocksdb_storage/storage_context/raw_iterator.rs"),
    ),
];

const CHARGES: [&str; 17] = [
    "OperationCost::",
    "OperationCost {",
    "StorageCost {",
    "Default::default()",
    "seek_count",
    "storage_loaded_bytes",
    "hash_node_calls",
    "sinsemilla_hash_calls",
    ".storage_cost",
    "added_bytes",
    "replaced_bytes",
    "removed_bytes",
    "add_key_value_storage_costs",
    "required_space",
    "add_cost(",
    "unwrap_add_cost(",
    "+=",
];

fn charges_stated_in(source: &str) -> Vec<(usize, &str, &str)> {
    let library = source.split("#[cfg(test)]").next().unwrap_or(source);
    library
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let code = line
                .split("//")
                .next()
                .unwrap_or(line)
                .replace("-> OperationCost {", "");
            CHARGES
                .into_iter()
                .find(|charge| code.contains(charge))
                .map(|charge| (index + 1, charge, line))
        })
        .collect()
}

#[test]
fn the_rocksdb_engine_charges_only_through_the_cost_model() {
    for (file, source) in SOURCES {
        let stated = charges_stated_in(source);
        assert!(stated.is_empty(), "{file} states a charge: {stated:#?}");
        assert!(
            file.ends_with("storage_context.rs")
                || file.ends_with("flat.rs")
                || source.contains("COST_MODEL"),
            "{file} reports costs and names no cost model"
        );
    }
}

#[test]
fn a_charge_stated_outside_the_model_is_found() {
    let stated = charges_stated_in(
        "fn get() {\n    cost.seek_count += 1; // one seek\n}\n// cost.seek_count\n#[cfg(test)]\nmod \
         tests { fn f() { OperationCost::default(); } }\n",
    );
    assert_eq!(stated.len(), 1);
    assert_eq!(stated[0].0, 2);
    assert!(charges_stated_in("fn cost() -> OperationCost {").is_empty());
    for charge in CHARGES {
        assert_eq!(charges_stated_in(charge).len(), 1, "{charge}");
    }
}
