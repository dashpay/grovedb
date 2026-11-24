//! What the rest of the engine does on a store that is not sound.
//!
//! The lists of `injected` ask the checker for the finding of each
//! corruption. The tests here ask the other readers and writers of the
//! engine a weaker thing, of the same stores and of stores that are damaged
//! at random: that every call returns. An error is an answer. What is none:
//!
//! - a panic that is raised in the engine's code,
//! - a cursor that shows more records than the store holds,
//! - a call that does not return.
//!
//! The damage is a page step, as in `injected`: a changed copy of a page in
//! the place of its current version. A page that is damaged at random is one
//! that a load takes: its image is well formed and holds the id of its page
//! (`lignin_store::load`, "What is checked"). So each case is a store that
//! a directory with right checksums can give the engine.
//!
//! A panic is told by where it was raised. The fixtures of these tests
//! expect an `Ok` of the engine in many places, and on a damaged store the
//! engine rightly answers an error there: a panic that is raised in a file
//! of the tests is that, and is no finding.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    panic::{self, AssertUnwindSafe},
    sync::{mpsc, Once},
    thread,
    time::Duration,
};

use lignin_format::{
    flat_page::{FlatPage, FlatPageKind},
    tree_page::{Side, TreePage},
    CellAddress, PageId,
};
use lignin_store::load::FRAME_FORMAT;
use lignin_tree::value::ValueBuf;

use super::{
    super::{plant_with_its_model, write_back},
    key, Planted, BESIDE_THE_PAGES, BIG, CELLS, DIRECTORY, FLAT, FLAT_TREES, INNER, NEVER, OUTER,
    VALUES, WHAT_IS_LEFT,
};
use crate::{
    txn::{
        tests::{
            delete,
            model::{self, OwnedBatch, OwnedSubtree},
            put,
            store::{Rng, SIZE},
            World,
        },
        Column, Unit,
    },
    verify::tests::{BELOW, GONE},
};

/// What a walk of these tests says when it has taken more steps than the
/// planted store has records.
const ENDLESS: &str = "a walk that does not end";

/// More steps than a walk of anything in the planted store takes: its
/// largest Merk has 400 nodes and its largest keyspace a few hundred
/// records.
const WALK_LIMIT: usize = 20_000;

/// How long a case may take before it is said not to return. A case on a
/// sound store takes a fraction of a second.
const CASE_LIMIT: Duration = Duration::from_secs(120);

/// Where a panic was raised, and what it said.
#[derive(Debug, Clone)]
struct Raised {
    file: String,
    line: u32,
    message: String,
}

thread_local! {
    /// Whether the panics of this thread are recorded and not printed.
    static WATCHED: Cell<bool> = const { Cell::new(false) };
    /// The first panic of this thread since it was last taken.
    static RAISED: RefCell<Option<Raised>> = const { RefCell::new(None) };
    /// What the checker said of the store in each check of this thread's
    /// drill: whether it is sound.
    static SOUND: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
}

/// Records the panics of the threads that ask for it, and leaves those of
/// every other thread to the hook that was there.
fn watch_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let before = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !WATCHED.with(Cell::get) {
                return before(info);
            }
            let at = info.location();
            let raised = Raised {
                file: at.map_or_else(String::new, |at| at.file().to_owned()),
                line: at.map_or(0, |at| at.line()),
                message: info.payload_as_str().unwrap_or("no message").to_owned(),
            };
            RAISED.with(|first| {
                first.borrow_mut().get_or_insert(raised);
            });
        }));
    });
}

/// How a step of a drill ended.
#[derive(Debug, Clone)]
enum End {
    /// It returned.
    Returned,
    /// A fixture of the tests did not get the answer it expects of a
    /// sound store: where, and what it said.
    Refused(Raised),
    /// A walk took more steps than the store has records.
    Endless(String),
    /// A panic in the engine's code.
    Panicked(Raised),
}

