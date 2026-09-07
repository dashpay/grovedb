# Query validation and compatibility boundaries

`PathQuery::classify()` owns the syntactic grammar. Internal
`ValidatedPathQuery` borrows the query, retains its `PathQueryShape` (including
aggregate kind and inner range), and applies the consumer's version and
envelope gates. `run_path_query`, `prove_query`, and `verify_path_query` dispatch
from that decision. The V1 prover carries it through recursion, including the
aggregate terminal depth and empty aggregate descents.

Validation does not normalize direction, rewrite pagination, serialize data,
open trees, or charge costs. Adding a shape belongs in the classifier and its
version/envelope policy, followed by execution arms in the three consumers.
The public query representation, query encoding, proof encoding, and V0
walkers are unchanged.

## Checks that require execution or proof contents

- Aggregate targets must have the appropriate provable tree features. Empty
  eligible trees return zero; a missing path is a different case.
- Count-offset proofs require an eligible count-bearing target. Generation
  still opens that target before rejecting a zero limit, preserving error
  precedence and operation costs.
- Axis layers must occur at the queried paths and match their traversal.
  Missing branches require authenticated absence; a single-path axis read
  requires an actual axis descent.
- Proof chains, aggregate commitments, succinctness, and absence are verified
  by the existing walkers.

## Result-specific APIs remain compatibility adapters

The older APIs cannot all adopt the unified grammar without changing their
contracts. They keep their existing validators and result projections. These
are boundaries for a later selection/aggregation/pagination redesign:

| API or behavior | Existing contract |
| --- | --- |
| `query_raw` and ordinary element reads | Permit offsets over keys, multiple ranges, and ordinary trees. A nonzero offset in the unified grammar instead requires the count-offset proof shape. Element APIs reject read modes. |
| Scalar `query_aggregate_*` / `verify_aggregate_*_query` | Require a leaf and return one scalar. Carrier queries need the per-key APIs or the unified dispatch. |
| Per-key aggregate APIs | Accept leaves as a singleton with an empty key, and carriers as one result per matched outer key. Missing outer keys are omitted; empty aggregate targets return zero. |
| Absence read/verify APIs | Require an explicit limit and project through `terminal_keys`. Their ordering follows that projection, which can differ from ordinary descending element results. Absence verification rejects a nonzero offset: a count-offset proof does not reveal which rows the offset skipped, so the projection cannot tell a skipped row from an absent key. |
| Optional-key reads | Reject an explicitly supplied offset, including `Some(0)`. Ordinary selection and proof generation treat `Some(0)` as no offset. |
| Parent-info verification | Rejects subqueries. Offsets follow the same envelope gate as every other proof verifier (V0 rejects a nonzero offset, V1 serves the count-offset shape, `Some(0)` is no offset). |
| Subset verification | Allows extra proof data, but does not universally permit adding a smaller result limit to a proof generated without that limit. Selection and pagination are still coupled. |
| V0 proof generation | Rejects a nonzero offset before inspecting pagination syntax, after aggregate/read-mode validation. |
| Legacy V0 element verification | Rejects a nonzero offset by envelope before pagination syntax; its error type differs from generation. |
| Aggregate/read-mode envelopes | Require V1. Axis and sum-budget capabilities additionally have separate version gates. Trusted aggregates retain their historical availability independently of envelope support. |

Compatibility tests compare reads with verified results where these contracts
match. They also cover conflicting shapes before storage/decode, descending
merged and subset queries, and absence results across all shipped versions.
Query/proof byte digests and operation-cost fixtures were captured from
`2fa0f133877420a0d9c91ba7bc51b1775ab8c783` before this refactor, using the same
dependency lockfile.
