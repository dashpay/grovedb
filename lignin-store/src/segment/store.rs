//! The segments of one database directory: reading them, and their lives
//! from creation to deletion.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
};

use lignin_format::{
    manifest::{FileState, LiveFile, Manifest},
    segment::{self, Block, SegmentIndex, SegmentTrailer, DATA_START, FRAME_LEN, MAX_BODY_LEN},
    FileHeader, FileId, FileKind, FormatError, FormatVersion,
};
use lignin_io::{DirEntry, EntryKind, OpenMode, Vfs, VfsFile};

use super::{
    error::SegmentStoreError,
    files::{create_file, list_or_create_dir, remove_if_present, FileNames},
    manifest::ManifestStore,
    writer::SegmentWriter,
};

const WALK_WINDOW_LEN: u64 = 256 * 1024;

/// Where a block is: its segment, the file offset it starts at and its
/// length in the file, frame included.
///
/// [`SegmentWriter::append`] returns it and [`SegmentStore::read`] follows
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockLocation {
    /// The segment that holds the block.
    pub file: FileId,
    /// The file offset of the block's first byte.
    pub offset: u64,
    /// The block's length in the file, its frame included.
    pub len: u32,
}

/// One segment of a store as it is now, from [`SegmentStore::extents`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    /// The segment.
    pub file: FileId,
    /// How a manifest written now would name the segment. A segment that is
    /// not named open takes no more blocks.
    pub state: FileState,
    /// Whether a writer of this session still has the segment.
    pub has_writer: bool,
    /// The file offset at which the segment ends now: after the last block
    /// that was appended, durable or not, and after the footer of a sealed
    /// segment.
    pub end: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Created,
    Open,
    Closed,
    Sealed,
}

impl Phase {
    /// The state a manifest names a segment in this phase with.
    fn file_state(self) -> FileState {
        match self {
            Self::Created | Self::Open => FileState::Open,
            Self::Closed => FileState::Closed,
            Self::Sealed => FileState::Sealed,
        }
    }
}

#[derive(Debug)]
struct State {
    phase: Phase,
    durable_len: u64,
    failed: bool,
    has_writer: bool,
    /// The handle the segment's blocks are appended through, and the only
    /// one the segment is synced through. The segment keeps it while it
    /// has its writer, and after that until a sync through it has covered
    /// every block, or the segment is cut where a sync failed. A segment
    /// that the store lets go before that takes the handle with it.
    appender: Option<Arc<dyn VfsFile>>,
}

impl State {
    fn is_written(&self) -> bool {
        matches!(self.phase, Phase::Created | Phase::Open)
    }

    /// Syncs the file through the handle that appended to it.
    fn sync_appended(&self) -> io::Result<()> {
        match &self.appender {
            Some(appender) => appender.sync_data(),
            None => Err(io::Error::other(
                "the handle that appended to the segment is closed",
            )),
        }
    }
}

#[derive(Debug)]
pub(super) struct Segment {
    id: FileId,
    path: PathBuf,
    version: FormatVersion,
    file: Box<dyn VfsFile>,
    end: AtomicU64,
    appendable: AtomicBool,
    state: Mutex<State>,
}

impl Segment {
    fn adopt(
        vfs: &dyn Vfs,
        db: &Path,
        named: &LiveFile,
        database: FormatVersion,
    ) -> Result<Self, SegmentStoreError> {
        let (id, valid_len) = (named.id(), named.valid_len());
        let path = FileNames::SEGMENT.path(db, id);
        let file = vfs
            .open(&path, OpenMode::Read)
            .map_err(SegmentStoreError::io("open", &path))?;
        let len = file
            .len()
            .map_err(SegmentStoreError::io("measure", &path))?;
        if len < valid_len {
            return Err(SegmentStoreError::FileTooShort {
                path,
                valid_len,
                len,
            });
        }
        let mut header = [0; FileHeader::ENCODED_LEN];
        file.read_exact_at(&mut header, 0)
            .map_err(SegmentStoreError::io("read", &path))?;
        let version = segment::decode_file_header(&header, id)
            .map_err(SegmentStoreError::format(&path, 0))?;
        if version > database {
            return Err(SegmentStoreError::FileNewerThanManifest {
                path,
                file: version,
                manifest: database,
            });
        }
        let phase = match named.state() {
            FileState::Sealed => Phase::Sealed,
            FileState::Open | FileState::Closed => Phase::Closed,
        };
        Ok(Self::new(id, path, version, file, phase, valid_len, None))
    }

