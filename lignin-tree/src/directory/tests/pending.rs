//! The records of a batch that are in no page yet ([`Pending`]): what the
//! writes of the batch read through them, what settling them leaves in the
//! directory, and how often it writes a leaf.

use std::collections::BTreeSet;

use lignin_io::sim::SimRng;

use super::{
    super::{Pending, Rekey, RekeyPlan, Restated, Settled},
    *,
};

/// Every record of the directory, as its keyspace has it.
fn records(pages: &Pages, directory: &Directory) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut cursor = directory.tree().cursor();
    let mut out = Vec::new();
    cursor.seek_to_first(pages).expect("a walk");
    while let Some(key) = cursor.key() {
        let mut value = Vec::new();
        assert!(cursor.value(pages, &mut value).expect("a value"));
        out.push((key.to_vec(), value));
        cursor.next(pages).expect("a walk");
    }
    out
}

fn image<'a>(pages: &'a Pages, page: PageId) -> FlatPage<'a> {
    FlatPage::decode(&pages.pages[&page], V1).expect("a page")
}

/// The leaf of the directory whose key range holds `key`, found through
/// the branches as the page format reads them.
fn leaf_of(pages: &Pages, directory: &Directory, key: &[u8]) -> PageId {
    let mut page = directory.root_page();
    loop {
        match image(pages, page) {
            FlatPage::Leaf(_) => return page,
            FlatPage::Branch(branch) => page = branch.child_for(key).expect("a child"),
        }
    }
}

fn leaves(pages: &Pages) -> u64 {
    let leaf = |page: &&PageId| image(pages, **page).leaf().is_some();
    pages.pages.keys().filter(leaf).count() as u64
}

/// What a settling did, with what the pages say of it.
#[derive(Debug)]
struct Done {
    settled: Settled,
    /// The pages that the keyspace has and did not have before: the
    /// pieces of the pages that were cut.
    made: u64,
    /// The pages that were freed: a leaf that a merge joined into
    /// another, a leaf that was left with no record, and a branch or a
    /// root's only child that went with one.
    freed: u64,
}

/// Holds what a settling of `pending` wrote to what the pages were handed
/// since `from` images of leaves: **no page was handed two images of a
/// leaf**, and the count of the keyspace's own code agrees.
fn once_a_page(pages: &Pages, from: usize, settled: Settled, counted: u64) {
    let handed = &pages.leaf_pages[from..];
    assert_eq!(settled.leaf_writes, handed.len() as u64);
    assert_eq!(
        settled.leaf_writes,
        crate::flat::leaf_images_written() - counted
    );
    let distinct: BTreeSet<PageId> = handed.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        handed.len(),
        "a page was handed two images of a leaf: {handed:?}"
    );
}

/// Settles `pending`, and holds what the settling says to what the pages
/// were handed and to the leaves that the keys of the records lay in:
/// **each of those leaves is written once or freed unwritten, one image
/// more is written for each page that a cut made, and no page is handed
/// two.** A test that calls this has every record change its leaf, and a
/// keyspace that keeps its levels.
fn settle(pages: &mut Pages, directory: &Directory, pending: Pending) -> Done {
    let homes: BTreeSet<PageId> = (pending.keys())
        .map(|key| leaf_of(pages, directory, key))
        .collect();
    let homes = homes.len() as u64;
    let (records, from) = (pending.len() as u64, pages.leaf_pages.len());
    let counted = crate::flat::leaf_images_written();
    let (had, frees): (BTreeSet<PageId>, _) = (pages.pages.keys().copied().collect(), pages.frees);
    let levels = pages.levels(directory);
    let settled = directory.settle(pages, pending).expect("settled");
    assert_eq!(settled.records, records);
    assert_eq!(settled.leaves, homes);
    once_a_page(pages, from, settled, counted);
    assert_eq!(pages.levels(directory), levels);
    let made = pages
        .pages
        .keys()
        .filter(|page| !had.contains(page))
        .count() as u64;
    let freed = pages.frees - frees;
    // At most one image for each leaf, and one more for each page that a
    // cut made: what the unit asks of a batch.
    assert!(
        (homes.saturating_sub(freed)..=homes + made).contains(&settled.leaf_writes),
        "{settled:?} for records in {homes} leaves, with {made} pages made and {freed} freed"
    );
    Done {
        settled,
        made,
        freed,
    }
}

fn key_of(n: u32) -> Vec<u8> {
    format!("host-{n:05}").into_bytes()
}

fn below_top(n: u32) -> Prefix {
    prefix_of(&[(key_of(n), 0)])
}

/// A directory with the root of the grove, which has a root, and `count`
/// Merks hosted in it, entered one call each.
fn wide(count: u32) -> (Pages, Directory) {
    let (mut pages, directory) = Pages::directory();
    let top = prefix_of(&[]);
    directory.add(&mut pages, &top, b"tree", None).expect("top");
    let rooted = named(&top, Some((cell(1, 1), b"root")));
    directory.set_roots(&mut pages, &[rooted]).expect("a root");
    for n in 0..count {
        let host = Host {
            parent: &top,
            key: &key_of(n),
            position: 0,
        };
        let added = directory.add(&mut pages, &below_top(n), b"tree", Some(host));
        added.expect("entered");
    }
    (pages, directory)
}

