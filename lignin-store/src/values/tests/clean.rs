//! Cleaning: moving the live values of a file and rewriting their owners'
//! pointers, against the model of pages.

use std::cell::RefCell;

use lignin_format::{
    value_log::{ValueLogError, ValuePointer},
    FormatError,
};
use lignin_io::{sim::SimConfig, OpenMode};

use super::{
    pages::{commit_prepared, Db, PageStore},
    run::{check_recovered, put, Op, Run, Tally},
    sim::*,
    store::{acquired, bytes, released, three_values, Store},
    *,
};

fn started(seed: u64) -> Run {
    Run::started(&sim_fs(SimConfig::new(seed)), seed)
}

fn pointer_of(db: &Db, page_id: u32, key: u32) -> ValuePointer {
    let cells = db.pages.cells(page(page_id));
    let cell = cells.iter().find(|cell| cell.key == key).expect("the cell");
    cell.pointer
}

#[test]
fn cleaning_never_changes_a_logical_value() {
    let mut all = Tally::default();
    for seed in [41, 42, 43, 44] {
        let mut run = started(seed);
        for _ in 0..1200 {
            run.step().expect("nothing fails");
            run.check_session();
        }
        let tally = run.tally.clone();
        println!("seed {seed}: {tally:?}");
        all.commits += tally.commits;
        all.rollbacks += tally.rollbacks;
        all.passes += tally.passes;
        all.moved += tally.moved;
        all.unowned += tally.unowned;
        all.dropped += tally.dropped;
        all.pins += tally.pins;

        run.flush().unwrap();
        run.end_session();
        let recovered = check_recovered(&run.fs, &run);
        assert_eq!(recovered.pages.latest(), run.attempted());
    }
    assert!(all.commits > 1500 && all.rollbacks > 200, "{all:?}");
    assert!(all.passes > 300 && all.moved > 1500, "{all:?}");
    assert!(all.unowned > 1500 && all.dropped > 200, "{all:?}");
    assert!(all.pins > 100, "{all:?}");
}

#[test]
fn a_pass_moves_the_live_values_and_leaves_the_rest() {
    let mut run = started(1);
    run.commit(&[
        put(1, 10, 40),
        put(1, 11, 700),
        put(2, 20, 90),
        put(2, 21, 33),
    ])
    .unwrap();
    run.commit(&[put(1, 10, 50), Op::Delete { key: 20 }])
        .unwrap();
    run.db().rotate().unwrap();
    assert_eq!(live(&run.db().values, 1), Some(3));
    let before: Vec<ValuePointer> = [(1, 10), (1, 11), (2, 21)]
        .map(|(page, key)| pointer_of(run.db(), page, key))
        .to_vec();

    let cleaned = run.clean(file_id(1)).unwrap();
    assert_eq!(
        cleaned,
        Cleaned {
            moved: 3,
            unowned: 2,
            gone: 0,
            busy: 0,
            live: 0,
            copied_bytes: 50 + 700 + 33 + 3 * 18,
        }
    );
    assert!(cleaned.is_empty());
    assert_eq!(cleaned.owners_not_found(), 0);
    run.check_session();

    let after: Vec<ValuePointer> = [(1, 10), (1, 11), (2, 21)]
        .map(|(page, key)| pointer_of(run.db(), page, key))
        .to_vec();
    let cleaner = run
        .db()
        .cleaner
        .as_ref()
        .expect("the cleaner's file")
        .file_id();
    for (from, to) in before.iter().zip(&after) {
        assert_eq!(from.file(), file_id(1));
        assert_eq!((to.file(), to.value_len()), (cleaner, from.value_len()));
    }
    assert_eq!(live(&run.db().values, cleaner.get()), Some(3));
    assert!(
        run.db().values.cleanable(0).is_empty(),
        "an empty file needs no pass"
    );
    assert_eq!(
        run.clean(file_id(1)).unwrap(),
        Cleaned {
            unowned: 5,
            ..Cleaned::default()
        }
    );
}

