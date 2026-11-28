//! The pace of the background loop after turns that failed (D-204;
//! `docs/lignindb/disk-full-and-backup.md`, DF12).
//!
//! The state is made by construction. A value is ready for its page step,
//! the log has refused the step's record and takes nothing until it is
//! rolled, and it has no file to turn to. Then the device refuses every
//! write and every new name, and its syncs work: the log cannot be
//! rolled, and every turn fails. A loop that takes each call for a turn
//! as a reason to turn at once does not wait in that state, because the
//! page step of each turn, refused by the log, is such a call.
//!
//! A second device takes the log's files and refuses the record of each
//! page step. There the log can be rolled, and it is the step that fails
//! the turn.

use std::{
    collections::BTreeSet,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use lignin_format::FileId;
use lignin_io::sim::{Fault, FaultRule, OpKind, OpSet};

use super::*;
use crate::{
    flush::{checkpoint::ROLL_ROUNDS, state::lock},
    segment::{
        tests::sim::{db_dir, shared, sim_fs},
        FileNames,
    },
    wal::{
        tests::faults::{Lies, Swapping},
        LogError,
    },
};

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// The device refuses every write and every new name from here on, and
/// its syncs work.
fn fill(fs: &SimVfs) {
    let full = FaultRule::new(Fault::Enospc).on(OpSet::WRITES | OpSet::NAMES);
    fs.add_rule(full.torn_append(false).forever());
}

/// The names of the log's files.
fn log_files(db: &Db) -> Vec<String> {
    let entries = db.vfs.read_dir(&FileNames::WAL.dir(&db.path));
    let mut names: Vec<String> = entries
        .expect("a listing")
        .into_iter()
        .map(|entry| entry.name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A pass in which the log refuses the record of the one page step:
/// the step is put off, and the pass has failed with what the log
/// answered.
fn pass_with_a_refused_step(db: &Db) {
    let error = db.wb().pass().expect_err("the log refused the record");
    assert!(
        matches!(
            error,
            FlushError::Log(LogError::NotAppended { .. } | LogError::TakenBack { .. })
        ),
        "{error}"
    );
}

/// A store in the state of the module's documentation, on a device that
/// is full.
fn stuck(config: FlushConfig) -> (SimVfs, Db) {
    let fs = sim_fs(SimConfig::new(5));
    let db = Db::create(shared(&fs), db_dir(), config, LogOptions::default());
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    // The log turns to the file it had ready, and has none to turn to:
    // the next turn of the log has to make one.
    assert!(db.log.rotate_early().expect("a turn of the log"));
    assert!(!db.log.has_file_ready());
    let file = db.log.end().expect("the log has a file").file();
    // The record of the page step is refused, and nothing else is: the
    // value is appended and covered, and stays ready.
    let record = FaultRule::new(Fault::Enospc)
        .on(OpKind::Append)
        .at(FileNames::WAL.path(db_dir(), file));
    fs.add_rule(record.torn_append(false));
    pass_with_a_refused_step(&db);
    assert_eq!(lock(&db.state.placing).len(), 1, "a value is ready");
    assert_eq!(db.pointers(page(1)).len(), 0, "and has no pointer");
    assert!(db.log.is_due(), "the log takes nothing until it is rolled");
    // The step asked the loop for a turn, for the cut that the log needs.
    assert!(std::mem::take(&mut *lock(&db.state.wake)));
    fill(&fs);
    (fs, db)
}

#[test]
fn the_wait_after_a_failed_turn_doubles_from_the_idle_time_to_its_bound() {
    let config = eager();
    assert_eq!((config.idle, config.max_failed_wait), (ms(50), ms(1000)));
    let waits = |config: FlushConfig| -> Vec<u128> {
        let wait = |in_vain| config.wait_after_failure(in_vain).as_millis();
        (0..9).map(wait).collect()
    };
    assert_eq!(
        waits(config),
        [50, 50, 100, 200, 400, 800, 1000, 1000, 1000]
    );
    assert_eq!(config.wait_after_failure(u32::MAX), ms(1000));
    // A bound below the idle time is the idle time, and no wait is
    // shorter than a millisecond.
    let low = FlushConfig {
        max_failed_wait: ms(10),
        ..config
    };
    assert_eq!(waits(low), [50; 9]);
    let none = FlushConfig {
        idle: Duration::ZERO,
        max_failed_wait: Duration::ZERO,
        ..config
    };
    assert_eq!(waits(none), [1; 9]);
    // An idle time of nothing grows from a millisecond.
    let busy = FlushConfig {
        idle: Duration::ZERO,
        max_failed_wait: ms(100),
        ..config
    };
    assert_eq!(waits(busy), [1, 1, 2, 4, 8, 16, 32, 64, 100]);
    // The wait grows to its bound however large the bound is: from a
    // millisecond, 32 doublings pass 30 days, and 31 do not.
    let days = |days: u64| Duration::from_secs(days * 24 * 3600);
    let patient = FlushConfig {
        idle: ms(1),
        max_failed_wait: days(30),
        ..config
    };
    assert!(patient.wait_after_failure(32) < days(25));
    assert_eq!(patient.wait_after_failure(33), days(30));
    assert_eq!(patient.wait_after_failure(u32::MAX), days(30));
    let endless = FlushConfig {
        max_failed_wait: Duration::MAX,
        ..patient
    };
    assert_eq!(endless.wait_after_failure(u32::MAX), Duration::MAX);
}

/// Each turn on the full device fails, moves nothing on, and is followed
/// by a longer wait, up to the bound. Every turn asks for the same log
/// file, makes one file operation, and leaves no name behind that no
/// directory sync covers. With room the next turn does what they could
/// not, and the turn that fails after it waits the idle time again.
#[test]
fn a_turn_on_a_device_that_stays_full_waits_longer_each_time_and_uses_up_no_id() {
    let config = FlushConfig {
        idle: ms(10),
        max_failed_wait: ms(80),
        ..eager()
    };
    let (fs, db) = stuck(config);
    let wb = db.wb();
    let files = log_files(&db);
    let ops = fs.stats().ops;
    let names = fs.crash_image().names().len();
    let mut errors = Vec::new();
    let mut in_vain = 0;
    let mut waits = Vec::new();
    for _ in 0..7 {
        let wait = wb.paced_turn(&mut in_vain, &mut |error| errors.push(error.to_string()));
        waits.push(wait.map(|wait| wait.as_millis()));
    }
    let ops = fs.stats().ops - ops;
    assert_eq!(waits, [10, 20, 40, 80, 80, 80, 80].map(Some));
    assert_eq!((errors.len(), db.stats().errors), (7, 7));
    // Every turn asked for the log file after the ones there are, and for
    // no other: no id was used up.
    let next = FileId::new(files.len() as u32 + 1).expect("an id");
    let made = FileNames::WAL.path(db_dir(), next);
    let cannot = format!("cannot create {}", made.display());
    let others: BTreeSet<&String> = (errors.iter())
        .filter(|error| !error.contains(&cannot))
        .collect();
    assert!(others.is_empty(), "not \"{cannot}\": {others:?}");
    assert_eq!(log_files(&db), files, "a turn left a file");
    assert_eq!(ops, 7, "the file operations of 7 failed turns");
    assert_eq!(
        fs.crash_image().names().len(),
        names,
        "a failed turn changed a name that no directory sync covers"
    );

    // With room the next turn rolls the log and places the value, and the
    // log goes on in the file that the failed turns had asked for.
    fs.clear_rules();
    let mut none = |error: FlushError| panic!("a turn with room failed: {error}");
    assert_eq!(wb.paced_turn(&mut in_vain, &mut none), None);
    assert_eq!(in_vain, 0);
    assert_eq!(db.pointers(page(1)).len(), 1, "the value is placed");
    assert_eq!(db.log.end().expect("the log has a file").file(), next);

    // The turn that fails after one that did not waits the idle time.
    db.commit(&[hash((1, 1), 3)]);
    fill(&fs);
    let mut failed = Vec::new();
    let wait = wb.paced_turn(&mut in_vain, &mut |error| failed.push(error.to_string()));
    assert_eq!(wait, Some(ms(10)), "{failed:?}");
    assert_eq!(failed.len(), 1);
    fs.clear_rules();
    assert_eq!(wb.paced_turn(&mut in_vain, &mut none), None);
    db.check_cold();
}

/// A device with a capacity that has no free byte makes names and
/// refuses their bytes: each turn makes the log's next file, is refused
/// its header and removes the file again, under the same id every time.
/// Those are two changes of that one name for each failed turn, which no
/// directory sync covers, and no other name is changed.
#[test]
fn a_failed_turn_on_a_device_with_no_free_byte_changes_one_name_twice() {
    let config = FlushConfig {
        idle: ms(10),
        max_failed_wait: ms(80),
        ..eager()
    };
    let fs = sim_fs(SimConfig::new(5));
    let db = Db::create(shared(&fs), db_dir(), config, LogOptions::default());
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    assert!(db.log.rotate_early().expect("a turn of the log"));
    let file = db.log.end().expect("the log has a file").file();
    let record = FaultRule::new(Fault::Enospc)
        .on(OpKind::Append)
        .at(FileNames::WAL.path(db_dir(), file));
    fs.add_rule(record.torn_append(false));
    pass_with_a_refused_step(&db);
    assert!(db.log.is_due());
    fs.set_capacity(Some(fs.used_bytes()));

    let wb = db.wb();
    let unsynced = |fs: &SimVfs| -> Vec<String> {
        let image = fs.crash_image();
        image.names().iter().map(ToString::to_string).collect()
    };
    let before = unsynced(&fs);
    let files = log_files(&db);
    let ops = fs.stats().ops;
    let mut in_vain = 0;
    let turns = 12;
    for _ in 0..turns {
        let wait = wb.paced_turn(&mut in_vain, &mut |_| {});
        assert!(wait.is_some(), "a turn on the full device did not fail");
    }
    let ops = fs.stats().ops - ops;
    let after = unsynced(&fs);
    let made: Vec<&String> = after.iter().skip(before.len()).collect();
    println!(
        "{turns} failed turns on a device with no free byte: {ops} file operations, {} unsynced \
         changes of names, the first of them: {:?}",
        made.len(),
        made.iter().take(4).collect::<Vec<_>>()
    );
    assert!(
        made.len() <= 2 * turns,
        "{} changes of names in {turns} failed turns: {made:?}",
        made.len()
    );
    let names: std::collections::BTreeSet<&str> = made
        .iter()
        .filter_map(|change| change.rsplit(' ').next())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    let name = names.first().expect("a name");
    assert!(name.contains("/wal/") && name.ends_with(".log"), "{name}");
    assert_eq!(log_files(&db), files, "a failed turn left a log file");
    assert!(ops <= 8 * turns as u64, "{ops} file operations");

    fs.set_capacity(None);
    let mut none = |error: FlushError| panic!("a turn with room failed: {error}");
    assert_eq!(wb.paced_turn(&mut in_vain, &mut none), None);
    assert_eq!(db.pointers(page(1)).len(), 1, "the value is placed");
    db.check_cold();
}

/// The loop itself, with somebody who asks for a turn all the time: each
/// failed turn is followed by its whole wait.
#[test]
fn the_loop_waits_after_each_failed_turn_whoever_asks_for_a_turn() {
    let config = FlushConfig {
        idle: ms(10),
        max_failed_wait: ms(40),
        ..eager()
    };
    let (_fs, db) = stuck(config);
    waits_between_six_failed_turns(&db, config, true);
}

/// Runs the loop of `db`, whose turns fail and move nothing on, until
/// six have failed, and checks that each was followed by its whole wait.
/// With `asked`, somebody asks for a turn all the time. Without it
/// nobody does but the turns themselves.
fn waits_between_six_failed_turns(db: &Db, config: FlushConfig, asked: bool) {
    let wb = db.wb();
    let ended = Mutex::new(Vec::new());
    let enough = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            wb.run(|_| {
                let mut ended = ended.lock().expect("the turns");
                ended.push(Instant::now());
                if ended.len() == 6 {
                    enough.store(true, Ordering::Release);
                }
            })
        });
        let _stop = StopOnDrop(wb);
        // Six failed turns take a fraction of a second. The two minutes
        // are a watchdog, which keeps a loop whose turns do not fail from
        // hanging the test, and nothing that is checked of the pace: that
        // is the time between the loop's own calls of `failed`, below.
        let began = Instant::now();
        while !enough.load(Ordering::Acquire) {
            assert!(
                began.elapsed() < Duration::from_secs(120),
                "the loop's turns do not fail"
            );
            if asked {
                wb.wake();
            }
            std::thread::yield_now();
        }
    });
    let ended = ended.into_inner().expect("the turns");
    // After the turn that failed as the n-th in a row the loop waited
    // as long as the configuration says, at least.
    for (turn, pair) in ended.windows(2).take(5).enumerate() {
        let waited = pair[1] - pair[0];
        let least = config.wait_after_failure(turn as u32 + 1);
        assert!(
            waited >= least,
            "the turn after failed turn {} came after {waited:?}, and not after {least:?}",
            turn + 1
        );
    }
    assert!(db.stats().turns_put_off >= 5, "{:?}", db.stats());
}

