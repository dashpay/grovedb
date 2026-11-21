//! OPS-08: a device that fills at random under a database whose background
//! work runs (`docs/lignindb/disk-full-and-backup.md`, DF1 to DF9).
//!
//! The sweep of `full.rs` refuses each operation of one script, with the
//! background work at rest. Here write-back and the reclaimer run on
//! their threads, and the device fills where the seed says: for one
//! operation, from an operation on, with syncs that are refused or that
//! work, or by the bytes its files hold. The session writes what the seed
//! draws, flushes, is closed and opened, and is looked at after a power
//! cut, with the device full and with room. The same model says what
//! every commit was answered with.
//!
//! Whenever there is room again, one of two holds and the session says
//! which: the next commit is taken, or the session says that it has
//! ended, and the one after it takes the commit.

use lignin_store::wal::SyncPolicy;

use super::full::{Mode, Session};
use crate::{
    backup::tests::world::{drawn, next},
    txn::tests::store::Rng,
};

const SEEDS: u64 = 12;
const ROUNDS: u64 = 40;

/// The device has room again. Says whether the session that was open took
/// the next commit, and not the one after it.
fn room(session: &mut Session) -> bool {
    session.fs.clear_rules();
    session.fs.set_capacity(None);
    session.told.note("|room|");
    let stayed = session.db.is_some();
    let before = session.told.latest;
    session.write("with room", next, false);
    if session.told.latest == before + 1 {
        return stayed;
    }
    // The commit was refused: the session had ended, said so, and was
    // closed for it.
    assert!(
        session.db.is_none(),
        "with room a commit is refused by a session that does not say that it has ended"
    );
    session.write("opened again", next, false);
    assert_eq!(
        session.told.latest,
        before + 1,
        "with room a commit is refused by a session that was opened for it"
    );
    false
}

#[test]
fn a_device_that_fills_at_random_under_a_running_database_never_corrupts_it() {
    let (mut went_on, mut reopened, mut cuts, mut published) = (0, 0, 0, 0);
    for seed in 0..SEEDS {
        let sync = match seed % 2 {
            0 => SyncPolicy::Never,
            _ => SyncPolicy::EveryCommit,
        };
        let mut session = Session::new(100 + seed, sync, false);
        let mut rng = Rng::new(seed + 1);
        let mut number = 0;
        let mut full = false;
        for _ in 0..ROUNDS {
            match rng.below(9) {
                0 => session.fs.add_rule(Mode::Once.rule(rng.below(40) as u64)),
                1 if !full => {
                    session.fs.add_rule(Mode::Full.rule(rng.below(40) as u64));
                    full = true;
                }
                2 if !full => {
                    let rule = Mode::FullButSyncs.rule(rng.below(40) as u64);
                    session.fs.add_rule(rule);
                    full = true;
                }
                3 if !full => {
                    let more = rng.below(96 << 10) as u64;
                    let capacity = session.fs.used_bytes() + more;
                    session.fs.set_capacity(Some(capacity));
                    full = true;
                }
                4..=5 if full => {
                    match room(&mut session) {
                        true => went_on += 1,
                        false => reopened += 1,
                    }
                    full = false;
                }
                _ => {}
            }
            for _ in 0..1 + rng.below(3) {
                number += 1;
                let in_txn = rng.chance(30);
                session.write("w", |state| drawn(state, &mut rng, number), in_txn);
            }
            if rng.chance(15) {
                session.flush();
            }
            if rng.chance(12) {
                session.check_cut("in a round");
                cuts += 1;
            }
            if rng.chance(8) {
                session.close();
            }
            session.check("in a round");
        }
        match room(&mut session) {
            true => went_on += 1,
            false => reopened += 1,
        }
        session.flush();
        assert_eq!(
            session.told.durable, session.told.latest,
            "a flush with room"
        );
        session.check_cut("at the end");
        // The background work comes to rest, and the store is sound as it
        // is and as the next open has it.
        session.rest();
        session.check("at rest");
        assert!(session.close(), "a close with room");
        assert!(session.open(), "the database does not open again");
        session.check("opened again");
        assert!(session.close(), "the last close");
        published += session.told.latest;
    }
    // Which of the two a seed meets is the threads' to decide: both are
    // counted, and neither is asked for.
    println!(
        "{SEEDS} seeds: the latest versions add up to {published}; with room again the session \
         that was open took the next commit {went_on} times, and {reopened} times the session \
         after it did, because it had ended or its caller had closed it; {cuts} power cuts \
         were looked at beside the two of each seed"
    );
}
