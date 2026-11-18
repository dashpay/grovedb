//! Checkpoints on a file system that does not fail: writing, naming,
//! replacing, discarding and loading, and every way a load is refused.

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use lignin_format::{
    manifest::{FileState, LiveFile},
    segment::{BlockContent, Compression},
    table as format, FileKind, FormatError,
};
use lignin_io::{sim::SimConfig, Create, OpenMode};

use super::*;
use crate::segment::{tests::sim::sim_fs, FileNames, ManifestChange, SegmentWriter};

/// A database directory with one segment being written and a table.
struct Fixture {
    fs: SimVfs,
    db: Db,
    table: PageTable,
    writer: SegmentWriter,
}

impl Fixture {
    fn new() -> Self {
        let fs = sim_fs(SimConfig::new(1));
        let mut db = Db::create(&fs);
        let writer = db.segments.create().expect("a segment");
        db.segments.commit(&mut db.manifest).expect("it is named");
        Self {
            fs,
            db,
            table: PageTable::new(),
            writer,
        }
    }

    fn block(&mut self, raw_page: u32, content: BlockContent, len: usize) -> BlockLocation {
        self.writer
            .append(
                page(raw_page),
                content,
                Compression::Raw,
                &vec![raw_page as u8; len],
            )
            .expect("the block is appended")
    }

    /// A base image and a delta for each of the pages 1 to 3, and page 4
    /// freed. Returns the last commit version.
    fn fill(&mut self) -> u64 {
        for raw_page in 1..=3 {
            let image = self.block(raw_page, BlockContent::Base, 100);
            let change = self.block(raw_page, BlockContent::Delta, 10);
            let born = u64::from(raw_page) * 2;
            self.table.push(page(raw_page), base(born, image)).unwrap();
            self.table
                .push(page(raw_page), delta(born + 1, change))
                .unwrap();
        }
        let image = self.block(4, BlockContent::Base, 30);
        self.table.push(page(4), base(8, image)).unwrap();
        self.table.push(page(4), free(9)).unwrap();
        9
    }

    fn checkpoint(&mut self, covers: u64) -> CheckpointInfo {
        self.db
            .checkpoint(&self.table, covers)
            .expect("the checkpoint is installed")
    }

    fn table_files(&self) -> Vec<String> {
        names_in(&self.fs, &table_dir())
    }
}

fn checkpoint_name(raw_id: u32) -> String {
    FileNames::PAGE_TABLE.file_name(id(raw_id))
}

fn checkpoint_path(raw_id: u32) -> std::path::PathBuf {
    FileNames::PAGE_TABLE.path(db_dir(), id(raw_id))
}

fn put(fs: &SimVfs, path: &Path, bytes: &[u8]) {
    let file = fs
        .open(path, OpenMode::Write(Create::IfMissing))
        .expect("the file is created");
    file.set_len(0).unwrap();
    file.write_at(bytes, 0).unwrap();
}

fn bytes_of(fs: &SimVfs, path: &Path) -> Vec<u8> {
    let file = fs.open(path, OpenMode::Read).expect("the file opens");
    let mut bytes = vec![0; file.len().unwrap() as usize];
    file.read_exact_at(&mut bytes, 0).unwrap();
    bytes
}

/// A manifest that names the checkpoint `named` and closed segments of the
/// given lengths.
fn manifest_naming(named: TableCheckpoint, segments: &[(u32, u64)]) -> Manifest {
    let files: Vec<LiveFile> = segments
        .iter()
        .map(|(raw_id, valid_len)| {
            LiveFile::new(
                FileKind::Segment,
                id(*raw_id),
                FileState::Closed,
                *valid_len,
            )
            .expect("a valid entry")
        })
        .collect();
    let last_file_ids = LastFileIds {
        segment: segments.iter().map(|(raw_id, _)| id(*raw_id)).max(),
        page_table: Some(named.file()),
        ..LastFileIds::default()
    };
    Manifest::new(last_file_ids, Some(named), files).expect("a valid manifest")
}

fn load_under(fs: &SimVfs, manifest: &Manifest) -> Result<PageTable, PageTableError> {
    PageTable::load(fs, db_dir(), manifest).map(|(table, _)| table)
}

