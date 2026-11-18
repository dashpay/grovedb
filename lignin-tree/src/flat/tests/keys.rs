//! Keys of every length, the longest among them longer than a page.

use super::*;

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

        // A key that differs from a long one in its last byte is another.
        let (long, _) = records.iter().next_back().expect("a record");
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

    // The long keys are one entry of the tree: the bucket of their head.
    let leaf = pages.image(tree.root()).leaf().expect("one page");
    assert_eq!(leaf.len(), 5);
    assert_eq!(
        leaf.key(3).expect("the bucket").to_vec(),
        [head.as_slice(), &[0]].concat()
    );

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
