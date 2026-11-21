//! Queries and proofs (GROVE-06): the same reads and the same proofs on
//! LigninDB and on RocksDB, with every answer, every `OperationCost` and
//! the bytes of every proof compared, call for call, on every grove
//! version.
//!
//! One grove holds a group of rows for each kind of tree whose data is one
//! Merk, a tree of items of many sizes with references of five kinds, trees
//! one inside the other, a wide tree that counts its rows, and elements
//! that a read fails at. It is read four times:
//!
//! - with no transaction, each read in a read transaction of its own;
//! - through a snapshot read transaction, after commits that the snapshot
//!   does not show;
//! - through a write transaction that has written and removed rows of its
//!   own and not committed them, before and after a commit beside it;
//! - by the prover, from the committed state, each proof checked by the
//!   verifier against the root hash of that state.
//!
//! What is not read here, and why:
//!
//! - **A descending range that excludes its end, where the end is past the
//!   last key of its subtree** (`QueryItem::Range`, `RangeTo` and
//!   `RangeAfterTo` read right to left). The cursor seeks to the end and
//!   steps back from past the last record, where RocksDB's answer depends
//!   on what follows the subtree in its column family. Every such range
//!   here ends at or below the last key of the subtrees it is read in.
//! - **A walk over the records of a tree that keeps its data flat.**
//! - **An indexed tree**, into which the engine refuses a write.

use std::{collections::BTreeMap, path::Path};

use grovedb_costs::OperationCost;
use grovedb_merk::proofs::query::AggregateSumQuery;
use grovedb_path::SubtreePath;
use grovedb_storage::RawIterator;
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

use super::{
    nested::{bidirectional, check, made_by, number, open, segments, Answer, Log, Recorder, KINDS},
    LigninEngine,
};
use crate::{
    engine::Engine, operations::proof::ProveOptions, query_result_type::QueryResultType,
    reference_path::ReferencePathType, AggregateSumPathQuery, Element, GroveDb,
    PathBranchChunkQuery, PathQuery, PathTrunkChunkQuery, Query, QueryItem, SizedQuery,
};

const GROUPS: &[u8] = b"groups";
const PLAIN: &[u8] = b"plain";
const WIDE: &[u8] = b"wide";
const ODD: &[u8] = b"odd";
const LINKED: &[u8] = b"linked";

/// The query of a reference whose target was written again after it. The
/// prover proves it, and the verifier refuses the proof on every engine.
const STALE: &str = "a stale reference";

/// How many rows a group holds, and how many the wide tree.
const ROWS: u8 = 9;
const WIDE_ROWS: u8 = 70;

fn row(n: u8) -> Vec<u8> {
    format!("row {n:02}").into_bytes()
}

fn group(kind: usize) -> Vec<u8> {
    format!("group {kind:02}").into_bytes()
}

fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
    segments.iter().map(|segment| segment.to_vec()).collect()
}

fn reference(to: ReferencePathType) -> Element {
    Element::new_reference(to)
}

/// Whether `version` makes elements that take backward references.
fn links_both_ways(version: &GroveVersion) -> bool {
    version.protocol_version >= GROVE_VERSIONS[3].protocol_version
}

/// Makes the grove of this module, with no transaction.
fn grove<E: Engine>(recorder: &mut Recorder<'_, '_, '_, E>) {
    let made = made_by(recorder.version);
    for top in [GROUPS, PLAIN, ODD] {
        recorder.insert(&[], top, Element::empty_tree());
    }
    recorder.insert(&[], WIDE, Element::empty_provable_count_tree());

    // A group for each kind of tree: nine numbers, and a reference to one
    // of them.
    for (kind, of_kind) in KINDS.iter().enumerate().take(made) {
        recorder.insert(&path(&[GROUPS]), &group(kind), (of_kind.make)());
        let rows = path(&[GROUPS, &group(kind)]);
        for n in 0..ROWS {
            let value = i64::from(n) * 7 - 20;
            recorder.insert(&rows, &row(n), number(of_kind.sums, value));
        }
        let to_a_row = ReferencePathType::SiblingReference(row(3));
        recorder.insert(&rows, b"to row 03", reference(to_a_row));
        // An item that is also a number, where the tree takes sums.
        let both = match of_kind.sums {
            true => Element::new_item_with_sum_item(b"both".to_vec(), 11),
            false => Element::new_item(b"both".to_vec()),
        };
        recorder.insert(&rows, b"both", both);
    }
    for n in 0..WIDE_ROWS {
        recorder.insert(&path(&[WIDE]), &row(n), Element::new_item(vec![n; 5]));
    }

    // Items of many sizes, one of them far longer than a cell holds in
    // line, with references of each kind that leads out of a plain tree,
    // a tree that nothing is written into, and trees two deep.
    let plain = path(&[PLAIN]);
    for n in 0..14u8 {
        let length = match n {
            9 => 2_500,
            n => usize::from(n) * 37 % 300,
        };
        recorder.insert(&plain, &row(n), Element::new_item(vec![n; length]));
    }
    recorder.insert(&plain, b"empty", Element::empty_tree());
    recorder.insert(&plain, b"nested", Element::empty_tree());
    let nested = path(&[PLAIN, b"nested"]);
    for inner in [b"inner a", b"inner b"] {
        recorder.insert(&nested, inner, Element::empty_tree());
        let rows = path(&[PLAIN, b"nested", inner]);
        for n in 0..4 {
            recorder.insert(&rows, &row(n), Element::new_item(vec![inner[6], n]));
        }
        recorder.insert(&rows, b"shared", Element::new_item(inner.to_vec()));
    }
    let inner_a = path(&[PLAIN, b"nested", b"inner a"]);
    let to_a_cousin = ReferencePathType::CousinReference(b"inner b".to_vec());
    // A cousin has the key of its reference, so `shared` leads to the
    // other tree's `shared`, and a key the other tree has not leads
    // nowhere: that insert is refused.
    recorder.delete(&inner_a, b"shared");
    recorder.insert(&inner_a, b"shared", reference(to_a_cousin.clone()));
    recorder.insert(&inner_a, b"shared by none", reference(to_a_cousin));
    let up_and_over =
        ReferencePathType::UpstreamFromElementHeightReference(1, path(&[b"inner b", &row(2)]));
    recorder.insert(&inner_a, b"up and over", reference(up_and_over));
    let to_a_sibling = ReferencePathType::SiblingReference(row(3));
    recorder.insert(&plain, b"to a sibling", reference(to_a_sibling));
    let to_a_reference = ReferencePathType::SiblingReference(b"to a sibling".to_vec());
    recorder.insert(&plain, b"to a reference", reference(to_a_reference));
    let to_a_group = ReferencePathType::AbsolutePathReference(path(&[GROUPS, &group(1), &row(2)]));
    recorder.insert(&plain, b"to a group", reference(to_a_group));
    let from_the_root =
        ReferencePathType::UpstreamRootHeightReference(0, path(&[GROUPS, &group(0), &row(1)]));
    recorder.insert(&plain, b"to the root's", reference(from_the_root));
    let to_the_wide = ReferencePathType::AbsolutePathReference(path(&[WIDE, &row(5)]));
    recorder.insert(&plain, b"to the wide tree", reference(to_the_wide));

    // What a read fails at, or answers in a way of its own: a reference
    // whose target is gone, a reference to a tree, a reference whose
    // target was written again after it, and a reference that may make
    // one hop where its target needs two.
    let odd = path(&[ODD]);
    recorder.insert(&odd, b"target", Element::new_item(b"gone".to_vec()));
    let to_the_target = ReferencePathType::SiblingReference(b"target".to_vec());
    recorder.insert(&odd, b"broken", reference(to_the_target));
    recorder.delete(&odd, b"target");
    let to_a_tree = ReferencePathType::AbsolutePathReference(path(&[GROUPS, &group(0)]));
    recorder.insert(&odd, b"to a tree", reference(to_a_tree));
    recorder.insert(&odd, b"item", Element::new_item(b"an item".to_vec()));
    recorder.insert(&odd, b"moved", Element::new_item(b"as it was".to_vec()));
    let to_the_moved = ReferencePathType::SiblingReference(b"moved".to_vec());
    recorder.insert(&odd, b"stale", reference(to_the_moved));
    recorder.insert(&odd, b"moved", Element::new_item(b"as it is".to_vec()));
    let one_hop = Element::new_reference_with_hops(
        ReferencePathType::AbsolutePathReference(path(&[PLAIN, b"to a reference"])),
        Some(1),
    );
    recorder.insert(&odd, b"one hop", one_hop);

    if links_both_ways(recorder.version) {
        recorder.insert(&[], LINKED, Element::empty_tree());
        let linked = path(&[LINKED]);
        let referred = Element::new_item_allowing_bidirectional_references(b"referred".to_vec());
        recorder.insert(&linked, b"referred", referred);
        recorder.insert(&linked, b"reference", bidirectional(&linked, b"referred"));
        recorder.insert(&linked, b"second", bidirectional(&linked, b"reference"));
        recorder.insert(&linked, b"plain", Element::new_item(vec![4; 30]));
    }
}

