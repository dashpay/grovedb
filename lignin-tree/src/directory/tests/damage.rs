//! Records that are not a directory: what the check and the lookups
//! report.

use super::{
    super::record::{
        child_key, dropped_key, encode_child, encode_dropped, encode_header, entry_key, root_key,
        Fields, HEADER_KEY,
    },
    *,
};

/// `a` with `a/b` and `a/b/c` below it, each with a root, and `x` with
/// `x/y`, which was dropped: `x/y` is left over.
fn sample() -> Grove {
    let mut grove = Grove::new();
    for keys in [
        &["a"][..],
        &["a", "b"],
        &["a", "b", "c"],
        &["x"],
        &["x", "y"],
    ] {
        grove.add(keys);
        grove.root(keys);
    }
    grove.unlink(&["x"]);
    grove.check();
    grove
}

fn put(grove: &mut Grove, key: &[u8], value: &[u8]) {
    let tree = *grove.directory.tree();
    tree.put(&mut grove.pages, key, value).expect("a record");
}

fn delete(grove: &mut Grove, key: &[u8]) {
    let tree = *grove.directory.tree();
    assert!(tree.delete(&mut grove.pages, key).expect("taken out"));
}

/// The parts of an entry that are written again as something else.
#[derive(Default)]
struct Other<'a> {
    id: Option<MerkId>,
    key: Option<&'a [u8]>,
    position: Option<u8>,
    parent: Option<&'a Prefix>,
    lineage: Option<&'a [u8]>,
}

/// Writes the entry at `keys` again with other parts.
fn rewrite(grove: &mut Grove, keys: &[&str], other: Other<'_>) {
    let entry = grove.stored(keys).expect("an entry");
    let mut fields = entry.fields();
    fields.id = other.id.unwrap_or(fields.id);
    fields.lineage = other.lineage.unwrap_or(fields.lineage);
    if let Some(host) = &mut fields.host {
        host.key = other.key.unwrap_or(host.key);
        host.position = other.position.unwrap_or(host.position);
        host.parent = other.parent.unwrap_or(host.parent);
    }
    let changed = fields.encode().expect("an entry");
    put(
        grove,
        &entry_key(&prefix_of(&path(keys))),
        changed.as_bytes(),
    );
}

fn found(grove: &Grove) -> (Vec<u8>, &'static str) {
    match grove.directory.check(&grove.pages) {
        Err(DirectoryError::Damaged { key, what }) => (key, what),
        other => panic!("the check found {other:?}"),
    }
}

