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

/// **A page that a reclaiming step freed before the sample was taken is
/// free, also where the checkpoint has lost it.** The table trims the
/// chain of a freed page away once no reader needs it (K10), so a
/// checkpoint that is written afterwards has no link of the page, and
/// nothing in it says that there was one. It can cover a version below
/// the one the page was freed with: then the log still holds what commits
/// did to the page, and the record of the step, which is before the place
/// of the sample, is all that says the page is gone.
///
/// Here page 2 has a change with no image, and page 3 is made, both by
/// commits the checkpoint does not cover. A step frees each of them, a
/// later commit makes a page under the id of page 2 again, and only then
/// is the sample taken.
#[test]
fn a_page_that_a_step_freed_before_the_sample_is_free_where_the_checkpoint_lost_it() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", inline(b"one")),
        insert((2, 1), b"b", inline(b"left over")),
    ]);
    session.write_back(1, 1);
    session.commit(&[
        hash((1, 1), 2),
        hash((2, 1), 2),
        insert((3, 1), b"c", inline(b"made and dropped")),
    ]);
    session.commit(&[hash((1, 1), 3)]);
    session.reclaim(2, &[], Some(3));
    session.reclaim(3, &[], Some(3));
    session.commit(&[insert((2, 1), b"n", inline(b"under the id again"))]);
    // The table has no link of page 2 or of page 3, as after a trim, and
    // page 1 is in a block as commit 1 left it.
    session.checkpoint(1);
    let model = session.kill();

    for kept in [None, Some(1)] {
        let options = RecoveryOptions {
            max_replay_pages: kept,
            ..options()
        };
        let store = dir.recover_with(&options).expect("the store recovers");
        assert_eq!(
            (store.report.covers, store.landed()),
            (version(1), version(4))
        );
        // Both records are before the place of the sample. Page 3 was
        // there to be freed, and of page 2 there was a dropped change.
        assert_eq!((store.report.changes, store.report.steps), (2, 1));
        assert_eq!(store.report.changes_dropped, 1);
        assert_eq!(store.report.pages_freed, 2);
        // With one page kept, page 3 is in a segment when it is freed.
        assert_eq!(store.report.pages_spilled > 0, kept.is_some());
        check(&store, &model, &[3]);
        assert_eq!(store.allocator.state(page(3)), PageState::Retired);
        assert_eq!(store.allocator.state(page(2)), PageState::Live);
        assert_eq!(
            frame_of(&store, page(2)).expect("the page").born(),
            version(4)
        );
    }
}

/// **A cell that a reclaiming step took out before the sample was taken
/// is out of a page that replay made.** A commit that the checkpoint does
/// not cover makes page 3. A step takes a cell out of it, and a later
/// commit puts another cell into the slot. Then a commit frees the page,
/// its chain is trimmed away, and the sample is taken: every record of
/// the page is before the place, and the checkpoint has no link of it.
/// Replay makes the page from the log, and the cell that the later commit
/// puts in fits only where the step's has left.
///
/// Page 2 is the same with a chain in the checkpoint that lacks the
/// commit before the step. Write-back takes no such checkpoint: it writes
/// the page first (K1). Replay holds the page with that commit's change,
/// so it makes the step all the same, and the page says what the session
/// left.
#[test]
fn a_cell_that_a_step_took_out_before_the_sample_is_out_of_a_page_replay_made() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", inline(b"one")),
        insert((2, 1), b"b", inline(b"stays")),
        insert((2, 2), b"c", inline(b"left over")),
    ]);
    session.write_back(1, 1);
    session.write_back(2, 1);
    session.commit(&[
        hash((2, 1), 2),
        insert((3, 1), b"d", inline(b"stays")),
        insert((3, 2), b"e", inline(b"left over")),
    ]);
    session.reclaim(2, &[2], None);
    session.reclaim(3, &[2], None);
    session.commit(&[
        insert((2, 2), b"f", inline(b"in its place")),
        insert((3, 2), b"g", inline(b"in its place")),
    ]);
    session.commit(&[hash((1, 1), 4), Op::Free { page: 3 }]);
    session.checkpoint(1);
    let model = session.kill();

    for kept in [None, Some(1)] {
        let options = RecoveryOptions {
            max_replay_pages: kept,
            ..options()
        };
        let store = dir.recover_with(&options).expect("the store recovers");
        assert_eq!(
            (store.report.covers, store.landed()),
            (version(1), version(4))
        );
        assert_eq!((store.report.changes, store.report.steps), (2, 2));
        assert_eq!(store.report.changes_dropped, 0);
        assert_eq!(store.report.pages_spilled > 0, kept.is_some());
        check(&store, &model, &[3]);
        let frame = frame_of(&store, page(2)).expect("the page");
        let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
        let keys: Vec<&[u8]> = tree.cells().map(|(_, cell)| cell.key()).collect();
        assert_eq!(keys, [b"b".as_slice(), b"f".as_slice()]);
    }
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
        ..options()
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

