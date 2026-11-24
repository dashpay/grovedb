//! The lignin command: inspect, verify, compact and migrate LigninDB databases
//!
//! `lignin` is the operator's tool for a LigninDB database directory
//! ([the plan](https://github.com/dashpay/grovedb/issues/1052), task
//! OPS-01). Each subcommand is a function of this crate that writes its
//! report to a writer ([`run`]), and the binary is [`main`] over the
//! process's arguments.
//!
//! ```text
//! lignin manifest  DIR                  the manifest: format, page size, live files, table checkpoint
//! lignin segments  DIR [--blocks]       each segment the manifest names, block by block
//! lignin pages     DIR [PAGE...] [--cells]
//!                                       pages of the latest version, decoded
//! lignin directory DIR                  every entry of the subtree directory
//! lignin verify    DIR [--no-hashes] [--beside-background]
//!                                       the physical invariant checker
//! lignin stats     DIR                  every count of the engine after an open
//! lignin compact   DIR [--timeout SECS] reclaim and clean until a whole pass of the cleaner moves nothing
//! lignin dump      DIR (SUBTREE | --all) FILE
//!                                       a subtree, with every subtree below it, to a file
//! lignin load      DIR FILE             a dump, into a database that does not have its subtrees
//! lignin migrate   SOURCE TARGET [--batch MIB] [--progress SECS] [--unreachable refuse|leave|carry]
//!                                       a RocksDB GroveDB into a new LigninDB database
//! lignin migrate   SOURCE --survey [--unreachable refuse|leave|carry]
//!                                       what a RocksDB GroveDB holds, and whether it migrates
//! ```
//!
//! `migrate` is in a build with the Cargo feature `migrate`, which compiles
//! RocksDB: the module `migrate` has its documentation.
//!
//! Every subcommand that opens the database (all but `manifest` and
//! `segments`) also takes `--cache MIB`, the buffer pool of the open
//! (64 MiB unless given). A subtree is named by its
//! 32 bytes in hex, as `lignin directory` prints it.
//!
//! # What a subcommand does to the directory
//!
//! | Subcommand | Opens the database | Writes |
//! | --- | --- | --- |
//! | `manifest`, `segments` | no: reads `MANIFEST` and the files it names, each up to the length the manifest vouches for | nothing |
//! | `pages`, `directory`, `stats`, `verify`, `dump` | yes ([`lignindb::Db::open`]) | what any open writes: the recovery starts the session's log file and replaces the manifest, and the close writes back and takes a table checkpoint |
//! | `compact` | yes, with a cleaner that leaves no dead byte it can move | the cleaner's copies, then what a close writes |
//! | `load` | yes | the subtrees of the dump, in one commit |
//! | `migrate` | no database at its target: it makes one beside it | the source: nothing. The target: a new database in the directory `TARGET.migrating`, which is given the name `TARGET` at the end |
//!
//! **A LigninDB database that a process has open is not opened again**: the
//! open takes the directory's lock and fails with
//! [`OpenError::Locked`](lignindb::db::OpenError::Locked). The tool is run
//! on a database whose node is stopped, or on a checkpoint of a live one
//! (`lignindb::checkpoint`): a checkpoint is a database of its own,
//! opened by the normal open. `manifest` and `segments` read files only,
//! and read a checkpoint whose files the live database goes on appending
//! to up to the lengths the checkpoint's manifest gives them. Run them
//! before anything opens a checkpoint that is to be backed up
//! ([`lignindb::backup`]): an open is a session, which writes.
//!
//! No subcommand reads a part of a checkpoint (a path whose final name
//! ends in `.creating`) or a directory that holds `MANIFEST.deleting` (a
//! database being removed): each refuses it before it reads anything,
//! and says what the operator can do.
//!
//! No subcommand but `migrate` makes a database: an open of a directory
//! that holds none fails
//! ([`OpenError::NoDatabase`](lignindb::db::OpenError::NoDatabase)), `load`
//! included. `migrate` makes one in a directory of its own beside its
//! target, and refuses a target whose name ends in `.creating`. Its source
//! is a RocksDB database, which it reads through RocksDB's read-only open
//! behind a probe of that database's lock, and never writes: the module
//! `migrate` says how.
//!
//! # Exit status
//!
//! | Status | |
//! | --- | --- |
//! | 0 | done; for `verify`, the store is sound |
//! | 1 | the subcommand failed: the error is printed |
//! | 2 | the arguments are not a subcommand |
//! | 3 | `verify`: a rule of the store does not hold. `migrate`: the migrated store differs from its source, or a rule of it does not hold |
//! | 4 | `verify`: the database holds the mark of a state-sync restore that did not finish ([`verify`]). `migrate`: the device of its target is full, and the same command goes on once there is room |
//!
//! # Choices made here that the plan left open
//!
//! 1. **The arguments are parsed by hand** ([`args`]): the LigninDB crates
//!    take pure-Rust dependencies only (DEC-07), and a command of nine
//!    subcommands needs no parser crate.
//! 2. **`verify` recomputes every hash by Merk's rules**
//!    (`grovedb_merk::store::lignin::hash::MerkHashRules::for_check`, at the
//!    latest GroveVersion, naming the value hash that a delete on grove
//!    versions 1 to 3 left: [`verify`]). The engine knows no hash rule; the tool is for
//!    GroveDB's databases, and a check of every other rule is
//!    `--no-hashes`.
//! 3. **A dump is a file of this crate's own format** ([`dump`]), which
//!    `load` reads back through the engine's write transaction. It is no
//!    replacement for GroveDB's `raw_storage` hatches (GROVE-09).