#[test]
fn a_moved_value_is_the_same_bytes_with_the_same_owner() {
    struct Everything(RefCell<Vec<(ValuePointer, ValuePointer)>>);
    impl ValueOwners for Everything {
        type Found = ();
        fn find(&self, _: PageId, _: ValuePointer) -> Result<Option<()>, OwnersError> {
            Ok(Some(()))
        }
        fn rewrite(&self, step: PointerRewrite<'_>, (): ()) -> Result<Rewrite, OwnersError> {
            assert_eq!(step.change(), ValueChange::moved(step.from(), step.to()));
            self.0.borrow_mut().push((step.from(), step.to()));
            let latest = version(1);
            Ok(Rewrite::Installed(step.install(latest)))
        }
    }

    let mut store = Store::new();
    let mut writer = store.writer();
    let mut cleaner = store.writer();
    let mut hold = ValueHold::new();
    let values: Vec<(u32, Vec<u8>)> = (0..60u32)
        .map(|at| (1 + at % 7, bytes(1 + (at as usize * 211) % 3000, at as u8)))
        .collect();
    let pointers: Vec<ValuePointer> = values
        .iter()
        .map(|(owner, value)| writer.append(&mut hold, page(*owner), value).unwrap())
        .collect();
    writer.finish().unwrap();
    store.commit();
    store.apply(&acquired(&pointers), 1);
    drop(hold);

    let owners = Everything(RefCell::new(Vec::new()));
    let cleaned = store
        .values
        .clean(file_id(1), &mut cleaner, &mut store.manifest, &owners)
        .unwrap();
    assert_eq!((cleaned.moved, cleaned.live), (60, 0));
    let moves = owners.0.into_inner();
    assert_eq!(
        moves.iter().map(|(from, _)| *from).collect::<Vec<_>>(),
        pointers
    );

    let extent = |pointer: ValuePointer| {
        let file = store
            .fs
            .open(&store.path(pointer.file().get()), OpenMode::Read)
            .unwrap();
        let mut records = vec![0; pointer.extent_len(V1) as usize];
        file.read_exact_at(&mut records, pointer.offset()).unwrap();
        records
    };
    for ((from, to), (owner, value)) in moves.iter().zip(&values) {
        assert_eq!(to.file(), file_id(2));
        assert_eq!(extent(*from), extent(*to), "the records are the same bytes");
        assert_eq!(&store.read(*to, *owner).unwrap(), value);
        assert_eq!(
            &store.read(*from, *owner).unwrap(),
            value,
            "and stay where they were"
        );
    }
    assert_eq!(live(&store.values, 2), Some(60));
    let named = store.named();
    assert!(
        named[1].valid_len() >= moves.last().expect("a move").1.end(V1),
        "the manifest covered every copy before a page pointed at one: {named:?}"
    );
}

#[test]
fn a_cleaned_file_outlives_the_snapshots_that_can_reach_it() {
    for (from, to) in [(1, 3), (2, 4), (1, 2), (2, 1)] {
        let mut run = started(2);
        run.commit(&[
            put(from, 10, 27),
            put(from, 20, 17),
            put(from, 30, 13),
            put(from, 40, 25),
        ])
        .unwrap();
        run.db().rotate().unwrap();
        let snapshot = run.pin();

        run.commit(&[
            Op::Move { key: 10, to },
            put(from, 20, 13),
            Op::Delete { key: 30 },
        ])
        .unwrap();
        let cleaned = run.clean(file_id(1)).unwrap();
        assert_eq!((cleaned.moved, cleaned.unowned, cleaned.live), (1, 3, 0));
        run.flush().unwrap();

        assert!(run.drop_empty().unwrap().is_empty());
        assert!(run.db().files_on_disk().contains(&file_id(1)));
        run.check_session();

        let later = run.pin();
        assert!(run.drop_empty().unwrap().is_empty());
        run.unpin(snapshot);
        assert_eq!(run.drop_empty().unwrap(), [file_id(1)]);
        assert!(!run.db().files_on_disk().contains(&file_id(1)));
        run.check_session();
        run.unpin(later);
    }
}

