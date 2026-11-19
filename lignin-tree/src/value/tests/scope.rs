//! The values of a write scope, from the write to the read of what its
//! commit left.

use lignin_format::{
    value_log::{extent_len, ValueLogError},
    FormatError,
};
use lignin_io::sim::{Event, OpKind};
use lignin_store::{cache::ValueId, values::ValueStoreError};

use super::*;
use crate::fragment::Values;

/// The reads of value-log files that the file system saw from its
/// operation `from` on: the file, the offset and the length of each.
pub(super) fn value_reads(db: &Db, from: usize) -> Vec<(String, u64, u64)> {
    let trace = db.fs.trace();
    let reads = trace[from..].iter().filter_map(|event| match event {
        Event::Op(op) if op.kind == OpKind::Read && op.path.contains("/val/") => {
            Some((op.path.clone(), op.offset, op.len))
        }
        _ => None,
    });
    reads.collect()
}

/// Checks that the bytes that are with each page of the latest version are
/// those of the pending values its cells hold, and no others.
pub(super) fn check_attached(db: &Db) {
    for page in db.pages() {
        let frame = db.current(page);
        let view = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
        let mut held: Vec<(ValueId, usize)> = view
            .pending_values()
            .map(|(_, pending)| (pending.id(), pending.value_len() as usize))
            .collect();
        let mut attached: Vec<(ValueId, usize)> = frame
            .attachments()
            .map(|(name, bytes)| (name, bytes.len()))
            .collect();
        held.sort();
        attached.sort();
        assert_eq!(attached, held, "{page}");
    }
}

fn is_new(pending: PendingValue) -> bool {
    u64::from_le_bytes(pending.id()) >> 63 == 1
}

fn pending_at(scope: &mut Scope<'_>, address: CellAddress) -> PendingValue {
    let place = scope.place(address).expect("a value out of line");
    place.pending().expect("a pending value")
}

/// A tree of `count` keys with large values, committed and written back in
/// rounds of `round` keys, in the order `order` gives.
fn grown(db: &Db, order: &[u32], round: usize) -> Tree {
    let mut tree = Tree::new();
    for keys in order.chunks(round) {
        let mut scope = db.scope();
        for &index in keys {
            tree.put(&key(index), &big(index));
            tree.flush(&mut scope);
        }
        db.commit(scope);
        db.write_back();
    }
    tree
}

fn shuffled(count: u32, seed: u64) -> Vec<u32> {
    let mut rng = Rng(seed);
    let mut order: Vec<u32> = (0..count).collect();
    for index in (1..order.len()).rev() {
        order.swap(index, rng.below(index + 1));
    }
    order
}

/// The life of a value that is too long for its cell: pending in the scope
/// under a name of the scope's own, in the commit's group for the page of
/// its cell and in the cell under its address in the log, read from the
/// frame until write-back has placed it, and from the value log with one
/// positional read afterwards.
#[test]
fn a_new_value_is_pending_until_write_back_places_it() {
    let db = Db::traced();
    let mut tree = Tree::new();
    let mut scope = db.scope();
    tree.put(&key(1), &big(1));
    tree.put(&key(2), b"a value that stays in its cell");
    tree.put(&key(3), b"");
    tree.flush(&mut scope);

    // In the scope: the three cases are told apart by the cell.
    let (long, short, empty) = (tree.at(&key(1)), tree.at(&key(2)), tree.at(&key(3)));
    assert_eq!(scope.place(short), None);
    assert_eq!(scope.place(empty), None);
    let own = pending_at(&mut scope, long);
    assert!(is_new(own) && own.value_len() as usize == big(1).len());
    assert_eq!(scope.read(long).expect("read"), big(1));
    assert_eq!(
        scope.read(short).expect("read"),
        b"a value that stays in its cell"
    );
    assert_eq!(scope.read(empty).expect("read"), b"");
    assert_eq!(scope.values.pages().collect::<Vec<_>>(), [long.page()]);
    assert_eq!(scope.values.pending_in(long.page()), 1);
    assert_eq!(scope.values.pending_in(page_id(77)), 0);
    assert!(scope.values.change().is_empty());

    // The commit: one value in the group, for the page of its cell, and
    // the cell holds the value's address in the log.
    db.commit(scope);
    let groups = db.groups();
    let [(owner, named, logged)] = &groups[0].values[..] else {
        panic!("one value is logged: {:?}", groups[0].values);
    };
    assert_eq!((*owner, &logged[..]), (long.page(), &big(1)[..]));
    assert!(!is_new(*named));
    let scanned = tree.scan(&db);
    assert_eq!(scanned[0].held, Some(ValuePlace::Pending(*named)));
    assert_eq!(groups[0].runs, [long.page()]);
    assert!(groups[0].released.is_empty());
    check_attached(&db);
    assert_eq!(db.values.counts(), []);

    // A reader finds it with the frame: no file of the value log is read.
    let before = db.fs.trace().len();
    let values: Vec<Vec<u8>> = tree.scan(&db).into_iter().map(|read| read.value).collect();
    assert_eq!(
        values,
        [big(1), b"a value that stays in its cell".to_vec(), vec![]]
    );
    assert_eq!(value_reads(&db, before), []);

    // Write-back places it. The cell holds the pointer, the value counts,
    // and a read is one positional read of the value's records.
    db.write_back();
    let scanned = tree.scan(&db);
    let pointer = scanned[0].held.and_then(|place| place.pointer());
    let pointer = pointer.expect("placed");
    assert_eq!(scanned[0].at, long, "placing moves no cell");
    db.check_counts();
    check_attached(&db);
    let before = db.fs.trace().len();
    let read = tree.scan(&db);
    assert_eq!(read[0].value, big(1));
    let reads = value_reads(&db, before);
    let extent = extent_len(pointer.value_len(), FormatVersion::CURRENT);
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert_eq!((reads[0].1, reads[0].2), (pointer.offset(), extent));
}

