# Changelog

All notable changes to GroveDB will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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
- An add-on operation returned by a partial batch's callback (`apply_partial_batch*`) no longer discards the ancestor update the paused batch is still carrying, on `GROVE_V4` (`apply_batch.add_on_op_collision: 1`). The leftover root-level `ReplaceTreeRootKey` / `InsertTreeWithRootHash` for a modified child tree was silently replaced by an add-on op at the same path and key, so the child's writes were committed but its parent element was rewritten from the callback's bytes with no root key and a stale aggregate, orphaning the subtree. A colliding add-on insert is now merged with the pending root state the same way in-batch inserts are during propagation, a colliding delete is accepted only if the child ended the batch empty, a collision with a pending non-Merk root update is refused with `InvalidBatchOperation`, and an add-on op duplicating a still-unexecuted user op of the initial batch is refused by the consistency check (last op wins when that check is disabled, as within a single batch). `GROVE_V3` keeps the legacy overwrite (#708)
- Ordinary (Item / Reference) replacements of a specialized value (a tree or a sum item) are charged from their own serialized bytes on `GROVE_V4` (`merk_versions.tree.put_value: 1`). Before, the node kept the predecessor's fixed `value_defined_cost`, so the replacement was charged as the predecessor's fixed size and the physical-size verification was skipped. Old-value removal accounting and the ordinary -> specialized direction are unchanged; grove v1..v3 keep the legacy charges (#908)
- V1 trunk and branch chunk proof generation reads the chunk, composite-row bindings, and ancestor layers from one snapshot, preventing concurrent commits from producing proofs with mismatched parent and child hashes (follow-up to #919).
- Trunk and branch chunk proofs (`prove_trunk_chunk` / `prove_branch_chunk`) bind every tree and reference row to the root hash on `GROVE_V4` (`proof.chunk_proof_row_binding: 1`). Released versions waived the value-hash check for any row that deserialized as a tree, so the returned tree metadata (type, aggregate count or sum, root key) was unbound and an item could be disguised as a tree under a genuine root hash; reference rows never verified. The prover now emits `KVValueHashFeatureTypeWithChildHash` for those rows and the verifier requires it. Chunks containing an indexed-tree row are refused, since no proof node carries its three-input binding (#859)
- A directly inserted reference whose chain runs back through the position being written is refused with `CyclicReference` on `GROVE_V4` (`add_element_on_transaction: 2`). Overwriting the item terminal of `A -> B` with a reference back to `A` was accepted — the chain resolved against the stale stored item — and committed a cycle that every later `get`, proof and `verify_grovedb` on either key failed on. `apply_batch` already refused it; `GROVE_V3` keeps the legacy outcome for replay
- Direct and batch references to `NonCounted`-wrapped items now commit to identical stored terminal bytes on `GROVE_V4` (`add_element_on_transaction: 2`). `verify_grovedb` and V1 proof generation select the terminal representation matching each reference's stored commitment, preserving older direct-write hashes across upgrades. Offset-paginated proof rows resolved through a reference may surface the wrapped target (`CountOffsetReturnedItem::resolved_from_reference`) (#858)
- Corrected proof verification logic in GroveDb (#371)
- Added ASCII check before appending string to hex display for better visualization (#376)

## Version History

For previous versions, see commit history.