fn format_error(error: PageTableError) -> (u64, FormatError) {
    match error {
        PageTableError::Format { offset, source, .. } => (offset, source),
        other => panic!("expected a format error, got {other:?}"),
    }
}

#[test]
fn a_checkpoint_is_written_named_and_loaded() {
    let mut fixture = Fixture::new();
    let newest = fixture.fill();
    let links = all_links(&fixture.table);
    let written = fixture.checkpoint(newest);

    assert_eq!(written.file(), id(1));
    assert_eq!(written.covers(), version(newest));
    assert_eq!(written.reaches(), Some(version(newest)));
    assert_eq!(written.links(), links.len() as u64);
    assert_eq!(
        written.segments(),
        &BTreeMap::from([(fixture.writer.file_id(), fixture.writer.end())])
    );
    let len = 20 + 8 + 1 + links.len() as u64 * 28 + 33;
    assert_eq!(written.named().valid_len(), len);
    assert_eq!(bytes_of(&fixture.fs, &checkpoint_path(1)).len() as u64, len);

    let manifest = fixture.db.manifest.manifest().unwrap();
    assert_eq!(manifest.table_checkpoint(), Some(written.named()));
    assert_eq!(manifest.last_file_ids().page_table, Some(id(1)));
    assert_eq!(fixture.table_files(), [checkpoint_name(1)]);

    let (loaded, info) = fixture.db.load().unwrap();
    assert_eq!(all_links(&loaded), links);
    assert_eq!(info, Some(written));
    assert_eq!(loaded.pages(), 4);
    assert_eq!(loaded.high_water(), Some(page(4)));
    for raw_page in 1..=4 {
        for at in 0..12 {
            assert_eq!(
                read_at(&loaded, raw_page, at),
                read_at(&fixture.table, raw_page, at)
            );
        }
    }
}

#[test]
fn installing_makes_the_blocks_durable_and_the_manifest_vouch_for_them() {
    let mut fixture = Fixture::new();
    let newest = fixture.fill();
    let segment = fixture.writer.file_id();
    let end = fixture.writer.end();
    let named_len = |db: &Db| {
        let manifest = db.manifest.manifest().unwrap();
        manifest
            .files()
            .iter()
            .find(|file| file.id() == segment)
            .unwrap()
            .valid_len()
    };
    assert_eq!(named_len(&fixture.db), 20);
    let written = fixture.checkpoint(newest);
    assert_eq!(named_len(&fixture.db), end);

    let image = fixture.fs.crash_image();
    let left = image.recover(&image.lose_all()).unwrap();
    let after = Db::open(&left).unwrap();
    let (loaded, info) = after.load().unwrap();
    assert_eq!(info, Some(written));
    let mut buf = Vec::new();
    for (_, link) in all_links(&loaded) {
        if let Some(location) = link.location() {
            after.segments.read(location, 1 << 16, &mut buf).unwrap();
        }
    }
}

#[test]
fn installing_tells_the_segment_store_of_the_new_manifest() {
    let mut fixture = Fixture::new();
    let newest = fixture.fill();
    fixture.writer.seal().unwrap();
    let second = fixture.db.segments.create().unwrap();
    let first = std::mem::replace(&mut fixture.writer, second).file_id();
    assert!(matches!(
        fixture
            .writer
            .append(page(1), BlockContent::Base, Compression::Raw, &[7; 40]),
        Err(SegmentStoreError::NotNamed { .. })
    ));
    fixture.checkpoint(newest);
    let moved = fixture.block(1, BlockContent::Base, 40);

    let mut removed = Vec::new();
    for raw_page in 1..=4 {
        fixture
            .table
            .push(page(raw_page), base(newest + 1, moved))
            .unwrap();
        fixture
            .table
            .trim(page(raw_page), version(newest + 1), &mut removed);
    }
    fixture.db.segments.drop_segment(first).unwrap();
    let seg_dir = FileNames::SEGMENT.dir(db_dir());
    assert_eq!(names_in(&fixture.fs, &seg_dir).len(), 2);
    let written = fixture.checkpoint(newest + 1);
    let second = fixture.writer.file_id();
    assert_eq!(
        names_in(&fixture.fs, &seg_dir),
        [FileNames::SEGMENT.file_name(second)]
    );
    assert_eq!(
        written.segments().keys().copied().collect::<Vec<_>>(),
        [second]
    );
    assert_eq!(fixture.db.load().unwrap().1, Some(written));
}

