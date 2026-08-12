# Counted skip for unproved ranked reads — design note

**Status:** design only — **deferred, not implemented**. No code was written.
**Branch:** `fix/counted-skip-unproved-read`, based on `a2791bb` (`develop`).
**Problem statement:** `platform.test-indexes/docs/ranked-index-testing/FIX_SPEC_OFFSET.md`.
**Line numbers below are as of `a2791bb`** — if they have drifted, search by function name.

### Why this is on the shelf

**This design was deliberately not implemented. It was superseded by a cheaper mitigation, not
abandoned as unworkable.** Nothing below was found to be wrong, infeasible, or blocked — the work
simply stopped being necessary before it started, and the note was kept as the record.

The spec's option A (this document) was the owner's first choice. It was then superseded as the
*immediate* remedy by option C: Platform serves unproved ranked reads through the prover internally
and verifies its own proof to recover the entries. That removes the linear offset walk with no
grovedb change, no query-grammar change and no cross-repo pin bump, so it shipped first. Measured:
78–129 µs round trip, with the deep-offset lever — the whole point of the exercise — dropping from
440 ms to a flat 48 µs. That is a complete fix for the denial-of-service problem, which is why this
one stopped being ship-blocking.

This design stays the correct long-term shape. What it would buy once picked up: the unproved read
stops paying prove-then-verify overhead (proof construction, serialization, and full verification on
every read) and instead does the counted descent directly — the same `O(log n + k)` work with none of
the proof machinery. Against the 78–129 µs baseline above, that is an optimization, not a fix; treat
it as such when deciding whether it earns the risk of touching the proof emitter. It also makes the
code comment at `mode_detection.rs:302-313` true for both paths rather than only the proved one.

The finding that reshaped the decision, recorded here so nobody re-derives it: **there is no
non-proof counted-skip primitive in merk.** The counted descent exists only inside the proof
emitter. Verified independently by an adversarial review. That is why option A is an extraction plus
a new entry point, not a wiring job — and why it was worth deferring rather than rushing.

## 0. What the problem is, restated against this repo

`indexed_axis_top_k_paginated_generic` (`grovedb/src/operations/indexed_tree.rs:1297`) skips the
offset by stepping a **storage** iterator once per skipped entry:

```rust
let mut iter = KVIterator::new(secondary_merk.storage.raw_iter(), &all_query)…;
while skipped < offset {
    match iter.next_kv() { Some((secondary_key, _)) => { decode(&secondary_key)?; skipped += 1 } … }
}
```

That is `Θ(min(offset, N))`. All three axes funnel through this one function
(`indexed_count_top_k_paginated:1483`, `indexed_sum_top_k_paginated:1677`,
`indexed_avg_top_k_paginated:1862`), so one fix covers the whole family.

The proved path for the same query shape is
`build_indexed_axis_paginated_proof` → `Merk::prove_count_offset_on_range(RangeFull, offset,
Some(k), !descending)` (`grovedb/src/operations/proof/indexed_axis/generate.rs:904-918`). It walks
the **merk tree** and collapses any wholly-in-range subtree whose aggregate count fits inside the
remaining offset into one step — `O(log n + k)`.

Confirmed against the tree: there is no non-proof counted-skip / rank-select primitive anywhere in
`merk`. `merk/src/proofs/query/count_offset/` is `emit / mod / prove / tests / verify`; every entry
point produces `Op`s. The independent reviewer reached the same conclusion.

## 1. Exactly what can be shared, and what cannot

I went through `emit_count_offset_proof` (`count_offset/emit.rs:95-503`) line by line. It does seven
things; only two of them are the counted-skip logic:

