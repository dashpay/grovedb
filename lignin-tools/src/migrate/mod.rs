//! `lignin migrate SOURCE TARGET`: a RocksDB GroveDB into a new LigninDB
//! database, offline ([the plan](https://github.com/dashpay/grovedb/issues/1052),
//! task OPS-02).
//!
//! A node that is migrated goes on from where its RocksDB database stood.
//! So the target has to answer every later operation as the source would
//! have: the same root hash, and the same answer, proof and cost for each
//! read and write. The migrator therefore carries every record that a
//! read of GroveDB can come to, as it is, and says of each record of the
//! source what became of it (`survey`).
//!
//! # What it does, in order
//!
//! 1. **It opens the source for reading** (`source`): RocksDB's
//!    read-only open, behind a lock that a running node holds. The source
//!    is the directory of a stopped node, or a checkpoint. Nothing is
//!    written there, and a write-ahead log that the node left is replayed
//!    in memory: the migration is of the state the node would recover to.
//!    **The node is stopped, and nothing else touches the source during
//!    the run**: a `LOCK` file that is replaced while a writer runs says
//!    nothing of that writer, and RocksDB itself would start a second
//!    writer on such a directory.
//!    A target that lies inside the source, or around it, is refused
//!    before anything is opened. **The tool compares names with their
//!    links followed, and the identity of each directory on both paths
//!    and inside the source; what a file system shows as two directories
//!    otherwise is not seen.**
//! 2. **It surveys the source** (`survey`): every record of every
//!    keyspace is counted as what it is, before the run writes anything.
//!    The survey says which records are carried and which are left behind
//!    by a rule: what a pending drop was still to remove, and what
//!    `--unreachable leave` leaves. A source that holds a record which is
//!    neither is refused here, with what was found. `--survey` stops
//!    after this step. **Every run surveys**, also one that goes on with a
//!    load or finds one complete: what refuses a source is looked for in
//!    the source as it is when the run reads it.
//! 3. **It loads the target** (`load`) in `TARGET.migrating/database`,
//!    a commit at a time, each Merk handed to the engine whole through its
//!    bulk loader. The first write of a new database enters the two
//!    subtrees that every grove has. Every commit of the load after it but
//!    the last holds the journal of where the load stands (`journal`),
//!    and the last removes it.
//! 4. **It compares** (`compare`): the source is read once more, and
//!    every record of it that is carried is held to what the target
//!    answers through the engine of GroveDB, with the root hash.
//! 5. **It verifies** the target with the physical invariant checker,
//!    every stored hash made again by Merk's rules, as `lignin verify`
//!    does.
//! 6. **It gives the database the name `TARGET`**, makes that name
//!    durable, and removes the directory it worked in.
//!
//! # What is at `TARGET`, and what a stop leaves
//!
//! Nothing is at `TARGET` until step 6: a path that is there and is not an
//! empty directory is refused. So a node cannot be started on a store that
//! is not whole, compared and verified. The directory that `TARGET` is to
//! be in has to be there: the command makes none above its own.
//!
//! `TARGET.migrating` holds the work: a file `MIGRATION`, which the
//! command makes there before anything else and which says by its bytes
//! that the directory is its own, and the database in the directory
//! `database`. The file is written and made durable under the name
//! `MIGRATION.new` and then given its own, so a file of the name
//! `MIGRATION` holds all of its bytes whatever stopped the run. **A run
//! that is stopped at any point before step 6, by a kill, a power cut or a
//! full device, is gone on with by the same command**: what the directory
//! holds says where it stands.
//!
//! | `TARGET.migrating` | What the next run does, after its survey |
//! | --- | --- |
//! | is not there, or is a directory with nothing in it | makes it its own, and begins the load |
//! | is a directory that holds nothing but a file `MIGRATION.new` which a stop of the command can have left: no longer than the command's file, and each byte of it as the command writes it or zero, as a byte reads that was never written | takes it for its own file, which a stop left before it had its name: removes it, makes the file again, and begins the load |
//! | is anything else that is not the command's: no directory, a directory with an entry that is neither the file `MIGRATION` nor the directory `database`, one whose `MIGRATION` holds other bytes than the command writes, or one whose lone `MIGRATION.new` no stop of the command leaves | refuses, and changes nothing there |
//! | is the command's, with no database or one that holds no record | begins the load. A complete load that carried no record leaves such a database too (a source that holds none, or one whose records are all left behind): the load is then made again, and the database holds no record again once it is complete |
//! | is the command's, with a database that has a journal of this source | goes on with the load from the last commit: a Merk that was handed over is not handed over again |
//! | is the command's, with a database that has a journal of another source, of another state of this one, or of a load that was told otherwise what becomes of the records nothing leads to | refuses: the directory is to be removed, or the run is told what the first was |
//! | is the command's, with a database that holds a record and no journal | the load is complete: compares, verifies and renames. A run that was stopped between the last commit of the load and the rename leaves this, and so does one whose comparison or verification failed |
//!
//! **Step 6** is the rename of `TARGET.migrating/database` to `TARGET`,
//! the syncs of the two directories the name moved between, and the
//! removal of the work directory.
//!
//! | A stop in step 6 | leaves | and the next run |
//! | --- | --- | --- |
//! | before the rename | the complete load in `TARGET.migrating`: the last row above, or the row of a database that holds no record where the migration carried none | does as that row says |
//! | after the rename and before both syncs: a kill, or a sync that failed, which the run says and ends on with status 1 | the database at `TARGET` under a name that a power cut can still take back, and the work directory with its file | makes the syncs, removes the work directory where it can, and refuses the target as one that is there: a database that a migration left there is whole |
//! | the same, and a power cut after it | the database at `TARGET`, or in `TARGET.migrating` again, as the device kept the name | does as the row above, or as the row before it |
//! | after the syncs | the database at `TARGET`, and of the work directory its file, or nothing, or no directory | removes what is left of the work directory where it can, and refuses the target as one that is there |
//!
//! A run that finds a directory at the target that is not empty, and
//! beside it a work directory of its own with no database, cannot tell
//! whether a run gave the target its name or something else was put
//! there after a run had made its work directory: it makes the two syncs
//! in either case, which change nothing but what a power cut can take
//! back, and says of the target only that it is there. A target that is
//! a file or a link is refused with nothing synced and nothing removed.
//! A directory at the target with nothing in it is no target that is
//! there: the run does what the first table says for its work directory.
//!
//! Once the syncs are made the run ends with status 0, whatever becomes
//! of the removal of the work directory and of the last lines of its
//! report: a work directory that is left holds nothing of the target, and
//! a report that could not be written changes nothing of it.
//!
//! A device that is full ends a run with status 4, at whichever step from
//! the making of the work directory to the rename it was met, and leaves
//! one of the rows of the first table: the run after room was made goes
//! on.
//!
//! **The work directory is the command's alone while it runs.** The lock
//! of the database keeps every other process out during the load and the
//! comparison, and again during the check. Between the two the lock is
//! given up and taken again, and a store that was written in between is
//! told by its commit version and refused. After the check the database
//! is renamed with no lock held, since a close of the database gives its
//! lock up: a process that opens `TARGET.migrating/database` in that
//! moment is not kept out. So nothing else is to open `TARGET.migrating`
//! while the command runs.
//!
//! # Records that nothing leads to
//!
//! RocksDB can hold records under a prefix that no element leads to and no
//! pending-drop record names (`beside`): a tree that a batch's plain
//! delete removed while it held an element leaves its records. The survey
//! says of each such prefix what it holds, and a run says how many
//! records under how many prefixes it found. `--unreachable` says what
//! becomes of them.
//!
//! **Unless the option is given they are left behind (`leave`)**, and the
//! target is what a database written on LigninDB holds. No element leads
//! to such a record, so the root hash is the same without it. What can
//! answer otherwise than on RocksDB is a read by the path of such a tree
//! that asks for no element on the way, a tree that is made again at such
//! a path, and a cursor that steps from no key (`beside` says each). It
//! is what a run does unless told because a database that a caller of
//! GroveDB wrote can hold such records, wherever a batch's plain delete
//! removed a tree that held an element: a run that refused them unless
//! it is told would refuse such a database, so they are left unless the
//! run is told otherwise.
//!
//! With `refuse` a source that holds any is refused before anything is
//! written. With `carry` they are carried as records that no tree links,
//! and a write below a tree that is made again at such a path is refused
//! by the engine as it stands (`beside` says what each choice gives a
//! later operation). A run that goes on with a work directory is given
//! the choice that the run which began it had.
//!
//! # Memory
//!
//! A Merk is handed over whole, so every node of the largest Merk of the
//! source is in memory at once, with what the engine holds of it until
//! its commit is made (`transactions.md` LD7). The records under a prefix
//! that nothing leads to are read whole too by a load that carries them,
//! which puts each such prefix into one commit, its nodes with its aux
//! and meta records. Beside it the migrator holds what one commit takes
//! (`--batch`: for each node about 1.5 KB with its key and value, for
//! each record that is no node the same with three times its key and
//! value, and a page of the store for each Merk it hands over; a commit
//! ends after the Merk that fills it), and the buffer pool of the target
//! (`--cache`).
//!
//! **The run says what peak it projects, before it writes anything**: a
//! floor (256 MiB, `--batch` and `--cache`), and for each node of the
//! heaviest Merk it hands over about 1.5 KB with the node's key and
//! value. An aux or a meta record that is read whole with a carried
//! prefix is counted as a node, with three times its bytes. The figure a
//! node is a measurement of this command as it is at this commit; the
//! projection is an estimate from it, and no cap that the process is
//! held to. **A source whose projection is above `--memory-limit` is
//! refused**, and nothing is written. Unless the option is given, the
//! limit is half of the machine's memory as `/proc/meminfo` states it,
//! which the run says. Where that file does not state it (it is not
//! there, as on a system that is not Linux, or it cannot be read) no
//! limit is held, which the run says too; and in a container the file
//! can state the memory of the host. A refusal says which part of the
//! projection is above the limit: the 256 MiB of any run; those with the
//! largest of what the load holds whole, which no setting makes less,
//! since loading it in parts is not built; or only the sum with
//! `--batch` and `--cache`, which smaller ones answer where the least
//! the command takes for them (1 MiB each) is not above the limit too.
//!
//! # Exit status
//!
//! | Status | |
//! | --- | --- |
//! | 0 | the target is at `TARGET` under a name that is durable: loaded, compared and verified. With `--survey`: the source can be migrated |
//! | 1 | the migration failed, or the source or the target was refused. The run put nothing at `TARGET`, but for one failure that its error states: the database got that name, and the name could not be made durable |
//! | 3 | the migrated store differs from its source, or a rule of it does not hold; it is left in `TARGET.migrating/database` |
//! | 4 | the device of the target is full, at a step from the making of the work directory to the rename; the same command goes on once there is room |

