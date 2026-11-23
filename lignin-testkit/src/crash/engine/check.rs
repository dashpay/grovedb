//! One state that a crash left: opened as a node opens its database, and
//! held to the blocks of the model.

use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use grovedb_version::version::GroveVersion;
use lignin_format::PageSize;
use lignin_io::{sim::SimVfs, Vfs};

use super::{
    grove::{self, db_dir, NotOpened, Tuning, AGAIN_KEY},
    model::{Block, Checkpoint, Doing, Expect, History, Landing, Places, Standing, State},
};
use crate::crash::check::{Kind, Violation};

/// What a checkpoint's directory holds in a state that a crash left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// No directory.
    Gone,
    /// A directory that holds no database: a checkpoint that is being
    /// made or removed.
    Nothing,
    /// A directory that is marked as being removed.
    Marked,
    /// The whole checkpoint, which opens at its block.
    Whole,
}

/// A state that recovered to where it may be.
#[derive(Clone, Debug)]
pub struct Recovered {
    /// Where the database is.
    pub landing: Landing,
    /// The version of the engine it is at.
    pub version: u64,
    /// What it holds.
    pub state: State,
    /// What the directory of each checkpoint holds, in the order of
    /// [`Expect::checkpoints`].
    pub checkpoints: Vec<(PathBuf, Held)>,
}

/// What a check goes by, beside the state and what is expected of it.
#[derive(Clone, Copy, Debug)]
pub struct Rules<'a> {
    /// The blocks of the lineage.
    pub history: &'a History,
    /// The subtrees that the walks of the lineage have reached.
    pub places: &'a Places,
    /// The grove version of the run.
    pub grove_version: &'static GroveVersion,
    /// The size of the pages of the run's databases.
    pub page_size: PageSize,
    /// Where the checks add the value hashes that the engine's checker
    /// named as left by a delete (`grove::verify`), one for each check of
    /// a database that holds one. Only a run on a grove version that
    /// leaves such a hash adds any.
    pub left_hashes: &'a AtomicU64,
}

impl Rules<'_> {
    /// The two checks of the engine's checker on the database in `dir`:
    /// `db` beside its threads, which is then closed, and the store that
    /// the close left. Returns the version the database opens at again.
    fn verified(
        &self,
        db: grove::Grove,
        state: &SimVfs,
        dir: &std::path::Path,
        tuning: &Tuning,
    ) -> Result<u64, String> {
        let beside = grove::verify(&db, self.grove_version)?;
        drop(db);
        let (version, closed) =
            grove::verify_closed(vfs_of(state), dir, tuning, self.grove_version)?;
        self.left_hashes
            .fetch_add(beside + closed, Ordering::Relaxed);
        Ok(version)
    }
}

/// What a check found wrong.
#[derive(Clone, Debug)]
pub struct Wrong {
    /// The violation.
    pub violation: Violation,
    /// Whether it is that a checkpoint is not as the call left it that
    /// returned: one that was made holds no database, or one that was
    /// deleted is there.
    pub checkpoint: bool,
    /// The version that the database of the state opened at, where what
    /// was found is of the database, and not of a checkpoint of it.
    pub version: Option<u64>,
}

impl From<Violation> for Wrong {
    fn from(violation: Violation) -> Self {
        Self {
            violation,
            checkpoint: false,
            version: None,
        }
    }
}

/// The tuning a state is opened with to be looked at, by turns: the pool.
fn tuning_of(page_size: PageSize, turn: u64) -> Tuning {
    Tuning {
        cache_size: [2 << 20, 128 << 10, 512 << 10][(turn % 3) as usize],
        ..Tuning::at_rest(page_size)
    }
}

fn violation(kind: Kind, said: impl Into<String>) -> Violation {
    Violation::new(kind, said)
}

/// Whether a database at `landing` has the block of `point`: it is at that
/// block or after it.
fn has(landing: Landing, point: usize) -> bool {
    match landing {
        Landing::Point(at) => at >= point,
        // Inside the making of a block a database is after the block
        // before it.
        Landing::Inside(at) => at > point,
        Landing::Attempt | Landing::InsideAttempt => true,
    }
}

