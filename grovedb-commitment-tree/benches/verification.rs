//! Benchmarks for Orchard proof and signature verification.
//!
//! Measures the actual time ratios between:
//! - Halo 2 ZK proof verification (per-bundle, scales with action count)
//! - RedPallas spend auth signature verification (per-action)
//! - RedPallas binding signature verification (per-bundle)
//! - Full BatchValidator (proof + all signatures)
//!
//! Run with:
//! ```
//! cargo bench -p grovedb-commitment-tree --bench verification
//! ```
//!
//! NOTE: The first run takes ~35 seconds to build ProvingKey + VerifyingKey.

use std::sync::OnceLock;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use grovedb_commitment_tree::{
    Anchor, Authorized, BatchValidator, Builder, Bundle, BundleType, DashMemo, Flags,
    FullViewingKey, Hashable, MerkleHashOrchard, NoteValue, ProvingKey, Scope, SpendingKey,
    VerifyingKey,
};
use rand::rngs::OsRng;

static PROVING_KEY: OnceLock<ProvingKey> = OnceLock::new();
static VERIFYING_KEY: OnceLock<VerifyingKey> = OnceLock::new();

fn get_pk() -> &'static ProvingKey {
    PROVING_KEY.get_or_init(ProvingKey::build)
}

fn get_vk() -> &'static VerifyingKey {
    VERIFYING_KEY.get_or_init(VerifyingKey::build)
}

/// Build a shielding bundle (output-only, SPENDS_DISABLED) with N outputs = N
/// actions.
fn build_bundle(num_outputs: usize) -> Bundle<Authorized, i64, DashMemo> {
    let mut rng = OsRng;
    let pk = get_pk();

    let sk = SpendingKey::from_bytes([7; 32]).unwrap();
    let fvk = FullViewingKey::from(&sk);
    let recipient = fvk.address_at(0u32, Scope::External);

    let anchor: Anchor = MerkleHashOrchard::empty_root(32.into()).into();

    let mut builder = Builder::<DashMemo>::new(
        BundleType::Transactional {
            flags: Flags::SPENDS_DISABLED,
            bundle_required: false,
        },
        anchor,
    );

    for _ in 0..num_outputs {
        builder
            .add_output(None, recipient, NoteValue::from_raw(5000), [0u8; 36])
            .unwrap();
    }

    let (unauthorized, _) = builder.build::<i64>(&mut rng).unwrap().unwrap();
    let sighash: [u8; 32] = unauthorized.commitment().into();
    let proven = unauthorized.create_proof(pk, &mut rng).unwrap();
    proven.apply_signatures(rng, sighash, &[]).unwrap()
}

fn benchmark_proof_verification(c: &mut Criterion) {
    let vk = get_vk();

    // Pre-build bundles with 1-4 actions
    let bundles: Vec<_> = (1..=4).map(build_bundle).collect();

    // --- Halo 2 proof verification (scales with action count) ---
    {
        let mut group = c.benchmark_group("halo2_proof");
        group.sample_size(10);
        for (i, bundle) in bundles.iter().enumerate() {
            let num_actions = i + 1;
            let instances: Vec<_> = bundle
                .actions()
                .iter()
                .map(|a| a.to_instance(*bundle.flags(), *bundle.anchor()))
                .collect();

            group.bench_function(BenchmarkId::new("actions", num_actions), |b| {
                b.iter(|| bundle.authorization().proof().verify(vk, &instances));
            });
        }
        group.finish();
    }
}

fn benchmark_signature_verification(c: &mut Criterion) {
    let bundle = build_bundle(1);
    let sighash: [u8; 32] = bundle.commitment().into();

    // --- RedPallas spend auth signature (per-action cost) ---
    {
        let action = &bundle.actions()[0];
        let rk = action.rk();
        let sig = action.authorization();

        c.bench_function("redpallas_spend_auth_sig", |b| {
            b.iter(|| rk.verify(&sighash, sig));
        });
    }

    // --- RedPallas binding signature (per-bundle cost) ---
    {
        let bvk = bundle.binding_validating_key();
        let binding_sig = bundle.authorization().binding_signature();

        c.bench_function("redpallas_binding_sig", |b| {
            b.iter(|| bvk.verify(&sighash, binding_sig));
        });
    }
}

fn benchmark_batch_validator(c: &mut Criterion) {
    let vk = get_vk();

    let bundles: Vec<_> = (1..=4).map(build_bundle).collect();

    // --- Full BatchValidator: proof + spend auth sigs + binding sig ---
    let mut group = c.benchmark_group("batch_validator");
    group.sample_size(10);
    for (i, bundle) in bundles.iter().enumerate() {
        let num_actions = i + 1;
        let sighash: [u8; 32] = bundle.commitment().into();

        group.bench_function(BenchmarkId::new("actions", num_actions), |b| {
            b.iter(|| {
                let mut validator = BatchValidator::new();
                validator.add_bundle(bundle, sighash);
                validator.validate(vk, OsRng)
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    benchmark_proof_verification,
    benchmark_signature_verification,
    benchmark_batch_validator,
);
criterion_main!(benches);
