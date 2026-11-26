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
the three numbers of writes of a whole run the root hash at the end is that
run's. The copy is removed at the end unless `--keep` is given, and the kept
database is never written, so one kept database is measured under any
number of settings, each from the same state. Its report holds the rows of
the write phases only, says of its method that no read was timed and nothing
was read cold, checkpointed or reopened, and has no size on disk.

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
| `point_proof_bytes`, `references_proof_bytes` | B | proof lengths |
| `wall_per_transition`, `block_commit`, `bulk_load` | µs, ms, s | `Instant` around apply and commit |
| `point_get`, `point_query_proof`, `references_proof` | µs | `Instant` around one read |
| `<read>_pages_asked`, `<read>_pages_missed` | pages | engine: how often its buffer pool was asked for a page over the timed reads of one kind, and how often it did not hold the page, for one read (LigninDB only; measured, since the pool counts its background threads too; no row for a kind of which no read was timed) |
| `page_reads_per_transition`, `pages_asked_per_transition` | pages | engine: the page versions its buffer pool did not hold and that were read from storage over a write phase, and the pages the pool was asked for, held or not, for one transition (LigninDB only; measured, since the pool counts its background threads too) |
| `process_read_bytes_per_transition`, `process_read_calls_per_transition`, `device_reads_per_transition`, `device_read_bytes_per_transition` | B, calls, requests, B | the operating system's counts of the process's reads over a write phase, for one transition ("The system's counts of reads" below; Linux only, measured) |
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
| `device_reads` | `io.stat` of the process's cgroup, `rios`, every device | read requests that reached a device |
| `device_read_bytes` | the same file, `rbytes` | the bytes of those requests |

The two counts of the cgroup need cgroup v2 with the `io` controller on for
the cgroup the process runs in, as a systemd unit with `IOAccounting=yes`
has; a run outside one has no such row. They are of every process of that
cgroup, and all four are of the whole process, its background threads
included, so the rows are measured rows.

**What they say depends on the memory the kernel's page cache has.** With
the page cache free, a file that was written or read before is read from
memory, and only `process_read_calls` counts the read. Under a memory
limit on the process's cgroup that leaves the page cache little room
(`MemoryMax` of a systemd unit), the counts of the device say how often a
read of either engine reached storage, which is the count the two engines
can be compared by where the state is far larger than the memory.

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
| `segment_bytes`, `value_log_bytes`, `regroup_bytes`, `table_checkpoint_bytes` | the flush bytes: every block write-back appended to a segment (also one whose link the page table then refused, which is dead), new values in the value log, the values a regrouping appended again, table-checkpoint files |
| `cleaner_block_bytes`, `cleaner_value_bytes` | the compaction bytes, which for LigninDB are the sum of these two and not the store's own count of the cleaner's appends. The cleaner counts a block once it has pointed the page at the copy or found the link gone, and the values of a file once its pass over the file has ended. A copy that is appended and not counted so yet is in no row until it is, in however many readings |
| `base_blocks`, `base_payload_bytes`, `delta_blocks`, `delta_payload_bytes`, `consolidations`, `runs_refused` | the blocks: base images and runs of deltas, before compression; how many images were written because a chain had reached a threshold, and how many because a run was refused |
| `flat_blocks`, `flat_block_bytes` | the images among the base images that are of flat pages, and the bytes they take in their segments |
| `chains_read`, `chain_blocks_read` | the pages write-back built from the blocks of their chains because the pool no longer held the version of the newest link (to apply a run to before it is written, and for a value that is given its place at a table checkpoint), and the blocks it read for them; they are among `block_reads` |
| `versions_held_for_readers` | page versions that were kept in memory and got no block of their own: their changes go into the block of a later version of their page, or into none where the page is freed first |
| `values_appended`, `value_payload_bytes`, `values_regrouped`, `regroup_copies_left`, `regroup_left_bytes`, `values_given_up` | the values: those appended for a commit and their bytes, those a regrouping moved, the copies of a regrouping that no cell had been given when it returned (a step is refused where the page is no longer the frame whose values were copied, because a commit superseded it, a write transaction holds a copy of it or a page step replaced it, and where the cell holds that value no more; after a refusal of the first kind the remaining steps are not tried) with the bytes of their values, and those given up. The two regroup counts are of regroupings that ended without an error. The bytes of every copy that was appended, moved or left, in a regrouping that failed or not, are in `regroup_bytes` |
| `table_checkpoints`, `cleaner_checkpoints`, `log_files_dropped`, `room_waits` | table checkpoints in all and those the cleaner asked for, log files, and commits that waited for write-back |
| `cleaner_blocks_moved`, `cleaner_values_moved`, `cleaner_bytes_trimmed`, `cleaner_bytes_dropped`, `cleaner_segments_dropped`, `cleaner_value_files_dropped` | what the cleaner moved and gave back |
| `pages_freed`, `value_reads` | pages the reclaimer freed; values read from the value log |
| `block_reads`, `block_read_bytes` | the blocks read from segments by their locations, and their bytes as stored: the base image and each delta run of a page that is loaded, and what write-back reads of a page it builds again. Each is one read of the file layer, so with `page_reads_per_transition` they say how many reads a page that the pool did not hold costs |
| `pool_evictions`, `pool_overflows` | page versions the buffer pool took out to make room, and frames it allocated beside its fixed ones because none of those could be evicted |
| `pool_releases`, `pool_loads_overtaken` | superseded page versions that a segment holds and that the pool gave up before the clock came to them, once no reader could ask for them; and loads whose blocks were read once more because a commit or a page step changed that part of the pool's table meanwhile |

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
the buffer pool or of a log file, an idle time, or the harness's own `pace`.
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

A run with `--engine rocksdb --engine lignindb` reports every row of both
engines, with each LigninDB row's ratio to RocksDB's in the same run. With no
`--engine` a run measures RocksDB alone.

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
