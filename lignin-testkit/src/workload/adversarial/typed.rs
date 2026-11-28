//! The append-only trees read through their own methods.

use grovedb::Element;

use super::DOMAIN_TYPED;
use crate::workload::{
    build::{self, child, path, tree_read, value},
    op::{AppendEntry, Op, Prove, TreeRead, Write},
    rng::Rng,
    zoo::commitment_entry,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"typed";
const MMR: &[u8] = b"mmr";
const BULK: &[u8] = b"bulk";
const DENSE: &[u8] = b"dense";
const COMMITMENTS: &[u8] = b"commitments";
const PRIVATE_DOCUMENTS: &[u8] = b"private-documents";

/// A chunk of the bulk tree, of the commitment tree and of the store holds
/// `2^CHUNK_POWER` values.
const CHUNK_POWER: u8 = 2;
const DENSE_HEIGHT: u8 = 4;
/// A dense tree of height `h` holds `2^h - 1` values.
const DENSE_CAPACITY: u64 = (1 << DENSE_HEIGHT) - 1;
const PRIVATE_DOCUMENT_ENTRY_SIZE: u32 = 24;

/// Every typed read of the append-only trees.
///
/// An `MmrTree`, a `BulkAppendTree`, a `DenseAppendOnlyFixedSizeTree`, a
/// `CommitmentTree` and a `PrivateDocumentStore` are appended to round by
/// round: the first three and the store through their direct methods, the
/// commitment tree through `commitment_tree_insert`, which takes a typed
/// note ciphertext where a batch takes bytes. After each block every tree
/// is read through each of its methods: a value that is stored, the newest
/// one, a position past the end, a page that crosses a chunk boundary, a
/// chunk that is complete and one that is not, the buffer, the counts and
/// the root hashes. Where the grove version proves them, a page of the
/// bulk tree, of the commitment tree and of the store is proven by
/// `prove_bulk_position_range`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypedReads {
    /// How many rounds to run.
    pub rounds: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for TypedReads {
    fn default() -> Self {
        Self {
            rounds: 6,
            features: Features::ALL,
        }
    }
}

impl TypedReads {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        let mut rng = Rng::derive(seed, DOMAIN_TYPED, 0);
        let root = path(&[ROOT]);
        let stores = self.features.private_document_store;
        let mut trees = vec![
            (MMR, Element::empty_mmr_tree()),
            (
                BULK,
                Element::empty_bulk_append_tree(CHUNK_POWER).expect("the chunk power is in range"),
            ),
            (DENSE, Element::empty_dense_tree(DENSE_HEIGHT)),
            (
                COMMITMENTS,
                Element::empty_commitment_tree(CHUNK_POWER).expect("the chunk power is in range"),
            ),
        ];
        if stores {
            trees.push((
                PRIVATE_DOCUMENTS,
                Element::empty_private_document_store(PRIVATE_DOCUMENT_ENTRY_SIZE, CHUNK_POWER)
                    .expect("the store configuration is in range"),
            ));
        }
        let mut ops = vec![build::insert(&[], ROOT, Element::empty_tree())];
        for (key, element) in trees {
            ops.push(build::insert(&root, key, element));
        }
        ops.push(Op::Commit);

        let tree = |key: &[u8]| child(&root, key);
        let mut appended = 0u64;
        let mut dense = 0u64;
        for round in 0..self.rounds {
            let appends = rng.between(2, 5);
            for _ in 0..appends {
                ops.push(build::append(
                    &tree(MMR),
                    AppendEntry::Mmr(value(&mut rng, 1, 60)),
                ));
                ops.push(build::append(
                    &tree(BULK),
                    AppendEntry::Bulk(value(&mut rng, 1, 60)),
                ));
                let AppendEntry::Commitment {
                    cmx,
                    rho,
                    cv_net,
                    payload,
                } = commitment_entry(&mut rng)
                else {
                    unreachable!("a commitment entry is a commitment");
                };
                ops.push(Op::Write(Write::CommitmentInsert {
                    path: tree(COMMITMENTS),
                    cmx,
                    rho,
                    cv_net,
                    payload,
                }));
                if stores {
                    ops.push(build::append(
                        &tree(PRIVATE_DOCUMENTS),
                        AppendEntry::PrivateDocument(rng.bytes(PRIVATE_DOCUMENT_ENTRY_SIZE)),
                    ));
                }
            }
            appended += appends;
            if dense < DENSE_CAPACITY {
                ops.push(build::append(
                    &tree(DENSE),
                    AppendEntry::Dense(value(&mut rng, 1, 40)),
                ));
                dense += 1;
            }
            ops.push(Op::Commit);

            // A position that holds a value, the newest one and one past
            // the end.
            let stored = rng.below(appended);
            let newest = appended - 1;
            let page_start = rng.below(appended);
            let page_limit = rng.between(1, 2 * (1 << CHUNK_POWER) + 1) as u16;
            let chunks = appended >> CHUNK_POWER;

            let mmr = tree(MMR);
            for read in [
                TreeRead::MmrValue(stored),
                TreeRead::MmrValue(newest),
                TreeRead::MmrValue(appended + u64::from(round)),
                TreeRead::MmrRootHash,
                TreeRead::MmrLeafCount,
            ] {
                ops.push(tree_read(&mmr, read));
            }

            let bulk = tree(BULK);
            for read in [
                TreeRead::BulkValue(stored),
                TreeRead::BulkValue(newest),
                TreeRead::BulkValue(appended),
                TreeRead::BulkRange {
                    start: page_start,
                    limit: page_limit,
                },
                TreeRead::BulkChunk(chunks.saturating_sub(1)),
                TreeRead::BulkChunk(chunks),
                TreeRead::BulkBuffer,
                TreeRead::BulkCount,
                TreeRead::BulkChunkCount,
            ] {
                ops.push(tree_read(&bulk, read));
            }

            let dense_tree = tree(DENSE);
            for read in [
                TreeRead::DenseValue(rng.below(dense) as u16),
                TreeRead::DenseValue(dense as u16),
                TreeRead::DenseRootHash,
                TreeRead::DenseCount,
            ] {
                ops.push(tree_read(&dense_tree, read));
            }

            let commitments = tree(COMMITMENTS);
            for read in [
                TreeRead::CommitmentValue(stored),
                TreeRead::CommitmentValue(appended + 1),
                TreeRead::CommitmentRange {
                    start: page_start,
                    limit: page_limit,
                },
                TreeRead::CommitmentAnchor,
                TreeRead::CommitmentCount,
            ] {
                ops.push(tree_read(&commitments, read));
            }

            let mut proven = vec![BULK, COMMITMENTS];
            if stores {
                let store = tree(PRIVATE_DOCUMENTS);
                ops.push(tree_read(&store, TreeRead::PrivateDocumentValue(stored)));
                ops.push(tree_read(&store, TreeRead::PrivateDocumentValue(appended)));
                ops.push(tree_read(&store, TreeRead::PrivateDocumentCount));
                if self.features.private_document_ranges {
                    ops.push(tree_read(
                        &store,
                        TreeRead::PrivateDocumentRange {
                            start: page_start,
                            limit: page_limit,
                        },
                    ));
                    proven.push(PRIVATE_DOCUMENTS);
                }
            }
            if self.features.v1_proofs {
                for key in proven {
                    // A page inside the tree, and one that runs past its
                    // end.
                    for start in [page_start, newest] {
                        ops.push(build::prove(Prove::BulkPositionRange {
                            path: root.clone(),
                            key: key.to_vec(),
                            start,
                            limit: page_limit,
                        }));
                    }
                }
            }
        }
        Prepared::stream(ops)
    }
}