impl End {
    fn is_a_finding(&self) -> bool {
        matches!(self, Self::Endless(_) | Self::Panicked(_))
    }
}

/// Whether `file` is a file of tests: a panic raised there is a fixture's.
fn of_the_tests(file: &str) -> bool {
    file.contains("/tests/") || file.ends_with("/tests.rs")
}

/// Runs `step` and says how it ended.
fn ended(step: impl FnOnce()) -> End {
    watch_panics();
    RAISED.with(|first| first.borrow_mut().take());
    WATCHED.with(|watched| watched.set(true));
    let caught = panic::catch_unwind(AssertUnwindSafe(step));
    WATCHED.with(|watched| watched.set(false));
    let raised = RAISED.with(|first| first.borrow_mut().take());
    match (caught, raised) {
        (Ok(()), _) => End::Returned,
        (Err(_), Some(raised)) if raised.message.starts_with(ENDLESS) => {
            End::Endless(raised.message)
        }
        (Err(_), Some(raised)) if of_the_tests(&raised.file) => End::Refused(raised),
        (Err(_), Some(raised)) => End::Panicked(raised),
        (Err(_), None) => End::Panicked(Raised {
            file: String::new(),
            line: 0,
            message: "a panic that the hook did not see".to_owned(),
        }),
    }
}

/// The subtrees the drill asks for: every one the planted store has made,
/// the two it dropped, and one that was never there.
const NAMES: [u8; 7] = [BIG, FLAT, OUTER, INNER, GONE, BELOW, NEVER];

/// The keys the drill asks for by name, beside [`key`].
const KEYS: [&[u8]; 12] = [
    b"host",
    b"left",
    b"right",
    b"one",
    b"two",
    b"x",
    b"y",
    b"a",
    b"im",
    b"root",
    b"long value",
    b"",
];

fn counted(steps: &mut usize, of: &str) {
    *steps += 1;
    assert!(*steps <= WALK_LIMIT, "{ENDLESS}: {of}");
}

/// Every read of a write transaction that has written nothing: the entry
/// of each subtree, its nodes by key and under a cursor in both
/// directions, each node's value and the child on each side, and the flat
/// records of each column by key and under a cursor.
fn reads(_: &Planted, world: &mut World<'_>) {
    let tx = world.begin();
    let mut long_key = vec![b'k'; 3000];
    let mut keys: Vec<Vec<u8>> = (0..400).map(key).collect();
    keys.extend(KEYS.iter().map(|key| key.to_vec()));
    keys.push(long_key.clone());
    long_key.push(b'k');
    keys.push(long_key);
    let mut out = Vec::new();
    for name in NAMES {
        let subtree = model::subtree(name);
        let _ = tx.txn.subtree(subtree, |entry| entry.is_some());
        for key in &keys {
            let handle = tx.txn.node(subtree, key, |node| {
                node.and_then(|node| {
                    let mut value = ValueBuf::new();
                    let _ = node.value(&mut value);
                    let _ = (node.link(Side::Left), node.link(Side::Right));
                    node.handle()
                })
            });
            let Ok(Some(handle)) = handle else { continue };
            for side in Side::BOTH {
                let _ = tx.txn.child(subtree, handle, side, |child| {
                    child.map(|child| {
                        let mut value = ValueBuf::new();
                        let _ = child.value(&mut value);
                        child.key().len()
                    })
                });
            }
        }
        if let Ok(mut cursor) = tx.txn.merk_cursor(subtree) {
            for back in [false, true] {
                let mut steps = 0;
                let mut moved = match back {
                    false => cursor.seek_to_first(),
                    true => cursor.seek_to_last(),
                };
                while moved.is_ok() && cursor.key().is_some() {
                    counted(&mut steps, "a cursor over a Merk");
                    let _ = cursor.node(|node| {
                        node.map(|node| {
                            let mut value = ValueBuf::new();
                            let _ = node.value(&mut value);
                        })
                    });
                    moved = match back {
                        false => cursor.next(),
                        true => cursor.prev(),
                    };
                }
            }
            for key in keys.iter().step_by(37) {
                let _ = cursor.seek(key);
                let _ = cursor.seek_for_prev(key);
            }
        }
        for column in Column::ALL {
            let unit = Unit::new(column, subtree);
            for key in &keys {
                out.clear();
                let _ = tx.txn.get(unit, key, &mut out);
            }
            let mut cursor = tx.txn.cursor(unit);
            for back in [false, true] {
                let mut steps = 0;
                let mut moved = match back {
                    false => cursor.seek_to_first(),
                    true => cursor.seek_to_last(),
                };
                while moved.is_ok() && cursor.key().is_some() {
                    counted(&mut steps, "a cursor over flat records");
                    out.clear();
                    let _ = cursor.value(&mut out);
                    moved = match back {
                        false => cursor.next(),
                        true => cursor.prev(),
                    };
                }
            }
            for key in keys.iter().step_by(37) {
                let _ = cursor.seek(key);
                let _ = cursor.seek_for_prev(key);
            }
        }
    }
    let _ = tx.txn.rollback();
}

