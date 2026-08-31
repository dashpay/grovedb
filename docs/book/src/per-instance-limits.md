# Per-Instance Query Limits

*Introduced under `GROVE_V4` (`path_query_methods.per_instance_query_limits`).*

`SizedQuery::limit` is one **global** budget shared by a whole query
traversal. That cannot express the most common bounded-fan-out read
there is: *"for each parent, return its first k children"*. It also
made limited path queries unmergeable — a single counter cannot mean
"5 rows from query A plus 7 from query B".

The per-instance limit fixes both. `Query::limit` is a cap carried by a
**query node**, and it is applied **per execution instance**: a query
node runs once for every parent key it is reached under (through a
default or conditional subquery branch), and each of those runs gets a
fresh budget of `limit` result rows for everything originating in that
instance's subtree — its own pushed elements plus all descendant
results.

```rust
// Top 2 documents per owner, at most 100 rows in total.
let mut docs = Query::new_range_full();
docs.limit = Some(2);                       // per-instance: fresh for every owner

let mut owners = Query::new_range_full();
owners.set_subquery(docs);

let query = PathQuery::new(
    vec![b"owners".to_vec()],
    SizedQuery::new(owners, Some(100), None), // global: shared by the whole walk
);
```

## Semantics

**Caps compose by `min`.** An instance's effective budget is the
smaller of its own `Query::limit` and whatever remains of every
enclosing budget — ancestor instances and the global
`SizedQuery::limit`. An ancestor's cap keeps bounding everything below
it: a group capped at 3 returns at most 3 rows even if its children's
caps would allow 6.

**The root node executes exactly once**, so `Query::limit` on the root
query is equivalent to `SizedQuery::limit`; setting both means the
smaller wins. The global limit *is* the level-0 instance limit.

**Rows count; traversal charges don't.** The empty-subtree charge
(`decrease_limit_on_range_with_no_sub_elements` on reads,
`decrease_limit_on_empty_sub_query_result` in proofs) keeps consuming
the **global** budget only — that charge exists to bound walks across
many empty subtrees, while per-instance caps bound result rows. Trusted
reads can opt empty-subtree charges into instance budgets too with
`QueryOptions::decrease_instance_limits_on_range_with_no_sub_elements`
(default `false`; proofs always use the default, since the V1 verifier
runs with `ProveOptions::default()`).

**Offset skips don't consume instance budgets.** A global
`SizedQuery::offset` skips rows before they are pushed; a skipped row
charges no budget, so an instance still returns its full `k` rows after
the skip window passes through it.

**`Some(0)` is malformed.** A node that may select nothing is rejected
(`InvalidQuery`) at every serving entry point, not treated as an empty
result.

## Merging — the lift

Merging limited path queries used to be refused outright. From merge v2
(`path_query_methods.merge = 2`), `PathQuery::merge` merges them by
**lifting**: a merged input's global `SizedQuery::limit` becomes its
branch-root query's `Query::limit`. The lift is exact, because the
branch instance executes exactly once (its path is a concrete key
chain), so "at most N rows from this whole input" and "at most N rows
per instance" coincide. Inputs that already carry per-instance limits
keep them on their branches.

Budgets never blend, so limits merge only as **exclusive grafts**:

- a limited input whose whole path is the common path lands *at the
  merged root*, where its query body would merge with the other inputs'
  — refused with a typed error;
- two limited inputs whose branches collide on the same key — refused;
- limit-free inputs merge exactly as before, on the same code path.

`prove_query_many` therefore serves limited path queries now, and the
verifier re-derives the identical merged query at the same grove
version.

## Proofs

The limit is not part of the proof wire format — like the global limit,
it is an input supplied identically to prover and verifier, changing
only which nodes the prover emits as values versus hashes. The V1
prover and verifier thread a shared global budget plus a frame-local
instance budget per layer; each layer's merk walk runs under
`min(global, instance)`, and the merk-level *"proof returns more data
than limit"* check is what rejects a proof that over-delivers against
an instance cap — a proof built for wider caps does not verify under
tighter ones.

## What fails closed

| Surface | Behavior |
|---|---|
| Grove versions before `GROVE_V4` | `NotSupported` at every read / prove / verify entry point; on the wire, a limit-carrying query encodes its node as `Query` encoding **version 3**, which pre-field decoders reject outright |
| V0 prover / verifier | `NotSupported` at dispatch — V0 is a frozen wire format whose accounting predates instance caps |
| Absence-proof verification | `NotSupported` — the terminal-keys projection reports every expected key the proof did not carry as absent, and which keys an instance-capped walk carries is data-dependent; keys beyond a cap would masquerade as proven-absent |
| `query_keys_optional` / `query_raw_keys_optional` | `NotSupported` — same false-absence hazard as absence proofs |
| Read-mode shapes (axis / sum-budget) | `InvalidQuery` at classify — read modes carry their own entry caps |
| Aggregate leaf/carrier and count-offset-paginated shapes | `InvalidQuery` — those shapes have their own result semantics |
| Colliding or root-landing limited merges | `NotSupported` with a typed error naming the collision |

## Wire encoding

A `Query` node carrying a per-instance limit encodes as version 3:
version byte `3`, then a flags byte (bit 0 = read mode present, bit 1 =
per-instance limit — always set; the encoding is canonical), the
standard fields, then the appended payloads. Every query without a
per-instance limit keeps its historical version-1/2 bytes unchanged, so
existing callers round-trip byte-identically, and decoders that predate
the field fail closed on exactly the queries they cannot execute.

## Known limitation

Parent tree rows added by `add_parent_tree_on_subquery` still do not
count against any budget (the M6 known limitation). Fixing it requires
charging the prover at exactly the points the verifier pushes those
rows, across every descent flavor — tracked as follow-up work.