fn sized(at: &[&[u8]], query: Query, limit: Option<u16>, offset: Option<u16>) -> PathQuery {
    PathQuery::new(path(at), SizedQuery::new(query, limit, offset))
}

fn one(item: QueryItem, left_to_right: bool) -> Query {
    Query::new_single_query_item_with_direction(item, left_to_right)
}

/// The query items of this module, over the keys of a group. No range
/// that excludes its end ends past `to row 03`, the last key of a group.
fn items() -> Vec<(&'static str, QueryItem)> {
    vec![
        ("a key", QueryItem::Key(row(3))),
        ("no key", QueryItem::Key(b"row 03a".to_vec())),
        ("a range", QueryItem::Range(row(2)..row(6))),
        ("a closed range", QueryItem::RangeInclusive(row(2)..=row(6))),
        ("all", QueryItem::RangeFull(..)),
        ("from", QueryItem::RangeFrom(row(5)..)),
        ("to", QueryItem::RangeTo(..row(4))),
        ("up to", QueryItem::RangeToInclusive(..=row(4))),
        ("after", QueryItem::RangeAfter(row(5)..)),
        ("after to", QueryItem::RangeAfterTo(row(1)..row(6))),
        (
            "after up to",
            QueryItem::RangeAfterToInclusive(row(1)..=row(6)),
        ),
        (
            "between keys",
            QueryItem::Range(b"row 02a".to_vec()..b"row 05a".to_vec()),
        ),
        ("below the first", QueryItem::RangeTo(..b"a".to_vec())),
        ("above the last", QueryItem::RangeFrom(b"zz".to_vec()..)),
    ]
}

fn direction(left_to_right: bool) -> &'static str {
    match left_to_right {
        true => "forward",
        false => "back",
    }
}

