//! `lignin migrate SOURCE TARGET`: a RocksDB GroveDB into a new LigninDB
//! database, offline ([the plan](https://github.com/dashpay/grovedb/issues/1052),
//! task OPS-02).
//!
//! A node that is migrated goes on from where its RocksDB database stood.
//! So the target has to answer every later operation as the source would
//! have: the same root hash, and the same answer, proof and cost for each
//! read and write. The migrator therefore carries every record that a
//! read of GroveDB can come to, as it is, and says of each record of the
//! source what became of it ([`survey`]).
//!
//! # What it does, in order
//!
//! 1. **It opens the source for reading** ([`source`]): RocksDB's
//!    read-only open, behind a lock that a running node holds. The source
//!    is the directory of a stopped node, or a checkpoint. Nothing is
//!    written there, and a write-ahead log that the node left is replayed
//!    in memory: the migration is of the state the node would recover to.
//!    A target that lies inside the source, or around it, is refused
//!    before anything is opened.
//! 2. **It surveys the source** ([`survey`]): every record of every
//!    keyspace is counted as what it is, before the run writes anything.
//!    A source that holds a record the migrator does not carry is refused
//!    here, with what was found. `--survey` stops after this step. **Every
//!    run surveys**, also one that goes on with a load or finds one
//!    complete: what refuses a source is looked for in the source as it
//!    is when the run reads it.
//! 3. **It loads the target** ([`load`]) in `TARGET.migrating/database`,
//!    a commit at a time, each Merk handed to the engine whole through its
//!    bulk loader. Every commit holds the journal of where the load stands
//!    ([`journal`]).
//! 4. **It compares** ([`compare`]): the source is read once more, and
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
//! is not whole, compared and verified.
//!
//! `TARGET.migrating` holds the work: a file `MIGRATION`, which the
//! command makes there before anything else and which says that the
//! directory is its own, and the database in `database`. **A run that is
//! stopped at any point, by a kill, a power cut or a full device, is gone
//! on with by the same command**: what the directory holds says where it
//! stands.
//!
//! | `TARGET.migrating` | What the next run does, after its survey |
//! | --- | --- |
//! | is not there, or is empty | makes it its own, and begins the load |
//! | holds something and no file `MIGRATION` | refuses: it is not the work of a migration, and nothing is made beside what someone put there |
//! | is the command's, with no database or one with nothing in it | begins the load |
//! | is the command's, with a database that has a journal of this source | goes on with the load from the last commit: a Merk that was handed over is not handed over again |
//! | is the command's, with a database that has a journal of another source, of another state of this one, or of a load that was told otherwise what becomes of the records nothing leads to | refuses: the directory is to be removed, or the run is told what the first was |
//! | is the command's, with a database that has a grove and no journal | the load is complete: compares, verifies and renames. A run that died between the verification and the rename leaves this, and so does one whose comparison or verification failed |
//!
//! A run that died after the rename, before it said that it was done,
//! leaves the database at `TARGET`, and a work directory with nothing but
//! its file where it died before removing that. The next run refuses the
//! target as one that is there, and says that a database which a
//! migration left there is whole; such a work directory can be removed.
//!
//! A device that is full ends a run with status 4, at whichever step it
//! was met, and leaves one of the rows above: the run after room was made
//! goes on.
//!
//! **The work directory is the command's alone while it runs.** The lock
//! of the database keeps every other process out during the load and the
//! comparison, and again during the check. Between the two the lock is
//! given up and taken again, and a store that was written in between is
//! told by its commit version and refused. After the check the database
//! is renamed with no lock held, since a close of the database gives its
//! lock up: a process that opens `TARGET.migrating/database` in that
//! moment is not kept out.
//!
//! # Records that nothing leads to
//!
//! RocksDB can hold records under a prefix that no element leads to and no
//! pending-drop record names ([`beside`]). The survey says of each such
//! prefix what it holds. `--unreachable` says what becomes of them: the
//! source is refused, which is what happens unless the option is given;
//! they are left behind (`leave`), and the target is what a database
//! written on LigninDB holds; or they are carried (`carry`), and a write
//! below a tree that is made again at such a path is refused by the engine
//! as it stands ([`beside`] says what each choice gives a later
//! operation). A run that goes on with a work directory is given the
//! choice that the run which began it was given.
//!
//! # Memory
//!
//! A Merk is handed over whole, so every node of the largest Merk of the
//! source is in memory at once, with the engine's pages of it
//! (`transactions.md` LD7). The survey says how large that Merk is. Beside
//! it the migrator holds what one commit takes (`--batch`: the keys and
//! values it carries, and a page of the store for each Merk it hands
//! over), and the buffer pool of the target (`--cache`).
//!
//! # Exit status
//!
//! | Status | |
//! | --- | --- |
//! | 0 | the target is at `TARGET`: loaded, compared and verified. With `--survey`: the source can be migrated |
//! | 1 | the migration failed, or the source or the target was refused; the run put nothing at `TARGET` |
//! | 3 | the migrated store differs from its source, or a rule of it does not hold; it is left in `TARGET.migrating/database` |
//! | 4 | the device of the target is full, at any step after the survey; the same command goes on once there is room |

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
use grovedb_version::version::GroveVersion;
use lignin_io::{Create, OpenMode, Vfs};
use lignin_tree::hash::HashRules;
use lignindb::{
    txn::{Column, Subtree},
    Db, Options,
};

