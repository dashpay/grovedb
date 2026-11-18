//! The allocator, one call at a time.

use std::collections::BTreeMap;

use super::*;

/// An allocator with `count` committed pages, ids 1 to `count`.
fn with_live(count: u32) -> PageAllocator {
    let ids = PageAllocator::new();
    for raw in 1..=count {
        assert_eq!(ids.allocate(), Ok(page(raw)));
        ids.publish(page(raw)).expect("it is private");
    }
    ids
}

#[test]
fn ids_are_handed_out_from_one_up() {
    let ids = PageAllocator::default();
    assert_eq!(ids.census().high_water, None);
    for raw in 1..=5000 {
        assert_eq!(ids.allocate(), Ok(page(raw)));
        assert_eq!(ids.state(page(raw)), PageState::Private);
    }
    assert_eq!(ids.state(page(5001)), PageState::Free);
    let census = ids.census();
    assert_eq!(census.high_water, Some(page(5000)));
    assert_eq!((census.private, census.live, census.retired), (5000, 0, 0));
    assert_eq!(census.free_below_high_water(), 0);
}

#[test]
fn an_id_that_is_given_back_is_free_at_once() {
    let ids = PageAllocator::new();
    let pins = Registry::default();
    let held: Vec<_> = (0..4).map(|_| ids.allocate().expect("an id")).collect();
    assert_eq!(held, pages(1..=4));

    pins.pin();
    let guard = pins.guard();
    ids.release(page(2)).expect("it is private");
    assert_eq!(ids.state(page(2)), PageState::Free);
    assert_eq!(ids.allocate(), Ok(page(2)));

    ids.release(page(3)).expect("it is private");
    ids.release(page(1)).expect("it is private");
    assert_eq!(ids.allocate(), Ok(page(1)));
    assert_eq!(ids.allocate(), Ok(page(3)));
    assert_eq!(ids.allocate(), Ok(page(5)));
    assert_eq!(reclaim(&ids, &pins), []);
    pins.unguard(guard);
}

#[test]
fn a_published_id_names_a_committed_page() {
    let ids = PageAllocator::new();
    let first = ids.allocate().expect("an id");
    ids.publish(first).expect("it is private");
    assert_eq!(ids.state(first), PageState::Live);
    assert_eq!(ids.allocate(), Ok(page(2)));
    let census = ids.census();
    assert_eq!((census.private, census.live, census.retired), (1, 1, 0));
}

#[test]
fn a_call_that_does_not_fit_the_state_is_refused_and_changes_nothing() {
    let ids = PageAllocator::new();
    let pins = Registry::default();
    let private = ids.allocate().expect("an id");
    let live = ids.allocate().expect("an id");
    let retired = ids.allocate().expect("an id");
    let free = page(9);
    ids.publish(live).expect("it is private");
    ids.publish(retired).expect("it is private");
    pins.publish(1);
    pins.pin();
    ids.free(retired, version(2)).expect("it is live");
    pins.publish(2);

    let states = [
        (free, PageState::Free),
        (private, PageState::Private),
        (live, PageState::Live),
        (retired, PageState::Retired),
    ];
    let census = ids.census();
    for (id, state) in states {
        let not_private = AllocError::NotPrivate { page: id, state };
        let not_live = AllocError::NotLive { page: id, state };
        if state != PageState::Private {
            assert_eq!(ids.release(id), Err(not_private));
            assert_eq!(ids.publish(id), Err(not_private));
        }
        if state != PageState::Live {
            assert_eq!(ids.free(id, version(3)), Err(not_live));
        }
        assert_eq!(ids.state(id), state);
    }
    assert_eq!(
        ids.free(live, CommitVersion::EMPTY),
        Err(AllocError::FreedByNoCommit { page: live })
    );
    assert_eq!(ids.state(live), PageState::Live);
    assert_eq!(ids.census(), census);
    assert_eq!(super::retired(&ids), [(retired, version(2))]);
    assert_eq!(reclaim(&ids, &pins), []);
}

fn pick(model: &BTreeMap<u32, PageState>, rng: &mut Rng, state: PageState) -> Option<u32> {
    let of: Vec<u32> = model
        .iter()
        .filter(|(_, found)| **found == state)
        .map(|(raw, _)| *raw)
        .collect();
    (!of.is_empty()).then(|| of[rng.below(of.len() as u64) as usize])
}

