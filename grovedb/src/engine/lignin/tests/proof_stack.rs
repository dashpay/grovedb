//! The stack that the proof of a query takes, on LigninDB and on RocksDB.
//!
//! The prover calls itself once for every tree inside a tree that a query
//! goes down into, so the stack a proof takes grows with the depth of its
//! query, up to `MAX_PROOF_DEPTH`. A proof is made on the thread of its
//! caller, with the stack that caller chose. The engine is part of what a
//! level takes: the prover holds the Merk of each level in its frame, with
//! the engine's handle of the subtree in it, and the reads of the engine
//! are the calls below the deepest frame.
//!
//! The test compares the two engines in one build, on every grove version,
//! over a chain of trees, in two ways.
//!
//! **Where a thread overflows.** For each engine it finds the shallowest
//! query whose proof overflows the stack of a thread of [`STACK`] bytes.
//! Each try is a process of its own: this binary, run for the one test
//! [`a_thread_of_a_given_stack_proves_a_query`], which makes a thread with
//! a stack of that size before the process has ended any thread, opens the
//! database, and proves on that thread. A try is proved where its process
//! ends well and says so, and has overflowed where the process was aborted
//! and names that thread as the one whose stack overflowed; any other end
//! fails the test. The test asserts that the query at which LigninDB
//! overflows is no shallower than the one at which RocksDB does.
//!
//! **How much stack a proof touches.** At each depth of [`COMPARED`] the
//! proof is made on a new thread with a stack of [`MEASURED`] bytes, and
//! the figure is how far below a local of that thread the pages reach that
//! the kernel has given the process in one run down from the local's page
//! (`/proc/self/pagemap`). A figure is taken only where the proof reached
//! further than the thread had before it. It ends at the start of a page,
//! so it is up to one page more than the proof took; a page that a frame
//! reached past without a touch would end the run there, and the figure
//! would be less than the proof took. Each depth is measured twice in a
//! database that was just opened, first with nothing of it read and then
//! again, and the larger figure is the depth's. The test asserts, each
//! with the pages the figures may be off by:
//!
//! - from the second depth of [`COMPARED`] to the third, the figure grows
//!   by no more on LigninDB than on RocksDB;
//! - at the first depth, where what grows with the depth is least,
//!   LigninDB takes no more than RocksDB and [`OWN`].
//!
//! The figures are printed. They are not taken, and the two assertions on
//! them not made, where [`unmeasured`] or [`measured`] names a reason: the
//! test prints it. The first part needs none of it.

use std::{
    fs::File,
    io::{ErrorKind, Read, Seek, SeekFrom},
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{mpsc, OnceLock},
};

use grovedb_merk::Merk;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{nested::open, LigninEngine};
use crate::{
    engine::Engine, operations::proof::MAX_PROOF_DEPTH, Element, GroveDb, PathQuery, Query,
    SizedQuery,
};

/// The stack that `std` gives a thread that is made with no size named,
/// where `RUST_MIN_STACK` names none either.
const STACK: usize = 2 * 1024 * 1024;

/// The stack of a measuring thread, and of the thread that makes the
/// chain.
const MEASURED: usize = 64 * 1024 * 1024;

/// How far below its local a measuring thread may have touched its stack
/// before the proof, and a page.
const BEFORE: usize = 64 * 1024;

/// The deepest query the prover takes below a path of one segment.
const DEEPEST: usize = MAX_PROOF_DEPTH - 1;

/// The depths at which the stack of the two engines is measured.
const COMPARED: [usize; 3] = [1, 8, 64];

/// How much more stack the proof of the shallowest query of [`COMPARED`]
/// may take on LigninDB than on RocksDB. What does not grow with the depth
/// is each engine's own: its calls below the deepest frame of the prover,
/// and its transaction in the frames above the first. The two engines are
/// not the same size there, and this is the room for it.
const OWN: usize = 16 * 1024;

/// The variable by which a process of this binary is told what to prove:
/// the engine's name, the number of the grove version, the depth of the
/// query and the stack of the thread in bytes, with a colon between them.
const ASKED: &str = "LIGNIN_PROOF_STACK";

