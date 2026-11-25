# Engine security review (VER-10)

The subject is the LigninDB engine as the branch had it when this review
began. This document is the review's record: the threat model, what the
means that were already here cover and what they leave, and a table of
every finding with its severity, where it is, what an attacker supplies,
what happens, and its status.

The three questions the review answers, from the task:

1. **Corrupted files never cause a panic or undefined behaviour.**
2. **Adversarial keys and values cannot exhaust memory.**
3. **Hostile state-sync chunks are rejected as they are on the RocksDB
   engine.**

The three surfaces, in the order an attacker reaches them: the network (a
state-sync peer), the disk (a file that is not what the engine wrote), and
the application's users (keys, values and operations). `audit-non-issues.md`
records the findings against GroveDB and Merk that were adversarially
verified and rejected; this review does not re-file from it.

## 1. The attackers

- **A state-sync peer** answers the requests of a node that syncs. It
  chooses every byte of every payload, and the chunk ids, tree types and
  root keys in a request it serves. It cannot choose what a node already
  holds: a syncing node checks each subtree against the hash its parent
  commits to, and a serving node answers from its own committed state.
- **The disk** gives back bytes that the engine did not write. Two kinds:
  - **Random damage**: a flipped bit, a torn or lost sector, a truncated
    or zero-filled file, a file of another length or a file that is gone.
    A checksum (CRC32C) is meant to catch it. A page a checksum passes
    agrees with the checksum stored beside it; that is not proof of who
    wrote the bytes (an accidental collision is possible), and a page made
    by hand carries a checksum made to match, which is the next kind.
  - **A directory made by hand with valid checksums**: a store, a
    checkpoint or a backup that an operator was handed or copied, in which
    every frame's checksum is right and the content is hostile. A checksum
    says nothing here: everything a decoder returns after it is
    attacker-chosen within what the decoder validates, and everything that
    relates one page or file to another — a page id, a link, a chain, a
    manifest's list, a directory record, a value pointer, a name in the
    directory — is the attacker's. This attacker needs the node's files,
    or an operator who opens a directory it supplied.
  - **A write that the system lost.** The operating system writes a
    file's changes to storage in its own time, and that write-back can
    fail with no call reporting it (`sync-handles.md`, section 1). A
    recovery and a backup read the end of the log, and of every file the
    manifest names open, through new handles: bytes that no sync covered
    (`sync-handles.md`, section 6). After such a failure they can be what
    the disk held before. To the engine these are bytes of the two kinds
    above. An item of the log that is not whole ends the log there, and an
    older item under a right checksum is what the campaign of section 3.3
    makes when it writes one frame over another or ends a file at an
    earlier frame. The open then lands on an earlier commit, at or after
    the last one that was made durable; no attacker chooses those bytes.
- **The application's users** supply keys and values of any bytes, of any
  length the public surface accepts, in any number and order, through
  GroveDB's operations.

## 2. What the means already here cover

The engine was not unguarded when this review began. Each means below is
stated with what it covers and what it leaves to this review.

### 2.1 The format decoders are fuzzed (FMT-08)

`lignin-format`'s eleven decoders each ran at least 24 CPU-hours of
libFuzzer with overflow checks on, from valid golden files and from mutated
ones, with no panic, overflow or out-of-bounds read (`fuzz/README.md`,
rule B10 of `format.md`). This review replayed the final corpora of that
campaign under AddressSanitizer and found no error (section 5.2). So a
single decoder of a page, a delta run, a log record, a segment, a
manifest, the value log or the page-table checkpoint did not panic on the
inputs the campaign and this review's replay drove through it (section
5.2) — the evidence of those runs, not a proof for every byte sequence.
What this leaves: what the callers of a decoder do with what it
returns, and the records that `lignin-tree` and `lignindb` decode
themselves, which no fuzz target covers.

### 2.2 The loader validates every field before it is used (STORE-05)

`lignin_store::load` reads a page through its chain and checks, before
anything uses a decoded field: the block's checksum; that the block is of
the kind the chain says; that a compressed payload's stated length is
within a bound and decompresses to exactly it; that the image is a
well-formed page of the store's size and holds the id of the page asked
for; that each delta run is of that page and applies to it; and that a run
is no longer than one page ("What is checked", `load/mod.rs`). A length,
a count or an id that a file states is bounded against the file or a
configured limit before memory is taken for it or a seek is made with it.
The per-crate reading of `lignin-store` for this review (section 4) found
no field a file decides that reaches an allocation, an index or an
arithmetic without such a check, outside the memory observations of
section 6.

### 2.3 The physical invariant checker (VER-07)

`lignindb::verify` reads a version of a store and reports every way its
pages are not what the layout says: an unreadable or misplaced page, a
link to no cell or to a cell of another Merk, a cycle among cells, a hash
that is not its recomputation, a directory record that disagrees with the
cells, a value pointer that leads nowhere. Its walks end on any input: each
page and each cell is entered once, by an explicit stack, and a second
arrival is a finding, not a loop. In a debug build a store is checked when
it is closed. This review uses the checker as the detector of an unsound
store, and confirms (section 3) that every reader and writer of the engine
returns — with an answer or an error — on each store the drill damages in
a way the checker calls unsound, and that none of those stores makes the
engine's code panic. The engine's own debug close deliberately asserts on
such a store (`Db::check_at_close`); the drill opens with that assertion
off, so this is an observation of the rest of the engine's code on the
stores the drill exercised, not a proof over every unsound store.

### 2.4 The crash and disk-full campaigns (VER-03, VER-04, OPS-08)

The store survives a crash at every write boundary and recovers to a
committed version (VER-03, one million crash points; VER-04, through the
whole engine). On a full device the writer fails cleanly and the database
stays readable (OPS-08). These cover the states a crash or a full disk
leaves. They do not cover a file that is changed by a hand, which is this
review's disk attacker.

### 2.5 The unsafe audit (VER-06)

Every LigninDB crate forbids `unsafe` but `lignin-io` and the buffer pool,
which carry Miri and loom coverage (`unsafe-audit.md`). This review read
the buffer pool's use of its unsafe blocks against a file's bytes
(section 4) and found no index, length or frame number that the unsafe
code trusts which a file decides: a page id from a file is hashed to a
bucket and compared, never used as an index, and every copy into a frame
is of a slice the caller made one page long.

**The pool on a target where it maps no memory.** The pool takes a large
arena as a mapping of no file only where `cache/frame.rs` declares the
system's call for one: Linux on x86-64 and on AArch64, and macOS, each with
64-bit pointers. On every other target, a 32-bit one among them, each block
of the pool is the allocator's, and the lengths and offsets of the unsafe
code are of that target's `usize`. The audit's Miri runs are of 64-bit
targets. For this review the pool's tests (`-p lignin-store --lib cache::`)
and the file layer's (`-p lignin-io`) ran under Miri for
`i686-unknown-linux-gnu`, each under Stacked Borrows and under Tree
Borrows:

| Tests | Borrow model | Passed | Failed | Ignored |
| --- | --- | --- | --- | --- |
| the buffer pool | Stacked Borrows | 105 | 0 | 3 |
| the buffer pool | Tree Borrows | 105 | 0 | 3 |
| `lignin-io` | Stacked Borrows | 363 | 0 | 17 |
| `lignin-io` | Tree Borrows | 363 | 0 | 17 |

