//! Keys of every length, the longest among them longer than a page.

use super::*;

type Records = BTreeMap<Vec<u8>, Vec<u8>>;

/// The acceptance test of the plan's draft 10: a key longer than one page
/// holds is stored and read back unchanged, so no key length that works on
/// RocksDB fails here.
#[test]
fn a_key_longer_than_a_page_is_stored_and_read_back_unchanged() {
    for size in [8_192, 16_384, 32_768, 65_536] {
        let config = FlatConfig::new(PageSize::new(size).expect("a page size"));
        let (mut pages, tree) = Mem::tree(config);
        let limit = config.key_limit();
        // Around the longest key a page stores, around a page, several
        // pages, and 8 MiB, the longest key RocksDB takes.
        let mut lens = vec![0, 1, limit - 1, limit, limit + 1, limit + 2, 2 * limit];
        lens.extend([size - 1, size, size + 1, 3 * size + 7, 300_000, 8 << 20]);
        let records: BTreeMap<Vec<u8>, Vec<u8>> = lens
            .iter()
            .enumerate()
            .map(|(n, &len)| (bytes(n as u64, len), bytes(1_000 + n as u64, 10 * n)))
            .collect();
        for (key, value) in &records {
            tree.put(&mut pages, key, value).expect("a put");
        }
        pages.commit();
        pages.place_values();

        for (key, value) in &records {
            assert_eq!(
                get(&pages, &tree, key).as_ref(),
                Some(value),
                "a key of {} bytes",
                key.len()
            );
        }
        let read: BTreeMap<_, _> = super::records(&pages, &tree).into_iter().collect();
        assert!(read == records);

        // A cursor gives the keys back whole, and in the order of their
        // bytes.
        let mut cursor = tree.cursor();
        cursor.seek_to_first(&pages).expect("a seek");
        for (key, value) in &records {
            let mut found = Vec::new();
            assert!(cursor.value(&pages, &mut found).expect("a value"));
            assert!(cursor.key() == Some(key.as_slice()) && &found == value);
            cursor.next(&pages).expect("a step");
        }
        assert!(!cursor.valid());

        // A key that differs from the longest one in its last byte is
        // another.
        let long = records.keys().max_by_key(|key| key.len()).expect("a key");
        assert_eq!(long.len(), 8 << 20);
        let mut other = long.clone();
        *other.last_mut().expect("a byte") ^= 1;
        assert_eq!(get(&pages, &tree, &other), None);
        assert!(!tree.delete(&mut pages, &other).expect("a delete"));

        for key in records.keys() {
            assert!(tree.delete(&mut pages, key).expect("a delete"));
            assert_eq!(get(&pages, &tree, key), None);
        }
        assert!(super::records(&pages, &tree).is_empty());
        assert!(pages.live.is_empty() && pages.pending.is_empty());
    }
}