mod beside;
mod compare;
mod grove;
mod journal;
mod load;
mod source;
mod survey;
mod walk;

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use grovedb::{GroveDb, LigninEngine};
use grovedb_merk::store::lignin::{hash::MerkHashRules, keeps_flat_records};
use grovedb_storage::flat::pending_prefix_drops_namespace;
use grovedb_version::version::GroveVersion;
use lignin_io::{Create, EntryKind, OpenMode, Vfs};
use lignin_tree::hash::HashRules;
use lignindb::{
    txn::{Column, Subtree},
    Db, Options,
};

use self::{
    journal::{Carried, Journal, Phase, JOURNAL_KEY},
    load::Load,
    source::{file_identity, Source},
    survey::{Survey, PEAK_A_NODE},
    walk::{Side, Walk, ROOT},
};
pub use crate::args::Unreachable;
use crate::{hex, verify::left_hashes, Outcome, ToolError};

/// What the name of the directory a migration works in ends with.
pub const WORK_SUFFIX: &str = ".migrating";

/// The name of the database in the directory a migration works in.
pub const WORK_DATABASE: &str = "database";

/// The name of the file that says a directory is the work of a migration.
/// The command makes it before anything else there, with the bytes
/// [`MARK_TEXT`]. It takes no directory that holds something and neither
/// this file nor, alone, what a stop left of its making
/// ([`WORK_MARK_NEW`]).
pub const WORK_MARK: &str = "MIGRATION";

/// The name under which the file [`WORK_MARK`] is written and made
/// durable before it is given its own, so that a file named
/// [`WORK_MARK`] always holds all of its bytes. A directory that holds
/// nothing but a file of this name which a stop of the one write can
/// have left is taken, and the file is made again.
pub const WORK_MARK_NEW: &str = "MIGRATION.new";

