//! The two sentences of the task, each on a tree that is written, split
//! and read through the store.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{
    scope::{check_attached, value_reads},
    *,
};

/// How many of `reads` begin where the read before them ended, in the same
/// file.
fn sequential(reads: &[(String, u64, u64)]) -> usize {
    let follows =
        |pair: &&[(String, u64, u64)]| pair[1].0 == pair[0].0 && pair[1].1 == pair[0].1 + pair[0].2;
    reads.windows(2).filter(follows).count()
}

/// **After consolidation, a scan over large values reads value-log blocks
/// sequentially.**
///
/// 2,000 values of 300 to 700 bytes are written in an order that is not
/// the keys', a commit for every 50 and a write-back after each, so the
/// value log holds them in the order they came. A scan in key order then
/// reads the value log at a new place for nearly every value. Then every
/// page is consolidated: its values are regrouped. After that the values
/// of each page lie back to back in scan order, and the same scan reads
/// the file on from where it stood for all values but the first of each
/// run.
#[test]
fn a_scan_over_large_values_reads_the_value_log_sequentially_after_consolidation() {
    const COUNT: u32 = 2_000;
    let db = Db::traced();
    let tree = grown(&db, &shuffled(COUNT, 77), 50);
    let expected: Vec<(Vec<u8>, Vec<u8>)> =
        (0..COUNT).map(|index| (key(index), big(index))).collect();
    // A scan in key order, as a cursor makes it: down the links from the
    // root, each value read where its cell says it is. Returns where each
    // record's cell is and the reads of the value log, one for each.
    let scan = |db: &Db| {
        let from = db.fs.trace().len();
        let scanned = tree.scan(db);
        let reads = value_reads(db, from);
        let cells: Vec<CellAddress> = scanned.iter().map(|read| read.at).collect();
        let records: Vec<(Vec<u8>, Vec<u8>)> = scanned
            .into_iter()
            .map(|read| (read.key, read.value))
            .collect();
        assert_eq!(records, expected);
        assert_eq!(reads.len(), COUNT as usize, "one read for each value");
        (cells, reads)
    };

    let (cells, before) = scan(&db);
    let scattered = sequential(&before);

    // Consolidation: the values of each page are regrouped, the pages in
    // the order of their ids, into a file that takes nothing else.
    let pages = db.pages();
    let mut to = db.close_value_file();
    let mut runs_before = 0;
    let mut moved = 0;
    for &page in &pages {
        let Some(regrouped) = db.regroup(page, &mut to, RegroupPolicy::ALWAYS) else {
            continue;
        };
        assert_eq!((regrouped.gone, regrouped.busy), (0, 0));
        assert!(regrouped.after.is_sequential(), "{page}: {regrouped:?}");
        runs_before += regrouped.before.runs;
        moved += regrouped.moved;
    }
    db.write_back();
    db.check_counts();
    check_attached(&db);

    // Every page's values are one run, in the order a scan reaches them.
    let mut place: HashMap<CellAddress, usize> = HashMap::new();
    for &page in &pages {
        let frame = db.current(page);
        let view = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
        assert!(Locality::of(&view).is_sequential(), "{page}");
        for (index, slot) in scan_order(&view).into_iter().enumerate() {
            place.insert(CellAddress::new(page, slot), index);
        }
    }

    let (cells_after, after) = scan(&db);
    assert_eq!(cells_after, cells, "regrouping moves no cell");
    let grouped = sequential(&after);

    // Wherever the scan goes on inside a page, from one cell to the next
    // one of the page, the read goes on where the last one ended.
    let mut inside = 0;
    for (index, pair) in cells.windows(2).enumerate() {
        let next_in_its_page =
            pair[0].page() == pair[1].page() && place[&pair[1]] == place[&pair[0]] + 1;
        if next_in_its_page {
            inside += 1;
            let (last, read) = (&after[index], &after[index + 1]);
            assert_eq!(
                (&read.0, read.1),
                (&last.0, last.1 + last.2),
                "record {index}"
            );
        }
    }
    println!(
        "{COUNT} values in {} pages of {} bytes: {scattered} of {} reads began where the \
         read before them ended before consolidation, {grouped} after; the scan stays \
         inside a page {inside} times; {runs_before} runs before, {} after; {moved} values \
         appended again",
        pages.len(),
        SIZE.get(),
        COUNT - 1,
        pages.len(),
    );
    assert!(
        scattered * 20 < COUNT as usize,
        "written in another order than the keys', the values are scattered: {scattered}"
    );
    // What is left are the places where the scan leaves a page: a node
    // high in the tree stands between the pages below it.
    assert!(inside * 100 >= COUNT as usize * 85, "{inside}");
    assert_eq!(moved, u64::from(COUNT));
}

