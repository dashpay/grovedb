//! The cases for operations and forms of query an older grove version
//! lacks: what it answers, what it refuses, and what it refuses to prove.
//!
//! Every read of these cases asks for a proof, where a generated stream for
//! such a version marks the read as not proven and so records no proof.

use grovedb::{
    AggregateSumPathQuery, AxisQuery, Element, PathQuery, Query as GroveQuery, QueryItem as Item,
    TreeType,
};
use grovedb_merk::proofs::query::{AggregateFold, AggregateSumQuery, IndexAxis};

use super::{
    case, insert, insert_only, item, path, provable_count_provable_sum_tree, provable_count_tree,
    provable_sum_tree, proven, query, sized, BatchOp, Case, Op, Query, QueryItem, Read, Write,
    HOST, SIBLING, TOP,
};

const GROUPS: &[u8] = b"groups";
const RANKED: &[u8] = b"ranked";
const BY_COUNT: &[u8] = b"by-count";

/// The key of the `row`-th row of a tree, or of the `row`-th tree of a
/// group of trees.
fn row(row: u8) -> Vec<u8> {
    vec![b'r', b'0' + row]
}

/// `TOP` holding a `HOST` tree of the given element with `rows` rows, each
/// the element `filler` gives for its number, and committed.
fn hosted(host: Element, rows: u8, filler: impl Fn(u8) -> Element) -> Vec<Op> {
    let mut ops = vec![
        insert(&[], TOP, Element::empty_tree()),
        insert(&[TOP], HOST, host),
    ];
    ops.extend((0..rows).map(|number| insert(&[TOP, HOST], &row(number), filler(number))));
    ops.push(Op::Commit);
    ops
}

/// `TOP` holding a `GROUPS` tree of three provable count trees, the
/// `group`-th with `group + 2` rows, and committed.
fn grouped() -> Vec<Op> {
    let mut ops = vec![
        insert(&[], TOP, Element::empty_tree()),
        insert(&[TOP], GROUPS, Element::empty_tree()),
    ];
    for group in 0..3 {
        ops.push(insert(
            &[TOP, GROUPS],
            &row(group),
            Element::empty_provable_count_tree(),
        ));
        for number in 0..group + 2 {
            ops.push(insert(
                &[TOP, GROUPS, &row(group)],
                &row(number),
                item(&[b'v', group, number]),
            ));
        }
    }
    ops.push(Op::Commit);
    ops
}

/// An item and a tree moved to other keys of their tree, each in a batch
/// of its own.
fn move_case() -> Case {
    let in_host: &[&[u8]] = &[TOP, HOST];
    let move_key = |key: &[u8], new_key: &[u8]| {
        Op::Batch(vec![BatchOp::Move {
            path: path(in_host),
            key: key.to_vec(),
            new_key: new_key.to_vec(),
        }])
    };
    let everything = || {
        Op::Read(Read::Query(Query {
            path: path(in_host),
            items: vec![QueryItem::RangeFull],
            subquery_path: None,
            subquery_all: true,
            limit: None,
            left_to_right: true,
            raw: true,
        }))
    };
    case(
        "move-in-batch",
        "Grove versions 1 to 3 have no batch Move: the batch is refused and the element stays \
         at its key.",
        vec![
            insert(&[], TOP, Element::empty_tree()),
            insert(&[TOP], HOST, Element::empty_tree()),
            insert(in_host, b"a", item(b"an item that moves")),
            insert(in_host, b"tree", Element::empty_tree()),
            insert(&[TOP, HOST, b"tree"], b"leaf", item(b"moves with its tree")),
            Op::Commit,
            move_key(b"a", b"b"),
            Op::Commit,
            everything(),
            move_key(b"tree", b"wood"),
            Op::Commit,
            everything(),
        ],
    )
}