/// Long keys that begin alike are told apart, replaced and removed one by
/// one, and stand among the short keys where their bytes put them.
#[test]
fn long_keys_with_one_head_are_kept_apart_and_in_order() {
    let config = small();
    let (mut pages, tree) = Mem::tree(config);
    let limit = config.key_limit();
    let head = vec![b'h'; limit];
    let long = |rest: &[u8]| [head.as_slice(), rest].concat();
    let mut model = BTreeMap::new();
    let mut put = |pages: &mut Mem, key: Vec<u8>, value: &[u8]| {
        tree.put(pages, &key, value).expect("a put");
        model.insert(key, value.to_vec());
    };
    // The head itself is a short key, and so are its neighbours.
    put(&mut pages, head.clone(), b"the head");
    put(&mut pages, head[..limit - 1].to_vec(), b"shorter");
    put(
        &mut pages,
        [&head[..limit - 1], b"i"].concat(),
        b"after every long key",
    );
    put(
        &mut pages,
        [&head[..limit - 1], b"g"].concat(),
        b"before the head",
    );
    for rest in [&b"b"[..], b"a", b"ab", b"\0", b"\0\0", b"\xff", b"c"] {
        put(&mut pages, long(rest), rest);
    }
    put(
        &mut pages,
        long(&vec![b'z'; 3 * config.capacity()]),
        &vec![7; 5_000],
    );
    put(&mut pages, long(b"ab"), b"replaced");
    assert!(super::records(&pages, &tree) == model.clone().into_iter().collect::<Vec<_>>());

    // The long keys are one entry of the tree: the bucket of their head,
    // which names the tree they are in.
    let leaf = pages.image(tree.root()).leaf().expect("one page");
    assert_eq!(leaf.len(), 5);
    assert_eq!(
        leaf.key(3).expect("the bucket").to_vec(),
        [head.as_slice(), &[0]].concat()
    );
    assert!(matches!(leaf.value(3), Some(FlatValue::Inline(root)) if root.len() == 4));

    // Seeks land between long keys, at them, and at the bucket's ends.
    let mut cursor = tree.cursor();
    for probe in [
        &b""[..],
        b"\0",
        b"\0\0\0",
        b"aa",
        b"ab",
        b"b\0",
        b"\xff\xff",
        b"zzzz",
    ] {
        let probe = long(probe);
        cursor.seek(&pages, &probe).expect("a seek");
        assert_eq!(
            cursor.key(),
            model
                .range(probe.clone()..)
                .next()
                .map(|(k, _)| k.as_slice())
        );
        cursor.seek_for_prev(&pages, &probe).expect("a seek");
        assert_eq!(
            cursor.key(),
            model.range(..=probe).next_back().map(|(k, _)| k.as_slice())
        );
    }

    // A range delete cuts into the bucket from either side, and through it.
    let mut delete = |pages: &mut Mem, start: &[u8], end: Option<&[u8]>| {
        tree.delete_range(pages, start, end)
            .expect("a range delete");
        model.retain(|key, _| {
            key.as_slice() < start || end.is_some_and(|end| key.as_slice() >= end)
        });
        assert!(super::records(pages, &tree) == model.clone().into_iter().collect::<Vec<_>>());
    };
    delete(&mut pages, &long(b"a"), Some(&long(b"ab")));
    delete(
        &mut pages,
        &long(b"c"),
        Some(&[&head[..limit - 1], b"i"].concat()),
    );
    delete(&mut pages, &head[..limit - 1], Some(&long(b"\0\0")));
    delete(&mut pages, &long(b"\0\0"), Some(&long(b"\0\0")));
    delete(&mut pages, &long(b"b"), None);
    assert_eq!(model.len(), 2);
    assert!(tree.delete(&mut pages, &long(b"\0\0")).expect("a delete"));
    assert!(tree.delete(&mut pages, &long(b"ab")).expect("a delete"));
    assert!(!tree.delete(&mut pages, &long(b"ab")).expect("a delete"));
    assert!(super::records(&pages, &tree).is_empty());
}

#[test]
fn every_key_length_around_the_limit_is_its_own_key() {
    let config = small();
    let (mut pages, tree) = Mem::tree(config);
    let limit = config.key_limit();
    let keys: Vec<Vec<u8>> = (limit - 3..=limit + 3).map(|len| vec![b'q'; len]).collect();
    for (n, key) in keys.iter().enumerate() {
        tree.put(&mut pages, key, &[n as u8]).expect("a put");
    }
    for (n, key) in keys.iter().enumerate() {
        assert_eq!(get(&pages, &tree, key), Some(vec![n as u8]));
    }
    let found: Vec<Vec<u8>> = super::records(&pages, &tree)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert_eq!(found, keys);
    // The empty key is a key, and the first one.
    tree.put(&mut pages, b"", b"empty").expect("a put");
    assert_eq!(get(&pages, &tree, b""), Some(b"empty".to_vec()));
    let mut cursor = tree.cursor();
    cursor.seek_to_first(&pages).expect("a seek");
    assert_eq!(cursor.key(), Some(&b""[..]));
}

