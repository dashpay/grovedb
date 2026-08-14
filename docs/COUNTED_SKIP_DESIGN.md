# Counted skip for unproved ranked reads — agreed design (v2)

**Status:** agreed, implemented on this branch.
**Branch:** `fix/counted-skip-unproved-read`, based on `a2791bb` (`develop`).
**Supersedes:** the v1 emitter-extraction proposal, preserved in git history at `356af1e8`
(`git show 356af1e8:docs/COUNTED_SKIP_DESIGN.md`). Nothing in v1 was found unsound; v2 is
strictly smaller and was reached independently by two research passes.

## 1. History, and why the unproved route is the fix rather than an optimization

`indexed_axis_top_k_paginated_generic` (`grovedb/src/operations/indexed_tree.rs`) skipped the
offset by stepping a storage iterator once per skipped entry — `Θ(min(offset, N))`, a
consensus-reachable linear walk (~440 ms at deep offsets on the measured Platform fixture). All
three axes (Count u64 / Sum i64 / Avg i128 fixed-point) funnel through this one function.

The first shipped mitigation (Platform PR #4382) routed the unproved read through
prove-then-verify internally: flat ~130 µs at any offset. It then drew a **blocking review** for a
structural reason: its handler-local retry re-executed the Drive request while still holding the
`platform_state` captured before the query began, so in the window where new state is published
but the block-height guard has not yet updated, a retry could attach the *old* block's signature
and metadata to *new*-state proof bytes. A counted unproved walk has no proof envelope and needs
no retry, so that failure mode ceases to exist — this design does not work around that review, it
removes the thing the review objected to.

v1 of this document proposed extracting the emitter's decision logic (`decide.rs`) and adding a
merk-level `read.rs` + `Merk::read_count_offset_on_range`. Two later research passes (run
independently, then compared) both rejected that shape as unnecessarily risky and converged on the
design below; the owner confirmed the direction.

## 2. Facts about `a2791bb` the design rests on (verified by both passes)

1. **No unproved counted-skip / rank-select primitive exists.** v1's negative claim holds:
   `merk/src/proofs/query/count_offset/` is `emit / mod / prove / tests / verify`, every entry
   point produces proof ops, and nothing else in the workspace descends by count.
2. **But no-proof aggregate-aware walks are established precedent.**
   `Merk::count_aggregate_on_range` (`merk/src/merk/get.rs:377`), `sum_aggregate_on_range`
   (`:431`) and `count_and_sum_aggregate_on_range` (`:491`) walk the tree using link aggregates
   with no proof machinery, and every `aggregate_*` proof module has a no-proof `walk.rs` sibling.
   `count_offset/` is the only one missing its own — and grovedb's own proved rank-of-key
   generator already trusts this family (`proof/indexed_axis/generate.rs:571` computes the rank
   with the *unproved* count walk).
3. **Everything a counted descent needs is public.** `Merk::walk` (`merk/src/merk/mod.rs:523`)
   yields a `RefWalker`; `RefWalker::{tree, walk}`, `TreeNode::{link, key, aggregate_data}` and
   `Link::aggregate_data` are all public. The load-bearing fact: **`Link::aggregate_data()`
   reads off `Link::Reference`** (`merk/src/tree/link.rs:209`), so a child subtree's population
   is known *before* paying a fetch for it. (It panics on `Link::Modified`, which a freshly
   opened read-only merk never holds — same exposure as merk's own walkers.) The implementation
   deliberately does **not** use the public `AggregateData::as_count_u64`, which silently
   returns 0 for non-count variants; it uses a strict matcher that treats any non-provable-count
   aggregate as corruption. v1's claim that a new entry point had to live inside merk (because
   `use_tree` is `pub(crate)`) was wrong.
4. **Counted rank equals iterator position on any valid secondary.** The secondary's merk key is
   the complete sort key — order-preserving axis encoding ‖ original item key
   (`make_axis_secondary_key`), so in-order traversal = lexicographic order = exactly what
   `raw_iter` produces, ties broken by item key inside the key bytes, total order, both
   directions. Every valid row contributes structural count exactly 1: the secondaries are
   `ProvableCountTree` (count axis) / `ProvableCountProvableSumTree` (sum, avg), the only writer
   (`mirror_indexed_axis_to_secondary`) writes exactly one count-1 payload per primary entry, and
   `verify_indexed_axis_content` enforces it.

## 3. The agreed design

A grovedb-local counted traversal in `operations/indexed_tree.rs`, reached through the public
`Merk::walk`. No merk change of any kind. Specialized to `RangeFull` — the paginated caller only
ever scans the whole secondary, so `classify_subtree`, inherited bounds, and the proof flow's
shape-rejection rules are all unnecessary here.

- **`offset == 0` keeps the raw-iterator path unchanged** — it is the overwhelmingly common
  shape, and one iterator seek plus `k` sequential steps is cheaper than loading a root-to-leaf
  merk path. The counted traversal serves `offset > 0` only.
- **Whole-population shortcut:** the root node is already loaded by the open, and its aggregate
  is the population `N`. `offset ≥ N` returns empty with **zero** additional fetches — the
  past-the-end case, which is the original DoS lever, costs the open and nothing else.
- **Counted descent**, per loaded node, children visited in direction order (ascending: left
  first; descending: right first):
  1. Read the first child's count from its **link**, without fetching the child.
  2. If that count ≤ remaining offset: subtract and skip the child entirely (no fetch). This is
     the counted skip.
  3. Otherwise fetch (`RefWalker::walk`) and recurse.
  4. Consume the node itself: its own structural count (aggregate − left link − right link, via
     `checked_sub`) must be exactly 1 — anything else is corruption and fails loud
     (`Error::CorruptedData`) rather than silently diverging from positional order. If offset
     remains, burn it; else emit the node's key into the page.
  5. Recurse into the second child only while the page is not full; return the moment it is —
     a reader has no obligation to keep walking, unlike the prover, which must hash-bind the
     rest of the tree.
- The page holds secondary **keys** only; the caller decodes them exactly as before (the old
  loop also discarded values). Cost is `O(depth + k)` node fetches for `offset > 0`.
- The three paginated APIs return `IndexedTopKPage { entries, skipped }`, where `skipped` is the
  **true** skipped count `min(offset, population)` — read from the root aggregate at zero extra
  cost. The old linear read structurally could not report this (an offset past the end just
  exhausted the iterator); the proved path attests the same quantity through its count
  commitments, so unproved and proved reads now agree on it. Like the entries, the unproved
  value is the local tree's claim, not independently verifiable.

### Where the two research passes differed (recorded per the consolidation request)

Both passes agreed on: the negative primitive claim; the walk-family precedent; link-count skip
without fetching; the root shortcut; own-count==1 fail-loud; the `offset == 0` fast path; the
`RangeFull` specialization; O(depth + k) cost; and proof-byte neutrality by construction. They
differed on two points:

1. **Placement.** One pass preferred a merk-level `count_offset/walk.rs` twin with an entry in
   `get.rs` (maximal conformance to the walk-family precedent, merk-level testability); the other
   preferred the grovedb-local specialization (no new public merk API, no cross-crate release
   coordination, no general-range machinery on the hot path). The owner chose grovedb-local. If a
   general-range or cross-crate primitive is ever wanted, the merk-level twin is the shape — and
   it must live under `merk::proofs::query`, because `classify_subtree` /
   `SubtreeClassification` are `pub(super)` there.
2. **Open-cost constant** (≈2 vs ≈3–5 seeks before the descent). Settled empirically by the cost
   regression test rather than argued.

## 4. Behavior in corrupt or concurrent states (unchanged posture from v1, re-confirmed)

- **Ghost rows** (storage rows absent from the tree, or vice versa): the old read iterates
  storage, the new read walks the tree, so results in a *drifted* secondary differ. That state
  is corruption by definition (`verify_grovedb` flags it; the drift tests build it deliberately),
  and the proved path already walks the tree — the change makes unproved reads agree with proved
  reads instead of with the raw-storage accident.
- **Corrupt keys inside the skipped region** are no longer decoded (not visiting them is the
  point); returned keys are still validated, and a visited node with own-count ≠ 1 fails loud.
- **Snapshot consistency: restored after a blocking Platform review finding.** The first
  implementation fetched nodes through independent `RefWalker` point-gets on a snapshotless
  transaction; a commit landing mid-descent could hand back a child from a newer state than its
  resident parent, and because merk's child loads never verify the fetched child against the
  parent's recorded link hash — and the count cross-checks cannot see a same-population update —
  the result was a *silently mixed page*. (The proved path survives the same torn reads because
  verification's ancestor-chain reconciliation rejects them; the unproved read has no such
  check, which is exactly why it needed the snapshot.) A second review round tightened the
  boundary further: pinning only the traversal still left **root-key discovery** outside the
  view, and a commit rotating the secondary root between discovery and traversal could leave the
  old root key resolving to a *demoted child* in the newer view — an internally consistent
  subtree that every count check accepts, silently truncating the page. The final shape
  therefore pins the *entire read*: one raw iterator (implicit RocksDB snapshot at creation plus
  the transaction's uncommitted-write overlay) is created under the parent merk's prefix,
  re-reads the indexed element to obtain the authoritative secondary root key, is retargeted to
  the secondary's prefix (`PrefixedRocksDbRawIterator::retarget`, same underlying iterator, same
  snapshot), and then serves the root fetch, the descent, and the collect. **Nothing the page is
  built from is read outside that one view**; the ordinary validated open still runs first, but
  purely for validation and the offset-0 fast path — none of its loads are trusted as page data.
  This restores, and slightly exceeds, the guarantee the replaced single-`KVIterator` linear
  scan had. The transaction-overlay half is pinned by an always-on test; the
  commit-interleaving half is not deterministically testable (no way to pause a synchronous
  read between fetches) and rests on RocksDB's iterator-snapshot contract, as the old code's
  guarantee did.
- **One function, two views.** `offset == 0` serves the storage-iterator view; `offset > 0`
  serves the merk-tree view. Identical on any clean secondary; in a drifted one, a caller paging
  `offset = 0, k, 2k, …` could see a discontinuity between the first page and the rest. Accepted:
  the drifted state is corruption, and the pages a drifted state produces are not consistent
  under the old code either (they include rows `verify_grovedb` rejects).

## 5. Proof-byte neutrality — how it is proven rather than asserted

The consensus-frozen envelope must not move. Three lines of proof, strongest first:

1. **By construction:** the diff touches only `grovedb/src/operations/indexed_tree.rs` and test
   files. No file under `merk/` and no grovedb proof module is modified — `git diff --stat`
   against the base commit is checkable in review. The new code calls only read-only public
   traversal APIs (`Merk::walk` / `RefWalker::walk`) and shares zero code with proof emission,
   so no code path exists through which emitted bytes could change.
2. **Empirically:** the entire existing **proof** suites — `merk` `count_offset` tests, grovedb
   `indexed_axis_proof_tests`, `indexed_axis_offset_proof_tests`, `count_offset_paginated_tests`
   — pass with **zero edits**. Any proof-test edit would be a red flag, not a rebase. (One
   non-proof test *was* deliberately edited: a drift-suite assertion that pinned the linear
   skip's decode-during-skip behavior — see §7.4.)
3. **Root-hash pinning:** existing tests assert verified proofs against the live root hash, so
   even an indirect state-shape change would surface as hash mismatches.

This is why v1's golden-digest machinery is not needed: it existed to police an edit to
`emit.rs`, and v2 makes no such edit.

## 6. Measured costs

Measured with the in-repo harness (`indexed_axis_paginated_cost_tests::measure_paginated_costs`,
release build, N-row Count-axis secondaries, `k ∈ {1, 100}`, wall-clock = min of 3 runs on a
loaded shared machine — the seek/byte counters are the machine-independent signal). "linear" is
the pre-change implementation, kept verbatim as a test-only baseline; the harness also asserts
both paths return identical rows at every point. Selected rows (`k = 1`; full grid in the
harness output):

| N | offset | counted (seeks / bytes / µs) | linear (seeks / bytes / µs) |
|---:|---:|---:|---:|
| 1e6 | 0 | 5 / 629 / 7 | 5 / 629 / 6 |
| 1e6 | N−1 | 24 / 5,985 / 32 | 1,000,004 / 316 MB / 306,699 |
| 1e6 | ≥ N | 5 / 817 / 9 | 1,000,004 / 316 MB / 312,300 |

(Numbers are from the final fully-pinned implementation. Relative to the original point-get
walk, the pinned view costs two extra seeks — the in-view re-read of the indexed element that
carries the authoritative secondary root key, and the root node fetch — and charges full
prefixed key bytes per fetch. Wall-clock is unchanged; the past-the-end shape stays flat at
every N.)

What the numbers establish:

- **Offset 0 has zero regression** — identical seeks, bytes, and wall-clock in both directions
  and at both `k` values, at every N (the fast path *is* the old code, and the always-on
  `paginated_offset_zero_costs_exactly_plain_top_k` test pins the equality).
- **The deep-offset seek count is the tree depth plus the two pinned-view discovery reads**:
  13 → 17 → 20 → 24 across N = 1e3 → 1e6, logarithmic exactly as designed; wall-clock 14–37 µs
  against the linear walk's 268 µs–312 ms.
- **Past-the-end — the DoS lever — is flat 5 seeks / ~817 B / ≤ 11 µs at every N**: the
  in-view element and root reads alone answer it with zero descent.
- Against the previously measured prove-then-verify route (78–129 µs flat, plus its proof
  construction/serialization/verification CPU), the counted read measures 6–37 µs at `k = 1` —
  the "unproved is even faster" claim is now measured, not modeled.
- **The one corner where the old path was faster, measured and accepted:** tiny positive offsets.
  At `offset = 1, k = 100` the counted path costs 107–112 seeks / ~14 KB / 141–155 µs against the
  linear read's 105 seeks / ~32 KB / ~30 µs — near-identical counters, ~5× wall-clock, because k
  tree-node point-gets are slower than k sequential iterator steps. The crossover to
  counted-wins sits around offset ≈ a few hundred rows. Absolute worst measured cost is ~155 µs
  (the same order as prove-then-verify's flat floor), the shape decays as offset grows, and the
  alternative — a threshold hybrid falling back to the linear skip for small offsets — would make
  skipped-region error semantics depend on the offset value. Accepted as-is; §8 keeps the hybrid
  on record if max-cost-per-request ever matters more than semantic uniformity.

## 7. Test plan

1. **Equality grid** (pins behavior; green before and after): the unchanged `indexed_*_top_k`
   iterator path is the oracle — `paginated(k, offset) == top_k(offset + k)[offset..]` across all
   three axes, both directions, offsets at and around subtree boundaries / population − 1 /
   population / past-end, `k ∈ {0, 1, small, larger-than-remainder}`, and tie-heavy populations
   exercising the item-key tiebreak.
2. **Offset-0 non-regression pin:** `paginated(k, 0)` must cost exactly what `top_k(k)` costs in
   `seek_count` — the fast path is the old code, and this assertion keeps it that way.
3. **The counted-skip assertion** — the caller-usable proof that the skip is counted rather than
   linear. Platform's executors discard `CostContext`, so this must live grovedb-side, and it
   can: every indexed API returns `CostResult`. On an N-row fixture, deep-offset `seek_count`
   must stay within `offset-0 seek_count + small multiple of the AVL depth bound
   (1.44·log2(N+2))`, and past-the-end must not exceed the offset-0 cost. **Red before this
   change** (the linear walk pays ~N seeks), green after — the red run is recorded in the commit
   message. The bound encodes *why* ("the offset costs at most one descent"), so it fails again
   if anyone reintroduces per-entry skip work.
4. **Existing suites** — every proof suite passes with zero edits (§5). Exactly one existing
   test changed, deliberately: the drift-suite case that asserted the linear skip decodes every
   skipped row (`indexed_tree_secondary_drift_tests`, the `offset = 1` assertion). Under the
   counted skip, a malformed-but-tree-resident row still occupies its position (it is counted)
   but its key is never decoded, so the read serves the positionally identical page instead of
   erroring; the assertion now pins that contract. This is §4's second bullet made concrete —
   detection of skipped-region corruption belongs to `verify_grovedb` and to the shapes that
   actually decode the row.

## 8. Explicitly out of scope

- Snapshot isolation for unproved reads (pre-existing, storage-layer).
- A general-range merk-level walker and the large-`k` iterator-hybrid collect — neither is needed
  by the only caller; both are recorded above should the need appear.
