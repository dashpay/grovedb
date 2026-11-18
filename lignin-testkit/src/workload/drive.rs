//! Workloads shaped like what Dash Platform's Drive stores.
//!
//! The paths, element types, reference shape and storage flags follow Drive:
//! documents under `DataContractDocuments / contract / 1 / type / 0` with a
//! reference per document in an index tree, identity balances as sum items
//! in one sum tree, and token balances as sum items in a count-sum tree per
//! token under a big sum tree. Each state transition is one batch.
//!
//! The submodules hold the shapes around the operations that reach storage
//! in a way of their own:
//!
//! - [`IdentityKeys`]: keys patched in place, searched through conditional
//!   subqueries and paged by offset.
//! - [`PlatformState`]: the records Drive keeps in auxiliary storage.
//! - [`IndexRekey`]: the value keys of an index moved to another encoding.
//! - [`Expiration`]: index entries dropped flat, which writes meta records.
//! - [`IndexAggregates`]: indexes that count, sum and rank, read by
//!   aggregate queries.

mod aggregates;
mod expiry;
mod keys;
mod rekey;
mod state;

use std::collections::BTreeMap;

pub use self::{
    aggregates::IndexAggregates, expiry::Expiration, keys::IdentityKeys, rekey::IndexRekey,
    state::PlatformState,
};

use grovedb::{element::reference_path::ReferencePathType, Element, TreeType};

use super::{
    build::{self, batch, child, path, storage_flags},
    op::{BatchOp, Op, Path, Query, QueryItem},
    rng::Rng,
    Source, Stream,
};

const DOMAIN_DOCUMENTS: u64 = 0xD0C0;
const DOMAIN_CONTRACT: u64 = 0xD0C1;
const DOMAIN_DOCUMENT_ID: u64 = 0xD0C2;
const DOMAIN_DOCUMENT_OWNER: u64 = 0xD0C3;
const DOMAIN_DOCUMENT_BODY: u64 = 0xD0C4;
const DOMAIN_DOCUMENT_UPDATE: u64 = 0xD0C5;
const DOMAIN_DOCUMENT_READS: u64 = 0xD0C6;
const DOMAIN_IDENTITY: u64 = 0x1D00;
const DOMAIN_BALANCES: u64 = 0xBA1A;
const DOMAIN_BALANCE_READS: u64 = 0xBA1B;
const DOMAIN_TOKENS: u64 = 0x70C0;
const DOMAIN_TOKEN_ID: u64 = 0x70C1;
const DOMAIN_TOKEN_READS: u64 = 0x70C2;
const DOMAIN_IDENTITY_KEYS: u64 = 0x1D01;
const DOMAIN_PLATFORM_STATE: u64 = 0x57A7;
const DOMAIN_INDEX_REKEY: u64 = 0xD0C7;
const DOMAIN_EXPIRATION: u64 = 0xD0C8;
const DOMAIN_INDEX_AGGREGATES: u64 = 0xD0C9;

/// Drive's root tree keys.
const ROOT_DOCUMENTS: &[u8] = &[64];
const ROOT_IDENTITIES: &[u8] = &[32];
const ROOT_BALANCES: &[u8] = &[96];
const ROOT_TOKENS: &[u8] = &[16];

const CONTRACT_KEY: &[u8] = &[0];
const CONTRACT_DOCUMENTS_KEY: &[u8] = &[1];
const DOCUMENT_TYPE: &[u8] = b"note";
const PRIMARY_KEY: &[u8] = &[0];
const OWNER_INDEX: &[u8] = b"$ownerId";
const IDENTITY_NONCE_KEY: &[u8] = &[64];
const TOKEN_BALANCES_KEY: &[u8] = &[128];
const TOKEN_STATUS_KEY: &[u8] = &[64];
const TOKEN_HISTORY_KEY: &[u8] = &[192];

const BLOCKS_PER_EPOCH: u64 = 64;

fn epoch_of_block(block: u64) -> u16 {
    ((block / BLOCKS_PER_EPOCH) % 1024) as u16
}

/// The 32-byte id of the `index`-th identity of `seed`.
pub fn identity_id(seed: u64, index: u64) -> [u8; 32] {
    Rng::derive(seed, DOMAIN_IDENTITY, index).id()
}