/// What the file [`WORK_MARK`] holds, to the byte.
pub const MARK_TEXT: &[u8] = b"The work directory of `lignin migrate`. The database it makes is \
    in `database`, and gets the name of the target once it is compared and verified.\n";

/// A migration to make.
#[derive(Debug, Clone)]
pub struct Migration {
    /// The directory of the RocksDB GroveDB: of a stopped node, or a
    /// checkpoint.
    pub source: PathBuf,
    /// The directory the LigninDB database is to be at, or `None` to
    /// survey the source and write nothing.
    pub target: Option<PathBuf>,
    /// How the database of the target is opened. A database is made where
    /// there is none, whatever `create_if_missing` says.
    pub options: Options,
    /// About how many bytes one commit takes: for each node what a node
    /// is measured to take with its key and value, for each record that
    /// is no node the same with three times its key and value, and a page
    /// of the store for each Merk it hands over.
    pub batch_bytes: usize,
    /// How often the load says where it stands, at the commit it has come
    /// to: with no time, after every commit.
    pub progress: Duration,
    /// What becomes of the records under a prefix that no element leads
    /// to and no pending-drop record names (`beside`).
    pub unreachable: Unreachable,
    /// The most memory, in bytes, that the migration may project to hold
    /// at its peak: a source that projects more is refused before
    /// anything is written. `None` is half of the machine's memory as
    /// `/proc/meminfo` states it, and no limit where that file does not
    /// state it.
    pub memory_limit: Option<u64>,
}

/// What a migration holds beside a commit, the buffer pool and the
/// heaviest Merk it hands over: the floor of what it projects.
const PEAK_FLOOR: u64 = 256 << 20;

/// The least that the command takes for `--batch` and for `--cache`: each
/// is given in MiB, and is 1 or more.
const LEAST_OF_A_SETTING: u128 = 1 << 20;

/// The memory of the machine, in bytes, as `/proc/meminfo` states it:
/// `None` where the file is not there, cannot be read or states no
/// `MemTotal`. In a container the file can state the memory of the host.
fn machine_memory() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib.saturating_mul(1024))
}

fn refused(what: impl Into<String>) -> ToolError {
    ToolError::Migrate(what.into())
}

/// The directory a migration to `target` works in: `target` with
/// [`WORK_SUFFIX`] after its last name.
pub fn work_dir(target: &Path) -> Result<PathBuf, ToolError> {
    let name = target.file_name().ok_or_else(|| {
        refused(format!(
            "the target {} has no name of its own",
            target.display()
        ))
    })?;
    let mut name = name.to_os_string();
    name.push(WORK_SUFFIX);
    Ok(target.with_file_name(name))
}

/// The most memory the process held, in bytes, as the system counts it:
/// `None` where the system does not say.
fn peak_memory() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// The bytes that are free on the device of `dir`, where the system says.
fn free_bytes(dir: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(dir).ok()?;
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// What the target's database holds beside what an engine makes when it
/// opens a new one: the journal of a load, or records of a grove.
enum Stands {
    /// Nothing: no load was begun, or a load of a source with no record
    /// was made, which a second load makes again as it is.
    Fresh,
    /// A load that is not complete.
    Loading(Box<Journal>),
    /// No journal, and a record: the load is complete.
    Loaded,
}

fn stands(db: &Db) -> Result<Stands, ToolError> {
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let root = operation.open(Subtree::new(ROOT))?;
    let root = root.ok_or_else(|| refused("the target has no root Merk: it is no grove"))?;
    let mut journal = Vec::new();
    if root.get(Column::Aux, JOURNAL_KEY, &mut journal)? {
        return Ok(Stands::Loading(Box::new(Journal::decode(&journal)?)));
    }
    let mut subtrees = 0;
    let mut walk = operation.entries()?;
    while let Some(listed) = walk.next()? {
        subtrees += u64::from(listed.live);
    }
    let mut holds = root.entry().root().is_some() || subtrees > 2;
    // The two subtrees that every grove has: a load can end with records
    // in the namespace of the pending drops alone, where the records of
    // root keys are kept too.
    let namespace = operation.open(Subtree::new(*pending_prefix_drops_namespace()))?;
    for opened in [Some(&root), namespace.as_ref()].into_iter().flatten() {
        for column in [Column::Data, Column::Aux, Column::Roots, Column::Meta] {
            let mut cursor = opened.cursor(column);
            cursor.seek_to_first()?;
            holds = holds || cursor.key().is_some();
        }
    }
    Ok(match holds {
        true => Stands::Loaded,
        false => Stands::Fresh,
    })
}

fn write_survey(survey: &Survey, told: Unreachable, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "the source holds:")?;
    for line in survey.report(told) {
        writeln!(out, "  {line}")?;
    }
    writeln!(out, "migrate: survey {}", survey.line())
}

/// Checks the store in `dir` with the physical invariant checker, every
/// stored hash made again by Merk's rules, and writes the report. The
/// store is the one that was compared at the commit version `compared`: a
/// store that was written since is refused.
fn verify(
    dir: &Path,
    options: &Options,
    compared: u64,
    out: &mut dyn Write,
) -> Result<bool, ToolError> {
    let options = Options {
        create_if_missing: false,
        ..options.clone()
    };
    let db = Db::open(dir, &options)?;
    db.unchecked_at_close();
    db.check_kinds_by(keeps_flat_records);
    if db.version().get() != compared {
        return Err(refused(format!(
            "the database in {} was written after it was compared: something else opened it. \
             Run the command again: it compares anew",
            dir.display()
        )));
    }
    // What this open wrote is durable before the directory gets its name.
    db.flush()?;
    let mut rules = MerkHashRules::for_check(GroveVersion::latest()).naming_left_hashes();
    let report = db.close_and_verify(Some(&mut rules as &mut dyn HashRules))?;
    writeln!(
        out,
        "the target, checked after its close with every hash made again by Merk's rules:"
    )?;
    writeln!(out, "{report}")?;
    if let Some(left) = left_hashes(&report) {
        writeln!(out, "{left}")?;
    }
    writeln!(
        out,
        "migrate: verify sound={} left_hashes={}",
        report.is_sound(),
        report.other_hashes.count
    )?;
    Ok(report.is_sound())
}

