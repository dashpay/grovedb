//! States that GroveDB writes on a grove version which is frozen for the
//! chains built with it, and what the physical checker of LigninDB says
//! of them (VER-07-F1). Each history is written once, for any engine, and
//! runs on LigninDB and on RocksDB on every grove version.
//!
//! - **The delete of a tree that holds elements** on `GROVE_V1` to
//!   `GROVE_V3` (GroveDB #686) leaves the tree it was in with a value hash
//!   that is not the one Merk's rules make, where the nodes of that tree
//!   have their aggregate in their hashes and those of the deleted tree
//!   have none. RocksDB holds that hash and LigninDB holds the same. The
//!   check of a close does not fail on it, the rules that name it count it
//!   apart, at the paths the grove's own check reports, and the rules
//!   that name none find it. An insert below the tree puts the hash of
//!   the rules.
//! - What the other frozen paths leave that is a hash: a tree that
//!   `GROVE_V1` and `GROVE_V2` insert as a plain value, the commitment of
//!   an append-only tree that a clear on `GROVE_V1` to `GROVE_V3` keeps,
//!   and a reference whose target was written again.

use std::{
    collections::{BTreeMap, BTreeSet},
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
};

use grovedb_costs::OperationCost;
use grovedb_merk::{
    element::{costs::ElementCostExtensions, tree_type::ElementTreeTypeExtensions},
    store::lignin::hash::MerkHashRules,
    tree::{combine_hash, value_hash, NULL_HASH},
    Merk, TreeType,
};
use grovedb_storage::prefix::build_prefix;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignindb::{verify::Report, Db, Options};

use super::{
    append_only::{verified, Answer, Appended, Commitment, History},
    nested::{made_by, KINDS},
    sync, Lignin, LigninEngine,
};
use crate::{
    engine::Engine, operations::delete::DeleteOptions, reference_path::ReferencePathType,
    replication::RestoreCommitMode, Element, GroveDb,
};

const ROOT: &[&[u8]] = &[];
const TOP: &[u8] = b"top";
const PARENT: &[u8] = b"parent";
const CHILD: &[u8] = b"child";
const AT_PARENT: &[&[u8]] = &[TOP, PARENT];
const AT_CHILD: &[&[u8]] = &[TOP, PARENT, CHILD];

type Hash = [u8; 32];

fn allowed() -> Option<DeleteOptions> {
    Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    })
}

/// Where the element of a tree is: the prefix of the Merk it is in, and
/// its key.
type Placed = (Hash, Vec<u8>);

fn placed(path: &[Vec<u8>]) -> Placed {
    let (key, above) = path.split_last().expect("the path of an element");
    let above: Vec<&[u8]> = above.iter().map(Vec::as_slice).collect();
    (build_prefix(above.as_slice().into()).unwrap(), key.clone())
}

/// The paths at which the grove's own check finds a value hash that is
/// not the one it makes, with the hash it makes and the one it found.
fn issues<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<(Vec<Vec<u8>>, Hash, Hash)> {
    let issues = db.verify_grovedb(None, true, false, v).expect("a check");
    let mut issues: Vec<_> = issues
        .into_iter()
        .map(|(path, (_, expected, stored))| (path, expected, stored))
        .collect();
    issues.sort();
    issues
}

/// What the checker of LigninDB says of the database in `dir`, which
/// nothing has open, with Merk's rules for a check on the grove version
/// `v`: those that name the value hash a delete left, or those that name
/// none.
fn report(dir: &Path, v: &GroveVersion, naming: bool) -> Report {
    let db = Db::open(dir, &Options::default()).expect("the database opens again");
    db.check_kinds_by(grovedb_merk::store::lignin::keeps_flat_records);
    let rules = MerkHashRules::for_check(v);
    let mut rules = match naming {
        true => rules.naming_left_hashes(),
        false => rules,
    };
    db.close_and_verify(Some(&mut rules)).expect("a close")
}

