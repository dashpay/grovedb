use super::{scan::scan_with, *};
use crate::{
    delta::{Delta, DeltaRun, DeltaRunWriter},
    flat_page::{FlatBranchBuilder, FlatLeafBuilder, FlatValue},
    header::write_record,
    ids::{CellAddress, SlotId},
    test_util::SplitMix64,
    tree_page::{CellContent, CellValue, HashField, Side, TreeCell},
    value_log::{PendingValue, ValuePlace, ValuePointer},
};

mod arbitrary;
mod changes;
mod cursor;
mod flat;
mod golden;
mod groups;
mod records;
mod tail;
mod values;

const V1: FormatVersion = FormatVersion::V1;

/// Records of at most 200 bytes of payload, so that values and groups of a
/// few hundred bytes are cut into several records by the code that cuts
/// them at 128 MiB.
const SMALL: Limits = Limits { max_payload: 200 };

const CAPACITY: usize = 4096;

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("a page id")
}

fn file_id(raw: u32) -> FileId {
    FileId::new(raw).expect("a file id")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("a slot")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn address(raw: u64) -> LogAddress {
    LogAddress::new(raw).expect("an address")
}

fn pointer(file: u32, offset: u64, len: u32) -> ValuePointer {
    ValuePointer::new(file_id(file), offset, len).expect("a pointer")
}

fn pending(at: u64, len: u32) -> PendingValue {
    address(at).pending(len).expect("a pending value")
}

fn wal_error(error: WalError) -> FormatError {
    FormatError::Wal(error)
}

/// What a cell holds for a value at `place`.
fn cell_value(place: ValuePlace) -> CellValue<'static> {
    CellValue::of_place(place)
}

/// The bytes of a leaf cell with the key `key` and the value `value`.
fn cell_bytes(key: &[u8], value: CellValue<'_>) -> Vec<u8> {
    CellContent {
        key,
        feature: &[],
        value_hash: &[0xCC; 32],
        kv_hash: &[0xDD; 32],
        left: None,
        right: None,
        hosted_roots: &[],
        value,
        fragment_root: false,
    }
    .encode()
    .expect("a cell")
}

/// The record with the payload `payload`.
fn framed(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_record(payload, &mut out).expect("a record");
    out
}

/// A value record, written field by field.
fn value_record(owner: u32, part: u16, value_len: u32, data: &[u8]) -> Vec<u8> {
    let mut payload = vec![2];
    payload.extend_from_slice(&owner.to_le_bytes());
    payload.extend_from_slice(&part.to_le_bytes());
    payload.extend_from_slice(&value_len.to_le_bytes());
    payload.extend_from_slice(data);
    framed(&payload)
}

/// A part of a group, written field by field around `entries`.
fn part_record(flags: u8, raw_version: u64, number: u32, entries: &[u8]) -> Vec<u8> {
    part_stating(flags, raw_version, number, entries.len() as u32, entries)
}

/// A part of a group that states `entries_len` as the length of its
/// entries, whatever their length is.
fn part_stating(
    flags: u8,
    raw_version: u64,
    number: u32,
    entries_len: u32,
    entries: &[u8],
) -> Vec<u8> {
    let mut payload = vec![4, flags];
    payload.extend_from_slice(&raw_version.to_le_bytes());
    payload.extend_from_slice(&number.to_le_bytes());
    payload.extend_from_slice(&entries_len.to_le_bytes());
    payload.extend_from_slice(entries);
    framed(&payload)
}

fn freed_entry(raw_page: u32) -> Vec<u8> {
    let mut entry = vec![3];
    entry.extend_from_slice(&raw_page.to_le_bytes());
    entry
}

fn pointer_entry(kind: u8, pointer: &ValuePointer) -> Vec<u8> {
    let mut entry = vec![kind];
    entry.extend_from_slice(&pointer.encode());
    entry
}

fn run_entry(kind: u8, run: &[u8]) -> Vec<u8> {
    let mut entry = vec![kind];
    entry.extend_from_slice(&(run.len() as u32).to_le_bytes());
    entry.extend_from_slice(run);
    entry
}

