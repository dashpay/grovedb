//! Verify-only fixture tests for `verify_aggregate_count_on_range_proof`.
//!
//! These tests do NOT generate proofs — they verify pre-captured proof
//! bytes against expected `(root_hash, count)` pairs. The fixtures are
//! produced by `dump_verify_only_fixtures` (in the main tests module)
//! and copied here as hex constants. The `verify_only_fixture_matches_fresh_prover_output`
//! test in `tests.rs` re-runs the prover and fails loudly if any
//! fixture drifts from the live encoding — so an encoding change is a
//! one-line edit here after that guard fires.
//!
//! Lives in its own file so the verify-only crate feature (`verify`)
//! can build and exercise just these — `tests.rs` pulls in the full
//! `minimal` feature for prover-side helpers and would not compile
//! under the leaner build.

use super::verify_aggregate_count_on_range_proof;
use crate::proofs::query::{aggregate_common::NULL_HASH, QueryItem};

/// Hex-encoded proof bytes for a 15-key `ProvableCountTree` (keys
/// "a"..="o", each with feature `ProvableCountedMerkNode(1)`) queried
/// with `RangeInclusive("c"..="l")`. Captured from
/// `dump_verify_only_fixtures`; regenerate if the proof encoding ever
/// changes.
pub(super) const FIXTURE_15_KEY_C_TO_L_PROOF_HEX: &str = "1e76f2d62fbefb076d8902b2f25bcf9acbd1e903b740c98ea0d926473922f6bbb50000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000011a01622022ec9d571ba774cf9e83d0194962f5d1e3aa1a48d486a67e2762a6c79590150000000000000003101a0163b7d770040f780e9deff6bc038abea66e108b88d098d16d24cd7486eb671060b20000000000000001111a0164d2ad1a0bb9fdf4450bd87151c08b9968cd046bda6654aabdba2430b0a981e7900000000000000007101e28b724715b1fab1f72e0be7e944488dcfbeeb875867d27c06ad9bad8c739997207ce95cd4d1789e01c0a079a3f2c18a3888f2d69fa6d0eabf51c2b434f7cb99e212f1a0042798fb890e8203f007f4f58b033a72cb4e070bfddceb27687d641fe0000000000000003111a01683fc14ed7ecde203a90425ee191e9db5966336d737f0398ec93b764517b6df400000000000000000f101e6da2e2f8e4bdead2a8ac51909f0fa0fb88d47d6bc3b84858bb739fb28a36501031b7c191d5ac70764f815bd7a6c7d0e628f48cef5b813933c07d5ce0ac1dbd5a995443ca10193ebf20e64468deaecc061a981a6dbf4f30e7154b5e9ab806866d00000000000000031a016c55c024f95ca4cc338f7cc2e25db37be2a3fa3a40b151017e460bfc0779cf369f0000000000000007101e3673296561a4d6c3e1ec5cd02c5c468acbd3c8ccd4a42906e8ed06d3fb587a0d2b6d9e310b7c94d3f91fcbb3d5f7547b76c6d1ab3ac3d3540752c5f0b46be24a2f66bf541434a53eae46fa4e6092c03511538c0e1a2c5fc0f0deb72de08a71e500000000000000031111";
pub(super) const FIXTURE_15_KEY_C_TO_L_ROOT_HEX: &str =
    "19ed16776ebe6643b342a238baf7508ddf687fc4bdd53e98f91df8bffb605d96";
pub(super) const FIXTURE_15_KEY_C_TO_L_COUNT: u64 = 10;

/// Empty proof bytes encode "empty merk" — the verifier returns
/// `(NULL_HASH, 0)`. This case has no prover dependency at all and is
/// the most basic compile-time signal that the verifier path is wired
/// up correctly under `--no-default-features --features verify`.
#[test]
fn empty_merk_returns_null_hash_and_zero_count() {
    let inner = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
    let (root, count) = verify_aggregate_count_on_range_proof(&[], &inner)
        .unwrap()
        .expect("verify on empty proof must succeed");
    assert_eq!(root, NULL_HASH);
    assert_eq!(count, 0);
}

/// A real `RangeInclusive("c"..="l")` proof against a 15-key
/// `ProvableCountTree`. The verifier must reconstruct the expected
/// merk root hash and recover count = 10.
#[test]
fn fixture_15_key_range_c_to_l_verifies() {
    let proof = hex::decode(FIXTURE_15_KEY_C_TO_L_PROOF_HEX).expect("valid hex");
    let mut expected_root = [0u8; 32];
    expected_root.copy_from_slice(&hex::decode(FIXTURE_15_KEY_C_TO_L_ROOT_HEX).expect("valid hex"));

    let inner = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (root, count) = verify_aggregate_count_on_range_proof(&proof, &inner)
        .unwrap()
        .expect("fixture proof must verify");
    assert_eq!(
        root, expected_root,
        "verifier reconstructed an unexpected root — fixture stale?"
    );
    assert_eq!(count, FIXTURE_15_KEY_C_TO_L_COUNT);
}

/// Mutating any single byte of the fixture proof must not yield a
/// `(honest_root, wrong_count)` outcome — the hash chain binds count via
/// `node_hash_with_count`, so any successful verify with the honest root
/// must reproduce the honest count. Single-fixture analogue of
/// `fuzz_byte_mutation_no_silent_forgery` that runs without the prover.
#[test]
fn fixture_byte_mutation_does_not_silently_forge_count() {
    let proof = hex::decode(FIXTURE_15_KEY_C_TO_L_PROOF_HEX).expect("valid hex");
    let mut expected_root = [0u8; 32];
    expected_root.copy_from_slice(&hex::decode(FIXTURE_15_KEY_C_TO_L_ROOT_HEX).expect("valid hex"));
    let inner = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());

    // Sanity check: the honest fixture verifies under the same code path
    // the mutation loop will exercise. Without this, an `Err`-on-honest
    // fixture would silently make every mutation a vacuous pass.
    let (honest_root, honest_count) = verify_aggregate_count_on_range_proof(&proof, &inner)
        .unwrap()
        .expect("honest fixture must verify (regenerate fixture if this fails)");
    assert_eq!(honest_root, expected_root);
    assert_eq!(honest_count, FIXTURE_15_KEY_C_TO_L_COUNT);

    for byte_idx in 0..proof.len() {
        for &delta in &[1u8, 0x55, 0xff] {
            let mut bytes = proof.clone();
            let original = bytes[byte_idx];
            let mutated = if delta == 0xff {
                original ^ 0xff
            } else {
                original.wrapping_add(delta)
            };
            if mutated == original {
                continue;
            }
            bytes[byte_idx] = mutated;
            if let Ok((root, count)) =
                verify_aggregate_count_on_range_proof(&bytes, &inner).unwrap()
                && root == expected_root
            {
                assert_eq!(
                    count, FIXTURE_15_KEY_C_TO_L_COUNT,
                    "SILENT FORGERY at byte {} (delta=0x{:02x}): \
                     verifier returned the honest root but a wrong count \
                     ({} != {}).",
                    byte_idx, delta, count, FIXTURE_15_KEY_C_TO_L_COUNT
                );
            }
            // Err and Ok-with-different-root are both safe outcomes.
        }
    }
}
