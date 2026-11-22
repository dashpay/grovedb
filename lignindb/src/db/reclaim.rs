//! Reclaiming what drops left: the background work that takes the cells,
//! the directory records and the flat pages of dropped subtrees out of the
//! store (`transactions.md` CL6, `lignin_tree::drop`).

use std::{collections::HashSet, sync::atomic::Ordering, thread};

use lignin_format::CommitVersion;
use lignin_store::flush::FlushConfig;
use lignin_tree::{
    directory::{Directory, Dropped},
    drop::{Busy, DropError, FlatDrop, FlatReclaimed, Progress, Reclaim, Reclaimer, Wait},
};

use super::store::{lock, Shared, Steps, Troubles};

/// How many page steps one call of the reclaimer may make before it
/// returns, so that a round can be ended between two calls.
const STEPS: usize = 256;

/// How many calls in a row a round makes for one dropped Merk or one range
/// delete before it goes on to the next, so that one long reclaiming does
/// not keep a round from ending.
const CALLS: usize = 64;

/// One dropped or cleared Merk whose leavings wait to be reclaimed.
#[derive(Debug)]
struct MerkJob {
    reclaim: Reclaim,
    wait: Wait,
}

/// The pages one lazy range delete left.
#[derive(Debug)]
struct FlatJob {
    drop: FlatDrop,
    wait: Wait,
}

/// What background reclaiming has done in a session
/// ([`DbStats::reclaimed`](super::DbStats::reclaimed)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReclaimStats {
    /// Dropped and cleared Merks that are reclaimed in full.
    pub merks: u64,
    /// The subtrees that were nested in them.
    pub nested: u64,
    /// Records taken out of the subtree directory.
    pub records: u64,
    /// Flat records of nested subtrees taken out of their columns.
    pub flat_records: u64,
    /// Cells taken out of tree pages.
    pub cells: u64,
    /// Pages freed: tree pages that kept no cell, and the flat pages that
    /// range deletes unlinked.
    pub pages_freed: u64,
    /// How many of the freed pages were given no free link, because no
    /// segment ever held them under their id's present life
    /// (`lignin_tree::drop`, choice 3): pages that were freed before
    /// write-back had written them.
    pub pages_never_linked: u64,
    /// Lazy range deletes whose pages are freed in full.
    pub range_deletes: u64,
    /// The bytes of the out-of-line values that were released.
    pub value_bytes: u64,
    /// Dropped Merks and range deletes that still wait: for a reader from
    /// before the drop, for a write transaction that holds a copy of a
    /// page, or for their turn.
    pub waiting: u64,
    /// Calls of the reclaimer that a page held back because an open write
    /// transaction had a private copy of it ([`Busy::Copied`]).
    pub blocked_by_copies: u64,
    /// Calls that would have freed a page which the latest commit wrote,
    /// and wait for the next commit ([`Busy::Newborn`]).
    pub blocked_by_newborn: u64,
    /// Calls that found their page changed each time they looked
    /// ([`Busy::Contended`]).
    pub blocked_by_contention: u64,
    /// Reclaimings that were given up because the store was not as its log
    /// has it ([`DropError::Broken`]). What they left stays where it is.
    pub given_up: u64,
}

/// What a session knows of the reclaiming that is owed. One round works on
/// it at a time.
#[derive(Debug, Default)]
pub(super) struct Reclaiming {
    merks: Vec<MerkJob>,
    flats: Vec<FlatJob>,
    /// The numbers of the range deletes that are known.
    known_flats: HashSet<u64>,
    /// The version at which the records of range deletes were last read.
    scanned: Option<CommitVersion>,
    /// Whether what a finished reclaiming left in the kept handles is
    /// still to be taken out of them.
    stale_handles: bool,
    done: ReclaimStats,
}

impl ReclaimStats {
    fn blocked(&mut self, busy: Busy) {
        let count = match busy {
            Busy::Copied => &mut self.blocked_by_copies,
            Busy::Newborn => &mut self.blocked_by_newborn,
            Busy::Contended => &mut self.blocked_by_contention,
        };
        *count += 1;
    }
}

impl Reclaiming {
    pub(super) fn stats(&self) -> ReclaimStats {
        ReclaimStats {
            waiting: (self.merks.len() + self.flats.len()) as u64,
            ..self.done
        }
    }

