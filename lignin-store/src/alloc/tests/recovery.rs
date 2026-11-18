//! The allocator of a recovered database, from the page table and replay.

use std::collections::BTreeMap;

use lignin_format::FileId;

use super::*;
use crate::{
    segment::BlockLocation,
    table::{Link, PageTable},
};

/// A location no block was written to: the table reads no block.
fn at(offset: u64) -> BlockLocation {
    BlockLocation {
        file: FileId::new(1).expect("not zero"),
        offset: 20 + 64 * offset,
        len: 64,
    }
}

fn base(raw_version: u64) -> Link {
    Link::base(version(raw_version), at(raw_version))
}

fn delta(raw_version: u64) -> Link {
    Link::delta(version(raw_version), at(raw_version))
}

fn free(raw_version: u64) -> Link {
    Link::free(version(raw_version))
}

fn table_of(chains: &[(u32, &[Link])]) -> PageTable {
    let table = PageTable::new();
    for (raw, chain) in chains {
        for link in *chain {
            table.push(page(*raw), *link).expect("the chain is valid");
        }
    }
    table
}

fn states(ids: &PageAllocator, up_to: u32) -> Vec<PageState> {
    (1..=up_to).map(|raw| ids.state(page(raw))).collect()
}

/// A registry in which the commit of `latest` is published and nothing is
/// held, as when a database has just been opened.
fn opened_at(latest: u64) -> Registry {
    let pins = Registry::default();
    pins.publish(latest);
    pins
}

use PageState::{Free, Live, Retired};

fn recovered(replay: Recovery, table: &PageTable, landed: u64) -> PageAllocator {
    replay
        .finish(table, version(landed))
        .expect("nothing is above the version landed on")
}

#[test]
fn an_empty_table_and_no_replay_give_an_empty_allocator() {
    let ids = recovered(Recovery::new(), &PageTable::new(), 0);
    assert_eq!(ids.census().high_water, None);
    assert_eq!(in_use(&ids), []);
    assert_eq!(ids.allocate(), Ok(page(1)));
}

#[test]
fn the_table_says_which_ids_are_in_use_and_which_were_freed() {
    let table = table_of(&[
        (1, &[base(1)]),
        (2, &[base(1), delta(2)]),
        (3, &[base(1), free(3)]),
        (5, &[free(4)]),
        (6, &[base(2), free(3), base(5)]),
        (4100, &[base(2), delta(2)]),
    ]);
    let ids = recovered(Recovery::default(), &table, 5);
    assert_eq!(
        states(&ids, 7),
        [Live, Live, Retired, Free, Retired, Live, Free]
    );
    assert_eq!(ids.state(page(4100)), Live);
    assert_eq!(
        retired(&ids),
        [(page(3), version(3)), (page(5), version(4))]
    );
    let census = ids.census();
    assert_eq!(census.high_water, Some(page(4100)));
    assert_eq!((census.private, census.live, census.retired), (0, 4, 2));

    assert_eq!(ids.allocate(), Ok(page(4)), "a freed id is not free yet");
    assert_eq!(reclaim(&ids, &opened_at(5)), pages([3, 5]));
    for raw in [3, 5, 7, 8] {
        assert_eq!(ids.allocate(), Ok(page(raw)));
    }
}

#[test]
fn replay_adds_the_pages_written_and_freed_after_the_checkpoint() {
    let table = table_of(&[(1, &[base(1)]), (2, &[base(2)]), (3, &[base(1), free(3)])]);
    let mut replay = Recovery::new();
    replay.wrote(page(2), version(4));
    replay.wrote(page(7), version(5));
    replay.freed(page(1), version(6));
    replay.wrote(page(3), version(7));
    replay.wrote(page(9), version(7));
    replay.freed(page(9), version(8));
    replay.freed(page(11), version(8));
    let ids = recovered(replay, &table, 8);
    assert_eq!(
        states(&ids, 11),
        [Retired, Live, Live, Free, Free, Free, Live, Free, Retired, Free, Retired]
    );
    assert_eq!(
        retired(&ids),
        [
            (page(1), version(6)),
            (page(9), version(8)),
            (page(11), version(8))
        ]
    );
    assert_eq!(ids.census().high_water, Some(page(11)));
    assert_eq!(reclaim(&ids, &opened_at(8)), pages([1, 9, 11]));
}

