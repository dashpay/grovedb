//! States that GroveDB writes on a grove version that is frozen for the
//! chain built with it, and that leave a stored hash other than the one
//! the rules of today make (VER-07-F1).

use grovedb_merk::{
    element::costs::ElementCostExtensions,
    tree::{combine_hash, value_hash},
    Merk, TreeType,
};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignindb::{Db, Options};

use super::{
    append_only::{verified, Answer, History},
    Lignin,
};
use crate::{engine::Engine, operations::delete::DeleteOptions, Element, GroveDb};
use grovedb_merk::store::lignin::hash::MerkHashRules;

const ROOT: &[&[u8]] = &[];
const TOP: &[u8] = b"top";
const PARENT: &[u8] = b"parent";

fn allowed() -> Option<DeleteOptions> {
    Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    })
}

/// The five calls of GroveDB #686 under a tree `top`, and what the grove's
/// own check says of it then.
fn deleted_under_a_provable_count_tree<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
) -> Vec<Answer> {
    let mut h = History::new(db, v);
    let at_parent: &[&[u8]] = &[TOP, PARENT];
    let at_child: &[&[u8]] = &[TOP, PARENT, b"child"];
    let tree = Element::empty_tree;
    h.write("made", db.insert(ROOT, TOP, tree(), None, None, v));
    let parent = Element::empty_provable_count_tree();
    h.write("made", db.insert(&[TOP], PARENT, parent, None, None, v));
    let sibling = Element::new_item(b"s".to_vec());
    h.write(
        "item",
        db.insert(at_parent, b"sibling", sibling, None, None, v),
    );
    h.write(
        "made",
        db.insert(at_parent, b"child", tree(), None, None, v),
    );
    let leaf = Element::new_item(b"value".to_vec());
    h.write("item", db.insert(at_child, b"leaf", leaf, None, None, v));
    h.write(
        "delete tree",
        db.delete(at_parent, b"child", allowed(), None, v),
    );
    h.element("element", db.get(&[TOP], PARENT, None, v));
    let issues = db.verify_grovedb(None, true, false, v).map(|issues| {
        let mut issues: Vec<_> = issues
            .into_iter()
            .map(|(path, (root, expected, stored))| {
                let path: Vec<String> = path.iter().map(hex::encode).collect();
                let hashes = [root, expected, stored].map(hex::encode);
                (path.join("/"), hashes)
            })
            .collect();
        issues.sort();
        issues
    });
    h.plain("issues", issues);
    verified(&mut h);
    h.answers
}

/// What the value hash of `parent` would be with the hash of the root of
/// its Merk made as that of a tree of `as_type`.
fn value_hash_with_root_as<E: Engine>(
    db: &GroveDb<E>,
    v: &GroveVersion,
    as_type: TreeType,
) -> [u8; 32] {
    let element = db.get(&[TOP], PARENT, None, v).unwrap().expect("parent");
    let bytes = element.serialize(v).expect("bytes");
    let Element::ProvableCountTree(root_key, ..) = element else {
        panic!("another element: {element:?}");
    };
    let engine = db.engine();
    let transaction = engine.start_read_transaction();
    let at_parent: &[&[u8]] = &[TOP, PARENT];
    let handle = engine
        .subtree(at_parent.into(), None, &transaction)
        .unwrap();
    let merk = Merk::open_layered_with_root_key(
        handle,
        root_key,
        as_type,
        Some(&Element::value_defined_cost_for_serialized_value),
        v,
    )
    .unwrap()
    .expect("the Merk of parent");
    let root = merk.root_hash().unwrap();
    combine_hash(&value_hash(&bytes).unwrap(), &root).unwrap()
}

/// A probe: what each engine holds after the delete, on every grove
/// version.
#[test]
fn probe_the_delete_under_a_provable_count_tree() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let dir = tempfile::tempdir().expect("a directory");
        let (on_lignin, hashes) = {
            let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
            db.engine().database().unchecked_at_close();
            let answers = deleted_under_a_provable_count_tree(&db, v);
            let hashes = [TreeType::NormalTree, TreeType::ProvableCountTree]
                .map(|as_type| hex::encode(value_hash_with_root_as(&db, v, as_type)));
            db.flush().expect("a flush");
            (answers, hashes)
        };
        let db = Db::open(dir.path(), &Options::default()).expect("the database opens again");
        let mut rules = MerkHashRules::for_check(v);
        let report = db.close_and_verify(Some(&mut rules)).expect("a close");
        eprintln!("PROBE v{number} lignin sound={}", report.is_sound());
        for finding in &report.findings {
            eprintln!("PROBE v{number} lignin finding: {finding}");
        }
        eprintln!(
            "PROBE v{number} value hash with the root as a plain tree's: {}",
            hashes[0]
        );
        eprintln!(
            "PROBE v{number} value hash with the root as a provable count tree's: {}",
            hashes[1]
        );
        for (call, _, answer) in &on_lignin {
            if call == "issues" || call == "verified" || call == "delete tree" {
                eprintln!("PROBE v{number} lignin {call}: {answer}");
            }
        }
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                .expect("a database on RocksDB");
            let on_rocksdb = deleted_under_a_provable_count_tree(&db, v);
            let hashes = [TreeType::NormalTree, TreeType::ProvableCountTree]
                .map(|as_type| hex::encode(value_hash_with_root_as(&db, v, as_type)));
            eprintln!(
                "PROBE v{number} rocksdb value hash, plain root: {}",
                hashes[0]
            );
            eprintln!(
                "PROBE v{number} rocksdb value hash, provable root: {}",
                hashes[1]
            );
            for (call, _, answer) in &on_rocksdb {
                if call == "issues" || call == "verified" {
                    eprintln!("PROBE v{number} rocksdb {call}: {answer}");
                }
            }
            let same = on_lignin == on_rocksdb;
            eprintln!(
                "PROBE v{number} answers the same on both engines: {same} ({} calls)",
                on_lignin.len()
            );
            for (step, (lignin, rocksdb)) in on_lignin.iter().zip(&on_rocksdb).enumerate() {
                if lignin != rocksdb {
                    eprintln!("PROBE v{number} call {step} differs:\n  lignin  {lignin:?}\n  rocksdb {rocksdb:?}");
                }
            }
        }
    }
}

