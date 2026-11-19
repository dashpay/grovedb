//! A recovery, as far as the tests of write-back need one: the table
//! checkpoint the manifest names, and the log replayed on it by the rules
//! of `page-table.md` (K3, K11, K13) and `log-format.md` (LG28, LG29).
//! STORE-07 builds the real one. This one exists to show that what
//! write-back leaves on disk is enough for it at every point.

use std::collections::BTreeSet;

use lignin_format::{
    tree_page::TreePage,
    wal::{Change, Entry, LogItem},
    FileId,
};

use super::*;

/// What a recovery found.
pub(super) struct Recovered {
    /// The version it landed on: the last commit whose group the log holds.
    pub(super) landed: CommitVersion,
    /// The version the checkpoint in force covers.
    pub(super) covers: CommitVersion,
    /// Whether the manifest names a checkpoint.
    pub(super) checkpoint: bool,
    /// What each page that was asked for says there, or `None` if it is
    /// no page at that version.
    pub(super) pages: BTreeMap<PageId, Option<Vec<Cell>>>,
}

/// Recovers the store in the directory `path` of `vfs`, and returns what
/// the pages `ids` say at the version it lands on.
///
/// It fails the test if what it finds breaks a rule that write-back is to
/// keep: a link above the version it lands on (K4), a change with no image
/// to apply to whose page is not freed or made again afterwards (K11), a
/// sample's place at which no mark of the checkpoint stands (LG29), or
/// counts of live values that are not the pointers the pages hold (DU5).
pub(super) fn recover(vfs: Arc<dyn Vfs>, path: &Path, ids: &[PageId]) -> Recovered {
    let size = Db::SIZE;
    let manifest = ManifestStore::open(vfs.clone(), path).expect("a manifest");
    let named = manifest.manifest().expect("a manifest").clone();
    let (table, info) = PageTable::load(&*vfs, path, &named).expect("the checkpoint loads");
    let segments = SegmentStore::open(&manifest).expect("the segments open");
    let covers = info
        .as_ref()
        .map_or(CommitVersion::EMPTY, |info| info.covers());
    let place = info.as_ref().and_then(|info| info.sample().place);
    let sample: Vec<FileCount> = info
        .iter()
        .flat_map(|info| &info.sample().counts)
        .map(|count| FileCount {
            file: count.file,
            values: count.values,
            bytes: count.bytes,
            miscounted: count.miscounted,
        })
        .collect();
    let mut counts = ValueStore::recover(&manifest, &sample).expect("the value log opens");
    let pool = BufferPool::new(PoolConfig {
        page_size: size,
        capacity: 4 << 20,
    })
    .expect("a pool");
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
                            Entry::Freed(id) if committed > covers => {
                                frames.insert(id, None);
                                dropped.remove(&id);
                            }
                            Entry::Released(pointer) => change.release(pointer),
                            Entry::Acquired(pointer) => change.acquire(pointer),
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
                        let due = {
                            let tree = TreePage::from_validated(&frame, FRAME_FORMAT);
                            step.is_due_in_tree(&tree.expect("a tree page"))
                        };
                        if due {
                            let delta = step.delta().expect("a tree step");
                            let mut tree = TreePageMut::open(&mut frame, size, FRAME_FORMAT);
                            delta
                                .apply(tree.as_mut().expect("a tree page"))
                                .expect("placed");
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