| # | Step | Shareable with a plain read? |
|---|---|---|
| 1 | `classify_subtree(lo, hi, range)` | **Already shared** — lives in `aggregate_common.rs`, used by aggregate-count/sum too. Reuse as-is. |
| 2 | Collapse decision (Disjoint / count ≤ offset / past-limit / descend) — `emit.rs:136-148` | **Yes.** Pure function of `(class, subtree_count, offset_remaining, limit_remaining)`. |
| 3 | `own_struct = count − left_link_count − right_link_count` — `emit.rs:223-244` | **Yes.** Three lines of arithmetic on link aggregates. |
| 4 | Direction ordering (`first_dir`/`second_dir`) — `emit.rs:324-328` | **Yes**, but it is two lines; sharing it is not worth an abstraction. |
| 5 | Per-node disposition (path / offset-skipped / returned / past-limit) — `emit.rs:412-430` | **Yes.** Pure function of `(is_in_range, own_struct, offset_remaining, limit_remaining)`. |
| 6 | Op emission: `Node::HashWithCount…`, `Push`/`PushInverted`, `Parent`/`Child`, `emit_returned_node` | **No** — proof-only. Must not be touched. |
| 7 | Rejection of unsupported in-range shapes (NonCounted / Reference / non-empty tree) — `emit.rs:276-319` | **No** — those are constraints of the *wire format*, not of the traversal. The reader has its own (different) rule; see §4.3. |

There is also one thing the reader must **not** share: the prover cannot stop early. Once the limit
is exhausted it still has to walk the rest of the tree and emit a `HashWithCount` per remaining
subtree, because the verifier reconstructs the root hash from the whole op stream. A plain read has
no such obligation and returns the moment `limit_remaining == Some(0)`. So the *traversal* is not
literally shared; the *decisions* are.

### Proposed extraction

New file `merk/src/proofs/query/count_offset/decide.rs`, holding only pure code moved verbatim out
of `emit.rs`:

```rust
pub(super) enum CollapseAction { Disjoint, SkippedByOffset, PastLimit }   // moved from emit.rs:511
pub(super) enum NodeDisposition { Path, SkippedByOffset, PastLimit, Returned }

/// Whole-subtree decision. `None` = must descend per-element.
pub(super) fn collapse_action(
    class: SubtreeClassification, subtree_count: u64,
    offset_remaining: u64, limit_remaining: Option<u64>,
) -> Option<CollapseAction>;

/// Per-node decision at a descended node.
pub(super) fn node_disposition(
    is_in_range: bool, own_struct: u64,
    offset_remaining: u64, limit_remaining: Option<u64>,
) -> NodeDisposition;

/// own_count = aggregate − left_link_count − right_link_count (saturating, as today).
pub(super) fn own_structural_count(node_count: u64, left: u64, right: u64) -> u64;
```

Both decision functions are **total, pure, allocation-free, and take no `&mut` state**. State
mutation (`offset_remaining -= 1`, `limit -= 1`, `returned += 1`) stays at each call site, exactly
where it is today. `emit.rs` shrinks by ~25 lines and gains three call sites; nothing else in it
moves.

**Why not a visitor/callback trait over the whole recursion?** It would force the prover's
walk shape onto the reader (no early exit), thread a lifetime-heavy sink through the hot path, and —
decisively — it would rewrite the function that produces consensus-frozen bytes. The
decision-function extraction touches the emitter in three mechanical spots and can be argued
bit-identical (§3); a visitor rewrite cannot.

## 2. The new read-only entry point

Two new files, mirroring the prover's own split exactly:

**`merk/src/proofs/query/count_offset/read.rs`** — the recursion:

```rust
pub(super) struct ReadState { offset_remaining: u64, limit_remaining: Option<u64>,
                              left_to_right: bool, done: bool }

pub(super) fn read_count_offset<S: Fetch + Sized + Clone>(
    walker: &mut RefWalker<'_, S>, range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>, subtree_hi_excl: Option<&[u8]>,
    state: &mut ReadState, out: &mut Vec<(Vec<u8>, Vec<u8>)>,
    grove_version: &GroveVersion,
) -> CostResult<(), Error>;
```

Same bound-tightening (`walk left → (lo, node_key)`, `walk right → (node_key, hi)`), same direction
ordering, same `classify_subtree`, same `collapse_action` / `node_disposition`. Differences from
`emit`: no `ops`, no `Node` construction, no `tree_type` parameter (nothing is hashed, so the
single-axis vs dual-axis PCPS split is irrelevant here), returns `()` instead of a structural count
(only the verifier needs that), and returns early once `state.done`.