#[test]
fn what_replay_reports_at_or_below_a_page_s_newest_link_is_ignored() {
    let table = table_of(&[
        (1, &[base(5)]),
        (2, &[base(1), free(3)]),
        (3, &[free(6)]),
        (4, &[base(2), free(4), base(7)]),
        (5, &[base(3), delta(8)]),
    ]);
    let mut replay = Recovery::new();
    replay.freed(page(1), version(4));
    replay.wrote(page(1), version(5));
    replay.wrote(page(2), version(2));
    replay.wrote(page(2), version(3));
    replay.freed(page(3), version(6));
    replay.freed(page(4), version(4));
    replay.wrote(page(4), version(7));
    replay.freed(page(5), version(8));
    let ids = recovered(replay, &table, 8);
    assert_eq!(states(&ids, 5), [Live, Retired, Retired, Live, Live]);
    assert_eq!(
        retired(&ids),
        [(page(2), version(3)), (page(3), version(6))]
    );

    let mut replay = Recovery::new();
    replay.freed(page(1), version(6));
    replay.wrote(page(2), version(4));
    replay.wrote(page(3), version(7));
    replay.freed(page(4), version(8));
    replay.freed(page(5), version(9));
    let ids = recovered(replay, &table, 9);
    assert_eq!(states(&ids, 5), [Retired, Live, Live, Retired, Retired]);
}

#[test]
fn the_newest_report_of_a_page_counts_whatever_the_order() {
    let reports: [&[(u64, bool)]; 6] = [
        &[(3, false), (5, true)],
        &[(5, true), (3, false)],
        &[(3, false), (5, true), (9, false)],
        &[(9, false), (5, true), (3, false)],
        &[(4, false), (4, true)],
        &[(4, true), (4, false)],
    ];
    let expected = [Retired, Retired, Live, Live, Retired, Retired];
    for (reports, expected) in reports.into_iter().zip(expected) {
        let mut replay = Recovery::new();
        for (raw_version, freed) in reports {
            if *freed {
                replay.freed(page(8), version(*raw_version));
            } else {
                replay.wrote(page(8), version(*raw_version));
            }
        }
        let ids = recovered(replay, &PageTable::new(), 9);
        assert_eq!(ids.state(page(8)), expected, "{reports:?}");
    }
}

#[test]
fn a_link_or_a_report_above_the_version_landed_on_is_refused() {
    let table = table_of(&[(1, &[base(1)]), (2, &[base(2), delta(6)]), (3, &[free(4)])]);
    let after = |raw_page, raw_version, landed| RecoveryError::LinkAfterLanding {
        page: page(raw_page),
        version: version(raw_version),
        landed: version(landed),
    };
    assert_eq!(
        Recovery::new().finish(&table, version(5)).err(),
        Some(after(2, 6, 5))
    );
    assert_eq!(
        Recovery::new().finish(&table, version(3)).err(),
        Some(after(2, 6, 3))
    );
    assert_eq!(
        Recovery::new()
            .finish(&table_of(&[(3, &[free(4)])]), version(3))
            .err(),
        Some(after(3, 4, 3))
    );
    assert!(Recovery::new().finish(&table, version(6)).is_ok());

    for freed in [false, true] {
        let mut replay = Recovery::new();
        replay.wrote(page(1), version(6));
        if freed {
            replay.freed(page(1), version(7));
        } else {
            replay.wrote(page(9), version(7));
        }
        let reported = if freed { 1 } else { 9 };
        assert_eq!(
            replay.finish(&table, version(6)).err(),
            Some(RecoveryError::ReportAfterLanding {
                page: page(reported),
                version: version(7),
                landed: version(6),
            })
        );
    }

    let mut replay = Recovery::new();
    replay.freed(page(4), CommitVersion::EMPTY);
    assert_eq!(
        replay.finish(&table, version(6)).err(),
        Some(RecoveryError::ReportOfNoCommit { page: page(4) })
    );
    let mut replay = Recovery::new();
    replay.wrote(page(4), CommitVersion::EMPTY);
    replay.wrote(page(4), version(6));
    assert_eq!(recovered(replay, &table, 6).state(page(4)), Live);
}

#[test]
fn a_free_that_the_crash_lost_does_not_retire_a_page_that_is_in_use() {
    // Commit 7 freed page 1 and was lost: recovery landed on version 5, and
    // the next commits take versions 6 and 7 again.
    let table = table_of(&[(1, &[base(1)])]);
    let mut replay = Recovery::new();
    replay.freed(page(1), version(7));
    assert!(replay.finish(&table, version(5)).is_err());
    let ids = recovered(Recovery::new(), &table, 5);
    assert_eq!(reclaim(&ids, &opened_at(7)), []);
    assert_eq!(ids.state(page(1)), Live);
}

