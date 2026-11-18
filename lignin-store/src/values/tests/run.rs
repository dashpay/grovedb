//! A workload on the model database, with a record of what every committed
//! version holds, written from the rules and not from the store.

use std::collections::BTreeMap;

use lignin_format::{manifest::LiveFile, value_log::ValuePointer, FileId, FileKind};
use lignin_io::sim::{SimRng, SimVfs};

use super::{
    pages::{Db, Logical, Txn},
    *,
};

/// The pages a drawn workload puts cells in: six tree pages and six flat
/// leaves.
const PAGES: u64 = 12;

/// One thing a transaction does to a cell.
#[derive(Debug, Clone)]
pub(super) enum Op {
    /// Gives the cell a value of `len` bytes, creating it in `page` if no
    /// cell has the key.
    Put { page: u32, key: u32, len: usize },
    /// Deletes the cell.
    Delete { key: u32 },
    /// Moves the cell to another page, with its value.
    Move { key: u32, to: u32 },
}

/// A put of a value of `len` bytes.
pub(super) fn put(page: u32, key: u32, len: usize) -> Op {
    Op::Put { page, key, len }
}

/// What a workload did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Tally {
    pub(super) commits: u64,
    pub(super) rollbacks: u64,
    pub(super) passes: u64,
    pub(super) moved: u64,
    pub(super) unowned: u64,
    pub(super) dropped: u64,
    pub(super) flushes: u64,
    pub(super) checkpoints: u64,
    pub(super) pins: u64,
}

pub(super) struct Run {
    pub(super) fs: SimVfs,
    /// The session, or `None` once it has ended.
    pub(super) db: Option<Db>,
    /// What each committed version holds, by version number. The last entry
    /// may be a commit whose outcome is not known.
    pub(super) history: Vec<Logical>,
    /// The newest version that a crash cannot take away.
    pub(super) durable: u64,
    /// The versions pinned by snapshots.
    pub(super) pins: Vec<u64>,
    pub(super) steps: u64,
    pub(super) tally: Tally,
    /// The pointers each key's cell has held at each version, since the
    /// journal was last made durable: the ones a recovery to that version
    /// may find. A rewrite that became durable takes the older pointer off.
    pub(super) pointers: BTreeMap<u64, BTreeMap<u32, Vec<ValuePointer>>>,
    rng: SimRng,
    next_key: u32,
}

impl Run {
    /// A workload on the database directory of `fs`, with no session yet.
    pub(super) fn new(fs: &SimVfs, seed: u64) -> Self {
        Self {
            fs: fs.clone(),
            db: None,
            history: vec![Logical::new()],
            durable: 0,
            pins: Vec::new(),
            steps: 0,
            tally: Tally::default(),
            pointers: BTreeMap::new(),
            rng: SimRng::new(seed),
            next_key: 1,
        }
    }

    /// A workload with a session on a new database.
    pub(super) fn started(fs: &SimVfs, seed: u64) -> Self {
        let mut run = Self::new(fs, seed);
        run.open().expect("the database is created");
        run
    }

    /// Starts a session: creates the database, or opens the one that is
    /// there.
    ///
    /// The version it finds is one the record allows, and is durable from
    /// here on: opening a database makes what it recovered durable.
    pub(super) fn open(&mut self) -> Result<(), ValueStoreError> {
        let db = Db::open(&self.fs)?;
        let version = db.pages.latest();
        assert!(
            self.durable <= version && version <= self.attempted(),
            "opened at version {version}, with {} durable and {} attempted",
            self.durable,
            self.attempted()
        );
        self.history.truncate(version as usize + 1);
        self.durable = version;
        self.db = Some(db);
        self.pointers.clear();
        self.note_pointers();
        Ok(())
    }

    pub(super) fn db(&mut self) -> &mut Db {
        self.db.as_mut().expect("a session")
    }

    fn latest(&self) -> u64 {
        self.db.as_ref().map_or(0, |db| db.pages.latest())
    }

