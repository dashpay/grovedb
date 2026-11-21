//! A recovery, as far as the tests of write-back need one: the table
//! checkpoint the manifest names, and the log replayed on it by the rules
//! of `page-table.md` (K3, K11, K13) and `log-format.md` (LG28, LG29).
//! It exists to show that what write-back leaves on disk is enough for a
//! recovery at every point, and it is written apart from the store's own
//! ([`crate::recovery`], STORE-07), which [`the_store_recovers_the_same`]
//! holds to it: on every state that a sweep recovers, the two land on the
//! same version and say the same of every page.

use std::collections::BTreeSet;

use lignin_format::{
    tree_page::TreePage,
    wal::{Change, Entry, LogItem},
    FileId,
};

use super::*;
use crate::{
    alloc::PageState,
    flush::page::pending_cells,
    recovery::{self, RecoveryOptions},
};

/// What a recovery found.
pub(crate) struct Recovered {
    /// The version it landed on: the last commit whose group the log holds.
    pub(crate) landed: CommitVersion,
    /// The version the checkpoint in force covers.
    pub(crate) covers: CommitVersion,
    /// Whether the manifest names a checkpoint.
    pub(crate) checkpoint: bool,
    /// What each page that was asked for says there, or `None` if it is
    /// no page at that version.
    pub(crate) pages: BTreeMap<PageId, Option<Vec<Cell>>>,
}

