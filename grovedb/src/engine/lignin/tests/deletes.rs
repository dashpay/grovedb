//! Deletes, clears and drops on the LigninDB engine (GROVE-05).
//!
//! On RocksDB the delete of a tree that holds elements removes the
//! records under the tree's prefix one by one, and those of every tree
//! found in it. On LigninDB the removals are staged and charged in the
//! same way, and the subtree is dropped with its element in their place
//! (`engine.md`, "A subtree leaves with its element"). The histories here
//! are written once, for any engine, and run on LigninDB and on RocksDB on
//! every grove version: every answer, every `OperationCost` and the root
//! hash after every write are compared, and the physical checker reads the
//! database that LigninDB leaves.
//!
//! - [`trees`]: a tree of each kind whose data is one Merk, with items, a
//!   tree in it and a tree in that, a tree that nothing was written into
//!   and one that was emptied. It is refused where it holds elements,
//!   cleared at each level, deleted with what it holds, made again under
//!   the same key as a tree of another kind, deleted in a transaction that
//!   is rolled back and in one that commits, with a savepoint, up the
//!   tree, and with a function for the removed bytes.
//! - [`referred`]: trees that hold elements which take backward
//!   references, deleted with the references kept up (grove versions 4
//!   and 5).
//! - [`with_flat_trees`]: a tree that holds append-only trees with
//!   records, walked and deleted.
//! - [`flat_drops`]: flat drops and the drain of their records, with the
//!   report of each drain.
//!
//! One read is in none of them: a neighbour's step from no key between a
//! flat drop and its drain (`transactions.md` 14.5 D3, `MERK-05-F2`).

use grovedb_costs::{storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval, CostResult};
use grovedb_merk::store::lignin::hash::MerkHashRules;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    append_only::{check, on_both_engines, taken, verified, Answer, History},
    nested::{bidirectional, made_by, Kind, KINDS},
    Lignin, LigninEngine,
};
use crate::{
    engine::Engine,
    operations::delete::{ClearOptions, DeleteOptions, DeleteUpTreeOptions},
    tests::faultable_reads::{
        with_failed_parent_read, with_one_failed_drain_step, DrainStep, RefusedReads,
        INJECTED_DRAIN_FAILURE, INJECTED_READ_FAILURE,
    },
    BackwardsReferences, Element, Error, GroveDb, TransactionArg,
};

const ROOT: &[&[u8]] = &[];
const NEST: &[u8] = b"nest";
const DEEP: &[u8] = b"deep";

fn allowed() -> Option<DeleteOptions> {
    Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    })
}

/// The `n`th element of a tree of `kind`: every other one a sum item
/// where the tree takes them.
fn item(kind: &Kind, n: u8) -> Element {
    match kind.sums && n % 2 == 1 {
        true => Element::new_sum_item(i64::from(n) + 1),
        false => Element::new_item(vec![n; 3 + usize::from(n) * 7]),
    }
}

fn key(n: u8) -> [u8; 2] {
    [b'i', b'0' + n]
}

/// Fills the tree `top`, which is there and of `kinds[0]`: six elements,
/// a tree `nest` of `kinds[1]` with four, a tree `deep` of `kinds[2]` in
/// that with three, a tree that nothing is written into, and a tree that
/// is written into and emptied.
fn fill<'db, E: Engine>(
    h: &mut History<'db, E>,
    at_top: &[&[u8]],
    kinds: [&Kind; 3],
    tx: TransactionArg<'db, '_, E>,
) {
    let (db, v) = (h.db, h.v);
    let at_nest = &[at_top, &[NEST]].concat()[..];
    let at_deep = &[at_top, &[NEST, DEEP]].concat()[..];
    for n in 0..6 {
        let element = item(kinds[0], n);
        h.write_in("item", db.insert(at_top, &key(n), element, None, tx, v), tx);
    }
    let nest = (kinds[1].make)();
    h.write_in("made", db.insert(at_top, NEST, nest, None, tx, v), tx);
    for n in 0..4 {
        let element = item(kinds[1], n);
        h.write_in(
            "item",
            db.insert(at_nest, &key(n), element, None, tx, v),
            tx,
        );
    }
    let deep = (kinds[2].make)();
    h.write_in("made", db.insert(at_nest, DEEP, deep, None, tx, v), tx);
    for n in 0..3 {
        let element = item(kinds[2], n);
        h.write_in(
            "item",
            db.insert(at_deep, &key(n), element, None, tx, v),
            tx,
        );
    }
    let never = (kinds[2].make)();
    h.write_in("made", db.insert(at_top, b"never", never, None, tx, v), tx);
    let emptied = (kinds[1].make)();
    h.write_in(
        "made",
        db.insert(at_top, b"emptied", emptied, None, tx, v),
        tx,
    );
    let at_emptied = &[at_top, &[b"emptied"]].concat()[..];
    let element = item(kinds[1], 0);
    h.write_in(
        "item",
        db.insert(at_emptied, &key(0), element, None, tx, v),
        tx,
    );
    h.write_in("delete", db.delete(at_emptied, &key(0), None, tx, v), tx);
}

/// What is read of the tree at `at_top` and of what [`fill`] puts in it.
fn seen<'db, E: Engine>(h: &mut History<'db, E>, at_top: &[&[u8]], tx: TransactionArg<'db, '_, E>) {
    let (db, v) = (h.db, h.v);
    let (top, above) = at_top.split_last().expect("the tree has a key");
    let at_nest = &[at_top, &[NEST]].concat()[..];
    let at_deep = &[at_top, &[NEST, DEEP]].concat()[..];
    let at_emptied = &[at_top, &[b"emptied"]].concat()[..];
    h.element("element", db.get(above, top, tx, v));
    h.element("element", db.get(at_top, NEST, tx, v));
    h.element("element", db.get(at_nest, DEEP, tx, v));
    for path in [at_top, at_nest, at_deep, at_emptied] {
        h.read("empty", db.is_empty_tree(path, tx, v));
        h.element("element", db.get(path, &key(0), tx, v));
        h.element("element", db.get(path, &key(2), tx, v));
        h.read("has", db.has_raw(path, &key(1), tx, v));
    }
    h.read("subtrees", db.find_subtrees(&at_top.into(), tx, v));
}

/// The kinds of the three trees of a round. On the grove versions whose
/// delete of a tree that holds elements opens the Merk above as a tree of
/// the deleted tree's kind (`delete_internal_on_transaction` v0, GroveDB
/// #686), a tree whose hashes hold its aggregate is deleted from a tree of
/// its own kind only: under another kind that delete panics, on every
/// engine. So there a round of such a kind has it at all three levels,
/// and the other rounds take the kinds before the first of them.
fn kinds_of(round: usize, made: usize, v: &GroveVersion) -> [&'static Kind; 3] {
    let legacy = v
        .grovedb_versions
        .operations
        .delete
        .delete_internal_on_transaction
        == 0;
    let plain = (KINDS.iter())
        .position(|kind| kind.name.starts_with("provable"))
        .expect("a kind whose hashes hold its aggregate");
    let of = |kinds: [usize; 3]| kinds.map(|kind| &KINDS[kind]);
    match (legacy, round < plain) {
        (false, _) => of([round, (round + 1) % made, (round + 5) % made]),
        (true, true) => of([round, (round + 1) % plain, (round + 2) % plain]),
        (true, false) => of([round; 3]),
    }
}

