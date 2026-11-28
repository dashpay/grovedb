//! The cells of tree pages as the owners of values: the cleaner's pointer
//! rewrites, and the regrouping of a page's values.

use std::cell::Cell;

use lignin_format::flat_page::{FlatLeafBuilder, FlatValue};
use lignin_store::values::{ValueOwners, ValueStoreError};

use super::{
    scope::{check_attached, value_reads},
    *,
};

/// A tree of `count` keys with large values, each commit of `round` keys
/// written back, in an order that is not the keys'.
fn tree_of(db: &Db, count: u32, round: usize) -> Tree {
    grown(db, &shuffled(count, u64::from(count)), round)
}

fn pointer_of(read: &Scanned) -> ValuePointer {
    read.held.and_then(|place| place.pointer()).expect("placed")
}

fn values_of(tree: &Tree, db: &Db) -> Vec<(Vec<u8>, Vec<u8>)> {
    let scanned = tree.scan(db).into_iter();
    scanned.map(|read| (read.key, read.value)).collect()
}

/// The pointers the cells of `page` hold, in the order a scan reads them.
fn in_scan_order(db: &Db, page: PageId) -> Vec<ValuePointer> {
    let frame = db.current(page);
    let view = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
    crate::value::order::pointers_in_order(&view)
}

fn locality(db: &Db, page: PageId) -> Locality {
    let frame = db.current(page);
    Locality::of(&TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page"))
}

impl Db {
    /// Installs a flat leaf as the page `page`, at the next version.
    fn install_flat(&self, page: PageId) {
        let mut leaf = FlatLeafBuilder::new(page, b"", SIZE.image_capacity()).expect("a leaf");
        leaf.push(b"key", FlatValue::Inline(b"value"))
            .expect("fits");
        let image = leaf.finish();
        let copy = self.pool.new_private(OwnerId::new(1 << 40), page);
        let copy = copy.expect("a copy");
        copy.write().expect("the latch")[..image.len()].copy_from_slice(&image);
        let mut state = self.state.borrow_mut();
        state.latest += 1;
        self.pool
            .install(&copy, version(state.latest))
            .expect("installed");
    }
}

/// A log under which the newest commit has installed its pages and is not
/// published yet: it says the version before as the latest.
struct Unpublished<'d>(&'d Db);

impl StepLog for Unpublished<'_> {
    fn latest(&self) -> CommitVersion {
        version(self.0.latest().get() - 1)
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        StepLog::append(&self.0, record)
    }
}

/// A commit installs its pages before its version is published, and
/// write-back can place a value in one of them meanwhile. That page
/// version is the current one: the owner of the value is found in it, and
/// its pointer is rewritten there (`transactions.md` PV7). Looked for in
/// the page as the latest published version has it, the value would have
/// no owner, and the value log would take its file for one that no
/// cleaning helps.
#[test]
fn an_owner_is_found_in_a_page_version_whose_commit_is_not_published_yet() {
    let db = Db::new();
    let order = shuffled(40, 40);
    let tree = grown(&db, &order, 10);
    let last = key(*order.last().expect("a key"));
    let scanned = tree.scan(&db);
    let read = (scanned.iter().find(|read| read.key == last)).expect("the last key");
    let (home, pointer) = (read.at.page(), pointer_of(read));
    let born = db.current(home).born();
    assert_eq!(born, db.latest(), "the last commit wrote the page");

    let owners = TreeOwners::new(db.loader(), &db.pool, SIZE, Unpublished(&db));
    assert_eq!(owners.find(home, pointer).expect("a look"), Some(born));

    // The file is cleaned with that commit unpublished: every value has
    // its owner, and none is left behind.
    let file = pointer.file();
    let mut to = db.close_value_file();
    let cleaned = {
        let mut manifest = db.manifest.borrow_mut();
        db.values.clean(file, &mut to, &mut manifest, &owners)
    }
    .expect("a pass");
    assert_eq!((cleaned.unowned, cleaned.busy, cleaned.live), (0, 0, 0));
    assert!(cleaned.moved >= 10, "{cleaned:?}");
    assert_eq!(values_of(&tree, &db).len(), 40);
}

