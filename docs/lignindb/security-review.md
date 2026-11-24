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
asked for nothing more without completing (stopped) is an allowed end, with
nothing committed. The two engines answer each case alike — the same requests
after the altered one, the same end, the same number of intermediate
commits. On the standard run the cases end as a mix of a committed restore
(an alteration that is an equivalent encoding, which rebuilds the source's
state), a refused payload, a resumable session and a session the altered
request starved. The test asserts a mix so that it cannot pass by refusing
everything.

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
where a state-sync peer or an application's user reaches a panic, undefined
behaviour, or an unsound store that is read as sound. Memory is treated in
section 6: an operation is bounded by the sizes the application supplies
and the values the store already holds, and a valid commit's own content
decides an open's or a recovery's memory — no bounded input was found that
makes unbounded memory.

## 5. The campaigns

### 5.1 Where they ran

The format build, the GroveDB test library and the engine tests do not
build under this repository's memory constraints on the shared box, so they
were built and run on a short-lived cloud sandbox and a dedicated droplet.
The sandbox-hours and droplet-hours are in the report. No wall-clock time
is stated as a result here; the campaigns' result is pass or fail and the
count of cases.

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

## 6. The memory observations

The second question — adversarial keys and values cannot exhaust memory —
is read as: no operation holds memory that is not bounded by the sizes the
application supplies and the store already holds, and there is no path
where a bounded input makes unbounded memory. A node key is capped at 255
bytes and a value at `MAX_VALUE_LEN` (a format limit, `u32`, not a
peak-memory budget), a long flat key is held in a nested tree whose memory
is the key's own length, and a transaction's write set is the writes the
application made, as on the RocksDB engine.

**One operation can retain more than the bytes it writes.** When a page
splits or merges, the cells that move take their out-of-line values with
them: each such value is read whole from the value log and held until the
commit (`lignin-tree/src/flat/store.rs`, `value/write.rs`, the move rule of
FMT-07). So one write of a few bytes can hold, besides its own value, the
existing values of up to a page's worth of carried cells — the cells' count
times the per-value size limit. There is no enforced per-operation
admission budget on this in the LigninDB crates; the bound is the value
size limit and the cells a page holds. The RocksDB engine's logical update
for the same operation need not rewrite those neighbours, so this is not
bounded by a RocksDB comparison. This is application-reachable, through
whatever the layer above stores out of line, and PERF-04 measures the bytes
it rewrites; a deployment that must bound an operation's peak sets the
value size limit of the layer above with that in mind. This review did not
establish a deployment OOM threshold, and records the shape of the cost
rather than declaring exhaustion impossible.

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

