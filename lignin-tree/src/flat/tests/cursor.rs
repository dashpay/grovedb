//! The cursor: where it stands, the keys of one prefix, and writes under
//! it.

use super::*;

fn filled(prefixes: &[&[u8]], keys: u32) -> (Mem, FlatTree, BTreeMap<Vec<u8>, Vec<u8>>) {
    let (mut pages, tree) = Mem::tree(small());
    let mut model = BTreeMap::new();
    for prefix in prefixes {
        for n in 0..keys {
            let key = [prefix, &(n * 3).to_be_bytes()[..]].concat();
            let value = bytes(u64::from(n), (n % 90) as usize);
            tree.put(&mut pages, &key, &value).expect("a put");
            model.insert(key, value);
        }
    }
    (pages, tree, model)
}

fn at(cursor: &Cursor, pages: &Mem) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut value = Vec::new();
    assert_eq!(
        cursor.value(pages, &mut value).expect("a value"),
        cursor.valid()
    );
    cursor.key().map(|key| (key.to_vec(), value))
}

#[test]
fn a_cursor_stands_nowhere_until_it_seeks_and_after_it_leaves_an_end() {
    let (mut pages, tree) = Mem::tree(small());
    let mut cursor = tree.cursor();
    assert!(!cursor.valid() && cursor.key().is_none());
    for seek in [Cursor::seek_to_first::<Mem>, Cursor::seek_to_last::<Mem>] {
        seek(&mut cursor, &pages).expect("a seek");
        assert_eq!(at(&cursor, &pages), None);
    }
    cursor.seek(&pages, b"k").expect("a seek");
    cursor.seek_for_prev(&pages, b"k").expect("a seek");
    assert!(!cursor.valid());

    for n in 0..2_000u32 {
        tree.put(&mut pages, &n.to_be_bytes(), &[n as u8])
            .expect("a put");
    }
    assert!(pages.image(tree.root()).level() >= 1);
    let mut cursor = tree.cursor();
    // A step from nowhere goes nowhere.
    cursor.next(&pages).expect("a step");
    cursor.prev(&pages).expect("a step");
    assert_eq!(at(&cursor, &pages), None);

    cursor.seek_to_first(&pages).expect("a seek");
    assert_eq!(
        at(&cursor, &pages),
        Some((0u32.to_be_bytes().to_vec(), vec![0]))
    );
    cursor.prev(&pages).expect("a step");
    assert_eq!(at(&cursor, &pages), None);
    cursor.prev(&pages).expect("a step");
    assert!(!cursor.valid());

    cursor.seek_to_last(&pages).expect("a seek");
    assert_eq!(
        at(&cursor, &pages),
        Some((1_999u32.to_be_bytes().to_vec(), vec![207]))
    );
    cursor.next(&pages).expect("a step");
    assert_eq!(at(&cursor, &pages), None);

    // Every record once, in key order, in both directions.
    cursor.seek_to_first(&pages).expect("a seek");
    for n in 0..2_000u32 {
        assert_eq!(cursor.key(), Some(&n.to_be_bytes()[..]));
        cursor.next(&pages).expect("a step");
    }
    assert!(!cursor.valid());
    cursor.seek_to_last(&pages).expect("a seek");
    for n in (0..2_000u32).rev() {
        assert_eq!(cursor.key(), Some(&n.to_be_bytes()[..]));
        cursor.prev(&pages).expect("a step");
    }
    assert!(!cursor.valid());
}