/// The reads the model of the tests makes of a transaction that has
/// written nothing, each compared with the model.
fn compared(_: &Planted, world: &mut World<'_>) {
    let reader = world.begin();
    world.check(&reader);
}

fn checked(planted: &Planted, _: &mut World<'_>) {
    let report = planted.check();
    SOUND.with(|sound| sound.borrow_mut().push(report.is_sound()));
    if std::env::var_os("LIGNIN_HOSTILE_REPORTS").is_some() {
        println!("REPORT {report}");
    }
}

/// The reclaiming of what the committed drops left.
fn swept(planted: &Planted, _: &mut World<'_>) {
    for _ in 0..4 {
        let _ = planted.db.sweep(&planted.versions);
    }
}

fn written_back(planted: &Planted, _: &mut World<'_>) {
    write_back(&planted.db, &planted.versions);
}

/// One commit that changes every Merk and every keyspace of the store:
/// nodes taken out, put between the others and given a value of the other
/// kind, in a Merk over many pages, in a Merk that hosts one and in the
/// hosted one, whose host the same batch puts, and flat records put and
/// deleted under short and long keys.
fn written(_: &Planted, world: &mut World<'_>) {
    let tx = world.begin();
    world.edit(&tx, BIG, |items| {
        for index in (0..400).step_by(3) {
            items.remove(&key(index));
        }
        for index in 0..90 {
            let len = if index % 4 == 0 { 500 } else { 25 };
            items.insert(
                format!("key{index:04}+").into_bytes(),
                vec![index as u8; len],
            );
        }
        for index in (1..400).step_by(7) {
            if let Some(value) = items.get_mut(&key(index)) {
                *value = vec![9; if value.len() > 100 { 20 } else { 450 }];
            }
        }
    });
    let mut outer = world.merk(&tx, OUTER, |items| {
        // The host's node is put with another value, so that the batch
        // holds it and names the hosted Merk in it, whatever becomes the
        // root of that Merk (`tree-layout.md` T3).
        items.insert(b"host".to_vec(), b"grove".to_vec());
        items.remove(&b"left"[..]);
        for index in 0..30 {
            items.insert(key(index), vec![index as u8; 35 + 20 * (index % 3)]);
        }
    });
    (outer.hosted).insert(b"host".to_vec(), vec![model::subtree(INNER)]);
    let inner = world.merk(&tx, INNER, |items| {
        items.remove(&b"one"[..]);
        for index in 0..30 {
            items.insert(key(index), vec![index as u8; 400 - 10 * index]);
        }
    });
    let meta = model::unit(Column::Meta, FLAT);
    let aux = model::unit(Column::Aux, FLAT);
    let mut records = vec![
        put(aux, b"long value", &[8; 20]),
        delete(aux, &[b'k'; 3000]),
        put(aux, &[b'j'; 2900], &[6; 2000]),
    ];
    for index in 0..300 {
        records.push(match index % 3 {
            0 => delete(meta, &key(index)),
            1 => put(meta, &key(index), &[index as u8; 700]),
            _ => put(meta, &key(1000 + index), &[index as u8; 40]),
        });
    }
    let batch = OwnedBatch {
        merks: vec![inner, outer],
        records,
        ..OwnedBatch::default()
    };
    let _ = world.apply(&tx, &batch);
    world.commit(tx, &[]);
}

