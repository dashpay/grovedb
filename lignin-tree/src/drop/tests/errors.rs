//! What is refused, and what a failure leaves.

use std::{error::Error, sync::atomic::Ordering};

use lignin_format::{
    tree_page::{CellContent, CellValue, Hash, TreeLink},
    value_log::ValuePointer,
    CellAddress, FileId, FormatError, PageId, SlotId,
};
use lignin_store::{alloc::PageState, load::LoadError};

use super::*;
use crate::{
    directory::{DirectoryError, Host, Root, RootChange},
    flat::{FlatError, FlatTree},
    fragment::{Fragments, Layout, LayoutParams, NodeWrite, ValueWrite, Write},
};

const HASH: Hash = [3; 32];

fn page_id(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("not zero")
}

fn fill(grove: &mut Grove, merk: usize, count: usize, from: usize) -> Vec<usize> {
    (0..count)
        .map(|n| {
            let at = from + (n * 7919) % count;
            let value = match at % 6 {
                0 => bytes(at as u64, 290),
                _ => bytes(at as u64, 18),
            };
            grove.put(merk, format!("k{at:05}").as_bytes(), &value)
        })
        .collect()
}

/// A grove with a Merk of `inside` nodes below a host, some of them hosts
/// of Merks in turn, dropped and ready to be reclaimed.
fn dropped(inside: usize) -> (Db, Grove, Directory, Dropped) {
    let db = Db::new();
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    fill(&mut grove, 0, 25, 0);
    let host = grove.put(0, b"host", b"a tree");
    let merk = grove.host(host);
    grove.flush(&mut txn);
    db.commit(txn);
    for chunk in 0..inside.div_ceil(60) {
        let mut txn = db.begin();
        let nodes = fill(
            &mut grove,
            merk,
            60.min(inside - chunk * 60),
            1_000 + chunk * 60,
        );
        for &node in nodes.iter().step_by(25) {
            let nested = grove.host(node);
            fill(&mut grove, nested, 3, 0);
        }
        grove.flush(&mut txn);
        db.commit(txn);
    }
    db.write_back();
    let mut txn = db.begin();
    grove.drop_merks(&mut txn, &[merk], false);
    db.commit(txn);
    db.tick();
    let directory = grove.directory(&db);
    let [left] = drops(&db, &directory)[..] else {
        panic!("one Merk was dropped");
    };
    (db, grove, directory, left)
}

#[test]
fn every_error_says_what_it_is() {
    let page = page_id(7);
    let source = || -> OwnersError { "the store said no".into() };
    let prefix = [9; 32];
    let errors = [
        (
            DropError::from(DirectoryError::NotFound { prefix }),
            "subtree drop: no Merk is entered under the prefix 09090909..",
            true,
        ),
        (
            DropError::from(FlatError::Freed { page }),
            "subtree drop: ",
            true,
        ),
        (
            DropError::from(FragmentError::NoRoom { page: None }),
            "subtree drop: a new page is above its capacity",
            true,
        ),
        (
            DropError::from(FormatError::MissingId),
            "subtree drop: ",
            true,
        ),
        (
            DropError::from(LoadError::NoPage {
                page,
                at: version(3),
                freed: None,
            }),
            "subtree drop: ",
            true,
        ),
        (
            DropError::Store(source()),
            "subtree drop: the store said no",
            true,
        ),
        (
            DropError::Broken {
                page,
                source: source(),
            },
            "the step on page 7 is in the log and was not made: the store said no",
            true,
        ),
        (
            DropError::Damaged {
                page,
                what: "a cell that two pointers name",
            },
            "what a dropped subtree left in page 7: a cell that two pointers name",
            false,
        ),
        (
            DropError::Record("a version that is none"),
            "record of unlinked pages: a version that is none",
            false,
        ),
        (
            DropError::Below { prefix },
            "the Merk under the prefix 09090909.. is dropped with a Merk it is below",
            false,
        ),
        (
            DropError::IdsExhausted,
            "every id of a record of unlinked pages was given out",
            false,
        ),
        (
            DropError::GivenUp,
            "reclaiming was given up after a step that broke",
            false,
        ),
    ];
    for (error, says, has_source) in errors {
        let said = error.to_string();
        assert!(said.starts_with(says), "{said}");
        assert_eq!(error.source().is_some(), has_source, "{said}");
        assert!(!format!("{error:?}").is_empty());
    }
}

