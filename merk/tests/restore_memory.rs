//! How much memory the restore of a Merk holds on the LigninDB store,
//! which is handed a restored Merk whole (LigninDB task MERK-07).
//!
//! The chunks of a restore are kept in memory until the Merk is whole, and
//! the transaction then holds every node again in its write set. This
//! measures both, as the resident memory of a process that does nothing
//! else. It is no test of a result: it is ignored, and is run by hand, in
//! steps that are processes of their own, so that the memory of the Merk
//! the chunks come from is not counted:
//!
//! ```text
//! RESTORE_MEMORY=produce RESTORE_MEMORY_DIR=<dir> RESTORE_MEMORY_NODES=1000000 \
//!   RESTORE_MEMORY_VALUE=400 cargo test -p grovedb-merk --features lignin_storage \
//!   --test restore_memory -- --ignored --nocapture
//! RESTORE_MEMORY=restore RESTORE_MEMORY_DIR=<dir> cargo test ... (as above)
//! RESTORE_MEMORY=restore-rocksdb RESTORE_MEMORY_DIR=<dir> cargo test ... (as above)
//! ```
//!
//! The first step builds a Merk of items on RocksDB and writes its chunks
//! to `<dir>/chunks`. The second restores a Merk from that file in a write
//! transaction of a LigninDB store and prints, in bytes and for each node,
//! what the process holds more than before the first chunk: once every
//! chunk has been processed, at most up to the end of `finalize`, and
//! afterwards. The third does the same in a transaction of RocksDB, which
//! takes the chunks as they arrive.

#![cfg(all(
    target_os = "linux",
    feature = "lignin_storage",
    feature = "rocksdb_storage"
))]

use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    time::Instant,
};

use grovedb_element::Element;
use grovedb_merk::{
    proofs::{
        chunk::{error::ChunkError, util::generate_traversal_instruction_as_vec_bytes},
        encode_into, Decoder, Node, Op as ProofOp,
    },
    store::{
        lignin::{LigninNodeStore, LigninSubtree, LigninWriter},
        NodeStore,
    },
    tree::kv::ValueDefinedCostType,
    ChunkProducer, CryptoHash, Error, Merk, Op, Restorer, TreeFeatureType, TreeType,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage};
use grovedb_version::version::GroveVersion;
use lignindb::txn::testing::Db;

type ValueDefinedCost = fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>;

const NO_VALUE_DEFINED_COST: Option<ValueDefinedCost> = None;

fn version() -> &'static GroveVersion {
    GroveVersion::latest()
}