#[test]
fn a_file_emptied_by_moves_is_kept_until_the_moves_are_durable() {
    let mut run = started(3);
    run.commit(&[put(1, 10, 300), put(2, 20, 300)]).unwrap();
    run.db().rotate().unwrap();
    run.flush().unwrap();
    assert_eq!(run.clean(file_id(1)).unwrap().moved, 2);
    assert!(run.drop_empty().unwrap().is_empty());
    assert!(run.db().files_on_disk().contains(&file_id(1)));

    run.flush().unwrap();
    assert_eq!(run.drop_empty().unwrap(), [file_id(1)]);
    run.check_session();
}

#[test]
fn a_value_whose_owner_cannot_be_found_is_kept() {
    for (from, to) in [(1, 3), (2, 4), (1, 2), (2, 1)] {
        let mut run = started(4);
        run.commit(&[put(from, 10, 25), put(from, 20, 17), put(from, 30, 16)])
            .unwrap();
        run.commit(&[put(from, 30, 11)]).unwrap();
        run.db().rotate().unwrap();
        let carried = run.history[2][&10].1.clone();

        run.carry(10, to).unwrap();
        let stale = pointer_of(run.db(), to, 10);
        assert_eq!(stale.file(), file_id(1));
        let wrong_owner = match run.db().value(stale, page(to)) {
            Err(ValueStoreError::Format { source, .. }) => source,
            other => panic!("expected the owner check to fail, got {other:?}"),
        };
        assert_eq!(
            wrong_owner,
            FormatError::ValueLog(ValueLogError::WrongOwner {
                expected: page(to),
                found: page(from)
            })
        );

        let cleaned = run.clean(file_id(1)).unwrap();
        assert_eq!(
            cleaned,
            Cleaned {
                moved: 2,
                unowned: 2,
                gone: 0,
                busy: 0,
                live: 1,
                copied_bytes: 17 + 11 + 2 * 18,
            }
        );
        assert_eq!(cleaned.owners_not_found(), 1);
        assert!(!cleaned.is_empty());
        run.flush().unwrap();
        assert!(run.drop_empty().unwrap().is_empty());
        assert!(run.db().files_on_disk().contains(&file_id(1)));
        assert_eq!(pointer_of(run.db(), to, 10), stale);
        assert_eq!(run.db().value(stale, page(from)).unwrap(), carried);
        assert!(
            run.db().values.cleanable(0).is_empty(),
            "another pass would find no more"
        );
        assert_eq!(live(&run.db().values, 1), Some(1));

        run.append_again(10, from).unwrap();
        run.check_session();
        assert_eq!(live(&run.db().values, 1), Some(0));
        assert_eq!(
            run.clean(file_id(1)).unwrap(),
            Cleaned {
                unowned: 4,
                ..Cleaned::default()
            }
        );
        run.flush().unwrap();
        assert_eq!(run.drop_empty().unwrap(), [file_id(1)]);
        run.check_session();
    }
}

#[test]
fn a_file_with_an_unfound_owner_is_worth_a_pass_again_once_its_count_changes() {
    let mut run = started(5);
    run.commit(&[put(1, 10, 25), put(1, 20, 17), put(1, 30, 900)])
        .unwrap();
    run.db().rotate().unwrap();
    run.carry(10, 3).unwrap();
    run.commit(&[Op::Delete { key: 30 }]).unwrap();
    assert_eq!(run.db().values.cleanable(50).len(), 1);
    assert_eq!(run.clean(file_id(1)).unwrap().owners_not_found(), 1);
    assert!(run.db().values.cleanable(0).is_empty());

    run.commit(&[put(5, 50, 8)]).unwrap();
    assert!(
        run.db().values.cleanable(0).is_empty(),
        "a change to another file says nothing new about this one"
    );
    run.append_again(10, 1).unwrap();
    assert!(
        run.db().values.cleanable(0).is_empty(),
        "and now it holds no live value"
    );
}