/// The history of trees that hold elements: see the module documentation.
fn trees<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let made = made_by(v);
    for round in 0..made {
        let kinds = kinds_of(round, made, v);
        let name = format!("h{round:02}").into_bytes();
        let holder: &[&[u8]] = &[name.as_slice()];
        let top: &[u8] = b"top";
        let at_top: &[&[u8]] = &[name.as_slice(), top];
        let at_nest: &[&[u8]] = &[name.as_slice(), top, NEST];
        let at_deep: &[&[u8]] = &[name.as_slice(), top, NEST, DEEP];
        // The tree is in a tree of its own kind, with a key on each side
        // of it, so that its node is not always the root of its Merk.
        h.write(
            "made",
            db.insert(ROOT, &name, (kinds[0].make)(), None, None, v),
        );
        for beside in [b"a".as_slice(), b"z"] {
            let element = Element::new_item(vec![1; 9]);
            h.write("item", db.insert(holder, beside, element, None, None, v));
        }
        h.write(
            "made",
            db.insert(holder, top, (kinds[0].make)(), None, None, v),
        );
        fill(&mut h, at_top, kinds, None);
        seen(&mut h, at_top, None);

        // A tree that holds elements is not deleted unless the caller
        // allows it.
        h.write("refused", db.delete(holder, top, None, None, v));
        h.write("if empty", db.delete_if_empty_tree(holder, top, None, v));
        h.write("refused", db.delete(at_top, NEST, None, None, v));

        // Clears, from the bottom: a tree of items, a tree with a tree in
        // it that the options refuse, and the same with the trees in it
        // deleted.
        h.plain("cleared", db.clear_subtree(at_deep, None, None, v));
        h.read("root", db.root_hash(None, v));
        seen(&mut h, at_top, None);
        h.plain("clear refused", db.clear_subtree(at_nest, None, None, v));
        let quiet = ClearOptions {
            trying_to_clear_with_subtrees_returns_error: false,
            ..Default::default()
        };
        h.plain(
            "clear refused",
            db.clear_subtree(at_nest, Some(quiet), None, v),
        );
        h.read("root", db.root_hash(None, v));
        for n in 0..3 {
            let element = item(kinds[2], n + 3);
            h.write("item", db.insert(at_deep, &key(n), element, None, None, v));
        }
        let with_trees = ClearOptions {
            allow_deleting_subtrees: true,
            ..Default::default()
        };
        h.plain(
            "cleared with trees",
            db.clear_subtree(at_nest, Some(with_trees), None, v),
        );
        h.read("root", db.root_hash(None, v));
        seen(&mut h, at_top, None);
        let unchecked = ClearOptions {
            check_for_subtrees: false,
            backwards_references: BackwardsReferences::DontCheck,
            ..Default::default()
        };
        let at_emptied = &[at_top, &[b"emptied"]].concat()[..];
        h.plain(
            "cleared unchecked",
            db.clear_subtree(at_emptied, Some(unchecked), None, v),
        );
        h.read("root", db.root_hash(None, v));

        // A clear that is told to look for no tree removes the element
        // of a tree that holds elements with the others. On RocksDB the
        // records of that tree stay where no element leads to them, and
        // here its subtree leaves: its key is not used again.
        let at_lost = &[at_nest, &[b"lost"]].concat()[..];
        h.write(
            "made",
            db.insert(at_nest, b"lost", (kinds[2].make)(), None, None, v),
        );
        for n in 0..2 {
            let element = item(kinds[2], n);
            h.write("item", db.insert(at_lost, &key(n), element, None, None, v));
        }
        let element = item(kinds[1], 5);
        h.write("item", db.insert(at_nest, &key(5), element, None, None, v));
        let unchecked = ClearOptions {
            check_for_subtrees: false,
            backwards_references: BackwardsReferences::DontCheck,
            ..Default::default()
        };
        h.plain(
            "cleared over a tree",
            db.clear_subtree(at_nest, Some(unchecked), None, v),
        );
        h.read("root", db.root_hash(None, v));
        h.read("empty", db.is_empty_tree(at_nest, None, v));

        // The tree in the cleared one is made again and filled, and the
        // whole tree is deleted with what it holds.
        h.write(
            "made",
            db.insert(at_nest, DEEP, (kinds[2].make)(), None, None, v),
        );
        for n in 0..3 {
            let element = item(kinds[2], n);
            h.write("item", db.insert(at_deep, &key(n), element, None, None, v));
            let element = item(kinds[1], n);
            h.write("item", db.insert(at_nest, &key(n), element, None, None, v));
        }
        h.write("delete tree", db.delete(holder, top, allowed(), None, v));
        seen(&mut h, at_top, None);

        // The same key again, as a tree of another kind: it starts with
        // nothing, and so does every tree made in it.
        let again = [kinds[1], kinds[2], kinds[0]];
        h.write(
            "made",
            db.insert(holder, top, (again[0].make)(), None, None, v),
        );
        seen(&mut h, at_top, None);
        h.write(
            "made",
            db.insert(at_top, NEST, (again[1].make)(), None, None, v),
        );
        seen(&mut h, at_top, None);
        h.write("delete tree", db.delete(at_top, NEST, allowed(), None, v));
        fill(&mut h, at_top, again, None);
        seen(&mut h, at_top, None);

        // In a transaction that is rolled back, and in one that commits
        // after it has made the tree again.
        let tx = db.start_transaction();
        h.write_in(
            "delete tree",
            db.delete(holder, top, allowed(), Some(&tx), v),
            Some(&tx),
        );
        seen(&mut h, at_top, Some(&tx));
        seen(&mut h, at_top, None);
        h.plain("rolled back", db.rollback_transaction(&tx));
        seen(&mut h, at_top, Some(&tx));
        h.write_in(
            "delete tree",
            db.delete(at_top, NEST, allowed(), Some(&tx), v),
            Some(&tx),
        );
        h.plain("savepoint", db.set_savepoint(&tx));
        h.write_in(
            "delete tree",
            db.delete(holder, top, allowed(), Some(&tx), v),
            Some(&tx),
        );
        h.write_in(
            "made",
            db.insert(holder, top, (kinds[0].make)(), None, Some(&tx), v),
            Some(&tx),
        );
        seen(&mut h, at_top, Some(&tx));
        h.plain("undone", db.rollback_to_savepoint(&tx));
        seen(&mut h, at_top, Some(&tx));
        h.write_in(
            "delete tree",
            db.delete(holder, top, allowed(), Some(&tx), v),
            Some(&tx),
        );
        h.write_in(
            "made",
            db.insert(holder, top, (kinds[2].make)(), None, Some(&tx), v),
            Some(&tx),
        );
        fill(&mut h, at_top, [kinds[2], kinds[0], kinds[1]], Some(&tx));
        h.read("commit", db.commit_transaction(tx));
        h.read("root", db.root_hash(None, v));
        seen(&mut h, at_top, None);

        // Up the tree: the one element of the deepest tree, and with it
        // each tree that it leaves empty.
        for n in 1..3 {
            h.write("delete", db.delete(at_deep, &key(n), None, None, v));
        }
        for n in 0..4 {
            h.write("delete", db.delete(at_nest, &key(n), None, None, v));
        }
        let up = DeleteUpTreeOptions {
            stop_path_height: Some(1),
            ..Default::default()
        };
        h.write(
            "delete up",
            db.delete_up_tree_while_empty(at_deep, &key(0), &up, None, v),
        );
        seen(&mut h, at_top, None);

        // With a function for the removed bytes: an element, and the tree
        // with what is left in it.
        let mut basic = |_: &mut Vec<u8>, key_bytes: u32, value_bytes: u32| {
            Ok((
                BasicStorageRemoval(key_bytes),
                BasicStorageRemoval(value_bytes),
            ))
        };
        h.write(
            "sectional",
            db.delete_with_sectional_storage_function(
                at_top.into(),
                &key(3),
                None,
                None,
                &mut basic,
                v,
            ),
        );
        h.write(
            "sectional tree",
            db.delete_with_sectional_storage_function(
                holder.into(),
                top,
                allowed(),
                None,
                &mut basic,
                v,
            ),
        );
        seen(&mut h, at_top, None);
    }
    verified(&mut h);
    h.answers
}

/// What [`trees`] must have done on the grove version `number`.
fn trees_told(answers: &[Answer], number: usize) {
    let made = made_by(&GROVE_VERSIONS[number - 1]);
    assert_eq!(taken(answers, "delete tree"), 6 * made, "v{number}");
    assert_eq!(taken(answers, "sectional tree"), made, "v{number}");
    assert_eq!(taken(answers, "sectional"), made, "v{number}");
    assert_eq!(taken(answers, "delete up"), made, "v{number}");
    assert_eq!(taken(answers, "refused"), 0, "v{number}");
    assert_eq!(taken(answers, "commit"), made, "v{number}");
    let said = |call: &str, answer: &str| {
        let said = |(named, _, said): &&Answer| named == call && said == answer;
        answers.iter().filter(said).count()
    };
    assert_eq!(said("cleared", "Ok(true)"), made, "v{number}");
    assert_eq!(said("cleared with trees", "Ok(true)"), made, "v{number}");
    assert_eq!(said("cleared unchecked", "Ok(true)"), made, "v{number}");
    assert_eq!(said("cleared over a tree", "Ok(true)"), made, "v{number}");
    assert_eq!(said("clear refused", "Ok(false)"), made, "v{number}");
    assert_eq!(said("if empty", "Ok(false)"), made, "v{number}");
    assert_eq!(said("verified", "Ok([])"), 1, "v{number}");
}

#[test]
fn trees_that_hold_elements_are_cleared_and_deleted_as_on_rocksdb() {
    on_both_engines!(trees, trees_told);
}

/// Trees that hold elements which take backward references, with a
/// reference to each from a tree beside them: the tree is deleted with
/// the check declared off while it holds them, in a transaction that is
/// rolled back, and with the references kept up, which walks it element
/// by element.
fn referred<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let made = made_by(v);
    let refs: &[&[u8]] = &[b"refs"];
    h.write(
        "made",
        db.insert(ROOT, b"refs", Element::empty_tree(), None, None, v),
    );
    for round in 0..made {
        let kinds = [&KINDS[round], &KINDS[(round + 3) % made]];
        let name = format!("t{round:02}").into_bytes();
        let top = name.as_slice();
        let holder = ROOT;
        let at_top: &[&[u8]] = &[top];
        let at_nest: &[&[u8]] = &[top, NEST];
        let owned = |path: &[&[u8]]| -> Vec<Vec<u8>> { path.iter().map(|s| s.to_vec()).collect() };
        h.write(
            "made",
            db.insert(ROOT, top, (kinds[0].make)(), None, None, v),
        );
        h.write(
            "made",
            db.insert(at_top, NEST, (kinds[1].make)(), None, None, v),
        );
        for n in 0..3 {
            h.write(
                "item",
                db.insert(at_top, &key(n), item(kinds[0], n), None, None, v),
            );
            h.write(
                "item",
                db.insert(at_nest, &key(n), item(kinds[1], n), None, None, v),
            );
        }
        let target = Element::new_item_allowing_bidirectional_references(vec![round as u8; 11]);
        let made_target = h.write(
            "target",
            db.insert(at_top, b"target", target, None, None, v),
        );
        if made_target.is_none() {
            // A grove version that makes no element which takes backward
            // references: its deletes of trees are those of [`trees`].
            return h.answers;
        }
        let target = Element::new_item_allowing_bidirectional_references(vec![round as u8; 5]);
        h.write(
            "target",
            db.insert(at_nest, b"target", target, None, None, v),
        );
        let near = format!("near{round:02}").into_bytes();
        let far = format!("far{round:02}").into_bytes();
        h.write(
            "reference",
            db.insert(
                refs,
                &near,
                bidirectional(&owned(at_top), b"target"),
                None,
                None,
                v,
            ),
        );
        h.write(
            "reference",
            db.insert(
                refs,
                &far,
                bidirectional(&owned(at_nest), b"target"),
                None,
                None,
                v,
            ),
        );
        h.element("followed", db.get(refs, &far, None, v));
        // Declared unchecked, the delete of a tree that holds such
        // elements is the caller's word; checked, it walks the tree.
        let unchecked = allowed()
            .map(|options| options.with_backwards_references(BackwardsReferences::DontCheck));
        if round % 2 == 0 {
            h.write("delete nest", db.delete(at_top, NEST, allowed(), None, v));
            h.element("followed", db.get(refs, &far, None, v));
            h.element("followed", db.get(refs, &near, None, v));
            h.write("delete tree", db.delete(holder, top, allowed(), None, v));
        } else {
            // In a transaction that is rolled back: the delete takes
            // the caller's word, and the references to what the tree
            // held are left with nothing to follow.
            let tx = db.start_transaction();
            h.write_in(
                "unchecked",
                db.delete(holder, top, unchecked, Some(&tx), v),
                Some(&tx),
            );
            h.element("followed", db.get(refs, &far, Some(&tx), v));
            h.element("element", db.get(at_nest, b"target", Some(&tx), v));
            h.plain("rolled back", db.rollback_transaction(&tx));
            h.element("followed", db.get(refs, &far, None, v));
            h.write("delete tree", db.delete(holder, top, allowed(), None, v));
        }
        h.element("followed", db.get(refs, &far, None, v));
        h.element("followed", db.get(refs, &near, None, v));
        h.element("element", db.get(ROOT, top, None, v));
        h.read("empty", db.is_empty_tree(refs, None, v));
        // The key again, with a tree in it.
        h.write(
            "made",
            db.insert(ROOT, top, (kinds[1].make)(), None, None, v),
        );
        h.write(
            "made",
            db.insert(at_top, NEST, (kinds[0].make)(), None, None, v),
        );
        h.read("empty", db.is_empty_tree(at_nest, None, v));
        h.write(
            "item",
            db.insert(at_nest, &key(0), item(kinds[0], 0), None, None, v),
        );
        h.element("element", db.get(at_nest, b"target", None, v));
    }
    verified(&mut h);
    h.answers
}

