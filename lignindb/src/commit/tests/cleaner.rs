//! The segment cleaner (`lignin_store::cleaner`, STORE-08) under the
//! commit path.
//!
//! The cleaner's own tests commit pages by hand. Here the pages are those
//! that the engine's commits make of the reference workload's transitions:
//! a document of 400 bytes in one Merk, a reference to it in another, and
//! the root Merk's elements above both. Write-back writes them, and the
//! cleaner runs with the owners of values that a store has, which are the
//! cells of tree pages and the entries of flat leaves.
//!
//! The stores of these tests are on files in memory, which the drop of a
//! store does not check, so each test runs the physical invariant checker
//! (`crate::verify`) itself: at rest once its cleaner has finished
//! ([`check_whole`]), and as a reader of a pinned version while the
//! cleaner works.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use lignin_io::{
    sim::{SimConfig, SimVfs},
    Vfs,
};
use lignin_store::{
    cleaner::{Cleaner, CleanerConfig, CleanerState},
    flush::FlushConfig,
    load::LoadBuffers,
    wal::{LogOptions, SyncPolicy},
};
use lignin_tree::value::{read_value, ValueBuf};

use super::{
    acceptance::{document_key, grove_batch, reference_key, DOCUMENTS, ROOT},
    aux, counted, pointers_held, DIR,
};
use crate::{
    snapshot::{Operation, Versions},
    txn::{
        tests::{
            model, put,
            store::{mix, Db, Outcome, SIZE},
            Tx, World,
        },
        Column,
    },
};

const TRANSITIONS: u64 = 20;
const DOCUMENT_LEN: usize = 400;
const NOTE_LEN: usize = 3000;
/// The flat records with a value out of line that the blocks write again
/// and again.
const NOTES: u64 = 7;

/// An empty store in memory, with no record kept of what is done to its
/// files: these tests make many operations.
fn store(seed: u64, frames: usize, flush: FlushConfig) -> (Arc<Db>, Versions) {
    let fs = SimVfs::new(SimConfig::new(seed));
    fs.create_dir(Path::new(DIR)).expect("a directory");
    fs.sync_dir(Path::new("/")).expect("its name is durable");
    let options = LogOptions {
        target_file_len: 48 << 10,
        sync: SyncPolicy::Never,
    };
    let vfs: Arc<dyn Vfs> = Arc::new(fs);
    Db::on(vfs, Path::new(DIR), frames, options, flush, None)
}

/// `len` bytes that say what they are: the `revision`th writing of the
/// thing `serial`. Whoever reads them can tell that they are whole.
fn told(serial: u64, revision: u64, len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len + 8);
    bytes.extend(serial.to_be_bytes());
    bytes.extend(revision.to_be_bytes());
    let mut state = mix(serial, &revision.to_be_bytes());
    while bytes.len() < len {
        state = mix(state, &[1]);
        bytes.extend(state.to_le_bytes());
    }
    bytes.truncate(len);
    bytes
}

/// The serial and the revision that `bytes` say, checked to be the whole
/// of what [`told`] made.
fn tells(bytes: &[u8]) -> (u64, u64) {
    let word = |at: usize| u64::from_be_bytes(bytes[at..at + 8].try_into().expect("8 bytes"));
    let (serial, revision) = (word(0), word(8));
    assert!(
        bytes == told(serial, revision, bytes.len()),
        "the value of {serial} at its revision {revision} is not whole"
    );
    (serial, revision)
}

fn weight(serial: u64, revision: u64) -> u64 {
    mix(serial.wrapping_mul(0x9E37_79B9), &revision.to_le_bytes())
}

/// What one committed version says, in a few numbers: how many documents
/// it has, and which revision of each document and of each note.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Said {
    documents: u64,
    revisions: u64,
    notes: u64,
}

/// The documents and the notes that the blocks have committed.
#[derive(Default)]
struct Grove {
    /// The revision of each document, by its serial.
    documents: BTreeMap<u64, u64>,
    /// The revision of each note that was written.
    notes: BTreeMap<u64, u64>,
    next: u64,
}

impl Grove {
    fn said(&self) -> Said {
        let sum = |of: &BTreeMap<u64, u64>| {
            of.iter().fold(0u64, |sum, (serial, revision)| {
                sum.wrapping_add(weight(*serial, *revision))
            })
        };
        Said {
            documents: self.documents.len() as u64,
            revisions: sum(&self.documents),
            notes: sum(&self.notes),
        }
    }

