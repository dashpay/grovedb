//! The comparison that a store of the tests makes when it is dropped
//! ([`Db::recovered_as_left`], D-108 item 3): it passes for a store whose
//! files are as the session left them, having compared pages, and it
//! fails for a store whose files are not.

use std::fs;

use super::{
    drops::planted,
    store::{Db, RecoveredAsLeft, Rng},
    *,
};

/// A store with commits in its log.
fn store() -> (std::sync::Arc<Db>, Versions) {
    let (db, versions) = Db::new(1_024);
    {
        let mut world = World::new(&db, &versions);
        planted(&mut world, &mut Rng::new(17), 400);
    }
    (db, versions)
}

#[test]
fn a_store_that_is_left_as_it_was_is_recovered_the_same_with_its_pages_compared() {
    let (db, _versions) = store();
    let same = db.recovered_as_left();
    assert!(
        matches!(same, Ok(RecoveredAsLeft::Same { live }) if live > 3),
        "{same:?}"
    );
    // That was the comparison of this store: the drop makes none on the
    // files a recovery has written to.
    db.files_are_gone();
}

#[test]
fn a_store_whose_log_lost_its_end_is_not_recovered_as_it_was_left() {
    let (db, _versions) = store();
    // The newest file of the log that holds anything loses a third of
    // what it holds: the last commits of the session are not in it any
    // more.
    let files = fs::read_dir(db.dir.join("wal")).expect("the log's directory");
    let mut files: Vec<_> = files.map(|file| file.expect("a file").path()).collect();
    files.sort();
    let held = |path: &std::path::PathBuf| fs::metadata(path).expect("a length").len();
    let newest = files.iter().rev().find(|path| held(path) > 0);
    let newest = newest.expect("a file of the log holds the commits");
    let file = fs::OpenOptions::new().write(true).open(newest);
    let cut = held(newest) * 2 / 3;
    file.expect("the file").set_len(cut).expect("it is cut");

    let differs = db.recovered_as_left();
    let differs = differs.expect_err("the log lost commits of the session");
    println!("{differs}");
    db.files_are_gone();
}
