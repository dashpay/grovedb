//! The write of many keys ([`FlatTree::write`]): what it leaves in the
//! keyspace, and how often it writes a leaf.

use super::{
    model::{key, near, value, Model},
    *,
};

/// The leaf whose key range holds `key`, found through the branches as the
/// page format reads them.
fn leaf_of(pages: &Mem, tree: &FlatTree, key: &[u8]) -> PageId {
    let mut page = tree.root();
    loop {
        match pages.image(page) {
            FlatPage::Leaf(_) => return page,
            FlatPage::Branch(branch) => page = branch.child_for(key).expect("a child"),
        }
    }
}

fn leaves(pages: &Mem) -> usize {
    (pages.pages.keys())
        .filter(|page| pages.image(**page).leaf().is_some())
        .count()
}

/// Makes `changes` and holds what the write says it wrote to what the
/// pages were handed: every image of a leaf, counted by the keyspace's own
/// count as well. Where no key of the changes is kept in a bucket:
/// **at most one image for each leaf that the write changed, and one more
/// for each further piece of a leaf that was cut**, as the keyspace's
/// own count of its cuts has them ([`leaf_pieces_cut`]), and **no
/// page handed two images of a leaf**, but the root where it is a leaf
/// afterwards and was none: it took what its only child held, after a
/// leaf beside that one was freed.
fn write(pages: &mut Mem, tree: &FlatTree, changes: &Changes) -> Written {
    let (before, counted) = (pages.leaf_writes, leaf_images_written());
    let from = pages.leaf_pages.len();
    let was_leaf = pages.image(tree.root()).leaf().is_some();
    let cut = leaf_pieces_cut();
    let written = tree.write(pages, changes).expect("a write of many keys");
    assert_eq!(written.leaf_writes, pages.leaf_writes - before);
    assert_eq!(written.leaf_writes, leaf_images_written() - counted);
    let limit = pages.config.key_limit();
    if changes.keys().all(|key| key.len() <= limit) {
        let made = leaf_pieces_cut() - cut;
        assert!(
            written.leaf_writes <= written.leaves + made,
            "{written:?}, with {made} further pieces"
        );
        let handed = &pages.leaf_pages[from..];
        let pages_handed: BTreeSet<PageId> = handed.iter().copied().collect();
        let collapsed = !was_leaf && pages.image(tree.root()).leaf().is_some();
        let twice = handed.len() - pages_handed.len();
        assert!(
            twice <= usize::from(collapsed),
            "a page was handed two images of a leaf: {handed:?}"
        );
        if twice == 1 {
            let root = handed.iter().filter(|page| **page == tree.root()).count();
            assert_eq!(root, 2, "the page that was handed two is the root");
        }
    }
    written
}

fn numbered(n: u32) -> Vec<u8> {
    format!("key-{n:06}").into_bytes()
}

/// A keyspace of `count` numbered keys with values of sixty bytes, put in
/// no order, so that its leaves are cut in the middle and have room.
fn spread(count: u32) -> (Mem, FlatTree) {
    let (mut pages, tree) = Mem::tree(small());
    let mut rng = SimRng::new(11);
    let mut keys: Vec<u32> = (0..count).collect();
    while !keys.is_empty() {
        let n = keys.swap_remove(rng.below(keys.len() as u64) as usize);
        tree.put(&mut pages, &numbered(n), &[n as u8; 60])
            .expect("a put");
    }
    (pages, tree)
}

#[test]
fn a_leaf_is_written_once_for_all_the_keys_it_takes() {
    let (mut pages, tree) = spread(2_000);
    let all = leaves(&pages);
    assert!(all > 20, "{all} leaves");

    // Every record gets another value of the length it had: no leaf grows
    // or shrinks, so none is cut and none is merged.
    let changes: Changes = (0..2_000)
        .map(|n| (numbered(n), Some(vec![!(n as u8); 60])))
        .collect();
    let takes = pages.takes;
    let written = write(&mut pages, &tree, &changes);
    assert_eq!(written.leaves, all as u64);
    assert_eq!(written.leaf_writes, all as u64);
    assert_eq!(pages.takes - takes, all as u64, "no branch is taken");
    assert_eq!(leaves(&pages), all);
    let found = records(&pages, &tree);
    assert_eq!(found.len(), 2_000);
    assert!(found
        .iter()
        .enumerate()
        .all(|(n, (key, value))| *key == numbered(n as u32) && *value == [!(n as u8); 60]));

    // The same records one put each write a leaf for each of them.
    let before = pages.leaf_writes;
    for n in 0..2_000 {
        tree.put(&mut pages, &numbered(n), &[n as u8; 60])
            .expect("a put");
    }
    assert_eq!(pages.leaf_writes - before, 2_000);

    // Three keys of two leaves: two leaves are written, and no other page
    // is taken.
    let (first, second) = (numbered(0), numbered(1));
    let last = numbered(1_999);
    assert_eq!(
        leaf_of(&pages, &tree, &first),
        leaf_of(&pages, &tree, &second)
    );
    assert_ne!(
        leaf_of(&pages, &tree, &first),
        leaf_of(&pages, &tree, &last)
    );
    let changes: Changes = [(first, Some(vec![1; 60])), (second, None), (last, None)].into();
    let takes = pages.takes;
    let written = write(&mut pages, &tree, &changes);
    assert_eq!((written.leaves, written.leaf_writes), (2, 2));
    assert_eq!(pages.takes - takes, 2);
    assert_eq!(records(&pages, &tree).len(), 1_998);
}

