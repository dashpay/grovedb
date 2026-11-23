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
| `planted` | Plants each of ten defects and requires the campaign to find it, in a run in which the defect acted: the campaign's own acceptance test. | 0 when every defect was found |

`lignin-ci.sh crash ARGS` is `planted` and then `run ARGS`, and is the nightly job ([`ci.md`](ci.md)). `lignin-crash` with no arguments prints every option.

A run is a function of its seed and of the options that shape it (`--faults`, `--write-backs`, `--ops`, `--lineages`, `--depth`, `--again`), on every machine: the workload, the faults, the crash states and the order they are recovered in. A campaign that found a violation prints the `seed` command that makes its run again. A campaign is a function of its options but for `--duration`. Runs are taken in in the order of their seeds whatever number of them is made at once (`--jobs`), and a campaign that is given `--points` ends with the first run that reaches them. A report has a digest of what was done, which two campaigns that did the same have in common.

The file layer is the simulated one of `lignin_io::sim`, so a campaign needs no disk and no scratch directory. A campaign of *n* crash states holds a hash set of their *n* fingerprints of 8 bytes, and each run a few megabytes beside what its lineages have seen.

## What a run is made of

| | |
| --- | --- |
| The store | `lignin-store` through its public API, with pages of 8 KiB, log files of 1.5 kB, segments of 24 KiB, value-log files of 6 kB, chains of zero to five runs, a table checkpoint that is due every 5 kB of log, and a cleaner that has a file closed at 1 KiB, moves 16 KiB in a pass and leaves none, a tenth or four tenths of the blocks dead, so that every kind of file is turned many times in sixty operations. |
| A lineage | One file system, and sixty operations of a workload on the store in it, over as many sessions as the workload's kills and closes make. |
| A run | Up to five lineages: the first begins with an empty directory, and each of the others with a state that a crash of an earlier one left, up to two crashes deep. |
| The faults of a run | By its seed: none for two seeds in four, failing syncs for one, and for one every kind of failure, torn appends, a write or a removal of a file that fails when the workload says so and a device that is full for a while. |
| A write-back that fails | In the runs of the odd seeds, which are one of the two with no fault and the one with every kind (`--write-backs on` and `off` say otherwise): now and then, after a write to a file, the system's own write of that file fails (`lignin_io::sim`, "A write-back that fails"). The file layer tells the handles that made the changes the write-back was to write, each by its next sync, and no other handle (`Told::Writers`), and reads go on returning what was written. A store that syncs a file through another handle than the one that wrote it can be answered `Ok` there over bytes that storage has lost. A crash state shows it where a manifest or a page has come to rest on those bytes: the two planted defects of a handle are found so. |
| Which handle syncs | In the sessions of every lineage, with or without a write-back that fails, a sync that is asked through a handle of a file to which another handle has written since its last sync is a violation (`docs/lignindb/sync-handles.md`, section 2; `Observer::take_foreign_sync` says what counts as writing). The recoveries of crash states run on the simulator with nothing between, and are not held to this. |
| What a failed sync leaves, and a write-back that failed | By the seed as well: lost, kept, or drawn (`SimConfig::unknown`), by turns. |
| The sector size | 512 bytes for two runs in four, then 4096 and 128, by turns of twelve seeds. |
| A crash image | The file system after one operation that changed what a crash can leave. A lineage has some seven hundred. |
| A crash state | What one crash plan leaves of an image: every plan where there are at most 48. Of an image with more, a choice: the two ends, the plans of a file system that writes in order with the first lost write cut short after each number of its leading sectors, each change lost alone, each change applied late, and three drawn from the seed. After every operation the states of a crash at that moment are gone through once more, and held to what the operation left expected. |
| A recovery | `lignin_store::recovery::recover` on the state, with the buffer pool, the window of the log reader and the number of pages a replay keeps by turns. Then every page, with the commit it was born at and the count of the cells that left it, every value, page id and count is read and held to the model, the engine's physical invariant checker (`lignindb::verify`) is run over the store, and the store's directories hold no file that its manifest does not name. |
| Again | One state in eight is recovered a second time with another tuning, goes on until a table checkpoint covers everything, is closed and opened again. In the runs with faults, one state in four is recovered once more on a copy of its own with the faults injected into the recovery: it returns the same store, or fails and is followed by a recovery with no fault that does. |
| The floor | No state is before the last point at which a sync of the log or a recovery returned `Ok`, nor before the last commit the log had reported durable, nor before the point that the durable part of the same or an earlier image recovered to. |
| A crash that loses nothing | The state with every unsynced change, which is what a killed process leaves absent a failed write-back (`lignin_io::vfs`, "Durability"), is at the last point or at the item that is being appended. Of an image that was taken after a write-back failed, and before a recovery with no write-back failing under it has returned the store again, that state is held to this only where it is what the session saw at that moment (`SimVfs::fingerprint`). Elsewhere it is held to the floor as every other is. |

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