/// A log that takes a number of records and then no more.
struct Full<'d> {
    db: &'d Db,
    left: Cell<usize>,
}

impl<'d> Full<'d> {
    fn after(db: &'d Db, records: usize) -> Self {
        let left = Cell::new(records);
        Self { db, left }
    }
}

impl StepLog for Full<'_> {
    fn latest(&self) -> CommitVersion {
        self.db.latest()
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        let Some(left) = self.left.get().checked_sub(1) else {
            return Err("the log is full".into());
        };
        self.left.set(left);
        StepLog::append(&self.db, record)
    }
}

#[test]
fn the_cleaner_of_the_value_log_finds_and_repoints_the_cells_of_tree_pages() {
    let db = Db::new();
    let tree = tree_of(&db, 150, 150);
    let before = tree.scan(&db);
    let file = pointer_of(&before[0]).file();
    let mut to = db.close_value_file();
    let owners = db.owners();

    // A cell is found in the page its value's record names, as the page
    // version that is current. A pointer that no cell of the page holds, a
    // flat page and a page that does not exist own nothing.
    let (home, pointer) = (before[20].at.page(), pointer_of(&before[20]));
    let born = db.current(home).born();
    assert_eq!(owners.find(home, pointer).expect("a look"), Some(born));
    let other = before.iter().find(|read| read.at.page() != home);
    let other = other.expect("a second page").at.page();
    assert_eq!(owners.find(other, pointer).expect("a look"), None);
    assert_eq!(owners.find(page_id(9_999), pointer).expect("a look"), None);
    db.install_flat(page_id(5_000));
    assert_eq!(owners.find(page_id(5_000), pointer).expect("a look"), None);

    // While a write transaction holds a copy of a page, no pointer of it
    // is rewritten (`transactions.md` PV7): the pass moves the values of
    // the other pages, and leaves this one for later.
    let mut scope = db.scope();
    let mut held = tree.clone();
    held.put(&before[20].key, b"short");
    held.flush(&mut scope);
    let copied: Vec<PageId> = scope.pages.copies.keys().copied().collect();
    let busy = before
        .iter()
        .filter(|read| copied.contains(&read.at.page()))
        .count() as u64;
    assert!(busy > 0 && busy < 150);
    let clean = |to: &mut ValueWriter| {
        let mut manifest = db.manifest.borrow_mut();
        db.values.clean(file, to, &mut manifest, &owners)
    };
    let cleaned = clean(&mut to).expect("a pass");
    assert_eq!(
        (cleaned.moved, cleaned.busy, cleaned.live),
        (150 - busy, busy, busy)
    );
    assert_eq!(db.moves().len() as u64, 150 - busy);
    db.roll_back(scope);

    // The values of those pages move once nothing holds them.
    let cleaned = clean(&mut to).expect("a pass");
    assert_eq!((cleaned.moved, cleaned.busy, cleaned.live), (busy, 0, 0));
    assert!(cleaned.is_empty());
    let after = tree.scan(&db);
    for (now, then) in after.iter().zip(&before) {
        assert_eq!(
            (&now.key, &now.value, now.at),
            (&then.key, &then.value, then.at)
        );
        assert_eq!(pointer_of(now).file(), to.file_id());
    }
    db.check_counts();

    // Each rewrite is in the log as a background change of its page, from
    // the old pointer to the new one, in the slot of its cell, and the
    // page is owed to a segment again.
    let moves = db.moves();
    assert_eq!(moves.len(), 150);
    for step in &moves {
        let from = step.from().pointer().expect("a pointer");
        let cell = before.iter().find(|read| pointer_of(read) == from);
        let cell = cell.expect("the pointer of a cell");
        assert_eq!(
            (step.page(), step.slot()),
            (cell.at.page(), Some(cell.at.slot()))
        );
        assert_eq!(step.to().file(), to.file_id());
    }
    assert!(db.pool.oldest_unwritten().is_some());
    db.write_back();
    assert_eq!(db.pool.oldest_unwritten(), None);
    assert_eq!(values_of(&tree, &db), values_of(&tree, &db));
}

