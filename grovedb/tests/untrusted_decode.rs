//! Client-visible query/proof types compose in an untrusted-only request.
#![cfg(any(feature = "minimal", feature = "verify"))]

use bincode::{BorrowDecodeUntrusted, Decode, DecodeUntrusted, Encode};
use grovedb::{
    operations::proof::{
        indexed_axis::{
            AncestorAttestation, IndexedAxisAggregateProof, IndexedAxisPaginatedProof,
            IndexedAxisRangeProof, IndexedTargetChain,
        },
        ProveOptions,
    },
    AggregateSumPathQuery, PathBranchChunkQuery, PathQuery, PathTrunkChunkQuery, Query, SizedQuery,
};

#[derive(Encode, DecodeUntrusted)]
struct ClientRequest {
    query: PathQuery,
    aggregate: AggregateSumPathQuery,
    branch: PathBranchChunkQuery,
    trunk: PathTrunkChunkQuery,
    options: ProveOptions,
}

fn round_trip_bytes<T: Encode + DecodeUntrusted<()> + for<'de> BorrowDecodeUntrusted<'de, ()>>(
    bytes: &[u8],
) {
    let config = bincode::config::standard().with_limit::<1048576>();
    let (owned, used): (T, _) = bincode::decode_from_slice_untrusted(bytes, config).unwrap();
    assert_eq!(used, bytes.len());
    assert_eq!(bincode::encode_to_vec(owned, config).unwrap(), bytes);
    let (borrowed, used): (T, _) =
        bincode::borrow_decode_from_slice_untrusted(bytes, config).unwrap();
    assert_eq!(used, bytes.len());
    assert_eq!(bincode::encode_to_vec(borrowed, config).unwrap(), bytes);
}

#[test]
fn client_can_derive_untrusted_decoding_for_query_requests() {
    let request = ClientRequest {
        query: PathQuery::new(
            vec![b"root".to_vec()],
            SizedQuery::new(Query::new_single_key(b"key".to_vec()), Some(5), None),
        ),
        aggregate: AggregateSumPathQuery::new_single_key(
            vec![b"root".to_vec()],
            b"key".to_vec(),
            100,
        ),
        branch: PathBranchChunkQuery::new(vec![b"root".to_vec()], b"key".to_vec(), 3),
        trunk: PathTrunkChunkQuery::new(vec![b"root".to_vec()], 4),
        options: ProveOptions::default(),
    };
    let bytes = bincode::encode_to_vec(request, bincode::config::standard()).unwrap();
    round_trip_bytes::<ClientRequest>(&bytes);
}

#[test]
fn indexed_envelopes_support_owned_and_borrowed_untrusted_decoding() {
    let config = bincode::config::standard();
    let range = IndexedAxisRangeProof {
        axis_tag: 0,
        layer_proofs: vec![vec![1, 2]],
        primary_root_hash: [3; 32],
        ancestor_attestations: vec![AncestorAttestation::MultiAxis(vec![(0, [4; 32])])],
        other_axes_root_hashes: vec![(1, [5; 32])],
        target_is_pcpsit: true,
        secondary_proof: vec![6, 7],
        target_chains: Vec::<IndexedTargetChain>::new(),
        requested_limit: Some(4),
        descending: false,
    };
    let paginated = IndexedAxisPaginatedProof {
        axis_tag: 0,
        layer_proofs: range.layer_proofs.clone(),
        primary_root_hash: range.primary_root_hash,
        ancestor_attestations: vec![AncestorAttestation::SingleSecondary([8; 32])],
        other_axes_root_hashes: range.other_axes_root_hashes.clone(),
        target_is_pcpsit: true,
        secondary_proof: range.secondary_proof.clone(),
        target_chains: Vec::new(),
        requested_k: 5,
        requested_offset: 20,
        descending: true,
    };
    let aggregate = IndexedAxisAggregateProof {
        axis_tag: 0,
        layer_proofs: range.layer_proofs.clone(),
        primary_root_hash: range.primary_root_hash,
        ancestor_attestations: vec![AncestorAttestation::NotIndexed],
        other_axes_root_hashes: range.other_axes_root_hashes.clone(),
        target_is_pcpsit: true,
        secondary_proof: range.secondary_proof.clone(),
        lo: -10,
        hi: 100,
        fold_tag: 1,
    };
    fn compare<
        T: Encode + Decode<()> + DecodeUntrusted<()> + for<'de> BorrowDecodeUntrusted<'de, ()>,
    >(
        value: T,
    ) {
        let config = bincode::config::standard();
        let bytes = bincode::encode_to_vec(value, config).unwrap();
        let (ordinary, consumed): (T, _) = bincode::decode_from_slice(&bytes, config).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(bincode::encode_to_vec(ordinary, config).unwrap(), bytes);
        round_trip_bytes::<T>(&bytes);
    }
    compare(range);
    compare(paginated);
    compare(aggregate);
    let mut hostile = vec![0]; // axis tag, then an unbacked layer-proofs count
    hostile.extend(bincode::encode_to_vec(u64::MAX, config).unwrap());
    macro_rules! reject {
        ($ty:ty) => {
            assert!(bincode::decode_from_slice_untrusted::<$ty, _>(&hostile, config).is_err());
            assert!(
                bincode::borrow_decode_from_slice_untrusted::<$ty, _>(&hostile, config).is_err()
            );
        };
    }
    reject!(IndexedAxisRangeProof);
    reject!(IndexedAxisPaginatedProof);
    reject!(IndexedAxisAggregateProof);
}
