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
6. **`cold`.** Each sample is the first read after closing the database,
   dropping its files from the page cache where the machine allows it, and
   opening it again.
7. **`create`, `update`, `delete`.** The page cache is warmed again, then
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
older row is larger for the same searches, so `lignin-bench compare` of an
older report with a newer one shows a change of the row that is one of the
count and not of the work.

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
options. Its `Counters` come from one reading of `lignin-store::stats`
through `Counters::from(&StoreStats)`, as the table in `bench/engine.rs`
lists:

- log bytes and groups;
- value cells and hash records;
- flusher and cleaner bytes;
- page reads and cache hits;
- keyed lookups and hashes, which the tree layer counts in the store's
  `Stats`.

Its background work is idle once no page version waits to be written back
and the flusher and the cleaner have written nothing for a second.

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
