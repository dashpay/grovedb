//! Batches applied in two segments.

use grovedb::Element;

use super::DOMAIN_PARTIAL;
use crate::workload::{
    build::{self, batch, child, path, storage_flags, sum, value},
    op::{AppendEntry, BatchOp, Op, Path, QueryItem},
    rng::Rng,
    Prepared, Stream,
};

const ROOT: &[u8] = b"partial";
const OWNERS: u64 = 6;

/// Partial batches.
///
/// A partial batch stops at a height of the grove, tells its caller what it
/// has cost so far and takes more operations before it goes on: the way to
/// charge for a state transition inside the batch that applies it. GroveDB
/// keeps the first segment's writes pending while the second is applied on
/// the same trees, and commits both at once.
///
/// Every transition here stores a document under its owner's tree in the
/// first segment. The second segment then pays: it lowers the owner's
/// balance and raises a fee pool, in sum trees the first segment did not
/// write to. The transitions vary what else happens: the pause height,
/// second segments that are empty, that write next to the first segment's
/// writes or that append to a log, and first segments that delete or patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartialBatches {
    /// How many partial batches to apply.
    pub transitions: u64,
    /// How many of them make a block.
    pub transitions_per_block: u32,
}

impl Default for PartialBatches {
    fn default() -> Self {
        Self {
            transitions: 120,
            transitions_per_block: 8,
        }
    }
}

struct Generator {
    rng: Rng,
    root: Path,
    balances: Vec<i64>,
    fees: i64,
    /// The documents each owner holds, as their keys.
    documents: Vec<Vec<Vec<u8>>>,
    stored: u64,
    ops: Vec<Op>,
}

impl PartialBatches {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(self.transitions_per_block > 0, "a block needs a transition");
        let mut generator = Generator {
            rng: Rng::derive(seed, DOMAIN_PARTIAL, 0),
            root: path(&[ROOT]),
            balances: Vec::new(),
            fees: 0,
            documents: vec![Vec::new(); OWNERS as usize],
            stored: 0,
            ops: Vec::new(),
        };
        generator.setup();
        let per_block = u64::from(self.transitions_per_block);
        for transition in 0..self.transitions {
            generator.transition(transition);
            if (transition + 1) % per_block == 0 || transition + 1 == self.transitions {
                generator.ops.push(Op::Commit);
                generator.read();
            }
        }
        Prepared::stream(generator.ops)
    }
}

fn owner_key(owner: u64) -> Vec<u8> {
    format!("owner-{owner}").into_bytes()
}

impl Generator {
    fn setup(&mut self) {
        let root = self.root.clone();
        let documents = child(&root, b"documents");
        self.ops.extend([
            build::insert(&[], ROOT, Element::empty_tree()),
            build::insert(&root, b"documents", Element::empty_tree()),
            build::insert(&root, b"balances", Element::empty_sum_tree()),
            build::insert(&root, b"pools", Element::empty_sum_tree()),
            build::insert(&root, b"log", Element::empty_mmr_tree()),
            build::insert(&child(&root, b"pools"), b"fees", Element::new_sum_item(0)),
        ]);
        for owner in 0..OWNERS {
            let balance = self.rng.between(1_000_000, 9_000_000) as i64;
            self.balances.push(balance);
            self.ops.push(build::insert(
                &child(&root, b"balances"),
                &owner_key(owner),
                Element::new_sum_item(balance),
            ));
            self.ops.push(build::insert(
                &documents,
                &owner_key(owner),
                Element::empty_count_tree(),
            ));
        }
        self.ops.push(Op::Commit);
    }

    /// The second segment's payment: the owner pays `fee` into the pool.
    fn payment(&mut self, owner: u64, fee: i64) -> Vec<BatchOp> {
        self.balances[owner as usize] -= fee;
        self.fees += fee;
        vec![
            batch::replace(
                &child(&self.root, b"balances"),
                &owner_key(owner),
                Element::new_sum_item(self.balances[owner as usize]),
            ),
            batch::replace(
                &child(&self.root, b"pools"),
                b"fees",
                Element::new_sum_item(self.fees),
            ),
        ]
    }

    fn new_document(&mut self, owner: u64) -> BatchOp {
        let key = self.stored.to_be_bytes().to_vec();
        self.stored += 1;
        self.documents[owner as usize].push(key.clone());
        batch::insert_only(
            &child(&child(&self.root, b"documents"), &owner_key(owner)),
            &key,
            Element::new_item_with_flags(
                value(&mut self.rng, 20, 200),
                storage_flags((self.stored % 512) as u16, None),
            ),
        )
    }

    fn transition(&mut self, transition: u64) {
        let owner = self.rng.below(OWNERS);
        let other = (owner + 1 + self.rng.below(OWNERS - 1)) % OWNERS;
        let documents = child(&self.root, b"documents");
        let owner_path = child(&documents, &owner_key(owner));
        let fee = self.rng.between(100, 5_000) as i64;
        let kind = transition % 9;

        let earlier = self.documents[owner as usize].first().cloned();
        let mut ops = vec![self.new_document(owner)];
        let mut add_on = Vec::new();
        let mut pause_height = None;
        match kind {
            0 => add_on = self.payment(owner, fee),
            1 => {}
            2 => {
                pause_height = Some(1);
                add_on = self.payment(owner, fee);
                add_on.push(batch::insert_or_replace(
                    &child(&self.root, b"balances"),
                    b"burned",
                    Element::new_sum_item(sum(&mut self.rng)),
                ));
            }
            3 => {
                pause_height = Some(2);
                add_on = self.payment(owner, fee);
            }
            4 => {
                if let Some(key) = &earlier {
                    self.documents[owner as usize].remove(0);
                    ops.push(batch::delete(&owner_path, key));
                }
                add_on = self.payment(owner, fee);
            }
            5 => {
                if let Some(key) = &earlier {
                    ops.push(batch::patch(
                        &owner_path,
                        key,
                        Element::new_item_with_flags(
                            value(&mut self.rng, 20, 200),
                            storage_flags(7, None),
                        ),
                        0,
                    ));
                }
                add_on = self.payment(owner, fee);
            }
            6 => {
                add_on = self.payment(owner, fee);
                add_on.push(self.new_document(other));
            }
            7 => {
                add_on = self.payment(owner, fee);
                add_on.push(batch::append(
                    &child(&self.root, b"log"),
                    AppendEntry::Mmr(value(&mut self.rng, 8, 40)),
                ));
            }
            _ => {
                ops.push(self.new_document(other));
                add_on = self.payment(other, fee);
            }
        }
        self.ops
            .push(build::partial_batch(ops, add_on, pause_height));
    }

    fn read(&mut self) {
        let owner = self.rng.below(OWNERS);
        let documents = child(&self.root, b"documents");
        self.ops.push(build::get(
            &child(&self.root, b"balances"),
            &owner_key(owner),
        ));
        self.ops
            .push(build::get(&child(&self.root, b"pools"), b"fees"));
        self.ops.push(build::get(&documents, &owner_key(owner)));
        self.ops.push(build::query(build::select(
            &child(&documents, &owner_key(owner)),
            vec![QueryItem::RangeFull],
            Some(6),
        )));
        self.ops.push(build::get(&self.root, b"log"));
    }
}
