//! The count of live values of each file, the rules for dropping a file,
//! and recovering the counts.

use lignin_format::value_log::{extent_len, ValuePointer};
use lignin_io::{Create, OpenMode};

use super::{
    sim::*,
    store::{acquired, acquired_counts, bytes, released, Store},
    *,
};

/// A count that says `values` values of `bytes` bytes are live in file 1.
fn stated(values: u64, bytes: u64) -> FileCount {
    FileCount {
        file: file_id(1),
        values,
        bytes,
        miscounted: false,
    }
}

fn count(file: u32, pointers: &[ValuePointer]) -> FileCount {
    FileCount {
        file: file_id(file),
        values: pointers.len() as u64,
        bytes: pointers.iter().map(|pointer| pointer.extent_len(V1)).sum(),
        miscounted: false,
    }
}

/// A store with one closed file that holds `lens.len()` values, none of
/// them counted, and the pointers to them.
fn one_closed_file(lens: &[usize]) -> (Store, Vec<ValuePointer>) {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let pointers = lens
        .iter()
        .enumerate()
        .map(|(at, &len)| {
            writer
                .append(&mut hold, page(1 + at as u32), &bytes(len, at as u8))
                .unwrap()
        })
        .collect();
    writer.finish().unwrap();
    store.commit();
    (store, pointers)
}

fn miscount(result: Result<(), ValueStoreError>) -> (ValuePointer, Miscount) {
    match result {
        Err(ValueStoreError::Miscounted { pointer, why }) => (pointer, why),
        other => panic!("expected a miscount, got {other:?}"),
    }
}

#[test]
fn a_change_counts_the_pointers_cells_start_and_stop_holding() {
    let mut store = Store::new();
    let (mut first, mut second) = (store.values.create().unwrap(), store.writer());
    let mut hold = ValueHold::new();
    let [a, b, c] = [10, 600, 77].map(|len| {
        first
            .append(&mut hold, page(1), &bytes(len, len as u8))
            .unwrap()
    });
    let [d, e] = [5, 3000].map(|len| {
        second
            .append(&mut hold, page(2), &bytes(len, len as u8))
            .unwrap()
    });
    store.cover();
    assert!(store.values.counts().is_empty());
    assert_eq!(store.values.count(file_id(1)), Some(count(1, &[])));
    assert_eq!(store.values.count(file_id(3)), None);

    store.apply(&acquired(&[a, b, d]), 1);
    assert_eq!(store.values.counts(), [count(1, &[a, b]), count(2, &[d])]);

    let mut change = ValueChange::new();
    assert!(change.is_empty());
    change.acquire(c);
    change.acquire(e);
    change.release(a);
    assert!(!change.is_empty());
    assert_eq!(
        (change.acquired(), change.released()),
        (&[c, e][..], &[a][..])
    );
    store.apply(&change, 2);
    assert_eq!(
        store.values.counts(),
        [count(1, &[b, c]), count(2, &[d, e])]
    );

    let moved = ValueChange::moved(b, a);
    assert_eq!((moved.acquired(), moved.released()), (&[a][..], &[b][..]));
    store.apply(&moved, 2);
    assert_eq!(store.values.count(file_id(1)), Some(count(1, &[a, c])));

    store.apply(&released(&[a, c, d]), 3);
    assert_eq!(store.values.counts(), [count(2, &[e])]);
    assert_eq!(store.values.count(file_id(1)), Some(count(1, &[])));
    store.apply(&ValueChange::new(), 4);
    assert_eq!(store.values.counts(), [count(2, &[e])]);
}

#[test]
fn a_pointer_acquired_and_released_in_one_change_leaves_the_count_as_it_was() {
    let (store, pointers) = one_closed_file(&[40, 50]);
    let mut change = acquired(&pointers);
    change.release(pointers[0]);
    change.release(pointers[1]);
    store.apply(&change, 1);
    assert!(store.values.counts().is_empty());
}

