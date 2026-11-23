//! The stream format, what a dump and a load refuse, and the shapes of a
//! subtree beside those of `engine/lignin/tests/hatch.rs`, on the engine
//! of the suite.

use grovedb_merk::{
    store::{NodeChangeSet, NodeStore},
    tree::hash::NULL_HASH,
};
use grovedb_storage::FlatStore;
use grovedb_version::version::GroveVersion;

use super::{
    format::{Counts, DumpReader, DumpWriter, Keyspace},
    DumpLoadError, DumpedSubtree,
};
use crate::{
    engine::{DefaultEngine, Engine, SubtreeHandle},
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element, Error,
};

const TREE: &[u8] = b"tree";

fn v() -> &'static GroveVersion {
    GroveVersion::latest()
}

type Record = (Keyspace, Vec<u8>, Vec<u8>);

/// A stream with the header given and `records`, as the writer makes it.
fn stream(element: &[u8], root: [u8; 32], records: &[Record]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = DumpWriter::new(&mut bytes, element, &root).expect("a header");
    for (keyspace, key, value) in records {
        writer.record(*keyspace, key, value).expect("a record");
    }
    let counts = writer.finish().expect("an end");
    assert_eq!(counts.bytes, bytes.len() as u64);
    bytes
}

type Read = (Vec<u8>, [u8; 32], Vec<Record>, Counts);

/// Everything a reader gives of `bytes`.
fn read(mut bytes: &[u8]) -> Result<(Read, usize), DumpLoadError> {
    let (mut reader, element, root) = DumpReader::open(&mut bytes)?;
    let mut records = Vec::new();
    while let Some(record) = reader.next()? {
        records.push(record);
    }
    let counts = reader.counts();
    Ok(((element, root, records, counts), bytes.len()))
}

fn some_records() -> Vec<Record> {
    vec![
        (Keyspace::Data, b"a".to_vec(), b"node a".to_vec()),
        (Keyspace::Data, b"ab".to_vec(), vec![]),
        (Keyspace::Data, b"b".to_vec(), vec![7; 300]),
        (Keyspace::Aux, vec![], b"under the empty key".to_vec()),
        (Keyspace::Aux, b"a".to_vec(), b"aux".to_vec()),
        (Keyspace::Roots, vec![], b"ab".to_vec()),
        (Keyspace::Meta, b"m".to_vec(), b"meta".to_vec()),
    ]
}

/// Puts the checksum of `body` after it.
fn sealed(mut body: Vec<u8>) -> Vec<u8> {
    let checksum = *blake3::hash(&body).as_bytes();
    body.extend_from_slice(&checksum);
    body
}

fn malformed(bytes: &[u8]) -> &'static str {
    match read(bytes) {
        Err(DumpLoadError::Malformed(why)) => why,
        other => panic!("not refused as malformed: {other:?}"),
    }
}

#[test]
fn a_stream_is_read_back_as_it_was_written_and_no_further() {
    let records = some_records();
    let mut bytes = stream(b"an element", [9; 32], &records);
    let length = bytes.len();
    bytes.extend_from_slice(b"what follows the dump");
    let ((element, root, read_back, counts), left) = read(&bytes).expect("a whole dump");
    assert_eq!((element.as_slice(), root), (&b"an element"[..], [9; 32]));
    assert_eq!(read_back, records);
    assert_eq!(counts.records, [3, 2, 1, 1]);
    assert_eq!(counts.bytes, length as u64);
    assert_eq!(left, b"what follows the dump".len());

    let empty = stream(&[], [0; 32], &[]);
    let ((element, _, records, counts), left) = read(&empty).expect("a dump of nothing");
    assert!(element.is_empty() && records.is_empty());
    assert_eq!((counts.records, counts.bytes, left), ([0; 4], 89, 0));
}

