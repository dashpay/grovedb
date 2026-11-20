//! Stores that pass, what a report says of them, and what a run is told.

use std::sync::atomic::AtomicU64;

use lignin_format::{tree_page::Side, CommitVersion, PageId};
use lignin_store::table::{Link, LinkKind};
use lignin_tree::{
    drop::{Column as FlatColumn, FlatDrops, FlatReclaimed, Progress},
    flat::FlatTree,
    hash::{HashRules, Refusal},
    node::NodeRef,
};

use super::{drop_subtree, key, plant, prefix, write_back, Planted, BIG, FLAT, INNER, OUTER};
use crate::{
    txn::{
        tests::{
            model, put,
            store::{Db, Rules, ENTERED, SIZE},
            World,
        },
        Column,
    },
    verify::{
        self, still::chain_fault, Finding, Grove, Options, Place, Report, Rule, Scope, Store,
        Unreached,
    },
};

#[test]
fn a_store_with_one_of_everything_is_sound() {
    let store = plant();
    let report = store.check();
    println!("{report}");
    assert!(report.is_sound(), "{report}");
    let census = &report.census;
    // The Merks every store has, the two that were entered and stay, and
    // what the dropped one left.
    assert_eq!(census.merks, u64::from(ENTERED) + 2);
    assert_eq!((census.dropped_merks, census.left_over_entries), (1, 1));
    assert_eq!(census.live_cells, 400 + 3 + 2);
    assert_eq!(census.left_cells, 41 + 2);
    assert_eq!(census.cells, census.live_cells + census.left_cells);
    assert_eq!(census.hashed_cells, census.live_cells);
    assert_eq!(census.hosted_roots, 1);
    assert!(report.unreached.is_empty(), "{report}");
    // 80 values of the big Merk, one of the hosting one, one flat record,
    // and those of the dropped Merks, which stay counted until they are
    // reclaimed.
    assert_eq!(census.placed_values, 80 + 1 + 1 + 10 + 1);
    assert_eq!(census.pending_values, 0);
    // The records that were written, and the one under a long key.
    assert!(census.flat_records > 300 + 2 + 2, "{report}");
    assert!(census.tree_pages > 10 && census.flat_pages > 10, "{report}");
    let ids = census.ids.expect("the store has an allocator");
    assert_eq!(ids.live, census.tree_pages + census.flat_pages);
    assert_eq!(ids.private, 0);
    assert!(census.links >= ids.live, "{report}");
    assert!(!census.segments.is_empty() && !census.values.is_empty());
    assert_eq!(
        census.values.iter().map(|used| used.values).sum::<u64>(),
        93
    );
}

#[test]
fn a_store_is_sound_before_write_back_places_its_values() {
    let (db, versions) = Db::new(256);
    let mut world = World::new(&db, &versions);
    let tx = world.begin();
    world.edit(&tx, BIG, |items| {
        for index in 0..30 {
            items.insert(key(index), vec![index as u8; 400]);
        }
    });
    world.write(
        &tx,
        vec![put(model::unit(Column::Aux, FLAT), b"long", &[1; 3000])],
    );
    world.commit(tx, &[]);
    let report = db.verify_still();
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.pending_values, 31);
    assert_eq!(report.census.placed_values, 0);

    write_back(&db, &versions);
    let report = db.verify_still();
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.pending_values, 0);
    assert_eq!(report.census.placed_values, 31);
}