    fn created(
        id: FileId,
        path: PathBuf,
        file: Box<dyn VfsFile>,
        appender: Arc<dyn VfsFile>,
    ) -> Self {
        Self::new(
            id,
            path,
            FormatVersion::CURRENT,
            file,
            Phase::Created,
            DATA_START,
            Some(appender),
        )
    }

    fn new(
        id: FileId,
        path: PathBuf,
        version: FormatVersion,
        file: Box<dyn VfsFile>,
        phase: Phase,
        len: u64,
        appender: Option<Arc<dyn VfsFile>>,
    ) -> Self {
        Self {
            id,
            path,
            version,
            file,
            end: AtomicU64::new(len),
            appendable: AtomicBool::new(false),
            state: Mutex::new(State {
                phase,
                durable_len: len,
                failed: false,
                has_writer: appender.is_some(),
                appender,
            }),
        }
    }

    pub(super) fn id(&self) -> FileId {
        self.id
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tells the file layer that the block at `location` is about to be
    /// read, if the range lies in the segment's file: after its header and
    /// before the end of what is written of it, as far as this handle
    /// knows. For a sealed segment that end is the file's, past its
    /// blocks; whether a location is a block is the read's to say.
    fn will_read(&self, location: BlockLocation) {
        let end = self.end.load(Ordering::Acquire);
        let len = u64::from(location.len);
        let inside = location.offset >= DATA_START
            && (location.offset.checked_add(len)).is_some_and(|after| after <= end);
        if inside {
            self.file.will_read(location.offset, len);
        }
    }

    fn read_exact(&self, buf: &mut [u8], offset: u64) -> Result<(), SegmentStoreError> {
        self.file
            .read_exact_at(buf, offset)
            .map_err(SegmentStoreError::io("read", &self.path))
    }

    fn format_error(&self, offset: u64) -> impl FnOnce(FormatError) -> SegmentStoreError + use<'_> {
        SegmentStoreError::format(&self.path, offset)
    }

