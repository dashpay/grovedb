//! What every version of a database under stress holds: *S(v)* for every
//! *v* a reader can ask at (`transactions.md` section 2).
//!
//! Each committer tells the history what its commit wrote, under the
//! version the commit was published as. Versions are dense, so the history
//! takes the commits in by their versions, whichever thread tells first,
//! and a reader of version *v* waits until every commit up to *v* is told.
//! A record is kept with its life: its value from each version that
//! changed it on. So one map answers for every version that is not pruned.

use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use crate::txn::{
    tests::model::{Val, Write},
    Column, Subtree, Unit,
};

/// The value of a record from each version that changed it on, the oldest
/// first. `None` is no record.
type Life = Vec<(u64, Option<Arc<Val>>)>;

#[derive(Default, Clone)]
pub(in crate::db::tests) struct State {
    /// The version through which every commit is taken in.
    version: u64,
    records: BTreeMap<(Unit, Vec<u8>), Life>,
    /// Whether each subtree is entered, from each version that changed
    /// that on.
    entered: BTreeMap<Subtree, Vec<(u64, bool)>>,
    /// The commits that were told before one of an earlier version.
    waiting: BTreeMap<u64, Arc<Vec<Write>>>,
    /// What each commit wrote, by its version, from the floor on.
    commits: BTreeMap<u64, Arc<Vec<Write>>>,
    /// The version below which nothing is asked any more.
    floor: u64,
    /// The version below which each writer asks nothing, while it has a
    /// commit under way.
    lows: [Option<u64>; 2],
}

/// A writer that can hold the history's floor down ([`History::hold`]).
#[derive(Debug, Clone, Copy)]
pub(in crate::db::tests) enum Slot {
    Block,
    Outside,
}

/// The value that `life` has at the version `at`.
fn at(life: &[(u64, Option<Arc<Val>>)], at: u64) -> Option<&Arc<Val>> {
    let newest = life.iter().rev().find(|(born, _)| *born <= at)?;
    newest.1.as_ref()
}

impl State {
    /// The keys of `unit` that have a life, with it.
    fn lives(&self, unit: Unit) -> impl Iterator<Item = (&Vec<u8>, &Life)> {
        (self.records.range((unit, Vec::new())..))
            .take_while(move |((own, _), _)| *own == unit)
            .map(|((_, key), life)| (key, life))
    }

    fn end(&mut self, version: u64, unit: Unit, key: &[u8]) {
        if let Some(life) = self.records.get_mut(&(unit, key.to_vec()))
            && life.last().is_some_and(|(_, value)| value.is_some())
        {
            life.push((version, None));
        }
    }

    fn take_in(&mut self, version: u64, writes: &[Write]) {
        for write in writes {
            match write {
                Write::Put(unit, key, value) => {
                    let life = self.records.entry((*unit, key.clone())).or_default();
                    life.push((version, Some(Arc::new(value.clone()))));
                }
                Write::Delete(unit, key) => self.end(version, *unit, key),
                Write::Leave(left) => {
                    let subtrees = std::iter::once(left.subtree).chain(left.below.iter().copied());
                    for subtree in subtrees {
                        for column in Column::ALL {
                            let unit = Unit::new(column, subtree);
                            if !write.touches(unit) {
                                continue;
                            }
                            let keys: Vec<Vec<u8>> =
                                self.lives(unit).map(|(key, _)| key.clone()).collect();
                            for key in keys {
                                self.end(version, unit, &key);
                            }
                        }
                        if subtree != left.subtree || !left.cleared {
                            (self.entered.entry(subtree).or_default()).push((version, false));
                        }
                    }
                }
                Write::Enter(subtree, _) => {
                    (self.entered.entry(*subtree).or_default()).push((version, true));
                }
                // A subtree that is stated anew keeps its records and
                // stays entered.
                Write::Stated(_) => {}
                Write::Range(..) => unreachable!("the stress writes no range delete of its own"),
                Write::Move(_) => unreachable!("the stress moves no subtree"),
            }
        }
    }

    /// The record under `key` of `unit` at `version`.
    pub(in crate::db::tests) fn record(
        &self,
        unit: Unit,
        key: &[u8],
        version: u64,
    ) -> Option<Arc<Val>> {
        assert!(self.floor <= version && version <= self.version);
        let life = self.records.get(&(unit, key.to_vec()))?;
        at(life, version).cloned()
    }

