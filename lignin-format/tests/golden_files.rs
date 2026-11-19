//! The golden files of each format version, in `fuzz/golden/v<N>/`.
//!
//! Each file is written here from fixed contents with the crate's own
//! writers. The test holds every committed file to two things: it decodes to
//! those contents, and the writers still write it byte for byte. Segments are
//! the exception to the second: which LZ4 stream a writer produces is not
//! part of the format (`segment-manifest.md`, section 5), so a segment is
//! checked by decoding it alone.
//!
//! The files are also the seeds of the fuzz targets (`fuzz/seed.sh`).
//! Version 1 is open until the first release (U10 of `format.md`): while it
//! is, `LIGNIN_WRITE_GOLDEN=1 cargo test -p lignin-format --test
//! golden_files` writes the files again after a deliberate change of a
//! layout. From the release on, the files of a version never change, and a
//! new version adds its own directory (U9).

use std::{env, fs, path::PathBuf};

use lignin_format::{
    delta::{consolidate, Delta, DeltaRun, DeltaRunWriter, RootList},
    flat_page::{FlatBranchBuilder, FlatLeafBuilder, FlatPage, FlatValue},
    manifest::{FileState, LastFileIds, LiveFile, Manifest, TableCheckpoint},
    segment::{self, Block, BlockContent, Compression, IndexEntry, SegmentIndex},
    table::{self, BlockPointer, Checkpoint, Link, LinkKind, ValueCount},
    tree_page::{CellContent, CellValue, HashField, Side, TreeLink, TreePage, TreePageMut},
    value_log::{self, ValuePlace, ValuePointer},
    wal::{
        self, write_file_start, write_reclaim, CarriedValue, CheckpointMark, FlatChange, FlatRun,
        FlatRunWriter, GroupWriter, LogItem, LogPlace, LogStart, Removed, ValueMove,
    },
    CellAddress, CommitVersion, FileHeader, FileId, FileKind, FormatVersion, PageId, PageSize,
    SlotId,
};

const V1: FormatVersion = FormatVersion::V1;
const SIZE: PageSize = PageSize::DEFAULT;

fn page(id: u32) -> PageId {
    PageId::new(id).expect("not zero")
}

fn file(id: u32) -> FileId {
    FileId::new(id).expect("a file id")
}

fn slot(id: u16) -> SlotId {
    SlotId::new(id).expect("not zero")
}

