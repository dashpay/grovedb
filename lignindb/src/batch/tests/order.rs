//! The outcome of a batch does not depend on the order of the calls of
//! one operation, where no two of them say the same thing of the same key.
//!
//! An operation is drawn with at most one put and one delete of a key, one
//! statement of what an element hosts and one of where a subtree hangs, one
//! root key and one "no root" of a Merk, and one enter and one leave of a
//! subtree. Its calls are then made
//! in the order they were drawn, and in other orders.

use std::collections::BTreeSet;

use super::{leaf, rules::shape, subtree, unit, Rng};
use crate::{
    batch::StagedBatch,
    txn::{Column, Hosted},
};

#[derive(Debug, Clone)]
enum Call {
    Put {
        name: u8,
        key: u8,
        value: Option<u8>,
        hosted: Option<Vec<u8>>,
    },
    DeleteNode {
        name: u8,
        key: u8,
    },
    Root {
        name: u8,
        key: Option<u8>,
    },
    Host {
        name: u8,
        key: u8,
        hosted: Vec<u8>,
    },
    Enter {
        name: u8,
    },
    Leave {
        name: u8,
        clear: bool,
    },
    Flat {
        name: u8,
        key: u8,
        value: Option<u8>,
    },
}

/// What a call says something of: no two calls of one operation have the
/// same.
type Said = (u8, u8, u8);

fn draw(rng: &mut Rng, said: &mut BTreeSet<Said>) -> Option<Call> {
    let name = 1 + rng.below(4) as u8;
    let key = b'a' + rng.below(2) as u8;
    // Where a subtree hangs is said once in an operation: of two elements
    // that name one subtree, the later statement stands.
    let hosted = |rng: &mut Rng, said: &mut BTreeSet<Said>| -> Option<Vec<u8>> {
        let hosted: Vec<u8> = (0..rng.below(3)).map(|_| 1 + rng.below(4) as u8).collect();
        let named: BTreeSet<u8> = hosted.iter().copied().collect();
        let once = named.iter().all(|name| !said.contains(&(7, *name, 0)));
        said.extend(named.into_iter().map(|name| (7, name, 0)));
        once.then_some(hosted)
    };
    let (call, says) = match rng.below(16) {
        0..=3 => {
            let value = (rng.below(3) > 0).then(|| rng.below(200) as u8);
            let hosted = match rng.below(4) {
                0 => Some(hosted(rng, said)?),
                _ => None,
            };
            // A put that says what its node hosts is a statement of it.
            let stated = hosted.is_some() && !said.insert((3, name, key));
            if stated {
                return None;
            }
            (
                Call::Put {
                    name,
                    key,
                    value,
                    hosted,
                },
                (0, name, key),
            )
        }
        4..=6 => (Call::DeleteNode { name, key }, (1, name, key)),
        7 => {
            let key = (rng.below(2) == 0).then_some(key);
            (Call::Root { name, key }, (2, name, u8::from(key.is_some())))
        }
        8 | 9 => {
            let hosted = hosted(rng, said)?;
            (Call::Host { name, key, hosted }, (3, name, key))
        }
        10 | 11 => (Call::Enter { name }, (4, name, 0)),
        12 | 13 => {
            let clear = rng.below(2) == 0;
            (Call::Leave { name, clear }, (5, name, 0))
        }
        _ => {
            let value = (rng.below(2) == 0).then(|| rng.below(200) as u8);
            let says = (6, name, key * 2 + u8::from(value.is_some()));
            (Call::Flat { name, key, value }, says)
        }
    };
    said.insert(says).then_some(call)
}