    fn block<'a>(
        &self,
        location: BlockLocation,
        max_payload_len: usize,
        buf: &'a mut Vec<u8>,
    ) -> Result<Block<'a>, SegmentStoreError> {
        let end = self.end.load(Ordering::Acquire);
        let inside = location.offset >= DATA_START
            && location
                .offset
                .checked_add(u64::from(location.len))
                .is_some_and(|block_end| block_end <= end);
        if !inside {
            return Err(SegmentStoreError::OutOfBounds { location, end });
        }
        let len = location.len as usize;
        let max = max_payload_len.min(MAX_BODY_LEN);
        if len > FRAME_LEN + max {
            return Err(self.format_error(location.offset)(
                FormatError::RecordTooLong {
                    len: (len - FRAME_LEN) as u64,
                    max: max as u64,
                },
            ));
        }
        buf.clear();
        buf.resize(len, 0);
        self.read_exact(buf, location.offset)?;
        Block::decode(self.version, buf, max_payload_len)
            .map_err(self.format_error(location.offset))
    }

    fn sealed_len(&self) -> Result<u64, SegmentStoreError> {
        let state = self.state();
        match state.phase {
            Phase::Sealed => Ok(state.durable_len),
            _ => Err(SegmentStoreError::NotSealed { file: self.id }),
        }
    }

    fn trailer(&self, valid_len: u64) -> Result<SegmentTrailer, SegmentStoreError> {
        let mut bytes = [0; SegmentTrailer::ENCODED_LEN];
        let at = valid_len.saturating_sub(bytes.len() as u64);
        self.read_exact(&mut bytes, at)?;
        SegmentTrailer::decode(self.version, &bytes, valid_len).map_err(self.format_error(at))
    }

    fn index<'a>(&self, buf: &'a mut Vec<u8>) -> Result<SegmentIndex<'a>, SegmentStoreError> {
        let valid_len = self.sealed_len()?;
        let trailer = self.trailer(valid_len)?;
        buf.clear();
        buf.resize(trailer.index_len() + SegmentTrailer::ENCODED_LEN, 0);
        self.read_exact(buf, trailer.index_offset())?;
        SegmentIndex::decode_tail(self.version, buf, valid_len)
            .map_err(self.format_error(trailer.index_offset()))
    }

    fn blocks_end(&self) -> Result<u64, SegmentStoreError> {
        let sealed_len = {
            let state = self.state();
            (state.phase == Phase::Sealed).then_some(state.durable_len)
        };
        match sealed_len {
            Some(valid_len) => Ok(self.trailer(valid_len)?.index_offset()),
            None => Ok(self.end.load(Ordering::Acquire)),
        }
    }

    fn live_file(&self) -> Result<LiveFile, SegmentStoreError> {
        let state = self.state();
        let file_state = state.phase.file_state();
        LiveFile::new(FileKind::Segment, self.id, file_state, state.durable_len)
            .map_err(self.format_error(0))
    }

    fn name(&self) {
        let mut state = self.state();
        if state.phase == Phase::Created {
            state.phase = Phase::Open;
            self.appendable.store(!state.failed, Ordering::Release);
        }
    }

    fn has_writer(&self) -> bool {
        self.state().has_writer
    }

    pub(super) fn has_failed(&self) -> bool {
        self.state().failed
    }

    fn check_appendable_in(&self, state: &State) -> Result<(), SegmentStoreError> {
        let file = self.id;
        match state.phase {
            _ if state.failed => Err(SegmentStoreError::SegmentFailed { file }),
            Phase::Open => Ok(()),
            Phase::Created => Err(SegmentStoreError::NotNamed { file }),
            Phase::Closed | Phase::Sealed => Err(SegmentStoreError::NotOpen { file }),
        }
    }

    pub(super) fn check_appendable(&self) -> Result<(), SegmentStoreError> {
        if self.appendable.load(Ordering::Acquire) {
            Ok(())
        } else {
            self.check_appendable_in(&self.state())
        }
    }

    pub(super) fn appended(&self, end: u64) {
        self.end.store(end, Ordering::Release);
    }

    fn fail(&self, state: &mut State) {
        state.failed = true;
        self.appendable.store(false, Ordering::Release);
        if !state.has_writer {
            self.cut(state);
        }
    }

    fn cut(&self, state: &mut State) {
        state.phase = Phase::Closed;
        state.appender = None;
        self.end.store(state.durable_len, Ordering::Release);
    }

    pub(super) fn sync(&self) -> Result<u64, SegmentStoreError> {
        let mut state = self.state();
        if !state.is_written() {
            return Ok(state.durable_len);
        }
        if state.failed {
            return Err(SegmentStoreError::SegmentFailed { file: self.id });
        }
        let end = self.end.load(Ordering::Acquire);
        if end > state.durable_len {
            if let Err(error) = state.sync_appended() {
                self.fail(&mut state);
                return Err(SegmentStoreError::io("sync", &self.path)(error));
            }
            state.durable_len = end;
            if !state.has_writer {
                state.appender = None;
            }
        }
        Ok(state.durable_len)
    }

    pub(super) fn seal_with(
        &self,
        append_footer: impl FnOnce() -> Result<u64, SegmentStoreError>,
    ) -> Result<u64, SegmentStoreError> {
        let mut state = self.state();
        self.check_appendable_in(&state)?;
        let len = append_footer()?;
        if let Err(error) = state.sync_appended() {
            self.fail(&mut state);
            return Err(SegmentStoreError::io("sync", &self.path)(error));
        }
        state.phase = Phase::Sealed;
        state.durable_len = len;
        self.appendable.store(false, Ordering::Release);
        self.end.store(len, Ordering::Release);
        Ok(len)
    }

    pub(super) fn release_writer(&self) {
        let mut state = self.state();
        state.has_writer = false;
        if state.failed && state.is_written() {
            self.cut(&mut state);
        }
        if self.end.load(Ordering::Acquire) == state.durable_len {
            state.appender = None;
        }
    }
}

#[derive(Debug, Default)]
struct Segments {
    live: BTreeMap<FileId, Arc<Segment>>,
    dropped: BTreeMap<FileId, PathBuf>,
    /// The highest id of a segment the store has had: what a manifest
    /// records.
    last_id: Option<FileId>,
}

