//! The power is cut at every point of a session's work (D-108 item 3,
//! D-110): commits, drops, the reclaimer's page steps, write-back's
//! passes and table checkpoints. Whatever the cut keeps, the next open
//! lands on a committed version at or after the last one that was made
//! durable, with what that version held, and the store is sound.

use std::collections::BTreeMap;

use lignin_io::sim::{CrashPoint, SimConfig, SimVfs};
use lignin_store::flush::Checkpointed;

use super::*;

/// What each subtree of the script holds at a version: how many nodes.
type State = BTreeMap<u8, u32>;

/// The subtrees the script ever enters.
const NAMES: [u8; 10] = [1, 2, 3, 9, 20, 21, 22, 23, 50, 77];

/// What a run of the script did, to the point where the power went.
#[derive(Default)]
struct Ran {
    /// What every published version holds.
    model: BTreeMap<u64, State>,
    /// What the commit that was under way when the power went would
    /// publish: its group can be in the log (DU2).
    under_way: Option<State>,
    /// The latest version that a flush or an installed table checkpoint
    /// made durable.
    durable: u64,
    /// The latest published version.
    latest: u64,
    /// Whether the whole script ran.
    whole: bool,
    /// Pages that a reclaiming step freed before write-back had linked
    /// them: their ids were another page's before (D-110).
    never_linked: u64,
    /// How many of them were freed before the first table checkpoint was
    /// installed: their freeing steps are in the log before it.
    never_linked_at_checkpoint: u64,
    /// Whether a pass of write-back met a page that a write transaction
    /// held back.
    held_back: bool,
    /// Table checkpoints that were installed.
    installed: u32,
    /// Table checkpoints that were put off for a page that was held back.
    put_off: u32,
}

/// The session's work, step by step on one thread, with the background
/// threads ended. It stops where the file layer has no power.
struct Script<'d> {
    db: &'d Db,
    fs: &'d SimVfs,
    ran: Ran,
    state: State,
}

