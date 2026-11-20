//! The proofs of the corpus, made by Merk on both engines (MERK-06).
//!
//! GroveDB does not run on LigninDB yet, so a step of a trace that proves
//! is proven by GroveDB on RocksDB alone. What GroveDB asks of Merk for
//! such a proof is asked here of a Merk on each engine, over the same
//! nodes: the Merk at each path the step names is opened on RocksDB, as
//! the trace's transaction reads it, and on a handle of the LigninDB
//! transaction that the replay has handed every write so far.
//!
//! - For a path query: on the Merk of each subtree above the query's path,
//!   the proof of the key that leads on, which is how GroveDB proves the
//!   layers of a path; and on the Merk at the path, the proof of the
//!   query's items with its limit and its direction. An item that asks for
//!   an aggregate over a range is also proven as that aggregate.
//! - For a trunk and for a branch chunk: the trunk or the branch proof of
//!   the Merk at the path.
//! - For chunks of a Merk by their ids (MERK-07): one producer is given
//!   each id the step names, in their order, or the id of the chunk at the
//!   root where the step names none. The call answers with every chunk
//!   and the id the producer gives as next after each, or with the first
//!   refusal.
//!
//! Every answer is compared as its encoded bytes, or as its error, and
//! every call must cost on LigninDB what it costs on RocksDB, the open of
//! the Merk included. The proofs GroveDB itself puts together on LigninDB,
//! against the proof bytes the corpus recorded, are GROVE-06's.

use grovedb::{Element, GroveDb, PathQuery, Transaction};
use grovedb_costs::OperationCost;
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    proofs::{encode_into, query::QueryItem, Op as ProofOp},
    store::{
        lignin::{LigninNodeStore, LigninSubtree, LigninWriter},
        NodeStore,
    },
    Merk, TreeType,
};
use grovedb_storage::Storage;
use grovedb_version::version::GroveVersion;
use lignin_testkit::{
    layout_sim::{tree::Merk as MerkModel, Grove},
    workload::op::{ChunkIds, Op, Prove, Read},
};

use super::NO_VALUE_DEFINED_COST;

/// What the proofs of a replay came to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Proved {
    /// The steps of the traces that prove.
    pub steps: u64,
    /// The calls of a Merk that were made on both engines, the opens that
    /// failed on both among them.
    pub calls: u64,
    /// The calls that were not made: no Merk is at the path, or what is
    /// there is no tree of a type that opens one.
    pub skipped: u64,
    /// Those that gave a proof, and the bytes of the proofs.
    pub proofs: u64,
    pub bytes: u64,
    /// Those among them that gave chunks of a Merk, and their bytes.
    pub chunk_calls: u64,
    pub chunk_bytes: u64,
}

impl Proved {
    pub fn add(&mut self, other: Proved) {
        self.steps += other.steps;
        self.calls += other.calls;
        self.skipped += other.skipped;
        self.proofs += other.proofs;
        self.bytes += other.bytes;
        self.chunk_calls += other.chunk_calls;
        self.chunk_bytes += other.chunk_bytes;
    }
}

/// One call of a Merk that makes a proof.
#[derive(Debug)]
enum Call {
    Query(Vec<QueryItem>, Option<u16>, bool),
    Count(QueryItem),
    Sum(QueryItem),
    CountAndSum(QueryItem),
    Trunk(u8, Option<u8>),
    Branch(Vec<u8>, u8),
    /// The chunks one producer of the Merk gives for these chunk ids.
    Chunks(Vec<Vec<u8>>),
}

/// What a path query asks of the Merks on its way and of the Merk at its
/// path.
fn of_path_query(query: &PathQuery, calls: &mut Vec<(Vec<Vec<u8>>, Call)>) {
    for (depth, key) in query.path.iter().enumerate() {
        let layer = Call::Query(vec![QueryItem::Key(key.clone())], None, true);
        calls.push((query.path[..depth].to_vec(), layer));
    }
    let asked = &query.query.query;
    let at = |call: Call| (query.path.clone(), call);
    calls.push(at(Call::Query(
        asked.items.clone(),
        query.query.limit,
        asked.left_to_right,
    )));
    for item in &asked.items {
        match item {
            QueryItem::AggregateCountOnRange(range) => {
                calls.push(at(Call::Count((**range).clone())))
            }
            QueryItem::AggregateSumOnRange(range) => calls.push(at(Call::Sum((**range).clone()))),
            QueryItem::AggregateCountAndSumOnRange(range) => {
                calls.push(at(Call::CountAndSum((**range).clone())))
            }
            _ => {}
        }
    }
}

