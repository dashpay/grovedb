//! The map from page id to state.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::alloc::states::States;

const ALL: [PageState; 4] = [
    PageState::Free,
    PageState::Private,
    PageState::Live,
    PageState::Retired,
];

fn scan_all(states: &States) -> Vec<(PageId, PageState)> {
    let mut found = Vec::new();
    let mut next = Some(PageId::MIN);
    while let Some(from) = next {
        next = states.scan(from, &mut found);
    }
    found
}

#[test]
fn every_id_is_free_until_it_is_given_a_state() {
    let mut states = States::new(PageId::MAX);
    for raw in [1, 2, 31, 32, 33, 4096, 4097, 1 << 20, u32::MAX] {
        assert_eq!(states.get(page(raw)), PageState::Free);
    }
    assert_eq!(states.lowest_free(), Some(page(1)));
    assert_eq!(states.high_water(), None);
    assert_eq!(scan_all(&states), []);
    for state in ALL {
        assert_eq!(states.count(state), 0);
    }
}

#[test]
fn a_state_is_read_back_and_its_neighbours_are_untouched() {
    for raw in [1, 2, 32, 33, 64, 4095, 4096, 4097, 8192, 8193, 1_000_000] {
        for new in ALL {
            let mut states = States::new(PageId::MAX);
            for near in [raw - 1, raw + 1] {
                if near != 0 {
                    states.set(page(near), PageState::Live);
                }
            }
            assert_eq!(states.set(page(raw), new), PageState::Free);
            assert_eq!(states.get(page(raw)), new);
            for near in [raw - 1, raw + 1] {
                if near != 0 {
                    assert_eq!(states.get(page(near)), PageState::Live);
                }
            }
            assert_eq!(states.set(page(raw), PageState::Retired), new);
            assert_eq!(states.get(page(raw)), PageState::Retired);
        }
    }
}

#[test]
fn the_lowest_free_id_is_found_across_words_and_chunks() {
    let mut states = States::new(PageId::MAX);
    for raw in 1..=10_000 {
        assert_eq!(states.lowest_free(), Some(page(raw)));
        states.set(page(raw), PageState::Live);
    }
    for raw in [9_000, 4_097, 4_096, 33, 32, 1] {
        states.set(page(raw), PageState::Free);
        assert_eq!(states.lowest_free(), Some(page(raw)));
    }
    for raw in [1, 32, 33, 4_096, 4_097, 9_000, 10_001] {
        assert_eq!(states.lowest_free(), Some(page(raw)));
        states.set(page(raw), PageState::Private);
    }
}

#[test]
fn a_chunk_that_was_skipped_is_free() {
    let mut states = States::new(PageId::MAX);
    states.set(page(3 * 4096 + 7), PageState::Live);
    assert_eq!(states.lowest_free(), Some(page(1)));
    for raw in 1..=4096 {
        states.set(page(raw), PageState::Live);
    }
    assert_eq!(states.lowest_free(), Some(page(4097)));
    assert_eq!(states.get(page(4097)), PageState::Free);
    assert_eq!(states.high_water(), Some(page(3 * 4096 + 7)));
}

#[test]
fn no_id_above_the_limit_is_found() {
    for limit in [1, 2, 31, 32, 33, 4095, 4096, 4097, 8192] {
        let mut states = States::new(page(limit));
        for raw in 1..=limit {
            assert_eq!(states.lowest_free(), Some(page(raw)), "limit {limit}");
            states.set(page(raw), PageState::Live);
        }
        assert_eq!(states.lowest_free(), None, "limit {limit}");
        assert_eq!(states.lowest_free(), None, "limit {limit}");
        states.set(page(limit), PageState::Free);
        assert_eq!(states.lowest_free(), Some(page(limit)));
    }
}

