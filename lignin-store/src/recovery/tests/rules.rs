//! The rules of replay, each on a log that is written for it
//! (`page-table.md` K3, K11, K13; `log-format.md` LG27 to LG29;
//! `transactions.md` DU2, DU3, DU5, FL7, ID4).

use lignin_format::manifest::FileState;

use super::*;
use crate::{segment::FileNames, table::LinkKind};

#[test]
fn a_new_store_recovers_to_no_commit_and_takes_the_first() {
    let dir = Dir::new();
    let store = dir.open();
    assert_eq!(store.landed(), CommitVersion::EMPTY);
    assert_eq!(store.report, RecoveryReport::default());
    assert!(store.checkpoint.is_none());
    assert_eq!(store.table.pages(), 0);
    // The session has its first log file, and write-back keeps the whole
    // log while no table checkpoint is named.
    assert_eq!(
        store.log.next_group().expect("a place").version(),
        version(1)
    );
    assert_eq!(store.flush.in_force(), Some((CommitVersion::EMPTY, None)));
    drop(store);

    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[insert((1, 1), b"a", inline(b"one"))]);
    let model = session.kill();
    let store = dir.open();
    assert_eq!(store.landed(), version(1));
    assert_eq!(
        (store.report.commits, store.report.pages_rebuilt),
        (1, 1),
        "{:?}",
        store.report
    );
    check(&store, &model, &[2]);
}

/// With no table checkpoint the whole log is replayed, from empty counts:
/// pages that are made, changed, freed and made again, values that are
/// placed, and a value that is still pending when the session ends.
#[test]
fn the_whole_log_is_replayed_on_an_empty_table() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", inline(b"one")),
        insert((2, 1), b"b", long(1, 900)),
        insert((3, 1), b"c", long(2, 700)),
    ]);
    session.place((2, 1));
    session.commit(&[hash((1, 1), 7), Op::Free { page: 3 }]);
    session.commit(&[insert((3, 4), b"d", long(3, 600)), remove((2, 1))]);
    session.commit(&[insert((2, 2), b"e", long(4, 500))]);
    session.place((3, 4));
    let model = session.kill();

    let store = dir.open();
    assert_eq!(store.landed(), version(4));
    assert_eq!(store.report.covers, CommitVersion::EMPTY);
    assert_eq!(store.report.commits, 4);
    assert_eq!((store.report.changes, store.report.steps), (2, 2));
    assert_eq!(store.report.pages_freed, 1);
    assert_eq!(store.report.pending_values, 1);
    check(&store, &model, &[4]);
    // The value that was placed and then released with its cell is not
    // counted, and the one that page 3 holds is.
    let counts = store.values.counts();
    assert_eq!(counts.iter().map(|count| count.values).sum::<u64>(), 1);

    // Write-back owes every page, and knows how much log no checkpoint
    // covers and where each replayed commit begins.
    assert_eq!(store.pool.census().unwritten, 3);
    assert_eq!(store.pool.oldest_unwritten(), Some(version(2)));
    assert_eq!(store.flush.in_force(), Some((CommitVersion::EMPTY, None)));
}

/// **K3 and K2.** A checkpoint holds the chains as they were, links above
/// the version it covers among them. Replay applies to a page only the
/// changes above its newest link, and applies every change to the counts
/// by the place of the sample alone (K13, DU5).
#[test]
fn replay_skips_what_a_page_already_has_and_counts_by_the_place() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", inline(b"one")),
        insert((2, 1), b"b", long(1, 900)),
    ]);
    session.write_back(1, 1);
    // The sample is taken here, and the mark written. What follows is
    // after its place.
    let sample = session.sample(1);
    // Page 2 is written back after the step that places its value, and
    // the checkpoint copies its chain then: the page has the change, and
    // the sample does not count it.
    let (_, placed) = session.place((2, 1));
    assert!(Some(placed.at) >= sample.place);
    session.commit(&[hash((1, 1), 2), hash((2, 1), 3)]);
    session.write_back(2, 2);
    let place = session.checkpoint_with(1, &sample, None);
    session.commit(&[hash((2, 1), 4)]);
    let model = session.kill();

    let store = dir.open();
    assert_eq!(
        (store.report.covers, store.landed()),
        (version(1), version(3))
    );
    assert_eq!(
        store
            .checkpoint
            .as_ref()
            .and_then(|info| info.sample().place),
        Some(place)
    );
    // Commits 2 and 3 are replayed. Page 2 has what commit 2 did, and the
    // step: neither is made again. The step is counted.
    assert_eq!(store.report.commits, 2);
    assert_eq!((store.report.changes, store.report.steps), (1, 0));
    check(&store, &model, &[]);
    assert_eq!(store.values.counts()[0].values, 1);
    // Page 1 holds commit 2's change from the log, page 2 commit 3's.
    let born = |id| frame_of(&store, page(id)).expect("the page").born();
    assert_eq!((born(1), born(2)), (version(2), version(3)));
    // Write-back is told that the log is kept for the checkpoint, and from
    // which commit on page 1 is in no block.
    assert_eq!(store.flush.in_force(), Some((version(1), Some(place))));
}