/// The database in `dir` holds the value hash that a delete left at
/// `left` and nowhere else, and nothing that is not sound: with the rules
/// that name such a hash the checker finds nothing and names those
/// places, and with the rules that name none it finds the value hash of
/// each of them and nothing else.
fn assert_left(dir: &Path, v: &GroveVersion, left: &BTreeSet<Placed>, told: &str) {
    let naming = report(dir, v, true);
    assert!(naming.is_sound(), "{told}: {:#?}", naming.findings);
    let other = &naming.other_hashes;
    assert_eq!(other.count as usize, other.first.len(), "{told}");
    let named: BTreeSet<Placed> = (other.first.iter())
        .map(|hash| {
            (
                hash.subtree.expect("a Merk of the directory"),
                hash.key.clone(),
            )
        })
        .collect();
    assert_eq!(&named, left, "{told}");

    let strict = report(dir, v, false);
    assert!(strict.other_hashes.is_empty(), "{told}");
    let found: BTreeSet<String> = (strict.findings.iter())
        .map(|finding| format!("{:?} at {}", finding.rule, finding.place))
        .collect();
    let expected: BTreeSet<String> = (other.first.iter())
        .map(|hash| format!("Hash(Value) at cell {}", hash.cell))
        .collect();
    assert_eq!(found, expected, "{told}");
    assert_eq!(strict.findings.len(), left.len(), "{told}");
    assert_eq!(strict.more_findings, 0, "{told}");
}

/// The value hash that `top` stores for `parent`, what it is with the
/// root of the Merk of `parent` hashed as that of a tree with no
/// aggregate in its hashes, and what Merk's rules make it.
fn hashes_of_parent<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> [Hash; 3] {
    let cost = Some(&Element::value_defined_cost_for_serialized_value);
    let top = db.get(ROOT, TOP, None, v).unwrap().expect("top");
    let (top_root, _) = top.root_key_and_tree_type_owned().expect("a tree");
    let element = db.get(&[TOP], PARENT, None, v).unwrap().expect("parent");
    let bytes = element.serialize(v).expect("bytes");
    let (root_key, own_type) = element.root_key_and_tree_type_owned().expect("a tree");
    let engine = db.engine();
    let transaction = engine.start_read_transaction();
    let at_top: &[&[u8]] = &[TOP];
    let handle = engine.subtree(at_top.into(), None, &transaction).unwrap();
    let merk = Merk::open_layered_with_root_key(handle, top_root, TreeType::NormalTree, cost, v)
        .unwrap()
        .expect("the Merk of top");
    let stored = merk
        .get_value_hash(PARENT, true, cost, v)
        .unwrap()
        .expect("a read")
        .expect("a node");
    let own = value_hash(&bytes).unwrap();
    let with_root_as = |as_type: TreeType| {
        let handle = engine.subtree(AT_PARENT.into(), None, &transaction);
        let merk =
            Merk::open_layered_with_root_key(handle.unwrap(), root_key.clone(), as_type, cost, v)
                .unwrap()
                .expect("the Merk of parent");
        combine_hash(&own, &merk.root_hash().unwrap()).unwrap()
    };
    [
        stored,
        with_root_as(TreeType::NormalTree),
        with_root_as(own_type),
    ]
}

/// The five calls of GroveDB #686 under a tree `top`: a provable count
/// tree `parent` with an item, a tree `child` in it with an item, and the
/// delete of `child` with what it holds.
fn deleted_under_a_provable_count_tree<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let tree = Element::empty_tree;
    h.write("made", db.insert(ROOT, TOP, tree(), None, None, v));
    let parent = Element::empty_provable_count_tree();
    h.write("made", db.insert(&[TOP], PARENT, parent, None, None, v));
    let sibling = Element::new_item(b"s".to_vec());
    h.write(
        "item",
        db.insert(AT_PARENT, b"sibling", sibling, None, None, v),
    );
    h.write("made", db.insert(AT_PARENT, CHILD, tree(), None, None, v));
    let leaf = Element::new_item(b"value".to_vec());
    h.write("item", db.insert(AT_CHILD, b"leaf", leaf, None, None, v));
    h.write(
        "delete tree",
        db.delete(AT_PARENT, CHILD, allowed(), None, v),
    );
    h.element("element", db.get(&[TOP], PARENT, None, v));
    verified(&mut h);
    h.answers
}

/// What one engine holds after a history that ends with `parent` under
/// `top`.
#[derive(Debug, PartialEq)]
struct Held {
    answers: Vec<Answer>,
    issues: Vec<(Vec<Vec<u8>>, Hash, Hash)>,
    hashes: [Hash; 3],
}

impl Held {
    fn of<E: Engine>(db: &GroveDb<E>, v: &GroveVersion, answers: Vec<Answer>) -> Self {
        assert!(
            answers
                .iter()
                .all(|(call, _, answer)| { call == "verified" || answer.starts_with("Ok(") }),
            "{answers:#?}"
        );
        Self {
            answers,
            issues: issues(db, v),
            hashes: hashes_of_parent(db, v),
        }
    }
}

