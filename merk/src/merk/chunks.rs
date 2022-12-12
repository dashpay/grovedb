//! Provides `ChunkProducer`, which creates chunk proofs for full replication of
//! a Merk.

use costs::CostsExt;
use storage::{RawIterator, StorageContext};

use super::Merk;
use crate::{
    error::Error,
    proofs::{chunk::get_next_chunk, Node, Op},
};

/// A `ChunkProducer` allows the creation of chunk proofs, used for trustlessly
/// replicating entire Merk trees. Chunks can be generated on the fly in a
/// random order, or iterated in order for slightly better performance.
pub struct ChunkProducer<'db, S: StorageContext<'db>> {
    trunk: Vec<Op>,
    chunk_boundaries: Vec<Vec<u8>>,
    raw_iter: S::RawIterator,
    index: usize,
}

impl<'db, S> ChunkProducer<'db, S>
where
    S: StorageContext<'db>,
{
    /// Creates a new `ChunkProducer` for the given `Merk` instance. In the
    /// constructor, the first chunk (the "trunk") will be created.
    pub fn new(merk: &Merk<S>) -> Result<Self, Error> {
        let (trunk, has_more) = merk
            .walk(|maybe_walker| match maybe_walker {
                Some(mut walker) => walker.create_trunk_proof(),
                None => Ok((vec![], false)).wrap_with_cost(Default::default()),
            })
            .unwrap()?;

        let chunk_boundaries = if has_more {
            trunk
                .iter()
                .filter_map(|op| match op {
                    Op::Push(Node::KVValueHashFeatureType(key, ..)) => Some(key.clone()),
                    _ => None,
                })
                .collect()
        } else {
            vec![]
        };

        let mut raw_iter = merk.storage.raw_iter();
        raw_iter.seek_to_first().unwrap();

        Ok(ChunkProducer {
            trunk,
            chunk_boundaries,
            raw_iter,
            index: 0,
        })
    }

    /// Gets the chunk with the given index. Errors if the index is out of
    /// bounds or the tree is empty - the number of chunks can be checked by
    /// calling `producer.len()`.
    pub fn chunk(&mut self, index: usize) -> Result<Vec<Op>, Error> {
        if index >= self.len() {
            return Err(Error::ChunkingError("Chunk index out-of-bounds"));
        }

        self.index = index;

        if index == 0 || index == 1 {
            self.raw_iter.seek_to_first().unwrap();
        } else {
            let preceding_key = self.chunk_boundaries.get(index - 2).unwrap();
            self.raw_iter.seek(preceding_key).unwrap();
            self.raw_iter.next().unwrap();
        }

        self.next_chunk()
    }

    /// Returns the total number of chunks for the underlying Merk tree.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        let boundaries_len = self.chunk_boundaries.len();
        if boundaries_len == 0 {
            1
        } else {
            boundaries_len + 2
        }
    }

    /// Gets the next chunk based on the `ChunkProducer`'s internal index state.
    /// This is mostly useful for letting `ChunkIter` yield the chunks in order,
    /// optimizing throughput compared to random access.
    fn next_chunk(&mut self) -> Result<Vec<Op>, Error> {
        if self.index == 0 {
            if self.trunk.is_empty() {
                return Err(Error::ChunkingError(
                    "Attempted to fetch chunk on empty tree",
                ));
            }
            self.index += 1;
            return Ok(self.trunk.clone());
        }

        if self.index >= self.len() {
            panic!("Called next_chunk after end");
        }

        let end_key = self.chunk_boundaries.get(self.index - 1);
        let end_key_slice = end_key.as_ref().map(|k| k.as_slice());

        self.index += 1;

        get_next_chunk(&mut self.raw_iter, end_key_slice).unwrap()
    }
}

impl<'db, S> IntoIterator for ChunkProducer<'db, S>
where
    S: StorageContext<'db>,
{
    type IntoIter = ChunkIter<'db, S>;
    type Item = <ChunkIter<'db, S> as Iterator>::Item;

    fn into_iter(self) -> Self::IntoIter {
        ChunkIter(self)
    }
}

/// A `ChunkIter` iterates through all the chunks for the underlying `Merk`
/// instance in order (the first chunk is the "trunk" chunk). Yields `None`
/// after all chunks have been yielded.
pub struct ChunkIter<'db, S>(ChunkProducer<'db, S>)
where
    S: StorageContext<'db>;

