//! What write-back takes from the layer above tree pages: the order in
//! which a scan reads the cells of a page, and the regrouping of a page's
//! values before a new base image of it.

use super::*;

/// The pointers the cells of the current version of `page` hold, in the
/// order of the cells' keys.
fn in_key_order(db: &Db, id: PageId) -> Vec<ValuePointer> {
    let frame = db.frame(id, db.latest()).expect("the page");
    let slots = ByKey::slots(&frame);
    slots.into_iter().map(|slot| db.pointer(id, slot)).collect()
}

fn is_one_run(pointers: &[ValuePointer]) -> bool {
    pointers
        .windows(2)
        .all(|pair| (pair[1].file(), pair[1].offset()) == (pair[0].file(), pair[0].end(V1)))
}

fn four_values(db: &Db) {
    db.commit(&[
        insert((1, 1), b"d", long(1, 1000)),
        insert((1, 2), b"b", long(2, 1100)),
        insert((1, 3), b"c", long(3, 1200)),
        insert((1, 4), b"a", long(4, 1300)),
    ]);
}

/// Write-back appends the pending values of a page in the order a scan
/// reads them, so the values of a new page are one run in the value log
/// from the start.
#[test]
fn the_pending_values_of_a_page_are_appended_in_the_order_a_scan_reads_them() {
    let db = Db::new();
    four_values(&db);
    let by_key = ByKey(&db);
    let pass = db.wb_with(Some(&by_key)).pass().expect("a pass");
    assert_eq!((pass.written, pass.placed), (1, 4));
    let pointers = in_key_order(&db, page(1));
    assert!(is_one_run(&pointers), "{pointers:?}");
    // The page was not regrouped: its values lay well already.
    assert_eq!(db.stats().values_regrouped, 0);
    assert_eq!(db.stats().values_appended, 4);
    db.check_cold();

    // With nothing handed to write-back, the order is that of the slots.
    let db = Db::new();
    four_values(&db);
    db.wb().pass().expect("a pass");
    let by_slot: Vec<ValuePointer> = (1..=4).map(|at| db.pointer(page(1), slot(at))).collect();
    assert!(is_one_run(&by_slot));
    assert!(!is_one_run(&in_key_order(&db, page(1))));
}

/// The values of a page that were written by one commit after another lie
/// where their writes left them. When the page is about to get a new base
/// image, they are regrouped first, and the image holds the new pointers.
#[test]
fn the_values_of_a_page_are_regrouped_before_its_base_image_is_written() {
    let config = FlushConfig {
        max_chain_deltas: 2,
        ..FlushConfig::new(Db::SIZE)
    };
    let db = Db::with(config);
    let by_key = ByKey(&db);
    let wb = db.wb_with(Some(&by_key));
    db.commit(&[
        insert((1, 1), b"a", long(1, 1000)),
        insert((2, 1), b"x", long(9, 900)),
    ]);
    wb.pass().expect("a pass");
    db.commit(&[
        insert((1, 2), b"c", long(2, 1100)),
        insert((2, 2), b"y", long(8, 800)),
    ]);
    wb.pass().expect("a pass");
    db.commit(&[insert((1, 3), b"b", long(3, 1200))]);
    wb.pass().expect("a pass");
    assert_eq!(db.shape(page(1)), "Bdd");
    assert!(!is_one_run(&in_key_order(&db, page(1))));
    let before = (db.stats(), db.values.counts(), in_key_order(&db, page(1)));
    assert_eq!(before.0.values_regrouped, 0);

    // The next change would be a third run: the page gets a base image.
    db.commit(&[hash((1, 1), 5)]);
    let pass = wb.pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (1, 0));
    let after = db.stats();
    assert_eq!(db.shape(page(1)), "B");
    assert_eq!(after.consolidations, before.0.consolidations + 1);
    assert_eq!(after.values_regrouped, 3);
    // The three copies are counted with their record headers: they are
    // bytes write-back wrote.
    assert_eq!(before.0.regroup_stored_bytes, 0);
    let copies: u64 = [1000, 1100, 1200]
        .iter()
        .map(|&len| {
            lignin_format::value_log::extent_len(len, lignin_format::FormatVersion::CURRENT)
        })
        .sum();
    assert_eq!(after.regroup_stored_bytes, copies);
    let pointers = in_key_order(&db, page(1));
    assert!(is_one_run(&pointers), "{pointers:?}");
    assert!(pointers.iter().all(|pointer| !before.2.contains(pointer)));
    // Each value is counted once, where it is now.
    let live = |counts: &[crate::values::FileCount]| -> u64 {
        counts.iter().map(|count| count.values).sum()
    };
    assert_eq!(live(&db.values.counts()), live(&before.1));
    // The image came from the frame the regrouping left, and what was
    // kept of the page went into it: the next change is its own record.
    db.check_cold();
    db.commit(&[hash((1, 2), 6)]);
    wb.pass().expect("a pass");
    let last = db.stats();
    assert_eq!(db.shape(page(1)), "Bd");
    assert_eq!(last.delta_bytes - after.delta_bytes, 4 + 35);
    assert_eq!(last.runs_refused, 0);
    db.check_cold();
}