fn referred_told(answers: &[Answer], number: usize) {
    // Grove versions 1 to 3 make no element that takes backward
    // references.
    if number <= 3 {
        assert_eq!(taken(answers, "target"), 0, "v{number}");
        return;
    }
    let made = made_by(&GROVE_VERSIONS[number - 1]);
    assert_eq!(taken(answers, "target"), 2 * made, "v{number}");
    assert_eq!(taken(answers, "reference"), 2 * made, "v{number}");
    assert_eq!(taken(answers, "delete nest"), made.div_ceil(2), "v{number}");
    assert_eq!(taken(answers, "delete tree"), made, "v{number}");
    // Every other round, the delete that declares the check off is made
    // of the tree while it holds such elements.
    assert_eq!(taken(answers, "unchecked"), made / 2, "v{number}");
    let said = |(named, _, said): &&Answer| named == "verified" && said == "Ok([])";
    assert_eq!(answers.iter().filter(said).count(), 1, "v{number}");
}

#[test]
fn trees_with_elements_that_take_backward_references_are_deleted_as_on_rocksdb() {
    on_both_engines!(referred, referred_told);
}

/// A database of LigninDB with the tree `top` of [`fill`]'s shape.
fn filled(dir: &std::path::Path, v: &GroveVersion) -> Lignin {
    let db = Lignin::open_with_engine(dir).expect("a database is made");
    let mut h = History::new(&db, v);
    let kinds = [&KINDS[0], &KINDS[1], &KINDS[3]];
    h.write(
        "made",
        db.insert(ROOT, b"top", Element::empty_tree(), None, None, v),
    );
    fill(&mut h, &[b"top"], kinds, None);
    assert!(h.answers.iter().all(|(.., said)| said.starts_with("Ok(")));
    drop(h);
    db
}

#[test]
fn a_deleted_tree_leaves_no_entry_and_the_checker_finds_the_store_sound() {
    for v in GROVE_VERSIONS.iter() {
        let dir = tempfile::tempdir().expect("a directory");
        {
            let db = filled(dir.path(), v);
            let engine = db.engine();
            let entry_of = |path: &[&[u8]]| {
                let tx = engine.start_read_transaction();
                let handle = engine.subtree(path.into(), None, &tx).unwrap();
                handle.entry().expect("the directory answers")
            };
            let top: &[&[u8]] = &[b"top"];
            let nest: &[&[u8]] = &[b"top", NEST];
            let deep: &[&[u8]] = &[b"top", NEST, DEEP];
            let emptied: &[&[u8]] = &[b"top", b"emptied"];
            // The tree that was written into and emptied is entered, with
            // no root, and the delete of its element takes the entry.
            let entry = entry_of(emptied).expect("an entry");
            assert_eq!(entry.root_key, None);
            (db.delete(top, b"emptied", None, None, v))
                .unwrap()
                .expect("an empty tree");
            assert_eq!(entry_of(emptied), None);
            // The tree with a tree in it: both entries go with the
            // element of the outer one.
            assert!(entry_of(nest).is_some() && entry_of(deep).is_some());
            (db.delete(top, NEST, allowed(), None, v))
                .unwrap()
                .expect("a tree that holds elements");
            assert_eq!((entry_of(nest), entry_of(deep)), (None, None));
            // A tree made under the key again is entered by its first
            // write, as a tree of its own type, and holds that write.
            db.insert(top, NEST, Element::empty_sum_tree(), None, None, v)
                .unwrap()
                .expect("a tree");
            assert_eq!(entry_of(nest), None);
            db.insert(nest, b"one", Element::new_sum_item(1), None, None, v)
                .unwrap()
                .expect("a sum item");
            let entry = entry_of(nest).expect("an entry");
            assert_eq!(
                entry.tree_type().expect("a tree type"),
                grovedb_merk::TreeType::SumTree
            );
            assert_eq!(entry.root_key.as_deref(), Some(b"one".as_slice()));
            // A clear that looks for no tree removes the element of the
            // tree with the others, and the entry goes with it. The
            // cleared tree keeps its own, with no root.
            let unchecked = ClearOptions {
                check_for_subtrees: false,
                backwards_references: BackwardsReferences::DontCheck,
                ..Default::default()
            };
            let cleared = db.clear_subtree(top, Some(unchecked), None, v);
            assert!(cleared.expect("a clear"));
            assert_eq!(entry_of(nest), None);
            assert_eq!(entry_of(top).expect("an entry").root_key, None);
            db.insert(top, NEST, Element::empty_tree(), None, None, v)
                .unwrap()
                .expect("a tree");
            db.insert(nest, b"two", Element::new_item(vec![2]), None, None, v)
                .unwrap()
                .expect("an item");
            assert!(entry_of(nest).is_some());
            (db.delete(ROOT, b"top", allowed(), None, v))
                .unwrap()
                .expect("the whole tree");
            assert_eq!((entry_of(top), entry_of(nest)), (None, None));
            db.flush().expect("a flush");
        }
        check(dir.path(), v);
    }
}

fn shown<T: std::fmt::Debug>(answer: CostResult<T, Error>) -> CostResult<String, Error> {
    answer.map_ok(|answer| format!("{answer:?}"))
}

/// Whether the walk of `v` for the trees in a tree takes every record of
/// the data keyspace for an element (`find_subtrees` v0, grove versions 1
/// to 3). Over a tree that holds an append-only tree with records it
/// fails on RocksDB, whose cursor shows those records, and so does the
/// delete of a tree above one.
fn walks_records_as_elements(v: &GroveVersion) -> bool {
    (v.grovedb_versions.operations.delete).delete_internal_on_transaction == 0
}

/// A tree that holds append-only trees: an MMR tree and a dense tree
/// beside items, and a tree with an MMR tree in it. It is walked for the
/// trees in it and deleted with what it holds while they have no record,
/// and again once they have; the keys are made again, and an append-only
/// tree under the key of a dropped one starts with no record. On the
/// grove versions whose walk takes a record for an element the second
/// walk and delete are left to [`walked_over_records`].
fn with_flat_trees<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let outer: &[&[u8]] = &[b"outer"];
    let inner: &[&[u8]] = &[b"outer", b"inner"];
    let make = |h: &mut History<'_, E>| {
        h.write(
            "made",
            db.insert(ROOT, b"outer", Element::empty_tree(), None, None, v),
        );
        h.write(
            "made",
            db.insert(outer, b"inner", Element::empty_sum_tree(), None, None, v),
        );
        h.write(
            "made",
            db.insert(outer, b"mmr", Element::empty_mmr_tree(), None, None, v),
        );
        h.write(
            "made",
            db.insert(outer, b"dense", Element::empty_dense_tree(3), None, None, v),
        );
        h.write(
            "made",
            db.insert(inner, b"mmr", Element::empty_mmr_tree(), None, None, v),
        );
        for n in 0..5u8 {
            h.write(
                "item",
                db.insert(outer, &key(n), item(&KINDS[0], n), None, None, v),
            );
            h.write(
                "item",
                db.insert(inner, &key(n), item(&KINDS[1], n), None, None, v),
            );
        }
    };
    make(&mut h);
    // The walk and the delete of a tree whose trees have no record.
    h.read("subtrees", db.find_subtrees(&outer.into(), None, v));
    h.write("delete tree", db.delete(ROOT, b"outer", allowed(), None, v));
    make(&mut h);
    for n in 0..7usize {
        let value = vec![n as u8; 1 + n * 23];
        h.write(
            "append",
            shown(db.mmr_tree_append(outer, b"mmr", value.clone(), None, v)),
        );
        h.write(
            "append",
            shown(db.mmr_tree_append(inner, b"mmr", value.clone(), None, v)),
        );
        h.write(
            "append",
            shown(db.dense_tree_insert(outer, b"dense", value, None, v)),
        );
    }
    h.write("refused", db.delete(ROOT, b"outer", None, None, v));
    if walks_records_as_elements(v) {
        verified(&mut h);
        return h.answers;
    }
    walk_and_delete(&mut h);
    // The keys again: an append-only tree under the key of a dropped one
    // has no record of it.
    make(&mut h);
    h.read(
        "count",
        shown(db.mmr_tree_leaf_count(outer, b"mmr", None, v)),
    );
    h.read(
        "value",
        shown(db.mmr_tree_get_value(outer, b"mmr", 0, None, v)),
    );
    h.write(
        "append",
        shown(db.mmr_tree_append(outer, b"mmr", vec![7; 30], None, v)),
    );
    h.write(
        "append",
        shown(db.mmr_tree_append(inner, b"mmr", vec![8; 30], None, v)),
    );
    h.read(
        "value",
        shown(db.mmr_tree_get_value(inner, b"mmr", 0, None, v)),
    );
    h.read("subtrees", db.find_subtrees(&outer.into(), None, v));
    verified(&mut h);
    h.answers
}

/// The walk for the trees in `outer` and in the tree in it, and the
/// delete of each with what it holds.
fn walk_and_delete<E: Engine>(h: &mut History<'_, E>) {
    let (db, v) = (h.db, h.v);
    let outer: &[&[u8]] = &[b"outer"];
    let inner: &[&[u8]] = &[b"outer", b"inner"];
    h.read("subtrees", db.find_subtrees(&outer.into(), None, v));
    h.read("subtrees", db.find_subtrees(&inner.into(), None, v));
    h.write(
        "delete inner",
        db.delete(outer, b"inner", allowed(), None, v),
    );
    h.element("element", db.get(outer, b"inner", None, v));
    h.write("delete tree", db.delete(ROOT, b"outer", allowed(), None, v));
    h.element("element", db.get(ROOT, b"outer", None, v));
    h.element("element", db.get(outer, b"mmr", None, v));
}

fn with_flat_trees_told(answers: &[Answer], number: usize) {
    let (appends, deletes) = if number >= 4 { (23, 2) } else { (21, 1) };
    assert_eq!(taken(answers, "append"), appends, "v{number}");
    assert_eq!(taken(answers, "delete tree"), deletes, "v{number}");
    assert_eq!(taken(answers, "refused"), 0, "v{number}");
    let said = |(named, _, said): &&Answer| named == "verified" && said == "Ok([])";
    assert_eq!(answers.iter().filter(said).count(), 1, "v{number}");
}

#[test]
fn a_tree_that_holds_append_only_trees_is_walked_and_deleted_as_on_rocksdb() {
    on_both_engines!(with_flat_trees, with_flat_trees_told);
}

/// The history of [`with_flat_trees`] on a grove version whose walk takes
/// a record for an element, up to the append-only trees with records, and
/// then the walk and the deletes that it leaves out.
fn walked_over_records<E: Engine>(dir: &std::path::Path, v: &GroveVersion) -> Vec<Answer> {
    let db = GroveDb::<E>::open_with_engine(dir).expect("a database is made");
    let mut answers = with_flat_trees(&db, v);
    let mut h = History::new(&db, v);
    walk_and_delete(&mut h);
    answers.append(&mut h.answers);
    answers
}

