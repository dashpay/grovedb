//! Indexes that count, sum and rank, read by aggregate queries.

use std::collections::BTreeMap;

use grovedb::{Element, PathQuery, Query, TreeType};
use grovedb_merk::proofs::query::{AggregateFold, IndexAxis};

use super::{
    document_reference, document_type_trees, epoch_of_block, identity_id, index_document,
    CONTRACT_DOCUMENTS_KEY, DOMAIN_INDEX_AGGREGATES, PRIMARY_KEY,
};
use crate::workload::{
    build::{self, batch, child, path, storage_flags},
    op::{BatchOp, Op, Path},
    rng::Rng,
    Features, Prepared, Stream,
};

/// A root tree of this workload's own, in place of Drive's
/// `DataContractDocuments`.
const ROOT: &[u8] = &[68];
const DOCUMENT_TYPE: &[u8] = b"order";
const CATEGORY: &[u8] = b"category";
const AMOUNT: &[u8] = b"amount";
const SCORE: &[u8] = b"score";
const OWNER: &[u8] = b"$ownerId";
const TAG: &[u8] = b"tag";

/// One index of the document type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Index {
    /// Counted by range: a provable count tree of count trees.
    Category,
    /// Summed by range: a provable sum tree of sum trees.
    Amount,
    /// Counted and summed by range: a tree committing both, of count-sum
    /// trees.
    Score,
    /// A category index for each owner, counted for several owners at once.
    OwnerCategory,
    /// Ranked by count: an indexed tree of count trees.
    Tag,
}

impl Index {
    const ALL: [Index; 5] = [
        Index::Category,
        Index::Amount,
        Index::Score,
        Index::OwnerCategory,
        Index::Tag,
    ];

    /// The tree holding one tree per value.
    fn property_tree(self, flags: Option<Vec<u8>>) -> Element {
        match self {
            Index::Category | Index::OwnerCategory => {
                Element::empty_provable_count_tree_with_flags(flags)
            }
            Index::Amount => Element::empty_provable_sum_tree_with_flags(flags),
            Index::Score => Element::empty_provable_count_provable_sum_tree_with_flags(flags),
            Index::Tag => Element::empty_provable_count_indexed_tree_with_flags(flags),
        }
    }

    /// The tree of one value, and the tree of references under it.
    fn value_tree(self, flags: Option<Vec<u8>>) -> (Element, TreeType) {
        match self {
            Index::Category | Index::OwnerCategory | Index::Tag => (
                Element::empty_count_tree_with_flags(flags),
                TreeType::CountTree,
            ),
            Index::Amount => (Element::empty_sum_tree_with_flags(flags), TreeType::SumTree),
            Index::Score => (
                Element::empty_count_sum_tree_with_flags(flags),
                TreeType::CountSumTree,
            ),
        }
    }
}

/// What one document is indexed by.
#[derive(Clone, Copy, Debug)]
struct Document {
    id: [u8; 32],
    owner: u64,
    category: u8,
    amount: u64,
    score: u8,
    tag: u8,
}

/// Documents under indexes that aggregate.
///
/// A document type keeps five indexes whose trees commit a count, a sum or
/// both into their hashes, so GroveDB can answer for a range of values
/// without reading the documents: how many, their total, both, and a count
/// for each of several owners. A fifth index is an indexed tree, which also
/// orders its values by how many documents each has, for the pages, bounds,
/// ranks and totals of a ranking. Documents are stored and deleted; the
/// reads are those aggregate queries.
///
/// A stream for a grove version without read modes reads the ranked index
/// by key only, and one for a grove version without the V1 proof envelope
/// marks the aggregate queries as not proven: such a version answers them
/// and refuses to prove them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexAggregates {
    /// How many documents to store.
    pub documents: u64,
    /// How many of them are deleted afterwards.
    pub deletes: u64,
    /// How many owners the documents are spread over.
    pub owners: u64,
    /// How many state transitions make a block.
    pub transitions_per_block: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for IndexAggregates {
    fn default() -> Self {
        Self {
            documents: 160,
            deletes: 60,
            owners: 5,
            transitions_per_block: 16,
            features: Features::ALL,
        }
    }
}

const CATEGORIES: u64 = 10;
const AMOUNTS: u64 = 24;
const SCORES: u64 = 12;
const TAGS: u64 = 8;

struct Generator {
    config: IndexAggregates,
    seed: u64,
    rng: Rng,
    read_rng: Rng,
    type_path: Path,
    documents: Vec<Document>,
    /// How many documents each value tree of each index holds.
    held: BTreeMap<(Index, Vec<Vec<u8>>), u64>,
    block: u64,
    reads: u64,
    ops: Vec<Op>,
}

