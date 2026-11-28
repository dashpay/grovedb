//! What a database of the campaign holds after each commit, and where a
//! crash may leave it.
//!
//! The model knows no page, file or thread. It is a list of **blocks**, in
//! the order of their commits: what each published, as a reader of GroveDB
//! finds it. Where the list comes from is said in [`super::grove`]: a
//! second database that is never crashed.

use std::{collections::BTreeSet, fmt, path::PathBuf, sync::Arc};

use super::check::Held;
use crate::diff::state::{Keyspace, Listing, Place, Record};

/// A hash of GroveDB.
pub type Hash = [u8; 32];

/// What a database holds at one version: the root hash of its grove, and
/// every record a walk from the root tree reaches
/// ([`stored`](crate::diff::state::stored)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    /// The root hash of the grove.
    pub root: Hash,
    /// The records, in the order of the walk.
    pub records: Arc<Vec<Record>>,
    /// A hash of the records: two states with one digest hold the same.
    pub digest: Hash,
    /// Whether the database says of itself that a restore did not finish
    /// (`GroveDb::has_incomplete_restore`).
    pub incomplete_restore: bool,
}

impl State {
    /// The state with `root` and what `listing` lists.
    ///
    /// The drops that are pending are listed twice by a walk: as the
    /// records a transaction sees, and as the engine reports them, which
    /// are the committed ones. The second list is left out here: of a
    /// committed state it says what the first says, and a state that is
    /// read through the transaction that is about to commit it has there
    /// the list of the commit before.
    pub fn new(root: Hash, listing: Listing, incomplete_restore: bool) -> Self {
        let records: Vec<Record> = (listing.records.into_iter())
            .filter(|record| record.keyspace != Keyspace::PendingDrop)
            .collect();
        let digest = digest_of(&records);
        Self {
            root,
            records: Arc::new(records),
            digest,
            incomplete_restore,
        }
    }

    /// How this state differs from `expected`, in a line: nothing if it
    /// does not.
    pub fn difference(&self, expected: &Self) -> Option<String> {
        if self.root != expected.root {
            return Some(format!(
                "the root hash is {} and not {}",
                hex::encode(self.root),
                hex::encode(expected.root)
            ));
        }
        if self.incomplete_restore != expected.incomplete_restore {
            return Some(format!(
                "the mark of a restore that did not finish is {} and not {}",
                self.incomplete_restore, expected.incomplete_restore
            ));
        }
        if self.digest == expected.digest {
            return None;
        }
        let (found, wanted) = (&self.records, &expected.records);
        let at = (found.iter().zip(wanted.iter())).position(|(found, wanted)| found != wanted);
        let at = at.unwrap_or(found.len().min(wanted.len()));
        let text = |records: &[Record]| match records.get(at) {
            Some(record) => record.text(),
            None => "nothing".to_string(),
        };
        Some(format!(
            "the stored records differ at record {at} of {} (the block has {}): found {}, the \
             block has {}",
            found.len(),
            wanted.len(),
            text(found),
            text(wanted)
        ))
    }
}

fn digest_of(records: &[Record]) -> Hash {
    let mut hasher = blake3::Hasher::new();
    let mut bytes = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    for record in records {
        bytes(&[record.path.len() as u8, record.left_behind as u8]);
        for segment in &record.path {
            bytes(segment);
        }
        bytes(&[
            record.secondary.is_some() as u8,
            record.secondary.unwrap_or(0),
        ]);
        bytes(&[record.keyspace as u8]);
        bytes(&record.key);
        bytes(&record.value);
    }
    *hasher.finalize().as_bytes()
}

/// Where the stream of a lineage goes on: the stream, and the operation of
/// it that comes next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    /// The stream of the run's plan.
    pub stream: usize,
    /// The next operation of that stream.
    pub op: usize,
}

/// What made a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Made {
    /// The open that made the database, or the state a lineage began with.
    Open,
    /// The commit of a write transaction: a block of the stream.
    Commit,
    /// A write with no transaction, beside the open one.
    Beside,
    /// `GroveDb::flush_pending_prefix_drops`, which commits by itself, and
    /// more than once.
    DropsFlushed,
    /// `GroveDb::wipe`.
    Wipe,
    /// A state that a crash left inside an operation that commits more
    /// than once, which a lineage began with.
    Inside,
}

