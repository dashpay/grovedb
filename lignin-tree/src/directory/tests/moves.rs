//! Moving a Merk with all that is below it (`tree-layout.md` D5).

use super::{
    super::{Rekey, RekeyPlan, Rekeyed, Step},
    *,
};

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

    // The host has a Merk at the position: another Merk, or the moved
    // Merk itself, which a move does not give another prefix where it is.
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
    let host = Host {
        parent: &top,
        key: b"a",
        position: 0,
    };
    let none = |_: &[Step<'_>]| unreachable!("the move is refused");
    let error = grove
        .directory
        .rekey(&mut grove.pages, &from, &[0xDD; 32], host, none);
    assert!(matches!(error, Err(DirectoryError::HostTaken { .. })));
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

/// One move by pairs: the Merk at a path goes to another key of the Merk
/// that hosts it, and the Merks below it that are named go with it.
struct Pairs {
    from: Prefix,
    to: Prefix,
    key: Vec<u8>,
    below: Vec<(Prefix, Prefix)>,
}

/// The move of the Merk at `from` to the key `key`, which names the Merks
/// at `below`, each by its keys below the moved one.
fn pairs(from: &[&str], key: &str, below: &[&[&str]]) -> Pairs {
    let mut to = from[..from.len() - 1].to_vec();
    to.push(key);
    let named = |keys: &&[&str]| {
        let old = prefix_of(&path(&[from, keys].concat()));
        let new = prefix_of(&path(&[&to[..], keys].concat()));
        (old, new)
    };
    Pairs {
        from: prefix_of(&path(from)),
        to: prefix_of(&path(&to)),
        key: key.as_bytes().to_vec(),
        below: below.iter().map(named).collect(),
    }
}

/// An error as it is shown: the errors of the directory are not compared.
fn shown(error: &DirectoryError) -> String {
    format!("{error:?}")
}

/// Every Merk below `a` in [`nested`], by its keys below it.
const BELOW_A: [&[&str]; 3] = [&["b"], &["b", "c"], &["d"]];

impl Grove {
    fn plan(
        &self,
        moves: &[Pairs],
        leaves: &dyn Fn(&Entry) -> bool,
    ) -> Result<RekeyPlan, DirectoryError> {
        let moves: Vec<Rekey<'_>> = (moves.iter())
            .map(|pairs| Rekey {
                from: &pairs.from,
                to: &pairs.to,
                key: &pairs.key,
                below: &pairs.below,
            })
            .collect();
        self.directory.plan_rekey(&self.pages, &moves, leaves)
    }

    fn rekey_pairs(&mut self, moves: &[Pairs]) -> Result<Rekeyed, DirectoryError> {
        let plan = self.plan(moves, &|_| false)?;
        self.directory.rekey_pairs(&mut self.pages, &plan)
    }

    fn root_owner(&self, root: CellAddress) -> Option<Prefix> {
        (self.directory.root_owner(&self.pages, root)).expect("a lookup")
    }

    /// Every record of the directory, with its value.
    fn records(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut cursor = self.directory.tree().cursor();
        let mut out = Vec::new();
        cursor.seek_to_first(&self.pages).expect("a walk");
        while let Some(key) = cursor.key() {
            let mut value = Vec::new();
            assert!(cursor.value(&self.pages, &mut value).expect("a value"));
            out.push((key.to_vec(), value));
            cursor.next(&self.pages).expect("a walk");
        }
        out
    }
}

#[test]
fn a_rekey_by_pairs_gives_each_merk_the_prefix_it_is_named() {
    let mut grove = nested();
    let old = [&["a"][..], &["a", "b"], &["a", "b", "c"], &["a", "d"]];
    let new = [&["z"][..], &["z", "b"], &["z", "b", "c"], &["z", "d"]];
    let before: Vec<Entry> = old.iter().map(|keys| grove.entry(keys)).collect();

    let moved = grove.rekey_pairs(&[pairs(&["a"], "z", &BELOW_A)]);
    assert_eq!(moved.expect("moved"), Rekeyed::default());
    for ((old, new), before) in old.iter().zip(new).zip(&before) {
        assert!(grove.open(old).is_none() && grove.stored(old).is_none());
        let after = grove.entry(new);
        assert_eq!(kept(&after), kept(before));
        assert_eq!(
            after.lineage().collect::<Vec<_>>(),
            before.lineage().collect::<Vec<_>>()
        );
        let host = after.host().expect("a host");
        let above = prefix_of(&path(&new[..new.len() - 1]));
        assert_eq!(
            (host.parent, host.key, host.position),
            (&above, new[new.len() - 1].as_bytes(), 0)
        );
        let root = after.root().expect("a root").address;
        assert_eq!(grove.root_owner(root), Some(prefix_of(&path(new))));
    }
    let hosted: Vec<Vec<u8>> = (grove.children(id(1)).into_iter())
        .map(|child| child.key)
        .collect();
    assert_eq!(hosted, [b"e", b"z"]);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (6, 0, 0));

    // The directory is, record for record, the one that the move by the
    // steps down leaves.
    let mut by_steps = nested();
    assert_eq!(by_steps.rekey(&["a"], &["z"]).expect("moved"), 4);
    assert_eq!(grove.records(), by_steps.records());
    // A pair for a Merk that is not below the moved one says nothing.
    let mut again = nested();
    let mut named = pairs(&["a"], "z", &BELOW_A);
    named.below.push(([0x31; 32], [0x32; 32]));
    let e = prefix_of(&path(&["e"]));
    named.below.push((e, [0x33; 32]));
    again.rekey_pairs(&[named]).expect("moved");
    assert_eq!(again.records(), by_steps.records());
}