/// Several keyspaces in one tree, each the keys of one prefix, as the
/// keyspaces of GroveDB's subtrees are (`flat-store.md`).
#[test]
fn a_cursor_within_a_prefix_shows_that_keyspace_and_nothing_else() {
    let prefixes: [&[u8]; 5] = [&[1; 32], &[2; 32], &[2; 33], &[9; 32], &[0xff; 32]];
    let (pages, tree, model) = filled(&prefixes, 300);
    assert!(pages.image(tree.root()).level() >= 1);
    let empty: &[u8] = &[5; 32];
    for prefix in prefixes.into_iter().chain([empty]) {
        let own: Vec<(Vec<u8>, Vec<u8>)> = model
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key[prefix.len()..].to_vec(), value.clone()))
            .collect();
        let mut cursor = tree.cursor_within(prefix);

        cursor.seek_to_first(&pages).expect("a seek");
        let mut found = Vec::new();
        while let Some(record) = at(&cursor, &pages) {
            found.push(record);
            cursor.next(&pages).expect("a step");
        }
        assert!(found == own);

        cursor.seek_to_last(&pages).expect("a seek");
        let mut found = Vec::new();
        while let Some(record) = at(&cursor, &pages) {
            found.push(record);
            cursor.prev(&pages).expect("a step");
        }
        found.reverse();
        assert!(found == own);

        // A seek stays inside the keyspace, wherever its key lies.
        for probe in [
            &b""[..],
            &[0, 0, 0, 4],
            &[0, 0, 0, 6],
            &[0, 0, 3, 0xff],
            &[0xff; 5],
            &[0; 40],
        ] {
            cursor.seek(&pages, probe).expect("a seek");
            let expected = own.iter().find(|(key, _)| key.as_slice() >= probe);
            assert_eq!(at(&cursor, &pages).as_ref(), expected);
            cursor.seek_for_prev(&pages, probe).expect("a seek");
            let expected = own.iter().rfind(|(key, _)| key.as_slice() <= probe);
            assert_eq!(at(&cursor, &pages).as_ref(), expected);
        }
    }
}

/// `flat-store.md`, rule 7: deleting the record a cursor stands on and
/// stepping on is defined. Here every write under a cursor is.
#[test]
fn a_cursor_goes_on_from_its_key_after_a_write() {
    let prefixes: [&[u8]; 3] = [&[1; 32], &[2; 32], &[3; 32]];
    let (mut pages, tree, mut model) = filled(&prefixes, 400);

    // `clear`: the walk that deletes what it stands on empties one
    // keyspace and leaves the others whole, while pages merge and leave
    // beneath it.
    let mut cursor = tree.cursor_within(prefixes[1]);
    cursor.seek_to_first(&pages).expect("a seek");
    let mut cleared = 0u32;
    while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
        assert_eq!(key, (cleared * 3).to_be_bytes());
        let whole = [prefixes[1], &key].concat();
        assert!(tree.delete(&mut pages, &whole).expect("a delete"));
        model.remove(&whole);
        cursor.next(&pages).expect("a step");
        cleared += 1;
    }
    assert_eq!(cleared, 400);
    assert!(records(&pages, &tree) == model.clone().into_iter().collect::<Vec<_>>());

    // The same walk backwards, in another keyspace.
    let mut cursor = tree.cursor_within(prefixes[2]);
    cursor.seek_to_last(&pages).expect("a seek");
    while let Some(key) = cursor.key().map(<[u8]>::to_vec) {
        let whole = [prefixes[2], &key].concat();
        assert!(tree.delete(&mut pages, &whole).expect("a delete"));
        model.remove(&whole);
        cursor.prev(&pages).expect("a step");
    }
    assert_eq!(model.len(), 400);
    assert!(records(&pages, &tree) == model.clone().into_iter().collect::<Vec<_>>());

    // A record that arrives ahead of the cursor is met, one that arrives
    // behind it is not, and one that leaves ahead of it is passed over.
    let key = |n: u32| [prefixes[0], &n.to_be_bytes()[..]].concat();
    let mut cursor = tree.cursor_within(prefixes[0]);
    cursor.seek(&pages, &30u32.to_be_bytes()).expect("a seek");
    tree.put(&mut pages, &key(31), b"ahead").expect("a put");
    tree.put(&mut pages, &key(29), b"behind").expect("a put");
    assert!(tree.delete(&mut pages, &key(33)).expect("a delete"));
    // The record the cursor stood on has another value, and is read anew.
    tree.put(&mut pages, &key(30), b"changed").expect("a put");
    assert_eq!(
        at(&cursor, &pages),
        Some((30u32.to_be_bytes().to_vec(), b"changed".to_vec()))
    );
    cursor.next(&pages).expect("a step");
    assert_eq!(
        at(&cursor, &pages),
        Some((31u32.to_be_bytes().to_vec(), b"ahead".to_vec()))
    );
    cursor.next(&pages).expect("a step");
    assert_eq!(cursor.key(), Some(&36u32.to_be_bytes()[..]));
    cursor.prev(&pages).expect("a step");
    tree.put(&mut pages, &key(32), b"between").expect("a put");
    cursor.prev(&pages).expect("a step");
    assert_eq!(cursor.key(), Some(&30u32.to_be_bytes()[..]));

    // A cursor whose record was removed has no value, and still steps.
    assert!(tree.delete(&mut pages, &key(30)).expect("a delete"));
    let mut value = Vec::new();
    assert!(!cursor.value(&pages, &mut value).expect("a read") && cursor.valid());
    cursor.prev(&pages).expect("a step");
    assert_eq!(
        at(&cursor, &pages),
        Some((29u32.to_be_bytes().to_vec(), b"behind".to_vec()))
    );
}

