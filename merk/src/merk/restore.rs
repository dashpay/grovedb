// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Provides `Restorer`, which can create a replica of a Merk instance by
//! receiving chunk proofs.

use std::collections::BTreeMap;

use grovedb_storage::{Batch, StorageContext};

use crate::{
    merk,
    merk::MerkSource,
    proofs::{
        chunk::{
            chunk::{LEFT, RIGHT},
            chunk_op::ChunkOp,
            error::{ChunkError, ChunkError::InternalError},
            util::{string_as_traversal_instruction, traversal_instruction_as_string},
        },
        tree::{execute, Child, Tree as ProofTree},
        Node, Op,
    },
    tree::{combine_hash, kv::ValueDefinedCostType, RefWalker, TreeNode},
    CryptoHash, Error,
    Error::{CostsError, StorageError},
    Link, Merk,
};

/// Restorer handles verification of chunks and replication of Merk trees.
/// Chunks can be processed randomly as long as their parent has been processed
/// already.
pub struct Restorer<S> {
    merk: Merk<S>,
    chunk_id_to_root_hash: BTreeMap<String, CryptoHash>,
    parent_key_value_hash: Option<CryptoHash>,
    // this is used to keep track of parents whose links need to be rewritten
    parent_keys: BTreeMap<String, Vec<u8>>,
}

impl<'db, S: StorageContext<'db>> Restorer<S> {
    /// Initializes a new chunk restorer with the expected root hash for the
    /// first chunk
    pub fn new(
        merk: Merk<S>,
        expected_root_hash: CryptoHash,
        parent_key_value_hash: Option<CryptoHash>,
    ) -> Self {
        let mut chunk_id_to_root_hash = BTreeMap::new();
        chunk_id_to_root_hash.insert(traversal_instruction_as_string(&vec![]), expected_root_hash);
        Self {
            merk,
            chunk_id_to_root_hash,
            parent_key_value_hash,
            parent_keys: BTreeMap::new(),
        }
    }

    // TODO: consider converting chunk id to a vec
    /// Processes a chunk at some chunk id, returns the chunks id's of chunks
    /// that can be requested
    pub fn process_chunk(
        &mut self,
        chunk_id: String,
        chunk: Vec<Op>,
    ) -> Result<Vec<String>, Error> {
        let expected_root_hash = self
            .chunk_id_to_root_hash
            .get(&chunk_id)
            .ok_or(Error::ChunkRestoringError(ChunkError::UnexpectedChunk))?;

        let mut parent_key_value_hash: Option<CryptoHash> = None;
        if chunk_id.is_empty() {
            parent_key_value_hash = self.parent_key_value_hash;
        }
        let chunk_tree = Self::verify_chunk(chunk, expected_root_hash, &parent_key_value_hash)?;

        let mut root_traversal_instruction = string_as_traversal_instruction(&chunk_id)?;

        if root_traversal_instruction.is_empty() {
            let _ = self.merk.set_base_root_key(Some(chunk_tree.key().to_vec()));
        } else {
            // every non root chunk has some associated parent with an placeholder link
            // here we update the placeholder link to represent the true data
            self.rewrite_parent_link(&chunk_id, &root_traversal_instruction, &chunk_tree)?;
        }

        // next up, we need to write the chunk and build the map again
        let chunk_write_result = self.write_chunk(chunk_tree, &mut root_traversal_instruction);
        if chunk_write_result.is_ok() {
            // if we were able to successfully write the chunk, we can remove
            // the chunk expected root hash from our chunk id map
            self.chunk_id_to_root_hash.remove(&chunk_id);
        }

        chunk_write_result
    }

    /// Process multi chunks (space optimized chunk proofs that can contain
    /// multiple singular chunks)
    pub fn process_multi_chunk(&mut self, multi_chunk: Vec<ChunkOp>) -> Result<Vec<String>, Error> {
        let mut expect_chunk_id = true;
        let mut chunk_ids = vec![];
        let mut current_chunk_id: String = "".to_string();

        for chunk_op in multi_chunk {
            if (matches!(chunk_op, ChunkOp::ChunkId(..)) && !expect_chunk_id)
                || (matches!(chunk_op, ChunkOp::Chunk(..)) && expect_chunk_id)
            {
                return Err(Error::ChunkRestoringError(ChunkError::InvalidMultiChunk(
                    "invalid multi chunk ordering",
                )));
            }
            match chunk_op {
                ChunkOp::ChunkId(instructions) => {
                    current_chunk_id = traversal_instruction_as_string(&instructions);
                }
                ChunkOp::Chunk(chunk) => {
                    // TODO: remove clone
                    let next_chunk_ids = self.process_chunk(current_chunk_id.clone(), chunk)?;
                    chunk_ids.extend(next_chunk_ids);
                }
            }
            expect_chunk_id = !expect_chunk_id;
        }
        Ok(chunk_ids)
    }