#[test]
fn a_stream_that_ends_early_or_differs_in_a_bit_is_no_dump() {
    let bytes = stream(b"an element", [9; 32], &some_records());
    for length in 0..bytes.len() {
        match read(&bytes[..length]) {
            Err(DumpLoadError::Io(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof, "{length}");
            }
            other => panic!("the first {length} bytes: {other:?}"),
        }
    }
    for at in 0..bytes.len() {
        for bit in 0..8 {
            let mut changed = bytes.clone();
            changed[at] ^= 1 << bit;
            assert!(read(&changed).is_err(), "bit {bit} of byte {at}");
        }
    }
}

#[test]
fn each_fault_of_a_stream_is_named() {
    let whole = stream(b"e", [1; 32], &some_records());
    let body = |records: &[Record]| {
        let mut bytes = stream(b"e", [1; 32], records);
        bytes.truncate(bytes.len() - 32);
        bytes
    };

    let mut other_magic = whole.clone();
    other_magic[0] = b'X';
    assert_eq!(malformed(&other_magic), "not the dump of a subtree");
    let mut other_version = whole.clone();
    other_version[11] = 2;
    assert_eq!(
        malformed(&other_version),
        "a dump of another version of the format"
    );
    let mut other_checksum = whole.clone();
    *other_checksum.last_mut().unwrap() ^= 1;
    assert_eq!(
        malformed(&other_checksum),
        "the checksum of the dump is not the one of its bytes"
    );

    // A dump that states another number of records, with the checksum of
    // its bytes.
    let mut counted = body(&some_records());
    let at = counted.len() - 1;
    counted[at] += 1;
    assert_eq!(
        malformed(&sealed(counted)),
        "the dump holds another number of records than it states"
    );

    // A record of a keyspace that there is none of: the header of an
    // empty dump, then the tag 5.
    let mut no_keyspace = body(&[]);
    let end = no_keyspace.len() - 9;
    no_keyspace[end] = 5;
    assert_eq!(malformed(&no_keyspace), "a record of no keyspace");

    let record = |keyspace, key: &[u8]| (keyspace, key.to_vec(), b"v".to_vec());
    for out_of_order in [
        vec![record(Keyspace::Data, b"b"), record(Keyspace::Data, b"a")],
        vec![record(Keyspace::Data, b"a"), record(Keyspace::Data, b"a")],
        vec![record(Keyspace::Aux, b"a"), record(Keyspace::Data, b"b")],
        vec![record(Keyspace::Meta, b""), record(Keyspace::Roots, b"")],
    ] {
        assert_eq!(
            malformed(&stream(b"e", [1; 32], &out_of_order)),
            "the records are not in the order of the format"
        );
    }
    assert_eq!(
        malformed(&stream(b"e", [1; 32], &[record(Keyspace::Roots, b"k")])),
        "a record of the roots keyspace under a key"
    );
}

#[test]
fn every_error_says_what_it_is() {
    let io: DumpLoadError = std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into();
    assert!(io.to_string().starts_with("i/o: "), "{io}");
    assert_eq!(
        DumpLoadError::Malformed("a record of no keyspace").to_string(),
        "malformed dump: a record of no keyspace"
    );
    assert_eq!(
        DumpLoadError::Refused("the element is no tree").to_string(),
        "refused: the element is no tree"
    );
    let grove: DumpLoadError = Error::InvalidInput("an input").into();
    assert_eq!(grove.to_string(), "invalid input: an input");
}

fn grove_with(key: &[u8], element: Element) -> TempGroveDb {
    let db = make_empty_grovedb();
    db.insert(EMPTY_PATH, key, element, None, None, v())
        .unwrap()
        .expect("an element under the root");
    db
}

fn put_item(db: &TempGroveDb, tree: &[u8], key: &[u8]) {
    let item = Element::new_item(key.to_vec());
    db.insert([tree].as_ref(), key, item, None, None, v())
        .unwrap()
        .expect("an item");
}

