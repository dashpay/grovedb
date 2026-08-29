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
  verifier recomputes. Requires `GROVE_V4`; earlier versions, V0 proofs,
  `Provable*` aggregate parents, and all batch entry points reject the new
  variants (fail closed). See `adr/bidirectional_references.md`.
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
- Corrected proof verification logic in GroveDb (#371)
- Added ASCII check before appending string to hex display for better visualization (#376)

## Version History

For previous versions, see commit history.
