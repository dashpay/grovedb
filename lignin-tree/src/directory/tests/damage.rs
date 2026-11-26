//! Records that are not a directory: what the check and the lookups
//! report.

use super::{
    super::record::{
        child_key, dropped_key, encode_child, encode_dropped, encode_header, entry_key, names_root,
        root_key, Fields, HEADER_KEY,
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
    // The records of the Merk are in one leaf, which is written once.
    grove.pages.writes_left = Some(0);
    let top = prefix_of(&[]);
    let host = Host {
        parent: &top,
        key: b"n",
        position: 0,
    };
    let added = grove
        .directory
        .add(&mut grove.pages, &[8; 32], b"", Some(host));
    let error = added.expect_err("the page write fails");
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

/// The walk to the Merk with a node beside a prefix reports the damage it
/// decodes, and it decodes of an entry that names no root the id and the
/// address alone: such an entry that is damaged behind them is passed, and
/// the check and the lookup by its prefix report it.
#[test]
fn the_walk_beside_a_prefix_reports_the_damage_it_reads() {
    let mut grove = sample();
    grove.add(&["a", "n"]);
    let rooted = grove.entry(&["a"]).as_bytes().to_vec();
    let rootless = grove.entry(&["a", "n"]).as_bytes().to_vec();
    assert!(names_root(&rooted).expect("an address"));
    assert!(!names_root(&rootless).expect("an address"));
    // Three prefixes behind every prefix of the sample.
    let (from, at, last): (Prefix, Prefix, Prefix) = ([0xfd; 32], [0xfe; 32], [0xff; 32]);
    let mut entries = grove.directory.entries();
    while let Some((prefix, _)) = entries.next(&grove.pages).expect("an entry") {
        assert!(prefix < from);
    }
    let key = entry_key(&at);
    let beside = |grove: &Grove| {
        grove
            .directory
            .rooted_beside(&grove.pages, &from, None, false)
    };
    let by_steps =
        |grove: &Grove| (grove.directory).rooted_beside_by_steps(&grove.pages, &from, None, false);
    let what = |found: Result<Option<Prefix>, DirectoryError>| match found {
        Err(DirectoryError::Damaged { key: found, what }) if found[..] == key => what,
        other => panic!("the walk found {other:?}"),
    };
    let with = |bytes: &[u8], more: &[u8]| [bytes, more].concat();

    // Entries as they are written: one with no root is passed, and one
    // with a root is the answer.
    put(&mut grove, &entry_key(&last), &rooted);
    put(&mut grove, &key, &rootless);
    assert_eq!(beside(&grove).expect("a walk"), Some(last));
    put(&mut grove, &key, &rooted);
    assert_eq!(beside(&grove).expect("a walk"), Some(at));

    // An entry that names a root is read whole.
    put(&mut grove, &key, &with(&rooted, &[0]));
    assert_eq!(what(beside(&grove)), "bytes after the entry");
    let mut no_address = rooted.clone();
    no_address[8..14].copy_from_slice(&[0, 0, 0, 0, 1, 0]);
    put(&mut grove, &key, &no_address);
    assert_eq!(what(beside(&grove)), "a root address that is not one");
    // An entry that ends before the address or in it is no entry.
    for len in [0, 8, 13] {
        put(&mut grove, &key, &rootless[..len]);
        assert_eq!(what(beside(&grove)), "an entry that ends early");
    }

    // An entry that names no root and is damaged behind the address: the
    // walk passes it, the walk by steps did not, and the check and the
    // lookup report it.
    for damaged in [with(&rootless, &[0]), rootless[..14].to_vec()] {
        put(&mut grove, &key, &damaged);
        assert_eq!(beside(&grove).expect("a walk"), Some(last));
        let says = what(by_steps(&grove));
        assert_eq!(
            what(grove.directory.stored(&grove.pages, &at).map(|_| None)),
            says
        );
        assert_eq!(found(&grove), (key.to_vec(), says));
    }

    // A key among the entries that is no prefix.
    delete(&mut grove, &key);
    let short = &key[..key.len() - 1];
    put(&mut grove, short, &rootless);
    let found = grove.directory.rooted_beside(&grove.pages, &at, None, true);
    assert!(
        matches!(&found, Err(DirectoryError::Damaged { key, what })
            if key[..] == *short && *what == "not a prefix"),
        "{found:?}"
    );
}

/// The walk reads no value of the entry it begins under and of the first
/// entry at or beyond the prefix it ends at: an entry there whose value
/// is out of line and cannot be read does not fail it, where the same
/// entry on its way does. And an entry with no root and no id is no entry
/// to it either.
#[test]
fn the_walk_beside_a_prefix_reads_the_values_of_the_entries_between_alone() {
    let mut grove = sample();
    grove.add(&["a", "n"]);
    let rooted = grove.entry(&["a"]).as_bytes().to_vec();
    let rootless = grove.entry(&["a", "n"]).as_bytes().to_vec();
    // Four prefixes behind every prefix of the sample, in their order.
    let behind: [Prefix; 4] = [[0xfc; 32], [0xfd; 32], [0xfe; 32], [0xff; 32]];
    let mut entries = grove.directory.entries();
    while let Some((prefix, _)) = entries.next(&grove.pages).expect("an entry") {
        assert!(prefix < behind[0]);
    }
    let [first, second, third, last] = behind;
    // An entry too long for its page, whose bytes are with no page.
    let out_of_line = vec![0xa5; 3_000];
    let lose_values = |grove: &mut Grove| grove.pages.values.clear();
    let beside = |grove: &Grove, from: &Prefix, until: Option<&Prefix>, back: bool| {
        (grove.directory).rooted_beside(&grove.pages, from, until, back)
    };
    let by_steps = |grove: &Grove, from: &Prefix, until: Option<&Prefix>, back: bool| {
        (grove.directory).rooted_beside_by_steps(&grove.pages, from, until, back)
    };

    // Under the prefix the walk begins at, both ways.
    put(&mut grove, &entry_key(&first), &rooted);
    put(&mut grove, &entry_key(&second), &out_of_line);
    put(&mut grove, &entry_key(&third), &rootless);
    put(&mut grove, &entry_key(&last), &rooted);
    lose_values(&mut grove);
    for (back, to) in [(false, last), (true, first)] {
        let (found, read) = grove.pages.reads(|| beside(&grove, &second, None, back));
        assert_eq!(found.expect("a walk"), Some(to));
        assert_eq!(
            by_steps(&grove, &second, None, back).expect("a walk"),
            Some(to)
        );
        assert_eq!(read.1, 0, "no value out of line was asked for");
    }
    // At the prefix the walk ends at.
    assert_eq!(
        beside(&grove, &first, Some(&second), false).expect("a walk"),
        None
    );
    assert_eq!(
        beside(&grove, &third, Some(&second), true).expect("a walk"),
        None
    );
    assert_eq!(
        by_steps(&grove, &first, Some(&second), false).expect("a walk"),
        None
    );
    // On the way, it is read, and cannot be: by either walk.
    for found in [
        beside(&grove, &first, None, false),
        beside(&grove, &last, None, true),
        by_steps(&grove, &first, None, false),
        beside(&grove, &first, Some(&third), false),
    ] {
        let failed = found.expect_err("an entry that cannot be read");
        assert!(
            matches!(failed, DirectoryError::Flat(FlatError::NoValue { .. })),
            "{failed}"
        );
    }

    // An entry with no root whose id is none: damage in front of the
    // address, which both walks report.
    let mut no_id = rootless.clone();
    no_id[..8].fill(0);
    put(&mut grove, &entry_key(&second), &no_id);
    for found in [
        beside(&grove, &first, None, false),
        by_steps(&grove, &first, None, false),
        beside(&grove, &third, None, true),
    ] {
        let failed = found.expect_err("no entry");
        assert!(
            matches!(&failed, DirectoryError::Damaged { key, what }
                if key[..] == entry_key(&second) && *what == "an entry with no id"),
            "{failed}"
        );
    }
    assert_eq!(
        names_root(&no_id).expect_err("no id").0,
        "an entry with no id"
    );
    assert_eq!(
        names_root(&rootless[..13]).expect_err("short").0,
        "an entry that ends early"
    );
}
