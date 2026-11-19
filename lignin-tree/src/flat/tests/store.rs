//! The flat keyspace on the page store: private copies in the buffer pool,
//! commits into a log group, write-back into segments and the value log,
//! and the value log's cleaner.

use std::{cell::RefCell, sync::Arc};

use lignin_format::{
    manifest::Manifest,
    segment::{BlockContent, Compression},
    value_log::ValueLogError,
    wal::{scan, write_file_start, Change, Entry, FlatChange, GroupWriter, LogItem, LogStart},
    CommitVersion, FormatError,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    alloc::{PageAllocator, PageState},
    cache::{BufferPool, OwnerId, PageRef, PoolConfig},
    load::{LoadError, PageLoader, FRAME_FORMAT},
    segment::{ManifestStore, SegmentStore, SegmentWriter},
    table::{Link, PageTable},
    values::{
        OwnersError, ValueChange, ValueHold, ValueOwners, ValueStore, ValueStoreError, ValueWriter,
    },
};
use tempfile::TempDir;

use super::{
    super::{Committed, FlatOwners, Private, StepLog},
    *,
};

type Writer<'s> = Private<'s, &'s PageAllocator>;

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

/// The pending values of the leaf in `frame`, each with the position of
/// its entry.
fn pending_of(frame: &[u8]) -> Vec<(usize, PendingValue)> {
    let Some(leaf) = FlatPage::decode(frame, V1).expect("a page").leaf() else {
        return Vec::new();
    };
    leaf.iter()
        .enumerate()
        .filter_map(|(slot, (_, value))| match value {
            FlatValue::OutOfLine(place) => Some((slot, place_of(place).pending()?)),
            FlatValue::Inline(_) => None,
        })
        .collect()
}

struct State {
    segment: SegmentWriter,
    value_writer: ValueWriter,
    log: Vec<u8>,
    latest: u64,
    owners: u64,
}

/// A store in a directory, with what the tasks after this one build
/// reduced to the steps the flat keyspace takes part in: a commit that
/// writes one log group and installs the copies, and a write-back that
/// places the pending values and writes the pages to a segment.
struct Db {
    _dir: TempDir,
    size: PageSize,
    config: FlatConfig,
    segments: SegmentStore,
    table: PageTable,
    pool: BufferPool,
    values: ValueStore,
    ids: PageAllocator,
    manifest: RefCell<ManifestStore>,
    state: RefCell<State>,
}

impl Db {
    fn new() -> Self {
        let size = PageSize::MIN;
        // In memory where there is such a place: the syncs of a store are
        // most of what these tests would wait for.
        let memory = std::path::Path::new("/dev/shm");
        let dir = match memory.is_dir() {
            true => tempfile::tempdir_in(memory),
            false => tempfile::tempdir(),
        };
        let dir = dir.expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
        let empty = Manifest::new(size, Default::default(), None, Vec::new()).expect("a manifest");
        let mut manifest = ManifestStore::create(vfs, dir.path(), empty).expect("a store");
        let segments = SegmentStore::open(&manifest).expect("segments");
        let segment = segments.create().expect("a segment");
        segments.commit(&mut manifest).expect("named");
        let values = ValueStore::open(&manifest, &[]).expect("a value log");
        let value_writer = values.create().expect("a value-log file");
        values.commit(&mut manifest).expect("named");
        let mut log = Vec::new();
        write_file_start(FileId::MIN, &LogStart::FIRST, &mut log).expect("a log file");
        let pool = BufferPool::new(PoolConfig {
            page_size: size,
            capacity: 4 << 20,
        });
        Self {
            _dir: dir,
            size,
            config: FlatConfig::new(size),
            segments,
            table: PageTable::new(),
            pool: pool.expect("a pool"),
            values,
            ids: PageAllocator::new(),
            manifest: RefCell::new(manifest),
            state: RefCell::new(State {
                segment,
                value_writer,
                log,
                latest: 0,
                owners: 0,
            }),
        }
    }

    fn latest(&self) -> CommitVersion {
        version(self.state.borrow().latest)
    }