/// The path queries of this module that select rows, each with a name.
fn row_queries(version: &GroveVersion) -> Vec<(String, PathQuery)> {
    let made = made_by(version);
    let mut queries = Vec::new();
    let groups = [group(0), group(1), group(5)];
    let subtrees: [(&str, Vec<&[u8]>); 5] = [
        ("plain", vec![PLAIN]),
        ("a tree", vec![GROUPS, &groups[0]]),
        ("a sum tree", vec![GROUPS, &groups[1]]),
        ("a provable count tree", vec![GROUPS, &groups[2]]),
        ("wide", vec![WIDE]),
    ];

    // Each item by itself, in both directions, whole and with a limit.
    for (subtree, at) in &subtrees {
        for (name, item) in items() {
            for left_to_right in [true, false] {
                for limit in [None, Some(3)] {
                    let limited = limit.map_or("", |_| ", three");
                    queries.push((
                        format!("{subtree}: {name} {}{limited}", direction(left_to_right)),
                        sized(at, one(item.clone(), left_to_right), limit, None),
                    ));
                }
            }
        }
    }

    // Several items in one query, with a limit, and with an offset for
    // the whole query, which no proof takes.
    for (subtree, at) in &subtrees {
        for left_to_right in [true, false] {
            let mut several = Query::new_with_direction(left_to_right);
            several.insert_key(row(1));
            several.insert_range(row(3)..row(5));
            several.insert_range_from(row(7)..);
            several.insert_key(b"no such key".to_vec());
            for (limit, offset) in [(None, None), (Some(4), None), (Some(3), Some(2))] {
                queries.push((
                    format!(
                        "{subtree}: several {} {limit:?} {offset:?}",
                        direction(left_to_right)
                    ),
                    sized(at, several.clone(), limit, offset),
                ));
            }
        }
    }

    // The rows of every group: a subquery under each key of a tree of
    // trees of every kind.
    for left_to_right in [true, false] {
        for limit in [None, Some(10)] {
            let mut rows = one(QueryItem::RangeFull(..), left_to_right);
            rows.set_subquery(one(QueryItem::RangeFull(..), left_to_right));
            queries.push((
                format!("every row {} {limit:?}", direction(left_to_right)),
                sized(&[GROUPS], rows, limit, None),
            ));
        }
        let mut some = Query::new_with_direction(left_to_right);
        some.insert_key(group(0));
        some.insert_key(group(5));
        some.insert_key(b"group none".to_vec());
        some.set_subquery(one(QueryItem::Range(row(2)..row(6)), left_to_right));
        queries.push((
            format!("the rows of two groups {}", direction(left_to_right)),
            sized(&[GROUPS], some, None, None),
        ));
        // A limit of its own for the rows of each group, which no count
        // has to commit, with and without a limit for the whole query.
        let mut two_rows = one(QueryItem::RangeFull(..), left_to_right);
        two_rows.limit = Some(2);
        let mut of_each = one(QueryItem::RangeFull(..), left_to_right);
        of_each.set_subquery(two_rows);
        for limit in [None, Some(7)] {
            queries.push((
                format!(
                    "two rows of each group {} {limit:?}",
                    direction(left_to_right)
                ),
                sized(&[GROUPS], of_each.clone(), limit, None),
            ));
        }
    }

    // A subquery for one key and another for the rest, and the trees
    // themselves beside their rows.
    let mut conditional = Query::new_range_full();
    conditional.set_subquery(one(QueryItem::RangeTo(..row(2)), true));
    conditional.add_conditional_subquery(
        QueryItem::Key(group(1)),
        None,
        Some(Query::new_single_key(row(2))),
    );
    queries.push((
        "a subquery of its own for one group".to_owned(),
        sized(&[GROUPS], conditional.clone(), None, None),
    ));
    conditional.add_parent_tree_on_subquery = true;
    queries.push((
        "the groups beside their rows".to_owned(),
        sized(&[GROUPS], conditional, Some(12), None),
    ));

    // Subqueries along a path, two levels of them from the root, items
    // and trees in one range, and a tree with no rows.
    let mut along = Query::new_single_key(b"nested".to_vec());
    along.set_subquery_path(path(&[b"inner a"]));
    along.set_subquery(Query::new_range_full());
    queries.push((
        "along a path".to_owned(),
        sized(&[PLAIN], along, None, None),
    ));
    let mut to_a_key = Query::new_single_key(b"nested".to_vec());
    to_a_key.set_subquery_path(path(&[b"inner b", b"shared"]));
    queries.push((
        "along a path to a key".to_owned(),
        sized(&[PLAIN], to_a_key, None, None),
    ));
    let mut inner = Query::new_range_full();
    inner.set_subquery(Query::new_range_full());
    let mut middle = Query::new_single_key(b"nested".to_vec());
    middle.set_subquery(inner);
    let mut from_the_root = Query::new_single_key(PLAIN.to_vec());
    from_the_root.set_subquery(middle);
    queries.push((
        "three levels from the root".to_owned(),
        sized(&[], from_the_root, None, None),
    ));
    for limit in [None, Some(5)] {
        let mut mixed = Query::new_range_full();
        mixed.set_subquery(Query::new_range_full());
        queries.push((
            format!("items and trees in one range {limit:?}"),
            sized(&[PLAIN], mixed, limit, None),
        ));
    }
    queries.push((
        "a tree nothing was written into".to_owned(),
        sized(&[PLAIN, b"empty"], Query::new_range_full(), None, None),
    ));
    queries.push((
        "no tree at the path".to_owned(),
        sized(&[PLAIN, b"none"], Query::new_range_full(), None, None),
    ));
    queries.push((
        "an item at the path".to_owned(),
        sized(&[PLAIN, &row(1)], Query::new_range_full(), None, None),
    ));

    // What a read fails at, and the elements that take backward
    // references.
    queries.push((
        "a broken reference".to_owned(),
        sized(&[ODD], Query::new_range_full(), None, None),
    ));
    queries.push((
        "a reference to a tree".to_owned(),
        sized(
            &[ODD],
            Query::new_single_key(b"to a tree".to_vec()),
            None,
            None,
        ),
    ));
    queries.push((
        STALE.to_owned(),
        sized(&[ODD], Query::new_single_key(b"stale".to_vec()), None, None),
    ));
    if links_both_ways(version) {
        queries.push((
            "linked both ways".to_owned(),
            sized(&[LINKED], Query::new_range_full(), None, None),
        ));
    }

    // Pages by count: a page of its own for each group, which a count
    // has to commit, the page of one tree, and an offset for the whole
    // query in a tree that counts.
    for left_to_right in [true, false] {
        let mut page = one(QueryItem::RangeFull(..), left_to_right);
        page.offset = Some(1);
        page.limit = Some(2);
        let mut pages = Query::new_with_direction(left_to_right);
        for kind in [5, 6, 8] {
            pages.insert_key(group(kind));
        }
        pages.set_subquery(page.clone());
        queries.push((
            format!("a page of each counting group {}", direction(left_to_right)),
            sized(&[GROUPS], pages, None, None),
        ));
        let mut of_every = one(QueryItem::RangeFull(..), left_to_right);
        of_every.set_subquery(page.clone());
        queries.push((
            format!("a page of every group {}", direction(left_to_right)),
            sized(&[GROUPS], of_every, None, None),
        ));
        for kind in (0..made).filter(|kind| [0, 5, 6, 8, 9].contains(kind)) {
            queries.push((
                format!("the page of group {kind:02} {}", direction(left_to_right)),
                sized(&[GROUPS, &group(kind)], page.clone(), None, None),
            ));
        }
        let mut far = one(QueryItem::Range(row(10)..row(60)), left_to_right);
        far.offset = Some(25);
        far.limit = Some(4);
        queries.push((
            format!("a page far into the wide tree {}", direction(left_to_right)),
            sized(&[WIDE], far, None, None),
        ));
        let mut beyond = one(QueryItem::RangeFull(..), left_to_right);
        beyond.offset = Some(200);
        beyond.limit = Some(4);
        queries.push((
            format!("a page beyond the wide tree {}", direction(left_to_right)),
            sized(&[WIDE], beyond, None, None),
        ));
        let whole = one(QueryItem::RangeFull(..), left_to_right);
        queries.push((
            format!("an offset in the wide tree {}", direction(left_to_right)),
            sized(&[WIDE], whole.clone(), Some(5), Some(20)),
        ));
        queries.push((
            format!("an offset in a counting group {}", direction(left_to_right)),
            sized(&[GROUPS, &group(5)], whole, Some(2), Some(4)),
        ));
    }
    queries
}

/// Pairs of path queries that are read and proven as one: keys and ranges
/// with no limit, in one subtree and in two, and pairs taken from
/// [`row_queries`] as they come, most of which do not merge.
fn pairs(version: &GroveVersion) -> Vec<(String, PathQuery, PathQuery)> {
    let key = |at: &[&[u8]], n: u8| sized(at, Query::new_single_key(row(n)), None, None);
    let all = |at: &[&[u8]]| sized(at, Query::new_range_full(), None, None);
    let range = one(QueryItem::Range(row(2)..row(6)), true);
    let mut pairs = vec![
        (
            "two keys of one tree".to_owned(),
            key(&[PLAIN], 1),
            key(&[PLAIN], 8),
        ),
        (
            "a range and a group".to_owned(),
            sized(&[PLAIN], range, None, None),
            all(&[GROUPS, &group(0)]),
        ),
        (
            "keys of two trees".to_owned(),
            key(&[GROUPS, &group(1)], 3),
            key(&[WIDE], 10),
        ),
        (
            "two trees side by side".to_owned(),
            all(&[PLAIN, b"nested", b"inner a"]),
            all(&[PLAIN, b"nested", b"inner b"]),
        ),
        // References by their whole paths, which is what a read of
        // encoded values takes.
        (
            "two references".to_owned(),
            sized(
                &[PLAIN],
                Query::new_single_key(b"to a group".to_vec()),
                None,
                None,
            ),
            sized(
                &[PLAIN],
                Query::new_single_key(b"to the wide tree".to_vec()),
                None,
                None,
            ),
        ),
    ];
    let queries = row_queries(version);
    for pair in queries.chunks(41).filter(|pair| pair.len() > 1) {
        pairs.push((
            format!("{} and {}", pair[0].0, pair[1].0),
            pair[0].1.clone(),
            pair[1].1.clone(),
        ));
    }
    pairs
}