use self::{
    journal::{Carried, Journal, Phase, JOURNAL_KEY},
    load::Load,
    source::Source,
    survey::Survey,
    walk::{Side, Walk, ROOT},
};
pub use crate::args::Unreachable;
use crate::{hex, verify::left_hashes, Outcome, ToolError};

/// What the name of the directory a migration works in ends with.
pub const WORK_SUFFIX: &str = ".migrating";

/// The name of the database in the directory a migration works in.
pub const WORK_DATABASE: &str = "database";

/// The name of the file that says a directory is the work of a migration.
/// The command makes it before anything else there, and takes no directory
/// that holds something and not this file.
pub const WORK_MARK: &str = "MIGRATION";

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
    /// About how many bytes one commit takes: the keys and values it
    /// carries, and a page of the store for each Merk it hands over.
    pub batch_bytes: usize,
    /// How often the load says where it stands, at the commit it has come
    /// to: with no time, after every commit.
    pub progress: Duration,
    /// What becomes of the records under a prefix that no element leads
    /// to and no pending-drop record names ([`beside`]).
    pub unreachable: Unreachable,
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
    /// Nothing: no load was begun.
    Fresh,
    /// A load that is not complete.
    Loading(Journal),
    /// No journal, and a grove: the load is complete.
    Loaded,
}