/// Runs `$history` on LigninDB and, where the RocksDB engine is compiled
/// in, on RocksDB, on the grove version `$v`. Gives what LigninDB holds,
/// which is what RocksDB holds, with the directory of its database, which
/// was closed as a debug build closes one: with the check of the engine.
macro_rules! on_both {
    ($v:expr, $told:expr, $history:path) => {{
        let (v, told): (&GroveVersion, &str) = ($v, $told);
        let dir = tempfile::tempdir().expect("a directory");
        let on_lignin = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let held = Held::of(&db, v, $history(&db, v));
            db.flush().expect("a flush");
            held
        };
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                .expect("a database on RocksDB");
            let on_rocksdb = Held::of(&db, v, $history(&db, v));
            assert_eq!(on_lignin, on_rocksdb, "{told}");
        }
        let _ = told;
        (on_lignin, dir)
    }};
}

fn at_parent() -> BTreeSet<Placed> {
    BTreeSet::from([placed(&[TOP.to_vec(), PARENT.to_vec()])])
}

/// The five calls leave, on grove versions 1 to 3, one value hash that is
/// not the one of the rules: that of `parent` in the Merk of `top`, made
/// with the root of the Merk of `parent` hashed with no count. Both
/// engines hold it, the grove's own check reports its path on both, and
/// the checker of LigninDB names that cell and finds nothing. On grove
/// versions 4 and 5 the hash is the one of the rules.
#[test]
fn the_delete_of_a_tree_under_a_provable_count_tree_leaves_rocksdbs_hash_and_no_damage() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let told = format!("grove version {number}");
        let (held, dir) = on_both!(v, &told, deleted_under_a_provable_count_tree);
        let [stored, with_no_count, of_the_rules] = held.hashes;
        assert_ne!(with_no_count, of_the_rules, "{told}");
        let path = vec![TOP.to_vec(), PARENT.to_vec()];
        match number <= 3 {
            true => {
                assert_eq!(stored, with_no_count, "{told}");
                let reported = vec![(path, of_the_rules, with_no_count)];
                assert_eq!(held.issues, reported, "{told}");
                assert_left(dir.path(), v, &at_parent(), &told);
            }
            false => {
                assert_eq!(stored, of_the_rules, "{told}");
                assert_eq!(held.issues, vec![], "{told}");
                assert_left(dir.path(), v, &BTreeSet::new(), &told);
            }
        }
    }
}

fn later_item() -> Element {
    Element::new_item(b"later".to_vec())
}

/// The five calls, and then a write below `parent` on the same grove
/// version.
fn deleted_and_written_below<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut answers = deleted_under_a_provable_count_tree(db, v);
    let mut h = History::new(db, v);
    h.write(
        "item",
        db.insert(AT_PARENT, b"later", later_item(), None, None, v),
    );
    verified(&mut h);
    answers.extend(h.answers);
    answers
}

/// The five calls, and then a write beside `parent` by the newest grove
/// version, as a chain makes one that has moved on.
fn deleted_and_written_beside<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut answers = deleted_under_a_provable_count_tree(db, v);
    let newest = GroveVersion::latest();
    let mut h = History::new(db, newest);
    h.write(
        "item",
        db.insert(&[TOP], b"beside", later_item(), None, None, newest),
    );
    verified(&mut h);
    answers.extend(h.answers);
    answers
}

/// [`deleted_and_written_beside`], and then a write below `parent` by the
/// newest grove version.
fn deleted_and_written_below_later<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut answers = deleted_and_written_beside(db, v);
    let newest = GroveVersion::latest();
    let mut h = History::new(db, newest);
    h.write(
        "item",
        db.insert(AT_PARENT, b"later", later_item(), None, None, newest),
    );
    verified(&mut h);
    answers.extend(h.answers);
    answers
}

/// An insert beside `parent` leaves the hash that the delete left, also
/// by a later grove version, whose rules for a check name it all the
/// same. An insert below `parent` puts the hash of the rules, on either
/// engine and by either version, and then the checker names nothing and
/// finds nothing.
#[test]
fn an_insert_below_the_tree_puts_the_hash_of_the_rules() {
    let newest = GroveVersion::latest();
    for (number, v) in GROVE_VERSIONS.iter().enumerate().take(3) {
        let told = format!("grove version {}", number + 1);
        let (held, dir) = on_both!(v, &told, deleted_and_written_below);
        let [stored, _, of_the_rules] = held.hashes;
        assert_eq!(stored, of_the_rules, "{told}");
        assert_eq!(held.issues, vec![], "{told}");
        assert_left(dir.path(), v, &BTreeSet::new(), &told);

        let (held, dir) = on_both!(v, &told, deleted_and_written_beside);
        let [stored, with_no_count, of_the_rules] = held.hashes;
        assert_eq!(stored, with_no_count, "{told}");
        assert_ne!(stored, of_the_rules, "{told}");
        assert_eq!(held.issues.len(), 1, "{told}");
        assert_left(dir.path(), newest, &at_parent(), &told);

        let (held, dir) = on_both!(v, &told, deleted_and_written_below_later);
        let [stored, _, of_the_rules] = held.hashes;
        assert_eq!(stored, of_the_rules, "{told}");
        assert_eq!(held.issues, vec![], "{told}");
        assert_left(dir.path(), newest, &BTreeSet::new(), &told);
    }
}