/// The drop of each Merk the store has in use, each in a commit of its
/// own. The Merk that another hosts goes with its host.
fn dropped(_: &Planted, world: &mut World<'_>) {
    for name in [OUTER, BIG] {
        let tx = world.begin();
        let batch = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(model::subtree(name))],
            ..OwnedBatch::default()
        };
        let _ = world.apply(&tx, &batch);
        world.commit(tx, &[]);
    }
}

type Step = for<'s> fn(&'s Planted, &mut World<'s>);

/// A name and a change to the children of a flat branch.
type BranchChange = (&'static str, fn(PageId, &mut Vec<PageId>));

/// What is asked of a damaged store, in this order. A step that a fixture
/// refuses is followed by the next all the same: after an error a session
/// goes on.
const DRILL: &[(&str, Step)] = &[
    ("the checker", checked),
    ("the reads", reads),
    ("the reads the model makes", compared),
    ("the reclaiming of what a drop left", swept),
    ("a commit into every Merk and keyspace", written),
    ("the reads after the commit", reads),
    ("write-back", written_back),
    ("the checker after the commit", checked),
    ("the drop of every subtree", dropped),
    ("the reclaiming of the dropped subtrees", swept),
    ("the reads after the drops", reads),
    ("write-back after the drops", written_back),
    ("the checker after the drops", checked),
];

/// A case that ended in a way that is no answer.
#[derive(Debug, Clone)]
struct Found {
    case: String,
    step: &'static str,
    end: End,
}

impl std::fmt::Display for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.end {
            End::Panicked(raised) => write!(
                f,
                "{}: {} panicked at {}:{}: {}",
                self.case, self.step, raised.file, raised.line, raised.message
            ),
            End::Endless(message) => write!(f, "{}: {}: {message}", self.case, self.step),
            End::Returned | End::Refused(_) => write!(f, "{}: {}", self.case, self.step),
        }
    }
}

/// What a drill made of one case.
#[derive(Debug, Default)]
struct Drilled {
    /// How each step ended, up to the first that ended in no answer. It
    /// is empty where the damage could not be made: that is no case.
    ends: Vec<End>,
    /// What the checker said in each of its steps: whether the store is
    /// sound.
    sound: Vec<bool>,
    found: Option<Found>,
}

/// Plants a store, damages it with `damage` and runs the drill. It stops
/// at the first step that ends in no answer.
fn drilled(case: &str, damage: impl FnOnce(&Planted) -> bool) -> Drilled {
    let (planted, model) = plant_with_its_model();
    planted.db.unchecked();
    let mut made = false;
    let damaged = ended(|| made = damage(&planted));
    if !made || !matches!(damaged, End::Returned) {
        return Drilled::default();
    }
    let mut world = World::new(&planted.db, &planted.versions);
    world.model = model;
    SOUND.with(|sound| sound.borrow_mut().clear());
    let mut drilled = Drilled::default();
    for (name, step) in DRILL {
        let end = ended(|| step(&planted, &mut world));
        drilled.ends.push(end.clone());
        if end.is_a_finding() {
            let case = case.to_owned();
            drilled.found = Some(Found {
                case,
                step: name,
                end,
            });
            break;
        }
    }
    drilled.sound = SOUND.with(|sound| sound.borrow_mut().split_off(0));
    drilled
}