- The engine above the store, with transactions, savepoints, database checkpoints and GroveDB's root hashes: the engine crash campaign below (VER-04), which builds on this one.
- The owners of values that the tree layer has. The cleaner of `lignin-store` is run as two operations of the workload, a pass over the segments and a pass over the value log, and the pass over the value log is given the session's own owners of values, which know the cells and entries the workload wrote. The owners of `lignin-tree`, which find the cell of a value through a tree, are in the engine crash campaign, whose databases run the cleaner on its own thread.
- Write-back on a thread beside a committer: a run is one thread, so that a seed gives one run. The stress tests and loom models of VER-05 are for what two threads do to each other.
- One rule of replay is reached only where a table checkpoint covers less than the log holds of a page that was freed and trimmed away (`page-table.md` K11). A single thread reaches it only after an injected fault has kept a page from being written; the campaign counts how often ("changes dropped for a lost page"), and the scripted tests of `lignin-store`'s recovery hold the rule by hand.

## The engine crash campaign

LigninDB task VER-04. Gate M5 asks that after any crash the database opens and its root hash equals that of some committed block, and that a crash at any point during a wipe leaves a database that opens, either unchanged or empty.

The campaign is `lignin-testkit::crash::engine`, and its module documentation is the contract: what a committed block is and where the campaign takes it from, what a state that a crash left is held to, what a seed gives and what it does not, which faults are planted, and what is not in it. It is the store's campaign one level up: GroveDB on the LigninDB engine (`GroveDb<LigninEngine>` on `lignindb::Db`), opened as a node opens it, with the three threads of each database at work.

### Running it

```text
cargo run --release -p lignin-testkit --bin lignin-crash -- engine run --points 1000000 --jobs 4 --save /tmp/states
cargo run --release -p lignin-testkit --bin lignin-crash -- engine wipe --runs 8 --jobs 4 --save /tmp/states
cargo run --release -p lignin-testkit --bin lignin-crash -- engine planted --jobs 4
```

| Command | What it does | Exit code |
| --- | --- | --- |
| `engine run` | The runs of the seeds from `--seed` on, until `--runs`, `--points` or `--duration` is reached, or a violation is found. | 0 when nothing was found, 1 when a violation was |
| `engine seed` | The run of one seed again: its streams, and not its crash states. | the same |
| `engine wipe` | The same campaign with lineages that wipe their database again and again, each time after a write that leaves something in it, and with no fault. | 0 when nothing was found and, of the crashes during the wipe of a grove that held something, some left the database as it was and some left it empty |
| `engine planted` | Plants each of nine defects and requires the campaign to find it: by the first violation of its campaign that fits the defect (of a kind it makes, in a lineage in which it acted, and as its act there asks), where the run of that violation's seed with no defect planted finds no violation. For a sync that was not made the act asks for a crash state of an image taken after the first such sync, other than the state of that image in which every unsynced change is kept. That a violation fits is evidence that the defect made it, and no proof. The campaign's own acceptance test. | 0 when every defect was found |

