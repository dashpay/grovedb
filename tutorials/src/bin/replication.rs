use std::collections::VecDeque;
use std::ops::Range;
use std::path::Path;
use grovedb::{operations::insert::InsertOptions, Element, GroveDb, PathQuery, Query, Transaction};
use grovedb::reference_path::ReferencePathType;
use rand::{distributions::Alphanumeric, Rng, thread_rng};
use rand::prelude::SliceRandom;
use grovedb::element::SumValue;
use grovedb::query_result_type::QueryResultType;
use grovedb_merk::{BatchEntry, ChunkProducer, CryptoHash, Error, Op};
use grovedb_merk::Error::{EdError, StorageError};
use grovedb_merk::proofs::chunk::error::ChunkError;
use grovedb_merk::Restorer;
use grovedb_merk::tree::kv::ValueDefinedCostType;
use grovedb_merk::tree::{RefWalker, TreeNode};
use grovedb_merk::TreeFeatureType::BasicMerkNode;
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use grovedb_storage::{StorageBatch, StorageContext};
use grovedb_storage::rocksdb_storage::PrefixedRocksDbStorageContext;
use grovedb_visualize::Visualize;

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
    let path_0 = generate_random_path("../tutorial-storage/", "/db_0", 24);
    let db_0 = populate_db(path_0.clone());
    let checkpoint_dir = path_0 + "/checkpoint";
    let path_checkpoint = Path::new(checkpoint_dir.as_str());
    db_0.create_checkpoint(&path_checkpoint).expect("cannot create checkpoint");
    let db_checkpoint_0 = GroveDb::open(path_checkpoint).expect("cannot open grovedb from checkpoint");

    let path_copy = generate_random_path("../tutorial-storage/", "/db_copy", 24);
    let mut db_copy = create_empty_db(path_copy.clone());

    println!("\n######### root_hashes:");
    let root_hash_0 = db_0.root_hash(None).unwrap().unwrap();
    println!("root_hash_0: {:?}", hex::encode(root_hash_0));
    let root_hash_checkpoint_0 = db_checkpoint_0.root_hash(None).unwrap().unwrap();
    println!("root_hash_checkpoint_0: {:?}", hex::encode(root_hash_checkpoint_0));
    let root_hash_copy = db_copy.root_hash(None).unwrap().unwrap();
    println!("root_hash_copy: {:?}", hex::encode(root_hash_copy));

    println!("\n######### db_checkpoint_0 -> db_copy state sync");
    db_copy.w_sync_db_demo(&db_checkpoint_0).unwrap();

    println!("\n######### root_hashes:");
    let root_hash_0 = db_0.root_hash(None).unwrap().unwrap();
    println!("root_hash_0: {:?}", hex::encode(root_hash_0));
    let root_hash_checkpoint_0 = db_checkpoint_0.root_hash(None).unwrap().unwrap();
    println!("root_hash_checkpoint_0: {:?}", hex::encode(root_hash_checkpoint_0));
    let root_hash_copy = db_copy.root_hash(None).unwrap().unwrap();
    println!("root_hash_copy: {:?}", hex::encode(root_hash_copy));

    let query_path = &[MAIN_ΚΕΥ, KEY_INT_0];
    let query_key = (20487u32).to_be_bytes().to_vec();
    println!("\n######## Query on db_checkpoint_0:");
    query_db(&db_checkpoint_0, query_path, query_key.clone());
    println!("\n######## Query on db_copy:");
    query_db(&db_copy, query_path, query_key.clone());

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
        //let be_num = u32::from_be_bytes(e.try_into().expect("Slice with incorrect length"));
        println!(">> {:?}", e);
    }

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    // Get hash from query proof and print to terminal along with GroveDB root hash.
    let (verify_hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    println!("verify_hash: {:?}", hex::encode(verify_hash));
    if verify_hash == db.root_hash(None).unwrap().unwrap() {
        println!("Query verified");
    } else { println!("Verification FAILED"); };
}