/// A reader's check of a version that it pinned, beside commits that came
/// after: it sees the version, and asks nothing of what the store keeps
/// beside its pages.
#[test]
fn a_pinned_version_is_checked_beside_later_commits() {
    let store = plant();
    let pin = store.versions.pin();
    let calls = AtomicU64::new(0);
    let before = (store.db).verify(pin.version(), Scope::Reader, Some(&mut Rules(&calls)));
    assert!(before.is_sound(), "{before}");
    assert_eq!(before.census.ids, None);
    assert_eq!(before.census.links, 0);

    drop_subtree(&store, OUTER);
    assert!(store.latest() > pin.version());
    let pinned = (store.db).verify(pin.version(), Scope::Reader, Some(&mut Rules(&calls)));
    assert!(pinned.is_sound(), "{pinned}");
    assert_eq!(pinned.census.merks, before.census.merks);
    assert_eq!(pinned.census.live_cells, before.census.live_cells);
    let latest = store.check();
    assert!(latest.is_sound(), "{latest}");
    assert_eq!(latest.census.merks, before.census.merks - 2);
    assert_eq!(latest.census.dropped_merks, 2);
    assert_eq!(latest.census.live_cells, before.census.live_cells - 5);
}

/// A store is sound before a reclaiming, after every page step of it, and
/// after it. From the step that takes the record of the dropped Merk to the
/// one that takes its last cell, the cells that are left have nothing that
/// leads to them: they are counted apart, and they are no finding. A crash
/// in that time leaves them for good (`lignin_tree::drop`).
#[test]
fn a_store_is_sound_before_during_and_after_a_reclaiming() {
    let store = plant();
    // A commit after the drop: a page that the latest commit wrote is not
    // freed before the next one.
    commit_later(&store);
    let before = store.check();
    assert!(before.is_sound(), "{before}");
    assert!(before.unreached.is_empty(), "{before}");
    let census = &before.census;
    assert_eq!((census.dropped_merks, census.left_over_entries), (1, 1));
    assert_eq!(census.left_cells, 43);

    let (mut merks, flat_drops, mut wait) = store.db.left(&store.versions);
    assert!(
        wait.poll(&store.versions),
        "no reader is from before the drop"
    );
    assert_eq!((merks.len(), flat_drops.len()), (1, 0));
    let (mut steps, mut with_unreached, mut cells) = (0, 0, before.census.cells);
    loop {
        let progress = store.db.reclaim(&store.versions, &mut merks[0], 1);
        steps += 1;
        let during = store.check();
        assert!(during.is_sound(), "after {steps} steps:\n{during}");
        let (census, unreached) = (&during.census, &during.unreached);
        assert!(
            census.cells <= cells,
            "a step takes cells out and puts none in"
        );
        cells = census.cells;
        assert_eq!(census.live_cells, before.census.live_cells);
        // The cells that are left are walked from the record of the
        // dropped Merk while it is there, and are reached by nothing once
        // it is gone.
        match census.dropped_merks {
            1 => assert_eq!((census.left_cells, unreached.cells), (43, 0), "{during}"),
            _ => {
                assert_eq!(census.left_cells, 0, "{during}");
                assert_eq!(census.cells, census.live_cells + unreached.cells);
                assert_eq!(census.left_over_entries, 0);
                with_unreached += u64::from(unreached.cells > 0);
            }
        }
        match progress {
            Progress::Done => break,
            Progress::More => {}
            Progress::Blocked(busy) => panic!("the step is refused: {busy:?}"),
        }
        assert!(steps < 200, "the reclaiming does not end");
    }
    assert!(steps > 4, "{steps} steps");
    assert!(
        with_unreached > 0,
        "no step left cells that nothing reaches"
    );

    let after = store.check();
    assert!(after.is_sound(), "{after}");
    assert!(after.unreached.is_empty(), "{after}");
    let census = &after.census;
    assert_eq!((census.dropped_merks, census.left_over_entries), (0, 0));
    assert_eq!(
        (census.left_cells, census.cells),
        (0, before.census.live_cells)
    );
    // The values of the cells that went are released from the counts.
    assert_eq!(census.placed_values, before.census.placed_values - 11);
    assert!(census.tree_pages < before.census.tree_pages, "{after}");
    // The pages that write-back owes after the steps are written, and the
    // store is sound in its segments as well.
    write_back(&store.db, &store.versions);
    let written = store.check();
    assert!(written.is_sound(), "{written}");
}

