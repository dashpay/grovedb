//! Hand-written cases for what older grove versions do and the workload
//! generators steer around.
//!
//! The generators leave three behaviours out of their streams so that every
//! stream applies and every query proves on every grove version (see
//! [`Features::deletes_tree_directly`](crate::workload::Features::deletes_tree_directly)
//! and [`Features::proves_queries_in`](crate::workload::Features::proves_queries_in)).
//! A node on an old version may have done exactly these things, so the
//! corpus records them here, as the engine returns them, on every grove
//! version:
//!
//! 1. **`GroveDb::delete` of a populated tree** when the tree or its parent
//!    commits an aggregate into its node hashes. Grove versions 1 to 3
//!    reopen the parent as a tree of the child's type. Under a provable
//!    parent that commits a link hash no proof verifies against, until the
//!    next write to the parent recomputes it ([`cases`], the `delete-…`
//!    ones). Where the parent's nodes are then hashed as another provable
//!    type, the engine panics ([`excluded`]). Where the child's type hashes
//!    what the parent's does, every pair of types is a case.
//! 2. **A query hosted by a `ProvableCountProvableSumTree` or an indexed
//!    tree**, which grove versions 1 and 2 refuse to prove (the
//!    `query-in-…` cases).
//! 3. **A reference stored in a `ProvableCountSumTree`**, whose proof does
//!    not verify on grove versions 1 and 2
//!    (`reference-in-provable-count-sum-tree`).
//!
//! Every case is recorded with every step in full.

use grovedb::{element::reference_path::ReferencePathType, Element};

use crate::workload::{AppendEntry, BatchOp, Op, Query, QueryItem, Read, Write};

/// A hand-written case: the operations applied to an empty database.
#[derive(Clone, Debug)]
pub struct Case {
    /// The name of the case, which names its trace.
    pub name: String,
    /// What the case shows.
    pub about: String,
    /// The operations.
    pub ops: Vec<Op>,
}

const TOP: &[u8] = b"top";
const PARENT: &[u8] = b"parent";
const CHILD: &[u8] = b"child";
const SIBLING: &[u8] = b"sibling";
const HOST: &[u8] = b"host";
const TARGET: &[u8] = b"target";

fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
    segments.iter().map(|segment| segment.to_vec()).collect()
}

fn insert(at: &[&[u8]], key: &[u8], element: Element) -> Op {
    Op::Write(Write::Insert {
        path: path(at),
        key: key.to_vec(),
        element,
    })
}

fn insert_in_batch(at: &[&[u8]], key: &[u8], element: Element) -> Op {
    Op::Batch(vec![BatchOp::InsertOrReplace {
        path: path(at),
        key: key.to_vec(),
        element,
    }])
}

fn get(at: &[&[u8]], key: &[u8]) -> Op {
    Op::Read(Read::Get {
        path: path(at),
        key: key.to_vec(),
    })
}

fn query(at: &[&[u8]], item: QueryItem, raw: bool) -> Op {
    Op::Read(Read::Query(Query {
        path: path(at),
        items: vec![item],
        subquery_path: None,
        subquery_all: false,
        limit: None,
        left_to_right: true,
        raw,
    }))
}

fn key(key: &[u8]) -> QueryItem {
    QueryItem::Key(key.to_vec())
}

fn item(value: &[u8]) -> Element {
    Element::new_item(value.to_vec())
}

/// A tree a case deletes or hosts a query in.
#[derive(Clone, Debug)]
struct Kind {
    /// The name of the tree type, as it appears in a case name.
    name: &'static str,
    /// The empty tree.
    element: Element,
    /// An element the tree accepts.
    filler: Element,
    /// How the tree is filled.
    fill: Fill,
    /// What the tree's node hashes commit to.
    hashes: Hashes,
}