fn stands(db: &Db) -> Result<Stands, ToolError> {
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let root = operation.open(Subtree::new(ROOT))?;
    let root = root.ok_or_else(|| refused("the target has no root Merk: it is no grove"))?;
    let mut journal = Vec::new();
    if root.get(Column::Aux, JOURNAL_KEY, &mut journal)? {
        return Ok(Stands::Loading(Journal::decode(&journal)?));
    }
    let mut subtrees = 0;
    let mut walk = operation.entries()?;
    while let Some(listed) = walk.next()? {
        subtrees += u64::from(listed.live);
    }
    let mut holds = root.entry().root().is_some() || subtrees > 2;
    for column in [Column::Data, Column::Aux, Column::Roots, Column::Meta] {
        let mut cursor = root.cursor(column);
        cursor.seek_to_first()?;
        holds = holds || cursor.key().is_some();
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

/// Makes the file that says `work` is the work directory of a migration,
/// and makes it durable with its name.
fn mark(vfs: &dyn Vfs, work: &Path) -> io::Result<()> {
    let file = vfs.open(&work.join(WORK_MARK), OpenMode::Append(Create::IfMissing))?;
    if file.len()? == 0 {
        file.append(
            b"The work directory of `lignin migrate`. The database it makes is in `database`, \
              and gets the name of the target once it is compared and verified.\n",
        )?;
    }
    file.sync_all()?;
    vfs.sync_dir(work)
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
    for made in [target, work.as_path()] {
        let made = resolved(made);
        if made.starts_with(&read) || read.starts_with(&made) {
            return Err(refused(format!(
                "the target {} and the source {} are one inside the other: a migration writes \
                 nothing into its source",
                target.display(),
                source.display()
            )));
        }
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
        let vfs = &migration.options.vfs;
        match vfs.read_dir(target) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(entries) if entries.is_empty() => {}
            Ok(_) => {
                return Err(refused(format!(
                    "the target {} is there and is not empty. A migration makes its database \
                     in {} and gives it the name of the target at its end, once it was \
                     compared and verified: a database that a migration left at the target is \
                     whole",
                    target.display(),
                    work.display()
                )));
            }
            Err(error) => {
                return Err(refused(format!(
                    "the target {} is no directory this command can make its database at: \
                     {error}",
                    target.display()
                )));
            }
        }
        // The work directory is the command's by the file it makes there
        // first: nothing is made beside what someone else put there.
        match vfs.read_dir(work) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                vfs.create_dir(work)?;
                mark(vfs.as_ref(), work)?;
            }
            Ok(entries) if entries.is_empty() => mark(vfs.as_ref(), work)?,
            Ok(entries) if entries.iter().any(|entry| entry.name == WORK_MARK) => {}
            Ok(_) => {
                return Err(refused(format!(
                    "{} is there and is not the work of a migration: it holds something, and \
                     no file {WORK_MARK}. Remove it, or give another target",
                    work.display()
                )));
            }
            Err(error) => return Err(ToolError::Io(error)),
        }
        let database = work.join(WORK_DATABASE);
        let options = Options {
            create_if_missing: true,
            ..migration.options.clone()
        };
        let db = Db::open(&database, &options)?;
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
                Some(journal)
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
                "THE TARGET IS NOT ITS SOURCE: {} differences in {} records. It is left in {} \
                 and is not to be used",
                compared.differing,
                compared.records(),
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
                 and nothing of {left}, and this run was told --unreachable {}. Give what the \
                 run that loaded it was given, or remove that directory to migrate anew",
                work.display(),
                migration.unreachable.name()
            )));
        }
        writeln!(
            out,
            "every record of the source that a migration carries reads the same from the \
             target ({} records in {} subtrees), and the root hash is {}",
            compared.records(),
            compared.subtrees,
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
        // The two directories the name moved between.
        vfs.sync_dir(work)?;
        if let Some(above) = target.parent() {
            let above = match above.as_os_str().is_empty() {
                true => Path::new("."),
                false => above,
            };
            vfs.sync_dir(above)?;
        }
        // The work directory has done its work: its file is all it holds.
        let cleared = (vfs.remove_file(&work.join(WORK_MARK))).and_then(|()| vfs.remove_dir(work));
        if let Err(error) = cleared {
            writeln!(
                out,
                "the work directory {} was not removed ({error}): it holds nothing of the \
                 target, and can be removed",
                work.display()
            )?;
        }
        // The records of the data keyspaces: nodes, records no tree links
        // and flat records.
        let (elements, seconds) = (compared.data, began.elapsed().as_secs_f64());
        writeln!(
            out,
            "migrate: done target={} seconds={seconds:.1} peak_memory_bytes={} elements={elements}{}",
            target.display(),
            peak_memory().map_or("unknown".to_owned(), |bytes| bytes.to_string()),
            // The rate of a run that made the whole target: its survey,
            // its load, its comparison and its check.
            match whole {
                true => format!(" elements_per_second={:.0}", elements as f64 / seconds),
                false => String::new(),
            }
        )?;
        writeln!(
            out,
            "migrated {} into {}: loaded, compared and verified",
            source.dir().display(),
            target.display()
        )?;
        Ok(Outcome::Done)
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
/// ([`STACK_BYTES`]); `out` is written on the caller's thread, each write
/// before the migration goes on.
///
/// A caller that has the source open in its own process is refused on
/// Linux, and must not call this on a system that does not list the locks
/// of a process ([`source`]).
pub fn migrate(migration: &Migration, out: &mut dyn Write) -> Result<Outcome, ToolError> {
    let (to, written) = mpsc::channel::<Vec<u8>>();
    let (wrote, made) = mpsc::channel::<io::Result<()>>();
    thread::scope(|scope| {
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
            false => "it has no LOCK file: it was not opened for writing since it was made",
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
    match run.made(out) {
        // A device that is full, at whichever step: what is in the work
        // directory is what the next run goes on from.
        Err(error) if lignindb::space::out_of_space(&error) => {
            writeln!(
                out,
                "THE DEVICE OF {} IS FULL: the migration stopped ({error}). Make room and run \
                 the same command again: it goes on from where this run stood",
                work.display()
            )?;
            Ok(Outcome::IncompleteMigration)
        }
        ended => ended,
    }
}