    /// Commits one block: 20 transitions, each a new document and a
    /// reference to it, as one operation of one transaction. With `churn`
    /// the block also writes two documents of earlier blocks again,
    /// deletes one with its reference, and writes a note again, so that
    /// values die in the value log, in tree pages and in flat leaves.
    /// Returns the version of the commit.
    fn block(&mut self, world: &mut World<'_>, round: u64, churn: bool) -> u64 {
        let tx = self.write(world, round, churn);
        Self::commit(world, tx)
    }

    /// The transaction of a block, with its writes made and not committed.
    fn write<'s>(&mut self, world: &World<'s>, round: u64, churn: bool) -> Tx<'s> {
        let tx = world.begin();
        let new: Vec<u64> = (self.next..self.next + TRANSITIONS).collect();
        self.next += TRANSITIONS;
        let earlier = |back: u64| round.checked_sub(back).map(|round| round * TRANSITIONS + 3);
        let (mut rewritten, mut gone) = (Vec::new(), None);
        if churn {
            rewritten.extend([earlier(2), earlier(5)].into_iter().flatten());
            rewritten.retain(|serial| self.documents.contains_key(serial));
            gone = earlier(3).map(|serial| serial + 4);
        }
        for serial in &new {
            self.documents.insert(*serial, 0);
        }
        for serial in &rewritten {
            *self.documents.get_mut(serial).expect("a document") += 1;
        }
        if let Some(serial) = gone {
            self.documents.remove(&serial);
        }
        let documents = &self.documents;
        let batch = grove_batch(
            world,
            &tx,
            |items| {
                for serial in new.iter().chain(&rewritten) {
                    let value = told(*serial, documents[serial], DOCUMENT_LEN);
                    items.insert(document_key(*serial), value);
                }
                if let Some(serial) = gone {
                    items.remove(&document_key(serial));
                }
            },
            |items| {
                for serial in &new {
                    items.insert(reference_key(*serial), document_key(*serial));
                }
                if let Some(serial) = gone {
                    items.remove(&reference_key(serial));
                }
            },
        );
        world.apply(&tx, &batch).expect("the block is applied");
        if churn {
            let note = round % NOTES;
            let revision = self
                .notes
                .entry(note)
                .and_modify(|at| *at += 1)
                .or_insert(0);
            let value = told(note, *revision, NOTE_LEN);
            world.write(&tx, vec![put(aux(ROOT), &[b'n', note as u8], &value)]);
        }
        tx
    }

    fn commit<'s>(world: &mut World<'s>, tx: Tx<'s>) -> u64 {
        match world.commit(tx, &[]) {
            Outcome::Committed(version) => version.get(),
            other => panic!("the block is committed, and not {other:?}"),
        }
    }
}

/// What `operation` reads of the grove: every document, with its value
/// from wherever it is, and every note.
fn read(db: &Db, operation: &Operation<'_>) -> Said {
    let mut said = Said::default();
    let open = |name| {
        let opened = operation.open(model::subtree(name));
        opened.expect("the directory is read").expect("a Merk")
    };
    let documents = open(DOCUMENTS);
    let reader = operation.nodes();
    let (mut buffers, mut buf) = (LoadBuffers::new(), ValueBuf::new());
    let mut cursor = lignin_tree::cursor::Cursor::open(&reader, documents.root());
    let mut node = cursor.seek_to_first(&reader, &mut buffers).expect("a seek");
    while let Some(pinned) = node {
        let value = read_value(&db.values, &pinned, &mut buf).expect("a document's value");
        assert_eq!(value.len(), DOCUMENT_LEN);
        let (serial, revision) = tells(value);
        assert_eq!(pinned.node().key(), document_key(serial));
        said.documents += 1;
        said.revisions = said.revisions.wrapping_add(weight(serial, revision));
        node = cursor.next(&reader, &mut buffers).expect("a step");
    }
    let root = open(ROOT);
    let mut value = Vec::new();
    for note in 0..NOTES {
        value.clear();
        let key = [b'n', note as u8];
        if root.get(Column::Aux, &key, &mut value).expect("a read") {
            assert_eq!(value.len(), NOTE_LEN);
            let (serial, revision) = tells(&value);
            assert_eq!(serial, note);
            said.notes = said.notes.wrapping_add(weight(serial, revision));
        }
    }
    said
}