/// What a crash while the cells of a dropped Merk go leaves: the report
/// says how many cells nothing reaches, how much room they take and where
/// the first of them are, and calls the store sound.
#[test]
fn cells_that_nothing_reaches_are_reported_apart() {
    let store = plant();
    commit_later(&store);
    let (mut merks, _, mut wait) = store.db.left(&store.versions);
    assert!(wait.poll(&store.versions));
    // The reclaiming is taken on until the record of the dropped Merk is
    // gone and the first cells with it, and is then given up, as a process
    // that ends gives it up.
    let report = loop {
        let progress = store.db.reclaim(&store.versions, &mut merks[0], 1);
        assert_eq!(progress, Progress::More);
        let report = store.check();
        if report.unreached.cells > 0 {
            break report;
        }
    };
    drop(merks);
    assert!(report.is_sound(), "{report}");
    let unreached = &report.unreached;
    assert!(unreached.pages >= 1 && unreached.bytes > 20 * unreached.cells);
    assert!(unreached.values > 0 && unreached.values <= 11, "{report}");
    let sample = (unreached.cells as usize).min(Unreached::SAMPLE);
    assert_eq!(unreached.first.len(), sample);
    assert_eq!(report.census.left_cells, 0);
    // Their values stay counted, so the counts of live values still agree
    // with the pointers the pages hold.
    let text = report.to_string();
    assert!(text.starts_with("sound: every rule holds"), "{text}");
    let said = format!("space that nothing reaches: {} cells", unreached.cells);
    assert!(text.contains(&said), "{text}");
    // Nothing leads a second reclaiming to them: the store has no record.
    let (merks, flat, _) = store.db.left(&store.versions);
    assert!(merks.is_empty() && flat.is_empty());
    assert_eq!(store.check().unreached.cells, unreached.cells);
}

/// A store with no directory is walked from the roots it is given, through
/// every pointer to a hosted root.
#[test]
fn a_store_with_no_directory_is_checked_from_its_roots() {
    let planted = plant();
    let db = &planted.db;
    let roots = [planted.root(BIG), planted.root(OUTER)];
    let check = |roots: &[_]| {
        let store = Store {
            ids: None,
            grove: Grove::Roots(roots),
            flat_drops: None,
            values_counted: false,
            ..parts(db, &[])
        };
        let calls = AtomicU64::new(0);
        let options = Options::new(planted.latest(), Scope::Still);
        verify::check(&store, &Guard, options, Some(&mut Rules(&calls)))
    };
    let report = check(&roots);
    // With no directory, the keyspaces are not known: their pages are
    // flat pages that nothing reaches, and nothing else is found.
    assert_eq!(report.rules(), [Rule::FlatPageUnreached], "{report}");
    assert_eq!(report.census.live_cells, 400 + 3 + 2);
    assert_eq!(report.census.hosted_roots, 1);
    assert_eq!(report.census.ids, None);
    // What the dropped Merk left has nothing that leads to it here.
    assert_eq!(report.unreached.cells, 43);
    // A root that is left out leaves its Merk unreached, and no finding.
    let fewer = check(&roots[..1]);
    assert_eq!(fewer.rules(), [Rule::FlatPageUnreached]);
    assert_eq!(fewer.unreached.cells, 43 + 5);
}

struct Guard;

impl lignin_tree::OperationGuard for Guard {}

/// The parts of the store `db` as its own check names them, with the
/// columns `columns`.
fn parts<'a>(db: &'a Db, columns: &'a [FlatColumn<'a>]) -> Store<'a, 'a> {
    Store {
        loader: db.loader(),
        table: &db.table,
        segments: &db.segments,
        values: &db.values,
        page_size: SIZE,
        ids: Some(&db.ids),
        grove: Grove::Directory {
            tree: db.directory(),
            columns,
        },
        flat_drops: Some(db.flat_drops()),
        flat: &[],
        values_counted: true,
    }
}