fn dump(db: &TempGroveDb, key: &[u8]) -> (Vec<u8>, DumpedSubtree) {
    let mut bytes = Vec::new();
    let dumped = db
        .dump_subtree(EMPTY_PATH, key, &mut bytes, None, v())
        .expect("a dump");
    (bytes, dumped)
}

fn refused<T: std::fmt::Debug>(answer: Result<T, DumpLoadError>) -> &'static str {
    match answer {
        Err(DumpLoadError::Refused(why)) => why,
        other => panic!("not refused: {other:?}"),
    }
}

/// Loads `bytes` under `key`, checks the root and replaces the element,
/// with no transaction.
fn apply(db: &TempGroveDb, key: &[u8], bytes: &[u8]) -> DumpedSubtree {
    let loaded = db
        .load_subtree(EMPTY_PATH, key, bytes, None, v())
        .expect("a load");
    let root = db
        .subtree_root_as(EMPTY_PATH, key, &loaded.element, None, v())
        .expect("a root");
    assert_eq!(root, loaded.root);
    db.replace_subtree_root(EMPTY_PATH, key, loaded.element.clone(), root, None, v())
        .unwrap()
        .expect("the element");
    loaded
}

/// What `apply` does after its load: the root is checked and the element
/// replaced, for a subtree that holds the records of `bytes` already.
fn apply_after_load(db: &TempGroveDb, key: &[u8], bytes: &[u8]) {
    let ((element, stated, ..), _) = read(bytes).expect("a dump");
    let element = Element::deserialize(&element, v()).expect("an element");
    let root = db
        .subtree_root_as(EMPTY_PATH, key, &element, None, v())
        .expect("a root");
    assert_eq!(root, stated);
    db.replace_subtree_root(EMPTY_PATH, key, element, root, None, v())
        .unwrap()
        .expect("the element");
}

fn same_grove(one: &TempGroveDb, other: &TempGroveDb) {
    let root = |db: &TempGroveDb| db.root_hash(None, v()).unwrap().expect("a root hash");
    assert_eq!(root(one), root(other));
    let issues = other.verify_grovedb(None, true, false, v());
    assert!(issues.expect("a verification").is_empty());
}

#[test]
fn a_subtree_that_is_not_one_tree_of_one_merk_is_not_dumped() {
    let db = grove_with(TREE, Element::new_item(b"no tree".to_vec()));
    let dumped = db.dump_subtree(EMPTY_PATH, TREE, Vec::new(), None, v());
    assert_eq!(refused(dumped), "the element is no tree");

    let db = grove_with(TREE, Element::empty_provable_count_indexed_tree());
    let dumped = db.dump_subtree(EMPTY_PATH, TREE, Vec::new(), None, v());
    assert_eq!(
        refused(dumped),
        "an indexed tree hosts several Merks, and a dump carries one subtree"
    );
    let indexed = Element::empty_provable_count_indexed_tree();
    let root = db.subtree_root_as(EMPTY_PATH, TREE, &indexed, None, v());
    assert!(matches!(root, Err(Error::NotSupported(_))), "{root:?}");
    let item = Element::new_item(vec![]);
    let root = db.subtree_root_as(EMPTY_PATH, TREE, &item, None, v());
    assert!(matches!(root, Err(Error::InvalidInput(_))), "{root:?}");

    let dumped = db.dump_subtree(EMPTY_PATH, b"no such key", Vec::new(), None, v());
    assert!(matches!(dumped, Err(DumpLoadError::Grove(_))), "{dumped:?}");
}

