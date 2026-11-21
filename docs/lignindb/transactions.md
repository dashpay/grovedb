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

The documents of the tasks that merged after this one fixed things it had
left open or stated inexactly: [`tree-layout.md`](tree-layout.md) (TREE-01),
[`page-table.md`](page-table.md) (STORE-03),
[`page-alloc.md`](page-alloc.md) (STORE-10),
[`cost-model.md`](cost-model.md) (BASE-06),
[`node-store.md`](node-store.md) (SEAM-01, SEAM-02) and the value log
([`value-log-format.md`](value-log-format.md) and `lignin-store::values`,
STORE-09). Section 18 lists every rule that was amended for them, so that a
reader of an earlier copy can see what moved.

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
- **Committed read**: a read through a write transaction that leaves that
  transaction's own writes out (RD10).
- **Change point**, **page step**: the instant a change to a cell takes
  effect (PV6), and the one checked step in which background work installs
  such a change (PV7).
- **Pending value**: a value too long for its cell that a commit has
  published and that write-back has not yet put into the value log (FL7).
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

A Merk node is a record of its key whether or not a link of its Merk's tree
leads to it. A storage batch can leave one that no link leads to (GM0). It
is read by its key and under an iterator, written and deleted as any
record, and in no hash. LigninDB keeps a written node that its tree does
not reach outside its tree pages, as an **unlinked record** in the flat
keyspace of the `data` column, by the rule of `lignindb::txn` (`MerkWrite`,
"Unlinked records"; MERK-09).

`aux`, `roots` and `meta` records are plain values. `roots` holds the base
root key (`ROOT_KEY_KEY`).

**ST2. A record key is an identity, not a location.** It is the same triple
that identifies a RocksDB key today (column family, then prefix followed by
key). "Wrote a key", "its own write" and "conflict" below are all decided by
record key. Where the record's bytes are (which page, which slot, how many
deltas, which value-log entry) is never part of it.

**ST3. The subtree directory is not a record.** It is derived: the entry of a
Merk says where that Merk's root is (TREE-05, `tree-layout.md` C5), and that
follows from the records. It belongs to a version like everything else: at
every version it agrees with the records of that version. An entry has no
record key, so it is in no write set and no key set, and the conflict rule
never sees it.

Two things qualify "agrees", and neither makes an entry a record:

- An entry of a Merk nested in a dropped subtree can stay in the directory's
  pages after the drop. It is storage that waits to be reclaimed, and is part
  of no state from the drop's version on (CV9).
- Inside an open write transaction of iteration two, the entry of a Merk can
  be ahead of the element that hosts the Merk (SC2, SC9). In every committed
  state the two agree.

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
| wipe (TXN-07): exactly one commit, which deletes the range "everything" | the flusher: write-back, placing values in the value log (FL7), consolidation, table checkpoints |
|  | the cleaner: relocation, deleting files, value-log cleaning |
|  | reclaiming the cells and pages of a dropped subtree (CV9, CL6) |
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
commit. Reclaiming afterwards changes no record and takes no version (CL6).
The RocksDB engine removes those records later, in separate steps (14.5).

What the drop writes to pages is small (`tree-layout.md` D1): the cell of
the host, and the directory entry of the dropped Merk. One exception, from
iteration two: a host inside the dropped Merk whose pointer is behind (SC9)
is written first, so that the walk of CL6 can follow every pointer. That is
bounded by what the transaction wrote. On existing GroveVersions no host is
behind. The cells of the
dropped Merk, and the cells and directory entries of every Merk nested in
it, stay in their pages until they are reclaimed (`tree-layout.md` D2). They
are storage, not state. Let *w* be the version of the drop's commit.

- No read, cursor or open at a version from *w* on returns a record of the
  dropped subtree, or a directory entry of a Merk nested in it: not by path,
  not by prefix, and not through a host. Inside the dropping transaction the
  same holds from the drop on (RD3).
- A subtree created later under a dropped prefix starts empty. It never
  shows a record or an entry that the drop left behind, whether or not they
  have been reclaimed yet.
- A snapshot pinned below *w* reads the subtree as it was (RD1). That is why
  reclaiming waits for it (CL6).

How a lookup tells a left-over entry from a live one is TREE-05's and
TREE-09's.

Clearing a Merk the same way (`tree-layout.md` D4) is a drop that keeps the
host: the Merk's directory entry becomes that of an empty Merk and is not
removed. The three points above and CL6 hold for it as for a drop.

On existing GroveVersions a clear that keeps the host is in the write set
what `Merk::clear` makes of it: one delete for each node, by its key, and
the root of the Merk stated as gone (GROVE-05). Its commit outcomes are
those of the same deletes on RocksDB (GM0), against every write, so it
adds no row to 14.5. The clear above, one range delete of the Merk's
records (ST4), is the engine's and is used by no operation of GroveDB: it
would give the same outcomes against every tree change, because a clear is
a tree change itself (GM2), and others against writes that do not
propagate to the root (14.2), as row D5 of 14.5 does. A clear has to read
every node for what it is charged on these versions either way; a later
GroveVersion that charges a clear by a rule of its own can take the range
delete (GROVE-13).

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

Its transaction has read nothing when the write is applied. So its batch
cannot keep the committed value of a Merk node (CF2a): a keep that names no
read is refused (`NoRead`), and one that names a read of another
transaction is one the engine cannot date, which is a conflict.

**RD8. Nobody but *T* sees *T*'s uncommitted writes.** No snapshot, no other
write transaction, no background actor and no checkpoint reads them (P-10).

**RD9. Equal records give equal costs.** What a read charges is a function of
the records it returns (BASE-06, SEAM-05). So a handle that returns the
records RocksDB would return charges what RocksDB would charge.

**RD10. A write transaction can read the committed state under its own
writes.** Beside the reads of RD3, an operation of *T* can ask for the
**committed read** of a record key *K*: the record *S(L)* has under *K*, for
the *L* of that operation (RD4), whatever *T*'s write set says about *K*.

| *T* has | The committed read returns |
| --- | --- |
| created *K*, which has no committed record | no record |
| replaced, deleted or dropped (CV9) the record under *K* | the committed record, as it is at that *L*: the one *T* wrote over, or what a foreign commit has made of it since |
| not written *K* | what RD3 returns |

The cost rules need it. A delete that carries no cost is charged from the
committed state, not from the transaction's view (`cost-model.md` D2), and
that is how every record removed by `clear` and by `Merk::clear` is charged.
What must be answerable is what that charge is a function of
(`cost-model.md` sections 4 and 9):

- whether *K* has a committed record at all
- for a flat, `aux`, `roots` or `meta` record: the length of its value
- for a Merk node: the length of its value, its feature type with its
  aggregate numbers, and for each child the length of the child's key and
  the child's aggregate data, all as committed. The bytes of the value are
  not needed.

A committed read is answered by key, never through a location *T* holds:
*T* may have rebuilt the tree around *K*, so the committed node is found in
the committed tree of its subtree, from the committed root (as in 7.3).
*T*'s base pin keeps those page versions readable (PN1). A transaction that
keeps only its own newest copy of a page cannot answer.

It is a read: it adds nothing to the write set, sets no first-write version
and takes no part in the conflict rule (CF4). For claims it is a read of its
unit like any other (SC3). What it charges is `cost-model.md` D2's to say,
not RD9's. A snapshot has no use for it: it has no writes.

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
   deleted and not overwritten. One case differs: what was never written
   back of a page that has since been freed is kept in memory, and its log
   records may go (FL6).
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

An operation takes its guard before it reads *L*, and holds it for as long
as it uses a page id, a page-table entry or a file location that it read
(`page-alloc.md` N4). Taking it after the read would leave a moment in which
the version it read can be reclaimed under it. An operation through a
snapshot or a cursor is covered by its version pin as well.

**PN6. Retire first, reclaim later.** Nothing that a reader could still reach
is reused or deleted directly. It is first **retired**: unlinked from every
current structure, so that no new operation can find it. It is **reclaimed**
only when both of these hold:

- no version pin needs it (PN2, PV3)
- every operation guard that was held when it was retired has been released

The first condition is counted in versions. The second is not, because
relocation and consolidation retire things without any commit: it needs the
engine's own clock of retirements (the "epoch" of TXN-02).

TXN-02 keeps the pins and the guards. A reclaimer learns of them through
three questions and nothing else (the `Pins` interface of `page-alloc.md`,
N1 to N3):

1. **The oldest pin**: a version *H* such that no pin held now is below it,
   and none taken later will be. It is never above the last published
   version. A commit that is installed and not yet published does not
   count, because a reader can still pin the version before it (CM3 steps 7
   and 8).
2. **The next epoch**: this starts an epoch *e*. Every guard held at that
   moment was taken in an earlier epoch, and every guard taken from then on
   is taken in *e* or later.
3. **The oldest guard**: an epoch *g* such that no guard held now was taken
   before it, and none taken later will be.

A reclaimer asks them in this order. Something retired at version *w*
meets the first condition, by the simple test of PV3, when *w* ≤ *H*. Only
then is it stamped with a fresh epoch *e*, and it is reclaimed once
*e* ≤ *g*. The epoch is started after the pin test has passed, not when the
thing was retired: a commit retires before it is published (CM3 step 7),
and until it is published a new operation can still read the version before
it. An answer stays a true lower bound after other threads have moved on,
because neither the oldest pin nor the oldest guard ever goes down (PN3). So
a reclaimer never waits, and several can run at once.

The page allocator (ID2, ID3) and the cleaner (CL3, CL5, CL6, STORE-08) use
the same three questions.

**PN7. Pins never block and are never refused.** A pin held for a long time
costs disk space, never correctness: it holds back the cleaner (CL1) and page
id reuse (ID2). STORE-08 bounds and reports that cost. It costs memory as
well, where a page version it needs is kept in the buffer pool and nowhere
else (FL6, FL7).

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
  reconciliation of a scope merge (LT4). It also ends when a write of the
  same transaction carries the cell to another page or deletes it
  (`tree-layout.md` S2, S6): a slot names a cell only while the cell stays
  in its page, and a freed slot can be given to another cell, even to one
  with the same key. So finding the expected key at a location does not
  show that the location is still good.
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

Readable means in memory, or rebuildable from what is on disk (PN2). For
one kind of page version it means in memory only. What was not yet written
back of a page when a commit freed it is never written back, and the buffer
pool keeps it, not evictable, for as long as it is needed (FL6). Only
readers of the running session can ask for it. No recovery does.

"Needed" speaks of pins. A running operation that holds no pin can still
have reached a frame, and its guard keeps the frame until it ends (PN5).
So a frame that is kept in memory because nothing else holds its content,
under FL6 or FL7, goes only by PN6: when the page version is no longer
needed and the operations that were running then have ended.

**PV4. Background work may change how a page version is stored, never what
it says.** Relocating its blocks, consolidating its delta chain into a new
base image, putting a pending value into the value log (FL7), and rewriting
a value pointer (when the value log is cleaned, or when a page's values are
regrouped at consolidation, TREE-08) all leave every visible page version
with the same logical content. Such a change
is applied either to every needed page version it affects, or only to the
current one while the old representation, and whatever it points to, stays
live for the older ones. A new representation is installed beside the old
one: bytes that a view borrows are never edited (LT2). The old one is
retired, under PN6, only once no needed page version consists of it.

A location that was good for a page version stays good for it (PN8).
Background work does not renumber anything that a pointer in another page,
the subtree directory, the value log or an open handle refers to.

Removing what a dropped subtree left behind (CL6) is allowed on the same
ground. Once the oldest pin has reached the drop's version, no needed page
version can be asked for those cells and directory entries (CV9), so taking
them out changes no read.

**PV5. A page version records its born version where the store keeps it.**
Deltas and base images carry the version that produced them, in memory and
on disk, so that PV1 can be evaluated for a pin and after a crash (DU3). The
byte layout is FMT-03, FMT-05 and STORE-03's. A run of page deltas carries
no commit version itself: the page table's link to it and the log group
that holds it do (PD5 of `page-delta.md`).

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

For the live-value counts that gives three cases:

- A commit changes them by the pointers its cells stop holding, at its
  commit point. A value the commit itself wrote has no pointer yet (FL7), so
  the commit adds nothing to any count.
- A pending value is counted at the page step that puts its pointer into
  the cell (PV7, FL7).
- A value the cleaner moves is counted, one fewer in the old file and one
  more in the new, at the page step that installs the move. It takes no
  commit version (`value-log-format.md` O9).

The count is still that of the latest version, as `value-log-format.md` O7
has it: a background change alters how that version is stored and counted,
and changes no version.

The counts have a lock of their own. Every change point that changes a
count writes its record to the log while it holds that lock (LT7), so the
log has the changes in the order the counts took them. DU5 says how the
counts are recovered.

**PV7. A value pointer is rewritten only in the current page version, and
never under a private copy.** Of the changes PV4 allows, three alter the
bytes of a page as a handle reads them: a value pointer rewrite, the placing
of a pending value, and the removal of what a dropped subtree left behind.
The rule is stated for the first. Its last part says how it reads for the
other two.

