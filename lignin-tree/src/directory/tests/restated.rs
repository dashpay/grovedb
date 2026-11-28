//! Stating a Merk anew: another tree type, and another place among the
//! Merks of its host, for a tree that is converted in place.

use super::{super::Restated, *};

/// The grove's root with an element `host` that hosts three Merks, at the
/// positions 0, 1 and 2, each with a root, and a Merk below the second.
fn hosted() -> (Grove, [Prefix; 3]) {
    let mut grove = Grove::new();
    grove.root(&[]);
    let top = prefix_of(&[]);
    let prefixes = [0u8, 1, 2].map(|position| prefix_of(&[(b"host".to_vec(), position)]));
    for (position, prefix) in prefixes.iter().enumerate() {
        let host = Host {
            parent: &top,
            key: b"host",
            position: position as u8,
        };
        let tree_type = [b't', position as u8];
        (grove.directory)
            .add(&mut grove.pages, prefix, &tree_type, Some(host))
            .expect("entered");
        grove.next_cell += 1;
        let root = named(prefix, Some((cell(9, grove.next_cell), b"root")));
        (grove.directory)
            .set_roots(&mut grove.pages, &[root])
            .expect("a root");
    }
    let below = Host {
        parent: &prefixes[1],
        key: b"root",
        position: 0,
    };
    let nested = prefix_of(&[(b"host".to_vec(), 1), (b"root".to_vec(), 0)]);
    (grove.directory)
        .add(&mut grove.pages, &nested, b"nested", Some(below))
        .expect("entered");
    (grove, prefixes)
}

fn open(grove: &Grove, prefix: &Prefix) -> Entry {
    let entry = grove.directory.open(&grove.pages, prefix);
    entry.expect("a lookup").expect("the Merk is in use")
}

/// What an entry holds beside its tree type and its position.
fn kept(entry: &Entry) -> (MerkId, RootAt, Prefix, Vec<u8>, Vec<MerkId>) {
    let root = entry.root().map(|root| (root.address, root.key.to_vec()));
    let host = entry.host().expect("a hosted Merk");
    let lineage = entry.lineage().collect();
    (entry.id(), root, *host.parent, host.key.to_vec(), lineage)
}

fn at(entry: &Entry) -> u8 {
    entry.host().expect("a hosted Merk").position
}

fn restated<'a>(prefix: &'a Prefix, tree_type: &'a [u8], position: Option<u8>) -> Restated<'a> {
    Restated {
        prefix,
        tree_type,
        position,
    }
}

#[test]
fn a_restated_merk_has_its_new_type_and_place_and_everything_else_as_it_was() {
    let (mut grove, [first, second, third]) = hosted();
    let before = [first, second, third].map(|prefix| open(&grove, &prefix));
    let top = open(&grove, &prefix_of(&[])).id();
    let nested = prefix_of(&[(b"host".to_vec(), 1), (b"root".to_vec(), 0)]);
    let nested_before = open(&grove, &nested);

    // The second and the third trade places, each with another tree
    // type, and the first is given another type where it is.
    let directory = grove.directory.clone();
    let changes = [
        restated(&first, b"wide", None),
        restated(&second, b"moved up", Some(2)),
        restated(&third, b"moved down", Some(1)),
    ];
    directory
        .restate(&mut grove.pages, &changes)
        .expect("restated");

    let after = [first, second, third].map(|prefix| open(&grove, &prefix));
    for (was, now) in before.iter().zip(&after) {
        assert_eq!(kept(now), kept(was));
    }
    let types: Vec<&[u8]> = after.iter().map(Entry::tree_type).collect();
    assert_eq!(types, [&b"wide"[..], b"moved up", b"moved down"]);
    assert_eq!(after.each_ref().map(at), [0, 2, 1]);

    // The host's records name each Merk at its new place.
    let children: Vec<(u8, Prefix, MerkId)> = (grove.children(top).into_iter())
        .map(|child| (child.position, child.prefix, child.id))
        .collect();
    assert_eq!(
        children,
        [
            (0, first, after[0].id()),
            (1, third, after[2].id()),
            (2, second, after[1].id()),
        ]
    );
    // What is below a Merk that moved is below it still.
    assert_eq!(open(&grove, &nested), nested_before);
    let below: Vec<Prefix> = (grove.children(after[1].id()).into_iter())
        .map(|child| child.prefix)
        .collect();
    assert_eq!(below, [nested]);
    let census = grove.check();
    assert_eq!((census.live, census.left_over), (5, 0));

    // The same statement again writes nothing, and so does one that
    // states what an entry holds.
    let revision = grove.pages.revision;
    let same = [
        restated(&first, b"wide", Some(0)),
        restated(&second, b"moved up", None),
    ];
    directory
        .restate(&mut grove.pages, &same)
        .expect("the same");
    directory.restate(&mut grove.pages, &[]).expect("nothing");
    assert_eq!(grove.pages.revision, revision);

    // A Merk moves to a place that no Merk has.
    directory
        .restate(&mut grove.pages, &[restated(&third, b"far", Some(9))])
        .expect("a free place");
    assert_eq!(at(&open(&grove, &third)), 9);
    assert_eq!(grove.check().live, 5);

    // A Merk with no host is given another tree type.
    let root = prefix_of(&[]);
    directory
        .restate(&mut grove.pages, &[restated(&root, b"big", None)])
        .expect("the root of the grove");
    assert_eq!(open(&grove, &root).tree_type(), b"big");
}