/// The path queries that ask for an aggregate over a range, of every
/// group and of the wide tree, and the same for each key of the groups.
fn aggregate_queries(version: &GroveVersion) -> Vec<(String, PathQuery)> {
    let made = made_by(version);
    let ranges = [
        ("a range", QueryItem::Range(row(2)..row(6))),
        ("from", QueryItem::RangeFrom(row(3)..)),
        ("up to", QueryItem::RangeToInclusive(..=row(4))),
    ];
    type Wrap = fn(Box<QueryItem>) -> QueryItem;
    let wraps: [(&str, Wrap); 3] = [
        ("count", QueryItem::AggregateCountOnRange),
        ("sum", QueryItem::AggregateSumOnRange),
        ("count and sum", QueryItem::AggregateCountAndSumOnRange),
    ];
    let mut queries = Vec::new();
    for (aggregate, wrap) in wraps {
        for (name, range) in &ranges {
            let item = || wrap(Box::new(range.clone()));
            for kind in 0..made {
                queries.push((
                    format!("{aggregate} of group {kind:02}, {name}"),
                    sized(
                        &[GROUPS, &group(kind)],
                        Query::new_single_query_item(item()),
                        None,
                        None,
                    ),
                ));
            }
            queries.push((
                format!("{aggregate} of the wide tree, {name}"),
                sized(&[WIDE], Query::new_single_query_item(item()), None, None),
            ));
            // The aggregate of each group that has one, by its key.
            let mut per_key = Query::new();
            for kind in (0..made).filter(|kind| [5, 6, 7, 8, 9, 11].contains(kind)) {
                per_key.insert_key(group(kind));
            }
            per_key.set_subquery(Query::new_single_query_item(item()));
            queries.push((
                format!("{aggregate} of each group, {name}"),
                sized(&[GROUPS], per_key, None, None),
            ));
        }
    }
    queries
}

/// The walks that add sum items up to a limit.
fn sum_walks(version: &GroveVersion) -> Vec<(String, AggregateSumPathQuery)> {
    let mut walks = Vec::new();
    for kind in 0..made_by(version) {
        for left_to_right in [true, false] {
            for (sum_limit, limit_of_items_to_check) in [(30, None), (1_000, Some(4)), (3, None)] {
                walks.push((
                    format!(
                        "group {kind:02} {} {sum_limit} {limit_of_items_to_check:?}",
                        direction(left_to_right)
                    ),
                    AggregateSumPathQuery {
                        path: path(&[GROUPS, &group(kind)]),
                        aggregate_sum_query: AggregateSumQuery {
                            items: vec![QueryItem::RangeFull(..)],
                            left_to_right,
                            sum_limit,
                            limit_of_items_to_check,
                        },
                    },
                ));
            }
        }
    }
    walks
}

/// The elements that are read by their keys: every key of the plain tree
/// and of the two odd ones, a row and the reference of every group, the
/// trees of the root, and what is not there.
fn keys(version: &GroveVersion) -> Vec<(Vec<Vec<u8>>, Vec<u8>)> {
    let mut keys: Vec<(Vec<Vec<u8>>, Vec<u8>)> = Vec::new();
    for n in [0, 3, 9, 13] {
        keys.push((path(&[PLAIN]), row(n)));
    }
    for key in [
        b"to a sibling".as_slice(),
        b"to a reference",
        b"to a group",
        b"to the root's",
        b"to the wide tree",
        b"empty",
        b"nested",
        b"no such key",
    ] {
        keys.push((path(&[PLAIN]), key.to_vec()));
    }
    for key in [b"shared".as_slice(), b"up and over", b"shared by none"] {
        keys.push((path(&[PLAIN, b"nested", b"inner a"]), key.to_vec()));
    }
    for key in [
        b"broken".as_slice(),
        b"to a tree",
        b"item",
        b"one hop",
        b"target",
        b"stale",
        b"moved",
    ] {
        keys.push((path(&[ODD]), key.to_vec()));
    }
    for kind in 0..made_by(version) {
        keys.push((path(&[GROUPS, &group(kind)]), row(3)));
        keys.push((path(&[GROUPS, &group(kind)]), b"to row 03".to_vec()));
        keys.push((path(&[GROUPS]), group(kind)));
    }
    for top in [GROUPS, PLAIN, WIDE, ODD, LINKED, b"nothing"] {
        keys.push((Vec::new(), top.to_vec()));
    }
    for key in [b"referred".as_slice(), b"reference", b"second", b"plain"] {
        keys.push((path(&[LINKED]), key.to_vec()));
    }
    // Under a tree that is not there, under an item, and under a tree
    // that nothing was written into.
    keys.push((path(&[PLAIN, b"none"]), row(1)));
    keys.push((path(&[PLAIN, &row(1)]), row(1)));
    keys.push((path(&[PLAIN, b"empty"]), row(1)));
    keys
}

fn named(phase: &str, call: &str, name: &str) -> String {
    format!("{phase} | {call} | {name}")
}

/// A key and the path of its subtree, as text.
fn at_key(at: &[Vec<u8>], key: &[u8]) -> String {
    let segments = at.iter().map(Vec::as_slice).chain([key]);
    let segments: Vec<_> = segments.map(String::from_utf8_lossy).collect();
    segments.join("/")
}

/// Reads the elements of [`keys`] in each way GroveDB reads one element.
fn read_keys<E: Engine>(recorder: &mut Recorder<'_, '_, '_, E>, phase: &str) {
    let (db, through, version) = (recorder.db, recorder.through, recorder.version);
    for (at, key) in keys(version) {
        let name = at_key(&at, &key);
        let segments = segments(&at);
        let at = segments.as_slice();
        let mut whole: Vec<&[u8]> = at.to_vec();
        whole.push(&key);
        let log = &mut *recorder.log;
        log.answer(
            named(phase, "get", &name),
            db.get(at, &key, through, version),
        );
        log.answer(
            named(phase, "get with no cache", &name),
            db.get_caching_optional(at.into(), &key, false, through, version),
        );
        log.answer(
            named(phase, "get raw", &name),
            db.get_raw(at.into(), &key, through, version),
        );
        log.answer(
            named(phase, "get raw optional", &name),
            db.get_raw_optional(at.into(), &key, through, version),
        );
        log.answer(
            named(phase, "has raw", &name),
            db.has_raw(at, &key, through, version),
        );
        log.answer(
            named(phase, "follow", &name),
            db.follow_reference(whole.as_slice().into(), true, through, version),
        );
    }
}