/// The inserts creating, under the root tree `root`, the contract `contract`
/// with one document type, its primary key tree and an empty tree for each
/// of `indexes`.
fn document_type_trees(
    root: &[u8],
    contract: &[u8; 32],
    document_type: &[u8],
    indexes: &[&[u8]],
) -> Vec<Op> {
    let flags = storage_flags(0, None);
    let tree = || Element::empty_tree_with_flags(flags.clone());
    let contract_path = path(&[root, contract]);
    let type_path = path(&[root, contract, CONTRACT_DOCUMENTS_KEY, document_type]);
    let mut ops = vec![
        build::insert(&[], root, Element::empty_tree()),
        build::insert(&path(&[root]), contract, tree()),
        build::insert(&contract_path, CONTRACT_DOCUMENTS_KEY, tree()),
        build::insert(
            &child(&contract_path, CONTRACT_DOCUMENTS_KEY),
            document_type,
            tree(),
        ),
        build::insert(&type_path, PRIMARY_KEY, tree()),
    ];
    for index in indexes {
        ops.push(build::insert(&type_path, index, tree()));
    }
    ops
}

/// The path from an index entry's tree of references to the document `id`
/// in the primary key tree.
fn document_reference(id: &[u8; 32]) -> ReferencePathType {
    ReferencePathType::UpstreamRootHeightReference(4, vec![PRIMARY_KEY.to_vec(), id.to_vec()])
}

/// Adds `reference` to the document `id` under the value key `value_key` of
/// the index tree at `index_path`. When `value_tree` is given, the value
/// key is new: its tree and the tree of references under it are created as
/// `value_tree` first.
fn index_document(
    ops: &mut Vec<BatchOp>,
    index_path: &[Vec<u8>],
    value_key: &[u8],
    value_tree: Option<Element>,
    id: &[u8; 32],
    reference: Element,
) {
    let value_path = child(index_path, value_key);
    if let Some(value_tree) = value_tree {
        ops.push(batch::insert_only(
            index_path,
            value_key,
            value_tree.clone(),
        ));
        ops.push(batch::insert_only(&value_path, PRIMARY_KEY, value_tree));
    }
    ops.push(batch::insert_only(
        &child(&value_path, PRIMARY_KEY),
        id,
        reference,
    ));
}

/// Documents with one index reference each.
///
/// The stream sets up one contract with one document type, then creates
/// `documents` documents, one per state transition: the document as an item
/// in the primary key tree and a reference to it in the `$ownerId` index.
/// It then replaces `updates` documents, refreshing their references, and
/// deletes `deletes` of them, removing index trees that become empty.
///
/// [`Documents::reference`] is the workload the plan's measurements were
/// taken on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Documents {
    /// How many documents to create.
    pub documents: u64,
    /// How many distinct owners the documents are spread over.
    pub owners: u64,
    /// The size of a document body in bytes.
    pub document_size: u32,
    /// How many state transitions make a block.
    pub transitions_per_block: u32,
    /// How many document replacements follow the creates.
    pub updates: u64,
    /// How many document deletions follow the updates; at most `documents`.
    pub deletes: u64,
    /// How many reads follow each block. Reads draw from a generator of
    /// their own, so this changes no write.
    pub reads_per_block: u32,
}

impl Default for Documents {
    fn default() -> Self {
        Self {
            documents: 600,
            owners: 12,
            document_size: 400,
            transitions_per_block: 20,
            updates: 120,
            deletes: 240,
            reads_per_block: 4,
        }
    }
}

