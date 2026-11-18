//! The chains in memory: the rules on changing one, what a reader at each
//! version is given, and a drawn workload against the model.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use super::{
    model::{ModelTable, Rng},
    *,
};

#[test]
fn a_page_without_a_link_has_nothing() {
    let table = PageTable::default();
    for raw in [1, 2, 4096, 4097, 1_000_000, u32::MAX] {
        assert_eq!(table.newest(page(raw)), None);
        assert_eq!(read_at(&table, raw, u64::MAX - 1), None);
        assert!(chain_of(&table, raw).is_empty());
        assert!(table.trim(page(raw), version(9), &mut Vec::new()));
    }
    assert_eq!((table.pages(), table.links()), (0, 0));
    assert_eq!(table.high_water(), None);
    assert_eq!(table.discard_after(version(0)), 0);
    assert!(all_links(&table).is_empty());
    assert_eq!(
        format!("{table:?}"),
        "PageTable { pages: 0, links: 0, high_water: None }"
    );
}

#[test]
fn a_reader_gets_the_base_image_and_the_deltas_up_to_its_version() {
    let table = PageTable::new();
    let (image, first, second) = (at(1, 20), at(1, 100), at(2, 20));
    table.push(page(7), base(5, image)).unwrap();
    table.push(page(7), delta(7, first)).unwrap();
    table.push(page(7), delta(9, second)).unwrap();

    assert_eq!(read_at(&table, 7, 4), None);
    assert_eq!(read_at(&table, 7, 5), Some((version(5), vec![image])));
    assert_eq!(read_at(&table, 7, 6), Some((version(5), vec![image])));
    assert_eq!(
        read_at(&table, 7, 7),
        Some((version(7), vec![image, first]))
    );
    assert_eq!(
        read_at(&table, 7, 8),
        Some((version(7), vec![image, first]))
    );
    let all = Some((version(9), vec![image, first, second]));
    assert_eq!(read_at(&table, 7, 9), all);
    assert_eq!(read_at(&table, 7, u64::MAX - 1), all);

    assert_eq!(table.newest(page(7)), Some(delta(9, second)));
    assert_eq!(
        chain_of(&table, 7),
        [base(5, image), delta(7, first), delta(9, second)]
    );
    assert_eq!((table.pages(), table.links()), (1, 3));
    assert_eq!(table.high_water(), Some(page(7)));
    assert_eq!(read_at(&table, 6, 9), None);
    assert_eq!(read_at(&table, 8, 9), None);
}

#[test]
fn a_newer_base_image_serves_the_readers_from_its_version_on() {
    let table = PageTable::new();
    let (old, change, new, later) = (at(1, 20), at(1, 100), at(2, 20), at(2, 100));
    table.push(page(3), base(5, old)).unwrap();
    table.push(page(3), delta(7, change)).unwrap();
    table.push(page(3), base(9, new)).unwrap();
    table.push(page(3), delta(9, later)).unwrap();

    assert_eq!(read_at(&table, 3, 8), Some((version(7), vec![old, change])));
    assert_eq!(read_at(&table, 3, 9), Some((version(9), vec![new, later])));
}

#[test]
fn links_are_refused_by_the_rule_they_break() {
    let table = PageTable::new();
    let block = at(1, 20);
    assert_eq!(
        table.push(page(1), delta(1, block)),
        Err(LinkError::DeltaWithoutBase)
    );
    table.push(page(1), base(5, block)).unwrap();
    for refused in [base(4, block), delta(4, block), free(4)] {
        assert_eq!(
            table.push(page(1), refused),
            Err(LinkError::OutOfOrder {
                newest: version(5),
                version: version(4),
            })
        );
    }
    table.push(page(1), free(5)).unwrap();
    assert_eq!(
        table.push(page(1), delta(6, block)),
        Err(LinkError::DeltaWithoutBase)
    );
    assert_eq!(table.push(page(1), free(6)), Err(LinkError::AlreadyFree));
    assert_eq!(chain_of(&table, 1), [base(5, block), free(5)]);

    let bad_locations = [
        BlockLocation { len: 7, ..block },
        BlockLocation { len: 0, ..block },
        BlockLocation {
            len: (1 << 27) + 5,
            ..block
        },
        BlockLocation {
            offset: 19,
            ..block
        },
        BlockLocation {
            offset: u64::MAX - 63,
            ..block
        },
    ];
    for location in bad_locations {
        let invalid = Err(LinkError::InvalidLocation { location });
        assert_eq!(table.push(page(2), base(1, location)), invalid);
        assert_eq!(table.push(page(1), base(6, location)), invalid);
        assert_eq!(table.push(page(1), delta(1, location)), invalid);
        assert_eq!(
            table.relocate(page(1), block, location),
            Err(LinkError::InvalidLocation { location })
        );
        assert_eq!(
            table.consolidate(page(1), block, location),
            Err(LinkError::InvalidLocation { location })
        );
    }
    let largest = BlockLocation {
        len: (1 << 27) + 4,
        offset: u64::MAX - (1 << 27) - 4,
        ..block
    };
    table.push(page(2), base(1, largest)).unwrap();
    assert_eq!(read_at(&table, 2, 1), Some((version(1), vec![largest])));
    assert_eq!((table.pages(), table.links()), (2, 3));
}