#[test]
fn a_batch_of_enters_writes_each_leaf_once_and_the_pieces_of_those_it_cuts() {
    let (mut pages, directory) = wide(400);
    let top = prefix_of(&[]);
    let (mut single, same) = (pages.clone(), directory.clone());
    let (before, all) = (pages.pages.clone(), leaves(&pages));
    let single_before = single.leaf_writes;
    assert!(all > 4, "{all} leaves");

    let mut pending = Pending::new();
    let next = directory.next_id(&pages).expect("an id");
    for n in 400..700 {
        let host = Host {
            parent: &top,
            key: &key_of(n),
            position: 0,
        };
        let id = directory
            .add_in(&mut pending, &pages, &below_top(n), b"tree", Some(host))
            .expect("entered");
        assert_eq!(id.get(), next.get() + u64::from(n - 400));
        same.add(&mut single, &below_top(n), b"tree", Some(host))
            .expect("entered");
    }
    // An entry and the record of its host for each, and the header.
    assert_eq!(pending.len(), 601);
    // Nothing of them is in a page: a read of the pages finds none, and a
    // read through the records finds each.
    assert_eq!(pages.pages, before);
    let last = below_top(699);
    assert_eq!(directory.open(&pages, &last).expect("a lookup"), None);
    let through = directory.open_in(&pending, &pages, &last);
    assert!(through.expect("a lookup").is_some());

    let Done {
        settled,
        made,
        freed,
    } = settle(&mut pages, &directory, pending);
    assert_eq!(freed, 0, "no leaf is merged");
    let pieces = leaves(&pages) - all;
    assert!(
        pieces > 0,
        "the entries are more than their leaves had room for"
    );
    // Each leaf once, and each piece of a leaf that was cut.
    assert_eq!(settled.leaf_writes, settled.leaves + pieces);
    assert!(settled.leaves <= all && settled.records == 601 && pieces <= made);
    println!("{settled:?} for 300 enters, with {pieces} new leaves, and {all} leaves before");

    // The directory holds what a call for each Merk leaves it holding,
    // and those calls wrote two leaves or three for each Merk.
    assert!(records(&pages, &directory) == records(&single, &same));
    assert!(single.leaf_writes - single_before >= 600);
    assert_eq!(
        directory.check(&pages).expect("a directory"),
        same.check(&single).expect("a directory")
    );
    assert!(directory.open(&pages, &last).expect("a lookup").is_some());
}

#[test]
fn a_batch_of_drops_writes_each_leaf_once() {
    let (mut pages, mut directory) = wide(900);
    let (mut single, mut same) = (pages.clone(), directory.clone());
    let all = leaves(&pages);
    // Every fifth Merk is dropped: no leaf is left under a quarter of a
    // page, so none is merged.
    let mut pending = Pending::new();
    let dropped: Vec<u32> = (0..900).step_by(5).collect();
    for n in &dropped {
        let left = directory.unlink_in(&mut pending, &pages, &below_top(*n));
        assert_eq!(left.expect("dropped").root, None);
        same.unlink(&mut single, &below_top(*n)).expect("dropped");
    }
    // The entry and the record of the host go, and a record is left.
    assert_eq!(pending.len(), 3 * dropped.len());
    assert!(directory
        .open(&pages, &below_top(0))
        .expect("a lookup")
        .is_some());
    let through = directory.open_in(&pending, &pages, &below_top(0));
    assert_eq!(through.expect("a lookup"), None);

    let single_writes = single.leaf_writes - pages.leaf_writes;
    let done = settle(&mut pages, &directory, pending);
    println!("{done:?} for 180 drops of {all} leaves, {single_writes} leaf writes a call each");
    assert!(
        done.settled.leaves <= all && done.made + done.freed <= 3,
        "{done:?}"
    );
    assert!(single_writes > 10 * done.settled.leaf_writes);
    assert!(records(&pages, &directory) == records(&single, &same));
    assert_eq!(
        directory.open(&pages, &below_top(0)).expect("a lookup"),
        None
    );
    assert!(directory
        .open(&pages, &below_top(1))
        .expect("a lookup")
        .is_some());
    let census = directory.check(&pages).expect("a directory");
    assert_eq!((census.live, census.dropped), (901 - 180, 180));
}

#[test]
fn a_batch_of_clears_writes_each_leaf_once() {
    let (mut pages, mut directory) = wide(600);
    // Every third Merk has a node, in cells of pages of its own.
    let rooted: Vec<u32> = (0..600).step_by(3).collect();
    for (at, n) in rooted.iter().enumerate() {
        let prefix = below_top(*n);
        let root = cell(100 + at as u32, 1 + (at % 7) as u16);
        let stated = directory.set_roots(&mut pages, &[named(&prefix, Some((root, b"k")))]);
        stated.expect("a root");
    }
    let (mut single, mut same) = (pages.clone(), directory.clone());
    let all = leaves(&pages);

    let mut pending = Pending::new();
    let next = directory.next_id(&pages).expect("an id");
    for (at, n) in rooted.iter().enumerate() {
        let prefix = below_top(*n);
        let was = directory.open(&pages, &prefix).expect("a lookup");
        let was = was.expect("entered");
        let left = directory.clear_in(&mut pending, &pages, &prefix);
        let left = left.expect("cleared").expect("it had a root");
        assert_eq!(
            (left.id, left.root),
            (was.id(), was.root().map(|r| r.address))
        );
        // The Merk is read through the records: with no root, under the
        // id that this clear gave out.
        let now = directory.open_in(&pending, &pages, &prefix);
        let now = now.expect("a lookup").expect("entered");
        assert_eq!(now.id().get(), next.get() + at as u64);
        assert_eq!(now.root(), None);
        // Cleared again in the same batch, it has nothing to leave.
        let again = directory.clear_in(&mut pending, &pages, &prefix);
        assert_eq!(again.expect("cleared"), None);
        same.clear(&mut single, &prefix).expect("cleared");
    }
    // Of each: the record of its root goes, its entry and the record of
    // its host are written, and a record is left. And the header.
    assert_eq!(pending.len(), 4 * rooted.len() + 1);
    // A Merk that is not rooted puts nothing.
    let none = directory.clear_in(&mut pending, &pages, &below_top(1));
    assert_eq!(none.expect("cleared"), None);
    assert_eq!(pending.len(), 4 * rooted.len() + 1);

    let single_writes = single.leaf_writes - pages.leaf_writes;
    let done = settle(&mut pages, &directory, pending);
    println!("{done:?} for 200 clears of {all} leaves, {single_writes} leaf writes a call each");
    assert!(done.settled.leaves <= all, "{done:?}");
    assert!(single_writes > 10 * done.settled.leaf_writes);
    assert!(records(&pages, &directory) == records(&single, &same));
    let census = directory.check(&pages).expect("a directory");
    assert_eq!((census.live, census.dropped), (601, 200));
}

