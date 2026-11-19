//! The key sets of the commits that open write transactions have not looked
//! at yet.

use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use lignin_format::CommitVersion;

use super::key::KeySet;

/// The key sets of the newest commits of one database, for as long as an
/// open write transaction has still to compare its write set with them
/// (`transactions.md` CF3, and the overlap set of WT7).
///
/// - **A committer** hands over the key set of its commit
///   ([`Self::record`]) while it holds the commit lock, *before* it
///   publishes the version. So whoever reads a latest version finds the key
///   set of every commit up to it here.
/// - **A write transaction** that has a write set watches from the moment
///   before its first write ([`Self::watch`]) and takes the key sets of the
///   commits it has not seen at the start of each of its operations
///   ([`Watch::take`]). A transaction with no write has nothing a commit
///   could meet, and watches nothing.
///
/// A key set is kept until every watching transaction has taken it. The
/// newest one is kept in any case: a transaction that starts to watch may
/// read a latest version that is one before it.
///
/// What this costs is the keys of the commits that are published while a
/// transaction with writes runs no operation. Remembering only the entries
/// a commit meets, at the commit, would bound that, and would make the
/// committer take each transaction's lock. TXN-05 owns how CF3 is kept and
/// can change this without changing what a transaction is told.
pub struct Commits {
    inner: Mutex<Inner>,
}

struct Inner {
    /// The key sets that are kept, oldest first, with their versions.
    sets: VecDeque<(CommitVersion, Arc<KeySet>)>,
    /// The newest version a key set was recorded for.
    newest: CommitVersion,
    /// For each watcher, the version up to which it has taken the key sets.
    watchers: Vec<Option<CommitVersion>>,
}

impl Inner {
    /// Drops the key sets that every watcher has taken, but for the newest.
    fn prune(&mut self) {
        let needed = self.watchers.iter().flatten().min().copied();
        while self.sets.len() > 1 {
            let oldest = self.sets.front().expect("more than one").0;
            if needed.is_some_and(|needed| oldest > needed) {
                break;
            }
            self.sets.pop_front();
        }
    }
}

impl Commits {
    /// The commits of a database whose latest committed version is
    /// `latest`: none of them is recorded.
    pub fn new(latest: CommitVersion) -> Self {
        Self {
            inner: Mutex::new(Inner {
                sets: VecDeque::new(),
                newest: latest,
                watchers: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records the key set of the commit of `version` (CM3 step 8).
    ///
    /// The committer calls this under the commit lock, after it has
    /// installed the commit and before it publishes `version`
    /// ([`Versions::publish`](crate::snapshot::Versions::publish)), so
    /// that no transaction reads `version` as the latest and finds no key
    /// set for it. Versions are recorded in order, each once.
    ///
    /// # Panics
    ///
    /// When `version` is not after every recorded version.
    pub fn record(&self, version: CommitVersion, keys: KeySet) {
        let mut inner = self.lock();
        assert!(
            version > inner.newest,
            "version {version} is recorded after {}",
            inner.newest
        );
        inner.newest = version;
        inner.sets.push_back((version, Arc::new(keys)));
        inner.prune();
    }

    /// How many key sets are kept.
    pub fn kept(&self) -> usize {
        self.lock().sets.len()
    }

    /// Starts to watch: every key set that is recorded from here on is kept
    /// until the watch has taken it, and so is the newest one recorded
    /// before.
    ///
    /// A transaction calls this before it reads the latest version its
    /// first write is dated with (WT5). That version is the newest recorded
    /// one or the one before it, so every commit after it is kept.
    pub(crate) fn watch(&self) -> Watch<'_> {
        let mut inner = self.lock();
        let newest = inner.newest;
        let kept = inner.sets.back().map(|(version, _)| *version);
        // The newest key set is kept whoever watches. One version before it
        // is the latest version a transaction can still read.
        let seen = match kept {
            Some(version) if version == newest => {
                CommitVersion::new(newest.get() - 1).expect("a version before a commit")
            }
            _ => newest,
        };
        let slot = match inner.watchers.iter().position(Option::is_none) {
            Some(slot) => slot,
            None => {
                inner.watchers.push(None);
                inner.watchers.len() - 1
            }
        };
        inner.watchers[slot] = Some(seen);
        Watch {
            commits: self,
            slot,
            seen,
        }
    }
}

impl fmt::Debug for Commits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.lock();
        f.debug_struct("Commits")
            .field("newest", &inner.newest)
            .field("kept", &inner.sets.len())
            .field("watchers", &inner.watchers.iter().flatten().count())
            .finish()
    }
}

/// The hold of one write transaction on the key sets it has not taken.
pub(crate) struct Watch<'c> {
    commits: &'c Commits,
    slot: usize,
    seen: CommitVersion,
}

impl Watch<'_> {
    /// The version up to which the key sets were taken.
    #[cfg(test)]
    pub(crate) fn seen(&self) -> CommitVersion {
        self.seen
    }

    /// Appends to `out` the key set of every commit after the ones taken
    /// before, up to and including `upto`, oldest first, each with its
    /// version.
    pub(crate) fn take(
        &mut self,
        upto: CommitVersion,
        out: &mut Vec<(CommitVersion, Arc<KeySet>)>,
    ) {
        if upto <= self.seen {
            return;
        }
        let mut inner = self.commits.lock();
        let seen = self.seen;
        out.extend(
            inner
                .sets
                .iter()
                .filter(|(version, _)| *version > seen && *version <= upto)
                .cloned(),
        );
        self.seen = upto;
        inner.watchers[self.slot] = Some(upto);
        inner.prune();
    }
}

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        let mut inner = self.commits.lock();
        inner.watchers[self.slot] = None;
        inner.prune();
    }
}