#[test]
fn a_freed_page_has_no_version_until_a_new_base_image() {
    let table = PageTable::new();
    let (first, second) = (at(1, 20), at(1, 100));
    table.push(page(4), base(5, first)).unwrap();
    table.push(page(4), free(8)).unwrap();
    assert_eq!(read_at(&table, 4, 7), Some((version(5), vec![first])));
    assert_eq!(read_at(&table, 4, 8), None);
    assert_eq!(read_at(&table, 4, 20), None);
    assert_eq!(table.newest(page(4)), Some(free(8)));

    table.push(page(4), base(12, second)).unwrap();
    assert_eq!(read_at(&table, 4, 7), Some((version(5), vec![first])));
    assert_eq!(read_at(&table, 4, 11), None);
    assert_eq!(read_at(&table, 4, 12), Some((version(12), vec![second])));

    table.push(page(9), free(3)).unwrap();
    assert_eq!(read_at(&table, 9, 3), None);
    assert_eq!(table.pages(), 2);
    assert_eq!(
        table.push(page(9), base(2, first)),
        Err(LinkError::OutOfOrder {
            newest: version(3),
            version: version(2),
        })
    );
}

#[test]
fn a_consolidated_image_goes_right_after_the_link_it_was_made_through() {
    let table = PageTable::new();
    let (image, first, second, third) = (at(1, 20), at(1, 100), at(1, 200), at(1, 300));
    let (whole, part) = (at(2, 20), at(2, 100));
    table.push(page(1), base(5, image)).unwrap();
    table.push(page(1), delta(7, first)).unwrap();
    table.push(page(1), delta(9, second)).unwrap();

    assert_eq!(table.consolidate(page(1), second, whole), Ok(version(9)));
    assert_eq!(read_at(&table, 1, 9), Some((version(9), vec![whole])));
    assert_eq!(
        read_at(&table, 1, 8),
        Some((version(7), vec![image, first]))
    );

    table.push(page(1), delta(9, third)).unwrap();
    assert_eq!(table.consolidate(page(1), first, part), Ok(version(7)));
    assert_eq!(
        chain_of(&table, 1),
        [
            base(5, image),
            delta(7, first),
            base(7, part),
            delta(9, second),
            base(9, whole),
            delta(9, third),
        ]
    );
    assert_eq!(read_at(&table, 1, 7), Some((version(7), vec![part])));
    assert_eq!(
        read_at(&table, 1, 9),
        Some((version(9), vec![whole, third]))
    );
    assert_eq!(table.links(), 6);

    let gone = at(3, 20);
    assert_eq!(
        table.consolidate(page(1), gone, whole),
        Err(LinkError::NotInChain { location: gone })
    );
    assert_eq!(
        table.consolidate(page(2), image, whole),
        Err(LinkError::NotInChain { location: image })
    );
}

#[test]
fn a_consolidation_through_a_link_that_is_no_longer_the_newest_keeps_the_later_delta() {
    let table = PageTable::new();
    let (image, first, same_version, whole) = (at(1, 20), at(1, 100), at(1, 200), at(2, 20));
    table.push(page(1), base(5, image)).unwrap();
    table.push(page(1), delta(9, first)).unwrap();
    table.push(page(1), delta(9, same_version)).unwrap();
    table.consolidate(page(1), first, whole).unwrap();
    assert_eq!(
        read_at(&table, 1, 9),
        Some((version(9), vec![whole, same_version]))
    );
}

