# LigninDB version model and transaction rules

This is the specification for task TXN-01 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
fixes the rules every transaction task builds on:

- what the database state is, and what a commit version names (sections 2
  and 3)
- what each kind of handle reads (section 4)
- what a snapshot pins, and how anything is ever reclaimed (sections 5 and 6)
- the write transaction: its write set, its private frames, its read rule,
  savepoints and rollback (section 7)
- commit, and exactly when a commit fails with a conflict (section 8)
- several threads writing inside one transaction: write scopes, latching and
  what must be finished before commit (section 9)
- how the flusher, the cleaner, page id reuse and checkpoints respect all of
  that (sections 10 and 11), and what a crash leaves (section 12)
- every pair of concurrent actors, with the rule that governs it (section 13)
- how GroveDB's use of the RocksDB engine maps onto these rules, and every
  place where LigninDB deliberately differs from it (section 14)

It defines no Rust API and no data structure. Those belong to TXN-02 to
TXN-09, STORE-08 and STORE-10. Section 15 says what each of them takes from
here and lists the tests. Section 16 shows how the read rule and the conflict
rule were derived from the RocksDB engine. Section 17 lists the choices made
here that the plan left open.

Every numbered rule is a requirement on the engine, and "must" marks one
inside running text. The error names (`Conflict`,
`ScopeOverlap` and so on) are working names: the task that implements an error
chooses its Rust name. What is fixed is that each is a distinct typed error.

It builds on [`format.md`](format.md) (FMT-01) and
[`platform-requirements.md`](platform-requirements.md) (BASE-05). Rule ids
from those documents are written with the document's name, for example
"format.md B11" or "P-09".

## 1. Terms and notation

- **Commit**: the one step that makes a set of writes part of the database.
- **Version**: the number of a commit (`CommitVersion`, format.md 3.2).
  *S(v)* is the state after commit *v*. *S(0)* is the empty database.
- **L**: the latest committed version. It changes only at a commit.
- **Record**, **record key**, **range**: section 2.
- **Handle**: what a caller reads or writes through. There are two kinds: a
  **snapshot** and a **write transaction**.
- **Operation**: one call into the engine through a handle, of the size of
  one storage call today. Each of these is one operation: reading one record
  (a point read in any column, or the fetch of one Merk node); opening a
  cursor; one cursor step; applying one batch of writes, or one write
  through an immediate context; ending a scope; a commit. A Merk traversal,
  a proof and a GroveDB query are many operations.
- **Caller operation**: one call of the layer above the engine, for example
  one GroveDB `get`, `insert` or `apply_batch`. It is made of many
  operations.
- **Foreign commit**: for an open write transaction *T*, any commit other
  than *T*'s own that is published while *T* is open.
- **Page version**: the content one commit gave to one page id (section 6).
- **Private frame**: a page image that holds uncommitted writes and that only
  its transaction can see (WT4).
- **Version pin**: a hold on one version that keeps it readable (section 5).
  A **frame pin** is something else: the buffer pool's guard against evicting
  a frame in use (STORE-04). This document never shortens either to "pin"
  where both could be meant.
- **Actors**: a **reader** (a snapshot), a **writer** (a thread writing
  through a scope of a write transaction), the **committer** (the thread
  running a commit), the **flusher**, the **cleaner** and a **checkpoint**
  (the thread creating one).

Notation used in the rules:

| Symbol | Meaning |
| --- | --- |
| *L* | latest committed version |
| *S(v)* | the state at version *v* |
| *H* | the oldest pinned version, or *L* when nothing is pinned (PN3) |
| *B(T)* | the version transaction *T*'s private frames are built over (WT3) |
| *fw(e)* | the first-write version of write-set entry *e* (WT5) |
| *v(c)* | the version of commit *c* |

## 2. The logical state

Everything in this document is stated over one model of the state. It is the
model the RocksDB engine has today, so that the two engines can be compared
rule by rule.

**ST1. The state is a set of records.** A record is a value under a **record
key**. A record key is three things:

| Part | Values |
| --- | --- |
| column | `data`, `aux`, `roots` or `meta` |
| subtree | 32 bytes: the path prefix of a subtree, or a prefix derived from one |
| key | the key bytes inside that subtree |

The four columns are the four RocksDB column families GroveDB uses today
(`default`, `aux`, `roots`, `meta`). The subtree part is the 32-byte prefix
that `RocksDbStorage::build_prefix` and `secondary_prefix_for` produce, and
the reserved prefix of the pending-drop records.

In the `data` column a record is one of two things:

- a **Merk node**: the key is the node's key. The value is the node as Merk
  holds it: the element bytes, the feature type and aggregate data, the value
  hash, the key-value hash, and the left and right links. A link carries the
  child's key, hash, heights and aggregate data.
- a **flat entry** of an append-only tree.

`aux`, `roots` and `meta` records are plain values. `roots` holds the base
root key (`ROOT_KEY_KEY`).

**ST2. A record key is an identity, not a location.** It is the same triple
that identifies a RocksDB key today (column family, then prefix followed by
key). "Wrote a key", "its own write" and "conflict" below are all decided by
record key. Where the record's bytes are (which page, which slot, how many
deltas, which value-log entry) is never part of it.

**ST3. The subtree directory is not a record.** It is derived from the parent
elements (TREE-05). It belongs to a version like everything else: at every
version it agrees with the records of that version.

**ST4. A range is a set of record keys** given without listing them. Four
shapes exist:

- every key of one column under one subtree
- the keys of one column under one subtree inside a key interval
- every key of every column under one subtree (a subtree drop, CV9)
- every key of every column (wipe)

A range contains a key whether or not a record exists under it.

**ST5. A write is a put of a record, a delete of a record, or a delete of a
range.** A put that leaves the bytes as they were is a write. So is a delete
of a key that has no record. (RocksDB counts both: cases A7 and A7b in
section 16.)

**ST6. One node in a Merk change set is one write of that node's record
key**, whatever its class: value written, structure changed, hash only or
removed (SEAM-01). The change set must name exactly the node records that
today's Merk hands to storage for the same operation: no node left out
because only its hash changed, and none added. The conflict rule equals
RocksDB's only if this holds (section 14).

## 3. Commit versions

format.md 3.2 fixes the number: a dense 64-bit count, version 0 for the empty
database, each commit the previous plus one. This section says what gets a
number. The dense count is kept as it is; nothing here needs gaps.

**CV1. One commit, one version, one log group.** A commit takes version
*L* + 1, appends one log group that carries it, and publishes it (CM3).

**CV2. A commit with an empty write set is not a commit.** It succeeds, takes
no version and appends nothing. A version therefore always has a log group,
and a run of log groups can be checked for a missing version.

**CV3. Every change to a record is a commit, and nothing else is.**

| Takes a version | Takes no version |
| --- | --- |
| commit of a write transaction with a non-empty write set | rollback, and a commit with an empty write set |
| a write made with no transaction (RD7), whatever it holds: puts, deletes, a range delete or a bulk load. One commit. | every read |
| wipe (TXN-07): exactly one commit, which deletes the range "everything" | the flusher: write-back, consolidation, table checkpoints |
|  | the cleaner: relocation, deleting files, value-log cleaning |
|  | reclaiming the pages of a dropped subtree (CV9) |
|  | creating, opening or deleting a checkpoint |

Because wipe is one commit, no handle ever sees a database that is wiped in
one column and not in another, and a crash during a wipe leaves the database
either unchanged or empty (P-23, VER-04). The RocksDB engine wipes in four
steps (14.5).

**CV4. *L* is one atomic value.** Publishing a commit is storing the new *L*.
Commits are therefore totally ordered, and "the latest committed state" is
well defined at every instant.

**CV5. Versions are assigned under the commit lock** (CM2), so they are dense
and never out of order.

**CV6. Along the history that survives, versions are dense, never reused
and never go down.** These are the three properties of format.md 3.2. They
are properties of the database's history, and a crash can shorten that
history: commits are not fsynced one by one (DEC-04), so recovery can land on
a version *Lr* below the last one the process published (DU2). The commits
above *Lr* are then not part of the database. Recovery must leave nothing
that names them (DU3), and the next commit takes *Lr* + 1.

That number was carried, before the crash, by a commit that no longer
exists. No other choice satisfies both format.md and DEC-04: leaving the
lost numbers unused would put a gap into the count, which format.md 3.2
rules out and on which the log format relies (consecutive groups differ by
one). format.md 3.2 states the qualification and points to DU3, which is
what makes the reuse safe.

**CV7. `CommitVersion::NONE` means "not yet".** A structure that records the
version at which something stopped being current (PV1) stores `NONE` while it
is still current.

**CV8. The same state can have several versions.** A wipe gives the empty
state a version other than 0. Nothing may treat equal states as equal
versions, or compare versions of two database directories (format.md 3.2).

**CV9. Dropping a subtree is a write of the transaction that drops it.** A
subtree drop (TREE-09) is a range delete (ST4) of every record under the
dropped prefixes. It is an entry in the write set of the transaction that
makes it, and it takes effect for every other handle at that transaction's
commit. Reclaiming the dropped pages afterwards changes no record and takes
no version (CL6). The RocksDB engine removes those records later, in
separate steps (14.5).

## 4. What each handle reads

| Handle | Reads | Sees a commit made after it was opened | Sees uncommitted writes |
| --- | --- | --- | --- |
| Snapshot | *S(s)*, for the one version *s* it pinned | never | never |
| Read with no transaction | a snapshot held for one caller operation (RD2) | not during that operation | never |
| Write transaction | its own writes, and otherwise *S(L)* at the moment of each read (RD3) | yes, for every key it has not written | only its own |
| Write with no transaction | a write transaction held for one caller operation (RD7) | as a write transaction | only its own |

**RD1. A snapshot reads one version.** Taking a snapshot pins *s* = *L* at
that instant. Every read through it, at any later time and from any thread,
returns exactly *S(s)*. A snapshot is read-only: a write or a commit through
it fails with a typed error (`ReadOnly`), and rollback is a no-op. This is
what `start_snapshot_read_transaction` gives today (P-11).

**RD2. A read with no transaction is a snapshot held for one caller
operation**, for example one GroveDB `get` or one query. RocksDB reads the
latest state at each storage read inside such an operation, so an operation
that makes several reads can see a commit land between them. Pinning gives
the result RocksDB gives when that commit lands just after the operation,
which is one of the results RocksDB can give. It is also what the plan's read
path says ("a query pins the current version").

**RD3. A write transaction reads its own writes, and for everything else the
latest committed state at the moment of the read.** For a record key *K*, a
read through transaction *T* returns:

1. if *T*'s newest write that covers *K* is a put: the value *T* put
2. if it is a delete, or a range delete that contains *K*: no record
3. if *T* has no write that covers *K*: the record *S(L)* has under *K*, for
   *L* as RD4 defines it

This is RocksDB's rule (P-08), not snapshot isolation. *T* has no snapshot of
its own. A commit made outside *T* while *T* is open is visible to *T*'s next
read of any key *T* has not written, and invisible for the keys it has.

**RD4. The moment of the read is one instant per operation.** Each operation
through a write transaction reads committed records from one version: the *L*
of some instant between the call and its return. An operation never mixes two
versions. Between two operations *L* may have moved.

An operation is as small as section 1 defines it, so this is no stronger than
RocksDB. There each of these is one call (`get`, an iterator, the application
of a batch), and each sees one state. Fetching several nodes of one Merk is
several operations, and a commit can land between two of them on either
engine; RD5 says what is then read.

**RD5. The overlay is by record, also for Merk.** RD3 applies to each node
record on its own. A link in a node that *T* wrote is followed by the child's
key: the child is *T*'s node under that key if *T* wrote one, and otherwise
the committed node under that key. The root of a Merk is the node under the
root key that the parent element holds, or that the base root record holds,
each read under RD3. So after a foreign commit, *T* can see a tree that mixes
its own older nodes with newer committed ones. Section 7.3 says what the
engine owes in that state, and section 14.3 walks through an example that was
run on RocksDB.

**RD6. A cursor through a write transaction merges two sides in key order.**

- The committed side is fixed at the *L* of the instant the cursor was
  opened. The cursor holds a version pin on it (PN1). A foreign commit made
  after that is not seen by this cursor. It is seen by the next cursor.