/// The segments of one database directory.
///
/// It reads blocks and footers from the segments a manifest names, creates
/// new segments, and deletes a segment once no manifest names it. See [the
/// module documentation](super) for the life of a segment and for what the
/// caller does with the manifest.
///
/// Every method takes `&self` and may be called from any thread. Reads do
/// not wait for appends or syncs. One segment is created at a time, and
/// nothing but another creation waits for that.
#[derive(Debug)]
pub struct SegmentStore {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    read_only: bool,
    /// The highest id a creation has taken. It is held for the length of a
    /// creation, so that segments enter the store in the order of their ids,
    /// and it is taken before `segments`.
    creating: Mutex<Option<FileId>>,
    segments: RwLock<Segments>,
    /// The blocks [`Self::read`] read, and their bytes.
    reads: BlockReads,
}

/// The blocks read by their locations from the segments of a store
/// ([`SegmentStore::reads`]): what page loads and write-back read of
/// base images and delta runs. The blocks a walk passes
/// ([`SegmentStore::walk`]) are not among them.
#[derive(Debug, Default)]
struct BlockReads {
    blocks: AtomicU64,
    bytes: AtomicU64,
}

impl SegmentStore {
    /// Opens the segments named by `manifest`, in its directory, for reading
    /// and writing.
    ///
    /// Each named segment must exist, be at least as long as its valid
    /// length, and start with the header of a segment of that id. A segment
    /// an earlier session left open is closed at its valid length: this
    /// session does not append to it, and blocks past that length are not
    /// read.
    ///
    /// Then every file in `seg/` with a segment's name that the manifest does
    /// not name is removed: a crash can leave one that was created and never
    /// named, or one that was dropped and not yet deleted. If such a file
    /// has an id above the last id in the manifest and holds more than a file
    /// header, the open fails with
    /// [`SegmentStoreError::UnnamedSegmentHoldsBlocks`] and removes nothing.
    /// Other entries of `seg/` are left alone.
    ///
    /// `seg/` is created if it is missing, and its name is durable when
    /// this returns ([`list_or_create_dir`]).
    pub fn open(manifest: &ManifestStore) -> Result<Self, SegmentStoreError> {
        let current = manifest.manifest()?;
        let store = Self::load(manifest.vfs().clone(), manifest.dir(), current, false)?;
        let dir_sync = manifest.dir_sync();
        let entries = list_or_create_dir(&*store.vfs, &store.dir, &FileNames::SEGMENT, dir_sync)?;
        store.remove_unnamed(&entries, current)?;
        Ok(store)
    }

    /// Opens the segments named by `manifest` in the database directory
    /// `dir`, for reading only. It checks the segments as [`Self::open`]
    /// does and changes nothing in the directory.
    pub fn open_read_only(
        vfs: Arc<dyn Vfs>,
        dir: &Path,
        manifest: &Manifest,
    ) -> Result<Self, SegmentStoreError> {
        Self::load(vfs, dir, manifest, true)
    }

    fn load(
        vfs: Arc<dyn Vfs>,
        dir: &Path,
        manifest: &Manifest,
        read_only: bool,
    ) -> Result<Self, SegmentStoreError> {
        let last_id = manifest.last_file_ids().segment;
        let mut segments = Segments {
            last_id,
            ..Segments::default()
        };
        for named in manifest.files() {
            if named.kind() == FileKind::Segment {
                let segment = Segment::adopt(&*vfs, dir, named, manifest.format_version())?;
                segments.live.insert(named.id(), Arc::new(segment));
            }
        }
        Ok(Self {
            vfs,
            dir: dir.to_path_buf(),
            read_only,
            creating: Mutex::new(last_id),
            segments: RwLock::new(segments),
            reads: BlockReads::default(),
        })
    }