/// One block: what an operation that commits published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// The first version of the engine that the operation made. It is
    /// [`Self::version`] for every operation that is one commit.
    pub first: u64,
    /// The version of the engine at which the database holds
    /// [`Self::state`]: the last one the operation made.
    pub version: u64,
    /// What the database holds from that version on.
    pub state: State,
    /// Where the stream goes on once this block is the last one.
    pub resume: Position,
    /// What made it.
    pub made: Made,
    /// What the database holds at each version before the last that the
    /// operation made, from [`Self::first`] on: empty for an operation
    /// that is one commit. A flush of the pending drops has them, from a
    /// copy of the files of the database that is never crashed, which was
    /// flushed first.
    pub inside: Vec<State>,
}

/// The blocks of one database, in the order of their commits. The number
/// of a block in it is its **point**.
#[derive(Clone, Debug, Default)]
pub struct History {
    blocks: Vec<Arc<Block>>,
}

impl History {
    /// A history that begins with `first`.
    pub fn new(first: Block) -> Self {
        Self {
            blocks: vec![Arc::new(first)],
        }
    }

    /// Adds a block, whose versions follow those of the last.
    pub fn push(&mut self, block: Arc<Block>) {
        if let Some(last) = self.blocks.last() {
            assert!(
                last.version < block.first && block.first <= block.version,
                "a block of versions {} to {} after one of version {}",
                block.first,
                block.version,
                last.version
            );
        }
        self.blocks.push(block);
    }

    /// The point of the last block.
    pub fn latest(&self) -> usize {
        self.blocks.len() - 1
    }

    /// The last block.
    pub fn last(&self) -> &Arc<Block> {
        &self.blocks[self.latest()]
    }

    /// The block at `point`.
    pub fn block(&self, point: usize) -> &Arc<Block> {
        &self.blocks[point]
    }

    /// The history up to `point`.
    pub fn up_to(&self, point: usize) -> Self {
        Self {
            blocks: self.blocks[..=point].to_vec(),
        }
    }

    /// A number that two histories of the same blocks have in common:
    /// every version and every state.
    pub fn digest(&self) -> u64 {
        let mut hasher = blake3::Hasher::new();
        for block in &self.blocks {
            hasher.update(&block.first.to_le_bytes());
            hasher.update(&block.version.to_le_bytes());
            hasher.update(&block.state.root);
            hasher.update(&block.state.digest);
        }
        let bytes = hasher.finalize();
        u64::from_le_bytes(bytes.as_bytes()[..8].try_into().expect("eight bytes"))
    }

    /// Where a database at `version` is, of the places `expect` allows.
    pub fn find(&self, expect: &Expect, version: u64) -> Result<Landing, String> {
        let landing = self.locate(expect, version).ok_or_else(|| {
            format!(
                "it is at version {version}, which no block made: {}",
                self.allowed(expect)
            )
        })?;
        match landing.allowed(expect) && version >= expect.durable {
            true => Ok(landing),
            false => Err(format!(
                "it is at {} (version {version}): {}",
                landing.text(),
                self.allowed(expect)
            )),
        }
    }

    /// Where a database at `version` is, allowed or not.
    fn locate(&self, expect: &Expect, version: u64) -> Option<Landing> {
        if let Some(attempt) = &expect.attempt
            && attempt.first <= version
        {
            return match version {
                at if at == attempt.version => Some(Landing::Attempt),
                at if at < attempt.version => Some(Landing::InsideAttempt),
                _ => None,
            };
        }
        let point = self.blocks.partition_point(|block| block.version < version);
        let block = self.blocks.get(point)?;
        match version {
            at if at == block.version => Some(Landing::Point(point)),
            at if at >= block.first => Some(Landing::Inside(point)),
            _ => None,
        }
    }

    /// What `expect` allows, in words.
    fn allowed(&self, expect: &Expect) -> String {
        let version = |point: usize| self.blocks.get(point).map_or(0, |block| block.version);
        let attempt = match &expect.attempt {
            Some(attempt) => format!(", or the one being made (version {})", attempt.version),
            None => String::new(),
        };
        let durable = match expect.durable {
            0 => String::new(),
            durable => format!(", and no version below {durable}"),
        };
        format!(
            "expected is a block from point {} (version {}) to point {} (version \
             {}){attempt}{durable}",
            expect.floor,
            version(expect.floor),
            expect.latest,
            version(expect.latest)
        )
    }
}

