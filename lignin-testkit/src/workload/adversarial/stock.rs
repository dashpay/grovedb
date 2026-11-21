//! One populated tree of every type, for the workloads that relocate and
//! drop whole trees.

use grovedb::{Element, TreeType};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

use crate::workload::{
    build::{self, batch, child, sum, value},
    op::{AppendEntry, Op},
    rng::Rng,
    zoo::{commitment_entry, TreeKind},
    Features,
};

const BULK_CHUNK_POWER: u8 = 2;
const DENSE_HEIGHT: u8 = 3;
const PRIVATE_DOCUMENT_ENTRY_SIZE: u32 = 16;

/// A tree [`stock`] stored, holding values and no child tree.
#[derive(Clone, Debug)]
pub(super) struct Stocked {
    /// The key the tree is at.
    pub(super) key: Vec<u8>,
    /// The type of the tree.
    pub(super) tree_type: TreeType,
    /// The keys stored in the tree, when it is a Merk tree.
    pub(super) rows: Vec<Vec<u8>>,
}

/// An append-only tree type.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AppendOnly {
    Mmr,
    Bulk,
    Dense,
    Commitment,
    PrivateDocuments,
}

impl AppendOnly {
    const ALL: [AppendOnly; 5] = [
        AppendOnly::Mmr,
        AppendOnly::Bulk,
        AppendOnly::Dense,
        AppendOnly::Commitment,
        AppendOnly::PrivateDocuments,
    ];

    fn name(self) -> &'static str {
        match self {
            AppendOnly::Mmr => "mmr",
            AppendOnly::Bulk => "bulk",
            AppendOnly::Dense => "dense",
            AppendOnly::Commitment => "commitment",
            AppendOnly::PrivateDocuments => "private-documents",
        }
    }

    fn element(self) -> Element {
        match self {
            AppendOnly::Mmr => Element::empty_mmr_tree(),
            AppendOnly::Bulk => Element::empty_bulk_append_tree(BULK_CHUNK_POWER)
                .expect("the chunk power is in range"),
            AppendOnly::Dense => Element::empty_dense_tree(DENSE_HEIGHT),
            AppendOnly::Commitment => Element::empty_commitment_tree(BULK_CHUNK_POWER)
                .expect("the chunk power is in range"),
            AppendOnly::PrivateDocuments => {
                Element::empty_private_document_store(PRIVATE_DOCUMENT_ENTRY_SIZE, 1)
                    .expect("the store configuration is in range")
            }
        }
    }

    /// One append to the tree at `tree`.
    fn append(self, tree: &[Vec<u8>], rng: &mut Rng) -> Op {
        match self {
            AppendOnly::Mmr => build::append(tree, AppendEntry::Mmr(value(rng, 1, 60))),
            AppendOnly::Bulk => build::append(tree, AppendEntry::Bulk(value(rng, 1, 60))),
            AppendOnly::Dense => build::append(tree, AppendEntry::Dense(value(rng, 1, 40))),
            AppendOnly::Commitment => Op::Batch(vec![batch::append(tree, commitment_entry(rng))]),
            AppendOnly::PrivateDocuments => build::append(
                tree,
                AppendEntry::PrivateDocument(rng.bytes(PRIVATE_DOCUMENT_ENTRY_SIZE)),
            ),
        }
    }
}

/// The elements a Merk tree of `kind` is filled with.
fn filler(kind: TreeKind, rng: &mut Rng) -> Element {
    if kind == TreeKind::CountSumIndexed {
        Element::new_item_with_sum_item(value(rng, 1, 40), sum(rng))
    } else if kind.sums() || kind == TreeKind::SumIndexed {
        Element::new_sum_item(sum(rng))
    } else {
        Element::new_item(value(rng, 1, 40))
    }
}

/// Stores under `parent` one tree of every type the features allow, each
/// with a few values, and returns them. Keys are `prefix` followed by the
/// name of the type.
pub(super) fn stock(
    parent: &[Vec<u8>],
    prefix: &str,
    features: Features,
    rng: &mut Rng,
    ops: &mut Vec<Op>,
) -> Vec<Stocked> {
    let mut stocked = Vec::new();
    let mut trees = Vec::new();
    let mut fills = Vec::new();
    for kind in TreeKind::ALL {
        if !kind.supported(features) {
            continue;
        }
        let key = format!("{prefix}{}", kind.name()).into_bytes();
        let tree = child(parent, &key);
        let rows: Vec<Vec<u8>> = (0..3u8).map(|row| vec![b'r', b'0' + row]).collect();
        trees.push(batch::insert_only(parent, &key, kind.element()));
        fills.push(Op::Batch(
            rows.iter()
                .map(|row| batch::insert_or_replace(&tree, row, filler(kind, rng)))
                .collect(),
        ));
        stocked.push(Stocked {
            key,
            tree_type: kind.element().tree_type().expect("a kind is a tree"),
            rows,
        });
    }
    for kind in AppendOnly::ALL {
        if kind == AppendOnly::PrivateDocuments && !features.private_document_store {
            continue;
        }
        let key = format!("{prefix}{}", kind.name()).into_bytes();
        let tree = child(parent, &key);
        for _ in 0..3 {
            fills.push(kind.append(&tree, rng));
        }
        let element = kind.element();
        stocked.push(Stocked {
            key: key.clone(),
            tree_type: element.tree_type().expect("an append-only tree is a tree"),
            rows: Vec::new(),
        });
        trees.push(batch::insert_only(parent, &key, element));
    }
    ops.push(Op::Batch(trees));
    ops.push(Op::Commit);
    ops.extend(fills);
    ops.push(Op::Commit);
    stocked
}