#[test]
fn a_database_without_a_checkpoint_has_an_empty_table() {
    let fixture = Fixture::new();
    let (loaded, info) = fixture.db.load().unwrap();
    assert_eq!(info, None);
    assert_eq!((loaded.pages(), loaded.links()), (0, 0));
    assert!(fixture.table_files().is_empty());
}

#[test]
fn a_checkpoint_of_an_empty_table_is_a_header_and_an_end_record() {
    let mut fixture = Fixture::new();
    let written = fixture.checkpoint(0);
    assert_eq!(written.named().valid_len(), 53);
    assert_eq!(written.reaches(), None);
    assert_eq!(written.links(), 0);
    assert!(written.segments().is_empty());
    let (loaded, info) = fixture.db.load().unwrap();
    assert_eq!(info, Some(written));
    assert_eq!(loaded.links(), 0);
}

#[test]
fn each_checkpoint_takes_the_next_id_and_the_one_before_is_deleted() {
    let mut fixture = Fixture::new();
    let newest = fixture.fill();
    for raw_id in 1..=4 {
        let image = fixture.block(9, BlockContent::Base, 20);
        fixture
            .table
            .push(page(9), base(newest + u64::from(raw_id), image))
            .unwrap();
        let written = fixture.checkpoint(newest + u64::from(raw_id));
        assert_eq!(written.file(), id(raw_id));
        assert_eq!(fixture.table_files(), [checkpoint_name(raw_id)]);
        assert_eq!(fixture.db.named(), Some(written.named()));
    }
    let again = Db::open(&fixture.fs).unwrap();
    let (loaded, _) = again.load().unwrap();
    assert_eq!(all_links(&loaded), all_links(&fixture.table));
    let written = again.checkpoints.write(&loaded, version(0)).unwrap();
    assert_eq!(written.file(), id(5));
}

#[test]
fn opening_removes_the_checkpoints_the_manifest_does_not_name() {
    let mut fixture = Fixture::new();
    let newest = fixture.fill();
    fixture.checkpoint(newest);
    let written = fixture.checkpoint(newest);
    assert_eq!(written.file(), id(2));
    let Fixture { fs, db, table, .. } = fixture;

    let unnamed = db.checkpoints.write(&table, version(newest)).unwrap();
    assert_eq!(unnamed.file(), id(3));
    put(&fs, &checkpoint_path(1), b"left by a crash");
    put(&fs, &checkpoint_path(7), &[]);
    put(
        &fs,
        &table_dir().join("000000002.pt"),
        b"not a checkpoint's name",
    );
    put(&fs, &table_dir().join("notes"), b"not ours");
    fs.create_dir(&table_dir().join("00000009.pt")).unwrap();
    drop(db);

    let db = Db::open(&fs).unwrap();
    assert_eq!(
        names_in(&fs, &table_dir()),
        ["000000002.pt", "00000002.pt", "00000009.pt", "notes"]
    );
    let (loaded, info) = db.load().unwrap();
    assert_eq!(info, Some(written));
    assert_eq!(all_links(&loaded), all_links(&table));
    let next = db.checkpoints.write(&loaded, version(newest)).unwrap();
    assert_eq!(next.file(), id(3));
}