#[test]
fn a_rewrite_is_refused_while_a_write_transaction_holds_a_private_copy_of_the_page() {
    for commit in [true, false] {
        let mut run = started(6);
        run.commit(&[put(1, 10, 40), put(1, 11, 50), put(3, 30, 60)])
            .unwrap();
        run.db().rotate().unwrap();
        let (txn, next) = run.begin(&[put(1, 12, 70)]).unwrap();
        let before = [pointer_of(run.db(), 1, 10), pointer_of(run.db(), 1, 11)];

        let cleaned = run.clean(file_id(1)).unwrap();
        assert_eq!(
            cleaned,
            Cleaned {
                moved: 1,
                unowned: 0,
                gone: 0,
                busy: 2,
                live: 2,
                copied_bytes: 40 + 50 + 60 + 3 * 18,
            }
        );
        assert_eq!(cleaned.owners_not_found(), 0);
        assert_eq!(
            [pointer_of(run.db(), 1, 10), pointer_of(run.db(), 1, 11)],
            before,
            "nothing was installed"
        );
        let cleaner = run
            .db()
            .cleaner
            .as_ref()
            .expect("the cleaner's file")
            .file_id();
        assert_eq!(
            live(&run.db().values, cleaner.get()),
            Some(1),
            "an abandoned move counts nothing"
        );
        run.check_session();
        assert_eq!(
            run.db().values.cleanable(0).len(),
            1,
            "the file is tried again later"
        );

        run.end(txn, next, commit).unwrap();
        run.check_session();
        let cleaned = run.clean(file_id(1)).unwrap();
        assert_eq!((cleaned.moved, cleaned.busy, cleaned.live), (2, 0, 0));
        run.check_session();
        run.flush().unwrap();
        assert_eq!(run.drop_empty().unwrap(), [file_id(1)]);
        run.end_session();
        check_recovered(&run.fs, &run);
    }
}

/// The pages, with something that happens between the cleaner's look at
/// them and its first rewrite.
struct Racing<'a, F: FnOnce()> {
    pages: &'a PageStore,
    race: RefCell<Option<F>>,
}

impl<F: FnOnce()> ValueOwners for Racing<'_, F> {
    type Found = u64;

    fn find(&self, page: PageId, pointer: ValuePointer) -> Result<Option<u64>, OwnersError> {
        self.pages.find(page, pointer)
    }

    fn rewrite(&self, step: PointerRewrite<'_>, found: u64) -> Result<Rewrite, OwnersError> {
        if let Some(race) = self.race.borrow_mut().take() {
            race();
        }
        self.pages.rewrite(step, found)
    }
}

