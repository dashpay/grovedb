//! The cases for what grove version 5 answers differently from the
//! versions before it, and for two behaviours of the storage-flag
//! callbacks on older versions.

use std::collections::BTreeSet;

use grovedb::{Element, ElementFlags, PathQuery, Query as GroveQuery, QueryItem as Item};
use grovedb_merk::proofs::query::IndexAxis;

use super::{case, insert, insert_only, item, path, proven, sized, BatchOp, Case, Op, Read, TOP};
use crate::workload::{HandoverBatch, PartialBatch};

const INDEX: &[u8] = b"index";
const OTHER: &[u8] = b"other";
const FILLED: &[u8] = b"filled";

fn key(tag: u8, number: u8) -> Vec<u8> {
    vec![tag, b'0' + number]
}

/// Everything the tree at `at` holds, as stored.
fn everything(at: &[&[u8]]) -> Op {
    Op::Read(Read::Rows {
        query: Box::new(PathQuery::new_unsized(
            path(at),
            GroveQuery::new_range_full(),
        )),
        raw: true,
    })
}

fn replace(at: &[&[u8]], key: &[u8], element: Element) -> BatchOp {
    BatchOp::Replace {
        path: path(at),
        key: key.to_vec(),
        element,
    }
}

/// An index of six keys, the first of which holds no document, the others
/// two each, next to a tree of two items.
fn index() -> Vec<Op> {
    let mut ops = vec![
        insert(&[], TOP, Element::empty_tree()),
        insert(&[TOP], INDEX, Element::empty_tree()),
        insert(&[TOP], OTHER, Element::empty_tree()),
        insert(&[TOP, OTHER], b"a", item(b"a row of another tree")),
        insert(&[TOP, OTHER], b"b", item(b"one more")),
    ];
    for number in 0..6 {
        let in_index = key(b'k', number);
        ops.push(insert(&[TOP, INDEX], &in_index, Element::empty_tree()));
        let documents = if number == 0 { 0 } else { 2 };
        for document in 0..documents {
            ops.push(insert(
                &[TOP, INDEX, &in_index],
                &key(b'd', document),
                item(&[b'v', number, document]),
            ));
        }
    }
    ops.push(Op::Commit);
    ops
}

/// Every key of a tree and every document under each.
fn documents_of_every_key() -> GroveQuery {
    let mut keys = GroveQuery::new_range_full();
    keys.set_subquery(GroveQuery::new_range_full());
    keys
}

/// A page of three documents: the keys of the index in order, the
/// documents under each, and a limit of the node that walks the keys.
fn page(left_to_right: bool) -> GroveQuery {
    let mut keys =
        GroveQuery::new_single_query_item_with_direction(Item::RangeFull(..), left_to_right);
    keys.set_subquery(GroveQuery::new_range_full());
    keys.limit = Some(3);
    keys
}

/// Pages of an index under a limit of the query node that walks its keys.
fn limit_over_subqueries_case() -> Case {
    let at_index = |query, limit| sized(&[TOP, INDEX], query, limit, None);
    // The page as `PathQuery::merge` joins it with a query for two keys of
    // the other tree: each is the subquery of its tree's key.
    let mut others = GroveQuery::new();
    others.insert_key(b"a".to_vec());
    others.insert_key(b"z".to_vec());
    let mut merged = GroveQuery::new();
    for (tree, body) in [(OTHER, others), (INDEX, page(true))] {
        merged.insert_item(Item::Key(tree.to_vec()));
        merged.add_conditional_subquery(Item::Key(tree.to_vec()), None, Some(body));
    }
    // The limit of the node at the top is in force on the layer below it.
    let mut trees = GroveQuery::new_single_key(INDEX.to_vec());
    trees.set_subquery(documents_of_every_key());
    trees.limit = Some(3);

    let mut ops = index();
    ops.extend([
        proven(at_index(page(true), None)),
        proven(at_index(page(false), None)),
        proven(at_index(page(true), Some(2))),
        proven(PathQuery::new_unsized(path(&[TOP]), trees)),
        proven(PathQuery::new_unsized(path(&[TOP]), merged)),
    ]);
    case(
        "limit-over-subqueries",
        "A limit of a query node on a layer that has subqueries: a page of an index, from \
         either end, next to a limit of the whole query, under a node above it that has the \
         limit, and merged with a query for another tree. Grove versions 1 to 3 refuse the \
         limit. Grove version 4 answers and proves the layer of the index with every key in \
         range. From grove version 5 the proof ends the layer after the key whose documents \
         filled the page; the rows are the same.",
        ops,
    )
}

