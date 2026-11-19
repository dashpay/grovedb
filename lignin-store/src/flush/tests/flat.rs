//! Flat pages in write-back. Every block of a flat page is its image
//! (`page-delta.md` PD13), and a pending value of a flat leaf is found by
//! the place its entry holds, since an entry has no slot to name.

use super::*;

fn cold(db: &Db, id: PageId) -> Vec<(Vec<u8>, Option<Vec<u8>>, Option<ValuePointer>)> {
    let pool = BufferPool::new(PoolConfig {
        page_size: db.size,
        capacity: 4 << 20,
    })
    .expect("a pool");
    let loader = PageLoader::new(&pool, &db.table, &db.segments, db.size).expect("a loader");
    let frame = loader
        .get(id, db.latest(), &mut LoadBuffers::new())
        .expect("the page is in a segment");
    db.flat_entries(id, &frame)
}

fn out(value: &Val) -> Vec<u8> {
    match value {
        Val::Out(bytes) | Val::Inline(bytes) => bytes.clone(),
    }
}

/// A flat leaf with a value out of line: the value is placed by a page
/// step that finds its entry by the pending value it holds, and the page
/// is then written as an image with the pointer. Every later version of
/// the page is an image too, whatever was kept of it.
#[test]
fn a_flat_leaf_is_written_as_images_with_its_values_placed() {
    let db = Db::new();
    let id = page(7);
    let (first, second, third) = (long(1, 900), long(2, 700), long(3, 800));
    db.commit_flat(
        id,
        &[
            (b"a", inline(b"in the page")),
            (b"b", first.clone()),
            (b"c", second.clone()),
        ],
        &BTreeMap::new(),
    );
    let warm = db.frame(id, db.latest()).expect("the page");
    assert_eq!(
        db.flat_entries(id, &warm)
            .iter()
            .filter(|entry| entry.1.is_none())
            .count(),
        2
    );
    drop(warm);

    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.placed, pass.written, pass.waiting), (2, 1, 0));
    let stats = db.stats();
    assert_eq!(
        (stats.values_placed, stats.base_blocks, stats.delta_blocks),
        (2, 1, 0)
    );
    assert_eq!(db.shape(id), "B");
    let entries = cold(&db, id);
    let values: Vec<Option<Vec<u8>>> = entries.iter().map(|entry| entry.1.clone()).collect();
    assert_eq!(
        values,
        [
            Some(b"in the page".to_vec()),
            Some(out(&first)),
            Some(out(&second))
        ]
    );
    let live: u64 = db.values.counts().iter().map(|count| count.values).sum();
    assert_eq!(live, 2);

    // The leaf is written again, with one value more and the two that are
    // placed left where they are.
    let placed: BTreeMap<Vec<u8>, ValuePointer> = entries
        .iter()
        .filter_map(|(key, _, pointer)| Some((key.clone(), (*pointer)?)))
        .collect();
    db.commit_flat(
        id,
        &[
            (b"a", inline(b"changed")),
            (b"b", first.clone()),
            (b"c", second.clone()),
            (b"d", third.clone()),
        ],
        &placed,
    );
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.placed, pass.written, pass.waiting), (1, 1, 0));
    assert_eq!(db.shape(id), "B");
    let stats = db.stats();
    assert_eq!(
        (stats.base_blocks, stats.delta_blocks, stats.runs_refused),
        (2, 0, 0)
    );
    let entries = cold(&db, id);
    let values: Vec<Option<Vec<u8>>> = entries.iter().map(|entry| entry.1.clone()).collect();
    assert_eq!(
        values,
        [
            Some(b"changed".to_vec()),
            Some(out(&first)),
            Some(out(&second)),
            Some(out(&third))
        ]
    );
    assert_eq!(entries[1].2, placed.get(&b"b"[..]).copied());
    let live: u64 = db.values.counts().iter().map(|count| count.values).sum();
    assert_eq!(live, 3);
    installed(db.wb().checkpoint().expect("a checkpoint"));
}

fn installed(checkpointed: Checkpointed) {
    assert!(
        matches!(checkpointed, Checkpointed::Installed { .. }),
        "{checkpointed:?}"
    );
}

/// A value of a flat leaf waits for a write transaction that holds a
/// private copy of the leaf, as a value of a tree page does.
#[test]
fn a_value_of_a_flat_leaf_waits_for_a_private_copy() {
    let db = Db::new();
    let id = page(7);
    db.commit_flat(id, &[(b"a", long(1, 900))], &BTreeMap::new());
    let owner = OwnerId::new(99);
    let copy = db
        .pool
        .make_private(owner, id, db.latest())
        .expect("a private copy");
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.placed, pass.written, pass.waiting), (0, 0, 1));
    assert_eq!(db.stats().steps_put_off, 1);

    db.pool.discard(&copy).expect("discarded");
    assert_eq!(db.wb().place_ready().expect("placed"), 1);
    let pass = db.wb().pass().expect("a pass");
    assert_eq!((pass.written, pass.waiting), (1, 0));
    assert_eq!(cold(&db, id)[0].1, Some(out(&long(1, 900))));
}
