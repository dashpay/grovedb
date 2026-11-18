//! Documents replaced, handed over and deleted across epochs, each batch
//! applied with the callbacks Drive applies it with.

use std::collections::BTreeMap;

use grovedb::{Element, PathQuery, Query, SizedQuery, TreeType};

use super::{
    document_reference, document_type_trees, identity_id, index_document, CONTRACT_DOCUMENTS_KEY,
    DOCUMENT_TYPE, DOMAIN_STORAGE_FEES, PRIMARY_KEY,
};
use crate::workload::{
    build::{self, batch, child, ends_block, path, storage_flags},
    flags::{Histories, History, CLEAR_OF_AN_EPOCH},
    op::{BatchOp, HandoverBatch, Op, Path},
    rng::Rng,
    Features, Prepared, Stream,
};

/// A root tree of this workload's own, beside Drive's `DataContractDocuments`.
const ROOT: &[u8] = &[82];
const KIND: &[u8] = b"kind";

/// The shortest document.
const SHORTEST: usize = 48;
/// The least and the most a document grows by in one update.
const GROWTH: (u64, u64) = (48, 160);

/// Documents that live through many epochs, and the storage fees their
/// owners pay and get back.
///
/// Platform charges for storage by the byte and by the epoch it is paid in,
/// and refunds what is freed to the identity that paid, in proportion to
/// the epochs still ahead. So every element carries flags: the epoch it was
/// created in, its owner and the bytes each later epoch added. Drive
/// applies every batch with two callbacks that keep those flags
/// ([`Op::FlagsBatch`]), and this workload applies every state transition
/// that way, an update of a document as from protocol version 15 once the
/// stream is halfway through:
///
/// - **Create.** A document in the primary key tree and a reference to it
///   in an index by kind, both with the flags of the epoch and the owner.
///   The first document of a kind creates the kind's index trees, which
///   carry its flags too.
/// - **Replace.** The document is written again with the flags of the
///   current epoch: longer, so that the flags record the bytes under that
///   epoch; shorter, so that the freed bytes go back to the epochs that
///   paid for them, the latest first, and to more than one where the
///   document is left shorter than it was before the latest one; or of the
///   same length, which keeps the flags as they were. Its index reference
///   is refreshed, as its hash follows the document.
/// - **Transfer.** The same three updates with the flags of another owner.
///   Drive's callback stores the new owner where the update changes the
///   length of a document that later epochs have added to, or makes it
///   longer, and keeps the old one otherwise.
/// - **Handover.** In the second half of the stream every update is
///   applied as Drive applies one from protocol version 15
///   ([`Op::FlagsHandoverBatch`]): the owner written is stored whatever
///   the update does to the length. One transfer in three goes to a
///   sponsor, an identity that pays for documents of others, and what the
///   sponsor holds goes to an owner. On the grove versions that account an
///   update as a replacement, either is a full handover: the document is
///   freed whole to the one and charged whole to the other.
/// - **Delete.** The document and its reference are deleted, and the index
///   trees of a kind with no document left with them. The cost names every
///   freed byte by owner and epoch.
///
/// The epoch advances every few blocks, up to the last epoch there is,
/// which lasts. After each block the documents are read as stored, flags
/// included, and a page of them is proven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageFees {
    /// How many state transitions to apply.
    pub transitions: u64,
    /// How many identities own documents.
    pub owners: u64,
    /// How many kinds the documents are indexed by.
    pub kinds: u64,
    /// How many state transitions make a block.
    pub transitions_per_block: u32,
    /// How many blocks make an epoch.
    pub blocks_per_epoch: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for StorageFees {
    fn default() -> Self {
        Self {
            transitions: 240,
            owners: 5,
            kinds: 12,
            transitions_per_block: 5,
            blocks_per_epoch: 2,
            features: Features::ALL,
        }
    }
}

struct Document {
    owner: u64,
    kind: Vec<u8>,
    /// The flags its index reference is stored with.
    reference_flags: Option<Vec<u8>>,
}

