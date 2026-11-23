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
//! - for a read GroveDB proves ([`provable`]), the proof
//!   `GroveDb::prove_query` builds from committed state, the cost of
//!   building it, and what the verifier makes of it.
//!
//! Nothing here requires an operation to succeed or a proof to verify: what
//! the engine returns is what is recorded.

use std::fmt::Debug;

use grovedb::{
    operations::{proof::indexed_axis::AxisEntries, QueryItemOrSumReturnType},
    query_result_type::QueryResultElements,
    AggregateSumQueryResult, AxisAggregateValue, AxisKeys, Element, GroveDb, IndexedAxisEntry,
    PathQuery, PathQueryRun,
};
use grovedb_costs::CostContext;
use grovedb_version::version::GroveVersion;

use super::{
    step::{Field, Outcome, Proof, Proved, Row, Step, Value, Verification},
    Error,
};
// The one engine there is where `GroveDb` is not generic over its engine
// (`workload::pinned`).
#[cfg(not(grovedb_engine_seam))]
use crate::workload::DefaultEngine as E;
use crate::workload::{
    Db, DefaultEngine, Op, Outcome as Applied, Query, Read, StreamEngine, Transactions, TxArg,
};

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
pub struct Runner<'db, E: StreamEngine = DefaultEngine> {
    db: &'db Db<E>,
    grove_version: &'db GroveVersion,
    transactions: Transactions<'db, E>,
    committed_root: [u8; 32],
}

