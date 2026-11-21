//! Page-table checkpoints: writing one, naming it in the manifest, and
//! reading the one the manifest names.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use lignin_format::{
    manifest::{Manifest, TableCheckpoint},
    table::{self, Decoder, Encoder, LogPlace, Record, ValueCount},
    CommitVersion, FileHeader, FileId, FileKind, PageId,
};
use lignin_io::{EntryKind, OpenMode, Vfs, VfsFile};

use super::{
    chains::Chains,
    error::PageTableError,
    page_table::{Link, LinkKind, PageTable},
};
use crate::segment::{
    create_file, list_or_create_dir, BlockLocation, FileNames, ManifestStore, SegmentStore,
    SegmentStoreError,
};

/// A sample of the count of live values of every value-log file, as a
/// checkpoint stores it for recovery.
///
/// The table does not read it. Whoever takes a checkpoint takes the sample
/// from the value log, with the place the log had reached, before
/// [`TableCheckpoints::write`] copies the first chain, and whoever recovers
/// gives it back to the value log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CountSample {
    /// The sample's place in the log: the counts include every change whose
    /// record is before it and none whose record is after it. It is the end
    /// of the mark that was written to the log when the sample was taken.
    /// `None` is the place before every record of the log.
    pub place: Option<LogPlace>,
    /// The count of each value-log file that has one, in ascending order of
    /// file id.
    pub counts: Vec<ValueCount>,
}

/// How far the log is durable, as whoever installs a checkpoint has made
/// sure of after [`TableCheckpoints::write`] returned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogDurable {
    /// The log is durable through the commit of this version.
    pub version: CommitVersion,
    /// Every record of the log before this place is durable: the records of
    /// commits and those of background changes, which have no version.
    /// `None` if none is known to be.
    pub place: Option<LogPlace>,
}

/// What a checkpoint holds, apart from its links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointInfo {
    named: TableCheckpoint,
    reaches: Option<CommitVersion>,
    trimmed_free: Option<CommitVersion>,
    links: u64,
    segments: BTreeMap<FileId, u64>,
    sample: CountSample,
    log_end: Option<LogPlace>,
}

impl CheckpointInfo {
    /// The checkpoint as a manifest names it: its file id, the length of the
    /// file and the commit version it covers.
    pub fn named(&self) -> TableCheckpoint {
        self.named
    }

    /// The id of the checkpoint file.
    pub fn file(&self) -> FileId {
        self.named.file()
    }

    /// The commit version the checkpoint covers: replay starts with the
    /// commit after it.
    pub fn covers(&self) -> CommitVersion {
        self.named.covers()
    }

    /// The newest version of any link in the checkpoint, or `None` if it
    /// has no links. It can be above [`Self::covers`].
    pub fn reaches(&self) -> Option<CommitVersion> {
        self.reaches
    }

    /// The newest version of a free link that had been trimmed out of the
    /// table when the checkpoint was written ([`PageTable::trimmed_free`]),
    /// or `None` if there was none. The checkpoint may hold no link of a
    /// page that was freed at or before this version.
    ///
    /// The file does not hold this. It is `None` for a checkpoint that was
    /// loaded: the log was durable through it before the checkpoint was
    /// named.
    pub fn trimmed_free(&self) -> Option<CommitVersion> {
        self.trimmed_free
    }

    /// The commit version the log must be durable through before a manifest
    /// names the checkpoint: the newest of [`Self::covers`],
    /// [`Self::reaches`] and [`Self::trimmed_free`].
    pub fn log_needed(&self) -> CommitVersion {
        let held = self.reaches.max(self.trimmed_free);
        held.map_or(self.covers(), |held| held.max(self.covers()))
    }

    /// The place the log must be durable up to before a manifest names the
    /// checkpoint: where the log ended when the last chain had been copied.
    /// A chain can hold a change whose record is after the place of the
    /// sample, and no record whose change a chain holds is at or after this
    /// place. It is never before the place of the sample.
    ///
    /// The file does not hold this. For a checkpoint that was loaded it is
    /// the place of the sample: the log was durable up to the end of the
    /// copy before the checkpoint was named.
    pub fn log_place_needed(&self) -> Option<LogPlace> {
        self.log_end
    }