#[test]
fn a_release_from_a_count_of_zero_keeps_the_file_for_good() {
    let (mut store, pointers) = one_closed_file(&[40, 50, 60]);
    let [a, b, c] = pointers[..] else {
        unreachable!()
    };
    store.apply(&acquired(&[a]), 1);
    store.apply(&released(&[a]), 2);
    assert_eq!(
        miscount(store.values.apply(&released(&[b]), version(3))),
        (b, Miscount::CountIsZero)
    );
    let wrong = FileCount {
        miscounted: true,
        ..count(1, &[])
    };
    assert_eq!(store.values.counts(), [wrong]);

    store.all_durable();
    assert!(store.drop_empty(100).is_empty());
    assert!(store.exists(1));
    assert_eq!(store.read(b, 2).unwrap(), bytes(50, 1));

    store.apply(&acquired(&[c]), 4);
    assert!(
        store.values.cleanable(0).is_empty(),
        "cleaning cannot empty a file whose count is wrong"
    );
    store.apply(&released(&[c]), 5);
    store.all_durable();
    assert!(store.drop_empty(100).is_empty());

    let counts = store.values.counts();
    let mut store = store.reopen(&counts);
    assert_eq!(store.values.counts(), [wrong]);
    assert!(store.drop_empty(100).is_empty());
    assert!(store.exists(1));
}

#[test]
fn a_change_is_applied_in_full_whatever_it_reports() {
    let (store, pointers) = one_closed_file(&[40, 50, 60]);
    let [a, b, c] = pointers[..] else {
        unreachable!()
    };
    let end = c.end(V1);
    let nowhere = ValuePointer::new(file_id(9), HEADER, 4).unwrap();
    let past = ValuePointer::new(file_id(1), end - 10, 40).unwrap();

    let mut change = ValueChange::new();
    change.acquire(a);
    change.acquire(past);
    change.acquire(nowhere);
    change.acquire(c);
    change.release(nowhere);
    assert_eq!(
        miscount(store.values.apply(&change, version(1))),
        (past, Miscount::OutOfBounds)
    );
    assert_eq!(store.values.counts(), [count(1, &[a, past, c])]);

    assert_eq!(
        miscount(store.values.apply(&acquired(&[nowhere, b]), version(2))),
        (nowhere, Miscount::UnknownFile)
    );
    assert_eq!(
        miscount(store.values.apply(&released(&[nowhere, a]), version(3))),
        (nowhere, Miscount::UnknownFile)
    );
    assert_eq!(store.values.counts(), [count(1, &[past, c, b])]);
}

#[test]
fn a_file_that_is_being_written_is_not_dropped() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    writer.append(&mut hold, page(1), b"never counted").unwrap();
    writer.sync().unwrap();
    drop(hold);
    store.all_durable();
    assert!(store.drop_empty(0).is_empty());

    writer.finish().unwrap();
    assert_eq!(store.drop_empty(0), [1]);
    assert!(!store.exists(1));
}

#[test]
fn a_file_with_a_live_value_is_not_dropped() {
    let (mut store, pointers) = one_closed_file(&[40, 50]);
    store.apply(&acquired(&pointers), 1);
    store.all_durable();
    assert!(store.drop_empty(1).is_empty());

    store.apply(&released(&pointers[..1]), 2);
    store.all_durable();
    assert!(store.drop_empty(2).is_empty());
    assert_eq!(store.read(pointers[1], 2).unwrap(), bytes(50, 1));

    store.apply(&released(&pointers[1..]), 3);
    store.all_durable();
    assert_eq!(store.drop_empty(3), [1]);
}

#[test]
fn a_file_is_not_dropped_before_the_change_that_emptied_it_is_durable() {
    let (mut store, pointers) = one_closed_file(&[40, 50]);
    store.apply(&acquired(&pointers), 1);
    store.apply(&released(&pointers[..1]), 2);
    let before = store.values.applied();
    store.apply(&released(&pointers[1..]), 3);
    let emptied = store.values.applied();
    assert!(before < emptied);
    assert!(store.drop_empty(3).is_empty());

    store.values.mark_durable(before).unwrap();
    assert!(store.drop_empty(3).is_empty());
    assert!(store.exists(1));

    store.values.mark_durable(emptied).unwrap();
    store.values.mark_durable(before).unwrap();
    assert_eq!(store.drop_empty(3), [1]);
}

