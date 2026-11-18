//! Trees served in chunks.

use grovedb::Element;

use super::DOMAIN_CHUNKS;
use crate::workload::{
    build::{self, batch, child, path, prove, sum, value},
    op::{AppendEntry, ChunkIds, Op, Path, Prove},
    rng::Rng,
    zoo::commitment_entry,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"chunks";
const PLAIN: &[u8] = b"plain";
const NESTED: &[u8] = b"nested";
const SUMS: &[u8] = b"sums";
const COUNTED: &[u8] = b"counted";
const PROVEN: &[u8] = b"proven";
const MMR: &[u8] = b"mmr";
const BULK: &[u8] = b"bulk";
const DENSE: &[u8] = b"dense";
const COMMITMENTS: &[u8] = b"commitments";
const PRIVATE_DOCUMENTS: &[u8] = b"private-documents";

const CHUNK_POWER: u8 = 2;
const DENSE_HEIGHT: u8 = 5;
const PRIVATE_DOCUMENT_ENTRY_SIZE: u32 = 20;

/// The fewest rows a Merk tree of this workload holds. An AVL tree of that
/// many nodes has a node wherever two steps from its root lead, so a chunk
/// two steps down exists whatever the order the rows were written in.
pub const MIN_CHUNKED_ROWS: u32 = 24;

/// The proofs that carry whole parts of a tree.
///
/// Two families of them are served from committed state:
///
/// - **State sync chunks**, `fetch_chunk`: what a node sends another that
///   is restoring its state. A Merk tree is served chunk by chunk from its
///   root down: the root tree, asked for by the root hash as the first
///   request is and then by a chunk id, and trees of several types below
///   it, at their root and two steps down. An append-only tree has no Merk
///   nodes and is served as pages of entries, from its first entry and
///   from a later one. Only the serving side is produced. Applying chunks
///   to a second database is the receiving side.
/// - **Trunk and branch chunks**, `prove_trunk_chunk` and
///   `prove_branch_chunk`: what a light client walks a large tree with.
///   The trunk is the top of a tree that commits to its count, down to a
///   depth, and a branch is what hangs below one key.
///
/// Each round writes more rows and entries and takes some rows away, so
/// that the same requests meet trees of another shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkProofs {
    /// How many rows each Merk tree starts with.
    pub rows: u32,
    /// How many rounds to run.
    pub rounds: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for ChunkProofs {
    fn default() -> Self {
        Self {
            rows: 48,
            rounds: 3,
            features: Features::ALL,
        }
    }
}

/// A Merk tree and the keys it holds.
struct Rows {
    key: &'static [u8],
    path: Path,
    keys: Vec<Vec<u8>>,
    sums: bool,
}

impl Rows {
    fn row(&self, rng: &mut Rng) -> Element {
        if self.sums {
            Element::new_sum_item(sum(rng))
        } else {
            Element::new_item(value(rng, 1, 50))
        }
    }
}

/// The MMR size of an MMR of `leaves` leaves.
fn mmr_size(leaves: u64) -> u64 {
    2 * leaves - u64::from(leaves.count_ones())
}

impl ChunkProofs {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(
            self.rows >= MIN_CHUNKED_ROWS,
            "a tree of fewer than {MIN_CHUNKED_ROWS} rows may have no chunk two steps down"
        );
        let mut rng = Rng::derive(seed, DOMAIN_CHUNKS, 0);
        let root = path(&[ROOT]);
        let stores = self.features.private_document_store;
        let mut ops = vec![build::insert(&[], ROOT, Element::empty_tree())];
        let mut merks: Vec<Rows> = [
            (PLAIN, Element::empty_tree(), false),
            (SUMS, Element::empty_sum_tree(), true),
            (COUNTED, Element::empty_count_tree(), false),
            (PROVEN, Element::empty_provable_count_tree(), false),
        ]
        .into_iter()
        .map(|(key, element, sums)| {
            ops.push(build::insert(&root, key, element));
            Rows {
                key,
                path: child(&root, key),
                keys: Vec::new(),
                sums,
            }
        })
        .collect();
        let nested = child(&child(&root, PLAIN), NESTED);
        ops.push(build::insert(
            &child(&root, PLAIN),
            NESTED,
            Element::empty_tree(),
        ));
        ops.push(build::insert(
            &nested,
            b"only",
            Element::new_item(value(&mut rng, 1, 50)),
        ));
        let mut appended = vec![
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
            appended.push((
                PRIVATE_DOCUMENTS,
                Element::empty_private_document_store(PRIVATE_DOCUMENT_ENTRY_SIZE, CHUNK_POWER)
                    .expect("the store configuration is in range"),
            ));
        }
        for (key, element) in appended {
            ops.push(build::insert(&root, key, element));
        }
        ops.push(Op::Commit);

        let mut entries = 0u64;
        for round in 0..self.rounds {
            // Rows: a first fill, then a few more and a few fewer.
            let writes = if round == 0 { self.rows } else { 6 };
            for merk in &mut merks {
                let mut rows = Vec::new();
                if round > 0 {
                    for _ in 0..4 {
                        let gone = merk.keys.swap_remove(rng.index(merk.keys.len()));
                        rows.push(batch::delete(&merk.path, &gone));
                    }
                }
                for _ in 0..writes {
                    let key = value(&mut rng, 4, 12);
                    rows.push(batch::insert_or_replace(
                        &merk.path,
                        &key,
                        merk.row(&mut rng),
                    ));
                    merk.keys.push(key);
                }
                ops.push(Op::Batch(rows));
            }
            // Entries: a few more in every append-only tree.
            let appends = rng.between(3, 6);
            let tree = |key: &[u8]| child(&root, key);
            for _ in 0..appends {
                ops.push(build::append(
                    &tree(MMR),
                    AppendEntry::Mmr(value(&mut rng, 1, 40)),
                ));
                ops.push(build::append(
                    &tree(BULK),
                    AppendEntry::Bulk(value(&mut rng, 1, 40)),
                ));
                ops.push(build::append(
                    &tree(DENSE),
                    AppendEntry::Dense(value(&mut rng, 1, 40)),
                ));
                ops.push(Op::Batch(vec![batch::append(
                    &tree(COMMITMENTS),
                    commitment_entry(&mut rng),
                )]));
                if stores {
                    ops.push(build::append(
                        &tree(PRIVATE_DOCUMENTS),
                        AppendEntry::PrivateDocument(rng.bytes(PRIVATE_DOCUMENT_ENTRY_SIZE)),
                    ));
                }
            }
            entries += appends;
            ops.push(Op::Commit);

            let chunk = |path: &[Vec<u8>], ids: ChunkIds| {
                prove(Prove::Chunk {
                    path: path.to_vec(),
                    ids,
                })
            };
            let merk_chunks =
                |ids: &[&[u8]]| ChunkIds::Merk(ids.iter().map(|id| id.to_vec()).collect());

            // State sync, from the root tree down.
            ops.push(chunk(&[], merk_chunks(&[])));
            ops.push(chunk(&[], merk_chunks(&[&[]])));
            ops.push(chunk(&root, merk_chunks(&[])));
            for merk in &merks {
                ops.push(chunk(&merk.path, merk_chunks(&[])));
                let side = u8::from(rng.chance(1, 2));
                ops.push(chunk(
                    &merk.path,
                    merk_chunks(&[&[side, 1 - side], &[1 - side, side]]),
                ));
                ops.push(chunk(&merk.path, merk_chunks(&[&[side]])));
            }
            ops.push(chunk(&nested, merk_chunks(&[])));
            let later = rng.below(entries);
            for start in [0, later] {
                let page = |state: u64, parameter: u8| ChunkIds::Entries {
                    start,
                    state,
                    parameter,
                };
                ops.push(chunk(&tree(MMR), page(mmr_size(entries), 0)));
                ops.push(chunk(&tree(BULK), page(entries, CHUNK_POWER)));
                ops.push(chunk(&tree(DENSE), page(entries, DENSE_HEIGHT)));
                ops.push(chunk(&tree(COMMITMENTS), page(entries, CHUNK_POWER)));
                if stores {
                    ops.push(chunk(&tree(PRIVATE_DOCUMENTS), page(entries, CHUNK_POWER)));
                }
            }

            // The trunk of the trees that commit to their count, and the
            // branch below some of their keys.
            for merk in &merks {
                if merk.key != COUNTED && merk.key != PROVEN {
                    continue;
                }
                let max_depth = rng.between(2, 4) as u8;
                ops.push(prove(Prove::TrunkChunk {
                    path: merk.path.clone(),
                    max_depth,
                    min_depth: (merk.key == PROVEN).then_some(1),
                }));
                // The keys at the bottom of a trunk are the ones a client
                // asks the branches of. Which they are depends on the shape
                // of the tree, so the last round asks below every key of
                // one tree, and the others below a few.
                let every_key = merk.key == COUNTED && round + 1 == self.rounds;
                let asked: Vec<Vec<u8>> = if every_key {
                    merk.keys.clone()
                } else {
                    (0..4)
                        .map(|_| merk.keys[rng.index(merk.keys.len())].clone())
                        .collect()
                };
                for key in asked {
                    ops.push(prove(Prove::BranchChunk {
                        path: merk.path.clone(),
                        key,
                        depth: max_depth,
                    }));
                }
            }
            let plain = &merks[0];
            ops.push(prove(Prove::BranchChunk {
                path: plain.path.clone(),
                key: plain.keys[rng.index(plain.keys.len())].clone(),
                depth: 2,
            }));
        }
        Prepared::stream(ops)
    }
}
