//! Running a trace: applying operations to a GroveDB and observing each one.
//!
//! [`Runner`] is the only code of the corpus that calls the engine. It
//! applies an operation through [`workload::apply`](crate::workload::apply()),
//! so recording, replaying and dumping drive the database exactly as every
//! other consumer of a workload does, and it reads back what the corpus
//! compares:
//!
//! - the `OperationCost` and the result of the operation, or its error;
//! - after everything but a read, the root hash, inside the open
//!   transaction, and after a commit the root hash of the committed state;
//! - for a query, the proof `GroveDb::prove_query` builds from committed
//!   state, the cost of building it, and what the verifier makes of it.
//!
//! Nothing here requires an operation to succeed or a proof to verify: what
//! the engine returns is what is recorded.

use std::fmt::Debug;

use grovedb::{GroveDb, Transaction};
use grovedb_costs::CostContext;
use grovedb_version::version::GroveVersion;

use super::{
    step::{Outcome, Proof, Proved, Row, Step, Value, Verification},
    Error,
};
use crate::workload::{apply, Op, Outcome as Applied, Query, Read};

/// The stack a thread needs to run a trace. GroveDB recurses once per level
/// of nesting when it queries and proves, about 5 MiB at depth 64 in an
/// unoptimized build.
pub const STACK_BYTES: usize = 64 << 20;

/// A step, with what a reader of a trace wants to know about it and no
/// comparison does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    /// What is compared.
    pub step: Step,
    /// The messages of the errors the step names.
    pub remarks: Vec<String>,
}

/// Applies the operations of a trace to one database, one transaction per
/// block.
pub struct Runner<'db> {
    db: &'db GroveDb,
    grove_version: &'db GroveVersion,
    transaction: Option<Transaction<'db>>,
    committed_root: [u8; 32],
}

impl<'db> Runner<'db> {
    /// A runner on `db`, which stays open for further traces afterwards.
    pub fn new(db: &'db GroveDb, grove_version: &'db GroveVersion) -> Result<Self, Error> {
        let committed_root = root_hash(db, None, grove_version)?;
        Ok(Self {
            db,
            grove_version,
            transaction: Some(db.start_transaction()),
            committed_root,
        })
    }

    /// Applies one operation and reads back everything the corpus compares.
    pub fn step(&mut self, op: &Op) -> Result<Observed, Error> {
        let mut remarks = Vec::new();
        let CostContext { value, mut cost } =
            apply(self.db, op, self.transaction.as_ref(), self.grove_version);
        let outcome = match value {
            Ok(Applied::EndOfBlock) => {
                let transaction = self.transaction.take();
                let committed = match transaction {
                    Some(transaction) => {
                        let committed = self.db.commit_transaction(transaction);
                        cost += committed.cost;
                        committed.value
                    }
                    None => Ok(()),
                };
                self.transaction = Some(self.db.start_transaction());
                self.committed_root = root_hash(self.db, None, self.grove_version)?;
                match committed {
                    Ok(()) => Outcome::Returned(Value::Nothing),
                    Err(error) => failed(&error, "commit failed", &mut remarks),
                }
            }
            Ok(applied) => Outcome::Returned(self.value(applied)?),
            Err(error) => failed(&error, "failed", &mut remarks),
        };
        let is_read = matches!(op, Op::Read(_));
        let root_hash = if is_read {
            None
        } else {
            Some(root_hash(
                self.db,
                self.transaction.as_ref(),
                self.grove_version,
            )?)
        };
        let proof = match op {
            Op::Read(Read::Query(query)) => Some(self.prove(query, &mut remarks)?),
            _ => None,
        };
        Ok(Observed {
            step: Step {
                cost,
                outcome,
                root_hash,
                proof,
            },
            remarks,
        })
    }

    /// What an applied operation returned, in the form that is compared.
    fn value(&self, applied: Applied) -> Result<Value, Error> {
        let serialize = |element: &grovedb::Element| {
            element
                .serialize(self.grove_version)
                .map_err(|error| Error::engine("serializing a returned element", &error))
        };
        Ok(match applied {
            Applied::Written | Applied::EndOfBlock => Value::Nothing,
            Applied::Element(element) => Value::Element(serialize(&element)?),
            Applied::Rows(rows) => {
                let mut encoded = Vec::new();
                for (path, key, element) in rows.to_path_key_elements() {
                    let element = serialize(&element)?;
                    encoded.push(Row { path, key, element });
                }
                Value::Rows(encoded)
            }
        })
    }