/// Recovers the store in the directory `path` of `vfs`, and returns what
/// the pages `ids` say at the version it lands on.
///
/// It fails the test if what it finds breaks a rule that write-back is to
/// keep: a link above the version it lands on (K4), a change with no image
/// to apply to whose page is not freed or made again afterwards (K11), a
/// sample's place at which no mark of the checkpoint stands (LG29), or
/// counts of live values that are not the pointers the pages hold (DU5).
pub(crate) fn recover(vfs: Arc<dyn Vfs>, path: &Path, ids: &[PageId]) -> Recovered {
    let size = Db::SIZE;
    let manifest = ManifestStore::open(vfs.clone(), path).expect("a manifest");
    let named = manifest.manifest().expect("a manifest").clone();
    let (table, info) = PageTable::load(&*vfs, path, &named).expect("the checkpoint loads");
    let segments = SegmentStore::open(&manifest).expect("the segments open");
    let covers = info
        .as_ref()
        .map_or(CommitVersion::EMPTY, |info| info.covers());
    let place = info.as_ref().and_then(|info| info.sample().place);
    let sample = sample_counts(info.as_ref());
    let mut counts = ValueStore::recover(&manifest, &sample).expect("the value log opens");
    let pool = new_pool(size);
    let loader = PageLoader::new(&pool, &table, &segments, size).expect("a loader");
    let mut buffers = LoadBuffers::new();

    // The pages that replay has changed, and `None` for one it has freed.
    let mut frames: BTreeMap<PageId, Option<Vec<u8>>> = BTreeMap::new();
    let mut dropped = BTreeSet::new();
    let mut marked = false;
    // The page as its chain gives it, for replay to go on from.
    let mut image_of = |id: PageId| -> Option<Vec<u8>> {
        let newest = table
            .newest(id)
            .filter(|link| link.kind != LinkKind::Free)?;
        let frame = loader.get(id, newest.version, &mut buffers);
        Some(frame.expect("the chain of a page reads").to_vec())
    };

    let mut reader = LogReader::new(vfs.clone(), path, &named);
    loop {
        let more = reader.next_item(|read| {
            let counted = place.is_none_or(|place| read.at >= place);
            match read.item {
                LogItem::Commit(group) => {
                    let committed = group.version();
                    let mut change = ValueChange::new();
                    for entry in group.entries() {
                        match entry {
                            Entry::Tree(run) if committed > covers => {
                                let id = run.page();
                                let has = table
                                    .newest(id)
                                    .is_some_and(|link| link.version >= committed);
                                if has {
                                    continue;
                                }
                                let known = frames.remove(&id).flatten();
                                let frame = match (known, run.created()) {
                                    (_, Some(_)) => Some(vec![0; size.get()]),
                                    (Some(frame), None) => Some(frame),
                                    (None, None) => image_of(id),
                                };
                                match frame {
                                    Some(mut frame) => {
                                        run.apply(&mut frame, size, V1).expect("a run applies");
                                        frames.insert(id, Some(frame));
                                        dropped.remove(&id);
                                    }
                                    // No image to apply to: the page was
                                    // freed and trimmed away (K11).
                                    None => drop(dropped.insert(id)),
                                }
                            }
                            Entry::Flat(run) if committed > covers => {
                                let id = run.page();
                                let has = table
                                    .newest(id)
                                    .is_some_and(|link| link.version >= committed);
                                if has {
                                    continue;
                                }
                                let known = frames.remove(&id).flatten();
                                let before = match (known, run.image()) {
                                    // An image needs no page before it.
                                    (_, Some(_)) => Some(None),
                                    (Some(frame), None) => Some(Some(frame)),
                                    (None, None) => image_of(id).map(Some),
                                };
                                match before {
                                    Some(before) => {
                                        let mut image = Vec::new();
                                        let capacity = size.image_capacity();
                                        run.apply(before.as_deref(), V1, capacity, &mut image)
                                            .expect("a flat run applies");
                                        image.resize(size.get(), 0);
                                        frames.insert(id, Some(image));
                                        dropped.remove(&id);
                                    }
                                    None => drop(dropped.insert(id)),
                                }
                            }
                            Entry::Freed(id) if committed > covers => {
                                // The chain already holds what the commit
                                // did, and more: the id was given to a new
                                // page since, and its block was linked
                                // before the checkpoint copied the chain
                                // (K3, as for a run).
                                let has = table
                                    .newest(id)
                                    .is_some_and(|link| link.version >= committed);
                                if has {
                                    continue;
                                }
                                frames.insert(id, None);
                                dropped.remove(&id);
                            }
                            Entry::Released(pointer) => change.release(pointer),
                            Entry::Acquired(pointer) => change.acquire(pointer),
                            // A commit that the checkpoint covers.
                            Entry::Tree(_) | Entry::Flat(_) | Entry::Freed(_) => {}
                        }
                    }
                    if counted {
                        counts
                            .apply(&change)
                            .expect("the counts take a commit's change");
                    }
                }
                LogItem::Change(Change::Value(step)) if counted => {
                    let id = step.page();
                    let frame = match frames.remove(&id) {
                        Some(known) => known,
                        None => image_of(id),
                    };
                    if let Some(mut frame) = frame {
                        match step.slot() {
                            Some(_) => {
                                let due = {
                                    let tree = TreePage::from_validated(&frame, FRAME_FORMAT);
                                    step.is_due_in_tree(&tree.expect("a tree page"))
                                };
                                if due {
                                    let delta = step.delta().expect("a tree step");
                                    let mut tree =
                                        TreePageMut::open(&mut frame, size, FRAME_FORMAT);
                                    delta
                                        .apply(tree.as_mut().expect("a tree page"))
                                        .expect("placed");
                                }
                            }
                            // An entry of a flat leaf, found by the place
                            // it holds.
                            None => {
                                let due = step.is_due_in_flat_leaf(&frame, FRAME_FORMAT);
                                if due.expect("a flat leaf") {
                                    let mut image = Vec::new();
                                    let capacity = size.image_capacity();
                                    step.apply_to_flat_leaf(
                                        &frame,
                                        FRAME_FORMAT,
                                        capacity,
                                        &mut image,
                                    )
                                    .expect("placed");
                                    image.resize(size.get(), 0);
                                    frame = image;
                                }
                            }
                        }
                        frames.insert(id, Some(frame));
                    } else if table.newest(id).is_none() {
                        dropped.insert(id);
                    } else {
                        frames.insert(id, None);
                    }
                    let mut change = ValueChange::new();
                    step.released()
                        .into_iter()
                        .for_each(|from| change.release(from));
                    change.acquire(step.acquired());
                    counts
                        .apply(&change)
                        .expect("the counts take a step's change");
                }
                LogItem::Change(_) => {}
                LogItem::Mark(mark) => {
                    if Some(read.end) == place {
                        assert_eq!(mark.covers(), covers, "the mark of the checkpoint");
                        marked = true;
                    }
                }
            }
        });
        if more.expect("the log reads").is_none() {
            break;
        }
    }
    assert!(
        place.is_none() || marked,
        "a mark ends at the sample's place"
    );
    assert!(dropped.is_empty(), "changes with no image: {dropped:?}");
    let end = reader.end().expect("the log has ended").clone();
    let landed = end.latest();
    assert!(
        landed >= covers,
        "the log reaches what the checkpoint covers"
    );
    assert_eq!(
        table.discard_after(landed),
        0,
        "a link above version {landed}"
    );

    let values = counts.finish().expect("the counts are whole");
    let log = LogStore::open(&manifest, &end, LogOptions::default()).expect("the log opens");
    let mut pages = BTreeMap::new();
    let mut pointers: BTreeMap<FileId, u64> = BTreeMap::new();
    for &id in ids {
        let frame = match frames.remove(&id) {
            Some(replayed) => replayed,
            None => match loader.get(id, landed, &mut buffers) {
                Ok(frame) => Some(frame.to_vec()),
                Err(LoadError::NoPage { .. }) => None,
                Err(error) => panic!("page {id} at version {landed}: {error}"),
            },
        };
        let cells = frame.map(|frame| cells_of(id, &frame, None, &values, &log));
        for pointer in cells.iter().flatten().filter_map(|cell| cell.pointer) {
            *pointers.entry(pointer.file()).or_default() += 1;
        }
        pages.insert(id, cells);
    }
    let counted: BTreeMap<FileId, u64> = values
        .counts()
        .iter()
        .inspect(|count| assert!(!count.miscounted, "{count:?}"))
        .filter(|count| count.values > 0)
        .map(|count| (count.file, count.values))
        .collect();
    assert_eq!(
        counted, pointers,
        "the counts are the pointers the pages hold"
    );
    Recovered {
        landed,
        covers,
        checkpoint: info.is_some(),
        pages,
    }
}