fn address(page_id: u32, slot_id: u16) -> CellAddress {
    CellAddress::new(page(page_id), slot(slot_id))
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn pointer(file_id: u32, offset: u64, len: u32) -> ValuePointer {
    ValuePointer::new(file(file_id), offset, len).expect("a pointer")
}

/// The 300 bytes of the value that page 7 keeps out of line.
fn long_value() -> Vec<u8> {
    (0..300u32).map(|i| (i * 7 % 251) as u8).collect()
}

/// Page 7 of `tree-page.md` 2.5: a node with a left child in the same page,
/// hosted roots, a feature and a value out of line, and a free slot.
fn tree_page() -> Vec<u8> {
    let mut frame = vec![0; SIZE.image_capacity()];
    let mut page = TreePageMut::init(&mut frame, SIZE, page(7), 2).expect("a frame");
    let roots = [Some(address(9, 1)), None];
    let node = CellContent {
        key: b"ab",
        feature: &[0x03, 0x2A],
        value_hash: &[0x11; 32],
        kv_hash: &[0x22; 32],
        left: Some(TreeLink {
            child: address(7, 3),
            hash: &[0x33; 32],
            heights: (1, 0),
            child_key_len: 3,
            aggregate: &[0x05],
        }),
        right: None,
        hosted_roots: &roots,
        value: CellValue::OutOfLine(pointer(12, 20, 300)),
        fragment_root: true,
    };
    let leaf = CellContent {
        key: b"abc",
        feature: &[],
        value_hash: &[0x44; 32],
        kv_hash: &[0x55; 32],
        left: None,
        right: None,
        hosted_roots: &[],
        value: CellValue::Inline(b"v"),
        fragment_root: false,
    };
    page.insert(slot(1), &node).expect("room");
    page.insert(slot(3), &leaf).expect("room");
    page.page().to_image()
}

/// A run of page 7 with a record of every kind that changes a cell
/// (`page-delta.md` 2.4), which applies to [`tree_page`].
fn tree_run() -> Vec<u8> {
    let link = |child, hash, heights, child_key_len, aggregate| TreeLink {
        child,
        hash,
        heights,
        child_key_len,
        aggregate,
    };
    let inserted = cell(b"b", b"w", [0xCC; 32], [0xDD; 32], false);
    let replaced = cell(b"b", b"wx", [0xCC; 32], [0xEE; 32], true);
    let roots = [None, Some(address(11, 3))];
    let mut bytes = Vec::new();
    let mut run = DeltaRunWriter::new(page(7), None, &mut bytes);
    let deltas = [
        Delta::Hash {
            slot: slot(1),
            field: HashField::Value,
            hash: &[0x66; 32],
        },
        Delta::Hash {
            slot: slot(1),
            field: HashField::KeyValue,
            hash: &[0x77; 32],
        },
        Delta::Hash {
            slot: slot(1),
            field: HashField::Child(Side::Left),
            hash: &[0x88; 32],
        },
        Delta::Child {
            slot: slot(1),
            side: Side::Left,
            child: address(7, 2),
        },
        Delta::Link {
            slot: slot(1),
            side: Side::Right,
            link: Some(link(address(9, 5), &[0x99; 32], (2, 1), 4, &[0x2A][..])),
        },
        Delta::Hash {
            slot: slot(1),
            field: HashField::Child(Side::Right),
            hash: &[0xAA; 32],
        },
        Delta::Child {
            slot: slot(1),
            side: Side::Right,
            child: address(10, 1),
        },
        Delta::Link {
            slot: slot(1),
            side: Side::Left,
            link: None,
        },
        Delta::Link {
            slot: slot(3),
            side: Side::Left,
            link: Some(link(address(7, 2), &[0xBB; 32], (0, 0), 1, &[][..])),
        },
        Delta::Link {
            slot: slot(1),
            side: Side::Right,
            link: None,
        },
        Delta::FragmentRoot {
            slot: slot(1),
            fragment_root: false,
        },
        Delta::FragmentRoot {
            slot: slot(3),
            fragment_root: true,
        },
        Delta::HostedRoot {
            slot: slot(1),
            index: 1,
            root: Some(address(8, 2)),
        },
        Delta::HostedRoot {
            slot: slot(1),
            index: 0,
            root: None,
        },
        Delta::HostedRoots {
            slot: slot(3),
            roots: RootList::from(&roots[..]),
        },
        Delta::Feature {
            slot: slot(1),
            feature: &[0x01, 0x02, 0x03],
        },
        Delta::Value {
            slot: slot(3),
            value: CellValue::Inline(b"vw"),
        },
        Delta::Value {
            slot: slot(1),
            value: CellValue::OutOfLine(pointer(13, 64, 300)),
        },
        Delta::Insert {
            slot: slot(2),
            cell: decoded_cell(&inserted),
        },
        Delta::Replace {
            slot: slot(2),
            cell: decoded_cell(&replaced),
        },
        Delta::Remove { slot: slot(3) },
    ];
    for delta in &deltas {
        run.push(delta).expect("a delta");
    }
    run.finish().expect("a run");
    bytes
}

/// A run that creates page 12 and gives it a cell whose value is pending.
fn created_run() -> Vec<u8> {
    let pending = ValuePlace::decode(pending_place()).expect("a place");
    let cell = cell(b"k", b"", [0x12; 32], [0x34; 32], true);
    let mut bytes = Vec::new();
    let mut run = DeltaRunWriter::new(page(12), Some(5), &mut bytes);
    run.push(&Delta::Insert {
        slot: slot(1),
        cell: decoded_cell(&cell),
    })
    .expect("a delta");
    run.push(&Delta::Value {
        slot: slot(1),
        value: CellValue::of_place(pending),
    })
    .expect("a delta");
    run.finish().expect("a run");
    bytes
}

/// The place of a pending value: the reserved file id, the log address 45,
/// and 29 bytes (`log-format.md` 4.8).
fn pending_place() -> [u8; 16] {
    let mut place = [0; 16];
    place[..4].copy_from_slice(&FileId::RESERVED.to_le_bytes());
    place[4..12].copy_from_slice(&45u64.to_le_bytes());
    place[12..].copy_from_slice(&29u32.to_le_bytes());
    place
}

fn cell(key: &[u8], value: &[u8], value_hash: [u8; 32], kv_hash: [u8; 32], root: bool) -> Vec<u8> {
    CellContent {
        key,
        feature: &[],
        value_hash: &value_hash,
        kv_hash: &kv_hash,
        left: None,
        right: None,
        hosted_roots: &[],
        value: CellValue::Inline(value),
        fragment_root: root,
    }
    .encode()
    .expect("a cell")
}

fn decoded_cell(bytes: &[u8]) -> lignin_format::tree_page::TreeCell<'_> {
    lignin_format::tree_page::TreeCell::decode(bytes, V1).expect("a cell")
}

