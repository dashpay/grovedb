//! Documents that expire, removed the way Drive removes them.

use std::collections::BTreeMap;

use grovedb::{Element, TreeType};

use super::{
    document_reference, document_type_trees, epoch_of_block, index_document,
    CONTRACT_DOCUMENTS_KEY, DOMAIN_EXPIRATION, PRIMARY_KEY,
};
use crate::workload::{
    build::{self, batch, child, path, storage_flags},
    op::{Op, Path, Query, QueryItem},
    rng::Rng,
    Features, Prepared, Stream,
};

/// A root tree of this workload's own, in place of Drive's
/// `DataContractDocuments`.
const ROOT: &[u8] = &[70];
const DOCUMENT_TYPE: &[u8] = b"session";
const INDEX: &[u8] = b"$expiresAt";

/// Documents with a time to live.
///
/// Every document is indexed by the block it expires at: under the index
/// tree, one tree per block holds a tree of references to the documents
/// expiring then. Each block stores documents and then removes what has
/// expired. The documents go one by one in a batch. The index entry for
/// the block is not taken apart: its tree of references and then the entry
/// itself are dropped flat, which removes each from its parent without
/// opening it and leaves a record of the storage to reclaim, and after the
/// block is committed the records are flushed, as Platform does.
///
/// A stream for a grove version without flat drops deletes the entry with
/// everything under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expiration {
    /// How many blocks to write.
    pub blocks: u64,
    /// How many documents a block stores.
    pub documents_per_block: u32,
    /// The longest a document lives, in blocks.
    pub longest_life: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for Expiration {
    fn default() -> Self {
        Self {
            blocks: 40,
            documents_per_block: 6,
            longest_life: 8,
            features: Features::ALL,
        }
    }
}

impl Expiration {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.longest_life > 0, "a document lives for a block");
        let mut rng = Rng::derive(seed, DOMAIN_EXPIRATION, 0);
        let contract = rng.id();
        let type_path = path(&[ROOT, &contract, CONTRACT_DOCUMENTS_KEY, DOCUMENT_TYPE]);
        let primary_path = child(&type_path, PRIMARY_KEY);
        let index_path = child(&type_path, INDEX);
        let mut ops = document_type_trees(ROOT, &contract, DOCUMENT_TYPE, &[INDEX]);
        ops.push(Op::Commit);

        let mut expiring: BTreeMap<u64, Vec<[u8; 32]>> = BTreeMap::new();
        for block in 1..=self.blocks {
            let flags = storage_flags(epoch_of_block(block), None);
            for _ in 0..self.documents_per_block {
                let id = rng.id();
                let expires_at = block + rng.between(1, u64::from(self.longest_life));
                let mut batch_ops = vec![batch::insert_only(
                    &primary_path,
                    &id,
                    Element::new_item_with_flags(rng.bytes(96), flags.clone()),
                )];
                let documents = expiring.entry(expires_at).or_default();
                let new_time = documents.is_empty();
                documents.push(id);
                index_document(
                    &mut batch_ops,
                    &index_path,
                    &expires_at.to_be_bytes(),
                    new_time.then(|| Element::empty_tree_with_flags(flags.clone())),
                    &id,
                    Element::Reference(document_reference(&id), Some(1), flags.clone()),
                );
                ops.push(Op::Batch(batch_ops));
            }

            let expired = expiring.remove(&block);
            if let Some(documents) = &expired {
                let time_key = block.to_be_bytes();
                let time_path = child(&index_path, &time_key);
                ops.push(Op::Batch(
                    documents
                        .iter()
                        .map(|id| batch::delete(&primary_path, id))
                        .collect(),
                ));
                self.remove_entry(block, &index_path, &time_path, &time_key, &mut ops);
            }
            ops.push(Op::Commit);
            if expired.is_some() && self.features.flat_drop {
                ops.push(Op::FlushPendingDrops);
            }
            self.read(
                block,
                &index_path,
                &primary_path,
                &expiring,
                &mut rng,
                &mut ops,
            );
        }
        Prepared::stream(ops)
    }

    /// Removes the index entry of one block with the references under it.
    fn remove_entry(
        &self,
        block: u64,
        index_path: &[Vec<u8>],
        time_path: &[Vec<u8>],
        time_key: &[u8],
        ops: &mut Vec<Op>,
    ) {
        if !self.features.flat_drop {
            ops.push(build::delete(index_path, time_key));
        } else if block.is_multiple_of(3) {
            ops.push(Op::Batch(vec![batch::drop_flat(
                time_path,
                PRIMARY_KEY,
                TreeType::NormalTree,
            )]));
            ops.push(Op::Batch(vec![batch::drop_flat(
                index_path,
                time_key,
                TreeType::NormalTree,
            )]));
        } else {
            ops.push(build::drop_flat(time_path, PRIMARY_KEY));
            ops.push(build::drop_flat(index_path, time_key));
        }
    }

    fn read(
        &self,
        block: u64,
        index_path: &[Vec<u8>],
        primary_path: &Path,
        expiring: &BTreeMap<u64, Vec<[u8; 32]>>,
        rng: &mut Rng,
        ops: &mut Vec<Op>,
    ) {
        ops.push(build::query(Query {
            subquery_path: Some(vec![PRIMARY_KEY.to_vec()]),
            subquery_all: true,
            ..build::select(
                index_path,
                vec![QueryItem::RangeTo((block + 3).to_be_bytes().to_vec())],
                Some(20),
            )
        }));
        ops.push(build::query(build::select_raw(
            index_path,
            vec![QueryItem::Key(block.to_be_bytes().to_vec())],
            None,
        )));
        let next = expiring.range(block + 1..).next();
        if let Some((_, documents)) = next {
            let id = documents[rng.index(documents.len())];
            ops.push(build::get(primary_path, &id));
        }
    }
}