    fn loader(&self) -> PageLoader<'_> {
        PageLoader::new(&self.pool, &self.table, &self.segments, self.size).expect("a loader")
    }

    /// The committed pages at `at`.
    fn reader_at(&self, at: CommitVersion) -> Committed<'_> {
        Committed::new(self.loader(), &self.values, at)
    }

    fn reader(&self) -> Committed<'_> {
        self.reader_at(self.latest())
    }

    /// A writer over the latest version.
    fn writer(&self) -> Writer<'_> {
        let owner = {
            let mut state = self.state.borrow_mut();
            state.owners += 1;
            OwnerId::new(state.owners)
        };
        Private::new(self.reader(), &self.pool, owner, &self.ids)
    }

    /// The committed pages at `at`, read through an empty buffer pool: each
    /// page comes from its segment.
    fn cold<R>(&self, at: CommitVersion, read: impl FnOnce(&Committed<'_>) -> R) -> R {
        let pool = BufferPool::new(PoolConfig {
            page_size: self.size,
            capacity: 4 << 20,
        })
        .expect("a pool");
        let loader = PageLoader::new(&pool, &self.table, &self.segments, self.size);
        read(&Committed::new(loader.expect("a loader"), &self.values, at))
    }

    /// Commits `writer`: one group in the log, with the values, the runs of
    /// the pages in ascending order and the pointers released, and then the
    /// copies installed and the freed pages freed (`transactions.md` CM3).
    fn commit(&self, mut writer: Writer<'_>) -> CommitVersion {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let version = version(state.latest + 1);
        let address = LogStart::FIRST
            .address(state.log.len() as u64)
            .expect("an address");
        let mut group = GroupWriter::new(version, address, &mut state.log).expect("a group");
        writer.log_values(&mut group).expect("the values");
        let mut pages: Vec<(PageId, bool)> = writer
            .written()
            .map(|page| (page, false))
            .chain(writer.freed().map(|page| (page, true)))
            .collect();
        pages.sort();
        let mut unchanged = Vec::new();
        for (page, freed) in pages {
            if freed {
                group.freed(page).expect("a freed page");
            } else if !writer.log_page(page, &mut group).expect("a run") {
                unchanged.push(page);
            }
        }
        for pointer in writer.released().to_vec() {
            group.released(&pointer).expect("a released pointer");
        }
        group.finish();

        let parts = writer.into_parts();
        for (copy, created) in parts.written {
            let page = copy.page();
            if unchanged.contains(&page) {
                self.pool.discard(&copy).expect("discarded");
                continue;
            }
            self.pool.install(&copy, version).expect("installed");
            if created {
                self.ids.publish(page).expect("published");
            }
        }
        for copy in parts.freed {
            let page = copy.page();
            self.pool.discard(&copy).expect("discarded");
            if self.table.newest(page).is_some() {
                self.table
                    .push(page, Link::free(version))
                    .expect("a free link");
            }
            self.pool.free(page, version).expect("freed");
            self.ids.free(page, version).expect("freed");
        }
        let mut change = ValueChange::new();
        parts
            .released
            .iter()
            .for_each(|&pointer| change.release(pointer));
        self.values.apply(&change, version).expect("counted");
        state.latest += 1;
        version
    }

    /// Gives up `writer`: its copies are discarded, and the ids of the
    /// pages it made go back.
    fn roll_back(&self, writer: Writer<'_>) {
        let parts = writer.into_parts();
        for (copy, created) in parts.written {
            self.pool.discard(&copy).expect("discarded");
            if created {
                self.ids.release(copy.page()).expect("released");
            }
        }
        for copy in parts.freed {
            self.pool.discard(&copy).expect("discarded");
        }
    }

    /// Write-back: every pending value of a current page is appended to
    /// the value log and given its pointer in a page step, and then every
    /// page version that no segment holds is written to one.
    fn write_back(&self) {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let latest = version(state.latest);
        let mut versions = Vec::new();
        self.pool.unwritten_versions(&mut versions);
        let mut owed = Vec::new();
        for born in versions {
            let mut pages = Vec::new();
            self.pool.unwritten_pages(born, &mut pages);
            pages.sort();
            owed.extend(pages.into_iter().map(|page| (born, page)));
        }

        // The values first, under a hold, and a manifest that covers them.
        let mut hold = ValueHold::new();
        let mut placed = Vec::new();
        for &(born, page) in &owed {
            let Some(frame) = self.pool.get_version(page, born) else {
                continue;
            };
            if !self.pool.page_step(page).is_current(&frame) {
                continue;
            }
            for (slot, pending) in pending_of(&frame) {
                let bytes = frame.attached(slot as u32).expect("the value's bytes");
                let pointer = state.value_writer.append(&mut hold, page, bytes);
                placed.push((page, pending, pointer.expect("appended")));
            }
        }
        if !placed.is_empty() {
            state.value_writer.sync().expect("synced");
            self.values
                .commit(&mut self.manifest.borrow_mut())
                .expect("covered");
        }

        // Then each pointer, in a page step that is recorded and counted.
        for (page, pending, pointer) in placed {
            let counts = self.values.step().expect("the counts");
            let step = self.pool.page_step(page);
            let old = step.current().expect("the page");
            let slot = pending_of(&old)
                .into_iter()
                .find(|&(_, held)| held == pending)
                .expect("the entry")
                .0;
            let change = ValueMove::in_flat_leaf(page, ValuePlace::Pending(pending), pointer)
                .expect("a step");
            change.write(&mut state.log).expect("a record");
            let mut image = Vec::new();
            change
                .apply_to_flat_leaf(&old, FRAME_FORMAT, self.config.capacity(), &mut image)
                .expect("placed");
            let mut blank = self.pool.blank();
            blank.copy_from(&old);
            blank[..image.len()].copy_from_slice(&image);
            blank.detach(slot as u32).expect("attached");
            step.replace(&old, blank).expect("replaced");
            drop(step);
            let mut acquired = ValueChange::new();
            acquired.acquire(pointer);
            counts.apply(&acquired, latest).expect("counted");
        }
        drop(hold);

        // Then the pages. A version that a commit has superseded with a
        // value that was never placed is kept for its readers instead.
        for (born, page) in owed {
            let Some(frame) = self.pool.get_version(page, born) else {
                continue;
            };
            if pending_of(&frame).is_empty() {
                self.write(state, page, born, &frame);
            } else {
                self.pool.hold_for_readers(&frame).expect("held");
            }
        }
    }

    fn write(&self, state: &mut State, page: PageId, born: CommitVersion, frame: &PageRef<'_>) {
        let image = FlatPage::decode(frame, V1).expect("a page").as_bytes();
        let block = state
            .segment
            .append(page, BlockContent::Base, Compression::Lz4, image)
            .expect("a block");
        self.table
            .push(page, Link::base(born, block))
            .expect("a link");
        self.pool.mark_written(frame).expect("written");
    }

    /// The groups of the log, each as its values (owner and bytes), the
    /// pages it has runs of, the pages it freed and the pointers released.
    fn groups(&self) -> Vec<Logged> {
        let state = self.state.borrow();
        let items = scan(&state.log).expect("a log");
        let mut groups = Vec::new();
        for item in items {
            let LogItem::Commit(group) = item else {
                continue;
            };
            let mut logged = Logged::default();
            for (address, value) in group.values() {
                let bytes = value.bytes().expect("the value's bytes").to_vec();
                let pending = address.pending(value.value_len()).expect("a pending value");
                logged.values.push((value.owner(), pending, bytes));
            }
            for entry in group.entries() {
                match entry {
                    Entry::Flat(run) => {
                        let changes = run.changes().map(|change| match change {
                            FlatChange::Image(image) => pending_of(image),
                            FlatChange::Insert { value, .. }
                            | FlatChange::Replace { value, .. } => match value {
                                FlatValue::OutOfLine(place) => place_of(place)
                                    .pending()
                                    .map(|held| (0, held))
                                    .into_iter()
                                    .collect(),
                                FlatValue::Inline(_) => Vec::new(),
                            },
                            _ => Vec::new(),
                        });
                        let held = changes.flatten().map(|(_, held)| held).collect();
                        logged.runs.push((run.page(), held));
                    }
                    Entry::Freed(page) => logged.freed.push(page),
                    Entry::Released(pointer) => logged.released.push(pointer),
                    other => panic!("no {other:?} is written here"),
                }
            }
            groups.push(logged);
        }
        groups
    }

    /// The pointer rewrites that the log holds, as background changes: the
    /// moves of values, without the placings of write-back.
    fn rewrites(&self) -> Vec<ValueMove> {
        let state = self.state.borrow();
        scan(&state.log)
            .expect("a log")
            .filter_map(|item| match item {
                LogItem::Change(Change::Value(step)) => Some(step),
                _ => None,
            })
            .filter(|step| !step.from().is_pending())
            .collect()
    }
}