/// The largest id of a log file in the directory.
fn last_log_id(db: &Db) -> u32 {
    let last = log_files(db).pop().expect("a log file");
    let raw = last.trim_end_matches(".log").parse();
    raw.expect("the name of a log file")
}

/// A store in the state of the module's documentation, on a device that
/// writes each record it is given into a log file and answers that it
/// has no room. The log finds such a record whole in its file: an item
/// counts as appended, the file takes no more, and a turn of the log ends
/// in a file that takes no item. Returns what the device is told by.
fn stuck_where_the_device_keeps_what_it_refuses(config: FlushConfig) -> (Arc<Lies>, Db) {
    let fs = sim_fs(SimConfig::new(5));
    let lies = Arc::new(Lies::default());
    let vfs = Arc::new(Swapping {
        inner: shared(&fs),
        lies: lies.clone(),
    });
    let db = Db::create(vfs, db_dir(), config, LogOptions::default());
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    let file = db.log.end().expect("the log has a file").file();
    let record = FaultRule::new(Fault::Enospc)
        .on(OpKind::Append)
        .at(FileNames::WAL.path(db_dir(), file));
    fs.add_rule(record.torn_append(false));
    pass_with_a_refused_step(&db);
    assert_eq!(lock(&db.state.placing).len(), 1, "a value is ready");
    assert!(db.log.is_due(), "the log takes nothing until it is rolled");
    assert!(std::mem::take(&mut *lock(&db.state.wake)));
    *lies.kept_and_refused.lock().expect("the lie") = true;
    (lies, db)
}