Two things can go wrong with a value pointer rewrite, and each ends with a
committed cell that points into a file whose live count no longer includes
the value, so that the file could be deleted under it
(`value-log-format.md` O7):

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
are page steps too. Every page step sees the page as the one before left
it. Which latch or retry an implementation uses for this is STORE-04's and
STORE-09's; it sits at level 5 of LT7.

Two things follow. What a commit installs for a page says what the page
version it supersedes says, with the commit's own changes, and holds the
value pointers that page version holds at that moment. It need not be, byte
for byte, the stored representation: the flusher may have consolidated the
page since the copy was made, which changes no pointer and for which no
commit waits (CM8). And a value pointer that background work replaced is
never committed again.

**The other two changes are installed in a page step as well, under the
same three checks.** Each would otherwise be undone, or done a second time,
by a commit that was built from a private copy.

- **Placing a pending value** (FL7). At write-back the cell that held the
  bytes of a value is given the pointer to them. Check 2 reads: the cell
  still holds that pending value. The file's count changes in the step
  (PV6). Without check 3, a transaction that copied the page before the step
  would commit a page in which the value is pending again, and it would be
  placed and counted twice. Check 1 holds wherever the value is counted.
  FL7 says what may be done for a page version that is no longer current,
  for which nothing is counted.
- **Removing a cell or a directory entry that a dropped subtree left
  behind** (CL6). Check 2 reads: the cell or entry is still there. The
  counts of the value pointers the cell held change in the step. Without
  check 3, a commit would bring the removed cell back, or install a version
  of a page that the removal had emptied and freed.

A step that is refused leaves the page as it was, and is tried again later.

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
- *T* writes to no data file before it commits: not to a segment, and not to
  the value log. A value too long for its cell stays with *T*'s private
  frame, or in its spill file, until the commit carries it in its log group
  (CM3 step 6, FL7).

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

The engine must not repair, reorder or refuse this state, with one
exception: what a move of *T* decides (MV7). Mainnet blocks
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

While it holds private copies, background work leaves those pages alone
(PV7): no pending value of them is placed, no value of them is moved, and
nothing a dropped subtree left in them is removed. Like a pin held for a
long time (PN7), that costs log and disk space, never correctness.

### 7.5 The move of a subtree

GroveDB's `Move` gives an element another key and keeps what is under it.
The prefix of a subtree is made from its path, so the subtree under the
element, and every subtree below that one, has another prefix afterwards
(`tree-layout.md` D5). A batch states this as a move (`SubtreeWrite::Move`;
D-212, D-213): the prefix the subtree has (`from`), the prefix it gets
(`to`), the element that hosts it afterwards, the read of the node that
hosts it before the batch, and for each subtree below it the prefix it has
and the prefix it gets (`below`). **The prefixes a move names** are `from`,
`to` and both prefixes of every pair of `below`.

**MV1. A move re-keys and copies no node.** The moved Merk, and every Merk
below it that goes with it, keeps its cells, its id, its root, its tree
type and the Merks it is below. The move writes the directory's records of
each under the new prefix, and each of its flat records, in every column,
under the new prefix with the key and the value it had. Under a prefix
that a Merk was taken from and none was brought to, the directory then has
no Merk and no column has a record (`tree-layout.md` D6). The moved Merk is
hosted by the node that the batch puts at the new key.

**MV2. The subtrees below.** Each Merk that the directory has below `from`
when the move is made, at any depth, does one of three things:

- one that `below` names goes to the prefix named for it. Its host keeps
  its key and its position, in the Merk above it under that Merk's new
  prefix;
- one that `below` does not name, and that has no node, leaves with the
  move as a drop takes a subtree (CV9): its entry and its flat records go,
  and so does every Merk below it that has no node and is not named;
- every other one refuses the batch (`MoveUnnamed`): a Merk with a node
  that is not named, and a Merk that is named below one that leaves.

A pair whose first prefix has no subtree gives no subtree another prefix.
Its two prefixes are prefixes the move names all the same: the one it
states for the subtree is free as every other (MV3), and the entry of the
move covers both (MV6). A pair whose first prefix has a subtree that is not
below `from` refuses the batch (`Moved`), unless the batch takes that
subtree out with one above it.

**MV3. What a batch with a move holds.** The batch is refused, and changes
nothing (WT10), where

- `from` has no subtree (`NotEntered`);
- the moved subtree has no host; the host that the move states is in
  another Merk than the host it has, or at another position; or the batch
  does not put the stated host, as a node that hosts `to` at that position
  (`MoveHost`);
- the moves of the batch name a prefix twice as one a subtree is taken
  from, or twice as one a subtree goes to (`SubtreeTwice`);
- a prefix that a move states for a subtree, `to` or the second prefix of
  a pair, has a subtree which no move of the batch takes from there and
  which the batch does not take out, by a drop of it or by a drop or a
  clear of a subtree above it (`Entered`);
- the node that hosts the moved subtree afterwards hosts, at that
  position, a subtree of which the same holds (the directory's
  `HostTaken`, as for an enter);
- the batch states anything else of a prefix that one of its moves names:
  a change set, a record, an enter, a clear, or a drop of a subtree that a
  move takes from its prefix. The one statement it may hold there is the
  drop of a subtree under a prefix that a subtree goes to and none is
  taken from (`Moved`);
- the host that a move states is in a Merk under a prefix that a move of
  the batch names (`Moved`);
- the batch drops or clears a subtree that the moved one is below, or one
  that is below the moved one (`Moved`).

The staged batch (`StagedBatch::move_subtree`) takes the host of a move
from what it was told hosts `to` when it is resolved, whichever operation
said so. Where it is resolved it refuses a move for whose `to` it was told
no host (`MoveHost`); moves of two operations (`Moved`); moves that name a
prefix twice on one side (`SubtreeTwice`); and what it holds under a
prefix that a move names by the rule before the last two, whichever
operation staged it (`Moved`). The drop that the rule allows is one of the
operation of the moves or of an earlier one: a drop that a later operation
states would be of the subtree that was moved there, and the batch applies
its drops before its moves.

**MV4. The order of a batch** is: its drops and clears, its moves, its
enters, its change sets, its records. **The moves of one batch are made
together**, from the state that its drops and clears leave: every Merk is
taken from its prefix before one is entered under its new prefix. So two
subtrees change places in one batch, and a subtree goes to the prefix that
another move of the batch takes one from.

**MV5. What the transaction reads afterwards.** From the move on, *T* reads
under each prefix that a subtree went to what it read under the prefix the
subtree was taken from: the committed records there and its own writes
from before the move, in every column, and what it writes under the new
prefix afterwards (RD3, RD5, RD6). What the subtree held in its data
column, the nodes of a Merk or flat records, it holds under the new prefix
(ST1). Under a prefix that a subtree was taken from, or that a subtree
which left with the move had (MV2), and that no move of the batch brought a
subtree to, it reads a subtree that is not entered and no record, until it
enters one there. That one is new and holds nothing of the one that was
moved: what *T* then drops or clears under that prefix takes nothing of
the moved one. The committed read (RD10) answers under every prefix what
the committed state has there: a move does not change it.

**MV6. The move in the write set.** The moves of one batch are one entry of
the write set, with the first-write version of their operation (WT5). The
entry covers every key of every column under every prefix they name, and
under the prefix of every subtree that left with them when they were
stated (CF10). A rollback to a savepoint from before it, or of the scope
that made it, removes it (WT8, SC6).

Frames that are made again (WT7) make the move again, over the committed
state as it is then: MV2 and MV3 are asked of the directory of that state.
So a subtree with no node that a foreign commit entered below `from` since
the move, and that the move does not name, leaves with the move from then
on, and *T* reads no record under it. Where the move cannot be made over
that state, the layout stops there: *T* serves its records by key (MV7),
and its commit is refused with the refusal that MV3 gives the move. A
foreign commit that wrote a key under a prefix the move names has met the
move by then (CF10). What is left is one that enters a subtree and writes
no such key, where a writer on existing GroveVersions writes the node that
hosts the subtree it enters.

Each time the move is made, the flat records of every Merk it re-keys are
written again: one put for each, with its value. That is the cost of a
move that does not grow with the cells of the moved subtree and does grow
with its flat records, at the write that states it and at every rebase of
a transaction that holds it (WT7c).

Measured once, for a moved Merk with 100,000 flat records of a 32-byte key
and a 100-byte value in one column
(`what_the_copy_of_the_flat_records_of_a_move_costs` in
`lignindb/src/db/tests/moves.rs`: a release build, 8 KiB pages, the store
in memory). The write that states the move copies 1,330 flat pages. Frames
that are made again after a foreign commit copy 1,330 again. The commit
makes 1,323 pages and changes 7, in a log group of 10,758,247 bytes; the
pages of the old range are unlinked, for the reclaimer to free
(`tree-layout.md` D6). On the machine that built it, under load, the write
took 0.87 s and 0.84 s in two runs, the rebuild 0.84 s and 1.61 s, and the
commit 14 ms and 103 ms: 8 to 9 microseconds for a record that is copied.

**MV7. By key, a move is not answered for.** WT7b has one exception. Where
*T* serves its records by key, a read or a write of a subtree is refused
if a move of *T* decides what is under it: a prefix the move names, a
subtree that left with it, a subtree that *T* entered below one of those
by the hosts its own enters state, and a subtree that the committed state
has below a prefix the move names, on either side. A batch that states a
move is refused there too. A cursor takes in the commits up to the version
it reads before it asks.

- Where a foreign commit has met a write of *T*, the refusal is a conflict
  (`MovedByKey`, CF6): the commit of *T* fails with `Conflict` anyway
  (CF2), and the refusal says so one step earlier. A rollback to a
  savepoint that empties the overlap set (WT8) gives *T* its frames again,
  and its reads with them, where the writes that are left can be laid out.
- Where the writes of *T* cannot be laid out over the version it reads,
  the refusal is the one that stops the layout, and no conflict.

Every other read is served as WT7b says, and the committed read (RD10) is
never refused for a move.

**MV8. A move and the scopes** (section 9). A move is laid out on the
frames by the write that states it. It claims every column of every prefix
it names as a write of each (SC3), so no scope beside it reads or writes
under them while its scope is open. A scope whose writes are kept in the
write set, because it was opened beside another and has not laid them out,
refuses a batch with a move, and every read and write that a move of the
transaction decides (MV7), with `MoveBeside`, until its writes are laid
out. Where the transaction itself serves its records by key, the refusal
is that of MV7, through whichever scope. Every scope whose writes are laid
out refuses a batch with a move, with `MoveBeside`, while
an element of a Merk under a prefix the move names is marked pending
(SC9), or was at some point of the writes that the transaction's frames
show.

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
   one log group for *v*, and then, if the sync policy asks for an fsync at
   commit, that fsync. The group holds the bytes of every new value, also of
   one that is too long for its cell (FL7), the structure deltas, one record
   per changed hash, the flat records, the range deletes, the pages the
   commit frees (ID4) and the value pointers its cells stop holding (DU5;
   STORE-01, FMT-05). The log is the only file a commit touches (CM7). This
   is the only step that touches a file and the last that can report an
   error. If it does, CM9 decides whether the commit happened.
7. Install, in memory only: turn the private frames into page versions born
   at *v*, mark the page versions they replace as superseded at *v*, publish
   the ids of the pages the commit created, free the ids of the pages it
   frees, with version *v* (`page-alloc.md` N6), and count the value
   pointers its cells stopped holding (PV6). This step writes to no file and
   cannot fail. None of it is visible yet: no reader looks at a page version
   born after the version it reads (PV1).
8. Publish: store *v* into *L*. Record the commit's key set for the conflict
   checks of the other open write transactions (CF3). Release the lock.

Hashes may be evaluated before the lock is taken, to keep it short. Step 5
finishes whatever is still dirty then.

A commit whose cells stop holding value pointers changes a count of live
values. It takes the counts lock before it appends its group in step 6 and
gives it back when step 7 has counted the change (LT7).

The pages step 7 frees are pages of the latest version. The commit holds
the commit lock, and if another commit came first, step 4 rebuilt its
private frames over what that commit left. So no other commit has freed
them. The conflict rule alone would not say so: it is about record keys, not
pages (`page-alloc.md` A4).

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

**The log is the only file a commit writes.** It appends to no value-log
file and syncs none, and it replaces no manifest: its new values travel in
its log group (FL7). The cut that follows a failed append (CM9 item 4)
replaces the manifest, and it is made before the next commit, not by one.

**CM8. Commit waits only for the commit lock, for flusher backpressure**
(FL5) **and for the counts lock** (LT7), **and, if its sync policy has it
fsync the log (CM7), for an fsync of the log that someone else is making.**
It never waits for a reader or for another open write transaction. Of the
creation of a checkpoint it waits for the sampling step, and, with the
exception that follows, for nothing more. The flusher and the cleaner can
make it wait only through the counts lock, for one hold of it, which
covers at most one append to the log and no fsync, and, by the same
exception, the flusher through an fsync of the log.