/// What GroveDB's drain of the pending prefix drops asks of the flat
/// keyspace (`flat-store.md`): the records in ascending order of their
/// prefix, a range delete for each doomed prefix, the delete of the
/// record, and every step durable in the order it was made.
#[test]
fn the_steps_of_a_drain_are_in_the_log_in_the_order_they_were_made() {
    let (mut pages, tree) = Mem::tree(small());
    let namespace = [0xd0; 32];
    let doomed: Vec<[u8; 32]> = (1..=6).map(|n| [n * 16; 32]).collect();
    let live = [0x77; 32];
    // The records, staged out of order, and the keyspaces they name. A
    // record's key is the prefix it dooms, and its value names a second.
    for (n, prefix) in doomed.iter().enumerate().rev() {
        if n % 2 == 0 {
            let record = [namespace, *prefix].concat();
            tree.put(&mut pages, &record, &doomed[n + 1])
                .expect("a put");
        }
        for key in 0..150u32 {
            let key = [&prefix[..], &key.to_be_bytes()].concat();
            tree.put(&mut pages, &key, &[n as u8; 20]).expect("a put");
        }
    }
    for key in 0..150u32 {
        let key = [&live[..], &key.to_be_bytes()].concat();
        tree.put(&mut pages, &key, b"stays").expect("a put");
    }
    pages.commit();
    let before = pages.log.len();

    // The drain: each step is a commit of its own.
    let mut listed = Vec::new();
    let mut cursor = tree.cursor_within(&namespace);
    cursor.seek_to_first(&pages).expect("a seek");
    while let Some(record) = at(&cursor, &pages) {
        listed.push(record);
        cursor.next(&pages).expect("a step");
    }
    let expected: Vec<_> = (0..3)
        .map(|n| (doomed[2 * n].to_vec(), doomed[2 * n + 1].to_vec()))
        .collect();
    assert_eq!(
        listed, expected,
        "the records, in ascending order of prefix"
    );
    for (primary, second) in &listed {
        for prefix in [primary, second] {
            tree.delete_prefix(&mut pages, prefix)
                .expect("a range delete");
            pages.commit();
        }
        // Deleting a range that is empty changes nothing.
        tree.delete_prefix(&mut pages, primary)
            .expect("a range delete");
        assert_eq!(pages.commit(), (Vec::new(), 0));
        let record = [&namespace[..], primary].concat();
        assert!(tree.delete(&mut pages, &record).expect("a delete"));
        pages.commit();
    }
    assert_eq!(records(&pages, &tree).len(), 150);

    // A crash keeps the commits up to some point. Whatever that point is,
    // a record is gone only if both of its ranges are, and a range of a
    // later record only if every earlier record is gone.
    let count = |pages: &Mem, prefix: &[u8]| {
        let mut cursor = tree.cursor_within(prefix);
        cursor.seek_to_first(pages).expect("a seek");
        std::iter::from_fn(|| cursor.valid().then(|| cursor.next(pages).expect("a step"))).count()
    };
    let mut states = Vec::new();
    for commits in before..=pages.log.len() {
        let recovered = pages.replay(commits);
        assert_eq!(count(&recovered, &live), 150);
        let state: Vec<(bool, usize, usize)> = (0..3)
            .map(|n| {
                let record = [&namespace[..], &doomed[2 * n]].concat();
                let listed = get(&recovered, &tree, &record).is_some();
                let ranges = (
                    count(&recovered, &doomed[2 * n]),
                    count(&recovered, &doomed[2 * n + 1]),
                );
                (listed, ranges.0, ranges.1)
            })
            .collect();
        for (n, &(listed, first, second)) in state.iter().enumerate() {
            assert!(
                listed || (first, second) == (0, 0),
                "a record gone before its ranges"
            );
            let earlier_done = state[..n].iter().all(|&(listed, ..)| !listed);
            assert!(earlier_done || (listed, first, second) == (true, 150, 150));
        }
        states.push(state);
    }
    assert_eq!(states.first().expect("a state")[0], (true, 150, 150));
    assert_eq!(states.last().expect("a state")[2], (false, 0, 0));
    states.dedup();
    assert_eq!(states.len(), 10, "nine steps, each of which is seen alone");
}