impl<'db, S> Iterator for ChunkIter<'db, S>
where
    S: StorageContext<'db>,
{
    type Item = Result<Vec<Op>, Error>;

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.0.len(), Some(self.0.len()))
    }

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.index >= self.0.len() {
            None
        } else {
            Some(self.0.next_chunk())
        }
    }
}

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
{
    /// Creates a `ChunkProducer` which can return chunk proofs for replicating
    /// the entire Merk tree.
    pub fn chunks(&self) -> Result<ChunkProducer<'db, S>, Error> {
        ChunkProducer::new(self)
    }
}

#[cfg(test)]
mod tests {
    use std::iter::empty;

    use storage::{rocksdb_storage::RocksDbStorage, Storage};
    use tempfile::TempDir;

    use super::*;
    use crate::{
        proofs::chunk::{verify_leaf, verify_trunk},
        test_utils::*,
    };

    #[test]
    fn len_small() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(1..256);
        merk.apply::<_, Vec<_>>(&batch, &[], None).unwrap().unwrap();

        let chunks = merk.chunks().unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks.into_iter().size_hint().0, 1);
    }

    #[test]
    fn len_big() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(1..10_000);
        merk.apply::<_, Vec<_>>(&batch, &[], None).unwrap().unwrap();

        let chunks = merk.chunks().unwrap();
        assert_eq!(chunks.len(), 129);
        assert_eq!(chunks.into_iter().size_hint().0, 129);
    }

    #[test]
    fn generate_and_verify_chunks() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(1..10_000);
        merk.apply::<_, Vec<_>>(&batch, &[], None).unwrap().unwrap();

        let mut chunks = merk.chunks().unwrap().into_iter().map(|x| x.unwrap());

        let chunk = chunks.next().unwrap();
        let (trunk, height) = verify_trunk(chunk.into_iter().map(Ok)).unwrap().unwrap();
        assert_eq!(height, 14);
        assert_eq!(trunk.hash().unwrap(), merk.root_hash().unwrap());

        assert_eq!(trunk.layer(7).count(), 128);

        for (ops, node) in chunks.zip(trunk.layer(height / 2)) {
            verify_leaf(ops.into_iter().map(Ok), node.hash().unwrap())
                .unwrap()
                .unwrap();
        }
    }

    #[test]
    fn chunks_from_reopen() {
        let tmp_dir = TempDir::new().expect("cannot create tempdir");
        let original_chunks = {
            let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
                .expect("cannot open rocksdb storage");

            let mut merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
                .unwrap()
                .unwrap();
            let batch = make_batch_seq(1..10);
            merk.apply::<_, Vec<_>>(&batch, &[], None).unwrap().unwrap();

            merk.chunks()
                .unwrap()
                .into_iter()
                .map(|x| x.unwrap())
                .collect::<Vec<_>>()
                .into_iter()
        };
        let storage = RocksDbStorage::default_rocksdb_with_path(tmp_dir.path())
            .expect("cannot open rocksdb storage");
        let merk = Merk::open_base(storage.get_storage_context(empty()).unwrap(), false)
            .unwrap()
            .unwrap();
        let reopen_chunks = merk.chunks().unwrap().into_iter().map(|x| x.unwrap());

        for (original, checkpoint) in original_chunks.zip(reopen_chunks) {
            assert_eq!(original.len(), checkpoint.len());
        }
    }

    // #[test]
    // fn chunks_from_checkpoint() {
    //     let mut merk = TempMerk::new();
    //     let batch = make_batch_seq(1..10);
    //     merk.apply(batch.as_slice(), &[]).unwrap();

    //     let path: std::path::PathBuf =
    // "generate_and_verify_chunks_from_checkpoint.db".into();     if path.
    // exists() {         std::fs::remove_dir_all(&path).unwrap();
    //     }
    //     let checkpoint = merk.checkpoint(&path).unwrap();

    //     let original_chunks =
    // merk.chunks().unwrap().into_iter().map(Result::unwrap);
    //     let checkpoint_chunks =
    // checkpoint.chunks().unwrap().into_iter().map(Result::unwrap);

    //     for (original, checkpoint) in original_chunks.zip(checkpoint_chunks) {
    //         assert_eq!(original.len(), checkpoint.len());
    //     }

    //     std::fs::remove_dir_all(&path).unwrap();
    // }

    #[test]
    fn random_access_chunks() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(1..111);
        merk.apply::<_, Vec<_>>(&batch, &[], None).unwrap().unwrap();

        let chunks = merk
            .chunks()
            .unwrap()
            .into_iter()
            .map(|x| x.unwrap())
            .collect::<Vec<_>>();

        let mut producer = merk.chunks().unwrap();
        for i in 0..chunks.len() * 2 {
            let index = i % chunks.len();
            assert_eq!(producer.chunk(index).unwrap(), chunks[index]);
        }
    }

    #[test]
    #[should_panic(expected = "Attempted to fetch chunk on empty tree")]
    fn test_chunk_empty() {
        let merk = TempMerk::new();

        let _chunks = merk
            .chunks()
            .unwrap()
            .into_iter()
            .map(|x| x.unwrap())
            .collect::<Vec<_>>();
    }

    #[test]
    #[should_panic(expected = "Chunk index out-of-bounds")]
    fn test_chunk_index_oob() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(1..42);
        merk.apply::<_, Vec<_>>(&batch, &[], None).unwrap().unwrap();

        let mut producer = merk.chunks().unwrap();
        let _chunk = producer.chunk(50000).unwrap();
    }

    // #[test]
    // fn test_chunk_index_gt_1_access() {
    //     let mut merk = TempMerk::new();
    //     let batch = make_batch_seq(1..513);
    //     merk.apply::<_, Vec<_>>(&batch, &[]).unwrap().unwrap();

    //     let mut producer = merk.chunks().unwrap();
    //     println!("length: {}", producer.len());
    //     let chunk = producer.chunk(2).unwrap();
    //     assert_eq!(
    //         chunk,
    //         vec![
    //             3, 8, 0, 0, 0, 0, 0, 0, 0, 18, 0, 60, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123,             123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123,             123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123,             123, 123, 123, 3, 8, 0, 0, 0, 0, 0, 0, 0, 19, 0, 60,
    // 123, 123, 123, 123, 123, 123,             123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,             123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    //             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123,             123, 123, 123, 123, 123, 123, 16, 3, 8, 0, 0,
    // 0, 0, 0, 0, 0, 20, 0, 60, 123, 123,             123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,             123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    //             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123,             123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 17, 3, 8, 0, 0, 0, 0, 0, 0, 0,             21, 0, 60, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,             123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    //             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123,             123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 16, 3, 8, 0,             0, 0, 0, 0, 0, 0, 22,
    // 0, 60, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123,             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123,             123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123,             123, 3, 8, 0, 0,
    // 0, 0, 0, 0, 0, 23, 0, 60, 123, 123, 123, 123, 123, 123, 123, 123,
    //             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123,             123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123,             123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,             123,
    // 123, 123, 123, 16, 3, 8, 0, 0, 0, 0, 0, 0, 0, 24, 0, 60, 123, 123, 123, 123,
    //             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123,             123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123,             123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,             123,
    // 123, 123, 123, 123, 123, 123, 123, 17, 17, 3, 8, 0, 0, 0, 0, 0, 0, 0, 25, 0,
    //             60, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123,             123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123,             123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 16, 3, 8, 0, 0,
    // 0, 0,             0, 0, 0, 26, 0, 60, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123,             123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123,             123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    //             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 3,             8, 0, 0, 0, 0, 0, 0, 0, 27, 0, 60, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123,             123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123,             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123,             123, 123, 123, 16, 3, 8, 0, 0, 0, 0,
    // 0, 0, 0, 28, 0, 60, 123, 123, 123, 123, 123,             123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    //             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123,             123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123,             123, 123, 123, 123, 123,
    // 123, 123, 17, 3, 8, 0, 0, 0, 0, 0, 0, 0, 29, 0, 60, 123,             123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    //             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123,             123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123,             123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 16, 3, 8, 0, 0, 0, 0, 0, 0,             0,
    // 30, 0, 60, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123,             123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123,             123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123,             123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 3, 8, 0, 0,
    //             0, 0, 0, 0, 0, 31, 0, 60, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123,             123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123,             123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,             123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    //             123, 16, 3, 8, 0, 0, 0, 0, 0, 0, 0, 32, 0, 60, 123, 123, 123,
    // 123, 123, 123, 123,             123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123,             123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123, 123,
    // 123,             123, 123, 123, 123, 123, 17, 17, 17
    //         ]
    //     );
    // }

    #[test]
    #[should_panic(expected = "Called next_chunk after end")]
    fn test_next_chunk_index_oob() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(1..42);
        merk.apply::<_, Vec<_>>(&batch, &[], None).unwrap().unwrap();

        let mut producer = merk.chunks().unwrap();
        let _chunk1 = producer.next_chunk();
        let _chunk2 = producer.next_chunk();
    }
}
