//! A Merk restored from chunks through the handles of an engine (MERK-07).

use grovedb_merk::{
    element::costs::ElementCostExtensions, ChunkProducer, Merk, Restorer, TreeType,
};

use super::{check, version, ROOT};
use crate::{
    batch::QualifiedGroveDbOp,
    engine::{Engine, SubtreeHandle},
    Element, GroveDb,
};

const ITEMS: u32 = 300;

fn key(n: u32) -> Vec<u8> {
    n.wrapping_mul(0x9e37_79b9).to_be_bytes().to_vec()
}

fn item(n: u32) -> Element {
    Element::new_item(vec![n as u8; 1 + (n % 90) as usize])
}

fn root_merk<'db, S: SubtreeHandle<'db>>(store: S) -> Merk<S> {
    Merk::open_base(
        store,
        TreeType::NormalTree,
        Some(&Element::value_defined_cost_for_serialized_value),
        version(),
    )
    .unwrap()
    .expect("the root Merk opens")
}

/// Writes [`ITEMS`] items into the root tree of a grove on `E`, and
/// restores the root Merk of a second grove on `E` from the chunks of the
/// first: the chunks come from a deferred handle of the first engine, the
/// restore writes through an immediate handle of the second, in a
/// transaction that is then committed. The second grove gives the root
/// hash of the first and each of its items. Returns the directory of the
/// second grove, which is closed, and what each chunk and the end of the
/// restore answered.
fn a_root_merk_restored<E: Engine>() -> (tempfile::TempDir, Vec<String>) {
    let v = version();
    let source_dir = tempfile::tempdir().expect("a directory");
    let source = GroveDb::<E>::open_with_engine(source_dir.path()).expect("a database");
    let ops = (0..ITEMS)
        .map(|n| QualifiedGroveDbOp::insert_or_replace_op(vec![], key(n), item(n)))
        .collect();
    source
        .apply_batch(ops, None, None, v)
        .unwrap()
        .expect("the items are written");
    let root_hash = source.root_hash(None, v).unwrap().expect("a root");

    let reading = source.engine().start_transaction();
    let from = root_merk(source.engine().subtree(ROOT, None, &reading).unwrap());
    assert_eq!(from.root_hash().unwrap(), root_hash);
    let mut producer = ChunkProducer::new(&from).expect("a producer");

    let dir = tempfile::tempdir().expect("a directory");
    let target = GroveDb::<E>::open_with_engine(dir.path()).expect("a database");
    let engine = target.engine();
    let transaction = engine.start_transaction();
    let store = engine.immediate_subtree(ROOT, &transaction).unwrap();
    let mut restorer = Restorer::new(root_merk(store), root_hash, None);
    let mut said = Vec::new();
    let mut pending = vec![vec![]];
    while let Some(chunk_id) = pending.pop() {
        let (chunk, _) = producer.chunk(&chunk_id, v).expect("a chunk");
        let answered = restorer.process_chunk(&chunk_id, chunk, v);
        said.push(format!("{chunk_id:?}: {answered:?}"));
        pending.extend(answered.expect("the chunk is taken"));
    }
    assert!(said.len() > 8, "{} chunks", said.len());
    let restored = restorer
        .finalize_with_grovedb_elements(v)
        .expect("the restore finishes");
    let root = restored.root_hash_key_and_aggregate_data().unwrap();
    said.push(format!("finalized: {root:?}"));
    drop(restored);
    engine
        .commit_transaction(transaction)
        .unwrap()
        .expect("a commit");

    assert_eq!(
        target.root_hash(None, v).unwrap().expect("a root"),
        root_hash
    );
    for n in 0..ITEMS {
        let read = target.get(ROOT, &key(n), None, v).unwrap();
        assert_eq!(read.expect("an item of the first grove"), item(n));
    }
    drop(target);
    (dir, said)
}

/// The physical checker finds the grove whose root Merk was restored
/// sound. Where the RocksDB engine is compiled in, each chunk and the end
/// of the restore answer on LigninDB what they answer on RocksDB.
#[test]
fn a_root_merk_is_restored_from_chunks_through_the_engine() {
    let (dir, said) = a_root_merk_restored::<super::LigninEngine>();
    check(dir.path());

    #[cfg(feature = "rocksdb_storage")]
    {
        let (_dir, on_rocksdb) = a_root_merk_restored::<crate::RocksDbEngine>();
        assert_eq!(said, on_rocksdb);
    }
    #[cfg(not(feature = "rocksdb_storage"))]
    let _ = said;
}