#[test]
fn a_move_is_abandoned_when_a_commit_superseded_the_page_after_the_cleaner_looked() {
    let mut run = started(7);
    run.commit(&[
        put(1, 10, 40),
        put(1, 11, 50),
        put(3, 30, 60),
        put(3, 31, 20),
        put(5, 50, 30),
    ])
    .unwrap();
    run.db().rotate().unwrap();
    let (txn, next) = run
        .begin(&[put(1, 10, 45), Op::Delete { key: 11 }, put(3, 32, 70)])
        .unwrap();
    run.db().prepare(&txn).unwrap();
    run.history.push(next);
    let before = [30, 31].map(|key| pointer_of(run.db(), 3, key));

    let cleaned = run
        .db()
        .clean_with(file_id(1), |pages, values| Racing {
            pages,
            race: RefCell::new(Some(move || {
                commit_prepared(pages, values, txn).expect("the commit is installed");
            })),
        })
        .unwrap();
    assert_eq!(
        cleaned,
        Cleaned {
            moved: 1,
            unowned: 0,
            gone: 0,
            busy: 4,
            live: 2,
            copied_bytes: 40 + 50 + 60 + 20 + 30 + 5 * 18,
        },
        "only the page the commit left alone took its rewrite"
    );
    assert_eq!(run.db().pages.latest(), 2);
    assert_eq!(
        [30, 31].map(|key| pointer_of(run.db(), 3, key)),
        before,
        "the cells the commit did not touch still hold the old pointers"
    );
    assert_ne!(pointer_of(run.db(), 5, 50).file(), file_id(1));
    run.check_session();
    assert_eq!(run.db().values.cleanable(0).len(), 1);

    let cleaned = run.clean(file_id(1)).unwrap();
    assert_eq!(
        (cleaned.moved, cleaned.unowned, cleaned.live),
        (2, 3, 0),
        "a fresh pass finds the two owners the commit left"
    );
    let moved = pointer_of(run.db(), 3, 30);
    run.check_session();

    run.flush().unwrap();
    assert_eq!(run.drop_empty().unwrap(), [file_id(1)]);
    run.check_session();
    run.end_session();
    let recovered = check_recovered(&run.fs, &run);
    assert_eq!(recovered.pages.latest(), 2);
    assert_eq!(pointer_of(&recovered, 3, 30), moved);
}

#[test]
fn values_written_ahead_of_a_commit_are_held_and_count_from_the_commit() {
    for commit in [true, false] {
        let mut run = started(8);
        run.commit(&[put(1, 10, 40), put(1, 11, 50)]).unwrap();
        let (txn, ahead) = run.begin(&[put(3, 30, 60)]).unwrap();
        assert_eq!(txn.held_files(), [file_id(1)]);
        run.db().rotate().unwrap();
        run.commit(&[Op::Delete { key: 10 }, Op::Delete { key: 11 }])
            .unwrap();
        let next = [(30, ahead[&30].clone())].into();
        assert_eq!(
            live(&run.db().values, 1),
            Some(0),
            "no count includes it yet"
        );

        let cleaned = run.clean(file_id(1)).unwrap();
        assert_eq!(
            cleaned,
            Cleaned {
                unowned: 3,
                ..Cleaned::default()
            }
        );
        run.flush().unwrap();
        assert!(run.drop_empty().unwrap().is_empty());
        assert!(run.db().files_on_disk().contains(&file_id(1)));

        run.end(txn, next, commit).unwrap();
        run.flush().unwrap();
        run.check_session();
        if commit {
            assert_eq!(live(&run.db().values, 1), Some(1));
            assert!(run.drop_empty().unwrap().is_empty());
            assert_eq!(pointer_of(run.db(), 3, 30).file(), file_id(1));
            assert_eq!(run.clean(file_id(1)).unwrap().moved, 1);
            run.flush().unwrap();
        }
        assert_eq!(run.drop_empty().unwrap(), [file_id(1)]);
        run.check_session();
    }
}

#[test]
fn a_pass_is_refused_for_a_file_that_cannot_be_cleaned() {
    let mut run = started(9);
    run.commit(&[put(1, 10, 40)]).unwrap();
    let open = run.db().writer.as_ref().expect("a writer").file_id();
    assert!(matches!(
        run.clean(open),
        Err(ValueStoreError::NotClosed { file }) if file == open
    ));
    assert!(matches!(
        run.clean(file_id(77)),
        Err(ValueStoreError::UnknownFile { .. })
    ));
    run.db().rotate().unwrap();

    let db = run.db();
    let other = Path::new("/other");
    db.fs.create_dir(other).unwrap();
    let mut elsewhere = ManifestStore::create(shared(&db.fs), other, empty_manifest()).unwrap();
    let to = db.cleaner.as_mut().expect("the cleaner's file");
    assert!(matches!(
        db.values.clean(open, to, &mut elsewhere, &db.pages),
        Err(ValueStoreError::WrongDirectory { .. })
    ));

    let manifest = read_manifest(&db.fs, db_dir()).unwrap();
    let reader = ValueStore::open_read_only(shared(&db.fs), db_dir(), &manifest).unwrap();
    assert!(matches!(
        reader.clean(open, to, &mut db.manifest, &db.pages),
        Err(ValueStoreError::ReadOnly)
    ));
    run.check_session();
}

