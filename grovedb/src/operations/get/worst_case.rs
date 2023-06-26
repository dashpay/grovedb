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

//! Worst case get costs

#[cfg(feature = "full")]
use grovedb_costs::OperationCost;
#[cfg(feature = "full")]
use grovedb_storage::rocksdb_storage::RocksDbStorage;

#[cfg(feature = "full")]
use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    GroveDb,
};

#[cfg(feature = "full")]
impl GroveDb {
    /// Worst case cost for has raw
    pub fn worst_case_for_has_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_has_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
            in_parent_tree_using_sums,
        );
        cost
    }

    /// Worst case cost for get raw
    pub fn worst_case_for_get_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_get_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
            in_parent_tree_using_sums,
        );
        cost
    }

    /// Worst case cost for get
    pub fn worst_case_for_get(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        max_references_sizes: Vec<u32>,
        in_parent_tree_using_sums: bool,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_get_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
            in_parent_tree_using_sums,
            max_references_sizes,
        );
        cost
    }
}
