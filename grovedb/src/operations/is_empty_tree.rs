use storage::rocksdb_storage::OptimisticTransactionDBTransaction;

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
        Ok(self
            .get_subtrees()
            .borrow_mut(path, transaction)?
            .apply(|s| s.is_empty_tree(transaction)))
    }
}