/// What one case of the family did on one engine.
#[derive(Debug, PartialEq)]
struct Case {
    /// `deleted`, or `panic`.
    outcome: &'static str,
    /// What the delete was charged, and the root hash of the grove after
    /// it.
    charged: Option<(OperationCost, Hash)>,
    /// The paths the grove's own check reports.
    issues: Vec<Vec<Vec<u8>>>,
    /// The stored value hash of `parent`, the one with no aggregate in
    /// the hash of the root, and the one of the rules.
    hashes: Option<[Hash; 3]>,
}

/// The tree that a case of the family deletes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Child {
    /// A tree of a kind whose data is one Merk, with an item.
    Kind(usize),
    /// A tree that keeps flat records, with one value.
    Mmr,
    Bulk,
    Dense,
    Commitment,
}

impl Child {
    fn name(self) -> &'static str {
        match self {
            Self::Kind(kind) => KINDS[kind].name,
            Self::Mmr => "mmr tree",
            Self::Bulk => "bulk append tree",
            Self::Dense => "dense tree",
            Self::Commitment => "commitment tree",
        }
    }
}

/// One case of the family: a tree `parent` of the kind `p` with elements,
/// the tree `c` in it with one element, and the delete of that with what
/// it holds.
fn family_case<E: Engine>(db: &GroveDb<E>, v: &GroveVersion, p: usize, c: Child) -> Case {
    let ok = "a write of the setup";
    let insert = |at: &[&[u8]], key: &[u8], element: Element| {
        db.insert(at, key, element, None, None, v)
            .unwrap()
            .expect(ok);
    };
    insert(ROOT, TOP, Element::empty_tree());
    insert(&[TOP], PARENT, (KINDS[p].make)());
    insert(AT_PARENT, b"sibling", Element::new_item(b"s".to_vec()));
    if KINDS[p].sums {
        insert(AT_PARENT, b"sum", Element::new_sum_item(5));
    }
    let value = b"value".to_vec();
    match c {
        Child::Kind(c) => {
            insert(AT_PARENT, CHILD, (KINDS[c].make)());
            insert(AT_CHILD, b"leaf", Element::new_item(value));
        }
        Child::Mmr => {
            insert(AT_PARENT, CHILD, Element::empty_mmr_tree());
            let appended = db.mmr_tree_append(AT_PARENT, CHILD, value, None, v);
            appended.unwrap().expect(ok);
        }
        Child::Bulk => {
            let tree = Element::empty_bulk_append_tree(2).expect("a chunk power");
            insert(AT_PARENT, CHILD, tree);
            let appended = db.bulk_append(AT_PARENT, CHILD, value, None, v);
            appended.unwrap().expect(ok);
        }
        Child::Dense => {
            insert(AT_PARENT, CHILD, Element::empty_dense_tree(3));
            let inserted = db.dense_tree_insert(AT_PARENT, CHILD, value, None, v);
            inserted.unwrap().expect(ok);
        }
        Child::Commitment => {
            insert(AT_PARENT, CHILD, Commitment::empty(None));
            let inserted = Commitment::append(db, AT_PARENT, CHILD, 0, None, v);
            inserted.unwrap().expect(ok);
        }
    }
    let deleted = catch_unwind(AssertUnwindSafe(|| {
        db.delete(AT_PARENT, CHILD, allowed(), None, v)
    }));
    match deleted {
        Err(_) => Case {
            outcome: "panic",
            charged: None,
            issues: Vec::new(),
            hashes: None,
        },
        Ok(deleted) => {
            let cost = deleted.cost.clone();
            deleted.unwrap().expect("the delete is taken");
            let root = db.root_hash(None, v).unwrap().expect("a root hash");
            let issues = issues(db, v);
            Case {
                outcome: "deleted",
                charged: Some((cost, root)),
                issues: issues.into_iter().map(|(path, ..)| path).collect(),
                hashes: Some(hashes_of_parent(db, v)),
            }
        }
    }
}