#[test]
fn two_merks_change_places_with_the_merks_below_them() {
    let mut grove = nested();
    for keys in [&["e", "b"][..], &["e", "q"]] {
        grove.add(keys);
        grove.root(keys);
    }
    let id_at = |grove: &Grove, keys: &[&str]| grove.entry(keys).id();
    let at = [
        &["a"][..],
        &["a", "b"],
        &["a", "b", "c"],
        &["a", "d"],
        &["e"],
        &["e", "b"],
        &["e", "q"],
    ];
    let ids: Vec<MerkId> = at.iter().map(|keys| id_at(&grove, keys)).collect();
    let records = grove.records();

    let swap = |grove: &mut Grove, a: &[&[&str]], e: &[&[&str]]| {
        let moves = [pairs(&["a"], "e", a), pairs(&["e"], "a", e)];
        let moved = grove.rekey_pairs(&moves);
        assert_eq!(moved.expect("moved"), Rekeyed::default());
    };
    swap(&mut grove, &BELOW_A, &[&["b"], &["q"]]);
    // Each is where the other was, with what was below it under the
    // other's key.
    let now = [
        &["e"][..],
        &["e", "b"],
        &["e", "b", "c"],
        &["e", "d"],
        &["a"],
        &["a", "b"],
        &["a", "q"],
    ];
    for (keys, id) in now.iter().zip(&ids) {
        let entry = grove.entry(keys);
        assert_eq!(entry.id(), *id);
        let host = entry.host().expect("a host");
        let above = prefix_of(&path(&keys[..keys.len() - 1]));
        assert_eq!(
            (host.parent, host.key),
            (&above, keys[keys.len() - 1].as_bytes())
        );
        let root = entry.root().expect("a root").address;
        assert_eq!(grove.root_owner(root), Some(prefix_of(&path(keys))));
    }
    for gone in [&["a", "b", "c"][..], &["a", "d"], &["e", "q"]] {
        assert!(grove.stored(gone).is_none());
    }
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (8, 0, 0));
    // And back: every record is as it was.
    swap(&mut grove, &[&["b"], &["q"]], &BELOW_A);
    assert_eq!(grove.records(), records);

    // A chain: `a` goes where `e` is, and `e` to a key that is free.
    let (a, e) = (ids[0], ids[4]);
    let moves = [
        pairs(&["a"], "e", &BELOW_A),
        pairs(&["e"], "f", &[&["b"], &["q"]]),
    ];
    grove.rekey_pairs(&moves).expect("moved");
    assert_eq!((id_at(&grove, &["e"]), id_at(&grove, &["f"])), (a, e));
    assert!(grove.stored(&["a"]).is_none());
    assert_eq!(id_at(&grove, &["f", "q"]), ids[6]);
    assert_eq!(grove.check().live, 8);
}