/// Flat leaf 21 with the prefix `user/`: a value inline, a value out of line
/// and an empty value.
fn flat_leaf() -> Vec<u8> {
    let mut leaf = FlatLeafBuilder::new(page(21), b"user/", 4096).expect("a header");
    leaf.push(b"user/alice", FlatValue::Inline(b"1"))
        .expect("room");
    leaf.push(
        b"user/bob",
        FlatValue::OutOfLine(&pointer(13, 64, 300).encode()),
    )
    .expect("room");
    leaf.push(b"user/carol", FlatValue::Inline(b""))
        .expect("room");
    leaf.finish()
}

/// Flat branch 22 at level 1 with the prefix `user/` and three children.
fn flat_branch() -> Vec<u8> {
    let mut branch =
        FlatBranchBuilder::new(page(22), b"user/", 4096, 1, page(31)).expect("a header");
    branch.push(b"user/b", page(32)).expect("room");
    branch.push(b"user/c", page(33)).expect("room");
    branch.finish()
}

/// A run of leaf 21 with a record of every kind a leaf takes.
fn flat_leaf_run() -> Vec<u8> {
    let leaf = flat_leaf();
    let pending = pending_place();
    let changes = [
        FlatChange::Image(&leaf),
        FlatChange::Insert {
            key: b"user/dave",
            value: FlatValue::Inline(b"4"),
        },
        FlatChange::Insert {
            key: b"user/erin",
            value: FlatValue::OutOfLine(&pending),
        },
        FlatChange::Replace {
            key: b"user/alice",
            value: FlatValue::Inline(b"one"),
        },
        FlatChange::Remove { key: b"user/dave" },
        FlatChange::RemoveRange {
            key: b"user/bob",
            count: 2,
        },
        FlatChange::SetPrefix { prefix: b"user/" },
    ];
    let mut bytes = Vec::new();
    let mut run = FlatRunWriter::new(page(21), &mut bytes);
    for change in &changes {
        run.push(change).expect("a change");
    }
    run.finish().expect("a run");
    bytes
}

/// A run of branch 22 that adds a child.
fn flat_branch_run() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut run = FlatRunWriter::new(page(22), &mut bytes);
    run.push(&FlatChange::InsertChild {
        key: b"user/d",
        child: page(34),
    })
    .expect("a change");
    run.finish().expect("a run");
    bytes
}

/// `val/00000012.val`: the value of page 7's node at offset 20, then two
/// more.
fn value_log() -> Vec<u8> {
    let mut out = FileHeader::new(FileKind::ValueLog, V1, 12)
        .expect("a header")
        .encode()
        .to_vec();
    value_log::write_value(page(7), &long_value(), &mut out).expect("a value");
    value_log::write_value(page(9), b"x", &mut out).expect("a value");
    value_log::write_value(page(21), &[0x5A; 4096], &mut out).expect("a value");
    out
}