impl Script<'_> {
    fn on(&self) -> bool {
        !self.fs.is_powered_off()
    }

    /// One commit that leaves the subtrees as `change` makes them.
    fn commit(&mut self, writes: Writes, change: impl FnOnce(&mut State)) -> bool {
        let mut next = self.state.clone();
        change(&mut next);
        self.ran.under_way = Some(next.clone());
        match writes.with(|batch| self.db.write(batch, &mut NoStaleHashes)) {
            Ok(Committed::Published(published)) => {
                self.ran.latest = published.version.get();
                self.ran.model.insert(self.ran.latest, next.clone());
                self.state = next;
                self.ran.under_way = None;
                true
            }
            Ok(Committed::Empty) => panic!("the commit writes"),
            Err(error) => {
                assert!(!self.on(), "a commit failed with the power on: {error}");
                false
            }
        }
    }

    fn plant(&mut self, name: u8, merk: Merk, nodes: u32) -> bool {
        let writes = Writes {
            enter: vec![(subtree(name), None)],
            merks: vec![(subtree(name), merk)],
            ..Writes::default()
        };
        self.commit(writes, |state| {
            state.insert(name, nodes);
        })
    }

    fn enter(&mut self, name: u8) -> bool {
        let writes = Writes {
            enter: vec![(subtree(name), None)],
            ..Writes::default()
        };
        self.commit(writes, |state| {
            state.insert(name, 0);
        })
    }

    fn drop_of(&mut self, name: u8) -> bool {
        let writes = Writes {
            drop: vec![subtree(name)],
            ..Writes::default()
        };
        self.commit(writes, |state| {
            state.remove(&name);
        })
    }

    /// Rounds of the reclaimer until `merks` dropped subtrees are taken
    /// out, or the power is gone.
    fn reclaim(&mut self, merks: u64) -> bool {
        for _ in 0..1_000 {
            self.db.reclaim();
            let reclaimed = self.db.stats().reclaimed;
            self.ran.never_linked = reclaimed.pages_never_linked;
            if !self.on() {
                return false;
            }
            if reclaimed.merks == merks && reclaimed.waiting == 0 {
                return true;
            }
        }
        panic!("the drops are not reclaimed: {}", self.db.stats());
    }

    /// A pass of write-back: page versions go to segments, and chains are
    /// trimmed.
    fn pass(&mut self) -> bool {
        match self.db.shared.write_back().pass() {
            Ok(pass) => {
                self.ran.held_back |= pass.blocked + pass.waiting > 0;
                true
            }
            Err(error) => {
                assert!(!self.on(), "a pass failed with the power on: {error}");
                false
            }
        }
    }

    /// A table checkpoint. One that is installed has synced the log, so
    /// every version published before it is durable.
    fn checkpoint(&mut self) -> bool {
        match self.db.shared.write_back().checkpoint() {
            Ok(Checkpointed::Installed { .. }) => {
                if self.ran.installed == 0 {
                    self.ran.never_linked_at_checkpoint = self.ran.never_linked;
                }
                self.ran.installed += 1;
                self.ran.durable = self.ran.latest;
                true
            }
            Ok(Checkpointed::PutOff { .. }) => {
                self.ran.put_off += 1;
                true
            }
            Err(error) => {
                assert!(!self.on(), "a checkpoint failed with the power on: {error}");
                false
            }
        }
    }

    fn flush(&mut self) -> bool {
        let through = self.ran.latest;
        match self.db.flush() {
            Ok(_) => {
                self.ran.durable = through;
                true
            }
            Err(error) => {
                assert!(!self.on(), "a flush failed with the power on: {error}");
                false
            }
        }
    }

    /// The whole script. `false` where the power went.
    fn run(&mut self) -> bool {
        let long = || Merk::sized(b'l', 6, 1500);
        let wide = |lead: u8| Merk::sized(lead, 120, 30);
        // A Merk that stays, and the first life of some page ids: a Merk
        // whose pages are written back, dropped, reclaimed and trimmed.
        let begun = self.plant(9, Merk::sized(b'k', 40, 20), 40)
            && self.plant(1, wide(b'a'), 120)
            && self.flush()
            && self.pass()
            && self.drop_of(1)
            && self.reclaim(1)
            && self.pass()
            // Two commits and their reclaims bring the ids back.
            && self.enter(20)
            && self.reclaim(1)
            && self.enter(21)
            && self.reclaim(1)
            // The second life of the ids: a Merk that is dropped before
            // write-back has linked its pages.
            && self.plant(2, wide(b'b'), 120)
            && self.drop_of(2)
            // The reclaiming, whose steps free pages that never had a link
            // and are given no free link (D-110). Then a pass that trims, a
            // table checkpoint with those steps in the log before it, and
            // the third life of the ids.
            && self.reclaim(2)
            && self.pass()
            && self.checkpoint()
            && self.enter(22)
            && self.reclaim(2)
            && self.enter(23)
            && self.reclaim(2)
            && self.plant(3, wide(b'c'), 120)
            // A Merk with values out of line, which wait for their place.
            && self.plant(50, long(), 6);
        if !begun {
            return false;
        }
        // A write transaction copies a page of the Merk with the long
        // values and stays open: write-back cannot place the values, and
        // the page puts the table checkpoint off.
        let txn = self.db.begin();
        let held = {
            let Ok(scope) = txn.scope() else {
                return false;
            };
            let again = Writes {
                merks: vec![(subtree(50), long())],
                ..Writes::default()
            };
            let applied = again.with(|batch| scope.apply(batch));
            scope.merge();
            applied.is_ok()
        };
        let ended = held && self.pass() && self.checkpoint();
        let rolled_back = self.db.rollback(&txn).is_ok();
        drop(txn);
        ended && rolled_back && self.pass() && self.flush() && self.checkpoint() && self.pass()
    }
}

/// Runs the script on a new database in `fs`, with the power cut after
/// `cut` more changes of the file layer than the open made, if a cut is
/// given.
fn ran(fs: &SimVfs, cut: Option<u64>) -> Ran {
    let options = Options {
        vfs: Arc::new(fs.clone()),
        ..options()
    };
    let db = Db::open("/db", &options).expect("a new database");
    halt_background(&db);
    let mut script = Script {
        db: &db,
        fs,
        ran: Ran::default(),
        state: State::new(),
    };
    // The version of the open's own commit: no subtree.
    script.ran.latest = db.version().get();
    script.ran.durable = script.ran.latest;
    script.ran.model.insert(script.ran.latest, State::new());
    if let Some(cut) = cut {
        fs.crash_at(CrashPoint::AfterChange(fs.stats().changes + cut));
    }
    script.ran.whole = script.run();
    let ran = script.ran;
    db.unchecked_at_close();
    drop(db);
    ran
}

/// What a reader of the latest version of `db` finds of the script's
/// subtrees.
fn found(db: &Db) -> State {
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let nodes = NAMES.into_iter().filter_map(|name| {
        let (nodes, records) = seen(&operation, subtree(name), b"k")?;
        assert!(records.is_empty());
        Some((name, nodes as u32))
    });
    nodes.collect()
}