/// What one commit group says.
#[derive(Debug, Default)]
struct Logged {
    values: Vec<(PageId, PendingValue, Vec<u8>)>,
    /// Each page with a run, and the pending values its records hold.
    runs: Vec<(PageId, Vec<PendingValue>)>,
    freed: Vec<PageId>,
    released: Vec<ValuePointer>,
}

impl StepLog for Db {
    fn latest(&self) -> CommitVersion {
        Db::latest(self)
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        self.state.borrow_mut().log.extend_from_slice(record);
        Ok(())
    }
}

/// Every record of the keyspace, through a cursor.
fn scan_all(pages: &impl FlatPages, tree: &FlatTree) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut cursor = tree.cursor();
    cursor.seek_to_first(pages).expect("a seek");
    while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
        let mut value = Vec::new();
        assert!(cursor.value(pages, &mut value).expect("a value"));
        found.insert(key, value);
        cursor.next(pages).expect("a step");
    }
    found
}

/// The leaf that holds `key`, and the place of the key's value out of line.
fn home(pages: &impl FlatPages, tree: &FlatTree, key: &[u8]) -> (PageId, ValuePlace) {
    let (_, page) = tree.descend(pages, key).expect("a leaf");
    let frame = pages.page(page).expect("the leaf");
    let leaf = FlatPage::decode(&frame, V1)
        .expect("a page")
        .leaf()
        .expect("a leaf");
    match leaf.get(key).expect("the entry") {
        FlatValue::OutOfLine(place) => (page, place_of(place)),
        FlatValue::Inline(_) => panic!("the value is in the page"),
    }
}

