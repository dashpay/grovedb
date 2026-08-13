//! End-to-end tests for the branched (multi-prefix) indexed-axis proof
//! envelopes (`prove_indexed_axis_*_branched` /
//! `verify_indexed_axis_*_branched`): one query over N sibling prefix
//! branches, one envelope, one reconstructed root hash.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::proofs::Query as MerkQuery;
    use grovedb_query::QueryItem as MerkQueryItem;
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::indexed_axis::{AxisEntries, IndexedAxisBranchedPaginatedProof},
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb,
    };

    const REGION: &[u8] = b"region";
    const CLS: &[u8] = b"cls";

    /// Build the branched fixture:
    /// `[TEST_LEAF, "region"]` is a plain tree whose children are one
    /// plain value tree per branch key, each holding a PCIT at `"cls"`
    /// seeded with the given `(group key, count)` entries — the same
    /// shape a compound index's prefix level takes.
    fn build_branched_fixture(
        db: &GroveDb,
        grove_version: &GroveVersion,
        branches: &[(&[u8], &[(&[u8], u64)])],
    ) {
        db.insert(
            [TEST_LEAF].as_ref(),
            REGION,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create region tree");
        for (branch_key, entries) in branches {
            db.insert(
                [TEST_LEAF, REGION].as_ref(),
                branch_key,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create branch value tree");
            db.insert(
                [TEST_LEAF, REGION, branch_key].as_ref(),
                CLS,
                Element::empty_provable_count_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create branch PCIT");
            let pcit_path: Vec<&[u8]> = vec![TEST_LEAF, REGION, branch_key, CLS];
            for (group_key, count) in *entries {
                db.insert_into_count_indexed_tree(
                    pcit_path.as_slice(),
                    group_key,
                    Element::empty_provable_count_tree(),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert group tree");
                let mut group_path = pcit_path.clone();
                group_path.push(group_key);
                for i in 0..*count {
                    db.insert(
                        group_path.as_slice(),
                        &i.to_be_bytes(),
                        Element::new_item(b"v".to_vec()),
                        None,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("insert doc");
                }
            }
        }
    }

    fn prefix<'a>() -> Vec<&'a [u8]> {
        vec![TEST_LEAF, REGION]
    }

    fn suffix<'a>() -> Vec<&'a [u8]> {
        vec![CLS]
    }

    fn branch_keys() -> Vec<Vec<u8>> {
        vec![b"east".to_vec(), b"west".to_vec()]
    }

    fn full_range_query() -> MerkQuery {
        let mut q = MerkQuery::new();
        q.insert_item(MerkQueryItem::RangeFull(std::ops::RangeFull));
        q
    }

    fn count_entries(entries: &AxisEntries) -> Vec<(u64, Vec<u8>)> {
        match entries {
            AxisEntries::Count(v) => v.clone(),
            other => panic!("expected count entries, got {other:?}"),
        }
    }

    #[test]
    fn branched_range_query_proves_and_verifies_with_one_root_hash() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_fixture(
            &db,
            grove_version,
            &[
                (b"east", &[(b"math", 3), (b"art", 5)]),
                (b"west", &[(b"science", 2)]),
            ],
        );

        let proof = db
            .prove_indexed_axis_query_branched(
                &prefix(),
                &branch_keys(),
                &suffix(),
                IndexAxis::Count,
                full_range_query(),
                Some(10),
                None,
                grove_version,
            )
            .unwrap()
            .expect("branched range prove succeeds");

        let result = GroveDb::verify_indexed_axis_query_branched(
            &proof,
            &prefix(),
            &branch_keys(),
            &suffix(),
            IndexAxis::Count,
            full_range_query(),
            Some(10),
        )
        .expect("branched range proof verifies");

        assert_eq!(result.branches.len(), 2);
        assert_eq!(
            count_entries(&result.branches[0]),
            vec![(3, b"math".to_vec()), (5, b"art".to_vec())],
            "east's own groups, ascending count order"
        );
        assert_eq!(
            count_entries(&result.branches[1]),
            vec![(2, b"science".to_vec())],
            "west's own groups — no cross-branch leakage"
        );
        assert_eq!(
            result.root_hash,
            db.root_hash(None, grove_version)
                .unwrap()
                .expect("root hash readable"),
            "one root hash for the whole envelope, matching the live grove"
        );
    }

    #[test]
    fn branched_paginated_top_k_proves_and_verifies() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_fixture(
            &db,
            grove_version,
            &[
                (b"east", &[(b"math", 3), (b"art", 5), (b"gym", 1)]),
                (b"west", &[(b"science", 2), (b"history", 7)]),
            ],
        );

        let proof = db
            .prove_indexed_axis_top_k_paginated_branched(
                &prefix(),
                &branch_keys(),
                &suffix(),
                IndexAxis::Count,
                2,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("branched paginated prove succeeds");

        let result = GroveDb::verify_indexed_axis_top_k_paginated_branched(
            &proof,
            &prefix(),
            &branch_keys(),
            &suffix(),
            IndexAxis::Count,
            2,
            0,
            true,
        )
        .expect("branched paginated proof verifies");

        assert_eq!(result.branches.len(), 2);
        let (east_skipped, east_entries) = &result.branches[0];
        assert_eq!(*east_skipped, 0);
        assert_eq!(
            count_entries(east_entries),
            vec![(5, b"art".to_vec()), (3, b"math".to_vec())],
            "east's own top 2 by count, descending"
        );
        let (west_skipped, west_entries) = &result.branches[1];
        assert_eq!(*west_skipped, 0);
        assert_eq!(
            count_entries(west_entries),
            vec![(7, b"history".to_vec()), (2, b"science".to_vec())],
            "west's own top 2 by count, descending"
        );
        assert_eq!(
            result.root_hash,
            db.root_hash(None, grove_version)
                .unwrap()
                .expect("root hash readable"),
        );
    }

    /// The envelope binds branch tails to branch keys through the
    /// multi-key proof's recorded hashes: verifying with the keys in a
    /// different order than the prover's branch alignment must fail,
    /// not silently relabel which prefix each page belongs to.
    #[test]
    fn reordered_branch_keys_do_not_verify() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_fixture(
            &db,
            grove_version,
            &[(b"east", &[(b"math", 3)]), (b"west", &[(b"science", 2)])],
        );
        let proof = db
            .prove_indexed_axis_query_branched(
                &prefix(),
                &branch_keys(),
                &suffix(),
                IndexAxis::Count,
                full_range_query(),
                Some(10),
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove succeeds");

        let reversed: Vec<Vec<u8>> = branch_keys().into_iter().rev().collect();
        assert!(
            GroveDb::verify_indexed_axis_query_branched(
                &proof,
                &prefix(),
                &reversed,
                &suffix(),
                IndexAxis::Count,
                full_range_query(),
                Some(10),
            )
            .is_err(),
            "branch tails must stay bound to their keys"
        );
    }

    /// A branch tail copied over another branch's slot fails the
    /// branching-level composition even though the tail is internally
    /// valid — the multi-key proof commits each key's own subtree.
    #[test]
    fn a_duplicated_branch_tail_does_not_verify() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_fixture(
            &db,
            grove_version,
            &[(b"east", &[(b"math", 3)]), (b"west", &[(b"science", 2)])],
        );
        let proof = db
            .prove_indexed_axis_top_k_paginated_branched(
                &prefix(),
                &branch_keys(),
                &suffix(),
                IndexAxis::Count,
                1,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove succeeds");

        let config = bincode::config::standard();
        let (mut envelope, _): (IndexedAxisBranchedPaginatedProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode own envelope");
        let cloned = IndexedAxisBranchedPaginatedProof {
            axis_tag: envelope.axis_tag,
            shared_layer_proofs: std::mem::take(&mut envelope.shared_layer_proofs),
            shared_ancestor_attestations: std::mem::take(
                &mut envelope.shared_ancestor_attestations,
            ),
            branching_layer_proof: std::mem::take(&mut envelope.branching_layer_proof),
            branches: {
                let mut branches = std::mem::take(&mut envelope.branches);
                let first = branches[0].clone();
                branches[1] = first;
                branches
            },
            requested_k: envelope.requested_k,
            requested_offset: envelope.requested_offset,
            descending: envelope.descending,
        };
        let tampered = bincode::encode_to_vec(&cloned, config).expect("re-encode");
        assert!(
            GroveDb::verify_indexed_axis_top_k_paginated_branched(
                &tampered,
                &prefix(),
                &branch_keys(),
                &suffix(),
                IndexAxis::Count,
                1,
                0,
                true,
            )
            .is_err(),
            "a duplicated branch tail must fail the branching-level composition"
        );
    }

    /// Echo mismatches (k, direction) and a wrong branch count are all
    /// rejected before any hashing happens.
    #[test]
    fn echo_and_branch_count_mismatches_do_not_verify() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_fixture(
            &db,
            grove_version,
            &[(b"east", &[(b"math", 3)]), (b"west", &[(b"science", 2)])],
        );
        let proof = db
            .prove_indexed_axis_top_k_paginated_branched(
                &prefix(),
                &branch_keys(),
                &suffix(),
                IndexAxis::Count,
                2,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove succeeds");

        // Wrong k.
        assert!(GroveDb::verify_indexed_axis_top_k_paginated_branched(
            &proof,
            &prefix(),
            &branch_keys(),
            &suffix(),
            IndexAxis::Count,
            3,
            0,
            true,
        )
        .is_err());
        // Wrong direction.
        assert!(GroveDb::verify_indexed_axis_top_k_paginated_branched(
            &proof,
            &prefix(),
            &branch_keys(),
            &suffix(),
            IndexAxis::Count,
            2,
            0,
            false,
        )
        .is_err());
        // A third key the envelope does not carry.
        let mut three = branch_keys();
        three.push(b"north".to_vec());
        assert!(GroveDb::verify_indexed_axis_top_k_paginated_branched(
            &proof,
            &prefix(),
            &three,
            &suffix(),
            IndexAxis::Count,
            2,
            0,
            true,
        )
        .is_err());
    }

    /// Fewer than two branch keys and duplicate branch keys are
    /// rejected at both ends.
    #[test]
    fn degenerate_branch_key_lists_are_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_fixture(&db, grove_version, &[(b"east", &[(b"math", 3)])]);

        let single = vec![b"east".to_vec()];
        assert!(db
            .prove_indexed_axis_query_branched(
                &prefix(),
                &single,
                &suffix(),
                IndexAxis::Count,
                full_range_query(),
                Some(10),
                None,
                grove_version,
            )
            .unwrap()
            .is_err());

        let duplicated = vec![b"east".to_vec(), b"east".to_vec()];
        assert!(db
            .prove_indexed_axis_query_branched(
                &prefix(),
                &duplicated,
                &suffix(),
                IndexAxis::Count,
                full_range_query(),
                Some(10),
                None,
                grove_version,
            )
            .unwrap()
            .is_err());
    }

    /// An `IN` element whose prefix was never written contributes the
    /// **empty page**, authenticated: the branching-level proof shows
    /// the key absent, the envelope carries no tail for it, and the
    /// present branch still proves normally under the same single root
    /// hash — the union semantics the query surface advertises.
    #[test]
    fn an_absent_branch_key_verifies_as_the_empty_page() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_fixture(&db, grove_version, &[(b"east", &[(b"math", 3)])]);

        let keys = vec![b"east".to_vec(), b"north".to_vec()];
        let proof = db
            .prove_indexed_axis_top_k_paginated_branched(
                &prefix(),
                &keys,
                &suffix(),
                IndexAxis::Count,
                2,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove succeeds with an absent branch");

        let result = GroveDb::verify_indexed_axis_top_k_paginated_branched(
            &proof,
            &prefix(),
            &keys,
            &suffix(),
            IndexAxis::Count,
            2,
            0,
            true,
        )
        .expect("an absent branch verifies as empty");
        assert_eq!(
            count_entries(&result.branches[0].1),
            vec![(3, b"math".to_vec())],
            "the present branch's page is intact"
        );
        assert_eq!(result.branches[1].0, 0, "nothing to skip in an absent tree");
        assert!(
            result.branches[1].1.is_empty(),
            "the absent branch is the empty page"
        );
        assert_eq!(
            result.root_hash,
            db.root_hash(None, grove_version)
                .unwrap()
                .expect("root hash readable"),
        );

        // The range shape has the same contract.
        let range_proof = db
            .prove_indexed_axis_query_branched(
                &prefix(),
                &keys,
                &suffix(),
                IndexAxis::Count,
                full_range_query(),
                Some(10),
                None,
                grove_version,
            )
            .unwrap()
            .expect("range prove succeeds with an absent branch");
        let range_result = GroveDb::verify_indexed_axis_query_branched(
            &range_proof,
            &prefix(),
            &keys,
            &suffix(),
            IndexAxis::Count,
            full_range_query(),
            Some(10),
        )
        .expect("range verify succeeds with an absent branch");
        assert!(range_result.branches[1].is_empty());
    }

    /// Absence forgery fails in both directions: an envelope claiming
    /// absence for a key the branching proof shows present, and an
    /// envelope carrying a tail for a key the proof shows absent.
    #[test]
    fn absence_forgeries_do_not_verify() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_fixture(
            &db,
            grove_version,
            &[(b"east", &[(b"math", 3)]), (b"west", &[(b"science", 2)])],
        );
        let config = bincode::config::standard();

        // Direction 1: both present; claim west absent.
        let both = branch_keys();
        let proof = db
            .prove_indexed_axis_top_k_paginated_branched(
                &prefix(),
                &both,
                &suffix(),
                IndexAxis::Count,
                1,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove succeeds");
        let (mut envelope, _): (IndexedAxisBranchedPaginatedProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode own envelope");
        envelope.branches[1] = None;
        let tampered = bincode::encode_to_vec(&envelope, config).expect("re-encode");
        assert!(
            GroveDb::verify_indexed_axis_top_k_paginated_branched(
                &tampered,
                &prefix(),
                &both,
                &suffix(),
                IndexAxis::Count,
                1,
                0,
                true,
            )
            .is_err(),
            "claiming a present branch absent must not verify"
        );

        // Direction 2: north is absent; graft east's tail onto it.
        let with_absent = vec![b"east".to_vec(), b"north".to_vec()];
        let proof = db
            .prove_indexed_axis_top_k_paginated_branched(
                &prefix(),
                &with_absent,
                &suffix(),
                IndexAxis::Count,
                1,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove succeeds");
        let (mut envelope, _): (IndexedAxisBranchedPaginatedProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode own envelope");
        envelope.branches[1] = envelope.branches[0].clone();
        let tampered = bincode::encode_to_vec(&envelope, config).expect("re-encode");
        assert!(
            GroveDb::verify_indexed_axis_top_k_paginated_branched(
                &tampered,
                &prefix(),
                &with_absent,
                &suffix(),
                IndexAxis::Count,
                1,
                0,
                true,
            )
            .is_err(),
            "a tail grafted onto an absent key must not verify"
        );
    }
}
