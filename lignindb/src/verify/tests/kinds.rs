//! The data column of a subtree, held to the tree type of its entry
//! (`verify::DataColumn`): stores that pass, and each way one does not.

use std::sync::atomic::AtomicU64;

use lignin_format::value_log::ValuePointer;
use lignin_tree::{
    directory::{Host, Prefix},
    drop::Column as FlatColumn,
    flat::FlatTree,
};

use super::{
    key, plant, prefix,
    sound::{parts, Guard},
    write_back, Planted, BELOW, BIG, INNER, NEVER, OUTER,
};
use crate::{
    txn::{
        tests::store::{Rules, TREE_TYPE},
        FlatKinds,
    },
    verify::{self, DataColumn, Finding, Grove, Options, Place, Report, Rule, Scope, Store},
};

/// The tree type that keeps flat records in these tests.
const FLAT_TYPE: &[u8] = b"flat";

fn kinds() -> FlatKinds {
    |tree_type| tree_type == FLAT_TYPE
}

/// The bytes of a node that no node links, as the engine stores one
/// (`txn::unlinked`): a node with no link and no feature, whose value is
/// `value`.
fn unlinked(value: &[u8]) -> Vec<u8> {
    [&[1, 0, 0][..], &[7; 32], &[8; 32], value].concat()
}

/// A store with one of everything, whose checks are given the tree types
/// that keep flat records. Its data column holds no record yet.
fn planted() -> Planted {
    let store = plant();
    store.db.check_kinds_by(kinds());
    store
}

impl Planted {
    /// Puts the record `value` under `key` of the subtree `name` into the
    /// data column, by hand.
    fn put_data(&self, name: u8, key: &[u8], value: &[u8]) {
        self.write_flat(|pages| {
            let key = [&prefix(name)[..], key].concat();
            (self.db.keyspaces().data.put(pages, &key, value)).expect("a put");
        });
    }

    /// States the entry of `name` as one of the tree type that keeps flat
    /// records, by hand.
    fn keeps_flat_records(&self, name: u8) {
        self.write_flat(|pages| {
            let directory = self.directory(pages);
            (directory.set_tree_type(pages, &prefix(name), FLAT_TYPE))
                .expect("the tree type is set");
        });
    }

    /// Enters the subtree `name` with no node, as one of `tree_type` that
    /// the node `key` of [`OUTER`] hosts in the place `position`, by hand.
    fn enter_below_outer(&self, name: u8, tree_type: &[u8], key: &[u8], position: u8) {
        self.write_flat(|pages| {
            let host = Host {
                parent: &prefix(OUTER),
                key,
                position,
            };
            let directory = self.directory(pages);
            (directory.add(pages, &prefix(name), tree_type, Some(host))).expect("it is entered");
        });
    }

    /// The key of the root of the Merk `name`.
    fn root_key(&self, name: u8) -> Vec<u8> {
        self.read(self.root(name), |cell| cell.key().to_vec())
    }
}

fn of(report: &Report, rule: Rule) -> Vec<&Finding> {
    (report.findings.iter())
        .filter(|finding| finding.rule == rule)
        .collect()
}

