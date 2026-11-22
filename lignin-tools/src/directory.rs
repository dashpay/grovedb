//! `lignin directory DIR`: every entry of the subtree directory.
//!
//! The database is opened, and the directory of the latest version is
//! walked in the order of the prefixes
//! ([`Operation::entries`](lignindb::snapshot::Operation::entries)), one
//! line for each entry as it is read. An entry of a subtree below one that
//! was dropped, which the reclaimer has not taken yet, is listed as left
//! over: it is not in use, and no read opens it. A page of the directory
//! that is not read ends the listing with the error, after the lines of the
//! entries read before it, and never as if the directory ended there.

use std::io::Write;

use lignindb::snapshot::Listed;

use crate::{args::Common, hex, open::open, Outcome, ToolError};

/// The line of one entry.
pub(crate) fn line(listed: &Listed) -> String {
    let entry = &listed.entry;
    let mut line = format!(
        "{} type {}",
        hex(listed.subtree.as_bytes()),
        hex(entry.tree_type())
    );
    match entry.root() {
        Some(root) => line += &format!(" root {} key {}", root.address, hex(root.key)),
        None => line += " empty",
    }
    if let Some(host) = entry.host() {
        line += &format!(
            " host {} key {} position {} depth {}",
            hex(host.parent),
            hex(host.key),
            host.position,
            entry.depth()
        );
    }
    if !listed.live {
        line += " left over";
    }
    line
}

/// Writes every entry of the directory of the database in `common.dir`
/// to `out`.
pub fn run(common: &Common, out: &mut dyn Write) -> Result<Outcome, ToolError> {
    let db = open(common)?;
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    writeln!(out, "version {}", operation.at())?;
    let mut walk = operation.entries()?;
    let (mut entries, mut live, mut hostless, mut deepest) = (0u64, 0u64, 0u64, 0);
    while let Some(listed) = walk.next()? {
        writeln!(out, "{}", line(&listed))?;
        entries += 1;
        live += u64::from(listed.live);
        hostless += u64::from(listed.entry.host().is_none());
        deepest = deepest.max(listed.entry.depth());
    }
    writeln!(
        out,
        "{entries} entries: {live} in use, {} left over; {hostless} with no host; the deepest \
         below {deepest} others",
        entries - live
    )?;
    drop(walk);
    drop(operation);
    drop(snapshot);
    db.close()?;
    Ok(Outcome::Done)
}