- *T*'s own writes are live: a write *T* makes after the cursor was opened
  is seen when the cursor reaches that key. A delete by *T* hides the
  committed record.
- Writing through *T* to the key the cursor stands on leaves the current
  entry unspecified until the cursor moves. TXN-03 may tighten this.

A cursor on a snapshot reads *S(s)* for its whole life (TREE-06).

**RD7. A write with no transaction is a write transaction of its own, opened
and committed around one caller operation.** It reads by RD3, and its
commit follows section 8 like any other, including the conflict rule. It
never waits for another open write transaction, and another open write
transaction never sees its writes before it commits. Once it has committed,
every other handle is governed by its own row of the table above: an open
write transaction sees it at once for the keys it has not written.

**RD8. Nobody but *T* sees *T*'s uncommitted writes.** No snapshot, no other
write transaction, no background actor and no checkpoint reads them (P-10).

**RD9. Equal records give equal costs.** What a read charges is a function of
the records it returns (BASE-06, SEAM-05). So a handle that returns the
records RocksDB would return charges what RocksDB would charge.

## 5. Version pins and operation guards

Two things keep data alive. A version pin keeps *content* readable for as
long as someone may ask for that version. An operation guard keeps
*locations* valid for the length of one operation.

**PN1. Who holds a version pin.**

| Holder | Version | Held |
| --- | --- | --- |
| a snapshot | *s* | from creation to drop |
| a read with no transaction | *L* at its start | for that caller operation (RD2) |
| a cursor of a write transaction | *L* when it was opened | until the cursor is dropped |
| a write transaction with a non-empty write set | its base *B(T)* | until it is rebased, committed or rolled back |
| a checkpoint being created | the version it captures | until its files are linked (CK3) |

A write transaction with an empty write set and no cursor pins nothing, so
an idle transaction costs nothing.

**PN2. What a version pin on *s* keeps.** As long as the pin is held, all of
this must stay true:

1. Every page version visible at *s* (PV1) can be read: it is in memory, or
   it can be rebuilt from what is on disk.
2. So the segment blocks, the log records not yet written back, and the
   value-log entries that those page versions consist of or point to are not
   deleted and not overwritten.
3. No page id that is reachable at *s* names a different page (ID2).
4. The subtree directory and the flat keyspaces at *s* are readable. They are
   pages, so 1 to 3 cover them.

A pin keeps nothing else. In particular it does not stop commits, write-back,
consolidation, relocation or checkpoints.

**PN3. A new pin is always on *L*.** There is no way to open an older
version. So the oldest pinned version, *H*, never goes down. When nothing is
pinned, *H* is *L*.

**PN4. Taking a pin takes no lock and cannot lose a race with reclamation.**
A pin must be announced where reclaimers look *before* the reader relies on
the version, and the reader must then confirm the version it announced is
still one a reclaimer has to respect. A design in which a reader loads *L*,
is delayed, and then registers a pin on a version whose page versions were
reclaimed in between is forbidden. TXN-02 chooses the mechanism, and VER-05
models this race.

**PN5. Every operation holds an operation guard.** That includes operations
of write transactions and of the flusher, the cleaner and a checkpoint. While
an operation runs, every frame it reached, every page-table entry it read and
every file location it took from one stays valid: the frame is not recycled
and the file is not deleted. A node view handed to a caller (TREE-02) holds
its guard for as long as the view lives.

**PN6. Retire first, reclaim later.** Nothing that a reader could still reach
is reused or deleted directly. It is first **retired**: unlinked from every
current structure, so that no new operation can find it. It is **reclaimed**
only when both of these hold:

- no version pin needs it (PN2, PV3)
- every operation guard that was held when it was retired has been released

The first condition is counted in versions. The second is not, because
relocation and consolidation retire things without any commit: it needs the
engine's own clock of retirements (the "epoch" of TXN-02).

**PN7. Pins never block and are never refused.** A pin held for a long time
costs disk space, never correctness: it holds back the cleaner (CL1) and page
id reuse (ID2). STORE-08 bounds and reports that cost.

**PN8. A location is good only at the version it was read at.** A location is
a page id, with or without a slot. An id names a page only together with a
version (format.md 3.1), so a location that outlives its operation needs
care. Views are covered by their guard (PN5). This rule is about what a
caller keeps after the view is gone: a pointer in a Merk link, a path cache,
a Merk cache owned by a scope (GROVE-12).

What keeps a location good is the pin on the version it was read at, or,
for a private frame, the frame itself:

- A location into a committed page is good for as long as its holder pins
  the version it was read at (PN1, PN2). A snapshot pins it for its life. A
  cursor pins it for the cursor's life, and that includes a cursor of a
  write transaction: the pages on its stack stay good across foreign
  commits, whatever the transaction's write set. A write transaction with a
  non-empty write set pins its base until it is rebased, committed or rolled
  back.
- Where nothing pins the version, the location is good only until the next
  commit. That is the case for what a write transaction with an empty write
  set keeps between operations, outside any cursor.
- A location into a private frame is good until that frame is replaced: by a
  rebase, a rollback, a rollback to a savepoint, a scope rollback, or the
  reconciliation of a scope merge (LT4).
- A location is never kept outside the handle it was read through.

The engine must make it impossible to follow a location that is no longer
good: an operation that is handed one must be able to tell, and must then
find the record by its key (RD5) instead. One way: the transaction counts
the commits it has observed and its own rebases and rollbacks, a location
carries the count at which it was issued, and a mismatch means "resolve by
key". TXN-03 and MERK-01 choose the mechanism.

## 6. Page versions

The rules above speak of content at a version. This section says what that
is for a page, so that the store (which knows no keys) can enforce them.

**PV1. A page version is the content one commit gave to one page id.** It has
a **born** version *b*: the commit that produced it. It is **superseded** at
version *w* when a later commit produces the next page version of the same
id, or frees the page. Until then *w* is `NONE` (CV7). A page version is
**visible** at version *s* when *b* ≤ *s* < *w*. A read at *s* of a page id
uses the one page version of that id that is visible at *s*.

**PV2. A published page version never changes.** Its logical content (cells,
keys, values, hashes, links) is fixed at the commit that bore it. A later
commit makes a new page version; it never edits an old one. This is why
readers need no latch (LT6).

**PV3. A page version is needed while a pin can see it.** It is **needed**
if it is visible at *L*, or visible at some pinned version. A needed page
version must stay readable (PN2). One that is not needed may be reclaimed
under PN6.

The simple sufficient test is *w* ≤ *H*: nothing older than the oldest pin
can be asked for. The exact test is that no pinned *s* has *b* ≤ *s* < *w*.
An implementation may use either. It must never reclaim a page version that
the exact test calls needed.

