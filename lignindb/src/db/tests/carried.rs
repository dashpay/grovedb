//! How many values out of line one small write carries (a measurement).
//!
//! A value of more than the inline threshold is stored out of line, and its
//! cell holds the place of the value. When a write moves a cell to another
//! page, at a split or a merge, the value goes with it: the commit reads it
//! whole from the value log and writes it into its log group again, for the
//! page the cell is in now (`log-format.md` LG23). So a write of a few
//! bytes can read, hold and log the values of every cell it carries.
//!
//! The test makes the worst case on purpose: a Merk whose every node has a
//! value out of line, and then nodes of eight bytes put into it one commit
//! at a time, at the end, at the start and between the others, and then the
//! large nodes taken out one commit at a time. Of each commit it prints the
//! bytes of the value records of its log group
//! ([`Published::value_bytes`]), which for these commits are the carried
//! values and nothing else, and how far the resident set of the process
//! grew while the commit ran.
//!
//! ```text
//! MALLOC_MMAP_THRESHOLD_=16384 LIGNIN_CARRIED_VALUE_LEN=1048576 \
//!   cargo test -p lignindb --lib -- --ignored --exact --nocapture \
//!   db::tests::carried::the_values_that_one_small_write_carries
//! ```

use std::fs;

use lignin_format::PageSize;

use super::{options, temp};
use crate::{
    backup::tests::world::{batch, commit, first, height, key, value, State},
    commit::{Committed, Published},
    txn::tests::model::{self, OwnedBatch, OwnedSubtree},
    Db, Options,
};

/// The items of a Merk, as the fixture that edits them has them.
type Items = std::collections::BTreeMap<Vec<u8>, Vec<u8>>;

/// The Merk of these commits: one that stands alone.
const MERK: u8 = 4;

/// The number of the first large node's key: there is room below it.
const FIRST: u32 = 1_000_000;

/// The resident set of the process now, and its peak since the last reset
/// of the peak, in kibibytes: zero where the system does not say.
fn resident() -> (u64, u64) {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| -> u64 {
        let line = status.lines().find(|line| line.starts_with(name));
        let value = line.and_then(|line| line.split_whitespace().nth(1));
        value.and_then(|kb| kb.parse().ok()).unwrap_or(0)
    };
    (field("VmRSS:"), field("VmHWM:"))
}

/// What one commit carried: the bytes of its value records, how many pages
/// they are for, the pages it made and freed, and how far the resident set
/// grew while it ran.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Carried {
    value_bytes: u64,
    value_pages: usize,
    created: usize,
    freed: usize,
    growth_kb: u64,
}

fn committed(db: &Db, state: &mut State, batch: &OwnedBatch) -> Carried {
    let _ = fs::write("/proc/self/clear_refs", "5");
    let (before, _) = resident();
    let published: Box<Published> = match commit(db, batch, false) {
        Ok(Committed::Published(published)) => published,
        other => panic!("{other:?}"),
    };
    let growth_kb = resident().1.saturating_sub(before);
    state.commit(batch);
    Carried {
        value_bytes: published.value_bytes,
        value_pages: published.value_pages,
        created: published.created.len(),
        freed: published.freed.len(),
        growth_kb,
    }
}

/// One run: `count` large nodes, then 300 small ones put where `place`
/// says, then the large ones taken out. Prints the commit that carried
/// most among the puts and among the removals.
fn run(page_size: PageSize, count: u32, value_len: usize, place: &str) {
    let dir = temp();
    let options = Options {
        page_size: Some(page_size),
        ..options()
    };
    let db = Db::open(dir.path(), &options).expect("a new database");
    let mut state = State::new();
    let begun = first(&state);
    committed(&db, &mut state, &begun);
    let mut number = 1;
    let mut next = |state: &State, edit: &dyn Fn(&mut Items)| {
        number += 1;
        let enter = match state.has(MERK) {
            true => Vec::new(),
            false => vec![OwnedSubtree::Enter(model::subtree(MERK), None)],
        };
        batch(vec![state.merk(MERK, edit)], vec![height(number)], enter)
    };
    let large = next(&state, &|items| {
        for n in 0..count {
            items.insert(key(b'm', FIRST + n * 16), value(n, 1, value_len));
        }
    });
    committed(&db, &mut state, &large);

    let mut puts = Vec::new();
    for n in 0..300u32 {
        let at = match place {
            "at the end" => FIRST + count * 16 + n,
            "at the start" => FIRST - 1 - n,
            _ => FIRST + (((n * 7919) % (count * 16)) | 1),
        };
        let small = next(&state, &|items| {
            items.insert(key(b'm', at), value(n, 2, 8));
        });
        puts.push(committed(&db, &mut state, &small));
    }
    let mut removals = Vec::new();
    for n in 0..count {
        let gone = next(&state, &|items| {
            items.remove(&key(b'm', FIRST + n * 16));
        });
        removals.push(committed(&db, &mut state, &gone));
    }
    db.close().expect("the session ends");

    for (what, commits) in [("puts of 8 bytes", &puts), ("removals", &removals)] {
        let worst = commits.iter().max().copied().unwrap_or_default();
        let carrying = commits
            .iter()
            .filter(|carried| carried.value_bytes > 0)
            .count();
        let most_memory = commits
            .iter()
            .map(|carried| carried.growth_kb)
            .max()
            .unwrap_or(0);
        println!(
            "CARRIED page={} large_nodes={count} value_len={value_len} {place}: {what}: {} commits, \
             {carrying} carry a value; the most: {} bytes of values ({:.1} of them) for {} pages, \
             {} pages made, {} freed; the largest growth of a commit {most_memory} KiB",
            page_size.get(),
            commits.len(),
            worst.value_bytes,
            worst.value_bytes as f64 / value_len as f64,
            worst.value_pages,
            worst.created,
            worst.freed,
        );
    }
}

/// The most that one small write carries, for two page sizes, Merks of
/// several sizes and three places of the small writes.
/// `LIGNIN_CARRIED_VALUE_LEN` sets the length of the large values (3,000
/// bytes if it is not set).
#[test]
#[ignore = "a measurement"]
fn the_values_that_one_small_write_carries() {
    let value_len = std::env::var("LIGNIN_CARRIED_VALUE_LEN")
        .map_or(3000, |len| len.parse().expect("a length"));
    // A Merk of large values is put by one batch, which holds them all.
    let counts: &[u32] = match value_len {
        0..65_536 => &[24, 96, 384],
        _ => &[24, 96],
    };
    for page_size in [PageSize::MIN, PageSize::DEFAULT] {
        for &count in counts {
            for place in ["at the end", "at the start", "between"] {
                run(page_size, count, value_len, place);
            }
        }
    }
}