/// A reclaiming step that takes cells out of a page and frees it in the
/// same step: the page goes with its cells, and the values the cells held
/// are released.
#[test]
fn a_reclaiming_step_that_removes_and_frees_takes_the_page() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    session.commit(&[
        insert((1, 1), b"a", inline(b"keep")),
        insert((2, 1), b"b", long(1, 600)),
        insert((2, 2), b"c", inline(b"left over")),
    ]);
    session.place((2, 1));
    // A page is freed with a version after the one it was born with.
    session.commit(&[insert((1, 2), b"d", inline(b"later"))]);
    session.write_back(1, 2);
    session.write_back(2, 1);
    session.checkpoint(2);
    session.reclaim(2, &[1, 2], Some(2));
    let model = session.kill();

    let store = dir.open();
    assert_eq!((store.report.changes, store.report.steps), (1, 1));
    assert_eq!(store.report.pages_freed, 1);
    check(&store, &model, &[2]);
    assert!(store.values.counts().iter().all(|count| count.values == 0));
    assert_eq!(store.allocator.state(page(2)), PageState::Retired);
}

/// The entries of the flat page in `frame`: the keys of a leaf, or the
/// separators of a branch.
fn flat_keys(frame: &[u8]) -> Vec<Vec<u8>> {
    use lignin_format::flat_page::FlatPage;
    let flat = FlatPage::from_validated(frame, FRAME_FORMAT).expect("a flat page");
    match (flat.leaf(), flat.branch()) {
        (Some(leaf), _) => leaf.iter().map(|(key, _)| key.to_vec()).collect(),
        (_, Some(branch)) => (0..branch.len())
            .filter_map(|at| branch.separator(at))
            .map(|key| key.to_vec())
            .collect(),
        _ => unreachable!("a flat page is a leaf or a branch"),
    }
}