/// Makes the file that says `work` is the work directory of a migration:
/// written and made durable under another name ([`WORK_MARK_NEW`]), and
/// then given its own, so that a file of its name holds all of its bytes
/// whatever stops the run.
fn mark(vfs: &dyn Vfs, work: &Path) -> io::Result<()> {
    let new = work.join(WORK_MARK_NEW);
    {
        let file = vfs.open(&new, OpenMode::Append(Create::New))?;
        file.append(MARK_TEXT)?;
        file.sync_all()?;
    }
    vfs.rename(&new, &work.join(WORK_MARK))?;
    vfs.sync_dir(work)
}

/// The directory that `path` is in: the one the process is in for a path
/// of one name.
fn above(path: &Path) -> &Path {
    match path.parent() {
        Some(above) if !above.as_os_str().is_empty() => above,
        _ => Path::new("."),
    }
}

/// What is at the path of a work directory.
enum Work {
    /// Nothing.
    Absent,
    /// A directory with nothing in it.
    Empty,
    /// A directory that holds nothing but a file [`WORK_MARK_NEW`] that a
    /// stop of the command can have left before anything else was made:
    /// no longer than [`MARK_TEXT`], each byte as it is there or zero.
    CutShort,
    /// The command's: its file is there, and its database or not yet.
    Ours {
        /// Whether the directory [`WORK_DATABASE`] is there.
        database: bool,
    },
    /// Something that is not the command's, in words.
    Foreign(String),
}

impl Work {
    /// Looks at `work`, which the directory above it lists as `kind`, and
    /// changes nothing.
    fn looked(vfs: &dyn Vfs, work: &Path, kind: Option<EntryKind>) -> Result<Self, ToolError> {
        match kind {
            None => return Ok(Self::Absent),
            Some(EntryKind::Dir) => {}
            Some(_) => return Ok(Self::Foreign("it is no directory".to_owned())),
        }
        let entries = vfs.read_dir(work)?;
        let alone = matches!(
            entries.as_slice(),
            [entry] if entry.name == WORK_MARK_NEW && entry.kind == EntryKind::File
        );
        if alone {
            // What a stop leaves of the one append that writes the file: as
            // many bytes as the file was given or fewer, each as it was
            // written or, where it never was, zero.
            let file = vfs.open(&work.join(WORK_MARK_NEW), OpenMode::Read)?;
            let len = file.len()?;
            let mut left = vec![0; len.min(MARK_TEXT.len() as u64) as usize];
            file.read_exact_at(&mut left, 0)?;
            let of_a_stop = len <= MARK_TEXT.len() as u64
                && (left.iter().zip(MARK_TEXT))
                    .all(|(byte, written)| byte == written || *byte == 0);
            return Ok(match of_a_stop {
                true => Self::CutShort,
                false => Self::Foreign(format!(
                    "its file {WORK_MARK_NEW} is not what a stop of a migration leaves"
                )),
            });
        }
        let (mut marked, mut database) = (false, false);
        for entry in &entries {
            match (entry.name.to_str(), entry.kind) {
                (Some(WORK_MARK), EntryKind::File) => marked = true,
                (Some(WORK_DATABASE), EntryKind::Dir) => database = true,
                _ => {
                    return Ok(Self::Foreign(format!(
                        "it holds {}, which is not what a migration makes there",
                        entry.name.to_string_lossy()
                    )));
                }
            }
        }
        if !marked {
            return Ok(match database {
                false => Self::Empty,
                true => Self::Foreign(format!(
                    "it holds a directory {WORK_DATABASE} and no file {WORK_MARK}"
                )),
            });
        }
        let file = vfs.open(&work.join(WORK_MARK), OpenMode::Read)?;
        // A byte more than the command writes says that the file is
        // another one.
        let len = file.len()?.min(MARK_TEXT.len() as u64 + 1);
        let mut written = vec![0; len as usize];
        file.read_exact_at(&mut written, 0)?;
        Ok(match written == MARK_TEXT {
            true => Self::Ours { database },
            false => Self::Foreign(format!(
                "its file {WORK_MARK} is not the one a migration writes"
            )),
        })
    }
}

/// The two syncs that make the name durable which a rename out of `work`
/// gave, into the directory `above`.
fn named_durably(vfs: &dyn Vfs, work: &Path, above: &Path) -> io::Result<()> {
    vfs.sync_dir(work)?;
    vfs.sync_dir(above)
}

/// Removes `work`, which has done its work and holds its file alone.
fn cleared(vfs: &dyn Vfs, work: &Path) -> io::Result<()> {
    vfs.remove_file(&work.join(WORK_MARK))?;
    vfs.remove_dir(work)
}

/// What the directories are that `path` is in, itself among them: those
/// of them that are there, by what [`file_identity`] says, along the name
/// that `path` has once its links are followed ([`resolved`]). A
/// directory can have a second name that no link gives it, as a mount
/// does, and then no name tells that two paths are one inside the other.
fn identities(path: &Path) -> Vec<(u64, u64)> {
    let named = resolved(path);
    let there = named.ancestors().map(std::fs::metadata);
    there
        .flatten()
        .map(|metadata| file_identity(&metadata))
        .collect()
}

/// What the directory `dir` and every directory in it are, at any depth,
/// with no link followed. A directory that cannot be listed counts as
/// itself.
fn directories_in(dir: &Path) -> Vec<(u64, u64)> {
    let mut found = Vec::new();
    let mut to_list = vec![dir.to_path_buf()];
    while let Some(dir) = to_list.pop() {
        let Ok(metadata) = std::fs::metadata(&dir) else {
            continue;
        };
        found.push(file_identity(&metadata));
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                to_list.push(entry.path());
            }
        }
    }
    found
}

/// `path` with the links of the part of it that is there followed, and
/// made absolute.
fn resolved(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut rest = Vec::new();
    let mut there = absolute.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(there) {
            return rest.iter().rev().fold(real, |path, name| path.join(name));
        }
        match (there.parent(), there.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                there = parent;
            }
            _ => return absolute,
        }
    }
}

