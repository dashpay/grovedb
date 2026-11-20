//! Chunk production and restore from chunks on both engines, over Merks
//! of the corpus (MERK-07).
//!
//! `compare` is given the grove a replayed trace leaves. It takes each
//! Merk of that grove that is live, has a root and is the Merk of no axis
//! through these steps:
//!
//! 1. `proofs::tree_type` gives the tree type the Merk is opened under. A
//!    Merk for which it gives none is counted as skipped and goes no
//!    further.
//! 2. The Merk is opened on RocksDB, in the trace's transaction, and on
//!    the LigninDB store the replay wrote. The two root hashes are equal.
//! 3. On RocksDB a chunk producer is made and asked for the chunk at the
//!    root. If either is refused, the same two calls on the store end in
//!    an error with the same `Debug` text, and the Merk is counted as
//!    giving no chunk and goes no further.
//! 4. `chunks_of` asks each engine for its chunks. The two maps of chunk id
//!    to chunk are equal.
//! 5. `restore` restores a Merk from that map on each engine, with no
//!    `parent_key_value_hash`, and ends with
//!    `finalize_with_grovedb_elements`. Each chunk id given, with what the
//!    chunk answered, and whether `finalize` succeeded, with its error
//!    where it did not, are the same on the two engines, compared as
//!    their `Debug` text. Where `finalize` succeeded, so is what
//!    `root_hash_key_and_aggregate_data` answers for the restored Merk,
//!    and the key and the record of every node reached from its root.
//!
//! The restores of one call go into one new RocksDB storage and one new
//! LigninDB store, each under the prefix the Merk has in the grove. What a
//! restore that does not finish wrote stays there.
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

/// The tally of `compare`, by the steps of the module documentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restored {
    /// The Merks that passed step 4, the chunks in their maps, and the
    /// encoded bytes of those chunks.
    pub merks: u64,
    pub chunks: u64,
    pub bytes: u64,
    /// Those of them whose map has more than one chunk.
    pub of_several_chunks: u64,
    /// Those of them whose `finalize` succeeded in step 5, and the nodes
    /// of their trees.
    pub finished: u64,
    pub nodes: u64,
    /// Those of them whose `finalize` did not succeed in step 5.
    pub refused: u64,
    /// The Merks that went no further than step 3.
    pub no_chunks: u64,
    /// The Merks that went no further than step 1.
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

/// The chunks of `merk` by chunk id: the chunk of every number its
/// producer counts, but for a number that is refused as a bad traversal
/// instruction. Any other refusal fails the test.
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

/// Restores a Merk from `chunks` into `store`: it gives the restore the
/// chunk at the root, and then every chunk whose id an answer names.
/// Returns, as text, each chunk id given with what the chunk answered,
/// and then the error of `finalize` or, where it succeeded, what
/// `root_hash_key_and_aggregate_data` answered and the key and the record
/// of every node reached from the root. Where `finalize` succeeded it
/// also returns how many nodes those are.
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

/// Compares chunk production and restore from chunks on the two engines
/// over the Merks of `grove` that the module documentation names, in its
/// steps.
pub fn compare(
    db: &GroveDb,
    transaction: &Transaction,
    reader: &WriteTxn<'_>,
    grove: &Grove,
    grove_version: &GroveVersion,
) -> Restored {
    let mut restored = Restored::default();
    // The two stores every restore of this call goes into. Neither
    // transaction is committed.
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

        // Step 3.
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
        // The subtree is entered unless the directory has an entry for it.
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
