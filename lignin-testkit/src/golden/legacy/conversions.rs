//! The cases for `GroveDb::convert_to_big_sum_tree`: every way it refuses a
//! conversion, next to the conversion it accepts once the reason is gone.
//!
//! A generated stream holds no refused operation, so these are the steps
//! whose recorded result is the error. Grove versions 1 to 4 have no such
//! operation and refuse every conversion here.

use grovedb::{
    element::reference_path::ReferencePathType, Element, PathQuery, Query as GroveQuery,
};
use grovedb_merk::proofs::query::IndexAxis;

use super::{case, get, insert, item, path, proven, Case, Op, Write, TOP};
use crate::workload::IndexedTree;

const SUMS: &[u8] = b"sums";
const COUNTED: &[u8] = b"counted";
const RANKED: &[u8] = b"ranked";
const WIDE: &[u8] = b"wide";
const POT: &[u8] = b"pot";

/// The conversion of the tree at `key`, in the transaction of the block
/// or, with `alone`, in one GroveDB opens for it.
fn convert_in(at: &[&[u8]], key: &[u8], alone: bool) -> Op {
    Op::Write(Write::ConvertToBigSumTree {
        path: path(at),
        key: key.to_vec(),
        own_transaction: alone,
    })
}

fn convert(at: &[&[u8]], key: &[u8]) -> Op {
    convert_in(at, key, false)
}

fn sum_item(sum: i64) -> Element {
    Element::new_sum_item(sum)
}

fn delete(at: &[&[u8]], key: &[u8]) -> Op {
    Op::Write(Write::Delete {
        path: path(at),
        key: key.to_vec(),
    })
}

/// An entry written through the methods of an indexed tree.
fn entry(tree: &[&[u8]], key: &[u8], element: Element, methods: IndexedTree) -> Op {
    Op::Write(Write::IndexedInsert {
        path: path(tree),
        key: key.to_vec(),
        element,
        tree: methods,
    })
}

fn remove_entry(tree: &[&[u8]], key: &[u8], methods: IndexedTree) -> Op {
    Op::Write(Write::IndexedDelete {
        path: path(tree),
        key: key.to_vec(),
        tree: methods,
    })
}

/// Everything the tree at `at` holds, proven.
fn everything(at: &[&[u8]]) -> Op {
    proven(PathQuery::new_unsized(
        path(at),
        GroveQuery::new_range_full(),
    ))
}

/// The first entries of the tree at `at` in the order of `axis`.
fn ranking(at: &[&[u8]], axis: IndexAxis) -> Op {
    proven(PathQuery::new_axis_top_k(path(at), axis, 8, 0, true))
}

/// A tree that ranks its entries by the axes named.
fn ranked_by(axes: &[IndexAxis]) -> Element {
    Element::ProvableCountProvableSumIndexedTree(
        None,
        0,
        0,
        axes.iter().map(|axis| (axis.tag(), None)).collect(),
        None,
    )
}

