use std::path::Path;

use crate::{Engine, Error, GroveDb};

impl<E: Engine> GroveDb<E> {
    /// Creates a checkpoint
    pub fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        self.db.create_checkpoint(path).map_err(|e| e.into())
    }

    /// Opens a checkpoint on engine `E`. [`GroveDb::open_checkpoint`] does
    /// this on the default engine.
    pub fn open_checkpoint_with_engine<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        Ok(Self::from_engine(E::open_checkpoint(path)?))
    }

    /// Deletes a checkpoint directory of engine `E`.
    /// [`GroveDb::delete_checkpoint`] does this on the default engine.
    pub fn delete_checkpoint_with_engine<P: AsRef<Path>>(path: P) -> Result<(), Error> {
        let path = path.as_ref();

        // Safety: prevent deletion of root or near-root paths
        let component_count = path.components().count();
        if component_count < 2 {
            return Err(Error::CorruptedData(
                "refusing to delete checkpoint: path too short (safety check)".to_string(),
            ));
        }

        // Verify this is a valid checkpoint by attempting to open it
        // This ensures we only delete actual GroveDB checkpoints
        {
            let _checkpoint_db = Self::open_checkpoint_with_engine(path)?;
            // checkpoint_db is dropped here, closing the database
        }

        std::fs::remove_dir_all(path)
            .map_err(|e| Error::CorruptedData(format!("failed to delete checkpoint: {}", e)))
    }
}

#[cfg(feature = "rocksdb_storage")]
impl GroveDb {
    /// Opens a checkpoint
    pub fn open_checkpoint<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        Self::open_checkpoint_with_engine(path)
    }

    /// Deletes a checkpoint directory.
    ///
    /// This removes the checkpoint directory and all its contents.
    ///
    /// # Safety
    /// This function verifies that the path is a valid GroveDB checkpoint
    /// by attempting to open it first. If the checkpoint cannot be opened,
    /// deletion is refused to prevent accidental deletion of arbitrary
    /// directories.
    pub fn delete_checkpoint<P: AsRef<Path>>(path: P) -> Result<(), Error> {
        Self::delete_checkpoint_with_engine(path)
    }
}