/// Recovers the state `state` and holds it to `rules` and `expect`:
///
/// 1. The database opens, as a node opens it: with the engine's open, which
///    makes a database where the directory holds none, and GroveDB on it.
/// 2. It is at a version that a block of the model has, at or after the
///    floor and not after the last block, or at the block that was being
///    made, and not below the version that was durable
///    ([`Expect::durable`]). A version inside a flush of the pending
///    drops, which commits more than once, is a place of its own.
/// 3. Its root hash is the block's, and so is every record that a walk
///    from the root tree reaches. Inside a flush of the pending drops it
///    holds what a copy of the files of the database that is never
///    crashed held at that version of its flush ([`Block::inside`]). No
///    other block has a version inside it.
/// 4. The engine's physical invariant checker finds it sound, beside the
///    threads of the open database and again after a close and a second
///    open, with the stored hashes held to Merk's rules for a check
///    ([`grove::verify`]). The second open is at the same version. In a
///    run on a grove version whose delete can leave a tree with a value
///    hash that is not the rules' (`grove::leaves_hashes`), that hash is
///    counted ([`Rules::left_hashes`]) and is no violation; in every other
///    run it is one.
/// 5. Each checkpoint that was made opens as a database of its own, at the
///    version and with the root hash and the records of its block, and is
///    sound to the checker; one whose creation or deletion a crash can have
///    cut short is whole in that way, or not there, or, for a deletion,
///    marked as being removed; one whose deletion returned `Ok` has no
///    directory; and one that the state the lineage began with had left
///    cut short is as that state had it. And the database has not lost
///    the block of a checkpoint that is whole.
///
/// The checkpoints are looked at before the database or after it, by
/// `turn`.
pub fn check_state(
    state: &SimVfs,
    rules: Rules<'_>,
    expect: &Expect,
    turn: u64,
) -> Result<Recovered, Wrong> {
    let tuning = tuning_of(rules.page_size, turn);
    let checkpoints_first = (turn / 3).is_multiple_of(2);
    let mut checkpoints = Vec::new();
    if checkpoints_first {
        checkpoints = check_checkpoints(state, rules, expect, &tuning)?;
    }
    let (landing, version, found) = check_database(state, rules, expect, &tuning)?;
    if !checkpoints_first {
        checkpoints = check_checkpoints(state, rules, expect, &tuning)?;
    }
    for (checkpoint, (_, held)) in expect.checkpoints.iter().zip(&checkpoints) {
        if *held == Held::Whole && !has(landing, checkpoint.point) {
            let said = format!(
                "the database is at {} and has lost the block of point {}, which the checkpoint \
                 in {} holds",
                landing.text(),
                checkpoint.point,
                checkpoint.dir.display()
            );
            return Err(violation(Kind::NotCommitted, said).into());
        }
    }
    Ok(Recovered {
        landing,
        version,
        state: found,
        checkpoints,
    })
}

/// How a state at `version` differs from what the database holds there,
/// inside the making of `block`. Only a flush of the pending drops makes a
/// version before its last.
fn inside(found: &State, block: &Block, version: u64) -> Option<String> {
    match block.inside.get((version - block.first) as usize) {
        Some(held) => found.difference(held),
        None => Some("the block is one commit, and no version is inside it".to_string()),
    }
}

/// How `found`, which a database holds that is at `version` and landed at
/// `landing`, is not what the model has there under `expect`: the state of
/// the block, or of the version inside the making of one.
///
/// A state that was recovered before and is left again by a later crash is
/// held to this again: the block that is expected at its version then may
/// be another.
pub fn not_the_block(
    rules: Rules<'_>,
    expect: &Expect,
    landing: Landing,
    version: u64,
    found: &State,
) -> Option<String> {
    let history = rules.history;
    let attempt = || expect.attempt.as_ref().expect("a landing at the attempt");
    let difference = match landing {
        Landing::Point(at) => found.difference(&history.block(at).state),
        Landing::Attempt => found.difference(&attempt().state),
        Landing::Inside(at) => inside(found, history.block(at), version),
        Landing::InsideAttempt => inside(found, attempt(), version),
    }?;
    Some(format!(
        "the database is at {} (version {version}) and does not hold what that block published: \
         {difference}",
        landing.text()
    ))
}

/// How the state with no unsynced change of an image, which is at version
/// `durable`, is too far behind the state with every change, which is at
/// version `reached`, under `expect`.
///
/// A flush of the pending drops makes its commits one after another. In a
/// session whose commits sync the log ([`Expect::syncs`]), each is durable
/// when it has returned, and the next is appended only then. So where the
/// files hold a commit of the flush, the commit before it is durable, and
/// the state with no unsynced change is one version behind the state with
/// every change at most.
pub fn behind_in_a_flush(expect: &Expect, durable: u64, reached: u64) -> Option<String> {
    let attempt = expect.attempt.as_ref()?;
    let inside = expect.syncs && expect.doing == Doing::DropsFlushed && reached >= attempt.first;
    (inside && durable + 1 < reached).then(|| {
        format!(
            "the state with no unsynced change is at version {durable} and the state with every \
             change at version {reached}, inside a flush of the pending drops in a session \
             whose commits sync the log: the commit of version {} had returned",
            reached - 1
        )
    })
}

fn vfs_of(state: &SimVfs) -> Arc<dyn Vfs> {
    Arc::new(state.clone())
}