#[test]
fn a_freed_id_waits_for_the_pins_below_the_version_that_freed_it() {
    let ids = with_live(3);
    let pins = Registry::default();
    pins.publish(1);
    let before = pins.pin();
    ids.free(page(2), version(2)).expect("it is live");

    assert_eq!(reclaim(&ids, &pins), [], "the commit is not published");
    pins.publish(2);
    let at = pins.pin();
    assert_eq!(reclaim(&ids, &pins), [], "a snapshot of version 1 is open");
    assert_eq!(ids.state(page(2)), PageState::Retired);
    assert_eq!(ids.allocate(), Ok(page(4)));

    pins.unpin(before);
    assert_eq!(
        reclaim(&ids, &pins),
        pages([2]),
        "a snapshot of version 2 cannot reach it"
    );
    assert_eq!(ids.state(page(2)), PageState::Free);
    assert_eq!(ids.allocate(), Ok(page(2)));
    pins.unpin(at);
}

#[test]
fn a_freed_id_is_not_used_again_before_its_commit_is_published() {
    let ids = with_live(1);
    let pins = Registry::default();
    pins.publish(1);
    ids.free(page(1), version(2)).expect("it is live");
    for _ in 0..3 {
        assert_eq!(reclaim(&ids, &pins), []);
    }
    assert_eq!(retired(&ids), [(page(1), version(2))]);
    pins.publish(2);
    assert_eq!(reclaim(&ids, &pins), pages([1]));
    assert_eq!(retired(&ids), []);
}

#[test]
fn a_freed_id_waits_for_the_operations_that_were_running() {
    let ids = with_live(2);
    let pins = Registry::default();
    pins.publish(1);
    let reading = pins.guard();
    ids.free(page(1), version(2)).expect("it is live");
    pins.publish(2);

    assert_eq!(
        reclaim(&ids, &pins),
        [],
        "an operation that read version 1 runs"
    );
    assert_eq!(ids.state(page(1)), PageState::Retired);
    assert_eq!(retired(&ids), [(page(1), version(2))]);
    let later = pins.guard();
    assert_eq!(reclaim(&ids, &pins), []);

    pins.unguard(reading);
    assert_eq!(
        reclaim(&ids, &pins),
        pages([1]),
        "an operation that began after the id was found unreachable does not hold it"
    );
    pins.unguard(later);
}

#[test]
fn an_operation_that_begins_before_the_pins_are_gone_holds_the_id() {
    let ids = with_live(1);
    let pins = Registry::default();
    pins.publish(1);
    let snapshot = pins.pin();
    ids.free(page(1), version(2)).expect("it is live");
    pins.publish(2);
    assert_eq!(reclaim(&ids, &pins), []);

    let through_snapshot = pins.guard();
    pins.unpin(snapshot);
    assert_eq!(reclaim(&ids, &pins), []);
    assert_eq!(reclaim(&ids, &pins), []);
    pins.unguard(through_snapshot);
    assert_eq!(reclaim(&ids, &pins), pages([1]));
}

#[test]
fn ids_freed_at_several_versions_come_back_as_the_oldest_pin_moves() {
    let ids = with_live(6);
    let pins = Registry::default();
    pins.publish(1);
    let mut held = BTreeMap::new();
    for commit in 2..=6 {
        held.insert(commit - 1, pins.pin());
        ids.free(page(commit as u32), version(commit))
            .expect("it is live");
        pins.publish(commit);
    }
    assert_eq!(reclaim(&ids, &pins), []);
    assert_eq!(ids.census().retired, 5);

    pins.unpin(held[&3]);
    assert_eq!(
        reclaim(&ids, &pins),
        [],
        "the pin on version 1 is the oldest"
    );
    pins.unpin(held[&1]);
    assert_eq!(reclaim(&ids, &pins), pages([2]));
    pins.unpin(held[&2]);
    assert_eq!(reclaim(&ids, &pins), pages([3, 4]));
    assert_eq!(
        retired(&ids),
        [(page(5), version(5)), (page(6), version(6))]
    );
    pins.unpin(held[&5]);
    assert_eq!(reclaim(&ids, &pins), []);
    pins.unpin(held[&4]);
    assert_eq!(reclaim(&ids, &pins), pages([5, 6]));
    assert_eq!(ids.census().retired, 0);
    assert_eq!(ids.census().free_below_high_water(), 5);
}

