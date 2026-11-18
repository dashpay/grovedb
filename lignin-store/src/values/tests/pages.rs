//! A model of what sits above the value log, for the tests: pages of cells
//! that own values, their versions, write transactions with private copies,
//! version pins, and a journal that makes pages and counts durable together.
//!
//! The real ones are the tree layer's cells (TREE-07, TREE-08), the page
//! store's page versions (STORE-03, STORE-05) and the log (STORE-01). The
//! model keeps the rules they keep towards the value log, and nothing else
//! of them. A page with an even id is a flat leaf, as a real image of the
//! flat page format; one with an odd id is a list of cells whose order
//! changes on every write, as slots do.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use lignin_format::{
    flat_page::{FlatLeafBuilder, FlatPage, FlatValue},
    header::{read_record, write_record},
    value_log::ValuePointer,
    CommitVersion, FileId, FormatVersion, PageId,
};
use lignin_io::{sim::SimVfs, Create, OpenMode, VfsFile};

use super::{sim::*, *};

const JOURNAL: &str = "pages.journal";
const JOURNAL_TEMP: &str = "pages.journal.tmp";
const MAX_RECORD_LEN: usize = 1 << 24;

/// What the database holds, as its users see it: each key's page and value.
pub(super) type Logical = BTreeMap<u32, (PageId, Vec<u8>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cell {
    pub(super) key: u32,
    pub(super) pointer: ValuePointer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Page {
    Tree(Vec<Cell>),
    Flat(Vec<u8>),
}

pub(super) fn is_flat(id: PageId) -> bool {
    id.get().is_multiple_of(2)
}

impl Page {
    fn build(id: PageId, mut cells: Vec<Cell>) -> Self {
        if is_flat(id) {
            cells.sort_by_key(|cell| cell.key);
            let mut builder = FlatLeafBuilder::new(b"", FlatPage::MAX_LEN).expect("a builder");
            for cell in cells {
                let pointer = cell.pointer.encode();
                builder
                    .push(&cell.key.to_be_bytes(), FlatValue::OutOfLine(&pointer))
                    .expect("the entry fits");
            }
            Self::Flat(builder.finish())
        } else {
            cells.rotate_left(1);
            Self::Tree(cells)
        }
    }

    fn cells(&self) -> Vec<Cell> {
        match self {
            Self::Tree(cells) => cells.clone(),
            Self::Flat(image) => FlatPage::decode(image, FormatVersion::CURRENT)
                .expect("a flat page")
                .leaf()
                .expect("a leaf")
                .iter()
                .map(|(key, value)| {
                    let FlatValue::OutOfLine(pointer) = value else {
                        panic!("the model stores only value pointers");
                    };
                    let key: [u8; 4] = key.to_vec().try_into().expect("a 4-byte key");
                    let pointer: [u8; 16] = pointer.try_into().expect("a 16-byte pointer");
                    Cell {
                        key: u32::from_be_bytes(key),
                        pointer: ValuePointer::decode(pointer).expect("a valid pointer"),
                    }
                })
                .collect(),
        }
    }

    fn holds(&self, pointer: ValuePointer) -> bool {
        match self {
            Self::Tree(cells) => cells.iter().any(|cell| cell.pointer == pointer),
            Self::Flat(image) => FlatPage::decode(image, FormatVersion::CURRENT)
                .ok()
                .and_then(FlatPage::leaf)
                .is_some_and(|leaf| pointer.find_in_flat_leaf(&leaf).is_some()),
        }
    }
}

#[derive(Debug, Clone)]
struct PageVersion {
    born: u64,
    superseded: Option<u64>,
    page: Page,
}

#[derive(Debug, Clone, Default)]
struct Pages(BTreeMap<PageId, Vec<PageVersion>>);

impl Pages {
    fn current_version(&self, id: PageId) -> Option<&PageVersion> {
        let version = self.0.get(&id)?.last()?;
        version.superseded.is_none().then_some(version)
    }

    fn current(&self, id: PageId) -> Option<&Page> {
        self.current_version(id).map(|version| &version.page)
    }

    fn current_cells(&self, id: PageId) -> Vec<Cell> {
        self.current(id).map(Page::cells).unwrap_or_default()
    }

    fn visible(&self, id: PageId, at: u64) -> Option<&Page> {
        self.0
            .get(&id)?
            .iter()
            .find(|version| version.born <= at && version.superseded.is_none_or(|end| at < end))
            .map(|version| &version.page)
    }

    fn install(&mut self, id: PageId, cells: Vec<Cell>, born: u64) {
        let versions = self.0.entry(id).or_default();
        if let Some(last) = versions.last_mut().filter(|last| last.superseded.is_none()) {
            last.superseded = Some(born);
        }
        if !cells.is_empty() {
            versions.push(PageVersion {
                born,
                superseded: None,
                page: Page::build(id, cells),
            });
        }
    }

    fn rewrite_current(&mut self, id: PageId, from: ValuePointer, to: ValuePointer) -> bool {
        let Some(version) = self.0.get_mut(&id).and_then(|versions| versions.last_mut()) else {
            return false;
        };
        if version.superseded.is_some() {
            return false;
        }
        let mut cells = version.page.cells();
        let Some(cell) = cells.iter_mut().find(|cell| cell.pointer == from) else {
            return false;
        };
        cell.pointer = to;
        version.page = Page::build(id, cells);
        true
    }

    /// The page as a checkpoint copies it, if it ever had a version.
    fn copy(&self, id: PageId) -> Option<PageCopy> {
        let last = self.0.get(&id)?.last()?;
        Some(match last.superseded {
            None => PageCopy {
                id,
                born: last.born,
                cells: last.page.cells(),
            },
            Some(freed) => PageCopy {
                id,
                born: freed,
                cells: Vec::new(),
            },
        })
    }

    fn all_current(&self) -> Vec<(PageId, Vec<Cell>)> {
        self.0
            .keys()
            .map(|&id| (id, self.current_cells(id)))
            .filter(|(_, cells)| !cells.is_empty())
            .collect()
    }
}

/// One record of the journal: what the log and the page-table checkpoint
/// store of a change point, as far as the value log is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Record {
    /// A checkpoint: a sample of the counts with its place in the journal,
    /// and a copy of every page, each made at a moment of its own at or
    /// after the sample.
    Checkpoint {
        /// The latest version when the counts were sampled.
        version: u64,
        /// How many records the journal held when the counts were sampled:
        /// the counts include those and no later one.
        place: u64,
        counts: Vec<FileCount>,
        pages: Vec<PageCopy>,
    },
    Commit {
        version: u64,
        pages: Vec<(PageId, Vec<Cell>)>,
        change: ValueChange,
    },
    Step {
        page: PageId,
        from: ValuePointer,
        to: ValuePointer,
    },
}

/// A page as a checkpoint copied it: the version that last changed it, and
/// its cells, which are none if that version freed it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PageCopy {
    id: PageId,
    born: u64,
    cells: Vec<Cell>,
}

