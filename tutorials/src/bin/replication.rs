use std::collections::VecDeque;
use std::path::Path;
use grovedb::{operations::insert::InsertOptions, Element, GroveDb, PathQuery, Query, Transaction, StateSyncInfo};
use grovedb::reference_path::ReferencePathType;
use rand::{distributions::Alphanumeric, Rng, };
use grovedb::element::SumValue;
use grovedb_path::{SubtreePath};

const MAIN_ΚΕΥ: &[u8] = b"key_main";
const MAIN_ΚΕΥ_EMPTY: &[u8] = b"key_main_empty";

const KEY_INT_0: &[u8] = b"key_int_0";
const KEY_INT_REF_0: &[u8] = b"key_int_ref_0";
const KEY_INT_A: &[u8] = b"key_sum_0";
const ROOT_PATH: &[&[u8]] = &[];

// Allow insertions to overwrite trees
// This is necessary so the tutorial can be rerun easily
const INSERT_OPTIONS: Option<InsertOptions> = Some(InsertOptions {
    validate_insertion_does_not_override: false,
    validate_insertion_does_not_override_tree: false,
    base_root_storage_is_free: true,
});

fn populate_db(grovedb_path: String) -> GroveDb {
    let db = GroveDb::open(grovedb_path).unwrap();

    insert_empty_tree_db(&db, ROOT_PATH, MAIN_ΚΕΥ);
    insert_empty_tree_db(&db, ROOT_PATH, MAIN_ΚΕΥ_EMPTY);
    insert_empty_tree_db(&db, &[MAIN_ΚΕΥ], KEY_INT_0);

    let tx = db.start_transaction();
    let batch_size = 100;
    for i in 0..=10 {
        insert_range_values_db(&db, &[MAIN_ΚΕΥ, KEY_INT_0], i * batch_size, i * batch_size + batch_size - 1, &tx);
    }
    let _ = db.commit_transaction(tx);

    insert_empty_tree_db(&db, &[MAIN_ΚΕΥ], KEY_INT_REF_0);

    let tx_2 = db.start_transaction();
    insert_range_ref_double_values_db(&db, &[MAIN_ΚΕΥ, KEY_INT_REF_0], KEY_INT_0, 1, 50, &tx_2);
    let _ = db.commit_transaction(tx_2);

    insert_empty_sum_tree_db(&db, &[MAIN_ΚΕΥ], KEY_INT_A);

    let tx_3 = db.start_transaction();
    insert_range_values_db(&db, &[MAIN_ΚΕΥ, KEY_INT_A], 1, 100, &tx_3);
    insert_sum_element_db(&db, &[MAIN_ΚΕΥ, KEY_INT_A], 101, 150, &tx_3);
    let _ = db.commit_transaction(tx_3);
    db
}

fn create_empty_db(grovedb_path: String) -> GroveDb   {
    let db = GroveDb::open(grovedb_path).unwrap();
    db
}

fn main() {
    let path_source = generate_random_path("../tutorial-storage/", "/db_0", 24);
    let db_source = populate_db(path_source.clone());

    let checkpoint_dir = path_source + "/checkpoint";
    let path_checkpoint = Path::new(checkpoint_dir.as_str());

    db_source.create_checkpoint(&path_checkpoint).expect("cannot create checkpoint");
    let db_checkpoint_0 = GroveDb::open(path_checkpoint).expect("cannot open groveDB from checkpoint");

    let path_destination = generate_random_path("../tutorial-storage/", "/db_copy", 24);
    let db_destination = create_empty_db(path_destination.clone());

    println!("\n######### root_hashes:");
    let root_hash_0 = db_source.root_hash(None).unwrap().unwrap();
    println!("root_hash_0: {:?}", hex::encode(root_hash_0));
    let root_hash_checkpoint_0 = db_checkpoint_0.root_hash(None).unwrap().unwrap();
    println!("root_hash_checkpoint_0: {:?}", hex::encode(root_hash_checkpoint_0));
    let root_hash_copy = db_destination.root_hash(None).unwrap().unwrap();
    println!("root_hash_copy: {:?}", hex::encode(root_hash_copy));

    println!("\n######### source_subtree_metadata");
    let source_tx = db_source.start_transaction();
    let subtrees_metadata = db_source.get_subtrees_metadata(&SubtreePath::empty(), &source_tx).unwrap();
    println!("{:?}", subtrees_metadata);

    println!("\n######### db_checkpoint_0 -> db_copy state sync");
    let state_info = db_destination.create_state_sync_info();
    let source_tx = db_source.start_transaction();
    let target_tx = db_destination.start_transaction();
    sync_db_demo(&db_checkpoint_0, &db_destination, state_info, &source_tx, &target_tx).unwrap();
    db_destination.commit_transaction(target_tx).unwrap().expect("expected to commit transaction");

    println!("\n######### verify db_copy");
    let incorrect_hashes = db_destination.verify_grovedb(None).unwrap();
    if incorrect_hashes.len() > 0 {
        println!("DB verification failed!");
    }
    else {
        println!("DB verification success");
    }

    println!("\n######### root_hashes:");
    let root_hash_0 = db_source.root_hash(None).unwrap().unwrap();
    println!("root_hash_0: {:?}", hex::encode(root_hash_0));
    let root_hash_checkpoint_0 = db_checkpoint_0.root_hash(None).unwrap().unwrap();
    println!("root_hash_checkpoint_0: {:?}", hex::encode(root_hash_checkpoint_0));
    let root_hash_copy = db_destination.root_hash(None).unwrap().unwrap();
    println!("root_hash_copy: {:?}", hex::encode(root_hash_copy));

    let query_path = &[MAIN_ΚΕΥ, KEY_INT_0];
    let query_key = (20487u32).to_be_bytes().to_vec();
    println!("\n######## Query on db_checkpoint_0:");
    query_db(&db_checkpoint_0, query_path, query_key.clone());
    println!("\n######## Query on db_copy:");
    query_db(&db_destination, query_path, query_key.clone());

    return;

}