/// What the node hashes of a tree commit to besides keys and values. A
/// node of one kind cannot be hashed as another, except that any node can
/// be hashed as a plain one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hashes {
    /// Nothing more.
    Plain,
    /// The count of a tree that keeps only a count.
    Count,
    /// The count of a tree that keeps a count and a sum.
    CountBesideSum,
    /// The sum.
    Sum,
    /// The count and the sum.
    CountAndSum,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    /// `GroveDb::insert`.
    Insert,
    /// A batch: the one way GroveDB writes to an indexed tree.
    Batch,
    /// An append to an `MmrTree`.
    Mmr,
}

impl Kind {
    fn new(name: &'static str, element: Element, filler: Element) -> Self {
        Kind {
            name,
            element,
            filler,
            fill: Fill::Insert,
            hashes: Hashes::Plain,
        }
    }

    fn filled_by(self, fill: Fill) -> Self {
        Kind { fill, ..self }
    }

    fn hashing(self, hashes: Hashes) -> Self {
        Kind { hashes, ..self }
    }

    /// The operation storing the tree's filler in the tree at `at`.
    fn store(&self, at: &[&[u8]], key: &[u8]) -> Op {
        match self.fill {
            Fill::Insert => insert(at, key, self.filler.clone()),
            Fill::Batch => insert_in_batch(at, key, self.filler.clone()),
            Fill::Mmr => Op::Write(Write::Append {
                path: path(at),
                entry: AppendEntry::Mmr(key.to_vec()),
            }),
        }
    }
}

fn tree() -> Kind {
    Kind::new("tree", Element::empty_tree(), item(b"value"))
}

fn sum_tree() -> Kind {
    Kind::new(
        "sum-tree",
        Element::empty_sum_tree(),
        Element::new_sum_item(5),
    )
}

fn count_tree() -> Kind {
    Kind::new("count-tree", Element::empty_count_tree(), item(b"value"))
}

fn mmr_tree() -> Kind {
    Kind::new("mmr-tree", Element::empty_mmr_tree(), item(b"unused")).filled_by(Fill::Mmr)
}

fn provable_count_tree() -> Kind {
    Kind::new(
        "provable-count-tree",
        Element::empty_provable_count_tree(),
        item(b"value"),
    )
    .hashing(Hashes::Count)
}

fn provable_count_sum_tree() -> Kind {
    Kind::new(
        "provable-count-sum-tree",
        Element::empty_provable_count_sum_tree(),
        Element::new_sum_item(5),
    )
    .hashing(Hashes::CountBesideSum)
}

fn provable_sum_tree() -> Kind {
    Kind::new(
        "provable-sum-tree",
        Element::empty_provable_sum_tree(),
        Element::new_sum_item(5),
    )
    .hashing(Hashes::Sum)
}

fn provable_count_provable_sum_tree() -> Kind {
    Kind::new(
        "provable-count-provable-sum-tree",
        Element::empty_provable_count_provable_sum_tree(),
        Element::new_sum_item(5),
    )
    .hashing(Hashes::CountAndSum)
}

fn provable_sum_indexed_tree() -> Kind {
    Kind::new(
        "provable-sum-indexed-tree",
        Element::empty_provable_sum_indexed_tree(),
        Element::new_sum_item(5),
    )
    .filled_by(Fill::Batch)
    .hashing(Hashes::Sum)
}

fn provable_count_indexed_tree() -> Kind {
    Kind::new(
        "provable-count-indexed-tree",
        Element::empty_provable_count_indexed_tree(),
        item(b"value"),
    )
    .filled_by(Fill::Batch)
    .hashing(Hashes::Count)
}

fn provable_count_provable_sum_indexed_tree() -> Kind {
    Kind::new(
        "provable-count-provable-sum-indexed-tree",
        Element::ProvableCountProvableSumIndexedTree(
            None,
            0,
            0,
            vec![(0, None), (1, None), (2, None)],
            None,
        ),
        Element::new_item_with_sum_item(b"value".to_vec(), 5),
    )
    .filled_by(Fill::Batch)
    .hashing(Hashes::CountAndSum)
}