/// A populated tree dropped flat, by `GroveDb::drop_flat_subtree` or by a
/// batch, and its storage reclaimed.
fn drop_flat_case(in_batch: bool) -> Case {
    let drop = if in_batch {
        Op::Batch(vec![BatchOp::DropFlat {
            path: path(&[TOP]),
            key: HOST.to_vec(),
            tree_type: TreeType::NormalTree,
        }])
    } else {
        Op::Write(Write::DropFlat {
            path: path(&[TOP]),
            key: HOST.to_vec(),
        })
    };
    let mut ops = hosted(Element::empty_tree(), 2, |number| item(&[b'v', number]));
    ops.extend([
        insert(&[TOP], SIBLING, item(b"stays")),
        Op::Commit,
        drop,
        Op::Commit,
        Op::FlushPendingDrops,
        query(&[TOP], QueryItem::RangeFull, true),
    ]);
    if in_batch {
        case(
            "drop-flat-in-batch",
            "Grove versions 1 to 3 have no flat drop: a batch DeleteTree that asks for one is \
             refused and the tree stays.",
            ops,
        )
    } else {
        case(
            "drop-flat",
            "Grove versions 1 to 3 have no flat drop: GroveDb::drop_flat_subtree is refused, \
             the tree stays and nothing is queued for reclaiming.",
            ops,
        )
    }
}

/// The three aggregates over a range of keys, each on the tree type that
/// commits it.
fn aggregate_case() -> Case {
    let hosts = [
        (b"count".as_slice(), provable_count_tree()),
        (b"sum".as_slice(), provable_sum_tree()),
        (b"both".as_slice(), provable_count_provable_sum_tree()),
    ];
    let mut ops = vec![insert(&[], TOP, Element::empty_tree())];
    for (key, host) in &hosts {
        ops.push(insert(&[TOP], key, host.element.clone()));
        for number in 0..5 {
            let element = match &host.filler {
                Element::SumItem(..) => Element::new_sum_item(10 * i64::from(number) - 15),
                _ => item(&[b'v', number]),
            };
            ops.push(insert(&[TOP, key], &row(number), element));
        }
    }
    ops.extend([
        Op::Commit,
        proven(PathQuery::new_aggregate_count_on_range(
            path(&[TOP, b"count"]),
            Item::RangeInclusive(row(1)..=row(3)),
        )),
        proven(PathQuery::new_aggregate_sum_on_range(
            path(&[TOP, b"sum"]),
            Item::RangeFrom(row(2)..),
        )),
        proven(PathQuery::new_aggregate_count_and_sum_on_range(
            path(&[TOP, b"both"]),
            Item::Range(row(0)..row(4)),
        )),
    ]);
    case(
        "aggregate-on-range",
        "Grove versions 1 and 2 answer an aggregate over a range of keys and refuse to prove \
         it: the proof needs the envelope of grove version 3.",
        ops,
    )
}

/// One count for each of several groups: a carrier query, whose subquery
/// is the aggregate.
fn aggregate_per_key_case() -> Case {
    let mut groups = GroveQuery::new();
    groups.insert_key(row(0));
    groups.insert_key(row(2));
    groups.insert_key(row(7));
    groups.set_subquery(GroveQuery::new_aggregate_count_on_range(Item::RangeFrom(
        row(1)..,
    )));
    let mut ops = grouped();
    ops.push(proven(sized(&[TOP, GROUPS], groups, None, None)));
    case(
        "aggregate-per-key",
        "Grove versions 1 and 2 answer an aggregate for each of several keys and refuse to \
         prove it, as they do for one key.",
        ops,
    )
}

/// The carrier of [`aggregate_per_key_case`] asked for every group.
fn aggregate_over_every_key_case() -> Case {
    let mut groups = GroveQuery::new_range_full();
    groups.set_subquery(GroveQuery::new_aggregate_count_on_range(Item::RangeFrom(
        row(1)..,
    )));
    let mut ops = grouped();
    ops.push(proven(sized(&[TOP, GROUPS], groups, None, None)));
    case(
        "aggregate-per-key-over-every-key",
        "Every grove version refuses an aggregate for each key when the keys are given as \
         the full range. The generators name the keys or bound the range. The range the \
         aggregate itself is taken over is bounded here, as GroveDB requires.",
        ops,
    )
}

