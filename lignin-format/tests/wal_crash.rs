//! The log under the crash model of the file layer: whatever a crash leaves
//! of a log file, a scan gives the longest prefix of whole items.
//!
//! The file layer's contract is in `lignin_io::vfs`: storage writes a sector
//! whole or not at all, bytes that were not synced can be lost or read as
//! zeros under a length that survived, and writes can reach the disk out of
//! order. `lignin_io::sim` is that contract as code. The log is appended to
//! a simulated file, one append for each item, with a sync now and then,
//! and after every append each state a crash can leave is read back.

use std::{io, path::Path};

use lignin_format::{
    delta::Delta,
    flat_page::FlatValue,
    tree_page::HashField,
    value_log::{ValuePlace, ValuePointer},
    wal::{
        self, CarriedValue, CheckpointMark, FlatChange, GroupWriter, LogStart, ValueMove,
        FIRST_ITEM_OFFSET,
    },
    CommitVersion, FileId, PageId, SlotId,
};
use lignin_io::{
    sim::{CrashImage, CrashPlan, SimConfig, SimVfs},
    Create, OpenMode, Vfs,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn bytes(&mut self, len: u64) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }

    /// Up to `most` bytes, and at least `least`.
    fn some_bytes(&mut self, least: u64, most: u64) -> Vec<u8> {
        let len = least + self.below(most - least + 1);
        self.bytes(len)
    }
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("a page id")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn pointer(file: u32, offset: u64, len: u32) -> ValuePointer {
    ValuePointer::new(FileId::new(file).expect("a file id"), offset, len).expect("a pointer")
}

/// A log file as it was written: its bytes, and where its items end.
struct Written {
    start: LogStart,
    bytes: Vec<u8>,
    ends: Vec<u64>,
    latest: u64,
}

impl Written {
    fn new(file: u32, start: LogStart) -> Self {
        let mut bytes = Vec::new();
        wal::write_file_start(FileId::new(file).expect("a file id"), &start, &mut bytes)
            .expect("a start");
        Self {
            start,
            bytes,
            ends: Vec::new(),
            latest: start.after().get(),
        }
    }

    /// The bytes of one more item: a commit group, a background change or a
    /// checkpoint mark.
    fn item(&mut self, rng: &mut Rng) -> Vec<u8> {
        let mut out = Vec::new();
        match rng.below(6) {
            0 => CheckpointMark::new(version(rng.below(self.latest + 1)))
                .write(&mut out)
                .expect("a mark"),
            1 => {
                let to = pointer(3, 20 + rng.below(9000), 77);
                let from = wal::LogAddress::new(45 + rng.below(9000))
                    .expect("an address")
                    .pending(77)
                    .expect("a pending value");
                ValueMove::in_tree(page(7), SlotId::MIN, ValuePlace::Pending(from), to)
                    .expect("a move")
                    .write(&mut out)
                    .expect("a change")
            }
            _ => {
                self.latest += 1;
                let at = self
                    .start
                    .address(self.bytes.len() as u64)
                    .expect("an address");
                let mut group =
                    GroupWriter::new(version(self.latest), at, &mut out).expect("a group");
                for _ in 0..rng.below(3) {
                    if rng.below(4) == 0 {
                        let carried = CarriedValue::new(page(9), page(8), pointer(2, 64, 300));
                        group.carried(&carried).expect("a carried value");
                    } else {
                        // Values of zeros as well: a sector of them that is
                        // lost reads as it was written.
                        let len = 1 + rng.below(1500);
                        let value = if rng.below(5) == 0 {
                            vec![0; len as usize]
                        } else {
                            rng.bytes(len)
                        };
                        group.value(page(9), &value).expect("a value");
                    }
                }
                let mut next = 0;
                for _ in 0..rng.below(5) {
                    next += 1 + rng.below(4) as u32;
                    let hash = [rng.next() as u8; 32];
                    let key = rng.some_bytes(1, 20);
                    let value = rng.some_bytes(0, 39);
                    match rng.below(3) {
                        0 => group.freed(page(next)),
                        1 => group.flat_run_with(page(next), |run| {
                            run.push(&FlatChange::Insert {
                                key: &key,
                                value: FlatValue::Inline(&value),
                            })
                        }),
                        _ => group.tree_run_with(page(next), None, |run| {
                            run.push(&Delta::Hash {
                                slot: SlotId::MIN,
                                field: HashField::KeyValue,
                                hash: &hash,
                            })
                        }),
                    }
                    .expect("a page");
                }
                group.finish();
            }
        }
        self.bytes.extend_from_slice(&out);
        self.ends.push(self.bytes.len() as u64);
        out
    }

