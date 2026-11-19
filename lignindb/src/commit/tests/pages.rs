//! What a commit does to pages, page ids and value pointers
//! (`transactions.md` CM3 steps 6 and 7), and the rules it keeps with the
//! log and the page table.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use lignin_store::{alloc::PageState, flush::Checkpointed, table::LinkKind};

use super::{
    aux, commit_reported, counted, groups, pending_held, pointers_held, records, writes_of,
};
use crate::{
    commit::{CommitError, Failure, NoStaleHashes},
    txn::{
        tests::{
            delete, model, put, scan,
            store::{Db, Outcome},
            World,
        },
        Column,
    },
};

/// D-080 item 3. A page is made under an id that named another page
/// before, and is freed again before write-back gave it a link. The page
/// table still has the free link of the earlier page as its newest and
/// takes no second one (`AlreadyFree`).
///
/// The rule: **the commit goes on as for any page it frees.** No segment
/// ever held the page, so the table has nothing to say of it that the
/// earlier free link does not say. The buffer pool keeps its versions for
/// the readers that can still ask for them, and the id goes back to the
/// allocator once, at the commit's version. What the free link would have
/// given a table checkpoint, the log durable through the freeing commit
/// before the checkpoint is named, is given by the place of the
/// checkpoint's copy (`page-table.md` K14 item 2): the group is in the log
/// before the commit frees anything.
#[test]
fn a_page_freed_again_before_it_was_written_back_is_given_no_second_free_link() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let unit = aux(4);
    let fill = |round: u8| -> Vec<model::Write> {
        (0..400u16)
            .map(|index| put(unit, &index.to_be_bytes(), &[round; 60]))
            .collect()
    };
    let everything = || vec![model::Write::Range(unit, Vec::new(), None)];

    // Pages are made, and freed: each gets its free link, the first link
    // of its chain.
    let tx = world.begin();
    world.write(&tx, fill(1));
    let made = commit_reported(&mut world, tx);
    assert!(made.created.len() > 2, "{made:?}");
    let tx = world.begin();
    world.write(&tx, everything());
    let first = commit_reported(&mut world, tx);
    assert!(
        first.freed.len() > 2 && first.freed_unlinked == 0,
        "{first:?}"
    );
    for page in &first.freed {
        let link = db.table.newest(*page).expect("the free link");
        assert_eq!((link.kind, link.version), (LinkKind::Free, first.version));
        assert_eq!(db.ids.state(*page), PageState::Free, "nothing is pinned");
    }

    // The ids name new pages, which a reader pins.
    let tx = world.begin();
    world.write(&tx, fill(2));
    let again = commit_reported(&mut world, tx);
    let reused: Vec<_> = (again.created.iter())
        .filter(|page| first.freed.contains(page))
        .copied()
        .collect();
    assert!(reused.len() > 2, "{again:?}");
    let reader = world.begin();
    let mut cursor = reader.txn.cursor(unit);
    cursor.seek_to_first().expect("a seek");

    // They are freed again, and write-back has not run in between.
    let tx = world.begin();
    world.write(&tx, everything());
    let second = commit_reported(&mut world, tx);
    assert!(reused.iter().all(|page| second.freed.contains(page)));
    assert_eq!(second.freed_unlinked, reused.len(), "{second:?}");
    for page in &reused {
        // The table says what it said: the page is free, from the earlier
        // link on. The id waits for the reader, once.
        let link = db.table.newest(*page).expect("the earlier free link");
        assert_eq!((link.kind, link.version), (LinkKind::Free, first.version));
        assert_eq!(db.ids.state(*page), PageState::Retired);
    }
    // The reader of the version between reads every record of it, from
    // the page versions that the pool keeps.
    let read = scan(&mut cursor, false);
    assert_eq!(read.len(), 400);
    assert!(read.iter().all(|(_, value)| value == &[2; 60]));
    drop(cursor);
    drop(reader);
    versions.reclaim();
    versions.reclaim();
    for page in &reused {
        assert_eq!(db.ids.state(*page), PageState::Free, "the id came back");
    }
    world.check_committed();

    // A table checkpoint that is taken now is named only with the log
    // durable through the commit that freed the pages.
    let write_back = db.write_back(&versions);
    let Checkpointed::Installed { covers, .. } = write_back.checkpoint().expect("a checkpoint")
    else {
        panic!("nothing stands in the way of a checkpoint");
    };
    assert_eq!(covers, versions.latest());
    assert!(db.log.durable().version >= second.version);

    // And the ids go round once more: made, freed with no link, reused.
    for round in 3..5 {
        let tx = world.begin();
        world.write(&tx, fill(round));
        commit_reported(&mut world, tx);
        let tx = world.begin();
        world.write(&tx, everything());
        let freed = commit_reported(&mut world, tx);
        assert!(!freed.freed.is_empty());
    }
    world.check_committed();
    assert_eq!(db.private_ids(), 0);
    assert!(db.flush_errors.lock().expect("the errors").is_empty());
    let counts = &db.counts;
    assert!(counts.freed_unwritten.load(Ordering::Relaxed) >= reused.len() as u64);
    assert!(counts.ids_reused.load(Ordering::Relaxed) >= reused.len() as u64);
}