/// **K11.** A checkpoint that lacks a page which was freed and trimmed
/// away leaves the page's earlier changes in the log with no image to
/// apply them to. They are dropped, and the commit that frees the page, or
/// one that makes it again, settles it.
#[test]
fn a_change_with_no_image_is_dropped_when_a_later_commit_frees_the_page_or_makes_it_again() {
    for made_again in [false, true] {
        let dir = Dir::new();
        let mut session = Session::begin(&dir, Model::default());
        session.commit(&[
            insert((1, 1), b"a", inline(b"one")),
            insert((2, 1), b"b", inline(b"two")),
            insert((3, 1), b"c", inline(b"three")),
        ]);
        session.write_back(1, 1);
        // Pages 2 and 3 were written back, freed by commit 3 and trimmed
        // away before this checkpoint was written: it has no link of
        // them, and covers commit 1.
        session.commit(&[hash((2, 1), 2), hash((3, 1), 2)]);
        session.commit(&[Op::Free { page: 2 }, Op::Free { page: 3 }]);
        if made_again {
            session.commit(&[insert((3, 2), b"d", inline(b"again"))]);
        }
        session.checkpoint(1);
        let model = session.kill();

        let store = dir.open();
        assert_eq!(store.report.covers, version(1));
        assert_eq!(store.report.changes_dropped, 2, "{:?}", store.report);
        match made_again {
            false => check(&store, &model, &[2, 3]),
            true => check(&store, &model, &[2]),
        }
        assert_eq!(store.table.newest(page(2)), None);
    }
}

/// **A background change is looked at from the place of the sample on,
/// and none before it** (LG28, K13). The steps before the place are in the
/// chains that the checkpoint copied. A step after it is made if the cell
/// still holds what the record says it held, and counted either way.
#[test]
fn a_page_step_is_replayed_from_the_place_of_the_sample_on() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", long(1, 600)),
        insert((2, 1), b"b", long(2, 700)),
    ]);
    let (first, _) = session.place((1, 1));
    session.place((2, 1));
    session.write_back(1, 1);
    session.write_back(2, 1);
    let place = session.checkpoint(1);
    // After the sample: the value of page 1 is moved to another place,
    // and a commit gives page 2 a second value, which is placed.
    let bytes = session.model.pending[&first.from().pending().expect("pending").id()].clone();
    let to = session.append_value(1, &bytes);
    let moved =
        ValueMove::in_tree(page(1), slot(1), ValuePlace::Placed(first.to()), to).expect("a move");
    session.step(&moved);
    session.commit(&[insert((2, 2), b"c", long(3, 800))]);
    session.place((2, 2));
    let model = session.kill();

    let store = dir.open();
    assert_eq!(store.landed(), version(2));
    assert_eq!(store.report.commits, 1);
    // Two records are after the place, and both are made: one to a page
    // as its chain gives it, one to a page that replay had changed.
    assert_eq!((store.report.changes, store.report.steps), (2, 2));
    assert_eq!(store.report.pending_values, 0);
    check(&store, &model, &[]);
    assert_eq!(
        store
            .checkpoint
            .as_ref()
            .and_then(|info| info.sample().place),
        Some(place)
    );
    // Page 1 is the page version of commit 1 still, in another frame,
    // and write-back owes it, as it owes page 2 of commit 2.
    assert_eq!(store.pool.census().unwritten, 2);
    let born = |id| frame_of(&store, page(id)).expect("the page").born();
    assert_eq!((born(1), born(2)), (version(1), version(2)));
    // Three values are live: the one that was moved counts once.
    let live: u64 = store.values.counts().iter().map(|count| count.values).sum();
    assert_eq!(live, 3);
}

