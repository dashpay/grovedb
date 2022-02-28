use std::rc::Rc;

use storage::{rocksdb_storage, Storage};

use crate::{Element, Error, GroveDb, Merk, PrefixedRocksDbStorage};

/// A helper function that builds a prefix for a key under a path and opens a
/// Merk instance.
fn create_merk_with_prefix<'a, P>(
    db: Rc<rocksdb_storage::OptimisticTransactionDB>,
    path: P,
    key: &'a [u8],
) -> Result<(Vec<u8>, Merk<PrefixedRocksDbStorage>), Error>
where
    P: IntoIterator<Item = &'a [u8]>,
{
    let subtree_prefix = GroveDb::compress_subtree_key(path, Some(key));
    Ok((
        subtree_prefix.clone(),
        Merk::open(PrefixedRocksDbStorage::new(db, subtree_prefix)?)
            .map_err(|e| Error::CorruptedData(e.to_string()))?,
    ))
}

impl GroveDb {
    pub fn insert<'a: 'b, 'b, 'c, P>(
        &'a mut self,
        path: P,
        key: &'c [u8],
        element: Element,
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'c [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        if transaction.is_none() && self.is_readonly {
            return Err(Error::DbIsInReadonlyMode);
        }
        let path_iter = path.into_iter();
        match element {
            Element::Tree(_) => {
                if path_iter.len() == 0 {
                    self.add_root_leaf(key, transaction)?;
                } else {
                    self.add_non_root_subtree(path_iter, key, transaction)?;
                }
            }
            _ => {
                // If path is empty that means there is an attempt to insert something into a
                // root tree and this branch is for anything but trees
                if path_iter.len() == 0 {
                    return Err(Error::InvalidPath(
                        "only subtrees are allowed as root tree's leafs",
                    ));
                }
                self.get_subtrees()
                    .borrow_mut(path_iter.clone(), transaction)?
                    .apply(|s| element.insert(s, key, transaction))?;
                self.propagate_changes(path_iter, transaction)?;
            }
        }
        Ok(())
    }

    /// Add subtree to the root tree
    fn add_root_leaf<'a: 'b, 'b>(
        &'a mut self,
        key: &[u8],
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<(), Error> {
        if transaction.is_none() && self.is_readonly {
            return Err(Error::DbIsInReadonlyMode);
        }

        // Open Merk and put handle into `subtrees` dictionary accessible by its
        // compressed path
        let (subtree_prefix, subtree_merk) = create_merk_with_prefix(self.db.clone(), [], key)?;
        self.get_subtrees()
            .insert_temp_tree_with_prefix(subtree_prefix, subtree_merk, transaction);

        let root_leaf_keys = match transaction {
            None => &mut self.root_leaf_keys,
            Some(_) => &mut self.temp_root_leaf_keys,
        };

        let root_tree = match transaction {
            None => &mut self.root_tree,
            Some(_) => &mut self.temp_root_tree,
        };
        // Update root leafs index to persist rs-merkle structure later
        if root_leaf_keys.get(&key.to_vec()).is_none() {
            root_leaf_keys.insert(key.to_vec(), root_tree.leaves_len());
        }
        self.propagate_changes([key], transaction)?;
        Ok(())
    }

    // Add subtree to another subtree.
    // We want to add a new empty merk to another merk at a key
    // first make sure other merk exist
    // if it exists, then create merk to be inserted, and get root hash
    // we only care about root hash of merk to be inserted
    //
    fn add_non_root_subtree<'a: 'b, 'b, 'c, P>(
        &'a mut self,
        path: P,
        key: &'c [u8],
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'c [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        if transaction.is_none() && self.is_readonly {
            return Err(Error::DbIsInReadonlyMode);
        }
        let subtrees = self.get_subtrees();
        let path_iter = path.into_iter();
        // First, check if a subtree exists to create a new subtree under it
        subtrees
            .borrow_mut(path_iter.clone(), transaction)
            .map_err(|e| {
                // When adding if the path does not exist, this means it is an invalid path
                if let Error::PathNotFound(str) = e {
                    Error::InvalidPath(str)
                } else {
                    e
                }
            })?;

        let (subtree_prefix, mut subtree_merk) =
            create_merk_with_prefix(self.db.clone(), path_iter.clone(), key)?;

        // If the subtree was deleted previously inside a transaction then we should
        // insert it as empty
        // TODO: open Merk on transactional data
        if transaction.is_some()
            && self
                .temp_deleted_subtrees
                .borrow()
                .contains(&subtree_prefix)
        {
            subtree_merk.clear(transaction).unwrap();
        }

        // Set tree value as a a subtree root hash
        let element = Element::Tree(subtree_merk.root_hash());
        self.get_subtrees()
            .insert_temp_tree_with_prefix(subtree_prefix, subtree_merk, transaction);

        subtrees
            .borrow_mut(path_iter.clone(), transaction)
            .expect("must exist at this point")
            .apply(|s| element.insert(s, key, transaction))?;
        self.propagate_changes(path_iter, transaction)?;

        Ok(())
    }

    pub fn insert_if_not_exists<'a: 'b, 'b, 'c, P>(
        &mut self,
        path: P,
        key: &'c [u8],
        element: Element,
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<bool, Error>
    where
        P: IntoIterator<Item = &'c [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let path_iter = path.into_iter();
        if self.get_raw(path_iter.clone(), key, transaction).is_ok() {
            Ok(false)
        } else {
            match self.insert(path_iter, key, element, transaction) {
                Ok(_) => Ok(true),
                Err(e) => Err(e),
            }
        }
    }
}
