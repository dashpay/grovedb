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

**The commit of a block at a pace, on both engines (PERF-07-F1).** `lignin-bench latency` (`lignin_testkit::bench::latency`) drives each engine it is given, one after the other in one run and each in a fresh database, with one stream of the reference workload's shape: the documents are loaded back to back, the database is opened again under the run's background, 200 blocks are applied untimed, and then the measured blocks, with a pause after every commit (5 ms unless the run says otherwise). The pause is counted from the commit's end, by the clock the commit is timed with, so that the two engines' blocks come at one pace whatever the harness does between them. A run whose engines end at different root hashes fails. A run takes the system's lock beside its database: a second run on the same scratch directory is refused, and a run begins by removing a database that an earlier one left there.

- The time of a commit is two readings of the system's monotonic clock around the commit of the block's transaction. A percentile of a run is a sample of it: the smallest with that share of the samples at or below it.
- The measuring thread's own processor clock is read around each commit as well, so a commit's time is also given as what its thread was off a processor for. For LigninDB the six counts of section 1 are read (`Db::stats`) once before the measured blocks and once after them. **A run whose percentiles are held against another's reads nothing of the engine between two blocks.** With `--parts`, as a traced run is made, the counts are read after each commit as well, outside every time that is taken and inside the pause. That call was seen to wait in `CleanerState::stats`, which takes the lock on the cleaner's list of the segments it writes and, for each of them, the segment's own lock. Where it takes longer than the pause the next block begins without one: a report counts the pauses that had passed before the harness came to sleep, whatever took the time, and the longest they had passed by; `waits.py` counts the waits apart, as the harness's between two blocks. A reading of those counts that takes no lock of the cleaner needs a call the engine does not have.
- The backgrounds are `quiet` and `heavy` as above, and `default`. GroveDB opens RocksDB with fixed options, so under `heavy` a second thread of the harness asks RocksDB for a flush of its memtables once ten more blocks are committed than when it last asked: at most one flush for ten blocks, and fewer where a flush outlasts them. Under `quiet` LigninDB keeps every block and every log file it writes, so a database under it grows by all that a run writes.
- Beside the measured blocks a thread of the harness sleeps for half a millisecond at a time and counts the sleeps that were late by 0.2, 1, 5 and 20 ms or more: what the machine does to a thread that waits for nothing. A percentile of a commit of a millisecond is read against it.

**What a commit waits for** is read from the system, on Linux, with `bpftrace` of 0.19 or later (`lignin-testkit/src/bench/latency/trace.bt`, run by `traced.sh` beside a latency run). The measuring thread has a name of its own. Each time it leaves a processor inside the measured blocks the script notes when, but for the pause of the harness, which is the first sleep the thread asks for after the line of a commit and before the line of the next block: a sleep that an engine asks for on that thread inside a block, as RocksDB does to slow its writes down, is a wait like another. When the thread is back after 0.2 ms or more the script takes its kernel stack and its user stack there, in the thread that comes back and before it goes on, which are the stacks it left with, and prints both, with the waking thread and the kernel stack of the waking, and with the file of the write or the allocation the thread was in, if it was in one. The waking thread is the one that made the sleeper runnable again (`sched_waking`); it need not be a thread that held what the sleeper waited for. It prints every commit of an ext4 journal with the number of its transaction, when it began and when it ended; every sync of the process that asked to wait for such a commit, or that took a millisecond or more, with the file's directory and name, the transaction and when it asked (a shorter sync that asked for none is counted and not listed); and every write of the measuring thread that took a millisecond or more. The harness writes lines to `/dev/null`, which the script prints: where the measured blocks begin and end, and in a traced run where each block begins and, after each commit, the commit's length and when it began, by the clock the commit was timed with, which is the clock the script stamps a wait with (`nsecs(monotonic)`; a trace in which a commit does not lie between the line of its block and its own line is refused, as one whose two clocks are not one). **Where a wait was is read from those lines**: one that ended before the commit began is of the block's operations, one inside the commit's own time is of the commit, and one after the commit ended is between two blocks, whenever the commit's line was written. No symbol of a stack, which a build can inline away, is needed for it. The stacks say what a wait was for, and a wait in a futex is given a name only by a frame that says it: the allocator where the frame above the C library's lock is one of its calls, an engine, the harness's file layer or the harness where the first frame that is not of a lock's own code is theirs, and what is left is counted as unresolved. `waits.py` makes its tables from such a trace: the waits of the measuring thread in the measured blocks by what they were for; the waits inside a write or an allocation by the file the tracer recorded for the call, the manifest's temporary file apart from the log's files, and one whose file the trace does not name counted as such; and, for the waits in a write to a file of the log, by that record and by no frame of a stack, the syncs that had a moment in common with the wait, which is an overlap in time, beside how many of those waits would have had a sync beside them had each begun at a moment taken at random; and apart from those, for a wait that by the frames it blocks in is for a commit of the journal, the commit that was under way when the wait began, if the trace has its beginning and its end, and the syncs that asked to wait for that commit before it had ended. The wait is tied to the commit by its stack and by the time; the number of the transaction is not read at the wait. Under `do_get_write_access` only a wait for the block's shadow bit is one for the journal's write of the block; a wait there for the block's lock is linked to nothing. A wait for room in the journal is for no commit and is counted apart.

**One command makes every table of section 3.5**: `lignin-testkit/src/bench/latency/campaign.sh OUT`, from a clean checkout, writes them to `OUT/tables.md` with the machine and the builds. It measures the file system that `OUT` is on. Its header says what it wants installed and what each part does. In short:

1. It makes three builds from clean checkouts of the checkout's commit, each in a target directory of its own and named by commit and sha256: the base, and the base with each of the two patches of section 3.5.3. Every build keeps its frame pointers, in Rust and in what is compiled from C and C++, because the trace reads a user stack by them; the builds of a campaign are alike in that and differ from a build of a node in it. Every build writes the paths of its checkout and its target directory the same way into what it compiles, so that a crate no patch touches comes out the same in all three, and lists its libraries by sha256: the campaign says which libraries each experiment's build has otherwise than the base's.
2. On `OUT`'s file system it makes rounds of four runs, one after the other, under each background, every run with both engines: the base, the base under a second name, and the two experiments. The second name is the control: two runs of one binary.
3. It makes the same rounds of three runs on two file systems that it makes in files under `OUT` and mounts through loop devices: the base and the base under a second name on ext4, and the base on XFS. Each is made on its loop device, and ext4 with its inode tables and its journal written out, which the kernel would otherwise write after the mount, beside the runs. A measured file system that was made shortly before the campaign should have been made so as well.
4. It makes the traced runs: each engine under each background, the experiments, and LigninDB on ext4, on ext4 with `lazytime` and on XFS in such files. A traced run marks every block and reads LigninDB's counts at each commit; the runs of 2 and 3 do neither.

