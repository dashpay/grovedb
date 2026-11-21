//! The open of a database for a subcommand, and the directories no
//! subcommand reads as one.

use std::path::Path;

use lignin_store::cleaner::CleanerConfig;
use lignindb::{
    checkpoint::{CREATING_SUFFIX, DELETING_FILE_NAME},
    db::OpenError,
    Db, Options,
};

use crate::{args::Common, ToolError};

/// What a path whose final name ends in `.creating` is, for the operator.
const CREATING: &str = "its final name ends in .creating: it is a checkpoint that is being \
     made, or the part of one that a creation cut short left, and it is never opened. Run \
     lignin on the checkpoint's target once its creation has returned. The next creation of \
     a checkpoint at that target takes over what a cut creation left, and \
     lignindb::checkpoint::delete removes it";

/// What a directory that holds `MANIFEST.deleting` is, for the operator.
const DELETING: &str = "it holds MANIFEST.deleting: a removal of it is under way, or one was \
     cut short and some of its files may be gone. lignindb::checkpoint::delete finishes the \
     removal. Where no removal made that entry and the database is to be kept, remove the \
     entry and nothing else";

fn refused(dir: &Path, what: &'static str) -> ToolError {
    ToolError::NotADatabase {
        dir: dir.to_path_buf(),
        what,
    }
}

/// Refuses, for the subcommands that read files without an open
/// (`manifest`, `segments`), a directory that the open refuses before it
/// reads anything ([`OpenError::Creating`], [`OpenError::Deleting`]), in
/// the same words as [`open`].
pub(crate) fn refuse_parts(dir: &Path) -> Result<(), ToolError> {
    // The bytes of the name, as the open looks at them: a name that is no
    // UTF-8 before its suffix ends in it all the same.
    let creating = dir.file_name().is_some_and(|name| {
        let name = name.as_encoded_bytes();
        let at = name.len().checked_sub(CREATING_SUFFIX.len());
        at.is_some_and(|at| name[at..].eq_ignore_ascii_case(CREATING_SUFFIX.as_bytes()))
    });
    if creating {
        return Err(refused(dir, CREATING));
    }
    if dir.join(DELETING_FILE_NAME).symlink_metadata().is_ok() {
        return Err(refused(dir, DELETING));
    }
    Ok(())
}

/// Opens the database in `common.dir`, which has to hold one: nothing is
/// made. `cleaner` is the cleaner's policy ([`Options::cleaner`]).
///
/// The open's refusal of a part of a checkpoint
/// ([`OpenError::Creating`]) and of a directory being removed
/// ([`OpenError::Deleting`]) is told in words that say what to do.
///
/// The close of the handle runs no physical check in a debug build
/// ([`Db::unchecked_at_close`]): a tool reports what it finds, and a store
/// that is not sound is one it is run on. `verify` checks on its own
/// terms.
pub(crate) fn open_with(common: &Common, cleaner: Option<CleanerConfig>) -> Result<Db, ToolError> {
    let options = Options {
        create_if_missing: false,
        cache_size: common.cache_size,
        cleaner,
        ..Options::default()
    };
    let db = Db::open(&common.dir, &options).map_err(|error| match error {
        OpenError::Creating { dir } => refused(&dir, CREATING),
        OpenError::Deleting { dir } => refused(&dir, DELETING),
        other => ToolError::Open(other),
    })?;
    db.unchecked_at_close();
    Ok(db)
}

/// [`open_with`] the engine's cleaner.
pub(crate) fn open(common: &Common) -> Result<Db, ToolError> {
    open_with(common, None)
}
