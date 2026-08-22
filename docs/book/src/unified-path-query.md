# The Unified PathQuery

GroveDB historically grew several parallel query surfaces: `PathQuery`
for key selection (plus the aggregate-on-range and count-offset shapes
that already ride inside it), `AggregateSumPathQuery` for sum-budget
trusted reads, and the indexed-axis family — roughly two dozen
`prove/verify_indexed_{count,sum,avg}_*` methods taking loose positional
arguments and their own standalone proof envelopes. As of GROVE_V4, one
`PathQuery` expresses **every** query shape, one read entry point
executes it, and one verify entry point checks its proof. The
specialized surfaces all remain available; the unified surface is
additive.

## Read modes

Everything new lives inside `Query`, behind the version byte its manual
encoding already had. A `Query` node gains one optional field:

```rust,ignore
pub struct Query {
    pub items: Vec<QueryItem>,
    pub default_subquery_branch: SubqueryBranch,
    pub conditional_subquery_branches: Option<IndexMap<QueryItem, SubqueryBranch>>,
    pub left_to_right: bool,
    pub add_parent_tree_on_subquery: bool,
    /// None = key selection (all pre-existing behavior). Boxed so the
    /// rare read-mode-bearing node doesn't widen every Query.
    pub read_mode: Option<Box<ReadMode>>,
}

pub enum ReadMode {
    /// Axis-ordered read of the indexed tree this node's path names.
    Axis(AxisQuery),
    /// Key-ordered read stopping on a running-sum budget.
    SumBudget(SumBudgetRead),
}
```

A node without a read mode encodes exactly as before — version byte
`1`, byte-for-byte identical, pinned by golden-byte tests. A node
carrying one bumps **its own** encoding to version `2`, which decoders
that predate read modes reject: an old node fails closed on precisely
the queries it cannot execute, and on nothing else.

`ReadMode::Axis` carries an `AxisQuery`:

```rust,ignore
pub struct AxisQuery {
    pub axis: IndexAxis,          // Count = 0 | Sum = 1 | Avg = 2
    pub traversal: AxisTraversal,
    pub descending: bool,
    pub projection: AxisProjection, // Entries = 0 (default) | Keys = 1
}

pub enum AxisTraversal {
    RankedPage { k: u16, offset: u64 },         // tag 0 (top-k / bottom-k)
    Bounded { lo: i128, hi: i128, limit: u16 }, // tag 1
    RankOfKey { key: Vec<u8> },                 // tag 2
    AggregateOverValueRange {                   // tag 3 (Count/Sum only)
        lo: i128,
        hi: i128,
        fold: AggregateFold,                    // Population = 0 | Total = 1
    },
}
```

`RankedPage` is directional: `descending: true` reads it as top-k,
`false` as bottom-k — one wire shape, both leaderboard ends.

