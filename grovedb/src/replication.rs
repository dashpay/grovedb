use std::{collections::VecDeque, iter::empty, slice};

use merk::Merk;
use storage::{
    rocksdb_storage::{self, PrefixedRocksDbStorageContext, RocksDbStorage},
    Storage,
};

use crate::{GroveDb, Hash};

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
    current_merk_restorer: MerkRestorer<'db>,
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
    pub fn new(storage: &'db RocksDbStorage, root_hash: Hash) -> Result<Self, RestorerError> {
        Ok(Restorer {
            current_merk_restorer: MerkRestorer::new(
                Merk::open(storage.get_storage_context(empty()).unwrap())
                    .unwrap()
                    .map_err(|e| RestorerError(e.to_string()))?,
                root_hash,
            ),
            current_merk_chunk_index: 0,
            current_merk_path: vec![],
            queue: VecDeque::new(),
            storage,
        })
    }

    pub fn process_chunk(&mut self, chunk: &[u8]) -> Result<RestorerResponse, RestorerError> {
        self.current_merk_restorer
            .process_chunk(chunk)
            .map_err(|e| RestorerError(e.to_string()))?;
        self.current_merk_chunk_index += 1;

        if self.current_merk_restorer.remaining_chunks_unchecked() == 0 {
            if let Some((next_path, expected_hash)) = self.queue.pop_front() {
                self.current_merk_restorer = MerkRestorer::new(
                    Merk::open(
                        self.storage
                            .get_storage_context(next_path.iter().map(|x| x.as_slice()))
                            .unwrap(),
                    )
                    .unwrap()
                    .map_err(|e| RestorerError(e.to_string()))?,
                    expected_hash,
                );
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
            Ok(RestorerResponse::AwaitNextChunk {
                path: self.current_merk_path.iter(),
                index: self.current_merk_chunk_index,
            })
        }
    }
}