/// A drop that is refused has changed no page: a Merk that is not entered,
/// one that is named twice, one with no node that has Merks below it, and
/// a write that the tree pages refuse.
#[test]
fn a_drop_that_is_refused_changes_nothing() {
    let db = Db::new();
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    let host = grove.put(0, b"host", b"a tree");
    let merk = grove.host(host);
    fill(&mut grove, merk, 12, 0);
    grove.flush(&mut txn);
    // A Merk with no node, and one that the directory has below it.
    let (empty, below) = ([0x11; 32], [0x12; 32]);
    let root_prefix = grove.merks[0].prefix;
    let directory = &mut grove.directory;
    let above = Host {
        parent: &root_prefix,
        key: b"empty",
        position: 0,
    };
    directory
        .add(&mut txn.flat, &empty, b"tree", Some(above))
        .expect("entered");
    let inside = Host {
        parent: &empty,
        key: b"a node that is not there",
        position: 0,
    };
    directory
        .add(&mut txn.flat, &below, b"tree", Some(inside))
        .expect("entered");
    db.commit(txn);
    let cells = db.cells();

    let prefix = grove.merks[merk].prefix;
    let unknown = [0x77; 32];
    let layout = Layout::new(db.size, LayoutParams::default()).expect("a layout");
    let refused = |write: &Write<'_>, leaving: &[Leaving<'_>]| {
        let mut txn = db.begin();
        let mut directory = grove.directory(&db);
        let at = txn.tree.at;
        let error = unlink(
            &mut Fragments::new(layout),
            &mut txn.tree,
            &mut txn.values,
            &mut directory,
            &mut Flats {
                pages: &mut txn.flat,
                columns: &grove.columns,
                drops: &grove.drops,
                at,
            },
            write,
            leaving,
        )
        .expect_err("refused");
        assert!(txn.tree.changed().is_empty());
        assert_eq!(txn.flat.written().count(), 0);
        db.roll_back(txn);
        error
    };
    let none = Write::default();
    let error = refused(&none, &[Leaving::Dropped(&unknown)]);
    assert!(matches!(
        error,
        DropError::Directory(DirectoryError::NotFound { prefix }) if prefix == unknown
    ));
    let twice = [Leaving::Dropped(&prefix), Leaving::Cleared(&prefix)];
    assert_eq!(twice[0].prefix(), twice[1].prefix());
    let error = refused(&none, &twice);
    assert!(matches!(
        error,
        DropError::Directory(DirectoryError::Twice { .. })
    ));
    let error = refused(&none, &[Leaving::Dropped(&empty)]);
    assert!(matches!(
        error,
        DropError::Directory(DirectoryError::NotEmpty { .. })
    ));
    // A Merk with the Merk it is below, in either order: the outer one
    // takes the inner one along.
    for both in [
        [Leaving::Cleared(&empty), Leaving::Dropped(&below)],
        [Leaving::Dropped(&below), Leaving::Cleared(&empty)],
    ] {
        let error = refused(&none, &both);
        assert!(matches!(error, DropError::Below { prefix } if prefix == below));
    }

    // A write that names a node at an address where no cell is.
    let nowhere = NodeWrite {
        at: Some(CellAddress::new(page_id(2), slot(999))),
        key: b"nothing",
        feature: &[0],
        value_hash: &HASH,
        kv_hash: &HASH,
        left: None,
        right: None,
        hosted: crate::fragment::HostedWrite::Keep,
        value: ValueWrite::Keep,
    };
    let nodes = [nowhere];
    let write = Write {
        nodes: &nodes,
        ..Write::default()
    };
    let error = refused(&write, &[Leaving::Dropped(&prefix)]);
    assert!(matches!(error, DropError::Fragment(_)), "{error}");

    // A Merk with no node is cleared, and dropped once nothing is below.
    let mut txn = db.begin();
    let mut directory = grove.directory(&db);
    let at = txn.tree.at;
    let cleared = unlink(
        &mut Fragments::new(layout),
        &mut txn.tree,
        &mut txn.values,
        &mut directory,
        &mut Flats {
            pages: &mut txn.flat,
            columns: &grove.columns,
            drops: &grove.drops,
            at,
        },
        &none,
        &[Leaving::Cleared(&empty)],
    )
    .expect("cleared");
    assert!(cleared.left.is_empty() && cleared.report.nodes.is_empty());
    db.roll_back(txn);
    assert_eq!(db.cells(), cells);
    grove.check(&db);
}

