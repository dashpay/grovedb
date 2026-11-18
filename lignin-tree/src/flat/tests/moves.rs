//! The move rule of the value log (`value-log-format.md` O5): an entry
//! that a split, a merge or a change of the root takes to another page gets
//! its value again, as a new value of that page, in the same write.

use super::{
    model::{key, near, value, Model},
    *,
};

/// Where each entry of the tree is, and what it holds for a value out of
/// line.
fn homes(pages: &Mem) -> BTreeMap<Vec<u8>, (PageId, Option<ValuePlace>)> {
    let mut homes = BTreeMap::new();
    for &page in pages.pages.keys() {
        let Some(leaf) = pages.image(page).leaf() else {
            continue;
        };
        let places: HashMap<usize, ValuePlace> = pages.places(page).into_iter().collect();
        for (slot, (key, _)) in leaf.iter().enumerate() {
            homes.insert(key.to_vec(), (page, places.get(&slot).copied()));
        }
    }
    homes
}

/// What one write did to the values out of line, checked against where
/// the entries were before it and are after it.
///
/// - an entry that stays in its page and keeps its value holds the place
///   it held: nothing is appended for it, and nothing released
/// - an entry that goes to another page holds a new pending value there,
///   which was held for that page, and its old place was given up
/// - a pointer is released exactly when its entry was carried, was given
///   another value or left the tree
fn check_write(
    pages: &mut Mem,
    before: &BTreeMap<Vec<u8>, (PageId, Option<ValuePlace>)>,
) -> (usize, usize) {
    let after = homes(pages);
    let carried = std::mem::take(&mut pages.carried);
    let released: HashSet<ValuePointer> = std::mem::take(&mut pages.released).into_iter().collect();
    let mut expected = HashSet::new();
    let (mut moved, mut stayed) = (0, 0);
    for (key, &(was, place)) in before {
        let Some(place) = place else { continue };
        match after.get(key) {
            Some(&(page, Some(now))) if page == was && now == place => stayed += 1,
            Some(&(page, Some(now))) if page != was => {
                moved += 1;
                let ValuePlace::Pending(held) = now else {
                    panic!("an entry was carried to page {page} with a pointer");
                };
                // One write can carry an entry more than once: into a
                // merged page, and from there into the root.
                let mut at = (was, place);
                while at != (page, now) {
                    let next = carried.iter().find(|carry| (carry.from, carry.place) == at);
                    let next = next.unwrap_or_else(|| panic!("{at:?} was not carried on"));
                    at = (next.to, ValuePlace::Pending(next.held));
                }
                assert!(pages.pending.contains_key(&(page, held.id())));
                expected.extend(place.pointer());
            }
            // The write replaced the value, or removed the entry.
            _ => expected.extend(place.pointer()),
        }
    }
    assert_eq!(
        released, expected,
        "the pointers released are not those given up"
    );
    (moved, stayed)
}

#[test]
fn every_entry_that_changes_its_page_takes_its_value_along() {
    let config = small();
    let mut totals = (0, 0);
    for seed in 0..4 {
        let mut rng = SimRng::new(0x05_0000 + seed);
        let (mut pages, tree) = Mem::tree(config);
        let mut model = Model::new();
        for step in 0..4_000 {
            let before = homes(&pages);
            // The long keys of the model are left out: a bucket's entry
            // gets another value with every key put into it.
            let short = |rng: &mut SimRng, model: &Model| loop {
                let key = if rng.below(3) == 0 {
                    near(rng, model, config)
                } else {
                    key(rng, config)
                };
                if key.len() <= config.key_limit() {
                    return key;
                }
            };
            let key = short(&mut rng, &model);
            let grows = step < 2_500;
            match rng.below(10) {
                op if op < if grows { 8 } else { 3 } => {
                    // Mostly values out of line, in entries of every size.
                    let value = match rng.below(4) {
                        0 => value(&mut rng, config),
                        _ => bytes(
                            rng.next_u64(),
                            config.max_inline() + 1 + rng.below(50) as usize,
                        ),
                    };
                    tree.put(&mut pages, &key, &value).expect("a put");
                    model.insert(key, value);
                }
                9 => {
                    let end = model
                        .range(key.clone()..)
                        .nth(4)
                        .map(|(key, _)| key.clone());
                    tree.delete_range(&mut pages, &key, end.as_deref())
                        .expect("a range delete");
                    model.retain(|k, _| *k < key || end.as_ref().is_some_and(|end| k >= end));
                }
                _ => {
                    tree.delete(&mut pages, &key).expect("a delete");
                    model.remove(&key);
                }
            }
            let (moved, stayed) = check_write(&mut pages, &before);
            totals = (totals.0 + moved, totals.1 + stayed);
            if rng.below(4) == 0 {
                pages.commit();
                if rng.below(2) == 0 {
                    pages.place_values();
                }
            }
        }
        // Every pointer in the tree leads to a value its page owns.
        assert!(records(&pages, &tree) == model.into_iter().collect::<Vec<_>>());
    }
    assert!(
        totals.0 > 2_000,
        "only {} entries moved with a value",
        totals.0
    );
}