/// The directory a migration of `source` to `target` works in, where the
/// two are apart and the target has a name a database can have: it is
/// found out before anything is opened or made.
fn placed(source: &Path, target: &Path) -> Result<PathBuf, ToolError> {
    let work = work_dir(target)?;
    let name = target.file_name().map(|name| name.to_string_lossy());
    if name.is_some_and(|name| name.to_ascii_lowercase().ends_with(".creating")) {
        return Err(refused(format!(
            "the target {} has a name that ends in .creating, under which LigninDB opens no \
             database: give it another name",
            target.display()
        )));
    }
    let read = resolved(source);
    // The source's directory and the directories in it, and those it is
    // in.
    let of_source = directories_in(source);
    let around_source = identities(source);
    // The database is made in a directory of the work directory, which
    // can be there already and be another directory than its name says.
    let database = work.join(WORK_DATABASE);
    for made in [target, work.as_path(), database.as_path()] {
        let named = resolved(made);
        let by_name = named.starts_with(&read) || read.starts_with(&named);
        // And by what the directories are: one that `made` is in is the
        // source's or one in it, or `made` is one the source is in.
        let within = identities(made);
        let inside = within.iter().any(|at| of_source.contains(at));
        let around =
            std::fs::metadata(made).is_ok_and(|made| around_source.contains(&file_identity(&made)));
        if by_name || inside || around {
            return Err(refused(format!(
                "the target {} and the source {} are one inside the other: a migration writes \
                 nothing into its source",
                target.display(),
                source.display()
            )));
        }
    }
    // The target is another directory than the one the work is done in
    // and than the one the database is made in, where they are there.
    let one = |made: &Path| match (std::fs::metadata(target), std::fs::metadata(made)) {
        (Ok(target), Ok(made)) => file_identity(&target) == file_identity(&made),
        _ => false,
    };
    if one(&work) || one(&database) {
        return Err(refused(format!(
            "the target {} and {}, where its database is made, are one directory",
            target.display(),
            work.display()
        )));
    }
    Ok(work)
}

/// What a run has once its source is surveyed and found to be one it
/// takes.
struct Run<'r> {
    migration: &'r Migration,
    source: &'r Source,
    side: &'r Side,
    survey: &'r Survey,
    target: &'r Path,
    work: &'r Path,
    began: Instant,
}