/// **Flat pages.** A commit logs a flat page as an image or as changes by
/// key, and a reclaiming step takes entries out of a leaf, one or a range,
/// and separators out of a branch. Each step is made once: a record that
/// is looked at again finds its first entry gone.
#[test]
fn flat_pages_are_replayed_by_key_and_reclaimed_once() {
    use lignin_format::{
        flat_page::{FlatBranchBuilder, FlatLeafBuilder, FlatValue},
        wal::{FlatChange, FlatRun, FlatRunWriter},
    };

    let dir = Dir::new();
    let session = Session::begin(&dir, Model::default());
    let capacity = dir.size.image_capacity();
    let log = &session.store.log;
    let commit = |write: &dyn Fn(&mut GroupWriter<'_>)| {
        let at = log.next_group().expect("a place");
        let mut bytes = Vec::new();
        let mut group = GroupWriter::new(at.version(), at.address(), &mut bytes).expect("a group");
        write(&mut group);
        group.finish();
        log.append_group(&at, &bytes).expect("appended");
    };
    let reclaim = |id: u32, removes: &[FlatChange<'_>]| {
        let mut run = Vec::new();
        let mut records = FlatRunWriter::new(page(id), &mut run);
        for change in removes {
            records.push(change).expect("a change");
        }
        records.finish().expect("a run");
        let removed = Removed::Flat(FlatRun::decode(&run, V1).expect("a run"));
        let mut record = Vec::new();
        write_reclaim(page(id), Some(removed), None, [], &mut record).expect("a record");
        log.append_change(&record).expect("appended");
    };

    // Commit 1: a leaf with five entries and a branch with three
    // separators, each as an image.
    let mut leaf = FlatLeafBuilder::new(page(5), b"", capacity).expect("a leaf");
    for key in [b"a", b"b", b"c", b"d", b"e"] {
        leaf.push(key, FlatValue::Inline(b"value"))
            .expect("an entry");
    }
    let leaf = leaf.finish();
    let mut branch = FlatBranchBuilder::new(page(6), b"", capacity, 1, page(50)).expect("a branch");
    for (key, child) in [(b"k", 51), (b"m", 52), (b"p", 53)] {
        branch.push(key, page(child)).expect("a separator");
    }
    let branch = branch.finish();
    commit(&|group| {
        group
            .flat_run_with(page(5), |run| run.push(&FlatChange::Image(&leaf)))
            .expect("a run");
        group
            .flat_run_with(page(6), |run| run.push(&FlatChange::Image(&branch)))
            .expect("a run");
    });
    // Commit 2: changes of the leaf by key.
    commit(&|group| {
        group
            .flat_run_with(page(5), |run| {
                run.push(&FlatChange::Insert {
                    key: b"f",
                    value: FlatValue::Inline(b"new"),
                })?;
                run.push(&FlatChange::Replace {
                    key: b"a",
                    value: FlatValue::Inline(b"other"),
                })
            })
            .expect("a run");
    });
    // Reclaiming steps: one entry leaves the leaf, then a range of two,
    // and a separator leaves the branch. Each record stands twice.
    let one = [FlatChange::Remove { key: b"b" }];
    let range = [FlatChange::RemoveRange {
        key: b"c",
        count: 2,
    }];
    let of_branch = [FlatChange::Remove { key: b"m" }];
    for _ in 0..2 {
        reclaim(5, &one);
        reclaim(5, &range);
        reclaim(6, &of_branch);
    }
    drop(session);

    let store = dir.open();
    assert_eq!(store.landed(), version(2));
    assert_eq!((store.report.changes, store.report.steps), (6, 3));
    let keys = |id| flat_keys(&frame_of(&store, page(id)).expect("the page"));
    assert_eq!(keys(5), [b"a".to_vec(), b"e".to_vec(), b"f".to_vec()]);
    assert_eq!(keys(6), [b"k".to_vec(), b"p".to_vec()]);
    let born = |id| frame_of(&store, page(id)).expect("the page").born();
    assert_eq!((born(5), born(6)), (version(2), version(1)));
}

