//! The cleaner's pass over the value log: which file is cleaned, the two
//! steps of a deletion, what holds a file back, and two kinds of owners.

use std::collections::BTreeSet;

use super::*;
use crate::{
    cleaner::values::{BUSY_PASSES, MAX_BUSY_DOUBLINGS},
    values::{
        Cleaned, OwnersError, PointerRewrite, Rewrite, ValueOwners, ValueStore, ValueStoreError,
    },
};

/// A store whose value-log files are full after two values of 400 bytes.
fn store() -> Db {
    let mut db = Db::with(small_segments());
    let values = {
        let manifest = db.manifest.lock().expect("the manifest");
        ValueStore::open(&manifest, &[]).expect("the value log")
    };
    db.values = values.with_target_file_len(700);
    db
}

fn set(at: (u32, u16), value: Val) -> Op {
    Op::Value {
        page: page(at.0),
        slot: slot(at.1),
        value,
    }
}

/// Four pages with two values each, written back page by page, so that
/// the values of a page lie in a file of their own.
fn filled() -> Db {
    filled_pages(4)
}

/// The same, with `pages` pages.
fn filled_pages(pages: u8) -> Db {
    let db = store();
    for id in 1..=pages {
        db.commit(&[
            insert((u32::from(id), 1), b"a", long(id, 400)),
            insert((u32::from(id), 2), b"b", long(id + 40, 400)),
        ]);
        db.wb().pass().expect("a pass");
    }
    db
}

pub(super) fn value_files(db: &Db) -> Vec<FileId> {
    let named = db.values.files_named().expect("the files");
    named.iter().map(|file| file.id()).collect()
}

pub(super) fn value_on_disk(db: &Db, file: FileId) -> bool {
    let path = crate::segment::FileNames::VALUE_LOG.path(&db.path, file);
    db.vfs.open(&path, lignin_io::OpenMode::Read).is_ok()
}