#[test]
fn a_merk_with_no_node_that_is_not_named_leaves_with_the_move() {
    let build = |rooted: bool| {
        let mut grove = nested();
        let x = grove.add(&["a", "x"]);
        // What a tree that was emptied and deleted leaves: an entry with
        // no node, and one below it.
        let (above, below) = (
            prefix_of(&path(&["a", "x"])),
            prefix_of(&path(&["a", "x", "y"])),
        );
        let host = Host {
            parent: &above,
            key: b"y",
            position: 0,
        };
        let y = (grove.directory).add(&mut grove.pages, &below, b"tree", Some(host));
        if rooted {
            grove.root(&["a", "x", "y"]);
        }
        (grove, x, y.expect("entered"))
    };
    let (mut grove, x, y) = build(false);
    let moves = [pairs(&["a"], "z", &BELOW_A)];
    let plan = grove.plan(&moves, &|_| false).expect("a plan");
    let left = [
        prefix_of(&path(&["a", "x"])),
        prefix_of(&path(&["a", "x", "y"])),
    ];
    assert_eq!(plan.left(), left);
    assert_eq!(plan.moved().count(), 4);
    let rekeyed = (grove.directory.rekey_pairs(&mut grove.pages, &plan)).expect("moved");
    // The innermost first: a Merk with no node is dropped when it hosts
    // none.
    let dropped = |id| Dropped { id, root: None };
    assert_eq!(rekeyed.left, [dropped(y), dropped(x)]);
    for keys in [&["a", "x"][..], &["a", "x", "y"], &["z", "x"]] {
        assert!(grove.stored(keys).is_none());
    }
    assert!(grove.open(&["z", "d"]).is_some());
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (6, 0, 2));
    assert_eq!(grove.reclaim(y) + grove.reclaim(x), 0);
    assert_eq!(grove.check().dropped, 0);

    // One that has a node is named, or the move is refused: also below a
    // Merk that would leave.
    let unnamed = |prefix| shown(&DirectoryError::Unnamed { prefix });
    let refused = |plan: Result<RekeyPlan, DirectoryError>| shown(&plan.expect_err("refused"));
    let (grove, ..) = build(true);
    assert_eq!(refused(grove.plan(&moves, &|_| false)), unnamed(left[1]));
    // A Merk that is named stays, so the Merk above it does not leave.
    for rooted in [false, true] {
        let (grove, ..) = build(rooted);
        let named = [&["b"][..], &["b", "c"], &["d"], &["x", "y"]];
        let plan = grove.plan(&[pairs(&["a"], "z", &named)], &|_| false);
        assert_eq!(refused(plan), unnamed(left[0]));
    }
    let grove = nested();
    let plan = grove.plan(&[pairs(&["a"], "z", &[&["b"], &["d"]])], &|_| false);
    let c = prefix_of(&path(&["a", "b", "c"]));
    assert_eq!(refused(plan), unnamed(c));
}