    fn remove_unnamed(
        &self,
        entries: &[DirEntry],
        manifest: &Manifest,
    ) -> Result<(), SegmentStoreError> {
        let named = named_segments(manifest);
        let last_id = manifest.last_file_ids().segment;
        let seg_dir = FileNames::SEGMENT.dir(&self.dir);
        let mut unnamed = Vec::new();
        for entry in entries {
            if entry.kind != EntryKind::File {
                continue;
            }
            let Some(id) = FileNames::SEGMENT.parse(&entry.name) else {
                continue;
            };
            if named.contains(&id) {
                continue;
            }
            let path = seg_dir.join(&entry.name);
            if Some(id) > last_id {
                let len = self
                    .vfs
                    .open(&path, OpenMode::Read)
                    .and_then(|file| file.len())
                    .map_err(SegmentStoreError::io("measure", &path))?;
                if len > DATA_START {
                    return Err(SegmentStoreError::UnnamedSegmentHoldsBlocks { path, len });
                }
            }
            unnamed.push(path);
        }
        for path in unnamed {
            remove_if_present(&*self.vfs, &path)?;
        }
        Ok(())
    }

    fn segments(&self) -> RwLockReadGuard<'_, Segments> {
        self.segments.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn segments_mut(&self) -> RwLockWriteGuard<'_, Segments> {
        self.segments
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn segment(&self, file: FileId) -> Result<Arc<Segment>, SegmentStoreError> {
        self.segments()
            .live
            .get(&file)
            .cloned()
            .ok_or(SegmentStoreError::UnknownSegment { file })
    }

    fn live(&self) -> Vec<Arc<Segment>> {
        self.segments().live.values().cloned().collect()
    }

    fn check_writable(&self) -> Result<(), SegmentStoreError> {
        if self.read_only {
            Err(SegmentStoreError::ReadOnly)
        } else {
            Ok(())
        }
    }

    fn check_same_dir(&self, manifest: &ManifestStore) -> Result<(), SegmentStoreError> {
        if manifest.dir() == self.dir {
            Ok(())
        } else {
            Err(SegmentStoreError::WrongDirectory {
                expected: self.dir.clone(),
                found: manifest.dir().to_path_buf(),
            })
        }
    }

    /// The database directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    #[cfg(test)]
    pub(super) fn is_creating(&self) -> bool {
        matches!(
            self.creating.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        )
    }

    /// Creates the next segment and returns its writer.
    ///
    /// The file is created under an id above every id used so far, with its
    /// header, and both are durable when this returns. The segment takes no
    /// block until a manifest names it: replace the manifest
    /// ([`Self::commit`]) before the first [`SegmentWriter::append`].
    ///
    /// One segment is created at a time: a second call waits for the first.
    /// The segment and its id enter the store in one step, when the file is
    /// whole. A manifest written before that step neither names the segment
    /// nor records its id, so the id is not used up before a manifest can
    /// name the segment ([`ManifestChange::FileIdReused`]).
    ///
    /// [`ManifestChange::FileIdReused`]: super::ManifestChange::FileIdReused
    pub fn create(&self) -> Result<SegmentWriter, SegmentStoreError> {
        self.check_writable()?;
        let mut taken = self.creating.lock().unwrap_or_else(PoisonError::into_inner);
        let next = taken.map_or(1, |last| last.get().wrapping_add(1));
        let id = FileId::new(next).ok_or(SegmentStoreError::FileIdsExhausted)?;
        *taken = Some(id);

        let appender = create_file(&*self.vfs, &self.dir, &FileNames::SEGMENT, id)?;
        let path = FileNames::SEGMENT.path(&self.dir, id);
        let reader = match self.vfs.open(&path, OpenMode::Read) {
            Ok(reader) => reader,
            Err(error) => {
                drop(appender);
                let _ = self.vfs.remove_file(&path);
                return Err(SegmentStoreError::io("open", &path)(error));
            }
        };
        let appender: Arc<dyn VfsFile> = Arc::from(appender);
        let segment = Arc::new(Segment::created(id, path, reader, appender.clone()));
        let mut segments = self.segments_mut();
        segments.live.insert(id, segment.clone());
        segments.last_id = Some(id);
        drop(segments);
        Ok(SegmentWriter::new(segment, appender))
    }

    /// Makes durable every block appended so far to the segments this
    /// session is writing.
    ///
    /// It may run while writers append. A block is covered if its append
    /// returned before this was called.
    ///
    /// Every segment is synced, whatever happens to another one, and the
    /// first error is returned. A segment whose sync has failed keeps
    /// failing this call until its writer is dropped.
    pub fn sync(&self) -> Result<(), SegmentStoreError> {
        let mut result = Ok(());
        for segment in self.live() {
            result = result.and(segment.sync().map(drop));
        }
        result
    }

