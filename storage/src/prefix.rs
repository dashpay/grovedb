//! The prefix that names a subtree.
//!
//! A subtree's prefix is derived from its path, and the prefix of an indexed
//! tree's secondary from the primary's prefix and the axis. The derivation is
//! the same on every storage engine, because state sync chunk ids carry the
//! prefix, and what it charges is a rule of the cost model. So it is a
//! function of this crate and of no engine.

use grovedb_costs::{CostContext, CostsExt};
use grovedb_path::SubtreePath;

use crate::{storage::SubtreePrefix, COST_MODEL};

fn build_prefix_body<B>(path: SubtreePath<B>) -> (Vec<u8>, Vec<u8>)
where
    B: AsRef<[u8]>,
{
    let segments_iter = path.into_reverse_iter();
    let mut res = Vec::new();
    let mut lengths = Vec::new();

    for s in segments_iter {
        res.extend_from_slice(s);
        lengths.push(u8::try_from(s.len()).expect(
            "path segment length must not exceed 255 bytes; \
             this is enforced at insert time via validate_key_length",
        ));
    }

    // Note: this uses native-endian encoding. Changing to big-endian would
    // be a breaking change since the output is hashed with Blake3 to produce
    // subtree prefixes stored in RocksDB — existing databases would become
    // unreadable. In practice GroveDB only targets little-endian platforms
    // (x86_64, aarch64), so this is not a portability concern.
    res.extend(lengths.len().to_ne_bytes());
    res.extend_from_slice(&lengths);
    (res, lengths)
}

/// Builds the prefix that names the subtree at `path`: the prefix of its
/// keys in storage, and its key in a map of subtrees.
pub fn build_prefix<B>(path: SubtreePath<B>) -> CostContext<SubtreePrefix>
where
    B: AsRef<[u8]>,
{
    let (body, segment_lens) = build_prefix_body(path);
    let prefix = if segment_lens.is_empty() {
        SubtreePrefix::default()
    } else {
        SubtreePrefix::from(blake3::hash(&body))
    };
    prefix.wrap_with_cost(COST_MODEL.open_by_path(segment_lens.into_iter().map(usize::from)))
}

/// Derive the per-axis secondary `SubtreePrefix` for any indexed-tree
/// element (`ProvableSumIndexedTree`, `ProvableCountIndexedTree`, or
/// `ProvableCountProvableSumIndexedTree`) whose primary prefix is
/// given and whose axis is identified by `axis_tag`.
///
/// Per S2-B (generalized): `secondary_prefix = Blake3(primary_prefix ‖
/// axis_tag)`. The axis tag byte is the `IndexAxis` value carried by
/// `grovedb-element::indexed::IndexAxis` (`0 = count`, `1 = sum`,
/// `2 = avg`). PCPSIT carries 1..=3 secondaries — one per axis — and
/// each lives at a distinct prefix derived via this function.
///
/// Three useful properties hold:
/// - **Primary parity with `Tree`.** The primary prefix is unchanged
///   from `build_prefix` for the same path, so an indexed-tree's
///   primary Merk lives where a `Tree` would.
/// - **Collision-free secondary.** The secondary's Blake3 input is a
///   33-byte block (prefix ‖ axis_tag) that no path-derived prefix can
///   produce — `build_prefix_body` always ends with per-segment length
///   bytes, never a single tag byte after a 32-byte block.
/// - **Per-axis isolation.** Different `axis_tag` values map the same
///   primary to different storage namespaces, so a PCPSIT's count /
///   sum / avg secondaries do not collide with each other.
///
/// Cost: one Blake3 call (33 bytes fits in a single 64-byte block).
pub fn secondary_prefix_for(
    primary_prefix: &SubtreePrefix,
    axis_tag: u8,
) -> CostContext<SubtreePrefix> {
    // 33 bytes (prefix + tag) fits within one Blake3 block.
    let mut hasher = blake3::Hasher::new();
    hasher.update(primary_prefix);
    hasher.update(&[axis_tag]);
    SubtreePrefix::from(hasher.finalize()).wrap_with_cost(COST_MODEL.secondary_prefix())
}

#[cfg(test)]
mod tests {
    use grovedb_costs::OperationCost;

    use super::*;

    #[test]
    fn the_root_has_the_zero_prefix_and_costs_nothing() {
        let root = build_prefix(SubtreePath::<[u8; 0]>::empty());
        assert_eq!(root.value, SubtreePrefix::default());
        assert_eq!(root.cost, OperationCost::default());
    }

    #[test]
    fn a_prefix_is_the_hash_of_the_segments_from_the_last_with_their_lengths() {
        let path: &[&[u8]] = &[b"ab", b"cde"];
        let mut body = b"cdeab".to_vec();
        body.extend(2usize.to_ne_bytes());
        body.extend([3, 2]);

        let prefix = build_prefix(path.into());
        assert_eq!(prefix.value, *blake3::hash(&body).as_bytes());
        assert_eq!(prefix.cost, COST_MODEL.open_by_path([3, 2]));
        assert_ne!(prefix.cost, OperationCost::default());
    }