    /// Notes the pointer each cell holds now, as one a recovery to the
    /// latest version may find.
    ///
    /// Once an append to the journal has failed, a crash can still bring
    /// the record back, with a rewrite this record of the run never saw
    /// installed. So nothing is said about pointers from then on.
    fn note_pointers(&mut self) {
        let Some(db) = &self.db else { return };
        if db.pages.journal_failed() {
            self.pointers.clear();
            return;
        }
        let held = self.pointers.entry(db.pages.latest()).or_default();
        for (_, cells) in db.pages.all() {
            for cell in cells {
                let seen: &mut Vec<ValuePointer> = held.entry(cell.key).or_default();
                if !seen.contains(&cell.pointer) {
                    seen.push(cell.pointer);
                }
            }
        }
    }

    /// The newest version whose commit may be in the journal.
    pub(super) fn attempted(&self) -> u64 {
        self.history.len() as u64 - 1
    }

    fn value(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.rng.next_u64() as u8).collect()
    }

    fn apply_ops(
        &mut self,
        txn: &mut Txn,
        ops: &[Op],
        next: &mut Logical,
    ) -> Result<(), ValueStoreError> {
        for op in ops {
            match *op {
                Op::Put { page, key, len } => {
                    let value = self.value(len);
                    let page = next.get(&key).map_or(super::page(page), |held| held.0);
                    self.db().put(txn, page, key, &value)?;
                    next.insert(key, (page, value));
                }
                Op::Delete { key } => {
                    let (page, _) = next.remove(&key).expect("the key");
                    self.db().delete(txn, page, key);
                }
                Op::Move { key, to } => {
                    let to = super::page(to);
                    let held = next.get_mut(&key).expect("the key");
                    let from = std::mem::replace(&mut held.0, to);
                    self.db().move_cell(txn, from, to, key)?;
                }
            }
        }
        Ok(())
    }

    /// Starts a transaction that does `ops`. Returns it with what the
    /// database holds once it has committed.
    pub(super) fn begin(&mut self, ops: &[Op]) -> Result<(Txn, Logical), ValueStoreError> {
        let mut txn = self.db().begin();
        let mut next = self.history[self.latest() as usize].clone();
        match self.apply_ops(&mut txn, ops, &mut next) {
            Ok(()) => Ok((txn, next)),
            Err(error) => {
                self.db().rollback(txn);
                Err(error)
            }
        }
    }

    /// Commits `txn`, after which the database holds `next`, or rolls it
    /// back. Returns the new version if it committed.
    pub(super) fn end(
        &mut self,
        txn: Txn,
        next: Logical,
        commit: bool,
    ) -> Result<Option<u64>, ValueStoreError> {
        if !commit {
            self.db().rollback(txn);
            self.tally.rollbacks += 1;
            return Ok(None);
        }
        let version = self.latest() + 1;
        self.history.truncate(version as usize);
        self.history.push(next);
        let committed = self.db().commit(txn);
        if self.latest() != version && !self.db().pages.journal_failed() {
            self.history.pop();
        }
        self.tally.commits += u64::from(committed.is_ok());
        self.note_pointers();
        committed.map(Some)
    }

    /// Runs `ops` in one transaction and commits it, or rolls it back.
    /// Returns the new version if it committed.
    pub(super) fn transact(
        &mut self,
        ops: &[Op],
        commit: bool,
    ) -> Result<Option<u64>, ValueStoreError> {
        let (txn, next) = self.begin(ops)?;
        self.end(txn, next, commit)
    }

    /// Commits a transaction that carries the cell `key` to the page `to`
    /// without its value, against the move rule.
    pub(super) fn carry(&mut self, key: u32, to: u32) -> Result<u64, ValueStoreError> {
        let (mut txn, mut next) = self.begin(&[])?;
        let held = next.get_mut(&key).expect("the key");
        let from = std::mem::replace(&mut held.0, super::page(to));
        self.db().carry(&mut txn, from, super::page(to), key);
        self.end(txn, next, true)
            .map(|version| version.expect("committed"))
    }

    /// Commits a transaction that appends the value of the cell `key` again
    /// for the page the cell is in. `recorded` is the page the value's
    /// record names as its owner.
    pub(super) fn append_again(&mut self, key: u32, recorded: u32) -> Result<u64, ValueStoreError> {
        let (mut txn, next) = self.begin(&[])?;
        let page = next[&key].0;
        let appended = self
            .db()
            .append_again(&mut txn, page, key, super::page(recorded));
        if let Err(error) = appended {
            self.db().rollback(txn);
            return Err(error);
        }
        self.end(txn, next, true)
            .map(|version| version.expect("committed"))
    }

    pub(super) fn commit(&mut self, ops: &[Op]) -> Result<u64, ValueStoreError> {
        self.transact(ops, true)
            .map(|version| version.expect("committed"))
    }

    pub(super) fn flush(&mut self) -> Result<(), ValueStoreError> {
        self.db().flush()?;
        self.durable = self.latest();
        self.tally.flushes += 1;
        self.pointers.clear();
        self.note_pointers();
        Ok(())
    }

    pub(super) fn checkpoint(&mut self) -> Result<(), ValueStoreError> {
        self.db().checkpoint()?;
        self.tally.checkpoints += 1;
        Ok(())
    }

    pub(super) fn clean(&mut self, file: FileId) -> Result<Cleaned, ValueStoreError> {
        let cleaned = self.db().clean(file);
        self.note_pointers();
        let cleaned = cleaned?;
        self.tally.passes += 1;
        self.tally.moved += cleaned.moved;
        self.tally.unowned += cleaned.unowned;
        Ok(cleaned)
    }

    pub(super) fn drop_empty(&mut self) -> Result<Vec<FileId>, ValueStoreError> {
        let dropped = self.db().drop_empty()?;
        self.tally.dropped += dropped.len() as u64;
        Ok(dropped)
    }

    pub(super) fn pin(&mut self) -> u64 {
        let version = self.db().pages.pin();
        self.pins.push(version);
        self.tally.pins += 1;
        version
    }

    pub(super) fn unpin(&mut self, version: u64) {
        let at = self
            .pins
            .iter()
            .position(|&pinned| pinned == version)
            .expect("the version is pinned");
        self.pins.swap_remove(at);
        self.db().pages.unpin(version);
    }

    fn draw_ops(&mut self) -> Vec<Op> {
        let mut held: Vec<(u32, u32)> = self.history[self.latest() as usize]
            .iter()
            .map(|(&key, (page, _))| (key, page.get()))
            .collect();
        let mut ops = Vec::new();
        for _ in 0..1 + self.rng.below(4) {
            let page = 1 + self.rng.below(PAGES) as u32;
            let len = match self.rng.below(10) {
                0..=5 => 1 + self.rng.below(64),
                6..=8 => 64 + self.rng.below(536),
                _ => 600 + self.rng.below(2400),
            } as usize;
            let picked = (!held.is_empty()).then(|| self.rng.below(held.len() as u64) as usize);
            match (self.rng.below(10), picked) {
                (0..=3, _) | (_, None) => {
                    let key = self.next_key;
                    self.next_key += 1;
                    held.push((key, page));
                    ops.push(Op::Put { page, key, len });
                }
                (4..=5, Some(at)) => {
                    let (key, page) = held[at];
                    ops.push(Op::Put { page, key, len });
                }
                (6..=7, Some(at)) => {
                    let (key, _) = held.swap_remove(at);
                    ops.push(Op::Delete { key });
                }
                (_, Some(at)) => {
                    if held[at].1 != page {
                        held[at].1 = page;
                        ops.push(Op::Move {
                            key: held[at].0,
                            to: page,
                        });
                    }
                }
            }
        }
        ops
    }

    /// One drawn step of the workload.
    pub(super) fn step(&mut self) -> Result<(), ValueStoreError> {
        self.steps += 1;
        match self.rng.below(100) {
            0..=49 => {
                let ops = self.draw_ops();
                let commit = self.rng.below(100) < 85;
                self.transact(&ops, commit).map(drop)
            }
            50..=59 => self.flush(),
            60..=66 => self.db().rotate(),
            67..=79 => {
                let cleanable = self.db().values.cleanable(0);
                match cleanable.first() {
                    Some(file) => self.clean(file.file).map(drop),
                    None => Ok(()),
                }
            }
            80..=87 => self.drop_empty().map(drop),
            88..=91 => self.checkpoint(),
            92..=96 => {
                if self.pins.len() < 3 {
                    self.pin();
                }
                Ok(())
            }
            _ => {
                if !self.pins.is_empty() {
                    let at = self.rng.below(self.pins.len() as u64) as usize;
                    self.unpin(self.pins[at]);
                }
                Ok(())
            }
        }
    }

    /// Runs drawn steps until `steps` of them have been taken in all.
    pub(super) fn run(&mut self, steps: u64) -> Result<(), ValueStoreError> {
        while self.steps < steps {
            self.step()?;
        }
        Ok(())
    }

    /// Checks the session against the record: every key of the latest
    /// version and of every pinned version reads back with its value, and
    /// the counts are what the cells give.
    #[track_caller]
    pub(super) fn check_session(&self) {
        let db = self.db.as_ref().expect("a session");
        let latest = db.pages.latest();
        assert_eq!(
            db.logical().expect("every value is read"),
            self.history[latest as usize],
            "the latest version, {latest}"
        );
        for &pinned in &self.pins {
            assert_eq!(
                db.logical_at(pinned)
                    .expect("every value of a pinned version is read"),
                self.history[pinned as usize],
                "the pinned version {pinned}"
            );
        }
        db.assert_counts_agree();
    }

    /// Ends the session, as a process that exits does.
    pub(super) fn end_session(&mut self) {
        self.db = None;
        self.pins.clear();
    }

    /// Moves to `left`, the file system a crash left. The next session is
    /// started by [`Self::open`].
    pub(super) fn crashed(&mut self, left: &SimVfs) {
        self.fs = left.clone();
        self.end_session();
    }

    /// Whether the session can go on after an operation failed. It cannot
    /// once the model's own journal has failed, or once the manifest is in
    /// doubt: both are settled by opening the database again.
    pub(super) fn can_go_on(&self) -> bool {
        self.db
            .as_ref()
            .is_some_and(|db| !db.pages.journal_failed() && db.manifest.manifest().is_ok())
    }

    /// Gives up the files that were being written when an operation failed:
    /// the next value starts a new file.
    pub(super) fn drop_writers(&mut self) {
        let db = self.db();
        db.writer = None;
        db.cleaner = None;
    }
}