    /// Takes the segment `file` out of the store. It is no longer read, and
    /// [`Self::describe`] no longer lists it. A walk that has begun goes on
    /// to its end.
    ///
    /// Its file is deleted by [`Self::manifest_replaced`], once the durable
    /// manifest does not name it. The segment must not have a writer.
    ///
    /// A manifest that was described before this call and installed after it
    /// still names the segment. That is the same as the replacement coming
    /// first: the file stays, a session that opens that manifest has the
    /// segment again, and the next replacement here leaves it out.
    pub fn drop_segment(&self, file: FileId) -> Result<(), SegmentStoreError> {
        self.check_writable()?;
        if self.segment(file)?.has_writer() {
            return Err(SegmentStoreError::SegmentBusy { file });
        }
        let mut segments = self.segments_mut();
        let segment = segments
            .live
            .remove(&file)
            .ok_or(SegmentStoreError::UnknownSegment { file })?;
        segments.dropped.insert(file, segment.path.clone());
        Ok(())
    }

    /// Every segment in the store as it is now, in the order of their ids:
    /// how a manifest written now would name it, whether a writer still has
    /// it, and where it ends. No file is read.
    ///
    /// A segment that is not named open and has no writer takes no more
    /// blocks, in this session or a later one.
    pub fn extents(&self) -> Vec<Extent> {
        self.live()
            .iter()
            .map(|segment| {
                let state = segment.state();
                Extent {
                    file: segment.id,
                    state: state.phase.file_state(),
                    has_writer: state.has_writer,
                    end: segment.end.load(Ordering::Acquire),
                }
            })
            .collect()
    }

    /// The file offset at which the blocks of the segment `file` end: where
    /// the footer of a sealed segment begins, which is read from its last
    /// bytes, and where any other segment ends now.
    pub fn blocks_end(&self, file: FileId) -> Result<u64, SegmentStoreError> {
        self.segment(file)?.blocks_end()
    }

    /// The segments in the store, as a manifest written now would name them:
    /// each with its state and the length of it that is durable.
    pub fn segments_named(&self) -> Result<Vec<LiveFile>, SegmentStoreError> {
        self.live()
            .iter()
            .map(|segment| segment.live_file())
            .collect()
    }

    /// The manifest `base` with its segments replaced by the ones in the
    /// store, and its last segment id raised to the highest id of a segment
    /// the store has had.
    ///
    /// A segment being written is named open, with the length that was
    /// durable when this was called. A segment whose file [`Self::create`]
    /// is still making is not in the store: it is not named and its id is
    /// not recorded. Everything else in `base` is kept.
    pub fn describe(&self, base: &Manifest) -> Result<Manifest, SegmentStoreError> {
        let mut files: Vec<_> = base
            .files()
            .iter()
            .filter(|file| file.kind() != FileKind::Segment)
            .copied()
            .collect();
        let (live, last_id) = {
            let segments = self.segments();
            let live: Vec<_> = segments.live.values().cloned().collect();
            (live, segments.last_id)
        };
        for segment in live {
            files.push(segment.live_file()?);
        }
        let mut last_file_ids = base.last_file_ids();
        last_file_ids.segment = last_file_ids.segment.max(last_id);
        Manifest::new(
            base.page_size(),
            last_file_ids,
            base.table_checkpoint(),
            files,
        )
        .map_err(SegmentStoreError::format(
            &self.dir.join(ManifestStore::FILE_NAME),
            0,
        ))
    }