    /// Whether a reclaiming has taken the record of its Merk out and not
    /// yet all of its cells: what is left of it is known here and nowhere
    /// on disk (`lignin_tree::drop`, "Reclaiming").
    fn holds_cells(&self) -> bool {
        self.merks.iter().any(|job| job.reclaim.holds_cells())
    }
}

/// What one round of reclaiming did ([`Db::reclaim`](super::Db::reclaim)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Round {
    /// Dropped Merks and range deletes that were reclaimed in full in this
    /// round.
    pub finished: u64,
    /// Those that are left: they wait for a reader, for a write
    /// transaction, for the next commit, or for the next round.
    pub waiting: u64,
    /// Whether one of them holds cells that nothing on disk leads to any
    /// more: a session should not end before it is done.
    pub holds_cells: bool,
    /// Calls of the reclaimer in this round that a page held back, or that
    /// failed.
    pub held_back: u64,
    /// Whether the round left one of them at its bound of calls, with
    /// nothing in its way: the next round goes on at once.
    pub more: bool,
    /// Whether something of this round failed: a call of the reclaimer,
    /// or a read the round makes for one. A call that a page held back
    /// is no failure.
    pub failed: bool,
}

/// What a round does after one call of the reclaimer.
enum Next {
    /// The reclaiming is done.
    Done,
    /// The call made its steps, and more are left: the next call follows.
    More,
    /// Nothing more is made of it in this round. It is kept for a later
    /// one, or given up.
    Later(bool),
}

impl Round {
    /// Counts what one call of the reclaimer answered, for a dropped Merk
    /// and for a range delete alike, and says what the round does next.
    fn took(
        &mut self,
        made: Result<Progress, DropError>,
        done: &mut ReclaimStats,
        troubles: &Troubles,
    ) -> Next {
        match made {
            Ok(Progress::Done) => {
                self.finished += 1;
                Next::Done
            }
            Ok(Progress::More) => Next::More,
            // A page that the latest commit wrote is freed after the next
            // one, and a copy goes with its transaction: both are for a
            // later round.
            Ok(Progress::Blocked(busy)) => {
                done.blocked(busy);
                self.held_back += 1;
                Next::Later(true)
            }
            Err(error @ (DropError::Broken { .. } | DropError::GivenUp)) => {
                troubles.reclaim_failed(&error);
                done.given_up += 1;
                Next::Later(false)
            }
            // The step changed nothing, and is made again.
            Err(error) => {
                troubles.reclaim_failed(&error);
                self.held_back += 1;
                Next::Later(true)
            }
        }
    }
}

impl Shared {
    fn reclaimer(&self) -> Reclaimer<'_, Steps<'_>> {
        let store = &*self.store;
        Reclaimer::new(
            store.loader(),
            &store.pool,
            &store.table,
            &store.ids,
            &store.values,
            store.page_size,
            self.steps(),
        )
    }

    /// Reads what the drops of earlier sessions left and did not reclaim,
    /// at the version the database was opened at. Nothing else runs yet.
    pub(super) fn adopt_drops(&self, directory: &Directory) -> Result<(), DropError> {
        let guard = self.versions.guard();
        let latest = guard.latest();
        let pages = self.store.committed(&guard, latest);
        let mut work = lock(&self.store.reclaiming);
        let mut drops = directory.drops();
        while let Some(dropped) = drops.next(&pages)? {
            work.merks.push(MerkJob {
                reclaim: Reclaim::new(dropped),
                wait: Wait::new(latest),
            });
        }
        *lock(&self.store.reclaimed) = work.stats();
        Ok(())
    }

    /// Hands the reclaimer what a commit of `version` dropped or cleared.
    pub(super) fn dropped(&self, version: CommitVersion, dropped: &[Dropped]) {
        if dropped.is_empty() {
            return;
        }
        let mut queue = lock(&self.store.dropped);
        queue.extend(dropped.iter().map(|dropped| (version, *dropped)));
        drop(queue);
        self.store.signal.wake();
    }

