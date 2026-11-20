//! What a pool costs to make, and what the first use of a frame costs:
//! measurements, run by hand in a release build.
//!
//! ```text
//! POOL_MIB=1024 cargo test --release -p lignin-store --lib -- \
//!     --ignored --nocapture cache::tests::measure
//! ```
//!
//! | Setting | Default | What it sets |
//! | --- | --- | --- |
//! | `POOL_MIB` | 128 | the configured size of the pool; with 0 no pool is made |
//! | `PAGE` | 32768 | the page size in bytes |
//! | `ROUNDS` | 5 | `making_a_pool`: how many pools are made, one after the other |
//! | `PASSES` | 2 | `loading_into_frames`: how often every frame is loaded into |
//! | `HITS` | 1 | `loading_into_frames`: how often every page in the pool is then read |
//!
//! Each test prints the time on the clock. For instructions, a test is run
//! alone under `perf stat` with one setting changed (no pool, one pass
//! more), and the difference is what the change costs.

use std::{hint::black_box, time::Instant};

use lignin_format::PageSize;

use super::*;

fn setting(name: &str, default: usize) -> usize {
    std::env::var(name).map_or(default, |text| text.parse().expect("a number"))
}

fn config() -> Option<PoolConfig> {
    let mib = setting("POOL_MIB", 128);
    (mib > 0).then(|| PoolConfig {
        page_size: PageSize::new(setting("PAGE", 32_768)).expect("a page size"),
        capacity: mib << 20,
    })
}

/// The memory of the process that is backed now, in KiB, where the system
/// tells it in a file.
fn resident() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?;
    line.trim().strip_suffix("kB")?.trim().parse().ok()
}

fn median(mut times: Vec<u128>) -> u128 {
    times.sort_unstable();
    times[times.len() / 2]
}

#[test]
#[ignore = "a measurement, run by hand"]
fn making_a_pool() {
    let Some(config) = config() else {
        println!("no pool is made");
        return;
    };
    let (mut made, mut dropped) = (Vec::new(), Vec::new());
    for round in 1..=setting("ROUNDS", 5) {
        let before = resident();
        let start = Instant::now();
        let pool = BufferPool::new(config).expect("holds frames");
        made.push(start.elapsed().as_micros());
        let grew = resident()
            .zip(before)
            .map_or("?".to_owned(), |(after, before)| {
                after.saturating_sub(before).to_string()
            });
        let frames = pool.frames();
        let start = Instant::now();
        drop(black_box(pool));
        dropped.push(start.elapsed().as_micros());
        println!(
            "round {round}: {frames} frames made in {} us, {grew} KiB more backed, dropped in {} us",
            made[made.len() - 1],
            dropped[dropped.len() - 1],
        );
    }
    println!(
        "a pool of {} MiB with pages of {} bytes: made in {} us, dropped in {} us (medians)",
        config.capacity >> 20,
        config.page_size.get(),
        median(made),
        median(dropped),
    );
}

/// Loads page `raw` as a load does: a miss, a blank frame written from end
/// to end, and the frame installed.
fn load(pool: &BufferPool, raw: u32) {
    let Err(miss) = pool.get(page(raw), version(1)) else {
        panic!("the page is not in the pool");
    };
    let mut blank = pool.blank();
    blank.fill(raw as u8);
    match pool
        .install_loaded(blank, miss, version(1), None)
        .expect("a load")
    {
        Loaded::Page(loaded) => {
            black_box(loaded[0]);
        }
        Loaded::Again(_) => panic!("nothing else changes the pool"),
    }
}

#[test]
#[ignore = "a measurement, run by hand"]
fn loading_into_frames() {
    let Some(config) = config() else {
        println!("no pool is made");
        return;
    };
    let pool = BufferPool::new(config).expect("holds frames");
    let frames = pool.frames() as u32;
    println!(
        "a pool of {} MiB with pages of {} bytes: {frames} frames",
        config.capacity >> 20,
        config.page_size.get(),
    );
    let passes = setting("PASSES", 2) as u32;
    for pass in 0..passes {
        let before = resident();
        let start = Instant::now();
        for raw in 1..=frames {
            load(&pool, pass * frames + raw);
        }
        let each = start.elapsed().as_nanos() / u128::from(frames);
        let grew = resident()
            .zip(before)
            .map_or("?".to_owned(), |(after, before)| {
                after.saturating_sub(before).to_string()
            });
        println!(
            "pass {}: a load into every frame, {each} ns a load, {grew} KiB more backed",
            pass + 1
        );
    }
    if passes == 0 {
        return;
    }
    assert_eq!(pool.overflow_frames(), 0);
    for round in 1..=setting("HITS", 1) {
        let last = (passes - 1) * frames;
        let start = Instant::now();
        for raw in 1..=frames {
            let found = pool.get(page(last + raw), version(1)).expect("in the pool");
            black_box(found[0]);
        }
        let each = start.elapsed().as_nanos() / u128::from(frames);
        println!("hits {round}: a hit on every page in the pool, {each} ns a hit");
    }
}