/// The family of the delete: every kind of tree whose data is one Merk
/// that the grove version makes (`nested::KINDS`) as `parent`, with each
/// of those kinds, an MMR tree, a bulk append tree, a dense tree and a
/// commitment tree as the tree that is deleted from it. An indexed tree
/// is no `parent` and no deleted tree of the family.
///
/// On every case the two engines answer the delete alike, with the same
/// cost and the same root hash after it, and the
/// value hashes that the checker of LigninDB names as left by a delete
/// are at the paths that the grove's own check reports on each engine,
/// no more and no fewer; with the rules that name none, the checker finds
/// exactly those. On grove versions 1 to 3 that is one path where the
/// hashes of `parent` take its aggregate and those of the deleted tree
/// take none, and none otherwise; the delete of a tree whose hashes take
/// an aggregate from a tree of another kind panics in Merk on both
/// engines, and leaves a store that is sound. On grove versions 4 and 5
/// no case leaves such a hash.
#[test]
fn the_hashes_the_checker_names_are_the_paths_the_groves_check_reports() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let made = made_by(v);
        let mut classes: BTreeMap<&str, usize> = BTreeMap::new();
        for (p, parent) in KINDS.iter().enumerate().take(made) {
            let flat = [Child::Mmr, Child::Bulk, Child::Dense, Child::Commitment];
            for c in (0..made).map(Child::Kind).chain(flat) {
                let told = format!("grove version {number}, {} in {}", c.name(), parent.name);
                let dir = tempfile::tempdir().expect("a directory");
                let on_lignin = {
                    let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
                    let case = family_case(&db, v, p, c);
                    db.flush().expect("a flush");
                    case
                };
                #[cfg(feature = "rocksdb_storage")]
                {
                    let dir = tempfile::tempdir().expect("a directory");
                    let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                        .expect("a database on RocksDB");
                    assert_eq!(on_lignin, family_case(&db, v, p, c), "{told}");
                }
                let left: BTreeSet<Placed> =
                    on_lignin.issues.iter().map(|path| placed(path)).collect();
                assert_eq!(left.len(), on_lignin.issues.len(), "{told}");
                assert_left(dir.path(), v, &left, &told);

                let class = match (on_lignin.outcome, on_lignin.hashes) {
                    ("panic", _) => "panic",
                    (_, Some([stored, with_no_aggregate, of_the_rules])) => {
                        match (stored == of_the_rules, stored == with_no_aggregate) {
                            (true, _) => {
                                assert!(left.is_empty(), "{told}");
                                "of the rules"
                            }
                            (false, true) => {
                                assert_eq!(left, at_parent(), "{told}");
                                "left"
                            }
                            (false, false) => panic!("{told}: a hash that is neither"),
                        }
                    }
                    other => panic!("{told}: {other:?}"),
                };
                assert!(number <= 3 || class == "of the rules", "{told}: {class}");
                *classes.entry(class).or_default() += 1;
            }
        }
        let expected: &[(&str, usize)] = match number {
            1..=3 => &[("left", 36), ("of the rules", 49), ("panic", 32)],
            4 => &[("of the rules", 117)],
            _ => &[("of the rules", 192)],
        };
        let expected: BTreeMap<&str, usize> = expected.iter().copied().collect();
        assert_eq!(classes, expected, "grove version {number}");
    }
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    let text = payload.downcast_ref::<String>().cloned();
    text.or_else(|| payload.downcast_ref::<&str>().map(ToString::to_string))
        .unwrap_or_default()
}

/// A count indexed tree with rows under `top`, beside `beside` items, and
/// its delete with what it holds: how the delete ended.
fn indexed_tree_deleted_beside<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    beside: usize,
) -> String {
    let ok = "a write of the setup";
    let tree = Element::empty_tree();
    db.insert(ROOT, TOP, tree, None, None, v)
        .unwrap()
        .expect(ok);
    let indexed = Element::empty_provable_count_indexed_tree();
    db.insert(&[TOP], b"indexed", indexed, None, None, v)
        .unwrap()
        .expect(ok);
    for n in 0..3u8 {
        let row = Element::new_item(vec![n; 4]);
        db.insert_into_count_indexed_tree(&[TOP, b"indexed"], &[b'k', n], row, None, v)
            .unwrap()
            .expect(ok);
    }
    for n in 0..beside {
        let item = Element::new_item(vec![7; 3]);
        db.insert(&[TOP], &[b'i', n as u8], item, None, None, v)
            .unwrap()
            .expect(ok);
    }
    let deleted = catch_unwind(AssertUnwindSafe(|| {
        db.delete(&[TOP], b"indexed", allowed(), None, v).unwrap()
    }));
    match deleted {
        Ok(Ok(())) => "deleted".to_string(),
        Ok(Err(error)) => format!("refused: {error}"),
        Err(payload) => format!("panic: {}", panic_text(payload)),
    }
}

