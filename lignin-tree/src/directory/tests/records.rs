//! The bytes of the records, and what is read as one.

use lignin_io::sim::SimRng;

use super::{
    super::record::{
        child_key, children_key, decode_child, decode_child_key, decode_dropped, decode_header,
        decode_id, decode_prefix, dropped_key, encode_child, encode_dropped, encode_header,
        entry_key, root_key, Fields, HEADER_KEY,
    },
    *,
};

fn lineage(ids: &[u64]) -> Vec<u8> {
    ids.iter().flat_map(|raw| id(*raw).encode()).collect()
}

/// An entry with every part, and its parts.
fn full() -> (Entry, Prefix) {
    let parent = [0xAB; 32];
    let above = lineage(&[1, 0x0102]);
    let fields = Fields {
        id: id(0x0304),
        root: Some(Root {
            address: cell(0x0A0B_0C0D, 0x0E0F),
            key: b"rk",
        }),
        tree_type: b"sum",
        host: Some(Host {
            parent: &parent,
            key: b"host",
            position: 2,
        }),
        lineage: &above,
    };
    (fields.encode().expect("an entry"), parent)
}

#[test]
fn an_entry_has_the_bytes_the_layout_says() {
    let (entry, parent) = full();
    let mut bytes = vec![4, 3, 0, 0, 0, 0, 0, 0];
    bytes.extend([0x0D, 0x0C, 0x0B, 0x0A, 0x0F, 0x0E]);
    bytes.extend([2, b'r', b'k']);
    bytes.extend([3, b's', b'u', b'm']);
    bytes.extend([2, 0]);
    bytes.push(2);
    bytes.extend(parent);
    bytes.extend([4, b'h', b'o', b's', b't']);
    bytes.extend([1, 0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 0, 0, 0, 0, 0]);
    assert_eq!(entry.as_bytes(), bytes);

    let read = Entry::decode(bytes).expect("the same entry");
    assert_eq!(read, entry);
    assert_eq!(read.id(), id(0x0304));
    let root = read.root().expect("a root");
    assert_eq!(
        (root.address, root.key),
        (cell(0x0A0B_0C0D, 0x0E0F), &b"rk"[..])
    );
    assert_eq!(read.tree_type(), b"sum");
    let host = read.host().expect("a host");
    assert_eq!(
        (host.parent, host.key, host.position),
        (&parent, &b"host"[..], 2)
    );
    assert_eq!(read.depth(), 2);
    assert_eq!(read.lineage().collect::<Vec<_>>(), [id(1), id(0x0102)]);
    assert_eq!(read.parent_id(), Some(id(0x0102)));
    let shown = format!("{read:?}");
    assert!(
        shown.starts_with("Entry { id: merk#772, root: Some(Root {"),
        "{shown}"
    );
    assert!(shown.ends_with("lineage: [merk#1, merk#258] }"), "{shown}");
}