The exception is the fsync of CM7. The log makes one fsync at a time
(STORE-01), so a commit that fsyncs the log waits at its own fsync for one
that a `flush()`, a checkpoint or the flusher is making then (DU1a), and
holds the commit lock while it does. A commit whose policy does not fsync
at the commit does not join it. A `flush()` whose fsync failed holds the
commit lock for the moment in which it records that the session ends
(DU1a), which is a wait for the commit lock like any other.

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
  reach. The same holds for the record of a background change that was
  installed (PV6). Those records are in the same log, between the groups
  (`lignin-store::values`).
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
4. **Before anything else is appended to the log, the failed bytes are cut
   off.** Recovery stops reading a log file at the first group that fails
   its test, and the next commit will carry the same version *v*. So, for
   (c), the engine ends that log file at the end of its last good record,
   records that length durably where recovery reads it (a closed entry in
   the manifest, `segment-manifest.md` M15), and goes on in a new file.
   STORE-01 and OPS-08 choose the mechanics.
5. While it cannot do that, for example because the disk is still full, the
   engine accepts no commit and installs no background change that needs a
   record in the log. It resumes when it can. Every read keeps working
   throughout.
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
the commit does not fail with `Conflict`, but for a kept value (CF2a).

In words: the commit fails when someone else committed a write to a key
after *T* first wrote that key. It does not matter that *T* wrote the key
again later (case A9). It does not matter how long *T* has been open: a
commit that landed before *T* first wrote the key is not a conflict, and *T*
then built on it (cases A2, B2).

**CF2a. A kept value is dated at its read** (D-097, D-155). An entry *e* of
*T* that writes a Merk node with its value kept, where the value is the
committed state's and not one *T* stated, depends on that value as *T*'s
writer read it. The write names that read by the handle the node was read
with (PN8). Let *r(e)* be the version the read was made at, *r(e)* ≤
*fw(e)*. The commit of *T* fails with `Conflict` when a foreign commit *c*
with *v(c)* > *r(e)* has a key set that meets the key of *e*: CF2 with
*r(e)* in the place of *fw(e)*. So a node is never published whose value
hash was made for a value another commit has replaced (case A19).

- **An entry that keeps a value *T* stated itself is not touched.** CF2
  decides from *T*'s first write of the key (case A19c). A node that *T*
  wrote before with its value kept has the committed value still, and a
  later keep of it is dated at the read that keep names.
- **A write that keeps a committed value and names no read is refused**
  (`NoRead`) and changes nothing (case A19d, WT10).
- **Where the engine cannot date a read, it takes *e* to be met**: a
  conflict that CF2a does not ask for, and never a wrong node. That is a
  handle the engine did not show a node with since *T* last ended, a read
  more than 64 commits before the write, and a handle of the frames of a
  write that was itself met in this way. A thread that reads and writes a
  Merk with no commit of another in between meets none of them.
- **Where a foreign commit after *r(e)* left no node under the key**, by
  deleting the node or by dropping or clearing its Merk, there is no value
  to keep. The write is refused and changes nothing: no keep with nothing
  to keep enters the write set. The refusal says that foreign commits are
  its cause (`ValueTaken`) where the engine can tell that of every node
  of the refused Merk that the batch has no value to keep of: the read is
  one it can date, the key
  set of a commit since holds the key, and nothing *T* wrote itself took
  the node away. That refusal is a conflict to the caller (CF6), and the
  transaction stays open (WT10). Everywhere else the refusal is of what is
  not there (`NoValue`, `NotEntered`), as it is with no commit in between:
  for a keep that names no read, for a read the engine cannot date, for a
  node *T* deleted itself or a Merk it dropped or cleared itself or that
  is below one it did, and for a foreign drop of a Merk above the node's,
  whose key set does not name the Merk below.
- **CF4 holds for the entry**: a rollback that removes it forgets the mark.

The transaction is given the key sets for this from the first node it is
shown, or the first cursor it opens over a Merk, and not only from its first
write. What it has to hold for that is bounded (CF3). It forgets its reads
when it ends: by a rollback, by a commit, and by a commit with an empty
write set, which is no commit (CV2) and ends it all the same. A keep by a
handle from before that is one the engine cannot date.

**CF3. Enough must be remembered to decide CF2 exactly.** While a write
transaction is open, the engine must be able to evaluate CF2 for it against
every commit published since its oldest first-write version. It never
answers "cannot tell". One way that needs no history: when a commit is
published, compare its key set with the write set of each open write
transaction and remember the entries it meets. A transaction's commit fails
if anything is remembered for it. Entries that a rollback removes are
forgotten with it (CF4).

For CF2a the engine answers "met" where it cannot tell, and it remembers
the key sets of the newest 64 commits for it and no more. A transaction
that has been shown a node and has no write uses key sets for nothing else.
So it holds back at most those 64, however many commits are published
while it stays open, and a keep by a read from further back is met. A
transaction with a write holds what CF2 needs, as the first paragraph says.

**CF4. What changes the outcome, and what does not.**

| Does not matter | Because |
| --- | --- |
| what *T* read | reads are not tracked. This is not serializability (case A3b). The reads that count are the read of a Merk node whose value a write keeps (CF2a), and the read of the node that hosts a subtree which a write moves (CF10). |
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

A drop deletes every record under every subtree below the dropped one (CV9).
The key set of its commit names the dropped subtree, and of the subtrees
below it those that its transaction had written under, which the entry of
the drop covers; so does a clear. A foreign commit that drops or clears a
subtree above one that *T* wrote under, by the Merks that *T* found that
subtree below at its last look at the committed state, meets every entry of
*T* under it. And for a subtree that *T* entered if it was absent
(`SubtreeWrite::EnterIfAbsent`), whether it was entered then or not, a
foreign commit that drops or clears a subtree which the stated host of that
enter was in or below, as *T* had the subtrees when it stated the enter,
meets the entries that *T* made under the entered subtree from that enter
on, until *T* entered the subtree again, and before that commit. A foreign
commit that moves subtrees (7.5) is, for both rules, one that drops every
prefix its moves name (CF10).

Neither rule sees the following, and there the outcome can depend on
whether *T* looked at the committed state between two foreign commits: a
subtree is not entered at *T*'s first write under it, a commit beside *T*
enters it, and a later commit takes it with a subtree above it. With an
enter of a subtree as one that is new (`SubtreeWrite::Enter`), which
records no names, that is every such pair of commits. With an enter if it
is absent it is a pair whose later commit takes the subtree with one that
is not among the names the enter recorded. An example: the enter states a
host in a Merk *P*, the first commit enters the subtree under an element of
a Merk *Q* beside *P*, and the later one drops *Q*.

A bulk load into a subtree, inside *T* or outside it, may be counted for
this rule as one write of the range "that subtree's column", in place of one
entry per loaded key. What is read back under those keys is the loaded
records (RD3).

**CF6. `Conflict` is one typed error, used for CF2 and CF2a and for nothing
else.** GROVE-01 maps it to the engine-neutral conflict error of SEAM-04, the
one RocksDB's `Busy` also satisfies. A commit that fails for another reason
must not return it, and a conflict must not be reported as anything else.

Two refusals are conflicts too, and say so. `ValueTaken` is the refusal of
a write: the keep of a value that a foreign commit took away since its read
(CF2a), and what CF10 tells in the same way for a move and for a write
into a subtree that a foreign commit took. `MovedByKey` is the refusal of
a read or a write that a move of *T* decides, once a foreign commit has
met *T* (MV7). Neither is `Conflict`, because it is the answer to a read or
a write and not to a commit, and it leaves the transaction open with what
it wrote before (WT10). A caller that takes it as a conflict rolls the
transaction back and makes its reads and writes again. The seam does not
map them to the engine-neutral conflict error yet: that is GROVE-10's,
which tells them by `TxnError::is_conflict()`.

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
| A foreign commit writes a Merk node between *T*'s read of it and *T*'s write of it, and *T*'s write keeps the node's value | success: the node holds the value *T* read, and the foreign value is lost (case A19) | `Conflict` (CF2a) | only with two writers of one Merk at a time |
| A foreign commit writes the node that hosts a subtree between *T*'s read of that node and *T*'s move of the subtree | success: the copy under the new path holds the records *T* read, under the hash *T* states for them, and what the foreign commit wrote under the old path is deleted with it | the batch is refused as a conflict (`ValueTaken`, CF10) | only with two writers of one Merk at a time |
| A foreign commit moves a subtree away or drops it, so that nothing is entered under its prefix, and *T*, of whose writes that commit met none, then writes into it | success: *T*'s records are under the old prefix, which no element leads to | the batch is refused: as a conflict (`ValueTaken`) where *T* holds the key set of that commit, and as `NotEntered` where it does not (CF10) | only with two writers at a time |
| A foreign commit meets a write of *T*, and *T* then reads or writes where a move of its own decides | the read and the write are served, and the commit fails with `Busy` | the read or the write is refused as a conflict (`MovedByKey`, MV7), and the commit fails with `Conflict` | only with two writers at a time |

LigninDB is stricter in the second and the third because the alternative is
to commit *T*'s older records on top of a range that was deleted or loaded
under it, which can leave a tree that no longer hashes to its root. It is
stricter in the fourth because a node of LigninDB can keep its value where
RocksDB's whole-record put states it again: without the rule the node would
be published with the foreign value under the hashes *T* made for the value
it read (D-097). It is stricter in the fifth because a move that re-keys
carries the Merk as it is when the batch is applied, under a host whose
hash was made for the Merk as *T* read it. In the sixth it refuses where
RocksDB leaves records that nothing leads to. The seventh has RocksDB's
outcome, one step earlier (D-213).

**CF10. The move of a subtree** (7.5; D-213).

*It is dated at a read.* The node that the batch puts at the new key states
the value hash of the node at the old key. For a tree that hash commits to
the root of the moved Merk as the mover read it, and the move carries the
Merk as it is when the batch is applied. So a move names the read of the
node that hosts the subtree before the batch, as a kept value names its read
(CF2a). The batch is refused with `ValueTaken`, for the Merk that host is
in, where a foreign commit wrote the host's key since that read, and where
the engine cannot date the read (the three cases of CF2a). A host that *T*
wrote itself with its value stated needs no date: CF2 decides from that
write. The refusal changes nothing (WT10), it is a conflict to the caller
(CF6), and the move can be made again from a new read. A move that names no
read is refused (`NoRead`) where the host is a node of the committed state
that *T* has not written. A commit that changed only flat records under the
moved subtree wrote no host and is not dated: no hash commits to them, and
the move carries them as they are.

*What a move takes and what it writes.* The entry of a move (MV6) covers
every key of every column under every prefix that the moves of its batch
name, and under the prefix of every subtree that left with them when they
were stated (MV2). The key set of its commit names each of those prefixes
as a subtree it takes whole, as the key set of a drop names its subtree
(CF5).
The node writes of the batch, the old host's key and the new one's among
them, are keys of its change sets as for any batch. So:

- a foreign commit after the move that writes a key under one of those
  prefixes, in any column, or that drops, clears or moves a subtree under
  one of them, or one above one of them by the first rule of CF5, meets
  the move: the commit of *T* fails with `Conflict`;
- the commit of *T* meets every entry that another transaction made before
  it under one of those prefixes, and under a subtree below one of them by
  the two rules of CF5. The names that an enter if absent recorded above
  its host are prefixes of the committed state: they stay as recorded
  when the transaction that stated the enter moves a subtree itself.

Neither sees a foreign commit after the move that enters a subtree below
`from` and writes no key under a prefix the move names. A writer that
writes the hosts above its Merk writes such a key. What *T* then does is
in MV6.

*A batch that is laid out after the commit of a move.* A transaction of
whose writes the mover's commit met none is not a conflict for it. Where
that commit left a prefix with no subtree, because it took one from there
and brought none, a later batch of the transaction that writes into the
subtree it had there finds none entered and is refused. The refusal is
`ValueTaken`, for that subtree, where the transaction holds the key set of
a commit that names the subtree as one it moved or dropped: it holds key
sets from the first node it is shown (CF2a, CF3). Where it holds none, the
refusal is `NotEntered`. Where a subtree is entered under the prefix
again, the batch is judged against that one.

*A refusal of the move itself that a foreign commit explains is a
conflict.* The engine asks the key sets it holds of the commits since the
read the move names, or all it holds where it cannot date that read:

| The batch is refused because | Told as `ValueTaken` where such a commit |
| --- | --- |
| `from` has no subtree | wrote every key of the Merk the host is in, or took a subtree out |
| a prefix that a subtree goes to has a subtree | wrote the key of the node that hosts the moved subtree afterwards, or a key under a prefix that a subtree goes to |

Every other refusal of MV3 is told as it is. "Explains" is not "is the
cause": a conflict that is false needs a batch whose maker got it wrong.

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