#[test]
fn a_mark_past_the_changes_applied_makes_no_later_change_durable() {
    let (mut store, pointers) = one_closed_file(&[40]);
    store.apply(&acquired(&pointers), 1);
    let later = ChangeMark {
        change: 99,
        ..store.values.applied()
    };
    store.values.mark_durable(later).unwrap();
    store.apply(&released(&pointers), 2);
    assert!(store.drop_empty(2).is_empty());
    store.all_durable();
    assert_eq!(store.drop_empty(2), [1]);
}

#[test]
fn a_file_outlives_the_pins_on_versions_that_reach_it() {
    let (mut store, pointers) = one_closed_file(&[40, 50]);
    store.apply(&acquired(&pointers), 3);
    store.apply(&released(&pointers[..1]), 5);
    store.apply(&released(&pointers[1..]), 8);
    store.all_durable();
    for pinned in [0, 3, 7] {
        assert!(store.drop_empty(pinned).is_empty(), "a pin on {pinned}");
        assert_eq!(store.read(pointers[1], 2).unwrap(), bytes(50, 1));
    }
    assert_eq!(store.drop_empty(8), [1]);
}

#[test]
fn a_file_that_is_counted_again_became_empty_when_it_last_did() {
    let (mut store, pointers) = one_closed_file(&[40, 50]);
    let [a, b] = pointers[..] else { unreachable!() };
    store.apply(&acquired(&[a]), 1);
    store.apply(&released(&[a]), 2);
    store.all_durable();
    store.apply(&acquired(&[b]), 6);
    assert!(store.drop_empty(6).is_empty());
    store.apply(&released(&[b]), 9);
    store.all_durable();
    assert!(store.drop_empty(8).is_empty());
    assert_eq!(store.drop_empty(9), [1]);
}

#[test]
fn a_file_with_a_value_that_is_held_is_not_dropped() {
    let mut store = Store::new();
    let (mut first, mut second) = (store.values.create().unwrap(), store.writer());
    let (mut one, mut other) = (ValueHold::new(), ValueHold::new());
    first.append(&mut one, page(1), b"held once").unwrap();
    first
        .append(&mut one, page(1), b"by the same hold")
        .unwrap();
    first
        .append(&mut other, page(1), b"and by another")
        .unwrap();
    second.append(&mut other, page(1), b"elsewhere").unwrap();
    assert_eq!(one.files().collect::<Vec<_>>(), [file_id(1)]);
    assert_eq!(other.files().collect::<Vec<_>>(), [file_id(1), file_id(2)]);
    first.finish().unwrap();
    second.finish().unwrap();
    store.all_durable();

    assert!(store.drop_empty(0).is_empty());
    drop(one);
    assert!(store.drop_empty(0).is_empty());
    drop(other);
    assert_eq!(store.drop_empty(0), [1, 2]);
}

#[test]
fn a_dropped_file_is_deleted_once_no_durable_manifest_names_it() {
    let (store, pointers) = one_closed_file(&[40]);
    let named = store.named();
    store.all_durable();
    assert_eq!(
        store
            .values
            .drop_empty(store.values.retire(version(0)))
            .unwrap(),
        [file_id(1)]
    );
    assert!(store.exists(1));
    assert_eq!(store.named(), named, "the durable manifest still names it");
    assert!(matches!(
        store.read(pointers[0], 1),
        Err(ValueStoreError::UnknownFile { .. })
    ));
    assert!(store.values.files_named().unwrap().is_empty());
    assert_eq!(store.values.count(file_id(1)), None);

    store.values.manifest_replaced(&store.manifest).unwrap();
    assert!(store.exists(1), "it is still named");
    let mut store = store;
    store.commit();
    assert!(!store.exists(1));
    assert!(store.named().is_empty());
    store.values.manifest_replaced(&store.manifest).unwrap();
}

#[test]
fn a_session_that_ends_before_the_manifest_is_replaced_leaves_the_file_to_the_next() {
    let (store, _) = one_closed_file(&[40]);
    store.all_durable();
    store
        .values
        .drop_empty(store.values.retire(version(0)))
        .unwrap();
    let mut store = store.reopen(&[]);
    assert!(store.exists(1));
    assert_eq!(store.named().len(), 1);
    store.all_durable();
    assert_eq!(store.drop_empty(0), [1]);
    assert!(!store.exists(1));
}