/// Pages of a tree that commits its count: one inside its rows and one past
/// their end.
fn count_offset_case() -> Case {
    let page = || GroveQuery::new_single_query_item(Item::RangeFrom(row(1)..));
    let mut ops = hosted(Element::empty_provable_count_tree(), 6, |number| {
        item(&[b'v', number])
    });
    ops.extend([
        proven(sized(&[TOP, HOST], page(), Some(2), Some(3))),
        proven(sized(&[TOP, HOST], page(), Some(2), Some(9))),
    ]);
    case(
        "count-offset-page",
        "Grove versions 1 and 2 answer a page of a tree that commits its count and refuse to \
         prove it: skipping rows by count needs the proof envelope of grove version 3.",
        ops,
    )
}

/// Reads of an indexed tree in the order of its counts, one branch at a
/// time and several at once.
fn axis_case() -> Case {
    let mut ops = vec![
        insert(&[], TOP, Element::empty_tree()),
        insert(&[TOP], RANKED, Element::empty_tree()),
    ];
    for branch in 0..2u8 {
        let in_branch: &[&[u8]] = &[TOP, RANKED, &row(branch)];
        let in_index: &[&[u8]] = &[TOP, RANKED, &row(branch), BY_COUNT];
        ops.push(Op::Batch(vec![
            insert_only(&[TOP, RANKED], &row(branch), Element::empty_tree()),
            insert_only(
                in_branch,
                BY_COUNT,
                Element::empty_provable_count_indexed_tree(),
            ),
        ]));
        ops.push(Op::Commit);
        for entry in 0..3u8 {
            ops.push(Op::Batch(vec![insert_only(
                in_index,
                &row(entry),
                Element::empty_count_tree(),
            )]));
            let in_entry: &[&[u8]] = &[TOP, RANKED, &row(branch), BY_COUNT, &row(entry)];
            ops.push(Op::Batch(
                (0..1 + (entry + branch) % 3 * 2)
                    .map(|number| insert_only(in_entry, &row(number), item(&[b'v', number])))
                    .collect(),
            ));
        }
        ops.push(Op::Commit);
    }
    let index = || path(&[TOP, RANKED, &row(0), BY_COUNT]);
    ops.extend([
        proven(PathQuery::new_axis_top_k(
            index(),
            IndexAxis::Count,
            2,
            0,
            true,
        )),
        proven(PathQuery::new_axis(
            index(),
            AxisQuery::bottom_k(IndexAxis::Count, 2, 1).keys_only(),
        )),
        proven(PathQuery::new_axis_bounded(
            index(),
            IndexAxis::Count,
            2,
            5,
            4,
            false,
        )),
        proven(PathQuery::new_axis_rank_of_key(
            index(),
            IndexAxis::Count,
            row(1),
            true,
        )),
        proven(PathQuery::new_axis_aggregate_over_value_range(
            index(),
            IndexAxis::Count,
            0,
            4,
            AggregateFold::Total,
        )),
        proven(PathQuery::new_branched_axis(
            path(&[TOP, RANKED]),
            vec![row(0), row(1), row(7)],
            vec![BY_COUNT.to_vec()],
            AxisQuery::top_k(IndexAxis::Count, 2, 0, true),
        )),
    ]);
    case(
        "axis-read",
        "Grove versions 1 to 3 store an indexed tree and refuse to read it in the order of \
         its aggregate: the read modes of a query come with grove version 4.",
        ops,
    )
}

