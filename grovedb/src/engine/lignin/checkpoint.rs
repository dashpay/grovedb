//! Checkpoints on the LigninDB engine (GROVE-08): the three calls of the
//! seam, and the errors of `lignindb` as the seam reports them.
//!
//! `docs/lignindb/engine.md`, "Checkpoints", says what each call does. A
//! checkpoint is made by `lignindb` ([`Db::checkpoint`]), opened by the
//! open of any database, which is told not to make one, and removed by
//! [`lignindb::checkpoint::delete`].

use std::path::Path;

use grovedb_storage::Error;
use lignindb::{
    checkpoint::{self, CheckpointError, CheckpointOptions},
    Db, Options,
};

use super::{
    error::{failed, not_supported},
    LigninEngine,
};

impl LigninEngine {
    /// [`Engine::create_checkpoint`](crate::Engine::create_checkpoint):
    /// a checkpoint of the database at `target`, with the default
    /// [`CheckpointOptions`].
    pub(super) fn checkpoint_at(&self, target: &Path) -> Result<(), Error> {
        (self.db.checkpoint(target, CheckpointOptions::default()))
            .map(|_| ())
            .map_err(creation_error)
    }

    /// [`Engine::open_checkpoint`](crate::Engine::open_checkpoint): the
    /// open of a database that makes none where it finds none.
    pub(super) fn open_checkpoint_at(path: &Path) -> Result<Self, Error> {
        let options = Options {
            create_if_missing: false,
            ..Options::default()
        };
        Self::new(Db::open(path, &options).map_err(failed)?)
    }

    /// [`Engine::delete_checkpoint`](crate::Engine::delete_checkpoint):
    /// [`lignindb::checkpoint::delete`] on the file system that
    /// [`Engine::open`](crate::Engine::open) opens a database on.
    pub(super) fn delete_checkpoint_at(path: &Path) -> Result<(), Error> {
        let vfs = Options::default().vfs;
        checkpoint::delete(&*vfs, path).map_err(failed)
    }
}

/// Why a checkpoint was not made, as the seam reports it.
///
/// A target whose name the engine keeps for the directories it builds
/// checkpoints in is a limit of the engine. The words of a creation that
/// was refused because the commit path of the session has stopped, and
/// of one whose own sync of the log failed, say that the database has to
/// be opened again, as those of a commit do. Every other error is a
/// storage error with the engine's words.
pub(super) fn creation_error(error: CheckpointError) -> Error {
    match error {
        CheckpointError::ReservedName { .. } => not_supported(&error.to_string()),
        CheckpointError::Stopped(_) | CheckpointError::Sync(_) => failed(format!(
            "{error}: no checkpoint is made before the database is opened again"
        )),
        error => failed(error),
    }
}