The directory entry of a Merk (ST3, TREE-05) belongs to that Merk's unit,
though it is stored in a directory page and not among the Merk's cells. The
scope that holds the write claim on the Merk writes the entry, in the same
operation as the change that moves the root: the root cell is carried,
another node becomes the root, or the Merk becomes empty or stops being
empty (`tree-layout.md` S3, T3). No other scope writes it, and opening the
Merk through it is a read of that unit. Entries of different Merks can share
a directory page, as cells of different Merks share a tree page. LT1 to LT5
hold for a directory page as for any other.

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
- A move claims every column of every prefix it names, as a write of each
  (MV8).

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

**The marks can be asked for.** The tree layer can ask whether a (subtree,
key) is marked. It is answered from the marks the asking scope has added
and from the transaction's merged ones, in the order of SC4. A mark of
another scope that is still open is not shown, as its writes are not. A
snapshot is always answered "no".

`tree-layout.md` T3 needs this. A write that stops below the host of its
Merk cannot rewrite the host's cell, so the pointer to the hosted root in
that cell can be behind for as long as the host's element is marked. While
it is marked the pointer is not followed, and the root is taken from the
Merk's directory entry, which its own scope keeps exact at every moment
(SC2). Finalize rewrites the element, and with it the pointer, before it
removes the mark. A commit refuses a transaction that has a mark left
(CM1), so in every committed state the pointer and the entry agree.

Nobody needs to see the mark of another open scope. A mark is set by the
scope that wrote the Merk below the marked element, and that scope holds
the write claim on that Merk until it ends, so no other open scope can open
it (SC3).

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
why SC7 can promise the result of running one scope after the other.

**The second application goes through the tree layer.** It runs the scope's
change sets against the merged pages and finds every cell by its key, as a
rebase does. It does not copy the scope's cells into the merged pages by
their addresses: the scope that merged first may have carried cells that the
second one wrote or reached (`tree-layout.md` T2, and section 17 item 10
there). The layout rules are a function of the pages and the change set
(`tree-layout.md` T4), so the second application is well defined and gives
a valid layout. It need not give the layout the scope had on its own copy,
and SC7 does not ask for that. Every location into the scope's private
frames ends with it (PN8).

TREE-01 does not keep the Merks of different scopes on pages of their own
(`tree-layout.md` T6), so this reconciliation is needed wherever two open
scopes have changed one page. What a scope may move on a shared page is the
tree layer's rule (`tree-layout.md` T2, T3): the cells of Merks it holds a
claim on, and the small fragments hosted in them unless another open scope
has claimed those. For that the tree layer can ask whether another open
scope holds a claim on a unit. The answer is used for placement only, where
SC7 promises nothing. It can be out of date at once, since a claim never
waits: a fragment that was carried a moment before its Merk was claimed is
the case the second application is for.

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

| Level | Lock | Taken for |
| --- | --- | --- |
| 1 | the commit lock | commit, wipe, checkpoint sampling |
| 2 | a transaction's own lock | rebase, scope merge |
| 3 | the file-set lock | manifest replacement, file deletion, checkpoint sampling |
| 4 | frame latches, in ascending page id order | one physical step on a frame (LT3) |
| 4a | the value log's lock on its counts of live values | a change point that changes a count; a sample of the counts |
| 5 | the internal locks of the page table and the allocator, and the lock that makes a page step one step (PV7) | one step. Nothing else is acquired under them. |

Level 4a was added after levels 1 to 5 had been named in other documents,
which is why it has no number of its own.

One lock of level 5 is taken under another in one place: write-back pushes
the link of a block in the page step of its page (`page-table.md` T18), so
it takes the page table's lock under the lock of the page step. The page
table calls nothing while it holds its lock, so no thread waits for a page
step under it.

**The counts lock (4a).** Every change point that changes a count of live
values takes it: a commit whose cells stop holding value pointers, and a
page step that places a value, installs a move or removes a dropped cell
(PV6, PV7). The holder writes its record to the log, makes the cell change
and counts it, in that order, and then gives the lock back. So the log has
the changes in the order the counts took them, and none is in the log and
not yet counted while another thread reads the counts. A sample of the
counts is read under the lock, together with its place in the log (DU5).

- A page step takes 4a before its level 5 lock on the page, so that nothing
  is acquired under level 5.
- A commit holds 4a from before its append in step 6 of CM3 until step 7
  has counted the change. Step 7 installs through page steps (PV7) and
  takes no frame latch while it holds 4a.
- The holder appends to the log, so whatever lock the log has for an append
  comes after 4a.
- The value log's own locks on its set of files and on its file map come
  after its counts lock and are internal to it. They are not level 3.
- No reader takes it.

## 10. The flusher, the cleaner and page ids

### 10.1 Flusher

**FL1. The flusher writes only published page versions.** It never reads or
writes a private frame. It does not write every published one: what is
older than the free link of a freed page, and not yet written back, never is
(FL6). An open write transaction writes to no data file
(WT4): its values reach the log with its commit and the value log at
write-back (FL7). The only bytes a transaction has outside the log are those
of a spill file, which no manifest names and which recovery ignores (WT4).

Bytes still reach the value log before any cell points to them. The flusher
appends a value before the page step that places it, and the cleaner appends
a copy before the page step that installs the move (PV7). Until that step
the value is **held** for the run that appended it. No live-value count
includes it yet (`value-log-format.md` O7), so the file that holds it is
not deleted while the hold lasts. When the step has counted the value, the
hold is dropped. When the step is refused, or the run gives the value up,
the hold is dropped as well and the bytes are dead space, which goes with
its file. No hold survives a crash, and neither does a value that no
recovered change counted.

**FL2. What it writes keeps its born version** (PV5).

**FL3. Write-back and consolidation never destroy a needed page version.**
Writing a new base image for a page supersedes the old chain physically.
The old chain stays live while a needed page version (PV3) consists of it.
The flusher may fold several commits' deltas of one page into one write only
if no pinned version falls between them.

**FL4. Nothing newer than the log survives a crash: the log goes first.**
After a crash, no page version born after the recovered version *Lr* may be
reachable (DU3). So a manifest names a table checkpoint only once the log
is durable through:

- the newest version the checkpoint covers or reaches (`page-table.md` K4)
- the newest free link the checkpoint may have lost to trimming
  (`page-table.md` T17, K10). The table remembers the newest free link a
  trim has removed, and a checkpoint notes that version after it has copied
  its last chain. Without this, a freed page whose chain was trimmed away
  could be lost when the crash loses the commit that freed it
  (`page-alloc.md` section 8 item 9)
- the place of the count sample the checkpoint carries (`page-table.md` K14,
  DU5)

"Durable through *v*" means what recovery would reach: the groups up to *v*
are synced, in the part of the log that recovery reads (CM9). The rule is
enforced in one place, where the checkpoint is installed, and a checkpoint
that needs more is refused. It costs one sync of the log for each table
checkpoint.

Recovery therefore never discards a page version for being too new. If it
finds a link born after *Lr*, this rule was broken, and that is an error
(`page-alloc.md` R7). The first version of this document offered such
discarding as a second way and left the choice to STORE-06 and STORE-07. It
is unsafe unless trimming never passes the version the log is durable
through (`page-table.md` section 6 item 9).

Replay can still meet a change it has nothing to apply to (`page-table.md`
K11). A page was freed at version *w* and its chain was trimmed, and the
checkpoint was written after that and covers less than *w*. The log then
holds changes to the page from commits between the two, and the checkpoint
has no image of the page. Replay drops such a change. Nothing needs the
page version it belonged to: the log is durable through *w*, so replay goes
on to the commit that frees the page. The record of a background change to
that page, between the two commits, is treated the same way (DU2). Only the
change to the page is dropped. What the change did to a count of live
values is still applied (DU5), and the freeing commit then releases what the
page's cells held. When replay ends, recovery checks that every page with a
dropped change, of a commit or of background work, was freed, or made again,
by a later commit it replayed (STORE-07). If one was not, the checkpoint or
the log is damaged. A change that makes a page needs no image, and is never
dropped.

One more thing follows the same order. A value pointer is stored where a
later session will find it only under a durable manifest that covers the
value (FL7).

**FL5. Backpressure stops commits only between commits.** When write-back
falls behind (STORE-06), the committer waits before step 2 of CM3, not while
it holds the commit lock. Readers are never slowed by backpressure.

What the committer waits for must be able to happen while it waits.
Write-back that a private copy of the waiting transaction itself holds back
(PV7, FL7) cannot, so the committer is never made to wait for that.

**FL6. A log file is deleted only when nothing in it is the only copy** of a
needed page version, of a commit that the last durable table checkpoint does
not cover, of a pending value (FL7), or of a count change that recovery
would still apply (DU5).

**A copy that the buffer pool is bound to keep counts as a copy.** That is
the case for a page that has been freed (`page-table.md` T18). The commit
that frees a page gives it its free link at once, and from then on nothing
older of that page is written back. The changes of earlier commits that
only the log and the buffer pool hold then:

- leave the write-back queue
- count as written back when the version a table checkpoint covers is
  chosen (`page-table.md` K1)
- stay in the buffer pool, which cannot evict them, until no reader can ask
  for a version below the freeing one: no pin is below it, and the
  operations that were running when that became true have ended (PV3, PN6).
  Then the pool drops them.

Those page versions, and a pending value in one of them, are needed only by
readers of the running session, which the pool serves. No recovery needs
them. A checkpoint that holds the free link, or that lost it to trimming,
is named only once the log is durable through the freeing version (FL4), so
a recovery from it lands where the page is free. A checkpoint from before
the free link covers less than those changes, and the second case above
keeps the log for it.

So no log file is held back for the sake of those readers, although a pin
or a running operation may still read the page versions: neither survives a
crash (DU3). The other reasons stand. A log file that replay still needs
for a commit the checkpoint in force does not cover, or for a count change
after the sample's place, is kept as before.

**FL7. A new value reaches the value log at write-back, not at commit.** A
value that is too long for its cell (TREE-08) and that a commit writes
travels in that commit's log group, whole (CM3 step 6). The commit appends
to no value-log file, syncs none and replaces no manifest (CM7). That is how
two rules hold together: a commit does not fsync (DEC-04), and a value
pointer is stored where a later session will find it only under a durable
manifest that covers the value (`lignin-store::values`;
`segment-manifest.md` M13).

From its commit until it is placed, the value is a **pending value** of its
cell. In the cell it has the pending form: a value pointer that names no
file, whose 8 other bytes identify the value (P7 of `value-log-format.md`;
PD16 of `page-delta.md` for where it may stand).

- **Where it is.** Its bytes are in the log group and in the frame of its
  page version. The store keeps that frame until the value is placed, or
  until the page version is no longer needed and no running operation can
  still reach it (PV3, PN6). It keeps the log file as well, except where FL6
  lets the frame stand for it.
- **Reading it.** A read through any handle that sees the commit is answered
  from the frame. It reads neither the log nor the value log. It returns the
  bytes, and charges the cost, that it will once the value is placed (RD9).
- **After a crash** recovery replays the group (DU2), and the value is
  pending again.
- **Counts.** No live-value count includes it: it has no pointer (PV6).
- **A carried cell.** A cell that a commit carries to another page gets its
  value again as a pending value of the new page
  (`value-log-format.md` O5), and stops holding its old pointer at that
  commit. Whether the group repeats the bytes or names the record they are
  in is FMT-05's. If it names the record, that record must stay readable
  until the value has been placed again.

**Write-back places it.** The flusher appends the pending values of a page
to the value log under a hold (FL1), has a durable manifest cover them, and
then gives each cell its pointer in a page step (PV7). The step takes no
version, counts the value in its file and is recorded with the pages (PV6).
The block that write-back then writes for the page holds the pointers.

The checks of PV7 apply. While a write transaction holds a private copy of
the page, the step is refused and the value stays pending. Unless that
transaction replaces or deletes the cell, its commit carries the value on,
still pending, in the page version it installs, and the flusher places it
then. Its bytes stay in the group that first carried them.

A page version can be superseded while it still has a pending value. If no
pin needs that page version (PV3), the value is never placed. If one does,
the page version must stay readable (PN2), and nothing is counted for it,
because the counts are those of the latest version. How it is kept is
STORE-06's choice: in memory, with its value in the log, or with the value
placed for that version alone under a hold that lasts for as long as the
version is needed. For a page that has been freed there is no choice: FL6
keeps it in memory and writes nothing of it back.

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
rewrites the pointer in its owning cell, in a page step (PV7). The move is
abandoned, and tried again later, when a check of that step fails:

- a commit has superseded the page since the cleaner looked at it, so the
  page version the cleaner found the owner in is no longer the current one
- the cell no longer holds the old pointer
- an open write transaction holds a private copy of the page at the moment
  of the step

A private copy matters only if it is held at that moment. One that was made
after the cleaner looked and given up before the step is harmless: its
transaction dropped the copy in a rollback or a rebase, so nothing can
commit the old pointer from it. (A transaction that committed the copy
superseded the page, which is the first case.) An implementation may refuse
for such a copy too. It need not.