impl Run<'_> {
    /// Makes the target: loads what is not loaded, compares, checks, and
    /// gives the directory its name.
    fn made(&self, out: &mut dyn Write) -> Result<Outcome, ToolError> {
        let Self {
            migration,
            source,
            side,
            survey,
            target,
            work,
            began,
        } = *self;
        let vfs = migration.options.vfs.as_ref();
        // The directory that the target and the work directory are in
        // says what each of the two is, without following a link.
        let above = above(target);
        let listed = match vfs.read_dir(above) {
            Ok(listed) => listed,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(refused(format!(
                    "the directory {} that the target {} is to be in is not there: make it \
                     first",
                    above.display(),
                    target.display()
                )));
            }
            Err(error) => return Err(ToolError::Io(error)),
        };
        let kind = |path: &Path| -> Result<Option<EntryKind>, ToolError> {
            let name = path.file_name();
            let named = listed
                .iter()
                .find(|entry| Some(entry.name.as_os_str()) == name);
            if let Some(entry) = named {
                return Ok(Some(entry.kind));
            }
            // A file system can find by a name what it lists under another
            // spelling of it. What the name leads to is then not known
            // without following it: the name is to be given as it is
            // listed.
            match vfs.read_dir(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                _ => Err(refused(format!(
                    "{} is found by that name, and the directory {} does not list it under \
                     that name: give the name as the directory has it",
                    path.display(),
                    above.display()
                ))),
            }
        };
        let holds = Work::looked(vfs, work, kind(work)?)?;
        // What is at the target, and whether it is a directory.
        let taken = match kind(target)? {
            None => None,
            Some(EntryKind::Dir) if vfs.read_dir(target)?.is_empty() => None,
            Some(EntryKind::Dir) => Some(("is not empty", true)),
            Some(_) => Some(("is no directory", false)),
        };
        if let Some((taken, directory)) = taken {
            // A directory at the target, and beside it the command's work
            // directory with no database, is what a run leaves that was
            // stopped after the rename: a name that a power cut can take
            // back, and a work directory that has done its work. It is
            // also what is left where something else was put at the
            // target after a run had made its work directory. The syncs
            // are made in either case, and nothing is said of what is at
            // the target but that it is there.
            match holds {
                Work::Ours { database: false } if directory => {
                    named_durably(vfs, work, above).map_err(|error| {
                        refused(format!(
                            "the target {} is there, and {} is this command's and holds no \
                             database: a run may have given the target its name and stopped \
                             before the name was durable. The syncs that make it so failed \
                             ({error}). Run the same command again",
                            target.display(),
                            work.display()
                        ))
                    })?;
                    let _ = cleared(vfs, work);
                }
                Work::Empty if directory => {
                    let _ = vfs.remove_dir(work);
                }
                _ => {}
            }
            return Err(refused(format!(
                "the target {} is there and {taken}. A migration makes its database in {} and \
                 gives it the name of the target at its end, once it was compared and \
                 verified: a database that a migration left at the target is whole",
                target.display(),
                work.display()
            )));
        }
        // The work directory is the command's by the file it makes there
        // first: nothing is made beside what someone else put there.
        match holds {
            Work::Foreign(what) => {
                return Err(refused(format!(
                    "{} is there and is not the work of a migration: {what}. Remove it, or \
                     give another target",
                    work.display()
                )));
            }
            Work::Absent => {
                vfs.create_dir(work)?;
                mark(vfs, work)?;
            }
            Work::Empty => mark(vfs, work)?,
            Work::CutShort => {
                vfs.remove_file(&work.join(WORK_MARK_NEW))?;
                mark(vfs, work)?;
            }
            // The file has its name: a run that was stopped before that
            // name was durable is gone on with here.
            Work::Ours { .. } => vfs.sync_dir(work)?,
        }
        // The name of the work directory is durable before a commit is
        // made in it, also where a run was stopped between the making of
        // the directory and this.
        vfs.sync_dir(above)?;
        let database = work.join(WORK_DATABASE);
        let options = Options {
            create_if_missing: true,
            ..migration.options.clone()
        };
        let db = Db::open(&database, &options)?;
        // The two subtrees that every grove has. The engine enters them
        // where they are absent, and says of a write that failed only
        // words: entered here, a device that is full is known as that.
        load::base(&db)?;
        let engine = LigninEngine::new(db)
            .map_err(|error| refused(format!("the work directory {}: {error}", work.display())))?;
        // What a load has written so far is no sound store, and a tool
        // does not panic over one: the store is checked at the end, by
        // `verify`.
        engine.database().unchecked_at_close();
        let grove = GroveDb::from_engine(engine);
        let engine = grove.engine();
        let db = engine.database();

        let fingerprint = source.fingerprint()?;
        let root_hash = survey.root_hash;
        let journal = match stands(db)? {
            // A database with a grove and no journal: a load that is
            // complete, whoever made it. It gets the name of the target
            // only as every other does: compared with the source record
            // by record, and checked.
            Stands::Loaded => None,
            Stands::Loading(journal) => {
                let same = (&journal.identity, journal.sequence, &journal.root_hash)
                    == (&fingerprint.identity, fingerprint.sequence, &root_hash);
                if !same {
                    return Err(refused(format!(
                        "{} holds a migration of another source, or of another state of this \
                         one (RocksDB identity {}, last write {}, root hash {}). Remove that \
                         directory to migrate anew",
                        work.display(),
                        String::from_utf8_lossy(&journal.identity).trim(),
                        journal.sequence,
                        hex(&journal.root_hash)
                    )));
                }
                // A journal that names no choice is of a source that holds
                // nothing beside its grove: no choice is made for it.
                let chosen = journal.unreachable;
                if chosen != Unreachable::Refuse && chosen != migration.unreachable {
                    return Err(refused(format!(
                        "the load in {} was begun with --unreachable {}, and goes on with \
                         that alone. Give it, or remove that directory to migrate anew",
                        work.display(),
                        chosen.name()
                    )));
                }
                writeln!(
                    out,
                    "going on with the load in {}: {} records were carried",
                    work.display(),
                    journal.carried.records()
                )?;
                writeln!(
                    out,
                    "migrate: resumed phase={} records={}",
                    journal.phase.name(),
                    journal.carried.records()
                )?;
                Some(*journal)
            }
            Stands::Fresh => Some(Journal {
                identity: fingerprint.identity.clone(),
                sequence: fingerprint.sequence,
                root_hash,
                // The choice is kept where there is something to choose
                // for.
                unreachable: match survey.orphans.is_empty() {
                    true => Unreachable::Refuse,
                    false => migration.unreachable,
                },
                phase: Phase::Grove,
                frames: Vec::new(),
                roots: None,
                carried: Carried::default(),
            }),
        };
        let needs = 2 * (survey.counts.bytes() + survey.root_keys.count.bytes) + (256 << 20);
        let free = free_bytes(work.parent().unwrap_or(Path::new(".")));
        writeln!(
            out,
            "the load needs about {} MiB on the device of {} while it runs; {}",
            needs.div_ceil(1 << 20),
            work.display(),
            match free {
                Some(free) => format!("{} MiB are free there", free >> 20),
                None => "how much is free there is not known".to_owned(),
            }
        )?;

        // Whether this run makes the whole target, from its first record.
        let whole = (journal.as_ref())
            .is_some_and(|journal| journal.phase == Phase::Grove && journal.frames.is_empty());
        if let Some(journal) = journal {
            let loading = Instant::now();
            let every = migration.progress;
            let mut load = Load::new(db, journal, migration.batch_bytes, every, out);
            if load.journal().phase == Phase::Grove {
                let frames = load.journal().frames.clone();
                load = Walk::resumed(source, side, load, false, frames).run()?;
            }
            if load.journal().phase == Phase::Roots {
                load.roots(source, engine)?;
            }
            load.drops(source, side)?;
            let (carried, commits) = (load.journal().carried, load.commits);
            db.flush()?;
            writeln!(
                out,
                "migrate: loaded subtrees={} merks={} nodes={} unlinked={} flat={} aux={} \
                 meta={} root_keys={} marks={} unreachable={} bytes={} commits={} seconds={:.1}",
                carried.subtrees,
                carried.merks,
                carried.nodes,
                carried.unlinked,
                carried.flat,
                carried.aux,
                carried.meta,
                carried.roots,
                carried.marks,
                carried.unreachable,
                carried.bytes,
                commits,
                loading.elapsed().as_secs_f64()
            )?;
        }

        let comparing = Instant::now();
        let compared = compare::compare(source, side, &grove, &root_hash)?;
        source.unchanged()?;
        writeln!(
            out,
            "migrate: compared subtrees={} data={} aux={} meta={} root_keys={} marks={} \
             unreachable_carried={} unreachable_left={} differing={} root_hash={} seconds={:.1}",
            compared.subtrees,
            compared.data,
            compared.aux,
            compared.meta,
            compared.roots,
            compared.marks,
            compared.unreachable_carried,
            compared.unreachable_left,
            compared.differing,
            hex(&root_hash),
            comparing.elapsed().as_secs_f64()
        )?;
        if !compared.is_same() {
            for difference in &compared.differences {
                writeln!(out, "  DIFFERS: {difference}")?;
            }
            writeln!(
                out,
                "THE TARGET IS NOT ITS SOURCE: {} differences. It is left in {} and is not to \
                 be used",
                compared.differing,
                work.display()
            )?;
            return Ok(Outcome::NotSound);
        }
        // What the target has of the records nothing leads to is what this
        // run is told becomes of them: a run that ends a load another run
        // made is told what that run was.
        let (carried, left) = (compared.unreachable_carried, compared.unreachable_left);
        let fits = match migration.unreachable {
            Unreachable::Refuse => carried == 0 && left == 0,
            Unreachable::Leave => carried == 0,
            Unreachable::Carry => left == 0,
        };
        if !fits {
            return Err(refused(format!(
                "of the prefixes of the source that nothing leads to, {} has {carried} carried \
                 and nothing of {left}, and this run has --unreachable {}. Give what the \
                 run that loaded it had, or remove that directory to migrate anew",
                work.display(),
                migration.unreachable.name()
            )));
        }
        writeln!(
            out,
            "every record of the source that this migration carries reads the same from the \
             target: {} records. The walk of the grove came to {} subtrees{}. The root hash \
             is {}",
            compared.of_the_source(),
            compared.subtrees,
            match carried {
                0 => String::new(),
                carried => format!(", and {carried} prefixes that nothing leads to are carried"),
            },
            hex(&root_hash)
        )?;
        let behind = survey.left_behind.records();
        if behind > 0 {
            writeln!(
                out,
                "not in the target: {behind} records under the prefixes of pending drops, which \
                 {} marks stand for",
                compared.marks
            )?;
        }
        if left > 0 {
            writeln!(
                out,
                "not in the target: the records under {left} prefixes that nothing leads to, \
                 which are left behind"
            )?;
        }

        let compared_at = db.version().get();
        drop(grove);
        let verifying = Instant::now();
        if !verify(&database, &migration.options, compared_at, out)? {
            writeln!(
                out,
                "THE TARGET IS NOT SOUND. It is left in {} and is not to be used",
                work.display()
            )?;
            return Ok(Outcome::NotSound);
        }
        writeln!(
            out,
            "migrate: verified seconds={:.1}",
            verifying.elapsed().as_secs_f64()
        )?;

        // An empty directory at the target makes room for the database.
        if vfs.read_dir(target).is_ok() {
            vfs.remove_dir(target)?;
        }
        vfs.rename(&database, target)?;
        // The database has its name. The two directories the name moved
        // between make it durable; where they do not, the error says what
        // is at the target, and is no error of a full device that a load
        // goes on from.
        if let Err(error) = named_durably(vfs, work, above) {
            return Err(refused(format!(
                "the database is at {}, loaded, compared and verified, and its name there \
                 could not be made durable ({error}): a power cut can take the name back and \
                 leave the database in {}. Run the same command again: it makes the name \
                 durable and then refuses the target as one that is there, or, where a power \
                 cut took the name back meanwhile, goes on from that directory",
                target.display(),
                database.display()
            )));
        }
        // From here the migration is made, whatever becomes of the work
        // directory, which holds its file alone, and of the lines that
        // say so: a report that cannot be written changes nothing of it.
        let mut last = String::new();
        if let Err(error) = cleared(vfs, work) {
            last.push_str(&format!(
                "the work directory {} was not removed ({error}): it holds nothing of the \
                 target, and can be removed\n",
                work.display()
            ));
        }
        // The records of the data keyspaces: nodes, records no tree links
        // and flat records.
        let (elements, seconds) = (compared.data, began.elapsed().as_secs_f64());
        last.push_str(&format!(
            "migrate: done target={} seconds={seconds:.1} peak_memory_bytes={} \
             elements={elements}{}\n",
            target.display(),
            peak_memory().map_or("unknown".to_owned(), |bytes| bytes.to_string()),
            // The rate of a run that made the whole target: its survey,
            // its load, its comparison and its check.
            match whole {
                true => format!(" elements_per_second={:.0}", elements as f64 / seconds),
                false => String::new(),
            }
        ));
        last.push_str(&format!(
            "migrated {} into {}: loaded, compared and verified\n",
            source.dir().display(),
            target.display()
        ));
        let _ = out.write_all(last.as_bytes());
        Ok(Outcome::Done)
    }
}