#[test]
fn a_keyspace_in_the_store_is_written_committed_and_read_back() {
    let db = Db::new();
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    let mut model = BTreeMap::new();
    let key = |n: u32| [&b"key-"[..], &n.to_be_bytes()].concat();
    for n in 0..400u32 {
        let value = bytes(u64::from(n), [0, 700, 1_500][n as usize % 3]);
        tree.put(&mut writer, &key(n), &value).expect("a put");
        model.insert(key(n), value);
    }
    let long = bytes(77, 20_000);
    tree.put(&mut writer, &long, &bytes(78, 9_000))
        .expect("a put");
    model.insert(long.clone(), bytes(78, 9_000));

    // The writer reads its own writes, and nobody else does: the root is
    // no page for a reader.
    assert!(scan_all(&writer, &tree) == model);
    let unborn = db.reader().page(tree.root());
    assert!(matches!(
        unborn,
        Err(FlatError::Load(LoadError::NoPage { .. }))
    ));
    assert_eq!(db.ids.state(tree.root()), PageState::Private);
    let first = db.commit(writer);
    assert_eq!(db.ids.state(tree.root()), PageState::Live);
    assert!(scan_all(&db.reader(), &tree) == model);

    // A second transaction, over pages that still have pending values:
    // every kind of write, and its reads beside a reader of the first.
    let mut writer = db.writer();
    let before = model.clone();
    for n in (0..400).step_by(7) {
        assert!(tree.delete(&mut writer, &key(n)).expect("a delete"));
        model.remove(&key(n));
    }
    for n in (1..400).step_by(11) {
        let value = bytes(u64::from(n) + 5_000, [1_600, 3, 0][n as usize % 3]);
        tree.put(&mut writer, &key(n), &value).expect("a put");
        model.insert(key(n), value);
    }
    tree.delete_range(&mut writer, &key(200), Some(&key(260)))
        .expect("a range delete");
    model.retain(|k, _| *k < key(200) || *k >= key(260));
    assert!(tree.delete(&mut writer, &long).expect("a delete"));
    model.remove(&long);
    assert!(scan_all(&writer, &tree) == model);
    assert!(scan_all(&db.reader(), &tree) == before);
    let second = db.commit(writer);
    assert!(scan_all(&db.reader(), &tree) == model);
    assert!(scan_all(&db.reader_at(first), &tree) == before);

    // The second group repeats no value of the first that is still
    // pending in a page it changed: only the values it wrote or carried.
    let groups = db.groups();
    assert_eq!(groups.len(), 2);
    for (owner, held, value) in &groups[1].values {
        let run = groups[1]
            .runs
            .iter()
            .find(|(page, _)| page == owner)
            .expect("a run");
        assert!(run.1.contains(held), "the run of {owner} names the value");
        assert!(value.len() > db.config.max_inline());
    }
    assert!(groups[1].released.is_empty(), "no value had a pointer yet");

    // Write-back places every value and puts every page into a segment.
    // Read through an empty pool, each page comes from there.
    db.write_back();
    assert!(scan_all(&db.reader(), &tree) == model);
    assert!(db.cold(second, |pages| scan_all(pages, &tree)) == model);
    assert!(db.pool.oldest_unwritten().is_none());

    // A page that a transaction takes and leaves as it was has no run, and
    // a transaction that is given up leaves nothing.
    let mut writer = db.writer();
    tree.put(&mut writer, &key(3), &model[&key(3)].clone())
        .expect("a put");
    assert_eq!(writer.written().count(), 1);
    let third = db.commit(writer);
    assert!(db.groups()[2].runs.is_empty());
    let mut writer = db.writer();
    tree.delete_range(&mut writer, b"", None)
        .expect("a range delete");
    for n in 0..300u32 {
        tree.put(&mut writer, &n.to_be_bytes(), &bytes(1, 900))
            .expect("a put");
    }
    assert_eq!(scan_all(&writer, &tree).len(), 300);
    assert!(writer.freed().count() > 3 && writer.written().count() > 3);
    let made: Vec<PageId> = writer
        .written()
        .filter(|&page| page != tree.root())
        .collect();
    db.roll_back(writer);
    assert!(made
        .iter()
        .all(|&page| db.ids.state(page) == PageState::Free));
    assert!(scan_all(&db.reader_at(third), &tree) == model);
    assert_eq!(db.groups().len(), 3);
}