impl<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine> Runner<'db, E> {
    /// A runner on `db`, which stays open for further traces afterwards.
    pub fn new(db: &'db Db<E>, grove_version: &'db GroveVersion) -> Result<Self, Error> {
        let committed_root = root_hash(db, None, grove_version)?;
        Ok(Self {
            db,
            grove_version,
            transactions: Transactions::new(db),
            committed_root,
        })
    }

    /// The transactions the trace runs in: what a caller reads through to
    /// see the state the last step left.
    pub fn transactions(&self) -> &Transactions<'db, E> {
        &self.transactions
    }

    /// Applies one operation and reads back everything the corpus compares.
    pub fn step(&mut self, op: &Op) -> Result<Observed, Error> {
        let mut remarks = Vec::new();
        // The transactions are kept by `Transactions`, which commits at the
        // end of a block and adds the cost of the commit, drops an
        // abandoned transaction, and holds the snapshot a stream opens.
        let CostContext { value, cost } = self.transactions.apply(op, self.grove_version);
        if op.ends_transaction() || op.commits_by_itself() {
            self.committed_root = root_hash(self.db, None, self.grove_version)?;
        }
        let outcome = match value {
            Ok(applied) => Outcome::Returned(self.value(applied, &mut remarks)?),
            Err(error) if matches!(op, Op::Commit) => failed(&error, "commit failed", &mut remarks),
            Err(error) => failed(&error, "failed", &mut remarks),
        };
        let (root_hash, proof) = match op {
            Op::Read(read) => (None, self.prove(read, &mut remarks)?),
            _ => (
                Some(root_hash(
                    self.db,
                    Some(self.transactions.block()),
                    self.grove_version,
                )?),
                None,
            ),
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
    ///
    /// Every outcome is named here and none falls through, so an outcome
    /// added to the workloads does not compile until it is recorded.
    fn value(&self, applied: Applied, remarks: &mut Vec<String>) -> Result<Value, Error> {
        let optional = |element: Option<Element>| -> Result<Field, Error> {
            Ok(match element {
                Some(element) => Field::Bytes(self.serialized(&element)?),
                None => Field::Absent,
            })
        };
        let bytes = |value: Option<Vec<u8>>| value.map_or(Field::Absent, Field::Bytes);
        let count_or_absent = |count: Option<u64>| count.map_or(Field::Absent, number);
        Ok(match applied {
            // A step on the transaction returns nothing: what it did shows
            // in the root hash of the step and in the reads after it.
            Applied::Written
            | Applied::EndOfBlock
            | Applied::RolledBack
            | Applied::SavepointSet
            | Applied::Abandoned
            | Applied::SnapshotOpens
            | Applied::SnapshotCloses => Value::Nothing,
            Applied::Flag(flag) => Value::Record("flag", vec![("flag", Field::Flag(flag))]),
            Applied::Existing(element) => {
                Value::Record("existing", vec![("element", optional(element)?)])
            }
            Applied::Changed { changed, previous } => Value::Record(
                "changed",
                vec![
                    ("changed", Field::Flag(changed)),
                    ("previous", optional(previous)?),
                ],
            ),
            Applied::Deleted(deleted) => {
                Value::Record("deleted", vec![("deleted", number(deleted))])
            }
            Applied::Appended { root, position } => Value::Record(
                "appended",
                vec![
                    ("root", Field::Bytes(root.to_vec())),
                    ("position", number(position)),
                ],
            ),
            Applied::Value(value) => Value::Record("value", vec![("value", bytes(value))]),
            Applied::Values(values) => Value::Record(
                "values",
                vec![("values", list(values, |value| vec![Field::Bytes(value)]))],
            ),
            Applied::Hash(hash) => {
                Value::Record("hash", vec![("hash", Field::Bytes(hash.to_vec()))])
            }
            Applied::Count(count) => Value::Record("count", vec![("count", number(count))]),
            Applied::Page(page) => Value::Record(
                "page",
                vec![
                    (
                        "entries",
                        list(page.entries, |(position, value)| {
                            vec![number(position), Field::Bytes(value)]
                        }),
                    ),
                    ("total", number(page.total_count)),
                ],
            ),
            Applied::OptionalRows(rows) => {
                let mut items = Vec::new();
                for (path, key, element) in rows {
                    let path = list(path, |segment| vec![Field::Bytes(segment)]);
                    items.push(vec![path, Field::Bytes(key), optional(element)?]);
                }
                Value::Record("optional-rows", vec![("rows", Field::List(items))])
            }
            Applied::ItemValues { values, skipped } => Value::Record(
                "item-values",
                vec![
                    ("values", list(values, |value| vec![Field::Bytes(value)])),
                    ("skipped", number(skipped)),
                ],
            ),
            Applied::ItemValuesOrSums { values, skipped } => Value::Record(
                "item-values-or-sums",
                vec![
                    ("values", list(values, item_or_sum)),
                    ("skipped", number(skipped)),
                ],
            ),
            Applied::SumValues { sums, skipped } => Value::Record(
                "sum-values",
                vec![
                    ("sums", list(sums, |sum| vec![number(sum)])),
                    ("skipped", number(skipped)),
                ],
            ),
            Applied::BigSum { count, sum } => Value::Record(
                "big-sum",
                vec![("count", count_or_absent(count)), ("sum", number(sum))],
            ),
            Applied::BigSumPerKey(keys) => Value::Record(
                "big-sum-per-key",
                vec![(
                    "keys",
                    list(keys, |(key, count, sum)| {
                        vec![Field::Bytes(key), count_or_absent(count), number(sum)]
                    }),
                )],
            ),
            Applied::Proof(proof) => Value::Record("proof", vec![("proof", Field::Bytes(proof))]),
            Applied::Element(element) => Value::Element(self.serialized(&element)?),
            Applied::Rows(rows) => Value::Rows(self.rows(rows)?),
            Applied::WrittenInSegments { first_segment } => Value::Record(
                "written-in-segments",
                vec![("first-segment", Field::Cost(first_segment))],
            ),
            Applied::Reclaimed(report) => {
                if let Some(error) = report.first_unresolved_error {
                    remarks.push(format!("first unresolved: {error}"));
                }
                Value::Record(
                    "reclaimed",
                    vec![
                        ("reclaimed", number(report.reclaimed_records)),
                        ("skipped-live", number(report.skipped_live)),
                        ("unresolved", number(report.unresolved_records)),
                    ],
                )
            }
            Applied::Run(run) => self.run(run)?,
            Applied::Sums(sums) => Value::Record("sums", sums_fields(sums)),
            Applied::Aux(value) => Value::Record(
                "aux",
                vec![("value", value.map_or(Field::Absent, Field::Bytes))],
            ),
            Applied::AuxEntries(entries) => Value::Record(
                "aux-entries",
                vec![(
                    "entries",
                    list(entries, |(key, value)| {
                        vec![Field::Bytes(key), Field::Bytes(value)]
                    }),
                )],
            ),
        })
    }

    /// What `GroveDb::run_path_query` returned, one kind of record for each
    /// shape of answer.
    fn run(&self, run: PathQueryRun) -> Result<Value, Error> {
        let per_key = |name, keys| Value::Record(name, vec![("keys", keys)]);
        Ok(match run {
            PathQueryRun::Elements { elements, skipped } => Value::Record(
                "elements",
                vec![
                    ("rows", Field::Rows(self.rows(elements)?)),
                    ("skipped", number(skipped)),
                ],
            ),
            PathQueryRun::AggregateCount(count) => {
                Value::Record("aggregate-count", vec![("count", number(count))])
            }
            PathQueryRun::AggregateSum(sum) => {
                Value::Record("aggregate-sum", vec![("sum", number(sum))])
            }
            PathQueryRun::AggregateCountAndSum { count, sum } => Value::Record(
                "aggregate-count-and-sum",
                vec![("count", number(count)), ("sum", number(sum))],
            ),
            PathQueryRun::AggregateCountPerKey(keys) => per_key(
                "aggregate-count-per-key",
                list(keys, |(key, count)| vec![Field::Bytes(key), number(count)]),
            ),
            PathQueryRun::AggregateSumPerKey(keys) => per_key(
                "aggregate-sum-per-key",
                list(keys, |(key, sum)| vec![Field::Bytes(key), number(sum)]),
            ),
            PathQueryRun::AggregateCountAndSumPerKey(keys) => per_key(
                "aggregate-count-and-sum-per-key",
                list(keys, |(key, count, sum)| {
                    vec![Field::Bytes(key), number(count), number(sum)]
                }),
            ),
            PathQueryRun::AxisEntries { entries, skipped } => {
                let (axis, entries) = self.axis_entries(entries)?;
                Value::Record(
                    "axis-entries",
                    vec![
                        ("axis", axis),
                        ("entries", entries),
                        ("skipped", skipped.map_or(Field::Absent, number)),
                    ],
                )
            }
            PathQueryRun::BranchedAxisEntries(branches) => {
                let mut items = Vec::new();
                for (key, entries) in branches {
                    let mut item = vec![Field::Bytes(key)];
                    match entries {
                        Some(entries) => {
                            let (axis, entries) = self.axis_entries(entries)?;
                            item.extend([axis, entries]);
                        }
                        None => item.push(Field::Absent),
                    }
                    items.push(item);
                }
                Value::Record(
                    "branched-axis-entries",
                    vec![("branches", Field::List(items))],
                )
            }
            PathQueryRun::AxisKeys { keys, skipped } => {
                let (axis, keys) = axis_keys(keys);
                Value::Record(
                    "axis-keys",
                    vec![
                        ("axis", axis),
                        ("keys", keys),
                        ("skipped", skipped.map_or(Field::Absent, number)),
                    ],
                )
            }
            PathQueryRun::BranchedAxisKeys(branches) => Value::Record(
                "branched-axis-keys",
                vec![(
                    "branches",
                    list(branches, |(key, keys)| {
                        let mut item = vec![Field::Bytes(key)];
                        match keys {
                            Some(keys) => {
                                let (axis, keys) = axis_keys(keys);
                                item.extend([axis, keys]);
                            }
                            None => item.push(Field::Absent),
                        }
                        item
                    }),
                )],
            ),
            PathQueryRun::AxisRank(rank) => {
                Value::Record("axis-rank", vec![("rank", number(rank))])
            }
            PathQueryRun::AxisAggregate(aggregate) => {
                let (metric, value) = match aggregate {
                    AxisAggregateValue::Population(population) => {
                        ("population", number(population))
                    }
                    AxisAggregateValue::Total(total) => ("total", number(total)),
                };
                Value::Record(
                    "axis-aggregate",
                    vec![("metric", Field::Word(metric)), ("value", value)],
                )
            }
            PathQueryRun::SumBudget(sums) => Value::Record("sum-budget", sums_fields(sums)),
        })
    }

    /// The entries of an axis read: the axis, and for each entry the value
    /// it is ordered by, its key and its element.
    fn axis_entries(&self, entries: AxisEntries) -> Result<(Field, Field), Error> {
        fn items<T: Into<i128>, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
            runner: &Runner<'_, E>,
            entries: Vec<IndexedAxisEntry<T>>,
        ) -> Result<Field, Error> {
            let mut items = Vec::new();
            for entry in entries {
                items.push(vec![
                    number(entry.ordering_value),
                    Field::Bytes(entry.primary_key),
                    Field::Bytes(runner.serialized(&entry.value)?),
                ]);
            }
            Ok(Field::List(items))
        }
        Ok(match entries {
            AxisEntries::Count(entries) => (Field::Word("count"), items(self, entries)?),
            AxisEntries::Sum(entries) => (Field::Word("sum"), items(self, entries)?),
            AxisEntries::Avg(entries) => (Field::Word("avg"), items(self, entries)?),
            AxisEntries::BigSum(entries) => (Field::Word("big_sum"), items(self, entries)?),
        })
    }

    fn rows(&self, rows: QueryResultElements) -> Result<Vec<Row>, Error> {
        let mut encoded = Vec::new();
        for (path, key, element) in rows.to_path_key_elements() {
            let element = self.serialized(&element)?;
            encoded.push(Row { path, key, element });
        }
        Ok(encoded)
    }

    fn serialized(&self, element: &Element) -> Result<Vec<u8>, Error> {
        element
            .serialize(self.grove_version)
            .map_err(|error| Error::engine("serializing a returned element", &error))
    }

    /// Proves a read against committed state and verifies the proof. `None`
    /// for a read that is not proved.
    fn prove(&self, read: &Read, remarks: &mut Vec<String>) -> Result<Option<Proof>, Error> {
        let Some(provable) = provable(read) else {
            return Ok(None);
        };
        let converted;
        let path_query = match provable {
            Provable::Rows(query) => {
                converted = query.to_path_query();
                &converted
            }
            Provable::Answer(path_query)
            | Provable::KeysOptional(path_query)
            | Provable::BigSum {
                query: path_query, ..
            } => path_query,
        };
        let CostContext { value, cost } = self.db.prove_query(path_query, None, self.grove_version);
        let proved = match value {
            Ok(bytes) => {
                let verified = match provable {
                    Provable::Rows(query) if query.raw => {
                        GroveDb::verify_query_raw(&bytes, path_query, self.grove_version)
                            .map(|(root, _)| root)
                    }
                    Provable::Rows(_) => {
                        GroveDb::verify_query(&bytes, path_query, self.grove_version)
                            .map(|(root, _)| root)
                    }
                    Provable::Answer(_) => {
                        GroveDb::verify_path_query(&bytes, path_query, self.grove_version)
                            .map(|verified| *verified.root_hash())
                    }
                    Provable::KeysOptional(_) => GroveDb::verify_query_with_absence_proof(
                        &bytes,
                        path_query,
                        self.grove_version,
                    )
                    .map(|(root, _)| root),
                    Provable::BigSum {
                        with_count,
                        per_key,
                        ..
                    } => {
                        verify_big_sum(&bytes, path_query, with_count, per_key, self.grove_version)
                    }
                };
                Proved::Bytes(bytes, verification(verified, &self.committed_root, remarks))
            }
            Err(error) => {
                remarks.push(format!("prove failed: {error}"));
                Proved::Failed(error_name(&error))
            }
        };
        Ok(Some(Proof { cost, proved }))
    }
}