#[test]
fn a_page_of_a_dropped_subtree_is_freed_with_an_earlier_version() {
    let ids = with_live(4);
    let pins = Registry::default();
    pins.publish(1);
    let old = pins.pin();
    pins.publish(5);
    let new = pins.pin();
    ids.free(page(1), version(6)).expect("it is live");
    pins.publish(6);

    ids.free(page(2), version(3)).expect("it is live");
    ids.free(page(3), version(5)).expect("it is live");
    ids.free(page(4), version(6)).expect("it is live");
    assert_eq!(reclaim(&ids, &pins), []);
    pins.unpin(old);
    assert_eq!(reclaim(&ids, &pins), pages([2, 3]));
    pins.unpin(new);
    assert_eq!(reclaim(&ids, &pins), pages([1, 4]));
}

/// Answers that were taken earlier, as a reclaim that is slow would have
/// them.
struct Answers {
    oldest_pin: u64,
    epoch: u64,
    oldest_guard: u64,
}

impl Pins for Answers {
    fn oldest_pin(&self) -> CommitVersion {
        version(self.oldest_pin)
    }

    fn next_epoch(&self) -> Epoch {
        Epoch::new(self.epoch)
    }

    fn oldest_guard(&self) -> Epoch {
        Epoch::new(self.oldest_guard)
    }
}

#[test]
fn two_reclaims_that_overtake_each_other_keep_each_id_for_its_own_guards() {
    let ids = with_live(3);
    for raw in 1..=3 {
        ids.free(page(raw), version(u64::from(raw) + 1))
            .expect("it is live");
    }
    let slow = Answers {
        oldest_pin: 2,
        epoch: 5,
        oldest_guard: 3,
    };
    let fast = Answers {
        oldest_pin: 3,
        epoch: 7,
        oldest_guard: 4,
    };
    assert_eq!(reclaim(&ids, &fast), []);
    assert_eq!(reclaim(&ids, &slow), []);
    let (waiting, draining) = ids.stages();
    assert_eq!(waiting, [(version(4), pages([3]))]);
    assert_eq!(
        draining,
        [
            (Epoch::new(7), version(2), pages([1])),
            (Epoch::new(7), version(3), pages([2]))
        ]
    );

    let ids = with_live(3);
    for raw in 1..=3 {
        ids.free(page(raw), version(u64::from(raw) + 1))
            .expect("it is live");
    }
    assert_eq!(reclaim(&ids, &slow), []);
    assert_eq!(
        reclaim(
            &ids,
            &Answers {
                oldest_pin: 3,
                epoch: 4,
                oldest_guard: 3
            }
        ),
        []
    );
    let (_, draining) = ids.stages();
    assert_eq!(
        draining,
        [
            (Epoch::new(4), version(3), pages([2])),
            (Epoch::new(5), version(2), pages([1]))
        ]
    );
    assert_eq!(retired(&ids).len(), 3);
    let later = |oldest_guard| Answers {
        oldest_pin: 3,
        epoch: 9,
        oldest_guard,
    };
    assert_eq!(reclaim(&ids, &later(4)), pages([2]));
    assert_eq!(reclaim(&ids, &later(4)), []);
    assert_eq!(reclaim(&ids, &later(5)), pages([1]));
    assert_eq!(reclaim(&ids, &later(9)), []);
    assert_eq!(ids.state(page(3)), PageState::Retired);
}

#[test]
fn a_reclaim_with_nothing_to_do_changes_nothing() {
    let ids = with_live(2);
    let pins = Registry::default();
    let before = ids.census();
    assert_eq!(reclaim(&ids, &pins), []);
    assert_eq!(ids.census(), before);
    assert_eq!(ids.stages(), (Vec::new(), Vec::new()));
}

#[test]
fn the_released_ids_are_added_to_what_the_caller_holds() {
    let ids = with_live(2);
    let pins = Registry::default();
    ids.free(page(2), version(2)).expect("it is live");
    pins.publish(2);
    let mut released = pages([7]);
    ids.reclaim(&pins, &mut released);
    assert_eq!(released, pages([7, 2]));
}