#[test]
fn a_file_with_no_count_at_open_is_empty() {
    let (store, pointers) = one_closed_file(&[40, 50]);
    let mut store = store.reopen(&acquired_counts(&pointers[1..]));
    assert_eq!(store.values.counts(), [count(1, &pointers[1..])]);
    assert!(store.drop_empty(0).is_empty());

    let mut store = store.reopen(&[]);
    assert!(store.values.counts().is_empty());
    store.all_durable();
    assert_eq!(store.drop_empty(0), [1]);
}

#[test]
fn a_file_that_recovery_left_empty_is_kept_until_what_was_recovered_is_durable() {
    let (store, pointers) = one_closed_file(&[40, 50]);
    let counts = acquired_counts(&pointers);

    let mut opened = store.reopen(&[]);
    assert!(
        opened.drop_empty(9).is_empty(),
        "the checkpoint the counts came from may not be durable yet"
    );
    assert!(opened.exists(1));
    opened.all_durable();
    assert_eq!(opened.drop_empty(9), [1]);

    let (store, _) = one_closed_file(&[40, 50]);
    let mut recovery = ValueStore::recover(&store.manifest, &counts).unwrap();
    recovery.apply(&released(&pointers)).unwrap();
    let mut store = Store {
        values: recovery.finish().unwrap(),
        ..store
    };
    assert!(
        store.drop_empty(9).is_empty(),
        "the log that was replayed may not be durable yet"
    );
    assert_eq!(store.read(pointers[0], 1).unwrap(), bytes(40, 0));

    let mut hold = ValueHold::new();
    let mut writer = store.writer();
    let later = writer.append(&mut hold, page(1), b"later").unwrap();
    writer.finish().unwrap();
    store.commit();
    store.apply(&acquired(&[later]), 4);
    store.apply(&released(&[later]), 5);
    drop(hold);
    let recovered = ChangeMark {
        change: 1,
        ..store.values.applied()
    };
    assert!(recovered < store.values.applied());
    store.values.mark_durable(recovered).unwrap();
    assert_eq!(
        store.drop_empty(9),
        [1],
        "what was recovered is durable, what the session did since is not"
    );
    store.all_durable();
    assert_eq!(store.drop_empty(9), [2]);
}

#[test]
fn a_pointer_that_no_durable_manifest_covers_is_counted_and_reported() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let covered = writer.append(&mut hold, page(1), b"covered").unwrap();
    store.cover();
    let synced = writer.append(&mut hold, page(1), b"synced").unwrap();
    writer.sync().unwrap();
    let appended = writer.append(&mut hold, page(1), b"appended").unwrap();

    store.apply(&acquired(&[covered]), 1);
    for (at, pointer) in [synced, appended].into_iter().enumerate() {
        assert!(!store.values.covers(pointer));
        assert_eq!(
            miscount(store.values.apply(&acquired(&[pointer]), version(2))),
            (pointer, Miscount::NotCovered)
        );
        assert_eq!(
            store.values.count(file_id(1)).map(|count| count.values),
            Some(2 + at as u64)
        );
    }
    store.cover();
    assert!(store.values.covers(synced) && store.values.covers(appended));
    let mut again = released(&[synced]);
    again.acquire(synced);
    store.apply(&again, 3);
}