/// A query that descends into every key of the index but some, which it
/// takes as rows.
fn branch_without_subquery_case() -> Case {
    let mut keys = documents_of_every_key();
    keys.add_conditional_subquery(Item::RangeFrom(key(b'k', 3)..), None, None);
    let mut ops = index();
    ops.push(proven(PathQuery::new_unsized(path(&[TOP, INDEX]), keys)));
    case(
        "branch-without-subquery",
        "A query node with a subquery for its keys and a branch for some of them that has \
         none, so the trees at those keys are rows. GroveDb::run_path_query answers it. On \
         grove versions 1 to 4 the prover does not find the path of such a tree in the query \
         and refuses the proof. Grove version 5 proves it.",
        ops,
    )
}

/// A page of trees as rows, the first of which holds nothing.
fn capped_tree_rows_case() -> Case {
    let rows = |limit: Option<u16>| {
        let mut keys = GroveQuery::new_range_full();
        keys.add_conditional_subquery(
            Item::RangeFrom(key(b'k', 2)..),
            None,
            Some(GroveQuery::new_range_full()),
        );
        keys.limit = limit;
        proven(PathQuery::new_unsized(path(&[TOP, INDEX]), keys))
    };
    let mut ops = index();
    ops.extend([rows(None), rows(Some(3))]);
    case(
        "capped-tree-rows",
        "A query node that takes the trees at its first keys as rows and descends into the \
         others, without a limit and with one of three rows. The first tree holds nothing. \
         With the limit, the prover of grove version 4 counts that tree as a row and its \
         verifier does not, so the proof does not verify. Grove version 5 counts it on both \
         sides. Grove versions 1 to 3 refuse the limit.",
        ops,
    )
}

/// Two layers of keys above the documents, some keys without any, read
/// under a limit of the whole query.
fn limit_over_empty_subtrees_case() -> Case {
    let mut ops = vec![
        insert(&[], TOP, Element::empty_tree()),
        insert(&[TOP], INDEX, Element::empty_tree()),
    ];
    for (group, documents) in [(0, [2, 0, 2]), (1, [0, 2, 1])] {
        let group = key(b'g', group);
        ops.push(insert(&[TOP, INDEX], &group, Element::empty_tree()));
        for (number, documents) in documents.into_iter().enumerate() {
            let in_group = key(b'k', number as u8);
            ops.push(insert(
                &[TOP, INDEX, &group],
                &in_group,
                Element::empty_tree(),
            ));
            for document in 0..documents {
                ops.push(insert(
                    &[TOP, INDEX, &group, &in_group],
                    &key(b'd', document),
                    item(&[b'v', document]),
                ));
            }
        }
    }
    ops.push(Op::Commit);
    let page = |limit: u16| {
        let mut groups = GroveQuery::new_range_full();
        groups.set_subquery(documents_of_every_key());
        proven(sized(&[TOP, INDEX], groups, Some(limit), None))
    };
    ops.extend([page(4), page(6)]);
    case(
        "limit-over-empty-subtrees",
        "Two layers of keys above the documents, read under a limit of the whole query of \
         four rows and of six. Grove versions 1 to 3 charge a key without documents to the \
         limit in one way when they read and in another when they prove: the read charges \
         the layer above for the rows that came back from below, or for one row if none \
         did, and forgets what the walk below charged for its own empty keys. On grove \
         versions 1 and 2 the two proofs do not verify. On grove version 3 they do, the \
         second to other rows than the read returns. From grove version 4 the read counts \
         as the prover does.",
        ops,
    )
}