/// A split, a merge and the root taking its last child, one at a time.
#[test]
fn a_split_and_a_merge_give_the_moved_values_to_their_new_pages() {
    let config = small();
    let (mut pages, tree) = Mem::tree(config);
    let big = |n: u8| vec![n; config.max_inline() + 1];
    // Keys that share nothing, so that a page holds few of them.
    let key = |n: u8| [vec![n], vec![b'k'; 400]].concat();

    // One leaf, the root, with ten values in the value log.
    for n in 0..10 {
        tree.put(&mut pages, &key(n), &big(n)).expect("a put");
    }
    pages.commit();
    pages.place_values();
    let root = tree.root();
    let before = homes(&pages);
    assert!(before
        .values()
        .all(|&(page, place)| page == root && place.is_some()));
    let pointers: Vec<ValuePointer> = (0..10)
        .map(|n| {
            before[&key(n)]
                .1
                .and_then(|place| place.pointer())
                .expect("placed")
        })
        .collect();
    assert!(pointers
        .iter()
        .all(|pointer| pages.value_log[pointer].0 == root));

    // The root splits. Every entry leaves the root's page, so every value
    // is held again, for the leaf its entry is in now, and every old
    // pointer is released, in this one write.
    for n in 10..20 {
        tree.put(&mut pages, &key(n), b"small").expect("a put");
    }
    assert!(pages.image(root).branch().is_some());
    let after = homes(&pages);
    for n in 0..10 {
        let (page, place) = after[&key(n)];
        let held = place
            .and_then(|place| place.pending())
            .expect("pending again");
        assert_ne!(page, root);
        assert_eq!(pages.pending[&(page, held.id())], big(n));
        assert!(pages
            .carried
            .iter()
            .any(|carry| carry.to == page && carry.held == held));
    }
    let mut released = pages.released.clone();
    released.sort_by_key(|pointer| pointer.offset());
    assert_eq!(released, pointers);
    assert!(pages.live.is_empty());
    check_write(&mut pages, &before);

    // After the commit and write-back each value has a record of its own
    // again, whose owner is the page of its entry.
    pages.commit();
    pages.place_values();
    let placed = homes(&pages);
    for n in 0..10 {
        let (page, place) = placed[&key(n)];
        let pointer = place.and_then(|place| place.pointer()).expect("placed");
        assert!(!pointers.contains(&pointer));
        assert_eq!(pages.value_log[&pointer], (page, big(n)));
    }

    // A put into one leaf moves nothing: the entries beside it keep their
    // pointers, and nothing is appended or released for them.
    tree.put(&mut pages, &key(3), &big(33)).expect("a put");
    let (moved, stayed) = check_write(&mut pages, &placed);
    assert_eq!((moved, stayed), (0, 9));
    pages.commit();
    pages.place_values();

    // Deletes empty the leaves until two of them merge and the root takes
    // the last one: the values that are left move again, each time with
    // their entry.
    let before = homes(&pages);
    for n in (10..20).chain([0, 1, 2, 4, 5, 6]) {
        assert!(tree.delete(&mut pages, &key(n)).expect("a delete"));
    }
    assert!(pages.image(root).leaf().is_some());
    let (moved, _) = check_write(&mut pages, &before);
    assert_eq!(moved, 4);
    for n in [3, 7, 8, 9] {
        let expected = if n == 3 { big(33) } else { big(n) };
        assert_eq!(get(&pages, &tree, &key(n)), Some(expected));
    }
    pages.commit();
    pages.place_values();
    assert!(homes(&pages).values().all(|&(page, _)| page == root));
    assert_eq!(pages.live.len(), 4);
    assert!(pages
        .live
        .iter()
        .all(|pointer| pages.value_log[pointer].0 == root));
    assert!(records(&pages, &tree).len() == 4);
}

/// A value that was never placed moves as its bytes: no pointer is
/// released for it, because its entry held none.
#[test]
fn an_entry_carried_while_its_value_is_pending_releases_nothing() {
    let config = small();
    let (mut pages, tree) = Mem::tree(config);
    let big = vec![9; config.max_inline() + 1];
    for n in 0..30u8 {
        tree.put(&mut pages, &[vec![n], vec![b'k'; 400]].concat(), &big)
            .expect("a put");
    }
    assert!(pages.image(tree.root()).branch().is_some());
    assert!(!pages.carried.is_empty() && pages.carried.iter().all(|c| c.place.is_pending()));
    assert!(pages.released.is_empty() && pages.live.is_empty());
    assert_eq!(pages.pending.len(), 30);
    assert_eq!(records(&pages, &tree).len(), 30);
}
