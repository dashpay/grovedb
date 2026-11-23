//! The candidate's store, read by its own checker when a case has ended
//! (VER-07-F1, D-343).
//!
//! Where the candidate is LigninDB's engine, the physical checker of
//! LigninDB reads its store once the last operation of a case was
//! compared, beside the engine's threads (`lignindb::Db::verify`): every
//! page, with the stored hashes of the nodes of every Merk made again by
//! Merk's rules for a check on the grove version of the case. The engine
//! gave the database the tree types when it opened it, so the checker
//! knows which subtree is a Merk. A finding is a divergence of the case
//! ([`Part::Check`](super::pair::Part::Check)), in any build.
//!
//! On grove versions 1 to 3 the delete of a tree that holds elements
//! leaves a value hash that is not the one the rules make (GroveDB #686),
//! on either engine. So the check differs by the grove version of the
//! case:
//!
//! - On versions 4 and 5 the rules name no such hash, and one is a
//!   finding as any other hash that differs. The close of a debug build
//!   is told the same ([`opened`]).
//! - On versions 1 to 3 the rules name it and the checker counts it
//!   apart. Each one it lists must be at a place the reference's own
//!   check (`verify_grovedb`) reports, and there must be no more of them
//!   than the reference reports places. That check also reports hashes
//!   the checker rightly does not name, so the places the checker names
//!   are a subset of the reference's. The checker lists the first
//!   [`OtherHashes::SAMPLE`] of them: those beyond are compared by their
//!   number alone, and counted as such ([`Closes::beyond_the_sample`]).
//!
//! The places are compared where the case ran to its last operation with
//! both engines answering alike and no stored difference of a named open
//! difference passed over. A case that ended at a named open difference,
//! or passed over records that one engine keeps, left the engines in
//! different states: there the store is checked for findings, and the
//! hashes the rules name are not compared.
//!
//! Both checks read committed state: what a case left in a transaction
//! that it did not commit is not read.

use std::{any::Any, collections::BTreeSet};

use grovedb::{Engine, GroveDb, LigninEngine};
use grovedb_merk::store::lignin::hash::MerkHashRules;
use grovedb_storage::prefix::build_prefix;
use grovedb_version::version::GroveVersion;
#[cfg(doc)]
use lignindb::verify::OtherHashes;

use crate::golden::trace::one_line;

/// Where an element is: the prefix of the Merk it is in, and its key.
type Place = ([u8; 32], Vec<u8>);

/// The last grove version on which a delete leaves that value hash.
const LAST_THAT_LEAVES_HASHES: u32 = 3;

/// How many findings, or places of the reference, a divergence lists.
const LISTED: usize = 8;

/// What the checks at the ends of cases came to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Closes {
    /// The cases at whose end the candidate's store was checked and found
    /// sound.
    pub checked: u64,
    /// The value hashes a delete left that the checker listed, each at a
    /// place the reference's own check reports. Grove versions 1 to 3.
    pub left_hashes: u64,
    /// Those the checker counted beyond the ones it lists, which were
    /// compared by their number alone.
    pub beyond_the_sample: u64,
}

impl Closes {
    /// Adds what other checks came to.
    pub fn add(&mut self, other: &Closes) {
        self.checked += other.checked;
        self.left_hashes += other.left_hashes;
        self.beyond_the_sample += other.beyond_the_sample;
    }

    /// For a reader, where a store was checked.
    pub fn line(&self) -> Option<String> {
        (self.checked > 0).then(|| {
            format!(
                "{} stores of the candidate checked sound at the end of their case; {} value \
                 hashes left by a delete, each at a place the reference reports, and {} more by \
                 their number alone",
                self.checked, self.left_hashes, self.beyond_the_sample
            )
        })
    }
}

/// What a check found wanting, as the lines of a divergence.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Wanting {
    pub(super) reference: Vec<String>,
    pub(super) candidate: Vec<String>,
}

/// The database as one on LigninDB's engine, where it is one.
fn on_lignindb<E: Engine>(db: &GroveDb<E>) -> Option<&GroveDb<LigninEngine>> {
    (db as &dyn Any).downcast_ref()
}

