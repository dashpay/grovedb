use crate::GroveDb;

impl GroveDb {
    /// Creates chunks iterator to replicate GroveDb.
    pub fn chunks_iter(&self) -> ChunksIter {
        self.chunks_iter_from(0)
    }

    /// Creates iterator to produce chunks starting from `n` in case replication
    /// was interrupted previously.
    pub fn chunks_iter_from(&self, from: usize) -> ChunksIter {
        todo!()
    }
}

/// Iterator over GroveDb chunks.
pub struct ChunksIter {}