/// On grove versions 1 to 3 the walk for the trees in a tree takes every
/// record of the data keyspace for an element. RocksDB's cursor over the
/// nodes of a tree is the cursor of its keyspace, which shows the records
/// of an append-only tree: the walk over one that has records fails
/// there, and so does the delete of a tree that holds one. The node
/// cursor of the LigninDB store shows the key of such a record and
/// refuses its value, until it shows the record's bytes (`MERK-05-F1`
/// part 2): the walk fails here too, with the store's refusal and not
/// with what GroveDB makes of the bytes, and so does the delete. This
/// test holds what each engine answers until then: the same up to the
/// first walk over records, RocksDB's failure there, and that LigninDB
/// either fails as RocksDB does, from which call on the two are compared
/// to the end, or fails each call that RocksDB fails with that refusal
/// and answers every other call as RocksDB does.
#[cfg(feature = "rocksdb_storage")]
#[test]
fn a_walk_that_takes_the_records_of_an_append_only_tree_for_elements() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        if !walks_records_as_elements(v) {
            continue;
        }
        let dir = tempfile::tempdir().expect("a directory");
        let on_lignin = walked_over_records::<LigninEngine>(dir.path(), v);
        check(dir.path(), v);
        let dir = tempfile::tempdir().expect("a directory");
        let on_rocksdb = walked_over_records::<crate::RocksDbEngine>(dir.path(), v);
        assert_eq!(on_lignin.len(), on_rocksdb.len(), "v{number}");
        // The nine answers of the walk and the deletes.
        let walk = on_rocksdb.len() - 9;
        assert_eq!(on_lignin[..walk], on_rocksdb[..walk], "v{number}");
        let (call, _, failed) = &on_rocksdb[walk];
        assert_eq!(call, "subtrees", "v{number}");
        assert!(failed.starts_with("Err("), "v{number}: {failed}");
        let (_, _, deleted) = &on_rocksdb[walk + 5];
        assert!(deleted.starts_with("Err("), "v{number}: {deleted}");
        if on_lignin[walk..] == on_rocksdb[walk..] {
            continue;
        }
        for (here, there) in on_lignin[walk..].iter().zip(&on_rocksdb[walk..]) {
            match there.2.starts_with("Err(") {
                true => {
                    let reason = "a flat record among the nodes of a Merk is no unlinked record";
                    let refused = here.2.starts_with("Err(StorageError(StorageError(")
                        && here.2.contains(reason);
                    assert!(refused, "v{number} {}: {}", here.0, here.2);
                    assert_eq!(here.0, there.0, "v{number}");
                }
                false => assert_eq!(here, there, "v{number}"),
            }
        }
    }
}

/// What a drain reported, as a history keeps it.
type Drained = (u64, u64, u64, bool);

fn drain<E: Engine>(h: &mut History<'_, E>) {
    let report = h.db.flush_pending_prefix_drops(h.v);
    let counts = |report: crate::operations::delete::PendingPrefixDropsReport| -> Drained {
        (
            report.reclaimed_records,
            report.skipped_live,
            report.unresolved_records,
            report.first_unresolved_error.is_some(),
        )
    };
    h.plain("drained", report.map(counts));
}

/// Flat drops and the drain of their records: a drop with no transaction,
/// which drains at once; drops that are refused; drops in a transaction
/// that is rolled back, and in one that commits and is drained
/// afterwards; a dropped path that is made again before its record is
/// drained, which is skipped as live until it is gone; a tree with a tree
/// in it, against the caller's word; and an append-only tree with
/// records. Each drain's report is in the history.
fn flat_drops<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let docs: &[&[u8]] = &[b"docs"];
    let unchecked = BackwardsReferences::DontCheck;
    h.write(
        "made",
        db.insert(ROOT, b"docs", Element::empty_tree(), None, None, v),
    );
    h.write(
        "item",
        db.insert(docs, b"item", Element::new_item(vec![3; 9]), None, None, v),
    );
    for (name, items) in [(b"a", 5u8), (b"b", 9), (b"c", 3), (b"d", 7), (b"n", 4)] {
        let at: &[&[u8]] = &[b"docs", name];
        h.write(
            "made",
            db.insert(docs, name, Element::empty_tree(), None, None, v),
        );
        for n in 0..items {
            h.write(
                "item",
                db.insert(at, &key(n), item(&KINDS[0], n), None, None, v),
            );
        }
    }
    let nested: &[&[u8]] = &[b"docs", b"n", b"below"];
    h.write(
        "made",
        db.insert(
            &nested[..2],
            b"below",
            Element::empty_sum_tree(),
            None,
            None,
            v,
        ),
    );
    h.write(
        "item",
        db.insert(nested, &key(1), item(&KINDS[1], 1), None, None, v),
    );
    h.write(
        "made",
        db.insert(docs, b"mmr", Element::empty_mmr_tree(), None, None, v),
    );
    for n in 0..5usize {
        let appended = db.mmr_tree_append(docs, b"mmr", vec![n as u8; 40], None, v);
        h.write("append", shown(appended));
    }
    drain(&mut h);

    // With no transaction the drop drains its own record.
    h.write("drop", db.drop_flat_subtree(docs, b"a", unchecked, None, v));
    drain(&mut h);
    h.element("element", db.get(docs, b"a", None, v));
    h.element(
        "element",
        db.get(&[b"docs".as_slice(), b"a"][..], &key(0), None, v),
    );
    // What a flat drop refuses.
    let checked = BackwardsReferences::Check;
    h.write(
        "drop refused",
        db.drop_flat_subtree(docs, b"b", checked, None, v),
    );
    h.write(
        "drop refused",
        db.drop_flat_subtree(docs, b"item", unchecked, None, v),
    );
    h.write(
        "drop refused",
        db.drop_flat_subtree(docs, b"none", unchecked, None, v),
    );

    // In a transaction: nothing is drained before the commit, and a
    // rollback takes the record with the drop.
    let tx = db.start_transaction();
    h.write_in(
        "drop",
        db.drop_flat_subtree(docs, b"b", unchecked, Some(&tx), v),
        Some(&tx),
    );
    drain(&mut h);
    h.element("element", db.get(docs, b"b", Some(&tx), v));
    h.element("element", db.get(docs, b"b", None, v));
    h.plain("rolled back", db.rollback_transaction(&tx));
    h.element("element", db.get(docs, b"b", Some(&tx), v));
    for name in [b"b", b"c"] {
        h.write_in(
            "drop",
            db.drop_flat_subtree(docs, name, unchecked, Some(&tx), v),
            Some(&tx),
        );
    }
    drain(&mut h);
    h.read("commit", db.commit_transaction(tx));
    h.read("root", db.root_hash(None, v));
    drain(&mut h);
    drain(&mut h);

    // A dropped path that is made again before its record is drained is
    // live: its record stays, and is reported, until the path is gone.
    let again: &[&[u8]] = &[b"docs", b"d"];
    let tx = db.start_transaction();
    h.write_in(
        "drop",
        db.drop_flat_subtree(docs, b"d", unchecked, Some(&tx), v),
        Some(&tx),
    );
    h.read("commit", db.commit_transaction(tx));
    h.write(
        "made",
        db.insert(docs, b"d", Element::empty_tree(), None, None, v),
    );
    h.write(
        "item",
        db.insert(
            again,
            b"fresh",
            Element::new_item(vec![5; 5]),
            None,
            None,
            v,
        ),
    );
    drain(&mut h);
    drain(&mut h);
    h.element("element", db.get(again, b"fresh", None, v));
    h.element("element", db.get(again, &key(0), None, v));
    // The path goes again by a second flat drop: on RocksDB the records
    // of the first are still under its prefix, and a delete that looks at
    // the subtree would find them (`transactions.md` 14.5 D3).
    h.write("delete", db.delete(again, b"fresh", None, None, v));
    let tx = db.start_transaction();
    h.write_in(
        "drop",
        db.drop_flat_subtree(docs, b"d", unchecked, Some(&tx), v),
        Some(&tx),
    );
    h.read("commit", db.commit_transaction(tx));
    drain(&mut h);

    // A tree with a tree in it, and an append-only tree with records.
    let tx = db.start_transaction();
    for name in [b"n".as_slice(), b"mmr"] {
        h.write_in(
            "drop",
            db.drop_flat_subtree(docs, name, unchecked, Some(&tx), v),
            Some(&tx),
        );
    }
    h.read("commit", db.commit_transaction(tx));
    drain(&mut h);
    h.element("element", db.get(nested, &key(1), None, v));
    h.read("empty", db.is_empty_tree(docs, None, v));
    verified(&mut h);
    h.answers
}

fn flat_drops_told(answers: &[Answer], number: usize) {
    let drained: Vec<&str> = (answers.iter())
        .filter(|(call, ..)| call == "drained")
        .map(|(.., said)| said.as_str())
        .collect();
    let none = "Ok((0, 0, 0, false))";
    // Grove versions 1 to 3 make no flat drop.
    let expected = match number >= 4 {
        false => [none; 9],
        true => [
            none,
            none,
            none,
            none,
            "Ok((2, 0, 0, false))",
            none,
            "Ok((0, 1, 0, false))",
            "Ok((0, 1, 0, false))",
            "Ok((1, 0, 0, false))",
        ],
    };
    assert_eq!(drained[..9], expected, "v{number}");
    let last = if number >= 4 {
        "Ok((2, 0, 0, false))"
    } else {
        none
    };
    assert_eq!(drained[9..], [last], "v{number}");
    let drops = if number >= 4 { 8 } else { 0 };
    assert_eq!(taken(answers, "drop"), drops, "v{number}");
    assert_eq!(taken(answers, "drop refused"), 0, "v{number}");
    let said = |(named, _, said): &&Answer| named == "verified" && said == "Ok([])";
    assert_eq!(answers.iter().filter(said).count(), 1, "v{number}");
}

#[test]
fn flat_drops_and_their_drains_report_as_on_rocksdb() {
    on_both_engines!(flat_drops, flat_drops_told);
}

