//! One state that a crash left: recovered, and held to the model.

use std::{collections::BTreeSet, fmt, path::Path, sync::Arc};

use lignin_format::PageId;
use lignin_io::{
    sim::{FaultOdds, SimVfs},
    Vfs,
};
use lignin_store::recovery::RecoveryReport;

use super::{
    model::{difference, Expect, History, Landing, Pages},
    store::{OpenError, Session, Tuning},
};

/// The database directory of every store of the campaign.
pub const DB: &str = "/db";

/// The database directory of every store of the campaign.
pub fn db_dir() -> &'static Path {
    Path::new(DB)
}

/// What a violation is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The store did not open after a crash, with no fault injected.
    NotRecovered,
    /// The store opened and is not at one with itself: a page id without a
    /// page, a value that is not read, a count that is not the pointers.
    Inconsistent,
    /// The store recovered to a state that is no committed version, or to
    /// one before a commit that was reported durable.
    NotCommitted,
    /// A second recovery, or the session that went on and was closed, gave
    /// another state than the first.
    NotTheSame,
    /// A store that was not crashed says other than the model, or a call
    /// failed with no fault injected.
    Session,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotRecovered => "the store did not recover",
            Self::Inconsistent => "the recovered store is not at one with itself",
            Self::NotCommitted => "the recovered state is no committed version",
            Self::NotTheSame => "recovering again gave another state",
            Self::Session => "a session went wrong with no crash",
        })
    }
}

/// Something the campaign is there to find.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// What it is a violation of.
    pub kind: Kind,
    /// What was found.
    pub detail: String,
}

impl Violation {
    /// A violation of `kind`.
    pub fn new(kind: Kind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.detail)
    }
}

/// A state that recovered to where it may be.
#[derive(Debug)]
pub struct Recovered {
    /// Where the store is.
    pub landing: Landing,
    /// What the recovery did, unless the directory holds no store.
    pub report: Option<RecoveryReport>,
    /// What the pages say.
    pub pages: Pages,
}

/// The ids of every page that a store which `expect` allows can have in
/// use.
pub fn expected_ids(history: &History, expect: &Expect) -> BTreeSet<PageId> {
    let points = (expect.floor..=expect.latest).map(|at| &history.point(at).pages);
    let attempt = expect.attempt.iter().map(|attempt| &attempt.pages);
    points
        .chain(attempt)
        .flat_map(|pages| pages.keys().copied())
        .collect()
}

fn opened(state: &SimVfs, tuning: &Tuning, kind: Kind) -> Result<Option<Session>, Violation> {
    let vfs: Arc<dyn Vfs> = Arc::new(state.clone());
    match Session::open(vfs, db_dir(), tuning) {
        Ok(session) => Ok(Some(session)),
        Err(OpenError::NoStore) => Ok(None),
        Err(error) => Err(Violation::new(kind, error.to_string())),
    }
}

/// Recovers `state`, which a crash left, with no fault injected, and holds
/// what it lands on to `history`: one of the points that `expect` allows,
/// with every page as that point has it, in a store that is at one with
/// itself ([`Session::read`]).
pub fn check_state(
    state: &SimVfs,
    history: &History,
    expect: &Expect,
    tuning: &Tuning,
) -> Result<Recovered, Violation> {
    state.set_faults(FaultOdds::NONE);
    let Some(session) = opened(state, tuning, Kind::NotRecovered)? else {
        return match expect.created {
            true => Err(Violation::new(
                Kind::NotRecovered,
                "the directory has no manifest, and the store was made",
            )),
            false => Ok(Recovered {
                landing: Landing::NoStore,
                report: None,
                pages: Pages::new(),
            }),
        };
    };
    let ids = expected_ids(history, expect);
    let pages = session
        .read(&ids)
        .map_err(|error| Violation::new(Kind::Inconsistent, error))?;
    let landed = session.latest().get();
    let landing = history
        .find(expect, landed, &pages)
        .map_err(|mismatch| Violation::new(Kind::NotCommitted, mismatch.to_string()))?;
    Ok(Recovered {
        landing,
        report: Some(*session.report()),
        pages,
    })
}

/// Holds a state that recovered once, to version `landed` with `pages`, to
/// what the store promises of the recoveries after it
/// (`lignin_store::recovery`, "Twice"):
///
/// - a second recovery, with `again` as its tuning, lands on the same
///   version with the same pages: the first one, which was dropped as a
///   killed process is, changed nothing that the next one starts from
/// - the session that follows writes everything back, takes its table
///   checkpoint and ends, and the store then opens with nothing to
///   replay, to the same version and pages, read from the segments
pub fn check_again(
    state: &SimVfs,
    again: &Tuning,
    landed: u64,
    pages: &Pages,
) -> Result<(), Violation> {
    let ids: BTreeSet<PageId> = pages.keys().copied().collect();
    let same = |session: &Session, what: &str| -> Result<(), Violation> {
        let found = session
            .read(&ids)
            .map_err(|error| Violation::new(Kind::NotTheSame, format!("{what}: {error}")))?;
        let now = session.latest().get();
        if now != landed {
            let said = format!("{what} is at version {now}, and the first was at {landed}");
            return Err(Violation::new(Kind::NotTheSame, said));
        }
        if found != *pages {
            let said = format!("{what}: {}", difference(&found, pages));
            return Err(Violation::new(Kind::NotTheSame, said));
        }
        Ok(())
    };
    let gone = || Violation::new(Kind::NotTheSame, "the manifest is gone");

    let second = opened(state, again, Kind::NotTheSame)?.ok_or_else(gone)?;
    same(&second, "the second recovery")?;
    let ended = second.finish().map_err(|error| {
        Violation::new(
            Kind::NotTheSame,
            format!("the session did not end: {error}"),
        )
    })?;
    if ended != Some(landed) {
        let said = format!("the last table checkpoint covers {ended:?}, not version {landed}");
        return Err(Violation::new(Kind::NotTheSame, said));
    }
    drop(second);

    let third = opened(state, again, Kind::NotTheSame)?.ok_or_else(gone)?;
    same(&third, "the store that was closed")?;
    let report = third.report();
    if report.commits != 0 || third.log_held() != 0 {
        let said = format!(
            "the store that was closed replayed {} commits and holds {} bytes of log",
            report.commits,
            third.log_held()
        );
        return Err(Violation::new(Kind::NotTheSame, said));
    }
    Ok(())
}