/// **Every value pointer a cell stops holding is reported to the value log
/// in the same commit.**
///
/// Random puts, overwrites and deletes of long and short values, in
/// commits of several writes, over a tree whose cells are carried between
/// pages as it grows and shrinks. Between commits, and beside an open
/// scope, write-back places the values, or leaves them pending; pages are
/// regrouped; value-log files are cleaned; a scope is rolled back.
///
/// After every commit:
///
/// - the pointers its group releases are exactly the pointers that cells
///   held before it and no cell holds after it
/// - the value log's counts are the cells that hold a pointer into each
///   file, with their bytes, and no count was ever found wrong
/// - every value reads back, through the owner check of the value log
/// - each value in the group is for the page whose cell holds its name
///
/// At the end every key is deleted, and no file counts a live value.
#[test]
fn every_pointer_a_cell_stops_holding_is_reported_in_its_commit() {
    let mut totals = (0, 0, 0, 0);
    for seed in 0..4u64 {
        let db = Db::new();
        let mut rng = Rng(seed);
        let mut tree = Tree::new();
        let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        let mut cleaning = db.close_value_file();
        let value = |rng: &mut Rng| match rng.below(10) {
            0..=6 => bytes(rng.next(), 257 + rng.below(600)),
            7 => Vec::new(),
            _ => bytes(rng.next(), 1 + rng.below(256)),
        };

        for round in 0..150 {
            let saved = (tree.clone(), model.clone());
            let mut scope = db.scope();
            for _ in 0..1 + rng.below(4) {
                for _ in 0..1 + rng.below(4) {
                    match rng.below(10) {
                        0..=5 => {
                            let key = key(rng.below(600) as u32);
                            let value = value(&mut rng);
                            tree.put(&key, &value);
                            model.insert(key, value);
                        }
                        _ => {
                            let key = key(rng.below(600) as u32);
                            assert_eq!(tree.delete(&key), model.remove(&key).is_some());
                        }
                    }
                }
                let report = tree.flush(&mut scope);
                totals.0 += report.carries.total().values;
                // Write-back runs beside the open scope, and leaves the
                // pages the scope holds as they are.
                if rng.below(6) == 0 {
                    db.write_back();
                }
            }
            // What the scope reads is what it wrote.
            if let Some((key, value)) = model.iter().nth(rng.below(model.len().max(1))) {
                assert_eq!(&scope.read(tree.at(key)).expect("read"), value);
            }
            if rng.below(8) == 0 {
                db.roll_back(scope);
                (tree, model) = saved;
                db.check_counts();
                continue;
            }

            let held = |db: &Db| -> HashSet<ValuePointer> {
                db.pointers()
                    .into_iter()
                    .map(|(_, pointer)| pointer)
                    .collect()
            };
            let before = held(&db);
            db.commit(scope);
            let after = held(&db);
            let group = db.groups().pop().expect("a group");
            let mut stopped: Vec<ValuePointer> = before.difference(&after).copied().collect();
            stopped.sort_by_key(|pointer| (pointer.file(), pointer.offset()));
            let released = group.released.clone();
            assert_eq!(released, stopped, "seed {seed}, round {round}");
            assert!(after.is_subset(&before), "a commit gives no cell a pointer");
            totals.1 += released.len();
            totals.2 += group.values.len();
            db.check_counts();
            check_attached(&db);

            let scanned = tree.scan(&db);
            let read: Vec<(&Vec<u8>, &Vec<u8>)> = scanned
                .iter()
                .map(|read| (&read.key, &read.value))
                .collect();
            assert_eq!(
                read,
                model.iter().collect::<Vec<_>>(),
                "seed {seed}, round {round}"
            );
            for (owner, name, bytes) in &group.values {
                let holder = scanned
                    .iter()
                    .find(|read| read.held == Some(ValuePlace::Pending(*name)));
                let holder = holder.expect("a cell holds the value's name");
                assert_eq!((holder.at.page(), &holder.value), (*owner, bytes));
            }

            match rng.below(10) {
                0..=5 => db.write_back(),
                6 => {
                    db.write_back();
                    let pages = db.pages();
                    let page = pages[rng.below(pages.len())];
                    if let Some(regrouped) = db.regroup(page, &mut cleaning, RegroupPolicy::ALWAYS)
                    {
                        assert!(regrouped.after.is_sequential());
                        totals.3 += regrouped.moved;
                    }
                }
                7 => {
                    db.write_back();
                    let closed = db.close_value_file();
                    db.finish(std::mem::replace(&mut cleaning, closed));
                    let mut manifest = db.manifest.borrow_mut();
                    let owners = db.owners();
                    for cleanable in db.values.cleanable(0) {
                        let cleaned = db
                            .values
                            .clean(cleanable.file, &mut cleaning, &mut manifest, &owners)
                            .expect("every live value has its owner");
                        assert!(cleaned.is_empty(), "{cleaned:?}");
                        totals.3 += cleaned.moved;
                    }
                }
                _ => {}
            }
            db.check_counts();
        }

        let mut scope = db.scope();
        for key in std::mem::take(&mut model).into_keys() {
            assert!(tree.delete(&key));
            tree.flush(&mut scope);
        }
        db.commit(scope);
        assert_eq!(db.pointers(), []);
        assert_eq!(db.values.counts(), [], "seed {seed}");
        assert_eq!(db.pages(), []);
    }
    println!(
        "{} values carried with their cells, {} pointers released by commits, {} values \
         logged, {} values moved by regrouping and cleaning",
        totals.0, totals.1, totals.2, totals.3
    );
    assert!(
        totals.0 > 100 && totals.1 > 1_000 && totals.3 > 100,
        "{totals:?}"
    );
}
