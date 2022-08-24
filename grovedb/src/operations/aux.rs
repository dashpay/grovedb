use costs::{
    cost_return_on_error_no_add, CostResult, CostsExt, KeyValueStorageCost, OperationCost,
};
use storage::StorageContext;

use crate::{util::meta_storage_context_optional_tx, Error, GroveDb, TransactionArg};

impl GroveDb {
    pub fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        meta_storage_context_optional_tx!(self.db, transaction, aux_storage, {
            cost_return_on_error_no_add!(
                &cost,
                aux_storage
                    .unwrap_add_cost(&mut cost)
                    .put_aux(key.as_ref(), value, cost_info)
                    .unwrap_add_cost(&mut cost)
                    .map_err(|e| e.into())
            );
        });

        Ok(()).wrap_with_cost(cost)
    }

    pub fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        meta_storage_context_optional_tx!(self.db, transaction, aux_storage, {
            cost_return_on_error_no_add!(
                &cost,
                aux_storage
                    .unwrap_add_cost(&mut cost)
                    .delete_aux(key.as_ref())
                    .unwrap_add_cost(&mut cost)
                    .map_err(|e| e.into())
            );
        });

        Ok(()).wrap_with_cost(cost)
    }

    pub fn get_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> CostResult<Option<Vec<u8>>, Error> {
        let mut cost = OperationCost::default();

        meta_storage_context_optional_tx!(self.db, transaction, aux_storage, {
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