#[test]
fn a_load_is_refused_by_what_the_target_holds() {
    let source = grove_with(TREE, Element::empty_tree());
    put_item(&source, TREE, b"a");
    let (bytes, _) = dump(&source, TREE);
    let load = |db: &TempGroveDb| db.load_subtree(EMPTY_PATH, TREE, &bytes[..], None, v());

    let no_tree = grove_with(TREE, Element::new_item(vec![]));
    assert_eq!(refused(load(&no_tree)), "the element is no tree");
    let other_type = grove_with(TREE, Element::empty_sum_tree());
    assert_eq!(
        refused(load(&other_type)),
        "the element is a tree of another type than the dumped one"
    );
    let absent = make_empty_grovedb();
    assert!(matches!(load(&absent), Err(DumpLoadError::Grove(_))));

    let holds_a_node = grove_with(TREE, Element::empty_tree());
    put_item(&holds_a_node, TREE, b"z");
    assert_eq!(refused(load(&holds_a_node)), "the subtree holds a record");

    // A record in each of the other keyspaces refuses the load as well.
    for keyspace in [Keyspace::Aux, Keyspace::Roots, Keyspace::Meta] {
        let db = grove_with(TREE, Element::empty_tree());
        let tx = db.start_transaction();
        let batch = <DefaultEngine as Engine>::Batch::default();
        let handle = (db.engine())
            .subtree([TREE].as_ref().into(), Some(&batch), &tx)
            .unwrap();
        match keyspace {
            Keyspace::Aux => SubtreeHandle::aux(&handle)
                .put(b"k", b"v", None)
                .unwrap()
                .expect("a put"),
            Keyspace::Meta => SubtreeHandle::meta(&handle)
                .put(b"k", b"v", None)
                .unwrap()
                .expect("a put"),
            _ => {
                let mut change_set = handle.change_set();
                change_set.put_root_key(b"k", None).expect("a root key");
                handle.commit(change_set).unwrap().expect("a change set");
            }
        }
        drop(handle);
        (db.engine().apply_batch(batch, &tx).unwrap()).expect("a batch");
        db.commit_transaction(tx).unwrap().expect("a commit");
        assert_eq!(
            refused(load(&db)),
            "the subtree holds a record",
            "{keyspace:?}"
        );
    }

    // And a target that takes it.
    let target = grove_with(TREE, Element::empty_tree());
    apply(&target, TREE, &bytes);
    same_grove(&source, &target);
}

#[test]
fn a_stream_whose_header_names_no_tree_is_not_loaded() {
    let db = grove_with(TREE, Element::empty_tree());
    let load = |bytes: &[u8]| db.load_subtree(EMPTY_PATH, TREE, bytes, None, v());
    let item = Element::new_item(vec![]).serialize(v()).expect("bytes");
    for element in [&b"\xff\xff\xff no element"[..], &item] {
        let loaded = load(&stream(element, [0; 32], &[]));
        assert!(
            matches!(
                loaded,
                Err(DumpLoadError::Malformed("the element of the dump"))
            ),
            "{loaded:?}"
        );
    }
    let tree = Element::empty_tree().serialize(v()).expect("bytes");
    let cut = stream(&tree, [0; 32], &[]);
    let loaded = load(&cut[..cut.len() - 1]);
    assert!(matches!(loaded, Err(DumpLoadError::Io(_))), "{loaded:?}");
}

/// The node records of a tree that holds `keys`, inserted in that order.
fn node_records(keys: &[&[u8]]) -> (Vec<u8>, Vec<Record>) {
    let db = grove_with(TREE, Element::empty_tree());
    for key in keys {
        put_item(&db, TREE, key);
    }
    let (bytes, _) = dump(&db, TREE);
    let ((element, _, records, _), _) = read(&bytes).expect("a dump");
    (element, records)
}