/// What becomes of a pointer rewrite besides a private copy, which the
/// test above has (`transactions.md` PV7, CL5): a page that a commit has
/// superseded since the look is looked at again and takes it, and it is
/// refused for a cell that no longer holds the pointer and for a page that
/// is gone.
#[test]
fn a_rewrite_is_refused_for_a_page_that_is_no_longer_as_it_was_looked_at() {
    let db = Db::new();
    let mut tree = tree_of(&db, 300, 300);
    let before = tree.scan(&db);
    let file = pointer_of(&before[0]).file();
    let mut to = db.close_value_file();
    let owners = db.owners();
    let copy = |to: &mut ValueWriter| {
        let copies = db.values.copy_live(file, to, &owners).expect("copied");
        db.cover(to);
        copies
    };

    // A commit supersedes pages: one cell gets another value, and the
    // pages on the way to it get a new version. Their steps are refused
    // and tried again with a fresh look, which moves their other values;
    // the cell's old value has no owner.
    let copies = copy(&mut to);
    assert_eq!(copies.len(), 300);
    let mut scope = db.scope();
    tree.put(&before[7].key, b"short");
    tree.flush(&mut scope);
    let written: Vec<PageId> = scope.pages.sink.runs.keys().copied().collect();
    db.commit(scope);
    let superseded = before
        .iter()
        .filter(|read| written.contains(&read.at.page()))
        .count() as u64;
    let cleaned = db.values.install(copies, &owners).expect("a pass");
    assert_eq!(
        (cleaned.moved, cleaned.busy, cleaned.gone, cleaned.live),
        (299, 0, 1, 0)
    );
    assert!(superseded > 1 && superseded < 300, "{superseded}");
    db.check_counts();

    // A later pass over the file the values went to. A cell whose pointer
    // another step replaced meanwhile is gone: here the regrouping of its
    // page.
    let mut next = db.close_value_file();
    let moved_to = db.finish(to);
    let copies = db
        .values
        .copy_live(moved_to, &mut next, &owners)
        .expect("copied");
    db.cover(&mut next);
    assert_eq!(copies.len(), 299);
    let page = before[7].at.page();
    let mut third = db.close_value_file();
    let regrouped = db.regroup(page, &mut third, RegroupPolicy::ALWAYS);
    let regrouped = regrouped.expect("regrouped");
    assert!(regrouped.moved > 0);
    let cleaned = db.values.install(copies, &owners).expect("a pass");
    assert_eq!(cleaned.gone, regrouped.moved);
    assert_eq!(cleaned.moved, 299 - regrouped.moved);
    assert_eq!(cleaned.live, 0, "the file holds no live value any more");
    db.check_counts();
    let values: Vec<Vec<u8>> = tree.scan(&db).into_iter().map(|read| read.value).collect();
    let expected: Vec<Vec<u8>> = before
        .iter()
        .map(|read| match read.key == before[7].key {
            true => b"short".to_vec(),
            false => read.value.clone(),
        })
        .collect();
    assert_eq!(values, expected);

    // A page that was freed since the look owns nothing: here every page
    // of the tree, whose keys a commit deleted.
    let mut last = db.close_value_file();
    let file = db.finish(next);
    let copies = db
        .values
        .copy_live(file, &mut last, &owners)
        .expect("copied");
    db.cover(&mut last);
    let live = copies.len() as u64;
    assert!(live > 0);
    let mut scope = db.scope();
    for read in &before {
        assert!(tree.delete(&read.key));
        tree.flush(&mut scope);
    }
    db.commit(scope);
    assert_eq!(db.pages(), []);
    let cleaned = db.values.install(copies, &owners).expect("a pass");
    assert_eq!((cleaned.moved, cleaned.gone, cleaned.live), (0, live, 0));
    assert_eq!(db.values.counts(), []);
}

