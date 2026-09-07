//! Scale and memory-ceiling measurement for state sync restore.
//!
//! The default restore is **atomic**: every write of the whole sync — all
//! restored subtrees, across every discovery batch — is held in a single
//! `OptimisticTransactionDB` transaction (a `WriteBatchWithIndex`) until
//! `commit_session` verifies the root hash (see the invariant comment in
//! `state_sync_session::commit`). That write batch lives in RocksDB's C++
//! heap, so its cost shows up in the process's memory footprint and
//! nowhere else — a Rust allocator hook would not see it.
//!
//! Setting `GROVEDB_SCALE_RESTORE_BUDGET_MIB=<n>` runs the same
//! measurement against `RestoreCommitMode::Incremental` instead, which is
//! how the before/after comparison is reproduced.
//!
//! These tests measure that ceiling. They build a synthetic grove shaped
//! roughly like Dash Platform state (identity-like items under a flat
//! subtree, document-like items across nested per-contract subtrees, a sum
//! tree, a commitment tree, an MMR, and two indexed trees with populated
//! axes), checkpoint it, restore it into a fresh directory, and report:
//!
//! - the peak process memory footprint during the restore window
//!   (sampled, plus the baseline captured just before the window so the
//!   restore's own increment is visible, and a reading taken immediately
//!   before `commit_session` so the write batch can be separated from the
//!   commit-time flush),
//! - wall-clock of the fetch/apply/commit loop,
//! - the number of `fetch_chunk` round trips and total wire bytes,
//! - the on-disk size of the source checkpoint and of the restored target.
//!
//! Every test here is `#[ignore]`d: they take minutes and gigabytes, so CI
//! cost is zero. Run one explicitly, in **release** (the unoptimized
//! profile makes the build phase hours long):
//!
//! ```text
//! cargo test --release -p grovedb \
//!     restore_memory_ceiling_tier_small -- --ignored --nocapture
//! ```
//!
//! `--nocapture` matters: the measurement is printed, not asserted.
//!
//! # What the number does and does not attribute
//!
//! The simulated remote peer (`checkpoint_db`) and the syncing node
//! (`target`) share one process, so the sampled window covers both sides
//! of the wire: the peer's read path (block cache fills, SST decompression
//! buffers) is counted alongside the target's write batch. The reported
//! increment is therefore an **upper bound** on what a real syncing node
//! needs, not an exact attribution.
//!
//! The bound is a tight one, and the tiers show why: RocksDB's read-side
//! caches are fixed-size, so the peer's contribution is a constant, while
//! the measured increment grows with state size. `mem before commit`
//! isolates the part that matters most — at that point the entire sync
//! write set is sitting in the transaction's `WriteBatchWithIndex` and
//! nothing has been flushed, so it is the write batch, not the flush, that
//! the reading reflects.

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        path::Path,
        sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    use grovedb_version::version::GroveVersion;
    use tempfile::TempDir;

    use crate::{
        batch::QualifiedGroveDbOp,
        replication::{RestoreCommitMode, CURRENT_STATE_SYNC_VERSION},
        Element, GroveDb,
    };

    /// Restore commit mode for this run, from the environment.
    ///
    /// Unset (the default) measures the atomic restore. Setting
    /// `GROVEDB_SCALE_RESTORE_BUDGET_MIB=<n>` measures the bounded-memory
    /// restore with an `n` MiB payload budget, which is how the
    /// before/after table in the PR is reproduced without editing code.
    /// `GROVEDB_SCALE_RESTORE_IN_FLIGHT=<n>` overrides the in-flight
    /// subtree cap (default 1):
    ///
    /// ```text
    /// GROVEDB_SCALE_RESTORE_BUDGET_MIB=64 cargo test --release -p grovedb \
    ///     restore_memory_ceiling_tier_medium -- --ignored --nocapture
    /// ```
    fn commit_mode_from_env() -> RestoreCommitMode {
        match std::env::var("GROVEDB_SCALE_RESTORE_BUDGET_MIB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            Some(mib) if mib > 0 => RestoreCommitMode::Incremental {
                budget_bytes: mib * 1024 * 1024,
                max_subtrees_in_flight: std::env::var("GROVEDB_SCALE_RESTORE_IN_FLIGHT")
                    .ok()
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|n| *n > 0)
                    .unwrap_or(1),
            },
            _ => RestoreCommitMode::Atomic,
        }
    }

    // ── Shape of the synthetic grove ────────────────────────────────────

    /// Parameterized "Platform-shaped" grove. Byte totals are dominated by
    /// `identities` and `contracts * documents_per_contract`; the other
    /// members exist so every state-sync transfer mode (Merk chunks,
    /// non-Merk entry replay, indexed header + axis secondaries) is
    /// exercised at scale rather than only in the round-trip unit tests.
    #[derive(Debug, Clone, Copy)]
    struct GroveShape {
        /// Human label used in the printed report.
        tier: &'static str,
        /// Items under a single flat `identities` subtree.
        identities: usize,
        /// Bytes per identity-like item value.
        identity_value_bytes: usize,
        /// Number of per-contract subtrees under `documents`.
        contracts: usize,
        /// Document-like items in each contract's `docs` subtree.
        documents_per_contract: usize,
        /// Bytes per document-like item value.
        document_value_bytes: usize,
        /// Sum items in the `balances` sum tree.
        sum_entries: usize,
        /// Notes appended to the `notes` commitment tree. Sinsemilla
        /// hashing dominates build time per note, so this stays small and
        /// roughly constant across tiers on purpose.
        commitment_entries: usize,
        /// Leaves appended to the `history` MMR tree.
        mmr_entries: usize,
        /// Entries in each of the two indexed trees (`idx_count` /
        /// `idx_sum`).
        indexed_entries: usize,
    }

    impl GroveShape {
        /// Total number of restored key/value entries across every
        /// member, i.e. the number of `WriteBatchWithIndex` skiplist
        /// entries the restore's write set is at least as large as.
        fn entry_count(&self) -> u64 {
            (self.identities
                + self.contracts * self.documents_per_contract
                + self.sum_entries
                + self.commitment_entries
                + self.mmr_entries
                + self.indexed_entries * 2) as u64
        }

        /// Bytes of item *values* in the identity-like and document-like
        /// subtrees. Deliberately partial: it excludes keys, Merk node
        /// overhead, and the sum / commitment / MMR / indexed members, so
        /// it is a floor on the logical content, reported only as a rough
        /// scale label. The on-disk and wire figures are the ones to
        /// derive ratios from.
        fn logical_value_bytes(&self) -> u64 {
            (self.identities * self.identity_value_bytes) as u64
                + (self.contracts * self.documents_per_contract * self.document_value_bytes) as u64
        }
    }

    /// ~10 MB of payload: a fast smoke run that proves the harness works.
    const TIER_TINY: GroveShape = GroveShape {
        tier: "tiny",
        identities: 4_000,
        identity_value_bytes: 256,
        contracts: 2,
        documents_per_contract: 8_000,
        document_value_bytes: 512,
        sum_entries: 2_000,
        commitment_entries: 128,
        mmr_entries: 2_000,
        indexed_entries: 500,
    };

    /// ~100 MB of payload.
    const TIER_SMALL: GroveShape = GroveShape {
        tier: "small",
        identities: 40_000,
        identity_value_bytes: 256,
        contracts: 8,
        documents_per_contract: 22_000,
        document_value_bytes: 512,
        sum_entries: 20_000,
        commitment_entries: 512,
        mmr_entries: 20_000,
        indexed_entries: 2_000,
    };

    /// ~1 GB of payload.
    const TIER_MEDIUM: GroveShape = GroveShape {
        tier: "medium",
        identities: 400_000,
        identity_value_bytes: 256,
        contracts: 16,
        documents_per_contract: 110_000,
        document_value_bytes: 512,
        sum_entries: 100_000,
        commitment_entries: 512,
        mmr_entries: 100_000,
        indexed_entries: 5_000,
    };

    /// Roughly the same on-disk size as [`SHAPE_KEY_HEAVY`], reached with
    /// few, large values.
    ///
    /// The pair exists to answer one attribution question: does the
    /// restore's memory ceiling track the write batch's *data* (the
    /// serialised key and value bytes) or its *index* (one skiplist entry
    /// per key in the `WriteBatchWithIndex`)? At ~40x the entry count for
    /// the same on-disk size, an index-dominated ceiling would show a
    /// dramatically worse ratio on the key-heavy side. Measured, it does
    /// not: cost per source byte matches within ~15% while cost per entry
    /// differs 60x, so the batch payload is the ceiling and the skiplist
    /// is a single-digit percentage of it.
    const SHAPE_VALUE_HEAVY: GroveShape = GroveShape {
        tier: "value-heavy",
        identities: 12_000,
        identity_value_bytes: 8_192,
        contracts: 2,
        documents_per_contract: 6_000,
        document_value_bytes: 8_192,
        sum_entries: 0,
        commitment_entries: 0,
        mmr_entries: 0,
        indexed_entries: 0,
    };

    /// See [`SHAPE_VALUE_HEAVY`]: same rough on-disk size, ~40x the entry
    /// count.
    const SHAPE_KEY_HEAVY: GroveShape = GroveShape {
        tier: "key-heavy",
        identities: 500_000,
        identity_value_bytes: 32,
        contracts: 5,
        documents_per_contract: 100_000,
        document_value_bytes: 32,
        sum_entries: 0,
        commitment_entries: 0,
        mmr_entries: 0,
        indexed_entries: 0,
    };

    /// ~4 GB of payload.
    const TIER_LARGE: GroveShape = GroveShape {
        tier: "large",
        identities: 1_200_000,
        identity_value_bytes: 256,
        contracts: 32,
        documents_per_contract: 220_000,
        document_value_bytes: 512,
        sum_entries: 200_000,
        commitment_entries: 512,
        mmr_entries: 200_000,
        indexed_entries: 10_000,
    };

    // ── Process memory sampling ─────────────────────────────────────────

    /// Current physical memory footprint of this process, in bytes.
    ///
    /// Read from the OS rather than from a Rust allocator hook on purpose:
    /// the allocation under measurement is RocksDB's C++
    /// `WriteBatchWithIndex`, which never passes through Rust's
    /// `GlobalAlloc`. Returns `None` on platforms with no reading (the
    /// harness then reports zeroes instead of failing).
    ///
    /// On macOS this is `ri_phys_footprint`, **not** resident size. The
    /// distinction decides whether the largest tiers mean anything: under
    /// memory pressure macOS compresses anonymous pages, which drops them
    /// out of RSS while the process still owns them. Sampling RSS makes a
    /// restore that is swamping the machine look *cheaper* than one that
    /// fits, which is exactly backwards. `phys_footprint` counts
    /// compressed pages and does not have that failure mode.
    #[cfg(target_os = "macos")]
    fn current_memory_bytes() -> Option<u64> {
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        // SAFETY: `proc_pid_rusage` writes one `rusage_info_v4` through the
        // buffer pointer when called with `RUSAGE_INFO_V4`; `info` is a
        // zero-initialised value of exactly that type.
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V4,
                (&raw mut info).cast(),
            )
        };
        (rc == 0).then_some(info.ri_phys_footprint)
    }

    /// Linux twin of the macOS reading above: field 2 of `/proc/self/statm`
    /// is the resident set in pages. Linux does not compress anonymous
    /// memory by default, so resident size is the comparable figure there.
    #[cfg(target_os = "linux")]
    fn current_memory_bytes() -> Option<u64> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: `sysconf` is a pure query with no pointer arguments.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        (page_size > 0).then(|| pages * page_size as u64)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn current_memory_bytes() -> Option<u64> {
        None
    }

    /// Background sampler recording the high-water mark of the process
    /// memory footprint over a window.
    struct RssSampler {
        peak: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl RssSampler {
        fn start() -> Self {
            let peak = Arc::new(AtomicU64::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let (peak_t, stop_t) = (Arc::clone(&peak), Arc::clone(&stop));
            let handle = std::thread::spawn(move || {
                while !stop_t.load(Ordering::Relaxed) {
                    if let Some(rss) = current_memory_bytes() {
                        peak_t.fetch_max(rss, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                // One final sample so a short window is never empty.
                if let Some(rss) = current_memory_bytes() {
                    peak_t.fetch_max(rss, Ordering::Relaxed);
                }
            });
            RssSampler {
                peak,
                stop,
                handle: Some(handle),
            }
        }

        fn finish(mut self) -> u64 {
            self.shut_down();
            self.peak.load(Ordering::Relaxed)
        }

        fn shut_down(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// Stop the sampler even when the measurement panics part-way through.
    /// Without this an `.expect()` anywhere between `start()` and
    /// `finish()` would leave the poll loop spinning for the rest of the
    /// test process.
    impl Drop for RssSampler {
        fn drop(&mut self) {
            self.shut_down();
        }
    }

    // ── Disk accounting ─────────────────────────────────────────────────

    /// Sum of file sizes under `path`, recursively. Deliberately logical
    /// (not `du`'s allocated blocks): a RocksDB checkpoint hard-links its
    /// SSTs, and the number wanted here is how many bytes the source would
    /// actually have to serve, not how much unique disk it occupies.
    fn dir_size_bytes(path: &Path) -> u64 {
        let mut total = 0;
        let Ok(entries) = std::fs::read_dir(path) else {
            return 0;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                total += dir_size_bytes(&entry.path());
            } else {
                total += meta.len();
            }
        }
        total
    }

    fn mib(bytes: u64) -> f64 {
        bytes as f64 / (1024.0 * 1024.0)
    }

    // ── Source construction ─────────────────────────────────────────────

    /// Deterministic pseudo-random-ish key so insertion order does not
    /// produce a degenerate (perfectly sequential) Merk shape.
    fn scattered_key(i: usize) -> Vec<u8> {
        // Multiply by a large odd constant and take the big-endian bytes:
        // a cheap bijection over `u64` that scatters consecutive `i`.
        (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15).to_be_bytes()[..]
            .iter()
            .copied()
            .chain((i as u32).to_be_bytes())
            .collect()
    }

    /// Deterministic **incompressible** value bytes.
    ///
    /// This matters for the measurement, not just for realism: a filler of
    /// repeated bytes compresses to nothing in RocksDB's SSTs, which would
    /// shrink the reported on-disk size by an order of magnitude and
    /// inflate every "peak memory versus on-disk size" ratio derived from
    /// it.
    /// A xorshift stream keeps the source's on-disk footprint honest.
    fn filler(seed: usize, len: usize) -> Vec<u8> {
        let mut state = (seed as u64).wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.extend_from_slice(&state.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    /// Apply `ops` in fixed-size batches so the *build* side never becomes
    /// the memory story the test is trying to measure on the restore side.
    fn apply_in_batches(db: &GroveDb, ops: Vec<QualifiedGroveDbOp>, grove_version: &GroveVersion) {
        const BATCH: usize = 2_000;
        let mut buf = Vec::with_capacity(BATCH);
        for op in ops {
            buf.push(op);
            if buf.len() == BATCH {
                db.apply_batch(std::mem::take(&mut buf), None, None, grove_version)
                    .unwrap()
                    .expect("batch insert should succeed");
                buf.reserve(BATCH);
            }
        }
        if !buf.is_empty() {
            db.apply_batch(buf, None, None, grove_version)
                .unwrap()
                .expect("final batch insert should succeed");
        }
    }

    /// Build the Platform-shaped grove described by `shape` at `path`.
    fn build_platform_shaped_grove(
        path: &Path,
        shape: &GroveShape,
        grove_version: &GroveVersion,
    ) -> GroveDb {
        let db = GroveDb::open(path).expect("open source grovedb");

        let root_trees: &[(&[u8], Element)] = &[
            (b"identities", Element::empty_tree()),
            (b"documents", Element::empty_tree()),
            (b"balances", Element::empty_sum_tree()),
            (
                b"notes",
                Element::empty_commitment_tree(4).expect("valid chunk power"),
            ),
            (b"history", Element::empty_mmr_tree()),
            (b"idx_count", Element::empty_provable_count_indexed_tree()),
            (b"idx_sum", Element::empty_provable_sum_indexed_tree()),
        ];
        for (key, element) in root_trees {
            db.insert(
                crate::SubtreePath::empty(),
                key,
                element.clone(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert root subtree");
        }

        // Identity-like items: one flat, wide subtree.
        let ops = (0..shape.identities)
            .map(|i| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![b"identities".to_vec()],
                    scattered_key(i),
                    Element::new_item(filler(i, shape.identity_value_bytes)),
                )
            })
            .collect();
        apply_in_batches(&db, ops, grove_version);

        // Document-like items: nested `documents/contract_i/docs/*`.
        for c in 0..shape.contracts {
            let contract = format!("contract_{c}").into_bytes();
            db.insert(
                [b"documents".as_ref()].as_ref(),
                &contract,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert contract subtree");
            db.insert(
                [b"documents".as_ref(), contract.as_ref()].as_ref(),
                b"docs",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert docs subtree");

            let ops = (0..shape.documents_per_contract)
                .map(|i| {
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![b"documents".to_vec(), contract.clone(), b"docs".to_vec()],
                        scattered_key(i),
                        Element::new_item(filler(i + c, shape.document_value_bytes)),
                    )
                })
                .collect();
            apply_in_batches(&db, ops, grove_version);
        }

        // Sum tree.
        let ops = (0..shape.sum_entries)
            .map(|i| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![b"balances".to_vec()],
                    scattered_key(i),
                    Element::new_sum_item((i as i64) * 7 - 3),
                )
            })
            .collect();
        apply_in_batches(&db, ops, grove_version);

        // Commitment tree (non-Merk entry replay + Sinsemilla frontier).
        // `cmx` / `rho` / `cv_net` must be canonical Pallas field elements,
        // so only the low limb varies and the high bytes stay zero.
        let field_element = |v: u64| {
            let mut out = [0u8; 32];
            out[..8].copy_from_slice(&v.to_le_bytes());
            out
        };
        for i in 0..shape.commitment_entries {
            db.commitment_tree_insert_raw(
                crate::SubtreePath::empty(),
                b"notes",
                field_element(i as u64),
                field_element(i as u64 + 1_000_000),
                field_element(i as u64 + 2_000_000),
                filler(i, 216),
                None,
                grove_version,
            )
            .unwrap()
            .expect("append commitment note");
        }

        // MMR tree (non-Merk entry replay).
        for i in 0..shape.mmr_entries {
            db.mmr_tree_append(
                crate::SubtreePath::empty(),
                b"history",
                filler(i, 48),
                None,
                grove_version,
            )
            .unwrap()
            .expect("append mmr leaf");
        }

        // Indexed trees (header page + primary chunks + one ordinary
        // Merk chunk stream per axis secondary).
        for i in 0..shape.indexed_entries {
            db.insert_into_count_indexed_tree(
                [b"idx_count".as_ref()].as_ref(),
                &scattered_key(i),
                Element::empty_provable_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCIT entry");
            db.insert_into_provable_sum_indexed_tree(
                [b"idx_sum".as_ref()].as_ref(),
                &scattered_key(i),
                Element::new_sum_item((i as i64) % 97),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PSIT entry");
        }

        db
    }

    // ── Measurement ─────────────────────────────────────────────────────

    struct Measurement {
        tier: &'static str,
        commit_mode: RestoreCommitMode,
        /// Intermediate commits the session took. Zero in atomic mode.
        intermediate_commits: usize,
        entries: u64,
        logical_bytes: u64,
        checkpoint_bytes: u64,
        restored_bytes: u64,
        baseline_rss: u64,
        /// Footprint sampled at the last moment before `commit_session`, i.e.
        /// with the entire sync write set accumulated in the transaction's
        /// `WriteBatchWithIndex` but nothing yet flushed.
        pre_commit_rss: u64,
        peak_rss: u64,
        wall: Duration,
        build_wall: Duration,
        fetch_calls: u64,
        wire_bytes: u64,
    }

    impl Measurement {
        fn report(&self) {
            println!(
                "\n=== state sync restore memory ceiling: tier {} ===",
                self.tier
            );
            println!("  restore commit mode    : {:>10?}", self.commit_mode);
            println!(
                "  intermediate commits   : {:>10}",
                self.intermediate_commits
            );
            println!(
                "  item value bytes       : {:>10.1} MiB",
                mib(self.logical_bytes)
            );
            println!(
                "  source checkpoint      : {:>10.1} MiB",
                mib(self.checkpoint_bytes)
            );
            println!(
                "  restored target on disk: {:>10.1} MiB",
                mib(self.restored_bytes)
            );
            println!(
                "  wire bytes fetched     : {:>10.1} MiB",
                mib(self.wire_bytes)
            );
            println!("  fetch_chunk round trips: {:>10}", self.fetch_calls);
            println!(
                "  build wall-clock       : {:>10.1} s",
                self.build_wall.as_secs_f64()
            );
            println!(
                "  restore wall-clock     : {:>10.1} s",
                self.wall.as_secs_f64()
            );
            println!(
                "  mem baseline (pre-sync): {:>10.1} MiB",
                mib(self.baseline_rss)
            );
            println!(
                "  mem before commit      : {:>10.1} MiB",
                mib(self.pre_commit_rss)
            );
            println!(
                "  mem peak  (during sync): {:>10.1} MiB",
                mib(self.peak_rss)
            );
            // Which of the two costs dominates decides the remedy: if the
            // peak is already reached before commit, the write batch is
            // the ceiling and only a scratch/staging strategy moves it; if
            // the peak arrives during commit, it is RocksDB's flush and is
            // tunable with write-buffer settings.
            println!(
                "  write-batch share      : {:>10.1} % of the increment is present pre-commit",
                100.0 * self.pre_commit_rss.saturating_sub(self.baseline_rss) as f64
                    / self.peak_rss.saturating_sub(self.baseline_rss).max(1) as f64
            );
            println!(
                "  mem increment          : {:>10.1} MiB",
                mib(self.peak_rss.saturating_sub(self.baseline_rss))
            );
            println!(
                "  peak mem / checkpoint  : {:>10.2} x",
                self.peak_rss as f64 / self.checkpoint_bytes.max(1) as f64
            );
            println!(
                "  increment / checkpoint : {:>10.2} x",
                self.peak_rss.saturating_sub(self.baseline_rss) as f64
                    / self.checkpoint_bytes.max(1) as f64
            );
            println!("  item entries restored  : {:>10}", self.entries);
            println!(
                "  increment / entry      : {:>10.1} B",
                self.peak_rss.saturating_sub(self.baseline_rss) as f64 / self.entries.max(1) as f64
            );
        }
    }

    /// Build → checkpoint → restore → commit, measuring the restore window.
    fn measure_restore(shape: &GroveShape) -> Measurement {
        let grove_version = GroveVersion::latest();
        let work = TempDir::new().expect("temp work dir");

        let source_path = work.path().join("source");
        let checkpoint_path = work.path().join("checkpoint");
        let target_path = work.path().join("target");
        std::fs::create_dir_all(&source_path).expect("create source dir");
        std::fs::create_dir_all(&target_path).expect("create target dir");

        let build_start = Instant::now();
        let source = build_platform_shaped_grove(&source_path, shape, grove_version);
        let build_wall = build_start.elapsed();
        let source_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("source root hash");
        source
            .create_checkpoint(&checkpoint_path)
            .expect("create checkpoint");
        // Drop the source DB before measuring: only the checkpoint (the
        // "remote peer") and the target participate in the restore, so the
        // builder's block cache and memtables must not be counted.
        drop(source);

        let checkpoint_bytes = dir_size_bytes(&checkpoint_path);
        let checkpoint_db = GroveDb::open(&checkpoint_path).expect("open checkpoint db");
        let target = GroveDb::open(&target_path).expect("open target db");

        let baseline_rss = current_memory_bytes().unwrap_or(0);
        let sampler = RssSampler::start();
        let sync_start = Instant::now();

        let commit_mode = commit_mode_from_env();
        let mut session = target
            .start_snapshot_syncing_with_mode(
                source_hash,
                64,
                CURRENT_STATE_SYNC_VERSION,
                commit_mode,
                grove_version,
            )
            .expect("start snapshot syncing");

        let mut queue: VecDeque<Vec<u8>> = VecDeque::new();
        queue.push_back(source_hash.to_vec());
        let mut fetch_calls = 0u64;
        let mut wire_bytes = 0u64;

        while let Some(chunk_id) = queue.pop_front() {
            let chunk_data = checkpoint_db
                .fetch_chunk(
                    chunk_id.as_slice(),
                    None,
                    CURRENT_STATE_SYNC_VERSION,
                    grove_version,
                )
                .expect("fetch chunk");
            fetch_calls += 1;
            wire_bytes += chunk_data.len() as u64;
            let more = session
                .apply_chunk(
                    chunk_id.as_slice(),
                    &chunk_data,
                    CURRENT_STATE_SYNC_VERSION,
                    grove_version,
                )
                .expect("apply chunk");
            queue.extend(more);
        }

        assert!(session.is_sync_completed(), "sync should have completed");
        // The whole sync write set is in the transaction's write batch and
        // nothing has been flushed yet: this reading separates the batch's
        // cost from the commit-time flush that follows.
        let pre_commit_rss = current_memory_bytes().unwrap_or(0);
        let intermediate_commits = session.intermediate_commits();
        target
            .commit_session(session, grove_version)
            .expect("commit session");

        let wall = sync_start.elapsed();
        let peak_rss = sampler.finish();

        assert_eq!(
            target.root_hash(None, grove_version).unwrap().unwrap(),
            source_hash,
            "restored root hash must match the source app hash"
        );

        drop(checkpoint_db);
        drop(target);
        let restored_bytes = dir_size_bytes(&target_path);

        Measurement {
            tier: shape.tier,
            commit_mode,
            intermediate_commits,
            entries: shape.entry_count(),
            logical_bytes: shape.logical_value_bytes(),
            checkpoint_bytes,
            restored_bytes,
            baseline_rss,
            pre_commit_rss,
            peak_rss,
            wall,
            build_wall,
            fetch_calls,
            wire_bytes,
        }
    }

    fn run_tier(shape: &GroveShape) {
        let measurement = measure_restore(shape);
        measurement.report();
    }

    #[test]
    #[ignore = "measurement harness: minutes of runtime, run explicitly in --release"]
    fn restore_memory_ceiling_tier_tiny() {
        run_tier(&TIER_TINY);
    }

    #[test]
    #[ignore = "measurement harness: minutes of runtime, run explicitly in --release"]
    fn restore_memory_ceiling_tier_small() {
        run_tier(&TIER_SMALL);
    }

    #[test]
    #[ignore = "measurement harness: minutes of runtime and >1 GiB of disk"]
    fn restore_memory_ceiling_tier_medium() {
        run_tier(&TIER_MEDIUM);
    }

    #[test]
    #[ignore = "measurement harness: tens of minutes and >4 GiB of disk and RAM"]
    fn restore_memory_ceiling_tier_large() {
        run_tier(&TIER_LARGE);
    }

    #[test]
    #[ignore = "measurement harness: write-batch data vs index attribution"]
    fn restore_memory_shape_value_heavy() {
        run_tier(&SHAPE_VALUE_HEAVY);
    }

    #[test]
    #[ignore = "measurement harness: write-batch data vs index attribution"]
    fn restore_memory_shape_key_heavy() {
        run_tier(&SHAPE_KEY_HEAVY);
    }
}
