//! Snapshots over a page store on disk (`transactions.md` T-RD1, T-PN2,
//! T-RD5): what a snapshot reads never changes, however long it is held,
//! while a writer supersedes, frees and reuses pages, write-back moves
//! page versions into segments, the pool evicts them, and each reclaim
//! takes back page ids, held frames and page-table links.

mod store;

use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
};

use lignin_format::{CellAddress, SlotId};
use lignin_store::{
    alloc::Pins,
    load::{LoadBuffers, LoadError},
};
use lignindb::snapshot::{LatestLease, Operation, SnapshotError, Versions};
use store::{page, read_page, version, Changes, Pages, Rng, Store};

/// Reads every page of `pages` through `operation` and checks that each is
/// the page the commit named there wrote.
fn check_pages(operation: &Operation<'_>, pages: &Pages, buffers: &mut LoadBuffers) {
    for (&id, &born) in pages {
        let frame = operation
            .page(page(id), buffers)
            .unwrap_or_else(|error| panic!("page {id} at {}: {error}", operation.at()));
        assert_eq!(read_page(&frame), (id, born), "at {}", operation.at());
        assert_eq!(frame.born(), version(born));
    }
}

/// The changes of one commit, drawn from the pages of the latest version:
/// it writes a few of them again, and every third commit frees one and
/// creates one.
fn draw(rng: &mut Rng, store: &Store, versions: &Versions, round: u64) -> Changes {
    let live: Vec<u32> = store.pages_at(versions.latest()).keys().copied().collect();
    let mut changes = Changes::default();
    let free = (round.is_multiple_of(3) && live.len() > 4)
        .then(|| live[rng.below(live.len() as u64) as usize]);
    for _ in 0..1 + rng.below(3) {
        let id = live[rng.below(live.len() as u64) as usize];
        if Some(id) != free && !changes.write.contains(&page(id)) {
            changes.write.push(page(id));
        }
    }
    if let Some(id) = free {
        changes.free.push(page(id));
        changes.create = 1;
    }
    changes
}