/// The acceptance test of the plan's draft 12, on the store: an entry with
/// a value out of line that a split or a merge takes to another page has
/// the value appended again with the new page as its owner, in the same
/// commit.
#[test]
fn a_carried_value_is_appended_again_for_its_new_page_in_the_same_commit() {
    let db = Db::new();
    let big = |n: u8| vec![n; db.config.max_inline() + 100 + usize::from(n)];
    let key = |n: u8| [vec![n], vec![b'k'; 400]].concat();

    // One leaf, the root, with eight values, placed in the value log.
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    let root = tree.root();
    for n in 0..8 {
        tree.put(&mut writer, &key(n), &big(n)).expect("a put");
    }
    db.commit(writer);
    db.write_back();
    let reader = db.reader();
    let old: Vec<ValuePointer> = (0..8)
        .map(|n| home(&reader, &tree, &key(n)).1.pointer().expect("placed"))
        .collect();
    let file = old[0].file();
    assert_eq!(db.values.count(file).map(|count| count.values), Some(8));
    drop(reader);

    // The commit that splits the root.
    let mut writer = db.writer();
    for n in 8..24 {
        tree.put(&mut writer, &key(n), b"small").expect("a put");
    }
    let split = db.commit(writer);
    let reader = db.reader();
    assert!(FlatPage::decode(&reader.page(root).expect("the root"), V1)
        .expect("a page")
        .branch()
        .is_some());
    let group = db.groups().pop().expect("the group of the split");

    for n in 0..8 {
        // The entry is in another page, and holds no pointer: its value is
        // pending again, and the group has the value's bytes in a record
        // whose owner is that page, at the address the entry names.
        let (page, place) = home(&reader, &tree, &key(n));
        let held = place.pending().expect("pending again");
        assert_ne!(page, root);
        assert!(group.values.contains(&(page, held, big(n))));
        // The run of the new page, in the same group, holds that entry.
        let run = group
            .runs
            .iter()
            .find(|(of, _)| *of == page)
            .expect("a run");
        assert!(run.1.contains(&held));
        // The value reads through the entry, from the frame of its page.
        assert_eq!(get(&reader, &tree, &key(n)), Some(big(n)));
        // The old record cannot stand for it: it names the old page.
        let mut buf = Vec::new();
        let wrong = db.values.read(old[usize::from(n)], page, &mut buf);
        let wrong = wrong.expect_err("a record of another owner");
        assert!(matches!(
            wrong,
            ValueStoreError::Format {
                source: FormatError::ValueLog(ValueLogError::WrongOwner { .. }),
                ..
            }
        ));
    }
    // The same group releases every old pointer, and the counts follow at
    // that commit.
    assert_eq!(group.released, old);
    assert_eq!(group.values.len(), 8);
    assert_eq!(db.values.count(file).map(|count| count.values), Some(0));
    drop(reader);

    // Write-back appends each value under its new owner, and from then on
    // the value reads through its own record.
    db.write_back();
    let reader = db.reader();
    let mut placed = Vec::new();
    for n in 0..8 {
        let (page, place) = home(&reader, &tree, &key(n));
        let pointer = place.pointer().expect("placed");
        assert!(!old.contains(&pointer));
        let mut buf = Vec::new();
        let value = db.values.read(pointer, page, &mut buf).expect("the value");
        assert_eq!((value.owner(), value.to_vec()), (page, big(n)));
        placed.push((page, pointer));
    }
    assert_eq!(db.values.count(file).map(|count| count.values), Some(8));
    assert!(db.cold(split, |pages| (0..8)
        .all(|n| get(pages, &tree, &key(n)) == Some(big(n)))));
    drop(reader);

    // A put that splits nothing leaves every other pointer where it is.
    let mut writer = db.writer();
    tree.put(&mut writer, &key(3), &big(33)).expect("a put");
    db.commit(writer);
    let group = db.groups().pop().expect("a group");
    assert_eq!(group.released, [placed[3].1]);
    assert_eq!(group.values.len(), 1);
    assert_eq!(group.values[0].2, big(33));
    let reader = db.reader();
    for n in (0..8).filter(|&n| n != 3) {
        assert_eq!(
            home(&reader, &tree, &key(n)),
            (
                placed[usize::from(n)].0,
                ValuePlace::Placed(placed[usize::from(n)].1)
            )
        );
    }
    drop(reader);
    db.write_back();

    // The commit that merges the leaves and gives the root its last one:
    // the values that are left are carried again, each with its entry.
    let reader = db.reader();
    let left: Vec<(u8, PageId, ValuePointer)> = [3, 6, 7]
        .into_iter()
        .map(|n| {
            let (page, place) = home(&reader, &tree, &key(n));
            (n, page, place.pointer().expect("placed"))
        })
        .collect();
    drop(reader);
    let mut writer = db.writer();
    for n in (0..24).filter(|n| ![3, 6, 7].contains(n)) {
        assert!(tree.delete(&mut writer, &key(n)).expect("a delete"));
    }
    let merged = db.commit(writer);
    let reader = db.reader();
    let group = db.groups().pop().expect("the group of the merge");
    assert!(!group.freed.is_empty());
    for &(n, was, pointer) in &left {
        let (page, place) = home(&reader, &tree, &key(n));
        let value = if n == 3 { big(33) } else { big(n) };
        assert_eq!(page, root);
        assert_ne!(was, root);
        assert!(group
            .values
            .contains(&(root, place.pending().expect("pending"), value.clone())));
        assert!(group.released.contains(&pointer));
        assert_eq!(get(&reader, &tree, &key(n)), Some(value));
    }
    assert_eq!(
        group.released.len(),
        8,
        "the five deleted and the three carried"
    );
    drop(reader);
    db.write_back();
    assert_eq!(db.values.count(file).map(|count| count.values), Some(3));
    let expected: BTreeMap<_, _> = [(key(3), big(33)), (key(6), big(6)), (key(7), big(7))].into();
    assert!(db.cold(merged, |pages| scan_all(pages, &tree)) == expected);
}