/// Checks what the blocks left: everybody reads what the model has, the
/// counts of live values are those of the pointers that the pages hold
/// (`transactions.md` PV6), nothing private is left, and the physical
/// invariant checker finds the store sound at rest, the page table with
/// the blocks that the cleaner moved among the rest. No pass of
/// write-back or of the cleaner runs meanwhile.
fn check_whole(world: &World<'_>, db: &Db, versions: &Versions) {
    assert!(db.flush_errors.lock().expect("the errors").is_empty());
    world.check_committed();
    let held = pointers_held(db, versions);
    assert!(!held.is_empty(), "no value reached the value log");
    assert_eq!(counted(db), held);
    assert_eq!(db.pool.census().private, 0);
    assert_eq!(db.private_ids(), 0);
    let report = db.verify_still();
    assert!(
        db.unsound(&report).is_empty() && report.more_findings == 0,
        "the store is not sound at rest:\n{report}"
    );
}

/// The most dead space, in a hundred, that a run found after a block,
/// among the bytes of all blocks and among those of all values, from the
/// block at which the store held `from` bytes of blocks; the blocks that
/// were measured; and the bytes of blocks at the end.
#[derive(Debug, Default)]
struct Measured {
    most_dead: f64,
    most_dead_values: f64,
    blocks: u64,
    block_bytes: u64,
}

/// Commits `blocks` blocks through the commit path, each followed by one
/// turn of write-back and, if `cleaned`, one pass of the cleaner over the
/// segments and one over the value log, as their loops give them.
fn run(blocks: u64, from: u64, cleaned: bool) -> Measured {
    let flush = FlushConfig {
        checkpoint_log_bytes: 256 << 10,
        ..FlushConfig::new(SIZE)
    };
    let (db, versions) = store(11, 96, flush);
    let mut world = World::new(&db, &versions);
    let write_back = db.write_back(&versions);
    let state = CleanerState::new(CleanerConfig::new());
    let cleaner = Cleaner::new(write_back, &state).with_stats(&db.stats);
    let mut grove = Grove::default();
    let mut measured = Measured::default();
    for round in 0..blocks {
        grove.block(&mut world, round, false);
        write_back.tick().expect("a turn of write-back");
        if cleaned {
            cleaner.pass().expect("a pass over the segments");
            (db.clean_values(&versions, &cleaner)).expect("a pass over the value log");
        }
        let space = cleaner.space().expect("the space");
        let values = cleaner.value_space().expect("the value log");
        if space.block_bytes() >= from {
            measured.most_dead = measured.most_dead.max(space.dead_percent());
            measured.most_dead_values = measured.most_dead_values.max(values.dead_percent());
            measured.blocks += 1;
        }
        measured.block_bytes = space.block_bytes();
        if (round + 1) % 50 == 0 || round + 1 == blocks {
            println!(
                "block {}: {} KiB of blocks in {} segments, {:.1}% dead; {} KiB of values in \
                 {} files, {:.1}% dead; the cleaner wrote {} KiB",
                round + 1,
                space.block_bytes() >> 10,
                space.segments.len(),
                space.dead_percent(),
                values.bytes >> 10,
                values.files,
                values.dead_percent(),
                db.stats.cleaner_bytes() >> 10,
            );
        }
    }
    let did = state.stats();
    println!("{measured:?}\n{did:?}");
    assert_eq!(did.errors, 0, "{did:?}");
    assert_eq!(
        db.stats.cleaner_bytes(),
        did.bytes_moved + did.bytes_lost + did.value_bytes_copied
    );
    if cleaned {
        assert!(did.blocks_moved > 0 && did.segments_dropped > 0, "{did:?}");
        // The value log is held to its bound by cleaning as well: values
        // die when a page is split and its cells are carried.
        assert!(
            did.values_moved > 0 && did.value_files_dropped > 0,
            "{did:?}"
        );
    }
    cleaner.finish().expect("the cleaner's files are closed");
    check_whole(&world, &db, &versions);
    let operation_said = {
        let snapshot = db.reads(&versions).snapshot();
        let operation = snapshot.operation();
        read(&db, &operation)
    };
    assert_eq!(operation_said, grove.said());
    measured
}

/// The bytes of blocks from which a store is held to the bound, as in the
/// cleaner's own acceptance test.
const FROM: u64 = 512 << 10;