/// The blocks of `seg/00000310.seg`, in file order: content, compression
/// asked for, and payload.
fn segment_blocks() -> Vec<(PageId, BlockContent, Compression, Vec<u8>)> {
    vec![
        (page(7), BlockContent::Base, Compression::Lz4, tree_page()),
        (page(21), BlockContent::Base, Compression::Raw, flat_leaf()),
        (page(7), BlockContent::Delta, Compression::Raw, tree_run()),
        (
            page(12),
            BlockContent::Delta,
            Compression::Lz4,
            created_run(),
        ),
    ]
}

/// `seg/00000310.seg`, sealed.
fn segment() -> Vec<u8> {
    let mut out = FileHeader::new(FileKind::Segment, V1, 310)
        .expect("a header")
        .encode()
        .to_vec();
    let mut entries = Vec::new();
    for (page, content, compression, payload) in segment_blocks() {
        let tag = segment::write_block(content, compression, &payload, &mut out).expect("a block");
        entries.push(IndexEntry { page, tag });
    }
    let index_offset = out.len() as u64;
    segment::write_footer(&entries, index_offset, &mut out).expect("a footer");
    out
}

/// Where each block of [`segment`] is: its offset and length.
fn block_pointers(segment: &[u8]) -> Vec<BlockPointer> {
    let index = SegmentIndex::decode_tail(V1, segment, segment.len() as u64).expect("an index");
    index
        .blocks()
        .map(|block| {
            BlockPointer::new(file(310), block.offset, block.tag.block_len() as u32)
                .expect("a block")
        })
        .collect()
}

/// `table/00000122.pt`, which covers version 41: page 7 is a base and a
/// delta in segment 310, page 9 was freed, page 12 was created by a delta's
/// base, and two value-log files are counted.
fn page_table() -> Checkpoint {
    let blocks = block_pointers(&segment());
    let links = vec![
        Link {
            page: page(7),
            version: version(40),
            kind: LinkKind::Base(blocks[0]),
        },
        Link {
            page: page(7),
            version: version(41),
            kind: LinkKind::Delta(blocks[2]),
        },
        Link {
            page: page(9),
            version: version(38),
            kind: LinkKind::Free,
        },
        Link {
            page: page(21),
            version: version(39),
            kind: LinkKind::Base(blocks[1]),
        },
    ];
    let value_counts = vec![
        ValueCount {
            file: file(12),
            values: 3,
            bytes: 4_454,
            miscounted: false,
        },
        ValueCount {
            file: file(13),
            values: 0,
            bytes: 0,
            miscounted: true,
        },
    ];
    let end = table::End {
        covers: version(41),
        reaches: Some(version(41)),
        links: links.len() as u64,
        place: Some(LogPlace::new(file(17), 81_940).expect("a place")),
        value_counts: value_counts.len() as u64,
    };
    Checkpoint {
        version: V1,
        end,
        links,
        value_counts,
    }
}

fn page_table_file() -> Vec<u8> {
    let checkpoint = page_table();
    table::encode(
        file(122),
        checkpoint.end.covers,
        checkpoint.links.iter().copied(),
        checkpoint.end.place,
        checkpoint.value_counts.iter().copied(),
    )
    .expect("a checkpoint")
}