The ignored tests are those that are marked to be ignored under Miri on
any target (`unsafe-audit.md`, section 3). Of the 112 tests of the pool in
a build for 64-bit Linux, 108 are built for this target: the four that are
not are of the mapping and of the layout of a frame, which are stated for
64-bit Linux alone. So on this 32-bit target Miri found no undefined
behaviour in what those tests execute. What a file, a peer or a user
supplies reaches the unsafe code there by the same calls as on the audited
targets, with the allocator's blocks in the place of a mapping.

### 2.6 The differential fuzzer (VER-01, GROVE-10)

The differential fuzzer applies random operation streams to the LigninDB
engine and the RocksDB engine and compares every answer, cost, root hash
and proof, and what each stores (`test-engines.md`). It is the authority on
whether a user's operations make the two engines answer differently. This
review adds the two surfaces it does not drive: a state-sync peer that
alters a payload (section 3.1), and a store that is changed behind the
engine's back (section 3.2).

## 3. What this review added, and what it shows

### 3.1 A state-sync peer that alters a payload

`grovedb/src/engine/lignin/tests/sync/peers.rs` runs a sync in which one
answer of the source is altered and every other request is served as the
source serves it. The alterations are a peer's: a chunk with a node in
another form or under another value or feature, a value or an element
changed under the hash the chunk states for it, a tree said to have another
root, kind or size, a page of entries with an entry changed, doubled or
dropped, a header with another hash or axis, a part missing or given twice,
a chunk cut short, and the payload changed at random. Each alteration is
placed at the first and the last part of the session it can be made on, and
the random cases at a request a seed chooses.

For every case, on LigninDB, and — where the RocksDB engine is compiled in
— on RocksDB by the same code: no call panics; the session ends within a
bounded number of requests; a session that commits ends on the root hash of
the source with the records of the source and a clean `verify_grovedb`; a
session a refusal abandoned refuses its commit; and where a refusal came
before the altered payload was applied, the test serves the source's own
payload for that request once and the session takes it (resumable), then
drops the session — the database left unchanged where nothing was
committed, or carrying the incomplete-restore marker where the accepted
retry had taken an intermediate commit. The test establishes that the
honest retry is accepted and that the drop leaves one of those two states;
it does not drive such a session on to a finished restore. A session that
asked for nothing more without completing (stopped) is an allowed end: it
makes no final commit, so where it took no intermediate commit the database
matches its prior root and records, and where it did the incomplete-restore
marker survives a reopen. The two engines answer each case alike — the same requests
after the altered one, the same end, the same number of intermediate
commits. On the standard run the cases end as a mix of a committed restore
(an alteration that is an equivalent encoding, which rebuilds the source's
state), a refused payload, a resumable session and a session the altered
request starved. The test asserts a mix so that it cannot pass by refusing
everything.

**The same tests as a campaign.** `LIGNIN_HOSTILE_PEERS=seeds` runs the
cases of the seeds alone: over a range of seeds, until a clock ends the
run, in one commit mode (`LIGNIN_HOSTILE_PEER_MODE`), against the grove
that `LIGNIN_HOSTILE_PEER_SHAPE` names. Shape 0 is the grove of the sync
tests. Every other number is a planned grove: a tree of every kind, an
append-only tree of every kind and an indexed tree of every kind, with
sizes that the number chooses, each among a few:

- 1 to 160 rows in a Merk, with values of 0 to 2,600 bytes;
- trees 1 to 20 deep;
- 0 to 40 values appended to an append-only tree (a dense tree takes the
  31 it has room for);
- 0 to 90 children of an indexed tree beside its child tree.

The making of a planned grove asserts that the grove holds what its plan
says. The case of a seed alters the payload of one request that the seed
chooses, as a whole or in one chunk of one part: a byte set, a bit turned,
a byte put in or taken out, or a few bytes copied from another place. A
case of a seed is held to everything a case of the list is held to, and in
the two-engine test both engines must answer it alike. The list itself is
held to the grove of the sync tests, which takes every alteration; a
planned grove with nothing appended has no entry of a page to change, and
the run says which alterations it did not make. Section 5.4 has the
campaign's counts.

The two inputs a peer can send that MERK-07 named:

- **A chunk that hashes right and is refused at a node** (a node of a
  family foreign to the tree type): the session is marked failed and
  abandoned; `apply_chunk` and `commit_session` refuse from then on, on
  both engines. It does not hang. The alterations "an item as a node of a
  tree with sums" and "a node of a tree with sums as one of a plain tree"
  are this, and end `Failed`.
- **A tree in which one key is linked twice**: the chunk protocol builds a
  Merk from a proof tree, which is a tree by construction (`execute`), and
  a hash node at a chunk boundary is named by its traversal position, so a
  peer cannot make two links of a restored Merk name one node through a
  session. On LigninDB such a tree would be refused at the hand-over
  (`put_staged` takes each node once), and no session reaches it.

### 3.2 Every call on an unsound store returns

`lignindb/src/verify/tests/injected/hostile.rs` takes a store that holds
one of everything the engine keeps, damages it in one way, and runs a
drill over it: the reads of a transaction and its cursors, a commit into
every Merk and keyspace, the drop of every subtree, the reclaiming of what
a drop leaves, write-back, and the checker, before and after each. A panic
raised in the engine's code, a walk that takes more steps than a bound far
above what the store's size calls for, and a call that does not return
within its time are findings; an error is an answer, and the answers need
not be right — the store is damaged, so the drill does not hold a read on a
damaged store to a model.

The damage is of three kinds, each a store a directory with valid checksums
can give the engine: every corruption of the checker's own acceptance list
(a hash changed, a link to no cell, a cell named twice, a value pointer
that leads nowhere, a directory record that disagrees with the cells, and
the rest); a child link made to lead back to its own cell, the cell above
it or the root, and a flat branch made to name itself or one leaf
throughout; and a page changed at random in a way a load still takes. On a
sound store every step returns and the checker calls it sound each time; the
acceptance-list run requires that every corruption made a case the checker
reported unsound, so none was passed over. Over every damaged store no step
panicked or ran past the step bound: each step returned, many with an error
the fixture expected of a damaged store. In a debug build the engine's own
close deliberately panics on a store its checker calls unsound
(`Db::check_at_close`); the drill opens its stores with that close-check
off (`Db::unchecked`), so the panics it forbids are those raised elsewhere
in the engine's code, not that deliberate one. A longer run of the random
case is named by `LIGNIN_HOSTILE_SEEDS`.

### 3.3 A directory damaged under right checksums