/// The variable that names the directory of the database to such a
/// process.
const ASKED_IN: &str = "LIGNIN_PROOF_STACK_IN";

/// The name of the thread that proves in such a process.
const PROVER: &str = "the thread that proves";

/// The signal of an abort (`SIGABRT`), by which the runtime ends a
/// process whose thread overflowed its stack.
const ABORT: i32 = 6;

/// What a process that proved prints, with the length of the proof.
const PROVED: &str = "the proof was made:";

const LIGNINDB: &str = "LigninDB";
const ROCKSDB: &str = "RocksDB";

/// A chain of [`DEEPEST`] trees, one inside the other, under the tree
/// `deep` at the root, with an item in the innermost.
fn chain<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) {
    let mut at: Vec<Vec<u8>> = Vec::new();
    for level in 0..=DEEPEST + 1 {
        let key = match level {
            0 => b"deep".to_vec(),
            level => (level - 1).to_string().into_bytes(),
        };
        let element = match level {
            level if level > DEEPEST => Element::new_item(b"innermost".to_vec()),
            _ => Element::empty_tree(),
        };
        let path: Vec<&[u8]> = at.iter().map(Vec::as_slice).collect();
        db.insert(path.as_slice(), &key, element, None, None, v)
            .unwrap()
            .expect("a level of the chain");
        at.push(key);
    }
}

/// The query of every key of `deep`, with `depth` subqueries of every
/// key, each inside the one before.
fn descending(depth: usize) -> PathQuery {
    let mut query = Query::new();
    query.insert_all();
    for _ in 0..depth {
        let mut outer = Query::new();
        outer.insert_all();
        outer.set_subquery(query);
        query = outer;
    }
    PathQuery::new(
        vec![b"deep".to_vec()],
        SizedQuery::new(query, Some(100), None),
    )
}

/// The size of the handle that engine `E` gives of a subtree, and of a
/// Merk on that handle, in bytes.
fn sizes<'db, E: Engine + 'db>() -> (usize, usize) {
    (
        size_of::<E::Subtree<'db>>(),
        size_of::<Merk<E::Subtree<'db>>>(),
    )
}

fn on_a_stack_of<T: Send>(bytes: usize, run: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        let thread = std::thread::Builder::new().stack_size(bytes);
        let thread = thread.spawn_scoped(scope, run).expect("a thread");
        thread.join().expect("the thread ends")
    })
}

/// The length of the proof of the query `depth` deep, made on a thread
/// with a stack of `stack` bytes in the database of `dir`. The thread is
/// made first and waits for the open of the database.
fn proved_on<E: Engine>(dir: &Path, v: &GroveVersion, depth: usize, stack: usize) -> usize
where
    GroveDb<E>: Send + Sync,
{
    let query = descending(depth);
    let db = OnceLock::new();
    let (opened, waits) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        // The end of this closure, also by a panic, ends the wait.
        let opened = opened;
        let (db, query) = (&db, &query);
        let thread = std::thread::Builder::new().stack_size(stack);
        let thread = thread.name(PROVER.to_owned());
        let prove = move || {
            waits.recv().expect("the database is opened");
            let db: &GroveDb<E> = db.get().expect("the database");
            let proof = db.prove_query(query, None, v).unwrap().expect("a proof");
            proof.len()
        };
        let thread = thread.spawn_scoped(scope, prove).expect("a thread");
        assert!(db.set(open::<E>(dir)).is_ok(), "the database is set once");
        opened.send(()).expect("the thread waits");
        thread.join().expect("the thread ends")
    })
}