/// A step of background work takes an entry out of a leaf under a reader,
/// whose pages are of the same version before and after
/// (`transactions.md` PV7). The cursor shows no other record under the
/// key it stands on, and goes on from that key.
#[test]
fn a_cursor_reads_by_its_key_where_an_entry_left_its_leaf() {
    use lignin_format::flat_page::FlatLeafBuilder;

    let key = |n: u8| vec![b'k', n];
    let value = |n: u8| vec![n; 3 + n as usize];
    let (mut pages, tree) = Mem::tree(small());
    for n in 0..10 {
        tree.put(&mut pages, &key(n), &value(n)).expect("a put");
    }
    pages.commit();
    let root = tree.root();
    let revision = pages.revision();
    let take_out = |pages: &mut Mem, gone: u8| {
        let leaf = pages.image(root).leaf().expect("a leaf");
        let mut built = FlatLeafBuilder::new(root, leaf.prefix(), 4_096).expect("a leaf");
        for (found, value) in leaf.iter().filter(|(found, _)| found.to_vec() != key(gone)) {
            built.push_key(found, value).expect("an entry");
        }
        let image = built.finish();
        pages.pages.insert(root, image.clone());
        pages.base.insert(root, image);
        assert_eq!(pages.revision(), revision);
    };
    let read = |cursor: &Cursor, pages: &Mem| {
        let mut out = Vec::new();
        let found = cursor.value(pages, &mut out).expect("a value");
        (cursor.key().map(<[u8]>::to_vec), found.then_some(out))
    };

    // An entry before the cursor's leaves: every entry behind it stands
    // one position earlier, and the cursor's record is read all the same.
    let mut cursor = tree.cursor();
    cursor.seek(&pages, &key(5)).expect("a seek");
    take_out(&mut pages, 1);
    assert_eq!(read(&cursor, &pages), (Some(key(5)), Some(value(5))));
    cursor.next(&pages).expect("a step");
    assert_eq!(read(&cursor, &pages), (Some(key(6)), Some(value(6))));
    take_out(&mut pages, 2);
    cursor.prev(&pages).expect("a step");
    assert_eq!(read(&cursor, &pages), (Some(key(5)), Some(value(5))));

    // The cursor's own entry leaves: it has no value any more, and the
    // cursor goes on to the records beside its key.
    take_out(&mut pages, 5);
    assert_eq!(read(&cursor, &pages), (Some(key(5)), None));
    let mut back = cursor.clone();
    cursor.next(&pages).expect("a step");
    assert_eq!(read(&cursor, &pages), (Some(key(6)), Some(value(6))));
    back.prev(&pages).expect("a step");
    assert_eq!(read(&back, &pages), (Some(key(4)), Some(value(4))));

    // The last entry leaves under a cursor that stands on it.
    cursor.seek_to_last(&pages).expect("a seek");
    take_out(&mut pages, 9);
    let mut back = cursor.clone();
    cursor.next(&pages).expect("a step");
    assert_eq!(read(&cursor, &pages), (None, None));
    back.prev(&pages).expect("a step");
    assert_eq!(read(&back, &pages), (Some(key(8)), Some(value(8))));
}

/// The records that a walk shows, and how many of them it is told to stop
/// at: none with `None`.
fn walked(
    cursor: &mut Cursor,
    pages: &impl FlatPages,
    back: bool,
    stop_at: Option<usize>,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut shown = Vec::new();
    let walk = cursor.walk(pages, back, |key, value| {
        let Some(value) = value else {
            return Look::Value;
        };
        shown.push((key.to_vec(), value.to_vec()));
        match Some(shown.len()) == stop_at {
            true => Look::Stop,
            false => Look::Pass,
        }
    });
    walk.expect("a walk");
    shown
}

