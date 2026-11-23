//! A database of the campaign: GroveDB on the LigninDB engine, opened on a
//! file layer the campaign gives it.
//!
//! Everything the campaign does to a database goes through here, and
//! through what a node calls: `GroveDb` for the operations of the stream,
//! its root hash, its flush, its wipe and its checkpoints, and the facade
//! of the engine (`lignindb::Db`) for a checkpoint that says how much log
//! write-back leaves to its first open (`Db::checkpoint`), for the removal
//! of a checkpoint on the campaign's file layer, and for the checker.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use grovedb::{engine::lignin::LigninEngine, Element, GroveDb, Transaction};
use grovedb_merk::store::lignin::hash::MerkHashRules;
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;
use lignin_format::PageSize;
use lignin_io::Vfs;
use lignin_store::{cleaner::CleanerConfig, flush::FlushConfig, wal::SyncPolicy};
use lignindb::{
    checkpoint::{self, CheckpointOptions},
    db::OpenError,
    Db, Options,
};

use super::model::{Hash, Places, State};
use crate::diff::state::stored;

/// GroveDB on the LigninDB engine.
pub type Grove = GroveDb<LigninEngine>;

/// A transaction of it.
pub type Tx<'db> = Transaction<'db, LigninEngine>;

/// The directory of the database of every lineage.
pub const DB: &str = "/db";

/// The directory the checkpoints of a database are made in.
pub const CHECKPOINTS: &str = "/checkpoints";

/// The directory of the database of every lineage.
pub fn db_dir() -> &'static Path {
    Path::new(DB)
}

/// The directory of checkpoint `number` of a lineage.
pub fn checkpoint_dir(number: u64) -> PathBuf {
    Path::new(CHECKPOINTS).join(format!("{number}"))
}

/// The root tree of a grove, as a path.
const ROOT: SubtreePath<'static, [u8; 0]> = SubtreePath::empty();

/// The key of the root tree under which the campaign writes with no
/// transaction, beside the block that is open. No workload writes there.
pub const BESIDE_KEY: &[u8] = b"\xfe\xfever-04 beside";

/// The key of the root tree under which a database that was recovered is
/// written, to show that it goes on.
pub const AGAIN_KEY: &[u8] = b"\xfe\xfever-04 again";

/// How a database is opened: the thresholds of its background work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    /// The size of the pages. A database keeps the one it was made with.
    pub page_size: PageSize,
    /// The memory of the buffer pool.
    pub cache_size: usize,
    /// The length at which a log file is full.
    pub log_file_len: u64,
    /// When a commit makes the log durable.
    pub sync: SyncPolicy,
    /// The thresholds of write-back.
    pub flush: FlushConfig,
    /// The policy of the cleaner.
    pub cleaner: CleanerConfig,
    /// How long the reclaimer sleeps when nobody wakes it.
    pub idle: Duration,
}

impl Tuning {
    /// A database that turns its files often and whose three threads are
    /// never far behind: log files of a few kilobytes, segments of a few
    /// pages, chains of two runs, a table checkpoint for every twelve
    /// kilobytes of log, and a cleaner for which a file can be closed
    /// once it holds two kilobytes, which copies thirty-two kilobytes of
    /// blocks in a pass, cleans a value-log file of which a fifth is
    /// dead, and works towards a tenth of dead blocks. Each of the three
    /// loops sleeps a millisecond when nobody wakes it; write-back waits
    /// longer after turns that failed (`FlushConfig::wait_after_failure`).
    pub fn small(page_size: PageSize) -> Self {
        let eager = Duration::from_millis(1);
        Self {
            page_size,
            cache_size: 512 << 10,
            log_file_len: 6_000,
            sync: SyncPolicy::Never,
            flush: FlushConfig {
                max_chain_deltas: 2,
                segment_len: 48 << 10,
                max_unwritten: 12,
                max_pending_bytes: 24 << 10,
                max_log_held: 48 << 10,
                checkpoint_log_bytes: 12 << 10,
                idle: eager,
                ..FlushConfig::new(page_size)
            },
            cleaner: CleanerConfig {
                target_dead_percent: 10,
                checkpoint_dead_percent: 30,
                seal_dead_percent: 5,
                max_segment_percent: 40,
                min_segment_len: 2 << 10,
                max_pass_bytes: 32 << 10,
                segment_len: 48 << 10,
                value_dead_percent: 20,
                idle: eager,
            },
            idle: eager,
        }
    }