/// A batch that inserts `tree` under `parent`, a tree of `TOP`, and one
/// that inserts it at another key and writes `entry` into it, each
/// followed by what `parent` then holds.
fn filled_tree_case(
    name: &str,
    about: &str,
    parent: Element,
    tree: Element,
    entry: Element,
) -> Case {
    case(
        name,
        about,
        vec![
            insert(&[], TOP, Element::empty_tree()),
            Op::Batch(vec![insert_only(&[TOP], FILLED, parent)]),
            Op::Commit,
            Op::Batch(vec![insert_only(&[TOP, FILLED], b"alone", tree.clone())]),
            Op::Commit,
            everything(&[TOP, FILLED]),
            Op::Batch(vec![
                insert_only(&[TOP, FILLED], b"tree", tree),
                insert_only(&[TOP, FILLED, b"tree"], b"entry", entry),
            ]),
            Op::Commit,
            everything(&[TOP, FILLED]),
            proven(PathQuery::new_unsized(
                path(&[TOP, FILLED, b"tree"]),
                GroveQuery::new_range_full(),
            )),
        ],
    )
}

/// The trees a batch creates and fills where the insert alone is refused.
fn filled_tree_cases() -> Vec<Case> {
    let axes = |axes: [IndexAxis; 2]| axes.map(|axis| (axis.tag(), None)).to_vec();
    let from_v5 = "From grove version 5 the batch that also writes into the tree is refused as \
                   the insert alone is. Grove versions 1 to 4 replace the insert with the state \
                   the level below computed and never check the element.";
    vec![
        filled_tree_case(
            "filled-tree",
            "A batch that inserts a tree and writes into it, where the insert alone is \
             accepted: every grove version takes both.",
            Element::empty_tree(),
            Element::empty_sum_tree(),
            Element::new_sum_item(5),
        ),
        filled_tree_case(
            "filled-tree-claims-a-sum",
            &format!(
                "An indexed tree inserted with a sum it does not hold yet. The insert alone is \
                 refused: an indexed tree is created empty. {from_v5}"
            ),
            Element::empty_tree(),
            Element::ProvableSumIndexedTree(None, None, 7, None),
            Element::new_sum_item(5),
        ),
        filled_tree_case(
            "filled-tree-axes-out-of-order",
            &format!(
                "An indexed tree whose axes are not in the order of their tags. The insert \
                 alone is refused. {from_v5} They commit the axes in the order given."
            ),
            Element::empty_tree(),
            Element::ProvableCountProvableSumIndexedTree(
                None,
                0,
                0,
                axes([IndexAxis::Sum, IndexAxis::Count]),
                None,
            ),
            Element::new_item_with_sum_item(b"entry".to_vec(), 5),
        ),
        filled_tree_case(
            "filled-tree-wrapped",
            &format!(
                "A tree wrapped as not counted under a parent that keeps no count. The insert \
                 alone is refused: the wrapper needs a parent that counts. {from_v5}"
            ),
            Element::empty_sum_tree(),
            Element::NonCounted(Box::new(Element::empty_sum_tree())),
            Element::new_sum_item(5),
        ),
        filled_tree_case(
            "filled-tree-entry-of-indexed-tree",
            &format!(
                "A plain tree as an entry of an indexed tree that ranks by sum. The insert \
                 alone is refused: an entry carries what the tree ranks by. {from_v5}"
            ),
            Element::empty_provable_sum_indexed_tree(),
            Element::empty_tree(),
            item(b"entry"),
        ),
    ]
}

/// Storage flags as Drive writes them: of `owner` where there is one, and
/// of one epoch.
fn flags(owner: Option<u8>, epoch: u16) -> Option<ElementFlags> {
    let mut flags = match owner {
        Some(owner) => [&[2][..], &[owner; 32]].concat(),
        None => vec![0],
    };
    flags.extend_from_slice(&epoch.to_be_bytes());
    Some(flags)
}