/// Proves what [`ASKED`] names and prints [`PROVED`], in a process that
/// [`fits`] started. In a run that was asked nothing it does nothing.
#[test]
fn a_thread_of_a_given_stack_proves_a_query() {
    let Ok(asked) = std::env::var(ASKED) else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(ASKED_IN).expect("a directory"));
    let dir = dir.as_path();
    let mut parts = asked.split(':');
    let mut part = || parts.next().expect("four parts");
    let engine = part();
    let number = |part: &str| part.parse::<usize>().expect("a number");
    let v = &GROVE_VERSIONS[number(part()) - 1];
    let (depth, stack) = (number(part()), number(part()));
    let proof = match engine {
        LIGNINDB => proved_on::<LigninEngine>(dir, v, depth, stack),
        ROCKSDB => proved_on::<crate::RocksDbEngine>(dir, v, depth, stack),
        other => panic!("no engine {other}"),
    };
    println!("{PROVED} {proof}");
}

/// Whether a thread with a stack of [`STACK`] bytes proves the query
/// `depth` deep on `engine`, in a process of its own: it does where the
/// process ended well and printed [`PROVED`], and it does not where the
/// process was aborted and said that the stack of [`PROVER`] overflowed.
/// Any other end of the process is a panic here.
fn fits(engine: &str, dir: &Path, version: usize, depth: usize) -> bool {
    let (_, module) = module_path!().split_once("::").expect("the crate");
    let test = format!("{module}::a_thread_of_a_given_stack_proves_a_query");
    let asked = format!("{engine}:{version}:{depth}:{STACK}");
    let started_as = || std::env::args_os().next().map(PathBuf::from);
    let binary = std::env::current_exe().ok().or_else(started_as);
    let binary = binary.expect("the path of this binary");
    // Through a shell that allows the process no core file: an overflow
    // is an abort.
    let process = Command::new("sh")
        .args(["-c", "ulimit -c 0; exec \"$0\" \"$@\""])
        .arg(binary)
        .args(["--exact", &test, "--nocapture", "--test-threads=1"])
        .env(ASKED, &asked)
        .env(ASKED_IN, dir)
        .output()
        .expect("a process of this binary");
    let printed = String::from_utf8_lossy(&process.stdout);
    let said = String::from_utf8_lossy(&process.stderr);
    if process.status.success() && printed.contains(PROVED) {
        return true;
    }
    let of_the_prover = format!("thread '{PROVER}'");
    let overflowed = said
        .lines()
        .any(|line| line.contains(&of_the_prover) && line.contains("has overflowed its stack"));
    assert!(
        process.status.signal() == Some(ABORT) && overflowed,
        "the process that was asked {asked} ended with {:?}:\n{printed}\n{said}",
        process.status
    );
    false
}

/// The shallowest query whose proof overflows a thread of [`STACK`] bytes
/// on `engine`, found on the assumption that a deeper query takes no less:
/// one past [`DEEPEST`] where the deepest is proved.
fn overflows_at(engine: &str, dir: &Path, version: usize) -> usize {
    // Every query that was tried and is shallower than `proved` was
    // proved, and every one from `over` on that was tried overflowed.
    let (mut proved, mut over) = (0, DEEPEST + 1);
    while proved < over {
        let depth = proved + (over - proved) / 2;
        match fits(engine, dir, version, depth) {
            true => proved = depth + 1,
            false => over = depth,
        }
    }
    over
}

/// The size of a page, as the kernel told the process at its start
/// (`AT_PAGESZ` of the auxiliary vector).
fn page_size() -> Result<usize, String> {
    const AT_PAGESZ: usize = 6;
    const WORD: usize = size_of::<usize>();
    let vector = std::fs::read("/proc/self/auxv").map_err(|e| format!("/proc/self/auxv: {e}"))?;
    let word = |bytes: &[u8]| usize::from_ne_bytes(bytes.try_into().expect("a word"));
    let (entries, _) = vector.as_chunks::<{ 2 * WORD }>();
    let mut entries = entries
        .iter()
        .map(|entry| (word(&entry[..WORD]), word(&entry[WORD..])));
    let size = entries.find(|(kind, _)| *kind == AT_PAGESZ);
    let (_, size) = size.ok_or("the auxiliary vector names no page size")?;
    Ok(size)
}

