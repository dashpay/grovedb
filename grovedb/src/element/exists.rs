#[cfg(feature = "full")]
use costs::CostResult;
#[cfg(feature = "full")]
use merk::Merk;
#[cfg(feature = "full")]
use storage::StorageContext;

#[cfg(feature = "full")]
use crate::{Element, Error};

impl Element {
    #[cfg(feature = "full")]
    /// Helper function that returns whether an element at the key for the
    /// element already exists.
    pub fn element_at_key_already_exists<'db, K: AsRef<[u8]>, S: StorageContext<'db>>(
        &self,
        merk: &mut Merk<S>,
        key: K,
    ) -> CostResult<bool, Error> {
        merk.exists(key.as_ref())
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }
}