/// PV6, CM3 step 7: a commit counts the value pointers its cells stopped
/// holding, lists them in its group, and adds to no count. The value it
/// writes is counted when write-back places it.
#[test]
fn a_commit_releases_the_pointers_its_cells_stop_holding() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let write_back = db.write_back(&versions);
    let (unit, merk) = (aux(4), 5);
    let long = |byte: u8, len: usize| vec![byte; len];
    let live = |db: &Db| counted(db).values().sum::<u64>();

    // A flat record and a Merk node, each with a value out of line. The
    // commit carries the values in its group and counts nothing.
    let tx = world.begin();
    world.write(&tx, vec![put(unit, b"flat", &long(1, 3000))]);
    world.edit(&tx, merk, |items| {
        items.insert(b"node".to_vec(), long(2, 400));
        items.insert(b"other".to_vec(), long(3, 400));
    });
    let first = commit_reported(&mut world, tx);
    assert_eq!((first.released, live(&db)), (0, 0));
    assert_eq!(first.value_pages, 1, "the tree page of the two nodes");
    assert!(first.value_bytes > 3800, "{first:?}");
    assert_eq!(pending_held(&db, &versions).len(), 3);

    // Write-back places them: now they are counted.
    let pass = write_back.pass().expect("a pass");
    assert_eq!(pass.waiting, 0, "{pass:?}");
    assert_eq!(db.flush.stats().values_placed, 3);
    assert_eq!(live(&db), 3);
    assert_eq!(counted(&db), pointers_held(&db, &versions));
    assert!(pending_held(&db, &versions).is_empty());

    // Each cell is given another value: the commit releases the pointer
    // it held, in its group and in the counts, as one with the pages.
    let tx = world.begin();
    world.write(&tx, vec![put(unit, b"flat", &long(4, 3000))]);
    world.edit(&tx, merk, |items| {
        items.insert(b"node".to_vec(), long(5, 400));
    });
    let second = commit_reported(&mut world, tx);
    assert_eq!(second.released, 2, "{second:?}");
    assert_eq!(groups(&db).last().map(|group| group.released), Some(2));
    assert_eq!(live(&db), 1, "the value of the node that was not written");
    assert_eq!(counted(&db), pointers_held(&db, &versions));
    world.check_committed();

    // A value that was still pending when its cell lost it had no pointer:
    // nothing is released for it.
    let tx = world.begin();
    world.write(&tx, vec![delete(unit, b"flat")]);
    world.edit(&tx, merk, |items| {
        items.remove(&b"node"[..]);
    });
    let third = commit_reported(&mut world, tx);
    assert_eq!((third.released, live(&db)), (0, 1));

    // And a delete of the last placed value releases it.
    write_back.pass().expect("a pass");
    let tx = world.begin();
    world.edit(&tx, merk, |items| {
        items.remove(&b"other"[..]);
    });
    let fourth = commit_reported(&mut world, tx);
    assert_eq!((fourth.released, live(&db)), (1, 0));
    assert!(pointers_held(&db, &versions).is_empty());
    world.check_committed();
    assert!(db.flush_errors.lock().expect("the errors").is_empty());
}