/// The calls of a Merk that the step `op` of a trace proves with, each
/// with the path of its Merk. None for a step that proves nothing.
fn calls(op: &Op) -> Vec<(Vec<Vec<u8>>, Call)> {
    let mut calls = Vec::new();
    let Op::Read(read) = op else {
        return calls;
    };
    match read {
        Read::Query(query) => of_path_query(&query.to_path_query(), &mut calls),
        Read::PathQuery {
            query,
            proven: true,
        } => of_path_query(query, &mut calls),
        Read::Prove(Prove::PathQuery(query)) => of_path_query(query, &mut calls),
        Read::Prove(Prove::Many(queries)) => {
            for query in queries {
                of_path_query(query, &mut calls);
            }
        }
        Read::Prove(Prove::TrunkChunk {
            path,
            max_depth,
            min_depth,
        }) => calls.push((path.clone(), Call::Trunk(*max_depth, *min_depth))),
        Read::Prove(Prove::BranchChunk { path, key, depth }) => {
            calls.push((path.clone(), Call::Branch(key.clone(), *depth)))
        }
        Read::Prove(Prove::Chunk {
            path,
            ids: ChunkIds::Merk(ids),
        }) => {
            // No id at all asks for the chunk at the root.
            let ids = match ids.is_empty() {
                true => vec![Vec::new()],
                false => ids.clone(),
            };
            calls.push((path.clone(), Call::Chunks(ids)))
        }
        _ => {}
    }
    calls
}

fn encoded<'o>(ops: impl IntoIterator<Item = &'o ProofOp>) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode_into(ops.into_iter(), &mut bytes);
    bytes
}

/// Makes `call` on `merk`: the bytes of the proof with what else the call
/// returned, or its error, and what it was charged.
fn answer<'db, S: NodeStore<'db>>(
    merk: &Merk<S>,
    call: &Call,
    grove_version: &GroveVersion,
) -> (Result<(Vec<u8>, String), String>, OperationCost) {
    let mut cost = OperationCost::default();
    let refused = |error: grovedb_merk::Error| format!("{error:?}");
    let answer = match call {
        Call::Query(items, limit, left_to_right) => merk
            .prove_unchecked_query_items(items, *limit, *left_to_right, grove_version)
            .unwrap_add_cost(&mut cost)
            .map(|(proof, limit)| (encoded(&proof), format!("{limit:?}")))
            .map_err(refused),
        Call::Count(range) => merk
            .prove_aggregate_count_on_range(range, grove_version)
            .unwrap_add_cost(&mut cost)
            .map(|(proof, count)| (encoded(&proof), format!("{count}")))
            .map_err(refused),
        Call::Sum(range) => merk
            .prove_aggregate_sum_on_range(range, grove_version)
            .unwrap_add_cost(&mut cost)
            .map(|(proof, sum)| (encoded(&proof), format!("{sum}")))
            .map_err(refused),
        Call::CountAndSum(range) => merk
            .prove_aggregate_count_and_sum_on_range(range, grove_version)
            .unwrap_add_cost(&mut cost)
            .map(|(proof, count, sum)| (encoded(&proof), format!("{count} {sum}")))
            .map_err(refused),
        Call::Trunk(max_depth, min_depth) => merk
            .trunk_query(*max_depth, *min_depth, grove_version)
            .unwrap_add_cost(&mut cost)
            .map(|trunk| {
                let depths = format!("{:?} {}", trunk.chunk_depths, trunk.tree_depth);
                (encoded(&trunk.proof), depths)
            })
            .map_err(refused),
        Call::Branch(key, depth) => merk
            .branch_query(key, *depth, grove_version)
            .unwrap_add_cost(&mut cost)
            .map(|branch| {
                let root = format!("{:?} {}", branch.branch_root_hash, branch.returned_depth);
                (encoded(&branch.proof), root)
            })
            .map_err(refused),
        // A producer reports no cost for a chunk.
        Call::Chunks(ids) => merk
            .chunks()
            .and_then(|mut producer| {
                let (mut bytes, mut following) = (Vec::new(), Vec::new());
                for id in ids {
                    let (chunk, next) = producer.chunk(id, grove_version)?;
                    bytes.extend(encoded(&chunk));
                    following.push(next);
                }
                Ok((bytes, format!("{following:?}")))
            })
            .map_err(refused),
    };
    (answer, cost)
}