An even round makes its runs in the reverse order of the round before it, so that after two rounds every two runs were made in both orders, and the two engines of a run exchange their order from round to round. A directory holds one campaign: what it was asked is kept there, a campaign that is asked something else there is refused, and a campaign that was stopped goes on where it was. The builds a campaign is made with are written down by its first call and a later call is held to them. Every run says the build that made it and the mount it was made on, by the file system's kind, options and id, and for a file system kept in a file the mount that file is on as well: a campaign that goes on refuses a run of another build or another mount. A campaign is of one host and of one file system under `OUT`: a call from another is refused, and runs made elsewhere are another campaign with its own controls. Work that is split over hosts of one kind is so many campaigns, each with its own `OUT` and the parts and backgrounds it is to make, a background's control and its experiments on one host; each has its own tables, which name its host, and no figure of one is held against a figure of another. **What a campaign assumes of its host:** a host given to it alone for as long as it runs; a fresh `OUT` with nothing mounted under it that the campaign did not mount; one copy of the builds a host; and its results left where it wrote them, none copied by hand from one directory into another. What the script checks of this it refuses by name, and it does not look for every way a host can be otherwise. One call at a time has a campaign's directory, and one a directory of builds. Under `OUT/mnt` the script removes and trims only on a file system it has verified to be its own, mounted at that directory, of the kind it makes there, from a loop device over its own file; a mount it does not know is refused and left alone, and so is `OUT/native`, where the runs on `OUT`'s own file system are made, if it is a mount point. The tables are made only of a directory that has what the campaign was asked.

