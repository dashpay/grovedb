//! Which value-log files a pass cleans: the floor on a file's own dead
//! share, the ceiling over which the file most worth it is cleaned
//! whatever its share, the files that are nearly empty, and the counts by
//! dead share.

use std::collections::BTreeSet;

use super::{values::value_files, *};
use crate::{cleaner::values::SWEEP_DEAD_PERCENT, values::ValueStore};

/// The values of a page.
const VALUES: u16 = 10;

/// `pages` pages of [`VALUES`] values of 400 bytes, the values of each
/// page in a value-log file of their own, which they fill and close.
fn files(pages: u32) -> Db {
    let mut db = Db::with(small_segments());
    let values = {
        let manifest = db.manifest.lock().expect("the manifest");
        ValueStore::open(&manifest, &[]).expect("the value log")
    };
    db.values = values.with_target_file_len(3_800);
    for id in 1..=pages {
        let cells: Vec<Op> = (1..=VALUES)
            .map(|at| {
                let seed = (id as u8).wrapping_mul(11).wrapping_add(at as u8);
                insert((id, at), &[b'k', at as u8], long(seed, 400))
            })
            .collect();
        db.commit(&cells);
        db.wb().pass().expect("a pass");
    }
    let of_page = |id: u32| -> BTreeSet<FileId> {
        (1..=VALUES)
            .map(|at| db.pointer(page(id), slot(at)).file())
            .collect()
    };
    let all: BTreeSet<FileId> = (1..=pages).flat_map(of_page).collect();
    assert!((1..=pages).all(|id| of_page(id).len() == 1));
    assert_eq!(all.len() as u32, pages, "a file for each page");
    db
}

/// The file that holds the values of page `id`, while its last value is
/// one of them.
fn file_of(db: &Db, id: u32) -> FileId {
    db.pointer(page(id), slot(VALUES)).file()
}

/// The first `count` values of page `id` are replaced by values that
/// their cells hold: they die where they are, and nothing new is written
/// to the value log for them.
fn kill(db: &Db, id: u32, slots: std::ops::RangeInclusive<u16>) {
    let ops: Vec<Op> = slots
        .map(|at| Op::Value {
            page: page(id),
            slot: slot(at),
            value: inline(b"gone"),
        })
        .collect();
    db.commit(&ops);
    db.wb().pass().expect("a pass");
}

/// A cleaner with the value log's three shares as given, which closes no
/// file and leaves the segments alone.
fn choosing(target: u8, floor: u8, ceiling: u8) -> CleanerConfig {
    CleanerConfig {
        value_dead_percent: target,
        value_min_file_dead_percent: floor,
        value_ceiling_dead_percent: ceiling,
        seal_dead_percent: 101,
        ..eager()
    }
}

/// A file under the floor is not cleaned while the value log is under its
/// ceiling, though the log is over its target and the file is the most
/// dead: its live values would be copied for little. Over the ceiling the
/// same file is cleaned. Each cleaning is counted in the class of the
/// share its file had.
#[test]
fn a_file_under_the_floor_is_cleaned_only_over_the_ceiling() {
    let db = files(4);
    let (thirty, fifty) = (file_of(&db, 1), file_of(&db, 2));
    kill(&db, 1, 1..=3);
    kill(&db, 2, 1..=5);

    let state = CleanerState::new(choosing(1, 40, 100));
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    let (file, cleaned) = pass.cleaned.expect("the file half of which is dead");
    assert_eq!((file, cleaned.moved), (fifty, 5));
    assert!(pass.swept.is_empty(), "{pass:?}");
    assert!(cleaner.value_space().expect("the space").dead_percent() > 1.0);
    for _ in 0..3 {
        let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
        assert_eq!(pass.cleaned, None, "{pass:?}");
    }
    assert!(value_files(&db).contains(&thirty));
    let did = state.stats();
    assert_eq!(did.value_files_by_dead, [0, 0, 1, 0, 0]);
    assert_eq!(did.value_bytes_by_dead, [0, 0, cleaned.copied_bytes, 0, 0]);
    assert_eq!(did.value_bytes_copied, cleaned.copied_bytes);

    let state = CleanerState::new(choosing(1, 40, 1));
    let cleaner = Cleaner::new(db.wb(), &state);
    let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
    let (file, cleaned) = pass.cleaned.expect("over the ceiling");
    assert_eq!((file, cleaned.moved), (thirty, 7));
    assert_eq!(state.stats().value_files_by_dead, [0, 1, 0, 0, 0]);
    settle_values(&cleaner, &Cells(&db));
    assert!(!value_files(&db).contains(&thirty));
    db.check_warm();
}

