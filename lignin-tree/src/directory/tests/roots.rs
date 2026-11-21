//! The roots a write changed, as the directory is told them
//! (`tree-layout.md` S3).

use super::{super::record::root_key, *};

fn carried(from: CellAddress, to: CellAddress) -> RootChange<'static> {
    RootChange::Carried { from, to }
}

impl Grove {
    fn set(&mut self, changes: &[RootChange<'_>]) -> Result<(), DirectoryError> {
        self.directory.set_roots(&mut self.pages, changes)
    }

    fn root_of(&self, keys: &[&str]) -> RootAt {
        let entry = self.entry(keys);
        entry.root().map(|root| (root.address, root.key.to_vec()))
    }

    fn owner(&self, address: CellAddress) -> Option<Prefix> {
        self.directory
            .root_owner(&self.pages, address)
            .expect("a lookup")
    }
}

#[test]
fn a_merk_is_named_with_its_root() {
    let mut grove = Grove::new();
    grove.add(&["a"]);
    let prefix = prefix_of(&path(&["a"]));
    assert_eq!(grove.root_of(&["a"]), None);
    assert!(
        grove.keys(RecordKind::Root).len() == 1,
        "the root of the grove"
    );

    // Its first node.
    grove
        .set(&[named(&prefix, Some((cell(4, 1), b"m")))])
        .expect("a root");
    assert_eq!(grove.root_of(&["a"]), Some((cell(4, 1), b"m".to_vec())));
    assert_eq!(grove.owner(cell(4, 1)), Some(prefix));
    // Another node becomes the root, in another cell.
    grove
        .set(&[named(&prefix, Some((cell(4, 2), b"k")))])
        .expect("a root");
    assert_eq!(grove.root_of(&["a"]), Some((cell(4, 2), b"k".to_vec())));
    assert_eq!(
        (grove.owner(cell(4, 1)), grove.owner(cell(4, 2))),
        (None, Some(prefix))
    );
    // Another node in the cell the root was in: only the entry is written.
    grove
        .set(&[named(&prefix, Some((cell(4, 2), b"j")))])
        .expect("a root");
    assert_eq!(grove.root_of(&["a"]), Some((cell(4, 2), b"j".to_vec())));
    // The same root writes nothing.
    let revision = grove.pages.revision;
    grove
        .set(&[named(&prefix, Some((cell(4, 2), b"j")))])
        .expect("a root");
    grove.set(&[]).expect("no change");
    assert_eq!(grove.pages.revision, revision);
    grove.check();
    // Its last node goes.
    grove.set(&[named(&prefix, None)]).expect("no root");
    assert_eq!(
        (grove.root_of(&["a"]), grove.owner(cell(4, 2))),
        (None, None)
    );
    assert_eq!(grove.keys(RecordKind::Root).len(), 1);
    grove.check();
}

#[test]
fn a_carried_root_is_found_by_the_address_it_had() {
    let mut grove = Grove::new();
    grove.add(&["a"]);
    grove.add(&["a", "b"]);
    let was = grove.root(&["a", "b"]);
    let prefix = prefix_of(&path(&["a", "b"]));
    let before = grove.entry(&["a", "b"]);

    // The write that carried it did not open the Merk: it has the address
    // the cell left, and the one it is at.
    grove.set(&[carried(was, cell(20, 3))]).expect("carried");
    let after = grove.entry(&["a", "b"]);
    assert_eq!(
        grove.root_of(&["a", "b"]),
        Some((cell(20, 3), b"root".to_vec()))
    );
    assert_eq!(
        (grove.owner(was), grove.owner(cell(20, 3))),
        (None, Some(prefix))
    );
    assert_eq!((after.id(), after.host()), (before.id(), before.host()));
    assert_eq!(after.tree_type(), before.tree_type());
    grove.check();

    // A Merk that is stated in both ways has the root it is named with.
    let both = [
        named(&prefix, Some((cell(21, 1), b"other"))),
        carried(cell(20, 3), cell(20, 9)),
    ];
    grove.set(&both).expect("named");
    assert_eq!(
        grove.root_of(&["a", "b"]),
        Some((cell(21, 1), b"other".to_vec()))
    );
    assert_eq!(grove.owner(cell(20, 9)), None);
    grove.check();
}

#[test]
fn a_root_takes_the_cell_another_root_left_in_the_same_write() {
    for order in [[0, 1, 2], [2, 1, 0], [1, 2, 0], [1, 0, 2]] {
        let mut grove = Grove::new();
        for name in ["a", "b", "c"] {
            grove.add(&[name]);
        }
        let at = ["a", "b", "c"].map(|name| grove.root(&[name]));
        let c = prefix_of(&path(&["c"]));
        // `a` goes to a new cell, `b` to the cell `a` left, and `c` is
        // named with the cell `b` left.
        let changes = [
            carried(at[0], cell(30, 1)),
            carried(at[1], at[0]),
            named(&c, Some((at[1], b"c-root"))),
        ];
        grove
            .set(&order.map(|index| changes[index]))
            .expect("a change");
        assert_eq!(grove.root_of(&["a"]), Some((cell(30, 1), b"root".to_vec())));
        assert_eq!(grove.root_of(&["b"]), Some((at[0], b"root".to_vec())));
        assert_eq!(grove.root_of(&["c"]), Some((at[1], b"c-root".to_vec())));
        assert_eq!(grove.owner(at[0]), Some(prefix_of(&path(&["b"]))));
        assert_eq!(grove.owner(at[1]), Some(c));
        assert_eq!(grove.owner(at[2]), None);
        assert_eq!(grove.keys(RecordKind::Root).len(), 4);
        grove.check();

        // Two roots change places.
        grove
            .set(&[carried(at[0], at[1]), carried(at[1], at[0])])
            .expect("a swap");
        assert_eq!(grove.root_of(&["b"]).map(|root| root.0), Some(at[1]));
        assert_eq!(grove.root_of(&["c"]).map(|root| root.0), Some(at[0]));
        grove.check();
    }
}

#[test]
fn a_cell_that_became_a_root_is_named() {
    let mut grove = Grove::new();
    grove.add(&["a"]);
    let was = grove.root(&["a"]);
    let prefix = prefix_of(&path(&["a"]));
    let pages = grove.pages.pages.clone();

    // The cell at 40/1 was no root. It is one now, and was carried in the
    // write that made it one: the address it had says nothing.
    let error = grove
        .set(&[carried(cell(40, 1), cell(41, 1))])
        .expect_err("no Merk");
    assert!(
        matches!(error, DirectoryError::UnknownRoot { address } if address == cell(40, 1)),
        "{error}"
    );
    // Its Merk is named in the same change, with the cell it is in.
    let wrong = [
        carried(cell(40, 1), cell(41, 1)),
        named(&prefix, Some((cell(41, 2), b"k"))),
    ];
    let error = grove.set(&wrong).expect_err("named with another cell");
    assert!(
        matches!(error, DirectoryError::UnknownRoot { .. }),
        "{error}"
    );
    assert_eq!(grove.pages.pages, pages, "a refused change writes nothing");

    // Nor does it count that a root which the directory knows is carried
    // to the same cell.
    let wrong = [carried(cell(40, 1), cell(41, 1)), carried(was, cell(41, 1))];
    let error = grove
        .set(&wrong)
        .expect_err("no Merk is named with the cell");
    assert!(
        matches!(error, DirectoryError::UnknownRoot { .. }),
        "{error}"
    );
    assert_eq!(grove.pages.pages, pages, "a refused change writes nothing");

    let right = [
        carried(cell(40, 1), cell(41, 1)),
        named(&prefix, Some((cell(41, 1), b"k"))),
    ];
    grove.set(&right).expect("a root");
    assert_eq!(grove.root_of(&["a"]), Some((cell(41, 1), b"k".to_vec())));
    assert_eq!(grove.owner(was), None);
    grove.check();
}

#[test]
fn a_change_of_roots_that_cannot_be_is_refused_and_writes_nothing() {
    let mut grove = Grove::new();
    grove.add(&["a"]);
    grove.add(&["b"]);
    grove.add(&["a", "left"]);
    let (a, b) = (grove.root(&["a"]), grove.root(&["b"]));
    let prefix = |name: &str| prefix_of(&path(&[name]));
    let (pa, pb, nothing) = (prefix("a"), prefix("b"), prefix("nothing"));
    let left = prefix_of(&path(&["a", "left"]));
    let left_root = grove.root(&["a", "left"]);
    grove.unlink(&["a"]);
    grove.add(&["a"]);
    let a2 = grove.root(&["a"]);
    assert_ne!(a, a2);
    let pages = grove.pages.pages.clone();

    let long = [1; 256];
    let refused: Vec<(Vec<RootChange<'_>>, &str)> = vec![
        (
            vec![named(&pa, None), named(&pa, None)],
            "is named twice in one change",
        ),
        (
            vec![carried(a2, cell(50, 1)), carried(a2, cell(50, 2))],
            "is named twice in one change",
        ),
        (
            vec![carried(a2, cell(50, 1)), carried(b, cell(50, 1))],
            "two Merks would have their root at 50/1",
        ),
        (
            vec![
                named(&pa, Some((cell(50, 1), b"k"))),
                carried(b, cell(50, 1)),
            ],
            "two Merks would have their root at 50/1",
        ),
        // The cell of a root that stays where it is.
        (vec![carried(a2, b)], "two Merks would have their root at"),
        (
            vec![named(&pa, Some((b, b"k")))],
            "two Merks would have their root at",
        ),
        (
            vec![named(&nothing, None)],
            "no Merk is entered under the prefix",
        ),
        // A Merk that a drop left is not named, and its root is carried by
        // no write.
        (
            vec![named(&left, None)],
            "no Merk is entered under the prefix",
        ),
        (
            vec![carried(left_root, cell(50, 1))],
            "no Merk has its root at",
        ),
        (
            vec![named(&pb, Some((cell(50, 1), &long)))],
            "a root key of 256 bytes is longer than 255 bytes",
        ),
    ];
    for (changes, says) in refused {
        let error = grove.set(&changes).expect_err("refused");
        assert!(error.to_string().contains(says), "{error}");
        assert_eq!(grove.pages.pages, pages, "a refused change writes nothing");
    }
    grove.check();

    // The cell of a root that goes away in the same change is free.
    grove
        .set(&[named(&pa, Some((b, b"k"))), named(&pb, None)])
        .expect("a root");
    assert_eq!(grove.owner(b), Some(pa));
    grove.check();
}

#[test]
fn a_root_record_of_a_merk_whose_root_is_elsewhere_is_damage() {
    let mut grove = Grove::new();
    grove.add(&["a"]);
    grove.root(&["a"]);
    let prefix = prefix_of(&path(&["a"]));
    let tree = *grove.directory.tree();
    tree.put(&mut grove.pages, &root_key(cell(60, 1)), &prefix)
        .expect("a record");
    let error = grove
        .set(&[carried(cell(60, 1), cell(60, 2))])
        .expect_err("damage");
    assert_eq!(
        error.to_string(),
        "subtree directory record 043c0000000100: the Merk has its root elsewhere"
    );
    tree.put(&mut grove.pages, &root_key(cell(60, 3)), &[1; 31])
        .expect("a record");
    let error = grove
        .set(&[carried(cell(60, 3), cell(60, 2))])
        .expect_err("damage");
    assert_eq!(
        error.to_string(),
        "subtree directory record 043c0000000300: not a prefix"
    );
}

/// The part of T-SC5 that is the directory's (`transactions.md` 15.2): two
/// Merks whose entries share a page each have their root moved, by two
/// writes, and both entries are exact whichever write comes first.
#[test]
fn two_merks_in_one_page_have_their_roots_moved_in_either_order() {
    let build = || {
        let mut grove = Grove::new();
        for name in ["a", "b"] {
            grove.add(&[name]);
        }
        let at = ["a", "b"].map(|name| grove.root(&[name]));
        assert_eq!(grove.pages.pages.len(), 1, "one page holds every record");
        (grove, at)
    };
    let moves = |at: [CellAddress; 2]| [carried(at[0], cell(70, 1)), carried(at[1], cell(70, 2))];
    let (mut first, at) = build();
    let (mut second, _) = build();
    for change in moves(at) {
        first.set(&[change]).expect("moved");
    }
    for change in moves(at).into_iter().rev() {
        second.set(&[change]).expect("moved");
    }
    for grove in [&first, &second] {
        assert_eq!(grove.root_of(&["a"]).map(|root| root.0), Some(cell(70, 1)));
        assert_eq!(grove.root_of(&["b"]).map(|root| root.0), Some(cell(70, 2)));
        grove.check();
    }
    assert_eq!(
        first.pages.pages, second.pages.pages,
        "the same pages, byte for byte"
    );
}
