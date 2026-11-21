//! The tests of the invariant checker.
//!
//! [`plant`] makes a store that holds one of everything the checker walks:
//! a Merk over many pages with values out of line, a Merk that an element
//! of another hosts, flat records under short and long keys, and what a
//! dropped Merk left. `injected` damages such a store in one way at a time
//! and asks for the finding; `sound` has the stores that pass.

mod injected;
mod sound;

use std::sync::{atomic::AtomicU64, Arc};

use lignin_format::{
    flat_page::{FlatPage, FlatPageKind},
    tree_page::{TreeCell, TreePage, TreePageMut},
    CellAddress, CommitVersion, PageId,
};
use lignin_store::{
    alloc::PageState,
    cache::PageRef,
    load::{LoadBuffers, FRAME_FORMAT},
};
use lignin_tree::directory::{Directory, Prefix};

use super::{Report, Scope};
use crate::{
    snapshot::Versions,
    txn::{
        tests::{
            model::{self, OwnedBatch, OwnedSubtree},
            put,
            store::{Db, Rules, ENTERED, SIZE},
            World,
        },
        Column, FlatWriter,
    },
};

/// A Merk of 400 nodes, a fifth of them with a value out of line.
pub(super) const BIG: u8 = 1;
/// The subtree whose flat records the store has.
pub(super) const FLAT: u8 = 4;
/// A Merk whose node `host` hosts [`INNER`].
pub(super) const OUTER: u8 = ENTERED + 10;
pub(super) const INNER: u8 = ENTERED + 11;
/// A Merk that was dropped, with [`BELOW`] hosted in it.
pub(super) const GONE: u8 = ENTERED + 12;
pub(super) const BELOW: u8 = ENTERED + 13;
/// A subtree that no store of these tests has entered.
pub(super) const NEVER: u8 = ENTERED + 20;

pub(super) fn prefix(name: u8) -> Prefix {
    [name; 32]
}

pub(super) fn key(index: usize) -> Vec<u8> {
    format!("key{index:04}").into_bytes()
}

/// A store with its versions.
pub(super) struct Planted {
    pub(super) db: Arc<Db>,
    pub(super) versions: Versions,
}

/// Makes every turn of write-back that moves something on: afterwards the
/// pages are in segments and no value is pending.
pub(super) fn write_back(db: &Db, versions: &Versions) {
    let write_back = db.write_back(versions);
    while write_back
        .pass()
        .expect("a pass of write-back")
        .progressed()
    {}
}

/// A store that holds one of everything, written back.
pub(super) fn plant() -> Planted {
    let (db, versions) = Db::new(1024);
    {
        let mut world = World::new(&db, &versions);
        let tx = world.begin();
        world.edit(&tx, BIG, |items| {
            for index in 0..400 {
                let len = if index % 5 == 0 { 400 } else { 30 };
                items.insert(key(index), vec![index as u8; len]);
            }
        });
        world.commit(tx, &[]);

        let tx = world.begin();
        let mut outer = world.merk(&tx, OUTER, |items| {
            items.insert(b"host".to_vec(), b"tree".to_vec());
            items.insert(b"left".to_vec(), vec![1; 40]);
            items.insert(b"right".to_vec(), vec![2; 300]);
        });
        (outer.hosted).insert(b"host".to_vec(), vec![model::subtree(INNER)]);
        let inner = world.merk(&tx, INNER, |items| {
            items.insert(b"one".to_vec(), vec![3; 20]);
            items.insert(b"two".to_vec(), vec![4; 20]);
        });
        let host = (model::subtree(OUTER), b"host".to_vec(), 0);
        let batch = OwnedBatch {
            merks: vec![inner, outer],
            records: vec![
                put(model::unit(Column::Aux, OUTER), b"a", b"1"),
                put(model::unit(Column::Meta, INNER), b"im", b"8"),
            ],
            subtrees: vec![
                OwnedSubtree::Enter(model::subtree(OUTER), None),
                OwnedSubtree::Enter(model::subtree(INNER), Some(host)),
            ],
        };
        world.apply(&tx, &batch).expect("the subtrees are made");
        world.commit(tx, &[]);

        let tx = world.begin();
        let aux = model::unit(Column::Aux, FLAT);
        let mut records = vec![
            put(aux, b"long value", &[7; 3000]),
            put(aux, &[b'k'; 3000], b"under a long key"),
        ];
        for index in 0..300 {
            let meta = model::unit(Column::Meta, FLAT);
            records.push(put(meta, &key(index), &[index as u8; 60]));
        }
        world.write(&tx, records);
        world.commit(tx, &[]);

        let tx = world.begin();
        let mut gone = world.merk(&tx, GONE, |items| {
            for index in 0..40 {
                let len = if index % 4 == 0 { 300 } else { 20 };
                items.insert(key(index), vec![index as u8; len]);
            }
            items.insert(b"host".to_vec(), b"tree".to_vec());
        });
        (gone.hosted).insert(b"host".to_vec(), vec![model::subtree(BELOW)]);
        let below = world.merk(&tx, BELOW, |items| {
            items.insert(b"x".to_vec(), vec![5; 20]);
            items.insert(b"y".to_vec(), vec![6; 300]);
        });
        let host = (model::subtree(GONE), b"host".to_vec(), 0);
        let batch = OwnedBatch {
            merks: vec![below, gone],
            records: vec![put(model::unit(Column::Aux, BELOW), b"left", b"over")],
            subtrees: vec![
                OwnedSubtree::Enter(model::subtree(GONE), None),
                OwnedSubtree::Enter(model::subtree(BELOW), Some(host)),
            ],
        };
        world.apply(&tx, &batch).expect("the subtrees are made");
        world.commit(tx, &[]);

        let tx = world.begin();
        let batch = OwnedBatch {
            subtrees: vec![OwnedSubtree::Drop(model::subtree(GONE))],
            ..OwnedBatch::default()
        };
        world.apply(&tx, &batch).expect("the subtree is dropped");
        world.commit(tx, &[]);
    }
    write_back(&db, &versions);
    Planted { db, versions }
}