fn leaves_hashes(grove_version: &GroveVersion) -> bool {
    grove_version.protocol_version <= LAST_THAT_LEAVES_HASHES
}

/// Told of a candidate's database right after it was opened, for a case
/// on `grove_version`: on a version that leaves no value hash, the close
/// of a debug build fails on one.
pub(super) fn opened<E: Engine>(db: &GroveDb<E>, grove_version: &'static GroveVersion) {
    if let Some(db) = on_lignindb(db)
        && !leaves_hashes(grove_version)
    {
        db.engine().check_closes_with_no_left_hashes(grove_version);
    }
}

fn place(path: &[Vec<u8>]) -> Option<Place> {
    let (key, above) = path.split_last()?;
    let above: Vec<&[u8]> = above.iter().map(Vec::as_slice).collect();
    Some((build_prefix(above.as_slice().into()).unwrap(), key.clone()))
}

fn shown(subtree: Option<&[u8; 32]>, key: &[u8]) -> String {
    let merk = subtree.map_or("a Merk the directory does not name".to_string(), |prefix| {
        format!("the Merk {}", hex::encode(prefix))
    });
    format!("the key {} of {merk}", hex::encode(key))
}

/// The hashes the checker named, `count` of them of which it listed
/// `named`, held against the places the reference reports.
fn compared(
    named: &[(Option<[u8; 32]>, Vec<u8>)],
    count: u64,
    reported: &BTreeSet<Place>,
) -> Result<Closes, Wanting> {
    let reports = |(subtree, key): &&(Option<[u8; 32]>, Vec<u8>)| {
        subtree.is_some_and(|prefix| reported.contains(&(prefix, key.clone())))
    };
    let mut candidate: Vec<String> = (named.iter().filter(|hash| !reports(hash)))
        .map(|(subtree, key)| {
            let at = shown(subtree.as_ref(), key);
            format!("names a value hash a delete left at {at}")
        })
        .collect();
    let listed = named.len() as u64;
    if count > reported.len() as u64 || count < listed {
        candidate.push(format!("counts {count} such value hashes, {listed} listed"));
    }
    if candidate.is_empty() {
        return Ok(Closes {
            checked: 1,
            left_hashes: listed,
            beyond_the_sample: count - listed,
        });
    }
    let mut reference = vec![format!("reports {} places", reported.len())];
    let places = reported.iter().take(LISTED);
    reference.extend(places.map(|(prefix, key)| format!("reports {}", shown(Some(prefix), key))));
    Err(Wanting {
        reference,
        candidate,
    })
}

/// Checks the candidate's store at the end of a case on `grove_version`.
/// `alike` says that the engines hold the same grove: the hashes the
/// rules name are then held against the reference. A candidate that is
/// not LigninDB's engine is not checked.
pub(super) fn check<A: Engine, B: Engine>(
    reference: &GroveDb<A>,
    candidate: &GroveDb<B>,
    grove_version: &'static GroveVersion,
    alike: bool,
) -> Result<Closes, Wanting> {
    let Some(candidate) = on_lignindb(candidate) else {
        return Ok(Closes::default());
    };
    let naming = leaves_hashes(grove_version);
    let mut rules = MerkHashRules::for_check(grove_version);
    if naming {
        rules = rules.naming_left_hashes();
    }
    let report = candidate.engine().database().verify(Some(&mut rules));
    if !report.is_sound() {
        let all = report.findings.len() as u64 + report.more_findings;
        let mut lines = vec![format!("{all} findings of the physical checker")];
        let findings = report.findings.iter().take(LISTED);
        lines.extend(findings.map(|finding| one_line(&format!("finding: {finding:?}"))));
        return Err(Wanting {
            reference: vec!["no finding is expected of the candidate's store".to_string()],
            candidate: lines,
        });
    }
    let sound = Closes {
        checked: 1,
        ..Closes::default()
    };
    let hashes = &report.other_hashes;
    if !alike || hashes.is_empty() {
        return Ok(sound);
    }
    let reported: BTreeSet<Place> = match reference.verify_grovedb(None, true, false, grove_version)
    {
        Ok(issues) => issues.keys().filter_map(|path| place(path)).collect(),
        Err(error) => {
            let error = one_line(&format!("{error:?}"));
            return Err(Wanting {
                reference: vec![format!("its own check failed: {error}")],
                candidate: vec![format!(
                    "counts {} value hashes a delete left",
                    hashes.count
                )],
            });
        }
    };
    let named: Vec<_> = (hashes.first.iter())
        .map(|hash| (hash.subtree, hash.key.clone()))
        .collect();
    compared(&named, hashes.count, &reported)
}