fn setting(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn number(name: &str, default: u64) -> u64 {
    setting(name).map_or(default, |value| value.parse().expect("a number"))
}

fn directory() -> PathBuf {
    PathBuf::from(setting("RESTORE_MEMORY_DIR").expect("RESTORE_MEMORY_DIR names a directory"))
}

/// A line of `/proc/self/status`, in bytes.
fn status(line: &str) -> u64 {
    let status = fs::read_to_string("/proc/self/status").expect("the status of the process");
    let found = status.lines().find_map(|at| at.strip_prefix(line));
    let kilobytes = found.and_then(|rest| rest.split_whitespace().next());
    kilobytes.expect("the line").parse::<u64>().expect("a size") * 1024
}

fn resident() -> u64 {
    status("VmRSS:")
}

fn most_resident() -> u64 {
    status("VmHWM:")
}

fn open<'db, S: NodeStore<'db>>(store: S) -> Merk<S> {
    Merk::open_base(
        store,
        TreeType::NormalTree,
        NO_VALUE_DEFINED_COST,
        version(),
    )
    .unwrap()
    .expect("the Merk opens")
}

/// Builds a Merk of `nodes` items of `value` bytes on RocksDB and writes
/// its root hash and its chunks to the directory.
fn produce(nodes: u64, value: usize) {
    let began = Instant::now();
    let storage = TempStorage::new();
    let transaction = storage.start_transaction();
    let context = || {
        storage
            .get_immediate_storage_context(SubtreePath::empty(), &transaction)
            .unwrap()
    };
    let mut merk = open(context());
    let mut next = 0;
    while next < nodes {
        let upto = nodes.min(next + 20_000);
        let batch: Vec<(Vec<u8>, Op)> = (next..upto)
            .map(|n| {
                let mut bytes = n.to_le_bytes().to_vec();
                bytes.resize(value, n as u8);
                let item = Element::new_item(bytes).serialize(version());
                let put = Op::Put(item.expect("an item"), TreeFeatureType::BasicMerkNode);
                (n.to_be_bytes().to_vec(), put)
            })
            .collect();
        merk.apply::<_, Vec<u8>>(&batch, &[], None, version())
            .unwrap()
            .expect("the batch applies");
        next = upto;
    }
    let root_hash = merk.root_hash().unwrap();
    println!(
        "RESTORE-MEMORY source: {nodes} nodes of {value} bytes, height {:?}, {:?}",
        merk.height(),
        began.elapsed()
    );

    let directory = directory();
    fs::create_dir_all(&directory).expect("the directory");
    fs::write(directory.join("root"), root_hash).expect("the root hash");
    let mut file = BufWriter::new(File::create(directory.join("chunks")).expect("the file"));
    let (mut chunks, mut bytes) = (0u64, 0u64);
    let height = merk.height().expect("the Merk has nodes") as usize;
    // The producer numbers the chunks a tree of this height has when every
    // node of it is there. A number that names a place with no node is
    // refused, and no restore asks for that chunk.
    let numbered = ChunkProducer::new(&merk).expect("the Merk has nodes").len();
    for index in 1..=numbered {
        // A Merk on RocksDB keeps every node it fetched: it is opened
        // again from time to time.
        if index % 2_000 == 0 {
            merk = open(context());
        }
        let mut producer = ChunkProducer::new(&merk).expect("the Merk has nodes");
        let chunk = match producer.chunk_with_index(index, version()) {
            Ok((chunk, _)) => chunk,
            Err(Error::ChunkingError(ChunkError::BadTraversalInstruction(_))) => continue,
            Err(error) => panic!("the chunk {index}: {error:?}"),
        };
        let id = generate_traversal_instruction_as_vec_bytes(height, index).expect("an id");
        let mut encoded = Vec::new();
        encode_into(chunk.iter(), &mut encoded);
        for part in [id.as_slice(), encoded.as_slice()] {
            file.write_all(&(part.len() as u32).to_le_bytes())
                .expect("a length");
            file.write_all(part).expect("a part");
        }
        chunks += 1;
        bytes += encoded.len() as u64;
    }
    file.flush().expect("the file is written");
    println!(
        "RESTORE-MEMORY chunks: {chunks} chunks of {bytes} bytes, {:?}",
        began.elapsed()
    );
}

/// Where each chunk is in the file of chunks, by its id.
fn index(file: &mut BufReader<File>) -> BTreeMap<Vec<u8>, (u64, usize)> {
    let mut index = BTreeMap::new();
    let mut at = 0u64;
    let mut length = [0u8; 4];
    while file.read_exact(&mut length).is_ok() {
        let mut id = vec![0; u32::from_le_bytes(length) as usize];
        file.read_exact(&mut id).expect("an id");
        file.read_exact(&mut length).expect("a length");
        let chunk = u32::from_le_bytes(length) as usize;
        at += 8 + id.len() as u64;
        index.insert(id, (at, chunk));
        at += chunk as u64;
        file.seek(SeekFrom::Start(at)).expect("the next chunk");
    }
    index
}

/// Restores the Merk whose chunks the directory holds into `store`, and
/// prints what the process held: before the first chunk, once every chunk
/// has been processed, at most up to the end of `finalize`, and after it.
/// Returns how many nodes the Merk has.
fn restore_into<'db, S: NodeStore<'db>>(engine: &str, store: S) -> u64 {
    let directory = directory();
    let root_hash: CryptoHash = fs::read(directory.join("root"))
        .expect("the root hash")
        .try_into()
        .expect("a hash");
    let mut file = BufReader::new(File::open(directory.join("chunks")).expect("the file"));
    let index = index(&mut file);
    let mut restorer = Restorer::new(open(store), root_hash, None);

    let began = Instant::now();
    let before = resident();
    let mut pending = vec![Vec::new()];
    let mut encoded = Vec::new();
    let mut nodes = 0u64;
    while let Some(chunk_id) = pending.pop() {
        let (at, length) = index[&chunk_id];
        encoded.resize(length, 0);
        file.seek(SeekFrom::Start(at)).expect("the chunk");
        file.read_exact(&mut encoded).expect("the chunk");
        let chunk: Vec<ProofOp> = Decoder::new(&encoded)
            .collect::<Result<_, _>>()
            .expect("the chunk decodes");
        nodes += chunk
            .iter()
            .filter(|op| matches!(op, ProofOp::Push(node) if !matches!(node, Node::Hash(_))))
            .count() as u64;
        let below = restorer.process_chunk(&chunk_id, chunk, version());
        pending.extend(below.expect("the chunk restores"));
    }
    let staged = resident();
    let processed = began.elapsed();

    let merk = restorer
        .finalize_with_grovedb_elements(version())
        .expect("the restore finalizes");
    assert_eq!(merk.root_hash().unwrap(), root_hash);
    let (most, after) = (most_resident(), resident());

    let each = |bytes: u64| bytes.saturating_sub(before) / nodes;
    println!(
        "RESTORE-MEMORY {engine}: {nodes} nodes in {} chunks",
        index.len()
    );
    println!("RESTORE-MEMORY {engine}: resident before the first chunk: {before}");
    println!(
        "RESTORE-MEMORY {engine}: every chunk processed: {staged} resident, {} more for a node, \
         after {processed:?}",
        each(staged)
    );
    println!(
        "RESTORE-MEMORY {engine}: most up to the end of finalize: {most} resident, {} more for a \
         node",
        each(most)
    );
    println!(
        "RESTORE-MEMORY {engine}: after finalize: {after} resident, {} more for a node, after {:?}",
        each(after),
        began.elapsed()
    );
    nodes
}

/// Restores the Merk in a write transaction of a LigninDB store, which is
/// handed the Merk whole, and prints what the process held.
fn restore_on_lignindb() {
    let (db, versions) = Db::new(number("RESTORE_MEMORY_FRAMES", 4_096) as usize);
    let path = db.path(&versions);
    let transaction = path.begin();
    let writer = LigninWriter::new(transaction.scope().expect("a scope"));
    let store = LigninNodeStore::immediate(&writer, LigninSubtree::alone([1; 32]));
    let nodes = restore_into("lignindb", store);
    let stats = transaction.stats();
    println!(
        "RESTORE-MEMORY lignindb: the transaction: {} writes, {} bytes in its write set ({} for \
         a node), {} tree pages, {} of them with values out of line",
        stats.writes,
        stats.write_set_bytes,
        stats.write_set_bytes as u64 / nodes,
        stats.tree_pages,
        stats.value_pages
    );
}

/// Restores the Merk in a transaction of RocksDB, which takes the chunks
/// as they arrive, and prints what the process held.
fn restore_on_rocksdb() {
    let storage = TempStorage::new();
    let transaction = storage.start_transaction();
    let context = storage
        .get_immediate_storage_context(SubtreePath::empty(), &transaction)
        .unwrap();
    restore_into("rocksdb", context);
}

#[test]
#[ignore = "a measurement that is run by hand, one step for each run of the test"]
fn the_memory_a_restore_holds() {
    match setting("RESTORE_MEMORY").as_deref() {
        Some("produce") => produce(
            number("RESTORE_MEMORY_NODES", 100_000),
            number("RESTORE_MEMORY_VALUE", 32) as usize,
        ),
        Some("restore") => restore_on_lignindb(),
        Some("restore-rocksdb") => restore_on_rocksdb(),
        other => {
            panic!("RESTORE_MEMORY is `produce`, `restore` or `restore-rocksdb`, not {other:?}")
        }
    }
}