/// **LG27 and LG28 for a reclaiming step.** What a dropped subtree left
/// in a page is removed in a page step with no version. Replay makes the
/// step when the cells are there, skips it for a page that has it, and
/// does not take a cell that a later commit put into a slot the step had
/// freed.
#[test]
fn a_reclaiming_step_is_made_once_and_never_takes_a_later_cell() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", inline(b"keep")),
        insert((1, 2), b"b", long(1, 600)),
        insert((1, 3), b"c", inline(b"left over")),
        insert((2, 1), b"d", inline(b"left over")),
        insert((2, 2), b"e", inline(b"left over")),
        insert((3, 1), b"f", long(2, 700)),
    ]);
    session.place((1, 2));
    session.place((3, 1));
    for id in 1..=3 {
        session.write_back(id, 1);
    }
    let place = session.checkpoint(1);

    // After the sample. Page 1: two cells leave, one of them with a value
    // in the value log. Page 2: its cells leave, and then a commit puts a
    // cell into a slot that the step freed. Page 3: the page is freed
    // with no list of its cells, at the version of the commit before.
    let first = session.reclaim(1, &[2, 3], None);
    assert!(first.at >= place);
    session.reclaim(2, &[1, 2], None);
    session.commit(&[insert((2, 1), b"g", inline(b"new"))]);
    session.reclaim(3, &[], Some(2));
    let model = session.kill();

    let store = dir.open();
    assert_eq!(store.landed(), version(2));
    assert_eq!((store.report.changes, store.report.steps), (3, 3));
    assert_eq!(store.report.pages_freed, 1);
    check(&store, &model, &[3]);
    // Both values were released: the one of the cell that left, and the
    // one of the page that was freed.
    assert!(store.values.counts().iter().all(|count| count.values == 0));
    assert_eq!(
        store
            .table
            .newest(page(3))
            .map(|link| (link.version, link.kind)),
        Some((version(2), LinkKind::Free))
    );
    drop(store);

    // A checkpoint of what that recovery rebuilt has the pages with the
    // steps, and its sample is the one from before them (K5, K13): the
    // records are after the place, and are looked at again. It covers
    // commit 1 and reaches commit 2 (K2). None of the steps is made a
    // second time. The cell that commit 2 put into slot 1 of page 2
    // stays, although the step's record says that slot 1 leaves: the
    // page's newest link is of a commit after the record.
    let mut session = Session::begin(&dir, model);
    session.write_back(1, 1);
    session.write_back(2, 2);
    let counts = store_counts_at(&dir, place);
    let sample = session.sample_at(place, &counts);
    session.checkpoint_with(1, &sample, None);
    let model = session.kill();
    let store = dir.open();
    assert_eq!(
        (store.report.covers, store.landed()),
        (version(1), version(2))
    );
    assert_eq!((store.report.changes, store.report.steps), (3, 0));
    assert_eq!(store.report.pages_rebuilt, 0);
    check(&store, &model, &[3]);
}

/// A reclaiming step that frees a page the checkpoint has lost needs no
/// image, and settles the changes to that page that were dropped (K11).
#[test]
fn a_reclaiming_step_frees_a_page_that_the_checkpoint_has_no_image_of() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", inline(b"one")),
        insert((2, 1), b"b", inline(b"left over")),
    ]);
    session.write_back(1, 1);
    session.checkpoint(1);
    session.commit(&[hash((1, 1), 2), hash((2, 1), 2)]);
    session.reclaim(2, &[], Some(2));
    let model = session.kill();

    let store = dir.open();
    assert_eq!(store.report.changes_dropped, 1);
    check(&store, &model, &[2]);
    assert_eq!(store.allocator.state(page(2)), PageState::Retired);
}

/// **A value that is pending where recovery lands has its bytes with the
/// frame of its page** (FL7): a reader is answered from the frame, and
/// write-back places the value from there.
#[test]
fn a_pending_value_of_a_rebuilt_page_is_with_its_frame() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", long(1, 3_000)),
        insert((1, 2), b"b", long(2, 5_000)),
    ]);
    session.commit(&[hash((1, 1), 9)]);
    let model = session.kill();

    let store = dir.open();
    assert_eq!(store.report.pending_values, 2);
    check(&store, &model, &[]);
    let frame = frame_of(&store, page(1)).expect("the page");
    let lens: Vec<usize> = frame.attachments().map(|(_, bytes)| bytes.len()).collect();
    assert_eq!(lens.iter().sum::<usize>(), 8_000);
    assert_eq!(store.pool.attached_bytes(), 8_000);
}