    /// Verifies the structure of a chunk and ensures the chunk matches the
    /// expected root hash
    fn verify_chunk(
        chunk: Vec<Op>,
        expected_root_hash: &CryptoHash,
        parent_key_value_hash_opt: &Option<CryptoHash>,
    ) -> Result<ProofTree, Error> {
        let chunk_len = chunk.len();
        let mut kv_count = 0;
        let mut hash_count = 0;

        // build tree from ops
        // ensure only made of KvValueFeatureType and Hash nodes and count them
        let tree = execute(chunk.clone().into_iter().map(Ok), false, |node| {
            if matches!(node, Node::KVValueHashFeatureType(..)) {
                kv_count += 1;
                Ok(())
            } else if matches!(node, Node::Hash(..)) {
                hash_count += 1;
                Ok(())
            } else {
                Err(Error::ChunkRestoringError(ChunkError::InvalidChunkProof(
                    "expected chunk proof to contain only kvvaluefeaturetype or hash nodes",
                )))
            }
        })
        .unwrap()?;

        // chunk len must be exactly equal to the kv_count + hash_count +
        // parent_branch_count + child_branch_count
        debug_assert_eq!(chunk_len, ((kv_count + hash_count) * 2) - 1);

        // chunk structure verified, next verify root hash
        match parent_key_value_hash_opt {
            Some(val_hash) => {
                let combined_hash = combine_hash(val_hash, &tree.hash().unwrap()).unwrap();
                if &combined_hash != expected_root_hash {
                    return Err(Error::ChunkRestoringError(ChunkError::InvalidChunkProof(
                        "chunk doesn't match expected root hash",
                    )));
                }
            }
            None => {
                if &tree.hash().unwrap() != expected_root_hash {
                    return Err(Error::ChunkRestoringError(ChunkError::InvalidChunkProof(
                        "chunk doesn't match expected root hash",
                    )));
                }
            }
        };

        Ok(tree)
    }

    /// Write the verified chunk to storage
    fn write_chunk(
        &mut self,
        chunk_tree: ProofTree,
        traversal_instruction: &mut Vec<bool>,
    ) -> Result<Vec<String>, Error> {
        // this contains all the elements we want to write to storage
        let mut batch = self.merk.storage.new_batch();
        let mut new_chunk_ids = Vec::new();

        chunk_tree.visit_refs_track_traversal_and_parent(
            traversal_instruction,
            None,
            &mut |proof_node, node_traversal_instruction, parent_key| {
                match &proof_node.node {
                    Node::KVValueHashFeatureType(key, value, value_hash, feature_type) => {
                        // build tree from node value
                        let mut tree = TreeNode::new_with_value_hash(
                            key.clone(),
                            value.clone(),
                            *value_hash,
                            *feature_type,
                        )
                        .unwrap();

                        // update tree links
                        *tree.slot_mut(LEFT) = proof_node.left.as_ref().map(Child::as_link);
                        *tree.slot_mut(RIGHT) = proof_node.right.as_ref().map(Child::as_link);

                        // encode the node and add it to the batch
                        let bytes = tree.encode();

                        batch.put(key, &bytes, None, None).map_err(CostsError)
                    }
                    Node::Hash(hash) => {
                        // the node hash points to the root of another chunk
                        // we get the chunk id and add the hash to restorer state
                        let chunk_id = traversal_instruction_as_string(node_traversal_instruction);
                        new_chunk_ids.push(chunk_id.clone());
                        self.chunk_id_to_root_hash.insert(chunk_id.clone(), *hash);
                        // TODO: handle unwrap
                        self.parent_keys
                            .insert(chunk_id, parent_key.unwrap().to_owned());
                        Ok(())
                    }
                    _ => {
                        // we do nothing for other node types
                        // technically verify chunk will be called before this
                        // as such this should be be reached
                        Ok(())
                    }
                }
            },
        )?;

        // write the batch
        self.merk
            .storage
            .commit_batch(batch)
            .unwrap()
            .map_err(StorageError)?;

        Ok(new_chunk_ids)
    }

    /// When we process truncated chunks, the parents of Node::Hash have invalid
    /// placeholder for links.
    /// When we get the actual chunk associated with the Node::Hash,
    /// we need to update the parent link to reflect the correct data.
    fn rewrite_parent_link(
        &mut self,
        chunk_id: &str,
        traversal_instruction: &[bool],
        chunk_tree: &ProofTree,
    ) -> Result<(), Error> {
        let parent_key = self
            .parent_keys
            .get(chunk_id)
            .ok_or(Error::ChunkRestoringError(InternalError(
                "after successful chunk verification parent key should exist",
            )))?;

        let mut parent = merk::fetch_node(
            &self.merk.storage,
            parent_key.as_slice(),
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )?
        .ok_or(Error::ChunkRestoringError(InternalError(
            "cannot find expected parent in memory, most likely state corruption issue",
        )))?;

        let is_left = traversal_instruction
            .last()
            .expect("rewrite is only called when traversal_instruction is not empty");

        let updated_key = chunk_tree.key();
        let updated_sum = chunk_tree.sum();

        if let Some(Link::Reference { key, sum, .. }) = parent.link_mut(*is_left) {
            *key = updated_key.to_vec();
            *sum = updated_sum;
        }

        let parent_bytes = parent.encode();
        self.merk
            .storage
            .put(parent_key, &parent_bytes, None, None)
            .unwrap()
            .map_err(StorageError)?;

        self.parent_keys
            .remove(chunk_id)
            .expect("confirmed parent key exists above");

        Ok(())
    }