/// A drain whose step fails, on an engine that fails the step it is told
/// to ([`RefusedReads`]): three flat drops are committed, and the drain is
/// run with the range delete of the second record failing, with the
/// removal of a record failing, and with nothing in its way; then a
/// fourth drop is drained while the check of whether its path is live has
/// no answer. After each pass the history has what the pass returned and
/// how many records are left.
fn failed_drains<E: Engine>(dir: &std::path::Path, v: &GroveVersion) -> Vec<Answer> {
    use grovedb_storage::flat::PendingPrefixDrops;

    let db = GroveDb::<RefusedReads<E>>::open_with_engine(dir).expect("a database is made");
    let mut h = History::new(&db, v);
    let docs: &[&[u8]] = &[b"docs"];
    let unchecked = BackwardsReferences::DontCheck;
    h.write(
        "made",
        db.insert(ROOT, b"docs", Element::empty_tree(), None, None, v),
    );
    for name in [b"a", b"b", b"c", b"d"] {
        let at: &[&[u8]] = &[b"docs", name];
        h.write(
            "made",
            db.insert(docs, name, Element::empty_tree(), None, None, v),
        );
        for n in 0..4 {
            h.write(
                "item",
                db.insert(at, &key(n), item(&KINDS[0], n), None, None, v),
            );
        }
    }
    let tx = db.start_transaction();
    for name in [b"a", b"b", b"c"] {
        h.write_in(
            "drop",
            db.drop_flat_subtree(docs, name, unchecked, Some(&tx), v),
            Some(&tx),
        );
    }
    h.read("commit", db.commit_transaction(tx));
    let pending = |h: &mut History<'_, RefusedReads<E>>| {
        let records = db.engine().pending_prefix_drop_records();
        h.plain(
            "pending",
            records.map(|records| records.len()).map_err(Error::from),
        );
    };
    let counts = |report: crate::operations::delete::PendingPrefixDropsReport| -> Drained {
        (
            report.reclaimed_records,
            report.skipped_live,
            report.unresolved_records,
            report.first_unresolved_error.is_some(),
        )
    };
    pending(&mut h);
    for (step, passes) in [(DrainStep::Ranges, 1), (DrainStep::Record, 0)] {
        let (drained, failed) =
            with_one_failed_drain_step(step, passes, || db.flush_pending_prefix_drops(v));
        assert!(failed, "the drain came to the step that fails");
        h.plain("failed drain", drained.map(counts));
        pending(&mut h);
    }
    drain(&mut h);
    pending(&mut h);

    // A record whose path cannot be checked is left and counted, and the
    // error of its check is in the report.
    h.write("drop", db.drop_flat_subtree(docs, b"d", unchecked, None, v));
    pending(&mut h);
    let tx = db.start_transaction();
    h.write(
        "made",
        db.insert(docs, b"e", Element::empty_tree(), None, Some(&tx), v),
    );
    h.write_in(
        "drop",
        db.drop_flat_subtree(docs, b"e", unchecked, Some(&tx), v),
        Some(&tx),
    );
    h.read("commit", db.commit_transaction(tx));
    let unresolved = with_failed_parent_read(b"docs", || db.flush_pending_prefix_drops(v));
    let error = unresolved
        .as_ref()
        .ok()
        .and_then(|report| report.first_unresolved_error.clone());
    if let Some(error) = error {
        assert!(error.contains(INJECTED_READ_FAILURE), "{error}");
    }
    h.plain("unresolved drain", unresolved.map(counts));
    pending(&mut h);
    drain(&mut h);
    pending(&mut h);
    verified(&mut h);
    h.answers
}

/// "An injected failure leaves the records for the next flush": the
/// history of [`failed_drains`] on both engines, on the grove versions
/// that make flat drops.
#[test]
fn a_drain_whose_step_fails_leaves_its_records_for_the_next_as_on_rocksdb() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate().skip(3) {
        let number = number + 1;
        let dir = tempfile::tempdir().expect("a directory");
        let on_lignin = failed_drains::<LigninEngine>(dir.path(), v);
        check(dir.path(), v);
        let said: Vec<(&str, &str)> = (on_lignin.iter())
            .filter(|(call, ..)| call.contains("drain") || call == "pending")
            .map(|(call, _, said)| (call.as_str(), said.as_str()))
            .collect();
        let injected = format!("Err(StorageError(StorageError(\"{INJECTED_DRAIN_FAILURE}\")))");
        assert_eq!(
            said,
            [
                ("pending", "Ok(3)"),
                // The first record is reclaimed before the step that fails.
                ("failed drain", injected.as_str()),
                ("pending", "Ok(2)"),
                // The ranges of the next are gone, and its record is not.
                ("failed drain", injected.as_str()),
                ("pending", "Ok(2)"),
                ("drained", "Ok((2, 0, 0, false))"),
                ("pending", "Ok(0)"),
                ("pending", "Ok(0)"),
                ("unresolved drain", "Ok((0, 0, 1, true))"),
                ("pending", "Ok(1)"),
                ("drained", "Ok((1, 0, 0, false))"),
                ("pending", "Ok(0)"),
            ],
            "v{number}"
        );
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let on_rocksdb = failed_drains::<crate::RocksDbEngine>(dir.path(), v);
            assert_eq!(on_lignin.len(), on_rocksdb.len(), "v{number}");
            for (step, (lignin, rocksdb)) in on_lignin.iter().zip(&on_rocksdb).enumerate() {
                assert_eq!(lignin, rocksdb, "call {step} on grove version {number}");
            }
        }
    }
}

/// What the physical checker counts in the open database of `db`, which
/// it has to find sound.
fn census(db: &Lignin, v: &GroveVersion) -> lignindb::verify::Census {
    let mut rules = MerkHashRules::for_check(v);
    let report = db.engine().database().verify(Some(&mut rules));
    assert!(report.is_sound(), "{:#?}", report.findings);
    report.census
}

/// Has the reclaiming of `db` take out everything that waits.
fn reclaimed(db: &Lignin) {
    let database = db.engine().database();
    for _ in 0..10_000 {
        let round = database.reclaim();
        if round.waiting == 0 && !round.more {
            return;
        }
    }
    panic!("the reclaiming did not come to an end");
}

/// "Dropped space is reclaimed": a tree of 2,000 items with twenty trees
/// of fifty in it is deleted while a snapshot from before reads it. Until
/// the snapshot goes, the cells are where they were, as what a dropped
/// Merk left, and the snapshot reads them. Then the reclaimer takes them
/// out, with the entries of the trees in it, and frees their pages.
#[test]
fn what_a_deleted_tree_left_is_reclaimed_and_its_pages_are_given_back() {
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        let insert = |path: &[&[u8]], key: &[u8], element: Element, tx| {
            (db.insert(path, key, element, None, tx, v).unwrap()).expect("an insert");
        };
        insert(ROOT, b"kept", Element::empty_tree(), None);
        for n in 0..300u32 {
            let item = Element::new_item(vec![n as u8; 120]);
            insert(&[b"kept"], &n.to_be_bytes(), item, None);
        }
        let empty = census(&db, v);
        let tx = db.start_transaction();
        insert(ROOT, b"top", Element::empty_tree(), Some(&tx));
        for n in 0..2000u32 {
            let item = Element::new_item(vec![n as u8; 120]);
            insert(&[b"top"], &n.to_be_bytes(), item, Some(&tx));
        }
        for tree in 0..20u8 {
            let name = [b't', tree];
            insert(&[b"top"], &name, Element::empty_sum_tree(), Some(&tx));
            for n in 0..50u8 {
                let item = Element::new_sum_item(i64::from(n));
                insert(&[b"top", &name], &[n], item, Some(&tx));
            }
        }
        (db.commit_transaction(tx).unwrap()).expect("a commit");
        let full = census(&db, v);
        // The tree, its 2,000 items and twenty trees, and their items.
        let nodes = 1 + 2000 + 20 + 20 * 50;
        assert_eq!(full.live_cells, empty.live_cells + nodes);
        assert_eq!(full.merks, empty.merks + 21);
        assert!(full.tree_pages > empty.tree_pages + 20, "{full:?}");

        let before = db.start_snapshot_read_transaction();
        (db.delete(ROOT, b"top", allowed(), None, v).unwrap()).expect("the tree is deleted");
        let dropped = census(&db, v);
        assert_eq!(dropped.live_cells, empty.live_cells);
        assert_eq!(dropped.merks, empty.merks);
        assert_eq!(dropped.left_cells, nodes - 1);
        assert_eq!((dropped.dropped_merks, dropped.left_over_entries), (1, 20));
        assert_eq!(dropped.tree_pages, full.tree_pages);
        // The snapshot reads the tree as it was, and no read after the
        // delete finds it.
        let old = db.get(&[b"top".as_slice(), b"t\x07"][..], &[9], Some(&before), v);
        assert_eq!(old.unwrap().expect("the item"), Element::new_sum_item(9));
        let gone = db.get(&[b"top".as_slice()][..], &7u32.to_be_bytes(), None, v);
        assert!(gone.unwrap().is_err());
        // Nothing is reclaimed while the snapshot may read it.
        let round = db.engine().database().reclaim();
        assert_eq!((round.finished, round.waiting), (0, 1));
        assert_eq!(census(&db, v).left_cells, nodes - 1);

        drop(before);
        reclaimed(&db);
        let after = census(&db, v);
        assert_eq!(after.live_cells, empty.live_cells);
        assert_eq!((after.left_cells, after.dropped_merks), (0, 0));
        assert_eq!(after.left_over_entries, 0);
        assert_eq!(after.cells, empty.cells);
        assert!(after.tree_pages <= empty.tree_pages + 1, "{after:?}");
        let stats = db.engine().database().stats().reclaimed;
        assert_eq!((stats.merks, stats.nested), (1, 20));
        assert_eq!(stats.cells, nodes - 1);
        assert!(stats.pages_freed >= full.tree_pages - empty.tree_pages - 1);
        println!(
            "tree pages: {} before the tree, {} with it, {} after its delete, {} reclaimed; \
             {} pages freed, {} cells taken out",
            empty.tree_pages,
            full.tree_pages,
            dropped.tree_pages,
            after.tree_pages,
            stats.pages_freed,
            stats.cells
        );
        db.flush().expect("a flush");
    }
    check(dir.path(), v);
}