**`merk/src/merk/read_count_offset.rs`** — the entry point, sibling of
`merk/src/merk/prove_count_offset.rs`:

```rust
impl<'db, S: StorageContext<'db>> Merk<S> {
    pub fn read_count_offset_on_range(
        &self, inner_range: &QueryItem, offset: u64, limit: Option<u64>,
        left_to_right: bool, grove_version: &GroveVersion,
    ) -> CostResult<CountOffsetReadResult, Error>;
}

pub struct CountOffsetReadResult {
    pub entries: Vec<(Vec<u8>, Vec<u8>)>,  // (key, value) in directional order
    pub offset_remaining: u64,             // > 0 ⇒ offset ran past the end of the population
}
```

It has to live under `merk/src/merk/` because `Merk::use_tree` is `pub(crate)` and `Merk::source()`
is `pub(in crate::merk)` — the same reason `prove_count_offset.rs` lives there. Same tree-type guard
as the prover (`ProvableCountTree` / `ProvableCountSumTree` / `ProvableCountProvableSumTree`), same
empty-merk behaviour (`use_tree_mut` → `None` ⇒ empty result, full offset remaining).

Values come for free — the node is loaded either way — so the primitive returns them, even though
the current grovedb caller discards them (`Some((secondary_key, _))`).

**GroveDB caller.** `indexed_axis_top_k_paginated_generic` replaces both loops with:

```rust
let read = cost_return_on_error!(&mut cost,
    secondary_merk.read_count_offset_on_range(
        &QueryItem::RangeFull(..), offset, Some(k as u64), !descending, grove_version));
let mut results = Vec::with_capacity(read.entries.len());
for (secondary_key, _) in read.entries {
    match decode(&secondary_key) {
        Some(d) => results.push(d),
        None => return Err(corrupted_secondary_key_error(axis, &secondary_key)).wrap_with_cost(cost),
    }
}
```

`Query`/`KVIterator` disappear from this function. `indexed_axis_top_k_generic` and
`indexed_axis_range_generic` are **untouched** — neither takes an offset, so neither has the bug.

### Ordering and tie-equivalence (the "identical entries, identical ordering" constraint)

- The secondary's merk key **is** the sort key: `axis_sort_bytes ‖ item_key`
  (`make_axis_secondary_key`, `indexed_tree.rs:78`). In-order traversal of the merk = lexicographic
  order = exactly the order `raw_iter` produces. Reverse traversal = `raw_iter` backwards.
- Keys are unique (the item key is a suffix of every secondary key), so the order is **total**.
  "Ties on the axis value" are broken by `item_key` inside the key bytes themselves — there is no
  comparator, no stability question, and nothing for a different traversal to reorder.
- `left_to_right = !descending` in both the old read and the prover, so the offset counts in the
  same directional order in all three paths.

### Why counted-skip == positional-skip here

The counted skip advances by *aggregate count*; the old loop advances by *iteration position*. They
agree iff every entry contributes exactly 1 to the count. In these secondaries it does, by
construction:

- Every secondary is count-bearing: `axis_secondary_tree_type` → `ProvableCountTree` (count axis) or
  `ProvableCountProvableSumTree` (sum, avg) — `indexed_tree.rs:57-70`.
- The only writer is `mirror_indexed_axis_to_secondary` (`indexed_tree.rs:2213`), which writes
  exactly one of `Item(∅)` / `SumItem(sum)` / `ItemWithSumItem(∅, sum)` — never `NonCounted`, never
  a tree, never a reference. Each contributes count = 1.
- `verify_grovedb`'s `verify_indexed_axis_content` (`grovedb/src/lib.rs:1614`) enforces exactly one
  secondary row per primary entry, with that payload, and nothing else in the secondary.

So on any state the DB considers valid, rank == position. §4 covers what happens when it isn't.

## 3. Proof bytes must not move — the argument, and how to test it

This is the load-bearing claim, so I want it argued *and* pinned by a test, not asserted.

