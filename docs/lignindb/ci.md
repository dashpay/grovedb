# CI for the LigninDB crates and the engine matrix

LigninDB task OPS-06 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 3](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960583786)).

GroveDB's CI runs on pull requests into `develop`, `master` and the `vX.Y-dev` branches, from the workflow files of the pull request's head. This document says which jobs check the LigninDB crates and the two engines, on which platforms, and how to run each by hand. Every LigninDB job is one command of `.github/scripts/lignin-ci.sh`, which runs from any directory of a checkout. The engine matrix is [`test-engines.md`](test-engines.md)'s; the crates a release publishes are [`publishing.md`](publishing.md)'s.

## What a pull request runs

`.github/workflows/grovedb.yml`. Every job but Formatting waits for Detect Changes and runs when the pull request changes code: Rust sources, a manifest, the toolchain file, a registry of test engines, the golden corpus, the cost model's directory, the fuzz crate of `lignin-format`, the workflow, its action or its scripts. A change of doc comments alone runs no tests.

| Job | Runner | What it runs | By hand |
| --- | --- | --- | --- |
| Test Ubuntu (1/3 to 3/3) | Linux x86_64 | the whole workspace with every feature, in three shards, with coverage: every suite on its first engine, the LigninDB crates' tests among them, the golden corpus replayed (`lignin-testkit --test golden`) and the differential fuzzer's acceptance test (`--test diff`) | `cargo test --workspace --all-features` |
| Test *package* on *engine* | Linux x86_64 | each suite on each engine of its registry but the first ([`test-engines.md`](test-engines.md)) | `.github/scripts/engine-suites.sh run` |
| LigninDB on Linux aarch64 | `ubuntu-24.04-arm` | the LigninDB crates' tests | `.github/scripts/lignin-ci.sh test` |
| LigninDB on macOS aarch64 | `macos-15` | the same | the same |
| LigninDB loom models | Linux x86_64 | every loom model of `lignin-store` and `lignindb`, at the bounds the models set | `lignin-ci.sh loom` |
| LigninDB Miri and fuzz targets | `ubuntu-latest` | Miri over `lignin-io` and the buffer pool, under both borrow models, and the checksum's software path; a build of the fuzz targets of `lignin-format` | `lignin-ci.sh miri`, `lignin-ci.sh fuzz-targets` |
| Cost model replay | Linux x86_64 | the golden corpus replayed on the cost model ([`cost-model.md`](cost-model.md)) | `lignin-ci.sh cost-model` |
| Linting | Linux x86_64 | clippy, every target type-checked, and three steps below | |
| | | the feature sets that must build apart | `lignin-ci.sh features` |
| | | the LigninDB crates' documentation with warnings denied | `lignin-ci.sh docs` |
| | | the order of the crates a release publishes | `.github/scripts/release-crates.sh check` |

"Linux x86_64" is the organisation's self-hosted runners for a pull request from the repository itself and `ubuntu-latest` for one from a fork (the comment at the top of `grovedb.yml`). A self-hosted runner takes its tools from its image and installs none, so the jobs that install a nightly toolchain (Miri) run on `ubuntu-latest` always.

### Both engines on every pull request

A pull request that changes either engine runs the suites of both. Detect Changes does not look at which crate changed: any change of code runs every job above. The RocksDB side is GroveDB's, Merk's and the storage crate's suites (the shards and the per-engine jobs); the LigninDB side is the LigninDB crates' tests, which include the corpus replayed through the LigninDB node store (`lignin-testkit/tests/merk_store.rs`), on all three platforms. When LigninDB is registered as an engine of GroveDB's and Merk's suites (GROVE-01, MERK-08), the per-engine jobs run those suites on it with no change here.

### The platforms of DEC-06

| Platform | Where the LigninDB crates' tests run |
| --- | --- |
| Linux x86_64 | the three test shards |
| Linux aarch64 | LigninDB on Linux aarch64 |
| macOS aarch64 | LigninDB on macOS aarch64 |

The two aarch64 runners take the paths that x86_64 never does: `crc32c` uses the CRC instructions of aarch64 there, and on macOS `lignin-io` runs on its own system calls (the conformance suite of IO-01 had been run on macOS by hand only). The checksum's table-driven path, which neither platform takes, runs under Miri on a big-endian target.

### The feature sets

`lignin-ci.sh features` builds what must build without the rest:

- **GroveDB with no engine** (SEAM-07, `engine.md`, "Features and engines"): no crate of RocksDB and no LigninDB crate among the crates of `grovedb --no-default-features --features estimated_costs`, its normal, build and dev dependencies; no gate in `grovedb` that says `minimal`; `engine_generic` alone type-checks; the tests of GroveDB and Merk that open no database pass.
- **Merk on LigninDB alone**: `grovedb-merk --no-default-features --features lignin_storage` builds with no RocksDB crate.
- **Merk on RocksDB alone**: `--features rocksdb_storage` has no LigninDB crate (D-082: neither engine is in a build that did not ask for it).
- **The LigninDB crates with features** (today `lignindb` with `testing`): each type-checks with none and with all of them.

### Documentation

`lignin-ci.sh docs` builds the documentation of the LigninDB crates with `-D warnings`, which fails on a broken intra-doc link. The older crates are not in it: `grovedb-merk` had 33 errors in older code when the step was added. A crate is added to the step when its documentation builds clean.

### Loom and Miri

The loom models run on every pull request: with their bounds they take minutes. `loom` is a dependency of `lignin-store` and `lignindb` under `cfg(loom)` only (D-059 item 3); `cargo audit` reads the lock file, which lists it, and passes, so the models stay where they are. The models build in `target/loom`, because the cfg changes every crate they compile.

Miri runs on the toolchain `nightly-2026-02-24` (`$LIGNIN_NIGHTLY`), the newest the project's machines have with Miri, so that a run in CI and one on a machine of the project interpret the same way. VER-06's audit (`unsafe-audit.md`) owns what Miri is run over; the job runs the commands of its section 3 that take minutes. The 64 schedules of the races run nightly.

### Test scratch

The tests make their databases in the temporary directory, which `TMPDIR` names. On a self-hosted runner it is the job's own directory, which the runner empties around every job (`.github/actions/persistent-runner`); `/dev/shm` there is small and shared, and no test may count on it (D-114).

## Nightly

`.github/workflows/lignindb-nightly.yml`, at 02:17 UTC, and by hand from the Actions tab. GitHub runs a scheduled workflow from the default branch only, so these start once the workflow is on `develop`. A pull request that changes the workflow or `lignin-ci.sh` runs every job briefly instead, to show that its commands work.

| Job | Nightly | On such a pull request | By hand |
| --- | --- | --- | --- |
| Differential fuzzer | one hour, RocksDB against the engine that stands in for LigninDB (VER-01, D-113 item 7) | two minutes | `lignin-ci.sh diff-fuzz 3600` |
| Corpus of the reference scale | the five traces of a million documents | grove version 5 | `lignin-ci.sh corpus-reference` |
| Corpus from its pinned engine | the corpus recorded again from the commit of `develop` its traces name, through `record-at-pin.sh`, and compared with the committed one byte for byte (D-107) | grove version 5 | `lignin-ci.sh corpus-at-pin` |
| Decoder fuzzing | five minutes for each fuzz target of `lignin-format` (FMT-08), from its seeds | 20 seconds each | `lignin-ci.sh fuzz-decoders 300` |
| Miri schedules | the pool's races under 64 of Miri's schedules, both borrow models | 4 | `lignin-ci.sh miri-schedules 64` |

The corpus from its pinned engine is what a sync with `develop` runs first (D-107): done every night, a recipe that has rotted is found the day it rots.

## What CI does not run

- The 24 hours of differential fuzzing of gate M5 and the one million crash points of M2 and M5 (VER-03, VER-04): runs of their tasks, on the project's machines.
- A crash campaign. The crash simulations that exist are tests on the simulated file layer of IO-02 (`lignin_io::sim`): every state a crash can leave of the log (`lignin-format/tests/wal_crash.rs`), and crashes during write-back and recovery (`lignin-store/src/flush/tests/crash.rs`, the recovery tests). They run with the LigninDB crates' tests on all three platforms, which is their smoke run. VER-03 adds the campaign (`lignin-testkit::crash`); its million crash points are a run of that task, and a shorter run of it belongs in the nightly workflow.
- The benchmarks: their numbers depend on the machine (`benchmarks.md`).

## Adding to it

- A new LigninDB crate (a workspace member whose name begins with `lignin`) is in `lignin-ci.sh test` and `docs` with no change: the script lists the members.
- A new loom model in `lignin-store` or `lignindb` is in `lignin-ci.sh loom`; a model in another crate needs that crate in the script's list.
- A new engine of a test suite is one registry line ([`test-engines.md`](test-engines.md)).