#[test]
fn a_leaf_that_outgrows_its_image_is_written_as_its_pieces() {
    let (mut pages, tree) = spread(2_000);
    let all = leaves(&pages);
    // New keys that all lie between two neighbours: one leaf takes them,
    // and is cut into the pieces they need.
    let home = leaf_of(&pages, &tree, &numbered(1_000));
    let new = |n: u32| [&numbered(1_000)[..], format!("+{n:03}").as_bytes()].concat();
    let end = (records(&pages, &tree).iter())
        .map(|(key, _)| key.clone())
        .find(|key| *key > numbered(1_000) && leaf_of(&pages, &tree, key) != home);
    let within = |key: &[u8]| end.as_deref().is_none_or(|end| key < end);
    assert!(within(&new(199)), "the new keys lie in one leaf");
    let changes: Changes = (0..200).map(|n| (new(n), Some(vec![7; 60]))).collect();
    let written = write(&mut pages, &tree, &changes);
    let pieces = leaves(&pages) - all;
    assert!(pieces >= 2, "{pieces} new leaves");
    assert_eq!(written.leaves, 1);
    assert_eq!(written.leaf_writes, 1 + pieces as u64);
    assert_eq!(records(&pages, &tree).len(), 2_200);

    // A leaf that is cut once: the new keys are few.
    let (mut pages, tree) = spread(2_000);
    let all = leaves(&pages);
    let mut count = 0;
    let written = loop {
        count += 1;
        let changes: Changes = [(new(count), Some(vec![7; 60]))].into();
        let written = write(&mut pages, &tree, &changes);
        if written.leaf_writes > 1 {
            break written;
        }
    };
    assert_eq!((written.leaves, written.leaf_writes), (1, 2));
    assert_eq!(leaves(&pages), all + 1);
    assert_eq!(records(&pages, &tree).len(), 2_000 + count as usize);
}

#[test]
fn deletes_of_keys_that_are_not_there_take_no_page() {
    let (mut pages, tree) = spread(600);
    let (revision, takes) = (pages.revision, pages.takes);
    let changes: Changes = (0..600)
        .map(|n| ([&numbered(n)[..], b"?"].concat(), None))
        .collect();
    let written = write(&mut pages, &tree, &changes);
    assert_eq!(written, Written::default());
    assert_eq!((pages.revision, pages.takes), (revision, takes));
    assert_eq!(tree.write(&mut pages, &Changes::new()).ok(), Some(written));
    assert_eq!((pages.revision, pages.takes), (revision, takes));

    // One of them is there: its leaf is written, and no other.
    let mut changes = changes;
    changes.insert(numbered(300), None);
    let written = write(&mut pages, &tree, &changes);
    assert_eq!((written.leaves, written.leaf_writes), (1, 1));
    assert_eq!(pages.takes - takes, 1);
    assert_eq!(records(&pages, &tree).len(), 599);
}

