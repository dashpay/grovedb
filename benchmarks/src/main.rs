use grovedb::{
    batch::{key_info::KeyInfo, GroveDbOp},
    Element, GroveDb, PathQuery, Query,
};
use rand::Rng;
use std::time::Instant;
use tempfile::TempDir;

/// Generate a random byte sequence
fn generate_random_bytes(size: usize) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let mut bytes = vec![0u8; size];
    rng.fill(&mut bytes[..]);
    bytes
}

/// Populate the root tree of a GroveDB instance with `population_size` number of key values
/// where the values are all `byte_size` bytes
fn populate(db: &GroveDb, population_size: u16, byte_size: usize) {
    for i in 0u16..population_size {
        let key = i.to_be_bytes().to_vec();
        let value = generate_random_bytes(byte_size);

        let op = GroveDbOp {
            path: grovedb::batch::KeyInfoPath(vec![]),
            key: KeyInfo::KnownKey(key.to_vec()),
            op: grovedb::batch::Op::Insert {
                element: Element::Item(value, None),
            },
        };

        // Insert key-value
        let _cost_context = db
            .apply_batch(vec![op], None, None)
            .map_err(|e| println!("Insertion error: {}", e))
            .cost;
    }
}

fn main() {
    let byte_sizes = [5, 500, 5000];

    println!("\nINSERT INTO EMPTY ROOT");
    println!("Inserts one key-value into an empty root.\n");

    for byte_size in byte_sizes {
        // Start tests
        let mut total_duration = 0;
        let mut total_seek_count = 0;
        let mut total_bytes_added = 0;
        let mut total_replaced_bytes = 0;
        let mut total_removed_bytes = 0;
        let mut total_loaded_bytes = 0;
        let mut total_hash_node_calls = 0;
        let repetitions = 100;

        for _ in 0..repetitions {
            // Specify a path and open GroveDB at the path as db
            let tmp_dir = TempDir::new().unwrap();
            let db = GroveDb::open(tmp_dir.path()).unwrap();

            let key = generate_random_bytes(5);
            let value = generate_random_bytes(byte_size);

            let op = GroveDbOp {
                path: grovedb::batch::KeyInfoPath(vec![]),
                key: KeyInfo::KnownKey(key.to_vec()),
                op: grovedb::batch::Op::Insert {
                    element: Element::Item(value, None),
                },
            };

            let start_time = Instant::now();

            // Insert key-value
            let cost_context = db
                .apply_batch(vec![op], None, None)
                .map_err(|e| println!("Insertion error: {}", e))
                .cost;

            let duration = start_time.elapsed().as_micros();

            total_duration += duration;

            // Add operation counts
            total_seek_count += cost_context.seek_count;
            total_bytes_added += cost_context.storage_cost.added_bytes;
            total_replaced_bytes += cost_context.storage_cost.replaced_bytes;
            total_removed_bytes += cost_context
                .storage_cost
                .removed_bytes
                .total_removed_bytes();
            total_loaded_bytes += cost_context.storage_loaded_bytes;
            total_hash_node_calls += cost_context.hash_node_calls;
        }

        let average_time = total_duration / repetitions;

        println!(
            "Average insertion time over {} repetitions for {} bytes: {} micros",
            repetitions, byte_size, average_time
        );
        println!(
            "Average seek count: {}",
            total_seek_count / repetitions as u16
        );
        println!(
            "Average bytes added: {}",
            total_bytes_added / repetitions as u32
        );
        println!(
            "Average bytes replaced: {}",
            total_replaced_bytes / repetitions as u32
        );
        println!(
            "Average bytes removed: {}",
            total_removed_bytes / repetitions as u32
        );
        println!(
            "Average bytes loaded: {}",
            total_loaded_bytes / repetitions as u32
        );
        println!(
            "Average hash node calls: {}",
            total_hash_node_calls / repetitions as u32
        );
    }

    println!("\nINSERT INTO NON-EMPTY ROOT");
    println!("Inserts one key-value into an empty root then another key-value. The second insert is timed.\n");

    for byte_size in byte_sizes {
        // Start tests
        let mut total_duration = 0;
        let mut total_seek_count = 0;
        let mut total_bytes_added = 0;
        let mut total_replaced_bytes = 0;
        let mut total_removed_bytes = 0;
        let mut total_loaded_bytes = 0;
        let mut total_hash_node_calls = 0;
        let repetitions = 100;

        for _ in 0..repetitions {
            // Specify a path and open GroveDB at the path as db
            let tmp_dir = TempDir::new().unwrap();
            let db = GroveDb::open(tmp_dir.path()).unwrap();

            let key_1 = generate_random_bytes(5);
            let value_1 = generate_random_bytes(5);

            let op = GroveDbOp {
                path: grovedb::batch::KeyInfoPath(vec![]),
                key: KeyInfo::KnownKey(key_1.to_vec()),
                op: grovedb::batch::Op::Insert {
                    element: Element::Item(value_1, None),
                },
            };

            // Insert key-value
            let _cost_context = db
                .apply_batch(vec![op], None, None)
                .map_err(|e| println!("Insertion error: {}", e))
                .cost;

            let key_2 = generate_random_bytes(5);
            let value_2 = generate_random_bytes(byte_size);

            let op = GroveDbOp {
                path: grovedb::batch::KeyInfoPath(vec![]),
                key: KeyInfo::KnownKey(key_2.to_vec()),
                op: grovedb::batch::Op::Insert {
                    element: Element::Item(value_2, None),
                },
            };

            let start_time = Instant::now();

            // Insert key-value
            let cost_context = db
                .apply_batch(vec![op], None, None)
                .map_err(|e| println!("Insertion error: {}", e))
                .cost;

            let duration = start_time.elapsed().as_micros();

            total_duration += duration;

            // Add operation counts
            total_seek_count += cost_context.seek_count;
            total_bytes_added += cost_context.storage_cost.added_bytes;
            total_replaced_bytes += cost_context.storage_cost.replaced_bytes;
            total_removed_bytes += cost_context
                .storage_cost
                .removed_bytes
                .total_removed_bytes();
            total_loaded_bytes += cost_context.storage_loaded_bytes;
            total_hash_node_calls += cost_context.hash_node_calls;
        }

        let average_time = total_duration / repetitions;

        println!(
            "Average insertion time over {} repetitions for {} bytes: {} micros",
            repetitions, byte_size, average_time
        );
        println!(
            "Average seek count: {}",
            total_seek_count / repetitions as u16
        );
        println!(
            "Average bytes added: {}",
            total_bytes_added / repetitions as u32
        );
        println!(
            "Average bytes replaced: {}",
            total_replaced_bytes / repetitions as u32
        );
        println!(
            "Average bytes removed: {}",
            total_removed_bytes / repetitions as u32
        );
        println!(
            "Average bytes loaded: {}",
            total_loaded_bytes / repetitions as u32
        );
        println!(
            "Average hash node calls: {}",
            total_hash_node_calls / repetitions as u32
        );
    }

    println!("\nDELETIONS");
    println!(
        "Inserts one key-value into the root and then deletes it. Only the delete is timed.\n"
    );

    for byte_size in byte_sizes {
        // Start tests
        let mut total_duration = 0;
        let mut total_seek_count = 0;
        let mut total_bytes_added = 0;
        let mut total_replaced_bytes = 0;
        let mut total_removed_bytes = 0;
        let mut total_loaded_bytes = 0;
        let mut total_hash_node_calls = 0;
        let repetitions = 100;

        // Insert Key-Value Pairs
        for _ in 0..repetitions {
            // Specify a path and open GroveDB at the path as db
            let tmp_dir = TempDir::new().unwrap();
            let db = GroveDb::open(tmp_dir.path()).unwrap();

            let key = generate_random_bytes(5);
            let value = generate_random_bytes(byte_size);

            let op = GroveDbOp {
                path: grovedb::batch::KeyInfoPath(vec![]),
                key: KeyInfo::KnownKey(key.clone()),
                op: grovedb::batch::Op::Insert {
                    element: Element::Item(value, None),
                },
            };

            // Insert key-value into the root tree
            let _cost_context = db
                .apply_batch(vec![op], None, None)
                .map_err(|e| println!("Insertion error: {}", e))
                .cost;

            let op = GroveDbOp {
                path: grovedb::batch::KeyInfoPath(vec![]),
                key: KeyInfo::KnownKey(key.clone()),
                op: grovedb::batch::Op::Delete,
            };

            let start_time = Instant::now();

            let cost_context = db
                .apply_batch(vec![op], None, None)
                .map_err(|e| println!("Deletion error: {}", e))
                .cost;

            let duration = start_time.elapsed().as_micros();

            total_duration += duration;

            // Add operation counts
            total_seek_count += cost_context.seek_count;
            total_bytes_added += cost_context.storage_cost.added_bytes;
            total_replaced_bytes += cost_context.storage_cost.replaced_bytes;
            total_removed_bytes += cost_context
                .storage_cost
                .removed_bytes
                .total_removed_bytes();
            total_loaded_bytes += cost_context.storage_loaded_bytes;
            total_hash_node_calls += cost_context.hash_node_calls;
        }

        let average_time = total_duration / repetitions;

        println!(
            "Average deletion time over {} repetitions for {} bytes: {} micros",
            repetitions, byte_size, average_time
        );
        println!(
            "Average seek count: {}",
            total_seek_count / repetitions as u16
        );
        println!(
            "Average bytes added: {}",
            total_bytes_added / repetitions as u32
        );
        println!(
            "Average bytes replaced: {}",
            total_replaced_bytes / repetitions as u32
        );
        println!(
            "Average bytes removed: {}",
            total_removed_bytes / repetitions as u32
        );
        println!(
            "Average bytes loaded: {}",
            total_loaded_bytes / repetitions as u32
        );
        println!(
            "Average hash node calls: {}",
            total_hash_node_calls / repetitions as u32
        );
    }

    println!("\nQUERY 1, 100");
    println!("Populate the root tree with 100 values and query 1 of them\n");

    for byte_size in byte_sizes {
        // Specify a path and open GroveDB at the path as db
        let tmp_dir = TempDir::new().unwrap();
        let db = GroveDb::open(tmp_dir.path()).unwrap();

        // Populate GroveDB with values
        populate(&db, 100, byte_size);

        // Start tests
        let mut total_duration = 0;
        let mut total_seek_count = 0;
        let mut total_bytes_added = 0;
        let mut total_replaced_bytes = 0;
        let mut total_removed_bytes = 0;
        let mut total_loaded_bytes = 0;
        let mut total_hash_node_calls = 0;
        let repetitions = 100;

        for _ in 0..repetitions {
            // Define the path to the subtree we want to query.
            let path = vec![];

            // Instantiate a new query.
            let mut query = Query::new();

            // Insert a range of keys to the query that we would like returned.
            // In this case, we are asking for key 30.
            query.insert_keys(vec![30_u8.to_be_bytes().to_vec()]);

            // Put the query into a new unsized path query.
            let path_query = PathQuery::new_unsized(path, query.clone());

            let start_time = Instant::now();

            // Execute the query and collect the result items in "elements".
            let cost_context = db.query_item_value(&path_query, true, None).cost;

            let duration = start_time.elapsed().as_micros();

            total_duration += duration;

            // Add operation counts
            total_seek_count += cost_context.seek_count;
            total_loaded_bytes += cost_context.storage_loaded_bytes;
            total_hash_node_calls += cost_context.hash_node_calls;
            total_bytes_added += cost_context.storage_cost.added_bytes;
            total_removed_bytes += cost_context
                .storage_cost
                .removed_bytes
                .total_removed_bytes();
            total_replaced_bytes += cost_context.storage_cost.replaced_bytes;
        }

        let average_time = total_duration / repetitions;

        println!(
            "Average query time over {} repetitions for {} bytes: {} micros",
            repetitions, byte_size, average_time
        );
        println!(
            "Average seek count: {}",
            total_seek_count / repetitions as u16
        );
        println!(
            "Average bytes added: {}",
            total_bytes_added / repetitions as u32
        );
        println!(
            "Average bytes replaced: {}",
            total_replaced_bytes / repetitions as u32
        );
        println!(
            "Average bytes removed: {}",
            total_removed_bytes / repetitions as u32
        );
        println!(
            "Average bytes loaded: {}",
            total_loaded_bytes / repetitions as u32
        );
        println!(
            "Average hash node calls: {}",
            total_hash_node_calls / repetitions as u32
        );
    }

    println!("\nQUERY 5, 100");
    println!("Populate the root tree with 100 values and query 5 of them\n");

    for byte_size in byte_sizes {
        // Specify a path and open GroveDB at the path as db
        let tmp_dir = TempDir::new().unwrap();
        let db = GroveDb::open(tmp_dir.path()).unwrap();

        // Populate GroveDB with values
        populate(&db, 100, byte_size);

        // Start tests
        let mut total_duration = 0;
        let mut total_seek_count = 0;
        let mut total_bytes_added = 0;
        let mut total_replaced_bytes = 0;
        let mut total_removed_bytes = 0;
        let mut total_loaded_bytes = 0;
        let mut total_hash_node_calls = 0;
        let repetitions = 100;

        for _ in 0..repetitions {
            // Define the path to the subtree we want to query.
            let path = vec![];

            // Instantiate a new query.
            let mut query = Query::new();

            // In this case, we are asking for keys 30 through 34.
            query.insert_keys(vec![
                30_u8.to_be_bytes().to_vec(),
                31_u8.to_be_bytes().to_vec(),
                32_u8.to_be_bytes().to_vec(),
                33_u8.to_be_bytes().to_vec(),
                34_u8.to_be_bytes().to_vec(),
            ]);

            // Put the query into a new unsized path query.
            let path_query = PathQuery::new_unsized(path, query.clone());

            let start_time = Instant::now();

            // Execute the query and collect the result items in "elements".
            let cost_context = db.query_item_value(&path_query, true, None).cost;

            let duration = start_time.elapsed().as_micros();

            total_duration += duration;

            // Add operation counts
            total_seek_count += cost_context.seek_count;
            total_loaded_bytes += cost_context.storage_loaded_bytes;
            total_hash_node_calls += cost_context.hash_node_calls;
            total_bytes_added += cost_context.storage_cost.added_bytes;
            total_removed_bytes += cost_context
                .storage_cost
                .removed_bytes
                .total_removed_bytes();
            total_replaced_bytes += cost_context.storage_cost.replaced_bytes;
        }

        let average_time = total_duration / repetitions;

        println!(
            "Average query time over {} repetitions for {} bytes: {} micros",
            repetitions, byte_size, average_time
        );
        println!(
            "Average seek count: {}",
            total_seek_count / repetitions as u16
        );
        println!(
            "Average bytes added: {}",
            total_bytes_added / repetitions as u32
        );
        println!(
            "Average bytes replaced: {}",
            total_replaced_bytes / repetitions as u32
        );
        println!(
            "Average bytes removed: {}",
            total_removed_bytes / repetitions as u32
        );
        println!(
            "Average bytes loaded: {}",
            total_loaded_bytes / repetitions as u32
        );
        println!(
            "Average hash node calls: {}",
            total_hash_node_calls / repetitions as u32
        );
    }

    println!("\nQUERY 5, 1000");
    println!("Populate the root tree with 1000 values and query 5 of them\n");

    for byte_size in byte_sizes {
        // Specify a path and open GroveDB at the path as db
        let tmp_dir = TempDir::new().unwrap();
        let db = GroveDb::open(tmp_dir.path()).unwrap();

        // Populate GroveDB with values
        populate(&db, 1000, byte_size);

        // Start tests
        let mut total_duration = 0;
        let mut total_seek_count = 0;
        let mut total_bytes_added = 0;
        let mut total_replaced_bytes = 0;
        let mut total_removed_bytes = 0;
        let mut total_loaded_bytes = 0;
        let mut total_hash_node_calls = 0;
        let repetitions = 100;

        for _ in 0..repetitions {
            // Define the path to the subtree we want to query.
            let path = vec![];

            // Instantiate a new query.
            let mut query = Query::new();

            // Insert a range of keys to the query that we would like returned.
            // In this case, we are asking for keys 30 through 34.
            query.insert_keys(vec![
                30_u8.to_be_bytes().to_vec(),
                31_u8.to_be_bytes().to_vec(),
                32_u8.to_be_bytes().to_vec(),
                33_u8.to_be_bytes().to_vec(),
                34_u8.to_be_bytes().to_vec(),
            ]);

            // Put the query into a new unsized path query.
            let path_query = PathQuery::new_unsized(path, query.clone());

            let start_time = Instant::now();

            // Execute the query and collect the result items in "elements".
            let cost_context = db.query_item_value(&path_query, true, None).cost;

            let duration = start_time.elapsed().as_micros();

            total_duration += duration;

            // Add operation counts
            total_seek_count += cost_context.seek_count;
            total_loaded_bytes += cost_context.storage_loaded_bytes;
            total_hash_node_calls += cost_context.hash_node_calls;
            total_bytes_added += cost_context.storage_cost.added_bytes;
            total_removed_bytes += cost_context
                .storage_cost
                .removed_bytes
                .total_removed_bytes();
            total_replaced_bytes += cost_context.storage_cost.replaced_bytes;
        }

        let average_time = total_duration / repetitions;

        println!(
            "Average query time over {} repetitions for {} bytes: {} micros",
            repetitions, byte_size, average_time
        );
        println!(
            "Average seek count: {}",
            total_seek_count / repetitions as u16
        );
        println!(
            "Average bytes added: {}",
            total_bytes_added / repetitions as u32
        );
        println!(
            "Average bytes replaced: {}",
            total_replaced_bytes / repetitions as u32
        );
        println!(
            "Average bytes removed: {}",
            total_removed_bytes / repetitions as u32
        );
        println!(
            "Average bytes loaded: {}",
            total_loaded_bytes / repetitions as u32
        );
        println!(
            "Average hash node calls: {}",
            total_hash_node_calls / repetitions as u32
        );
    }

    println!("\nQUERY 5, 5000");
    println!("Populate the root tree with 5000 values and query 5 of them\n");

    for byte_size in byte_sizes {
        // Specify a path and open GroveDB at the path as db
        let tmp_dir = TempDir::new().unwrap();
        let db = GroveDb::open(tmp_dir.path()).unwrap();

        // Populate GroveDB with values
        populate(&db, 5000, byte_size);

        // Start tests
        let mut total_duration = 0;
        let mut total_seek_count: u32 = 0;
        let mut total_bytes_added = 0;
        let mut total_replaced_bytes = 0;
        let mut total_removed_bytes = 0;
        let mut total_loaded_bytes = 0;
        let mut total_hash_node_calls = 0;
        let repetitions = 100;

        for _ in 0..repetitions {
            // Define the path to the subtree we want to query.
            let path = vec![];

            // Instantiate a new query.
            let mut query = Query::new();

            // Insert a range of keys to the query that we would like returned.
            // In this case, we are asking for keys 30 through 34.
            query.insert_keys(vec![
                30_u8.to_be_bytes().to_vec(),
                31_u8.to_be_bytes().to_vec(),
                32_u8.to_be_bytes().to_vec(),
                33_u8.to_be_bytes().to_vec(),
                34_u8.to_be_bytes().to_vec(),
            ]);

            // Put the query into a new unsized path query.
            let path_query = PathQuery::new_unsized(path, query.clone());

            let start_time = Instant::now();

            // Execute the query and collect the result items in "elements".
            let cost_context = db.query_item_value(&path_query, true, None).cost;

            let duration = start_time.elapsed().as_micros();

            total_duration += duration;

            // Add operation counts
            total_seek_count += cost_context.seek_count as u32;
            total_loaded_bytes += cost_context.storage_loaded_bytes;
            total_hash_node_calls += cost_context.hash_node_calls;
            total_bytes_added += cost_context.storage_cost.added_bytes;
            total_removed_bytes += cost_context
                .storage_cost
                .removed_bytes
                .total_removed_bytes();
            total_replaced_bytes += cost_context.storage_cost.replaced_bytes;
        }

        let average_time = total_duration / repetitions;

        println!(
            "Average query time over {} repetitions for {} bytes: {} micros",
            repetitions, byte_size, average_time
        );
        println!(
            "Average seek count: {}",
            total_seek_count / repetitions as u32
        );
        println!(
            "Average bytes added: {}",
            total_bytes_added / repetitions as u32
        );
        println!(
            "Average bytes replaced: {}",
            total_replaced_bytes / repetitions as u32
        );
        println!(
            "Average bytes removed: {}",
            total_removed_bytes / repetitions as u32
        );
        println!(
            "Average bytes loaded: {}",
            total_loaded_bytes / repetitions as u32
        );
        println!(
            "Average hash node calls: {}",
            total_hash_node_calls / repetitions as u32
        );
    }

    println!("\nQUERY 5, 10000");
    println!("Populate the root tree with 10000 values and query 5 of them\n");

    for byte_size in byte_sizes {
        // Specify a path and open GroveDB at the path as db
        let tmp_dir = TempDir::new().unwrap();
        let db = GroveDb::open(tmp_dir.path()).unwrap();

        // Populate GroveDB with values
        populate(&db, 10000, byte_size);

        // Start tests
        let mut total_duration = 0;
        let mut total_seek_count: u32 = 0;
        let mut total_bytes_added = 0;
        let mut total_replaced_bytes = 0;
        let mut total_removed_bytes = 0;
        let mut total_loaded_bytes = 0;
        let mut total_hash_node_calls = 0;
        let repetitions = 100;

        for _ in 0..repetitions {
            // Define the path to the subtree we want to query.
            let path = vec![];

            // Instantiate a new query.
            let mut query = Query::new();

            // Insert a range of keys to the query that we would like returned.
            // In this case, we are asking for keys 30 through 34.
            query.insert_keys(vec![
                30_u8.to_be_bytes().to_vec(),
                31_u8.to_be_bytes().to_vec(),
                32_u8.to_be_bytes().to_vec(),
                33_u8.to_be_bytes().to_vec(),
                34_u8.to_be_bytes().to_vec(),
            ]);

            // Put the query into a new unsized path query.
            let path_query = PathQuery::new_unsized(path, query.clone());

            let start_time = Instant::now();

            // Execute the query and collect the result items in "elements".
            let cost_context = db.query_item_value(&path_query, true, None).cost;

            let duration = start_time.elapsed().as_micros();

            total_duration += duration;

            // Add operation counts
            total_seek_count += cost_context.seek_count as u32;
            total_loaded_bytes += cost_context.storage_loaded_bytes;
            total_hash_node_calls += cost_context.hash_node_calls;
            total_bytes_added += cost_context.storage_cost.added_bytes;
            total_removed_bytes += cost_context
                .storage_cost
                .removed_bytes
                .total_removed_bytes();
            total_replaced_bytes += cost_context.storage_cost.replaced_bytes;
        }

        let average_time = total_duration / repetitions;

        println!(
            "Average query time over {} repetitions for {} bytes: {} micros",
            repetitions, byte_size, average_time
        );
        println!(
            "Average seek count: {}",
            total_seek_count / repetitions as u32
        );
        println!(
            "Average bytes added: {}",
            total_bytes_added / repetitions as u32
        );
        println!(
            "Average bytes replaced: {}",
            total_replaced_bytes / repetitions as u32
        );
        println!(
            "Average bytes removed: {}",
            total_removed_bytes / repetitions as u32
        );
        println!(
            "Average bytes loaded: {}",
            total_loaded_bytes / repetitions as u32
        );
        println!(
            "Average hash node calls: {}",
            total_hash_node_calls / repetitions as u32
        );
    }
}
