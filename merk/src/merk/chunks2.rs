// TODO: add MIT License
// TODO: add module description
// TODO: figure out verification features

use std::{
    cmp::max,
    collections::{LinkedList, VecDeque},
    path::Iter,
};

use ed::Encode;
use grovedb_costs::{CostResult, CostsExt, OperationCost};
use grovedb_storage::StorageContext;
use integer_encoding::VarInt;

use crate::{
    error::Error,
    proofs::{
        chunk::{
            chunk_op::ChunkOp,
            error::{ChunkError, ChunkError::InternalError},
            util::{
                chunk_height, generate_traversal_instruction, number_of_chunks,
                traversal_instruction_as_string, write_to_vec,
            },
        },
        Node, Op,
    },
    tree::RefWalker,
    Error::ChunkingError,
    Merk, PanicSource,
};

// TODO: move types to some other file
// TODO: add documentation
#[derive(Debug)]
pub struct SubtreeChunk {
    chunk: Vec<Op>,
    next_index: Option<usize>,
    remaining_limit: Option<usize>,
}

impl SubtreeChunk {
    pub fn new(chunk: Vec<Op>, next_index: Option<usize>, remaining_limit: Option<usize>) -> Self {
        Self {
            chunk,
            next_index,
            remaining_limit,
        }
    }
}

#[derive(Debug)]
pub struct MultiChunk {
    pub chunk: Vec<ChunkOp>,
    pub next_index: Option<usize>,
    pub remaining_limit: Option<usize>,
}

impl MultiChunk {
    pub fn new(
        chunk: Vec<ChunkOp>,
        next_index: Option<usize>,
        remaining_limit: Option<usize>,
    ) -> Self {
        Self {
            chunk,
            next_index,
            remaining_limit,
        }
    }
}

/// A `ChunkProducer` allows the creation of chunk proofs, used for trustlessly
/// replicating entire Merk trees. Chunks can be generated on the fly in a
/// random order, or iterated in order for slightly better performance.
pub struct ChunkProducer<'db, S> {
    /// Represents the max height of the Merk tree
    height: usize,
    /// Represents the index of the next chunk
    index: usize,
    merk: &'db Merk<S>,
}