/// Where a database that was opened is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landing {
    /// At the block of this point.
    Point(usize),
    /// Inside the making of the block of this point: at a version that an
    /// operation which commits more than once made before its last.
    Inside(usize),
    /// At the block that was being made when the crash came.
    Attempt,
    /// Inside the making of that block.
    InsideAttempt,
}

impl Landing {
    /// Whether `expect` allows a database to be here.
    ///
    /// A database inside the making of a block is after the block before
    /// it and before the block itself: it is allowed where the floor is
    /// below the block.
    pub fn allowed(&self, expect: &Expect) -> bool {
        match *self {
            Self::Point(at) => expect.floor <= at && at <= expect.latest,
            Self::Inside(at) => expect.floor < at && at <= expect.latest,
            Self::Attempt | Self::InsideAttempt => expect.attempt.is_some(),
        }
    }

    /// The landing in words.
    pub fn text(&self) -> String {
        match self {
            Self::Point(at) => format!("point {at}"),
            Self::Inside(at) => format!("a commit inside the making of point {at}"),
            Self::Attempt => "the block that was being made".to_string(),
            Self::InsideAttempt => "a commit inside the block that was being made".to_string(),
        }
    }
}

/// What the workload was doing when a crash image was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Doing {
    /// Nothing that writes: between two operations, or in one of the
    /// stream that commits nothing.
    Nothing,
    /// Opening the database: making it, or recovering it.
    Open,
    /// Committing a block.
    Commit,
    /// A write with no transaction.
    Beside,
    /// Flushing the drops that are pending.
    DropsFlushed,
    /// `GroveDb::flush`.
    Flush,
    /// Making a checkpoint of the database.
    Checkpoint,
    /// Deleting a checkpoint.
    DeleteCheckpoint,
    /// Wiping the database.
    Wipe,
    /// Closing the database.
    Close,
}

impl Doing {
    /// The name the counts know it by.
    pub fn name(self) -> &'static str {
        match self {
            Self::Nothing => "nothing that commits",
            Self::Open => "an open",
            Self::Commit => "a commit",
            Self::Beside => "a write with no transaction",
            Self::DropsFlushed => "a flush of pending drops",
            Self::Flush => "a flush",
            Self::Checkpoint => "the making of a checkpoint",
            Self::DeleteCheckpoint => "the deletion of a checkpoint",
            Self::Wipe => "a wipe",
            Self::Close => "a close",
        }
    }
}

/// What is known of a checkpoint of the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Its creation is under way, or failed: the target holds nothing or
    /// the whole checkpoint.
    Building,
    /// Its creation returned `Ok`: the target holds the whole checkpoint,
    /// after any crash.
    Made,
    /// Its deletion is under way, or failed: the target holds the whole
    /// checkpoint, a directory that is marked as being removed, or nothing.
    Deleting,
    /// Its deletion returned `Ok`: the target is gone, after any crash
    /// (`lignindb::checkpoint::delete`, "When the call returns `Ok`, the
    /// removal is durable").
    Deleted,
    /// The lineage began with a state that a crash left, in which the
    /// creation or the deletion of the checkpoint was cut short: its
    /// target was what this says, no whole checkpoint, and nothing of the
    /// lineage changes it.
    Left(Held),
}

/// A checkpoint of the database, as the campaign expects to find it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    /// Its directory.
    pub dir: PathBuf,
    /// The point of the block it holds.
    pub point: usize,
    /// What is known of it.
    pub standing: Standing,
}

/// What is expected of a database after a crash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expect {
    /// The first point a crash may leave the database at: no crash loses
    /// this block.
    pub floor: usize,
    /// The point of the last block.
    pub latest: usize,
    /// The block that is being made: a crash may leave the database at it,
    /// or at a commit inside its making.
    pub attempt: Option<Arc<Block>>,
    /// The checkpoints of the database.
    pub checkpoints: Vec<Checkpoint>,
    /// What the workload is doing.
    pub doing: Doing,
    /// Whether what a wipe dropped can still be waiting to be reclaimed:
    /// from the wipe until the session ends.
    pub wiped: bool,
    /// The version of the engine that no crash leaves the database below,
    /// where that is known more closely than by [`Self::floor`]: the
    /// version that the state with no unsynced change recovered to, which
    /// can be that of the block being made or of a commit inside the
    /// making of a block. 0 where nothing of the kind is known.
    pub durable: u64,
    /// Whether a commit of the session syncs the log before it returns:
    /// a commit that has returned `Ok` is then durable.
    pub syncs: bool,
}