`projection` is an **unproved-read** choice for the two entry-listing
traversals. `Entries` (the default) returns each entry with its
resolved primary value; `Keys` returns the `(ordering_value,
original_key)` pairs straight from the pinned secondary view and never
opens the primary — no primary point reads after the page was
collected (which, through a caller-supplied `None` transaction, would
sit outside the iterator's view), and no reads for values a caller
that only ranks would discard. A proof always carries the values, and
verification yields entries; keys are a strict projection of them, so
the prover and verifier treat a `Keys` query exactly as `Entries`.
`run_path_query` returns `AxisKeys` / `BranchedAxisKeys` for a `Keys`
read.

`AggregateOverValueRange` makes the caller SAY which scalar they mean,
because both readings are meaningful on both axes and the "obvious"
one flips per axis. `[lo, hi]` selects entries by their own axis
value; the fold picks the aggregate over exactly those entries:

| over counts `[3, 1, 5]`, band `[2, 10]` | answer |
|---|---|
| `Population` — how many entries fall in the band | **2** |
| `Total` — the selected values summed | **8** |

Population counts *entries*, not distinct values (two entries sharing
a value are two secondary nodes). Every axis secondary is a
dual-aggregate `ProvableCountProvableSumTree` — the count axis mirrors
each entry's `count_value` into its sum half — which is what makes all
four (axis, fold) cells one committed `O(log n)` scalar, and is also a
**security requirement**: the single-aggregate node hashes share a
preimage layout, so dual-aggregate commitments are what keep a
population proof and a total proof from being confused for one
another.

`ReadMode::SumBudget` carries the stop condition that
`AggregateSumPathQuery` serves: `sum_limit` (a **net** budget — negative
sum items give budget back) and an optional `match_limit` capping how
many sum items may match. All wire tags are frozen.

## The three read-mode shapes

The grammar (enforced by `PathQuery::classify`, below) admits exactly
three placements:

```text
1. Single-path axis read
   path  = [...path to the indexed tree]
   query = Query { read_mode: Axis(axis_query) }        // nothing else

2. Branched axis read — one axis read fanned over sibling branches
   path  = prefix
   query = Query {
       items: [Key(k1), .., Key(kn)],                   // the branches
       default_subquery_branch: {
           subquery_path: Some(suffix),                 // shared, non-empty
           subquery: Query { read_mode: Axis(axis_query) },
       },
   }

3. Sum-budget read
   path  = [...path to the tree holding sum items]
   query = Query { items, left_to_right, read_mode: SumBudget(budget) }
```

The branched shape is the query form of the branched indexed-axis
proof: `prefix / branch_key_i / suffix → axis read`. Because it is
built from ordinary `Key` items and an ordinary `SubqueryBranch`, it
falls out of the machinery that already existed — no new structural
concepts. Constructors cover all of it, so callers never hand-assemble:
`PathQuery::new_axis_top_k`, `new_axis_bounded`, `new_axis_rank_of_key`,
`new_axis_aggregate_over_value_range`, `new_branched_axis`, and
`new_sum_budget` (plus `AxisQuery::bottom_k` for the ascending page).

## One shape decision: `classify()`

```rust,ignore
pub enum PathQueryShape<'q> {
    KeySelection,
    CountOffsetPaginated { inner: &'q QueryItem },
    AggregateLeaf    { kind: AggregateKind, inner: &'q QueryItem },
    AggregateCarrier { kind: AggregateKind, inner: &'q QueryItem },
    AxisRead         { axis: &'q AxisQuery },
    BranchedAxisRead { branch_items: &'q [QueryItem],
                       suffix: &'q [Vec<u8>], axis: &'q AxisQuery },
    SumBudget        { budget: &'q SumBudgetRead, items: &'q [QueryItem] },
}

impl PathQuery {
    pub fn classify(&self) -> Result<PathQueryShape<'_>, Error>;
}
```

`classify()` is **pure** (no database access — a proof verifier, which
holds only the query, classifies identically to the prover), **total**
(every `PathQuery` maps to exactly one shape or to a typed error naming
the violated rule), and it mirrors the prover's historical gate order
for the pre-existing shapes, so migrating call sites keep their exact
error surface. Because prover and verifier must agree on what a query
*means*, classification is consensus-relevant: any change to it belongs
behind a grove-version gate.

The proof walk resolves read modes the same way on both sides through
`PathQuery::axis_read_at_path` / `sum_budget_read_at_path` — one
resolver, so the two sides cannot disagree about which layers are
read-mode layers.

## One read entry point: `run_path_query`

`GroveDb::run_path_query` executes any shape as a trusted read and
returns a typed `PathQueryRun` variant mirroring it: `Elements` for key
selection, the aggregate variants, `AxisEntries` /
`BranchedAxisEntries` / `AxisRank` / `AxisAggregate` for axis reads,
and `SumBudget` for budget walks. Under the hood it routes to the
engine that already serves each shape — `query_raw`, the
`query_aggregate_*` readers, the `indexed_{count,sum,avg}_*`
primitives, the budgeted sum reader — so the unified answer is always
equal to the dedicated entry point's answer (pinned by differential
tests).

Branched reads mirror the proof's absence semantics: a branch key — or
any suffix segment under it — that does not exist yields `None` for
that branch rather than failing the whole read.

## One verify entry point: `verify_path_query`

`GroveDb::verify_path_query(proof, path_query, grove_version)` verifies
any provable shape and returns a typed `VerifiedPathQuery`. Key
selection and the aggregate families route to the existing verifiers
unchanged. The read-mode shapes verify **GroveDBProof V1** envelopes
carrying two new layer kinds.

### Axis descents (`ProofBytes::IndexedTreeAxisDescent`)

When the query node governing an indexed tree carries `ReadMode::Axis`,
the prover emits — in place of the primary descent — a payload holding
a proof over the queried per-axis **secondary**:

```rust,ignore
pub struct AxisDescentProof {
    pub axis_tag: u8,
    pub target_is_pcpsit: bool,
    pub other_axes_root_hashes: Vec<(u8, [u8; 32])>, // PCPSIT only
    pub primary_root_hash: [u8; 32],
    pub rank: Option<u64>,                           // RankOfKey only
    pub secondary_proof: Vec<u8>,
}
```

The payload echoes **no traversal parameters** — the verifier resolves
axis, bounds, direction, caps, and the aggregate **fold** from the
query it independently holds, matching the V1 envelope's
query-as-input philosophy. The one exception is the `RankOfKey` rank,
which must travel to drive the count-offset verification walk; the
count commitments attest it, and the single yielded entry must be the
queried key.