/// A clear and a drop of a Merk with no node read the first Merk that
/// the directory has below it, whole: a record of it that is damaged
/// refuses both, and nothing is changed.
#[test]
fn a_clear_of_a_merk_with_no_node_reads_the_merk_below_it_as_a_drop_does() {
    let db = Db::new();
    let mut txn = db.begin();
    let mut grove = Grove::new(&db, &mut txn);
    grove.flush(&mut txn);
    let (empty, below) = ([0x11; 32], [0x12; 32]);
    let root_prefix = grove.merks[0].prefix;
    let directory = &mut grove.directory;
    let above = Host {
        parent: &root_prefix,
        key: b"empty",
        position: 0,
    };
    let id = directory
        .add(&mut txn.flat, &empty, b"tree", Some(above))
        .expect("entered");
    let inside = Host {
        parent: &empty,
        key: b"inside",
        position: 0,
    };
    directory
        .add(&mut txn.flat, &below, b"tree", Some(inside))
        .expect("entered");
    // The record of the host of the Merk below holds no prefix and id.
    let key = crate::directory::record::child_key(id, 0, b"inside");
    let tree = *directory.tree();
    tree.put(&mut txn.flat, &key, b"no record")
        .expect("a record");
    db.commit(txn);

    let layout = Layout::new(db.size, LayoutParams::default()).expect("a layout");
    for leaving in [Leaving::Cleared(&empty), Leaving::Dropped(&empty)] {
        let mut txn = db.begin();
        let mut directory = grove.directory(&db);
        let at = txn.tree.at;
        let error = unlink(
            &mut Fragments::new(layout),
            &mut txn.tree,
            &mut txn.values,
            &mut directory,
            &mut Flats {
                pages: &mut txn.flat,
                columns: &grove.columns,
                drops: &grove.drops,
                at,
            },
            &Write::default(),
            &[leaving],
        )
        .expect_err("refused");
        assert!(
            matches!(
                &error,
                DropError::Directory(DirectoryError::Damaged { key: at, .. }) if *at == key
            ),
            "{error}"
        );
        assert!(txn.tree.changed().is_empty());
        assert_eq!(txn.flat.written().count(), 0);
        db.roll_back(txn);
    }
}

/// Pages with cells as these tests give them, and a dropped Merk whose
/// root is a cell of the first.
struct Crafted {
    db: Db,
    directory: Directory,
    dropped: Dropped,
    page: PageId,
    pages: Vec<PageId>,
    cells: usize,
}

struct Cell {
    left: Option<u16>,
    right: Option<u16>,
    fragment_root: bool,
    pointer: Option<u64>,
    /// The cells of other pages that the cell names, by the number of the
    /// page among those made: its left child, and its right one if there
    /// are two.
    far: &'static [(usize, u16)],
}

const LEAF: Cell = Cell {
    left: None,
    right: None,
    fragment_root: false,
    pointer: None,
    far: &[],
};

fn crafted(cells: &[Cell], root: u16) -> Crafted {
    crafted_pages(&[cells], root)
}