/// Why no figure is taken on this machine, where a reason is known before
/// a measurement: a setting of the kernel for huge pages, the one for all
/// sizes or the one of a size, is `always`, under which a touch of a stack
/// may be given more than a page, or the settings that are there cannot
/// all be read.
fn unmeasured() -> Option<String> {
    let settings = Path::new("/sys/kernel/mm/transparent_hugepage");
    let sizes = match std::fs::read_dir(settings) {
        Ok(sizes) => sizes,
        Err(e) if e.kind() == ErrorKind::NotFound => return None,
        Err(e) => return Some(format!("{}: {e}", settings.display())),
    };
    let mut all = vec![settings.join("enabled")];
    for size in sizes {
        let size = match size {
            Ok(size) => size.file_name(),
            Err(e) => return Some(format!("{}: {e}", settings.display())),
        };
        if size.as_encoded_bytes().starts_with(b"hugepages-") {
            all.push(settings.join(size).join("enabled"));
        }
    }
    all.into_iter().find_map(|setting| {
        let why = match std::fs::read_to_string(&setting) {
            Ok(modes) => modes.contains("[always]").then(|| "is `always`".to_owned()),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => Some(e.to_string()),
        };
        why.map(|why| format!("{}: {why}", setting.display()))
    })
}

/// How many bytes below the address `top`, a local of the calling thread,
/// its stack was touched: down to the start of the lowest page of the run
/// of pages that the kernel has given the process and that ends at the
/// page of `top`, within [`MEASURED`] bytes. An error where the kernel
/// does not say.
fn touched_below(top: usize) -> Result<usize, String> {
    const GIVEN: u64 = 0b11 << 62;
    let map = "/proc/self/pagemap";
    let page = page_size()?;
    let (first, last) = (top.saturating_sub(MEASURED) / page, top / page);
    let mut entries = vec![0; 8 * (last - first + 1)];
    let read = File::open(map).and_then(|mut map| {
        map.seek(SeekFrom::Start(8 * first as u64))?;
        map.read_exact(&mut entries)
    });
    read.map_err(|e| format!("{map}: {e}"))?;
    let (entries, _) = entries.as_chunks::<8>();
    let given = |entry: &&[u8; 8]| u64::from_ne_bytes(**entry) & GIVEN != 0;
    let run = entries.iter().rev().take_while(given).count();
    if run == 0 {
        return Err(format!("{map} names the page of a local as not given"));
    }
    Ok(top - (last + 1 - run) * page)
}

/// The stack that the proof of a query `depth` deep touched on a new
/// thread, in bytes, with the length of the proof. An error where the
/// kernel does not say, where the thread's stack was touched further than
/// [`BEFORE`] and a page before the proof, or where the proof reached no
/// further than the thread had before it.
fn measured<E: Engine>(
    db: &GroveDb<E>,
    depth: usize,
    v: &GroveVersion,
) -> Result<(usize, usize), String>
where
    GroveDb<E>: Sync,
{
    let query = descending(depth);
    on_a_stack_of(MEASURED, || {
        let local = 0u8;
        let top = std::ptr::from_ref(std::hint::black_box(&local)) as usize;
        let before = touched_below(top)?;
        if before > BEFORE + page_size()? {
            return Err(format!(
                "{before} bytes of the stack touched before the proof"
            ));
        }
        let proof = db.prove_query(&query, None, v).unwrap().expect("a proof");
        let after = touched_below(top)?;
        if after <= before {
            return Err(format!(
                "the proof reached no further than the {before} bytes touched before it"
            ));
        }
        Ok((after, proof.len()))
    })
}

/// What was measured of one engine on one grove version: at each depth of
/// [`COMPARED`], the stack of the first proof in a database that was just
/// opened, of the same proof again, and the length of the proof.
struct Taken([(usize, usize, usize); 3]);

impl Taken {
    /// The stack at the depth with the number `nth` in [`COMPARED`].
    fn stack(&self, nth: usize) -> usize {
        let (first, again, _) = self.0[nth];
        first.max(again)
    }