/// A file that is half dead is cleaned: its live value is moved, and the
/// file goes once the move is durable and nothing can reach it (CL5),
/// which with no operation running is at once. No value changes, at any
/// version a reader can ask for.
#[test]
fn a_value_log_file_is_cleaned_and_goes() {
    let db = filled();
    let before = value_files(&db);
    assert!(before.len() >= 4, "{before:?}");
    // One value of each of three pages is replaced: half of each of their
    // files is dead.
    for id in 1..=3u8 {
        db.commit(&[set((u32::from(id), 2), long(id + 80, 300))]);
        db.wb().pass().expect("a pass");
    }
    let state = CleanerState::new(eager());
    let stats = crate::stats::Stats::new();
    let cleaner = Cleaner::new(db.wb(), &state).with_stats(&stats);
    let space = cleaner.value_space().expect("the space");
    assert!(space.files >= 4 && space.dead_bytes() > 1100, "{space:?}");
    assert!(space.dead_percent() > 20.0 && space.dead_percent() < 60.0);
    assert_eq!(space.bytes, space.live_bytes + space.dead_bytes());
    let live = db.live_values();

    // A pass cleans the worst file, makes the log durable, and drops the
    // file.
    // The owners are used under an operation guard of the pass's own,
    // which is given back before the file is asked for.
    let (durable, guards) = (db.log.durable(), db.versions.guards());
    let first = cleaner.pass_values(&Cells(&db)).expect("a pass");
    let (file, cleaned) = first.cleaned.expect("a file was cleaned");
    assert_eq!((cleaned.moved, cleaned.live), (1, 0), "{cleaned:?}");
    assert!(first.progressed());
    assert_eq!(db.versions.guards(), guards + 1);
    assert_eq!(first.dropped, [file]);
    assert!(db.log.durable().place > durable.place);
    assert!(!value_on_disk(&db, file));
    assert_eq!(db.live_values(), live, "a move changes no count in all");

    let (moved, mut dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!(moved, 2, "the two other files");
    dropped.push(file);
    assert_eq!(dropped.len(), 3, "{dropped:?}");
    for file in &dropped {
        assert!(!value_on_disk(&db, *file), "value-log file {file}");
        assert!(!value_files(&db).contains(file));
    }
    let done = state.stats();
    assert_eq!((done.values_moved, done.value_files_dropped), (3, 3));
    assert_eq!(done.value_files_cleaned, 3);
    assert_eq!(stats.cleaner_bytes(), done.value_bytes_copied);
    assert!(done.value_bytes_copied > 1200);
    assert!(cleaner.value_space().expect("the space").dead_bytes() < space.dead_bytes());
    assert_eq!(db.live_values(), live);
    db.wb().pass().expect("a pass");
    db.check_warm();
    db.check_cold();

    // The next session has the values where the cleaner put them.
    cleaner.finish().expect("the cleaner's files are closed");
    let db = db.reopen();
    db.check_cold();
    assert_eq!(db.live_values(), live);
}

/// With nothing dead, and with dead space within the target, no file is
/// cleaned.
#[test]
fn no_file_is_cleaned_while_the_dead_space_is_within_the_target() {
    let db = filled();
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    assert_eq!(pass, ValuePass::default());
    assert!(!pass.progressed());

    db.commit(&[set((1, 2), long(90, 300))]);
    db.wb().pass().expect("a pass");
    let lenient = CleanerState::new(CleanerConfig {
        value_dead_percent: 60,
        ..eager()
    });
    let pass = (Cleaner::new(db.wb(), &lenient).pass_values(&Cells(&db))).expect("a pass");
    assert_eq!(pass.cleaned, None);
    assert_eq!(lenient.stats().value_passes, 1);
    assert_eq!(lenient.stats().value_files_cleaned, 0);
    // An empty value log has no dead space either.
    let empty = store();
    let none = CleanerState::new(eager());
    let space = Cleaner::new(empty.wb(), &none).value_space().expect("it");
    assert_eq!((space.files, space.dead_percent()), (0, 0.0));
}

/// A file that the commits emptied, with no cleaning, goes the same way.
#[test]
fn a_file_that_commits_emptied_is_dropped() {
    let db = filled();
    let before = value_files(&db);
    db.commit(&[
        set((1, 1), inline(b"short")),
        set((1, 2), inline(b"short too")),
    ]);
    db.wb().pass().expect("a pass");
    let state = CleanerState::new(CleanerConfig {
        value_dead_percent: 100,
        ..eager()
    });
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    assert!(pass.cleaned.is_none(), "{pass:?}");
    assert_eq!(pass.dropped.len(), 1, "{pass:?}");
    assert!(pass.progressed());
    assert_eq!(value_files(&db).len(), before.len() - 1);
    assert!(!value_on_disk(&db, pass.dropped[0]));
    db.check_warm();
}

/// D-209. A store on a simulated device in which write-back was refused
/// an append to the value-log file it fills, with the values of two pages
/// in the file already; with `sync_refused`, the sync of the file at its
/// giving up is refused too. The third value has gone to another file by
/// a later pass. Returns the store and the file that was given up.
fn given_up(sync_refused: bool) -> (Db, FileId) {
    use lignin_io::sim::{Fault, FaultRule, OpKind};

    use crate::{
        segment::{tests::sim::db_dir, FileNames},
        wal::LogOptions,
    };

    let (fs, db) = Db::on_sim(small_segments(), LogOptions::default());
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", long(2, 900)),
        insert((3, 1), b"c", long(3, 900)),
    ]);
    let file = FileId::new(1).expect("a file id");
    let path = FileNames::VALUE_LOG.path(db_dir(), file);
    // The header of the file and two values are appended, and the third
    // value is refused.
    let append = FaultRule::new(Fault::Enospc).on(OpKind::Append).at(&path);
    fs.add_rule(append.skip(3).torn_append(false));
    if sync_refused {
        // The creation of the file syncs its header, and the next sync
        // of the file is the one of its giving up.
        let sync = FaultRule::new(Fault::Enospc).on(OpKind::SyncData).at(&path);
        fs.add_rule(sync.skip(1).forever());
    }
    let refused = db.wb().pass().expect_err("an append to the value log");
    let said = refused.to_string();
    assert!(
        said.contains("append to") && said.contains("00000001.val"),
        "{said}"
    );
    fs.clear_rules();
    assert!(value_files(&db).contains(&file), "the file was made");
    db.wb().pass().expect("a pass on a device with room");
    assert_ne!(db.pointers(page(3))[0].file(), file, "the third value");
    (db, file)
}

/// D-209. A value-log file that write-back gave up after a refused append
/// is cleaned and dropped by the cleaner in the same session: the file
/// was synced through its writer and closed where it ended, so its two
/// values were placed in it, and once one of them is dead the cleaner
/// moves the other out and the file goes, with no close and no open of
/// the store in between.
#[test]
fn a_value_log_file_that_write_back_gave_up_is_cleaned_and_dropped_with_no_reopen() {
    let (db, file) = given_up(false);
    assert_eq!(db.pointers(page(1))[0].file(), file);
    assert_eq!(db.pointers(page(2))[0].file(), file);
    assert_eq!(db.stats().values_given_up, 0);
    let named = db.values.files_named().expect("the files");
    let given_up = named.iter().find(|named| named.id() == file);
    let given_up = given_up.expect("the manifest names the file");
    assert_eq!(given_up.state(), FileState::Closed);
    let path = crate::segment::FileNames::VALUE_LOG.path(&db.path, file);
    let on_device = db.vfs.open(&path, lignin_io::OpenMode::Read);
    let on_device = on_device.and_then(|file| file.len()).expect("a length");
    assert_eq!(given_up.valid_len(), on_device, "nothing of it is unsynced");

    db.commit(&[set((1, 1), inline(b"short"))]);
    db.wb().pass().expect("a pass");
    let live = db.live_values();
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    let (cleaned_file, cleaned) = pass.cleaned.expect("a file was cleaned");
    assert_eq!(cleaned_file, file);
    assert_eq!((cleaned.moved, cleaned.live), (1, 0), "{cleaned:?}");
    assert_eq!(pass.dropped, [file]);
    assert!(!value_on_disk(&db, file));
    assert!(!value_files(&db).contains(&file));
    assert_ne!(db.pointers(page(2))[0].file(), file);
    assert_eq!(db.live_values(), live);
    db.wb().pass().expect("a pass");
    db.check_warm();
    db.check_cold();
}