    /// The sample of the value log's counts that the checkpoint holds. A
    /// recovery from the checkpoint starts the counts from it, and the log
    /// is needed from its place on.
    pub fn sample(&self) -> &CountSample {
        &self.sample
    }

    /// The place from which the log is kept while a manifest names the
    /// checkpoint, or may come to: the place of the sample. The log file
    /// that it names is not deleted, also when the place is that file's
    /// end, and no later one is. A recovery reads that file to check the
    /// mark the sample was taken at, and takes the background changes and
    /// the count changes from the place on. `None` is the place before
    /// every record: the whole log is kept.
    ///
    /// The log is also kept from the group of the commit after
    /// [`Self::covers`], which can be in an earlier file.
    pub fn log_kept_from(&self) -> Option<LogPlace> {
        self.sample.place
    }

    /// The number of links in the checkpoint.
    pub fn links(&self) -> u64 {
        self.links
    }

    /// The segments the checkpoint points into, each with the file offset
    /// at which the last block it points to ends. None of these segments
    /// may be deleted, or named with a shorter valid length, while a
    /// manifest names the checkpoint.
    pub fn segments(&self) -> &BTreeMap<FileId, u64> {
        &self.segments
    }

    fn check_covered_by(&self, manifest: &Manifest) -> Result<(), PageTableError> {
        let order = |kind: FileKind, id: FileId| (kind.code(), id);
        for (&segment, &needed) in &self.segments {
            let valid_len = manifest
                .files()
                .binary_search_by_key(&order(FileKind::Segment, segment), |file| {
                    order(file.kind(), file.id())
                })
                .ok()
                .map(|at| manifest.files()[at].valid_len());
            if valid_len.is_none_or(|valid_len| valid_len < needed) {
                return Err(PageTableError::NotCovered {
                    segment,
                    needed,
                    valid_len,
                });
            }
        }
        Ok(())
    }
}

/// For each segment, where the last block that a link points to ends.
#[derive(Default)]
struct Extents {
    segments: BTreeMap<FileId, u64>,
}

impl Extents {
    fn add(&mut self, location: BlockLocation) {
        let end = location.offset + u64::from(location.len);
        let extent = self.segments.entry(location.file).or_insert(end);
        *extent = (*extent).max(end);
    }

    fn finish(self) -> BTreeMap<FileId, u64> {
        self.segments
    }
}

fn to_format(page: PageId, link: Link) -> Result<table::Link, lignin_format::FormatError> {
    let pointer = |location: BlockLocation| {
        table::BlockPointer::new(location.file, location.offset, location.len)
    };
    let kind = match link.kind {
        LinkKind::Base(location) => table::LinkKind::Base(pointer(location)?),
        LinkKind::Delta(location) => table::LinkKind::Delta(pointer(location)?),
        LinkKind::Free => table::LinkKind::Free,
    };
    Ok(table::Link {
        page,
        version: link.version,
        kind,
    })
}

fn from_format(link: table::Link) -> Link {
    let location = |block: table::BlockPointer| BlockLocation {
        file: block.file(),
        offset: block.offset(),
        len: block.len(),
    };
    let kind = match link.kind {
        table::LinkKind::Base(block) => LinkKind::Base(location(block)),
        table::LinkKind::Delta(block) => LinkKind::Delta(location(block)),
        table::LinkKind::Free => LinkKind::Free,
    };
    Link {
        version: link.version,
        kind,
    }
}

#[derive(Debug, Default)]
struct Files {
    last_id: Option<FileId>,
    /// The checkpoint files this session knows to be in `table/`.
    on_disk: BTreeSet<FileId>,
    /// Those of them that were written here and that no manifest names yet.
    waiting: BTreeSet<FileId>,
}

/// The page-table checkpoints of one database directory: the files in
/// `table/`.
///
/// It writes a checkpoint of a [`PageTable`], has the manifest name it, and
/// deletes the checkpoints the manifest no longer names. See [the module
/// documentation](super) for the steps and for what a crash at each of them
/// leaves.
///
/// Every method takes `&self` and may be called from any thread. The caller
/// holds the directory's lock for as long as this lives.
#[derive(Debug)]
pub struct TableCheckpoints {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    files: Mutex<Files>,
}

