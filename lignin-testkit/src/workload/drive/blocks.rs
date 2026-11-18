//! Blocks that are not simply applied and committed.

use std::collections::BTreeMap;

use grovedb::{Element, PathQuery, Query, QueryItem, SizedQuery};

use super::DOMAIN_BLOCKS;
use crate::workload::{
    build::{self, batch, path, read, step, through_snapshot},
    op::{BatchOp, Op, Read, TransactionOp},
    rng::Rng,
    Prepared, Stream,
};

/// A root tree of this workload's own, in the place of Drive's `Balances`.
const ROOT: &[u8] = &[80];
const BALANCES: &[u8] = b"balances";
const NONCES: &[u8] = b"nonces";

/// The transaction of a block used the way Platform uses it.
///
/// Each state transition moves credits from one identity to another and
/// bumps the sender's nonce, in one batch. When the two are the same
/// identity the credits are a fee: they leave it and reach no one. The blocks around them go through every
/// way a transaction ends and unwinds:
///
/// - **A proposal being prepared.** Platform sets a savepoint before each
///   state transition. One that turns out invalid after it has written is
///   rolled back to its savepoint, which is then set again for the next.
///   GroveDB has no call that releases a savepoint, so the savepoints of
///   the transitions that are kept stay on the stack until the commit.
/// - **Genesis.** A savepoint is set once, each attempt sets its own inside
///   it and is rolled back, and the last attempt rolls back to the first
///   savepoint before the block is written for good.
/// - **A proposal that is refused.** Everything the block wrote is rolled
///   back with `rollback_transaction`, and the transaction goes on to hold
///   the next block.
/// - **A round that is abandoned.** The transaction is dropped without a
///   commit and another is started.
/// - **A query during a block.** A snapshot read transaction is opened, the
///   next block is written and committed, and the query reads through the
///   snapshot: it gets the balances of the block before, where the same
///   read against committed state gets the new ones. Some snapshot reads
///   come while the block is still open.
///
/// What a block that was rolled back or dropped wrote is never stored: the
/// reads after it return what the block before it left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockExecution {
    /// How many identities hold a balance.
    pub identities: u32,
    /// How many blocks to write.
    pub blocks: u32,
    /// How many state transitions a block holds.
    pub transitions_per_block: u32,
}

impl Default for BlockExecution {
    fn default() -> Self {
        Self {
            identities: 12,
            blocks: 30,
            transitions_per_block: 5,
        }
    }
}

struct State {
    rng: Rng,
    ids: Vec<[u8; 32]>,
    /// The balance and the nonce of each identity, as committed and with
    /// what the open block has written so far.
    committed: BTreeMap<[u8; 32], (i64, u64)>,
    open: BTreeMap<[u8; 32], (i64, u64)>,
}

impl State {
    /// One state transition: credits move and the sender's nonce goes up.
    fn transfer(&mut self) -> Op {
        let from = self.any();
        let to = self.any();
        let amount = self.rng.between(1, 500) as i64;
        let (balances, nonces) = (path(&[ROOT, BALANCES]), path(&[ROOT, NONCES]));
        let mut set = |id: &[u8; 32], change: i64, used: u64, ops: &mut Vec<BatchOp>| {
            let entry = self.open.get_mut(id).expect("an identity has a balance");
            entry.0 += change;
            entry.1 += used;
            ops.push(batch::replace(
                &balances,
                id,
                Element::new_sum_item(entry.0),
            ));
            if used > 0 {
                ops.push(batch::insert_or_replace(
                    &nonces,
                    id,
                    Element::new_item(entry.1.to_be_bytes().to_vec()),
                ));
            }
        };
        let mut ops = Vec::new();
        set(&from, -amount, 1, &mut ops);
        if to != from {
            set(&to, amount, 0, &mut ops);
        }
        Op::Batch(ops)
    }

    fn any(&mut self) -> [u8; 32] {
        self.ids[self.rng.index(self.ids.len())]
    }

    /// The reads of a query: one balance, a page of balances, one nonce.
    fn query(&mut self) -> Vec<Read> {
        let (balances, nonces) = (path(&[ROOT, BALANCES]), path(&[ROOT, NONCES]));
        let from = self.any();
        vec![
            read::get(&balances, &self.any()),
            Read::Rows {
                query: Box::new(PathQuery::new(
                    balances,
                    SizedQuery::new(
                        Query::new_single_query_item(QueryItem::RangeFrom(from.to_vec()..)),
                        Some(3),
                        None,
                    ),
                )),
                raw: true,
            },
            read::has_raw(&nonces, &self.any()),
        ]
    }
}