impl Documents {
    /// One million 400-byte documents over ten thousand owners, one document
    /// and one reference per state transition, twenty transitions per block,
    /// writes only.
    pub fn reference() -> Self {
        Self {
            documents: 1_000_000,
            owners: 10_000,
            document_size: 400,
            transitions_per_block: 20,
            updates: 0,
            deletes: 0,
            reads_per_block: 0,
        }
    }

    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.documents > 0 && self.owners > 0, "nothing to generate");
        assert!(self.transitions_per_block > 0, "a block needs a transition");
        assert!(
            self.deletes <= self.documents,
            "more deletes than documents"
        );
        let mut rng = Rng::derive(seed, DOMAIN_DOCUMENTS, 0);
        let stride = coprime_stride(&mut rng, self.documents);
        let start = rng.below(self.documents);
        Stream::new(DocumentsSource {
            config: *self,
            seed,
            rng,
            read_rng: Rng::derive(seed, DOMAIN_DOCUMENT_READS, 0),
            phase: Phase::Setup,
            block: 0,
            created: 0,
            updated: 0,
            deleted: 0,
            reads: 0,
            stride,
            start,
            live_per_owner: BTreeMap::new(),
        })
    }

    /// The contract's id.
    pub fn contract_id(&self, seed: u64) -> [u8; 32] {
        Rng::derive(seed, DOMAIN_CONTRACT, 0).id()
    }

    /// The id of the `index`-th document.
    pub fn document_id(&self, seed: u64, index: u64) -> [u8; 32] {
        Rng::derive(seed, DOMAIN_DOCUMENT_ID, index).id()
    }

    /// Which owner, `0..owners`, the `index`-th document belongs to.
    pub fn owner_of(&self, seed: u64, index: u64) -> u64 {
        Rng::derive(seed, DOMAIN_DOCUMENT_OWNER, index).below(self.owners)
    }

    /// The path of the document type tree.
    pub fn document_type_path(&self, seed: u64) -> Path {
        let contract = self.contract_id(seed);
        path(&[
            ROOT_DOCUMENTS,
            &contract,
            CONTRACT_DOCUMENTS_KEY,
            DOCUMENT_TYPE,
        ])
    }

    /// The path of the tree holding the documents, keyed by document id.
    pub fn primary_path(&self, seed: u64) -> Path {
        child(&self.document_type_path(seed), PRIMARY_KEY)
    }

    /// The path of the tree holding one owner's references, keyed by
    /// document id.
    pub fn owner_references_path(&self, seed: u64, owner: u64) -> Path {
        let mut path = self.document_type_path(seed);
        path.push(OWNER_INDEX.to_vec());
        path.push(identity_id(seed, owner).to_vec());
        path.push(PRIMARY_KEY.to_vec());
        path
    }

    /// The query for one document by id.
    pub fn point_query(&self, seed: u64, index: u64) -> Query {
        build::select(
            &self.primary_path(seed),
            vec![QueryItem::Key(self.document_id(seed, index).to_vec())],
            None,
        )
    }

    /// The query for up to `limit` documents of one owner, read through the
    /// index references.
    pub fn owner_query(&self, seed: u64, owner: u64, limit: u16) -> Query {
        build::select(
            &self.owner_references_path(seed, owner),
            vec![QueryItem::RangeFull],
            Some(limit),
        )
    }

    fn creation_epoch(&self, index: u64) -> u16 {
        epoch_of_block(1 + index / u64::from(self.transitions_per_block))
    }

    fn index_reference(
        &self,
        seed: u64,
        index: u64,
    ) -> (ReferencePathType, Option<u8>, Option<Vec<u8>>) {
        let owner = identity_id(seed, self.owner_of(seed, index));
        (
            ReferencePathType::UpstreamRootHeightReference(
                4,
                vec![PRIMARY_KEY.to_vec(), self.document_id(seed, index).to_vec()],
            ),
            Some(1),
            storage_flags(self.creation_epoch(index), Some(&owner)),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Setup,
    Create,
    Update,
    Delete,
    Done,
}

struct DocumentsSource {
    config: Documents,
    seed: u64,
    rng: Rng,
    read_rng: Rng,
    phase: Phase,
    block: u64,
    created: u64,
    updated: u64,
    deleted: u64,
    reads: u64,
    stride: u64,
    start: u64,
    live_per_owner: BTreeMap<u64, u64>,
}

impl DocumentsSource {
    /// The document deleted `position`-th. Every document appears once.
    fn deletion_order(&self, position: u64) -> u64 {
        let documents = u128::from(self.config.documents);
        let index =
            (u128::from(self.start) + u128::from(position) * u128::from(self.stride)) % documents;
        index as u64
    }

    fn setup(&self, out: &mut Vec<Op>) {
        let contract = self.config.contract_id(self.seed);
        let contract_path = path(&[ROOT_DOCUMENTS, &contract]);
        let documents_path = child(&contract_path, CONTRACT_DOCUMENTS_KEY);
        let type_path = self.config.document_type_path(self.seed);
        let flags = storage_flags(0, None);
        let mut rng = Rng::derive(self.seed, DOMAIN_CONTRACT, 1);
        out.push(build::insert(&[], ROOT_DOCUMENTS, Element::empty_tree()));
        out.push(build::insert(
            &path(&[ROOT_DOCUMENTS]),
            &contract,
            Element::empty_tree_with_flags(flags.clone()),
        ));
        out.push(build::insert(
            &contract_path,
            CONTRACT_KEY,
            Element::new_item_with_flags(rng.bytes(320), flags.clone()),
        ));
        out.push(build::insert(
            &contract_path,
            CONTRACT_DOCUMENTS_KEY,
            Element::empty_tree_with_flags(flags.clone()),
        ));
        out.push(build::insert(
            &documents_path,
            DOCUMENT_TYPE,
            Element::empty_tree_with_flags(flags.clone()),
        ));
        out.push(build::insert(
            &type_path,
            PRIMARY_KEY,
            Element::empty_tree_with_flags(flags.clone()),
        ));
        out.push(build::insert(
            &type_path,
            OWNER_INDEX,
            Element::empty_tree_with_flags(flags),
        ));
    }

    fn create(&mut self, index: u64) -> Op {
        let config = self.config;
        let seed = self.seed;
        let epoch = epoch_of_block(self.block);
        let id = config.document_id(seed, index);
        let owner_index = config.owner_of(seed, index);
        let owner = identity_id(seed, owner_index);
        let body = Rng::derive(seed, DOMAIN_DOCUMENT_BODY, index).bytes(config.document_size);
        let mut ops = vec![batch::insert_only(
            &config.primary_path(seed),
            &id,
            Element::new_item_with_flags(body, storage_flags(epoch, Some(&owner))),
        )];
        let references = config.owner_references_path(seed, owner_index);
        let live = self.live_per_owner.entry(owner_index).or_insert(0);
        if *live == 0 {
            let owner_tree = &references[..references.len() - 1];
            let index_tree = &owner_tree[..owner_tree.len() - 1];
            let flags = storage_flags(epoch, None);
            ops.push(batch::insert_only(
                index_tree,
                &owner,
                Element::empty_tree_with_flags(flags.clone()),
            ));
            ops.push(batch::insert_only(
                owner_tree,
                PRIMARY_KEY,
                Element::empty_tree_with_flags(flags),
            ));
        }
        *live += 1;
        let (reference, max_hop, flags) = config.index_reference(seed, index);
        ops.push(batch::insert_only(
            &references,
            &id,
            Element::Reference(reference, max_hop, flags),
        ));
        Op::Batch(ops)
    }

    fn update(&mut self, update: u64) -> Op {
        let config = self.config;
        let seed = self.seed;
        let index = self.rng.below(config.documents);
        let id = config.document_id(seed, index);
        let owner_index = config.owner_of(seed, index);
        let owner = identity_id(seed, owner_index);
        let body = Rng::derive(seed, DOMAIN_DOCUMENT_UPDATE, update).bytes(config.document_size);
        let (reference, max_hop, flags) = config.index_reference(seed, index);
        Op::Batch(vec![
            batch::replace(
                &config.primary_path(seed),
                &id,
                Element::new_item_with_flags(
                    body,
                    storage_flags(config.creation_epoch(index), Some(&owner)),
                ),
            ),
            BatchOp::RefreshReference {
                path: config.owner_references_path(seed, owner_index),
                key: id.to_vec(),
                reference,
                max_hop,
                flags,
                non_counted: false,
                trusted: true,
            },
        ])
    }

    fn delete(&mut self, position: u64) -> Op {
        let config = self.config;
        let seed = self.seed;
        let index = self.deletion_order(position);
        let id = config.document_id(seed, index);
        let owner_index = config.owner_of(seed, index);
        let owner = identity_id(seed, owner_index);
        let references = config.owner_references_path(seed, owner_index);
        let mut ops = vec![
            batch::delete(&references, &id),
            batch::delete(&config.primary_path(seed), &id),
        ];
        let live = self
            .live_per_owner
            .get_mut(&owner_index)
            .expect("a deleted document was created");
        *live -= 1;
        if *live == 0 {
            self.live_per_owner.remove(&owner_index);
            let owner_tree = &references[..references.len() - 1];
            let index_tree = &owner_tree[..owner_tree.len() - 1];
            ops.push(batch::delete_tree(
                owner_tree,
                PRIMARY_KEY,
                TreeType::NormalTree,
            ));
            ops.push(batch::delete_tree(index_tree, &owner, TreeType::NormalTree));
        }
        Op::Batch(ops)
    }

    /// A document that exists once the current block is committed.
    fn live_document(&mut self) -> Option<u64> {
        match self.phase {
            Phase::Setup | Phase::Done => None,
            Phase::Create => (self.created > 0).then(|| self.read_rng.below(self.created)),
            Phase::Update => Some(self.read_rng.below(self.config.documents)),
            Phase::Delete => (self.deleted < self.config.documents).then(|| {
                let position = self
                    .read_rng
                    .between(self.deleted, self.config.documents - 1);
                self.deletion_order(position)
            }),
        }
    }

    fn reads(&mut self, out: &mut Vec<Op>) {
        let config = self.config;
        let seed = self.seed;
        for _ in 0..config.reads_per_block {
            let Some(index) = self.live_document() else {
                return;
            };
            let owner = config.owner_of(seed, index);
            let kind = self.reads % 4;
            self.reads += 1;
            out.push(match kind {
                0 => build::get(&config.primary_path(seed), &config.document_id(seed, index)),
                1 => build::query(config.point_query(seed, index)),
                2 => build::query(config.owner_query(seed, owner, 100)),
                _ => {
                    let references = config.owner_references_path(seed, owner);
                    let index_tree = &references[..references.len() - 2];
                    build::query(Query {
                        subquery_path: Some(vec![PRIMARY_KEY.to_vec()]),
                        subquery_all: true,
                        ..build::select(
                            index_tree,
                            vec![QueryItem::Key(identity_id(seed, owner).to_vec())],
                            Some(100),
                        )
                    })
                }
            });
        }
    }
}

impl Source for DocumentsSource {
    fn fill(&mut self, out: &mut Vec<Op>) -> bool {
        let per_block = u64::from(self.config.transitions_per_block);
        match self.phase {
            Phase::Setup => self.setup(out),
            Phase::Create => {
                let end = (self.created + per_block).min(self.config.documents);
                for index in self.created..end {
                    out.push(self.create(index));
                }
                self.created = end;
            }
            Phase::Update => {
                let end = (self.updated + per_block).min(self.config.updates);
                for update in self.updated..end {
                    out.push(self.update(update));
                }
                self.updated = end;
            }
            Phase::Delete => {
                let end = (self.deleted + per_block).min(self.config.deletes);
                for position in self.deleted..end {
                    out.push(self.delete(position));
                }
                self.deleted = end;
            }
            Phase::Done => return false,
        }
        out.push(Op::Commit);
        self.reads(out);
        self.block += 1;
        self.phase = match self.phase {
            Phase::Setup => Phase::Create,
            Phase::Create if self.created < self.config.documents => Phase::Create,
            Phase::Create | Phase::Update if self.updated < self.config.updates => Phase::Update,
            Phase::Create | Phase::Update | Phase::Delete if self.deleted < self.config.deletes => {
                Phase::Delete
            }
            _ => Phase::Done,
        };
        self.phase != Phase::Done
    }
}

/// A uniform position in `0..count` other than `this`. `count` must be at
/// least two.
fn another(rng: &mut Rng, count: u64, this: u64) -> u64 {
    let offset = 1 + u128::from(rng.below(count - 1));
    ((u128::from(this) + offset) % u128::from(count)) as u64
}

/// A step size that visits every one of `count` positions once.
fn coprime_stride(rng: &mut Rng, count: u64) -> u64 {
    if count <= 2 {
        return 1;
    }
    loop {
        let stride = rng.between(1, count - 1);
        if gcd(stride, count) == 1 {
            return stride;
        }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Identity balances in a sum tree.
///
/// Each identity is created by one state transition: its balance as a sum
/// item under `Balances`, and its own tree under `Identities` holding a
/// nonce. A transfer then replaces the sender's and the recipient's balance
/// and the sender's nonce; a sender with nothing to send is topped up
/// instead, which replaces only its own balance and nonce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Balances {
    /// How many identities to create.
    pub identities: u64,
    /// How many transfers follow.
    pub transfers: u64,
    /// How many state transitions make a block.
    pub transitions_per_block: u32,
    /// How many reads follow each block. Reads draw from a generator of
    /// their own, so this changes no write.
    pub reads_per_block: u32,
}

impl Default for Balances {
    fn default() -> Self {
        Self {
            identities: 200,
            transfers: 600,
            transitions_per_block: 20,
            reads_per_block: 3,
        }
    }
}

impl Balances {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.identities > 0, "nothing to generate");
        assert!(self.transitions_per_block > 0, "a block needs a transition");
        Stream::new(BalancesSource {
            config: *self,
            seed,
            rng: Rng::derive(seed, DOMAIN_BALANCES, 0),
            read_rng: Rng::derive(seed, DOMAIN_BALANCE_READS, 0),
            started: false,
            block: 0,
            balances: Vec::new(),
            nonces: Vec::new(),
            transferred: 0,
            reads: 0,
        })
    }
}

