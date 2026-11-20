//! The key sets of the commits that open write transactions have not looked
//! at yet.

use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use lignin_format::CommitVersion;

use super::{key::KeySet, reads::KEPT_COMMITS};

/// The key sets of the newest commits of one database, for as long as an
/// open write transaction has still to compare its write set with them
/// (`transactions.md` CF3, and the overlap set of WT7).
///
/// - **A committer** hands over the key set of its commit
///   ([`Self::record`]) while it holds the commit lock, *before* it
///   publishes the version. So whoever reads a latest version finds the key
///   set of every commit up to it here.
/// - **A write transaction** that has a write set watches from the moment
///   before its first write (`watch`) and takes the key sets of the
///   commits it has not seen at the start of each of its operations
///   (`Watch::take`). One that has been shown a node watches from before
///   that read, and one that has opened a cursor over a Merk from when it
///   opened it, for a write that keeps a node's value (`super::reads`). A
///   transaction with no write that remembers no such read has nothing a
///   commit could meet. It does not start to watch, and one that watches
///   still, because it has forgotten its reads since, gives the watch up
///   with its next read that shows no node, or when it ends.
///
/// How long a key set is kept depends on what the transactions that have
/// not taken it need it for:
///
/// - **A transaction with a write holds every key set until it has taken
///   it** (`Watch::hold_all`). The conflict rule compares its writes with
///   every commit since the first of them, and never answers "cannot
///   tell" (CF3).
/// - **A transaction with no write holds the newest 64**
///   (`reads::KEPT_COMMITS`; `Watch::hold_newest`, which a watch starts
///   with). It uses key sets
///   only to date its reads, for a write that keeps a value, and it keeps
///   no more of them than that itself: a read from further back is taken
///   to be met whatever the commits since wrote (`super::reads`). What it
///   is given then has a gap, which it takes as that. The newest of them
///   may be of a commit that is not published yet, which the transaction
///   is not given with the others.
///
/// The newest key set is kept in any case: a transaction that starts to
/// watch may read a latest version that is one before it.
///
/// What this costs is the keys of the commits that are published while a
/// transaction that has a write runs no operation. Remembering only the
/// entries a commit meets, at the commit, would bound that, and would make
/// the committer take each transaction's lock. The commit keeps CF3 this
/// way (`crate::commit`, "The conflict rule"). A transaction that has been
/// shown a node and has no write holds a bounded number, however long it
/// runs no operation.
pub struct Commits {
    inner: Mutex<Inner>,
}

struct Inner {
    /// The key sets that are kept, oldest first, with their versions.
    sets: VecDeque<(CommitVersion, Arc<KeySet>)>,
    /// The newest version a key set was recorded for.
    newest: CommitVersion,
    /// The watchers, each in its slot.
    watchers: Vec<Option<Watcher>>,
}

/// What one watching transaction has taken, and what it holds.
#[derive(Debug, Clone, Copy)]
struct Watcher {
    /// The version up to which it has taken the key sets.
    seen: CommitVersion,
    /// Whether it holds every key set it has not taken, and not only the
    /// newest [`KEPT_COMMITS`] of them.
    all: bool,
}