impl Record {
    /// A checkpoint of `pages` as they all are at `version`, with the counts
    /// of that moment, at the start of a journal.
    fn start(version: u64, pages: Vec<(PageId, Vec<Cell>)>, counts: Vec<FileCount>) -> Self {
        let pages = pages
            .into_iter()
            .map(|(id, cells)| PageCopy {
                id,
                born: version,
                cells,
            })
            .collect();
        Self::Checkpoint {
            version,
            place: 0,
            counts,
            pages,
        }
    }
}

fn put_pointers(out: &mut Vec<u8>, pointers: &[ValuePointer]) {
    out.extend_from_slice(&(pointers.len() as u32).to_le_bytes());
    for pointer in pointers {
        out.extend_from_slice(&pointer.encode());
    }
}

fn put_cells(out: &mut Vec<u8>, cells: &[Cell]) {
    out.extend_from_slice(&(cells.len() as u32).to_le_bytes());
    for cell in cells {
        out.extend_from_slice(&cell.key.to_le_bytes());
        out.extend_from_slice(&cell.pointer.encode());
    }
}

fn put_pages(out: &mut Vec<u8>, pages: &[(PageId, Vec<Cell>)]) {
    out.extend_from_slice(&(pages.len() as u32).to_le_bytes());
    for (id, cells) in pages {
        out.extend_from_slice(&id.encode());
        put_cells(out, cells);
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_le_bytes)
    }

    fn page(&mut self) -> Option<PageId> {
        PageId::decode(self.take()?)
    }

    fn pointer(&mut self) -> Option<ValuePointer> {
        ValuePointer::decode(self.take()?).ok()
    }

    fn pointers(&mut self) -> Option<Vec<ValuePointer>> {
        (0..self.u32()?).map(|_| self.pointer()).collect()
    }

    fn cells(&mut self) -> Option<Vec<Cell>> {
        (0..self.u32()?)
            .map(|_| {
                Some(Cell {
                    key: self.u32()?,
                    pointer: self.pointer()?,
                })
            })
            .collect()
    }

    fn pages(&mut self) -> Option<Vec<(PageId, Vec<Cell>)>> {
        (0..self.u32()?)
            .map(|_| Some((self.page()?, self.cells()?)))
            .collect()
    }

    fn copies(&mut self) -> Option<Vec<PageCopy>> {
        (0..self.u32()?)
            .map(|_| {
                Some(PageCopy {
                    id: self.page()?,
                    born: self.u64()?,
                    cells: self.cells()?,
                })
            })
            .collect()
    }
}