    #[test]
    fn a_secondary_prefix_is_the_hash_of_the_primary_and_the_axis_tag() {
        let path: &[&[u8]] = &[b"indexed"];
        let primary = build_prefix(path.into()).value;
        let mut input = primary.to_vec();
        input.push(2);

        let secondary = secondary_prefix_for(&primary, 2);
        assert_eq!(secondary.value, *blake3::hash(&input).as_bytes());
        assert_eq!(secondary.cost, COST_MODEL.secondary_prefix());
        assert_ne!(secondary.value, secondary_prefix_for(&primary, 1).value);
        assert_ne!(secondary.value, primary);
    }

    #[test]
    fn test_build_prefix() {
        let path_a = [b"aa".as_ref(), b"b"];
        let path_b = [b"a".as_ref(), b"ab"];
        assert_ne!(
            build_prefix(path_a.as_ref().into()),
            build_prefix(path_b.as_ref().into()),
        );
        assert_eq!(
            build_prefix(path_a.as_ref().into()),
            build_prefix(path_a.as_ref().into()),
        );
    }

    #[test]
    #[should_panic(expected = "path segment length must not exceed 255 bytes")]
    fn test_build_prefix_rejects_oversized_segment() {
        let oversized_key = vec![0xABu8; 256];
        let path: &[&[u8]] = &[&oversized_key];
        // Previously this would silently truncate the length to 0 (256 as u8 == 0),
        // causing different paths to hash to the same prefix (collision).
        let _ = build_prefix(path.as_ref().into());
    }

    #[test]
    fn secondary_prefix_for_is_deterministic_and_distinct_from_primary() {
        let primary = build_prefix([b"foo".as_ref(), b"bar"].as_ref().into()).unwrap();

        let s1 = secondary_prefix_for(&primary, 0).unwrap();
        let s2 = secondary_prefix_for(&primary, 0).unwrap();
        assert_eq!(s1, s2, "secondary prefix derivation must be deterministic");
        assert_ne!(primary, s1, "secondary prefix must differ from its primary");
    }

    #[test]
    fn secondary_prefix_for_distinguishes_different_primaries() {
        let primary_a = build_prefix([b"foo".as_ref(), b"bar"].as_ref().into()).unwrap();
        let primary_b = build_prefix([b"foo".as_ref(), b"baz"].as_ref().into()).unwrap();

        let s_a = secondary_prefix_for(&primary_a, 0).unwrap();
        let s_b = secondary_prefix_for(&primary_b, 0).unwrap();
        assert_ne!(
            s_a, s_b,
            "different primaries must produce different secondaries"
        );
    }

    #[test]
    fn secondary_prefix_for_handles_empty_path_root_prefix() {
        // A root-level indexed tree has primary_prefix = all-zero default.
        // The derivation must still yield a well-defined 32-byte hash.
        let primary = SubtreePrefix::default();
        let secondary = secondary_prefix_for(&primary, 0).unwrap();
        assert_ne!(secondary, SubtreePrefix::default());
        // And the derivation is stable.
        let secondary_again = secondary_prefix_for(&primary, 0).unwrap();
        assert_eq!(secondary, secondary_again);
    }

    #[test]
    fn secondary_prefix_for_distinguishes_different_axes() {
        // The axis tag byte enters the Blake3 input — different axes for
        // the SAME primary must produce different secondary prefixes so
        // PCPSIT's count / sum / avg secondaries do not collide.
        let primary = build_prefix([b"foo".as_ref(), b"bar"].as_ref().into()).unwrap();
        let s_count = secondary_prefix_for(&primary, 0).unwrap();
        let s_sum = secondary_prefix_for(&primary, 1).unwrap();
        let s_avg = secondary_prefix_for(&primary, 2).unwrap();
        assert_ne!(s_count, s_sum, "count and sum axes must be distinct");
        assert_ne!(s_count, s_avg, "count and avg axes must be distinct");
        assert_ne!(s_sum, s_avg, "sum and avg axes must be distinct");
    }

    #[test]
    fn secondary_prefix_for_does_not_collide_with_path_derived_prefix() {
        // The secondary's Blake3 input is `primary || axis_tag` (33 bytes).
        // Path-derived prefixes hash a variable-length `path_body` that
        // always ends with per-segment-length bytes followed by a single
        // segment-count word — never a single tag byte after a 32-byte
        // block. So a collision would require Blake3 output collision
        // (infeasible) AND is structurally impossible to construct via
        // build_prefix. This test sanity-checks that a few path-derived
        // prefixes do not happen to collide with secondaries.
        let primary = build_prefix([b"a".as_ref(), b"b"].as_ref().into()).unwrap();
        let secondary = secondary_prefix_for(&primary, 0).unwrap();

        for path in [
            [b"a".as_ref(), b"b"].as_ref(),
            [b"x".as_ref(), b"y"].as_ref(),
            [b"foo".as_ref(), b"bar", b"baz"].as_ref(),
            [].as_ref(),
        ] {
            let p = build_prefix(path.into()).unwrap();
            assert_ne!(p, secondary);
        }
    }
}
