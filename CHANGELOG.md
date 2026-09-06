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
- V4 trunk and branch chunk proof generation reads the chunk, composite-row bindings, and ancestor layers from one snapshot, preventing concurrent commits from producing proofs with mismatched parent and child hashes (follow-up to #919).
- Trunk and branch chunk proofs (`prove_trunk_chunk` / `prove_branch_chunk`) bind every tree and reference row to the root hash on `GROVE_V4` (`proof.chunk_proof_row_binding: 1`). Released versions waived the value-hash check for any row that deserialized as a tree, so the returned tree metadata (type, aggregate count or sum, root key) was unbound and an item could be disguised as a tree under a genuine root hash; reference rows never verified. The prover now emits `KVValueHashFeatureTypeWithChildHash` for those rows and the verifier requires it. Chunks containing an indexed-tree row are refused, since no proof node carries its three-input binding (#859)
- Direct and batch references to `NonCounted`-wrapped items now commit to identical stored terminal bytes on `GROVE_V4` (`add_element_on_transaction: 2`). `verify_grovedb` and V1 proof generation select the terminal representation matching each reference's stored commitment, preserving older direct-write hashes across upgrades. Offset-paginated proof rows resolved through a reference may surface the wrapped target (`CountOffsetReturnedItem::resolved_from_reference`) (#858)
- Corrected proof verification logic in GroveDb (#371)
- Added ASCII check before appending string to hex display for better visualization (#376)

## Version History

For previous versions, see commit history.
