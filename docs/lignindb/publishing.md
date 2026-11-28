# Publishing the LigninDB crates

LigninDB task OPS-06 ([plan](https://github.com/dashpay/grovedb/issues/1052), [task register 3](https://github.com/dashpay/grovedb/issues/1052#issuecomment-6960583786)). Sam (`@QuantumExplorer`), who owns GroveDB's crates on crates.io, publishes. This document says which LigninDB crates are published, when, and what he does, in order.

## What is to be published

Today every LigninDB crate has `publish = false`. The plan, for the first release after the merge (subject to the open decision below):

| Crate | To be published | Why |
| --- | --- | --- |
| `lignin-io` | yes | a dependency of `lignin-format` (for its tests), `lignin-store`, `lignin-tree` and `lignindb` |
| `lignin-format` | yes | a dependency of `grovedb-merk` (`lignin_storage`) |
| `lignin-store` | yes | a dependency of `lignin-tree` and `lignindb` |
| `lignin-tree` | yes | a dependency of `grovedb-merk` (`lignin_storage`) |
| `lignindb` | yes | a dependency of `grovedb-merk` (`lignin_storage`, and a dev-dependency), and of `grovedb` from GROVE-01 |
| `lignin-tools` | when OPS-01 has built the `lignin` command | today it is empty. Its command will be `lignin`, a binary of the package `lignin-tools`: the crate name `lignin` is taken on crates.io. |
| `lignin-testkit` | never | the plan's crate table: workloads, corpus, fuzzer and benchmarks are the project's tools, and it depends on RocksDB |
| `lignin-format/fuzz` | never | not a workspace member, `publish = false` |

The five libraries take the version of the GroveDB crates (7.0.0 today) and move with it, and the crates that name them ask for that version. (Not every crate of a release has it: the imported `grovedb-bincode` and `grovedb-bincode-derive` are at 2.1.0.) On 19 November 2026 none of the seven names was taken on crates.io.

## Why the first release after the merge has to publish them

`grovedb-merk` is published to crates.io (6.1.1 is there). Since MERK-01 it has optional dependencies on `lignindb`, `lignin-format` and `lignin-tree` (the feature `lignin_storage`) and a dev-dependency on `lignindb` with a version. crates.io resolves every dependency a published crate names, optional ones and dev-dependencies with a version included. So once `feat/LigninDB` is merged into `develop`, no release can publish `grovedb-merk` until those crates, and `lignin-store` and `lignin-io` under them, are on crates.io. That holds whatever the default engine is: it does not wait for the cutover (OPS-07).

No release has published `grovedb-commitment-tree`, and none has published `grovedb` since 3.1.0: the commitment tree depends on a git tag of the orchard fork, which crates.io refuses. The release workflow tries the commitment tree second to last, the run stops there, and `grovedb` is not attempted, as today. Nothing about LigninDB changes that.

## The release workflow

`.github/workflows/release-crates.yml` publishes the crates in the order `.github/scripts/release-crates.sh list` gives. The five LigninDB library crates are in that list, between `grovedb-private-document-store` and `grovedb-merk`:

```text
lignin-io, lignin-format, lignin-store, lignin-tree, lignindb, grovedb-merk
```

Before the first upload the workflow runs `release-crates.sh check --release`. It fails, and nothing is uploaded, when a listed crate cannot be published to crates.io (`publish = false`), when a crate that can be published is not listed, or when a listed crate depends on a workspace crate that is not listed before it. Without that step the workflow published the crates one by one and stopped at the first that failed: after the merge, at `grovedb-merk`, with fifteen crates of the release already on crates.io.

CI runs `release-crates.sh check` (without `--release`) in the job Linting, on every pull request that changes code ([`ci.md`](ci.md)), so a new dependency between workspace crates is put in order when it is added, not found during a release.

## What Sam does, in order

For the first GroveDB release whose `develop` has the LigninDB crates:

1. **Decide that the LigninDB crates are published now** (see "Open decision" below). If not, release from a commit before the merge of `feat/LigninDB` instead.
2. **Remove `publish = false`** from the manifests of `lignin-io`, `lignin-format`, `lignin-store`, `lignin-tree` and `lignindb`, in the commit that bumps the version. Leave it on `lignin-tools` and `lignin-testkit`.
3. **Check that the token can create crates.** The workflow publishes with the secret `CARGO_GROVE_TOKEN`. A crates.io token can be limited to updating crates that exist, or to some crate names; the five are new names, so the token needs the scope that publishes new crates and no name pattern that leaves them out.
4. **Check the release before tagging it**, in a checkout of that commit:
   ```sh
   .github/scripts/release-crates.sh check --release
   cargo publish --dry-run -p lignin-io -p lignin-format -p lignin-store -p lignin-tree -p lignindb
   ```
   The first must print that the crates are in an order crates.io can resolve. The second packages the five crates and builds each from its package, resolving the others from the packages it has just made.
5. **Publish the GitHub release** with the tag `v<version>`, as for every release. `release-crates.yml` checks the tag, runs the check of step 4 and publishes the crates in order. A run that stops can be run again: versions already on crates.io are skipped.
6. **Give the new crates their owners.** The first upload makes the account of the token the only owner. Give the five the owners the other GroveDB crates have (on 19 November 2026, `QuantumExplorer` alone), and any team that should be able to publish them: `cargo owner --add <login> lignin-io` and so on.
7. **Look at what was published:** each of the five at the release's version on crates.io, `grovedb-merk` after them, and the documentation on docs.rs.

From then on a release publishes them like any other GroveDB crate. When OPS-01 has built the command, `lignin-tools` follows the same steps 2 to 6 and is added to `release-crates.sh` after `lignindb`.

## Open decision

Publishing the LigninDB crates puts an engine whose on-disk format is not frozen (format version 1 stays open until the cutover, OPS-07) on crates.io, under a feature of Merk that nothing turns on by default. The three ways:

- **Publish them with the first release after the merge** (the plan above). Nothing selects LigninDB before OPS-07; the crates' descriptions should then say that the format is open until the cutover.
- **Release from before the merge** until the cutover. `develop`'s releases then carry no LigninDB code at all, and the first one after the merge is the cutover's.
- **Reserve the names now** with an empty crate at version 0.0.0, so that no one else takes them, and publish the real crates by either of the ways above.

This is the owner's and Sam's to decide; the release workflow supports the first two as it stands.