**PV4. Background work may change how a page version is stored, never what
it says.** Relocating its blocks, consolidating its delta chain into a new
base image, and rewriting a value pointer (when the value log is cleaned, or
when a page's values are regrouped at consolidation, TREE-08) all leave
every visible page version with the same logical content. Such a change
is applied either to every needed page version it affects, or only to the
current one while the old representation, and whatever it points to, stays
live for the older ones. A new representation is installed beside the old
one: bytes that a view borrows are never edited (LT2). The old one is
retired, under PN6, only once no needed page version consists of it.

A location that was good for a page version stays good for it (PN8).
Background work does not renumber anything that a pointer in another page,
the subtree directory, the value log or an open handle refers to.

**PV5. A page version records its born version where the store keeps it.**
Deltas and base images carry the version that produced them, in memory and
on disk, so that PV1 can be evaluated for a pin and after a crash (DU3). The
byte layout is FMT-03, FMT-05 and STORE-03's.

**PV6. A background change takes no version. It has a change point, as a
commit has.** It is not a commit (CV3): *L* does not move, no write
transaction rebases, and no conflict can come of it. It is made durable and
recovered by the same means as the pages themselves, so that after any crash
a page and everything stored about it agree. A background change to a page
version born after the recovered version is gone with that page version
(DU3).

Every change to a cell has one **change point**: the instant from which
every handle, and every recovery that keeps the change, sees it. For a
change a transaction makes, it is that transaction's commit point (CM4). For
a change background work makes, it is the page step that installs it (PV7).
Whatever the store keeps about cells changes at the change point, as one
unit with the cell change, and not before. That holds for the live-value
counts of the value log (`value-log-format.md` O6, O7).

`value-log-format.md` words this, in O7 and O9, as "the same commit as the
cell change" and "the commit that changes the cell". For a value that
background work moves there is no commit. "Commit" there is to be read as
the change point. The count is still that of the latest version, as O7 has
it: a background move changes how that version is stored and counted, and
changes no version. That document does not yet say so (section 17 item 23).

**PV7. A value pointer is rewritten only in the current page version, and
never under a private copy.** Of the changes PV4 allows, only a value
pointer rewrite alters the bytes of a page as a handle reads them. Two
things can go wrong with one, and each ends with a committed cell that
points into a file whose live count no longer includes the value, so that
the file could be deleted under it (`value-log-format.md` O7):

- A write transaction made a private copy of the page (WT4) before the
  rewrite. It does not see the rewrite, and its commit brings the old
  pointer back.
- Background work found the owner of a value by looking at a page
  (`value-log-format.md` O9), and a commit superseded that page version
  before the rewrite was applied. The rewrite then lands on a page version
  that is no longer current, while the current one keeps the old pointer.

So a value pointer rewrite is installed in a **page step**: one step with
respect to every commit that installs a version of that page, and to the
making of any private copy of it. In that step it is checked that:

1. the rewrite changes the current page version (it may change older
   needed ones as well, PV4, but never those alone)
2. the owning cell in it still holds the old pointer
3. no open write transaction holds a private copy of the page

If a check fails, nothing is installed and nothing is counted. The copy of
the value that was appended for the move is dead, and the page is left for
later. If all three hold, the new representation is installed and the
counts change, in that same step (PV6).

Installing a page version at commit (CM3 step 7) and making a private copy
are page steps too. Each of the three sees the page as the one before left
it. Which latch or retry an implementation uses for this is STORE-04's and
STORE-09's; it sits at level 5 of LT7.

Two things follow. What a commit installs for a page says what the page
version it supersedes says, with the commit's own changes, and holds the
value pointers that page version holds at that moment. It need not be, byte
for byte, the stored representation: the flusher may have consolidated the
page since the copy was made, which changes no pointer and for which no
commit waits (CM8). And a value pointer that background work replaced is
never committed again.

## 7. The write transaction

### 7.1 State

**WT1. Any number of write transactions may be open. One commits at a time.**
The engine does not refuse a second one. Platform holds one long transaction
per block (P-07), and a write made with no transaction opens a short one
beside it (RD7, P-09). The plan's "one write transaction commits at a time"
is CM2.

**WT2. A write transaction consists of:**

- its **write set**: the ordered list of its writes (ST5), each a put, a
  delete or a range delete, with marks for savepoints and scopes
- a **first-write version** for each write-set entry (WT5)
- its **pending marks** (SC9)
- its **base version** *B(T)* and its **private frames** (WT3, WT4)

**WT3. The write set is the truth, and the private frames are derived from
it.** At every moment *T* can be read, its private frames, laid over the
committed pages of *B(T)*, must show exactly the state that results from
applying *T*'s write set, in order, to *S(B(T))*. A transaction's frames are
never a source of information that its write set does not have.

The write set is self-contained. Nothing that a commit, the flusher or the
cleaner does to committed data changes or removes what a write-set entry
says. An entry can refer to bytes that are stored as part of the committed
state: the value of a node whose hash or links *T* changed, but whose value
it did not. *T* keeps those bytes readable for as long as it may read them.
While they sit in its private frames, its base pin (PN1) and PV7 do that.
When a rebase moves the pin, *T* takes its own copy of whatever it still
needs (WT7).

**WT4. Private frames belong to one transaction.**

- Only *T*, through its scopes, reads or changes them.
- They are not page versions: they have no born version, no reader at any
  version can reach them, and the flusher never writes them (FL1).
- A page id that *T* allocates is private until *T* commits. If *T* rolls
  back, the id goes back to the allocator at once: nobody else ever saw it.
- If a write set outgrows memory, TXN-03 may spill it to a scratch file. Such
  a file is never named by the manifest and is ignored by recovery.

**WT5. First-write version.** When an operation adds an entry *e* to *T*'s
write set, *fw(e)* is the *L* that operation read (RD4). A batch of writes
applied in one operation adds one entry for each key the batch ends up
holding (TXN-08, GM0), and all of them get the same value. The moment that
counts is when the engine applies the write to the transaction, not when the
caller started to compute it (16.2).

The first-write version of a record key *K* in *T* is the smallest *fw(e)*
over the entries of *T*'s write set that cover *K*. An entry that a savepoint
or scope rollback has removed no longer counts (WT8).

### 7.2 Reading

**WT6. A write transaction reads by RD3 to RD6**, through any of its scopes
(SC4).

### 7.3 Rebase: when a commit lands while the transaction is open

While no commit has been published since *B(T)*, WT3 gives RD3 for free:
*B(T)* is *L*. A foreign commit breaks that, and the engine has to restore
it.

**WT7. Before an operation of *T* reads or writes, *B(T)* must be the *L*
that operation reads.**

- If *T*'s write set is empty, there is nothing to do: *T* has no private
  frames, and *B(T)* is simply *L*.
- Otherwise *T* is **rebased**: its private frames are rebuilt so that WT3
  holds again over the new *L*. They then show *T*'s write set applied, in
  order, to *S(L)*. As a set of records that is exactly the overlay of RD3
  and RD5. Before it gives up the old base, *T* takes its own copy of every
  committed byte its write set still refers to (WT3). A rebase runs while no
  other operation of *T* is running.

Whether those records still form a proper tree depends on whether a foreign
commit wrote a key that *T* had already written. Let the **overlap set** of
*T* be the set of pairs CF2 names: a write-set entry *e* of *T*, and a
foreign commit *c* with *v(c)* > *fw(e)* whose key set meets *e*.

**WT7a. Empty overlap set: the rebased state is an ordinary state, and *T*
can commit it.** No Merk that *T* wrote has been changed by a foreign commit
since *T* first wrote it (GM3). So for writes that propagate to the root
(section 14.2) the records form a well-formed tree, and after *T* commits
the database passes the physical invariant checker (VER-07). The pages may
differ from the ones *T* had before the rebase. The records may not.

This case is real in iteration one. Example: *T* has written aux key `x`,
and a write with no transaction commits aux key `y`, which lives on the same
flat page. *T* must then read `y`, keep `x`, and commit both (case B16).

**WT7b. Non-empty overlap set: the records may no longer fit together, and
the engine serves them exactly as they are.** *T* reads its own records
where it wrote them and the committed records elsewhere:

- a node *T* wrote may carry a child hash that is older than the committed
  child it now leads to
- a link in a node *T* wrote may name a child key that no longer exists.
  Fetching it reports the node as missing, as a RocksDB `get` returning
  nothing does today
- a cursor over a Merk lists the committed keys of that subtree merged with
  *T*'s own, whether or not *T*'s root reaches them

The engine must not repair, reorder or refuse this state. Mainnet blocks
32326 to 32328 were executed in it (P-09), and VER-08 replays them: the root
hash and the costs computed through *T* have to be the ones RocksDB
computed. A transaction in this state fails at commit (CF2), unless a
savepoint rollback empties its overlap set first (WT8).

**WT7c. No speed target applies to a rebase.** In Platform a foreign commit
during the block transaction is a bug that was fixed at height 32329. The
targets in the plan are for the case *B(T)* = *L*.

How to rebase is TXN-03's. One way that meets WT7a and WT7b: keep, beside
the frames, an index of the write set by record key. On a foreign commit,
drop the private frames. With an empty overlap set, apply the write set again
over the new *L*. With a non-empty one, answer every fetch by key: from the
index if *T* wrote the key, and otherwise by descending the committed tree of
that subtree, which is well formed, from its committed root.

### 7.4 Savepoints and rollback

**WT8. A savepoint is a mark in the write set** (TXN-04). Savepoints form a
stack.

- **Roll back to the newest savepoint** removes every write-set entry and
  every pending mark added after it, and pops it. Entries from before it
  keep their first-write versions. A key first written after the savepoint is
  no longer written by *T* at all: it leaves *T*'s reads (RD3) and the
  conflict check (CF4), as in RocksDB (cases A4, A5).
- What *T* reads afterwards is its remaining write set over the *L* of that
  moment (WT7), not the state the database had when the savepoint was set
  (case A4b).
- **Pop** removes the newest mark without undoing anything. The writes after
  it now belong to the savepoint below it.
- Rolling back with no savepoint set fails with a typed error
  (`NoSavepoint`) and changes nothing (case A17).
- A savepoint can be set only while no scope is open (SC8).
- Savepoints still on the stack at commit or drop are ignored (P-12).

**WT9. Rollback empties the transaction and leaves it usable.** The write
set, the first-write versions, the pending marks, the savepoints, the private
frames and the version pin all go, and page ids it allocated return to the
allocator. No version is taken. Writes made through it afterwards start
fresh, with new first-write versions (case A14). Dropping a write transaction
without committing it is a rollback (P-07, P-13).

**WT10. An operation that fails adds nothing.** If an operation through *T*
returns an error, *T*'s write set is what it was before the call.

**WT11. A write transaction can be held for any length of time, across
threads** (P-07). It is shared between threads; writes go through scopes
(section 9).

## 8. Commit

### 8.1 Sequence

**CM1. What must be true before commit starts.** If either of these fails,
commit returns a typed error, and the transaction stays open and unchanged:

- no scope of the transaction is open (`ScopesOpen`, SC8)
- the transaction has no pending mark (`NotFinalized`, SC9)

**CM2. One commit at a time.** Commit holds the **commit lock** from step 2
to step 8 of CM3. Besides commits, only the sampling step of a checkpoint
takes it (CK2). Readers, writers in other transactions, the flusher and the
cleaner never wait for it.

**CM3. The steps of a commit**, in order:

1. If the write set is empty, return success (CV2).
2. Take the commit lock. *L* is now fixed until step 8.
3. Evaluate the conflict rule (CF2) against that *L*. On a conflict, release
   the lock and return `Conflict`.
4. If *B(T)* is not *L*, rebase (WT7a; the overlap set is empty, or step 3
   would have failed).
5. Evaluate every hash that is still dirty (TREE-04). After this step no
   stored hash differs from its recomputation.
6. Take version *v* = *L* + 1 and do all the file I/O of the commit: append
   one log group for *v* (new values, structure deltas, one record per
   changed hash, flat records and range deletes; STORE-01), and then, if the
   sync policy asks for an fsync at commit, that fsync. This is the only
   step that touches a file and the last that can report an error. If it
   does, CM9 decides whether the commit happened.
7. Install, in memory only: turn the private frames into page versions born
   at *v*, mark the page versions they replace as superseded at *v*, and
   retire the page ids the commit frees. This step writes to no file and
   cannot fail. None of it is visible yet: no reader looks at a page version
   born after the version it reads (PV1).
8. Publish: store *v* into *L*. Record the commit's key set for the conflict
   checks of the other open write transactions (CF3). Release the lock.

Hashes may be evaluated before the lock is taken, to keep it short. Step 5
finishes whatever is still dirty then.

**CM4. Step 8 is the commit point.** A pin taken before it sees none of the
commit. A pin taken after it sees all of it. There is no moment at which a
handle sees part of a commit.

**CM5. How a commit can end.**

| Outcome | The transaction | The database |
| --- | --- | --- |
| success | ended | *L* is *v* |
| `ScopesOpen`, `NotFinalized` | still open, unchanged | unchanged |
| `Conflict` | ended, its writes discarded | unchanged |
| a storage error that leaves the group out of the log (CM9), for example no space | ended, its writes discarded | unchanged and still readable; no recovery ever brings the commit back (OPS-08) |
| `NotDurable`: the group is in the log (CM9), but the fsync that the sync policy asked for failed | ended | *L* is *v*; *v* is not known to be durable (DU1) |
| unsettled: the engine could not find out whether the group is in the log (CM9) | ended | unchanged in this session, which accepts no further commit; the next open settles it (DU2) |

A crash between steps 6 and 8 can leave a complete log group for *v*.
Recovery then includes *v* (DU2), though the caller never saw success. No
crash leaves part of *v*.

**CM6. A commit ends the transaction whether it succeeds or not**, except
for the two precondition errors. This is what today's `commit_transaction`
does: it takes the transaction by value.

**CM7. Commit does not fsync** unless the sync policy says so (DEC-04,
section 12). Where it does, the fsync is part of step 6 of CM3, after the
append and before anything is installed or published.

**CM8. Commit waits only for the commit lock and for flusher backpressure**
(FL5). It never waits for a reader, for another open write transaction, for
the cleaner or for the creation of a checkpoint beyond its sampling step.

**CM9. The log decides whether a commit happened, and the engine must agree
with it.** Recovery takes a commit exactly when it finds its group in the
log (DU2). The engine uses the same test. The group of *v* is **in the log**
when, read back from the log file as it is now, it passes recovery's own
test: it decodes, its checksum verifies (FMT-05, format.md C7), and it lies
in the part of the file that recovery reads (DU2).

Four things hold at all times:

- (a) *v* is published only if its group is in the log. When the file layer
  accepts every byte of the group without an error, that is established, and
  nothing is read back.
- (b) A commit reported as failed has no group in the log and never gets
  one. So no recovery, at any later time, brings it back.
- (c) Nothing that fails recovery's test lies before the group of a
  published commit in what recovery reads. So a published commit can be lost
  only by losing its own bytes (DU1), never by being out of recovery's
  reach.
- (d) When the engine cannot tell whether a group is in the log, it says so
  and publishes nothing more.

When step 6 of CM3 reports an error, these give one procedure:

1. The engine finds out whether the group is in the log. If the error came
   from the fsync, it is: the append had returned cleanly. If it came from
   the append, the engine applies the test, reading the group back unless
   the file layer's answer already shows that bytes are missing. A group of
   full length whose bytes do not verify is not in the log.
2. **In the log: the commit has happened.** Steps 7 and 8 run. Commit
   returns `NotDurable` if it was the fsync that failed, and success
   otherwise. With the default policy there is no fsync at commit (DEC-04).
3. **Not in the log: the commit has failed.** Nothing was installed (step 7
   has not run), *L* is unchanged, no page version born at *v* exists, and
   the transaction is ended as by WT9. Nothing ever completes the group, so
   (b) holds from this moment, whenever a crash comes.
4. **Before the next commit is appended, the failed bytes are cut off.**
   Recovery stops reading a log file at the first group that fails its test,
   and the next commit will carry the same version *v*. So, for (c), the
   engine ends that log file at the end of its last good group, records that
   length durably where recovery reads it (a closed entry in the manifest,
   `segment-manifest.md` M15), and goes on in a new file. STORE-01 and
   OPS-08 choose the mechanics.
5. While it cannot do that, for example because the disk is still full, the
   engine accepts no commit. It resumes when it can. Every read keeps
   working throughout.
6. If the engine cannot apply the test, because the read-back itself fails,
   the outcome is unsettled (d). Commit returns an error that says so, the
   session accepts no further commit, and the next open settles the matter
   by DU2. This is the only case in which a commit that returned an error
   can turn out to have happened, and the error names it.
7. Whatever else the transaction wrote to data files is dead space (FL1).

### 8.2 The conflict rule

This is the rule RocksDB's optimistic transactions apply, stated over record
keys. Section 16 derives it from the RocksDB source and from runs against
the RocksDB engine.

**CF1. Every commit has a key set**: the record keys its writes put or
delete, the ranges its range deletes cover, and the ranges it loads in bulk
(CF5). A key or range written twice in one commit counts once.

**CF2. The rule.** A commit of *T* fails with `Conflict` exactly when there
is an entry *e* in *T*'s write set and a foreign commit *c* such that

- *v(c)* > *fw(e)*, and
- the key set of *c* meets the keys of *e*.

Two keys meet when they are the same record key. A key meets a range that
contains it. Two ranges meet when they share a key (ST4). In all other cases
the commit does not fail with `Conflict`.

In words: the commit fails when someone else committed a write to a key
after *T* first wrote that key. It does not matter that *T* wrote the key
again later (case A9). It does not matter how long *T* has been open: a
commit that landed before *T* first wrote the key is not a conflict, and *T*
then built on it (cases A2, B2).

**CF3. Enough must be remembered to decide CF2 exactly.** While a write
transaction is open, the engine must be able to evaluate CF2 for it against
every commit published since its oldest first-write version. It never
answers "cannot tell". One way that needs no history: when a commit is
published, compare its key set with the write set of each open write
transaction and remember the entries it meets. A transaction's commit fails
if anything is remembered for it. Entries that a rollback removes are
forgotten with it (CF4).

**CF4. What changes the outcome, and what does not.**

| Does not matter | Because |
| --- | --- |
| what *T* read | reads are not tracked. This is not serializability (case A3b). |
| whether the foreign write changed the value | a write is a write (ST5; cases A7, A7b) |
| who made the foreign commit | a write with no transaction, another write transaction and a wipe count alike (cases A1, A1b, A13) |
| a foreign write to the same subtree and key in another column | the column is part of the key (case A6) |
| when *T* was started | only first-write versions count (cases A2, A15) |

| Does matter | How |
| --- | --- |
| rolling back to a savepoint | the entries it removes leave the rule. An entry from before the savepoint stays, with its first-write version (cases A4, A5, A5b, A16). |
| rolling back a scope | the same (SC6) |
| rolling back the whole transaction | everything leaves (case A14) |

**CF5. Ranges.** A range delete inside *T*, such as a subtree drop (CV9), is
one write-set entry with its own first-write version. A range delete in a
foreign commit meets every entry of *T* whose keys lie in the range. Wipe is
a commit whose key set is everything, so it meets every entry.

A bulk load into a subtree, inside *T* or outside it, may be counted for
this rule as one write of the range "that subtree's column", in place of one
entry per loaded key. What is read back under those keys is the loaded
records (RD3).

**CF6. `Conflict` is one typed error, used for this and for nothing else.**
GROVE-01 maps it to the engine-neutral conflict error of SEAM-04, the one
RocksDB's `Busy` also satisfies. A commit that fails for another reason must
not return it, and a conflict must not be reported as anything else.

**CF7. A second write transaction is the same rule.** TXN-05 says "a second
write transaction fails at commit with a conflict". Read through CF2: of two
write transactions open together, the one that commits second fails exactly
when it had written, before the first committed, a key that the first also
wrote. If it writes its keys only after the first has committed, or the two
key sets do not meet, both commit (cases A13, A13b, B9, B10).

**CF8. A write with no transaction can conflict too.** It is a write
transaction (RD7). Its commit fails only if another thread commits a write to
one of its keys between its write and its commit.

**CF9. Where the conflict rule differs from RocksDB.** In each case RocksDB's
own result depends on the state of its memtables, which differs from node to
node, so no consensus result can depend on it. Section 14.5 lists these
together with the differences that are not about conflicts.

| Situation | RocksDB | LigninDB | Reachable in Platform |
| --- | --- | --- | --- |
| The memtable history no longer reaches back to a first write | commit fails with `TryAgain`, conflict or not (case A11b) | decides CF2 exactly; never an undecided error | only after hundreds of megabytes of foreign writes while one transaction is open |
| A foreign range delete covers a key *T* wrote | `Busy` only if a point entry for that key is still in a memtable or its retained history (cases A10 to A10d, B17) | always `Conflict` (CF5) | not by the call sites BASE-05 lists: the wipe starts a state-sync restore (P-22), and pending drops are flushed after the block commit and at startup (P-24) |
| Data loaded outside *T* under keys *T* wrote | never a conflict: file ingestion is invisible to the check (case A18) | `Conflict` (CF5) | no: devnet seeding loads a subtree under which *T* has written nothing (P-30) |

LigninDB is stricter in the last two because the alternative is to commit
*T*'s older records on top of a range that was deleted or loaded under it,
which can leave a tree that no longer hashes to its root.

## 9. Write scopes: several threads in one transaction

DEC-03: inside one write transaction, several threads may write different
Merks at once. Iteration one uses one thread and one scope at a time. The
rules are the same for one scope and for many.

### 9.1 Scopes and claims

**SC1. Every write of a transaction goes through a scope.** A scope belongs
to one transaction and is used by one thread at a time. A transaction may
have any number of scopes open. How scopes map to GroveDB operations is
GROVE-01's choice in iteration one and GROVE-12's in iteration two.

**SC2. The unit of exclusion is a column of a subtree**: the pair (column,
subtree) of ST1. A Merk is the unit (`data`, its prefix). The aux records of
the root subtree are one unit.

**SC3. A scope claims the units it touches, and a claim never waits.**

- The first write of a scope to a unit takes a **write claim** on it. The
  first read takes a **read claim**.
- Read claims on one unit by several scopes are compatible. A write claim is
  compatible with nothing held by another open scope.
- If a claim cannot be granted, the operation fails at once with
  `ScopeOverlap`. It has no effect, and the scope stays open and usable.
- A scope keeps its claims until it ends. A read claim becomes a write claim
  when the scope writes, if no other scope holds a claim on that unit.
- A read through the transaction outside any scope is treated as a read by a
  scope that ends with the call: it fails with `ScopeOverlap` if another
  scope holds a write claim on the unit.

Read claims are what make the result independent of timing: without them, a
scope could read a Merk before or after another scope's writes to it were
merged.

**SC4. A scope reads its own writes, then the transaction's merged writes,
then the committed state**, each by RD3 to RD6. It never sees the writes of
another scope that is still open.

**SC5. A scope's writes get their first-write version when the scope makes
them**, not when it merges (WT5).

**SC6. A scope ends in one of two ways.**

- **Merge**: its writes and pending marks become the transaction's. From
  then on savepoints, rollback and commit treat them like any other write of
  the transaction.
- **Rollback**: its writes, the first-write versions they introduced, the
  pending marks it added and the page ids it allocated are removed. The
  transaction is as if the scope had never been opened. This is what a
  failed state transition needs.

Either one is a single step for every other scope: no scope sees a merge or
a rollback half done. Merges of one transaction happen one at a time.

**SC7. The result does not depend on the interleaving.** For scopes that end
by merging, the transaction's records, every hash, and every `OperationCost`
charged inside each scope are the same for every interleaving of their
operations, and equal to running the scopes one after another in any order.
SC3 guarantees it: two scopes open at the same time have not written
anything the other read or wrote. A foreign commit is an input like any
other: what a scope reads of it depends on when it lands (RD4), with one
scope or with many.

What is not promised is physical identity. Page ids, slot numbers and file
offsets may depend on which scope allocated first. TXN-09's "same pages"
means the same logical content.

**SC8. Savepoints and commit need all scopes closed.** Setting a savepoint,
rolling back to one, and commit fail with `ScopesOpen` while a scope is open
(TXN-04, TXN-05). A rebase (WT7) may happen while scopes are open: their
writes are in the write set, under their own marks.

**SC9. Pending marks.** The engine does not know GroveDB elements, so it
cannot tell whether every changed Merk has been carried into its parent. The
transaction therefore keeps a set of **pending marks**: opaque (subtree, key)
pairs that a scope adds and removes. Commit fails with `NotFinalized` while
the set is not empty (CM1). A mark is not a claim: adding one on a parent's
element does not claim the parent's Merk, so scopes writing two children of
one parent do not overlap. Marks are undone with the scope or savepoint that
added them.

On existing GroveVersions every write propagates to the root before it
returns, and no mark is ever set. From iteration two, a write that stops at a
stop point marks its parent element, and finalize removes the mark when it
has updated that element (GROVE-11).

### 9.2 What must be finished before commit

1. Every scope is closed (CM1, SC8).
2. No pending mark is left: the tree is finalized up to the GroveDB root
   (CM1, SC9).
3. Every dirty hash is evaluated. Commit does this itself (CM3 step 5); the
   caller does not have to.

Savepoints may remain (WT8).

### 9.3 Latching

A claim settles who may write a Merk. Pages are a different matter: the
cells of two Merks can share a page (TREE-01).

**LT1. Sharing a page is invisible.** No operation of a scope fails, waits
for another scope to end, or returns a different result because of what else
lives on the pages it uses. Only a claim (SC3) can make one scope lose to
another.

**LT2. Bytes behind a view do not change.** While a node view handed to a
caller lives (TREE-02), the bytes it borrows are not modified, by its own
scope or by any other.

**LT3. A latch is short, ordered and never visible to the caller.** A latch
protects the bytes of one frame during one physical step.

- No latch is held when an operation returns to its caller, and none is held
  across a claim (which never waits anyway).
- An operation that needs several latches takes them in ascending page id
  order. If it finds it needs a lower id than one it holds, it releases and
  starts that step again.

So a scope waits for another scope for at most one physical step, and scopes
cannot deadlock.

**LT4. A private frame has one writer at a time.** LT2 and LT3 together
leave no room for two open scopes to modify the same private frame while
either holds a view into it. So when the Merks of two open scopes share a
page, each scope works on its own private copy of that page, and the copies
are reconciled when the second scope merges, by applying that scope's writes
again on the merged page. That is the same step as a rebase (WT7a), and it is
why SC7 can promise the result of running one scope after the other. TREE-01
may instead guarantee that Merks written by different scopes never share a
page, in which case no reconciliation is ever needed.

**LT5. A page is made private once per owner.** When two operations of the
same owner (a scope, or the transaction during a merge) need a private copy
of the same committed page at the same moment, exactly one copy is made and
both use it.

**LT6. Readers take no latch.** A snapshot reads published page versions,
which never change (PV2). It holds an operation guard (PN5) and frame pins,
and nothing a writer can make it wait for.

**LT7. Lock order.** Where an implementation has these locks, a thread
acquires them only in this order, and never waits for an earlier one while
holding a later one:

1. the commit lock (commit, wipe, checkpoint sampling)
2. a transaction's own lock (rebase, scope merge)
3. the file-set lock (manifest replacement, file deletion, checkpoint
   sampling)
4. frame latches, in ascending page id order
5. the internal locks of the page table and the allocator, which are held
   for one step and under which nothing else is acquired. The page step of
   PV7 is taken here.

## 10. The flusher, the cleaner and page ids

### 10.1 Flusher

**FL1. The flusher writes only published page versions.** It never reads or
writes a private frame. Bytes of an uncommitted transaction may reach a data
file only as dead space: nothing committed points to them, and recovery
ignores them. While the transaction is open they are its own: no live-value
count includes them yet (`value-log-format.md` O7), so the file that holds
them is not deleted, and they are not reclaimed, until the transaction has
committed or rolled back. After a rollback the cleaner reclaims them.

**FL2. What it writes keeps its born version** (PV5).

**FL3. Write-back and consolidation never destroy a needed page version.**
Writing a new base image for a page supersedes the old chain physically.
The old chain stays live while a needed page version (PV3) consists of it.
The flusher may fold several commits' deltas of one page into one write only
if no pinned version falls between them.

**FL4. Nothing newer than the log survives a crash.** After a crash, no page
version born after the recovered version *Lr* may be reachable (DU3). Either
the flusher makes the log durable through *v* before a table checkpoint or
manifest that reaches page versions born at *v* becomes durable, or recovery
discards page versions born after *Lr* by their born versions. STORE-06 and
STORE-07 choose.

**FL5. Backpressure stops commits only between commits.** When write-back
falls behind (STORE-06), the committer waits before step 2 of CM3, not while
it holds the commit lock. Readers are never slowed by backpressure.

**FL6. A log file is deleted only when nothing in it is the only copy** of a
needed page version, or of a commit that the last durable table checkpoint
does not cover.

### 10.2 Cleaner

**CL1. A block is live while a needed page version consists of it.** A
segment block (a base image or a delta) and a value-log entry are live if a
needed page version (PV3) is built from them or points to them. That covers
the page versions visible at *L* and those visible at any pinned version.
Everything else is dead.

**CL2. Relocation is not a commit.** The cleaner copies live blocks to a new
file and switches their locations in the page table. No page version changes
(PV4), no version is taken, and a reader at any version reads the same
content before and after.

**CL3. A file is deleted in two steps.** It is retired when it holds no live
block, and deleted under PN6. It is also not deleted while a checkpoint being
created has it in its file set (CK3). The cleaner either relocates blocks
that only a pin keeps live, or waits for the pin: STORE-08 chooses.

**CL4. Relocation loses to a commit.** If the block being relocated is
superseded while it is copied, the switch of locations must fail and the copy
is dead. The cleaner never brings a superseded block back as current.

**CL5. Value-log cleaning follows PV4, PV6 and PV7.** Moving a value
rewrites the pointer in its owning cell, in a page step (PV7). If the
owner's page was superseded, or copied by an open write transaction, since
the cleaner looked, the move is abandoned and tried again later.

The old entry stays live while any needed page version still points to it.
So a file that such moves emptied while *L* was *v* is deleted only when no
version before *v* is pinned (`value-log-format.md` O8) and PN6 allows it.
It is also deleted only once the rewrites that emptied it, and the copies
they point to, are durable: otherwise a crash could bring back pages that
point into a file that is gone.

**CL6. A dropped subtree's pages are reclaimed when no pin can reach them.**
The drop unlinks the subtree in the commit that drops it, at version *w*
(TREE-09). Its pages are retired at *w* and reclaimed under PN6, that is,
once *H* ≥ *w*.

**CL7. The cleaner is never in a reader's or the committer's way** beyond
one step on a page-table entry (LT7 level 5).

### 10.3 Page ids

**ID1. An id allocated by an open transaction is private** until it commits
(WT4).

**ID2. A freed id is reused only when no pin can reach the old page.** A
page freed by the commit of version *w* was reachable at versions below *w*.
Its id may be handed out again once no version below *w* is pinned, that is,
once *H* ≥ *w* (STORE-10). This is what "a page id identifies a page only
together with a version at which it is reachable" (format.md 3.1) needs.

**ID3. Reuse is also held back by operation guards** (PN6): an operation that
read the id before it was freed may still be using it.

## 11. Checkpoints

**CK1. A checkpoint is exactly one committed version.** It holds *S(vc)* for
the *vc* that was *L* at its sampling step: every commit up to *vc*, nothing
of any later commit, and nothing uncommitted.

**CK2. Sampling is one step.** The checkpoint records *vc*, the manifest it
starts from, the set of live files and the valid length of each, while
holding the commit lock and the file-set lock. No commit is published, no
manifest is replaced and no file is deleted during that step. The log length
it records ends exactly at the group of *vc*.

**CK3. From sampling until its files are linked, a checkpoint holds a
version pin on *vc* and its file set.** No file in the set is deleted in
that time (CL3). Both are released when creation ends, whether it succeeded
or not.

**CK4. It makes *vc* durable.** The files are fsynced up to their recorded
lengths before they are linked (TXN-06), so *vc* is durable in the live
database as well (DU1).

**CK5. Commits go on after sampling.** Bytes appended to a shared file past
its recorded length are not part of the checkpoint: its manifest gives each
file's valid length. A commit never waits for the fsync or the linking.

**CK6. A catch-up by the flusher before sampling is ordinary write-back**
(FL1 to FL5). It changes no version.

**CK7. Afterwards the checkpoint is a database of its own.** It has its own
directory, lock, *L* (starting at *vc*, format.md 3.2) and pins. It holds no
pin and no file hold in the live database. The two share files only as hard
links, and neither changes a shared byte: files are append-only, and a file
from an earlier session is never appended to (format.md B11, STORE-07). So
"once no snapshot or checkpoint needs them" in STORE-08 means, for a
checkpoint, the hold of CK3: a finished checkpoint needs nothing from the
live directory.

**CK8. Open write transactions are not part of it and are not disturbed.**
A checkpoint does not wait for them.

**CK9. Two checkpoints at once are independent.** Each samples its own *vc*.

**CK10. A failed creation leaves the live database untouched.** The target
directory may be incomplete. Removing it is the caller's job (P-18).

## 12. Durability and recovery

**DU1. Published is not durable.** A commit is visible from its commit point
(CM4) and durable only once the log is fsynced through it: by `flush()`, by a
checkpoint (CK4), or by the sync policy (STORE-01).

**DU1a. `flush()` has a target version.** `flush()` reads *L* once, after it
has been called. That value is its target *F*. When `flush()` returns
success, every commit up to *F* is durable: a crash at any later time
recovers to a version at or above *F* (P-05). A commit published after *F*
was read may or may not be covered. If the fsync fails, `flush()` returns the
error and promises nothing.

No lock is needed for this. A commit appends its whole log group before it is
published (CM3 steps 6 and 8), so when *F* is read every group up to *F* is
already in the log, and `flush()` only has to fsync the log files that hold
them. It holds an operation guard on those files (PN5), so none is deleted
under it, and it makes no reader, writer or committer wait. A checkpoint
running at the same time fsyncs for itself (CK4); the two do not need an
order.

**DU2. After a crash the state is one committed version.** Recovery lands on
*S(Lr)* for a single *Lr*, with *Ld* ≤ *Lr* ≤ *Lc*, where *Ld* is the newest
version known durable before the crash and *Lc* the newest version whose
group was in the log, in the sense of CM9, when the crash came. Never part
of a commit, and never a mix of two.

Recovery reads a log file that the manifest names as closed only up to its
valid length, and an open one up to the first group that fails its test
(`segment-manifest.md` M15; CM9, DU3). A group beyond that point does not
count, whatever its bytes.
Within what recovery reads, every group that passes is a commit the engine
had treated as happened (CM9), or one it had not yet answered for.

**DU3. Nothing else survives.** No uncommitted write, no private frame, no
pin, no scope and no page version born after *Lr* is reachable after
recovery (FL4).

Version numbers above *Lr* are assigned again (CV6). So that this is safe,
recovery must have durably recorded, before the first commit of the new
session is published, where each file of the earlier session stops being
valid (`segment-manifest.md` M15 has recovery name those files as closed,
with their valid lengths). No later recovery may then replay a log group, or
reach a page version, that the earlier session wrote for a version above
*Lr*, even if its bytes are complete on disk. The same holds for a failed
commit within a session (CM9).

**DU4. Recovery runs alone.** No handle exists and no background actor runs
until it has finished, so it is in no interleaving.

## 13. Every pair of actors

The actors are the six of section 1. "Writer" is a thread in a scope of an
open write transaction; its writes are uncommitted. "Committer" covers every
commit: of a write transaction, of a write with no transaction, and wipe.

Each row names what the two share and the rules that settle it. "Waits" says
whether either can be made to wait for the other.

| # | Pair | What they share | Rules | Waits |
| --- | --- | --- | --- | --- |
| 1 | Reader, reader | published page versions | PV2, LT6 | never |
| 2 | Reader, writer | nothing: the reader cannot reach private frames | RD8, WT4 | never |
| 3 | Reader, committer | *L*, and the page versions the commit supersedes | CM4, PN4, PV1, PV3 | never |
| 4 | Reader, flusher | frames and locations of page versions | PN5, PV4, FL3 | never |
| 5 | Reader, cleaner | blocks, files and page ids the reader's version needs | PN2, PN6, CL1 to CL5, ID2 | never |
| 6 | Reader, checkpoint | files | PN5, CK7 | never |
| 7a | Writer, writer in the same transaction | units, private frames, the transaction's merged writes | SC3, SC4, SC6, SC7, LT1 to LT5 | one physical step (LT3); a claim never waits |
| 7b | Writer, writer in another transaction | nothing until one commits | RD8, WT1, WT4 | never |
| 8a | Writer, commit of its own transaction | the write set | CM1, SC8 | commit is refused while a scope is open |
| 8b | Writer, a foreign commit | *L*, the keys both wrote, and locations the writer kept | RD3, RD4, WT7, WT7a, WT7b, PN8, CF2, CF3 | never; the writer rebases at its next operation |
| 9 | Writer, flusher | the committed pages a writer has copied, and the bytes it wrote to data files ahead of its commit; never a private frame | FL1, WT4, PV7 | never; the flusher leaves a copied page's values where they are |
| 10 | Writer, cleaner | the committed pages under the writer's frames, their value pointers, page ids, locations the writer kept, and the bytes it wrote ahead of its commit | PN1 (base pin), PN5, PN8, PV4, PV7, WT3, FL1, CL5, ID1, ID2 | never; the cleaner leaves a copied page for later |
| 11 | Writer, checkpoint | nothing: uncommitted writes are not captured | CK1, CK8 | never |
| 12 | Committer, committer | *L*, the log, versions | CM2, CV4, CV5, CF2, CF7, CF8 | the second waits for the commit lock |
| 13 | Committer, flusher | the log, the page versions just born, and pages whose values the flusher regroups | CM3 steps 6 and 7, CM9, FL1, FL4, FL5, FL6, PV7 | the committer, before it takes the lock (FL5); otherwise one page step |
| 14 | Committer, cleaner | page-table entries; pages and ids the commit frees; pages whose value pointers the cleaner rewrites | CL4, CL5, CL6, PV7, ID2, PN6 | one step on a page-table entry |
| 15 | Committer, checkpoint | *L*, the log's length, the file set | CK2, CK5, LT7 | each waits for the other's hold on the commit lock; the checkpoint holds it only for sampling |
| 16 | Flusher, flusher | the pages being written back | PV4, FL3, LT7 level 5 | one step on a page-table entry |
| 17 | Flusher, cleaner | page-table locations, segment files, the manifest | PV4, FL3, CL2, CL4, LT7 level 3 | the file-set lock |
| 18 | Flusher, checkpoint | the manifest and the open files | CK2, CK3, CK6 | the file-set lock, during sampling |
| 19 | Cleaner, cleaner | blocks and files | CL3, CL4, PN6 | the file-set lock |
| 20 | Cleaner, checkpoint | the file set | CK2, CK3, CL3 | file deletion waits for CK3 |
| 21 | Checkpoint, checkpoint | the commit lock and file-set lock during sampling | CK2, CK9 | sampling only |

Six actors give 21 unordered pairs, counting each actor with itself. Rows 7
and 8 are split because "writer" and "committer" each cover two cases that
different rules settle.

**Why pairs are enough.** Every rule cited is an invariant on shared state
that holds whatever else is running: a page version never changes (PV2);
nothing needed is reclaimed (PN6, PV3); only the commit lock orders commits
(CM2); only claims order scopes (SC3). No rule has the form "unless a third
actor is also running". The one rule that involves three parties, CF2, is
stated over all foreign commits at once. Section 15.2 still lists combined
scenarios for the stress tests, because an implementation can break an
invariant only under a particular schedule.

**Events that are not actors.** These happen on a writer's or a caller's
thread and are covered by the rules of that thread's row:

| Event | Rules |
| --- | --- |
| rollback of a transaction, rollback to a savepoint, pop | WT8, WT9, CF4 |
| scope merge and scope rollback | SC6 |
| rebase | WT7 |
| a commit that fails | CM5, CM9 |
| `flush()` | DU1a; it is an fsync and takes no lock that a reader, a writer or the committer needs |
| wipe | CV3, CF5; it is a commit, so a snapshot taken before it keeps reading its own version (RD1) |
| subtree drop | CV9, CF5; reclaiming its pages is the cleaner's (CL6) |
| crash and recovery | section 12 |

## 14. How GroveDB's use of the RocksDB engine maps onto these rules

The engine sees record keys. GroveDB callers think in elements and paths.
This section connects the two, for the GroveVersions that exist today, and
then lists every place where LigninDB deliberately differs from the RocksDB
engine (14.5).

### 14.1 Which keys a GroveDB write writes

A GroveDB operation that changes an element at path *p* writes, in one
operation of its transaction:

- in the Merk at *p*: the node of the element, every node on the way from it
  to that Merk's root, and every node a rotation touched. All are rewritten
  because their hashes or links changed. A removed node is a delete.
- in each ancestor Merk, up to the root Merk: the node of the element that
  holds the child subtree (its root key, hash or aggregate changed), and
  every node from it to that Merk's root.
- in `roots`: the base root key, if the root Merk's root key changed.
- for the append-only trees: their flat entries under their own prefix, and
  then the same chain of ancestors.

Other operations write only what they name: `put_aux` and `delete_aux` one
`aux` key, the pending-drop record one `meta` key.

These enter the transaction when the operation applies its storage batch,
not while Merk computes them (WT5). On RocksDB that is the call to
`commit_multi_context_batch` with the transaction. Most operations apply one
batch, at their end; the append-only trees apply their flat entries first
and the tree changes after.

**GM0. What an operation writes is what its storage batch ends up holding.**
A `StorageBatch` is a map from key to the final put or delete of that key,
not a list of everything Merk did (TXN-08). The write-set entries of an
operation are those final entries, one per key. First-write versions are
tracked over them, not over Merk's steps.

- A key that Merk deleted and put again inside one batch is one entry.
- An operation that applies an empty batch adds nothing to the write set. It
  can neither cause a conflict nor suffer one. That is the case for
  `insert_if_not_exists` on a key that exists (case B18),
  `insert_if_changed_value` with an equal element (case B20), an operation
  that fails before it applies its batch (case B22), and every read.
- A plain `insert` writes even when the element equals the stored one (case
  B19). So does a delete (case B21).

One read in today's write path does not go through the transaction: when a
delete carries no cost information, `continue_write_batch` reads the
record's size from the committed state, not from the transaction's own
writes. On LigninDB that is a read with no transaction at that moment (RD2).
The cost model (BASE-06, SEAM-05) says when it happens.

### 14.2 What follows

**GM1. Any two tree changes share a key.** Every operation that changes the
tree rewrites or deletes the node that is the root of the root Merk when it
runs. If the root Merk is empty, it writes the base root key. Take *T*'s
earliest tree change that no rollback has removed: it wrote the committed
root of that moment. The first tree change committed outside *T* after it
wrote the same key.

**GM2. So, on existing GroveVersions:** if *T* has made a tree change that no
rollback has removed, and any tree change is then committed outside *T*,
*T*'s commit fails with `Conflict`. It makes no difference how far apart the
two paths are (case B1). If the outside commit comes first, and *T* makes its
first tree change after it, *T* commits (case B2).

**GM3. A Merk that *T* wrote and that a later foreign commit changed is in
the overlap set.** The argument is GM1's, applied to that Merk: *T*'s first
write to it rewrote or deleted its committed root node, and so did the first
foreign commit that changed it afterwards. This holds in iteration two as
well, because a write always rewrites its own Merk up to that Merk's root.
It is the fact WT7a relies on.

**GM4. Aux and tree writes do not meet.** A transaction that wrote only aux
keys is not disturbed by a tree change outside it, and the reverse (cases B3,
B4). Two writers of the same aux key do meet (case B15).

**GM5. A transaction that wrote nothing cannot conflict** (cases A15, B5).

**GM6. Savepoints decide by what remains.** If every tree change *T* made
before the outside commit has been rolled back, *T* commits (case B6). If one
from before the savepoint remains, it does not (case B7).

Two kinds of write do not follow GM1, because they do not propagate to the
root: a state-sync restore, which writes nodes directly in its own
transaction, and the replacement for `ingest_subtree_sst` (GROVE-09), which
loads a subtree's records outside any transaction. CF2 applies to them
through the keys they actually write. `replace_subtree_root`, which follows
such a load, is an ordinary tree change: it rewrites the parent element and
propagates to the root, so GM1 and GM2 hold for it.

### 14.3 The mainnet case, step by step

At mainnet heights 32326 to 32328 the block transaction was open and had
written, a batch of vote deletions was applied with no transaction, and the
block went on (P-09). By the rules:

1. The deletions commit at once, as their own commit (RD7, WT1).
2. The block transaction's next reads see them, except where it had written
   the node itself (RD3, RD5). It is in the state of WT7b.
3. Its commit fails with `Conflict` (GM2).
4. The database keeps the deletions and none of the block's writes (CM5).

The same shape, small enough to check by hand, was run on RocksDB (case B8).
The database holds two empty trees `A` and `B` under the root. `root(x)` is
the root hash read through handle *x*. `H{...}` is the root hash of a
database that holds exactly those items.

| Step | Action | Observed |
| --- | --- | --- |
| 1 | *T* inserts `A/a1` | `root(T)` = `H{a1}` |
| 2 | a write with no transaction inserts `B/b1` | `root(none)` = `H{b1}`. `root(T)` is still `H{a1}`: *T* reads its own root node, which carries the old hash of `B` |
| 3 | *T* reads `B/b1` | found: *T* never wrote that node |
| 4 | *T* inserts `A/a2` | `root(T)` = `H{a1,a2}`, still without `b1` |
| 5 | *T* inserts `B/b2` | `root(T)` = `H{a1,a2,b1,b2}`: this write loaded `B` as committed and rewrote the nodes above it |
| 6 | *T* commits | conflict |
| 7 |  | `root(none)` = `H{b1}` |

GROVE-01 must give these seven results on LigninDB.

### 14.4 Iteration two

With stop points (GROVE-11) a write rewrites nodes only up to its own Merk's
root, and the ancestors are rewritten by finalize. The engine rule does not
change. Its reach does: GM1 no longer holds between a write and a foreign
commit that touch different Merks, until finalize runs. A transaction that
wrote Merk *M* is then not disturbed by a foreign commit to a sibling of *M*:
finalize reads the committed parent, adds *M*'s new root to it, and the
commit succeeds with both changes. A foreign commit to *M* itself still
conflicts (GM3). RocksDB is gone by then, so there is nothing to match;
GROVE-11 should state this where it defines finalize.

### 14.5 Every deliberate difference from the RocksDB engine

On existing GroveVersions LigninDB must behave as the RocksDB engine does.
These are the places where this document asks for something else. Each is
either a case in which RocksDB's result depends on node-local state, or a
case its callers are told not to rely on. The table is meant to be complete
for the reads, the commit outcomes and the atomicity that a caller of
GroveDB can observe. A further difference in those is a defect, of this
document or of the engine.

| # | What | RocksDB engine today | LigninDB | Why, and who can see it |
| --- | --- | --- | --- | --- |
| D1 | A read with no transaction | reads the latest state at each storage read | pins one version for the operation (RD2) | The result is one RocksDB can also give. Visible only when a commit lands during the operation. |
| D2 | Wipe | four range deletes, one column family after another, each its own write (`storage.rs:871-881`) | one commit (CV3) | P-23 and VER-04 ask for a wipe that a crash leaves whole or undone. On RocksDB a reader, a checkpoint or a crash between two steps sees some columns wiped. |
| D3 | Flat subtree drop | the drop's commit deletes the element and writes a pending-drop record. The subtree's records stay until `flush_pending_prefix_drops` deletes them: one range delete per prefix and column family, then one delete of the record, each its own write (`storage.rs:928-966,994-1005`) | the subtree's records are removed in the drop's commit (CV9). The flush checks liveness as today, reclaims the pages, which takes no version (CL6), and deletes each pending-drop record with a write with no transaction: one commit per record | TREE-09 unlinks at the drop. The records in question are unreachable from any element. They can be seen only by reading a dropped prefix directly, or by re-creating the dropped path before the flush, which callers are told not to do. |
| D4 | Commit when the memtable history is too short | `TryAgain` | CF2 decided exactly (CF9) | node-local |
| D5 | A range delete outside *T* over a key *T* wrote | `Busy` or success, depending on the memtables | `Conflict` (CF9) | node-local; not reached by Platform's call sites |
| D6 | Records loaded outside *T* under a key *T* wrote | success | `Conflict` (CF9) | devnet seeding only; not reached there |

Three things that might look like differences are not. A second write
transaction is allowed on both engines and conflicts by the same rule (CF7).
Both engines end the transaction when a commit fails (CM6). Version numbers
are internal to the engine: nothing above it sees how many a wipe or a flush
took.

## 15. What the tasks take from here

### 15.1 By task

| Task | Takes from here | Still its own |
| --- | --- | --- |
| SEAM-01, MERK-01 | ST6: a change set names exactly the node records Merk writes today. Fetch by link must be answerable by child key (RD5). A location kept in a link is checked before it is followed (PN8). | The trait and the change-set types. |
| SEAM-04, GROVE-01 | The four handle rows of section 4. `Conflict` maps to the engine-neutral conflict error (CF6). `ReadOnly` maps to `SnapshotReadOnlyTransaction`. A read with no transaction is a snapshot per operation (RD2), a write with no transaction a transaction per operation (RD7). The seven steps of 14.3. | How scopes map to operations in iteration one (SC1). |
| TXN-02 | RD1, section 5, PV1 to PV3, LT6. | The pin and epoch mechanism (PN4, PN6). |
| TXN-03 | RD3 to RD6, section 7, PN8, PV7. | Frame and write-set structures; how to rebase (WT7); how a stale location is detected (PN8); how the write set stays self-contained (WT3); spilling (WT4). |
| TXN-04 | WT8, SC8. | The undo marks. |
| TXN-05 | Section 8, including the failed commit (CM9). | The log group; how CF3 is kept. |
| TXN-06 | Section 11, DU1. | Linking, the copy fallback, the checkpoint manifest. |
| TXN-07 | Wipe is one commit over the range "everything" (CV3, CF5). `flush()` is DU1a. | The facade. |
| TXN-08 | A batch is one operation: one read of *L*, one first-write version, one entry per key it ends up holding (RD4, WT5, GM0). | Batch semantics. |
| TXN-09 | Section 9. | Claim tables, merge, the scope's undo. |
| TREE-01 | LT1, LT4: what must hold when two Merks share a page. | Whether they may, and placement. |
| TREE-06 | RD6. | The cursor. |
| TREE-09 | A drop is a range delete in its transaction's write set (CV9, CF5); its pages are reclaimed under CL6. | The unlink. |
| STORE-04 | LT2 to LT6, PN5, and the page step of PV7. | Frames, latches, eviction. |
| STORE-01, STORE-06, STORE-07 | FL1 to FL6, PV5, DU1a, DU2, DU3, CM9. | Thresholds, the table checkpoint, replay, how a failed group is put out of reach. |
| STORE-08 | CL1 to CL7, PN6, CK3. | Liveness accounting, throttling. |
| STORE-09, TREE-08 | PV4, PV6, PV7 and CL5: a value pointer rewrite takes no version, is installed in a page step that checks the current page version, the owner and private copies, changes the counts in that step, is recovered with the pages, and an emptied file goes only when CL5 allows. FL1: values written ahead of a commit are held for their transaction. | When a file is cleaned, when a value is durable, regrouping, and what makes the page step one step. |
| STORE-10 | ID1 to ID3. | The allocator. |
| GROVE-05 | The drop removes the subtree's records in its own commit (CV9). The flush reclaims pages without a version (CL6) and removes each pending-drop record with one commit (14.5 D3). | The liveness check and the report. |
| GROVE-11, GROVE-12 | SC9 (pending marks), 14.4, SC3 and SC7. A scope's Merk cache keeps locations only under PN8. | Stop points, finalize, the GroveDB scope. |
| VER-05 | Section 13, and the models and scenarios of 15.2. | The models. |
| VER-08 | 14.3: the three mainnet heights run in the state of WT7b. | The replay. |
| OPS-08 | CM9: what a commit that fails for lack of space must leave. | The ENOSPC campaign. |

Which crate can enforce what follows from the layering in the plan.
`lignin-store` knows no keys: it enforces sections 5, 6 and 10, LT2 to LT6
and FL4. Record keys, write sets, claims and the conflict rule live in
`lignindb`, with `lignin-tree` for cursors and for answering a fetch by key.

### 15.2 Tests

Each line is a test some task owes. "Both engines" means the test runs
against RocksDB and LigninDB through the seams (SEAM-06) and must give the
same result.

**Read rule (TXN-02, TXN-03, GROVE-01)**

- T-RD1: a snapshot taken before a commit never returns anything of that
  commit, however long it is held and whatever the flusher and cleaner do.
- T-RD2: cases A3, A3b, A3c, A3d, B5, B11, B12, B16 of section 16. Both
  engines.
- T-RD3: cursor cases A12 and A12b. Both engines.
- T-RD4: the seven steps of 14.3 give the same root hashes and the same
  `OperationCost` for every operation. Both engines.
- T-RD5: writes and commit through a snapshot fail with `ReadOnly` (B13).
- T-RD6: a location kept across a foreign commit, a rebase, a rollback, a
  rollback to a savepoint and a scope rollback is never followed: the read
  returns the record RD3 names (PN8). Run with page ids being freed and
  reused in between.
- T-RD7: a cursor opened through a write transaction, with an empty write
  set and with a non-empty one, keeps reading its opening version while
  foreign commits free and reuse the pages on its stack (RD6, PN1, PN8).

**Conflict rule (TXN-05, GROVE-01)**

- T-CF1: every case of the tables in 16.3 and 16.4 whose LigninDB result is
  not marked as a difference gives the listed result. Both engines.
- T-CF2: the differences of CF9 give `Conflict` on LigninDB: a foreign range
  delete, a wipe and a load over a key the transaction wrote.
- T-CF3: a model test. A random schedule of puts, deletes, range deletes,
  savepoints, rollbacks and commits over several transactions is run against
  a model that evaluates CF2 directly. Every commit outcome agrees, in both
  directions: no missed conflict and no extra one.
- T-CF4: after 400 MiB of foreign writes during an open transaction (the
  setup of case A11b), LigninDB still answers CF2 exactly.
- T-CF5: operations that apply an empty batch add nothing (cases B18, B20,
  B22), and a plain insert of an equal element does write (B19). Both
  engines.
- T-CM1: every write and every fsync of step 6 of a commit is made to fail
  or to tear at every boundary (IO-02), under the default policy and under
  one that fsyncs each commit, and the outcome is checked against a model
  that applies recovery's test to the log (CM9). A commit reported as failed
  is never recovered, whenever the crash comes: at once, after the cut of
  item 4, or after later commits. A commit reported as succeeded or as
  `NotDurable` is recovered whenever its bytes survive, and always after a
  later successful `flush()`. A group of full length whose bytes do not
  verify counts as failed. Every handle reads what it read before a failed
  commit, and the next commit takes the same version and is recovered.

**Rebase (TXN-03)**

- T-RB1: the empty-overlap case of WT7a on one flat page (B16): the
  transaction reads the foreign key, keeps its own, and its commit publishes
  both.
- T-RB2: after any rebase with an empty overlap set, the committed state
  passes the physical invariant checker (VER-07).
- T-RB3: with a non-empty overlap set, every fetch returns the record RD5
  names, checked against a model that keeps records in a map.

**Savepoints (TXN-04)**

- T-SP1: cases A4, A4b, A5, A5b, A16, A17, B6, B7. Both engines.

**Scopes (TXN-09, VER-05)**

- T-SC1: two scopes on disjoint units, every interleaving that loom
  explores: the records, hashes and costs equal the serial run (SC7).
- T-SC2: each overlap is refused at once and leaves the scope usable: write
  against write, write against read, read against write (SC3).
- T-SC3: a scope rollback beside a merging scope leaves exactly the merging
  scope's writes (SC6), also when their Merks share a page (LT4).
- T-SC4: commit and savepoint with a scope open fail with `ScopesOpen`;
  commit with a pending mark fails with `NotFinalized` (CM1).

**Pins, reclamation and background work (TXN-02, STORE-08, STORE-10,
VER-05)**

- T-PN1: a loom model of PN4: a reader that pins while a commit is published
  and a reclaimer runs never reads a reclaimed page version.
- T-PN2: with a snapshot held at *s*, a workload that supersedes, frees and
  reuses pages never changes what the snapshot reads (PV3, ID2, CL1).
- T-PN3: no page id is reused while a pin below its freeing version is held
  (ID2; STORE-10's model check).
- T-PN4: relocation racing a commit that supersedes the block never makes
  the old block current (CL4).
- T-PN5: a file is not deleted while an operation that read a location in it
  is running (PN5, PN6).
- T-VL1: a value pointer rewrite, by cleaning and by regrouping, racing a
  write transaction that copies the same page and then commits or rolls
  back, and racing a commit outside any open transaction that supersedes the
  page after the cleaner looked at it. No committed cell ever holds a
  pointer that its file's live count does not include, every value reads
  back, an abandoned move counts nothing, and the page a commit installed
  says what the page rebuilt from disk says (PV7, PV6).
- T-VL2: values a transaction wrote ahead of its commit survive the cleaning
  of their file while it is open, are counted from its commit, and are
  reclaimed after a rollback (FL1).
- T-VL3: a crash at any point after moves emptied a value-log file never
  recovers a page that points into a deleted file, and the counts recovered
  agree with the pages (CL5, PV6).
- T-VL4: a write transaction that rewrote a node without changing its value
  still reads that value after a foreign commit overwrote it, after its
  rebase, and after the cleaner moved or deleted the old entry (WT3).

**Checkpoint and durability (TXN-06, VER-04)**

- T-CK1: a checkpoint taken while commits are published holds exactly one of
  their versions, and its root hash is that version's (CK1, CK2).
- T-CK2: an open write transaction's writes are in no checkpoint (CK8).
- T-CK3: the cleaner running during creation deletes no file of the file set
  (CK3), and afterwards deleting any live file leaves the checkpoint
  readable (CK7).
- T-DU1: after a crash at any write boundary the state is *S(Lr)* for one
  *Lr* at or after the target of the last successful `flush()` (DU1a, DU2).
- T-DU2: `flush()` racing commits: every commit acknowledged before
  `flush()` was called is recovered after a crash that follows its return
  (DU1a).
- T-DU3: a database that lost commits in a crash, then committed again under
  the same version numbers, and then crashed again, never recovers a group
  or a page version of the lost commits (DU3, CV6).
- T-WP1: a wipe racing readers, a checkpoint and a crash: each sees the
  database either as it was or empty in all four columns (CV3).

**Loom models owed by VER-05**: pin against publication and reclamation
(PN4); page-version install and publication against a reader (CM3, CM4);
claims (SC3); scope merge against scope read (SC6); rebase against the
transaction's other operations (WT7); a kept location against a rebase and
against page id reuse (PN8); making a page private once (LT5); the page
step: a value pointer rewrite against making the page private and against a
commit that supersedes it (PV7);
latch order (LT3); commit lock against checkpoint sampling and manifest
replacement (CK2, LT7); `flush()` against a commit in progress (DU1a);
relocation against supersession (CL4); page id retirement against reuse
(ID2, ID3).

**Combined scenarios for the stress test (VER-05)**: readers on snapshots of
different ages, one block transaction with several scopes, writes with no
transaction beside it, the flusher under backpressure, the cleaner, and a
checkpoint every few commits, all at once, with a model that knows *S(v)*
for every *v* checking every read and every commit outcome.

## 16. Derivation from the RocksDB engine

The read rule and the conflict rule were taken from the code, then checked
by running it.

### 16.1 Versions read

- `grovedb-storage` at this branch's base (`1b7294e8`): `rocksdb = "0.25.0"`
  (`storage/Cargo.toml`).
- `rocksdb` 0.25.0 depends on `librocksdb-sys` 0.19.0+11.8.1, which bundles
  RocksDB 11.8.1 (`include/rocksdb/version.h`).

### 16.2 What the code does

Paths under `storage/` are in this repository. The others are in the RocksDB
11.8.1 source tree.

**How GroveDB uses it**

| Fact | Where |
| --- | --- |
| The database is an `OptimisticTransactionDB` with default transaction-database options. | `storage/src/rocksdb_storage/storage.rs:117,376-388` |
| `start_transaction` makes a transaction with default options: no snapshot. | `storage.rs:1136-1138`; `rocksdb` `optimistic_transaction_db.rs:255-260` |
| Every read goes through `Tx::get`, `get_cf`, `raw_iterator`, which pass the transaction's snapshot. A plain transaction has none, which reads the latest state. | `storage.rs:224-271`; `storage_context/context_tx.rs:310-375,399-411` |
| `start_snapshot_read_transaction` sets a snapshot at creation, and writes and commit through it are refused. | `storage.rs:859-866,207-213,275-327` |
| A write through a transactional context goes into a `StorageBatch`, not into the transaction. It enters the transaction when the batch is applied: `commit_multi_context_batch` calls `rebuild_from_writebatch`. | `context_tx.rs:126-160`; `storage.rs:768-785,1215-1226` |
| A write through an immediate context goes into the transaction at once. | `storage_context/context_immediate.rs:94-184` |
| A GroveDB operation given no transaction starts one, uses it, and commits it. Every production caller of `commit_multi_context_batch` passes a transaction. | `grovedb/src/util.rs:8-35` |
| Three writes bypass transactions: `wipe` and `delete_prefix_ranges` (range deletes, one `delete_range_cf` call per column family) and `remove_pending_prefix_drop_record` (a point delete), plus `ingest_subtree_sst` behind `unsafe-dump-load`. | `storage.rs:871-911,928-966,994-1005,811-829` |
| Two reads bypass transactions: the size of a record deleted without cost information, and the list of pending-drop records. Both read the committed state. | `storage.rs:645-761,975-990` |
| A `StorageBatch` keeps one final entry per key. | `storage/src/storage.rs:319-378` |
| `commit_transaction` takes the transaction by value and returns RocksDB's error. | `storage.rs:324-327,1140-1147` |

**What RocksDB does**

| Fact | Where |
| --- | --- |
| A read returns the transaction's own write if it has one, and otherwise reads the database: `GetFromBatchAndDB`. An iterator merges the transaction's batch with a database iterator made at that moment. | `utilities/transactions/transaction_base.cc:286-292,474-488` |
| Put, delete and merge call `TryLock`, which for an optimistic transaction only records the key. Reads record nothing. | `transaction_base.cc:584-650`; `optimistic_transaction.cc:158-183` |
| The key is recorded with the database's latest sequence number at that moment, unless the transaction has a snapshot. | `optimistic_transaction.cc:171-180` |
| A key recorded twice keeps the smaller sequence number. | `lock/point/point_lock_tracker.cc:43-63` |
| `rebuild_from_writebatch` replays each entry through the same put and delete, so each is recorded then. It has no handler for range deletes, so a transaction cannot hold one. | `transaction_base.cc:894-975`; `include/rocksdb/write_batch.h:305` |
| Rolling back to a savepoint removes the keys recorded only since it, and keeps the others with their sequence numbers. | `transaction_base.cc:187-213,823-840`; `point_lock_tracker.cc:136-167` |
| Commit checks every recorded key, then writes the batch. Both validation policies run the same check. | `optimistic_transaction.cc:60-148` |
| The check for one key: find the newest sequence number under which the key was written, and fail with `Busy` if it is greater than the recorded one. | `transaction_util.cc:50-150`, line 132 and 146 |
| It looks only in the memtables and their retained history. If those do not reach back to the recorded sequence number, it fails with `TryAgain`. | `transaction_util.cc:67-106`; `db/db_impl/db_impl.cc:6020-6165` |
| A range tombstone counts only where a point entry for the key is found in those memtables. | `db/memtable.cc:1374-1387,1583-1598` |
| Opening an optimistic database keeps flushed memtables as history. | `optimistic_transaction_db_impl.cc:80-89` |
| `Rollback` clears the keys, the batch and the savepoints. | `optimistic_transaction.cc:150-153`; `transaction_base.cc:88-101` |

CF2 is these facts with "sequence number" read as "version" and "recorded
key" as "write-set entry". RocksDB's sequence number counts writes, not
commits, but it moves only at a commit, and the check only compares two of
them, so the comparison is the same.

### 16.3 Runs against RocksDB

Each case below was run against `rocksdb` 0.25.0 opened with GroveDB's
options and column families. *T* is a transaction from `transaction()`. *W*
is a write made outside it: a direct write to the database unless the case
says otherwise. "LigninDB" is what CF2 and RD3 require.

| Case | Sequence | RocksDB | LigninDB |
| --- | --- | --- | --- |
| A1 | *T* puts *K*; *W* puts *K*; *T* commits | `Busy` | `Conflict` |
| A1b | as A1, *W* being a second transaction that commits | `Busy` | `Conflict` |
| A2 | *T* starts; *W* puts *K*; *T* puts *K*; *T* commits | success; *K* holds *T*'s value | same |
| A3 | *T* puts *K1*; *W* puts *K2*; *T* reads *K2*; *T* commits | reads *W*'s value; success | same |
| A3b | *T* reads *K*; *W* puts *K*; *T* reads *K*; *T* puts *K2*; *T* commits | second read gives *W*'s value; success | same |
| A3c | *T* puts *K*; *W* puts *K*; *T* reads *K* | *T*'s value | same |
| A3d | *T* deletes *K*; *W* puts *K*; *T* reads *K* | no record | same |
| A4 | *T* sets a savepoint; puts *K*; *W* puts *K*; *T* rolls back to it; commits | success | same |
| A4b | as A4; then *T* reads *K* | *W*'s value | same |
| A5 | *T* puts *K*; sets a savepoint; puts *K*; *W* puts *K*; *T* rolls back to it; commits | `Busy` | `Conflict` |
| A5b | *T* sets a savepoint; puts *K*; *W* puts *K*; *T* rolls back; puts *K*; commits | success; *K* holds *T*'s second value | same |
| A6 | *T* puts *K* in `aux`; *W* puts *K* in `data`; *T* commits | success | same |
| A7 | *T* puts *K*; *W* deletes *K*, which never existed; *T* commits | `Busy` | `Conflict` |
| A7b | *T* puts *K*; *W* puts *K* with the value it already had; *T* commits | `Busy` | `Conflict` |
| A8 | *T* puts *K1*; *W* puts *K2*; *T* puts *K2*; *T* commits | success; *K2* holds *T*'s value | same |
| A9 | *T* puts *K*; *W* puts *K*; *T* puts *K* again; *T* commits | `Busy` | `Conflict` |
| A10 | *T* puts a new key *K*; *W* deletes a range containing *K*; *T* commits | success | **`Conflict`** (CF9) |
| A10b | *K* was committed and is in the memtable; then as A10 | `Busy` | `Conflict` |
| A10c | *K* was committed and flushed; then as A10 | `Busy` (the flushed memtable is still history) | `Conflict` |
| A10d | *K* was committed, then 400 MiB written elsewhere; then as A10 | success | **`Conflict`** (CF9) |
| A11 | *T* puts *K*; memtables are flushed; *T* commits | success | same |
| A11b | *T* puts *K*; *W* writes 400 MiB to other keys; *T* commits | `TryAgain` | **success** (CF9) |
| A12 | *T* opens a cursor; *W* puts *K2*; scan it; open a second cursor and scan | first: without *K2*; second: with it | same (RD6) |
| A12b | *T* opens a cursor; *T* puts *K2*; scan it | with *K2* | same (RD6) |
| A13 | two transactions write *K*; the second commits first; the first commits | `Busy` | `Conflict` |
| A13b | two transactions write different keys; both commit | both succeed | same |
| A14 | *T* puts *K*; *W* puts *K*; *T* rolls back entirely; puts *K*; commits | success; *K* holds *T*'s value | same |
| A15 | *T* writes nothing; *W* puts *K*; *T* commits | success | same |
| A16 | *T* sets savepoint 1; puts *K*; sets savepoint 2; puts *K2*; *W* puts *K*; *T* rolls back to 2; commits | `Busy` | `Conflict` |
| A17 | *T* rolls back to a savepoint when none is set | error | `NoSavepoint` |
| A18 | *T* puts *K*; *W* ingests a file holding *K*; *T* commits | success; *K* holds *T*'s value | **`Conflict`** (CF9) |

### 16.4 Runs against GroveDB on RocksDB

Each case starts from a database holding two empty trees `A` and `B` under
the root, at `GroveVersion::latest()`. "None" is a call with no transaction.

| Case | Sequence | RocksDB | LigninDB |
| --- | --- | --- | --- |
| B1 | *T* inserts `A/a1`; None inserts `B/b1`; *T* commits | `Busy` | `Conflict` |
| B2 | *T* starts; None inserts `B/b1`; *T* inserts `A/a1`; *T* commits | success; both items present | same |
| B3 | *T* puts an aux key; None inserts `B/b1`; *T* commits | success | same |
| B4 | *T* inserts `A/a1`; None puts an aux key; *T* commits | success | same |
| B5 | *T* only reads; None inserts `B/b1`; *T* reads `B/b1`; *T* commits | found; success | same |
| B6 | *T* sets a savepoint; inserts `A/a1`; None inserts `B/b1`; *T* rolls back to it; inserts `A/a2`; commits | success | same |
| B7 | *T* inserts `A/a0`; sets a savepoint; inserts `A/a1`; None inserts `B/b1`; *T* rolls back to it; commits | `Busy` | `Conflict` |
| B8 | the seven steps of 14.3 | as listed there | same |
| B9 | *T1* inserts `A/a1`; *T2* inserts `B/b1`; *T1* commits; *T2* commits | success, then `Busy` | success, then `Conflict` |
| B10 | *T1* inserts `A/a1` and commits; then *T2*, open since before, inserts `B/b1` and commits | both succeed | same |
| B11 | a snapshot read transaction is opened; None inserts `B/b1`; read it through the snapshot and with None | snapshot: not found; None: found | same |
| B12 | *T* inserts `A/a1`; read it with None and through *T* | None: not found; *T*: found | same |
| B13 | insert through a snapshot read transaction | refused, `SnapshotReadOnlyTransaction` | refused |
| B14 | *T* inserts `A/a1`; None inserts `A/a2`; *T* commits | `Busy` | `Conflict` |
| B15 | *T* puts aux key `x`; None puts aux key `x`; *T* commits | `Busy` | `Conflict` |
| B16 | *T* puts aux key `x`; None puts aux key `y`; *T* reads `y`; *T* commits | found; success | same |
| B17 | *T* inserts `A/a1`; `wipe()`; *T* commits | `Busy` (the trees were still in the memtable) | `Conflict` |
| B18 | `A/a1` exists; *T* calls `insert_if_not_exists` for `A/a1`; None inserts `B/b1`; *T* commits | success: *T* wrote nothing | same |
| B19 | `A/a1` exists; *T* inserts `A/a1` with the same value; None inserts `B/b1`; *T* commits | `Busy` | `Conflict` |
| B20 | `A/a1` exists; *T* calls `insert_if_changed_value` for `A/a1` with the same value; None inserts `B/b1`; *T* commits | success: *T* wrote nothing | same |
| B21 | `A/a1` exists; *T* deletes `A/a1`; None inserts `B/b1`; *T* commits | `Busy` | `Conflict` |
| B22 | *T* deletes a key that does not exist, which fails; None inserts `B/b1`; *T* commits | success: *T* wrote nothing | same |

### 16.5 How this was checked

- The RocksDB facts of 16.2 were read in the source that `cargo` unpacks for
  `librocksdb-sys` 0.19.0+11.8.1, at the lines given.
- The cases of 16.3 and 16.4 were run by one scratch program built against
  `rocksdb` 0.25.0 and this branch's `grovedb`. It prints the observed
  result beside the result predicted from the source, for the 52 cases other
  than B8. 51 predictions matched. The one that did not was A10c: the
  prediction had overlooked that a flushed memtable stays in the history that
  the check reads. A10d was added to show the other side of that. B8 is a
  trace; its seven results are in 14.3.
- The program is not part of the repository: this task's location is this
  file. T-CF1 and T-RD2 make its cases permanent tests on both engines.
- Platform's handling of the mainnet heights was read in
  `packages/rs-drive/src/drive/votes/cleanup/remove_all_votes_given_by_identities/v0/mod.rs`
  at `979510a9`, the commit BASE-05 pins.

## 17. Choices made here that the plan left open

Each is a decision the plan did not make and that later tasks build on. They
are listed so the owner can overrule one before code depends on it.

1. **Commit versions stay dense** (section 3), with one qualification that
   the owner ruled into format.md 3.2 with this task: a version is never
   reused and never goes down along the history that survives a crash. With
   DEC-04 a crash can lose the newest commits, and the next commit then
   takes a number a lost commit had (CV6). Keeping lost numbers unused
   instead would break "no gaps". DU3 is the recovery rule that makes the
   reuse safe. A second, smaller point: a commit with an empty write set
   takes no version (CV2).
2. **The state is modelled as records keyed as RocksDB keys them** (ST1,
   ST2), and the conflict rule is over those keys, not over pages. A
   page-level rule would be simpler to build and would not be RocksDB's.
3. **A read with no transaction pins one version for the operation** (RD2).
   RocksDB reads the latest state at each storage read.
4. **The overlay of a write transaction is by record, also inside a Merk**
   (RD5, WT7b). This is what RocksDB does and what the three mainnet heights
   need. It obliges TXN-03 to answer fetches by key once a foreign commit has
   overlapped the transaction.
5. **The engine allows several open write transactions** (WT1). A write with
   no transaction beside the block transaction already needs two.
6. **The write set is the truth and frames are derived** (WT3), which is
   what makes rebase, savepoints and scope rollback one mechanism.
7. **A failed commit ends the transaction** (CM6), except for the two
   precondition errors.
8. **Three deliberate differences from RocksDB** (CF9): no `TryAgain`; a
   foreign range delete or wipe always conflicts with a key the transaction
   wrote; so does data loaded outside the transaction. RocksDB's result in
   each depends on its memtables.
9. **TXN-05's "a second write transaction fails at commit with a conflict"
   is read as CF2** (CF7), not as "any second transaction fails".
10. **Scopes take read claims as well as write claims** (SC3). TXN-09's row
    speaks of the Merks a scope "touches". Shared read claims let scopes read
    common Merks; without any read claim the result would depend on timing.
11. **The unit of a claim is a column of a subtree** (SC2), so all aux
    records of the root subtree are one unit.
12. **TXN-09's "same pages" is read as the same logical content** (SC7).
    Page ids, slots and offsets can depend on which scope allocated first.
13. **Pending marks are an engine feature** (SC9), so that the engine can
    refuse a commit of an unfinalized tree and so that marks roll back with
    scopes and savepoints. GROVE-11 could instead keep them itself.
14. **Two scopes never modify one private frame at the same time** (LT4):
    either each has its own copy, reconciled at merge, or TREE-01 keeps their
    Merks on separate pages.
15. **A finished checkpoint holds nothing in the live database** (CK7).
    STORE-08's "once no snapshot or checkpoint needs them" then refers to a
    checkpoint only while it is being created (CK3).
16. **Checkpoint sampling takes the commit lock** (CK2), briefly. Commits
    continue during the fsync and the linking (CK5).
17. **In iteration two the conflict rule reaches less far** (14.4), because
    ancestors are written at finalize. GROVE-11 should state it.
18. **Wipe is one commit** (CV3), where the RocksDB engine takes four steps
    (14.5 D2).
19. **A subtree drop removes its records in the drop's commit** (CV9), where
    the RocksDB engine removes them at the later flush (14.5 D3). This
    follows TREE-09 and fixes what GROVE-05 maps the flush to.
20. **An operation is the size of one storage call** (section 1, RD4), so
    that the read rule is exactly as fine-grained as RocksDB's.
21. **A location kept between operations must be checked before it is
    followed** (PN8). This binds the path cache of PERF-03 and the scope
    caches of GROVE-12.
22. **The log decides whether a commit happened** (CM9): the engine applies
    recovery's own test to the group. A group that passes is published even
    if the call that wrote it reported an error, and a commit reported as
    failed can never be recovered. All file I/O of a commit, a per-commit
    fsync included, is in one step before anything is installed (CM3, CM7).
    This adds two commit outcomes, `NotDurable` and unsettled (CM5). And
    **`flush()` covers the commits published before it read *L*** (DU1a).
23. **A background change takes no version** (PV6). `value-log-format.md`
    O7 and O9 speak of "the commit that changes the cell" also for a value
    the cleaner moves. Making such a move a commit would rebase open write
    transactions and end kept locations during normal work, so it is not
    one, and PV6 says how O7 and O9 read. That document needs the same
    sentence, which only the owner can rule in. **A value pointer is
    rewritten only in the current page version and never under a private
    copy** (PV7): background work abandons the move and tries later. The
    other way, rebuilding the private copy, would put background work on the
    writer's path. **The write set is self-contained** (WT3), and **bytes
    written ahead of a commit are held for their transaction** (FL1).
