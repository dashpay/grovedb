use std::{collections::VecDeque, iter::empty, slice};

use merk::{
    proofs::{Decoder, Node, Op},
    Merk,
};
use storage::{
    rocksdb_storage::{PrefixedRocksDbStorageContext, RocksDbStorage},
    Storage,
};

use crate::{Element, GroveDb, Hash};

impl GroveDb {
    /// Creates a chunk producer to replicate GroveDb.
    pub fn chunks(&self) -> ChunkProducer {
        ChunkProducer {}
    }
}

/// GroveDb chunks producer.
pub struct ChunkProducer {}

// TODO: make generic over storage context
type MerkRestorer<'db> = merk::Restorer<PrefixedRocksDbStorageContext<'db>>;

type Path = Vec<Vec<u8>>;

/// Structure to drive GroveDb restore process.
pub struct Restorer<'db> {
    current_merk_restorer: Option<MerkRestorer<'db>>,
    current_merk_chunk_index: usize,
    current_merk_path: Path,
    queue: VecDeque<(Path, Hash)>,
    storage: &'db RocksDbStorage,
}

/// Indicates what next piece of information `Restorer` expects or wraps a
/// successful result.
pub enum RestorerResponse<'a> {
    AwaitNextChunk {
        path: slice::Iter<'a, Vec<u8>>,
        index: usize,
    },
    Ready,
}

#[derive(Debug)]
pub struct RestorerError(String);

impl<'db> Restorer<'db> {
    /// Create a GroveDb restorer using a backing storage and root hash.
    pub fn new(storage: &'db RocksDbStorage, root_hash: Hash) -> Result<Self, RestorerError> {
        Ok(Restorer {
            current_merk_restorer: Some(MerkRestorer::new(
                Merk::open(storage.get_storage_context(empty()).unwrap())
                    .unwrap()
                    .map_err(|e| RestorerError(e.to_string()))?,
                root_hash,
            )),
            current_merk_chunk_index: 0,
            current_merk_path: vec![],
            queue: VecDeque::new(),
            storage,
        })
    }

    /// Process next chunk and receive instruction on what to do next.
    pub fn process_chunk(&mut self, chunk: &[u8]) -> Result<RestorerResponse, RestorerError> {
        if self.current_merk_restorer.is_none() {
            // Last restorer was consumed and no more Merks to process.
            return Ok(RestorerResponse::Ready)
        }
        // First we decode a chunk to take out info about nested trees to add them into
        // todo list.
        //
        // TODO: do not decode twice (because Merk does the same too)
        let mut ops = Vec::new();
        for op in Decoder::new(chunk) {
            let op = op.map_err(|e| RestorerError(e.to_string()))?;
            match &op {
                Op::Push(Node::KV(key, bytes)) | Op::PushInverted(Node::KV(key, bytes)) => {
                    if let Element::Tree(hash, _) =
                        Element::deserialize(bytes).map_err(|e| RestorerError(e.to_string()))?
                    {
                        let mut path = self.current_merk_path.clone();
                        path.push(key.clone());
                        self.queue.push_back((path, hash));
                    }
                }
                _ => {}
            }
            ops.push(op);
        }

        // Process chunk using Merk's possibilities.
        let remaining = self
            .current_merk_restorer
            .as_mut()
            .expect("restorer exists at this point")
            .process_chunk(chunk)
            .map_err(|e| RestorerError(e.to_string()))?;

        self.current_merk_chunk_index += 1;

        if remaining == 0 {
            // If no more chunks for this Merk required decide if we're done or take a next
            // Merk to process.
            self.current_merk_restorer
                .take()
                .expect("restorer exists at this point")
                .finalize()
                .map_err(|e| RestorerError(e.to_string()))?;
            if let Some((next_path, expected_hash)) = self.queue.pop_front() {
                // Process next subtree.
                self.current_merk_restorer = Some(MerkRestorer::new(
                    Merk::open(
                        self.storage
                            .get_storage_context(next_path.iter().map(|x| x.as_slice()))
                            .unwrap(),
                    )
                    .unwrap()
                    .map_err(|e| RestorerError(e.to_string()))?,
                    expected_hash,
                ));
                self.current_merk_chunk_index = 0;
                self.current_merk_path = next_path;

                Ok(RestorerResponse::AwaitNextChunk {
                    path: self.current_merk_path.iter(),
                    index: self.current_merk_chunk_index,
                })
            } else {
                Ok(RestorerResponse::Ready)
            }
        } else {
            // Request a chunk at the same path but with incremented index.
            Ok(RestorerResponse::AwaitNextChunk {
                path: self.current_merk_path.iter(),
                index: self.current_merk_chunk_index,
            })
        }
    }
}
