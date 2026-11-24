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
//! 2. **It surveys the source** ([`survey`]): every record of every
//!    keyspace is counted as what it is, before anything is written. A
//!    source that holds a record the migrator does not carry is refused
//!    here, with what was found. `--survey` stops after this step.
//! 3. **It loads the target** ([`load`]) in `TARGET.migrating`, a commit
//!    at a time, each Merk handed to the engine whole through its bulk
//!    loader. Every commit holds the journal of where the load stands
//!    ([`journal`]).
//! 4. **It compares** ([`compare`]): the source is read once more, and
//!    every record of it is held to what the target answers through the
//!    engine of GroveDB, with the root hash.
//! 5. **It verifies** the target with the physical invariant checker,
//!    every stored hash made again by Merk's rules, as `lignin verify`
//!    does.
//! 6. **It gives the directory the name `TARGET`**, and makes that name
//!    durable.
//!
//! # What is at `TARGET`, and what a stop leaves
//!
//! Nothing is at `TARGET` until step 6: a path that is there and is not an
//! empty directory is refused. So a node cannot be started on a store that
//! is not whole, compared and verified.
//!
//! `TARGET.migrating` holds the work. **A run that is stopped at any point,
//! by a kill, a power cut or a full device, is gone on with by the same
//! command**: what the directory holds says where it stands.
//!
//! | `TARGET.migrating` | What the next run does |
//! | --- | --- |
//! | is not there, or holds a database with nothing in it | surveys, and begins the load |
//! | holds a database with a journal of this source | goes on with the load from the last commit: a Merk that was handed over is not handed over again |
//! | holds a database with a journal of another source, or of another state of this one | refuses: the directory is to be removed |
//! | holds a database with no journal | the load is complete: compares, verifies and renames. A run that died between the verification and the rename leaves this, and so does one whose comparison or verification failed |
//!
//! A device that is full ends a run with status 4 and leaves the second
//! row: the run after room was made goes on.
//!
//! # Memory
//!
//! A Merk is handed over whole, so every node of the largest Merk of the
//! source is in memory at once, with the engine's pages of it
//! (`transactions.md` LD7). The survey says how large that Merk is. Beside
//! it the migrator holds what one commit takes (`--batch`), and the buffer
//! pool of the target (`--cache`).
//!
//! # Exit status
//!
//! | Status | |
//! | --- | --- |
//! | 0 | the target is at `TARGET`: loaded, compared and verified. With `--survey`: the source can be migrated |
//! | 1 | the migration failed, or the source or the target was refused; nothing is at `TARGET` |
//! | 3 | the migrated store differs from its source, or a rule of it does not hold; it is left in `TARGET.migrating` |
//! | 4 | the device of the target is full; the same command goes on once there is room |

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
    time::Instant,
};

use grovedb::{GroveDb, LigninEngine};
use grovedb_merk::store::lignin::{hash::MerkHashRules, keeps_flat_records};
use grovedb_version::version::GroveVersion;
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
use crate::{hex, verify::left_hashes, Outcome, ToolError};

/// What the name of the directory a migration works in ends with.
pub const WORK_SUFFIX: &str = ".migrating";

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
    /// About how many bytes of the source's records one commit takes.
    pub batch_bytes: usize,
    /// Whether the records under a prefix that no element leads to and no
    /// pending-drop record names are left behind. Without, a source that
    /// holds any is refused ([`survey`]).
    pub leave_unreachable: bool,
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

fn write_survey(survey: &Survey, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "the source holds:")?;
    for line in survey.report() {
        writeln!(out, "  {line}")?;
    }
    writeln!(out, "migrate: survey {}", survey.line())
}