/// Nodes that no node links under a Merk, and records of any bytes beside
/// the nodes of a tree that keeps flat records, are what the tree types
/// allow. The same records under the other tree type are not, which the
/// same store shows when its check is given other tree types.
#[test]
fn a_store_whose_data_columns_follow_their_tree_types_is_sound() {
    let store = planted();
    // Under a Merk: nodes that no node links, under keys of no node of
    // that Merk. One has its value out of line, and one the key of a node
    // of another Merk.
    store.put_data(BIG, b"beside", &unlinked(b"a value"));
    store.put_data(BIG, b"beside too", &unlinked(&[9; 3000]));
    store.put_data(BIG, &store.root_key(INNER), &unlinked(b"a value"));
    // Under a tree that keeps flat records and has the nodes of a Merk:
    // records of any bytes, the bytes of a node among them, under keys of
    // no node of that Merk.
    store.keeps_flat_records(INNER);
    store.put_data(INNER, b"a record", b"any bytes");
    store.put_data(INNER, b"long", &[0x5a; 3000]);
    store.put_data(INNER, b"as a node", &unlinked(b"the bytes of a node"));
    store.put_data(INNER, &key(0), b"under a key of the big Merk");
    // Under an entry that is not in use, nothing is asked.
    store.put_data(BELOW, b"left", b"any bytes");
    let report = store.check();
    assert!(report.is_sound(), "{report}");
    assert_eq!(report.census.left_over_entries, 1, "{report}");

    // With no tree type that keeps flat records, the records under the
    // tree are held to the form of a node: three of the four have it not.
    store.db.check_kinds_by(|_| false);
    let report = store.check();
    assert_eq!(report.rules(), [Rule::RecordKind], "{report}");
    assert_eq!(report.findings.len(), 3, "{report}");
    // With every tree type keeping flat records, nothing has a form to
    // keep, and no key is held twice.
    store.db.check_kinds_by(|_| true);
    let report = store.check();
    assert!(report.is_sound(), "{report}");
    store.db.check_kinds_by(kinds());
}

/// A store whose check is given no tree types is not held to them: the
/// same records are findings once they are given.
#[test]
fn with_no_tree_types_given_the_data_column_is_not_held_to_them() {
    let store = plant();
    store.db.unchecked();
    store.put_data(BIG, b"a flat record", b"any bytes");
    store.put_data(BIG, &key(0), &unlinked(b"a value"));
    store.enter_below_outer(NEVER, FLAT_TYPE, b"left", 1);
    let report = store.check();
    assert!(report.is_sound(), "{report}");
    store.db.check_kinds_by(kinds());
    let report = store.check();
    assert_eq!(
        report.rules(),
        [Rule::RecordKind, Rule::FlatTreeHost, Rule::KeyHeldTwice],
        "{report}"
    );
    assert_eq!(report.findings.len(), 3, "{report}");
}

/// One way a data column is not what the tree type of its entry allows:
/// what it is, the change that makes it, and the rule whose finding it
/// gives, alone.
struct Damage {
    name: &'static str,
    rule: Rule,
    inject: fn(&Planted),
}

const DAMAGE: &[Damage] = &[
    Damage {
        name: "a flat record that is no node, under a Merk",
        rule: Rule::RecordKind,
        inject: |store| store.put_data(BIG, b"a flat record", b"any bytes"),
    },
    Damage {
        name: "a flat record out of line that is no node, under a Merk",
        rule: Rule::RecordKind,
        inject: |store| store.put_data(BIG, b"a flat record", &[0x5a; 3000]),
    },
    Damage {
        name: "the bytes of a node cut short, under a Merk",
        rule: Rule::RecordKind,
        inject: |store| store.put_data(BIG, b"cut", &unlinked(b"")[..40]),
    },
    Damage {
        name: "a tree that keeps flat records stated as a Merk",
        rule: Rule::RecordKind,
        inject: |store| {
            store.keeps_flat_records(INNER);
            store.put_data(INNER, b"a record", b"any bytes");
            assert!(store.check().is_sound(), "the records of the tree");
            store.write_flat(|pages| {
                let directory = store.directory(pages);
                (directory.set_tree_type(pages, &prefix(INNER), TREE_TYPE))
                    .expect("the tree type is set");
            });
        },
    },
    Damage {
        name: "a node that no node links under the key of a node of its Merk",
        rule: Rule::KeyHeldTwice,
        inject: |store| store.put_data(BIG, &key(7), &unlinked(b"a value")),
    },
    Damage {
        name: "a flat record under the key of a node of its tree's Merk",
        rule: Rule::KeyHeldTwice,
        inject: |store| {
            store.keeps_flat_records(INNER);
            store.put_data(INNER, &store.root_key(INNER), b"any bytes");
        },
    },
    Damage {
        name: "a tree that keeps flat records hosted in the second place",
        rule: Rule::FlatTreeHost,
        inject: |store| store.enter_below_outer(NEVER, FLAT_TYPE, b"left", 1),
    },
    Damage {
        name: "a second Merk hosted by the host of a tree that keeps flat records",
        rule: Rule::FlatTreeHost,
        inject: |store| {
            store.keeps_flat_records(INNER);
            store.enter_below_outer(NEVER, TREE_TYPE, b"host", 1);
        },
    },
];