/// **"Dead space stays at or below 20% under the reference workload with
/// no open snapshot"**, with the engine's commits: blocks of 20
/// transitions, a turn of write-back and of the cleaner after each, and
/// no snapshot. After every block, from the one at which the store holds
/// 512 KiB of blocks, at most 20 bytes in a hundred of the bytes of all
/// blocks are dead, and at most 20 in a hundred of those of all values.
#[test]
fn dead_space_stays_at_or_below_20_percent_when_the_engine_commits_the_blocks() {
    let measured = run(400, FROM, true);
    assert!(measured.blocks > 100, "{measured:?}");
    assert!(measured.most_dead <= 20.0, "{measured:?}");
    assert!(measured.most_dead_values <= 20.0, "{measured:?}");
}

/// The same blocks with no cleaner: what the bound is kept against.
#[test]
fn without_the_cleaner_most_of_what_the_engine_wrote_is_dead() {
    let measured = run(150, 0, false);
    assert!(measured.most_dead > 50.0, "{measured:?}");
}

/// Runs what it was given when it goes, however the scope ends: a loop
/// on another thread is stopped although the test fails.
struct OnDrop<F: FnMut()>(F);

impl<F: FnMut()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

/// Waits until `reached` says so, for a thread that another one has to
/// catch up with. A wait that does not end fails the test.
fn wait_until(what: &str, reached: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !reached() {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "waited two minutes for {what}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// **A committer, write-back, a reader with a pin and the cleaner at
/// once, on the commit path.**
///
/// Blocks are committed back to back, each with changes that make values
/// die. Write-back's loop and the cleaner's loop run on threads of their
/// own, with small files, so that segments and value-log files are
/// emptied and deleted all the time. A reader takes a snapshot, which
/// pins its version, reads every document and every note of it, waits
/// until commits have gone by, and reads them again: each time it is
/// given exactly what that version committed, every value whole, wherever
/// write-back and the cleaner have put the blocks and the values since.
/// Every fourth pin, the physical invariant checker reads that version
/// too, as a reader (`verify::Scope::Reader`), and finds it sound.
///
/// The threads take turns by construction: the reader holds each pin
/// until two blocks are committed after it, and every few blocks the
/// committer waits for a pin of the reader to end, which it then does at
/// once, and for passes of the cleaner to be made.
#[test]
fn the_cleaner_runs_beside_the_commit_path_write_back_and_a_snapshot() {
    const BLOCKS: u64 = 240;
    let flush = FlushConfig {
        max_unwritten: 64,
        max_pending_bytes: 96 << 10,
        max_log_held: 192 << 10,
        checkpoint_log_bytes: 64 << 10,
        segment_len: 128 << 10,
        idle: Duration::from_millis(1),
        ..FlushConfig::new(SIZE)
    };
    let (db, versions) = store(12, 48, flush);
    let mut world = World::new(&db, &versions);
    let write_back = db.write_back(&versions);
    let state = CleanerState::new(CleanerConfig {
        min_segment_len: 16 << 10,
        segment_len: 128 << 10,
        idle: Duration::from_millis(1),
        ..CleanerConfig::new()
    });
    let cleaner = Cleaner::new(write_back, &state).with_stats(&db.stats);
    let mut grove = Grove::default();
    // What each version says. A commit is made and its version is put
    // here under one lock, so whoever has seen a version finds it here.
    let published = Mutex::new(BTreeMap::from([(versions.latest().get(), grove.said())]));
    let (done, failed) = (AtomicBool::new(false), AtomicBool::new(false));
    // Whether the committer waits for the reader.
    let waits = AtomicBool::new(false);
    let cleaner_errors = Mutex::new(Vec::new());
    let (pins, behind, documents_read) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let checks = AtomicU64::new(0);

    std::thread::scope(|scope| {
        scope.spawn(|| write_back.run(|_| failed.store(true, Ordering::Relaxed)));
        let _stop = OnDrop(|| write_back.stop());
        scope.spawn(|| {
            cleaner.run(
                |cleaner| db.clean_values(&versions, cleaner),
                |error| (cleaner_errors.lock().expect("the errors")).push(error.to_string()),
            )
        });
        let _stop_cleaner = OnDrop(|| cleaner.stop());
        scope.spawn(|| {
            let reads = db.reads(&versions);
            let ended = || done.load(Ordering::SeqCst) || waits.load(Ordering::SeqCst);
            while !done.load(Ordering::SeqCst) {
                let snapshot = reads.snapshot();
                let at = snapshot.version();
                let expected = *(published.lock().expect("the versions"))
                    .get(&at.get())
                    .expect("a version that was published");
                assert_eq!(read(&db, &snapshot.operation()), expected, "at {at}");
                // Every fourth pin, the physical invariant checker reads
                // the pinned version as a reader does, under an operation
                // guard of its own, while the cleaner works.
                if pins.load(Ordering::SeqCst) % 4 == 0 {
                    let _guard = versions.guard();
                    db.check_trees(&versions, at);
                    checks.fetch_add(1, Ordering::Relaxed);
                }
                // The pin is held while two blocks are committed, and
                // while write-back and the cleaner work on what they
                // superseded.
                while versions.latest().get() < at.get() + 2 && !ended() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(read(&db, &snapshot.operation()), expected, "at {at}, later");
                behind.fetch_add(u64::from(at < versions.latest()), Ordering::Relaxed);
                documents_read.fetch_add(2 * expected.documents, Ordering::Relaxed);
                drop(snapshot);
                pins.fetch_add(1, Ordering::SeqCst);
                // Nothing is pinned for a moment: what the pin kept goes.
                std::thread::sleep(Duration::from_millis(2));
            }
        });

        let _done = OnDrop(|| done.store(true, Ordering::SeqCst));
        for round in 0..BLOCKS {
            let tx = grove.write(&world, round, true);
            {
                let mut published = published.lock().expect("the versions");
                let version = Grove::commit(&mut world, tx);
                published.insert(version, grove.said());
            }
            if round % 8 == 7 {
                let (pinned, passed) = (pins.load(Ordering::SeqCst), state.stats());
                waits.store(true, Ordering::SeqCst);
                wait_until("a pin of the reader to end", || {
                    pins.load(Ordering::SeqCst) > pinned
                });
                waits.store(false, Ordering::SeqCst);
                wait_until("passes of the cleaner", || {
                    let did = state.stats();
                    did.passes >= passed.passes + 3 && did.value_passes >= passed.value_passes + 3
                });
            }
        }
    });
    assert!(
        !failed.load(Ordering::Relaxed),
        "a turn of write-back failed"
    );
    let cleaner_errors = cleaner_errors.into_inner().expect("the errors");
    assert!(cleaner_errors.is_empty(), "{cleaner_errors:?}");

    let (did, flushed) = (state.stats(), db.flush.stats());
    let (pins, behind) = (pins.into_inner(), behind.into_inner());
    let values = cleaner.value_space().expect("the value log");
    println!(
        "{did:?}\n{pins} pins, {behind} of them behind, {} documents read; {} KiB of values, \
         {} KiB of them live",
        documents_read.into_inner(),
        values.bytes >> 10,
        values.live_bytes >> 10,
    );
    assert_eq!((did.errors, did.segments_unreadable), (0, 0), "{did:?}");
    assert!(did.links_trimmed > 0 && did.blocks_moved > 0, "{did:?}");
    assert!(did.segments_dropped > 0, "{did:?}");
    assert!(
        did.values_moved > 0 && did.value_files_dropped > 0,
        "{did:?}"
    );
    assert!(flushed.checkpoints > 0, "{flushed:?}");
    assert_eq!(
        db.stats.cleaner_bytes(),
        did.bytes_moved + did.bytes_lost + did.value_bytes_copied
    );
    assert!(
        pins >= BLOCKS / 8 && behind >= BLOCKS / 16,
        "{pins} pins, {behind} behind"
    );
    let checks = checks.into_inner();
    assert!(checks >= pins / 4, "{checks} checks in {pins} pins");
    // The cleaner did not copy the value log over and over: a pin that
    // keeps emptied files has no other file cleaned for them.
    assert!(
        did.value_bytes_copied < 8 * values.bytes,
        "{} bytes of values were copied, the value log holds {}",
        did.value_bytes_copied,
        values.bytes
    );

    // What the threads left is whole, and it still is once the cleaner
    // has taken what the last table checkpoint sets free.
    check_whole(&world, &db, &versions);
    write_back.checkpoint().expect("a checkpoint");
    for _ in 0..4 {
        cleaner.pass().expect("a pass over the segments");
        (db.clean_values(&versions, &cleaner)).expect("a pass over the value log");
    }
    cleaner.finish().expect("the cleaner's files are closed");
    check_whole(&world, &db, &versions);
    let snapshot = db.reads(&versions).snapshot();
    assert_eq!(read(&db, &snapshot.operation()), grove.said());
}