fn crafted_pages(pages: &[&[Cell]], root: u16) -> Crafted {
    let db = Db::new();
    let mut txn = db.begin();
    let mut directory = Directory::create(&mut txn.flat, db.config).expect("a directory");
    let (grove, merk) = ([1; 32], [2; 32]);
    directory
        .add(&mut txn.flat, &grove, b"tree", None)
        .expect("entered");
    let host = Host {
        parent: &grove,
        key: b"host",
        position: 0,
    };
    directory
        .add(&mut txn.flat, &merk, b"tree", Some(host))
        .expect("entered");
    let made: Vec<PageId> = pages
        .iter()
        .map(|_| txn.tree.create().expect("a page"))
        .collect();
    let page = made[0];
    for (&page, cells) in made.iter().zip(pages) {
        let link = |child: CellAddress| TreeLink {
            child,
            hash: &HASH,
            heights: (0, 0),
            child_key_len: 4,
            aggregate: &[],
        };
        let near = |to: Option<u16>| to.map(|to| link(CellAddress::new(page, slot(to))));
        let far = |to: Option<&(usize, u16)>| {
            to.map(|&(page, to)| link(CellAddress::new(made[page], slot(to))))
        };
        txn.tree
            .write(page, |nodes| {
                for (n, cell) in cells.iter().enumerate() {
                    let key = format!("c{n:03}");
                    let pointer = cell.pointer.map(|offset| {
                        ValuePointer::new(FileId::new(900).expect("a file"), offset, 300)
                            .expect("a pointer")
                    });
                    nodes.insert(&CellContent {
                        key: key.as_bytes(),
                        feature: &[0],
                        value_hash: &HASH,
                        kv_hash: &HASH,
                        left: far(cell.far.first()).or(near(cell.left)),
                        right: far(cell.far.get(1)).or(near(cell.right)),
                        hosted_roots: &[],
                        value: match pointer {
                            Some(pointer) => CellValue::OutOfLine(pointer),
                            None => CellValue::Inline(b"a value"),
                        },
                        fragment_root: cell.fragment_root,
                    })?;
                }
                Ok(())
            })
            .expect("the cells");
    }
    let named = RootChange::Named {
        prefix: &merk,
        root: Some(Root {
            address: CellAddress::new(page, slot(root)),
            key: b"c000",
        }),
    };
    directory
        .set_roots(&mut txn.flat, &[named])
        .expect("the root");
    let dropped = directory.unlink(&mut txn.flat, &merk).expect("dropped");
    db.commit(txn);
    db.tick();
    let directory = Directory::load(&db.reader(), directory.root_page(), db.config);
    Crafted {
        db,
        directory: directory.expect("a directory"),
        dropped,
        page,
        pages: made,
        cells: pages.iter().map(|cells| cells.len()).sum(),
    }
}