#[test]
fn a_statement_that_is_refused_writes_nothing() {
    let (mut grove, [first, second, third]) = hosted();
    let directory = grove.directory.clone();
    let root = prefix_of(&[]);
    let unknown = prefix_of(&path(&["nowhere"]));
    let long = [7; 256];
    let refused: Vec<(Vec<Restated<'_>>, &str)> = vec![
        (
            vec![restated(&first, b"a", None), restated(&first, b"b", None)],
            "Twice",
        ),
        (vec![restated(&unknown, b"a", None)], "NotFound"),
        (vec![restated(&root, b"a", Some(0))], "NoParent"),
        // The place of a Merk that stays where it is.
        (vec![restated(&second, b"a", Some(2))], "HostTaken"),
        // Two Merks to one place, which one of them leaves.
        (
            vec![
                restated(&second, b"a", Some(0)),
                restated(&third, b"a", Some(0)),
                restated(&first, b"a", Some(1)),
            ],
            "HostTaken",
        ),
        (
            vec![restated(&first, b"a", None), restated(&second, &long, None)],
            "TooLong",
        ),
    ];
    let records = |grove: &Grove| RecordKind::ALL.map(|kind| grove.keys(kind));
    let before = records(&grove);
    let entries = [first, second, third].map(|prefix| open(&grove, &prefix));
    let revision = grove.pages.revision;
    for (changes, expected) in refused {
        let error = directory
            .restate(&mut grove.pages, &changes)
            .expect_err("refused");
        let found = match error {
            DirectoryError::Twice { prefix } if prefix == first => "Twice",
            DirectoryError::NotFound { prefix } if prefix == unknown => "NotFound",
            DirectoryError::NoParent { parent } if parent == root => "NoParent",
            DirectoryError::HostTaken { parent, .. } if parent == root => "HostTaken",
            DirectoryError::TooLong { len: 256, .. } => "TooLong",
            other => panic!("{other:?}"),
        };
        assert_eq!(found, expected);
        assert_eq!(grove.pages.revision, revision, "{expected}");
    }
    assert_eq!(records(&grove), before);
    assert_eq!(
        [first, second, third].map(|prefix| open(&grove, &prefix)),
        entries
    );

    // A Merk below a dropped one is in use no more, and is not restated.
    let nested = prefix_of(&[(b"host".to_vec(), 1), (b"root".to_vec(), 0)]);
    (grove.directory)
        .unlink(&mut grove.pages, &second)
        .expect("dropped");
    let directory = grove.directory.clone();
    let gone = directory.restate(&mut grove.pages, &[restated(&nested, b"a", None)]);
    assert!(matches!(gone, Err(DirectoryError::NotFound { prefix }) if prefix == nested));
    // The place the dropped Merk had is free.
    directory
        .restate(&mut grove.pages, &[restated(&third, b"a", Some(1))])
        .expect("the place of a dropped Merk");
    assert_eq!(at(&open(&grove, &third)), 1);
}
