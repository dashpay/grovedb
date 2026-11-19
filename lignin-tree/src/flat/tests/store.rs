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
    alloc::{AllocError, PageAllocator, PageState},
    cache::{BufferPool, CacheError, OwnerId, PageRef, PoolConfig, ValueId},
    load::{LoadError, PageLoader, FRAME_FORMAT},
    segment::{ManifestStore, SegmentStore, SegmentWriter},
    table::{Link, PageTable},
    values::{
        OwnersError, ValueChange, ValueHold, ValueOwners, ValueStore, ValueStoreError, ValueWriter,
    },
};
use tempfile::TempDir;

use super::{
    super::{Committed, FlatOwners, PageIds, Private, StepLog},
    *,
};
use crate::OperationGuard;

/// The guard of a reader's operation, with a number to know it by. It
/// keeps nothing here: nothing is reclaimed in these tests while a reader
/// reads.
struct Guard(u8);

impl OperationGuard for Guard {}

type Reader<'s> = Committed<'s, 's, Guard>;
type Writer<'s> = Private<'s, 's, Guard, &'s PageAllocator>;

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
    fn reader_at(&self, at: CommitVersion) -> Reader<'_> {
        Committed::new(self.loader(), &self.values, &Guard(0), at)
    }

    fn reader(&self) -> Reader<'_> {
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
    fn cold<R>(&self, at: CommitVersion, read: impl FnOnce(&Reader<'_>) -> R) -> R {
        let pool = BufferPool::new(PoolConfig {
            page_size: self.size,
            capacity: 4 << 20,
        })
        .expect("a pool");
        let loader = PageLoader::new(&pool, &self.table, &self.segments, self.size);
        read(&Committed::new(
            loader.expect("a loader"),
            &self.values,
            &Guard(0),
            at,
        ))
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
        for page in parts.unused {
            self.ids.release(page).expect("released");
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
    fn roll_back(&self, mut writer: Writer<'_>) {
        writer.roll_back().expect("rolled back");
        assert_eq!(writer.written().chain(writer.freed()).count(), 0);
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
            for (_, pending) in pending_of(&frame) {
                let bytes = frame.attached(pending.id()).expect("the value's bytes");
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
            assert!(pending_of(&old).iter().any(|&(_, held)| held == pending));
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
            blank.detach(pending.id()).expect("attached");
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

    // The values are moved twice from one look: the second move finds
    // every page as it was looked at, with another pointer in the entry.
    let moved = to.file_id();
    let mut next = {
        let mut manifest = db.manifest.borrow_mut();
        to.finish().expect("closed");
        db.values
            .commit(&mut manifest)
            .expect("closed in the manifest");
        let next = db.values.create().expect("a value-log file");
        db.values.commit(&mut manifest).expect("named");
        next
    };
    let mut copy = |file| {
        let copies = db.values.copy_live(file, &mut next, &owners);
        next.sync().expect("synced");
        db.values
            .commit(&mut db.manifest.borrow_mut())
            .expect("covered");
        copies.expect("copied")
    };
    let (first, second) = (copy(moved), copy(moved));
    assert_eq!((first.len(), second.len()), (3, 3));
    let cleaned = db.values.install(first, &owners).expect("a pass");
    assert_eq!((cleaned.moved, cleaned.busy, cleaned.gone), (3, 0, 0));
    let cleaned = db.values.install(second, &owners).expect("a pass");
    assert_eq!((cleaned.moved, cleaned.busy, cleaned.gone), (0, 0, 3));
    assert_eq!(db.values.count(moved).map(|count| count.values), Some(0));

    // A commit frees the page between the look and the rewrite: the owner
    // is gone. The values are in a leaf below the root by then, carried
    // there by a split.
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
    let (leaf, place) = home(&db.reader(), &tree, &key(0));
    assert_ne!(leaf, tree.root());
    let carried = place.pointer().expect("placed").file();
    drop(db.close_value_file());
    let copies = copy(carried);
    assert_eq!(copies.len(), 4);
    let mut writer = db.writer();
    tree.delete_range(&mut writer, b"", Some(&[120]))
        .expect("a range delete");
    assert!(writer.freed().any(|page| page == leaf));
    db.commit(writer);
    let cleaned = db.values.install(copies, &owners).expect("a pass");
    assert_eq!((cleaned.moved, cleaned.busy, cleaned.gone), (0, 0, 4));
    assert_eq!(db.values.count(carried).map(|count| count.values), Some(0));
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
    let base = Committed::new(loader, &db.values, &Guard(0), first);
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
    assert!(pool.census().private > 0);
    writer.roll_back().expect("rolled back");
    assert_eq!(pool.census().private, 0);
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

    // A page the writer made and freed again is gone at once. Its id is
    // the writer's until the writer ends, so that nobody else is given it
    // while a rollback could still bring the page back.
    let made = writer.allocate().expect("a page");
    assert_eq!(db.ids.state(made), PageState::Private);
    writer.free(made).expect("freed");
    assert_eq!(db.ids.state(made), PageState::Private);
    assert_eq!(db.pool.page_step(made).private_copies(), 0);
    assert!(matches!(writer.page(made), Err(FlatError::Freed { page }) if page == made));
    assert!(!writer.written().any(|page| page == made));
    assert!(!writer.freed().any(|page| page == made));

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
    let unread = {
        let held = writer.page(root).expect("the root");
        writer.read_value(&held, root, 0, ValuePlace::Pending(orphan), &mut out)
    };
    assert!(matches!(unread, Err(FlatError::NoValue { .. })));
    let uncarried = writer.carry_value(root, 0, made, ValuePlace::Pending(orphan));
    assert!(matches!(uncarried, Err(FlatError::NoValue { .. })));
    db.roll_back(writer);
    assert_eq!(db.pool.page_step(first).private_copies(), 0);
    assert_eq!(db.ids.state(made), PageState::Free);

    // A reader asks for a pending value of a page that has none attached.
    let reader = db.reader();
    let held = reader.page(first).expect("a page");
    let unread = reader.read_value(&held, first, 0, ValuePlace::Pending(orphan), &mut out);
    assert!(matches!(unread, Err(FlatError::NoValue { .. })));
    assert_eq!(scan_all(&reader, &tree).len(), 60);
}

/// A reader that holds a leaf reads the pending values of its entries
/// from what it holds: write-back gives the page another frame, with the
/// pointer in the entry and the bytes no longer attached, and takes
/// nothing from the frame the reader holds (`transactions.md` FL7).
#[test]
fn a_reader_reads_a_pending_value_from_the_page_it_holds() {
    let db = Db::new();
    let big = |n: u8| vec![n; db.config.max_inline() + 1 + n as usize];
    let key = |n: u8| vec![b'k', n];
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    for n in 0..6 {
        tree.put(&mut writer, &key(n), &big(n)).expect("a put");
    }
    db.commit(writer);

    let reader = db.reader();
    let root = tree.root();
    let held = reader.page(root).expect("the root");
    let places: Vec<(usize, ValuePlace)> = pending_of(&held)
        .into_iter()
        .map(|(slot, pending)| (slot, ValuePlace::Pending(pending)))
        .collect();
    assert_eq!(places.len(), 6);
    db.write_back();
    let now = reader.page(root).expect("the root");
    assert!(pending_of(&now).is_empty() && pending_of(&held).len() == 6);
    for &(slot, place) in &places {
        let mut out = Vec::new();
        reader
            .read_value(&held, root, slot, place, &mut out)
            .expect("the value of the page that is held");
        assert_eq!(out, big(slot as u8));
        // The page as it is now has no such value.
        let gone = reader.read_value(&now, root, slot, place, &mut out);
        assert!(matches!(gone, Err(FlatError::NoValue { page }) if page == root));
    }
    for n in 0..6 {
        assert_eq!(get(&reader, &tree, &key(n)), Some(big(n)));
    }
}

/// A writer changes the copies it made, and no others: a copy that the
/// pool has under the writer's name from elsewhere is refused, and a copy
/// that cannot be made whole is not left behind.
#[test]
fn a_writer_owns_the_copies_it_made_and_no_others() {
    let db = Db::new();
    let big = |n: u8| vec![n; db.config.max_inline() + 1];
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    tree.put(&mut writer, b"a", &big(1)).expect("a put");
    db.commit(writer);
    let root = tree.root();

    let mut first = db.writer();
    tree.put(&mut first, b"b", b"first").expect("a put");
    let owner = OwnerId::new(db.state.borrow().owners);
    let mut second = Private::new(db.reader(), &db.pool, owner, &db.ids);
    let refused = tree.put(&mut second, b"c", b"second");
    assert!(matches!(
        refused,
        Err(FlatError::Cache(CacheError::AlreadyPrivate { page, .. })) if page == root
    ));
    // The writer that was refused is given up, and takes nothing along.
    second.roll_back().expect("rolled back");
    assert_eq!(db.pool.page_step(root).private_copies(), 1);
    assert_eq!(get(&first, &tree, b"b"), Some(b"first".to_vec()));
    db.commit(first);
    assert_eq!(get(&db.reader(), &tree, b"b"), Some(b"first".to_vec()));

    // A page whose pending value has bytes of another length with it, and
    // then none: the copy is of no use, and does not stay in the way of
    // page steps.
    for bytes in [Some(Arc::from(&[0u8; 5][..])), None] {
        {
            let step = db.pool.page_step(root);
            let old = step.current().expect("the page");
            let name = pending_of(&old)[0].1.id();
            let mut blank = db.pool.blank();
            blank.copy_from(&old);
            blank.detach(name).expect("attached");
            if let Some(bytes) = bytes {
                blank.attach(name, bytes);
            }
            step.replace(&old, blank).expect("replaced");
        }
        let mut writer = db.writer();
        let lost = tree.put(&mut writer, b"d", b"more");
        assert!(matches!(lost, Err(FlatError::NoValue { page }) if page == root));
        assert_eq!(db.pool.page_step(root).private_copies(), 0);
        writer.roll_back().expect("rolled back");
        assert_eq!(db.pool.census().private, 0);
    }
}

/// Two entries of a leaf that hold one pending value, of two lengths: the
/// bytes with the page are the value of the entry whose length they have,
/// and the writer takes them for neither.
#[test]
fn a_writer_refuses_a_leaf_whose_entries_share_a_pending_value() {
    let db = Db::new();
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    tree.put(&mut writer, b"a", b"small").expect("a put");
    db.commit(writer);
    let root = tree.root();
    {
        let place = |len| {
            let pending = PendingValue::new([9; 8], len).expect("a pending value");
            ValuePlace::Pending(pending).encode()
        };
        let mut leaf =
            lignin_format::flat_page::FlatLeafBuilder::new(root, b"", 4_096).expect("a leaf");
        leaf.push(b"a", FlatValue::OutOfLine(&place(2_000)))
            .expect("an entry");
        leaf.push(b"b", FlatValue::OutOfLine(&place(3_000)))
            .expect("an entry");
        let image = leaf.finish();
        let step = db.pool.page_step(root);
        let old = step.current().expect("the page");
        let mut blank = db.pool.blank();
        blank.copy_from(&old);
        blank.fill(0);
        blank[..image.len()].copy_from_slice(&image);
        blank.attach([9; 8], Arc::from(vec![1; 2_000]));
        step.replace(&old, blank).expect("replaced");
    }
    // A reader reads the bytes for the entry that says their length.
    assert_eq!(get(&db.reader(), &tree, b"a"), Some(vec![1; 2_000]));
    let mut out = Vec::new();
    let other = tree.get(&db.reader(), b"b", &mut out);
    assert!(matches!(other, Err(FlatError::NoValue { page }) if page == root));
    let mut writer = db.writer();
    let shared = tree.put(&mut writer, b"c", b"more");
    assert!(matches!(
        shared,
        Err(FlatError::Damaged { page, what }) if page == root
            && what == "two entries that hold one pending value"
    ));
    assert_eq!(db.pool.page_step(root).private_copies(), 0);
    writer.roll_back().expect("rolled back");
}

/// Ids that a test hands out, and the ids that came back.
struct Given<'a> {
    next: Option<PageId>,
    released: &'a RefCell<Vec<PageId>>,
}

impl PageIds for Given<'_> {
    fn allocate(&mut self) -> Result<PageId, AllocError> {
        self.next.take().ok_or(AllocError::Exhausted)
    }

    fn release(&mut self, page: PageId) -> Result<(), AllocError> {
        self.released.borrow_mut().push(page);
        Ok(())
    }
}

/// An id and a copy leave a writer together or not at all: an id whose
/// page could not be made goes back when the writer ends, and an id whose
/// copy the pool would not discard stays reserved.
#[test]
fn a_writer_gives_back_an_id_only_with_its_copy() {
    let db = Db::new();
    let owner = OwnerId::new(7);

    // The pool has a copy of the page that the id names, from elsewhere.
    let released = RefCell::new(Vec::new());
    let page = page_id(900);
    let foreign = db.pool.new_private(owner, page).expect("a copy");
    let ids = Given {
        next: Some(page),
        released: &released,
    };
    let mut writer = Private::new(db.reader(), &db.pool, owner, ids);
    let refused = writer.allocate();
    assert!(matches!(
        refused,
        Err(FlatError::Cache(CacheError::AlreadyPrivate { .. }))
    ));
    assert!(matches!(writer.allocate(), Err(FlatError::Alloc(_))));
    assert_eq!(writer.written().count(), 0);
    writer.roll_back().expect("rolled back");
    assert_eq!(*released.borrow(), [page]);
    db.pool.discard(&foreign).expect("discarded");

    // A copy that is latched elsewhere cannot be discarded: the page stays
    // the writer's, with its id.
    let mut writer = db.writer();
    let owner = OwnerId::new(db.state.borrow().owners);
    let made = writer.allocate().expect("a page");
    let other = db.pool.private(owner, made).expect("the copy");
    let latch = other.read();
    let held = writer.free(made);
    assert!(matches!(
        held,
        Err(FlatError::Cache(CacheError::Latched { .. }))
    ));
    assert!(writer.written().any(|page| page == made));
    drop(latch);
    writer.free(made).expect("freed");
    assert!(!writer.written().any(|page| page == made));
    assert_eq!(db.ids.state(made), PageState::Private);

    // The same at the end of the writer, for a page it made and for one it
    // copied: every other id goes back, and what stayed goes back with the
    // next try, once.
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    db.commit(writer);
    let mut writer = db.writer();
    let owner = OwnerId::new(db.state.borrow().owners);
    tree.put(&mut writer, b"key", b"value").expect("a put");
    let (kept, gone) = (
        writer.allocate().expect("a page"),
        writer.allocate().expect("a page"),
    );
    let others = [kept, tree.root()].map(|page| db.pool.private(owner, page).expect("the copy"));
    let latches = others.each_ref().map(|other| other.read());
    for _ in 0..2 {
        let held = writer.roll_back();
        assert!(matches!(
            held,
            Err(FlatError::Cache(CacheError::Latched { .. }))
        ));
        assert_eq!(db.ids.state(gone), PageState::Free);
        assert_eq!(db.ids.state(kept), PageState::Private);
        let mut left = [tree.root(), kept];
        left.sort();
        assert_eq!(writer.written().collect::<Vec<_>>(), left);
        assert_eq!(db.pool.census().private, 2);
    }
    drop(latches);
    writer.roll_back().expect("rolled back");
    assert_eq!(db.ids.state(kept), PageState::Free);
    assert_eq!(db.pool.census().private, 0);
    assert_eq!(writer.written().count(), 0);
    // A writer that was given up whole is as a new one.
    writer.roll_back().expect("nothing left");
    tree.put(&mut writer, b"key", b"value").expect("a put");
    db.commit(writer);
    assert_eq!(get(&db.reader(), &tree, b"key"), Some(b"value".to_vec()));
}

/// A group is written for one place (`log-format.md` LG22). A commit that
/// wrote its group for a place it did not get writes it again, and the
/// group it appends holds every value of the writer, new and carried, at
/// the addresses its pages name.
#[test]
fn a_group_that_is_written_again_holds_the_values_of_its_writer_again() {
    let db = Db::new();
    let big = |n: u8| vec![n; db.config.max_inline() + 100 + usize::from(n)];
    let key = |n: u8| [vec![n], vec![b'k'; 400]].concat();
    // Writes the writer's group for a place far from the one it will get,
    // and leaves it at that.
    let abandon = |writer: &mut Writer<'_>| {
        let version = version(db.state.borrow().latest + 1);
        let elsewhere = LogStart::FIRST.address(1 << 30).expect("an address");
        let mut bytes = Vec::new();
        let mut group = GroupWriter::new(version, elsewhere, &mut bytes).expect("a group");
        writer.log_values(&mut group).expect("the values");
        for page in writer.written().collect::<Vec<_>>() {
            writer.log_page(page, &mut group).expect("a run");
        }
    };

    // Four values in the value log, and one that is pending in the root
    // from a commit of its own.
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    let root = tree.root();
    for n in 0..4 {
        tree.put(&mut writer, &key(n), &big(n)).expect("a put");
    }
    db.commit(writer);
    db.write_back();
    let old: Vec<ValuePointer> = (0..4)
        .map(|n| {
            home(&db.reader(), &tree, &key(n))
                .1
                .pointer()
                .expect("placed")
        })
        .collect();
    let mut writer = db.writer();
    tree.put(&mut writer, &key(4), &big(4)).expect("a put");
    db.commit(writer);
    let earlier = home(&db.reader(), &tree, &key(4)).1;
    assert!(earlier.is_pending());

    // A new value beside it: the group holds that one, at the address the
    // entry names, and not the value that an earlier group holds.
    let mut writer = db.writer();
    tree.put(&mut writer, &key(5), &big(5)).expect("a put");
    abandon(&mut writer);
    abandon(&mut writer);
    db.commit(writer);
    let reader = db.reader();
    let (page, place) = home(&reader, &tree, &key(5));
    let held = place.pending().expect("pending");
    let group = db.groups().pop().expect("the group");
    assert_eq!(group.values, [(root, held, big(5))]);
    assert_eq!((page, home(&reader, &tree, &key(4)).1), (root, earlier));
    drop(reader);

    // A split carries all six: four from the value log, and two that are
    // pending, one of them from the group before.
    let mut writer = db.writer();
    for n in 6..24 {
        tree.put(&mut writer, &key(n), b"small").expect("a put");
    }
    abandon(&mut writer);
    let split = db.commit(writer);
    let reader = db.reader();
    let group = db.groups().pop().expect("the group of the split");
    assert_eq!(group.values.len(), 6);
    assert_eq!(group.released, old);
    for n in 0..6 {
        let (page, place) = home(&reader, &tree, &key(n));
        let held = place.pending().expect("pending again");
        assert_ne!(page, root);
        assert!(group.values.contains(&(page, held, big(n))), "value {n}");
        let run = group.runs.iter().find(|(of, _)| *of == page);
        assert!(run.expect("a run").1.contains(&held));
    }
    drop(reader);
    db.write_back();
    assert!(db.cold(split, |pages| (0..6)
        .all(|n| get(pages, &tree, &key(n)) == Some(big(n)))));
}

/// The bytes that are with the frame `frame`, by their names, beside the
/// names of the pending values its leaf holds.
fn attached_and_held(
    frame: &[u8],
    mut attached: Vec<(ValueId, usize)>,
) -> [Vec<(ValueId, usize)>; 2] {
    let mut held: Vec<(ValueId, usize)> = pending_of(frame)
        .into_iter()
        .map(|(_, pending)| (pending.id(), pending.value_len() as usize))
        .collect();
    held.sort_unstable();
    attached.sort_unstable();
    [attached, held]
}

/// The bytes of a pending value are with the frame of its page under the
/// 8 bytes its entry holds: under the writer's name before the commit,
/// under the value's place in the log from the commit on, wherever the
/// entry stands in its leaf, and under nothing once the value is placed.
#[test]
fn the_bytes_of_a_pending_value_are_with_its_page_under_its_name() {
    let db = Db::new();
    let big = |n: u8| vec![n; db.config.max_inline() + 1 + n as usize];
    let key = |n: u8| vec![b'k', n];
    let mut writer = db.writer();
    let owner = OwnerId::new(db.state.borrow().owners);
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    let root = tree.root();
    for n in [4, 2, 6] {
        tree.put(&mut writer, &key(n), &big(n)).expect("a put");
    }
    let names_of = |attachments: Vec<(ValueId, usize)>, frame: &[u8]| {
        let [attached, held] = attached_and_held(frame, attachments);
        assert_eq!(attached, held);
        attached
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
    };
    let of_copy = |owner| {
        let copy = db.pool.private(owner, root).expect("the writer's copy");
        let latch = copy.read();
        let attached = latch.attachments().map(|(name, bytes)| (name, bytes.len()));
        names_of(attached.collect(), &latch)
    };
    let of_page = || {
        let frame = db.reader().page(root).expect("the root");
        let attached = frame.attachments().map(|(name, bytes)| (name, bytes.len()));
        names_of(attached.collect(), &frame)
    };

    // The writer's names are no places in a log.
    let own = of_copy(owner);
    assert_eq!(own.len(), 3);
    assert!(own.iter().all(|name| name[7] & 0x80 != 0));

    // The commit gives each value its place, and the bytes go with it.
    db.commit(writer);
    let logged = of_page();
    assert_eq!(logged.len(), 3);
    assert!(logged.iter().all(|name| !own.contains(name)));
    for n in [2, 4, 6] {
        assert_eq!(get(&db.reader(), &tree, &key(n)), Some(big(n)));
    }

    // Entries come before and between them, and one leaves: the values
    // that stay keep their names, in other positions.
    let mut writer = db.writer();
    let owner = OwnerId::new(db.state.borrow().owners);
    let before = pending_of(&db.reader().page(root).expect("the root"));
    // The writer reads a page it has not taken as a reader does.
    assert_eq!(get(&writer, &tree, &key(6)), Some(big(6)));
    assert_eq!(writer.written().count(), 0);
    tree.put(&mut writer, &key(1), &big(1)).expect("a put");
    tree.put(&mut writer, &key(3), b"small").expect("a put");
    assert!(tree.delete(&mut writer, &key(4)).expect("a delete"));
    let moved = of_copy(owner);
    assert_eq!(moved.len(), 3);
    assert_eq!(moved.iter().filter(|name| logged.contains(name)).count(), 2);
    for n in [1, 2, 6] {
        assert_eq!(get(&writer, &tree, &key(n)), Some(big(n)));
    }
    db.commit(writer);
    let after = pending_of(&db.reader().page(root).expect("the root"));
    let kept: Vec<_> = before
        .iter()
        .filter_map(|&(slot, pending)| {
            let now = after.iter().find(|&&(_, held)| held == pending)?.0;
            Some((slot, now))
        })
        .collect();
    assert_eq!(kept, [(0, 1), (2, 3)]);
    assert_eq!(of_page().len(), 3);
    for n in [1, 2, 6] {
        assert_eq!(get(&db.reader(), &tree, &key(n)), Some(big(n)));
    }

    // Write-back places the values, and nothing is with the page.
    db.write_back();
    assert!(of_page().is_empty());
    for n in [1, 2, 6] {
        assert_eq!(get(&db.reader(), &tree, &key(n)), Some(big(n)));
    }
}

/// A writer goes on in the next operation of its transaction, under that
/// operation's guard, with the copies and the values it holds.
#[test]
fn a_writer_goes_on_under_the_guard_of_its_next_operation() {
    let db = Db::new();
    let big = |n: u8| vec![n; db.config.max_inline() + 1];
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    tree.put(&mut writer, b"a", &big(1)).expect("a put");
    db.commit(writer);
    let at = db.latest();

    let (first, second) = (Guard(1), Guard(2));
    let base = Committed::new(db.loader(), &db.values, &first, at);
    assert_eq!(base.guard().0, 1);
    let owner = OwnerId::new(77);
    let mut writer = Private::new(base, &db.pool, owner, &db.ids);
    tree.put(&mut writer, b"b", &big(2)).expect("a put");
    let revision = writer.revision();

    let mut writer = writer.under(&second);
    assert_eq!(writer.base().guard().0, 2);
    assert_eq!(writer.base().at(), at);
    assert_eq!(writer.revision(), revision);
    assert_eq!(writer.written().collect::<Vec<_>>(), [tree.root()]);
    let shown = format!("{writer:?}");
    assert!(
        shown.starts_with("Private { base: Committed { at: "),
        "{shown}"
    );
    assert_eq!(get(&writer, &tree, b"a"), Some(big(1)));
    assert_eq!(get(&writer, &tree, b"b"), Some(big(2)));
    tree.put(&mut writer, b"c", &big(3)).expect("a put");
    db.commit(writer);
    for (key, n) in [(b"a", 1), (b"b", 2), (b"c", 3)] {
        assert_eq!(get(&db.reader(), &tree, key), Some(big(n)));
    }

    // A reader goes on the same way.
    let reader = Committed::new(db.loader(), &db.values, &first, at).under(&second);
    assert_eq!(reader.guard().0, 2);
    assert_eq!(get(&reader, &tree, b"c"), None);
    assert_eq!(
        format!("{reader:?}"),
        format!("Committed {{ at: {at:?}, .. }}")
    );
}

/// Two writers under one owner id take one page at the same moment, each
/// the only writer for all it knows: the pool gives one of them the copy
/// and refuses the other, so no copy has two writers.
#[test]
fn of_two_writers_under_one_owner_one_is_refused_its_page() {
    let db = Db::new();
    let mut writer = db.writer();
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    tree.put(&mut writer, b"a", b"committed").expect("a put");
    db.commit(writer);
    let (root, at) = (tree.root(), db.latest());
    let owner = OwnerId::new(1_000);
    let (pool, table, segments) = (&db.pool, &db.table, &db.segments);
    let (values, ids, size) = (&db.values, &db.ids, db.size);

    let rounds = if cfg!(miri) { 2 } else { 200 };
    for round in 0..rounds {
        let start = std::sync::Barrier::new(2);
        let put = |value: &'static [u8]| {
            let start = &start;
            move || {
                let loader = PageLoader::new(pool, table, segments, size).expect("a loader");
                let base = Committed::new(loader, values, &Guard(0), at);
                let mut writer = Private::new(base, pool, owner, ids);
                start.wait();
                let put = tree.put(&mut writer, b"b", value);
                // The writer that was given the copy changed it, alone.
                let read = put.is_ok().then(|| get(&writer, &tree, b"b"));
                start.wait();
                writer.roll_back().expect("rolled back");
                (put, read, value)
            }
        };
        let results = std::thread::scope(|scope| {
            let threads = [scope.spawn(put(b"one")), scope.spawn(put(b"other"))];
            threads.map(|thread| thread.join().expect("the thread ends"))
        });
        let mut given = 0;
        for (put, read, value) in results {
            match put {
                Ok(()) => {
                    given += 1;
                    assert_eq!(read, Some(Some(value.to_vec())), "round {round}");
                }
                Err(FlatError::Cache(CacheError::AlreadyPrivate { page, owner: of })) => {
                    assert_eq!((page, of), (root, owner), "round {round}");
                }
                Err(other) => panic!("round {round}: {other}"),
            }
        }
        assert_eq!(given, 1, "round {round}");
        assert_eq!(pool.page_step(root).private_copies(), 0);
    }
    assert_eq!(get(&db.reader(), &tree, b"b"), None);
}