/// [`drilled`] on a thread of its own, with a limit to how long it may
/// take. A case that takes longer is said not to return: its thread is
/// left behind, and the test process ends with it.
fn drilled_in_time(
    case: String,
    damage: impl FnOnce(&Planted) -> bool + Send + 'static,
) -> Drilled {
    let (done, wait) = mpsc::channel();
    let name = case.clone();
    let worker = thread::Builder::new().name(case.clone());
    let worker = worker.spawn(move || {
        let _ = done.send(drilled(&name, damage));
    });
    worker.expect("a thread");
    match wait.recv_timeout(CASE_LIMIT) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the thread of the case {case:?} ended outside a step of its drill")
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let end = End::Endless(format!("no return within {CASE_LIMIT:?}"));
            let step = "a step of the drill";
            Drilled {
                found: Some(Found { case, step, end }),
                ..Drilled::default()
            }
        }
    }
}

/// On a sound store every step of the drill returns, and the checker
/// calls the store sound each time it is asked: no fixture of the drill
/// refuses anything and the drill does no damage of its own, so a step
/// that is refused on a damaged store is refused for the damage.
#[test]
fn every_step_of_the_drill_returns_on_a_sound_store() {
    let drilled = drilled_in_time("a sound store".to_owned(), |_| true);
    assert!(drilled.found.is_none(), "{:?}", drilled.found);
    assert_eq!(drilled.ends.len(), DRILL.len());
    for ((name, _), end) in DRILL.iter().zip(&drilled.ends) {
        assert!(matches!(end, End::Returned), "{name}: {end:?}");
    }
    let checks = DRILL
        .iter()
        .filter(|(name, _)| name.starts_with("the checker"));
    assert_eq!(drilled.sound, vec![true; checks.count()]);
    assert_eq!(drilled.sound.len(), 3);
}

/// A panic of the engine's code is told from one of a fixture by its
/// file, and a walk that takes too many steps by its message.
#[test]
fn a_panic_is_told_by_where_it_was_raised() {
    let end = ended(|| panic!("a fixture's expectation"));
    assert!(matches!(end, End::Refused(_)), "{end:?}");
    let end = ended(|| {
        let mut steps = WALK_LIMIT;
        counted(&mut steps, "a walk of the test");
    });
    assert!(matches!(end, End::Endless(_)), "{end:?}");
    assert!(matches!(ended(|| ()), End::Returned));
    assert!(of_the_tests(
        "lignindb/src/verify/tests/injected/hostile.rs"
    ));
    assert!(of_the_tests("lignin-tree/src/flat/tests.rs"));
    assert!(!of_the_tests("lignin-tree/src/flat/tree.rs"));
    assert!(!of_the_tests("lignindb/src/txn/txn.rs"));
}

/// What the cases of one test came to.
#[derive(Default)]
struct Tally {
    cases: usize,
    /// The cases in which the checker's first step called the store
    /// unsound: the damage is one the checker reports.
    reported: usize,
    /// The steps that a fixture refused, by the place of its expectation.
    refused: BTreeMap<String, usize>,
    found: Vec<Found>,
}

impl Tally {
    fn take(&mut self, drilled: Drilled) {
        self.cases += usize::from(!drilled.ends.is_empty());
        self.reported += usize::from(drilled.sound.first() == Some(&false));
        for end in &drilled.ends {
            if let End::Refused(raised) = end {
                let at = format!("{}:{}", raised.file, raised.line);
                *self.refused.entry(at).or_default() += 1;
            }
        }
        self.found.extend(drilled.found);
    }