/// Pages that are not what a drop leaves are not changed, and the Merk
/// keeps its record.
#[test]
fn a_page_that_is_not_what_a_drop_leaves_is_not_changed() {
    let root = |left, right| Cell {
        left,
        right,
        fragment_root: true,
        ..LEAF
    };
    let marked = Cell {
        fragment_root: true,
        ..LEAF
    };
    let holds = |offset| Cell {
        pointer: Some(offset),
        ..LEAF
    };
    let cases: Vec<(Vec<Cell>, u16, &str)> = vec![
        (vec![root(None, None)], 5, "no cell where a pointer"),
        (vec![LEAF], 1, "is not marked as the root of its fragment"),
        (
            vec![root(Some(9), None)],
            1,
            "names a free slot of its page",
        ),
        (
            vec![root(Some(2), None), marked],
            1,
            "is marked as the root of a fragment",
        ),
        (
            vec![root(Some(2), Some(2)), LEAF],
            1,
            "a cell that two pointers name",
        ),
        (
            vec![root(Some(2), None), LEAF, root(Some(2), None)],
            1,
            "a cell that stays names a cell of the subtree",
        ),
        (
            vec![root(Some(2), Some(3)), holds(64), holds(64)],
            1,
            "one value pointer that two hold",
        ),
    ];
    for (cells, root, says) in cases {
        let made = crafted(&cells, root);
        let reclaimer = made.db.reclaimer();
        let mut reclaim = Reclaim::new(made.dropped);
        for _ in 0..2 {
            let refused = reclaimer.reclaim(
                &mut reclaim,
                &made.directory,
                &made.db.columns(),
                &GUARD,
                usize::MAX,
            );
            let error = refused.expect_err("a page that is damaged");
            assert!(matches!(error, DropError::Damaged { page, .. } if page == made.page));
            assert!(error.to_string().contains(says), "{error}");
        }
        assert!(!reclaim.holds_cells() && reclaim.reclaimed().records == 0);
        assert_eq!(drops(&made.db, &made.directory), [made.dropped]);
        assert_eq!(made.db.cells(), made.cells);
    }

    // The root is said to be where there is no page, and in a flat page.
    let made = crafted(&[root(None, None)], 1);
    let reclaimer = made.db.reclaimer();
    let flat = made.directory.root_page();
    for (page, says) in [
        (page_id(4_000), "no page where"),
        (flat, "no tree page where"),
    ] {
        let moved = Dropped {
            root: Some(CellAddress::new(page, slot(1))),
            ..made.dropped
        };
        let mut reclaim = Reclaim::new(moved);
        let refused = reclaimer.reclaim(
            &mut reclaim,
            &made.directory,
            &made.db.columns(),
            &GUARD,
            usize::MAX,
        );
        let error = refused.expect_err("no such root");
        assert!(error.to_string().contains(says), "{error}");
    }
    assert_eq!(drops(&made.db, &made.directory), [made.dropped]);
    // A drop that the directory has no record of.
    let unknown = Dropped {
        id: crate::directory::MerkId::new(77).expect("an id"),
        root: None,
    };
    let refused = reclaimer.reclaim(
        &mut Reclaim::new(unknown),
        &made.directory,
        &made.db.columns(),
        &GUARD,
        1,
    );
    assert!(matches!(
        refused,
        Err(DropError::Directory(DirectoryError::NotDropped { .. }))
    ));
}

/// A cell that cells of two pages name is not what a write leaves. It is
/// taken out once, as the first of them named it, and the second time is
/// an error and no step: by then its slot could hold another cell.
#[test]
fn a_cell_that_two_pages_name_is_taken_out_once() {
    let marked = |far| Cell {
        fragment_root: true,
        far,
        ..LEAF
    };
    let pages: [&[Cell]; 3] = [
        &[marked(&[(1, 1), (2, 1)])],
        &[marked(&[(2, 1)])],
        &[marked(&[])],
    ];
    let made = crafted_pages(&pages, 1);
    assert_eq!(made.db.cells(), made.cells);
    let reclaimer = made.db.reclaimer();
    let mut reclaim = Reclaim::new(made.dropped);
    let columns = made.db.columns();
    let mut refused = Vec::new();
    for _ in 0..8 {
        match reclaimer.reclaim(&mut reclaim, &made.directory, &columns, &GUARD, usize::MAX) {
            Ok(Progress::Done) => break,
            Ok(progress) => panic!("{progress:?}"),
            Err(error) => refused.push(error),
        }
    }
    let [DropError::Damaged { page, what }] = &refused[..] else {
        panic!("{refused:?}");
    };
    assert_eq!(*page, made.pages[2]);
    assert!(what.contains("two cells of the subtree name"), "{what}");
    // The three cells went, each once, with their pages.
    assert!(reclaim.is_done());
    assert_eq!(reclaim.reclaimed().cells, 3);
    assert_eq!(reclaim.reclaimed().pages_freed, 3);
    assert_eq!(made.db.cells(), 0);
    let steps = made.db.reclaims();
    let freed = steps.iter().filter(|step| step.1.is_some());
    let freed: Vec<PageId> = freed.map(|step| step.0).collect();
    let mut once = freed.clone();
    once.sort();
    once.dedup();
    assert_eq!((freed.len(), once), (3, made.pages.clone()));
}

