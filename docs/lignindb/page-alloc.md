# LigninDB page id allocation

This is the specification for task STORE-10 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)). It
defines:

- the **states** of a page id, and the rules on moving between them
  (section 2)
- **when the id of a freed page may name another page** (section 3)
- what the allocator **asks of the layers around it**, which do not exist
  yet (section 4)
- how the allocator of a session is **derived after a crash** (section 5)
- the **model check** that is its acceptance test (section 6)

It builds on [format.md](format.md) (FMT-01, section 3.1),
[transactions.md](transactions.md) (TXN-01: ID1 to ID3, PN1 to PN6) and
[page-table.md](page-table.md) (STORE-03), and uses their rule numbers. The
code is `lignin-store::alloc`. Section 7 gives the sizes, section 8 lists the
choices made here that the plan left open, and section 9 maps each rule to
its tests.

## 1. Scope

This document fixes which page ids are in use and when an id may be handed
out. It does not say:

- where a page is, or what a freed page leaves in the page table
  (STORE-03). The allocator does not read or change the table.
- how version pins and operation guards are kept (TXN-02). The allocator is
  told, through three questions (section 4.1).
- how a write transaction keeps the ids it holds across scopes and
  savepoints (TXN-03, TXN-04, TXN-09).
- which log record says that a page was freed (FMT-05), or how the log is
  replayed (STORE-07). Section 5 says what replay reports.

The allocator knows no keys, no trees and no page contents. It reads no
page and no file, and nothing of it is written to disk.

## 2. The states of an id

**A1. Every page id is in one of four states.**

| State | Means | Moves on by |
| --- | --- | --- |
| free | Nothing uses the id. Every id starts here. | **allocate**, to private |
| private | An open write transaction holds it. No commit has published a page under it. | **publish**, to live, or **release**, to free |
| live | It names a committed page. | **free**, to retired |
| retired | Its page was freed by a commit, and a reader may still reach that page. | **reclaim**, to free |

**A2. Allocate hands out the lowest free id** (format.md 3.1: ids are
dense). So the ids in use stay as low as the history allows, which keeps the
page table's array short, and the same calls in the same order give the same
ids. When no id is free it fails (`AllocError::Exhausted`). Ids run from 1
to 2^32 − 1.

**A3. A private id is seen by nobody but its transaction** (ID1, WT4).
*Release* makes it free at once, with no waiting: it is for the ids of a
transaction, a scope or a savepoint that is rolled back (WT9, SC6, WT8),
and for a page that a transaction created and emptied again before it
committed. *Publish* is called for each new page in the step of a commit
that installs its page versions (CM3 step 7).

**A4. Free retires the id of a committed page, with the version it was freed
at.** `free(page, w)` says: the page can be reached at versions below *w*,
and at no version from *w* on. Whoever calls it vouches for that.

- A commit that frees a page passes its own version, in step 7 of CM3,
  before it is published.
- Whoever reclaims the pages of a dropped subtree (CL6, TREE-09) passes the
  version of the drop, or any later version up to the latest published one.
  Such a free can come with an earlier version than a free before it.
- Version 0 is refused (`AllocError::FreedByNoCommit`): it is the empty
  database, which holds no page.

The caller also vouches that the id still names the page it means. The
allocator cannot tell a page from a later one under the same id, so a free
that comes after someone else freed the page, and the id was used again,
would retire the new page. A commit knows that it cannot be so once its
conflict check has passed (CM3 step 3), and the pages of a dropped subtree
are freed by its reclaimer alone.

**A5. A call that does not fit the state of its id is refused, and changes
nothing.** Releasing or publishing an id that is not private is
`AllocError::NotPrivate`, and freeing one that is not live is
`AllocError::NotLive`. So an id cannot be freed twice, given back after it
was published (which would skip section 3), or freed before it was
published.

**A6. Every operation is one step under the allocator's own lock** (LT7,
level 5). The lock is held for no file operation and no call into other
code.

## 3. When the id of a freed page is used again

A page that was freed at version *w* was reachable at the versions below
*w*. Two kinds of reader can still be there, and `transactions.md` PN6 has a
test for each.

**A7. No version pin can reach the page** (ID2): the oldest pinned version,
*H*, is *w* or later. This is the simple test of PV3, the one
`PageTable::trim` uses. A pin between two frees does not hold back the newer
one more than needed, but a free between two pins waits for the older pin.

