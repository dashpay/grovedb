# Changelog

All notable changes to GroveDB will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Bidirectional references (#345): four new `Element` variants —
  `BidirectionalReference` (discriminant 25), `ItemWithBackwardsReferences`
  (26), `SumItemWithBackwardsReferences` (27), and
  `ItemWithSumItemWithBackwardsReferences` (28, the `ItemWithSumItem`
  twin) — plus a
  backward-references subsystem that keeps reference chains consistent:
  updating a referenced element propagates the new hash along every chain,
  and deleting/overwriting it cascades the chains away (each affected
  reference must opt in via `cascade_on_update`). Opt-in per call through
  the new `propagate_backward_references` flag on `InsertOptions` /
  `DeleteOptions`. The referrer list is stored on the element itself under a
  two-layer hash (`combine(inner, backrefs)`), so registering a referrer
  never re-hashes what existing referrers committed to; public reads return
  the stripped element, and proofs authenticate these elements through the
  new `Node::KVBackwardsReferencesValueHash` wire node whose value hash the
  verifier recomputes. Requires `GROVE_V4`; earlier versions, V0 proofs, and
  `Provable*` aggregate parents reject the new variants (fail closed).
  `apply_batch` supports the whole family when the batch opts in via
  `BatchApplyOptions::propagate_backward_references`: a preprocessing pass
  expands the batch into the derived registration/propagation/cascade
  operations the live flagged flow performs (shared semantic core, so batch
  and non-batch execution produce byte-identical root hashes), including
  references whose targets are created in the same batch; conflicting
  combinations (a reference plus its target's deletion, a cascade hitting
  another op's position, `RefreshReference` on a bidirectional reference)
  fail closed. Each backward-references item declares how many referrers it
  accepts (`BackwardReferences::max_incoming`, authenticated with the
  element, at most the `MAX_BACKWARD_REFERENCES` ceiling of 256; the plain
  constructors declare 32, the `_with_capacity` constructors take an explicit
  value): registration past the capacity fails and an update may not lower
  it below the registered referrers. Average/worst-case batch estimation
  charges the derived fan-out on `GROVE_V4` (a written item's declared
  capacity, the ceiling for writes that cannot see the element they
  displace, ≤10-hop chains, 1 referrer per reference) while pre-V4
  estimation stays byte-stable for replay. See
  `adr/bidirectional_references.md`.
- **BREAKING**: Added `add_parent_tree_on_subquery` feature to PathQuery (#379)
  - New field in `Query` struct: `add_parent_tree_on_subquery: bool`
  - When set to `true`, parent tree elements (like CountTree or SumTree) are included in query results when performing subqueries
  - Particularly useful for aggregate trees where you need both the aggregate value and individual elements
  - Requires GroveVersion v2 or higher
  - Updated proof verification logic to handle parent tree inclusion

### Changed
- Updated delete function to include grove_version parameter (#377)
- Adjusted batch size type for better performance (#377)
- Renamed `prove_internal` to `prove_query_non_serialized` for clarity (#373)

### Fixed
- Ordinary (Item / Reference) replacements of a specialized value (a tree or a sum item) are charged from their own serialized bytes on `GROVE_V4` (`merk_versions.tree.put_value: 1`). Before, the node kept the predecessor's fixed `value_defined_cost`, so the replacement was charged as the predecessor's fixed size and the physical-size verification was skipped. Old-value removal accounting and the ordinary -> specialized direction are unchanged; grove v1..v3 keep the legacy charges (#908)
- V1 trunk and branch chunk proof generation reads the chunk, composite-row bindings, and ancestor layers from one snapshot, preventing concurrent commits from producing proofs with mismatched parent and child hashes (follow-up to #919).
- Trunk and branch chunk proofs (`prove_trunk_chunk` / `prove_branch_chunk`) bind every tree and reference row to the root hash on `GROVE_V4` (`proof.chunk_proof_row_binding: 1`). Released versions waived the value-hash check for any row that deserialized as a tree, so the returned tree metadata (type, aggregate count or sum, root key) was unbound and an item could be disguised as a tree under a genuine root hash; reference rows never verified. The prover now emits `KVValueHashFeatureTypeWithChildHash` for those rows and the verifier requires it. Chunks containing an indexed-tree row are refused, since no proof node carries its three-input binding (#859)
- A directly inserted reference whose chain runs back through the position being written is refused with `CyclicReference` on `GROVE_V4` (`add_element_on_transaction: 2`). Overwriting the item terminal of `A -> B` with a reference back to `A` was accepted — the chain resolved against the stale stored item — and committed a cycle that every later `get`, proof and `verify_grovedb` on either key failed on. `apply_batch` already refused it; `GROVE_V3` keeps the legacy outcome for replay
- Direct and batch references to `NonCounted`-wrapped items now commit to identical stored terminal bytes on `GROVE_V4` (`add_element_on_transaction: 2`). `verify_grovedb` and V1 proof generation select the terminal representation matching each reference's stored commitment, preserving older direct-write hashes across upgrades. Offset-paginated proof rows resolved through a reference may surface the wrapped target (`CountOffsetReturnedItem::resolved_from_reference`) (#858)
- Corrected proof verification logic in GroveDb (#371)
- Added ASCII check before appending string to hex display for better visualization (#376)

## Version History

For previous versions, see commit history.