#[test]
fn the_check_finds_every_record_that_does_not_fit() {
    type Damage = fn(&mut Grove);
    fn a() -> Prefix {
        prefix_of(&path(&["a"]))
    }
    fn b() -> Prefix {
        prefix_of(&path(&["a", "b"]))
    }
    fn y() -> Prefix {
        prefix_of(&path(&["x", "y"]))
    }
    /// Takes out the entry that `x/y` left, with the record of its root.
    fn forget_y(grove: &mut Grove) {
        let left = grove.stored(&["x", "y"]).expect("an entry");
        delete(grove, &root_key(left.root().expect("a root").address));
        delete(grove, &entry_key(&y()));
    }
    let cases: Vec<(Damage, RecordKind, &str)> = vec![
        (
            |grove| delete(grove, &HEADER_KEY),
            RecordKind::Header,
            "the directory has no header",
        ),
        (
            |grove| put(grove, &HEADER_KEY, &[1; 3]),
            RecordKind::Header,
            "a header that is not an id",
        ),
        (
            |grove| put(grove, &entry_key(&[9; 32]), &[1; 5]),
            RecordKind::Entry,
            "an entry that ends early",
        ),
        (
            |grove| put(grove, &[RecordKind::Entry.code(), 1, 2], &[1; 5]),
            RecordKind::Entry,
            "not a prefix",
        ),
        (
            |grove| put(grove, &HEADER_KEY, &encode_header(id(6))),
            RecordKind::Entry,
            "an id that was not given out",
        ),
        (
            |grove| {
                let id = Some(id(2));
                rewrite(
                    grove,
                    &["a", "b"],
                    Other {
                        id,
                        ..Other::default()
                    },
                );
            },
            RecordKind::Entry,
            "an id that another Merk has",
        ),
        (
            |grove| {
                let root = grove.entry(&["a"]).root().expect("a root").address;
                delete(grove, &root_key(root));
            },
            RecordKind::Entry,
            "a root that has no record",
        ),
        (
            |grove| {
                let root = grove.entry(&["a"]).root().expect("a root").address;
                put(grove, &root_key(root), &[7; 32]);
            },
            RecordKind::Entry,
            "a root in the cell of another Merk",
        ),
        (
            |grove| put(grove, &root_key(cell(80, 1)), &[7; 32]),
            RecordKind::Root,
            "a root record of no entry",
        ),
        (
            |grove| put(grove, &dropped_key(id(900)), &encode_dropped(None)),
            RecordKind::Dropped,
            "an id that was not given out",
        ),
        (
            |grove| put(grove, &dropped_key(id(2)), &encode_dropped(None)),
            RecordKind::Entry,
            "an id that another Merk has",
        ),
        (
            |grove| put(grove, &dropped_key(id(2)), &[1; 2]),
            RecordKind::Dropped,
            "a dropped Merk's record that is not an address",
        ),
        (
            |grove| {
                put(
                    grove,
                    &[RecordKind::Dropped.code(), 1],
                    &encode_dropped(None),
                )
            },
            RecordKind::Dropped,
            "not an id",
        ),
        // The handle was loaded before the pages got another drop.
        (
            |grove| {
                let mut other = grove.directory.clone();
                other
                    .unlink(&mut grove.pages, &prefix_of(&path(&["a"])))
                    .expect("dropped");
            },
            RecordKind::Dropped,
            "the handle has other dropped Merks",
        ),
        (
            |grove| delete(grove, &entry_key(&b())),
            RecordKind::Root,
            "a root record of no entry",
        ),
        (
            |grove| {
                let root = grove.entry(&["a", "b"]).root().expect("a root").address;
                delete(grove, &root_key(root));
                delete(grove, &entry_key(&b()));
            },
            RecordKind::Entry,
            "a Merk in use hosts a Merk with no entry",
        ),
        (
            |grove| {
                let key = Some(&b"other"[..]);
                rewrite(
                    grove,
                    &["a", "b"],
                    Other {
                        key,
                        ..Other::default()
                    },
                );
            },
            RecordKind::Entry,
            "an entry under the record of another host",
        ),
        (
            |grove| {
                let position = Some(1);
                rewrite(
                    grove,
                    &["a", "b"],
                    Other {
                        position,
                        ..Other::default()
                    },
                );
            },
            RecordKind::Entry,
            "an entry under the record of another host",
        ),
        (
            |grove| {
                let parent = Some(&[3; 32]);
                rewrite(
                    grove,
                    &["a", "b"],
                    Other {
                        parent,
                        ..Other::default()
                    },
                );
            },
            RecordKind::Entry,
            "an entry that names another parent",
        ),
        (
            |grove| {
                let above = [id(77).encode(), id(2).encode()].concat();
                let lineage = Some(&above[..]);
                rewrite(
                    grove,
                    &["a", "b"],
                    Other {
                        lineage,
                        ..Other::default()
                    },
                );
            },
            RecordKind::Entry,
            "an entry with other Merks above it",
        ),
        (
            |grove| {
                let above = id(3).encode();
                let lineage = Some(&above[..]);
                rewrite(
                    grove,
                    &["a", "b", "c"],
                    Other {
                        lineage,
                        ..Other::default()
                    },
                );
            },
            RecordKind::Entry,
            "an entry with other Merks above it",
        ),
        (
            |grove| {
                let above = id(77).encode();
                let lineage = Some(&above[..]);
                rewrite(
                    grove,
                    &["a", "b"],
                    Other {
                        lineage,
                        ..Other::default()
                    },
                );
            },
            RecordKind::Entry,
            "an entry under the record of another host",
        ),
        // The Merk lost its last node, and Merks are entered below it.
        (
            |grove| {
                let a = a();
                let change = named(&a, None);
                grove
                    .directory
                    .set_roots(&mut grove.pages, &[change])
                    .expect("no root");
            },
            RecordKind::Entry,
            "a Merk with no node hosts a Merk",
        ),
        (
            |grove| {
                put(
                    grove,
                    &child_key(id(500), 0, b"k"),
                    &encode_child(&[5; 32], id(6)),
                )
            },
            RecordKind::Child,
            "a child record that no Merk leads to",
        ),
        (
            |grove| put(grove, &child_key(id(2), 0, b"k"), &[5; 3]),
            RecordKind::Child,
            "a child record that is not a prefix and an id",
        ),
        (
            |grove| {
                put(
                    grove,
                    &[RecordKind::Child.code(), 2, 0, 0, 0, 0, 0, 0, 0],
                    &[5; 40],
                )
            },
            RecordKind::Child,
            "a child record under no position and key",
        ),
        (
            |grove| delete(grove, &child_key(id(2), 0, b"b")),
            RecordKind::Child,
            "a child record that no Merk leads to",
        ),
        (
            |grove| delete(grove, &child_key(id(3), 0, b"c")),
            RecordKind::Entry,
            "an entry that no Merk leads to",
        ),
        // The entry that `x/y` left, with no dropped Merk above it.
        (
            |grove| {
                forget_y(grove);
                let under = [(b"x".to_vec(), 0), (b"y".to_vec(), 0), (b"z".to_vec(), 0)];
                let host = Host {
                    parent: &y(),
                    key: b"z",
                    position: 0,
                };
                let fields = Fields {
                    id: id(7),
                    root: None,
                    tree_type: b"",
                    host: Some(host),
                    lineage: &id(6).encode(),
                };
                let entry = fields.encode().expect("an entry");
                put(grove, &entry_key(&prefix_of(&under)), entry.as_bytes());
                put(
                    grove,
                    &child_key(id(6), 0, b"z"),
                    &encode_child(&prefix_of(&under), id(7)),
                );
                put(grove, &HEADER_KEY, &encode_header(id(8)));
            },
            RecordKind::Entry,
            "a left-over entry that a lookup returns",
        ),
        // A root where the dropped Merk `x` has its cells, and one where
        // the Merk `a/b` has its root.
        (
            |grove| {
                let x = grove.drops()[0].root.expect("a root");
                let c = prefix_of(&path(&["a", "b", "c"]));
                let was = grove
                    .entry(&["a", "b", "c"])
                    .root()
                    .expect("a root")
                    .address;
                delete(grove, &root_key(was));
                put(grove, &root_key(x), &c);
                let entry = grove.stored(&["a", "b", "c"]).expect("an entry");
                let mut fields = entry.fields();
                fields.root = Some(Root {
                    address: x,
                    key: b"root",
                });
                put(
                    grove,
                    &entry_key(&c),
                    fields.encode().expect("an entry").as_bytes(),
                );
            },
            RecordKind::Entry,
            "a root in the cell of a dropped Merk",
        ),
        (
            |grove| {
                let c = prefix_of(&path(&["a", "b", "c"]));
                let was = grove
                    .entry(&["a", "b", "c"])
                    .root()
                    .expect("a root")
                    .address;
                delete(grove, &root_key(was));
                let entry = grove.stored(&["a", "b", "c"]).expect("an entry");
                let mut fields = entry.fields();
                fields.root = grove.entry(&["a", "b"]).root().map(|root| Root {
                    address: root.address,
                    key: b"root",
                });
                put(
                    grove,
                    &entry_key(&c),
                    fields.encode().expect("an entry").as_bytes(),
                );
            },
            RecordKind::Entry,
            "a root in the cell of another Merk",
        ),
        (
            |grove| put(grove, &[9, 9], b"what"),
            RecordKind::Header,
            "a record of no kind",
        ),
        (
            |grove| put(grove, &[0], b"what"),
            RecordKind::Header,
            "a record of no kind",
        ),
        (
            |grove| put(grove, &[RecordKind::Header.code(), 0], b"what"),
            RecordKind::Header,
            "a record of no kind",
        ),
        (
            |grove| {
                put(
                    grove,
                    &child_key(id(6), 0, b"k"),
                    &encode_child(&[5; 32], id(900)),
                )
            },
            RecordKind::Entry,
            "an id that was not given out",
        ),
        // A record under the left-over `x/y` that names the Merk `a`.
        (
            |grove| {
                put(
                    grove,
                    &child_key(id(6), 0, b"k"),
                    &encode_child(&[5; 32], id(2)),
                )
            },
            RecordKind::Entry,
            "a Merk that two hosts have",
        ),
        // The same for the Merk at the root, which no host has, and for a
        // dropped Merk.
        (
            |grove| {
                put(
                    grove,
                    &child_key(id(6), 0, b"k"),
                    &encode_child(&[5; 32], id(1)),
                )
            },
            RecordKind::Entry,
            "a left-over record of a Merk that is entered",
        ),
        (
            |grove| {
                put(
                    grove,
                    &child_key(id(6), 0, b"k"),
                    &encode_child(&[5; 32], id(5)),
                )
            },
            RecordKind::Entry,
            "a left-over record of a Merk that is entered",
        ),
        // A child record of a Merk whose entry is gone that leads back to
        // that Merk.
        (
            |grove| {
                forget_y(grove);
                let record = encode_child(&[5; 32], id(6));
                put(grove, &child_key(id(6), 0, b"k"), &record);
            },
            RecordKind::Entry,
            "a Merk that two hosts have",
        ),
    ];
    for (damage, kind, says) in cases {
        let mut grove = sample();
        damage(&mut grove);
        let (key, what) = found(&grove);
        // A record of no kind is reported with no key.
        let kind = (says != "a record of no kind").then_some(kind);
        assert_eq!((RecordKind::of(&key), what), (kind, says));
    }
}