    /// Prints the tally and requires that no case ended in no answer.
    fn close(self) {
        println!(
            "{} cases, {} that the checker reported, {} steps that a fixture refused, {} findings",
            self.cases,
            self.reported,
            self.refused.values().sum::<usize>(),
            self.found.len()
        );
        for (at, steps) in &self.refused {
            println!("REFUSED at {at}: {steps} steps");
        }
        for one in &self.found {
            println!("FOUND {one}");
        }
        assert!(self.found.is_empty(), "{} findings", self.found.len());
    }
}

/// Every corruption of the checker's lists, and the drill on each.
#[test]
fn the_engine_answers_on_every_corruption_of_the_lists() {
    let lists = [
        CELLS,
        VALUES,
        DIRECTORY,
        FLAT_TREES,
        BESIDE_THE_PAGES,
        WHAT_IS_LEFT,
    ];
    let mut tally = Tally::default();
    for injection in lists.into_iter().flatten() {
        let inject = injection.inject;
        tally.take(drilled_in_time(injection.name.to_owned(), move |planted| {
            inject(planted);
            true
        }));
    }
    tally.close();
}

/// The cells of the Merk `name` with the cell above each, the root first.
/// The walk is made before any damage.
fn with_parents(planted: &Planted, name: u8) -> Vec<(CellAddress, Option<CellAddress>)> {
    let (mut cells, mut next) = (Vec::new(), vec![(planted.root(name), None)]);
    while let Some((at, parent)) = next.pop() {
        cells.push((at, parent));
        let children = planted.read(at, |cell| {
            let links = Side::BOTH.into_iter().filter_map(|side| cell.link(side));
            links.map(|link| link.child).collect::<Vec<_>>()
        });
        next.extend(children.into_iter().map(|child| (child, Some(at))));
    }
    cells
}

/// A link of the Merk `name` that is made to name `to`: the link of the
/// `nth` cell that has one, counted from the root.
fn relink(
    planted: &Planted,
    name: u8,
    nth: usize,
    to: impl Fn(CellAddress, Option<CellAddress>, CellAddress) -> CellAddress,
) -> bool {
    let cells = with_parents(planted, name);
    let root = cells[0].0;
    let linked: Vec<_> = (cells.iter())
        .filter_map(|(at, parent)| {
            let side = planted.read(*at, |cell| {
                Side::BOTH.into_iter().find(|side| cell.has_link(*side))
            });
            side.map(|side| (*at, *parent, side))
        })
        .collect();
    let Some((at, parent, side)) = linked.into_iter().nth(nth) else {
        return false;
    };
    let to = to(at, parent, root);
    planted.step_tree(at, |page| {
        page.set_child(at.slot(), side, to).expect("an address");
    });
    true
}

/// Links that lead back: to the cell they are in, to the cell above it and
/// to the root of the Merk, near the root and far from it, in a Merk over
/// many pages and in one of a few nodes; and flat branches that name
/// themselves or the page above them.
#[test]
fn the_engine_answers_where_links_lead_back() {
    type To = fn(CellAddress, Option<CellAddress>, CellAddress) -> CellAddress;
    let targets: [(&str, To); 3] = [
        ("its own cell", |at, _, _| at),
        ("the cell above", |at, parent, _| parent.unwrap_or(at)),
        ("the root", |_, _, root| root),
    ];
    let mut tally = Tally::default();
    for name in [BIG, OUTER, INNER] {
        for nth in [0, 1, 5, 40, 150] {
            for (what, to) in targets {
                let case = format!("link {nth} of the Merk {name} names {what}");
                tally.take(drilled_in_time(case, move |planted| {
                    relink(planted, name, nth, to)
                }));
            }
        }
    }
    let branches: [BranchChange; 3] = [
        ("a flat branch names itself first", |branch, children| {
            children[0] = branch;
        }),
        ("a flat branch names itself last", |branch, children| {
            *children.last_mut().expect("a child") = branch;
        }),
        ("a flat branch names one leaf throughout", |_, children| {
            let first = children[0];
            children.iter_mut().for_each(|child| *child = first);
        }),
    ];
    for (case, change) in branches {
        tally.take(drilled_in_time(case.to_owned(), move |planted| {
            let (branch, _) = planted.branch();
            planted.rebuild_branch(branch, |children, _| change(branch, children));
            true
        }));
    }
    tally.close();
}

