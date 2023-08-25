// TODO: add MIT License
// TODO: add module description

use std::io::Write;

// TODO: figure out better nomenclature
use crate::{proofs::chunk::binary_range::BinaryRange, Error};
use crate::{proofs::chunk::error::ChunkError, Error::InternalError};

// TODO: add documentation
fn chunk_height_per_layer(height: usize) -> Vec<usize> {
    // every chunk has a fixed height of 2
    // it is possible for a chunk to not reach full capacity
    let mut two_count = height / 2;
    if height % 2 != 0 {
        two_count += 1;
    }

    return vec![2; two_count];
}

/// Represents the height as a linear combination of 3 amd 2
/// of the form 3x + 2y
/// this breaks the tree into layers of height 3 or 2
/// the minimum chunk height is 2, so if tree height is less than 2
/// we just return a single layer of height 2
fn chunk_height_per_layer_lin_comb(height: usize) -> Vec<usize> {
    let mut two_count = 0;
    let mut three_count = height / 3;

    // minimum chunk height is 2, if tree height is less than 2
    // return a single layer with chunk height 2
    if height < 2 {
        two_count = 1;
    } else {
        match height % 3 {
            0 => { /* do nothing */ }
            1 => {
                // reduce the three_count by 1
                // so the remainder becomes 3 + 1
                // which is equivalent to 2 + 2
                three_count = three_count - 1;
                two_count += 2;
            }
            2 => {
                // remainder is a factor of 2
                // just increase the two_count
                two_count += 1;
            }
            // this is unreachable because height is a positive number
            // remainder set after diving by 3 is fixed to [0,1,2]
            _ => unreachable!(""),
        }
    }

    let mut layer_heights = vec![3; three_count];
    layer_heights.extend(vec![2; two_count]);

    layer_heights
}

/// Return the layer a chunk subtree belongs to
pub fn chunk_layer(height: usize, chunk_id: usize) -> Result<usize, Error> {
    // remaining depth tells us how deep in the tree the specified chunk is
    let mut remaining_depth = generate_traversal_instruction(height, chunk_id)?.len() + 1;
    let layer_heights = chunk_height_per_layer(height);

    let mut layer = 1;

    while remaining_depth > 1 {
        // remaining depth will always larger than the next layer height
        // if it is not already 1
        // this is because a every chunk always starts at a layer boundary
        // and remaining depth points to a chunk
        debug_assert!(remaining_depth > layer_heights[layer - 1]);

        remaining_depth = remaining_depth - layer_heights[layer - 1];
        layer = layer + 1;
    }

    Ok(layer - 1)
}

/// Return the depth of a chunk given the height
/// and chunk id
pub fn chunk_height(height: usize, chunk_id: usize) -> Result<usize, Error> {
    let chunk_layer = chunk_layer(height, chunk_id)?;
    let layer_heights = chunk_height_per_layer(height);

    Ok(layer_heights[chunk_layer])
}

/// Given a tree of height h, return the number of chunks needed
/// to completely represent the tree
pub fn number_of_chunks(height: usize) -> usize {
    let layer_heights = chunk_height_per_layer(height);
    number_of_chunks_internal(layer_heights)
}

/// Locates the subtree represented by a chunk id and returns
/// the number of chunks under that subtree
pub fn number_of_chunks_under_chunk_id(height: usize, chunk_id: usize) -> Result<usize, Error> {
    let chunk_layer = chunk_layer(height, chunk_id)?;
    let layer_heights = chunk_height_per_layer(height);

    // we only care about the layer heights after the chunk layer
    // as we are getting the number of chunks under a subtree and not
    // the entire tree of height h
    Ok(number_of_chunks_internal(
        layer_heights[chunk_layer..].to_vec(),
    ))
}

