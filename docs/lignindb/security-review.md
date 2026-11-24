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
    A checksum (CRC32C) is meant to catch it. A page that a checksum
    passes is one the engine wrote, or one changed with its checksum made
    again, which is the next kind.
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
manifest, the value log or the page-table checkpoint does not panic on any
bytes. What this leaves: what the callers of a decoder do with what it
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
returns — with an answer or an error — on a store the checker calls
unsound, and that no such store makes the engine panic.

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
session a refusal abandoned refuses its commit; and a session a refusal
caught before it applied anything is left resumable with the database
unchanged. The two engines answer each case alike — the same requests
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
raised in the engine's code, a cursor that shows more records than the
store holds, and a call that does not return within its time are findings;
an error is an answer.

The damage is of three kinds, each a store a directory with valid checksums
can give the engine: every corruption of the checker's own acceptance list
(a hash changed, a link to no cell, a cell named twice, a value pointer
that leads nowhere, a directory record that disagrees with the cells, and
the rest); a child link made to lead back to its own cell, the cell above
it or the root, and a flat branch made to name itself or one leaf
throughout; and a page changed at random in a way a load still takes. On a
sound store every step returns and the checker calls it sound each time.
Over every damaged store no step panicked, hung or showed a phantom record:
each step returned, many with an error the fixture expected of a damaged
store. A longer run of the random case is named by `LIGNIN_HOSTILE_SEEDS`.

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
behaviour, an unsound store that is read as sound, or memory that is not
bounded by the size of what they supplied.

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
that AddressSanitizer flagged.

## 6. The memory observations

The second question — adversarial keys and values cannot exhaust memory —
is met: a node key is capped at 255 bytes and a value at the same
`MAX_VALUE_LEN` the RocksDB engine allows, a long flat key is held in a
nested tree whose memory is the key's own length, and a transaction's write
set is the writes the application made, as on the RocksDB engine. One
operation that splits or merges a page appends again, and holds until the
commit, the out-of-line values of the cells it carries (the move rule of
FMT-07); that is bounded by the value-size limit times the cells a page
holds, and is no more than the RocksDB engine rewrites for the same
operation, where a value is rewritten on every change. PERF-04 measures it.

