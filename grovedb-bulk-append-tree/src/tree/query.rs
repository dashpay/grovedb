//! Read operations for BulkAppendTree.

use super::{
    keys::{buffer_key, chunk_key},
    BulkAppendTree,
};
use crate::{chunk::deserialize_chunk_blob, BulkAppendError, BulkStore};

impl BulkAppendTree {
    /// Get a value by global 0-based position.
    pub fn get_value<S: BulkStore>(
        &self,
        store: &S,
        position: u64,
    ) -> Result<Option<Vec<u8>>, BulkAppendError> {
        if position >= self.total_count {
            return Ok(None);
        }

        let chunk_idx = position / self.chunk_size() as u64;
        let intra_idx = (position % self.chunk_size() as u64) as u32;
        let completed_chunks = self.chunk_count();

        if chunk_idx < completed_chunks {
            // Read from completed chunk blob
            let ckey = chunk_key(chunk_idx);
            match store.get(&ckey) {
                Ok(Some(blob)) => {
                    let entries = deserialize_chunk_blob(&blob)?;
                    if (intra_idx as usize) < entries.len() {
                        Ok(Some(entries[intra_idx as usize].clone()))
                    } else {
                        Err(BulkAppendError::CorruptedData(format!(
                            "chunk {} has only {} entries, requested index {}",
                            chunk_idx,
                            entries.len(),
                            intra_idx
                        )))
                    }
                }
                Ok(None) => Err(BulkAppendError::CorruptedData(format!(
                    "missing chunk blob {}",
                    chunk_idx
                ))),
                Err(e) => Err(BulkAppendError::StorageError(e)),
            }
        } else {
            // Read from current buffer
            let bkey = buffer_key(intra_idx);
            match store.get(&bkey) {
                Ok(v) => Ok(v),
                Err(e) => Err(BulkAppendError::StorageError(e)),
            }
        }
    }

    /// Get a completed chunk blob by chunk index.
    ///
    /// Returns `None` if the chunk hasn't been completed yet.
    pub fn get_chunk<S: BulkStore>(
        &self,
        store: &S,
        chunk_index: u64,
    ) -> Result<Option<Vec<u8>>, BulkAppendError> {
        if chunk_index >= self.chunk_count() {
            return Ok(None);
        }
        let ckey = chunk_key(chunk_index);
        store.get(&ckey).map_err(BulkAppendError::StorageError)
    }

    /// Get all current buffer entries.
    pub fn get_buffer<S: BulkStore>(&self, store: &S) -> Result<Vec<Vec<u8>>, BulkAppendError> {
        let bc = self.buffer_count();
        let mut entries = Vec::with_capacity(bc as usize);
        for i in 0..bc {
            let bkey = buffer_key(i);
            match store.get(&bkey) {
                Ok(Some(bytes)) => entries.push(bytes),
                Ok(None) => {
                    return Err(BulkAppendError::CorruptedData(format!(
                        "missing buffer entry {}",
                        i
                    )));
                }
                Err(e) => return Err(BulkAppendError::StorageError(e)),
            }
        }
        Ok(entries)
    }
}