#[test]
fn every_record_that_its_tree_type_does_not_allow_is_found() {
    let mut missed = Vec::new();
    for damage in DAMAGE {
        let store = planted();
        assert!(store.check().is_sound(), "before {}", damage.name);
        store.db.unchecked();
        (damage.inject)(&store);
        let report = store.check();
        println!("{:<72} {:?}", damage.name, report.rules());
        if report.rules() != [damage.rule] || report.findings.len() != 1 {
            missed.push(format!(
                "{}: one finding of {:?} is expected\n{report}",
                damage.name, damage.rule
            ));
        }
    }
    assert!(missed.is_empty(), "{}", missed.join("\n"));
    assert_eq!(DAMAGE.len(), 8, "the list in the report names each");
}

/// A key that is held twice is reported at the cell of the node, with the
/// place of the record, and a record of the wrong form at the record.
#[test]
fn a_finding_of_the_data_column_names_the_cell_and_the_record() {
    let store = planted();
    store.db.unchecked();
    store.keeps_flat_records(INNER);
    store.put_data(INNER, &store.root_key(INNER), b"any bytes");
    store.put_data(BIG, b"a flat record", b"any bytes");
    let report = store.check();
    let twice = of(&report, Rule::KeyHeldTwice);
    assert_eq!(twice.len(), 1, "{report}");
    assert_eq!(twice[0].place, Place::Cell(store.root(INNER)), "{report}");
    assert!(twice[0].detail.contains("of flat page"), "{report}");
    let wrong = of(&report, Rule::RecordKind);
    assert_eq!(wrong.len(), 1, "{report}");
    assert!(
        matches!(wrong[0].place, Place::FlatEntry { .. }),
        "{report}"
    );
    let named = format!("{}", Place::Merk(prefix(BIG)));
    assert!(wrong[0].detail.contains(&named), "{report}");
    assert_eq!(report.findings.len(), 2, "{report}");
}

/// A record whose value is not read has the finding of its value, and
/// is asked no form: what its bytes are is not known.
#[test]
fn a_record_whose_value_is_not_read_is_asked_no_form() {
    let store = planted();
    store.db.unchecked();
    // A node that no node links, whose value write-back places in the
    // value log.
    store.put_data(BIG, b"beside", &unlinked(&[9; 3000]));
    write_back(&store.db, &store.versions);
    assert!(store.check().is_sound());
    let key = [&prefix(BIG)[..], b"beside"].concat();
    let leaf = store.leaf_with(|at, value| at == key && !value.is_inline());
    store.rebuild_leaf(leaf, |entries| {
        let place = entries.iter_mut().find(|entry| entry.0 == key);
        let place = place.expect("the record");
        let pointer = ValuePointer::decode(place.1[..].try_into().expect("16 bytes"));
        let pointer = pointer.expect("a pointer");
        let moved = ValuePointer::new(pointer.file(), pointer.offset() + 3, pointer.value_len());
        place.1 = moved.expect("a pointer").encode().to_vec();
    });
    let report = store.check();
    assert!(!report.has(Rule::RecordKind), "{report}");
    // The value is read by the census of its page and again where the
    // record is held to its form: that it is not read is said once.
    assert_eq!(of(&report, Rule::ValueUnreadable).len(), 1, "{report}");
}

