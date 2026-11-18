# Golden trace corpus

This directory is the definition of "same answers" for LigninDB
(dashpay/grovedb#1052, task BASE-02). It records what GroveDB on the RocksDB
engine returned, step by step, for every workload on every grove version:

- the `OperationCost` of each operation;
- the root hash after each write, batch and commit;
- the result of each read;
- for each query, the proof bytes, the cost of proving it and whether the
  proof verifies.

A replay runs the same operations again and compares every one of those
values. The code is `lignin-testkit::golden`; its module documentation
describes the design.

## Pinned engine

Every trace names the commit of the engine it was recorded from, in its
`engine` line: the newest commit that touched a workspace crate outside
LigninDB. The corpus here was recorded from
`22f163c2b82d9bc5c985a77034529a32ef9a7c04` (`develop`, "range aggregates and
count offsets on a ProvableCountProvableBigSumTree"). The workspace manifest
is not part of that: LigninDB adds its crates to it. `Cargo.lock` is not
committed in this repository, so the commit pins GroveDB's own code and not
the versions of its dependencies. `record` refuses to run when an engine
crate has uncommitted changes, unless it is given the commit to write down
(`--engine`).

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

## What is recorded

For each grove version (`v1` to `v5`):

| Trace | Database | Seed | Steps written out in full |
| --- | --- | --- | --- |
| one per workload of `Workload::suite` (`documents`, `balances`, …) | its own | `0x1` | the first of each kind of operation |
| `suite`: every workload of the suite, one after another | one for all | `0x4c49474e494e` | the first of each kind of operation |
| `legacy-…`: one per hand-written case, see below | its own | `0x0` (no generator is used) | all |

A workload is generated for the features of the grove version
(`Features::for_grove_version`), so the same seed gives another stream on
another grove version. Each segment of a trace closes with the number of
operations and the digest of its stream, the one BASE-01's tests pin.

The standard scale is 130 traces and 164,051 steps in 2.6 MB. On the
machine it was recorded on (28 cores, shared), recording it takes about 25
seconds in a release build and the replay test about 35 seconds in a debug
build, eight traces at a time.

A consumer that computes with the steps, as a cost model checked against
every recorded `OperationCost` does, takes them as values from
`golden::record::each_step` over the traces of `golden::plan::plan`.

### What the corpus does not see

The corpus records what `workload::apply` gets back from GroveDB, so it is
blind where `apply` or GroveDB's public interface is:

- `GroveDb::clear_subtree` returns no cost, so a `ClearSubtree` step records
  a cost of zero. Its root hash is recorded.
- `apply` drops what the typed append methods return (the new root and
  position of an `MmrTree`, `BulkAppendTree` and dense tree, the outcome of
  a `PrivateDocumentStore` insert). The root hash after the append covers
  the state they describe.
- An error is recorded by the name of its variant, and for a variant that
  wraps the error of a lower layer, that error's variant as well
  (`MerkError::…`). Its message is written next to it as a remark and is not
  compared.
- A proof is the one `GroveDb::prove_query` builds with no options, from
  committed state, for the queries the workloads make. Verification is
  recorded as its outcome (verified, refused, or verified to another root),
  not as the rows the verifier returns.

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
  (not compared), then every value.
- `tally` counts the operations that failed, the queries that could not be
  proved and the proofs that do not verify. All three are zero for every
  generated workload.
- `end` carries the number of steps and one 256-bit digest over every byte
  of every step. That digest, not the short lines, decides whether a replay
  is the same.

A trace too long for a line per step (the reference scale) has `@steps
digest` checkpoints in their place.

## Older grove versions: the hand-written cases

BASE-01's generators steer around three behaviours of older grove versions
so that every stream applies and every query proves. A node on such a
version may have done exactly these things, so the corpus records them, as
the engine returns them, on every grove version. Each case is a few
operations on an empty database (`lignin-testkit::golden::legacy`), recorded
in full. The numbers are the tally of the trace: queries that could not be
proved / proofs that do not verify.

**1. `GroveDb::delete` of a populated tree when the tree or its parent
commits an aggregate into its node hashes.** Grove versions 1 to 3 reopen
the parent as a tree of the child's type. Each case reads the parent before
the delete, after it, and after the next write to the parent.

| Case | v1, v2 | v3 | v4, v5 |
| --- | --- | --- | --- |
| `delete-tree-under-provable-count-tree` | 0 / 1 | 0 / 2 | 0 / 0 |
| `delete-sum-tree-under-provable-count-sum-tree` | 0 / 1 | 0 / 2 | 0 / 0 |
| `delete-count-tree-under-provable-sum-tree` | 0 / 1 | 0 / 2 | 0 / 0 |
| `delete-mmr-tree-under-provable-count-tree` | 0 / 1 | 0 / 2 | 0 / 0 |
| `delete-tree-under-provable-count-provable-sum-tree` | 3 / 0 | 0 / 2 | 0 / 0 |
| `delete-provable-count-tree-under-provable-count-tree` | 0 / 0 | 0 / 0 | 0 / 0 |
| `delete-provable-count-indexed-tree-under-provable-count-tree` | 0 / 0 | 0 / 0 | 0 / 0 |
| `delete-only-provable-count-tree-under-tree` | 0 / 0 | 0 / 0 | 0 / 0 |
| `delete-only-provable-sum-indexed-tree-under-tree` | 0 / 0 | 0 / 0 | 0 / 0 |
| `delete-only-tree-under-provable-count-tree` | 0 / 0 | 0 / 0 | 0 / 0 |

In the first five the delete commits a link hash for the parent that leaves
its aggregate out, so the root hash after the delete differs from the one
grove versions 4 and 5 give. The proof of a query in the parent is then
refused by the verifier. On grove version 3 the proof of the parent's own
element is refused as well. The next write to the parent recomputes the
hash and the proofs verify again. In the last five the reopened parent
hashes as it should (the child is of a type that hashes what the parent's
does, or the parent is left empty); what differs between grove versions
there is the cost of the delete.

**2. A query hosted by a `ProvableCountProvableSumTree` or an indexed
tree.** Grove versions 1 and 2 refuse to prove it (`NotSupported`): it needs
the proof envelope of grove version 3.

| Case | v1, v2 | v3, v4, v5 |
| --- | --- | --- |
| `query-in-provable-count-provable-sum-tree` | 3 / 0 | 0 / 0 |
| `query-in-provable-sum-indexed-tree` | 3 / 0 | 0 / 0 |
| `query-in-provable-count-indexed-tree` | 3 / 0 | 0 / 0 |
| `query-in-provable-count-provable-sum-indexed-tree` | 3 / 0 | 0 / 0 |

**3. A reference stored in a `ProvableCountSumTree`.** On grove versions 1
and 2 its proof does not verify (`InvalidProof`).

| Case | v1, v2 | v3, v4, v5 |
| --- | --- | --- |
| `reference-in-provable-count-sum-tree` | 0 / 4 | 0 / 0 |

`golden::legacy`'s tests run every case on every grove version and check
these numbers, and `tests/golden.rs` checks that no other trace of the
corpus has a failed operation or a proof that does not verify.

A fourth behaviour stays in the generated streams: on grove versions 1 to 4,
deleting from a count-indexed tree leaves orphan rows that `verify_grovedb`
reports. The corpus records the costs, root hashes, results and proofs of
those streams as they are. It does not record what `verify_grovedb` reports.

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
  directory (`--scratch /dev/shm/…`), and a gigabyte of scratch. All five
  take that long side by side (`--jobs 5`), or five times as long one after
  another, which is the default.
- When the machine is short of memory the same run takes several times as
  long, on a disk or in memory: RocksDB reads through memory-mapped files,
  and those pages are the first to be evicted.

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