#[test]
fn the_cleaner_of_the_value_log_finds_and_repoints_the_entries_of_flat_leaves() {
    let db = Db::new();
    let big = |n: u8| vec![n; db.config.max_inline() + 1 + usize::from(n)];
    let key = |n: u8| [vec![n], vec![b'k'; 300]].concat();
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    for n in 0..60 {
        tree.put(&mut writer, &key(n), &big(n)).expect("a put");
    }
    db.commit(writer);
    db.write_back();
    let reader = db.reader();
    let homes: Vec<(PageId, ValuePointer)> = (0..60)
        .map(|n| {
            let (page, place) = home(&reader, &tree, &key(n));
            (page, place.pointer().expect("placed"))
        })
        .collect();
    drop(reader);
    let file = homes[0].1.file();

    // The file is closed, and another takes what is moved out of it.
    let mut to = db.close_value_file();
    let owners = FlatOwners::new(db.loader(), &db.pool, db.config, &db);

    // An entry is found in the page its value's record names, as the page
    // version that is current. A pointer that no entry holds, a branch and
    // a page that does not exist own nothing.
    let (page, pointer) = homes[20];
    assert_eq!(
        owners.find(page, pointer).expect("a look"),
        Some(version(1))
    );
    let other = homes
        .iter()
        .find(|(other, _)| *other != page)
        .expect("a second leaf");
    assert_eq!(owners.find(other.0, pointer).expect("a look"), None);
    assert_eq!(owners.find(tree.root(), pointer).expect("a look"), None);
    assert_eq!(owners.find(page_id(9_999), pointer).expect("a look"), None);

    // While a write transaction holds a copy of a page, no pointer of it
    // is rewritten (`transactions.md` PV7): the pass moves the values of
    // the other pages, and leaves this one for later.
    let mut writer = db.writer();
    tree.put(&mut writer, &key(20), &big(20)).expect("a put");
    let busy = homes.iter().filter(|(of, _)| *of == page).count() as u64;
    let clean = |to: &mut ValueWriter| {
        let mut manifest = db.manifest.borrow_mut();
        db.values.clean(file, to, &mut manifest, &owners)
    };
    let cleaned = clean(&mut to).expect("a pass");
    assert_eq!(
        (cleaned.moved, cleaned.busy, cleaned.live),
        (60 - busy, busy, busy)
    );
    assert_eq!(db.rewrites().len() as u64, 60 - busy);
    db.roll_back(writer);

    // The values of that page move once nothing holds it.
    let cleaned = clean(&mut to).expect("a pass");
    assert_eq!((cleaned.moved, cleaned.busy, cleaned.live), (busy, 0, 0));
    assert!(cleaned.is_empty());
    let reader = db.reader();
    for n in 0..60 {
        let (page, place) = home(&reader, &tree, &key(n));
        let pointer = place.pointer().expect("placed");
        assert_eq!(page, homes[usize::from(n)].0, "a rewrite moves no entry");
        assert_eq!(pointer.file(), to.file_id());
        assert_eq!(get(&reader, &tree, &key(n)), Some(big(n)));
    }
    // Each rewrite is in the log as a background change of its leaf, from
    // the old pointer to the new one, and the page is owed to a segment
    // again.
    let rewrites = db.rewrites();
    assert_eq!(rewrites.len(), 60);
    for step in &rewrites {
        let from = step.from().pointer().expect("a pointer");
        assert!(homes.contains(&(step.page(), from)) && step.slot().is_none());
        assert_eq!(step.to().file(), to.file_id());
    }
    assert_eq!(db.pool.oldest_unwritten(), Some(version(1)));
    drop(reader);
    db.write_back();
    assert!(db.cold(version(1), |pages| (0..60).all(|n| get(
        pages,
        &tree,
        &key(n)
    ) == Some(big(n)))));
}

impl Db {
    /// Closes the value-log file that write-back appends to, gives
    /// write-back another, and returns the writer of a third, for what a
    /// cleaning pass moves.
    fn close_value_file(&self) -> ValueWriter {
        let mut manifest = self.manifest.borrow_mut();
        let mut create = || {
            let writer = self.values.create().expect("a value-log file");
            self.values.commit(&mut manifest).expect("named");
            writer
        };
        let (next, to) = (create(), create());
        let old = std::mem::replace(&mut self.state.borrow_mut().value_writer, next);
        old.finish().expect("closed");
        self.values
            .commit(&mut manifest)
            .expect("closed in the manifest");
        to
    }
}

