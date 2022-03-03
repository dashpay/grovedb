use crate::{Error, GroveDb, TransactionArg};
use crate::merk_optional_tx;

impl GroveDb {
    pub fn is_empty_tree<'p, P>(&self, path: P, transaction: TransactionArg) -> Result<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: Clone + DoubleEndedIterator,
    {
        merk_optional_tx!(self.db, path, transaction, subtree, {
            Ok(subtree.is_empty_tree())
        })
    }
}