#[test]
fn the_ids_run_out_at_the_limit_and_come_back() {
    let ids = PageAllocator::with_limit(page(3));
    let pins = Registry::default();
    for raw in 1..=3 {
        assert_eq!(ids.allocate(), Ok(page(raw)));
    }
    assert_eq!(ids.allocate(), Err(AllocError::Exhausted));
    ids.publish(page(1)).expect("it is private");
    pins.publish(1);
    ids.free(page(1), version(2)).expect("it is live");
    assert_eq!(
        ids.allocate(),
        Err(AllocError::Exhausted),
        "a retired id is not free"
    );
    pins.publish(2);
    assert_eq!(reclaim(&ids, &pins), pages([1]));
    assert_eq!(ids.allocate(), Ok(page(1)));
    assert_eq!(ids.allocate(), Err(AllocError::Exhausted));
    ids.release(page(3)).expect("it is private");
    assert_eq!(ids.allocate(), Ok(page(3)));
}

#[test]
fn the_census_the_scan_and_the_retired_list_agree_with_the_states() {
    let ids = PageAllocator::new();
    let pins = Registry::default();
    let mut expected = BTreeMap::new();
    for raw in 1..=9000 {
        assert_eq!(ids.allocate(), Ok(page(raw)));
    }
    for raw in 1..=9000 {
        let state = match raw % 4 {
            0 => {
                ids.release(page(raw)).expect("it is private");
                continue;
            }
            1 => PageState::Private,
            2 => {
                ids.publish(page(raw)).expect("it is private");
                PageState::Live
            }
            _ => {
                ids.publish(page(raw)).expect("it is private");
                ids.free(page(raw), version(u64::from(raw % 7) + 2))
                    .expect("it is live");
                PageState::Retired
            }
        };
        expected.insert(page(raw), state);
    }
    pins.publish(4);
    let released = reclaim(&ids, &pins);
    assert!(!released.is_empty());
    for id in &released {
        assert!(expected.remove(id).is_some());
        assert_eq!(ids.state(*id), PageState::Free);
    }

    let found = in_use(&ids);
    assert_eq!(
        found,
        expected
            .iter()
            .map(|(id, state)| (*id, *state))
            .collect::<Vec<_>>()
    );
    let count = |state| expected.values().filter(|found| **found == state).count() as u64;
    let census = ids.census();
    assert_eq!(census.high_water, Some(page(9000)));
    assert_eq!(census.private, count(PageState::Private));
    assert_eq!(census.live, count(PageState::Live));
    assert_eq!(census.retired, count(PageState::Retired));
    assert_eq!(census.free_below_high_water(), 9000 - expected.len() as u64);
    let listed = retired(&ids);
    assert_eq!(listed.len() as u64, census.retired);
    for (id, freed_at) in listed {
        assert_eq!(expected[&id], PageState::Retired);
        assert_eq!(freed_at, version(u64::from(id.get() % 7) + 2));
        assert!(freed_at > version(4));
    }

    let mut some = Vec::new();
    assert_eq!(ids.scan(page(4000), &mut some), Some(page(4097)));
    assert_eq!(some.first().map(|(id, _)| id.get()), Some(4001));
    assert_eq!(some.last().map(|(id, _)| id.get()), Some(4094));
}

#[test]
fn the_census_of_an_empty_allocator_has_no_free_id_below_its_high_water() {
    let ids = PageAllocator::new();
    assert_eq!(ids.census().free_below_high_water(), 0);
    let first = ids.allocate().expect("an id");
    ids.release(first).expect("it is private");
    let census = ids.census();
    assert_eq!(census.high_water, Some(first));
    assert_eq!(census.free_below_high_water(), 1);
}