#[test]
fn a_block_is_relocated_only_while_a_link_points_to_it() {
    let table = PageTable::new();
    let (image, change, copy) = (at(1, 20), at(1, 100), at(5, 4000));
    table.push(page(1), base(5, image)).unwrap();
    table.push(page(1), delta(7, change)).unwrap();
    let longer = BlockLocation { len: 80, ..copy };

    table.relocate(page(1), image, longer).unwrap();
    assert_eq!(chain_of(&table, 1), [base(5, longer), delta(7, change)]);
    table.relocate(page(1), change, copy).unwrap();
    assert_eq!(chain_of(&table, 1), [base(5, longer), delta(7, copy)]);
    assert_eq!(
        table.relocate(page(1), image, copy),
        Err(LinkError::NotInChain { location: image })
    );
    assert_eq!(
        table.relocate(page(2), copy, image),
        Err(LinkError::NotInChain { location: copy })
    );

    let newer = at(6, 20);
    table.push(page(1), base(9, newer)).unwrap();
    let mut removed = Vec::new();
    assert!(!table.trim(page(1), version(9), &mut removed));
    assert_eq!(removed, [delta(7, copy), base(5, longer)]);
    assert_eq!(
        table.relocate(page(1), copy, image),
        Err(LinkError::NotInChain { location: copy })
    );
    table.push(page(1), free(10)).unwrap();
    assert_eq!(
        table.relocate(page(1), image, copy),
        Err(LinkError::NotInChain { location: image })
    );
}

#[test]
fn a_trim_keeps_what_a_reader_at_the_oldest_version_needs() {
    let build = || {
        let table = PageTable::new();
        table.push(page(1), base(5, at(1, 20))).unwrap();
        table.push(page(1), delta(7, at(1, 100))).unwrap();
        table.push(page(1), base(9, at(1, 200))).unwrap();
        table.push(page(1), delta(11, at(1, 300))).unwrap();
        table
    };
    for (oldest, kept_from) in [(0, 0), (4, 0), (5, 0), (8, 0), (9, 2), (10, 2), (99, 2)] {
        let table = build();
        let before = chain_of(&table, 1);
        let mut removed = vec![free(1)];
        assert!(!table.trim(page(1), version(oldest), &mut removed));
        assert_eq!(chain_of(&table, 1), before[kept_from..], "oldest {oldest}");
        let mut expected = before[..kept_from].to_vec();
        expected.push(free(1));
        expected.reverse();
        assert_eq!(removed, expected, "oldest {oldest}");
        assert_eq!(table.links(), (4 - kept_from) as u64);
        for at in oldest.max(5)..13 {
            assert_eq!(read_at(&table, 1, at), read_at(&build(), 1, at));
        }
    }
}

#[test]
fn a_trim_past_the_freeing_version_removes_the_mark_and_everything_before_it() {
    let (image, change, reused) = (at(1, 20), at(1, 100), at(2, 20));
    let table = PageTable::new();
    table.push(page(1), base(5, image)).unwrap();
    table.push(page(1), delta(6, change)).unwrap();
    table.push(page(1), free(8)).unwrap();

    let mut removed = Vec::new();
    assert!(!table.trim(page(1), version(7), &mut removed));
    assert!(removed.is_empty());
    assert_eq!(table.links(), 3);
    assert_eq!(table.trimmed_free(), None);
    assert!(table.trim(page(1), version(8), &mut removed));
    assert_eq!(removed, [delta(6, change), base(5, image)]);
    assert_eq!((table.pages(), table.links()), (0, 0));
    assert_eq!(table.newest(page(1)), None);
    assert_eq!(table.high_water(), Some(page(1)));
    assert_eq!(table.trimmed_free(), Some(version(8)));
    table.push(page(1), base(3, reused)).unwrap();

    let table = PageTable::new();
    table.push(page(1), base(5, image)).unwrap();
    table.push(page(1), free(8)).unwrap();
    table.push(page(1), base(12, reused)).unwrap();
    table.push(page(1), delta(13, change)).unwrap();
    removed.clear();
    assert!(!table.trim(page(1), version(10), &mut removed));
    assert_eq!(removed, [base(5, image)]);
    assert_eq!(chain_of(&table, 1), [base(12, reused), delta(13, change)]);
    assert_eq!(read_at(&table, 1, 10), None);
    assert_eq!((table.pages(), table.links()), (1, 2));
    assert_eq!(table.trimmed_free(), Some(version(8)));

    let table = PageTable::new();
    table.push(page(1), free(2)).unwrap();
    assert!(!table.trim(page(1), version(1), &mut removed));
    assert_eq!(table.trimmed_free(), None);
    assert!(table.trim(page(1), version(2), &mut removed));
    assert_eq!(removed, [base(5, image)]);
    assert_eq!(table.pages(), 0);
    assert_eq!(table.trimmed_free(), Some(version(2)));
}