    /// The tuning of a session, by turns: the pool, the length of a log
    /// file, the runs a chain may have, so that one session finds chains
    /// that another's thresholds allowed, the dead space the cleaner
    /// leaves, and whether a commit syncs the log.
    pub fn of_turn(page_size: PageSize, turn: u64) -> Self {
        let small = Self::small(page_size);
        let pick = |every: u64, of: u64| ((turn / every) % of) as usize;
        Self {
            cache_size: [small.cache_size, 128 << 10, 2 << 20][pick(1, 3)],
            log_file_len: [small.log_file_len, 20_000][pick(3, 2)],
            sync: [
                SyncPolicy::Never,
                SyncPolicy::Never,
                SyncPolicy::EveryCommit,
                SyncPolicy::Never,
            ][pick(1, 4)],
            flush: FlushConfig {
                max_chain_deltas: [2, 0, 5][pick(2, 3)],
                ..small.flush
            },
            cleaner: CleanerConfig {
                target_dead_percent: [10, 0, 40][pick(5, 3)],
                value_dead_percent: [20, 0][pick(7, 2)],
                ..small.cleaner
            },
            ..small
        }
    }

    /// A database whose background work is left to its defaults, with a
    /// small pool: the tuning of the database that is never crashed, and
    /// of the databases that are opened to be looked at.
    pub fn at_rest(page_size: PageSize) -> Self {
        Self {
            page_size,
            cache_size: 2 << 20,
            log_file_len: 1 << 20,
            sync: SyncPolicy::Never,
            flush: FlushConfig::new(page_size),
            cleaner: CleanerConfig::new(),
            idle: Options::DEFAULT_IDLE,
        }
    }

    /// The options of the engine for this tuning, on `vfs`, with no
    /// reserve for a full device.
    pub fn options(&self, vfs: Arc<dyn Vfs>, create_if_missing: bool) -> Options {
        Options {
            create_if_missing,
            page_size: Some(self.page_size),
            cache_size: self.cache_size,
            sync: self.sync,
            log_file_len: Some(self.log_file_len),
            flush: Some(self.flush),
            cleaner: Some(self.cleaner),
            idle: self.idle,
            reserve: 0,
            vfs,
        }
    }
}

/// Why a directory was not opened as a database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotOpened {
    /// It holds no database, and none was to be made.
    NoDatabase,
    /// It is marked as being removed (`lignindb::checkpoint::delete`).
    Deleting,
    /// The engine did not open it.
    Failed(String),
}

impl std::fmt::Display for NotOpened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDatabase => f.write_str("the directory holds no database"),
            Self::Deleting => f.write_str("the directory is marked as being removed"),
            Self::Failed(error) => write!(f, "the database did not open: {error}"),
        }
    }
}

/// Opens the database in `dir` on `vfs` as a node opens it: the engine's
/// open, and GroveDB on it. With `create_if_missing`, a directory that
/// holds no database gets one.
pub fn open(
    vfs: Arc<dyn Vfs>,
    dir: &Path,
    tuning: &Tuning,
    create_if_missing: bool,
) -> Result<Grove, NotOpened> {
    let db =
        Db::open(dir, &tuning.options(vfs, create_if_missing)).map_err(|error| match error {
            OpenError::NoDatabase { .. } => NotOpened::NoDatabase,
            OpenError::Deleting { .. } => NotOpened::Deleting,
            other => NotOpened::Failed(other.to_string()),
        })?;
    let engine = LigninEngine::new(db).map_err(|error| NotOpened::Failed(error.to_string()))?;
    Ok(GroveDb::from_engine(engine))
}

/// The version of the engine that `db` is at.
pub fn version_of(db: &Grove) -> u64 {
    db.engine().database().version().get()
}

/// The root hash of the grove, as `through` reads it.
pub fn root_of<'db>(
    db: &'db Grove,
    through: &Tx<'db>,
    version: &GroveVersion,
) -> Result<Hash, String> {
    let root = db.root_hash(Some(through), version).unwrap();
    root.map_err(|error| format!("the root hash is not read: {error}"))
}