#[test]
fn errors_states_and_the_allocator_print() {
    let live = PageState::Live;
    let cases = [
        (AllocError::Exhausted, "no page id is left"),
        (
            AllocError::NotPrivate {
                page: page(7),
                state: live,
            },
            "page id 7 is live, not held by an open transaction",
        ),
        (
            AllocError::NotLive {
                page: page(8),
                state: PageState::Retired,
            },
            "page id 8 is retired, not a committed page",
        ),
        (
            AllocError::NotLive {
                page: page(8),
                state: PageState::Private,
            },
            "page id 8 is private, not a committed page",
        ),
        (
            AllocError::NotPrivate {
                page: page(9),
                state: PageState::Free,
            },
            "page id 9 is free, not held by an open transaction",
        ),
        (
            AllocError::FreedByNoCommit { page: page(3) },
            "page id 3 cannot be freed at version 0",
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
        let boxed: Box<dyn std::error::Error> = Box::new(error);
        assert!(boxed.source().is_none());
    }

    let ids = with_live(2);
    ids.allocate().expect("an id");
    ids.free(page(1), version(2)).expect("it is live");
    assert_eq!(
        format!("{ids:?}"),
        "PageAllocator { high_water: Some(page#3), private: 1, live: 1, retired: 1 }"
    );
    assert_eq!(Epoch::new(7).get(), 7);
    assert!(Epoch::new(7) < Epoch::new(8));
}

#[test]
fn a_fork_is_a_copy() {
    let ids = with_live(2);
    let fork = ids.fork();
    ids.free(page(1), version(2)).expect("it is live");
    assert_eq!(fork.state(page(1)), PageState::Live);
    assert_eq!(ids.state(page(1)), PageState::Retired);
}

#[test]
fn a_drawn_workload_agrees_with_a_model_of_the_rules() {
    for seed in 1..=6 {
        let mut rng = Rng::new(seed);
        let limit = [5, 40, 5000][seed as usize % 3];
        let ids = PageAllocator::with_limit(page(limit));
        let pins = Registry::default();
        let mut model: BTreeMap<u32, PageState> = BTreeMap::new();
        let mut freed_at: BTreeMap<u32, u64> = BTreeMap::new();
        let mut snapshots: Vec<u64> = Vec::new();
        let mut reused = 0;
        let mut ever_live = std::collections::BTreeSet::new();
        for step in 0..40_000 {
            match rng.below(10) {
                0..=2 => {
                    let lowest = (1..=limit).find(|raw| !model.contains_key(raw));
                    let got = ids.allocate();
                    assert_eq!(got.ok(), lowest.map(page), "step {step}");
                    if let Some(raw) = lowest {
                        reused += u64::from(ever_live.contains(&raw));
                        model.insert(raw, PageState::Private);
                    }
                }
                3 => {
                    if let Some(raw) = pick(&model, &mut rng, PageState::Private) {
                        ids.release(page(raw)).expect("it is private");
                        model.remove(&raw);
                    }
                }
                4 | 5 => {
                    let commit = pins.latest() + 1;
                    for _ in 0..rng.below(4) {
                        if let Some(raw) = pick(&model, &mut rng, PageState::Private) {
                            ids.publish(page(raw)).expect("it is private");
                            model.insert(raw, PageState::Live);
                            ever_live.insert(raw);
                        }
                    }
                    for _ in 0..rng.below(4) {
                        if let Some(raw) = pick(&model, &mut rng, PageState::Live) {
                            ids.free(page(raw), version(commit)).expect("it is live");
                            model.insert(raw, PageState::Retired);
                            freed_at.insert(raw, commit);
                        }
                    }
                    pins.publish(commit);
                }
                6 => snapshots.push(pins.pin()),
                7 => {
                    if !snapshots.is_empty() {
                        let at = rng.below(snapshots.len() as u64) as usize;
                        pins.unpin(snapshots.swap_remove(at));
                    }
                }
                _ => {
                    let oldest = snapshots.iter().copied().min().unwrap_or(pins.latest());
                    let due: Vec<u32> = freed_at
                        .iter()
                        .filter(|(_, at)| **at <= oldest)
                        .map(|(raw, _)| *raw)
                        .collect();
                    assert_eq!(
                        reclaim(&ids, &pins),
                        pages(due.iter().copied()),
                        "step {step}"
                    );
                    for raw in due {
                        freed_at.remove(&raw);
                        model.remove(&raw);
                    }
                }
            }
            if step % 64 == 0 {
                let expected: Vec<_> = model
                    .iter()
                    .map(|(raw, state)| (page(*raw), *state))
                    .collect();
                assert_eq!(in_use(&ids), expected, "step {step}");
                let waiting: Vec<_> = freed_at
                    .iter()
                    .map(|(raw, at)| (page(*raw), version(*at)))
                    .collect();
                assert_eq!(retired(&ids), waiting, "step {step}");
            }
        }
        assert!(reused > 100, "seed {seed}: {reused} ids were used again");
    }
}
