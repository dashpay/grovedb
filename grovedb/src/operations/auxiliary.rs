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

//! Auxiliary operations

#[cfg(feature = "full")]
use costs::{
    cost_return_on_error_no_add, storage_cost::key_value_cost::KeyValueStorageCost, CostResult,
    CostsExt, OperationCost,
};
#[cfg(feature = "full")]
use storage::StorageContext;
use storage::{Storage, StorageBatch};

#[cfg(feature = "full")]
use crate::{util::meta_storage_context_optional_tx, Error, GroveDb, TransactionArg};

#[cfg(feature = "full")]
impl GroveDb {
    /// Put op for aux storage
    pub fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let batch = StorageBatch::new();

        meta_storage_context_optional_tx!(self.db, Some(&batch), transaction, aux_storage, {
            cost_return_on_error_no_add!(
                &cost,
                aux_storage
                    .unwrap_add_cost(&mut cost)
                    .put_aux(key.as_ref(), value, cost_info)
                    .unwrap_add_cost(&mut cost)
                    .map_err(|e| e.into())
            );
        });

        self.db
            .commit_multi_context_batch(batch, transaction)
            .add_cost(cost)
            .map_err(Into::into)
    }

    /// Delete op for aux storage
    pub fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let batch = StorageBatch::new();

        meta_storage_context_optional_tx!(self.db, Some(&batch), transaction, aux_storage, {
            cost_return_on_error_no_add!(
                &cost,
                aux_storage
                    .unwrap_add_cost(&mut cost)
                    .delete_aux(key.as_ref(), cost_info)
                    .unwrap_add_cost(&mut cost)
                    .map_err(|e| e.into())
            );
        });

        self.db
            .commit_multi_context_batch(batch, transaction)
            .add_cost(cost)
            .map_err(Into::into)
    }

    /// Get op for aux storage
    pub fn get_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> CostResult<Option<Vec<u8>>, Error> {
        let mut cost = OperationCost::default();

        meta_storage_context_optional_tx!(self.db, None, transaction, aux_storage, {
            let value = cost_return_on_error_no_add!(
                &cost,
                aux_storage
                    .unwrap_add_cost(&mut cost)
                    .get_aux(key)
                    .unwrap_add_cost(&mut cost)
                    .map_err(|e| e.into())
            );

            Ok(value).wrap_with_cost(cost)
        })
    }
}