#[test]
fn a_batch_of_moves_writes_each_leaf_once() {
    let (mut pages, mut directory) = wide(600);
    let (mut single, mut same) = (pages.clone(), directory.clone());
    let all = leaves(&pages);
    // Two hundred Merks go to other keys of the Merk that hosts them, and
    // two of them change places.
    let moved: Vec<(Prefix, Prefix, Vec<u8>)> = (0..200)
        .map(|n| (below_top(n), below_top(1_000 + n), key_of(1_000 + n)))
        .chain([
            (below_top(300), below_top(301), key_of(301)),
            (below_top(301), below_top(300), key_of(300)),
        ])
        .collect();
    let rekeys: Vec<Rekey<'_>> = (moved.iter())
        .map(|(from, to, key)| Rekey {
            from,
            to,
            key,
            below: &[],
        })
        .collect();
    let plan = directory.plan_rekey(&pages, &rekeys, &|_| false);
    let plan = plan.expect("a plan");
    let ids: Vec<MerkId> = (moved.iter())
        .map(|(from, ..)| directory.open(&pages, from).expect("a lookup"))
        .map(|entry| entry.expect("entered").id())
        .collect();

    let mut pending = Pending::new();
    let rekeyed = directory.rekey_pairs_in(&mut pending, &pages, &plan);
    assert_eq!(rekeyed.expect("moved").left, Vec::new());
    same.rekey_pairs(&mut single, &plan).expect("moved");
    // Of each: its entry and the record of its host go and are written
    // under the new prefix and the new key. The two that change places
    // leave and take the same four keys.
    assert_eq!(pending.len(), 4 * 200 + 4);

    let single_writes = single.leaf_writes - pages.leaf_writes;
    let done = settle(&mut pages, &directory, pending);
    println!("{done:?} for 202 moves of {all} leaves, {single_writes} leaf writes for the call");
    assert!(done.settled.leaves <= all, "{done:?}");
    assert!(records(&pages, &directory) == records(&single, &same));
    for ((from, to, key), id) in moved.iter().zip(&ids) {
        let entry = directory.open(&pages, to).expect("a lookup");
        let entry = entry.expect("the Merk is under its new prefix");
        assert_eq!(entry.id(), *id);
        assert_eq!(entry.host().map(|host| host.key), Some(&key[..]));
        let stays = moved.iter().any(|(_, to, _)| to == from);
        let there = directory.open(&pages, from).expect("a lookup");
        assert_eq!(there.is_some(), stays);
    }
    directory.check(&pages).expect("a directory");
}

