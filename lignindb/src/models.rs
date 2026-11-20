//! Models of what the parts of the engine do to each other across the
//! crates' borders, each on the code of both sides (VER-05;
//! `transactions.md` section 15.2, "Loom models owed by VER-05").
//!
//! The buffer pool, the page id allocator and the page table are
//! `lignin-store`'s, and what they are asked about, the version pins and
//! the operation guards, is this crate's ([`Versions`]). The models of each
//! side run it against a stand-in of the other: the pool's races
//! (`lignin-store` `cache::tests::races`) against a counter that plays the
//! versions, and the pins' (`crate::snapshot`, `tests::loom`) against a flag
//! that plays a page version. Here the two run together: a reader takes its
//! pin from the real [`Versions`] and its page from the real pool, a commit
//! installs in the one and publishes in the other, and the reclaim that the
//! versions start is the pool's and the allocator's own.
//!
//! Each model is written once and run two ways:
//!
//! - **Under loom**, which runs both crates' code under the interleavings
//!   it explores:
//!   `RUSTFLAGS="--cfg loom" cargo test -p lignindb --release --lib models::`
//!   (`.github/scripts/lignin-ci.sh loom`).
//! - **On real threads**, in every normal test run, each race many times.
//!
//! The page table and the allocator take locks of `std`, also under loom.
//! Each of their calls is one hold of one lock and calls nothing of loom
//! inside it (the allocator asks its three questions before it locks), so
//! under loom a call of theirs is one step, and loom chooses the order of
//! those steps wherever a call of the pool or of the versions stands
//! between them.
//!
//! The models find a reader, a write-back and a freer that break their
//! rule: the modules `broken` run the same models against them, under loom,
//! and each must fail.

#[cfg(loom)]
use ::loom as rt;
#[cfg(not(loom))]
use std as rt;

use lignin_format::{CommitVersion, FileId, PageId, PageSize};
use lignin_store::{
    alloc::{PageAllocator, PageState},
    cache::{BufferPool, CacheError, OwnerId, PoolConfig},
    segment::BlockLocation,
    table::{Link, LinkError, LinkKind, PageTable},
};
use rt::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};

use crate::snapshot::{Reclaim, Versions};

/// A model that loom explores up to `preemptions` preemptions.
/// `LOOM_MAX_PREEMPTIONS` sets another bound for all of them.
#[cfg(loom)]
fn model(preemptions: usize, body: impl Fn() + Sync + Send + 'static) {
    let mut builder = ::loom::model::Builder::new();
    if builder.preemption_bound.is_none() {
        builder.preemption_bound = Some(preemptions);
    }
    builder.max_branches = 200_000;
    builder.check(body);
}