/// The second sentence of the task: every value pointer a cell stops
/// holding is reported in the same commit. A cell stops holding its
/// pointer when it is given another value, long or short, and when its
/// node is deleted. A pending value has no pointer, and nothing is
/// reported for it.
#[test]
fn a_pointer_that_a_cell_stops_holding_is_released_by_that_commit() {
    let db = Db::new();
    let mut tree = grown(&db, &[1, 2, 3, 4], 4);
    let held: Vec<ValuePointer> = tree
        .scan(&db)
        .iter()
        .map(|read| read.held.and_then(|place| place.pointer()).expect("placed"))
        .collect();
    assert_eq!(
        db.values.count(held[0].file()).map(|count| count.values),
        Some(4)
    );

    let mut scope = db.scope();
    tree.put(&key(1), &big(101));
    tree.put(&key(2), b"short now");
    assert!(tree.delete(&key(3)));
    tree.flush(&mut scope);
    assert_eq!(scope.read(tree.at(&key(1))).expect("read"), big(101));
    let mut expected = held[..3].to_vec();
    expected.sort_by_key(|pointer| (pointer.file(), pointer.offset()));
    assert_eq!(scope.values.released_pointers(), expected);
    let change = scope.values.change();
    assert_eq!(change.released().len(), 3);
    assert!(change.acquired().is_empty());
    // Until the commit the counts are those of the committed cells.
    assert_eq!(
        db.values.count(held[0].file()).map(|count| count.values),
        Some(4)
    );

    db.commit(scope);
    let group = db.groups().pop().expect("a group");
    assert_eq!(group.released, expected);
    assert_eq!(group.values.len(), 1);
    assert_eq!(
        db.values.count(held[0].file()).map(|count| count.values),
        Some(1)
    );
    db.check_counts();

    // The new value of key 1 is pending. Giving the cell another value,
    // and then deleting it, releases nothing: no count ever held it.
    for value in [Some(big(201)), None] {
        let mut scope = db.scope();
        match &value {
            Some(value) => tree.put(&key(1), value),
            None => assert!(tree.delete(&key(1))),
        }
        tree.flush(&mut scope);
        assert_eq!(scope.values.released_pointers(), []);
        db.commit(scope);
        assert_eq!(db.groups().pop().expect("a group").released, []);
        check_attached(&db);
        db.check_counts();
    }
    let left: Vec<Vec<u8>> = tree.scan(&db).into_iter().map(|read| read.key).collect();
    assert_eq!(left, [key(2), key(4)]);
}

