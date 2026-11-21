//! The prefix that names a subtree, charged by the model's rules.
//!
//! The real crate's `prefix` module has the same two functions. They are
//! written out here so that what a derivation charges comes from
//! [`rules`](crate::cost_model::rules) and not from the rules it is
//! checked against.

use grovedb_costs::{CostContext, CostsExt};
use grovedb_path::SubtreePath;

use crate::{cost_model::rules, storage::SubtreePrefix};

/// The prefix of the subtree at `path`: the Blake3 hash of its
/// segments from the last to the first, their number as a machine word,
/// and the length of each as a byte. Charged by rule P1.
pub fn build_prefix<B>(path: SubtreePath<B>) -> CostContext<SubtreePrefix>
where
    B: AsRef<[u8]>,
{
    let mut body = Vec::new();
    let mut lengths = Vec::new();
    for segment in path.into_reverse_iter() {
        body.extend_from_slice(segment);
        lengths.push(u8::try_from(segment.len()).expect(
            "path segment length must not exceed 255 bytes; this is enforced at insert time via \
             validate_key_length",
        ));
    }
    let cost = rules::open_by_path(lengths.iter().map(|len| usize::from(*len)));
    if lengths.is_empty() {
        return SubtreePrefix::default().wrap_with_cost(cost);
    }
    body.extend(lengths.len().to_ne_bytes());
    body.extend(lengths);
    SubtreePrefix::from(blake3::hash(&body)).wrap_with_cost(cost)
}

/// The prefix of the secondary of an indexed tree: the Blake3 hash of
/// the primary prefix and the axis tag. Charged by rule P3.
pub fn secondary_prefix_for(
    primary_prefix: &SubtreePrefix,
    axis_tag: u8,
) -> CostContext<SubtreePrefix> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(primary_prefix);
    hasher.update(&[axis_tag]);
    SubtreePrefix::from(hasher.finalize()).wrap_with_cost(rules::secondary_prefix())
}