impl fmt::Debug for Watch<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Watch").field(&self.seen).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txn::key::{Column, Subtree, Unit};

    fn version(raw: u64) -> CommitVersion {
        CommitVersion::new(raw).expect("a version")
    }

    fn keys(key: &[u8]) -> KeySet {
        let mut set = KeySet::new();
        set.key(Unit::new(Column::Aux, Subtree::new([0; 32])), key);
        set
    }

    fn taken(watch: &mut Watch<'_>, upto: u64) -> Vec<u64> {
        let mut out = Vec::new();
        watch.take(version(upto), &mut out);
        out.iter().map(|(version, _)| version.get()).collect()
    }

    #[test]
    fn with_nobody_watching_only_the_newest_key_set_is_kept() {
        let commits = Commits::new(version(4));
        assert_eq!(commits.kept(), 0);
        for raw in 5..9 {
            commits.record(version(raw), keys(b"k"));
            assert_eq!(commits.kept(), 1);
        }
        assert!(format!("{commits:?}").contains("kept: 1"));
    }

    #[test]
    fn a_watch_is_given_every_commit_after_the_version_it_can_have_read() {
        let commits = Commits::new(version(0));
        // Nothing is recorded: the latest version is the one it was opened
        // with.
        let mut first = commits.watch();
        assert_eq!(first.seen(), version(0));
        commits.record(version(1), keys(b"a"));
        commits.record(version(2), keys(b"b"));
        // The newest recorded commit may not be published yet, so a watch
        // that starts now is also given that one.
        let mut second = commits.watch();
        assert_eq!(second.seen(), version(1));
        commits.record(version(3), keys(b"c"));
        assert_eq!(commits.kept(), 3);

        assert_eq!(taken(&mut second, 2), [2]);
        assert_eq!(taken(&mut second, 2), [0u64; 0]);
        assert_eq!(taken(&mut first, 3), [1, 2, 3]);
        // What both have taken is dropped.
        assert_eq!(commits.kept(), 1);
        assert_eq!(taken(&mut second, 3), [3]);
        assert_eq!(format!("{first:?}"), format!("Watch({:?})", version(3)));
    }

    #[test]
    fn a_dropped_watch_holds_nothing_back() {
        let commits = Commits::new(version(0));
        let watch = commits.watch();
        let other = commits.watch();
        for raw in 1..6 {
            commits.record(version(raw), keys(b"k"));
        }
        assert_eq!(commits.kept(), 5);
        drop(watch);
        assert_eq!(commits.kept(), 5);
        drop(other);
        assert_eq!(commits.kept(), 1);
        // The slot is used again.
        let mut again = commits.watch();
        assert_eq!(again.seen(), version(4));
        assert_eq!(taken(&mut again, 5), [5]);
    }

    /// Every way two commits can interleave with a transaction that starts
    /// to watch, reads the latest version for its first write, and later
    /// takes the key sets up to the latest version it reads then: it is
    /// given exactly the commits after its first-write version (CF3).
    #[test]
    fn a_transaction_is_given_every_commit_after_its_first_write_in_every_interleaving() {
        // The committer records a key set and then publishes the version,
        // twice. The transaction's three steps fall between those four.
        let mut interleavings = 0;
        for places in 0u32..1 << 7 {
            if places.count_ones() != 3 {
                continue;
            }
            interleavings += 1;
            let commits = Commits::new(version(10));
            let mut latest = version(10);
            let (mut committer, mut reader) = (0, 0);
            let mut watch = None;
            let mut first_write = None;
            let mut taken = None;
            for step in 0..7 {
                if places & (1 << step) == 0 {
                    let commit = version(11 + committer / 2);
                    match committer % 2 {
                        0 => commits.record(commit, keys(b"k")),
                        _ => latest = commit,
                    }
                    committer += 1;
                    continue;
                }
                match reader {
                    0 => watch = Some(commits.watch()),
                    1 => first_write = Some(latest),
                    _ => {
                        let mut out = Vec::new();
                        (watch.as_mut().expect("it watches")).take(latest, &mut out);
                        taken = Some((latest, out));
                    }
                }
                reader += 1;
            }
            let (first_write, (upto, taken)) = (first_write.unwrap(), taken.unwrap());
            let taken: Vec<u64> = taken.iter().map(|(version, _)| version.get()).collect();
            let after: Vec<u64> = (first_write.get() + 1..=upto.get()).collect();
            assert!(
                after.iter().all(|version| taken.contains(version)),
                "{places:07b}: first write at {first_write:?}, taken {taken:?} up to {upto:?}"
            );
            assert!(taken.iter().all(|version| *version <= upto.get()));
        }
        assert_eq!(interleavings, 35);
    }

    #[test]
    #[should_panic(expected = "is recorded after")]
    fn a_version_is_recorded_once() {
        let commits = Commits::new(version(2));
        commits.record(version(2), keys(b"k"));
    }
}