### 3.1 The argument

1. Proof bytes are produced by exactly two things: the `ops.push_back(…)` calls in `emit.rs` and the
   `Node` values they carry. The extraction moves **no** `push_back` and constructs **no** `Node`.
   `emit_returned_node`, the `HashWithCount` / `HashWithCountAndSum` / `KVDigestCount` /
   `KVDigestCountSum` construction, and the `Push`/`PushInverted`/`Parent`/`Child` sequencing are
   untouched, character for character.
2. The three extracted functions are pure: no I/O, no interior mutability, no allocation, total over
   their input domain. Their bodies are the existing expressions moved verbatim.
3. Therefore, for every call site, the emitted op is a pure function of (the decision returned, the
   node data read from the tree). If the decision agrees with the branch the old code would have
   taken, the op is identical.
4. So bit-identity reduces to a single finite claim: **the extracted functions agree with the
   pre-extraction branch logic on every input.** That claim is testable exhaustively (§3.2).
5. Verifier, envelope (`proof/indexed_axis/envelope.rs`), and
   `merk_versions.proof.prove_count_offset_on_range` are not touched at all. No version bump.

### 3.2 How we test it

Four layers, cheapest first:

1. **Decision-table equivalence.** In `decide.rs`'s test module, keep a `reference_*` copy of the
   pre-extraction branch logic (literally the `match` blocks as they exist at `a2791bb`) and assert
   equality over the full behavioural domain: `class ∈ {Disjoint, Contained, Boundary}` ×
   `subtree_count ∈ {0,1,2,3}` × `offset_remaining ∈ {0,1,2,3}` × `limit_remaining ∈ {None, Some(0),
   Some(1)}`, and the same for `node_disposition`. A few hundred cases, exhaustive over every
   comparison boundary that matters. Optionally a `proptest` sweep over full `u64`s on top.
2. **Golden proof-byte digests — the real guarantee.** Before touching anything, run a generator on
   the base commit that builds a deterministic fixture corpus and prints `blake3(encode_into(ops))`
   per case; commit those digests as constants and assert them after. Corpus: the existing
   `make_15_key_provable_count_tree` fixture plus a PCPS fixture and a ~1000-key fixture, crossed
   with `offset ∈ {0, 1, 7, 14, 15, 1000}`, `limit ∈ {None, Some(0), Some(1), Some(5)}`,
   `left_to_right ∈ {true, false}`, and each of the three host tree types. `count_offset/tests.rs`
   already has the `encode_proof` helper, so this is a small addition in the right place.
   A digest mismatch fails loudly and names the exact case.
3. **Existing suites unchanged.** `count_offset/tests.rs` (1490 lines) plus
   `grovedb/src/tests/indexed_axis_offset_proof_tests.rs` and `indexed_axis_proof_tests.rs` must pass
   with **zero edits**. Any test needing an edit is a red flag, not a rebase.
4. **Cross-check against the reader.** A differential test asserting the proved path and the new
   unproved path return the same keys in the same order for a randomized corpus — this is what
   catches a divergence in the *decisions* even if the bytes are stable.

If review still judges any edit to `emit.rs` unacceptable, there is a strictly-safer fallback:
**do not touch `emit.rs` at all**, duplicate the two decision blocks in `read.rs`, and add the §3.2.1
table test asserting the two copies agree. Bit-identity then holds by construction (the compiler
sees an unchanged emitter) at the cost of ~25 duplicated lines. I lean to the extraction because
duplicated decision logic silently drifts, but the fallback is a one-line change of plan and I will
take it if the owner prefers it.

## 4. Testable assertion that the skip is counted, not linear

Noted that the executors discard `CostContext` (`cost: _` at `execute_top_k.rs:66/84/102/182`), so
this cannot be asserted Platform-side. It has to be asserted here, and it can be — every grovedb
indexed-tree API returns `CostResult`, so a test can read `OperationCost` directly.

The intent to encode is "**the offset costs at most one descent**", not "it is fast". Proposed
permanent regression test in `grovedb/src/tests/` (new
`indexed_axis_paginated_cost_tests.rs`, matching the existing per-topic test-file convention):