/// The move rule (`value-log-format.md` O5): a cell that is carried to
/// another page stops holding its pointer in that commit, and the commit
/// logs the value again for the new page. The value is read from the value
/// log for it, with the page it left as its owner.
#[test]
fn a_carried_cell_has_its_value_logged_again_for_its_new_page() {
    let db = Db::new();
    let mut tree = grown(&db, &shuffled(120, 3), 40);
    let before: HashMap<Vec<u8>, Scanned> = tree
        .scan(&db)
        .into_iter()
        .map(|read| (read.key.clone(), read))
        .collect();
    assert!(before
        .values()
        .all(|read| read.held.is_some_and(|place| !place.is_pending())));

    let mut scope = db.scope();
    let mut carried = 0;
    for index in 120..200 {
        tree.put(&key(index), &big(index));
        carried += tree.flush(&mut scope).carries.total().values;
    }
    assert!(
        carried > 20,
        "the writes carry cells with their values: {carried}"
    );

    // In the scope a carried cell holds a pending value of the scope's
    // own, and reads as before.
    let moved: Vec<&Scanned> = before
        .values()
        .filter(|read| tree.at(&read.key).page() != read.at.page())
        .collect();
    assert!(!moved.is_empty());
    for read in &moved {
        let at = tree.at(&read.key);
        assert!(is_new(pending_at(&mut scope, at)));
        assert_eq!(scope.read(at).expect("read"), read.value);
    }
    let mut released: Vec<ValuePointer> = moved
        .iter()
        .map(|read| read.held.and_then(|place| place.pointer()).expect("placed"))
        .collect();
    released.sort_by_key(|pointer| (pointer.file(), pointer.offset()));
    assert_eq!(scope.values.released_pointers(), released);

    db.commit(scope);
    let group = db.groups().pop().expect("a group");
    assert_eq!(group.released, released);
    assert_eq!(group.values.len(), 80 + moved.len());
    for read in &moved {
        let at = tree.at(&read.key);
        let logged = group
            .values
            .iter()
            .filter(|(owner, _, bytes)| *owner == at.page() && *bytes == read.value);
        assert_eq!(
            logged.count(),
            1,
            "the value is in the group for its new page"
        );
    }
    check_attached(&db);
    db.check_counts();

    // Once it is placed, the value's record names the page of its cell: a
    // read through the pointer checks that.
    db.write_back();
    db.check_counts();
    for read in tree.scan(&db) {
        let expected = before.get(&read.key).map_or_else(
            || big(u32::from_be_bytes(read.key[..].try_into().expect("a key"))),
            |before| before.value.clone(),
        );
        assert_eq!(read.value, expected);
        assert!(read.held.is_some_and(|place| !place.is_pending()));
    }
}

/// A cell that is carried while its value is still pending from an earlier
/// commit held no pointer: nothing is released, and the bytes, which are
/// with the frame of the page it leaves, go into the group again for the
/// page it is in now. What stays behind in a page keeps its bytes there.
#[test]
fn a_cell_carried_while_its_value_is_pending_takes_the_bytes_along() {
    let db = Db::new();
    let mut tree = Tree::new();
    let mut scope = db.scope();
    for index in shuffled(60, 5) {
        tree.put(&key(index), &big(index));
        tree.flush(&mut scope);
    }
    db.commit(scope);
    let before: HashMap<Vec<u8>, Scanned> = tree
        .scan(&db)
        .into_iter()
        .map(|read| (read.key.clone(), read))
        .collect();
    assert!(before
        .values()
        .all(|read| read.held.is_some_and(|place| place.is_pending())));

    let mut scope = db.scope();
    for index in 60..140 {
        tree.put(&key(index), b"short");
        tree.flush(&mut scope);
    }
    let moved: Vec<&Scanned> = before
        .values()
        .filter(|read| tree.at(&read.key).page() != read.at.page())
        .collect();
    let stayed = before.len() - moved.len();
    assert!(
        moved.len() > 5 && stayed > 5,
        "{} of {}",
        moved.len(),
        before.len()
    );
    for read in before.values() {
        let at = tree.at(&read.key);
        let carried = at.page() != read.at.page();
        assert_eq!(is_new(pending_at(&mut scope, at)), carried);
        assert_eq!(scope.read(at).expect("read"), read.value);
    }
    assert_eq!(scope.values.released_pointers(), []);

    db.commit(scope);
    let group = db.groups().pop().expect("a group");
    assert_eq!(group.released, []);
    assert_eq!(group.values.len(), moved.len());
    check_attached(&db);
    let scanned = tree.scan(&db);
    for read in scanned.iter().filter(|read| before.contains_key(&read.key)) {
        assert_eq!(read.value, before[&read.key].value);
        let carried = read.at.page() != before[&read.key].at.page();
        // A value that stayed keeps the address the first commit gave it.
        assert_eq!(read.held == before[&read.key].held, !carried);
    }
    assert_eq!(db.values.counts(), []);

    db.write_back();
    db.check_counts();
    check_attached(&db);
    assert_eq!(db.pointers().len(), before.len());
    for read in tree.scan(&db) {
        if let Some(before) = before.get(&read.key) {
            assert_eq!(read.value, before.value);
        }
    }
}