    /// The records of `unit` at `version`, in key order.
    pub(in crate::db::tests) fn unit(&self, unit: Unit, version: u64) -> Vec<(Vec<u8>, Arc<Val>)> {
        assert!(self.floor <= version && version <= self.version);
        self.lives(unit)
            .filter_map(|(key, life)| Some((key.clone(), at(life, version)?.clone())))
            .collect()
    }

    /// Whether `subtree` is entered at `version`.
    pub(in crate::db::tests) fn is_entered(&self, subtree: Subtree, version: u64) -> bool {
        assert!(self.floor <= version && version <= self.version);
        let Some(changes) = self.entered.get(&subtree) else {
            return false;
        };
        let newest = changes.iter().rev().find(|(born, _)| *born <= version);
        newest.is_some_and(|(_, entered)| *entered)
    }

    /// Every subtree that the history has heard of.
    pub(in crate::db::tests) fn subtrees(&self) -> Vec<Subtree> {
        self.entered.keys().copied().collect()
    }

    /// The versions above `after`, up to `through`, whose commits wrote a
    /// key that `own` writes (`transactions.md` CF2).
    pub(in crate::db::tests) fn met(&self, own: &Write, after: u64, through: u64) -> Vec<u64> {
        if through <= after {
            return Vec::new();
        }
        assert!(self.floor <= after + 1 && through <= self.version);
        (self.commits.range(after + 1..=through))
            .filter(|(_, writes)| writes.iter().any(|write| write.meets(own)))
            .map(|(version, _)| *version)
            .collect()
    }
}

/// See the [module documentation](self).
pub(in crate::db::tests) struct History {
    state: Mutex<State>,
    told: Condvar,
}

