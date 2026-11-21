//! When a transaction read what it keeps: the versions of the leases it
//! has shown nodes under, and the key sets of the commits since.
//!
//! A node that is written with its value kept (`ValuePut::Keep`) depends on
//! that value as its writer read it (`crate::txn`, "A kept value and the
//! read it is of"). The write names the read by the node's handle, and a
//! handle says which lease it is of and nothing else. So the transaction
//! remembers, for each lease it has shown a node under, the version the
//! node was shown at, and keeps the key sets of the commits it takes in
//! afterwards. A keep whose lease has ended is compared with those: see
//! [`Reads::since`].
//!
//! Whatever cannot be answered from what is kept is [`Since::Unknown`],
//! which a write takes as a commit that wrote the key. So nothing here has
//! to be complete for the rule to hold: a lease that was never noted, or a
//! key set that was never taken in, costs a conflict that may be false.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use lignin_format::CommitVersion;
use lignin_tree::node::Lease;

use super::key::{KeySet, Unit};

/// How many key sets a transaction keeps for the keeps it may be given: a
/// kept value whose writer read it more than this many commits before the
/// write is taken to be met ([`Since::Unknown`]).
///
/// Not measured. What it bounds is the memory of a transaction that has
/// given out a handle and beside which commits land; what it costs is a
/// conflict that may be false for a writer that is that many commits
/// behind.
///
/// It is also how many key sets a transaction with no write holds back
/// where every transaction takes them from (`Commits`): it would keep no
/// more of them here.
pub(crate) const KEPT_COMMITS: usize = 64;

/// What the commits since a read did to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Since {
    /// No commit was taken in since the read, or none of them wrote the
    /// key.
    Unwritten,
    /// A commit wrote the key, or a range that holds it, after the read.
    Written,
    /// The transaction cannot say: it does not know the lease, or it no
    /// longer has the key set of a commit since the read.
    Unknown,
}

/// The reads a transaction's kept values can depend on.
#[derive(Debug, Default)]
pub(crate) struct Reads {
    /// For each lease the transaction has shown a node under, the version
    /// the node was shown at.
    leases: HashMap<Lease, CommitVersion>,
    /// The lease that was noted last: a node that is shown under the same
    /// lease as the one before it, which is nearly every one, costs one
    /// comparison and no lookup.
    last: Option<Lease>,
    /// The lease of the transaction's frames that was dated last, or that
    /// is left undated on purpose ([`Self::forget_frames`]). It stays when
    /// the lease's date goes with the key sets ([`Self::took`]): a lease
    /// that was dated once is never dated again at a later version.
    framed: Option<Lease>,
    /// The key sets of the commits the transaction took in, oldest first.
    /// Their versions follow one another with no gap.
    sets: VecDeque<(CommitVersion, Arc<KeySet>)>,
}

impl Reads {
    /// Whether the transaction remembers a lease it has shown a node under.
    pub(crate) fn any(&self) -> bool {
        !self.leases.is_empty()
    }

    /// Forgets every lease and every key set: the transaction has ended,
    /// and with it the handle of every node it showed.
    pub(crate) fn clear(&mut self) {
        self.leases.clear();
        self.last = None;
        self.framed = None;
        self.sets.clear();
    }

    /// A node is shown under `lease`, as the committed version `at` has it
    /// or as the transaction's frames over that version show it: its
    /// handle is of that lease.
    ///
    /// A lease is of one version. Should it be noted for two, the one it
    /// was noted for first stands.
    pub(crate) fn note(&mut self, lease: Lease, at: CommitVersion) {
        if self.last != Some(lease) {
            self.leases.entry(lease).or_insert(at);
            self.last = Some(lease);
        }
    }

    /// [`Self::note`] for the lease of the transaction's frames, which is
    /// noted with every node the frames show and with every write that
    /// hands handles back. It costs one comparison while the frames are
    /// those that were dated last, whatever other lease was noted in
    /// between.
    pub(crate) fn note_frames(&mut self, lease: Lease, at: CommitVersion) {
        if self.framed != Some(lease) {
            self.framed = Some(lease);
            self.leases.entry(lease).or_insert(at);
        }
    }

    /// Forgets `lease`, the lease of the transaction's frames: a keep by a
    /// handle of it is not answered for any more ([`Since::Unknown`]), and
    /// the lease is not dated again by [`Self::note_frames`].
    pub(crate) fn forget_frames(&mut self, lease: Lease) {
        self.leases.remove(&lease);
        self.framed = Some(lease);
    }