The memory a file can make the engine take is a different matter, and is
the disk attacker's, not the user's. The table records where a file's own
content — the length of a log group, the spread of the page ids in a table
checkpoint, the number of pages a log names with a value not yet placed —
decides how much memory an open or a recovery takes. Each is reached only
by a file the attacker wrote, and each reads the node's own files, where
integrity is the checker's to attest (`audit-non-issues.md`, "Trust-boundary
findings"); none is a panic or undefined behaviour.

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
| VER-10-2 | low | `lignin-tree` `flat/cursor.rs`, `next_leaf`/`down` | a directory in which two slots of a flat branch, or two buckets of a leaf, name one lower page (a DAG with valid checksums) | a scan of that keyspace visits the shared pages once per path to them; with an empty leaf below, one `seek`/`next` call does not return. Reached at open (`Directory::load` scans the dropped records) and in any scan of the keyspace. The checker is not affected: it walks pages directly and reports the page named twice | **open (owner)**: the fix is a bound on a scan in the flat cursor, a hot read path and a design choice |
| VER-10-3 | low | `lignin-store` `table/checkpoint.rs`, `table/chains.rs` | a table checkpoint whose links name page ids far apart (one per 4096-id chunk) | the page table allocates a chunk of 4096 node slots for the first id of each chunk, so a checkpoint of N such links takes about N × 112 KiB at open; the allocator's map grows the same way | **open (owner)**: inherent to the chunked page table; a bound belongs with STORE-03 / the page-table format. Reads the node's own checkpoint file |
| VER-10-4 | low | `lignin-store` `recovery/replay.rs`, `wal/reader.rs` | a log whose groups create many pages that carry a value not yet placed, or one group whose stated length is the file's | recovery holds every page with a pending value to the end of the log, and the log reader holds one whole commit group; the memory of an open is the log's own content, bounded by no option | **accepted**: a recovery reads the node's own files; the reservations go through `try_reserve_exact` and fail as an error, not a panic, where the allocator refuses. Noted for the open's memory bound |
| VER-10-5 | low | `lignindb` `db/open.rs`; `lignin-store` `segment/store.rs` (`remove_unnamed`), `wal/store.rs`, `values/store.rs`, `table/checkpoint.rs` | a store directory in which the `seg`, `wal`, `val` or `table` subdirectory is a symbolic link to another database's directory, under a manifest that names no file there | the open (`SegmentStore::open` and its peers, reached by `recover`) lists that directory through the link and removes the plain files in it that the manifest does not name, so it deletes the files of the directory the link leads to; it skips an entry that is not a plain file but does not ask whether the subdirectory it lists is itself a link | **open (owner)**: the open should refuse a store subdirectory that is a link, or stat it before it removes within it. Needs an operator to open a directory an attacker supplied |
| VER-10-6 | low | `lignindb` `backup/mod.rs` (`plan`, `copy`) | a backup's source directory in which a file the manifest names is a symbolic link, or a sparse file whose manifest length is large | the backup opens each file the source's manifest names by its path and copies its bytes, following a link, so a backup of a supplied source can carry bytes the link leads to; a sparse file named with a large length is copied dense | **open (owner)**: the backup should stat each file it copies. Needs an operator to back up a directory an attacker supplied |
| VER-10-7 | low | `lignindb` `db/open.rs`, `backup/mod.rs`; `space.rs` | a store directory in which `MANIFEST`, or a file the manifest names, or `RESERVE`, is a named pipe | the open, a backup, or a session that keeps a reserve blocks reading the pipe, with no error; every `lignin` subcommand and `Db::open` on such a directory hang | **accepted**: a directory an attacker supplied, opened by an operator; a blocking read of a pipe is not a panic. Noted |
| VER-10-8 | low | `lignin-tree` `drop/flat.rs`, `drop/merk.rs`, `drop/step.rs` | a directory with a drop record that names a page or a root cell in use | the reclaimer frees the page and everything its branches and buckets name, or removes the cells the root reaches, with no error: data a live tree still names is taken out in the background | **accepted**: whoever writes the drop record has the node's files and could remove the data directly; a `verify` run before the reclaim reports the store as unsound. Noted |
| VER-10-9 | low | `lignindb` `db/mod.rs`, `Db::wipe`; `lignin-tools` `dump.rs` | a directory whose entries name each other as hosts, or whose host names a prefix with no entry | a wipe drops the subtrees that no other hosts, found by the entry's own host, so entries in a host cycle are left; `lignin dump --all` leaves the same entries out of "the whole database" with no word | **accepted**: a directory an attacker supplied; the result is that a wipe of a damaged directory leaves records and a dump omits unreachable ones. Noted |
| VER-10-10 | low | `lignin-tools` `segments.rs`, `dump.rs` | a segment file the manifest names as longer than it is on disk (a sparse file), or a value record with a large stated length | `lignin segments` reads a segment whole up to its manifest length, and a sparse file is read dense | **accepted**: a tool run on a directory an attacker supplied; the reads are bounded by the manifest's own lengths. Noted |
| VER-10-11 | low | `lignindb` `verify/mod.rs` | a store with very many values out of line or flat records | the checker holds in memory a record per out-of-line value and the key of every flat record of a rooted subtree's data column, with no cap: its memory is proportional to the store's size | **accepted**: by design — the checker is run on a store to judge it, and the store's own size bounds its memory. Noted so an operator knows a checker's run costs memory in proportion to the store |
| VER-10-12 | low | `lignin-store` `flush/run.rs`, `flush/write_back.rs` | — (not an attacker's input) | a panic in a write-back turn leaves the `running` flag set, so a committer that later waits for room never returns; no file byte was found that makes the turn panic | **accepted**: not reached by any input this review found; a robustness note that a background panic should end the committer's wait rather than hang. Noted |

## 8. What is accepted, and what would make it a finding

Every row marked **accepted** or **open (owner)** is a state a directory
made by hand reaches, with the node's files or an operator who opens a
supplied directory, except VER-10-11 and VER-10-12, which are a checker's
memory by design and a robustness note. None is reached by a state-sync
peer or by an application's user through GroveDB's operations, and none is
a panic or undefined behaviour. They **become a finding of higher severity
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
