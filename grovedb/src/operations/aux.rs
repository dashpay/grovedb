use storage::{Storage, Transaction};

use crate::{Error, GroveDb, PrefixedRocksDbStorage};

impl GroveDb {
    pub fn put_aux<'a: 'b, 'b>(
        &'a mut self,
        key: &[u8],
        value: &[u8],
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<(), Error> {
        if let Some(tx) = transaction {
            let transaction = self.meta_storage.transaction(tx);
            transaction.put_aux(key, value)?;
            Ok(())
        } else {
            if self.is_readonly {
                return Err(Error::DbIsInReadonlyMode);
            }
            Ok(self.meta_storage.put_aux(key, value)?)
        }
    }

    pub fn delete_aux<'a: 'b, 'b>(
        &'a mut self,
        key: &[u8],
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<(), Error> {
        if let Some(tx) = transaction {
            let transaction = self.meta_storage.transaction(tx);
            transaction.delete_aux(key)?;
            Ok(())
        } else {
            if self.is_readonly {
                return Err(Error::DbIsInReadonlyMode);
            }
            Ok(self.meta_storage.delete_aux(key)?)
        }
    }

    pub fn get_aux<'a: 'b, 'b>(
        &'a mut self,
        key: &[u8],
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<Option<Vec<u8>>, Error> {
        if let Some(tx) = transaction {
            let transaction = self.meta_storage.transaction(tx);
            Ok(transaction.get_aux(key)?)
        } else {
            Ok(self.meta_storage.get_aux(key)?)
        }
    }
}