#[test]
fn node_records_that_are_not_one_tree_are_not_loaded() {
    // `b` is the root, with `a` on its left and `c` on its right.
    let (element, tree) = node_records(&[b"b", b"a", b"c"]);
    let record = |key: &[u8]| {
        let found = tree.iter().find(|(_, of, _)| of == key);
        found.expect("a record of the tree").2.clone()
    };
    let node = |key: &[u8], bytes: Vec<u8>| (Keyspace::Data, key.to_vec(), bytes);
    // `x` with `y` under it, and `y` with `x` under it.
    let (_, x_above) = node_records(&[b"x", b"y"]);
    let (_, y_above) = node_records(&[b"y", b"x"]);
    let x_to_y = x_above[0].2.clone();
    let y_to_x = y_above[1].2.clone();

    let cases: [(&str, Vec<Record>); 6] = [
        (
            "a record of the data keyspace is no node",
            vec![node(b"a", b"no node".to_vec())],
        ),
        (
            "a link names no node of the dump",
            vec![node(b"a", record(b"a")), node(b"b", record(b"b"))],
        ),
        (
            "two links name one node",
            vec![
                node(b"a", record(b"b")),
                node(b"b", record(b"b")),
                node(b"c", record(b"c")),
            ],
        ),
        (
            "the nodes of the dump are not one tree",
            vec![node(b"a", record(b"a")), node(b"c", record(b"c"))],
        ),
        (
            "the nodes of the dump are not one tree",
            vec![node(b"x", x_to_y.clone()), node(b"y", y_to_x.clone())],
        ),
        (
            "the nodes of the dump are not one tree",
            vec![
                node(b"a", record(b"a")),
                node(b"x", x_to_y),
                node(b"y", y_to_x),
            ],
        ),
    ];
    for (why, records) in cases {
        let db = grove_with(TREE, Element::empty_tree());
        let bytes = stream(&element, [0; 32], &records);
        let loaded = db.load_subtree(EMPTY_PATH, TREE, &bytes[..], None, v());
        match loaded {
            Err(DumpLoadError::Malformed(said)) => assert_eq!(said, why),
            other => panic!("{why}: {other:?}"),
        }
        // Nothing of it is in the tree.
        let (_, after) = dump(&db, TREE);
        assert_eq!(after.data_records, 0, "{why}");
    }
}