#[test]
fn the_table_remembers_the_newest_free_link_a_trim_removed() {
    let table = PageTable::new();
    let mut removed = Vec::new();
    for (raw_page, freed) in [(1, 9), (2, 4), (3, 7)] {
        table
            .push(
                page(raw_page),
                base(1, at(1, 20 + 64 * u64::from(raw_page))),
            )
            .unwrap();
        table.push(page(raw_page), free(freed)).unwrap();
    }
    table.push(page(3), base(8, at(2, 20))).unwrap();
    assert_eq!(table.trimmed_free(), None);

    assert!(table.trim(page(2), version(20), &mut removed));
    assert_eq!(table.trimmed_free(), Some(version(4)));
    assert!(table.trim(page(1), version(20), &mut removed));
    assert_eq!(table.trimmed_free(), Some(version(9)));
    assert!(!table.trim(page(3), version(20), &mut removed));
    assert_eq!(chain_of(&table, 3), [base(8, at(2, 20))]);
    assert_eq!(table.trimmed_free(), Some(version(9)));

    let table = PageTable::new();
    table.push(page(1), free(3)).unwrap();
    table.push(page(1), base(5, at(1, 20))).unwrap();
    table.push(page(1), free(6)).unwrap();
    table.push(page(1), base(6, at(1, 84))).unwrap();
    assert!(!table.trim(page(1), version(5), &mut removed));
    assert_eq!(table.trimmed_free(), Some(version(3)));
    assert!(!table.trim(page(1), version(6), &mut removed));
    assert_eq!(chain_of(&table, 1), [base(6, at(1, 84))]);
    assert_eq!(table.trimmed_free(), Some(version(6)));

    assert_eq!(table.discard_after(version(0)), 1);
    assert_eq!(table.trimmed_free(), Some(version(6)));
}

#[test]
fn links_born_after_a_version_are_discarded() {
    let table = PageTable::new();
    table.push(page(1), base(5, at(1, 20))).unwrap();
    table.push(page(1), delta(7, at(1, 100))).unwrap();
    table.push(page(1), delta(9, at(1, 200))).unwrap();
    table.push(page(2), base(8, at(1, 300))).unwrap();
    table.push(page(3), base(2, at(1, 400))).unwrap();
    table.push(page(3), free(8)).unwrap();
    table.push(page(5000), base(9, at(1, 500))).unwrap();
    table.push(page(5000), free(9)).unwrap();

    assert_eq!(table.discard_after(version(9)), 0);
    assert_eq!(table.discard_after(version(7)), 5);
    assert_eq!(
        chain_of(&table, 1),
        [base(5, at(1, 20)), delta(7, at(1, 100))]
    );
    assert_eq!(table.newest(page(2)), None);
    assert_eq!(chain_of(&table, 3), [base(2, at(1, 400))]);
    assert_eq!(table.newest(page(5000)), None);
    assert_eq!((table.pages(), table.links()), (2, 3));
    assert_eq!(table.high_water(), Some(page(5000)));
    assert_eq!(table.discard_after(version(0)), 3);
    assert_eq!((table.pages(), table.links()), (0, 0));
}

