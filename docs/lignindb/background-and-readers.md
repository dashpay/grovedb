# Background work and readers beside the writer

LigninDB task PERF-07 ([plan](https://github.com/dashpay/grovedb/issues/1052)).

This document says which parts of a commit's time the engine counts apart, what keeps write-back's placing of values off the thread of a commit, how commits beside background work and readers beside a writer are measured, and what the measurements gave. The rules it names are those of [transactions.md](transactions.md); the code is `lignindb::commit`, `lignin_store::flush` and `lignin_store::cache`.

## 1. The thread of a commit

An open database has three threads of its own (`lignindb::db`, "Background work"): write-back, the reclaiming and the cleaner. A commit is made on the thread of its caller (`lignindb::commit`, "The steps of a commit").

**The parts of a commit's time that are counted apart.** `CommitStats` gives five of them as totals of a session, and `FlushStats::room_waited` a sixth:

| Count | What is timed | Among those who take what it waits for there |
| --- | --- | --- |
| `lock_waited` | taking the commit lock | another commit; the sample of a checkpoint of the database; a reclaiming that loads a directory handle again |
| `counts_waited` | taking the lock on the counts of live values, for the place of the commit's group | a page step of the background work or of another committer's placing; the mark of a table checkpoint; a turn of the log |
| `appending` | `LogStore::append_group`: the check of the group, the log's own lock, the append to the log's file | whoever appends to the log |
| `reclaiming` | `Versions::reclaim` after the publication: page ids, page versions kept for readers and directory handles that no reader can reach | the reclaimers' own locks |
| `placing` | `WriteBack::place_ready` after the commit: the page steps of the values that a private copy held back | below |
| `room_waited` | `WriteBack::wait_for_room`, while write-back is over its bounds (FL5) | a turn of write-back |

The measurement of section 2 gives each of these for every commit, and the rest of the commit's time beside them.

**What the thread of a commit does to files.** A commit that is not empty appends its group to the log (`appending`), and each page step that `WriteBack::place_ready` makes after it appends one record to the log (`placing`). Where a commit's thread does more is said where it is done, and these are such places: the commit of a load has the load's values covered where no manifest covers them yet, which syncs a value-log file and replaces the manifest (`lignindb::commit`, "The commit of a load"); a commit that finds a failed append of the log uncut, or the manifest in doubt, has the cut made or the manifest settled first (`lignindb::commit`, `lignindb::db`); a page step whose record the log took back has the log settled (`WriteBack::place_ready`); and a committer that finds write-back over a bound while no loop of write-back is running makes a turn of write-back itself (`WriteBack::wait_for_room`). The loop says that it runs on its own thread, at its first instruction, and `Db` gives its handle out only then (`lignindb/src/db/tests/started.rs`), so a commit through a handle whose background work was started finds the loop for as long as the loop runs.

The tests of `lignindb/src/db/tests/held.rs` hold each kind of sync that the background work makes (of a value-log file, of a segment, of the log, of a table checkpoint, of a manifest, of the names of a directory) with its thread inside it, and make six commits and six reads meanwhile, within write-back's bounds: each commit is published and read, and the committing thread makes no sync.

**`WriteBack::place_ready`, which a committer calls after its commit, tries page steps of values that a pass found held back by a private copy, and of no others** (`lignin_store::flush`, "Pending values"). A value that is too long for its cell is appended to the value log by a pass of write-back, and a page step then gives its cell the pointer. No step is made on a page of which a write transaction holds a private copy (PV7). A pass that finds such a copy cannot place the value, and where blocks are applied back to back a pass can find one every time it looks. So a pass lists such a value for the committers, and the thread of a commit goes through the listed values with that call once its commit is published and its copies are gone. The call takes the lock of that list and never the lock of the list that a pass keeps for itself.

**Backpressure is a wait of the committer by design** (FL5): a session that commits faster than write-back writes waits, between two commits, for a turn of it. The bounds are `FlushConfig`'s. No run of section 3 met one.

## 2. How it is measured

`lignin-testkit/tests/background.rs` holds two measurements that a normal test run leaves out. Both drive `GroveDb` on the LigninDB engine with the reference workload's shape (`Documents::reference`: documents of 400 bytes, one document and one reference a state transition, 20 transitions a block, one write transaction a block) on a store that holds about 100 documents for each owner.

- `writer_latency_beside_background_work` times the commit of each block, reads the thread's own processor clock around it, and reads the engine's counts before and after it. The background is set by `BG_BACKGROUND`: `quiet` is a cleaner that moves nothing and a table checkpoint that is due to write-back's loop only when 64 GiB of log are held, which no run here came near (the close takes one, after the measured blocks); `default` is the engine's; `heavy` is segments and log files of 4 MiB, a table checkpoint due at each megabyte of log, and a cleaner that works towards 5% of dead space. `BG_SYNC_MS` makes every sync take that much longer, as on a device whose flush takes milliseconds: the file layer of the harness sleeps before it has the system make the sync.
- `reader_throughput_beside_one_writer` reads on N threads for a fixed time, beside a writer that applies blocks back to back and with none: a `get` of one document, a proof of one document, and a control row in which each thread hashes memory of its own. It gives the reads in a second of the clock and in a second of the readers' own processor time, and the percentiles of one read.

`lignindb/src/snapshot/tests/scaling.rs` measures the layer below, pages read through a snapshot. `KINDS="many pages"` has each reader go round pages of its own, `SPREAD` of them and four at least, four at a read, and `POOL` is the size of the pool, as the bytes of that many pages: with a pool smaller than the readers' pages, a page that a read asks for can have left the pool, and is then loaded into a frame that is found for it (3.4 gives the counts of the runs). With more `THREADS` than the pool has lanes, readers share lanes.

The processors of the machines below are hyperthreads: two of them can be on one core. The control row is what such a machine gives threads that share nothing.

Neither measurement asserts anything of a time.

## 3. What the measurements gave

Every time below is from a rented machine with dedicated processors that ran nothing else: a `c-16-intel` droplet (16 processors, 32 GB, ext4 with `data=ordered` on a local disk, Linux 6.8.0, rustc 1.99.1, release build; the median sync of a 4 KiB append is 0.25 to 0.30 ms), and for the reader tables a `c-48-intel` (48 processors of a Xeon Gold 6548N; 0.9 ms). A percentile is given as the upper edge of a step of the harness's histogram, or as the longest time of the run where that is less; a step is 66 µs wide from 2.1 to 4.2 ms and 33 µs from 1.05 to 2.1 ms.

### 3.1 The commit of a block beside background work

2,000 blocks back to back on a store of 100,000 documents, five runs of each setting in turn; the median of the five runs' values, and their range. A turn of write-back's loop looks at every page that is owed.

| Background | Syncs | Median, µs | 99%, µs | 99.9%, µs |
| --- | --- | --- | --- | --- |
| quiet | as the disk makes them | 1,770 (1,671 to 1,770) | 2,228 (2,097 to 2,228) | 2,490 (2,294 to 2,556) |
| default | as the disk makes them | 1,737 (1,704 to 1,802) | 2,228 (2,228 to 2,359) | 2,621 (2,490 to 2,621) |
| heavy | as the disk makes them | 1,770 (1,704 to 1,835) | 2,359 (2,163 to 2,425) | 2,621 (2,359 to 2,884) |
| quiet | 5 ms longer | 1,704 (1,606 to 1,737) | 2,228 (2,163 to 2,294) | 2,556 (2,556 to 2,818) |
| default | 5 ms longer | 1,671 (1,638 to 1,704) | 2,228 (2,228 to 2,294) | 2,687 (2,425 to 2,818) |
| heavy | 5 ms longer | 1,638 (1,540 to 1,671) | 2,163 (2,064 to 2,228) | 2,425 (2,359 to 2,490) |

**With syncs 5 ms longer, the median of the five runs' 99th percentiles is 2,228 µs under a quiet and under a default background and 2,163 µs under a heavy one.** With fast syncs it is 2,228 µs under a quiet and under a default background and 2,359 µs under a heavy one, which is above the highest of the quiet runs: four of the five heavy runs have a higher 99th percentile, and a higher median, than the quiet run of their turn.

Three more runs of each setting gave, beside the time of a commit, the time its thread ran on a processor during it and the rest:

| Background | Syncs | The commit, 99%, µs | On a processor, 99%, µs | Off the processor, 99%, µs | Off the processor, 99.9%, µs |
| --- | --- | --- | --- | --- | --- |
| quiet | as the disk makes them | 2,163 (2,097 to 2,228) | 2,163 (2,097 to 2,228) | 14 (3 to 22) | 26 (24 to 58) |
| default | as the disk makes them | 2,228 (2,097 to 2,294) | 2,228 (2,097 to 2,294) | 104 (98 to 111) | 238 (139 to 270) |
| heavy | as the disk makes them | 2,228 (2,163 to 2,294) | 2,228 (2,163 to 2,294) | 96 (84 to 100) | 164 (156 to 225) |
| quiet | 5 ms longer | 2,097 (2,064 to 2,228) | 2,064 (2,032 to 2,228) | 135 (106 to 152) | 295 (234 to 360) |
| default | 5 ms longer | 2,064 (1,999 to 2,163) | 2,032 (1,966 to 2,064) | 129 (125 to 156) | 258 (221 to 287) |
| heavy | 5 ms longer | 1,999 (1,999 to 2,097) | 1,966 (1,933 to 2,097) | 135 (119 to 139) | 320 (287 to 377) |

In these runs the 99th percentile of the time a commit's thread was off a processor is at most 0.16 ms, and the 99.9th at most 0.38 ms, under every background. The time off a processor is what the thread slept for in a wait and what it was kept off a processor for; a wait that spins is not in it. Under the heavy setting with fast syncs the 99th percentile of the operations of a block, which are made before the commit, is higher as well: 39.8 ms against 32.0 ms under the quiet setting (the medians of the five runs).

The parts of a commit that the engine counts apart, at the 99th percentile (µs; the range of the runs' values, the default and the heavy setting together; five runs, and three for the append):

| Part | Fast syncs | Syncs 5 ms longer |
| --- | --- | --- |
| `lock_waited` | under 1 | under 1 |
| `counts_waited` | 1 | 3 to 40 |
| `appending` | 184 to 201 | 201 to 217 |
| `placing` | 41 to 48 | 72 to 86 |
| `reclaiming` | 11 to 21 | 60 to 80 |
| `room_waited` | none in any run | none in any run |

### 3.2 Commits of 5 to 23 ms

In 54 runs with syncs 5 ms longer (108,000 commits, of several builds of this task) ten commits took 5.4 to 23.0 ms, and in 41 runs with fast syncs none took more than 4.1 ms. Two of the ten were made by a build with marks on the commit path: 4.5 of 6.3 ms and 6.3 of 8.0 ms lay between the mark before `LogStore::append_group` and the mark after it. The other eight were made before the append was counted apart, and show at most 0.11 ms in the parts that were. The longest of the ten, 23.0 ms, was in a run with the quiet background, which takes no table checkpoint.

In the runs that list their syncs, the harness reported for the two commits with the longest `appending` (0.84 and 0.75 ms, against a median of 0.10 ms) a sync of a segment or of the log's file that was under way at their end and had begun 9.6 to 11.2 ms earlier, 5 ms of which is the pause. The harness of those runs took a sync for under way while it had not listed the sync's end.

What such an append waits for was not found on that machine.

On a second machine of the same kind such commits are frequent (a `c-16-intel` whose 16 processors are a Xeon Gold 6548N's, with the same image, file system and mount options; the median sync of a 4 KiB append is 0.39 to 0.83 ms in eight probes there, and the longest 15.8 ms). Three runs of 2,000 blocks of each setting, the median and the range of the runs' values:

| Background | Syncs | 99%, µs | 99.9%, µs | Commits whose append took 2 ms or more, in each run |
| --- | --- | --- | --- | --- |
| default | as the disk makes them | 1,737 (1,507 to 1,868) | 2,475 (1,999 to 2,556) | none |
| default | 5 ms longer | 1,442 (1,376 to 1,704) | 9,699 (9,175 to 25,690) | 7, 7 and 4 |
| quiet | 5 ms longer | 1,409 (1,376 to 1,671) | 5,636 (4,719 to 10,486) | 4, 4 and 4 |

**The machine's own syncs are enough for such an append.** One more run of each background with syncs as the disk makes them had none under the default and under the heavy background and three under the quiet one: of 86.6, 80.2 and 3.1 ms, in commits that had a moment in common with a sync of 107, of 100 and of 27 ms. Over all the runs of this task's builds on that machine, 3 of 12,000 commits with syncs as the disk makes them had an append of 2 ms or more, and 74 of 30,000 with syncs 5 ms longer. The 99th percentile of the commit of a block was 1.34 to 1.87 ms in every one of those 21 runs.

In six of the runs with syncs 5 ms longer, the file layer of the harness timed each append call of the system's file layer (a change that is not in the harness). The six runs had 33 commits whose append took 2 ms or more (2.1 to 60.5 ms), and in all 33 the append call on the log's file, on the committing thread, took the time of the append to within 0.1 ms. For 31 of the 33 a sync of 8 ms or more, the pause included, had a moment in common with the commit: of a value-log file for 12, of the manifest's temporary file for 10, of a directory's names for 10, of the log's file for 3. **So on that machine an append to the log waits inside the file layer's call while files other than the log are synced.** Why the system holds the call was not found: no stack of the system was taken. A reading that was not tested is that the file system has one journal, that a sync of any file commits it, and that a write that makes a file longer waits for that commit.

### 3.3 Readers beside the writer

Reads through `GroveDb` with no transaction, on 1 to 47 threads beside a writer that applies blocks back to back, three runs of 3 s each, on the 48 processors. "Of one reader's" is the reads in a second of a reader's own processor time against those of one reader alone; the control's column is the same for its threads.

| Readers | `get`: reads/s | of one reader's | proof: reads/s | of one reader's | control: of one thread's |
| --- | --- | --- | --- | --- | --- |
| 1 | 74,800 | 100% | 9,300 | 100% | 100% |
| 4 | 287,700 | 96 to 98% | 37,000 | 98 to 104% | 98 to 100% |
| 8 | 598,100 | 98 to 100% | 73,100 | 97 to 101% | 98 to 99% |
| 16 | 1,181,800 | 98 to 100% | 148,400 | 99 to 102% | 93 to 99% |
| 24 | 1,562,000 | 86 to 89% | 184,700 | 77 to 85% | 87 to 89% |
| 32 | 1,780,100 | 73 to 75% | 204,900 | 68 to 71% | 70 to 74% |
| 40 | 1,915,800 | 64 to 65% | 209,400 | 56 to 58% | 60 to 62% |
| 47 | 2,038,600 | 59 to 60% | 225,500 | 52 to 55% | 54 to 56% |

**Up to 16 threads a reader makes 95 to 104% of one reader's reads in a second of its processor time.** Beyond 16 it makes fewer, and so does a thread of the control: 47 threads of the control do the work of 25. With no writer the rows are up to 11 points higher (a proof at 24 readers), and the control's up to 10.

How long one read takes beside the writer (µs; the range of the three runs):

| Readers | `get`: median | 99% | 99.9% | proof: median | 99% | 99.9% |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 13 | 22 | 41 to 43 | 109 | 135 to 188 | 184 to 225 |
| 16 | 13 | 22 | 48 to 50 | 107 to 109 | 164 to 172 | 188 to 197 |
| 32 | 18 to 20 | 31 | 70 to 72 | 139 to 164 | 221 to 225 | 270 to 278 |
| 47 | 22 | 35 | 80 to 86 | 205 to 209 | 254 to 270 | 410 to 3,146 |

With no writer the 99.9th percentile of a `get` is 24 to 39 µs at every count. The longest read of a run is 0.5 to 3.3 ms up to 24 threads and 6 to 21 ms at 47, where the longest step of a thread of the control is 6 to 10 ms.

### 3.4 The pool under readers

Pages read through a snapshot (`scaling.rs`), on the 48 processors beside a writer of 2,000 commits a second, each row the median of three runs of a second; against one reader's reads in a second of its processor time:

| Readers | A snapshot and an operation, no page | Four pages of its own | One page that all read, three of its own |
| --- | --- | --- | --- |
| 16 | 97% | 98% | 95% |
| 32 | 83% | 84% | 79% |
| 48 | 35% | 64% | 59% |

- **A read of pages that are in the pool, one of which every reader reads, keeps 59% of one reader's rate at 48 threads**, where 47 threads of the control keep 54 to 56% (3.3).
- **More readers than the pool has lanes** (128 lanes; these rows have more threads than processors as well). Four pages of its own beside the writer: 57% at 128 threads, 47% at 192, 45% at 256. The three other rows of that run (the page that all read, and both with no writer) change by at most five points from 128 to 256 threads. The rows change the number of threads and the sharing of lanes together and do not tell the two apart. The pool is given no parameter for the number of lanes.
- **A snapshot and an operation alone fall off past 32 threads**: 35% at 48, and fewer of them in all than at 32 (479 against 745 million in a second). One pair takes 34 ns alone and 98 ns at 48 threads; a `get` takes 13 µs. When that row was measured, a thread began its look for a vacant cell for a pin at its number times eight in a first chunk of 256 cells (`lignindb/src/snapshot/slots.rs`), which gave 32 cells to begin at, so more than 32 threads could not all begin at a cell of their own. Whether that is what the row shows was not isolated. A thread now begins at the cell that the bits of its number in reverse order name, so the first 256 threads each begin at a cell of their own, the first 32 eight cells apart as before and the first 64 four. Measured before and after that change on another machine of 48 processors (the same measurement, five rounds of each build in turn, the median of each row), beside the writer at 48 threads: a snapshot and an operation alone, 745 million in a second against 359 million, which is 64% of one reader's rate for a second of its processor time against 31%; four pages of its own, 143 million against 119; one page that all read and three of its own, 126 million against 107. At 16 and at 32 threads the two builds make the same (618 million pairs a second at 32). On that machine the fall past 32 threads follows the cells to begin at; the row above was measured on another machine and was not measured again.
- **Readers that load nearly every page they read do not scale.** With `POOL=128` and `SPREAD=256` (37.7 million pages loaded against 3,612 found in the pool, over the runs together), two to 48 readers together make 170,000 to 310,000 reads of four pages in a second, where one alone makes 157,000 to 192,000, and the writer beside two readers or more makes 940 to 1,700 of its 2,000 commits. A fixed frame is evicted under one lock for the whole pool, which is held while the frame's part of the table is locked and the lanes are looked through (`BufferPool::evict`). With `POOL=8192` for the same pages, where one page in nine thousand is loaded, the readers keep 96% at 16 threads, 80% at 32 and 69% at 48 beside the writer, and 96%, 81% and 63% with none.

## 4. What is open

1. **A slow append to the log** (3.2). On one of the two machines it waits inside the file layer's call beside syncs of other files; why the system holds the call was not found, and nothing here is built against it. Whether an append waits for a flush on a device whose flush takes milliseconds is to be measured on such a device: the file layer of the harness makes a sync longer, and not the file system's own part of it.
2. **Evictions are one at a time** (3.4), for readers and for the writer, which takes the frames of its copies from the same pool. It was measured on a store far smaller than the machine's memory, where the system's cache is taken to answer the reads of a file; that was not measured.
3. **The cells that threads begin at when they look for a pin's cell are 32** (3.4).

## 5. Choices made here that the plan left open

1. **"Unaffected" is read as within the range of the quiet setting from run to run**, for the 99th percentile of the commit of a block, and the time a commit's thread spends off a processor is reported beside it.
2. **A committer still tries the page steps of the values that a private copy held back.** Left to write-back alone, such a value is placed only by a pass that finds its page with no copy, and nothing has a pass come at such a moment while blocks are applied back to back.
3. **The measurements are tests that a normal run leaves out**, in `lignin-testkit`, and print tables.