/// Runs the path queries of this module in each way GroveDB reads rows
/// and aggregates, the deprecated read of encoded values among them.
#[allow(deprecated)]
fn read_queries<E: Engine>(recorder: &mut Recorder<'_, '_, '_, E>, phase: &str) {
    use QueryResultType::{QueryKeyElementPairResultType, QueryPathKeyElementTrioResultType};

    let (db, through, version) = (recorder.db, recorder.through, recorder.version);
    for (name, query) in row_queries(version) {
        let log = &mut *recorder.log;
        log.answer(
            named(phase, "query", &name),
            db.query(
                &query,
                true,
                true,
                true,
                QueryPathKeyElementTrioResultType,
                through,
                version,
            ),
        );
        log.answer(
            named(phase, "query with no cache", &name),
            db.query(
                &query,
                false,
                false,
                false,
                QueryKeyElementPairResultType,
                through,
                version,
            ),
        );
        log.answer(
            named(phase, "query raw", &name),
            db.query_raw(
                &query,
                true,
                true,
                true,
                QueryPathKeyElementTrioResultType,
                through,
                version,
            ),
        );
        log.answer(
            named(phase, "item values", &name),
            db.query_item_value(&query, true, true, true, through, version),
        );
        log.answer(
            named(phase, "item values or sums", &name),
            db.query_item_value_or_sum(&query, true, true, true, through, version),
        );
        log.answer(
            named(phase, "sums", &name),
            db.query_sums(&query, true, true, true, through, version),
        );
        log.answer(
            named(phase, "run", &name),
            db.run_path_query(
                &query,
                true,
                true,
                true,
                QueryPathKeyElementTrioResultType,
                through,
                version,
            ),
        );
        if query.query.limit.is_some() {
            log.answer(
                named(phase, "keys optional", &name),
                db.query_keys_optional(&query, true, true, true, through, version),
            );
            log.answer(
                named(phase, "raw keys optional", &name),
                db.query_raw_keys_optional(&query, true, true, true, through, version),
            );
        }
    }
    for (name, first, second) in pairs(version) {
        let both = [&first, &second];
        recorder.log.answer(
            named(phase, "many raw", &name),
            db.query_many_raw(
                &both,
                true,
                true,
                true,
                QueryPathKeyElementTrioResultType,
                through,
                version,
            ),
        );
        recorder.log.answer(
            named(phase, "many encoded", &name),
            db.query_encoded_many(&both, true, true, true, through, version),
        );
    }
    for (name, query) in aggregate_queries(version) {
        let log = &mut *recorder.log;
        log.answer(
            named(phase, "count", &name),
            db.query_aggregate_count(&query, through, version),
        );
        log.answer(
            named(phase, "sum", &name),
            db.query_aggregate_sum(&query, through, version),
        );
        log.answer(
            named(phase, "count and sum", &name),
            db.query_aggregate_count_and_sum(&query, through, version),
        );
        log.answer(
            named(phase, "big sum", &name),
            db.query_aggregate_big_sum(&query, through, version),
        );
        log.answer(
            named(phase, "count and big sum", &name),
            db.query_aggregate_count_and_big_sum(&query, through, version),
        );
        log.answer(
            named(phase, "run", &name),
            db.run_path_query(
                &query,
                true,
                true,
                true,
                QueryPathKeyElementTrioResultType,
                through,
                version,
            ),
        );
    }
    for (name, walk) in sum_walks(version) {
        recorder.log.answer(
            named(phase, "sum walk", &name),
            db.query_aggregate_sums(&walk, true, true, through, version),
        );
    }
}

/// An answer that was charged nothing: what the verifier made of a proof.
fn note(log: &mut Log, name: String, answer: String) {
    log.answers.push((name, OperationCost::default(), answer));
}

/// What the verifier made of a proof: the root hash it came to, and its
/// answer as text.
type Verified = Result<([u8; 32], String), crate::Error>;

/// Whether the verifier took a proof and came to `root` by it: the call
/// the outcome is noted under, and the outcome as text.
fn verification(verified: Verified, root: &[u8; 32]) -> (&'static str, String) {
    match verified {
        Ok((verified_root, answer)) if verified_root == *root => {
            ("verified", format!("Ok({answer})"))
        }
        Ok((_, answer)) => (NOT_VERIFIED, format!("another root: {answer}")),
        Err(error) => (NOT_VERIFIED, error.to_string()),
    }
}

/// Has the verifier check the proof of a path query: the verifier of any
/// shape of query, and for a sum over a tree that sums in 128 bits the
/// verifier of such a sum, which the shape of a query does not tell from
/// a sum of 64 bits.
fn verify(proof: &[u8], query: &PathQuery, version: &GroveVersion) -> Verified {
    let by_shape = GroveDb::verify_path_query(proof, query, version);
    let by_shape = by_shape.map(|verified| (*verified.root_hash(), format!("{verified:?}")));
    by_shape.or_else(|refused| {
        let wide = match query.query.query.items.first() {
            Some(QueryItem::AggregateSumOnRange(_)) => {
                GroveDb::verify_aggregate_big_sum_query(proof, query, version)
                    .map(|(root, sum)| (root, format!("a sum of {sum}")))
            }
            Some(QueryItem::AggregateCountAndSumOnRange(_)) => {
                GroveDb::verify_aggregate_count_and_big_sum_query(proof, query, version)
                    .map(|(root, count, sum)| (root, format!("{count} rows, a sum of {sum}")))
            }
            _ => return Err(refused),
        };
        wide.map_err(|_| refused)
    })
}

/// The call under which a proof is noted that the verifier refused, or by
/// which it came to another root hash than the committed one.
const NOT_VERIFIED: &str = "not verified";

/// Proves every path query of this module from the committed state, and
/// has the verifier check each proof that was made. What it made of the
/// proof is noted as an answer of its own.
/// [`assert_verified`] then holds every proof to the root hash of the
/// committed state.
fn prove<E: Engine>(recorder: &mut Recorder<'_, '_, '_, E>, phase: &str) {
    let (db, version) = (recorder.db, recorder.version);
    let root = db.root_hash(None, version).unwrap().expect("a root hash");
    let mut queries = row_queries(version);
    queries.extend(aggregate_queries(version));
    for (at, (name, query)) in queries.iter().enumerate() {
        let proved = db.prove_query(query, None, version);
        if let Ok(proof) = &proved.value {
            let (call, verified) = verification(verify(proof, query, version), &root);
            note(recorder.log, named(phase, call, name), verified);
        }
        recorder.log.answer(named(phase, "prove", name), proved);
        // The read that answers with a proof, for some of the queries: it
        // takes no transaction, and is the prover otherwise.
        if at % 16 == 0 {
            let transaction = db.start_transaction();
            for through in [None, Some(&transaction)] {
                recorder.log.answer(
                    named(phase, "read proved", name),
                    db.get_proved_path_query(query, None, through, version),
                );
            }
        }
        // The other setting of the one option a prover is given, where
        // it can matter: a query with a limit and a subquery.
        if query.query.limit.is_some() && query.query.query.has_subquery() {
            let options = ProveOptions {
                decrease_limit_on_empty_sub_query_result: false,
            };
            recorder.log.answer(
                named(phase, "prove, empty trees uncounted", name),
                db.prove_query(query, Some(options), version),
            );
        }
    }
    for (name, first, second) in pairs(version) {
        let proved = db.prove_query_many(vec![&first, &second], None, version);
        recorder
            .log
            .answer(named(phase, "prove many", &name), proved);
    }

    // The trunk of a tree that counts, and the branch under each key the
    // trunk ends at.
    for (name, at, depth) in [
        ("the wide tree", path(&[WIDE]), 3u8),
        ("a counting group", path(&[GROUPS, &group(5)]), 2),
        ("a tree", path(&[GROUPS, &group(0)]), 2),
    ] {
        let trunk = PathTrunkChunkQuery {
            path: at.clone(),
            max_depth: depth,
            min_depth: None,
        };
        let proved = db.prove_trunk_chunk(&trunk, version);
        if let Ok(proof) = &proved.value {
            let verified = GroveDb::verify_trunk_chunk_proof(proof, &trunk, version);
            let leaves = match &verified {
                Ok((_, trunk)) => trunk.leaf_keys.clone(),
                Err(_) => BTreeMap::new(),
            };
            let verified = verified.map(|(root, trunk)| (root, format!("{:?}", trunk.elements)));
            let (call, verified) = verification(verified, &root);
            let trunk_name = format!("the trunk of {name}");
            note(recorder.log, named(phase, call, &trunk_name), verified);
            for (key, leaf) in leaves {
                let branch = PathBranchChunkQuery {
                    path: at.clone(),
                    key: key.clone(),
                    depth,
                };
                let proved = db.prove_branch_chunk(&branch, version);
                let branch_name = format!("{name} under {}", String::from_utf8_lossy(&key));
                if let Ok(proof) = &proved.value {
                    // A branch is held to the hash the trunk gives for
                    // its key, which the trunk's proof led to the root.
                    let verified =
                        GroveDb::verify_branch_chunk_proof(proof, &branch, leaf.hash, version);
                    let verified = verified.map(|branch| (root, format!("{:?}", branch.elements)));
                    let (call, verified) = verification(verified, &root);
                    note(recorder.log, named(phase, call, &branch_name), verified);
                }
                recorder
                    .log
                    .answer(named(phase, "prove a branch", &branch_name), proved);
            }
        }
        recorder
            .log
            .answer(named(phase, "prove a trunk", name), proved);
    }
}