/// D-209. Where the sync of the file at its giving up is refused as well,
/// the file is closed where it was last durable: none of the values that
/// were appended to it is covered, so no cell is given a pointer into it,
/// and the two are counted as given up and go to another file with the
/// third. The file holds no live value, and the cleaner drops it in the
/// same session. No session ends for that sync: a table checkpoint is
/// installed after it.
#[test]
fn a_value_log_file_given_up_with_its_sync_refused_is_closed_where_it_was_durable_and_dropped() {
    let (db, file) = given_up(true);
    for id in 1..=3 {
        let pointers = db.pointers(page(id));
        assert_eq!(pointers.len(), 1, "page {id}");
        assert_ne!(pointers[0].file(), file, "page {id}");
    }
    assert_eq!(db.stats().values_given_up, 2);
    let named = db.values.files_named().expect("the files");
    let given_up = named.iter().find(|named| named.id() == file);
    let given_up = given_up.expect("the manifest names the file");
    assert_eq!(given_up.state(), FileState::Closed);
    let path = crate::segment::FileNames::VALUE_LOG.path(&db.path, file);
    let on_device = db.vfs.open(&path, lignin_io::OpenMode::Read);
    let on_device = on_device.and_then(|file| file.len()).expect("a length");
    assert!(
        given_up.valid_len() < on_device,
        "{} of {on_device} bytes",
        given_up.valid_len()
    );

    let flush = db.wb().state();
    assert!(!flush.segments_not_durable());
    let installed = db.wb().checkpoint().expect("a table checkpoint");
    assert!(
        matches!(installed, Checkpointed::Installed { .. }),
        "{installed:?}"
    );

    let live = db.live_values();
    assert_eq!(live, 3);
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    assert_eq!(pass.cleaned, None, "{pass:?}");
    assert_eq!(pass.dropped, [file]);
    assert!(!value_on_disk(&db, file));
    assert_eq!(db.live_values(), live);
    db.check_warm();
    db.check_cold();
}