/// The pages, failing where a test says.
struct Failing<'a> {
    pages: &'a PageStore,
    fail_holds: bool,
    rewrites_before_failure: RefCell<u64>,
}

impl ValueOwners for Failing<'_> {
    type Found = u64;

    fn find(&self, page: PageId, pointer: ValuePointer) -> Result<Option<u64>, OwnersError> {
        if self.fail_holds {
            return Err("the page cannot be read".into());
        }
        self.pages.find(page, pointer)
    }

    fn rewrite(&self, step: PointerRewrite<'_>, found: u64) -> Result<Rewrite, OwnersError> {
        let mut left = self.rewrites_before_failure.borrow_mut();
        if *left == 0 {
            return Err("the page cannot be latched".into());
        }
        *left -= 1;
        self.pages.rewrite(step, found)
    }
}

#[test]
fn a_pass_that_fails_part_way_leaves_every_value_with_one_owner() {
    for (fail_holds, rewrites, moved) in [(true, 0, 0), (false, 0, 0), (false, 2, 2)] {
        let mut run = started(10);
        run.commit(&[
            put(1, 10, 40),
            put(1, 11, 50),
            put(2, 20, 60),
            put(3, 30, 20),
        ])
        .unwrap();
        run.db().rotate().unwrap();
        let error = run
            .db()
            .clean_with(file_id(1), |pages, _| Failing {
                pages,
                fail_holds,
                rewrites_before_failure: RefCell::new(rewrites),
            })
            .unwrap_err();
        let message = if fail_holds {
            "the page cannot be read"
        } else {
            "the page cannot be latched"
        };
        assert_eq!(
            error.to_string(),
            format!("the owner of a value could not be asked: {message}")
        );
        assert!(matches!(error, ValueStoreError::Owners(_)));
        assert_eq!(live(&run.db().values, 1), Some(4 - moved));
        run.check_session();

        let cleaned = run.clean(file_id(1)).unwrap();
        assert_eq!((cleaned.moved, cleaned.live), (4 - moved, 0));
        run.check_session();
        run.flush().unwrap();
        run.end_session();
        check_recovered(&run.fs, &run);
    }
}

#[test]
fn a_pass_that_finds_an_owner_the_count_does_not_include_keeps_the_file() {
    let (mut store, mut cleaner, pointers) = three_values(1);

    let error = store
        .values
        .clean(file_id(1), &mut cleaner, &mut store.manifest, &Everything)
        .unwrap_err();
    assert!(matches!(
        error,
        ValueStoreError::Miscounted { pointer, why: Miscount::CountIsZero } if pointer == pointers[1]
    ));
    let source = store.values.count(file_id(1)).expect("the file");
    assert_eq!((source.values, source.miscounted), (0, true));
    assert_eq!(
        live(&store.values, 2),
        Some(3),
        "the pass was carried out in full"
    );
    store.all_durable();
    assert!(store.drop_empty(9).is_empty());
    assert!(store.exists(1));
}