fn make(batch: &StagedBatch<usize>, call: &Call, tag: usize) {
    match call {
        Call::Put {
            name,
            key,
            value,
            hosted,
        } => {
            let node = leaf(&[*key], &[value.unwrap_or_default()]);
            let hosted: Option<Vec<_>> =
                (hosted.as_ref()).map(|hosted| hosted.iter().copied().map(subtree).collect());
            let hosted = match &hosted {
                Some(hosted) => Hosted::Set(hosted),
                None => Hosted::Keep,
            };
            batch.put_node(subtree(*name), &node.put(value.is_some(), hosted), tag);
        }
        Call::DeleteNode { name, key } => batch.delete_node(subtree(*name), &[*key], tag),
        Call::Root { name, key } => {
            batch.set_root(subtree(*name), key.as_ref().map(std::slice::from_ref));
        }
        Call::Host { name, key, hosted } => {
            let hosted: Vec<_> = hosted.iter().copied().map(subtree).collect();
            batch.host(subtree(*name), &[*key], &hosted);
        }
        // The tree type says which enter stands.
        Call::Enter { name } => batch.enter(subtree(*name), tag.to_string().as_bytes()),
        Call::Leave { name, clear: true } => batch.clear(subtree(*name)),
        Call::Leave { name, clear: false } => batch.drop_subtree(subtree(*name)),
        Call::Flat {
            name,
            key,
            value: Some(value),
        } => batch.put(unit(Column::Aux, *name), &[*key], &[*value], tag),
        Call::Flat {
            name,
            key,
            value: None,
        } => batch.delete(unit(Column::Aux, *name), &[*key], tag),
    }
}

/// What the operations come to, each with its calls in the order `order`
/// gives, or that the batch is refused. `asks` asks the batch how much it
/// holds between the calls.
fn outcome(
    operations: &[Vec<Call>],
    order: &[Vec<usize>],
    asks: bool,
) -> Option<(Vec<String>, String, Vec<usize>)> {
    let batch = StagedBatch::new();
    let mut first = 0;
    for (calls, order) in operations.iter().zip(order) {
        batch.next_operation();
        for at in order {
            make(&batch, &calls[*at], first + at);
            if asks {
                let _ = (batch.len(), batch.is_empty());
            }
        }
        first += calls.len();
    }
    // What the batch says it holds, before its last operation is settled.
    let (len, empty) = (batch.len(), batch.is_empty());
    let Ok(resolved) = batch.resolve() else {
        assert!(!empty, "a batch that is refused is not empty");
        return None;
    };
    assert_eq!((len, empty), (resolved.len(), resolved.is_empty()));
    let mut write = shape(&resolved);
    // Enters that do not hang on one another keep the order of their calls.
    let mut subtrees = std::mem::take(&mut write.subtrees);
    subtrees.sort();
    let tags = resolved.outcomes().map(|outcome| *outcome.tag).collect();
    Some((subtrees, format!("{write:?}"), tags))
}

#[test]
fn the_order_of_the_calls_of_one_operation_does_not_matter() {
    let (mut refused, mut applied) = (0, 0);
    for seed in 1..=800u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let operations: Vec<Vec<Call>> = (0..1 + rng.below(4))
            .map(|_| {
                let mut said = BTreeSet::new();
                (0..rng.below(9))
                    .filter_map(|_| draw(&mut rng, &mut said))
                    .collect()
            })
            .collect();
        let as_drawn: Vec<Vec<usize>> = (operations.iter())
            .map(|calls| (0..calls.len()).collect())
            .collect();
        let expected = outcome(&operations, &as_drawn, false);
        match expected {
            Some(_) => applied += 1,
            None => refused += 1,
        }
        for _ in 0..3 {
            let mut order = as_drawn.clone();
            for order in &mut order {
                for at in (1..order.len()).rev() {
                    order.swap(at, rng.below(at + 1));
                }
            }
            let asks = rng.below(2) == 0;
            assert_eq!(
                outcome(&operations, &order, asks),
                expected,
                "seed {seed}: {operations:?} in the order {order:?}, asking: {asks}"
            );
        }
    }
    // Both kinds of batch are drawn.
    assert!(
        refused > 50 && applied > 50,
        "{refused} refused, {applied} applied"
    );
}