/// D-209, for the cleaner's own file. A cleaning copies two live values
/// into the cleaner's value-log file, and the device refuses the second
/// copy. The cleaner gives its file up: the copy that is in it is synced
/// through the handle that appended it, and the file is closed where it
/// ends. No cell points into it, so it holds no live value, and the
/// passes on a device with room clean the first file into another one
/// and drop both, in the same session.
#[test]
fn the_cleaners_own_value_log_file_is_given_up_after_a_refused_copy_and_dropped() {
    use lignin_io::sim::{Fault, FaultRule, OpKind, SimConfig};

    use crate::{
        flush::tests::handles::Handles,
        segment::{
            tests::sim::{db_dir, shared, sim_fs},
            FileNames,
        },
        wal::LogOptions,
    };

    let fs = sim_fs(SimConfig::new(5));
    let vfs = Handles::over(shared(&fs));
    let db = Db::create(
        vfs.clone(),
        db_dir(),
        small_segments(),
        LogOptions::default(),
    );
    db.commit(&[
        insert((1, 1), b"a", long(1, 900)),
        insert((2, 1), b"b", long(2, 900)),
        insert((3, 1), b"c", long(3, 900)),
    ]);
    db.wb().pass().expect("a pass");
    let first = db.pointers(page(1))[0].file();
    db.wb().close_value_file(first).expect("the file is closed");
    db.commit(&[set((1, 1), inline(b"short"))]);
    db.wb().pass().expect("a pass");
    let live = db.live_values();
    assert_eq!(live, 2);

    // The cleaner's file is the next one of the value log. Its header
    // and the first copy are appended, and the second copy is refused.
    let own = FileId::new(first.get() + 1).expect("a file id");
    let path = FileNames::VALUE_LOG.path(db_dir(), own);
    let name = path.file_name().expect("a name").to_string_lossy();
    let refused = FaultRule::new(Fault::Enospc).on(OpKind::Append).at(&path);
    fs.add_rule(refused.skip(2).torn_append(false));
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let error = cleaner.pass_values(&Cells(&db));
    let error = error.expect_err("a copy is refused");
    assert!(error.to_string().contains(&*name), "{error}");
    fs.clear_rules();
    assert_eq!(state.stats().value_files_given_up, 1);
    assert_eq!(db.pointers(page(2))[0].file(), first, "no value was moved");

    let seen = vfs.seen(&name);
    let appended: Vec<u64> = (seen.iter().filter(|seen| seen.op == "append"))
        .map(|seen| seen.handle)
        .collect();
    assert_eq!(appended.len(), 3, "{seen:?}");
    let last = seen.last().expect("a sync");
    assert_eq!((last.op, last.ok), ("sync", true), "{seen:?}");
    assert!(
        appended.iter().all(|&handle| handle == last.handle),
        "{seen:?}"
    );
    let named = db.values.files_named().expect("the files");
    let given_up = named.iter().find(|named| named.id() == own);
    let given_up = given_up.expect("the manifest names the cleaner's file");
    assert_eq!(given_up.state(), FileState::Closed);

    let (moved, dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!(moved, 2);
    assert!(
        dropped.contains(&first) && dropped.contains(&own),
        "{dropped:?}"
    );
    assert!(!value_on_disk(&db, first) && !value_on_disk(&db, own));
    assert_eq!(db.live_values(), live);
    db.wb().pass().expect("a pass");
    db.check_warm();
    db.check_cold();
}

/// **Test T-PN5, for a value-log file.** A file that a cleaning emptied is
/// not deleted while an operation that was running then is running: it
/// may have read a pointer into the file.
#[test]
fn an_emptied_file_waits_for_the_operations_that_were_running() {
    let db = filled();
    db.commit(&[set((1, 2), long(81, 300))]);
    db.wb().pass().expect("a pass");
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    // An operation reads the pointer of the value that is about to move.
    let old = db.pointer(page(1), slot(1));
    db.versions.hold_guard();
    let first = cleaner.pass_values(&Cells(&db)).expect("a pass");
    let (file, cleaned) = first.cleaned.expect("a file was cleaned");
    assert_eq!((file, cleaned.moved), (old.file(), 1));
    assert_ne!(db.pointer(page(1), slot(1)), old);
    for _ in 0..12 {
        let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
        assert!(pass.dropped.is_empty(), "an operation is still running");
    }
    // The operation follows its pointer, and finds the value.
    let mut buf = Vec::new();
    let read = db.values.read(old, page(1), &mut buf).expect("the value");
    assert_eq!(
        read.to_vec(),
        (0..400)
            .map(|at| 1u8.wrapping_add(at as u8))
            .collect::<Vec<_>>()
    );
    assert!(value_on_disk(&db, file));

    db.versions.release_guard();
    let (_, dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!(dropped, [file]);
    assert!(db.values.read(old, page(1), &mut buf).is_err());
    db.check_warm();
}

/// A snapshot from before a commit that superseded the page reads the old
/// pointer from the page's older version, so the file that held the value
/// stays while the snapshot does (`value-log-format.md` O8).
#[test]
fn an_emptied_file_waits_for_a_pin_on_a_version_that_points_into_it() {
    let db = filled();
    db.commit(&[set((1, 2), long(81, 300))]);
    db.wb().pass().expect("a pass");
    let old = db.pointer(page(1), slot(1));
    db.versions.pin(db.latest());
    // A commit supersedes the page: the snapshot reads the version before.
    db.commit(&[hash((1, 1), 7)]);
    db.wb().pass().expect("a pass");

    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let (moved, dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!((moved, dropped.len()), (1, 0), "{dropped:?}");
    assert_ne!(db.pointer(page(1), slot(1)), old);
    for _ in 0..4 {
        assert!((cleaner.pass_values(&Cells(&db)).expect("a pass").dropped).is_empty());
    }
    // The snapshot reads its version from the segments, with the old
    // pointer, and the value is there.
    db.wb().pass().expect("a pass");
    db.check_cold();
    assert!(value_on_disk(&db, old.file()));

    db.versions.unpin();
    let (_, dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!(dropped, [old.file()]);
    db.check_cold();
}

/// A file that waits to be deleted is dead space that no cleaning gives
/// back: while a pin keeps it, no other file is cleaned on its account.
#[test]
fn no_file_is_cleaned_for_the_sake_of_one_that_waits_to_be_deleted() {
    let db = filled();
    // Two files are half dead, and a snapshot reads the versions of their
    // pages that the commits after it supersede.
    db.commit(&[set((1, 2), long(81, 300)), set((2, 2), long(82, 300))]);
    db.wb().pass().expect("a pass");
    db.versions.pin(db.latest());
    db.commit(&[hash((1, 1), 7), hash((2, 1), 7)]);
    db.wb().pass().expect("a pass");

    let state = CleanerState::new(CleanerConfig {
        value_dead_percent: 15,
        ..eager()
    });
    let cleaner = Cleaner::new(db.wb(), &state);
    let before = cleaner.value_space().expect("the space");
    assert!(
        before.dead_percent() > 15.0 && before.waiting_bytes == 0,
        "{before:?}"
    );

    // One of them is cleaned, and waits for the snapshot. With it, more
    // than the target is dead still; without it, less is, and the other
    // half-dead file is left as it is.
    let (moved, dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!((moved, dropped.len()), (1, 0), "{dropped:?}");
    let waiting = cleaner.value_space().expect("the space");
    assert!(waiting.dead_percent() > 15.0, "{waiting:?}");
    assert!(waiting.waiting_bytes > 700, "{waiting:?}");
    let helped = (waiting.dead_bytes() - waiting.waiting_bytes) as f64 * 100.0
        / (waiting.bytes - waiting.waiting_bytes) as f64;
    assert!(helped > 1.0 && helped <= 15.0, "{helped}: {waiting:?}");
    for _ in 0..40 {
        let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
        assert!(
            pass.cleaned.is_none() && pass.dropped.is_empty(),
            "{pass:?}"
        );
    }
    assert_eq!(state.stats().value_files_cleaned, 1);

    // The snapshot goes, and so does the file. Nothing else is cleaned.
    db.versions.unpin();
    let (moved, dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!((moved, dropped.len()), (0, 1), "{dropped:?}");
    let after = cleaner.value_space().expect("the space");
    assert_eq!(after.waiting_bytes, 0);
    assert!(after.dead_percent() <= 15.0, "{after:?}");
    db.check_warm();
}

/// A move is put off while a write transaction holds a private copy of
/// the page (PV7), and made by a later pass.
#[test]
fn a_move_under_a_private_copy_is_put_off_and_made_later() {
    let db = filled();
    db.commit(&[set((1, 2), long(81, 300))]);
    db.wb().pass().expect("a pass");
    let old = db.pointer(page(1), slot(1));
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);

    let mut txn = db.begin();
    txn.copy(page(1));
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    let (file, cleaned) = pass.cleaned.expect("a file was looked at");
    assert_eq!((cleaned.moved, cleaned.busy, cleaned.live), (0, 1, 1));
    assert!(!pass.progressed());
    assert_eq!(db.pointer(page(1), slot(1)), old);
    assert_eq!(state.stats().values_left, 1);
    // The copy that nothing points to was written, and is counted.
    assert_eq!(state.stats().value_bytes_copied, cleaned.copied_bytes);
    assert!(cleaned.copied_bytes > 400);

    // While the copy is held, the file is set aside for a few passes:
    // each try appends the value again. Then it is tried, for nothing,
    // and set aside again, for twice the delay.
    let set_aside = |tries: u64, wait: u64| {
        for _ in 1..wait {
            let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
            assert!(pass.cleaned.is_none(), "{pass:?}");
        }
        let did = state.stats();
        assert_eq!(did.value_bytes_copied, tries * cleaned.copied_bytes);
        assert_eq!((did.value_files_cleaned, did.values_left), (tries, tries));
    };
    set_aside(1, BUSY_PASSES);
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    assert_eq!(pass.cleaned, Some((file, cleaned)));
    set_aside(2, 2 * BUSY_PASSES);

    txn.roll_back();
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    let (tried, again) = pass.cleaned.expect("the file's wait is over");
    assert_eq!((tried, again.moved), (file, 1));
    // The file goes, and so does the one that took the copy nobody
    // points to.
    let (moved, dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!(moved, 0);
    assert!(
        pass.dropped
            .iter()
            .chain(&dropped)
            .any(|gone| *gone == file),
        "{dropped:?}"
    );
    assert_ne!(db.pointer(page(1), slot(1)), old);
    db.check_warm();
}

/// Files whose values cannot be moved, because a write transaction holds
/// copies of their pages, take no pass from a file that can be cleaned.
/// Each of them is set aside when it was tried, not to be taken again
/// before [`BUSY_PASSES`] passes later, and twice that delay after each
/// further try in vain, however many are set aside with it, and a pass in
/// which every file is set aside is counted.
#[test]
fn files_with_busy_pages_keep_no_other_file_from_being_cleaned() {
    const PAGES: u8 = 6;
    let db = filled_pages(PAGES);
    for id in 1..=PAGES {
        db.commit(&[set((u32::from(id), 2), long(id + 80, 300))]);
        db.wb().pass().expect("a pass");
    }
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    // The first value of each page, which lives in a file half of which
    // is dead: whether it is where it was before the cleaner began.
    let pointers = || -> Vec<_> {
        (1..=PAGES)
            .map(|id| db.pointer(page(u32::from(id)), slot(1)))
            .collect()
    };
    let old = pointers();
    let unmoved = || -> Vec<bool> {
        let now = pointers();
        old.iter().zip(&now).map(|(old, now)| old == now).collect()
    };
    let busy = u64::from(PAGES) - 1;
    let mut txn = db.begin();
    for id in 1..PAGES {
        txn.copy(page(u32::from(id)));
    }

    // The files that a pass tried for nothing.
    let tried_in_vain = || -> Vec<FileId> {
        let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
        let in_vain = |(_, cleaned): &&(FileId, Cleaned)| cleaned.moved == 0 && cleaned.busy != 0;
        let cleaned = pass.cleaned.iter().chain(&pass.swept);
        cleaned.filter(in_vain).map(|(file, _)| *file).collect()
    };

    // Each file is tried once: five for nothing, and the one whose page
    // nobody holds is cleaned, in the same pass as they are if it comes
    // after them.
    let set_aside: BTreeSet<FileId> = (0..PAGES).flat_map(|_| tried_in_vain()).collect();
    assert_eq!(set_aside.len() as u64, busy, "{set_aside:?}");
    let did = state.stats();
    // The value of the free page is moved, and with the passes going on
    // past the files tried for nothing, the copy of a cleaning before may
    // be moved again in them.
    assert_eq!(did.values_left, busy, "{did:?}");
    assert!(did.values_moved >= 1, "{did:?}");
    assert_eq!(unmoved(), [true, true, true, true, true, false]);

    // From then on each of the five is tried again after BUSY_PASSES
    // passes, then twice and four times that: three times in the next 80
    // passes. A pass in which none is due tries none of them and is
    // counted. The others clean the files that the tries filled with
    // copies nothing points to.
    let passes = 10 * BUSY_PASSES;
    let all_busy = state.stats().value_passes_all_busy;
    let mut again = BTreeMap::new();
    let mut trying = 0u64;
    for _ in 0..passes {
        let tried = tried_in_vain();
        trying += u64::from(!tried.is_empty());
        for file in tried {
            *again.entry(file).or_insert(0u64) += 1;
        }
    }
    let idle = state.stats().value_passes_all_busy - all_busy;
    assert!(
        idle > 0 && trying + idle <= passes,
        "{trying} passes that tried and {idle} idle passes"
    );
    assert!(again.keys().eq(set_aside.iter()), "{again:?}");
    assert!(again.values().all(|tries| *tries == 3), "{again:?}");
    assert_eq!(unmoved(), [true, true, true, true, true, false]);

    // Each of the five is tried again when its time has come, and moved.
    txn.roll_back();
    for _ in 0..BUSY_PASSES << MAX_BUSY_DOUBLINGS {
        cleaner.pass_values(&Cells(&db)).expect("a pass");
    }
    settle_values(&cleaner, &Cells(&db));
    assert_eq!(unmoved(), [false; 6]);
    db.check_warm();
}

/// A file of which less than one byte in a hundred is dead is not worth
/// a cleaning: its live values would be appended again for almost
/// nothing. With a target of nothing dead it is cleaned all the same, or
/// that target could not be met.
#[test]
fn a_file_that_is_almost_all_live_is_cleaned_only_for_a_target_of_nothing_dead() {
    let mut db = Db::with(small_segments());
    let values = {
        let manifest = db.manifest.lock().expect("the manifest");
        ValueStore::open(&manifest, &[]).expect("the value log")
    };
    db.values = values.with_target_file_len(1 << 20);
    // 130 values on 26 pages in one file, which is then closed.
    for id in 1..=26u8 {
        let cells: Vec<Op> = (1..=5u8)
            .map(|at| {
                let value = long(id.wrapping_mul(7).wrapping_add(at), 400);
                insert((u32::from(id), u16::from(at)), &[b'k', at], value)
            })
            .collect();
        db.commit(&cells);
        db.wb().pass().expect("a pass");
    }
    let files = value_files(&db);
    assert_eq!(files.len(), 1, "{files:?}");
    let almost_live = files[0];
    assert!(db.wb().close_value_file(almost_live).expect("closed"));
    // One of its values is replaced, and three times the value that
    // replaced it, in the file that is being filled: most of what is dead
    // is there, where no cleaning reaches it.
    for round in 0..4u8 {
        db.commit(&[set((1, 1), long(200 + round, 300))]);
        db.wb().pass().expect("a pass");
    }
    // A cleaner that closes no file.
    let with_target = |value_dead_percent| CleanerConfig {
        value_dead_percent,
        seal_dead_percent: 101,
        ..eager()
    };

    let state = CleanerState::new(with_target(1));
    let cleaner = Cleaner::new(db.wb(), &state);
    let space = cleaner.value_space().expect("the space");
    assert!(space.dead_percent() > 1.0, "{space:?}");
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    assert_eq!(pass, ValuePass::default());
    assert_eq!(value_files(&db).len(), 2);

    let state = CleanerState::new(with_target(0));
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    let (file, cleaned) = pass.cleaned.expect("a cleaning");
    assert_eq!((file, cleaned.moved, cleaned.live), (almost_live, 129, 0));
    db.check_warm();
}

/// **Test T-CK3, for the value log.** A checkpoint that is being created
/// holds the value-log files, and none is deleted until it lets go.
#[test]
fn an_emptied_file_waits_for_a_checkpoint_that_holds_the_files() {
    let db = filled();
    db.commit(&[set((1, 2), long(81, 300))]);
    db.wb().pass().expect("a pass");
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let hold = db.values.hold_files();
    let (moved, dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!((moved, dropped.len()), (1, 0));
    for _ in 0..4 {
        assert!((cleaner.pass_values(&Cells(&db)).expect("a pass").dropped).is_empty());
    }
    // Nothing tells the cleaner that the hold is gone: it asks again
    // after a while.
    drop(hold);
    let mut dropped = Vec::new();
    for _ in 0..40 {
        dropped.extend(cleaner.pass_values(&Cells(&db)).expect("a pass").dropped);
    }
    assert_eq!(dropped.len(), 1);
    assert!(!value_on_disk(&db, dropped[0]));
    db.check_warm();
}

/// Owners that know the pages on one side of a line only: what one kind
/// of owner is to the values of the other kind.
struct Side<'d> {
    cells: Cells<'d>,
    odd: bool,
}

impl ValueOwners for Side<'_> {
    type Found = (SlotId, CommitVersion);

    fn find(&self, id: PageId, pointer: ValuePointer) -> Result<Option<Self::Found>, OwnersError> {
        match (id.get() % 2 == 1) == self.odd {
            true => self.cells.find(id, pointer),
            false => Ok(None),
        }
    }

    fn rewrite(
        &self,
        step: PointerRewrite<'_>,
        found: Self::Found,
    ) -> Result<Rewrite, OwnersError> {
        assert_eq!(step.page().get() % 2 == 1, self.odd, "another's value");
        self.cells.rewrite(step, found)
    }
}

/// Two of the four pages have a value replaced: the file of each is half
/// dead, and holds one live value.
fn two_half_dead() -> Db {
    let db = filled();
    for id in 1..=2u8 {
        db.commit(&[set((u32::from(id), 2), long(id + 80, 300))]);
        db.wb().pass().expect("a pass");
    }
    db
}

/// The cleaner needs every kind of owner: with one kind alone the values
/// of the other are taken for values with no owner, and their file is
/// never emptied. With both, each finds its own.
#[test]
fn both_kinds_of_owners_are_asked() {
    // The owners of odd pages alone: the file of page 2 is looked at, its
    // value has no owner, and the cleaning says so. The file is not
    // looked at again while its count stands.
    let db = two_half_dead();
    let odd = Side {
        cells: Cells(&db),
        odd: true,
    };
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let (mut moved, mut not_found) = (0, 0);
    for _ in 0..8 {
        match cleaner.pass_values(&odd) {
            Ok(pass) => {
                let cleaned = pass.cleaned.iter().chain(&pass.swept);
                moved += cleaned.map(|(_, cleaned)| cleaned.moved).sum::<u64>();
            }
            Err(CleanerError::Values(ValueStoreError::OwnersNotFound { cleaned, .. })) => {
                // Neither the value that died nor the one that lives has
                // an owner on this side.
                assert_eq!((cleaned.moved, cleaned.unowned, cleaned.live), (0, 2, 1));
                not_found += 1;
            }
            Err(error) => panic!("{error}"),
        }
    }
    assert_eq!((moved, not_found), (1, 1), "each file is looked at once");
    assert_eq!(state.stats().errors, 1);
    assert_eq!(state.stats().value_files_cleaned, 2);
    let left = db.pointer(page(2), slot(1));
    assert!(value_on_disk(&db, left.file()));
    db.check_warm();

    // Both kinds: each value is found by the owners of its side.
    let db = two_half_dead();
    let side = |odd| Side {
        cells: Cells(&db),
        odd,
    };
    let both = BothOwners(side(true), side(false));
    let found = (both.find(page(1), db.pointer(page(1), slot(1)))).expect("asked");
    assert!(matches!(found, Some(FoundBy::First(_))), "{found:?}");
    let found = (both.find(page(2), db.pointer(page(2), slot(1)))).expect("asked");
    assert!(matches!(found, Some(FoundBy::Second(_))), "{found:?}");
    let nowhere = both.find(page(77), left).expect("asked");
    assert_eq!(nowhere, None);

    let before = [1, 2].map(|id| db.pointer(page(id), slot(1)));
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let (moved, dropped) = settle_values(&cleaner, &both);
    assert_eq!(moved, 2);
    assert_eq!(state.stats().errors, 0);
    for (id, old) in [1, 2].into_iter().zip(before) {
        assert_ne!(db.pointer(page(id), slot(1)), old);
        assert!(dropped.contains(&old.file()), "{dropped:?}");
    }
    db.check_warm();
    // A reference to both is owners too.
    let by_reference: &dyn ValueOwners<Found = _> = &&both;
    assert_eq!(by_reference.find(page(77), left).expect("asked"), None);
}

/// The log is made durable before a file is given up, and once for the
/// changes that emptied it, not once a pass.
#[test]
fn the_log_is_synced_once_for_the_moves_that_emptied_a_file() {
    let db = filled();
    db.commit(&[set((1, 2), long(81, 300))]);
    db.wb().pass().expect("a pass");
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    db.versions.hold_guard();
    let before = db.log.durable();
    cleaner.pass_values(&Cells(&db)).expect("a pass");
    let after = db.log.durable();
    assert!(after.place > before.place, "{before:?} {after:?}");
    // While the file waits, further passes sync nothing and ask nothing.
    let epoch = db.versions.next_epoch();
    for _ in 0..5 {
        cleaner.pass_values(&Cells(&db)).expect("a pass");
    }
    assert_eq!(db.log.durable(), after);
    assert_eq!(db.versions.next_epoch().get(), epoch.get() + 1);
    db.versions.release_guard();
    let (_, dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!(dropped.len(), 1);
}

/// **What a cleaning found is what the pass ends with, also when the step
/// behind it fails.** A commit emptied one file, and the live value of
/// another, which is half dead, has no owner among the owners given. The
/// cleaning says so, the pass goes on to the emptied file, and the sync
/// of the log that a deletion waits for fails. The file whose owners were
/// not found is not looked at again while its count stands, so the pass
/// that looked at it has to name it.
#[test]
fn owners_that_were_not_found_are_not_replaced_by_a_later_failure() {
    use lignin_io::sim::{Fault, FaultRule, OpSet, SimConfig};

    use crate::segment::{tests::sim::sim_fs, FileNames};

    let fs = sim_fs(SimConfig::new(5));
    let db = super::crash::new_store(&fs);
    for id in 1..=4u8 {
        db.commit(&[
            insert((u32::from(id), 1), b"a", long(id, 400)),
            insert((u32::from(id), 2), b"b", long(id + 40, 400)),
        ]);
        db.wb().pass().expect("a pass");
    }
    db.commit(&[
        set((1, 1), inline(b"short")),
        set((1, 2), inline(b"short too")),
    ]);
    db.wb().pass().expect("a pass");
    db.commit(&[set((2, 2), long(82, 300))]);
    db.wb().pass().expect("a pass");
    let half_dead = db.pointer(page(2), slot(1)).file();
    // A commit that no sync has made durable, in a log whose syncs fail.
    db.commit(&[insert((5, 1), b"k", inline(b"v"))]);
    let log = db.log.end().expect("the log has a file").file();
    fs.add_rule(
        FaultRule::new(Fault::Eio)
            .on(OpSet::SYNCS)
            .at(FileNames::WAL.path(&db.path, log))
            .forever(),
    );

    let odd = Side {
        cells: Cells(&db),
        odd: true,
    };
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let faults = fs.stats().faults;
    let error = (cleaner.pass_values(&odd)).expect_err("the owners are not found");
    assert!(
        fs.stats().faults > faults,
        "the pass asked for a sync of the log, and it failed"
    );
    match error {
        CleanerError::Values(ValueStoreError::OwnersNotFound { file, .. }) => {
            assert_eq!(file, half_dead);
        }
        other => panic!("the pass ended with: {other}"),
    }
}

/// Owners that, around the looks at one value, have a commit to its page
/// made on another thread and wait for it to end: once after the look the
/// cleaner makes before its copies, so that the page it steps on has been
/// superseded, and once in the look of its first try again.
struct Hot<'d> {
    cells: Cells<'d>,
    target: ValuePointer,
    looks: std::cell::Cell<u32>,
    commits: std::cell::Cell<u8>,
}

impl Hot<'_> {
    /// A commit to the target's page on another thread, which must end
    /// while this thread waits inside a look of the cleaner.
    fn commit_beside(&self) {
        let db = self.cells.0;
        let page = self.target_page();
        let round = self.commits.get() + 1;
        let (sent, ended) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                db.commit(&[Op::Hash {
                    page,
                    slot: slot(2),
                    byte: round,
                }]);
                let _ = sent.send(());
            });
            if ended
                .recv_timeout(std::time::Duration::from_secs(60))
                .is_err()
            {
                eprintln!("a commit to the page was held up by the cleaner's look");
                std::process::abort();
            }
        });
        self.commits.set(round);
    }

    fn target_page(&self) -> PageId {
        page(1)
    }
}