/// The pages that a lazy range delete left are flat pages that no
/// keyspace reaches: the store names the keyspace of their records, and
/// they are walked from there, before and after the reclaimer frees them.
#[test]
fn the_pages_a_range_delete_left_are_walked_from_its_record() {
    let store = plant();
    let drops = store.db.flat_drops();
    let check = |drops: Option<FlatDrops>| {
        let calls = AtomicU64::new(0);
        (store.db).verify_with(
            store.latest(),
            Scope::Still,
            Some(&mut Rules(&calls)),
            drops,
        )
    };
    let before = check(Some(drops));
    assert!(before.is_sound(), "{before}");
    assert_eq!(before.census.left_flat_pages, 0);
    // A store that does not name the keyspace has pages nothing reaches.
    assert_eq!(check(None).rules(), [Rule::FlatPageUnreached]);

    // The subtree with the many flat records is dropped: the leaves
    // between the two ends of its range are unlinked unread.
    drop_subtree(&store, FLAT);
    let dropped = check(Some(drops));
    assert!(dropped.is_sound(), "{dropped}");
    assert!(dropped.census.left_flat_pages >= 1, "{dropped}");
    assert!(dropped.census.flat_records < before.census.flat_records - 200);
    assert!(dropped
        .to_string()
        .contains("more pages left by range deletes"));
    let unnamed = check(None);
    assert_eq!(unnamed.rules(), [Rule::FlatPageUnreached], "{unnamed}");

    // A commit later the reclaimer frees them, and takes their record.
    commit_later(&store);
    let swept = store.db.sweep(&store.versions);
    assert!(!swept.waiting && swept.blocked.is_none(), "{swept:?}");
    assert!(!swept.flat_drops.is_empty(), "{swept:?}");
    let after = check(Some(drops));
    assert!(after.is_sound(), "{after}");
    assert_eq!(after.census.left_flat_pages, 0);
    assert!(
        after.census.flat_pages < dropped.census.flat_pages,
        "{after}"
    );
    assert_eq!(after.census.dropped_merks, 0);
    assert!(after.unreached.is_empty(), "{after}");

    // A record that names a tree page, and one that names a page which a
    // keyspace reaches.
    let at = store.latest();
    let record = |id: u64, page: PageId| {
        let key = [&[2][..], &id.to_be_bytes()].concat();
        let value = [&at.get().to_le_bytes()[..], &page.encode(), &[0, 0]].concat();
        (key, value)
    };
    let (tree_page, reached) = (store.tree_pages()[0], store.db.keyspaces().aux.root());
    store.write_flat(|pages| {
        for (id, page) in [(100, tree_page), (101, reached)] {
            let (key, value) = record(id, page);
            drops.tree().put(pages, &key, &value).expect("a record");
        }
    });
    let damaged = check(Some(drops));
    assert!(damaged.has(Rule::FlatDropRecord), "{damaged}");
    assert!(damaged.has(Rule::FlatPageNamedTwice), "{damaged}");

    // A record that is no record.
    store.write_flat(|pages| {
        (drops
            .tree()
            .put(pages, &[2, 0, 0, 0, 0, 0, 0, 0, 200], b"x"))
        .expect("a record");
    });
    assert!(check(Some(drops)).has(Rule::FlatDropRecord));
    store.db.unchecked();
}

/// A commit that changes nothing of what a test looks at: a page that the
/// latest commit wrote is not freed before the next one.
fn commit_later(store: &Planted) {
    let mut world = World::new(&store.db, &store.versions);
    let tx = world.begin();
    world.write(
        &tx,
        vec![put(model::unit(Column::Aux, BIG), b"later", b"1")],
    );
    world.commit(tx, &[]);
}

