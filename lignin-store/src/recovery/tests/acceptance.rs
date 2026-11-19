//! The acceptance tests of STORE-07: **recovery after 256 MB of log
//! completes within 5 s**, and **replaying twice gives the same state**.
//!
//! The store is one that a session of the reference workload's shape
//! leaves when it is killed (`flush/tests/measure.rs` has the shape, from
//! `transactions.md` 18.8): blocks of 20 transitions, one log group each.
//! A transition changes a hash in the root page and in one page of each
//! of three levels, and puts one new cell with a value of 460 bytes into a
//! leaf, out of line. Write-back is one block behind: the values of a
//! block are placed, each in a page step, after the next block's group. A
//! table checkpoint covers the pages the session began with, and the log
//! after it is what recovery replays.

use std::time::{Duration, Instant};

use super::*;

/// Transitions in a block.
const TRANSITIONS: u32 = 20;
/// Cells in the root page, and in each page of the two levels under it.
const FAN: u32 = 64;
/// The first page of the second level, and of the third.
const SECOND: u32 = 2;
const THIRD: u32 = SECOND + 8;
/// The first leaf.
const LEAVES: u32 = THIRD + FAN;
/// Leaves that take cells at one time: a transition's cell goes to the
/// next of them in turn.
const OPEN_LEAVES: u32 = 512;
/// Cells a leaf has when the session begins, and how many it takes.
const LEAF_BEGINS_WITH: u16 = 40;
const LEAF_TAKES: u32 = 150;
/// Bytes of a value that is too long for its cell.
const VALUE_LEN: usize = 460;

/// A session of the reference workload's shape on a new store.
struct Workload<'d> {
    session: Session<'d>,
    /// Transitions made so far.
    made: u32,
    /// The cells whose values the last block left pending.
    waiting: Vec<(u32, u16)>,
    /// How many cells each leaf holds.
    cells: BTreeMap<u32, u16>,
    /// The place the checkpoint's sample has, from which the log counts.
    checkpoint: LogPlace,
}

impl<'d> Workload<'d> {
    /// A store with its upper pages and its first leaves written back,
    /// under a table checkpoint that covers them.
    fn begin(dir: &'d Dir) -> Self {
        let mut session = Session::begin(dir, Model::default());
        let upper = 1..LEAVES;
        for ids in upper.clone().collect::<Vec<_>>().chunks(8) {
            let ops: Vec<Op> = ids
                .iter()
                .flat_map(|&id| (1..=FAN as u16).map(move |at| (id, at)))
                .map(|at| insert(at, &key(at.0, at.1.into()), inline(&[at.1 as u8; 8])))
                .collect();
            session.commit(&ops);
        }
        let mut cells = BTreeMap::new();
        for ids in (LEAVES..LEAVES + OPEN_LEAVES).collect::<Vec<_>>().chunks(8) {
            let ops: Vec<Op> = ids
                .iter()
                .flat_map(|&id| (1..=LEAF_BEGINS_WITH).map(move |at| (id, at)))
                .map(|at| insert(at, &key(at.0, at.1.into()), inline(&[at.1 as u8; 8])))
                .collect();
            session.commit(&ops);
            cells.extend(ids.iter().map(|&id| (id, LEAF_BEGINS_WITH)));
        }
        let covers = session.latest().get();
        for id in upper.chain(LEAVES..LEAVES + OPEN_LEAVES) {
            session.write_back(id, covers);
        }
        let checkpoint = session.checkpoint(covers);
        Self {
            session,
            made: 0,
            waiting: Vec::new(),
            cells,
            checkpoint,
        }
    }

    /// The leaf that the transition `made` puts its cell into.
    fn leaf(made: u32) -> u32 {
        let round = made / (OPEN_LEAVES * LEAF_TAKES);
        LEAVES + round * OPEN_LEAVES + made % OPEN_LEAVES
    }

    /// One block: its group, and then the page steps that place the
    /// values of the block before it.
    fn block(&mut self) {
        let mut ops = Vec::with_capacity(5 * TRANSITIONS as usize);
        let mut placed = Vec::with_capacity(TRANSITIONS as usize);
        for _ in 0..TRANSITIONS {
            let made = self.made;
            self.made += 1;
            let stamp = made as u8;
            let at = |of: u32| 1 + (made / of % FAN) as u16;
            ops.push(hash((1, at(1)), stamp));
            ops.push(hash((SECOND + made % 8, at(8)), stamp));
            ops.push(hash((THIRD + made % FAN, at(FAN)), stamp));
            let leaf = Self::leaf(made);
            let held = self.cells.entry(leaf).or_insert(0);
            if *held > 0 {
                ops.push(hash((leaf, 1), stamp));
            }
            *held += 1;
            let value = Value::Out((0..VALUE_LEN).map(|n| stamp ^ n as u8).collect());
            ops.push(insert((leaf, *held), &key(leaf, made), value));
            placed.push((leaf, *held));
        }
        self.session.commit(&ops);
        let waiting = std::mem::replace(&mut self.waiting, placed);
        self.session.place_all(&waiting);
        if self.session.store.log.is_due() {
            self.session.turn();
        }
    }

    /// How many bytes of log lie after the checkpoint's sample.
    fn logged(&self) -> u64 {
        let store = &self.session.store;
        let end = store.log.end().expect("the end of the log");
        let files = store.log.files_named().expect("the files");
        let lens: u64 = files
            .iter()
            .filter(|file| file.id() >= self.checkpoint.file() && file.id() < end.file())
            .map(|file| file.valid_len())
            .sum();
        lens + end.offset() - self.checkpoint.offset()
    }
}

