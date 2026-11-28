//! The power is cut at every point of a session's work (D-108 item 3,
//! D-110): commits, drops, the reclaimer's page steps, write-back's
//! passes and table checkpoints. Whatever the cut keeps, the next open
//! lands on a committed version at or after the last one that was made
//! durable, with what that version held, and the store is sound.
//!
//! A second session is swept the same way: one whose commit moves
//! subtrees to other prefixes (`transactions.md` 7.5, D-213 item 6).

use std::collections::BTreeMap;

use lignin_io::sim::{CrashPoint, SimConfig, SimVfs};
use lignin_store::flush::Checkpointed;

use super::{moves::Grove, *};

/// What each subtree of a script holds at a version: the keys and the
/// values of the nodes of its Merk, and its flat record under one key in
/// each column that has one. The three lives of the page ids are
/// Merks of as many nodes with other keys and values, so a version that
/// was recovered with the pages of another life is told from its own.
type State = Grove;

/// What a table checkpoint of the script comes to, where the power is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// It is installed.
    Installed,
    /// It is put off for a page that a write transaction holds back.
    PutOff,
}

/// The subtrees the first script ever enters, and the one that every
/// recovered session plants.
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

    fn plant(&mut self, name: u8, merk: Merk) -> bool {
        let items = merk.items();
        let writes = Writes {
            enter: vec![(subtree(name), None)],
            merks: vec![(subtree(name), merk)],
            ..Writes::default()
        };
        self.commit(writes, |state| {
            state.insert(name, (items, Vec::new()));
        })
    }

    fn enter(&mut self, name: u8) -> bool {
        let writes = Writes {
            enter: vec![(subtree(name), None)],
            ..Writes::default()
        };
        self.commit(writes, |state| {
            state.insert(name, (Items::new(), Vec::new()));
        })
    }

    /// The commit of a write transaction that moves the subtrees of the
    /// grove (`moves::moves`).
    fn moved(&mut self) -> bool {
        let (_, next) = moves::moves(&self.state, |_| None);
        self.ran.under_way = Some(next);
        match moves::commit_moves(self.db, &self.state) {
            Ok((Committed::Published(published), next)) => {
                self.ran.latest = published.version.get();
                self.ran.model.insert(self.ran.latest, next.clone());
                self.state = next;
                self.ran.under_way = None;
                true
            }
            Ok((Committed::Empty, _)) => panic!("the commit writes"),
            Err(error) => {
                assert!(!self.on(), "the moves failed with the power on: {error}");
                false
            }
        }
    }

    /// Rounds of the reclaimer until nothing waits for it, or the power
    /// is gone.
    fn settle(&mut self) -> bool {
        for _ in 0..1_000 {
            let waiting = self.db.reclaim().waiting;
            if !self.on() {
                return false;
            }
            if waiting == 0 {
                return true;
            }
        }
        panic!("what the moves left is not reclaimed: {}", self.db.stats());
    }

    /// The second script: a grove is committed and made durable, and one
    /// commit moves its subtrees. Two change places and a third goes to
    /// another key, each with the subtree below it and with flat records
    /// under both, and a subtree with no node leaves with its move. Then
    /// what left is reclaimed, and write-back and a table checkpoint take
    /// the commit to the segments.
    fn moves(&mut self) -> bool {
        let (grove, held) = moves::grove();
        self.commit(grove, |state| *state = held)
            && self.flush()
            && self.pass()
            && self.moved()
            && self.settle()
            && self.flush()
            && self.pass()
            && self.checkpoint(Expected::Installed)
            && self.pass()
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

    /// A table checkpoint, which comes to `expected` where the power is
    /// on. One that is installed has synced the log, so every version
    /// published before it is durable.
    fn checkpoint(&mut self, expected: Expected) -> bool {
        let taken = self.db.shared.write_back().checkpoint();
        let came_to = match &taken {
            Ok(Checkpointed::Installed { .. }) => Some(Expected::Installed),
            Ok(Checkpointed::PutOff { blocked }) if *blocked > 0 => Some(Expected::PutOff),
            _ => None,
        };
        assert!(
            taken.is_err() || came_to == Some(expected),
            "a table checkpoint at version {} came to {taken:?}, and not to {expected:?}",
            self.ran.latest
        );
        match taken {
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
        let begun = self.plant(9, Merk::sized(b'k', 40, 20))
            && self.plant(1, wide(b'a'))
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
            && self.plant(2, wide(b'b'))
            && self.drop_of(2)
            // The reclaiming, whose steps free pages that never had a link
            // and are given no free link (D-110). Then a pass that trims, a
            // table checkpoint with those steps in the log before it, and
            // the third life of the ids.
            && self.reclaim(2)
            && self.pass()
            && self.checkpoint(Expected::Installed)
            && self.enter(22)
            && self.reclaim(2)
            && self.enter(23)
            && self.reclaim(2)
            && self.plant(3, wide(b'c'))
            // A Merk with values out of line, which wait for their place.
            && self.plant(50, long());
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
        let ended = held && self.pass() && self.checkpoint(Expected::PutOff);
        let rolled_back = self.db.rollback(&txn).is_ok();
        drop(txn);
        ended
            && rolled_back
            && self.pass()
            && self.flush()
            && self.checkpoint(Expected::Installed)
            && self.pass()
    }
}

/// One of the scripts.
type Run<'r> = &'r dyn Fn(&mut Script<'_>) -> bool;

/// Runs `script` on a new database in `fs`, with the power cut after
/// `cut` more changes of the file layer than the open made, if a cut is
/// given.
fn ran(fs: &SimVfs, cut: Option<u64>, script: Run<'_>) -> Ran {
    let options = Options {
        vfs: Arc::new(fs.clone()),
        ..options()
    };
    let db = open_at_rest("/db", &options).expect("a new database");
    let mut session = Script {
        db: &db,
        fs,
        ran: Ran::default(),
        state: State::new(),
    };
    // The version of the open's own commit: no subtree.
    session.ran.latest = db.version().get();
    session.ran.durable = session.ran.latest;
    session.ran.model.insert(session.ran.latest, State::new());
    if let Some(cut) = cut {
        fs.crash_at(CrashPoint::AfterChange(fs.stats().changes + cut));
    }
    session.ran.whole = script(&mut session);
    let ran = session.ran;
    db.unchecked_at_close();
    drop(db);
    ran
}

/// What a reader of the latest version of `db` finds of the subtrees of
/// the scripts: every key and value of each Merk, and the flat record
/// under the one key that a script writes one under.
fn found(db: &Db) -> State {
    let names: Vec<u8> = NAMES.into_iter().chain(moves::NAMES).collect();
    moves::found(db, &names)
}

/// The script alone, with the power on: it makes what the sweep is for.
#[test]
fn the_script_frees_pages_that_never_had_a_link_around_a_table_checkpoint() {
    let fs = SimVfs::new(SimConfig::new(31));
    let ran = ran(&fs, None, &|script: &mut Script<'_>| script.run());
    assert!(ran.whole);
    assert!(
        ran.never_linked > 0,
        "no page was freed before it had a link"
    );
    // The first table checkpoint was installed with those freeing steps
    // in the log before it. The script asserts what each of its three
    // checkpoints came to where it takes it: installed, put off,
    // installed.
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
/// was made durable, and a reader finds what that version held: every
/// key and every value of every Merk. The reclaimer of the new session
/// then takes out what was left, a commit is taken, and the store is
/// sound to the physical checker. The background threads of that session
/// are ended for it, so its close finishes nothing more: the checker
/// reads the store as the reclaiming and the commit left it.
///
/// The commits of the script make nothing durable by themselves (the
/// default sync policy), so a cut lands on the last version that a flush
/// or an installed table checkpoint made durable, or on a later one that
/// the cut happened to keep.
#[test]
fn a_power_cut_at_any_point_of_a_session_recovers_a_committed_version() {
    let swept = swept(31, &|script: &mut Script<'_>| script.run());
    let Swept {
        cuts, landed_on, ..
    } = &swept;
    assert!(*cuts > 50, "{cuts} cuts");
    // Cuts are recovered at versions all along the script, the last one
    // among them.
    let latest = swept.whole.latest;
    assert!(landed_on.len() as u64 > latest / 2, "{landed_on:?}");
    assert!(landed_on.contains_key(&latest), "{landed_on:?}");
}

/// What a sweep of a script came to.
struct Swept {
    /// The script with the power on.
    whole: Ran,
    /// How many cuts were made.
    cuts: u32,
    /// How many states landed on each version.
    landed_on: BTreeMap<u64, u32>,
}

/// The sweep of `script`, as the test above describes it, with the
/// simulator seeded with `seed`.
fn swept(seed: u64, script: Run<'_>) -> Swept {
    let whole = ran(&SimVfs::new(SimConfig::new(seed)), None, script);
    assert!(whole.whole);
    let (mut cuts, mut states, mut in_flight) = (0u32, 0u32, 0u32);
    let mut landed_on: BTreeMap<u64, u32> = BTreeMap::new();
    for cut in 1.. {
        let fs = SimVfs::new(SimConfig::new(seed));
        let ran = ran(&fs, Some(cut), script);
        if ran.whole && !fs.is_powered_off() {
            break;
        }
        assert!(
            fs.is_powered_off(),
            "a cut after change {cut}: the script stopped with the power on"
        );
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
    Swept {
        whole,
        cuts,
        landed_on,
    }
}

/// D-213 item 6: the power is cut after every change that the second
/// script makes to the files, and each cut is recovered as in the sweep
/// above. The commit that moves the subtrees is recovered whole or not
/// at all: a state lands on the version before it, with every node and
/// every flat record under the prefixes they had, or on its version, with
/// each under the prefix its subtree went to, and the store is sound to
/// the physical checker either way.
#[test]
fn a_power_cut_at_any_point_of_a_commit_that_moves_subtrees_recovers_it_whole_or_not_at_all() {
    let swept = swept(53, &|script: &mut Script<'_>| script.moves());
    // The open's commit, the grove, and the moves.
    let moved = swept.whole.latest;
    assert_eq!(swept.whole.model.len(), 3);
    let (before, after) = (&swept.whole.model[&(moved - 1)], &swept.whole.model[&moved]);
    assert_ne!(before, after);
    assert!(before.len() > after.len(), "a subtree left with a move");
    let landed = |version: u64| swept.landed_on.get(&version).copied().unwrap_or(0);
    assert!(
        landed(moved - 1) > 0 && landed(moved) > 0,
        "{:?}",
        swept.landed_on
    );
    assert!(swept.cuts > 20, "{} cuts", swept.cuts);
}