/// Checks the store in `dir` with the physical invariant checker, every
/// stored hash made again by Merk's rules, and writes the report.
fn verify(dir: &Path, options: &Options, out: &mut dyn Write) -> Result<bool, ToolError> {
    let options = Options {
        create_if_missing: false,
        ..options.clone()
    };
    let db = Db::open(dir, &options)?;
    db.unchecked_at_close();
    db.check_kinds_by(keeps_flat_records);
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

/// Runs `migration`, and writes what it does and finds to `out`: lines
/// for a reader, and among them lines that begin with `migrate:` and hold
/// `name=value` words.
pub fn migrate(migration: &Migration, out: &mut dyn Write) -> Result<Outcome, ToolError> {
    let began = Instant::now();
    let source = Source::open(&migration.source)?;
    let fingerprint = source.fingerprint()?;
    writeln!(
        out,
        "the source {}: RocksDB identity {}, last write {}; {}",
        source.dir().display(),
        String::from_utf8_lossy(&fingerprint.identity).trim(),
        fingerprint.sequence,
        match source.is_locked() {
            true => "no process has it open, and none can open it while this runs",
            false => "it has no LOCK file: it was not opened for writing since it was made",
        }
    )?;
    let side = Side::read(&source)?;
    let surveyed = |out: &mut dyn Write| -> Result<Survey, ToolError> {
        let began = Instant::now();
        let survey = survey::survey(&source, &side)?;
        write_survey(&survey, out)?;
        writeln!(
            out,
            "migrate: surveyed seconds={:.1}",
            began.elapsed().as_secs_f64()
        )?;
        let unreachable: u64 = (survey.orphans.iter())
            .map(|orphan| orphan.counts.records())
            .sum();
        if survey.faults.is_empty() && unreachable > 0 && migration.leave_unreachable {
            writeln!(
                out,
                "LEFT BEHIND, AS --leave-unreachable SAYS: {unreachable} records under {} \
                 prefixes that no element leads to",
                survey.orphans.len()
            )?;
            return Ok(survey);
        }
        match survey.is_carried() {
            true => Ok(survey),
            false => Err(refused(format!(
                "the source {} holds what a migration does not carry (the lines NOT CARRIED \
                 above): nothing was written",
                source.dir().display()
            ))),
        }
    };
    let Some(target) = &migration.target else {
        surveyed(out)?;
        source.unchanged()?;
        writeln!(out, "the source can be migrated")?;
        return Ok(Outcome::Done);
    };

    let vfs = &migration.options.vfs;
    let work = work_dir(target)?;
    match vfs.read_dir(target) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(entries) if entries.is_empty() => {}
        Ok(_) => {
            return Err(refused(format!(
                "the target {} is there and is not empty. A migration makes its database in \
                 {} and gives it the name of the target at its end",
                target.display(),
                work.display()
            )));
        }
        Err(error) => {
            return Err(refused(format!(
                "the target {} is no directory this command can make its database at: {error}",
                target.display()
            )));
        }
    }
    let begun = match vfs.read_dir(&work) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Ok(entries) => !entries.is_empty(),
        Err(error) => return Err(ToolError::Io(error)),
    };
    // A source that is not carried is refused before anything is written:
    // where no work was begun, the survey comes before the database.
    let mut survey = match begun {
        true => None,
        false => Some(surveyed(out)?),
    };

    let options = Options {
        create_if_missing: true,
        ..migration.options.clone()
    };
    let engine = LigninEngine::open_with(&work, &options)
        .map_err(|error| refused(format!("the work directory {}: {error}", work.display())))?;
    // What a load has written so far is no sound store, and a tool does
    // not panic over one: the store is checked at the end, by `verify`.
    engine.database().unchecked_at_close();
    let grove = GroveDb::from_engine(engine);
    let engine = grove.engine();
    let db = engine.database();

    let root_hash = survey::root_hash(&source)?;
    let journal = match stands(db)? {
        Stands::Loaded => None,
        Stands::Loading(journal) => {
            let same = (&journal.identity, journal.sequence, &journal.root_hash)
                == (&fingerprint.identity, fingerprint.sequence, &root_hash);
            if !same {
                return Err(refused(format!(
                    "{} holds a migration of another source, or of another state of this one \
                     (RocksDB identity {}, last write {}, root hash {}). Remove that directory \
                     to migrate anew",
                    work.display(),
                    String::from_utf8_lossy(&journal.identity).trim(),
                    journal.sequence,
                    hex(&journal.root_hash)
                )));
            }
            writeln!(
                out,
                "going on with the load in {}: {} records were carried",
                work.display(),
                journal.carried.records()
            )?;
            Some(journal)
        }
        Stands::Fresh => {
            if survey.is_none() {
                survey = Some(surveyed(out)?);
            }
            Some(Journal {
                identity: fingerprint.identity.clone(),
                sequence: fingerprint.sequence,
                root_hash,
                phase: Phase::Grove,
                frames: Vec::new(),
                roots: None,
                carried: Carried::default(),
            })
        }
    };
    if let Some(survey) = &survey {
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
    }

    if let Some(journal) = journal {
        let loaded = (|| -> Result<Carried, ToolError> {
            let loading = Instant::now();
            let mut load = Load::new(db, journal, migration.batch_bytes, out);
            if load.journal().phase == Phase::Grove {
                let frames = load.journal().frames.clone();
                load = Walk::resumed(&source, &side, load, false, frames).run()?;
            }
            if load.journal().phase == Phase::Roots {
                load.roots(&source, engine)?;
            }
            load.drops(&source)?;
            let (carried, commits) = (load.journal().carried, load.commits);
            db.flush()?;
            writeln!(
                out,
                "migrate: loaded subtrees={} merks={} nodes={} unlinked={} flat={} aux={} \
                 meta={} root_keys={} marks={} bytes={} commits={} seconds={:.1}",
                carried.subtrees,
                carried.merks,
                carried.nodes,
                carried.unlinked,
                carried.flat,
                carried.aux,
                carried.meta,
                carried.roots,
                carried.marks,
                carried.bytes,
                commits,
                loading.elapsed().as_secs_f64()
            )?;
            Ok(carried)
        })();
        if let Err(error) = loaded {
            if lignindb::space::out_of_space(&error) {
                writeln!(
                    out,
                    "THE DEVICE OF {} IS FULL: the load stopped ({error}). Make room and run \
                     the same command again: it goes on from the last commit",
                    work.display()
                )?;
                return Ok(Outcome::IncompleteMigration);
            }
            return Err(error);
        }
    }

    let comparing = Instant::now();
    let compared = compare::compare(&source, &side, &grove, &root_hash)?;
    source.unchanged()?;
    writeln!(
        out,
        "migrate: compared subtrees={} data={} aux={} meta={} root_keys={} marks={} \
         differing={} root_hash={} seconds={:.1}",
        compared.subtrees,
        compared.data,
        compared.aux,
        compared.meta,
        compared.roots,
        compared.marks,
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
            "THE TARGET IS NOT ITS SOURCE: {} differences in {} records. It is left in {} and \
             is not to be used",
            compared.differing,
            compared.records(),
            work.display()
        )?;
        return Ok(Outcome::NotSound);
    }
    writeln!(
        out,
        "every record of the source reads the same from the target ({} records in {} \
         subtrees), and the root hash is {}",
        compared.records(),
        compared.subtrees,
        hex(&root_hash)
    )?;

    drop(grove);
    let verifying = Instant::now();
    if !verify(&work, &migration.options, out)? {
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
    vfs.rename(&work, target)?;
    if let Some(above) = target.parent() {
        let above = match above.as_os_str().is_empty() {
            true => Path::new("."),
            false => above,
        };
        vfs.sync_dir(above)?;
    }
    writeln!(
        out,
        "migrate: done target={} seconds={:.1} peak_memory_bytes={}",
        target.display(),
        began.elapsed().as_secs_f64(),
        peak_memory().map_or("unknown".to_owned(), |bytes| bytes.to_string())
    )?;
    writeln!(
        out,
        "migrated {} into {}: loaded, compared and verified",
        source.dir().display(),
        target.display()
    )?;
    Ok(Outcome::Done)
}