#[test]
fn recovery_brings_the_counts_to_the_version_it_lands_on() {
    let mut store = Store::new();
    let (mut first, mut second) = (store.values.create().unwrap(), store.writer());
    let mut hold = ValueHold::new();
    let [a, b, c] =
        [10, 600, 77].map(|len| first.append(&mut hold, page(1), &bytes(len, 1)).unwrap());
    let [d, e] = [5, 3000].map(|len| second.append(&mut hold, page(2), &bytes(len, 2)).unwrap());
    first.finish().unwrap();
    second.finish().unwrap();
    store.commit();

    let changes = [
        acquired(&[a, b, d]),
        ValueChange::moved(a, e),
        released(&[b]),
        acquired(&[c]),
    ];
    store.apply(&changes[0], 1);
    let checkpoint = store.values.counts();
    for (at, change) in changes[1..].iter().enumerate() {
        store.apply(change, 2 + at as u64);
    }
    let last = store.values.counts();
    assert_eq!(last, [count(1, &[c]), count(2, &[d, e])]);

    for replayed in 0..changes.len() {
        let mut recovery = ValueStore::recover(&store.manifest, &checkpoint).unwrap();
        for change in &changes[1..=replayed] {
            recovery.apply(change).unwrap();
        }
        let recovered = recovery.finish().unwrap();
        let again = ValueStore::open(&store.manifest, &checkpoint).unwrap();
        for (at, change) in changes[1..=replayed].iter().enumerate() {
            again.apply(change, version(2 + at as u64)).unwrap();
        }
        assert_eq!(recovered.counts(), again.counts(), "{replayed} replayed");
        if replayed == changes.len() - 1 {
            assert_eq!(recovered.counts(), last);
        }
    }
}

#[test]
fn recovery_starts_from_a_checkpoint_older_than_the_deletion_of_a_file() {
    let (mut store, pointers) = one_closed_file(&[40, 50]);
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let kept = writer.append(&mut hold, page(3), b"kept").unwrap();
    writer.finish().unwrap();
    store.commit();
    store.apply(&acquired(&[pointers[0], pointers[1], kept]), 1);
    let checkpoint = store.values.counts();
    let emptying = released(&pointers);
    store.apply(&emptying, 2);
    store.all_durable();
    drop(hold);
    assert_eq!(store.drop_empty(2), [1]);
    assert!(!store.exists(1));

    let mut recovery = ValueStore::recover(&store.manifest, &checkpoint).unwrap();
    recovery.apply(&emptying).unwrap();
    let recovered = recovery.finish().unwrap();
    assert_eq!(recovered.counts(), [count(2, &[kept])]);
    drop(recovered);

    let stray = store.path(1);
    let file = store.fs.open(&stray, OpenMode::Write(Create::New)).unwrap();
    drop(file);
    let recovery = ValueStore::recover(&store.manifest, &checkpoint).unwrap();
    assert!(matches!(
        recovery.finish(),
        Err(ValueStoreError::LiveValuesInMissingFile { file, values: 2 }) if file == file_id(1)
    ));
    assert!(store.exists(1), "a recovery that fails removes nothing");
    assert!(matches!(
        ValueStore::open(&store.manifest, &checkpoint),
        Err(ValueStoreError::LiveValuesInMissingFile { .. })
    ));
}

#[test]
fn recovery_counts_a_value_that_was_acquired_and_released_in_a_file_since_deleted() {
    let (store, _) = one_closed_file(&[40]);
    let gone = ValuePointer::new(file_id(7), HEADER, 9).unwrap();
    let mut recovery = ValueStore::recover(&store.manifest, &[]).unwrap();
    recovery.apply(&acquired(&[gone])).unwrap();
    recovery.apply(&released(&[gone])).unwrap();
    assert!(recovery.finish().unwrap().counts().is_empty());
}

#[test]
fn recovery_reports_a_pointer_the_manifest_does_not_cover() {
    let mut store = Store::new();
    let mut writer = store.writer();
    let mut hold = ValueHold::new();
    let covered = writer.append(&mut hold, page(1), b"covered").unwrap();
    writer.sync().unwrap();
    store.commit();
    let uncovered = writer.append(&mut hold, page(1), b"not covered").unwrap();
    writer.sync().unwrap();
    drop((writer, hold));

    let store = store.reopen(&[]);
    let mut recovery = ValueStore::recover(&store.manifest, &[]).unwrap();
    recovery.apply(&acquired(&[covered])).unwrap();
    assert_eq!(
        miscount(recovery.apply(&acquired(&[uncovered]))),
        (uncovered, Miscount::OutOfBounds)
    );
    let recovered = recovery.finish().unwrap();
    assert_eq!(recovered.counts(), [count(1, &[covered, uncovered])]);
}