/// The writes a history makes between two rounds of reads, through the
/// transaction of `recorder` or through none: rows written, replaced and
/// removed in the groups, the plain tree and the wide one, a row in a tree
/// that had none, and a group that is new. `salt` tells the rounds apart.
/// The last key of a subtree stays, and so does every row a reference
/// leads to: a plain reference is hashed with its target as that was when
/// the reference was written, and its proof is refused once the target
/// is another (`a stale reference`).
fn rewrite<E: Engine>(recorder: &mut Recorder<'_, '_, '_, E>, salt: u8) {
    let made = made_by(recorder.version);
    for (kind, of_kind) in KINDS.iter().enumerate().take(made) {
        let rows = path(&[GROUPS, &group(kind)]);
        let value = i64::from(salt) * 100 + kind as i64;
        recorder.insert(&rows, &row(20 + salt), number(of_kind.sums, value));
        recorder.insert(&rows, &row(0), number(of_kind.sums, -value));
        recorder.delete(&rows, &row(4 + salt));
    }
    let plain = path(&[PLAIN]);
    recorder.insert(&plain, &row(30 + salt), Element::new_item(vec![salt; 700]));
    recorder.insert(&plain, &row(4), Element::new_item(vec![salt; 9]));
    recorder.delete(&plain, &row(5 + salt));
    let empty = path(&[PLAIN, b"empty"]);
    recorder.insert(&empty, &row(salt), Element::new_item(vec![salt]));
    let wide = path(&[WIDE]);
    for n in 0..6 {
        recorder.delete(&wide, &row(10 * salt + n));
        recorder.insert(&wide, &row(100 + 10 * salt + n), Element::new_item(vec![n]));
    }
    let new_group = format!("group new {salt}").into_bytes();
    recorder.insert(&path(&[GROUPS]), &new_group, Element::empty_sum_tree());
    let rows = path(&[GROUPS, &new_group]);
    for n in 0..3 {
        recorder.insert(&rows, &row(n), Element::new_sum_item(i64::from(n) + 1));
    }
}

/// The history of this module on one engine.
fn reads_and_proofs<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    let db = open::<E>(dir);
    let mut log = Log::default();
    grove(&mut Recorder::new(&db, version, None, &mut log));

    // With no transaction, and by the prover.
    let mut committed = Recorder::new(&db, version, None, &mut log);
    read_keys(&mut committed, "committed");
    read_queries(&mut committed, "committed");
    prove(&mut committed, "committed");

    // A snapshot, and commits that it does not show.
    let snapshot = db.start_snapshot_read_transaction();
    rewrite(&mut Recorder::new(&db, version, None, &mut log), 1);
    let mut pinned = Recorder::new(&db, version, Some(&snapshot), &mut log);
    read_keys(&mut pinned, "snapshot");
    read_queries(&mut pinned, "snapshot");
    drop(snapshot);

    // A write transaction with writes of its own: before a commit beside
    // it, and after one, which it reads wherever it has not written.
    let transaction = db.start_transaction();
    let mut inside = Recorder::new(&db, version, Some(&transaction), &mut log);
    rewrite(&mut inside, 2);
    read_keys(&mut inside, "transaction");
    read_queries(&mut inside, "transaction");
    let mut outside = Recorder::new(&db, version, None, &mut log);
    let beside = path(&[PLAIN, b"nested", b"inner b"]);
    outside.insert(&beside, &row(40), Element::new_item(b"beside".to_vec()));
    outside.delete(&beside, &row(1));
    let mut inside = Recorder::new(&db, version, Some(&transaction), &mut log);
    read_keys(&mut inside, "transaction, after a commit beside it");
    read_queries(&mut inside, "transaction, after a commit beside it");
    db.rollback_transaction(&transaction).expect("a rollback");
    drop(transaction);

    // What is committed by now, read and proven again.
    let mut committed = Recorder::new(&db, version, None, &mut log);
    read_queries(&mut committed, "committed again");
    prove(&mut committed, "committed again");
    db.flush().expect("a flush");
    log
}

/// The records a cursor shows from one end of its subtree to the other,
/// sought first, as one answer of a history: each key with its record,
/// what the walk was charged, and how it ended.
fn walk<E: Engine>(
    log: &mut Log,
    name: String,
    cursor: &mut crate::engine::SubtreeCursor<'_, E>,
    forward: bool,
) {
    let mut cost = OperationCost::default();
    let mut records = String::new();
    match forward {
        true => cursor.seek_to_first(),
        false => cursor.seek_to_last(),
    }
    .unwrap_add_cost(&mut cost);
    while cursor.valid().unwrap_add_cost(&mut cost) {
        let key = cursor.key().unwrap_add_cost(&mut cost).expect("a key");
        records.push_str(&String::from_utf8_lossy(key));
        let record = cursor.value().unwrap_add_cost(&mut cost).expect("a record");
        records.push_str(&format!(" {}, ", hex::encode(record)));
        match forward {
            true => cursor.next(),
            false => cursor.prev(),
        }
        .unwrap_add_cost(&mut cost);
    }
    records.push_str(&format!(
        "{:?}",
        cursor.status().map_err(|error| error.to_string())
    ));
    log.answers.push((name, cost, records));
}

/// One cursor read in several subtrees (`Engine::retarget_cursor`),
/// through a read transaction, a snapshot read transaction and a write
/// transaction that has written one of the subtrees. Commits land after
/// the cursor was opened, in every subtree it is then pointed at.
fn pointed_elsewhere<E: Engine>(dir: &Path, version: &'static GroveVersion) -> Log {
    use grovedb_merk::store::NodeStore;
    use grovedb_storage::prefix::build_prefix;

    let db = open::<E>(dir);
    let mut log = Log::default();
    let trees: [&[u8]; 3] = [b"first", b"second", b"third"];
    let mut recorder = Recorder::new(&db, version, None, &mut log);
    for (at, tree) in trees.iter().enumerate() {
        recorder.insert(&[], tree, Element::empty_tree());
        for n in 0..4 + at as u8 {
            recorder.insert(&path(&[tree]), &row(2 * n), Element::new_item(vec![n; 3]));
        }
    }
    let prefix = |tree: &[u8]| build_prefix(SubtreePath::from([tree].as_ref())).unwrap();
    let engine = db.engine();
    for (round, kind) in ["read", "snapshot", "write"].into_iter().enumerate() {
        let round = round as u8;
        let transaction = match kind {
            "read" => engine.start_read_transaction(),
            "snapshot" => engine.start_snapshot_read_transaction(),
            _ => engine.start_transaction(),
        };
        if kind == "write" {
            // The transaction's own writes, which its cursor shows in
            // the subtree they are made in and in no other.
            let mut inside = Recorder::new(&db, version, Some(&transaction), &mut log);
            let second = path(&[trees[1]]);
            inside.insert(&second, &row(1), Element::new_item(b"written".to_vec()));
            inside.delete(&second, &row(6));
        }
        let handle = engine.subtree_by_prefix(prefix(trees[0]), None, &transaction);
        let handle = handle.unwrap();
        let mut cursor = NodeStore::cursor(&handle);
        walk::<E>(&mut log, format!("{kind}: the first"), &mut cursor, true);
        // Commits land in every subtree.
        let mut outside = Recorder::new(&db, version, None, &mut log);
        for tree in trees {
            let item = Element::new_item(vec![round; 5]);
            outside.insert(&path(&[tree]), &row(50 + round), item);
            outside.delete(&path(&[tree]), &row(2 * round));
        }
        // The cursor is pointed at each of them in turn, and at a subtree
        // that is not there. It is sought before it is read.
        for (name, tree, forward) in [
            ("the second", trees[1], true),
            ("the third", trees[2], false),
            ("nothing", b"none".as_slice(), true),
            ("the first again", trees[0], false),
            ("the second again", trees[1], false),
        ] {
            cursor = E::retarget_cursor(cursor, prefix(tree));
            walk::<E>(&mut log, format!("{kind}: {name}"), &mut cursor, forward);
        }
        // A cursor that is opened now shows the commits.
        drop(cursor);
        let late = engine.start_read_transaction();
        let handle = engine.subtree_by_prefix(prefix(trees[1]), None, &late);
        let handle = handle.unwrap();
        let mut cursor = NodeStore::cursor(&handle);
        walk::<E>(&mut log, format!("{kind}: afterwards"), &mut cursor, true);
    }
    db.flush().expect("a flush");
    log
}

