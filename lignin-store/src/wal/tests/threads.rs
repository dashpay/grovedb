//! The log under threads: appends of every kind, syncs, turns and reads at
//! once.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex,
};

use lignin_format::wal::LoggedValue;

use super::*;

#[test]
fn appends_syncs_turns_and_reads_at_once_leave_one_log_in_the_order_of_its_places() {
    let std = StdDb::new(small_files(20_000));
    let Db {
        vfs,
        dir,
        manifest,
        log,
    } = std.db;
    let manifest = Mutex::new(manifest);
    let written = Mutex::new(Vec::<Item>::new());
    let values = Mutex::new(Vec::<(PendingValue, Vec<u8>)>::new());
    let done = AtomicBool::new(false);
    let (taken, turns, read) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    const COMMITS: u64 = 400;
    const CHANGES: u64 = 250;

    std::thread::scope(|scope| {
        // One committer: it asks for its place, writes its group and
        // appends it, and writes the group again when another append or a
        // turn took the place.
        scope.spawn(|| {
            let mut rng = Rng(1);
            let mut commits = 0;
            while commits < COMMITS {
                let at = match log.next_group() {
                    Ok(at) => at,
                    Err(LogError::NoActiveFile) => continue,
                    Err(error) => panic!("{error}"),
                };
                let (bytes, in_group) = group_with_values(&at, &mut rng);
                match log.append_group(&at, &bytes) {
                    Ok(appended) => {
                        commits += 1;
                        assert_eq!(at.version(), version(commits));
                        let item = Item::appended(appended, bytes, Some(at.version()));
                        written.lock().unwrap().push(item);
                        values.lock().unwrap().extend(in_group);
                    }
                    Err(LogError::PlaceTaken { expected, .. }) => {
                        assert_eq!(expected, at.place());
                        taken.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(error) => panic!("{error}"),
                }
            }
            done.store(true, Ordering::Release);
        });
        // Page steps and marks, which ask for no place.
        for thread in 0..3 {
            let (log, written) = (&log, &written);
            scope.spawn(move || {
                let mut rng = Rng(10 + thread);
                let mut last = None;
                for step in 0..CHANGES {
                    let (appended, bytes) = if step % 7 == 0 {
                        let covers = CommitVersion::EMPTY;
                        (log.append_mark(covers).unwrap(), mark(covers))
                    } else {
                        let bytes = change(&mut rng);
                        (log.append_change(&bytes).unwrap(), bytes)
                    };
                    // A thread's own items are in the log in the order it
                    // appended them.
                    assert!(Some(appended.at) >= last);
                    last = Some(appended.end);
                    written
                        .lock()
                        .unwrap()
                        .push(Item::appended(appended, bytes, None));
                    std::thread::yield_now();
                }
            });
        }
        // Syncs, of everything and through one item.
        scope.spawn(|| {
            let mut before = log.durable();
            while !done.load(Ordering::Acquire) {
                let durable = match written.lock().unwrap().last().map(|item| item.end) {
                    Some(end) => log.sync_through(end).unwrap(),
                    None => log.sync().unwrap(),
                };
                assert!(durable.place >= before.place && durable.version >= before.version);
                before = durable;
                std::thread::yield_now();
            }
        });
        // Whoever keeps the files: a next file, a new manifest and a turn.
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                let turned = log.roll(&mut manifest.lock().unwrap()).unwrap();
                turns.fetch_add(u64::from(turned), Ordering::Relaxed);
                std::thread::yield_now();
            }
        });
        // A reader of pending values, which waits for no append.
        scope.spawn(|| {
            let mut buf = Vec::new();
            let mut next = 0;
            while !done.load(Ordering::Acquire) {
                let Some((pending, value)) = values.lock().unwrap().get(next).cloned() else {
                    std::thread::yield_now();
                    continue;
                };
                let found = log.read_pending(&pending, page(9), &mut buf).unwrap();
                let LoggedValue::Bytes(bytes) = found else {
                    panic!("the bytes of a value");
                };
                assert_eq!(bytes.to_vec(), value);
                read.fetch_add(1, Ordering::Relaxed);
                next += 1;
            }
        });
    });

    let mut written = written.into_inner().unwrap();
    written.sort_by_key(|item| item.at);
    let (items, end) = read_log(&vfs, &dir).unwrap();
    assert_eq!(items.len() as u64, COMMITS + 3 * CHANGES);
    assert_eq!(items, written, "one log, in the order of its places");
    for pair in items.windows(2) {
        let same_file = pair[0].at.file() == pair[1].at.file();
        assert!(
            !same_file || pair[0].end == pair[1].at,
            "nothing between two items"
        );
    }
    assert_eq!(end.latest(), version(COMMITS));
    assert_eq!(end.place(), log.end());
    let (taken, turns, read) = (taken.into_inner(), turns.into_inner(), read.into_inner());
    println!("{taken} places taken, {turns} turns, {read} values read");
    assert!(turns >= 1, "{turns} turns");
    assert!(read > 0);
    drop(manifest);
}