/// A walk shows the records that the cursor's steps show, each once and
/// in their order, the one it starts on first, and leaves the cursor on
/// the record it is told to stop at, or nowhere: within a keyspace and
/// over all of them, both ways, over values in their pages and out of
/// line, and over keys that are in the trees of buckets.
#[test]
fn a_walk_shows_what_the_steps_show_and_stops_where_it_is_told() {
    let prefixes: [&[u8]; 4] = [&[1; 32], &[2; 32], &[2; 33], &[0xff; 32]];
    let (mut pages, tree, mut model) = filled(&prefixes, 120);
    let config = small();
    let mut put = |key: Vec<u8>, value: Vec<u8>| {
        tree.put(&mut pages, &key, &value).expect("a put");
        model.insert(key, value);
    };
    // Five buckets of eight long keys in the second keyspace, every third
    // with a value out of line, and values out of line under short keys
    // of the first.
    let (mut long, mut out_of_line) = (0, 0);
    for n in 0..40u8 {
        let mut key = vec![2; 32];
        key.extend(vec![b'h' + n / 8; config.key_limit()]);
        key.push(n);
        assert!(key.len() > config.key_limit() + 32);
        let len = if n % 3 == 0 {
            config.max_inline() + 40
        } else {
            9
        };
        out_of_line += usize::from(len > config.max_inline());
        long += 1;
        put(key, bytes(u64::from(n), len));
    }
    for n in (0..120u32).step_by(7) {
        let key = [&[1; 32][..], &(n * 3).to_be_bytes()].concat();
        out_of_line += 1;
        put(
            key,
            bytes(u64::from(n), config.max_inline() + 1 + n as usize),
        );
    }
    assert_eq!((long, out_of_line), (40, 14 + 18));
    assert!(pages.image(tree.root()).level() >= 1);

    let (empty, all): (&[u8], &[u8]) = (&[5; 32], &[]);
    let mut walks = 0;
    for prefix in prefixes.into_iter().chain([empty, all]) {
        let own: Vec<(Vec<u8>, Vec<u8>)> = model
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key[prefix.len()..].to_vec(), value.clone()))
            .collect();
        for back in [false, true] {
            let mut in_order = own.clone();
            if back {
                in_order.reverse();
            }
            for start in [0, in_order.len() / 3, in_order.len().saturating_sub(1)] {
                let rest = in_order.get(start..).unwrap_or_default();
                let stops = [Some(1), Some(2), Some(17), Some(rest.len()), None];
                for stop_at in stops.into_iter().filter(|stop_at| *stop_at != Some(0)) {
                    let mut cursor = tree.cursor_within(prefix);
                    match rest.first() {
                        Some((key, _)) => cursor.seek(&pages, key).expect("a seek"),
                        None => cursor.seek_to_first(&pages).expect("a seek"),
                    }
                    let shown = walked(&mut cursor, &pages, back, stop_at);
                    let until = stop_at.map_or(rest.len(), |stop_at| stop_at.min(rest.len()));
                    assert!(
                        shown == rest[..until],
                        "{prefix:?} {back} {start} {stop_at:?}"
                    );
                    // The cursor stands on the record it stopped at, and a
                    // step goes on from there.
                    let stands = stop_at.and_then(|stop_at| rest.get(stop_at - 1));
                    assert_eq!(at(&cursor, &pages).as_ref(), stands);
                    if let Some(stop_at) = stop_at.filter(|_| stands.is_some()) {
                        match back {
                            false => cursor.next(&pages).expect("a step"),
                            true => cursor.prev(&pages).expect("a step"),
                        }
                        assert_eq!(at(&cursor, &pages).as_ref(), rest.get(stop_at));
                    }
                    walks += 1;
                }
            }
        }
    }
    // Every keyspace but the empty one was walked from three of its
    // records, both ways.
    assert!(walks >= 5 * 2 * 3 * 4, "{walks}");
}

/// Pages that count how often a page is asked for and how often a value
/// out of line, and that can refuse every value out of line.
struct Counted<'a> {
    pages: &'a Mem,
    asked: std::cell::Cell<u64>,
    values: std::cell::Cell<u64>,
    no_values: std::cell::Cell<bool>,
}

impl<'a> Counted<'a> {
    fn new(pages: &'a Mem) -> Self {
        Self {
            pages,
            asked: std::cell::Cell::new(0),
            values: std::cell::Cell::new(0),
            no_values: std::cell::Cell::new(false),
        }
    }
}

impl FlatPages for Counted<'_> {
    type Page<'p>
        = &'p [u8]
    where
        Self: 'p;

    fn page(&self, page: PageId) -> Result<&[u8], FlatError> {
        self.asked.set(self.asked.get() + 1);
        self.pages.page(page)
    }

    fn read_value(
        &self,
        held: &&[u8],
        page: PageId,
        slot: usize,
        place: ValuePlace,
        out: &mut Vec<u8>,
    ) -> Result<(), FlatError> {
        self.values.set(self.values.get() + 1);
        if self.no_values.get() {
            return Err(FlatError::NoValue { page });
        }
        self.pages.read_value(held, page, slot, place, out)
    }

    fn revision(&self) -> Revision {
        self.pages.revision()
    }
}

