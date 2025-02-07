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

use grovedb_costs::{
    cost_return_on_error, storage_cost::key_value_cost::KeyValueStorageCost, CostResult, CostsExt,
    OperationCost,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{Storage, StorageContext};

use crate::{util::TxRef, Error, GroveDb, TransactionArg};

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
        let tx = TxRef::new(&self.db, transaction);
        let batch = Default::default();

        let aux_storage = self
            .db
            .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), tx.as_ref())
            .unwrap_add_cost(&mut cost);

        cost_return_on_error!(
            &mut cost,
            aux_storage
                .put_aux(key.as_ref(), value, cost_info)
                .map_err(Into::into)
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    /// Delete op for aux storage
    pub fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);
        let batch = Default::default();

        let aux_storage = self
            .db
            .get_transactional_storage_context(SubtreePath::empty(), Some(&batch), tx.as_ref())
            .unwrap_add_cost(&mut cost);

        cost_return_on_error!(
            &mut cost,
            aux_storage
                .delete_aux(key.as_ref(), cost_info)
                .map_err(|e| e.into())
        );

        cost_return_on_error!(
            &mut cost,
            self.db
                .commit_multi_context_batch(batch, Some(tx.as_ref()))
                .map_err(Into::into)
        );

        tx.commit_local().wrap_with_cost(cost)
    }

    /// Get op for aux storage
    pub fn get_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> CostResult<Option<Vec<u8>>, Error> {
        let mut cost = OperationCost::default();
        let tx = TxRef::new(&self.db, transaction);

        let aux_storage = self
            .db
            .get_transactional_storage_context(SubtreePath::empty(), None, tx.as_ref())
            .unwrap_add_cost(&mut cost);

        aux_storage
            .get_aux(key.as_ref())
            .map_err(|e| e.into())
            .add_cost(cost)
    }
}