#[test]
fn recovery_reports_a_release_from_a_count_of_zero() {
    let (store, pointers) = one_closed_file(&[40, 50]);
    let mut recovery =
        ValueStore::recover(&store.manifest, &acquired_counts(&pointers[..1])).unwrap();
    recovery.apply(&released(&pointers[..1])).unwrap();
    assert_eq!(
        miscount(recovery.apply(&released(&pointers[1..]))),
        (pointers[1], Miscount::CountIsZero)
    );
    let recovered = recovery.finish().unwrap();
    let wrong = FileCount {
        miscounted: true,
        ..count(1, &[])
    };
    assert_eq!(recovered.counts(), [wrong]);
    recovered.mark_durable(recovered.applied()).unwrap();
    assert!(recovered
        .drop_empty(recovered.retire(version(9)))
        .unwrap()
        .is_empty());
}

#[test]
fn the_files_worth_cleaning_are_the_closed_ones_with_the_largest_dead_share() {
    let mut store = Store::new();
    let mut hold = ValueHold::new();
    let mut files = Vec::new();
    for _ in 0..5 {
        let mut writer = store.writer();
        let pointers: Vec<ValuePointer> = (0..10)
            .map(|at| writer.append(&mut hold, page(1), &bytes(82, at)).unwrap())
            .collect();
        files.push((writer, pointers));
    }
    store.cover();
    let len = 10 * 100;
    let live = [10, 7, 2, 0, 5];
    for ((_, pointers), live) in files.iter().zip(live) {
        store.apply(&acquired(&pointers[..live]), 1);
    }
    assert!(
        store.values.cleanable(0).is_empty(),
        "none of them is closed"
    );
    let open = files.pop().expect("the fifth file");
    for (writer, _) in files {
        writer.finish().unwrap();
    }

    let cleanable = |file: u32, live: u64| Cleanable {
        file: file_id(file),
        len,
        live_bytes: live * 100,
    };
    assert_eq!(
        store.values.cleanable(0),
        [cleanable(3, 2), cleanable(2, 7), cleanable(1, 10)]
    );
    assert_eq!(
        store.values.cleanable(1),
        [cleanable(3, 2), cleanable(2, 7)]
    );
    assert_eq!(
        store.values.cleanable(30),
        [cleanable(3, 2), cleanable(2, 7)]
    );
    assert_eq!(store.values.cleanable(31), [cleanable(3, 2)]);
    assert_eq!(store.values.cleanable(80), [cleanable(3, 2)]);
    assert!(store.values.cleanable(81).is_empty());
    assert!(store.values.cleanable(100).is_empty());

    let (writer, pointers) = open;
    store.apply(&released(&pointers[..3]), 2);
    writer.finish().unwrap();
    assert_eq!(
        store.values.cleanable(30),
        [cleanable(3, 2), cleanable(5, 2), cleanable(2, 7)],
        "files with the same dead share come in order of their ids"
    );
}

#[test]
fn a_file_that_became_empty_after_the_caller_stopped_waiting_is_not_dropped() {
    let (mut store, pointers) = one_closed_file(&[40]);
    store.apply(&acquired(&pointers), 1);
    let waited_for = store.values.retire(version(2));
    store.apply(&released(&pointers), 2);
    store.all_durable();

    let dropped = store.values.drop_empty(waited_for).unwrap();
    assert!(
        dropped.is_empty(),
        "an operation that began after the mark may hold a pointer into the file"
    );
    assert_eq!(store.read(pointers[0], 1).unwrap(), bytes(40, 0));
    assert_eq!(store.drop_empty(2), [1]);
}

#[test]
fn a_checkpoint_holds_the_files_it_sampled_until_it_has_linked_them() {
    let (mut store, _) = one_closed_file(&[40]);
    store.all_durable();
    let sampled = store.values.hold_files();
    assert_eq!(sampled.files().collect::<Vec<_>>(), [file_id(1)]);
    assert!(store.drop_empty(0).is_empty());
    drop(sampled);

    store
        .values
        .drop_empty(store.values.retire(version(0)))
        .unwrap();
    let sampled = store.values.hold_files();
    assert_eq!(
        sampled.files().collect::<Vec<_>>(),
        [file_id(1)],
        "a file that was dropped and is still on disk is held too"
    );
    store.commit();
    assert!(store.named().is_empty());
    assert!(
        store.exists(1),
        "the manifest no longer names it, and it stays"
    );
    store.values.manifest_replaced(&store.manifest).unwrap();
    assert!(store.exists(1));

    drop(sampled);
    store.values.manifest_replaced(&store.manifest).unwrap();
    assert!(!store.exists(1));
}