/// Opens the Merk of `model` on a handle, and returns it with what the
/// open was charged, or the error of the open.
fn open<'db, S: NodeStore<'db>>(
    store: S,
    root_key: Option<Vec<u8>>,
    tree_type: TreeType,
    grove_version: &GroveVersion,
) -> (Result<Merk<S>, String>, OperationCost) {
    let opened = Merk::open_layered_with_root_key(
        store,
        root_key,
        tree_type,
        NO_VALUE_DEFINED_COST,
        grove_version,
    );
    (
        opened.value.map_err(|error| format!("{error:?}")),
        opened.cost,
    )
}

/// The tree type of the Merk `model`: that of the element that hosts it,
/// read from RocksDB. `None` for a Merk whose host is no tree with a Merk
/// of that type to open.
pub fn tree_type(
    db: &GroveDb,
    transaction: &Transaction,
    grove: &Grove,
    model: &MerkModel,
    grove_version: &GroveVersion,
) -> Option<TreeType> {
    let Some(host) = model.host.map(|host| grove.node(host)) else {
        return Some(TreeType::NormalTree);
    };
    let context = db
        .raw_storage()
        .get_transactional_storage_context_by_subtree_prefix(
            grove.merk(host.merk).prefix,
            None,
            transaction,
        )
        .unwrap();
    let node = NodeStore::get_node(&context, &host.key, NO_VALUE_DEFINED_COST, grove_version);
    let node = node.unwrap().expect("the host is read")?;
    let element = Element::deserialize(node.value_as_slice(), grove_version).ok()?;
    element.tree_type()
}

/// Makes the calls of a Merk that the step `op` proves with, on RocksDB
/// and on LigninDB, and compares what each answered and what it cost.
pub fn compare(
    db: &GroveDb,
    transaction: &Transaction,
    writer: &LigninWriter<'_, '_>,
    grove: &Grove,
    op: &Op,
    grove_version: &GroveVersion,
) -> Proved {
    let calls = calls(op);
    let mut proved = Proved {
        steps: u64::from(!calls.is_empty()),
        ..Proved::default()
    };
    for (path, call) in &calls {
        let found = grove
            .merks()
            .find(|(_, merk)| merk.live && merk.axis.is_none() && merk.path.as_ref() == Some(path));
        let Some((_, model)) = found else {
            proved.skipped += 1;
            continue;
        };
        let Some(tree_type) = tree_type(db, transaction, grove, model, grove_version) else {
            proved.skipped += 1;
            continue;
        };
        let root_key = model.root.map(|root| grove.node(root).key.to_vec());

        let context = db
            .raw_storage()
            .get_transactional_storage_context_by_subtree_prefix(model.prefix, None, transaction)
            .unwrap();
        let (expected, expected_open) = open(context, root_key.clone(), tree_type, grove_version);
        let store = LigninNodeStore::deferred(writer, LigninSubtree::alone(model.prefix), None);
        let (opened, open_cost) = open(store, root_key, tree_type, grove_version);
        assert_eq!(
            open_cost, expected_open,
            "what the open of the Merk {path:?} is charged"
        );
        let (merk, expected) = match (opened, expected) {
            (Ok(merk), Ok(expected)) => (merk, expected),
            (Err(error), Err(expected)) => {
                assert_eq!(error, expected, "the open of the Merk {path:?}");
                proved.calls += 1;
                continue;
            }
            (opened, expected) => panic!(
                "the Merk {path:?} opens on one engine only: {:?}, and on RocksDB {:?}",
                opened.err(),
                expected.err()
            ),
        };

        let (found, cost) = answer(&merk, call, grove_version);
        let (expected, expected_cost) = answer(&expected, call, grove_version);
        assert_eq!(found, expected, "the proof {call:?} of the Merk {path:?}");
        assert_eq!(
            cost, expected_cost,
            "what the proof {call:?} of the Merk {path:?} is charged"
        );
        proved.calls += 1;
        if let Ok((proof, _)) = &found {
            proved.proofs += 1;
            proved.bytes += proof.len() as u64;
            if matches!(call, Call::Chunks(_)) {
                proved.chunk_calls += 1;
                proved.chunk_bytes += proof.len() as u64;
            }
        }
    }
    proved
}