/// A walk over sum items that stops at a running total, through the reader
/// of its own and as a read mode of a query.
fn sum_budget_case() -> Case {
    let mut ops = hosted(Element::empty_sum_tree(), 6, |number| match number {
        2 => item(b"no sum"),
        _ => Element::new_sum_item(100 * i64::from(number) + 50),
    });
    let items = || vec![Item::RangeFull(..)];
    ops.extend([
        Op::Read(Read::SumBudget {
            query: Box::new(AggregateSumPathQuery::new(
                path(&[TOP, HOST]),
                AggregateSumQuery {
                    items: items(),
                    left_to_right: true,
                    sum_limit: 500,
                    limit_of_items_to_check: None,
                },
            )),
            skip_non_sum_items: true,
            ignore_references: false,
        }),
        proven(PathQuery::new_sum_budget(
            path(&[TOP, HOST]),
            items(),
            true,
            500,
            None,
        )),
        proven(PathQuery::new_sum_budget(
            path(&[TOP, HOST]),
            items(),
            false,
            10_000,
            Some(3),
        )),
    ]);
    case(
        "sum-budget-window",
        "Every grove version walks sum items up to a running total through \
         GroveDb::query_aggregate_sums. Grove versions 1 to 3 refuse the same walk as a read \
         mode of a query, which is the form that is proven.",
        ops,
    )
}

/// A limit of its own for each group.
fn subquery_limit_case() -> Case {
    let mut first_rows = GroveQuery::new_range_full();
    first_rows.limit = Some(2);
    let mut groups = GroveQuery::new_range_full();
    groups.set_subquery(first_rows);
    let mut ops = grouped();
    ops.push(proven(sized(&[TOP, GROUPS], groups, None, None)));
    case(
        "subquery-limit",
        "Grove versions 1 to 3 refuse a query whose subquery has a limit of its own.",
        ops,
    )
}

/// A page of its own for each group.
fn subquery_offset_case() -> Case {
    let mut page = GroveQuery::new_range_full();
    page.offset = Some(1);
    page.limit = Some(2);
    let mut groups = GroveQuery::new();
    groups.insert_key(row(1));
    groups.insert_key(row(2));
    groups.set_subquery(page);
    let mut ops = grouped();
    ops.push(proven(sized(&[TOP, GROUPS], groups, None, None)));
    case(
        "subquery-offset",
        "Grove versions 1 to 4 refuse a query whose subquery has an offset of its own.",
        ops,
    )
}

/// An offset where no count commits what it skips: a page of a plain
/// tree, and an offset for the whole of a query that has a subquery.
fn offset_without_count_case() -> Case {
    let all_groups = || {
        let mut groups = GroveQuery::new_range_full();
        groups.set_subquery(GroveQuery::new_range_full());
        sized(&[TOP, GROUPS], groups, Some(3), Some(2))
    };
    let rows = |raw| {
        Op::Read(Read::Rows {
            query: Box::new(all_groups()),
            raw,
        })
    };
    let mut ops = grouped();
    ops.extend([
        insert(&[TOP], SIBLING, item(b"a row beside the groups")),
        Op::Commit,
        proven(sized(
            &[TOP],
            GroveQuery::new_range_full(),
            Some(1),
            Some(1),
        )),
        proven(all_groups()),
        rows(false),
        rows(true),
    ]);
    case(
        "offset-without-count",
        "No grove version proves an offset the rows of a plain tree are skipped by, or an \
         offset next to a subquery. GroveDb::run_path_query answers the first and refuses \
         the second, which GroveDb::query and GroveDb::query_raw take.",
        ops,
    )
}

/// The cases of this module, in the order they are documented.
pub(super) fn cases() -> Vec<Case> {
    vec![
        move_case(),
        drop_flat_case(false),
        drop_flat_case(true),
        aggregate_case(),
        aggregate_per_key_case(),
        count_offset_case(),
        axis_case(),
        sum_budget_case(),
        subquery_limit_case(),
        subquery_offset_case(),
        offset_without_count_case(),
        aggregate_over_every_key_case(),
    ]
}