impl Record {
    fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        match self {
            Self::Checkpoint {
                version,
                place,
                counts,
                pages,
            } => {
                payload.push(1);
                payload.extend_from_slice(&version.to_le_bytes());
                payload.extend_from_slice(&place.to_le_bytes());
                payload.extend_from_slice(&(counts.len() as u32).to_le_bytes());
                for count in counts {
                    payload.extend_from_slice(&count.file.encode());
                    payload.extend_from_slice(&count.values.to_le_bytes());
                    payload.extend_from_slice(&count.bytes.to_le_bytes());
                    payload.push(u8::from(count.miscounted));
                }
                payload.extend_from_slice(&(pages.len() as u32).to_le_bytes());
                for copy in pages {
                    payload.extend_from_slice(&copy.id.encode());
                    payload.extend_from_slice(&copy.born.to_le_bytes());
                    put_cells(&mut payload, &copy.cells);
                }
            }
            Self::Commit {
                version,
                pages,
                change,
            } => {
                payload.push(2);
                payload.extend_from_slice(&version.to_le_bytes());
                put_pages(&mut payload, pages);
                put_pointers(&mut payload, change.acquired());
                put_pointers(&mut payload, change.released());
            }
            Self::Step { page, from, to } => {
                payload.push(3);
                payload.extend_from_slice(&page.encode());
                payload.extend_from_slice(&from.encode());
                payload.extend_from_slice(&to.encode());
            }
        }
        let mut framed = Vec::new();
        write_record(&payload, &mut framed).expect("the record fits a frame");
        framed
    }

    fn decode(payload: &[u8]) -> Option<Self> {
        let mut reader = Reader(payload);
        let [tag] = reader.take()?;
        let record = match tag {
            1 => Self::Checkpoint {
                version: reader.u64()?,
                place: reader.u64()?,
                counts: (0..reader.u32()?)
                    .map(|_| {
                        Some(FileCount {
                            file: FileId::decode(reader.take()?)?,
                            values: reader.u64()?,
                            bytes: reader.u64()?,
                            miscounted: reader.take::<1>()? == [1],
                        })
                    })
                    .collect::<Option<_>>()?,
                pages: reader.copies()?,
            },
            2 => {
                let (version, pages) = (reader.u64()?, reader.pages()?);
                let mut change = ValueChange::new();
                reader
                    .pointers()?
                    .into_iter()
                    .for_each(|pointer| change.acquire(pointer));
                reader
                    .pointers()?
                    .into_iter()
                    .for_each(|pointer| change.release(pointer));
                Self::Commit {
                    version,
                    pages,
                    change,
                }
            }
            3 => Self::Step {
                page: reader.page()?,
                from: reader.pointer()?,
                to: reader.pointer()?,
            },
            _ => return None,
        };
        reader.0.is_empty().then_some(record)
    }

    /// The records at the start of `bytes`, up to the first one that is not
    /// whole.
    fn read_all(mut bytes: &[u8]) -> Vec<Self> {
        let mut records = Vec::new();
        while let Ok((payload, rest)) = read_record(bytes, MAX_RECORD_LEN) {
            let Some(record) = Self::decode(payload) else {
                break;
            };
            records.push(record);
            bytes = rest;
        }
        records
    }
}

fn journal_path(name: &str) -> PathBuf {
    db_dir().join(name)
}

fn journal_error(op: &'static str, name: &str) -> impl FnOnce(std::io::Error) -> ValueStoreError {
    let path = journal_path(name);
    move |source| ValueStoreError::Io { op, path, source }
}

/// Replaces the journal with one that starts from `checkpoint`, so that a
/// crash leaves the old journal or the new one.
fn start_journal(fs: &SimVfs, checkpoint: &Record) -> Result<Box<dyn VfsFile>, ValueStoreError> {
    let (path, temporary) = (journal_path(JOURNAL), journal_path(JOURNAL_TEMP));
    match fs.remove_file(&temporary) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(journal_error("remove", JOURNAL_TEMP)(error));
        }
        _ => {}
    }
    let file = fs
        .open(&temporary, OpenMode::Write(Create::New))
        .map_err(journal_error("create", JOURNAL_TEMP))?;
    file.write_at(&checkpoint.encode(), 0)
        .map_err(journal_error("write", JOURNAL_TEMP))?;
    file.sync_all()
        .map_err(journal_error("sync", JOURNAL_TEMP))?;
    drop(file);
    fs.rename(&temporary, &path)
        .map_err(journal_error("rename", JOURNAL_TEMP))?;
    fs.sync_dir(db_dir())
        .map_err(journal_error("sync the directory of", JOURNAL))?;
    fs.open(&path, OpenMode::Append(Create::Never))
        .map_err(journal_error("open", JOURNAL))
}

/// The records of the journal. A database whose creation a crash cut short
/// has a manifest and no journal yet, which is the journal of an empty one.
fn read_journal(fs: &SimVfs) -> Result<Vec<Record>, ValueStoreError> {
    let file = match fs.open(&journal_path(JOURNAL), OpenMode::Read) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(vec![Record::start(0, Vec::new(), Vec::new())]);
        }
        Err(error) => return Err(journal_error("open", JOURNAL)(error)),
    };
    let len = file.len().map_err(journal_error("measure", JOURNAL))?;
    let mut bytes = vec![0; len as usize];
    file.read_exact_at(&mut bytes, 0)
        .map_err(journal_error("read", JOURNAL))?;
    Ok(Record::read_all(&bytes))
}

