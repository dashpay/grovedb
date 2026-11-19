//! The keyspace against a `BTreeMap`, over random streams of operations.

use std::{collections::BTreeMap, ops::Bound};

use super::*;

pub(super) type Model = BTreeMap<Vec<u8>, Vec<u8>>;

/// Draws keys that meet: few different ones, long shared prefixes, every
/// length up to the longest a page stores, and keys longer than that.
pub(super) fn key(rng: &mut SimRng, config: FlatConfig) -> Vec<u8> {
    fn letters(rng: &mut SimRng, len: u64, of: u64) -> Vec<u8> {
        (0..len).map(|_| b'a' + rng.below(of) as u8).collect()
    }
    let limit = config.key_limit();
    match rng.below(40) {
        0..=13 => [b"k".to_vec(), letters(rng, 3, 12)].concat(),
        14..=19 => {
            let group = letters(rng, 1, 3);
            [vec![b'p'; 40], group, letters(rng, 2, 26)].concat()
        }
        // Neighbours that differ late, in groups that differ early: long
        // separators, which no prefix shortens where groups meet.
        20..=29 => {
            let group = vec![b'g', rng.below(40) as u8];
            [group, vec![b'x'; limit * 7 / 8], letters(rng, 2, 6)].concat()
        }
        30..=32 => {
            let len = rng.below(limit as u64 + 1);
            letters(rng, len, 2)
        }
        33 => vec![b'e'; limit - rng.below(2) as usize],
        // Long keys of a few buckets: few that meet often, and enough of
        // them for the tree of a bucket to have levels.
        34..=37 => {
            let head = vec![b'e' + rng.below(2) as u8; limit];
            let rest = match rng.below(2) {
                0 => {
                    let len = 1 + rng.below(3);
                    letters(rng, len, 3)
                }
                _ => letters(rng, 3, 9),
            };
            [head, rest].concat()
        }
        _ => {
            let len = limit as u64 + 1 + rng.below(4 * config.capacity() as u64);
            [vec![b'l'; len as usize], letters(rng, 1, 4)].concat()
        }
    }
}

pub(super) fn value(rng: &mut SimRng, config: FlatConfig) -> Vec<u8> {
    let len = match rng.below(10) {
        0..=4 => rng.below(40),
        5..=7 => rng.below(config.max_inline() as u64 + 1),
        _ => config.max_inline() as u64 + 1 + rng.below(2 * config.max_inline() as u64),
    };
    bytes(rng.next_u64(), len as usize)
}

/// A key of the model near a drawn one, or the drawn one.
pub(super) fn near(rng: &mut SimRng, model: &Model, config: FlatConfig) -> Vec<u8> {
    let drawn = key(rng, config);
    match rng.below(3) {
        0 => drawn,
        _ => model
            .range(drawn.clone()..)
            .next()
            .map_or(drawn, |(key, _)| key.clone()),
    }
}

fn check(pages: &Mem, tree: &FlatTree, model: &Model) {
    let found = records(pages, tree);
    let expected: Vec<_> = model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    assert_eq!(found.len(), expected.len());
    assert!(found == expected, "the keyspace and the model differ");
}

/// Walks a cursor from a drawn key in both directions, beside the model.
fn walk(rng: &mut SimRng, pages: &Mem, tree: &FlatTree, model: &Model, config: FlatConfig) {
    let from = near(rng, model, config);
    let mut cursor = tree.cursor();
    let read = |cursor: &Cursor| {
        let mut value = Vec::new();
        cursor.value(pages, &mut value).expect("a value");
        cursor.key().map(|key| (key.to_vec(), value))
    };
    let pair = |entry: Option<(&Vec<u8>, &Vec<u8>)>| entry.map(|(k, v)| (k.clone(), v.clone()));
    let steps = rng.below(40);

    cursor.seek(pages, &from).expect("a seek");
    let mut expected = model.range(from.clone()..);
    for _ in 0..steps {
        assert_eq!(read(&cursor), pair(expected.next()));
        cursor.next(pages).expect("a step");
    }

    cursor.seek_for_prev(pages, &from).expect("a seek");
    let mut expected = model.range(..=from.clone()).rev();
    for _ in 0..steps {
        assert_eq!(read(&cursor), pair(expected.next()));
        cursor.prev(pages).expect("a step");
    }

    // A turn in the middle: forward, then back over the same records.
    cursor.seek(pages, &from).expect("a seek");
    cursor.next(pages).expect("a step");
    cursor.prev(pages).expect("a step");
    let second = model.range(from.clone()..).nth(1).is_some();
    let first = pair(model.range(from..).next()).filter(|_| second);
    assert_eq!(read(&cursor), first);
}