    /// The transaction took in the key set `keys` of the commit `version`.
    ///
    /// None is taken while no lease is remembered, and those that were kept
    /// go then: a lease is noted in the operation that shows its first
    /// node, or when its cursor is opened, and what it needs is the key
    /// sets that are taken in after that.
    pub(crate) fn took(&mut self, version: CommitVersion, keys: &Arc<KeySet>) {
        if self.leases.is_empty() {
            self.sets.clear();
            return;
        }
        let newest = self.sets.back().map(|(newest, _)| *newest);
        if newest.is_some_and(|newest| version <= newest) {
            // A watch that starts again is given the newest key set once
            // more.
            return;
        }
        let follows = newest.is_none_or(|newest| newest.next() == Some(version));
        if !follows {
            // A gap: what is before it answers for no read any more.
            self.sets.clear();
        }
        self.sets.push_back((version, Arc::clone(keys)));
        let full = self.sets.len() > KEPT_COMMITS;
        if full {
            self.sets.pop_front();
        }
        if full || !follows {
            // A lease from before the oldest key set but one would need a
            // key set that is gone: it is as good as unknown.
            let oldest = self.sets.front().map(|(oldest, _)| *oldest);
            let before = self.leases.len();
            self.leases.retain(|_, at| at.next() >= oldest);
            if self.leases.len() != before {
                self.last = None;
            }
        }
    }