impl TableCheckpoints {
    /// Opens the checkpoints of the directory of `manifest`.
    ///
    /// `table/` is created if it is missing, and its name is durable when
    /// this returns ([`list_or_create_dir`]). Every file in it with a
    /// checkpoint's name that the manifest does not name is removed: a
    /// crash can leave one that was written and never named, or one that
    /// was replaced and not yet deleted. Other entries of `table/` are
    /// left alone.
    pub fn open(manifest: &ManifestStore) -> Result<Self, PageTableError> {
        let current = manifest.manifest()?;
        let (vfs, dir) = (manifest.vfs().clone(), manifest.dir().to_path_buf());
        let on_disk = list_or_create_dir(&*vfs, &dir, &FileNames::PAGE_TABLE, manifest.dir_sync())?
            .iter()
            .filter(|entry| entry.kind == EntryKind::File)
            .filter_map(|entry| FileNames::PAGE_TABLE.parse(&entry.name))
            .collect();
        let checkpoints = Self {
            vfs,
            dir,
            files: Mutex::new(Files {
                last_id: current.last_file_ids().page_table,
                on_disk,
                waiting: BTreeSet::new(),
            }),
        };
        checkpoints.remove_unnamed(current)?;
        Ok(checkpoints)
    }

    /// The database directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn files(&self) -> MutexGuard<'_, Files> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn path(&self, id: FileId) -> PathBuf {
        FileNames::PAGE_TABLE.path(&self.dir, id)
    }

    fn check_same_dir(&self, found: &Path) -> Result<(), PageTableError> {
        if found == self.dir {
            Ok(())
        } else {
            Err(PageTableError::Store(SegmentStoreError::WrongDirectory {
                expected: self.dir.clone(),
                found: found.to_path_buf(),
            }))
        }
    }

    /// Removes the file of checkpoint `id`. A file that is not there counts
    /// as removed.
    fn remove(&self, id: FileId) -> Result<(), PageTableError> {
        let path = self.path(id);
        match self.vfs.remove_file(&path) {
            Err(source) if source.kind() != ErrorKind::NotFound => {
                Err(PageTableError::NotDeleted { path, source })
            }
            _ => {
                self.files().on_disk.remove(&id);
                Ok(())
            }
        }
    }

    /// Removes every checkpoint file that `manifest` does not name and that
    /// is not waiting to be named.
    fn remove_unnamed(&self, manifest: &Manifest) -> Result<(), PageTableError> {
        let named = manifest.table_checkpoint().map(|table| table.file());
        let unnamed: Vec<FileId> = {
            let mut files = self.files();
            if let Some(named) = named {
                files.waiting.remove(&named);
            }
            files
                .on_disk
                .iter()
                .filter(|id| Some(**id) != named && !files.waiting.contains(id))
                .copied()
                .collect()
        };
        let mut result = Ok(());
        for id in unnamed {
            result = result.and(self.remove(id));
        }
        result
    }

    /// Writes a checkpoint of `table` to a new file in `table/` and makes
    /// it durable. `covers` is the commit version it covers: the caller
    /// vouches that every change a commit up to `covers` made to a page is
    /// in a block that a link of `table` points to.
    ///
    /// No manifest names the checkpoint yet: [`Self::install`] does that,
    /// and until then a crash leaves the checkpoint the manifest named
    /// before.
    ///
    /// The table can be changed while this runs. Each page's chain is
    /// written as it is at one moment, and the pages are not all taken at
    /// the same moment.
    ///
    /// `sample` is stored in the checkpoint as it is given, and
    /// [`CheckpointInfo::sample`] gives it back when the checkpoint is
    /// loaded. Its counts must be in ascending order of file id, one for a
    /// file. A database with no value-log file has an empty sample.
    ///
    /// `log_end` says where the log ends at the moment it is asked: the
    /// place after its last record, or `None` while it has none. The record
    /// of every change that the table holds at that moment is before the
    /// place, and the place never goes back. It is asked twice:
    ///
    /// - Before the first chain is copied. The sample must have been taken,
    ///   and its mark written, by then: a sample whose place is after the
    ///   log's end is refused ([`PageTableError::SampleAhead`]), and nothing
    ///   is written. Only the two places are compared: that the record
    ///   which ends at the sample's place is its mark is not checked here.
    /// - After the last chain is copied. The checkpoint is installed only
    ///   once the log is durable up to that place
    ///   ([`CheckpointInfo::log_place_needed`]).
    ///
    /// If this fails, the file is removed again and the id is not used a
    /// second time in this session. If the removal fails as well, the file
    /// is left with no manifest naming it, and a later call that is told of
    /// a manifest, or the next session, removes it.
    pub fn write(
        &self,
        table: &PageTable,
        covers: CommitVersion,
        sample: &CountSample,
        log_end: impl Fn() -> Option<LogPlace>,
    ) -> Result<CheckpointInfo, PageTableError> {
        let began = log_end();
        if let Some(place) = sample.place
            && Some(place) > began
        {
            return Err(PageTableError::SampleAhead {
                place,
                log_end: began,
            });
        }
        let id = {
            let mut files = self.files();
            let next = files.last_id.map_or(1, |last| last.get().wrapping_add(1));
            let id = FileId::new(next).ok_or(PageTableError::FileIdsExhausted)?;
            files.last_id = Some(id);
            id
        };
        let file = create_file(&*self.vfs, &self.dir, &FileNames::PAGE_TABLE, id)?;
        {
            let mut files = self.files();
            files.on_disk.insert(id);
            files.waiting.insert(id);
        }
        // A log's end does not go back, so this is not before the sample.
        let log_end = || log_end().max(began);
        let path = self.path(id);
        let written = write_records(&*file, &path, id, table, covers, sample, &log_end);
        drop(file);
        if written.is_err() {
            self.files().waiting.remove(&id);
            let _ = self.remove(id);
        }
        written
    }

    /// Replaces the manifest with one that names `checkpoint`, which
    /// [`Self::write`] returned, and deletes the checkpoint it named before.
    ///
    /// `log` is how far the log is durable. The checkpoint is refused if it
    /// needs the log durable through a later version than
    /// [`LogDurable::version`] ([`PageTableError::LogBehind`]): if it covers
    /// a later version, holds a link of one, or was written after a free
    /// link of one had been trimmed out of the table. Sync the log through
    /// [`CheckpointInfo::log_needed`] first. A recovery then never lands
    /// below a link of the checkpoint it loads, and never where a page that
    /// the checkpoint has no link of is still in use.
    ///
    /// It is also refused if the log ended, when the last chain had been
    /// copied, after [`LogDurable::place`]
    /// ([`PageTableError::LogBehindCopy`]). Sync the log after `write` has
    /// returned, up to [`CheckpointInfo::log_place_needed`] or further. A
    /// chain can hold a change that a background step made after the sample
    /// was taken, and such a change has no version: were its record lost,
    /// the checkpoint would hold a value pointer that no count includes. A
    /// recovery also applies to the sample the changes after its place, so
    /// the log it finds must reach that place, which is not after the end
    /// of the copy.
    ///
    /// Then every segment is synced, so that the blocks the checkpoint
    /// points to are durable. The new manifest names the segments as they
    /// are then ([`SegmentStore::describe`]), and it is installed only if it
    /// vouches for every block the checkpoint points to
    /// ([`PageTableError::NotCovered`]) and the checkpoint does not cover an
    /// earlier version than the one named before
    /// ([`PageTableError::CoversWentBack`]).
    ///
    /// After [`PageTableError::NotDeleted`], or
    /// [`SegmentStoreError::NotDeleted`], the new manifest is in force.
    /// After any other error [`ManifestStore::manifest`] tells where things
    /// stand: it gives the old manifest, and the checkpoint can be installed
    /// again or discarded, or it fails with
    /// [`SegmentStoreError::ManifestInDoubt`], and the directory has the old
    /// manifest or the new one.
    pub fn install(
        &self,
        checkpoint: &CheckpointInfo,
        log: LogDurable,
        manifest: &mut ManifestStore,
        segments: &SegmentStore,
    ) -> Result<(), PageTableError> {
        self.check_same_dir(manifest.dir())?;
        self.check_same_dir(segments.dir())?;
        let needed = checkpoint.log_needed();
        if needed > log.version {
            return Err(PageTableError::LogBehind {
                needed,
                durable: log.version,
            });
        }
        if let Some(needed) = checkpoint.log_end
            && Some(needed) > log.place
        {
            return Err(PageTableError::LogBehindCopy {
                needed,
                durable: log.place,
            });
        }
        segments.sync()?;
        let manifest_path = self.dir.join(ManifestStore::FILE_NAME);
        manifest.update(|current| {
            let described = segments.describe(current)?;
            if let Some(named) = current.table_checkpoint()
                && checkpoint.covers() < named.covers()
            {
                return Err(PageTableError::CoversWentBack {
                    current: named.covers(),
                    offered: checkpoint.covers(),
                });
            }
            checkpoint.check_covered_by(&described)?;
            let mut last_file_ids = described.last_file_ids();
            last_file_ids.page_table = last_file_ids.page_table.max(Some(checkpoint.file()));
            Manifest::new(
                described.page_size(),
                last_file_ids,
                Some(checkpoint.named()),
                described.files().to_vec(),
            )
            .map_err(PageTableError::format(&manifest_path, 0))
        })?;
        let told = segments.manifest_replaced(manifest);
        let removed = self.manifest_replaced(manifest);
        told.map_err(PageTableError::from).and(removed)
    }

    /// Tells the checkpoints that the manifest was replaced or opened again.
    /// Every checkpoint file that the durable manifest does not name is
    /// deleted, except one that was written here and not installed yet.
    ///
    /// [`Self::install`] calls this. Call it after
    /// [`SegmentStoreError::ManifestInDoubt`], once the manifest has been
    /// opened again.
    ///
    /// A file that cannot be deleted does not stop the rest: the call ends
    /// with [`PageTableError::NotDeleted`], and the next call tries the
    /// deletion again.
    pub fn manifest_replaced(&self, manifest: &ManifestStore) -> Result<(), PageTableError> {
        self.check_same_dir(manifest.dir())?;
        self.remove_unnamed(manifest.manifest()?)
    }

    /// Deletes a checkpoint that was written and is not going to be
    /// installed. If `manifest` names it, it stays.
    pub fn discard(
        &self,
        checkpoint: CheckpointInfo,
        manifest: &ManifestStore,
    ) -> Result<(), PageTableError> {
        self.check_same_dir(manifest.dir())?;
        let named = manifest.manifest()?.table_checkpoint();
        if named.map(|table| table.file()) == Some(checkpoint.file()) {
            return Ok(());
        }
        self.files().waiting.remove(&checkpoint.file());
        self.remove(checkpoint.file())
    }
}

