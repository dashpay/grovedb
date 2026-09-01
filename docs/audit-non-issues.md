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
  `Element` bytes that the proof chain binds to the trusted root, so a forged
  count is ultimately rejected as a bad proof.

  Be precise about *when* that binding happens, though: for the non-Merk
  lower layers it is **after** the arithmetic, not before. `verify.rs:1953-2016`
  dispatches the MMR / bulk-append / dense verifiers using an `Element`
  decoded from the proof's own `value_bytes`, and only at `verify.rs:2018`
  does `combine_hash(value_hash(value_bytes), &lower_hash)` check it against
  the parent-committed hash. A forged count therefore *does* reach the math;
  what makes that safe is that the release build wraps and the chain check
  then rejects the result. Do not lean on "verified before the arithmetic"
  as the invariant — and note that a build with `overflow-checks = true`
  panics instead of wrapping.

Verified-and-rejected instances:

| Issue | Claim | Why unreachable |
|---|---|---|
| [#684](https://github.com/dashpay/grovedb/issues/684) (closed) | Unchecked size arithmetic undercounts storage costs | Overflow needs a single ~4 GiB length; cannot exist under key/value caps. Costs are bit-identical for every input that can occur. Its fix PR [#737](https://github.com/dashpay/grovedb/pull/737) was also closed: it converted `saturating_sub` to erroring `checked_sub` in fee-critical `paid_value_len` arithmetic — a semantic change resting on unverified never-underflows claims, with consensus-grade blast radius under a live protocol version. Do not re-propose checked arithmetic here unless narrowed to addition sites (behavior-identical by construction) and landed at the start of a protocol-version window. |
| [#715](https://github.com/dashpay/grovedb/issues/715) (closed) | `StorageCost::verify` passes after u32 wrap | Same: `added_bytes + replaced_bytes` ≈ 4 GiB required; inputs are real serialized lengths, not attacker-supplied. |
| [#716](https://github.com/dashpay/grovedb/issues/716) (closed) | Sectioned removal totals wrap u32 | Same: one element's removals would have to exceed 4 GiB of actually-stored bytes. |
| [#693](https://github.com/dashpay/grovedb/issues/693) (closed) | Bulk-append `2 * leaf_count` overflows near 2^63 | Append side: 2^63 real appends is physically impossible. Verify side: the release build wraps harmlessly and the forged element is then rejected by the `combine_hash` chain. **Note the ordering** — the element is *not* hash-verified before the arithmetic runs: `verify.rs:1953-2016` dispatches the lower-layer verifiers using an `Element` decoded from the proof's own `value_bytes`, and only at `verify.rs:2018` does `combine_hash(value_hash(value_bytes), &lower_hash)` bind it to the parent-committed hash. So `total_count` reaching `2 * leaf_count - popcount` is unauthenticated at that moment; what saves it is that the wrap produces a value the subsequent chain check rejects, not a prior verification. In a build with `overflow-checks = true` this is a panic instead. |

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
| [#691](https://github.com/dashpay/grovedb/issues/691) (closed) — a dense proof carrying only `node_hashes: [(0, root)]` and no entries verifies against a non-empty tree | The behavior is real but no in-repo verifier accepts it as an absence claim. `verify_for_query` (`grovedb-dense-fixed-sized-merkle-tree/src/verify.rs:86`) derives `expected_positions` from the query and errors on any missing position (`:108`); GroveDB uses exactly this when `report_contents == true` (`proof/verify.rs:2669`). When `report_contents == false` (`verify.rs:2644`) the entries are discarded and the root is forced to the parent-committed child hash by `combine_hash` (`verify.rs:2018`). The BulkAppend/CommitmentTree path calls the unbound `verify_and_get_root`, but the prover-chosen `dense_root` must satisfy `compute_state_root` against the trusted root (`grovedb-bulk-append-tree/src/proof/mod.rs:517`), and both `verify_against_query` (`:592-610`) and `verify_bulk_append_lower_layer` (`proof/verify.rs:2498-2515`) run their own missing-position check. The sibling bypasses from the same audit batch were already pinned by `test_vuln1_node_hashes_{root,ancestor}_bypass_rejected` (`tests.rs:247,270`), which date to the crate's first commit `6e4855f6` (2026-02-23) and so predate the filing. |

**Becomes real if:** a new write path bypasses the 255-byte key check, a
verifier starts trusting a decoded field instead of the hash-verified
element bytes, or a consumer calls the dense crate's
`verify_against_expected_root` / `verify_and_get_root` directly and treats
"verified, zero entries" as proof of absence without a completeness check.

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

## Trust-boundary findings: corrupt storage and caller-chosen paths

GroveDB's trust boundary puts local storage *inside* it. Findings whose
trigger is "corrupt local storage" or "the caller passed a bad argument"
are not vulnerabilities — integrity auditing is delegated to
`Merk::verify` and `GroveDb::verify_grovedb` on purpose.

| Issue | Claim | Why not real |
|---|---|---|
| [#682](https://github.com/dashpay/grovedb/issues/682) (closed) | Lazy-loaded Merk references do not validate fetched node metadata | Accurate as described — `TreeNode::load` (`merk/src/tree/mod.rs:1570-1614`) copies the parent link's `hash`, `child_heights` and `aggregate_data` verbatim without verifying them, and `Walker::detach` (`merk/src/tree/walk/mod.rs:80-95`) discards the link before fetching, so the apply path checks nothing. But the trigger is corrupt storage or a hostile `Fetch` impl, and `Fetch` has exactly two in-tree implementations (`MerkSource`, `PanicSource`). No untrusted input reaches `load`; proofs over a corrupt child fail verifier-side anyway because the verifier recomputes `kv_hash` from the transmitted value. **Do not "fix" this without weighing cost:** validating `hash_for_link` on every link load adds `hash_node_calls` to `OperationCost` on the hot read path, and cost changes are replay-critical (Platform replays historical blocks using the estimate as an admission bound). Separately, the `debug_assert_eq!(tree.key(), link.key())` at `merk/src/tree/mod.rs:1606` is tautological and can never fire — `TreeNode::decode` overwrites the decoded key with the lookup key (`merk/src/tree/encoding.rs:136-137`) — so it is dead weight that reads as a safety check. |
| [#700](https://github.com/dashpay/grovedb/issues/700) (closed) | `delete_checkpoint` can delete non-checkpoint GroveDB directories | Technically accurate — a checkpoint dir is structurally identical to a normal RocksDB dir, so `open_checkpoint` cannot distinguish them, and the `path.components().count() < 2` guard admits `/tmp`. But there is no confused deputy: the only input is a path the caller chose, and the only call sites are tests (`checkpoint_tests.rs:211`, `misc_coverage_tests.rs:1821,1832`). A live DB is protected by RocksDB's LOCK, so only closed DBs are reachable, and only by a caller who typed the path. Side effect worth knowing: `open_checkpoint` on a real DB dir mutates it (WAL replay / MANIFEST churn) before the delete decision. |

**Becomes real if:** a `Fetch` implementation is exposed to untrusted data
(e.g. a network-backed source), or grovedb itself calls `delete_checkpoint`
with a path derived from configuration or network input rather than from an
immediate caller.

## Version-gating and build hygiene reported as vulnerabilities

| Issue | Claim | Why not real |
|---|---|---|
| [#702](https://github.com/dashpay/grovedb/issues/702) (closed) | `GroveVersion::default` creates protocol version 0, satisfying v0 gates | Inert on two independent grounds. (1) `protocol_version` is **never read by any logic in the workspace** — grep finds it only in the four version-constant definitions and in test assertions; every gate reads a *feature* slot such as `grove_version.grovedb_versions.operations.insert.*`. (2) `GroveVersion::default()` is behaviorally identical to `GROVE_V1`: every `FeatureVersion` in V1 is `0`, and the only non-zero values in `v1.rs` are `protocol_version: 1` (never read) and `max_aggregate_sum_query_elements_scanned: 1024`, which is not a version slot and whose hand-written `impl Default` returns the same 1024. So the worst outcome is "the caller got V1 behavior", which is already legal via an explicit `GROVE_V1`. |
| [#703](https://github.com/dashpay/grovedb/issues/703) (closed) | Public versioned APIs miss explicit version gates and could execute writes instead of returning `VersionError` | Three of the seven cited refs are stale line numbers resolving to `root_key`, `root_hash` and `verify_grovedb` — two accessors and a verification helper, none version-dependent, none a write path; gating them would break callers on any unrelated slot bump. The four typed non-Merk append entry points (`mmr_tree_append`, `bulk_append`, `dense_tree_insert`, `commitment_tree_insert`) do lack a *top-level* gate but already fail closed deeper and **before any mutation**, via their cost dispatchers (`grovedb-merkle-mountain-range/src/cost/mod.rs:88-98`, `grovedb-bulk-append-tree/src/cost/mod.rs:182-201`, `grovedb-dense-fixed-sized-merkle-tree/src/tree/root_maintenance/mod.rs:56-71`, `grovedb-commitment-tree/src/commitment_tree/cost/mod.rs:43,73`). Writes also land in a `StorageBatch` committed only on success. The genuine residual is narrower and different: those four types have no `element_creation` gate of the kind `PrivateDocumentStore` uses (`grovedb_versions.rs:180-186`, enabled only in `v4.rs:407`) — a design-consistency question about when they become *creatable*, worth filing separately if wanted. |
| [#721](https://github.com/dashpay/grovedb/issues/721) (closed) | `grovedbg` build script downloads a release artifact at build time | Acceptable posture. Gated off by default (`default = ["full", "estimated_costs"]`; with `grovedbg` off, `build.rs` compiles to a literal no-op and `reqwest`/`sha2` are not even resolved), and integrity is enforced by a pinned version tag plus a pinned `GROVEDBG_SHA256` asserted at `build.rs:33`. A substituted, tampered, or 404 artifact fails the build — fail-closed. Real but minor DX defect not named in the issue: the `if !grovedbg_zip_path.exists()` guard at `:15` caches a bad download, so every later rebuild fails the SHA assert without re-fetching until `target/` is cleared. |

**Becomes real if:** `protocol_version` gains a reader that dispatches
behavior on it; one of the four append paths gains a mutation that runs
before its cost dispatcher; or `grovedbg` is added to the default feature
set or its SHA-256 pin is dropped.
