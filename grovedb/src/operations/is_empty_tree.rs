use std::collections::HashMap;

use merk::Merk;
use storage::{
    rocksdb_storage::{OptimisticTransactionDBTransaction, PrefixedRocksDbStorage},
    RawIterator,
};

use crate::{Error, GroveDb};

impl GroveDb {
    pub fn is_empty_tree<'a, P>(
        &self,
        path: P,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<bool, Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
        <P as IntoIterator>::IntoIter: Clone + DoubleEndedIterator,
    {
        let (merk, prefix) = self.get_subtrees().get(path, transaction)?;
        let result = merk.is_empty_tree(transaction);
        if let Some(prefix) = prefix {
            self.get_subtrees()
                .insert_temp_tree_with_prefix(prefix, merk, transaction);
        }
        Ok(result)
    }
}