/// The panic of the same frozen path (GroveDB #686): on grove versions 1
/// to 3 the Merk a tree is deleted from is opened as a tree of the
/// deleted tree's type, and the hash of its root is then asked of nodes
/// that are not of that type. It needs no second indexed tree: a count
/// indexed tree with rows that is deleted from a plain tree which keeps
/// one item panics, on both engines alike, and one that is alone in its
/// tree is deleted. The store that the panic leaves is sound.
#[test]
fn an_indexed_tree_deleted_from_a_tree_that_keeps_an_element_panics_on_both_engines() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        for beside in [0, 1] {
            let told = format!("grove version {number}, beside {beside} items");
            let dir = tempfile::tempdir().expect("a directory");
            let on_lignin = {
                let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
                let said = indexed_tree_deleted_beside(&db, v, beside);
                db.flush().expect("a flush");
                said
            };
            assert_left(dir.path(), v, &BTreeSet::new(), &told);
            #[cfg(feature = "rocksdb_storage")]
            {
                let dir = tempfile::tempdir().expect("a directory");
                let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                    .expect("a database on RocksDB");
                assert_eq!(
                    on_lignin,
                    indexed_tree_deleted_beside(&db, v, beside),
                    "{told}"
                );
            }
            match (number <= 3, beside) {
                (true, 1) => {
                    assert!(on_lignin.starts_with("panic: "), "{told}: {on_lignin}");
                    assert!(on_lignin.contains("hash_for_link"), "{told}: {on_lignin}");
                    let inconsistent = "feature_type is inconsistent with its tree_type";
                    assert!(on_lignin.contains(inconsistent), "{told}: {on_lignin}");
                }
                _ => assert_eq!(on_lignin, "deleted", "{told}"),
            }
        }
    }
}

/// The three kinds of tree that `GROVE_V1` and `GROVE_V2` insert as plain
/// values, each inserted and never written into: whether each has the
/// hash of its bytes alone as its value hash.
fn trees_inserted_plain<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let cost = Some(&Element::value_defined_cost_for_serialized_value);
    let mut h = History::new(db, v);
    let trees = [
        (&b"count sum"[..], Element::empty_count_sum_tree()),
        (b"provable count", Element::empty_provable_count_tree()),
        (
            b"provable count sum",
            Element::empty_provable_count_sum_tree(),
        ),
    ];
    for (key, tree) in &trees {
        h.write("made", db.insert(ROOT, key, tree.clone(), None, None, v));
    }
    let engine = db.engine();
    let transaction = engine.start_read_transaction();
    let handle = engine.subtree(ROOT.into(), None, &transaction).unwrap();
    let merk = Merk::open_base(handle, TreeType::NormalTree, cost, v)
        .unwrap()
        .expect("the root Merk");
    for (key, tree) in &trees {
        let stored = merk.get_value_hash(key, true, cost, v).unwrap();
        let stored = stored.expect("a read").expect("a node");
        let own = value_hash(&tree.serialize(v).expect("bytes")).unwrap();
        let combined = combine_hash(&own, &NULL_HASH).unwrap();
        assert!(stored == own || stored == combined);
        h.plain("of its bytes alone", Ok(stored == own));
    }
    verified(&mut h);
    h.answers
}

/// How many of `answers` are of a call named `call` that answered
/// `answer`.
fn said(answers: &[Answer], call: &str, answer: &str) -> usize {
    let said = |(named, _, said): &&Answer| named == call && said == answer;
    answers.iter().filter(said).count()
}

/// A tree that `GROVE_V1` and `GROVE_V2` insert as a plain value has the
/// hash of its bytes until something is written below it, on both
/// engines, and the store is sound with it: Merk's rules for a check take
/// either hash of such a tree while it has no root.
#[test]
fn a_tree_inserted_as_a_plain_value_closes_sound() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let told = format!("grove version {number}");
        let dir = tempfile::tempdir().expect("a directory");
        let on_lignin = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let answers = trees_inserted_plain(&db, v);
            db.flush().expect("a flush");
            answers
        };
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                .expect("a database on RocksDB");
            assert_eq!(on_lignin, trees_inserted_plain(&db, v), "{told}");
        }
        let plain = said(&on_lignin, "of its bytes alone", "Ok(true)");
        assert_eq!(plain, if number <= 2 { 3 } else { 0 }, "{told}");
        // The grove's own check makes the combined hash for every tree,
        // and so reports the three on those versions, on both engines.
        let clean = said(&on_lignin, "verified", "Ok([])");
        assert_eq!(clean, usize::from(number > 2), "{told}: {on_lignin:#?}");
        assert_left(dir.path(), v, &BTreeSet::new(), &told);
    }
}