/// The tree types of grove versions 1 to 3 that `GroveDb::delete` can
/// delete a child from: the Merk trees but the indexed ones, which take
/// their writes through batches.
fn parents() -> [Kind; 9] {
    [
        tree(),
        sum_tree(),
        Kind::new(
            "big-sum-tree",
            Element::empty_big_sum_tree(),
            Element::new_sum_item(5),
        ),
        count_tree(),
        Kind::new(
            "count-sum-tree",
            Element::empty_count_sum_tree(),
            Element::new_sum_item(5),
        ),
        provable_count_tree(),
        provable_count_sum_tree(),
        provable_sum_tree(),
        provable_count_provable_sum_tree(),
    ]
}

/// The tree types of grove versions 1 to 3 a case deletes: the parents,
/// the indexed trees and, for the append-only trees, an `MmrTree`.
fn children() -> Vec<Kind> {
    let mut children = parents().to_vec();
    children.extend([
        provable_sum_indexed_tree(),
        provable_count_indexed_tree(),
        provable_count_provable_sum_indexed_tree(),
        mmr_tree(),
    ]);
    children
}

/// Whether grove versions 1 to 3 panic when `GroveDb::delete` removes a
/// populated `child` from a `parent` that keeps another element: they hash
/// the parent's nodes as the child's type, which only works when that type
/// hashes what the parent's does, or nothing more than keys and values.
fn legacy_delete_panics(parent: &Kind, child: &Kind) -> bool {
    child.hashes != Hashes::Plain && child.hashes != parent.hashes
}

/// A populated `child` tree deleted with `GroveDb::delete` from a `parent`
/// tree that then holds a sibling or nothing.
///
/// The parent is read before the delete, after it, and after the next write
/// to it, and the tree holding the parent is read after the delete.
fn delete_ops(parent: &Kind, child: &Kind, with_sibling: bool) -> Vec<Op> {
    let in_parent: &[&[u8]] = &[TOP, PARENT];
    let mut ops = vec![
        insert(&[], TOP, Element::empty_tree()),
        insert(&[TOP], PARENT, parent.element.clone()),
    ];
    if with_sibling {
        ops.push(parent.store(in_parent, SIBLING));
    }
    ops.push(insert(in_parent, CHILD, child.element.clone()));
    ops.push(child.store(&[TOP, PARENT, CHILD], b"leaf"));
    ops.push(Op::Commit);
    ops.push(query(in_parent, QueryItem::RangeFull, true));
    ops.push(Op::Write(Write::Delete {
        path: path(in_parent),
        key: CHILD.to_vec(),
    }));
    ops.push(Op::Commit);
    ops.push(query(in_parent, QueryItem::RangeFull, true));
    ops.push(query(&[TOP], key(PARENT), true));
    if with_sibling {
        ops.push(get(in_parent, SIBLING));
    }
    ops.push(parent.store(in_parent, b"later"));
    ops.push(Op::Commit);
    ops.push(query(in_parent, QueryItem::RangeFull, true));
    ops
}

fn delete_case(parent: Kind, child: Kind, with_sibling: bool, about: &str) -> Case {
    let name = if with_sibling {
        format!("delete-{}-under-{}", child.name, parent.name)
    } else {
        format!("delete-only-{}-under-{}", child.name, parent.name)
    };
    Case {
        name,
        about: about.to_string(),
        ops: delete_ops(&parent, &child, with_sibling),
    }
}

/// Queries hosted by `host`, which holds two elements, and the query of the
/// tree holding `host`.
fn query_case(host: Kind) -> Case {
    let in_host: &[&[u8]] = &[TOP, HOST];
    let create = match host.fill {
        Fill::Batch => insert_in_batch(&[TOP], HOST, host.element.clone()),
        Fill::Insert | Fill::Mmr => insert(&[TOP], HOST, host.element.clone()),
    };
    Case {
        name: format!("query-in-{}", host.name),
        about: format!(
            "Grove versions 1 and 2 refuse to prove a query hosted by a {}: it needs the proof \
             envelope of grove version 3.",
            host.name
        ),
        ops: vec![
            insert(&[], TOP, Element::empty_tree()),
            create,
            host.store(in_host, b"a"),
            host.store(in_host, b"b"),
            Op::Commit,
            query(in_host, QueryItem::RangeFull, true),
            query(in_host, QueryItem::RangeFull, false),
            query(in_host, key(b"a"), true),
            query(&[TOP], key(HOST), true),
            get(in_host, b"a"),
        ],
    }
}