#[test]
fn a_write_of_a_batch_reads_what_the_writes_before_it_put() {
    let (mut pages, mut directory) = wide(4);
    let top = prefix_of(&[]);
    let host = |parent, key| Host {
        parent,
        key,
        position: 0,
    };
    // A Merk with no node that hosts one, both in the pages: the calls
    // keep no rule of the callers, and a batch that drops both finds it.
    let hosting = below_top(3);
    let hosted = prefix_of(&[(key_of(3), 0), (b"d".to_vec(), 0)]);
    directory
        .add(&mut pages, &hosted, b"tree", Some(host(&hosting, b"d")))
        .expect("entered");
    let before = pages.pages.clone();
    let mut pending = Pending::new();

    // A Merk and a Merk it hosts, entered in one batch: the second reads
    // the first, for the Merks it is below.
    let (outer, inner) = (prefix_of(&path(&["o"])), prefix_of(&path(&["o", "i"])));
    let outer_id = directory
        .add_in(
            &mut pending,
            &pages,
            &outer,
            b"tree",
            Some(host(&top, b"o")),
        )
        .expect("entered");
    let inner_id = directory
        .add_in(
            &mut pending,
            &pages,
            &inner,
            b"sum",
            Some(host(&outer, b"i")),
        )
        .expect("entered");
    let entry = directory.open_in(&pending, &pages, &inner);
    let entry = entry.expect("a lookup").expect("entered");
    assert_eq!(entry.id(), inner_id);
    assert_eq!(entry.parent_id(), Some(outer_id));
    assert_eq!(entry.depth(), 2);
    // Entered twice, and at a host that is taken: refused, with nothing
    // put.
    let records = pending.clone();
    let twice = directory.add_in(
        &mut pending,
        &pages,
        &inner,
        b"sum",
        Some(host(&outer, b"j")),
    );
    assert!(matches!(twice, Err(DirectoryError::Exists { .. })));
    let other = prefix_of(&path(&["o", "other"]));
    let taken = directory.add_in(
        &mut pending,
        &pages,
        &other,
        b"sum",
        Some(host(&outer, b"i")),
    );
    assert!(matches!(taken, Err(DirectoryError::HostTaken { .. })));
    assert_eq!(pending, records);

    // Their roots are named in the same batch, and a root that is carried
    // is found by the record that is pending.
    let changes = [
        named(&outer, Some((cell(4, 1), b"i"))),
        named(&inner, Some((cell(4, 2), b"n"))),
    ];
    directory
        .set_roots_in(&mut pending, &pages, &changes)
        .expect("roots");
    let carried = RootChange::Carried {
        from: cell(4, 2),
        to: cell(5, 9),
    };
    directory
        .set_roots_in(&mut pending, &pages, &[carried])
        .expect("carried");
    let entry = directory.open_in(&pending, &pages, &inner);
    let root = entry
        .expect("a lookup")
        .expect("entered")
        .root()
        .map(|root| root.address);
    assert_eq!(root, Some(cell(5, 9)));
    // A root in a cell that a pending record gives to another Merk is
    // refused.
    let records = pending.clone();
    let first = below_top(0);
    let taken = named(&first, Some((cell(5, 9), b"x")));
    let refused = directory.set_roots_in(&mut pending, &pages, &[taken]);
    assert!(matches!(refused, Err(DirectoryError::RootTaken { .. })));
    assert_eq!(pending, records);

    // A Merk that is dropped and entered again in one batch: the enter
    // finds the prefix free, and the Merk has another id.
    let again = below_top(1);
    let was = directory.open(&pages, &again).expect("a lookup");
    let was = was.expect("entered").id();
    directory
        .unlink_in(&mut pending, &pages, &again)
        .expect("dropped");
    assert_eq!(
        directory
            .open_in(&pending, &pages, &again)
            .expect("a lookup"),
        None
    );
    let second = key_of(1);
    let id = directory
        .add_in(
            &mut pending,
            &pages,
            &again,
            b"big",
            Some(host(&top, &second)),
        )
        .expect("entered again");
    assert!(id > was && id > inner_id);

    // A Merk with no node that hosts one is not dropped, by a record of
    // the pages or by one that is pending, and is dropped once the Merk
    // it hosts was.
    let below = prefix_of(&[(key_of(2), 0), (b"c".to_vec(), 0)]);
    let parent = below_top(2);
    directory
        .add_in(
            &mut pending,
            &pages,
            &below,
            b"tree",
            Some(host(&parent, b"c")),
        )
        .expect("entered");
    let parent_id = directory.open(&pages, &parent).expect("a lookup");
    let parent_id = parent_id.expect("entered").id();
    assert!(!directory.has_children(&pages, parent_id).expect("a walk"));
    assert!(directory
        .has_children_in(&pending, &pages, parent_id)
        .expect("a walk"));
    let refused = directory.unlink_in(&mut pending, &pages, &parent);
    assert!(matches!(refused, Err(DirectoryError::NotEmpty { .. })));
    directory
        .unlink_in(&mut pending, &pages, &below)
        .expect("dropped");
    assert!(!directory
        .has_children_in(&pending, &pages, parent_id)
        .expect("a walk"));
    directory
        .unlink_in(&mut pending, &pages, &parent)
        .expect("dropped");

    // The same of a Merk that the pages have below one: its record of
    // the host is in the pages, and a pending record takes it out.
    let hosting_id = directory.open(&pages, &hosting).expect("a lookup");
    let hosting_id = hosting_id.expect("entered").id();
    let refused = directory.unlink_in(&mut pending, &pages, &hosting);
    assert!(matches!(refused, Err(DirectoryError::NotEmpty { .. })));
    directory
        .unlink_in(&mut pending, &pages, &hosted)
        .expect("dropped");
    assert!(directory.has_children(&pages, hosting_id).expect("a walk"));
    assert!(!directory
        .has_children_in(&pending, &pages, hosting_id)
        .expect("a walk"));
    directory
        .unlink_in(&mut pending, &pages, &hosting)
        .expect("dropped");

    // A Merk is stated anew as the records leave it.
    let restated = Restated {
        prefix: &outer,
        tree_type: b"count",
        position: None,
    };
    directory
        .restate_in(&mut pending, &pages, &[restated])
        .expect("stated");
    let entry = directory.open_in(&pending, &pages, &outer);
    let entry = entry.expect("a lookup").expect("entered");
    assert_eq!(entry.tree_type(), b"count");
    assert_eq!(entry.root().map(|root| root.address), Some(cell(4, 1)));

    // No page has anything of it until the records are settled.
    assert_eq!(pages.pages, before);
    assert_eq!(directory.open(&pages, &outer).expect("a lookup"), None);
    directory.settle(&mut pages, pending).expect("settled");
    let entry = directory.open(&pages, &inner).expect("a lookup");
    assert_eq!(
        entry.expect("entered").root().map(|r| r.address),
        Some(cell(5, 9))
    );
    assert_eq!(
        directory
            .open(&pages, &again)
            .expect("a lookup")
            .map(|e| e.id()),
        Some(id)
    );
    assert_eq!(directory.open(&pages, &parent).expect("a lookup"), None);
    assert_eq!(
        directory.root_owner(&pages, cell(4, 2)).expect("a lookup"),
        None
    );
    assert_eq!(
        directory.root_owner(&pages, cell(5, 9)).expect("a lookup"),
        Some(inner)
    );
    assert_eq!(
        directory.next_id(&pages).expect("an id").get(),
        id.get() + 2
    );
    assert_eq!(directory.open(&pages, &hosting).expect("a lookup"), None);
    assert_eq!(directory.open(&pages, &hosted).expect("a lookup"), None);
    directory.check(&pages).expect("a directory");
}

#[test]
fn records_that_are_dropped_unsettled_were_never_put() {
    let (mut pages, mut directory) = wide(40);
    let top = prefix_of(&[]);
    let (before, kept) = (pages.pages.clone(), directory.clone());
    let (revision, next) = (pages.revision, directory.next_id(&pages).expect("an id"));
    let mut pending = Pending::new();
    for n in 40..80 {
        let host = Host {
            parent: &top,
            key: &key_of(n),
            position: 0,
        };
        let added = directory.add_in(&mut pending, &pages, &below_top(n), b"tree", Some(host));
        added.expect("entered");
    }
    for n in 0..20 {
        let left = directory.unlink_in(&mut pending, &pages, &below_top(n));
        left.expect("dropped");
    }
    assert!(!pending.is_empty());
    drop(pending);
    // The pages are as they were, to the byte, and so is every answer of
    // a handle of them.
    assert_eq!((&pages.pages, pages.revision), (&before, revision));
    assert_eq!(kept.next_id(&pages).expect("an id"), next);
    for n in 0..80 {
        let entered = kept.open(&pages, &below_top(n)).expect("a lookup");
        assert_eq!(entered.is_some(), n < 40);
    }
    // The next batch is given the ids that the dropped one was.
    let mut pending = Pending::new();
    let host = Host {
        parent: &top,
        key: &key_of(90),
        position: 0,
    };
    let id = kept.add_in(&mut pending, &pages, &below_top(90), b"tree", Some(host));
    assert_eq!(id.expect("entered"), next);
    kept.settle(&mut pages, pending).expect("settled");
    kept.check(&pages).expect("a directory");
}