/// `log-format.md` LG22: a group is written for one place. Where
/// something is appended between the moment the commit asked for its
/// place and its append, which the lock on the counts keeps from
/// happening in a store that keeps its rules, the log refuses the group
/// and the commit writes it again for the new place: every value is named
/// anew, and its cell leads to its bytes in the log.
#[test]
fn a_group_that_lost_its_place_is_written_again_with_its_values_named_anew() {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let committer = db.committer(&versions);
    let stop = AtomicBool::new(false);
    let unit = aux(4);
    let mut lost = 0;
    std::thread::scope(|scope| {
        // Marks, appended without the lock on the counts.
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                (db.log.append_mark(super::version(1))).expect("a mark");
                std::thread::sleep(Duration::from_micros(40));
            }
        });
        for round in 0..400u16 {
            if committer.stats().rewritten >= 3 && round >= 40 {
                break;
            }
            let key = (round % 9).to_be_bytes();
            let writes = vec![put(unit, &key, &vec![round as u8; 3000])];
            loop {
                let tx = world.begin();
                world.write(&tx, writes.clone());
                world.edit(&tx, 5, |items| {
                    items.insert(key.to_vec(), vec![round as u8; 400]);
                });
                let wrote = writes_of(&tx);
                match db.commit_as(&committer, &tx.txn, None) {
                    Ok(published) => {
                        let published = published.expect("a version");
                        assert_eq!(world.model.commit(&wrote), published.version.get());
                        break;
                    }
                    // The place was taken more often than a commit tries:
                    // the commit failed, with nothing appended.
                    Err(CommitError::Failed(Failure::PlaceLost)) => lost += 1,
                    Err(other) => panic!("{other}"),
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
    });
    let stats = committer.stats();
    assert!(stats.rewritten > 0, "no group lost its place: {stats:?}");
    assert_eq!(stats.failed, lost);

    // Every value reads as written, from the frame of its page under the
    // name its cell holds, and that name is the address of its bytes in
    // the log.
    world.check_committed();
    let pending = pending_held(&db, &versions);
    assert!(pending.len() >= 9, "{} pending values", pending.len());
    let mut buf = Vec::new();
    for (page, value) in pending {
        let logged = db.log.read_pending(&value, page, &mut buf);
        let logged = logged.unwrap_or_else(|error| panic!("{page}: {error}"));
        assert_eq!(logged.owner(), page);
        assert_eq!(logged.value_len(), value.value_len());
        let bytes = logged
            .bytes()
            .expect("the bytes are repeated, not named")
            .to_vec();
        assert!(bytes.iter().all(|byte| *byte == bytes[0]));
    }
    // Versions are dense all the same: a group that lost its place took
    // no version.
    let logged = groups(&db);
    assert_eq!(logged.len() as u64, versions.latest().get());
    assert!(logged
        .iter()
        .enumerate()
        .all(|(index, group)| group.version == index as u64 + 1));
}