#[test]
fn a_leaf_that_loses_entries_is_looked_at_as_after_a_delete() {
    let (mut pages, tree) = spread(2_000);
    let keys_of = |pages: &Mem, leaf: PageId| -> Vec<Vec<u8>> {
        (records(pages, &tree).into_iter())
            .map(|(key, _)| key)
            .filter(|key| leaf_of(pages, &tree, key) == leaf)
            .collect()
    };
    // Every record of one leaf but one goes: the leaf is under a quarter
    // of a page, and is merged with the leaf to its right, which single
    // deletes have left under half a page and over a quarter.
    let home = leaf_of(&pages, &tree, &numbered(1_000));
    let own = keys_of(&pages, home);
    assert!(own.len() > 4);
    let after = (records(&pages, &tree).into_iter()).find(|(key, _)| key > &own[own.len() - 1]);
    let beside = leaf_of(&pages, &tree, &after.expect("a leaf to the right").0);
    let half = small().capacity() / 2;
    for key in keys_of(&pages, beside) {
        if pages.pages[&beside].len() < half {
            break;
        }
        assert!(tree.delete(&mut pages, &key).expect("a delete"));
    }
    assert!(pages.pages[&beside].len() > small().capacity() / 4);
    let (all, before) = (leaves(&pages), records(&pages, &tree).len());
    let changes: Changes = own[1..].iter().map(|key| (key.clone(), None)).collect();
    let written = write(&mut pages, &tree, &changes);
    assert_eq!(leaves(&pages), all - 1, "the two leaves are one");
    assert!(!pages.pages.contains_key(&beside));
    assert_eq!(written.leaves, 1);
    assert_eq!(written.leaf_writes, 1, "the two as one, and nothing before");
    assert_eq!(records(&pages, &tree).len(), before + 1 - own.len());

    // Every record of a leaf goes: the leaf leaves the tree, and is not
    // written.
    let home = leaf_of(&pages, &tree, &numbered(300));
    let own = keys_of(&pages, home);
    let (all, before) = (leaves(&pages), records(&pages, &tree).len());
    let changes: Changes = own.iter().map(|key| (key.clone(), None)).collect();
    let written = write(&mut pages, &tree, &changes);
    assert!(!pages.pages.contains_key(&home));
    assert_eq!(leaves(&pages), all - 1);
    assert_eq!((written.leaves, written.leaf_writes), (1, 0));
    assert_eq!(records(&pages, &tree).len(), before - own.len());

    // Every record goes, in one write: one page is left.
    let rest: Changes = (records(&pages, &tree).into_iter())
        .map(|(key, _)| (key, None))
        .collect();
    write(&mut pages, &tree, &rest);
    pages.commit();
    assert_eq!(pages.pages.len(), 1, "an empty keyspace is its root");
    assert!(pages
        .image(tree.root())
        .leaf()
        .is_some_and(|leaf| leaf.is_empty()));
}

#[test]
fn a_step_that_took_the_entries_out_before_the_leaf_is_taken_leaves_nothing_to_write() {
    let (mut pages, tree) = Mem::tree(small());
    for n in 0..8 {
        tree.put(&mut pages, &numbered(n), b"a value")
            .expect("a put");
    }
    pages.commit();
    // A page step takes two entries out of the leaf once the write has
    // read it and before it takes it: the write decides by what it took.
    let (mut after, same) = Mem::tree(small());
    for n in 2..8 {
        same.put(&mut after, &numbered(n), b"a value")
            .expect("a put");
    }
    assert_eq!(same.root(), tree.root());
    let image = after.pages[&tree.root()].clone();
    pages.step_before_take = Some((tree.root(), image));
    let changes: Changes = [(numbered(0), None), (numbered(1), None)].into();
    let leaf_writes = pages.leaf_writes;
    let written = tree.write(&mut pages, &changes).expect("a write");
    assert_eq!(written, Written::default());
    assert_eq!(pages.leaf_writes, leaf_writes);
    assert_eq!(records(&pages, &tree).len(), 6);
}

/// What a run of the model reached.
#[derive(Debug, Default)]
struct Reached {
    records: usize,
    level: u8,
    writes: u64,
    changes: u64,
    leaves: u64,
    leaf_writes: u64,
    counted: u64,
    long: u64,
    out_of_line: u64,
}