struct Inner {
    pages: Pages,
    latest: u64,
    pins: Vec<u64>,
    private: BTreeMap<PageId, usize>,
    journal: Box<dyn VfsFile>,
    /// How many records the journal holds.
    logged: u64,
    /// Set once an append to the journal or a sync of it has failed: what the
    /// journal holds is then not known, and the session has to end.
    journal_failed: bool,
    /// The checkpoint that is being taken, if one is.
    checkpointing: Option<Checkpointing>,
}

impl Inner {
    fn log(&mut self, record: &Record) -> Result<(), ValueStoreError> {
        let appended = self.journal.append(&record.encode());
        self.journal_failed |= appended.is_err();
        self.logged += u64::from(appended.is_ok());
        appended
            .map(drop)
            .map_err(journal_error("append to", JOURNAL))
    }
}

/// A checkpoint that is being taken: the sample of the counts, and the
/// pages copied so far.
struct Checkpointing {
    version: u64,
    place: u64,
    counts: Vec<FileCount>,
    /// The pages still to copy, the next one last.
    left: Vec<PageId>,
    copied: Vec<PageCopy>,
}

/// The pages, as the value log's cleaner is given them.
pub(super) struct PageStore {
    inner: RefCell<Inner>,
}

impl PageStore {
    fn new(pages: Pages, latest: u64, journal: Box<dyn VfsFile>) -> Self {
        Self {
            inner: RefCell::new(Inner {
                pages,
                latest,
                pins: Vec::new(),
                private: BTreeMap::new(),
                journal,
                logged: 1,
                journal_failed: false,
                checkpointing: None,
            }),
        }
    }

    pub(super) fn latest(&self) -> u64 {
        self.inner.borrow().latest
    }

    pub(super) fn journal_failed(&self) -> bool {
        self.inner.borrow().journal_failed
    }

    /// The oldest pinned version, or the latest one if nothing is pinned.
    pub(super) fn oldest_pin(&self) -> CommitVersion {
        let inner = self.inner.borrow();
        let oldest = inner.pins.iter().copied().min().unwrap_or(inner.latest);
        version(oldest)
    }

    /// Pins the latest version and returns it.
    pub(super) fn pin(&self) -> u64 {
        let mut inner = self.inner.borrow_mut();
        let latest = inner.latest;
        inner.pins.push(latest);
        latest
    }

    pub(super) fn unpin(&self, version: u64) {
        let mut inner = self.inner.borrow_mut();
        let at = inner
            .pins
            .iter()
            .position(|&pinned| pinned == version)
            .expect("the version is pinned");
        inner.pins.swap_remove(at);
    }

    /// The cells of the current version of `page`.
    pub(super) fn cells(&self, page: PageId) -> Vec<Cell> {
        self.inner.borrow().pages.current_cells(page)
    }

    /// The cells of the version of `page` that a pin on `version` reads.
    pub(super) fn cells_at(&self, page: PageId, version: u64) -> Vec<Cell> {
        let inner = self.inner.borrow();
        let visible = inner.pages.visible(page, version);
        visible.map(Page::cells).unwrap_or_default()
    }

    /// Every page of the current version that has cells.
    pub(super) fn all(&self) -> Vec<(PageId, Vec<Cell>)> {
        self.inner.borrow().pages.all_current()
    }

    fn copy(&self, page: PageId) -> Vec<Cell> {
        let mut inner = self.inner.borrow_mut();
        *inner.private.entry(page).or_default() += 1;
        inner.pages.current_cells(page)
    }

    fn forget_copies(&self, pages: impl Iterator<Item = PageId>) {
        let mut inner = self.inner.borrow_mut();
        for page in pages {
            let copies = inner.private.get_mut(&page).expect("the page was copied");
            *copies -= 1;
            if *copies == 0 {
                inner.private.remove(&page);
            }
        }
    }
}

impl ValueOwners for PageStore {
    /// The version at which the page version that was looked at was born.
    type Found = u64;

    fn find(&self, page: PageId, pointer: ValuePointer) -> Result<Option<u64>, OwnersError> {
        let inner = self.inner.borrow();
        let current = inner.pages.current_version(page);
        Ok(current
            .filter(|current| current.page.holds(pointer))
            .map(|current| current.born))
    }

    fn rewrite(&self, step: PointerRewrite<'_>, found: u64) -> Result<Rewrite, OwnersError> {
        let mut inner = self.inner.borrow_mut();
        let (page, from, to) = (step.page(), step.from(), step.to());
        if inner.private.contains_key(&page) {
            return Ok(Rewrite::Refused(Refusal::Busy));
        }
        let Some(current) = inner.pages.current_version(page) else {
            return Ok(Rewrite::Refused(Refusal::OwnerGone));
        };
        if current.born != found {
            return Ok(Rewrite::Refused(Refusal::Busy));
        }
        if !current.page.holds(from) {
            return Ok(Rewrite::Refused(Refusal::OwnerGone));
        }
        inner.log(&Record::Step { page, from, to })?;
        assert!(inner.pages.rewrite_current(page, from, to));
        let latest = version(inner.latest);
        Ok(Rewrite::Installed(step.install(latest)))
    }
}