/// A reclaiming of unlinked flat pages that stops part of the way keeps
/// its record, and the id of a page it freed is given out again, to a tree
/// page as well. The record then names a page that is another page: the
/// reclaimer passes it by its birth, and so does the checker.
#[test]
fn a_page_under_the_id_of_a_freed_one_is_not_what_a_range_delete_left() {
    let store = plant();
    {
        let mut world = World::new(&store.db, &store.versions);
        let tx = world.begin();
        // More records of the subtree, and records of the subtrees beside
        // it in the column: its range then lies between two leaves that
        // stay, and the leaves between them are unlinked one by one.
        let meta = |name| model::unit(Column::Meta, name);
        let more = (300..900).map(|index| put(meta(FLAT), &key(index), &[index as u8; 60]));
        let beside = [FLAT - 1, FLAT + 1].into_iter().flat_map(|name| {
            (0..40).map(move |index| put(meta(name), &key(index), &[index as u8; 60]))
        });
        world.write(&tx, more.chain(beside).collect());
        world.commit(tx, &[]);
    }
    drop_subtree(&store, FLAT);
    commit_later(&store);
    let (_, flat_drops, mut wait) = store.db.left(&store.versions);
    assert!(wait.poll(&store.versions));
    // The drop left a record for each column it unlinked pages of: the
    // one of the many records names the leaves between the two ends.
    let record = flat_drops.iter().max_by_key(|record| record.pages.len());
    let record = record.expect("the drop left pages").clone();
    assert!(record.pages.len() > 2, "{flat_drops:?}");

    // One page step: the first of the pages is freed, and the record stays.
    {
        let reclaimer = store.db.reclaimer(&store.versions);
        let guard = store.versions.guard();
        let mut done = FlatReclaimed::default();
        let step = reclaimer.reclaim_flat(&store.db.flat_drops(), &record, &guard, 1, &mut done);
        assert_eq!(step.expect("a step"), Progress::More);
    }
    let stopped = store.check();
    assert!(stopped.is_sound(), "{stopped}");
    assert!(stopped.census.left_flat_pages > 0, "{stopped}");

    // The freed id comes back, and a Merk that grows takes it for a tree
    // page: the lowest free id is given out first.
    store.versions.reclaim();
    store.versions.reclaim();
    {
        let mut world = World::new(&store.db, &store.versions);
        let tx = world.begin();
        world.edit(&tx, 2, |items| {
            for index in 0..300 {
                items.insert(key(index), vec![index as u8; 40]);
            }
        });
        world.commit(tx, &[]);
    }
    let trees = store.tree_pages();
    let reused = record
        .pages
        .iter()
        .filter(|left| trees.contains(&left.page));
    assert_eq!(
        reused.count(),
        1,
        "the id of the freed page names a tree page"
    );
    let report = store.check();
    assert!(report.is_sound(), "{report}");
    assert_eq!(
        report.census.left_flat_pages,
        stopped.census.left_flat_pages
    );

    let swept = store.db.sweep(&store.versions);
    assert!(
        swept.blocked.is_none() && !swept.flat_drops.is_empty(),
        "{swept:?}"
    );
    let after = store.check();
    assert!(after.is_sound(), "{after}");
    assert_eq!(after.census.left_flat_pages, 0);
}

/// Several columns may be kept in one keyspace, each under a lead of its
/// own. The keyspace is walked once, and a record is of the column whose
/// lead it begins with.
#[test]
fn columns_that_share_a_keyspace_are_walked_once() {
    let store = plant();
    store.db.unchecked();
    let mut made = None;
    store.write_flat(|pages| {
        let tree = FlatTree::create(pages, store.db.config()).expect("a keyspace");
        for (lead, name) in [(&b"a"[..], BIG), (b"m", OUTER), (b"ma", INNER)] {
            let key = [lead, &prefix(name)[..], b"record"].concat();
            tree.put(pages, &key, b"a value").expect("a put");
        }
        made = Some(tree);
    });
    let shared = made.expect("the keyspace was made");
    let check = |leads: &[&'static [u8]]| {
        let mut columns = store.db.keyspaces().columns().to_vec();
        columns.extend(leads.iter().map(|lead| FlatColumn { tree: shared, lead }));
        let calls = AtomicU64::new(0);
        let options = Options::new(store.latest(), Scope::Still);
        verify::check(
            &parts(&store.db, &columns),
            &Guard,
            options,
            Some(&mut Rules(&calls)),
        )
    };
    // One lead is the beginning of another: the record is of the column
    // under which the directory has its prefix.
    let all = check(&[b"a", b"m", b"ma"]);
    assert!(all.is_sound(), "{all}");
    // A record under a lead that no column of the keyspace has.
    let fewer = check(&[b"a", b"m"]);
    assert_eq!(fewer.rules(), [Rule::FlatRecordWithoutEntry], "{fewer}");
    assert_eq!(fewer.findings.len(), 1);
    assert_eq!(check(&[b"a"]).findings.len(), 2);
    // The keyspace named as that of no column holds no record of a subtree.
    let unnamed = check(&[]);
    assert_eq!(unnamed.rules(), [Rule::FlatPageUnreached], "{unnamed}");
}

