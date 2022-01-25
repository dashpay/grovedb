use storage::rocksdb_storage::OptimisticTransactionDBTransaction;

use crate::{Element, Error, GroveDb};

impl GroveDb {
    pub fn delete(
        &mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<(), Error> {
        if let None = transaction {
            if self.is_readonly {
                return Err(Error::DbIsInReadonlyMode);
            }
        }
        if path.is_empty() {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leafs currently cannot be deleted",
            ))
        } else {
            let element = self.get_raw(path, &key, transaction)?;
            {
                // To get the merk, we have two functions,
                // 1. get the subtree without transaction
                // 2. get with transaction
                // - if temp doesn't have get without transaction
                // how do we return a reference to the memory location tho
                // if we might have to remove the merk so it is always the correct instance
                let mut merk = self.get_subtrees().get(path, transaction)?;
                // let mut merk;
                // match transaction {
                //     None => {
                //         merk = self.get_subtrees().get_subtree_without_transaction(path)?;
                //     },
                //     Some(_) => {
                //         let prefix = &Self::compress_subtree_key(path, None);
                //         if self.temp_subtrees.borrow().contains_key(prefix) {
                //             // get the merk out
                //             merk =
                // self.temp_subtrees.borrow_mut().remove(prefix).expect("confirmed it's in the
                // hashmap");         } else {
                //             // merk is not in the hash map get it without transaction
                //             merk =
                // self.get_subtrees().get_subtree_without_transaction(path)?;
                //         }
                //     }
                // }

                // let subtrees = match transaction {
                //     None => &mut self.subtrees,
                //     Some(_) => &mut self.temp_subtrees,
                // };

                // let mut merk = subtrees
                //     .get_mut(&Self::compress_subtree_key(path, None))
                //     .ok_or(Error::InvalidPath("no subtree found under that path"))?;
                Element::delete(&mut merk, key.clone(), transaction)?;

                // after deletion, if there is a transaction, add the merk back into the hashmap
                self.get_subtrees()
                    .insert_temp_tree(path, merk, transaction);
            }

            if let Element::Tree(_) = element {
                // TODO: dumb traversal should not be tolerated
                let mut concat_path: Vec<Vec<u8>> = path.iter().map(|x| x.to_vec()).collect();
                concat_path.push(key);
                let subtrees_paths = self.find_subtrees(concat_path, transaction)?;
                // let subtrees = match transaction {
                //     None => &mut self.subtrees,
                //     Some(_) => &mut self.temp_subtrees,
                // };

                for subtree_path in subtrees_paths {
                    // TODO: eventually we need to do something about this nested slices
                    let subtree_path_ref: Vec<&[u8]> =
                        subtree_path.iter().map(|x| x.as_slice()).collect();
                    let prefix = Self::compress_subtree_key(&subtree_path_ref, None);
                    // Here we seem to get the subtree, then call clear on it
                    // Seems to only care about subtrees that are non transactional
                    let subtree = self
                        .get_subtrees()
                        .get_subtree_without_transaction(subtree_path_ref.as_slice());
                    if subtree.is_ok() {
                        subtree
                            .expect("confirmed it's valid")
                            .clear(transaction)
                            .map_err(|e| {
                                Error::CorruptedData(format!(
                                    "unable to cleanup tree from storage: {}",
                                    e
                                ))
                            })?;
                    }
                }
            }
            self.propagate_changes(path, transaction)?;
            Ok(())
        }
    }

    // TODO: dumb traversal should not be tolerated
    /// Finds keys which are trees for a given subtree recursively.
    /// One element means a key of a `merk`, n > 1 elements mean relative path
    /// for a deeply nested subtree.
    pub(crate) fn find_subtrees(
        &self,
        path: Vec<Vec<u8>>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Vec<Vec<Vec<u8>>>, Error> {
        let mut queue: Vec<Vec<Vec<u8>>> = vec![path.clone()];
        let mut result: Vec<Vec<Vec<u8>>> = vec![path];

        while let Some(q) = queue.pop() {
            // TODO: eventually we need to do something about this nested slices
            let q_ref: Vec<&[u8]> = q.iter().map(|x| x.as_slice()).collect();
            // Get the correct subtree with q_ref as path
            let merk = self.get_subtrees().get(&q_ref, transaction)?;
            let mut iter = Element::iterator(merk.raw_iter());
            // let mut iter = self.elements_iterator(&q_ref, transaction)?;
            while let Some((key, value)) = iter.next()? {
                match value {
                    Element::Tree(_) => {
                        let mut sub_path = q.clone();
                        sub_path.push(key);
                        queue.push(sub_path.clone());
                        result.push(sub_path);
                    }
                    _ => {}
                }
            }
        }
        Ok(result)
    }
}