/// One directory that settles every call at once, and one that settles
/// the calls of a batch together.
struct Twins {
    single: (Pages, Directory),
    batched: (Pages, Directory),
    rng: SimRng,
    next_cell: u16,
    counts: Counts,
}

#[derive(Debug, Default)]
struct Counts {
    batches: u64,
    calls: u64,
    refused: u64,
    records: u64,
    leaves: u64,
    leaf_writes: u64,
    single_writes: u64,
    moves: u64,
    given_up: u64,
    through: u64,
    lost_levels: u64,
}

const POOL: u8 = 120;
const HOST_KEYS: [&[u8]; 5] = [b"a", b"b", b"c", b"", &[0x4C; 90]];

fn pool(n: u8) -> Prefix {
    match n {
        0 => prefix_of(&[]),
        n => [n; 32],
    }
}

/// What a call answered, as far as the two directories must agree on it.
fn answer<T: std::fmt::Debug>(result: &Result<T, DirectoryError>) -> String {
    match result {
        Ok(done) => format!("{done:?}"),
        Err(error) => format!("refused: {error}"),
    }
}

impl Twins {
    fn new(seed: u64) -> Self {
        let (mut pages, directory) = Pages::directory();
        let added = directory.add(&mut pages, &pool(0), b"tree", None);
        added.expect("the root of the grove");
        Self {
            single: (pages.clone(), directory.clone()),
            batched: (pages, directory),
            rng: SimRng::new(seed),
            next_cell: 0,
            counts: Counts::default(),
        }
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.rng.below(bound)
    }

    fn pick(&mut self) -> Prefix {
        pool(self.below(u64::from(POOL)) as u8)
    }

    /// A prefix of the pool: three times in four one under which a Merk
    /// is in use before the batch, if `live`, or one under which none is,
    /// where a few draws find one.
    fn pick_where(&mut self, live: bool) -> Prefix {
        let mut prefix = self.pick();
        if self.below(4) == 0 {
            return prefix;
        }
        for _ in 0..12 {
            let (pages, directory) = &self.single;
            if directory.open(pages, &prefix).expect("a lookup").is_some() == live {
                break;
            }
            prefix = self.pick();
        }
        prefix
    }

    /// Notes what the same call answered in the two directories: the same,
    /// and for a call that was refused, with no record put.
    fn same<T: std::fmt::Debug>(
        &mut self,
        single: Result<T, DirectoryError>,
        batched: Result<T, DirectoryError>,
        (before, pending): (&Pending, &Pending),
    ) -> bool {
        assert_eq!(answer(&single), answer(&batched));
        self.counts.calls += 1;
        if batched.is_err() {
            assert_eq!(pending, before, "a call that is refused puts no record");
            self.counts.refused += 1;
        }
        self.counts.through += u64::from(batched.is_ok() && !before.is_empty());
        batched.is_ok()
    }

