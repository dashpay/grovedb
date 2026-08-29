//! Adversarial-input tests for the untrusted state-sync decode surfaces.
//!
//! Every function exercised here parses bytes a *remote peer* chose. A
//! syncing node hands them to the decoder before anything has been
//! verified, so the decoders carry three obligations, and each gets a
//! property here:
//!
//! 1. **No panic on any input.** A panic in a decoder is a remote crash of
//!    a syncing node.
//! 2. **Output bounded by input.** A decoder must never turn `n` wire
//!    bytes into materially more than `n` bytes of payload — otherwise a
//!    small message is an allocation bomb. (The bound is on *payload*
//!    bytes: `unpack_nested_bytes` documents that `Vec` headers are a
//!    constant factor on top, which is why the transport still owns the
//!    absolute message-size cap.)
//! 3. **Canonical framing.** Every one of these decoders rejects trailing
//!    bytes and length mismatches, so `encode(decode(x)) == x` must hold
//!    whenever `decode` succeeds. That is strictly stronger than
//!    `decode(encode(v)) == v`: it also rules out two distinct encodings
//!    of the same value, which is what lets per-chunk identity checks
//!    upstream be byte comparisons.
//!
//! Random bytes almost never reach the deep branches of a structured
//! decoder, so each surface is fuzzed twice: with unstructured bytes, and
//! with a *valid encoding whose bytes have been mutated* — the input class
//! a byzantine peer actually produces.

#[cfg(test)]
mod tests {
    use grovedb_merk::{
        tree::hash::{axes_digest, combine_hash_three, CryptoHash},
        tree_type::TreeType,
    };
    use proptest::prelude::*;

    use crate::{
        replication::{
            indexed_sync::{
                is_indexed_header_request, verify_indexed_binding, IndexedHeader,
                IndexedHeaderRequest,
            },
            non_merk_sync::{
                decode_non_merk_page, encode_non_merk_page, NonMerkChunkId, MAX_PAGE_BYTES,
                MAX_PAGE_ENTRIES,
            },
            utils::{
                decode_global_chunk_id, encode_global_chunk_id, pack_nested_bytes,
                unpack_nested_bytes,
            },
        },
        Element,
    };

    /// A fixed app hash for `decode_global_chunk_id`, which short-circuits
    /// on an input equal to the app hash. No generated input can collide
    /// with this by accident.
    const FUZZ_APP_HASH: [u8; 32] = [0xAB; 32];

    // ── Strategies ──────────────────────────────────────────────────────