struct BalancesSource {
    config: Balances,
    seed: u64,
    rng: Rng,
    read_rng: Rng,
    started: bool,
    block: u64,
    balances: Vec<i64>,
    nonces: Vec<u64>,
    transferred: u64,
    reads: u64,
}

impl BalancesSource {
    fn create(&mut self, index: u64) -> Op {
        let id = identity_id(self.seed, index);
        let balance = self.rng.between(1_000, 1_000_000_000) as i64;
        self.balances.push(balance);
        self.nonces.push(0);
        let flags = storage_flags(epoch_of_block(self.block), None);
        Op::Batch(vec![
            batch::insert_only(&path(&[ROOT_BALANCES]), &id, Element::new_sum_item(balance)),
            batch::insert_only(
                &path(&[ROOT_IDENTITIES]),
                &id,
                Element::empty_tree_with_flags(flags.clone()),
            ),
            batch::insert_only(
                &path(&[ROOT_IDENTITIES, &id]),
                IDENTITY_NONCE_KEY,
                Element::new_item_with_flags(0u64.to_be_bytes().to_vec(), flags),
            ),
        ])
    }

    fn transfer(&mut self) -> Op {
        let identities = self.balances.len() as u64;
        let sender = self.rng.below(identities);
        let recipient = if identities > 1 {
            another(&mut self.rng, identities, sender)
        } else {
            sender
        };
        let sender_id = identity_id(self.seed, sender);
        let balances_path = path(&[ROOT_BALANCES]);
        let available = self.balances[sender as usize];
        let mut ops = Vec::new();
        if available == 0 || recipient == sender {
            let top_up = self.rng.between(1_000, 1_000_000) as i64;
            self.balances[sender as usize] += top_up;
        } else {
            let amount = self.rng.between(1, available as u64) as i64;
            self.balances[sender as usize] -= amount;
            self.balances[recipient as usize] += amount;
            ops.push(batch::replace(
                &balances_path,
                &identity_id(self.seed, recipient),
                Element::new_sum_item(self.balances[recipient as usize]),
            ));
        }
        ops.push(batch::replace(
            &balances_path,
            &sender_id,
            Element::new_sum_item(self.balances[sender as usize]),
        ));
        self.nonces[sender as usize] += 1;
        ops.push(batch::replace(
            &path(&[ROOT_IDENTITIES, &sender_id]),
            IDENTITY_NONCE_KEY,
            Element::new_item_with_flags(
                self.nonces[sender as usize].to_be_bytes().to_vec(),
                storage_flags(epoch_of_block(self.block), None),
            ),
        ));
        Op::Batch(ops)
    }