Section 3.2 changes a page of a store that is open, behind the checksums.
`lignindb/src/db/tests/damaged.rs` is the disk's second attacker from the
outside: a case takes a database directory that the engine made, changes
one file of it, writes every checksum of that file again (the header's and
each frame's, as the fuzz targets of `lignin-format` do), and gives the
directory to the engine as a node gives it its own. So what was changed
goes through the whole of the load: the manifest, the page table, the
replay of the log, the segments and the value log.

**The directories** are of six shapes:

- Merks that were written, entered and dropped over many commits, one of
  them hosted by a node of another, with flat records beside them;
- flat records over several pages, with long keys and long values;
- Merks whose values are out of line and were rewritten, so that the value
  log holds dead values beside the live ones;
- a checkpoint of a database that went on writing, made behind a table
  checkpoint;
- a database after drops, the reclaiming of at least one dropped Merk in
  full, and passes of the cleaner;
- the files of a session that was not closed, as they were when each of
  its threads had ended its turn.

**The file** is the manifest, a file of the log, a segment, a file of the
value log or the table checkpoint. **The change** is one of nine: a byte or
a bit of a frame's body; a number of one to eight bytes set to a value that
a length, a count, an offset or an id is likely to be wrong at; the bodies
of two frames of one length exchanged; one such body written over the
other; the file ended at an earlier frame; a frame of the file appended
once more; a byte of the file's header; the length of a frame.

**The steps** are what a node does with its directory: the open; every
read of a write transaction and of a snapshot (the entry of each subtree,
its nodes by key and under cursors in both directions, each node's value
and children, the flat records of each column by key and under a cursor);
the checker; a write and its commit; a flush of the log; rounds of the
reclaimer; a pass of the cleaner; the checker again; a checkpoint and a
backup of it; the close, with the check of the store that is then still; a
backup of the closed directory; and a second open. Write-back makes its
turns on its own thread while the session is open.

**A finding** is a panic raised in the engine's code on the thread of the
case; a thread of the session that panicked, which the session says when it
is closed; a walk that takes more than 20,000 steps in a directory of a few
hundred records; or a case that does not end in 120 seconds. A process that
a signal ends (a stack that overflows, memory that is not there) is found
by the script that runs the campaign, from the seed that the process wrote
down before the case. An error is an answer. So is a report of the checker
that the store is unsound, and so is a read that returns what was not
written: the directory is damaged.

**The controls.** Before its cases every process makes the same steps on
the directory of each shape with nothing changed, four times with a write
of its own each, and every step must return; and on a directory whose
manifest ends behind its header, which the open, the backup and the second
open must refuse. A test beside the campaign asserts that the seal leaves
every file of every shape as the engine wrote it and that the frames it
walks end where the file ends. So a change anywhere in a file is under a
right checksum afterwards, unless the change itself ends the walk: a length
that is changed can leave bytes behind the last frame that fits.

**What the campaign does not cover**: two files changed in one directory;
an entry of a directory that is of another type (the tests of VER-10-5 to
VER-10-7 do that); a store of more than a few hundred records; the file
layer that reads with `O_DIRECT`. Section 5.3 has the campaign's counts.

## 4. The per-crate reading

Every non-test file of `lignin-format`, `lignin-io`, `lignin-store`,
`lignin-tree`, `lignindb`, `lignin-tools`, `merk/src/store/lignin` and
`grovedb/src/engine/lignin`, and the state-sync code of
`grovedb/src/replication`, was read for the sites a decoded or supplied
value reaches: a panic (`unwrap`, `expect`, indexing, a slice, an `as`
cast of a decoded length, arithmetic that can overflow), an allocation or a
read sized by a stored field, a walk or a recursion that a cycle or a
chosen depth does not end, and `unsafe` whose argument a file decides. The
findings of that reading are in the table of section 7. Where a site looked
dangerous and a check stands before it, the check is named in the table's
"what happens" column so that it is not re-traced.

The shape of what the reading found: the decoders and the loader are
guarded (sections 2.1, 2.2); descents in the tree layer and walks in the
checker end by a level or a visited-set; the places that do not end, or
that grow without a bound, are reached only through a directory made by
hand (the disk's second attacker), and are in the table. No site was found
in those files where a state-sync peer or an application's user reaches a
panic, undefined behaviour, or an unsound store that is read as sound.
Three rows of the table are of what an application's operations reach, and
none is of that kind. One is the stack that Merk's own batch code takes
for a run of adjacent removals (VER-10-14): it is GroveDB's code on both
engines, outside the files read here, and was found by another task.
Another is the values that a small write carries (VER-10-15), and the
third the copies of large values that the background threads write and do
not install (VER-10-16); both were found by measuring. Memory is
treated in section 6: an operation is bounded by the sizes the application
supplies and the values the store already holds, and a valid commit's own
content decides an open's or a recovery's memory — no bounded input was
found that makes unbounded memory.

## 5. The campaigns

### 5.1 Where they ran

The format build, the GroveDB test library and the engine tests do not
build under this repository's memory constraints on the shared box, so they
were built and run on short-lived cloud sandboxes (8 virtual CPUs, 16 GiB)
and on a droplet with 16 dedicated virtual CPUs. The sandbox-hours and
droplet-hours are in the report. No wall-clock time is stated as a result
here; a campaign's result is pass or fail, the count of its cases, and the
CPU time of its processes (user and system time, as `time` reports them
for each process, in hours).

### 5.2 The decoder corpora under AddressSanitizer

The eleven FMT-08 decoder corpora (the campaign's final corpora, together
with the seed corpora built from the golden files) were replayed under
AddressSanitizer, built with `cargo fuzz build -s address
--debug-assertions`. Every file of every corpus was executed with no
AddressSanitizer error, no panic and no overflow. This covers the
transitive code the decoders reach — `crc32c` and `lz4_flex`, which
FMT-08's own campaign ran without a sanitizer — on the inputs the corpora
hold. `lignin-format`, `crc32c` and `lz4_flex` (built with `safe-decode`,
`safe-encode` and `checked-decode`) carry no `unsafe` the replay reached
that AddressSanitizer flagged. A replay establishes that the decoders did
not fault on the bytes the corpora hold, together with FMT-08's 24
CPU-hours per target; it is not a proof that a decoder cannot fault on some
byte sequence no run reached. A checksum a page passes says only that the
bytes agree with the checksum stored beside it — not who wrote them (an
accidental collision is possible), and a page made by hand carries a
checksum made to match.

### 5.3 Directories damaged under right checksums

The campaign of section 3.3 is the test
`the_engine_answers_on_directories_damaged_under_right_checksums` with a
range of seeds and a clock (`LIGNIN_DAMAGE_SEEDS`, `LIGNIN_DAMAGE_SECONDS`).
A seed is one case: its number modulo six is the shape, and it draws the
file and the change. Each run made the six directories once
(`LIGNIN_DAMAGE_IMAGES`) and gave them to eight processes, each with seeds
of its own. A release build is what a node runs; the debug build has the
overflow checks and the debug assertions of the engine, its close check
left out for these stores (section 3.2).

Three runs were made. The third is of the cases as they are in the tree:
what was changed in the harness after it is how a kept directory is looked
at and written, and comments. The second is of that harness but for one of
its directories: the checkpoint was made with no table checkpoint before
it, so its open replays the whole log, and in the third run it is made
behind a table checkpoint, as it is now. The first is of the harness before
five more things in it were changed, which are said below the table.

**The engine of the counted cases** is the one of the integration branch at
its merge of OPS-03's second unit (`d3f79b521`) for the second and third
runs, and at an earlier merge (`f22f6c7d1`) for the first; between the two
the engine's crates differ in `lignindb` alone, in the code of a
transaction, an error and an option. This branch was rebased after the
runs, onto a base that changes more of the engine: how write-back places
values and how a commit is told of it, the batch and the write set of a
transaction, and in `lignin-store/src/wal` how the log's next file is made
where its name is taken. That last is a state these directories hold: a
manifest that is ended at an earlier frame names fewer files of the log
than the directory has.

| Run | Build | Processes | Seeds from | Cases | CPU-hours | Findings |
| --- | --- | --- | --- | --- | --- | --- |
| third | release | 8 | 6,000,000,000,000 | 5,244,272 | 6.7 | 0 |
| second | release | 8 | 4,000,000,000,000 | 8,377,781 | 13.2 | 0 |
| second | debug | 8 | 5,000,000,000,000 | 1,180,682 | 15.2 | 0 |
| first | release | 8 | 1,000,000,000,000 | 11,331,623 | 15.9 | 0 |
| first | release | 8 | 2,000,000,000,000 | 7,859,442 | 16.4 | 0 |
| first | debug | 8 | 3,000,000,000,000 | 2,037,281 | 15.9 | 0 |

The process of worker `w` of a run takes the seeds from the run's first
seed plus `w` times 1,000,000,000 on, in order. No process of any run was
ended by a signal, and each exited with no error.

**Why that much.** FMT-08's bar for one decoder was 24 CPU-hours. The first
run is two of those bars: one for the release build and one shared between
a second release run on other directories and the debug build. The second
and third runs are 35.1 CPU-hours more, on the harness as it is. A case
takes 5 to 8 milliseconds of CPU in a release build and 28 to 46 in a debug
build. The smallest of the six runs, the second in a debug build, has
196,779 cases of the shape it made least, 156,651 of the kind of file (the
table checkpoint), and 25,442 of the change (two frames exchanged).

**What the cases of the second and third runs came to**, together:

14,802,735 cases, by the step and what it answered. A step that an
earlier answer rules out is not made: a directory that no open takes has
no read, and its backup and its second open are still made.

| Step | Returned | Every other answer, with its count |
| --- | --- | --- |
| open | 7,261,667 | error Recovery 5,516,863; error Manifest 1,716,460; error OtherEngine 224,107; error FormatVersion 56,643; error Catalog 26,375; error PageSize 620 |
| reads | 7,261,667 | none |
| snapshot reads | 7,261,667 | none |
| check | 6,516,067 | error unsound 745,600 |
| write | 6,975,360 | a fixture's panic where its batch was refused 205,494; a fixture's panic where its read of a node was refused 76,653; error Commit 4,160 |
| flush | 7,261,667 | none |
| reclaim | 7,261,667 | none |
| clean | 6,818,205 | error Values 348,206; error Block 72,892; error Payload 21,332; error Flush 1,032 |
| check after the write | 6,494,880 | error unsound 766,787 |
| checkpoint | 7,261,667 | none |
| backup of a checkpoint | 7,261,667 | none |
| close | 6,273,312 | error unsound 988,355 |
| backup | 9,852,835 | error NotADatabase 1,997,210; error Short 1,763,659; error Log 1,189,031 |
| open again | 6,273,313 | error Recovery 5,516,863; error Manifest 1,716,460; error unsound 988,352; error OtherEngine 224,107; error FormatVersion 56,643; error Catalog 26,377; error PageSize 620 |

- **An open that takes the directory is half the cases**, and of those one
  in ten is called unsound by the first check: what was changed was under
  a right checksum and the load took it.
- **The two counts of a fixture's panic are not the engine's.** The write
  of a case is the batch fixture of the backup's and the transactions'
  tests, which panics where the engine refuses its batch, or its read of a
  node before the write, with an error. The engine returned an error
  there. A panic is the engine's when it is raised in a file that is no
  test's, and no case had one.
- **The close calls more stores unsound than the checks before it.** The
  check of a session reads a version of the store; the close checks the
  store that is then still, with what it keeps beside its pages.
- **In three cases the close called the store unsound and the second open
  did not**: two were then refused at the open, by the catalog, and one
  opened and was checked sound. The first run has no such case.

**What changed between the first run and the second**, and what it means
for the first:

- A thread of a session that panicked between two steps of a case was
  counted as an error of the close and not as a finding. No case of the
  first run has that answer of a close in its counts, so the first run hid
  none.
- The directory of the session that was not closed was read while the
  session's threads wrote, so its files could be of two moments. The cases
  of that shape in the first run are of a directory under right checksums
  that the engine may never have had on disk at one time.
- The directory after drops was taken to be cleaned when a count of the
  cleaner was above zero, which it could be before the drops. From the
  second run on, the making asserts that a dropped Merk was reclaimed in
  full behind the drops.
- The first run's counts tell a fixture's panic without its place. From
  the second run on each is counted under the line that raised it.
- The controls of section 3.3 are of the second and third runs only.

### 5.4 A state-sync peer that alters a payload

The campaign of section 3.1 ran on the droplet in slices: one process of
one of the two tests, on the grove of one shape, in one commit mode, over
seeds of its own, ended by a clock after 300 seconds. Sixteen workers ran
slices for two hours. Three workers in four ran the two-engine test, which
syncs from RocksDB to RocksDB and to LigninDB and compares the answers; the
fourth ran the LigninDB test. Ten workers committed in parts and six once:
of the twelve of the two-engine test, eight in parts and four once.

In the second run the shape of slice `n` of worker `w` is `(16 n + w)`
modulo 49: the grove of the sync tests and the 48 planned groves, each
worker going through them in an order of its own. Each of the 49 groves
was run by seven or eight slices, in both commit modes, and by the
two-engine test in both: 2,169 to 8,943 cases a grove.

| Run | Slices | Passed | Cases | CPU-hours |
| --- | --- | --- | --- | --- |
| second | 384 | 384 | 247,495 | 29.1 |
| first | 384 | 384 | not kept | 28.7 |

Of the 247,495 cases of the second run, 145,027 ran on both engines (the
two-engine test) and 102,468 on LigninDB alone. No case broke a rule, and
no case of the two-engine test was answered differently by the two
engines, in the end, in the requests after the altered one or in the
number of intermediate commits. How the sessions ended:

| End of the session | Cases |
| --- | --- |
| a payload was refused after its application began, and the session failed | 209,016 |
| a payload was refused before its application began, and the session took the source's own payload for the request | 35,574 |
| the session asked for what the source does not serve | 1,521 |
| the session committed the source's state (the alteration was an equivalent encoding) | 906 |
| every request was answered and the commit was refused | 478 |

What was changed in the harness after the second run changes no case of
it: how the seeds of a shard are stepped to, which the campaign does not
use, and a comparison in the making of a planned grove that is gone. The
engine is the one of the integration branch at `d3f79b521`, as for the
damaged directories.

The first run is weaker in three ways, and the second is the one to read.
Its harness was the one before the planned groves asserted what they hold:
a grove could hold less than its plan (a kind of tree left out, a tree with
fewer appended values) with nothing said. Its shapes were taken modulo 48,
which with sixteen workers gave each worker three groves only, so a grove
was run by one worker, in one commit mode, and twelve of the 48 by the
LigninDB test alone. And the log lines with the counts of its cases were
not kept. Its slices all passed, each asserting that no case broke a rule
and, in the two-engine test, that the engines answered every case alike.

## 6. The memory observations

The second question — adversarial keys and values cannot exhaust memory —
is read as: no operation holds memory that is not bounded by the sizes the
application supplies and the store already holds, and there is no path
where a bounded input makes unbounded memory. A node key is capped at 255
bytes and a value at `MAX_VALUE_LEN` (a format limit, `u32`, not a
peak-memory budget), a long flat key is held in a nested tree whose memory
is the key's own length, and a transaction's write set is the writes the
application made, as on the RocksDB engine.

### 6.1 What was measured, and how

`grovedb/src/engine/lignin/tests/measured.rs` makes one GroveDB operation
at the largest input of each kind and prints how far the resident set of
the process grew from just before the operation to its peak during it
(`VmHWM` of `/proc/self/status`, reset before the operation). The figures
are of Linux on x86-64, from a debug build of GroveDB's test library, one
case in a process of its own, with the allocator set to take every block of
16 KiB or more from the system (`MALLOC_MMAP_THRESHOLD_=16384`), so that
memory an earlier operation freed does not hide what a later one takes.
Each process ran in a scope with a limit of 8 GiB and no swap, with its
database on a disk. The same test ran on both engines; RocksDB's own
allocations are in its figures.

GroveDB itself caps a key and a segment of a path at 255 bytes. It caps
neither the size of a value, nor how deep trees are in trees, nor how many
operations a batch holds: those are the application's to cap. So the rows
below are at the cap for a key, and at sizes chosen to show the line for
the rest.

### 6.2 One operation at its largest input

Growth of the resident set, in MiB, LigninDB / RocksDB:

| Operation | put | get | replace | delete |
| --- | --- | --- | --- | --- |
| an item under a key of 255 bytes, below 8 path segments of 255 bytes | 0.8 / 1.1 | 0.4 / 0.9 | | |
| an item of 20 KiB | 0.4 / 0.7 | 0.7 / 0.6 | 0.4 / 0.6 | 0.9 / 1.3 |
| an item of 1 MiB | 4.3 / 4.3 | 3.1 / 2.8 | 7.2 / 7.1 | 3.5 / 4.5 |
| an item of 16 MiB | 64.2 / 64.3 | 56.5 / 47.8 | 112.1 / 112.0 | 48.3 / 64.4 |
| an item of 64 MiB | 256.2 / 256.2 | 191.8 / 191.6 | 448.1 / 447.8 | 256.1 / 256.2 |
| an item below 16 trees in trees | 0.8 / 1.1 | 0.6 / 0.8 | | |
| an item below 64 trees in trees | 0.6 / 1.2 | 0.5 / 0.7 | | |
| an item below 256 trees in trees | 0.9 / 1.1 | 0.8 / 0.8 | | |

| One batch of items of 100 bytes under keys of 32 | LigninDB | RocksDB |
| --- | --- | --- |
| 1,000 items | 18.3 | 23.9 |
| 10,000 items | 51.1 | 45.7 |
| 100,000 items | 380.6 | 299.1 |

What the rows say:

- **A value costs a small multiple of itself, the same on both engines.** A
  put holds about four times the value and a replace seven. The copies are
  GroveDB's and Merk's (the element, its serialized form, the node that
  holds it), not the engine's.
- **Depth costs nothing that grows**: under a mebibyte at 256 trees.
- **A batch costs 3 to 4 KiB an operation on both engines** at 100,000
  operations: 3.9 KiB on LigninDB and 3.1 KiB on RocksDB.

Each of these is memory in proportion to what the user submits in the
operation, which the application's costs count. None is a finding.

### 6.3 A small write beside large values

**One operation can hold more than the bytes it writes.** A value above the
inline threshold of 256 bytes is stored out of line, and its cell holds the
place of the value. When a write fills a page of a Merk, cells leave for
another page, and each cell that ends in another page takes its value with
it (`tree-layout.md`, X1): the commit reads each such value whole, holds
them together, and appends them to its log group again (`log-format.md`,
LG23). So one write of a few bytes can read, hold and log the values of
every cell it carries. The rules that move cells never look at whether a
value is out of line (`tree-layout.md`, X5).

The test makes this on purpose: `count` items of one size in one tree, and
then 256 items of 8 bytes put one at a time between them. For each put it
records the growth of the resident set and the bytes that the commit
appended to the log, which is the engine's own count on LigninDB
(`CommitStats::group_bytes`). RocksDB does not give such a count to the
test, so for both engines the bytes that the process gave the system to
write during the put are recorded too (`wchar` of `/proc/self/io`).

| Large items | The put that grew most, LigninDB | Its log bytes | A put at the median, LigninDB | The put that grew most, RocksDB | A put at the median, RocksDB |
| --- | --- | --- | --- | --- | --- |
| 200 of 64 KiB | 16.4 MiB | 8.0 MiB (127 values) | 0.9 MiB, 520 bytes logged | 1.9 MiB | 1.1 MiB, 0.5 MiB written |
| 200 of 256 KiB | 63.9 MiB | 31.8 MiB (127 values) | 3.9 MiB, 520 bytes logged | 8.3 MiB | 6.0 MiB, 2.0 MiB written |
| 200 of 1 MiB | 254.9 MiB | 127.0 MiB (127 values) | 16.3 MiB, 520 bytes logged | 32.7 MiB | 24.4 MiB, 8.0 MiB written |
| 48 of 16 MiB | 959.6 MiB | 496.0 MiB (31 values) | 207.3 MiB, 498 bytes logged | 320.1 MiB | 303.4 MiB, 96.0 MiB written |

What the rows say:

- **The bound is a line.** The commit that splits the page appends the
  values of 127 cells to the log, and the process holds about twice their
  bytes while it does. That is about all the cells that a page of 32 KiB
  holds (the measurement at the engine's own surface, `carried.rs`, counts
  up to 130 values in one commit); with longer keys a page holds fewer.
  In the last row the tree has 48 large items, and the commit that grew
  most carries 31 of them.
- **Every other put is the Merk's path**, on both engines: Merk reads the
  nodes from the root to the key with their values. RocksDB then writes
  each of them again, value and all, at every put (the median of bytes
  written is eight values). LigninDB writes 520 bytes at the median, since a
  hash or a link that changes leaves a value where it is (`tree-layout.md`,
  X2).
- **So the engines differ in when, not only in how much.** Over the 256
  puts of the first three rows LigninDB appends 10.2, 40.4 and 161.2 MiB to
  its log, of which the one commit that splits the page is 8.0, 31.8 and
  127.0. In one commit LigninDB writes and holds far more than RocksDB ever
  does: 127 values against eight. Over all the puts the RocksDB process
  gives the system more to write: 124.8, 498.1 and 1,991.0 MiB, against
  33.5, 158.1 and 1,272.6 MiB from the LigninDB process. Most of what the
  LigninDB process writes there is not its log. It is what its background
  threads write, which is below.

`tree-layout.md` gives the rate over a workload: 0.8 values appended again
for each value written on a tree that grows by random keys, and about a
thirtieth of that on a tree that does not grow (X4). On its reference
workload that is 368 bytes appended again on top of the 459 a transition
writes (section 14.5); PERF-04's row `bytes_logged_per_transition`
(`benchmarks.md`) counts such bytes with everything else a transition
appends to the log. A transition of that workload carries no value of the
sizes above.

A put and a delete that follow each other do not split and merge a whole
page in turn: a page gives a fragment away again only when a delete takes
it below 35% of its capacity (M2). The moves of a small fragment, one below
a sixteenth of a page, to its parent's page and out of it again (M1, V1)
carry at most the few cells of such a fragment. What a sequence of writes
that is made to repeat those moves carries was not measured.

**What the background threads write.** The case of 200 items of 1 MiB ran
once more under `strace`, which names the file and the thread of every
write. The values in the tree are 200 MiB. The log took 200.1 MiB while the
large items were put and 161.2 MiB during the 256 small puts. The value log
took 1,648.0 MiB and 1,215.0 MiB, 2.8 GiB in all, in records of one value
each:

| Thread | Value records it wrote while the 200 large items were put | Its counts for that time | Value records it wrote during the 256 small puts | Its counts for that time |
| --- | --- | --- | --- | --- |
| write-back | 1,539 | 199 values appended, none regrouped | 562 | 162 appended, none regrouped |
| the cleaner | 109 | 93 copied: 15 moved, 78 left | 653 | 665 copied: 159 moved, 506 left |

The counts were read when the small puts began and when they ended, and the
cleaner counts a pass when it ends, so its counts and its writes differ by
the pass that was under way.

- **Write-back's records beyond the values it appends are the copies of its
  regrouping** (`lignin-tree`, `value/regroup.rs`). Before write-back
  writes a new base image of a page whose values lie apart in the value
  log, it appends every value of the page again, in the order of a scan,
  and then gives each cell the place of its copy in a page step. A step is
  refused where a commit has superseded the page since its values were
  copied, and is not made where the cell no longer holds the value. The
  copy is then dead space in the value log. `values_regrouped` counts the
  values whose cells took their copies. No count has the bytes of the
  other copies, `flush_bytes` neither: in this run they are 1,740 copies,
  1.7 GiB, and not one copy was taken.
- **The cleaner's copies are counted.** It copies the live values of the
  value-log file with the most dead space and installs them the same way.
  Three of its four copies were left (`values_left`).

A page's values bound the copies of one regrouping, and a regrouping is
made where write-back writes a base image of the page, so this is work on
the disk in proportion to the values of a page for each time the page is
written back. No commit waits for a copy as such; a commit that waits for
room in the log waits for write-back's pass, of which the copies are a
part.

How much of it there is goes with the size of the values. In runs of the
measurement that printed the engine's counts before and after the 256
small puts, the process wrote 33.1 MiB with items of 64 KiB, and its log
and the bytes its counts show are all of that. With items of 1 MiB it
wrote 1,470.8 MiB, of which 910.7 are counted. With 48 items of 16 MiB it
wrote 18.5 GiB, of which 3.7 are counted. For the same 256 puts the
RocksDB process wrote 124.8 MiB, 1,991.0 MiB and 22.7 GiB, more each time.
What the RocksDB process writes while the large items themselves are put
was not measured. This is row VER-10-16 of section 7.

**At a memory limit.** The case of 200 items of 1 MiB ran once more in a
scope whose memory limit was 128 MiB above what the process held before the
put that carries 127 values, with no swap. The commit did not fail with an
error. The kernel killed the process while that put ran (the out-of-memory
kill of the memory cgroup, signal 9): a limit of that kind refuses no
allocation, so the engine is given no error to return. The directory that
the process left opened by recovery. The physical checker found it sound,
GroveDB's verification found no issue, each of the 200 large items read
whole, and the small items in it were those of the 44 puts before the one
that was killed, and none of that put. So a node that is killed there
loses the write that was under way and nothing that was committed.

**What bounds it** is the cells of one page times the largest value the
application lets a user store, in one commit. GroveDB's costs do not count
it: they are Merk's, and equal on both engines. This is row VER-10-15 of
section 7.

A file's own content can also decide how much memory an open or a recovery
takes: the length of a log group, the spread of the page ids in a table
checkpoint, the number of pages a log names with a value not yet placed
(the table). A large but valid commit reaches the same WAL-group path, so
this is not only a hand-made file. The log reader's window uses
`try_reserve_exact`, so an allocation the allocator refuses there is an
error, not an abort; `install_rebuilt` uses `collect`, `Vec::with_capacity`
and `to_vec`, which abort on an allocation failure rather than return, so
the clean-failure guarantee holds for the log reader's window and not for
the rebuilt-page path. None of these is a panic from a decoded value or
undefined behaviour, and integrity of the node's own files is the checker's
to attest (`audit-non-issues.md`, "Trust-boundary findings").

## 7. Findings

Severity is by who can reach the state from now on, and then by what
happens there.

- **high**: a state-sync peer, by what it sends, or an application's user,
  by an operation the application accepts, reaches a panic or a crash,
  unbounded memory, an unsound store, a wrong answer or undefined
  behaviour.
- **medium**: such an attacker reaches a cost that is bounded and that the
  application's costs do not count (VER-10-15); or a batch that the
  application's own limits keep a user from making, and that its own code
  could make, reaches a crash (VER-10-14); or a directory made by hand
  reaches harm outside itself (VER-10-5).
- **low**: the state needs a caller to do what application code cannot, or
  write access to a node's files (a directory made by hand with valid
  checksums, which the attacker puts in the node's place or hands to an
  operator), or it is a debug-only assertion, or a cost in proportion to
  the node's own data, or work of the background threads on the disk that
  is bounded and, where it was measured, below what the RocksDB engine
  writes for the same operations (VER-10-16).

Status: **fixed** names the commit by its subject and the test that fails
without it; **open (owner)** is sent to the owner because the fix is in
another unit's file or is a design choice; **open (upstream)** is in
GroveDB's own code, the same on both engines; **accepted** is left as it is
with the reason.

No finding is of high severity. Two of medium severity are open: VER-10-14,
which is upstream's, and VER-10-15, whose bound is stated and whose
follow-up is named in its row. Of the rows of low severity, three are open
with the owner: VER-10-2, VER-10-3 and VER-10-16.

| id | severity | where | an attacker supplies | what happens | status |
| --- | --- | --- | --- | --- | --- |
| VER-10-1 | low | `lignin-tree` `directory/rekey.rs`, `plan_rekey` | a directory with a rootless Merk whose child record names itself, or two rootless Merks that host each other | a move's walk over the Merks below the moved one reads the cycle's records again without end and grows its plan without bound (a hang, then out of memory) | **fixed** ("fix(lignin-tree): a rekey walk ends on a directory that cycles"): the walk refuses a Merk reached a second time, which `Directory::check` already calls damage. A test plants the cycle and shows the walk does not end before the fix |
| VER-10-2 | low | `lignin-tree` `flat/cursor.rs`, `next_leaf`/`down` | a directory in which two slots of a flat branch, or two buckets of a leaf, name one lower page (a DAG with valid checksums) | a scan of that keyspace visits the shared pages once per path to them; the work is the product of the fan-outs, which an acyclic shared-page graph of a few pages already drives past any practical deadline (with an empty leaf below, a single `seek`/`next` call). Reached at open (`Directory::load` scans the dropped records) and in any scan of the keyspace. The checker is not affected: it walks pages directly and reports the page named twice | **open (owner)**: the fix is a bound on a scan in the flat cursor, a hot read path and a design choice |
| VER-10-3 | low | `lignin-store` `table/checkpoint.rs`, `table/chains.rs` | a table checkpoint whose links name page ids far apart (one per 4096-id chunk) | the page table allocates a chunk of 4096 node slots (112 KiB) for the first id of each 4096-id range a link falls in, and the directory of chunk pointers grows with the largest id, so a checkpoint of N spread links takes about N × 112 KiB at open; the allocator's map grows the same way | **open (owner)**: inherent to the chunked page table; a bound belongs with STORE-03 / the page-table format. Reads the node's own checkpoint file |
| VER-10-4 | low | `lignin-store` `recovery/replay.rs`, `wal/reader.rs` | a log whose groups create many pages that carry a value not yet placed, or one group whose stated length is the file's (a valid large commit reaches the same path) | recovery holds every page with a pending value to the end of the log, and the log reader holds one whole commit group; the memory of an open is the log's own content, bounded by no option | **accepted**: a recovery reads the node's own files, and a valid large commit reaches the same WAL-group path, so the cost is application-reachable, not only a hand-made file (section 6). The log reader's window reserves through `try_reserve_exact` and fails as an error where the allocator refuses; the rebuilt-page path (`install_rebuilt`: `collect`, `with_capacity`, `to_vec`) aborts rather than returns. A memory cost, not a panic from a decoded value. Noted for the open's memory bound |
| VER-10-5 | medium | `lignindb` `db/open.rs`; `lignin-store` `segment/store.rs` (`remove_unnamed`), `wal/store.rs`, `values/store.rs`, `table/checkpoint.rs` | a store directory in which the `seg`, `wal`, `val` or `table` subdirectory is a symbolic link to another database's directory, under a manifest that names no plain file there | before the fix, the open (`SegmentStore::open` and its peers, reached by `recover`) listed that directory through the link and removed the files in it that the manifest does not name, deleting the files of the directory the link leads to: data destruction outside the supplied directory. A file with an id above the manifest's last id was protected by a length or content check first; files at or below it were removed with no check | **fixed** ("fix(lignindb): refuse a store subdirectory that is not a directory"): the open's look at the directory, before the lock and again under it, refuses an entry named `seg`, `wal`, `val` or `table` that the listing has as anything but a directory, with `OpenError::StoreDirNotADirectory`, before a store opens it or removes within it. A listing follows no link. Test `a_store_subdirectory_that_is_a_link_is_refused_and_not_followed`: before the fix the open is taken. As for every look at a directory by its path, an entry that is exchanged between the look and the store's own listing is not covered (`lignindb::db`, "Where the guarantee ends") |
| VER-10-6 | low | `lignindb` `backup/mod.rs` (`plan`, `copy`) | a backup's source directory in which a file the manifest names, the lock file, the manifest or the store subdirectory of a named file is a symbolic link | before the fix, the backup opened each file the source's manifest names by its path and copied its bytes, following a link, so a backup of a supplied source could carry bytes from outside the source (a confidentiality reach) | **fixed** ("fix(lignindb): a backup reads no entry that is not a regular file"): each entry the copy reads is looked at first in the listing of its directory, and a lock file, a manifest or a named file that is listed as anything but a regular file, and the store subdirectory of a named file that is listed as anything but a directory, end the copy with `BackupError::NotRegular` before anything is made. Test `a_source_with_a_link_or_a_pipe_where_the_copy_reads_is_refused`: before the fix the copy of a source with a linked lock file is made. A sparse file that the manifest names with a large length is still copied dense: the copy is bounded by the manifest's own lengths, and that part is **accepted** |
| VER-10-7 | low | `lignindb` `db/open.rs`, `backup/mod.rs`; `space.rs` | a store directory in which `LOCK`, `MANIFEST`, `RESERVE`, `RESERVE.new`, a file the manifest names or the table checkpoint is a named pipe | before the fix, a call that opened such a path for reading waited on the pipe with no error: the open and a backup read `MANIFEST`, and the stores read the files the manifest names | **fixed** ("fix(lignindb): an open opens no file that is not a regular file", and the backup's fix of VER-10-6): the open refuses each of those entries where its directory lists it as anything but a regular file, with `OpenError::NotARegularFile`, before it opens it: the four of the database directory at the look, and the files the manifest names under the lock. Test `a_pipe_or_a_link_in_the_place_of_a_file_is_refused_and_not_opened`: before the fix the open of a directory whose `MANIFEST` is a pipe does not return. An entry that the manifest does not name is not opened unless the store's own listing has it as a regular file |
| VER-10-8 | low | `lignin-tree` `drop/flat.rs`, `drop/merk.rs`, `drop/step.rs` | a directory with a drop record that names a page or a root cell in use | the reclaimer frees the page and everything its branches and buckets name, or removes the cells the root reaches, with no error: the birth and shape checks it makes are not ownership checks, so data a live tree still names is taken out in the background | **accepted**: whoever writes the drop record has the node's files and could remove the data directly; a `verify` that completes before any background reclaim (not only before an explicit sweep) reports the store as unsound. Noted |
| VER-10-9 | low | `lignin-tools` `dump.rs` (`order`); `lignindb` `db/mod.rs`, `Db::wipe` | a directory whose entries name each other as hosts (a host cycle); with one host per entry the cycle has no hostless top, so it is reached by selecting a member of it, not by `--all` | the dump's walk reached a subtree a second time and panicked on the second `remove`, past the dump's output-file cleanup; `lignin dump --all` and `Db::wipe` leave the cycle's entries out, since a cycle has no top (a wrong answer on a damaged directory) | **fixed** for the panic ("fix(lignin-tools): a dump of a directory whose hosts cycle is refused": the walk returns a `ToolError` on a repeated entry; a test selects a cycle member and shows the refusal, and that `--all` leaves a topless cycle out). **accepted** for the omission and the wipe: a directory an attacker supplied, a wrong answer on it |
| VER-10-10 | low | `lignin-tools` `segments.rs`, `dump.rs` | a segment file the manifest names as longer than it is on disk (a sparse file), or a value record with a large stated length | `lignin segments` reads a segment whole up to its manifest length (it skips a file shorter than its manifest length first), so holes in a file that is logically long enough are read dense; an ordinary segment of that logical length costs the same | **accepted**: a tool run on a directory an attacker supplied; the reads are bounded by the manifest's own lengths. Noted |
| VER-10-11 | low | `lignindb` `verify/mod.rs` | a store with very many values out of line or flat records | the checker holds in memory a record per out-of-line value and the key of every flat record of a rooted subtree's data column, with no cap: its memory is proportional to the store's size | **accepted**: by design — the checker is run on a store to judge it, and the store's own size bounds its memory. Noted so an operator knows a checker's run costs memory in proportion to the store |
| VER-10-12 | low | `lignin-store` `flush/run.rs`, `flush/write_back.rs` | — (not an attacker's input) | a panic in a write-back turn leaves the `running` flag set, so a committer that later waits for room never returns; no file byte was found that makes the turn panic | **accepted**: not reached by any input this review found; a robustness note that a background panic should end the committer's wait rather than hang. Noted |
| VER-10-13 | low | `lignin-tree` `bulk/load.rs` (`Loader::node`), driven by `lignindb` `txn/frames.rs` (a whole-Merk load) | a node feed whose keys are out of order or whose link heights are wrong | the loader stores the heights as stated and does not check its keys' order; a feed that is out of order would commit a store the checker then flags (`KeyOrder`, `LinkHeights`) | **accepted**: an internal invariant, not an attacker-reachable feed at this head. Its callers are the write transaction's whole-Merk load (`txn/frames.rs`), fed the nodes of a change set the Merk layer built in traversal order, and the state-sync restore, which hands a hash-verified proof tree; neither drives attacker-chosen order or heights. `lignin load` does not reach it (it writes node by node through `scope.apply`, not the loader). Becomes a finding if a future caller drives the loader from an order it has not verified |
| VER-10-14 | medium | `merk` `tree/ops.rs`, `Walker::apply_sorted`: GroveDB's Merk, the same code on both engines | a batch that removes a run of adjacent keys of one Merk | on a removal the node is replaced by its neighbour in key order from the taller child, and `apply_sorted` is called again on the same subtree for what is left of the batch there: one call nested in another for each removal that follows another. The nesting is as deep as the run of removals, up to about a quarter of the Merk's keys, where the run begins at the key of a subtree's root; a batch that removes every key, or a run from the lowest key, does not nest. Measured in a release build of upstream's `develop`, with RocksDB (the function is the same file on this branch): 2.22 KiB of stack for each nested call, so 915 adjacent removals overflow a thread's stack of 2 MiB and 3,723 one of 8 MiB, and the process aborts. A measurement of this on glibc has to turn the cache of thread stacks off (`GLIBC_TUNABLES=glibc.pthread.stack_cache_size=0`): a thread that asks for 2 MiB in a process that has had larger threads can be given a cached stack of 8 MiB, and then survives a depth that overflows 2 MiB | **open (upstream)**: not fixed on this branch, since it is Merk's own code and no part of the engine. What bounds it is the application's own limits on the batches it makes. A test that shows it against `develop` is kept for the report to upstream |
| VER-10-15 | medium | `lignin-tree` (the move rules of `tree-layout.md`, X1), `lignindb` `commit` (the values of a log group, `log-format.md` LG23) | values above the inline threshold of 256 bytes in one tree, and then a small write into the same tree | when the small write fills a page of the Merk, cells leave for another page, and each takes its out-of-line value with it: the commit reads each such value whole, holds them together, and appends them to its log group. Measured through GroveDB (section 6.3): one put of an item of 8 bytes appends the values of 127 cells to the log and holds twice their bytes, which with 200 items of 64 KiB, 256 KiB and 1 MiB in the tree is 8.0, 31.8 and 127.0 MiB appended and 16.4, 63.9 and 254.9 MiB held. On RocksDB the same put takes the Merk's path only (1.9, 8.3 and 32.7 MiB held). GroveDB's costs do not count it. Under a memory limit below what the commit holds, the commit does not fail with an error: the kernel kills the process, and the directory it leaves recovers sound at the commit before | **open (owner)**: not fixed on this branch; it is the format's rule for a cell that moves. What bounds it is the cells of one page times the application's own limit on the size of a value, in one commit, and until the follow-up that limit is a condition of deployment. The follow-up that is named: the commit appends the carried values one at a time and does not hold them together, which changes memory and neither the format, the layout nor an answer. A split that chose its part by the bytes of the values it would carry is possible too (`tree-layout.md`, X5), and is a change of the layout |
| VER-10-16 | low | `lignin-store` `flush/write_back.rs` (`regroup`), `cleaner/values.rs`; `lignin-tree` `value/regroup.rs`, `value/owners.rs` (`copy_grouped`, `install_grouped`) | values above the inline threshold of 256 bytes in one tree, and writes into that tree | before write-back writes a new base image of a page whose values lie apart in the value log, it appends every value of the page again and then gives each cell the place of its copy. A copy whose page a commit has superseded since, or whose cell no longer holds the value, is not taken and is dead space. The cleaner's copies of a value-log file's live values are left the same way. Measured through GroveDB (section 6.3): while 200 items of 1 MiB and then 256 items of 8 bytes were put into one tree, the process wrote 2.8 GiB to the value log for 200 MiB of values. Write-back wrote 2,101 value records where its counts show 361 values appended and none regrouped, and the cleaner left 584 of the 758 copies it counts. `FlushStats::values_regrouped` counts the copies that were taken; no count, `StoreStats::flush_bytes` neither, has the bytes of the others. With items of 16 MiB the process wrote 18.5 GiB during the 256 small puts, of which its log and its counts show 3.7 | **open (owner)**: not fixed on this branch; write-back and the regrouping are other units' code. Work of the background threads on the disk, bounded by the values of a page for each time write-back writes the page, and during the same small puts below what the RocksDB engine's process writes. Whoever reads the bytes a workload writes from the engine's counts reads too few where values are large. What a fix would look at: a regrouping that is put off while commits supersede its page, and a count of the bytes that were copied and not taken |

## 8. What is open or accepted, and what would make it a finding of higher severity

Every row marked **accepted** or **open (owner)** is a state that a
directory made by hand reaches, with the node's files or an operator who
opens a supplied directory, except these:

- VER-10-4 and VER-10-11: a memory cost in proportion to the node's own
  data (the log groups of a valid commit, the store's own size), and not
  unbounded memory from a bounded input.
- VER-10-12: a note on robustness, reached by no input this review found.
- VER-10-13: an invariant of the loader whose callers at this head (the
  write transaction's whole-Merk load, the verified restore) never drive it
  out of order.
- VER-10-14 and VER-10-15: the two rows of medium severity that an
  application's operations reach. The first is upstream's and is bounded
  by the application's own limits on the batches it makes. The second is
  bounded by the cells of a page times the application's own limit on the
  size of a value.
- VER-10-16: work of the background threads that an application's
  operations bring about, bounded by the values of a page for each time
  the page is written back.

None is a panic, an unsound store read as sound, or unbounded memory that a
state-sync peer reaches, or that an application's user reaches through an
operation within the application's own limits. The three rows that a
directory with a link or a named pipe in it reached (VER-10-5, VER-10-6,
VER-10-7) are fixed, and so are the two walks that a directory with a cycle
in it did not end or panicked on (VER-10-1, VER-10-9).

They **become a finding of higher severity if**:

- an application's user or a state-sync peer can bring the engine into one
  of the states that a directory made by hand reaches: in particular, if a
  public operation comes to write a directory record that cycles (VER-10-1,
  VER-10-9) or a drop record that names a live page (VER-10-8) without the
  write building it from a sound state;
- a path that the engine opens comes to be built from bytes that a peer or
  a user supplies, and not from the operator's own directory (VER-10-5,
  VER-10-6, VER-10-7);
- an application lets a user make a batch that removes a run of adjacent
  keys longer than a thread's stack takes (VER-10-14): that is a crash a
  user reaches;
- an application lets a user store values whose size, times the cells of a
  page and twice over, is memory that a node does not have (VER-10-15);
- a workload is found in which the copies that write-back makes and does
  not install are more than the RocksDB engine writes for the same
  operations, or in which a commit's wait for room in the log grows with
  them (VER-10-16).

## 9. Code and tests

- `grovedb/src/engine/lignin/tests/sync/peers.rs`: the state-sync peer
  (sections 3.1 and 5.4). Runs on both engines where the RocksDB engine is
  compiled in. `LIGNIN_HOSTILE_PEERS=wide` makes every alteration at every
  part and adds a session that commits in parts; `LIGNIN_HOSTILE_PEERS=seeds`
  runs the seeds alone, with `LIGNIN_HOSTILE_PEER_SEEDS`,
  `LIGNIN_HOSTILE_PEER_SHAPE`, `LIGNIN_HOSTILE_PEER_MODE`,
  `LIGNIN_HOSTILE_PEER_SECONDS` and `LIGNIN_HOSTILE_PEER_SHARD`.
- `lignindb/src/verify/tests/injected/hostile.rs`: every call on an
  unsound store (section 3.2). `LIGNIN_HOSTILE_SEEDS` sets the range of the
  random case.
- `lignindb/src/db/tests/damaged.rs`: directories damaged under right
  checksums (sections 3.3 and 5.3). `LIGNIN_DAMAGE_SEEDS`,
  `LIGNIN_DAMAGE_SECONDS`, `LIGNIN_DAMAGE_OUT` and `LIGNIN_DAMAGE_IMAGES`
  make it a campaign.
- `lignin-tree/src/directory/tests/moves.rs`,
  `a_move_over_a_merk_that_hosts_itself_is_refused_not_walked_forever`: the
  test of VER-10-1.
- `lignindb/src/db/tests/open.rs`,
  `a_store_subdirectory_that_is_a_link_is_refused_and_not_followed` and
  `a_pipe_or_a_link_in_the_place_of_a_file_is_refused_and_not_opened`: the
  tests of VER-10-5 and VER-10-7.
- `lignindb/src/backup/tests/copy.rs`,
  `a_source_with_a_link_or_a_pipe_where_the_copy_reads_is_refused`: the
  test of VER-10-6.
- `grovedb/src/engine/lignin/tests/measured.rs`: the memory of one
  operation on either engine (section 6), a measurement that is run by
  hand, one case in a process (`LIGNIN_MEMORY_CASE`,
  `LIGNIN_MEMORY_ENGINE`), and the check of the database that a process
  killed at a memory limit left (`LIGNIN_MEMORY_DIR`).
- `lignindb/src/db/tests/carried.rs`: the bytes of values that one small
  write carries, at the engine's own surface, a measurement that is run by
  hand.