/// Given the heights of a tree per layer, return the total number of chunks in
/// that tree
fn number_of_chunks_internal(layer_heights: Vec<usize>) -> usize {
    // a layer consists of 1 or more subtrees of a given height
    // here we figure out number of exit nodes from a single subtree for each layer
    let mut single_subtree_exits_per_layer = layer_heights
        .into_iter()
        .map(exit_node_count)
        .collect::<Vec<usize>>();

    // we don't care about exit nodes from the last layer
    // as that points to non-existent subtrees
    single_subtree_exits_per_layer.pop();

    // now we get the total exit nodes per layer
    // by multiplying the exits per subtree with the number of subtrees on that
    // layer
    let mut chunk_counts_per_layer = vec![1];
    for i in 0..single_subtree_exits_per_layer.len() {
        let previous_layer_chunk_count = chunk_counts_per_layer[i];
        let current_layer_chunk_count =
            previous_layer_chunk_count * single_subtree_exits_per_layer[i];
        chunk_counts_per_layer.push(current_layer_chunk_count);
    }

    return chunk_counts_per_layer.into_iter().sum();
}

/// Calculates the maximum number of exit nodes for a tree of height h.
fn exit_node_count(height: usize) -> usize {
    2_usize.pow(height as u32)
}

/// Generate instruction for traversing to a given chunk in a binary tree
pub fn generate_traversal_instruction(height: usize, chunk_id: usize) -> Result<Vec<bool>, Error> {
    let mut instructions = vec![];

    let total_chunk_count = number_of_chunks(height);

    // out of bounds
    if chunk_id < 1 || chunk_id > total_chunk_count {
        return Err(Error::ChunkingError(ChunkError::OutOfBounds(
            "chunk id out of bounds",
        )));
    }

    let mut chunk_range = BinaryRange::new(1, total_chunk_count).map_err(|_| {
        Error::ChunkingError(ChunkError::InternalError(
            "failed to initialize chunk range",
        ))
    })?;

    // total chunk count will always be odd because
    // from the initial chunk (1) we have an even number of
    // exit nodes, and they have even numbers of exit nodes ...
    // so total_chunk_count = 1 + some_even_number = odd
    debug_assert_eq!(chunk_range.odd(), true);

    // bisect and reduce the chunk range until we get to the desired chunk
    // we keep track of every left right decision we make
    while chunk_range.len() > 1 {
        if chunk_range.odd() {
            // checks if we last decision we made got us to the desired chunk id
            let advance_result = chunk_range.advance_range_start().unwrap();
            chunk_range = advance_result.0;
            if advance_result.1 == chunk_id {
                return Ok(instructions);
            }
        } else {
            // for even chunk range, we are at the decision point
            // we can either go left or right
            // we first check which half the desired chunk is
            // then follow that path
            let chunk_id_half = chunk_range
                .which_half(chunk_id)
                .expect("chunk id must exist in range");
            instructions.push(chunk_id_half);
            chunk_range = chunk_range
                .get_half(chunk_id_half)
                .expect("confirmed range is not odd");
        }
    }

    // chunk range len is exactly 1
    // this must be the desired chunk id
    // return instructions that got us here
    return Ok(instructions);
}

/// Convert traversal instruction to byte string
/// 1 represents left
/// 0 represents right
pub fn traversal_instruction_as_string(instruction: Vec<bool>) -> String {
    instruction
        .iter()
        .map(|v| if *v { "1" } else { "0" })
        .collect()
}

// TODO: move this to a better file
pub fn write_to_vec<W: Write>(dest: &mut W, value: &[u8]) -> Result<(), Error> {
    dest.write_all(value)
        .map_err(|_e| InternalError("failed to write to vector"))
}

#[cfg(test)]
mod test {
    use byteorder::LE;

    use super::*;
    use crate::proofs::chunk::chunk2::{LEFT, RIGHT};

    #[test]
    fn test_chunk_height_per_layer() {
        let layer_heights = chunk_height_per_layer(10);
        assert_eq!(layer_heights.iter().sum::<usize>(), 10);
        assert_eq!(layer_heights, [2, 2, 2, 2, 2]);

        let layer_heights = chunk_height_per_layer(45);
        assert_eq!(layer_heights.iter().sum::<usize>(), 46);
        assert_eq!(layer_heights, [2; 23]);

        let layer_heights = chunk_height_per_layer(2);
        assert_eq!(layer_heights.iter().sum::<usize>(), 2);
        assert_eq!(layer_heights, [2]);

        // height less than 2
        let layer_heights = chunk_height_per_layer(1);
        assert_eq!(layer_heights.iter().sum::<usize>(), 2);
        assert_eq!(layer_heights, [2]);

        let layer_heights = chunk_height_per_layer(0);
        assert_eq!(layer_heights.iter().sum::<usize>(), 0);
        assert_eq!(layer_heights, Vec::<usize>::new());
    }

