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