/// On a device that keeps the start record it refuses, every turn of a
/// roll is made and ends in a file that takes no item. Such a roll has
/// failed, with what the device answered, and so has the turn of the loop
/// that made it: the loop waits after it, longer each time, as after any
/// turn that failed and moved nothing on. A roll makes a bounded number
/// of turns, so each failed turn of the loop uses that many log files at
/// most, and the wait bounds the turns. With a device that takes records
/// again, the next turn rolls the log and places the value.
#[test]
fn a_roll_that_ends_in_a_file_that_takes_no_item_is_a_turn_that_failed() {
    let config = FlushConfig {
        idle: ms(10),
        max_failed_wait: ms(80),
        ..eager()
    };
    let (lies, db) = stuck_where_the_device_keeps_what_it_refuses(config);
    let wb = db.wb();
    let before = last_log_id(&db);
    let mut errors = Vec::new();
    let mut in_vain = 0;
    let mut waits = Vec::new();
    let turns = 5;
    for _ in 0..turns {
        let wait = wb.paced_turn(&mut in_vain, &mut |error| errors.push(error));
        waits.push(wait.map(|wait| wait.as_millis()));
        // The page step of the turn asked for the next turn, as it does
        // on a device that keeps nothing.
        assert!(std::mem::take(&mut *lock(&db.state.wake)));
    }
    assert_eq!(waits, [Some(10), Some(20), Some(40), Some(80), Some(80)]);
    assert_eq!(errors.len(), turns);
    for error in &errors {
        let FlushError::Log(LogError::NoActiveFileSince { source, .. }) = error else {
            panic!("{error}");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::StorageFull, "{error}");
    }
    assert_eq!(db.stats().errors, turns as u64);
    assert_eq!(lock(&db.state.placing).len(), 1, "the value is ready");
    let used = (last_log_id(&db) - before) as usize;
    println!("{turns} failed turns on a device that keeps what it refuses used {used} log files");
    assert!(used > 0, "no turn of the log was made");
    assert!(used <= turns * ROLL_ROUNDS, "{used} log files");

    *lies.kept_and_refused.lock().expect("the lie") = false;
    let mut none = |error: FlushError| panic!("a turn on a device with room failed: {error}");
    assert_eq!(wb.paced_turn(&mut in_vain, &mut none), None);
    assert_eq!(in_vain, 0);
    assert_eq!(db.pointers(page(1)).len(), 1, "the value is placed");
    db.check_cold();
}