    #[test]
    fn test_exit_node_count() {
        // tree with just one node has 2 exit nodes
        assert_eq!(exit_node_count(1), 2);

        // tree with height 2 has 4 exit nodes
        assert_eq!(exit_node_count(2), 4);

        // tree with height 6 has 64 exit nodes
        assert_eq!(exit_node_count(6), 64);
    }

    #[test]
    fn test_number_of_chunks() {
        // given a chunk of height less than 3 chunk count should be 1
        assert_eq!(number_of_chunks(1), 1);
        assert_eq!(number_of_chunks(2), 1);

        // tree with height 4 should have 5 chunks
        // we split the tree into 2 layers of chunk height 2 each
        // first layer contains just one chunk (1), but has 4 exit nodes
        // hence total chunk count = 1 + 4 = 5
        assert_eq!(number_of_chunks(4), 5);

        // tree with height 6 should have 21 chunks
        // will be split into three layers of chunk height 2 = [2,2,2]
        // first chunk takes 1, has 2^2 = 4 exit nodes
        // second chunk takes 4 with each having 2^2 exit nodes
        // total exit from second chunk = 4 * 4 = 16
        // total chunks = 1 + 4 + 16 = 21
        assert_eq!(number_of_chunks(6), 21);

        // tree with height 10 should have 341 chunks
        // will be split into 5 layers = [2,2,2,2,2]
        // first layer has just 1 chunk, exit nodes = 2^2 = 4
        // second layer has 4 chunks, exit nodes = 2^2 * 4 = 16
        // third layer has 16 chunks, exit nodes = 2^2 * 16 = 64
        // fourth layer has 64 chunks, exit nodes = 2^2 * 64 = 256
        // fifth layer has 256 chunks
        // total chunks = 1 + 4 + 16 + 64 + 256 = 341 chunks
        assert_eq!(number_of_chunks(10), 341);
    }

    #[test]
    fn test_number_of_chunks_under_chunk_id() {
        // tree with height less than 3 should have just 1 chunk
        assert_eq!(number_of_chunks_under_chunk_id(1, 1).unwrap(), 1);
        assert_eq!(number_of_chunks_under_chunk_id(2, 1).unwrap(), 1);

        // asking for chunk out of bounds should return error
        assert!(number_of_chunks_under_chunk_id(1, 3).is_err());

        // tree with height 4 should have 5 chunks at chunk id 1
        // but 1 chunk at id 2 - 5
        assert_eq!(number_of_chunks_under_chunk_id(4, 1).unwrap(), 5);
        assert_eq!(number_of_chunks_under_chunk_id(4, 2).unwrap(), 1);
        assert_eq!(number_of_chunks_under_chunk_id(4, 3).unwrap(), 1);
        assert_eq!(number_of_chunks_under_chunk_id(4, 4).unwrap(), 1);
        assert_eq!(number_of_chunks_under_chunk_id(4, 5).unwrap(), 1);

        // tree with height 10 should have 341 chunks
        // layer_heights = [2, 2, 2, 2, 2]
        // chunk_id 1 = 341
        // chunk_id 2 = 85 i.e (341 - 1) / 2^2
        // chunk_id 3 = 21 i.e (85 - 1) / 2^2
        // chunk_id 4 = 5 i.e (21 - 1) / 2^2
        // chunk_id 5 = 1 i.e (5 - 1) / 2^2
        // chunk_id 6 = 1 on the same layer as 5
        // chunk_id 87 = 85 as chunk 87 should wrap back to the same layer as chunk_id 2
        // chunk_id 88 = mirrors chunk_id 3
        // chunk_id 89 = mirrors chunk_id 4
        // chunk_id 90 = mirrors chunk_id 5
        assert_eq!(number_of_chunks_under_chunk_id(10, 1).unwrap(), 341);
        assert_eq!(number_of_chunks_under_chunk_id(10, 2).unwrap(), 85);
        assert_eq!(number_of_chunks_under_chunk_id(10, 3).unwrap(), 21);
        assert_eq!(number_of_chunks_under_chunk_id(10, 4).unwrap(), 5);
        assert_eq!(number_of_chunks_under_chunk_id(10, 5).unwrap(), 1);
        assert_eq!(number_of_chunks_under_chunk_id(10, 6).unwrap(), 1);
        assert_eq!(number_of_chunks_under_chunk_id(10, 87).unwrap(), 85);
        assert_eq!(number_of_chunks_under_chunk_id(10, 88).unwrap(), 21);
        assert_eq!(number_of_chunks_under_chunk_id(10, 89).unwrap(), 5);
        assert_eq!(number_of_chunks_under_chunk_id(10, 90).unwrap(), 1);
    }

