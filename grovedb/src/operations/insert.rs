use std::rc::Rc;

use storage::{rocksdb_storage, Storage};

use crate::{Element, Error, GroveDb, Merk, PrefixedRocksDbStorage};

/// A helper function that builds a prefix for a key under a path and opens a
/// Merk instance.
fn create_merk_with_prefix(
    db: Rc<rocksdb_storage::OptimisticTransactionDB>,
    path: &[&[u8]],
    key: &[u8],
) -> Result<(Vec<u8>, Merk<PrefixedRocksDbStorage>), Error> {
    let subtree_prefix = GroveDb::compress_subtree_key(path, Some(key));
    Ok((
        subtree_prefix.clone(),
        Merk::open(PrefixedRocksDbStorage::new(db, subtree_prefix)?)
            .map_err(|e| Error::CorruptedData(e.to_string()))?,
    ))
}

impl GroveDb {
    pub fn insert<'a: 'b, 'b>(
        &'a mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        element: Element,
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<(), Error> {
        if transaction.is_none() && self.is_readonly {
            return Err(Error::DbIsInReadonlyMode);
        }

        match element {
            Element::Tree(_) => {
                if path.is_empty() {
                    self.add_root_leaf(&key, transaction)?;
                } else {
                    self.add_non_root_subtree(path, key, transaction)?;
                }
            }
            _ => {
                // If path is empty that means there is an attempt to insert something into a
                // root tree and this branch is for anything but trees
                if path.is_empty() {
                    return Err(Error::InvalidPath(
                        "only subtrees are allowed as root tree's leafs",
                    ));
                }

                let (mut merk, prefix) = self
                    .get_subtrees()
                    .get(path, transaction)
                    .map_err(|_| Error::InvalidPath("no subtree found under that path"))?;
                element.insert(&mut merk, key, transaction)?;
                if let Some(prefix) = prefix {
                    self.get_subtrees()
                        .insert_temp_tree_with_prefix(prefix, merk, transaction);
                } else {
                    self.get_subtrees()
                        .insert_temp_tree(path, merk, transaction);
                }
                self.propagate_changes(path, transaction)?;
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
        let (subtree_prefix, subtree_merk) = create_merk_with_prefix(self.db.clone(), &[], key)?;
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
        self.propagate_changes(&[key], transaction)?;
        Ok(())
    }

    // Add subtree to another subtree.
    // We want to add a new empty merk to another merk at a key
    // first make sure other merk exist
    // if it exists, then create merk to be inserted, and get root hash
    // we only care about root hash of merk to be inserted
    //
    fn add_non_root_subtree<'a: 'b, 'b>(
        &'a mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<(), Error> {
        if transaction.is_none() &&  self.is_readonly {
            return Err(Error::DbIsInReadonlyMode);
        }

        // First, check if a subtree exists to create a new subtree under it
        let (parent, prefix) = self.get_subtrees().get(path, transaction)?;
        if let Some(prefix) = prefix {
            self.get_subtrees()
                .insert_temp_tree_with_prefix(prefix, parent, transaction);
        } else {
            self.get_subtrees()
                .insert_temp_tree(path, parent, transaction);
        }

        let (subtree_prefix, subtree_merk) = create_merk_with_prefix(self.db.clone(), path, &key)?;

        // Set tree value as a a subtree root hash
        let element = Element::Tree(subtree_merk.root_hash());
        self.get_subtrees()
            .insert_temp_tree_with_prefix(subtree_prefix, subtree_merk, transaction);

        // Had to take merk from `subtrees` once again to solve multiple &mut s
        let (mut merk, prefix) = self
            .get_subtrees()
            .get(path, transaction)
            .expect("confirmed subtree exists above");

        // need to mark key as taken in the upper tree
        element.insert(&mut merk, key, transaction)?;
        if let Some(prefix) = prefix {
            self.get_subtrees()
                .insert_temp_tree_with_prefix(prefix, merk, transaction);
        } else {
            self.get_subtrees()
                .insert_temp_tree(path, merk, transaction);
        }

        self.propagate_changes(path, transaction)?;

        Ok(())
    }

    pub fn insert_if_not_exists<'a: 'b, 'b>(
        &mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        element: Element,
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<bool, Error> {
        if self.get(path, &key, transaction).is_ok() {
            return Ok(false);
        }
        match self.insert(path, key, element, transaction) {
            Ok(_) => Ok(true),
            Err(e) => Err(e),
        }
    }
}