/// An image whose pending values the writer does not hold as its entries
/// say is refused before the copy changes: the copy's bytes, and the bytes
/// that are with its frame, are as they were, so no entry is left with a
/// name that its bytes are not under.
#[test]
fn a_write_whose_values_the_writer_does_not_hold_changes_nothing() {
    let db = Db::new();
    let mut writer = db.writer();
    let owner = OwnerId::new(db.state.borrow().owners);
    let tree = FlatTree::create(&mut writer, db.config).expect("a keyspace");
    let root = tree.root();
    let first = vec![9; db.config.max_inline() + 1];
    tree.put(&mut writer, b"a", &first).expect("a put");
    let one = writer.hold_value(root, &[1; 2_000]).expect("held");
    let two = writer.hold_value(root, &[2; 3_000]).expect("held");
    let image = |values: [PendingValue; 2]| {
        let mut leaf =
            lignin_format::flat_page::FlatLeafBuilder::new(root, b"", db.config.capacity())
                .expect("a leaf");
        for (key, value) in [b"x", b"y"].into_iter().zip(values) {
            let place = ValuePlace::Pending(value).encode();
            leaf.push(key, FlatValue::OutOfLine(&place))
                .expect("an entry");
        }
        leaf.finish()
    };
    // The copy's bytes, and what is with its frame: each name with its
    // bytes.
    let copy = || {
        let copy = db.pool.private(owner, root).expect("the writer's copy");
        let latch = copy.read();
        let mut attached: Vec<(ValueId, Vec<u8>)> = latch
            .attachments()
            .map(|(name, bytes)| (name, bytes.to_vec()))
            .collect();
        attached.sort_unstable();
        let lengths = attached.iter().map(|(name, bytes)| (*name, bytes.len()));
        let [lengths, held] = attached_and_held(&latch, lengths.collect());
        assert_eq!(lengths, held);
        (latch.to_vec(), attached)
    };
    let before = copy();
    assert_eq!(before.1.len(), 1);
    assert_eq!(before.1[0].1, first);

    // The second entry says another length than its value has.
    let shorter = PendingValue::new(two.id(), 2_500).expect("a pending value");
    let refused = writer.write(root, &image([one, shorter]));
    assert!(matches!(refused, Err(FlatError::NoValue { page }) if page == root));
    assert!(copy() == before);
    // Two entries hold one value.
    let refused = writer.write(root, &image([one, one]));
    assert!(matches!(
        refused,
        Err(FlatError::Damaged { page, what }) if page == root
            && what == "two entries that hold one pending value"
    ));
    assert!(copy() == before);
    assert_eq!(get(&writer, &tree, b"a"), Some(first));

    // The image whose entries say what the writer holds is taken, and its
    // commit names both values.
    writer.write(root, &image([one, two])).expect("written");
    let (_, attached) = copy();
    assert_eq!(attached, {
        let mut attached = [(one.id(), vec![1; 2_000]), (two.id(), vec![2; 3_000])];
        attached.sort_unstable();
        attached
    });
    db.commit(writer);
    assert_eq!(get(&db.reader(), &tree, b"x"), Some(vec![1; 2_000]));
    assert_eq!(get(&db.reader(), &tree, b"y"), Some(vec![2; 3_000]));
}