#[test]
fn no_pointer_is_installed_to_a_copy_the_manifest_does_not_cover() {
    let (mut store, mut cleaner, pointers) = three_values(3);
    assert!(pointers.iter().all(|&pointer| store.values.covers(pointer)));

    let copies = store
        .values
        .copy_live(file_id(1), &mut cleaner, &Everything)
        .unwrap();
    assert_eq!((copies.file(), copies.len()), (file_id(1), 3));
    assert!(!copies.is_empty());
    let copy = ValuePointer::new(file_id(2), 20, 30).expect("the first copy");
    assert_eq!(store.read(copy, 1).unwrap(), bytes(30, 0));
    assert!(!store.values.covers(copy));

    let uncovered = |store: &Store, copies| match store.values.install(copies, &Everything) {
        Err(ValueStoreError::NotCovered { pointer }) => pointer,
        other => panic!("expected the install to be refused, got {other:?}"),
    };
    assert_eq!(uncovered(&store, copies), copy);
    assert_eq!(
        (live(&store.values, 1), live(&store.values, 2)),
        (Some(3), Some(0))
    );

    let copies = store
        .values
        .copy_live(file_id(1), &mut cleaner, &Everything)
        .unwrap();
    cleaner.sync().unwrap();
    assert_eq!(
        uncovered(&store, copies).file(),
        file_id(2),
        "synced is not covered"
    );

    let copies = store
        .values
        .copy_live(file_id(1), &mut cleaner, &Everything)
        .unwrap();
    cleaner.sync().unwrap();
    store.commit();
    let cleaned = store.values.install(copies, &Everything).unwrap();
    assert_eq!(
        cleaned,
        Cleaned {
            moved: 3,
            copied_bytes: 3 * 48,
            ..Cleaned::default()
        }
    );
    assert_eq!(
        (live(&store.values, 1), live(&store.values, 2)),
        (Some(0), Some(3))
    );
    assert!(
        store.values.covers(copy),
        "the earlier copies are covered too"
    );
    assert!(!store
        .values
        .covers(ValuePointer::new(file_id(2), cleaner.end(), 1).expect("past the end")));
    assert!(!store
        .values
        .covers(ValuePointer::new(file_id(9), 20, 1).expect("in no file")));
}

#[test]
fn copies_that_are_given_up_hold_their_file_no_longer() {
    let (mut store, mut cleaner, pointers) = three_values(3);
    let copies = store
        .values
        .copy_live(file_id(1), &mut cleaner, &Everything)
        .unwrap();
    cleaner.finish().unwrap();
    store.all_durable();
    assert!(store.drop_empty(1).is_empty(), "the copies hold file 2");
    drop(copies);
    assert_eq!(store.drop_empty(1), [2]);
    assert_eq!(live(&store.values, 1), Some(3));
    for (at, &pointer) in pointers.iter().enumerate() {
        assert_eq!(store.read(pointer, 1).unwrap(), bytes(30, at as u8));
    }
}

#[test]
fn a_file_with_no_live_value_is_copied_to_nothing() {
    let (mut store, mut cleaner, pointers) = three_values(3);
    store.apply(&released(&pointers), 2);
    struct Nothing;
    impl ValueOwners for Nothing {
        type Found = ();
        fn find(&self, _: PageId, _: ValuePointer) -> Result<Option<()>, OwnersError> {
            Ok(None)
        }
        fn rewrite(&self, _: PointerRewrite<'_>, (): ()) -> Result<Rewrite, OwnersError> {
            unreachable!("nothing was copied")
        }
    }
    let end = cleaner.end();
    let copies = store
        .values
        .copy_live(file_id(1), &mut cleaner, &Nothing)
        .unwrap();
    assert!(copies.is_empty());
    assert_eq!(cleaner.end(), end);
    let cleaned = store.values.install(copies, &Nothing).unwrap();
    assert_eq!(
        cleaned,
        Cleaned {
            unowned: 3,
            ..Cleaned::default()
        }
    );
    store.all_durable();
    assert_eq!(store.drop_empty(2), [1]);
}

/// Owners that count a rewrite and then do not report it installed.
struct CountsAndFails {
    refuses: bool,
}

impl ValueOwners for CountsAndFails {
    type Found = ();

    fn find(&self, _: PageId, _: ValuePointer) -> Result<Option<()>, OwnersError> {
        Ok(Some(()))
    }

    fn rewrite(&self, step: PointerRewrite<'_>, (): ()) -> Result<Rewrite, OwnersError> {
        let _counted = step.install(version(1));
        if self.refuses {
            Ok(Rewrite::Refused(Refusal::Busy))
        } else {
            Err("the page could not be written".into())
        }
    }
}

