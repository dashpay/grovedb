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
- A reference whose terminal is a `NonCounted`-wrapped item now commits to the terminal's stored bytes (wrapper included) on every path. Direct insert (`GROVE_V4`+, `add_element_on_transaction: 2`), `verify_grovedb` and the V1 prover use the new commitment-preserving `follow_reference_as_stored`, matching what the batch resolver always hashed; `follow_reference` stays the looked-through presentation read. Offset-paginated proof rows resolved through a reference may surface the wrapped target (`CountOffsetReturnedItem::resolved_from_reference`) (#858)
- Corrected proof verification logic in GroveDb (#371)
- Added ASCII check before appending string to hex display for better visualization (#376)

## Version History

For previous versions, see commit history.
