//! Page id allocation beside the page table, as the layers above use the
//! two: a snapshot keeps reading its page while the id of that page is
//! freed, held back, and then used for another page.

use std::sync::Mutex;

use lignin_format::{CommitVersion, FileId, PageId};
use lignin_store::{
    alloc::{AllocError, Epoch, PageAllocator, PageState, Pins, Recovery},
    segment::BlockLocation,
    table::{Link, PageTable},
};

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

/// A location for the block of `page` that the commit of `raw_version`
/// wrote. No block is there: neither the table nor the allocator reads one.
fn block(page: PageId, raw_version: u64) -> BlockLocation {
    BlockLocation {
        file: FileId::new(1).expect("not zero"),
        offset: 20 + 4096 * (u64::from(page.get()) * 100 + raw_version),
        len: 4096,
    }
}

#[derive(Default)]
struct Held {
    latest: u64,
    snapshots: Vec<u64>,
    epoch: u64,
    operations: Vec<u64>,
}

/// The version pins and operation guards of a session, kept under a lock.
#[derive(Default)]
struct Session {
    held: Mutex<Held>,
}

impl Session {
    fn with<R>(&self, change: impl FnOnce(&mut Held) -> R) -> R {
        change(&mut self.held.lock().expect("no thread panicked"))
    }

    fn publish(&self, latest: u64) {
        self.with(|held| held.latest = latest);
    }

    fn snapshot(&self) -> u64 {
        self.with(|held| {
            held.snapshots.push(held.latest);
            held.latest
        })
    }

    fn drop_snapshot(&self, pinned: u64) {
        self.with(|held| {
            let at = held.snapshots.iter().position(|pin| *pin == pinned);
            held.snapshots.remove(at.expect("it is held"));
        });
    }

    fn begin_operation(&self) -> u64 {
        self.with(|held| {
            held.operations.push(held.epoch);
            held.epoch
        })
    }

    fn end_operation(&self, epoch: u64) {
        self.with(|held| {
            let at = held.operations.iter().position(|guard| *guard == epoch);
            held.operations.remove(at.expect("it is held"));
        });
    }
}

impl Pins for Session {
    fn oldest_pin(&self) -> CommitVersion {
        self.with(|held| version(held.snapshots.iter().copied().min().unwrap_or(held.latest)))
    }

    fn next_epoch(&self) -> Epoch {
        self.with(|held| {
            held.epoch += 1;
            Epoch::new(held.epoch)
        })
    }

    fn oldest_guard(&self) -> Epoch {
        self.with(|held| Epoch::new(held.operations.iter().copied().min().unwrap_or(held.epoch)))
    }
}

fn reclaim(ids: &PageAllocator, session: &Session) -> Vec<PageId> {
    let mut released = Vec::new();
    ids.reclaim(session, &mut released);
    released
}

fn read(table: &PageTable, page: PageId, at: u64) -> Option<Vec<BlockLocation>> {
    let mut chain = Vec::new();
    table.read(page, version(at), &mut chain).map(|_| chain)
}

#[test]
fn a_snapshot_reads_its_page_until_it_is_dropped_and_then_the_id_names_another() {
    let ids = PageAllocator::new();
    let table = PageTable::new();
    let session = Session::default();

    // Commit 1 creates two pages.
    let first = ids.allocate().expect("an id");
    let second = ids.allocate().expect("an id");
    for page in [first, second] {
        ids.publish(page).expect("it is private");
        table
            .push(page, Link::base(version(1), block(page, 1)))
            .expect("a first link");
    }
    session.publish(1);
    let snapshot = session.snapshot();

    // Commit 2 frees the first page.
    ids.free(first, version(2)).expect("it is live");
    table
        .push(first, Link::free(version(2)))
        .expect("the page was not freed before");
    session.publish(2);

    // The snapshot can reach the page, so its id stays out of use, through
    // any number of reclaims and commits.
    assert_eq!(reclaim(&ids, &session), []);
    let third = ids.allocate().expect("an id");
    assert_eq!(third.get(), 3);
    ids.publish(third).expect("it is private");
    table
        .push(third, Link::base(version(3), block(third, 3)))
        .expect("a first link");
    session.publish(3);
    assert_eq!(reclaim(&ids, &session), []);
    assert_eq!(ids.state(first), PageState::Retired);
    assert_eq!(read(&table, first, snapshot), Some(vec![block(first, 1)]));
    assert_eq!(read(&table, first, 3), None);

    // An operation that began under the snapshot still holds the id when
    // the snapshot is dropped.
    let operation = session.begin_operation();
    session.drop_snapshot(snapshot);
    assert_eq!(reclaim(&ids, &session), []);
    session.end_operation(operation);
    assert_eq!(reclaim(&ids, &session), [first]);

    // Whoever trims the table finds nothing left of the page, and the id
    // names a new page from commit 4 on.
    let mut removed = Vec::new();
    assert!(table.trim(first, version(3), &mut removed));
    assert_eq!(removed, [Link::base(version(1), block(first, 1))]);
    assert_eq!(ids.allocate(), Ok(first));
    ids.publish(first).expect("it is private");
    table
        .push(first, Link::base(version(4), block(first, 4)))
        .expect("a first link");
    session.publish(4);
    assert_eq!(read(&table, first, 4), Some(vec![block(first, 4)]));
}

#[test]
fn a_transaction_that_rolls_back_leaves_no_trace() {
    let ids = PageAllocator::new();
    let session = Session::default();
    let snapshot = session.snapshot();
    let held: Vec<_> = (0..3).map(|_| ids.allocate().expect("an id")).collect();
    for page in &held {
        ids.release(*page).expect("it is private");
    }
    assert_eq!(ids.census().private, 0);
    assert_eq!(ids.allocate(), Ok(held[0]));
    assert_eq!(
        ids.publish(held[1]),
        Err(AllocError::NotPrivate {
            page: held[1],
            state: PageState::Free
        })
    );
    session.drop_snapshot(snapshot);
}

#[test]
fn the_next_session_derives_the_allocator_from_the_table_and_the_log() {
    let table = PageTable::new();
    let page = |raw| PageId::new(raw).expect("not zero");
    // The checkpoint covers version 2: page 1 lives, page 2 was freed by
    // commit 2, and page 3 was freed and trimmed away before.
    table
        .push(page(1), Link::base(version(1), block(page(1), 1)))
        .expect("a first link");
    table
        .push(page(2), Link::base(version(1), block(page(2), 1)))
        .expect("a first link");
    table
        .push(page(2), Link::free(version(2)))
        .expect("the page was not freed before");

    // The log after it: commit 3 created page 3 again and page 4, and
    // commit 4 freed page 1.
    let mut replay = Recovery::new();
    replay.wrote(page(3), version(3));
    replay.wrote(page(4), version(3));
    replay.freed(page(1), version(4));
    let ids = replay.finish(&table);

    let states: Vec<_> = (1..=5).map(|raw| ids.state(page(raw))).collect();
    assert_eq!(
        states,
        [
            PageState::Retired,
            PageState::Retired,
            PageState::Live,
            PageState::Live,
            PageState::Free
        ]
    );
    let session = Session::default();
    session.publish(4);
    assert_eq!(ids.allocate(), Ok(page(5)));
    assert_eq!(reclaim(&ids, &session), [page(2), page(1)]);
    assert_eq!(ids.allocate(), Ok(page(1)));
}
