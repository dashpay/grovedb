//! `lignin verify DIR [--no-hashes] [--beside-background]`: the physical
//! invariant checker (`lignindb::verify`) on a database.
//!
//! The check reads every page of the latest version, walks every flat
//! keyspace and every Merk, reads every value out of line, and with
//! `--no-hashes` left out makes every stored hash again by Merk's rules
//! (`MerkHashRules::for_check` at the latest GroveVersion: a reference's
//! value hash, which no rule can make from the pages, is taken as stored).
//! It prints the report and exits with 0 for a sound store and 3 for one
//! in which a rule does not hold.
//!
//! **The value hash that a delete on grove versions 1 to 3 left.** On
//! those versions the delete of a tree that holds elements can leave the
//! tree it was in with a value hash that is not the one the rules make
//! (GroveDB #686; `grovedb_merk::store::lignin::hash` says which hash it
//! is). RocksDB holds that hash after such a delete, and
//! `GroveDb::verify_grovedb` reports its path on either engine. `verify`
//! does not call a store unsound for it: it checks with the rules that
//! name it (`MerkHashRules::naming_left_hashes`), says after the report
//! how many there are and in which cells, and the status is the one the
//! rest of the check gives.
//!
//! **Where it runs.** By default the database is opened and closed, and
//! the store is checked after the close
//! ([`Db::close_and_verify`](lignindb::Db::close_and_verify)): still, with
//! what the store keeps beside its pages (live page ids, counts of live
//! values, the page table against its segments) compared too. With
//! `--beside-background` the check runs while the session's write-back,
//! cleaner and reclaimer are running ([`Db::verify`](lignindb::Db::verify)),
//! as a node runs it on its own open database. It checks what a reader
//! sees: the comparisons of the still store are left out.
//! **Reclaiming waits while a check runs**: the check pins its version with
//! the reclaiming of dropped subtrees held back until it ends, so that it
//! does not read a version behind which a reclaimer worked, which a sound
//! store can look unsound at. Another check of the same handle waits for
//! it too; nothing else waits for it but what waits for any reader.
//!
//! **A database with the mark of an incomplete restore.** State sync that
//! commits in parts keeps an aux record at the root
//! ([`INCOMPLETE_RESTORE_KEY`]) from its first commit until its last. A
//! database that holds it is unsound by its own statement: its elements
//! commit to Merks that were not restored, so the value hashes of their
//! hosts do not hold, by design. Such a database is to be discarded, and
//! is never served. `verify` says so, makes no hash again (with them,
//! every host of a Merk not restored would be reported), checks the other
//! rules of its scope, and exits with 4 whatever it finds.

use std::io::Write;

use grovedb_merk::store::lignin::hash::MerkHashRules;
use grovedb_version::version::GroveVersion;
use lignin_tree::hash::HashRules;
use lignindb::{
    txn::{Column, Subtree},
    verify::Report,
    Db,
};

use crate::{args::Common, open::open, Outcome, ToolError};

/// The key of the aux record of the root subtree that marks a state-sync
/// restore which has not finished: GroveDB's
/// `replication::INCOMPLETE_RESTORE_AUX_KEY`.
pub const INCOMPLETE_RESTORE_KEY: &[u8] = b"grovedb_state_sync_restore_in_progress";

/// The subtree of the root Merk of a grove: the prefix of the empty path.
pub const ROOT: Subtree = Subtree::new([0; 32]);

/// Whether the latest version of `db` holds the mark of a restore that did
/// not finish.
pub fn incomplete_restore(db: &Db) -> Result<bool, ToolError> {
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    let Some(root) = operation.open(ROOT)? else {
        return Ok(false);
    };
    let mut value = Vec::new();
    Ok(root.get(Column::Aux, INCOMPLETE_RESTORE_KEY, &mut value)?)
}

/// What a check found, with the outcome it gives.
#[derive(Debug)]
struct Verdict {
    /// The checker's report.
    report: Report,
    /// Whether the database holds the mark of an incomplete restore, and
    /// so was checked with no hash made again.
    incomplete_restore: bool,
}

impl Verdict {
    /// The outcome: the mark first, whatever the report says.
    fn outcome(&self) -> Outcome {
        match (self.incomplete_restore, self.report.is_sound()) {
            (true, _) => Outcome::IncompleteRestore,
            (false, true) => Outcome::Done,
            (false, false) => Outcome::NotSound,
        }
    }
}

/// [`Db::verify`] beside the background work of `db`, which this command
/// opened and no writer shares: the mark is looked up at the latest
/// version, which is the one the check pins. Merk's rules check the
/// hashes where `hashes` says so and the database holds no mark of an
/// incomplete restore. Reclaiming waits while it runs.
fn beside_background(db: &Db, hashes: bool) -> Result<Verdict, ToolError> {
    let incomplete_restore = incomplete_restore(db)?;
    let mut merk = rules(hashes && !incomplete_restore);
    let report = db.verify(merk.as_mut().map(|rules| rules as &mut dyn HashRules));
    Ok(Verdict {
        report,
        incomplete_restore,
    })
}

fn rules(hashes: bool) -> Option<MerkHashRules<'static>> {
    hashes.then(|| MerkHashRules::for_check(GroveVersion::latest()).naming_left_hashes())
}

/// What `verify` says of the value hashes that a delete on grove versions
/// 1 to 3 left, if the check came to any.
fn left_hashes(report: &Report) -> Option<String> {
    let left = &report.other_hashes;
    let said = match left.count {
        0 => return None,
        1 => "1 value hash of a tree is the one grove versions 1 to 3 left after a delete below \
              it (GroveDB #686), not the one the rules make"
            .to_owned(),
        count => format!(
            "{count} value hashes of trees are the ones grove versions 1 to 3 left after a \
             delete below them (GroveDB #686), not the ones the rules make"
        ),
    };
    let cells: Vec<String> = (left.first.iter())
        .map(|hash| format!("cell {}", hash.cell))
        .collect();
    let more = match left.count - left.first.len() as u64 {
        0 => String::new(),
        more => format!(" and {more} more"),
    };
    Some(format!("{said}: {}{more}", cells.join(", ")))
}

/// Checks the database in `common.dir` and writes the report to `out`.
pub fn run(
    common: &Common,
    hashes: bool,
    beside: bool,
    out: &mut dyn Write,
) -> Result<Outcome, ToolError> {
    let db = open(common)?;
    let verdict = match beside {
        true => {
            let verdict = beside_background(&db, hashes)?;
            db.close()?;
            verdict
        }
        false => {
            let incomplete_restore = incomplete_restore(&db)?;
            let mut merk = rules(hashes && !incomplete_restore);
            let report =
                db.close_and_verify(merk.as_mut().map(|rules| rules as &mut dyn HashRules))?;
            Verdict {
                report,
                incomplete_restore,
            }
        }
    };
    if verdict.incomplete_restore {
        writeln!(
            out,
            "INCOMPLETE RESTORE: the root holds the mark of a state-sync restore that did not \
             finish. Its elements commit to Merks that were not restored, so the database is \
             unsound by its own statement: discard it. No hash was made again."
        )?;
    }
    let scope = match beside {
        true => "beside the background work",
        false => "the still store, after the close",
    };
    let hashed = match (hashes, verdict.incomplete_restore) {
        (true, false) => "every hash made again by Merk's rules",
        _ => "no hash made again",
    };
    writeln!(out, "checked {scope}, {hashed}")?;
    write!(out, "{}", verdict.report)?;
    if let Some(left) = left_hashes(&verdict.report) {
        write!(out, "\n{left}")?;
    }
    Ok(verdict.outcome())
}
