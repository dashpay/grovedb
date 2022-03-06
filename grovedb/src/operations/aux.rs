use std::iter::empty;

use storage::StorageContext;

use crate::{util::storage_context_optional_tx, Error, GroveDb, TransactionArg};

impl GroveDb {
    pub fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        transaction: TransactionArg,
    ) -> Result<(), Error> {
        storage_context_optional_tx!(self.db, empty(), transaction, aux_storage, {
            aux_storage.put_aux(key, value)?;
        });
        Ok(())
    }

    pub fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> Result<(), Error> {
        storage_context_optional_tx!(self.db, empty(), transaction, aux_storage, {
            aux_storage.delete_aux(key)?;
        });
        Ok(())
    }

    pub fn get_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> Result<Option<Vec<u8>>, Error> {
        storage_context_optional_tx!(self.db, empty(), transaction, aux_storage, {
            Ok(aux_storage.get_aux(key)?)
        })
    }
}
