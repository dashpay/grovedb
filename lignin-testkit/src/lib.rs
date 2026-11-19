//! LigninDB test kit: workloads, golden corpus, differential fuzzer, crash simulation and benchmarks

#![forbid(unsafe_code)]

pub mod bench;
pub mod cost_model;
// Two engines are compared through the `Engine` seam, which the engine the
// golden corpus is pinned to does not have (`build.rs`).
#[cfg(grovedb_engine_seam)]
pub mod diff;
pub mod golden;
pub mod layout_sim;
pub mod workload;