/// A group is written for one place (`log-format.md` LG22). A commit that
/// writes its group again for another place has every value of the scope
/// written and named anew, and the cells hold the names of the last one.
#[test]
fn a_group_that_is_written_again_names_the_values_anew() {
    let db = Db::new();
    let mut tree = grown(&db, &[1, 2], 2);
    let mut scope = db.scope();
    tree.put(&key(1), &big(11));
    tree.put(&key(3), &big(13));
    tree.flush(&mut scope);
    let cells = [tree.at(&key(1)), tree.at(&key(3))];

    let names = |scope: &mut Scope<'_>| cells.map(|at| pending_at(scope, at));
    let first = names(&mut scope);
    let mut discarded = Vec::new();
    let elsewhere = LogStart::FIRST.address(1 << 20).expect("an address");
    let mut group = GroupWriter::new(version(9), elsewhere, &mut discarded).expect("a group");
    let source = db.source();
    scope
        .values
        .log_values(&mut scope.pages, &source, &mut group)
        .expect("the values");
    drop(group);
    assert!(
        discarded.is_empty(),
        "a group that is not finished leaves nothing"
    );
    let second = names(&mut scope);
    assert!(first.iter().all(|name| is_new(*name)));
    assert!(second.iter().all(|name| !is_new(*name)));
    assert_eq!(scope.read(cells[0]).expect("read"), big(11));

    db.commit(scope);
    let group = db.groups().pop().expect("a group");
    let third: Vec<PendingValue> = group.values.iter().map(|(_, name, _)| *name).collect();
    assert_eq!(group.values.len(), 2);
    assert!(third.iter().all(|name| !second.contains(name)));
    let held: Vec<PendingValue> = tree
        .scan(&db)
        .iter()
        .filter_map(|read| read.held.and_then(|place| place.pending()))
        .collect();
    assert_eq!(held.len(), 2);
    assert!(held.iter().all(|name| third.contains(name)));
    check_attached(&db);
    let values: Vec<Vec<u8>> = tree.scan(&db).into_iter().map(|read| read.value).collect();
    assert_eq!(values, [big(11), big(2), big(13)]);
}

/// A read through a pointer checks the owner (`value-log-format.md` P4,
/// O10): a cell that holds the pointer of a value of another page, as a
/// cell carried without its value would, is not read.
#[test]
fn a_pointer_to_a_value_of_another_page_is_not_read() {
    let db = Db::new();
    let mut tree = grown(&db, &shuffled(120, 9), 120);
    let scanned = tree.scan(&db);
    let (first, other) = (
        &scanned[0],
        scanned
            .iter()
            .find(|read| read.at.page() != scanned[0].at.page()),
    );
    let other = other.expect("a second page");
    let stolen = other
        .held
        .and_then(|place| place.pointer())
        .expect("placed");

    let mut scope = db.scope();
    scope
        .pages
        .write(first.at.page(), |nodes| {
            let mut node = nodes.node_mut(first.at.slot()).expect("the cell");
            node.set_value(CellValue::OutOfLine(stolen))
        })
        .expect("written");
    let refused = scope
        .read(first.at)
        .expect_err("another page owns the value");
    let wrong_owner = |error: &ValueError| {
        matches!(
            error,
            ValueError::Store(ValueStoreError::Format {
                source: FormatError::ValueLog(ValueLogError::WrongOwner { .. }),
                ..
            })
        )
    };
    assert!(wrong_owner(&refused), "{refused}");
    assert!(refused.to_string().starts_with("value log: "));
    assert!(std::error::Error::source(&refused).is_some());
    db.commit(scope);

    let reader = NodeReader::new(db.loader(), &Guard, Lease::take(), db.latest());
    let node = reader
        .node(first.at, &mut LoadBuffers::new())
        .expect("the node");
    let mut buf = ValueBuf::new();
    let refused = read_value(&db.values, &node, &mut buf).expect_err("not read");
    assert!(wrong_owner(&refused), "{refused}");
    // The node that owns the value reads it.
    let node = reader
        .node(other.at, &mut LoadBuffers::new())
        .expect("the node");
    assert_eq!(
        read_value(&db.values, &node, &mut buf).expect("read"),
        other.value
    );
    let _ = &mut tree;
}