    /// One batch, in the order a write transaction makes its calls: drops
    /// and clears, moves, Merks stated anew, enters, and the roots.
    fn batch(&mut self) {
        let start = self.single.0.leaf_writes;
        let mut pending = Pending::new();
        // The moves are planned on the directory as it is before the
        // batch, which is the same in both.
        let plan = (self.below(3) == 0).then(|| self.plan()).flatten();

        for _ in 0..self.below(4) {
            let (prefix, before) = (self.pick_where(true), pending.clone());
            let (single, batched) = (&mut self.single, &mut self.batched);
            if self.rng.below(3) == 0 {
                let one = single.1.clear(&mut single.0, &prefix);
                let two = batched.1.clear_in(&mut pending, &batched.0, &prefix);
                self.same(one, two, (&before, &pending));
            } else {
                let one = single.1.unlink(&mut single.0, &prefix);
                let two = batched.1.unlink_in(&mut pending, &batched.0, &prefix);
                self.same(one, two, (&before, &pending));
            }
        }
        let pending_before_moves = pending.clone();
        if let Some(plan) = plan {
            let (single, batched) = (&mut self.single, &mut self.batched);
            let one = single.1.rekey_pairs(&mut single.0, &plan);
            let two = batched.1.rekey_pairs_in(&mut pending, &batched.0, &plan);
            // A plan that the drops of its batch made stale is refused
            // when it is half made, and has put no record then, as any
            // call that is refused: the batch goes on.
            let before = pending_before_moves.clone();
            self.counts.given_up += u64::from(two.is_err());
            self.counts.moves += u64::from(self.same(one, two, (&before, &pending)));
        }
        for _ in 0..self.below(3) {
            let (prefix, before) = (self.pick_where(true), pending.clone());
            let tree_type = [&b"tree"[..], b"sum", b""][self.below(3) as usize];
            let stated = [Restated {
                prefix: &prefix,
                tree_type,
                position: (self.below(4) == 0).then_some(self.below(2) as u8),
            }];
            let (single, batched) = (&mut self.single, &mut self.batched);
            let one = single.1.restate(&mut single.0, &stated);
            let two = batched.1.restate_in(&mut pending, &batched.0, &stated);
            self.same(one, two, (&before, &pending));
        }
        for _ in 0..self.below(9) {
            let (prefix, parent) = (self.pick_where(false), self.pick_where(true));
            let before = pending.clone();
            let host = (self.below(30) > 0).then(|| Host {
                parent: &parent,
                key: HOST_KEYS[self.below(HOST_KEYS.len() as u64) as usize],
                position: self.below(2) as u8,
            });
            let (single, batched) = (&mut self.single, &mut self.batched);
            let one = single.1.add(&mut single.0, &prefix, b"tree", host);
            let two = batched
                .1
                .add_in(&mut pending, &batched.0, &prefix, b"tree", host);
            self.same(one, two, (&before, &pending));
        }
        for _ in 0..self.below(3) {
            let before = pending.clone();
            let named: Vec<(Prefix, Option<CellAddress>)> = (0..1 + self.below(3))
                .map(|_| {
                    let root = (self.below(5) > 0).then(|| {
                        self.next_cell = self.next_cell % 40 + 1;
                        cell(7 + u32::from(self.next_cell % 3), self.next_cell)
                    });
                    (self.pick_where(true), root)
                })
                .collect();
            let mut changes: Vec<RootChange<'_>> = (named.iter())
                .map(|(prefix, root)| super::named(prefix, root.map(|at| (at, &b"r"[..]))))
                .collect();
            if self.below(3) == 0 {
                let from = cell(7 + self.below(3) as u32, 1 + self.below(40) as u16);
                let to = cell(7 + self.below(3) as u32, 1 + self.below(40) as u16);
                changes.push(RootChange::Carried { from, to });
            }
            let (single, batched) = (&mut self.single, &mut self.batched);
            let one = single.1.set_roots(&mut single.0, &changes);
            let two = batched.1.set_roots_in(&mut pending, &batched.0, &changes);
            self.same(one, two, (&before, &pending));
        }

        // The batch is settled: the two directories hold the same records,
        // and their handles answer alike.
        let (pages, directory) = &mut self.batched;
        let (records, from) = (pending.len() as u64, pages.leaf_pages.len());
        let counted = crate::flat::leaf_images_written();
        let single_writes = self.single.0.leaf_writes - start;
        let levels = pages.levels(directory);
        let had: BTreeSet<PageId> = pages.pages.keys().copied().collect();
        let settled = directory.settle(pages, pending).expect("settled");
        assert_eq!(settled.records, records);
        assert!(settled.leaves <= settled.records);
        // At most one image for each leaf that took records, and one more
        // for each page that a cut made, whatever the keyspace's shape.
        let made = pages.pages.keys().filter(|page| !had.contains(page));
        let made = made.count() as u64;
        assert!(
            settled.leaf_writes <= settled.leaves + made,
            "{settled:?}, with {made} pages made"
        );
        self.counts.lost_levels += u64::from(pages.levels(directory) < levels);
        // No page is handed two images of a leaf, but the root where the
        // keyspace loses a level.
        match pages.levels(directory) == levels {
            true => once_a_page(pages, from, settled, counted),
            false => assert_eq!(settled.leaf_writes, (pages.leaf_pages.len() - from) as u64),
        }
        self.counts.batches += 1;
        self.counts.records += settled.records;
        self.counts.leaves += settled.leaves;
        self.counts.leaf_writes += settled.leaf_writes;
        self.counts.single_writes += single_writes;
        self.agree();
    }

    /// A plan for a move of a Merk in use to another key of its host and
    /// to a prefix of the pool, with what is below it left out.
    fn plan(&mut self) -> Option<RekeyPlan> {
        let (from, to) = (self.pick_where(true), self.pick_where(false));
        let key = HOST_KEYS[self.below(HOST_KEYS.len() as u64) as usize];
        let rekey = Rekey {
            from: &from,
            to: &to,
            key,
            below: &[],
        };
        let (pages, directory) = &self.batched;
        directory.plan_rekey(pages, &[rekey], &|_| false).ok()
    }

    fn agree(&self) {
        let ((single, one), (batched, two)) = (&self.single, &self.batched);
        assert!(records(single, one) == records(batched, two));
        // The calls keep no rule of the callers (a Merk with no node can
        // be made to host one), so the check may report: the same of
        // both.
        assert_eq!(answer(&one.check(single)), answer(&two.check(batched)));
        for n in 0..POOL {
            let opened = one.open(single, &pool(n)).expect("a lookup");
            assert_eq!(opened, two.open(batched, &pool(n)).expect("a lookup"));
        }
    }
}

/// The calls of a batch that are settled together leave the directory as
/// the same calls leave it when each is settled at once, answer by
/// answer: each call of the batch reads what the calls before it put.
#[test]
fn a_batch_that_is_settled_once_agrees_with_one_that_settles_every_call() {
    let mut total = Counts::default();
    for seed in 1..=12 {
        let mut twins = Twins::new(seed);
        for _ in 0..600 {
            twins.batch();
        }
        println!("seed {seed}: {:?}", twins.counts);
        let counts = twins.counts;
        total.batches += counts.batches;
        total.calls += counts.calls;
        total.refused += counts.refused;
        total.records += counts.records;
        total.leaves += counts.leaves;
        total.leaf_writes += counts.leaf_writes;
        total.single_writes += counts.single_writes;
        total.moves += counts.moves;
        total.given_up += counts.given_up;
        total.through += counts.through;
        total.lost_levels += counts.lost_levels;
    }
    println!("{total:?}");
    assert!(total.batches > 6_000 && total.through > 10_000, "{total:?}");
    assert!(total.refused > 5_000 && total.moves > 100, "{total:?}");
    assert!(
        total.given_up > 0,
        "a move was refused half made: {total:?}"
    );
    assert!(total.records > 2 * total.leaves, "{total:?}");
    assert!(2 * total.single_writes > 3 * total.leaf_writes, "{total:?}");
}