The old entry stays live while any needed page version still points to it.
So a file that such moves emptied while *L* was *v* is deleted only when no
version before *v* is pinned (`value-log-format.md` O8) and PN6 allows it.
It is also deleted only once the rewrites that emptied it, and the copies
they point to, are durable: otherwise a crash could bring back pages that
point into a file that is gone.

**CL6. What a dropped subtree leaves behind is reclaimed when no pin can
reach it.** The drop unlinks the subtree in the commit that drops it, at
version *w* (TREE-09, CV9). What it leaves is retired at *w* and reclaimed
under PN6: once *H* ≥ *w*, and once the operations that were running then
have ended.

Reclaiming is a walk, not a list of pages, because the cells of a dropped
Merk can share pages with cells that live on (`tree-layout.md` P2, D2). The
reclaimer (TREE-09; for section 13 it is part of the cleaner) follows the
dropped Merk from its root. It removes each cell, removes the directory
entries of the Merks nested in the subtree, reports the value pointers the
removed cells held (`value-log-format.md` O6), and frees a page when its
last cell is gone.

- **It is a background change.** It takes no version, and each removal has
  a change point at which the counts of the reported value pointers change
  (PV6). Until then those pointers stay counted, which keeps their files.
- **Each removal is a page step, under the checks of PV7.** In particular
  it is not installed while an open write transaction holds a private copy
  of the page. So no commit brings a removed cell back, and no commit
  installs a version of a page the reclaimer has freed.
- **A page is freed once.** A page whose cells are all left-overs is freed
  by the reclaimer and by nobody else, and a page that still has a live
  cell is not freed by it. The reclaimer frees with the drop's version *w*,
  or with any later version up to *L* (`page-alloc.md` A4). The id is then
  reused under ID2 and ID3.
- **It does not merge.** A page that reclaiming leaves nearly empty stays so
  until a write touches it (`tree-layout.md` D3).

As built (`lignin-tree::drop`):

- **The order.** The directory records of the nested Merks go first, the
  innermost first, then the record of the dropped Merk, then the cells from
  the root down. Until the record of the dropped Merk is gone, no cell is,
  so a reclaimer that begins again finds every pointer exact.
- **A crash while the cells go leaves the rest of them.** From the step
  that takes the Merk's record out, only the reclaimer's memory knows the
  cells that are left. They are in no state and no read returns them, and
  the value pointers they hold stay counted (`tree-layout.md` D2).
- **A page is freed with the latest version**, since nothing keeps the
  drop's. A page whose current version the latest commit bore waits for the
  next commit.
- **A write copies a tree page before it is planned on it** (WT11, PV7).
  A step takes cells out of a page that has no private copy, and a write
  that had read the page before would change another page than it planned
  on.

**CL7. The cleaner is never in a reader's or the committer's way** beyond
one page step, and for the committer the hold of the counts lock that goes
with it (LT7 levels 4a and 5).

### 10.3 Page ids

**ID1. An id allocated by an open transaction is private** until it commits
(WT4). The transaction keeps every id it allocated. It gives one back only
when no rollback of its own can bring the page back: when it rolls back,
when it rolls back the scope or the savepoint that allocated the id, or at
its commit, for a page it created and emptied again (`page-alloc.md` N5).
So within one transaction a page id names one page. The commit publishes
the ids of the pages it installs, in step 7 of CM3.

**ID2. A freed id is reused only when no pin can reach the old page.** A
page freed by the commit of version *w* was reachable at versions below *w*.
Its id may be handed out again once no version below *w* is pinned, that is,
once *H* ≥ *w* (STORE-10). This is what "a page id identifies a page only
together with a version at which it is reachable" (format.md 3.1) needs.

A commit frees its pages in step 7 of CM3, with its own version, before it
is published. The reclaimer of a dropped subtree frees with the drop's
version or a later one (CL6). Whoever frees a page vouches for the version,
and that the id still names the page it means (`page-alloc.md` A4, N7).

**ID3. Reuse is also held back by operation guards** (PN6): an operation that
read the id before it was freed may still be using it. The allocator applies
both tests through the three questions of PN6. The engine asks it to
reclaim after a commit is published and after the oldest pin is dropped
(`page-alloc.md` N8).

**ID4. Nothing of the allocator is on disk.** After a crash it is derived
from the page table that recovery loaded and from what replay reports: for
each commit recovery keeps, the pages it wrote and the pages it freed
(`page-alloc.md` R1 to R7). So the log says which pages a commit freed
(FMT-05). An id that an open transaction held, or that a lost commit
published or freed, comes out as the version recovery landed on has it
(DU3, CV6). No id is private after recovery.

**ID5. Nothing else holds an id back.** Not durability: a crash that lands
below the freeing version finds the old page through the rules that keep
its chain (FL4). Not the page table: the freed page's chain, with its free
link, can still be there when the id is handed out. Not the buffer pool: a
frame of the old page can still be cached. So a structure keyed by page id
tells the pages of one id apart by their versions, as it must for a
snapshot (format.md 3.1; `page-alloc.md` A11).

## 11. Checkpoints

**CK1. A checkpoint is exactly one committed version.** It holds *S(vc)* for
the *vc* that was *L* at its sampling step: every commit up to *vc*, nothing
of any later commit, and nothing uncommitted.

**CK2. Sampling is one step.** The checkpoint records *vc*, the manifest it
starts from, the set of live files and the valid length of each, while
holding the commit lock and the file-set lock. No commit is published, no
manifest is replaced and no file is deleted during that step. The log length
it records ends at the end of a record, at or after the group of *vc*. It
holds no group of a later commit. It can hold records of background changes
that were installed after that group (PV6): they change no record of the
state, and each of them points only at what the sampled manifest covers
(FL7).

**CK3. From sampling until its files are linked, a checkpoint holds a
version pin on *vc* and its file set.** No file in the set is deleted in
that time (CL3). Both are released when creation ends, whether it succeeded
or not.

**CK4. It makes *vc* durable.** The files are fsynced up to their recorded
lengths before they are linked (TXN-06), so *vc* is durable in the live
database as well (DU1).

**CK5. Commits go on after sampling.** Bytes appended to a shared file past
its recorded length are not part of the checkpoint: its manifest gives each
file's valid length. A commit never waits for the linking. It waits for the
fsync only in the one case of CM8: a commit whose sync policy has it fsync
the log (CM7) waits at its own fsync for an fsync of the log that the
checkpoint is making then. No other commit waits for it.

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
under it. It takes the commit lock only to record a failure, and no other
lock of LT7 but the one on the counts (4a), for a moment before the fsync
and after it, where it reads how far the counts are durable (DU5). So no
reader and no write transaction waits for it. A commit can wait for a
`flush()` whose fsync succeeds at two of the locks this document names,
and at no other:

- at the counts lock, for one of those two moments, as it waits there for
  any other holder (CM8);
- at the fsync, if its sync policy has it sync at the commit (CM7). The
  fsync is the log's, which makes one at a time (STORE-01), so such a
  commit waits for the fsync of a `flush()` as it waits for that of another
  commit, and holds the commit lock while it does. A commit whose policy
  does not sync does not join the fsync.

A checkpoint running at the same time fsyncs for itself (CK4); the two do
not need an order.

If the fsync fails, the engine records under the commit lock that the
session takes no write any more (D-098), so that every commit is published
before that record or refused. A commit of any sync policy that comes to
the commit lock at that moment waits for the record there, and is then
refused.

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

The records of background changes lie in the same log, between the groups
(PV6). They change no record of the state, so they do not move *Lr*.
Recovery reads them under the same test and to the same point. What it does
with one has two parts, and each is decided by itself:

| Part of the record | Recovery |
| --- | --- |
| its change to the page | applies it, unless the page already has the change (DU5) or the checkpoint has no image of the page (FL4). Then it skips it. |
| its change to a count of live values | applies it exactly when the record is after the place of the count sample (DU5), whatever became of the first part |

The same two parts, under the same two rules, are what the group of a commit
is to one page it changed (`page-table.md` K3 and K11).

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

**DU5. The counts of live values are recovered from a sample and the log.**
They are part of the page state (PV6) and are stored with it, not in the
value log's files and not in the manifest.

- Every change point that changes a count stores the change with its
  record: the log group of a commit, the record of a page step (LT7).
- A **sample** of all the counts is stored in the table checkpoint file
  (`page-table.md` K12) with its **place in the log**: the counts in it
  include every change whose record is before that place, and none whose
  record is after it. It is read under the counts lock.
- Recovery takes the sample of the table checkpoint that the recovered
  manifest names. To it, it applies the count change of every record it
  keeps after the sample's place, each once, in the order of the log
  (STORE-07). The counts are then those of the page state recovery lands on
  (`value-log-format.md` O7). With no checkpoint named there is no sample:
  the counts start from nothing, and every change in the log applies.

What the counts include is decided by the place alone. A table checkpoint
copies each chain at another moment (`page-table.md` K5), so a page in it
can already hold a change whose record is after the place. Replay skips
that change for the page, which has it (`page-table.md` K3), and still
counts it. It also still counts a change that it dropped because the
checkpoint has no image of the page (FL4).

A background change has no version, so replay cannot tell by version
whether a page already has it. Its record must be recognisable against the
page. For a value pointer rewrite the test is that the cell no longer holds
the old pointer. That is enough, because a pointer is never given to a
second cell.

Four things follow for the checkpoint and the log:

- A sample is in force only with its checkpoint, and only once the log is
  durable up to its place (`page-table.md` K14, FL4).
- "Covers version *C*" (`page-table.md` K1) speaks of commits. Whoever takes
  a table checkpoint vouches for background changes as well: every change
  whose record is before the place where replay starts for the pages is in
  the checkpoint's chains. The one exception is K1's own: a change to a
  page whose free link has been pushed since (FL6).
- Recovery reads the log from two places, the group of *C* + 1 for the
  pages and the sample's place for the counts, and the log is kept from the
  earlier one (`page-table.md` K13, FL6). The changes between them are
  replayed for the pages only, or counted only, whichever place comes
  first.
- The places of successive samples never go back. A later checkpoint with
  an earlier place would ask for log that may be gone. The table does not
  check this. STORE-06 takes the samples and deletes the log files, so it
  keeps the rule (`page-table.md` K15).

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
| 4 | Reader, flusher | frames and locations of page versions; pending values and what was never written back of a freed page, both of which a reader takes from the frame | PN5, PV3, PV4, FL3, FL6, FL7 | never |
| 5 | Reader, cleaner | blocks, files and page ids the reader's version needs | PN2, PN6, CL1 to CL5, ID2 | never |
| 6 | Reader, checkpoint | files | PN5, CK7 | never |
| 7a | Writer, writer in the same transaction | units, private frames, the transaction's merged writes | SC3, SC4, SC6, SC7, LT1 to LT5 | one physical step (LT3); a claim never waits |
| 7b | Writer, writer in another transaction | nothing until one commits | RD8, WT1, WT4 | never |
| 8a | Writer, commit of its own transaction | the write set | CM1, SC8 | commit is refused while a scope is open |
| 8b | Writer, a foreign commit | *L*, the keys both wrote, and locations the writer kept | RD3, RD4, WT7, WT7a, WT7b, PN8, CF2, CF3 | never; the writer rebases at its next operation |
| 9 | Writer, flusher | the committed pages a writer has copied; never a private frame, and no data file: a writer writes to none (WT4) | FL1, FL5, FL7, WT4, PV7 | never; the flusher leaves a copied page's values as they are, the pending ones pending and the placed ones in place |
| 10 | Writer, cleaner | the committed pages under the writer's frames, their value pointers, what a dropped subtree left in them, page ids, and locations the writer kept | PN1 (base pin), PN5, PN8, PV4, PV7, WT3, WT11, CL5, CL6, ID1, ID2 | never; the cleaner leaves a copied page for later |
| 11 | Writer, checkpoint | nothing: uncommitted writes are not captured | CK1, CK8 | never |
| 12 | Committer, committer | *L*, the log, versions | CM2, CV4, CV5, CF2, CF7, CF8 | the second waits for the commit lock |
| 13 | Committer, flusher | the log, the page versions just born with their pending values, pages whose values the flusher places or regroups, and the counts of live values | CM3 steps 6 and 7, CM8, CM9, FL1, FL4 to FL7, PV6, PV7, LT7 | the committer, before it takes the lock (FL5); otherwise one page step, or one hold of the counts lock by either; a commit that fsyncs the log (CM7) waits for an fsync of the log that the flusher is making (CM8) |
| 14 | Committer, cleaner | page-table entries; pages and ids the commit frees; pages whose value pointers the cleaner rewrites or whose left-over cells it removes; the counts of live values | CL4 to CL7, PV6, PV7, ID2, PN6, LT7 | one page step, or one hold of the counts lock by either |
| 15 | Committer, checkpoint | *L*, the log's length, the file set | CK2, CK5, LT7 | each waits for the other's hold on the commit lock; the checkpoint holds it only for sampling; a commit that fsyncs the log (CM7) waits for an fsync of the log that the checkpoint is making (CM8, CK5) |
| 16 | Flusher, flusher | the pages being written back, and the counts of live values | PV4, FL3, FL7, LT7 levels 4a and 5 | one page step, or one hold of the counts lock |
| 17 | Flusher, cleaner | page-table locations, segment and value-log files, the manifest, pages both would change in a page step, and the counts of live values | PV4, PV7, FL1, FL3, FL7, CL2, CL4, CL5, LT7 levels 3, 4a and 5 | the file-set lock; one page step, or one hold of the counts lock |
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
| `flush()` | DU1a; it is an fsync of the log, which makes one at a time. Of the locks of LT7 it takes the one on the counts for a moment before and after the fsync, and the commit lock only after an fsync that failed, to record that the session ends |
| wipe | CV3, CF5; it is a commit, so a snapshot taken before it keeps reading its own version (RD1) |
| subtree drop, and a clear that keeps the host | CV9, CF5; reclaiming what it leaves is the cleaner's (CL6) |
| a committed read | RD10; it is a read, and takes what a read takes |
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
A `StorageBatch` is a map from key to one put or delete of that key, not a
list of everything Merk did (TXN-08). The write-set entries of an operation
are the entries the batch holds when it is applied, one per key. First-write
versions are tracked over them, not over Merk's steps.