#[test]
fn the_last_id_is_the_last_one_found() {
    let mut states = States::new(PageId::MAX);
    let last_chunk = u32::MAX - 4094;
    for raw in last_chunk..u32::MAX {
        states.set(page(raw), PageState::Live);
    }
    for raw in 1..=4096 {
        states.set(page(raw), PageState::Live);
    }
    assert_eq!(states.lowest_free(), Some(page(4097)));

    let mut states = States::new(PageId::MAX);
    for raw in last_chunk..u32::MAX {
        states.set(page(raw), PageState::Live);
    }
    let mut found = Vec::new();
    assert_eq!(states.scan(page(last_chunk), &mut found), None);
    assert_eq!(found.len(), 4094);
    assert_eq!(states.get(PageId::MAX), PageState::Free);
    states.set(PageId::MAX, PageState::Retired);
    assert_eq!(states.get(PageId::MAX), PageState::Retired);
    assert_eq!(states.high_water(), Some(PageId::MAX));
    found.clear();
    assert_eq!(states.scan(PageId::MAX, &mut found), None);
    assert_eq!(found, [(PageId::MAX, PageState::Retired)]);
}

#[test]
fn a_scan_gives_the_ids_in_use_from_a_page_on() {
    let mut states = States::new(PageId::MAX);
    let set = [
        (1, PageState::Live),
        (2, PageState::Private),
        (4096, PageState::Retired),
        (4097, PageState::Live),
        (5 * 4096, PageState::Private),
    ];
    for (raw, state) in set {
        states.set(page(raw), state);
    }
    states.set(page(9000), PageState::Live);
    states.set(page(9000), PageState::Free);
    let expected: Vec<_> = set
        .iter()
        .map(|(raw, state)| (page(*raw), *state))
        .collect();
    assert_eq!(scan_all(&states), expected);

    let mut found = Vec::new();
    assert_eq!(states.scan(page(2), &mut found), Some(page(4097)));
    assert_eq!(found, expected[1..3]);
    found.clear();
    assert_eq!(
        states.scan(page(4098), &mut found),
        Some(page(2 * 4096 + 1))
    );
    assert_eq!(found, []);
    assert_eq!(
        states.scan(page(2 * 4096 + 1), &mut found),
        Some(page(3 * 4096 + 1))
    );
    assert_eq!(found, []);
    assert_eq!(
        states.scan(page(3 * 4096 + 1), &mut found),
        Some(page(5 * 4096 + 1))
    );
    assert_eq!(found, expected[4..]);
    assert_eq!(states.scan(page(5 * 4096 + 1), &mut found), None);
}

#[test]
fn a_drawn_workload_agrees_with_a_map() {
    for (seed, limit) in [(1, 9_000), (2, 64), (3, 5_000), (4, 33)] {
        let mut rng = Rng::new(seed);
        let mut states = States::new(page(limit));
        let mut model: BTreeMap<u32, PageState> = BTreeMap::new();
        let mut free: BTreeSet<u32> = (1..=limit).collect();
        for step in 0..60_000 {
            if rng.below(3) == 0 {
                let lowest = free.pop_first();
                assert_eq!(states.lowest_free(), lowest.map(page), "step {step}");
                if let Some(raw) = lowest {
                    states.set(page(raw), PageState::Private);
                    model.insert(raw, PageState::Private);
                }
                continue;
            }
            let raw = 1 + rng.below(u64::from(limit)) as u32;
            let new = ALL[rng.below(4) as usize];
            let old = model.get(&raw).copied().unwrap_or(PageState::Free);
            assert_eq!(states.set(page(raw), new), old, "step {step}");
            if new == PageState::Free {
                model.remove(&raw);
                free.insert(raw);
            } else {
                model.insert(raw, new);
                free.remove(&raw);
            }
            let probe = 1 + rng.below(u64::from(limit)) as u32;
            let expected = model.get(&probe).copied().unwrap_or(PageState::Free);
            assert_eq!(states.get(page(probe)), expected, "step {step}");
        }
        let expected: Vec<_> = model
            .iter()
            .map(|(raw, state)| (page(*raw), *state))
            .collect();
        assert_eq!(scan_all(&states), expected);
        for state in [PageState::Private, PageState::Live, PageState::Retired] {
            let count = model.values().filter(|found| **found == state).count();
            assert_eq!(states.count(state), count as u64);
        }
    }
}