    fn reads(&mut self, out: &mut Vec<Op>) {
        if self.balances.is_empty() {
            return;
        }
        let balances_path = path(&[ROOT_BALANCES]);
        for _ in 0..self.config.reads_per_block {
            let id = identity_id(self.seed, self.read_rng.below(self.balances.len() as u64));
            let kind = self.reads % 3;
            self.reads += 1;
            out.push(match kind {
                0 => build::get(&balances_path, &id),
                1 => build::query(build::select(
                    &balances_path,
                    vec![QueryItem::RangeFrom(id.to_vec())],
                    Some(10),
                )),
                _ => build::get(&path(&[ROOT_IDENTITIES, &id]), IDENTITY_NONCE_KEY),
            });
        }
    }
}

impl Source for BalancesSource {
    fn fill(&mut self, out: &mut Vec<Op>) -> bool {
        let per_block = u64::from(self.config.transitions_per_block);
        let created = self.balances.len() as u64;
        if !self.started {
            self.started = true;
            out.push(build::insert(&[], ROOT_BALANCES, Element::empty_sum_tree()));
            out.push(build::insert(&[], ROOT_IDENTITIES, Element::empty_tree()));
        } else if created < self.config.identities {
            for index in created..(created + per_block).min(self.config.identities) {
                out.push(self.create(index));
            }
        } else if self.transferred < self.config.transfers {
            let end = (self.transferred + per_block).min(self.config.transfers);
            for _ in self.transferred..end {
                out.push(self.transfer());
            }
            self.transferred = end;
        } else {
            return false;
        }
        out.push(Op::Commit);
        self.reads(out);
        self.block += 1;
        (self.balances.len() as u64) < self.config.identities
            || self.transferred < self.config.transfers
    }
}