#[test]
fn a_cursor_is_pointed_at_another_subtree_as_on_rocksdb() {
    for version in [&GROVE_VERSIONS[0], GroveVersion::latest()] {
        let dir = tempfile::tempdir().expect("a directory");
        let log = pointed_elsewhere::<LigninEngine>(dir.path(), version);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let on_rocksdb = pointed_elsewhere::<crate::RocksDbEngine>(dir.path(), version);
            assert_same_calls(&log.answers, &on_rocksdb.answers);
        }
        check(dir.path(), version);
        // What each cursor showed of the second subtree: the rows it had
        // when the cursor was opened, and for the write transaction its
        // own row in the place of the one it removed.
        let shown = |name: &str| -> Vec<String> {
            let answer = log.answers.iter().find(|answer| answer.0 == name);
            let records = &answer.expect("a walk of that name").2;
            let keys = records
                .split(", ")
                .filter(|record| record.starts_with("row"));
            keys.map(|record| record[..6].to_owned()).collect()
        };
        assert_eq!(
            shown("read: the second"),
            ["row 00", "row 02", "row 04", "row 06", "row 08"]
        );
        assert_eq!(
            shown("snapshot: the second"),
            ["row 02", "row 04", "row 06", "row 08", "row 50"]
        );
        assert_eq!(
            shown("write: the second"),
            ["row 01", "row 04", "row 08", "row 50", "row 51"]
        );
        assert_eq!(shown("write: nothing"), Vec::<String>::new());
    }
}

/// A cursor of the LigninDB store keeps one version pinned for as long as
/// it lives, and a pinned version holds back what the engine reclaims.
/// GroveDB keeps no cursor past the operation that opened it: when a read,
/// a query, a walk or a proof has returned, through a write transaction of
/// the caller's or through none, the engine holds the pins it held before.
#[test]
fn no_operation_keeps_a_version_pinned_once_it_has_returned() {
    use grovedb_merk::store::NodeStore;

    let version = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    let db = open::<LigninEngine>(dir.path());
    let mut log = Log::default();
    grove(&mut Recorder::new(&db, version, None, &mut log));
    let pins = || db.engine().database().stats().pins;
    assert_eq!(pins(), 0, "nothing is open");

    let transaction = db.start_transaction();
    rewrite(
        &mut Recorder::new(&db, version, Some(&transaction), &mut log),
        1,
    );
    // What the open transaction holds by itself.
    let held = pins();
    let plain: [&[u8]; 1] = [PLAIN];
    for through in [None, Some(&transaction)] {
        let mut recorder = Recorder::new(&db, version, through, &mut log);
        read_keys(&mut recorder, "pins");
        assert_eq!(pins(), held, "after reads by key");
        read_queries(&mut recorder, "pins");
        assert_eq!(pins(), held, "after queries");
        let subtrees = db.find_subtrees(&SubtreePath::from(plain.as_ref()), through, version);
        assert!(subtrees.unwrap().expect("the subtrees").len() > 3);
        let empty = db.is_empty_tree(plain.as_ref(), through, version);
        assert!(!empty.unwrap().expect("an answer"));
        let records = db.get_aux_by_key_prefix(b"none", through);
        assert!(records.unwrap().expect("a walk").is_empty());
        let issues = db.verify_grovedb(through, false, true, version);
        assert!(issues.is_ok(), "{issues:?}");
        assert_eq!(pins(), held, "after walks");
    }
    prove(&mut Recorder::new(&db, version, None, &mut log), "pins");
    assert_eq!(pins(), held, "after proofs");

    // A cursor that lives is a pin that is held, and goes with it.
    let engine = db.engine();
    let handle = engine.subtree(SubtreePath::from(plain.as_ref()), None, &transaction);
    let handle = handle.unwrap();
    let mut cursor = NodeStore::cursor(&handle);
    cursor.seek_to_first().unwrap();
    assert_eq!(pins(), held + 1, "an open cursor");
    drop(cursor);
    assert_eq!(pins(), held);
    drop(handle);
    db.rollback_transaction(&transaction).expect("a rollback");
    drop(transaction);
    assert_eq!(pins(), 0, "nothing is open");
}

/// A move of a cursor that is refused inside a scan fails the operation,
/// on both engines alike: the histories of
/// [`refused_scans`](crate::tests::refused_scans::refused_scans), which
/// refuse each move of each scan in turn, give the same answers at the
/// same costs.
#[test]
fn a_move_refused_inside_a_scan_fails_the_operation_as_on_rocksdb() {
    use crate::tests::refused_scans::refused_scans;

    for version in GROVE_VERSIONS {
        let dir = tempfile::tempdir().expect("a directory");
        let log = refused_scans::<LigninEngine>(dir.path(), version);
        #[cfg(feature = "rocksdb_storage")]
        {
            let dir = tempfile::tempdir().expect("a directory");
            let on_rocksdb = refused_scans::<crate::RocksDbEngine>(dir.path(), version);
            assert_same_calls(&log, &on_rocksdb);
        }
        check(dir.path(), version);
        eprintln!(
            "refused scans on grove version {}: {} calls",
            version.protocol_version,
            log.len()
        );
    }
}

/// The kind of a call, from its name, and whether it was answered.
fn kind_of(answer: &Answer) -> (String, bool) {
    let mut parts = answer.0.split(" | ");
    let call = parts.nth(1).unwrap_or("write");
    (call.to_owned(), answer.2.starts_with("Ok"))
}

/// How many calls of each kind a history made, and how many of them were
/// answered and not refused.
fn tally(log: &Log) -> BTreeMap<String, (usize, usize)> {
    let mut tally: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for answer in &log.answers {
        let (call, answered) = kind_of(answer);
        let of_call = tally.entry(call).or_default();
        of_call.0 += 1;
        of_call.1 += usize::from(answered);
    }
    tally
}