#[test]
fn a_checkpoint_the_manifest_cannot_vouch_for_is_not_installed() {
    let mut fixture = Fixture::new();
    let newest = fixture.fill();
    let first = fixture.checkpoint(newest);
    let segment = fixture.writer.file_id();
    let end = fixture.writer.end();

    let past_the_end = BlockLocation {
        file: segment,
        offset: end,
        len: 64,
    };
    fixture.table.push(page(8), base(10, past_the_end)).unwrap();
    let written = fixture
        .db
        .checkpoints
        .write(&fixture.table, version(10))
        .unwrap();
    let db = &mut fixture.db;
    let error = db
        .checkpoints
        .install(&written, LOG, &mut db.manifest, &db.segments)
        .unwrap_err();
    assert!(matches!(
        error,
        PageTableError::NotCovered { segment: found, needed, valid_len: Some(valid_len) }
            if found == segment && needed == end + 64 && valid_len == end
    ));
    assert_eq!(db.named(), Some(first.named()));
    assert_eq!(
        fixture.table_files(),
        [checkpoint_name(1), checkpoint_name(2)]
    );

    let db = &mut fixture.db;
    db.checkpoints.discard(written, &db.manifest).unwrap();
    assert_eq!(fixture.table_files(), [checkpoint_name(1)]);

    let mut removed = Vec::new();
    fixture.table.trim(page(8), version(10), &mut removed);
    fixture.table.discard_after(version(9));
    let nowhere = at(40, 20);
    fixture.table.push(page(8), base(10, nowhere)).unwrap();
    let written = fixture
        .db
        .checkpoints
        .write(&fixture.table, version(10))
        .unwrap();
    assert_eq!(written.file(), id(3));
    let db = &mut fixture.db;
    let error = db
        .checkpoints
        .install(&written, LOG, &mut db.manifest, &db.segments)
        .unwrap_err();
    assert!(matches!(
        error,
        PageTableError::NotCovered { segment, needed: 84, valid_len: None } if segment == id(40)
    ));

    fixture.table.discard_after(version(9));
    let good = fixture.block(8, BlockContent::Base, 5);
    fixture.table.push(page(8), base(10, good)).unwrap();
    let db = &mut fixture.db;
    db.checkpoints
        .install(&written, LOG, &mut db.manifest, &db.segments)
        .unwrap_err();
    let second = fixture.checkpoint(10);
    assert_eq!(second.file(), id(4));
    assert_eq!(
        fixture.table_files(),
        [checkpoint_name(3), checkpoint_name(4)]
    );
    let db = &fixture.db;
    db.checkpoints.discard(written, &db.manifest).unwrap();
    db.checkpoints
        .discard(second.clone(), &db.manifest)
        .unwrap();
    assert_eq!(fixture.table_files(), [checkpoint_name(4)]);
    assert_eq!(fixture.db.load().unwrap().1, Some(second));
}

#[test]
fn a_checkpoint_never_covers_less_than_the_one_before_it() {
    let mut fixture = Fixture::new();
    let newest = fixture.fill();
    let first = fixture.checkpoint(newest);
    let written = fixture
        .db
        .checkpoints
        .write(&fixture.table, version(newest - 1))
        .unwrap();
    let db = &mut fixture.db;
    let error = db
        .checkpoints
        .install(&written, LOG, &mut db.manifest, &db.segments)
        .unwrap_err();
    assert!(matches!(
        error,
        PageTableError::CoversWentBack { current, offered }
            if current == version(newest) && offered == version(newest - 1)
    ));
    assert_eq!(db.named(), Some(first.named()));
    let same = fixture.checkpoint(newest);
    assert_eq!(same.file(), id(3));
    assert_eq!(
        fixture.table_files(),
        [checkpoint_name(2), checkpoint_name(3)]
    );
}

