//! The chunks of the corpus's Merks, and their restore, on both engines
//! (MERK-07).
//!
//! When a trace has been replayed, Merks of the grove it leaves are copied
//! as state sync copies a Merk, on RocksDB and on LigninDB: each live Merk
//! that has a root, is the Merk of no axis and is hosted by an element
//! that opens a Merk.
//!
//! - **The chunks.** Every chunk of the Merk is produced from the Merk on
//!   RocksDB, as the trace's transaction reads it, and from the Merk on the
//!   LigninDB store, which the replay has handed every write. The two are
//!   the same bytes.
//! - **The restore.** A Merk is restored from those chunks on each engine,
//!   in a store that holds the Merks restored from that grove and nothing
//!   else: on RocksDB chunk by
//!   chunk, and on LigninDB staged and handed over whole. The values are
//!   finalized as elements, as GroveDB finalizes a subtree. Each chunk and
//!   the end of the restore answer the same on both, and a restore that
//!   finishes leaves the same root hash and the same record for every
//!   node.
//!
//! A restore that GroveDB drives, with the Merks of a grove entered under
//! their elements, is GROVE-07's.

use std::collections::BTreeMap;

use grovedb::{GroveDb, Transaction};
use grovedb_merk::{
    proofs::{
        chunk::{error::ChunkError, util::generate_traversal_instruction_as_vec_bytes},
        encode_into, Op as ProofOp,
    },
    store::{
        lignin::{LigninNodeStore, LigninSubtree, LigninWriter},
        NodeStore,
    },
    ChunkProducer, CryptoHash, Error, Merk, Restorer, TreeType,
};
use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage};
use grovedb_version::version::GroveVersion;
use lignin_testkit::layout_sim::Grove;
use lignindb::txn::{testing::Db, WriteTxn};

use super::{proofs::tree_type, records, NO_VALUE_DEFINED_COST};

/// What the restores of a replay came to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restored {
    /// The Merks whose chunks were produced on both engines, their chunks
    /// and the bytes of those.
    pub merks: u64,
    pub chunks: u64,
    pub bytes: u64,
    /// The Merks of more than one chunk.
    pub of_several_chunks: u64,
    /// The Merks that a restore finished on both engines, and their nodes.
    pub finished: u64,
    pub nodes: u64,
    /// The Merks whose restore both engines refused alike.
    pub refused: u64,
    /// The Merks that give no chunk on either engine: their nodes are of
    /// another family than the tree type they are opened under.
    pub no_chunks: u64,
    /// The Merks that were left out: what hosts them is no tree of a type
    /// that opens one.
    pub skipped: u64,
}

impl Restored {
    pub fn add(&mut self, other: Restored) {
        self.merks += other.merks;
        self.chunks += other.chunks;
        self.bytes += other.bytes;
        self.of_several_chunks += other.of_several_chunks;
        self.finished += other.finished;
        self.nodes += other.nodes;
        self.refused += other.refused;
        self.no_chunks += other.no_chunks;
        self.skipped += other.skipped;
    }
}

fn open<'db, S: NodeStore<'db>>(
    store: S,
    root_key: Option<Vec<u8>>,
    tree_type: TreeType,
    grove_version: &GroveVersion,
) -> Merk<S> {
    Merk::open_layered_with_root_key(
        store,
        root_key,
        tree_type,
        NO_VALUE_DEFINED_COST,
        grove_version,
    )
    .unwrap()
    .expect("the Merk opens")
}

/// The chunks of `merk`, by chunk id. The producer numbers the chunks a
/// tree of the Merk's height has when every node of it is there, so a
/// number can name a place where this tree has no node: such a chunk is
/// refused, and no restore asks for it.
fn chunks_of<'db, S: NodeStore<'db>>(
    merk: &Merk<S>,
    grove_version: &GroveVersion,
) -> BTreeMap<Vec<u8>, Vec<ProofOp>> {
    let height = merk.height().expect("the Merk has nodes") as usize;
    let mut producer = ChunkProducer::new(merk).expect("the Merk has nodes");
    let mut chunks = BTreeMap::new();
    for index in 1..=producer.len() {
        let chunk_id = generate_traversal_instruction_as_vec_bytes(height, index).expect("an id");
        match producer.chunk_with_index(index, grove_version) {
            Ok((chunk, _)) => {
                chunks.insert(chunk_id, chunk);
            }
            Err(Error::ChunkingError(ChunkError::BadTraversalInstruction(_))) => {}
            Err(error) => panic!("the chunk {index}: {error:?}"),
        }
    }
    chunks
}