/// A pending value whose bytes are not with its page is an error of the
/// read, for a reader and for a scope.
#[test]
fn a_pending_value_without_its_bytes_is_not_read() {
    let db = Db::new();
    let mut tree = Tree::new();
    let mut scope = db.scope();
    tree.put(&key(1), &big(1));
    tree.put(&key(2), &big(2));
    tree.flush(&mut scope);
    let page = tree.at(&key(1)).page();
    db.commit(scope);
    let held = tree.scan(&db)[0].held.and_then(|place| place.pending());
    let held = held.expect("pending");

    // The cell says another length than the bytes with the frame have.
    let longer = PendingValue::new(held.id(), held.value_len() + 1).expect("a pending value");
    // A name that no bytes are attached under: one that an earlier commit
    // could have given, and one of a scope's own.
    let unknown = PendingValue::new(77u64.to_le_bytes(), 300).expect("a pending value");
    let own = PendingValue::new((1u64 << 63 | 5).to_le_bytes(), 300).expect("a pending value");
    for bad in [longer, unknown, own] {
        let mut scope = db.scope();
        let at = tree.at(&key(1));
        scope
            .pages
            .write(page, |nodes| {
                let mut node = nodes.node_mut(at.slot()).expect("the cell");
                node.set_value(CellValue::Pending(bad))
            })
            .expect("written");
        let refused = scope.read(at).expect_err("no such bytes");
        assert!(
            matches!(refused, ValueError::NoValue { page: of } if of == page),
            "{refused}"
        );
        assert_eq!(
            refused.to_string(),
            format!("the bytes of a pending value of page {page} are missing")
        );
        // The commit does not install such a page either.
        let mut latch = scope.pages.copies[&page].frame.write().expect("the latch");
        let refused = scope.values.attach(&mut latch).expect_err("no such bytes");
        assert!(matches!(refused, ValueError::NoValue { .. }), "{refused}");
        drop(latch);
        // The value of the other cell, which the scope did not touch, is
        // read from the frame of the committed page.
        assert_eq!(scope.read(tree.at(&key(2))).expect("read"), big(2));
        db.roll_back(scope);
    }

    // A reader that holds a page whose frame lost the bytes: the page as
    // write-back left it, read for a cell that still says pending.
    let mut scope = db.scope();
    let at = tree.at(&key(1));
    scope
        .pages
        .write(page, |nodes| {
            let mut node = nodes.node_mut(at.slot()).expect("the cell");
            node.set_value(CellValue::Pending(unknown))
        })
        .expect("written");
    for copy in std::mem::take(&mut scope.pages.copies).into_values() {
        db.pool.install(&copy.frame, version(2)).expect("installed");
    }
    db.state.borrow_mut().latest = 2;
    let reader = NodeReader::new(db.loader(), &Guard, Lease::take(), db.latest());
    let node = reader.node(at, &mut LoadBuffers::new()).expect("the node");
    let refused = read_value(&db.values, &node, &mut ValueBuf::new()).expect_err("no bytes");
    assert!(
        matches!(refused, ValueError::NoValue { page: of } if of == page),
        "{refused}"
    );
}