/// The roll alone fails the turn. On the same device a write transaction
/// holds a private copy of the page, so the page step of each turn is
/// held back and no record is offered to the log: the pass is refused
/// nothing. The turn has failed all the same, by its roll, with what
/// the device answered, and is followed by its wait.
#[test]
fn a_roll_that_ends_in_vain_fails_a_turn_whose_pass_is_refused_nothing() {
    let config = FlushConfig {
        idle: ms(10),
        max_failed_wait: ms(80),
        ..eager()
    };
    let (_lies, db) = stuck_where_the_device_keeps_what_it_refuses(config);
    let wb = db.wb();
    let mut txn = db.begin();
    assert!(!txn.copy(page(1)), "the page is there");
    let held_back = db.stats().steps_put_off;
    let mut errors = Vec::new();
    let mut in_vain = 0;
    let mut waits = Vec::new();
    let turns = 3;
    for _ in 0..turns {
        let wait = wb.paced_turn(&mut in_vain, &mut |error| errors.push(error));
        waits.push(wait.map(|wait| wait.as_millis()));
        // No page step was refused its record, so none asked for a turn.
        assert!(!std::mem::take(&mut *lock(&db.state.wake)));
    }
    assert_eq!(waits, [Some(10), Some(20), Some(40)]);
    assert_eq!(errors.len(), turns);
    for error in &errors {
        let FlushError::Log(LogError::NoActiveFileSince { source, .. }) = error else {
            panic!("{error}");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::StorageFull, "{error}");
    }
    let held_back = db.stats().steps_put_off - held_back;
    assert_eq!(
        held_back, turns as u64,
        "the step of each turn was held back"
    );
    assert_eq!(db.state.listed(), (0, 1), "the value is ready");
    txn.roll_back();
}

