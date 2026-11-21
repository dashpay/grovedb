# CI for the LigninDB crates and the engine matrix

LigninDB task OPS-06 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 3](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960583786)).

GroveDB's CI runs on pull requests into `develop`, `master` and the `vX.Y-dev` branches, from the workflow files of the pull request's head. This document says which jobs check the LigninDB crates and the two engines, on which platforms, and how to run each by hand. The commands of the LigninDB jobs are those of `.github/scripts/lignin-ci.sh`, which runs from any directory of a checkout; the table gives each job's. The engine matrix is [`test-engines.md`](test-engines.md)'s; the crates a release publishes are [`publishing.md`](publishing.md)'s.

## What a pull request runs

`.github/workflows/grovedb.yml`. The jobs below wait for Detect Changes and run when the pull request changes code (Formatting and Linting on any change of code, the tests unless the change is of doc comments alone): Rust sources, a manifest, the toolchain file, a registry of test engines, the golden corpus, the cost model's directory, the fuzz crate of `lignin-format`, the workflow, its action or its scripts.

| Job | Runner | What it runs | By hand |
| --- | --- | --- | --- |
| Test Ubuntu (1/3 to 3/3) | Linux x86_64 | the workspace but the two imported crates `grovedb-bincode` and `grovedb-bincode-derive` (the job Bincode tests them when they change), with every feature, in three shards, with coverage (`cargo llvm-cov nextest`): every suite on its first engine, the LigninDB crates' tests among them, the golden corpus replayed (`lignin-testkit --test golden`), the differential fuzzer's acceptance test (`--test diff`) and the short form of the store crash campaign with its planted defects (`--test crash`) | `cargo test --workspace --exclude grovedb-bincode --exclude grovedb-bincode-derive --all-features`, which runs the doc tests too |
| Test *package* on *engine* | Linux x86_64 | each suite on each engine of its registry but the first ([`test-engines.md`](test-engines.md)) | `.github/scripts/engine-suites.sh run` |
| LigninDB on Linux aarch64 | `ubuntu-24.04-arm` | the LigninDB crates' tests, then the tests of Merk's node store on LigninDB | `.github/scripts/lignin-ci.sh test` |
| LigninDB on macOS aarch64 | `macos-15` | the same | the same |
| LigninDB loom models | Linux x86_64 | the loom models of the buffer pool (`lignin-store` `cache::tests::races`), the write scopes and the snapshots (`lignindb` `scope::tests::model`, `snapshot::tests::loom`), and the pool and the page ids under the real versions, with a page that is freed while write-back holds it (`lignindb` `models`), at the bounds the models set | `lignin-ci.sh loom` |
| LigninDB Miri and fuzz targets | `ubuntu-latest` | Miri over `lignin-io` under Stacked Borrows, the buffer pool under both borrow models, and the checksum's software path; a build of the fuzz targets of `lignin-format` | `lignin-ci.sh miri`, `lignin-ci.sh fuzz-targets` |
| Cost model replay | Linux x86_64 | the golden corpus replayed on the cost model ([`cost-model.md`](cost-model.md)) | `lignin-ci.sh cost-model` |
| Linting | Linux x86_64 | clippy, every target type-checked, and three steps below | |
| | | the feature sets that must build apart | `lignin-ci.sh features` |
| | | the LigninDB crates' documentation with warnings denied, but `lignin-testkit`'s | `lignin-ci.sh docs` |
| | | the order of the crates a release publishes | `.github/scripts/release-crates.sh check` |

"Linux x86_64" is the organisation's self-hosted runners for a pull request from the repository itself and `ubuntu-latest` for one from a fork (the comment at the top of `grovedb.yml`). A self-hosted runner takes its tools from its image and installs none, so the jobs that install a nightly toolchain (Miri) run on `ubuntu-latest` always.

### Both engines on every pull request

A pull request that changes either engine runs the suites of both. Detect Changes does not look at which crate changed: a change of code other than doc comments runs every job above. The RocksDB side is GroveDB's, Merk's and the storage crate's suites (the shards and the per-engine jobs); the LigninDB side is the LigninDB crates' tests, which include the corpus replayed through the LigninDB node store (`lignin-testkit/tests/merk_store.rs`), on all three platforms. When LigninDB is registered as an engine of GroveDB's and Merk's suites (GROVE-01, MERK-08), the per-engine jobs run those suites on it with no change here.

### The platforms of DEC-06

| Platform | Where the LigninDB crates' tests run |
| --- | --- |
| Linux x86_64 | the three test shards |
| Linux aarch64 | LigninDB on Linux aarch64 |
| macOS aarch64 | LigninDB on macOS aarch64 |

The LigninDB code outside those crates is Merk's node store on LigninDB (`merk/src/store/lignin`, in `grovedb-merk` behind the feature `lignin_storage`). The shards build with every feature and run its tests on Linux x86_64. On the two aarch64 platforms, `lignin-ci.sh test` runs them after the crates' tests, as a command of their own: `cargo test -p grovedb-merk --features lignin_storage --lib store::lignin`. It runs whether or not the crates' tests passed, and fails when its tests do not build or the filter matches no test. Arguments given to `lignin-ci.sh test` (a test name, for example) go to the crates' run only.

