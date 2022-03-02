use merk::Merk;

use crate::{Error, GroveDb, TransactionArg};

impl GroveDb {
    pub fn is_empty_tree<'p, P>(&self, path: P, transaction: TransactionArg) -> Result<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: Clone + DoubleEndedIterator,
    {
        if let Some(tx) = transaction {
            let subtree_storage = self
                .db
                .get_prefixed_transactional_context_from_path(path, tx);
            let subtree = Merk::open(subtree_storage)
                .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))?;
            Ok(subtree.is_empty_tree())
        } else {
            let subtree_storage = self.db.get_prefixed_context_from_path(path);
            let subtree = Merk::open(subtree_storage)
                .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))?;
            Ok(subtree.is_empty_tree())
        }
    }
}