impl IndexAggregates {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.documents > 0 && self.owners > 0, "nothing to generate");
        assert!(
            self.deletes <= self.documents,
            "more deletes than documents"
        );
        assert!(self.transitions_per_block > 0, "a block needs a transition");
        let mut rng = Rng::derive(seed, DOMAIN_INDEX_AGGREGATES, 0);
        let contract = rng.id();
        let mut generator = Generator {
            config: *self,
            seed,
            rng,
            read_rng: Rng::derive(seed, DOMAIN_INDEX_AGGREGATES, 1),
            type_path: path(&[ROOT, &contract, CONTRACT_DOCUMENTS_KEY, DOCUMENT_TYPE]),
            documents: Vec::new(),
            held: BTreeMap::new(),
            block: 0,
            reads: 0,
            ops: Vec::new(),
        };
        generator.setup(&contract);
        let per_block = u64::from(self.transitions_per_block);
        for stored in 0..self.documents {
            generator.store();
            if build::ends_block(stored + 1, self.documents, per_block) {
                generator.end_block();
            }
        }
        for deleted in 0..self.deletes {
            generator.delete();
            if build::ends_block(deleted + 1, self.deletes, per_block) {
                generator.end_block();
            }
        }
        Prepared::stream(generator.ops)
    }
}

impl Generator {
    fn index_path(&self, index: Index) -> Path {
        index_path(&self.type_path, index)
    }

    /// The keys from an index tree down to the value tree of `document`.
    fn value_keys(&self, index: Index, document: &Document) -> Vec<Vec<u8>> {
        match index {
            Index::Category => vec![vec![document.category]],
            Index::Amount => vec![document.amount.to_be_bytes().to_vec()],
            Index::Score => vec![vec![document.score]],
            Index::OwnerCategory => vec![
                identity_id(self.seed, document.owner).to_vec(),
                CATEGORY.to_vec(),
                vec![document.category],
            ],
            Index::Tag => vec![vec![document.tag]],
        }
    }

    /// The reference an index stores for `document`.
    fn reference(&self, index: Index, document: &Document, flags: Option<Vec<u8>>) -> Element {
        let reference = document_reference(&document.id);
        match index {
            Index::Category | Index::OwnerCategory | Index::Tag => {
                Element::Reference(reference, Some(1), flags)
            }
            Index::Amount => {
                Element::ReferenceWithSumItem(reference, Some(1), document.amount as i64, flags)
            }
            Index::Score => {
                Element::ReferenceWithSumItem(reference, Some(1), i64::from(document.score), flags)
            }
        }
    }

    fn setup(&mut self, contract: &[u8; 32]) {
        let flags = storage_flags(0, None);
        self.ops
            .extend(document_type_trees(ROOT, contract, DOCUMENT_TYPE, &[OWNER]));
        let mut indexes = Vec::new();
        for index in [Index::Category, Index::Amount, Index::Score, Index::Tag] {
            let key = self.index_path(index).pop().expect("an index has a key");
            indexes.push(batch::insert_only(
                &self.type_path,
                &key,
                index.property_tree(flags.clone()),
            ));
        }
        self.ops.push(Op::Batch(indexes));
        self.ops.push(Op::Commit);
        self.block += 1;
    }

    fn store(&mut self) {
        let document = Document {
            id: self.rng.id(),
            owner: self.rng.below(self.config.owners),
            category: self.rng.below(CATEGORIES) as u8,
            amount: 100 * (1 + self.rng.below(AMOUNTS)),
            score: self.rng.below(SCORES) as u8,
            tag: self.rng.below(TAGS) as u8,
        };
        let flags = storage_flags(epoch_of_block(self.block), None);
        let mut ops = vec![batch::insert_only(
            &child(&self.type_path, PRIMARY_KEY),
            &document.id,
            Element::new_item_with_flags(self.rng.bytes(80), flags.clone()),
        )];
        for index in Index::ALL {
            let keys = self.value_keys(index, &document);
            let mut parent = self.index_path(index);
            if index == Index::OwnerCategory {
                let owner_held = self
                    .held
                    .range((index, vec![keys[0].clone()])..)
                    .next()
                    .is_some_and(|((held_index, held), _)| {
                        *held_index == index && held[0] == keys[0]
                    });
                if !owner_held {
                    ops.push(batch::insert_only(
                        &parent,
                        &keys[0],
                        Element::empty_tree_with_flags(flags.clone()),
                    ));
                    ops.push(batch::insert_only(
                        &child(&parent, &keys[0]),
                        &keys[1],
                        index.property_tree(flags.clone()),
                    ));
                }
                parent.extend_from_slice(&keys[..2]);
            }
            let held = self.held.entry((index, keys.clone())).or_insert(0);
            *held += 1;
            let new_value = *held == 1;
            index_document(
                &mut ops,
                &parent,
                keys.last().expect("an index has a value"),
                new_value.then(|| index.value_tree(flags.clone()).0),
                &document.id,
                self.reference(index, &document, flags.clone()),
            );
        }
        self.documents.push(document);
        self.ops.push(Op::Batch(ops));
    }