/// What a write tells of a pending value that the scope's values do not
/// hold is kept as an error, and the commit fails with it.
#[test]
fn a_pending_value_that_the_scope_did_not_make_fails_its_commit() {
    let db = Db::new();
    let own = PendingValue::new((1u64 << 63 | 900).to_le_bytes(), 300).expect("a pending value");
    let (from, to) = (page_id(3), page_id(4));

    // A cell is given a pending value that `write` did not make.
    let mut values = TreeValues::new(InlinePolicy::DEFAULT);
    values.check().expect("nothing is lost yet");
    values.placed(own, to);
    let lost = values.check().expect_err("lost");
    assert!(
        matches!(lost, ValueError::NoValue { page } if page == to),
        "{lost}"
    );

    // A cell is carried with a pending value of the scope's own that the
    // scope does not hold for the page it leaves.
    let mut values = TreeValues::new(InlinePolicy::DEFAULT);
    let carried = values.carried(ValuePlace::Pending(own), from, to);
    assert!(is_new(carried) && carried.value_len() == 300);
    let lost = values.check().expect_err("lost");
    assert!(
        matches!(lost, ValueError::NoValue { page } if page == from),
        "{lost}"
    );
    assert_eq!(values.pages().count(), 0);

    // The commit of such a scope fails before it writes a value.
    let mut scope = db.scope();
    scope.values = values;
    let source = db.source();
    let mut log = Vec::new();
    let address = LogStart::FIRST.address(100).expect("an address");
    let mut group = GroupWriter::new(version(1), address, &mut log).expect("a group");
    let refused = scope
        .values
        .log_values(&mut scope.pages, &source, &mut group)
        .expect_err("lost");
    assert!(matches!(refused, ValueError::NoValue { .. }), "{refused}");
    assert_eq!(group.encoded_len(), 0);

    // Releasing a pending value that is not held is not an error: nothing
    // was counted for it.
    let mut values = TreeValues::new(InlinePolicy::DEFAULT);
    values.released(ValuePlace::Pending(own), from);
    values.check().expect("nothing is lost");
    assert!(values.change().is_empty());
}

/// The values a scope holds for a page and the pending values the cells of
/// the page hold are the same set at the commit. A value that no cell
/// holds, a value that two cells hold, and a cell whose length is not the
/// value's each fail it, in the log and at the frame.
#[test]
fn the_values_of_a_scope_are_those_its_cells_hold() {
    let db = Db::new();
    let commit = |scope: &mut Scope<'_>| {
        let source = db.source();
        let mut log = Vec::new();
        let address = LogStart::FIRST.address(100).expect("an address");
        let mut group = GroupWriter::new(version(1), address, &mut log).expect("a group");
        let logged = scope
            .values
            .log_values(&mut scope.pages, &source, &mut group);
        let attached: Vec<Result<(), ValueError>> = scope
            .pages
            .copies
            .values()
            .map(|copy| {
                scope
                    .values
                    .attach(&mut copy.frame.write().expect("the latch"))
            })
            .collect();
        (logged, attached)
    };
    let pending_of = |write: ValueWrite<'_>| match write {
        ValueWrite::Pending(pending) => pending,
        other => panic!("{other:?}"),
    };

    // A value that is held for a page whose cells do not hold it.
    let mut tree = Tree::new();
    let mut scope = db.scope();
    tree.put(&key(1), &big(1));
    tree.flush(&mut scope);
    let page = tree.at(&key(1)).page();
    let stray = pending_of(scope.values.write(&big(2)).expect("a value"));
    scope.values.placed(stray, page);
    let (logged, attached) = commit(&mut scope);
    let no_cell =
        |error: &ValueError| matches!(error, ValueError::NoCell { page: of } if *of == page);
    let logged = logged.expect_err("no cell");
    assert!(no_cell(&logged), "{logged}");
    assert!(no_cell(attached[0].as_ref().expect_err("no cell")));
    assert_eq!(
        logged.to_string(),
        format!("no cell of page {page} holds a value that is kept for it")
    );
    db.roll_back(scope);

    // A value that two cells of a page hold.
    let mut tree = Tree::new();
    let mut scope = db.scope();
    tree.put(&key(1), &big(1));
    tree.put(&key(2), &big(2));
    tree.flush(&mut scope);
    let (one, two) = (tree.at(&key(1)), tree.at(&key(2)));
    assert_eq!(one.page(), two.page());
    let shared = pending_at(&mut scope, one);
    let longer = PendingValue::new(shared.id(), big(2).len() as u32).expect("a pending value");
    scope
        .pages
        .write(two.page(), |nodes| {
            let mut node = nodes.node_mut(two.slot()).expect("the cell");
            node.set_value(CellValue::Pending(longer))
        })
        .expect("written");
    let (logged, attached) = commit(&mut scope);
    // The second cell states the length of its own value, which the one
    // value cannot have as well.
    let logged = logged.expect_err("a length that is not the value's");
    assert!(matches!(logged, ValueError::NoValue { .. }), "{logged}");
    assert!(attached[0].is_err());
    scope
        .pages
        .write(two.page(), |nodes| {
            let mut node = nodes.node_mut(two.slot()).expect("the cell");
            node.set_value(CellValue::Pending(shared))
        })
        .expect("written");
    let (logged, attached) = commit(&mut scope);
    let logged = logged.expect_err("shared");
    assert!(
        matches!(logged, ValueError::Shared { page } if page == one.page()),
        "{logged}"
    );
    assert!(matches!(attached[0], Err(ValueError::Shared { .. })));
    assert_eq!(
        logged.to_string(),
        format!("two cells of page {} hold one pending value", one.page())
    );
    db.roll_back(scope);
}