#[test]
fn a_snapshot_reads_its_version_while_pages_are_superseded_freed_and_reused() {
    const COMMITS: u64 = if cfg!(miri) { 30 } else { 1_500 };

    let store = Store::new(6);
    let versions = store.versions();
    let first = store.commit(&versions, &Changes::create(10));
    store.write_back();
    assert_eq!(first, version(1));

    let done = AtomicBool::new(false);
    let longest = AtomicU64::new(0);
    let reads = AtomicU64::new(0);
    thread::scope(|scope| {
        for reader in 0..4u64 {
            let (store, versions, done) = (&store, &versions, &done);
            let (longest, reads) = (&longest, &reads);
            scope.spawn(move || {
                let mut rng = Rng::new(reader);
                let mut buffers = LoadBuffers::new();
                while !done.load(Ordering::Relaxed) {
                    let snapshot = versions.snapshot(store.loader());
                    let pages = store.pages_at(snapshot.version());
                    let rounds = if rng.below(20) == 0 {
                        60
                    } else {
                        1 + rng.below(4)
                    };
                    for _ in 0..rounds {
                        check_pages(&snapshot.operation(), &pages, &mut buffers);
                        reads.fetch_add(1, Ordering::Relaxed);
                        thread::yield_now();
                    }
                    let behind = versions.latest().get() - snapshot.version().get();
                    longest.fetch_max(behind, Ordering::Relaxed);
                }
            });
        }
        for _ in 0..2 {
            let (store, versions, done, reads) = (&store, &versions, &done, &reads);
            scope.spawn(move || {
                let mut lease = LatestLease::new();
                let mut buffers = LoadBuffers::new();
                while !done.load(Ordering::Relaxed) {
                    let operation = versions.operation(store.loader(), &mut lease);
                    let pages = store.pages_at(operation.at());
                    check_pages(&operation, &pages, &mut buffers);
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            });
        }

        // With no snapshot held for long, ids come back and are used
        // again, and the chains are trimmed.
        let mut rng = Rng::new(99);
        let mut buffers = LoadBuffers::new();
        for round in 1..COMMITS / 2 {
            store.commit(&versions, &draw(&mut rng, &store, &versions, round));
            store.write_back();
        }

        // A snapshot that is held to the end, taken when a few ids are
        // free: the commits after it use them for new pages.
        let live: Vec<u32> = store.pages_at(versions.latest()).keys().copied().collect();
        let free = live[..3].iter().map(|id| page(*id)).collect();
        store.commit(
            &versions,
            &Changes {
                free,
                ..Changes::default()
            },
        );
        let held = versions.snapshot(store.loader());
        let held_pages = store.pages_at(held.version());
        let reused_before = store.counts.ids_reused.load(Ordering::Relaxed);
        for round in 1..COMMITS / 2 {
            let mut changes = draw(&mut rng, &store, &versions, round);
            changes.create += usize::from(round <= 3);
            store.commit(&versions, &changes);
            store.write_back();
            if round.is_multiple_of(64) {
                check_pages(&held.operation(), &held_pages, &mut buffers);
            }
        }
        while reads.load(Ordering::Relaxed) < COMMITS {
            thread::yield_now();
        }
        done.store(true, Ordering::Relaxed);

        // Every page the snapshot saw has been written again or freed
        // since, and ids that were free at its version name new pages.
        let now = store.pages_at(versions.latest());
        assert!(held_pages
            .iter()
            .all(|(id, born)| now.get(id) != Some(born)));
        assert!(store.counts.ids_reused.load(Ordering::Relaxed) > reused_before);
        assert_eq!(versions.oldest_pin(), held.version());
        check_pages(&held.operation(), &held_pages, &mut buffers);
        assert!(store.ids.census().retired > 0);
        drop(held);
    });

    // With every reader gone, one reclaim takes everything back.
    versions.reclaim();
    versions.reclaim();
    assert_eq!((versions.pins_held(), versions.guards_held()), (0, 0));
    assert_eq!(versions.oldest_pin(), versions.latest());
    assert_eq!(store.ids.census().retired, 0);
    assert_eq!(store.pool.census().held, 0);

    let counts = &store.counts;
    let stats = store.pool.stats();
    println!(
        "{counts:?}\n{stats:?}\nthe longest snapshot was {} commits behind, {} reads",
        longest.load(Ordering::Relaxed),
        reads.load(Ordering::Relaxed)
    );
    assert!(counts.ids_reused.load(Ordering::Relaxed) > 10);
    assert!(counts.links_trimmed.load(Ordering::Relaxed) > 10);
    assert!(stats.evictions > 0 && stats.misses > 0);
    if !cfg!(miri) {
        assert!(longest.load(Ordering::Relaxed) > 3);
    }
}

#[test]
fn a_snapshot_never_sees_a_later_commit_and_is_read_only() {
    let store = Store::new(8);
    let versions = store.versions();
    store.commit(&versions, &Changes::create(2));
    let snapshot = versions.snapshot(store.loader());
    assert_eq!(snapshot.version(), version(1));
    assert_eq!(snapshot.pin().version(), version(1));
    assert_eq!(format!("{:?}", snapshot.pin()), "VersionPin(v1)");
    assert_eq!(format!("{snapshot:?}"), "Snapshot(v1)");

    // Commit 2 writes page 1 again, frees page 2 and creates page 3.
    store.commit(
        &versions,
        &Changes {
            write: vec![page(1)],
            create: 1,
            free: vec![page(2)],
        },
    );
    store.write_back();
    let mut buffers = LoadBuffers::new();
    let operation = snapshot.operation();
    assert_eq!(operation.at(), version(1));
    assert_eq!(
        format!("{operation:?}"),
        format!(
            "Operation {{ at: v1, guard: OperationGuard({:?}), .. }}",
            operation.guard().epoch()
        )
    );
    assert_eq!(operation.guard().epoch(), versions.epoch());
    for id in [1, 2] {
        let frame = operation.page(page(id), &mut buffers).expect("a page");
        assert_eq!(read_page(&frame), (id, 1));
    }
    // Page 3 is no page at version 1.
    assert!(matches!(
        operation.page(page(3), &mut buffers),
        Err(LoadError::NoPage { .. })
    ));
    drop(operation);

    // A reader with no pin reads the latest version, under its guard.
    let mut lease = LatestLease::new();
    let latest = versions.operation(store.loader(), &mut lease);
    assert_eq!((latest.at(), latest.lease()), (version(2), lease.lease()));
    assert_eq!(latest.guard().latest(), version(2));
    assert_eq!(
        read_page(&latest.page(page(1), &mut buffers).expect("a page")),
        (1, 2)
    );
    assert_eq!(
        read_page(&latest.page(page(3), &mut buffers).expect("a page")),
        (3, 2)
    );
    assert!(latest.page(page(2), &mut buffers).is_err());
    drop(latest);

    // Writes and commits through it are refused, and a rollback does
    // nothing (RD1).
    assert_eq!(snapshot.commit(), Err(SnapshotError::ReadOnly));
    snapshot.rollback();
    assert_eq!(snapshot.version(), version(1));

    // The id of the freed page waits for the snapshot, and for an
    // operation that was running when the snapshot went.
    versions.reclaim();
    assert_eq!(store.ids.census().retired, 1);
    let running = versions.guard();
    drop(snapshot);
    assert_eq!(store.ids.census().retired, 1);
    drop(running);
    versions.reclaim();
    assert_eq!(store.ids.census().retired, 0);
    assert_eq!(store.ids.allocate(), Ok(page(2)));
}

#[test]
fn a_snapshot_reads_nodes_under_one_lease_and_its_guard() {
    let store = Store::new(8);
    let versions = store.versions();
    store.commit(&versions, &Changes::create(2));
    let snapshot = versions.snapshot(store.loader());
    let lease = snapshot.lease();
    assert_eq!(snapshot.lease(), lease);
    assert_ne!(versions.snapshot(store.loader()).lease(), lease);

    let mut buffers = LoadBuffers::new();
    let operation = snapshot.operation();
    assert_eq!(operation.lease(), lease);
    let nodes = operation.nodes();
    assert_eq!((nodes.at(), nodes.lease()), (version(1), lease));
    assert!(std::ptr::eq(nodes.guard(), operation.guard()));
    let node = nodes
        .node(CellAddress::new(page(2), SlotId::MIN), &mut buffers)
        .expect("the node");
    assert_eq!(node.node().key(), b"page");
    let handle = node.handle();
    assert_eq!(handle.lease(), lease);
    drop(node);

    // A handle that was kept is followed by a later operation of the same
    // snapshot, and not under another lease.
    let later = snapshot.operation();
    let found = later
        .nodes()
        .resolve(handle, &mut buffers)
        .expect("it reads");
    assert_eq!(found.expect("the handle is good").node().key(), b"page");
    let mut unpinned = LatestLease::default();
    let other = versions.operation(store.loader(), &mut unpinned);
    assert!(other
        .nodes()
        .resolve(handle, &mut buffers)
        .expect("it reads")
        .is_none());

    // A reader with no pin keeps its lease, and a handle it read, for as
    // long as the latest version stays. A commit ends both.
    let kept = other
        .nodes()
        .node(CellAddress::new(page(1), SlotId::MIN), &mut buffers)
        .expect("the node")
        .handle();
    drop(other);
    let before = unpinned.lease();
    let same = versions.operation(store.loader(), &mut unpinned);
    assert_eq!((same.lease(), unpinned.lease()), (before, before));
    let found = same.nodes().resolve(kept, &mut buffers).expect("it reads");
    assert!(found.is_some());
    drop(found);
    drop(same);
    store.commit(&versions, &Changes::create(1));
    let moved = versions.operation(store.loader(), &mut unpinned);
    assert_ne!(moved.lease(), before);
    assert_eq!(moved.lease(), unpinned.lease());
    let found = moved.nodes().resolve(kept, &mut buffers).expect("it reads");
    assert!(found.is_none());
    drop(found);
    drop(moved);
    let before = unpinned.lease();
    unpinned.renew();
    assert_ne!(unpinned.lease(), before);

    // A snapshot that is first asked through an operation takes its lease
    // then, and keeps it.
    let fresh = versions.snapshot(store.loader());
    let taken = fresh.operation().lease();
    assert_eq!(fresh.lease(), taken);
}