/// The long keys with one head are a tree of their own, which grows as any
/// other: there are as many of them, with values as long, as the keyspace
/// has room for, and the tree they are in holds one entry for all of them.
#[test]
fn the_long_keys_of_one_head_are_a_tree_that_grows_and_shrinks() {
    let config = small();
    let (mut pages, tree) = Mem::tree(config);
    let limit = config.key_limit();
    let head = vec![b'h'; limit];
    // Rests of every kind: short ones, and long ones that share a head of
    // their own, which is a bucket in the bucket's tree.
    let long = |n: u32| {
        let rest = match n % 3 {
            0 => n.to_be_bytes().to_vec(),
            1 => [vec![b'r'; limit], n.to_be_bytes().to_vec()].concat(),
            _ => [vec![b's'; 2 * limit + 5], n.to_be_bytes().to_vec()].concat(),
        };
        [head.as_slice(), &rest].concat()
    };
    let mut model = Records::new();
    for n in 0..1_500u32 {
        let value = bytes(u64::from(n), (n as usize * 37) % (3 * config.max_inline()));
        tree.put(&mut pages, &long(n), &value).expect("a put");
        model.insert(long(n), value);
    }
    tree.put(&mut pages, b"a", b"before").expect("a put");
    tree.put(&mut pages, b"z", b"after").expect("a put");
    model.insert(b"a".to_vec(), b"before".to_vec());
    model.insert(b"z".to_vec(), b"after".to_vec());
    pages.commit();
    pages.place_values();
    let expected =
        |model: &Records| -> Vec<_> { model.iter().map(|(k, v)| (k.clone(), v.clone())).collect() };
    assert!(records(&pages, &tree) == expected(&model));

    let root = pages.image(tree.root()).leaf().expect("one page");
    assert_eq!(root.len(), 3);
    let Some(FlatValue::Inline(inner)) = root.value(1) else {
        panic!("a bucket names its tree in the page");
    };
    let inner = PageId::decode(inner.try_into().expect("four bytes")).expect("a page");
    assert!(pages.image(inner).level() >= 1 && pages.pages.len() > 30);
    let held: usize = model.values().map(Vec::len).sum();
    assert!(held > 100 * config.capacity());

    // A cursor goes through the bucket's tree and the buckets in it, in
    // both directions, and from a seek into the middle of each.
    let mut cursor = tree.cursor();
    cursor.seek_to_first(&pages).expect("a seek");
    for (key, value) in &model {
        let mut found = Vec::new();
        assert!(cursor.value(&pages, &mut found).expect("a value"));
        assert!(cursor.key() == Some(key.as_slice()) && &found == value);
        cursor.next(&pages).expect("a step");
    }
    assert!(!cursor.valid());
    cursor.seek_to_last(&pages).expect("a seek");
    for key in model.keys().rev() {
        assert!(cursor.key() == Some(key.as_slice()));
        cursor.prev(&pages).expect("a step");
    }
    assert!(!cursor.valid());
    for n in (0..1_500).step_by(7) {
        let mut probe = long(n);
        probe.push(0);
        cursor.seek(&pages, &probe).expect("a seek");
        let after = model
            .range(probe.clone()..)
            .next()
            .map(|(k, _)| k.as_slice());
        assert_eq!(cursor.key(), after);
        cursor.seek_for_prev(&pages, &probe).expect("a seek");
        assert_eq!(cursor.key(), Some(long(n).as_slice()));
    }
    let mut within = tree.cursor_within(&[head.as_slice(), &vec![b'r'; limit]].concat());
    within.seek_to_first(&pages).expect("a seek");
    let mut seen = 0;
    while within.valid() {
        seen += 1;
        within.next(&pages).expect("a step");
    }
    assert_eq!(seen, 500);

    // Ranges that end inside the bucket's tree, inside a bucket of that
    // tree, and outside of both.
    let delete = |pages: &mut Mem, model: &mut Records, start: Vec<u8>, end: Option<Vec<u8>>| {
        tree.delete_range(pages, &start, end.as_deref())
            .expect("a range delete");
        model.retain(|key, _| *key < start || end.as_ref().is_some_and(|end| key >= end));
        assert!(records(pages, &tree) == expected(model));
        pages.commit();
    };
    delete(&mut pages, &mut model, long(300), Some(long(600)));
    delete(&mut pages, &mut model, long(301), Some(long(1_000)));
    delete(&mut pages, &mut model, long(2), Some(long(302)));
    delete(&mut pages, &mut model, b"b".to_vec(), Some(long(30)));
    tree.delete_prefix(
        &mut pages,
        &[head.as_slice(), &vec![b's'; limit + 3]].concat(),
    )
    .expect("a prefix delete");
    model.retain(|key, _| !key.get(limit..).is_some_and(|rest| rest.starts_with(b"s")));
    assert!(records(&pages, &tree) == expected(&model));
    assert!(model.len() > 300);
    delete(&mut pages, &mut model, long(1_003), None);
    for key in model.keys().cloned().collect::<Vec<_>>() {
        assert!(tree.delete(&mut pages, &key).expect("a delete"));
    }
    assert!(records(&pages, &tree).is_empty());
    assert_eq!(
        pages.pages.len(),
        1,
        "the bucket's tree left with its last key"
    );
    assert!(pages.live.is_empty() && pages.pending.is_empty());
}
