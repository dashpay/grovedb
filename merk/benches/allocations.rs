//! Heap allocations of a Merk for one state transition, counted at the
//! allocator (LigninDB task MERK-08).
//!
//! The acceptance of PERF-01 names the allocations of a transition at
//! MERK-08 as what it is measured against, and this is how MERK-08
//! counted them (`docs/lignindb/node-store.md`, "What later tasks get",
//! has the count). The count is of calls and not of time. The keys and the
//! values are the same on every run. The count is not: runs of one build
//! on LigninDB differed by a fraction of a call for a transition. What
//! the threads of the database have done by the time of a read or of a
//! commit decides some of the calls.
//!
//! ```text
//! cargo bench -p grovedb-merk --features lignin_storage --bench allocations
//! ```
//!
//! # What a transition is
//!
//! The reference workload of the LigninDB plan adds one document of 400
//! bytes and one reference to it with each state transition, twenty
//! transitions to a block, into the Merks of a grove. Here a transition
//! is two puts into one Merk: a value of 400 bytes and a value of 36
//! bytes, which stands for the reference by its size and is no encoded
//! reference. The Merk holds `MERK_ALLOCATIONS_DOCUMENTS` such pairs
//! before the first counted transition (100,000 unless the variable says
//! another number). A transition is, in the write transaction of its
//! block:
//!
//! 1. a deferred handle of the Merk's subtree is opened with a batch of
//!    its own, and the Merk is opened on it with the root key it had
//!    after the transition before, as GroveDB opens the Merk of a tree
//!    with the root key its element holds
//!    (`Merk::open_layered_with_root_key`);
//! 2. one batch of the two puts is applied (`Merk::apply`), each under a
//!    key of 32 bytes that the Merk does not hold, and the Merk is
//!    dropped;
//! 3. the batch is applied to the transaction (`TestEngine::apply_batch`).
//!
//! After twenty transitions the transaction is committed and the next is
//! started. `MERK_ALLOCATIONS_BLOCKS` blocks are counted (50 unless the
//! variable says another number, which is not 0).
//!
//! # What is counted
//!
//! Every call that asks the allocator for memory (`alloc`, `alloc_zeroed`,
//! `realloc`) on the thread that runs the transitions, whether or not it
//! is given any, and the bytes asked for, in three parts: the Merk (steps 1 and 2), the application
//! of the batch (step 3), and the transaction (its start and its commit,
//! as a twentieth of a block's). The keys and values of a transition's
//! puts are made before its count starts. What a database does on threads
//! of its own, as write-back does, is not counted.
//!
//! The engines are those of Merk's test suite (`merk::test_utils`):
//! LigninDB, whose handles are `store::lignin` on a write transaction, and
//! RocksDB beside it. The row of RocksDB is not the whole of what RocksDB
//! allocates: its library asks the system for its memory itself, and
//! only the calls of the Rust code above it are counted. On LigninDB every
//! layer is Rust, and every call is counted.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    env,
};

use grovedb_merk::{
    test_utils::{LigninTestEngine, RocksDbTestEngine, TestEngine},
    tree::kv::ValueDefinedCostType,
    Merk, Op, TreeFeatureType, TreeType,
};
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

thread_local! {
    /// The calls and the bytes this thread asked the allocator for.
    static ASKED: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

struct CountingAllocator;

impl CountingAllocator {
    fn count(bytes: usize) {
        let _ = ASKED.try_with(|asked| {
            let (calls, total) = asked.get();
            asked.set((calls.wrapping_add(1), total.wrapping_add(bytes as u64)));
        });
    }
}

// SAFETY: every method hands its arguments, unchanged, to the same method
// of `System`, so the memory is what `System` gives and takes back under
// the contract the caller keeps. The three that ask for memory count
// first. Counting adds, wrapping, to a thread-local cell that is a
// constant at first and has no destructor: it allocates nothing and
// cannot unwind.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::count(layout.size());
        // SAFETY: the caller keeps the contract of `GlobalAlloc::alloc`,
        // which is the contract of `System.alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::count(layout.size());
        // SAFETY: the caller keeps the contract of
        // `GlobalAlloc::alloc_zeroed`, which is the contract of
        // `System.alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::count(new_size);
        // SAFETY: the caller keeps the contract of `GlobalAlloc::realloc`,
        // which is the contract of `System.realloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller keeps the contract of `GlobalAlloc::dealloc`,
        // which is the contract of `System.dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// What this thread asked the allocator for so far: calls and bytes.
fn asked() -> (u64, u64) {
    ASKED.with(Cell::get)
}

/// What was asked for since `before`, added to `part`.
fn add_since(part: &mut (u64, u64), before: (u64, u64)) {
    let now = asked();
    part.0 += now.0.wrapping_sub(before.0);
    part.1 += now.1.wrapping_sub(before.1);
}

const TRANSITIONS_IN_A_BLOCK: u64 = 20;
const DOCUMENT_LEN: usize = 400;
const REFERENCE_LEN: usize = 36;

/// The keys of the run: 32 bytes each, the same on every run.
struct Keys(u64);

impl Keys {
    fn next(&mut self) -> Vec<u8> {
        let mut key = Vec::with_capacity(32);
        for _ in 0..4 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            key.extend(self.0.to_be_bytes());
        }
        key
    }

    /// The puts of `transitions` transitions, sorted by key: a document
    /// and a reference for each.
    fn puts(&mut self, transitions: u64) -> Vec<(Vec<u8>, Op)> {
        let feature = TreeFeatureType::BasicMerkNode;
        let mut puts = Vec::new();
        for _ in 0..transitions {
            puts.push((self.next(), Op::Put(vec![0xd0; DOCUMENT_LEN], feature)));
            puts.push((self.next(), Op::Put(vec![0x4e; REFERENCE_LEN], feature)));
        }
        puts.sort_by(|a, b| a.0.cmp(&b.0));
        puts
    }
}