/// What `db` holds as `through` reads it: the root hash of the grove and
/// every record a walk from the root tree reaches, with what the subtrees
/// of `places` left behind that it no longer reaches. Also returns the
/// subtrees to tell the next walk of.
pub fn read<'db>(
    db: &'db Grove,
    through: &Tx<'db>,
    version: &GroveVersion,
    places: &Places,
) -> Result<(State, Places), String> {
    let root = root_of(db, through, version)?;
    let listing = stored(db, through, version, places)
        .map_err(|error| format!("the stored records are not read: {error}"))?;
    let marked = db
        .has_incomplete_restore()
        .map_err(|error| format!("the mark of a restore is not read: {error}"))?;
    let state = State::new(root, listing.clone(), marked);
    Ok((state, listing.handed_on()))
}

/// What the latest version of `db` holds ([`read`]), through a snapshot.
pub fn read_committed(
    db: &Grove,
    version: &GroveVersion,
    places: &Places,
) -> Result<(State, Places), String> {
    let snapshot = db.start_snapshot_read_transaction();
    read(db, &snapshot, version, places)
}

/// Checks `db` with the engine's physical invariant checker as a reader
/// sees it, beside the three threads of the database (`Db::verify`), with
/// the stored hashes of the nodes of every Merk held to Merk's rules for
/// a check (`MerkHashRules::for_check`): each is made again and compared,
/// but for the value hashes that those rules take as they are stored.
pub fn verify(db: &Grove) -> Result<(), String> {
    let mut rules = MerkHashRules::for_check(GroveVersion::latest());
    let report = db.engine().database().verify(Some(&mut rules));
    match report.is_sound() {
        true => Ok(()),
        false => Err(format!(
            "the checker of the engine, beside its threads: {report}"
        )),
    }
}

/// Opens the database in `dir` with the engine alone, closes it, and has
/// the physical invariant checker read the store that the close left
/// (`Db::close_and_verify`): every rule of the layout, what a store keeps
/// beside its pages, and the stored hashes by the same rules of Merk's.
/// Returns the version the database is at.
pub fn verify_closed(vfs: Arc<dyn Vfs>, dir: &Path, tuning: &Tuning) -> Result<u64, String> {
    let db = Db::open(dir, &tuning.options(vfs, false))
        .map_err(|error| format!("the database that was closed did not open again: {error}"))?;
    let version = db.version().get();
    let mut rules = MerkHashRules::for_check(GroveVersion::latest());
    let report = db
        .close_and_verify(Some(&mut rules))
        .map_err(|error| format!("the close: {error}"))?;
    match report.is_sound() {
        true => Ok(version),
        false => Err(format!(
            "the checker of the engine, after a close: {report}"
        )),
    }
}

/// Makes a checkpoint of `db` in `target` through GroveDB's own call
/// (`GroveDb::create_checkpoint`), which on this engine is `Db::checkpoint`
/// with the engine's defaults.
pub fn create_checkpoint(db: &Grove, target: &Path) -> Result<(), String> {
    db.create_checkpoint(target)
        .map_err(|error| error.to_string())
}

/// Makes a checkpoint of `db` in `target` through the facade of the
/// engine, with write-back catching up before the sample where it holds
/// more log than `catch_up_above`. Returns the version it holds.
pub fn checkpoint(db: &Grove, target: &Path, catch_up_above: Option<u64>) -> Result<u64, String> {
    let options = CheckpointOptions { catch_up_above };
    let created = db.engine().database().checkpoint(target, options);
    created
        .map(|created| created.version.get())
        .map_err(|error| error.to_string())
}

/// Removes the checkpoint in `dir`: `lignindb::checkpoint::delete`, which
/// is what GroveDB's `delete_checkpoint` calls on this engine, on the file
/// layer of the campaign.
pub fn delete_checkpoint(vfs: &dyn Vfs, dir: &Path) -> Result<(), String> {
    checkpoint::delete(vfs, dir).map_err(|error| error.to_string())
}