/// The end of a checkpoint file that is being written.
struct Appender<'a> {
    file: &'a dyn VfsFile,
    path: &'a Path,
    len: u64,
}

impl Appender<'_> {
    /// Appends `records`, if there are any, and empties the buffer.
    fn append(&mut self, records: &mut Vec<u8>) -> Result<(), PageTableError> {
        if !records.is_empty() {
            self.file
                .append(records)
                .map_err(PageTableError::io("append to", self.path))?;
            self.len += records.len() as u64;
            records.clear();
        }
        Ok(())
    }
}

fn write_records(
    file: &dyn VfsFile,
    path: &Path,
    id: FileId,
    table: &PageTable,
    covers: CommitVersion,
    sample: &CountSample,
    log_end: &dyn Fn() -> Option<LogPlace>,
) -> Result<CheckpointInfo, PageTableError> {
    let mut end = Appender {
        file,
        path,
        len: FileHeader::ENCODED_LEN as u64,
    };
    let mut encoder = Encoder::new();
    let mut extents = Extents::default();
    let mut records = Vec::new();
    let mut links = Vec::new();
    let mut next = Some(PageId::MIN);
    while let Some(from) = next {
        next = table.scan(from, &mut links);
        for (page, link) in links.drain(..) {
            if let Some(location) = link.location() {
                extents.add(location);
            }
            to_format(page, link)
                .and_then(|link| encoder.push(link, &mut records))
                .map_err(PageTableError::format(path, end.len))?;
            end.append(&mut records)?;
        }
    }
    // Both are read after the last chain was copied. A free link that a
    // trim removed before its chain was copied is then no newer than
    // the one, and the record of a change that a copied chain holds is
    // before the other.
    let trimmed_free = table.trimmed_free();
    let log_end = log_end();
    for count in &sample.counts {
        let offset = end.len + records.len() as u64;
        encoder
            .push_value_count(*count, &mut records)
            .map_err(PageTableError::format(path, offset))?;
        if records.len() >= table::MAX_RECORD_LEN {
            end.append(&mut records)?;
        }
    }
    let offset = end.len + records.len() as u64;
    let totals = encoder
        .finish(covers, sample.place, &mut records)
        .map_err(PageTableError::format(path, offset))?;
    end.append(&mut records)?;
    file.sync_data().map_err(PageTableError::io("sync", path))?;

    Ok(CheckpointInfo {
        named: TableCheckpoint::new(id, end.len, covers)
            .map_err(PageTableError::format(path, 0))?,
        reaches: totals.reaches,
        trimmed_free,
        links: totals.links,
        segments: extents.finish(),
        sample: sample.clone(),
        log_end,
    })
}