    /// How much more stack the proof took at the third depth of
    /// [`COMPARED`] than at the second.
    fn grown(&self) -> usize {
        self.stack(2).saturating_sub(self.stack(1))
    }

    fn proofs(&self) -> [usize; 3] {
        self.0.map(|(.., proof)| proof)
    }
}

fn taken<E: Engine>(dir: &Path, v: &GroveVersion) -> Result<Taken, String>
where
    GroveDb<E>: Sync,
{
    let mut taken = [(0, 0, 0); 3];
    for (taken, depth) in taken.iter_mut().zip(COMPARED) {
        let db = open::<E>(dir);
        let (first, proof) = measured(&db, depth, v)?;
        let (again, same) = measured(&db, depth, v)?;
        assert_eq!(proof, same, "the proof at depth {depth}");
        *taken = (first, again, proof);
    }
    Ok(Taken(taken))
}

#[test]
fn a_proof_takes_no_more_stack_on_lignindb_than_on_rocksdb() {
    let sized = [sizes::<LigninEngine>(), sizes::<crate::RocksDbEngine>()];
    for (name, (handle, merk)) in [LIGNINDB, ROCKSDB].into_iter().zip(sized) {
        println!("{name}: a Merk is {merk} bytes with a handle of {handle}");
    }
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let on_lignin = tempfile::tempdir().expect("a directory");
        let on_rocksdb = tempfile::tempdir().expect("a directory");
        let (on_lignin, on_rocksdb) = (on_lignin.path(), on_rocksdb.path());
        on_a_stack_of(MEASURED, || chain(&open::<LigninEngine>(on_lignin), v));
        on_a_stack_of(MEASURED, || {
            chain(&open::<crate::RocksDbEngine>(on_rocksdb), v)
        });

        let lignin = overflows_at(LIGNINDB, on_lignin, number);
        let rocksdb = overflows_at(ROCKSDB, on_rocksdb, number);
        println!(
            "grove version {number}: the shallowest query whose proof overflows a thread of \
             {STACK} bytes is {lignin} deep on LigninDB and {rocksdb} on RocksDB ({} is none)",
            DEEPEST + 1
        );
        assert!(
            lignin >= rocksdb,
            "grove version {number}: a thread of {STACK} bytes overflows at a query {lignin} deep \
             on LigninDB and {rocksdb} deep on RocksDB"
        );

        let figures = match unmeasured() {
            Some(why) => Err(why),
            None => taken::<LigninEngine>(on_lignin, v).and_then(|lignin| {
                let rocksdb = taken::<crate::RocksDbEngine>(on_rocksdb, v)?;
                Ok((lignin, rocksdb, page_size()?))
            }),
        };
        let (lignin, rocksdb, page) = match figures {
            Ok(figures) => figures,
            Err(why) => {
                println!("grove version {number}: the stack of a proof is not measured: {why}");
                continue;
            }
        };
        let [shallowest, low, high] = COMPARED;
        for (name, taken) in [(LIGNINDB, &lignin), (ROCKSDB, &rocksdb)] {
            println!(
                "grove version {number}, {name}: {} bytes a level from depth {low} to {high}; \
                 by depth (first proof, again, bytes of proof): {:?}",
                taken.grown() / (high - low),
                COMPARED.iter().zip(taken.0).collect::<Vec<_>>()
            );
        }
        assert_eq!(lignin.proofs(), rocksdb.proofs(), "grove version {number}");
        assert!(
            lignin.stack(0) <= rocksdb.stack(0) + OWN + page,
            "grove version {number}, a query {shallowest} deep: the proof takes {} bytes of stack \
             on LigninDB and {} on RocksDB",
            lignin.stack(0),
            rocksdb.stack(0)
        );
        assert!(
            lignin.grown() <= rocksdb.grown() + 2 * page,
            "grove version {number}, from a query {low} deep to one {high} deep: the proof takes \
             {} bytes of stack more on LigninDB and {} on RocksDB",
            lignin.grown(),
            rocksdb.grown()
        );
    }
}
