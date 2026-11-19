# Golden trace corpus

This directory is the definition of "same answers" for LigninDB
(dashpay/grovedb#1052, task BASE-02). It records what GroveDB on the RocksDB
engine returned, step by step, for every workload on every grove version:

- the `OperationCost` of each operation;
- the root hash after each write, batch and commit;
- the result of each read;
- for each read GroveDB proves, the proof bytes, the cost of proving it and
  whether the proof verifies.

A replay runs the same operations again and compares every one of those
values. The code is `lignin-testkit::golden`; its module documentation
describes the design.

## Pinned engine

Every trace names the commit of the engine it was recorded from, in its
`engine` line: the newest commit that touched a workspace crate outside
LigninDB. The corpus here was recorded from
`2cfc1ce1f8e413cb97952f4c6b81128ae5576e89` (`develop`, "convert a tree with a
64-bit sum to its 128-bit counterpart in place", #1063). The workspace
manifest is not part of that: LigninDB adds its crates to it. `Cargo.lock`
is not committed in this repository, so the commit pins GroveDB's own code
and not the versions of its dependencies. `record` refuses to run when an
engine crate has uncommitted changes.

The pin has moved three times. The corpus was first recorded from
`22f163c2b82d9bc5c985a77034529a32ef9a7c04` (`develop`, "range aggregates and
count offsets on a ProvableCountProvableBigSumTree"). When `develop` was
merged into `feat/LigninDB` up to `f1016919`, it brought #1059, which takes
the referrer list of an element that takes backward references out of every
node hash. That changes the root hashes, costs and proofs of the six traces
that hold such an element: `element-zoo`, `reference-chains` and `suite` on
grove versions 4 and 5. Every other trace, the five of the reference scale
included, came out as it was but for its `engine` line. The other commits of
that range (#1056, #1055, #1061) changed no trace.

When `develop` was merged up to `2a79d035`, every trace of both scales came
out as it was but for its `engine` line: the four commits of that range
reached nothing the corpus held then. Two workloads and eleven hand-written
cases were added for them afterwards, recorded from the same pin:

- #1058 can end the proof of a layer early, on grove version 5, when the
  layer has subqueries below it and the limit of a query node is in force
  there: the limit of its own node, or one it inherits from a node above. It
  does so when the rows below the layer's keys spend the limit before the
  layer's last key. The earlier workloads and cases put such a limit only on
  a node with no subquery below it (`pages`, `identity-keys`,
  `subquery-limit`). `capped-layers` and `legacy-limit-over-subqueries` put
  it on a layer with subqueries.
- #1057 changes an estimate. The corpus records no estimate.
- #1060 refuses, on grove version 5, a tree that one batch creates and fills
  and that the insert alone would refuse. The `legacy-filled-tree-…` cases
  make such batches.
- #1062 adds two indexed trees with a 128-bit sum and the axis they are
  ranked on. `wide-ranks` produces them.

When `develop` was merged up to `2cfc1ce1`, every trace of both scales came
out as it was but for its `engine` line again. That range is one commit,
#1063:

- It adds `GroveDb::convert_to_big_sum_tree`, on grove version 5. No
  workload converts a tree yet.
- It changes what `GroveDb::reconcile_indexed_tree_secondaries` writes on
  grove version 5 when the rankings of the tree are damaged: every row it
  removes leaves storage, a row no tree links is removed or written again,
  and the row of an enclosing indexed tree follows. Grove versions 1 to 4
  keep the repair they had. The streams repair trees whose rankings are in
  step (`indexed-writes`, `wide-ranks`), where both forms of the repair
  find no row to remove or to write.
- A scan that a storage error cuts short is an error, on every grove
  version. No scan of the corpus fails.

One case was added with it, recorded from the same pin: `ranking-drift`,
which holds what the repair is for on grove versions 1 to 4, and what the
repair of those versions leaves in storage (case 13 below).

## Using it

```text
cargo test -p lignin-testkit --test golden                           replay, as CI does
cargo run --release -p lignin-testkit --bin lignin-golden -- replay  the same, as a command
cargo run --release -p lignin-testkit --bin lignin-golden -- record  record the whole corpus again
cargo run --release -p lignin-testkit --bin lignin-golden -- dump --out DIR
cargo run --release -p lignin-testkit --bin lignin-golden -- list
```

`--only NAME` and `--grove-version N` select traces, `--reference` selects
the reference scale, `--scratch DIR` says where the databases are created and
`--jobs N` how many traces run at once. `record` exits with 1 when a
generated workload records an operation that fails or a proof that does not
verify, and `replay` when anything differs.

**When a replay differs** it names the trace, the step and the part of the
step (cost, result, root hash, proof), and prints that step in full as it
came out now. To see what it was, dump the trace from a checkout of the
commit the trace names and from the checkout that differs, and compare the
two files:

```text
lignin-golden dump --only documents --grove-version 3 --out /tmp/now      (this checkout)
lignin-golden dump --only documents --grove-version 3 --out /tmp/pinned   (the pinned commit)
diff /tmp/pinned/v3/documents.trace /tmp/now/v3/documents.trace
```

**When to record again.** `record` regenerates every file for whatever
`Workload::suite` returns, so a workload added to the suite needs nothing
but a new recording. Record again only when the operation streams change or
when a change to an existing grove version's results has been decided; a
replay that differs for any other reason is a bug in the change under test.

**Recording from the pinned engine.** A recording comes from the engine
crates of the checkout it runs in, and `feat/LigninDB` changes those crates.
To add to the corpus without moving the pin, record in a worktree of the
pinned commit, with the workspace manifest and the LigninDB crates taken
from the branch that has the new workloads, and write into that branch's
corpus:

```text
git worktree add --detach <scratch>/pinned 2cfc1ce1
git -C <scratch>/pinned checkout <branch> -- Cargo.toml lignin-format lignin-io \
    lignin-store lignin-tree lignindb lignin-tools lignin-testkit
cp <checkout of the branch>/Cargo.lock <scratch>/pinned/
cd <scratch>/pinned
cargo run --release -p lignin-testkit --bin lignin-golden -- record \
    --corpus <checkout of the branch>/lignin-testkit/corpus
```

`2cfc1ce1` is a commit of `develop`, so it has no LigninDB crate: the
checkout brings the branch's, with the manifest that lists them, and the
copied `Cargo.lock` gives the build the branch's versions of the
dependencies. None of these is an engine crate, so the traces come out
naming `2cfc1ce1`. A trace whose operations and recorded values did not change
comes out byte for byte as it was, so `git status` shows what the new
workloads added. Then replay on the branch itself: what was recorded from
the pinned engine has to come out the same there.

## What is recorded

For each grove version (`v1` to `v5`):

| Trace | Database | Seed | Steps written out in full |
| --- | --- | --- | --- |
| one per workload of `Workload::suite` (`documents`, `balances`, …) | its own | `0x1` | the first of each kind of operation; every step of `tree-deletes` |
| `suite`: every workload of the suite, one after another | one for all | `0x4c49474e494e` | the first of each kind of operation |
| `legacy-…`: one per hand-written case, see below | its own | `0x0` (no generator is used) | all |

A workload is generated for the features of the grove version
(`Features::for_grove_version`), so the same seed gives another stream on
another grove version. Each segment of a trace closes with the number of
operations and the digest of its stream, the one BASE-01's tests pin.

What a step holds depends on its operation:

| Operation | Result | Root hash | Proof |
| --- | --- | --- | --- |
| a write, a batch | none | after it, inside the open transaction | |
| a batch in two segments (`PartialBatch`) | the cost GroveDB handed its caller when the first segment paused | after it | |
| a batch with Drive's storage-flag callbacks (`FlagsBatch`), one that updates a document (`FlagsHandoverBatch`), and one in two segments (`FlagsPartialBatch`) | none, and the cost at the pause | after it | |
| a delete with Drive's callback for freed bytes (`SectionalDelete`, `SectionalDeleteUpTree`) | none, and how many keys it deleted | after it | |
| `Commit` | none; the cost is that of committing the transaction | of the committed state | |
| `FlushPendingDrops` | the records reclaimed, skipped as live again, and left unresolved | after it | |
| `Get` | the element | | |
| `Query` | the rows | | always |
| `PathQuery` (`GroveDb::run_path_query`) | the answer in the form of its shape: rows and how many the offset skipped, a count, a sum, both, one per key, axis entries or keys, a rank, an aggregate over an axis, a sum-budget window | | when the stream marks the read as proven |
| `Rows` (`GroveDb::query`, `GroveDb::query_raw`) | the rows and how many the offset skipped | | |
| `SumBudget`, `GetAux`, `AuxByPrefix` | the sums and where the walk stopped; the record; the records | | |
| an append through a typed method (`Append` but for a commitment, `CommitmentInsert`) | the root of the tree and the position of the value | after it | |
| a conditional write (`InsertIfNotExists`, `InsertIfChanged`, `DeleteIfEmptyTree`, `DeleteUpTree`, `DeleteBuilt`, `IndexedDelete`) | whether it wrote, the element it found or replaced, or how many keys it deleted | after it | |
| a step on the transaction (`Rollback`, `Abandon`, `SetSavepoint`, `RollbackToSavepoint`, `OpenSnapshot`, `CloseSnapshot`) | none | after it, inside the open transaction | |
| a read inside the block or through the snapshot (`BlockRead`, `SnapshotRead`) | the result of the read | inside the open transaction, which the read leaves as it is | |
| `HasRaw`, `IsEmptyTree`, `Fetch`, `Tree` | a flag, the element if there is one, the value, page, count or hash | | |
| `KeysOptional` | a row for each key asked for, empty where nothing is stored | | always, verified with `GroveDb::verify_query_with_absence_proof` |
| `ItemValues`, `SumValues`, `Many` | the values or sums and how many the offset skipped; the rows | | |
| `BigSum` | the sum and the count if asked for, for the range or for each key | | always, verified with the verifier of its form |
| `Prove` (`GroveDb::get_proved_path_query`, `prove_query_many`, `prove_bulk_position_range`, `prove_trunk_chunk`, `prove_branch_chunk`, `fetch_chunk`) | the proof or chunk bytes | | |

What Drive's callbacks do shows in two places. The flags they store are
part of the element, so the root hash of the step commits to them and the
reads after the block return them. The bytes an operation frees are in its
cost, named by owner and epoch: `removed={owner/epoch=bytes,…}`, where an
operation under GroveDB's own callbacks has one number. Of each kind of
operation, the first step that names one epoch and the first that names
several epochs of one owner are written out in full as well.

A `FlagsHandoverBatch` runs the callback Drive applies a batch that updates
a document with, from protocol version 15 on: an element written under
another owner goes to that owner with its history, and whole where one of
the two owners is in the batch's list of owners who pay for all of an
element (a gas sponsor). GroveDB takes that second answer from a callback
on grove version 5 only, so the streams of the earlier versions hold no
such handover. There the old element is freed like a deleted one, key
included, and the new one is added whole.

`tree-deletes` is written out in full because its steps are read next to
each other. Each round deletes, in one block and in each of four ways,
three trees that hold the same number of items: `fresh-…`, created and
filled in that block, `rewritten-…`, committed in the block before and
written into in this one, and `untouched-…`, committed in the block before.
GroveDB charges what a deleted tree held by the records the committed state
holds under its keys, not by what the transaction sees. So in rounds 0 and
2, where the block replaced two values of the rewritten tree and nothing
else, its delete frees exactly the bytes the delete of its untouched twin
frees, although one value is a hundred bytes longer by then and another
twenty shorter, which shows in the bytes it loads. The delete of the fresh
tree frees 38 bytes for each of the keys the tree held, the stored key and
the length of a record that is not there, and loads less.
`the_charge_for_a_deleted_tree_is_in_the_corpus` checks the files for
this.

`wide-sums` keeps two ledgers in the trees whose total is a 128-bit sum,
which grove version 5 adds: a `CountBigSumTree`, with a count and a sum
next to its root key, and a `ProvableBigSumTree`, whose sum is in every
node hash. Grove versions 1 to 4 refuse to create either, so there the
same ledgers are a `CountSumTree` and a `ProvableSumTree` and the amounts
are small: the five traces of the workload hold one shape with a narrow
and with a wide total. The totals are in the elements the reads return and
in the sum of a range of keys of the second ledger, which is a `BigSum`
step on grove version 5 and an aggregate `PathQuery` before. No other
workload holds the two trees: the streams of `element-zoo` were pinned
before GroveDB had them.

`wide-ranks` keeps two indexed trees that rank their entries by a sum,
with a 128-bit sum where the grove version has them: a
`ProvableBigSumIndexedTree`, and a `ProvableCountProvableBigSumIndexedTree`
that also ranks by count and by average. An entry is a sum item or a tree
that keeps the sum of what is put into it, so an entry can rank at a value
no `i64` holds. The entries are written through the four methods GroveDB
has for the two trees (`IndexedInsert` and `IndexedDelete` steps) and
through batches, and the rankings are read as `PathQuery` steps on the big
sum axis: pages, the entries in a band of values, the rank of a key, how
many entries a band holds, and one page for each of several trees. Grove
versions 1 to 4 refuse to create the two trees, so there the same shapes
are a `ProvableSumIndexedTree` and a `ProvableCountProvableSumIndexedTree`
with small sums, ranked on the sum axis, which grove version 4 reads and
grove versions 1 to 3 do not. On those four versions a batch that writes
entries of an indexed tree can leave its rankings out of step with them:
it reads an entry from storage while its own writes are pending
(`apply_batch.indexed_mirror_entry_read`). So there every entry is written
through the methods of its tree and the rankings are rebuilt
(`ReconcileIndexed`) before they are read.

`capped-layers` reads pages of an index whose keys each hold a few
documents, with the limit of the page on the query node that walks the
keys (`Query::limit`): a limit in force on a layer that has subqueries,
alone, next to a limit of the whole query, inherited by the layer below,
and on one branch of a merged query. Grove version 4 proves the layer of
the index with every key in range, and grove version 5 ends it after the
key whose documents filled the page, so the same steps have other proofs
on the two versions and the same rows. Grove versions 1 to 3 have no limit
of a query node. There the same layers are read under a limit of the whole
query, which those versions charge for a key without documents in a way of
their own: the walk below a key starts with what is left of the limit, and
the layer above is then charged for the rows that came back, or for one
row if none did.

A stream marks a path query as proven when GroveDB proves it on the grove
version the stream was generated for. Such a proof is verified with
`GroveDb::verify_path_query`, the proof of a `Query` with
`GroveDb::verify_query` or `GroveDb::verify_query_raw`.
`every_proven_read_is_recorded_with_its_proof` checks the committed files
against the streams: a step has a proof exactly when its read is one of
these.

The standard scale is 380 traces and 229,526 steps in 5.8 MB: for each
grove version the 31 workloads of the suite, `suite` and 44 hand-written
cases. On the machine it was recorded on (28 cores, shared), recording it
takes under a minute in a release build and the replay test about a minute
in a debug build, eight traces at a time.

A consumer that computes with the steps, as a cost model checked against
every recorded `OperationCost` does, takes them as values from
`golden::record::each_step` over the traces of `golden::plan::plan`.

### What the corpus does not see

The corpus records what `workload::apply` gets back from GroveDB, so it is
blind where `apply` or GroveDB's public interface is:

- `GroveDb::clear_subtree` returns no cost, so a `ClearSubtree` step records
  a cost of zero. Its root hash is recorded.
- A `Prove` read returns the bytes of its proof or chunk as its result.
  They are compared as the result; no verification of them is recorded.
- `GroveDb::fetch_chunk` returns no cost. A chunk step records the cost of
  the reads that name the chunk: the root hash, the root key or the element
  of the tree.
- A step on the transaction returns nothing. What a rollback restored
  shows in the root hash of its step and in the reads after it.
- An error is recorded by the name of its variant, and for a variant that
  wraps the error of a lower layer, that error's variant as well
  (`MerkError::…`). Its message is written next to it as a remark and is not
  compared.
- A proof is the one `GroveDb::prove_query` builds with no options, from
  committed state, for the queries the workloads make. Verification is
  recorded as its outcome (verified, refused, or verified to another root),
  not as the rows the verifier returns.
- `FlushPendingDrops` records the three counts of its report. The text of
  the first unresolved error is a remark and is not compared.

## File format

A trace file is text. After the header (`name`, `seed`, `grove-version`,
`engine`), each `workload` line opens the steps of one operation stream and
a `stream` line closes them.

- Every step has one short line, such as `c8a41v62cfq5fa2p77c2`: a letter for
  each part of the step and the first 16 bits of the BLAKE3 digest of that
  part. `c` is the cost, `v` the result, `e` the error of a failed
  operation, `r` the root hash, `q` the cost of proving and `p` the proof
  with what the verifier made of it. Sixteen bits are enough to say which
  part of which step differs.
- A step written out in full follows its line as an indented block: its
  number in the trace, the operation and any error message as `#` remarks
  (not compared), then every value. A result that is neither one element
  nor plain rows is a `result <kind>` line and a line for each of its
  fields; a list is its length and an `item` line for each entry, bytes
  written as `0x` and hex.
- `tally` counts the operations that failed, the queries that could not be
  proved and the proofs that do not verify. All three are zero for every
  generated workload.
- `end` carries the number of steps and one 256-bit digest over every byte
  of every step. That digest, not the short lines, decides whether a replay
  is the same.

A trace too long for a line per step (the reference scale) has `@steps
digest` checkpoints in their place.

## Older grove versions: the hand-written cases

BASE-01's generators steer around thirteen behaviours of older grove versions
so that every stream applies and every read marked as proven proves. A node
on such a version may have done exactly these things, so the corpus records
them, as the engine returns them, on every grove version. Each case is a
few operations on an empty database (`lignin-testkit::golden::legacy`),
recorded in full. The numbers are the tally of the trace: operations that
failed / reads that could not be proved / proofs that do not verify. A case
the tables do not give numbers for records none of the three.

**1. `GroveDb::delete` of a populated tree when the tree or its parent
commits an aggregate into its node hashes.** Grove versions 1 to 3 reopen
the parent as a tree of the child's type. Each case reads the parent before
the delete, after it, and after the next write to the parent.

| Case | v1, v2 | v3 | v4, v5 |
| --- | --- | --- | --- |
| `delete-tree-under-provable-count-tree` | 0 / 0 / 1 | 0 / 0 / 2 | 0 / 0 / 0 |
| `delete-sum-tree-under-provable-count-sum-tree` | 0 / 0 / 1 | 0 / 0 / 2 | 0 / 0 / 0 |
| `delete-count-tree-under-provable-sum-tree` | 0 / 0 / 1 | 0 / 0 / 2 | 0 / 0 / 0 |
| `delete-mmr-tree-under-provable-count-tree` | 0 / 0 / 1 | 0 / 0 / 2 | 0 / 0 / 0 |
| `delete-tree-under-provable-count-provable-sum-tree` | 0 / 3 / 0 | 0 / 0 / 2 | 0 / 0 / 0 |
| `delete-provable-count-provable-sum-tree-under-provable-count-provable-sum-tree` | 0 / 3 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `delete-provable-count-provable-sum-indexed-tree-under-provable-count-provable-sum-tree` | 0 / 3 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |

Recording none of the three: `delete-provable-count-tree-under-provable-count-tree`,
`delete-provable-count-indexed-tree-under-provable-count-tree`,
`delete-provable-count-sum-tree-under-provable-count-sum-tree`,
`delete-provable-sum-tree-under-provable-sum-tree`,
`delete-provable-sum-indexed-tree-under-provable-sum-tree`,
`delete-only-provable-count-tree-under-tree`,
`delete-only-provable-sum-indexed-tree-under-tree` and
`delete-only-tree-under-provable-count-tree`.

In the first five the delete commits a link hash for the parent that leaves
its aggregate out, so the root hash after the delete differs from the one
grove versions 4 and 5 give. The proof of a query in the parent is then
refused by the verifier. On grove version 3 the proof of the parent's own
element is refused as well. The next write to the parent recomputes the
hash and the proofs verify again. In the others the reopened parent hashes
as it should: the child is of a type that hashes what the parent's does
(all seven such pairs are cases), or the parent is left empty. What differs
between grove versions there is the cost of the delete. The two under a
`ProvableCountProvableSumTree` cannot be proved on grove versions 1 and 2
for the reason of the next group.

**2. A query hosted by a `ProvableCountProvableSumTree` or an indexed
tree.** Grove versions 1 and 2 refuse to prove it (`NotSupported`): it needs
the proof envelope of grove version 3.

| Case | v1, v2 | v3, v4, v5 |
| --- | --- | --- |
| `query-in-provable-count-provable-sum-tree` | 0 / 3 / 0 | 0 / 0 / 0 |
| `query-in-provable-sum-indexed-tree` | 0 / 3 / 0 | 0 / 0 / 0 |
| `query-in-provable-count-indexed-tree` | 0 / 3 / 0 | 0 / 0 / 0 |
| `query-in-provable-count-provable-sum-indexed-tree` | 0 / 3 / 0 | 0 / 0 / 0 |

**3. A reference stored in a `ProvableCountSumTree`.** On grove versions 1
and 2 its proof does not verify (`InvalidProof`).

| Case | v1, v2 | v3, v4, v5 |
| --- | --- | --- |
| `reference-in-provable-count-sum-tree` | 0 / 0 / 4 | 0 / 0 / 0 |

**4 to 9. Operations and forms of query an older grove version lacks.** In
these cases every path query asks for a proof, where a generated stream
marks as not proven the ones its grove version refuses to prove.

| Case | What it asks for | v1, v2 | v3 | v4 | v5 |
| --- | --- | --- | --- | --- | --- |
| `move-in-batch` | 4. a batch `Move` of an item, then of a tree | 2 / 0 / 0 | 2 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `drop-flat` | 5. `GroveDb::drop_flat_subtree`, then the flush | 1 / 0 / 0 | 1 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `drop-flat-in-batch` | 5. a batch `DeleteTree` that drops flat, then the flush | 1 / 0 / 0 | 1 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `aggregate-on-range` | 6. a count, a sum, and both over a range of keys | 0 / 3 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `aggregate-per-key` | 6. a count for each of three keys, one of them absent | 0 / 1 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `count-offset-page` | 7. two pages of a `ProvableCountTree`, skipped to by count | 0 / 2 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `axis-read` | 8. six reads of an indexed tree in the order of its counts | 6 / 6 / 0 | 6 / 6 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `sum-budget-window` | 8. a walk over sums up to a total, as a read mode | 2 / 2 / 0 | 2 / 2 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `subquery-limit` | 9. a limit of its own for each subquery | 1 / 1 / 0 | 1 / 1 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `subquery-offset` | 9. an offset of its own for each subquery | 1 / 1 / 0 | 1 / 1 / 0 | 1 / 1 / 0 | 0 / 0 / 0 |

- A refused `Move` and a refused flat drop fail with
  `VersionError::UnknownVersionMismatch` and leave the root hash as it was.
  The flush that follows a refused drop reclaims nothing.
- Grove versions 1 and 2 answer an aggregate and a count-offset page and
  refuse only the proof (`NotSupported` for the aggregate, `InvalidQuery`
  for the page).
- A read mode and a limit or offset of a subquery's own are refused both
  as a read and as a proof (`NotSupported`). `sum-budget-window` also walks
  the same sums through `GroveDb::query_aggregate_sums_with_options`, which
  every grove version serves.

**What every grove version refuses.** Two more cases hold what the
generators never ask for on any grove version, so that another engine has
the refusals to compare with:

| Case | What it asks for | v1 to v5 |
| --- | --- | --- |
| `offset-without-count` | a page of a plain tree, and an offset next to a subquery | 1 / 2 / 0 |
| `aggregate-per-key-over-every-key` | a count for each key of a full range | 1 / 1 / 0 |

`GroveDb::run_path_query` answers the page of a plain tree and no proof is
built for it: the rows an offset skips there are read, not counted. It
refuses an offset next to a subquery (`InvalidQuery`), which
`GroveDb::query` and `GroveDb::query_raw` take; the case records all three.
What those two return is not the same on every grove version: for a limit
of three after an offset of two, over groups of two, three and four rows,
grove versions 1 to 3 return two rows and grove versions 4 and 5 three.
A count for each key is refused when the keys are given as the full range
(`InvalidQuery`).

**10. A limit of a query node on a layer that has subqueries** (GroveDB
#1058), and what the pages of `capped-layers` steer around.

| Case | What it asks for | v1, v2 | v3 | v4 | v5 |
| --- | --- | --- | --- | --- | --- |
| `limit-over-subqueries` | five pages of an index: from either end, next to a limit of the whole query, under a node above that has the limit, and merged with a query for another tree | 5 / 5 / 0 | 5 / 5 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `capped-tree-rows` | trees as the rows of a page, the first of them empty, without a limit and with one | 1 / 1 / 0 | 1 / 1 / 0 | 0 / 0 / 1 | 0 / 0 / 0 |
| `limit-over-empty-subtrees` | two layers of keys above the documents under a limit of the whole query, of four rows and of six | 0 / 0 / 2 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `branch-without-subquery` | a node with a subquery for its keys and a branch without one for some | 0 / 1 / 0 | 0 / 1 / 0 | 0 / 1 / 0 | 0 / 0 / 0 |

- Grove versions 1 to 3 refuse a limit of a query node (`NotSupported`).
  Grove version 4 answers the five pages and proves the layer of the index
  with every key in range: 500, 500, 389, 500 and 587 bytes. Grove version 5
  returns the same rows and ends the layer after the key whose documents
  filled the page: 431, 389, 389, 431 and 518 bytes. The third page is
  ended by the limit of the whole query on both versions.
  `the_layer_a_limit_ends_early_is_in_the_corpus` checks the files for
  this.
- A tree that holds nothing and is a row of a page is counted by the
  prover of grove version 4 and not by its verifier, so the proof of the
  page does not verify (`InvalidProof`). Grove version 5 counts it on both
  sides.
- Grove versions 1 to 3 charge a key without documents to a limit of the
  whole query in one way when they read and in another when they prove. On
  grove versions 1 and 2 the two proofs of `limit-over-empty-subtrees` do
  not verify. On grove version 3 they verify, the one for six rows to
  other rows than the read returns. The trace does not show that: it
  records that a proof verifies, not what it proves.
  `a_limit_over_keys_without_documents_is_proven_as_it_is_read_from_grove_version_4`
  in `tests/upstream.rs` compares the two.
- A branch with neither a path nor a subquery, under a node that has a
  subquery, makes the tree at its keys a row. `GroveDb::run_path_query`
  answers it on every grove version, and the prover refuses it up to grove
  version 4 (`CorruptedPath`).

**11. A tree that one batch creates and fills** (GroveDB #1060). Each case
makes the insert alone in a batch, then the insert at another key together
with a write into the new tree.

| Case | The tree and where it is inserted | v1, v2 | v3, v4 | v5 |
| --- | --- | --- | --- | --- |
| `filled-tree` | a sum tree under a tree | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `filled-tree-claims-a-sum` | an indexed tree that claims a sum of 7 | 1 / 1 / 0 | 1 / 0 / 0 | 2 / 0 / 0 |
| `filled-tree-axes-out-of-order` | an indexed tree whose axes are sum, then count | 1 / 1 / 0 | 1 / 0 / 0 | 2 / 0 / 0 |
| `filled-tree-wrapped` | a sum tree wrapped as not counted, under a sum tree | 1 / 0 / 0 | 1 / 0 / 0 | 2 / 0 / 0 |
| `filled-tree-entry-of-indexed-tree` | a plain tree as an entry of an indexed tree that ranks by sum | 1 / 1 / 0 | 1 / 0 / 0 | 2 / 0 / 0 |

Every grove version refuses the insert alone in the last four
(`InvalidBatchOperation`, and `InvalidInput` for the entry of an indexed
tree). Grove versions 1 to 4 take the same insert when the batch also
writes into the tree: the insert is replaced by the state the level below
computed, and the element is never checked. They store what the batch
computed, so the claimed sum is gone and the axes stay in the order given.
Grove version 5 refuses the batch with the error of the insert alone. Each
case ends on a proven read of the filled tree, which grove versions 1 and 2
refuse to prove where the tree or its parent is an indexed tree (group 2).

**12. Two behaviours of the storage-flag callbacks.**

| Case | What it does | v1, v2, v3 | v4 | v5 |
| --- | --- | --- | --- | --- |
| `flags-of-another-length` | stores flags of another length than the stored ones without the callback changing them: no flags over flags of two epochs in a batch, and flags of one epoch over flags of two in a partial batch that keeps the size | 2 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| `full-handover` | hands what a gas sponsor paid for to its owner, which the callback answers by accounting the update as a replacement | 1 / 0 / 0 | 1 / 0 / 0 | 0 / 0 / 0 |

Grove versions 1 to 3 charge the first kind of update for the length of
the stored flags, and the batch fails with a storage cost mismatch
(`StorageError::CostError`). Grove versions 1 to 4 do not take the answer of
the second from a callback and refuse the batch (`CorruptedData`).

**13. A ranking left out of step with its tree.**

| Case | What it does | v1, v2, v3 | v4 | v5 |
| --- | --- | --- | --- | --- |
| `ranking-drift` | removes entries from three trees ranked by count, through the method of the tree and in a batch, reads each ranking, repairs the tree and reads the ranking again | 7 / 7 / 0 | 5 / 3 / 0 | 0 / 0 / 0 |

To maintain a ranking, grove versions 1 to 4 read an entry back by key
after the write (`apply_batch.indexed_mirror_entry_read` 0). Where that
read meets a link the write did not load, it takes the record from storage,
and the record of a removed entry is still there while the removal is
pending. So the removal is taken for no change, and the row of the entry
stays in the ranking. Whether it happens depends on the shape of the tree
around the removed entry.

**One row.** The case builds the smallest tree in which it happens, five
entries with the removed one at the root, twice: one tree loses the entry
through `GroveDb::delete_from_count_indexed_tree`, the other through a
batch.

- On grove versions 1 to 4 each tree then holds four entries and its
  ranking five rows. The removal frees 160 bytes where grove version 5
  frees 356.
- Grove version 4 can neither read nor prove such a ranking: both fail on
  the row whose entry is gone (`CorruptedData`). Grove versions 1 to 3
  refuse to read any ranking (`NotSupported`), so there the row shows only
  in the costs and in the root hash.
- `GroveDb::reconcile_indexed_tree_secondaries` removes the row (196 bytes)
  on grove versions 1 to 4, and the ranking of grove version 4 is read and
  proven again. After both repairs the root hash is the one grove version 5
  has had since the removals.
- On grove version 5 the removal takes the row with it, the ranking is
  read and proven at once, and the repair does not move the root hash.

**Three rows, and what the repair leaves.** A third tree is filled with
sixteen keys in order and loses the fourth, the eighth and the twelfth
through the method of the tree. Each removal leaves its row on grove
versions 1 to 4 (160 bytes freed, 356 on grove version 5).

- The repair of grove versions 1 to 4 removes the rows one commit of the
  ranking's tree at a time, all into one operation of the storage batch,
  which keeps a put over a later delete of the same key. Removing a row
  rewrites the nodes around it, and a row that is rewritten and then
  removed stays in storage. The repair frees 196 bytes, one row of the
  three. The root hash it leaves is the one grove version 5 has: the tree
  of the ranking holds thirteen rows.
- The rows of a ranking are read from storage and proven from the tree. So
  after the repair, grove version 4 proves the ranking, the proof verifies,
  and the read of the same ranking fails on a row whose entry is gone
  (`CorruptedData`). This is the one kind of step in the corpus where a read
  fails and its proof is recorded.
- A second repair frees nothing and moves nothing: the released repair
  does not see a row that is in storage and in no tree. The read fails as
  before.
- On grove version 5 the three removals take their rows with them, and
  both repairs change nothing.

A record that is stored under a key of a subtree and that no node of the
tree links is therefore a state a released grove version reaches through
its public operations, and another engine has to hold it to give these
answers: a cursor over the subtree shows the record, the tree and its root
hash do not have it. The repair of grove version 5 removes such records
(GroveDB #1063). No stream or case runs it on a tree that holds one yet.

The other half of what was seen in a generated stream (BASE-01, `wide-ranks`
on grove version 4: a ranking that lacks the row of an entry a batch
wrote) is not in the case. Batches that insert, replace and delete entries
of such trees left their rankings in step when it was looked for.

`golden::legacy`'s tests run every case on every grove version and check
these numbers, and `tests/golden.rs` checks that no other trace of the
corpus has a failed operation or a proof that does not verify.

The generated streams delete from indexed trees on grove versions 1 to 4
as well, and `verify_grovedb` reports a row such a removal leaves. The
corpus records the costs, root hashes, results and proofs of those streams
as they are. It does not record what `verify_grovedb` reports. The removal
and the repair of `indexed-writes` (steps 14 and 16) cost on grove version 4
what they cost on grove version 5, and the repair frees nothing: that
stream does not hold the drift, and `ranking-drift` is where the corpus
holds it knowingly.

### Excluded: the deletes that panic

On grove versions 1 to 3, `GroveDb::delete` of a populated tree panics
("feature_type is inconsistent with its tree_type", in
`TreeNode::hash_for_link`) when

- the child's type commits an aggregate into its node hashes: a
  `ProvableCountTree`, `ProvableCountSumTree`, `ProvableSumTree`,
  `ProvableCountProvableSumTree` or one of the three indexed trees;
- the parent keeps at least one other element;
- and the parent's type commits something else, or nothing: the parent's
  remaining nodes are hashed as the child's type, which they are not.

That is 56 pairs of parent and child type (`golden::legacy::excluded`). A
panic returns nothing to record, and no chain can have committed a block
that panicked, so these are not in the corpus for grove versions 1 to 3.
`the_excluded_cases_are_the_deletes_that_panic` runs every pair of parent
and child type on those versions and checks that the ones that panic are
exactly these 56. From grove version 4 the same deletes succeed and are part
of the generated streams (`element-zoo`, `tiny-subtrees`, `delete-heavy`).

## The reference scale and the two named proofs

`reference/v<N>.trace` is the workload the plan's measurements were taken
on, `Documents::reference()` with seed `0x1`: one million 400-byte documents
over ten thousand owners, one document and one index reference per state
transition, twenty transitions per block, 1,050,008 operations. It is
recorded and replayed only on request (`--reference`, or the ignored test
`the_reference_scale_replays_green`):

- Recording or replaying one grove version takes 7 to 8 minutes on the
  machine above, in a release build with the database on a memory-backed
  directory (`--scratch /dev/shm/…`), and a gigabyte of scratch. One after
  another, which is the default, all five take 40 minutes. Three at a time
  (`--jobs 3`) they took 13 minutes.
- When the machine is short of memory the same run takes several times as
  long, on a disk or in memory: RocksDB reads through memory-mapped files,
  and those pages are the first to be evicted. Five at a time on that
  machine took 37 minutes for that reason.

The file has a checkpoint every 1,000 steps and then, in full, the root
hash and the two named proofs:

- the **point proof** of document 500,000 by its id
  (`Documents::point_query`);
- the **100-document proof**: the first hundred documents of owner 0, read
  through its index references (`Documents::owner_query`).

| Grove version | Point proof | 100-document proof |
| --- | --- | --- |
| 1, 2 | 2,222 bytes | 52,545 bytes |
| 3, 4, 5 | 2,227 bytes | 52,552 bytes |

The state is the same on every grove version (one root hash). Grove
versions 3 and later wrap a proof in an envelope that names its version,
which is the difference in size. `the_named_proofs_are_in_the_corpus`
checks, without the database, that the committed proofs have these sizes and
verify to the recorded root hash and to one and one hundred 400-byte
documents.

### These are not the sizes the plan measured

The plan lists a 2,848-byte point proof and a 49,835-byte 100-document
proof. The harness those were measured with is not in the repository, in
Platform, or in the issue, and BASE-01 modelled Drive's shapes without
measuring. This workload does not reproduce them, and the cause could not be
pinned to one detail, so the workload was left as it is:

- **Bytes per state transition.** The plan says a transition adds 551 bytes
  of keys and values. This workload adds 583: a 32-byte key and a 441-byte
  item for the document (400 bytes of body, 35 bytes of storage flags with
  the owner id), a 32-byte key and a 78-byte reference. The difference is 32
  bytes, the size of one owner id.
- **Point proof.** A point proof here is 1,003 bytes plus 68 for every
  level of the primary tree above the document (998 plus 68 on grove
  versions 1 and 2). Over 4,000 documents spread evenly through the million
  it is 1,343 to 2,431 bytes, 2,227 at the median. 2,848 bytes would be a
  document 27 levels down. The deepest of the 4,000 is 21 levels down.
- **100-document proof.** Over the first 400 owners, those with a hundred
  documents or more give 52,110 to 52,824 bytes. A proved row is 68 bytes
  plus the document, so a hundred 441-byte documents are 50,900 bytes before
  any tree above them, which is already more than 49,835. The documents the
  plan measured were stored in about 30 bytes less each.
- **What would fit.** A 409-byte document (storage flags without the owner
  id) with the reference as it is, or a 405-byte document without flags
  with an 82-byte reference, both give 551 bytes per transition and a
  100-document proof that small. Either would also need several hundred
  bytes more in the trees above the documents than this workload has (a
  root tree with Drive's other subtrees, more than one contract or document
  type), and the three published numbers do not say which. Changing the
  workload to hit them would be fitting it to the numbers.

Until the measuring harness is found, these traces record the reference
workload as BASE-01 defined it, at the sizes it has.