impl History {
    /// The history of a database that holds nothing at `version`.
    pub(in crate::db::tests) fn new(version: u64) -> Self {
        let state = State {
            version,
            floor: version,
            ..State::default()
        };
        Self {
            state: Mutex::new(state),
            told: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tells the history what the commit that was published as `version`
    /// wrote.
    pub(in crate::db::tests) fn commit(&self, version: u64, writes: Vec<Write>) {
        let mut state = self.lock();
        assert!(version > state.version, "version {version} is told twice");
        let told = state.waiting.insert(version, Arc::new(writes));
        assert!(told.is_none(), "version {version} is told twice");
        while let Some(writes) = {
            let next = state.version + 1;
            state.waiting.remove(&next)
        } {
            let version = state.version + 1;
            state.take_in(version, &writes);
            state.commits.insert(version, writes);
            state.version = version;
        }
        drop(state);
        self.told.notify_all();
    }

    /// The history, once every commit up to `version` is told. A commit is
    /// told by its thread right after it is published, so this waits for
    /// a moment at most, and fails the test if it waits a minute.
    pub(in crate::db::tests) fn through(&self, version: u64) -> MutexGuard<'_, State> {
        let began = Instant::now();
        let mut state = self.lock();
        while state.version < version {
            let waited = began.elapsed();
            assert!(
                waited < Duration::from_secs(60),
                "version {version} is published and nobody told what it wrote: the history is at {}",
                state.version
            );
            let wait = (self.told).wait_timeout(state, Duration::from_millis(50));
            state = wait.unwrap_or_else(PoisonError::into_inner).0;
        }
        state
    }

    /// A history of its own that holds what this one holds now.
    pub(in crate::db::tests) fn copy(&self) -> Self {
        Self {
            state: Mutex::new(self.lock().clone()),
            told: Condvar::new(),
        }
    }

    /// The version through which every commit is told.
    pub(in crate::db::tests) fn version(&self) -> u64 {
        self.lock().version
    }

    /// Says that the writer of `slot` asks nothing below the version that
    /// `latest` reads, which is the database's latest: it is read under
    /// the history's lock, so no pruning comes between the read and the
    /// word. It holds until [`Self::release`].
    pub(in crate::db::tests) fn hold(&self, slot: Slot, latest: impl FnOnce() -> u64) {
        let mut state = self.lock();
        state.lows[slot as usize] = Some(latest());
    }

    /// Ends what [`Self::hold`] began.
    pub(in crate::db::tests) fn release(&self, slot: Slot) {
        self.lock().lows[slot as usize] = None;
    }

    /// Forgets what only a reader below `floor` could ask for, and no
    /// writer that holds the floor down asks for.
    pub(in crate::db::tests) fn prune(&self, floor: u64) {
        let mut state = self.lock();
        let lows = state.lows.iter().flatten().copied();
        let floor = lows.fold(floor, u64::min).min(state.version);
        if floor <= state.floor {
            return;
        }
        state.floor = floor;
        state.records.retain(|_, life| {
            let kept = life.iter().rposition(|(born, _)| *born <= floor);
            life.drain(..kept.unwrap_or(0));
            !matches!(&life[..], [(born, None)] if *born <= floor)
        });
        state.commits = state.commits.split_off(&floor);
    }
}

#[test]
fn the_history_answers_for_every_version_it_was_told() {
    use crate::txn::tests::model::{Left, Val};
    let subtree = |name: u8| Subtree::new([name; 32]);
    let aux = |name: u8| Unit::new(Column::Aux, subtree(name));
    let put = |name: u8, key: &[u8], value: &[u8]| {
        Write::Put(aux(name), key.to_vec(), Val::Flat(value.to_vec()))
    };
    let flat = |value: Option<Arc<Val>>| match value.as_deref() {
        Some(Val::Flat(value)) => Some(value.clone()),
        Some(Val::Node(_)) => panic!("a node"),
        None => None,
    };
    let history = History::new(1);
    // Version 3 is told before version 2, and is taken in after it.
    history.commit(
        3,
        vec![put(1, b"a", b"three"), Write::Delete(aux(1), b"b".to_vec())],
    );
    assert_eq!(history.version(), 1);
    history.commit(
        2,
        vec![
            Write::Enter(subtree(1), None),
            Write::Enter(subtree(2), Some(subtree(1))),
            put(1, b"a", b"two"),
            put(1, b"b", b"two"),
            put(2, b"c", b"two"),
        ],
    );
    assert_eq!(history.version(), 3);
    let left = Left {
        below: vec![subtree(2)],
        ..Left::of(subtree(1), false)
    };
    history.commit(4, vec![Write::Leave(left)]);
    {
        let state = history.through(4);
        assert_eq!(flat(state.record(aux(1), b"a", 1)), None);
        assert_eq!(flat(state.record(aux(1), b"a", 2)), Some(b"two".to_vec()));
        assert_eq!(flat(state.record(aux(1), b"a", 3)), Some(b"three".to_vec()));
        assert_eq!(flat(state.record(aux(1), b"b", 3)), None);
        assert_eq!(flat(state.record(aux(2), b"c", 3)), Some(b"two".to_vec()));
        assert_eq!(state.unit(aux(1), 2).len(), 2);
        assert_eq!(state.unit(aux(1), 3).len(), 1);
        assert!(state.unit(aux(1), 4).is_empty() && state.unit(aux(2), 4).is_empty());
        assert!(!state.is_entered(subtree(1), 1) && state.is_entered(subtree(2), 3));
        assert!(!state.is_entered(subtree(1), 4) && !state.is_entered(subtree(2), 4));
        assert_eq!(state.subtrees().len(), 2);
        // A write of the key that versions 2 and 3 wrote, first made at
        // version 1, meets both, and the drop of version 4, which takes
        // every key of the subtree. One first made at 2 meets version 3.
        let own = put(1, b"a", b"own");
        assert_eq!(state.met(&own, 1, 4), [2, 3, 4]);
        assert_eq!(state.met(&own, 1, 3), [2, 3]);
        assert_eq!(state.met(&own, 2, 3), [3]);
        assert!(state.met(&own, 3, 3).is_empty());
        // The drop of version 4 meets every key of the two subtrees.
        assert_eq!(state.met(&put(2, b"z", b"own"), 3, 4), [4]);
    }
    // A writer that holds the floor down keeps what it may ask for.
    history.hold(Slot::Block, || 2);
    history.prune(4);
    assert_eq!(
        flat(history.through(4).record(aux(1), b"a", 2)),
        Some(b"two".to_vec())
    );
    history.release(Slot::Block);
    history.hold(Slot::Outside, || 4);
    history.prune(3);
    history.release(Slot::Outside);
    assert_eq!(history.copy().version(), 4);
    let state = history.through(4);
    assert_eq!(flat(state.record(aux(1), b"a", 3)), Some(b"three".to_vec()));
    assert_eq!(flat(state.record(aux(1), b"b", 4)), None);
    assert_eq!(state.unit(aux(2), 3).len(), 1);
    drop(state);
    // With nothing asked below the drop, its records are forgotten whole.
    history.prune(4);
    assert!(history.lock().records.is_empty());
}