#[test]
fn the_arena_takes_back_the_places_of_removed_links() {
    let table = PageTable::new();
    let empty = table.memory_usage();
    table.push(page(1), base(1, at(1, 20))).unwrap();
    let one_page = table.memory_usage();
    assert!(one_page > empty);

    let mut removed = Vec::new();
    let mut grown = 0;
    for round in 0..200u64 {
        for step in 1..=50 {
            let born = round * 100 + step;
            table.push(page(1), delta(born, at(2, 20 + born))).unwrap();
        }
        let born = round * 100 + 60;
        table.push(page(1), base(born, at(3, 20 + born))).unwrap();
        assert!(!table.trim(page(1), version(born), &mut removed));
        assert_eq!(removed.len(), 51);
        removed.clear();
        assert_eq!(table.links(), 1);
        if round == 0 {
            grown = table.memory_usage();
            assert!(grown > one_page);
        }
    }
    assert_eq!(table.memory_usage(), grown);
    assert_eq!(table.discard_after(version(0)), 1);
    assert_eq!(table.memory_usage(), grown);
}

#[test]
fn a_scan_gives_every_chain_in_runs_of_page_ids() {
    let table = PageTable::new();
    let pages = [1, 2, 4096, 4097, 20_000, u32::MAX - 1, u32::MAX];
    for raw in pages {
        let image = at(1, 20 + u64::from(raw));
        table.push(page(raw), base(1, image)).unwrap();
        table.push(page(raw), delta(2, image)).unwrap();
    }
    table.push(page(4096), free(3)).unwrap();
    let chain = |raw: u32| {
        let image = at(1, 20 + u64::from(raw));
        vec![(page(raw), base(1, image)), (page(raw), delta(2, image))]
    };

    let mut out = Vec::new();
    assert_eq!(table.scan(PageId::MIN, &mut out), Some(page(4097)));
    let mut first = [chain(1), chain(2), chain(4096)].concat();
    first.push((page(4096), free(3)));
    assert_eq!(out, first);
    assert_eq!(table.scan(page(2), &mut out), Some(page(4097)));
    assert_eq!(out[first.len()..], first[2..]);

    out.clear();
    assert_eq!(table.scan(page(4097), &mut out), Some(page(8193)));
    assert_eq!(out, chain(4097));
    out.clear();
    assert_eq!(table.scan(page(8193), &mut out), Some(page(20_481)));
    assert_eq!(out, chain(20_000));
    out.clear();
    assert_eq!(table.scan(page(20_481), &mut out), None);
    assert_eq!(out, [chain(u32::MAX - 1), chain(u32::MAX)].concat());
    out.clear();
    assert_eq!(table.scan(page(u32::MAX), &mut out), None);
    assert_eq!(out, chain(u32::MAX));

    assert_eq!(all_links(&table).len(), 15);
    assert_eq!(PageTable::new().scan(PageId::MIN, &mut out), None);
    assert_eq!(out.len(), 2);
}

/// The steps of the drawn workload use these pages: a few in one chunk of
/// the array, and some far apart.
const PAGES: [u32; 9] = [1, 2, 3, 4, 4096, 4097, 9000, 123_456, 50_000_000];

fn check_against(table: &PageTable, model: &ModelTable, newest: u64) {
    assert_eq!(all_links(table), model.links());
    assert_eq!(table.links(), model.links().len() as u64);
    assert_eq!(table.pages(), model.chains.len() as u64);
    assert_eq!(table.trimmed_free(), model.trimmed_free);
    for raw in PAGES {
        assert_eq!(chain_of(table, raw), model.chain(raw));
        assert_eq!(table.newest(page(raw)), model.chain(raw).last().copied());
        for raw_version in 0..=newest + 1 {
            assert_eq!(
                read_at(table, raw, raw_version),
                model.read(raw, version(raw_version)),
                "page {raw} at {raw_version}"
            );
        }
    }
}