#[test]
fn recovery_errors_print() {
    let cases = [
        (
            RecoveryError::LinkAfterLanding {
                page: page(2),
                version: version(6),
                landed: version(5),
            },
            "the page table holds page 2 at version 6, and recovery landed on version 5",
        ),
        (
            RecoveryError::ReportAfterLanding {
                page: page(3),
                version: version(8),
                landed: version(7),
            },
            "replay reported page 3 for version 8, and recovery landed on version 7",
        ),
        (
            RecoveryError::ReportOfNoCommit { page: page(4) },
            "replay reported page 4 for version 0",
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
        let boxed: Box<dyn std::error::Error> = Box::new(error);
        assert!(boxed.source().is_none());
    }
}

#[test]
fn a_recovery_prints() {
    let mut replay = Recovery::new();
    replay.wrote(page(2), version(4));
    assert_eq!(
        format!("{replay:?}"),
        "Recovery { replayed: {page#2: (v4, Wrote)} }"
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Created,
    Changed,
    Freed,
}

/// What every commit of a drawn history did to each page id.
fn history(rng: &mut Rng, commits: u64, ids: u32) -> BTreeMap<u32, Vec<(u64, Event)>> {
    let mut events: BTreeMap<u32, Vec<(u64, Event)>> = BTreeMap::new();
    for commit in 1..=commits {
        for raw in 1..=ids {
            let of = events.entry(raw).or_default();
            let live = of.last().is_some_and(|(_, event)| *event != Event::Freed);
            let event = match (live, rng.below(8)) {
                (false, 0 | 1) => Event::Created,
                (true, 0 | 1) => Event::Changed,
                (true, 2) => Event::Freed,
                _ => continue,
            };
            of.push((commit, event));
        }
    }
    events
}

/// A chain that holds every event of one page up to `through`: the links
/// of the page as it is then, and maybe what a trim has not removed yet.
/// Returns whether the page was freed by then and its chain is gone.
fn push_chain(
    rng: &mut Rng,
    table: &PageTable,
    raw: u32,
    events: &[(u64, Event)],
    through: u64,
) -> bool {
    let held: Vec<_> = events.iter().filter(|(at, _)| *at <= through).collect();
    let Some((_, last)) = held.last() else {
        return false;
    };
    let trimmed = rng.below(2) == 0;
    if *last == Event::Freed && trimmed {
        return true;
    }
    let start = if trimmed {
        held.iter().rposition(|(_, event)| *event == Event::Created)
    } else {
        Some(0)
    };
    for (commit, event) in &held[start.expect("a page is created first")..] {
        let link = match event {
            Event::Created => base(*commit),
            Event::Changed => delta(*commit),
            Event::Freed => free(*commit),
        };
        table.push(page(raw), link).expect("the chain is valid");
    }
    false
}

#[test]
fn drawn_histories_recover_the_allocator_of_the_version_landed_on() {
    let (mut live, mut waiting, mut ignored) = (0, 0, 0);
    for seed in 1..=300 {
        let mut rng = Rng::new(seed);
        let (commits, ids) = (12, 40);
        let events = history(&mut rng, commits, ids);
        let covers = rng.below(commits + 1);
        let landed = covers + rng.below(commits + 1 - covers);

        let table = PageTable::new();
        let mut replay = Recovery::new();
        let mut reports = Vec::new();
        let mut expected = BTreeMap::new();
        for (raw, events) in &events {
            let through = covers + rng.below(landed + 1 - covers);
            let gone = push_chain(&mut rng, &table, *raw, events, through);
            for (commit, event) in events {
                if (covers + 1..=landed).contains(commit) {
                    reports.push((*raw, *commit, *event));
                    ignored += u64::from(*commit <= through);
                }
            }
            let state = match events.iter().rev().find(|(at, _)| *at <= landed) {
                None => continue,
                Some((freed_at, Event::Freed)) if gone && *freed_at <= covers => continue,
                Some((freed_at, Event::Freed)) => (Retired, Some(version(*freed_at))),
                Some(_) => (Live, None),
            };
            expected.insert(page(*raw), state);
        }
        for at in (1..reports.len()).rev() {
            reports.swap(at, rng.below(at as u64 + 1) as usize);
        }
        for (raw, commit, event) in reports {
            match event {
                Event::Freed => replay.freed(page(raw), version(commit)),
                _ => replay.wrote(page(raw), version(commit)),
            }
        }

        let ids = recovered(replay, &table, landed);
        let found = in_use(&ids);
        let states: Vec<_> = expected
            .iter()
            .map(|(id, (state, _))| (*id, *state))
            .collect();
        assert_eq!(found, states, "seed {seed}");
        let freed: Vec<_> = expected
            .iter()
            .filter_map(|(id, (_, freed_at))| Some((*id, (*freed_at)?)))
            .collect();
        assert_eq!(retired(&ids), freed, "seed {seed}");
        assert_eq!(ids.census().private, 0);
        live += ids.census().live;
        waiting += freed.len();

        let released: Vec<_> = freed.iter().map(|(id, _)| *id).collect();
        assert_eq!(reclaim(&ids, &opened_at(landed)), released, "seed {seed}");
        let lowest = (1..)
            .map(page)
            .find(|id| expected.get(id).is_none_or(|(state, _)| *state != Live));
        assert_eq!(ids.allocate().ok(), lowest, "seed {seed}");
    }
    assert!(
        live > 1000 && waiting > 1000 && ignored > 1000,
        "{live} {waiting} {ignored}"
    );
}