impl Expect {
    /// A database that is at the block of `point` and nowhere else.
    pub fn exactly(point: usize) -> Self {
        Self {
            floor: point,
            latest: point,
            attempt: None,
            checkpoints: Vec::new(),
            doing: Doing::Nothing,
            wiped: false,
            durable: 0,
            syncs: false,
        }
    }

    /// What is expected of the state that a process leaves that is killed
    /// with no crash of the machine: nothing is lost, so the database is
    /// at the last block, or at the one that is being made.
    pub fn killed(&self) -> Self {
        Self {
            floor: self.latest,
            ..self.clone()
        }
    }
}

impl fmt::Display for Expect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "points {} to {}", self.floor, self.latest)?;
        if let Some(attempt) = &self.attempt {
            write!(f, " or the block being made (version {})", attempt.version)?;
        }
        if self.durable > 0 {
            write!(f, ", not below version {}", self.durable)?;
        }
        write!(f, ", during {}", self.doing.name())
    }
}

/// The subtrees that the walks of a lineage have reached: every walk is
/// told of them, and lists what a subtree that it does not reach has left
/// behind.
pub type Places = BTreeSet<Place>;

#[cfg(test)]
mod tests {
    use super::*;

    fn record(key: u8, value: u8) -> Record {
        Record {
            path: vec![vec![1]],
            secondary: None,
            keyspace: Keyspace::Nodes,
            key: vec![key],
            value: vec![value],
            left_behind: false,
        }
    }

    fn state(root: u8, records: Vec<Record>) -> State {
        let listing = Listing {
            records,
            ..Listing::default()
        };
        State::new([root; 32], listing, false)
    }

    fn block(first: u64, version: u64, root: u8) -> Block {
        Block {
            first,
            version,
            state: state(root, vec![record(root, root)]),
            resume: Position::default(),
            made: Made::Commit,
            inside: Vec::new(),
        }
    }

    /// Blocks of versions 2, 3, 5 to 7, and 8.
    fn history() -> History {
        let mut history = History::new(block(2, 2, 0));
        for (first, version) in [(3, 3), (5, 7), (8, 8)] {
            history.push(Arc::new(block(first, version, version as u8)));
        }
        history
    }

    #[test]
    fn a_version_is_a_point_or_inside_the_making_of_one() {
        let history = history();
        let expect = Expect {
            floor: 0,
            latest: 3,
            ..Expect::exactly(0)
        };
        let find = |version| history.find(&expect, version);
        assert_eq!(find(2), Ok(Landing::Point(0)));
        assert_eq!(find(3), Ok(Landing::Point(1)));
        assert_eq!(find(5), Ok(Landing::Inside(2)));
        assert_eq!(find(6), Ok(Landing::Inside(2)));
        assert_eq!(find(7), Ok(Landing::Point(2)));
        assert_eq!(find(8), Ok(Landing::Point(3)));
        // A version that no block made: before the first, in a gap, and
        // after the last.
        for version in [1, 4, 9] {
            let said = find(version).expect_err("no block");
            assert!(said.contains("which no block made"), "{said}");
        }
        assert_eq!(history.latest(), 3);
        assert_eq!(history.last().version, 8);
        assert_eq!(history.up_to(1).latest(), 1);
        assert_ne!(history.digest(), history.up_to(2).digest());
        assert_eq!(history.digest(), history.up_to(3).digest());
    }

    #[test]
    fn no_state_is_below_the_floor_or_above_the_last_block() {
        let history = history();
        let expect = Expect {
            floor: 2,
            latest: 2,
            ..Expect::exactly(0)
        };
        // The floor is the block of versions 5 to 7: a state inside its
        // making is before it.
        for version in [2, 3, 5, 6, 8] {
            let said = history.find(&expect, version).expect_err("not allowed");
            assert!(said.contains("expected is a block from point 2"), "{said}");
        }
        assert_eq!(history.find(&expect, 7), Ok(Landing::Point(2)));
        let below = Expect {
            floor: 1,
            ..expect.clone()
        };
        assert_eq!(history.find(&below, 6), Ok(Landing::Inside(2)));
    }

