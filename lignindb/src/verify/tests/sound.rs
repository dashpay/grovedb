//! Stores that pass, what a report says of them, and what a run is told.

use std::sync::atomic::AtomicU64;

use lignin_format::{tree_page::Side, CommitVersion, PageId};
use lignin_store::table::{Link, LinkKind};
use lignin_tree::{
    directory::record,
    drop::FlatDrops,
    hash::{HashRules, Refusal},
    node::NodeRef,
};

use super::{key, plant, prefix, write_back, Planted, BIG, FLAT, GONE, INNER, OUTER};
use crate::{
    snapshot::Versions,
    txn::{
        tests::{
            model::{self, OwnedBatch, OwnedSubtree},
            put,
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

/// A Merk that is entered, filled and dropped, and whose record is then
/// taken out of the directory: what a reclaiming leaves from the moment it
/// begins on the cells (`lignin_tree::drop`, step 2).
const LOST: u8 = ENTERED + 14;
const LOST_NODES: usize = 20;

fn rules(calls: &AtomicU64) -> Rules<'_> {
    Rules(calls)
}

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
    let before = (store.db).verify(pin.version(), Scope::Reader, Some(&mut rules(&calls)));
    assert!(before.is_sound(), "{before}");
    assert_eq!(before.census.ids, None);
    assert_eq!(before.census.links, 0);

    {
        let mut world = World::new(&store.db, &store.versions);
        world.model.version = store.latest().get();
        let tx = world.begin();
        let batch = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(model::subtree(OUTER))],
            ..OwnedBatch::default()
        };
        // The model of this world knows no record of the store: the drop
        // is applied to the store alone.
        let scope = tx.txn.scope().expect("a scope");
        batch
            .with(|batch| scope.apply(batch))
            .expect("the subtree is dropped");
        scope.merge();
        (store.db.commit(&store.versions, &tx.txn)).expect("a commit");
    }
    assert!(store.latest() > pin.version());
    let pinned = (store.db).verify(pin.version(), Scope::Reader, Some(&mut rules(&calls)));
    assert!(pinned.is_sound(), "{pinned}");
    assert_eq!(pinned.census.merks, before.census.merks);
    assert_eq!(pinned.census.live_cells, before.census.live_cells);
    let latest = store.check();
    assert!(latest.is_sound(), "{latest}");
    assert_eq!(latest.census.merks, before.census.merks - 2);
    assert_eq!(latest.census.dropped_merks, 2);
    assert_eq!(latest.census.live_cells, before.census.live_cells - 5);
}

/// What a reclaiming leaves from the moment it takes the record of a
/// dropped Merk: cells that nothing leads to. They are counted, and they
/// are no finding.
#[test]
fn cells_that_nothing_reaches_are_reported_apart() {
    let store = plant();
    {
        let mut world = World::new(&store.db, &store.versions);
        world.model.version = store.latest().get();
        let tx = world.begin();
        let lost = world.merk(&tx, LOST, |items| {
            for index in 0..LOST_NODES {
                let len = if index % 4 == 0 { 300 } else { 20 };
                items.insert(key(index), vec![index as u8; len]);
            }
        });
        let batch = OwnedBatch {
            merks: vec![lost],
            subtrees: vec![OwnedSubtree::Enter(model::subtree(LOST), None)],
            ..OwnedBatch::default()
        };
        world.apply(&tx, &batch).expect("the subtree is made");
        world.commit(tx, &[]);
    }
    let id = {
        let guard = store.versions.guard();
        let pages = store.db.committed(&guard, store.latest());
        let directory = store.db.directory_at(&pages);
        let entry = directory.stored(&pages, &prefix(LOST)).expect("a read");
        entry.expect("the Merk is entered").id()
    };
    {
        let mut world = World::new(&store.db, &store.versions);
        world.model.version = store.latest().get();
        let tx = world.begin();
        let batch = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(model::subtree(LOST))],
            ..OwnedBatch::default()
        };
        let scope = tx.txn.scope().expect("a scope");
        batch
            .with(|batch| scope.apply(batch))
            .expect("the subtree is dropped");
        scope.merge();
        (store.db.commit(&store.versions, &tx.txn)).expect("a commit");
    }
    write_back(&store.db, &store.versions);
    let dropped = store.check();
    assert!(dropped.is_sound(), "{dropped}");
    assert!(dropped.unreached.is_empty());
    assert_eq!(dropped.census.dropped_merks, 2);

    // The record of the dropped Merk goes, as the reclaimer takes it
    // before the first cell.
    store.write_flat(|pages| {
        let key = record::dropped_key(id);
        assert!(store.db.directory().delete(pages, &key).expect("a delete"));
    });
    let report = store.check();
    assert!(report.is_sound(), "{report}");
    let unreached = &report.unreached;
    assert_eq!(unreached.cells, LOST_NODES as u64);
    assert_eq!(unreached.values, LOST_NODES as u64 / 4);
    assert!(unreached.pages >= 1 && unreached.bytes > 20 * LOST_NODES as u64);
    assert_eq!(unreached.first.len(), Unreached::SAMPLE.min(LOST_NODES));
    assert_eq!(
        report.census.left_cells,
        dropped.census.left_cells - LOST_NODES as u64
    );
    // Their values stay counted, so the counts still agree.
    assert_eq!(report.census.placed_values, dropped.census.placed_values);
    let text = report.to_string();
    assert!(text.starts_with("sound: every rule holds"), "{text}");
    assert!(
        text.contains("space that nothing reaches: 20 cells"),
        "{text}"
    );
    store.db.unchecked();
}