/// After the file most worth it, a pass cleans the files that are at
/// least [`SWEEP_DEAD_PERCENT`] dead, most dead first, while their live
/// bytes stay within the pass's budget; the first is cleaned whatever it
/// holds. A file under that share waits for a pass of its own.
#[test]
fn files_that_are_nearly_empty_are_cleaned_in_the_same_pass_within_its_budget() {
    assert_eq!(SWEEP_DEAD_PERCENT, 90);
    let run = |budget: Option<u64>| {
        let db = files(5);
        let file: Vec<FileId> = (1..=5).map(|id| file_of(&db, id)).collect();
        kill(&db, 1, 1..=5);
        for id in 2..=4 {
            kill(&db, id, 1..=9);
        }
        let one = db.values.count(file[1]).expect("counted").bytes;
        let config = CleanerConfig {
            max_pass_bytes: budget.unwrap_or(one),
            ..choosing(1, 40, 100)
        };
        let state = CleanerState::new(config);
        let cleaner = Cleaner::new(db.wb(), &state);
        let mut passes = Vec::new();
        for _ in 0..4 {
            let pass = cleaner.pass_values(&Cells(&db)).expect("a pass");
            let cleaned: Vec<FileId> = pass
                .cleaned
                .iter()
                .chain(&pass.swept)
                .map(|(file, _)| *file)
                .collect();
            passes.push(cleaned);
        }
        assert!(value_files(&db).contains(&file[4]), "a file all live stays");
        db.check_warm();
        (file, passes, state.stats())
    };

    // A budget of one value: the first file, and one more.
    let (file, passes, did) = run(None);
    assert_eq!(
        passes,
        [vec![file[1], file[2]], vec![file[3]], vec![file[0]], vec![]]
    );
    assert_eq!(did.value_files_by_dead, [0, 0, 1, 0, 3]);

    // Room for all: the three nearly empty files in one pass.
    let (file, passes, _) = run(Some(u64::MAX));
    assert_eq!(passes[0], [file[1], file[2], file[3]]);
    assert_eq!(passes[1], [file[0]]);
}

/// **The workload that defeats a floor.** The deaths are spread evenly
/// over the files: each round kills one value of every page, so every file
/// is as dead as every other, and none reaches the floor of 60 in a
/// hundred before the log is half dead. With a ceiling of 15 the log is
/// held at it after every round, the files cleaned being under the floor;
/// with none it goes on past the ceiling.
#[test]
fn deaths_spread_evenly_over_the_files_are_held_at_the_ceiling() {
    let run = |ceiling: u8| {
        let db = files(20);
        let state = CleanerState::new(choosing(5, 60, ceiling));
        let cleaner = Cleaner::new(db.wb(), &state);
        let mut most = 0.0f64;
        for round in 1..=6 {
            for id in 1..=20 {
                kill(&db, id, round..=round);
            }
            settle_values(&cleaner, &Cells(&db));
            let dead = cleaner.value_space().expect("the space").dead_percent();
            most = most.max(dead);
        }
        db.check_warm();
        (most, state.stats())
    };
    let (held, did) = run(15);
    assert!(held <= 15.0, "{held:.1}% dead with a ceiling of 15");
    let under_the_floor: u64 = did.value_files_by_dead[..3].iter().sum();
    assert!(under_the_floor > 0, "{did:?}");
    let (unheld, _) = run(100);
    assert!(unheld > 40.0, "{unheld:.1}% dead with no ceiling");
}