/// The same device under the loop itself, with somebody who asks for a
/// turn all the time: each turn is followed by its whole wait.
#[test]
fn the_loop_waits_after_a_roll_that_ended_in_a_file_that_takes_no_item() {
    let config = FlushConfig {
        idle: ms(10),
        max_failed_wait: ms(40),
        ..eager()
    };
    let (_lies, db) = stuck_where_the_device_keeps_what_it_refuses(config);
    let before = last_log_id(&db);
    waits_between_six_failed_turns(&db, config, true);
    let failed = db.stats().errors as usize;
    let used = (last_log_id(&db) - before) as usize;
    assert!(
        used <= (failed + 1) * ROLL_ROUNDS,
        "{used} log files in {failed} failed turns"
    );
}

/// A store with a value whose page step is due, on a device that has no
/// room for the record of a page step in a log file, writes nothing of
/// it, and takes everything else: the log's files and their start
/// records, the value, the manifests and every sync. Returns the device
/// and what it is told by.
fn stuck_where_the_device_refuses_the_records_of_page_steps(
    config: FlushConfig,
) -> (SimVfs, Arc<Lies>, Db) {
    let fs = sim_fs(SimConfig::new(5));
    let lies = Arc::new(Lies::default());
    let vfs = Arc::new(Swapping {
        inner: shared(&fs),
        lies: lies.clone(),
    });
    let db = Db::create(vfs, db_dir(), config, LogOptions::default());
    db.commit(&[insert((1, 1), b"a", long(1, 900))]);
    *lies.steps_refused.lock().expect("the lie") = true;
    (fs, lies, db)
}

