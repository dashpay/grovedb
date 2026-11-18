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
    table::{self, Decoder, Encoder, Record},
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

/// What a checkpoint holds, apart from its links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointInfo {
    named: TableCheckpoint,
    reaches: Option<CommitVersion>,
    links: u64,
    segments: BTreeMap<FileId, u64>,
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
    /// `table/` is created if it is missing, and its name is made durable
    /// either way. Every file in it with a checkpoint's name that the
    /// manifest does not name is removed: a crash can leave one that was
    /// written and never named, or one that was replaced and not yet
    /// deleted. Other entries of `table/` are left alone.
    pub fn open(manifest: &ManifestStore) -> Result<Self, PageTableError> {
        let current = manifest.manifest()?;
        let (vfs, dir) = (manifest.vfs().clone(), manifest.dir().to_path_buf());
        let on_disk = list_or_create_dir(&*vfs, &dir, &FileNames::PAGE_TABLE)?
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
    /// If this fails, the file is removed again and the id is not used a
    /// second time in this session.
    pub fn write(
        &self,
        table: &PageTable,
        covers: CommitVersion,
    ) -> Result<CheckpointInfo, PageTableError> {
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
        let written = write_records(&*file, &self.path(id), id, table, covers);
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
    /// `log_durable` is the commit version the log is durable through. The
    /// checkpoint is refused if it covers, or holds a link of, a later
    /// version ([`PageTableError::LogBehind`]): sync the log through
    /// [`CheckpointInfo::reaches`] first. A recovery then never lands below
    /// a link of the checkpoint it loads.
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
        log_durable: CommitVersion,
        manifest: &mut ManifestStore,
        segments: &SegmentStore,
    ) -> Result<(), PageTableError> {
        self.check_same_dir(manifest.dir())?;
        self.check_same_dir(segments.dir())?;
        let covers = checkpoint.covers();
        let needed = checkpoint
            .reaches()
            .map_or(covers, |reaches| reaches.max(covers));
        if needed > log_durable {
            return Err(PageTableError::LogBehind {
                needed,
                durable: log_durable,
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
    let totals = encoder
        .finish(covers, &mut records)
        .map_err(PageTableError::format(path, end.len))?;
    end.append(&mut records)?;
    file.sync_data().map_err(PageTableError::io("sync", path))?;

    Ok(CheckpointInfo {
        named: TableCheckpoint::new(id, end.len, covers)
            .map_err(PageTableError::format(path, 0))?,
        reaches: totals.reaches,
        links: totals.links,
        segments: extents.finish(),
    })
}

impl PageTable {
    /// Reads the checkpoint that `manifest` names, in the database directory
    /// `dir`, and returns the table it holds with what else the checkpoint
    /// says. A manifest that names no checkpoint gives an empty table and
    /// `None`.
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
            links: end.links,
            segments: extents.finish(),
        };
        info.check_covered_by(manifest)?;
        Ok((Self::from_chains(chains), Some(info)))
    }
}