/// The seven trees a conversion applies to, each with the methods that
/// write its entries where it ranks them.
fn convertible() -> [(&'static [u8], Element, Option<IndexedTree>); 7] {
    [
        (b"sum", Element::empty_sum_tree(), None),
        (b"count-sum", Element::empty_count_sum_tree(), None),
        (b"provable-sum", Element::empty_provable_sum_tree(), None),
        (
            b"provable-count-sum",
            Element::empty_provable_count_sum_tree(),
            None,
        ),
        (
            b"provable-count-provable-sum",
            Element::empty_provable_count_provable_sum_tree(),
            None,
        ),
        (
            b"sum-indexed",
            Element::empty_provable_sum_indexed_tree(),
            Some(IndexedTree::Sum),
        ),
        (
            b"count-sum-indexed",
            ranked_by(&[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]),
            Some(IndexedTree::CountSum),
        ),
    ]
}

/// Each of the seven trees converted, and then asked to convert again: in
/// the block that converted it, in a later block, and given no transaction.
fn twice_case() -> Case {
    let mut ops = vec![insert(&[], TOP, Element::empty_tree())];
    for (name, tree, methods) in convertible() {
        ops.push(insert(&[TOP], name, tree));
        for (key, sum) in [(b"a", 7), (b"b", -3)] {
            let amount = Element::new_item_with_sum_item(vec![b'v'], sum);
            ops.push(match methods {
                Some(methods) => entry(&[TOP, name], key, amount, methods),
                None => insert(&[TOP, name], key, amount),
            });
        }
    }
    ops.push(Op::Commit);
    for (index, (name, ..)) in convertible().into_iter().enumerate() {
        ops.push(convert(&[TOP], name));
        if index % 2 == 0 {
            ops.push(convert(&[TOP], name));
        }
    }
    ops.push(Op::Commit);
    ops.push(everything(&[TOP]));
    for (name, ..) in convertible() {
        ops.push(convert(&[TOP], name));
    }
    ops.push(Op::Commit);
    for (name, ..) in convertible() {
        ops.push(convert_in(&[TOP], name, true));
    }
    for (name, ..) in convertible() {
        ops.push(get(&[TOP], name));
    }
    ops.push(everything(&[TOP]));
    case(
        "convert-twice",
        "Each of the seven trees that keep a 64-bit sum is converted to the tree that keeps \
         it in 128 bits, and then asked to convert again: in the block that converted it, in \
         a later block, and given no transaction. Grove version 5 converts each once and \
         refuses every further conversion, as the tree has no counterpart any more. Grove \
         versions 1 to 4 have no such operation and refuse them all.",
        ops,
    )
}

/// What is no tree with a 64-bit sum: the trees without a sum, the trees
/// whose sum has 128 bits already, and what is no tree.
fn no_counterpart_case() -> Case {
    let unconvertible: [(&[u8], Element); 12] = [
        (b"tree", Element::empty_tree()),
        (b"count", Element::empty_count_tree()),
        (b"provable-count", Element::empty_provable_count_tree()),
        (
            b"count-indexed",
            Element::empty_provable_count_indexed_tree(),
        ),
        (b"big-sum", Element::empty_big_sum_tree()),
        (b"count-big-sum", Element::empty_count_big_sum_tree()),
        (b"provable-big-sum", Element::empty_provable_big_sum_tree()),
        (
            b"provable-count-provable-big-sum",
            Element::empty_provable_count_provable_big_sum_tree(),
        ),
        (
            b"big-sum-indexed",
            Element::empty_provable_big_sum_indexed_tree(),
        ),
        (
            b"count-big-sum-indexed",
            Element::ProvableCountProvableBigSumIndexedTree(
                None,
                0,
                0,
                [IndexAxis::Count, IndexAxis::Avg, IndexAxis::BigSum]
                    .map(|axis| (axis.tag(), None))
                    .to_vec(),
                None,
            ),
        ),
        (b"item", item(b"no tree")),
        (
            b"reference",
            Element::new_reference(ReferencePathType::SiblingReference(b"item".to_vec())),
        ),
    ];
    let mut ops = vec![
        insert(&[], TOP, Element::empty_tree()),
        insert(&[TOP], SUMS, Element::empty_sum_tree()),
        insert(&[TOP, SUMS], b"a", sum_item(4)),
    ];
    for (name, element) in &unconvertible {
        ops.push(insert(&[TOP], name, element.clone()));
    }
    ops.push(Op::Commit);
    // Half of them in the block, with the sum item, and half given no
    // transaction.
    let (in_block, alone): (Vec<_>, Vec<_>) = unconvertible
        .iter()
        .enumerate()
        .partition(|(index, _)| index % 2 == 0);
    for (_, (name, _)) in in_block {
        ops.push(convert(&[TOP], name));
    }
    ops.extend([convert(&[TOP, SUMS], b"a"), Op::Commit]);
    for (_, (name, _)) in alone {
        ops.push(convert_in(&[TOP], name, true));
    }
    ops.extend([convert_in(&[TOP, SUMS], b"a", true), get(&[TOP], SUMS)]);
    case(
        "convert-without-a-counterpart",
        "A conversion is asked of what is no tree with a 64-bit sum: the four trees that \
         keep no sum, the six whose sum has 128 bits as they are created, an item, a \
         reference to an item, and a sum item in a sum tree. Every grove version refuses \
         each, grove version 5 for what the element is and grove versions 1 to 4 because \
         they have no such operation; they also refuse to create the trees they do not \
         know.",
        ops,
    )
}

/// A conversion at a place that holds no tree to convert.
fn nothing_there_case() -> Case {
    let rooted: &[u8] = b"rooted";
    let ops = vec![
        insert(&[], TOP, Element::empty_tree()),
        insert(&[TOP], SUMS, Element::empty_sum_tree()),
        insert(&[TOP, SUMS], b"a", sum_item(5)),
        insert(&[TOP], b"item", item(b"no tree")),
        insert(&[], rooted, Element::empty_sum_tree()),
        insert(&[rooted], b"a", sum_item(-5)),
        Op::Commit,
        // No such key, in a tree and in the root tree.
        convert(&[TOP], b"absent"),
        convert(&[], b"absent"),
        // No such subtree to look in.
        convert(&[TOP, b"absent"], SUMS),
        convert(&[b"absent"], SUMS),
        // The path runs through an item, and through a sum item.
        convert(&[TOP, b"item"], SUMS),
        convert(&[TOP, SUMS, b"a"], SUMS),
        // A key and a path segment longer than a key can be.
        convert(&[TOP], &[b'k'; 256]),
        convert(&[TOP, &[b'k'; 256]], SUMS),
        Op::Commit,
        convert_in(&[TOP], b"absent", true),
        convert_in(&[TOP, b"absent"], SUMS, true),
        get(&[TOP], SUMS),
        // The trees that are there convert, the one in the root tree too.
        convert(&[], rooted),
        Op::Commit,
        convert_in(&[TOP], SUMS, true),
        get(&[], rooted),
        get(&[TOP], SUMS),
        everything(&[TOP]),
    ];
    case(
        "convert-what-is-not-there",
        "A conversion is asked at a key that is not stored, in a subtree that does not \
         exist, through an item and through a sum item, and at a key and under a path \
         segment of 256 bytes, in the block and given no transaction. Every grove version \
         refuses each. The two trees that are there, one of them in the root tree, are then \
         converted by grove version 5.",
        ops,
    )
}

/// A tree whose parent adds its sum to a 64-bit sum is converted after the
/// parent.
fn before_its_parent_case() -> Case {
    let sums: &[&[u8]] = &[TOP, SUMS];
    let ranked: &[&[u8]] = &[TOP, RANKED];
    let left_out =
        Element::new_not_summed(Element::empty_sum_tree()).expect("a sum tree can be left out");
    let children: [&[u8]; 2] = [b"empty", b"filled"];
    let mut ops = vec![
        insert(&[], TOP, Element::empty_tree()),
        insert(&[TOP], SUMS, Element::empty_sum_tree()),
        insert(sums, b"a", sum_item(1)),
        insert(sums, b"empty", Element::empty_sum_tree()),
        insert(sums, b"filled", Element::empty_sum_tree()),
        insert(&[TOP, SUMS, b"filled"], b"a", sum_item(9)),
        insert(sums, b"left-out", left_out),
        insert(&[TOP, SUMS, b"left-out"], b"a", sum_item(70)),
        insert(&[TOP], RANKED, Element::empty_provable_sum_indexed_tree()),
        entry(ranked, b"a", sum_item(-40), IndexedTree::Sum),
        entry(ranked, POT, Element::empty_sum_tree(), IndexedTree::Sum),
        insert(&[TOP, RANKED, POT], b"a", sum_item(10)),
        Op::Commit,
    ];
    for child in children {
        ops.push(convert(sums, child));
    }
    ops.extend([
        convert(ranked, POT),
        Op::Commit,
        convert_in(sums, b"filled", true),
        convert_in(ranked, POT, true),
        get(&[TOP], SUMS),
        // A child the parent leaves out of its sum adds nothing either
        // way.
        convert(sums, b"left-out"),
        Op::Commit,
        get(sums, b"left-out"),
        // The parents, and then their children.
        convert(&[TOP], SUMS),
        convert(&[TOP], RANKED),
        Op::Commit,
        get(&[TOP], SUMS),
        get(&[TOP], RANKED),
        ranking(ranked, IndexAxis::BigSum),
    ]);
    for child in children {
        ops.push(convert(sums, child));
    }
    ops.extend([
        convert(ranked, POT),
        insert(&[TOP, SUMS, b"filled"], b"b", sum_item(i64::MAX)),
        insert(&[TOP, RANKED, POT], b"b", sum_item(i64::MAX)),
        insert(&[TOP, RANKED, POT], b"c", sum_item(i64::MAX)),
        Op::Commit,
        get(&[TOP], SUMS),
        get(&[TOP], RANKED),
        get(ranked, POT),
        everything(sums),
        ranking(ranked, IndexAxis::BigSum),
    ]);
    case(
        "convert-before-its-parent",
        "A tree is asked to convert while the tree it is in adds its sum to a 64-bit sum: \
         an empty and a filled sum tree inside a sum tree, and a sum tree that is an entry \
         of a tree ranking by a 64-bit sum. Grove version 5 refuses each, as a tree with a \
         128-bit sum adds nothing to such a parent, converts the child its parent leaves \
         out of the sum, and converts the others once their parents are converted; the \
         sums then pass an `i64`, in the trees and in the ranking. Grove versions 1 to 4 \
         refuse every conversion.",
        ops,
    )
}

/// A tree that holds a tree with a 128-bit sum, which would add its sum to
/// the converted tree and adds nothing today.
fn wide_child_case() -> Case {
    let mut ops = vec![insert(&[], TOP, Element::empty_tree())];
    let trees: [(&[u8], Element, Element); 2] = [
        (
            SUMS,
            Element::empty_sum_tree(),
            Element::empty_big_sum_tree(),
        ),
        (
            COUNTED,
            Element::empty_count_sum_tree(),
            Element::empty_count_big_sum_tree(),
        ),
    ];
    for (name, tree, wide) in trees.clone() {
        ops.extend([
            insert(&[TOP], name, tree),
            insert(&[TOP, name], b"a", sum_item(4)),
            insert(&[TOP, name], WIDE, wide),
        ]);
        for number in 0..3u8 {
            ops.push(insert(&[TOP, name, WIDE], &[number], sum_item(i64::MAX)));
        }
    }
    ops.push(Op::Commit);
    for (name, ..) in trees.clone() {
        ops.push(convert(&[TOP], name));
    }
    ops.extend([
        Op::Commit,
        convert_in(&[TOP], SUMS, true),
        get(&[TOP], SUMS),
        get(&[TOP], COUNTED),
        // Without that child the tree converts.
        delete(&[TOP, SUMS], WIDE),
        convert(&[TOP], SUMS),
        Op::Commit,
        get(&[TOP], SUMS),
        everything(&[TOP]),
    ]);
    case(
        "convert-with-a-wide-child",
        "A sum tree and a count sum tree hold a tree with a 128-bit sum, which adds nothing \
         to a 64-bit sum and its whole sum to a 128-bit one. Grove version 5 refuses to \
         convert them, as their sums would change, and converts the first once that child \
         is deleted. Grove versions 1 to 4 refuse every conversion, and do not create the \
         second of the two children.",
        ops,
    )
}

/// A tree inserted with a sum and no children: its element says another
/// sum than its tree has.
fn claimed_sum_case() -> Case {
    let trees: [(&[u8], Element); 2] = [
        (SUMS, Element::SumTree(None, 9, None)),
        (COUNTED, Element::CountSumTree(None, 2, 9, None)),
    ];
    let mut ops = vec![insert(&[], TOP, Element::empty_tree())];
    for (name, tree) in trees.clone() {
        ops.push(insert(&[TOP], name, tree));
    }
    ops.push(Op::Commit);
    for (name, _) in trees.clone() {
        ops.push(convert(&[TOP], name));
    }
    ops.push(Op::Commit);
    for (name, _) in trees.clone() {
        ops.extend([convert_in(&[TOP], name, true), get(&[TOP], name)]);
    }
    for (name, _) in trees.clone() {
        ops.push(insert(&[TOP, name], b"a", sum_item(4)));
    }
    ops.extend([convert(&[TOP], SUMS), Op::Commit]);
    ops.extend([convert_in(&[TOP], COUNTED, true), everything(&[TOP])]);
    case(
        "convert-claimed-sum",
        "A sum tree and a count sum tree are inserted with a sum and no children, so their \
         elements name a sum their trees do not have. Grove version 5 refuses to convert \
         them, in the block and given no transaction, and converts each once a write has \
         brought its element in line with its tree. Grove versions 1 to 4 refuse every \
         conversion.",
        ops,
    )
}

/// An indexed tree with a key the axis of 128-bit sums cannot rank.
fn long_key_case() -> Case {
    let by_sum: &[u8] = b"by-sum";
    let by_count_and_sum: &[u8] = b"by-count-and-sum";
    let by_count: &[u8] = b"by-count";
    let amount = |sum| Element::new_item_with_sum_item(vec![b'v'], sum);
    let trees: [(&[u8], Element, IndexedTree, usize); 3] = [
        (
            by_sum,
            Element::empty_provable_sum_indexed_tree(),
            IndexedTree::Sum,
            240,
        ),
        (
            by_count_and_sum,
            ranked_by(&[IndexAxis::Count, IndexAxis::Sum]),
            IndexedTree::CountSum,
            240,
        ),
        (
            by_count,
            ranked_by(&[IndexAxis::Count]),
            IndexedTree::CountSum,
            247,
        ),
    ];
    let mut ops = vec![insert(&[], TOP, Element::empty_tree())];
    for (name, tree, methods, longest) in trees.clone() {
        ops.push(insert(&[TOP], name, tree));
        for (len, sum) in [(239, 1), (longest, -2)] {
            ops.push(entry(&[TOP, name], &vec![b'k'; len], amount(sum), methods));
        }
    }
    ops.push(Op::Commit);
    for (name, ..) in trees.clone() {
        ops.push(convert(&[TOP], name));
    }
    ops.push(Op::Commit);
    for (name, ..) in trees.clone() {
        ops.push(get(&[TOP], name));
    }
    // Without the long key the two that were refused convert.
    for (name, _, methods, longest) in trees.clone().into_iter().take(2) {
        ops.extend([
            remove_entry(&[TOP, name], &vec![b'k'; longest], methods),
            convert(&[TOP], name),
        ]);
    }
    ops.push(Op::Commit);
    for (name, ..) in trees.clone() {
        ops.push(get(&[TOP], name));
    }
    ops.extend([
        ranking(&[TOP, by_sum], IndexAxis::BigSum),
        ranking(&[TOP, by_count_and_sum], IndexAxis::BigSum),
        ranking(&[TOP, by_count], IndexAxis::Count),
    ]);
    case(
        "convert-key-too-long-to-rank",
        "Three indexed trees hold a long key: 240 bytes in a tree ranking by sum and in one \
         ranking by count and sum, 247 bytes in one ranking by count alone. The axis of \
         64-bit sums ranks keys of up to 247 bytes and the axis of 128-bit sums keys of up \
         to 239. Grove version 5 refuses to convert the first two, converts the third, \
         which gains no such axis, and converts the first two once the key is removed. \
         Grove versions 1 to 4 refuse every conversion.",
        ops,
    )
}

/// Indexed trees whose rows would pass through a sum no `i64` holds if a
/// conversion removed them one by one.
fn both_signs_case() -> Case {
    const LARGE: i64 = 1 << 61;
    let by_sum: &[&[u8]] = &[TOP, b"by-sum"];
    let by_average: &[&[u8]] = &[TOP, b"by-average"];
    let mut ops = vec![
        insert(&[], TOP, Element::empty_tree()),
        insert(
            &[TOP],
            by_sum[1],
            Element::empty_provable_sum_indexed_tree(),
        ),
        insert(
            &[TOP],
            by_average[1],
            ranked_by(&[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]),
        ),
    ];
    for (key, sum) in [
        (b"e", -2 * LARGE),
        (b"a", LARGE),
        (b"b", LARGE),
        (b"c", LARGE),
        (b"d", LARGE),
    ] {
        ops.push(entry(by_sum, key, sum_item(sum), IndexedTree::Sum));
    }
    for (key, sum) in [(b"n", -1), (b"p", 1), (b"x", i64::MAX)] {
        let amount = Element::new_item_with_sum_item(vec![b'v'], sum);
        ops.push(entry(by_average, key, amount, IndexedTree::CountSum));
    }
    ops.extend([
        Op::Commit,
        ranking(by_sum, IndexAxis::Sum),
        ranking(by_average, IndexAxis::Sum),
        ranking(by_average, IndexAxis::Avg),
        convert(&[TOP], by_sum[1]),
        Op::Commit,
        convert_in(&[TOP], by_average[1], true),
        get(&[TOP], by_sum[1]),
        get(&[TOP], by_average[1]),
        ranking(by_sum, IndexAxis::BigSum),
        ranking(by_average, IndexAxis::BigSum),
        ranking(by_average, IndexAxis::Avg),
        ranking(by_average, IndexAxis::Count),
        // The converted trees rank an entry past an `i64` at either end.
        entry(by_sum, b"f", sum_item(-4 * LARGE), IndexedTree::BigSum),
        entry(by_sum, b"g", sum_item(-4 * LARGE), IndexedTree::BigSum),
        entry(by_sum, b"h", sum_item(-4 * LARGE), IndexedTree::BigSum),
        entry(
            by_average,
            b"y",
            Element::new_item_with_sum_item(vec![b'v'], i64::MAX),
            IndexedTree::CountBigSum,
        ),
        Op::Commit,
        get(&[TOP], by_sum[1]),
        get(&[TOP], by_average[1]),
        ranking(by_sum, IndexAxis::BigSum),
        ranking(by_average, IndexAxis::BigSum),
    ]);
    case(
        "convert-rows-of-both-signs",
        "A tree ranking by sum holds one large debit and four credits that together are \
         more than an `i64` holds, and a tree ranking by count, sum and average holds a \
         debit of one, a credit of one and the largest `i64`. A conversion writes the rows \
         of the sums again under longer keys; were the old rows removed one at a time, the \
         rest would pass through a total no `i64` holds. Grove version 5 converts both \
         trees with every ranking as it was, and the trees then take entries that carry \
         their totals past an `i64` at either end. Grove versions 1 to 4 refuse the \
         conversions, and grove versions 1 to 3 the rankings.",
        ops,
    )
}

/// An empty indexed tree as a row of a proven query, which the workload
/// that converts trees steers around: it reads the tree that holds its
/// trees, some of them empty indexed trees, without a proof where the grove
/// version does not prove such a row.
fn empty_indexed_row_case() -> Case {
    let hosts: [(&[u8], Element, Element, IndexedTree); 3] = [
        (
            b"by-count",
            Element::empty_provable_count_indexed_tree(),
            item(b"an entry"),
            IndexedTree::Count,
        ),
        (
            b"by-sum",
            Element::empty_provable_sum_indexed_tree(),
            sum_item(5),
            IndexedTree::Sum,
        ),
        (
            b"by-count-and-sum",
            ranked_by(&[IndexAxis::Count, IndexAxis::Sum]),
            Element::new_item_with_sum_item(vec![b'v'], 5),
            IndexedTree::CountSum,
        ),
    ];
    let mut ops = vec![insert(&[], TOP, Element::empty_tree())];
    for (host, tree, ..) in hosts.clone() {
        ops.extend([
            insert(&[TOP], host, Element::empty_tree()),
            insert(&[TOP, host], RANKED, tree),
            insert(&[TOP, host], b"beside", item(b"a row beside it")),
        ]);
    }
    ops.push(Op::Commit);
    for (host, ..) in hosts.clone() {
        ops.push(everything(&[TOP, host]));
    }
    for (host, _, element, methods) in hosts.clone() {
        ops.push(entry(&[TOP, host, RANKED], b"a", element, methods));
    }
    ops.push(Op::Commit);
    for (host, ..) in hosts {
        ops.push(everything(&[TOP, host]));
    }
    case(
        "empty-indexed-tree-row",
        "Three trees each hold an empty indexed tree and an item, and every row of each is \
         proven: before the indexed tree has an entry, and after. Grove versions 1 and 2 \
         build the proof of an empty indexed tree as a row with another value hash than \
         the verifier works out, so those proofs do not verify; with an entry in the tree \
         they do. From grove version 3 all of them verify.",
        ops,
    )
}

/// The cases, in the order they are recorded.
pub(super) fn cases() -> Vec<Case> {
    vec![
        twice_case(),
        no_counterpart_case(),
        nothing_there_case(),
        before_its_parent_case(),
        wide_child_case(),
        claimed_sum_case(),
        long_key_case(),
        both_signs_case(),
        empty_indexed_row_case(),
    ]
}