#[test]
fn an_acquisition_does_not_hide_a_release_the_count_did_not_include() {
    let (store, pointers) = one_closed_file(&[40, 50]);
    let [held, new] = pointers[..] else {
        unreachable!()
    };
    let mut change = acquired(&[new]);
    change.release(held);
    assert_eq!(
        miscount(store.values.apply(&change, version(1))),
        (held, Miscount::CountIsZero)
    );
    let wrong = FileCount {
        miscounted: true,
        ..count(1, &[new])
    };
    assert_eq!(store.values.counts(), [wrong]);

    let mut recovery = ValueStore::recover(&store.manifest, &[]).unwrap();
    assert_eq!(
        miscount(recovery.apply(&change)),
        (held, Miscount::CountIsZero)
    );
    assert_eq!(recovery.finish().unwrap().counts(), [wrong]);

    let mut twice = acquired(&[held, held, new]);
    twice.release(held);
    twice.release(new);
    let (store, _) = one_closed_file(&[40, 50]);
    store.apply(&twice, 1);
    assert_eq!(
        store.values.counts(),
        [count(1, &[held])],
        "a pointer released and acquired in one change is taken off both, once for each pair"
    );
}

#[test]
fn recovery_does_not_remove_a_file_whose_count_was_found_too_low() {
    let (store, pointers) = one_closed_file(&[40, 50]);
    store.all_durable();
    store
        .values
        .drop_empty(store.values.retire(version(0)))
        .unwrap();
    let mut store = store;
    let manifest = store.manifest.manifest().unwrap().clone();
    store
        .manifest
        .update(|_| {
            Manifest::new(manifest.last_file_ids(), None, Vec::new()).map_err(|source| {
                ValueStoreError::Format {
                    path: db_dir().to_path_buf(),
                    offset: 0,
                    source,
                }
            })
        })
        .unwrap();
    assert!(store.exists(1), "dropped, no longer named, not yet deleted");

    let mut recovery = ValueStore::recover(&store.manifest, &[]).unwrap();
    assert_eq!(
        miscount(recovery.apply(&released(&pointers[..1]))),
        (pointers[0], Miscount::CountIsZero)
    );
    assert!(matches!(
        recovery.finish(),
        Err(ValueStoreError::MiscountedFileNotNamed { file }) if file == file_id(1)
    ));
    assert!(store.exists(1), "nothing was removed");

    store.fs.remove_file(&store.path(1)).unwrap();
    let mut recovery = ValueStore::recover(&store.manifest, &[]).unwrap();
    recovery.apply(&released(&pointers[..1])).unwrap_err();
    assert!(recovery.finish().unwrap().counts().is_empty());
}

#[test]
fn a_file_is_kept_for_the_pins_there_were_when_the_caller_began_to_wait() {
    let (store, pointers) = one_closed_file(&[40]);
    store.apply(&acquired(&pointers), 1);
    store.apply(&released(&pointers), 2);
    store.all_durable();

    let pinned_at_1 = store.values.retire(version(1));
    assert_eq!(pinned_at_1, store.values.retire(version(1)));
    assert_ne!(pinned_at_1, store.values.retire(version(2)));
    let dropped = store.values.drop_empty(pinned_at_1).unwrap();
    assert!(
        dropped.is_empty(),
        "the snapshot of version 1 has ended since, but an operation that began during the \
         wait may have read the pointer through it"
    );
    assert_eq!(store.read(pointers[0], 1).unwrap(), bytes(40, 0));
    assert!(store.exists(1));

    let after_the_pin = store.values.retire(version(2));
    assert_eq!(
        store.values.drop_empty(after_the_pin).unwrap(),
        [file_id(1)]
    );
}

