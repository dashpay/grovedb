//! Frontier-less seeding throughput benchmark — **test / devnet only**.
//!
//! Pre-populates a real RocksDB-backed [`CommitmentTree`] with `N` random
//! filler notes using [`CommitmentTree::append_many_without_frontier`] (the
//! frontier-less path added for devnet shielded-pool seeding), and reports how
//! long it takes. This is the bulk-append path that skips the per-note
//! Pallas/Sinsemilla hashing, so it measures blake3 BulkAppendTree work +
//! serialization + storage I/O only.
//!
//! Run with:
//! ```text
//! cargo bench -p grovedb-commitment-tree --bench seeding --features test-seeding-ct
//! ```
//!
//! The note count defaults to 1,000,000 and can be overridden:
//! ```text
//! SEED_N=200000 cargo bench -p grovedb-commitment-tree --bench seeding --features test-seeding-ct
//! ```

#[cfg(feature = "test-seeding-ct")]
fn main() {
    use std::time::Instant;

    use grovedb_commitment_tree::{ciphertext_payload_size, CommitmentTree, DashMemo};
    use grovedb_path::SubtreePath;
    use grovedb_storage::{rocksdb_storage::RocksDbStorage, Storage, StorageBatch};
    use rand::{rngs::StdRng, Rng, SeedableRng};

    let n: u64 = std::env::var("SEED_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    // Production value: 2^11 = 2048 notes per completed chunk.
    let chunk_power: u8 = 11;
    let payload_len = ciphertext_payload_size::<DashMemo>();
    let entry_len = 64 + payload_len; // cmx(32) + rho(32) + payload

    // Real on-disk RocksDB in a temp dir.
    let tmp = tempfile::TempDir::new().expect("create tempdir");
    let storage =
        RocksDbStorage::default_rocksdb_with_path(tmp.path()).expect("open rocksdb storage");
    let tx = storage.start_transaction();
    let batch = StorageBatch::new();
    let ct_path = SubtreePath::from(&[b"shielded_notes" as &[u8]]);
    let ctx = storage
        .get_transactional_storage_context(ct_path, Some(&batch), &tx)
        .value;
    let mut ct =
        CommitmentTree::<_, DashMemo>::new(chunk_power, ctx).expect("create commitment tree");

    // Deterministic, lazily-generated note stream — never materialized in full.
    let mut rng = StdRng::seed_from_u64(0xC0FFEE);
    let notes = std::iter::from_fn(move || {
        let mut cmx = [0u8; 32];
        let mut rho = [0u8; 32];
        let mut payload = vec![0u8; payload_len];
        rng.fill_bytes(&mut cmx);
        rng.fill_bytes(&mut rho);
        rng.fill_bytes(&mut payload);
        Some((cmx, rho, payload))
    })
    .take(n as usize);

    eprintln!(
        "Seeding {n} notes (chunk_power={chunk_power}, payload={payload_len}B, entry={entry_len}B, ~{} MiB of note data)...",
        (n as usize * entry_len) / (1024 * 1024)
    );

    // 1. Frontier-less bulk append (compute + in-memory batch accumulation).
    let t_seed = Instant::now();
    let summary = ct
        .append_many_without_frontier(notes)
        .value
        .expect("frontier-less seeding");
    let seed_elapsed = t_seed.elapsed();

    // Release the storage context's borrow of the batch before committing.
    drop(ct);

    // 2. Flush the batch into the transaction, then commit the transaction to
    //    disk (the actual RocksDB write).
    let t_commit = Instant::now();
    storage
        .commit_multi_context_batch(batch, Some(&tx))
        .value
        .expect("commit batch into transaction");
    storage
        .commit_transaction(tx)
        .value
        .expect("commit transaction to disk");
    let commit_elapsed = t_commit.elapsed();

    let total = seed_elapsed + commit_elapsed;
    let rate = |d: std::time::Duration| n as f64 / d.as_secs_f64();

    eprintln!("---------------------------------------------");
    eprintln!("appended        : {}", summary.appended);
    eprintln!("total_count     : {}", summary.total_count);
    eprintln!("compactions     : {}", summary.compactions);
    eprintln!(
        "seed (compute)  : {:.3?}   ({:.0} notes/s)",
        seed_elapsed,
        rate(seed_elapsed)
    );
    eprintln!("commit (disk)   : {:.3?}", commit_elapsed);
    eprintln!(
        "TOTAL           : {:.3?}   ({:.0} notes/s)",
        total,
        rate(total)
    );
    eprintln!("---------------------------------------------");
}

#[cfg(not(feature = "test-seeding-ct"))]
fn main() {
    eprintln!(
        "The `seeding` benchmark requires the `test-seeding-ct` feature.\n\
         Run: cargo bench -p grovedb-commitment-tree --bench seeding --features test-seeding-ct"
    );
}