impl BlockExecution {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.identities > 0, "a transfer needs an identity");
        let mut rng = Rng::derive(seed, DOMAIN_BLOCKS, 0);
        let root = path(&[ROOT]);
        let balances = path(&[ROOT, BALANCES]);
        let mut ops = vec![
            build::insert(&[], ROOT, Element::empty_tree()),
            build::insert(&root, BALANCES, Element::empty_sum_tree()),
            build::insert(&root, NONCES, Element::empty_tree()),
        ];
        let mut committed = BTreeMap::new();
        let mut ids = Vec::new();
        for _ in 0..self.identities {
            let id = rng.id();
            let balance = rng.between(10_000, 1_000_000) as i64;
            ops.push(build::insert(
                &balances,
                &id,
                Element::new_sum_item(balance),
            ));
            committed.insert(id, (balance, 0));
            ids.push(id);
        }
        ops.push(Op::Commit);
        let mut state = State {
            rng,
            ids,
            open: committed.clone(),
            committed,
        };

        for block in 0..self.blocks {
            // What a query sees depends on when its snapshot was taken:
            // every fourth block is written under one taken before it.
            let snapshot = block % 4 == 1;
            let queries = if snapshot {
                let queries = state.query();
                ops.extend(queries.iter().cloned().map(Op::Read));
                ops.push(step(TransactionOp::OpenSnapshot));
                queries
            } else {
                Vec::new()
            };

            match block % 6 {
                // Genesis: attempts inside one savepoint, all rolled back.
                0 => {
                    ops.push(step(TransactionOp::SetSavepoint));
                    for _ in 0..2 {
                        ops.push(step(TransactionOp::SetSavepoint));
                        ops.push(state.transfer());
                        ops.push(step(TransactionOp::RollbackToSavepoint));
                        state.open = state.committed.clone();
                    }
                    ops.push(state.transfer());
                    ops.push(step(TransactionOp::RollbackToSavepoint));
                    state.open = state.committed.clone();
                    self.proposal(&mut state, &mut ops);
                    self.commit(&mut state, &mut ops);
                }
                // A refused proposal: the block is rolled back, and the
                // same transaction holds the block that replaces it.
                2 => {
                    self.proposal(&mut state, &mut ops);
                    ops.push(step(TransactionOp::Rollback));
                    state.open = state.committed.clone();
                    ops.extend(state.query().into_iter().map(Op::Read));
                    self.proposal(&mut state, &mut ops);
                    self.commit(&mut state, &mut ops);
                }
                // An abandoned round: the transaction is dropped.
                4 => {
                    self.proposal(&mut state, &mut ops);
                    ops.push(step(TransactionOp::Abandon));
                    state.open = state.committed.clone();
                    ops.extend(state.query().into_iter().map(Op::Read));
                }
                _ => {
                    self.proposal(&mut state, &mut ops);
                    if snapshot {
                        // The block is still open: the snapshot and the
                        // committed state agree.
                        ops.push(through_snapshot(queries[0].clone()));
                    }
                    self.commit(&mut state, &mut ops);
                }
            }

            if snapshot {
                for query in queries {
                    ops.push(Op::Read(query.clone()));
                    ops.push(through_snapshot(query));
                }
                ops.push(step(TransactionOp::CloseSnapshot));
            }
        }
        Prepared::stream(ops)
    }

    /// The state transitions of a proposal, each behind a savepoint. One in
    /// four turns out invalid after it has written, and is unwound.
    fn proposal(&self, state: &mut State, ops: &mut Vec<Op>) {
        for _ in 0..self.transitions_per_block {
            ops.push(step(TransactionOp::SetSavepoint));
            let before = state.open.clone();
            ops.push(state.transfer());
            if state.rng.chance(1, 4) {
                ops.push(step(TransactionOp::RollbackToSavepoint));
                ops.push(step(TransactionOp::SetSavepoint));
                state.open = before;
            }
        }
    }

    fn commit(&self, state: &mut State, ops: &mut Vec<Op>) {
        ops.push(Op::Commit);
        state.committed = state.open.clone();
        ops.extend(state.query().into_iter().map(Op::Read));
    }
}
