use std::collections::{HashMap, HashSet};

use merk::Merk;
use storage::StorageContext;

use super::BatchError;

/// Intermediate in-memory state of the batch to keep track of subtrees
/// accesses.
struct CachedSubtrees<S, F> {
    subtrees: HashMap<Vec<Vec<u8>>, Merk<S>>,
    removed_subtrees: HashSet<Vec<Vec<u8>>>,
    get_merk_fn: F,
}

impl<S, F, E> CachedSubtrees<S, F>
where
    E: std::error::Error,
    F: Fn(&[Vec<u8>]) -> Result<Merk<S>, E>,
{
    /// Create empty subtrees cache.
    fn new(get_merk_fn: F) -> Self {
        Self {
            subtrees: HashMap::new(),
            removed_subtrees: HashSet::new(),
            get_merk_fn,
        }
    }

    /// Get subtree from cache or from database in case it's taken for the first
    /// time and put it into cache.
    fn get(&mut self, path: Vec<Vec<u8>>) -> Result<&mut Merk<S>, BatchError> {
        if self.removed_subtrees.contains(&path) {
            return Err(BatchError::DeletedSubtreeAccess);
        }
        self.insert(path)
    }

    /// Open and return a subtree . While `get` also does the same it will fail
    /// if subtree was deleted before, `insert` on the other hand means explicit
    /// tree insertion even after subtree was removed.
    fn insert(&mut self, path: Vec<Vec<u8>>) -> Result<&mut Merk<S>, BatchError> {
        self.removed_subtrees.remove(&path);
        if !self.subtrees.contains_key(&path) {
            self.subtrees.insert(
                path.to_vec(),
                (self.get_merk_fn)(&path).map_err(|e| BatchError::MerkError(e.to_string()))?,
            );
        }
        Ok(self
            .subtrees
            .get_mut(&path)
            .expect("must exist at this point"))
    }

    /// Mark subtree as deleted, this will invalidate all subsequent actions on
    /// it unless it will be inserted explicitly again.
    fn delete<'db>(&mut self, path: Vec<Vec<u8>>) -> Result<(), BatchError>
    where
        S: StorageContext<'db>,
    {
        if let Some(mut s) = self.subtrees.remove(&path) {
            s.clear()
                .map_err(|e| BatchError::MerkError(e.to_string()))?;
        }
        self.removed_subtrees.insert(path);
        Ok(())
    }
}
