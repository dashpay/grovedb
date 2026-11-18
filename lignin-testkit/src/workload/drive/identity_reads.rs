//! Identities read through the query entry points Drive answers its
//! requests with.

use grovedb::{Element, PathQuery, Query, QueryItem, SizedQuery};

use super::{epoch_of_block, DOMAIN_IDENTITY_READS};
use crate::workload::{
    build::{batch, child, ends_block, path, prove, storage_flags, value},
    op::{BatchOp, Op, Path, Prove, Read},
    rng::Rng,
    Features, Prepared, Stream,
};

/// Root trees of this workload's own, in the places of Drive's `Identities`,
/// `Balances` and `Pools`.
const ROOT_IDENTITIES: &[u8] = &[74];
const ROOT_BALANCES: &[u8] = &[76];
const ROOT_POOLS: &[u8] = &[78];

const KEYS_KEY: &[u8] = &[128];
const TOTALS: &[u8] = b"totals";
const EPOCHS: &[u8] = b"epochs";

/// The most keys an identity is given.
const MAX_KEYS: u64 = 6;
/// Credits come in amounts whose sum outgrows 64 bits.
const LARGE_CREDITS: i64 = i64::MAX / 2;

/// Identities, their keys and their balances, read the way Drive reads
/// them.
///
/// An identity is a tree holding a tree of its public keys, each an item
/// under its key id, and a sum item in the tree of balances. Fee pools hold
/// credits in trees whose 128-bit sum the tree commits to, one for the
/// totals and one for each epoch. Blocks create identities, add keys and
/// change balances. After each block the state is read through the entry
/// points that return something other than rows:
///
/// - **Keys by id.** Drive asks for specific keys of an identity and gets
///   an answer for each, empty where the identity has no such key:
///   `query_keys_optional` and `query_raw_keys_optional`. The same query is
///   proven, and the proof shows that the missing keys are missing.
/// - **Values.** `query_item_value` returns the keys of an identity as
///   bytes, a page of them at an offset. `query_item_value_or_sum` returns
///   balances as numbers and keys as bytes. `query_sums` returns balances.
/// - **Several queries at once.** The keys of two identities are read by
///   `query_many_raw` and proven by `prove_query_many`.
/// - **Big sums.** The credits of the pools are summed over a range of
///   keys, with and without their count, for one tree and for the tree of
///   each epoch, where the grove version stores a tree that commits to a
///   128-bit sum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentityReads {
    /// How many identities to create.
    pub identities: u64,
    /// How many balance changes and added keys follow.
    pub updates: u64,
    /// How many state transitions make a block.
    pub transitions_per_block: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for IdentityReads {
    fn default() -> Self {
        Self {
            identities: 24,
            updates: 40,
            transitions_per_block: 8,
            features: Features::ALL,
        }
    }
}

struct Identity {
    id: [u8; 32],
    keys: u64,
}

struct State {
    rng: Rng,
    features: Features,
    identities: Vec<Identity>,
    pools: u64,
    block: u64,
}

fn key_id(index: u64) -> Vec<u8> {
    vec![index as u8]
}

fn keys_path(id: &[u8; 32]) -> Path {
    path(&[ROOT_IDENTITIES, id, KEYS_KEY])
}

fn keys(keys: impl IntoIterator<Item = Vec<u8>>) -> Query {
    let mut query = Query::new();
    for key in keys {
        query.insert_key(key);
    }
    query
}

fn epoch_key(epoch: u64) -> Vec<u8> {
    (epoch as u16).to_be_bytes().to_vec()
}

impl IdentityReads {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.transitions_per_block > 0, "a block needs a transition");
        assert!(self.identities > 0, "a read needs an identity");
        let big_sums = self.features.provable_count_provable_big_sum_tree;
        let mut ops = vec![
            Op::Batch(vec![
                batch::insert_only(&[], ROOT_IDENTITIES, Element::empty_tree()),
                batch::insert_only(&[], ROOT_BALANCES, Element::empty_sum_tree()),
                batch::insert_only(&[], ROOT_POOLS, Element::empty_tree()),
            ]),
            Op::Commit,
        ];
        if big_sums {
            let pools = path(&[ROOT_POOLS]);
            ops.push(Op::Batch(vec![
                batch::insert_only(
                    &pools,
                    TOTALS,
                    Element::empty_provable_count_provable_big_sum_tree(),
                ),
                batch::insert_only(&pools, EPOCHS, Element::empty_tree()),
            ]));
            ops.push(Op::Commit);
        }
        let mut state = State {
            rng: Rng::derive(seed, DOMAIN_IDENTITY_READS, 0),
            features: self.features,
            identities: Vec::new(),
            pools: 0,
            block: 1,
        };
        let transitions = self.identities + self.updates;
        for done in 1..=transitions {
            let transition = if done <= self.identities {
                state.create()
            } else {
                state.update()
            };
            ops.push(Op::Batch(transition));
            if ends_block(done, transitions, u64::from(self.transitions_per_block)) {
                if big_sums {
                    ops.push(Op::Batch(state.pay_pools()));
                }
                ops.push(Op::Commit);
                state.reads(&mut ops);
                state.block += 1;
            }
        }
        Prepared::stream(ops)
    }
}

impl State {
    fn flags(&self) -> Option<Vec<u8>> {
        storage_flags(epoch_of_block(self.block), None)
    }

    fn key(&mut self, id: &[u8; 32], index: u64) -> BatchOp {
        batch::insert_only(
            &keys_path(id),
            &key_id(index),
            Element::new_item_with_flags(value(&mut self.rng, 40, 100), self.flags()),
        )
    }