The fold deliberately does not travel either: every secondary's nodes
commit BOTH aggregates, so one descent proof serves the Population and
the Total question alike, each verified against its own hash-bound
number — cross-feeding a proof built for one fold to a query asking
the other yields that question's own correct answer, never a confused
one. (The standalone envelopes, whose verifiers take loose arguments
instead of a query, DO echo the fold and authenticate the echo.)

Where the primary-descent shape supplies its 32-byte secondary-root
attestation raw (safe there because it enters the `combine_hash_three`
preimage), the axis descent **recomputes** it: the verifier checks the
secondary proof for the query's traversal, derives the secondary root
from it, rebuilds the third combine input (the recomputed root for
PCIT/PSIT; the axes digest over the carried other-axes roots plus the
recomputed queried-axis root for PCPSIT, family-checked against
axis-relabel forgery), and requires

```text
combine_hash_three(H(element bytes), primary_root, attestation)
    == the parent-committed value_hash
```

Branched reads need no special envelope: branch keys are `Key` items at
the branching layer (one multi-key Merk proof, absence proven natively
by Merk), each present branch descends the shared suffix to its own
axis-descent terminal, and shared-prefix layers are deduplicated by
`LayerProof` nesting itself. A proof that shows a branch key *present*
while omitting its axis layer is rejected — hiding entries behind fake
absence fails closed — and an axis-read position with a missing lower
layer is a hard error, never a silent absence.

### Sum-budget windows (`ProofBytes::SumBudgetWindow`)

A sum-budget proof is an ordinary Merk proof over exactly the window of
elements the budget walk scanned, plus the window's size and whether
the walk exhausted the ranges:

```rust,ignore
pub struct SumBudgetWindowProof {
    pub exhausted: bool,
    pub window_len: u16,
    pub merk_proof: Vec<u8>,
}
```

The verifier executes the window proof with the query's own items —
limited to the claimed window on a stop, **unlimited on claimed
exhaustion** (so the proof itself must prove the range end) — then
**replays the read engine's fold** element by element: saturating
net-budget subtraction, the per-match limit, and the grove-version
global scan cap. A window that continues past a fired stop, stops short
of one, or misstates exhaustion is rejected, and the verified answer
carries the attested stop reason (`BudgetReached`, `MatchLimitReached`,
`HardScanCapReached`, or `Exhausted`).

The provable fold semantics **skip** non-sum elements and **skip**
references — the two behaviors a single-subtree window proof can replay
deterministically (a reference's target lives outside the window). The
unified trusted read uses the same semantics, so read and verified
results agree over any state. The legacy `AggregateSumPathQuery`
surface keeps its configurable options, including reference following.

## Version gating

Everything new activates at **GROVE_V4** and fails closed below it, on
both sides:

| Gate | Meaning |
| --- | --- |
| `Query` encoding version `2` | Old decoders reject read-mode-bearing queries outright |
| `path_query_methods.unified_read_mode` | Read-mode shapes served by `run_path_query` |
| `proof.axis_descent_in_v1_envelope` | Prover emits / verifier accepts axis descents |
| `proof.sum_budget_in_v1_envelope` | Prover emits / verifier accepts sum-budget windows |
| `path_query_methods.merge` | `PathQuery::merge` requires direction agreement and propagates it (previously input directions were silently dropped) |
| `operations.indexed_axis.*` | The standalone indexed-axis family's own slots (all `0`; first divergence bumps a number instead of forking silently) |

Prover and verifier read the same slots, so there is no version at
which the two sides disagree about whether a shape exists. V0 proofs
are untouched: axis and sum-budget shapes refuse the V0 envelope with
the same contract the aggregate-on-range shapes use.

## Relationship to the specialized surfaces

Every pre-existing surface remains first-class: the
`prove/verify_indexed_*` methods and their standalone echo-based
envelopes, `AggregateSumPathQuery` and its budgeted reader, and the
per-shape `verify_aggregate_*` entry points. The unified entry points
route to the same engines, and where both a standalone envelope and an
embedded V1 proof exist for the same read, tests pin that they yield
identical entries and reconstruct the same root hash. New callers
should prefer `PathQuery` + `run_path_query` + `verify_path_query`; the
specialized surfaces are the engines underneath and the compatibility
surface for existing integrations.

Two things deliberately do **not** merge:

- **Read-mode queries and `PathQuery::merge`.** Merging sibling
  single-path axis reads into the branched shape is expressible, but
  `Query`-level merges reject any input carrying a read mode — the item
  algebra has no semantics for them, and silently merging one as key
  selection would change what the query means. Construct the branched
  shape directly with `new_branched_axis`.
- **Chunk queries.** `PathTrunkChunkQuery` / `PathBranchChunkQuery`
  describe tree-shape transfer for replication, not data selection, and
  stay their own types.
