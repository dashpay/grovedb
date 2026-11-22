//! Checkpoints of a grove: the committed state of an open database in a
//! directory of its own, which opens as a database.
//!
//! The three calls are written for any engine, and each is the engine's
//! call of the same name ([`Engine::create_checkpoint`],
//! [`Engine::open_checkpoint`], [`Engine::delete_checkpoint`]).
//! `docs/lignindb/engine.md`, "Checkpoints", says what each does on the
//! LigninDB engine. What a caller has to know of that engine is here:
//!
//! - **A creation builds in a directory beside its target**, under the
//!   target's name with `.creating` added, and gives it the target's name
//!   when the checkpoint is whole. A creation that failed or was cut can
//!   leave that directory. It is no checkpoint: it is not opened, and
//!   whoever lists a directory of checkpoints does not count it. It keeps
//!   disk space until it goes: a later creation at the same target takes
//!   it over, and a deletion of the target or of that directory itself
//!   removes it. A target whose own name ends in `.creating`, in any case
//!   of its ASCII letters, is refused.
//! - **A file system can list a name in another spelling than the one it
//!   was given.** Whoever keeps checkpoints keeps the names it made them
//!   under, and compares those, not what the directory lists.
//! - **A directory that holds a file named `MANIFEST.deleting` is not
//!   opened**, by `GroveDb::open_checkpoint` or by `GroveDb::open`: that
//!   is the name a deletion gives the manifest of what it removes.
//!   `GroveDb::delete_checkpoint` finishes such a deletion. It goes by
//!   what the files hold and not by that name, so it also removes a
//!   whole database beside which something else made a file of that
//!   name: whoever wants to keep such a database removes that one file.

use std::path::Path;

use crate::{Engine, Error, GroveDb};

impl<E: Engine> GroveDb<E> {
    /// Creates a checkpoint: the state that is committed now, in a new
    /// directory at `path`, which opens as a database of its own. A
    /// `path` that is there already is refused and left as it is.
    pub fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        self.db.create_checkpoint(path).map_err(|e| e.into())
    }

    /// Opens a checkpoint on engine `E`. [`GroveDb::open_checkpoint`] does
    /// this on the default engine. No database is made where there is
    /// none.
    pub fn open_checkpoint_with_engine<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        Ok(Self::from_engine(E::open_checkpoint(path)?))
    }

    /// Deletes a checkpoint directory of engine `E`, which no handle has
    /// open. [`GroveDb::delete_checkpoint`] does this on the default
    /// engine.
    ///
    /// A path of fewer than two components is refused before the engine
    /// is asked. What the engine removes, and what it refuses and leaves
    /// as it is, is [`Engine::delete_checkpoint`]'s to say.
    pub fn delete_checkpoint_with_engine<P: AsRef<Path>>(path: P) -> Result<(), Error> {
        let path = path.as_ref();

        // Safety: prevent deletion of root or near-root paths
        let component_count = path.components().count();
        if component_count < 2 {
            return Err(Error::CorruptedData(
                "refusing to delete checkpoint: path too short (safety check)".to_string(),
            ));
        }

        E::delete_checkpoint(path)
    }
}

#[cfg(any(feature = "rocksdb_storage", feature = "lignin_storage"))]
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
    ///
    /// On the LigninDB engine the checkpoint is not opened: the engine
    /// reads the manifest of the directory to know that it holds a
    /// database of its own, and refuses a directory that holds none, or
    /// that a handle has open. The module documentation says what else it
    /// removes there.
    pub fn delete_checkpoint<P: AsRef<Path>>(path: P) -> Result<(), Error> {
        Self::delete_checkpoint_with_engine(path)
    }
}