#[test]
fn a_drawn_workload_leaves_what_the_model_has() {
    for seed in 1..=6 {
        let mut rng = Rng::new(seed);
        let table = PageTable::new();
        let mut model = ModelTable::default();
        let mut newest = 0u64;
        let mut blocks = 0u64;
        let mut taken = [0; 7];
        let mut refused = 0;
        for step in 0..6_000 {
            let raw = PAGES[rng.below(PAGES.len() as u64) as usize];
            let chain = model.chain(raw).to_vec();
            let known = (!chain.is_empty())
                .then(|| chain[rng.below(chain.len() as u64) as usize].location())
                .flatten();
            blocks += 1;
            let fresh = BlockLocation {
                file: id(1 + (blocks % 5) as u32),
                offset: 20 + blocks * 100,
                len: 8 + rng.below(4000) as u32,
            };
            let stale = at(77, 20 + rng.below(3));
            let born = if rng.below(12) == 0 {
                newest.saturating_sub(rng.below(3))
            } else {
                newest += rng.below(2);
                newest
            };
            let kind = rng.below(100);
            let ok = match kind {
                0..22 => {
                    taken[0] += 1;
                    let pushed = table.push(page(raw), base(born, fresh));
                    assert_eq!(pushed, model.push(raw, base(born, fresh)));
                    pushed.is_ok()
                }
                22..50 => {
                    taken[1] += 1;
                    let pushed = table.push(page(raw), delta(born, fresh));
                    assert_eq!(pushed, model.push(raw, delta(born, fresh)));
                    pushed.is_ok()
                }
                50..58 => {
                    taken[2] += 1;
                    let pushed = table.push(page(raw), free(born));
                    assert_eq!(pushed, model.push(raw, free(born)));
                    pushed.is_ok()
                }
                58..70 => {
                    taken[3] += 1;
                    let through = known.unwrap_or(stale);
                    let made = table.consolidate(page(raw), through, fresh);
                    assert_eq!(made, model.consolidate(raw, through, fresh));
                    made.is_ok()
                }
                70..80 => {
                    taken[4] += 1;
                    let from = known.unwrap_or(stale);
                    let moved = table.relocate(page(raw), from, fresh);
                    assert_eq!(moved, model.relocate(raw, from, fresh));
                    moved.is_ok()
                }
                80..98 => {
                    taken[5] += 1;
                    let oldest = version(newest.saturating_sub(rng.below(6)));
                    let mut removed = Vec::new();
                    let vacant = table.trim(page(raw), oldest, &mut removed);
                    removed.reverse();
                    assert_eq!((removed, vacant), model.trim(raw, oldest));
                    true
                }
                _ => {
                    taken[6] += 1;
                    let kept = version(newest.saturating_sub(rng.below(4)));
                    assert_eq!(table.discard_after(kept), model.discard_after(kept));
                    true
                }
            };
            refused += u32::from(!ok);
            if step % 97 == 0 {
                check_against(&table, &model, newest);
            }
        }
        check_against(&table, &model, newest);
        assert!(taken.iter().all(|count| *count > 50), "{taken:?}");
        assert!(refused > 300, "{refused}");
        assert_eq!(table.high_water(), Some(page(50_000_000)));
    }
}

#[test]
fn readers_see_whole_chains_while_a_writer_changes_them() {
    let table = PageTable::new();
    let location = |born: u64| BlockLocation {
        file: id(1),
        offset: 20 + born * 64,
        len: 64,
    };
    let pages = [1, 5000, 9999];
    for raw in pages {
        table.push(page(raw), base(0, location(0))).unwrap();
    }
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                let mut chain = Vec::new();
                let mut reads = 0u64;
                while !done.load(Ordering::Acquire) || reads < 1000 {
                    let raw = pages[(reads % 3) as usize];
                    let born = table
                        .read(page(raw), version(u64::MAX - 1), &mut chain)
                        .expect("the page has a version");
                    let first = (chain[0].offset - 20) / 64;
                    for (step, block) in chain.iter().enumerate() {
                        assert_eq!(*block, location(first + step as u64));
                    }
                    assert_eq!(born.get(), first + chain.len() as u64 - 1);
                    reads += 1;
                }
            });
        }
        let mut removed = Vec::new();
        let mut born = 0;
        while born < 3_000 {
            born += 1;
            for raw in pages {
                table.push(page(raw), delta(born, location(born))).unwrap();
            }
            if born % 7 == 0 {
                born += 1;
                for raw in pages {
                    let newest = location(born - 1);
                    table.relocate(page(raw), newest, newest).unwrap();
                    table.push(page(raw), base(born, location(born))).unwrap();
                    table.trim(page(raw), version(born), &mut removed);
                }
            }
        }
        done.store(true, Ordering::Release);
    });
}