/// Tokens in count trees.
///
/// Every token has a count-sum tree of holder balances (the count is the
/// number of holders, the sum the supply) under one big sum tree, a status
/// item, and a provable count tree logging its actions. An action mints,
/// transfers or burns: balances are inserted, replaced and, when they reach
/// zero, deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tokens {
    /// How many tokens to create.
    pub tokens: u32,
    /// How many identities may hold a token.
    pub holders: u64,
    /// How many mint, transfer and burn actions follow.
    pub actions: u64,
    /// How many state transitions make a block.
    pub transitions_per_block: u32,
    /// How many reads follow each block. Reads draw from a generator of
    /// their own, so this changes no write.
    pub reads_per_block: u32,
}

impl Default for Tokens {
    fn default() -> Self {
        Self {
            tokens: 4,
            holders: 60,
            actions: 800,
            transitions_per_block: 20,
            reads_per_block: 4,
        }
    }
}

impl Tokens {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.tokens > 0 && self.holders > 0, "nothing to generate");
        assert!(self.transitions_per_block > 0, "a block needs a transition");
        Stream::new(TokensSource {
            config: *self,
            seed,
            rng: Rng::derive(seed, DOMAIN_TOKENS, 0),
            read_rng: Rng::derive(seed, DOMAIN_TOKEN_READS, 0),
            started: false,
            block: 0,
            tokens: Vec::new(),
            acted: 0,
            reads: 0,
        })
    }
}

