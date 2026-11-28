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
    /// The LigninDB engine builds the checkpoint beside `path`, in a
    /// staging directory under `path`'s name with `.creating` added: in
    /// it, a directory `checkpoint.creating`, which gets the name `path`
    /// when the checkpoint is whole. A creation that failed or was cut
    /// can leave the staging directory, with what was built in it. It is
    /// no checkpoint: it is not opened, and whoever lists a directory of
    /// checkpoints does not count it. What was built keeps disk space
    /// until it goes: a later creation at `path` that gets as far as
    /// building removes it and builds there, and
    /// [`Self::delete_checkpoint_with_engine`] says when a deletion
    /// removes it. The engine takes a directory under that name for what
    /// a creation left only where it holds nothing but an empty file
    /// `LOCK` and a directory `checkpoint.creating`: a database, or
    /// anything else that lies there, refuses the creation and is as it
    /// was. A `path` whose own name ends in `.creating`, in any case of
    /// its ASCII letters, is refused.
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
    /// - **The RocksDB engine** opens `path` as a checkpoint first. Where
    ///   that fails, it removes nothing. Where it succeeds, it closes the
    ///   database and calls `std::fs::remove_dir_all(path)`, and returns
    ///   an error if that removal fails. The open does not tell a
    ///   checkpoint from another database of the engine.
    /// - **The LigninDB engine** opens nothing. Its removal is
    ///   `lignindb::checkpoint::delete`. **It can remove more than a
    ///   checkpoint.** What follows is a warning and not the rule: the
    ///   rule, with what the call looks at before it removes anything
    ///   and what it leaves, is the documentation of that function.
    ///   Among what the call can remove:
    ///   - a closed database of the engine that is no checkpoint, which
    ///     it does not tell from one, with everything in its directory:
    ///     a checkpoint that was made inside that directory goes with
    ///     it, whoever has it open;
    ///   - what a deletion left that was cut short, and a whole database
    ///     beside a file named `MANIFEST.deleting` which something else
    ///     made: that is the name a deletion gives the manifest;
    ///   - an empty directory, and a directory that holds an empty file
    ///     named `LOCK` and nothing else;
    ///   - **a directory whose name ends in `.creating`, in that
    ///     spelling, where it is what a creation of a checkpoint left**:
    ///     one that holds nothing but an empty file `LOCK` and a
    ///     directory `checkpoint.creating`, with whatever that directory
    ///     holds. The engine looks for no database in it. One that holds
    ///     anything else is refused, with nothing removed, and so is one
    ///     that another look of `lignindb::checkpoint` refuses: among
    ///     them, one whose lock is held;
    ///   - for a `path` whose own name does not end so, the directory of
    ///     `path`'s name with `.creating` added, in the same way: once
    ///     it has removed `path`, and where nothing has the name `path`.
    ///     Where `path` is refused, that directory is not looked for.
    ///     **Such a directory that no creation left is passed over**,
    ///     with no error and with nothing in it touched, whether it lay
    ///     there before the call or came while `path` was removed. Where
    ///     the call removed `path`, a directory whose lock is held stays
    ///     too, with no error, and any other error of that second removal
    ///     is returned though `path` is gone: one of the file system, and
    ///     that of a look under the lock which did not find the directory
    ///     that was locked. Where nothing has the name `path`, a
    ///     directory whose lock is held is the error of the call, and
    ///     with one that no creation left the call answers as where
    ///     nothing is there.
    ///
    ///   So a caller gives this call no path that it does not want gone.
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
    /// On the RocksDB engine this function attempts to open the path as
    /// a checkpoint first. If that fails, deletion is refused, to prevent
    /// accidental deletion of arbitrary directories. The open does not
    /// tell a checkpoint from another database of the engine, so the
    /// path of such a database can be removed too.
    ///
    /// On the LigninDB engine the checkpoint is not opened, and the call
    /// can remove more than a checkpoint:
    /// [`Self::delete_checkpoint_with_engine`] warns of what, an empty
    /// directory and what a creation left under a name that ends in
    /// `.creating` among it.
    pub fn delete_checkpoint<P: AsRef<Path>>(path: P) -> Result<(), Error> {
        Self::delete_checkpoint_with_engine(path)
    }
}
