use crate::{util::merk_optional_tx, Error, GroveDb, TransactionArg};

impl GroveDb {
    pub fn is_empty_tree<'p, P>(&self, path: P, transaction: TransactionArg) -> Result<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: Clone + DoubleEndedIterator + ExactSizeIterator,
    {
        let path_iter = path.into_iter();
        self.check_subtree_exists_path_not_found(path_iter.clone(), None, transaction)?;
        merk_optional_tx!(self.db, path_iter, transaction, subtree, {
            Ok(subtree.is_empty_tree())
        })
    }
}