/// On a device that takes a new log file with its start record and
/// refuses the record of each page step, every roll of the log succeeds
/// and the step of every turn is refused. The step is put off, and the
/// turn has failed by it, with what the log answered: the loop waits
/// after it, longer each time, as after any turn that failed and moved
/// nothing on. Before, such a turn had not failed, and the call for a
/// turn that its own step made brought the next one at once. Each turn
/// that rolls the log uses one log file, so the wait bounds the ids. With
/// a device that takes the record, the next turn places the value.
#[test]
fn a_page_step_whose_record_the_log_refused_is_a_turn_that_failed() {
    let config = FlushConfig {
        idle: ms(10),
        max_failed_wait: ms(80),
        ..eager()
    };
    let (fs, lies, db) = stuck_where_the_device_refuses_the_records_of_page_steps(config);
    let wb = db.wb();
    let before = last_log_id(&db);
    // The names that were changed and that no directory sync covers.
    let unsynced = || fs.crash_image().names().len();
    assert_eq!(unsynced(), 0, "the store begins with such a name");
    let mut errors = Vec::new();
    let mut in_vain = 0;
    let mut waits = Vec::new();
    let turns = 5;
    for _ in 0..turns {
        let wait = wb.paced_turn(&mut in_vain, &mut |error| errors.push(error));
        waits.push(wait.map(|wait| wait.as_millis()));
        // The step of the turn asked for the next turn, with nobody else
        // who asks for one.
        assert!(std::mem::take(&mut *lock(&db.state.wake)));
        assert!(db.log.is_due(), "the log takes nothing until it is rolled");
        assert_eq!(unsynced(), 0, "the turn left such a name");
    }
    assert_eq!(waits, [Some(10), Some(20), Some(40), Some(80), Some(80)]);
    assert_eq!(errors.len(), turns);
    for error in &errors {
        let FlushError::Log(LogError::TakenBack { source, .. }) = error else {
            panic!("{error}");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::StorageFull, "{error}");
    }
    assert_eq!(db.stats().errors, turns as u64);
    assert_eq!(lock(&db.state.placing).len(), 1, "the value is ready");
    assert_eq!(db.pointers(page(1)).len(), 0, "and has no pointer");
    // The first turn found the log in a file that takes items. Every
    // later one rolled the log first, to one file more.
    let used = (last_log_id(&db) - before) as usize;
    println!(
        "{turns} failed turns on a device that refuses the records of page steps used {used} \
         log files"
    );
    assert_eq!(used, turns - 1, "log files");

    *lies.steps_refused.lock().expect("the lie") = false;
    let mut none = |error: FlushError| panic!("a turn on a device with room failed: {error}");
    assert_eq!(wb.paced_turn(&mut in_vain, &mut none), None);
    assert_eq!(in_vain, 0);
    assert_eq!(db.pointers(page(1)).len(), 1, "the value is placed");
    db.check_cold();
}

/// A failed turn that moved something on ends the row of turns that
/// failed and moved nothing on, and is followed by the idle time. Here
/// three turns fail in a row on the device that refuses the records of
/// page steps, and the wait has grown. Then a page with no pending value
/// is committed, in a log file that a commit's thread has turned to, as
/// it does before it commits. The next turn fails by its page step as the
/// ones before it, and writes that page: it has moved something on.
#[test]
fn a_failed_turn_that_moved_something_on_ends_the_row_of_turns_in_vain() {
    let config = FlushConfig {
        idle: ms(10),
        max_failed_wait: ms(80),
        ..eager()
    };
    let (_fs, _lies, db) = stuck_where_the_device_refuses_the_records_of_page_steps(config);
    let wb = db.wb();
    let mut in_vain = 0;
    let mut failed = 0;
    let mut turn = |in_vain: &mut u32| {
        let wait = wb.paced_turn(in_vain, &mut |_| failed += 1);
        wait.map(|wait| wait.as_millis())
    };
    let row: Vec<_> = (0..3).map(|_| turn(&mut in_vain)).collect();
    assert_eq!((row, in_vain), (vec![Some(10), Some(20), Some(40)], 3));

    let written = db.stats().base_blocks + db.stats().delta_blocks;
    {
        let mut manifest = db.manifest.lock().expect("the manifest");
        db.log.roll(&mut manifest).expect("a turn of the log");
    }
    db.commit(&[insert((2, 1), b"b", inline(b"short"))]);
    assert_eq!(turn(&mut in_vain), Some(10), "the turn wrote a page");
    assert_eq!(in_vain, 0);
    let stats = db.stats();
    assert_eq!(stats.base_blocks + stats.delta_blocks, written + 1);
    assert_eq!(lock(&db.state.placing).len(), 1, "the value is ready");

    // The row begins anew with the next turn that moves nothing on.
    let row: Vec<_> = (0..3).map(|_| turn(&mut in_vain)).collect();
    assert_eq!((row, in_vain), (vec![Some(10), Some(20), Some(40)], 3));
    assert_eq!((failed, db.stats().errors), (7, 7));
}

