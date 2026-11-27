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