/// Random writes of many keys beside a `BTreeMap`, with single puts,
/// deletes, range deletes and commits between them. A write whose keys
/// are all stored as they are is held to the leaves its keys lay in.
fn run(seed: u64, config: FlatConfig, steps: u64) -> Reached {
    let mut rng = SimRng::new(seed);
    let (mut pages, tree) = Mem::tree(config);
    let mut model = Model::new();
    let mut reached = Reached::default();
    let short = |key: &[u8]| key.len() <= config.key_limit();
    for step in 0..steps {
        let (puts, size) = match step * 4 / steps {
            0 | 1 => (75, 1 + rng.below(60)),
            2 => (50, 1 + rng.below(30)),
            _ => (25, 1 + rng.below(90)),
        };
        match rng.below(100) {
            0..80 => {
                let mut changes = Changes::new();
                for _ in 0..size {
                    let key = match rng.below(3) {
                        0 => key(&mut rng, config),
                        _ => near(&mut rng, &model, config),
                    };
                    // Now and then the value that the key has.
                    let record = match (rng.below(100) < puts, rng.below(12)) {
                        (false, _) => None,
                        (true, 0) if model.contains_key(&key) => model.get(&key).cloned(),
                        (true, _) => Some(value(&mut rng, config)),
                    };
                    changes.insert(key, record);
                }
                // The leaves that the keys lie in which change one, as
                // the tree is before the write.
                // A value that the key has already, in its page, changes
                // nothing, as a delete of a key that is not there.
                let same = |key: &Vec<u8>, value: &Vec<u8>| {
                    value.len() <= config.max_inline() && model.get(key) == Some(value)
                };
                let changes_it = |key: &Vec<u8>, record: &Option<Vec<u8>>| match record {
                    Some(value) => !same(key, value),
                    None => model.contains_key(key),
                };
                let homes: BTreeSet<PageId> = (changes.iter())
                    .filter(|(key, record)| changes_it(key, record))
                    .map(|(key, _)| leaf_of(&pages, &tree, key))
                    .collect();
                let all_short = changes.keys().all(|key| short(key));
                let written = write(&mut pages, &tree, &changes);
                if all_short {
                    assert_eq!(written.leaves, homes.len() as u64);
                    reached.counted += 1;
                }
                reached.writes += 1;
                reached.changes += changes.len() as u64;
                reached.leaves += written.leaves;
                reached.leaf_writes += written.leaf_writes;
                reached.long += changes.keys().filter(|key| !short(key)).count() as u64;
                for (key, record) in changes {
                    let out_of_line = |value: &Vec<u8>| value.len() > config.max_inline();
                    reached.out_of_line += u64::from(record.as_ref().is_some_and(out_of_line));
                    match record {
                        Some(value) => drop(model.insert(key, value)),
                        None => drop(model.remove(&key)),
                    }
                }
            }
            80..86 => {
                let (key, value) = (key(&mut rng, config), value(&mut rng, config));
                tree.put(&mut pages, &key, &value).expect("a put");
                model.insert(key, value);
            }
            86..90 => {
                let key = near(&mut rng, &model, config);
                let was = tree.delete(&mut pages, &key).expect("a delete");
                assert_eq!(was, model.remove(&key).is_some());
            }
            90..92 => {
                let start = near(&mut rng, &model, config);
                let end = (model.range(start.clone()..).nth(rng.below(5) as usize))
                    .map(|(key, _)| key.clone());
                let Some(end) = end else { continue };
                tree.delete_range(&mut pages, &start, Some(&end))
                    .expect("a range delete");
                let gone: Vec<_> = model.range(start..end).map(|(k, _)| k.clone()).collect();
                gone.iter().for_each(|key| drop(model.remove(key)));
            }
            92..96 => {
                pages.commit();
                pages.place_values();
            }
            _ => drop(pages.commit()),
        }
        if step % 16 == 0 {
            let found = records(&pages, &tree);
            let expected: Vec<_> = model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            assert!(found == expected, "the keyspace and the model differ");
            reached.records = reached.records.max(model.len());
            reached.level = reached.level.max(pages.image(tree.root()).level());
        }
    }
    let found = records(&pages, &tree);
    let expected: Vec<_> = model.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    assert!(found == expected, "the keyspace and the model differ");
    pages.commit();

    // Every record leaves by writes of many keys, and one page is left.
    while !model.is_empty() {
        let from = near(&mut rng, &model, config);
        let mut gone: Vec<Vec<u8>> = (model.range(from..).take(1 + rng.below(70) as usize))
            .map(|(key, _)| key.clone())
            .collect();
        if gone.is_empty() {
            gone = model.keys().take(40).cloned().collect();
        }
        let changes: Changes = gone.iter().map(|key| (key.clone(), None)).collect();
        write(&mut pages, &tree, &changes);
        gone.iter().for_each(|key| drop(model.remove(key)));
        let found = records(&pages, &tree);
        assert_eq!(found.len(), model.len());
    }
    pages.commit();
    assert_eq!(pages.pages.len(), 1, "an empty keyspace is its root");
    assert!(pages.live.is_empty() && pages.pending.is_empty());
    println!("seed {seed:#x}, {steps} steps: {reached:?}");
    reached
}

#[test]
fn random_writes_of_many_keys_agree_with_a_btree_map() {
    let mut most = Reached::default();
    for seed in 1..=8 {
        let reached = run(seed, small(), 1_500);
        most.records = most.records.max(reached.records);
        most.level = most.level.max(reached.level);
        most.writes += reached.writes;
        most.changes += reached.changes;
        most.leaves += reached.leaves;
        most.leaf_writes += reached.leaf_writes;
        most.counted += reached.counted;
        most.long += reached.long;
        most.out_of_line += reached.out_of_line;
    }
    println!("{most:?}");
    assert!(most.records > 300 && most.level >= 2, "{most:?}");
    assert!(2 * most.changes > 3 * most.leaves, "{most:?}");
    assert!(most.leaf_writes != most.leaves, "{most:?}");
    assert!(
        most.counted > 100 && most.long > 500 && most.out_of_line > 500,
        "{most:?}"
    );
}

#[test]
fn random_writes_of_many_keys_agree_with_a_btree_map_on_every_page_size() {
    for (seed, size) in [8_192, 16_384, 32_768, 65_536].into_iter().enumerate() {
        let config = FlatConfig::new(PageSize::new(size).expect("a page size"));
        let reached = run(40 + seed as u64, config, 500);
        assert!(reached.writes > 300, "{reached:?}");
    }
}

