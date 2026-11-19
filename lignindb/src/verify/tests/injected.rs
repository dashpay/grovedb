//! The acceptance test of the checker: a list of corruptions, each
//! injected into a store that was sound, and each found.
//!
//! A corruption is one change that no writer of the engine makes: to a
//! page, through a page step that puts a changed copy in its place; to the
//! directory or a flat keyspace, through a commit made by hand; or to what
//! the store keeps beside its pages. Each names the rule that must be
//! among the findings. Most give others too, because one damage breaks
//! several rules at once.

use lignin_format::{
    delta::{Delta, DeltaRunWriter},
    flat_page::{FlatBranchBuilder, FlatLeafBuilder, FlatPage, FlatValue},
    segment::{BlockContent, Compression},
    tree_page::{CellValue, HashField, Side, TreeCell, TreeLink, TreePage, TreePageMut},
    value_log::{PendingValue, ValuePointer},
    CellAddress, PageId, SlotId,
};
use lignin_store::{
    load::FRAME_FORMAT,
    segment::BlockLocation,
    table::{Link, LinkKind},
    values::ValueChange,
};
use lignin_tree::{
    directory::{record, Host, Root, RootChange},
    flat::FlatTree,
};

use super::{plant, prefix, Planted, BIG, INNER, NEVER, OUTER};
use crate::{
    txn::tests::store::{SIZE, TREE_TYPE},
    verify::Rule,
};

/// One corruption: what it is, the change that makes it, and the rule
/// whose finding it must give.
struct Injection {
    name: &'static str,
    rule: Rule,
    inject: fn(&Planted),
}

const fn injection(name: &'static str, rule: Rule, inject: fn(&Planted)) -> Injection {
    Injection { name, rule, inject }
}

/// Injects each corruption of `list` into a store of its own and asks the
/// checker for its rule.
fn each_is_found(list: &[Injection]) {
    let mut missed = Vec::new();
    for injection in list {
        let planted = plant();
        planted.db.unchecked();
        (injection.inject)(&planted);
        let report = planted.check();
        println!("{:<64} {:?}", injection.name, report.rules());
        if !report.has(injection.rule) {
            missed.push(format!(
                "{}: {:?} is not among the findings\n{report}",
                injection.name, injection.rule
            ));
        }
    }
    assert!(missed.is_empty(), "{}", missed.join("\n"));
}

/// A link of a cell, with what it borrows copied.
struct OwnedLink {
    child: CellAddress,
    hash: [u8; 32],
    heights: (u8, u8),
    child_key_len: u8,
    aggregate: Vec<u8>,
}

impl OwnedLink {
    fn of(cell: &TreeCell<'_>, side: Side) -> Option<Self> {
        cell.link(side).map(|link| Self {
            child: link.child,
            hash: *link.hash,
            heights: link.heights,
            child_key_len: link.child_key_len,
            aggregate: link.aggregate.to_vec(),
        })
    }

    fn put(&self, page: &mut TreePageMut<'_>, slot: SlotId, side: Side) {
        let link = TreeLink {
            child: self.child,
            hash: &self.hash,
            heights: self.heights,
            child_key_len: self.child_key_len,
            aggregate: &self.aggregate,
        };
        page.set_link(slot, side, Some(&link)).expect("a link");
    }
}