/// The same device under the loop itself, with nobody who asks for a
/// turn but the page step of each turn: each turn is followed by its
/// whole wait, and no turn uses more than one log file.
#[test]
fn the_loop_waits_after_a_turn_whose_page_step_the_log_refused() {
    let config = FlushConfig {
        idle: ms(10),
        max_failed_wait: ms(40),
        ..eager()
    };
    let (_fs, _lies, db) = stuck_where_the_device_refuses_the_records_of_page_steps(config);
    let before = last_log_id(&db);
    waits_between_six_failed_turns(&db, config, false);
    let failed = db.stats().errors as usize;
    let used = (last_log_id(&db) - before) as usize;
    assert!(used <= failed, "{used} log files in {failed} failed turns");
}

/// Whether a sleep of `state` for `wait` ends by itself, within a minute,
/// while `meanwhile` is called again and again. The sleeper is let go
/// whatever is found.
fn ends(state: &FlushState, wait: Option<Duration>, mut meanwhile: impl FnMut()) -> bool {
    std::thread::scope(|scope| {
        let sleeper = scope.spawn(|| state.sleep(wait));
        let began = Instant::now();
        while !sleeper.is_finished() && began.elapsed() < Duration::from_secs(60) {
            meanwhile();
            std::thread::yield_now();
        }
        let ended = sleeper.is_finished();
        let stopping = state.stopping.swap(true, Ordering::AcqRel);
        state.wake();
        sleeper.join().expect("the sleeper");
        state.stopping.store(stopping, Ordering::Release);
        ended
    })
}

/// The sleep after a turn that failed lasts as long as it was told to,
/// whoever asks for a turn before it and during it, and is counted as a
/// turn that was put off. After a turn that did not fail the same call
/// ends the sleep, whose idle time is an hour here. The end of the loop
/// ends the wait after a failed turn too.
#[test]
fn a_call_for_a_turn_ends_no_wait_after_a_failed_turn() {
    let config = FlushConfig {
        idle: Duration::from_secs(3600),
        ..eager()
    };
    let state = FlushState::new(config);
    state.wake();
    let began = Instant::now();
    let asking = AtomicBool::new(true);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while asking.load(Ordering::Acquire) {
                state.wake();
                std::thread::yield_now();
            }
        });
        state.sleep(Some(ms(60)));
        asking.store(false, Ordering::Release);
    });
    assert!(began.elapsed() >= ms(60), "{:?}", began.elapsed());
    assert_eq!(state.stats().turns_put_off, 1);
    // A wait in which nobody asked for a turn put none off.
    *lock(&state.wake) = false;
    state.sleep(Some(ms(1)));
    assert_eq!(state.stats().turns_put_off, 1);

    state.wake();
    assert!(
        ends(&state, None, || {}),
        "a call for a turn ended no sleep"
    );
    assert!(ends(&state, None, || state.wake()), "nor one during it");
    assert_eq!(state.stats().turns_put_off, 1);

    let hour = Some(Duration::from_secs(3600));
    state.stopping.store(true, Ordering::Release);
    assert!(ends(&state, hour, || {}), "the loop was stopped");
}