/// A page that a write transaction holds a private copy of is not
/// regrouped: it is written as it is, and regrouped another time.
#[test]
fn a_page_under_a_private_copy_is_written_without_a_regrouping() {
    let db = Db::with(FlushConfig {
        max_chain_deltas: 0,
        ..FlushConfig::new(Db::SIZE)
    });
    for change in [
        insert((1, 1), b"b", long(1, 1000)),
        insert((2, 1), b"x", long(9, 900)),
        insert((1, 2), b"a", long(2, 1100)),
    ] {
        db.commit(&[change]);
        db.wb().pass().expect("a pass");
    }
    assert!(!is_one_run(&in_key_order(&db, page(1))));

    let by_key = ByKey(&db);
    let wb = db.wb_with(Some(&by_key));
    db.commit(&[hash((1, 1), 1)]);
    let mut txn = db.begin();
    txn.copy(page(1));
    let pass = wb.pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (1, 0));
    assert_eq!(db.stats().values_regrouped, 0);
    assert!(!is_one_run(&in_key_order(&db, page(1))));
    txn.roll_back();
    db.check_cold();

    // Once the copy is gone, the next image of the page is regrouped.
    db.commit(&[hash((1, 1), 2)]);
    wb.pass().expect("a pass");
    assert_eq!(db.stats().values_regrouped, 2);
    assert!(is_one_run(&in_key_order(&db, page(1))));
    db.check_cold();
}

/// What regroups fails. Regrouping is for the scans to come and nothing a
/// page waits for: the pass reports the error, writes the page as it is,
/// and regroups nothing more.
#[test]
fn a_regrouping_that_fails_is_reported_and_the_page_is_written_as_it_is() {
    struct Failing(AtomicU64);
    impl PageValues for Failing {
        fn scan_order(&self, _: &[u8], _: &mut Vec<SlotId>) {}

        fn regroup(
            &self,
            _: PageId,
            to: &mut ValueWriter,
            cover: &mut dyn FnMut(&mut ValueWriter) -> Result<(), FlushError>,
        ) -> Result<u64, RegroupError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            cover(to)?;
            Err("the tree layer could not".into())
        }
    }

    let db = Db::new();
    db.commit(&[
        insert((1, 1), b"a", inline(b"v")),
        insert((2, 1), b"b", inline(b"w")),
    ]);
    let failing = Failing(AtomicU64::new(0));
    let error = db
        .wb_with(Some(&failing))
        .pass()
        .expect_err("a regrouping failed");
    assert_eq!(
        error.to_string(),
        "the values of page 1 were not regrouped: the tree layer could not"
    );
    assert!(std::error::Error::source(&error).is_some());
    // Both pages are written, and the second was not offered.
    assert_eq!(failing.0.load(Ordering::SeqCst), 1);
    assert_eq!(db.state.unwritten(), 0);
    assert_eq!(db.stats().base_blocks, 2);
    db.check_cold();
    // The next pass tries a regrouping again, for the page that needs one.
    db.commit(&[hash((2, 1), 1)]);
    let pass = db.wb_with(Some(&failing)).pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (1, 0));
    assert_eq!(failing.0.load(Ordering::SeqCst), 1);
    db.check_cold();
}
