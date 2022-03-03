use merk::Merk;

use crate::{merk_optional_tx, Element, Error, GroveDb, TransactionArg};

impl GroveDb {
    pub fn insert<'p, P>(
        &mut self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: TransactionArg,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
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
                // If path is empty that means there is an attempt to insert
                // something into a root tree and this branch is for anything
                // but trees
                if path_iter.len() == 0 {
                    return Err(Error::InvalidPath(
                        "only subtrees are allowed as root tree's leafs",
                    ));
                }
                merk_optional_tx!(self.db, path_iter.clone(), transaction, mut subtree, {
                    element.insert(&mut subtree, key)?;
                });
                self.propagate_changes(path_iter, transaction)?;
            }
        }
        Ok(())
    }

    /// Add subtree to the root tree
    fn add_root_leaf(&mut self, key: &[u8], transaction: TransactionArg) -> Result<(), Error> {
        let mut root_leaf_keys = if let Some(tx) = transaction {
            let meta_storage = self.db.get_prefixed_transactional_context(Vec::new(), tx);
            Self::get_root_leaf_keys(meta_storage)?
        } else {
            let meta_storage = self.db.get_prefixed_context(Vec::new());
            Self::get_root_leaf_keys(meta_storage)?
        };
        if root_leaf_keys.get(&key.to_vec()).is_none() {
            root_leaf_keys.insert(key.to_vec(), root_leaf_keys.len());
        }
        Ok(())
    }

    /// Add subtree to another subtree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    fn add_non_root_subtree<'p, P>(
        &mut self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let path_iter = path.into_iter();
        if let Some(tx) = transaction {
            let parent_storage = self
                .db
                .get_prefixed_transactional_context_from_path(path_iter.clone(), tx);
            let mut parent_subtree = Merk::open(parent_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            let child_storage = self.db.get_prefixed_transactional_context_from_path(
                path_iter.clone().chain(std::iter::once(key)),
                tx,
            );
            let child_subtree = Merk::open(child_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            let element = Element::Tree(child_subtree.root_hash());
            element.insert(&mut parent_subtree, key)?;
        } else {
            let parent_storage = self.db.get_prefixed_context_from_path(path_iter.clone());
            let mut parent_subtree = Merk::open(parent_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            let child_storage = self
                .db
                .get_prefixed_context_from_path(path_iter.clone().chain(std::iter::once(key)));
            let child_subtree = Merk::open(child_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            let element = Element::Tree(child_subtree.root_hash());
            element.insert(&mut parent_subtree, key)?;
        }
        self.propagate_changes(path_iter, transaction)?;
        Ok(())
    }

    pub fn insert_if_not_exists<'p, P>(
        &mut self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: TransactionArg,
    ) -> Result<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
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