/// Drops the subtree `name` of `store` in a commit of its own.
pub(super) fn drop_subtree(store: &Planted, name: u8) {
    let mut world = World::new(&store.db, &store.versions);
    let tx = world.begin();
    let batch = OwnedBatch {
        subtrees: vec![OwnedSubtree::Drop(model::subtree(name))],
        ..OwnedBatch::default()
    };
    world.apply(&tx, &batch).expect("the subtree is dropped");
    world.commit(tx, &[]);
}

impl Planted {
    pub(super) fn latest(&self) -> CommitVersion {
        self.versions.latest()
    }

    /// The checker's report on the latest version of the store, which is
    /// still, with the hash rules of the tests' model.
    pub(super) fn check(&self) -> Report {
        let calls = AtomicU64::new(0);
        let rules = &mut Rules(&calls);
        (self.db).verify(self.latest(), Scope::Still, Some(rules))
    }

    /// The page `page` of the latest version, pinned.
    pub(super) fn frame(&self, page: PageId) -> PageRef<'_> {
        let mut buffers = LoadBuffers::new();
        (self.db.loader().get(page, self.latest(), &mut buffers)).expect("the page is read")
    }

    /// The pages of the latest version: every live id.
    pub(super) fn pages(&self) -> Vec<PageId> {
        let (mut from, mut states, mut pages) = (Some(PageId::MIN), Vec::new(), Vec::new());
        while let Some(page) = from {
            states.clear();
            from = self.db.ids.scan(page, &mut states);
            let live = states.iter().filter(|(_, state)| *state == PageState::Live);
            pages.extend(live.map(|(page, _)| *page));
        }
        pages
    }

    pub(super) fn tree_pages(&self) -> Vec<PageId> {
        let is_tree = |page: &PageId| self.frame(*page)[0] == TreePage::KIND_CODE;
        self.pages().into_iter().filter(is_tree).collect()
    }

    pub(super) fn flat_pages(&self) -> Vec<PageId> {
        let is_flat =
            |page: &PageId| FlatPageKind::from_code(self.frame(*page)[0], FRAME_FORMAT).is_ok();
        self.pages().into_iter().filter(is_flat).collect()
    }

    /// The first cell, in the order of the addresses, that `wanted` takes.
    pub(super) fn cell(&self, wanted: impl Fn(CellAddress, &TreeCell<'_>) -> bool) -> CellAddress {
        self.cells(wanted)
            .into_iter()
            .next()
            .expect("the store has such a cell")
    }

    /// Every cell that `wanted` takes, in the order of the addresses.
    pub(super) fn cells(
        &self,
        wanted: impl Fn(CellAddress, &TreeCell<'_>) -> bool,
    ) -> Vec<CellAddress> {
        let mut found = Vec::new();
        for id in self.tree_pages() {
            let frame = self.frame(id);
            let page = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
            for (slot, cell) in page.cells() {
                let address = CellAddress::new(id, slot);
                if wanted(address, &cell) {
                    found.push(address);
                }
            }
        }
        found
    }

    /// What `read` makes of the cell at `address`.
    pub(super) fn read<R>(&self, address: CellAddress, read: impl FnOnce(&TreeCell<'_>) -> R) -> R {
        let frame = self.frame(address.page());
        let page = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
        read(&page.cell(address.slot()).expect("a cell"))
    }

    /// Puts a changed copy of the page `page` in place of its current
    /// version, as a page step does (`transactions.md` PV7): the damage a
    /// test does to a page.
    pub(super) fn step(&self, page: PageId, change: impl FnOnce(&mut [u8])) {
        let _held = self.frame(page);
        let step = self.db.pool.page_step(page);
        let current = step.current().expect("the page is in the pool");
        let mut changed = self.db.pool.blank();
        changed.copy_from(&current);
        change(&mut changed);
        step.replace(&current, changed).expect("the step is made");
    }

    /// A page step on the tree page of the cell at `address`.
    pub(super) fn step_tree(
        &self,
        address: CellAddress,
        change: impl FnOnce(&mut TreePageMut<'_>),
    ) {
        self.step(address.page(), |bytes| {
            let mut page = TreePageMut::open(bytes, SIZE, FRAME_FORMAT).expect("a tree page");
            change(&mut page);
        });
    }

    /// The flat pages that `wanted` takes.
    pub(super) fn flat(&self, wanted: impl Fn(&FlatPage<'_>) -> bool) -> Vec<PageId> {
        let takes = |page: &PageId| {
            let frame = self.frame(*page);
            wanted(&FlatPage::decode(&frame, FRAME_FORMAT).expect("a flat page"))
        };
        self.flat_pages().into_iter().filter(takes).collect()
    }

    /// Commits by hand what `write` does to the flat pages of the store.
    pub(super) fn write_flat(&self, write: impl FnOnce(&mut FlatWriter<'_, '_>)) {
        self.db.commit_flat(&self.versions, write);
    }

    /// The directory of the store as `pages` have it.
    pub(super) fn directory(&self, pages: &FlatWriter<'_, '_>) -> Directory {
        self.db.directory_at(pages)
    }

    /// The root cell of the Merk `name`, which is in use.
    pub(super) fn root(&self, name: u8) -> CellAddress {
        let roots = self.db.roots(&self.versions, self.latest());
        *roots
            .get(&model::subtree(name))
            .expect("the Merk has a root")
    }
}