/// The value-log files `manifest` names.
pub(super) fn named_values(manifest: &Manifest) -> Vec<LiveFile> {
    manifest
        .files()
        .iter()
        .filter(|file| file.kind() == FileKind::ValueLog)
        .copied()
        .collect()
}

/// Opens the database a crash or the end of a session left in `fs` and
/// checks it against the record of `run`:
///
/// - it opens, and every change the journal kept can be counted
/// - it is at one committed version, no older than the newest durable one
/// - every key of that version is there, in its page, and its value reads
///   back unchanged through the pointer its cell holds, in one read
/// - no cell holds a pointer older than one that was durable
/// - the counts are what the recovered cells give
/// - `val/` holds exactly the files the manifest names, and each of them is
///   whole values up to its valid length
pub(super) fn check_recovered(fs: &SimVfs, run: &Run) -> Db {
    let db = Db::open(fs).unwrap_or_else(|error| panic!("the database does not open: {error}"));
    let version = db.pages.latest();
    assert!(
        run.durable <= version && version <= run.attempted(),
        "recovered version {version}, with {} durable and {} attempted",
        run.durable,
        run.attempted()
    );
    assert_eq!(
        db.logical().expect("every recovered value is read"),
        run.history[version as usize],
        "version {version}"
    );
    if let Some(held) = run.pointers.get(&version) {
        for (page, cells) in db.pages.all() {
            for cell in cells {
                let seen = held.get(&cell.key);
                assert!(
                    seen.is_none_or(|seen| seen.contains(&cell.pointer)),
                    "the cell {} of {page} holds {:?}, and held {seen:?} since the journal was \
                     last made durable",
                    cell.key,
                    cell.pointer
                );
            }
        }
    }
    db.assert_counts_agree();
    let named = named_values(db.manifest.manifest().expect("not in doubt"));
    let ids: Vec<FileId> = named.iter().map(LiveFile::id).collect();
    assert_eq!(db.files_on_disk().into_iter().collect::<Vec<_>>(), ids);
    for file in named {
        let mut walk = db.values.walk(file.id()).expect("a named file is walked");
        while walk.next_value().expect("a whole value").is_some() {}
        assert_eq!(walk.offset(), file.valid_len(), "{file:?}");
    }
    db
}