/// The keys of a write of many keys arrive in order where their writers
/// make them so: a leaf that they outgrow is cut so that the pages behind
/// them stay full, as it is for the same keys one put each.
#[test]
fn keys_that_arrive_in_order_in_writes_of_many_leave_full_pages() {
    let config = small();
    let fill = |pages: &Mem| {
        let leaves: Vec<usize> = (pages.pages.values())
            .filter(|image| image[0] == 1)
            .map(Vec::len)
            .collect();
        leaves.iter().sum::<usize>() as f64 / (leaves.len() * config.capacity()) as f64
    };
    let (mut pages, tree) = Mem::tree(config);
    let (mut single, same) = Mem::tree(config);
    for batch in 0..800u64 {
        let mut changes = Changes::new();
        for n in batch * 5..batch * 5 + 5 {
            for subtree in [3u8, 5, 9] {
                let key = [&[subtree; 32][..], &n.to_be_bytes()].concat();
                same.put(&mut single, &key, &[subtree; 24]).expect("a put");
                changes.insert(key, Some(vec![subtree; 24]));
            }
        }
        write(&mut pages, &tree, &changes);
    }
    assert!(records(&pages, &tree) == records(&single, &same));
    assert_eq!(records(&pages, &tree).len(), 12_000);
    let (many, one) = (fill(&pages), fill(&single));
    println!(
        "leaves are {:.1}% full, and {:.1}% one put each",
        100.0 * many,
        100.0 * one
    );
    assert!(many > 0.88, "leaves are {:.0}% full", 100.0 * many);
}

/// A key that is kept in a bucket is written after the others: it does
/// not part the keys of a leaf that stand on either side of it.
#[test]
fn a_long_key_does_not_part_the_keys_of_a_leaf() {
    let (mut pages, tree) = Mem::tree(small());
    let long = vec![b'm'; small().key_limit() + 40];
    let changes: Changes = [
        (b"a".to_vec(), Some(b"first".to_vec())),
        (long.clone(), None),
        (b"z".to_vec(), Some(b"last".to_vec())),
    ]
    .into();
    let written = write(&mut pages, &tree, &changes);
    assert_eq!((written.leaves, written.leaf_writes), (1, 1));
    assert_eq!(pages.pages.len(), 1, "no bucket is made for a delete");

    // With a value, the long key makes its bucket: the leaf of the two
    // short keys is written for them, and again for the bucket.
    let changes: Changes = [
        (b"b".to_vec(), Some(b"second".to_vec())),
        (long.clone(), Some(b"under a long key".to_vec())),
        (b"y".to_vec(), Some(b"fourth".to_vec())),
    ]
    .into();
    let written = write(&mut pages, &tree, &changes);
    assert_eq!(written.leaves, 2);
    assert_eq!(
        written.leaf_writes, 4,
        "the leaf, the bucket's tree twice, the leaf"
    );
    assert_eq!(
        get(&pages, &tree, &long),
        Some(b"under a long key".to_vec())
    );
    assert_eq!(records(&pages, &tree).len(), 5);
}

/// A value that a key has already, in its page, changes nothing: its leaf
/// is taken, as a put of the key takes it, and is not written.
#[test]
fn a_value_that_a_key_has_already_writes_nothing() {
    let (mut pages, tree) = spread(600);
    let (revision, takes, all) = (pages.revision, pages.takes, leaves(&pages) as u64);
    let changes: Changes = (0..600)
        .map(|n| (numbered(n), Some(vec![n as u8; 60])))
        .collect();
    let written = write(&mut pages, &tree, &changes);
    assert_eq!(written, Written::default());
    assert_eq!(pages.revision, revision, "no page is written");
    assert_eq!(pages.takes - takes, all, "each leaf is taken once");

    // One of them gets another value, and one key that is not there is
    // deleted beside it: one leaf is written.
    let mut changes = changes;
    changes.insert(numbered(300), Some(vec![9; 60]));
    changes.insert([&numbered(300)[..], b"?"].concat(), None);
    let written = write(&mut pages, &tree, &changes);
    assert_eq!((written.leaves, written.leaf_writes), (1, 1));
    assert_eq!(get(&pages, &tree, &numbered(300)), Some(vec![9; 60]));

    // A value out of line is written again: it is not read to be
    // compared.
    let long = vec![7; small().max_inline() + 1];
    let one: Changes = [(numbered(1), Some(long.clone()))].into();
    assert_eq!(write(&mut pages, &tree, &one).leaf_writes, 1);
    assert_eq!(write(&mut pages, &tree, &one).leaf_writes, 1);
    assert_eq!(get(&pages, &tree, &numbered(1)), Some(long));
}