/// The three checks of a pointer rewrite (`transactions.md` PV7), each
/// refused alone.
#[test]
fn a_rewrite_is_refused_for_a_page_that_is_no_longer_as_it_was_looked_at() {
    let db = Db::new();
    let big = |n: u8| vec![n; db.config.max_inline() + 1];
    let key = |n: u8| vec![n; 20];
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    for n in 0..5 {
        tree.put(&mut writer, &key(n), &big(n)).expect("a put");
    }
    db.commit(writer);
    db.write_back();
    let file = home(&db.reader(), &tree, &key(0))
        .1
        .pointer()
        .expect("placed")
        .file();
    let mut to = db.close_value_file();
    let owners = FlatOwners::new(db.loader(), &db.pool, db.config, &db);
    let copy = |to: &mut ValueWriter| {
        let copies = db.values.copy_live(file, to, &owners).expect("copied");
        to.sync().expect("synced");
        db.values
            .commit(&mut db.manifest.borrow_mut())
            .expect("covered");
        copies
    };

    // A commit supersedes the page between the look and the rewrite: the
    // values wait for the next pass.
    let copies = copy(&mut to);
    assert_eq!(copies.len(), 5);
    let mut writer = db.writer();
    tree.put(&mut writer, &key(9), b"another entry")
        .expect("a put");
    db.commit(writer);
    let cleaned = db.values.install(copies, &owners).expect("a pass");
    assert_eq!(
        (cleaned.moved, cleaned.busy, cleaned.gone, cleaned.live),
        (0, 5, 0, 5)
    );

    // A commit gives one entry another value, and removes another: their
    // old values have no owner any more, and the others move.
    db.write_back();
    let mut writer = db.writer();
    tree.put(&mut writer, &key(1), &big(11)).expect("a put");
    assert!(tree.delete(&mut writer, &key(2)).expect("a delete"));
    db.commit(writer);
    db.write_back();
    let copies = copy(&mut to);
    assert_eq!(copies.len(), 3);
    let cleaned = db.values.install(copies, &owners).expect("a pass");
    assert_eq!(
        (cleaned.moved, cleaned.busy, cleaned.unowned, cleaned.live),
        (3, 0, 2, 0)
    );

    // A commit frees the page between the look and the rewrite: the owner
    // is gone.
    let last = db.state.borrow().value_writer.file_id();
    let moved = to.file_id();
    let copies = {
        let mut manifest = db.manifest.borrow_mut();
        to.finish().expect("closed");
        db.values
            .commit(&mut manifest)
            .expect("closed in the manifest");
        let mut next = db.values.create().expect("a value-log file");
        db.values.commit(&mut manifest).expect("named");
        let copies = db
            .values
            .copy_live(moved, &mut next, &owners)
            .expect("copied");
        next.sync().expect("synced");
        db.values.commit(&mut manifest).expect("covered");
        copies
    };
    assert_eq!(copies.len(), 3);
    let mut writer = db.writer();
    for n in 0..40u8 {
        tree.put(
            &mut writer,
            &[vec![100 + n], vec![b'k'; 400]].concat(),
            b"more",
        )
        .expect("a put");
    }
    db.commit(writer);
    db.write_back();
    let mut writer = db.writer();
    tree.delete_range(&mut writer, &key(0), Some(&key(5)))
        .expect("a range delete");
    db.commit(writer);
    let cleaned = db.values.install(copies, &owners);
    let cleaned = match cleaned {
        Ok(cleaned) => cleaned,
        Err(ValueStoreError::OwnersNotFound { cleaned, .. }) => cleaned,
        Err(other) => panic!("{other}"),
    };
    assert_eq!(cleaned.moved, 0);
    assert_eq!(cleaned.gone + cleaned.busy, 3);
    assert_eq!(db.values.count(moved).map(|count| count.values), Some(0));
    assert_eq!(db.values.count(last).map(|count| count.values), Some(0));
    assert_eq!(get(&db.reader(), &tree, &key(1)), None);
}