The two aarch64 runners take the paths that x86_64 never does: `crc32c` uses the CRC instructions of aarch64 there, and on macOS `lignin-io` runs on its own system calls (the conformance suite of IO-01 had been run on macOS by hand only). The checksum's table-driven path, which neither platform takes, runs under Miri on a big-endian target.

### The feature sets

`lignin-ci.sh features` builds what must build without the rest:

- **GroveDB with no engine** (SEAM-07, `engine.md`, "Features and engines"): no crate of RocksDB and no LigninDB crate among the crates of `grovedb --no-default-features --features estimated_costs`, its normal, build and dev dependencies; no gate in `grovedb` that says `minimal`; `engine_generic` alone type-checks; the tests of GroveDB and Merk that open no database pass.
- **Merk on LigninDB alone**: `grovedb-merk --no-default-features --features lignin_storage` builds with no RocksDB crate.
- **Merk on RocksDB alone**: `--features rocksdb_storage` has no LigninDB crate (D-082: neither engine is in a build that did not ask for it).
- **The LigninDB crates with features** (today `lignindb` with `testing`): each type-checks with none and with all of them.

### Documentation

`lignin-ci.sh docs` builds the documentation of six LigninDB crates with `-D warnings`, which fails on a broken intra-doc link. `lignin-testkit` is not in it: its documentation had two ambiguous links (in `diff`) when the step was added. Nor are the older crates: `grovedb-merk` had 33 errors in older code. A crate is added to the step when its documentation builds clean.

### Loom and Miri

The loom models run on every pull request that runs the tests: with their bounds they take minutes (seven on a loaded strixbox). `loom` is a dependency of `lignin-store` and `lignindb` under `cfg(loom)` only (D-059 item 3); `cargo audit` reads the lock file, which lists it, and passes, so the models stay where they are. The models build in `target/loom`, because the cfg changes every crate they compile.

Miri runs on the toolchain `nightly-2026-02-24` (`$LIGNIN_NIGHTLY`), the newest the project's machines have with Miri, so that a run in CI and one on a machine of the project interpret the same way. The unsafe audit (VER-06) owns what Miri is run over. A pull request runs the pool, which has the unsafe code, under both borrow models, and the file layer, which has none of its own, under Stacked Borrows: one pass over `lignin-io` took 27 minutes on a loaded strixbox. The checksum runs its published vectors and random lengths there. The nightly run adds the file layer under Tree Borrows and the checksum test that walks 128 MiB (`lignin-ci.sh miri --full`), and the races under 64 of Miri's schedules (`lignin-ci.sh miri-schedules 64`). `lignin-ci.sh miri` goes on to its next set after a set that failed, and within a set to the next test target, and fails at the end, so one run shows the failures of every set. A test binary that Miri aborts, as it does where it finds undefined behaviour, does not run the tests that were left in it.

### Test scratch

The tests make their databases in the temporary directory, which `TMPDIR` names. On a self-hosted runner it is the job's own directory, which the runner empties around every job (`.github/actions/persistent-runner`); `/dev/shm` there is small and shared, and no test may count on it (D-114).

### Open files

`lignin-ci.sh test` raises the soft limit of open files to the hard limit, at most 65,536, before it runs the tests, and so do `stress` and `tsan`. macOS starts a process with 256, and the test kit's tests hold more RocksDB files than that at once: without the raise, 32 of them fail on a Mac with "Too many open files". The engine's stress test (`lignindb` `db::tests::stress`) wants more than 256 too: it holds a database and a checkpoint of it open at once, both in small files, and a run that may not open as many files as it wants fails at its start and says so. Someone who runs these tests on a Mac by hand, without the script, raises the soft limit first, within the hard one: `ulimit -Sn 65536`.

### Hosted runners

The jobs on GitHub's hosted runners (`ubuntu-24.04-arm`, `macos-15`) build the LigninDB crates' tests with `CARGO_PROFILE_DEV_DEBUG=line-tables-only`, so a panic still names its file and line.

Why: the arm job died three runs in a row ("The runner has received a shutdown signal", exit code 143) about two minutes into the last phase of the build, while the test executables were linked, with no test run; its log shows no "No space left on device" and no killed compiler. The likely cause is that phase's disk or memory. A reproduction of the job's first command on strixbox (x86_64, four build jobs as on the arm runner, a cap of 16 GiB of memory with no swap, no compiler cache) gave:

| Debug info | Executables (38) | All artifacts | RocksDB's objects, besides | Peak memory, page cache included |
| --- | --- | --- | --- | --- |
| full, Cargo's default | 9.3 GiB | 11.7 GiB | 1.9 GiB | 14.9 GB |
| line tables | 5.3 GiB | 7.2 GiB | 1.9 GiB | 11.0 GB |

The 38 executables are the 35 test binaries and the programs `lignin-diff`, `lignin-bench` and `lignin-golden`. In all, line tables take about a third less disk (9.1 GiB against 13.6) and a quarter less memory. GitHub gives its standard arm runner 4 cores, 16 GB of memory and a 14 GB SSD. The arm build was not measured.

