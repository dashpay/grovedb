//! Identity keys, stored and searched the way Drive does.

use grovedb::{element::reference_path::ReferencePathType, Element, Query, QueryItem};

use super::{epoch_of_block, identity_id, DOMAIN_IDENTITY_KEYS};
use crate::workload::{
    build::{self, batch, child, path, storage_flags},
    op::{self, BatchOp, Op, Path},
    rng::Rng,
    Features, Prepared, Stream,
};

/// Drive keeps identities under `Identities`, which [`super::Balances`]
/// writes. This workload stores the same shape under a root tree of its own,
/// so both streams can be applied to one database.
const ROOT: &[u8] = &[36];

const REVISION_KEY: &[u8] = &[192];
const KEYS_KEY: &[u8] = &[128];
const KEY_REFERENCES_KEY: &[u8] = &[160];
const PURPOSE_AUTHENTICATION: u8 = 0;
const PURPOSE_TRANSFER: u8 = 3;
const SECURITY_LEVELS: u8 = 4;

/// What disabling a key adds to its serialization: a presence byte and the
/// time it was disabled at.
const DISABLED_AT_SIZE: i32 = 9;

/// Identity public keys with their searchable references.
///
/// Every identity has a tree of its keys and, next to it, a tree of
/// references to them by purpose and security level. An identity update
/// disables a key, which patches the stored key in place and refreshes the
/// reference to it, enables it again, or adds a key. The reads are the ones
/// Drive serves: a page of an identity's keys by limit and offset, the keys
/// of chosen purposes and security levels through conditional subqueries,
/// and keys by id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentityKeys {
    /// How many identities to create.
    pub identities: u64,
    /// How many keys an identity starts with, at most 100.
    pub keys: u32,
    /// How many identity updates follow.
    pub updates: u64,
    /// How many state transitions make a block.
    pub transitions_per_block: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for IdentityKeys {
    fn default() -> Self {
        Self {
            identities: 24,
            keys: 6,
            updates: 160,
            transitions_per_block: 10,
            features: Features::ALL,
        }
    }
}

/// One stored key.
struct Key {
    bytes: Vec<u8>,
    purpose: u8,
    security_level: u8,
    disabled: bool,
}

struct Identity {
    id: [u8; 32],
    revision: u64,
    keys: Vec<Key>,
}

impl Identity {
    fn path(&self) -> Path {
        path(&[ROOT, &self.id])
    }

    fn keys_path(&self) -> Path {
        child(&self.path(), KEYS_KEY)
    }

    fn references_path(&self) -> Path {
        child(&self.path(), KEY_REFERENCES_KEY)
    }

    /// The tree holding the reference to `key`.
    fn reference_path(&self, key: &Key) -> Path {
        let mut path = child(&self.references_path(), &[key.purpose]);
        if key.purpose == PURPOSE_AUTHENTICATION {
            path.push(vec![key.security_level]);
        }
        path
    }

    fn bump_revision(&mut self, epoch: u16) -> BatchOp {
        self.revision += 1;
        batch::replace(
            &self.path(),
            REVISION_KEY,
            Element::new_item_with_flags(
                self.revision.to_be_bytes().to_vec(),
                storage_flags(epoch, None),
            ),
        )
    }
}

/// The key of a key: its id as a varint, one byte for the ids used here.
fn key_id(index: usize) -> Vec<u8> {
    vec![u8::try_from(index).expect("an identity has fewer than 128 keys")]
}

fn key_reference(index: usize) -> ReferencePathType {
    ReferencePathType::UpstreamRootHeightReference(2, vec![KEYS_KEY.to_vec(), key_id(index)])
}