/// A page that only page steps changed is written to a segment like any
/// other when replay keeps too many pages: its block is a base image
/// under a link of the version the page had, behind the link that was
/// there, and a later step finds the page through it.
#[test]
fn a_page_that_only_steps_changed_is_written_under_the_version_it_had() {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    let pages = 1..=4u32;
    for id in pages.clone() {
        session.commit(&[insert((id, 1), b"a", long(id as u8, 500))]);
        session.place((id, 1));
    }
    for id in pages.clone() {
        session.write_back(id, u64::from(id));
    }
    session.checkpoint(4);
    // After the sample every value is moved, and the value of page 1 a
    // second time.
    let mut moved_twice = None;
    for id in pages.clone().chain([1]) {
        let frame = &session.model.pages[&page(id)];
        let tree = TreePage::from_validated(frame, FRAME_FORMAT).expect("a tree page");
        let from = tree
            .cell(slot(1))
            .and_then(|cell| cell.value_pointer())
            .expect("placed");
        let to = session.append_value(id, &vec![id as u8; 500]);
        let step =
            ValueMove::in_tree(page(id), slot(1), ValuePlace::Placed(from), to).expect("a move");
        moved_twice = Some(session.step(&step));
    }
    let model = session.kill();

    let one = RecoveryOptions {
        max_replay_pages: Some(1),
        ..options()
    };
    let store = dir.recover_with(&one).expect("the store recovers");
    assert_eq!(
        (store.report.covers, store.landed()),
        (version(4), version(4))
    );
    assert_eq!((store.report.commits, store.report.steps), (0, 5));
    assert!(store.report.pages_spilled >= 4, "{:?}", store.report);
    // Every page reads with its last pointer, whether its frame is in the
    // pool or its block is read. The values are other bytes than the
    // model's, so the pages are compared and not the values.
    for id in pages.clone() {
        let frame = frame_of(&store, page(id)).expect("the page");
        let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
        assert_eq!(Some(tree.to_image()), model.image(page(id)), "page {id}");
        assert_eq!(frame.born(), version(u64::from(id)));
    }
    // Page 1 was written with its first move, and read back for its
    // second: two base images of version 1 follow the one it had.
    let mut chain = Vec::new();
    store.table.chain(page(1), &mut chain);
    let kinds: Vec<bool> = chain
        .iter()
        .map(|link| link.version == version(1) && matches!(link.kind, LinkKind::Base(_)))
        .collect();
    assert!(
        kinds.len() == 3 && kinds.iter().all(|&base| base),
        "{chain:?}"
    );
    assert!(moved_twice.is_some());
    let live: u64 = store.values.counts().iter().map(|count| count.values).sum();
    assert_eq!(live, 4);
}

/// One act of a session's history.
enum Act {
    Commit(Vec<Op>),
    /// Write-back places the pending value of a cell.
    Place(u32, u16),
    /// Write-back writes a page, born at the version, to a segment.
    WriteBack(u32, u64),
    /// A reclaiming step takes the cells of the slots out of a page, or
    /// frees the page with the version.
    Reclaim(u32, Vec<u16>, Option<u64>),
    /// A table checkpoint that covers the version begins: the sample of
    /// the counts is taken and its mark is in the log.
    Sample(u64),
    /// The checkpoint that began is written and named.
    Install,
}

/// Runs `acts` on a new store and kills the session. Returns the
/// directory, what the pages say then, and the last commit.
fn left_by(acts: &[Act]) -> (Dir, Model, CommitVersion) {
    let dir = Dir::new();
    let mut session = Session::begin(&dir, Model::default());
    let mut begun = None;
    for act in acts {
        match act {
            Act::Commit(ops) => drop(session.commit(ops)),
            Act::Place(id, slot) => drop(session.place((*id, *slot))),
            Act::WriteBack(id, born) => session.write_back(*id, *born),
            Act::Reclaim(id, slots, freed) => drop(session.reclaim(*id, slots, *freed)),
            Act::Sample(covers) => begun = Some((*covers, session.sample(*covers))),
            Act::Install => {
                let (covers, sample) = begun.take().expect("a checkpoint that began");
                session.checkpoint_with(covers, &sample, None);
            }
        }
    }
    let latest = session.latest();
    let model = session.kill();
    (dir, model, latest)
}