    /// Each nodes height is not added to state as such the producer could lie
    /// about the height values after replication we need to verify the
    /// heights and if invalid recompute the correct values
    fn rewrite_heights(&mut self) -> Result<(), Error> {
        fn rewrite_child_heights<'s, 'db, S: StorageContext<'db>>(
            mut walker: RefWalker<MerkSource<'s, S>>,
            batch: &mut <S as StorageContext<'db>>::Batch,
        ) -> Result<(u8, u8), Error> {
            // TODO: remove unwrap
            let mut cloned_node = TreeNode::decode(
                walker.tree().key().to_vec(),
                walker.tree().encode().as_slice(),
                None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
            )
            .unwrap();

            let mut left_height = 0;
            let mut right_height = 0;

            if let Some(left_walker) = walker
                .walk(LEFT, None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>)
                .unwrap()?
            {
                let left_child_heights = rewrite_child_heights(left_walker, batch)?;
                left_height = left_child_heights.0.max(left_child_heights.1) + 1;
                *cloned_node.link_mut(LEFT).unwrap().child_heights_mut() = left_child_heights;
            }

            if let Some(right_walker) = walker
                .walk(RIGHT, None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>)
                .unwrap()?
            {
                let right_child_heights = rewrite_child_heights(right_walker, batch)?;
                right_height = right_child_heights.0.max(right_child_heights.1) + 1;
                *cloned_node.link_mut(RIGHT).unwrap().child_heights_mut() = right_child_heights;
            }

            let bytes = cloned_node.encode();
            batch
                .put(walker.tree().key(), &bytes, None, None)
                .map_err(CostsError)?;

            Ok((left_height, right_height))
        }

        let mut batch = self.merk.storage.new_batch();
        // TODO: deal with unwrap
        let mut tree = self.merk.tree.take().unwrap();
        let walker = RefWalker::new(&mut tree, self.merk.source());

        rewrite_child_heights(walker, &mut batch)?;

        self.merk.tree.set(Some(tree));

