//! The rewrite of index value keys that Drive runs once, by moving them.

use std::collections::{BTreeMap, BTreeSet};

use grovedb::{element::reference_path::ReferencePathType, Element};

use super::{epoch_of_block, DOMAIN_INDEX_REKEY};
use crate::workload::{
    build::{self, batch, child, path, storage_flags},
    op::{BatchOp, Op, Path, Query, QueryItem},
    rng::Rng,
    Features, Prepared, Stream,
};

/// Drive keeps documents under `DataContractDocuments`, which
/// [`super::Documents`] writes. This workload stores the same shape under a
/// root tree of its own.
const ROOT: &[u8] = &[66];
const CONTRACT_DOCUMENTS_KEY: &[u8] = &[1];
const DOCUMENT_TYPE: &[u8] = b"reading";
const PRIMARY_KEY: &[u8] = &[0];
const INDEX: &[u8] = b"height";
const TOP_BIT: u64 = 1 << 63;

/// An index over an unsigned integer whose value keys are moved.
///
/// Documents are indexed by an integer property: under the property's tree,
/// one tree per value holds the references to the documents with that value.
/// The keys of those trees first flip the top bit of the value, as for a
/// signed integer. The rewrite then moves every value tree to the key that
/// is the plain value, as Drive does on the first block of the protocol
/// version that changed the encoding: each move keeps the tree and what is
/// under it, a value and the same value with its top bit set trade keys in
/// one batch, and a batch holds `moves_per_batch` moves. Documents are
/// written and read before and after.
///
/// A stream for a grove version without `Move` leaves the keys as they
/// are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexRekey {
    /// How many documents are stored before the rewrite.
    pub documents: u64,
    /// How many distinct values the property takes.
    pub values: u32,
    /// How many of those values also occur with the top bit set.
    pub twins: u32,
    /// How many moves go into one batch.
    pub moves_per_batch: u32,
    /// How many documents are stored after the rewrite.
    pub documents_after: u64,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for IndexRekey {
    fn default() -> Self {
        Self {
            documents: 240,
            values: 40,
            twins: 6,
            moves_per_batch: 8,
            documents_after: 40,
            features: Features::ALL,
        }
    }
}

struct Generator {
    config: IndexRekey,
    rng: Rng,
    contract: [u8; 32],
    values: Vec<u64>,
    /// The documents stored under each value.
    stored: BTreeMap<u64, u64>,
    rekeyed: bool,
    block: u64,
    ops: Vec<Op>,
}

impl IndexRekey {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.values > 0, "nothing to index by");
        assert!(self.twins <= self.values, "more twins than values");
        assert!(self.moves_per_batch > 0, "a batch needs a move");
        let mut rng = Rng::derive(seed, DOMAIN_INDEX_REKEY, 0);
        let mut distinct = BTreeSet::new();
        while (distinct.len() as u32) < self.values {
            distinct.insert(rng.below(1_000_000));
        }
        let mut values: Vec<u64> = distinct.into_iter().collect();
        let twins: Vec<u64> = values[..self.twins as usize]
            .iter()
            .map(|value| value | TOP_BIT)
            .collect();
        values.extend(twins);
        let mut generator = Generator {
            config: *self,
            contract: rng.id(),
            rng,
            values,
            stored: BTreeMap::new(),
            rekeyed: false,
            block: 0,
            ops: Vec::new(),
        };
        generator.setup();
        generator.store(self.documents);
        generator.read();
        if self.features.move_element {
            generator.rekey();
            generator.read();
        }
        generator.store(self.documents_after);
        generator.read();
        Prepared::stream(generator.ops)
    }
}

impl Generator {
    fn type_path(&self) -> Path {
        path(&[ROOT, &self.contract, CONTRACT_DOCUMENTS_KEY, DOCUMENT_TYPE])
    }

    fn primary_path(&self) -> Path {
        child(&self.type_path(), PRIMARY_KEY)
    }

    fn index_path(&self) -> Path {
        child(&self.type_path(), INDEX)
    }

    /// The key a value's tree is at: the value with its top bit flipped
    /// before the rewrite, the value itself after it.
    fn value_key(&self, value: u64) -> [u8; 8] {
        if self.rekeyed {
            value.to_be_bytes()
        } else {
            (value ^ TOP_BIT).to_be_bytes()
        }
    }

