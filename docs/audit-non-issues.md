# Audit non-issues: verified unreachable or by-design

This file records findings that security audits have raised (or are likely to
raise) against GroveDB and that were **adversarially verified and rejected**.
If you are running an audit — human or automated — check this list before
filing. Re-filing one of these without new information wastes a triage cycle.

Each entry states the claim, why it is not a real issue, and what would have
to change for it to become one. If the "becomes real if" condition holds,
file away — that is a new finding, not a re-report.

## How to use this file in an audit

1. Match your candidate finding against the entries below (grep for the
   function or file name).
2. If it matches and the "becomes real if" condition does not hold, drop it.
3. If you believe the invariant that makes it safe has been broken, file the
   issue and cite this file — say explicitly which invariant no longer holds.

---

## Unreachable arithmetic overflow (magnitude class)

The recurring pattern: an audit finds unchecked `u32`/`u64` arithmetic on
byte counts or element counts and reports wraparound. These are not reachable
because the inputs are derived from **real stored bytes or hash-committed
counts**, both of which are capped far below the wrap point:

- Key lengths are capped at 255 bytes by every public insert path (direct and
  batch) — enforced since PR #506.
- Value/element sizes are capped orders of magnitude below 4 GiB by platform
  state-transition and document size limits.
- Tree element counts are accumulated one append at a time and committed into
  hash-verified `Element` bytes; a verifier reads the count from bytes that
  are checked against the trusted root **before** any arithmetic runs, so a
  forged count is rejected as a bad proof, not fed into the math.

Verified-and-rejected instances:

| Issue | Claim | Why unreachable |
|---|---|---|
| [#684](https://github.com/dashpay/grovedb/issues/684) (closed) | Unchecked size arithmetic undercounts storage costs | Overflow needs a single ~4 GiB length; cannot exist under key/value caps. Costs are bit-identical for every input that can occur. Its fix PR [#737](https://github.com/dashpay/grovedb/pull/737) was also closed: it converted `saturating_sub` to erroring `checked_sub` in fee-critical `paid_value_len` arithmetic — a semantic change resting on unverified never-underflows claims, with consensus-grade blast radius under a live protocol version. Do not re-propose checked arithmetic here unless narrowed to addition sites (behavior-identical by construction) and landed at the start of a protocol-version window. |
| [#715](https://github.com/dashpay/grovedb/issues/715) (closed) | `StorageCost::verify` passes after u32 wrap | Same: `added_bytes + replaced_bytes` ≈ 4 GiB required; inputs are real serialized lengths, not attacker-supplied. |
| [#716](https://github.com/dashpay/grovedb/issues/716) (closed) | Sectioned removal totals wrap u32 | Same: one element's removals would have to exceed 4 GiB of actually-stored bytes. |
| [#693](https://github.com/dashpay/grovedb/issues/693) (closed) | Bulk-append `2 * leaf_count` overflows near 2^63 | Append side: 2^63 real appends is physically impossible. Verify side: `total_count` comes from hash-verified `Element::BulkAppendTree(count, ...)` bytes — a forged count breaks the parent hash before the arithmetic runs. |

**Becomes real if:** a code path feeds these computations an integer that is
neither derived from actually-stored bytes nor hash-verified before use
(e.g. a length decoded from an untrusted proof or network message and used
in arithmetic before verification).

## Capacity-boundary behavior on impossible tree sizes

| Issue | Claim | Why not real |
|---|---|---|
| [#694](https://github.com/dashpay/grovedb/issues/694) (closed) | `CommitmentTree::append_raw` mutates bulk storage before `TreeFull` is returned | (1) `TreeFull` fires at 2^32 leaves — 4.3 billion commitments in one tree. (2) Even at the boundary, the bulk write goes into the op's storage batch, which is committed only if the op succeeds; on error the transaction discards the batch. Rollback is the default, not the caller's job. |

**Becomes real if:** commitment-tree appends stop going through the
transactional storage-batch flow, or a caller commits a batch after
observing an append error.

## By-design behavior reported as bugs

| Issue | Claim | Design rationale |
|---|---|---|
| [#697](https://github.com/dashpay/grovedb/issues/697) (closed) | Replication reads are not snapshot-isolated | Replication is designed to serve snapshots **from a checkpoint**, not from a live DB. Snapshot isolation is delegated to checkpointing on purpose. Pre-ship state-sync robustness work is tracked in #679/#695/#704/#706. |
| [#699](https://github.com/dashpay/grovedb/issues/699) (closed) | Checkpoints copy unbounded WALs (`u64::MAX` flush threshold) | Deliberate fast-checkpoint tradeoff: bounded checkpoint latency in exchange for larger checkpoint dirs and WAL replay on open. No data loss either way. Reopen only with a concrete operational repro. |
| [#711](https://github.com/dashpay/grovedb/issues/711) (closed) | `continue_from_ops` underreports accumulated cost on error paths | Only fires on **rejected** ops and is deterministic — every node computes the identical value, so no divergence and no credit inflation. Wontfix; a tidy-up returning accumulated cost is welcome but not a bug. |
| `clear_subtree` returns `Result`, not `CostResult` | Costs are "dropped" | Intentional: `clear_subtree` discards costs by design. |
| `Link::Modified` panics in `hash()` / `aggregate_data()` | Panic reachable | Intentional invariant enforcement: `Link::Modified` must never have these called; the panic is the contract, documented in code comments. |

## Verified-unreachable proof/encoding findings

| Claim | Why not real |
|---|---|
| Proof encoding truncates keys ≥ 256 bytes | Unreachable: every public insert path (direct + batch) enforces the 255-byte key limit (PR #506), so oversized keys cannot reach proof encoding. The `debug_assert!` is sufficient. Raw-Merk hardening exists separately (#728). |
| `feature_type` forgery in `KVValueHashFeatureType` proof nodes | The decoded `_feature_type` is discarded by the verifier; the canonical type/sum/count lives in the hash-verified `Element` bytes. Forged values never reach callers. Documented in `verify.rs` comments and `proof_exploit_tests.rs`. |
| `saturating_sub` on negative `SumItem` values corrupts `sum_limit` | Correct as written: `sum_limit` tracks the remaining **net sum** budget; +7 and −4 must consume 3, not 11. Absolute-value math would be the bug. |

**Becomes real if:** a new write path bypasses the 255-byte key check, or a
verifier starts trusting a decoded field instead of the hash-verified
element bytes.

## Proof-envelope trailing bytes: strict rejection is canonical, do not re-gate leniency

Two related claim shapes, both resolved:

| Claim | Why not real |
|---|---|
| "Proof envelopes accept trailing bytes after the encoded proof (malleability)" | Already fixed. PR [#661](https://github.com/dashpay/grovedb/pull/661) (May 2026) introduced `decode_grovedb_proof_canonical`, which errors unless bincode consumes the full buffer. Every envelope entry point routes through it (`verify_query`, `verify_query_raw`, `verify_query_with_options`, trunk-chunk, `verify_path_query`, all aggregate verifiers), and the newer V1 payload decodes (sum-budget window, axis descent, indexed-axis) carry their own trailing-byte checks. Rejection is version-independent (verified empirically under GROVE_V1–V4) and covered by 10 `trailing_bytes` regression tests. |
| "Verifier strictness is consensus-behavior-changing, so rejection must be version-gated: legacy versions accept, new versions reject" (PR [#739](https://github.com/dashpay/grovedb/pull/739) shape) | The gating premise is false: **there was never a lenient GROVE_V3 verifier in production.** grovedb v4.1.0 — the last release that accepted trailing bytes — does not contain GROVE_V3 at all, so v3 could not activate on it. Every Dash Platform v4.0.0 build from beta.1 (2026-06-02) onward pins grovedb revs that include the strict decode (platform v4.0.0 final → grovedb v5.0.0; platform v4.1.1 → v5.0.1). GROVE_V3 activated ~mid-June 2026 on one of those strict builds, so strict rejection has been the live v3 behavior since the first v3 block. Re-introducing acceptance for v1/v2/v3 would loosen live verifiers and create exactly the intra-version divergence a gate exists to prevent. v1/v2 proofs are only verified by current-code clients, which have been strict since platform v4.0.0 shipped with no fallout. |

**Becomes real if:** evidence emerges that some production verifier ran a
lenient grovedb (< v5.0.0) under GROVE_V3, or a consumer surfaces with
persisted/padded proof blobs that must keep verifying. Absent that, do not
re-propose version-gated leniency for proof-envelope trailing bytes.