impl<'db, S> ChunkProducer<'db, S>
where
    S: StorageContext<'db>,
{
    /// Creates a new `ChunkProducer` for the given `Merk` instance
    pub(crate) fn new(merk: &'db Merk<S>) -> Result<Self, Error> {
        let tree_height = merk
            .height()
            .ok_or(Error::ChunkingError(ChunkError::EmptyTree(
                "cannot create chunk producer for empty Merk",
            )))?;
        Ok(Self {
            height: tree_height as usize,
            index: 1,
            merk,
        })
    }

    /// Gets the chunk with the given index. Errors if the index is out of
    /// bounds or the tree is empty - the number of chunks can be checked by
    /// calling `producer.len()`.
    pub fn chunk(&mut self, index: usize) -> Result<Vec<Op>, Error> {
        // ensure that the chunk index is within bounds
        let max_chunk_index = self.len();
        if index < 1 || index > max_chunk_index {
            return Err(ChunkingError(ChunkError::OutOfBounds(
                "chunk index out of bounds",
            )));
        }

        self.index = index + 1;

        let traversal_instructions = generate_traversal_instruction(self.height, index)?;

        let chunk_height = chunk_height(self.height, index).unwrap();

        self.merk.walk(|maybe_walker| match maybe_walker {
            Some(mut walker) => {
                walker.traverse_and_build_chunk(&traversal_instructions, chunk_height)
            }
            None => Err(Error::ChunkingError(ChunkError::EmptyTree(
                "cannot create chunk producer for empty Merk",
            ))),
        })
    }

    // TODO: add documentation
    pub fn multi_chunk_with_limit(
        &mut self,
        index: usize,
        limit: Option<usize>,
    ) -> Result<MultiChunk, Error> {
        // TODO: what happens if the vec is filled?
        //  we need to have some kind of hardhoc limit value if none is supplied.
        //  maybe we can just do something with the length to fix this?
        let mut chunk = vec![];

        let mut current_index = Some(index);
        let mut current_limit = limit;

        // generate as many subtree chunks as we can
        // until we have exhausted all or hit a limit restriction
        while current_index != None {
            let current_index_traversal_instruction = generate_traversal_instruction(
                self.height,
                current_index.expect("confirmed is Some"),
            )?;
            let chunk_id_op = ChunkOp::ChunkId(current_index_traversal_instruction);

            // factor in the ChunkId encoding length in limit calculations
            let temp_limit = if let Some(limit) = current_limit {
                let chunk_id_op_encoding_len = chunk_id_op.encoding_length().map_err(|e| {
                    Error::ChunkingError(ChunkError::InternalError("cannot get encoding length"))
                })?;
                if limit >= chunk_id_op_encoding_len {
                    Some(limit - chunk_id_op_encoding_len)
                } else {
                    Some(0)
                }
            } else {
                None
            };

            let subtree_multi_chunk_result = self.subtree_multi_chunk_with_limit(
                current_index.expect("confirmed is not None"),
                temp_limit,
            );

            let limit_too_small_error = matches!(
                subtree_multi_chunk_result,
                Err(ChunkingError(ChunkError::LimitTooSmall(..)))
            );

            if limit_too_small_error {
                if chunk.is_empty() {
                    // no progress, return limit too small error
                    return Err(Error::ChunkingError(ChunkError::LimitTooSmall(
                        "limit too small for initial chunk",
                    )));
                } else {
                    // made progress, send accumulated chunk
                    break;
                }
            }

            let subtree_multi_chunk = subtree_multi_chunk_result?;

            chunk.push(chunk_id_op);
            chunk.push(ChunkOp::Chunk(subtree_multi_chunk.chunk));

            // update loop parameters
            current_index = subtree_multi_chunk.next_index;
            current_limit = subtree_multi_chunk.remaining_limit;
        }

        Ok(MultiChunk::new(chunk, current_index, current_limit))
    }

    /// Packs as many chunks as it can from a starting chunk index, into a
    /// vector. Stops when we have exhausted all chunks or we have reached
    /// some limit.
    pub fn subtree_multi_chunk_with_limit(
        &mut self,
        index: usize,
        limit: Option<usize>,
    ) -> Result<SubtreeChunk, Error> {
        let mut chunk_byte_length = 0;

        let max_chunk_index = number_of_chunks(self.height);
        let mut chunk_index = index;

        // we first get the chunk at the given index
        let chunk_ops = self.chunk(chunk_index)?;
        chunk_byte_length = chunk_ops.encoding_length().map_err(|e| {
            Error::ChunkingError(ChunkError::InternalError("can't get encoding length"))
        })?;
        chunk_index += 1;

        let mut chunk = VecDeque::from(chunk_ops);

        // ensure the limit is not less than first chunk byte length
        // if it is we can't proceed and didn't make progress so we return an error
        if let Some(limit) = limit {
            if chunk_byte_length > limit {
                return Err(Error::ChunkingError(ChunkError::LimitTooSmall(
                    "limit too small for initial chunk",
                )));
            }
        }

        let mut iteration_index = 0;
        while iteration_index < chunk.len() {
            // we only perform replacements on Hash nodes
            if matches!(chunk[iteration_index], Op::Push(Node::Hash(..))) {
                let replacement_chunk = self.chunk(chunk_index)?;

                // calculate the new total
                let new_total = replacement_chunk.encoding_length().map_err(|e| {
                    Error::ChunkingError(ChunkError::InternalError("can't get encoding length"))
                })? + chunk_byte_length
                    - chunk[iteration_index].encoding_length().map_err(|e| {
                        Error::ChunkingError(ChunkError::InternalError("can't get encoding length"))
                    })?;

                // verify that this chunk doesn't make use exceed the limit
                if let Some(limit) = limit {
                    if new_total > limit {
                        let next_index = match chunk_index > max_chunk_index {
                            true => None,
                            _ => Some(chunk_index),
                        };

                        return Ok(SubtreeChunk::new(
                            chunk.into(),
                            next_index,
                            Some(limit - chunk_byte_length),
                        ));
                    }
                }

                chunk_byte_length = new_total;
                chunk_index += 1;

                chunk.remove(iteration_index);
                for op in replacement_chunk.into_iter().rev() {
                    chunk.insert(iteration_index, op);
                }
            } else {
                iteration_index += 1;
            }
        }

        let remaining_limit = limit.map(|l| l - chunk_byte_length);
        let next_index = match chunk_index > max_chunk_index {
            true => None,
            _ => Some(chunk_index),
        };

        Ok(SubtreeChunk::new(chunk.into(), next_index, remaining_limit))
    }

    /// Returns the total number of chunks for the underlying Merk tree.
    pub fn len(&self) -> usize {
        number_of_chunks(self.height as usize)
    }

    /// Gets the next chunk based on the `ChunkProducer`'s internal index state.
    /// This is mostly useful for letting `ChunkIter` yield the chunks in order,
    /// optimizing throughput compared to random access.
    // TODO: does this really optimize throughput, how can you make the statement
    // true?
    fn next_chunk(&mut self) -> Option<Result<Vec<Op>, Error>> {
        // for now not better than random access
        // TODO: fix
        let max_index = number_of_chunks(self.height);
        if self.index > max_index {
            return None;
        }

        let chunk = self.chunk(self.index);

        return Some(chunk);
    }

    // TODO: test this logic out
    fn get_chunk_encoding_length(chunk: &[Op]) -> usize {
        // TODO: deal with error
        chunk
            .iter()
            .fold(0, |sum, op| sum + op.encoding_length().unwrap())
    }
}