/// A reference stored in a `ProvableCountSumTree`, read back alone and with
/// its sibling, as stored and followed.
fn reference_case() -> Case {
    let host = provable_count_sum_tree();
    let in_host: &[&[u8]] = &[TOP, HOST];
    let reference = Element::new_reference(ReferencePathType::AbsolutePathReference(path(&[
        TOP, TARGET,
    ])));
    Case {
        name: format!("reference-in-{}", host.name),
        about: "On grove versions 1 and 2 the proof of a reference stored in a \
                ProvableCountSumTree does not verify: the verifier computes another hash for \
                the tree than the one its parent commits."
            .to_string(),
        ops: vec![
            insert(&[], TOP, Element::empty_tree()),
            insert(&[TOP], TARGET, item(b"the value referred to")),
            insert(&[TOP], HOST, host.element.clone()),
            host.store(in_host, b"a"),
            insert(in_host, b"r", reference),
            Op::Commit,
            query(in_host, QueryItem::RangeFull, true),
            query(in_host, QueryItem::RangeFull, false),
            query(in_host, key(b"r"), true),
            query(in_host, key(b"r"), false),
            query(in_host, key(b"a"), true),
            get(in_host, b"r"),
        ],
    }
}

/// Every hand-written case, in the order they are documented.
pub fn cases() -> Vec<Case> {
    let wrong_link = "Grove versions 1 to 3 reopen the parent as a tree of the child's type, so \
                      the delete commits a link hash for the parent that leaves its aggregate \
                      out. No proof through the parent verifies until the next write to the \
                      parent recomputes the hash.";
    let mut cases = vec![
        delete_case(provable_count_tree(), tree(), true, wrong_link),
        delete_case(provable_count_sum_tree(), sum_tree(), true, wrong_link),
        delete_case(provable_sum_tree(), count_tree(), true, wrong_link),
        delete_case(
            provable_count_provable_sum_tree(),
            tree(),
            true,
            "As under a provable count tree. On grove versions 1 and 2 no query hosted by the \
             parent can be proved either.",
        ),
        delete_case(
            provable_count_tree(),
            mmr_tree(),
            true,
            "As for a Merk child: grove versions 1 to 3 reopen the parent as an MmrTree, which \
             hashes like a plain tree.",
        ),
    ];
    let same_hashes = "The child's type hashes what the parent's does, so the parent reopened as \
                       the child's type hashes as it should. Grove versions 1 to 3 still charge \
                       for reopening it.";
    cases.extend(
        [
            (provable_count_tree(), provable_count_tree()),
            (provable_count_tree(), provable_count_indexed_tree()),
            (provable_count_sum_tree(), provable_count_sum_tree()),
            (provable_sum_tree(), provable_sum_tree()),
            (provable_sum_tree(), provable_sum_indexed_tree()),
            (
                provable_count_provable_sum_tree(),
                provable_count_provable_sum_tree(),
            ),
            (
                provable_count_provable_sum_tree(),
                provable_count_provable_sum_indexed_tree(),
            ),
        ]
        .map(|(parent, child)| delete_case(parent, child, true, same_hashes)),
    );
    cases.extend([
        delete_case(
            tree(),
            provable_count_tree(),
            false,
            "The parent is left empty, so grove versions 1 to 3 have no node to hash as a \
             provable count tree and do not panic as they do when a sibling stays.",
        ),
        delete_case(
            tree(),
            provable_sum_indexed_tree(),
            false,
            "As for the only provable count tree under a tree, with an indexed child.",
        ),
        delete_case(
            provable_count_tree(),
            tree(),
            false,
            "The parent is left empty, so its link hash is the same whatever type it is \
             reopened as.",
        ),
    ]);
    cases.extend(
        [
            provable_count_provable_sum_tree(),
            provable_sum_indexed_tree(),
            provable_count_indexed_tree(),
            provable_count_provable_sum_indexed_tree(),
        ]
        .map(query_case),
    );
    cases.push(reference_case());
    cases
}

