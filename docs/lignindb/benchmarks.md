# LigninDB benchmark suite

This is the documentation for task BASE-03 of the LigninDB plan
([dashpay/grovedb#1052](https://github.com/dashpay/grovedb/issues/1052)): one
harness for every row of the plan's prediction table, on either engine. The
code is `lignin-testkit::bench`, and the command is `lignin-bench`. PERF-04,
PERF-05 and PERF-09 read its reports.

## Running it

```sh
cargo build --release -p lignin-testkit --bin lignin-bench
lignin-bench run --scratch /path/to/scratch --out run.json --label <machine> [--perf]
lignin-bench reads --scratch /path/to/scratch --out reads.json   # the warm reads of a kept database
lignin-bench writes --scratch /path/to/scratch --out writes.json # the writes at size over a copy of a kept database
lignin-bench table run.json                # rows beside the plan's values
lignin-bench compare run1.json run2.json   # how far two runs are apart
lignin-bench score run.json writes.json    # the rows scored against the plan's table
```

A reference run applies `Documents::reference()` with seed 1: one million
400-byte documents over 10,000 owners, each with a reference in the
`$ownerId` index, one document and one reference per state transition, and 20
transitions per block. It then applies 20,000 more creates, 20,000 updates and
20,000 deletes. The run needs about 1.1 GB of scratch space, which it removes
at the end unless `--keep` is given. GroveDB recurses per level, so the
harness runs on a thread with a 64 MiB stack.

Times on the project's machines:

- Apple M4 Max: about six minutes when the machine is quiet.
- Linux x86_64 box (strixbox): one to two hours, because its CPU weight is
  deliberately low and its I/O is shared.

`--perf` attaches `perf stat -e instructions:u` to the measuring thread: a time
row that is measured under it carries its instructions per sample, and a row
during which `perf` failed keeps its time and has no count. Where `perf` counts
nothing for a thread when the run starts (it is not installed, or the kernel
does not allow it), a run with `--perf` is refused. A run without `--perf`
counts no instructions. A table says under its first line when no row of it has
a count: that the run counted none, or, where the report says that `perf` was
counting, that `perf` gave none. A gate that compares instructions passes the
option.

A measurement that looks for where a row's time goes can have the same `perf`
count more than the instructions. With `LIGNIN_BENCH_PERF_EVENTS` set to
events as `perf stat -e` takes them
(`cycles:u,cycles:k,cache-misses,dTLB-load-misses`), a run with `--perf` gives
them to `perf` after `instructions:u`, and what a `perf` printed is written to
standard error, after a line `# counter NAME`, where the `perf` is stopped and
its output is read; a write that fails is not tried again. A run starts one
`perf` for each part it times under `perf` (`perf-load-apply`,
`perf-load-commit`, `perf-create-apply`, `perf-warm-get` and so on; the
applies of the load are among them, whose row carries no count), and one
before those to see whether `perf` counts here (`perf-probe`). A run of two
engines writes each engine's counters in the order the engines were named.
No row and no column of the report is of those events. A row's count is read
from the line of `perf`'s output that holds the word `instructions`, so a
list with that word in it is refused at the start of the run. While the
variable names events, a row has a count only where exactly one line of what
`perf` printed holds the word and that line's event is named `instructions:u`.
It has none where `perf` prints the word in a second line, of whatever event,
and none where the only line with the word is named otherwise, as `perf`
names the event on a processor whose counting units it lists apart
(`cpu_core/instructions:u/`); a probe with no count refuses the run. `perf` prints with each count the share of the
time the event was counted, and it scales a count whose share is under 100%;
with more events than the processor has counters that can be the
instructions' count too.

`--set KEY=VALUE` changes one setting of LigninDB, or the harness's pacing
of it, for the run ("LigninDB's settings" below).

`lignin-bench reads` measures the warm rows alone (the point gets, the point
proofs and the proofs of 100 references, with the proof sizes) over the
databases that an earlier run kept. That run is given `--keep` and
`--creates 0 --updates 0 --deletes 0`, so that every loaded document is still
there, and `reads` is given the same `--scratch`, `--seed`, `--documents`,
`--owners` and engines. It opens each database as the run left it, which is
the settled state, so its rows have the phase `settled`. Its report holds
those rows only, says of its workload and method what it did (no write after
the load, no cold read, no checkpoint, no timed reopen) and has no size on
disk (both are `null`), and `table` and `compare` read it as any other. The
reads of the settled rows are a stream of their own, apart from that of the
`warm` rows: with the same seed, documents, owners, `--warmup-reads`,
`--point-reads` and `--reference-reads`, `reads` asks for the documents and
owners that the run's `settled` rows asked for, in the same order.

`lignin-bench writes` measures the writes at size alone (the phases
`create`, `update` and `delete`) over a copy of the databases that such a
run kept (`--keep` and `--creates 0 --updates 0 --deletes 0`). It is given
the same `--scratch`, `--seed`, `--documents`, `--owners` and engines, and
the writes to apply as `--creates`, `--updates` and `--deletes`. For each
engine it copies the kept database to `<engine>-writes` beside it, opens the
copy with the settings of this call (`--set`), waits for the background work
of the opened engine, makes the untimed warm-up reads, and applies the
writes as a whole run applies them after its load: the stream is that
run's, with the blocks of the set-up and of the load passed over, so with
the three numbers of writes and the churn of a whole run the root hash at
the end is that run's. The kept database is made with no churn (the
default), as below. The copy is removed at the end unless `--keep` is given, and the kept
database is never written, so one kept database is measured under any
number of settings, each from the same state, one call at a time: while a
call has the copy of an engine it holds the lock file `<engine>-writes.lock`
beside it, and a second call for that engine and scratch directory is
refused. Its report holds the rows of
the write phases only, says of its method that no read was timed and nothing
was read cold, checkpointed or reopened, and has no size after the load
or the flush (each write phase has its size at its end, below).

`--churn N` applies N creates after the load and before the measured
creates, as a write phase of its own, `churn`, with every row of a write
phase. The phases after it begin on a state that writes have already
churned, which is what a row of the steady state needs: the value cleaner
moves nothing for the first 20,000 to 40,000 transitions after a kept
state is opened (below). It is the same stream as creates that are not
split, so `writes --churn 100000 --creates 200000` ends on the grove that
`writes --creates 300000` ends on.

Every write phase says what it left on disk. `size_on_disk_at_end` is the
size of the database directory at the phase's end, once the background
work the phase set off is idle, for both engines. For LigninDB, which
counts it (`lignindb::Db::space`, the cleaner's own accounting), the
dead share of the segments and of the value log, and the bytes each
share is of, are read at the phase's start and at its end, outside every
timed region: `segment_dead_share_at_start`, `segments_size_at_start`,
`value_log_dead_share_at_start`, `value_log_size_at_start`, and the same
four `_at_end`. A phase that starts below the value cleaner's target and
ends above it holds two regimes, and these rows show it.

The cache of the engine is not warm as after a load when the writes begin:
it holds what the warm-up reads left in it, and the kernel's page cache
holds what the copying left and the memory allows. A whole run reads every
file of the database once before its writes at size; `writes` does not.

### The two states of the warm reads

The plan's recorded values and the RocksDB baseline below were taken right
after the load, and that is the phase `warm`: RocksDB then still has
compactions to make, and LigninDB reads beside what write-back has not placed
yet. A node reads a database that has settled, and an engine can read faster
or slower then, so the ratio of the two engines differs between the states. A
run measures both, and a table prints both.

Smaller runs take `--documents`, `--owners`, `--creates`, `--updates`,
`--deletes` and the sample counts (`--point-reads`, `--reference-reads`,
`--warmup-reads`, `--cold-reads`, `--checkpoints`, `--reopens`). The 100-reference
proof needs owners with at least 100 documents, so keep about 100 documents
per owner.

## What a run does

1. **`load`.** The contract set-up block is applied untimed. The million
   creates follow, through `workload::apply`, one write transaction per
   block. Bulk load, and every count and measured row averaged over the
   load.
2. **`at-size`.** The size on disk right after the load.
3. **`warm`.** Untimed warm-up reads, then the timed reads:
   - point gets of random documents;
   - point proofs (`prove_query` of one document by id);
   - proofs of 100 references to documents (the `$ownerId` index of a random
     owner with at least 100 documents, references followed).

   The first proof of each kind is verified against the root hash.
4. **Checkpoints** before a flush (`at-size`) and after it (`flushed`). The
   size on disk is taken again after a flush, once the engine's background
   work is idle (`flushed`). That is the row compared with the plan's 1.01 GB.
5. **Reopens** (`flushed`).
6. **`settled`.** The warm reads of step 3 once more, from a stream of
   documents and owners of its own, in the settled state: after the flush, after the database was
   closed and opened again, and after a wait for the background work of
   the opened engine. The rows have the ids of the warm rows and the
   phase `settled`.
7. **`cold`.** Each sample is the first read after closing the database,
   dropping its files from the page cache where the machine allows it, and
   opening it again.
8. **`create`, `update`, `delete`.** The page cache is warmed again, then
   20,000 transitions of each kind are applied, sampled per block.

Every time row is a median over its samples, and every sample is kept in the
report. Each timed region holds only the engine call. The harness's own work
stays outside it: reading the log, the counters, building the next query, the
perf switch. A write phase ends by waiting for the engine's background work to
go idle, so the flushes and compactions it set off are counted in it.

### Cold reads

There are two methods, and the report's `method.cold_method` says which one
was used:

- **`reopen-and-drop-file-cache`.** Every file of the database is dropped from
  the page cache with `posix_fadvise(POSIX_FADV_DONTNEED)` while the database is
  closed. This needs no privilege, unlike `/proc/sys/vm/drop_caches`, which
  this box does not allow. The method is used only where a 4 MiB probe file is
  read from storage again after the drop. `/proc/self/io` checks this.
- **`reopen`.** Only the engine's caches are emptied. This is used on macOS:
  dropping the page cache there needs root (`purge`).

The bytes a cold read reads from storage are recorded per sample, on Linux
only.

## Rows and where each number comes from

| Row | Unit | Source |
| --- | --- | --- |
| `logical_bytes_per_transition` | B | each key written plus its element as GroveDB serializes it |
| `node_records_per_transition` | records | engine: Merk node records written |
| `bytes_handed_per_transition` | B | engine: key and value bytes of every record written |
| `bytes_logged_per_transition` | B | engine: bytes appended to the write-ahead log |
| `physical_bytes_per_transition` | B | engine: log plus flush plus compaction (cleaner) bytes |
| `keyed_lookups_per_transition` | lookups | engine: searches by key that went on below the root of a Merk, where it counts them; else `OperationCost::seek_count`, less one per record written |
| `seeks_per_transition` | seeks | `OperationCost::seek_count` |
| `hashes_per_transition` | calls | `OperationCost::hash_node_calls` |
| `engine_hashes_per_transition` | calls | engine: hashes computed, where it counts them (LigninDB only) |
| `node_record_*_share` | fraction | the node records in the log, decoded (RocksDB only) |
| `size_on_disk` | B | file lengths under the database directory |
| `physical_bytes_to_flushed` | B | engine: log plus flush plus compaction (cleaner) bytes from the end of the load to the `flushed` size, what the background work did after the last commit of the load included; a total, not per transition. Each detail of the engine (below) has a row `<detail>_to_flushed` of the same span |
| `point_proof_bytes`, `references_proof_bytes` | B | proof lengths |
| `wall_per_transition`, `block_commit`, `bulk_load` | µs, ms, s | `Instant` around apply and commit |
| `other_threads_cpu_per_transition` | µs | the process's CPU clock less the measuring thread's, from a write phase's first block to the end of the background work it set off, for one transition: the CPU of the engine's background threads (measured). Under `--perf` it also holds the harness's threads that read each `perf`'s replies, which wait in a read between its calls. The measuring thread's own CPU is the `cpu_per_sample` of the phase's time rows, which covers its applies and commits only and is there only where the system reports a thread's time (Linux; on macOS it is empty and this row is not) |
| `point_get`, `point_query_proof`, `references_proof` | µs | `Instant` around one read |
| `<read>_pages_asked`, `<read>_pages_missed` | pages | engine: the page versions that were read from storage over the timed reads of one kind (`_pages_missed`), and those together with the asks its buffer pool answered from memory (`_pages_asked`), for one read (LigninDB only; measured, since the engine counts its background threads too; no row for a kind of which no read was timed) |
| `page_reads_per_transition`, `pages_asked_per_transition` | pages | engine: the page versions that were read from storage over a write phase, and those together with the asks its buffer pool answered from memory, for one transition. A page version that is read is a base image and the runs after it, read in full for a load; a writer's first copy of a page that the pool does not hold is one, and so is a load that found the page put there by another thread meanwhile. It is the loads that are counted and not the pool's misses (the detail `pool_misses`), which count an ask for a page that does not exist and miss the writer's copy (LigninDB only; measured, since the engine counts its background threads too) |
| `process_read_bytes_per_transition`, `process_read_calls_per_transition`, `device_reads_per_transition`, `device_read_bytes_per_transition` | B, calls, requests, B | the operating system's counts of reads over a write phase, two of the process and two of its cgroup, for one transition ("The system's counts of reads" below; Linux only, measured) |
| `<read>_process_read_bytes`, `<read>_process_read_calls`, `<read>_device_reads`, `<read>_device_read_bytes` | B, calls, requests, B | the same counts over the timed reads of one kind, for one read |
| `checkpoint`, `reopen` | ms | `Instant` around one checkpoint or open |

GroveDB's storage layer charges one seek for every record it writes. So, for
an engine that does not count its lookups (RocksDB), `keyed_lookups_per_transition`
subtracts the records the engine logged, which leaves the lookups. GroveDB's
charges are the same on every engine, so they cannot show the lookups LigninDB
saves; LigninDB counts them itself. It counts a search from the root of a Merk
that follows a link for the key. A node that is reached through a location
that was kept, and the root itself, which the subtree directory names by its
address, are no lookups.

A build of LigninDB from before that rule counted every search by key of a
Merk that has a node, the ones that its root answered among them. A report of
such a build names `searches by key` in the source of the row, where a report
of this one names `searches by key that went on below the root of a Merk`. The
older row counts every search that the newer one counts and, beyond those,
each search that the root of a Merk answered. So `lignin-bench compare` of an
older report with a newer one can show a change of the row that is one of the
count and not of the work.

### The system's counts of reads

RocksDB counts no read for the harness, and LigninDB's pool counts the pages
it loads, not the reads a load makes. So beside the engines' counts a write
phase, and the timed reads of each kind in the phases `warm` and `settled`,
carry four counts of the operating system, each the difference of two
readings (`bench::machine::StorageReads`), on Linux:

| Row | From | What it counts |
| --- | --- | --- |
| `process_read_bytes` | `/proc/self/io`, `read_bytes` | bytes the process caused to be read from storage, read-ahead included |
| `process_read_calls` | `/proc/self/io`, `syscr` | the process's read system calls, whatever they read and wherever it came from |
| `device_reads` | `io.stat` of the process's cgroup, `rios`, summed over the devices the file lists | the read I/Os the kernel counted for the cgroup |
| `device_read_bytes` | the same file, `rbytes`, summed likewise | the bytes the kernel counted as read for the cgroup |

The two counts of the cgroup are read from `io.stat` in the directory of
the cgroup the process runs in, where the cgroup v2 hierarchy is mounted
whole and once at `/sys/fs/cgroup` with nothing mounted under it, as a
machine that systemd runs has it, and the file that is opened is on that
mount. They need the `io` controller on for that cgroup, as a unit with
`IOAccounting=yes` has it. In any other layout of the mounts, or where
the file cannot be read, a run has no such row. What the four counts are
of:

- The cgroup's two are of **every process of that cgroup**, so a run that
  is to be read by them runs in a cgroup of its own. They are the
  kernel's `rios` and `rbytes`, summed over the devices the file lists,
  and no more is said of them here than that.
- The process's two are of **the whole process**: the engine's background
  threads, and the harness too. The harness reads files of `/proc`
  between the phases, and for RocksDB it reads the end of the write-ahead
  log after each commit and RocksDB's own `LOG` while it waits for
  background work, so `process_read_calls` is not the engine's reads
  alone, least of all over few reads.

So the rows are measured rows, and they compare two runs that were made
the same way.

**What they say depends on what the kernel's page cache holds.** A read
of pages that the page cache holds is answered from memory, and only
`process_read_calls` counts it. Under a memory limit on the run's cgroup
that leaves the page cache little room (`MemoryMax` of a systemd unit),
the cgroup's counts are how the storage I/O of the two engines can be
compared where the state is far larger than the memory. They are the
kernel's count of I/Os, not of the engine's reads: one I/O can answer
several of those. A database that was written before the run, as the
kept one of `reads`, may lie in the page cache whatever the run's limit
is; to have such a run read it from storage, the page cache is emptied
before the run (`echo 3 > /proc/sys/vm/drop_caches`).

### What ext4 reads into the page cache, by kind of file

The counts above do not say which files a run's reads were of.
`lignin-testkit/bench/read-split.sh` runs one `lignin-bench` call under a
memory limit and counts, in the kernel, each time ext4 is asked to fill
the page cache and the pages it is asked for (`read-split.bt`, a bpftrace
script on ext4's read-ahead and read-folio operations), by the name of the
file. `read-split.py` then gives the calls and the bytes by kind of file,
divided by a number the caller gives: for LigninDB the value log (`val`),
the segments (`seg`, page images and delta runs), the log and the table
checkpoints; for RocksDB its table files and logs.

```bash
# The creates at size over a kept reference state, pool 162 MiB, under the memory
# limit of the engine (its peak anonymous memory plus 32 MiB). The kept database is
# read once first: what of it the page cache still holds when `writes` copies it is
# copied from memory.
find /data/ref/lignindb -type f -exec cat {} + > /dev/null
sudo lignin-testkit/bench/read-split.sh 233M 50000 /tmp/split-create -- \
  "$PWD/target/release/lignin-bench" writes --engine lignindb --scratch /data/ref \
  --out /tmp/split-create.json --creates 50000 --updates 0 --deletes 0 --set cache_mb=162
# The reads over the kept state itself, which leaves the page cache first: the totals
# of the whole call (the open, the warm-up, the timed reads of each kind, their checks).
sudo DROP_PAGE_CACHE=1 lignin-testkit/bench/read-split.sh 233M 1 /tmp/split-reads -- \
  "$PWD/target/release/lignin-bench" reads --engine lignindb --scratch /data/ref \
  --out /tmp/split-reads.json --creates 0 --updates 0 --deletes 0 --set cache_mb=162
```

The counts are of the time the probes are in place, from before the call
is started until after it has ended, and of the whole machine: the call
from its open to its close, the programs that start it, and whatever
else reads files meanwhile. So it needs a machine at rest, and a divisor
that fits what the call did: of a `writes` call with one phase, whose
copy was read from memory, the phase's transitions, with the open and the
warm-up reads in the sum. What is counted is what ext4 was asked for by
those two operations, a call and the pages of its window. It is not the
kernel's count of I/Os, which the cgroup's `device_reads` of the same run
is. It needs root, Linux with systemd and cgroup v2, bpftrace and a
database on ext4. The times of a run under the probes are no result.

### RocksDB's counters

GroveDB opens RocksDB with statistics off and keeps the handle private, so the
RocksDB engine reads what RocksDB leaves in its directory:

- **The write-ahead log (`*.log`).** The harness reads it incrementally after
  every commit, in RocksDB's log format: 32 KiB blocks, fragments, and write
  batches.
  - Merk node records are the puts to the default column family. The other
    families are aux (1), roots (2) and meta (3).
  - Consecutive batches must carry consecutive sequence numbers, so a missed
    batch fails the run.
- **The info log (`LOG`).** Its `EVENT_LOG_v1` lines give:
  - `table_file_creation.file_size`: the bytes of every table file written;
  - `compaction_finished.total_output_size`: the bytes compactions wrote;
  - the flush and compaction start and finish events, which tell when
    background work is idle.

  Flush bytes are the table bytes that no compaction wrote.
- **Not available:** page reads and cache hits.

On Linux, `/proc/self/io` `write_bytes` is reported beside the physical-bytes
row as a cross-check. In the reference runs on disk it agreed with the log,
flush and compaction sum to within 1%. It counts the whole process and the
children it has waited for, so beside other work, or on a file system that
does not count writes (tmpfs), it can differ.

### The LigninDB engine of the harness

The harness drives engines through `bench::Engine`, its own small interface:

- open;
- a block's write transaction (apply through `workload::apply`, commit);
- `settle` after opens and commits;
- reads and proofs;
- root hash;
- checkpoint;
- flush;
- waiting for background work;
- counters.

`bench::LigninDbEngine` (`bench/lignindb.rs`) implements it over
`GroveDb<LigninEngine>`, opened as `Engine::open` opens it, with the default
options unless the run sets one (below). Its `Counters` come from one reading of `lignin-store::stats`
through `Counters::from(&StoreStats)`, as the table in `bench/engine.rs`
lists:

- log bytes and groups;
- value cells and hash records;
- write-back's bytes, and the cleaner's, which the harness takes from the
  cleaner's own counts in the same reading (the details below);
- page reads and cache hits;
- keyed lookups and hashes, which the tree layer counts in the store's
  `Stats`.

Its background work counts as idle once a turn of write-back that began
after the wait did has ended, no value waits for its place, and five readings
of the stats in a row, each a fifth of the quiet time after the one before,
found no turn of write-back under way and the same counts of what write-back,
the cleaner and the reclaimer have written, moved and given back. The quiet
time is a second, or three idle times of the engine's loops where a setting
makes that longer. That is a sample of those counts and no more: the one turn
the harness witnesses is write-back's, and it cannot tell whether the cleaner
or the reclaimer ran in that time, nor that nothing is left for them. The
pages that write-back leaves for a later turn wait on: they are counted in
the phase in which they are written, and what is written when the database is
closed is in no phase.

A turn of the background work that failed ends the run with an error. The
harness looks for one where it reads the engine's stats: after every commit,
at each look of its waits, and where it reads the counters of a phase. A
session counts its failed turns from 0, so a turn that failed while the
database was opened is found at the first of those readings; a failure after
the last reading of a session is not seen.

Beside the counters it reports **details**, each as a measured row
`<detail>_per_transition` of every write phase, from the same reading as the
counters. They say where the logged and the background bytes went:

| Details | What they split |
| --- | --- |
| `log_group_bytes`, `log_page_step_bytes`, `log_mark_bytes` | the bytes logged: commit groups, the records of background page steps (a value given its place, a regrouped or cleaned value given its new pointer, a reclaiming step), checkpoint marks. `log_page_steps` counts the steps. |
| `log_inserts`, `log_insert_bytes`, `log_removes`, `log_values`, `log_value_bytes` | what the commit groups carry of cells put into pages (a new node's, and a cell carried from another page, repeated whole), of cells taken out, and of out-of-line values (new ones, and those of carried cells, repeated) |
| `log_values_carried`, `log_value_bytes_carried` | of those values, the ones the groups repeat because a cell or a flat entry was carried to another page: the value log's move rule (`value-log-format.md` O5), counted by the commit that logs them |
| `segment_bytes`, `value_log_bytes`, `regroup_bytes`, `table_checkpoint_bytes` | the flush bytes: every block write-back appended to a segment (also one whose link the page table then refused, which is dead), new values in the value log, the values a regrouping appended again, table-checkpoint files |
| `cleaner_block_bytes`, `cleaner_value_bytes` | the compaction bytes, which for LigninDB are the sum of these two and not the store's own count of the cleaner's appends. The cleaner counts a block once it has pointed the page at the copy or found the link gone, and the values of a file once its pass over the file has ended. A copy that is appended and not counted so yet is in no row until it is, in however many readings |
| `base_blocks`, `base_payload_bytes`, `delta_blocks`, `delta_payload_bytes`, `consolidations`, `runs_refused` | the blocks: base images and runs of deltas, before compression; how many images were written because a chain had reached a threshold, and how many because a run was refused |
| `flat_blocks`, `flat_block_bytes` | the images among the base images that are of flat pages, and the bytes they take in their segments |
| `chains_read`, `chain_blocks_read` | the pages write-back built from the blocks of their chains: the page under a run that is about to be written, where the pool does not hold it as a version of its own (it was evicted, or the run is a page step's on a version that a segment already holds), and a page whose chain is consolidated. And the blocks it read for them, each counted when its segment gave it, those of a chain it could not read to the end among them; they are among `block_reads` |
| `versions_held_for_readers` | page versions that were kept in memory and got no block of their own: their changes go into the block of a later version of their page, or into none where the page is freed first |
| `values_appended`, `value_payload_bytes`, `values_regrouped`, `regroup_copies_left`, `regroup_left_bytes`, `values_given_up` | the values: those appended for a commit and their bytes, those a regrouping moved, the copies of a regrouping that no cell had been given when it returned (a step is refused where the page is no longer the frame whose values were copied, because a commit superseded it, a write transaction holds a copy of it or a page step replaced it, and where the cell holds that value no more; after a refusal of the first kind the remaining steps are not tried) with the bytes of their values, and those given up. The two regroup counts are of regroupings that ended without an error. The bytes of every copy that was appended, moved or left, in a regrouping that failed or not, are in `regroup_bytes` |
| `table_checkpoints`, `cleaner_checkpoints`, `log_files_dropped`, `room_waits` | table checkpoints in all and those the cleaner asked for, log files, and commits that waited for write-back |
| `cleaner_blocks_moved`, `cleaner_values_moved`, `cleaner_bytes_trimmed`, `cleaner_bytes_dropped`, `cleaner_segments_dropped`, `cleaner_value_files_dropped` | what the cleaner moved and gave back |
| `pages_freed`, `value_reads` | pages the reclaimer freed; values read from the value log |
| `block_reads`, `block_read_bytes` | the blocks read from segments by their locations, and their bytes as stored, each one read of the file layer: the base image and each delta run of a page that is loaded, what write-back reads of a page it builds (`chain_blocks_read`), each block the cleaner reads to move it, whether or not the move then stands, and what a verification reads. The blocks of the loaded pages alone are not counted apart |
| `pool_evictions`, `pool_overflows` | page versions the buffer pool took out to make room, and frames it allocated beside its fixed ones because an eviction found none of those to take (none could be evicted, or, with evictions on several threads at once, its own steps did not come to one that could) |
| `pool_releases`, `pool_loads_overtaken`, `pool_misses` | superseded page versions that a segment holds and that the pool gave up before the clock came to them, once no reader could ask for them; loads that the pool took nothing of because a commit or a page step changed that part of its table meanwhile (their blocks are read once more unless another thread's load put the page there first); and the asks the pool did not answer from memory, an ask for a page that does not exist among them |

It also reports **gauges**, read once after every commit of a write phase and
given as they are, not per transition: `log_held_mean` and `log_held_max`, the
log that no table checkpoint covers, which is about what a recovery after a
crash at that moment replays (it also reads the headers of the log's files and
what a file holds before the first group that is not covered); and
`page_versions_waiting_mean` and `page_versions_waiting_max`, the page
versions that wait to be written back as write-back tells them, each of which
holds a frame of the pool. Both are what the engine's `Watch` says at the
reading, which a pass that is under way has not brought up to date.

**What no count holds.** LigninDB writes its manifest anew whenever the files
it names or their lengths change, a footer at the end of each sealed segment
and a header at the start of each log file, segment and value-log file. None
of them is in `physical_bytes_per_transition` (a table-checkpoint file is
counted whole, its header with it). On Linux, on a file system that counts
writes, `process_write_bytes_per_transition` is another count of what the
process wrote, with them in it. It is a check beside the engine's counts, not
their sum plus those bytes: the file system counts the pages it writes, not
the appends.

### LigninDB's settings

`--set KEY=VALUE` opens LigninDB with one setting changed, in a run that
measures LigninDB: a threshold of write-back or of the cleaner, the size of
the buffer pool or of a log file, an idle time, the upkeep of the tree pages
(`upkeep=written`, where a run is to measure L1 asked of every fragment a
write holds; `tree-layout.md`, L4), or the harness's own `pace`.
`bench/lignindb.rs` lists the keys and the option each sets. A report records
the settings of its run (`method.engine_settings`), and `lignin-bench table`
prints them. A key that names no setting, a value its setting cannot take,
and a setting in a run that does not measure LigninDB are refused before
anything is measured, by the tool and by `bench::run` alike. RocksDB takes no
setting.

`pace` is the harness's own. LigninDB's write-back is woken by every commit,
and a turn of it writes the pages that are due, so in a run that applies
blocks back to back the number of commits a turn finds follows the machine.
With `--set pace=block` the harness waits after every commit, outside every
timed region, until a turn of write-back that began after the commit has
ended and no value waits for its place: write-back has a turn for every
block by itself, as where blocks come seconds apart, and writes in it the
pages it does not leave for a later turn. The cleaner and the
reclaimer are not waited for. `pace=free`, the default, applies blocks back
to back, which is how the RocksDB baseline is measured.

### The log file's length

`Options::log_file_len` is 16 MiB by default. It is the length at which
the log turns to its next file, not when the log is dropped: a file is
deleted once a table checkpoint covers all of it and no hold keeps it, and
write-back decides when a checkpoint is taken
(`FlushConfig::checkpoint_log_bytes`), so a store can hold several files.
What it costs
the commits, measured with `lignin-bench writes` over one kept reference
state, with a pool of 162 MiB and `pace=block`, 200,000 transitions (10,000
blocks) a phase, the runs at once on a DigitalOcean `so-8vcpu-64gb-intel`
droplet, one run a row. The default before, 64 MiB, ran twice under two
names, which shows the spread of one setting:

| `log_file_len` | p99 of `block_commit`: create / update / delete | maximum |
| --- | --- | --- |
| 64 MiB, first name | 7.31 / 5.15 / 3.43 ms | 16.4 / 20.1 / 14.6 ms |
| 64 MiB, second name | 7.24 / 5.17 / 3.19 ms | 14.9 / 20.2 / 24.4 ms |
| 16 MiB | 7.00 / 5.15 / 3.26 ms | 17.5 / 15.2 / 26.6 ms |

At p99, 16 MiB is at or under the slower of the two runs of 64 MiB in
every phase: 0.24 and 0.31 ms under them in create, where those two differ
by 0.07 ms. Its maxima are 1.1 ms and 2.2 ms over the higher of them in
create and delete, where those two differ by 1.5 ms and 9.8 ms, and 4.9 and
5.0 ms under them in update, where they differ by 0.1 ms. Back to back, churns over the reference state ended, after the
run's last checkpoint, with 0 to 51 MB of log with files of 64 MiB (three
runs) and with 1.5 and 2.3 MB with files of 16 MiB (two runs).

A run with `--engine rocksdb --engine lignindb` reports every row of both
engines, with each LigninDB row's ratio to RocksDB's in the same run. With no
`--engine` a run measures RocksDB alone.

### Scoring against the plan's table

`lignin-bench score REPORT...` scores the rows of each report against the
plan's prediction table (`bench::score::TARGETS`, every row of the
table). The plan's values were measured on a machine the project does not
have, so a row is scored as LigninDB's value over RocksDB's value of the
same row in the same report, against the target and the accept over the
plan's RocksDB value: for the wall time a transition, 50 / 298 = 0.168
and 80 / 298 = 0.268. The page reads a transition, of which RocksDB
counts none and the plan's value is an estimate, are scored by their
value, as the gate's rulings read that row. A row is "target met" at or
under the target, "accept met" at or under the accept, and "missed"
above it; a ratio row with no RocksDB row in the report, or one of 0, has
no score.

A row is scored only in the state the plan means, and a report of
another workload than the reference (seed 1, a million documents over
10,000 owners) has none scored:

- the bytes logged and physical, the wall time, the commit of a block and
  the page reads a transition at steady state: in a write phase at size
  (`create`, `update`, `delete`) of 100,000 transitions or more of a
  report with a churn, whose value log's dead share ends within a point
  of where it began (a phase that moves further is treated as of two
  regimes, the value cleaner behind and then catching up), not in the
  load or the churn;
- the keyed lookups, a count that no regime moves, in the load and every
  write phase at size;
- the reads warm and settled, not cold;
- the size, the checkpoint and the reopen after the flush (`flushed`).

Under the scored rows the score gives the steady state of each write
phase whose rows say it: the dead shares of the segments and of the value
log at its start and end, the values the value cleaner moved a
transition, and each engine's size at the phase's end. It then lists the
rows of the table it did not score, and for those that no report of this
harness measures what measures them: iteration two; the bulk load, which
is the bulk loader's test through the engine (a report's `bulk_load` is
the load through GroveDB's write transactions); the crash recovery; the
identity of answers. `scoring-run.md` is the scoring of the reference
workload that M6 rests on.

## The report

One JSON document per run, with schema `lignin-bench/1`, holding:

- the machine;
- the load averages before and after the run;
- the commit;
- the GroveVersion;
- the workload and its seed;
- the method (sample counts, the cold method, whether `perf` and thread CPU
  time were available).

Per engine, the report holds:

- the root hash after the load and at the end;
- the raw totals of each write phase, including the node-record anatomy;
- the disk usage;
- the rows.

A row holds:

- its id, phase, unit and kind: count (equal in every run of one commit
  with the same settings),
  measured (bytes and sizes that follow when background work ran or what
  the rest of the process wrote: bytes logged, physical, flush, compaction
  and process write bytes, size on disk), or time;
- its value, its samples and their summary (median, mean, minimum, p10, p90,
  maximum);
- instructions per sample and thread CPU time per sample, where measured;
- storage bytes read per sample, for cold reads;
- the plan's recorded value and the ratio to it;
- the ratio to RocksDB in the same run;
- its source.

`lignin-bench compare` refuses two runs of different workloads, GroveVersions,
row units, or a row that is a time row in one run and not in the other. It
reports each row's change, and for time rows the change in instructions per
sample. It counts count, measured and time rows apart; count rows are the
ones two runs of one commit with the same settings agree on exactly. A report from before the
measured kind still reads back, and against a newer report its rows are
compared as the newer one classes them; a binary from before it cannot read
a newer report.

### What moves a time row between two builds

Two things move a time row with no change of the code under test, and a
difference between two builds is held against both before it is read as a
difference of their code. Both were measured on the write rows of the
reference workload, on four DigitalOcean `c-16-intel` droplets (16 dedicated
vCPUs of a Xeon Platinum 8358, kernel 6.8.0, ext4) that ran nothing else:
every build made from nothing in an empty tree by rustc 1.99.1 and linked by
LLD 23.1.1, whole runs one after another. A percentage below is of a
row's value in one of the four write phases: the wall time a transition, or
the commit of a block.

**One file, run twice.** Seven times a binary was run twice in one sitting
(each second build of a commit from nothing that was made here is the same
file as the first, byte for byte, and counts as that file).
From the first run to the second, LigninDB's wall time a transition changed
by -1.5 to +2.4% and its commit of a block by -3.2 to +4.6%, and RocksDB's
rows, in the six of them that ran both engines, moved as far (-1.8 to +3.6%
and -3.3 to +4.7%). In the one sitting of eight runs, both files that were
run again, five and seven runs later, were slower on LigninDB the second
time in every phase (the wall time a transition by 0.1 to 2.4%).

**Where the functions of the binary stand.** The same libraries, linked into
a binary whose functions stand at other addresses, run at another speed. Two
links of `lignin-bench` that compile nothing anew, each made of both source
trees of the case below; a row is the change from the usual link of the same
tree:

| The link | LigninDB, wall time a transition | LigninDB, commit of a block | RocksDB, wall time a transition | The ratio of the two wall times |
| --- | --- | --- | --- | --- |
| The sections of the text in a random order (two orders) | +3.1 to +8.5% | -2.6 to +5.6% | +4.7 to +8.3% | -2.3 to +2.4% |
| The text in its order, with 32 bytes before its first function | +0.8 to +5.4% | +0.3 to +5.8% | -3.8 to +3.6% | -1.0 to +6.4% |

- The random order is lld's `--shuffle-sections`, given to the link of the
  binary alone:
  `cargo rustc --release -p lignin-testkit --bin lignin-bench -- -C link-arg=-Wl,--shuffle-sections=.text*=SEED`.
  With it, on LigninDB alone and with `perf` counting
  (`LIGNIN_BENCH_PERF_EVENTS`), a transition's applies ran the same
  instructions to within 1.0% (-1.0 to 0.0%) in 3.6 to 7.6% more user cycles,
  with 46 to 59% more iTLB load misses, 7 to 21% more dTLB load misses and
  2.5 to 5.2% more L1 data load misses. A random order is the far end of what a layout does: the
  usual order keeps the functions of one object file together.
- The 32 bytes are an object file of one function, `.fill 32,1,0x90` in a
  section of its own, linked in with `-C link-arg=PAD.o`, kept with
  `-C link-arg=-Wl,-u,NAME` and put first with
  `-C link-arg=-Wl,--symbol-ordering-file=FILE`. In one of the two binaries
  18,099 of the 28,569 named functions of the text then stand 32 bytes
  later, the first 17 MB of its 27, and the others where they stood
  (alignment in between takes the 32 bytes up); in the other 18,063 of
  28,533. The two builds with the 32 bytes ran after the two without, in the
  sitting whose later runs were the slower ones on LigninDB (above), so the
  sitting may account for part of this row; these runs do not say how much.

So a difference of a few percent in a time row between two builds, at equal
instructions, is no finding by itself: it is within what a second run and
another layout of the same code give. What separates it is each binary run
twice in turned order, the same pair in a second layout, and the
instructions a sample, which the random order left within 1.0%. The ratio to
RocksDB in the same run is steadier than the times where a layout moves both
engines, as the random order does, and no steadier where it does not.

The case this was measured for: Merk's handle on LigninDB as one pointer to a
block against the handle held in place (`node-store.md`, "Handles"). Two
pairs of binaries were made of the same two trees of the engine. Between the
pairs every workspace library is equal byte for byte but the harness's own:
the second pair's source has the counting of more `perf` events, the first
pair's does not, and the two builds of a pair have the same harness source.
In four pairings of two runs
each, the first pair's wall time a transition changed by -1.0 to +3.1% from
the handle in place to the block (higher in 12 of 16 rows, +0.9% in the
mean). In eleven pairings on three machines and in the layouts above (four
of the eleven with `perf` counting), the second pair's changed by -3.6 to
+0.6% (lower in 38 of 44 rows, -1.2% in the mean). The commit of a block
changed by -3.6 to +3.4% in the first pair (0.0% in the mean) and by -1.7 to
+3.9% in the second (higher in 31 of 44 rows, +1.3% in the mean).

## Large batches

The reference workload applies batches of some tens of records. `bench::batches`
times two batches of thousands of operations, on one engine after the other,
each in a write transaction of its own: the application of the batch and the
commit. The time of the application includes the making of GroveDB's operations
out of the workload's, which is the same work on both engines.

- **moves**: trees under twelve parents, each with one item, and one batch that
  moves every tree to another key of its parent;
- **mixed**: a tree of items under each parent, and one batch over them of
  which a third replaces an item, a third deletes one and the rest puts a new
  one.

Each time is of one batch, so it is a result of one run and not a median of
samples. GroveDB's charges for each batch and the root hash after it are the
same on both engines, which the small form of the test asserts on every run.
The measurement is the ignored test, at ten thousand moves and ten thousand
mixed operations, five times on each engine in turn:

```sh
cargo test --release -p lignin-testkit --test bench batches_of_ten_thousand -- --ignored --nocapture
```

It is no part of a `lignin-bench` report.

As measured on 26 November 2026, before and after a batch came to write each
leaf of the subtree directory once for the records it puts there (TXN-03;
`lignin-tree::directory`, "The records of a batch"). Both are release builds
on one host that other work shared, each from a clean export of its commit in
a target directory of its own, run in turns (after, before, before, after),
ten rounds each. A ratio is of the two engines in one round:

| Batch | LigninDB applies, before | after | Of RocksDB's time, before | after |
| --- | --- | --- | --- | --- |
| moves | 1,027 to 1,317 ms | 384 to 511 ms | 7.3 to 9.5 | 2.7 to 3.1 |
| mixed | 88 to 134 ms | 98 to 125 ms | 1.4 to 1.9 | 1.5 to 1.7 |

RocksDB applied the moves in 127 to 170 ms and the mixed batch in 57 to 81
ms. The mixed batch enters, moves and drops no subtree, so it puts few
records into the directory, and it is as it was. The commits are within
each other's spread: LigninDB commits the moves in 25 to 60 ms where it
did in 20 to 69 ms.

## RocksDB baseline and the plan's recorded values

These are the count and measured rows of the reference run, on
`feat/LigninDB` at 564bb7bf with this harness. Every run on both machines
gives the same count rows. The measured rows (bytes logged, physical bytes,
size on disk) differ between runs, bytes logged only below the precision
shown. The write rows are per state transition, averaged over the load of
one million documents; proof sizes are medians, and the size is the whole
database after a flush.

| Row | Measured | Plan | Difference |
| --- | --- | --- | --- |
| Keys and values added | 583 B | 551 B | +5.9% |
| Node records written | 47.1 | 58.8 | -19.9% |
| Bytes handed to RocksDB | 20.8 KB | 23.9 KB | -13.0% |
| Bytes logged | 21.0 KB | 23.9 KB | -12.2% |
| Physical bytes written | 48.6 to 48.9 KB | 56 KB | -13% |
| Keyed lookups | 57.0 | 70 | -18.5% |
| Hashes (`hash_node_calls`) | 159 | about 122 | +30% |
| Point proof | 2,227 B | 2,848 B | -21.8% |
| Proof of 100 references | 52,586 B | 49,835 B | +5.5% |
| Size on disk, flushed | 1.03 to 1.07 GB | 1.01 GB | +2 to +6% |

The node-record anatomy of the load splits the bytes RocksDB is handed as
follows:

- 47.9% element values;
- 24.4% keys (the record's own key and its link keys);
- 14.5% the node's own two hashes;
- 11.2% its links' child hashes;
- 2.1% other.

About 520 B of the values per transition are new. The other 45% of the
handed bytes are values rewritten unchanged. That is close to the plan's
40%.

### Why five rows miss by more than 10%

Node records, bytes handed and logged, physical bytes, keyed lookups and the
point proof all say the same thing: every path from a document to the root is
shorter here than in the grove the plan measured. Each extra level on the
path adds:

- one node record of a few hundred bytes;
- one or two keyed lookups;
- about 100 B to a point proof.

The reference workload (BASE-01) models Drive's document layout under a root
tree with one key, and a document tree with one contract. Drive's root tree
has 19 keys, and a real `DataContractDocuments` tree holds many contracts.

An experiment adds Drive's 18 other root trees, empty, to the grove before the
load:

- node records +2.0;
- lookups +3;
- point proof +206 B.

1,000 more empty contracts under `DataContractDocuments` add as much again.
The experiment was not committed, and the workload was not changed to fit.

The gain depends on where the contract sits in that tree, so the plan's
grove cannot be reconstructed from its numbers. Its gaps are also not
consistent with a single extra depth:

- the point proof suggests about 6 more levels;
- node records suggest about 12.

The hash row goes the other way: 159 measured against 122. This is a
difference of definition. `hash_node_calls` counts 64-byte Blake3 blocks of
every hash GroveDB computes:

- each node hash (two blocks);
- the value and key-value hashes of new and changed elements;
- the subtree prefix hashes of every Merk that is opened.

The plan's "about 122, 61 when finalized once per block" fits two blocks per
rewritten node record (2 × 58.8 = 117.6) plus a few. That looks like node
hashing only.

The proof of 100 references (+5.5%), the size on disk and the logical bytes
are within 10%.

### CPU

`perf record` over a 200,000-document load on the Linux box shows where the
write path's CPU goes:

| Where | Share |
| --- | --- |
| RocksDB code, on the writer thread | 39.0% |
| RocksDB's background threads | 7.6% |
| Kernel time on the writer thread, mostly page faults on RocksDB's memory-mapped files | 15.0% |
| libc and the allocator, which RocksDB and GroveDB share | 18.4% |
| GroveDB and Merk | 17.5% |
| Blake3 | 2.4% |

The plan says 61% inside RocksDB and 2.6% Blake3. RocksDB plus the kernel
time it causes comes to 61.6%.

### Time rows on the project's machines

The values below are medians, on the reference workload.

- **Apple M4 Max:** three runs at `d7a932fb`, made only while the machine's
  one-minute load was under 10.
- **Linux x86_64 box (strixbox):** two runs, at `33eb751f` and `d7a932fb`.
  These commits measure RocksDB identically on Linux. The box's CPU weight is
  deliberately low, and its storage is shared with other jobs (I/O pressure
  "full" 30 to 50%). Its wall times are therefore dominated by waiting, and
  its stable measure is the user-space instructions per sample.

| Row | M4 Max | strixbox wall | strixbox instructions | Plan (M5 Ultra) |
| --- | --- | --- | --- | --- |
| Bulk load of 1M | 236 to 240 s | 5,031 and 13,552 s | 2.83 × 10¹² | 138 to 151 s |
| Wall time per transition (create at size) | 210 to 215 µs | 266 and 510 µs | 2.59 and 2.64 M | 298 µs |
| Update per transition | 200 to 205 µs | 379 and 1,801 µs | 2.45 and 2.43 M | |
| Delete per transition | 208 to 216 µs | 3,252 and 3,244 µs | 2.72 and 2.58 M | |
| Commit of a 20-transition block | 0.78 to 0.81 ms | 1.3 and 1.1 ms | 9.6 and 9.7 M | about 1.0 ms |
| Point get, warm | 8.0 to 8.8 µs | 556 and 14.8 µs | 87 and 89 k | 8.9 µs |
| Point query with proof, warm | 59.5 to 62.8 µs | 3,102 and 84.7 µs | 814 and 800 k | 73.6 µs |
| 100 references with proof, warm | 1,192 to 1,297 µs | 24,731 and 5,815 µs | 13.8 and 13.8 M | 1,262 µs |
| Point get, cold | 53 to 68 µs | 1,767 and 1,361 µs | 116 and 118 k | |
| Point query with proof, cold | 158 to 168 µs | 3,720 and 2,875 µs | 1.06 and 1.06 M | |
| 100 references with proof, cold | 1,867 to 2,001 µs | 82,519 and 440,265 µs | 16.6 and 16.6 M | |
| Checkpoint, after a flush | 5.6 to 6.3 ms | 65.7 and 50.3 ms | 508 and 494 k | 10 ms |
| Checkpoint, right after the load | 9.7 to 11.6 ms | 66.9 and 68.2 ms | 632 and 887 k | |
| Reopen | 7.4 to 8.4 ms | 94.6 and 90.0 ms | 95.3 and 94.1 M | 31 ms |

On strixbox, `perf` was dropped from the second run's bulk-load row after
one of its 50,000 enable/disable round trips timed out under the box's load.
The row then reports no instructions rather than a partial count.

How well the runs agree, within 10%:

- **M4 Max.** The three pairs agree on 18, 17 and 15 of the 19 time rows.
  - Bulk load, every write row, every commit and every warm read agree within
    1 to 8% in every pair.
  - The rows that miss are the I/O-bound millisecond rows: the checkpoints,
    the reopen after a flush, and the cold point get. They differ by 11 to 27%.
  - One run started while the five-minute load was 16, and it is the high one
    in every pair it misses.
- **strixbox.** Wall time agrees on 5 of 19 rows. Instructions per sample
  agree on 15 of 16 rows, most within 2%. The miss is the checkpoint right
  after the load (+40%), which copies the live log, whose length at that
  moment varies.

In a run of RocksDB alone every ratio to RocksDB in the same run is 1. The
ratio is computed for every engine a run measures after RocksDB.

## Where the state is larger than the memory

LigninDB task PERF-06 ([plan](https://github.com/dashpay/grovedb/issues/1052)).
What a transition and a read make the engine read from storage when the
pool holds a part of the state, measured on a rented machine of the class
that is used for disk results (`so-8vcpu-64gb-intel`, 8 dedicated CPUs,
its own NVMe, ext4, kernel 6.8), with `lignin-bench writes` and `reads`
over one kept reference state (the load of a whole run with the default
sizes, 0.9 GB on disk). A page read is a page version that was read from
its segments for a load (`page_reads_per_transition`); "paced" is one turn
of write-back a block (`--set pace=block`), as where blocks come seconds
apart.

**How long a phase is decides what such a row holds.** For the first
20,000 to 40,000 transitions after the kept state is opened the value
cleaner moves nothing; from then on it moves some 10 to 100 values a
transition, each by a page step on the page that owns the value, which is
loaded if the pool does not hold it and is written again by write-back.
Every table below says how long its phases were.

### The pool's size

Paced, the page cache free, 100,000 transitions a phase (create, update,
delete, in that order), then the reads over the kept state with each
pool; the commit `a31331d78`:

| Pool (`Options::cache_size`) | Pages read a transition: create / update / delete | Values the value cleaner moved a transition | Physical bytes a transition | Pages read a point get | a point proof | a proof of 100 references |
| --- | --- | --- | --- | --- | --- | --- |
| 128 MiB | 3.03 / 3.38 / 3.53 | 10 / 14 / 10 | 46 / 48 / 45 KB | 0.49 | 0.64 | 51 |
| 162 MiB | 3.36 / 4.00 / 6.25 | 22 / 25 / 86 | 60 / 61 / 128 KB | 0.35 | 0.46 | 38 |
| 162 MiB, the value cleaner held off (`cleaner_value_target=100`) | 2.11 / 2.13 / 2.15 | 0 | 36 / 36 / 36 KB | | | |
| 256 MiB (the default) | 2.33 / 3.49 / 3.57 | 31 / 84 / 107 | 69 / 127 / 151 KB | 0.06 | 0.06 | 5.2 |
| 512 MiB | 0.59 / 0.67 / 0.66 | 10 / 14 / 9 | 46 / 49 / 44 KB | 0.006 | 0.004 | 1.1 |

The rows of a transition follow the value cleaner more than the pool: the
pool of 256 MiB read more pages than that of 128 MiB in two phases of
these runs, in which the cleaner moved six to ten times as many values.
The row with the cleaner held off is what the transitions themselves and
write-back read. One run of each; how much the cleaner moves differs from
run to run of one setting.

The same with phases of 20,000 transitions, in which the value cleaner
moved nothing in create and update and 10 values a transition in delete
(an earlier commit, `a630ea831`, which counted the pool's misses; they
differ from the pages read by a few in a hundred): 128 MiB 2.51 / 2.26 /
2.72; 162 MiB 2.22 / 2.08 / 2.47; 256 MiB 1.68 / 1.61 / 1.85; 512 MiB
0.58 / 0.50 / 0.54, with 30 to 46 KB of physical bytes a transition.

### The chain threshold

`FlushConfig::max_chain_deltas` at 4, 8 (the default) and 16, each on a
reference state that was loaded with it; pool 162 MiB, paced, phases of
20,000 transitions, the commit `934c766b5`. The rows "under the limit" are
of runs whose cgroup had 233 MiB (`MemoryMax`), 32 MiB more than the
process's own memory, so that the page cache held little; the reads there
are over the kept state with the page cache emptied first.

| | 4 | 8 | 16 |
| --- | --- | --- | --- |
| physical bytes a transition, create / update / delete (page cache free) | 48 / 54 / 58 KB | 32 / 43 / 41 KB | 24 / 22 / 35 KB |
| the database after the load | 869 MB | 902 MB | 978 MB |
| under the limit: wall time a transition | 1,875 / 1,827 / 1,976 µs | 1,941 / 1,832 / 1,987 µs | 1,976 / 1,915 / 2,088 µs |
| the cgroup's read I/Os a transition (`device_reads`) | 9.6 / 10.6 / 11.0 | 11.4 / 12.7 / 15.8 | 16.5 / 20.9 / 24.9 |
| the cgroup's bytes read a transition | 117 / 140 / 151 KB | 113 / 134 / 187 KB | 132 / 175 / 226 KB |
| under the limit, mean time: a point get / a point proof / a proof of 100 references | 203 µs / 330 µs / 20.0 ms | 200 µs / 328 µs / 21.6 ms | 197 µs / 307 µs / 20.9 ms |
| the cgroup's read I/Os a get / a proof of 100 references | 1.4 / 147 | 1.6 / 167 | 1.9 / 200 |

A longer chain is fewer bytes written, more blocks to read for a page
that is not in the pool, and a larger database. Since the blocks of a
chain are named to the file layer before they are read
(`VfsFile::will_read`), the reads under the limit took no longer with 16
than with 4. It is one round, with no second run of any setting, and
without the cold rows of a whole run: an operator's choice on those
numbers, and the default stays 8.
