// TODO: add license

//! Provides `Restorer`, which can create a replica of a Merk instance by
//! receiving chunk proofs.

use std::collections::BTreeMap;

use grovedb_storage::{Batch, StorageContext};

use crate::{
    merk::MerkSource,
    proofs::{
        chunk::{
            chunk_op::ChunkOp,
            error::ChunkError,
            util::{traversal_instruction_as_string, write_to_vec},
        },
        tree::{execute, Child, Tree as ProofTree},
        Node,
    },
    tree::{RefWalker, Tree},
    CryptoHash, Error,
    Error::{CostsError, EdError, StorageError},
    Link, Merk,
    TreeFeatureType::BasicMerk,
};

// TODO: add documentation
pub struct Restorer<S> {
    merk: Merk<S>,
    chunk_id_to_root_hash: BTreeMap<String, CryptoHash>,
}

impl<'db, S: StorageContext<'db>> Restorer<S> {
    // TODO: add documenation
    pub fn new(merk: Merk<S>, expected_root_hash: CryptoHash) -> Self {
        let mut chunk_id_to_root_hash = BTreeMap::new();
        chunk_id_to_root_hash.insert(traversal_instruction_as_string(vec![]), expected_root_hash);

        Self {
            merk,
            chunk_id_to_root_hash,
        }
    }

    // TODO: add documentation
    // what does the restorer process?
    // it should be able to process single chunks, subtree chunks and multi chunks
    // right? or just one of them?
    // I think it should process just multi chunk at least for now
    pub fn process_multi_chunk(
        &mut self,
        chunk: impl IntoIterator<Item = ChunkOp>,
    ) -> Result<(), Error> {
        // chunk id, chunk
        // we use the chunk id to know what to verify against
        let mut chunks = chunk.into_iter();

        // TODO: clean this up, make external function that peeks and asserts
        let chunk_id_string = if let Some(ChunkOp::ChunkId(chunk_id)) = chunks.next() {
            traversal_instruction_as_string(chunk_id)
        } else {
            return Err(Error::ChunkRestoringError(ChunkError::ExpectedChunkId));
        };

        // TODO: deal with unwrap
        let expected_root_hash = self.chunk_id_to_root_hash.get(&chunk_id_string).unwrap();
        dbg!(expected_root_hash);

        if let Some(ChunkOp::Chunk(chunk)) = chunks.next() {
            // todo: deal with error
            let tree = execute(chunk.into_iter().map(Ok), false, |_| Ok(()))
                .unwrap()
                .unwrap();
            debug_assert!(tree.hash().unwrap() == *expected_root_hash);
            dbg!("yayy");
            self.write_chunk(tree);
        } else {
            return Err(Error::ChunkRestoringError(ChunkError::ExpectedChunk));
        }

        Ok(())
    }

    /// Writes the data contained in `tree` (extracted from a verified chunk
    /// proof) to the RocksDB.
    fn write_chunk(&mut self, tree: ProofTree) -> Result<(), Error> {
        let mut batch = self.merk.storage.new_batch();

        tree.visit_refs(&mut |proof_node| {
            if let Some((mut node, key)) = match &proof_node.node {
                Node::KV(key, value) => Some((
                    Tree::new(key.clone(), value.clone(), None, BasicMerk).unwrap(),
                    key,
                )),
                Node::KVValueHash(key, value, value_hash) => Some((
                    Tree::new_with_value_hash(key.clone(), value.clone(), *value_hash, BasicMerk)
                        .unwrap(),
                    key,
                )),
                Node::KVValueHashFeatureType(key, value, value_hash, feature_type) => Some((
                    Tree::new_with_value_hash(
                        key.clone(),
                        value.clone(),
                        *value_hash,
                        *feature_type,
                    )
                    .unwrap(),
                    key,
                )),
                _ => None,
            } {
                // TODO: encode tree node without cloning key/value
                // *node.slot_mut(true) = proof_node.left.as_ref().map(Child::as_link);
                // *node.slot_mut(false) = proof_node.right.as_ref().map(Child::as_link);

                let bytes = node.encode();
                batch.put(key, &bytes, None, None).map_err(CostsError)
            } else {
                Ok(())
            }
        })?;

        self.merk
            .storage
            .commit_batch(batch)
            .unwrap()
            .map_err(StorageError)
    }
}

#[cfg(test)]
mod tests {
    use grovedb_path::SubtreePath;
    use grovedb_storage::{rocksdb_storage::test_utils::TempStorage, Storage};

    use super::*;
    use crate::{merk::chunks2::ChunkProducer, test_utils::make_batch_seq, Merk};

    #[test]
    fn restoration_test() {
        // Create source merk and populate
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let mut original = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx)
                .unwrap(),
            false,
        )
        .unwrap()
        .unwrap();
        let batch = make_batch_seq(0..15);
        original
            .apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(original.height(), Some(4));

        // Create to be restored merk
        let storage = TempStorage::new();
        let tx2 = storage.start_transaction();
        let restored_merk = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx2)
                .unwrap(),
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(restored_merk.height(), None);

        // assert initial conditions
        assert_ne!(
            original.root_hash().unwrap(),
            restored_merk.root_hash().unwrap()
        );

        // Perform Restoration
        let mut chunk_producer =
            ChunkProducer::new(&original).expect("should create chunk producer");

        let mut restorer = Restorer::new(restored_merk, original.root_hash().unwrap());

        let chunk = chunk_producer
            .multi_chunk_with_limit(1, None)
            .expect("should generate chunk");

        assert_eq!(chunk.next_index, None);
        assert_eq!(chunk.remaining_limit, None);
        assert_eq!(chunk.chunk.len(), 2);

        restorer.process_multi_chunk(chunk.chunk).unwrap();
    }
}