#[cfg(test)]
mod tests {
    use grovedb::{operations::delete::DeleteOptions, Element};
    use grovedb_version::version::GROVE_VERSIONS;

    use super::*;
    use crate::{
        diff::{
            case::Case,
            engines::{Candidate, Reference, StandIn},
            pair::{run, testing::scratch, Part, Run, Side, Stored},
        },
        golden::record::on_a_deep_stack,
        workload::{Op, StreamEngine, Write},
    };

    const TOP: &[u8] = b"top";
    const PARENT: &[u8] = b"parent";
    const CHILD: &[u8] = b"child";

    /// The calls of GroveDB #686 under a tree `top`: a provable count
    /// tree `parent` with an item, a tree `child` in it with an item, and
    /// the delete of `child` with what it holds, committed.
    fn history() -> Vec<Op> {
        let insert = |path: &[&[u8]], key: &[u8], element: Element| {
            Op::Write(Write::Insert {
                path: path.iter().map(|segment| segment.to_vec()).collect(),
                key: key.to_vec(),
                element,
            })
        };
        vec![
            insert(&[], TOP, Element::empty_tree()),
            insert(&[TOP], PARENT, Element::empty_provable_count_tree()),
            insert(&[TOP, PARENT], b"sibling", Element::new_item(b"s".to_vec())),
            insert(&[TOP, PARENT], CHILD, Element::empty_tree()),
            insert(
                &[TOP, PARENT, CHILD],
                b"leaf",
                Element::new_item(b"value".to_vec()),
            ),
            Op::Write(Write::Delete {
                path: vec![TOP.to_vec(), PARENT.to_vec()],
                key: CHILD.to_vec(),
            }),
            Op::Commit,
        ]
    }