    /// Proves `query` against committed state and verifies the proof.
    fn prove(&self, query: &Query, remarks: &mut Vec<String>) -> Result<Proof, Error> {
        let path_query = query.to_path_query();
        let CostContext { value, cost } =
            self.db.prove_query(&path_query, None, self.grove_version);
        let proved = match value {
            Ok(bytes) => {
                let verified = if query.raw {
                    GroveDb::verify_query_raw(&bytes, &path_query, self.grove_version)
                        .map(|(root, _)| root)
                } else {
                    GroveDb::verify_query(&bytes, &path_query, self.grove_version)
                        .map(|(root, _)| root)
                };
                Proved::Bytes(bytes, verification(verified, &self.committed_root, remarks))
            }
            Err(error) => {
                remarks.push(format!("prove failed: {error}"));
                Proved::Failed(error_name(&error))
            }
        };
        Ok(Proof { cost, proved })
    }
}

/// What the verifier made of a proof, given the root hash it verified to.
fn verification(
    verified: Result<[u8; 32], grovedb::Error>,
    committed_root: &[u8; 32],
    remarks: &mut Vec<String>,
) -> Verification {
    match verified {
        Ok(root) if root == *committed_root => Verification::Verified,
        Ok(root) => {
            remarks.push(format!("the proof verifies to {}", hex::encode(root)));
            Verification::OtherRoot
        }
        Err(error) => {
            remarks.push(format!("verification refused: {error}"));
            Verification::Refused(error_name(&error))
        }
    }
}

fn failed(error: &grovedb::Error, what: &str, remarks: &mut Vec<String>) -> Outcome {
    remarks.push(format!("{what}: {error}"));
    Outcome::Failed(error_name(error))
}

fn root_hash(
    db: &GroveDb,
    transaction: Option<&Transaction>,
    grove_version: &GroveVersion,
) -> Result<[u8; 32], Error> {
    db.root_hash(transaction, grove_version)
        .value
        .map_err(|error| Error::engine("reading the root hash", &error))
}

/// The name of an error: its variant, without what the variant carries.
///
/// The name is what a caller matches on and what another engine has to
/// return as well. The message is free text and is not compared.
pub fn error_name(error: &impl Debug) -> String {
    let debug = format!("{error:?}");
    let end = debug
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .unwrap_or(debug.len());
    debug[..end].to_string()
}

#[cfg(test)]
mod tests {
    use grovedb::{Element, Error as GroveError};

    use super::*;
    use crate::workload::{BatchOp, QueryItem, Write};

    fn insert(path: &[&[u8]], key: &[u8], element: Element) -> Op {
        Op::Write(Write::Insert {
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            key: key.to_vec(),
            element,
        })
    }

    fn query(path: &[&[u8]], raw: bool) -> Op {
        Op::Read(Read::Query(Query {
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            items: vec![QueryItem::RangeFull],
            subquery_path: None,
            subquery_all: false,
            limit: None,
            left_to_right: true,
            raw,
        }))
    }

    fn get(path: &[&[u8]], key: &[u8]) -> Op {
        Op::Read(Read::Get {
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            key: key.to_vec(),
        })
    }

    #[test]
    fn an_error_is_named_by_its_variant() {
        assert_eq!(error_name(&GroveError::CyclicReference), "CyclicReference");
        assert_eq!(
            error_name(&GroveError::PathKeyNotFound("a key".to_string())),
            "PathKeyNotFound"
        );
        #[derive(Debug)]
        #[allow(dead_code)]
        enum Shapes {
            Named { field: u8 },
            Unit2,
        }
        assert_eq!(error_name(&Shapes::Named { field: 1 }), "Named");
        assert_eq!(error_name(&Shapes::Unit2), "Unit2");
    }

