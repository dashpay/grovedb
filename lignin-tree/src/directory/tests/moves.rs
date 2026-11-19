//! Moving a Merk with all that is below it (`tree-layout.md` D5).

use super::{super::Step, *};

/// `a` with `a/b`, `a/b/c` and `a/d` below it, and `e` beside it, each
/// with a root.
fn nested() -> Grove {
    let mut grove = Grove::new();
    for keys in [
        &["a"][..],
        &["a", "b"],
        &["a", "b", "c"],
        &["a", "d"],
        &["e"],
    ] {
        grove.add(keys);
        grove.root(keys);
    }
    grove
}

impl Grove {
    /// Moves the Merk at `from` to `to`, and what is below it to the
    /// prefixes of the paths below `to`.
    fn rekey(&mut self, from: &[&str], to: &[&str]) -> Result<u64, DirectoryError> {
        let (from, to) = (path(from), path(to));
        let ((key, position), above) = to.split_last().expect("a path");
        let parent = prefix_of(above);
        let host = Host {
            parent: &parent,
            key,
            position: *position,
        };
        let derive = |steps: &[Step<'_>]| {
            let below = steps.iter().map(|step| (step.key.to_vec(), step.position));
            prefix_of(&to.iter().cloned().chain(below).collect::<Vec<_>>())
        };
        self.directory.rekey(
            &mut self.pages,
            &prefix_of(&from),
            &prefix_of(&to),
            host,
            derive,
        )
    }
}

/// What an entry holds that a move keeps.
fn kept(entry: &Entry) -> (MerkId, RootAt, Vec<u8>) {
    let root = entry.root().map(|root| (root.address, root.key.to_vec()));
    (entry.id(), root, entry.tree_type().to_vec())
}

#[test]
fn a_move_rekeys_every_entry_under_the_moved_key() {
    let mut grove = nested();
    let old = [&["a"][..], &["a", "b"], &["a", "b", "c"], &["a", "d"]];
    let new = [&["z"][..], &["z", "b"], &["z", "b", "c"], &["z", "d"]];
    let before: Vec<Entry> = old.iter().map(|keys| grove.entry(keys)).collect();
    let records = grove.keys(RecordKind::Entry).len();

    assert_eq!(grove.rekey(&["a"], &["z"]).expect("moved"), 4);
    for ((old, new), before) in old.iter().zip(new).zip(&before) {
        assert!(grove.open(old).is_none() && grove.stored(old).is_none());
        let after = grove.entry(new);
        // The same Merk: its id, its root and its type. No cell moved.
        assert_eq!(kept(&after), kept(before));
        assert_eq!(
            after.lineage().collect::<Vec<_>>(),
            before.lineage().collect::<Vec<_>>()
        );
        // It is hosted where its new path says.
        let host = after.host().expect("a host");
        let above = prefix_of(&path(&new[..new.len() - 1]));
        assert_eq!(
            (host.parent, host.key),
            (&above, new[new.len() - 1].as_bytes())
        );
        let root = after.root().expect("a root").address;
        let owner = grove
            .directory
            .root_owner(&grove.pages, root)
            .expect("a lookup");
        assert_eq!(owner, Some(prefix_of(&path(new))));
    }
    assert_eq!(grove.keys(RecordKind::Entry).len(), records);
    let hosted: Vec<Vec<u8>> = grove
        .children(id(1))
        .into_iter()
        .map(|child| child.key)
        .collect();
    assert_eq!(hosted, [b"e", b"z"]);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (6, 0, 0));

    // A Merk that is entered below the moved one afterwards is below it.
    grove.add(&["z", "b", "new"]);
    assert_eq!(grove.entry(&["z", "b", "new"]).depth(), 3);
    // And a drop of the moved Merk leaves what is below it, as before the
    // move.
    grove.unlink(&["z"]);
    for keys in &new[1..] {
        assert!(grove.open(keys).is_none() && grove.stored(keys).is_some());
    }
    assert_eq!(grove.check().left_over, 4);
}