fn insert_empty_tree_db(db: &GroveDb, path: &[&[u8]], key: &[u8])
{
    db.insert(path, key, Element::empty_tree(), INSERT_OPTIONS, None)
        .unwrap()
        .expect("successfully inserted tree");
}
fn insert_range_values_db(db: &GroveDb, path: &[&[u8]], min_i: u32, max_i: u32, transaction: &Transaction)
{
    for i in min_i..=max_i {
        let i_vec = i.to_be_bytes().to_vec();
        db.insert(
            path,
            &i_vec,
            Element::new_item(i_vec.to_vec()),
            INSERT_OPTIONS,
            Some(&transaction),
        )
            .unwrap()
            .expect("successfully inserted values");
    }
}

fn insert_range_ref_double_values_db(db: &GroveDb, path: &[&[u8]], ref_key: &[u8], min_i: u32, max_i: u32, transaction: &Transaction)
{
    for i in min_i..=max_i {
        let i_vec = i.to_be_bytes().to_vec();
        let value = i * 2;
        let value_vec = value.to_be_bytes().to_vec();
        db.insert(
            path,
            &i_vec,
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                MAIN_ΚΕΥ.to_vec(),
                ref_key.to_vec(),
                value_vec.to_vec()
            ])),
            INSERT_OPTIONS,
            Some(&transaction),
        )
            .unwrap()
            .expect("successfully inserted values");
    }
}

fn insert_empty_sum_tree_db(db: &GroveDb, path: &[&[u8]], key: &[u8])
{
    db.insert(path, key, Element::empty_sum_tree(), INSERT_OPTIONS, None)
        .unwrap()
        .expect("successfully inserted tree");
}
fn insert_sum_element_db(db: &GroveDb, path: &[&[u8]], min_i: u32, max_i: u32, transaction: &Transaction)
{
    for i in min_i..=max_i {
        //let value : u32 = i;
        let value = i as u64;
        //let value: u64 = 1;
        let i_vec = i.to_be_bytes().to_vec();
        db.insert(
            path,
            &i_vec,
            Element::new_sum_item(value as SumValue),
            INSERT_OPTIONS,
            Some(&transaction),
        )
            .unwrap()
            .expect("successfully inserted values");
    }
}
fn generate_random_path(prefix: &str, suffix: &str, len: usize) -> String {
    let random_string: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect();
    format!("{}{}{}", prefix, random_string, suffix)
}

fn query_db(db: &GroveDb, path: &[&[u8]], key: Vec<u8>) {
    let path_vec: Vec<Vec<u8>> = path.iter()
        .map(|&slice| slice.to_vec())
        .collect();

    let mut query = Query::new();
    query.insert_key(key);

   let path_query = PathQuery::new_unsized(path_vec, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");
    for e in elements.into_iter() {
        println!(">> {:?}", e);
    }

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    // Get hash from query proof and print to terminal along with GroveDB root hash.
    let (verify_hash, _) = GroveDb::verify_query(&proof, &path_query).unwrap();
    println!("verify_hash: {:?}", hex::encode(verify_hash));
    if verify_hash == db.root_hash(None).unwrap().unwrap() {
        println!("Query verified");
    } else { println!("Verification FAILED"); };
}

fn sync_db_demo(
    source_db: &GroveDb,
    target_db: &GroveDb,
    state_sync_info: StateSyncInfo,
    source_tx: &Transaction,
    target_tx: &Transaction,
) -> Result<(), grovedb::Error> {
    let app_hash = source_db.root_hash(None).value.unwrap();
    let (chunk_ids, mut state_sync_info) = target_db.start_snapshot_syncing(state_sync_info, app_hash, target_tx)?;

    let mut chunk_queue : VecDeque<Vec<u8>> = VecDeque::new();

    chunk_queue.extend(chunk_ids);

    while let Some(chunk_id) = chunk_queue.pop_front() {
        let ops = source_db.fetch_chunk(chunk_id.as_slice(), source_tx)?;
        let (more_chunks, new_state_sync_info) = target_db.apply_chunk(state_sync_info, (chunk_id.as_slice(), ops), target_tx)?;
        state_sync_info = new_state_sync_info;
        chunk_queue.extend(more_chunks);
    }

    Ok(())
}