**A8. No running operation can reach the page** (ID3). An operation of a
write transaction with an empty write set holds no version pin (PN1). It
reads the latest version once (RD4), and that can be the version just
before *w* was published. What keeps its page ids good is its operation
guard (PN5). So, once A7 holds for an id, the allocator starts a new epoch
of the clock of retirements, and the id is free when every guard of an
earlier epoch has been released.

The epoch is started after A7 was found to hold, not when the page was
freed: a commit frees its pages before it is published, and until then a new
operation can still read the version before it.

**A9. Reclaim asks three questions, in this order, and then takes one
step** (`PageAllocator::reclaim`):

1. the oldest pin, *H*
2. the next epoch, *e*, which starts the epoch
3. the oldest guard, *g*

Then, under its lock: every retired id with *w* ≤ *H* is stamped with *e*,
and every stamped id whose stamp is at most *g* becomes free. With no pin
below *w* and no guard held, one call frees the id. Otherwise the id stays
retired, and a later call frees it.

**A10. Reclaim never waits, and old answers stay true.** The oldest pin and
the oldest guard never go down (PN3), so an answer that was taken before
other threads moved on is still a lower bound when it is used. Two reclaims
may run at once. Each id is held for the guards of the epoch it was stamped
with, whichever reclaim frees it.

**A11. Nothing else holds an id back.**

- Not durability. Section 5.3 says why a crash is safe.
- Not the page table. The chain of the freed page, with its free link, may
  still be in the table when the id is handed out, and the table takes the
  base image of the new page after the free link (T4).
- Not the buffer pool. A frame of the old page may still be cached. A
  structure keyed by page id must tell the pages of one id apart by their
  versions, as it must for a snapshot (format.md 3.1).

**Why this is enough.** Let page *P* under id *p* be freed at *w*, and let
a reclaim take its answers at the moments *t1* < *t2* < *t3*, with *w* ≤ *H*.
*H* is at most the latest version, so *w* was published before *t1*.

- A reader with a version pin on *s* can reach *P* only if *s* < *w*. No
  such pin is held at *t1*, and none is taken later, because a new pin is on
  the latest version (PN3).
- An operation without a pin can reach *P* only if the version it read is
  below *w*, so it read it before *w* was published, before *t1*. It took
  its guard before that (4.2). So its guard is of an epoch before *e*, and
  *p* stays retired until it is released.
- A handle that reads at *w* or later never reaches *P* (A4).

So when *p* becomes free, nothing that is open can reach *P*, and nothing
that opens later will.

## 4. What the allocator asks of the layers around it

The buffer pool, the flusher and the transaction layer do not exist yet.
This is what the allocator needs from them.

### 4.1 From whoever keeps the pins and the guards (TXN-02)

The trait `Pins`. A value it returns must stay true, and it is called with
the allocator's lock not held.

**N1. `oldest_pin`** returns a version *H* such that no version pin that is
held now is on a version below *H*, and none that is taken later will be.
When nothing is pinned it is the latest published version (PN3). It is never
above the version of the last commit that was published (CM3 step 8): a
commit that is installed and not yet published does not count, because a
reader can still pin the version before it. The race of PN4 is on this side:
a reader that announces a pin on a version below an answer already given
must find out and pin again.

**N2. `next_epoch`** starts an epoch *e* and returns it. Every operation
guard that is held at that moment was taken in an epoch before *e*, and
every guard that is taken from then on is taken in *e* or later.

**N3. `oldest_guard`** returns an epoch *g* such that no guard that is held
now was taken in an epoch before *g*, and none that is taken later will be.
When no guard is held it is the current epoch.

### 4.2 From every operation (PN5)

**N4. An operation takes its guard before it reads the latest committed
version**, and holds the guard for as long as it uses a page id it read. An
operation through a snapshot or a cursor is covered by the version pin as
well.

### 4.3 From a write transaction (TXN-03, TXN-04, TXN-05, TXN-09)

**N5. A transaction keeps the ids it allocated**, and gives one back with
*release* only when no rollback of its own can bring the page back: when it
rolls back, when it rolls back the scope or the savepoint that allocated the
id, or at its commit, for a page it emptied again. This is what
`tree-layout.md` S4 needs: within one write a page id names one page.

**N6. A commit calls *publish* and *free* in the step that installs its page
versions** (CM3 step 7), which cannot fail. Both are refused only for an id
in the wrong state (A5), which is a defect of the caller.

**N7. Whoever frees a page vouches for its version, and that the id still
names that page** (A4), and does not use the id after the call.

### 4.4 From whoever runs the engine