/// The first sentence of the task, for one page: after its values are
/// regrouped, a scan of the page reads them as one sequential read.
#[test]
fn regrouping_puts_the_values_of_a_page_back_to_back_in_scan_order() {
    let db = Db::traced();
    let tree = tree_of(&db, 150, 30);
    let before = tree.scan(&db);
    let mut to = db.close_value_file();
    let page = before[70].at.page();
    let was = locality(&db, page);
    assert!(was.values > 10 && was.runs > was.values / 2, "{was:?}");
    let old = in_scan_order(&db, page);

    let regrouped = db.regroup(page, &mut to, RegroupPolicy::ALWAYS);
    let regrouped = regrouped.expect("regrouped");
    assert_eq!(
        (regrouped.moved, regrouped.gone, regrouped.busy),
        (was.values, 0, 0)
    );
    assert_eq!(regrouped.before, was);
    assert_eq!(regrouped.after, locality(&db, page));
    assert_eq!(
        (regrouped.after.values, regrouped.after.runs),
        (was.values, 1)
    );
    assert_eq!(regrouped.after.bytes, was.bytes);

    // The cells hold pointers into the other file, back to back, in the
    // order a scan reaches them, and no cell has moved.
    let new = in_scan_order(&db, page);
    let mut end = new[0].offset();
    for (pointer, old) in new.iter().zip(&old) {
        assert_eq!((pointer.file(), pointer.offset()), (to.file_id(), end));
        assert_eq!(pointer.value_len(), old.value_len());
        end = pointer.end(FormatVersion::CURRENT);
    }
    assert_eq!(end, to.end());
    let after = tree.scan(&db);
    for (now, then) in after.iter().zip(&before) {
        assert_eq!(
            (&now.key, &now.value, now.at),
            (&then.key, &then.value, then.at)
        );
    }

    // The counts moved with the pointers, and each move is in the log as a
    // background change of the page.
    db.check_counts();
    assert_eq!(
        db.values.count(to.file_id()).map(|count| count.values),
        Some(was.values)
    );
    let moves = db.moves();
    assert_eq!(moves.len() as u64, was.values);
    for (step, (from, to)) in moves.iter().zip(old.iter().zip(&new)) {
        assert_eq!(step.page(), page);
        assert_eq!((step.from().pointer(), step.to()), (Some(*from), *to));
    }

    // A scan of the page's records reads the file from one end of the
    // group to the other.
    let records: Vec<&Scanned> = after.iter().filter(|read| read.at.page() == page).collect();
    let reader = NodeReader::new(db.loader(), &Guard, Lease::take(), db.latest());
    let from = db.fs.trace().len();
    let mut buf = ValueBuf::new();
    let view = db.current(page);
    let view = TreePage::from_validated(&view, FRAME_FORMAT).expect("a tree page");
    for slot in scan_order(&view) {
        let node = reader
            .node(CellAddress::new(page, slot), &mut LoadBuffers::new())
            .expect("the node");
        read_value(&db.values, &node, &mut buf).expect("read");
    }
    let reads = value_reads(&db, from);
    assert_eq!(reads.len(), records.len());
    for pair in reads.windows(2) {
        assert_eq!(pair[1].0, pair[0].0, "one file");
        assert_eq!(
            pair[1].1,
            pair[0].1 + pair[0].2,
            "the next read begins where the last ended"
        );
    }

    // Values that lie in one run are left alone, and so is a page with no
    // value in the value log, a flat page and a page that does not exist.
    let owners = db.owners();
    let mut again = |page, policy| {
        let grouped = owners.copy_grouped(&db.values, page, &mut to, policy);
        grouped.expect("a look").is_some()
    };
    assert!(!again(page, RegroupPolicy::ALWAYS));
    assert!(!again(page_id(9_999), RegroupPolicy::ALWAYS));
    db.install_flat(page_id(5_000));
    assert!(!again(page_id(5_000), RegroupPolicy::ALWAYS));
    let scattered = before.iter().find(|read| read.at.page() != page);
    let scattered = scattered.expect("a second page").at.page();
    assert!(!again(scattered, RegroupPolicy::new(101)));
    assert!(again(scattered, RegroupPolicy::DEFAULT));
    check_attached(&db);
}