/// Where the lead of one column is the beginning of another's, a key can
/// read as a record of both, each under a prefix that is entered. It is
/// held as a record of the data column, whichever of the two that is.
#[test]
fn a_key_that_reads_as_a_record_of_two_columns_is_held_as_the_data_columns() {
    let store = plant();
    // The store's own check knows no keyspace but its own.
    store.db.unchecked();
    // Under the lead `a` the key below is of this prefix, and under the
    // lead `ab` of the big Merk's.
    let other: Prefix = {
        let mut other = prefix(BIG);
        other[0] = b'b';
        other
    };
    let mut made = None;
    store.write_flat(|pages| {
        let directory = store.directory(pages);
        (directory.add(pages, &other, TREE_TYPE, None)).expect("it is entered");
        let tree = FlatTree::create(pages, store.db.config()).expect("a keyspace");
        let key = [&b"ab"[..], &prefix(BIG)[..], b"k"].concat();
        assert_eq!(
            key,
            [&b"a"[..], &other[..], &prefix(BIG)[31..], b"k"].concat()
        );
        tree.put(pages, &key, b"any bytes").expect("a put");
        made = Some(tree);
    });
    let shared = made.expect("the keyspace was made");
    let mut columns = store.db.keyspaces().columns().to_vec();
    let first = columns.len();
    for lead in [&b"a"[..], b"ab"] {
        columns.push(FlatColumn { tree: shared, lead });
    }
    let check = |column: Option<usize>| {
        let calls = AtomicU64::new(0);
        let data = column.map(|column| DataColumn {
            column,
            keeps_flat_records: kinds(),
        });
        let parts = Store {
            grove: Grove::Directory {
                tree: store.db.directory(),
                columns: &columns,
                data,
            },
            ..parts(&store.db, &columns)
        };
        let options = Options::new(store.latest(), Scope::Still);
        verify::check(&parts, &Guard, options, Some(&mut Rules(&calls)))
    };
    assert!(check(None).is_sound());
    for column in [first, first + 1] {
        let report = check(Some(column));
        assert_eq!(report.rules(), [Rule::RecordKind], "{report}");
        assert_eq!(report.findings.len(), 1, "{report}");
    }
}

/// Where several columns share one keyspace, the data column is the one
/// that the store names, and the records under the other leads are held
/// to nothing.
#[test]
fn the_data_column_is_the_one_the_store_names_in_a_shared_keyspace() {
    let store = plant();
    // The store's own check knows no keyspace but its own.
    store.db.unchecked();
    let mut made = None;
    store.write_flat(|pages| {
        let tree = FlatTree::create(pages, store.db.config()).expect("a keyspace");
        for lead in [&b"a"[..], b"m"] {
            let key = [lead, &prefix(BIG)[..], b"record"].concat();
            tree.put(pages, &key, b"any bytes").expect("a put");
        }
        made = Some(tree);
    });
    let shared = made.expect("the keyspace was made");
    let mut columns = store.db.keyspaces().columns().to_vec();
    let first = columns.len();
    for lead in [&b"a"[..], b"m"] {
        columns.push(FlatColumn { tree: shared, lead });
    }
    let check = |column: Option<usize>| {
        let calls = AtomicU64::new(0);
        let data = column.map(|column| DataColumn {
            column,
            keeps_flat_records: kinds(),
        });
        let parts = Store {
            grove: Grove::Directory {
                tree: store.db.directory(),
                columns: &columns,
                data,
            },
            ..parts(&store.db, &columns)
        };
        let options = Options::new(store.latest(), Scope::Still);
        verify::check(&parts, &Guard, options, Some(&mut Rules(&calls)))
    };
    assert!(check(None).is_sound());
    // The store's own data column holds no record.
    assert!(check(Some(0)).is_sound());
    let places: Vec<Place> = [first, first + 1]
        .into_iter()
        .map(|column| {
            let report = check(Some(column));
            assert_eq!(report.rules(), [Rule::RecordKind], "{report}");
            assert_eq!(report.findings.len(), 1, "{report}");
            report.findings[0].place
        })
        .collect();
    assert_ne!(places[0], places[1], "each column has its own record");
}