/// The script alone, with the power on: it makes what the sweep is for.
#[test]
fn the_script_frees_pages_that_never_had_a_link_around_a_table_checkpoint() {
    let fs = SimVfs::new(SimConfig::new(31));
    let ran = ran(&fs, None);
    assert!(ran.whole);
    assert!(
        ran.never_linked > 0,
        "no page was freed before it had a link"
    );
    // The first table checkpoint was installed with those freeing steps
    // in the log before it; the one in the middle was put off.
    assert_eq!(ran.never_linked_at_checkpoint, ran.never_linked);
    assert!(
        ran.held_back,
        "no page was held back by the write transaction"
    );
    assert_eq!((ran.installed, ran.put_off), (2, 1));
    assert_eq!(ran.durable, ran.latest);
    println!(
        "{} changes of the file layer, {} versions, {} pages never linked",
        fs.stats().changes,
        ran.latest,
        ran.never_linked
    );
}

/// The power is cut after every change that the script makes to the
/// files, and each cut is recovered in four states: nothing kept
/// that was not synced, everything kept, and two drawn from the seed.
/// Each state opens. It lands on a version the script published, or on
/// the one whose commit was under way, at or after the last version that
/// was made durable, and a reader finds what that version held. The
/// reclaimer of the new session then takes out what was left, a commit
/// is taken, and the store is sound to the physical checker.
///
/// The commits of the script make nothing durable by themselves (the
/// default sync policy), so a cut lands on the last version that a flush
/// or an installed table checkpoint made durable, or on a later one that
/// the cut happened to keep.
#[test]
fn a_power_cut_at_any_point_of_a_session_recovers_a_committed_version() {
    let whole = ran(&SimVfs::new(SimConfig::new(31)), None);
    let (mut cuts, mut states, mut in_flight) = (0u32, 0u32, 0u32);
    let mut landed_on: BTreeMap<u64, u32> = BTreeMap::new();
    for cut in 1.. {
        let fs = SimVfs::new(SimConfig::new(31));
        let ran = ran(&fs, Some(cut));
        if ran.whole && !fs.is_powered_off() {
            break;
        }
        cuts += 1;
        let image = fs.crash_image();
        let plans = [
            image.lose_all(),
            image.keep_all(),
            image.sample(0),
            image.sample(1),
        ];
        for (index, plan) in plans.iter().enumerate() {
            let context = format!("a cut after change {cut}, crash plan {index}");
            let left = image
                .recover(plan)
                .unwrap_or_else(|error| panic!("{context}: no such state: {error:?}"));
            let options = Options {
                vfs: Arc::new(left),
                ..options()
            };
            let db = Db::open("/db", &options)
                .unwrap_or_else(|error| panic!("{context}: the next open: {error}"));
            states += 1;
            let landed = db.version().get();
            assert!(
                ran.durable <= landed && landed <= ran.latest + 1,
                "{context}: landed on {landed}, durable {}, latest {}",
                ran.durable,
                ran.latest
            );
            let expected = match ran.model.get(&landed) {
                Some(state) => state,
                None => {
                    in_flight += 1;
                    let under_way = ran.under_way.as_ref();
                    under_way.unwrap_or_else(|| panic!("{context}: no commit made {landed}"))
                }
            };
            assert_eq!(&found(&db), expected, "{context}: at version {landed}");
            *landed_on.entry(landed).or_default() += 1;

            // The session goes on: what drops left is reclaimed, a commit
            // is taken, and the store is whole.
            halt_background(&db);
            for _ in 0..1_000 {
                if db.reclaim().waiting == 0 {
                    break;
                }
            }
            let stats = db.stats();
            assert_eq!(stats.reclaimed.waiting, 0, "{context}: {stats}");
            assert_eq!(stats.background.reclaim_errors, 0, "{context}: {stats}");
            plant(&db, subtree(77), Merk::sized(b'k', 5, 20));
            let report = db
                .close_and_verify(None)
                .unwrap_or_else(|error| panic!("{context}: the close: {error}"));
            assert!(report.is_sound(), "{context}:\n{report}");
        }
    }
    println!(
        "{cuts} cuts, {states} states; {in_flight} landed on a commit that was under way; \
         versions landed on: {landed_on:?}; the script publishes {} versions",
        whole.latest
    );
    assert!(cuts > 50, "{cuts} cuts");
    // Cuts are recovered at versions all along the script, the last one
    // among them.
    assert!(landed_on.len() as u64 > whole.latest / 2, "{landed_on:?}");
    assert!(landed_on.contains_key(&whole.latest), "{landed_on:?}");
}