/// A regrouping installs nothing that the page steps refuse: before the
/// copies are covered, under a private copy, on a page a commit has
/// superseded, and when the log does not take the record.
#[test]
fn a_regrouping_that_is_refused_leaves_the_page_and_the_counts_as_they_were() {
    let db = Db::new();
    let mut tree = tree_of(&db, 150, 30);
    let before = tree.scan(&db);
    let page = before[70].at.page();
    let old = in_scan_order(&db, page);
    let counts = db.values.counts();
    let mut to = db.close_value_file();
    let owners = db.owners();
    let unchanged = || {
        assert_eq!(in_scan_order(&db, page), old);
        assert_eq!(db.values.counts(), counts);
        db.check_counts();
    };
    let copy = |to: &mut ValueWriter| {
        let grouped = owners.copy_grouped(&db.values, page, to, RegroupPolicy::ALWAYS);
        grouped.expect("copied").expect("the values are scattered")
    };

    // No durable manifest covers the copies yet.
    let grouped = copy(&mut to);
    assert_eq!(
        (grouped.page(), grouped.len(), grouped.is_empty()),
        (page, old.len(), false)
    );
    assert_eq!(grouped.before(), locality(&db, page));
    let refused = owners.install_grouped(&db.values, grouped);
    let refused = refused.expect_err("not covered");
    assert!(
        matches!(refused, ValueError::NotCovered { page: of } if of == page),
        "{refused}"
    );
    assert_eq!(
        refused.to_string(),
        format!("no durable manifest covers the regrouped values of page {page}")
    );
    unchanged();

    // A write transaction holds a private copy of the page.
    let grouped = copy(&mut to);
    db.cover(&mut to);
    let mut scope = db.scope();
    scope.pages.write(page, |_| Ok(())).expect("copied");
    let refused = owners.install_grouped(&db.values, grouped).expect("a pass");
    assert_eq!(
        (refused.moved, refused.gone, refused.busy),
        (0, 0, old.len() as u64)
    );
    let lens = old.iter().map(|pointer| u64::from(pointer.value_len()));
    assert_eq!(refused.left_bytes, lens.sum::<u64>());
    assert_eq!(refused.after, refused.before);
    db.roll_back(scope);
    unchanged();

    // The log does not take the record of the step: the page keeps its
    // pointers and nothing is counted.
    let failing = TreeOwners::new(db.loader(), &db.pool, SIZE, Full::after(&db, 0));
    let grouped = failing
        .copy_grouped(&db.values, page, &mut to, RegroupPolicy::ALWAYS)
        .expect("copied")
        .expect("scattered");
    db.cover(&mut to);
    let refused = failing.install_grouped(&db.values, grouped);
    let refused = refused.expect_err("the log is full");
    assert!(matches!(refused, ValueError::Log(_)), "{refused}");
    assert_eq!(refused.to_string(), "log: the log is full");
    assert!(std::error::Error::source(&refused).is_some());
    unchanged();

    // A commit supersedes the page between the copy and the steps.
    let grouped = copy(&mut to);
    db.cover(&mut to);
    let mut scope = db.scope();
    tree.put(&before[70].key, b"short");
    tree.flush(&mut scope);
    db.commit(scope);
    let refused = owners.install_grouped(&db.values, grouped).expect("a pass");
    assert_eq!(
        (refused.moved, refused.gone, refused.busy),
        (0, 0, old.len() as u64)
    );
    db.check_counts();
    let mut expected = old.clone();
    expected.retain(|pointer| *pointer != pointer_of(&before[70]));
    assert_eq!(in_scan_order(&db, page), expected);

    // The page as it is now is regrouped by a later pass.
    let regrouped = db.regroup(page, &mut to, RegroupPolicy::ALWAYS);
    let regrouped = regrouped.expect("regrouped");
    assert_eq!(regrouped.moved, expected.len() as u64);
    assert!(regrouped.after.is_sequential());
    db.check_counts();
}