    fn setup(&mut self) {
        let flags = storage_flags(0, None);
        let tree = || Element::empty_tree_with_flags(flags.clone());
        let contract_path = path(&[ROOT, &self.contract]);
        self.ops.extend([
            build::insert(&[], ROOT, Element::empty_tree()),
            build::insert(&path(&[ROOT]), &self.contract, tree()),
            build::insert(&contract_path, CONTRACT_DOCUMENTS_KEY, tree()),
            build::insert(
                &child(&contract_path, CONTRACT_DOCUMENTS_KEY),
                DOCUMENT_TYPE,
                tree(),
            ),
            build::insert(&self.type_path(), PRIMARY_KEY, tree()),
            build::insert(&self.type_path(), INDEX, tree()),
            Op::Commit,
        ]);
        self.block += 1;
    }

    /// Stores `count` documents, twenty to a block.
    fn store(&mut self, count: u64) {
        for stored in 0..count {
            let epoch = epoch_of_block(self.block);
            let flags = storage_flags(epoch, None);
            let id = self.rng.id();
            let value = self.values[self.rng.index(self.values.len())];
            let value_key = self.value_key(value);
            let index_path = self.index_path();
            let value_path = child(&index_path, &value_key);
            let mut ops = vec![batch::insert_only(
                &self.primary_path(),
                &id,
                Element::new_item_with_flags(self.rng.bytes(120), flags.clone()),
            )];
            let documents = self.stored.entry(value).or_insert(0);
            *documents += 1;
            if *documents == 1 {
                ops.push(batch::insert_only(
                    &index_path,
                    &value_key,
                    Element::empty_tree_with_flags(flags.clone()),
                ));
                ops.push(batch::insert_only(
                    &value_path,
                    PRIMARY_KEY,
                    Element::empty_tree_with_flags(flags.clone()),
                ));
            }
            ops.push(batch::insert_only(
                &child(&value_path, PRIMARY_KEY),
                &id,
                Element::Reference(
                    ReferencePathType::UpstreamRootHeightReference(
                        4,
                        vec![PRIMARY_KEY.to_vec(), id.to_vec()],
                    ),
                    Some(1),
                    flags,
                ),
            ));
            self.ops.push(Op::Batch(ops));
            if (stored + 1) % 20 == 0 || stored + 1 == count {
                self.ops.push(Op::Commit);
                self.block += 1;
            }
        }
    }

    /// Moves every value tree to its new key, in key order.
    fn rekey(&mut self) {
        let index_path = self.index_path();
        let present: BTreeSet<[u8; 8]> = self
            .stored
            .keys()
            .map(|&value| self.value_key(value))
            .collect();
        let limit = self.config.moves_per_batch as usize;
        let mut pending: Vec<BatchOp> = Vec::new();
        for key in &present {
            let mut new_key = *key;
            new_key[0] ^= 0x80;
            if present.contains(&new_key) {
                if new_key < *key {
                    continue;
                }
                pending.push(batch::move_key(&index_path, &new_key, key));
            }
            pending.push(batch::move_key(&index_path, key, &new_key));
            if pending.len() >= limit {
                self.ops.push(Op::Batch(std::mem::take(&mut pending)));
            }
        }
        if !pending.is_empty() {
            self.ops.push(Op::Batch(pending));
        }
        self.ops.push(Op::Commit);
        self.block += 1;
        self.rekeyed = true;
    }

    /// Reads documents through the index: by one value, and by a range of
    /// keys from each end.
    fn read(&mut self) {
        let index_path = self.index_path();
        let through_index = |items, left_to_right| {
            build::query(Query {
                subquery_path: Some(vec![PRIMARY_KEY.to_vec()]),
                subquery_all: true,
                left_to_right,
                ..build::select(&index_path, items, Some(30))
            })
        };
        let stored: Vec<u64> = self.stored.keys().copied().collect();
        if stored.is_empty() {
            return;
        }
        let value = stored[self.rng.index(stored.len())];
        let key = self.value_key(value);
        self.ops
            .push(through_index(vec![QueryItem::Key(key.to_vec())], true));
        self.ops.push(through_index(
            vec![QueryItem::RangeFrom(key.to_vec())],
            true,
        ));
        self.ops
            .push(through_index(vec![QueryItem::RangeFull], false));
        self.ops.push(build::get(&index_path, &key));
    }
}