/// The drain's range delete on this engine: a prefix that is entered is
/// dropped with its records in every keyspace, one that is not entered
/// is nothing to it, and the prefix of all `0xFF` bytes is refused as on
/// RocksDB, where it stands among the prefixes: those before it are
/// dropped, and those after it are not.
#[test]
fn a_range_delete_of_a_prefix_drops_what_is_entered_under_it() {
    use grovedb_storage::{flat::PendingPrefixDrops, FlatStore};

    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        let engine = db.engine();
        let (left, absent) = ([7; 32], [9; 32]);
        // A subtree that no element leads to, with a record in each of
        // its keyspaces.
        let write = || {
            let tx = engine.start_transaction();
            let batch = <LigninEngine as Engine>::Batch::default();
            let handle = engine.subtree_by_prefix(left, Some(&batch), &tx).unwrap();
            (handle.put(b"data", b"1", None).unwrap()).expect("staging");
            (handle.aux().put(b"aux", b"2", None).unwrap()).expect("staging");
            (handle.meta().put(b"meta", b"3", None).unwrap()).expect("staging");
            drop(handle);
            (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
            (engine.commit_transaction(tx).unwrap()).expect("a commit");
        };
        let read = || {
            let tx = engine.start_read_transaction();
            let handle = engine.subtree_by_prefix(left, None, &tx).unwrap();
            let entered = handle.entry().expect("the directory answers").is_some();
            let records = [
                handle.get(b"data").unwrap().expect("a read"),
                handle.aux().get(b"aux").unwrap().expect("a read"),
                handle.meta().get(b"meta").unwrap().expect("a read"),
            ];
            (entered, records.iter().flatten().count())
        };
        write();
        assert_eq!(read(), (true, 3));
        let refused = engine.delete_prefix_ranges(&[absent, [0xFF; 32], left]);
        let refused = refused.expect_err("the prefix of all 0xFF bytes");
        assert!(refused.to_string().contains("all-0xFF"), "{refused}");
        assert_eq!(read(), (true, 3));
        (engine.delete_prefix_ranges(&[absent, left])).expect("a range delete");
        assert_eq!(read(), (false, 0));
        (engine.delete_prefix_ranges(&[left, absent])).expect("a second range delete");
        // Among the prefixes the refusal stands where its prefix does.
        write();
        let refused = engine.delete_prefix_ranges(&[left, [0xFF; 32]]);
        let refused = refused.expect_err("the prefix of all 0xFF bytes");
        assert!(refused.to_string().contains("all-0xFF"), "{refused}");
        assert_eq!(read(), (false, 0));
        // Entered again, the prefix holds what is written then.
        write();
        assert_eq!(read(), (true, 3));
        db.flush().expect("a flush");
    }
    check(dir.path(), v);
}

/// A walk that the transaction refuses fails the delete, which drops
/// nothing: here another open write scope of the transaction holds a
/// tree inside the tree that is deleted.
#[test]
fn a_delete_whose_walk_is_refused_fails_and_drops_nothing() {
    use grovedb_storage::{error::WriteScopeError, Error as StorageError, FlatStore};

    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = filled(dir.path(), v);
        let engine = db.engine();
        let top: &[&[u8]] = &[b"top"];
        let deep: &[&[u8]] = &[b"top", NEST, DEEP];
        let entered = |path: &[&[u8]]| {
            let tx = engine.start_read_transaction();
            let handle = engine.subtree(path.into(), None, &tx).unwrap();
            handle.entry().expect("the directory answers").is_some()
        };
        let root_before = db.root_hash(None, v).unwrap().expect("a root");
        let tx = db.start_transaction();
        let holder = engine.open_write_scope(&tx).expect("a scope");
        let held = LigninEngine::scope_transaction(&holder);
        let handle = engine.immediate_subtree(deep.into(), held).unwrap();
        (handle.delete(key(1), None).unwrap()).expect_err("a flat record among nodes");
        (handle.aux().put(b"k", b"v", None).unwrap()).expect("a record of the deepest tree");
        drop(handle);

        let refused = db.delete(ROOT, b"top", allowed(), Some(&tx), v).unwrap();
        let refused = refused.expect_err("a walk into what another scope holds");
        let overlap = StorageError::WriteScope(WriteScopeError::Overlap).to_string();
        assert!(refused.to_string().contains(&overlap), "{refused}");
        (engine.rollback_write_scope(holder)).expect("the scope is rolled back");
        let root = db.root_hash(Some(&tx), v).unwrap().expect("a root");
        assert_eq!(root, root_before);
        let found = db.get(deep, &key(1), Some(&tx), v).unwrap();
        found.expect("the deepest tree has what it had");

        // With nothing in its way the delete is made, and commits.
        (db.delete(ROOT, b"top", allowed(), Some(&tx), v).unwrap()).expect("the tree is deleted");
        assert!(entered(top) && entered(deep), "nothing is committed yet");
        (db.commit_transaction(tx).unwrap()).expect("a commit");
        assert!(!entered(top) && !entered(deep));
        db.flush().expect("a flush");
    }
    check(dir.path(), v);
}

/// The drops are of the look. A writer deletes a tree, and after its
/// group has looked and before the group is applied another writer's
/// delete of the same tree is published: the transaction refuses the drop
/// of a subtree with no entry, the engine looks again and states no drop,
/// and the delete is applied without the removals it holds for the
/// subtrees that went. In the first fixture its commit is a conflict,
/// and in the second it is made. The grove is as one delete leaves it.
#[test]
fn a_tree_that_another_commit_takes_after_the_look_is_not_dropped_again() {
    use std::sync::Arc;

    use super::super::{AFTER_A_LOOK, REFUSED};

    let v = GroveVersion::latest();
    // The tree that both delete holds a tree with elements, and then a
    // tree that holds such a tree: the writes that the first delete
    // staged two trees below the one that left are not handed over
    // either.
    let top: &[&[u8]] = &[b"top"];
    for (holder, key, keeps) in [(top, NEST, true), (ROOT, b"top".as_slice(), false)] {
        let dir = tempfile::tempdir().expect("a directory");
        let db = Arc::new(filled(dir.path(), v));
        let other = {
            let db = db.clone();
            Box::new(move || {
                (db.delete(holder, key, allowed(), None, v).unwrap()).expect("the other delete");
            })
        };
        let tx = db.start_transaction();
        REFUSED.take();
        AFTER_A_LOOK.set([super::apart(other)].into_iter().collect());
        let deleted = db.delete(holder, key, allowed(), Some(&tx), v).unwrap();
        assert!(AFTER_A_LOOK.take().is_empty(), "the group looked");
        let refused = REFUSED.take();
        assert!(
            refused
                .first()
                .is_some_and(|refused| refused.contains("not entered")),
            "the drop of a subtree that the other commit took: {refused:?}"
        );
        // The second look states no drop, and nothing of the group is of
        // a subtree any more: its removal of the element is taken, and
        // meets the other writer's where it commits.
        assert_eq!(refused.len(), 1, "{refused:?}");
        deleted.expect("the delete is applied with no drop");
        // What is left of the delete is the removal of a node that is
        // gone, which changes nothing. In the first fixture the tree is
        // in a tree, and the delete also wrote nodes above it with what
        // it read of them before the other writer's commit wrote them:
        // its commit is a conflict (`transactions.md` CF2a). In the
        // second the tree is the one node of the root Merk: the delete
        // wrote no other node, and it commits.
        let committed = db.commit_transaction(tx).unwrap();
        match keeps {
            true => {
                let lost = committed.expect_err("a node above was written by both");
                assert!(lost.is_commit_conflict(), "{lost}");
            }
            false => committed.expect("the removal of a node that is gone"),
        }
        let gone = db.get(holder, key, None, v).unwrap();
        assert!(matches!(gone, Err(Error::PathKeyNotFound(_))), "{gone:?}");
        let root = db.root_hash(None, v).unwrap().expect("a root");
        let alone = {
            let dir = tempfile::tempdir().expect("a directory");
            let db = filled(dir.path(), v);
            (db.delete(holder, key, allowed(), None, v).unwrap()).expect("a delete");
            db.root_hash(None, v).unwrap().expect("a root")
        };
        assert_eq!(root, alone);
        db.flush().expect("a flush");
        drop(db);
        check(dir.path(), v);
    }
}

/// The entry of the one Merk of a tree stands for its element: a group
/// that writes a record of an entered tree and states no element reads
/// no node of the Merk above, so it is applied while another open write
/// scope of its transaction holds that Merk. A tree that is not entered
/// has no entry to stand for its element, which is read, and the scope's
/// claim refuses the group.
#[test]
fn a_record_of_an_entered_tree_is_written_beside_a_scope_that_holds_the_merk_above() {
    use grovedb_storage::{error::WriteScopeError, Error as StorageError, FlatStore};

    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = filled(dir.path(), v);
        let engine = db.engine();
        let entered: &[&[u8]] = &[b"top", NEST];
        let never: &[&[u8]] = &[b"top", b"never"];
        let tx = db.start_transaction();
        let holder = engine.open_write_scope(&tx).expect("a scope");
        let held = LigninEngine::scope_transaction(&holder);
        // The scope holds the Merk that holds the elements of both trees.
        let above: &[&[u8]] = &[b"top"];
        (db.insert(
            above,
            b"held",
            Element::new_item(vec![1]),
            None,
            Some(held),
            v,
        ))
        .unwrap()
        .expect("a node of the Merk above");
        let writer = engine.open_write_scope(&tx).expect("a second scope");
        let view = LigninEngine::scope_transaction(&writer);
        let put = |path: &[&[u8]]| {
            let batch = <LigninEngine as Engine>::Batch::default();
            let handle = engine.subtree(path.into(), Some(&batch), view).unwrap();
            (handle.aux().put(b"a", b"1", None).unwrap()).expect("staging");
            drop(handle);
            engine.apply_batch(batch, view).unwrap().map(|_| ())
        };
        put(entered).expect("a record of an entered tree");
        // By its prefix, the same: this group states no drop, and reads
        // the entry of the one subtree it writes and no entry above it.
        {
            let prefix = grovedb_storage::prefix::build_prefix(entered.into()).value;
            let batch = <LigninEngine as Engine>::Batch::default();
            let handle = engine
                .subtree_by_prefix(prefix, Some(&batch), view)
                .unwrap();
            (handle.aux().put(b"b", b"2", None).unwrap()).expect("staging");
            drop(handle);
            (engine.apply_batch(batch, view).unwrap()).expect("a record by prefix");
        }
        let refused = put(never).expect_err("the element of a tree that is not entered");
        assert!(
            matches!(refused, StorageError::WriteScope(WriteScopeError::Overlap)),
            "{refused}"
        );
        engine.merge_write_scope(writer).expect("a merge");
        engine.merge_write_scope(holder).expect("a merge");
        (db.commit_transaction(tx).unwrap()).expect("a commit");
        let reader = engine.start_read_transaction();
        let handle = engine.subtree(entered.into(), None, &reader).unwrap();
        let read = handle.aux().get(b"a").unwrap().expect("a read");
        assert_eq!(read, Some(b"1".to_vec()));
        let read = handle.aux().get(b"b").unwrap().expect("a read");
        assert_eq!(read, Some(b"2".to_vec()), "the record by prefix");
        drop(handle);
        drop(reader);
        db.flush().expect("a flush");
    }
    check(dir.path(), v);
}