#[test]
fn a_release_that_the_counted_bytes_do_not_include_marks_the_file() {
    let [short, long, last] = [10, 500, 10];
    let extent = |len: usize| extent_len(len as u32, V1);

    let (store, pointers) = one_closed_file(&[short, long, last]);
    let mut store = store.reopen(&[stated(1, extent(short))]);
    assert_eq!(
        miscount(store.values.apply(&released(&pointers[1..2]), version(2))),
        (pointers[1], Miscount::BytesDisagree),
        "the one value counted is shorter than the one released"
    );
    assert_eq!(
        store.values.count(file_id(1)),
        Some(FileCount {
            miscounted: true,
            ..stated(0, 0)
        })
    );
    store.all_durable();
    assert!(
        store.drop_empty(9).is_empty(),
        "the short value is still live"
    );
    assert_eq!(store.read(pointers[0], 1).unwrap(), bytes(short, 0));

    let (store, pointers) = one_closed_file(&[short, long, last]);
    let store = store.reopen(&[stated(1, extent(short) + 7)]);
    assert_eq!(
        miscount(store.values.apply(&released(&pointers[..1]), version(2))),
        (pointers[0], Miscount::BytesDisagree),
        "the last value goes and leaves bytes counted"
    );
    assert_eq!(live(&store.values, 1), Some(0));
    assert!(store.values.count(file_id(1)).unwrap().miscounted);

    let (store, pointers) = one_closed_file(&[short, long, last]);
    let store = store.reopen(&[stated(2, extent(short) + extent(last))]);
    assert_eq!(
        miscount(store.values.apply(&released(&pointers[1..2]), version(2))),
        (pointers[1], Miscount::BytesDisagree)
    );
    assert_eq!(
        store.values.count(file_id(1)),
        Some(FileCount {
            miscounted: true,
            ..stated(1, 0)
        })
    );

    let (store, pointers) = one_closed_file(&[short, long, last]);
    let store = store.reopen(&[stated(2, extent(short) + extent(long))]);
    store.apply(&released(&pointers[..1]), 2);
    store.apply(&released(&pointers[1..2]), 3);
    assert_eq!(
        store.values.count(file_id(1)),
        Some(stated(0, 0)),
        "a count whose bytes are those of its values goes to zero without complaint"
    );
}

#[test]
fn recovery_marks_a_file_whose_counted_bytes_a_replayed_release_does_not_fit() {
    let (store, pointers) = one_closed_file(&[10, 500]);
    let short = extent_len(10, V1);
    let mut recovery = ValueStore::recover(&store.manifest, &[stated(1, short)]).unwrap();
    assert_eq!(
        miscount(recovery.apply(&released(&pointers[1..]))),
        (pointers[1], Miscount::BytesDisagree)
    );
    let recovered = recovery.finish().unwrap();
    assert_eq!(
        recovered.counts(),
        [FileCount {
            miscounted: true,
            ..stated(0, 0)
        }]
    );
    recovered.mark_durable(recovered.applied()).unwrap();
    let dropped = recovered.drop_empty(recovered.retire(version(9))).unwrap();
    assert!(dropped.is_empty());
}

#[test]
fn a_step_holds_the_counts_still_for_a_sample_or_for_a_change() {
    let (store, pointers) = one_closed_file(&[40, 50]);
    store.apply(&acquired(&pointers[..1]), 1);

    let before = store.values.applied();
    let step = store.values.step().unwrap();
    assert_eq!(step.counts(), [count(1, &pointers[..1])]);
    drop(step);
    assert_eq!(
        store.values.applied(),
        before,
        "a step that applies nothing is no change"
    );

    let step = store.values.step().unwrap();
    step.apply(&acquired(&pointers[1..]), version(2)).unwrap();
    assert_eq!(store.values.counts(), [count(1, &pointers)]);
    assert!(store.values.applied() > before);

    let values = &store.values;
    let step = values.step().unwrap();
    std::thread::scope(|scope| {
        let (read, was_read) = std::sync::mpsc::channel();
        scope.spawn(move || read.send(values.counts()));
        let waited = std::time::Duration::from_millis(50);
        assert!(
            was_read.recv_timeout(waited).is_err(),
            "the counts are not read while a step is taken"
        );
        let moved = ValueChange::moved(pointers[0], pointers[0]);
        step.apply(&moved, version(3)).unwrap();
        assert_eq!(was_read.recv().unwrap(), [count(1, &pointers)]);
    });
}