Which call survives for a key is not simply the last one
(`StorageBatch::put`, `delete` and `Operations::delete_into` in
`storage/src/storage.rs`). The batch counts **operation generations**:
`StorageBatch::next_operation` starts a new one, and a caller that runs
several Merk commits against one batch calls it between them (the repair
of a ranking on grove versions 1 to 4 does not, see below).

| Call | Kept when | Otherwise |
| --- | --- | --- |
| put | always. It replaces whatever the batch holds for the key. | |
| delete | the batch holds nothing for the key, or holds an entry of an earlier generation, which it replaces | dropped: the entry of the same generation stays |

So inside one generation a put wins over a delete of the same key, whichever
of the two came first, and of two deletes the first stays, with its cost
information. Rebalancing deletes a node and puts it again in one Merk
commit, and the put is the intended end state. Across a boundary the later
operation wins: its delete removes a key an earlier operation had put. The
four columns are kept apart, as the column is part of the key (ST1).

The put also wins where it is not the intended end state. Two Merk commits
of one generation can write one Merk: a node that the first put and the
second deleted stays in the batch as a put. Where the tree that the batch
leaves does not reach such a node, its record is one that no node links
(ST1). The repair of an indexed tree's rankings on grove versions 1 to 4
makes one commit for each row it removes, in one generation, and leaves
rows so (the corpus case `ranking-drift`). The write-set entry of such a
key is the put, as the batch holds it.

Two ways of joining batches follow from the two calls. `merge` replays the
other batch's entries through put and delete in the current generation, so
one of its deletes does not replace an entry this batch made in that
generation. `merge_overwriting` lets every entry of the other batch replace
this batch's entry for its key, and then starts a new generation.

TXN-08 keeps exactly this on LigninDB: the surviving entry is the write-set
entry, it is what the transaction reads back under that key (RD3), and it is
the only one charged (`cost-model.md` B1).

- A key that Merk deleted and put again inside one Merk commit is one entry,
  a put, in whichever order the two calls came.
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
writes. On LigninDB that is a committed read (RD10), made by the operation
that applies the batch, so it reads the *L* the batch's first-write version
is taken from. `cost-model.md` D2 says when it happens and what it charges.

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

The argument needs a committed root node. A Merk that was empty when *T*
first wrote it has none: *T* and the foreign commit each create a root, and
those can be different keys. On existing GroveVersions the two still meet,
in the element that hosts the Merk, which both rewrite (GM1). In iteration
two a write can stop below the host, and then they share no key until
finalize. GROVE-11 has to settle that case where it defines stop points
(section 18, open point 1).

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
conflicts (GM3), if *M* had a root when the transaction first wrote it.
RocksDB is gone by then, so there is nothing to match; GROVE-11 should state
this where it defines finalize.

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
| D3 | Flat subtree drop | the drop's commit deletes the element and writes a pending-drop record. The subtree's records stay until `flush_pending_prefix_drops` deletes them: one range delete per prefix and column family, then one delete of the record, each its own write (`storage.rs:928-966,994-1005`) | the subtree's records are removed in the drop's commit (CV9). The flush checks liveness as today and deletes each pending-drop record with a write with no transaction: one commit per record. Its range delete finds nothing entered under the prefixes of such a record, and drops what it does find. The cells and pages the drop left are reclaimed by the engine's background work, which takes no version (CL6) and does not wait for the flush | TREE-09 unlinks at the drop. The records in question are unreachable from any element. Between the drop and the flush the RocksDB engine still returns them in two cases: to a read of a dropped prefix made directly, and to a caller that re-creates the dropped path before the flush, which callers are told not to do. LigninDB returns them in neither (CV9). **Open, `MERK-05-F2`:** a third reader sees them on RocksDB in that time, and is to see the same on LigninDB: a cursor of the subtree that is the neighbour of a dropped prefix in its column, at a step from no key, which lands on the dropped records there. A descending range that excludes its end and ends past the last key of that neighbour answers its rows on RocksDB where the dropped records follow it and nothing else does, and none on LigninDB, inside the dropping transaction from the drop on and for every reader until the flush. The follow-up keeps, for each prefix of a pending-drop record and each line, that it held a record, until the range delete names the prefix. The same holds, with no flush to end it, for the records of the trees that a flat drop leaves behind against its caller's word, for those of a tree that an element was written over (`GROVE-03-F1`), and for those of a tree whose element a clear removed that was told to look for no tree (`check_for_subtrees: false`). **Open, `GROVE-03-F1`:** the records of such a tree, and of a tree that an element was written over, are also read on RocksDB by the path of the tree where no element is asked for on the way (`has_raw`), and by a tree that is made again under its key (`is_empty_tree`), with no flush to end it; LigninDB is to answer the same. |
| D4 | Commit when the memtable history is too short | `TryAgain` | CF2 decided exactly (CF9) | node-local |
| D5 | A range delete outside *T* over a key *T* wrote | `Busy` or success, depending on the memtables | `Conflict` (CF9) | node-local; not reached by Platform's call sites |
| D6 | Records loaded outside *T* under a key *T* wrote | success | `Conflict` (CF9) | devnet seeding only; not reached there |
| D7 | A Merk node that a foreign commit wrote between *T*'s read of it and *T*'s write of it with its value kept | success; the node holds the value *T* read, and the foreign write is lost | `Conflict` (CF2a, CF9); where the foreign commit took the node away, the write is refused, as a conflict where the engine can tell (`ValueTaken`, CF2a) | needs two writers of one Merk at a time. A caller that retries on a conflict sees one where RocksDB would have dropped the other writer's value. |
| D8 | A move of a subtree whose host a foreign commit wrote between *T*'s read of the host and *T*'s batch | success; the copy holds what *T* read, and the foreign write under the old path is lost | the batch is refused as a conflict, and the transaction stays open (`ValueTaken`, CF10) | a move of LigninDB re-keys the Merk as it is at the batch (`tree-layout.md` D5). Needs two writers at a time; a caller that retries makes the move again from a new read. |
| D9 | A write into a subtree that a foreign commit moved away or dropped, so that nothing is entered under its prefix, by a transaction of whose writes that commit met none | success; the records are under the old prefix, which no element leads to | the batch is refused, as a conflict where the engine can tell (CF10) | needs two writers at a time. RocksDB then holds records that only a read of the prefix made directly returns, or a caller that makes the path again, as in D3. |
| D10 | A read or a write where a move of *T* decides, after a foreign commit has met a write of *T* | served; the commit fails | refused as a conflict (`MovedByKey`, MV7); the commit fails | needs two writers at a time. The transaction fails on both engines, on LigninDB one step earlier. |

Three things that might look like differences are not. A second write
transaction is allowed on both engines and conflicts by the same rule (CF7).
Both engines end the transaction when a commit fails (CM6). Version numbers
are internal to the engine: nothing above it sees how many a wipe or a flush
took.

## 15. What the tasks take from here

### 15.1 By task

| Task | Takes from here | Still its own |
| --- | --- | --- |
| SEAM-01, MERK-01 | ST6: a change set names exactly the node records Merk writes today. Fetch by link must be answerable by child key (RD5). A location kept in a link is checked before it is followed (PN8). A location in a private frame also ends when the cell is carried or deleted, and the key found at it proves nothing (PN8). | The trait and the change-set types. |
| SEAM-04, GROVE-01 | The four handle rows of section 4. `Conflict` maps to the engine-neutral conflict error (CF6). `ReadOnly` maps to `SnapshotReadOnlyTransaction`. A read with no transaction is a snapshot per operation (RD2), a write with no transaction a transaction per operation (RD7). The seven steps of 14.3. | How scopes map to operations in iteration one (SC1). |
| SEAM-05, MERK-05 | RD10: the committed read that a delete with no cost, `clear` and `Merk::clear` are charged from (`cost-model.md` D2). | The cost functions, and how Merk asks for the read. |
| TXN-02 | RD1, section 5, PV1 to PV3, LT6. The three questions of PN6 (`Pins`, `page-alloc.md` N1 to N3) and the order of guard and read in PN5 (N4). When the engine asks the allocator to reclaim (ID3). | The pin and epoch mechanism (PN4, PN6). |
| TXN-03 | RD3 to RD6, RD10, section 7, PN8, PV7. The ids a transaction holds (ID1). A transaction writes to no data file (WT4). | Frame and write-set structures; how to rebase (WT7); how a stale location is detected (PN8); how the write set stays self-contained (WT3); how the committed read is answered (RD10); spilling (WT4). |
| TXN-04 | WT8, SC8. The ids a rolled-back savepoint allocated go back at once (ID1). | The undo marks. |
| TXN-05 | Section 8, including the failed commit (CM9). The group carries new values whole, and the log is the only file a commit writes (CM3 step 6, CM7, FL7). Step 7 publishes and frees page ids and counts released value pointers, under the counts lock (CM3, LT7). | The log group; how CF3 is kept. |
| TXN-06 | Section 11, DU1. Where the sampled log ends (CK2). The count sample comes with the table checkpoint (DU5). | Linking, the copy fallback, the checkpoint manifest. |
| TXN-07 | Wipe is one commit over the range "everything" (CV3, CF5). `flush()` is DU1a. | The facade. |
| TXN-08 | A batch is one operation: one read of *L*, one first-write version, one entry per key it ends up holding (RD4, WT5, GM0). Which call survives for a key, by operation generation (GM0). | Batch semantics. |
| TXN-09 | Section 9. The directory entry of a Merk is in that Merk's unit (SC2). Marks and other scopes' claims can be asked for by the tree layer (SC9, LT4). | Claim tables, merge, the scope's undo. |
| TREE-03 | LT1, LT4: it runs a scope's change sets again on merged pages, by key. It may ask whether another open scope has claimed a unit, for placement only. | The layout rules (`tree-layout.md`). |
| TREE-05 | ST3, SC2, SC9, CV9: an entry is no record; it is written by the scope that holds its Merk and is exact while the host is marked pending; an entry that a drop left behind is never returned. | The entry, and how a lookup tells a left-over one. |
| TREE-06 | RD6. | The cursor. |
| TREE-09 | A drop is a range delete in its transaction's write set (CV9, CF5). What it leaves is reclaimed by a walk, under CL6: after the pins and guards, in page steps (PV7), with a page freed once, at the drop's version or later. | The unlink, keeping the root's address, and the walk. |
| STORE-03 | FL4 is `page-table.md` K4, K10 and K14, enforced where a checkpoint is installed. The count sample of DU5 is stored in its checkpoint file (K12). | The table and the checkpoint file. |
| STORE-04 | LT2 to LT6, PN5, and the page step of PV7. A frame with a pending value is kept until the value is placed (FL7). What was never written back of a freed page is kept, not evictable, until no reader can ask for it (FL6). | Frames, latches, eviction. |
| STORE-01, STORE-06, STORE-07 | FL1 to FL7, PV5, DU1a, DU2, DU3, DU5, CM9, ID4. The log goes first (FL4). Write-back places pending values in page steps (FL7, PV7). No committer waits for write-back its own copies hold back (FL5). Replay reports written and freed pages (ID4) and recovers the counts (DU5). Nothing older than a page's free link is written back, and no log file is held for it (FL6). Replay drops a change to a page the checkpoint has no image of, and checks that the page was freed or made again (FL4). The places of count samples never go back (DU5). | Thresholds, the table checkpoint, replay, how a failed group is put out of reach, and how a superseded page version with a pending value is kept (FL7). STORE-06 measures how long a pending value waits under a private copy, and how much log it holds (section 18, open point 2). |
| STORE-08 | CL1 to CL7, PN6 with its three questions, CK3. | Liveness accounting, throttling. |
| STORE-09, TREE-08 | PV4, PV6, PV7 and CL5: a value pointer rewrite takes no version, is installed in a page step that checks the current page version, the owner and private copies held at that moment, changes the counts in that step, is recovered with the pages, and an emptied file goes only when CL5 allows. FL1: a value appended ahead of its page step is held. FL7: new values come from write-back, not from commits. The counts lock is level 4a of LT7, and the counts recover by DU5. | When a file is cleaned, when a value is durable, regrouping, and what makes the page step one step. |
| STORE-10 | ID1 to ID5, and the three questions of PN6. | The allocator. |
| FMT-05 | The log group carries new values whole (FL7), says which pages the commit freed (ID4) and which value pointers its cells stopped holding (DU5). A background change has a record of its own in the log, with its count change (PV6, DU5). | The records. Whether a carried value is repeated or named (FL7). |
| GROVE-05 | The drop removes the subtree's records in its own commit (CV9). The flush removes each pending-drop record with one commit, and what the drop left is reclaimed without a version (CL6) by the engine's background work (14.5 D3). | Done: `engine.md`, "A subtree leaves with its element". A clear that keeps the host is the deletes of `Merk::clear` (CV9). |
| GROVE-11, GROVE-12 | SC9 (pending marks), 14.4, SC3 and SC7. A scope's Merk cache keeps locations only under PN8. A host marked pending is not followed (SC9). GM3 does not cover a Merk that was empty when it was first written (14.2). | Stop points, finalize, the GroveDB scope. The rule for two transactions that each create the root of one empty Merk (GM3). |
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

