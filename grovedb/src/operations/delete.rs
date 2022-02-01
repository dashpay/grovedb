use storage::rocksdb_storage::OptimisticTransactionDBTransaction;

use crate::{Element, Error, GroveDb};

impl GroveDb {
    pub fn delete<'a, P>(
        &mut self,
        path: P,
        key: &'a [u8],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        if transaction.is_none() && self.is_readonly {
            return Err(Error::DbIsInReadonlyMode);
        }
        let path_iter = path.into_iter();
        if path_iter.len() == 0 {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leafs currently cannot be deleted",
            ))
        } else {
            let element = self.get_raw(path_iter.clone(), key.as_ref(), transaction)?;
            {
                let (mut merk, prefix) = self.get_subtrees().get(path_iter.clone(), transaction)?;
                Element::delete(&mut merk, key, transaction)?;

                // after deletion, if there is a transaction, add the merk back into the hashmap
                if let Some(prefix) = prefix {
                    self.get_subtrees()
                        .insert_temp_tree_with_prefix(prefix, merk, transaction);
                } else {
                    self.get_subtrees()
                        .insert_temp_tree(path_iter.clone(), merk, transaction);
                }
            }

            if let Element::Tree(_) = element {
                // TODO: dumb traversal should not be tolerated
                let subtrees_paths =
                    self.find_subtrees(path_iter.clone().chain(std::iter::once(key)), transaction)?;

                for subtree_path in subtrees_paths {
                    let mut subtree = self.get_subtrees().get_subtree_without_transaction(
                        subtree_path.iter().map(|x| x.as_slice()),
                    )?;
                    subtree.clear(transaction).map_err(|e| {
                        Error::CorruptedData(format!("unable to cleanup tree from storage: {}", e))
                    })?;
                }
            }
            self.propagate_changes(path_iter, transaction)?;
            Ok(())
        }
    }

    // TODO: dumb traversal should not be tolerated
    /// Finds keys which are trees for a given subtree recursively.
    /// One element means a key of a `merk`, n > 1 elements mean relative path
    /// for a deeply nested subtree.
    pub(crate) fn find_subtrees<'a, P>(
        &self,
        path: P,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Vec<Vec<Vec<u8>>>, Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
    {
        // TODO: remove conversion to vec;
        // However, it's not easy for a reason:
        // new keys to enqueue are taken from raw iterator which returns Vec<u8>;
        // changing that to slice is hard as cursor should be moved for next iteration
        // which requires exclusive (&mut) reference, also there is no guarantee that
        // slice which points into storage internals will remain valid if raw iterator
        // got altered so why that reference should be exclusive;

        let mut queue: Vec<Vec<Vec<u8>>> =
            vec![path.into_iter().map(|x| x.as_ref().to_vec()).collect()];
        let mut result: Vec<Vec<Vec<u8>>> = queue.clone();

        while let Some(q) = queue.pop() {
            // Get the correct subtree with q_ref as path
            let (merk, _) = self
                .get_subtrees()
                .get(q.iter().map(|x| x.as_slice()), transaction)?;
            let mut raw_iter = Element::iterator(merk.raw_iter(transaction));
            while let Some((key, value)) = raw_iter.next()? {
                if let Element::Tree(_) = value {
                    let mut sub_path = q.clone();
                    sub_path.push(key.to_vec());
                    queue.push(sub_path.clone());
                    result.push(sub_path);
                }
            }
        }
        Ok(result)
    }
}
