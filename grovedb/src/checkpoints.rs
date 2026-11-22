//! Checkpoints of a grove: the committed state of an open database in a
//! directory of its own, which opens as a database.
//!
//! The three calls are written for any engine, and each is the engine's
//! call of the same name ([`Engine::create_checkpoint`],
//! [`Engine::open_checkpoint`], [`Engine::delete_checkpoint`]). What each
//! does on the LigninDB engine is in `docs/lignindb/engine.md`,
//! "Checkpoints", and what a caller has to know of it is on the calls.

use std::path::Path;

use crate::{Engine, Error, GroveDb};

impl<E: Engine> GroveDb<E> {
    /// Creates a checkpoint: the state that is committed now, in a new
    /// directory at `path`, which opens as a database of its own. A
    /// `path` that is there already is refused and left as it is.
    ///
    /// The LigninDB engine builds the checkpoint in a directory beside
    /// `path`, under `path`'s name with `.creating` added, and gives it
    /// the name `path` when it is whole. A creation that failed or was
    /// cut can leave that directory. It is no checkpoint: it is not
    /// opened, and whoever lists a directory of checkpoints does not
    /// count it. It keeps disk space until it goes: a later creation at
    /// `path` takes it over, and [`Self::delete_checkpoint_with_engine`]
    /// removes it. A `path` whose own name ends in `.creating`, in any
    /// case of its ASCII letters, is refused.
    pub fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        self.db.create_checkpoint(path).map_err(|e| e.into())
    }

    /// Opens a checkpoint on engine `E`. [`GroveDb::open_checkpoint`] does
    /// this on the default engine. No database is made where there is
    /// none.
    ///
    /// The LigninDB engine opens no path whose last name ends in
    /// `.creating`, in any case of its ASCII letters, and no directory
    /// that holds an entry named `MANIFEST.deleting`: that is the name a
    /// deletion gives the manifest of what it removes.
    pub fn open_checkpoint_with_engine<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        Ok(Self::from_engine(E::open_checkpoint(path)?))
    }

    /// Deletes a checkpoint directory of engine `E`, which no handle has
    /// open. [`GroveDb::delete_checkpoint`] does this on the default
    /// engine.
    ///
    /// A path of fewer than two components is refused before the engine
    /// is asked. What is removed then is [`Engine::delete_checkpoint`]'s
    /// to say, and the engines differ:
    ///
    /// - **The RocksDB engine** opens the checkpoint first. Where that
    ///   fails, it removes nothing.
    /// - **The LigninDB engine** opens nothing, and removes more than a
    ///   checkpoint (`docs/lignindb/engine.md`, "Checkpoints", has the
    ///   whole rule):
    ///   - a directory, and not a link to one, that holds a manifest of
    ///     the engine and that no handle has open, with everything in
    ///     it: a checkpoint, and any other closed database of the engine;
    ///   - a directory that holds the manifest under the name
    ///     `MANIFEST.deleting`, which a deletion that was cut short left,
    ///     and a whole database beside a file of that name which
    ///     something else made;
    ///   - an empty directory, and one that holds an empty file named
    ///     `LOCK` and nothing else;
    ///   - **a directory whose name ends in `.creating`, in that
    ///     spelling, with whatever it holds**, unless its lock is held:
    ///     the engine takes every directory of such a name for what a
    ///     creation of a checkpoint left, and looks for no database in
    ///     it;
    ///   - the directory of `path`'s name with `.creating` added, in the
    ///     same way, after it removed `path` and where nothing has the
    ///     name `path`. An error of that removal is returned though
    ///     `path` is gone.
    ///
    ///   It refuses, with nothing removed, a file, a link, a database
    ///   that a handle has open, and a directory of any other name that
    ///   holds files and no manifest of the engine. It goes by the name
    ///   as the directory above lists it, byte for byte.
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
    /// On the RocksDB engine this function verifies that the path is a
    /// valid GroveDB checkpoint by attempting to open it first. If the
    /// checkpoint cannot be opened, deletion is refused to prevent
    /// accidental deletion of arbitrary directories.
    ///
    /// On the LigninDB engine the checkpoint is not opened, and more than
    /// a checkpoint is removed: [`Self::delete_checkpoint_with_engine`]
    /// lists what, an empty directory and any directory whose name ends
    /// in `.creating` among it.
    pub fn delete_checkpoint<P: AsRef<Path>>(path: P) -> Result<(), Error> {
        Self::delete_checkpoint_with_engine(path)
    }
}