/// How a read is proved: always with `GroveDb::prove_query`, and verified by
/// the verifier of its kind of answer.
#[derive(Clone, Copy, Debug)]
pub enum Provable<'read> {
    /// A query that returns rows, verified by `GroveDb::verify_query`, or by
    /// `GroveDb::verify_query_raw` when it returns elements as stored.
    Rows(&'read Query),
    /// A path query in GroveDB's whole grammar, verified by
    /// `GroveDb::verify_path_query`.
    Answer(&'read PathQuery),
    /// A query for keys that answers for each of them, verified by
    /// `GroveDb::verify_query_with_absence_proof`.
    KeysOptional(&'read PathQuery),
    /// A 128-bit sum of a key range, verified by the verifier named after
    /// its reader, such as `GroveDb::verify_aggregate_big_sum_query`.
    BigSum {
        /// The query.
        query: &'read PathQuery,
        /// Whether the count is returned with the sum.
        with_count: bool,
        /// Whether there is one answer for each selected key.
        per_key: bool,
    },
}

/// Verifies the proof of a [`Read::BigSum`] with the verifier of its form
/// and returns the root hash it verifies to.
#[expect(
    clippy::result_large_err,
    reason = "a verifier fails with GroveDB's own error, whatever its size"
)]
fn verify_big_sum(
    proof: &[u8],
    query: &PathQuery,
    with_count: bool,
    per_key: bool,
    grove_version: &GroveVersion,
) -> Result<[u8; 32], grovedb::Error> {
    match (with_count, per_key) {
        (false, false) => GroveDb::verify_aggregate_big_sum_query(proof, query, grove_version)
            .map(|(root, _)| root),
        (true, false) => {
            GroveDb::verify_aggregate_count_and_big_sum_query(proof, query, grove_version)
                .map(|(root, ..)| root)
        }
        (false, true) => {
            GroveDb::verify_aggregate_big_sum_query_per_key(proof, query, grove_version)
                .map(|(root, _)| root)
        }
        (true, true) => {
            GroveDb::verify_aggregate_count_and_big_sum_query_per_key(proof, query, grove_version)
                .map(|(root, _)| root)
        }
    }
}