Each of those jobs prints the runner's cores, memory, swap and free disk at its start (`lignin-ci.sh machine`), and again every minute while the tests build and run, so a runner that dies leaves its last sample in the log. On macOS the memory printed is what is installed, and the swap what is in use.

## Nightly

`.github/workflows/lignindb-nightly.yml`, at 02:17 UTC, and by hand from the Actions tab. GitHub runs a scheduled workflow from the default branch only, so these start once the workflow is on `develop`. A pull request that changes the workflow or `lignin-ci.sh` runs every job briefly instead, to show that its commands work.

| Job | Nightly | On such a pull request | By hand |
| --- | --- | --- | --- |
| Differential fuzzer | one hour, RocksDB against the engine that stands in for LigninDB (VER-01, D-113 item 7), from a seed of its own each night (the run's id, which the log names) | two minutes, seed 1 | `lignin-ci.sh diff-fuzz 3600 --seed N` |
| Corpus of the reference scale | the five traces of a million documents | grove version 5 | `lignin-ci.sh corpus-reference` |
| Corpus from its pinned engine | the corpus recorded again from the commit of `develop` its traces name, through `record-at-pin.sh`, and compared with the committed one byte for byte (D-107) | grove version 5 | `lignin-ci.sh corpus-at-pin` |
| Decoder fuzzing | five minutes for each fuzz target of `lignin-format` (FMT-08), from its seeds | 20 seconds each | `lignin-ci.sh fuzz-decoders 300` |
| Miri | the file layer under both borrow models and every checksum test; the pool's races under 64 of Miri's schedules, both borrow models | the races under 4 | `lignin-ci.sh miri --full`, `lignin-ci.sh miri-schedules 64` |
| Store crash campaign | the planted defects the campaign has to find, then five million distinct crash states of the page store, each recovered and held to the model (VER-03, [`crash-tests.md`](crash-tests.md)), from seeds of its own each night (the run's id on, which the log names) | the planted defects, then the runs of seeds 1 to 4 | `lignin-ci.sh crash --points 5000000 --jobs 4 --seed N` |
| Engine schedules | each step of the engine's background work, a reader and an open of a checkpoint, against commits let in at one of its points, each sweep at every point (`lignindb` `db::tests::schedules`, VER-05) | each sweep at 4 points at most, but the one whose points are few, which tries them all | `lignin-ci.sh schedules`, `lignin-ci.sh schedules 4` |
| Engine stress | an hour of readers, writers, write-back, the reclaimer, the cleaner and checkpoints at once (`db::tests::stress`), in an optimised build with the assertions of a debug build | one minute | `lignin-ci.sh stress 3600` |
| ThreadSanitizer | the threaded tests of the buffer pool and of the engine, the schedules among them, and half an hour of the stress test, with the standard library built under the sanitizer | half a minute of the stress test | `lignin-ci.sh tsan 1800` |

The corpus from its pinned engine is what a sync with `develop` runs first (D-107): done every night, a recipe that has rotted is found the day it rots.

The tests of a pull request run the engine's schedules too, as tests of `lignindb`: each sweep at twelve of its points at most, evenly spread (`LIGNIN_SCHEDULE_POINTS=all` runs every point, `=N` at most `N`; one sweep, whose points are few, always tries them all), and the short form of the stress test. The sanitizer's job runs the schedules as tests too, so there each sweep tries the points it tries with the crate's tests, whatever the job of the schedules is given.

## Crash simulation

Crashes are simulated on the file layer of IO-02 (`lignin_io::sim`), which needs no disk.

- **With the crates' tests, on all three platforms:** every state a crash can leave of the log (`lignin-format/tests/wal_crash.rs`), crashes during write-back and recovery at chosen points of scripted sessions (`lignin-store/src/flush/tests/crash.rs`, the recovery tests), and the short form of the store crash campaign (`lignin-testkit/tests/crash.rs`): twelve runs of random workloads with every kind of injected faults, some sixteen thousand crash states, and the eight planted defects, each of which the campaign has to find.
- **Every night:** the campaign itself, `lignin-crash` ([`crash-tests.md`](crash-tests.md)). A run that finds a violation names its seed, and `lignin-crash seed --seed N` makes that run again on any machine.

Gate M2 asks for one million crash points: `lignin-crash run --seed 1 --points 1000000` is that run, on any machine. The nightly run is five times that, from other seeds each night.

## What CI does not run

- The 24 hours of differential fuzzing of gate M5 and the crash campaign of the engine (VER-04): runs of their tasks, on the project's machines.
- The benchmarks: their numbers depend on the machine (`benchmarks.md`).

## Adding to it

- A new LigninDB crate (a workspace member whose name begins with `lignin`) is in `lignin-ci.sh test` and `docs` with no change: the script lists the members.
- A new loom model is in `lignin-ci.sh loom` when its module is: the script names the modules of the models, because under `--cfg loom` the other tests of a library compile too and fail outside a model.
- A new engine of a test suite is one registry line ([`test-engines.md`](test-engines.md)).
