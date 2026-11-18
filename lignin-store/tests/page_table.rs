//! The size of the page table for one million pages, in memory and as a
//! checkpoint file, and a measurement of its lookups.
//!
//! These run against the crate as it is built for use, in a process of
//! their own.

use std::{sync::Arc, time::Instant};

use lignin_format::{
    manifest::{FileState, LastFileIds, LiveFile, Manifest},
    CommitVersion, FileId, FileKind, PageId,
};
use lignin_io::{StdVfs, Vfs};
use lignin_store::{
    segment::{BlockLocation, FileNames, ManifestStore},
    table::{Link, PageTable, TableCheckpoints},
};

const PAGES: u32 = 1_000_000;
const LIMIT: u64 = 32_000_000;
const SEGMENTS: u32 = 64;
const BLOCK_LEN: u32 = 4104;

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

/// Where the image of page `raw` is: 4 KiB pages spread over 64 segments.
fn image_of(raw: u32) -> BlockLocation {
    BlockLocation {
        file: FileId::new(1 + raw % SEGMENTS).expect("not zero"),
        offset: 20 + u64::from(raw / SEGMENTS) * u64::from(BLOCK_LEN),
        len: BLOCK_LEN,
    }
}

fn table_of(pages: u32) -> PageTable {
    let table = PageTable::new();
    for raw in 1..=pages {
        let born = version(u64::from(raw % 1000));
        table
            .push(page(raw), Link::base(born, image_of(raw)))
            .expect("a first link");
    }
    table
}

/// The resident memory of this process, where the system tells.
fn resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[test]
fn the_table_for_a_million_pages_fits_in_32_megabytes() {
    let before = resident_bytes();
    let start = Instant::now();
    let table = table_of(PAGES);
    let built = start.elapsed();
    let in_memory = table.memory_usage() as u64;
    println!(
        "in memory: {in_memory} bytes for {} pages and {} links, built in {built:?}",
        table.pages(),
        table.links()
    );
    if let (Some(before), Some(after)) = (before, resident_bytes()) {
        println!(
            "resident memory grew by {} bytes",
            after.saturating_sub(before)
        );
    }
    assert_eq!(
        (table.pages(), table.links()),
        (u64::from(PAGES), u64::from(PAGES))
    );
    assert!(in_memory <= LIMIT, "{in_memory}");
    assert!(in_memory >= u64::from(PAGES) * PageTable::LINK_BYTES as u64);

    let dir = tempfile::tempdir().expect("a directory");
    let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
    let empty = Manifest::new(LastFileIds::default(), None, Vec::new()).expect("a manifest");
    let manifest = ManifestStore::create(vfs.clone(), dir.path(), empty).expect("a manifest");
    let checkpoints = TableCheckpoints::open(&manifest).expect("the checkpoints open");
    let start = Instant::now();
    let written = checkpoints
        .write(&table, version(999))
        .expect("the checkpoint is written");
    let wrote = start.elapsed();
    let path = FileNames::PAGE_TABLE.path(dir.path(), written.file());
    let on_disk = std::fs::metadata(&path).expect("the file").len();
    println!("on disk: {on_disk} bytes, written and synced in {wrote:?}");
    assert_eq!(on_disk, written.named().valid_len());
    assert_eq!(on_disk, 28_002_258);
    assert!(on_disk <= LIMIT);

    let segments: Vec<LiveFile> = written
        .segments()
        .iter()
        .map(|(file, end)| {
            LiveFile::new(FileKind::Segment, *file, FileState::Closed, *end).expect("an entry")
        })
        .collect();
    assert_eq!(segments.len(), SEGMENTS as usize);
    let last_file_ids = LastFileIds {
        segment: FileId::new(SEGMENTS),
        page_table: Some(written.file()),
        ..LastFileIds::default()
    };
    let named = Manifest::new(last_file_ids, Some(written.named()), segments).expect("a manifest");
    let start = Instant::now();
    let (loaded, info) = PageTable::load(&*vfs, dir.path(), &named).expect("the checkpoint loads");
    println!("loaded in {:?}", start.elapsed());
    assert_eq!(info, Some(written));
    assert_eq!(loaded.memory_usage() as u64, in_memory);
    let mut chain = Vec::new();
    for raw in (1..=PAGES).step_by(997).chain([PAGES]) {
        let born = loaded.read(page(raw), version(999), &mut chain);
        assert_eq!(born, Some(version(u64::from(raw % 1000))));
        assert_eq!(chain, [image_of(raw)]);
    }

    let room = (LIMIT - in_memory) / PageTable::LINK_BYTES as u64;
    println!("room for {room} more links below {LIMIT} bytes");
    for raw in (1..=PAGES).step_by(10) {
        let change = BlockLocation {
            len: 56,
            ..image_of(raw)
        };
        table
            .push(page(raw), Link::delta(version(1000), change))
            .expect("a delta");
    }
    let with_deltas = table.memory_usage() as u64;
    println!(
        "with a delta on every tenth page: {with_deltas} bytes for {} links",
        table.links()
    );
    assert!(with_deltas <= LIMIT, "{with_deltas}");
    assert!(room >= u64::from(PAGES) / 10);
}

/// Prints what a lookup costs in a table of `LIGNIN_TABLE_PAGES` pages
/// (one million unless set), over `LIGNIN_TABLE_LOOKUPS` lookups of drawn
/// page ids. Run it under `perf stat -e instructions:u` to count the
/// instructions, with the lookups set to 0 for what building the table
/// takes.
#[test]
#[ignore = "a measurement, run by hand"]
fn measure_lookups() {
    let setting = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let pages = setting("LIGNIN_TABLE_PAGES", u64::from(PAGES)) as u32;
    let lookups = setting("LIGNIN_TABLE_LOOKUPS", 50_000_000);
    let table = table_of(pages);

    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut sum = 0u64;
    let start = Instant::now();
    for _ in 0..lookups {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let raw = 1 + (state % u64::from(pages)) as u32;
        let newest = table.newest(page(raw)).expect("the page has a link");
        sum = sum.wrapping_add(newest.version.get());
    }
    let elapsed = start.elapsed();
    println!(
        "{pages} pages, {lookups} lookups in {elapsed:?}: {:.1} ns each (sum {sum})",
        elapsed.as_nanos() as f64 / lookups.max(1) as f64
    );
}