Severity: **high** — a state-sync peer or an application's user reaches a
panic, unbounded memory, an unsound store, a wrong answer, or undefined
behaviour; **medium** — the same from a directory made by hand with valid
checksums, where the fix is worth making; **low** — a state only a
directory made by hand with valid checksums reaches (so the attacker has
the node's files or supplies the operator a directory), a debug-only
assertion, or a bounded cost. Status: **fixed** names the commit; **open
(owner)** is sent to the owner because the fix is in another unit's file or
is a design choice; **accepted** is left as it is with the reason.

No finding is of high severity, and none of high severity is open.

| id | severity | where | an attacker supplies | what happens | status |
| --- | --- | --- | --- | --- | --- |
| VER-10-1 | low | `lignin-tree` `directory/rekey.rs`, `plan_rekey` | a directory with a rootless Merk whose child record names itself, or two rootless Merks that host each other | a move's walk over the Merks below the moved one reads the cycle's records again without end and grows its plan without bound (a hang, then out of memory) | **fixed** (`eb78068a9`): the walk refuses a Merk reached a second time, which `Directory::check` already calls damage. A test plants the cycle and shows the walk does not end before the fix |
| VER-10-2 | low | `lignin-tree` `flat/cursor.rs`, `next_leaf`/`down` | a directory in which two slots of a flat branch, or two buckets of a leaf, name one lower page (a DAG with valid checksums) | a scan of that keyspace visits the shared pages once per path to them; the work is the product of the fan-outs, which an acyclic shared-page graph of a few pages already drives past any practical deadline (with an empty leaf below, a single `seek`/`next` call). Reached at open (`Directory::load` scans the dropped records) and in any scan of the keyspace. The checker is not affected: it walks pages directly and reports the page named twice | **open (owner)**: the fix is a bound on a scan in the flat cursor, a hot read path and a design choice |
| VER-10-3 | low | `lignin-store` `table/checkpoint.rs`, `table/chains.rs` | a table checkpoint whose links name page ids far apart (one per 4096-id chunk) | the page table allocates a chunk of 4096 node slots (112 KiB) for the first id of each 4096-id range a link falls in, and the directory of chunk pointers grows with the largest id, so a checkpoint of N spread links takes about N × 112 KiB at open; the allocator's map grows the same way | **open (owner)**: inherent to the chunked page table; a bound belongs with STORE-03 / the page-table format. Reads the node's own checkpoint file |
| VER-10-4 | low | `lignin-store` `recovery/replay.rs`, `wal/reader.rs` | a log whose groups create many pages that carry a value not yet placed, or one group whose stated length is the file's (a valid large commit reaches the same path) | recovery holds every page with a pending value to the end of the log, and the log reader holds one whole commit group; the memory of an open is the log's own content, bounded by no option | **accepted**: a recovery reads the node's own files, and a valid large commit reaches the same WAL-group path, so the cost is application-reachable, not only a hand-made file (section 6). The log reader's window reserves through `try_reserve_exact` and fails as an error where the allocator refuses; the rebuilt-page path (`install_rebuilt`: `collect`, `with_capacity`, `to_vec`) aborts rather than returns. A memory cost, not a panic from a decoded value. Noted for the open's memory bound |
| VER-10-5 | medium | `lignindb` `db/open.rs`; `lignin-store` `segment/store.rs` (`remove_unnamed`), `wal/store.rs`, `values/store.rs`, `table/checkpoint.rs` | a store directory in which the `seg`, `wal`, `val` or `table` subdirectory is a symbolic link to another database's directory, under a manifest that names no plain file there | the open (`SegmentStore::open` and its peers, reached by `recover`) lists that directory through the link and removes the files in it that the manifest does not name, deleting the files of the directory the link leads to — data destruction outside the supplied directory. A file with an id above the manifest's last id is protected by a length or content check first; files at or below it are removed with no check. It skips an entry that is itself not a plain file but does not ask whether the subdirectory it lists is a link | **open (owner)**, medium for the external data destruction: the open should refuse a store subdirectory that is a link, or open it no-follow, before it removes within it. No peer or application route; needs an operator to open a directory an attacker supplied |
| VER-10-6 | low | `lignindb` `backup/mod.rs` (`plan`, `copy`) | a backup's source directory in which a file the manifest names is a symbolic link, or a sparse file whose manifest length is large | the backup opens each file the source's manifest names by its path and copies its bytes, following a link, so a backup of a supplied source can carry bytes from outside the source (a confidentiality reach); a sparse file named with a large length is copied dense | **open (owner)**: the backup should open copied inputs no-follow (a stat that follows the link would not fix it). Needs an operator to back up a directory an attacker supplied |
| VER-10-7 | low | `lignindb` `db/open.rs`, `backup/mod.rs`; `space.rs` | a store directory in which `MANIFEST`, or a file the manifest names, or `RESERVE`, is a named pipe | a command that opens the affected path blocks reading the pipe, with no error: the open and a backup read `MANIFEST`, a session that keeps a reserve reads `RESERVE`, and reading a manifest-named file's contents hangs the reads that reach it (listing a file's metadata does not). It is not every subcommand | **accepted**: a directory an attacker supplied, opened by an operator; a blocking read of a pipe is not a panic. Noted |
| VER-10-8 | low | `lignin-tree` `drop/flat.rs`, `drop/merk.rs`, `drop/step.rs` | a directory with a drop record that names a page or a root cell in use | the reclaimer frees the page and everything its branches and buckets name, or removes the cells the root reaches, with no error: the birth and shape checks it makes are not ownership checks, so data a live tree still names is taken out in the background | **accepted**: whoever writes the drop record has the node's files and could remove the data directly; a `verify` that completes before any background reclaim (not only before an explicit sweep) reports the store as unsound. Noted |
| VER-10-9 | low | `lignin-tools` `dump.rs` (`order`); `lignindb` `db/mod.rs`, `Db::wipe` | a directory whose entries name each other as hosts (a host cycle); with one host per entry the cycle has no hostless top, so it is reached by selecting a member of it, not by `--all` | the dump's walk reached a subtree a second time and panicked on the second `remove`, past the dump's output-file cleanup; `lignin dump --all` and `Db::wipe` leave the cycle's entries out, since a cycle has no top (a wrong answer on a damaged directory) | **fixed** for the panic (`bac15b357`, test tightened here: the walk returns a `ToolError` on a repeated entry; a test selects a cycle member and shows the refusal, and that `--all` leaves a topless cycle out). **accepted** for the omission and the wipe: a directory an attacker supplied, a wrong answer on it |
| VER-10-10 | low | `lignin-tools` `segments.rs`, `dump.rs` | a segment file the manifest names as longer than it is on disk (a sparse file), or a value record with a large stated length | `lignin segments` reads a segment whole up to its manifest length (it skips a file shorter than its manifest length first), so holes in a file that is logically long enough are read dense; an ordinary segment of that logical length costs the same | **accepted**: a tool run on a directory an attacker supplied; the reads are bounded by the manifest's own lengths. Noted |
| VER-10-11 | low | `lignindb` `verify/mod.rs` | a store with very many values out of line or flat records | the checker holds in memory a record per out-of-line value and the key of every flat record of a rooted subtree's data column, with no cap: its memory is proportional to the store's size | **accepted**: by design — the checker is run on a store to judge it, and the store's own size bounds its memory. Noted so an operator knows a checker's run costs memory in proportion to the store |
| VER-10-12 | low | `lignin-store` `flush/run.rs`, `flush/write_back.rs` | — (not an attacker's input) | a panic in a write-back turn leaves the `running` flag set, so a committer that later waits for room never returns; no file byte was found that makes the turn panic | **accepted**: not reached by any input this review found; a robustness note that a background panic should end the committer's wait rather than hang. Noted |
| VER-10-13 | low | `lignin-tree` `bulk/load.rs` (`Loader::node`), driven by `lignindb` `txn/frames.rs` (a whole-Merk load) | a node feed whose keys are out of order or whose link heights are wrong | the loader stores the heights as stated and does not check its keys' order; a feed that is out of order would commit a store the checker then flags (`KeyOrder`, `LinkHeights`) | **accepted**: an internal invariant, not an attacker-reachable feed at this head. Its callers are the write transaction's whole-Merk load (`txn/frames.rs`), fed the nodes of a change set the Merk layer built in traversal order, and the state-sync restore, which hands a hash-verified proof tree; neither drives attacker-chosen order or heights. `lignin load` does not reach it (it writes node by node through `scope.apply`, not the loader). Becomes a finding if a future caller drives the loader from an order it has not verified |

## 8. What is accepted, and what would make it a finding

Every row marked **accepted** or **open (owner)** is a state a directory
made by hand reaches, with the node's files or an operator who opens a
supplied directory, except: VER-10-4 and VER-10-11, a memory cost in
proportion to legitimate data (a valid commit's WAL groups, the store's own
size) rather than unbounded memory from a bounded input; VER-10-12, a
robustness note; and VER-10-13, an internal loader invariant whose callers
at this head (the write transaction's whole-Merk load, the verified restore)
never drive it out of order. None is a panic, an unsound store read as
sound, or unbounded memory reached by a state-sync peer or by an
application's user through GroveDB's operations. VER-10-5 is medium
for the data it can destroy outside the supplied directory; the rest are
low. The one corrupted-directory panic found (the dump of a host cycle,
VER-10-9) is fixed, and no panic is reached from a peer or an application's
user. They **become a finding of higher severity
if**: an application's user or a state-sync peer can bring the engine into
one of these states — in particular, if a public operation comes to write a
directory record that cycles (VER-10-1, VER-10-9) or a drop record that
names a live page (VER-10-8) without the write building it from a sound
state; or if a path the engine opens comes to be built from bytes a peer or
a user supplies rather than from the operator's own directory (VER-10-5,
VER-10-6, VER-10-7).

## 9. Code and tests

- `grovedb/src/engine/lignin/tests/sync/peers.rs`: the state-sync peer
  (section 3.1). Runs on both engines where the RocksDB engine is compiled
  in. `LIGNIN_HOSTILE_PEERS=wide` makes every alteration at every part and
  adds a session that commits in parts; `LIGNIN_HOSTILE_PEER_SEEDS` and
  `LIGNIN_HOSTILE_PEER_SHARD` set and shard the random cases.
- `lignindb/src/verify/tests/injected/hostile.rs`: every call on an
  unsound store (section 3.2). `LIGNIN_HOSTILE_SEEDS` sets the range of the
  random case.
- `lignin-tree/src/directory/tests/moves.rs`,
  `a_move_over_a_merk_that_hosts_itself_is_refused_not_walked_forever`: the
  test of VER-10-1.