`lignin-ci.sh crash-engine ARGS` is `engine planted`, `engine wipe` and then `engine run ARGS`, and is a nightly job ([`ci.md`](ci.md)). `lignin-crash engine` with no arguments prints every option.

**A seed gives the streams of a run, and not its crash states.** The streams of a run are a function of the seed and of the options that shape a run, on every machine. In the first lineage of a run with no faults that finds no violation, the operations, their order and every block they commit follow from the seed, the command and those options alone: the report has a digest of those blocks. A database has three threads beside the one that calls it, and where their changes to the files fall among the caller's is the machine's to say. So two runs of one seed can recover other crash states and count other numbers of them; a fault fails whichever thread makes the next call of its kind, so the operations after it can be others; and a later lineage begins with a state that a crash left. `engine run`, `engine seed` and `engine wipe` therefore report a violation that is found in a crash state with what a second run need not give: what was expected when its image was taken; the thread that made the change; the crash plan; the last steps of the lineage; and, where `--save` names a directory, the directory with the files of its state, which `lignin verify` and the engine open, or the reason why they could not be written. A violation of a session, which is found in a database that was not crashed, has no crash state and no files. `engine planted` prints, for a defect that is found, one line with the kind and the text of the violation that counted, and for one that is not found, a line that says so with a violation of its campaign in full where it has one; it reports in full, and keeps the files of, a run that has a violation with no defect planted.

The file layer is the simulated one of `lignin_io::sim`, so a campaign needs no disk but for the states it saves.

### What a run is made of