    /// One round of the background work: the reclaimers of the store take
    /// their step, and what the drops left is reclaimed as far as it can
    /// be now. `ending` says that the session ends: only what holds cells
    /// is taken on.
    pub(super) fn reclaim_round(&self, ending: bool) -> Round {
        self.versions.reclaim();
        let store = &*self.store;
        let mut work = lock(&store.reclaiming);
        let work = &mut *work;
        // Every failure of a round is counted under the lock on the
        // work, so what is counted between here and the end of the round
        // is this round's, whoever makes a round beside it.
        let failures = || store.troubles.reclaim.load(Ordering::Relaxed);
        let failed_before = failures();
        for (version, dropped) in lock(&store.dropped).drain(..) {
            work.merks.push(MerkJob {
                reclaim: Reclaim::new(dropped),
                wait: Wait::new(version),
            });
        }
        *lock(&store.reclaimed) = work.stats();
        let reclaimer = self.reclaimer();
        let catalog = *store.catalog();
        if !ending {
            self.scan_flat_drops(work, &reclaimer);
        }

        let mut round = Round::default();
        let columns = catalog.keyspaces.columns();
        let mut merks = std::mem::take(&mut work.merks);
        merks.retain_mut(|job| {
            if ending && !job.reclaim.holds_cells() {
                return true;
            }
            if !job.wait.poll(&self.versions) {
                return true;
            }
            for _ in 0..CALLS {
                let guard = self.versions.guard();
                let Some(directory) = store.directories.at(guard.latest()) else {
                    store
                        .troubles
                        .reclaim_failed(&"no handle is kept for the latest version");
                    return true;
                };
                let made = reclaimer.reclaim(&mut job.reclaim, &directory, &columns, &guard, STEPS);
                match round.took(made, &mut work.done, &store.troubles) {
                    Next::Done => {
                        let taken = job.reclaim.reclaimed();
                        let done = &mut work.done;
                        done.merks += 1;
                        done.nested += taken.merks.len() as u64;
                        done.records += taken.records;
                        done.flat_records += taken.flat_records;
                        done.cells += taken.cells;
                        done.pages_freed += taken.pages_freed;
                        done.pages_never_linked += taken.pages_never_linked;
                        done.value_bytes += taken.value_bytes;
                        return false;
                    }
                    Next::More => {}
                    Next::Later(keep) => return keep,
                }
            }
            round.more = true;
            true
        });
        work.merks = merks;

        if !ending {
            let mut flats = std::mem::take(&mut work.flats);
            flats.retain_mut(|job| {
                if !job.wait.poll(&self.versions) {
                    return true;
                }
                let mut taken = FlatReclaimed::default();
                let keep = 'calls: {
                    for _ in 0..CALLS {
                        let guard = self.versions.guard();
                        let made = reclaimer.reclaim_flat(
                            &catalog.flat_drops,
                            &job.drop,
                            &guard,
                            STEPS,
                            &mut taken,
                        );
                        match round.took(made, &mut work.done, &store.troubles) {
                            Next::Done => {
                                work.done.range_deletes += 1;
                                work.known_flats.remove(&job.drop.id);
                                break 'calls false;
                            }
                            Next::More => {}
                            Next::Later(keep) => break 'calls keep,
                        }
                    }
                    round.more = true;
                    true
                };
                work.done.pages_freed += taken.pages_freed;
                work.done.pages_never_linked += taken.pages_never_linked;
                work.done.value_bytes += taken.value_bytes;
                keep
            });
            work.flats = flats;
        }