/// ST5, CV3, LG17: a delete of a key that has no record is a write, and
/// so is a put that the same transaction takes back. The commit of either
/// takes a version and changes no page: a copy that the writes took is as
/// its page was, and is discarded, not installed.
#[test]
fn a_commit_that_changes_no_page_takes_a_version_and_installs_nothing() {
    let (db, versions) = Db::new(64);
    let mut world = World::new(&db, &versions);
    let unit = model::unit(Column::Meta, 6);
    let unwritten = db.pool.census().unwritten;

    for (round, writes) in [
        vec![delete(unit, b"never there")],
        vec![
            put(unit, b"never there", b"for a while"),
            delete(unit, b"never there"),
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let before = versions.latest();
        let tx = world.begin();
        for write in writes {
            world.write(&tx, vec![write]);
        }
        let published = commit_reported(&mut world, tx);

        assert_eq!(Some(published.version), before.next());
        // Only the put took a copy, and the delete made it as it was.
        assert_eq!(published.unchanged_flat > 0, round == 1, "{published:?}");
        assert!(published.created.is_empty() && published.freed.is_empty());
        assert_eq!(published.changed, 0);
        assert_eq!(
            db.pool.census().unwritten,
            unwritten,
            "nothing was installed"
        );
        assert_eq!(db.pool.census().private, 0, "and nothing is left over");
        let group = groups(&db).pop().expect("the group");
        assert_eq!(group.version, published.version.get());
        assert!(group.flat_runs.is_empty() && group.tree_runs.is_empty());
        assert!(group.values.is_empty() && group.freed.is_empty());
    }

    // It is a commit like any other to a transaction that wrote the key.
    let met = world.begin();
    world.write(&met, vec![put(unit, b"never there", b"t")]);
    let outside = records(vec![delete(unit, b"never there")]);
    assert!(matches!(
        world.write_outside(&outside, &[&met]),
        Outcome::Committed(_)
    ));
    assert_eq!(world.commit(met, &[]), Outcome::Conflict);
    world.check_committed();
}

/// T-CF4: commits outside an open transaction write `total` bytes to keys
/// it has not written. The conflict rule is decided exactly all the same
/// (CF3): the transaction commits, and one whose key a last commit
/// outside writes fails. RocksDB answers `TryAgain` for both once its
/// memtable history is outgrown (case A11b).
fn commits_outside_do_not_change_the_answer(total: usize) -> usize {
    const VALUE: usize = 1 << 20;
    let (db, versions) = Db::new(256);
    let committer = db.committer(&versions);
    let write = |unit, key: &[u8], value: &[u8]| {
        let batch = records(vec![put(unit, key, value)]);
        batch.with(|batch| committer.write(batch, &mut NoStaleHashes))
    };
    let begin = |key: &[u8]| {
        let txn = committer.path().begin();
        let scope = txn.scope().expect("a scope");
        let batch = records(vec![put(aux(7), key, b"written first")]);
        batch.with(|batch| scope.apply(batch)).expect("a write");
        scope.merge();
        txn
    };
    let (unmet, met) = (begin(b"k1"), begin(b"k2"));

    let value = vec![0x5a; VALUE];
    let rounds = total.div_ceil(VALUE);
    for round in 0..rounds {
        // A few keys, written again and again: what is held is the log
        // and the value log, not the state.
        let key = [(round % 8) as u8];
        write(aux(9), &key, &value).expect("a commit outside");
    }
    let kept = db.commits.kept();
    write(aux(7), b"k2", b"written outside").expect("the commit that meets");

    let published = committer
        .commit(&unmet, &mut NoStaleHashes)
        .expect("nobody met it");
    assert_eq!(published.version(), Some(versions.latest()));
    let conflict = committer.commit(&met, &mut NoStaleHashes);
    assert!(
        matches!(conflict, Err(CommitError::Conflict)),
        "{conflict:?}"
    );
    assert!(committer.stats().group_bytes as usize >= total);
    assert_eq!(committer.stats().conflicts, 1);
    assert!(db.flush_errors.lock().expect("the errors").is_empty());
    kept
}

#[test]
fn the_conflict_rule_is_exact_after_many_commits_outside() {
    // The key sets of the commits that the open transactions have not
    // looked at are what the rule is decided from: their keys, not their
    // values.
    let kept = commits_outside_do_not_change_the_answer(24 << 20);
    assert!(kept >= 24, "{kept} key sets");
}

/// `cargo test --release -p lignindb --lib -- --ignored four_hundred`
#[test]
#[ignore = "writes 400 MiB to the log and the value log"]
fn the_conflict_rule_is_exact_after_four_hundred_megabytes_outside() {
    let kept = commits_outside_do_not_change_the_answer(400 << 20);
    println!("{kept} key sets were kept for the two open transactions");
}