/// What a state transition does, in the order the generator tries them
/// round after round. One that finds no document to act on creates one.
#[derive(Clone, Copy)]
enum Transition {
    Create,
    Grow { transfer: bool },
    ShrinkWithinEpoch,
    ShrinkAcrossEpochs { transfer: bool },
    ShrinkYoung { transfer: bool },
    SameLength { transfer: bool },
    Delete,
}

const ROUND: [Transition; 20] = [
    Transition::Create,
    Transition::Create,
    Transition::Grow { transfer: false },
    Transition::Create,
    Transition::Grow { transfer: false },
    Transition::SameLength { transfer: false },
    Transition::Grow { transfer: false },
    Transition::ShrinkWithinEpoch,
    Transition::Create,
    Transition::Grow { transfer: true },
    Transition::ShrinkAcrossEpochs { transfer: false },
    Transition::Grow { transfer: false },
    Transition::ShrinkYoung { transfer: true },
    Transition::Delete,
    Transition::Grow { transfer: false },
    Transition::SameLength { transfer: true },
    Transition::ShrinkAcrossEpochs { transfer: true },
    Transition::Create,
    Transition::ShrinkYoung { transfer: false },
    Transition::Delete,
];

struct State {
    seed: u64,
    rng: Rng,
    owners: u64,
    kinds: u64,
    primary_path: Path,
    index_path: Path,
    documents: BTreeMap<[u8; 32], Document>,
    /// How many documents each kind indexes.
    indexed: BTreeMap<Vec<u8>, u64>,
    /// What the flags of every document hold.
    histories: Histories,
    epoch: u16,
    /// Whether a batch that updates a document hands it over: the
    /// protocol of the second half of the stream.
    hands_over: bool,
    /// The owners a handover to or from which is a full one.
    sponsors: Vec<[u8; 32]>,
}

impl StorageFees {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.owners > 1, "a transfer needs two owners");
        assert!(self.kinds > 0, "a document has a kind");
        assert!(self.transitions_per_block > 0, "a block needs a transition");
        assert!(self.blocks_per_epoch > 0, "an epoch needs a block");
        let mut rng = Rng::derive(seed, DOMAIN_STORAGE_FEES, 0);
        let contract = rng.id();
        let type_path = path(&[ROOT, &contract, CONTRACT_DOCUMENTS_KEY, DOCUMENT_TYPE]);
        let mut ops = document_type_trees(ROOT, &contract, DOCUMENT_TYPE, &[KIND]);
        ops.push(Op::Commit);
        let mut state = State {
            seed,
            rng,
            owners: self.owners,
            kinds: self.kinds,
            primary_path: child(&type_path, PRIMARY_KEY),
            index_path: child(&type_path, KIND),
            documents: BTreeMap::new(),
            indexed: BTreeMap::new(),
            histories: Histories::new(),
            epoch: 0,
            hands_over: false,
            sponsors: Vec::new(),
        };
        if self.features.full_handovers {
            state.sponsors.push(identity_id(seed, state.sponsor()));
        }
        let mut block = 0u64;
        for (done, transition) in (1..=self.transitions).zip(ROUND.iter().cycle()) {
            state.hands_over = done > self.transitions / 2;
            let (batch, updates) = state.transition(*transition);
            let op = if updates && state.hands_over {
                Op::FlagsHandoverBatch(HandoverBatch {
                    ops: batch,
                    full_handover_owners: state.sponsors.clone(),
                })
            } else {
                Op::FlagsBatch(batch)
            };
            state.histories.record(&op);
            ops.push(op);
            if ends_block(
                done,
                self.transitions,
                u64::from(self.transitions_per_block),
            ) {
                ops.push(Op::Commit);
                state.reads(&mut ops);
                block += 1;
                let epoch = block / u64::from(self.blocks_per_epoch);
                state.epoch = u16::try_from(epoch).unwrap_or(u16::MAX);
            }
        }
        Prepared::stream(ops)
    }
}