    /// Unstructured peer bytes.
    fn arbitrary_bytes() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), 0..512)
    }

    /// Apply `mutations` single-byte edits (splice / truncate / flip) to
    /// `bytes`. This is what turns a valid encoding into the near-miss
    /// inputs that reach a decoder's interior error branches.
    fn mutate(mut bytes: Vec<u8>, mutations: Vec<(usize, u8, u8)>) -> Vec<u8> {
        for (raw_index, kind, value) in mutations {
            match kind % 3 {
                0 if !bytes.is_empty() => {
                    let i = raw_index % bytes.len();
                    bytes[i] = value;
                }
                1 => {
                    let i = raw_index % (bytes.len() + 1);
                    bytes.insert(i, value);
                }
                _ if !bytes.is_empty() => {
                    let i = raw_index % bytes.len();
                    bytes.remove(i);
                }
                _ => {}
            }
        }
        bytes
    }

    fn mutations() -> impl Strategy<Value = Vec<(usize, u8, u8)>> {
        prop::collection::vec((any::<usize>(), any::<u8>(), any::<u8>()), 0..4)
    }

    fn hash_strategy() -> impl Strategy<Value = CryptoHash> {
        any::<[u8; 32]>()
    }

    fn indexed_header_strategy() -> impl Strategy<Value = IndexedHeader> {
        (
            hash_strategy(),
            prop::collection::vec((0u8..3, hash_strategy()), 1..=3),
        )
            .prop_map(|(primary_root_hash, axes)| IndexedHeader {
                primary_root_hash,
                axes,
            })
    }

    fn indexed_header_request_strategy() -> impl Strategy<Value = IndexedHeaderRequest> {
        prop::collection::vec(
            (
                0u8..3,
                prop::option::of(prop::collection::vec(any::<u8>(), 1..48)),
            ),
            1..=3,
        )
        .prop_map(|axes| IndexedHeaderRequest { axes })
    }

    fn nested_bytes_strategy() -> impl Strategy<Value = Vec<Vec<u8>>> {
        prop::collection::vec(prop::collection::vec(any::<u8>(), 0..40), 0..12)
    }

    // ── Shared assertions ───────────────────────────────────────────────

    /// The payload bytes `unpack_nested_bytes` hands back must be exactly
    /// framed by the input: 4 count bytes, then 4 length bytes plus the
    /// payload per element, with nothing left over.
    fn assert_unpack_is_bounded_and_exact(
        input: &[u8],
        parts: &[Vec<u8>],
    ) -> Result<(), TestCaseError> {
        let payload: usize = parts.iter().map(Vec::len).sum();
        prop_assert!(
            payload <= input.len(),
            "unpacked {payload} payload bytes from a {}-byte input",
            input.len()
        );
        prop_assert!(
            parts.len() <= input.len().saturating_sub(4) / 4,
            "unpacked {} elements from a {}-byte input",
            parts.len(),
            input.len()
        );
        prop_assert_eq!(
            4 + 4 * parts.len() + payload,
            input.len(),
            "framing must consume the input exactly"
        );
        Ok(())
    }

    // ── `unpack_nested_bytes` ───────────────────────────────────────────

    proptest! {
        /// Arbitrary bytes: never panics, and any success is exactly
        /// framed, input-bounded, and re-encodes to the same bytes.
        #[test]
        fn unpack_nested_bytes_is_total_and_canonical(input in arbitrary_bytes()) {
            if let Ok(parts) = unpack_nested_bytes(&input) {
                assert_unpack_is_bounded_and_exact(&input, &parts)?;
                prop_assert_eq!(
                    pack_nested_bytes(parts).unwrap(),
                    input,
                    "a successful decode must re-encode to the same bytes"
                );
            }
        }

        /// The same, driven by mutated valid packings so the interior
        /// error branches (declared count, per-element length, trailing
        /// bytes) are actually reached.
        #[test]
        fn unpack_nested_bytes_survives_mutated_packings(
            parts in nested_bytes_strategy(),
            muts in mutations(),
        ) {
            let input = mutate(pack_nested_bytes(parts).unwrap(), muts);
            if let Ok(parts) = unpack_nested_bytes(&input) {
                assert_unpack_is_bounded_and_exact(&input, &parts)?;
            }
        }

        /// Forward round trip: every value survives encode → decode.
        #[test]
        fn pack_nested_bytes_round_trips(parts in nested_bytes_strategy()) {
            let encoded = pack_nested_bytes(parts.clone()).unwrap();
            prop_assert_eq!(unpack_nested_bytes(&encoded).unwrap(), parts);
        }
    }

    // ── `decode_global_chunk_id` ────────────────────────────────────────

    /// Valid global chunk ids, from which mutants are derived.
    fn global_chunk_id_strategy() -> impl Strategy<Value = Vec<u8>> {
        (
            any::<[u8; 32]>(),
            prop::option::of(prop::collection::vec(any::<u8>(), 1..40)),
            0u8..=16,
            nested_bytes_strategy(),
        )
            .prop_map(|(prefix, root_key, tree_byte, chunk_ids)| {
                let tree_type = TreeType::try_from(tree_byte).expect("0..=16 are all valid");
                encode_global_chunk_id(prefix, root_key, tree_type, chunk_ids)
                    .expect("components are in range")
            })
    }

    fn assert_global_chunk_id_bounded(
        input: &[u8],
        decoded: &crate::replication::ChunkIdentifier,
    ) -> Result<(), TestCaseError> {
        let (_, root_key, _, nested) = decoded;
        let root_key_len = root_key.as_ref().map_or(0, Vec::len);
        let nested_len: usize = nested.iter().map(Vec::len).sum();
        prop_assert!(
            root_key_len + nested_len <= input.len(),
            "decoded {} payload bytes from a {}-byte chunk id",
            root_key_len + nested_len,
            input.len()
        );
        prop_assert!(
            nested.len() <= input.len() / 4,
            "decoded {} nested chunk ids from a {}-byte chunk id",
            nested.len(),
            input.len()
        );
        Ok(())
    }

    proptest! {
        #[test]
        fn decode_global_chunk_id_is_total_and_canonical(input in arbitrary_bytes()) {
            prop_assume!(input.as_slice() != FUZZ_APP_HASH.as_slice());
            if let Ok(decoded) = decode_global_chunk_id(&input, &FUZZ_APP_HASH) {
                assert_global_chunk_id_bounded(&input, &decoded)?;
                let (prefix, root_key, tree_type, nested) = decoded;
                prop_assert_eq!(
                    encode_global_chunk_id(prefix, root_key, tree_type, nested).unwrap(),
                    input,
                    "a successful decode must re-encode to the same bytes"
                );
            }
        }

        #[test]
        fn decode_global_chunk_id_survives_mutated_ids(
            valid in global_chunk_id_strategy(),
            muts in mutations(),
        ) {
            let input = mutate(valid, muts);
            prop_assume!(input.as_slice() != FUZZ_APP_HASH.as_slice());
            if let Ok(decoded) = decode_global_chunk_id(&input, &FUZZ_APP_HASH) {
                assert_global_chunk_id_bounded(&input, &decoded)?;
            }
        }

        /// Every well-formed chunk id decodes back to its components.
        #[test]
        fn global_chunk_id_round_trips(valid in global_chunk_id_strategy()) {
            prop_assume!(valid.as_slice() != FUZZ_APP_HASH.as_slice());
            let (prefix, root_key, tree_type, nested) =
                decode_global_chunk_id(&valid, &FUZZ_APP_HASH).expect("valid id decodes");
            prop_assert_eq!(
                encode_global_chunk_id(prefix, root_key, tree_type, nested).unwrap(),
                valid
            );
        }
    }

    // ── `IndexedHeader` / `IndexedHeaderRequest` ────────────────────────

    proptest! {
        #[test]
        fn indexed_header_decode_is_total_and_canonical(input in arbitrary_bytes()) {
            if let Ok(header) = IndexedHeader::decode(&input) {
                prop_assert!((1..=3).contains(&header.axes.len()));
                prop_assert_eq!(input.len(), 33 + 33 * header.axes.len());
                prop_assert_eq!(header.encode(), input);
            }
        }

        #[test]
        fn indexed_header_survives_mutated_encodings(
            header in indexed_header_strategy(),
            muts in mutations(),
        ) {
            let input = mutate(header.encode(), muts);
            if let Ok(decoded) = IndexedHeader::decode(&input) {
                prop_assert!((1..=3).contains(&decoded.axes.len()));
                prop_assert_eq!(decoded.encode(), input);
            }
        }

        #[test]
        fn indexed_header_round_trips(header in indexed_header_strategy()) {
            prop_assert_eq!(IndexedHeader::decode(&header.encode()).unwrap(), header);
        }

        #[test]
        fn indexed_header_request_decode_is_total_and_canonical(input in arbitrary_bytes()) {
            if let Ok(request) = IndexedHeaderRequest::decode(&input) {
                prop_assert!((1..=3).contains(&request.axes.len()));
                let key_bytes: usize = request
                    .axes
                    .iter()
                    .map(|(_, key)| key.as_ref().map_or(0, Vec::len))
                    .sum();
                prop_assert!(key_bytes <= input.len());
                // Anything that decodes as a header request must also be
                // recognised as one — otherwise `fetch_chunk` would route
                // it to the Merk chunk producer instead.
                prop_assert!(is_indexed_header_request(&input));
                prop_assert_eq!(request.encode(), input);
            }
        }

        #[test]
        fn indexed_header_request_survives_mutated_encodings(
            request in indexed_header_request_strategy(),
            muts in mutations(),
        ) {
            let input = mutate(request.encode(), muts);
            if let Ok(decoded) = IndexedHeaderRequest::decode(&input) {
                prop_assert!((1..=3).contains(&decoded.axes.len()));
                prop_assert_eq!(decoded.encode(), input);
            }
        }

        #[test]
        fn indexed_header_request_round_trips(request in indexed_header_request_strategy()) {
            let encoded = request.encode();
            prop_assert!(is_indexed_header_request(&encoded));
            prop_assert_eq!(IndexedHeaderRequest::decode(&encoded).unwrap(), request);
        }

        /// A Merk traversal instruction (only `0x00` / `0x01` bytes) must
        /// never be mistaken for a header request — the disambiguation the
        /// marker byte exists for.
        #[test]
        fn traversal_instructions_are_never_header_requests(
            instruction in prop::collection::vec(0u8..=1, 0..24),
        ) {
            prop_assert!(!is_indexed_header_request(&instruction));
        }
    }

    // ── `NonMerkChunkId` and non-Merk pages ─────────────────────────────

    fn non_merk_page_strategy() -> impl Strategy<Value = Vec<u8>> {
        (
            any::<bool>(),
            prop::collection::vec(any::<u8>(), 0..32),
            prop::collection::vec(prop::collection::vec(any::<u8>(), 0..24), 0..8),
        )
            .prop_map(|(more, aux, entries)| {
                encode_non_merk_page(more, aux, entries).expect("page encodes")
            })
    }

    proptest! {
        #[test]
        fn non_merk_chunk_id_decode_is_total_and_canonical(input in arbitrary_bytes()) {
            if let Ok(id) = NonMerkChunkId::decode(&input) {
                prop_assert_eq!(input.len(), 17);
                prop_assert_eq!(id.encode(), input);
            }
        }

        #[test]
        fn non_merk_chunk_id_round_trips(start in any::<u64>(), state in any::<u64>(), param in any::<u8>()) {
            let id = NonMerkChunkId { start, state, param };
            prop_assert_eq!(NonMerkChunkId::decode(&id.encode()).unwrap(), id);
        }

        #[test]
        fn decode_non_merk_page_is_total_and_bounded(input in arbitrary_bytes()) {
            if let Ok((more, aux, entries)) = decode_non_merk_page(&input) {
                prop_assert!(entries.len() <= MAX_PAGE_ENTRIES);
                let payload: usize = aux.len() + entries.iter().map(Vec::len).sum::<usize>();
                prop_assert!(
                    payload <= input.len(),
                    "decoded {payload} payload bytes from a {}-byte page",
                    input.len()
                );
                if let Some((_last, head)) = entries.split_last() {
                    prop_assert!(head.iter().map(Vec::len).sum::<usize>() < MAX_PAGE_BYTES);
                }
                prop_assert_eq!(encode_non_merk_page(more, aux, entries).unwrap(), input);
            }
        }

        #[test]
        fn decode_non_merk_page_survives_mutated_pages(
            page in non_merk_page_strategy(),
            muts in mutations(),
        ) {
            let input = mutate(page, muts);
            if let Ok((more, aux, entries)) = decode_non_merk_page(&input) {
                prop_assert!(entries.len() <= MAX_PAGE_ENTRIES);
                prop_assert_eq!(encode_non_merk_page(more, aux, entries).unwrap(), input);
            }
        }

        #[test]
        fn non_merk_page_round_trips(
            more in any::<bool>(),
            aux in prop::collection::vec(any::<u8>(), 0..32),
            entries in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..24), 0..8),
        ) {
            let encoded = encode_non_merk_page(more, aux.clone(), entries.clone()).unwrap();
            let (d_more, d_aux, d_entries) = decode_non_merk_page(&encoded).unwrap();
            prop_assert_eq!(d_more, more);
            prop_assert_eq!(d_aux, aux);
            prop_assert_eq!(d_entries, entries);
        }
    }

    // ── Targeted error branches: `IndexedHeader::decode` ────────────────
    //
    // One test per distinct rejection in the decoder, asserting the
    // message and not just `is_err()`, so a branch that starts returning
    // the wrong diagnosis is caught.

    fn header_decode_err(bytes: &[u8]) -> String {
        format!(
            "{}",
            IndexedHeader::decode(bytes).expect_err("decode should reject")
        )
    }

    #[test]
    fn indexed_header_decode_rejects_empty_input() {
        assert!(header_decode_err(&[]).contains("too short"));
    }

    #[test]
    fn indexed_header_decode_rejects_input_shorter_than_the_fixed_prefix() {
        // 32 bytes carry the primary root hash but leave no axis count.
        assert!(header_decode_err(&[0u8; 32]).contains("too short"));
    }

    #[test]
    fn indexed_header_decode_rejects_zero_axes() {
        let mut bytes = vec![0u8; 33];
        bytes[32] = 0;
        assert!(header_decode_err(&bytes).contains("axis count must be 1..=3"));
    }

    #[test]
    fn indexed_header_decode_rejects_more_than_three_axes() {
        let mut bytes = vec![0u8; 33 + 4 * 33];
        bytes[32] = 4;
        assert!(header_decode_err(&bytes).contains("axis count must be 1..=3"));
        // Also the extreme: a count byte of 255.
        let mut huge = vec![0u8; 33];
        huge[32] = u8::MAX;
        assert!(header_decode_err(&huge).contains("axis count must be 1..=3"));
    }

    #[test]
    fn indexed_header_decode_rejects_a_short_axis_section() {
        let header = IndexedHeader {
            primary_root_hash: [1u8; 32],
            axes: vec![(0, [2u8; 32])],
        };
        let mut bytes = header.encode();
        bytes.pop();
        assert!(header_decode_err(&bytes).contains("axis section length mismatch"));
    }

    #[test]
    fn indexed_header_decode_rejects_trailing_bytes() {
        let header = IndexedHeader {
            primary_root_hash: [1u8; 32],
            axes: vec![(0, [2u8; 32]), (1, [3u8; 32])],
        };
        let mut bytes = header.encode();
        bytes.push(0);
        assert!(header_decode_err(&bytes).contains("axis section length mismatch"));
    }

    #[test]
    fn indexed_header_decode_rejects_a_count_that_overstates_the_section() {
        // Two axes' worth of bytes, but the count claims three.
        let mut bytes = IndexedHeader {
            primary_root_hash: [1u8; 32],
            axes: vec![(0, [2u8; 32]), (1, [3u8; 32])],
        }
        .encode();
        bytes[32] = 3;
        assert!(header_decode_err(&bytes).contains("axis section length mismatch"));
    }

    // ── Targeted error branches: `verify_indexed_binding` ───────────────

    const VALUE_HASH: CryptoHash = [0x11; 32];
    const PRIMARY_ROOT: CryptoHash = [0x22; 32];
    const SECONDARY_ROOT: CryptoHash = [0x33; 32];

    /// The parent binding a correct single-axis group must reproduce.
    fn single_axis_binding() -> CryptoHash {
        combine_hash_three(&VALUE_HASH, &PRIMARY_ROOT, &SECONDARY_ROOT).unwrap()
    }

    #[test]
    fn verify_indexed_binding_accepts_a_correct_single_axis_group() {
        for element in [
            Element::empty_provable_count_indexed_tree(),
            Element::empty_provable_sum_indexed_tree(),
        ] {
            verify_indexed_binding(
                &element,
                &VALUE_HASH,
                &single_axis_binding(),
                &PRIMARY_ROOT,
                &[(0, SECONDARY_ROOT)],
            )
            .expect("a group whose actual roots reproduce the parent binding must be accepted");
        }
    }

    #[test]
    fn verify_indexed_binding_rejects_a_mismatched_single_axis_group() {
        let err = verify_indexed_binding(
            &Element::empty_provable_count_indexed_tree(),
            &VALUE_HASH,
            &single_axis_binding(),
            &[0xEE; 32], // not the primary root the binding commits to
            &[(0, SECONDARY_ROOT)],
        )
        .expect_err("a wrong primary root must be rejected");
        assert!(matches!(err, crate::Error::CorruptedData(_)), "got {err:?}");
        assert!(format!("{err}").contains("indexed subtree joint verification failed"));
    }

    #[test]
    fn verify_indexed_binding_rejects_a_single_axis_group_with_the_wrong_arity() {
        for secondaries in [
            &[][..],
            &[(0, SECONDARY_ROOT), (1, SECONDARY_ROOT)][..],
            &[
                (0, SECONDARY_ROOT),
                (1, SECONDARY_ROOT),
                (2, SECONDARY_ROOT),
            ][..],
        ] {
            let err = verify_indexed_binding(
                &Element::empty_provable_sum_indexed_tree(),
                &VALUE_HASH,
                &single_axis_binding(),
                &PRIMARY_ROOT,
                secondaries,
            )
            .expect_err("a single-axis element must reject a non-singleton secondary set");
            assert!(matches!(err, crate::Error::InternalError(_)), "got {err:?}");
            assert!(
                format!("{err}").contains("single-axis indexed group finalized with"),
                "got {err}"
            );
        }
    }

    #[test]
    fn verify_indexed_binding_accepts_and_rejects_a_three_axis_group() {
        let element = Element::empty_provable_count_provable_sum_indexed_tree(vec![
            (0, None),
            (1, None),
            (2, None),
        ])
        .expect("canonical three-axis configuration");
        let axes: Vec<(u8, CryptoHash)> = vec![(0, [1u8; 32]), (1, [2u8; 32]), (2, [3u8; 32])];
        let binding =
            combine_hash_three(&VALUE_HASH, &PRIMARY_ROOT, &axes_digest(&axes).unwrap()).unwrap();

        verify_indexed_binding(&element, &VALUE_HASH, &binding, &PRIMARY_ROOT, &axes)
            .expect("the canonical axes digest must reproduce the binding");

        // Reordering the axes changes the canonical digest.
        let mut swapped = axes.clone();
        swapped.swap(0, 2);
        let err = verify_indexed_binding(&element, &VALUE_HASH, &binding, &PRIMARY_ROOT, &swapped)
            .expect_err("a reordered axis set must not reproduce the binding");
        assert!(format!("{err}").contains("indexed subtree joint verification failed"));

        // So does dropping one.
        let err =
            verify_indexed_binding(&element, &VALUE_HASH, &binding, &PRIMARY_ROOT, &axes[..2])
                .expect_err("a truncated axis set must not reproduce the binding");
        assert!(format!("{err}").contains("indexed subtree joint verification failed"));
    }

    #[test]
    fn verify_indexed_binding_rejects_a_non_indexed_element() {
        let err = verify_indexed_binding(
            &Element::empty_tree(),
            &VALUE_HASH,
            &single_axis_binding(),
            &PRIMARY_ROOT,
            &[(0, SECONDARY_ROOT)],
        )
        .expect_err("a non-indexed element has no three-input binding to verify");
        assert!(matches!(err, crate::Error::InternalError(_)), "got {err:?}");
        assert!(
            format!("{err}").contains("called on a non-indexed element"),
            "got {err}"
        );
    }
}