fn check_database(
    state: &SimVfs,
    rules: Rules<'_>,
    expect: &Expect,
    tuning: &Tuning,
) -> Result<(Landing, u64, State), Wrong> {
    let history = rules.history;
    let db = grove::open(vfs_of(state), db_dir(), tuning, true)
        .map_err(|error| violation(Kind::NotRecovered, error.to_string()))?;
    let version = grove::version_of(&db);
    // What is found from here on is of a database at this version.
    let wrong = |kind: Kind, said: String| Wrong {
        version: Some(version),
        ..Wrong::from(violation(kind, said))
    };
    let landing = (history.find(expect, version)).map_err(|said| {
        wrong(
            Kind::NotCommitted,
            format!("the database opened, and {said}"),
        )
    })?;
    let (found, _) = grove::read_committed(&db, rules.grove_version, rules.places)
        .map_err(|error| wrong(Kind::Inconsistent, error))?;
    if let Some(said) = not_the_block(rules, expect, landing, version, &found) {
        return Err(wrong(Kind::NotCommitted, said));
    }
    let again = (rules.verified(db, state, db_dir(), tuning))
        .map_err(|error| wrong(Kind::Inconsistent, error))?;
    if again != version {
        let said = format!(
            "the database opened at version {version}, was closed, and opened at version {again}"
        );
        return Err(wrong(Kind::NotTheSame, said));
    }
    Ok((landing, version, found))
}

fn check_checkpoints(
    state: &SimVfs,
    rules: Rules<'_>,
    expect: &Expect,
    tuning: &Tuning,
) -> Result<Vec<(PathBuf, Held)>, Wrong> {
    let mut found = Vec::new();
    for checkpoint in &expect.checkpoints {
        let held = check_checkpoint(state, rules, checkpoint, tuning)?;
        found.push((checkpoint.dir.clone(), held));
    }
    Ok(found)
}

fn check_checkpoint(
    state: &SimVfs,
    rules: Rules<'_>,
    checkpoint: &Checkpoint,
    tuning: &Tuning,
) -> Result<Held, Wrong> {
    let dir = &checkpoint.dir;
    let wrong = |kind: Kind, said: String| {
        let said = format!(
            "the checkpoint in {} of point {}: {said}",
            dir.display(),
            checkpoint.point
        );
        Wrong::from(violation(kind, said))
    };
    let gone = match state.read_dir(dir) {
        Ok(_) => false,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(wrong(Kind::NotRecovered, format!("its directory: {error}"))),
    };
    if checkpoint.standing == Standing::Deleted {
        // A removal that returned is durable: the directory is gone.
        return match gone {
            true => Ok(Held::Gone),
            false => Err(Wrong {
                checkpoint: true,
                ..wrong(
                    Kind::NotCommitted,
                    "its deletion had returned, and its directory is there".to_string(),
                )
            }),
        };
    }
    let nothing = match gone {
        true => Held::Gone,
        false => Held::Nothing,
    };
    let opened = match grove::open(vfs_of(state), dir, tuning, false) {
        Ok(db) => Ok(db),
        Err(NotOpened::NoDatabase) => Err(nothing),
        Err(NotOpened::Deleting) => Err(Held::Marked),
        Err(error) => {
            let said = format!("{error}, and it is {:?}", checkpoint.standing);
            return Err(wrong(Kind::NotRecovered, said));
        }
    };
    // What a crash left of a checkpoint that it cut short stays as it
    // was: the block of its point need not be one of this lineage, and is
    // not looked at.
    if let Standing::Left(left) = checkpoint.standing {
        let held = match &opened {
            Ok(_) => Held::Whole,
            Err(held) => *held,
        };
        return match held == left {
            true => Ok(held),
            false => {
                let said = format!(
                    "it is {held:?}, and the state that the lineage began with had left it \
                     {left:?}"
                );
                Err(wrong(Kind::NotCommitted, said))
            }
        };
    }
    let db = match (opened, checkpoint.standing) {
        (Ok(db), _) => db,
        (Err(Held::Marked), Standing::Deleting) => return Ok(Held::Marked),
        (Err(held @ (Held::Nothing | Held::Gone)), Standing::Building | Standing::Deleting) => {
            return Ok(held);
        }
        (Err(held), standing) => {
            // A checkpoint that was made and holds no database is not as
            // the call left it that returned.
            let lost = standing == Standing::Made && held != Held::Marked;
            let said = match held {
                Held::Marked => NotOpened::Deleting.to_string(),
                _ => NotOpened::NoDatabase.to_string(),
            };
            return Err(Wrong {
                checkpoint: lost,
                ..wrong(
                    Kind::NotRecovered,
                    format!("{said}, and it is {standing:?}"),
                )
            });
        }
    };
    let block = rules.history.block(checkpoint.point);
    let version = grove::version_of(&db);
    if version != block.version {
        let said = format!(
            "it opened at version {version}, and its block is of version {}",
            block.version
        );
        return Err(wrong(Kind::NotCommitted, said));
    }
    let (found, _) = grove::read_committed(&db, rules.grove_version, rules.places)
        .map_err(|error| wrong(Kind::Inconsistent, error))?;
    if let Some(difference) = found.difference(&block.state) {
        return Err(wrong(Kind::NotCommitted, difference));
    }
    let again = (rules.verified(db, state, dir, tuning))
        .map_err(|error| wrong(Kind::Inconsistent, error))?;
    if again != version {
        let said = format!("it was closed and opened at version {again}");
        return Err(wrong(Kind::NotTheSame, said));
    }
    Ok(Held::Whole)
}