/// A clear that is told to look for no tree, over a tree `p` that holds
/// an item and a tree `u` with two items, and what is read where `u` was:
/// whether a key of `u` is read by its path with no element asked for on
/// the way, before and after a tree is made under the key again,
/// whether that tree is empty, and what the grove's own check finds.
/// Gives every call, and the reads apart.
fn where_a_clear_took_a_tree<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
) -> (Vec<Answer>, Vec<Answer>) {
    let mut h = History::new(db, v);
    let p: &[&[u8]] = &[b"p"];
    let u: &[&[u8]] = &[b"p", b"u"];
    h.write(
        "made",
        db.insert(ROOT, b"p", Element::empty_tree(), None, None, v),
    );
    h.write(
        "made",
        db.insert(p, b"u", Element::empty_tree(), None, None, v),
    );
    let item = |n: u8| Element::new_item(vec![n; 9]);
    h.write("item", db.insert(p, b"i", item(0), None, None, v));
    for n in 1..3 {
        h.write("item", db.insert(u, &key(n), item(n), None, None, v));
    }
    let unchecked = ClearOptions {
        check_for_subtrees: false,
        backwards_references: BackwardsReferences::DontCheck,
        ..Default::default()
    };
    h.plain("cleared", db.clear_subtree(p, Some(unchecked), None, v));
    h.read("root", db.root_hash(None, v));
    h.read("empty", db.is_empty_tree(p, None, v));
    let mut reads = History::new(db, v);
    reads.read("has/p/u/i1", db.has_raw(u, &key(1), None, v));
    h.write(
        "made",
        db.insert(p, b"u", Element::empty_tree(), None, None, v),
    );
    reads.read("has/p/u/i1 again", db.has_raw(u, &key(1), None, v));
    reads.read("empty/p/u", db.is_empty_tree(u, None, v));
    verified(&mut reads);
    (h.answers, reads.answers)
}

/// The reads of [`where_a_clear_took_a_tree`], each with the answer that
/// is required, which is RocksDB's, and with the one LigninDB gives until
/// `GROVE-03-F1` closes the difference (D-236). They are a part of that
/// task's acceptance list: once the nodes of a tree whose element went
/// without a delete are kept as records that no tree links, LigninDB
/// gives the second column, and the third goes.
const READS_WHERE_A_CLEAR_TOOK_A_TREE: [(&str, &str, &str); 4] = [
    ("has/p/u/i1", "Ok(true)", "Ok(false)"),
    ("has/p/u/i1 again", "Ok(true)", "Ok(false)"),
    ("empty/p/u", "Ok(false)", "Ok(true)"),
    // The grove's own check walks the tree that was made again by its
    // records, and on RocksDB meets those of the tree that went.
    ("verified", "an error", "Ok([])"),
];

/// A known difference, and no property of the engine (D-236,
/// `GROVE-03-F1`): where a clear that was told to look for no tree
/// removed the element of a tree, RocksDB still reads the records of
/// that tree, and LigninDB, which dropped its subtree, does not.
/// RocksDB's answers are the required ones and are held here on RocksDB.
/// What LigninDB answers today is held beside them, so that the
/// difference is the one that is written down. Every other call of the
/// history answers and costs the same on both.
#[test]
fn lignindb_does_not_yet_read_what_rocksdb_reads_where_a_clear_took_a_tree() {
    let answers = |required: bool| -> Vec<(String, String)> {
        let gave = |&(read, rocksdb, lignindb): &(&str, &str, &str)| {
            let gave = if required { rocksdb } else { lignindb };
            (read.to_owned(), gave.to_owned())
        };
        READS_WHERE_A_CLEAR_TOOK_A_TREE.iter().map(gave).collect()
    };
    let gave = |reads: &[Answer]| -> Vec<(String, String)> {
        let reads = reads.iter();
        let gave = |(call, _, gave): &Answer| match gave.starts_with("Err(") {
            true if call == "verified" => (call.clone(), "an error".to_owned()),
            _ => (call.clone(), gave.clone()),
        };
        reads.map(gave).collect()
    };
    for v in GROVE_VERSIONS.iter() {
        let dir = tempfile::tempdir().expect("a directory");
        let (on_lignin, read) = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let answers = where_a_clear_took_a_tree(&db, v);
            db.flush().expect("a flush");
            answers
        };
        check(dir.path(), v);
        let made = on_lignin.iter().all(|(.., said)| said.starts_with("Ok("));
        assert!(made, "every call of the history is made: {on_lignin:?}");
        // The known difference: nothing of the tree that went is read.
        assert_eq!(gave(&read), answers(false));
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                .expect("a database on RocksDB");
            let (on_rocksdb, required) = where_a_clear_took_a_tree(&db, v);
            assert_eq!(on_lignin, on_rocksdb);
            // The required answers: the records of the tree are there.
            assert_eq!(gave(&required), answers(true));
        }
    }
}

/// A tree `t` that holds one aux record, put through its path at the
/// seam, is deleted through GroveDB and made again. Gives what is read of
/// the record under the tree that was made again.
fn an_aux_record_under_a_tree_made_again<E: Engine>(dir: &std::path::Path) -> Option<Vec<u8>> {
    use grovedb_storage::FlatStore;

    use crate::engine::SubtreeHandle;

    let v = GroveVersion::latest();
    let db = GroveDb::<E>::open_with_engine(dir).expect("a database is made");
    let tree: &[&[u8]] = &[b"t"];
    let make = || {
        (db.insert(ROOT, b"t", Element::empty_tree(), None, None, v)
            .unwrap())
        .expect("a tree");
    };
    make();
    let engine = db.engine();
    {
        let tx = engine.start_transaction();
        let batch = E::Batch::default();
        let handle = engine.subtree(tree.into(), Some(&batch), &tx).unwrap();
        (handle.aux().put(b"k", b"12345678", None).unwrap()).expect("staging");
        drop(handle);
        (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
        (engine.commit_transaction(tx).unwrap()).expect("a commit");
    }
    (db.delete(ROOT, b"t", None, None, v).unwrap()).expect("the delete of an empty tree");
    make();
    let read = {
        let tx = engine.start_read_transaction();
        let handle = engine.subtree(tree.into(), None, &tx).unwrap();
        handle.aux().get(b"k").unwrap().expect("a read")
    };
    db.flush().expect("a flush");
    read
}

/// A difference by design, and not parity (`transactions.md` CV9, D-241):
/// a drop takes the records of a subtree in every keyspace. The delete of
/// a tree on RocksDB removes the records of its data keyspace and no
/// other, so an aux record that was put under the tree's prefix is still
/// read under a tree that is made again at the key. On LigninDB the
/// subtree left with its element, and the tree that is made again holds
/// nothing. No operation of GroveDB puts an aux or a meta record under
/// the prefix of a tree: the record here is put at the seam.
#[test]
fn the_aux_records_of_a_deleted_tree_go_with_it_where_rocksdb_keeps_them() {
    let dir = tempfile::tempdir().expect("a directory");
    let on_lignin = an_aux_record_under_a_tree_made_again::<LigninEngine>(dir.path());
    assert_eq!(on_lignin, None);
    check(dir.path(), GroveVersion::latest());
    #[cfg(feature = "rocksdb_storage")]
    {
        let dir = tempfile::tempdir().expect("a directory");
        let on_rocksdb = an_aux_record_under_a_tree_made_again::<crate::RocksDbEngine>(dir.path());
        assert_eq!(on_rocksdb.as_deref(), Some(b"12345678".as_slice()));
    }
}

/// What a group drops and what it leaves out, by what its batches say,
/// in groups that are made at the seam by writes of the root Merk and
/// rolled back.
///
/// - A tree whose element a batch removes is dropped where the batch was
///   told the path of the subtree that the element is in: through the
///   handle that removes it, or through another handle of that subtree.
/// - One that a later batch of the group puts again, as the tree it is,
///   is not removed by the group.
/// - One that is removed in a group which was told no path of that
///   subtree leads the engine to no subtree.
/// - What a later batch puts into the subtree of a tree that is dropped
///   is not handed over. What it puts into the subtree of a removed tree
///   that stays entered is.
/// - Removals that a later batch holds for subtrees that are not
///   entered, of a removed tree, below it and beside it, are not handed
///   over, and nothing is entered for them. Puts into them are refused
///   with the group: no element is there to enter them under.
/// - An item that is written over the tree by a batch which also holds
///   the removals of the tree's records drops the subtree.
/// - A group that removes the element of an entered tree and in which a
///   batch enters a subtree itself is refused, and changes nothing.
#[test]
fn a_group_drops_what_the_directory_has_under_the_nodes_it_removes() {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::{
        store::lignin::LigninSubtree, tree::kv::ValueDefinedCostType, Merk, Op, TreeFeatureType,
        TreeType,
    };
    use grovedb_storage::{
        prefix::{build_prefix, secondary_prefix_for},
        Error as StorageError, FlatStore,
    };

    const NO_COST: Option<fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>> = None;
    type Batch = <LigninEngine as Engine>::Batch;
    type Tx<'a> = <LigninEngine as Engine>::Transaction<'a>;
    // A write of the node `key` of the root Merk, through a handle by
    // path or by prefix.
    fn root_write(
        engine: &LigninEngine,
        batch: &Batch,
        tx: &Tx<'_>,
        by_path: bool,
        key: &[u8],
        op: Op,
    ) {
        let v = GroveVersion::latest();
        let root = match by_path {
            true => engine.subtree(ROOT.into(), Some(batch), tx),
            false => engine.subtree_by_prefix([0; 32], Some(batch), tx),
        };
        let mut merk = Merk::open_base(root.unwrap(), TreeType::NormalTree, NO_COST, v)
            .unwrap()
            .expect("the root Merk");
        (merk.apply::<_, Vec<u8>>(&[(key.to_vec(), op)], &[], None, v))
            .unwrap()
            .expect("a write of the root Merk");
    }
    fn entered(engine: &LigninEngine, tx: &Tx<'_>, prefix: [u8; 32]) -> bool {
        let handle = engine.subtree_by_prefix(prefix, None, tx).unwrap();
        handle.entry().expect("the directory answers").is_some()
    }
    fn entered_at(engine: &LigninEngine, tx: &Tx<'_>, path: &[&[u8]]) -> bool {
        let handle = engine.subtree(path.into(), None, tx).unwrap();
        handle.entry().expect("the directory answers").is_some()
    }
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        let tree: &[&[u8]] = &[b"t"];
        let never: &[&[u8]] = &[b"n"];
        let below: &[&[u8]] = &[b"n", b"u"];
        for key in [b"a", b"n", b"t"] {
            (db.insert(ROOT, key, Element::empty_tree(), None, None, v)
                .unwrap())
            .expect("a tree");
        }
        (db.insert(tree, b"i", Element::new_item(vec![1]), None, None, v)
            .unwrap())
        .expect("an item");
        let engine = db.engine();
        let own = build_prefix(tree.into()).value;
        let beside = {
            let own = build_prefix(never.into()).value;
            secondary_prefix_for(&own, IndexAxis::Count.tag()).value
        };
        // The element of the tree as it is, with the root key it has: an
        // empty tree over it would be an element written over a tree.
        let again = || {
            let again = Element::new_tree(Some(b"i".to_vec()));
            let again = again.serialize(v).expect("bytes");
            Op::Put(again, TreeFeatureType::BasicMerkNode)
        };
        // The entry of the tree `t` once a group of `batches` is applied.
        let after = |batches: &dyn Fn(usize, &Batch, &Tx<'_>), count: usize| {
            let tx = engine.start_transaction();
            let mut group = <LigninEngine as Engine>::WriteGroup::default();
            for part in 0..count {
                let batch = Batch::default();
                batches(part, &batch, &tx);
                (engine.settle_batch(&mut group, batch).unwrap()).expect("a settled batch");
            }
            let applied = engine.apply_write_group(group, &tx).unwrap();
            let of_tree = entered(engine, &tx, own);
            let others = [never, below].map(|path| entered_at(engine, &tx, path));
            let others = others.contains(&true) || entered(engine, &tx, beside);
            let late = {
                let handle = engine.subtree_by_prefix(own, None, &tx).unwrap();
                handle.aux().get(b"late").unwrap().expect("a read")
            };
            engine.rollback_transaction(&tx).expect("a rollback");
            (applied.map(|_| ()), of_tree, others, late.is_some())
        };
        let late = |batch: &Batch, tx: &Tx<'_>, by_path: bool| {
            let handle = match by_path {
                true => engine.subtree(tree.into(), Some(batch), tx),
                false => engine.subtree_by_prefix(own, Some(batch), tx),
            };
            (handle.unwrap().aux().put(b"late", b"v", None).unwrap()).expect("staging");
        };

        // Removed through its path.
        let (applied, entered_after, ..) = after(
            &|_, batch, tx| root_write(engine, batch, tx, true, b"t", Op::Delete),
            1,
        );
        applied.expect("a group");
        assert!(!entered_after, "removed through its path");

        // Put again by a later batch.
        let (applied, entered_after, ..) = after(
            &|part, batch, tx| match part {
                0 => root_write(engine, batch, tx, true, b"t", Op::Delete),
                _ => root_write(engine, batch, tx, true, b"t", again()),
            },
            2,
        );
        applied.expect("a group");
        assert!(entered_after, "put again by a later batch");

        // Removed in a group that was told no path of the root.
        let (applied, entered_after, ..) = after(
            &|_, batch, tx| root_write(engine, batch, tx, false, b"t", Op::Delete),
            1,
        );
        applied.expect("a group");
        assert!(entered_after, "removed with no path");

        // Removed through a handle by prefix, in a batch that another
        // handle told the path of the root.
        let (applied, entered_after, ..) = after(
            &|_, batch, tx| {
                root_write(engine, batch, tx, false, b"t", Op::Delete);
                let root = engine.subtree(ROOT.into(), Some(batch), tx).unwrap();
                (root.aux().put(b"k", b"v", None).unwrap()).expect("staging");
            },
            1,
        );
        applied.expect("a group");
        assert!(!entered_after, "the batch was told the path of the root");

        // Dropped, and a later batch puts a record into its subtree.
        for by_path in [true, false] {
            let (applied, entered_after, _, late) = after(
                &|part, batch, tx| match part {
                    0 => root_write(engine, batch, tx, true, b"t", Op::Delete),
                    _ => late(batch, tx, by_path),
                },
                2,
            );
            applied.expect("a group");
            assert!(!entered_after && !late, "a put into a dropped subtree");
        }

        // Removed with no path, so that its subtree stays, and a later
        // batch puts a record into that subtree: the record is written.
        let (applied, entered_after, _, late) = after(
            &|part, batch, tx| match part {
                0 => root_write(engine, batch, tx, false, b"t", Op::Delete),
                _ => late(batch, tx, false),
            },
            2,
        );
        applied.expect("a group");
        assert!(entered_after && late, "a put into a subtree that stays");

        // A tree that nothing was entered for is removed, and a later
        // batch holds a write for its subtree, for one below it and for
        // one beside it: removals, and then puts.
        let others = |put: bool| {
            after(
                &|part, batch, tx| match part {
                    0 => root_write(engine, batch, tx, true, b"n", Op::Delete),
                    _ => {
                        let write = |handle: <LigninEngine as Engine>::Subtree<'_>| {
                            let aux = handle.aux();
                            let staged = match put {
                                true => aux.put(b"k", b"v", None),
                                false => aux.delete(b"k", None),
                            };
                            staged.unwrap().expect("staging");
                        };
                        for path in [never, below] {
                            write(engine.subtree(path.into(), Some(batch), tx).unwrap());
                        }
                        let handle = engine.subtree_beside(never.into(), beside, Some(batch), tx);
                        write(handle.unwrap());
                    }
                },
                2,
            )
        };
        let (applied, _, entered_after, _) = others(false);
        applied.expect("removals in subtrees that are not entered");
        assert!(!entered_after, "nothing is entered for a removal");
        let (applied, _, entered_after, _) = others(true);
        let refused = applied.expect_err("puts under a key that holds no element");
        assert!(
            matches!(refused, StorageError::NotSupported(_)),
            "{refused}"
        );
        assert!(!entered_after, "a refused group changed nothing");

        // An item is written over the tree, in a batch that also holds
        // the removals of the tree's records, as a clear of its
        // keyspace stages them: the look for what is written over drops
        // the subtree, and the removals are left out.
        let (applied, entered_after, ..) = after(
            &|_, batch, tx| {
                let handle = engine.subtree(tree.into(), Some(batch), tx).unwrap();
                (FlatStore::clear(&handle).unwrap()).expect("staging");
                drop(handle);
                let item = Element::new_item(vec![9]).serialize(v).expect("bytes");
                let over = Op::Put(item, TreeFeatureType::BasicMerkNode);
                root_write(engine, batch, tx, true, b"t", over);
            },
            1,
        );
        applied.expect("an item over a tree whose records the group removes");
        assert!(!entered_after, "written over, with its records removed");

        // The tree is removed, and a batch enters its subtree itself.
        let (applied, entered_after, ..) = after(
            &|part, batch, tx| match part {
                0 => root_write(engine, batch, tx, true, b"t", Op::Delete),
                _ => {
                    (batch.enter(&LigninSubtree::alone(own), b"tree")).expect("an enter");
                    let handle = engine.subtree_by_prefix(own, Some(batch), tx).unwrap();
                    (handle.aux().put(b"k", b"v", None).unwrap()).expect("staging");
                }
            },
            2,
        );
        let refused = applied.expect_err("a group that writes the directory itself");
        assert!(
            matches!(refused, StorageError::NotSupported(_)),
            "{refused}"
        );
        assert!(entered_after, "a refused group changed nothing");
        let item = db.get(tree, b"i", None, v).unwrap().expect("the item");
        assert_eq!(item, Element::new_item(vec![1]));
        db.flush().expect("a flush");
    }
    check(dir.path(), v);
}

