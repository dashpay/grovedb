//! One state that a crash left: recovered, and held to the model.

use std::{
    any::Any,
    collections::BTreeSet,
    fmt,
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    sync::Arc,
};

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
    /// one before a commit that was durable.
    NotCommitted,
    /// A second recovery, a recovery with faults injected, or the session
    /// that went on and was closed, gave another state than the first.
    NotTheSame,
    /// A store that was not crashed says other than the model, or a call
    /// failed with no fault injected.
    Session,
    /// The store panicked, or the campaign did.
    Panicked,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotRecovered => "the store did not recover",
            Self::Inconsistent => "the recovered store is not at one with itself",
            Self::NotCommitted => "the recovered state is no committed version",
            Self::NotTheSame => "recovering again gave another state",
            Self::Session => "a session went wrong with no crash",
            Self::Panicked => "a panic",
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

/// What a panic said.
pub fn panic_text(panic: &(dyn Any + Send)) -> &str {
    let text = panic.downcast_ref::<&str>().copied();
    text.or(panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a panic that is no text")
}

/// Does `work`, and makes a violation of a panic in it: a store that
/// panics over what a crash left has not recovered, and the campaign says
/// where.
pub fn no_panic<T>(
    what: &str,
    work: impl FnOnce() -> Result<T, Violation>,
) -> Result<T, Violation> {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|panic| {
        let said = format!("{what} panicked: {}", panic_text(&*panic));
        Err(Violation::new(Kind::Panicked, said))
    })
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
/// itself ([`Session::read`]) and that the engine's physical invariant
/// checker finds sound ([`Session::verify`]).
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
    session
        .verify()
        .map_err(|error| Violation::new(Kind::Inconsistent, error))?;
    tidy(&session, Kind::Inconsistent)?;
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

/// A store that a recovery with no fault returned has no file among those
/// of its log, its segments, its value log and its table checkpoints that
/// the manifest in force does not name ([`Session::strays`]): whatever an
/// earlier session dropped and could not remove, or made and never named,
/// the recovery has removed.
fn tidy(session: &Session, kind: Kind) -> Result<(), Violation> {
    let strays = session
        .strays()
        .map_err(|error| Violation::new(kind, error))?;
    match strays.first() {
        Some(stray) => {
            let said = format!(
                "{} is in the store's directory after a recovery, and the manifest in force \
                 does not name it ({} such files)",
                stray.display(),
                strays.len()
            );
            Err(Violation::new(kind, said))
        }
        None => Ok(()),
    }
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
///   replay, to the same version and pages, read from the segments, and
///   with no file that its manifest does not name
pub fn check_again(
    state: &SimVfs,
    again: &Tuning,
    landed: u64,
    pages: &Pages,
) -> Result<(), Violation> {
    let gone = || Violation::new(Kind::NotTheSame, "the manifest is gone");

    let second = opened(state, again, Kind::NotTheSame)?.ok_or_else(gone)?;
    same(&second, landed, pages, "the second recovery")?;
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
    same(&third, landed, pages, "the store that was closed")?;
    tidy(&third, Kind::NotTheSame)?;
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

/// The store of `session` is at the version `landed` with `pages`, as the
/// first recovery of its state found it, and sound to the checker.
fn same(session: &Session, landed: u64, pages: &Pages, what: &str) -> Result<(), Violation> {
    let not_same = |said: String| Violation::new(Kind::NotTheSame, format!("{what}: {said}"));
    let ids: BTreeSet<PageId> = pages.keys().copied().collect();
    let found = session.read(&ids).map_err(not_same)?;
    let now = session.latest().get();
    if now != landed {
        return Err(not_same(format!(
            "it is at version {now}, and the first was at {landed}"
        )));
    }
    if found != *pages {
        return Err(not_same(difference(&found, pages)));
    }
    session.verify().map_err(not_same)
}

/// What a recovery with faults injected came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Faulted {
    /// It returned a store, the one a recovery with no fault returns.
    Returned,
    /// A fault failed it, and the recovery after it returned that store.
    Failed,
    /// The directory holds no store, and it said so.
    NoStore,
}

/// Recovers `state` with `odds` of faults injected: a state that a crash
/// left as a recovery with no fault found it on a copy of its own, at the
/// version `landed` with `pages`, or with no store if `landed` is `None`.
///
/// A recovery is a function of the files it finds, so one that a fault
/// does not fail returns that same store. One that a fault fails says that
/// the file layer failed and not that the store is damaged, and the
/// recovery after it, with no fault, returns that same store again
/// (`lignin_store::recovery`, "Twice"). One that fails with no fault
/// injected has no excuse.
pub fn check_with_faults(
    state: &SimVfs,
    odds: FaultOdds,
    tuning: &Tuning,
    landed: Option<u64>,
    pages: &Pages,
) -> Result<Faulted, Violation> {
    let before = state.stats().faults;
    state.set_faults(odds);
    let vfs: Arc<dyn Vfs> = Arc::new(state.clone());
    let opened_with_faults = Session::open(vfs, db_dir(), tuning);
    state.set_faults(FaultOdds::NONE);
    let injected = state.stats().faults > before;
    let other = |said: &str| Violation::new(Kind::NotTheSame, said);
    match (opened_with_faults, landed) {
        (Ok(session), Some(landed)) => {
            same(&session, landed, pages, "a recovery with faults injected")?;
            Ok(Faulted::Returned)
        }
        (Ok(_), None) => Err(other(
            "a recovery with faults injected returned a store, and the directory held none",
        )),
        (Err(OpenError::NoStore), None) => Ok(Faulted::NoStore),
        (Err(OpenError::NoStore), Some(_)) => Err(other(
            "a recovery with faults injected found no store, and the directory holds one",
        )),
        (Err(OpenError::Failed { damage: false, .. }), _) if injected => {
            let what = "the recovery after one that a fault failed";
            match (opened(state, tuning, Kind::NotTheSame)?, landed) {
                (Some(session), Some(landed)) => same(&session, landed, pages, what)?,
                (None, None) => {}
                (Some(_), None) => return Err(other("it returned a store where there was none")),
                (None, Some(_)) => return Err(other("it found no store where there was one")),
            }
            Ok(Faulted::Failed)
        }
        (Err(error), _) => {
            let said = match injected {
                true => format!("a recovery that only faults were injected into: {error}"),
                false => format!("a recovery of a state that had recovered: {error}"),
            };
            Err(Violation::new(Kind::NotRecovered, said))
        }
    }
}

#[cfg(test)]
mod tests {
    use lignin_io::{
        sim::{Fault, FaultRule, SimConfig},
        Create, OpenMode,
    };
    use lignin_store::segment::ManifestStore;

    use super::*;
    use crate::crash::{
        model::{Change, Value},
        store::Committed,
    };

    /// A file system with an empty directory for a store.
    fn empty() -> SimVfs {
        let fs = SimVfs::new(SimConfig::new(5));
        fs.create_dir(db_dir()).expect("a directory");
        fs.sync_dir(Path::new("/")).expect("synced");
        fs
    }

    /// What a killed session leaves of a store with one commit: the files,
    /// the version and the pages.
    fn killed() -> (SimVfs, u64, Pages) {
        let fs = empty();
        let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
        Session::create(vfs.clone(), db_dir()).expect("a store");
        let session = Session::open(vfs, db_dir(), &Tuning::small()).expect("opened");
        let entries = vec![(b"k".to_vec(), Value::Inline(vec![1]))];
        let committed = session.commit(&[Change::NewFlat { entries }], false, false, &mut |_| {});
        assert!(matches!(committed, Committed::Yes { .. }), "{committed:?}");
        let pages = session.read(&BTreeSet::new()).expect("read");
        let landed = session.latest().get();
        (fs, landed, pages)
    }

    /// The files of `fs` once more, as a crash that loses nothing leaves
    /// them.
    fn copy(fs: &SimVfs) -> SimVfs {
        let image = fs.crash_image();
        image.recover(&image.keep_all()).expect("fits")
    }

    fn said<T: fmt::Debug>(checked: Result<T, Violation>) -> (Kind, String) {
        let violation = checked.expect_err("a violation");
        (violation.kind, violation.detail)
    }

    #[test]
    fn a_file_that_the_manifest_does_not_name_is_told() {
        let (fs, _, _) = killed();
        let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
        // A file that an earlier session left with no name in the
        // manifest: the recovery removes it.
        let left = db_dir().join("seg/00000077.seg");
        let create = OpenMode::Write(Create::New);
        drop(vfs.open(&left, create).expect("made"));
        let session = Session::open(vfs.clone(), db_dir(), &Tuning::small()).expect("opened");
        assert_eq!(session.strays(), Ok(Vec::new()));
        assert_eq!(tidy(&session, Kind::Inconsistent), Ok(()));
        assert!(vfs.open(&left, OpenMode::Read).is_err());

        // One that is there once the recovery has returned, of any kind
        // and under any name, is a violation.
        let strays = ["wal/00000091.log", "val/notes.txt", "table/00000092.pt"];
        for name in strays {
            drop(vfs.open(&db_dir().join(name), create).expect("made"));
        }
        let found = session.strays().expect("listed");
        let expected: Vec<_> = strays.iter().map(|name| db_dir().join(name)).collect();
        assert_eq!(found, expected);
        let (kind, detail) = said(tidy(&session, Kind::Inconsistent));
        assert_eq!(kind, Kind::Inconsistent);
        assert!(
            detail.starts_with("/db/wal/00000091.log is in the store's directory after a recovery")
                && detail.ends_with("(3 such files)"),
            "{detail}"
        );
    }

    #[test]
    fn a_state_that_recovers_again_to_another_state_is_told() {
        let (fs, landed, pages) = killed();
        let tuning = Tuning::small();
        assert_eq!(check_again(&copy(&fs), &tuning, landed, &pages), Ok(()));

        let other = check_again(&copy(&fs), &tuning, landed + 1, &pages);
        let violation = other.clone().expect_err("another version");
        assert!(
            violation
                .to_string()
                .starts_with("recovering again gave another state: the second recovery: "),
            "{violation}"
        );
        let (kind, detail) = said(other);
        assert_eq!(kind, Kind::NotTheSame);
        assert!(
            detail.ends_with(&format!(
                "it is at version {landed}, and the first was at {}",
                landed + 1
            )),
            "{detail}"
        );
        let (kind, detail) = said(check_again(&copy(&fs), &tuning, landed, &Pages::new()));
        assert_eq!(kind, Kind::NotTheSame);
        assert!(detail.contains("is in use and should not be"), "{detail}");
        let (kind, detail) = said(check_again(&empty(), &tuning, landed, &pages));
        assert_eq!(
            (kind, detail.as_str()),
            (Kind::NotTheSame, "the manifest is gone")
        );
    }

    #[test]
    fn a_recovery_with_faults_returns_the_store_that_the_one_without_returned() {
        let (fs, landed, pages) = killed();
        let tuning = Tuning::small();
        let check = |state: &SimVfs, landed: Option<u64>| {
            check_with_faults(state, FaultOdds::NONE, &tuning, landed, &pages)
        };
        // No fault is drawn: the recovery is the one that was made before.
        assert_eq!(check(&copy(&fs), Some(landed)), Ok(Faulted::Returned));
        assert_eq!(check(&empty(), None), Ok(Faulted::NoStore));
        let (kind, detail) = said(check(&copy(&fs), None));
        assert_eq!(kind, Kind::NotTheSame);
        assert!(
            detail.contains("returned a store, and the directory held none"),
            "{detail}"
        );
        let (kind, detail) = said(check(&empty(), Some(landed)));
        assert_eq!(kind, Kind::NotTheSame);
        assert!(
            detail.contains("found no store, and the directory holds one"),
            "{detail}"
        );
        let (kind, detail) = said(check(&copy(&fs), Some(landed + 1)));
        assert_eq!(kind, Kind::NotTheSame);
        assert!(
            detail.starts_with("a recovery with faults injected: it is at version"),
            "{detail}"
        );

        // A fault fails the recovery, which takes the directory's lock
        // first: the recovery after it is held to the same.
        let failing = |state: SimVfs| {
            state.add_rule(FaultRule::new(Fault::Eio));
            state
        };
        assert_eq!(
            check(&failing(copy(&fs)), Some(landed)),
            Ok(Faulted::Failed)
        );
        assert_eq!(check(&failing(empty()), None), Ok(Faulted::Failed));
        let (kind, detail) = said(check(&failing(copy(&fs)), None));
        assert_eq!(kind, Kind::NotTheSame);
        assert_eq!(detail, "it returned a store where there was none");
        let (kind, detail) = said(check(&failing(empty()), Some(landed)));
        assert_eq!(kind, Kind::NotTheSame);
        assert_eq!(detail, "it found no store where there was one");
        let (kind, detail) = said(check(&failing(copy(&fs)), Some(landed + 1)));
        assert_eq!(kind, Kind::NotTheSame);
        assert!(
            detail.starts_with("the recovery after one that a fault failed: it is at version"),
            "{detail}"
        );

        // A recovery that fails with no fault injected has no excuse, and
        // neither has the one after a fault that says the store is
        // damaged.
        let damaged = || {
            let state = copy(&fs);
            let manifest = db_dir().join(ManifestStore::FILE_NAME);
            let file = state
                .open(&manifest, OpenMode::Write(Create::IfMissing))
                .expect("opened");
            file.write_at(b"not a manifest at all", 0).expect("written");
            state
        };
        let (kind, detail) = said(check(&damaged(), Some(landed)));
        assert_eq!(kind, Kind::NotRecovered);
        assert!(
            detail.starts_with("a recovery of a state that had recovered: the store is damaged"),
            "{detail}"
        );
        let (kind, detail) = said(check(&failing(damaged()), Some(landed)));
        assert_eq!(kind, Kind::NotTheSame);
        assert!(detail.starts_with("the store is damaged"), "{detail}");
        // The same state, held to the model, did not recover.
        let history = History::new();
        let (kind, detail) = said(check_state(
            &damaged(),
            &history,
            &Expect::exactly(0),
            &tuning,
        ));
        assert_eq!(kind, Kind::NotRecovered);
        assert!(detail.starts_with("the store is damaged"), "{detail}");
        let (kind, detail) = said(check_state(
            &empty(),
            &history,
            &Expect::exactly(0),
            &tuning,
        ));
        assert_eq!(kind, Kind::NotRecovered);
        assert_eq!(
            detail,
            "the directory has no manifest, and the store was made"
        );
    }

    #[test]
    fn a_panic_is_a_violation_that_says_what_the_panic_said() {
        let fine = no_panic("a recovery", || Ok(3));
        assert_eq!(fine, Ok(3));
        let refused = no_panic("a recovery", || {
            Err::<u8, _>(Violation::new(Kind::NotRecovered, "no"))
        });
        assert_eq!(refused, Err(Violation::new(Kind::NotRecovered, "no")));

        for (said, work) in [
            ("out of range", (|| panic!("out of range")) as fn() -> u8),
            ("index 9", || panic!("index {}", 9)),
            ("a panic that is no text", || std::panic::panic_any(9u8)),
        ] {
            let violation = no_panic("a recovery", || Ok(work())).expect_err("a panic");
            assert_eq!(violation.kind, Kind::Panicked);
            assert_eq!(violation.detail, format!("a recovery panicked: {said}"));
        }
        assert_eq!(
            Violation::new(Kind::Panicked, "x").to_string(),
            "a panic: x"
        );
    }
}