| | |
| --- | --- |
| The database | `GroveDb<LigninEngine>`, with pages of 8 KiB (of 32 KiB in one run in eight), a pool of 128 KiB to 2 MiB, log files of 6 or 20 kB, segments of 48 KiB, chains of zero to five runs, a table checkpoint that is due every 12 kB of log, a cleaner for which a file can be closed once it holds 2 KiB, which copies 32 KiB of blocks in a pass, leaves none, a tenth or four tenths of the blocks dead and cleans a value-log file of which a fifth, or any byte, is dead, and three loops that sleep a millisecond when nobody wakes them (write-back longer after turns that failed). One session in four syncs the log with every commit. |
| The workload | The streams of the workload generators (`lignin-testkit::workload`), sixty operations of each, on a grove version drawn from the seed: every element and tree type, batches, deletes, flat drops, blocks that are committed, rolled back and abandoned, savepoints, reads through the block and through a snapshot, and on the grove versions that have them a move in a batch and the conversion of a tree in place, made in the transaction of its block. A conversion that GroveDB commits in a transaction of its own is left out of a stream, since the campaign makes every commit itself. |
| The steps between | A flush, a checkpoint of the database (through GroveDB's `create_checkpoint`, or through the engine's `Db::checkpoint` with write-back catching up before it always or never) and the deletion of the oldest, a wipe, a write with no transaction beside the open block, which the block's commit is then refused for where the two conflict, a pause of a few milliseconds, a close and a kill; in a run with faults, a planted fault. Fewer of them inside a block than between two. |
| A lineage | One database, and sixty steps on it, over as many sessions as its closes, its kills and its faults make. |
| A run | Up to four lineages: the first begins with no database, and each of the others with a state that a crash of an earlier one left, up to two crashes deep. |
| A burst | One to six steps, during which the three threads of the database work beside the workload. Between two bursts every thread that is about to change a file waits, and the crash images of the burst are gone through. |
| The faults of a run | By its seed: none for two seeds in four, failing syncs for one, and for one a fault of every kind: a sync, a write, an operation on a name, or a device that is full for three writes. A fault fails the next call of its kind, whichever of the four threads makes it. |
| A crash image | The file system after one operation, of any of the four threads, that changed what a crash can leave, with the thread that made it. And the file system as it is when the campaign has the answer of a call that changes what is expected of a crash: a commit, a write beside a block or a wipe that published or was refused, a flush, a flush of the pending drops that committed, the creation or the deletion of a checkpoint, an open, a close. The first is held to what was expected while its call ran, the second to what is expected from the answer on. A change that another thread of the database makes between the return of the call and that image is in it: where it made good what the call had answered for and not done, the files as the call left them are held only to what was expected before the answer. |
| A crash state | What one crash plan leaves of an image. The plans are those the store's campaign picks, where they are at most twenty-four. Of more: the state with no unsynced change and the one with every change; the eight plans of a file system that writes in order that are nearest to the image as it stands, which have its newest change lost or cut short; and fourteen of the others, at even distances. An image here can have hundreds of unsynced changes, of four threads, and the store's choice grows with the square of their number (`--plans N` takes another number, and `--plans 0` the store's choice whole). After every burst, and when a session is killed, the states of a crash at that moment are gone through once more, and held to what is expected then. A state that was recovered before in its lineage is not opened again: what it held is held to what is expected then, and it is opened again where the lineage has reached subtrees since. |
| A recovery | The engine's open and GroveDB on it, as a node opens its database. The version it is at is that of a block; the root hash and every record a walk from the root tree reaches are that block's; the engine's physical invariant checker finds it sound, beside the threads of the open database and again after a close and a second open, with the stored hashes of the nodes held to Merk's rules for a check (each made again and compared, but for the value hashes that those rules take as they are stored). In a run on grove versions 1 to 3 the rules name the value hash that the delete of a tree with elements can leave there ([engine.md](engine.md#the-value-hash-that-a-delete-leaves-on-grove-versions-1-to-3)): it is no violation, and the report of the campaign counts how often a check named one ("value hashes a delete left, as the checks named them", which is not printed where it is none). In a run on a later version it is a violation. Inside a flush of the pending drops, which commits more than once, it holds what a copy of the files of the database that is never crashed held at that version of its flush. |
| The checkpoints | In every state, each checkpoint that was made opens as a database of its own, at the version of its block and with that block's root hash and records, and is sound. One whose creation had not returned is whole in that way or not there, one that is being deleted is whole, marked as being removed, or gone, and one whose deletion has returned has no directory. A lineage that begins with a state that a crash left has the checkpoints of that state, and what the crash left of one that it cut short (no directory, one that holds no database, one that is marked) stays what it was. |
| Again | One state in sixteen goes on: it takes a block and a flush with the thresholds of a session, is closed and opened again, and is at the version after the one it recovered to. So does each of its checkpoints, with a block of its own, and each is found as it was recovered when its turn comes: none changes another. |
| The floor | No state is at a block before the last that a flush, the creation of a checkpoint, a commit of a session that syncs with every commit, a close or an open made durable, nor below the version that the durable part of the same or an earlier image of the same file system recovered to, which can be that of the block being made. Inside a flush of the pending drops in a session that syncs with every commit, the state with no unsynced change is one version behind the state with every change at most. |
| A crash that loses nothing | The state with every unsynced change, which is what a killed process leaves, is at the last block or at the one that is being made, and not below the version the database had reached. A session that is killed is followed by an open, which finds the database there. The kill makes nothing durable: the file system stays as the process left it, and a crash before that open has returned may leave any block from the floor on. |
| A close | The drop of GroveDB's handle, as a node closes its database: it answers nothing. A close that a fault can have failed raises no floor. A panic of a background thread of the engine is known by the panic hook of the process and is a violation of the session that was open. |

### The threads, and the cleaner's turns

Every crash image is taken with the thread whose operation made it: the caller's, write-back's, the reclaimer's or the cleaner's. A report counts the write boundaries that each of the four made and the crash states recovered at a change of each, for example "write boundaries made by the cleaner's thread" and "states of a crash at a change of the cleaner's thread", beside what the engine says its background work did ("passes of the cleaner", "blocks moved by the cleaner", "values moved by the cleaner", "segments deleted by the cleaner", "table checkpoints", "dropped subtrees reclaimed"). It counts them by what the workload was doing as well: "states of a crash during the commit of a block", "during a flush", "during the making of a checkpoint", "during a wipe" and so on. The images taken when a call had answered are counted apart from the write boundaries. These counts are of one campaign and are not the same in a second one from the same seeds.

### A database with the mark of a restore that did not finish

No workload of the campaign restores a database by state sync, so no block holds the mark of a restore that did not finish (`GroveDb::has_incomplete_restore`). The mark is part of what a state is compared by: a crash state that held it would be at no block, and is a violation before the checker is asked. The checker is never run over a database that holds the mark, in which it reports value hashes by design.

How the mark is left where a commit fails or a process stops is held on the campaign's databases by the tests of `lignin-testkit/tests/engine_restore_mark.rs`: a later commit of a session that fails, a process that stops during a session, and a commit of a session that is published and answered with an error. A database that is left with the mark says that it holds a restore that did not finish, starts no session, and has nothing for the checker to find but those value hashes. Where the process stops, what the killed process leaves has the mark, and a machine that stops then leaves a database with the mark or the database as it was before the session.

### What it costs

A crash state costs an open of the database, a walk of its grove, the physical checker twice and a second open, and the same for each checkpoint it has. One state in sixteen goes on, which costs a block, a flush and two opens more for the database and for each checkpoint that is whole. A lineage pays this once for each distinct state it recovers, and once more for a state that a later crash leaves again after the lineage has reached more subtrees. A state that was recovered before costs the making of its files from the image, to see that it is the same, and a comparison with what is expected then. A flush of the pending drops costs one open more for each change that a copy of the files makes during it.

### Reading a violation

```text
VIOLATION: seed 1, lineage 0, after step 25, a crash at write boundary 102 of session 1: the recovered state is no committed version: the database opened, and it is at point 3 (version 5): expected is a block from point 4 (version 6) to point 4 (version 6), or the one being made (version 7)
  when the image was taken: points 4 to 4 or the block being made (version 7), during a wipe, at a change by the caller's thread
  the crash kept: CrashPlan { names: [], changes: [Survivor { change: 0, tear: None }] }
  the files of the state: /tmp/states/seed-1-lineage-0-session-1-boundary-102-violation-0
  the last steps: Write(Insert), commit, Batch, flush, wipe
to make the workload of that run again: lignin-crash engine seed --seed 1
```

A point is a block of the lineage, by its place in the model's list, and the version is the engine's. The first line says where in the lineage the crash came and what was found; the second what the model expected when the image was taken, what the workload was doing then, whether the image is of a change or of no change (the moment a call had answered, or the end of a burst), and which thread made the change or the last one before; the third which of the unsynced changes the crash kept, as `lignin_io::sim` names them. The directory is a new one (a name that is taken gets `-2`, `-3` and so on). It holds the files of the state, as the crash left them and before any open, under the paths the campaign uses (`db`, `checkpoints/N`), two names of one file as hard links of one file, and a file `VIOLATION.txt` with these lines but for the last: `lignin verify` and the engine open them. Where the files could not be written, the report says why in their place. The last command makes the streams of the run again, and not its crash states. A violation with no boundary was found in a database that was not crashed. A panic of the engine is a violation of its own kind, and its own message and place are on standard error.

### What the engine campaign leaves to others

The module documentation has the list ("What is not in it"). In short: no state is recovered with faults injected into the recovery; a flush of the pending drops is made only in a session into which no fault was injected, after a kill and an open where one was; a database is closed by the drop of GroveDB's handle, which answers nothing; the streams have no conversion of a tree in a transaction of its own, and on grove versions 1 to 3 none that appends to a tree; no lineage restores by state sync; one thread commits; of the crash plans of an image twenty-four are taken unless a run is told another number; and no durable byte is damaged.