/// A write transaction: private copies of the pages it changes, the values
/// it appended ahead of its commit, and what its commit reports to the value
/// log.
#[derive(Default)]
pub(super) struct Txn {
    pages: BTreeMap<PageId, Vec<Cell>>,
    acquired: Vec<ValuePointer>,
    released: Vec<ValuePointer>,
    hold: ValueHold,
}

impl Txn {
    pub(super) fn change(&self) -> ValueChange {
        let mut change = ValueChange::new();
        self.acquired
            .iter()
            .for_each(|&pointer| change.acquire(pointer));
        self.released
            .iter()
            .for_each(|&pointer| change.release(pointer));
        change
    }

    /// The files that hold values this transaction appended.
    pub(super) fn held_files(&self) -> Vec<FileId> {
        self.hold.files().collect()
    }

    fn stop_holding(&mut self, pointer: ValuePointer) {
        match self.acquired.iter().position(|&own| own == pointer) {
            Some(at) => {
                self.acquired.swap_remove(at);
            }
            None => self.released.push(pointer),
        }
    }
}

/// A database directory on the simulated file system: the value log, and
/// the model of the layers above it.
pub(super) struct Db {
    pub(super) fs: SimVfs,
    pub(super) manifest: ManifestStore,
    pub(super) values: ValueStore,
    pub(super) writer: Option<ValueWriter>,
    pub(super) cleaner: Option<ValueWriter>,
    pub(super) pages: PageStore,
}

pub(super) const TARGET_FILE_LEN: u64 = 2048;

impl Db {
    /// Starts a session on the directory [`DB`]: creates the database if
    /// the directory has none, and opens it otherwise.
    pub(super) fn open(fs: &SimVfs) -> Result<Self, ValueStoreError> {
        match read_manifest(fs, db_dir()) {
            Err(SegmentStoreError::NoManifest { .. }) => Self::create(fs),
            Err(error) => Err(error.into()),
            Ok(_) => Self::recover(fs),
        }
    }

    fn create(fs: &SimVfs) -> Result<Self, ValueStoreError> {
        let manifest = ManifestStore::create(shared(fs), db_dir(), empty_manifest())?;
        let values = ValueStore::open(&manifest, &[])?.with_target_file_len(TARGET_FILE_LEN);
        let journal = start_journal(fs, &Record::start(0, Vec::new(), Vec::new()))?;
        Ok(Self {
            fs: fs.clone(),
            manifest,
            values,
            writer: None,
            cleaner: None,
            pages: PageStore::new(Pages::default(), 0, journal),
        })
    }

    /// Opens the database as a new session does, after a crash or after the
    /// last session ended: the manifest, then the pages and the counts from
    /// the journal's last checkpoint and the records after its place.
    ///
    /// The checkpoint copied each page at a moment of its own, so a copy may
    /// already hold a change whose record comes after the place. A commit
    /// is skipped for a page whose copy is at least as new, and a pointer
    /// rewrite for a page whose cell no longer holds the old pointer. The
    /// counts take every change after the place, skipped for its page or
    /// not.
    fn recover(fs: &SimVfs) -> Result<Self, ValueStoreError> {
        let mut manifest = ManifestStore::open(shared(fs), db_dir())?;
        let records = read_journal(fs)?;
        let last = records
            .iter()
            .rposition(|record| matches!(record, Record::Checkpoint { .. }))
            .expect("the journal starts with a checkpoint");
        let Record::Checkpoint {
            version,
            place,
            counts,
            pages: copies,
        } = &records[last]
        else {
            unreachable!("found as a checkpoint");
        };

        let mut recovery = ValueStore::recover(&manifest, counts)?;
        let mut pages = Pages::default();
        let mut latest = *version;
        let mut copied = BTreeMap::new();
        for copy in copies {
            pages.install(copy.id, copy.cells.clone(), copy.born);
            copied.insert(copy.id, copy.born);
        }
        for record in &records[*place as usize..] {
            let change = match record {
                Record::Checkpoint { .. } => continue,
                Record::Commit {
                    version,
                    pages: changed,
                    change,
                } => {
                    latest = *version;
                    for (id, cells) in changed {
                        if copied.get(id).is_none_or(|&born| born < latest) {
                            pages.install(*id, cells.clone(), latest);
                        }
                    }
                    change.clone()
                }
                Record::Step { page, from, to } => {
                    pages.rewrite_current(*page, *from, *to);
                    ValueChange::moved(*from, *to)
                }
            };
            recovery.apply(&change)?;
        }
        let values = recovery.finish()?.with_target_file_len(TARGET_FILE_LEN);

        let current = Pages(
            pages
                .all_current()
                .into_iter()
                .map(|(id, cells)| {
                    let version = PageVersion {
                        born: latest,
                        superseded: None,
                        page: Page::build(id, cells),
                    };
                    (id, vec![version])
                })
                .collect(),
        );
        let journal = start_journal(
            fs,
            &Record::start(latest, current.all_current(), values.counts()),
        )?;
        values.mark_durable(values.applied())?;
        values.commit(&mut manifest)?;
        Ok(Self {
            fs: fs.clone(),
            manifest,
            values,
            writer: None,
            cleaner: None,
            pages: PageStore::new(current, latest, journal),
        })
    }