/// Every call of the two histories gave the same answer at the same
/// cost. Lists the calls that did not, the first of them in full.
#[cfg_attr(not(feature = "rocksdb_storage"), allow(dead_code))]
fn assert_same_calls(lignin: &[Answer], rocksdb: &[Answer]) {
    let short = |text: &str| -> String { text.chars().take(600).collect() };
    let mut different = Vec::new();
    for (call, (lignin, rocksdb)) in lignin.iter().zip(rocksdb).enumerate() {
        if lignin != rocksdb {
            different.push(format!(
                "call {call}: {}\n  LigninDB {:?}\n    {}\n  RocksDB  {:?}\n    {}",
                lignin.0,
                lignin.1,
                short(&lignin.2),
                rocksdb.1,
                short(&rocksdb.2)
            ));
        }
    }
    assert!(
        different.is_empty(),
        "{} of {} calls differ; the first of them:\n{}",
        different.len(),
        lignin.len(),
        different
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(lignin.len(), rocksdb.len());
}

/// The queries whose proofs the verifier of `version` refuses, on every
/// engine: the proof of a reference whose target was written again after
/// it, and on grove version 3 the proof of a reference that leads to a
/// tree with nodes, for which the prover of that version gives a proof
/// node its verifier does not take.
fn refused_by_the_verifier(version: &GroveVersion) -> Vec<&'static str> {
    let mut refused = vec![STALE];
    if version.protocol_version == GROVE_VERSIONS[2].protocol_version {
        refused.push("a reference to a tree");
    }
    refused
}

/// Every proof that was made was taken by the verifier and led it to the
/// root hash of the committed state, but those of
/// [`refused_by_the_verifier`], each of which it refused in both rounds
/// of proofs.
fn assert_verified(log: &Log, version: &GroveVersion) {
    let refused = (log.answers.iter()).filter(|answer| kind_of(answer).0 == NOT_VERIFIED);
    let mut refused: Vec<String> = refused
        .map(|answer| format!("{}: {}", answer.0, answer.2))
        .collect();
    refused.sort();
    let mut expected: Vec<String> = Vec::new();
    for name in refused_by_the_verifier(version) {
        for phase in ["committed", "committed again"] {
            expected.push(named(phase, NOT_VERIFIED, name));
        }
    }
    expected.sort();
    let names: Vec<&str> = (refused.iter())
        .map(|refused| refused.split(": ").next().unwrap_or_default())
        .collect();
    assert_eq!(names, expected, "{refused:#?}");
}

/// Runs the history on LigninDB, and where the RocksDB engine is compiled
/// in on RocksDB too, and compares every call. The physical checker then
/// reads the LigninDB database the history left.
fn on_both_engines(version: &'static GroveVersion) -> Log {
    let dir = tempfile::tempdir().expect("a directory");
    let log = reads_and_proofs::<LigninEngine>(dir.path(), version);
    #[cfg(feature = "rocksdb_storage")]
    {
        let dir = tempfile::tempdir().expect("a directory");
        let on_rocksdb = reads_and_proofs::<crate::RocksDbEngine>(dir.path(), version);
        assert_same_calls(&log.answers, &on_rocksdb.answers);
    }
    check(dir.path(), version);
    assert_verified(&log, version);
    let tally = tally(&log);
    eprintln!(
        "reads and proofs on grove version {}, proofs of version {}: {} calls",
        version.protocol_version,
        proof_version(version),
        log.answers.len()
    );
    for (call, (made, answered)) in &tally {
        eprintln!("  {call}: {made} calls, {answered} answered");
    }
    log
}

/// The version of the proofs that the prover of `version` makes.
fn proof_version(version: &GroveVersion) -> u16 {
    let proof = &version.grovedb_versions.operations.proof;
    proof.prove_query_non_serialized
}

/// The histories of this module are run on every grove version, and so
/// with the provers of both proof versions.
#[test]
fn the_grove_versions_prove_in_both_proof_versions() {
    let proved: std::collections::BTreeSet<u16> =
        GROVE_VERSIONS.iter().map(proof_version).collect();
    assert_eq!(proved.into_iter().collect::<Vec<u16>>(), [0, 1]);
}

/// What every grove version answers: each way to read an element and
/// rows, and proofs that the verifier took.
fn assert_answered(log: &Log, calls: &[&str]) {
    let tally = tally(log);
    for call in calls {
        let (made, answered) = tally.get(*call).copied().unwrap_or_default();
        assert!(answered > 0, "{call}: {made} calls, none answered");
    }
}

const ON_EVERY_VERSION: [&str; 23] = [
    "get",
    "get with no cache",
    "get raw",
    "get raw optional",
    "has raw",
    "follow",
    "query",
    "query with no cache",
    "query raw",
    "item values",
    "item values or sums",
    "sums",
    "run",
    "keys optional",
    "raw keys optional",
    "many raw",
    "many encoded",
    "sum walk",
    "prove",
    "prove many",
    "prove, empty trees uncounted",
    "read proved",
    "verified",
];

#[test]
fn reads_and_proofs_on_grove_v1() {
    assert_answered(&on_both_engines(&GROVE_VERSIONS[0]), &ON_EVERY_VERSION);
}

#[test]
fn reads_and_proofs_on_grove_v2() {
    assert_answered(&on_both_engines(&GROVE_VERSIONS[1]), &ON_EVERY_VERSION);
}

#[test]
fn reads_and_proofs_on_grove_v3() {
    assert_answered(&on_both_engines(&GROVE_VERSIONS[2]), &ON_EVERY_VERSION);
}

#[test]
fn reads_and_proofs_on_grove_v4() {
    assert_answered(&on_both_engines(&GROVE_VERSIONS[3]), &ON_EVERY_VERSION);
}

/// The newest version also answers every aggregate, proves trunks and
/// branches, and reads a page of its own for each group through a write
/// transaction of the caller's, which the engine refused before it could
/// point a cursor of such a transaction at another subtree.
#[test]
fn reads_and_proofs_on_grove_v5() {
    let log = on_both_engines(&GROVE_VERSIONS[4]);
    assert_answered(&log, &ON_EVERY_VERSION);
    assert_answered(
        &log,
        &[
            "count",
            "sum",
            "count and sum",
            "big sum",
            "count and big sum",
            "prove a trunk",
            "prove a branch",
        ],
    );
    let pages = |phase: &str| {
        let of_phase = log.answers.iter().filter(move |answer| {
            answer.0.starts_with(&format!("{phase} | "))
                && answer.0.contains("a page of each counting group")
                && ["query raw", "run"].contains(&kind_of(answer).0.as_str())
        });
        of_phase
            .map(|answer| answer.2.starts_with("Ok"))
            .collect::<Vec<bool>>()
    };
    for phase in [
        "committed",
        "snapshot",
        "transaction",
        "transaction, after a commit beside it",
    ] {
        assert_eq!(pages(phase), [true; 4], "{phase}");
    }
    // A page by count is proven as well, of each group and of one tree,
    // and the verifier takes the proof.
    for page in [
        "a page of each counting group",
        "a page far into the wide tree",
    ] {
        for call in ["prove", "verified"] {
            let answered = log.answers.iter().filter(|answer| {
                kind_of(answer) == (call.to_owned(), true) && answer.0.contains(page)
            });
            assert_eq!(answered.count(), 4, "{call}: {page}");
        }
    }
}