/// How the store's recovery is run on a state: with the pool of these
/// tests, and with as many rebuilt pages kept in memory as `kept` says.
pub(crate) fn options(kept: Option<usize>) -> RecoveryOptions {
    RecoveryOptions {
        pool_capacity: 4 << 20,
        max_replay_pages: kept,
        ..RecoveryOptions::default()
    }
}

/// What the store's own recovery (`crate::recovery`) makes of the
/// directory `path` of `vfs`, held to what the recovery of these tests
/// found there (`expected`, for the pages `ids`).
///
/// - It lands on the same version, from the same checkpoint, and every
///   page says the same there, with every value. A pending value has its
///   bytes with its frame (`transactions.md` FL7).
/// - The counts of live values are the pointers the pages hold (DU5), and
///   no count was found wrong.
/// - A page id is in use exactly when its page is (ID4).
/// - **Replaying twice gives the same state.** The recovered store is
///   dropped as a killed process leaves it, and the directory is recovered
///   again, this time with one rebuilt page kept in memory, so that every
///   other one is written to a segment while the log is read. The second
///   recovery finds what the first found.
///
/// Returns how many pages the second recovery wrote to a segment while it
/// read the log.
pub(crate) fn the_store_recovers_the_same(
    vfs: Arc<dyn Vfs>,
    path: &Path,
    ids: &[PageId],
    expected: &Recovered,
) -> u64 {
    let mut first = None;
    let mut spilled = 0;
    for kept in [None, Some(1)] {
        let manifest = ManifestStore::open(vfs.clone(), path).expect("a manifest");
        let store = recovery::recover(manifest, &options(kept))
            .unwrap_or_else(|error| panic!("the store does not recover: {error}"));
        let found = found_by(&store, ids);
        assert_eq!(store.landed(), expected.landed, "{kept:?}");
        assert_eq!(store.report.covers, expected.covers, "{kept:?}");
        assert_eq!(store.checkpoint.is_some(), expected.checkpoint, "{kept:?}");
        assert_eq!(store.report.miscounts, 0, "{:?}", store.report);
        for (id, cells) in &found {
            match (cells, &expected.pages[id]) {
                (Some(cells), Some(expected)) => assert!(
                    same(cells, expected),
                    "page {id}, {kept:?}: {cells:?}, not {expected:?}"
                ),
                (None, None) => {}
                (cells, expected) => panic!("page {id}, {kept:?}: {cells:?}, not {expected:?}"),
            }
        }
        match &first {
            None => {
                assert_eq!(store.report.pages_spilled, 0, "{:?}", store.report);
                first = Some((store.landed(), found.len()));
            }
            Some(first) => assert_eq!(*first, (store.landed(), found.len())),
        }
        spilled = store.report.pages_spilled;
    }
    spilled
}

/// What the pages `ids` say in a recovered store at the version it landed
/// on, with what the store says of their ids and of its values checked on
/// the way.
fn found_by(store: &recovery::Recovered, ids: &[PageId]) -> BTreeMap<PageId, Option<Vec<Cell>>> {
    let landed = store.landed();
    let loader =
        PageLoader::new(&store.pool, &store.table, &store.segments, Db::SIZE).expect("a loader");
    let mut buffers = LoadBuffers::new();
    let mut pages = BTreeMap::new();
    let mut pointers: BTreeMap<FileId, u64> = BTreeMap::new();
    for &id in ids {
        let cells = match loader.get(id, landed, &mut buffers) {
            Ok(frame) => {
                assert_eq!(store.allocator.state(id), PageState::Live, "page {id}");
                for cell in pending_cells(&frame).expect("a page") {
                    let bytes = frame.attached(cell.pending.id());
                    assert_eq!(
                        bytes.map(<[u8]>::len),
                        Some(cell.pending.value_len() as usize),
                        "a pending value of page {id} is with its frame"
                    );
                }
                Some(cells_of(
                    id,
                    &frame,
                    Some(&frame),
                    &store.values,
                    &store.log,
                ))
            }
            Err(LoadError::NoPage { .. }) => {
                assert_ne!(store.allocator.state(id), PageState::Live, "page {id}");
                None
            }
            Err(error) => panic!("page {id} at version {landed}: {error}"),
        };
        for pointer in cells.iter().flatten().filter_map(|cell| cell.pointer) {
            *pointers.entry(pointer.file()).or_default() += 1;
        }
        pages.insert(id, cells);
    }
    let counted: BTreeMap<FileId, u64> = store
        .values
        .counts()
        .iter()
        .inspect(|count| assert!(!count.miscounted, "{count:?}"))
        .filter(|count| count.values > 0)
        .map(|count| (count.file, count.values))
        .collect();
    assert_eq!(
        counted, pointers,
        "the counts are the pointers the pages hold"
    );
    pages
}
