//! Any sequence of writes and evaluations: the marks are complete at every
//! moment, and what is evaluated is what the grove says the hashes are.

use std::collections::BTreeSet;

use super::{acceptance::value, grove::Grove, *};

struct Run {
    grove: Grove,
    rng: Rng,
    /// The keys of each Merk.
    keys: Vec<BTreeSet<Vec<u8>>>,
    evaluations: usize,
    eager_writes: usize,
    drops: usize,
}

impl Run {
    fn new(seed: u64) -> Self {
        Self {
            grove: Grove::new(true),
            rng: Rng(seed),
            keys: vec![BTreeSet::new()],
            evaluations: 0,
            eager_writes: 0,
            drops: 0,
        }
    }

    fn merk(&mut self) -> usize {
        let alive: Vec<usize> = (0..self.grove.merks.len())
            .filter(|merk| self.grove.merks[*merk].alive)
            .collect();
        alive[self.rng.below(alive.len())]
    }

    fn key_of(&mut self, merk: usize) -> Option<Vec<u8>> {
        let keys = &self.keys[merk];
        let index = self.rng.below(keys.len().max(1));
        keys.iter().nth(index).cloned()
    }

    fn put(&mut self) {
        let merk = self.merk();
        let key = match self.rng.chance(60) {
            true => format!("key {:03}", self.rng.below(400)).into_bytes(),
            false => self.key_of(merk).unwrap_or_else(|| b"key".to_vec()),
        };
        let len = match self.rng.chance(6) {
            true => 300 + self.rng.below(500),
            false => 8 + self.rng.below(120),
        };
        let number = self.rng.below(1 << 20);
        self.grove.put(merk, &key, &value(number, len));
        self.keys[merk].insert(key);
        self.eager_writes += usize::from(!self.grove.lazy);
    }

    fn delete(&mut self) {
        let merk = self.merk();
        let Some(key) = self.key_of(merk) else { return };
        if merk == 0 && self.keys[0].len() == 1 {
            return;
        }
        let hosted = self.grove.node(merk, &key).hosted.len();
        self.grove.delete(merk, &key);
        self.keys[merk].remove(&key);
        self.drops += hosted;
    }

    fn host(&mut self) {
        let merk = self.merk();
        let alive = self.grove.merks.iter().filter(|merk| merk.alive).count();
        let Some(key) = self.key_of(merk).filter(|_| alive < 9) else {
            return;
        };
        let hosted = self.grove.host(merk, &key);
        assert_eq!(hosted, self.keys.len());
        self.keys.push(BTreeSet::new());
    }

    /// Evaluates one marked node, or asks for the root hash of one Merk.
    fn ask(&mut self) {
        self.evaluations += 1;
        if self.rng.chance(50) {
            let marked: Vec<CellAddress> = self.grove.hashes.marked().map(|(at, _)| at).collect();
            if !marked.is_empty() {
                let at = marked[self.rng.below(marked.len())];
                self.grove.evaluate(at);
                assert_eq!(self.grove.hashes.stale(at), Stale::NONE);
            }
            return;
        }
        let merk = self.merk();
        let Some(root) = self.grove.merks[merk].root else {
            return;
        };
        let key = self.grove.nodes[root].key.clone();
        let at = self.grove.root_of(merk);
        let expected = self.grove.expected(merk, &key).hash;
        assert_eq!(self.grove.hash_of(at), expected, "the root hash of a Merk");
    }

    fn step(&mut self) {
        match self.rng.below(100) {
            0..=54 => self.put(),
            55..=69 => self.delete(),
            70..=73 => self.host(),
            74..=85 => self.ask(),
            86..=89 => {
                self.grove.evaluate_all();
                self.grove.check_hashes();
            }
            90..=93 => self.grove.log.write_back(&mut self.grove.mem),
            _ => self.grove.lazy = self.rng.chance(75),
        }
    }
}

#[test]
fn whatever_is_written_and_whenever_it_is_asked_for_the_hashes_are_right() {
    let mut carried_marked = 0;
    let mut marks_dropped = 0;
    let mut reads = 0;
    let mut ghost_marks = 0;
    let (mut eager_writes, mut drops) = (0, 0);
    for seed in 1..=4 {
        let mut run = Run::new(seed * 0x5EED);
        for round in 0..900 {
            run.step();
            if round % 12 == 0 {
                run.grove.check_marks();
            }
            let marked = run.grove.hashes.marked();
            ghost_marks += marked
                .filter(|(at, _)| run.grove.ghosts.contains(at))
                .count();
        }
        run.grove.check_marks();
        assert_eq!(run.grove.root_hash(), run.grove.expected_root());
        run.grove.evaluate_all();
        run.grove.check_hashes();
        run.grove.mem.check_records();

        let counters = *run.grove.hashes.counters();
        let calls = run.grove.rules.calls.len() as u64;
        assert_eq!(
            counters.value_hashes + counters.kv_hashes + counters.node_hashes,
            calls,
            "every call of a rule is counted"
        );
        assert_eq!(
            counters.written,
            run.grove.mem.shadow.hashes.len() as u64 - eager_hash_records(&run),
            "every hash an evaluation wrote is one hash record"
        );
        assert!(run.evaluations > 20 && run.grove.merks.len() > 2);
        carried_marked += run.grove.carried_marked;
        marks_dropped += run.grove.marks_dropped;
        reads += run.grove.log.reads;
        eager_writes += run.eager_writes;
        drops += run.drops;
    }
    assert!(carried_marked > 0, "cells were carried with their marks");
    assert!(marks_dropped > 0, "marked cells were removed");
    assert!(reads > 0, "values out of their cells were hashed");
    assert!(ghost_marks > 0, "Merks were dropped with marks in them");
    assert!(eager_writes > 0 && drops > 0);
}

/// The hash records that writes made which stated their hashes themselves.
fn eager_hash_records(run: &Run) -> u64 {
    run.grove.eager_hash_records as u64
}