/// What the transitions of a run asked the allocator for, by part.
#[derive(Default)]
struct Counted {
    merk: (u64, u64),
    batch: (u64, u64),
    transaction: (u64, u64),
}

/// Applies `puts` to the Merk under `root` as one transition, and counts
/// its parts into `counted` if there is one.
fn transition<E: TestEngine>(
    engine: &E,
    transaction: &E::Transaction<'_>,
    root: &mut Option<Vec<u8>>,
    puts: &[(Vec<u8>, Op)],
    counted: Option<&mut Counted>,
) {
    let grove_version = GroveVersion::latest();
    let mut unused = Counted::default();
    let counted = counted.unwrap_or(&mut unused);

    let before = asked();
    let batch = E::Batch::default();
    let store = engine.subtree(SubtreePath::empty(), Some(&batch), transaction);
    let mut merk = Merk::open_layered_with_root_key(
        store.unwrap(),
        root.take(),
        TreeType::NormalTree,
        None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
        grove_version,
    )
    .unwrap()
    .expect("the merk opens");
    merk.apply::<_, Vec<u8>>(puts, &[], None, grove_version)
        .unwrap()
        .expect("the batch applies");
    *root = merk.root_key();
    drop(merk);
    add_since(&mut counted.merk, before);

    let before = asked();
    engine
        .apply_batch(batch, transaction)
        .unwrap()
        .expect("the batch is applied");
    add_since(&mut counted.batch, before);
}

/// Runs the workload on a new database of `E` and returns what the counted
/// transitions asked the allocator for.
fn run<E: TestEngine>(documents: u64, blocks: u64) -> Counted {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let engine = E::open(directory.path());
    let mut keys = Keys(0x9E37_79B9_7F4A_7C15);
    let mut root = None;

    // The documents the Merk holds before the count, a thousand to a
    // transaction.
    let mut loaded = 0;
    while loaded < documents {
        let transitions = (documents - loaded).min(1_000);
        let puts = keys.puts(transitions);
        let transaction = engine.start_transaction();
        transition(&engine, &transaction, &mut root, &puts, None);
        engine
            .commit_transaction(transaction)
            .unwrap()
            .expect("the transaction commits");
        loaded += transitions;
    }

    let mut counted = Counted::default();
    for _ in 0..blocks {
        let puts: Vec<_> = (0..TRANSITIONS_IN_A_BLOCK).map(|_| keys.puts(1)).collect();
        let before = asked();
        let transaction = engine.start_transaction();
        add_since(&mut counted.transaction, before);
        for puts in &puts {
            transition(&engine, &transaction, &mut root, puts, Some(&mut counted));
        }
        let before = asked();
        engine
            .commit_transaction(transaction)
            .unwrap()
            .expect("the transaction commits");
        add_since(&mut counted.transaction, before);
    }
    counted
}

fn number(variable: &str, default: u64) -> u64 {
    match env::var(variable) {
        Ok(value) => value
            .parse()
            .unwrap_or_else(|_| panic!("{variable} is no number: {value}")),
        Err(_) => default,
    }
}

fn main() {
    // `cargo bench` says `--bench`. Without it, as under `cargo test
    // --benches`, the run is a small one that shows the target works.
    let measured = env::args().any(|argument| argument == "--bench");
    let (documents, blocks) = match measured {
        true => (
            number("MERK_ALLOCATIONS_DOCUMENTS", 100_000),
            number("MERK_ALLOCATIONS_BLOCKS", 50),
        ),
        false => (200, 2),
    };
    let transitions = blocks.checked_mul(TRANSITIONS_IN_A_BLOCK);
    let transitions = transitions.filter(|transitions| *transitions > 0);
    let transitions = transitions.expect("MERK_ALLOCATIONS_BLOCKS is 0 or too many");
    println!(
        "allocations of a Merk for one state transition: {documents} documents held, \
         {transitions} transitions counted in {blocks} blocks"
    );
    println!(
        "{:<10} {:>12} {:>12} {:>12} {:>12} {:>14}",
        "engine", "merk", "batch", "transaction", "total", "bytes"
    );
    let report = |engine: &str, counted: Counted| {
        let each = |part: (u64, u64)| part.0 as f64 / transitions as f64;
        let calls = counted.merk.0 + counted.batch.0 + counted.transaction.0;
        let bytes = counted.merk.1 + counted.batch.1 + counted.transaction.1;
        println!(
            "{:<10} {:>12.2} {:>12.2} {:>12.2} {:>12.2} {:>14.0}",
            engine,
            each(counted.merk),
            each(counted.batch),
            each(counted.transaction),
            calls as f64 / transitions as f64,
            bytes as f64 / transitions as f64,
        );
        println!(
            "{:<10} {calls} calls in {transitions} transitions: {} of the Merk, {} of the batch, \
             {} of the transaction",
            format!("{engine}:"),
            counted.merk.0,
            counted.batch.0,
            counted.transaction.0,
        );
    };
    report("lignin", run::<LigninTestEngine>(documents, blocks));
    report("rocksdb", run::<RocksDbTestEngine>(documents, blocks));
}