/// Open (`engine.md`, "A subtree leaves with its element"): a commit that
/// enters the subtree of a tree after the look of a delete which found
/// it not entered. The delete states no drop for a subtree that is
/// absent, and the two wrote no key in common, so both commit: the entry
/// is left with no element, where RocksDB keeps a record that no element
/// leads to. The drain's range delete takes it.
#[test]
fn a_subtree_that_another_commit_enters_after_the_look_of_its_delete_outlives_its_element() {
    use std::sync::Arc;

    use grovedb_storage::{flat::PendingPrefixDrops, prefix::build_prefix, FlatStore};

    use super::super::AFTER_A_LOOK;

    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    {
        let db = Arc::new(Lignin::open_with_engine(dir.path()).expect("a database is made"));
        for key in [b"kept", b"tree"] {
            (db.insert(ROOT, key, Element::empty_tree(), None, None, v)
                .unwrap())
            .expect("a tree");
        }
        let tree: &[&[u8]] = &[b"tree"];
        let other = {
            let db = db.clone();
            Box::new(move || {
                let engine = db.engine();
                let tx = engine.start_transaction();
                let batch = <LigninEngine as Engine>::Batch::default();
                let handle = engine.subtree(tree.into(), Some(&batch), &tx).unwrap();
                (handle.aux().put(b"k", b"v", None).unwrap()).expect("staging");
                drop(handle);
                (engine.apply_batch(batch, &tx).unwrap()).expect("a batch");
                (engine.commit_transaction(tx).unwrap()).expect("a commit");
            })
        };
        let tx = db.start_transaction();
        AFTER_A_LOOK.set([super::apart(other)].into_iter().collect());
        let deleted = db.delete(ROOT, b"tree", None, Some(&tx), v).unwrap();
        assert!(AFTER_A_LOOK.take().is_empty(), "the group looked");
        deleted.expect("the delete of a tree that holds nothing");
        (db.commit_transaction(tx).unwrap()).expect("no key was written by both");
        let gone = db.get(ROOT, b"tree", None, v).unwrap();
        assert!(matches!(gone, Err(Error::PathKeyNotFound(_))), "{gone:?}");
        let engine = db.engine();
        let own = build_prefix(tree.into()).value;
        let entered = || {
            let tx = engine.start_read_transaction();
            let handle = engine.subtree_by_prefix(own, None, &tx).unwrap();
            handle.entry().expect("the directory answers").is_some()
        };
        assert!(entered(), "the entry outlives its element");
        (engine.delete_prefix_ranges(&[own])).expect("a range delete");
        assert!(!entered());
        db.flush().expect("a flush");
    }
    check(dir.path(), v);

    // On RocksDB the two commit as well, the delete begun before the
    // other writer's commit and made after it, and the record stays
    // under the prefix of the tree that is gone.
    #[cfg(feature = "rocksdb_storage")]
    {
        let dir = tempfile::tempdir().expect("a directory");
        let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
            .expect("a database on RocksDB");
        for key in [b"kept", b"tree"] {
            (db.insert(ROOT, key, Element::empty_tree(), None, None, v)
                .unwrap())
            .expect("a tree");
        }
        let tree: &[&[u8]] = &[b"tree"];
        let engine = db.engine();
        let tx = db.start_transaction();
        {
            let other = engine.start_transaction();
            let batch = <crate::RocksDbEngine as Engine>::Batch::default();
            let handle = engine.subtree(tree.into(), Some(&batch), &other).unwrap();
            (handle.aux().put(b"k", b"v", None).unwrap()).expect("staging");
            drop(handle);
            (engine.apply_batch(batch, &other).unwrap()).expect("a batch");
            (engine.commit_transaction(other).unwrap()).expect("a commit");
        }
        let deleted = db.delete(ROOT, b"tree", None, Some(&tx), v).unwrap();
        deleted.expect("the delete of a tree that holds nothing");
        (db.commit_transaction(tx).unwrap()).expect("no key was written by both");
        let gone = db.get(ROOT, b"tree", None, v).unwrap();
        assert!(matches!(gone, Err(Error::PathKeyNotFound(_))), "{gone:?}");
        let own = build_prefix(tree.into()).value;
        let read = engine.start_read_transaction();
        let handle = engine.subtree_by_prefix(own, None, &read).unwrap();
        let left = handle.aux().get(b"k").unwrap().expect("a read");
        assert_eq!(left.as_deref(), Some(b"v".as_slice()));
    }
}