#[test]
fn a_rewrite_that_was_counted_and_not_reported_installed_keeps_the_old_file() {
    for refuses in [true, false] {
        let (mut store, mut cleaner, pointers) = three_values(3);
        let error = store
            .values
            .clean(
                file_id(1),
                &mut cleaner,
                &mut store.manifest,
                &CountsAndFails { refuses },
            )
            .unwrap_err();
        assert!(
            matches!(
                error,
                ValueStoreError::Miscounted { pointer, why: Miscount::NotInstalled }
                    if pointer == pointers[0]
            ),
            "{error}"
        );
        let moved = if refuses { 3 } else { 1 };
        let source = store.values.count(file_id(1)).expect("the file");
        assert_eq!((source.values, source.miscounted), (3 - moved, true));
        assert_eq!(live(&store.values, 2), Some(moved));

        store.apply(&released(&pointers[moved as usize..]), 2);
        store.all_durable();
        assert!(
            store.drop_empty(9).is_empty(),
            "the cell may still hold the old pointer"
        );
        assert_eq!(store.read(pointers[0], 1).unwrap(), bytes(30, 0));
    }
}

#[test]
fn a_file_is_worth_another_pass_when_a_value_was_counted_while_the_last_one_ran() {
    let mut run = started(11);
    run.commit(&[put(1, 10, 40)]).unwrap();
    let (txn, ahead) = run.begin(&[put(3, 30, 60)]).unwrap();
    run.db().rotate().unwrap();
    run.db().prepare(&txn).unwrap();
    let mut next = run.history[1].clone();
    next.insert(30, ahead[&30].clone());
    run.history.push(next);

    let cleaned = run
        .db()
        .clean_with(file_id(1), |pages, values| Racing {
            pages,
            race: RefCell::new(Some(move || {
                commit_prepared(pages, values, txn).expect("the commit is installed");
            })),
        })
        .unwrap();
    assert_eq!(
        (cleaned.moved, cleaned.unowned, cleaned.busy, cleaned.live),
        (1, 1, 0, 1)
    );
    assert_eq!(cleaned.owners_not_found(), 1);
    run.check_session();
    assert_eq!(
        run.db().values.cleanable(0).len(),
        1,
        "the value has an owner now, which the next pass finds"
    );

    let cleaned = run.clean(file_id(1)).unwrap();
    assert_eq!((cleaned.moved, cleaned.live), (1, 0));
    run.check_session();
}

#[test]
fn a_pass_takes_only_what_its_own_store_made() {
    let (mut store, mut cleaner, _) = three_values(3);
    let (mut other, mut foreign, _) = three_values(3);

    assert!(matches!(
        store
            .values
            .copy_live(file_id(1), &mut foreign, &Everything),
        Err(ValueStoreError::WrongStore)
    ));
    assert!(matches!(
        store
            .values
            .clean(file_id(1), &mut foreign, &mut store.manifest, &Everything),
        Err(ValueStoreError::WrongStore)
    ));

    let copies = other
        .values
        .copy_live(file_id(1), &mut foreign, &Everything)
        .unwrap();
    foreign.sync().unwrap();
    other.commit();
    cleaner.sync().unwrap();
    store.commit();
    assert!(matches!(
        store.values.install(copies, &Everything),
        Err(ValueStoreError::WrongStore)
    ));
    assert_eq!(
        (live(&store.values, 1), live(&store.values, 2)),
        (Some(3), Some(0))
    );

    assert!(matches!(
        store.values.mark_durable(other.values.applied()),
        Err(ValueStoreError::WrongStore)
    ));
    assert!(matches!(
        store.values.drop_empty(version(9), other.values.applied()),
        Err(ValueStoreError::WrongStore)
    ));
    let earlier = store.values.applied();
    let reopened = store.reopen(&[]);
    assert!(matches!(
        reopened.values.mark_durable(earlier),
        Err(ValueStoreError::WrongStore)
    ));
}