/// How many leaves the tree below `page` has.
fn leaves_below(pages: &Mem, page: PageId) -> u64 {
    match pages.image(page) {
        FlatPage::Leaf(_) => 1,
        FlatPage::Branch(branch) => (0..=branch.len())
            .map(|index| leaves_below(pages, branch.child(index).expect("a child")))
            .sum(),
    }
}

/// A walk holds a leaf across the records it shows of it: it asks for
/// each leaf once for its records and once more to step out of it, and
/// for the branches on the way to the next, where the cursor's steps ask
/// for the leaf of a record twice for each record.
#[test]
fn a_walk_asks_for_a_leaf_once_for_the_records_it_shows_of_it() {
    let (mut pages, tree) = Mem::tree(small());
    let records = 20_000u64;
    for n in 0..records as u32 {
        tree.put(&mut pages, &n.to_be_bytes(), &[n as u8; 2])
            .expect("a put");
    }
    let levels = u64::from(pages.image(tree.root()).level()) + 1;
    let leaves = leaves_below(&pages, tree.root());
    assert!(
        levels >= 2 && leaves >= 20,
        "{levels} levels, {leaves} leaves"
    );
    let counted = Counted::new(&pages);
    for back in [false, true] {
        let to_the_edge = |cursor: &mut Cursor| match back {
            false => cursor.seek_to_first(&counted).expect("a seek"),
            true => cursor.seek_to_last(&counted).expect("a seek"),
        };
        let mut cursor = tree.cursor();
        to_the_edge(&mut cursor);
        counted.asked.set(0);
        let mut shown = 0;
        let walk = cursor.walk(&counted, back, |_, value| {
            let Some(value) = value else {
                return Look::Value;
            };
            shown += 1;
            assert_eq!(value.len(), 2);
            Look::Pass
        });
        walk.expect("a walk");
        let by_walk = counted.asked.replace(0);
        assert_eq!(shown, records);
        assert!(!cursor.valid());

        to_the_edge(&mut cursor);
        counted.asked.set(0);
        let mut value = Vec::new();
        while cursor.valid() {
            assert!(cursor.value(&counted, &mut value).expect("a value"));
            match back {
                false => cursor.next(&counted).expect("a step"),
                true => cursor.prev(&counted).expect("a step"),
            }
        }
        let by_steps = counted.asked.replace(0);
        assert!(by_steps >= 2 * records, "{by_steps} by steps");
        assert!(
            by_walk >= leaves && by_walk <= 2 * levels * leaves,
            "{by_walk} by the walk, {leaves} leaves of {levels} levels"
        );
        assert!(by_walk * 100 < by_steps, "{by_walk} against {by_steps}");
    }
}

