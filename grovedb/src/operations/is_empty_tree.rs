use storage::{rocksdb_storage::OptimisticTransactionDBTransaction, RawIterator};

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

        let mut iter = merk.raw_iter(transaction);
        iter.seek_to_first();

        Ok(!iter.valid())
    }
}