#[test]
fn a_plan_is_refused_where_the_directory_cannot_take_it() {
    let mut grove = nested();
    let records = grove.records();
    let refused = |grove: &Grove, moves: &[Pairs]| {
        let plan = grove.plan(moves, &|_| false);
        shown(&plan.expect_err("refused"))
    };
    let prefix = |keys: &[&str]| prefix_of(&path(keys));
    assert_eq!(
        refused(&grove, &[pairs(&["nope"], "z", &[])]),
        shown(&DirectoryError::NotFound {
            prefix: prefix(&["nope"])
        })
    );
    // The root of the grove has no host to get another key of.
    let top = Pairs {
        from: prefix_of(&[]),
        to: [0x44; 32],
        key: b"k".to_vec(),
        below: Vec::new(),
    };
    assert_eq!(
        refused(&grove, &[top]),
        shown(&DirectoryError::NoHost {
            prefix: prefix_of(&[])
        })
    );
    // A Merk is taken from its prefix once, and one goes to a prefix.
    let twice = [pairs(&["a"], "z", &BELOW_A), pairs(&["a"], "y", &BELOW_A)];
    assert_eq!(
        refused(&grove, &twice),
        shown(&DirectoryError::Twice {
            prefix: prefix(&["a"])
        })
    );
    let twice = [
        pairs(&["a"], "z", &BELOW_A),
        pairs(&["a", "b"], "y", &[&["c"]]),
    ];
    assert_eq!(
        refused(&grove, &twice),
        shown(&DirectoryError::Twice {
            prefix: prefix(&["a", "b"])
        })
    );
    let twice = [pairs(&["a"], "z", &BELOW_A), pairs(&["e"], "z", &[])];
    assert_eq!(
        refused(&grove, &twice),
        shown(&DirectoryError::Twice {
            prefix: prefix(&["z"])
        })
    );
    // A prefix that a Merk in use has is not free, and neither is a host
    // that has a Merk at the position.
    assert_eq!(
        refused(&grove, &[pairs(&["a"], "e", &BELOW_A)]),
        shown(&DirectoryError::Exists {
            prefix: prefix(&["e"])
        })
    );
    let mut elsewhere = pairs(&["a"], "e", &BELOW_A);
    elsewhere.to = [0x45; 32];
    assert_eq!(
        refused(&grove, &[elsewhere]),
        shown(&DirectoryError::HostTaken {
            parent: prefix_of(&[]),
            position: 0
        })
    );
    assert_eq!(grove.records(), records);

    // A Merk that the same write drops first is in nobody's way, and one
    // that it drops is not moved.
    let (a, e) = (grove.entry(&["a"]).id(), grove.entry(&["e"]).id());
    let moves = [pairs(&["a"], "e", &BELOW_A)];
    let plan = grove.plan(&moves, &|entry| entry.id() == a);
    assert_eq!(
        shown(&plan.expect_err("refused")),
        shown(&DirectoryError::NotFound {
            prefix: prefix(&["a"])
        })
    );
    let plan = grove
        .plan(&moves, &|entry| entry.id() == e)
        .expect("a plan");
    // A plan that is applied with that Merk still there is refused.
    let mut early = nested();
    let stopped = early.directory.rekey_pairs(&mut early.pages, &plan);
    assert_eq!(
        shown(&stopped.expect_err("refused")),
        shown(&DirectoryError::Exists {
            prefix: prefix(&["e"])
        })
    );
    grove.unlink(&["e"]);
    let rekeyed = grove.directory.rekey_pairs(&mut grove.pages, &plan);
    assert_eq!(rekeyed.expect("moved"), Rekeyed::default());
    assert_eq!(grove.entry(&["e"]).id(), a);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (5, 0, 1));
}

#[test]
fn a_rekey_by_pairs_displaces_what_a_drop_left() {
    let mut grove = nested();
    for keys in [&["u"][..], &["u", "v"]] {
        grove.add(keys);
        grove.root(keys);
    }
    let left = grove.entry(&["u", "v"]);
    let was = left.root().expect("a root").address;
    let dropped = grove.unlink(&["u"]);
    assert!(grove.open(&["u", "v"]).is_none() && grove.stored(&["u", "v"]).is_some());

    let e = grove.entry(&["e"]);
    let mut moved = pairs(&["e"], "w", &[]);
    moved.to = prefix_of(&path(&["u", "v"]));
    let rekeyed = grove.rekey_pairs(&[moved]).expect("moved");
    assert_eq!(rekeyed.displaced, [prefix_of(&path(&["u", "v"]))]);
    assert!(rekeyed.left.is_empty());
    // The Merk in use has the prefix now, and the root of the one that
    // was left leads to nothing.
    let now = grove.entry(&["u", "v"]);
    assert_eq!(kept(&now), kept(&e));
    assert_eq!(now.host().expect("a host").key, b"w");
    assert_eq!(grove.root_owner(was), None);
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (6, 0, 1));
    // The reclaimer finds what the dropped Merk left by the record of the
    // host, and leaves the entry that is another Merk's.
    grove.reclaim(dropped.id);
    assert_eq!(kept(&grove.entry(&["u", "v"])), kept(&e));
    let census = grove.check();
    assert_eq!((census.live, census.left_over, census.dropped), (6, 0, 0));
}