#[test]
fn an_entry_with_no_root_and_no_host_is_its_id_and_its_type() {
    let fields = Fields {
        id: id(9),
        root: None,
        tree_type: b"",
        host: None,
        lineage: &[],
    };
    let entry = fields.encode().expect("an entry");
    assert_eq!(
        entry.as_bytes(),
        [9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    let read = Entry::decode(entry.as_bytes().to_vec()).expect("an entry");
    assert_eq!((read.root(), read.host(), read.depth()), (None, None, 0));
    assert_eq!((read.tree_type(), read.parent_id()), (&b""[..], None));
    assert_eq!(read.lineage().count(), 0);

    // An empty key is a key: a root may have it, and so may a host.
    let parent = [1; 32];
    let above = lineage(&[3]);
    let fields = Fields {
        root: Some(Root {
            address: cell(1, 1),
            key: b"",
        }),
        host: Some(Host {
            parent: &parent,
            key: b"",
            position: 0,
        }),
        lineage: &above,
        ..fields
    };
    let read = Entry::decode(fields.encode().expect("an entry").as_bytes().to_vec());
    let read = read.expect("an entry");
    assert_eq!(read.root().map(|root| root.key), Some(&b""[..]));
    assert_eq!(read.host().map(|host| host.key), Some(&b""[..]));
}

#[test]
fn an_entry_takes_parts_of_up_to_255_bytes_and_65535_merks_above() {
    let long = [7; 256];
    let parent = [1; 32];
    let above = lineage(&[3]);
    let root = |key| {
        Some(Root {
            address: cell(1, 1),
            key,
        })
    };
    let host = |key| {
        Some(Host {
            parent: &parent,
            key,
            position: 0,
        })
    };
    let fields = Fields {
        id: id(1),
        root: root(&long[..255]),
        tree_type: &long[..255],
        host: host(&long[..255]),
        lineage: &above,
    };
    let entry = fields.encode().expect("the longest parts");
    assert_eq!(
        Entry::decode(entry.as_bytes().to_vec()).expect("read"),
        entry
    );

    let too_long = [
        (
            Fields {
                root: root(&long),
                ..fields
            },
            "a root key",
        ),
        (
            Fields {
                tree_type: &long,
                ..fields
            },
            "a tree type",
        ),
        (
            Fields {
                host: host(&long),
                ..fields
            },
            "a host's key",
        ),
    ];
    for (fields, part) in too_long {
        let error = fields.encode().expect_err("a part of 256 bytes");
        assert!(
            matches!(error, DirectoryError::TooLong { what, len: 256 } if what == part),
            "{error}"
        );
    }

    let deepest = vec![1; 8 * 65_535];
    let deep = Fields {
        lineage: &deepest,
        ..fields
    }
    .encode()
    .expect("65535 Merks above");
    assert_eq!(deep.depth(), 65_535);
    assert_eq!(Entry::decode(deep.as_bytes().to_vec()).expect("read"), deep);
    let deeper = vec![1; 8 * 65_536];
    let error = Fields {
        lineage: &deeper,
        ..fields
    }
    .encode()
    .expect_err("too deep");
    assert!(matches!(error, DirectoryError::TooDeep));
}

#[test]
fn bytes_that_are_no_entry_are_refused() {
    let (entry, _) = full();
    let bytes = entry.as_bytes();
    let refused = |bytes: Vec<u8>| Entry::decode(bytes).expect_err("not an entry").0;

    // Every entry that is cut short, and one with a byte too many.
    for len in 0..bytes.len() {
        assert_eq!(refused(bytes[..len].to_vec()), "an entry that ends early");
    }
    assert_eq!(refused([bytes, &[0]].concat()), "bytes after the entry");

    let with = |at: usize, byte: u8| {
        let mut bytes = bytes.to_vec();
        bytes[at] = byte;
        bytes
    };
    let mut no_id = bytes.to_vec();
    no_id[..8].fill(0);
    assert_eq!(refused(no_id), "an entry with no id");
    // A page with no slot.
    let mut half = with(12, 0);
    half[13] = 0;
    assert_eq!(refused(half), "a root address that is not one");
    let mut rootless = bytes.to_vec();
    rootless[8..14].fill(0);
    assert_eq!(refused(rootless), "a root key with no root");
    let mut unnamed = bytes.to_vec();
    let end = unnamed.len();
    unnamed[end - 8..].fill(0);
    assert_eq!(refused(unnamed), "an ancestor with no id");
    // A length that says more than is there, in each of the three places.
    for at in [14, 17, 56] {
        assert_eq!(refused(with(at, 200)), "an entry that ends early");
    }
    // A host with no Merk above, and Merks above with no host.
    assert_eq!(refused(with(21, 0)), "bytes after the entry");
    let hostless = [&bytes[..21], &[1, 0]].concat();
    assert_eq!(refused(hostless), "an entry that ends early");
}

#[test]
fn no_bytes_make_the_decoder_panic_and_an_entry_has_one_encoding() {
    let (entry, _) = full();
    let mut rng = SimRng::new(0x7ee5);
    let mut read = 0;
    for round in 0..40_000 {
        let mut bytes = entry.as_bytes().to_vec();
        match round % 4 {
            0 => bytes.truncate(rng.below(bytes.len() as u64 + 1) as usize),
            1 => bytes.extend((0..rng.below(20)).map(|_| rng.next_u64() as u8)),
            _ => {}
        }
        for _ in 0..rng.below(4) {
            if let Some(len) = std::num::NonZeroU64::new(bytes.len() as u64) {
                bytes[rng.below(len.get()) as usize] = rng.next_u64() as u8;
            }
        }
        if round % 97 == 0 {
            bytes = (0..rng.below(120)).map(|_| rng.next_u64() as u8).collect();
        }
        let Ok(decoded) = Entry::decode(bytes.clone()) else {
            continue;
        };
        read += 1;
        // What is read is written back as the same bytes, by its parts.
        assert_eq!(
            decoded.fields().encode().expect("an entry").as_bytes(),
            bytes
        );
        let _ = format!("{decoded:?}");
    }
    assert!(read > 5_000, "{read} of the byte strings were entries");
}

#[test]
fn the_records_of_one_kind_are_one_range_of_keys() {
    let prefix = [0x5A; 32];
    let keys: [(RecordKind, Vec<u8>); 5] = [
        (RecordKind::Header, HEADER_KEY.to_vec()),
        (RecordKind::Entry, entry_key(&prefix).to_vec()),
        (RecordKind::Child, child_key(id(0x0807), 3, b"key")),
        (
            RecordKind::Root,
            root_key(cell(0x0102_0304, 0x0506)).to_vec(),
        ),
        (RecordKind::Dropped, dropped_key(id(0x0807)).to_vec()),
    ];
    for (kind, key) in &keys {
        assert_eq!(RecordKind::of(key), Some(*kind));
        assert_eq!(key[0], kind.code());
    }
    assert_eq!(RecordKind::ALL.map(RecordKind::code), [1, 2, 3, 4, 5]);
    assert_eq!(RecordKind::of(&[]), None);
    assert_eq!(RecordKind::of(&[0]), None);
    assert_eq!(RecordKind::of(&[6, 1]), None);

    assert_eq!(keys[1].1[1..], prefix);
    assert_eq!(keys[2].1, [3, 7, 8, 0, 0, 0, 0, 0, 0, 3, b'k', b'e', b'y']);
    assert!(keys[2].1.starts_with(&children_key(id(0x0807))));
    assert_eq!(keys[3].1, [4, 4, 3, 2, 1, 6, 5]);
    assert_eq!(keys[4].1, [5, 7, 8, 0, 0, 0, 0, 0, 0]);
    // The longest key is far below what a page of the smallest size
    // stores as it is.
    let longest = child_key(id(u64::MAX), 255, &[0xFF; 255]);
    assert_eq!(longest.len(), 265);
    assert!(longest.len() < FlatConfig::new(PageSize::MIN).key_limit());
}

#[test]
fn the_small_records_are_read_back_and_nothing_else_is() {
    assert_eq!(decode_header(&encode_header(id(77))), Ok(id(77)));
    for wrong in [&[][..], &[0; 8], &[1; 7], &[1; 9]] {
        let error = decode_header(wrong).expect_err("no header");
        assert_eq!(error.to_string(), "a header that is not an id");
    }

    let prefix = [9; 32];
    let child = encode_child(&prefix, id(5));
    assert_eq!(decode_child(&child), Ok((prefix, id(5))));
    for wrong in [
        &child[..39],
        &[child.as_slice(), &[0]].concat(),
        &[0; 40],
        &[],
    ] {
        let error = decode_child(wrong).expect_err("no child record");
        assert_eq!(error.0, "a child record that is not a prefix and an id");
    }
    assert_eq!(decode_child_key(&[4, b'a', b'b']), Ok((4, &b"ab"[..])));
    assert_eq!(decode_child_key(&[4]), Ok((4, &b""[..])));
    assert!(decode_child_key(&[4; 256]).is_ok());
    for wrong in [&[][..], &[4; 257]] {
        let error = decode_child_key(wrong).expect_err("no key of a child record");
        assert_eq!(error.0, "a child record under no position and key");
    }

    assert_eq!(decode_prefix(&prefix), Ok(prefix));
    assert_eq!(
        decode_prefix(&prefix[1..]).expect_err("short").0,
        "not a prefix"
    );

    let root = Some(cell(3, 4));
    assert_eq!(decode_dropped(&encode_dropped(root)), Ok(root));
    assert_eq!(decode_dropped(&encode_dropped(None)), Ok(None));
    for wrong in [&[][..], &[1; 5], &[1; 7], &[1, 0, 0, 0, 0, 0]] {
        let error = decode_dropped(wrong).expect_err("no address");
        assert_eq!(error.0, "a dropped Merk's record that is not an address");
    }

    assert_eq!(decode_id(&id(8).encode()), Ok(id(8)));
    for wrong in [&[][..], &[0; 8], &[1; 9]] {
        assert_eq!(decode_id(wrong).expect_err("no id").0, "not an id");
    }
}

#[test]
fn an_id_is_a_number_from_one_up() {
    assert_eq!(MerkId::new(0), None);
    assert_eq!(MerkId::FIRST.get(), 1);
    assert_eq!(MerkId::decode([0; 8]), None);
    let id = id(0x0102_0304_0506_0708);
    assert_eq!(id.encode(), [8, 7, 6, 5, 4, 3, 2, 1]);
    assert_eq!(MerkId::decode(id.encode()), Some(id));
    assert_eq!(
        format!("{id} {id:?}"),
        "72623859790382856 merk#72623859790382856"
    );
    assert!(MerkId::FIRST < id);
}