impl IdentityKeys {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.identities > 0 && self.keys > 0, "nothing to generate");
        assert!(self.keys <= 100, "an identity starts with at most 100 keys");
        assert!(self.transitions_per_block > 0, "a block needs a transition");
        let mut generator = Generator {
            config: *self,
            rng: Rng::derive(seed, DOMAIN_IDENTITY_KEYS, 0),
            read_rng: Rng::derive(seed, DOMAIN_IDENTITY_KEYS, 1),
            identities: Vec::new(),
            ops: vec![build::insert(&[], ROOT, Element::empty_tree()), Op::Commit],
            block: 1,
            reads: 0,
        };
        let per_block = u64::from(self.transitions_per_block);
        for index in 0..self.identities {
            let id = identity_id(seed, index);
            generator.create(id);
            if build::ends_block(index + 1, self.identities, per_block) {
                generator.end_block();
            }
        }
        for update in 0..self.updates {
            generator.update();
            if build::ends_block(update + 1, self.updates, per_block) {
                generator.end_block();
            }
        }
        Prepared::stream(generator.ops)
    }
}

struct Generator {
    config: IdentityKeys,
    rng: Rng,
    read_rng: Rng,
    identities: Vec<Identity>,
    ops: Vec<Op>,
    block: u64,
    reads: u64,
}

impl Generator {
    fn new_key(&mut self, index: usize) -> Key {
        let transfer = index > 0 && index % 5 == 4;
        Key {
            bytes: build::value(&mut self.rng, 40, 75),
            purpose: if transfer {
                PURPOSE_TRANSFER
            } else {
                PURPOSE_AUTHENTICATION
            },
            security_level: (index % usize::from(SECURITY_LEVELS)) as u8,
            disabled: false,
        }
    }

    /// The operations storing a key and the reference to it.
    fn store_key(identity: &Identity, index: usize, ops: &mut Vec<BatchOp>) {
        let key = &identity.keys[index];
        ops.push(batch::insert_only(
            &identity.keys_path(),
            &key_id(index),
            Element::new_item(key.bytes.clone()),
        ));
        ops.push(batch::insert_only(
            &identity.reference_path(key),
            &key_id(index),
            Element::new_reference(key_reference(index)),
        ));
    }

    fn create(&mut self, id: [u8; 32]) {
        let flags = storage_flags(epoch_of_block(self.block), None);
        let identity = Identity {
            id,
            revision: 0,
            keys: (0..self.config.keys as usize)
                .map(|index| self.new_key(index))
                .collect(),
        };
        let tree = || Element::empty_tree_with_flags(flags.clone());
        let references = identity.references_path();
        let authentication = child(&references, &[PURPOSE_AUTHENTICATION]);
        let mut ops = vec![
            batch::insert_only(&path(&[ROOT]), &id, tree()),
            batch::insert_only(
                &identity.path(),
                REVISION_KEY,
                Element::new_item_with_flags(0u64.to_be_bytes().to_vec(), flags.clone()),
            ),
            batch::insert_only(&identity.path(), KEYS_KEY, tree()),
            batch::insert_only(&identity.path(), KEY_REFERENCES_KEY, tree()),
            batch::insert_only(&references, &[PURPOSE_AUTHENTICATION], tree()),
            batch::insert_only(&references, &[PURPOSE_TRANSFER], tree()),
        ];
        for level in 0..SECURITY_LEVELS {
            ops.push(batch::insert_only(&authentication, &[level], tree()));
        }
        for index in 0..identity.keys.len() {
            Self::store_key(&identity, index, &mut ops);
        }
        self.ops.push(Op::Batch(ops));
        self.identities.push(identity);
    }

