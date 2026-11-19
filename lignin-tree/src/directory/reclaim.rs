//! Taking out what a dropped Merk left in the directory.

use std::sync::Arc;

use super::{
    error::DirectoryError,
    record::{child_key, dropped_key, entry_key, root_key, Child, MerkId},
    Directory,
};
use crate::flat::{FlatPages, FlatPagesMut};

impl Directory {
    /// The keys of the records that `child` left, a Merk that was below
    /// the dropped or left-over Merk `parent` ([`Self::children`]), in the
    /// order in which they are taken out: the record of its root, its
    /// entry, and the record of its host.
    ///
    /// The entry and the record of the root are not among them if another
    /// Merk was entered under the prefix since: they are that Merk's.
    ///
    /// The Merks below `child` come first: it is refused with
    /// [`DirectoryError::Nested`] while one of them has a record. A Merk
    /// that is in use is refused with [`DirectoryError::InUse`].
    pub fn left_by<P: FlatPages + ?Sized>(
        &self,
        pages: &P,
        parent: MerkId,
        child: &Child,
    ) -> Result<Vec<Vec<u8>>, DirectoryError> {
        if self.has_children(pages, child.id)? {
            return Err(DirectoryError::Nested { id: child.id });
        }
        let mut keys = Vec::new();
        let entry = self.stored(pages, &child.prefix)?;
        if let Some(entry) = entry.filter(|entry| entry.id() == child.id) {
            if self.is_live(&entry) {
                return Err(DirectoryError::InUse {
                    prefix: child.prefix,
                });
            }
            if let Some(root) = entry.root()
                && self.root_owner(pages, root.address)? == Some(child.prefix)
            {
                keys.push(root_key(root.address).to_vec());
            }
            keys.push(entry_key(&child.prefix).to_vec());
        }
        keys.push(child_key(parent, child.position, &child.key));
        Ok(keys)
    }

    /// Takes out the records that `child` left ([`Self::left_by`]).
    pub fn reclaim_child<P: FlatPagesMut + ?Sized>(
        &self,
        pages: &mut P,
        parent: MerkId,
        child: &Child,
    ) -> Result<(), DirectoryError> {
        for key in self.left_by(pages, parent, child)? {
            self.tree.delete(pages, &key)?;
        }
        Ok(())
    }

    /// Takes out the record of the dropped Merk `id`: all it left is
    /// reclaimed. It is the last record to go, and is refused with
    /// [`DirectoryError::Nested`] while a Merk that was below it has one.
    pub fn reclaimed<P: FlatPagesMut + ?Sized>(
        &mut self,
        pages: &mut P,
        id: MerkId,
    ) -> Result<(), DirectoryError> {
        let key = dropped_key(id);
        if self.get(pages, &key)?.is_none() {
            return Err(DirectoryError::NotDropped { id });
        }
        if self.has_children(pages, id)? {
            return Err(DirectoryError::Nested { id });
        }
        self.tree.delete(pages, &key)?;
        if self.dropped.contains(&id) {
            Arc::make_mut(&mut self.dropped).remove(&id);
        }
        Ok(())
    }
}