    #[test]
    fn every_kind_of_step_is_observed() {
        let grove_version = GroveVersion::latest();
        let dir = tempfile::tempdir().expect("temp dir");
        let db = GroveDb::open(dir.path()).expect("open");
        let mut runner = Runner::new(&db, grove_version).expect("runner");
        let empty_root = runner.committed_root;

        let written = runner
            .step(&insert(&[], b"tree", Element::empty_tree()))
            .expect("step");
        assert_eq!(written.step.outcome, Outcome::Returned(Value::Nothing));
        assert!(written.step.cost.seek_count > 0);
        let root_in_transaction = written.step.root_hash.expect("a write has a root hash");
        assert_ne!(root_in_transaction, empty_root);
        assert_eq!(written.step.proof, None);
        assert!(written.remarks.is_empty());
        assert_eq!(
            runner.committed_root, empty_root,
            "nothing is committed yet"
        );

        let batch = runner
            .step(&Op::Batch(vec![
                BatchOp::InsertOnly {
                    path: vec![b"tree".to_vec()],
                    key: b"a".to_vec(),
                    element: Element::new_item(b"one".to_vec()),
                },
                BatchOp::InsertOnly {
                    path: vec![b"tree".to_vec()],
                    key: b"b".to_vec(),
                    element: Element::new_reference(
                        grovedb::element::reference_path::ReferencePathType::SiblingReference(
                            b"a".to_vec(),
                        ),
                    ),
                },
            ]))
            .expect("step");
        let root_after_batch = batch.step.root_hash.expect("a batch has a root hash");
        assert_ne!(root_after_batch, root_in_transaction);

        let commit = runner.step(&Op::Commit).expect("step");
        assert_eq!(commit.step.outcome, Outcome::Returned(Value::Nothing));
        assert_eq!(commit.step.root_hash, Some(root_after_batch));
        assert_eq!(runner.committed_root, root_after_batch);

        let item = Element::new_item(b"one".to_vec())
            .serialize(grove_version)
            .expect("serialize");
        let read = runner.step(&get(&[b"tree"], b"b")).expect("step");
        assert_eq!(
            read.step.outcome,
            Outcome::Returned(Value::Element(item.clone())),
            "get follows the reference"
        );
        assert_eq!(read.step.root_hash, None);
        assert_eq!(read.step.proof, None);

        for raw in [false, true] {
            let rows = runner.step(&query(&[b"tree"], raw)).expect("step");
            let Outcome::Returned(Value::Rows(rows_returned)) = &rows.step.outcome else {
                panic!("a query returns rows: {rows:?}");
            };
            assert_eq!(rows_returned.len(), 2);
            assert_eq!(rows_returned[0].path, vec![b"tree".to_vec()]);
            assert_eq!(rows_returned[0].key, b"a");
            assert_eq!(rows_returned[0].element, item);
            assert_eq!(rows_returned[1].element == item, !raw);
            let proof = rows.step.proof.expect("a query is proved");
            let Proved::Bytes(bytes, verification) = proof.proved else {
                panic!("the query is provable");
            };
            assert!(!bytes.is_empty());
            assert_eq!(verification, Verification::Verified);
            assert!(proof.cost.seek_count > 0);
            assert_eq!(rows.step.root_hash, None);
            assert!(rows.remarks.is_empty());
        }
    }

    #[test]
    fn failures_are_observed_not_raised() {
        let grove_version = GroveVersion::latest();
        let dir = tempfile::tempdir().expect("temp dir");
        let db = GroveDb::open(dir.path()).expect("open");
        let mut runner = Runner::new(&db, grove_version).expect("runner");
        runner
            .step(&insert(&[], b"tree", Element::empty_tree()))
            .expect("step");
        runner.step(&Op::Commit).expect("step");
        let committed = runner.committed_root;

        let missing = runner.step(&get(&[b"tree"], b"absent")).expect("step");
        assert_eq!(
            missing.step.outcome,
            Outcome::Failed("PathKeyNotFound".to_string())
        );
        assert_eq!(missing.remarks.len(), 1);
        assert!(missing.remarks[0].starts_with("failed: "), "{missing:?}");

        let refused = runner
            .step(&insert(&[b"nowhere"], b"key", Element::empty_tree()))
            .expect("step");
        assert!(
            matches!(refused.step.outcome, Outcome::Failed(_)),
            "{refused:?}"
        );
        assert_eq!(
            refused.step.root_hash,
            Some(committed),
            "a refused write leaves the root hash alone, and it is still read"
        );

        let absent = runner.step(&query(&[b"nowhere"], false)).expect("step");
        assert_eq!(
            absent.step.outcome,
            Outcome::Returned(Value::Rows(Vec::new())),
            "a query of a missing path returns no rows, and proves that"
        );
        assert!(matches!(
            absent.step.proof,
            Some(Proof {
                proved: Proved::Bytes(_, Verification::Verified),
                ..
            })
        ));
    }

