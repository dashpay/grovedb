use std::{
    collections::VecDeque,
    iter::{empty, once},
};

use merk::{
    proofs::{Node, Op},
    Merk,
};
use storage::{
    rocksdb_storage::{PrefixedRocksDbStorageContext, RocksDbStorage},
    Storage, StorageContext,
};

use crate::{Element, Error, GroveDb, Hash};

const OPS_PER_CHUNK: usize = 128;

impl GroveDb {
    /// Creates a chunk producer to replicate GroveDb.
    pub fn chunks(&self) -> SubtreeChunkProducer {
        SubtreeChunkProducer::new(&self.db)
    }
}

/// Subtree chunks producer.
pub struct SubtreeChunkProducer<'db> {
    storage: &'db RocksDbStorage,
    cache: Option<SubtreeChunkProducerCache<'db>>,
}

struct SubtreeChunkProducerCache<'db> {
    current_merk_path: Vec<Vec<u8>>,
    current_merk: Merk<PrefixedRocksDbStorageContext<'db>>,
    // This needed to be an `Option` becase it requres a reference on Merk but it's within the same
    // struct and during struct init a referenced Merk would be moved inside a struct, using
    // `Option` this init happens in two steps.
    current_chunk_producer: Option<merk::ChunkProducer<'db, PrefixedRocksDbStorageContext<'db>>>,
}

impl<'db> SubtreeChunkProducer<'db> {
    fn new(storage: &'db RocksDbStorage) -> Self {
        SubtreeChunkProducer {
            storage,
            cache: None,
        }
    }

    pub fn chunks_in_current_producer(&self) -> usize {
        self.cache
            .as_ref()
            .map(|c| c.current_chunk_producer.as_ref().map(|p| p.len()))
            .flatten()
            .unwrap_or(0)
    }

    pub fn get_chunk<'p, P>(&mut self, path: P, index: usize) -> Result<Vec<Op>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: Clone,
    {
        let path_iter = path.into_iter();

        if let Some(SubtreeChunkProducerCache {
            current_merk_path, ..
        }) = &self.cache
        {
            if !itertools::equal(current_merk_path, path_iter.clone()) {
                self.cache = None;
            }
        }

        if self.cache.is_none() {
            let ctx = self.storage.get_storage_context(path_iter.clone()).unwrap();
            let current_merk = Merk::open(ctx)
                .unwrap()
                .map_err(|e| Error::CorruptedData(e.to_string()))?;

            if current_merk.root_hash().unwrap() == [0; 32] {
                return Ok(Vec::new());
            }

            self.cache = Some(SubtreeChunkProducerCache {
                current_merk_path: path_iter.map(|p| p.to_vec()).collect(),
                current_merk,
                current_chunk_producer: None,
            });
            let cache = self.cache.as_mut().expect("exists at this point");
            cache.current_chunk_producer = Some(
                merk::ChunkProducer::new(&cache.current_merk)
                    .map_err(|e| Error::CorruptedData(e.to_string()))?,
            );
        }

        self.cache
            .as_mut()
            .expect("must exist at this point")
            .current_chunk_producer
            .as_mut()
            .expect("must exist at this point")
            .chunk(index)
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }
}

// TODO: make generic over storage_cost context
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
#[derive(Debug)]
pub enum RestorerResponse {
    AwaitNextChunk { path: Vec<Vec<u8>>, index: usize },
    Ready,
}

#[derive(Debug)]
pub struct RestorerError(String);

impl<'db> Restorer<'db> {
    /// Create a GroveDb restorer using a backing storage_cost and root hash.
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
    pub fn process_chunk(
        &mut self,
        chunk_ops: impl IntoIterator<Item = Op>,
    ) -> Result<RestorerResponse, RestorerError> {
        if self.current_merk_restorer.is_none() {
            // Last restorer was consumed and no more Merks to process.
            return Ok(RestorerResponse::Ready);
        }
        // First we decode a chunk to take out info about nested trees to add them into
        // todo list.
        let mut ops = Vec::new();
        for op in chunk_ops {
            ops.push(op);
            match ops.last().expect("just inserted") {
                Op::Push(Node::KV(key, bytes)) | Op::PushInverted(Node::KV(key, bytes)) => {
                    if let Element::Tree(root_key, _) =
                        Element::deserialize(bytes).map_err(|e| RestorerError(e.to_string()))?
                    {
                        if root_key.is_none() || self.current_merk_path.last() == Some(key) {
                            // We add only subtrees of the current subtree to queue, skipping
                            // itself; Also skipping empty Merks.
                            continue;
                        }
                        let mut path = self.current_merk_path.clone();
                        path.push(key.clone());
                        self.queue.push_back((path, root_key)); //todo: this needs to be fixed
                    }
                }
                _ => {}
            }
        }

        // Process chunk using Merk's possibilities.
        let remaining = self
            .current_merk_restorer
            .as_mut()
            .expect("restorer exists at this point")
            .process_chunk(ops)
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
                    path: self.current_merk_path.clone(),
                    index: self.current_merk_chunk_index,
                })
            } else {
                Ok(RestorerResponse::Ready)
            }
        } else {
            // Request a chunk at the same path but with incremented index.
            Ok(RestorerResponse::AwaitNextChunk {
                path: self.current_merk_path.clone(),
                index: self.current_merk_chunk_index,
            })
        }
    }
}