/// A regrouping whose step fails keeps the steps it made before: each is
/// in the log, in its page and in the counts, and the others are not.
#[test]
fn a_regrouping_that_fails_part_way_keeps_the_steps_it_made() {
    let db = Db::new();
    let tree = tree_of(&db, 150, 30);
    let before = tree.scan(&db);
    let page = before[70].at.page();
    let old = in_scan_order(&db, page);
    let mut to = db.close_value_file();

    let failing = TreeOwners::new(db.loader(), &db.pool, SIZE, Full::after(&db, 3));
    let grouped = failing
        .copy_grouped(&db.values, page, &mut to, RegroupPolicy::ALWAYS)
        .expect("copied")
        .expect("scattered");
    db.cover(&mut to);
    let refused = failing.install_grouped(&db.values, grouped);
    let refused = refused.expect_err("the log is full");
    assert!(matches!(refused, ValueError::Log(_)), "{refused}");
    let now = in_scan_order(&db, page);
    assert!(now[..3]
        .iter()
        .all(|pointer| pointer.file() == to.file_id()));
    assert_eq!(now[3..], old[3..]);
    assert_eq!(db.moves().len(), 3);
    assert_eq!(
        db.values.count(to.file_id()).map(|count| count.values),
        Some(3)
    );
    db.check_counts();
    assert_eq!(values_of(&tree, &db).len(), 150);
    assert_eq!(tree.scan(&db), {
        let mut expected = before.clone();
        for read in expected.iter_mut().filter(|read| read.at.page() == page) {
            let at = old.iter().position(|pointer| *pointer == pointer_of(read));
            read.held = Some(ValuePlace::Placed(now[at.expect("a value of the page")]));
        }
        expected
    });
}

/// A step that the counts did not take as reported is made all the same,
/// and so are the steps after it. The call then fails with that error,
/// also if a later step failed for another reason.
#[test]
fn a_count_that_is_wrong_is_reported_before_any_other_error() {
    let db = Db::new();
    let tree = tree_of(&db, 150, 30);
    let before = tree.scan(&db);
    let mut to = db.close_value_file();

    // The counts lose every value, while the cells go on pointing.
    let mut lost = ValueChange::new();
    for (_, pointer) in db.pointers() {
        lost.release(pointer);
    }
    db.values.apply(&lost, db.latest()).expect("counted");

    let miscounted =
        |error: &ValueError| matches!(error, ValueError::Store(ValueStoreError::Miscounted { .. }));
    let page = before[70].at.page();
    let owners = db.owners();
    let grouped = owners
        .copy_grouped(&db.values, page, &mut to, RegroupPolicy::ALWAYS)
        .expect("copied")
        .expect("scattered");
    db.cover(&mut to);
    let refused = owners.install_grouped(&db.values, grouped);
    let refused = refused.expect_err("miscounted");
    assert!(miscounted(&refused), "{refused}");
    let now = in_scan_order(&db, page);
    assert!(now.iter().all(|pointer| pointer.file() == to.file_id()));
    assert_eq!(db.moves().len(), now.len());

    let other = before.iter().find(|read| read.at.page() != page);
    let other = other.expect("a second page").at.page();
    let failing = TreeOwners::new(db.loader(), &db.pool, SIZE, Full::after(&db, 2));
    let grouped = failing
        .copy_grouped(&db.values, other, &mut to, RegroupPolicy::ALWAYS)
        .expect("copied")
        .expect("scattered");
    db.cover(&mut to);
    let refused = failing.install_grouped(&db.values, grouped);
    let refused = refused.expect_err("miscounted, and then the log is full");
    assert!(miscounted(&refused), "{refused}");
    let moved = in_scan_order(&db, other);
    let moved = moved
        .iter()
        .filter(|pointer| pointer.file() == to.file_id());
    assert_eq!(moved.count(), 2);
    assert_eq!(values_of(&tree, &db).len(), 150);
}