#[test]
fn a_checkpoint_is_not_named_before_the_log_is_durable_through_all_it_holds() {
    let mut fixture = Fixture::new();
    let old = fixture.block(1, BlockContent::Base, 50);
    let change = fixture.block(1, BlockContent::Delta, 9);
    let new = fixture.block(1, BlockContent::Base, 50);
    fixture.table.push(page(1), base(1, old)).unwrap();
    fixture.table.push(page(1), delta(2, change)).unwrap();
    fixture.table.push(page(1), base(3, new)).unwrap();
    let mut removed = Vec::new();
    fixture.table.trim(page(1), version(3), &mut removed);
    assert_eq!(chain_of(&fixture.table, 1), [base(3, new)]);

    let db = &mut fixture.db;
    let written = db.checkpoints.write(&fixture.table, version(2)).unwrap();
    assert_eq!(
        (written.covers(), written.reaches()),
        (version(2), Some(version(3)))
    );
    for durable in [0, 2] {
        let error = db
            .checkpoints
            .install(&written, version(durable), &mut db.manifest, &db.segments)
            .unwrap_err();
        assert!(matches!(
            error,
            PageTableError::LogBehind { needed, durable: found }
                if needed == version(3) && found == version(durable)
        ));
        assert_eq!(db.named(), None);
    }
    db.checkpoints
        .install(&written, version(3), &mut db.manifest, &db.segments)
        .unwrap();
    assert_eq!(db.named(), Some(written.named()));

    let ahead = db.checkpoints.write(&fixture.table, version(5)).unwrap();
    let error = db
        .checkpoints
        .install(&ahead, version(4), &mut db.manifest, &db.segments)
        .unwrap_err();
    assert!(matches!(
        error,
        PageTableError::LogBehind { needed, .. } if needed == version(5)
    ));
    assert_eq!(db.named(), Some(written.named()));
    db.checkpoints
        .install(&ahead, version(5), &mut db.manifest, &db.segments)
        .unwrap();

    let empty = db.checkpoints.write(&PageTable::new(), version(6)).unwrap();
    assert_eq!(empty.reaches(), None);
    let error = db
        .checkpoints
        .install(&empty, version(5), &mut db.manifest, &db.segments)
        .unwrap_err();
    assert!(matches!(
        error,
        PageTableError::LogBehind { needed, .. } if needed == version(6)
    ));
    db.checkpoints.discard(empty, &db.manifest).unwrap();
    assert_eq!(fixture.table_files(), [checkpoint_name(2)]);
}

#[test]
fn an_older_checkpoint_does_not_replace_a_newer_one() {
    let mut fixture = Fixture::new();
    let newest = fixture.fill();
    let db = &mut fixture.db;
    let older = db
        .checkpoints
        .write(&fixture.table, version(newest))
        .unwrap();
    let newer = db
        .checkpoints
        .write(&fixture.table, version(newest))
        .unwrap();
    db.checkpoints
        .install(&newer, LOG, &mut db.manifest, &db.segments)
        .unwrap();
    assert_eq!(
        names_in(&fixture.fs, &table_dir()),
        ["00000001.pt", "00000002.pt"]
    );
    let error = db
        .checkpoints
        .install(&older, LOG, &mut db.manifest, &db.segments)
        .unwrap_err();
    assert!(matches!(
        store_error(error),
        SegmentStoreError::ManifestChange(ManifestChange::FileIdReused {
            kind: FileKind::PageTable,
            ..
        })
    ));
    assert_eq!(db.named(), Some(newer.named()));
    db.checkpoints
        .install(&newer, LOG, &mut db.manifest, &db.segments)
        .unwrap();
    db.checkpoints.discard(older, &db.manifest).unwrap();
    assert_eq!(names_in(&fixture.fs, &table_dir()), ["00000002.pt"]);
}

#[test]
fn the_parts_of_another_directory_are_refused() {
    let mut fixture = Fixture::new();
    let other = Path::new("/other");
    fixture.fs.create_dir(other).unwrap();
    let mut elsewhere = ManifestStore::create(arc(&fixture.fs), other, empty_manifest()).unwrap();
    let segments_elsewhere = SegmentStore::open(&elsewhere).unwrap();
    let written = fixture
        .db
        .checkpoints
        .write(&fixture.table, version(0))
        .unwrap();
    let db = &mut fixture.db;

    let wrong = |error: PageTableError| {
        assert!(matches!(
            store_error(error),
            SegmentStoreError::WrongDirectory { expected, found }
                if expected == db_dir() && found == other
        ));
    };
    wrong(
        db.checkpoints
            .install(&written, LOG, &mut elsewhere, &db.segments)
            .unwrap_err(),
    );
    wrong(
        db.checkpoints
            .install(&written, LOG, &mut db.manifest, &segments_elsewhere)
            .unwrap_err(),
    );
    wrong(db.checkpoints.manifest_replaced(&elsewhere).unwrap_err());
    wrong(
        db.checkpoints
            .discard(written.clone(), &elsewhere)
            .unwrap_err(),
    );
    assert_eq!(db.checkpoints.dir(), db_dir());
    db.checkpoints
        .install(&written, LOG, &mut db.manifest, &db.segments)
        .unwrap();
}