/// A store with no directory is walked from the roots it is given, through
/// every pointer to a hosted root.
#[test]
fn a_store_with_no_directory_is_checked_from_its_roots() {
    let planted = plant();
    let db = &planted.db;
    let roots = [planted.root(BIG), planted.root(OUTER)];
    let check = |roots: &[_], values_counted| {
        let store = Store {
            loader: db.loader(),
            table: &db.table,
            segments: &db.segments,
            values: &db.values,
            page_size: SIZE,
            ids: None,
            grove: Grove::Roots(roots),
            flat_drops: None,
            flat: &[],
            values_counted,
        };
        let calls = AtomicU64::new(0);
        let options = Options::new(planted.latest(), Scope::Still);
        verify::check(&store, &Guard, options, Some(&mut rules(&calls)))
    };
    let report = check(&roots, false);
    // With no directory, the keyspaces are not known: their pages are
    // flat pages that nothing reaches, and nothing else is found.
    assert_eq!(report.rules(), [Rule::FlatPageUnreached], "{report}");
    assert_eq!(report.census.live_cells, 400 + 3 + 2);
    assert_eq!(report.census.hosted_roots, 1);
    assert_eq!(report.census.ids, None);
    // What the dropped Merk left has nothing that leads to it here.
    assert_eq!(report.unreached.cells, 43);
    // A root that is left out leaves its Merk unreached, and no finding.
    let fewer = check(&roots[..1], false);
    assert_eq!(fewer.rules(), [Rule::FlatPageUnreached]);
    assert_eq!(fewer.unreached.cells, 43 + 5);
}

struct Guard;

impl lignin_tree::OperationGuard for Guard {}

/// The pages that a lazy range delete left are flat pages that no
/// keyspace reaches: the store names the keyspace of their records, and
/// they are walked from there.
#[test]
fn the_pages_a_range_delete_left_are_walked_from_its_record() {
    let store = plant();
    let meta = store.db.keyspaces().meta;
    let mut drops = None;
    store.write_flat(|pages| {
        drops = Some(FlatDrops::create(pages, store.db.config()).expect("a keyspace"));
    });
    let drops = drops.expect("the keyspace was made");
    let check = |drops: Option<FlatDrops>| {
        let calls = AtomicU64::new(0);
        (store.db).verify_with(
            store.latest(),
            Scope::Still,
            Some(&mut rules(&calls)),
            drops,
        )
    };
    let before = check(Some(drops));
    assert!(before.is_sound(), "{before}");
    assert_eq!(before.census.left_flat_pages, 0);
    // A store that does not name the keyspace has pages nothing reaches.
    assert_eq!(check(None).rules(), [Rule::FlatPageUnreached]);

    let at = store.latest();
    store.write_flat(|pages| {
        let range = [&prefix(FLAT)[..], b"key"].concat();
        let left = drops.unlink_prefix(pages, &meta, &range, at);
        assert!(
            left.expect("the range is unlinked").is_some(),
            "pages are left"
        );
    });
    let report = check(Some(drops));
    assert!(report.is_sound(), "{report}");
    assert!(report.census.left_flat_pages >= 1, "{report}");
    assert!(report.census.flat_records < before.census.flat_records - 200);
    assert!(report
        .to_string()
        .contains("more pages left by range deletes"));
    let unnamed = check(None);
    assert_eq!(unnamed.rules(), [Rule::FlatPageUnreached], "{unnamed}");

    // A record that names a tree page, and one that names a page which a
    // keyspace reaches.
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

    let keyspaces = store.db.keyspaces();
    let columns = [
        keyspaces.data,
        keyspaces.aux,
        keyspaces.roots,
        keyspaces.meta,
    ]
    .map(|tree| lignin_tree::drop::Column { tree, lead: &[] });
    let parts = Store {
        loader: store.db.loader(),
        table: &store.db.table,
        segments: &store.db.segments,
        values: &store.db.values,
        page_size: SIZE,
        ids: Some(&store.db.ids),
        grove: Grove::Directory {
            tree: store.db.directory(),
            columns: &columns,
        },
        flat_drops: None,
        flat: &[],
        values_counted: true,
    };
    let options = Options {
        max_findings: 5,
        ..Options::new(store.latest(), Scope::Still)
    };
    let few = verify::check(&parts, &Guard, options, Some(&mut Wrong));
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
    let page = (db.ids.scan(PageId::MIN, &mut Vec::new())).map_or(PageId::MIN, |_| PageId::MIN);
    (db.ids.free(page, versions.latest())).expect("it is live");
}

#[test]
fn a_host_of_a_merk_that_stands_alone_is_the_fixtures_own() {
    let store = plant();
    let host = store.cell(|_, cell| cell.hosted_root_count() > 0 && cell.key() == b"host");
    let _ = (GONE, INNER);
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
            .verify(store.latest(), Scope::Still, Some(&mut rules(&calls)));
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

fn _versions(_: &Versions) {}