fn decode(bytes: &[u8]) -> Result<(Record<'_>, usize), FormatError> {
    Record::decode(bytes, V1)
}

/// What a scan of a new log file followed by `records` ends with.
fn tail_of(records: &[u8]) -> LogTail {
    tail_after(&LogStart::FIRST, Limits::FORMAT, records)
}

fn tail_after(start: &LogStart, limits: Limits, records: &[u8]) -> LogTail {
    let mut log = new_log(1, start);
    log.extend_from_slice(records);
    items_with(&log, limits).1
}

/// The error a scan of a new log file followed by `records` stops with.
fn refusal(records: &[u8]) -> FormatError {
    tail_of(records).failure().expect("a failure").error
}

/// The bytes of a log file that holds its header and its start record.
fn new_log(file: u32, start: &LogStart) -> Vec<u8> {
    let mut log = Vec::new();
    write_file_start(file_id(file), start, &mut log).expect("a start");
    log
}

/// Appends the group of `raw_version` to `log`, a file with the start
/// record `start`, with what `fill` gives it.
fn push_group(
    log: &mut Vec<u8>,
    start: &LogStart,
    limits: Limits,
    raw_version: u64,
    fill: impl FnOnce(&mut GroupWriter<'_>),
) {
    let at = start.address(log.len() as u64).expect("an address");
    let mut group =
        GroupWriter::with_limits(version(raw_version), at, limits, log).expect("a group");
    fill(&mut group);
    group.finish();
}

fn items(log: &[u8]) -> (Vec<LogItem<'_>>, LogTail) {
    items_with(log, Limits::FORMAT)
}

fn items_with(log: &[u8], limits: Limits) -> (Vec<LogItem<'_>>, LogTail) {
    let mut scan = scan_with(log, limits).expect("a log file");
    let items = scan.by_ref().collect();
    (items, scan.tail())
}

/// Writes `item` again with the writers, as the item at `offset` of a log
/// file that starts with `start`.
fn written_again(item: &LogItem<'_>, start: &LogStart, offset: u64, limits: Limits) -> Vec<u8> {
    let mut out = Vec::new();
    match item {
        LogItem::Commit(group) => {
            let at = start.address(offset).expect("an address");
            assert_eq!(group.address(), at);
            let mut writer =
                GroupWriter::with_limits(group.version(), at, limits, &mut out).expect("a group");
            for (at, value) in group.values() {
                let pending = match value {
                    LoggedValue::Bytes(bytes) => writer.value(bytes.owner(), &bytes.to_vec()),
                    LoggedValue::Carried(carried) => writer.carried(&carried),
                }
                .expect("a value");
                assert_eq!(LogAddress::of(&pending), Ok(at));
                assert_eq!(pending.value_len(), value.value_len());
            }
            for entry in group.entries() {
                match entry {
                    Entry::Tree(run) => writer.tree_run(&run),
                    Entry::Flat(run) => writer.flat_run(&run),
                    Entry::Freed(page) => writer.freed(page),
                    Entry::Released(pointer) => writer.released(&pointer),
                    Entry::Acquired(pointer) => writer.acquired(&pointer),
                }
                .expect("an entry");
            }
            writer.finish();
        }
        LogItem::Change(change) => change.write(&mut out).expect("a change"),
        LogItem::Mark(mark) => mark.write(&mut out).expect("a mark"),
    }
    out
}

/// A log file with random items, and where each of them begins and ends.
struct Built {
    log: Vec<u8>,
    start: LogStart,
    limits: Limits,
    /// The offset at which each item ends, in order. The first item begins
    /// at [`FIRST_ITEM_OFFSET`].
    ends: Vec<u64>,
    /// The offset at which each record ends, in order.
    record_ends: Vec<u64>,
    /// The version of the last group at or before each item, in order.
    versions: Vec<u64>,
}

impl Built {
    /// The offset at which the item begins that holds the byte at `at`.
    fn item_start(&self, at: u64) -> u64 {
        let before = self.ends.partition_point(|end| *end <= at);
        before
            .checked_sub(1)
            .map_or(FIRST_ITEM_OFFSET, |item| self.ends[item])
    }

    /// The number of items that end at or before `at`.
    fn items_before(&self, at: u64) -> usize {
        self.ends.partition_point(|end| *end <= at)
    }

    /// The offset at which the record begins that holds the byte at `at`.
    fn record_start(&self, at: u64) -> u64 {
        let before = self.record_ends.partition_point(|end| *end <= at);
        before
            .checked_sub(1)
            .map_or(FIRST_ITEM_OFFSET, |record| self.record_ends[record])
    }

    /// Checks that a scan of `bytes`, which are the log with something
    /// done to its bytes from `damaged_at` on, gives exactly the items
    /// that end at or before `damaged_at`, each as it was written, and
    /// stops at the end of the last of them.
    fn check_prefix(&self, bytes: &[u8], damaged_at: u64) -> LogTail {
        let mut scan = scan_with(bytes, self.limits).expect("the start is whole");
        let whole = self.items_before(damaged_at);
        let mut at = FIRST_ITEM_OFFSET;
        for &end in &self.ends[..whole] {
            assert_eq!(scan.offset(), at);
            let item = scan.next().expect("a whole item");
            let again = written_again(&item, &self.start, at, self.limits);
            assert_eq!(
                again,
                self.log[at as usize..end as usize],
                "the item at {at}"
            );
            at = end;
        }
        assert_eq!(scan.next(), None, "nothing after offset {at}");
        let tail = scan.tail();
        assert_eq!(tail.valid_len(), at);
        let latest = whole
            .checked_sub(1)
            .map_or(self.start.after(), |item| version(self.versions[item]));
        assert_eq!(tail.latest(), latest);
        tail
    }
}

fn random_pointer(rng: &mut SplitMix64) -> ValuePointer {
    pointer(
        1 + (rng.next_u64() % 4) as u32,
        20 + rng.next_u64() % 100_000,
        1 + (rng.next_u64() % 5000) as u32,
    )
}

fn random_place(rng: &mut SplitMix64, len: u32) -> ValuePlace {
    if rng.next_u64().is_multiple_of(2) {
        ValuePlace::Pending(pending(45 + rng.next_u64() % 100_000, len))
    } else {
        ValuePlace::Placed(pointer(
            1 + (rng.next_u64() % 4) as u32,
            20 + rng.next_u64() % 100_000,
            len,
        ))
    }
}

fn sorted_pointers(rng: &mut SplitMix64, most: u64) -> Vec<ValuePointer> {
    let mut pointers: Vec<_> = (0..rng.next_u64() % (most + 1))
        .map(|_| random_pointer(rng))
        .collect();
    pointers.sort_by_key(|pointer| (pointer.file(), pointer.offset()));
    pointers.dedup_by_key(|pointer| (pointer.file(), pointer.offset()));
    pointers
}

/// The longest run an entry of a group holds under `limits`.
fn max_run_len(limits: Limits) -> usize {
    limits.max_entries() - 5
}

/// The bytes of a run of the tree page `page` with a few random records,
/// at most `max` bytes long.
fn random_tree_run(rng: &mut SplitMix64, page: PageId, removes_only: bool, max: usize) -> Vec<u8> {
    loop {
        let run = any_tree_run(rng, page, removes_only);
        if run.len() <= max {
            return run;
        }
    }
}

fn any_tree_run(rng: &mut SplitMix64, page: PageId, removes_only: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let created = (!removes_only && rng.next_u64().is_multiple_of(5)).then(|| rng.next_u64());
    let mut run = DeltaRunWriter::new(page, created, &mut bytes);
    for _ in 0..1 + rng.next_u64() % 3 {
        let slot = slot(1 + (rng.next_u64() % 40) as u16);
        let hash = [rng.next_u64() as u8; 32];
        let key_len = 1 + (rng.next_u64() % 6) as usize;
        let cell = cell_bytes(&rng.bytes(key_len), cell_value(random_place(rng, 300)));
        let small_len = (rng.next_u64() % 12) as usize;
        let small = rng.bytes(small_len);
        let delta = match if removes_only { 0 } else { rng.next_u64() % 7 } {
            0 => Delta::Remove { slot },
            1 => Delta::Hash {
                slot,
                field: HashField::ALL[(rng.next_u64() % 4) as usize],
                hash: &hash,
            },
            2 => Delta::Child {
                slot,
                side: Side::BOTH[(rng.next_u64() % 2) as usize],
                child: CellAddress::new(page, slot),
            },
            3 => Delta::FragmentRoot {
                slot,
                fragment_root: rng.next_u64().is_multiple_of(2),
            },
            4 => Delta::Value {
                slot,
                value: CellValue::Inline(&small),
            },
            5 => Delta::Value {
                slot,
                value: CellValue::OutOfLine(random_pointer(rng)),
            },
            _ => Delta::Insert {
                slot,
                cell: TreeCell::decode(&cell, V1).expect("a cell"),
            },
        };
        run.push(&delta).expect("a record");
    }
    run.finish().expect("a run");
    bytes
}

/// The bytes of a run of the flat page `page` with a few random records,
/// at most `max` bytes long.
fn random_flat_run(rng: &mut SplitMix64, page: PageId, removes_only: bool, max: usize) -> Vec<u8> {
    loop {
        let run = any_flat_run(rng, page, removes_only);
        if run.len() <= max {
            return run;
        }
    }
}

fn any_flat_run(rng: &mut SplitMix64, page: PageId, removes_only: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut run = FlatRunWriter::new(page, &mut bytes);
    if !removes_only && rng.next_u64().is_multiple_of(4) {
        let image = if rng.next_u64().is_multiple_of(2) {
            let mut leaf = FlatLeafBuilder::new(page, b"k", CAPACITY).expect("a leaf");
            leaf.push(b"k1", FlatValue::Inline(b"one")).expect("fits");
            leaf.finish()
        } else {
            let mut branch =
                FlatBranchBuilder::new(page, b"", CAPACITY, 1, page).expect("a branch");
            branch.push(b"m", page).expect("fits");
            branch.finish()
        };
        run.push(&FlatChange::Image(&image)).expect("an image");
    }
    for _ in 0..1 + rng.next_u64() % 3 {
        let key_len = (rng.next_u64() % 9) as usize;
        let key = rng.bytes(key_len);
        let inline_len = (rng.next_u64() % 12) as usize;
        let inline = rng.bytes(inline_len);
        let place = random_place(rng, 77).encode();
        let value = if rng.next_u64().is_multiple_of(3) {
            FlatValue::OutOfLine(&place)
        } else {
            FlatValue::Inline(&inline)
        };
        let change = match if removes_only {
            rng.next_u64() % 2
        } else {
            rng.next_u64() % 6
        } {
            0 => FlatChange::Remove { key: &key },
            1 => FlatChange::RemoveRange {
                key: &key,
                count: 2 + (rng.next_u64() % 9) as u16,
            },
            2 => FlatChange::Insert { key: &key, value },
            3 => FlatChange::Replace { key: &key, value },
            4 => FlatChange::InsertChild {
                key: &key,
                child: page,
            },
            _ => FlatChange::SetPrefix { prefix: &key },
        };
        run.push(&change).expect("a record");
    }
    run.finish().expect("a run");
    bytes
}

fn random_group(rng: &mut SplitMix64, limits: Limits, group: &mut GroupWriter<'_>) {
    let max = max_run_len(limits);
    for _ in 0..rng.next_u64() % 4 {
        let owner = page(1 + (rng.next_u64() % 50) as u32);
        if rng.next_u64().is_multiple_of(4) {
            let carried = CarriedValue::new(owner, page(77), random_pointer(rng));
            group.carried(&carried).expect("a carried value");
        } else {
            let len = 1 + (rng.next_u64() % 500) as usize;
            group.value(owner, &rng.bytes(len)).expect("a value");
        }
    }
    let mut next = 0;
    for _ in 0..rng.next_u64() % 9 {
        next += 1 + (rng.next_u64() % 5) as u32;
        match rng.next_u64() % 6 {
            0 => group.freed(page(next)),
            1 | 2 => {
                let run = random_flat_run(rng, page(next), false, max);
                group.flat_run(&FlatRun::decode(&run, V1).expect("a run"))
            }
            3 => group.tree_run_with(page(next), None, |run| {
                run.push(&Delta::Remove { slot: slot(3) })
            }),
            _ => {
                let run = random_tree_run(rng, page(next), false, max);
                group.tree_run(&DeltaRun::decode(&run, V1).expect("a run"))
            }
        }
        .expect("a page");
    }
    for pointer in sorted_pointers(rng, 3) {
        group.released(&pointer).expect("a pointer");
    }
    for pointer in sorted_pointers(rng, 2) {
        group.acquired(&pointer).expect("a pointer");
    }
}

fn random_change(rng: &mut SplitMix64, latest: u64, out: &mut Vec<u8>) {
    let page = page(1 + (rng.next_u64() % 50) as u32);
    match rng.next_u64() % 4 {
        0 => {
            let to = random_pointer(rng);
            let from = random_place(rng, to.value_len());
            ValueMove::in_tree(page, slot(1 + (rng.next_u64() % 9) as u16), from, to)
                .expect("a move")
                .write(out)
        }
        1 => {
            let to = random_pointer(rng);
            let from = random_place(rng, to.value_len());
            ValueMove::in_flat_leaf(page, from, to)
                .expect("a move")
                .write(out)
        }
        _ => {
            let tree = random_tree_run(rng, page, true, usize::MAX);
            let flat = random_flat_run(rng, page, true, usize::MAX);
            let removed = match rng.next_u64() % 3 {
                0 => None,
                1 => Some(Removed::Tree(DeltaRun::decode(&tree, V1).expect("a run"))),
                _ => Some(Removed::Flat(FlatRun::decode(&flat, V1).expect("a run"))),
            };
            let free = removed.is_none() || (latest > 0 && rng.next_u64().is_multiple_of(2));
            let freed = (free && latest > 0).then(|| version(1 + rng.next_u64() % latest));
            if removed.is_none() && freed.is_none() {
                return CheckpointMark::new(CommitVersion::EMPTY)
                    .write(out)
                    .expect("a mark");
            }
            let released = sorted_pointers(rng, 3);
            write_reclaim(page, removed, freed, released, out)
        }
    }
    .expect("a change");
}

/// A log file with `count` random items: commit groups with their values,
/// runs and pointers, background changes and checkpoint marks.
fn random_log(seed: u64, limits: Limits, count: usize) -> Built {
    let mut rng = SplitMix64::new(seed);
    let after = rng.next_u64() % 3;
    let start = LogStart::new(rng.next_u64() % 1_000_000, version(after)).expect("a start");
    let mut log = new_log(1 + (rng.next_u64() % 9) as u32, &start);
    let mut latest = after;
    let (mut ends, mut versions) = (Vec::new(), Vec::new());
    for _ in 0..count {
        match rng.next_u64() % 5 {
            0 => random_change(&mut rng, latest, &mut log),
            1 => CheckpointMark::new(version(rng.next_u64() % (latest + 1)))
                .write(&mut log)
                .expect("a mark"),
            _ => {
                latest += 1;
                push_group(&mut log, &start, limits, latest, |group| {
                    random_group(&mut rng, limits, group)
                });
            }
        }
        ends.push(log.len() as u64);
        versions.push(latest);
    }
    let mut record_ends = Vec::new();
    let mut at = FIRST_ITEM_OFFSET as usize;
    while at < log.len() {
        at += RecordHeader::ENCODED_LEN
            + u32::from_le_bytes(*log[at..].first_chunk().expect("a frame")) as usize;
        record_ends.push(at as u64);
    }
    assert_eq!(at, log.len());
    Built {
        log,
        start,
        limits,
        ends,
        record_ends,
        versions,
    }
}