#![forbid(unsafe_code)]

pub mod args;
pub mod compact;
pub mod directory;
pub mod dump;
mod error;
pub mod manifest;
#[cfg(feature = "migrate")]
pub mod migrate;
mod open;
pub mod pages;
pub mod segments;
pub mod stats;
pub mod verify;

use std::{ffi::OsString, io::Write};

pub use error::ToolError;

use self::args::Command;

/// What a subcommand found, for the exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It did what it was asked.
    Done,
    /// `verify` found a rule of the store that does not hold.
    NotSound,
    /// `verify` found the mark of a restore that did not finish.
    IncompleteRestore,
    /// `migrate` met a full device, in a state that the same command goes
    /// on from once there is room.
    IncompleteMigration,
}

impl Outcome {
    /// The exit status of the process.
    pub const fn status(self) -> i32 {
        match self {
            Self::Done => 0,
            Self::NotSound => 3,
            Self::IncompleteRestore | Self::IncompleteMigration => 4,
        }
    }
}

/// Runs the subcommand `args` names (without the program's name), and
/// writes its report to `out`.
pub fn run(args: &[OsString], out: &mut dyn Write) -> Result<Outcome, ToolError> {
    match args::parse(args)? {
        Command::Help => {
            out.write_all(args::USAGE.as_bytes())?;
            Ok(Outcome::Done)
        }
        Command::Manifest { dir } => manifest::run(&dir, out),
        Command::Segments { dir, blocks } => segments::run(&dir, blocks, out),
        Command::Pages {
            common,
            pages,
            cells,
        } => pages::run(&common, &pages, cells, out),
        Command::Directory { common } => directory::run(&common, out),
        Command::Verify {
            common,
            hashes,
            beside_background,
        } => verify::run(&common, hashes, beside_background, out),
        Command::Stats { common } => stats::run(&common, out),
        Command::Compact { common, timeout } => compact::run(&common, timeout, out),
        Command::Dump { common, what, file } => dump::dump(&common, &what, &file, out),
        Command::Load { common, file } => dump::load(&common, &file, out),
        #[cfg(feature = "migrate")]
        Command::Migrate {
            source,
            target,
            cache_size,
            batch_bytes,
            progress,
            unreachable,
        } => {
            let migration = migrate::Migration {
                source,
                target,
                options: lignindb::Options {
                    cache_size,
                    ..lignindb::Options::default()
                },
                batch_bytes,
                progress,
                unreachable,
            };
            migrate::migrate(&migration, out)
        }
        #[cfg(not(feature = "migrate"))]
        Command::Migrate { .. } => Err(ToolError::NotBuilt {
            subcommand: "migrate",
            feature: "migrate",
        }),
    }
}

/// The command line: runs [`run`] on `args` (with the program's name
/// first), the report to `out` and an error to `err`, and returns the exit
/// status.
pub fn main(
    args: impl IntoIterator<Item = OsString>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let args: Vec<OsString> = args.into_iter().skip(1).collect();
    match run(&args, out) {
        Ok(outcome) => outcome.status(),
        Err(error) => {
            let _ = writeln!(err, "lignin: {error}");
            if matches!(error, ToolError::Usage(_)) {
                let _ = err.write_all(args::USAGE.as_bytes());
            }
            error.status()
        }
    }
}

/// `bytes` in lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests;