/// One value `GroveDb::query_item_value_or_sum` returned: what it is, then
/// what it holds.
fn item_or_sum(value: QueryItemOrSumReturnType) -> Vec<Field> {
    match value {
        QueryItemOrSumReturnType::ItemData(item) => {
            vec![Field::Word("item"), Field::Bytes(item)]
        }
        QueryItemOrSumReturnType::SumValue(sum) => vec![Field::Word("sum"), number(sum)],
        QueryItemOrSumReturnType::BigSumValue(sum) => vec![Field::Word("big-sum"), number(sum)],
        QueryItemOrSumReturnType::CountValue(count) => vec![Field::Word("count"), number(count)],
        QueryItemOrSumReturnType::CountSumValue(count, sum) => {
            vec![Field::Word("count-and-sum"), number(count), number(sum)]
        }
        QueryItemOrSumReturnType::CountBigSumValue(count, sum) => {
            vec![Field::Word("count-and-big-sum"), number(count), number(sum)]
        }
        QueryItemOrSumReturnType::ItemDataWithSumValue(item, sum) => {
            vec![
                Field::Word("item-with-sum"),
                Field::Bytes(item),
                number(sum),
            ]
        }
    }
}

/// How a read is proved, for a read that is.
///
/// Every kind of read is named here and none falls through, so a kind added
/// to the workloads does not compile until it is said whether, and how, it
/// is proved. A read that is recorded without its proof is a read whose
/// proof bytes no replay compares.
///
/// A [`Read::PathQuery`] is proved when its stream marks it as proven. The
/// ones it does not mark are the queries GroveDB refuses to prove on the
/// grove version the stream was generated for. The readers that return
/// rows, the sum-budget walk and the auxiliary keyspace have no proof, and
/// neither have the checks, the typed reads of the append-only trees and
/// the readers that return values or sums. A [`Read::Prove`] returns the
/// bytes of its proof as its result, so they are compared as the result
/// and nothing is proved a second time.
pub fn provable(read: &Read) -> Option<Provable<'_>> {
    match read {
        Read::Query(query) => Some(Provable::Rows(query)),
        Read::PathQuery {
            query,
            proven: true,
        } => Some(Provable::Answer(query)),
        Read::KeysOptional { query, .. } => Some(Provable::KeysOptional(query)),
        Read::BigSum {
            query,
            with_count,
            per_key,
        } => Some(Provable::BigSum {
            query,
            with_count: *with_count,
            per_key: *per_key,
        }),
        Read::PathQuery { proven: false, .. }
        | Read::Get { .. }
        | Read::Rows { .. }
        | Read::SumBudget { .. }
        | Read::GetAux { .. }
        | Read::AuxByPrefix { .. }
        | Read::HasRaw { .. }
        | Read::IsEmptyTree { .. }
        | Read::Fetch { .. }
        | Read::Tree { .. }
        | Read::ItemValues { .. }
        | Read::SumValues { .. }
        | Read::Many { .. }
        | Read::Prove(_) => None,
    }
}

fn number(value: impl Into<i128>) -> Field {
    Field::Number(value.into())
}

fn list<T>(items: Vec<T>, fields: impl FnMut(T) -> Vec<Field>) -> Field {
    Field::List(items.into_iter().map(fields).collect())
}

/// The keys of an axis read: the axis, and for each key the value it is
/// ordered by.
fn axis_keys(keys: AxisKeys) -> (Field, Field) {
    fn items<T: Into<i128>>(keys: Vec<(T, Vec<u8>)>) -> Field {
        list(keys, |(ordering_value, key)| {
            vec![number(ordering_value), Field::Bytes(key)]
        })
    }
    match keys {
        AxisKeys::Count(keys) => (Field::Word("count"), items(keys)),
        AxisKeys::Sum(keys) => (Field::Word("sum"), items(keys)),
        AxisKeys::Avg(keys) => (Field::Word("avg"), items(keys)),
        AxisKeys::BigSum(keys) => (Field::Word("big_sum"), items(keys)),
    }
}

/// What a walk over sum items returned: each key with its sum, whether the
/// walk stopped at GroveDB's hard limit, and how many elements it met.
fn sums_fields(sums: AggregateSumQueryResult) -> Vec<(&'static str, Field)> {
    vec![
        (
            "sums",
            list(sums.results, |(key, sum)| {
                vec![Field::Bytes(key), number(sum)]
            }),
        ),
        ("hard-limit-reached", Field::Flag(sums.hard_limit_reached)),
        ("scanned", number(sums.elements_scanned)),
    ]
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

fn root_hash<'db, #[cfg(grovedb_engine_seam)] E: StreamEngine>(
    db: &'db Db<E>,
    transaction: TxArg<'db, '_, E>,
    grove_version: &GroveVersion,
) -> Result<[u8; 32], Error> {
    db.root_hash(transaction, grove_version)
        .value
        .map_err(|error| Error::engine("reading the root hash", &error))
}