    /// Grove version 1 cannot prove a query hosted by a
    /// `ProvableCountProvableSumTree`, and its proof of a reference in a
    /// `ProvableCountSumTree` does not verify.
    #[test]
    fn a_proof_that_fails_or_does_not_verify_is_observed() {
        let grove_version = GroveVersion::first();
        let dir = tempfile::tempdir().expect("temp dir");
        let db = GroveDb::open(dir.path()).expect("open");
        let mut runner = Runner::new(&db, grove_version).expect("runner");
        let reference = Element::new_reference(
            grovedb::element::reference_path::ReferencePathType::AbsolutePathReference(vec![
                b"target".to_vec(),
            ]),
        );
        for op in [
            insert(&[], b"target", Element::new_item(b"value".to_vec())),
            insert(
                &[],
                b"unprovable",
                Element::empty_provable_count_provable_sum_tree(),
            ),
            insert(&[b"unprovable"], b"a", Element::new_sum_item(5)),
            insert(
                &[],
                b"unverifiable",
                Element::empty_provable_count_sum_tree(),
            ),
            insert(&[b"unverifiable"], b"r", reference),
            Op::Commit,
        ] {
            let observed = runner.step(&op).expect("step");
            assert!(
                matches!(observed.step.outcome, Outcome::Returned(_)),
                "{observed:?}"
            );
        }

        let unprovable = runner.step(&query(&[b"unprovable"], true)).expect("step");
        assert!(matches!(
            unprovable.step.outcome,
            Outcome::Returned(Value::Rows(_))
        ));
        let proof = unprovable
            .step
            .proof
            .as_ref()
            .expect("proving is attempted");
        assert_eq!(proof.proved, Proved::Failed("NotSupported".to_string()));
        assert_eq!(unprovable.remarks.len(), 1, "{unprovable:?}");
        assert!(unprovable.remarks[0].starts_with("prove failed: "));

        let unverifiable = runner.step(&query(&[b"unverifiable"], true)).expect("step");
        let proof = unverifiable
            .step
            .proof
            .as_ref()
            .expect("the query is proved");
        assert!(
            matches!(
                &proof.proved,
                Proved::Bytes(bytes, Verification::Refused(error))
                    if !bytes.is_empty() && error == "InvalidProof"
            ),
            "{proof:?}"
        );
        assert!(unverifiable.remarks[0].starts_with("verification refused: "));
    }

    #[test]
    fn a_proof_is_verified_against_the_committed_root() {
        let mut remarks = Vec::new();
        assert_eq!(
            verification(Ok([1; 32]), &[1; 32], &mut remarks),
            Verification::Verified
        );
        assert!(remarks.is_empty());
        assert_eq!(
            verification(Ok([2; 32]), &[1; 32], &mut remarks),
            Verification::OtherRoot
        );
        assert_eq!(
            remarks,
            [format!("the proof verifies to {}", "02".repeat(32))]
        );
        assert_eq!(
            verification(
                Err(GroveError::InvalidProof(
                    grovedb::PathQuery::new_unsized(Vec::new(), grovedb::Query::new()),
                    "broken".to_string()
                )),
                &[1; 32],
                &mut remarks
            ),
            Verification::Refused("InvalidProof".to_string())
        );
        assert!(
            remarks[1].starts_with("verification refused: "),
            "{remarks:?}"
        );
    }

    /// A read inside an open block is answered from the transaction and
    /// proved from committed state, so the proof is of another state. That
    /// is recorded as it comes.
    #[test]
    fn a_proof_of_uncommitted_state_verifies_to_the_committed_root() {
        let grove_version = GroveVersion::latest();
        let dir = tempfile::tempdir().expect("temp dir");
        let db = GroveDb::open(dir.path()).expect("open");
        let mut runner = Runner::new(&db, grove_version).expect("runner");
        runner
            .step(&insert(&[], b"tree", Element::empty_tree()))
            .expect("step");
        runner.step(&Op::Commit).expect("step");
        runner
            .step(&insert(
                &[b"tree"],
                b"a",
                Element::new_item(b"uncommitted".to_vec()),
            ))
            .expect("step");
        let rows = runner.step(&query(&[b"tree"], false)).expect("step");
        let Outcome::Returned(Value::Rows(returned)) = &rows.step.outcome else {
            panic!("a query returns rows: {rows:?}");
        };
        assert_eq!(returned.len(), 1, "the query reads the open transaction");
        let Some(Proof {
            proved: Proved::Bytes(_, verification),
            ..
        }) = rows.step.proof
        else {
            panic!("the query is provable");
        };
        assert_eq!(verification, Verification::Verified);
    }
}