/// `wal/00000017.log`: a group with values and an entry of each kind, a
/// background change of each kind, a checkpoint mark, a group that frees a
/// page and an empty group.
fn wal() -> Vec<u8> {
    let start = LogStart::new(4_096, version(40)).expect("a start");
    let mut out = Vec::new();
    write_file_start(file(17), &start, &mut out).expect("a start");

    let at = start.address(out.len() as u64).expect("an address");
    let mut group = GroupWriter::new(version(41), at, &mut out).expect("a group");
    let pending = group
        .value(page(7), b"a value too long for its cell")
        .expect("a value");
    group
        .carried(&CarriedValue::new(page(9), page(7), pointer(12, 20, 300)))
        .expect("a value");
    group
        .tree_run_with(page(7), None, |run| {
            run.push(&Delta::Hash {
                slot: slot(1),
                field: HashField::Value,
                hash: &[0x55; 32],
            })?;
            run.push(&Delta::Value {
                slot: slot(1),
                value: CellValue::Pending(pending),
            })
        })
        .expect("a run");
    group.freed(page(9)).expect("an entry");
    let flat = flat_branch_run();
    group
        .flat_run(&FlatRun::decode(&flat, V1).expect("a run"))
        .expect("a run");
    group.released(&pointer(12, 20, 300)).expect("an entry");
    group.acquired(&pointer(13, 364, 29)).expect("an entry");
    group.finish();

    let pending = ValuePlace::Pending(pending);
    let placed = pointer(13, 364, 29);
    let in_tree = ValueMove::in_tree(page(7), slot(1), pending, placed).expect("a step");
    wal::Change::Value(in_tree).write(&mut out).expect("a step");
    let moved = pointer(14, 20, 300);
    let in_leaf =
        ValueMove::in_flat_leaf(page(21), ValuePlace::Placed(pointer(13, 64, 300)), moved)
            .expect("a step");
    wal::Change::Value(in_leaf).write(&mut out).expect("a step");
    let mut removes = Vec::new();
    let mut run = DeltaRunWriter::new(page(7), None, &mut removes);
    run.push(&Delta::Remove { slot: slot(3) }).expect("a delta");
    run.finish().expect("a run");
    let removed = Removed::Tree(DeltaRun::decode(&removes, V1).expect("a run"));
    write_reclaim(
        page(7),
        Some(removed),
        Some(version(41)),
        [pointer(13, 64, 300)],
        &mut out,
    )
    .expect("a step");
    write_reclaim(page(30), None, Some(version(41)), [], &mut out).expect("a step");
    CheckpointMark::new(version(41))
        .write(&mut out)
        .expect("a mark");

    let at = start.address(out.len() as u64).expect("an address");
    let mut group = GroupWriter::new(version(42), at, &mut out).expect("a group");
    group.freed(page(12)).expect("an entry");
    group.finish();
    let at = start.address(out.len() as u64).expect("an address");
    GroupWriter::new(version(43), at, &mut out)
        .expect("a group")
        .finish();
    out
}

/// `MANIFEST` of the store the other files belong to.
fn manifest() -> Manifest {
    let last = LastFileIds {
        wal: Some(file(17)),
        segment: Some(file(311)),
        value_log: Some(file(13)),
        page_table: Some(file(122)),
    };
    let table = TableCheckpoint::new(file(122), page_table_file().len() as u64, version(41))
        .expect("a checkpoint");
    let live = |kind, id, state, len| LiveFile::new(kind, file(id), state, len).expect("a file");
    let files = vec![
        live(FileKind::Wal, 17, FileState::Open, wal().len() as u64),
        live(
            FileKind::Segment,
            310,
            FileState::Sealed,
            segment().len() as u64,
        ),
        live(FileKind::Segment, 311, FileState::Open, 20),
        live(
            FileKind::ValueLog,
            12,
            FileState::Closed,
            value_log().len() as u64,
        ),
        live(FileKind::ValueLog, 13, FileState::Open, 20),
    ];
    Manifest::new(SIZE, last, Some(table), files).expect("a manifest")
}

/// Every golden file: its name and what the writers write for it.
fn golden() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("manifest", manifest().encode().expect("a manifest")),
        ("wal", wal()),
        ("segment", segment()),
        ("value_log", value_log()),
        ("page_table", page_table_file()),
        ("tree_page", tree_page()),
        ("tree_run", tree_run()),
        ("created_run", created_run()),
        ("flat_leaf", flat_leaf()),
        ("flat_branch", flat_branch()),
        ("flat_leaf_run", flat_leaf_run()),
        ("flat_branch_run", flat_branch_run()),
    ]
}

fn directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fuzz/golden/v1")
}

