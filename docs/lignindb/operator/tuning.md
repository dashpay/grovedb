# Tuning LigninDB

This chapter lists the settings of the LigninDB engine that a node can
change, their defaults, and when to change them. It is part of task
OPS-05 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)).
It gives no performance figures: the measurements are in
[benchmarks.md](../benchmarks.md), with the method that made them.
**The defaults are the ones to run with** unless something below says
otherwise for your node. The documentation of write-back's and the
cleaner's settings says that none of them changes what a reader is
given, and costs are charged from the cost model over logical sizes, not
from the engine's work ([cost-model.md](../cost-model.md)).

The settings are the fields of `lignindb::Options`. An application hands
them to GroveDB when it opens a database on LigninDB, as
`EngineChoice::Lignin(options)` to `EitherEngine::open_with`, or to
`LigninEngine::open_with` ([engine.md](../engine.md#the-choice-at-the-open));
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
| `Options::log_file_len` | 64 MiB | the length at which a log file is full and the log goes on in the next one | no reason known for a node |
| `Options::reserve` | 0 bytes | the room a session keeps in a file of its own, to give back when the device fills ([backup and full disk](backup-and-full-disk.md#the-reserve)) | on a node whose device can fill: the design text's first estimate is one log file and one segment, 128 MiB with the defaults |
| `Options::idle` | 100 ms | how long the thread that reclaims dropped subtrees sleeps when nobody wakes it | no reason known |
| `Options::vfs` | `StdVfs` | the file layer the database's files are read and written through. On Linux, an application built with the feature `direct_io` of `lignin-io` can give `lignin_io::linux::DirectVfs`, which reads with `O_DIRECT`; on a file system that takes no direct reads the open is then refused (`… cannot be opened for direct reads: …`, [messages](messages.md#the-files-at-an-open)) | leave it: `benchmarks.md` has no run with direct reads |
| `Options::create_if_missing` | yes | whether an open makes a database in a directory that holds none | GroveDB's `open_checkpoint` and `lignin` set it off; a node that must never start on an empty directory sets it off |

### Memory

Each open database has a buffer pool of its own: the live database, and
every checkpoint the node has open (a node can keep checkpoints open to
serve them). **Each pool is sized by the `cache_size` of its own open**,
so a process that has several databases open has as many pools: size
them together, within the memory the node may use, beside what the
application and the rest of the engine hold.

- **The live database** takes the largest share. A pool that holds the
  pages the node reads again and again answers them without a read of
  the device; how much that is worth on a given workload is in
  [benchmarks.md](../benchmarks.md).
- **A checkpoint that is opened to be read** needs less: it takes no
  commits, so no private copies and no page versions of its own commits
  fill its pool. The example in the documentation of `lignindb::Options`
  opens one with 16 MiB.
- **The pool also holds what it cannot evict**: the private copies of
  open write transactions, the page versions that wait for write-back
  (committers wait once `max_unwritten` of them wait), and at an open the
  pages that replay rebuilds (up to `max_unwritten`, beyond which replay
  writes out those it can; a page with a value that waits for its place
  stays, whatever their number). A `cache_size` too small to hold one frame
  of a page refuses the open ([messages](messages.md#in-a-recovery)).

### Durability

| `Options::sync` | A commit is durable | After a power cut, the open lands on |
| --- | --- | --- |
| never (the default) | after the next `flush()`, checkpoint, close, or table checkpoint of write-back, each of which syncs the log | the last commit before the last sync, or a later one: never on a part of a commit |
| `SyncPolicy::Interval(d)` | after the next sync: a commit syncs the log where the last sync is at least `d` ago, and so does a turn of write-back's loop between commits. That is no bound in time where no commit and no turn comes, or a sync fails | the last commit before the last sync that was made, or a later one |
| `SyncPolicy::EveryCommit` | when its commit returns `Ok`; a commit that returns `NotDurable` is published and not known to be durable | the last commit that returned `Ok`, or a later one whose bytes reached the device all the same: one whose call had not returned when the power was cut, or one that returned `NotDurable` or `Unsettled` |

In every case the open lands on one committed version, never on a part
of a commit, and on none older than the last one made durable
([transactions.md](../transactions.md#12-durability-and-recovery), DU2).
The default is the plan's decision DEC-04, and it is what GroveDB does
on RocksDB, whose commits are not synced either. An application that
applies its last commits again after a crash runs with it.
A commit writes its group to the log file before it returns, so a
process that is killed while the machine keeps running leaves its
commits in the system's cache, where the next open reads them; what a
sync adds is the power cut. `EveryCommit` makes each commit wait for its
sync, which costs what a sync costs on the device; give it where each
commit must survive a power cut on its own.

## Write-back

Write-back is the engine's background thread that writes the pages that
commits changed from the log into segments, takes table checkpoints, and
so lets the log go. Its settings are `Options::flush`, a
`lignin_store::flush::FlushConfig`; unset, it is `FlushConfig::new` of
the database's page size, and the defaults below are for pages of
32 KiB.

| Option | Default | What it is | When to change it |
| --- | --- | --- | --- |
| `FlushConfig::max_log_held` | 256 MiB | the most log that no table checkpoint covers before a committer is made to wait. It is what a recovery reads at an open, and room on the device. | lower for a quicker open after a crash and less log on the device, at the price of more table checkpoints; higher where commits wait for it in bursts |
| `FlushConfig::checkpoint_log_bytes` | 64 MiB | a table checkpoint is taken when this much was logged since the last one | with `max_log_held`; keep it below that |
| `FlushConfig::max_unwritten` | 4096 | the most page versions that wait to be written before a committer is made to wait | higher where the pool is large and commits come in bursts; it is pool memory that cannot be evicted |
| `FlushConfig::max_pending_bytes` | 32 MiB | the most bytes of values in pages that wait for write-back before a committer is made to wait | with `max_unwritten` |
| `FlushConfig::max_chain_deltas` | 8 | the most delta runs after a page's base image before the page is written whole again | lower makes reads of a page cheaper and writes larger; leave it |
| `FlushConfig::max_chain_bytes` | 16 KiB | the most bytes of delta runs after a base image (half a page) | as above |
| `FlushConfig::segment_len` | 64 MiB | the length at which a segment is sealed | leave it; the reserve's estimate depends on it |
| `FlushConfig::max_deferred` | 1024 | the most pages a turn of write-back's loop leaves for a later turn: those that changed last, flat pages before tree pages. A turn leaves no more than half of `max_unwritten` and a quarter of the pool's frames whatever this says, and none where a table checkpoint is due; a table checkpoint and the end of a session leave none. 0 writes every page it can at every turn | no reason known |
| `FlushConfig::idle` | 50 ms | how long write-back's loop sleeps when nobody wakes it | no reason known |
| `FlushConfig::max_failed_wait` | 1 s | the bound of the wait after a turn that failed, before the next turn: the wait doubles from the idle time up to it ([DF12](../disk-full-and-backup.md#52-the-order)), and is never shorter than the idle time, whatever this says | no reason known |

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
| `CleanerConfig::value_dead_percent` | 14 % | the same for the value log: above it, a pass cleans the file with the largest share of dead bytes | as above |
| `CleanerConfig::checkpoint_dead_percent` | 4 % | the most of the store that may wait for a table checkpoint to be deleted, before the cleaner asks for one | leave it |
| `CleanerConfig::seal_dead_percent` | 2 % | a file that is being filled is closed for cleaning when its dead bytes are this share of the store | leave it |
| `CleanerConfig::max_segment_percent` | 1 % | a file that is being filled is closed when it holds this share of the store | leave it |
| `CleanerConfig::min_segment_len` | 64 KiB | no file is closed for the cleaner before it holds this much | leave it |
| `CleanerConfig::max_pass_bytes` | 8 MiB | the bytes of blocks after which a pass over the segments stops copying: it looks before each block, so the last block it copies can take it past that many. A pass over the value log has its own measure, one file. With `idle` it bounds how fast the cleaner copies blocks | lower where the cleaner's writes compete with the node's; higher where dead space grows faster than the cleaner gives it back |
| `CleanerConfig::segment_len` | 64 MiB | the length of the segments the cleaner moves blocks into | leave it |
| `CleanerConfig::idle` | 100 ms | how long the cleaner sleeps between passes when nobody wakes it | with `max_pass_bytes` |

The documentation of `CleanerConfig::new` states what the defaults give:
the dead space of a store stays below 20 in a hundred under a steady
load of commits, as read after a pass of the cleaner (14 that the
cleaner leaves, and at most 4 more that wait for a table checkpoint),
for a store with no reader on an old version, no operation that runs for
long, and blocks that are large against `min_segment_len`. What a burst
of commits frees faster than the cleaner moves shows until it has caught
up. A snapshot that is kept holds what was freed after its version, and
a checkpoint that is kept holds the files it links, whatever the
cleaner's settings
([backup and full disk](backup-and-full-disk.md#space-that-does-not-come-back)).
**What to watch:** the `on disk:` line of `lignin stats`, against what
the database holds.

## The tool

`lignin` opens a database with a pool of its own, which is not the
node's, and takes:

| Option | Default | What it is |
| --- | --- | --- |
| `lignin --cache` | 64 MiB | the buffer pool of every subcommand that opens a database, and of the target of `migrate` |
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