```rust
// N entries, k results. AVL depth is bounded by 1.44·log2(N+2).
let CostContext { value: at_0,   cost: cost_0 } =
    db.indexed_count_top_k_paginated(path, k, 0, false, tx, v);
let CostContext { value: at_far, cost: cost_far } =
    db.indexed_count_top_k_paginated(path, k, N - k as u64, false, tx, v);
at_0.expect("offset 0"); at_far.expect("deep offset");

let depth_bound = (1.44 * ((N + 2) as f64).log2()).ceil() as u64;

// The counted skip pays one root-to-leaf descent for the offset — nothing per skipped entry.
assert!(cost_far.seek_count <= cost_0.seek_count + 2 * depth_bound,
        "offset skip is walking entries: seek_count {} at offset {} vs {} at offset 0 (depth bound {})",
        cost_far.seek_count, N - k as u64, cost_0.seek_count, depth_bound);
assert!(cost_far.storage_loaded_bytes <= cost_0.storage_loaded_bytes.saturating_mul(3) + 4096, …);
```

Properties that make this a real regression test rather than a snapshot:

- **It fails today.** At `N = 10_000, k = 10`, the current loop does ≈ 10_000 iterator steps
  (`seek_count` ≈ N) against a bound of ≈ `cost_0.seek_count + 2·20`. Red before, green after — the
  red run is worth recording in the commit message.
- **It is machine-independent.** `OperationCost` counters only, never wall-clock. E1's own plan says
  the same, for the same reason.
- **It encodes why.** The bound is stated as "offset ≤ one descent", so it keeps failing if someone
  reintroduces any per-entry work in the skip, even a cheap one.

Plus a merk-level twin in `count_offset/tests.rs`: for a 1000-key fixture,
`read_count_offset_on_range(RangeFull, 999, Some(1), …)` must return the last key and touch
`seek_count ≤ some small multiple of the depth` — this pins the primitive independently of grovedb's
wrapper.

And, per the spec's verification plan, an **equality** test across the offset grid × all three axes ×
both directions × tie-heavy populations (many entries sharing an axis value, so the item-key
tiebreak is exercised), asserting the new results equal the pre-fix results element-for-element.

## 5. What could go wrong

Ordered by how much I want the owner to look at it.

1. **Storage rows that are not in the tree ("ghost rows"), and vice versa.** The old read iterates
   *storage*; the new read walks the *tree*. In a drifted secondary these differ — a ghost row is
   returned today and would not be after. This state is corruption by definition
   (`verify_grovedb` flags it; `indexed_tree_secondary_drift_tests.rs` builds it deliberately by
   injecting rows directly into storage), and the **proved** path already walks the tree, so the
   change makes unproved reads agree with proved reads instead of disagreeing. I consider that a
   fix, but it is a returned-value change in a corrupt state and the spec says "cost only", so it
   needs an explicit owner decision.
2. **Corrupt keys in the skipped region stop being detected.** Today every skipped key is decoded and
   a malformed one raises `corrupted_secondary_key_error`. A counted skip never looks at them, so
   that error becomes `Ok`. This is inherent to option A — validating the skipped region *is* the
   linear scan. Returned keys are still validated. Documented, not fixed.
3. **`NonCounted`-wrapped entry in a secondary.** Cannot happen (§2), but if it did, counted skip and
   positional skip would disagree — permanently and invisibly. Proposal: the reader returns
   `Error::CorruptedState` on an in-range node with `own_struct == 0`, mirroring the prover's refusal
   rather than silently dropping the entry. Fail loud over a silent divergence.
