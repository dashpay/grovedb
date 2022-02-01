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
        let (merk, _) = self.get_subtrees().get(path, transaction)?;

        Ok(merk.is_empty_tree())
    }
}
