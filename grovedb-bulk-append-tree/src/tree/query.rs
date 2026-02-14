//! Read operations for BulkAppendTree.

use super::{
    keys::{buffer_key, epoch_key},
    BulkAppendTree,
};
use crate::{epoch::deserialize_epoch_blob, BulkAppendError, BulkStore};

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

        let epoch_idx = position / self.epoch_size as u64;
        let intra_idx = (position % self.epoch_size as u64) as u32;
        let completed_epochs = self.epoch_count();

        if epoch_idx < completed_epochs {
            // Read from completed epoch blob
            let ekey = epoch_key(epoch_idx);
            match store.get(&ekey) {
                Ok(Some(blob)) => {
                    let entries = deserialize_epoch_blob(&blob)?;
                    if (intra_idx as usize) < entries.len() {
                        Ok(Some(entries[intra_idx as usize].clone()))
                    } else {
                        Err(BulkAppendError::CorruptedData(format!(
                            "epoch {} has only {} entries, requested index {}",
                            epoch_idx,
                            entries.len(),
                            intra_idx
                        )))
                    }
                }
                Ok(None) => Err(BulkAppendError::CorruptedData(format!(
                    "missing epoch blob {}",
                    epoch_idx
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

    /// Get a completed epoch blob by epoch index.
    ///
    /// Returns `None` if the epoch hasn't been completed yet.
    pub fn get_epoch<S: BulkStore>(
        &self,
        store: &S,
        epoch_index: u64,
    ) -> Result<Option<Vec<u8>>, BulkAppendError> {
        if epoch_index >= self.epoch_count() {
            return Ok(None);
        }
        let ekey = epoch_key(epoch_index);
        store.get(&ekey).map_err(BulkAppendError::StorageError)
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