/// Chunk producer wrapper which uses bigger messages that may include chunks of
/// requested subtree with its right siblings.
///
/// Because `Restorer` builds GroveDb replica breadth-first way from top to
/// bottom it makes sense to send a subtree's siblings next instead of its own
/// subtrees.
pub struct SiblingsChunkProducer<'db> {
    chunk_producer: SubtreeChunkProducer<'db>,
}

#[derive(Debug)]
pub struct GroveChunk {
    subtree_chunks: Vec<(usize, Vec<Op>)>,
}

impl<'db> SiblingsChunkProducer<'db> {
    pub fn new(chunk_producer: SubtreeChunkProducer<'db>) -> Self {
        SiblingsChunkProducer { chunk_producer }
    }

    /// Get a collection of chunks possibly from different Merks with the first
    /// one as requested.
    pub fn get_chunk<'p, P>(&mut self, path: P, index: usize) -> Result<Vec<GroveChunk>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: Clone + DoubleEndedIterator + ExactSizeIterator,
    {
        let path_iter = path.into_iter();
        let mut result = Vec::new();
        let mut ops_count = 0;

        if path_iter.len() == 0 {
            // We're at the root of GroveDb, no siblings here.
            self.process_subtree_chunks(&mut result, &mut ops_count, empty(), index)?;
            return Ok(result);
        };

        // Get siblings on the right to send chunks of multiple Merks if it meets the
        // limit.

        let mut siblings_keys: VecDeque<Vec<u8>> = VecDeque::new();

        let mut parent_path = path_iter.clone();
        let requested_key = parent_path.next_back();

        let parent_ctx = self
            .chunk_producer
            .storage
            .get_storage_context(parent_path.clone())
            .unwrap();
        let mut siblings_iter = Element::iterator(parent_ctx.raw_iter()).unwrap();

        if let Some(key) = requested_key {
            siblings_iter.fast_forward(key)?;
        }

        while let Some(element) = siblings_iter.next().unwrap()? {
            if let (key, Element::Tree(..)) = element {
                siblings_keys.push_back(key);
            }
        }

        let mut current_index = index;
        // Process each subtree
        while let Some(subtree_key) = siblings_keys.pop_front() {
            let subtree_path = parent_path
                .clone()
                .map(|x| x.as_ref())
                .chain(once(subtree_key.as_slice()));

            self.process_subtree_chunks(&mut result, &mut ops_count, subtree_path, current_index)?;
            // Going to a next sibling, should start from 0.

            if ops_count >= OPS_PER_CHUNK {
                break;
            }
            current_index = 0;
        }

        Ok(result)
    }

    /// Process one subtree's chunks
    fn process_subtree_chunks<'p, P>(
        &mut self,
        result: &mut Vec<GroveChunk>,
        ops_count: &mut usize,
        subtree_path: P,
        from_index: usize,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: Clone,
    {
        let path_iter = subtree_path.into_iter();

        let mut current_index = from_index;
        let mut subtree_chunks = Vec::new();

        loop {
            let ops = self
                .chunk_producer
                .get_chunk(path_iter.clone(), current_index)?;

            *ops_count += ops.len();
            subtree_chunks.push((current_index, ops));
            current_index += 1;
            if current_index >= self.chunk_producer.chunks_in_current_producer()
                || *ops_count >= OPS_PER_CHUNK
            {
                break;
            }
        }

        result.push(GroveChunk { subtree_chunks });

        Ok(())
    }
}

