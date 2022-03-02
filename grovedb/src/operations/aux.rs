use storage::StorageContext;

use crate::{Error, GroveDb, TransactionArg};

impl GroveDb {
    pub fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        transaction: TransactionArg,
    ) -> Result<(), Error> {
        if let Some(tx) = transaction {
            let aux_storage = self.db.get_prefixed_transactional_context(Vec::new(), tx);
            aux_storage.put_aux(key, value)?;
        } else {
            let aux_storage = self.db.get_prefixed_context(Vec::new());
            aux_storage.put_aux(key, value)?;
        }
        Ok(())
    }

    pub fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> Result<(), Error> {
        if let Some(tx) = transaction {
            let aux_storage = self.db.get_prefixed_transactional_context(Vec::new(), tx);
            aux_storage.delete_aux(key)?;
        } else {
            let aux_storage = self.db.get_prefixed_context(Vec::new());
            aux_storage.delete_aux(key)?;
        }
        Ok(())
    }

    pub fn get_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        transaction: TransactionArg,
    ) -> Result<Option<Vec<u8>>, Error> {
        if let Some(tx) = transaction {
            let aux_storage = self.db.get_prefixed_transactional_context(Vec::new(), tx);
            Ok(aux_storage.get_aux(key)?)
        } else {
            let aux_storage = self.db.get_prefixed_context(Vec::new());
            Ok(aux_storage.get_aux(key)?)
        }
    }
}
