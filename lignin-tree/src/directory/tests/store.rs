//! The directory on the page store: it is written in the commit of the
//! transaction that changes it, and read at a version.

use lignin_format::CommitVersion;

use super::*;
use crate::flat::tests::store::{Db, Reader, Writer};

/// A directory in a store, with the paths of its Merks.
struct Stored {
    db: Db,
    root: PageId,
}

impl Stored {
    fn load(&self, pages: &impl FlatPages) -> Directory {
        Directory::load(pages, self.root, self.db.config).expect("a directory")
    }

    fn open(&self, reader: &Reader<'_>, keys: &[&str]) -> Option<Entry> {
        let prefix = prefix_of(&path(keys));
        self.load(reader).open(reader, &prefix).expect("a lookup")
    }

    /// Enters the Merk at `keys` with a root, through `writer`.
    fn add(&self, writer: &mut Writer<'_>, keys: &[&str], at: CellAddress) {
        let directory = self.load(writer);
        let whole = path(keys);
        let prefix = prefix_of(&whole);
        let above = whole
            .split_last()
            .map(|(seg, above)| (seg, prefix_of(above)));
        let host = above.as_ref().map(|((key, position), parent)| Host {
            parent,
            key,
            position: *position,
        });
        directory
            .add(writer, &prefix, b"tree", host)
            .expect("entered");
        let change = named(&prefix, Some((at, b"root")));
        directory.set_roots(writer, &[change]).expect("a root");
    }
}

fn stored() -> (Stored, CommitVersion) {
    let db = Db::new();
    let mut writer = db.writer();
    let directory = Directory::create(&mut writer, db.config).expect("a directory");
    let root = directory.root_page();
    let first = db.commit(writer);
    let stored = Stored { db, root };
    let mut writer = stored.db.writer();
    let merks = [&[][..], &["a"], &["a", "b"], &["a", "b", "c"], &["e"]];
    for (slot, keys) in (1..).zip(merks) {
        stored.add(&mut writer, keys, cell(5, slot));
    }
    let second = stored.db.commit(writer);
    assert!(second > first);
    (stored, second)
}

#[test]
fn the_directory_is_written_in_the_commit_of_its_transaction() {
    let (stored, entered) = stored();
    let db = &stored.db;
    // Two commits so far, each one group of the log, with the pages of
    // the directory among its runs and nothing written anywhere else.
    let groups = db.groups();
    assert_eq!(groups.len(), 2);
    assert!(groups
        .iter()
        .all(|group| !group.runs.is_empty() && group.freed.is_empty()));
    assert!(groups[1].runs.iter().any(|(page, _)| *page == stored.root));
    let reader = db.reader();
    for keys in [&[][..], &["a"], &["a", "b"], &["a", "b", "c"], &["e"]] {
        let entry = stored.open(&reader, keys).expect("the Merk is entered");
        assert_eq!(entry.depth(), keys.len());
    }
    stored.load(&reader).check(&reader).expect("a directory");

    // A write transaction moves a root and enters a Merk. Until it
    // commits, no reader has any of it.
    let mut writer = db.writer();
    let directory = stored.load(&writer);
    let carried = RootChange::Carried {
        from: cell(5, 3),
        to: cell(8, 1),
    };
    directory
        .set_roots(&mut writer, &[carried])
        .expect("carried");
    stored.add(&mut writer, &["e", "f"], cell(8, 2));
    let moved = prefix_of(&path(&["a", "b"]));
    let mine = directory
        .open(&writer, &moved)
        .expect("a lookup")
        .expect("entered");
    assert_eq!(mine.root().map(|root| root.address), Some(cell(8, 1)));
    let reader = db.reader();
    let root = |entry: Option<Entry>| entry.and_then(|entry| Some(entry.root()?.address));
    assert_eq!(root(stored.open(&reader, &["a", "b"])), Some(cell(5, 3)));
    assert!(stored.open(&reader, &["e", "f"]).is_none());
    assert_eq!(db.groups().len(), 2);

    // Its commit is one group, and a reader of the new version has all of
    // it. A reader of the version before has none of it.
    let committed = db.commit(writer);
    assert_eq!(db.groups().len(), 3);
    let reader = db.reader_at(committed);
    assert_eq!(root(stored.open(&reader, &["a", "b"])), Some(cell(8, 1)));
    assert_eq!(root(stored.open(&reader, &["e", "f"])), Some(cell(8, 2)));
    stored.load(&reader).check(&reader).expect("a directory");
    let before = db.reader_at(entered);
    assert_eq!(root(stored.open(&before, &["a", "b"])), Some(cell(5, 3)));
    assert!(stored.open(&before, &["e", "f"]).is_none());
    stored.load(&before).check(&before).expect("a directory");

    // A transaction that is rolled back leaves nothing.
    let mut writer = db.writer();
    let mut directory = stored.load(&writer);
    directory
        .unlink(&mut writer, &prefix_of(&path(&["a"])))
        .expect("dropped");
    assert!(directory.open(&writer, &moved).expect("a lookup").is_none());
    db.roll_back(writer);
    assert_eq!(db.groups().len(), 3);
    let reader = db.reader();
    assert_eq!(root(stored.open(&reader, &["a", "b"])), Some(cell(8, 1)));
    stored.load(&reader).check(&reader).expect("a directory");
}