    /// The same calls made of a database, on `grove_version`.
    fn made<E: Engine>(db: &GroveDb<E>, grove_version: &GroveVersion) {
        let put = |at: &[&[u8]], key: &[u8], element: Element| {
            let inserted = db.insert(at, key, element, None, None, grove_version);
            inserted.unwrap().expect("the element is taken");
        };
        put(&[], TOP, Element::empty_tree());
        put(&[TOP], PARENT, Element::empty_provable_count_tree());
        put(&[TOP, PARENT], b"sibling", Element::new_item(b"s".to_vec()));
        put(&[TOP, PARENT], CHILD, Element::empty_tree());
        put(
            &[TOP, PARENT, CHILD],
            b"leaf",
            Element::new_item(b"value".to_vec()),
        );
        let with_what_it_holds = DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: false,
            ..Default::default()
        };
        let at: &[&[u8]] = &[TOP, PARENT];
        let deleted = db.delete(at, CHILD, Some(with_what_it_holds), None, grove_version);
        deleted.unwrap().expect("the tree is deleted");
    }

    fn compared_on<A: StreamEngine, B: StreamEngine>(
        case: &Case,
        reference: &Side<A>,
        candidate: &Side<B>,
    ) -> Run {
        let scratch = scratch();
        let ran = on_a_deep_stack(|| {
            let (scratch, stored) = (scratch.path(), Stored::AtTheEnd);
            run(case, reference, candidate, scratch, stored, &|| false)
        });
        ran.expect("the case runs")
    }

    /// A case that leaves a value hash on grove versions 1 to 3 runs to
    /// its end on every grove version: the store of LigninDB is checked,
    /// and on those versions the hash is counted, at the place the
    /// reference reports. A candidate that is not LigninDB is not checked.
    #[test]
    fn a_case_ends_with_the_check_of_the_candidates_store() {
        for grove_version in GROVE_VERSIONS.iter() {
            let number = grove_version.protocol_version;
            let case = Case::of(0, grove_version, history());
            let reference = Side::<Reference>::of("reference");
            let ran = compared_on(&case, &reference, &Side::<Candidate>::of("candidate"));
            assert_eq!(ran.divergence, None, "grove version {number}");
            assert_eq!(ran.tally.steps, 7, "grove version {number}");
            assert_eq!(ran.tally.failed, 0, "grove version {number}");
            let expected = Closes {
                checked: 1,
                left_hashes: u64::from(number <= 3),
                beyond_the_sample: 0,
            };
            assert_eq!(ran.tally.closes, expected, "grove version {number}");

            let ran = compared_on(&case, &reference, &Side::<StandIn>::of("stand-in"));
            assert_eq!(ran.divergence, None, "grove version {number}");
            assert_eq!(ran.tally.closes, Closes::default());
        }
    }

    /// The check itself, on two databases that hold the history: the hash
    /// is at the key of `parent` in the Merk of `top`; held against a
    /// reference that reports no place it is wanting; it is not compared
    /// where the case did not end alike; and by the rules of a version
    /// that leaves no such hash it is a finding.
    #[test]
    fn a_left_hash_is_held_against_the_places_the_reference_reports() {
        for grove_version in GROVE_VERSIONS.iter() {
            let number = grove_version.protocol_version;
            let dir = scratch();
            let open_reference = |name: &str| {
                let db = GroveDb::<Reference>::open_with_engine(dir.path().join(name));
                db.expect("a database")
            };
            let reference = open_reference("reference");
            let candidate = GroveDb::<Candidate>::open_with_engine(dir.path().join("candidate"));
            let candidate = candidate.expect("a database");
            made(&reference, grove_version);
            made(&candidate, grove_version);
            let left = u64::from(number <= 3);
            let sound = Closes {
                checked: 1,
                ..Closes::default()
            };
            let with_the_hash = Closes {
                left_hashes: left,
                ..sound
            };
            let checked = check(&reference, &candidate, grove_version, true);
            assert_eq!(checked, Ok(with_the_hash), "grove version {number}");
            let checked = check(&reference, &candidate, grove_version, false);
            assert_eq!(checked, Ok(sound), "grove version {number}");
            let unchecked = check(&candidate, &reference, grove_version, true);
            assert_eq!(unchecked, Ok(Closes::default()), "grove version {number}");
            if left == 0 {
                continue;
            }

            let empty = open_reference("empty");
            let wanting = check(&empty, &candidate, grove_version, true);
            let wanting = wanting.expect_err("a hash where the reference reports none");
            let above: &[&[u8]] = &[TOP];
            let merk = hex::encode(build_prefix(above.into()).unwrap());
            let at = format!("the key {} of the Merk {merk}", hex::encode(PARENT));
            let names = format!("names a value hash a delete left at {at}");
            let counts = "counts 1 such value hashes, 1 listed".to_string();
            assert_eq!(wanting.candidate, [names, counts], "grove version {number}");
            assert_eq!(wanting.reference, ["reports 0 places"]);

            let later = GROVE_VERSIONS.last().expect("a grove version");
            let wanting = check(&reference, &candidate, later, true);
            let wanting = wanting.expect_err("a finding by the rules of the last version");
            let lines = wanting.candidate;
            assert_eq!(lines[0], "1 findings of the physical checker", "{lines:?}");
            assert!(lines[1].contains("Hash(Value)"), "{lines:?}");
        }
    }

    /// Both engines open with the history as grove version 3 wrote it,
    /// and the case is of the last grove version: the engines answer its
    /// one operation alike, and the hash the delete left is a divergence
    /// of the close check, at the last step, in a debug build as in a
    /// release build.
    #[test]
    #[expect(
        clippy::result_large_err,
        reason = "a database is opened with GroveDB's own error, whatever its size"
    )]
    fn a_left_hash_on_a_later_grove_version_is_a_divergence_of_the_close_check() {
        fn with_the_history<E: StreamEngine>(name: &str) -> Side<E> {
            Side::opened_by(name, |dir| {
                let db = GroveDb::<E>::open_with_engine(dir)?;
                made(&db, &GROVE_VERSIONS[2]);
                Ok(db)
            })
        }
        let later = GROVE_VERSIONS.last().expect("a grove version");
        let insert = Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"after".to_vec(),
            element: Element::new_item(vec![1; 8]),
        });
        let case = Case::of(0, later, vec![insert]);
        let reference = with_the_history::<Reference>("reference");
        let candidate = with_the_history::<Candidate>("candidate");
        let ran = compared_on(&case, &reference, &candidate);
        let divergence = ran.divergence.expect("a divergence");
        assert_eq!(
            (divergence.step, &divergence.parts[..]),
            (0, &[Part::Check][..])
        );
        let lines = &divergence.candidate;
        assert_eq!(lines[0], "1 findings of the physical checker", "{lines:?}");
        assert!(lines[1].contains("Hash(Value)"), "{lines:?}");
        assert_eq!(ran.tally.closes, Closes::default());
        assert_eq!(ran.tally.steps, 1);
    }

    fn at(n: u8) -> ([u8; 32], Vec<u8>) {
        ([n; 32], vec![n])
    }

    fn named(places: &[u8]) -> Vec<(Option<[u8; 32]>, Vec<u8>)> {
        let named = places.iter().map(|n| at(*n));
        named.map(|(prefix, key)| (Some(prefix), key)).collect()
    }

    /// The hashes the checker lists are each at a place the reference
    /// reports, and it counts no more of them than the reference reports
    /// places and no fewer than it lists.
    #[test]
    fn the_named_hashes_are_a_subset_of_the_reported_places() {
        let reported: BTreeSet<Place> = (1..=5).map(at).collect();
        let closes = |left_hashes, beyond_the_sample| Closes {
            checked: 1,
            left_hashes,
            beyond_the_sample,
        };
        assert_eq!(compared(&named(&[2, 4]), 2, &reported), Ok(closes(2, 0)));
        assert_eq!(compared(&named(&[]), 0, &reported), Ok(closes(0, 0)));
        // More than the checker lists: by their number.
        assert_eq!(compared(&named(&[1, 2]), 5, &reported), Ok(closes(2, 3)));
        let wanting = compared(&named(&[1, 2]), 6, &reported).expect_err("more than reported");
        assert_eq!(wanting.candidate, ["counts 6 such value hashes, 2 listed"]);
        assert_eq!(wanting.reference.len(), 6);
        let wanting = compared(&named(&[1, 2]), 1, &reported).expect_err("fewer than listed");
        assert_eq!(wanting.candidate, ["counts 1 such value hashes, 2 listed"]);

        // A place the reference does not report, by its Merk or by its key.
        let wanting = compared(&named(&[2, 6]), 2, &reported).expect_err("a place not reported");
        let six = format!("the key 06 of the Merk {}", hex::encode([6; 32]));
        assert_eq!(
            wanting.candidate,
            [format!("names a value hash a delete left at {six}")]
        );
        let other_key = [(Some([2; 32]), vec![3])];
        assert!(compared(&other_key, 1, &reported).is_err());
        // A Merk the directory does not name is at no reported place.
        let unnamed = [(None, vec![2])];
        let wanting = compared(&unnamed, 1, &reported).expect_err("no Merk");
        let at = "the key 02 of a Merk the directory does not name";
        assert_eq!(
            wanting.candidate,
            [format!("names a value hash a delete left at {at}")]
        );
    }

    #[test]
    fn what_the_checks_came_to_adds_up_and_is_told_where_a_store_was_checked() {
        let mut closes = Closes {
            checked: 2,
            left_hashes: 3,
            beyond_the_sample: 4,
        };
        closes.add(&closes.clone());
        assert_eq!(
            closes.line().expect("a line"),
            "4 stores of the candidate checked sound at the end of their case; 6 value hashes \
             left by a delete, each at a place the reference reports, and 8 more by their number \
             alone"
        );
        assert_eq!(Closes::default().line(), None);
    }
}