        work.stale_handles |= round.finished > 0;
        if work.stale_handles && !ending {
            work.stale_handles = !self.refresh_directory();
        }
        round.waiting = (work.merks.len() + work.flats.len()) as u64;
        round.holds_cells = work.holds_cells();
        round.failed = failures() != failed_before;
        *lock(&store.reclaimed) = work.stats();
        round
    }

    /// Reads the records of the lazy range deletes at the latest version,
    /// if a commit was published since they were last read: a drop, a
    /// clear and an enter over what a drop left write them.
    fn scan_flat_drops(&self, work: &mut Reclaiming, reclaimer: &Reclaimer<'_, Steps<'_>>) {
        let guard = self.versions.guard();
        let latest = guard.latest();
        if work.scanned == Some(latest) {
            return;
        }
        match reclaimer.flat_drops(&self.store.catalog().flat_drops, &guard) {
            Ok(found) => {
                // The records were read at the version that was the latest
                // during the read, which a commit can have moved on from
                // `latest`. A reader of any version before the one that
                // wrote a record still reads what the record names, so the
                // wait is for a version that is not before the read: the
                // latest now.
                let read_by = self.versions.latest();
                for drop in found {
                    if work.known_flats.insert(drop.id) {
                        work.flats.push(FlatJob {
                            drop,
                            wait: Wait::new(read_by),
                        });
                    }
                }
                // A commit that came during the read has the records read
                // again.
                work.scanned = Some(latest);
            }
            Err(error) => self.store.troubles.reclaim_failed(&error),
        }
    }

    /// Loads the subtree directory of the latest version again and keeps
    /// it in the place of the handle that is kept for that version, so that
    /// the ids of the Merks that are reclaimed leave the handles. Returns
    /// whether it did.
    ///
    /// The commit lock is held, so the version stays the latest while the
    /// handle is loaded, and the reclaiming is at rest: the handle is one
    /// that was loaded while its version was the newest
    /// ([`Directories`](crate::snapshot::Directories)).
    fn refresh_directory(&self) -> bool {
        let store = &*self.store;
        let catalog = store.catalog();
        let _commit = store.committing.lock();
        let guard = self.versions.guard();
        let latest = guard.latest();
        let pages = store.committed(&guard, latest);
        match catalog.load_directory(&pages) {
            Ok(directory) => {
                store.directories.set(latest, directory);
                true
            }
            Err(error) => {
                store.troubles.reclaim_failed(&error);
                false
            }
        }
    }

    /// The thread that reclaims: a round whenever it is woken, by a commit
    /// that dropped something or by a reader that gave up a pin on an older
    /// version (`Reclaim::wake`), and one every `idle` while nothing wakes
    /// it. When the session ends, whoever closes it goes on with the
    /// reclaimings that hold cells ([`Self::finish_reclaiming`]).
    ///
    /// **A round that failed does not bring the next round forward**, as
    /// in write-back's loop (`lignin_store::flush::WriteBack::run`): after
    /// a round in which a call of the reclaimer failed the thread waits
    /// `idle`, and twice as long after each further such round in a row,
    /// up to `FlushConfig::max_failed_wait` of the session's write-back,
    /// whoever wakes it. The wait is `idle` again after the first round in
    /// which nothing fails. A call that a page held back is no failure.
    pub(super) fn reclaim_loop(&self, idle: std::time::Duration) {
        let store = &*self.store;
        store.signal.attach(thread::current());
        // The rounds in a row, up to the last one, in which a call failed.
        let mut failed_rounds = 0_u32;
        while !store.signal.stopping() {
            #[cfg(test)]
            store.signal.rounds.fetch_add(1, Ordering::Relaxed);
            let (round, failed_wait) = self.paced_round(idle, &mut failed_rounds);
            if store.signal.stopping() {
                break;
            }
            match failed_wait {
                Some(wait) => store.signal.sleep(wait),
                // A round goes on at once only where it left a reclaiming
                // at its bound of calls. Everything else waits for a
                // commit, a reader or a write transaction, which wake the
                // thread or are asked for again after the idle time.
                None if !round.more => {
                    #[cfg(test)]
                    store.signal.idled.fetch_add(1, Ordering::Relaxed);
                    thread::park_timeout(idle)
                }
                None => {}
            }
        }
    }

    /// One round of the thread that reclaims, and how long the thread
    /// waits after it, whoever wakes it: `None` after a round in which no
    /// call of the reclaimer failed. `failed_rounds` is the thread's count
    /// of the rounds in a row in which one did.
    pub(super) fn paced_round(
        &self,
        idle: std::time::Duration,
        failed_rounds: &mut u32,
    ) -> (Round, Option<std::time::Duration>) {
        let store = &*self.store;
        let round = self.reclaim_round(false);
        if !round.failed {
            *failed_rounds = 0;
            return (round, None);
        }
        *failed_rounds = failed_rounds.saturating_add(1);
        let pacing = FlushConfig {
            idle,
            ..store.flush.config()
        };
        (round, Some(pacing.wait_after_failure(*failed_rounds)))
    }

    /// What is done for the reclaiming when a session ends: every
    /// reclaiming that holds cells goes on until it is done or held back,
    /// because what it has left is known to nothing on disk. One that is
    /// held back does not end the others: the rounds go on while one of
    /// them moves.
    pub(super) fn finish_reclaiming(&self) {
        loop {
            let round = self.reclaim_round(true);
            if !round.holds_cells || !round.more {
                return;
            }
        }
    }
}