        self.merk
            .storage
            .commit_batch(batch)
            .unwrap()
            .map_err(StorageError)
    }

    /// Rebuild restoration state from partial storage state
    fn attempt_state_recovery(&mut self) -> Result<(), Error> {
        // TODO: think about the return type some more
        let (bad_link_map, parent_keys) = self.merk.verify(false);
        if !bad_link_map.is_empty() {
            self.chunk_id_to_root_hash = bad_link_map;
            self.parent_keys = parent_keys;
        }

        Ok(())
    }

    /// Consumes the `Restorer` and returns a newly created, fully populated
    /// Merk instance. This method will return an error if called before
    /// processing all chunks.
    pub fn finalize(mut self) -> Result<Merk<S>, Error> {
        // ensure all chunks have been processed
        if !self.chunk_id_to_root_hash.is_empty() || !self.parent_keys.is_empty() {
            return Err(Error::ChunkRestoringError(
                ChunkError::RestorationNotComplete,
            ));
        }

        // get the latest version of the root node
        let _ = self
            .merk
            .load_base_root(None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>);

        // if height values are wrong, rewrite height
        if self.verify_height().is_err() {
            let _ = self.rewrite_heights();
            // update the root node after height rewrite
            let _ = self
                .merk
                .load_base_root(None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>);
        }

        if !self.merk.verify(self.merk.is_sum_tree).0.is_empty() {
            return Err(Error::ChunkRestoringError(ChunkError::InternalError(
                "restored tree invalid",
            )));
        }

        Ok(self.merk)
    }

    /// Verify that the child heights of the merk tree links correctly represent
    /// the tree
    fn verify_height(&self) -> Result<(), Error> {
        let tree = self.merk.tree.take();
        let height_verification_result = if let Some(tree) = &tree {
            self.verify_tree_height(tree, tree.height())
        } else {
            Ok(())
        };
        self.merk.tree.set(tree);
        height_verification_result
    }

    fn verify_tree_height(&self, tree: &TreeNode, parent_height: u8) -> Result<(), Error> {
        let (left_height, right_height) = tree.child_heights();

        if (left_height.abs_diff(right_height)) > 1 {
            return Err(Error::CorruptedState(
                "invalid child heights, difference greater than 1 for AVL tree",
            ));
        }

        let max_child_height = left_height.max(right_height);
        if parent_height <= max_child_height || parent_height - max_child_height != 1 {
            return Err(Error::CorruptedState(
                "invalid child heights, parent height is not 1 less than max child height",
            ));
        }

        let left_link = tree.link(LEFT);
        let right_link = tree.link(RIGHT);

        if (left_height == 0 && left_link.is_some()) || (right_height == 0 && right_link.is_some())
        {
            return Err(Error::CorruptedState(
                "invalid child heights node has child height 0, but hash child",
            ));
        }

        if let Some(link) = left_link {
            let left_tree = link.tree();
            if left_tree.is_none() {
                let left_tree = TreeNode::get(
                    &self.merk.storage,
                    link.key(),
                    None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
                )
                .unwrap()?
                .ok_or(Error::CorruptedState("link points to non-existent node"))?;
                self.verify_tree_height(&left_tree, left_height)?;
            } else {
                self.verify_tree_height(left_tree.unwrap(), left_height)?;
            }
        }

        if let Some(link) = right_link {
            let right_tree = link.tree();
            if right_tree.is_none() {
                let right_tree = TreeNode::get(
                    &self.merk.storage,
                    link.key(),
                    None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
                )
                .unwrap()?
                .ok_or(Error::CorruptedState("link points to non-existent node"))?;
                self.verify_tree_height(&right_tree, right_height)?;
            } else {
                self.verify_tree_height(right_tree.unwrap(), right_height)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{
            test_utils::TempStorage, PrefixedRocksDbImmediateStorageContext,
            PrefixedRocksDbStorageContext,
        },
        RawIterator, Storage,
    };

    use super::*;
    use crate::{
        merk::chunks::ChunkProducer,
        proofs::chunk::{
            chunk::tests::traverse_get_node_hash, error::ChunkError::InvalidChunkProof,
        },
        test_utils::{
            make_batch_seq, TempMerk,
        },
        Error::ChunkRestoringError,
        Merk, PanicSource,
    };

    #[test]
    fn test_chunk_verification_non_avl_tree() {
        let non_avl_tree_proof = vec![
            Op::Push(Node::KV(vec![1], vec![1])),
            Op::Push(Node::KV(vec![2], vec![2])),
            Op::Parent,
            Op::Push(Node::KV(vec![3], vec![3])),
            Op::Parent,
        ];
        assert!(Restorer::<PrefixedRocksDbStorageContext>::verify_chunk(
            non_avl_tree_proof,
            &[0; 32],
            &None
        )
        .is_err());
    }

    #[test]
    fn test_chunk_verification_only_kv_feature_and_hash() {
        // should not accept kv
        let invalid_chunk_proof = vec![Op::Push(Node::KV(vec![1], vec![1]))];
        let verification_result =
            Restorer::<PrefixedRocksDbStorageContext>::verify_chunk(invalid_chunk_proof, &[0; 32], &None);
        assert!(matches!(
            verification_result,
            Err(ChunkRestoringError(InvalidChunkProof(
                "expected chunk proof to contain only kvvaluefeaturetype or hash nodes",
            )))
        ));

        // should not accept kvhash
        let invalid_chunk_proof = vec![Op::Push(Node::KVHash([0; 32]))];
        let verification_result =
            Restorer::<PrefixedRocksDbStorageContext>::verify_chunk(invalid_chunk_proof, &[0; 32], &None);
        assert!(matches!(
            verification_result,
            Err(ChunkRestoringError(InvalidChunkProof(
                "expected chunk proof to contain only kvvaluefeaturetype or hash nodes",
            )))
        ));

        // should not accept kvdigest
        let invalid_chunk_proof = vec![Op::Push(Node::KVDigest(vec![0], [0; 32]))];
        let verification_result =
            Restorer::<PrefixedRocksDbStorageContext>::verify_chunk(invalid_chunk_proof, &[0; 32], &None);
        assert!(matches!(
            verification_result,
            Err(ChunkRestoringError(InvalidChunkProof(
                "expected chunk proof to contain only kvvaluefeaturetype or hash nodes",
            )))
        ));

        // should not accept kvvaluehash
        let invalid_chunk_proof = vec![Op::Push(Node::KVValueHash(vec![0], vec![0], [0; 32]))];
        let verification_result =
            Restorer::<PrefixedRocksDbStorageContext>::verify_chunk(invalid_chunk_proof, &[0; 32], &None);
        assert!(matches!(
            verification_result,
            Err(ChunkRestoringError(InvalidChunkProof(
                "expected chunk proof to contain only kvvaluefeaturetype or hash nodes",
            )))
        ));

        // should not accept kvrefvaluehash
        let invalid_chunk_proof = vec![Op::Push(Node::KVRefValueHash(vec![0], vec![0], [0; 32]))];
        let verification_result =
            Restorer::<PrefixedRocksDbStorageContext>::verify_chunk(invalid_chunk_proof, &[0; 32], &None);
        assert!(matches!(
            verification_result,
            Err(ChunkRestoringError(InvalidChunkProof(
                "expected chunk proof to contain only kvvaluefeaturetype or hash nodes",
            )))
        ));
    }

    fn get_node_hash(node: Node) -> Result<CryptoHash, String> {
        match node {
            Node::Hash(hash) => Ok(hash),
            _ => Err("expected node hash".to_string()),
        }
    }

    #[test]
    fn test_process_chunk_correct_chunk_id_map() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let mut merk_tree = merk.tree.take().expect("should have inner tree");
        merk.tree.set(Some(merk_tree.clone()));
        let mut tree_walker = RefWalker::new(&mut merk_tree, PanicSource {});

        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let restoration_merk = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .unwrap();

        // restorer root hash should be empty
        assert_eq!(restoration_merk.root_hash().unwrap(), [0; 32]);

        // at the start both merks should have different root hash values
        assert_ne!(
            merk.root_hash().unwrap(),
            restoration_merk.root_hash().unwrap()
        );

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");
        let mut restorer = Restorer::new(restoration_merk, merk.root_hash().unwrap(), None);

        // initial restorer state should contain just the root hash of the source merk
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 1);
        assert_eq!(
            restorer.chunk_id_to_root_hash.get(""),
            Some(merk.root_hash().unwrap()).as_ref()
        );

        // generate first chunk
        let (chunk, _) = chunk_producer.chunk_with_index(1).unwrap();
        // apply first chunk
        let new_chunk_ids = restorer
            .process_chunk(traversal_instruction_as_string(&vec![]), chunk)
            .expect("should process chunk successfully");
        assert_eq!(new_chunk_ids.len(), 4);

        // after first chunk application
        // the chunk_map should contain 4 items
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 4);
        // assert all the chunk hash values
        assert_eq!(
            restorer.chunk_id_to_root_hash.get("11"),
            Some(get_node_hash(traverse_get_node_hash(&mut tree_walker, &[LEFT, LEFT])).unwrap())
                .as_ref()
        );
        assert_eq!(
            restorer.chunk_id_to_root_hash.get("10"),
            Some(get_node_hash(traverse_get_node_hash(&mut tree_walker, &[LEFT, RIGHT])).unwrap())
                .as_ref()
        );
        assert_eq!(
            restorer.chunk_id_to_root_hash.get("01"),
            Some(get_node_hash(traverse_get_node_hash(&mut tree_walker, &[RIGHT, LEFT])).unwrap())
                .as_ref()
        );
        assert_eq!(
            restorer.chunk_id_to_root_hash.get("00"),
            Some(get_node_hash(traverse_get_node_hash(&mut tree_walker, &[RIGHT, RIGHT])).unwrap())
                .as_ref()
        );

        // generate second chunk
        let (chunk, _) = chunk_producer.chunk_with_index(2).unwrap();
        // apply second chunk
        let new_chunk_ids = restorer
            .process_chunk(traversal_instruction_as_string(&vec![LEFT, LEFT]), chunk)
            .unwrap();
        assert_eq!(new_chunk_ids.len(), 0);
        // chunk_map should have 1 less element
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 3);
        assert_eq!(restorer.chunk_id_to_root_hash.get("11"), None);

        // let's try to apply the second chunk again, should not work
        let (chunk, _) = chunk_producer.chunk_with_index(2).unwrap();
        // apply second chunk
        let chunk_process_result =
            restorer.process_chunk(traversal_instruction_as_string(&vec![LEFT, LEFT]), chunk);
        assert!(chunk_process_result.is_err());
        assert!(matches!(
            chunk_process_result,
            Err(Error::ChunkRestoringError(ChunkError::UnexpectedChunk))
        ));

        // next let's get a random but expected chunk and work with that e.g. chunk 4
        // but let's apply it to the wrong place
        let (chunk, _) = chunk_producer.chunk_with_index(4).unwrap();
        let chunk_process_result =
            restorer.process_chunk(traversal_instruction_as_string(&vec![LEFT, RIGHT]), chunk);
        assert!(chunk_process_result.is_err());
        assert!(matches!(
            chunk_process_result,
            Err(Error::ChunkRestoringError(ChunkError::InvalidChunkProof(
                ..
            )))
        ));

        // correctly apply chunk 5
        let (chunk, _) = chunk_producer.chunk_with_index(5).unwrap();
        // apply second chunk
        let new_chunk_ids = restorer
            .process_chunk(traversal_instruction_as_string(&vec![RIGHT, RIGHT]), chunk)
            .unwrap();
        assert_eq!(new_chunk_ids.len(), 0);
        // chunk_map should have 1 less element
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 2);
        assert_eq!(restorer.chunk_id_to_root_hash.get("00"), None);

        // correctly apply chunk 3
        let (chunk, _) = chunk_producer.chunk_with_index(3).unwrap();
        // apply second chunk
        let new_chunk_ids = restorer
            .process_chunk(traversal_instruction_as_string(&vec![LEFT, RIGHT]), chunk)
            .unwrap();
        assert_eq!(new_chunk_ids.len(), 0);
        // chunk_map should have 1 less element
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 1);
        assert_eq!(restorer.chunk_id_to_root_hash.get("10"), None);

        // correctly apply chunk 4
        let (chunk, _) = chunk_producer.chunk_with_index(4).unwrap();
        // apply second chunk
        let new_chunk_ids = restorer
            .process_chunk(traversal_instruction_as_string(&vec![RIGHT, LEFT]), chunk)
            .unwrap();
        assert_eq!(new_chunk_ids.len(), 0);
        // chunk_map should have 1 less element
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 0);
        assert_eq!(restorer.chunk_id_to_root_hash.get("01"), None);

        // finalize merk
        let restored_merk = restorer.finalize().expect("should finalized successfully");

        assert_eq!(
            restored_merk.root_hash().unwrap(),
            merk.root_hash().unwrap()
        );
    }

    fn assert_raw_db_entries_eq(
        restored: &Merk<PrefixedRocksDbImmediateStorageContext>,
        original: &Merk<PrefixedRocksDbImmediateStorageContext>,
        length: usize,
    ) {
        assert_eq!(restored.root_hash().unwrap(), original.root_hash().unwrap());

        let mut original_entries = original.storage.raw_iter();
        let mut restored_entries = restored.storage.raw_iter();
        original_entries.seek_to_first().unwrap();
        restored_entries.seek_to_first().unwrap();

        let mut i = 0;
        loop {
            assert_eq!(
                restored_entries.valid().unwrap(),
                original_entries.valid().unwrap()
            );
            if !restored_entries.valid().unwrap() {
                break;
            }

            assert_eq!(restored_entries.key(), original_entries.key());
            assert_eq!(restored_entries.value(), original_entries.value());

            restored_entries.next().unwrap();
            original_entries.next().unwrap();

            i += 1;
        }

        assert_eq!(i, length);
    }

    // Builds a source merk with batch_size number of elements
    // attempts restoration on some empty merk
    // verifies that restoration was performed correctly.
    fn test_restoration_single_chunk_strategy(batch_size: u64) {
        // build the source merk
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let mut source_merk = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .unwrap();
        let batch = make_batch_seq(0..batch_size);
        source_merk
            .apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");

        // build the restoration merk
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let restoration_merk = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .unwrap();

        // at the start
        // restoration merk should have empty root hash
        // and source merk should have a different root hash
        assert_eq!(restoration_merk.root_hash().unwrap(), [0; 32]);
        assert_ne!(
            source_merk.root_hash().unwrap(),
            restoration_merk.root_hash().unwrap()
        );

        // instantiate chunk producer and restorer
        let mut chunk_producer =
            ChunkProducer::new(&source_merk).expect("should create chunk producer");
        let mut restorer = Restorer::new(restoration_merk, source_merk.root_hash().unwrap(), None);

        // perform chunk production and processing
        let mut chunk_id_opt = Some("".to_string());
        while let Some(chunk_id) = chunk_id_opt {
            let (chunk, next_chunk_id) = chunk_producer
                .chunk(chunk_id.as_str())
                .expect("should get chunk");
            restorer
                .process_chunk(chunk_id.to_string(), chunk)
                .expect("should process chunk successfully");
            chunk_id_opt = next_chunk_id;
        }

        // after chunk processing we should be able to finalize
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 0);
        assert_eq!(restorer.parent_keys.len(), 0);
        let restored_merk = restorer.finalize().expect("should finalize");

        // compare root hash values
        assert_eq!(
            source_merk.root_hash().unwrap(),
            restored_merk.root_hash().unwrap()
        );

        assert_raw_db_entries_eq(&restored_merk, &source_merk, batch_size as usize);
    }

    #[test]
    fn restore_single_chunk_20() {
        test_restoration_single_chunk_strategy(20);
    }

    #[test]
    fn restore_single_chunk_1000() {
        test_restoration_single_chunk_strategy(1000);
    }

    #[test]
    fn test_process_multi_chunk_no_limit() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let restoration_merk = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .unwrap();

        // restorer root hash should be empty
        assert_eq!(restoration_merk.root_hash().unwrap(), [0; 32]);

        // at the start both merks should have different root hash values
        assert_ne!(
            merk.root_hash().unwrap(),
            restoration_merk.root_hash().unwrap()
        );

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");
        let mut restorer = Restorer::new(restoration_merk, merk.root_hash().unwrap(), None);

        assert_eq!(restorer.chunk_id_to_root_hash.len(), 1);
        assert_eq!(
            restorer.chunk_id_to_root_hash.get(""),
            Some(merk.root_hash().unwrap()).as_ref()
        );

        // generate multi chunk from root with no limit
        let chunk = chunk_producer
            .multi_chunk_with_limit("", None)
            .expect("should generate multichunk");

        assert_eq!(chunk.chunk.len(), 2);
        assert_eq!(chunk.next_index, None);
        assert_eq!(chunk.remaining_limit, None);

        let next_ids = restorer
            .process_multi_chunk(chunk.chunk)
            .expect("should process chunk");
        // should have replicated all chunks
        assert_eq!(next_ids.len(), 0);
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 0);
        assert_eq!(restorer.parent_keys.len(), 0);

        let restored_merk = restorer.finalize().expect("should be able to finalize");

        // compare root hash values
        assert_eq!(
            restored_merk.root_hash().unwrap(),
            merk.root_hash().unwrap()
        );
    }

    #[test]
    fn test_process_multi_chunk_no_limit_but_non_root() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let restoration_merk = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .unwrap();

        // restorer root hash should be empty
        assert_eq!(restoration_merk.root_hash().unwrap(), [0; 32]);

        // at the start both merks should have different root hash values
        assert_ne!(
            merk.root_hash().unwrap(),
            restoration_merk.root_hash().unwrap()
        );

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");
        let mut restorer = Restorer::new(restoration_merk, merk.root_hash().unwrap(), None);

        assert_eq!(restorer.chunk_id_to_root_hash.len(), 1);
        assert_eq!(
            restorer.chunk_id_to_root_hash.get(""),
            Some(merk.root_hash().unwrap()).as_ref()
        );

        // first restore the first chunk
        let (chunk, next_chunk_index) = chunk_producer.chunk_with_index(1).unwrap();
        let new_chunk_ids = restorer
            .process_chunk(traversal_instruction_as_string(&vec![]), chunk)
            .expect("should process chunk");
        assert_eq!(new_chunk_ids.len(), 4);
        assert_eq!(next_chunk_index, Some(2));
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 4);
        assert_eq!(restorer.parent_keys.len(), 4);

        // generate multi chunk from the 2nd chunk with no limit
        let multi_chunk = chunk_producer
            .multi_chunk_with_limit_and_index(next_chunk_index.unwrap(), None)
            .unwrap();
        // tree of height 4 has 5 chunks
        // we have restored the first leaving 4 chunks
        // each chunk has an extra chunk id, since they are disjoint
        // hence the size of the multi chunk should be 8
        assert_eq!(multi_chunk.chunk.len(), 8);
        let new_chunk_ids = restorer.process_multi_chunk(multi_chunk.chunk).unwrap();
        assert_eq!(new_chunk_ids.len(), 0);
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 0);
        assert_eq!(restorer.parent_keys.len(), 0);

        let restored_merk = restorer.finalize().expect("should be able to finalize");

        // compare root hash values
        assert_eq!(
            restored_merk.root_hash().unwrap(),
            merk.root_hash().unwrap()
        );
    }

    #[test]
    fn test_process_multi_chunk_with_limit() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let restoration_merk = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .unwrap();

        // restorer root hash should be empty
        assert_eq!(restoration_merk.root_hash().unwrap(), [0; 32]);

        // at the start both merks should have different root hash values
        assert_ne!(
            merk.root_hash().unwrap(),
            restoration_merk.root_hash().unwrap()
        );

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");
        let mut restorer = Restorer::new(restoration_merk, merk.root_hash().unwrap(), None);

        // build multi chunk with with limit of 325
        let multi_chunk = chunk_producer
            .multi_chunk_with_limit("", Some(600))
            .unwrap();
        // should only contain the first chunk
        assert_eq!(multi_chunk.chunk.len(), 2);
        // should point to chunk 2
        assert_eq!(multi_chunk.next_index, Some("11".to_string()));
        let next_ids = restorer.process_multi_chunk(multi_chunk.chunk).unwrap();
        assert_eq!(next_ids.len(), 4);
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 4);
        assert_eq!(restorer.parent_keys.len(), 4);

        // subsequent chunks are of size 321
        // with limit just above 642 should get 2 chunks (2 and 3)
        // disjoint, so multi chunk len should be 4
        let multi_chunk = chunk_producer
            .multi_chunk_with_limit(multi_chunk.next_index.unwrap().as_str(), Some(645))
            .unwrap();
        assert_eq!(multi_chunk.chunk.len(), 4);
        assert_eq!(multi_chunk.next_index, Some("01".to_string()));
        let next_ids = restorer.process_multi_chunk(multi_chunk.chunk).unwrap();
        // chunks 2 and 3 are leaf chunks
        assert_eq!(next_ids.len(), 0);
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 2);
        assert_eq!(restorer.parent_keys.len(), 2);

        // get the last 2 chunks
        let multi_chunk = chunk_producer
            .multi_chunk_with_limit(multi_chunk.next_index.unwrap().as_str(), Some(645))
            .unwrap();
        assert_eq!(multi_chunk.chunk.len(), 4);
        assert_eq!(multi_chunk.next_index, None);
        let next_ids = restorer.process_multi_chunk(multi_chunk.chunk).unwrap();
        // chunks 2 and 3 are leaf chunks
        assert_eq!(next_ids.len(), 0);
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 0);
        assert_eq!(restorer.parent_keys.len(), 0);

        // finalize merk
        let restored_merk = restorer.finalize().unwrap();

        // compare root hash values
        assert_eq!(
            restored_merk.root_hash().unwrap(),
            merk.root_hash().unwrap()
        );
    }

    // Builds a source merk with batch_size number of elements
    // attempts restoration on some empty merk, with multi chunks
    // verifies that restoration was performed correctly.
    fn test_restoration_multi_chunk_strategy(batch_size: u64, limit: Option<usize>) {
        // build the source merk
        let mut source_merk = TempMerk::new();
        let batch = make_batch_seq(0..batch_size);
        source_merk
            .apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");

        // build the restoration merk
        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let restoration_merk = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .unwrap();

        // at the start
        // restoration merk should have empty root hash
        // and source merk should have a different root hash
        assert_eq!(restoration_merk.root_hash().unwrap(), [0; 32]);
        assert_ne!(
            source_merk.root_hash().unwrap(),
            restoration_merk.root_hash().unwrap()
        );

        // instantiate chunk producer and restorer
        let mut chunk_producer =
            ChunkProducer::new(&source_merk).expect("should create chunk producer");
        let mut restorer = Restorer::new(restoration_merk, source_merk.root_hash().unwrap(), None);

        // perform chunk production and processing
        let mut chunk_id_opt = Some("".to_string());
        while let Some(chunk_id) = chunk_id_opt {
            let multi_chunk = chunk_producer
                .multi_chunk_with_limit(chunk_id.as_str(), limit)
                .expect("should get chunk");
            restorer
                .process_multi_chunk(multi_chunk.chunk)
                .expect("should process chunk successfully");
            chunk_id_opt = multi_chunk.next_index;
        }

        // after chunk processing we should be able to finalize
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 0);
        assert_eq!(restorer.parent_keys.len(), 0);
        let restored_merk = restorer.finalize().expect("should finalize");

        // compare root hash values
        assert_eq!(
            source_merk.root_hash().unwrap(),
            restored_merk.root_hash().unwrap()
        );
    }

    #[test]
    fn restore_multi_chunk_20_no_limit() {
        test_restoration_multi_chunk_strategy(20, None);
    }

    #[test]
    #[should_panic]
    fn restore_multi_chunk_20_tiny_limit() {
        test_restoration_multi_chunk_strategy(20, Some(1));
    }

    #[test]
    fn restore_multi_chunk_20_limit() {
        test_restoration_multi_chunk_strategy(20, Some(1200));
    }

    #[test]
    fn restore_multi_chunk_10000_limit() {
        test_restoration_multi_chunk_strategy(10000, Some(1200));
    }

    #[test]
    fn test_restoration_interruption() {
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let storage = TempStorage::new();
        let tx = storage.start_transaction();
        let restoration_merk = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .unwrap();

        // restorer root hash should be empty
        assert_eq!(restoration_merk.root_hash().unwrap(), [0; 32]);

        // at the start both merks should have different root hash values
        assert_ne!(
            merk.root_hash().unwrap(),
            restoration_merk.root_hash().unwrap()
        );

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");
        let mut restorer = Restorer::new(restoration_merk, merk.root_hash().unwrap(), None);

        assert_eq!(restorer.chunk_id_to_root_hash.len(), 1);
        assert_eq!(
            restorer.chunk_id_to_root_hash.get(""),
            Some(merk.root_hash().unwrap()).as_ref()
        );

        // first restore the first chunk
        let (chunk, next_chunk_index) = chunk_producer.chunk_with_index(1).unwrap();
        let new_chunk_ids = restorer
            .process_chunk(traversal_instruction_as_string(&vec![]), chunk)
            .expect("should process chunk");
        assert_eq!(new_chunk_ids.len(), 4);
        assert_eq!(next_chunk_index, Some(2));
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 4);
        assert_eq!(restorer.parent_keys.len(), 4);

        // store old state for later reference
        let old_chunk_id_to_root_hash = restorer.chunk_id_to_root_hash.clone();
        let old_parent_keys = restorer.parent_keys.clone();

        // drop the restorer and the restoration merk
        drop(restorer);
        // open the restoration merk again and build a restorer from it
        let restoration_merk = Merk::open_base(
            storage
                .get_immediate_storage_context(SubtreePath::empty(), &tx)
                .unwrap(),
            false,
            None::<&fn(&[u8]) -> Option<ValueDefinedCostType>>,
        )
        .unwrap()
        .unwrap();
        let mut restorer = Restorer::new(restoration_merk, merk.root_hash().unwrap(), None);

        // assert the state of the restorer
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 1);
        assert_eq!(restorer.parent_keys.len(), 0);

        // recover state
        let recovery_attempt = restorer.attempt_state_recovery();
        assert!(recovery_attempt.is_ok());
        assert_eq!(restorer.chunk_id_to_root_hash.len(), 4);
        assert_eq!(restorer.parent_keys.len(), 4);

        // assert equality to old state
        assert_eq!(old_chunk_id_to_root_hash, restorer.chunk_id_to_root_hash);
        assert_eq!(old_parent_keys, restorer.parent_keys);
    }
}