impl PageTable {
    /// Reads the checkpoint that `manifest` names, in the database directory
    /// `dir`, and returns the table it holds with what else the checkpoint
    /// says, the sample of the value counts among it
    /// ([`CheckpointInfo::sample`]). A manifest that names no checkpoint
    /// gives an empty table and `None`.
    ///
    /// The file must be at least as long as its valid length, start with the
    /// header of a checkpoint of that id, hold whole records up to exactly
    /// its valid length, and cover the commit version the manifest says. The
    /// manifest must vouch for every block a link points to
    /// ([`PageTableError::NotCovered`]). Nothing in the directory is
    /// changed.
    pub fn load(
        vfs: &dyn Vfs,
        dir: &Path,
        manifest: &Manifest,
    ) -> Result<(Self, Option<CheckpointInfo>), PageTableError> {
        let Some(named) = manifest.table_checkpoint() else {
            return Ok((Self::new(), None));
        };
        let (id, valid_len) = (named.file(), named.valid_len());
        let path = FileNames::PAGE_TABLE.path(dir, id);
        let file = vfs
            .open(&path, OpenMode::Read)
            .map_err(PageTableError::io("open", &path))?;
        let len = file.len().map_err(PageTableError::io("measure", &path))?;
        if len < valid_len {
            return Err(PageTableError::FileTooShort {
                path,
                valid_len,
                len,
            });
        }
        let mut buf = vec![0; FileHeader::ENCODED_LEN];
        file.read_exact_at(&mut buf, 0)
            .map_err(PageTableError::io("read", &path))?;
        let version =
            table::decode_file_header(&buf, id).map_err(PageTableError::format(&path, 0))?;
        if version > manifest.format_version() {
            return Err(PageTableError::FileNewerThanManifest {
                path,
                file: version,
                manifest: manifest.format_version(),
            });
        }

        let mut decoder = Decoder::new(version);
        let mut chains = Chains::new();
        let mut extents = Extents::default();
        let mut counts = Vec::new();
        let mut offset = FileHeader::ENCODED_LEN as u64;
        let end = loop {
            let window = (valid_len - offset).min(table::MAX_RECORD_LEN as u64) as usize;
            buf.resize(window, 0);
            file.read_exact_at(&mut buf, offset)
                .map_err(PageTableError::io("read", &path))?;
            let (record, rest) = decoder
                .read(&buf)
                .map_err(PageTableError::format(&path, offset))?;
            offset += (window - rest.len()) as u64;
            match record {
                Record::Links(links) => {
                    for link in links {
                        let page = link.page;
                        let link = from_format(link);
                        if let Some(location) = link.location() {
                            extents.add(location);
                        }
                        chains
                            .push(page, link)
                            .map_err(|source| PageTableError::Link { page, source })?;
                    }
                }
                Record::ValueCounts(record) => counts.extend(record),
                Record::End(end) => break end,
            }
        };
        if offset != valid_len {
            let longer = table::TableError::LengthMismatch {
                expected: offset,
                found: valid_len,
            };
            return Err(PageTableError::format(&path, offset)(longer.into()));
        }
        if end.covers != named.covers() {
            return Err(PageTableError::CoversMismatch {
                path,
                manifest: named.covers(),
                file: end.covers,
            });
        }
        let info = CheckpointInfo {
            named,
            reaches: end.reaches,
            trimmed_free: None,
            links: end.links,
            segments: extents.finish(),
            sample: CountSample {
                place: end.place,
                counts,
            },
            log_end: end.place,
        };
        info.check_covered_by(manifest)?;
        Ok((Self::from_chains(chains), Some(info)))
    }
}