use std::panic::{catch_unwind, AssertUnwindSafe};

use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

use super::nested::{made_by, KINDS};

/// The value hash that `top` stores for `parent`, and what it would be
/// with the root of the Merk of `parent` hashed as a plain tree's and as
/// a tree's of the type of `parent`.
fn hashes_of_parent<E: Engine>(db: &GroveDb<E>, v: &GroveVersion) -> [[u8; 32]; 3] {
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
        let at_parent: &[&[u8]] = &[TOP, PARENT];
        let handle = engine
            .subtree(at_parent.into(), None, &transaction)
            .unwrap();
        let merk = Merk::open_layered_with_root_key(handle, root_key.clone(), as_type, cost, v)
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

/// One case of the family: a tree of the kind `p` with elements, a tree
/// of the kind `c` in it (an MMR tree for `c` past the kinds) that holds
/// one, and the delete of that.
fn family_case<E: Engine>(db: &GroveDb<E>, v: &GroveVersion, p: usize, c: usize) -> String {
    let at_parent: &[&[u8]] = &[TOP, PARENT];
    let at_child: &[&[u8]] = &[TOP, PARENT, b"child"];
    let ok = "a write of the setup";
    db.insert(ROOT, TOP, Element::empty_tree(), None, None, v)
        .unwrap()
        .expect(ok);
    db.insert(&[TOP], PARENT, (KINDS[p].make)(), None, None, v)
        .unwrap()
        .expect(ok);
    let sibling = Element::new_item(b"s".to_vec());
    db.insert(at_parent, b"sibling", sibling, None, None, v)
        .unwrap()
        .expect(ok);
    if KINDS[p].sums {
        let sum = Element::new_sum_item(5);
        db.insert(at_parent, b"sum", sum, None, None, v)
            .unwrap()
            .expect(ok);
    }
    match KINDS.get(c) {
        Some(kind) => {
            db.insert(at_parent, b"child", (kind.make)(), None, None, v)
                .unwrap()
                .expect(ok);
            let leaf = Element::new_item(b"value".to_vec());
            db.insert(at_child, b"leaf", leaf, None, None, v)
                .unwrap()
                .expect(ok);
        }
        None => {
            let mmr = Element::empty_mmr_tree();
            db.insert(at_parent, b"child", mmr, None, None, v)
                .unwrap()
                .expect(ok);
            db.mmr_tree_append(at_parent, b"child", b"value".to_vec(), None, v)
                .unwrap()
                .expect(ok);
        }
    }
    let deleted = catch_unwind(AssertUnwindSafe(|| {
        db.delete(at_parent, b"child", allowed(), None, v).unwrap()
    }));
    let deleted = match deleted {
        Err(_) => return "panic".to_string(),
        Ok(Err(error)) => return format!("refused: {error}"),
        Ok(Ok(())) => "deleted",
    };
    let [stored, plain, own] = hashes_of_parent(db, v);
    let class = match (stored == own, stored == plain) {
        (true, true) => "right (plain is right)",
        (true, false) => "right",
        (false, true) => "LEGACY",
        (false, false) => "OTHER",
    };
    let issues = db
        .verify_grovedb(None, true, false, v)
        .expect("a check")
        .len();
    format!("{deleted} stored={class} issues={issues}")
}

/// A probe: the family of GroveDB #686 on every grove version.
#[test]
fn probe_the_family() {
    for (number, v) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let made = made_by(v);
        for p in 0..made {
            for c in 0..=made {
                let child = KINDS
                    .get(c)
                    .filter(|_| c < made)
                    .map_or("mmr tree", |kind| kind.name);
                let c = if c < made { c } else { KINDS.len() };
                let dir = tempfile::tempdir().expect("a directory");
                let on_lignin = {
                    let db = Lignin::open_with_engine(dir.path()).expect("a database is made");
                    db.engine().database().unchecked_at_close();
                    let said = family_case(&db, v, p, c);
                    if said != "panic" {
                        db.flush().expect("a flush");
                    }
                    said
                };
                let findings = if on_lignin == "panic" {
                    "not checked".to_string()
                } else {
                    let db = Db::open(dir.path(), &Options::default()).expect("opens again");
                    let mut rules = MerkHashRules::for_check(v);
                    let report = db.close_and_verify(Some(&mut rules)).expect("a close");
                    let texts: Vec<String> = report
                        .findings
                        .iter()
                        .map(|finding| finding.to_string())
                        .collect();
                    format!("{} {texts:?}", report.findings.len())
                };
                #[cfg(feature = "rocksdb_storage")]
                let on_rocksdb = {
                    let dir = tempfile::tempdir().expect("a directory");
                    let db = GroveDb::<crate::RocksDbEngine>::open_with_engine(dir.path())
                        .expect("a database on RocksDB");
                    family_case(&db, v, p, c)
                };
                #[cfg(not(feature = "rocksdb_storage"))]
                let on_rocksdb = String::new();
                eprintln!(
                    "FAMILY v{number} | {} | {child} | lignin: {on_lignin} | findings: {findings} | \
                     rocksdb: {on_rocksdb} | same={}",
                    KINDS[p].name,
                    on_lignin == on_rocksdb
                );
            }
        }
    }
}