#[test]
fn a_checkpoint_of_many_pages_takes_several_records() {
    let fs = sim_fs(SimConfig::new(2));
    let db = Db::create(&fs);
    let table = PageTable::new();
    let pages = 2 * format::LINKS_PER_RECORD as u32 + 300;
    for raw_page in 1..=pages {
        let file = 1 + raw_page % 3;
        let image = at(file, 20 + u64::from(raw_page) * 64);
        table.push(page(raw_page), base(5, image)).unwrap();
        if raw_page % 4 == 0 {
            table
                .push(page(raw_page), delta(6, at(4, 20 + u64::from(raw_page))))
                .unwrap();
        }
    }
    let far = page(3_000_000);
    table.push(far, base(7, at(9, 1 << 40))).unwrap();
    let written = db.checkpoints.write(&table, version(6)).unwrap();
    assert_eq!(written.links(), u64::from(pages + pages / 4 + 1));
    assert_eq!(written.reaches(), Some(version(7)));
    let extents: Vec<(u32, u64)> = written
        .segments()
        .iter()
        .map(|(file, end)| (file.get(), *end))
        .collect();
    assert_eq!(extents.len(), 5);
    assert_eq!(extents[3], (4, 20 + u64::from(pages / 4 * 4) + 64));
    assert_eq!(extents[4], (9, (1 << 40) + 64));

    let manifest = manifest_naming(written.named(), &extents);
    let (loaded, info) = PageTable::load(&fs, db_dir(), &manifest).unwrap();
    assert_eq!(info, Some(written.clone()));
    assert_eq!(all_links(&loaded), all_links(&table));
    assert_eq!(loaded.high_water(), Some(far));

    for cut in 0..extents.len() {
        let mut short = extents.clone();
        short[cut].1 -= 1;
        let error = load_under(&fs, &manifest_naming(written.named(), &short)).unwrap_err();
        assert!(matches!(
            error,
            PageTableError::NotCovered { segment, needed, valid_len: Some(valid_len) }
                if (segment.get(), needed) == extents[cut] && valid_len + 1 == needed
        ));
        let mut missing = extents.clone();
        missing.remove(cut);
        let error = load_under(&fs, &manifest_naming(written.named(), &missing)).unwrap_err();
        assert!(matches!(
            error,
            PageTableError::NotCovered { segment, valid_len: None, .. }
                if segment.get() == extents[cut].0
        ));
    }
}

#[test]
fn a_checkpoint_written_while_the_table_changes_holds_each_chain_as_it_once_was() {
    let fs = sim_fs(SimConfig::new(3));
    let db = Db::create(&fs);
    let table = PageTable::new();
    let pages = 3 * format::LINKS_PER_RECORD as u32;
    let location = |raw_page: u32, born: u64| at(1, 20 + (u64::from(raw_page) * 1000 + born) * 64);
    for raw_page in 1..=pages {
        table
            .push(page(raw_page), base(0, location(raw_page, 0)))
            .unwrap();
    }
    let done = AtomicBool::new(false);
    let mut checkpoints = Vec::new();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut born = 0;
            while !done.load(Ordering::Acquire) && born < 900 {
                born += 1;
                for raw_page in (1..=pages).step_by(97) {
                    table
                        .push(page(raw_page), delta(born, location(raw_page, born)))
                        .unwrap();
                }
            }
        });
        for _ in 0..4 {
            checkpoints.push(db.checkpoints.write(&table, version(0)).unwrap());
        }
        done.store(true, Ordering::Release);
    });

    let whole = manifest_naming(checkpoints[0].named(), &[(1, u64::MAX)]);
    for written in checkpoints {
        let manifest = manifest_naming(written.named(), &[(1, u64::MAX)]);
        let (loaded, info) = PageTable::load(&fs, db_dir(), &manifest).unwrap();
        assert_eq!(info.as_ref(), Some(&written));
        let mut newest = None;
        for raw_page in 1..=pages {
            let chain = chain_of(&loaded, raw_page);
            let now = chain_of(&table, raw_page);
            assert!(!chain.is_empty() && chain.len() <= now.len());
            assert_eq!(chain, now[..chain.len()]);
            newest = newest.max(chain.last().map(|link| link.version));
        }
        assert_eq!(written.reaches(), newest);
        assert_eq!(written.links(), loaded.links());
    }
    assert!(load_under(&fs, &whole).is_ok());
}