4. **Cost profile of the *collect* phase.** The skip gets much cheaper; the k-window changes from a
   sequential `raw_iter` scan to k node fetches through `RefWalker::walk`. For small k (the ranked
   surface's actual use) this is noise; for large k on a cold cache it could regress. Mitigation:
   measure at `k ∈ {1, 10, 100, 1000}` before/after. If it regresses, the fallback is a **hybrid** —
   use a counted descent only to *rank-select* the key at position `offset`, then hand that key to
   the existing `KVIterator` as a directional range start and collect k exactly as today. That keeps
   the collect phase byte-identical to current behaviour and shrinks the diff, at the cost of
   sharing less with the prover. I am not proposing it first because the full walk is what makes
   unproved reads structurally match the proved path.
5. **`value_defined_cost_fn`.** `emit` passes `None` to `walker.walk(..)` even though the secondary
   is opened with `Some(&Element::value_defined_cost_for_serialized_value)`. That affects `KV` cost
   accounting only, not values. I will mirror the prover (pass `None`) so the two walks stay
   comparable, and note it.
6. **Resident memory.** `RefWalker::walk` upgrades `Link::Reference` → `Link::Loaded`, so walked
   nodes stay attached to the in-memory tree. Bounded here at `O(log n + k)` nodes, and the
   secondary `Merk` is opened per query and dropped — same profile as the prover.
7. **Version gating.** `prove_count_offset_on_range` is gated via
   `merk_versions.proof.prove_count_offset_on_range`. A read is not consensus-visible, and adding a
   field to `MerkProofVersions` means touching `v1.rs`–`v4.rs`. My inclination is **not** to gate a
   read-only method and to say so in its doc comment; happy to gate it if the convention is meant to
   be absolute. Owner call — it is cheap either way.
8. **`k = 0` / `offset` past the end / empty secondary.** All three return an empty vec today; the
   walker must too (`limit = Some(0)` ⇒ immediate stop; exhausted population ⇒ empty entries with
   `offset_remaining > 0`; `use_tree` `None` ⇒ empty). Covered by the equality grid.
9. **`u64` offset vs `u16` k.** Unchanged from today; `limit` becomes `Some(k as u64)` exactly as the
   prover already does at `generate.rs:911`.

## 6. Open questions — unanswered, must be settled before implementing

These were put to the owner and overtaken by the decision to ship option C first. Whoever picks this
up needs answers to 1–3 before writing code; 4 is a scope confirmation.

1. **Ghost-row divergence (risk 1)** — accept it (unproved reads start matching proved reads), or
   gate the change so behaviour in a drifted state is bit-preserved?
2. **`emit.rs` edit** — extract the pure decision functions (the recommendation, §1), or zero-diff on
   the emitter with duplicated decision logic plus an agreement test (§3.2 fallback)?
3. **Version gating (risk 7)** — gate `read_count_offset_on_range` behind a new `MerkVersions`
   field, or leave a read-only method ungated?
4. **Scope** — this fixes the paginated read only. `indexed_axis_top_k_generic` and
   `indexed_axis_range_generic` take no offset and are untouched. Confirm they should stay that way
   (recommendation: yes).

### One thing that changed since this was written

By the time this lands, Platform's unproved ranked reads will already be going through
prove-then-verify (option C). So the pre/post equality grid in §4 has a **third** baseline available
and should use it: new counted read == old linear read == prove-then-verify result, same entries,
same order. Three-way agreement is a stronger check than the two-way one originally planned. It is
also worth confirming at that point whether `indexed_axis_top_k_paginated_generic` still has a live
caller, or whether the win is to point Platform's unproved path back at it and retire the
prove-then-verify detour.

## 7. Implementation plan, if picked up

Sequenced so the byte-neutrality of step 2 is provable in isolation:

1. Generate and commit the golden proof-byte digests **on the base commit**, before any change.
   Do not skip this or fold it into step 2 — the digests are only trustworthy if they were produced
   by a binary that predates the extraction.
2. Extract `decide.rs`; rewire `emit.rs`'s three call sites; run the whole merk + grovedb proof
   suite and the golden digests. This step alone must be provably byte-neutral.
3. Add `read.rs` + `Merk::read_count_offset_on_range` with merk-level tests.
4. Rewire `indexed_axis_top_k_paginated_generic`.
5. Add the cost regression test (record its red run) and the pre/post equality grid.
6. Multi-agent review of the diff per the repo's pipeline.