#[test]
fn a_node_record_that_merk_would_encode_otherwise_is_not_loaded() {
    // A count tree whose root `b` has `a` on its left: the record of `b`
    // begins with that link, which ends with the count of the nodes under
    // it, 1, as a varint of one byte.
    let db = grove_with(TREE, Element::empty_count_tree());
    for key in [&b"b"[..], b"a", b"c"] {
        put_item(&db, TREE, key);
    }
    let (bytes, _) = dump(&db, TREE);
    let ((element, _, mut records, _), _) = read(&bytes).expect("a dump");
    let root = &mut records[1].2;
    assert_eq!(
        (root[..3].to_vec(), root[37..39].to_vec()),
        (b"\x01\x01a".to_vec(), vec![3, 1])
    );
    // The same count in two bytes: a record that decodes to the same node.
    root.splice(38..39, [0x81, 0x00]);

    let target = grove_with(TREE, Element::empty_count_tree());
    let bytes = stream(&element, [0; 32], &records);
    let loaded = target.load_subtree(EMPTY_PATH, TREE, &bytes[..], None, v());
    match loaded {
        Err(DumpLoadError::Malformed(said)) => {
            assert_eq!(said, "a node record is not as Merk encodes its node")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(dump(&target, TREE).1.data_records, 0);
}

#[test]
fn an_element_that_does_not_name_what_the_records_hold_is_refused() {
    // The records of a tree whose root is `b`, and of an MMR tree.
    let (named, tree) = node_records(&[b"b", b"a", b"c"]);
    let mmr = grove_with(TREE, Element::empty_mmr_tree());
    for n in 0..5u8 {
        mmr.mmr_tree_append(EMPTY_PATH, TREE, vec![n; 20], None, v())
            .unwrap()
            .expect("an append");
    }
    let (mmr_bytes, _) = dump(&mmr, TREE);
    let ((_, _, leaves, _), _) = read(&mmr_bytes).expect("a dump");
    let bytes = |element: Element| element.serialize(v()).expect("bytes");

    // A stream whose element names no root, another root, or a root of no
    // record, and one whose element counts nothing over records.
    let cases: [(Element, Vec<u8>, Vec<Record>); 4] = [
        (
            Element::empty_tree(),
            bytes(Element::empty_tree()),
            tree.clone(),
        ),
        (
            Element::empty_tree(),
            bytes(Element::Tree(Some(b"a".to_vec()), None)),
            tree.clone(),
        ),
        (Element::empty_tree(), named.clone(), vec![]),
        (
            Element::empty_mmr_tree(),
            bytes(Element::empty_mmr_tree()),
            leaves,
        ),
    ];
    for (empty, stated, records) in cases {
        let db = grove_with(TREE, empty);
        let stream = stream(&stated, [0; 32], &records);
        let loaded = db.load_subtree(EMPTY_PATH, TREE, &stream[..], None, v());
        match loaded {
            Err(DumpLoadError::Malformed(said)) => assert_eq!(
                said,
                "the element of the dump does not name what its records hold"
            ),
            other => panic!("{other:?}"),
        }
    }

    // A subtree that was loaded and whose element was not replaced is such
    // a subtree, and is not dumped.
    let tree = grove_with(TREE, Element::empty_tree());
    put_item(&tree, TREE, b"a");
    for (source, empty) in [
        (tree, Element::empty_tree()),
        (mmr, Element::empty_mmr_tree()),
    ] {
        let (bytes, _) = dump(&source, TREE);
        let target = grove_with(TREE, empty);
        target
            .load_subtree(EMPTY_PATH, TREE, &bytes[..], None, v())
            .expect("a load");
        let dumped = target.dump_subtree(EMPTY_PATH, TREE, Vec::new(), None, v());
        assert_eq!(
            refused(dumped),
            "the element does not name what its subtree holds"
        );
        apply_after_load(&target, TREE, &bytes);
        same_grove(&source, &target);
    }
}

#[test]
fn a_root_that_the_records_do_not_give_is_an_error() {
    let db = grove_with(TREE, Element::empty_tree());
    for key in [&b"a"[..], b"b", b"c"] {
        put_item(&db, TREE, key);
    }
    let held = db
        .get_raw(EMPTY_PATH, TREE, None, v())
        .unwrap()
        .expect("the tree");
    let Element::Tree(Some(root_key), _) = &held else {
        panic!("{held:?}");
    };
    let root = db.subtree_root_as(EMPTY_PATH, TREE, &held, None, v());
    assert_ne!(root.expect("the root of the tree"), NULL_HASH);
    // Read as a tree whose nodes carry a count, which these do not.
    let counted = Element::ProvableCountTree(Some(root_key.clone()), 3, None);
    let root = db.subtree_root_as(EMPTY_PATH, TREE, &counted, None, v());
    assert!(matches!(root, Err(Error::MerkError(_))), "{root:?}");
    // And under a root key that no node has.
    let absent = Element::Tree(Some(b"no such node".to_vec()), None);
    let root = db.subtree_root_as(EMPTY_PATH, TREE, &absent, None, v());
    assert!(root.is_err(), "{root:?}");
}

#[test]
fn a_tree_that_holds_nothing_is_dumped_and_loaded() {
    for element in [
        Element::empty_tree(),
        Element::empty_count_tree(),
        Element::empty_mmr_tree(),
        Element::empty_commitment_tree(4).expect("a chunk power"),
    ] {
        let source = grove_with(TREE, element.clone());
        let (bytes, dumped) = dump(&source, TREE);
        let records = [
            dumped.data_records,
            dumped.aux_records,
            dumped.root_key_records,
            dumped.meta_records,
        ];
        assert_eq!(records, [0; 4], "{element:?}");
        assert_eq!(dumped.element, element);
        let target = grove_with(TREE, element);
        assert_eq!(apply(&target, TREE, &bytes), dumped);
        same_grove(&source, &target);
    }
    let empty = grove_with(TREE, Element::empty_tree());
    let root = empty.subtree_root_as(EMPTY_PATH, TREE, &Element::empty_tree(), None, v());
    assert_eq!(root.expect("a root"), NULL_HASH);
}

#[test]
fn the_append_only_trees_are_dumped_and_loaded() {
    type Append = fn(&TempGroveDb, u8);
    let trees: [(Element, Append); 3] = [
        (Element::empty_mmr_tree(), |db, n| {
            db.mmr_tree_append(EMPTY_PATH, TREE, vec![n; 40], None, v())
                .unwrap()
                .expect("an append");
        }),
        (
            Element::empty_bulk_append_tree(2).expect("a chunk power"),
            |db, n| {
                db.bulk_append(EMPTY_PATH, TREE, vec![n; 40], None, v())
                    .unwrap()
                    .expect("an append");
            },
        ),
        (Element::empty_dense_tree(5), |db, n| {
            db.dense_tree_insert(EMPTY_PATH, TREE, vec![n; 40], None, v())
                .unwrap()
                .expect("an append");
        }),
    ];
    for (element, append) in trees {
        let source = grove_with(TREE, element.clone());
        for n in 0..23 {
            append(&source, n);
        }
        let (bytes, dumped) = dump(&source, TREE);
        assert!(dumped.data_records > 0, "{element:?}");
        let target = grove_with(TREE, element.clone());
        assert_eq!(apply(&target, TREE, &bytes), dumped);
        same_grove(&source, &target);
        assert_eq!(dump(&target, TREE).0, bytes, "{element:?}");
        append(&source, 23);
        append(&target, 23);
        same_grove(&source, &target);
    }
}

#[test]
fn the_records_beside_the_nodes_are_dumped_and_loaded() {
    let source = grove_with(TREE, Element::empty_tree());
    for key in [&b"a"[..], b"b", b"c", b"d", b"e"] {
        put_item(&source, TREE, key);
    }
    let tx = source.start_transaction();
    let batch = <DefaultEngine as Engine>::Batch::default();
    let handle = (source.engine())
        .subtree([TREE].as_ref().into(), Some(&batch), &tx)
        .unwrap();
    for n in 0..3u8 {
        (SubtreeHandle::aux(&handle).put([n], &[n; 9], None).unwrap()).expect("an aux record");
        (SubtreeHandle::meta(&handle)
            .put([n, n], &[n; 5], None)
            .unwrap())
        .expect("a meta record");
    }
    let mut change_set = handle.change_set();
    change_set.put_root_key(b"b", None).expect("a root key");
    handle.commit(change_set).unwrap().expect("a change set");
    drop(handle);
    (source.engine().apply_batch(batch, &tx).unwrap()).expect("a batch");
    source.commit_transaction(tx).unwrap().expect("a commit");

    let (bytes, dumped) = dump(&source, TREE);
    let records = [
        dumped.data_records,
        dumped.aux_records,
        dumped.root_key_records,
        dumped.meta_records,
    ];
    assert_eq!(records, [5, 3, 1, 3]);
    let target = grove_with(TREE, Element::empty_tree());
    assert_eq!(apply(&target, TREE, &bytes), dumped);
    same_grove(&source, &target);
    assert_eq!(dump(&target, TREE).0, bytes);

    let tx = target.start_transaction();
    let handle = (target.engine())
        .subtree([TREE].as_ref().into(), None, &tx)
        .unwrap();
    let aux = SubtreeHandle::aux(&handle)
        .get([2u8])
        .unwrap()
        .expect("a read");
    assert_eq!(aux, Some(vec![2; 9]));
    let meta = SubtreeHandle::meta(&handle)
        .get([1u8, 1])
        .unwrap()
        .expect("a read");
    assert_eq!(meta, Some(vec![1; 5]));
    let root_key = handle.root_key().unwrap().expect("a read");
    assert_eq!(root_key, Some(b"b".to_vec()));
}