    /// The end of the last item that lies wholly within the first `len`
    /// bytes, and the number of items up to there.
    fn whole_within(&self, len: u64) -> (usize, u64) {
        let count = self.ends.partition_point(|end| *end <= len);
        let end = count
            .checked_sub(1)
            .map_or(FIRST_ITEM_OFFSET, |item| self.ends[item]);
        (count, end)
    }

    /// Reads `recovered`, what a crash left of the file, and checks that
    /// the scan gives exactly the items of the longest prefix of it that is
    /// as it was written. Returns the tail and whether all of the file is
    /// there.
    fn check(&self, recovered: &[u8]) -> (wal::LogTail, bool) {
        assert!(recovered.len() <= self.bytes.len());
        let intact = recovered
            .iter()
            .zip(&self.bytes)
            .position(|(left, right)| left != right)
            .unwrap_or(recovered.len()) as u64;
        let (count, end) = self.whole_within(intact);

        let mut scan = wal::scan(recovered).expect("the start of the file was synced");
        let mut at = FIRST_ITEM_OFFSET;
        for &item_end in &self.ends[..count] {
            assert_eq!(scan.offset(), at);
            assert!(scan.next().is_some(), "the item at {at} is whole");
            at = item_end;
        }
        assert!(scan.next().is_none(), "nothing behind offset {at}");
        let tail = scan.tail();
        assert_eq!(tail.valid_len(), end);
        assert_eq!(tail.failure().is_none(), end == recovered.len() as u64);
        (tail, intact == self.bytes.len() as u64)
    }
}

fn read(fs: &SimVfs, path: &Path) -> Option<Vec<u8>> {
    let file = match fs.open(path, OpenMode::Read) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => panic!("{error}"),
    };
    let mut bytes = vec![0; file.len().expect("a length") as usize];
    file.read_exact_at(&mut bytes, 0).expect("the bytes");
    Some(bytes)
}

/// The states a crash can leave: what a device that writes in order
/// leaves, with each tear of the first lost write; each single lost write;
/// each write applied late; every state there is while there are few; and
/// drawn ones.
fn plans(image: &CrashImage, draws: u64) -> Vec<CrashPlan> {
    let mut plans: Vec<CrashPlan> = image
        .prefixes()
        .chain(image.holes())
        .chain(image.reorders())
        .collect();
    plans.extend(image.every_plan(600).unwrap_or_default());
    plans.extend((0..draws).map(|draw| image.sample(draw)));
    plans
}

#[derive(Debug, Default)]
struct Seen {
    states: u64,
    lost: u64,
    survivors_behind_a_loss: u64,
    cut_inside_an_item: u64,
}

#[test]
fn whatever_a_crash_leaves_of_a_log_file_reads_as_its_longest_prefix_of_whole_items() {
    let path = Path::new("/log");
    let mut seen = Seen::default();
    for (seed, sector_size) in [(1, 512), (2, 64), (3, 4096), (4, 1), (5, 64)] {
        let mut rng = Rng(seed);
        let fs = SimVfs::new(SimConfig {
            sector_size,
            ..SimConfig::new(seed)
        });
        let file = fs
            .open(path, OpenMode::Append(Create::New))
            .expect("a file");
        fs.sync_dir(Path::new("/")).expect("a name");
        let mut written = Written::new(7, LogStart::new(seed * 1000, version(seed)).unwrap());
        file.append(&written.bytes).expect("an append");
        file.sync_data().expect("a sync");
        let mut synced = written.bytes.len() as u64;

        for _ in 0..40 {
            let at = written.bytes.len() as u64;
            let item = written.item(&mut rng);
            assert_eq!(file.append(&item).expect("an append"), at);
            if rng.below(4) == 0 {
                file.sync_data().expect("a sync");
                synced = written.bytes.len() as u64;
            }

            let image = fs.crash_image();
            for plan in plans(&image, 40) {
                let after = image.recover(&plan).expect("a state the contract allows");
                let recovered = read(&after, path).expect("the name was synced");
                // What was synced is there, so every item that was synced
                // is read.
                assert_eq!(
                    recovered[..synced as usize],
                    written.bytes[..synced as usize]
                );
                let (tail, whole) = written.check(&recovered);
                assert!(tail.valid_len() >= written.whole_within(synced).1);

                seen.states += 1;
                seen.lost += u64::from(!whole);
                let valid = tail.valid_len() as usize;
                seen.cut_inside_an_item += u64::from(recovered.len() > valid);
                seen.survivors_behind_a_loss += u64::from(
                    written
                        .ends
                        .windows(2)
                        .filter(|ends| ends[0] as usize >= valid)
                        .any(|ends| {
                            let item = ends[0] as usize..ends[1] as usize;
                            recovered.get(item.clone()) == Some(&written.bytes[item])
                        }),
                );
            }
        }
    }
    println!("{seen:?}");
    assert!(seen.states > 20_000, "{seen:?}");
    assert!(
        seen.lost > 10_000 && seen.cut_inside_an_item > 5_000,
        "{seen:?}"
    );
    assert!(seen.survivors_behind_a_loss > 1_000, "{seen:?}");
}