fn committed(name: &str) -> Vec<u8> {
    let path = directory().join(name);
    fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn the_writers_write_the_golden_files() {
    let write = env::var_os("LIGNIN_WRITE_GOLDEN").is_some();
    if write {
        fs::create_dir_all(directory()).expect("the golden directory");
    }
    for (name, bytes) in golden() {
        if write {
            fs::write(directory().join(name), &bytes).expect("a golden file");
        } else if name != "segment" {
            assert!(
                committed(name) == bytes,
                "{name} is not what the writers write"
            );
        }
    }
    let mut names: Vec<_> = fs::read_dir(directory())
        .expect("the golden directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .into_string()
                .expect("a name")
        })
        .collect();
    names.sort();
    let mut expected: Vec<_> = golden()
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect();
    expected.sort();
    assert_eq!(names, expected, "a golden file without a writer here");
}

#[test]
fn the_manifest_decodes() {
    assert_eq!(Manifest::decode(&committed("manifest")), Ok(manifest()));
}

#[test]
fn the_page_table_decodes() {
    assert_eq!(
        table::decode(&committed("page_table"), file(122)),
        Ok(page_table())
    );
}

#[test]
fn the_value_log_decodes() {
    let bytes = committed("value_log");
    let values: Vec<_> = value_log::scan(&bytes)
        .expect("a value log")
        .map(|value| value.expect("a value"))
        .map(|(pointer, value)| (pointer, value.owner(), value.to_vec()))
        .collect();
    let expected = [
        (pointer(12, 20, 300), page(7), long_value()),
        (pointer(12, 338, 1), page(9), b"x".to_vec()),
        (pointer(12, 357, 4096), page(21), vec![0x5A; 4096]),
    ];
    assert_eq!(values, expected);
    let value = pointer(12, 20, 300)
        .read(&bytes[20..], page(7), V1)
        .expect("a value");
    assert_eq!(value.to_vec(), long_value());
}

#[test]
fn the_segment_decodes() {
    let bytes = committed("segment");
    assert_eq!(segment::decode_file_header(&bytes, file(310)), Ok(V1));
    let index = SegmentIndex::decode_tail(V1, &bytes, bytes.len() as u64).expect("an index");
    let expected = segment_blocks();
    assert_eq!(index.len(), expected.len());
    let mut compressed = 0;
    for (block, (page, content, _, payload)) in index.blocks().zip(&expected) {
        assert_eq!(block.page, *page);
        let start = block.offset as usize;
        let stored = &bytes[start..start + block.tag.block_len()];
        let read = Block::decode(V1, stored, SIZE.get()).expect("a block");
        assert_eq!(read.tag(), block.tag);
        assert_eq!(read.content(), *content);
        assert_eq!(*read.payload().expect("a payload"), payload[..]);
        compressed += usize::from(read.compression() == Compression::Lz4);
    }
    assert_eq!(
        compressed, 2,
        "both blocks that asked for LZ4 are compressed"
    );
}

#[test]
fn the_tree_page_and_its_runs_decode_and_apply() {
    let image = committed("tree_page");
    assert_eq!(image.len(), 247);
    let page7 = TreePage::decode(&image, V1, SIZE).expect("a page");
    assert_eq!(page7.id(), page(7));
    assert_eq!(page7.cell_count(), 2);
    assert_eq!(page7.check_fragments(), Ok(()));
    let node = page7.cell(slot(1)).expect("a cell");
    assert_eq!(node.value_pointer(), Some(pointer(12, 20, 300)));
    assert_eq!(page7.cell(slot(3)).expect("a cell").key(), b"abc");

    let tree_run = committed("tree_run");
    let run = DeltaRun::decode(&tree_run, V1).expect("a run");
    assert_eq!(run.deltas().len(), 21);
    let mut out = Vec::new();
    consolidate(page(7), Some(&image), [&run], V1, SIZE, &mut out).expect("consolidates");
    let after = TreePage::decode(&out, V1, SIZE).expect("a page");
    assert_eq!(after.departures(), 3);
    assert_eq!(
        after.cell(slot(1)).expect("a cell").value_pointer(),
        Some(pointer(13, 64, 300))
    );
    assert_eq!(
        after.cell(slot(2)).expect("a cell").value().inline(),
        Some(&b"wx"[..])
    );
    assert!(after.cell(slot(3)).is_none());

    let created_run = committed("created_run");
    let run = DeltaRun::decode(&created_run, V1).expect("a run");
    assert_eq!(run.created(), Some(5));
    let mut out = Vec::new();
    consolidate(page(12), None, [&run], V1, SIZE, &mut out).expect("consolidates");
    let created = TreePage::decode(&out, V1, SIZE).expect("a page");
    assert_eq!(created.departures(), 5);
    assert!(created.has_pending_value());
}