/// A key that gets the value it has is judged by the leaf as it is taken:
/// where a step of background work took its record out since the leaf was
/// read, the record is put, as a put of the key alone puts it.
#[test]
fn a_value_that_a_step_took_out_before_the_leaf_is_taken_is_put() {
    let (mut pages, tree) = Mem::tree(small());
    for n in 0..8 {
        tree.put(&mut pages, &numbered(n), b"a value")
            .expect("a put");
    }
    pages.commit();
    // The leaf as a step leaves it, without the first two records.
    let (mut after, same) = Mem::tree(small());
    for n in 2..8 {
        same.put(&mut after, &numbered(n), b"a value")
            .expect("a put");
    }
    let image = after.pages[&tree.root()].clone();
    pages.step_before_take = Some((tree.root(), image));
    let changes: Changes = [(numbered(0), Some(b"a value".to_vec()))].into();
    let takes = pages.takes;
    let written = tree.write(&mut pages, &changes).expect("a write");
    assert_eq!(pages.takes - takes, 1, "the leaf is taken");
    assert!(pages.step_before_take.is_none(), "the step was made");
    assert_eq!((written.leaves, written.leaf_writes), (1, 1));
    assert_eq!(get(&pages, &tree, &numbered(0)), Some(b"a value".to_vec()));
    assert_eq!(get(&pages, &tree, &numbered(1)), None);
    assert_eq!(records(&pages, &tree).len(), 7);
}

/// Leaves that are emptied and merged by one write, from one end of the
/// keyspace to the other, each handed at most one image.
#[test]
fn a_write_that_takes_most_records_hands_no_page_two_images() {
    for keep in [2, 3, 5, 9, 17, 40] {
        let (mut pages, tree) = spread(3_000);
        let before = leaves(&pages);
        let changes: Changes = (0..3_000)
            .filter(|n| n % keep != 0)
            .map(|n| (numbered(n), None))
            .collect();
        let homes: BTreeSet<PageId> = (changes.keys())
            .map(|key| leaf_of(&pages, &tree, key))
            .collect();
        let written = write(&mut pages, &tree, &changes);
        let after = leaves(&pages);
        let left: Vec<Vec<u8>> = (0..3_000).step_by(keep as usize).map(numbered).collect();
        let found: Vec<Vec<u8>> = (records(&pages, &tree).into_iter())
            .map(|(key, _)| key)
            .collect();
        assert_eq!(found, left);
        assert_eq!(written.leaves, homes.len() as u64);
        println!("one record in {keep} is kept: {before} leaves, then {after}, {written:?}");
        // No leaf is written that is then freed for being empty, and a
        // leaf that is merged into another is written once before, where
        // it was the right one of the two, or not at all.
        assert!(written.leaf_writes <= written.leaves);
        // With few of its records left, a leaf is under a quarter of a
        // page: leaves are merged, also where both took changes.
        assert!(after <= before);
        assert!(keep < 17 || 3 * after < before, "leaves are merged");
        pages.commit();
    }
}

/// Two leaves that are the root's only children are merged into the
/// root, in one image: the root would take what the merged page holds
/// next, which would be a second image of the same leaf.
#[test]
fn two_leaves_below_the_root_are_merged_into_the_root() {
    // Keys in no order, so that the root is cut in the middle.
    let (mut pages, tree) = Mem::tree(small());
    let mut n = 0;
    while pages.image(tree.root()).leaf().is_some() {
        tree.put(&mut pages, &numbered(n * 7_919 % 1_000), &[n as u8; 60])
            .expect("a put");
        n += 1;
    }
    assert_eq!((leaves(&pages), pages.image(tree.root()).level()), (2, 1));
    let keys_of = |pages: &Mem, leaf: PageId| -> Vec<Vec<u8>> {
        (records(pages, &tree).into_iter())
            .map(|(key, _)| key)
            .filter(|key| leaf_of(pages, &tree, key) == leaf)
            .collect()
    };
    // The leaf to the right loses all its records but one, and the one to
    // the left none: one leaf is changed, and one image is written, of
    // the root.
    let last = records(&pages, &tree).pop().expect("a record").0;
    let (left, right) = (
        leaf_of(&pages, &tree, &numbered(0)),
        leaf_of(&pages, &tree, &last),
    );
    let own = keys_of(&pages, right);
    assert!(left != right && own.len() > 4);
    // The leaf to the left is made short enough for the two to be one.
    let mut kept = records(&pages, &tree).len();
    for key in keys_of(&pages, left) {
        if pages.pages[&left].len() < small().capacity() / 2 {
            break;
        }
        assert!(tree.delete(&mut pages, &key).expect("a delete"));
        kept -= 1;
    }
    assert_eq!(leaves(&pages), 2);
    let from = pages.leaf_pages.len();
    let changes: Changes = own[1..].iter().map(|key| (key.clone(), None)).collect();
    let written = write(&mut pages, &tree, &changes);
    assert_eq!((written.leaves, written.leaf_writes), (1, 1));
    assert_eq!(pages.leaf_pages[from..], [tree.root()]);
    assert_eq!(records(&pages, &tree).len(), kept + 1 - own.len());
    pages.commit();
    assert_eq!(pages.pages.len(), 1, "the root holds every record");
}