/// The writer of a run's report, which keeps whether a write to it
/// failed: an error of the report is not one of the device that the
/// database is on.
struct Report<'w> {
    to: &'w mut dyn Write,
    failed: bool,
}

impl Write for Report<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.to.write(bytes);
        self.failed = self.failed || written.is_err();
        written
    }

    fn flush(&mut self) -> io::Result<()> {
        let flushed = self.to.flush();
        self.failed = self.failed || flushed.is_err();
        flushed
    }
}

/// The stack of the thread a migration runs on. The walk of the grove goes
/// a call deeper for each tree on a path ([`walk::DEEPEST`]), and so do
/// reads of GroveDB and checks of the engine: none of them is to end the
/// stack of whichever thread the caller is on. The memory of a stack is
/// taken as it is used.
const STACK_BYTES: usize = 64 << 20;

/// What the thread of a migration writes, made by the caller's writer on
/// the caller's thread: each write waits until it is made there, and has
/// its result.
struct Relayed {
    to: mpsc::Sender<Vec<u8>>,
    made: mpsc::Receiver<io::Result<()>>,
}

impl Write for Relayed {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let gone = || io::Error::other("the writer of the migration's report is gone");
        self.to.send(bytes.to_vec()).map_err(|_| gone())?;
        self.made.recv().map_err(|_| gone())??;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Runs `migration`, and writes what it does and finds to `out`: lines
/// for a reader, and among them lines that begin with `migrate:` and hold
/// `name=value` words.
///
/// The work is done on a thread of its own, with a stack for a deep grove
/// (`STACK_BYTES`); `out` is written on the caller's thread, each write
/// before the migration goes on.
///
/// On Linux a caller that has the source open for writing in its own
/// process is refused. On another system a caller must not have the
/// source open in its own process at all, for writing or by a second
/// migration: the module `source` says why.
pub fn migrate(migration: &Migration, out: &mut dyn Write) -> Result<Outcome, ToolError> {
    thread::scope(|scope| {
        // The ends of the two channels that this thread holds are of this
        // block: where the caller's writer panics they are let go of
        // before the scope waits for the migration, whose next write then
        // fails and ends it.
        let (to, written) = mpsc::channel::<Vec<u8>>();
        let (wrote, made) = mpsc::channel::<io::Result<()>>();
        let worker = thread::Builder::new()
            .name("lignin-migrate".to_owned())
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, move || run(migration, &mut Relayed { to, made }))?;
        // The writes of the migration, in its order. The loop ends when
        // the migration has ended and let go of its end of the channel.
        for bytes in written {
            if wrote.send(out.write_all(&bytes)).is_err() {
                break;
            }
        }
        match worker.join() {
            Ok(ended) => ended,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

/// [`migrate`], on the thread it runs on.
fn run(migration: &Migration, out: &mut dyn Write) -> Result<Outcome, ToolError> {
    let began = Instant::now();
    // Where the database is made is apart from the source: that is known
    // before anything is opened.
    let work = match &migration.target {
        Some(target) => Some(placed(&migration.source, target)?),
        None => None,
    };
    let source = Source::open(&migration.source)?;
    let fingerprint = source.fingerprint()?;
    writeln!(
        out,
        "the source {}: RocksDB identity {}, last write {}; {}",
        source.dir().display(),
        String::from_utf8_lossy(&fingerprint.identity).trim(),
        fingerprint.sequence,
        match source.is_locked() {
            true => "no process has it open for writing, and none can open it while this runs",
            false => {
                "it has no LOCK file, so nothing keeps a process from opening it for writing \
                 while this runs: a file that such a process makes or changes before the \
                 target was compared is found then, and what it does later is not seen"
            }
        }
    )?;
    let side = Side::read(&source)?;

    // Every run surveys its source, also one that goes on with a load or
    // finds one complete: what refuses a source is looked for in the
    // source as it is now.
    let surveying = Instant::now();
    // The paths of the prefixes that nothing leads to cost a walk of the
    // grove: a run that only surveys looks for them.
    let survey = survey::survey(&source, &side, migration.target.is_none())?;
    write_survey(&survey, migration.unreachable, out)?;
    writeln!(
        out,
        "migrate: surveyed seconds={:.1}",
        surveying.elapsed().as_secs_f64()
    )?;
    if !survey.is_taken(migration.unreachable) {
        return Err(refused(format!(
            "the source {} holds what a migration does not carry (the lines NOT CARRIED \
             above): nothing was written by this run",
            source.dir().display()
        )));
    }
    let unreachable: u64 = (survey.orphans.iter())
        .map(|orphan| orphan.counts.records())
        .sum();
    if unreachable > 0 {
        writeln!(
            out,
            "{}, AS --unreachable {} SAYS: {unreachable} records under {} prefixes that no \
             element leads to",
            match migration.unreachable {
                Unreachable::Carry => "CARRIED AS RECORDS NO TREE LINKS",
                _ => "LEFT BEHIND",
            },
            migration.unreachable.name(),
            survey.orphans.len()
        )?;
    }
    // What the run holds at its peak is known before it writes anything.
    // The sums are made in a width that holds each of them: one that no
    // number of the limit's width holds is above every limit.
    let heaviest = survey.heaviest(migration.unreachable);
    // What the run's own settings add, and what no setting makes less.
    let settings = migration.batch_bytes as u128 + migration.options.cache_size as u128;
    let whole = u128::from(PEAK_FLOOR) + u128::from(heaviest.peak());
    let projected = whole + settings;
    let floor = u128::from(PEAK_FLOOR) + settings;
    let (limit, whose) = match (migration.memory_limit, machine_memory()) {
        (Some(limit), _) => (Some(limit), "as --memory-limit says".to_owned()),
        (None, Some(machine)) => (
            Some(machine / 2),
            format!(
                "half of the {} MiB of this machine, as no --memory-limit is given",
                machine >> 20
            ),
        ),
        (None, None) => (None, String::new()),
    };
    writeln!(
        out,
        "the migration projects a peak of about {} MiB of memory: {} MiB for a commit and the \
         buffer pool, and about {PEAK_A_NODE} bytes with its key and its value for each node \
         of the largest Merk it hands over, which it holds whole ({}). {}",
        projected.div_ceil(1 << 20),
        floor >> 20,
        heaviest.named(),
        match limit {
            Some(limit) => format!("The limit is {} MiB: {whose}", limit >> 20),
            None => "The memory of the machine is not read here (/proc/meminfo is not \
                     there, cannot be read or states no MemTotal), and no --memory-limit \
                     is given: no limit is held"
                .to_owned(),
        }
    )?;
    writeln!(
        out,
        "migrate: memory projected_peak_bytes={} limit_bytes={}",
        // A sum that no number of this width holds is shown as the
        // largest.
        u64::try_from(projected).unwrap_or(u64::MAX),
        limit.map_or("none".to_owned(), |limit| limit.to_string())
    )?;
    if let Some(limit) = limit.filter(|limit| projected > u128::from(*limit)) {
        // Which part is above the limit says what can answer the refusal,
        // as far as anything can: a limit has a largest value, and a commit
        // and the buffer pool have a least size.
        let more = "a larger --memory-limit can take it, where there is one and the machine \
                    has that memory";
        let cause = if u128::from(PEAK_FLOOR) > u128::from(limit) {
            format!(
                "the limit is below the 256 MiB that a run holds whatever it migrates and \
                 however it is set. Only {more}"
            )
        } else if whole > u128::from(limit) {
            format!(
                "the largest of what it holds whole is {}, which with the 256 MiB of any run \
                 is about {} MiB, and no --batch or --cache makes that less. Only {more}. A \
                 load of it in parts, which such a source needs otherwise, is not built",
                heaviest.named(),
                whole.div_ceil(1 << 20)
            )
        } else {
            let least = whole + 2 * LEAST_OF_A_SETTING;
            let stated = format!(
                "--batch and --cache are {} MiB of it, and without them the run projects \
                 about {} MiB, which the limit holds",
                settings.div_ceil(1 << 20),
                whole.div_ceil(1 << 20)
            );
            if least > u128::from(limit) {
                format!(
                    "{stated}. But each of the two is 1 MiB or more, and with both at 1 MiB \
                     the run still projects about {} MiB. Only {more}",
                    least.div_ceil(1 << 20)
                )
            } else {
                format!(
                    "{stated}. A migration with a smaller --batch or --cache can take it: \
                     with both at 1 MiB, the least each can be, the run projects about {} \
                     MiB. Or {more}",
                    least.div_ceil(1 << 20)
                )
            }
        };
        return Err(refused(format!(
            "the source {} projects a peak of about {} MiB of memory, above the limit of {} \
             MiB ({whose}). Nothing was written by this run: {cause}",
            source.dir().display(),
            projected.div_ceil(1 << 20),
            limit >> 20
        )));
    }
    let (Some(target), Some(work)) = (&migration.target, &work) else {
        source.unchanged()?;
        writeln!(out, "the source can be migrated")?;
        return Ok(Outcome::Done);
    };

    let run = Run {
        migration,
        source: &source,
        side: &side,
        survey: &survey,
        target,
        work,
        began,
    };
    let mut report = Report {
        to: &mut *out,
        failed: false,
    };
    let ended = run.made(&mut report);
    let reported = !report.failed;
    match ended {
        // A device that is full, at whichever step: what is in the work
        // directory is what the next run goes on from. An error of the
        // report is none of that device.
        Err(error) if reported && lignindb::space::out_of_space(&error) => {
            // The status says it, also where the report takes no line.
            let _ = writeln!(
                out,
                "THE DEVICE OF {} IS FULL: the migration stopped ({error}). Make room and run \
                 the same command again: it goes on from where this run stood",
                work.display()
            );
            Ok(Outcome::IncompleteMigration)
        }
        ended => ended,
    }
}