    /// What the commits did to the record key `key` of `unit` since a node
    /// was read under `lease`, for a write at the version `at` that has
    /// taken in every commit up to it.
    pub(crate) fn since(&self, lease: Lease, at: CommitVersion, unit: Unit, key: &[u8]) -> Since {
        let Some(&read) = self.leases.get(&lease) else {
            return Since::Unknown;
        };
        if read >= at {
            return Since::Unwritten;
        }
        // The key sets of the commits after `read`, up to `at`.
        let (Some((oldest, _)), Some((newest, _))) = (self.sets.front(), self.sets.back()) else {
            return Since::Unknown;
        };
        let first = read.next().expect("a version before another");
        if *oldest > first || *newest < at {
            return Since::Unknown;
        }
        let skip = (first.get() - oldest.get()) as usize;
        let written = (self.sets.iter().skip(skip))
            .take_while(|(version, _)| *version <= at)
            .any(|(_, keys)| keys.meets_key(unit, key));
        match written {
            true => Since::Written,
            false => Since::Unwritten,
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::txn::key::{Column, Subtree};

    fn version(raw: u64) -> CommitVersion {
        CommitVersion::new(raw).expect("a version")
    }

    fn unit(name: u8) -> Unit {
        Unit::new(Column::Data, Subtree::new([name; 32]))
    }

    fn keys(write: impl FnOnce(&mut KeySet)) -> Arc<KeySet> {
        let mut set = KeySet::new();
        write(&mut set);
        Arc::new(set)
    }

    #[test]
    fn a_read_is_answered_from_the_key_sets_of_the_commits_after_it() {
        let mut reads = Reads::default();
        let (old, new, never) = (Lease::take(), Lease::take(), Lease::take());
        assert!(!reads.any());
        reads.note(old, version(3));
        reads.note(old, version(9));
        assert!(reads.any());
        // Nothing was taken in since: the read is of the write's version.
        assert_eq!(
            reads.since(old, version(3), unit(1), b"k"),
            Since::Unwritten
        );
        assert_eq!(
            reads.since(never, version(3), unit(1), b"k"),
            Since::Unknown
        );

        reads.took(
            version(4),
            &keys(|set| {
                set.key(unit(1), b"other");
            }),
        );
        reads.took(
            version(5),
            &keys(|set| {
                set.key(unit(1), b"k");
            }),
        );
        reads.note(new, version(5));
        reads.took(
            version(6),
            &keys(|set| {
                set.range(unit(2), b"a", Some(b"c"));
            }),
        );
        reads.took(
            version(7),
            &keys(|set| {
                set.subtree(Subtree::new([3; 32]));
            }),
        );

        // The commit of version 5 wrote the key: after the old read, and
        // not after the new one.
        assert_eq!(
            reads.since(old, version(4), unit(1), b"k"),
            Since::Unwritten
        );
        assert_eq!(reads.since(old, version(5), unit(1), b"k"), Since::Written);
        assert_eq!(reads.since(old, version(7), unit(1), b"k"), Since::Written);
        assert_eq!(
            reads.since(new, version(7), unit(1), b"k"),
            Since::Unwritten
        );
        assert_eq!(
            reads.since(old, version(7), unit(1), b"j"),
            Since::Unwritten
        );
        // The unit is part of the key.
        assert_eq!(
            reads.since(old, version(7), unit(4), b"k"),
            Since::Unwritten
        );
        // A range and a dropped subtree hold the keys they cover.
        assert_eq!(reads.since(new, version(7), unit(2), b"b"), Since::Written);
        assert_eq!(
            reads.since(new, version(5), unit(2), b"b"),
            Since::Unwritten
        );
        assert_eq!(
            reads.since(new, version(7), unit(2), b"c"),
            Since::Unwritten
        );
        assert_eq!(
            reads.since(new, version(7), unit(3), b"any"),
            Since::Written
        );
        assert_eq!(
            reads.since(new, version(6), unit(3), b"any"),
            Since::Unwritten
        );
        // A write that has not taken in every commit up to its version
        // cannot be answered.
        assert_eq!(reads.since(old, version(8), unit(1), b"j"), Since::Unknown);

        reads.clear();
        assert!(!reads.any());
        assert_eq!(reads.since(old, version(7), unit(1), b"j"), Since::Unknown);
    }

    #[test]
    fn a_read_from_before_the_key_sets_that_are_kept_is_not_known() {
        let mut reads = Reads::default();
        let (first, second, last) = (Lease::take(), Lease::take(), Lease::take());
        reads.note(first, version(10));
        reads.note(second, version(12));
        let none = keys(|_| ());
        let most = 10 + KEPT_COMMITS as u64;
        for commit in 11..=most {
            reads.took(version(commit), &none);
        }
        // Every key set since the first read is there.
        assert_eq!(
            reads.since(first, version(most), unit(1), b"k"),
            Since::Unwritten
        );
        // One more, and the oldest goes: the first read is not known any
        // more, and the later ones are.
        reads.took(version(most + 1), &none);
        reads.note(last, version(most + 1));
        assert_eq!(
            reads.since(first, version(most + 1), unit(1), b"k"),
            Since::Unknown
        );
        assert_eq!(
            reads.since(second, version(most + 1), unit(1), b"k"),
            Since::Unwritten
        );
        assert_eq!(
            reads.since(last, version(most + 1), unit(1), b"k"),
            Since::Unwritten
        );

        // A gap in what was taken in: nothing before it is answered.
        reads.took(version(most + 5), &none);
        assert_eq!(
            reads.since(second, version(most + 5), unit(1), b"k"),
            Since::Unknown
        );
        assert_eq!(
            reads.since(last, version(most + 5), unit(1), b"k"),
            Since::Unknown
        );
        reads.note(last, version(most + 5));
        assert_eq!(
            reads.since(last, version(most + 5), unit(1), b"k"),
            Since::Unwritten
        );
    }

    #[test]
    fn a_key_set_is_kept_only_while_a_lease_is_remembered() {
        let mut reads = Reads::default();
        let none = keys(|_| ());
        let wrote = keys(|set| {
            set.key(unit(1), b"k");
        });
        for commit in 4..=6 {
            reads.took(version(commit), &wrote);
        }
        assert!(reads.sets.is_empty());
        // A lease of the version that was taken in last needs none of
        // them. One of a version before it cannot be answered for: a
        // cursor that was opened then, and tells of its lease only now.
        let (now, earlier) = (Lease::take(), Lease::take());
        reads.note(now, version(6));
        reads.note(earlier, version(5));
        reads.took(version(7), &none);
        assert_eq!(reads.sets.len(), 1);
        assert_eq!(
            reads.since(now, version(7), unit(1), b"k"),
            Since::Unwritten
        );
        assert_eq!(
            reads.since(earlier, version(7), unit(1), b"k"),
            Since::Unknown
        );
        // The version a lease was first noted for stands.
        reads.note(earlier, version(7));
        reads.note(now, version(7));
        reads.took(version(8), &wrote);
        assert_eq!(reads.since(now, version(8), unit(1), b"k"), Since::Written);
        // A key set that is taken a second time changes nothing.
        reads.took(version(8), &none);
        reads.took(version(7), &wrote);
        assert_eq!(reads.sets.len(), 2);
        assert_eq!(
            reads.since(now, version(7), unit(1), b"k"),
            Since::Unwritten
        );
        // A lease that is forgotten is not known.
        reads.forget_frames(earlier);
        assert_eq!(
            reads.since(earlier, version(8), unit(1), b"j"),
            Since::Unknown
        );
        // The frames' lease is dated once, whatever is noted in between,
        // and not again once it was forgotten.
        reads.note_frames(earlier, version(8));
        assert_eq!(
            reads.since(earlier, version(8), unit(1), b"j"),
            Since::Unknown
        );
        let frames = Lease::take();
        reads.note_frames(frames, version(8));
        reads.note(now, version(8));
        reads.note_frames(frames, version(9));
        assert_eq!(
            reads.since(frames, version(8), unit(1), b"k"),
            Since::Unwritten
        );
        // With the last lease forgotten, the key sets go.
        for commit in 9..=9 + KEPT_COMMITS as u64 {
            reads.took(version(commit), &none);
        }
        assert!(!reads.any());
        reads.took(version(10 + KEPT_COMMITS as u64), &none);
        assert!(reads.sets.is_empty());
    }
}
