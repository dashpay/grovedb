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

//! Delete
//! Implements functions in Element for deleting

#[cfg(feature = "full")]
use grovedb_costs::{storage_cost::removal::StorageRemovedBytes, CostResult, CostsExt};
#[cfg(feature = "full")]
use grovedb_merk::{BatchEntry, Error as MerkError, Merk, MerkOptions, Op};
#[cfg(feature = "full")]
use grovedb_storage::StorageContext;

#[cfg(feature = "full")]
use crate::{Element, Error};

impl Element {
    #[cfg(feature = "full")]
    /// Delete an element from Merk under a key
    pub fn delete<'db, K: AsRef<[u8]>, C: StorageContext<'db>>(
        merk: &mut Merk<C>,
        key: K,
        merk_options: Option<MerkOptions>,
        is_layered: bool,
        is_sum: bool,
    ) -> CostResult<(), Error> {
        let op = match (is_sum, is_layered) {
            (true, true) => Op::DeleteLayeredMaybeSpecialized,
            (true, false) => Op::DeleteMaybeSpecialized,
            (false, true) => Op::DeleteLayered,
            (false, false) => Op::Delete,
        };
        let batch = [(key, op)];
        let uses_sum_nodes = merk.is_sum_tree;
        merk.apply_with_specialized_costs::<_, Vec<u8>>(
            &batch,
            &[],
            merk_options,
            &|key, value| {
                Self::specialized_costs_for_key_value(key, value, uses_sum_nodes)
                    .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    #[cfg(feature = "full")]
    /// Delete an element from Merk under a key
    pub fn delete_with_sectioned_removal_bytes<'db, K: AsRef<[u8]>, C: StorageContext<'db>>(
        merk: &mut Merk<C>,
        key: K,
        merk_options: Option<MerkOptions>,
        is_layered: bool,
        is_in_sum_tree: bool,
        sectioned_removal: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            MerkError,
        >,
    ) -> CostResult<(), Error> {
        let op = match (is_in_sum_tree, is_layered) {
            (true, true) => Op::DeleteLayeredMaybeSpecialized,
            (true, false) => Op::DeleteMaybeSpecialized,
            (false, true) => Op::DeleteLayered,
            (false, false) => Op::Delete,
        };
        let batch = [(key, op)];
        let uses_sum_nodes = merk.is_sum_tree;
        merk.apply_with_costs_just_in_time_value_update::<_, Vec<u8>>(
            &batch,
            &[],
            merk_options,
            &|key, value| {
                Self::specialized_costs_for_key_value(key, value, uses_sum_nodes)
                    .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
            },
            Some(&Element::value_defined_cost_for_serialized_value),
            &mut |_costs, _old_value, _value| Ok((false, None)),
            sectioned_removal,
        )
        .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    #[cfg(feature = "full")]
    /// Delete an element from Merk under a key to batch operations
    pub fn delete_into_batch_operations<K: AsRef<[u8]>>(
        key: K,
        is_layered: bool,
        is_sum: bool,
        batch_operations: &mut Vec<BatchEntry<K>>,
    ) -> CostResult<(), Error> {
        let op = match (is_sum, is_layered) {
            (true, true) => Op::DeleteLayeredMaybeSpecialized,
            (true, false) => Op::DeleteMaybeSpecialized,
            (false, true) => Op::DeleteLayered,
            (false, false) => Op::Delete,
        };
        let entry = (key, op);
        batch_operations.push(entry);
        Ok(()).wrap_with_cost(Default::default())
    }
}