#[test]
fn a_lookup_reports_a_record_that_is_not_one() {
    let mut grove = sample();
    let a = prefix_of(&path(&["a"]));
    let key = entry_key(&a);
    put(&mut grove, &key, &[0; 18]);
    for found in [
        grove.directory.open(&grove.pages, &a),
        grove.directory.stored(&grove.pages, &a),
    ] {
        let error = found.expect_err("no entry");
        assert!(
            matches!(&error, DirectoryError::Damaged { key: at, what }
                if at[..] == key && *what == "an entry with no id"),
            "{error}"
        );
    }
    let at = cell(81, 1);
    put(&mut grove, &root_key(at), &[1; 5]);
    let error = grove
        .directory
        .root_owner(&grove.pages, at)
        .expect_err("no prefix");
    assert!(
        matches!(
            error,
            DirectoryError::Damaged {
                what: "not a prefix",
                ..
            }
        ),
        "{error}"
    );

    // A directory with no header gives no id out.
    delete(&mut grove, &HEADER_KEY);
    let error = grove
        .directory
        .add(&mut grove.pages, &[8; 32], b"", None)
        .expect_err("no id");
    assert_eq!(
        error.to_string(),
        "subtree directory record 01: the directory has no header"
    );
    // Nor does one that gave the last id out.
    put(&mut grove, &HEADER_KEY, &encode_header(id(u64::MAX)));
    let pages = grove.pages.pages.clone();
    let error = grove
        .directory
        .add(&mut grove.pages, &[8; 32], b"", None)
        .expect_err("no id");
    assert!(matches!(error, DirectoryError::IdsExhausted), "{error}");
    assert_eq!(grove.pages.pages, pages);
}