/// A call that room was made ends the wait after a failed turn: the one
/// that is running when it comes, and the one that begins next where
/// none is. It ends that one sleep and no later one, also where the
/// sleep it ended was one after a turn that did not fail. A wait that it
/// cut short has put no turn off.
#[test]
fn a_call_that_room_was_made_ends_one_wait_after_a_failed_turn() {
    let config = FlushConfig {
        idle: Duration::from_secs(3600),
        ..eager()
    };
    let state = FlushState::new(config);
    let hour = Some(Duration::from_secs(3600));

    // During the wait.
    assert!(
        ends(&state, hour, || state.room_made()),
        "room was made during the wait"
    );
    // Before it: the wait that begins next ends at once.
    state.room_made();
    assert!(ends(&state, hour, || {}), "room was made before the wait");
    assert_eq!(state.stats().turns_put_off, 0);

    // The wait after that lasts as long as it was told to, with nobody
    // who asks for a turn or with somebody. (The helper above asked for
    // one to let its sleeper go.)
    *lock(&state.wake) = false;
    let began = Instant::now();
    state.sleep(Some(ms(60)));
    assert!(began.elapsed() >= ms(60), "{:?}", began.elapsed());
    assert_eq!(state.stats().turns_put_off, 0);
    state.wake();
    let began = Instant::now();
    state.sleep(Some(ms(60)));
    assert!(began.elapsed() >= ms(60), "{:?}", began.elapsed());
    assert_eq!(state.stats().turns_put_off, 1);

    // A sleep after a turn that did not fail ends for the call as for a
    // call for a turn, and the call is used up by it.
    state.room_made();
    assert!(ends(&state, None, || {}), "the idle sleep did not end");
    let began = Instant::now();
    state.sleep(Some(ms(60)));
    assert!(began.elapsed() >= ms(60), "{:?}", began.elapsed());
}

/// A wait that lasted its time has put off the turn that was asked for
/// in it, whatever the loop finds when it leaves the wait: a call that
/// room was made, or the end of the loop. A wait that was left before its
/// time has put none off, and neither has one in which no turn was asked
/// for.
#[test]
fn a_wait_that_lasted_its_time_has_put_a_turn_off_whatever_came_at_its_end() {
    use crate::flush::state::put_off;

    let wait = ms(60);
    assert!(put_off(true, wait, wait));
    assert!(put_off(true, wait + ms(1), wait));
    assert!(!put_off(true, wait - Duration::from_nanos(1), wait));
    assert!(!put_off(false, wait, wait));

    // The sleep itself, with a wait whose time has passed when it
    // begins: a turn was asked for, and room was made.
    let state = FlushState::new(eager());
    state.wake();
    state.room_made();
    state.sleep(Some(Duration::ZERO));
    assert_eq!(state.stats().turns_put_off, 1);
    // No turn was asked for.
    state.room_made();
    state.sleep(Some(Duration::ZERO));
    assert_eq!(state.stats().turns_put_off, 1);
    // A turn was asked for, and the loop is told to end.
    state.wake();
    state.stopping.store(true, Ordering::Release);
    state.sleep(Some(Duration::ZERO));
    assert_eq!(state.stats().turns_put_off, 2);
}

/// The loop itself: its turns fail on a device that stays full, and its
/// waits are an hour. Told that room was made, it takes its next turn at
/// once, each time it is told.
#[test]
fn the_loop_takes_its_next_turn_at_once_when_it_is_told_that_room_was_made() {
    let config = FlushConfig {
        idle: Duration::from_secs(3600),
        max_failed_wait: Duration::from_secs(3600),
        ..eager()
    };
    let (_fs, db) = stuck(config);
    let wb = db.wb();
    let failed = || db.stats().errors;
    let reached = |turns: u64| {
        let began = Instant::now();
        while failed() < turns && began.elapsed() < Duration::from_secs(60) {
            std::thread::yield_now();
        }
        failed() >= turns
    };
    std::thread::scope(|scope| {
        scope.spawn(|| wb.run(|_| {}));
        // The first turn is made at once, and the wait after it is an
        // hour.
        let _stop = StopOnDrop(wb);
        let first = reached(1);
        let mut told = Vec::new();
        for turns in 2..=4 {
            wb.room_made();
            told.push(reached(turns));
        }
        assert!(first, "the loop made no turn");
        assert_eq!(told, [true; 3], "the loop went on waiting");
    });
}