/// An MMR tree with values under `top`, cleared and left so.
fn append_only_tree_cleared<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let tree = Element::empty_tree();
    h.write("made", db.insert(ROOT, TOP, tree, None, None, v));
    let mmr = Element::empty_mmr_tree();
    h.write("made", db.insert(&[TOP], b"mmr", mmr, None, None, v));
    for n in 0..3u8 {
        let appended = db.mmr_tree_append(&[TOP], b"mmr", vec![n; 9], None, v);
        h.write("append", appended);
    }
    h.plain("cleared", db.clear_subtree(&[TOP, b"mmr"], None, None, v));
    h.read("root", db.root_hash(None, v));
    h.element("element", db.get(&[TOP], b"mmr", None, v));
    verified(&mut h);
    h.answers
}

/// The clear of an append-only tree on grove versions 1 to 3 keeps the
/// element with the commitment to what was cleared (GroveDB #893), and
/// the grove's own check reports the tree on both engines. The value hash
/// of such a tree is its writer's to state, so the store is sound.
#[test]
fn an_append_only_tree_cleared_on_the_frozen_path_closes_sound() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let told = format!("grove version {number}");
        let dir = tempfile::tempdir().expect("a directory");
        let on_lignin = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let answers = append_only_tree_cleared(&db, v);
            db.flush().expect("a flush");
            answers
        };
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                .expect("a database on RocksDB");
            assert_eq!(on_lignin, append_only_tree_cleared(&db, v), "{told}");
        }
        assert_eq!(said(&on_lignin, "cleared", "Ok(true)"), 1, "{told}");
        let clean = said(&on_lignin, "verified", "Ok([])");
        assert_eq!(clean, usize::from(number > 3), "{told}: {on_lignin:#?}");
        assert_left(dir.path(), v, &BTreeSet::new(), &told);
    }
}

/// A reference under `top` whose target is written again after it.
fn reference_left_behind<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let tree = Element::empty_tree();
    h.write("made", db.insert(ROOT, TOP, tree, None, None, v));
    let target = Element::new_item(b"first".to_vec());
    h.write("item", db.insert(&[TOP], b"target", target, None, None, v));
    let to_target =
        ReferencePathType::AbsolutePathReference(vec![TOP.to_vec(), b"target".to_vec()]);
    let reference = Element::new_reference(to_target);
    h.write(
        "reference",
        db.insert(&[TOP], b"ref", reference, None, None, v),
    );
    let again = Element::new_item(b"second".to_vec());
    h.write("item", db.insert(&[TOP], b"target", again, None, None, v));
    verified(&mut h);
    h.answers
}

/// A reference commits to its target as that was when the reference was
/// written. Once the target is written again the grove's own check
/// reports the reference, on both engines, and the store is sound: the
/// value hash of a reference is its writer's to state.
#[test]
fn a_reference_whose_target_was_written_again_closes_sound() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let told = format!("grove version {}", number + 1);
        let dir = tempfile::tempdir().expect("a directory");
        let on_lignin = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            let answers = reference_left_behind(&db, v);
            db.flush().expect("a flush");
            answers
        };
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                .expect("a database on RocksDB");
            assert_eq!(on_lignin, reference_left_behind(&db, v), "{told}");
        }
        let reported = format!("Ok([{:?}])", vec![TOP.to_vec(), b"ref".to_vec()]);
        assert_eq!(
            said(&on_lignin, "verified", &reported),
            1,
            "{told}: {on_lignin:#?}"
        );
        assert_left(dir.path(), v, &BTreeSet::new(), &told);
    }
}