/// A walk whose leaf does not show the record the cursor stands on shows
/// what the cursor's steps show: where an entry left the leaf under a
/// reader, and where a write changed the pages since the cursor moved. A
/// record that is gone is shown by its key and has no value, and a walk
/// that passes it goes on from its key.
#[test]
fn a_walk_goes_on_from_its_key_where_its_leaf_does_not_show_its_record() {
    use lignin_format::flat_page::FlatLeafBuilder;

    let key = |n: u8| vec![b'k', n];
    let value = |n: u8| vec![n; 3 + n as usize];
    let (mut pages, tree) = Mem::tree(small());
    for n in 0..10 {
        tree.put(&mut pages, &key(n), &value(n)).expect("a put");
    }
    pages.commit();
    let root = tree.root();
    let revision = pages.revision();
    let take_out = |pages: &mut Mem, gone: u8| {
        let leaf = pages.image(root).leaf().expect("a leaf");
        let mut built = FlatLeafBuilder::new(root, leaf.prefix(), 4_096).expect("a leaf");
        for (found, value) in leaf.iter().filter(|(found, _)| found.to_vec() != key(gone)) {
            built.push_key(found, value).expect("an entry");
        }
        let image = built.finish();
        pages.pages.insert(root, image.clone());
        pages.base.insert(root, image);
        assert_eq!(pages.revision(), revision);
    };
    let shown = |cursor: &Cursor, pages: &Mem, back: bool| {
        let mut cursor = cursor.clone();
        let mut keys = Vec::new();
        let walk = cursor.walk(pages, back, |found, read| {
            let Some(read) = read else {
                return Look::Value;
            };
            assert_eq!((found[0], read), (b'k', &value(found[1])[..]));
            keys.push(found[1]);
            Look::Pass
        });
        walk.expect("a walk");
        assert!(!cursor.valid());
        keys
    };

    // An entry before the cursor's leaves under a reader: the cursor's
    // entry stands one position earlier, and is shown all the same.
    let mut cursor = tree.cursor();
    cursor.seek(&pages, &key(5)).expect("a seek");
    take_out(&mut pages, 1);
    assert_eq!(shown(&cursor, &pages, false), [5, 6, 7, 8, 9]);
    assert_eq!(shown(&cursor, &pages, true), [5, 4, 3, 2, 0]);
    // The cursor's own entry leaves: its value is not shown.
    take_out(&mut pages, 5);
    assert_eq!(shown(&cursor, &pages, false), [6, 7, 8, 9]);
    assert_eq!(shown(&cursor, &pages, true), [4, 3, 2, 0]);
    // Its key is, as the cursor shows it, and a walk that stops there
    // leaves the cursor where it was.
    let mut stays = cursor.clone();
    let walk = stays.walk(&pages, false, |found, read| {
        assert_eq!((found, read), (&key(5)[..], None));
        Look::Stop
    });
    walk.expect("a walk");
    let mut out = Vec::new();
    assert!(!stays.value(&pages, &mut out).expect("a read"));
    assert_eq!(stays.key(), Some(&key(5)[..]));
    // A walk that stops leaves the cursor on a record of the leaf as it
    // is, and the next walk shows that record first.
    let walk = cursor.walk(&pages, false, |found, _| match found[1] == 8 {
        true => Look::Stop,
        false => Look::Pass,
    });
    walk.expect("a walk");
    assert_eq!(cursor.key(), Some(&key(8)[..]));
    take_out(&mut pages, 7);
    assert_eq!(shown(&cursor, &pages, true), [8, 6, 4, 3, 2, 0]);

    // Writes since the cursor moved: the record it stands on is shown by
    // its key if it is still there, and the records beside the key are
    // those of the pages as they are.
    assert_eq!(pages.revision(), revision);
    tree.put(&mut pages, &key(7), &value(7)).expect("a put");
    assert!(tree.delete(&mut pages, &key(9)).expect("a delete"));
    assert_ne!(pages.revision(), revision);
    assert_eq!(shown(&cursor, &pages, false), [8]);
    assert_eq!(shown(&cursor, &pages, true), [8, 7, 6, 4, 3, 2, 0]);
    assert!(tree.delete(&mut pages, &key(8)).expect("a delete"));
    assert_eq!(shown(&cursor, &pages, false), [0u8; 0]);
    assert_eq!(shown(&cursor, &pages, true), [7, 6, 4, 3, 2, 0]);
}

/// A cursor that stands nowhere is shown nothing by a walk, and a walk
/// that fails leaves no position.
#[test]
fn a_walk_from_nowhere_shows_nothing_and_one_that_fails_leaves_no_position() {
    let (mut pages, tree, model) = filled(&[&[1; 4]], 3_000);
    let mut cursor = tree.cursor();
    let walk = cursor.walk(&pages, false, |_, _| -> Look {
        panic!("a record from nowhere")
    });
    walk.expect("a walk");
    assert!(!cursor.valid());

    // The second leaf of the tree is not among the pages any more.
    let mut gone = match pages.image(tree.root()) {
        FlatPage::Branch(branch) => branch.child(1).expect("a child"),
        FlatPage::Leaf(_) => panic!("a tree of one leaf"),
    };
    while let FlatPage::Branch(branch) = pages.image(gone) {
        gone = branch.child(0).expect("a child");
    }
    pages.pages.remove(&gone);
    cursor.seek_to_first(&pages).expect("a seek");
    let mut shown = 0;
    let failed = cursor.walk(&pages, false, |_, _| {
        shown += 1;
        Look::Pass
    });
    assert!(
        matches!(failed, Err(FlatError::Freed { page }) if page == gone),
        "{failed:?}"
    );
    assert!(shown > 0 && shown < model.len(), "{shown} records shown");
    assert!(!cursor.valid());
    assert_eq!(cursor.key(), None);
}