**N8. Reclaim is called after a commit is published and after the oldest pin
is dropped.** It is cheap when there is nothing to do. An id that waits only
for an operation guard is freed by the next call, whenever that comes: no
call is owed when a guard is released, and the id costs nothing but its
number in the meantime. Reclaim reports the ids it freed. Their chains can
then be trimmed out of the page table (`PageTable::trim`), subject to
section 5.3.

## 5. After a crash

**R1. Nothing of the allocator is on disk.** A page-table checkpoint lists
no free ids (page-table.md, choice 14). The allocator of a session is
derived from the page table that recovery loaded and from the log groups it
replayed (`alloc::Recovery`).

### 5.1 What the table says

**R2. A page with a link is in use, unless its newest link is a free
link.** A page whose newest link is a free link of version *w* was freed at
*w*, and its id is retired with *w*.

### 5.2 What replay says

**R3. Replay reports, for each commit it takes, every page the commit wrote
and every page it freed**, with the commit's version: `Recovery::wrote` and
`Recovery::freed`. A page that a commit created or changed counts as
written. Nothing is reported of a group that recovery does not take (DU2).

**R4. For one page, the table is believed up to the version of its newest
link, and replay after it** (K3). A link of version *v* says that the chain
holds every change up to *v* (T3). So a report of a version at or below the
page's newest link is ignored, and replay does not need to filter what it
reports. Of the reports above that link the newest one counts, whatever
order they come in. A commit that changes a page and frees it has freed it.

**R5. Every other id is free**: an id that no link and no report names. That
covers the ids open transactions held (DU3), the ids of commits that the
crash lost (CV6), and ids that were never used.

**R6. No id is private after recovery, and the freed ones are retired.** No
pin and no guard survives a crash, so the first reclaim of the session frees
them.

**R7. Recovery says which version it landed on, and nothing above it is
accepted.** `Recovery::finish` is given that version, and fails if the
newest link of a page (`RecoveryError::LinkAfterLanding`) or a report
(`RecoveryError::ReportAfterLanding`) is of a later one, or a report is of
version 0 (`RecoveryError::ReportOfNoCommit`). The version numbers above the
one landed on are given to new commits (CV6). So an id that seemed freed at
one of them, by a commit the crash lost, would be handed out by a reclaim
of the new history while its page is in use. A link above the version landed
on means that K4 was broken.

### 5.3 Why reuse does not wait for the log

A commit is published before it is durable (DU1). So an id can be freed at
*w*, handed out again and committed at *x*, and then a crash can land on a
version *r* below *w*, where the id must name the old page again. The
allocator does not hold the id back for that, because the old page is kept
by other rules:

- If the checkpoint that recovery loads holds the free link of *w*, or a
  link of the new page, the log was durable through that link's version
  before the checkpoint was named (K4). Then *r* ≥ *w*.
- Otherwise the checkpoint holds the chain of the old page as it was before
  *w*, and the blocks of that chain are still in their segments (K6). Replay
  frees the page if it reaches *w*, and reports the new page if it reaches
  *x*.

One case is not covered by the page table as it stands: see choice 9 of
section 8.

## 6. The model check

The acceptance test of the task is "no id is reused while a snapshot that
can reach it is open (model-checked)". The check is
`alloc::tests::model_check`.

### 6.1 What is modelled

The allocator under test is the real one. Everything it does not own is a
model written from `transactions.md`: the commits, the write transactions,
the version pins, the operation guards and whoever reclaims a dropped
subtree. The model keeps what the allocator is never told: which page each
id named from which commit to which, and which reader can reach it.

A step is what the engine does under one lock or with one atomic write:

| Actor | Steps |
| --- | --- |
| a write transaction | allocate an id; give back the id it took last; mark a committed page to be freed, or dropped, by its commit; roll back |
| its commit | install (CM3 step 7: publish its ids, free its pages), then publish (step 8). A commit whose page another commit has freed fails with a conflict and is rolled back. Only one commit is between the two steps. |
| a snapshot | pin the latest version; drop the pin |
| an operation without a pin | take its guard; read the latest version; end |
| a reclaim | ask for the oldest pin; start the next epoch; ask for the oldest guard; take its step in the allocator |
| the reclaimer of a dropped page | free the page with the version of the drop, or with the latest version |

The answers a reclaim got are handed to the allocator when its last step
runs, and the test fails if the allocator asks for them in another order
than the model gave them in. So every interleaving of other actors between
the questions of a reclaim is visited.

### 6.2 What is checked

After every step, for every id:

- **Safety.** If the id is free, no open reader can reach a page it named:
  no pin, no operation that has read its version, not the latest version,
  and not a commit that is installed and not yet published. Allocate hands
  out only free ids, and what it hands out is checked the same way.
- **One owner.** An id is private exactly when one transaction holds it,
  live exactly when it names one committed page that was not freed, and
  otherwise retired or free. It never has two holders.
- **Order.** Allocate returns the lowest free id, and fails exactly when
  none is free.
- **Bookkeeping.** The census and the list of retired ids, with their
  versions, are what the model has.
- **Nothing is lost.** On a copy of the state, once every reader is gone
  one reclaim leaves no id retired.

The model's write transactions hold no version pin. What keeps one from
freeing a page that another commit freed first is the conflict check of its
own commit, as A4 has it.

### 6.3 How the states are visited

`explore` visits every state that can be reached within its bounds, depth
first, and stops at the first that breaks a rule, with the steps that led
there. Two states are the same when they differ only in the numbers of their
epochs, not the order of those, or in a page that no reader can reach any
more. The bounds are the number of ids, commits, transactions, ids and
marked pages for each transaction, pins, operations and reclaims.

| Run | Bounds | States | Steps taken |
| --- | --- | --- | --- |
| `cargo test`, every run | 2 ids, 3 commits, 2 transactions, 1 pin, 1 operation, 1 reclaim | 80,330 | 596,996 |
| `cargo test`, every run | the first with 1 transaction and dropped pages | 38,044 | 226,711 |
| `cargo test`, every run | the first with 2 commits, 1 transaction and 2 reclaims at once | 88,116 | 565,820 |
| `--ignored`, release build | the first with 3 ids, 4 commits and 2 pins | 6,080,809 | 50,181,388 |
| `--ignored`, release build | the first with 3 ids, 2 pins and 1 transaction that holds 2 ids and frees 2 pages | 419,914 | 2,754,134 |
| `--ignored`, release build | the first with 4 commits and 2 pins | 438,250 | 3,443,778 |
| `--ignored`, release build | the first with 2 pins and 2 operations | 1,133,172 | 10,315,430 |
| `--ignored`, release build | the first with 2 pins and dropped pages | 693,117 | 5,969,272 |
| `--ignored`, release build | the first with 1 transaction, 2 reclaims at once and dropped pages | 1,029,092 | 7,044,615 |

The runs of every `cargo test` take about 5 seconds in a debug build. The
six others take two and a half minutes in a release build and half a
gigabyte, for 9.8 million states and 79.7 million steps.

Each run must also have used an id again, used one again beside an open
reader that cannot reach the old page, left an id waiting for a pin and for
a guard, run out of ids and, where pages are dropped, freed one with a
version below the latest, or it fails.

Drawn runs take 200,000 steps on allocators that are never copied, with 6
ids, 3 transactions, 3 pins, 2 operations, 2 reclaims and no bound on the
commits, under the same checks.

### 6.4 The check finds what it is for

Each rule the argument of section 3 uses is broken in turn, and the same
exploration must then find a free id that a reader can reach:

| Broken | Rule |
| --- | --- |
| the oldest pin is reported one version too high | A7, N1 |
| operation guards are not looked at | A8, N3 |
| a reclaim starts its epoch before it asks for the oldest pin | A9 |
| a commit frees a page with the version before its own | A4 |
| an operation reads the latest version before it takes its guard | N4 |

All five are found, by the exhaustive run and by the drawn runs.

## 7. Sizes

| Quantity | Value |
| --- | --- |
| The state of an id | 2 bits |
| A chunk of states | 4096 ids, 1024 bytes, allocated when an id in it is first used |
| The states of one million ids | 245 chunks, 250,880 bytes |
| A retired id | 4 bytes, plus one map entry for each freeing version |
| Allocate | finds the lowest free id from a hint: the first chunk that can have one |
| Release, publish, free, state | one indexed read or write |
| Reclaim | in proportion to the ids it frees |

## 8. Choices made here that the plan left open

Each of these is a decision the plan did not make. They are listed so the
owner can overrule one before TXN-02, TXN-03, TXN-05, TREE-09, STORE-07,
STORE-08 and VER-07 build on it.

1. **The allocator tells a private id from a live one** (A1), so a commit
   publishes each id it installs a page under. Without it one call less is
   needed, and giving back a published id, which would hand it out under
   open readers, could not be refused. VER-07 gets the four counts and can
   compare each state with what it finds.
2. **The lowest free id is handed out** (A2), not the most recently freed.
3. **A freed id carries a version that the caller vouches for** (A4), and
   that version need not be the latest. TREE-09 frees the pages of a dropped
   subtree after the drop, with the version of the drop.