/// With no rules no hash is recomputed, and a rule that refuses is a
/// finding: the caller's rules answer for every node.
#[test]
fn the_hashes_are_recomputed_by_the_rules_that_are_given() {
    struct Refusing;
    impl HashRules for Refusing {
        fn value_hash(
            &mut self,
            node: NodeRef<'_>,
            _: &[u8],
            _: &[Option<[u8; 32]>],
        ) -> Result<[u8; 32], Refusal> {
            Ok(*node.value_hash())
        }

        fn kv_hash(&mut self, _: &[u8], _: &[u8; 32]) -> Result<[u8; 32], Refusal> {
            Err("no rule".into())
        }

        fn node_hash(&mut self, node: NodeRef<'_>) -> Result<[u8; 32], Refusal> {
            let none = [0; 32];
            Ok(*node.link(Side::Left).map_or(&none, |link| link.hash))
        }
    }

    let store = plant();
    let unhashed = store.db.verify(store.latest(), Scope::Still, None);
    assert!(unhashed.is_sound(), "{unhashed}");
    assert_eq!(unhashed.census.hashed_cells, 0);

    let refused = store
        .db
        .verify(store.latest(), Scope::Still, Some(&mut Refusing));
    assert_eq!(refused.rules(), [Rule::HashUnchecked], "{refused}");
    assert_eq!(refused.findings.len() as u64, refused.census.live_cells);
}

/// A run keeps its findings up to the limit it is given and counts the
/// others.
#[test]
fn a_run_keeps_its_findings_up_to_a_limit() {
    let store = plant();
    store.db.unchecked();
    struct Wrong;
    impl HashRules for Wrong {
        fn value_hash(
            &mut self,
            _: NodeRef<'_>,
            _: &[u8],
            _: &[Option<[u8; 32]>],
        ) -> Result<[u8; 32], Refusal> {
            Ok([1; 32])
        }

        fn kv_hash(&mut self, _: &[u8], _: &[u8; 32]) -> Result<[u8; 32], Refusal> {
            Ok([2; 32])
        }

        fn node_hash(&mut self, _: NodeRef<'_>) -> Result<[u8; 32], Refusal> {
            Ok([3; 32])
        }
    }
    // Every stored hash of every cell in use differs: more findings than
    // a run keeps unless it is told otherwise.
    let all = store
        .db
        .verify(store.latest(), Scope::Still, Some(&mut Wrong));
    assert_eq!(all.findings.len(), Options::MAX_FINDINGS);
    let total = all.findings.len() as u64 + all.more_findings;
    assert!(total > 2 * 405, "{total}");
    assert!(!all.is_sound());

    let columns = store.db.keyspaces().columns();
    let options = Options {
        max_findings: 5,
        ..Options::new(store.latest(), Scope::Still)
    };
    let few = verify::check(
        &parts(&store.db, &columns),
        &Guard,
        options,
        Some(&mut Wrong),
    );
    assert_eq!(few.findings.len(), 5);
    assert_eq!(few.more_findings, total - 5);
    assert!(!few.is_sound());
    let text = few.to_string();
    assert!(
        text.starts_with(&format!("NOT SOUND: {total} findings")),
        "{text}"
    );
    assert!(
        text.contains(&format!("and {} more", few.more_findings)),
        "{text}"
    );
    assert_eq!(
        Options::new(store.latest(), Scope::default()).scope,
        Scope::Reader
    );
}