    fn create(&mut self) -> Vec<BatchOp> {
        let id = self.rng.id();
        let identity_path = path(&[ROOT_IDENTITIES, &id]);
        let tree = Element::empty_tree_with_flags(self.flags());
        let mut ops = vec![
            batch::insert_only(&path(&[ROOT_IDENTITIES]), &id, tree.clone()),
            batch::insert_only(&identity_path, KEYS_KEY, tree),
        ];
        let keys = self.rng.between(1, 3);
        for index in 0..keys {
            ops.push(self.key(&id, index));
        }
        ops.push(batch::insert_only(
            &path(&[ROOT_BALANCES]),
            &id,
            Element::new_sum_item_with_flags(self.rng.between(1, 1_000_000) as i64, self.flags()),
        ));
        self.identities.push(Identity { id, keys });
        ops
    }

    fn update(&mut self) -> Vec<BatchOp> {
        let chosen = self.rng.index(self.identities.len());
        let id = self.identities[chosen].id;
        let keys = self.identities[chosen].keys;
        if keys < MAX_KEYS && self.rng.chance(1, 3) {
            self.identities[chosen].keys += 1;
            return vec![self.key(&id, keys)];
        }
        vec![batch::replace(
            &path(&[ROOT_BALANCES]),
            &id,
            Element::new_sum_item_with_flags(self.rng.between(1, 1_000_000) as i64, self.flags()),
        )]
    }

    /// The fees of the block, paid into the totals and into the tree of the
    /// block's epoch, which the first block of an epoch creates.
    fn pay_pools(&mut self) -> Vec<BatchOp> {
        let pools = path(&[ROOT_POOLS]);
        let epochs = child(&pools, EPOCHS);
        let epoch = self.block / 3;
        let mut ops = Vec::new();
        if self.block.is_multiple_of(3) || self.pools == 0 {
            ops.push(batch::insert_only(
                &epochs,
                &epoch_key(epoch),
                Element::empty_provable_count_provable_big_sum_tree(),
            ));
        }
        let fee = || Element::new_sum_item(LARGE_CREDITS);
        let payment = self.block.to_be_bytes();
        ops.push(batch::insert_only(&child(&pools, TOTALS), &payment, fee()));
        ops.push(batch::insert_only(
            &child(&epochs, &epoch_key(epoch)),
            &payment,
            fee(),
        ));
        self.pools = epoch + 1;
        ops
    }

    fn reads(&mut self, ops: &mut Vec<Op>) {
        let chosen = self.rng.index(self.identities.len());
        let identity = &self.identities[chosen];
        let (id, held) = (identity.id, identity.keys);

        // Keys by id: one the identity has, one it may have, and two no
        // identity has.
        let wanted = [0, self.rng.below(MAX_KEYS), MAX_KEYS, MAX_KEYS + 3];
        let by_id = PathQuery::new(
            keys_path(&id),
            SizedQuery::new(keys(wanted.map(key_id)), Some(wanted.len() as u16), None),
        );
        for raw in [false, true] {
            ops.push(Op::Read(Read::KeysOptional {
                query: Box::new(by_id.clone()),
                raw,
            }));
        }

        let page = PathQuery::new(
            keys_path(&id),
            SizedQuery::new(
                Query::new_range_full(),
                Some(self.rng.between(1, 3) as u16),
                (held > 1).then(|| self.rng.below(held) as u16),
            ),
        );
        let balances = PathQuery::new(
            path(&[ROOT_BALANCES]),
            SizedQuery::new(
                Query::new_single_query_item(QueryItem::RangeFrom(self.rng.id().to_vec()..)),
                Some(self.rng.between(1, 5) as u16),
                None,
            ),
        );
        ops.push(Op::Read(Read::ItemValues {
            query: Box::new(page.clone()),
            or_sums: false,
        }));
        for query in [page, balances.clone()] {
            ops.push(Op::Read(Read::ItemValues {
                query: Box::new(query),
                or_sums: true,
            }));
        }
        ops.push(Op::Read(Read::SumValues {
            query: Box::new(balances),
        }));

        // The keys of two identities, read and proven together.
        let other = self.identities[self.rng.index(self.identities.len())].id;
        if other != id {
            let queries = vec![
                PathQuery::new_unsized(keys_path(&id), keys([key_id(0)])),
                PathQuery::new_unsized(
                    keys_path(&other),
                    keys([key_id(0), key_id(1), key_id(MAX_KEYS)]),
                ),
            ];
            ops.push(Op::Read(Read::Many {
                queries: queries.clone(),
            }));
            ops.push(prove(Prove::Many(queries)));
        }

        if self.features.provable_count_provable_big_sum_tree {
            self.pool_reads(ops);
        }
    }

    fn pool_reads(&mut self, ops: &mut Vec<Op>) {
        let pools = path(&[ROOT_POOLS]);
        let since = QueryItem::RangeFrom(self.rng.below(self.block + 1).to_be_bytes().to_vec()..);
        for with_count in [false, true] {
            let aggregate = |range: QueryItem| {
                if with_count {
                    Query::new_aggregate_count_and_sum_on_range(range)
                } else {
                    Query::new_aggregate_sum_on_range(range)
                }
            };
            ops.push(Op::Read(Read::BigSum {
                query: Box::new(PathQuery::new_unsized(
                    child(&pools, TOTALS),
                    aggregate(since.clone()),
                )),
                with_count,
                per_key: false,
            }));
            let mut per_epoch = keys((0..self.pools).map(epoch_key));
            per_epoch.set_subquery(aggregate(since.clone()));
            ops.push(Op::Read(Read::BigSum {
                query: Box::new(PathQuery::new_unsized(child(&pools, EPOCHS), per_epoch)),
                with_count,
                per_key: true,
            }));
        }
    }
}