4. **The allocator is told about pins and guards through three questions**
   (4.1), and is passed their keeper at each reclaim. It holds no reference
   to it and registers nothing with it. STORE-08 needs the same two tests
   for blocks and files, and can use the same trait.
5. **The clock of retirements is started by the reclaimer** (A8, N2), after
   the pin test has passed, and guards are compared with it by epoch. The
   other way, stamping an id when it is freed, is wrong for a commit, which
   frees before it is published. TXN-02 chooses how a guard announces its
   epoch.
6. **Reclaim is called by the engine, not from inside allocate** (N8). An
   allocation that finds no free id below the high-water mark takes a new
   one, without looking at the retired ids.
7. **Reuse does not wait for the log to be durable** (A11, 5.3).
8. **Replay may report everything it takes, in any order** (R3, R4). The
   allocator applies K3 itself, against the table, so a replay that forgets
   to skip what a page already has cannot free a page that is in use.
   **Recovery passes the version it landed on, and a link or a report above
   it is an error** (R7), not something to drop silently: STORE-07 has then
   read a group it should not have, or loaded a checkpoint that K4 should
   have refused.
9. **A freed page's chain must not be trimmed away while a recovery could
   still land below the version that freed it. Nothing enforces that
   yet.** T11 removes a free link with everything older once no reader
   needs it, and the page then has no link. A checkpoint that is written
   after that has no link of the page, so its "reaches" does not include
   the freeing version, and K4 lets it be named with the log durable only
   through the version it covers. If it covers a version below the freeing
   one and the crash loses the freeing commit, recovery lands where the
   page is in use, and its chain is gone. This was reproduced on
   `lignin-store::table` as merged: base image at version 1, free link at
   2, trim at 2, checkpoint covering 1 installed with the log durable
   through 1, and the loaded table has no page at version 1. It does not
   depend on the id being used again, so the allocator cannot close it. Two
   ways to close it, both outside this task: whoever trims passes the
   smaller of the oldest pin and the version the log is durable through
   when the newest link is a free link; or the table remembers the newest
   free link it trimmed away and `TableCheckpoints::install` asks for the
   log to be durable through it. R5 relies on one of them.
10. **An id space that runs out is an error, not a wait** (A2). At 2^32 − 1
    pages the page table alone is 112 GiB (format.md 3.1).

## 9. Code and tests

Run the tests with `cargo test -p lignin-store alloc` and
`cargo test -p lignin-store --test page_alloc`. The full bounds of the model
check are
`cargo test --release -p lignin-store --lib -- --ignored --nocapture every_interleaving`.

| Rule | Code | Tests |
| --- | --- | --- |
| A1, A3, A4, A5 | `alloc::PageAllocator::{allocate, release, publish, free}`, `alloc::AllocError` | Each call in each state: what it changes, and that a refused call changes nothing. Each error, by its text. |
| A2 | `alloc::states` | The lowest free id across words and chunks, in chunks that were skipped, up to a limit and at the last id of all. 240,000 drawn steps against a map. |
| A6 | `PageAllocator` | Four writers that allocate, commit, free and roll back, four readers on snapshots and in operations, and reclaims, on threads, until 40,000 ids have come back and the readers have looked at an id 400,000 times: no reader sees an id it can reach handed out, and no id has two holders. |
| A7, A8, A9, A10 | `PageAllocator::reclaim`, `alloc::Pins` | **Acceptance test** (section 6). Directed: an id waits for the pin below its version and not for a later one; for its commit to be published; for an operation that was running and not for a later one; two reclaims that overtake each other. 240,000 drawn steps against a model of the rules. |
| N1 to N4 | `alloc::{Pins, Epoch}` | The model of section 6, and each of its rules broken (6.4). |
| R1 to R7 | `alloc::{Recovery, RecoveryError}` | What a table says; what replay adds; reports at or below a page's newest link; the order of reports; a link and a report above the version landed on, and a free that the crash lost. 300 drawn histories of 12 commits over 40 ids, with a checkpoint that covers any version, a recovery that lands on any later one, chains that reach any version in between, with and without what a trim removes: the allocator is the one of the version landed on. |
| Introspection | `PageAllocator::{state, census, retired, scan}` | Against the states of 9,000 ids. |
| Beside the table | `tests/page_alloc.rs` | A snapshot reads its page through the free of that page and other commits; the id comes back when the snapshot and an operation under it are gone, and names a new page; a session derived from a table and a log. |