impl Planted {
    /// The cells of the Merk `name`, a parent before its children.
    fn merk(&self, name: u8) -> Vec<CellAddress> {
        let (mut cells, mut next) = (Vec::new(), vec![self.root(name)]);
        while let Some(at) = next.pop() {
            cells.push(at);
            let children = |cell: &TreeCell<'_>| {
                let links = Side::BOTH.into_iter().filter_map(|side| cell.link(side));
                links.map(|link| link.child).collect::<Vec<_>>()
            };
            next.extend(self.read(at, children));
        }
        cells
    }

    /// A cell of the Merk `name` and a side on which `wanted` takes its
    /// link.
    fn link(
        &self,
        name: u8,
        wanted: impl Fn(CellAddress, &TreeLink<'_>) -> bool,
    ) -> (CellAddress, Side) {
        let found = self.merk(name).into_iter().find_map(|at| {
            let side = self.read(at, |cell| {
                Side::BOTH
                    .into_iter()
                    .find(|side| cell.link(*side).is_some_and(|link| wanted(at, &link)))
            });
            side.map(|side| (at, side))
        });
        found.expect("the Merk has such a link")
    }

    /// A link of [`BIG`] to a cell of another page.
    fn crossing(&self) -> (CellAddress, Side) {
        self.link(BIG, |at, link| link.child.page() != at.page())
    }

    /// A link of [`BIG`] to a cell of the same page.
    fn inside(&self) -> (CellAddress, Side) {
        self.link(BIG, |at, link| link.child.page() == at.page())
    }

    fn owned_link(&self, at: CellAddress, side: Side) -> OwnedLink {
        self.read(at, |cell| OwnedLink::of(cell, side))
            .expect("a link")
    }

    /// The cells of [`BIG`] whose value is in the value log, with their
    /// pointers.
    fn placed(&self) -> Vec<(CellAddress, ValuePointer)> {
        (self.merk(BIG).into_iter())
            .filter_map(|at| Some((at, self.read(at, |cell| cell.value_pointer())?)))
            .collect()
    }

    /// A cell of [`BIG`] whose value is in its cell.
    fn inline(&self, not_in: Option<PageId>) -> CellAddress {
        let found = self.merk(BIG).into_iter().find(|at| {
            Some(at.page()) != not_in && self.read(*at, |cell| cell.value().is_inline())
        });
        found.expect("a value in its cell")
    }

    /// The cell of the node `host` of [`OUTER`], which hosts [`INNER`].
    fn host(&self) -> CellAddress {
        let found = (self.merk(OUTER).into_iter())
            .find(|at| self.read(*at, |cell| cell.hosted_root_count() > 0));
        found.expect("the host")
    }

    /// A flat branch of the store, with its children.
    fn branch(&self) -> (PageId, Vec<PageId>) {
        let page =
            self.flat(|page| matches!(page, FlatPage::Branch(branch) if branch.len() > 1))[0];
        let frame = self.frame(page);
        let branch = FlatPage::decode(&frame, FRAME_FORMAT).expect("a flat page");
        let branch = branch.branch().expect("a branch");
        let children = (0..=branch.len()).map(|index| branch.child(index).expect("a child"));
        (page, children.collect())
    }

    /// Writes the branch `page` again, with the children and the level that
    /// `change` leaves.
    fn rebuild_branch(&self, page: PageId, change: impl FnOnce(&mut Vec<PageId>, &mut u8)) {
        self.step(page, |bytes| {
            let branch = FlatPage::decode(bytes, FRAME_FORMAT).expect("a flat page");
            let branch = branch.branch().expect("a branch");
            let mut level = branch.level();
            let mut children: Vec<PageId> = (0..=branch.len())
                .map(|index| branch.child(index).expect("a child"))
                .collect();
            let separators: Vec<Vec<u8>> = (0..branch.len())
                .map(|index| branch.separator(index).expect("a separator").to_vec())
                .collect();
            change(&mut children, &mut level);
            let capacity = SIZE.image_capacity();
            let mut built =
                FlatBranchBuilder::new(page, &[], capacity, level, children[0]).expect("a branch");
            for (separator, child) in separators.iter().zip(&children[1..]) {
                built.push(separator, *child).expect("a separator");
            }
            let image = built.finish();
            bytes.fill(0);
            bytes[..image.len()].copy_from_slice(&image);
        });
    }

    /// Writes the leaf `page` again, with the entries that `change` leaves:
    /// each a key, its value's bytes, and whether those are a place.
    fn rebuild_leaf(&self, page: PageId, change: impl FnOnce(&mut Vec<(Vec<u8>, Vec<u8>, bool)>)) {
        self.step(page, |bytes| {
            let leaf = FlatPage::decode(bytes, FRAME_FORMAT).expect("a flat page");
            let leaf = leaf.leaf().expect("a leaf");
            let mut entries: Vec<_> = (leaf.iter())
                .map(|(key, value)| (key.to_vec(), value.bytes().to_vec(), !value.is_inline()))
                .collect();
            change(&mut entries);
            let mut built = FlatLeafBuilder::new(page, &[], SIZE.image_capacity()).expect("a leaf");
            for (key, value, out_of_line) in &entries {
                let value = match out_of_line {
                    true => FlatValue::OutOfLine(value),
                    false => FlatValue::Inline(value),
                };
                built.push(key, value).expect("an entry");
            }
            let image = built.finish();
            bytes.fill(0);
            bytes[..image.len()].copy_from_slice(&image);
        });
    }

    /// The leaf that holds an entry `wanted` takes.
    fn leaf_with(&self, wanted: impl Fn(&[u8], &FlatValue<'_>) -> bool) -> PageId {
        let found = self.flat(|page| {
            (page.leaf()).is_some_and(|leaf| {
                leaf.iter()
                    .any(|(key, value)| wanted(&key.to_vec(), &value))
            })
        });
        *found.first().expect("the store has such a leaf")
    }

    /// The first byte of the page `page`, after its kind, whose change
    /// makes the page one that `refused` refuses, changed.
    fn spoil(&self, page: PageId, refused: impl Fn(&[u8]) -> bool) {
        self.step(page, |bytes| {
            for at in 5..bytes.len() {
                bytes[at] ^= 0x5a;
                if refused(bytes) {
                    return;
                }
                bytes[at] ^= 0x5a;
            }
            panic!("no byte of the page spoils it");
        });
    }

    /// A segment of this session that takes blocks, with one block that
    /// holds `payload` as `content` of `page`.
    fn block(&self, page: PageId, content: BlockContent, payload: &[u8]) -> BlockLocation {
        let mut writer = self.db.segments.create().expect("a segment");
        let mut manifest = self.db.manifest.lock().expect("the manifest");
        self.db.segments.commit(&mut manifest).expect("it is named");
        let location = (writer.append(page, content, Compression::Raw, payload)).expect("a block");
        writer.sync().expect("it is durable");
        location
    }

    /// The base image of the tree page `page` as `change` leaves it.
    fn image(&self, page: PageId, change: impl FnOnce(&mut TreePageMut<'_>)) -> Vec<u8> {
        let mut bytes = self.frame(page).to_vec();
        change(&mut TreePageMut::open(&mut bytes, SIZE, FRAME_FORMAT).expect("a tree page"));
        (TreePage::from_validated(&bytes, FRAME_FORMAT).expect("a tree page")).to_image()
    }

    /// A run of the tree page `page` that holds `deltas`.
    fn run(&self, page: PageId, deltas: &[Delta<'_>]) -> Vec<u8> {
        let mut run = Vec::new();
        let mut writer = DeltaRunWriter::new(page, None, &mut run);
        deltas
            .iter()
            .for_each(|delta| writer.push(delta).expect("a record"));
        writer.finish().expect("a run");
        run
    }

    /// The newest link of the page `page` in the page table.
    fn newest(&self, page: PageId) -> Link {
        self.db
            .table
            .newest(page)
            .expect("the page was written back")
    }
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("a slot")
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("a page id")
}

const OTHER_HASH: [u8; 32] = [0xab; 32];

const CELLS: &[Injection] = &[
    injection(
        "a value hash is changed",
        Rule::Hash(HashField::Value),
        |store| {
            let at = store.inline(None);
            store.step_tree(at, |page| {
                (page.set_hash(at.slot(), HashField::Value, &OTHER_HASH)).expect("a hash");
            });
        },
    ),
    injection(
        "a key-value hash is changed",
        Rule::Hash(HashField::KeyValue),
        |store| {
            let at = store.inline(None);
            store.step_tree(at, |page| {
                (page.set_hash(at.slot(), HashField::KeyValue, &OTHER_HASH)).expect("a hash");
            });
        },
    ),
    injection(
        "the hash in a link is changed",
        Rule::Hash(HashField::Child(Side::Left)),
        |store| {
            let (at, _) = store.link(BIG, |_, _| true);
            let side = store.read(at, |cell| match cell.has_link(Side::Left) {
                true => Side::Left,
                false => Side::Right,
            });
            if side == Side::Right {
                // The rule that is asked for names the left side.
                let (at, _) = store.link(BIG, |_, _| false);
                unreachable!("{at}");
            }
            store.step_tree(at, |page| {
                (page.set_hash(at.slot(), HashField::Child(side), &OTHER_HASH)).expect("a hash");
            });
        },
    ),
    injection(
        "a value is changed under its hash",
        Rule::Hash(HashField::Value),
        |store| {
            let at = store.inline(None);
            store.step_tree(at, |page| {
                (page.set_value(at.slot(), CellValue::Inline(b"another value"))).expect("a value");
            });
        },
    ),
    injection(
        "a link names a slot with no cell",
        Rule::DanglingAddress,
        |store| {
            let (at, side) = store.crossing();
            let child = store.owned_link(at, side).child;
            let free = store.frame(child.page());
            let free = (TreePage::from_validated(&free, FRAME_FORMAT).expect("a tree page"))
                .lowest_free_slot();
            store.step_tree(at, |page| {
                let to = CellAddress::new(child.page(), free);
                page.set_child(at.slot(), side, to).expect("an address");
            });
        },
    ),
    injection(
        "a link names a page that is not there",
        Rule::DanglingAddress,
        |store| {
            let (at, side) = store.crossing();
            store.step_tree(at, |page| {
                let to = CellAddress::new(self::page(4_000_000), slot(1));
                page.set_child(at.slot(), side, to).expect("an address");
            });
        },
    ),
    injection("a link names a flat page", Rule::DanglingAddress, |store| {
        let (at, side) = store.crossing();
        let flat = store.flat_pages()[0];
        store.step_tree(at, |page| {
            let to = CellAddress::new(flat, slot(1));
            page.set_child(at.slot(), side, to).expect("an address");
        });
    }),
    injection("two links name one cell", Rule::CellNamedTwice, |store| {
        let (first, side) = store.crossing();
        let named = store.owned_link(first, side).child;
        let (second, other) = store.link(BIG, |at, link| {
            at != first && link.child.page() != at.page() && link.child != named
        });
        store.step_tree(second, |page| {
            page.set_child(second.slot(), other, named)
                .expect("an address");
        });
    }),
    injection(
        "a link to another page states another key length",
        Rule::LinkKeyLength,
        |store| {
            let (at, side) = store.crossing();
            let mut link = store.owned_link(at, side);
            link.child_key_len += 1;
            store.step_tree(at, |page| link.put(page, at.slot(), side));
        },
    ),
    injection("a link states other heights", Rule::LinkHeights, |store| {
        let (at, side) = store.crossing();
        let mut link = store.owned_link(at, side);
        link.heights.0 += 1;
        store.step_tree(at, |page| link.put(page, at.slot(), side));
    }),
    injection(
        "the two children of a node are exchanged",
        Rule::KeyOrder,
        |store| {
            let both =
                |cell: &TreeCell<'_>| cell.has_link(Side::Left) && cell.has_link(Side::Right);
            let at = (store.merk(BIG).into_iter())
                .find(|at| store.read(*at, both))
                .expect("a node with two children");
            let (left, right) = (
                store.owned_link(at, Side::Left).child,
                store.owned_link(at, Side::Right).child,
            );
            store.step_tree(at, |page| {
                page.set_child(at.slot(), Side::Left, right)
                    .expect("an address");
                page.set_child(at.slot(), Side::Right, left)
                    .expect("an address");
            });
        },
    ),
    injection("a fragment root loses its mark", Rule::Fragments, |store| {
        let at = store.cell(|_, cell| cell.is_fragment_root());
        store.step_tree(at, |page| {
            page.set_fragment_root(at.slot(), false).expect("a mark");
        });
    }),
    injection(
        "a cell inside a fragment is marked as a root",
        Rule::Fragments,
        |store| {
            let at = store.cell(|_, cell| !cell.is_fragment_root());
            store.step_tree(at, |page| {
                page.set_fragment_root(at.slot(), true).expect("a mark");
            });
        },
    ),
    injection(
        "a link inside a page states another key length",
        Rule::Fragments,
        |store| {
            let (at, side) = store.inside();
            let mut link = store.owned_link(at, side);
            link.child_key_len += 1;
            store.step_tree(at, |page| link.put(page, at.slot(), side));
        },
    ),
    injection(
        "a tree page is not well formed",
        Rule::TreePageFormat,
        |store| {
            let page = store.tree_pages()[0];
            store.spoil(page, |bytes| {
                let checked = TreePage::from_validated(bytes, FRAME_FORMAT)
                    .and_then(|read| read.check(SIZE).map(|()| read.id()));
                checked.is_err()
            });
        },
    ),
    injection(
        "a tree page holds the id of another page",
        Rule::PageId,
        |store| {
            let pages = store.tree_pages();
            let other = store.frame(pages[1]).to_vec();
            store.step(pages[0], |bytes| bytes.copy_from_slice(&other));
        },
    ),
    injection(
        "a page is not read from its segment",
        Rule::PageUnreadable,
        |store| {
            let block = store
                .newest(store.tree_pages()[0])
                .location()
                .expect("a block");
            let id = store.db.ids.allocate().expect("an id");
            store.db.ids.publish(id).expect("it is private");
            let nowhere = BlockLocation {
                offset: block.offset + 1,
                ..block
            };
            (store.db.table.push(id, Link::base(store.latest(), nowhere))).expect("a link");
        },
    ),
    injection(
        "a cell that nothing reaches names a cell in use",
        Rule::CellNamedTwice,
        |store| {
            // The left child of the root is cut off with all below it, and
            // then names the root of another Merk.
            let root = store.root(BIG);
            let cut = store.owned_link(root, Side::Left).child;
            let in_use = store.root(INNER);
            store.step_tree(root, |page| {
                page.set_link(root.slot(), Side::Left, None)
                    .expect("no link");
            });
            let side = store.read(cut, |cell| match cell.has_link(Side::Left) {
                true => Side::Left,
                false => Side::Right,
            });
            store.step_tree(cut, |page| {
                page.set_child(cut.slot(), side, in_use)
                    .expect("an address");
            });
        },
    ),
];

const VALUES: &[Injection] = &[
    injection(
        "a cell holds the pointer of another cell",
        Rule::ValueShared,
        |store| {
            let placed = store.placed();
            let ((_, pointer), (second, _)) = (placed[0], placed[1]);
            store.step_tree(second, |page| {
                (page.set_value(second.slot(), CellValue::OutOfLine(pointer))).expect("a value");
            });
        },
    ),
    injection(
        "a cell was carried without its value",
        Rule::ValueOwner,
        |store| {
            let (first, pointer) = store.placed()[0];
            let second = store.inline(Some(first.page()));
            store.step_tree(first, |page| {
                (page.set_value(first.slot(), CellValue::Inline(b"gone"))).expect("a value");
            });
            store.step_tree(second, |page| {
                (page.set_value(second.slot(), CellValue::OutOfLine(pointer))).expect("a value");
            });
        },
    ),
    injection(
        "a value pointer leads to no value",
        Rule::ValueUnreadable,
        |store| {
            let (at, pointer) = store.placed()[0];
            let moved =
                ValuePointer::new(pointer.file(), pointer.offset() + 3, pointer.value_len());
            let moved = moved.expect("a pointer");
            store.step_tree(at, |page| {
                (page.set_value(at.slot(), CellValue::OutOfLine(moved))).expect("a value");
            });
        },
    ),
    injection(
        "a value pointer states another length",
        Rule::ValueUnreadable,
        |store| {
            let (at, pointer) = store.placed()[0];
            let longer =
                ValuePointer::new(pointer.file(), pointer.offset(), pointer.value_len() + 1);
            let longer = longer.expect("a pointer");
            store.step_tree(at, |page| {
                (page.set_value(at.slot(), CellValue::OutOfLine(longer))).expect("a value");
            });
        },
    ),
    injection(
        "a pending value has no bytes with its page",
        Rule::PendingBytes,
        |store| {
            let at = store.inline(None);
            let pending = PendingValue::new([9; 8], 300).expect("a pending value");
            store.step_tree(at, |page| {
                (page.set_value(at.slot(), CellValue::Pending(pending))).expect("a value");
            });
        },
    ),
    injection(
        "a value that a cell holds is released",
        Rule::ValueCount,
        |store| {
            let mut change = ValueChange::new();
            change.release(store.placed()[0].1);
            (store.db.values.apply(&change, store.latest())).expect("it was counted");
        },
    ),
    injection(
        "a flat record's pointer leads to no value",
        Rule::ValueUnreadable,
        |store| {
            let leaf = store.leaf_with(|_, value| !value.is_inline());
            store.rebuild_leaf(leaf, |entries| {
                let place = entries.iter_mut().find(|entry| entry.2).expect("a place");
                let pointer = ValuePointer::decode(place.1[..].try_into().expect("16 bytes"));
                let pointer = pointer.expect("a pointer");
                let moved =
                    ValuePointer::new(pointer.file(), pointer.offset() + 3, pointer.value_len());
                place.1 = moved.expect("a pointer").encode().to_vec();
            });
        },
    ),
];

const DIRECTORY: &[Injection] = &[
    injection(
        "an entry names a root that is no cell",
        Rule::EntryRoot,
        |store| {
            let flat = store.flat_pages()[0];
            store.write_flat(|pages| {
                let root = Root {
                    address: CellAddress::new(flat, slot(1)),
                    key: &super::key(0),
                };
                let named = RootChange::Named {
                    prefix: &prefix(BIG),
                    root: Some(root),
                };
                let directory = store.directory(pages);
                directory
                    .set_roots(pages, &[named])
                    .expect("the root is set");
            });
        },
    ),
    injection(
        "an entry states another root key",
        Rule::EntryRoot,
        |store| {
            let address = store.root(BIG);
            store.write_flat(|pages| {
                let root = Root {
                    address,
                    key: b"another key",
                };
                let named = RootChange::Named {
                    prefix: &prefix(BIG),
                    root: Some(root),
                };
                let directory = store.directory(pages);
                directory
                    .set_roots(pages, &[named])
                    .expect("the root is set");
            });
        },
    ),
    injection(
        "an entry names a host that is no node",
        Rule::EntryHost,
        |store| {
            store.write_flat(|pages| {
                let host = Host {
                    parent: &prefix(BIG),
                    key: b"nobody",
                    position: 0,
                };
                let directory = store.directory(pages);
                (directory.add(pages, &prefix(NEVER), TREE_TYPE, Some(host)))
                    .expect("it is entered");
            });
        },
    ),
    injection(
        "a host points at another cell",
        Rule::HostPointer,
        |store| {
            let (host, other) = (store.host(), store.root(BIG));
            store.step_tree(host, |page| {
                (page.set_hosted_root(host.slot(), 0, Some(other))).expect("a pointer");
            });
        },
    ),
    injection("a host has lost its pointer", Rule::HostPointer, |store| {
        let host = store.host();
        store.step_tree(host, |page| {
            page.set_hosted_root(host.slot(), 0, None)
                .expect("no pointer");
        });
    }),
    injection(
        "a hosted root has no entry",
        Rule::HostedUnentered,
        |store| {
            store.write_flat(|pages| {
                let mut directory = store.directory(pages);
                directory
                    .unlink(pages, &prefix(INNER))
                    .expect("it is unlinked");
            });
        },
    ),
    injection(
        "a root's record is taken out of the directory",
        Rule::Directory,
        |store| {
            let key = record::root_key(store.root(BIG));
            store.write_flat(|pages| {
                assert!(store.db.directory().delete(pages, &key).expect("a delete"));
            });
        },
    ),
    injection(
        "a flat record under a prefix with no entry",
        Rule::FlatRecordWithoutEntry,
        |store| {
            store.write_flat(|pages| {
                let key = [&prefix(NEVER)[..], b"orphan"].concat();
                (store.db.keyspaces().aux.put(pages, &key, b"a record")).expect("a put");
            });
        },
    ),
    injection(
        "a flat record whose key holds no prefix",
        Rule::FlatRecordWithoutEntry,
        |store| {
            store.write_flat(|pages| {
                (store.db.keyspaces().meta.put(pages, b"short", b"a record")).expect("a put");
            });
        },
    ),
];

const FLAT_TREES: &[Injection] = &[
    injection(
        "a branch names a child twice",
        Rule::FlatPageNamedTwice,
        |store| {
            let (branch, _) = store.branch();
            store.rebuild_branch(branch, |children, _| children[1] = children[0]);
        },
    ),
    injection(
        "a branch names a page that is not there",
        Rule::FlatDanglingPage,
        |store| {
            let (branch, _) = store.branch();
            store.rebuild_branch(branch, |children, _| children[1] = page(4_000_000));
        },
    ),
    injection(
        "a branch names a tree page",
        Rule::FlatDanglingPage,
        |store| {
            let (branch, _) = store.branch();
            let tree = store.tree_pages()[0];
            store.rebuild_branch(branch, |children, _| children[1] = tree);
        },
    ),
    injection("a branch is a level too high", Rule::FlatLevel, |store| {
        let (branch, _) = store.branch();
        store.rebuild_branch(branch, |_, level| *level += 1);
    }),
    injection(
        "two children of a branch are exchanged",
        Rule::FlatKeyOrder,
        |store| {
            let (branch, _) = store.branch();
            store.rebuild_branch(branch, |children, _| children.swap(0, 1));
        },
    ),
    injection(
        "a key above the limit that is no bucket",
        Rule::FlatBucket,
        |store| {
            let limit = store.db.config().key_limit();
            let leaf = store.leaf_with(|_, _| true);
            store.rebuild_leaf(leaf, |entries| {
                entries.push((vec![0xff; limit + 5], b"a value".to_vec(), false));
            });
        },
    ),
    injection(
        "a bucket of long keys that names no page",
        Rule::FlatBucket,
        |store| {
            let limit = store.db.config().key_limit();
            let leaf = store.leaf_with(|key, _| key.len() > limit);
            store.rebuild_leaf(leaf, |entries| {
                let bucket = (entries.iter_mut()).find(|entry| entry.0.len() > limit);
                bucket.expect("the bucket").1 = b"xx".to_vec();
            });
        },
    ),
    injection(
        "a bucket names a page of another keyspace",
        Rule::FlatPageNamedTwice,
        |store| {
            let limit = store.db.config().key_limit();
            let other = store.db.keyspaces().meta.root();
            let leaf = store.leaf_with(|key, _| key.len() > limit);
            store.rebuild_leaf(leaf, |entries| {
                let bucket = (entries.iter_mut()).find(|entry| entry.0.len() > limit);
                bucket.expect("the bucket").1 = other.encode().to_vec();
            });
        },
    ),
    injection(
        "a flat page that nothing reaches",
        Rule::FlatPageUnreached,
        |store| {
            store.write_flat(|pages| {
                FlatTree::create(pages, store.db.config()).expect("a keyspace");
            });
        },
    ),
    injection(
        "a flat page is not well formed",
        Rule::FlatPageFormat,
        |store| {
            let leaf = store.leaf_with(|_, _| true);
            store.spoil(leaf, |bytes| FlatPage::decode(bytes, FRAME_FORMAT).is_err());
        },
    ),
    injection(
        "a flat page holds the id of another page",
        Rule::PageId,
        |store| {
            let pages = store.flat(|page| page.leaf().is_some());
            let other = store.frame(pages[1]).to_vec();
            store.step(pages[0], |bytes| bytes.copy_from_slice(&other));
        },
    ),
];

const BESIDE_THE_PAGES: &[Injection] = &[
    injection(
        "the id of a page in use is freed",
        Rule::IdNotLive,
        |store| {
            let page = store.tree_pages()[0];
            (store.db.ids.free(page, store.latest())).expect("it is live");
        },
    ),
    injection("a live id names no page", Rule::IdWithoutPage, |store| {
        let id = store.db.ids.allocate().expect("an id");
        store.db.ids.publish(id).expect("it is private");
    }),
    injection(
        "a base image in a segment holds a pending value",
        Rule::PendingInSegment,
        |store| {
            let at = store.inline(None);
            let pending = PendingValue::new([9; 8], 300).expect("a pending value");
            let image = store.image(at.page(), |page| {
                (page.set_value(at.slot(), CellValue::Pending(pending))).expect("a value");
            });
            let block = store.block(at.page(), BlockContent::Base, &image);
            (store
                .db
                .table
                .push(at.page(), Link::base(store.latest(), block)))
            .expect("a link");
        },
    ),
    injection(
        "a run in a segment ends with a pending value",
        Rule::PendingInSegment,
        |store| {
            let at = store.inline(None);
            let pending = PendingValue::new([9; 8], 300).expect("a pending value");
            let run = store.run(
                at.page(),
                &[Delta::Value {
                    slot: at.slot(),
                    value: CellValue::Pending(pending),
                }],
            );
            let block = store.block(at.page(), BlockContent::Delta, &run);
            (store
                .db
                .table
                .push(at.page(), Link::delta(store.latest(), block)))
            .expect("a link");
        },
    ),
    injection(
        "a value is placed under a pointer of another length",
        Rule::PlacedLength,
        |store| {
            let (at, pointer) = store.placed()[0];
            let pending = PendingValue::new([9; 8], pointer.value_len() + 7);
            let run = store.run(
                at.page(),
                &[
                    Delta::Value {
                        slot: at.slot(),
                        value: CellValue::Pending(pending.expect("a pending value")),
                    },
                    Delta::Value {
                        slot: at.slot(),
                        value: CellValue::OutOfLine(pointer),
                    },
                ],
            );
            let block = store.block(at.page(), BlockContent::Delta, &run);
            (store
                .db
                .table
                .push(at.page(), Link::delta(store.latest(), block)))
            .expect("a link");
        },
    ),
    injection(
        "a link names the block of another page",
        Rule::Block,
        |store| {
            let pages = store.tree_pages();
            let other = store.newest(pages[1]).location().expect("a block");
            (store
                .db
                .table
                .push(pages[0], Link::base(store.latest(), other)))
            .expect("a link");
        },
    ),
    injection("two links name one block", Rule::BlockNamedTwice, |store| {
        let page = store.tree_pages()[0];
        let LinkKind::Base(block) = store.newest(page).kind else {
            panic!("the page was written back as an image");
        };
        (store.db.table.push(page, Link::base(store.latest(), block))).expect("a link");
    }),
    injection("a link names no block", Rule::Block, |store| {
        let page = store.tree_pages()[0];
        let block = store.newest(page).location().expect("a block");
        let nowhere = BlockLocation {
            offset: block.offset + 1,
            ..block
        };
        (store
            .db
            .table
            .push(page, Link::base(store.latest(), nowhere)))
        .expect("a link");
    }),
    injection("a run of deltas for a flat page", Rule::Block, |store| {
        let flat = store.db.keyspaces().meta.root();
        let run = store.run(flat, &[Delta::Remove { slot: slot(1) }]);
        let block = store.block(flat, BlockContent::Delta, &run);
        (store
            .db
            .table
            .push(flat, Link::delta(store.latest(), block)))
        .expect("a link");
    }),
    injection(
        "a link says an image and its block holds a run",
        Rule::Block,
        |store| {
            let page = store.tree_pages()[0];
            let run = store.run(page, &[Delta::Remove { slot: slot(1) }]);
            let block = store.block(page, BlockContent::Delta, &run);
            (store.db.table.push(page, Link::base(store.latest(), block))).expect("a link");
        },
    ),
];

#[test]
fn every_corruption_of_the_cells_is_found() {
    each_is_found(CELLS);
}

#[test]
fn every_corruption_of_the_values_is_found() {
    each_is_found(VALUES);
}

#[test]
fn every_corruption_of_the_directory_is_found() {
    each_is_found(DIRECTORY);
}

#[test]
fn every_corruption_of_the_flat_trees_is_found() {
    each_is_found(FLAT_TREES);
}

#[test]
fn every_corruption_beside_the_pages_is_found() {
    each_is_found(BESIDE_THE_PAGES);
}

/// The lists name every rule but those that need a store of another kind,
/// which `sound.rs` and the tests below have.
#[test]
fn the_lists_cover_the_rules() {
    let lists = [CELLS, VALUES, DIRECTORY, FLAT_TREES, BESIDE_THE_PAGES];
    let covered: Vec<Rule> = (lists.iter())
        .flat_map(|list| list.iter().map(|injection| injection.rule))
        .collect();
    for rule in [
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
        Rule::FlatRecordWithoutEntry,
        Rule::FlatPageFormat,
        Rule::FlatDanglingPage,
        Rule::FlatPageNamedTwice,
        Rule::FlatLevel,
        Rule::FlatKeyOrder,
        Rule::FlatBucket,
        Rule::FlatPageUnreached,
        Rule::IdNotLive,
        Rule::IdWithoutPage,
        Rule::Block,
        Rule::BlockNamedTwice,
    ] {
        assert!(
            covered.contains(&rule),
            "no corruption is injected for {rule:?}"
        );
    }
    let total: usize = lists.iter().map(|list| list.len()).sum();
    assert_eq!(total, 55, "the list in the report names each");
}