/// Restores a Merk from `chunks` into `store`, in the order the restore
/// asks for them, and returns what each chunk and the end of the restore
/// answered, with every node the restored tree holds as its record.
fn restore<'db, S: NodeStore<'db>>(
    store: S,
    tree_type: TreeType,
    root_hash: CryptoHash,
    chunks: &BTreeMap<Vec<u8>, Vec<ProofOp>>,
    grove_version: &GroveVersion,
) -> (Vec<String>, Option<u64>) {
    let merk = open(store, None, tree_type, grove_version);
    let mut restorer = Restorer::new(merk, root_hash, None);
    let mut said = Vec::new();
    let mut pending = vec![Vec::new()];
    while let Some(chunk_id) = pending.pop() {
        let chunk = chunks[&chunk_id].clone();
        let answered = restorer.process_chunk(&chunk_id, chunk, grove_version);
        said.push(format!("{chunk_id:?}: {answered:?}"));
        pending.extend(answered.unwrap_or_default());
    }
    match restorer.finalize_with_grovedb_elements(grove_version) {
        Err(error) => {
            said.push(format!("not finalized: {error:?}"));
            (said, None)
        }
        Ok(merk) => {
            let root = merk.root_hash_key_and_aggregate_data().unwrap();
            said.push(format!("finalized: {root:?}"));
            let nodes = records(&merk.storage, merk.root_key());
            said.extend(
                nodes.iter().map(|(key, record, _)| {
                    format!("{} {}", hex::encode(key), hex::encode(record))
                }),
            );
            (said, Some(nodes.len() as u64))
        }
    }
}

/// Produces the chunks of the Merks of `grove` that are copied on both
/// engines, and restores each Merk from them on both.
pub fn compare(
    db: &GroveDb,
    transaction: &Transaction,
    reader: &WriteTxn<'_>,
    grove: &Grove,
    grove_version: &GroveVersion,
) -> Restored {
    let mut restored = Restored::default();
    // The stores the Merks are restored into, which hold nothing else. The
    // LigninDB transaction is not committed: a Merk that is restored alone
    // has elements whose Merks are not there.
    let rocks = TempStorage::new();
    let rocks_transaction = rocks.start_transaction();
    let (lignin, versions) = Db::new(1024);
    let path = lignin.path(&versions);
    let lignin_transaction = path.begin();
    let writer = LigninWriter::new(lignin_transaction.scope().expect("a scope"));

    for (_, model) in grove
        .merks()
        .filter(|(_, merk)| merk.live && merk.axis.is_none())
    {
        let Some(root) = model.root else {
            continue;
        };
        let Some(tree_type) = tree_type(db, transaction, grove, model, grove_version) else {
            restored.skipped += 1;
            continue;
        };
        let root_key = Some(grove.node(root).key.to_vec());
        let at = &model.path;

        let context = db
            .raw_storage()
            .get_transactional_storage_context_by_subtree_prefix(model.prefix, None, transaction)
            .unwrap();
        let expected = open(context, root_key.clone(), tree_type, grove_version);
        let store = LigninNodeStore::read_only(reader, LigninSubtree::alone(model.prefix));
        let source = open(store, root_key, tree_type, grove_version);
        let root_hash = expected.root_hash().unwrap();
        assert_eq!(source.root_hash().unwrap(), root_hash, "the Merk {at:?}");

        // A Merk under a tree type of another family than its nodes are of
        // gives no chunk, on either engine.
        let produced = (ChunkProducer::new(&expected))
            .and_then(|mut producer| producer.chunk(&[], grove_version).map(|_| ()));
        if let Err(refused) = produced {
            let found = (ChunkProducer::new(&source))
                .and_then(|mut producer| producer.chunk(&[], grove_version).map(|_| ()));
            assert_eq!(
                format!("{found:?}"),
                format!("{:?}", Err::<(), _>(refused)),
                "the chunk at the root of the Merk {at:?}"
            );
            restored.no_chunks += 1;
            continue;
        }

        let chunks = chunks_of(&source, grove_version);
        assert_eq!(
            chunks,
            chunks_of(&expected, grove_version),
            "the chunks of the Merk {at:?}"
        );
        restored.merks += 1;
        restored.chunks += chunks.len() as u64;
        restored.of_several_chunks += u64::from(chunks.len() > 1);
        for chunk in chunks.values() {
            let mut bytes = Vec::new();
            encode_into(chunk.iter(), &mut bytes);
            restored.bytes += bytes.len() as u64;
        }

        let context = rocks
            .get_immediate_storage_context_by_subtree_prefix(model.prefix, &rocks_transaction)
            .unwrap();
        let (expected, expected_nodes) =
            restore(context, tree_type, root_hash, &chunks, grove_version);
        let store = LigninNodeStore::immediate(&writer, LigninSubtree::alone(model.prefix));
        // A store of the tests is made with some subtrees entered, that of
        // the root Merk among them.
        if store.entry().expect("the directory is read").is_none() {
            store.enter(tree_type).expect("the subtree is entered");
        }
        let (found, nodes) = restore(store, tree_type, root_hash, &chunks, grove_version);
        for (found, expected) in found.iter().zip(&expected) {
            assert_eq!(found, expected, "the restore of the Merk {at:?}");
        }
        assert_eq!(
            found.len(),
            expected.len(),
            "the restore of the Merk {at:?}"
        );
        assert_eq!(nodes, expected_nodes, "the restore of the Merk {at:?}");
        match nodes {
            Some(nodes) => {
                restored.finished += 1;
                restored.nodes += nodes;
            }
            None => restored.refused += 1,
        }
    }
    restored
}