A traced run writes a line where its measured blocks begin and where they end, and `waits.py` counts the waits between the two: the load and the warm blocks are applied by the same thread through the same calls. Before a figure is taken from a report it is held to the campaign and to its place in it: the words it was started with are those the campaign starts that place with, the engines are in the round's order, the switches of its experiment and no other were set, it was or was not watched, marked and read at each commit as its place has it, its binary, its host and its mount are the campaign's, and every report of the campaign has one stream, one pace, one machine and one list of settings for an engine under a background. A report that is not so ends the campaign with no table. A traced run does nothing before its tracer is attached: the harness says that its measuring thread is there, waits to be let go, and fails if it is not within two minutes. Before its runs a campaign makes a short traced run of each engine and reads it, so that a tracer that cannot attach, or stacks that are cut short, end it in its first minutes. That the stacks are whole wants twenty waits inside the blocks whose stacks are whole; with fewer the campaign makes a longer run, and where that has too few as well it says that the question is open on that machine. A trace of that check with no wait in it at all, in which the tracer does not count one time that its probe for a thread that comes back fired, ends the campaign: its marks and its syncs come from other probes, and such a trace is of a tracer whose probe never fired. Where the probe fired and no wait was long enough to be written, the check is inconclusive like any with too few. After that, whatever of the campaign is not whole gets no table, and `tables.md` has in its place what is missing: a part of runs that has not every one of its runs; a traced run that was not made or that failed (a tracer that did not attach, that ended before its run, that did not write its last line, or that did not end by itself with success after one interrupt; a trace without the harness's lines); a trace in which the tracer says that it lost events; and one that has twenty waits or more inside its blocks, of whatever length, of which a fifth do not reach the harness by their user stacks, so that what those waits were in is not known. A trace with fewer such waits says that its stacks are inconclusive. `bpftrace` keeps the stacks it takes in maps of a fixed size, and drops one that falls where another is kept; it says so in its error output, which is kept beside the trace. So that few fall on one another, a stack is taken only for a wait that is written, when it has ended, and not each time a thread leaves a processor. `waits.py` counts those lines, says how many waits inside a block have no user stack at all, and refuses a trace in which more stacks were dropped than five in a hundred of its waits, one in which more than five in a hundred of the measuring thread's own waits inside a block have no user stack, whatever the waits of other threads have, and a campaign's trace that has not the tracer's error output beside it. The tracer writes a wait from 0.2 ms and a sync, a write or a request to a device from a millisecond; these bounds are the campaign's, a table counts from no shorter a wait than its trace has, and a trace made with other bounds is refused. The parts a campaign is of are all that it was ever asked for, written down before any of them is begun. The tables that can be made are printed all the same: those of the runs need no trace. The campaign's last line says that it finished only if every table is there and is of what it says.

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
- **Readers that load nearly every page they read do not wait for one lock of the pool, and the writer beside them makes fewer commits.** With `POOL=128` and `SPREAD=256` nearly every page read is loaded (37.7 million loaded against 3,612 found in the pool, over the runs together). When the rows of this section were measured, a fixed frame was evicted under one lock for the whole pool: on the 48 processors two to 48 readers together made 170,000 to 310,000 reads of four pages in a second, where one alone made 157,000 to 192,000, and the writer beside two readers or more made 940 to 1,700 of its 2,000 commits. The clock now has no lock of its own: a thread that needs a frame moves the hand, which is an atomic, and locks only the part of the table that holds the frame it tries (`BufferPool::evict`). Measured before and after that change on a machine of 16 processors (the same measurement, three rounds of each build in turn, the median of each row and the range of the writer's commits; reads of four pages in a second by all readers):

  | Readers | No writer: before | after | Beside the writer: before | after | The writer's commits in a second: before | after |
  | --- | --- | --- | --- | --- | --- | --- |
  | 1 | 184,000 | 184,000 | 158,000 | 150,000 | 2,000 | 2,000 |
  | 2 | 268,000 | 250,000 | 218,000 | 200,000 | 2,000 | 2,000 |
  | 4 | 375,000 | 378,000 | 306,000 | 312,000 | 2,000 | 2,000 |
  | 8 | 276,000 | 554,000 | 270,000 | 382,000 | 1,999 to 2,000 | 1,320 to 1,884 |
  | 16 | 269,000 | 682,000 | 243,000 | 514,000 | 1,912 to 1,998 | 685 to 1,138 |

  With no writer eight readers make twice the reads they made and 16 make 2.5 times; beside the writer 1.4 and 2.1 times. Two readers make 7 to 8% fewer, and one beside the writer 5% fewer. The writer takes the frames of its copies from the same pool, and no lock holds the readers back from them now: beside eight such readers it makes 66 to 94% of its commits and beside 16 (17 threads on the 16 processors) 34 to 57%, where it made all of them and 96 to 100%. Before the change the writer on this machine made all its commits beside one to eight readers, so the 940 to 1,700 of the 48 processors are not a row of this table, and the 48 processors were not measured again. With `POOL=8192` for the same pages, where one page in nine thousand is loaded, the readers on the 48 processors kept 96% at 16 threads, 80% at 32 and 69% at 48 beside the writer, and 96%, 81% and 63% with none. On the 16 processors with `POOL=8192`, where no read of the runs loaded a page and none evicted, the two builds are within 2% of each other in every row but one, where two rounds of one build are up to 12% apart (two rounds of each in turn, the median of each row): 16 readers make 41.1 and 41.2 million reads of four pages in a second with no writer and 39.8 and 40.0 million beside it, eight readers 34.5 and 33.9 million and, the one row, 30.0 and 31.9 million, and the writer makes 1,996 to 2,000 of its commits before the change and 1,999 to 2,000 after it.

### 3.5 The commit of a block at a pace, and what a long commit waits for

PERF-07-F1. 3.2 left open why an append to the log waits while files other than the log are synced, and whether RocksDB's commits on the same machine wait as well.

**The tables of this section are those of `campaign.sh` on machines of the plan's reference class**, from two runs of it at its defaults (100,000 blocks a run, two rounds, the backgrounds `quiet` and `heavy`, traced runs of 30,000 blocks). Each run was made on four rented machines of one kind, each running nothing else: a `c-16-intel` droplet (16 dedicated processors, Intel Xeon Platinum 8358 under KVM, 32 GB, a local NVMe disk under ext4 with `relatime`, `discard`, `commit=30` and a journal of 64 MiB, Linux 6.8.0 on x86-64, Ubuntu 24.04, bpftrace 0.20.2, rustc 1.100.0). A campaign is of one machine, so each run is four campaigns: H1 made the runs of 3.5.1 under `quiet` and H2 under `heavy`, H3 the second file system under `quiet` and then every traced run, and H4 the second file system under `heavy`. No table compares rows of two machines. The four machines of a run built its three binaries alike, byte for byte, and each experiment's binary differs from the base only in the libraries its patch reaches (`lignin_io` and `lignin_store`, and what is built on them).

- **The run of record** measures the engine of `118a58059`, in the region `nyc1`. Its code of LigninDB's crates, of Merk, of the storage crate and of GroveDB's two engines is that of `588668e1f`, which has PERF-08's cleaner of the value log and its log files of 16 MiB; the builds of the scoring run (PERF-09) add to that engine only the reading function `lignindb::Db::space`, and the engine changes merged after `118a58059` are in none of these tables. Binaries: base `6135bb3b…dc4b77c1`, the log allocated ahead `ad61d6d8…93882e75`, fewer syncs `40c69860…aa16a7da` (sha256). The median sync of a 4 KiB append (`fdatasync`, of 1,000) on the file system of the runs: 0.28, 0.27, 0.25 and 0.23 ms on H1 to H4. 3.5.1 to 3.5.3 are of this run.
- **The first run** measures `f4eb8bccc`, the engine before PERF-08 (and before PERF-05-F2 and the checkpoint changes of OPS-03-F1 and OPS-03-F3), in the region `nyc3`; the median sync was 0.29, 0.34, 0.34 and 0.27 ms on H1 to H4. Its tables are in 3.5.4 and decide nothing.

In every run of percentiles of both (the 56 rows of an engine in a run, in each of the two), at most 0.03% of the sleeps of 500 µs of the harness's probe were late by a millisecond or more, and none by 5 ms. The preflight of each run found the user stacks whole on these machines: in the run of record 1,647 of 1,647 waits of LigninDB inside a block reached the harness, and 93 of 93 of RocksDB; no traced run dropped a stack. The mechanism of 3.5.2 was first traced in a Linux guest (Linux 6.8, ext4 with `data=ordered` and `relatime`) of a machine that ran other work beside it; the counts and the times of that trace are in the pull request of the task, as figures of that guest.

#### 3.5.1 Both engines in one run

##### PERF-07-F2's run of record: the commit of a block on both engines, `quiet` (H1) and `heavy` (H2)

The runs of the control: one binary, the base, under its two names `base` and `base-again`, two rounds each; every run has both engines, one after the other, and the order of the engines is turned from round to round. The store is of 100,000 documents of 1,000 owners (seed 1); a run applies 200 warm blocks and then measures 100,000, with a pause of 5 ms after each commit. The median of four runs is the mean of the two middle runs' figures, rounded half up; the range is the lowest and the highest of the four.

| Engine | Background | Run | Round | Median, µs | 99%, µs | 99.9%, µs | Longest, µs | Off a processor: 99%, µs | 99.9%, µs | longest, µs |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| LigninDB | quiet | base | 1 | 1,469 | 2,006 | 2,293 | 3,046 | 94 | 118 | 447 |
| LigninDB | quiet | base-again | 1 | 1,451 | 1,929 | 2,190 | 2,900 | 92 | 120 | 514 |
| LigninDB | quiet | base | 2 | 1,424 | 1,891 | 2,156 | 2,796 | 85 | 111 | 569 |
| LigninDB | quiet | base-again | 2 | 1,450 | 1,934 | 2,209 | 2,887 | 86 | 111 | 343 |
| **LigninDB** | **quiet** | **median of 4 (range)** | | 1,451 (1,424 to 1,469) | 1,932 (1,891 to 2,006) | **2,200** (2,156 to 2,293) | **2,894** (2,796 to 3,046) | 89 (85 to 94) | 115 (111 to 120) | 481 (343 to 569) |
| RocksDB | quiet | base | 1 | 2,611 | 3,265 | 3,954 | 5,619 | 0 | 17 | 3,078 |
| RocksDB | quiet | base-again | 1 | 2,582 | 3,209 | 4,164 | 8,585 | 0 | 16 | 6,352 |
| RocksDB | quiet | base | 2 | 2,592 | 3,224 | 4,074 | 5,739 | 0 | 15 | 3,185 |
| RocksDB | quiet | base-again | 2 | 2,608 | 3,294 | 4,089 | 5,666 | 0 | 16 | 1,747 |
| **RocksDB** | **quiet** | **median of 4 (range)** | | 2,600 (2,582 to 2,611) | 3,245 (3,209 to 3,294) | **4,082** (3,954 to 4,164) | **5,703** (5,619 to 8,585) | 0 (0 to 0) | 16 (15 to 17) | 3,132 (1,747 to 6,352) |
| LigninDB | heavy | base | 1 | 1,442 | 2,147 | 2,529 | 3,466 | 141 | 454 | 1,558 |
| LigninDB | heavy | base-again | 1 | 1,426 | 2,122 | 2,538 | 3,929 | 140 | 462 | 2,311 |
| LigninDB | heavy | base | 2 | 1,292 | 1,900 | 2,270 | 2,995 | 141 | 456 | 1,165 |
| LigninDB | heavy | base-again | 2 | 1,299 | 1,885 | 2,272 | 9,840 | 118 | 399 | 8,498 |
| **LigninDB** | **heavy** | **median of 4 (range)** | | 1,363 (1,292 to 1,442) | 2,011 (1,885 to 2,147) | **2,401** (2,270 to 2,538) | **3,698** (2,995 to 9,840) | 141 (118 to 141) | 455 (399 to 462) | 1,935 (1,165 to 8,498) |
| RocksDB | heavy | base | 1 | 2,043 | 13,513 | 18,227 | 49,065 | 5 | 5,214 | 46,684 |
| RocksDB | heavy | base-again | 1 | 2,045 | 13,528 | 18,735 | 51,357 | 5 | 6,269 | 48,949 |
| RocksDB | heavy | base | 2 | 1,931 | 12,916 | 17,037 | 36,671 | 5 | 5,799 | 34,177 |
| RocksDB | heavy | base-again | 2 | 1,935 | 13,033 | 17,071 | 52,724 | 4 | 6,035 | 50,530 |
| **RocksDB** | **heavy** | **median of 4 (range)** | | 1,989 (1,931 to 2,045) | 13,273 (12,916 to 13,528) | **17,649** (17,037 to 18,735) | **50,211** (36,671 to 52,724) | 5 (4 to 5) | 5,917 (5,214 to 6,269) | 47,817 (34,177 to 50,530) |

**LigninDB's tail is not worse than RocksDB's on this class, quiet or heavy.** At the 99.9th percentile LigninDB's commit took 2,200 µs under `quiet` and 2,401 µs under `heavy`, RocksDB's 4,082 and 17,649 µs; the longest commit of a run took LigninDB 2,894 and 3,698 µs and RocksDB 5,703 and 50,211 µs (medians of the four runs). Over the four runs, LigninDB's 99.9th percentile was 2,156 to 2,293 µs under `quiet` against RocksDB's 3,954 to 4,164, and 2,270 to 2,538 µs under `heavy` against 17,037 to 18,735; its longest commit 2,796 to 3,046 µs against 5,619 to 8,585, and 2,995 to 9,840 µs against 36,671 to 52,724. In each of the eight runs LigninDB's 99.9th percentile and its longest commit were below RocksDB's of the same run. Under `heavy` the harness asks RocksDB for a flush of its memtables at most once in ten blocks (section 5, item 5); in each of its four runs there, its three longest commits were off a processor for 92 to 96% of their time. LigninDB's longest commit under `heavy` was 9,840 µs in one run, 8,498 µs of it off a processor; in the other three it was 3.0 to 3.9 ms.

Every table of `campaign.sh` of both runs, its reports and its traces are kept with the evidence of PERF-07-F2; the rows above are those of "The commit of a block, on the file system of the runs".

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

**Behind which sync.** The trace has every commit of the journal with the number of its transaction, and for a sync of the process the transaction whose commit it asked to wait for (`jbd2_log_wait_commit`) and when it asked. In a run that was traced so, every wait with the stack above began while a commit of the journal was under way, and a sync of the process, made by the thread of write-back or by the cleaner's, had asked to wait for that same commit before it ended: of a value-log file, of the manifest's temporary file, of the names of the database directory. The trace has when a sync asked, and not whether it then slept: the kernel takes a commit for done shortly before it tells the trace so, and a sync that asks in between waits for nothing. In that run each sync had asked before the commit began. They were few. Others were traced before the trace said when a sync asked: of those it says that a sync named the commit and was under way when it ended, which shows less. Many more were traced before the trace had the transactions at all: of those it says only that a sync of the process was under way at a moment of nearly every one, of a value-log file, of the manifest's temporary file, of the names of the database directory, of a segment, of a log file, of a table checkpoint. That is an overlap in time. It was there more often than for waits of those lengths at moments taken at random, and it does not say which sync a wait was behind. Which sync a commit of the journal was begun for the trace does not say: several can wait for one commit.

**It is the file's time and not its growth.** All but a few of those waits were at `file_modified`, before the write's data. A few were at the write's end (`ext4_da_write_end`), where ext4 records the file's new length, and waited there for a transaction of the journal that was being closed (`wait_transaction_locked`).

**Two more ways into the journal from the same call.** Where the journal has no room for the change, the writing thread goes for the journal's checkpoint (`__jbd2_log_wait_for_space`) and waits: for the checkpoint's lock, or inside it (`jbd2_log_do_checkpoint`) for a buffer that is being written, for the journal's superblock (`jbd2_write_superblock`), for a flush of the device (`blkdev_issue_flush`). Who held the lock or had the buffer written the trace does not say. From the same place the kernel can also have the thread wait for a commit (`jbd2_log_wait_commit`); it was seen in the traces, rarely. And where the journal's running transaction is being closed for its commit, a write that needs it waits for the next one (`wait_transaction_locked`).

**RocksDB's commits meet the same wait.** The `write` of a commit to RocksDB's log has the same stack in the trace, and so has the `fallocate` with which RocksDB sets room aside for its log. How often each engine meets it on the reference class is in the tables below: on those machines RocksDB's commit met it and LigninDB's did not. In the guest's traces it went with how long the commits of the journal took in that run, and a run of RocksDB with long ones had more such waits than a run of LigninDB with short ones. In that run a commit also waited inside RocksDB, on a condition variable of its commit (`OptimisticTransaction::CommitWithParallelValidate`), from the beginning of a background thread's sync of a log file to its end. What the traces show to differ between the engines in every run is how often the journal commits: more often for LigninDB. GroveDB asks RocksDB for no sync at a commit (`WriteOptions::default()`), and every sync of RocksDB that the trace lists in the measured blocks was made by one of its background threads: of table files, of log files, of its own manifest and of the names of its directory. LigninDB's background threads sync several files for every block that is committed at this pace. Of each traced run, `tables.md` gives the commits of each journal in the measured blocks, and of LigninDB's the syncs a block by what they are of.

**A read waits the same way.** A `pread64` of the measuring thread, during the operations of a block and seldom in a commit, has the stack from `ext4_dirty_inode` down, under `touch_atime` and `filemap_read`: with `relatime`, a file that was changed since it was last read gets a new time of access at the next read, and a segment that is appended to is such a file.

**The locks of the engine.** The commit's thread was also seen to wait for locks of the engine, with a thread of write-back or of the cleaner as the waking thread: the lock on the counts of live values (`ValueStore::step`), what write-back keeps of commits (`WriteBack::committed`), the log's own lock where a commit asks for the place of its group (`LogStore::next_group`), and less often the pool's and write-back's lists of values to place. A page step appends its record to the log under the lock on the counts, and an append holds the log's lock for its call of the file layer, so an append of another thread that waits as above is one way to such a wait. Which thread held each lock, and for what, the trace does not say: it has the waking thread, the one that made the commit's thread runnable again, and that thread is not shown to have held the lock.

**The allocator.** During the operations of a block, and less often in a commit, the measuring thread waited for the lock of the C library's allocator (`__lll_lock_wait_private` under `malloc`, `realloc` and `free`), with the threads of the cleaner and of write-back as the waking threads. The harness is built with the system's allocator.

##### PERF-07-F2's run of record: what the measuring thread waited for (H3)

Traced runs of 30,000 blocks of the base, one engine each, on the file system of the runs; the waits of the measuring thread of a millisecond or more inside a block, by what `waits.py` reads them to be.

| Run | Where | What the thread waited for | Waits | In all, ms | Longest, ms |
| --- | --- | --- | --- | --- | --- |
| LigninDB, `quiet` | the operations | a lock of the engine, or a condition it waits on | 86 | 107.9 | 1.8 |
| LigninDB, `heavy` | the operations | a lock of the engine, or a condition it waits on | 8 | 8.7 | 1.3 |
| LigninDB, `heavy` | the operations | `pread64`: ext4 writes the file's times to the journal, which is writing that block of inodes | 2 | 6.8 | 4.7 |
| RocksDB, `quiet` | the commit | taken off the processor while it could run | 1 | 2.0 | 2.0 |
| RocksDB, `quiet` | the operations | taken off the processor while it could run | 1 | 2.8 | 2.8 |
| RocksDB, `heavy` | the commit | `fallocate`: ext4 writes the file's times to the journal, which is writing that block of inodes | 16 | 181.6 | 45.1 |
| RocksDB, `heavy` | the commit | `write`: ext4 writes the file's times to the journal, which is writing that block of inodes | 11 | 128.9 | 26.5 |
| RocksDB, `heavy` | the commit | `fallocate`: ext4 writes a change of the file to the journal, which is writing the block it changes | 1 | 7.7 | 7.7 |
| RocksDB, `heavy` | the commit | taken off the processor while it could run | 1 | 3.0 | 3.0 |
| RocksDB, `heavy` | the commit | `mprotect`, for the lock of the process's memory map | 1 | 2.2 | 2.2 |
| RocksDB, `heavy` | the operations | taken off the processor while it could run | 3 | 14.9 | 6.0 |
| RocksDB, `heavy` | the operations | a lock of the engine, or a condition it waits on | 3 | 10.6 | 8.4 |

| Run | Commit of the block, µs: median, 99%, 99.9%, longest | Syncs a block | Commits of the journal in the measured blocks: a second, median, longest |
| --- | --- | --- | --- |
| LigninDB, `quiet` | 1,224, 1,683, 1,922, 2,333 | 5.47: 1.81 of the manifest's temporary file, 1.81 of the names of the database directory, 1.77 of a value-log file, 0.08 of others | 193.0, 0.23 ms, 7.3 ms |
| LigninDB, `heavy` | 1,248, 1,827, 2,175, 2,609 | 11.43: 3.25, 3.25 and 2.02 of those, 1.01 of a segment, 0.65 of a log file, 0.41 of a table checkpoint, 0.84 of the names of the four directories | 393.2, 0.22 ms, 40.5 ms |
| RocksDB, `quiet` | 2,424, 3,018, 3,446, 4,497 | | 3.1, 0.35 ms, 2.3 ms |
| RocksDB, `heavy` | 1,919, 12,340, 16,290, 48,130 | | 36.8, 0.26 ms, 61.8 ms |

The syncs a block are LigninDB's, counted by the harness's file layer; GroveDB asks RocksDB for no sync at a commit. In LigninDB's traced run under `heavy`, 416 more waits (1,042 ms in all) were between two blocks, where a traced run reads the engine's counts (section 2); they are in no row.

**The waits in a write to a file of the log.** LigninDB had none of a millisecond in either traced run, nor in any of the seven other traced runs of LigninDB (3.5.3). RocksDB under `heavy` had 28 (318.2 ms in all), in the `write` or the `fallocate` for a file of its log, each of them for a commit of the ext4 journal by the frames it blocked in, and each begun while a commit of that journal was under way. A sync of the process had a moment in common with 3 of the 28, of the names of RocksDB's directory, where waits begun at moments taken at random would have had 3.0; for 3 of the 28, a sync of the names of RocksDB's directory had asked to wait for the commit of the journal that the wait began in, before it ended. Which sync the other 25 commits of the journal were begun for the trace does not say.

**What a long commit waited for on this machine, against the mechanism above.** In 30,000 traced blocks under each background LigninDB's commit did not once wait a millisecond. Its waits of a millisecond or more were in the operations of a block: for a lock of the engine or a condition it waits on, with write-back's thread as the waking thread (all 86 in the run that follows every thread of the process), and twice in a read, where ext4 stamps the file's time of access. The wait this section describes, an append held at `file_modified` while the journal writes the block of inodes, is on this machine RocksDB's: under `heavy` its commit met it 28 times, the longest for 45 ms, in a write or an allocation for its log. LigninDB's journal committed 193 to 393 times a second at a median of 0.22 to 0.23 ms, and the longest commit of the journal took 7.3 and 40.5 ms; none of those commits held an append to LigninDB's log for a millisecond. The mechanism stands as the cause of a long commit when the journal commits while an append stamps the log's file; on a disk whose syncs take a quarter of a millisecond, LigninDB's appends did not meet it in these runs. No read of the measuring thread in the traced runs of LigninDB on ext4 waited for its page from the device: every read that waited was at a stamp of the file's time, the store being held in the system's cache, so what a read of several blocks from the device costs is not measured here.

#### 3.5.3 Three experiments

Each is held to one rule: it lowers the 99.9th percentile of LigninDB's commit by more than two runs of one binary differ. `tables.py` reads that as: under one background, the median of the experiment's runs is below the median of the control's runs by more than the control's runs are apart, the highest of them less the lowest. The control is one binary under two names: the base on the measured file system (`base`, `base-again`), and for the second file system the base on ext4 (`ext4`, `ext4-again`).

**The two experiments on the engine are patches beside the harness, and neither is in the engine.** A campaign builds each on the base. Each is a switch of the environment around a change that is made to be measured: no failure of the file layer is handled in it as the engine handles one, and no test holds it. What a change that is kept would have to bring is in section 4.

1. **The log's file allocated ahead** (`log-allocated-ahead.patch`, `LIGNIN_LOG_ALLOCATE=fallocate`). When a log file is created, its target length is allocated once (`fallocate`) and the file has that length from then on. An append is written at the place the log ends at: below the target length it extends nothing, and one that goes past it extends the file as before. What 3.5.2 says of it before any table: the append waits where the file's time is stamped, and a file that is allocated ahead has its time stamped like any other, so this change by itself leaves the wait where it is. It can take the second way away, the one at the write's end. A mount with `lazytime` does not take the first away either on the kernel that was traced (Linux 6.8): a write to a file there had the same stack from `file_modified` down to the journal, with the log allocated ahead and without. A campaign traces a run on such a mount with the log allocated ahead and one without, and makes no table of percentiles there.

   The run of record (H1 under `quiet`, H2 under `heavy`; 99.9th percentile of LigninDB's commit, µs):

   | Background | Rows | Runs | Median (range) | Below the control's median by | The control's runs are apart by | By more than that |
   | --- | --- | --- | --- | --- | --- | --- |
   | quiet | `base`, `base-again` (the control) | 4 | 2,200 (2,156 to 2,293) | | 138 | |
   | quiet | `allocated` | 2 | 2,196 (2,191 to 2,201) | 4 | 138 | no |
   | heavy | `base`, `base-again` (the control) | 4 | 2,401 (2,270 to 2,538) | | 268 | |
   | heavy | `allocated` | 2 | 2,488 (2,440 to 2,535) | −87 | 268 | no |

   The traced runs under `quiet` (H3), 30,000 blocks each: `allocated-quiet` (the log allocated ahead) had no wait of a millisecond in a write to the log, nor had `lignindb-quiet` (3.5.2); on ext4 with `lazytime`, in a file of the measured file system, neither `allocated-quiet-on-ext4-lazytime` nor `lignindb-quiet-on-ext4-lazytime` had one. Their waits of a millisecond were in the operations, for a lock of the engine (107, 101 and 113, of at most 1.9 ms) and, in `lignindb-quiet-on-ext4-lazytime`, twice off the processor while it could run.

   **Not kept.** With the log allocated ahead, the 99.9th percentile of the commit is 4 µs below the control's median under `quiet` and 87 µs above it under `heavy`, where the control's own four runs are 138 and 268 µs apart; and on this machine there was no wait for it to take away, since no traced run, with the log allocated ahead or without, had an append wait a millisecond. So nothing of it is built, and section 4, item 2 stays what a kept form would have to bring.

2. **Fewer syncs in a turn of write-back** (`fewer-syncs.patch`, `LIGNIN_COVER_BYTES=1048576`). A turn of write-back's loop appends the pending values to the value log, syncs that file and replaces the manifest, which is a sync of the manifest's temporary file and one of the directory's names, before it gives a page a pointer to a value. With the patch a turn of the background loop leaves the pending values where they are while fewer than a megabyte of them are held and more are held than at the turn before, so one such set of three syncs covers the values of several turns. A turn that finds no more than the one before covers what is there, and so does a pass that may leave no page for a later one. The values that a regrouping copies, and what the cleaner moves, are covered as before: the patch does not reach them, and the cleaner is outside this task.

   The run of record (H1 under `quiet`, H2 under `heavy`; 99.9th percentile of LigninDB's commit, µs):

   | Background | Rows | Runs | Median (range) | Below the control's median by | The control's runs are apart by | By more than that |
   | --- | --- | --- | --- | --- | --- | --- |
   | quiet | `base`, `base-again` (the control) | 4 | 2,200 (2,156 to 2,293) | | 138 | |
   | quiet | `covered` | 2 | 2,463 (2,437 to 2,488) | −263 | 138 | no |
   | heavy | `base`, `base-again` (the control) | 4 | 2,401 (2,270 to 2,538) | | 268 | |
   | heavy | `covered` | 2 | 2,632 (2,527 to 2,737) | −232 | 268 | no |

   The syncs a block of the traced runs under `quiet` (H3), 30,000 blocks each: `covered-quiet` 2.69 (0.88 of the manifest's temporary file, 0.88 of the names of the database directory, 0.85 of a value-log file, 0.08 of others) against 5.47 of `lignindb-quiet` (1.81, 1.81, 1.77 and 0.08); the journal committed 92.2 times a second against 193.0. Its commit's time off a processor was longer (99.9%: 341 µs against 88), and its commit waited once for a lock of the engine (1.3 ms), where the base's did not wait.

   **Not kept.** Fewer syncs in a turn of write-back halves the syncs of a block and the commits of the journal, and the 99.9th percentile of the commit is 263 µs above the control's median under `quiet` and 232 µs above it under `heavy`, where the control's runs are 138 and 268 µs apart: worse by more than the control's spread under `quiet`, and nowhere lower. Nothing of it is built.

3. **A second file system.** XFS beside ext4, both made by the campaign in files of the measured file system, so that the two differ in the file system and in nothing under it. No code. What a trace on XFS showed of the mechanism: where the write stamps the file's time, and where a read stamps the time of access, the thread waited for the lock of the inode (`xfs_ilock` under `xfs_vn_update_time`). Less often a read waited at `xfs_ilock` called from `xfs_file_buffered_read`, which is before the read and not at a time's stamping. Those were all the places in the file system at which the thread left the processor for a millisecond inside a measured block. What the holder of that lock waited for was not traced. The need is the same and the wait is at another place; on the reference class the table below finds no difference beyond the control's spread.

   The run of record (H3 under `quiet`, H4 under `heavy`; the file systems in files of the measured one, through loop devices with direct I/O; ext4 there has a journal of 512 MiB; 99.9th percentile of LigninDB's commit, µs):

   | Background | Rows | Runs | Median (range) | Below the control's median by | The control's runs are apart by | By more than that |
   | --- | --- | --- | --- | --- | --- | --- |
   | quiet | `ext4`, `ext4-again` (the control) | 4 | 2,256 (2,121 to 2,534) | | 413 | |
   | quiet | `xfs` | 2 | 2,144 (2,120 to 2,167) | 113 | 413 | no |
   | heavy | `ext4`, `ext4-again` (the control) | 4 | 2,258 (2,214 to 2,310) | | 96 | |
   | heavy | `xfs` | 2 | 2,317 (2,288 to 2,345) | −59 | 96 | no |

   The median sync of a 4 KiB append there: on ext4 0.71 ms (H3) and 0.60 ms (H4), on XFS 0.36 and 0.31 ms. The traced runs under `quiet` (H3), 30,000 blocks each: `lignindb-quiet-on-ext4` waited a millisecond or more 134 times, all for a lock of the engine in the operations; `lignindb-quiet-on-xfs` 88 times for a lock of the engine, 11 times taken off the processor while it could run, and 4 times in a read where XFS stamps the file's time and waits for the lock of the inode (`xfs_ilock`, 11.2 ms in all, the longest 3.9 ms). Neither had a wait in a write to the log, and the commit's 99.9th percentile in them was 1,945 µs on ext4 and 1,843 µs on XFS.

   **Not kept.** XFS is 113 µs below the ext4 control's median under `quiet` and 59 µs above it under `heavy`, where the control's runs are 413 and 96 µs apart: within the spread of one binary on one file system under both backgrounds. On XFS a read waited a millisecond for the inode's lock four times where on ext4 no read waited a millisecond, and on neither did an append to the log wait.

#### 3.5.4 The first run: the engine before the cleaner's change

The same command, at the same defaults, split over four machines in the same way, on `f4eb8bccc`: the engine before PERF-08 changed the cleaner of the value log and the length of the log's files, and before PERF-05-F2 and the checkpoint changes of OPS-03-F1 and OPS-03-F3. Region `nyc3`; the median sync of a 4 KiB append was 0.29, 0.34, 0.34 and 0.27 ms on H1 to H4. Binaries: base `951041ab…59b9dfdb1`, the log allocated ahead `f13ad086…7f8351e1`, fewer syncs `2b099ebd…09be1a373`. These tables decide nothing.

The commit of a block, medians of the four runs of the control (range), µs:

| Engine | Background | Median | 99% | 99.9% | Longest | Off a processor: 99% | 99.9% | longest |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| LigninDB | quiet | 1,678 (1,660 to 1,702) | 2,163 (2,138 to 2,257) | 2,450 (2,426 to 2,555) | 3,724 (3,021 to 6,239) | 106 (102 to 114) | 410 (394 to 435) | 2,010 (665 to 4,398) |
| RocksDB | quiet | 3,088 (3,076 to 3,097) | 3,794 (3,766 to 3,799) | 4,193 (4,157 to 4,273) | 5,769 (5,155 to 36,012) | 0 (0 to 0) | 22 (19 to 24) | 2,224 (270 to 33,399) |
| LigninDB | heavy | 1,796 (1,789 to 1,800) | 2,605 (2,581 to 2,616) | 3,157 (3,122 to 3,229) | 8,303 (6,147 to 9,802) | 206 (193 to 231) | 607 (587 to 692) | 5,178 (3,030 to 5,479) |
| RocksDB | heavy | 2,600 (2,597 to 2,615) | 18,564 (18,378 to 18,618) | 25,042 (24,121 to 25,538) | 48,715 (37,581 to 78,987) | 7 (6 to 8) | 5,454 (4,385 to 7,209) | 45,740 (33,651 to 76,042) |

In each of its eight runs of the control LigninDB's 99.9th percentile was below RocksDB's of the same run, and so was its longest commit in seven: in one run under `quiet` LigninDB's longest took 6,239 µs and RocksDB's 6,003 µs. Over the four runs under `quiet`, LigninDB's longest commit reached 6,239 µs where RocksDB's lowest longest commit was 5,155 µs; at the 99.9th percentile the first run answers as the run of record does in every run of the control.

The experiments, 99.9th percentile of LigninDB's commit, µs:

| Background | Rows | Median (range) | Below the control's median by | The control's runs are apart by | By more than that |
| --- | --- | --- | --- | --- | --- |
| quiet | `base`, `base-again` (the control) | 2,450 (2,426 to 2,555) | | 129 | |
| quiet | `allocated` | 2,442 (2,439 to 2,445) | 8 | 129 | no |
| quiet | `covered` | 2,776 (2,724 to 2,828) | −326 | 129 | no |
| heavy | `base`, `base-again` (the control) | 3,157 (3,122 to 3,229) | | 107 | |
| heavy | `allocated` | 3,209 (3,098 to 3,321) | −52 | 107 | no |
| heavy | `covered` | 3,266 (3,255 to 3,277) | −109 | 107 | no |
| quiet | `ext4`, `ext4-again` (the control) | 2,402 (2,332 to 2,591) | | 258 | |
| quiet | `xfs` | 2,565 (2,534 to 2,595) | −163 | 258 | no |
| heavy | `ext4`, `ext4-again` (the control) | 3,216 (3,113 to 3,323) | | 210 | |
| heavy | `xfs` | 3,177 (3,136 to 3,217) | 39 | 210 | no |

The traced runs (H3, 30,000 blocks each): of LigninDB's nine, one had a wait of a millisecond in a write to the log, on XFS (2.0 ms, at the inode's lock); RocksDB under `heavy` had 49 (360.8 ms in all, the longest 27.9 ms) in the `write` or the `fallocate` for its log, at the journal as in the run of record. The preflight found the user stacks whole (2,576 of 2,576 waits of LigninDB inside a block, 91 of 91 of RocksDB).

**What moved in the tail.** From the first run to the run of record LigninDB's 99.9th percentile went from 2,450 to 2,200 µs under `quiet` and from 3,157 to 2,401 µs under `heavy`, and its median from 1,678 to 1,451 and from 1,796 to 1,363 µs: lower by more than either run's control is apart (129 and 138 µs under `quiet`, 107 and 268 under `heavy`). That is not the cleaner's change alone. The engine between the two runs changed by more (Merk's handle of PERF-05-F2, the checkpoints' staging), and the two runs are on machines of two regions, on which RocksDB, whose path through a commit is the same code in both runs, moved as well: its 99.9th percentile from 4,193 to 4,082 µs under `quiet` and from 25,042 to 17,649 µs under `heavy`, its median from 3,088 to 2,600 and from 2,600 to 1,989 µs. Within each run, LigninDB's 99.9th percentile was 0.58 and then 0.54 of RocksDB's under `quiet`, and 0.13 and 0.14 under `heavy`. LigninDB's syncs a block stayed where they were (5.45 and 5.47 under `quiet`, 11.58 and 11.43 under `heavy`, in the traced runs). What the tables show of the cleaner's change is that with it the tail is no longer: on these machines it is shorter, and it stays below RocksDB's by about as much as before.

## 4. What is open

1. **A slow append to the log** (3.2, 3.5). It waits inside the file layer's call where ext4 stamps the file's time, while the journal writes a commit or has no room (3.5.2), and nothing in the engine is built against it. On the machines of the reference class (3.5) no traced run of LigninDB had an append wait a millisecond, and RocksDB's commit under `heavy` did; none of the three experiments of 3.5.3 is kept. Whether an append waits for a flush on a device whose flush takes milliseconds is to be measured on such a device: the file layer of the harness makes a sync longer, and not the file system's own part of it.
2. **What a kept form of the log allocated ahead has to bring**, if a later table earns it: on the reference class none did (3.5.3). The patch keeps the handle's own count of where the file ends and writes there; a kept form has to say what a failed append leaves in a file that is longer than what was appended, where today the engine reads the file's length to find out (`LogStore::append_checked`), and what the next session finds of a file that was allocated and not filled. A reader of the log ends at the first record that is not whole, and zeros are no record (`log-format.md`, LG31), so a tail that was allocated and never written ends the log where it should with no change of the format. A kept form has to show that by a test and by both crash campaigns of `crash-tests.md`, the store's (`lignin-ci.sh crash`) and the engine's (`lignin-ci.sh crash-engine`), each ending clean; the fault-injecting file layer of `lignin-io` has to model the allocation call; and the Miri and loom jobs of `ci.md` have to pass where `lignin-io` or the buffer pool is touched (`lignin-ci.sh miri`, `lignin-ci.sh loom`). No format version changes. If the reader turned out to need a change of the format to end at such a tail, the work stops there and is reported with the figures.
3. **The locks a commit waits for** (3.5.2): which thread holds the lock on the counts of live values, write-back's record of commits and the log's lock for milliseconds, and during what.
4. **The allocator's lock** (3.5.2), which the committing thread waits for beside the cleaner and write-back. Which allocations and frees meet was not looked for, nor what another allocator makes of it.
5. **A read that changes its file's time of access** (3.5.2). A file layer that opens its files with `O_NOATIME`, or a file system mounted with `noatime`, has no such change; neither was measured.
6. **The writer beside readers that evict at every read** (3.4). The writer takes the frames of its copies from the pool the readers take theirs from, and since the clock has no lock of its own it makes 34 to 57% of its commits beside 16 such readers on 16 processors. It was measured with a pool of 128 frames, on a store far smaller than the machine's memory, where the system's cache is taken to answer the reads of a file; that was not measured. With a pool that holds the pages the readers read, the writer makes its commits.
7. **The cells that threads begin at when they look for a pin's cell are 32** (3.4).

## 5. Choices made here that the plan left open

1. **"Unaffected" is read as within the range of the quiet setting from run to run**, for the 99th percentile of the commit of a block, and the time a commit's thread spends off a processor is reported beside it.
2. **A committer still tries the page steps of the values that a private copy held back.** Left to write-back alone, such a value is placed only by a pass that finds its page with no copy, and nothing has a pass come at such a moment while blocks are applied back to back.
3. **The measurements are tests that a normal run leaves out**, in `lignin-testkit`, and print tables.
4. **Both engines are paced by the clock** (PERF-07-F1): the same pause after every commit, whatever the engine, counted from the commit's end, so that what the harness does after a commit is no part of the pace. LigninDB's write-back is woken by each commit, and a report gives the turns of its loop a block, so that "a block a turn" can be read off it and is not taken for granted.
5. **A heavy background for RocksDB is a flush asked from a second thread** (PERF-07-F1), at most once for ten blocks. GroveDB opens RocksDB with fixed options, and the harness changes nothing below GroveDB.
6. **The rule of an experiment is read on medians** (3.5.3): the median of its runs against the median of the control's, and the control's runs apart by their highest less their lowest. With two rounds and two names the control has four runs of LigninDB under a background and an experiment two.
7. **An experiment on the engine is a patch beside the harness** (3.5.3), built by the campaign on the commit it measures, and not a switch in the engine: a change that no table has earned is in no build that is not made to measure it.
