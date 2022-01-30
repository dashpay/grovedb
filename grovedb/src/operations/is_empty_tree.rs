use std::collections::HashMap;

use merk::Merk;
use storage::{
    rocksdb_storage::{OptimisticTransactionDBTransaction, PrefixedRocksDbStorage},
    RawIterator,
};

use crate::{Error, GroveDb};

impl GroveDb {
    pub fn is_empty_tree(
        &self,
        path: &[&[u8]],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<bool, Error> {
        let (merk, _) = self.get_subtrees().get(path, transaction)?;

        Ok(merk.is_empty_tree())
    }
}