/// Iterate over each chunk, returning `None` after last chunk
impl<'db, S> Iterator for ChunkProducer<'db, S>
where
    S: StorageContext<'db>,
{
    type Item = Result<Vec<Op>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_chunk()
    }
}

impl<'db, S> Merk<S>
where
    S: StorageContext<'db>,
{
    /// Creates a `ChunkProducer` which can return chunk proofs for replicating
    /// the entire Merk tree.
    pub fn chunks(&'db self) -> Result<ChunkProducer<'db, S>, Error> {
        ChunkProducer::new(self)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        proofs::{
            chunk::chunk2::{
                tests::{traverse_get_kv_feature_type, traverse_get_node_hash},
                LEFT, RIGHT,
            },
            tree::execute,
            Tree,
        },
        test_utils::{make_batch_seq, TempMerk},
    };

    #[derive(Default)]
    struct NodeCounts {
        hash: usize,
        kv_hash: usize,
        kv: usize,
        kv_value_hash: usize,
        kv_digest: usize,
        kv_ref_value_hash: usize,
        kv_value_hash_feature_type: usize,
    }

    impl NodeCounts {
        fn sum(&self) -> usize {
            return self.hash
                + self.kv_hash
                + self.kv
                + self.kv_value_hash
                + self.kv_digest
                + self.kv_ref_value_hash
                + self.kv_value_hash_feature_type;
        }
    }

    fn count_node_types(tree: Tree) -> NodeCounts {
        let mut counts = NodeCounts::default();

        tree.visit_nodes(&mut |node| {
            match node {
                Node::Hash(_) => counts.hash += 1,
                Node::KVHash(_) => counts.kv_hash += 1,
                Node::KV(..) => counts.kv += 1,
                Node::KVValueHash(..) => counts.kv_value_hash += 1,
                Node::KVDigest(..) => counts.kv_digest += 1,
                Node::KVRefValueHash(..) => counts.kv_ref_value_hash += 1,
                Node::KVValueHashFeatureType(..) => counts.kv_value_hash_feature_type += 1,
            };
        });

        counts
    }

    #[test]
    fn test_merk_chunk_len() {
        // Tree of height 5 - max of 31 elements, min of 16 elements
        // 5 will be broken into 3 layers = [2, 2, 2]
        // exit nodes from first layer = 2^2 = 4
        // exit nodes from the second layer = 4 ^ 2^2 = 16
        // total_chunk = 1 + 4 + 16 = 21 chunks
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..20);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(5));
        let chunk_producer = ChunkProducer::new(&merk).unwrap();
        assert_eq!(chunk_producer.len(), 21);

        // Tree of height 10 - max of 1023 elements, min of 512 elements
        // 4 layers -> [2,2,2,2,2]
        // chunk_count_per_layer -> [1, 4, 16, 64, 256]
        // total = 341 chunks
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..1000);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(10));
        let chunk_producer = ChunkProducer::new(&merk).unwrap();
        assert_eq!(chunk_producer.len(), 341);
    }

    #[test]
    fn test_chunk_producer_iter() {
        // tree with height 4
        // full tree
        //              7
        //           /      \
        //        3            11
        //      /   \        /    \
        //     1     5      9      13
        //   /  \   / \    / \    /   \
        //  0   2  4   6  8  10  12   14
        // going to be broken into [2, 2]
        // that's a total of 5 chunks

        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");

        // build iterator from first chunk producer
        let mut chunks = merk.chunks().expect("should return producer");

        // ensure that the chunks gotten from the iterator is the same
        // as that from the chunk producer
        for i in 1..=5 {
            assert_eq!(
                chunks.next().unwrap().unwrap(),
                chunk_producer.chunk(i).unwrap()
            );
        }

        // returns None after max
        assert_eq!(chunks.next().is_none(), true);
    }

    #[test]
    fn test_random_chunk_access() {
        // tree with height 4
        // full tree
        //              7
        //           /      \
        //        3            11
        //      /   \        /    \
        //     1     5      9      13
        //   /  \   / \    / \    /   \
        //  0   2  4   6  8  10  12   14
        // going to be broken into [2, 2]
        // that's a total of 5 chunks

        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let mut inner_tree = merk.tree.take().expect("has inner tree");
        merk.tree.set(Some(inner_tree.clone()));

        // TODO: should I be using panic source?
        let mut tree_walker = RefWalker::new(&mut inner_tree, PanicSource {});

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");
        assert_eq!(chunk_producer.len(), 5);

        // assert bounds
        assert_eq!(chunk_producer.chunk(0).is_err(), true);
        assert_eq!(chunk_producer.chunk(6).is_err(), true);

        // first chunk
        // expected:
        //              7
        //           /      \
        //        3            11
        //      /   \        /    \
        //   H(1)   H(5)    H(9)   H(13)
        let chunk = chunk_producer.chunk(1).expect("should generate chunk");
        assert_eq!(chunk.len(), 13);
        assert_eq!(
            chunk,
            vec![
                Op::Push(traverse_get_node_hash(&mut tree_walker, &[LEFT, LEFT])),
                Op::Push(traverse_get_kv_feature_type(&mut tree_walker, &[LEFT])),
                Op::Parent,
                Op::Push(traverse_get_node_hash(&mut tree_walker, &[LEFT, RIGHT])),
                Op::Child,
                Op::Push(traverse_get_kv_feature_type(&mut tree_walker, &[])),
                Op::Parent,
                Op::Push(traverse_get_node_hash(&mut tree_walker, &[RIGHT, LEFT])),
                Op::Push(traverse_get_kv_feature_type(&mut tree_walker, &[RIGHT])),
                Op::Parent,
                Op::Push(traverse_get_node_hash(&mut tree_walker, &[RIGHT, RIGHT])),
                Op::Child,
                Op::Child
            ]
        );

        // second chunk
        // expected:
        //         1
        //        /  \
        //       0    2
        let chunk = chunk_producer.chunk(2).expect("should generate chunk");
        assert_eq!(chunk.len(), 5);
        assert_eq!(
            chunk,
            vec![
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[LEFT, LEFT, LEFT]
                )),
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[LEFT, LEFT]
                )),
                Op::Parent,
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[LEFT, LEFT, RIGHT]
                )),
                Op::Child
            ]
        );

        // third chunk
        // expected:
        //         5
        //        /  \
        //       4    6
        let chunk = chunk_producer.chunk(3).expect("should generate chunk");
        assert_eq!(chunk.len(), 5);
        assert_eq!(
            chunk,
            vec![
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[LEFT, RIGHT, LEFT]
                )),
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[LEFT, RIGHT]
                )),
                Op::Parent,
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[LEFT, RIGHT, RIGHT]
                )),
                Op::Child
            ]
        );

        // third chunk
        // expected:
        //         9
        //        /  \
        //       8    10
        let chunk = chunk_producer.chunk(4).expect("should generate chunk");
        assert_eq!(chunk.len(), 5);
        assert_eq!(
            chunk,
            vec![
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[RIGHT, LEFT, LEFT]
                )),
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[RIGHT, LEFT]
                )),
                Op::Parent,
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[RIGHT, LEFT, RIGHT]
                )),
                Op::Child
            ]
        );

        // third chunk
        // expected:
        //         13
        //        /  \
        //       12    14
        let chunk = chunk_producer.chunk(5).expect("should generate chunk");
        assert_eq!(chunk.len(), 5);
        assert_eq!(
            chunk,
            vec![
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[RIGHT, RIGHT, LEFT]
                )),
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[RIGHT, RIGHT]
                )),
                Op::Parent,
                Op::Push(traverse_get_kv_feature_type(
                    &mut tree_walker,
                    &[RIGHT, RIGHT, RIGHT]
                )),
                Op::Child
            ]
        );
    }

    #[test]
    fn test_subtree_chunk_no_limit() {
        // tree of height 4
        // 5 chunks
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        // generate multi chunk with no limit
        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");
        let mut chunk_result = chunk_producer
            .subtree_multi_chunk_with_limit(1, None)
            .expect("should generate chunk with limit");

        assert_eq!(chunk_result.remaining_limit, None);
        assert_eq!(chunk_result.next_index, None);

        let tree = execute(chunk_result.chunk.into_iter().map(Ok), false, |_| Ok(()))
            .unwrap()
            .expect("should reconstruct tree");
        assert_eq!(tree.hash().unwrap(), merk.root_hash().unwrap());

        // assert that all nodes are of type kv_value_hash_feature_type
        let node_counts = count_node_types(tree);
        assert_eq!(node_counts.hash, 0);
        assert_eq!(node_counts.kv_hash, 0);
        assert_eq!(node_counts.kv, 0);
        assert_eq!(node_counts.kv_value_hash, 0);
        assert_eq!(node_counts.kv_digest, 0);
        assert_eq!(node_counts.kv_ref_value_hash, 0);
        assert_eq!(node_counts.kv_value_hash_feature_type, 15);
    }

    #[test]
    fn test_subtree_chunk_with_limit() {
        // tree of height 4
        // 5 chunks
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");

        // initial chunk is of size 453, so limit of 10 is too small
        // should return an error
        let chunk = chunk_producer.subtree_multi_chunk_with_limit(1, Some(10));
        assert!(chunk.is_err());

        // get just the fist chunk
        let chunk_result = chunk_producer
            .subtree_multi_chunk_with_limit(1, Some(453))
            .expect("should generate chunk with limit");
        assert_eq!(chunk_result.remaining_limit, Some(0));
        assert_eq!(chunk_result.next_index, Some(2));

        let mut chunk = chunk_result.chunk;
        assert_eq!(chunk.encoding_length().unwrap(), 453);
        assert_eq!(chunk.len(), 13); // op count
        let tree = execute(chunk.into_iter().map(Ok), false, |_| Ok(()))
            .unwrap()
            .expect("should reconstruct tree");
        assert_eq!(tree.hash().unwrap(), merk.root_hash().unwrap());

        let node_counts = count_node_types(tree);
        assert_eq!(node_counts.kv_value_hash_feature_type, 3);
        assert_eq!(node_counts.hash, 4);
        assert_eq!(node_counts.sum(), 4 + 3);

        // get up to second chunk
        let chunk_result = chunk_producer
            .subtree_multi_chunk_with_limit(1, Some(737))
            .expect("should generate chunk with limit");
        assert_eq!(chunk_result.remaining_limit, Some(0));
        assert_eq!(chunk_result.next_index, Some(3));

        let mut chunk = chunk_result.chunk;
        assert_eq!(chunk.encoding_length().unwrap(), 737);
        assert_eq!(chunk.len(), 17); // op count
        let tree = execute(chunk.into_iter().map(Ok), false, |_| Ok(()))
            .unwrap()
            .expect("should reconstruct tree");
        assert_eq!(tree.hash().unwrap(), merk.root_hash().unwrap());

        let node_counts = count_node_types(tree);
        assert_eq!(node_counts.kv_value_hash_feature_type, 6);
        assert_eq!(node_counts.hash, 3);
        assert_eq!(node_counts.sum(), 6 + 3);

        // get up to third chunk
        let chunk_result = chunk_producer
            .subtree_multi_chunk_with_limit(1, Some(1021))
            .expect("should generate chunk with limit");
        assert_eq!(chunk_result.remaining_limit, Some(0));
        assert_eq!(chunk_result.next_index, Some(4));

        let mut chunk = chunk_result.chunk;
        assert_eq!(chunk.encoding_length().unwrap(), 1021);
        assert_eq!(chunk.len(), 21); // op count
        let tree = execute(chunk.into_iter().map(Ok), false, |_| Ok(()))
            .unwrap()
            .expect("should reconstruct tree");
        assert_eq!(tree.hash().unwrap(), merk.root_hash().unwrap());

        let node_counts = count_node_types(tree);
        assert_eq!(node_counts.kv_value_hash_feature_type, 9);
        assert_eq!(node_counts.hash, 2);
        assert_eq!(node_counts.sum(), 9 + 2);

        // get up to fourth chunk
        let chunk_result = chunk_producer
            .subtree_multi_chunk_with_limit(1, Some(1305))
            .expect("should generate chunk with limit");
        assert_eq!(chunk_result.remaining_limit, Some(0));
        assert_eq!(chunk_result.next_index, Some(5));

        let mut chunk = chunk_result.chunk;
        assert_eq!(chunk.encoding_length().unwrap(), 1305);
        assert_eq!(chunk.len(), 25); // op count
        let tree = execute(chunk.into_iter().map(Ok), false, |_| Ok(()))
            .unwrap()
            .expect("should reconstruct tree");
        assert_eq!(tree.hash().unwrap(), merk.root_hash().unwrap());

        let node_counts = count_node_types(tree);
        assert_eq!(node_counts.kv_value_hash_feature_type, 12);
        assert_eq!(node_counts.hash, 1);
        assert_eq!(node_counts.sum(), 12 + 1);

        // get up to fifth chunk
        let chunk_result = chunk_producer
            .subtree_multi_chunk_with_limit(1, Some(1589))
            .expect("should generate chunk with limit");
        assert_eq!(chunk_result.remaining_limit, Some(0));
        assert_eq!(chunk_result.next_index, None);

        let mut chunk = chunk_result.chunk;
        assert_eq!(chunk.encoding_length().unwrap(), 1589);
        assert_eq!(chunk.len(), 29); // op count
        let tree = execute(chunk.into_iter().map(Ok), false, |_| Ok(()))
            .unwrap()
            .expect("should reconstruct tree");
        assert_eq!(tree.hash().unwrap(), merk.root_hash().unwrap());

        let node_counts = count_node_types(tree);
        assert_eq!(node_counts.kv_value_hash_feature_type, 15);
        assert_eq!(node_counts.hash, 0);
        assert_eq!(node_counts.sum(), 15);

        // limit larger than total chunk
        let chunk_result = chunk_producer
            .subtree_multi_chunk_with_limit(1, Some(usize::MAX))
            .expect("should generate chunk with limit");
        assert_eq!(chunk_result.remaining_limit, Some(18446744073709550026));
        assert_eq!(chunk_result.next_index, None);

        let mut chunk = chunk_result.chunk;
        assert_eq!(chunk.encoding_length().unwrap(), 1589);
        assert_eq!(chunk.len(), 29); // op count
        let tree = execute(chunk.into_iter().map(Ok), false, |_| Ok(()))
            .unwrap()
            .expect("should reconstruct tree");
        assert_eq!(tree.hash().unwrap(), merk.root_hash().unwrap());

        let node_counts = count_node_types(tree);
        assert_eq!(node_counts.kv_value_hash_feature_type, 15);
        assert_eq!(node_counts.hash, 0);
        assert_eq!(node_counts.sum(), 15);
    }

    #[test]
    fn test_multi_chunk_with_no_limit_trunk() {
        // tree of height 4
        // 5 chunks
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");

        // we generate the chunk starting from index 2, this has no hash nodes
        // so no multi chunk will be generated
        let chunk_result = chunk_producer
            .multi_chunk_with_limit(1, None)
            .expect("should generate chunk with limit");

        assert_eq!(chunk_result.remaining_limit, None);
        assert_eq!(chunk_result.next_index, None);

        // should only contain 2 items, the starting chunk id and the entire tree
        assert_eq!(chunk_result.chunk.len(), 2);

        // assert items
        assert_eq!(chunk_result.chunk[0], ChunkOp::ChunkId(vec![]));
        if let ChunkOp::Chunk(chunk) = &chunk_result.chunk[1] {
            let tree = execute(chunk.clone().into_iter().map(Ok), false, |_| Ok(()))
                .unwrap()
                .expect("should reconstruct tree");
            assert_eq!(tree.hash().unwrap(), merk.root_hash().unwrap());
        } else {
            panic!("expected ChunkOp::Chunk");
        }
    }

    #[test]
    fn test_multi_chunk_with_no_limit_not_trunk() {
        // tree of height 4
        // 5 chunks
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");

        // we generate the chunk starting from index 2, this has no hash nodes
        // so no multi chunk will be generated
        let chunk_result = chunk_producer
            .multi_chunk_with_limit(2, None)
            .expect("should generate chunk with limit");

        assert_eq!(chunk_result.remaining_limit, None);
        assert_eq!(chunk_result.next_index, None);

        // chunk 2 - 5 will be considered separate subtrees
        // each will have an accompanying chunk id, so 8 elements total
        assert_eq!(chunk_result.chunk.len(), 8);

        // assert the chunk id's
        assert_eq!(chunk_result.chunk[0], ChunkOp::ChunkId(vec![LEFT, LEFT]));
        assert_eq!(chunk_result.chunk[2], ChunkOp::ChunkId(vec![LEFT, RIGHT]));
        assert_eq!(chunk_result.chunk[4], ChunkOp::ChunkId(vec![RIGHT, LEFT]));
        assert_eq!(chunk_result.chunk[6], ChunkOp::ChunkId(vec![RIGHT, RIGHT]));

        // assert the chunks
        assert_eq!(
            chunk_result.chunk[1],
            ChunkOp::Chunk(chunk_producer.chunk(2).expect("should generate chunk"))
        );
        assert_eq!(
            chunk_result.chunk[3],
            ChunkOp::Chunk(chunk_producer.chunk(3).expect("should generate chunk"))
        );
        assert_eq!(
            chunk_result.chunk[5],
            ChunkOp::Chunk(chunk_producer.chunk(4).expect("should generate chunk"))
        );
        assert_eq!(
            chunk_result.chunk[7],
            ChunkOp::Chunk(chunk_producer.chunk(5).expect("should generate chunk"))
        );
    }

    #[test]
    fn test_multi_chunk_with_limit() {
        // tree of height 4
        // 5 chunks
        let mut merk = TempMerk::new();
        let batch = make_batch_seq(0..15);
        merk.apply::<_, Vec<_>>(&batch, &[], None)
            .unwrap()
            .expect("apply failed");
        assert_eq!(merk.height(), Some(4));

        let mut chunk_producer = ChunkProducer::new(&merk).expect("should create chunk producer");

        // ensure that the remaining limit, next index and values given are correct
        // if limit is smaller than first chunk, we should get an error
        let chunk_result = chunk_producer.multi_chunk_with_limit(1, Some(5));
        assert!(matches!(
            chunk_result,
            Err(Error::ChunkingError(ChunkError::LimitTooSmall(..)))
        ));

        // get chunk 2
        // data size of chunk 2 is exactly 317
        // chunk op encoding for chunk 2 = 321
        // hence limit of 317 will be insufficient
        let chunk_result = chunk_producer.multi_chunk_with_limit(2, Some(317));
        assert!(matches!(
            chunk_result,
            Err(Error::ChunkingError(ChunkError::LimitTooSmall(..)))
        ));

        // get chunk 2 and 3
        // chunk 2 chunk op = 331
        // chunk 3 chunk op = 321
        let chunk_result = chunk_producer
            .multi_chunk_with_limit(2, Some(321 + 321 + 5))
            .expect("should generate chunk");
        assert_eq!(chunk_result.next_index, Some(4));
        assert_eq!(chunk_result.remaining_limit, Some(5));
        assert_eq!(chunk_result.chunk.len(), 4);
        assert_eq!(chunk_result.chunk[0], ChunkOp::ChunkId(vec![LEFT, LEFT]));
        assert_eq!(chunk_result.chunk[2], ChunkOp::ChunkId(vec![LEFT, RIGHT]));
    }
}