    #[test]
    fn test_traversal_instruction_generation() {
        //              3
        //           /      \
        //          1         7
        //        /    \    /   \
        //       0       2 5      8
        //                / \      \
        //               4   6      9
        // height: 4
        // layer_height: 3, 3
        //              3
        //           /      \
        //          1         7
        //        /    \    /   \
        //       0       2 5      8
        // ............................
        //                / \      \
        //               4   6      9
        // 5 chunks
        // chunk 1 entry - 3
        // chunk 2 entry - 0
        // chunk 3 entry - 2
        // chunk 4 entry - 5
        // chunk 5 entry - 8

        // chunk 1 entry - 3 is at the top of the tree so empty instruction set
        let instruction =
            generate_traversal_instruction(4, 1).expect("should generate traversal instruction");
        let empty_instruction: &[bool] = &[];
        assert_eq!(instruction, empty_instruction);

        // chunk 2 entry - 0
        // go left twice from root i.e 3 left -> 1 left -> 0
        let instruction =
            generate_traversal_instruction(4, 2).expect("should generate traversal instruction");
        assert_eq!(instruction, &[LEFT, LEFT]);

        // chunk 3 entry - 2
        // go left then right from root i.e 3 left -> 1 right -> 2
        let instruction =
            generate_traversal_instruction(4, 3).expect("should generate traversal instruction");
        assert_eq!(instruction, &[LEFT, RIGHT]);

        // chunk 4 entry - 5
        // go right then left i.e 3 right -> 7 left -> 5
        let instruction =
            generate_traversal_instruction(4, 4).expect("should generate traversal instruction");
        assert_eq!(instruction, &[RIGHT, LEFT]);

        // chunk 5 entry - 8
        // go right twice i.e 3 right -> 7 right -> 8
        let instruction =
            generate_traversal_instruction(4, 5).expect("should generate traversal instruction");
        assert_eq!(instruction, &[RIGHT, RIGHT]);

        // out of bound tests
        assert_eq!(generate_traversal_instruction(4, 6).is_err(), true);
        assert_eq!(generate_traversal_instruction(4, 0).is_err(), true);
    }

    #[test]
    fn test_chunk_height() {
        // tree of height 6
        // all chunks have the same height
        // since layer height = [2,2,2]
        // we have 21 chunks in a tree of this height
        for i in 1..=21 {
            assert_eq!(chunk_height(6, i).unwrap(), 2);
        }

        // tree of height 5
        // layer_height = [2, 2]
        // we also have 21 chunks here
        for i in 1..=21 {
            assert_eq!(chunk_height(5, i).unwrap(), 2);
        }

        // tree of height 10
        // layer_height = [3, 3, 3, 3]
        // just going to check chunk 1 - 5
        assert_eq!(chunk_height(10, 1).unwrap(), 2);
        assert_eq!(chunk_height(10, 2).unwrap(), 2);
        assert_eq!(chunk_height(10, 3).unwrap(), 2);
        assert_eq!(chunk_height(10, 4).unwrap(), 2);
        assert_eq!(chunk_height(10, 5).unwrap(), 2);
    }

    #[test]
    fn test_traversal_instruction_as_string() {
        assert_eq!(traversal_instruction_as_string(vec![]), "");
        assert_eq!(traversal_instruction_as_string(vec![LEFT]), "1");
        assert_eq!(traversal_instruction_as_string(vec![RIGHT]), "0");
        assert_eq!(
            traversal_instruction_as_string(vec![RIGHT, LEFT, LEFT, RIGHT]),
            "0110"
        );
    }
}