    fn open_writer(&mut self) -> Result<ValueWriter, ValueStoreError> {
        let writer = self.values.create()?;
        self.values.commit(&mut self.manifest)?;
        Ok(writer)
    }

    /// The writer the transactions append through, in a file that is not
    /// full.
    fn writer(&mut self) -> Result<&mut ValueWriter, ValueStoreError> {
        if self.writer.as_ref().is_some_and(ValueWriter::is_full) {
            self.rotate()?;
        }
        if self.writer.is_none() {
            self.writer = Some(self.open_writer()?);
        }
        Ok(self.writer.as_mut().expect("just created"))
    }

    /// Finishes the transactions' file. The next value starts another.
    pub(super) fn rotate(&mut self) -> Result<(), ValueStoreError> {
        match self.writer.take() {
            Some(writer) => writer.finish().map(drop),
            None => Ok(()),
        }
    }

    pub(super) fn begin(&self) -> Txn {
        Txn::default()
    }

    fn private<'a>(&self, txn: &'a mut Txn, page: PageId) -> &'a mut Vec<Cell> {
        txn.pages
            .entry(page)
            .or_insert_with(|| self.pages.copy(page))
    }

    /// Gives the cell `key` of `page` the value `value`, creating the cell
    /// if the page has none with that key.
    pub(super) fn put(
        &mut self,
        txn: &mut Txn,
        page: PageId,
        key: u32,
        value: &[u8],
    ) -> Result<(), ValueStoreError> {
        let pointer = self.writer()?.append(&mut txn.hold, page, value)?;
        txn.acquired.push(pointer);
        let cells = self.private(txn, page);
        let old = match cells.iter_mut().find(|cell| cell.key == key) {
            Some(cell) => Some(std::mem::replace(&mut cell.pointer, pointer)),
            None => {
                cells.push(Cell { key, pointer });
                None
            }
        };
        if let Some(old) = old {
            txn.stop_holding(old);
        }
        Ok(())
    }

    fn take(&self, txn: &mut Txn, page: PageId, key: u32) -> Cell {
        let cells = self.private(txn, page);
        let at = cells
            .iter()
            .position(|cell| cell.key == key)
            .expect("the cell");
        cells.remove(at)
    }

    pub(super) fn delete(&self, txn: &mut Txn, page: PageId, key: u32) {
        let cell = self.take(txn, page, key);
        txn.stop_holding(cell.pointer);
    }

    /// Carries the cell `key` from the page `from` to the page `to` without
    /// its value, which the move rule forbids: the cell keeps a pointer to a
    /// value whose back pointer names `from`.
    pub(super) fn carry(&self, txn: &mut Txn, from: PageId, to: PageId, key: u32) {
        let cell = self.take(txn, from, key);
        self.private(txn, to).push(cell);
    }

    /// Moves the cell `key` from the page `from` to the page `to` as the
    /// move rule has it: the value is appended again with `to` as its owner.
    pub(super) fn move_cell(
        &mut self,
        txn: &mut Txn,
        from: PageId,
        to: PageId,
        key: u32,
    ) -> Result<(), ValueStoreError> {
        self.carry(txn, from, to, key);
        self.append_again(txn, to, key, from)
    }

    /// Appends the value of the cell `key` of `page` again, with `page` as
    /// its owner. `recorded` is the owner the value's record names now.
    pub(super) fn append_again(
        &mut self,
        txn: &mut Txn,
        page: PageId,
        key: u32,
        recorded: PageId,
    ) -> Result<(), ValueStoreError> {
        let cell = self.take(txn, page, key);
        let value = self.value(cell.pointer, recorded)?;
        txn.stop_holding(cell.pointer);
        self.put(txn, page, key, &value)
    }

    /// Makes the values `txn` appended durable and covered by the manifest,
    /// which has to come before its commit is written to the journal.
    pub(super) fn prepare(&mut self, txn: &Txn) -> Result<(), ValueStoreError> {
        if txn.acquired.is_empty() {
            return Ok(());
        }
        self.values.sync()?;
        self.values.commit(&mut self.manifest)
    }

    /// Commits `txn` and returns the new version. A transaction that holds
    /// a pointer the manifest does not cover is rolled back, with nothing
    /// written to the journal.
    pub(super) fn commit(&mut self, txn: Txn) -> Result<u64, ValueStoreError> {
        let prepared = self.prepare(&txn).and_then(|()| {
            let uncovered = txn
                .acquired
                .iter()
                .find(|&&pointer| !self.values.covers(pointer));
            uncovered.map_or(Ok(()), |&pointer| {
                Err(ValueStoreError::NotCovered { pointer })
            })
        });
        match prepared {
            Ok(()) => self.commit_prepared(txn),
            Err(error) => {
                self.rollback(txn);
                Err(error)
            }
        }
    }

    /// Commits `txn`, whose values [`Self::prepare`] has had covered.
    pub(super) fn commit_prepared(&self, txn: Txn) -> Result<u64, ValueStoreError> {
        commit_prepared(&self.pages, &self.values, txn)
    }

    pub(super) fn rollback(&self, txn: Txn) {
        self.pages.forget_copies(txn.pages.keys().copied());
    }

    /// Makes the journal durable, and with it every change applied so far.
    pub(super) fn flush(&self) -> Result<(), ValueStoreError> {
        let mark = self.values.applied();
        let mut inner = self.pages.inner.borrow_mut();
        let synced = inner.journal.sync_data();
        inner.journal_failed |= synced.is_err();
        synced.map_err(journal_error("sync", JOURNAL))?;
        self.values.mark_durable(mark)
    }

    /// Begins a checkpoint: samples the counts and their place in the
    /// journal, in one step of the counts. A checkpoint that was being taken
    /// is given up.
    pub(super) fn begin_checkpoint(&self) -> Result<(), ValueStoreError> {
        let step = self.values.step()?;
        let mut inner = self.pages.inner.borrow_mut();
        inner.checkpointing = Some(Checkpointing {
            version: inner.latest,
            place: inner.logged,
            counts: step.counts(),
            left: inner.pages.0.keys().rev().copied().collect(),
            copied: Vec::new(),
        });
        Ok(())
    }

    /// Copies up to `pages` more pages into the checkpoint that is being
    /// taken, each as it is now, and returns how many are left to copy.
    pub(super) fn copy_pages(&self, pages: usize) -> usize {
        let mut inner = self.pages.inner.borrow_mut();
        let inner = &mut *inner;
        let taking = inner.checkpointing.as_mut().expect("a checkpoint is begun");
        for _ in 0..pages {
            let Some(id) = taking.left.pop() else { break };
            taking.copied.extend(inner.pages.copy(id));
        }
        taking.left.len()
    }

    /// Copies the pages that are left and writes the checkpoint to the
    /// journal.
    pub(super) fn finish_checkpoint(&self) -> Result<(), ValueStoreError> {
        self.copy_pages(usize::MAX);
        let mut inner = self.pages.inner.borrow_mut();
        let taken = inner.checkpointing.take().expect("a checkpoint is begun");
        inner.log(&Record::Checkpoint {
            version: taken.version,
            place: taken.place,
            counts: taken.counts,
            pages: taken.copied,
        })
    }

    /// Whether a checkpoint is being taken.
    pub(super) fn is_checkpointing(&self) -> bool {
        self.pages.inner.borrow().checkpointing.is_some()
    }

    /// Takes a checkpoint in one go: the counts and every page at the same
    /// moment.
    pub(super) fn checkpoint(&self) -> Result<(), ValueStoreError> {
        self.begin_checkpoint()?;
        self.finish_checkpoint()
    }

    /// One cleaning pass over `file`, into the cleaner's own file.
    pub(super) fn clean(&mut self, file: FileId) -> Result<Cleaned, ValueStoreError> {
        self.clean_with(file, |pages| pages)
    }

    /// The cleaner's own writer, in a file that is not full.
    fn cleaner(&mut self) -> Result<&mut ValueWriter, ValueStoreError> {
        if self.cleaner.as_ref().is_some_and(ValueWriter::is_full) {
            self.cleaner.take().expect("checked").finish()?;
        }
        if self.cleaner.is_none() {
            self.cleaner = Some(self.open_writer()?);
        }
        Ok(self.cleaner.as_mut().expect("just created"))
    }

    /// As [`Self::clean`], with the cleaner given `owners(pages)` in place
    /// of the pages.
    pub(super) fn clean_with<'a, O: ValueOwners + 'a>(
        &'a mut self,
        file: FileId,
        owners: impl FnOnce(&'a PageStore) -> O,
    ) -> Result<Cleaned, ValueStoreError> {
        self.cleaner()?;
        let to = self.cleaner.as_mut().expect("just created");
        let owners = owners(&self.pages);
        self.values.clean(file, to, &mut self.manifest, &owners)
    }

    /// The first half of a pass over `file`, with the copies made durable
    /// and covered by the manifest. Other change points can come before
    /// [`Self::install`].
    pub(super) fn copy_live(&mut self, file: FileId) -> Result<CopiedValues<u64>, ValueStoreError> {
        self.cleaner()?;
        let to = self.cleaner.as_mut().expect("just created");
        let copies = self.values.copy_live(file, to, &self.pages)?;
        to.sync()?;
        self.values.commit(&mut self.manifest)?;
        Ok(copies)
    }

    /// The second half of a pass.
    pub(super) fn install(&self, copies: CopiedValues<u64>) -> Result<Cleaned, ValueStoreError> {
        self.values.install(copies, &self.pages)
    }

    /// Drops the files nothing can reach and replaces the manifest, which
    /// deletes them.
    pub(super) fn drop_empty(&mut self) -> Result<Vec<FileId>, ValueStoreError> {
        let dropped = self
            .values
            .drop_empty(self.values.retire(self.pages.oldest_pin()))?;
        self.values.commit(&mut self.manifest)?;
        Ok(dropped)
    }

    /// Reads the value at `pointer` for a cell of `page`, and checks that it
    /// took one operation of the file layer.
    pub(super) fn value(
        &self,
        pointer: ValuePointer,
        page: PageId,
    ) -> Result<Vec<u8>, ValueStoreError> {
        let before = self.fs.stats().ops;
        let mut buf = Vec::new();
        let value = self.values.read(pointer, page, &mut buf)?.to_vec();
        assert_eq!(self.fs.stats().ops, before + 1, "a read is one operation");
        Ok(value)
    }

    fn read_cells(
        &self,
        pages: impl Iterator<Item = (PageId, Vec<Cell>)>,
    ) -> Result<Logical, ValueStoreError> {
        let mut logical = Logical::new();
        for (page, cells) in pages {
            for cell in cells {
                let value = self.value(cell.pointer, page)?;
                assert!(
                    logical.insert(cell.key, (page, value)).is_none(),
                    "two cells have the key {}",
                    cell.key
                );
            }
        }
        Ok(logical)
    }

    /// Every key's page and value in the latest version, each value read
    /// through the pointer its cell holds.
    pub(super) fn logical(&self) -> Result<Logical, ValueStoreError> {
        self.read_cells(self.pages.all().into_iter())
    }

    /// As [`Self::logical`], for the version a pin on `version` reads.
    pub(super) fn logical_at(&self, version: u64) -> Result<Logical, ValueStoreError> {
        let ids: Vec<PageId> = self.pages.inner.borrow().pages.0.keys().copied().collect();
        self.read_cells(
            ids.into_iter()
                .map(|id| (id, self.pages.cells_at(id, version))),
        )
    }

    /// The live values and their bytes in each file, counted from the cells
    /// of the latest version.
    pub(super) fn recount(&self) -> BTreeMap<FileId, (u64, u64)> {
        let mut counts = BTreeMap::new();
        for (_, cells) in self.pages.all() {
            for cell in cells {
                let count: &mut (u64, u64) = counts.entry(cell.pointer.file()).or_default();
                count.0 += 1;
                count.1 += cell.pointer.extent_len(FormatVersion::CURRENT);
            }
        }
        counts
    }

    /// Checks that the store's counts are what the cells of the latest
    /// version give.
    #[track_caller]
    pub(super) fn assert_counts_agree(&self) {
        let kept: BTreeMap<FileId, (u64, u64)> = self
            .values
            .counts()
            .into_iter()
            .map(|count| {
                assert!(!count.miscounted, "{count:?}");
                (count.file, (count.values, count.bytes))
            })
            .collect();
        assert_eq!(kept, self.recount(), "the counts and the pages disagree");
    }

    /// The value-log files in `val/`, by id.
    pub(super) fn files_on_disk(&self) -> BTreeSet<FileId> {
        let dir = FileNames::VALUE_LOG.dir(db_dir());
        let entries = self.fs.read_dir(&dir).expect("val/ is listed");
        entries
            .iter()
            .filter_map(|entry| FileNames::VALUE_LOG.parse(&entry.name))
            .collect()
    }
}

/// Writes the commit of `txn` to the journal, installs its pages and counts
/// its values. The values it appended must already be covered by the
/// manifest ([`Db::prepare`]).
fn commit_prepared(
    pages: &PageStore,
    values: &ValueStore,
    txn: Txn,
) -> Result<u64, ValueStoreError> {
    let change = txn.change();
    let step = values.step()?;
    let mut inner = pages.inner.borrow_mut();
    let version = inner.latest + 1;
    let changed: Vec<_> = txn.pages.clone().into_iter().collect();
    let logged = inner.log(&Record::Commit {
        version,
        pages: changed.clone(),
        change: change.clone(),
    });
    for (id, _) in &changed {
        let copies = inner.private.get_mut(id).expect("the page was copied");
        *copies -= 1;
        if *copies == 0 {
            inner.private.remove(id);
        }
    }
    logged?;
    for (id, cells) in changed {
        inner.pages.install(id, cells, version);
    }
    inner.latest = version;
    let counted = step.apply(&change, super::version(version));
    drop(txn);
    counted.map(|()| version)
}
