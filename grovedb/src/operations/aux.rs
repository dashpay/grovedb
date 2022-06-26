use costs::{cost_return_on_error_no_add, CostContext, CostsExt, OperationCost};
use storage::StorageContext;

use crate::{util::meta_storage_context_optional_tx, Error, GroveDb, TransactionArg};

impl GroveDb {
    pub fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        transaction: TransactionArg,
    ) -> CostContext<Result<(), Error>> {
        let mut cost = OperationCost::default();

        meta_storage_context_optional_tx!(self.db, transaction, aux_storage, {
            cost_return_on_error_no_add!(
                &cost,
                aux_storage
                    .put_aux(key.as_ref(), value)
                    .map_err(|e| e.into())
            );
        });

        cost.seek_count = 1;
        cost.storage_written_bytes = key.as_ref().len() as u32 + value.len() as u32;
        Ok(()).wrap_with_cost(cost)
    }

    pub fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> CostContext<Result<(), Error>> {
        let mut cost = OperationCost::default();

        meta_storage_context_optional_tx!(self.db, transaction, aux_storage, {
            cost_return_on_error_no_add!(
                &cost,
                aux_storage.delete_aux(key.as_ref()).map_err(|e| e.into())
            );
        });

        cost.seek_count = 1;
        cost.storage_written_bytes = key.as_ref().len() as u32;
        Ok(()).wrap_with_cost(cost)
    }

    pub fn get_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> CostContext<Result<Option<Vec<u8>>, Error>> {
        let mut cost = OperationCost::default();

        meta_storage_context_optional_tx!(self.db, transaction, aux_storage, {
            let value =
                cost_return_on_error_no_add!(&cost, aux_storage.get_aux(key).map_err(|e| e.into()));

            cost = OperationCost {
                seek_count: 1,
                loaded_bytes: value.as_ref().map(|v| v.len()).unwrap_or(0) as u32,
                ..Default::default()
            };

            Ok(value).wrap_with_cost(cost)
        })
    }
}