/// What a finding and a report say, for whoever reads them.
#[test]
fn a_report_reads_as_the_rule_the_place_and_what_was_found() {
    let store = plant();
    store.db.unchecked();
    let page = store.tree_pages()[0];
    (store.db.ids.free(page, store.latest())).expect("it is live");
    let report = store.check();
    assert_eq!(report.rules(), [Rule::IdNotLive]);
    let finding = &report.findings[0];
    assert_eq!(
        finding.to_string(),
        format!(
            "page {page}: the page is of the version and its id is retired (a page of the \
             latest version has a live id)"
        )
    );
    let text = report.to_string();
    for part in [
        "NOT SOUND: 1 findings",
        "tree pages with",
        "flat pages with",
        "hosted roots followed",
        "value-log files",
        "page ids:",
        "links of the page table name",
        "checked in",
    ] {
        assert!(text.contains(part), "{part:?} is not in:\n{text}");
    }

    let cell =
        lignin_format::CellAddress::new(page, lignin_format::SlotId::new(3).expect("a slot"));
    let file = lignin_format::FileId::MIN;
    let block = lignin_store::segment::BlockLocation {
        file,
        offset: 64,
        len: 10,
    };
    let places = [
        (Place::Store, "the store".to_owned()),
        (Place::Page(page), format!("page {page}")),
        (Place::Cell(cell), format!("cell {cell}")),
        (
            Place::FlatEntry { page, slot: 2 },
            format!("entry 2 of flat page {page}"),
        ),
        (Place::Merk([0xab; 32]), format!("Merk {}", "ab".repeat(32))),
        (
            Place::Block(block),
            format!("block at 64 of segment {file}, 10 bytes"),
        ),
        (Place::ValueFile(file), format!("value-log file {file}")),
    ];
    for (place, text) in places {
        assert_eq!(place.to_string(), text);
    }
    // Every rule says what holds in a sound store, and no two say the same.
    let mut texts = std::collections::BTreeSet::new();
    for rule in ALL_RULES {
        let text = rule.to_string();
        assert!(text.len() > 15 && text == rule.text(), "{rule:?}");
        assert!(texts.insert(text), "{rule:?}");
    }
    let finding = Finding {
        rule: Rule::Chain,
        place: Place::Store,
        detail: "a detail".to_owned(),
    };
    assert_eq!(
        finding.to_string(),
        "the store: a detail (a chain of the page table keeps the rules of chains)"
    );
    assert_eq!(Report::default().phases.total(), std::time::Duration::ZERO);
}

const ALL_RULES: [Rule; 40] = {
    use lignin_format::tree_page::HashField;
    [
        Rule::PageUnreadable,
        Rule::PageId,
        Rule::TreePageFormat,
        Rule::Fragments,
        Rule::DanglingAddress,
        Rule::CellNamedTwice,
        Rule::LinkKeyLength,
        Rule::LinkHeights,
        Rule::KeyOrder,
        Rule::Hash(HashField::Value),
        Rule::Hash(HashField::KeyValue),
        Rule::Hash(HashField::Child(Side::Left)),
        Rule::HashUnchecked,
        Rule::ValueUnreadable,
        Rule::ValueOwner,
        Rule::ValueShared,
        Rule::PendingBytes,
        Rule::PendingInSegment,
        Rule::PlacedLength,
        Rule::ValueCount,
        Rule::Directory,
        Rule::EntryRoot,
        Rule::EntryHost,
        Rule::HostPointer,
        Rule::HostedUnentered,
        Rule::HostUnnamed,
        Rule::FlatRecordWithoutEntry,
        Rule::FlatPageFormat,
        Rule::FlatDanglingPage,
        Rule::FlatPageNamedTwice,
        Rule::FlatLevel,
        Rule::FlatKeyOrder,
        Rule::FlatBucket,
        Rule::FlatPageUnreached,
        Rule::FlatDropRecord,
        Rule::IdNotLive,
        Rule::IdWithoutPage,
        Rule::Chain,
        Rule::Block,
        Rule::BlockNamedTwice,
    ]
};

