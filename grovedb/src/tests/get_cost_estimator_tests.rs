//! Tests for get cost estimator functions (average_case and worst_case).

#[cfg(test)]
mod tests {
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{key_info::KeyInfo::KnownKey, KeyInfoPath},
        GroveDb,
    };

    // -----------------------------------------------------------------------
    // Helper: build a simple KeyInfoPath of known keys
    // -----------------------------------------------------------------------
    fn make_path(segments: &[&[u8]]) -> KeyInfoPath {
        KeyInfoPath::from_known_path(segments.iter().copied())
    }

    // =======================================================================
    // Average case: has_raw
    // =======================================================================

    #[test]
    fn average_case_for_has_raw_nonzero_cost() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"mykey".to_vec());

        let cost = GroveDb::average_case_for_has_raw(
            &path,
            &key,
            128,
            TreeType::NormalTree,
            grove_version,
        )
        .expect("average_case_for_has_raw should succeed");

        assert!(
            cost.seek_count > 0,
            "has_raw should require at least one seek"
        );
        assert!(
            cost.storage_loaded_bytes > 0,
            "has_raw should load some bytes from storage"
        );
    }

    // =======================================================================
    // Average case: has_raw_tree
    // =======================================================================

    #[test]
    fn average_case_for_has_raw_tree_nonzero_cost() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"tree_key".to_vec());

        let cost = GroveDb::average_case_for_has_raw_tree(
            &path,
            &key,
            0, // estimated_flags_size
            TreeType::NormalTree,
            TreeType::NormalTree,
            grove_version,
        )
        .expect("average_case_for_has_raw_tree should succeed");

        assert!(
            cost.seek_count > 0,
            "has_raw_tree should require at least one seek"
        );
        assert!(
            cost.storage_loaded_bytes > 0,
            "has_raw_tree should load some bytes from storage"
        );
    }

    // =======================================================================
    // Average case: get_raw
    // =======================================================================

    #[test]
    fn average_case_for_get_raw_nonzero_cost() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"item_key".to_vec());

        let cost = GroveDb::average_case_for_get_raw(
            &path,
            &key,
            256,
            TreeType::NormalTree,
            grove_version,
        )
        .expect("average_case_for_get_raw should succeed");

        assert!(
            cost.seek_count > 0,
            "get_raw should require at least one seek"
        );
        assert!(
            cost.storage_loaded_bytes > 0,
            "get_raw should load some bytes from storage"
        );
    }

    // =======================================================================
    // Average case: get (no references and with references)
    // =======================================================================

    #[test]
    fn average_case_for_get_no_references() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"item_key".to_vec());

        let cost = GroveDb::average_case_for_get(
            &path,
            &key,
            TreeType::NormalTree,
            128,
            vec![], // no references
            grove_version,
        )
        .expect("average_case_for_get with no references should succeed");

        assert!(
            cost.seek_count > 0,
            "get with no references should require at least one seek"
        );
        assert!(
            cost.storage_loaded_bytes > 0,
            "get with no references should load some bytes"
        );
    }

    #[test]
    fn average_case_for_get_with_references_higher_cost() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"ref_key".to_vec());

        let cost_no_refs = GroveDb::average_case_for_get(
            &path,
            &key,
            TreeType::NormalTree,
            128,
            vec![], // no references
            grove_version,
        )
        .expect("average_case_for_get with no references should succeed");

        let cost_with_refs = GroveDb::average_case_for_get(
            &path,
            &key,
            TreeType::NormalTree,
            128,
            vec![64, 64], // two reference hops
            grove_version,
        )
        .expect("average_case_for_get with references should succeed");

        // Following references requires additional seeks and bytes loaded
        assert!(
            cost_with_refs.seek_count > cost_no_refs.seek_count
                || cost_with_refs.storage_loaded_bytes > cost_no_refs.storage_loaded_bytes,
            "get with references should cost more than without: refs={:?}, no_refs={:?}",
            cost_with_refs,
            cost_no_refs,
        );
    }

    // =======================================================================
    // Average case: get_tree
    // =======================================================================

    #[test]
    fn average_case_for_get_tree_nonzero_cost() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"tree_key".to_vec());

        let cost = GroveDb::average_case_for_get_tree(
            &path,
            &key,
            0,
            TreeType::NormalTree,
            TreeType::NormalTree,
            grove_version,
        )
        .expect("average_case_for_get_tree should succeed");

        assert!(
            cost.seek_count > 0,
            "get_tree should require at least one seek"
        );
        assert!(
            cost.storage_loaded_bytes > 0,
            "get_tree should load some bytes from storage"
        );
    }

    // =======================================================================
    // Worst case: has_raw
    // =======================================================================

    #[test]
    fn worst_case_for_has_raw_nonzero_cost() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"mykey".to_vec());

        let cost =
            GroveDb::worst_case_for_has_raw(&path, &key, 128, TreeType::NormalTree, grove_version)
                .expect("worst_case_for_has_raw should succeed");

        assert!(
            cost.seek_count > 0,
            "worst-case has_raw should require at least one seek"
        );
        assert!(
            cost.storage_loaded_bytes > 0,
            "worst-case has_raw should load some bytes"
        );
    }

    // =======================================================================
    // Worst case: get_raw
    // =======================================================================

    #[test]
    fn worst_case_for_get_raw_nonzero_cost() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"item_key".to_vec());

        let cost =
            GroveDb::worst_case_for_get_raw(&path, &key, 256, TreeType::NormalTree, grove_version)
                .expect("worst_case_for_get_raw should succeed");

        assert!(
            cost.seek_count > 0,
            "worst-case get_raw should require at least one seek"
        );
        assert!(
            cost.storage_loaded_bytes > 0,
            "worst-case get_raw should load some bytes"
        );
    }

    // =======================================================================
    // Worst case: get (no references and with references)
    // =======================================================================

    #[test]
    fn worst_case_for_get_no_references() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"item_key".to_vec());

        let cost = GroveDb::worst_case_for_get(
            &path,
            &key,
            128,
            vec![], // no references
            TreeType::NormalTree,
            grove_version,
        )
        .expect("worst_case_for_get with no references should succeed");

        assert!(
            cost.seek_count > 0,
            "worst-case get with no references should require at least one seek"
        );
        assert!(
            cost.storage_loaded_bytes > 0,
            "worst-case get with no references should load some bytes"
        );
    }

    #[test]
    fn worst_case_for_get_with_references_higher_cost() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"ref_key".to_vec());

        let cost_no_refs = GroveDb::worst_case_for_get(
            &path,
            &key,
            128,
            vec![],
            TreeType::NormalTree,
            grove_version,
        )
        .expect("worst_case_for_get with no references should succeed");

        let cost_with_refs = GroveDb::worst_case_for_get(
            &path,
            &key,
            128,
            vec![64, 64],
            TreeType::NormalTree,
            grove_version,
        )
        .expect("worst_case_for_get with references should succeed");

        assert!(
            cost_with_refs.seek_count > cost_no_refs.seek_count
                || cost_with_refs.storage_loaded_bytes > cost_no_refs.storage_loaded_bytes,
            "worst-case get with references should cost more than without: refs={:?}, no_refs={:?}",
            cost_with_refs,
            cost_no_refs,
        );
    }

    // =======================================================================
    // Cross-comparison: worst_case >= average_case
    // =======================================================================

    #[test]
    fn worst_case_costs_gte_average_case() {
        let grove_version = GroveVersion::latest();
        let path = make_path(&[b"root", b"subtree"]);
        let key = KnownKey(b"mykey".to_vec());

        let element_size: u32 = 128;

        // Compare has_raw
        let avg_has_raw = GroveDb::average_case_for_has_raw(
            &path,
            &key,
            element_size,
            TreeType::NormalTree,
            grove_version,
        )
        .expect("average has_raw should succeed");

        let worst_has_raw = GroveDb::worst_case_for_has_raw(
            &path,
            &key,
            element_size,
            TreeType::NormalTree,
            grove_version,
        )
        .expect("worst has_raw should succeed");

        assert!(
            worst_has_raw.seek_count >= avg_has_raw.seek_count,
            "worst-case seek_count should be >= average-case for has_raw: worst={}, avg={}",
            worst_has_raw.seek_count,
            avg_has_raw.seek_count,
        );
        assert!(
            worst_has_raw.storage_loaded_bytes >= avg_has_raw.storage_loaded_bytes,
            "worst-case storage_loaded_bytes should be >= average-case for has_raw: worst={}, avg={}",
            worst_has_raw.storage_loaded_bytes,
            avg_has_raw.storage_loaded_bytes,
        );

        // Compare get_raw
        let avg_get_raw = GroveDb::average_case_for_get_raw(
            &path,
            &key,
            element_size,
            TreeType::NormalTree,
            grove_version,
        )
        .expect("average get_raw should succeed");

        let worst_get_raw = GroveDb::worst_case_for_get_raw(
            &path,
            &key,
            element_size,
            TreeType::NormalTree,
            grove_version,
        )
        .expect("worst get_raw should succeed");

        assert!(
            worst_get_raw.seek_count >= avg_get_raw.seek_count,
            "worst-case seek_count should be >= average-case for get_raw: worst={}, avg={}",
            worst_get_raw.seek_count,
            avg_get_raw.seek_count,
        );
        assert!(
            worst_get_raw.storage_loaded_bytes >= avg_get_raw.storage_loaded_bytes,
            "worst-case storage_loaded_bytes should be >= average-case for get_raw: worst={}, avg={}",
            worst_get_raw.storage_loaded_bytes,
            avg_get_raw.storage_loaded_bytes,
        );

        // Compare get (no references)
        let avg_get = GroveDb::average_case_for_get(
            &path,
            &key,
            TreeType::NormalTree,
            element_size,
            vec![],
            grove_version,
        )
        .expect("average get should succeed");

        let worst_get = GroveDb::worst_case_for_get(
            &path,
            &key,
            element_size,
            vec![],
            TreeType::NormalTree,
            grove_version,
        )
        .expect("worst get should succeed");

        assert!(
            worst_get.seek_count >= avg_get.seek_count,
            "worst-case seek_count should be >= average-case for get: worst={}, avg={}",
            worst_get.seek_count,
            avg_get.seek_count,
        );
        assert!(
            worst_get.storage_loaded_bytes >= avg_get.storage_loaded_bytes,
            "worst-case storage_loaded_bytes should be >= average-case for get: worst={}, avg={}",
            worst_get.storage_loaded_bytes,
            avg_get.storage_loaded_bytes,
        );
    }
}