impl State {
    fn flags(&self, owner: u64) -> Option<Vec<u8>> {
        storage_flags(self.epoch, Some(&identity_id(self.seed, owner)))
    }

    fn history(&self, id: &[u8; 32]) -> &History {
        self.histories
            .get(&self.primary_path, id)
            .expect("every document is followed")
    }

    /// A document `matches` holds for, chosen at random.
    fn pick(&mut self, matches: impl Fn(&History) -> bool) -> Option<[u8; 32]> {
        let ids: Vec<[u8; 32]> = self
            .documents
            .keys()
            .filter(|id| matches(self.history(id)))
            .copied()
            .collect();
        (!ids.is_empty()).then(|| ids[self.rng.index(ids.len())])
    }

    /// The identity that pays for documents of others once documents are
    /// handed over: the one after the owners.
    fn sponsor(&self) -> u64 {
        self.owners
    }

    /// An owner other than `owner`. Once documents are handed over, one in
    /// three goes to the sponsor, and the sponsor's go to an owner.
    fn another_owner(&mut self, owner: u64) -> u64 {
        if owner == self.sponsor() {
            return self.rng.below(self.owners);
        }
        if self.hands_over && self.rng.chance(1, 3) {
            return self.sponsor();
        }
        let step = self.rng.between(1, self.owners - 1);
        ((u128::from(owner) + u128::from(step)) % u128::from(self.owners)) as u64
    }

    /// The operations of a state transition, and whether it updates a
    /// document.
    fn transition(&mut self, transition: Transition) -> (Vec<BatchOp>, bool) {
        let epoch = self.epoch;
        // A document the current epoch is later than the first of, and no
        // earlier than the latest in its flags.
        let later = move |history: &History| {
            epoch > history.base_epoch && history.later.keys().all(|later| *later <= epoch)
        };
        // The bytes an update may take off to free all the latest epoch
        // can give back and bytes of the one before it, or of the first.
        let across = |history: &History| history.freeable_across(1, SHORTEST);
        let chosen = match transition {
            Transition::Create => None,
            Transition::Grow { transfer } => self.pick(later).map(|id| {
                let growth = self.rng.between(GROWTH.0, GROWTH.1) as usize;
                (id, transfer, self.history(&id).len + growth)
            }),
            Transition::SameLength { transfer } => self
                .pick(later)
                .map(|id| (id, transfer, self.history(&id).len)),
            // Shorter by less than the latest epoch added: that epoch
            // alone gets bytes back.
            Transition::ShrinkWithinEpoch => self
                .pick(|history| later(history) && history.freeable_within_latest().is_some())
                .map(|id| {
                    let history = self.history(&id);
                    let len = history.len;
                    let (low, high) = history.freeable_within_latest().expect("picked for it");
                    let freed = self.rng.between(low as u64, high as u64);
                    (id, false, len - freed as usize)
                }),
            // Shorter by more than the latest epoch added: that epoch gives
            // back all it can and an earlier one the rest.
            Transition::ShrinkAcrossEpochs { transfer } => self
                .pick(|history| later(history) && across(history).is_some())
                .map(|id| {
                    let history = self.history(&id);
                    let len = history.len;
                    let (low, high) = across(history).expect("picked for it");
                    let freed = self.rng.between(low as u64, high as u64);
                    (id, transfer, len - freed as usize)
                }),
            // A document no later epoch has added to, made shorter.
            Transition::ShrinkYoung { transfer } => self
                .pick(|history| {
                    later(history)
                        && history.later.is_empty()
                        && history.len >= SHORTEST + CLEAR_OF_AN_EPOCH
                })
                .map(|id| {
                    let len = self.history(&id).len;
                    let shorter = self
                        .rng
                        .between(SHORTEST as u64, (len - CLEAR_OF_AN_EPOCH) as u64);
                    (id, transfer, shorter as usize)
                }),
            Transition::Delete => {
                // A document whose flags hold several epochs, if there is
                // one.
                let id = self
                    .pick(|history| !history.later.is_empty())
                    .or_else(|| self.pick(|_| true));
                let ops = match id {
                    Some(id) => self.delete(&id),
                    None => self.create(),
                };
                return (ops, false);
            }
        };
        match chosen {
            Some((id, transfer, len)) => (self.replace(&id, transfer, len), true),
            None => (self.create(), false),
        }
    }

