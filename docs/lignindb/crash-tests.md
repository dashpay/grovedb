# Store crash tests

LigninDB task VER-03 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 3](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960583786)). Gate M2 asks that the page store survives one million simulated crash points with zero violations: the recovered state is always some committed version.

The campaign is `lignin-testkit::crash`, and its module documentation is the contract: what is run, what "the recovered state" and "committed" mean, what a violation is, and what is not in it. This page says how to run it and what a run is made of.

## Running it

```text
cargo run --release -p lignin-testkit --bin lignin-crash -- run --points 1000000 --jobs 4
cargo run --release -p lignin-testkit --bin lignin-crash -- seed --seed 4711
cargo run --release -p lignin-testkit --bin lignin-crash -- planted
```

| Command | What it does | Exit code |
| --- | --- | --- |
| `run` | The runs of the seeds from `--seed` on, until `--runs`, `--points` or `--duration` is reached, or a violation is found. | 0 when nothing was found, 1 when a violation was |
| `seed` | The run of one seed again. | the same |
| `planted` | Plants each of eight defects and requires the campaign to find it, in a run in which the defect acted: the campaign's own acceptance test. | 0 when every defect was found |

`lignin-ci.sh crash ARGS` is `planted` and then `run ARGS`, and is the nightly job ([`ci.md`](ci.md)). `lignin-crash` with no arguments prints every option.

A run is a function of its seed and of the options that shape it (`--faults`, `--ops`, `--lineages`, `--depth`, `--again`), on every machine: the workload, the faults, the crash states and the order they are recovered in. A campaign that found a violation prints the `seed` command that makes its run again. A campaign is a function of its options but for `--duration`. Runs are taken in in the order of their seeds whatever number of them is made at once (`--jobs`), and a campaign that is given `--points` ends with the first run that reaches them. A report has a digest of what was done, which two campaigns that did the same have in common.

The file layer is the simulated one of `lignin_io::sim`, so a campaign needs no disk and no scratch directory. A campaign of *n* crash states holds a hash set of their *n* fingerprints of 8 bytes, and each run a few megabytes beside what its lineages have seen.

## What a run is made of

| | |
| --- | --- |
| The store | `lignin-store` through its public API, with pages of 8 KiB, log files of 1.5 kB, segments of 24 KiB, value-log files of 6 kB, chains of zero to five runs, a table checkpoint that is due every 5 kB of log, and a cleaner that has a file closed at 1 KiB, moves 16 KiB in a pass and leaves none, a tenth or four tenths of the blocks dead, so that every kind of file is turned many times in sixty operations. |
| A lineage | One file system, and sixty operations of a workload on the store in it, over as many sessions as the workload's kills and closes make. |
| A run | Up to five lineages: the first begins with an empty directory, and each of the others with a state that a crash of an earlier one left, up to two crashes deep. |
| The faults of a run | By its seed: none for two seeds in four, failing syncs for one, and for one every kind of failure, torn appends, a write or a removal of a file that fails when the workload says so and a device that is full for a while. |
| What a failed sync leaves | By the seed as well: lost, kept, or drawn (`SimConfig::unknown`), by turns. |
| The sector size | 512 bytes for two runs in four, then 4096 and 128, by turns of twelve seeds. |
| A crash image | The file system after one operation that changed what a crash can leave. A lineage has some seven hundred. |
| A crash state | What one crash plan leaves of an image: every plan where there are at most 48. Of an image with more, a choice: the two ends, the plans of a file system that writes in order with the first lost write cut short after each number of its leading sectors, each change lost alone, each change applied late, and three drawn from the seed. After every operation the states of a crash at that moment are gone through once more, and held to what the operation left expected. |
| A recovery | `lignin_store::recovery::recover` on the state, with the buffer pool, the window of the log reader and the number of pages a replay keeps by turns. Then every page, with the commit it was born at and the count of the cells that left it, every value, page id and count is read and held to the model, the engine's physical invariant checker (`lignindb::verify`) is run over the store, and the store's directories hold no file that its manifest does not name. |
| Again | One state in eight is recovered a second time with another tuning, goes on until a table checkpoint covers everything, is closed and opened again. In the runs with faults, one state in four is recovered once more on a copy of its own with the faults injected into the recovery: it returns the same store, or fails and is followed by a recovery with no fault that does. |
| The floor | No state is before the last point at which a sync of the log or a recovery returned `Ok`, nor before the last commit the log had reported durable, nor before the point that the durable part of the same or an earlier image recovered to. |
| A crash that loses nothing | The state with every unsynced change, which is what a killed process leaves, is at the last point or at the item that is being appended. |

## What it costs

On the project's Linux machine (28 threads, shared with other work), four threads recover 2,000 to 11,000 distinct crash states a second between them, by how busy the machine is, and a run of the default shape has forty to sixty thousand: a quarter of a minute to a minute and a half of one thread. A million crash states are some twenty-five runs. The short form that runs with the crates' tests (`lignin-testkit/tests/crash.rs`) is twelve runs of two lineages of ten operations, some sixteen thousand states.

## Reading a violation

```text
VIOLATION: seed 4711, lineage 2, after operation 17, a crash at write boundary 212: the recovered state is no committed version: landed on version 9, below version 11, which was durable
  the crash kept: CrashPlan { names: [], changes: [Survivor { change: 0, tear: None }] }
  the last operations: commit (3 new), checkpoint, hold [page#3], pass, kill, sync log
```

The seed makes the run again, and the report ends with the command for it:

```text
to make that run again: lignin-crash seed --seed 4711
```

The lineage says how many stores of the run came before this one, the operation and the write boundary where in its life the crash came, and the plan which of the unsynced changes the crash kept, as `lignin_io::sim` names them. A violation with no boundary was found in a store that was not crashed. A panic of the store is a violation of its own kind: one over a crash state is reported with its boundary and plan, and one anywhere else in a lineage with the lineage and its last operations. The panic's own message and place are on standard error.

## What the campaign leaves to others

- The engine above the store, with transactions, savepoints, database checkpoints and GroveDB's root hashes: VER-04, which builds on this campaign.
- The owners of values that the tree layer has. The cleaner of `lignin-store` is run as two operations of the workload, a pass over the segments and a pass over the value log, and the pass over the value log is given the session's own owners of values, which know the cells and entries the workload wrote. The owners of `lignin-tree`, which find the cell of a value through a tree, are in the engine's campaign (VER-04).
- Write-back on a thread beside a committer: a run is one thread, so that a seed gives one run. The stress tests and loom models of VER-05 are for what two threads do to each other.
- One rule of replay is reached only where a table checkpoint covers less than the log holds of a page that was freed and trimmed away (`page-table.md` K11). A single thread reaches it only after an injected fault has kept a page from being written; the campaign counts how often ("changes dropped for a lost page"), and the scripted tests of `lignin-store`'s recovery hold the rule by hand.