/// The message every excluded case panics with on grove versions 1 to 3.
pub const EXCLUDED_PANIC: &str = "feature_type is inconsistent with its tree_type";

/// The cases the corpus cannot hold: on grove versions 1 to 3 the engine
/// panics on the delete, so there is nothing to record and no chain can
/// have committed it.
///
/// They are `GroveDb::delete` of a populated tree whose type commits an
/// aggregate into its node hashes, from a parent that keeps a sibling and
/// whose type commits something else: the parent's remaining nodes are
/// hashed as the child's type, which they are not. From grove version 4 the
/// same operations are part of the generated streams.
pub fn excluded() -> Vec<Case> {
    let mut cases = Vec::new();
    for child in children() {
        for parent in parents() {
            if legacy_delete_panics(&parent, &child) {
                cases.push(delete_case(
                    parent,
                    child.clone(),
                    true,
                    "Grove versions 1 to 3 panic: the parent's nodes are hashed as the child's \
                     type.",
                ));
            }
        }
    }
    cases
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        panic::{catch_unwind, AssertUnwindSafe},
    };

    use grovedb::TreeType;
    use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;
    use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

    use super::*;
    use crate::{
        golden::{
            corpus::in_parallel,
            record::{record, Detailing, Sample, SegmentSpec, Source, TraceSpec},
        },
        workload::Features,
    };

    fn tree_type(kind: &Kind) -> TreeType {
        kind.element.tree_type().expect("a kind is a tree")
    }

    /// Records the operations on an empty database. `Err` carries the
    /// message of a panic.
    fn run(ops: Vec<Op>, grove_version: &'static GroveVersion) -> Result<String, String> {
        let spec = TraceSpec {
            name: "case".to_string(),
            seed: 0,
            grove_version,
            segments: vec![SegmentSpec {
                workload: "case".to_string(),
                source: Source::Ops(ops),
                checkpoint_every: None,
                sample: Sample::Everything,
            }],
        };
        let scratch = tempfile::tempdir().expect("temp dir");
        catch_unwind(AssertUnwindSafe(|| {
            record(&spec, "engine", Detailing::Sampled, scratch.path())
                .expect("record")
                .render()
        }))
        .map_err(|panic| {
            panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    panic
                        .downcast_ref::<&str>()
                        .map(|message| message.to_string())
                })
                .unwrap_or_default()
        })
    }

    #[test]
    fn cases_have_names_of_their_own_and_end_on_a_read() {
        let mut names = BTreeSet::new();
        for case in cases().into_iter().chain(excluded()) {
            assert!(
                names.insert(case.name.clone()),
                "{} is named twice",
                case.name
            );
            assert!(
                case.name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-'),
                "{} is not a file name",
                case.name
            );
            assert!(!case.about.is_empty());
            assert!(
                matches!(case.ops.last(), Some(Op::Read(_))),
                "{}",
                case.name
            );
            let mut committed = false;
            for op in &case.ops {
                match op {
                    Op::Commit => committed = true,
                    Op::Read(_) => assert!(committed, "{} reads an open block", case.name),
                    _ => committed = false,
                }
            }
        }
        assert_eq!(cases().len(), 20);
        assert_eq!(excluded().len(), 4 * 8 + 3 * 8);
    }

    /// The cases are the ones the generators steer around: on the grove
    /// versions that have the behaviour, `Features` says so.
    #[test]
    fn the_generators_steer_around_every_case() {
        let oldest = Features::for_grove_version(&GROVE_VERSIONS[0]);
        let third = Features::for_grove_version(&GROVE_VERSIONS[2]);
        let fourth = Features::for_grove_version(&GROVE_VERSIONS[3]);
        let mut deletes: Vec<(Kind, Kind)> = vec![
            (provable_count_tree(), tree()),
            (provable_count_sum_tree(), sum_tree()),
            (provable_sum_tree(), count_tree()),
            (provable_count_provable_sum_tree(), tree()),
            (provable_count_tree(), mmr_tree()),
            (tree(), provable_count_tree()),
            (tree(), provable_sum_indexed_tree()),
        ];
        let mut matching = 0;
        for child in children() {
            for parent in parents() {
                if child.hashes != Hashes::Plain && child.hashes == parent.hashes {
                    let name = delete_case(parent.clone(), child.clone(), true, "about").name;
                    assert!(
                        cases().iter().any(|case| case.name == name),
                        "{name} does not panic and is not a case"
                    );
                    deletes.push((parent, child.clone()));
                    matching += 1;
                }
            }
        }
        assert_eq!(matching, 7);
        for child in children() {
            for parent in parents() {
                if legacy_delete_panics(&parent, &child) {
                    deletes.push((parent, child.clone()));
                }
            }
        }
        for (parent, child) in &deletes {
            let (parent, child) = (tree_type(parent), tree_type(child));
            for features in [oldest, third] {
                assert!(!features.deletes_tree_directly(parent, child));
            }
            assert!(fourth.deletes_tree_directly(parent, child));
        }
        for host in [
            provable_count_provable_sum_tree(),
            provable_sum_indexed_tree(),
            provable_count_indexed_tree(),
            provable_count_provable_sum_indexed_tree(),
            provable_count_sum_tree(),
        ] {
            assert!(!oldest.proves_queries_in(tree_type(&host)), "{}", host.name);
            assert!(third.proves_queries_in(tree_type(&host)), "{}", host.name);
        }
    }

    /// Keeps the panics this module expects out of the test output. Any
    /// other panic is reported as before.
    fn silence_expected_panics() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let report = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |panic| {
                let expected = panic
                    .payload()
                    .downcast_ref::<String>()
                    .is_some_and(|message| message.contains(EXCLUDED_PANIC));
                if !expected {
                    report(panic);
                }
            }));
        });
    }

    /// Every pair of parent and child `GroveDb::delete` can meet on grove
    /// versions 1 to 3 either panics, and is in [`excluded`], or records.
    #[test]
    fn the_excluded_cases_are_the_deletes_that_panic() {
        silence_expected_panics();
        let mut runs = Vec::new();
        for grove_version in &GROVE_VERSIONS[..3] {
            for parent in parents() {
                for child in children() {
                    runs.push((grove_version, parent.clone(), child));
                }
            }
        }
        let outcomes = in_parallel(&runs, 8, |(grove_version, parent, child)| {
            let case = delete_case(parent.clone(), child.clone(), true, "about");
            let panics = legacy_delete_panics(parent, child);
            let outcome = match run(case.ops, grove_version) {
                Ok(_) if !panics => return Ok(None),
                Err(message) if panics && message.contains(EXCLUDED_PANIC) => {
                    return Ok(Some(case.name));
                }
                Ok(_) => "records".to_string(),
                Err(panic) => format!("panics: {panic}"),
            };
            Err(format!(
                "{} on grove version {}: {outcome}",
                case.name, grove_version.protocol_version
            ))
        });
        let failures: Vec<&String> = outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
        let panicking: BTreeSet<String> = outcomes.into_iter().flatten().flatten().collect();
        let excluded: BTreeSet<String> = excluded().into_iter().map(|case| case.name).collect();
        assert_eq!(panicking, excluded);
    }

    /// From grove version 4 the excluded cases record: they are deletes the
    /// generated streams hold.
    #[test]
    fn the_excluded_cases_record_from_grove_version_4() {
        let runs: Vec<(&'static GroveVersion, Case)> = GROVE_VERSIONS[3..]
            .iter()
            .flat_map(|grove_version| {
                excluded()
                    .into_iter()
                    .map(move |case| (grove_version, case))
            })
            .collect();
        let failures: Vec<String> = in_parallel(&runs, 8, |(grove_version, case)| {
            let outcome = match run(case.ops.clone(), grove_version) {
                Ok(text) if text.contains("\ntally failed=0 unproved=0 unverified=0\n") => {
                    return None;
                }
                Ok(_) => "an operation fails or a proof does not verify".to_string(),
                Err(panic) => format!("panics: {panic}"),
            };
            Some(format!(
                "{} on grove version {}: {outcome}",
                case.name, grove_version.protocol_version
            ))
        })
        .into_iter()
        .flatten()
        .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// The hand-written cases apply on every grove version, and show the
    /// behaviour they are there for on the versions that have it.
    #[test]
    fn the_cases_show_what_the_older_grove_versions_do() {
        let after_delete = "Write(Delete";
        for case in cases() {
            let mut refused = Vec::new();
            let mut unproved = Vec::new();
            let mut roots_after_delete = Vec::new();
            let versions: Vec<&'static GroveVersion> = GROVE_VERSIONS.iter().collect();
            let texts = in_parallel(&versions, 5, |grove_version| {
                run(case.ops.clone(), grove_version)
            });
            for text in texts {
                let text = text.unwrap_or_else(|panic| panic!("{} panics: {panic}", case.name));
                assert!(
                    !text.contains("\n  failed "),
                    "{}: an operation failed",
                    case.name
                );
                refused.push(text.matches("  verification refused InvalidProof").count());
                unproved.push(text.matches("  prove-failed NotSupported").count());
                let tally = format!(
                    "\ntally failed=0 unproved={} unverified={}\n",
                    unproved.last().expect("just pushed"),
                    refused.last().expect("just pushed")
                );
                assert!(
                    text.contains(&tally),
                    "{}: the tally is not {tally}",
                    case.name
                );
                assert_eq!(
                    text.matches("  verification ").count()
                        + text.matches("  prove-failed ").count(),
                    case.ops
                        .iter()
                        .filter(|op| matches!(op, Op::Read(Read::Query(_))))
                        .count(),
                    "{}: every query is proved or refused",
                    case.name
                );
                assert!(!text.contains("other-root"), "{}", case.name);
                let root = text
                    .split_once(after_delete)
                    .and_then(|(_, rest)| rest.split_once("\n  root "))
                    .map(|(_, rest)| rest[..64].to_string());
                roots_after_delete.push(root);
            }
            let name = case.name.as_str();
            let wrong_link = [
                "delete-tree-under-provable-count-tree",
                "delete-sum-tree-under-provable-count-sum-tree",
                "delete-count-tree-under-provable-sum-tree",
                "delete-mmr-tree-under-provable-count-tree",
            ];
            if wrong_link.contains(&name) {
                assert_eq!(refused, [1, 1, 2, 0, 0], "{name}");
                assert_eq!(unproved, [0; 5], "{name}");
                assert_eq!(roots_after_delete[0], roots_after_delete[2], "{name}");
                assert_ne!(roots_after_delete[2], roots_after_delete[3], "{name}");
                assert_eq!(roots_after_delete[3], roots_after_delete[4], "{name}");
            } else if name == "delete-tree-under-provable-count-provable-sum-tree" {
                assert_eq!(refused, [0, 0, 2, 0, 0], "{name}");
                assert_eq!(unproved, [3, 3, 0, 0, 0], "{name}");
                assert_ne!(roots_after_delete[2], roots_after_delete[3], "{name}");
            } else if name.ends_with("-under-provable-count-provable-sum-tree") {
                assert_eq!(refused, [0; 5], "{name}");
                assert_eq!(unproved, [3, 3, 0, 0, 0], "{name}");
                assert_eq!(roots_after_delete[2], roots_after_delete[3], "{name}");
            } else if name.starts_with("delete-") {
                assert_eq!(refused, [0; 5], "{name}");
                assert_eq!(unproved, [0; 5], "{name}");
            } else if name.starts_with("query-in-") {
                assert_eq!(refused, [0; 5], "{name}");
                assert_eq!(unproved, [3, 3, 0, 0, 0], "{name}");
            } else {
                assert_eq!(name, "reference-in-provable-count-sum-tree");
                assert_eq!(refused, [4, 4, 0, 0, 0], "{name}");
                assert_eq!(unproved, [0; 5], "{name}");
            }
        }
    }
}