/// A log of page steps that can be shared between threads, which is what
/// write-back asks of what it is handed.
struct SharedLog {
    latest: CommitVersion,
    records: std::sync::Mutex<Vec<Vec<u8>>>,
}

impl StepLog for SharedLog {
    fn latest(&self) -> CommitVersion {
        self.latest
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        self.records.lock().expect("the log").push(record.to_vec());
        Ok(())
    }
}

/// Write-back is in `lignin-store`, which knows no tree. It reaches the
/// scan order of a page and the regrouping of its values through the trait
/// it defines, and [`Regrouping`] is what stands behind it.
#[test]
fn write_back_reaches_the_scan_order_and_the_regrouping_through_its_trait() {
    use lignin_store::flush::{FlushError, PageValues, Regroup};

    let db = Db::traced();
    let tree = tree_of(&db, 150, 30);
    let before = tree.scan(&db);
    let mut to = db.close_value_file();
    let page = before[70].at.page();
    let was = locality(&db, page);
    assert!(was.values > 10 && !was.is_sequential(), "{was:?}");
    let log = SharedLog {
        latest: db.latest(),
        records: Default::default(),
    };
    let always = RegroupPolicy::ALWAYS;
    let regrouping = Regrouping::new(db.loader(), &db.pool, &db.values, SIZE, &log, always);
    let through: &dyn PageValues = &regrouping;

    // The order in which write-back appends the pending values of a page.
    let frame = db.current(page);
    let mut order = Vec::new();
    through.scan_order(&frame, &mut order);
    let view = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
    assert_eq!(order, scan_order(&view));
    order.clear();
    through.scan_order(&[0xFF; 64], &mut order);
    assert!(
        order.is_empty(),
        "bytes that are no tree page have no order"
    );
    drop(frame);

    // A regrouping whose copies cannot be covered installs nothing.
    let mut refuse = |_: &mut ValueWriter| Err(FlushError::SegmentsNotDurable);
    let refused = through.regroup(page, &mut to, &mut refuse);
    assert_eq!(
        refused.expect_err("not covered").to_string(),
        FlushError::SegmentsNotDurable.to_string()
    );
    assert_eq!(locality(&db, page), was);
    assert!(log.records.lock().expect("the log").is_empty());

    // The regrouping, with the copies covered between its two halves as
    // write-back covers them.
    let covered = std::cell::Cell::new(0);
    let mut cover = |to: &mut ValueWriter| {
        covered.set(covered.get() + 1);
        db.cover(to);
        Ok(())
    };
    let moved = through
        .regroup(page, &mut to, &mut cover)
        .expect("regrouped");
    assert_eq!((moved, covered.get()), (Regroup::moved(was.values), 1));
    let moved = moved.moved;
    let now = locality(&db, page);
    assert_eq!((now.values, now.runs), (was.values, 1));
    assert_eq!(log.records.lock().expect("the log").len() as u64, moved);
    assert_eq!(values_of(&tree, &db).len(), before.len());
    db.check_counts();

    // A page whose values lie in one run is left alone, and nothing is
    // covered for it.
    let again = through
        .regroup(page, &mut to, &mut cover)
        .expect("nothing to do");
    assert_eq!((again, covered.get()), (Regroup::default(), 1));
}