/// A page in the middle of the walk that is not what a drop leaves stays
/// as it is, with what hangs below it, and the other pages are reclaimed.
#[test]
fn a_damaged_page_does_not_keep_the_other_pages_from_going() {
    let (db, grove, directory, left) = dropped(600);
    // A cell of the subtree in a page of its own loses its mark, as if the
    // page had been written by something else.
    let root_page = left.root.expect("a root").page();
    let page = |node: &super::grove::Node| node.at.expect("a cell").page();
    let own: Vec<PageId> = {
        let in_use: Vec<PageId> = grove.nodes.iter().filter(|n| n.alive).map(page).collect();
        let mut pages: Vec<PageId> = grove
            .nodes
            .iter()
            .filter(|node| node.left_behind)
            .map(page)
            .filter(|page| !in_use.contains(page) && *page != root_page)
            .collect();
        pages.sort();
        pages.dedup();
        pages
    };
    let spoiled = own[own.len() / 2];
    let mut txn = db.begin();
    txn.tree
        .write(spoiled, |nodes| {
            let roots: Vec<SlotId> = nodes.view().tree_page().fragment_roots().collect();
            for root in roots {
                let mut node = nodes.node_mut(root).expect("a cell");
                node.set_fragment_root(false)?;
            }
            Ok(())
        })
        .expect("spoiled");
    db.commit(txn);
    db.tick();
    let in_page = tree(&db.current(spoiled).expect("the page")).cell_count();

    let reclaimer = db.reclaimer();
    let mut reclaim = Reclaim::new(left);
    let mut errors = 0;
    for _ in 0..4 {
        match reclaimer.reclaim(&mut reclaim, &directory, &db.columns(), &GUARD, usize::MAX) {
            Err(DropError::Damaged { page, .. }) => {
                assert_eq!(page, spoiled);
                errors += 1;
            }
            Ok(Progress::Blocked(Busy::Newborn)) => drop(db.tick()),
            other => panic!("{other:?}"),
        }
    }
    assert!(errors >= 2 && reclaim.holds_cells() && !reclaim.is_done());
    let taken = reclaim.reclaimed().cells as usize;
    assert!(taken > 0 && taken < grove.left_behind());
    // The page is as it was, and every cell that is left hangs below it.
    assert_eq!(
        tree(&db.current(spoiled).expect("the page")).cell_count(),
        in_page
    );
    assert!(db.cells() >= grove.alive() + in_page);
    assert_eq!(db.cells(), grove.alive() + grove.left_behind() - taken);
    grove.check(&db);
}

/// The log refuses the record of a step, at every point of a reclaiming
/// in turn. Nothing is changed by a step that failed, and the reclaiming
/// goes on from where it was.
#[test]
fn a_step_that_the_log_refuses_changes_nothing_and_is_made_again() {
    let (db, grove, directory, left) = dropped(400);
    let reclaimer = db.reclaimer();
    let mut reclaim = Reclaim::new(left);
    let mut refusals = 0;
    for call in 0.. {
        let fails = call % 3 == 1;
        let before = (db.cells(), reclaim.reclaimed().clone(), db.reclaims().len());
        if fails {
            db.refuse_appends.store(1, Ordering::SeqCst);
        }
        let progress = reclaimer.reclaim(&mut reclaim, &directory, &db.columns(), &GUARD, 1);
        let refused = db.refuse_appends.swap(0, Ordering::SeqCst) == 0 && fails;
        match progress {
            Err(DropError::Store(error)) => {
                assert!(refused && error.to_string() == "the log is full");
                refusals += 1;
                let after = (db.cells(), reclaim.reclaimed().clone(), db.reclaims().len());
                assert_eq!(before, after);
            }
            Ok(Progress::Done) => break,
            Ok(Progress::More) => assert!(!refused),
            Ok(Progress::Blocked(Busy::Newborn)) => drop(db.tick()),
            other => panic!("{other:?}"),
        }
        db.check_pages();
    }
    assert!(refusals > 10);
    assert_eq!(reclaim.reclaimed().cells as usize, grove.left_behind());
    check_reclaimed(&db, &grove);
}