impl ValueOwners for Hot<'_> {
    type Found = <Cells<'static> as ValueOwners>::Found;

    fn find(&self, id: PageId, pointer: ValuePointer) -> Result<Option<Self::Found>, OwnersError> {
        if pointer != self.target {
            return self.cells.find(id, pointer);
        }
        let look = self.looks.get() + 1;
        self.looks.set(look);
        match look {
            1 => {
                let found = self.cells.find(id, pointer);
                self.commit_beside();
                found
            }
            2 => {
                self.commit_beside();
                self.cells.find(id, pointer)
            }
            _ => self.cells.find(id, pointer),
        }
    }

    fn rewrite(
        &self,
        step: PointerRewrite<'_>,
        found: Self::Found,
    ) -> Result<Rewrite, OwnersError> {
        self.cells.rewrite(step, found)
    }
}

/// A page that a commit supersedes between the cleaner's look and its
/// step, as the pages that most commits change are while commits come
/// back to back: the step is refused, and tried again with a fresh look
/// ([`crate::values::BUSY_RETRIES`]), which moves the value. The look of
/// the try again holds no lock of the store that a commit takes: a commit
/// to the same page, made on another thread from inside that look, ends
/// while the cleaner waits for it.
#[test]
fn a_commit_to_a_page_the_cleaner_tries_again_is_not_held_up() {
    let db = filled();
    db.commit(&[set((1, 2), long(81, 300))]);
    db.wb().pass().expect("a pass");
    let old = db.pointer(page(1), slot(1));
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let owners = Hot {
        cells: Cells(&db),
        target: old,
        looks: 0.into(),
        commits: 0.into(),
    };
    let pass = cleaner.pass_values(&owners).expect("a pass");
    assert_eq!(
        (owners.looks.get(), owners.commits.get()),
        (2, 2),
        "{pass:?}"
    );
    let busy: u64 = pass
        .cleaned
        .iter()
        .chain(&pass.swept)
        .map(|(_, cleaned)| cleaned.busy)
        .sum();
    assert_eq!(busy, 0, "{pass:?}");
    let now = db.pointer(page(1), slot(1));
    assert_ne!(now.file(), old.file(), "the value moved on the try again");
    db.check_warm();
}
