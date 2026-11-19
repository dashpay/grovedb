//! Tells the crate whether the GroveDB it is built against has the `Engine`
//! seam.
//!
//! On the branch it has: `GroveDb<E>` is generic over its engine, and this
//! crate applies a stream to any engine. The golden corpus is recorded from
//! upstream's engine at a pinned commit, with this crate taken from the
//! branch (`corpus/README.md`), and upstream's `GroveDb` names RocksDB. There
//! the crate builds in the form that names no engine parameter, and without
//! the differential fuzzer, which needs two engines.
//!
//! The seam is `grovedb/src/engine/mod.rs`, next to this crate in every
//! workspace it is built in.

use std::{env, path::PathBuf};

/// Set, it builds the crate in the form it has against the pinned engine
/// although the seam is there: a check that this form still compiles, short
/// of the recording recipe itself.
const PINNED_FORM: &str = "LIGNIN_TESTKIT_PINNED_FORM";

fn main() {
    println!("cargo::rustc-check-cfg=cfg(grovedb_engine_seam)");
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"));
    let seam = manifest_dir.join("../grovedb/src/engine/mod.rs");
    println!("cargo::rerun-if-changed={}", seam.display());
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed={PINNED_FORM}");
    if seam.is_file() && env::var_os(PINNED_FORM).is_none() {
        println!("cargo::rustc-cfg=grovedb_engine_seam");
    }
}
