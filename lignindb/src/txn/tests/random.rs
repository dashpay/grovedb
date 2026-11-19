//! Random schedules of write transactions, writes with no transaction,
//! commits, rollbacks and rollbacks to a mark, on a store on disk, against
//! the model that keeps records in maps (`transactions.md` T-RB2, T-RB3):
//! every read through every transaction, every cursor, every committed
//! read and every commit outcome agrees with it, and the committed tree
//! pages pass the physical check after every commit.

use super::{
    model::{self, value, OwnedBatch},
    store::{Db, Outcome, Rng},
    *,
};

/// A transaction of a schedule, with the places it can be rolled back to.
struct Open<'s> {
    tx: Tx<'s>,
    marks: Vec<(Mark, usize)>,
}

fn key(rng: &mut Rng, keys: usize) -> Vec<u8> {
    format!("k{:03}", rng.below(keys)).into_bytes()
}

/// A batch of a few changes, as `tx` would make them now.
fn batch<'s>(world: &World<'s>, tx: &Tx<'s>, rng: &mut Rng, keys: usize) -> OwnedBatch {
    let mut batch = OwnedBatch::default();
    if rng.chance(70) {
        let merks = 1 + rng.below(2);
        let first = 1 + rng.below(3) as u8;
        for name in (first..=3).take(merks) {
            let merk = world.merk(tx, name, |items| {
                for _ in 0..1 + rng.below(4) {
                    let key = key(rng, keys);
                    if rng.chance(35) {
                        items.remove(&key);
                    } else {
                        let len = rng.below(200);
                        items.insert(key, value(rng, len));
                    }
                }
            });
            if !merk.nodes.is_empty() || !merk.removed.is_empty() || merk.root.is_some() {
                batch.records.extend(World::root_record(&merk));
                batch.merks.push(merk);
            }
        }
    }
    if batch.merks.is_empty() || rng.chance(40) {
        let units = [
            model::unit(Column::Aux, 0),
            model::unit(Column::Meta, 9),
            model::unit(Column::Data, 7),
            model::unit(Column::Roots, 8),
        ];
        let unit = units[rng.below(units.len())];
        let mut written = BTreeSet::new();
        for _ in 0..1 + rng.below(3) {
            let key = key(rng, keys);
            if !written.insert(key.clone()) {
                continue;
            }
            batch.records.push(match rng.below(10) {
                0..=5 => {
                    let len = rng.below(120);
                    put(unit, &key, &value(rng, len))
                }
                6..=8 => delete(unit, &key),
                _ => {
                    let end = rng.chance(70).then(|| {
                        let mut end = key.clone();
                        end.push(b'5');
                        format!("k{:03}", rng.below(keys)).into_bytes().max(end)
                    });
                    Write::Range(unit, key, end)
                }
            });
        }
    }
    batch
}

fn run(seed: u64, steps: usize, keys: usize) {
    let (db, versions) = Db::new(96);
    let mut world = World::new(&db, &versions);
    let mut rng = Rng::new(seed);
    let mut open: Vec<Open<'_>> = Vec::new();
    let (mut commits, mut conflicts, mut rebuilds) = (0, 0, 0);
    for step in 0..steps {
        let pick =
            |rng: &mut Rng, open: &Vec<Open<'_>>| (!open.is_empty()).then(|| rng.below(open.len()));
        match rng.below(100) {
            0..=9 if open.len() < 3 => open.push(Open {
                tx: world.begin(),
                marks: Vec::new(),
            }),
            0..=49 => {
                let Some(at) = pick(&mut rng, &open) else {
                    continue;
                };
                let this = &mut open[at];
                if rng.chance(25) {
                    let writes = this.tx.model.borrow().writes.len();
                    this.marks.push((this.tx.txn.mark(), writes));
                }
                let batch = batch(&world, &this.tx, &mut rng, keys);
                world.apply(&this.tx, &batch).unwrap_or_else(|error| {
                    panic!("seed {seed}, step {step}: {error}");
                });
                world.check(&this.tx);
            }
            50..=64 => {
                let tx = world.begin();
                let batch = batch(&world, &tx, &mut rng, keys);
                (world.apply(&tx, &batch)).expect("a write with no transaction");
                let others: Vec<&Tx<'_>> = open.iter().map(|open| &open.tx).collect();
                let outcome = world.commit(tx, &others);
                commits += usize::from(matches!(outcome, Outcome::Committed(_)));
                world.check_committed();
            }
            65..=76 => {
                let Some(at) = pick(&mut rng, &open) else {
                    continue;
                };
                let done = open.swap_remove(at);
                rebuilds += done.tx.txn.stats().rebuilds;
                let others: Vec<&Tx<'_>> = open.iter().map(|open| &open.tx).collect();
                match world.commit(done.tx, &others) {
                    Outcome::Committed(_) => commits += 1,
                    Outcome::Conflict => conflicts += 1,
                    Outcome::Empty => {}
                }
                world.check_committed();
            }
            77..=81 => {
                let Some(at) = pick(&mut rng, &open) else {
                    continue;
                };
                let this = &mut open[at];
                this.tx.txn.rollback().expect("no scope is open");
                this.tx.forget();
                this.marks.clear();
                world.check(&this.tx);
            }
            82..=89 => {
                let Some(at) = pick(&mut rng, &open) else {
                    continue;
                };
                let this = &mut open[at];
                if this.marks.is_empty() {
                    continue;
                }
                let to = rng.below(this.marks.len());
                let (mark, writes) = this.marks[to];
                this.marks.truncate(to);
                this.tx.txn.rewind(mark);
                this.tx.model.borrow_mut().writes.truncate(writes);
                world.check(&this.tx);
            }
            _ => {
                for this in &open {
                    world.check(&this.tx);
                }
            }
        }
    }
    for this in &open {
        world.check(&this.tx);
    }
    drop(open);
    world.check_committed();
    assert_eq!(
        db.private_ids(),
        0,
        "seed {seed}: no id is left with a transaction"
    );
    assert_eq!(db.pool.census().private, 0, "seed {seed}: no copy is left");
    assert!(commits > steps / 20, "seed {seed}: {commits} commits");
    assert!(conflicts > 0 || steps < 400, "seed {seed}: no conflict");
    assert!(rebuilds > 0 || steps < 400, "seed {seed}: no rebase");
}

#[test]
fn every_read_and_every_commit_outcome_agrees_with_the_model() {
    let steps = if cfg!(miri) { 40 } else { 700 };
    for seed in 0..6 {
        run(seed, steps, 60);
    }
}

#[test]
fn it_agrees_with_the_model_where_few_keys_make_many_overlaps() {
    let steps = if cfg!(miri) { 40 } else { 500 };
    for seed in 100..104 {
        run(seed, steps, 12);
    }
}