/// Without loom the race is run on real threads, `LIGNIN_RACE_ROUNDS` times
/// or a few hundred.
#[cfg(not(loom))]
fn model(_preemptions: usize, body: impl Fn() + Sync + Send + 'static) {
    let rounds = std::env::var("LIGNIN_RACE_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(if cfg!(miri) { 4 } else { 400 });
    for _ in 0..rounds {
        body();
    }
}

/// A flag that is not set, stored the way every later value is: loom gives
/// a new atomic its first value as a weaker store, which a `SeqCst` load
/// could still return after a later `SeqCst` store.
fn flag() -> AtomicBool {
    let flag = AtomicBool::new(false);
    flag.store(false, Ordering::SeqCst);
    flag
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn page() -> PageId {
    PageId::new(1).expect("a page id")
}

fn pool() -> Arc<BufferPool> {
    let size = PageSize::MIN;
    let pool = BufferPool::new(PoolConfig {
        page_size: size,
        capacity: 4 * (size.get() + 512),
    });
    Arc::new(pool.expect("a pool"))
}

fn join(threads: Vec<thread::JoinHandle<()>>) {
    for thread in threads {
        thread.join().expect("the thread did not panic");
    }
}

/// What the versions of a model have reclaimed when the oldest pin may
/// have moved up: the page versions that the pool holds for readers.
struct PoolHook(Arc<BufferPool>);

impl Reclaim for PoolHook {
    fn reclaim(&self, pins: &Versions) {
        self.0.reclaim(pins);
    }
}

/// A store at version 1 whose one page has one version, which no segment
/// holds: the pool is where a reader finds it.
struct Store {
    versions: Versions,
    pool: Arc<BufferPool>,
    /// Whether write-back had version 1 of the page held for readers.
    held: AtomicBool,
}

/// What a page version of the models holds: the number of its commit, in
/// every byte.
fn fill(version: u64) -> u8 {
    version as u8
}

impl Store {
    fn new() -> Arc<Self> {
        let pool = pool();
        let copy = pool
            .new_private(OwnerId::new(1), page())
            .expect("a new page");
        copy.write().expect("a private copy").fill(fill(1));
        drop(pool.install(&copy, version(1)).expect("the first commit"));
        drop(copy);
        Arc::new(Self {
            versions: Versions::with_reclaim(version(1), Box::new(PoolHook(pool.clone()))),
            pool,
            held: flag(),
        })
    }

    /// Commit 2: a private copy of the page is written, installed as the
    /// next version of the page, and the version is published (CM3 steps 7
    /// and 8). Then the commit asks for a reclaim, as every commit does.
    fn commit(&self) {
        let copy = (self.pool.make_private(OwnerId::new(2), page(), version(1)))
            .unwrap_or_else(|_| panic!("the page is in the pool"));
        copy.write().expect("a private copy").fill(fill(2));
        drop(self.pool.install(&copy, version(2)).expect("a commit"));
        drop(copy);
        self.versions.publish(version(2)).expect("the next version");
        self.versions.reclaim();
    }

    /// Write-back finds version 1 of the page among what it owes. Once the
    /// page is superseded it has that version held for the readers that
    /// can still ask for it, in place of writing it (FL6, FL7), and asks
    /// for a reclaim.
    fn write_back(&self) {
        if let Some(frame) = self.pool.get_version(page(), version(1)) {
            match self.pool.hold_for_readers(&frame) {
                Ok(()) => self.held.store(true, Ordering::SeqCst),
                // The commit has not installed yet: the page version is
                // the current one, and stays owed.
                Err(CacheError::StillCurrent { .. }) => {}
                Err(error) => panic!("the hold of a page version that is owed: {error}"),
            }
        }
        self.versions.reclaim();
    }

    /// A read of the page at the version `at`: the reader is given the
    /// page version of its version from the pool, whole. No segment holds
    /// it, so a miss is a page version that was reclaimed under a reader.
    fn read(&self, at: CommitVersion) {
        match self.pool.get(page(), at) {
            Ok(frame) => {
                let expected = fill(at.get());
                assert!(
                    frame.first() == Some(&expected) && frame.last() == Some(&expected),
                    "a reader of version {at} was given another version of its page"
                );
            }
            Err(_) => panic!("a reader of version {at} found its page version reclaimed"),
        }
    }
}

/// A commit, write-back with a reclaim, and a reader, each on a thread of
/// its own, on the pool's and the versions' own code.
fn readers_race(reader: impl Fn(&Store) + Send + Sync + Copy + 'static) {
    model(3, move || {
        let store = Store::new();
        let threads = vec![
            thread::spawn({
                let store = store.clone();
                move || store.commit()
            }),
            thread::spawn({
                let store = store.clone();
                move || store.write_back()
            }),
            thread::spawn({
                let store = store.clone();
                move || reader(&store)
            }),
        ];
        join(threads);
        // With every reader gone: the current version stays, and what was
        // held of the version before goes after two reclaims at most, the
        // second for the operations that ran during the first.
        store.versions.reclaim();
        store.versions.reclaim();
        store.read(version(2));
        if store.held.load(Ordering::SeqCst) {
            assert!(
                store.pool.get(page(), version(1)).is_err(),
                "a page version that was held for readers stays with no reader left"
            );
        }
        assert_eq!(store.versions.pins_held(), 0);
    });
}

/// PN4, PN6, PV3 and FL6 on the code of both crates: a reader that pins a
/// version reads the page version of that version from the pool, whenever
/// the commit installs and publishes, write-back has the superseded
/// version held and the pool reclaims it.
#[test]
fn a_pinned_reader_is_given_its_page_version_through_a_commit_and_a_reclaim() {
    readers_race(|store| {
        let pin = store.versions.pin();
        store.read(pin.version());
        drop(pin);
    });
}

/// PN5: the same for an operation that pins nothing and reads the latest
/// version under its guard.
#[test]
fn an_operation_is_given_its_page_version_through_a_commit_and_a_reclaim() {
    readers_race(|store| {
        let guard = store.versions.guard();
        store.read(guard.latest());
        drop(guard);
    });
}

/// A snapshot whose pin goes while its operation still reads: the guard
/// alone keeps what the operation reads.
#[test]
fn an_operation_outlives_the_pin_of_its_snapshot() {
    readers_race(|store| {
        let pin = store.versions.pin();
        let guard = store.versions.guard();
        let at = pin.version();
        drop(pin);
        store.read(at);
        drop(guard);
    });
}

/// What the versions of a model have reclaimed: the page ids that no
/// reader can reach any more.
struct IdsHook(Arc<PageAllocator>);

impl Reclaim for IdsHook {
    fn reclaim(&self, pins: &Versions) {
        self.0.reclaim(pins, &mut Vec::new());
    }
}

/// A store at version 1 with one page, whose id commit 2 frees and commit
/// 3 may be given again.
struct Ids {
    versions: Versions,
    ids: Arc<PageAllocator>,
    page: PageId,
    /// Whether the id of the page names another page by now.
    reused: AtomicBool,
}

impl Ids {
    fn new() -> Arc<Self> {
        let ids = Arc::new(PageAllocator::new());
        let page = ids.allocate().expect("an id");
        ids.publish(page).expect("it is private");
        Arc::new(Self {
            versions: Versions::with_reclaim(version(1), Box::new(IdsHook(ids.clone()))),
            ids,
            page,
            reused: flag(),
        })
    }

    /// Commit 2 frees the page and is published (CM3 step 7, ID2), and
    /// asks for a reclaim. Commit 3 then makes a page, under whatever id
    /// the allocator gives it.
    fn commits(&self) {
        self.ids
            .free(self.page, version(2))
            .expect("the page is in use");
        self.versions.publish(version(2)).expect("the next version");
        self.versions.reclaim();
        let made = self.ids.allocate().expect("an id");
        if made == self.page {
            self.reused.store(true, Ordering::SeqCst);
        }
    }

    /// A read of the page at the version `at`: a reader below the freeing
    /// version follows the id to the page it names at its version, so the
    /// id must not name another page yet (ID2, T-PN3).
    fn read(&self, at: CommitVersion) {
        if at < version(2) {
            assert!(
                !self.reused.load(Ordering::SeqCst),
                "the id of a page was given out again under a reader of version {at}"
            );
        }
    }
}

fn ids_race(reader: impl Fn(&Ids) + Send + Sync + Copy + 'static) {
    model(3, move || {
        let store = Ids::new();
        let threads = vec![
            thread::spawn({
                let store = store.clone();
                move || store.commits()
            }),
            thread::spawn({
                let store = store.clone();
                move || store.versions.reclaim()
            }),
            thread::spawn({
                let store = store.clone();
                move || reader(&store)
            }),
        ];
        join(threads);
        // With every reader gone the id is free again, or in use again.
        store.versions.reclaim();
        store.versions.reclaim();
        assert_ne!(store.ids.state(store.page), PageState::Retired);
    });
}

/// ID2, ID3 and the three questions of PN6 on the allocator's and the
/// versions' own code: the id of a page that a commit freed is not given
/// out again while a reader that pinned a version below the freeing one
/// reads.
#[test]
fn a_page_id_is_not_given_out_again_under_a_pinned_reader() {
    ids_race(|store| {
        let pin = store.versions.pin();
        store.read(pin.version());
        drop(pin);
    });
}

/// The same for an operation under its guard.
#[test]
fn a_page_id_is_not_given_out_again_under_an_operation() {
    ids_race(|store| {
        let guard = store.versions.guard();
        store.read(guard.latest());
        drop(guard);
    });
}

fn block(file: u32) -> BlockLocation {
    BlockLocation {
        file: FileId::new(file).expect("a file id"),
        offset: 64,
        len: 128,
    }
}

/// D-116, `page-table.md` T18: write-back holds a page version that it
/// owes while the page is freed. The page was made under an id that named
/// another page before, so the newest link of the id is that page's free
/// link, and the table gives a freer no second one (`AlreadyFree`).
///
/// Write-back looks again, under a page step, before it pushes the link of
/// the block it wrote (`looks_again`), and the freer pushes its free link
/// again once the pool is told, if the first push was refused
/// (`pushes_again`). With both, whichever way the two interleave, the page
/// table ends with a free link as the newest of the id: it says of the id
/// what the allocator says.
fn a_page_is_freed_while_write_back_holds_it(looks_again: bool, pushes_again: bool) {
    model(3, move || {
        let pool = pool();
        let table = Arc::new(PageTable::new());
        // The first life of the id: a page that a segment held, freed at
        // version 2.
        table
            .push(page(), Link::base(version(1), block(1)))
            .expect("a link");
        table
            .push(page(), Link::free(version(2)))
            .expect("a free link");
        // The second life: a page made at version 3, which no segment
        // holds yet.
        let copy = pool
            .new_private(OwnerId::new(1), page())
            .expect("a new page");
        copy.write().expect("a private copy").fill(fill(3));
        drop(pool.install(&copy, version(3)).expect("a commit"));
        drop(copy);

        let write_back = thread::spawn({
            let (pool, table) = (pool.clone(), table.clone());
            move || {
                // The pass has the page version in hand, and has written
                // its block.
                let Some(frame) = pool.get_version(page(), version(3)) else {
                    return;
                };
                let step = pool.page_step(page());
                if !looks_again || step.owes(frame.born()) {
                    match table.push(page(), Link::base(version(3), block(2))) {
                        // A free link above the version refuses the block.
                        Ok(()) | Err(LinkError::OutOfOrder { .. }) => {}
                        Err(error) => panic!("the link of the block: {error}"),
                    }
                }
                drop(step);
            }
        });
        let freer = thread::spawn({
            let (pool, table) = (pool.clone(), table.clone());
            move || {
                // Commit 4 frees the page: the free link, the pool, and
                // the free link again if the first was not taken.
                let free = || match table.push(page(), Link::free(version(4))) {
                    Ok(()) => false,
                    Err(LinkError::AlreadyFree) => true,
                    Err(error) => panic!("the free link: {error}"),
                };
                let never_linked = free();
                pool.free(page(), version(4))
                    .expect("the page is freed once");
                if never_linked && pushes_again {
                    free();
                }
            }
        });
        join(vec![write_back, freer]);
        let newest = table.newest(page()).expect("the id has links");
        assert_eq!(
            newest.kind,
            LinkKind::Free,
            "the page table says that the id of a freed page is in use"
        );
    });
}

#[test]
fn a_page_that_is_freed_while_write_back_holds_it_stays_freed() {
    a_page_is_freed_while_write_back_holds_it(true, true);
}

/// The models find what breaks the rules. These run under loom only: on
/// real threads the windows are a few instructions wide.
#[cfg(loom)]
mod broken {
    use super::*;

    /// PN4's forbidden design: the reader reads the latest version, and
    /// reads at it once its pin is announced, without looking again.
    #[test]
    #[should_panic(expected = "found its page version reclaimed")]
    fn a_reader_that_reads_the_version_before_it_pins_is_found() {
        readers_race(|store| {
            let at = store.versions.latest();
            let pin = store.versions.pin();
            store.read(at);
            drop(pin);
        });
    }

    /// PN5: an operation that reads the latest version before it takes its
    /// guard.
    #[test]
    #[should_panic(expected = "found its page version reclaimed")]
    fn an_operation_that_reads_the_version_before_its_guard_is_found() {
        readers_race(|store| {
            let at = store.versions.latest();
            let guard = store.versions.guard();
            store.read(at);
            drop(guard);
        });
    }

    /// A reader that holds nothing while it reads.
    #[test]
    #[should_panic(expected = "found its page version reclaimed")]
    fn a_reader_that_holds_nothing_is_found() {
        readers_race(|store| {
            let at = store.versions.latest();
            store.read(at);
        });
    }

    #[test]
    #[should_panic(expected = "given out again under a reader")]
    fn a_reader_of_a_page_id_that_holds_nothing_is_found() {
        ids_race(|store| {
            let at = store.versions.latest();
            thread::yield_now();
            store.read(at);
        });
    }

    #[test]
    #[should_panic(expected = "given out again under a reader")]
    fn a_reader_of_a_page_id_that_reads_the_version_before_its_guard_is_found() {
        ids_race(|store| {
            let at = store.versions.latest();
            let guard = store.versions.guard();
            store.read(at);
            drop(guard);
        });
    }

    /// The write-back of before D-116: the link is pushed without a second
    /// look.
    #[test]
    #[should_panic(expected = "the id of a freed page is in use")]
    fn a_write_back_that_does_not_look_again_is_found() {
        a_page_is_freed_while_write_back_holds_it(false, true);
    }

    /// The freer of before D-116: the free link is not pushed again.
    #[test]
    #[should_panic(expected = "the id of a freed page is in use")]
    fn a_freer_that_does_not_push_again_is_found() {
        a_page_is_freed_while_write_back_holds_it(true, false);
    }
}