/// A leaf that is left with no entry is freed and takes no image. Where
/// it was one of the root's two children, the root takes what the other
/// holds: one image, also where the write has changes of that one too.
#[test]
fn a_leaf_that_is_emptied_below_the_root_takes_no_image() {
    for and_the_left in [false, true] {
        let (mut pages, tree) = Mem::tree(small());
        let mut n = 0;
        while pages.image(tree.root()).leaf().is_some() {
            tree.put(&mut pages, &numbered(n), &[n as u8; 60])
                .expect("a put");
            n += 1;
        }
        let right = leaf_of(&pages, &tree, &numbered(n - 1));
        let all = records(&pages, &tree);
        let mut changes: Changes = (all.iter())
            .filter(|(key, _)| leaf_of(&pages, &tree, key) == right)
            .map(|(key, _)| (key.clone(), None))
            .collect();
        let gone = changes.len();
        if and_the_left {
            changes.insert(numbered(0), Some(vec![9; 60]));
        }
        let from = pages.leaf_pages.len();
        let written = write(&mut pages, &tree, &changes);
        let both = u64::from(and_the_left);
        // The emptied leaf takes none; the root takes the other leaf's
        // entries, and is written again for that leaf's own change.
        assert_eq!((written.leaves, written.leaf_writes), (1 + both, 1 + both));
        assert!(pages.leaf_pages[from..]
            .iter()
            .all(|page| *page == tree.root()));
        assert_eq!(records(&pages, &tree).len(), all.len() - gone);
        if and_the_left {
            assert_eq!(get(&pages, &tree, &numbered(0)), Some(vec![9; 60]));
        }
    }
}

/// Builds a leaf or a branch by hand, as the page format has it.
fn leaf_image(page: u32, keys: &[&[u8]]) -> Vec<u8> {
    use lignin_format::flat_page::FlatLeafBuilder;
    let mut image = FlatLeafBuilder::new(page_id(page), b"", small().capacity()).expect("a leaf");
    for key in keys {
        image
            .push(key, FlatValue::Inline(b"was"))
            .expect("an entry");
    }
    image.finish()
}

fn branch_image(page: u32, level: u8, first: u32, rest: &[(&[u8], u32)]) -> Vec<u8> {
    use lignin_format::flat_page::FlatBranchBuilder;
    let capacity = small().capacity();
    let mut image = FlatBranchBuilder::new(page_id(page), b"", capacity, level, page_id(first))
        .expect("a branch");
    for (separator, child) in rest {
        image.push(separator, page_id(*child)).expect("a separator");
    }
    image.finish()
}

fn by_hand(images: Vec<Vec<u8>>) -> (Mem, FlatTree) {
    let mut pages = Mem::new(small());
    for (at, image) in images.into_iter().enumerate() {
        pages.pages.insert(page_id(at as u32 + 1), image);
    }
    pages.next_page = pages.pages.len() as u32;
    pages.base = pages.pages.clone();
    (pages, FlatTree::open(page_id(1), small()))
}

/// Where a branch has a separator before the one that the branch above
/// it has, a key is taken for a leaf only where every branch sends it
/// there: each key of a write goes where a put of it alone goes.
#[test]
fn a_key_is_written_where_a_put_of_it_alone_goes_also_below_a_branch_that_is_damaged() {
    // The root sends the keys from `m` on to the branch 3, whose own
    // separator `a` is before `m`.
    let made = || {
        by_hand(vec![
            branch_image(1, 2, 2, &[(b"m", 3)]),
            branch_image(2, 1, 4, &[(b"f", 5)]),
            branch_image(3, 1, 6, &[(b"a", 7)]),
            leaf_image(4, &[b"c"]),
            leaf_image(5, &[b"g"]),
            leaf_image(6, &[b"0"]),
            leaf_image(7, &[b"x"]),
        ])
    };
    let (mut single, tree) = made();
    for key in [&b"b"[..], b"z"] {
        tree.put(&mut single, key, b"put").expect("a put");
    }
    let (mut pages, tree) = made();
    let changes: Changes = [
        (b"b".to_vec(), Some(b"put".to_vec())),
        (b"z".to_vec(), Some(b"put".to_vec())),
    ]
    .into();
    let written = tree.write(&mut pages, &changes).expect("a write");
    assert_eq!((written.leaves, written.leaf_writes), (2, 2));
    assert!(
        pages.pages == single.pages,
        "the pages of a put for each key"
    );
    for key in [&b"b"[..], b"z"] {
        assert_eq!(get(&pages, &tree, key), Some(b"put".to_vec()));
    }
}