    fn delete(&mut self) {
        let chosen = self.rng.index(self.documents.len());
        let document = self.documents.swap_remove(chosen);
        let mut ops: Vec<BatchOp> = Vec::new();
        for index in Index::ALL {
            let keys = self.value_keys(index, &document);
            let mut parent = self.index_path(index);
            if index == Index::OwnerCategory {
                parent.extend_from_slice(&keys[..2]);
            }
            let value_key = keys.last().expect("an index has a value");
            let value_path = child(&parent, value_key);
            ops.push(batch::delete(
                &child(&value_path, PRIMARY_KEY),
                &document.id,
            ));
            let held = self
                .held
                .get_mut(&(index, keys.clone()))
                .expect("a stored document is indexed");
            *held -= 1;
            if *held == 0 {
                self.held.remove(&(index, keys.clone()));
                let (_, tree_type) = index.value_tree(None);
                ops.push(batch::delete_tree(&value_path, PRIMARY_KEY, tree_type));
                ops.push(batch::delete_tree(&parent, value_key, tree_type));
            }
        }
        ops.push(batch::delete(
            &child(&self.type_path, PRIMARY_KEY),
            &document.id,
        ));
        self.ops.push(Op::Batch(ops));
    }

    fn end_block(&mut self) {
        self.ops.push(Op::Commit);
        self.block += 1;
        for _ in 0..4 {
            self.read();
        }
    }

    fn read(&mut self) {
        let rng = &mut self.read_rng;
        let kind = self.reads % 8;
        self.reads += 1;
        let index_path = |index| index_path(&self.type_path, index);
        let ranked = index_path(Index::Tag);
        let query = match kind {
            0 => {
                let low = rng.below(CATEGORIES) as u8;
                let high = rng.between(u64::from(low), CATEGORIES) as u8;
                PathQuery::new_aggregate_count_on_range(
                    index_path(Index::Category),
                    grovedb::QueryItem::RangeInclusive(vec![low]..=vec![high]),
                )
            }
            1 => {
                let low = 100 * rng.below(AMOUNTS);
                let high = low + 100 * rng.between(1, AMOUNTS);
                PathQuery::new_aggregate_sum_on_range(
                    index_path(Index::Amount),
                    grovedb::QueryItem::Range(
                        low.to_be_bytes().to_vec()..high.to_be_bytes().to_vec(),
                    ),
                )
            }
            2 => PathQuery::new_aggregate_count_and_sum_on_range(
                index_path(Index::Score),
                grovedb::QueryItem::RangeFrom(vec![rng.below(SCORES) as u8]..),
            ),
            3 => {
                let mut owners = Query::new();
                for _ in 0..rng.between(1, 3) {
                    owners.insert_key(identity_id(self.seed, rng.below(self.config.owners)).into());
                }
                owners.set_subquery_path(vec![CATEGORY.to_vec()]);
                owners.set_subquery(Query::new_aggregate_count_on_range(
                    grovedb::QueryItem::RangeTo(..vec![rng.between(1, CATEGORIES) as u8]),
                ));
                build::sized(&index_path(Index::OwnerCategory), owners, None, None)
            }
            _ if !self.config.features.read_modes => {
                let tag = [rng.below(TAGS) as u8];
                if self.held.contains_key(&(Index::Tag, vec![tag.to_vec()])) {
                    self.ops.push(build::get(&ranked, &tag));
                }
                return;
            }
            4 => PathQuery::new_axis_top_k(
                ranked,
                IndexAxis::Count,
                rng.between(1, 4) as u16,
                rng.below(3),
                true,
            ),
            5 => {
                let low = i128::from(rng.below(12));
                PathQuery::new_axis_bounded(
                    ranked,
                    IndexAxis::Count,
                    low,
                    low + i128::from(rng.between(1, 30)),
                    5,
                    rng.chance(1, 2),
                )
            }
            6 => {
                let held: Vec<&Vec<Vec<u8>>> = self
                    .held
                    .keys()
                    .filter(|(index, _)| *index == Index::Tag)
                    .map(|(_, keys)| keys)
                    .collect();
                let Some(keys) = (!held.is_empty()).then(|| held[rng.index(held.len())]) else {
                    return;
                };
                PathQuery::new_axis_rank_of_key(ranked, IndexAxis::Count, keys[0].clone(), true)
            }
            _ => PathQuery::new_axis_aggregate_over_value_range(
                ranked,
                IndexAxis::Count,
                i128::from(rng.below(10)),
                i128::from(rng.between(10, 60)),
                if rng.chance(1, 2) {
                    AggregateFold::Population
                } else {
                    AggregateFold::Total
                },
            ),
        };
        self.ops.push(build::path_query_read(
            query,
            self.config.features.v1_proofs,
        ));
    }
}

fn index_path(type_path: &[Vec<u8>], index: Index) -> Path {
    child(
        type_path,
        match index {
            Index::Category => CATEGORY,
            Index::Amount => AMOUNT,
            Index::Score => SCORE,
            Index::OwnerCategory => OWNER,
            Index::Tag => TAG,
        },
    )
}