/// How a session ended that syncs a new database on `T` from `source`.
/// The database is closed, and checked as `T` checks one.
fn synced<S: Engine, T: sync::Side>(source: &GroveDb<S>, v: &GroveVersion) -> String {
    let root = source.root_hash(None, v).unwrap().expect("a root hash");
    let dir = tempfile::tempdir().expect("a directory");
    let said = {
        let target = sync::open::<T>(dir.path());
        let said = (|| {
            let mode = RestoreCommitMode::Atomic;
            let mut session = match sync::begin(&target, root, mode, v) {
                Ok(session) => session,
                Err(error) => return format!("not begun: {error}"),
            };
            let mut requests = sync::Requests::new(source, root, v);
            let mut chunks = 0;
            while let Some((request, payload)) = requests.fetch() {
                if let Err(error) = requests.apply(&mut session, &request, &payload) {
                    return format!("chunk {chunks} refused: {error}");
                }
                chunks += 1;
            }
            if !session.is_sync_completed() {
                return format!("not completed after {chunks} chunks");
            }
            match target.commit_session(session, v) {
                Err(error) => format!("commit refused after {chunks} chunks: {error}"),
                Ok(()) => {
                    let synced = target.root_hash(None, v).unwrap().expect("a root hash");
                    assert_eq!(synced, root);
                    let issues = issues(&target, v);
                    let paths: Vec<_> = issues.into_iter().map(|(path, ..)| path).collect();
                    format!("committed after {chunks} chunks, issues at {paths:?}")
                }
            }
        })();
        target.flush().expect("a flush");
        said
    };
    T::check(dir.path(), v);
    said
}

/// How the sessions end that sync a new database on each engine from a
/// grove on `S` after the five calls, with a write below `parent` after
/// them if `written_below`.
fn synced_from<S: sync::Side>(v: &GroveVersion, written_below: bool) -> Vec<String> {
    let dir = tempfile::tempdir().expect("a directory");
    let source = sync::open::<S>(dir.path());
    match written_below {
        true => deleted_and_written_below(&source, v),
        false => deleted_under_a_provable_count_tree(&source, v),
    };
    let said = vec![synced::<S, LigninEngine>(&source, v)];
    #[cfg(feature = "rocksdb_storage")]
    let said = [said, vec![synced::<S, crate::RocksDbEngine>(&source, v)]].concat();
    source.flush().expect("a flush");
    said
}

/// What a session answers to its third chunk while the value hash of
/// `parent` is the one the delete left.
const NOT_THE_EXPECTED_ROOT: &str = "chunk 2 refused: internal error: Unable to process incoming \
                                     chunk: chunk restoring error invalid chunk proof: chunk \
                                     doesn't match expected root hash";

/// State sync of a grove that holds the hash a delete left is refused by
/// the session at its third chunk, with the same error on RocksDB as on
/// LigninDB and in every direction between them. The session commits
/// once, at its end (`RestoreCommitMode::Atomic`), so nothing is
/// committed, and the database that was to be synced closes sound. After an insert
/// below `parent` the grove syncs in every direction, as it does on grove
/// versions 4 and 5.
#[test]
fn state_sync_refuses_a_grove_with_the_hash_a_delete_left_alike_on_both_engines() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let directions = |written_below: bool| {
            let said = synced_from::<LigninEngine>(v, written_below);
            #[cfg(feature = "rocksdb_storage")]
            let said = [said, synced_from::<crate::RocksDbEngine>(v, written_below)].concat();
            said
        };
        let committed = |said: &[String]| {
            let first = &said[0];
            assert!(first.starts_with("committed after "), "{first}");
            assert!(first.ends_with(" chunks, issues at []"), "{first}");
            assert!(said.iter().all(|other| other == first), "{said:#?}");
        };
        let said = directions(false);
        match number <= 3 {
            true => assert!(
                said.iter().all(|said| said == NOT_THE_EXPECTED_ROOT),
                "grove version {number}: {said:#?}"
            ),
            false => committed(&said),
        }
        if number <= 3 {
            committed(&directions(true));
        }
    }
}

/// A caller that knows its grove was written on later grove versions only
/// has the close of a debug build fail on the hash a delete left
/// (`LigninEngine::check_closes_with_no_left_hashes`): on the versions
/// that leave it the close then panics with the rule of a value hash, and
/// on the others it passes.
#[test]
fn a_close_that_expects_no_left_hash_fails_on_one() {
    if !cfg!(debug_assertions) {
        return;
    }
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let dir = tempfile::tempdir().expect("a directory");
        let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
        db.engine().check_closes_with_no_left_hashes(v);
        deleted_under_a_provable_count_tree(&db, v);
        db.flush().expect("a flush");
        let closed = catch_unwind(AssertUnwindSafe(move || drop(db)));
        match number <= 3 {
            true => {
                let failure = panic_text(closed.expect_err("the close fails"));
                let rule = "a value hash is the hash of its value";
                assert!(failure.contains(rule), "grove version {number}: {failure}");
            }
            false => closed.expect("the close passes"),
        }
    }
}
