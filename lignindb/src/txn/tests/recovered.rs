//! The comparison that a store of the tests makes when it is dropped
//! (D-108 item 3): what the store's recovery makes of the files against
//! what the session had. It passes for a store whose files are as the
//! session left them, and it fails for a store whose log lost commits and
//! for one whose page differs from what the log makes of it.

use std::fs;

use lignin_format::PageId;
use lignin_store::alloc::PageState;

use super::{
    drops::planted,
    model,
    store::{Db, Outcome, RecoveredAsLeft, Rng, ENTERED},
    *,
};

/// A value that is found again in the page that holds it.
const MARKED: [u8; 60] = [0xab; 60];

/// A store with commits in its log, the last of them a record whose
/// value is [`MARKED`].
fn store() -> (std::sync::Arc<Db>, Versions) {
    let (db, versions) = Db::new(1_024);
    {
        let mut world = World::new(&db, &versions);
        planted(&mut world, &mut Rng::new(17), 400);
        let tx = world.begin();
        let unit = model::unit(Column::Aux, ENTERED + 20);
        world.write(&tx, vec![put(unit, b"marked", &MARKED)]);
        let committed = world.commit(tx, &[]);
        assert!(matches!(committed, Outcome::Committed(_)), "{committed:?}");
    }
    (db, versions)
}

#[test]
fn a_store_that_is_left_as_it_was_is_recovered_the_same() {
    let (db, versions) = store();
    let same = Db::into_recovered(db, versions);
    assert!(
        matches!(same, Ok(RecoveredAsLeft::Same { live }) if live > 3),
        "{same:?}"
    );
}

#[test]
fn a_store_whose_log_lost_its_end_is_not_recovered_as_it_was_left() {
    let (db, versions) = store();
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

    let differs = Db::into_recovered(db, versions);
    let differs = differs.expect_err("the log lost commits of the session");
    println!("{differs}");
}

/// The recovery lands on the session's version with the same pages in
/// use, and one page of the session is not the page that the log makes:
/// a byte of a value was changed in a page step that wrote no record. The
/// comparison names the page, by its image.
#[test]
fn a_page_that_is_not_what_the_log_makes_of_it_is_found_by_its_image() {
    let (db, versions) = store();
    let (mut ids, mut next) = (Vec::new(), Some(PageId::MIN));
    while let Some(from) = next {
        next = db.ids.scan(from, &mut ids);
    }
    let mut live = ids.iter().filter(|(_, state)| *state == PageState::Live);
    let marked = live.find_map(|(id, _)| {
        let frame = db.pool.page_step(*id).current()?;
        let at = frame
            .windows(MARKED.len())
            .position(|bytes| bytes == MARKED)?;
        Some((*id, at))
    });
    let (id, at) = marked.expect("the page of the record is in the pool");
    {
        let step = db.pool.page_step(id);
        let frame = step.current().expect("the page");
        let mut changed = db.pool.blank();
        changed.copy_from(&frame);
        changed.get_mut(at..at + 1).expect("a byte of the value")[0] = 0xac;
        assert!(
            step.replace(&frame, changed).is_ok(),
            "the frame is replaced"
        );
    }

    let differs = Db::into_recovered(db, versions);
    let differs = differs.expect_err("a page of the session is not the log's");
    assert!(
        differs.contains(&format!("page {id} at ")) && differs.contains("is not the page"),
        "{differs}"
    );
}