    /// A document whose element is `len` bytes long without its flags.
    fn document(&mut self, len: usize, owner: u64) -> Element {
        let flags = self.flags(owner);
        build::item_of_len(&mut self.rng, len, flags)
    }

    fn value_path(&self, kind: &[u8]) -> Path {
        child(&self.index_path, kind)
    }

    /// The tree of references to the documents of `kind`.
    fn references_path(&self, kind: &[u8]) -> Path {
        child(&self.value_path(kind), PRIMARY_KEY)
    }

    fn create(&mut self) -> Vec<BatchOp> {
        let id = self.rng.id();
        let owner = self.rng.below(self.owners);
        let kind = format!("kind-{}", self.rng.below(self.kinds)).into_bytes();
        let len = self.rng.between(SHORTEST as u64, 240) as usize;
        let flags = self.flags(owner);
        let document = self.document(len, owner);
        let mut ops = vec![batch::insert_only(&self.primary_path, &id, document)];
        let indexed = self.indexed.entry(kind.clone()).or_default();
        let value_tree = (*indexed == 0).then(|| Element::empty_tree_with_flags(flags.clone()));
        *indexed += 1;
        index_document(
            &mut ops,
            &self.index_path,
            &kind,
            value_tree,
            &id,
            Element::new_reference_with_flags(document_reference(&id), flags.clone()),
        );
        self.documents.insert(
            id,
            Document {
                owner,
                kind,
                reference_flags: flags,
            },
        );
        ops
    }

    fn replace(&mut self, id: &[u8; 32], transfer: bool, len: usize) -> Vec<BatchOp> {
        let stored = self.documents[id].owner;
        let owner = if transfer {
            self.another_owner(stored)
        } else {
            stored
        };
        let element = self.document(len, owner);
        let references = self.references_path(&self.documents[id].kind);
        let document = self.documents.get_mut(id).expect("a stored document");
        document.owner = owner;
        vec![
            batch::replace(&self.primary_path, id, element),
            BatchOp::RefreshReference {
                path: references,
                key: id.to_vec(),
                reference: document_reference(id),
                max_hop: None,
                flags: document.reference_flags.clone(),
                non_counted: false,
                trusted: true,
            },
        ]
    }

    fn delete(&mut self, id: &[u8; 32]) -> Vec<BatchOp> {
        let document = self.documents.remove(id).expect("a stored document");
        let value_path = self.value_path(&document.kind);
        let mut ops = vec![
            batch::delete(&self.primary_path, id),
            batch::delete(&self.references_path(&document.kind), id),
        ];
        let indexed = self
            .indexed
            .get_mut(&document.kind)
            .expect("an indexed kind");
        *indexed -= 1;
        if *indexed == 0 {
            ops.push(batch::delete_tree(
                &value_path,
                PRIMARY_KEY,
                TreeType::NormalTree,
            ));
            ops.push(batch::delete_tree(
                &self.index_path,
                &document.kind,
                TreeType::NormalTree,
            ));
        }
        ops
    }

    fn reads(&mut self, ops: &mut Vec<Op>) {
        let page = |limit: u16| {
            PathQuery::new(
                self.primary_path.clone(),
                SizedQuery::new(Query::new_range_full(), Some(limit), None),
            )
        };
        ops.push(build::rows(page(16), true));
        ops.push(build::proven(page(4)));
        // The documents of one kind, through the index.
        let kind = self.indexed.iter().find(|(_, indexed)| **indexed > 0);
        if let Some((kind, _)) = kind {
            ops.push(build::proven(PathQuery::new_unsized(
                self.references_path(kind),
                Query::new_range_full(),
            )));
        }
    }
}