/// A page that the pool does not hold is loaded for its copy, and the copy
/// is made while what was loaded is held.
#[test]
fn a_writer_copies_a_page_that_the_pool_does_not_hold() {
    let db = Db::new();
    let big = |n: u8| vec![n; db.config.max_inline() + 1];
    let key = |n: u8| [vec![n], vec![b'k'; 400]].concat();
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    for n in 0..60 {
        tree.put(&mut writer, &key(n), &big(n)).expect("a put");
    }
    let first = db.commit(writer);
    db.write_back();

    // Another pool over the same segments: every page is a miss.
    let pool = BufferPool::new(PoolConfig {
        page_size: db.size,
        capacity: 4 << 20,
    })
    .expect("a pool");
    let loader = PageLoader::new(&pool, &db.table, &db.segments, db.size).expect("a loader");
    let base = Committed::new(loader, &db.values, first);
    let mut writer = Private::new(base, &pool, OwnerId::new(1), &db.ids);
    assert_eq!(pool.stats().misses, 0);
    for n in (0..60).step_by(2) {
        assert!(tree.delete(&mut writer, &key(n)).expect("a delete"));
    }
    tree.put(&mut writer, &key(1), &big(100)).expect("a put");
    assert!(pool.stats().misses > 0);
    for n in (3..60).step_by(2) {
        assert_eq!(get(&writer, &tree, &key(n)), Some(big(n)));
    }
    assert_eq!(get(&writer, &tree, &key(1)), Some(big(100)));
    assert_eq!(scan_all(&writer, &tree).len(), 30);
    // The pointers of the values that left, and of those that a merge
    // carried, are released, each once, in the order of the value log.
    let released = writer.released().to_vec();
    assert!(released.len() >= 31, "{} pointers", released.len());
    assert!(released.is_sorted_by(|a, b| (a.file(), a.offset()) < (b.file(), b.offset())));
    let parts = writer.into_parts();
    for (copy, created) in parts.written.iter().map(|(copy, created)| (copy, *created)) {
        pool.discard(copy).expect("discarded");
        if created {
            db.ids.release(copy.page()).expect("released");
        }
    }
    parts
        .freed
        .iter()
        .for_each(|copy| pool.discard(copy).expect("discarded"));
    assert_eq!(scan_all(&db.reader(), &tree).len(), 60);
}

#[test]
fn a_writer_keeps_the_pages_it_frees_and_refuses_them() {
    let db = Db::new();
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    let key = |n: u8| [vec![n], vec![b'k'; 400]].concat();
    for n in 0..60 {
        tree.put(&mut writer, &key(n), &[n; 30]).expect("a put");
    }
    db.commit(writer);
    db.write_back();
    assert_eq!(db.reader().at(), version(1));

    // A committed page that the writer frees is kept from page steps until
    // the commit, and is no page for the writer any more.
    let mut writer = db.writer();
    assert_eq!(writer.base().at(), version(1));
    let (_, first) = tree.descend(&writer, &key(0)).expect("a leaf");
    tree.delete_range(&mut writer, b"", Some(&key(30)))
        .expect("a range delete");
    let freed: Vec<PageId> = writer.freed().collect();
    assert!(freed.contains(&first));
    assert!(matches!(writer.page(first), Err(FlatError::Freed { page }) if page == first));
    assert!(matches!(writer.take(first), Err(FlatError::Freed { .. })));
    assert!(matches!(
        writer.write(first, &[]),
        Err(FlatError::Freed { .. })
    ));
    assert!(matches!(writer.free(first), Err(FlatError::Freed { .. })));
    assert_eq!(db.pool.page_step(first).private_copies(), 1);
    assert_eq!(scan_all(&writer, &tree).len(), 30);

    // A page the writer made and freed again is gone at once, and its id
    // with it.
    let made = writer.allocate().expect("a page");
    assert_eq!(db.ids.state(made), PageState::Private);
    writer.free(made).expect("freed");
    assert_eq!(db.ids.state(made), PageState::Free);
    assert_eq!(db.pool.page_step(made).private_copies(), 0);

    // An image that is longer than a frame, and a pending value whose
    // bytes nobody gave, are refused.
    let root = tree.root();
    let long = vec![0; db.size.get() + 1];
    assert!(matches!(
        writer.write(root, &long),
        Err(FlatError::Damaged { .. })
    ));
    let orphan = PendingValue::new([7; 8], 3_000).expect("a pending value");
    let mut leaf =
        lignin_format::flat_page::FlatLeafBuilder::new(root, b"", 8_192).expect("a leaf");
    let place = ValuePlace::Pending(orphan).encode();
    leaf.push(b"key", FlatValue::OutOfLine(&place))
        .expect("an entry");
    let orphaned = writer.write(root, &leaf.finish());
    assert!(matches!(orphaned, Err(FlatError::NoValue { page }) if page == root));
    let mut out = Vec::new();
    let unread = writer.read_value(root, 0, ValuePlace::Pending(orphan), &mut out);
    assert!(matches!(unread, Err(FlatError::NoValue { .. })));
    let uncarried = writer.carry_value(root, 0, made, ValuePlace::Pending(orphan));
    assert!(matches!(uncarried, Err(FlatError::NoValue { .. })));
    db.roll_back(writer);
    assert_eq!(db.pool.page_step(first).private_copies(), 0);

    // A reader asks for a pending value of a page that has none attached.
    let reader = db.reader();
    let unread = reader.read_value(first, 0, ValuePlace::Pending(orphan), &mut out);
    assert!(matches!(unread, Err(FlatError::NoValue { .. })));
    assert_eq!(scan_all(&reader, &tree).len(), 60);
}