/// **Wherever the session is killed, the pages that a checkpoint has lost
/// are what the session left.** One history, killed after each of its
/// acts, and each store recovered twice, the second time with one page
/// kept. Every page, page id and count is held to a model that the acts
/// made, and that knows nothing of checkpoints.
///
/// The history has every way in which a page step meets a page that the
/// checkpoint in force has no chain of. The table of these tests never
/// has a link of a page that was not written back, which is how a table
/// is after the chain of a freed page was trimmed away (K10):
///
/// - Page 2 is older than the checkpoint, changed by a commit after it,
///   and freed by a step. A commit makes a page under its id again.
/// - Page 3 is made by a commit that the checkpoint does not cover. A
///   step takes a cell out, a commit puts another into the slot, and
///   steps take a cell out and free the page after the sample was taken.
/// - Page 4 is older than the checkpoint, with a value in the value log.
///   Everything that steps do to it comes between the sample and the
///   copy of the chains: its cell leaves, with the pointer, and it is
///   freed.
///
/// Before the first checkpoint is named the whole log is replayed on an
/// empty table, and every record counts. Afterwards the same records are
/// before and after the place of a sample. Both give what the model says.
/// The second checkpoint covers everything, and what follows it is the
/// plain case.
#[test]
fn a_history_with_pages_the_checkpoint_lost_recovers_the_same_wherever_it_is_killed() {
    let history = [
        Act::Commit(vec![
            insert((1, 1), b"a", inline(b"one")),
            insert((2, 1), b"b", inline(b"two")),
            insert((2, 2), b"c", inline(b"left over")),
            insert((4, 1), b"g", inline(b"four")),
            insert((4, 2), b"h", long(4, 700)),
        ]),
        Act::Place(4, 2),
        Act::WriteBack(1, 1),
        Act::Commit(vec![
            hash((1, 1), 2),
            hash((2, 1), 2),
            hash((4, 1), 2),
            insert((3, 1), b"d", inline(b"three")),
            insert((3, 2), b"e", long(3, 600)),
        ]),
        Act::Place(3, 2),
        Act::Reclaim(3, vec![2], None),
        Act::Commit(vec![
            insert((3, 2), b"f", inline(b"in its place")),
            hash((1, 1), 3),
        ]),
        Act::Reclaim(2, vec![2], None),
        Act::Reclaim(2, vec![], Some(3)),
        Act::Commit(vec![
            insert((2, 1), b"n", inline(b"under the id again")),
            hash((1, 1), 4),
        ]),
        // The first checkpoint, which covers commit 1 and has the chain of
        // page 1 alone. Between its sample and its copy, steps go on.
        Act::Sample(1),
        Act::Reclaim(3, vec![1], None),
        Act::Reclaim(4, vec![2], None),
        Act::Commit(vec![hash((1, 1), 5)]),
        Act::Reclaim(3, vec![], Some(5)),
        Act::Reclaim(4, vec![], Some(5)),
        Act::Install,
        Act::Commit(vec![
            hash((2, 1), 6),
            insert((3, 1), b"p", inline(b"under the id again")),
        ]),
        // The second checkpoint, which covers every commit so far.
        Act::WriteBack(1, 5),
        Act::WriteBack(2, 6),
        Act::WriteBack(3, 6),
        Act::Sample(6),
        Act::Install,
        Act::Commit(vec![hash((3, 1), 7)]),
    ];

    let (mut from_a_checkpoint, mut looked_at_before_the_place) = (0, 0);
    for acts in 0..=history.len() {
        let (dir, model, latest) = left_by(&history[..acts]);
        let gone: Vec<u32> = (1..=4)
            .filter(|&id| !model.pages.contains_key(&page(id)))
            .collect();
        for kept in [None, Some(1)] {
            let options = RecoveryOptions {
                max_replay_pages: kept,
                ..options()
            };
            let store = dir
                .recover_with(&options)
                .unwrap_or_else(|error| panic!("after {acts} acts, {kept:?} kept: {error}"));
            assert_eq!(store.landed(), latest, "after {acts} acts");
            check(&store, &model, &gone);
            let report = store.report;
            from_a_checkpoint += u64::from(store.checkpoint.is_some());
            // With the first checkpoint in force, the records of the steps
            // before its sample are looked at for the pages it lost.
            if report.covers == version(1) {
                assert_eq!(report.changes_dropped, 3, "after {acts} acts: {report:?}");
                looked_at_before_the_place += report.changes - 4;
            }
        }
    }
    // Eight of the stores have a checkpoint, six of them the first, and
    // each recovery from the first looks at three records before its
    // place: the value of page 3 that was placed, the cell that left the
    // page, and the step that freed page 2.
    assert_eq!((from_a_checkpoint, looked_at_before_the_place), (16, 36));
}