/// The name of an error: its variant, and for a variant that only wraps the
/// error of a lower layer, that error's variant as well, as in
/// `MerkError::StorageError`.
///
/// The name is what a caller matches on and what another engine has to
/// return as well. The message is free text and is not compared.
///
/// A storage error is named in terms that name no engine
/// ([`storage_error_name`]).
pub fn error_name(error: &grovedb::Error) -> String {
    let wrapped = match error {
        grovedb::Error::MerkError(wrapped) => Some(variant_name(wrapped)),
        grovedb::Error::StorageError(wrapped) => Some(storage_error_name(wrapped)),
        grovedb::Error::VersionError(wrapped) => Some(variant_name(wrapped)),
        grovedb::Error::ElementError(wrapped) => Some(variant_name(wrapped)),
        grovedb::Error::QueryError(wrapped) => Some(variant_name(wrapped)),
        _ => None,
    };
    match wrapped {
        Some(wrapped) => format!("{}::{wrapped}", variant_name(error)),
        None => variant_name(error),
    }
}

/// The name of a storage error, in terms that name no engine.
///
/// An engine reports a failure of its own in its own variant, with its
/// own words: the RocksDB engine as `RocksDBError` or `StorageError`,
/// LigninDB as `StorageError`. Both are named `StorageError`. An error
/// that the seam classes as an outcome of a commit
/// (`grovedb_storage::Error::commit_error`) is named by its class, as in
/// `Commit::Conflict`: a conflict is a `RocksDBError` of kind `Busy` on
/// RocksDB and `Commit(Conflict)` on LigninDB. Every other variant is one
/// that any engine returns, and keeps its name.
///
/// The corpus records no storage error of an engine's own (a test of
/// `tests/golden.rs` holds that), so the names of its traces are what the
/// engine it is pinned to gave them.
#[cfg(grovedb_engine_seam)]
fn storage_error_name(error: &grovedb_storage::Error) -> String {
    if let Some(class) = error.commit_error() {
        return format!("Commit::{}", variant_name(&class));
    }
    match variant_name(error).as_str() {
        "RocksDBError" => "StorageError".to_string(),
        name => name.to_string(),
    }
}

/// The name of a storage error at the pin, where the seam does not exist:
/// its variant.
#[cfg(not(grovedb_engine_seam))]
fn storage_error_name(error: &impl Debug) -> String {
    variant_name(error)
}