#[test]
fn a_move_below_another_merk_changes_the_merks_above() {
    let mut grove = nested();
    let e = grove.entry(&["e"]).id();
    let (a, b, c) = (
        grove.entry(&["a"]).id(),
        grove.entry(&["a", "b"]).id(),
        grove.entry(&["a", "b", "c"]).id(),
    );
    // `a/b` goes below `e`, one level up, and `c` with it.
    assert_eq!(grove.rekey(&["a", "b"], &["e", "b2"]).expect("moved"), 2);
    let moved = grove.entry(&["e", "b2"]);
    assert_eq!((moved.id(), moved.parent_id()), (b, Some(e)));
    assert_eq!(moved.lineage().collect::<Vec<_>>(), [id(1), e]);
    let below = grove.entry(&["e", "b2", "c"]);
    assert_eq!(below.lineage().collect::<Vec<_>>(), [id(1), e, b]);
    assert_eq!(below.id(), c);
    assert_eq!(grove.children(a).len(), 1);
    assert_eq!(grove.children(e).len(), 1);
    grove.check();
    // It is not below `a` any more: a drop of `a` leaves it in use.
    grove.unlink(&["a"]);
    assert!(grove.open(&["e", "b2", "c"]).is_some());
    assert!(grove.open(&["a", "d"]).is_none());

    // Deeper again, and then to the top with no change of key.
    grove.add(&["e", "x"]);
    grove.root(&["e", "x"]);
    assert_eq!(
        grove.rekey(&["e", "b2"], &["e", "x", "b2"]).expect("moved"),
        2
    );
    assert_eq!(grove.entry(&["e", "x", "b2", "c"]).depth(), 4);
    assert_eq!(grove.rekey(&["e", "x", "b2"], &["b2"]).expect("moved"), 2);
    assert_eq!(
        grove.entry(&["b2", "c"]).lineage().collect::<Vec<_>>(),
        [id(1), b]
    );
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (5, 1, 1));
}

#[test]
fn a_move_is_told_the_steps_down_to_each_merk() {
    let mut grove = nested();
    // A second Merk of the host `b`, at another position.
    let b = prefix_of(&path(&["a", "b"]));
    let second = [(b"a".to_vec(), 0), (b"b".to_vec(), 0), (b"c".to_vec(), 3)];
    let host = Host {
        parent: &b,
        key: b"c",
        position: 3,
    };
    let added = grove
        .directory
        .add(&mut grove.pages, &prefix_of(&second), b"axis", Some(host));
    let added = added.expect("entered");

    let top = prefix_of(&[]);
    let to = [(b"z".to_vec(), 0)];
    let host = Host {
        parent: &top,
        key: b"z",
        position: 0,
    };
    let mut told = Vec::new();
    let derive = |steps: &[Step<'_>]| {
        let below: Vec<Seg> = steps
            .iter()
            .map(|step| (step.key.to_vec(), step.position))
            .collect();
        told.push(below.clone());
        prefix_of(&to.iter().cloned().chain(below).collect::<Vec<_>>())
    };
    let from = prefix_of(&path(&["a"]));
    let moved = grove
        .directory
        .rekey(&mut grove.pages, &from, &prefix_of(&to), host, derive);
    assert_eq!(moved.expect("moved"), 5);
    let step = |key: &str, position: u8| (key.as_bytes().to_vec(), position);
    assert_eq!(
        told,
        [
            vec![step("b", 0)],
            vec![step("b", 0), step("c", 0)],
            vec![step("b", 0), step("c", 3)],
            vec![step("d", 0)],
        ]
    );
    let moved = [(b"z".to_vec(), 0), (b"b".to_vec(), 0), (b"c".to_vec(), 3)];
    let entry = grove
        .directory
        .open(&grove.pages, &prefix_of(&moved))
        .expect("a lookup");
    let entry = entry.expect("the Merk at the other position");
    assert_eq!((entry.id(), entry.tree_type()), (added, &b"axis"[..]));
    assert_eq!(entry.host().map(|host| host.position), Some(3));
    grove.check();
}

#[test]
fn a_move_takes_the_place_of_what_a_drop_left() {
    let mut grove = nested();
    for keys in [&["z"][..], &["z", "b"], &["z", "b", "c"]] {
        grove.add(keys);
        grove.root(keys);
    }
    let old_root = grove.entry(&["z", "b"]).root().expect("a root").address;
    let dropped = grove.unlink(&["z"]);
    assert!(grove.stored(&["z", "b"]).is_some());

    // `a` moves to where `z` was: its Merks take the places of the entries
    // that `z` left, and have nothing of them.
    let b = grove.entry(&["a", "b"]);
    assert_eq!(grove.rekey(&["a"], &["z"]).expect("moved"), 4);
    assert_eq!(kept(&grove.entry(&["z", "b"])), kept(&b));
    assert_eq!(
        grove
            .directory
            .root_owner(&grove.pages, old_root)
            .expect("a lookup"),
        None
    );
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (6, 0, 1));
    // The reclaimer finds the records of the old hosts, and takes them.
    assert_eq!(grove.reclaim(dropped.id), 2);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (6, 0, 0));
    assert_eq!(kept(&grove.entry(&["z", "b"])), kept(&b));
}