/// What a run reached, so that a test can say it was not a small one.
#[derive(Debug, Default)]
struct Reached {
    records: usize,
    pages: usize,
    level: u8,
    carried_pending: usize,
    carried_placed: usize,
    released: usize,
    freed: usize,
    logged: usize,
    /// The highest level of the tree of a bucket of long keys.
    bucket_level: u8,
}

/// The highest level that the tree of a bucket has.
fn bucket_level(pages: &Mem) -> u8 {
    let limit = pages.config.key_limit();
    let roots = pages
        .pages
        .keys()
        .filter_map(|&page| pages.image(page).leaf());
    roots
        .flat_map(|leaf| leaf.iter())
        .filter(|(key, _)| key.len() > limit)
        .map(|(_, value)| match value {
            FlatValue::Inline(root) => PageId::decode(root.try_into().expect("four bytes")),
            FlatValue::OutOfLine(_) => None,
        })
        .map(|root| pages.image(root.expect("a page")).level())
        .max()
        .unwrap_or(0)
}

fn run(seed: u64, config: FlatConfig, steps: u64) -> Reached {
    let mut rng = SimRng::new(seed);
    let (mut pages, tree) = Mem::tree(config);
    let mut model = Model::new();
    let mut reached = Reached::default();
    for step in 0..steps {
        // The keyspace grows, is churned, and is taken down again.
        let (puts, deletes, wide) = match step * 4 / steps {
            0 | 1 => (72, 8, 0),
            2 => (45, 25, 1),
            _ => (20, 45, 4),
        };
        match rng.below(100) {
            op if op < puts => {
                let key = match rng.below(4) {
                    0 => near(&mut rng, &model, config),
                    _ => key(&mut rng, config),
                };
                let value = value(&mut rng, config);
                tree.put(&mut pages, &key, &value).expect("a put");
                model.insert(key, value);
            }
            op if op < puts + deletes => {
                let key = near(&mut rng, &model, config);
                let was = tree.delete(&mut pages, &key).expect("a delete");
                assert_eq!(was, model.remove(&key).is_some());
            }
            op if op < puts + deletes + 3 => {
                let (a, b) = (
                    near(&mut rng, &model, config),
                    near(&mut rng, &model, config),
                );
                let (start, end) = (a.clone().min(b.clone()), a.max(b));
                // A short range most of the time, so that the keyspace lives.
                let end = match rng.below(40) {
                    0 if wide > 1 => None,
                    reach if reach < wide => Some(end),
                    _ => model
                        .range(start.clone()..)
                        .nth(rng.below(6) as usize)
                        .map(|(key, _)| key.clone())
                        .or(Some(end)),
                };
                tree.delete_range(&mut pages, &start, end.as_deref())
                    .expect("a range delete");
                let upper = end.map_or(Bound::Unbounded, Bound::Excluded);
                let gone: Vec<_> = model
                    .range((Bound::Included(start), upper))
                    .map(|(key, _)| key.clone())
                    .collect();
                gone.iter().for_each(|key| drop(model.remove(key)));
            }
            op if op < puts + deletes + 9 => {
                let key = near(&mut rng, &model, config);
                assert_eq!(get(&pages, &tree, &key).as_ref(), model.get(&key));
            }
            op if op < puts + deletes + 12 => walk(&mut rng, &pages, &tree, &model, config),
            op if op < puts + deletes + 14 => {
                reached.logged += pages.commit().1;
                pages.place_values();
            }
            _ => reached.logged += pages.commit().1,
        }
        if step % 128 == 0 {
            check(&pages, &tree, &model);
            reached.records = reached.records.max(model.len());
            reached.pages = reached.pages.max(pages.pages.len());
            reached.level = reached.level.max(pages.image(tree.root()).level());
            reached.bucket_level = reached.bucket_level.max(bucket_level(&pages));
        }
    }
    check(&pages, &tree, &model);
    reached.logged += pages.commit().1;

    // Every record leaves, one key at a time, and one page is left.
    let mut keys: Vec<_> = model.keys().cloned().collect();
    while !keys.is_empty() {
        let key = keys.swap_remove(rng.below(keys.len() as u64) as usize);
        assert!(tree.delete(&mut pages, &key).expect("a delete"));
        model.remove(&key);
        if keys.len() % 97 == 0 {
            check(&pages, &tree, &model);
        }
    }
    pages.commit();
    assert_eq!(pages.pages.len(), 1, "an empty keyspace is its root");
    assert!(pages
        .image(tree.root())
        .leaf()
        .is_some_and(|leaf| leaf.is_empty()));
    assert!(pages.live.is_empty() && pages.pending.is_empty());

    reached.carried_placed = pages
        .carried
        .iter()
        .filter(|c| !c.place.is_pending())
        .count();
    reached.carried_pending = pages.carried.len() - reached.carried_placed;
    reached.released = pages.released.len();
    reached.freed = pages.freed.len();
    println!("seed {seed:#x}, {steps} steps: {reached:?}");
    reached
}