/// The name of the variant of an enum value, without what the variant
/// carries.
fn variant_name(value: &impl Debug) -> String {
    let debug = format!("{value:?}");
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
        assert_eq!(variant_name(&Shapes::Named { field: 1 }), "Named");
        assert_eq!(variant_name(&Shapes::Unit2), "Unit2");

        assert_eq!(
            error_name(&GroveError::MerkError(
                grovedb_merk::Error::KeyNotFoundError("a key")
            )),
            "MerkError::KeyNotFoundError"
        );
        assert_eq!(
            error_name(&GroveError::MerkError(grovedb_merk::Error::CorruptedState(
                "a state"
            ))),
            "MerkError::CorruptedState"
        );
        assert_eq!(
            error_name(&GroveError::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "a method".to_string(),
                    known_versions: vec![0],
                    received: 1,
                }
            )),
            "VersionError::UnknownVersionMismatch"
        );
    }

    /// A rollback to a savepoint that was never set fails on each engine
    /// in the engine's own words, and a commit that met another is a
    /// conflict in each engine's own form. Both are named alike.
    #[cfg(grovedb_engine_seam)]
    #[test]
    fn a_storage_error_is_named_in_terms_that_name_no_engine() {
        use grovedb::{engine::Engine, LigninEngine, RocksDbEngine};
        use grovedb_storage::error::CommitError;

        fn names<E: Engine>() -> (String, String, String) {
            let grove_version = GroveVersion::latest();
            let dir = tempfile::tempdir().expect("temp dir");
            let db = GroveDb::<E>::open_with_engine(dir.path()).expect("open");
            let root: &[&[u8]] = &[];
            let put = |key: &[u8], transaction| {
                db.insert(
                    root,
                    key,
                    Element::new_item(vec![1]),
                    None,
                    transaction,
                    grove_version,
                )
            };

            let transaction = db.start_transaction();
            let no_savepoint = db
                .rollback_to_savepoint(&transaction)
                .expect_err("no savepoint was set");
            assert_eq!(no_savepoint.commit_error(), None);
            drop(transaction);

            let transaction = db.start_transaction();
            (put(b"k", Some(&transaction)).unwrap()).expect("a write of the transaction");
            put(b"k", None).unwrap().expect("a commit beside it");
            let conflict = db
                .commit_transaction(transaction)
                .unwrap()
                .expect_err("the commit met another");
            assert!(conflict.is_commit_conflict());
            let debug = format!("{:?} {:?}", no_savepoint, conflict);
            (error_name(&no_savepoint), error_name(&conflict), debug)
        }

        let (no_savepoint, conflict, in_its_words) = names::<RocksDbEngine>();
        assert_eq!(no_savepoint, "StorageError::StorageError");
        assert_eq!(conflict, "StorageError::Commit::Conflict");
        // RocksDB's own variant is what was named so.
        assert_eq!(
            in_its_words.matches("RocksDBError").count(),
            2,
            "{in_its_words}"
        );
        let (no_savepoint, conflict, in_its_words) = names::<LigninEngine>();
        assert_eq!(no_savepoint, "StorageError::StorageError");
        assert_eq!(conflict, "StorageError::Commit::Conflict");
        assert!(!in_its_words.contains("RocksDBError"), "{in_its_words}");

        // A variant that every engine returns keeps its name, and so does
        // a class of a commit that is no conflict.
        let named = |error: grovedb_storage::Error| error_name(&GroveError::StorageError(error));
        assert_eq!(
            named(grovedb_storage::Error::SnapshotReadOnlyTransaction(
                "a write"
            )),
            "StorageError::SnapshotReadOnlyTransaction"
        );
        assert_eq!(
            named(grovedb_storage::Error::Commit(CommitError::NotDurable)),
            "StorageError::Commit::NotDurable"
        );
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

    fn whole_tree(path: &[&[u8]]) -> Box<PathQuery> {
        Box::new(PathQuery::new_unsized(
            path.iter().map(|segment| segment.to_vec()).collect(),
            grovedb::Query::new_range_full(),
        ))
    }

    fn sum_walk(path: &[&[u8]], sum_limit: u64) -> Read {
        Read::SumBudget {
            query: Box::new(grovedb::AggregateSumPathQuery::new(
                path.iter().map(|segment| segment.to_vec()).collect(),
                grovedb_merk::proofs::query::AggregateSumQuery {
                    items: vec![grovedb::QueryItem::RangeFull(..)],
                    left_to_right: true,
                    sum_limit,
                    limit_of_items_to_check: None,
                },
            )),
            skip_non_sum_items: true,
            ignore_references: false,
        }
    }

    #[test]
    fn it_is_said_of_every_read_whether_it_is_proved() {
        let Op::Read(rows) = query(&[], false) else {
            unreachable!("a query is a read")
        };
        assert!(matches!(provable(&rows), Some(Provable::Rows(_))));
        assert!(matches!(
            provable(&Read::PathQuery {
                query: whole_tree(&[]),
                proven: true,
            }),
            Some(Provable::Answer(_))
        ));
        for read in [
            Read::PathQuery {
                query: whole_tree(&[]),
                proven: false,
            },
            Read::Get {
                path: Vec::new(),
                key: b"key".to_vec(),
            },
            Read::Rows {
                query: whole_tree(&[]),
                raw: true,
            },
            sum_walk(&[], 1),
            Read::GetAux {
                key: b"key".to_vec(),
            },
            Read::AuxByPrefix { prefix: Vec::new() },
        ] {
            assert!(provable(&read).is_none(), "{read:?}");
        }
    }

    #[test]
    fn every_shape_of_answer_is_a_record_of_its_own() {
        use grovedb::query_result_type::QueryResultElement;

        let grove_version = GroveVersion::latest();
        let dir = tempfile::tempdir().expect("temp dir");
        let db = GroveDb::open(dir.path()).expect("open");
        let runner = Runner::new(&db, grove_version).expect("runner");
        let element = Element::new_item(b"one".to_vec());
        let serialized = element.serialize(grove_version).expect("serialize");
        let key = || b"k".to_vec();
        fn entry<T>(ordering_value: T, element: &Element) -> IndexedAxisEntry<T> {
            IndexedAxisEntry {
                ordering_value,
                primary_key: b"k".to_vec(),
                value: element.clone(),
            }
        }
        let entries = |ordering_value: i128| {
            Field::List(vec![vec![
                Field::Number(ordering_value),
                Field::Bytes(key()),
                Field::Bytes(serialized.clone()),
            ]])
        };
        let keys = |ordering_value: i128| {
            Field::List(vec![vec![
                Field::Number(ordering_value),
                Field::Bytes(key()),
            ]])
        };
        let per_key =
            |kind, item: Vec<Field>| Value::Record(kind, vec![("keys", Field::List(vec![item]))]);
        let axis = |kind, list, axis, items, skipped| {
            Value::Record(
                kind,
                vec![
                    ("axis", Field::Word(axis)),
                    (list, items),
                    ("skipped", skipped),
                ],
            )
        };
        let sums = |kind| {
            Value::Record(
                kind,
                vec![
                    (
                        "sums",
                        Field::List(vec![vec![Field::Bytes(key()), Field::Number(5)]]),
                    ),
                    ("hard-limit-reached", Field::Flag(true)),
                    ("scanned", Field::Number(6)),
                ],
            )
        };
        let walked = || AggregateSumQueryResult {
            results: vec![(key(), 5)],
            hard_limit_reached: true,
            elements_scanned: 6,
        };
        let answers: Vec<(PathQueryRun, Value)> = vec![
            (
                PathQueryRun::Elements {
                    elements: QueryResultElements::from_elements(vec![
                        QueryResultElement::PathKeyElementTrioResultItem((
                            vec![b"p".to_vec()],
                            key(),
                            element.clone(),
                        )),
                    ]),
                    skipped: 3,
                },
                Value::Record(
                    "elements",
                    vec![
                        (
                            "rows",
                            Field::Rows(vec![Row {
                                path: vec![b"p".to_vec()],
                                key: key(),
                                element: serialized.clone(),
                            }]),
                        ),
                        ("skipped", Field::Number(3)),
                    ],
                ),
            ),
            (
                PathQueryRun::AggregateCount(4),
                Value::Record("aggregate-count", vec![("count", Field::Number(4))]),
            ),
            (
                PathQueryRun::AggregateSum(-4),
                Value::Record("aggregate-sum", vec![("sum", Field::Number(-4))]),
            ),
            (
                PathQueryRun::AggregateCountAndSum { count: 2, sum: -9 },
                Value::Record(
                    "aggregate-count-and-sum",
                    vec![("count", Field::Number(2)), ("sum", Field::Number(-9))],
                ),
            ),
            (
                PathQueryRun::AggregateCountPerKey(vec![(key(), 1)]),
                per_key(
                    "aggregate-count-per-key",
                    vec![Field::Bytes(key()), Field::Number(1)],
                ),
            ),
            (
                PathQueryRun::AggregateSumPerKey(vec![(key(), -1)]),
                per_key(
                    "aggregate-sum-per-key",
                    vec![Field::Bytes(key()), Field::Number(-1)],
                ),
            ),
            (
                PathQueryRun::AggregateCountAndSumPerKey(vec![(key(), 1, -1)]),
                per_key(
                    "aggregate-count-and-sum-per-key",
                    vec![Field::Bytes(key()), Field::Number(1), Field::Number(-1)],
                ),
            ),
            (
                PathQueryRun::AxisEntries {
                    entries: AxisEntries::Count(vec![entry(5, &element)]),
                    skipped: Some(2),
                },
                axis(
                    "axis-entries",
                    "entries",
                    "count",
                    entries(5),
                    Field::Number(2),
                ),
            ),
            (
                PathQueryRun::AxisEntries {
                    entries: AxisEntries::Sum(vec![entry(-5, &element)]),
                    skipped: None,
                },
                axis("axis-entries", "entries", "sum", entries(-5), Field::Absent),
            ),
            (
                PathQueryRun::AxisEntries {
                    entries: AxisEntries::Avg(vec![entry(-6, &element)]),
                    skipped: None,
                },
                axis("axis-entries", "entries", "avg", entries(-6), Field::Absent),
            ),
            (
                PathQueryRun::BranchedAxisEntries(vec![
                    (
                        b"b".to_vec(),
                        Some(AxisEntries::Count(vec![entry(5, &element)])),
                    ),
                    (b"c".to_vec(), None),
                ]),
                Value::Record(
                    "branched-axis-entries",
                    vec![(
                        "branches",
                        Field::List(vec![
                            vec![
                                Field::Bytes(b"b".to_vec()),
                                Field::Word("count"),
                                entries(5),
                            ],
                            vec![Field::Bytes(b"c".to_vec()), Field::Absent],
                        ]),
                    )],
                ),
            ),
            (
                PathQueryRun::AxisKeys {
                    keys: AxisKeys::Count(vec![(7, key())]),
                    skipped: Some(0),
                },
                axis("axis-keys", "keys", "count", keys(7), Field::Number(0)),
            ),
            (
                PathQueryRun::AxisKeys {
                    keys: AxisKeys::Sum(vec![(-7, key())]),
                    skipped: None,
                },
                axis("axis-keys", "keys", "sum", keys(-7), Field::Absent),
            ),
            (
                PathQueryRun::AxisKeys {
                    keys: AxisKeys::Avg(vec![(-8, key())]),
                    skipped: None,
                },
                axis("axis-keys", "keys", "avg", keys(-8), Field::Absent),
            ),
            (
                PathQueryRun::BranchedAxisKeys(vec![
                    (b"b".to_vec(), Some(AxisKeys::Sum(vec![(-7, key())]))),
                    (b"c".to_vec(), None),
                ]),
                Value::Record(
                    "branched-axis-keys",
                    vec![(
                        "branches",
                        Field::List(vec![
                            vec![Field::Bytes(b"b".to_vec()), Field::Word("sum"), keys(-7)],
                            vec![Field::Bytes(b"c".to_vec()), Field::Absent],
                        ]),
                    )],
                ),
            ),
            (
                PathQueryRun::AxisRank(9),
                Value::Record("axis-rank", vec![("rank", Field::Number(9))]),
            ),
            (
                PathQueryRun::AxisAggregate(AxisAggregateValue::Population(3)),
                Value::Record(
                    "axis-aggregate",
                    vec![
                        ("metric", Field::Word("population")),
                        ("value", Field::Number(3)),
                    ],
                ),
            ),
            (
                PathQueryRun::AxisAggregate(AxisAggregateValue::Total(-3)),
                Value::Record(
                    "axis-aggregate",
                    vec![
                        ("metric", Field::Word("total")),
                        ("value", Field::Number(-3)),
                    ],
                ),
            ),
            (PathQueryRun::SumBudget(walked()), sums("sum-budget")),
        ];
        for (answer, expected) in answers {
            assert_eq!(runner.run(answer).expect("a record"), expected);
        }
        let mut remarks = Vec::new();
        assert_eq!(
            runner
                .value(Applied::Sums(walked()), &mut remarks)
                .expect("a record"),
            sums("sums")
        );
        assert_eq!(
            runner
                .value(
                    Applied::Reclaimed(grovedb::PendingPrefixDropsReport {
                        reclaimed_records: 1,
                        skipped_live: 2,
                        unresolved_records: 3,
                        first_unresolved_error: Some("a storage fault".to_string()),
                    }),
                    &mut remarks
                )
                .expect("a record"),
            Value::Record(
                "reclaimed",
                vec![
                    ("reclaimed", Field::Number(1)),
                    ("skipped-live", Field::Number(2)),
                    ("unresolved", Field::Number(3)),
                ],
            )
        );
        assert_eq!(remarks, ["first unresolved: a storage fault"]);
    }

    #[test]
    fn the_operations_of_the_later_workloads_are_observed() {
        let grove_version = GroveVersion::latest();
        let dir = tempfile::tempdir().expect("temp dir");
        let db = GroveDb::open(dir.path()).expect("open");
        let mut runner = Runner::new(&db, grove_version).expect("runner");
        let empty_root = runner.committed_root;
        let returned = |observed: Observed| match observed.step.outcome {
            Outcome::Returned(value) => value,
            Outcome::Failed(error) => panic!("failed: {error} {:?}", observed.remarks),
        };

        let put = runner
            .step(&Op::Write(Write::PutAux {
                key: b"aux-a".to_vec(),
                value: b"one".to_vec(),
            }))
            .expect("step");
        assert_eq!(
            put.step.root_hash,
            Some(empty_root),
            "no tree commits to it"
        );
        assert_eq!(returned(put), Value::Nothing);
        runner.step(&Op::Commit).expect("step");
        let aux = |key: &[u8]| Op::Read(Read::GetAux { key: key.to_vec() });
        let stored = runner.step(&aux(b"aux-a")).expect("step");
        assert_eq!((stored.step.root_hash, &stored.step.proof), (None, &None));
        assert_eq!(
            returned(stored),
            Value::Record("aux", vec![("value", Field::Bytes(b"one".to_vec()))])
        );
        assert_eq!(
            returned(runner.step(&aux(b"missing")).expect("step")),
            Value::Record("aux", vec![("value", Field::Absent)])
        );
        assert_eq!(
            returned(
                runner
                    .step(&Op::Read(Read::AuxByPrefix {
                        prefix: b"aux-".to_vec()
                    }))
                    .expect("step")
            ),
            Value::Record(
                "aux-entries",
                vec![(
                    "entries",
                    Field::List(vec![vec![
                        Field::Bytes(b"aux-a".to_vec()),
                        Field::Bytes(b"one".to_vec())
                    ]])
                )]
            )
        );

        for op in [
            insert(&[], b"tree", Element::empty_tree()),
            insert(&[b"tree"], b"sums", Element::empty_sum_tree()),
            insert(&[b"tree", b"sums"], b"a", Element::new_sum_item(5)),
            Op::Commit,
        ] {
            runner.step(&op).expect("step");
        }
        let before = runner.committed_root;
        let partial = runner
            .step(&Op::PartialBatch(crate::workload::PartialBatch {
                ops: vec![BatchOp::InsertOnly {
                    path: vec![b"tree".to_vec(), b"sums".to_vec()],
                    key: b"b".to_vec(),
                    element: Element::new_sum_item(7),
                }],
                add_on: vec![BatchOp::InsertOnly {
                    path: vec![b"tree".to_vec()],
                    key: b"note".to_vec(),
                    element: Element::new_item(b"added once the first segment paused".to_vec()),
                }],
                pause_height: None,
            }))
            .expect("step");
        assert_ne!(partial.step.root_hash, Some(before));
        let whole = partial.step.cost.clone();
        let Value::Record("written-in-segments", fields) = returned(partial) else {
            panic!("a partial batch returns the cost of its first segment");
        };
        let [("first-segment", Field::Cost(first_segment))] = fields.as_slice() else {
            panic!("{fields:?}");
        };
        assert!(first_segment.seek_count > 0);
        assert!(first_segment.seek_count < whole.seek_count);
        runner.step(&Op::Commit).expect("step");

        let in_sums: &[&[u8]] = &[b"tree", b"sums"];
        let walk = runner
            .step(&Op::Read(sum_walk(in_sums, 100)))
            .expect("step");
        assert_eq!(walk.step.proof, None);
        assert_eq!(
            returned(walk),
            Value::Record(
                "sums",
                vec![
                    (
                        "sums",
                        Field::List(vec![
                            vec![Field::Bytes(b"a".to_vec()), Field::Number(5)],
                            vec![Field::Bytes(b"b".to_vec()), Field::Number(7)],
                        ])
                    ),
                    ("hard-limit-reached", Field::Flag(false)),
                    ("scanned", Field::Number(2)),
                ]
            )
        );
        let reads = [
            Read::Rows {
                query: whole_tree(in_sums),
                raw: true,
            },
            Read::PathQuery {
                query: whole_tree(in_sums),
                proven: false,
            },
            Read::PathQuery {
                query: whole_tree(in_sums),
                proven: true,
            },
        ];
        for (read, proved) in reads.into_iter().zip([false, false, true]) {
            let rows = runner.step(&Op::Read(read)).expect("step");
            assert_eq!(rows.step.root_hash, None);
            match &rows.step.proof {
                Some(proof) => {
                    assert!(proved);
                    assert!(matches!(
                        proof.proved,
                        Proved::Bytes(_, Verification::Verified)
                    ));
                }
                None => assert!(!proved),
            }
            let Value::Record("elements", fields) = returned(rows) else {
                panic!("a path query that selects keys returns elements");
            };
            let [("rows", Field::Rows(rows)), ("skipped", Field::Number(0))] = fields.as_slice()
            else {
                panic!("{fields:?}");
            };
            assert_eq!(rows.len(), 2);
        }

        runner
            .step(&Op::Write(Write::DropFlat {
                path: vec![b"tree".to_vec()],
                key: b"sums".to_vec(),
            }))
            .expect("step");
        runner.step(&Op::Commit).expect("step");
        let committed = runner.committed_root;
        let flush = runner.step(&Op::FlushPendingDrops).expect("step");
        assert_eq!(flush.step.root_hash, Some(committed));
        assert_eq!(flush.step.proof, None);
        assert_eq!(
            returned(flush),
            Value::Record(
                "reclaimed",
                vec![
                    ("reclaimed", Field::Number(1)),
                    ("skipped-live", Field::Number(0)),
                    ("unresolved", Field::Number(0)),
                ]
            )
        );
        let gone = runner.step(&query(&[b"tree"], true)).expect("step");
        let Value::Rows(rows) = returned(gone) else {
            panic!("a query returns rows");
        };
        assert_eq!(rows.len(), 1, "the note stays and the dropped tree is gone");
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