struct TokenState {
    id: [u8; 32],
    balances: BTreeMap<u64, i64>,
    actions: u64,
}

struct TokensSource {
    config: Tokens,
    seed: u64,
    rng: Rng,
    read_rng: Rng,
    started: bool,
    block: u64,
    tokens: Vec<TokenState>,
    acted: u64,
    reads: u64,
}

impl TokensSource {
    fn balances_path(id: &[u8; 32]) -> Path {
        path(&[ROOT_TOKENS, TOKEN_BALANCES_KEY, id])
    }

    fn history_path(id: &[u8; 32]) -> Path {
        path(&[ROOT_TOKENS, TOKEN_HISTORY_KEY, id])
    }

    fn create(&mut self, index: u64) -> Op {
        let id = Rng::derive(self.seed, DOMAIN_TOKEN_ID, index).id();
        self.tokens.push(TokenState {
            id,
            balances: BTreeMap::new(),
            actions: 0,
        });
        let flags = storage_flags(epoch_of_block(self.block), None);
        Op::Batch(vec![
            batch::insert_only(
                &path(&[ROOT_TOKENS, TOKEN_BALANCES_KEY]),
                &id,
                Element::empty_count_sum_tree(),
            ),
            batch::insert_only(
                &path(&[ROOT_TOKENS, TOKEN_STATUS_KEY]),
                &id,
                Element::new_item_with_flags(vec![0], flags),
            ),
            batch::insert_only(
                &path(&[ROOT_TOKENS, TOKEN_HISTORY_KEY]),
                &id,
                Element::empty_provable_count_tree(),
            ),
        ])
    }

    /// The holder at or after a random position, wrapping to the first.
    fn existing_holder(balances: &BTreeMap<u64, i64>, holders: u64, rng: &mut Rng) -> Option<u64> {
        let from = rng.below(holders);
        balances
            .range(from..)
            .next()
            .or_else(|| balances.iter().next())
            .map(|(&holder, _)| holder)
    }

    /// A holder's balance, zero for an identity holding none.
    fn balance(&self, token: usize, holder: u64) -> i64 {
        self.tokens[token]
            .balances
            .get(&holder)
            .copied()
            .unwrap_or(0)
    }

    /// Sets a holder's balance, with the batch operation that does it.
    fn set_balance(&mut self, token: usize, holder: u64, balance: i64) -> BatchOp {
        let state = &mut self.tokens[token];
        let balances_path = Self::balances_path(&state.id);
        let key = identity_id(self.seed, holder);
        if balance == 0 {
            state.balances.remove(&holder);
            batch::delete(&balances_path, &key)
        } else if state.balances.insert(holder, balance).is_some() {
            batch::replace(&balances_path, &key, Element::new_sum_item(balance))
        } else {
            batch::insert_only(&balances_path, &key, Element::new_sum_item(balance))
        }
    }