#[test]
fn the_flat_pages_and_their_runs_decode_and_apply() {
    let leaf = committed("flat_leaf");
    let read = FlatPage::decode(&leaf, V1)
        .expect("a page")
        .leaf()
        .expect("a leaf");
    assert_eq!(read.id(), page(21));
    let keys: Vec<_> = read.iter().map(|(key, _)| key.to_vec()).collect();
    assert_eq!(keys, [&b"user/alice"[..], b"user/bob", b"user/carol"]);
    assert_eq!(pointer(13, 64, 300).find_in_flat_leaf(&read), Some(1));

    let branch = committed("flat_branch");
    let read = FlatPage::decode(&branch, V1)
        .expect("a page")
        .branch()
        .expect("a branch");
    assert_eq!(read.level(), 1);
    assert_eq!(read.child_for(b"user/a"), Some(page(31)));
    assert_eq!(read.child_for(b"user/bz"), Some(page(32)));
    assert_eq!(read.child_for(b"user/c"), Some(page(33)));

    let run_bytes = committed("flat_leaf_run");
    let run = FlatRun::decode(&run_bytes, V1).expect("a run");
    assert_eq!(run.changes().len(), 7);
    let mut out = Vec::new();
    run.apply(None, V1, 4096, &mut out).expect("applies");
    let after = FlatPage::decode(&out, V1)
        .expect("a page")
        .leaf()
        .expect("a leaf");
    let keys: Vec<_> = after.iter().map(|(key, _)| key.to_vec()).collect();
    assert_eq!(keys, [&b"user/alice"[..], b"user/erin"]);
    assert!(after.has_pending_value());

    let run_bytes = committed("flat_branch_run");
    let run = FlatRun::decode(&run_bytes, V1).expect("a run");
    let mut out = Vec::new();
    run.apply(Some(&branch), V1, 4096, &mut out)
        .expect("applies");
    let after = FlatPage::decode(&out, V1)
        .expect("a page")
        .branch()
        .expect("a branch");
    assert_eq!(after.child_for(b"user/d"), Some(page(34)));
}

#[test]
fn the_log_decodes() {
    let bytes = committed("wal");
    let mut scan = wal::scan(&bytes).expect("a log");
    let items: Vec<_> = scan.by_ref().collect();
    let tail = scan.tail();
    assert_eq!(tail.failure(), None);
    assert_eq!(tail.valid_len(), bytes.len() as u64);
    assert_eq!(tail.latest(), version(43));
    let kinds: Vec<_> = items
        .iter()
        .map(|item| match item {
            LogItem::Commit(group) => format!("commit {}", group.version().get()),
            LogItem::Change(wal::Change::Value(_)) => "value".to_owned(),
            LogItem::Change(wal::Change::Reclaim(_)) => "reclaim".to_owned(),
            LogItem::Mark(mark) => format!("mark {}", mark.covers().get()),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "commit 41",
            "value",
            "value",
            "reclaim",
            "reclaim",
            "mark 41",
            "commit 42",
            "commit 43"
        ]
    );
    let LogItem::Commit(group) = items[0] else {
        unreachable!("the first item is a group");
    };
    assert_eq!(group.values().count(), 2);
    assert_eq!(group.entries().count(), 5);
}