**Read rule (TXN-02, TXN-03, MERK-05, GROVE-01)**

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
- T-RD8: the committed read (RD10), owed by TXN-03. For a key the
  transaction created it answers "no record". For a record it replaced,
  deleted or dropped it gives the committed sizes, for a node the fields of
  `cost-model.md` section 4. It gives the newer ones after a foreign commit,
  and the same answers after a rebase and after a rollback to a savepoint.
  Owed by MERK-05 and SEAM-05, on both engines: a tree that is filled and
  then deleted whole in one transaction, and one that was committed first,
  are charged as `cost-model.md` D2 says (section 11.1 there has both rows).

**Conflict rule (TXN-05, TXN-08, GROVE-01)**

- T-CF1: every case of the tables in 16.3 and 16.4 whose LigninDB result is
  not marked as a difference gives the listed result. Both engines.
- T-CF2: the differences of CF9 give `Conflict` on LigninDB: a foreign range
  delete, a wipe and a load over a key the transaction wrote.
- T-CF2a: a kept value is dated at its read (cases A19 to A19d). Both
  orders of the foreign commit and the write; a foreign delete, clear and
  drop that take the node away (`ValueTaken` where the read is dated, the
  plain refusal where it is not); a read that cannot be dated; a
  transaction with no write beside a thousand commits holds 64 key sets,
  and one with a write holds them all (`lignindb/src/commit/tests/kept.rs`).
- T-CF3: a model test. A random schedule of puts, deletes, range deletes,
  savepoints, rollbacks and commits over several transactions is run against
  a model that evaluates CF2 directly. Every commit outcome agrees, in both
  directions: no missed conflict and no extra one.
- T-CF4: after 400 MiB of foreign writes during an open transaction (the
  setup of case A11b), LigninDB still answers CF2 exactly.
- T-CF5: operations that apply an empty batch add nothing (cases B18, B20,
  B22), and a plain insert of an equal element does write (B19). Both
  engines.
- T-CF6: which call survives in a batch (GM0), owed by TXN-08. In one
  operation generation a put followed by a delete of the same key leaves the
  put, and so does a delete followed by a put; of two deletes the first
  stays. After `next_operation` a delete replaces an earlier put. `merge`
  and `merge_overwriting` join batches as GM0 says. In each case the
  transaction reads back the survivor, holds one write-set entry for the
  key, and is charged for the survivor only. Both engines.
- T-CM1: every write and every fsync of step 6 of a commit is made to fail
  or to tear at every boundary (IO-02), under the default policy and under
  one that fsyncs each commit, and the outcome is checked against a model
  that applies recovery's test to the log (CM9). A commit reported as failed
  is never recovered, whenever the crash comes: at once, after the cut of
  item 4, or after later commits. A commit reported as succeeded or as
  `NotDurable` is recovered whenever its bytes survive, and always after a
  later successful `flush()`. A group of full length whose bytes do not
  verify counts as failed. Every handle reads what it read before a failed
  commit, and the next commit takes the same version and is recovered. The
  record of a background change is never appended behind failed bytes: one
  that was installed is recovered whenever its bytes survive (CM9).

**The move of a subtree (TXN-03)**

- T-MV1: a move against a model that knows a move (MV1, MV2, MV4, MV5):
  every read by key and by cursor, in every column, after a move, after
  two subtrees changed places, after a chain, after a tree and an item
  changed places and after a move of a subtree five deep; one transaction
  that moves a subtree several times with writes at every prefix and a
  tree entered again at the old key, cleared and dropped there; and what
  another transaction reads after the commit
  (`lignindb/src/txn/tests/moves.rs`).
- T-MV2: after a move the directory and every flat column hold, record for
  record, what they hold where the subtree was written under the new
  prefix from the start: the entries with their ids, the records that lead
  from a host to its Merks and from a root's cell to its Merk, the id the
  next Merk gets, and the flat records, those of a subtree that keeps its
  data flat among them. The physical checker finds the store sound, also
  through `Db::verify`, and the next session reads the same
  (`lignindb/src/txn/tests/moves.rs`, `lignindb/src/db/tests/moves.rs`).
- T-MV3: every refusal of MV3 and of MV8, and those of the staged batch,
  each leaving the transaction as it was
  (`lignindb/src/txn/tests/moves.rs`, `lignindb/src/batch/tests/rules.rs`).
- T-MV4: a rollback to a savepoint from before a move; frames that are made
  again over a directory with another subtree below the moved one; a move
  that cannot be made again (MV6).
- T-MV5: CF10 and MV7. The dating, with the foreign commit before the read
  and after it, with no read named and with a read that cannot be dated; a
  foreign commit after the move under each kind of prefix its entry
  covers, above the source and below the target; the mover's commit
  against the transactions that wrote beside it; a batch that is laid out
  after it; the refusals that a foreign commit explains, with a drop in
  the batch and without; every read and write by key where a move decides,
  a cursor as the first call after the commit among them; and the scopes
  of MV8. Eleven planted faults of these rules are each caught by a test.
- T-MV6: the power is cut after every change of a session whose commit
  moves subtrees: two change places, with flat records under each and
  under the subtree below it, and a subtree with no node leaves. Every cut
  recovers a version the session published, with what that version held,
  and the store is sound; a cut that comes after the grove was made
  durable recovers the version before the commit of the moves or its
  version (`lignindb/src/db/tests/power.rs`).

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

**Scopes (TXN-09, TREE-03, TREE-05, VER-05)**

- T-SC1: two scopes on disjoint units, every interleaving that loom
  explores: the records, hashes and costs equal the serial run (SC7).
- T-SC2: each overlap is refused at once and leaves the scope usable: write
  against write, write against read, read against write (SC3).
- T-SC3: a scope rollback beside a merging scope leaves exactly the merging
  scope's writes (SC6), also when their Merks share a page (LT4).
- T-SC4: commit and savepoint with a scope open fail with `ScopesOpen`;
  commit with a pending mark fails with `NotFinalized` (CM1).
- T-SC5: the directory entry and the marks (SC2, SC9), owed by TXN-09 and
  TREE-05. Two scopes write two Merks whose directory entries share a page,
  and each moves its root: both entries are exact after either merge order.
  A scope that stops below its host marks the host's element; while the
  mark stands, a read of that Merk takes the root from the entry and never
  from the host's cell; the question about a mark is answered from the
  asking scope's marks and the merged ones, and "no" for a snapshot.
- T-SC6: the second application (LT4), owed by TXN-09 and TREE-03. The
  scope that merges first carries cells on a page the other scope has
  changed, among them a small fragment of a Merk the other scope claimed a
  moment later. After both merges the records, hashes and costs are those
  of the serial run, and the pages pass the physical invariant checker
  (VER-07).

**Pins, reclamation and background work (TXN-02, STORE-06 to STORE-10,
TREE-09, VER-05)**

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
- T-PN6: the three questions (PN6), owed by TXN-02. Its answers are put in
  place of the model that STORE-10's model check uses for pins and guards
  (`page-alloc.md` section 6), and the check passes. With one thing broken
  at a time it fails: the oldest pin answered one version too high, a
  commit that is installed and not yet published taken for the latest
  version, a guard taken after the read of *L* (PN5), the epoch started
  before the pin test.
- T-PN7: a page is freed while changes of earlier commits to it are only
  in the log and the buffer pool (FL6), owed by STORE-06 with STORE-04. A
  snapshot below the freeing version keeps reading those page versions,
  also after the log file that held them is deleted and under memory
  pressure. They are never written back. The pool drops them when no reader
  can ask for them any more. A crash at any point lands on a version at
  which every page in use is whole.
- T-ID1: after a crash at any point the allocator derived from the page
  table and from what replay reports is the allocator of the version
  recovery landed on: no id of a lost commit or of an open transaction is
  live, and none that is in use is free (ID4). Owed by STORE-07.
- T-CL1: a dropped subtree (CV9, CL6), owed by TREE-09 and TREE-05. From
  the drop's commit on no read, cursor or open returns a record of it or a
  directory entry of a Merk nested in it, before and after reclaiming. A
  subtree created again under the same path starts empty. A snapshot taken
  before the drop reads all of the subtree for as long as it is held.
  Reclaiming races a
  write transaction that has copied a shared page and then commits or rolls
  back: no removed cell comes back, every page is freed exactly once, and
  the counts of live values agree with the pages afterwards. The same for a
  clear.
- T-VL1: a value pointer rewrite, by cleaning and by regrouping, racing a
  write transaction that copies the same page and then commits or rolls
  back, and racing a commit outside any open transaction that supersedes the
  page after the cleaner looked at it, and a transaction that copies the
  page and gives the copy up before the step. No committed cell ever holds a
  pointer that its file's live count does not include, every value reads
  back, an abandoned move counts nothing, and the page a commit installed
  says what the page rebuilt from disk says (PV7, PV6).
- T-VL2: a value appended ahead of the page step that will point to it, by
  write-back or by the cleaner, survives the cleaning and the deletion of
  files for as long as it is held, is counted from that step, and is dead
  space when the step is refused (FL1).
- T-VL3: a crash at any point after moves emptied a value-log file never
  recovers a page that points into a deleted file, and the counts recovered
  agree with the pages (CL5, PV6).
- T-VL4: a write transaction that rewrote a node without changing its value
  still reads that value after a foreign commit overwrote it, after its
  rebase, and after the cleaner moved or deleted the old entry (WT3). The
  same while the value is pending and when it is placed meanwhile (FL7).
- T-VL5: pending values (FL7, PV7), owed by STORE-06 with TXN-05 and
  STORE-04. A commit that writes values too long for their cells appends to
  the log only: no value-log file is written or synced and no manifest is
  replaced (CM7). Every handle that sees the commit reads the value, with
  the same bytes and the same cost before and after write-back. After a
  crash before write-back, a recovery that keeps the commit (DU2) has the
  value, from the log. Write-back racing a write transaction that copies
  the page and then commits or rolls back places every value exactly once
  and counts it once. A committer under backpressure is never left waiting
  for write-back that its own copies hold back (FL5). A snapshot of a page
  version that was superseded with a pending value keeps reading it.
- T-VL6: the counts after a crash (DU5), owed by STORE-07 with STORE-03.
  Samples are taken while commits, write-back and value moves run, and a
  crash comes at any point. The counts recovered from the sample in force
  and the records after its place equal, for each file, the number of
  recovered cells that point into it. That includes a checkpoint whose
  pages already hold a change whose record is after the place, and one
  whose replay for the pages starts before it. No sample has a place
  before the sample of the checkpoint before it.

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
- T-DU4: the log goes first (FL4), owed by STORE-06 and STORE-07. With
  commits, write-back, trimming and table checkpoints running, a crash at
  any point never recovers a link born after the version it lands on, and
  never loses a page that is in use there. That includes the case
  `page-alloc.md` reproduced: a page is freed, its chain is trimmed, and a
  checkpoint is written after that. The checkpoint is not installed while
  the log is durable only to a version before the freeing commit, so a
  crash that loses that commit never finds it in force. Once it is in
  force and covers less than the freeing commit, replay drops the changes
  it has no image for and ends with that page freed or made again (FL4).
- T-CK4: a checkpoint sampled while page steps append their records (CK2),
  owed by TXN-06. Its log ends at the end of a record, it opens, its state
  is *S(vc)*, and its counts of live values agree with its pages.

**Loom models owed by VER-05**: pin against publication and reclamation
(PN4); page-version install and publication against a reader (CM3, CM4);
claims (SC3); scope merge against scope read (SC6); rebase against the
transaction's other operations (WT7); a kept location against a rebase and
against page id reuse (PN8); making a page private once (LT5); the page
step: a value pointer rewrite against making the page private and against a
commit that supersedes it (PV7); the same for the placing of a pending value
(FL7) and for the removal of a dropped cell with the freeing of its page
(CL6); the counts lock against a commit's append and install and against a
page step (LT7 level 4a); the three questions against a commit that is
installed and not yet published (PN6);
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