/// **Replay keeps a bounded number of rebuilt pages.** With room for two,
/// the others are written to a segment while the log is read, each as a
/// base image under a link of the commit that changed it last, and a page
/// that is changed again afterwards is read back from there. The store
/// that comes out says what one that kept every page says.
#[test]
fn rebuilt_pages_beyond_what_replay_keeps_are_written_to_a_segment() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    let pages = 1..=12u32;
    for id in pages.clone() {
        session.commit(&[
            insert((id, 1), b"a", inline(&[id as u8; 40])),
            insert((id, 2), b"b", long(id as u8, 400)),
        ]);
    }
    // Every other page has its value placed. The others keep a pending
    // value, and cannot be written.
    for id in pages.clone().filter(|id| id % 2 == 0) {
        session.place((id, 2));
    }
    for id in pages.clone() {
        session.commit(&[hash((id, 1), 0x40 + id as u8)]);
    }
    let model = session.kill();
    let before = dir.files();

    let kept_all = dir.open();
    assert_eq!(kept_all.report.pages_spilled, 0);
    assert_eq!(kept_all.report.pages_rebuilt, 12);
    check(&kept_all, &model, &[]);
    drop(kept_all);

    let few = RecoveryOptions {
        max_replay_pages: Some(2),
        ..options(dir.size)
    };
    let store = dir.recover_with(&few).expect("the store recovers");
    assert_eq!(store.landed(), version(24));
    check(&store, &model, &[]);
    let report = store.report;
    // The six pages with a pending value are never written. The six
    // others are, once their value is placed, and a page that a later
    // commit changes again is read back from its block.
    assert!(report.pages_spilled >= 6, "{report:?}");
    assert!(report.pages_rebuilt >= 6, "{report:?}");
    assert_eq!(report.pending_values, 6);
    for id in pages.clone().filter(|id| id % 2 == 0) {
        let newest = store.table.newest(page(id)).expect("a link");
        assert!(matches!(newest.kind, LinkKind::Base(_)), "page {id}");
        assert!(newest.version <= version(24));
    }
    // What the pool cannot evict is what write-back owes.
    assert_eq!(store.pool.census().unwritten as u64, report.pages_rebuilt);

    // No file of the earlier session was appended to or cut: what
    // recovery wrote is in files of its own.
    let after = dir.files();
    for (path, bytes) in &before {
        let name = path.to_string_lossy();
        if name == "MANIFEST" {
            continue;
        }
        let Some(now) = after.get(path) else {
            // The file that the earlier session would have turned its
            // log to next holds nothing of the log, and went.
            assert!(
                name.starts_with("wal/") && bytes.len() == 20,
                "{name} is gone"
            );
            continue;
        };
        assert!(bytes.starts_with(now), "{name} holds other bytes");
        assert!(name.starts_with("wal/") || now == bytes, "{name} was cut");
    }
    assert!(after.keys().any(|path| path.starts_with("seg")));
    drop(store);

    // The store that the second recovery leaves is recovered again, and
    // is the same.
    let again = dir.open();
    assert_eq!(again.landed(), version(24));
    check(&again, &model, &[]);
}