/// A page whose chain is `base(b)`, then a delta for each version up to
/// `n`: the blocks of the page version that a reader of the newest version
/// is given must be exactly those, with none left out.
#[test]
fn a_checkpoint_written_while_chains_are_consolidated_and_trimmed_holds_whole_page_versions() {
    let fs = sim_fs(SimConfig::new(5));
    let db = Db::create(&fs);
    let table = PageTable::new();
    let pages = 2 * format::LINKS_PER_RECORD as u32;
    let block = |file: u32, raw_page: u32, born: u64| {
        at(file, 20 + (u64::from(raw_page) * 1000 + born) * 64)
    };
    let born_of = |location: BlockLocation| ((location.offset - 20) / 64) % 1000;
    for raw_page in 1..=pages {
        table
            .push(page(raw_page), base(0, block(1, raw_page, 0)))
            .unwrap();
    }
    let done = AtomicBool::new(false);
    let mut checkpoints = Vec::new();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut removed = Vec::new();
            let mut born = 0;
            while !done.load(Ordering::Acquire) && born < 900 {
                born += 1;
                for raw_page in (1..=pages).step_by(53) {
                    let change = block(1, raw_page, born);
                    table.push(page(raw_page), delta(born, change)).unwrap();
                    if born % 3 == 0 {
                        let image = block(2, raw_page, born);
                        table.consolidate(page(raw_page), change, image).unwrap();
                    }
                    if born % 6 == 0 {
                        table.trim(page(raw_page), version(born), &mut removed);
                    }
                    if born % 30 == 0 {
                        table.push(page(raw_page), free(born)).unwrap();
                        table.trim(page(raw_page), version(born), &mut removed);
                        let image = block(2, raw_page, born);
                        table.push(page(raw_page), base(born, image)).unwrap();
                    }
                }
            }
        });
        for _ in 0..6 {
            let started_at = table.newest(page(1)).unwrap().version;
            let written = db.checkpoints.write(&table, started_at).unwrap();
            checkpoints.push((started_at, written));
        }
        done.store(true, Ordering::Release);
    });

    let (mut consolidated, mut freed) = (0, 0);
    for (started_at, written) in checkpoints {
        let manifest = manifest_naming(written.named(), &[(1, u64::MAX), (2, u64::MAX)]);
        let (loaded, _) = PageTable::load(&fs, db_dir(), &manifest).unwrap();
        let mut newest = None;
        for raw_page in (1..=pages).step_by(53) {
            let Some((born, blocks)) = read_at(&loaded, raw_page, u64::MAX - 1) else {
                freed += 1;
                continue;
            };
            assert!(born.get() + 1 >= started_at.get(), "page {raw_page}");
            let first = born_of(blocks[0]);
            assert_eq!(blocks[0].file, id(if first == 0 { 1 } else { 2 }));
            for (step, block) in blocks.iter().enumerate().skip(1) {
                assert_eq!((block.file, born_of(*block)), (id(1), first + step as u64));
            }
            assert_eq!(born.get(), first + blocks.len() as u64 - 1);
            consolidated += usize::from(first != 0);
            newest = newest.max(Some(born));
        }
        assert!(written.reaches() >= newest);
    }
    println!("{consolidated} consolidated chains, {freed} pages found freed");
    assert!(consolidated > 0);
}

