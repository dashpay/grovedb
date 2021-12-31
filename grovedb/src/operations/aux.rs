use storage::Storage;

use crate::{Error, GroveDb};

impl GroveDb {
    pub fn put_aux(&mut self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        Ok(self.meta_storage.put_aux(key, value)?)
    }

    pub fn delete_aux(&mut self, key: &[u8]) -> Result<(), Error> {
        Ok(self.meta_storage.delete_aux(key)?)
    }

    pub fn get_aux(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.meta_storage.get_aux(key)?)
    }
}
