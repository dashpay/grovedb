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

//! GroveDB storage_cost layer worst case costs traits.
//!
//! Nothing here needs a storage engine: an estimate is a function of the
//! lengths a path may have and of the cost model.

use grovedb_costs::{cost_model::StorageCostModel, OperationCost};

/// Worst Key Length should be implemented for items being used
/// for get_storage_context_cost path items
pub trait WorstKeyLength {
    /// the max length of the key
    fn max_length(&self) -> u8;
}

/// Return worst case cost for storage context creation.
///
/// It is what `model` charges for a subtree handle opened by its path
/// (rule P1 of `docs/lignindb/cost-model.md`), with every segment of `path`
/// at the longest length it may have.
pub fn get_storage_context_cost<L: WorstKeyLength>(
    model: StorageCostModel,
    path: &[L],
) -> OperationCost {
    model.open_by_path(path.iter().map(|segment| usize::from(segment.max_length())))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Longest(u8);

    impl WorstKeyLength for Longest {
        fn max_length(&self) -> u8 {
            self.0
        }
    }

    const MODEL: StorageCostModel = StorageCostModel::V0;

    fn estimate(segment_lens: &[u8]) -> OperationCost {
        let path: Vec<Longest> = segment_lens.iter().copied().map(Longest).collect();
        get_storage_context_cost(MODEL, &path)
    }

    fn hashes(blocks: u32) -> OperationCost {
        OperationCost {
            hash_node_calls: blocks,
            ..Default::default()
        }
    }

    #[test]
    fn the_root_subtree_costs_nothing_to_open() {
        assert_eq!(estimate(&[]), OperationCost::default());
    }

    #[test]
    fn a_path_is_charged_the_hash_blocks_of_its_longest_form() {
        let counted = size_of::<usize>() as u8;
        assert_eq!(estimate(&[0]), hashes(1));
        assert_eq!(estimate(&[63 - counted]), hashes(1));
        assert_eq!(estimate(&[64 - counted]), hashes(2));
        assert_eq!(estimate(&[70]), hashes(2));
        assert_eq!(estimate(&[62 - counted, 0]), hashes(1));
        assert_eq!(estimate(&[62 - counted, 1]), hashes(2));
        assert_eq!(estimate(&[255, 0, 255]), hashes(9));
        assert_eq!(estimate(&[255; 4]), hashes(17));
    }

    #[test]
    fn the_estimate_is_what_the_model_charges_for_the_handle() {
        for segment_lens in [&[][..], &[0], &[1], &[55], &[56], &[255, 0, 255]] {
            assert_eq!(
                estimate(segment_lens),
                MODEL.open_by_path(segment_lens.iter().map(|len| usize::from(*len)))
            );
        }
    }
}