    fn update(&mut self) {
        let epoch = epoch_of_block(self.block);
        let chosen = self.rng.index(self.identities.len());
        let roll = self.rng.below(10);
        let index = self.rng.index(self.identities[chosen].keys.len());
        let trusted = self.rng.chance(1, 2);
        let added = (roll >= 7 && self.identities[chosen].keys.len() < 127)
            .then(|| self.new_key(self.identities[chosen].keys.len()));
        let identity = &mut self.identities[chosen];
        let mut ops = Vec::new();
        match added {
            Some(key) => {
                identity.keys.push(key);
                Self::store_key(identity, identity.keys.len() - 1, &mut ops);
            }
            None => {
                let key = &mut identity.keys[index];
                key.disabled = !key.disabled;
                let (stored, change_in_bytes) = if key.disabled {
                    let mut stored = key.bytes.clone();
                    stored.push(1);
                    stored.extend_from_slice(&(self.block * 3_000).to_be_bytes());
                    (stored, DISABLED_AT_SIZE)
                } else {
                    (key.bytes.clone(), -DISABLED_AT_SIZE)
                };
                let key = &identity.keys[index];
                ops.push(batch::patch(
                    &identity.keys_path(),
                    &key_id(index),
                    Element::new_item(stored),
                    change_in_bytes,
                ));
                ops.push(BatchOp::RefreshReference {
                    path: identity.reference_path(key),
                    key: key_id(index),
                    reference: key_reference(index),
                    max_hop: None,
                    flags: None,
                    non_counted: false,
                    trusted,
                });
            }
        }
        ops.push(identity.bump_revision(epoch));
        self.ops.push(Op::Batch(ops));
    }

    fn end_block(&mut self) {
        self.ops.push(Op::Commit);
        self.block += 1;
        for _ in 0..3 {
            self.read();
        }
    }

    fn read(&mut self) {
        let identity = &self.identities[self.read_rng.index(self.identities.len())];
        let keys = identity.keys.len();
        let kind = self.reads % 4;
        self.reads += 1;
        let op = match kind {
            0 => {
                let offset = self.read_rng.between(1, keys as u64) as u16;
                let limit = self.read_rng.between(1, 4) as u16;
                let query = Query::new_single_query_item_with_direction(
                    QueryItem::RangeFull(..),
                    self.read_rng.chance(2, 3),
                );
                build::rows(
                    build::sized(&identity.keys_path(), query, Some(limit), Some(offset)),
                    false,
                )
            }
            1 => {
                let wanted = self.read_rng.between(1, u64::from(SECURITY_LEVELS)) as u8;
                let mut levels = Query::new();
                for level in 0..wanted {
                    let mut keys_of_level = Query::new();
                    if level % 2 == 0 {
                        keys_of_level.insert_range_after(vec![]..);
                    } else {
                        keys_of_level.insert_key(key_id(usize::from(level)));
                    }
                    levels.insert_key(vec![level]);
                    levels.add_conditional_subquery(
                        QueryItem::Key(vec![level]),
                        None,
                        Some(keys_of_level),
                    );
                }
                let mut purposes = Query::new();
                purposes.insert_key(vec![PURPOSE_AUTHENTICATION]);
                purposes.insert_key(vec![PURPOSE_TRANSFER]);
                purposes.add_conditional_subquery(
                    QueryItem::Key(vec![PURPOSE_AUTHENTICATION]),
                    None,
                    Some(levels),
                );
                purposes.add_conditional_subquery(
                    QueryItem::Key(vec![PURPOSE_TRANSFER]),
                    None,
                    Some(Query::new_range_full()),
                );
                build::proven(build::sized(
                    &identity.references_path(),
                    purposes,
                    Some(20),
                    None,
                ))
            }
            2 => {
                let first = self.read_rng.index(keys);
                let second = self.read_rng.index(keys);
                build::query(build::select(
                    &identity.keys_path(),
                    vec![
                        op::QueryItem::Key(key_id(first.min(second))),
                        op::QueryItem::Key(key_id(first.max(second) + 1)),
                    ],
                    Some(2),
                ))
            }
            _ if self.config.features.instance_limits => {
                let mut first_of_level = Query::new_range_full();
                first_of_level.limit = Some(1);
                let mut levels = Query::new_range_full();
                levels.set_subquery(first_of_level);
                build::proven(build::sized(
                    &child(&identity.references_path(), &[PURPOSE_AUTHENTICATION]),
                    levels,
                    None,
                    None,
                ))
            }
            _ => build::get(&identity.path(), REVISION_KEY),
        };
        self.ops.push(op);
    }
}
