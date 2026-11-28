# The scoring run

LigninDB task PERF-09 ([plan](https://github.com/dashpay/grovedb/issues/1052)): the plan's prediction table, scored for gate M6. **Every row with a RocksDB value is scored as LigninDB's value over RocksDB's value of the same row, measured in the same run of one binary on the same host, against the plan's target and accept over the plan's RocksDB column.** The plan's absolute values were measured on an Apple M5 Ultra that the project does not have, so no absolute value here is held against them; the absolute values stand beside the ratios, for the host they were measured on. Two rows have no RocksDB value to divide by and are scored by their value: the page reads a transition (RocksDB counts none, and the plan's 8 is an estimate) and the crash recovery (the plan measured none on RocksDB). The row of identical answers is scored by the golden corpus, which compares LigninDB's answers with RocksDB's as recorded. The two rows of iteration two are not scored yet.

The engine scored is that of `feat/LigninDB` at `588668e1f`. This document sits on `006f8ef02`, whose merges since `588668e1f` (the migrator's check of its source, the latency tooling of `lignin-bench`, the documents' tests) change no code of LigninDB's crates, of Merk, of the storage crate or of GroveDB's two engines (`grovedb/src/engine/lignin` and `rocksdb`): in GroveDB they move the look at a directory's entries out of the two-engine type (`grovedb/src/engine/entries.rs`), which a benchmark does not run. `lignin-bench score` (`benchmarks.md`, "Scoring against the plan's table") holds the plan's table and scores a report's rows in the state each row is of; the tables below are its output and the rows of the reports it read.

## 1. The score

"Target" and "accept" are the plan's, as ratios to its RocksDB column where the row is scored as a ratio. "Measured" is the ratio on the host of section 2 that the row's sitting ran on (A or B), with LigninDB's and RocksDB's absolute values beside it. Write rows are of the steady state (section 3), create / update / delete.

| Measure | Target | Accept | Measured | LigninDB / RocksDB | Verdict | Why a miss is a miss |
| --- | --- | --- | --- | --- | --- | --- |
| Bytes logged per transition | 0.100 | 0.134 | 0.173 / 0.155 / 0.110 (B, paced, 162 MiB) | 3,944 / 3,253 / 2,431 B against 22,828 / 21,004 / 22,022 B | missed (accept met in delete) | The commit groups alone, the cells and hashes of the nodes a transition changes, are 3,366 / 2,574 / 2,112 B, and the page steps of the values that regrouping and the cleaner move add 578 / 679 / 320 B, against an accept of 3,056 / 2,812 / 2,949 B (the log's parts: PERF-04) |
| Physical bytes written per transition | 0.089 | 0.143 | 0.649 / 0.740 / 0.662 (B, paced, 162 MiB) | 43.9 / 42.5 / 40.5 KB against 67.6 / 57.5 / 61.2 KB | missed | The commit groups, the new blocks and the new values alone are 12.3 / 10.7 / 9.8 KB, above the accept of 9.7 / 8.2 / 8.7 KB; the segment cleaner's copies are 20.8 / 20.0 / 21.5 KB, about half of the whole, and the table checkpoints 5.3 / 5.2 / 5.9 KB (the parts: PERF-04; the segment cleaner's share: PERF-08) |
| Wall time per transition | 0.168 | 0.268 | 2.07 / 2.10 / 2.11 and 2.06 / 2.06 / 2.09 (C, paced, 256 MiB, twice, every phase of one regime); 2.41 in the one back-to-back phase of one regime (B, delete) | 1,210 / 1,188 / 1,270 µs against 583 / 567 / 603 µs (C, first call) | missed | On the CPU, 452 µs of the 562 µs a transition between the two engines are below the storage seam, where LigninDB's store spends 633 µs and RocksDB's whole store 181 µs (PERF-03) |
| Keyed lookups per transition | 0 | 0 | 0.054 load, 0.051 / 0.019 / 0.018 (A and B) | 3.08 load, 3.10 / 1.10 / 1.10 against 57.0, 60.5 / 58.5 / 62.5 | missed | What is left is a write of a key new to its Merk and the first read in a transaction of the element that hosts a Merk, by call site (PERF-03) |
| Hashes per transition, iteration two | 0.500 | 0.574 | not scored | 159 and 159 `hash_node_calls` | iteration two | |
| Commit of a 20-transition block | 0.2 | 0.5 | 0.475 / 0.469 / 0.418 and 0.480 / 0.467 / 0.420 (C, paced, 256 MiB, twice, every phase of one regime); 0.482 in the one back-to-back phase of one regime (B, delete) | 1.41 / 1.33 / 1.26 ms against 2.98 / 2.83 / 3.01 ms (C, first call) | accept met | On the CPU, about half of LigninDB's commit is the tree-page codec (0.48 of 1.03 ms), then the buffer pool (0.17 ms) and the transaction's seal (0.14 ms) (PERF-03) |
| Write throughput, 8 threads, iteration two | 5× | 3× | not scored | | iteration two | |
| Bulk load of 1M transitions | 0.173 | 0.346 | less than 0.0312 (B) | 16.6 s against more than 533.5 s, GroveDB's build of the same grove on RocksDB in that run (section 6) | target met | |
| Point get | 0.225 | 0.562 | 0.85 / 0.79 warm, 0.72 / 0.74 settled (A, twice) | 28.8 / 26.9 µs warm against 34.0 / 34.1 µs | missed | Above the storage seam a get takes 2.5 µs on LigninDB and 1.8 µs on RocksDB; below it LigninDB takes 14.3 µs against RocksDB's 23.3 µs (0.62), where the accept leaves about 12 µs (PERF-05, settled) |
| Point query with proof | 0.136 | 0.272 | 0.70 / 0.69 warm, 0.56 / 0.60 settled (A, twice) | 161 / 155 µs warm against 229 / 225 µs | missed | Work above the seam takes about 40 µs on either engine, 22% of RocksDB's row, so the accept leaves about 10 µs below the seam, where LigninDB takes 64 µs against RocksDB's 145 µs (PERF-05, settled) |
| 100 references to documents, with proof | 0.357 | 0.594 | 0.77 / 0.80 warm, 0.70 / 0.70 settled (A, twice) | 3.80 / 3.87 ms warm against 4.90 / 4.85 ms | missed | Above the seam about 0.6 ms on either engine; below it LigninDB takes 1.94 ms against RocksDB's 2.98 ms (0.65), where the accept leaves about 1.5 ms (PERF-05, settled) |
| Page reads per transition, state far larger than cache | 2 pages | 3 pages | 2.87 / 3.09 / 2.82 (B, paced, 162 MiB) | | missed in update (accept met in create and delete) | The cache's own row is 2.1; what is above it is the value cleaner's 4 to 7 moves a transition, each of which loads the page that owns the value (PERF-06, PERF-08) |
| Size after 1M transitions, random documents | 0.842 | 0.990 | 0.948 paced (B); 0.899 / 0.959 back to back (A, twice) | 970 MB against 1,023 MB; 928 / 981 MB against 1,032 / 1,023 MB | accept met | About 0.80 GB of the store is live after the load; the rest is the dead share the cleaners are set to hold (PERF-08) |
| Checkpoint | 1.5 | 4.0 | 0.745 / 0.986 flushed (A, twice) | 4.28 / 4.45 ms against 5.75 / 4.52 ms | target met | |
| Reopen | 0.645 | 1.935 | 0.647 / 0.599 flushed (A, twice) | 17.2 / 17.4 ms against 26.7 / 29.1 ms | accept met in one run, target met in the other | |
| Crash recovery, 256 MB of log | 1 s | 5 s | 1.01 / 0.95 / 1.09 s (A) | | accept met (target met in one of three) | |
| Root hash, proof bytes, `OperationCost` | identical | identical | identical | | met | |

## 2. The head, the binary and the hosts

**The binary.** The benchmark, bulk-load and recovery runs of hosts A and B used one build of `lignin-bench` and two test binaries, of the commit `9d60d8f1f` of the branch of this task: the engine at `588668e1f` and this task's harness, which adds the churn, the size and dead shares of each phase and `Db::space` to read them, and the score. Built from nothing (`git archive` of the commit and its lock file, an empty target directory) at one path with rustc 1.99.1, three times on the two hosts, it came out byte for byte the same each time: `lignin-bench` sha256 `77bf94e5fff72f8c8c79b8e086b3a6039504ea78d239fb5e0bd518827d39f643`, the bulk-load test binary `6ce64346…`, `lignin-store`'s test binary `299482c6…`. The path is in the binary: two builds of one tree at two paths differ. The branch's later commits change the score and its tests, the refusal of a churn that would overflow, comments and documents; the score was run again over the reports of that binary. Host C ran the merge `d8957896e` of this task into `feat/LigninDB`, built the same way: `lignin-bench` sha256 `b66eea17d8e9d3084f9d934355ab514581dca34d2b4024004de82b029ff91900`; its engine is the scored engine (the head line above), its harness the branch's last.

**The hosts.** Three DigitalOcean `so-16vcpu-128gb-intel` droplets (16 dedicated vCPUs, 128 GB, local NVMe; ext4 with `discard`, kernel 6.8.0-149, Ubuntu 24.04, no attached volume), each running nothing but the calls of this document, one call at a time unless a table says otherwise. They are of one class and not of one processor:

| | Processor | `fdatasync` of a 4 KiB append, median of 200 |
| --- | --- | --- |
| A | Intel Xeon Gold 5318Y, 2.1 GHz | 0.73 to 1.01 ms |
| B | Intel Xeon Gold 6548N | 0.47 to 0.97 ms |
| C | Intel Xeon Platinum 8358, 2.6 GHz | 0.87 to 1.03 ms |

B takes a little over half of A's time for the same call of RocksDB (a transition of the load 440 against 765 µs, a warm point get 17.7 against 34.0 µs), and the ratios of the two engines differ between the sittings of the two hosts by as much as 0.1 to 0.3 (a settled proof of 100 references 0.70 on A and 0.80 on B; a cold point proof 0.68 and 1.00), with a load back to back on A and paced on B. So every ratio is given with its host, and no ratio of one host is held against one of another.

**The reference state.** The reference workload of `benchmarks.md` (a million documents of 400 bytes over 10,000 owners, a reference each, 20 transitions a block, seed 1). The whole runs of A load it back to back (`pace=free`, as RocksDB's baseline is measured); the steady-state sittings of B and C run over the state that a paced load on the same host left (`--set pace=block`), kept and copied for every call.

## 3. The write rows at steady state

Each call of this section is `lignin-bench writes` over a copy of B's kept state, with RocksDB in the same call but where a table says otherwise: a churn of 100,000 creates (the phase `churn`, given and not scored), then 200,000 creates, 200,000 updates and 200,000 deletes. Paced is one turn of write-back a block (`pace=block`), as where blocks come seconds apart; back to back applies blocks as fast as they come. The pool is 162 MiB (170 MB, the plan's cache) or the default 256 MiB.

**The counts**, four calls at once (so their time rows are not read), create / update / delete:

| Setting | Bytes logged, LigninDB / RocksDB | Physical bytes, LigninDB / RocksDB | Pages read a transition | Values the value cleaner moved a transition | Value log dead, start → end | Segments dead, start → end |
| --- | --- | --- | --- | --- | --- | --- |
| paced, 162 MiB | 3,944 / 3,253 / 2,431 B against 22,828 / 21,004 / 22,022 B | 43.9 / 42.5 / 40.5 KB against 67.6 / 57.5 / 61.2 KB | 2.87 / 3.09 / 2.82 | 4.7 / 7.2 / 4.4 | 17.9 → 17.9 / 17.9 → 17.8 / 17.8 → 17.8% | 16.5 → 16.0 / 16.0 → 16.5 / 16.5 → 16.0% |
| paced, 256 MiB | 3,948 / 3,260 / 2,420 B | 43.5 / 43.1 / 39.9 KB | 2.24 / 2.43 / 2.24 | 4.8 / 7.3 / 4.2 | 17.7 → 17.9 / 17.9 → 17.9 / 17.9 → 18.8% | 16.7 → 16.6 / 16.6 → 15.9 / 15.9 → 16.6% |
| back to back, 162 MiB | 4,650 / 3,286 / 2,589 B | 53.0 / 46.5 / 44.1 KB | 3.92 / 3.38 / 3.74 | 19.0 / 7.8 / 8.3 | 17.6 → 11.0 / 11.0 → 17.3 / 17.3 → 16.9% | 16.9 → 16.2 / 16.2 → 15.3 / 15.3 → 16.3% |
| back to back, 256 MiB | 4,668 / 3,641 / 2,411 B | 54.3 / 46.5 / 42.3 KB | 2.82 / 2.90 / 2.19 | 19.3 / 15.3 / 4.2 | 17.8 → 18.9 / 18.9 → 13.2 / 13.2 → 18.1% | 17.4 → 16.1 / 16.1 → 15.2 / 15.2 → 17.1% |

RocksDB's bytes are the same in every call to within 0.01%: 22,828 / 21,004 / 22,022 B logged and 67.6 / 57.5 / 61.2 KB physical a transition.

**One regime or two.** The value cleaner holds the value log near its target of 18% dead. A phase whose value log ends within a point of where it began is taken as of one regime; one that moves further holds two, the cleaner behind and then catching up, and is not scored. The two ends are what is read, not the share between them. Paced, every phase is of one regime at both pools, and the score of section 1 is of the paced phases at 162 MiB, the plan's cache. Back to back, five of the six phases move more than a point: the value log falls to 11.0% (162 MiB, create) and 13.2% (256 MiB, update), with 15 to 19 values moved a transition, and rises back in the next phase; those phases are given and not scored. The back-to-back phases of one regime (delete at 162 MiB: 2.59 KB logged, 44.1 KB physical, 3.74 pages) score as the paced ones do, but for the pages, which are over the accept.

**The time rows**, one call at a time, back to back (as RocksDB's baseline is measured), create / update / delete:

| Call | LigninDB, wall time a transition | RocksDB | Ratio | LigninDB, commit of a block | RocksDB | Ratio |
| --- | --- | --- | --- | --- | --- | --- |
| 256 MiB, first | 1,147 / 1,120 / 1,189 µs | 480 / 468 / 500 µs | 2.39 / 2.39 / 2.38 | 1.44 / 1.33 / 1.26 ms | 2.62 / 2.45 / 2.63 ms | 0.549 / 0.546 / 0.481 |
| 256 MiB, second | 1,136 / 1,127 / 1,206 µs | 480 / 468 / 500 µs | 2.37 / 2.41 / 2.41 | 1.44 / 1.35 / 1.28 ms | 2.64 / 2.47 / 2.66 ms | 0.544 / 0.545 / 0.482 |
| 162 MiB | 1,164 / 1,141 / 1,220 µs | 481 / 470 / 502 µs | 2.42 / 2.43 / 2.43 | 1.41 / 1.31 / 1.25 ms | 2.66 / 2.50 / 2.68 ms | 0.530 / 0.524 / 0.466 |

The value log's ends in these calls, create / update / delete: 256 MiB first 15.8 → 17.8 / 17.8 → 10.2 / 10.2 → 18.2%, second 11.4 → 17.7 / 17.7 → 19.6 / 19.6 → 19.1%, 162 MiB 17.2 → 8.3 / 8.3 → 20.0 / 20.0 → 17.9%. Only the second call's delete is of one regime: wall time 2.41, commit 0.482. The other phases are given: their wall time is 2.37 to 2.43 and their commit 0.466 to 0.549. The two calls at 256 MiB differ by −1.0 to +1.4% in LigninDB's wall time and by under 0.2% in RocksDB's.

**The time rows, paced**, on C, one call at a time, the default pool of 256 MiB, create / update / delete. Pacing is LigninDB's setting alone: RocksDB takes no setting, and in the same call it applies its blocks back to back, as its baseline is measured.

| Call | LigninDB, wall time a transition | RocksDB | Ratio | LigninDB, commit of a block | RocksDB | Ratio | Value log dead, start → end |
| --- | --- | --- | --- | --- | --- | --- | --- |
| first | 1,210 / 1,188 / 1,270 µs | 583 / 567 / 603 µs | 2.07 / 2.10 / 2.11 | 1.41 / 1.33 / 1.26 ms | 2.98 / 2.83 / 3.01 ms | 0.475 / 0.469 / 0.418 | 18.0 → 17.6 / 17.6 → 17.6 / 17.6 → 17.9% |
| second | 1,211 / 1,184 / 1,276 µs | 587 / 574 / 610 µs | 2.06 / 2.06 / 2.09 | 1.43 / 1.33 / 1.27 ms | 2.98 / 2.85 / 3.03 ms | 0.480 / 0.467 / 0.420 | 17.9 → 17.6 / 17.6 → 17.8 / 17.8 → 17.8% |

Every phase of both calls is of one regime, and the time rows of section 1 are scored on them; the back-to-back phase of one regime on B agrees in its verdicts (wall time 2.41, missed; commit 0.482, accept met). The two calls differ by under 0.8% in LigninDB's wall time and by up to 1.4% in RocksDB's.

**The state far larger than the memory.** Paced at 162 MiB, each engine in a call of its own, under a memory limit of its own (`MemoryMax` of a systemd unit, with the page cache emptied first): LigninDB 233 MiB and RocksDB 687 MiB, the limits of PERF-06's measurement of this workload (each engine's peak memory there, with 32 and 194 MiB more), so RocksDB has about three times the memory for its cache. The two ran at once, beside the control below (a call of LigninDB alone), so their times are of that sitting and their ratios are of one sitting, not of one call; they are given and not scored. Create / update / delete:

| | Wall time a transition | Pages read a transition | The cgroup's read I/Os a transition | Its bytes read a transition | Value log dead, start → end |
| --- | --- | --- | --- | --- | --- |
| LigninDB, 233 MiB | 2,045 / 2,018 / 2,125 µs | 2.87 / 3.09 / 2.83 | 16.5 / 17.2 / 15.9 | 173 / 183 / 165 KB | 18.0 → 18.0 / 18.0 → 17.6 / 17.6 → 17.9% |
| RocksDB, 687 MiB | 1,459 / 1,412 / 1,623 µs | | 17.8 / 16.4 / 19.1 | 73 / 67 / 78 KB | |
| LigninDB, the value cleaner held off (`cleaner_value_target=100`), memory free | 1,155 / 1,115 / 1,206 µs | 2.18 / 2.21 / 2.23 | | | 42.3 → 55.1 / 55.1 → 60.6 / 60.6 → 64.2% |

LigninDB takes 1.40 / 1.43 / 1.31 times RocksDB's time there, with about as many read I/Os and 2.1 to 2.7 times the bytes. With the value cleaner held off the pages read a transition are the cache's own, 2.18 to 2.23, inside the accept, while the value log grows without a bound: what the paced row has above that is the value cleaner's.

**What the size is at steady state.** The size of each engine's database at each phase's end, back to back at 256 MiB (first call): LigninDB 1,266 / 1,191 / 1,080 MB against RocksDB's 1,287 / 1,283 / 1,496 MB, for 1.3, 1.3 and 1.1 million live documents. Paced at 162 MiB: 1,259 / 1,252 / 1,075 MB against 1,287 / 1,283 / 1,225 MB. LigninDB's is the smaller in every phase of every call of this section that measured both engines.

## 4. The reads

`lignin-bench run` on A, twice (`a`, `b`), right after the load (`warm`, the plan's state) and settled (after a flush, a reopen and a wait for the background work), 5,000 point gets, 5,000 point proofs and 500 proofs of 100 references a kind, the median:

| Read | LigninDB, warm a / b | RocksDB | Ratio | LigninDB, settled a / b | RocksDB | Ratio |
| --- | --- | --- | --- | --- | --- | --- |
| point get | 28.8 / 26.9 µs | 34.0 / 34.1 µs | 0.85 / 0.79 | 27.5 / 27.9 µs | 38.5 / 37.6 µs | 0.72 / 0.74 |
| point query with proof | 161 / 155 µs | 229 / 225 µs | 0.70 / 0.69 | 148 / 156 µs | 264 / 262 µs | 0.56 / 0.60 |
| 100 references with proof | 3.80 / 3.87 ms | 4.90 / 4.85 ms | 0.77 / 0.80 | 3.67 / 3.71 ms | 5.24 / 5.26 ms | 0.70 / 0.70 |

`lignin-bench reads` over the kept state of run `a`, settled, two rounds at each pool in turned order:

| Read | 256 MiB, LigninDB / RocksDB, two rounds | ratio | 162 MiB, LigninDB / RocksDB, two rounds | ratio | Pages read a read, 256 / 162 MiB |
| --- | --- | --- | --- | --- | --- |
| point get | 21.6 / 32.9, 21.5 / 33.5 µs | 0.657 / 0.641 | 23.7 / 33.0, 23.4 / 32.9 µs | 0.720 / 0.710 | 0.058 / 0.351 |
| point query with proof | 130 / 238, 130 / 239 µs | 0.544 / 0.544 | 141 / 240, 140 / 241 µs | 0.588 / 0.581 | 0.062 / 0.463 |
| 100 references with proof | 3.33 / 5.01, 3.28 / 5.05 ms | 0.664 / 0.650 | 5.31 / 5.03, 5.02 / 5.11 ms | 1.06 / 0.98 | 5.2 / 37.5 |

So a point proof read 0.46 pages on average over its 5,000 samples at a pool of 162 MiB: one read a point proof, which the page-reads row asks for, holds. Proof sizes are those of both engines alike: 2,227 B a point proof and 52,586 B a proof of 100 references.

Cold, the first read after a reopen with the files dropped from the page cache (40 samples, run `a` / `b`): a point get 0.79 / 0.80 of RocksDB's (1.76 / 1.81 ms against 2.21 / 2.27 ms), a point proof 0.68 / 0.66, a proof of 100 references 0.55 / 0.54. The plan has no cold row.

## 5. The load, the size, the checkpoint and the reopen

From the two whole runs on A (`run` with `--keep --creates 0 --updates 0 --deletes 0 --perf`, back to back) and the paced load on B:

| Row | Run `a` | Run `b` | Paced load, B |
| --- | --- | --- | --- |
| size after the load, flushed | 928 MB against 1,032 MB: 0.899 | 981 MB against 1,023 MB: 0.959 | 970 MB against 1,023 MB: 0.948 |
| checkpoint, flushed | 4.28 ms against 5.75 ms: 0.745 | 4.45 ms against 4.52 ms: 0.986 | 3.74 ms against 3.89 ms: 0.961 |
| checkpoint, right after the load | 4.35 ms against 11.3 ms: 0.384 | 4.45 ms against 8.38 ms: 0.530 | 2.99 ms against 11.5 ms: 0.260 |
| reopen, flushed | 17.2 ms against 26.7 ms: 0.647 | 17.4 ms against 29.1 ms: 0.599 | 13.9 ms against 32.3 ms: 0.432 |
| reopen, cold | 37.2 ms against 50.9 ms: 0.731 | 40.4 ms against 49.8 ms: 0.811 | 39.4 ms against 45.3 ms: 0.870 |

The plan's checkpoint and reopen are of the flushed state, and those rows are scored; the others are given. Both make syncs, whose cost on these hosts the `fdatasync` probe of section 2 samples.

The load itself, through GroveDB's write transactions, a block at a time (not scored: the write rows are of the steady state, and the plan's bulk load is the bulk loader of section 6): LigninDB's wall time a transition 2.42 / 2.41 times RocksDB's back to back (1,850 / 1,830 µs against 765 / 760 µs) and 2.51 times paced on B; its commit of a block 0.556 / 0.562 of RocksDB's; bytes logged 4,650 / 4,545 B against 20,995 B (0.221 / 0.216) and physical bytes 60.7 / 58.9 KB against 49.0 KB (1.24 / 1.20) back to back in runs `a` / `b`, 3,898 B and 43.3 KB against 20,995 B and 49.0 KB paced.

## 6. The bulk load

`the_reference_workload_loads_through_the_engine_within_the_time_and_fill` (`lignin-testkit`'s `tests/bulk_load.rs`, ignored, release build) on B's NVMe: GroveDB builds the reference grove on RocksDB, then the grove's 2,020,007 nodes of 20,007 Merks are handed to the engine's bulk loader as whole Merks and committed. GroveDB's build of the grove on RocksDB had applied 1,000,001 of the stream's 1,050,008 operations (the set-up, 50,000 blocks of 20 creates, and their commits) at about 534 s, the test's last report of it, with 50,007 operations still to run; the load into LigninDB took 16.6 s (13.6 s in the write scopes, 3.0 s in the commits), into 12,535 pages 95.6% full, and passed the physical invariant checker. The ratio is less than 0.0312 of RocksDB's (16.59 s over a build of more than 533.5 s, the progress report rounded to the second; target 0.173, accept 0.346); the test does not report the build's whole time, so the exact ratio is not measured. Reading the grove from RocksDB is the stream's side and is not in the load's time. The bulk loader is a load of a whole state, as a migration makes it, not a load through GroveDB's write transactions, which section 5 gives.

## 7. The crash recovery

`recovery_after_256_mb_of_log_completes_within_5_s` (`lignin-store`, ignored, release build) on A's NVMe: 258 MiB of log after the table checkpoint (271,195,468 bytes, 16,711 commits of 20 transitions, 334,200 page steps), recovered three times, the third with 1,024 pages kept so that 3,940 pages are written to a segment: 1.01 s, 0.95 s and 1.09 s. The accept of 5 s is met; the target of 1 s is met in one of three. The plan measured no RocksDB value.

## 8. The same answers

The golden corpus replayed on LigninDB (`lignin-golden replay --engine lignindb`, built from the same tree on A): 430 traces, 430 as recorded (235,858 steps), which compares every root hash, proof and `OperationCost` with what RocksDB gave when the corpus was recorded. Every report of this document that measured both engines has the same root hash on both after the load (`f635ed0c…`) and at the end, and the same proof sizes where it has proofs.

## 9. The commit's tail

The row of the plan is the commit of a block, which section 3 scores from the harness's own samples (the median of 10,000 blocks a phase). Its percentiles, and what a long commit waits for, are measured by a campaign of LigninDB task PERF-07-F2 on a store of 100,000 documents of 1,000 owners (seed 1, 200 warm and 100,000 measured blocks a run, the two engines in one run): the tables of PERF-07-F2's run of record in `background-and-readers.md`. Its engine is `118a58059`. Its code of LigninDB's crates, of Merk and of the storage crate is that of the scored engine `588668e1f`, and GroveDB's engine module differs from it only by OPS-02-F1's move of the look at a directory's entries into `engine/entries.rs`, which a commit does not reach; the builds of this document add to the scored engine only the reading function `Db::space` (section 2). Its hosts are DigitalOcean `c-16-intel` droplets in `nyc1` (an `fdatasync` median of 0.28 ms on the host of the quiet background and 0.27 ms on that of the heavy one), not the class of this document's hosts, so its times are not held beside section 3's rows: only its two engines are held against each other. Over the four runs of its control (`base` and `base-again`), the 99.9th percentile of the commit is 2,156 to 2,293 µs on LigninDB against 3,954 to 4,164 µs on RocksDB with a quiet background, and 2,270 to 2,538 µs against 17,037 to 18,735 µs with a heavy one; the longest commit is 2,796 to 3,046 µs against 5,619 to 8,585 µs quiet, and 2,995 to 9,840 µs against 36,671 to 52,724 µs heavy; the median is 1,424 to 1,469 µs against 2,582 to 2,611 µs quiet, and 1,292 to 1,442 µs against 1,931 to 2,045 µs heavy. So on that class LigninDB's tail is shorter than RocksDB's at the 99.9th percentile and at the longest commit in every run, with either background. The campaign's first run, on hosts of the same class in `nyc3` and on an earlier engine (`f4eb8bccc`, before PERF-08's changes to the cleaner and the log), answers the same way at the 99.9th percentile: 2,426 to 2,555 µs on LigninDB against 4,157 to 4,273 µs on RocksDB quiet, and 3,122 to 3,229 µs against 24,121 to 25,538 µs heavy.

## 10. What these numbers can tell

- **Two runs of one binary.** On A the two whole runs differ by −6.5 to +8.6% in LigninDB's time rows and by −2.2 to +9.2% in RocksDB's, the checkpoint aside (RocksDB's moved by 21 to 26%); on B the two time calls differ by under 1.5%. These are the spreads that were observed, not a bound: a difference of a ratio within them is no finding.
- **Another build.** The same libraries linked with the text in another order moved LigninDB's wall time a transition by +3.1 to +8.5% and RocksDB's by +4.7 to +8.3% in the write rows (`benchmarks.md`, "What moves a time row between two builds"). The ratios here are of one binary.
- **Two count rows of the load differ between two runs of one binary.** LigninDB's `other_records_per_transition` (0.339504 and 0.339568) and `bytes_handed_per_transition` (3,271.344 and 3,271.377 B) differ by 0.019% and 0.001% between runs `a` and `b`, with equal root hashes; they are compared within 0.1%, and no row of the plan's table rests on them.
- **The system's read counts of a write phase** (`process_read_*`, `device_read*`) include the reads of the closing `Db::space` and of the size of the directory at the phase's end, a few footer reads and directory entries a phase of 100,000 transitions or more; the engine's own counts are read before them. No scored row is one of them.

## 11. How the tables were made

On each host, from `git archive` of `9d60d8f1f` with its lock file, unpacked into an empty directory:

```sh
cargo build --release --locked -p lignin-testkit --bin lignin-bench
cargo test --release --locked -p lignin-store --lib --no-run
cargo test --release --locked -p lignin-testkit --test bulk_load --no-run
```

Host A:

```sh
lignin-bench run --scratch A --engine rocksdb --engine lignindb --keep --creates 0 --updates 0 --deletes 0 --perf
lignin-bench run --scratch B --engine rocksdb --engine lignindb --keep --creates 0 --updates 0 --deletes 0 --perf
lignin-bench reads --scratch A --engine rocksdb --engine lignindb --creates 0 --updates 0 --deletes 0
lignin-bench reads --scratch A --engine rocksdb --engine lignindb --creates 0 --updates 0 --deletes 0 --set cache_mb=162
lignin-bench reads --scratch A --engine lignindb --engine rocksdb --creates 0 --updates 0 --deletes 0 --set cache_mb=162
lignin-bench reads --scratch A --engine lignindb --engine rocksdb --creates 0 --updates 0 --deletes 0
lignin-store-tests --ignored --nocapture --exact recovery::tests::acceptance::recovery_after_256_mb_of_log_completes_within_5_s
cargo run --release --locked -p lignin-testkit --bin lignin-golden -- replay --engine lignindb
```

Host B, `W="--churn 100000 --creates 200000 --updates 200000 --deletes 200000"`, each `writes` over its own copy of the kept state where calls ran at once:

```sh
lignin-bench run --scratch REF --engine rocksdb --engine lignindb --keep --creates 0 --updates 0 --deletes 0 --set pace=block
lignin-bench writes --scratch S1 --engine rocksdb --engine lignindb $W --set pace=block --set cache_mb=162   # these four at once
lignin-bench writes --scratch S2 --engine rocksdb --engine lignindb $W --set pace=block
lignin-bench writes --scratch S3 --engine rocksdb --engine lignindb $W --set cache_mb=162
lignin-bench writes --scratch S4 --engine rocksdb --engine lignindb $W
lignin-bench writes --scratch REF --engine rocksdb --engine lignindb $W                     # twice, alone
lignin-bench writes --scratch REF --engine rocksdb --engine lignindb $W --set cache_mb=162  # alone
systemd-run -p MemoryMax=233M -p MemorySwapMax=0 -p IOAccounting=yes lignin-bench writes --scratch S5 --engine lignindb $W --set pace=block --set cache_mb=162   # these three at once,
systemd-run -p MemoryMax=687M -p MemorySwapMax=0 -p IOAccounting=yes lignin-bench writes --scratch S5 --engine rocksdb $W                                   # the page cache emptied first
lignin-bench writes --scratch S6 --engine lignindb $W --set pace=block --set cache_mb=162 --set cleaner_value_target=100
BULK_LOAD_DIR=DIR bulk_load-tests --ignored --nocapture --exact the_reference_workload_loads_through_the_engine_within_the_time_and_fill
```

Host C, from `git archive` of `d8957896e` with its lock file, the same `W`:

```sh
lignin-bench run --scratch REF --engine rocksdb --engine lignindb --keep --creates 0 --updates 0 --deletes 0 --set pace=block
lignin-bench writes --scratch REF --engine rocksdb --engine lignindb $W --set pace=block   # twice, alone
```

Every `lignin-bench` call also took `--out REPORT.json --label so-16vcpu-128gb-intel --commit COMMIT`. Then `lignin-bench score REPORT...` of the branch's head (its rules of which phases are scored are later than the measured binary's) over the reports, which prints the scored rows and the steady state of each phase.