/// A carried value is read when it is logged. Before that its bytes are
/// not in memory, and a page is not installed with a value whose bytes are
/// not with it.
#[test]
fn a_carried_value_is_with_its_page_only_once_it_is_logged() {
    let db = Db::new();
    let mut tree = grown(&db, &shuffled(120, 3), 120);
    let mut scope = db.scope();
    for index in 120..200 {
        tree.put(&key(index), b"short");
        tree.flush(&mut scope);
    }
    let carried: Vec<PageId> = scope.values.pages().collect();
    assert!(!carried.is_empty());
    for page in &carried {
        let mut latch = scope.pages.copies[page].frame.write().expect("the latch");
        let refused = scope.values.attach(&mut latch).expect_err("not read yet");
        assert!(
            matches!(refused, ValueError::NoValue { page: of } if of == *page),
            "{refused}"
        );
        assert_eq!(latch.attachments().count(), 0, "the frame is as it was");
    }
    let total: usize = carried
        .iter()
        .map(|page| scope.values.pending_in(*page))
        .sum();
    db.commit(scope);
    assert_eq!(db.groups().pop().expect("a group").values.len(), total);
    check_attached(&db);
    db.check_counts();
}

/// What a savepoint keeps of a scope's values is a clone, and the clone is
/// not changed by what the scope does afterwards.
#[test]
fn a_clone_of_the_values_is_what_a_savepoint_keeps() {
    let pointer = |offset| ValuePointer::new(FileId::MIN, offset, 300).expect("a pointer");
    let mut values = TreeValues::new(InlinePolicy::DEFAULT);
    let (from, to) = (page_id(3), page_id(4));
    values.released(ValuePlace::Placed(pointer(900)), from);
    let first = values.carried(ValuePlace::Placed(pointer(20)), from, to);
    let saved = values.clone();

    values.released(ValuePlace::Pending(first), to);
    values.released(ValuePlace::Placed(pointer(400)), from);
    assert_eq!(values.pending_in(to), 0);
    assert_eq!(
        values.released_pointers(),
        [pointer(20), pointer(400), pointer(900)]
    );

    let mut values = saved;
    assert_eq!(values.pending_in(to), 1);
    assert_eq!(values.released_pointers(), [pointer(20), pointer(900)]);
    // Carried again, the value keeps naming the record it is in, and its
    // pointer is released once.
    let second = values.carried(ValuePlace::Pending(first), to, from);
    assert!(second != first && second.value_len() == 300);
    assert_eq!((values.pending_in(to), values.pending_in(from)), (0, 1));
    assert_eq!(values.released_pointers(), [pointer(20), pointer(900)]);
    values.check().expect("nothing is lost");
}

/// A value of more than one record (`value-log-format.md` V6) is put
/// together by the read.
#[test]
fn a_value_of_several_records_is_read_whole() {
    use lignin_format::value_log::MAX_PART_LEN;

    let db = Db::new();
    let page = page_id(5);
    let value: Vec<u8> = (0..MAX_PART_LEN + 3).map(|at| (at % 251) as u8).collect();
    let mut hold = ValueHold::new();
    let pointer = {
        let mut state = db.state.borrow_mut();
        let pointer = state.value_writer.append(&mut hold, page, &value);
        pointer.expect("appended")
    };
    let mut buf = ValueBuf::new();
    let read = db.source().placed(pointer, page, &mut buf).expect("read");
    assert_eq!(read.len(), value.len());
    assert!(read == value);
}