#[test]
fn random_operations_agree_with_a_btree_map() {
    for seed in 0..6 {
        let reached = run(0x7e57_0000 + seed, small(), 12_000);
        assert!(reached.records > 1_500 && reached.pages > 120 && reached.level >= 2);
        assert!(reached.carried_pending > 20 && reached.carried_placed > 100);
        assert!(reached.released > 500 && reached.freed > 100);
        assert!(reached.bucket_level >= 1);
    }
}

#[test]
fn random_operations_agree_with_a_btree_map_on_every_page_size() {
    for (seed, size) in [8_192, 16_384, 32_768, 65_536].into_iter().enumerate() {
        let config = FlatConfig::new(PageSize::new(size).expect("a page size"));
        let reached = run(0x51_2e00 + seed as u64, config, 5_000);
        assert!(reached.level >= 1 && reached.carried_placed > 20 && reached.freed > 20);
    }
}

#[test]
fn random_operations_agree_with_a_btree_map_at_both_inline_limits() {
    let least = small()
        .with_max_inline(FlatConfig::MIN_INLINE)
        .expect("a limit");
    let reached = run(0x1_0001, least, 8_000);
    assert!(reached.level >= 2 && reached.carried_placed > 250);
    let most = small()
        .with_max_inline(small().capacity() / 4)
        .expect("a limit");
    let reached = run(0x1_0002, most, 8_000);
    assert!(reached.level >= 2 && reached.carried_placed > 100);
}

/// Range deletes of every reach, checked against the model each time.
#[test]
fn range_deletes_agree_with_a_btree_map() {
    for seed in 0..40 {
        let mut rng = SimRng::new(0xde1e_7e00 + seed);
        let config = small();
        let (mut pages, tree) = Mem::tree(config);
        let mut model = Model::new();
        for _ in 0..(200 + 40 * seed) {
            let (key, value) = (key(&mut rng, config), value(&mut rng, config));
            tree.put(&mut pages, &key, &value).expect("a put");
            model.insert(key, value);
        }
        pages.commit();
        pages.place_values();
        while !model.is_empty() {
            let (a, b) = (
                near(&mut rng, &model, config),
                near(&mut rng, &model, config),
            );
            let (start, end) = (a.clone().min(b.clone()), a.max(b));
            let end = (rng.below(5) != 0).then_some(end);
            let start = if rng.below(5) == 0 { Vec::new() } else { start };
            tree.delete_range(&mut pages, &start, end.as_deref())
                .expect("a range delete");
            let upper = end.map_or(Bound::Unbounded, Bound::Excluded);
            let gone: Vec<_> = model
                .range((Bound::Included(start), upper))
                .map(|(key, _)| key.clone())
                .collect();
            gone.iter().for_each(|key| drop(model.remove(key)));
            check(&pages, &tree, &model);
            pages.commit();
        }
        assert_eq!(pages.pages.len(), 1);
        assert!(pages.live.is_empty() && pages.pending.is_empty());
    }
}