/// The directory's part of T-CL1 (`transactions.md` 15.2), on the store.
#[test]
fn a_dropped_subtree_is_opened_by_the_snapshots_from_before_and_by_no_other() {
    let (stored, entered) = stored();
    let db = &stored.db;
    let below: [&[&str]; 2] = [&["a", "b"], &["a", "b", "c"]];

    let mut writer = db.writer();
    let mut directory = stored.load(&writer);
    let dropped = directory
        .unlink(&mut writer, &prefix_of(&path(&["a"])))
        .expect("dropped");
    // Inside the transaction, from the drop on.
    for keys in below {
        let prefix = prefix_of(&path(keys));
        assert!(directory
            .open(&writer, &prefix)
            .expect("a lookup")
            .is_none());
        assert!(stored
            .load(&writer)
            .open(&writer, &prefix)
            .expect("a lookup")
            .is_none());
        // Every other handle still has the subtree.
        assert!(stored.open(&db.reader(), keys).is_some());
    }
    let at = db.commit(writer);

    // From the drop's commit on, no open returns an entry of it, though
    // the entries are in the pages.
    let reader = db.reader_at(at);
    let after = stored.load(&reader);
    assert!(stored.open(&reader, &["a"]).is_none());
    for keys in below {
        let prefix = prefix_of(&path(keys));
        assert!(after.open(&reader, &prefix).expect("a lookup").is_none());
        assert!(after.stored(&reader, &prefix).expect("a lookup").is_some());
    }
    assert!(stored.open(&reader, &["e"]).is_some());
    let census = after.check(&reader).expect("a directory");
    assert_eq!((census.live, census.left_over, census.dropped), (2, 2, 1));
    // The snapshot from before the drop reads all of it.
    let before = db.reader_at(entered);
    for keys in [&["a"][..], &["a", "b"], &["a", "b", "c"]] {
        assert!(stored.open(&before, keys).is_some());
    }

    // The subtree is entered again under the same path, and starts empty.
    let mut writer = db.writer();
    stored.add(&mut writer, &["a"], cell(6, 1));
    let again = db.commit(writer);
    let reader = db.reader_at(again);
    let entry = stored.open(&reader, &["a"]).expect("entered again");
    assert!(entry.id() > dropped.id);
    for keys in below {
        assert!(stored.open(&reader, keys).is_none());
    }

    // The reclaimer comes: before and after it, the same answers, and the
    // snapshot from before the drop still reads the subtree.
    let mut writer = db.writer();
    let mut directory = stored.load(&writer);
    let mut children = directory.children(dropped.id);
    let b = children
        .next(&writer)
        .expect("a record")
        .expect("the Merk that was below");
    let c = directory
        .children(b.id)
        .next(&writer)
        .expect("a record")
        .expect("below that");
    directory
        .reclaim_child(&mut writer, b.id, &c)
        .expect("reclaimed");
    directory
        .reclaim_child(&mut writer, dropped.id, &b)
        .expect("reclaimed");
    directory
        .reclaimed(&mut writer, dropped.id)
        .expect("reclaimed");
    let reclaimed = db.commit(writer);
    let reader = db.reader_at(reclaimed);
    let after = stored.load(&reader);
    for keys in below {
        let prefix = prefix_of(&path(keys));
        assert!(after.open(&reader, &prefix).expect("a lookup").is_none());
        assert!(after.stored(&reader, &prefix).expect("a lookup").is_none());
    }
    let census = after.check(&reader).expect("a directory");
    assert_eq!((census.live, census.left_over, census.dropped), (3, 0, 0));
    for keys in [&["a"][..], &["a", "b"], &["a", "b", "c"]] {
        let entry = stored
            .open(&before, keys)
            .expect("the snapshot has the subtree");
        assert!(entry.id().get() <= dropped.id.get() + 2);
    }
    let reader = db.reader_at(at);
    assert_eq!(
        stored
            .load(&reader)
            .check(&reader)
            .expect("a directory")
            .left_over,
        2
    );
}

#[test]
fn the_directory_is_read_from_the_segments_and_the_value_log() {
    let (stored, _) = stored();
    let db = &stored.db;
    // A Merk so deep that its entry is out of line.
    let names: Vec<String> = (0..200).map(|level| format!("deep-{level}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut writer = db.writer();
    for depth in 1..=names.len() {
        stored.add(&mut writer, &names[..depth], cell(40, depth as u16));
    }
    let at = db.commit(writer);
    db.write_back();
    db.cold(at, |reader| {
        let directory = stored.load(reader);
        let prefix = prefix_of(&path(&names));
        let entry = directory
            .open(reader, &prefix)
            .expect("a lookup")
            .expect("entered");
        assert_eq!(entry.depth(), 200);
        assert!(entry.as_bytes().len() > db.config.max_inline());
        assert_eq!(entry.root().map(|root| root.address), Some(cell(40, 200)));
        let census = directory.check(reader).expect("a directory");
        assert_eq!((census.live, census.left_over), (205, 0));
    });
}