- `grovedb-storage` at commit `1b7294e8`, the tree this specification was
  written against: `rocksdb = "0.25.0"` (`storage/Cargo.toml`).
- `rocksdb` 0.25.0 depends on `librocksdb-sys` 0.19.0+11.8.1, which bundles
  RocksDB 11.8.1 (`include/rocksdb/version.h`).

### 16.2 What the code does

Paths under `storage/` and `grovedb/` are in this repository, and their line
numbers, here and in 14.5, are those of commit `1b7294e8`. Later tasks move
some of this code. The other paths are in the RocksDB 11.8.1 source tree.

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
| A `StorageBatch` keeps one entry per key. A put replaces it. A delete replaces only an entry of an earlier operation generation (GM0). | `storage/src/storage.rs:320-380,471-473,561-570,613-681` |
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
says otherwise. "LigninDB" is what CF2, CF2a and RD3 require.

| Case | Sequence | RocksDB | LigninDB |
| --- | --- | --- | --- |
| A1 | *T* puts *K*; *W* puts *K*; *T* commits | `Busy` | `Conflict` |
| A1b | as A1, *W* being a second transaction that commits | `Busy` | `Conflict` |
| A2 | *T* starts; *W* puts *K*; *T* puts *K*; *T* commits | success; *K* holds *T*'s value | same. For a Merk node: where *K* is the root of its Merk, *T* read it before *W*'s commit and that commit changed a node below it, the commit is taken on both engines, and the root is published whole as *T* wrote it, with the link hashes of *T*'s read. The Merk then has a link whose hash is not its child's, and the physical checker reports it. After Merk next changes the value of a node at or below that child, from a read of the Merk that no commit to it followed, the link holds its child's hash again. The links of a node that states its value are not dated (D-155 item 3). A test pins it for LigninDB (`a_stated_value_is_published_with_the_link_hashes_of_its_read`); for RocksDB it follows from this row, whose put is a whole record |
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
| A19 | *T* reads *K*; *W* puts *K*; *T* puts *K* with the value it read; *T* commits | success; *K* holds the value *T* read, and *W*'s is lost | same for a record whose value *T* states (case A2). For a Merk node that *T* writes with other hashes or links and its value kept: **`Conflict`** (CF2a) |
| A19b | as A19, *W* putting another key *K2* | success | same |
| A19c | *T* puts *K*; *W* puts *K2*; *T* puts *K* again with the value it put; *T* commits | success | same (CF2 alone: the value a later write keeps is *T*'s own) |
| A19d | as A19, *T*'s write of the node keeping its value and naming no read | not a case there: a put states its value | refused, `NoRead` |

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
  `rocksdb` 0.25.0 and `grovedb` at `1b7294e8`. It prints the observed
  result beside the result predicted from the source, for the 52 cases other
  than B8. 51 predictions matched. The one that did not was A10c: the
  prediction had overlooked that a flushed memtable stays in the history that
  the check reads. A10d was added to show the other side of that. B8 is a
  trace; its seven results are in 14.3.
- The program is not part of the repository: this task's location is this
  file. T-CF1 and T-RD2 make its cases permanent tests on both engines.
- Cases A19 to A19c came with CF2a (D-155) and were run later, on
  2026-11-20, against `rocksdb` 0.25.0 through `RocksDbStorage` of this
  repository at `5c83bdc8`: a database from `default_rocksdb_with_path`, *T*
  from `start_transaction`, *W* a direct put on the database. A scratch test
  did it, which is not part of the repository either. The three results are
  what the source predicts, and A1, run beside them, gave `Busy`. A19d is
  not a case of RocksDB, whose put states its value.
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
    each has its own copy, and the copies are reconciled at merge by a
    second application through the tree layer. TREE-01 decided against the
    other way this item used to offer, keeping the Merks of different scopes
    on separate pages (`tree-layout.md` T6).
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
    follows TREE-09 and fixes what GROVE-05 maps the flush to: the removal
    of the pending-drop records, and a drop of whatever is still entered
    under their prefixes, which after a flat drop through the engine is
    nothing.
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
23. **A background change takes no version** (PV6), a value the cleaner
    moves included. Making such a move a commit would rebase open write
    transactions and end kept locations during normal work. The owner ruled
    the same wording into `value-log-format.md` O7 and O9 with this task,
    which had spoken of "the commit that changes the cell". **A value
    pointer is rewritten only in the current page version and never under a
    private copy** (PV7): background work abandons the move and tries later.
    The other way, rebuilding the private copy, would put background work on
    the writer's path. **The write set is self-contained** (WT3), and
    **bytes appended ahead of the page step that will point to them are
    held** (FL1).

The amendments of section 18 follow owner rulings. In writing them down,
these further choices were made. They are listed for the same reason.

24. **A pending value is placed under the checks of PV7** (FL7), so not
    while a write transaction holds a private copy of the page. The ruling
    says only that write-back places new values. The other way lets the
    step through and corrects the copy when its transaction commits. It
    would spare write-back the wait for a long transaction, and would put
    the correction into step 7 of a commit. With the choice made here, **no
    committer waits for write-back that its own copies hold back** (FL5).
    This choice decides when write-back can run, which the first ruling
    did not. The owner has since ruled for it, with a measurement owed by
    STORE-06 (section 18, open point 2).
25. **A page version that is superseded with a pending value and still
    pinned is STORE-06's to keep readable**, in one of two ways (FL7).
    Nothing is counted for it either way. For a freed page FL6 has since
    settled it.
26. **What a dropped subtree leaves behind is removed in page steps, under
    the checks of PV7, and only once no pin and no running operation can
    reach it** (CL6). `page-alloc.md` A4 asks for the page step; the wait
    is what CL6 always said of pages, now said of cells.
27. **A committed read is taken at the *L* of its operation and counts as a
    read for claims** (RD10). The first version called it a read with no
    transaction.
28. **The tree layer can ask whether another open scope has claimed a
    unit** (LT4), for placement only. `tree-layout.md` T2 needs the answer
    and did not list the question.
29. **The records of background changes are in the commit log**, so CM9
    keeps failed bytes from in front of them too, and a checkpoint's log
    can end after its last group (CK2).
30. **The counts lock is level 4a** (LT7), so that level 5 keeps the number
    other documents cite.
31. **GM3 is narrowed** to a Merk that had a root when it was first
    written. The empty case is open point 1 of section 18.

## 18. Amendments

The first version of this document merged as `6ef51947`. Seven units that
merged after it, and follow-ups to two of them, found rules it lacked or
stated inexactly, and the project owner ruled on each. This section lists
what changed, by source. "New" marks a rule id that did not exist before.
No rule id was renumbered or removed. LT7's levels are now a table, with
one level added between 4 and 5.

### 18.1 From TREE-01, `tree-layout.md`

| Change | Rules |
| --- | --- |
| The directory entry of a Merk belongs to that Merk's unit. The scope that holds the Merk writes it. A directory page is shared like any other. | SC2, ST3 |
| The tree layer can ask whether a host's element is marked pending. While it is, the root is taken from the directory entry. | SC9 |
| The second application of a scope's writes goes through the tree layer, by key. The alternative of separate pages is gone. The tree layer can ask for other scopes' claims, for placement. | LT4, section 17 item 14 |
| The cells and directory entries a drop leaves behind stay until reclaimed, and from the drop's commit on no open returns them. A clear can be a drop that keeps the host. | CV9, ST3, CV3, row D3 of 14.5 |
| Reclaiming is a walk over cells, not a list of pages. | CL6, PV4, PV7, WT11 |
| A location in a private frame also ends when the cell is carried or deleted. The key found at a location proves nothing. | PN8 |
| A write can stop below the host of its Merk. A Merk that was empty when a transaction first wrote it then shares no key with a foreign commit to it, so GM3 is narrowed to a Merk that had a root. | GM3, 14.4 |

### 18.2 From SEAM-02, `node-store.md` and `storage/src/storage.rs`

| Change | Rules |
| --- | --- |
| A storage batch does not hold "the final put or delete" of a key. Inside one operation generation a put wins over a delete, whichever came first. Only an entry of an earlier generation is replaced by a delete. | GM0, the row of 16.2 |

### 18.3 From STORE-03 and STORE-10, `page-table.md` and `page-alloc.md`

| Change | Rules |
| --- | --- |
| The log goes first, enforced where a table checkpoint is installed: through what the checkpoint covers, what it reaches, the newest free link it lost to trimming, and the place of its count sample (the last two from the follow-up to STORE-03). Recovery discards nothing for being too new. Replay drops a change to a page the checkpoint has no image of. | FL4 |
| A copy that the buffer pool is bound to keep counts as a copy. Nothing older than a page's free link is written back, and no log file is held for it (from the same follow-up). | FL6, FL1, FL7, PV3, PN2, PN7 |
| A reclaimer learns of pins and guards through three questions, asked in a fixed order. TXN-02 answers them. | PN6 |
| An operation takes its guard before it reads *L*. | PN5 |
| A commit publishes new page ids and frees old ones in step 7. A transaction keeps the ids it allocated. Whoever frees vouches for the version. | CM3, ID1, ID2, ID3 |
| Nothing of the allocator is on disk. Reuse waits for nothing but pins and guards. | ID4 (new), ID5 (new) |

### 18.4 From BASE-06, `cost-model.md`

| Change | Rules |
| --- | --- |
| A write transaction can read the committed version of a record it has changed or created. | RD10 (new), GM0 |

### 18.5 From STORE-09, `lignin-store::values`

| Change | Rules |
| --- | --- |
| New values travel in the commit's log group and are put into the value log at write-back. A commit syncs no value-log file and replaces no manifest. A value that is committed and not yet written back is read from its frame. | FL7 (new), FL1, FL6, CM3, CM7, WT4, PV4 |
| The placing of a pending value is a page step. A commit adds to no count; it only releases. | PV6, PV7, FL5 |
| A value move is abandoned for a private copy only if the copy is held when the pointer is installed. | CL5 |
| The value log's counts lock has a place in the lock order, and a change point writes its log record while it holds it. | LT7 (level 4a), CM3, CM8, CL7 |
| The counts recover from a sample in the table checkpoint and the changes replayed after the sample's place. The places of successive samples never go back. | DU5 (new), FL4, FL6 |
| Records of background changes are in the log, between the groups. Recovery reads them under the same test as the groups. | CM9, CK2, DU2 |

### 18.6 From FMT-03, `page-delta.md` and `value-log-format.md`

| Change | Rules |
| --- | --- |
| A run of page deltas carries no commit version. The page table's link and the log group carry it. | PV5 |
| A pending value has a form in its cell: a value pointer that names no file. It does not stand in a page version that a segment makes durable. | FL7 |

### 18.7 Terms, tables and choices

Section 1 has four new terms: committed read, change point, page step and
pending value. Section 17 has the new items 24 to 31, and its items 14 and
23 were reworded.

The pairs of section 13 (rows 4, 9, 10, 13, 14, 16 and 17) and its table of
events, the table of tasks in 15.1, and the tests of 15.2 follow these
changes. New tests: T-RD8, T-CF6, T-SC5, T-SC6, T-PN6, T-PN7, T-ID1, T-CL1,
T-VL5, T-VL6, T-DU4 and T-CK4. Changed: T-CM1, T-VL1, T-VL2 and T-VL4.

### 18.8 Open points

Found while the amendments were written, and not settled by them. The
project owner has since ruled on each. What is left of each is owed by the
task it names:

1. **Two transactions that each create the root of the same empty Merk
   share no key in iteration two** until finalize (GM3, 14.4). Nothing is
   wrong on existing GroveVersions. GROVE-11 owes the rule for the new
   version, for example a record of the Merk's unit that every change of
   its root writes.
2. **Whether a pending value may be placed under a private copy.** It may
   not: the owner ruled for choice 24 of section 17, as PV7 has it for a
   value move. A pending value on a page that a transaction has copied then
   waits for that transaction, and its log file with it. STORE-06 owes a
   measurement on the reference workload with back-to-back blocks: how long
   a pending value waits, and how much log it holds. If the log is held
   without bound, STORE-06 takes the other way that choice 24 describes.
3. **A superseded page version with a pending value that a pin still
   needs** (choice 25): STORE-06 chooses between the two ways and reports.
   Keeping it in memory needs FL3's rule on folding read for a version that
   is kept there, and placing it for that version alone needs a hold that
   outlives the step. For a page that has been freed, FL6 settles it.
4. **What a clear that keeps the host is in the write set** (CV9): closed by
   GROVE-05. It is the per-node deletes of `Merk::clear`, whose commit
   outcomes on existing GroveVersions are RocksDB's by construction, and
   14.5 has no row for it. The engine's range delete of a Merk's records
   stays for a later GroveVersion with a cost rule of its own (GROVE-13).