/// `Restorer` wrapper that applies multiple chunks at once and eventually
/// returns less requests. It is named by analogy with IO types that do less
/// syscalls.
pub struct BufferedRestorer<'db> {
    restorer: Restorer<'db>,
}

impl<'db> BufferedRestorer<'db> {
    pub fn new(restorer: Restorer<'db>) -> Self {
        BufferedRestorer { restorer }
    }

    /// Process next chunk and receive instruction on what to do next.
    pub fn process_grove_chunks<'a, I>(
        &'a mut self,
        chunks: I,
    ) -> Result<RestorerResponse, RestorerError>
    where
        I: IntoIterator<Item = GroveChunk> + ExactSizeIterator,
    {
        let mut response = RestorerResponse::Ready;

        for c in chunks.into_iter() {
            for ops in c.subtree_chunks.into_iter().map(|x| x.1) {
                response = self.restorer.process_chunk(ops)?;
            }
        }

        Ok(response)
    }
}

#[cfg(test)]
mod test {
    use rand::RngCore;
    use storage::rocksdb_storage::test_utils::TempStorage;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        batch::GroveDbOp,
        tests::{make_test_grovedb, TempGroveDb, ANOTHER_TEST_LEAF, TEST_LEAF},
    };

    fn replicate(original_db: &GroveDb) -> TempDir {
        let replica_tempdir = TempDir::new().unwrap();

        {
            let replica_storage =
                RocksDbStorage::default_rocksdb_with_path(replica_tempdir.path()).unwrap();
            let mut chunk_producer = original_db.chunks();

            let mut restorer = Restorer::new(
                &replica_storage,
                original_db.root_hash(None).unwrap().unwrap(),
            )
            .expect("cannot create restorer");

            // That means root tree chunk with index 0
            let mut next_chunk: (Vec<Vec<u8>>, usize) = (vec![], 0);

            loop {
                let chunk = chunk_producer
                    .get_chunk(next_chunk.0.iter().map(|x| x.as_slice()), next_chunk.1)
                    .expect("cannot get next chunk");
                match restorer.process_chunk(chunk).expect("cannot process chunk") {
                    RestorerResponse::Ready => break,
                    RestorerResponse::AwaitNextChunk { path, index } => {
                        next_chunk = (path, index);
                    }
                }
            }
        }
        replica_tempdir
    }

    fn replicate_bigger_messages(original_db: &GroveDb) -> TempDir {
        let replica_tempdir = TempDir::new().unwrap();

        {
            let replica_storage =
                RocksDbStorage::default_rocksdb_with_path(replica_tempdir.path()).unwrap();
            let mut chunk_producer = SiblingsChunkProducer::new(original_db.chunks());

            let mut restorer = BufferedRestorer::new(
                Restorer::new(
                    &replica_storage,
                    original_db.root_hash(None).unwrap().unwrap(),
                )
                .expect("cannot create restorer"),
            );

            // That means root tree chunk with index 0
            let mut next_chunk: (Vec<Vec<u8>>, usize) = (vec![], 0);

            loop {
                let chunks = chunk_producer
                    .get_chunk(next_chunk.0.iter().map(|x| x.as_slice()), next_chunk.1)
                    .expect("cannot get next chunk");
                match restorer
                    .process_grove_chunks(chunks.into_iter())
                    .expect("cannot process chunk")
                {
                    RestorerResponse::Ready => break,
                    RestorerResponse::AwaitNextChunk { path, index } => {
                        next_chunk = (path, index);
                    }
                }
            }
        }

        replica_tempdir
    }

    fn test_replication_internal<'a, I, R, F>(
        original_db: &TempGroveDb,
        to_compare: I,
        replicate_fn: F,
    ) where
        R: AsRef<[u8]> + 'a,
        I: Iterator<Item = &'a [R]>,
        F: Fn(&GroveDb) -> TempDir,
    {
        let expected_root_hash = original_db.root_hash(None).unwrap().unwrap();

        let replica_tempdir = replicate_fn(&original_db);

        let replica = GroveDb::open(replica_tempdir.path()).unwrap();
        assert_eq!(
            replica.root_hash(None).unwrap().unwrap(),
            expected_root_hash
        );

        for full_path in to_compare {
            let (key, path) = full_path.split_last().unwrap();
            assert_eq!(
                original_db
                    .get(path.iter().map(|x| x.as_ref()), key.as_ref(), None)
                    .unwrap()
                    .unwrap(),
                replica
                    .get(path.iter().map(|x| x.as_ref()), key.as_ref(), None)
                    .unwrap()
                    .unwrap()
            );
        }
    }

    fn test_replication<'a, I, R>(original_db: &TempGroveDb, to_compare: I)
    where
        R: AsRef<[u8]> + 'a,
        I: Iterator<Item = &'a [R]> + Clone,
    {
        test_replication_internal(original_db, to_compare.clone(), replicate);
        test_replication_internal(original_db, to_compare, replicate_bigger_messages);
    }

    #[test]
    fn replicate_wrong_root_hash() {
        let db = make_test_grovedb();
        let mut bad_hash = db.root_hash(None).unwrap().unwrap();
        bad_hash[0] = bad_hash[0].wrapping_add(1);

        let temp_storage = TempStorage::default();
        let mut restorer = Restorer::new(&temp_storage, bad_hash).unwrap();
        let mut chunks = db.chunks();
        assert!(restorer
            .process_chunk(chunks.get_chunk([], 0).unwrap())
            .is_err());
    }

    #[test]
    fn replicate_provide_wrong_tree() {
        let db = make_test_grovedb();
        db.insert(
            [TEST_LEAF],
            b"key1",
            Element::new_item(b"ayya".to_vec()),
            None,
        )
        .unwrap()
        .expect("cannot insert an element");
        db.insert(
            [ANOTHER_TEST_LEAF],
            b"key1",
            Element::new_item(b"ayyb".to_vec()),
            None,
        )
        .unwrap()
        .expect("cannot insert an element");

        let expected_hash = db.root_hash(None).unwrap().unwrap();

        let temp_storage = TempStorage::default();
        let mut restorer = Restorer::new(&temp_storage, expected_hash).unwrap();
        let mut chunks = db.chunks();

        let next_op = restorer
            .process_chunk(chunks.get_chunk([], 0).unwrap())
            .unwrap();
        match next_op {
            RestorerResponse::AwaitNextChunk { path, index } => {
                // Feed restorer a wrong Merk!
                let chunk = if path == [TEST_LEAF] {
                    chunks.get_chunk([ANOTHER_TEST_LEAF], index).unwrap()
                } else {
                    chunks.get_chunk([TEST_LEAF], index).unwrap()
                };
                assert!(restorer.process_chunk(chunk).is_err());
            }
            _ => {}
        }
    }

    #[test]
    fn replicate_nested_grovedb() {
        let db = make_test_grovedb();
        db.insert(
            [TEST_LEAF],
            b"key1",
            Element::new_item(b"ayya".to_vec()),
            None,
        )
        .unwrap()
        .expect("cannot insert an element");
        db.insert([ANOTHER_TEST_LEAF], b"key2", Element::empty_tree(), None)
            .unwrap()
            .expect("cannot insert an element");
        db.insert(
            [ANOTHER_TEST_LEAF, b"key2"],
            b"key3",
            Element::empty_tree(),
            None,
        )
        .unwrap()
        .expect("cannot insert an element");
        db.insert(
            [ANOTHER_TEST_LEAF, b"key2", b"key3"],
            b"key4",
            Element::new_item(b"ayyb".to_vec()),
            None,
        )
        .unwrap()
        .expect("cannot insert an element");

        let to_compare = [
            [TEST_LEAF].as_ref(),
            [TEST_LEAF, b"key1"].as_ref(),
            [ANOTHER_TEST_LEAF].as_ref(),
            [ANOTHER_TEST_LEAF, b"key2"].as_ref(),
            [ANOTHER_TEST_LEAF, b"key2", b"key3"].as_ref(),
            [ANOTHER_TEST_LEAF, b"key2", b"key3", b"key4"].as_ref(),
        ];
        test_replication(&db, to_compare.into_iter());
    }

    #[test]
    fn replicate_a_big_one() {
        const HEIGHT: usize = 3;
        const SUBTREES_FOR_EACH: usize = 3;
        const SCALARS_FOR_EACH: usize = 600;

        let db = make_test_grovedb();
        let mut to_compare = Vec::new();

        let mut rng = rand::thread_rng();
        let mut subtrees: VecDeque<Vec<[u8; 8]>> = VecDeque::new();

        // Generate root tree leafs
        for _ in 0..SUBTREES_FOR_EACH {
            let mut bytes = [0; 8];
            rng.fill_bytes(&mut bytes);
            db.insert([], &bytes, Element::empty_tree(), None)
                .unwrap()
                .unwrap();
            subtrees.push_front(vec![bytes.clone()]);
            to_compare.push(vec![bytes.clone()]);
        }

        while let Some(path) = subtrees.pop_front() {
            let mut batch = Vec::new();

            if path.len() < HEIGHT {
                for _ in 0..SUBTREES_FOR_EACH {
                    let mut bytes = [0; 8];
                    rng.fill_bytes(&mut bytes);

                    batch.push(GroveDbOp::insert_run_op(
                        path.iter().map(|x| x.to_vec()).collect(),
                        bytes.to_vec(),
                        Element::empty_tree(),
                    ));

                    let mut new_path = path.clone();
                    new_path.push(bytes);
                    subtrees.push_front(new_path.clone());
                    to_compare.push(new_path.clone());
                }
            }

            for _ in 0..SCALARS_FOR_EACH {
                let mut bytes = [0; 8];
                let mut bytes_val = vec![];
                rng.fill_bytes(&mut bytes);
                rng.fill_bytes(&mut bytes_val);

                batch.push(GroveDbOp::insert_run_op(
                    path.iter().map(|x| x.to_vec()).collect(),
                    bytes.to_vec(),
                    Element::new_item(bytes_val),
                ));

                let mut new_path = path.clone();
                new_path.push(bytes);
                to_compare.push(new_path.clone());
            }

            db.apply_batch(batch, None, None).unwrap().unwrap();
        }

        test_replication(&db, to_compare.iter().map(|x| x.as_slice()));
    }

    #[test]
    fn replicate_from_checkpoint() {
        // Create a simple GroveDb first
        let db = make_test_grovedb();
        db.insert(
            [TEST_LEAF],
            b"key1",
            Element::new_item(b"ayya".to_vec()),
            None,
        )
        .unwrap()
        .unwrap();
        db.insert(
            [ANOTHER_TEST_LEAF],
            b"key2",
            Element::new_item(b"ayyb".to_vec()),
            None,
        )
        .unwrap()
        .unwrap();

        // Save its state with checkpoint
        let checkpoint_dir_parent = TempDir::new().unwrap();
        let checkpoint_dir = checkpoint_dir_parent.path().join("cp");
        db.create_checkpoint(&checkpoint_dir).unwrap();

        // Alter the db to make difference between current state and checkpoint
        db.delete([TEST_LEAF], b"key1", None).unwrap().unwrap();
        db.insert(
            [TEST_LEAF],
            b"key3",
            Element::new_item(b"ayyd".to_vec()),
            None,
        )
        .unwrap()
        .unwrap();
        db.insert(
            [ANOTHER_TEST_LEAF],
            b"key2",
            Element::new_item(b"ayyc".to_vec()),
            None,
        )
        .unwrap()
        .unwrap();

        let checkpoint_db = GroveDb::open(&checkpoint_dir).unwrap();

        // Ensure checkpoint differs from current state
        assert_ne!(
            checkpoint_db
                .get([ANOTHER_TEST_LEAF], b"key2", None)
                .unwrap()
                .unwrap(),
            db.get([ANOTHER_TEST_LEAF], b"key2", None).unwrap().unwrap(),
        );

        // Build a replica from checkpoint
        let replica_dir = replicate(&checkpoint_db);
        let replica_db = GroveDb::open(&replica_dir).unwrap();

        assert_eq!(
            checkpoint_db
                .get([TEST_LEAF], b"key1", None)
                .unwrap()
                .unwrap(),
            replica_db.get([TEST_LEAF], b"key1", None).unwrap().unwrap(),
        );
        assert_eq!(
            checkpoint_db
                .get([ANOTHER_TEST_LEAF], b"key2", None)
                .unwrap()
                .unwrap(),
            replica_db
                .get([ANOTHER_TEST_LEAF], b"key2", None)
                .unwrap()
                .unwrap(),
        );
        assert!(matches!(
            replica_db.get([TEST_LEAF], b"key3", None).unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));

        // Drop original db and checkpoint dir too to ensure there is no dependency
        drop(db);
        drop(checkpoint_db);
        drop(checkpoint_dir);

        assert_eq!(
            replica_db
                .get([ANOTHER_TEST_LEAF], b"key2", None)
                .unwrap()
                .unwrap(),
            Element::new_item(b"ayyb".to_vec())
        );
    }
}