/// The record of a step is in the log, and the store does not take what it
/// says: the id of the page is not one that can be freed. The reclaiming
/// is given up.
#[test]
fn a_step_that_cannot_be_finished_gives_the_reclaiming_up() {
    let (db, grove, directory, left) = dropped(500);
    let page = |node: &super::grove::Node| node.at.expect("a cell").page();
    let in_use: Vec<PageId> = grove.nodes.iter().filter(|n| n.alive).map(page).collect();
    let own = grove
        .nodes
        .iter()
        .filter(|node| node.left_behind)
        .map(page)
        .find(|page| !in_use.contains(page))
        .expect("a page of the subtree alone");
    // Somebody else has retired the id.
    db.ids.free(own, db.latest()).expect("retired");
    assert_eq!(db.state(own), PageState::Retired);

    let reclaimer = db.reclaimer();
    let mut reclaim = Reclaim::new(left);
    let broke = loop {
        match reclaimer.reclaim(&mut reclaim, &directory, &db.columns(), &GUARD, usize::MAX) {
            Ok(Progress::Blocked(Busy::Newborn)) => drop(db.tick()),
            Ok(progress) => panic!("{progress:?}"),
            Err(error) => break error,
        }
    };
    assert!(
        matches!(&broke, DropError::Broken { page, .. } if *page == own),
        "{broke}"
    );
    assert!(!reclaim.is_done());
    for _ in 0..2 {
        let refused =
            reclaimer.reclaim(&mut reclaim, &directory, &db.columns(), &GUARD, usize::MAX);
        assert!(matches!(refused, Err(DropError::GivenUp)));
    }

    // The same for a page that a range delete left.
    let mut txn = db.begin();
    let tree = FlatTree::create(&mut txn.flat, db.config).expect("a tree");
    let records = FlatDrops::create(&mut txn.flat, db.config).expect("the records");
    for n in 0..3_000 {
        let key = format!("key-{n:05}");
        tree.put(&mut txn.flat, key.as_bytes(), &bytes(n, 40))
            .expect("a put");
    }
    db.commit(txn);
    let mut txn = db.begin();
    let at = txn.tree.at;
    records
        .unlink_prefix(&mut txn.flat, &tree, b"", at)
        .expect("unlinked");
    db.commit(txn);
    db.tick();
    let [range] = &reclaimer.flat_drops(&records, &GUARD).expect("the records")[..] else {
        panic!("one range delete was made");
    };
    db.ids
        .free(range.pages[0].page, db.latest())
        .expect("retired");
    let mut done = FlatReclaimed::default();
    let broke = reclaimer.reclaim_flat(&records, range, &GUARD, usize::MAX, &mut done);
    assert!(matches!(broke, Err(DropError::Broken { .. })), "{broke:?}");
}

/// A cell holds a value pointer that the counts of live values do not
/// have. The step that takes the cell out is made, and the reclaiming
/// says that a count did not agree.
#[test]
fn a_count_that_does_not_agree_does_not_stop_the_reclaiming() {
    let cells = [
        Cell {
            left: Some(2),
            fragment_root: true,
            pointer: Some(64),
            ..LEAF
        },
        LEAF,
    ];
    let made = crafted(&cells, 1);
    let reclaimer = made.db.reclaimer();
    assert_eq!(reclaimer.latest(), made.db.latest());
    let mut reclaim = Reclaim::new(made.dropped);
    finish(&made.db, &made.directory, &mut reclaim, usize::MAX);
    assert!(reclaim.is_done() && reclaim.miscount().is_some());
    assert_eq!(
        (reclaim.reclaimed().cells, reclaim.reclaimed().values),
        (2, 1)
    );
    assert_eq!(made.db.cells(), 0);
    assert!(made.db.current(made.page).is_none());
}