#[test]
fn a_load_is_refused_when_the_file_is_not_the_checkpoint_the_manifest_names() {
    let mut fixture = Fixture::new();
    let newest = fixture.fill();
    let written = fixture.checkpoint(newest);
    let fs = fixture.fs.clone();
    let good = fixture.db.manifest.manifest().unwrap().clone();
    let segments: Vec<(u32, u64)> = written
        .segments()
        .iter()
        .map(|(file, end)| (file.get(), *end))
        .collect();
    let path = checkpoint_path(1);
    let bytes = bytes_of(&fs, &path);
    let len = bytes.len() as u64;
    let named = |valid_len: u64, covers: u64| {
        let named = TableCheckpoint::new(id(1), valid_len, version(covers)).unwrap();
        manifest_naming(named, &segments)
    };
    assert!(load_under(&fs, &good).is_ok());
    assert!(load_under(&fs, &named(len, newest)).is_ok());

    let error = load_under(&fs, &named(len, newest + 1)).unwrap_err();
    assert!(matches!(
        error,
        PageTableError::CoversMismatch { manifest, file, .. }
            if manifest == version(newest + 1) && file == version(newest)
    ));
    let error = load_under(&fs, &named(len + 1, newest)).unwrap_err();
    assert!(matches!(
        error,
        PageTableError::FileTooShort { valid_len, len: found, .. }
            if valid_len == len + 1 && found == len
    ));

    let end_at = len - format::END_RECORD_LEN as u64;
    for (valid_len, offset) in [
        (20, 20),
        (27, 20),
        (28, 20),
        (end_at - 1, 20),
        (end_at, end_at),
        (end_at + 7, end_at),
        (len - 1, end_at),
    ] {
        let (found_at, source) =
            format_error(load_under(&fs, &named(valid_len, newest)).unwrap_err());
        assert_eq!(found_at, offset, "valid length {valid_len}");
        assert!(matches!(source, FormatError::Truncated { .. }), "{source}");
    }

    let mut longer = bytes.clone();
    longer.extend_from_slice(&[0; 40]);
    put(&fs, &path, &longer);
    assert!(load_under(&fs, &named(len, newest)).is_ok());
    let (offset, source) = format_error(load_under(&fs, &named(len + 40, newest)).unwrap_err());
    assert_eq!(
        (offset, source),
        (
            len,
            format::TableError::LengthMismatch {
                expected: len,
                found: len + 40,
            }
            .into()
        )
    );

    let mut damaged = bytes.clone();
    damaged[40] ^= 1;
    put(&fs, &path, &damaged);
    let (offset, source) = format_error(load_under(&fs, &good).unwrap_err());
    assert_eq!(offset, 20);
    assert!(matches!(source, FormatError::ChecksumMismatch { .. }));

    let other_id = format::encode(id(2), version(newest), []).unwrap();
    put(&fs, &path, &other_id);
    let (offset, source) = format_error(load_under(&fs, &named(53, newest)).unwrap_err());
    assert_eq!(
        (offset, source),
        (
            0,
            format::TableError::WrongFileId {
                expected: 1,
                found: 2,
            }
            .into()
        )
    );

    put(&fs, &path, &bytes[..10]);
    let error = load_under(&fs, &good).unwrap_err();
    assert!(matches!(
        error,
        PageTableError::FileTooShort { len: 10, .. }
    ));

    fs.remove_file(&path).unwrap();
    let error = load_under(&fs, &good).unwrap_err();
    assert!(matches!(
        error,
        PageTableError::Io { op: "open", ref source, .. }
            if source.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
fn checkpoint_ids_run_out() {
    let fs = sim_fs(SimConfig::new(4));
    let last_file_ids = LastFileIds {
        page_table: Some(id(u32::MAX - 1)),
        ..LastFileIds::default()
    };
    let manifest = Manifest::new(last_file_ids, None, Vec::new()).unwrap();
    let manifest = ManifestStore::create(arc(&fs), db_dir(), manifest).unwrap();
    let mut db = Db::with(manifest).unwrap();
    let table = PageTable::new();
    let last = db.checkpoint(&table, 0).unwrap();
    assert_eq!(last.file(), id(u32::MAX));
    assert_eq!(names_in(&fs, &table_dir()), ["4294967295.pt"]);
    for _ in 0..2 {
        assert!(matches!(
            db.checkpoints.write(&table, version(0)),
            Err(PageTableError::FileIdsExhausted)
        ));
    }
    assert_eq!(db.load().unwrap().1, Some(last));
}