#[test]
fn a_failed_page_write_is_reported_as_the_keyspace_reports_it() {
    let mut grove = sample();
    grove.pages.writes_left = Some(1);
    let top = prefix_of(&[]);
    let host = Host {
        parent: &top,
        key: b"n",
        position: 0,
    };
    let added = grove
        .directory
        .add(&mut grove.pages, &[8; 32], b"", Some(host));
    let error = added.expect_err("the second page write fails");
    assert!(
        matches!(error, DirectoryError::Flat(FlatError::Freed { .. })),
        "{error}"
    );
    assert!(error.to_string().starts_with("subtree directory: page "));
    assert!(std::error::Error::source(&error).is_some());

    // The same for a walk whose page is gone.
    let mut grove = sample();
    let root = grove.directory.root_page();
    grove.pages.pages.remove(&root);
    let error = grove
        .directory
        .children(id(1))
        .next(&grove.pages)
        .expect_err("no page");
    assert!(matches!(error, DirectoryError::Flat(_)), "{error}");
    let loaded = Directory::load(&grove.pages, root, grove.pages.config);
    assert!(matches!(loaded, Err(DirectoryError::Flat(_))));
    assert!(matches!(
        grove.directory.check(&grove.pages),
        Err(DirectoryError::Flat(_))
    ));
}