#[test]
fn a_crash_that_loses_the_end_of_one_log_file_ends_the_log_there() {
    let dir = Path::new("/wal");
    let (first_path, second_path) = (
        Path::new("/wal/00000001.log"),
        Path::new("/wal/00000002.log"),
    );
    let (mut states, mut both, mut ended_in_first, mut second_missing) = (0, 0, 0, 0);
    for seed in 1..=12 {
        let mut rng = Rng(0xF05 + seed);
        let fs = SimVfs::new(SimConfig {
            sector_size: 64,
            ..SimConfig::new(seed)
        });
        fs.create_dir(dir).expect("a directory");
        fs.sync_dir(Path::new("/")).expect("a name");

        // The first file: its start is durable, and a few items are not.
        let first_file = fs
            .open(first_path, OpenMode::Append(Create::New))
            .expect("a file");
        fs.sync_dir(dir).expect("a name");
        let mut first = Written::new(1, LogStart::FIRST);
        first_file.append(&first.bytes).expect("an append");
        first_file.sync_data().expect("a sync");
        for _ in 0..1 + rng.below(4) {
            let item = first.item(&mut rng);
            first_file.append(&item).expect("an append");
        }
        if rng.below(3) == 0 {
            first_file.sync_data().expect("a sync");
        }

        // The log goes on in a second file, which says where it begins.
        let next = first
            .start
            .next(first.bytes.len() as u64, version(first.latest))
            .expect("a start");
        let second_file = fs
            .open(second_path, OpenMode::Append(Create::New))
            .expect("a file");
        let mut second = Written::new(2, next);
        second_file.append(&second.bytes).expect("an append");
        if rng.below(2) == 0 {
            second_file.sync_data().expect("a sync");
            fs.sync_dir(dir).expect("a name");
        }
        for _ in 0..1 + rng.below(4) {
            let item = second.item(&mut rng);
            second_file.append(&item).expect("an append");
        }

        let image = fs.crash_image();
        for plan in plans(&image, 300) {
            let after = image.recover(&plan).expect("a state the contract allows");
            states += 1;
            let recovered = read(&after, first_path).expect("the name was synced");
            let (tail, whole) = first.check(&recovered);

            // The second file continues the log only behind all of the
            // first: if the first lost anything, what the second says of
            // its own beginning does not fit.
            let follows = |start: &LogStart| {
                first
                    .start
                    .check_next(tail.valid_len(), tail.latest(), start)
                    .is_ok()
            };
            assert_eq!(follows(&next), whole);
            let Some(recovered) = read(&after, second_path) else {
                second_missing += 1;
                continue;
            };
            let Ok(scan) = wal::scan(&recovered) else {
                // Its start record did not survive: it holds nothing.
                second_missing += 1;
                continue;
            };
            assert_eq!(scan.start(), next);
            if follows(&scan.start()) {
                both += 1;
                second.check(&recovered);
            } else {
                ended_in_first += 1;
            }
        }
    }
    println!(
        "{states} states: {both} with both files, {ended_in_first} ended in the first, \
         {second_missing} without the second"
    );
    assert!(states > 3_000 && both > 300 && ended_in_first > 300 && second_missing > 100);
}