/// Whether a load takes `image` as the page `page`: the checks of
/// `lignin_store::load` on a base image, made on a frame.
fn loads(image: &[u8], page: PageId) -> bool {
    let is_flat = FlatPageKind::from_code(image[0], FRAME_FORMAT).is_ok();
    if is_flat {
        return FlatPage::decode(image, FRAME_FORMAT).is_ok_and(|flat| flat.id() == page);
    }
    TreePage::from_validated(image, FRAME_FORMAT)
        .is_ok_and(|tree| tree.id() == page && tree.check(SIZE).is_ok())
}

/// One change of a few bytes of `image`, of a kind `rng` chooses: a byte
/// set, a bit turned, the id of another page of the store or a small
/// number written over two or four bytes, or a few bytes copied from
/// another place of the page.
fn change(image: &mut [u8], used: usize, pages: &[PageId], rng: &mut Rng) {
    let at = rng.below(used);
    match rng.below(7) {
        0 => image[at] = rng.next() as u8,
        1 => image[at] ^= 1 << rng.below(8),
        2 => image[at] = image[at].wrapping_add(1),
        3 => {
            let id = pages[rng.below(pages.len())].get().to_le_bytes();
            let end = (at + 4).min(image.len());
            image[at..end].copy_from_slice(&id[..end - at]);
        }
        4 => {
            let small = (rng.below(48) as u16).to_le_bytes();
            let end = (at + 2).min(image.len());
            image[at..end].copy_from_slice(&small[..end - at]);
        }
        5 => {
            let (from, len) = (rng.below(used), 1 + rng.below(8));
            for offset in 0..len {
                if at + offset < image.len() && from + offset < image.len() {
                    image[at + offset] = image[from + offset];
                }
            }
        }
        _ => image[at] = 0,
    }
}

/// Damages one page of `planted` at random, with the changes `seed`
/// gives, and says whether it did: the page is one a load takes, and
/// differs from what it was.
fn damage_at_random(planted: &Planted, seed: u64) -> bool {
    let mut rng = Rng::new(seed);
    let pages = planted.pages();
    for _ in 0..200 {
        let page = pages[rng.below(pages.len())];
        let before = planted.frame(page).to_vec();
        // The bytes in use end where only zeros follow.
        let used = before
            .iter()
            .rposition(|byte| *byte != 0)
            .map_or(1, |last| last + 1);
        let mut after = before.clone();
        for _ in 0..1 + rng.below(3) {
            change(&mut after, used, &pages, &mut rng);
        }
        if after != before && loads(&after, page) {
            planted.step(page, |bytes| bytes.copy_from_slice(&after));
            return true;
        }
    }
    false
}

/// The seeds of [`the_engine_answers_on_pages_damaged_at_random`]: the
/// range `LIGNIN_HOSTILE_SEEDS` names as `first..end`, or a short one.
fn seeds() -> std::ops::Range<u64> {
    let Ok(named) = std::env::var("LIGNIN_HOSTILE_SEEDS") else {
        return 0..24;
    };
    let (first, end) = named.split_once("..").expect("first..end");
    first.parse().expect("a seed")..end.parse().expect("a seed")
}

/// Stores with one page damaged at random, and the drill on each. Each
/// seed is printed before its case runs, so that a process that is ended
/// from outside has named the case it was in.
#[test]
fn the_engine_answers_on_pages_damaged_at_random() {
    let mut tally = Tally::default();
    for seed in seeds() {
        println!("SEED {seed}");
        let case = format!("seed {seed}");
        tally.take(drilled_in_time(case, move |planted| {
            damage_at_random(planted, seed)
        }));
    }
    tally.close();
}