/// A walk reads a value only where its caller asks for it: records are
/// passed and stopped at by their keys with no value read, a value out of
/// line that cannot be read fails the walk only where it is asked for,
/// and `Look::Value` for a record that is shown with its value passes
/// the record.
#[test]
fn a_walk_reads_a_value_only_where_it_is_asked_for() {
    let key = |n: u8| vec![b'k', n];
    let long = small().max_inline() + 9;
    let value = |n: u8| vec![n; if n.is_multiple_of(3) { long } else { 5 }];
    let (mut pages, tree) = Mem::tree(small());
    for n in 0..10 {
        tree.put(&mut pages, &key(n), &value(n)).expect("a put");
    }
    let counted = Counted::new(&pages);
    for back in [false, true] {
        let order: Vec<u8> = match back {
            false => (0..10).collect(),
            true => (0..10).rev().collect(),
        };
        let from = |cursor: &mut Cursor| cursor.seek(&counted, &key(order[0])).expect("a seek");

        // By the keys alone, with no value to be had.
        counted.no_values.set(true);
        counted.values.set(0);
        let mut cursor = tree.cursor();
        from(&mut cursor);
        let mut keys = Vec::new();
        let walk = cursor.walk(&counted, back, |found, read| {
            assert_eq!(read, None);
            keys.push(found[1]);
            match found[1] == order[7] {
                true => Look::Stop,
                false => Look::Pass,
            }
        });
        walk.expect("a walk");
        assert_eq!(keys, order[..8]);
        assert_eq!(cursor.key(), Some(&key(order[7])[..]));
        assert_eq!(counted.values.get(), 0);

        // A value in its leaf is shown with no read of a value, and one
        // out of line that cannot be read fails the walk where it is
        // asked for.
        from(&mut cursor);
        let mut inline = Vec::new();
        let walk = cursor.walk(&counted, back, |found, read| match read {
            None if found[1].is_multiple_of(3) && found[1] != order[9] => Look::Pass,
            None => Look::Value,
            Some(read) => {
                inline.push((found[1], read.len()));
                Look::Pass
            }
        });
        // The last record either way has its value out of line.
        assert!(order[9].is_multiple_of(3));
        assert!(matches!(walk, Err(FlatError::NoValue { .. })), "{walk:?}");
        assert_eq!(counted.values.get(), 1);
        assert!(!cursor.valid());
        let in_leaf = order.iter().filter(|n| !n.is_multiple_of(3));
        assert!(inline == in_leaf.map(|n| (*n, 5)).collect::<Vec<_>>());

        // Every value, and `Value` said again of a record that is shown
        // with its value: the record is passed.
        counted.no_values.set(false);
        counted.values.set(0);
        from(&mut cursor);
        let mut shown = Vec::new();
        let walk = cursor.walk(&counted, back, |found, read| {
            shown.push((found[1], read.map(<[u8]>::to_vec)));
            Look::Value
        });
        walk.expect("a walk");
        assert!(!cursor.valid());
        let expected = (order.iter()).flat_map(|n| [(*n, None), (*n, Some(value(*n)))]);
        assert!(shown == expected.collect::<Vec<_>>());
        assert_eq!(counted.values.get(), 4);
    }
}

/// The cursor stands where a walk found it for as long as the walk's
/// caller runs: after a caller that panicked, at a key or at a value, the
/// cursor names the record it stood on, and reads it.
#[test]
fn a_walk_keeps_the_cursors_place_while_its_caller_runs() {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    let (pages, tree, model) = filled(&[&[4; 3]], 50);
    let records: Vec<(Vec<u8>, Vec<u8>)> = model.into_iter().collect();
    for at_value in [false, true] {
        for back in [false, true] {
            let mut cursor = tree.cursor();
            cursor.seek(&pages, &records[20].0).expect("a seek");
            let mut calls = 0;
            let walked = catch_unwind(AssertUnwindSafe(|| {
                cursor.walk(&pages, back, |_, read| {
                    calls += 1;
                    // The third record that is shown, by its key or by
                    // its value.
                    match (read.is_some(), at_value, calls) {
                        (false, false, 3) | (true, true, 6) => panic!("the caller gives up"),
                        (false, true, _) => Look::Value,
                        _ => Look::Pass,
                    }
                })
            }));
            assert!(walked.is_err());
            assert_eq!(at(&cursor, &pages), Some(records[20].clone()));
            // And goes on from there.
            match back {
                false => cursor.next(&pages).expect("a step"),
                true => cursor.prev(&pages).expect("a step"),
            }
            let beside = if back { 19 } else { 21 };
            assert_eq!(at(&cursor, &pages), Some(records[beside].clone()));
        }
    }
}