/// A database that was recovered goes on: it is opened with the tuning of
/// a session whose threads are eager, a block is committed to it and made
/// durable, and it is closed. Then it opens at the version after the one
/// it recovered to, with the root hash that the block gave it, and is
/// sound to the checker. The same is done to every checkpoint that is
/// whole, before the database or after it, and neither changes the other:
/// each of them is found, when its turn comes, at the version and with
/// the records it was recovered with, whatever was done to the others
/// before, and each takes a block of its own that no other has.
///
/// `expect` and `recovered` are what [`check_state`] held the state to and
/// what it found. `state` is changed by this: it is a copy that the caller
/// keeps for nothing else.
pub fn check_again(
    state: &SimVfs,
    rules: Rules<'_>,
    expect: &Expect,
    recovered: &Recovered,
    turn: u64,
) -> Result<(), Violation> {
    // Each directory, with the version and the state it was recovered at:
    // the database as it was found, a checkpoint at its block.
    let mut dirs: Vec<(PathBuf, u64, &State)> = Vec::new();
    dirs.push((db_dir().to_path_buf(), recovered.version, &recovered.state));
    for (checkpoint, (dir, held)) in expect.checkpoints.iter().zip(&recovered.checkpoints) {
        if *held == Held::Whole {
            let block = rules.history.block(checkpoint.point);
            dirs.push((dir.clone(), block.version, &block.state));
        }
    }
    if turn.is_multiple_of(2) {
        dirs.reverse();
    }
    let eager = Tuning::of_turn(rules.page_size, turn);
    let version = rules.grove_version;
    let mut left = Vec::new();
    for (place, (dir, at, held)) in dirs.iter().enumerate() {
        let wrong = |said: String| {
            let said = format!("going on in {}: {said}", dir.display());
            violation(Kind::NotTheSame, said)
        };
        // The database is opened as a node opens it, which makes one where
        // a crash left none. A checkpoint is opened where it is.
        let db = grove::open(vfs_of(state), dir, &eager, dir == db_dir())
            .map_err(|error| wrong(error.to_string()))?;
        let before = grove::version_of(&db);
        if before != *at {
            return Err(wrong(format!(
                "it opens at version {before}, and it was recovered at version {at}"
            )));
        }
        let (found, _) = grove::read_committed(&db, version, rules.places).map_err(wrong)?;
        if let Some(difference) = found.difference(held) {
            return Err(wrong(format!(
                "it does not hold what it was recovered with: {difference}"
            )));
        }
        // A value of its own, so that the block of one is not the block of
        // another that was recovered with the same.
        let value = turn.wrapping_mul(8).wrapping_add(place as u64);
        let block = db.start_transaction();
        grove::put_item(&db, AGAIN_KEY, value, Some(&block), version).map_err(wrong)?;
        let root = grove::root_of(&db, &block, version).map_err(wrong)?;
        (db.commit_transaction(block).unwrap())
            .map_err(|error| wrong(format!("the commit: {error}")))?;
        db.flush()
            .map_err(|error| wrong(format!("the flush: {error}")))?;
        drop(db);
        left.push((dir.clone(), before + 1, root));
    }
    // Every one of them is where its own block left it, whatever was done
    // to the others since.
    let at_rest = tuning_of(rules.page_size, turn);
    for (dir, expected, root) in &left {
        let wrong = |said: String| {
            let said = format!("after going on in {}: {said}", dir.display());
            violation(Kind::NotTheSame, said)
        };
        let db = grove::open(vfs_of(state), dir, &at_rest, false)
            .map_err(|error| wrong(error.to_string()))?;
        let found = grove::version_of(&db);
        if found != *expected {
            return Err(wrong(format!(
                "it is at version {found}, and the block it took made version {expected}"
            )));
        }
        let snapshot = db.start_snapshot_read_transaction();
        let read = grove::root_of(&db, &snapshot, version).map_err(wrong)?;
        drop(snapshot);
        if read != *root {
            return Err(wrong(format!(
                "its root hash is {} and the block it took gave it {}",
                hex::encode(read),
                hex::encode(root)
            )));
        }
        rules.verified(db, state, dir, &at_rest).map_err(wrong)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use grovedb::{BackwardsReferences, Element};
    use lignin_io::sim::SimConfig;

    use super::*;
    use crate::{
        crash::engine::{
            campaign::flushed_on_a_copy,
            grove::{checkpoint_dir, Grove, CHECKPOINTS},
            model::{Block, Doing, Made, Position},
            observe::Watcher,
        },
        diff::state::{Keyspace, Listing, Record},
    };

    const TREES: [&[u8]; 2] = [b"a", b"b"];

    fn block(db: &Grove, first: u64, made: Made, places: &mut Places) -> Arc<Block> {
        let version = GroveVersion::latest();
        let (state, reached) = grove::read_committed(db, version, places).expect("read");
        places.extend(reached);
        Arc::new(Block {
            first,
            version: grove::version_of(db),
            state,
            resume: Position { stream: 0, op: 0 },
            made,
            inside: Vec::new(),
        })
    }

    /// Two trees with a row each, as one block, and the drop of both, as
    /// the next: the blocks of points 1 and 2.
    fn two_drops(db: &Grove, history: &mut History, places: &mut Places) {
        let version = GroveVersion::latest();
        let root: &[&[u8]] = &[];
        let made = db.start_transaction();
        for tree in TREES {
            let inserted = db.insert(
                root,
                tree,
                Element::empty_tree(),
                None,
                Some(&made),
                version,
            );
            inserted.unwrap().expect("a tree");
            let row = Element::new_item(vec![7; 40]);
            let inserted = db.insert([tree].as_ref(), b"row", row, None, Some(&made), version);
            inserted.unwrap().expect("a row");
        }
        let first = grove::version_of(db) + 1;
        db.commit_transaction(made).unwrap().expect("committed");
        history.push(block(db, first, Made::Commit, places));

        let dropped = db.start_transaction();
        for tree in TREES {
            let unchecked = BackwardsReferences::DontCheck;
            let drop = db.drop_flat_subtree(root, tree, unchecked, Some(&dropped), version);
            drop.unwrap().expect("a drop");
        }
        let first = grove::version_of(db) + 1;
        db.commit_transaction(dropped).unwrap().expect("committed");
        history.push(block(db, first, Made::Commit, places));
    }

    /// The checks of a run on a grove version whose delete can leave a
    /// tree with a value hash that is not the rules' count that hash, once
    /// for the open database and once for the closed one, and find the
    /// database sound. The checks of a run on a later version find the
    /// same database inconsistent, and count nothing.
    #[test]
    fn a_hash_that_a_delete_left_is_counted_by_the_checks_of_its_version() {
        use grovedb::operations::delete::DeleteOptions;
        use grovedb_version::version::v3::GROVE_V3;

        let page_size = PageSize::MIN;
        let tuning = Tuning::at_rest(page_size);
        let fs = SimVfs::new(SimConfig::new(11));
        let db = grove::open(vfs_of(&fs), db_dir(), &tuning, true).expect("made");
        let mut places = Places::new();
        let history = History::new((*block(&db, 1, Made::Open, &mut places)).clone());
        let root: &[&[u8]] = &[];
        let insert = |at: &[&[u8]], key: &[u8], element: Element| {
            let inserted = db.insert(at, key, element, None, None, &GROVE_V3);
            inserted.unwrap().expect("a write of the grove");
        };
        insert(root, b"parent", Element::empty_provable_count_tree());
        insert(&[b"parent"], b"beside", Element::new_item(vec![1]));
        insert(&[b"parent"], b"child", Element::empty_tree());
        insert(&[b"parent", b"child"], b"leaf", Element::new_item(vec![2]));
        let with_what_it_holds = DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: false,
            ..Default::default()
        };
        let deleted = db.delete(
            &[b"parent"],
            b"child",
            Some(with_what_it_holds),
            None,
            &GROVE_V3,
        );
        deleted.unwrap().expect("the delete");
        db.flush().expect("flushed");
        let at = grove::version_of(&db);
        drop(db);

        let left_hashes = AtomicU64::new(0);
        let rules = |grove_version: &'static GroveVersion| Rules {
            history: &history,
            places: &places,
            grove_version,
            page_size,
            left_hashes: &left_hashes,
        };
        let open = || grove::open(vfs_of(&fs), db_dir(), &tuning, false).expect("opens");
        let of_its_version = rules(&GROVE_V3).verified(open(), &fs, db_dir(), &tuning);
        assert_eq!(of_its_version, Ok(at));
        assert_eq!(left_hashes.load(Ordering::Relaxed), 2);

        let of_a_later = rules(GroveVersion::latest()).verified(open(), &fs, db_dir(), &tuning);
        let said = of_a_later.expect_err("a hash that differs");
        assert!(
            said.contains("a value hash is the hash of its value"),
            "{said}"
        );
        assert_eq!(left_hashes.load(Ordering::Relaxed), 2);
    }

    /// A flush of the pending drops commits more than once, and a crash
    /// between two of its commits leaves the records of the drops the
    /// flush had not come to, which the block after the flush does not
    /// hold. Such a state is held to what the flush left at its version on
    /// a copy of the files, and to that also once the history of its
    /// lineage has gone on: the states are checked after the flush has
    /// become a block of the history, with another block after it.
    #[test]
    fn a_state_inside_a_flush_of_drops_is_held_to_what_the_flush_left_at_its_version() {
        let version = GroveVersion::latest();
        let page_size = PageSize::MIN;
        let tuning = Tuning::at_rest(page_size);
        let fs = SimVfs::new(SimConfig::new(7));
        let watcher = Watcher::new(fs, Expect::exactly(0), None);
        let db = grove::open(watcher.vfs(), db_dir(), &tuning, true).expect("made");
        let mut places = Places::new();
        let mut history = History::new((*block(&db, 1, Made::Open, &mut places)).clone());
        two_drops(&db, &mut history, &mut places);
        let before = grove::version_of(&db);
        let holds_drops = |state: &State| {
            let of_a_drop = |record: &&Record| record.keyspace == Keyspace::DropRecord;
            state.records.iter().filter(of_a_drop).count()
        };
        assert_eq!(holds_drops(&history.last().state), 2);

        // What the flush leaves after each of its commits is said by a
        // copy of the files that is flushed first, and so is how many
        // commits it makes: its versions follow one another from the one
        // after the last block, the records of the drops go one by one,
        // and a version inside it has the record of one drop.
        let mut on_the_copy =
            flushed_on_a_copy(watcher.fs(), &tuning, version, &places).expect("flushed");
        let end = before + on_the_copy.len() as u64;
        assert!(on_the_copy.keys().copied().eq(before + 1..=end));
        let state = on_the_copy.remove(&end).expect("the last commit");
        let after = Arc::new(Block {
            first: before + 1,
            version: end,
            state,
            resume: Position { stream: 0, op: 0 },
            made: Made::DropsFlushed,
            inside: on_the_copy.into_values().collect(),
        });
        // The records of drops that the database holds at each version
        // from the block before the flush to the end of the flush.
        let held: Vec<usize> = std::iter::once(&history.last().state)
            .chain(&after.inside)
            .chain([&after.state])
            .map(holds_drops)
            .collect();
        assert!(held.windows(2).all(|pair| pair[0] >= pair[1]), "{held:?}");
        assert_eq!((held[0], held[held.len() - 1]), (2, 0), "{held:?}");
        let one_left = held[..held.len() - 1].iter().position(|held| *held == 1);
        let one_left = one_left.expect("a version inside the flush with one record") as u64;
        let with_one = before + one_left;
        let after_one = after.inside[one_left as usize - 1].clone();
        assert_eq!(
            grove::version_of(&db),
            before,
            "the copy is another file system"
        );

        // From here on an image is taken with what the flush may leave,
        // whichever thread's change makes it: those before are let go.
        watcher.expecting(|expect| {
            expect.floor = 0;
            expect.latest = 2;
            expect.attempt = Some(after.clone());
            expect.doing = Doing::DropsFlushed;
        });
        watcher.drain();
        let flushed = db.flush_pending_prefix_drops(version).expect("flushed");
        assert_eq!(flushed.reclaimed_records, 2);
        assert_eq!(grove::version_of(&db), after.version);
        let taken = watcher.drain();
        history.push(after.clone());
        watcher.expecting(|expect| *expect = Expect::exactly(3));
        grove::put_item(&db, AGAIN_KEY, 1, None, version).expect("written");
        history.push(block(&db, after.version + 1, Made::Beside, &mut places));

        let left_hashes = AtomicU64::new(0);
        let rules = Rules {
            history: &history,
            places: &places,
            grove_version: version,
            page_size,
            left_hashes: &left_hashes,
        };
        // Where the states of the flush landed, with how many records of
        // drops each holds.
        let mut met = Vec::new();
        for (turn, taken) in taken.iter().enumerate() {
            // What a process that is killed then leaves: every change.
            let plan = taken.image.keep_all();
            let state = taken.image.recover(&plan).expect("a plan of the image");
            let recovered = check_state(&state, rules, &taken.expect, turn as u64)
                .unwrap_or_else(|wrong| panic!("boundary {}: {}", taken.boundary, wrong.violation));
            met.push((
                recovered.landing,
                recovered.version,
                holds_drops(&recovered.state),
            ));
        }
        // The commits of the flush are appends to the log, and each is a
        // write boundary: a crash after one of them leaves the records
        // of the drops that the copy held at that version, and after the
        // last none.
        let landed = |at: usize| match before + at as u64 {
            version if version == before => (Landing::Point(2), version, held[at]),
            version if version == end => (Landing::Attempt, version, held[at]),
            version => (Landing::InsideAttempt, version, held[at]),
        };
        let allowed: Vec<_> = (0..held.len()).map(landed).collect();
        for inside_or_at_its_end in &allowed[1..] {
            assert!(met.contains(inside_or_at_its_end), "{met:?}");
        }
        assert!(met.iter().all(|met| allowed.contains(met)), "{met:?}");

        // At the first version of the flush with the record of one drop
        // the database holds what the copy held then, and neither the
        // records of both drops, which is what it held before the flush,
        // nor none, which is what the flush leaves: each of the two is
        // sound, and not that version of the flush.
        let flushed = history.block(3);
        let last = &history.block(2).state;
        assert_eq!(inside(&after_one, flushed, with_one), None);
        let both_left = inside(last, flushed, with_one);
        assert!(both_left.expect("too many").contains("differ"));
        let none_left = inside(&after.state, flushed, with_one);
        assert!(none_left.expect("too few").contains("differ"));
        // The same, of a state that was recovered before and is left again.
        let expect = Expect::exactly(3);
        let landing = Landing::Inside(3);
        let again = |state: &State| not_the_block(rules, &expect, landing, with_one, state);
        assert_eq!(again(&after_one), None);
        let said = again(last).expect("too many");
        assert!(said.contains("inside the making of point 3"), "{said}");
        // A block that is one commit has no version inside it.
        let wipe = Block {
            made: Made::Wipe,
            inside: Vec::new(),
            ..(**flushed).clone()
        };
        let said = inside(&after_one, &wipe, with_one);
        assert!(said.expect("no such version").contains("is one commit"));
        drop(db);
    }

    /// A checkpoint is held to what its standing asks: one that was made
    /// is whole, at the version and with the records of its block; one
    /// whose creation or deletion a crash can have cut short may be
    /// missing; one whose deletion returned has no directory. And a
    /// database that goes on is found as it was recovered, with each of
    /// its checkpoints.
    #[test]
    fn a_checkpoint_is_held_to_what_its_standing_asks() {
        let version = GroveVersion::latest();
        let page_size = PageSize::MIN;
        let tuning = Tuning::at_rest(page_size);
        let fs = SimVfs::new(SimConfig::new(11));
        fs.create_dir(Path::new(CHECKPOINTS)).expect("made");
        fs.sync_dir(Path::new("/")).expect("synced");
        let db = grove::open(vfs_of(&fs), db_dir(), &tuning, true).expect("made");
        let mut places = Places::new();
        let mut history = History::new((*block(&db, 1, Made::Open, &mut places)).clone());
        // Point 1 is an item, and the checkpoint is made of it. Point 2
        // is the item with another value.
        let made = checkpoint_dir(0);
        for value in [1, 2] {
            grove::put_item(&db, AGAIN_KEY, value, None, version).expect("written");
            let at = grove::version_of(&db);
            history.push(block(&db, at, Made::Beside, &mut places));
            if value == 1 {
                grove::create_checkpoint(&db, &made).expect("made");
            }
        }
        db.flush().expect("flushed");
        drop(db);
        // Every check is of a copy of the files: a check opens them.
        let image = fs.crash_image();
        let state = || {
            image
                .recover(&image.keep_all())
                .expect("a plan of the image")
        };

        let left_hashes = AtomicU64::new(0);
        let rules = Rules {
            history: &history,
            places: &places,
            grove_version: version,
            page_size,
            left_hashes: &left_hashes,
        };
        let expecting = |dir: &Path, point: usize, standing: Standing| Expect {
            checkpoints: vec![Checkpoint {
                dir: dir.to_path_buf(),
                point,
                standing,
            }],
            ..Expect::exactly(2)
        };
        let held = |rules: Rules<'_>, expect: &Expect| {
            let recovered = check_state(&state(), rules, expect, 0)?;
            assert_eq!(recovered.landing, Landing::Point(2));
            Ok::<Held, Wrong>(recovered.checkpoints[0].1)
        };
        let refused = |rules: Rules<'_>, expect: &Expect| {
            let wrong = held(rules, expect).expect_err("the checkpoint is not as it may be");
            let said = wrong.violation.to_string();
            (wrong.violation.kind, wrong.checkpoint, said)
        };

        // The checkpoint that was made, at each standing.
        for standing in [Standing::Made, Standing::Building, Standing::Deleting] {
            let found = held(rules, &expecting(&made, 1, standing));
            assert_eq!(found.expect("whole"), Held::Whole, "{standing:?}");
        }
        let (kind, of_a_call, said) = refused(rules, &expecting(&made, 1, Standing::Deleted));
        assert_eq!((kind, of_a_call), (Kind::NotCommitted, true), "{said}");
        assert!(said.contains("its deletion had returned"), "{said}");
        // It is of point 1, and of no other point.
        let (kind, of_a_call, said) = refused(rules, &expecting(&made, 2, Standing::Made));
        assert_eq!((kind, of_a_call), (Kind::NotCommitted, false), "{said}");
        assert!(said.contains("it opened at version"), "{said}");
        // It holds what point 1 published: a model whose point 1 has the
        // version of it and holds something else refuses it.
        let other = Block {
            state: history.block(2).state.clone(),
            ..(**history.block(1)).clone()
        };
        let mut another = history.up_to(0);
        another.push(Arc::new(other));
        another.push(history.block(2).clone());
        let other_rules = Rules {
            history: &another,
            ..rules
        };
        let (kind, of_a_call, said) = refused(other_rules, &expecting(&made, 1, Standing::Made));
        assert_eq!((kind, of_a_call), (Kind::NotCommitted, false), "{said}");
        assert!(said.contains("the root hash is"), "{said}");

        // A directory that is not there, at each standing.
        let missing = checkpoint_dir(7);
        let (kind, of_a_call, said) = refused(rules, &expecting(&missing, 1, Standing::Made));
        assert_eq!((kind, of_a_call), (Kind::NotRecovered, true), "{said}");
        assert!(said.contains("and it is Made"), "{said}");
        let gone = [
            Standing::Building,
            Standing::Deleting,
            Standing::Deleted,
            Standing::Left(Held::Gone),
        ];
        for standing in gone {
            let found = held(rules, &expecting(&missing, 1, standing));
            assert_eq!(found.expect("gone"), Held::Gone, "{standing:?}");
        }

        // What a crash had left of a checkpoint that it cut short, in the
        // state a lineage began with, stays what it was: no database is
        // there later where none was, and no directory where none was. Its
        // point need be no block of the lineage.
        for (dir, left) in [(&made, Held::Nothing), (&missing, Held::Nothing)] {
            let expect = expecting(dir, 99, Standing::Left(left));
            let (kind, of_a_call, said) = refused(rules, &expect);
            assert_eq!((kind, of_a_call), (Kind::NotCommitted, false), "{said}");
            assert!(said.contains("had left it Nothing"), "{said}");
        }

        // The database and its checkpoint go on, each from what it was
        // recovered with.
        let expect = expecting(&made, 1, Standing::Made);
        let recovered = check_state(&state(), rules, &expect, 0).expect("recovered");
        for turn in [0, 1] {
            check_again(&state(), rules, &expect, &recovered, turn).expect("it goes on");
        }
        // A database that is found at another version than it was
        // recovered at, or with other records, has not gone on from there:
        // nor has a checkpoint that holds other records than its block.
        let earlier = Recovered {
            version: recovered.version - 1,
            ..recovered.clone()
        };
        let said = check_again(&state(), rules, &expect, &earlier, 1).expect_err("another version");
        assert_eq!(said.kind, Kind::NotTheSame);
        assert!(
            said.to_string()
                .contains("going on in /db: it opens at version"),
            "{said}"
        );
        let with_other = Recovered {
            state: history.block(1).state.clone(),
            ..recovered.clone()
        };
        let said =
            check_again(&state(), rules, &expect, &with_other, 1).expect_err("other records");
        assert!(
            said.to_string()
                .contains("does not hold what it was recovered with"),
            "{said}"
        );
        let said = check_again(&state(), other_rules, &expect, &recovered, 0);
        let said = said
            .expect_err("a checkpoint with other records")
            .to_string();
        assert!(said.contains("going on in /checkpoints/0"), "{said}");
    }

    /// Inside a flush of the pending drops in a session whose commits sync
    /// the log, the commit before the one that the files hold last has
    /// returned, and is durable.
    #[test]
    fn inside_a_flush_that_syncs_the_durable_state_is_one_commit_behind_at_most() {
        // A flush that makes versions 9 to 11, of a database at version 8.
        let flush = Block {
            first: 9,
            version: 11,
            state: State::new([1; 32], Listing::default(), false),
            resume: Position { stream: 0, op: 0 },
            made: Made::DropsFlushed,
            inside: Vec::new(),
        };
        let expect = Expect {
            attempt: Some(Arc::new(flush)),
            doing: Doing::DropsFlushed,
            syncs: true,
            ..Expect::exactly(0)
        };
        // The files hold nothing of the flush, or its first commit: the
        // database before it is what was durable.
        assert_eq!(behind_in_a_flush(&expect, 8, 8), None);
        assert_eq!(behind_in_a_flush(&expect, 8, 9), None);
        // They hold its second commit, so the first had returned.
        let said = behind_in_a_flush(&expect, 8, 10).expect("the first commit is lost");
        assert!(
            said.contains("the commit of version 9 had returned"),
            "{said}"
        );
        assert_eq!(behind_in_a_flush(&expect, 9, 10), None);
        assert_eq!(behind_in_a_flush(&expect, 10, 10), None);
        assert!(behind_in_a_flush(&expect, 9, 11).is_some());
        assert_eq!(behind_in_a_flush(&expect, 10, 11), None);
        // A session whose commits do not sync the log promises nothing of
        // the kind, nor does another operation, nor one with no attempt.
        let lazy = Expect {
            syncs: false,
            ..expect.clone()
        };
        assert_eq!(behind_in_a_flush(&lazy, 8, 11), None);
        let commit = Expect {
            doing: Doing::Commit,
            ..expect.clone()
        };
        assert_eq!(behind_in_a_flush(&commit, 8, 11), None);
        let idle = Expect {
            attempt: None,
            ..expect.clone()
        };
        assert_eq!(behind_in_a_flush(&idle, 8, 11), None);
    }
}