#[test]
fn a_merk_with_no_host_can_be_given_one() {
    let mut grove = nested();
    let alone = [0xEE; 32];
    let added = grove
        .directory
        .add(&mut grove.pages, &alone, b"alone", None)
        .expect("entered");
    let top = prefix_of(&[]);
    let to = prefix_of(&path(&["hosted"]));
    let host = Host {
        parent: &top,
        key: b"hosted",
        position: 0,
    };
    let none = |_: &[Step<'_>]| unreachable!("nothing is below it");
    let moved = grove
        .directory
        .rekey(&mut grove.pages, &alone, &to, host, none);
    assert_eq!(moved.expect("moved"), 1);
    let entry = grove.entry(&["hosted"]);
    assert_eq!((entry.id(), entry.parent_id()), (added, Some(id(1))));
    assert_eq!(entry.root(), None);
    grove.check();
}

#[test]
fn a_move_that_cannot_be_is_refused_and_writes_nothing() {
    let mut grove = nested();
    grove.add(&["bare"]);
    let pages = grove.pages.pages.clone();
    let refused: [(&[&str], &[&str], &str); 8] = [
        (&["nothing"], &["z"], "no Merk is entered under the prefix"),
        (&["a"], &["e"], "a Merk is entered under the prefix"),
        (&["a"], &["a"], "a Merk is entered under the prefix"),
        (
            &["a"],
            &["nothing", "z"],
            "the host's Merk, under the prefix",
        ),
        (&["a"], &["a", "z"], "would move below itself"),
        (&["a"], &["a", "b", "c", "z"], "would move below itself"),
        (&["e"], &["a", "b"], "a Merk is entered under the prefix"),
        (
            &["a", "b"],
            &["bare", &"k".repeat(256)],
            "a host's key of 256 bytes",
        ),
    ];
    for (from, to, says) in refused {
        let error = grove.rekey(from, to).expect_err("refused");
        assert!(
            error.to_string().contains(says),
            "{from:?} to {to:?}: {error}"
        );
        assert_eq!(grove.pages.pages, pages, "a refused move writes nothing");
    }

    // The host has a Merk at the position, entered under another prefix
    // than the one the move goes to.
    let top = prefix_of(&[]);
    let host = Host {
        parent: &top,
        key: b"e",
        position: 0,
    };
    let from = prefix_of(&path(&["a"]));
    let none = |_: &[Step<'_>]| unreachable!("the move is refused");
    let error = grove
        .directory
        .rekey(&mut grove.pages, &from, &[0xDD; 32], host, none);
    let error = error.expect_err("the host is taken");
    assert!(
        matches!(error, DirectoryError::HostTaken { parent, position: 0 } if parent == top),
        "{error}"
    );
    assert_eq!(grove.pages.pages, pages);
    // A left-over Merk is moved by nobody.
    grove.unlink(&["a"]);
    let error = grove.rekey(&["a", "b"], &["z"]).expect_err("left over");
    assert!(matches!(error, DirectoryError::NotFound { .. }), "{error}");
}

#[test]
fn a_move_onto_a_merk_in_use_below_the_moved_key_stops_half_way() {
    let mut grove = nested();
    let top = prefix_of(&[]);
    let host = Host {
        parent: &top,
        key: b"z",
        position: 0,
    };
    // The caller's prefixes are wrong: the Merk `b` is said to go where
    // `e` is.
    let e = prefix_of(&path(&["e"]));
    let from = prefix_of(&path(&["a"]));
    let to = prefix_of(&path(&["z"]));
    let moved = grove
        .directory
        .rekey(&mut grove.pages, &from, &to, host, |_| e);
    let error = moved.expect_err("a Merk is there");
    assert!(
        matches!(error, DirectoryError::Exists { prefix } if prefix == e),
        "{error}"
    );
    // The pages are no directory now, and their writer gives them up.
    assert!(grove.directory.check(&grove.pages).is_err());
}

#[test]
fn a_move_whose_records_are_not_there_is_damage() {
    let mut grove = nested();
    let tree = *grove.directory.tree();
    let b = prefix_of(&path(&["a", "b"]));
    let key = super::super::record::entry_key(&b);
    assert!(tree.delete(&mut grove.pages, &key).expect("taken out"));
    let error = grove.rekey(&["a"], &["z"]).expect_err("damage");
    assert!(
        matches!(&error, DirectoryError::Damaged { key: at, what }
            if at[..] == key && *what == "a Merk in use hosts a Merk that has no entry"),
        "{error}"
    );
}