/// A re-keying that is refused when it is half made has put no record,
/// and the handle has the ids it had: the batch can go on.
#[test]
fn a_move_that_is_refused_half_made_puts_no_record() {
    let (mut pages, mut directory) = wide(6);
    let top = prefix_of(&[]);
    let (from, to) = (below_top(0), below_top(100));
    let rekey = Rekey {
        from: &from,
        to: &to,
        key: &key_of(100),
        below: &[],
    };
    let plan = directory.plan_rekey(&pages, &[rekey], &|_| false);
    let plan = plan.expect("a plan");
    // A call of the batch enters a Merk where the plan found none: the
    // plan is of another state now.
    let mut pending = Pending::new();
    let host = Host {
        parent: &top,
        key: b"other",
        position: 0,
    };
    directory
        .add_in(&mut pending, &pages, &to, b"tree", Some(host))
        .expect("entered");
    let (before, kept) = (pending.clone(), directory.clone());
    let refused = directory.rekey_pairs_in(&mut pending, &pages, &plan);
    assert!(
        matches!(refused, Err(DirectoryError::Exists { .. })),
        "{refused:?}"
    );
    assert_eq!(pending, before);
    directory.settle(&mut pages, pending).expect("settled");
    // The Merk that was to move is where it was, and the one that was
    // entered is there.
    assert!(directory.open(&pages, &from).expect("a lookup").is_some());
    assert!(directory.open(&pages, &to).expect("a lookup").is_some());
    for n in 0..6 {
        let opened = directory.open(&pages, &below_top(n)).expect("a lookup");
        assert_eq!(opened, kept.open(&pages, &below_top(n)).expect("a lookup"));
    }
    directory.check(&pages).expect("a directory");
}

/// The walk of the Merks below a Merk reads keys and no value, with
/// pending records as without: a record of the host that is damaged and
/// that a pending record takes out stands in nobody's way, as it does not
/// once the drop that takes it out is settled by itself.
#[test]
fn a_damaged_record_that_a_pending_record_takes_out_is_passed() {
    let made = || {
        let (mut pages, directory) = wide(2);
        let hosting = below_top(1);
        let hosted = prefix_of(&[(key_of(1), 0), (b"d".to_vec(), 0)]);
        let host = Host {
            parent: &hosting,
            key: b"d",
            position: 0,
        };
        directory
            .add(&mut pages, &hosted, b"tree", Some(host))
            .expect("entered");
        // The record of the host holds bytes that are no prefix and id.
        let entry = directory.open(&pages, &hosting).expect("a lookup");
        let key = super::super::record::child_key(entry.expect("entered").id(), 0, b"d");
        let tree = *directory.tree();
        tree.put(&mut pages, &key, b"no record").expect("a record");
        (pages, directory, hosting, hosted)
    };
    let (mut pages, mut directory, hosting, hosted) = made();
    directory.unlink(&mut pages, &hosted).expect("dropped");
    directory.unlink(&mut pages, &hosting).expect("dropped");

    let (mut batched, mut same, hosting, hosted) = made();
    let mut pending = Pending::new();
    same.unlink_in(&mut pending, &batched, &hosted)
        .expect("dropped");
    same.unlink_in(&mut pending, &batched, &hosting)
        .expect("dropped");
    same.settle(&mut batched, pending).expect("settled");
    assert!(records(&pages, &directory) == records(&batched, &same));
}

/// A batch whose calls leave a record as the pages have it writes no leaf
/// for it.
#[test]
fn a_batch_that_restores_what_it_changed_writes_no_leaf() {
    let (mut pages, directory) = wide(50);
    let (before, revision) = (pages.pages.clone(), pages.revision);
    let prefix = below_top(7);
    let mut pending = Pending::new();
    for tree_type in [&b"sum"[..], b"tree"] {
        let stated = Restated {
            prefix: &prefix,
            tree_type,
            position: None,
        };
        directory
            .restate_in(&mut pending, &pages, &[stated])
            .expect("stated");
    }
    assert_eq!(pending.len(), 1);
    let settled = directory.settle(&mut pages, pending).expect("settled");
    let none = Settled {
        records: 1,
        leaves: 0,
        leaf_writes: 0,
    };
    assert_eq!(settled, none);
    assert_eq!((&pages.pages, pages.revision), (&before, revision));
}

/// A settling that fails in a page after it wrote a leaf is reported as
/// the keyspace reports it: the writer's pages are then given up.
#[test]
fn a_settling_that_fails_after_a_leaf_is_reported() {
    let (mut pages, mut directory) = wide(400);
    let mut pending = Pending::new();
    for n in (0..400).step_by(3) {
        let left = directory.unlink_in(&mut pending, &pages, &below_top(n));
        left.expect("dropped");
    }
    let homes: BTreeSet<PageId> = (pending.keys())
        .map(|key| leaf_of(&pages, &directory, key))
        .collect();
    assert!(homes.len() > 3);
    let written = pages.leaf_writes;
    pages.writes_left = Some(2);
    let failed = directory.settle(&mut pages, pending);
    assert!(
        matches!(failed, Err(DirectoryError::Flat(FlatError::Freed { .. }))),
        "{failed:?}"
    );
    assert_eq!(
        pages.leaf_writes - written,
        2,
        "two leaves were written before"
    );
}