    fn act(&mut self) -> Op {
        let token = self.rng.index(self.tokens.len());
        let roll = self.rng.below(20);
        let source = if roll < 8 {
            None
        } else {
            Self::existing_holder(
                &self.tokens[token].balances,
                self.config.holders,
                &mut self.rng,
            )
        };
        let mut ops = Vec::new();
        let mut record = Vec::with_capacity(73);
        match source {
            None => {
                let holder = self.rng.below(self.config.holders);
                let amount = self.rng.between(1, 1_000_000) as i64;
                let balance = self.balance(token, holder);
                ops.push(self.set_balance(token, holder, balance + amount));
                record.push(0);
                record.extend_from_slice(&identity_id(self.seed, holder));
                record.extend_from_slice(&amount.to_be_bytes());
            }
            Some(holder) => {
                let balance = self.tokens[token].balances[&holder];
                let amount = if self.rng.chance(1, 3) {
                    balance
                } else {
                    self.rng.between(1, balance as u64) as i64
                };
                ops.push(self.set_balance(token, holder, balance - amount));
                let transfer = roll < 17 && self.config.holders > 1;
                record.push(if transfer { 1 } else { 2 });
                record.extend_from_slice(&identity_id(self.seed, holder));
                if transfer {
                    let recipient = another(&mut self.rng, self.config.holders, holder);
                    let held = self.balance(token, recipient);
                    ops.push(self.set_balance(token, recipient, held + amount));
                    record.extend_from_slice(&identity_id(self.seed, recipient));
                }
                record.extend_from_slice(&amount.to_be_bytes());
            }
        }
        let state = &mut self.tokens[token];
        ops.push(batch::insert_only(
            &Self::history_path(&state.id),
            &state.actions.to_be_bytes(),
            Element::new_item_with_flags(record, storage_flags(epoch_of_block(self.block), None)),
        ));
        state.actions += 1;
        Op::Batch(ops)
    }

    fn reads(&mut self, out: &mut Vec<Op>) {
        if self.tokens.is_empty() {
            return;
        }
        for _ in 0..self.config.reads_per_block {
            let token = self.read_rng.index(self.tokens.len());
            let id = self.tokens[token].id;
            let kind = self.reads % 4;
            self.reads += 1;
            match kind {
                0 => {
                    let holder = Self::existing_holder(
                        &self.tokens[token].balances,
                        self.config.holders,
                        &mut self.read_rng,
                    );
                    if let Some(holder) = holder {
                        out.push(build::get(
                            &Self::balances_path(&id),
                            &identity_id(self.seed, holder),
                        ));
                    }
                }
                1 => out.push(build::query(build::select(
                    &Self::balances_path(&id),
                    vec![QueryItem::RangeFull],
                    Some(20),
                ))),
                2 => out.push(build::query(Query {
                    left_to_right: false,
                    ..build::select(
                        &Self::history_path(&id),
                        vec![QueryItem::RangeFull],
                        Some(10),
                    )
                })),
                _ => out.push(build::get(&path(&[ROOT_TOKENS, TOKEN_BALANCES_KEY]), &id)),
            }
        }
    }
}

impl Source for TokensSource {
    fn fill(&mut self, out: &mut Vec<Op>) -> bool {
        let per_block = u64::from(self.config.transitions_per_block);
        let created = self.tokens.len() as u64;
        let wanted = u64::from(self.config.tokens);
        if !self.started {
            self.started = true;
            let root = path(&[ROOT_TOKENS]);
            out.push(build::insert(&[], ROOT_TOKENS, Element::empty_tree()));
            out.push(build::insert(
                &root,
                TOKEN_BALANCES_KEY,
                Element::empty_big_sum_tree(),
            ));
            out.push(build::insert(
                &root,
                TOKEN_STATUS_KEY,
                Element::empty_tree(),
            ));
            out.push(build::insert(
                &root,
                TOKEN_HISTORY_KEY,
                Element::empty_tree(),
            ));
        } else if created < wanted {
            for index in created..(created + per_block).min(wanted) {
                out.push(self.create(index));
            }
        } else if self.acted < self.config.actions {
            let end = (self.acted + per_block).min(self.config.actions);
            for _ in self.acted..end {
                out.push(self.act());
            }
            self.acted = end;
        } else {
            return false;
        }
        out.push(Op::Commit);
        self.reads(out);
        self.block += 1;
        (self.tokens.len() as u64) < wanted || self.acted < self.config.actions
    }
}
