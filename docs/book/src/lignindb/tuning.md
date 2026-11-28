<!-- This chapter is docs/lignindb/operator/tuning.md, written here by lignin-tools/tests/operator_docs.rs: edit that file, then run `LIGNIN_OPERATOR_DOCS=write cargo test -p lignin-tools --test operator_docs`. -->

# Tuning LigninDB

This chapter lists the settings of the LigninDB engine that a node can
change, their defaults, and when to change them. It is part of task
OPS-05 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)).
It gives no performance figures: the measurements are in
[benchmarks.md](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/benchmarks.md), with the method that made them.
**The defaults are the ones to run with** unless something below says
otherwise for your node. The documentation of write-back's and the
cleaner's settings says that none of them changes what a reader is
given, and costs are charged from the cost model over logical sizes, not
from the engine's work ([cost-model.md](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/cost-model.md)).

The settings are the fields of `lignindb::Options`. An application hands
them to GroveDB when it opens a database on LigninDB, as
`EngineChoice::Lignin(options)` to `EitherEngine::open_with`, or to
`LigninEngine::open_with` ([engine.md](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/engine.md#the-choice-at-the-open));
`grovedb::engine::either::lignindb` is the `lignindb` crate, so an
application needs no dependency of its own. Which of them a node's
configuration file exposes, and under which names, is the application's
to say.

The defaults in the tables below are held to the code by
`lignin-tools/tests/operator_docs.rs`: a default that changes in the code
fails that test until this chapter says the new one.

## The database

| Option | Default | What it is | When to change it |
| --- | --- | --- | --- |
| `Options::cache_size` | 256 MiB | the buffer pool: the memory in which the database keeps its pages. It also holds what it cannot evict: the private copies of pages that open write transactions made, and the page versions that write-back has not written yet. | see [Memory](#memory) |
| `Options::sync` | never | when a commit makes the log durable; see [Durability](#durability) | see [Durability](#durability) |
| `Options::page_size` | unset: 32 KiB for a new database | the size of the pages of a new database. A database keeps the size it was made with for good: unset, an open takes it; set, an open of a database of another size is refused. | leave it unset: a database is opened with its own size |
| `Options::log_file_len` | 16 MiB | the length at which a log file is full: the log goes on in it until it is turned to the next one, so a file can be longer by what was committed meanwhile | no reason known for a node |
| `Options::reserve` | 0 bytes | the room a session keeps in a file of its own, to give back when the device fills ([backup and full disk](backup-and-full-disk.md#the-reserve)) | on a node whose device can fill: the design text's first estimate is one log file and one segment, 80 MiB with the defaults |
| `Options::idle` | 100 ms | how long the thread that reclaims dropped subtrees sleeps when nobody wakes it | no reason known |
| `Options::upkeep` | the fragments in which a write changed a cell | how much of a write's path the layout of the tree pages measures as the write passes; see [Upkeep](#upkeep) | no reason known for a node; see [Upkeep](#upkeep) for the two cases the other value is for |
| `Options::flush` | unset: write-back's defaults for the database's page size | write-back's settings, `FlushConfig` ([write-back](#write-back)) | leave it unset; where it is set, start from `FlushConfig::new` with the database's page size, since its sizes follow the page size |
| `Options::cleaner` | unset: the cleaner's defaults | the cleaner's settings, `CleanerConfig` ([the cleaner](#the-cleaner)) | leave it unset; where it is set, start from `CleanerConfig::new()` |
| `Options::vfs` | `StdVfs` | the file layer the database's files are read and written through. On Linux, an application built with the feature `direct_io` of `lignin-io` can give `lignin_io::linux::DirectVfs`, which reads with `O_DIRECT`; on a file system that takes no direct reads the open is then refused (`… cannot be opened for direct reads: …`, [messages](messages.md#the-files-at-an-open)) | leave it: `benchmarks.md` has no run with direct reads |
| `Options::create_if_missing` | yes | whether an open makes a database in a directory that holds none | GroveDB's `open_checkpoint` and `lignin`'s commands that read a database set it off (`lignin migrate` makes the target's database); a node that must never start on an empty directory sets it off |

### Memory

Each open database has a buffer pool of its own: the live database, and
every checkpoint the node has open (a node can keep checkpoints open to
serve them). **Each pool is sized by the `cache_size` of its own open**,
so a process that has several databases open has as many pools: size
them together, within the memory the node may use, beside what the
application and the rest of the engine hold. A pool whose every frame
is held, as by a large write transaction, takes frames beyond its
`cache_size` until they are given back, so leave room above the sum.

- **The live database** takes the largest share. A pool that holds the
  pages the node reads again and again answers them without a read of
  the device; how much that is worth on a given workload is in
  [benchmarks.md](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/benchmarks.md), whose table
  ["The pool's size"](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/benchmarks.md#the-pools-size) has the pages a
  transition and a read take from storage at four pool sizes on the
  reference state.
- **A checkpoint that is opened to be read** needs less: it takes no
  commits, so no private copies and no page versions of its own commits
  fill its pool. The example in the documentation of `lignindb::Options`
  opens one with 16 MiB.
- **The pool also holds what it cannot evict**: the private copies of
  open write transactions, the page versions that wait for write-back
  (committers wait once more than `max_unwritten` of them wait), and at an open the
  pages that replay rebuilds (up to `max_unwritten`, beyond which replay
  writes out those it can; a page with a value that waits for its place
  stays, whatever their number). A `cache_size` too small to hold one frame
  of a page refuses the open ([messages](messages.md#in-a-recovery)).

### Durability

| `Options::sync` | A commit is durable | After a power cut, the open lands on |
| --- | --- | --- |
| never (the default) | after the next `flush()`, checkpoint, close, or table checkpoint of write-back, each of which syncs the log | the last commit before the last sync, or a later one: never on a part of a commit |
| `SyncPolicy::Interval(d)` | after the next sync: a commit syncs the log where the last sync is at least `d` ago, and so does a turn of write-back's loop between commits. That is no bound in time where no commit and no turn comes, or a sync fails | the last commit before the last sync that was made, or a later one |
| `SyncPolicy::EveryCommit` | when its commit returns `Ok`; a commit that returns `NotDurable` is published and not known to be durable | the last commit that returned `Ok`, or a later one whose bytes reached the device all the same: one whose call had not returned when the power was cut, or one that returned `NotDurable`, `Unsettled` or `Halted` (its group was in the log when the session stopped) |

In every case the open lands on one committed version, never on a part
of a commit, and on none older than the last one made durable
([transactions.md](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/transactions.md#12-durability-and-recovery), DU2).
The default is the plan's decision DEC-04, and it is what GroveDB does
on RocksDB, whose commits are not synced either. An application that
applies its last commits again after a crash runs with it.
A commit writes its group to the log file before it returns, so a
process that is killed while the machine keeps running leaves its
commits in the system's cache, where the next open reads them, unless
the system's own writing of them back to the device failed meanwhile,
after which it can forget them; what a sync adds is the power cut and
that failure. `EveryCommit` makes each commit wait for its
sync, which costs what a sync costs on the device; give it where each
commit must survive a power cut on its own.

### Upkeep

`Options::upkeep` says in which parts of a write's path the layout of the
tree pages is brought back into shape as the write passes. It has two
values, `Upkeep::Changed`, the default, and `Upkeep::Written`.

**What it is about.** The database keeps the nodes of its trees in pages.
In a page, the nodes that hang together are a fragment: a node, and below
it whatever is reached without leaving the page. The layout keeps a
fragment within half a page, so that pages fill well and a fragment that
has to leave a full page finds a page that can take it. A fragment that
has grown above that has its top node moved to the page above it. To find
such a fragment, a write counts the nodes of a fragment up to that size.

**What a write touches.** A write that changes one value writes a new hash
into every node above it, up to the root. Only the bottom of that path
changes in size or shape. In the nodes above, a hash is replaced by a hash,
and their fragments are as large after the write as they were before.

- With `Upkeep::Changed`, a write measures the fragments in which it
  changed something: a node that it added, moved or made longer or shorter,
  or that now points at another node. It passes over the fragments in which
  it only replaced hashes.
- With `Upkeep::Written`, a write measures every fragment on its path.

**What does not depend on it.**

- What the database answers, its root hashes and the costs GroveDB
  reports: a node holds the same under either value.
- The format of the files. Nothing records the value. A database is opened
  with either, whichever it was written with, and a session may say another
  value than the last one did. Nothing is converted.
- The other rules of the layout. Under either value a page never holds
  more than its capacity, and a small fragment is brought back to the page
  of its parent when that has room.

**What depends on it.** How much work a write does, and where a node is
put from then on. On the reference workload the two values left the same
layout within a tenth of a percent in pages, a tenth of a point in how full
they are, and a hundredth of a page written for each transition. On no
workload that was measured did `Upkeep::Changed` write more than a
hundredth of a page more for each transition. A write executed fewer
instructions with it, most where a value is replaced by one of the same
length. The measurements are in `docs/lignindb/tree-layout.md`, section
14.6, with a workload that was written against `Upkeep::Changed`: one that
tries to leave as many fragments above the size as the rules allow.

**What `Upkeep::Changed` leaves for later.** A fragment that is above the
size is measured again by a write that changes something in it, or by one
that is planned a second time (below), and until then it stays as it is.
With `Upkeep::Written` any write that passes through it measures it. Two
kinds of fragment wait longer for that:

- The rules that move a fragment into another page run after the
  measuring, and can join two fragments into one that is above the size.
- Where the top node of a fragment has been moved up and what is left
  below it is still above the size, `Upkeep::Written` measures what is
  left in the same write, if the write passes through it.
  `Upkeep::Changed` leaves it for a write that changes something there.

Each measuring moves one node up at the most, so under either value it
can take more than one write to bring a fragment within the size. Such a
fragment is never larger than its page.

**Where a write finds no room.** A write of one kind can be refused
because a page is full of nodes it does not hold: a write that changes a
subtree and not the element that holds it (`tree-layout.md`, T3). Whether
it finds room is up to where the nodes are, so the two values can differ
in it. With `Upkeep::Changed`, a write whose first plan finds no room is
planned a second time, as `Upkeep::Written` plans it, with the pages and
everything else as they were, and the result of that second plan is the
answer. In it, a node into which the write only wrote a hash is measured
like any other. So `Upkeep::Changed` refuses a write for room only where
`Upkeep::Written` does from the same state, and it takes some writes that
`Upkeep::Written` refuses. Nothing more is promised: any other error, such
as a page that cannot be read, can be met under one value and not under
the other. And two databases that took the same writes under the two
values can have their nodes in other pages, so that a later write of that
kind finds room in one and not in the other.

**When to say `Upkeep::Written`.** Two cases are known, and neither is the
running of a node:

- to compare the two values on a workload;
- where the size that a fragment is kept within has been lowered in the
  code (it is no option): `Upkeep::Written` measures each fragment that a
  write passes through against the new size, `Upkeep::Changed` each
  fragment that a write changes.

## Write-back

Write-back is the engine's background thread that writes the pages that
commits changed from the log into segments, takes table checkpoints, and
so lets the log go. Its settings are `Options::flush`, a
`lignin_store::flush::FlushConfig`; unset, it is `FlushConfig::new` of
the database's page size, and the defaults below are for pages of
32 KiB.

| Option | Default | What it is | When to change it |
| --- | --- | --- | --- |
| `FlushConfig::max_log_held` | 256 MiB | the most log that no table checkpoint covers before a committer is made to wait: the commits a recovery replays at an open lie in it, and it takes room on the device. A recovery reads the whole items after them too | lower for a quicker open after a crash and less log on the device, at the price of more table checkpoints; higher where commits wait for it in bursts |
| `FlushConfig::checkpoint_log_bytes` | 64 MiB | a table checkpoint is taken when this much was logged since the last one | with `max_log_held`; keep it below that |
| `FlushConfig::max_unwritten` | 4096 | the most page versions that wait to be written before a committer is made to wait | higher where the pool is large and commits come in bursts; it is pool memory that cannot be evicted |
| `FlushConfig::max_pending_bytes` | 32 MiB | the most bytes of values in pages that wait for write-back before a committer is made to wait | with `max_unwritten` |
| `FlushConfig::max_chain_deltas` | 8 | the most delta runs after a page's base image before the page is written whole again | lower makes a page that is not in memory fewer blocks to read and writes larger; higher (16) writes a quarter to a half fewer bytes for more reads of the device and a larger database. ["The chain threshold"](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/benchmarks.md#the-chain-threshold) has one measurement of 4, 8 and 16; leave it unless the bytes written are what the node is short of |
| `FlushConfig::max_chain_bytes` | 16 KiB | the most bytes of delta runs after a base image (half a page) | as above |
| `FlushConfig::segment_len` | 64 MiB | the length at which a segment is sealed | leave it; the reserve's estimate depends on it |
| `FlushConfig::max_deferred` | 1024 | the most pages a turn of write-back's loop leaves for a later turn: those that changed last, flat pages before tree pages. A turn leaves no more than half of `max_unwritten` and a quarter of the pool's frames whatever this says, and none where a table checkpoint is due; a table checkpoint and the end of a session leave none. 0 writes every page it can at every turn | no reason known |
| `FlushConfig::idle` | 50 ms | how long write-back's loop sleeps when nobody wakes it | no reason known |
| `FlushConfig::max_failed_wait` | 1 s | the bound of the wait after a turn that failed, before the next turn: the wait doubles from the idle time, or 1 ms where that is shorter, up to it ([DF12](https://github.com/dashpay/grovedb/blob/develop/docs/lignindb/disk-full-and-backup.md#52-the-order)), and is never shorter than that first wait, whatever this says. It doubles after each failed turn in a row that moved nothing on; a failed turn that moved something on starts it anew. Only the end of the session and a notice that room was made end it sooner; another request for a turn does not. The cleaner's loop and the thread that reclaims dropped subtrees take the same bound for their own waits after a failed pass, from their own idle times | no reason known |

**What to watch** is `Db::stats` of the node's own handle while it runs,
which counts from the open of that session: the log held against
`max_log_held`, the table checkpoints put off and failed, the waits for
room and the segments whose sync failed. `lignin stats` prints the same
counts, as lines `log held:` and `checkpoints put off`, for the session
that it opens itself on a stopped database or a checkpoint, so its
counts begin at that open. Commits that wait for room often mean that
write-back does not keep up with the node, or that something holds the
log ([backup and full disk](backup-and-full-disk.md#the-log-grows-and-does-not-shrink)).

## The cleaner

The cleaner moves what is still live out of segments and value-log files
that are mostly dead, and gives the files back. Its settings are
`Options::cleaner`, a `lignin_store::cleaner::CleanerConfig`; unset, it
is `CleanerConfig::new`.

| Option | Default | What it is | When to change it |
| --- | --- | --- | --- |
| `CleanerConfig::target_dead_percent` | 14 % | the share of dead bytes of the segments that the cleaner works towards | lower to take less disk, at the price of more bytes written for each byte given back; higher for fewer writes and more disk |
| `CleanerConfig::value_dead_percent` | 18 % | the same for the value log: above it, a pass cleans the closed file with the largest share of dead bytes among those worth it (the next two rows), and after it those of them at least 90 % dead, within `max_pass_bytes`. A move refused because a commit changed its page first, or a write transaction held it, is tried again up to three times. A file whose cleaning moved nothing because its steps were refused at every try may not be taken again before 8 passes later while it stays cleanable, and each further such cleaning in a row doubles that delay, up to 256 passes; a pass whose files all moved nothing goes on to the next files worth cleaning while its budget lasts, as the `max_pass_bytes` row says | as above |
| `CleanerConfig::value_min_file_dead_percent` | 40 % | the floor: the share of a value-log file's own bytes that must be dead for a pass to clean it, the first file or a nearly empty one, while the value log is not over its ceiling, and one byte in a hundred at least; over the ceiling one byte in a hundred is enough, and with `value_dead_percent` at 0 any dead byte is. Each live value of a file costs a copy and a page step that has its page read and written again | lower to keep the value log smaller, at the price of copying files that are mostly live; leave it |
| `CleanerConfig::value_ceiling_dead_percent` | 20 % | over this share of dead bytes in the value log, a pass cleans the most dead file it may take once one byte in a hundred of it is dead. **It is the worst case of the value log's dead space**: once the cleaner has caught up, the share is at most this, but for the files being filled, those that wait to be deleted or that a pin or a checkpoint keeps, and those set aside after cleanings that moved nothing, as files whose pages most commits change can be while commits come back to back; a log whose deaths are spread evenly over its files, so that no file reaches the floor, can rise to it | with `value_dead_percent`; keep it at or above it |
| `CleanerConfig::checkpoint_dead_percent` | 4 % | the most bytes of segment blocks, as a share of the bytes of all segment blocks, that may wait for a table checkpoint to be deleted, before the cleaner asks for one | leave it |
| `CleanerConfig::seal_dead_percent` | 2 % | a file that is being filled is closed for cleaning when its dead bytes are this share of the bytes of its kind (the blocks of the segments, or the values of the value log), not of the database | leave it |
| `CleanerConfig::max_segment_percent` | 1 % | a file that is being filled is closed when it holds this share of the bytes of its kind (the blocks of the segments, or the values of the value log), not of the database | leave it |
| `CleanerConfig::min_segment_len` | 64 KiB | no file is closed for the cleaner before it holds this much | leave it |
| `CleanerConfig::max_pass_bytes` | 8 MiB | the bytes of blocks after which a pass over the segments stops copying: it looks before each block, so the last block it copies can take it past that many. A pass over the value log cleans one file whole, and after it nearly empty files whose live bytes stay within this; if none of those moved anything, it goes on to the next files worth cleaning while that sum is at most this, so the last can take it past. With `idle` it bounds how fast the cleaner copies blocks | lower where the cleaner's writes compete with the node's; higher where dead space grows faster than the cleaner gives it back |
| `CleanerConfig::segment_len` | 64 MiB | the length of the segments the cleaner moves blocks into | leave it |
| `CleanerConfig::idle` | 100 ms | how long the cleaner sleeps between passes when nobody wakes it | with `max_pass_bytes` |

The documentation of `CleanerConfig::new` states what the defaults give:
the dead space of a store stays below 20 in a hundred under a steady
load of commits, as read after a pass of the cleaner (14 that the
cleaner leaves, and at most 4 more that wait for a table checkpoint),
for a store with no reader on an old version, no operation that runs for
long, and blocks that are large against `min_segment_len`. The value log
is at most 20 in a hundred dead once the cleaner has caught up, but for
the files being filled, those that wait or are kept, and those set aside
after cleanings that moved nothing, and deaths spread
evenly over its files can bring it there: while it is over 18, the cleaner
copies files at least 40 in a hundred dead, and over 20 the most dead file
once one byte in a hundred of it is. It can come to rest well under 18, a cleaning giving
back whole files. The cleaner's module documentation (`lignin_store::cleaner`,
"Which files a pass cleans") has the rule whole.

Under commits back to back, as in a resync or a bulk load of large values,
the value log is not held there: a file whose last live values belong to
pages that a write transaction holds at nearly every try can stay until
the load pauses, because only the commit that holds such a page
could move its values. Under updates of the reference workload back to
back on a 16-processor machine (DigitalOcean `so-16vcpu-128gb-intel`), the
value log grew while the load lasted, to 62 to 69 in a hundred dead at
worst, and the store to 1.6 to 1.9 GB where a paced churn holds it under
1 GB; it came back when the load ended. Leave room on the device for twice
the store during a resync or a bulk load of large values. What a burst
of commits frees faster than the cleaner moves shows until it has caught
up. A snapshot that is kept holds what was freed after its version, and
a checkpoint that is kept holds the files it links, whatever the
cleaner's settings
([backup and full disk](backup-and-full-disk.md#space-that-does-not-come-back)).
**What to watch:** the `on disk:` line of `lignin stats`, against what
the database holds.

## Open files

A LigninDB database keeps its files open for as long as it is open, and
a node keeps several databases open at once: the live one and each
checkpoint it keeps open, as a node that serves its checkpoints does.
**Every one of them counts against the node process's limit of open
files.** Many Linux systems start a process with a soft limit of 1,024,
and macOS starts one with 256. Nothing in the engine bounds the files it
holds open. Set the limit before the node's first start on LigninDB
([the runbook](migration.md#10-start-the-node-on-lignindb)), and raise
it as the database grows.

### What a database holds open

What one open database holds, from the code
(`lignin_store::segment`, `lignin_store::values`, `lignin_store::wal`,
`lignin_store::flush`, `lignindb::db`) and held to it by the tests of
`lignindb/src/db/tests/open_files.rs`:

| Descriptors | For | How long |
| --- | --- | --- |
| 1 | `LOCK`, the lock of the directory | the whole session |
| 1 for each segment | each segment the manifest names: one handle to read it | until the file leaves the database |
| 1 for each value-log file | each value-log file the manifest names, the same way | until the file leaves the database |
| 1 for each log file | the log files the log still has: those no table checkpoint covers yet, those that a checkpoint being made or a backup holds, the one it writes to, and the next one it has ready. That is 2 or 3 while write-back keeps up. While it is behind, `max_log_held` over `log_file_len` gives 16 full files with the defaults, a nominal figure and no ceiling: committers go on past a turn that moved nothing, the log can turn before a file is full, and a hold keeps covered files | until a table checkpoint covers the file and nothing holds it |
| 1 more for each file being written | a segment or value-log file that the session writes is held to read and to append: write-back's segment and value-log file, the cleaner's two, and the value-log file of a load. That is at most 5 in the normal course; a file whose write failed keeps its handle until it is given up | until the file is closed |
| none at rest | `MANIFEST` and the table checkpoint are read and closed; a new manifest, a new table checkpoint and the listing or sync of a directory take 1 each for the length of the call | |

So right after the open of a database that was closed, before the session
writes, a database holds one descriptor for each file that
`lignin manifest` counts on its line `live files: …` (its log files,
segments and value-log files), plus one for `LOCK`; once it writes, and
after an open that wrote back what it replayed, add the files it is
writing. A segment or a
value-log file leaves the database when the cleaner has given it back and
no running operation can still read it, and a log file when a table
checkpoint covers it and nothing holds it: its handle is closed then, and
its name removed.

- **A checkpoint that is opened** is a database of its own, and holds
  the same of its own files: the segments and value-log files the live
  database's manifest named when the checkpoint was made, which are the
  same files under a second name, and log files of its own, which can be
  fewer or more than the live database's. Its own `lignin manifest`
  gives its count. The live database's descriptors do not change. A
  checkpoint that nothing has open holds no descriptor; it keeps the
  disk space of its files.
- **The creation of a checkpoint** opens none of the files it links. It
  holds the locks of its staging directory and of the directory it builds
  in, its manifest, and a directory it lists or syncs for the length of
  that call: 4 in all with every file linked, on Linux. On macOS, on
  APFS, the directories of its last set of syncs are held together, one
  descriptor each: the test's small database needed 10 there. Where a file cannot be linked (another file system), every file
  it copies stays open until that set of syncs: one descriptor for each
  copied file, and one more. A creation can first have write-back catch
  up, which makes files as write-back does.
- **A snapshot held for long** pins its version. The value-log files that
  hold values of that version stay in the database, and open, until it is
  let go, however much the cleaner could give back meanwhile; and an
  operation that runs for long (a read, a cursor) keeps the segments the
  cleaner emptied since it began. The count then grows with what the
  database writes for as long as they are held. The engine's soak test
  held 757 descriptors in one process during a snapshot held for 30
  minutes, where 140 to 170 were open at rest.
- **The file layer with direct reads** (`lignin_io::linux::DirectVfs`)
  keeps up to 8 io_uring instances, 1 descriptor each, and makes one
  more for each batch of reads under way beyond them.
- **`lignin`** opens the database once for `pages`, `directory`,
  `stats`, `verify`, `dump`, `load` and `compact`, which then hold what
  a node's open holds; `dump` and `load` hold their dump file as well.
  `manifest` and `segments` read one file at a time. `migrate` holds the
  database it builds, and opens the RocksDB source with RocksDB's
  `max_open_files` set to 256, which sizes RocksDB's cache of table files.

### What sets the number of files

A segment or value-log file that is being filled is closed, and a new one
begun, at the first of these
([the cleaner](#the-cleaner), [write-back](#write-back)):

- it holds 1 % of the bytes of its kind (`CleanerConfig::max_segment_percent`)
  or 2 % of them are dead in it (`CleanerConfig::seal_dead_percent`), and
  at least 64 KiB (`CleanerConfig::min_segment_len`): the cleaner looks
  at this at each of its passes;
- it reaches 64 MiB: write-back's segments (`FlushConfig::segment_len`),
  the cleaner's (`CleanerConfig::segment_len`), and value-log files,
  whose length no option changes. The length is looked at before the next
  append, and a block or a value is appended whole, so a file can end past
  it by what was appended last: 64 MiB is a target, not a bound;
- the session ends: a session never appends to a file of an earlier one,
  so each start of the node begins new files, whatever their length.

The cleaner then empties files and gives them back, and copies what is
still live out of them into files of its own, which the same rules close.
So **a file is about 1 % of its kind**, or less where it was closed by
the share of it that died or by the end of a session. On a full node of
a test network that keeps three checkpoints, a store of 1.05 GB had 72
segments (1.3 MB to 18.9 MB, median 8.2 MB) and 116 value-log files
(62 KB to 32.2 MB, median 3.7 MB): about 180 files a GB. The median
value-log file is small because write-back's file is closed near 1 % of
the value log as it was then: a file closed while the database was
smaller stays as small for as long as its values live, since the cleaner
takes a file only once a share of it is dead. Both rules keep a file open until it
holds 64 KiB, so the files below that were closed by the end of a
session. A file grows past 1 % where more is written into it between two
passes of the cleaner, as in a burst of commits.

**How a database came to be sets its count.** The same state of that
network, loaded by `lignin migrate`, was 852 MB in 50 segments and 11
value-log files (61 files, about 70 a GB), where the node's own store,
written block by block, was 999 MB in 188 files at that height. A load
writes its values into a file of its own, which it turns at 64 MiB and
the cleaner does not close; what a node writes after a migration goes
into files that the 1 % rule closes. These are two measurements of one
state, not bounds: plan with the node's. How a given node's count moves
depends on what it writes and on how long the values of its older files
live. `lignin compact` on a stopped database gives
back the files that hold dead values and writes their live values into
new files that the same rule closes, so the count falls by the files it
gave back less the files it wrote, and not at all where no file held
dead space. On a database the tests' code wrote in 3,600 commits over six
sessions (65 MB of values), it gave back 80 value-log files and the count
went from 106 to 76; the segments went from 25 to 24.

**The count grows with the database.** While 1 % of each kind is less
than 64 MiB, which is so up to about 6 GB of segments and 6 GB of
values, the files can grow with the database, and then their number
grows more slowly than its bytes; past that, plan on about one file for
each 64 MiB. How closely a database follows that depends on
how its files die, which the code does not bound. For planning:

| Database | If files stay at today's sizes | If every file grows to 1 % of its kind, up to 64 MiB (an estimate) |
| --- | --- | --- |
| 1.05 GB | 188, as measured | about 200 |
| 10.5 GB | 1,880 | 200 to 400 |
| 105 GB | 18,800 | 1,600 to 3,200 |

The estimate is for a database of the measured one's shape (by its
medians, about 0.6 GB of segments and 0.45 GB of values in 1.05 GB), with
the files that closed early or wait to be given back counted as many
again. **Plan with the column "If files stay at today's sizes"**, and
read the count of your own node as it grows.

### The limit to set

For a node that keeps `k` checkpoints open, with one more opened or being
made at times (a comparison, a backup, the next checkpoint), and a
database of `F` files, the count that `lignin manifest` prints on its
line `live files: …`:

```text
open files ≥ (k + 2) × (F + 30) + what the application holds itself
```

The 30 for each open database is an estimate for an ordinary load, not a
bound: the lock, the log files beyond those counted in `F`, the files it
is writing and what a call holds for a moment. A log that a backup or a
slow write-back holds, a snapshot held for long, and a creation that
copies its files can take more. The application's own descriptors (its
sockets, its other files, another engine beside LigninDB) come on top.

**Worked example.** On the node measured above, `F` is about 190 (188
segments and value-log files, and its log files) and `k` is 3:
(3 + 2) × (190 + 30) = 1,100. The node held 191 to 194 descriptors for
the live store, 770 to 779 with three checkpoints open, and 967 at the
moment a fourth was made. With the application's own descriptors the
process held 903 to 910 at rest and reached 1,098 while a checkpoint was
being made. Under a soft limit of 1,024, the open of the checkpoint for
a comparison failed with `the value log was not recovered: …`.

**Set the limit at 1,048,576** for soft and hard where the system allows
it, and at least twice what the formula gives for the database you plan
for. A limit costs nothing until the descriptors are used. At today's
sizes, 65,536 covers a node with three checkpoints up to about 36 GB.
How to set it:

```text
systemd service, in [Service]:        LimitNOFILE=1048576
                                      then: systemctl daemon-reload; systemctl restart SERVICE
docker run:                           --ulimit nofile=1048576:1048576   (a new container)
docker compose, under the service:    ulimits: { nofile: { soft: 1048576, hard: 1048576 } }
                                      then: docker compose up -d   (recreates the container)
a shell that starts the node:         ulimit -n 1048576   (within the hard limit, ulimit -Hn)
macOS launchd, in the job's plist:    SoftResourceLimits and HardResourceLimits, NumberOfFiles
```

On Linux the kernel's `fs.nr_open` (1,048,576 unless changed) caps a hard
limit; on macOS `kern.maxfilesperproc` does. A process keeps the limit it
was started with: restart the node after a change (on Linux, `prlimit`
can also change the limit of a running process). A container takes its
limit from the runtime, not from the host's shell: check it inside. What
a running node may hold, and what it holds, for its process id `PID`:

```text
Linux:  grep 'Max open files' /proc/PID/limits;  ls /proc/PID/fd | wc -l
macOS:  lsof -a -p PID -Ff | grep -Ec '^f[0-9]+$'     (launchctl limit maxfiles: what launchd gives its jobs)
```

### What the engine does at the limit

Each case is held by a test of `lignindb/src/db/tests/open_files.rs`, in
a child process whose limit is lowered or whose descriptors are taken,
with the session's threads stopped so that the test makes each turn of
write-back and each pass of the cleaner itself. What the threads do with
a failed turn, the count in `Db::stats` and the wait before the next
turn, is stated from their code:

- **An open** of a database or of a checkpoint fails with the error of
  the file it could not open or the directory it could not list, which
  names it and ends with the system's words, "Too many open files
  (os error 24)" ([messages](messages.md#at-the-limit-of-open-files)).
  Before its replay, the open opens the segments and value-log files its
  manifest names, which changes none of them nor the manifest
  (it can remove files the manifest does not name, which a crash left);
  the node above failed at one of those. Past them, as it replays the log
  and begins its own, an open can write a new manifest and segments of
  what it replayed, and one that fails there can have left those, and the
  session's first log file with its header alone, which the next open
  removes: a state the next open starts from, as after a crash. Raise the limit and open again. The
  database the process has open already, the live one beside a
  checkpoint, is not touched and goes on.
- **A commit** that needs no new file is published: it appends to a log
  file that is open. Where the log cannot make its next file, it goes on
  in the one that is full, past `log_file_len`. A load (`lignin migrate`)
  can need a new value-log file, and fails with the error where it cannot
  make one. A commit that finds a replacement of the manifest unfinished
  settles it first, and where that meets the limit the commit is refused
  with the error, its transaction whole, and can be tried again.
- **Write-back and the cleaner** write what they can into the files they
  have open, and report an error for the turn or the pass at the first
  new file or manifest it needs; the turn may have done some of its work
  before. Their
  threads count it in `Db::stats`, and try again after a wait
  ([write-back](#write-back), `max_failed_wait`). The session is not
  ended for it. Meanwhile the log, and the page versions that wait for
  write-back in the buffer pool, grow; a committer that finds them over
  their bounds waits for a turn, and goes on when the turn moves nothing.
  Once descriptors are free, the turns that follow write back and take
  the table checkpoint.
- **The creation of a checkpoint** fails with the error of the call
  that met the limit. Where it failed before it gave the checkpoint its
  name, nothing is at its path, and what it built can be left under
  `path.creating`, which the next creation at the path takes over
  ([what a cut-short checkpoint leaves](backup-and-full-disk.md#what-a-cut-short-copy-or-checkpoint-leaves)).
  Where it failed in the syncs after that, the whole checkpoint can be at
  its path: look at both names before you make it again
  ([messages](messages.md#at-a-checkpoint)). The live database's
  commits go on beside it.

Nothing that was committed is lost, and no session is ended, for the
limit. But a node at the limit makes no checkpoint and writes back only
into the files it has open. Raise the limit and restart the node.

## The tool

`lignin` opens a database with a pool of its own, which is not the
node's, and takes:

| Option | Default | What it is |
| --- | --- | --- |
| `--cache` | 64 MiB | the buffer pool of every `lignin` subcommand that opens a database, given after its directory (`lignin stats DIR --cache 32`), and of the target of `migrate` |
| `lignin migrate --batch` | 160 MiB | about how much one commit of a migration holds |
| `lignin migrate --progress` | 5 s | how often a migration says where its load stands |
| `lignin migrate --memory-limit` | half the machine's memory as `/proc/meminfo` states it; none where that file does not state it | the most a migration may project to hold |
| `lignin compact --timeout` | 600 s | after how long `compact` starts no further turn of the cleaner; the turn under way and the close end after it |

Where to give them, and when, is in [the runbook](migration.md#6-migrate).
To read what a database holds after a change of the settings, run
`lignin stats` on it with the node stopped:

```sh
: "${DB:?set DB to the directory of the database}"
lignin stats "$DB"
```