    #[test]
    fn a_state_may_be_at_the_block_that_is_being_made() {
        let history = history().up_to(2);
        let mut expect = Expect::exactly(2);
        assert!(history.find(&expect, 8).is_err());
        expect.attempt = Some(Arc::new(block(8, 9, 9)));
        assert_eq!(history.find(&expect, 8), Ok(Landing::InsideAttempt));
        assert_eq!(history.find(&expect, 9), Ok(Landing::Attempt));
        assert!(history.find(&expect, 10).is_err());
        assert_eq!(history.find(&expect, 7), Ok(Landing::Point(2)));
        // What a killed process leaves is at the last block or at the one
        // being made, and so is what `exactly` allows.
        let killed = Expect {
            floor: 0,
            ..expect.clone()
        }
        .killed();
        assert_eq!(killed.floor, 2);
        assert_eq!(history.find(&killed, 7), Ok(Landing::Point(2)));
        assert!(history.find(&killed, 5).is_err());
        assert_eq!(history.find(&killed, 8), Ok(Landing::InsideAttempt));
        assert!(Landing::Attempt.allowed(&killed) && Landing::InsideAttempt.allowed(&killed));
        assert!(!Landing::Point(1).allowed(&killed) && !Landing::Inside(2).allowed(&killed));
        assert!(format!("{expect}").contains("the block being made (version 9)"));
        for landing in [
            Landing::Point(1),
            Landing::Inside(1),
            Landing::Attempt,
            Landing::InsideAttempt,
        ] {
            assert!(!landing.text().is_empty());
        }
    }

    #[test]
    #[should_panic(expected = "a block of versions 3 to 3 after one of version 8")]
    fn a_block_follows_the_versions_of_the_last() {
        history().push(Arc::new(block(3, 3, 1)));
    }

    #[test]
    fn two_states_differ_by_their_root_their_mark_or_a_record() {
        let one = state(1, vec![record(1, 1), record(2, 2)]);
        assert_eq!(one.difference(&one.clone()), None);
        let other_root = state(2, vec![record(1, 1), record(2, 2)]);
        let said = one.difference(&other_root).expect("differs");
        assert!(said.starts_with("the root hash is 0101"), "{said}");
        let marked = State {
            incomplete_restore: true,
            ..one.clone()
        };
        let said = one.difference(&marked).expect("differs");
        assert!(said.contains("is false and not true"), "{said}");
        let changed = state(1, vec![record(1, 1), record(2, 3)]);
        let said = one.difference(&changed).expect("differs");
        assert!(said.contains("differ at record 1 of 2"), "{said}");
        let shorter = state(1, vec![record(1, 1)]);
        let said = one.difference(&shorter).expect("differs");
        assert!(said.contains("the block has nothing"), "{said}");
        let said = shorter.difference(&one).expect("differs");
        assert!(said.contains("found nothing"), "{said}");
    }

    #[test]
    fn the_drops_the_engine_reports_are_not_in_a_state() {
        let mut reported = record(9, 9);
        reported.keyspace = Keyspace::PendingDrop;
        let with = state(1, vec![record(1, 1), reported]);
        assert_eq!(with.records.len(), 1);
        assert_eq!(with.digest, state(1, vec![record(1, 1)]).digest);
    }

    #[test]
    fn no_state_is_below_the_version_that_was_durable() {
        let history = history();
        let expect = Expect {
            floor: 0,
            latest: 3,
            durable: 6,
            ..Expect::exactly(0)
        };
        // Version 6 is inside the making of the block of versions 5 to 7.
        for version in [2, 3, 5] {
            let said = history.find(&expect, version).expect_err("below");
            assert!(said.contains("and no version below 6"), "{said}");
        }
        assert_eq!(history.find(&expect, 6), Ok(Landing::Inside(2)));
        assert_eq!(history.find(&expect, 8), Ok(Landing::Point(3)));
        assert!(format!("{expect}").contains("not below version 6"));
    }

    #[test]
    fn every_name_is_said() {
        for doing in [
            Doing::Nothing,
            Doing::Open,
            Doing::Commit,
            Doing::Beside,
            Doing::DropsFlushed,
            Doing::Flush,
            Doing::Checkpoint,
            Doing::DeleteCheckpoint,
            Doing::Wipe,
            Doing::Close,
        ] {
            assert!(!doing.name().is_empty());
        }
        let expect = Expect::exactly(3);
        assert_eq!(
            format!("{expect}"),
            "points 3 to 3, during nothing that commits"
        );
    }
}