/// The key of the cell that transition `made` put into the page `id`.
fn key(id: u32, made: u32) -> Vec<u8> {
    format!("{id:08x}/{made:015}").into_bytes()
}

/// What two recoveries of one store must agree on, beside what [`check`]
/// holds each of them to.
fn state(store: &Recovered) -> impl PartialEq + std::fmt::Debug {
    let mut ids = Vec::new();
    let mut next = Some(PageId::MIN);
    while let Some(from) = next {
        next = store.allocator.scan(from, &mut ids);
    }
    (store.landed(), store.values.counts(), ids)
}

/// Builds a store whose log holds `log_bytes` bytes after its table
/// checkpoint, kills the session, and recovers the store once for each
/// entry of `kept`, with that many rebuilt pages kept in memory
/// ([`RecoveryOptions::max_replay_pages`]): `None` keeps them all here,
/// and a number makes replay write pages to a segment while it reads the
/// log. Between two recoveries the store is dropped as a killed process
/// leaves it, and each lands on the same state. Returns how long each
/// recovery took and what it did.
///
/// The store is built in memory, where the syncs of the session that
/// builds it cost nothing, and recovered where [`Dir::on_disk`] puts it.
fn recover_after(log_bytes: u64, kept: &[Option<usize>]) -> Vec<(Duration, RecoveryReport)> {
    let dir = Dir::of(PageSize::DEFAULT);
    let mut workload = Workload::begin(&dir);
    while workload.logged() < log_bytes {
        workload.block();
    }
    let (logged, blocks) = (workload.logged(), workload.made / TRANSITIONS);
    let latest = workload.session.latest();
    let pending = workload.waiting.len() as u64;
    let model = workload.session.kill();
    let dir = dir.on_disk().unwrap_or(dir);

    let mut first = None;
    let mut recoveries = Vec::with_capacity(kept.len());
    for &kept in kept {
        let options = RecoveryOptions {
            max_replay_pages: kept,
            ..RecoveryOptions::default()
        };
        let began = Instant::now();
        let store = recover(dir.manifest(), &options).expect("the store");
        let took = began.elapsed();
        let report = store.report;
        assert_eq!(store.landed(), latest);
        assert!(report.log_bytes >= logged, "{report:?}");
        assert_eq!(u64::from(blocks), report.commits);
        assert_eq!(report.pending_values, pending);
        assert_eq!(report.changes_dropped, 0);
        assert_eq!(report.pages_spilled > 0, kept.is_some(), "{report:?}");
        assert!(store.checkpoint.is_some() && report.covers < latest);
        check(&store, &model, &[]);
        let found = (report.covers, format!("{:?}", state(&store)));
        assert_eq!(first.get_or_insert_with(|| found.clone()), &found);
        recoveries.push((took, report));
    }
    recoveries
}

/// **Replaying twice gives the same state**, on a store of the reference
/// workload's shape with a few megabytes of log: both recoveries land on
/// the last commit, and every page, every pending value, every count of
/// live values and every page id is what the session left. So does a
/// third, which keeps fewer pages in memory than the log changes and
/// writes the others to a segment.
#[test]
fn replaying_twice_gives_the_same_state() {
    let recoveries = recover_after(3 << 20, &[None, None, Some(400)]);
    let (_, report) = recoveries[0];
    assert!(report.commits > 100, "{report:?}");
    assert!(report.steps > 2_000, "{report:?}");
    assert!(report.pages_rebuilt > 500, "{report:?}");
    let (_, wrote) = recoveries[2];
    assert!(wrote.pages_spilled > 400, "{wrote:?}");
    assert!(wrote.pages_rebuilt < report.pages_rebuilt, "{wrote:?}");
}

/// **Recovery after 256 MB of log completes within 5 s.** The log after
/// the table checkpoint is 256 MiB, which is the most write-back lets a
/// session hold (`FlushConfig::max_log_held`), and the session was killed
/// with nothing synced since its log last turned. The time is that of
/// [`recover`] alone, from the manifest to a store that takes commits.
///
/// The store is recovered three times. The log changes fewer pages than
/// replay keeps by default, so the first two recoveries write no page. The
/// third keeps 1024, which is more than the session changes in turn and
/// fewer than the log changes in all: it writes pages to a segment
/// several times over, and is held to the same 5 s.
///
/// The store is in memory unless `LIGNIN_STORE_TEST_DIR` names a
/// directory on a disk. There the time is the replay and a dozen syncs
/// (see "What a recovery costs"), so the test measures the disk's syncs as
/// much as the recovery, and fails on a disk that takes seconds for one.
#[test]
#[ignore = "the acceptance measurement, for a release build: 256 MiB of log, about 1 GiB of memory"]
fn recovery_after_256_mb_of_log_completes_within_5_s() {
    let recoveries = recover_after(256 << 20, &[None, None, Some(1024)]);
    for (took, report) in &recoveries {
        println!(
            "recovered {} MiB of log in {took:?}: {report:?}",
            report.log_bytes >> 20
        );
        assert!(report.log_bytes >= 256 << 20, "{report:?}");
        assert!(*took < Duration::from_secs(5), "{took:?}");
    }
}
