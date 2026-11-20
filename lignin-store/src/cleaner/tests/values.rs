//! The cleaner's pass over the value log: which file is cleaned, the two
//! steps of a deletion, what holds a file back, and two kinds of owners.

use super::*;
use crate::values::{ValueStore, ValueStoreError};

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
    let db = store();
    for id in 1..=4u8 {
        db.commit(&[
            insert((u32::from(id), 1), b"a", long(id, 400)),
            insert((u32::from(id), 2), b"b", long(id + 40, 400)),
        ]);
        db.wb().pass().expect("a pass");
    }
    db
}

fn value_files(db: &Db) -> Vec<FileId> {
    let named = db.values.files_named().expect("the files");
    named.iter().map(|file| file.id()).collect()
}

fn value_on_disk(db: &Db, file: FileId) -> bool {
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

    // While the copy is held, the file is left alone for a few passes:
    // each try would append the value again.
    for _ in 1..crate::cleaner::values::BUSY_PASSES {
        let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
        assert!(pass.cleaned.is_none(), "{pass:?}");
    }
    assert_eq!(state.stats().value_bytes_copied, cleaned.copied_bytes);
    let again = cleaner.pass_values(&Cells(&db)).expect("a pass");
    let (tried, busy) = again.cleaned.expect("the file is tried again");
    assert_eq!((tried, busy.moved, busy.busy), (file, 0, 1));
    for _ in 1..crate::cleaner::values::BUSY_PASSES {
        cleaner.pass_values(&Cells(&db)).expect("a pass");
    }

    txn.roll_back();
    // The file goes, and so does the one that took the copy nobody
    // points to.
    let (moved, dropped) = settle_values(&cleaner, &Cells(&db));
    assert_eq!(moved, 1);
    assert!(dropped.contains(&file), "{dropped:?}");
    assert_ne!(db.pointer(page(1), slot(1)), old);
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
            Ok(pass) => moved += pass.cleaned.map_or(0, |(_, cleaned)| cleaned.moved),
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
