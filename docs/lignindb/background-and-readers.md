# Background work and readers beside the writer

LigninDB tasks PERF-07 and PERF-07-F1 ([plan](https://github.com/dashpay/grovedb/issues/1052)).

This document says which parts of a commit's time the engine counts apart, what keeps write-back's placing of values off the thread of a commit, how commits beside background work and readers beside a writer are measured, what the measurements gave, and what a long commit of a block waits for in the system (section 3.5). The rules it names are those of [transactions.md](transactions.md); the code is `lignindb::commit`, `lignin_store::flush` and `lignin_store::cache`.

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

**The commit of a block at a pace, on both engines (PERF-07-F1).** `lignin-bench latency` (`lignin_testkit::bench::latency`) drives each engine it is given, one after the other in one run and each in a fresh database, with one stream of the reference workload's shape: the documents are loaded back to back, the database is opened again under the run's background, 200 blocks are applied untimed, and then the measured blocks, with a pause after every commit (5 ms unless the run says otherwise). A run whose engines end at different root hashes fails.

- The time of a commit is an `Instant` around the commit of the block's transaction. A percentile of a run is a sample of it: the smallest with that share of the samples at or below it.
- The measuring thread's own processor clock is read around each commit as well, so a commit's time is also given as what its thread was off a processor for. For LigninDB the six counts of section 1 are read before each block and after its commit (`Db::stats`), outside every time that is taken. That call was seen to wait in `CleanerState::stats`, which takes the lock on the cleaner's list of the segments it writes and, for each of them, the segment's own lock. The pause after such a block is longer by the wait, and `waits.py` counts those waits apart, as the harness's between two blocks.
- The backgrounds are `quiet` and `heavy` as above, and `default`. GroveDB opens RocksDB with fixed options, so under `heavy` a second thread of the harness asks RocksDB for a flush of its memtables once ten more blocks are committed than when it last asked: at most one flush for ten blocks, and fewer where a flush outlasts them. Under `quiet` LigninDB keeps every block and every log file it writes, so a database under it grows by all that a run writes.
- Beside the measured blocks a thread of the harness sleeps for half a millisecond at a time and counts the sleeps that were late by 0.2, 1, 5 and 20 ms or more: what the machine does to a thread that waits for nothing. A percentile of a commit of a millisecond is read against it.

**What a commit waits for** is read from the system, on Linux, with `bpftrace` (`lignin-testkit/src/bench/latency/trace.bt`, run by `traced.sh` beside a latency run). The measuring thread has a name of its own. Each time it leaves a processor, the script keeps the kernel stack and the user stack it leaves with, but for the pause of the harness after a commit, which it knows by its length: a sleep that an engine asks for on that thread, as RocksDB does to slow its writes down, is a wait like another. When the thread is back after 0.2 ms or more it prints both, with the thread that woke it and the kernel stack of the waking. It prints every commit of an ext4 journal with the number of its transaction, when it began and when it ended; every sync of the process that asked to wait for such a commit, or that took a millisecond or more, with the file's directory and name, the transaction and when it asked (a shorter sync that asked for none is counted and not listed); and every write of the measuring thread that took a millisecond or more. The harness writes lines to `/dev/null`, which the script prints: where the measured blocks begin and end, and after a commit of 2 ms or more. `waits.py` makes its tables from such a trace: the waits of the measuring thread in the measured blocks by what they were for; and, for the waits in a write to a file of the log, the syncs that had a moment in common with the wait, which is an overlap in time, beside how many of those waits would have had a sync beside them had each begun at a moment taken at random; and apart from those, for a wait that by its kernel stack is for a commit of the journal, the commit that was under way when the wait began, if the trace has its beginning and its end, and the syncs that asked to wait for that commit before it had ended. The wait is tied to the commit by its stack and by the time; the number of the transaction is not read at the wait. A wait in the journal's checkpoint, for room, is for no commit and is counted apart.

**One command makes every table of section 3.5**: `lignin-testkit/src/bench/latency/campaign.sh OUT`, from a clean checkout, writes them to `OUT/tables.md` with the machine and the builds. It measures the file system that `OUT` is on. Its header says what it wants installed and what each part does. In short:

1. It makes three builds from clean checkouts of the checkout's commit, each in a target directory of its own and named by commit and sha256: the base, and the base with each of the two patches of section 3.5.3. Every build keeps its frame pointers, in Rust and in what is compiled from C and C++, because the trace reads a user stack by them; the builds of a campaign are alike in that and differ from a build of a node in it.
2. On `OUT`'s file system it makes rounds of four runs, one after the other, under each background, every run with both engines: the base, the base under a second name, and the two experiments. The second name is the control: two runs of one binary.
3. It makes the same rounds of three runs on two file systems that it makes in files under `OUT` and mounts through loop devices: the base and the base under a second name on ext4, and the base on XFS.
4. It makes the traced runs: each engine under each background, the experiments, and LigninDB on ext4, on ext4 with `lazytime` and on XFS in such files.

An even round makes its runs in the reverse order of the round before it, so that after two rounds every two runs were made in both orders, and the two engines of a run exchange their order from round to round. A directory holds one campaign: what it was asked is kept there, a campaign that is asked something else there is refused, and a campaign that was stopped goes on where it was.

A traced run writes a line where its measured blocks begin and where they end, and `waits.py` counts the waits between the two: the load and the warm blocks are applied by the same thread through the same calls. A wait is placed by its user stack, in a block or between two, and one whose stack does not reach the harness is counted apart. A report that is not of the name, the background, the blocks and the build that the campaign has for it, and a part of the campaign that has some of its runs and not all, end the campaign with no table. Before its runs a campaign makes a short traced run of each engine and reads it, so that a tracer that cannot attach, or stacks that are cut short, end it in its first minutes. After that a traced run that fails (a tracer that did not attach or that ended before its run, a trace with no such line), that is missing, or in which a fifth of the waits that are not between two blocks cannot be placed, gets no table of waits: the other runs are made, `tables.md` has the tables of the runs, which need no trace, and in that run's place why it has none. The campaign's last line says that it finished only if every table is there and is of what it says.

`pairs.sh` in the same directory is for a machine that other work shares: it makes two runs at a time, each on a file system of its own, so that both are made under what the machine does at that time, and has the two exchange their file systems from round to round.

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

### 3.5 The commit of a block at a pace, and what a long commit waits for

PERF-07-F1. 3.2 left open why an append to the log waits while files other than the log are synced, and whether RocksDB's commits on the same machine wait as well.

**The tables of this section are those of `campaign.sh` on a machine of the plan's reference class** (16 dedicated processors, a local NVMe disk, ext4), **and each has its place below, marked, until that run is made.** No figure of another machine is in this section. The mechanism of 3.5.2 was traced in a Linux guest (Linux 6.8, ext4 with `data=ordered` and `relatime`) of a machine that ran other work beside it; the counts and the times of that trace are in the pull request of the task, as figures of that guest.

#### 3.5.1 Both engines in one run

> **Place of a table of the reference class:** "The commit of a block, on the file system of the runs" of `tables.md`: for each engine and background, the median, the 99th and the 99.9th percentile and the longest commit of each run of 100,000 blocks, the 99th and the 99.9th percentile and the longest of the commit's time off a processor, and the median of the runs with their range.

#### 3.5.2 What a long commit waits for

**An append to the log waits where the file system stamps the log's file with the time of the change.** The thread is in `LogStore::append_group`, in the one `write` of the commit's group. It leaves the processor with this stack, innermost first:

```text
do_get_write_access
jbd2_journal_get_write_access
__ext4_journal_get_write_access
ext4_reserve_inode_write
__ext4_mark_inode_dirty
ext4_dirty_inode
__mark_inode_dirty
generic_update_time
file_modified
ext4_buffered_write_iter
ext4_file_write_iter
vfs_write
```

and is woken at `journal_end_buffer_io_sync`, from the end of a request to the device. So the write has not copied a byte yet. It is at `file_modified`, where a write gives the file the time of its change. ext4 keeps that change of the inode in its journal. The block of the inode table that holds the inode is in the transaction that the journal is committing, the journal is writing that block to the device, and until that write has ended nothing may change the block: the thread waits for its end, with its own change of the journal begun. A commit of the journal is what a sync asks for, of whatever file.

**Behind which sync.** The trace has every commit of the journal with the number of its transaction, and for a sync of the process the transaction whose commit it asked to wait for (`jbd2_log_wait_commit`) and when it asked. In a run that was traced so, every wait with the stack above began while a commit of the journal was under way, and a sync of the process, made by the thread of write-back or by the cleaner's, had asked to wait for that same commit before it ended: of a value-log file, of the manifest's temporary file, of the names of the database directory. They were few. Others were traced before the trace said when a sync asked: of those it says that a sync named the commit and was under way when it ended, which shows less. Many more were traced before the trace had the transactions at all: of those it says only that a sync of the process was under way at a moment of nearly every one, of a value-log file, of the manifest's temporary file, of the names of the database directory, of a segment, of a log file, of a table checkpoint. That is an overlap in time. It was there more often than for waits of those lengths at moments taken at random, and it does not say which sync a wait was behind. Which sync a commit of the journal was begun for the trace does not say: several can wait for one commit.

**It is the file's time and not its growth.** All but a few of those waits were at `file_modified`, before the write's data. A few were at the write's end (`ext4_da_write_end`), where ext4 records the file's new length, and waited there for a transaction of the journal that was being closed (`wait_transaction_locked`).

**Two more ways into the journal from the same call.** Where the journal has no room for the change, the writing thread goes into the journal's checkpoint (`__jbd2_log_wait_for_space`, `jbd2_log_do_checkpoint`) and waits there: for a buffer that is being written, for the journal's superblock (`jbd2_write_superblock`), for a flush of the device (`blkdev_issue_flush`). Who had the buffer written the trace does not say. From the same place the kernel can also have the thread wait for a commit (`jbd2_log_wait_commit`); it was seen in the traces, rarely. And where the journal's running transaction is being closed for its commit, a write that needs it waits for the next one (`wait_transaction_locked`).

**RocksDB's commits meet the same wait.** The `write` of a commit to RocksDB's log has the same stack in the trace, and so has the `fallocate` with which RocksDB sets room aside for its log. How often each engine meets it is the tables' to say: in the traces it went with how long the commits of the journal took in that run, and a run of RocksDB with long ones had more such waits than a run of LigninDB with short ones. In that run a commit also waited inside RocksDB, on a condition variable of its commit (`OptimisticTransaction::CommitWithParallelValidate`), from the beginning of a background thread's sync of a log file to its end. What the traces show to differ between the engines in every run is how often the journal commits: more often for LigninDB. GroveDB asks RocksDB for no sync at a commit (`WriteOptions::default()`), and every sync of RocksDB that the trace lists in the measured blocks was made by one of its background threads: of table files, of log files, of its own manifest and of the names of its directory. LigninDB's background threads sync several files for every block that is committed at this pace. Of each traced run, `tables.md` gives the commits of each journal in the measured blocks, and of LigninDB's the syncs a block by what they are of.

**A read waits the same way.** A `pread64` of the measuring thread, during the operations of a block and seldom in a commit, has the stack from `ext4_dirty_inode` down, under `touch_atime` and `filemap_read`: with `relatime`, a file that was changed since it was last read gets a new time of access at the next read, and a segment that is appended to is such a file.

**The locks of the engine.** The commit's thread was also seen to wait for locks that the threads of write-back and of the cleaner gave up: the lock on the counts of live values (`ValueStore::step`), what write-back keeps of commits (`WriteBack::committed`), the log's own lock where a commit asks for the place of its group (`LogStore::next_group`), and less often the pool's and write-back's lists of values to place. A page step appends its record to the log under the lock on the counts, and an append holds the log's lock for its call of the file layer, so an append of another thread that waits as above is one way to such a wait. Which thread held each lock, and for what, the trace does not say: it has the thread that woke the commit's, which is the one that gave the lock up last.

**The allocator.** During the operations of a block, and less often in a commit, the measuring thread waited for the lock of the C library's allocator (`__lll_lock_wait_private` under `malloc`, `realloc` and `free`), woken by the threads of the cleaner and of write-back. The harness is built with the system's allocator.

> **Place of the tables of the reference class:** "What the measuring thread waited for, in the traced runs" of `tables.md`, for LigninDB and RocksDB under each background: the waits by what they were for, with their number, their time in all and the longest; and for the waits in a write to a file of the log, the syncs under way at a moment of the wait and, apart from them, the syncs that waited for the commit of the journal that the wait began in.

#### 3.5.3 Three experiments

Each is held to one rule: it lowers the 99.9th percentile of LigninDB's commit by more than two runs of one binary differ. `tables.py` reads that as: under one background, the median of the experiment's runs is below the median of the control's runs by more than the control's runs are apart, the highest of them less the lowest. The control is one binary under two names: the base on the measured file system (`base`, `base-again`), and for the second file system the base on ext4 (`ext4`, `ext4-again`).

**The two experiments on the engine are patches beside the harness, and neither is in the engine.** A campaign builds each on the base. Each is a switch of the environment around a change that is made to be measured: no failure of the file layer is handled in it as the engine handles one, and no test holds it. What a change that is kept would have to bring is in section 4.

1. **The log's file allocated ahead** (`log-allocated-ahead.patch`, `LIGNIN_LOG_ALLOCATE=fallocate`). When a log file is created, its target length is allocated once (`fallocate`) and the file has that length from then on. An append is written at the place the log ends at: below the target length it extends nothing, and one that goes past it extends the file as before. What 3.5.2 says of it before any table: the append waits where the file's time is stamped, and a file that is allocated ahead has its time stamped like any other, so this change by itself leaves the wait where it is. It can take the second way away, the one at the write's end. A mount with `lazytime` does not take the first away either on the kernel that was traced (Linux 6.8): a write to a file there had the same stack from `file_modified` down to the journal, with the log allocated ahead and without. A campaign traces a run on such a mount with the log allocated ahead and one without, and makes no table of percentiles there.

   > **Place of a table of the reference class, and of one sentence, kept or not and why:** "The control and the two experiments", row `allocated`, of `tables.md`, with the waits of the traced runs `allocated-*`, `lignindb-*-on-ext4-lazytime` and `allocated-*-on-ext4-lazytime`.

2. **Fewer syncs in a turn of write-back** (`fewer-syncs.patch`, `LIGNIN_COVER_BYTES=1048576`). A turn of write-back's loop appends the pending values to the value log, syncs that file and replaces the manifest, which is a sync of the manifest's temporary file and one of the directory's names, before it gives a page a pointer to a value. With the patch a turn of the background loop leaves the pending values where they are while fewer than a megabyte of them are held and more are held than at the turn before, so one such set of three syncs covers the values of several turns. A turn that finds no more than the one before covers what is there, and so does a pass that may leave no page for a later one. The values that a regrouping copies, and what the cleaner moves, are covered as before: the patch does not reach them, and the cleaner is outside this task.

   > **Place of a table of the reference class, and of one sentence, kept or not and why:** "The control and the two experiments", row `covered`, of `tables.md`, with the syncs a block of the traced runs `lignindb-*` and `covered-*`.

3. **A second file system.** XFS beside ext4, both made by the campaign in files of the measured file system, so that the two differ in the file system and in nothing under it. No code. What a trace on XFS showed of the mechanism: where the write stamps the file's time, and where a read stamps the time of access, the thread waited for the lock of the inode (`xfs_ilock` under `xfs_vn_update_time`). Less often a read waited at `xfs_ilock` called from `xfs_file_buffered_read`, which is before the read and not at a time's stamping. Those were all the places in the file system at which the thread left the processor for a millisecond inside a measured block. What the holder of that lock waited for was not traced. The need is the same and the wait is at another place; whether it is shorter is the table's to say.

   > **Place of a table of the reference class, and of one sentence:** "A second file system: XFS against ext4" of `tables.md`, where the control is ext4 under its two names (rows `ext4` and `ext4-again`) and the row held against it is `xfs`, with the waits of the traced runs `lignindb-*-on-ext4` and `lignindb-*-on-xfs`.

## 4. What is open

1. **A slow append to the log** (3.2, 3.5). It waits inside the file layer's call where ext4 stamps the file's time, while the journal writes a commit or has no room (3.5.2), and nothing in the engine is built against it. **The tables of 3.5 are to be made on a machine of the reference class** with `campaign.sh`, and with them the three sentences of 3.5.3. Whether an append waits for a flush on a device whose flush takes milliseconds is to be measured on such a device: the file layer of the harness makes a sync longer, and not the file system's own part of it.
2. **What a kept form of the log allocated ahead has to bring**, if a table of the reference class earns it. The patch keeps the handle's own count of where the file ends and writes there; a kept form has to say what a failed append leaves in a file that is longer than what was appended, where today the engine reads the file's length to find out (`LogStore::append_checked`), and what the next session finds of a file that was allocated and not filled. A reader of the log ends at the first record that is not whole, and zeros are no record (`log-format.md`, LG31), so a tail that was allocated and never written ends the log where it should with no change of the format; a test and the crash campaigns have to show it, and the fault-injecting file layer has to model the call.
3. **The locks a commit waits for** (3.5.2): which thread holds the lock on the counts of live values, write-back's record of commits and the log's lock for milliseconds, and during what.
4. **The allocator's lock** (3.5.2), which the committing thread waits for beside the cleaner and write-back. Which allocations and frees meet was not looked for, nor what another allocator makes of it.
5. **A read that changes its file's time of access** (3.5.2). A file layer that opens its files with `O_NOATIME`, or a file system mounted with `noatime`, has no such change; neither was measured.
6. **Evictions are one at a time** (3.4), for readers and for the writer, which takes the frames of its copies from the same pool. It was measured on a store far smaller than the machine's memory, where the system's cache is taken to answer the reads of a file; that was not measured.
7. **The cells that threads begin at when they look for a pin's cell are 32** (3.4).

## 5. Choices made here that the plan left open

1. **"Unaffected" is read as within the range of the quiet setting from run to run**, for the 99th percentile of the commit of a block, and the time a commit's thread spends off a processor is reported beside it.
2. **A committer still tries the page steps of the values that a private copy held back.** Left to write-back alone, such a value is placed only by a pass that finds its page with no copy, and nothing has a pass come at such a moment while blocks are applied back to back.
3. **The measurements are tests that a normal run leaves out**, in `lignin-testkit`, and print tables.
4. **Both engines are paced by the clock** (PERF-07-F1): the same pause after every commit, whatever the engine. LigninDB's write-back is woken by each commit, and a report gives the turns of its loop a block, so that "a block a turn" can be read off it and is not taken for granted.
5. **A heavy background for RocksDB is a flush asked from a second thread** (PERF-07-F1), at most once for ten blocks. GroveDB opens RocksDB with fixed options, and the harness changes nothing below GroveDB.
6. **The rule of an experiment is read on medians** (3.5.3): the median of its runs against the median of the control's, and the control's runs apart by their highest less their lowest. With two rounds and two names the control has four runs of LigninDB under a background and an experiment two.
7. **An experiment on the engine is a patch beside the harness** (3.5.3), built by the campaign on the commit it measures, and not a switch in the engine: a change that no table has earned is in no build that is not made to measure it.
