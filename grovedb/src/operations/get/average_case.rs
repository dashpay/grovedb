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

//! Average case get costs

#[cfg(feature = "full")]
use costs::OperationCost;
#[cfg(feature = "full")]
use storage::rocksdb_storage::RocksDbStorage;

#[cfg(feature = "full")]
use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    GroveDb,
};

#[cfg(feature = "full")]
impl GroveDb {
    /// Get the Operation Cost for a has query that doesn't follow
    /// references with the following parameters
    pub fn average_case_for_has_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_has_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_element_size,
            in_parent_tree_using_sums,
        );
        cost
    }

    /// Get the Operation Cost for a has query where we estimate that we
    /// would get a tree with the following parameters
    pub fn average_case_for_has_raw_tree(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        is_sum_tree: bool,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_has_raw_tree_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_flags_size,
            is_sum_tree,
            in_parent_tree_using_sums,
        );
        cost
    }

    /// Get the Operation Cost for a get query that doesn't follow
    /// references with the following parameters
    pub fn average_case_for_get_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_get_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_element_size,
            in_parent_tree_using_sums,
        );
        cost
    }

    /// Get the Operation Cost for a get query with the following parameters
    pub fn average_case_for_get(
        path: &KeyInfoPath,
        key: &KeyInfo,
        in_parent_tree_using_sums: bool,
        estimated_element_size: u32,
        estimated_references_sizes: Vec<u32>,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_get_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            in_parent_tree_using_sums,
            estimated_element_size,
            estimated_references_sizes,
        );
        cost
    }

    /// Get the Operation Cost for a get query with the following parameters
    pub fn average_case_for_get_tree(
        path: &KeyInfoPath,
        key: &KeyInfo,
        estimated_flags_size: u32,
        is_sum_tree: bool,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_average_case_get_raw_tree_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            estimated_flags_size,
            is_sum_tree,
            in_parent_tree_using_sums,
        );
        cost
    }
}