    /// Tells the store that the manifest was replaced. Call it after every
    /// [`ManifestStore::update`] that returned `Ok`.
    ///
    /// A new segment that the durable manifest names takes blocks from now
    /// on. The file of a dropped segment that the durable manifest no longer
    /// names is deleted.
    ///
    /// A file that cannot be deleted does not stop the rest: the call ends
    /// with [`SegmentStoreError::NotDeleted`], and the next call tries the
    /// deletion again.
    pub fn manifest_replaced(&self, manifest: &ManifestStore) -> Result<(), SegmentStoreError> {
        self.check_writable()?;
        self.check_same_dir(manifest)?;
        let named = named_segments(manifest.manifest()?);
        for segment in self.live() {
            if named.contains(&segment.id) {
                segment.name();
            }
        }

        let unnamed = {
            let mut segments = self.segments_mut();
            let dropped = std::mem::take(&mut segments.dropped);
            let (kept, unnamed): (BTreeMap<_, _>, BTreeMap<_, _>) =
                dropped.into_iter().partition(|(id, _)| named.contains(id));
            segments.dropped = kept;
            unnamed
        };
        let mut result = Ok(());
        for (id, path) in unnamed {
            match self.vfs.remove_file(&path) {
                Err(source) if source.kind() != ErrorKind::NotFound => {
                    self.segments_mut().dropped.insert(id, path.clone());
                    result = result.and(Err(SegmentStoreError::NotDeleted { path, source }));
                }
                _ => {}
            }
        }
        result
    }

    /// Replaces the manifest with one that names the store's segments as
    /// they are now: [`ManifestStore::update`] with [`Self::describe`], then
    /// [`Self::manifest_replaced`].
    ///
    /// After [`SegmentStoreError::NotDeleted`] the new manifest is in force.
    /// After any other error [`ManifestStore::manifest`] tells where things
    /// stand: it gives the old manifest, and this call can be made again, or
    /// it fails with [`SegmentStoreError::ManifestInDoubt`], and the directory
    /// has the old manifest or the new one.
    pub fn commit(&self, manifest: &mut ManifestStore) -> Result<(), SegmentStoreError> {
        self.check_writable()?;
        self.check_same_dir(manifest)?;
        manifest.update(|current| self.describe(current))?;
        self.manifest_replaced(manifest)
    }

    /// Reads the block at `location` into `buf` and returns it, with its
    /// checksum verified.
    ///
    /// `max_payload_len` is the longest payload the caller accepts: nothing
    /// longer is read or allocated. `buf` is cleared first and can be reused
    /// from one read to the next. The location must be exactly one block,
    /// inside the segment's blocks.
    pub fn read<'a>(
        &self,
        location: BlockLocation,
        max_payload_len: usize,
        buf: &'a mut Vec<u8>,
    ) -> Result<Block<'a>, SegmentStoreError> {
        let block = self
            .segment(location.file)?
            .block(location, max_payload_len, buf)?;
        self.reads.blocks.fetch_add(1, Ordering::Relaxed);
        let bytes = u64::from(location.len);
        self.reads.bytes.fetch_add(bytes, Ordering::Relaxed);
        Ok(block)
    }

    /// Tells the file layer that the blocks at `locations` are about to be
    /// read, each with [`Self::read`]: the blocks of one page's chain, which
    /// were written at different times and so, as a rule, lie apart. A
    /// file layer that acts on the hint ([`VfsFile::will_read`]) can bring
    /// them in at the same time, where the reads that follow would
    /// otherwise wait for the device once for each block that is not in
    /// memory.
    ///
    /// It is advice: nothing is checked and nothing is reported, and
    /// neither the file layer nor the system need act on it. A location
    /// that names no segment of the store, or does not lie in its
    /// segment's file, is passed over here and refused by its read. One
    /// block alone is not worth a call, and none is made for it.
    pub fn will_read(&self, locations: &[BlockLocation]) {
        if locations.len() < 2 {
            return;
        }
        // As a read does, each call of the file layer is made with no lock
        // of the store held.
        for &location in locations {
            if let Ok(segment) = self.segment(location.file) {
                segment.will_read(location);
            }
        }
    }

    /// How many blocks [`Self::read`] has read since the store was opened,
    /// and their bytes as stored, frames included: every read that
    /// returned a block, whoever made it. Each is one read of the file
    /// layer. A read that failed is not counted, and nor are the blocks
    /// of a walk.
    pub fn reads(&self) -> (u64, u64) {
        let reads = &self.reads;
        (
            reads.blocks.load(Ordering::Relaxed),
            reads.bytes.load(Ordering::Relaxed),
        )
    }

    /// The format version the segment `file` was written in, from its file
    /// header: the version a payload read from one of its blocks is decoded
    /// under.
    pub fn format_version(&self, file: FileId) -> Result<FormatVersion, SegmentStoreError> {
        Ok(self.segment(file)?.version)
    }

    /// Reads the footer of the sealed segment `file` into `buf` and returns
    /// its index, checked against the segment's valid length.
    pub fn index<'a>(
        &self,
        file: FileId,
        buf: &'a mut Vec<u8>,
    ) -> Result<SegmentIndex<'a>, SegmentStoreError> {
        self.segment(file)?.index(buf)
    }

    /// Starts a walk over the blocks of the segment `file`, from its first
    /// block to the end of its blocks as it is now. Blocks appended later
    /// are not part of it. If the segment is cut short while the walk is
    /// under way, because a sync of it failed, the walk ends where the
    /// segment then ends.
    ///
    /// This is how a segment with no footer is read without pointers.
    /// `max_payload_len` is the longest payload the caller accepts.
    pub fn walk(
        &self,
        file: FileId,
        max_payload_len: usize,
    ) -> Result<BlockWalk, SegmentStoreError> {
        let segment = self.segment(file)?;
        let end = segment.blocks_end()?;
        Ok(BlockWalk {
            segment,
            max_payload_len,
            at: DATA_START,
            end,
            window: Vec::new(),
            window_at: DATA_START,
        })
    }
}