/// **Afterwards the session starts files of its own** (M15, M16, DU3):
/// the manifest names every file of the earlier session closed, at the
/// length that is part of the store, and the log goes on in a new file. A
/// group that a crash cut short is never read again, and its version is
/// given to the next commit.
#[test]
fn the_files_of_the_earlier_session_are_closed_and_its_torn_commit_gives_its_version_back() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[insert((1, 1), b"a", inline(b"one"))]);
    session.commit(&[insert((1, 2), b"b", long(1, 800))]);
    session.place((1, 2));
    let kept = session.model.clone();
    let third = session.commit(&[hash((1, 1), 3)]);
    session.kill();
    // The crash keeps all of the third group but its last byte.
    let log_file = FileNames::WAL.path(&dir.path, third.at.file());
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&log_file)
        .expect("the log file");
    file.set_len(third.end.offset() - 1).expect("cut");
    drop(file);
    let before = dir.files();

    let store = dir.open();
    assert_eq!(store.landed(), version(2));
    check(&store, &kept, &[]);
    let named = store.manifest.manifest().expect("a manifest").clone();
    let files = named.files();
    let (last, earlier) = files
        .iter()
        .filter(|file| file.kind() == lignin_format::FileKind::Wal)
        .collect::<Vec<_>>()
        .split_last()
        .map(|(last, earlier)| (**last, earlier.to_vec()))
        .expect("log files");
    // The session's log ends in a file of its own, and the file it turns
    // to next is made: those two are open, and every file of the earlier
    // session is closed, the log where its last whole item ends.
    let (active, earlier) = earlier.split_last().expect("the session's file");
    assert!(active.id() > third.at.file() && last.id() > active.id());
    assert_eq!(
        (active.state(), last.state()),
        (FileState::Open, FileState::Open)
    );
    for file in files
        .iter()
        .filter(|file| file.kind() != lignin_format::FileKind::Wal || file.id() < active.id())
    {
        assert_ne!(file.state(), FileState::Open, "{file:?}");
    }
    assert_eq!(
        earlier.last().map(|file| file.valid_len()),
        Some(third.at.offset())
    );
    // The value-log file of the earlier session is as it was.
    let after = dir.files();
    for (name, bytes) in before.iter().filter(|(name, _)| name.starts_with("val")) {
        assert_eq!(after.get(name), Some(bytes), "{name:?}");
    }
    drop(store);

    // The next commit is commit 3, and the group that was cut is not.
    let mut session = Session::begin(&dir, kept);
    let appended = session.commit(&[hash((1, 1), 0x33)]);
    assert!(appended.at.file() > active.id());
    let model = session.kill();
    let store = dir.open();
    assert_eq!(store.landed(), version(3));
    check(&store, &model, &[]);
}

/// **T-ID1.** The page ids of a recovered store are those of the version
/// it landed on: an id is in use exactly when its page is, whether the
/// table checkpoint or the replayed log says so, and the id of a page
/// that a lost commit made is free.
#[test]
fn the_page_ids_are_those_of_the_version_recovery_lands_on() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", inline(b"one")),
        insert((2, 1), b"b", inline(b"two")),
        insert((3, 1), b"c", inline(b"three")),
    ]);
    for id in 1..=3 {
        session.write_back(id, 1);
    }
    session.commit(&[Op::Free { page: 2 }]);
    session
        .store
        .table
        .push(page(2), Link::free(version(2)))
        .expect("freed");
    session.checkpoint(2);
    session.commit(&[Op::Free { page: 3 }, insert((5, 1), b"e", inline(b"five"))]);
    session.commit(&[insert((2, 1), b"f", inline(b"two again"))]);
    let kept = session.model.clone();
    let lost = session.commit(&[insert((7, 1), b"g", inline(b"lost")), Op::Free { page: 1 }]);
    session.kill();
    let log_file = FileNames::WAL.path(&dir.path, lost.at.file());
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&log_file)
        .expect("the log file");
    file.set_len(lost.at.offset() + 9).expect("cut");
    drop(file);

    let store = dir.open();
    assert_eq!(
        (store.report.covers, store.landed()),
        (version(2), version(4))
    );
    check(&store, &kept, &[3, 4, 6, 7]);
    let state = |id| store.allocator.state(page(id));
    assert_eq!(
        [1, 2, 3, 4, 5, 6, 7].map(state),
        [
            PageState::Live,
            PageState::Live,
            PageState::Retired,
            PageState::Free,
            PageState::Live,
            PageState::Free,
            PageState::Free,
        ]
    );
}

/// The counts of live values that a session of the store of `dir` had
/// when its log had reached `place`: what a sample taken there holds. In
/// these tests no value is counted before the first checkpoint mark, and
/// the counts of the sample the manifest names stand for an earlier
/// place, so this is that sample's counts.
fn store_counts_at(dir: &Dir, place: LogPlace) -> Vec<FileCount> {
    let manifest = dir.manifest();
    let named = manifest.manifest().expect("a manifest");
    let (_, info) = PageTable::load(&*dir.vfs, &dir.path, named).expect("the checkpoint loads");
    let sample = info.expect("a checkpoint").sample().clone();
    assert_eq!(sample.place, Some(place));
    sample
        .counts
        .iter()
        .map(|count| FileCount {
            file: count.file,
            values: count.values,
            bytes: count.bytes,
            miscounted: count.miscounted,
        })
        .collect()
}