/// An item of `len` bytes of value with the flags of `owner` and `epoch`.
fn flagged(len: usize, owner: u8, epoch: u16) -> Element {
    Element::new_item_with_flags(vec![b'x'; len], flags(Some(owner), epoch))
}

/// An update that stores flags of another length than the stored ones
/// without the flags callback changing them.
fn flag_length_case() -> Case {
    let at: &[&[u8]] = &[TOP];
    let batch = |op: BatchOp| Op::FlagsBatch(vec![op]);
    let partial = |op: BatchOp| {
        Op::FlagsPartialBatch(PartialBatch {
            ops: vec![op],
            add_on: Vec::new(),
            pause_height: None,
        })
    };
    case(
        "flags-of-another-length",
        "An update that stores flags of another length than the stored ones without the flags \
         callback changing them: an element without flags over one whose flags hold two \
         epochs, by a batch, and an update of the same size in a partial batch over flags of \
         two epochs, which stores the flags as written. Grove versions 1 to 3 charge the \
         update for the length of the stored flags, and the batch fails with a storage cost \
         mismatch. From grove version 4 it is applied.",
        vec![
            insert(&[], TOP, Element::empty_tree()),
            Op::Commit,
            batch(insert_only(at, b"bare", flagged(40, 1, 0))),
            Op::Commit,
            batch(replace(at, b"bare", flagged(180, 1, 1))),
            Op::Commit,
            everything(at),
            batch(replace(at, b"bare", item(&[b'y'; 200]))),
            Op::Commit,
            everything(at),
            partial(insert_only(at, b"past", flagged(40, 1, 2))),
            Op::Commit,
            partial(replace(at, b"past", flagged(180, 1, 3))),
            Op::Commit,
            everything(at),
            partial(replace(at, b"past", flagged(180, 1, 4))),
            Op::Commit,
            everything(at),
        ],
    )
}

/// What a sponsor paid for handed to its owner.
fn full_handover_case() -> Case {
    let at: &[&[u8]] = &[TOP];
    let (sponsor, owner) = (7, 9);
    let hand_over = |element: Element, full: bool| {
        let sponsors: BTreeSet<[u8; 32]> = full.then_some([sponsor; 32]).into_iter().collect();
        Op::FlagsHandoverBatch(HandoverBatch {
            ops: vec![replace(at, b"paid", element)],
            full_handover_owners: sponsors,
        })
    };
    case(
        "full-handover",
        "A document a gas sponsor paid for, over two epochs, handed to its owner by the \
         callback Drive passes with a document update. From grove version 5 the update is \
         accounted as a replacement: the sponsor is given back every byte, epoch by epoch, \
         and the owner is charged for the element as written. Grove versions 1 to 4 do not \
         take that answer from the callback and refuse the batch. The same update between \
         two owners of whom neither is a sponsor follows, which every grove version takes.",
        vec![
            insert(&[], TOP, Element::empty_tree()),
            Op::Commit,
            Op::FlagsBatch(vec![insert_only(at, b"paid", flagged(40, sponsor, 0))]),
            Op::Commit,
            Op::FlagsBatch(vec![replace(at, b"paid", flagged(180, sponsor, 1))]),
            Op::Commit,
            everything(at),
            hand_over(flagged(300, owner, 2), true),
            Op::Commit,
            everything(at),
            hand_over(flagged(420, sponsor, 3), false),
            Op::Commit,
            everything(at),
        ],
    )
}

/// The cases of this module, in the order they are documented.
pub(super) fn cases() -> Vec<Case> {
    let mut cases = vec![
        limit_over_subqueries_case(),
        capped_tree_rows_case(),
        limit_over_empty_subtrees_case(),
        branch_without_subquery_case(),
    ];
    cases.extend(filled_tree_cases());
    cases.extend([flag_length_case(), full_handover_case()]);
    cases
}