/// A leaf is merged with the leaf to its left where the write has no key
/// left for that one, also where the two are the first children of a
/// branch that is not the first of its level: the key the write goes on
/// with lies below another branch.
#[test]
fn a_leaf_is_merged_with_a_left_neighbour_that_no_key_is_left_for() {
    // The branch 3 has two children, so the leaf to the left of its last
    // one is its first: no separator of the branch says where the key
    // range of that one begins, and the root says `m`.
    let made = || {
        by_hand(vec![
            branch_image(1, 2, 2, &[(b"m", 3)]),
            branch_image(2, 1, 4, &[(b"f", 5)]),
            branch_image(3, 1, 6, &[(b"t", 7)]),
            leaf_image(4, &[b"a", b"b"]),
            leaf_image(5, &[b"g", b"h"]),
            leaf_image(6, &[b"n", b"o"]),
            leaf_image(7, &[b"t", b"u"]),
        ])
    };
    let keys = |pages: &Mem, tree: &FlatTree| -> Vec<u8> {
        let keys = records(pages, tree).into_iter().map(|(key, _)| key);
        keys.collect::<Vec<_>>().concat()
    };
    // The last child of its branch loses an entry, and a key far to the
    // left, below another branch, is written: the leaf goes into the one
    // to its left.
    let (mut pages, tree) = made();
    let changes: Changes = [(b"u".to_vec(), None), (b"a".to_vec(), Some(b"is".to_vec()))].into();
    let written = write(&mut pages, &tree, &changes);
    assert_eq!((written.leaves, written.leaf_writes), (2, 2));
    assert!(!pages.pages.contains_key(&page_id(7)), "merged to the left");
    assert_eq!(keys(&pages, &tree), b"abghnot");

    // With a key left for the leaf to its left, it is not: it is written
    // by itself. The leaf to its left then loses its own entry and takes
    // it in, as its neighbour to the right that the write is done with:
    // each page is handed one image.
    let (mut pages, tree) = made();
    let from = pages.leaf_pages.len();
    let changes: Changes = [(b"u".to_vec(), None), (b"o".to_vec(), None)].into();
    let written = write(&mut pages, &tree, &changes);
    assert_eq!((written.leaves, written.leaf_writes), (2, 2));
    assert_eq!(pages.leaf_pages[from..], [page_id(7), page_id(6)]);
    assert_eq!(keys(&pages, &tree), b"abghnt");
}

/// A root that is a leaf and is cut is written as its pieces, each on a
/// new page, and no image more.
#[test]
fn a_root_that_is_cut_is_written_as_its_pieces() {
    let (mut pages, tree) = Mem::tree(small());
    let mut n = 0;
    let written = loop {
        let changes: Changes = (n..n + 20)
            .map(|n| (numbered(n), Some(vec![n as u8; 60])))
            .collect();
        n += 20;
        let written = write(&mut pages, &tree, &changes);
        if pages.image(tree.root()).leaf().is_none() {
            break written;
        }
        assert_eq!((written.leaves, written.leaf_writes), (1, 1));
    };
    assert_eq!(leaves(&pages), 2);
    assert_eq!((written.leaves, written.leaf_writes), (1, 2));
}

/// A tree whose root has one child, a leaf, which no write of this code
/// leaves: a write that takes an entry out of the leaf puts the tree
/// right in the one image it writes, of the root.
#[test]
fn a_root_with_one_child_takes_what_the_leaf_holds_in_one_image() {
    let (mut pages, tree) = by_hand(vec![
        branch_image(1, 1, 2, &[]),
        leaf_image(2, &[b"a", b"b", b"c"]),
    ]);
    let from = pages.leaf_pages.len();
    let changes: Changes = [(b"b".to_vec(), None)].into();
    let written = write(&mut pages, &tree, &changes);
    assert_eq!((written.leaves, written.leaf_writes), (1, 1));
    assert_eq!(pages.leaf_pages[from..], [tree.root()]);
    pages.commit();
    assert_eq!(pages.pages.len(), 1);
    let found = records(&pages, &tree).into_iter().map(|(key, _)| key);
    assert_eq!(found.collect::<Vec<_>>().concat(), b"ac");
}

/// A leaf that is the only child of a branch which is not the root has no
/// neighbour to be merged with: it is written by itself, once, and the
/// branch above it is then looked at, as after a delete.
#[test]
fn a_leaf_that_is_alone_below_a_branch_is_written_by_itself() {
    let (mut pages, tree) = by_hand(vec![
        branch_image(1, 2, 2, &[(b"m", 3)]),
        branch_image(2, 1, 4, &[]),
        branch_image(3, 1, 5, &[(b"t", 6)]),
        leaf_image(4, &[b"a", b"b", b"c"]),
        leaf_image(5, &[b"n", b"o"]),
        leaf_image(6, &[b"t", b"u"]),
    ]);
    let from = pages.leaf_pages.len();
    let changes: Changes = [(b"b".to_vec(), None)].into();
    let written = write(&mut pages, &tree, &changes);
    assert_eq!((written.leaves, written.leaf_writes), (1, 1));
    assert_eq!(pages.leaf_pages[from..], [page_id(4)]);
    // The branch with one child went into the branch beside it, and the
    // root took what that one holds.
    pages.commit();
    assert_eq!((leaves(&pages), pages.image(tree.root()).level()), (3, 1));
    let found = records(&pages, &tree).into_iter().map(|(key, _)| key);
    assert_eq!(found.collect::<Vec<_>>().concat(), b"acnotu");
}