/// An item of the root tree, written through `through` or with no
/// transaction.
pub fn put_item<'db>(
    db: &'db Grove,
    key: &[u8],
    value: u64,
    through: Option<&Tx<'db>>,
    version: &GroveVersion,
) -> Result<(), String> {
    let item = Element::new_item(value.to_be_bytes().to_vec());
    let inserted = db.insert(ROOT, key, item, None, through, version).unwrap();
    inserted.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use lignin_io::sim::{SimConfig, SimVfs};

    use super::*;

    fn fs() -> Arc<dyn Vfs> {
        Arc::new(SimVfs::new(SimConfig::new(5)))
    }

    #[test]
    fn a_grove_is_made_written_read_checked_and_opened_again() {
        let (vfs, version) = (fs(), GroveVersion::latest());
        let tuning = Tuning::small(PageSize::MIN);
        assert_eq!(
            open(vfs.clone(), db_dir(), &tuning, false).map(|_| ()),
            Err(NotOpened::NoDatabase)
        );
        let db = open(vfs.clone(), db_dir(), &tuning, true).expect("made");
        // The open's own commits: the keyspaces, and the subtrees every
        // grove has.
        assert_eq!(version_of(&db), 2);
        let (empty, places) = read_committed(&db, version, &Places::new()).expect("read");
        assert!(!empty.incomplete_restore);
        put_item(&db, BESIDE_KEY, 7, None, version).expect("written");
        assert_eq!(version_of(&db), 3);
        let block = db.start_transaction();
        put_item(&db, AGAIN_KEY, 8, Some(&block), version).expect("written");
        let (ahead, _) = read(&db, &block, version, &places).expect("read");
        let (committed, _) = read_committed(&db, version, &places).expect("read");
        assert_ne!(ahead.root, committed.root);
        assert_ne!(committed.digest, empty.digest);
        db.commit_transaction(block).unwrap().expect("committed");
        let (now, _) = read_committed(&db, version, &places).expect("read");
        assert_eq!(now.difference(&ahead), None);
        verify(&db).expect("sound");

        // A checkpoint holds the version it was made at, and is a database
        // of its own.
        vfs.create_dir(Path::new(CHECKPOINTS)).expect("made");
        let target = checkpoint_dir(1);
        assert_eq!(checkpoint(&db, &target, None), Ok(4));
        assert!(checkpoint(&db, &target, Some(0)).is_err());
        assert!(create_checkpoint(&db, &target).is_err());
        // GroveDB's own call makes one as well.
        let second = checkpoint_dir(2);
        create_checkpoint(&db, &second).expect("made");
        let copy = open(vfs.clone(), &second, &Tuning::at_rest(PageSize::MIN), false);
        assert_eq!(version_of(&copy.expect("opens")), 4);
        put_item(&db, BESIDE_KEY, 9, None, version).expect("written");
        drop(db);
        let at_rest = Tuning::at_rest(PageSize::MIN);
        let copy = open(vfs.clone(), &target, &at_rest, false).expect("opens");
        let (held, _) = read_committed(&copy, version, &places).expect("read");
        assert_eq!(held.difference(&now), None);
        drop(copy);
        assert_eq!(verify_closed(vfs.clone(), &target, &at_rest), Ok(4));
        assert_eq!(verify_closed(vfs.clone(), db_dir(), &at_rest), Ok(5));
        delete_checkpoint(&*vfs, &target).expect("deleted");
        assert_eq!(
            open(vfs.clone(), &target, &at_rest, false).map(|_| ()),
            Err(NotOpened::NoDatabase)
        );
        assert!(delete_checkpoint(&*vfs, &target).is_err());
        assert!(verify_closed(vfs, &target, &at_rest).is_err());
    }

    #[test]
    fn the_tunings_go_by_turns() {
        let page = PageSize::MIN;
        let small = Tuning::small(page);
        assert_eq!(Tuning::of_turn(page, 0), small);
        let all: Vec<Tuning> = (0..84).map(|turn| Tuning::of_turn(page, turn)).collect();
        let syncing = (all.iter()).filter(|tuning| tuning.sync == SyncPolicy::EveryCommit);
        assert_eq!(syncing.count(), 21);
        for chain in [0, 2, 5] {
            assert!(all
                .iter()
                .any(|tuning| tuning.flush.max_chain_deltas == chain));
        }
        for dead in [0, 10, 40] {
            let has = |tuning: &Tuning| tuning.cleaner.target_dead_percent == dead;
            assert!(all.iter().any(has));
        }
        let options = small.options(fs(), false);
        assert!(!options.create_if_missing);
        assert_eq!(options.page_size, Some(page));
        assert_eq!(options.log_file_len, Some(6_000));
        assert_eq!(Tuning::at_rest(page).idle, Options::DEFAULT_IDLE);
        for not in [
            NotOpened::NoDatabase,
            NotOpened::Deleting,
            NotOpened::Failed("why".to_string()),
        ] {
            assert!(!not.to_string().is_empty());
        }
    }
}