/// The rules of chains, which the page table keeps for every link it
/// takes: no call of it makes a chain that breaks one.
#[test]
fn a_chain_that_breaks_a_rule_of_chains_is_told() {
    let version = |raw| CommitVersion::new(raw).expect("a version");
    let block = lignin_store::segment::BlockLocation {
        file: lignin_format::FileId::MIN,
        offset: 64,
        len: 10,
    };
    let (base, delta, free) = (
        |at| Link::base(version(at), block),
        |at| Link::delta(version(at), block),
        |at| Link::free(version(at)),
    );
    assert!(matches!(base(1).kind, LinkKind::Base(_)));
    for (before, link, fault) in [
        (None, base(1), None),
        (None, free(1), None),
        (None, delta(1), Some("a chain begins with a run of deltas")),
        (Some(base(2)), delta(2), None),
        (Some(delta(2)), delta(3), None),
        (Some(delta(2)), base(2), None),
        (Some(base(2)), free(3), None),
        (Some(free(3)), base(3), None),
        (
            Some(base(3)),
            delta(2),
            Some("the versions of a chain go down"),
        ),
        (
            Some(free(3)),
            delta(4),
            Some("a run of deltas follows a free link"),
        ),
        (
            Some(free(3)),
            free(4),
            Some("a free link follows a free link"),
        ),
    ] {
        assert_eq!(chain_fault(before, link), fault, "{before:?} {link:?}");
    }
}

/// The check that a store's drop makes: a store that is left unsound fails
/// its test, unless the test took the store out.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "the store of a test is left unsound")]
fn a_store_that_is_left_unsound_fails_its_test() {
    let Planted { db, versions } = plant();
    (db.ids.free(PageId::MIN, versions.latest())).expect("it is live");
}

#[test]
fn a_host_of_a_merk_that_stands_alone_is_the_fixtures_own() {
    let store = plant();
    let host = store.cell(|_, cell| cell.hosted_root_count() > 0 && cell.key() == b"host");
    let inner = store.root(INNER);
    // The host is given a second Merk: one that every store of the tests
    // has, and then one that the test entered.
    for (hosted, reported) in [(BIG, false), (OUTER, true)] {
        let second = store.root(hosted);
        store.step_tree(host, |page| {
            let roots = [Some(inner), Some(second)];
            page.set_hosted_roots(host.slot(), &roots)
                .expect("the pointers");
        });
        let calls = AtomicU64::new(0);
        let report = store
            .db
            .verify(store.latest(), Scope::Still, Some(&mut Rules(&calls)));
        assert!(report.has(Rule::HostUnnamed), "{report}");
        let unsound = store.db.unsound(&report);
        assert_eq!(
            unsound
                .iter()
                .any(|finding| finding.rule == Rule::HostUnnamed),
            reported,
            "{report}"
        );
    }
    store.db.unchecked();
}

/// The checker reads the tree pages through the tree layer's own trait, and
/// changes none: a write, a new page and a freed page are refused.
#[test]
fn the_checker_changes_no_page() {
    use lignin_tree::{
        fragment::{FragmentError, Pages},
        node::Lease,
    };

    use crate::verify::read::{Frames, TreeRead};

    let store = plant();
    let root = store.root(BIG);
    let page = root.page();
    let frames = Frames::new(store.db.loader(), store.latest());
    let mut pages = TreeRead::new(frames, Lease::take());
    let found = pages.read(page, |page| page.node(root.slot()).is_some());
    assert!(found.expect("the page is read"));
    let refused = [
        pages.write(page, |_| Ok(())).expect_err("no write"),
        pages.create().map(|_| ()).expect_err("no new page"),
        pages.free(page).expect_err("no page is freed"),
    ];
    for error in refused {
        assert!(matches!(error, FragmentError::Pages(_)), "{error}");
        let said = "the invariant checker changes no page";
        assert!(error.to_string().contains(said), "{error}");
    }
}