/// A re-keying that is refused after it dropped a Merk with a node leaves
/// the handle with the ids it had: what is below that Merk is still
/// opened.
#[test]
fn a_move_that_is_refused_half_made_leaves_the_handle_its_ids() {
    let (mut pages, mut directory) = wide(6);
    let top = prefix_of(&[]);
    let (from, to) = (below_top(0), below_top(100));
    let host = |parent, key| Host {
        parent,
        key,
        position: 0,
    };
    // A Merk below the one that moves, with no node, and one below that:
    // the move names neither, so both leave with it.
    let inner = prefix_of(&[(key_of(0), 0), (b"c".to_vec(), 0)]);
    let innermost = prefix_of(&[(key_of(0), 0), (b"c".to_vec(), 0), (b"d".to_vec(), 0)]);
    directory
        .add(&mut pages, &inner, b"tree", Some(host(&from, b"c")))
        .expect("entered");
    directory
        .add(&mut pages, &innermost, b"tree", Some(host(&inner, b"d")))
        .expect("entered");
    let rekey = Rekey {
        from: &from,
        to: &to,
        key: &key_of(100),
        below: &[],
    };
    let plan = directory.plan_rekey(&pages, &[rekey], &|_| false);
    let plan = plan.expect("a plan");
    // Calls of the batch give the Merk below a node, and enter a Merk
    // where the plan found none: the plan is of another state now. It
    // drops the Merk below, which has a node by then, and is refused.
    let mut pending = Pending::new();
    let rooted = named(&inner, Some((cell(50, 1), b"k")));
    directory
        .set_roots_in(&mut pending, &pages, &[rooted])
        .expect("a root");
    directory
        .add_in(
            &mut pending,
            &pages,
            &to,
            b"tree",
            Some(host(&top, b"other")),
        )
        .expect("entered");
    let before = pending.clone();
    let refused = directory.rekey_pairs_in(&mut pending, &pages, &plan);
    assert!(
        matches!(refused, Err(DirectoryError::Exists { .. })),
        "{refused:?}"
    );
    assert_eq!(pending, before);
    let opened = directory.open_in(&pending, &pages, &innermost);
    assert!(
        opened.expect("a lookup").is_some(),
        "the Merk below is in use"
    );
    directory.settle(&mut pages, pending).expect("settled");
    assert!(directory
        .open(&pages, &innermost)
        .expect("a lookup")
        .is_some());
    let loaded = Directory::load(&pages, directory.root_page(), pages.config);
    let loaded = loaded.expect("a directory");
    for prefix in [from, inner, innermost, to] {
        let opened = directory.open(&pages, &prefix).expect("a lookup");
        assert_eq!(opened, loaded.open(&pages, &prefix).expect("a lookup"));
    }
}

/// The first Merk below a Merk is read as the walk reads it once the
/// records are settled: a record of the pages that stays is read before a
/// pending one that is behind it, so a value that cannot be read ends the
/// call with that error, with pending records as without.
#[test]
fn a_pending_record_does_not_hide_a_record_of_the_pages_before_it() {
    let made = || {
        // Values of more than sixteen bytes are out of line.
        let mut pages = Pages::new();
        pages.config = (pages.config)
            .with_max_inline(FlatConfig::MIN_INLINE)
            .expect("a limit");
        let config = pages.config;
        let directory = Directory::create(&mut pages, config).expect("a directory");
        let top = prefix_of(&[]);
        directory
            .add(&mut pages, &top, b"tree", None)
            .expect("entered");
        let parent = prefix_of(&path(&["p"]));
        let host = Host {
            parent: &top,
            key: b"p",
            position: 0,
        };
        let id = directory
            .add(&mut pages, &parent, b"tree", Some(host))
            .expect("entered");
        let first = prefix_of(&path(&["p", "a"]));
        let host = Host {
            parent: &parent,
            key: b"a",
            position: 0,
        };
        directory
            .add(&mut pages, &first, b"tree", Some(host))
            .expect("entered");
        (pages, directory, parent, id)
    };
    let last = prefix_of(&path(&["p", "z"]));
    let add_last = |directory: &Directory, pending: &mut Pending, pages: &Pages, parent| {
        let host = Host {
            parent,
            key: b"z",
            position: 0,
        };
        directory
            .add_in(pending, pages, &last, b"tree", Some(host))
            .expect("entered");
    };
    // The value of the record of the first Merk below is lost.
    let lose = |pages: &mut Pages, directory: &Directory, id: MerkId| {
        let key = super::super::record::child_key(id, 0, b"a");
        let leaf = leaf_of(pages, directory, &key);
        let image = image(pages, leaf).leaf().expect("a leaf");
        let held = image.get(&key).expect("the record");
        let lignin_format::flat_page::FlatValue::OutOfLine(place) = held else {
            panic!("the record is kept out of line");
        };
        let place = ValuePlace::decode(place.try_into().expect("16 bytes")).expect("a place");
        let name = place.pending().expect("a pending value").id();
        assert!(pages.values.remove(&name).is_some());
    };

    // Each call settled by itself.
    let (mut pages, mut directory, parent, id) = made();
    let mut pending = Pending::new();
    add_last(&directory, &mut pending, &pages, &parent);
    directory.settle(&mut pages, pending).expect("settled");
    lose(&mut pages, &directory, id);
    let single = directory.unlink(&mut pages, &parent);
    assert!(
        matches!(single, Err(DirectoryError::Flat(FlatError::NoValue { .. }))),
        "{single:?}"
    );

    // The same with the enter pending.
    let (mut pages, mut directory, parent, id) = made();
    lose(&mut pages, &directory, id);
    let mut pending = Pending::new();
    add_last(&directory, &mut pending, &pages, &parent);
    let batched = directory.unlink_in(&mut pending, &pages, &parent);
    assert_eq!(answer(&single), answer(&batched));

    // And a record that is damaged, which the walk for a drop does not
    // decode and the first Merk below is: with a pending record behind it
    // and with one that takes it out.
    let (mut pages, directory, parent, id) = made();
    let key = super::super::record::child_key(id, 0, b"a");
    let tree = *directory.tree();
    tree.put(&mut pages, &key, b"no record").expect("a record");
    let mut pending = Pending::new();
    add_last(&directory, &mut pending, &pages, &parent);
    assert!(directory
        .has_children_in(&pending, &pages, id)
        .expect("a walk"));
    let first = directory.first_child_in(&pending, &pages, id);
    assert!(
        matches!(&first, Err(DirectoryError::Damaged { key: at, .. }) if *at == key),
        "{first:?}"
    );
    let mut without = directory.children(id);
    assert_eq!(answer(&first), answer(&without.next(&pages)));
    // Once a pending record takes it out, the pending record behind it is
    // the first, and nothing of the damaged one is read.
    let (mut pending, mut handle) = (Pending::new(), directory.clone());
    add_last(&directory, &mut pending, &pages, &parent);
    handle
        .unlink_in(&mut pending, &pages, &prefix_of(&path(&["p", "a"])))
        .expect("dropped");
    let first = directory.first_child_in(&pending, &pages, id);
    assert_eq!(first.expect("a walk").map(|child| child.prefix), Some(last));
}