/// A walk over the blocks of one segment, in file order.
///
/// Every byte it passes must belong to a whole block: the first one that
/// does not ends the walk with an error.
#[derive(Debug)]
pub struct BlockWalk {
    segment: Arc<Segment>,
    max_payload_len: usize,
    at: u64,
    end: u64,
    window: Vec<u8>,
    window_at: u64,
}

impl BlockWalk {
    /// The file offset of the block [`Self::next_block`] reads next, which is
    /// where the walk ends once it has returned `None`.
    pub fn offset(&self) -> u64 {
        self.at
    }

    /// Reads the next block, with its checksum verified, or `None` after the
    /// last one.
    pub fn next_block(&mut self) -> Result<Option<(BlockLocation, Block<'_>)>, SegmentStoreError> {
        self.end = self.end.min(self.segment.end.load(Ordering::Acquire));
        if self.at >= self.end {
            return Ok(None);
        }
        let len = self.load_record()?;
        let (offset, at) = (self.at, self.window_offset());
        let (block, rest) = Block::read(
            self.segment.version,
            &self.window[at..at + len],
            self.max_payload_len,
        )
        .map_err(self.segment.format_error(offset))?;
        let len = len - rest.len();
        self.at += len as u64;
        let location = BlockLocation {
            file: self.segment.id,
            offset,
            len: len as u32,
        };
        Ok(Some((location, block)))
    }

    fn window_offset(&self) -> usize {
        (self.at - self.window_at) as usize
    }

    fn load_record(&mut self) -> Result<usize, SegmentStoreError> {
        let left = self.end - self.at;
        let frame_len = left.min(FRAME_LEN as u64) as usize;
        self.fill(frame_len)?;
        let at = self.window_offset();
        let frame = &self.window[at..at + frame_len];
        let len = match Block::read(self.segment.version, frame, self.max_payload_len) {
            Ok(_) => frame_len,
            Err(FormatError::Truncated { needed, .. }) if needed as u64 <= left => needed,
            Err(FormatError::Truncated { needed, .. }) => {
                return Err(self.segment.format_error(self.at)(FormatError::Truncated {
                    needed,
                    available: left as usize,
                }));
            }
            Err(error) => return Err(self.segment.format_error(self.at)(error)),
        };
        self.fill(len)?;
        Ok(len)
    }

    fn fill(&mut self, len: usize) -> Result<(), SegmentStoreError> {
        if self.window.len() - self.window_offset() >= len {
            return Ok(());
        }
        let left = self.end - self.at;
        let window_len = left.min(WALK_WINDOW_LEN.max(len as u64)) as usize;
        self.window.clear();
        self.window_at = self.at;
        self.window.resize(window_len, 0);
        let read = self.segment.read_exact(&mut self.window, self.at);
        if read.is_err() {
            self.window.clear();
        }
        read
    }
}

fn named_segments(manifest: &Manifest) -> BTreeSet<FileId> {
    manifest
        .files()
        .iter()
        .filter(|file| file.kind() == FileKind::Segment)
        .map(LiveFile::id)
        .collect()
}