impl Inner {
    /// Drops the key sets that no watcher holds, but for the newest: those
    /// that every watcher has taken, and those that only watchers with no
    /// write have not taken and that are not among the newest
    /// [`KEPT_COMMITS`].
    fn prune(&mut self) {
        // The oldest version taken by a watcher that holds every key set,
        // and by one that holds the newest.
        let least = |all: bool| {
            let watchers = self.watchers.iter().flatten();
            let seen = watchers.filter(|watcher| watcher.all == all);
            seen.map(|watcher| watcher.seen).min()
        };
        let (all, newest) = (least(true), least(false));
        while self.sets.len() > 1 {
            let oldest = self.sets.front().expect("more than one").0;
            let among_newest = self.sets.len() <= KEPT_COMMITS;
            let held = all.is_some_and(|seen| oldest > seen)
                || (among_newest && newest.is_some_and(|seen| oldest > seen));
            if held {
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
    /// Nothing that can fail comes between this call and the publication:
    /// a recorded key set is not taken back, and it is given to a
    /// transaction only once `version` is the latest.
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

    /// The newest version whose key set was recorded, or the version the
    /// database had when these were made: the latest version, once the
    /// commit that recorded it has published it.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn newest(&self) -> CommitVersion {
        self.lock().newest
    }

    /// How many key sets are kept.
    pub fn kept(&self) -> usize {
        self.lock().sets.len()
    }

    /// Starts to watch, as a transaction with no write does. Of the newest
    /// key set recorded before and of those that are recorded from here
    /// on, the newest [`KEPT_COMMITS`] are kept until the watch has taken
    /// them.
    ///
    /// A transaction that writes has the watch hold every key set
    /// ([`Watch::hold_all`]) before it reads the latest version its first
    /// write is dated with (WT5). That version is the newest recorded one
    /// or the one before it, so every commit after it is kept.
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
        let watcher = Watcher { seen, all: false };
        inner.watchers[slot] = Some(watcher);
        Watch {
            commits: self,
            slot,
            watcher,
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
    /// What its slot holds.
    watcher: Watcher,
}

impl Watch<'_> {
    /// Holds every key set that is there now and every one that is
    /// recorded from here on, until the watch has taken it: the
    /// transaction has a write, or is about to make its first, for which
    /// it reads the latest version after this call (CF3, WT5).
    pub(crate) fn hold_all(&mut self) {
        self.hold(true);
    }

    /// Holds the newest [`KEPT_COMMITS`] key sets only: the transaction
    /// has no write, and dates its reads with them.
    pub(crate) fn hold_newest(&mut self) {
        self.hold(false);
    }

    fn hold(&mut self, all: bool) {
        if self.watcher.all == all {
            return;
        }
        self.watcher.all = all;
        let mut inner = self.commits.lock();
        inner.watchers[self.slot] = Some(self.watcher);
        inner.prune();
    }

    /// The version up to which the key sets were taken.
    #[cfg(test)]
    pub(crate) fn seen(&self) -> CommitVersion {
        self.watcher.seen
    }

    /// Appends to `out` the key set of every commit after the ones taken
    /// before, up to and including `upto`, oldest first, each with its
    /// version: every one of them for a watch that holds all, and those
    /// that are kept still for one that holds the newest. What such a
    /// watch is not given is older than everything it is given.
    pub(crate) fn take(
        &mut self,
        upto: CommitVersion,
        out: &mut Vec<(CommitVersion, Arc<KeySet>)>,
    ) {
        if upto <= self.watcher.seen {
            return;
        }
        let mut inner = self.commits.lock();
        let seen = self.watcher.seen;
        out.extend(
            inner
                .sets
                .iter()
                .filter(|(version, _)| *version > seen && *version <= upto)
                .cloned(),
        );
        self.watcher.seen = upto;
        inner.watchers[self.slot] = Some(self.watcher);
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
        f.debug_tuple("Watch").field(&self.watcher.seen).finish()
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

    /// A watch that holds the newest key sets lets what is older go while
    /// it takes none, and is then given what is left. One that holds all
    /// holds every key set, and a watch goes from one to the other.
    #[test]
    fn a_watch_holds_the_newest_key_sets_until_it_is_told_to_hold_all() {
        let kept = KEPT_COMMITS as u64;
        let commits = Commits::new(version(0));
        let mut reads = commits.watch();
        for raw in 1..=1000 {
            commits.record(version(raw), keys(b"k"));
            assert_eq!(commits.kept() as u64, raw.min(kept));
        }
        // It is given the newest, in order, and nothing from before them.
        let newest: Vec<u64> = (1001 - kept..=1000).collect();
        assert_eq!(taken(&mut reads, 1000), newest);
        assert_eq!(commits.kept(), 1);

        // From when it holds all, nothing that is there goes.
        for raw in 1001..=1100 {
            commits.record(version(raw), keys(b"k"));
        }
        assert_eq!(commits.kept() as u64, kept);
        reads.hold_all();
        reads.hold_all();
        for raw in 1101..=1500 {
            commits.record(version(raw), keys(b"k"));
        }
        assert_eq!(commits.kept() as u64, kept + 400);
        // And back: what is older than the newest goes at once.
        reads.hold_newest();
        assert_eq!(commits.kept() as u64, kept);

        // Beside a watch that holds all, the key sets stay for that one.
        let mut writes = commits.watch();
        writes.hold_all();
        assert_eq!(writes.seen(), version(1499));
        for raw in 1501..=1700 {
            commits.record(version(raw), keys(b"k"));
        }
        assert_eq!(commits.kept(), 201);
        assert_eq!(
            taken(&mut writes, 1700),
            (1500..=1700).collect::<Vec<u64>>()
        );
        assert_eq!(commits.kept() as u64, kept);
        let newest: Vec<u64> = (1701 - kept..=1700).collect();
        assert_eq!(taken(&mut reads, 1700), newest);
        assert_eq!(commits.kept(), 1);
    }

    /// CF3 for a transaction that watched with no write before: it holds
    /// all before it reads the version its first write is dated with, and
    /// is given every commit after that version, however many were
    /// recorded before and after. The newest recorded commit may not be
    /// published yet when it reads.
    #[test]
    fn a_watch_that_comes_to_hold_all_is_given_every_commit_after_its_first_write() {
        for unpublished in [0, 1] {
            let commits = Commits::new(version(0));
            let mut watch = commits.watch();
            for raw in 1..=300 {
                commits.record(version(raw), keys(b"k"));
            }
            watch.hold_all();
            let first_write = 300 - unpublished;
            for raw in 301..=600 {
                commits.record(version(raw), keys(b"k"));
            }
            let taken = taken(&mut watch, 600);
            let after: Vec<u64> = (first_write + 1..=600).collect();
            assert_eq!(&taken[taken.len() - after.len()..], after, "{unpublished}");
        }
    }

    #[test]
    #[should_panic(expected = "is recorded after")]
    fn a_version_is_recorded_once() {
        let commits = Commits::new(version(2));
        commits.record(version(2), keys(b"k"));
    }
}
