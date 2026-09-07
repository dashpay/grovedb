//! GroveDB debugging support module.
//!
//! HTTP responses preserve the pre-backward-references element shapes by
//! default, including for the bundled GroveDBG v1.2.0 UI. Clients that
//! understand the dedicated backward-references variants can opt in on
//! each request with `x-grovedbg-backward-references: true`.
//! In that format, stored nodes report `Some(count)` while proof elements
//! report `None`: proofs authenticate the referrer-list hash but omit the
//! list itself.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    sync::{Arc, Weak},
    time::{Duration, Instant, SystemTime},
};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use grovedb_merk::{
    debugger::NodeDbg,
    proofs::{Decoder, Node, Op},
    tree::value_hash,
    TreeFeatureType,
};
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;
use grovedbg_types::{
    DropSessionRequest, MerkProofNode, MerkProofOp, NewSessionResponse, NodeFetchRequest,
    NodeUpdate, Path, PathQuery, Query, QueryItem, SessionId, SizedQuery, SubqueryBranch,
    WithSession,
};
use indexmap::IndexMap;
use tempfile::tempdir;
use tokio::{
    net::ToSocketAddrs,
    select,
    sync::{RwLock, RwLockReadGuard},
    time::sleep,
};
use tokio_util::sync::CancellationToken;
use tower_http::services::ServeDir;

use crate::{
    operations::proof::{GroveDBProof, LayerProof, MerkOnlyLayerProof, ProofBytes, ProveOptions},
    query_result_type::{QueryResultElement, QueryResultElements, QueryResultType},
    reference_path::ReferencePathType,
    GroveDb,
};

const GROVEDBG_ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grovedbg.zip"));

const SESSION_TIMEOUT: Duration = Duration::from_secs(60 * 10);

#[derive(Clone, Copy)]
enum BackwardReferencesFormat {
    Legacy,
    Extended,
}

impl From<&HeaderMap> for BackwardReferencesFormat {
    fn from(headers: &HeaderMap) -> Self {
        if headers
            .get("x-grovedbg-backward-references")
            .is_some_and(|value| value == "true")
        {
            Self::Extended
        } else {
            Self::Legacy
        }
    }
}

impl BackwardReferencesFormat {
    fn element(self, element: grovedbg_types::Element) -> grovedbg_types::Element {
        use grovedbg_types::Element;

        if matches!(self, Self::Extended) {
            return element;
        }
        // Preserve precisely the mappings used before the dedicated
        // variants were introduced. Other element families are unchanged.
        match element {
            Element::ItemWithBackwardsReferences {
                value,
                element_flags,
                ..
            } => Element::Item {
                value,
                element_flags,
            },
            Element::SumItemWithBackwardsReferences {
                value,
                element_flags,
                ..
            } => Element::SumItem {
                value,
                element_flags,
            },
            Element::ItemWithSumItemWithBackwardsReferences {
                value,
                sum_item_value,
                element_flags,
                ..
            } => Element::ItemWithSumItem {
                value,
                sum_item_value,
                element_flags,
            },
            Element::BidirectionalReference { reference, .. } => Element::Reference(reference),
            element => element,
        }
    }

    fn node_update(self, mut node: NodeUpdate) -> NodeUpdate {
        node.element = self.element(node.element);
        node
    }

    fn proof(self, mut proof: grovedbg_types::Proof) -> grovedbg_types::Proof {
        if matches!(self, Self::Legacy) {
            proof.root_layer = self.proof_layer(proof.root_layer);
        }
        proof
    }

    fn proof_layer(self, mut layer: grovedbg_types::ProofLayer) -> grovedbg_types::ProofLayer {
        layer.merk_proof = layer
            .merk_proof
            .into_iter()
            .map(|op| match op {
                MerkProofOp::Push(node) => MerkProofOp::Push(self.proof_node(node)),
                MerkProofOp::PushInverted(node) => MerkProofOp::PushInverted(self.proof_node(node)),
                op => op,
            })
            .collect();
        layer.lower_layers = layer
            .lower_layers
            .into_iter()
            .map(|(key, layer)| (key, self.proof_layer(layer)))
            .collect();
        layer
    }

    fn proof_node(self, node: MerkProofNode) -> MerkProofNode {
        match node {
            MerkProofNode::KV(key, element) => MerkProofNode::KV(key, self.element(element)),
            MerkProofNode::KVValueHash(key, element, hash) => {
                MerkProofNode::KVValueHash(key, self.element(element), hash)
            }
            MerkProofNode::KVValueHashFeatureType(key, element, hash, feature) => {
                MerkProofNode::KVValueHashFeatureType(key, self.element(element), hash, feature)
            }
            MerkProofNode::KVRefValueHash(key, element, hash) => {
                MerkProofNode::KVRefValueHash(key, self.element(element), hash)
            }
            node => node,
        }
    }
}

pub(super) fn start_visualizer<A>(grovedb: Weak<GroveDb>, addr: A)
where
    A: ToSocketAddrs + Send + 'static,
{
    std::thread::spawn(move || {
        let grovedbg_tmp =
            tempfile::tempdir().expect("cannot create tempdir for grovedbg contents");
        let grovedbg_zip = grovedbg_tmp.path().join("grovedbg.zip");
        let grovedbg_www = grovedbg_tmp.path().join("grovedbg_www");

        fs::write(&grovedbg_zip, GROVEDBG_ZIP).expect("cannot crate grovedbg.zip");
        zip_extensions::inflate::zip_extract::zip_extract(&grovedbg_zip, &grovedbg_www)
            .expect("cannot extract grovedbg contents");

        let cancellation_token = CancellationToken::new();

        let state: AppState = AppState {
            cancellation_token: cancellation_token.clone(),
            grovedb,
            sessions: Default::default(),
        };

        let app = Router::new()
            .route("/new_session", post(new_session))
            .route("/drop_session", post(drop_session))
            .route("/fetch_node", post(fetch_node))
            .route("/fetch_root_node", post(fetch_root_node))
            .route("/prove_path_query", post(prove_path_query))
            .route("/fetch_with_path_query", post(fetch_with_path_query))
            .fallback_service(ServeDir::new(grovedbg_www))
            .with_state(state.clone());

        let rt = tokio::runtime::Runtime::new().unwrap();

        let cloned_cancellation_token = cancellation_token.clone();
        rt.spawn(async move {
            loop {
                select! {
                    _ = cloned_cancellation_token.cancelled() => break,
                    _ = sleep(Duration::from_secs(10)) => {
                        let now = Instant::now();
                        let mut lock = state.sessions.write().await;
                        let to_delete: Vec<SessionId> = lock.iter().filter_map(
                            |(id, session)|
                                (session.last_access < now - SESSION_TIMEOUT).then_some(*id)
                        ).collect();

                        to_delete.into_iter().for_each(|id| { lock.remove(&id); });
                    }
                }
            }
        });

        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .expect("can't bind visualizer port");
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    cancellation_token.cancelled().await;
                })
                .await
                .unwrap()
        });
    });
}

#[derive(Clone)]
struct AppState {
    cancellation_token: CancellationToken,
    grovedb: Weak<GroveDb>,
    sessions: Arc<RwLock<HashMap<SessionId, Session>>>,
}

impl AppState {
    fn verify_running(&self) -> Result<(), AppError> {
        if self.grovedb.strong_count() == 0 {
            self.cancellation_token.cancel();
            Err(AppError::Closed)
        } else {
            Ok(())
        }
    }

    async fn new_session(&self) -> Result<SessionId, AppError> {
        let grovedb = self.grovedb.upgrade().ok_or(AppError::Closed)?;
        let id = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();
        self.sessions
            .write()
            .await
            .insert(id, Session::new(&grovedb)?);

        Ok(id)
    }

    async fn drop_session(&self, id: SessionId) {
        self.sessions.write().await.remove(&id);
    }

    async fn get_checkpointed_grovedb(
        &self,
        id: SessionId,
    ) -> Result<RwLockReadGuard<'_, GroveDb>, AppError> {
        self.verify_running()?;
        let mut lock = self.sessions.write().await;
        if let Some(session) = lock.get_mut(&id) {
            session.last_access = Instant::now();
            Ok(RwLockReadGuard::map(lock.downgrade(), |l| {
                &l.get(&id)
                    .as_ref()
                    .expect("checked above")
                    .checkpointed_grovedb
            }))
        } else {
            Err(AppError::NoSession)
        }
    }
}

struct Session {
    last_access: Instant,
    _tempdir: tempfile::TempDir,
    checkpointed_grovedb: GroveDb,
}

impl Session {
    fn new(grovedb: &GroveDb) -> Result<Self, AppError> {
        let tempdir = tempdir().map_err(|e| AppError::Any(e.to_string()))?;
        let path = tempdir.path().join("grovedbg_session");
        grovedb
            .create_checkpoint(&path)
            .map_err(|e| AppError::Any(e.to_string()))?;
        let checkpointed_grovedb = GroveDb::open(path).map_err(|e| AppError::Any(e.to_string()))?;
        Ok(Session {
            last_access: Instant::now(),
            _tempdir: tempdir,
            checkpointed_grovedb,
        })
    }
}

#[derive(Debug)]
enum AppError {
    Closed,
    NoSession,
    Any(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        match self {
            AppError::Closed => {
                (StatusCode::SERVICE_UNAVAILABLE, "GroveDB is closed").into_response()
            }
            AppError::NoSession => {
                (StatusCode::UNAUTHORIZED, "No session with this id").into_response()
            }
            AppError::Any(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    }
}

impl<E: std::error::Error> From<E> for AppError {
    fn from(err: E) -> Self {
        Self::Any(err.to_string())
    }
}

async fn new_session(State(state): State<AppState>) -> Result<Json<NewSessionResponse>, AppError> {
    Ok(Json(NewSessionResponse {
        session_id: state.new_session().await?,
    }))
}

async fn drop_session(
    State(state): State<AppState>,
    Json(DropSessionRequest { session_id }): Json<DropSessionRequest>,
) {
    state.drop_session(session_id).await;
}

async fn fetch_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(WithSession {
        session_id,
        request: NodeFetchRequest { path, key },
    }): Json<WithSession<NodeFetchRequest>>,
) -> Result<Json<Option<NodeUpdate>>, AppError> {
    let db = state.get_checkpointed_grovedb(session_id).await?;
    let transaction = db.start_transaction();

    let merk = db
        .open_transactional_merk_at_path(
            path.as_slice().into(),
            &transaction,
            None,
            GroveVersion::latest(),
        )
        .unwrap()?;
    let node = merk.get_node_dbg(&key)?;

    if let Some(node) = node {
        let node_update: NodeUpdate = node_to_update(path, node)?;
        Ok(Json(Some(
            BackwardReferencesFormat::from(&headers).node_update(node_update),
        )))
    } else {
        Ok(None.into())
    }
}

async fn fetch_root_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(WithSession {
        session_id,
        request: (),
    }): Json<WithSession<()>>,
) -> Result<Json<Option<NodeUpdate>>, AppError> {
    let db = state.get_checkpointed_grovedb(session_id).await?;
    let transaction = db.start_transaction();

    let merk = db
        .open_transactional_merk_at_path(
            SubtreePath::empty(),
            &transaction,
            None,
            GroveVersion::latest(),
        )
        .unwrap()?;

    let node = merk.get_root_node_dbg()?;

    if let Some(node) = node {
        let node_update: NodeUpdate = node_to_update(Vec::new(), node)?;
        Ok(Json(Some(
            BackwardReferencesFormat::from(&headers).node_update(node_update),
        )))
    } else {
        Ok(None.into())
    }
}

async fn prove_path_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(WithSession {
        session_id,
        request: json_path_query,
    }): Json<WithSession<PathQuery>>,
) -> Result<Json<grovedbg_types::Proof>, AppError> {
    let db = state.get_checkpointed_grovedb(session_id).await?;

    let path_query = path_query_to_grovedb(json_path_query);

    let grovedb_proof = db
        .prove_query_non_serialized(&path_query, None, GroveVersion::latest())
        .unwrap()?;
    Ok(Json(
        BackwardReferencesFormat::from(&headers).proof(proof_to_grovedbg(grovedb_proof)?),
    ))
}

async fn fetch_with_path_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(WithSession {
        session_id,
        request: json_path_query,
    }): Json<WithSession<PathQuery>>,
) -> Result<Json<Vec<grovedbg_types::NodeUpdate>>, AppError> {
    let db = state.get_checkpointed_grovedb(session_id).await?;

    let path_query = path_query_to_grovedb(json_path_query);

    let grovedb_query_result = db
        .query_raw(
            &path_query,
            false,
            true,
            false,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            GroveVersion::latest(),
        )
        .unwrap()?
        .0;
    let format = BackwardReferencesFormat::from(&headers);
    Ok(Json(
        query_result_to_grovedbg(&db, grovedb_query_result)?
            .into_iter()
            .map(|node| format.node_update(node))
            .collect(),
    ))
}

fn query_result_to_grovedbg(
    db: &GroveDb,
    query_result: QueryResultElements,
) -> Result<Vec<NodeUpdate>, crate::Error> {
    let mut result = Vec::new();
    let transaction = db.start_transaction();

    let mut last_merk: Option<(Vec<Vec<u8>>, grovedb_merk::Merk<_>)> = None;

    for qr in query_result.elements.into_iter() {
        if let QueryResultElement::PathKeyElementTrioResultItem((path, key, _)) = qr {
            let merk: &grovedb_merk::Merk<_> = match &mut last_merk {
                Some((last_merk_path, last_merk)) if last_merk_path == &path => last_merk,
                _ => {
                    last_merk = Some((
                        path.clone(),
                        db.open_transactional_merk_at_path(
                            path.as_slice().into(),
                            &transaction,
                            None,
                            GroveVersion::latest(),
                        )
                        .unwrap()?,
                    ));
                    &last_merk.as_ref().unwrap().1
                }
            };

            if let Some(node) = merk.get_node_dbg(&key)? {
                result.push(node_to_update(path, node)?);
            }
        }
    }
    Ok(result)
}

/// Print a proof to stdout in the GroveDBG wire encoding
/// (bincode-over-serde, standard configuration) as a hex string,
/// framed by marker lines so it can be grepped out of longer logs.
/// Uses the current `grovedbg_types::Proof` schema, including the dedicated
/// backward-references variants with unknown referrer counts. A consumer
/// must support that schema and bincode-over-serde imports; the bundled
/// GroveDBG v1.2.0 UI does not support importing these dumps.
/// On conversion or encoding failure the error goes to stderr instead.
pub fn dump_proof_grovedbg_stdout(proof: GroveDBProof) {
    let grovedbg_proof = proof_to_grovedbg(proof).map_err(|e| e.to_string());
    let encoded = grovedbg_proof.and_then(|p| {
        bincode::serde::encode_to_vec(p, bincode::config::standard()).map_err(|e| e.to_string())
    });

    match encoded {
        Ok(p) => {
            println!("==========GroveDBG proof dump starts after this line==========");
            println!("{}", hex::encode(p));
            println!("==========GroveDBG proof dump ends before this line===========");
        }
        Err(e) => {
            eprintln!("Unable to dump proof to grovedbg: {e}");
        }
    }
}

fn proof_to_grovedbg(proof: GroveDBProof) -> Result<grovedbg_types::Proof, crate::Error> {
    match proof {
        GroveDBProof::V0(p) => Ok(grovedbg_types::Proof {
            root_layer: proof_layer_to_grovedbg(p.root_layer)?,
            prove_options: prove_options_to_grovedbg(p.prove_options),
        }),
        GroveDBProof::V1(p) => Ok(grovedbg_types::Proof {
            root_layer: v1_proof_layer_to_grovedbg(p.root_layer)?,
            // V1 proofs no longer embed ProveOptions; use default for debugger
            prove_options: prove_options_to_grovedbg(ProveOptions::default()),
        }),
    }
}

fn proof_layer_to_grovedbg(
    proof_layer: MerkOnlyLayerProof,
) -> Result<grovedbg_types::ProofLayer, crate::Error> {
    Ok(grovedbg_types::ProofLayer {
        merk_proof: merk_proof_to_grovedbg(&proof_layer.merk_proof)?,
        lower_layers: proof_layer
            .lower_layers
            .into_iter()
            .map(|(k, v)| proof_layer_to_grovedbg(v).map(|layer| (k, layer)))
            .collect::<Result<BTreeMap<Vec<u8>, grovedbg_types::ProofLayer>, crate::Error>>()?,
    })
}

fn v1_proof_layer_to_grovedbg(
    proof_layer: LayerProof,
) -> Result<grovedbg_types::ProofLayer, crate::Error> {
    let merk_bytes = match &proof_layer.merk_proof {
        ProofBytes::Merk(bytes) => bytes.as_slice(),
        // Non-Merk proofs (MMR, BulkAppendTree, DenseTree) cannot be
        // decoded into Merk proof ops; return an empty op list.
        _ => &[],
    };
    Ok(grovedbg_types::ProofLayer {
        merk_proof: merk_proof_to_grovedbg(merk_bytes)?,
        lower_layers: proof_layer
            .lower_layers
            .into_iter()
            .map(|(k, v)| v1_proof_layer_to_grovedbg(v).map(|layer| (k, layer)))
            .collect::<Result<BTreeMap<Vec<u8>, grovedbg_types::ProofLayer>, crate::Error>>()?,
    })
}

fn merk_proof_to_grovedbg(merk_proof: &[u8]) -> Result<Vec<MerkProofOp>, crate::Error> {
    let decoder = Decoder::new(merk_proof);
    decoder
        .map(|op_result| {
            op_result
                .map_err(crate::Error::MerkError)
                .and_then(merk_proof_op_to_grovedbg)
        })
        .collect::<Result<Vec<MerkProofOp>, _>>()
}
fn merk_proof_op_to_grovedbg(op: Op) -> Result<MerkProofOp, crate::Error> {
    Ok(match op {
        Op::Push(node) => MerkProofOp::Push(merk_proof_node_to_grovedbg(node)?),
        Op::PushInverted(node) => MerkProofOp::PushInverted(merk_proof_node_to_grovedbg(node)?),
        Op::Parent => MerkProofOp::Parent,
        Op::Child => MerkProofOp::Child,
        Op::ParentInverted => MerkProofOp::ParentInverted,
        Op::ChildInverted => MerkProofOp::ChildInverted,
    })
}

fn merk_proof_node_to_grovedbg(node: Node) -> Result<MerkProofNode, crate::Error> {
    Ok(match node {
        Node::Hash(hash) => MerkProofNode::Hash(hash),
        Node::KVHash(hash) => MerkProofNode::KVHash(hash),
        // grovedbg has no dedicated variant; show as a KVValueHash-style
        // node with the backrefs hash in the hash slot.
        Node::KVBackwardsReferencesValueHash(key, value, backrefs_hash) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KVValueHash(key, proof_element_to_grovedbg(element), backrefs_hash)
        }
        Node::KVDigest(key, hash) => MerkProofNode::KVDigest(key, hash),
        Node::KVDigestCount(key, hash, count) => {
            // KVDigestCount is like KVDigest but with count for ProvableCountTree
            // Use KVValueHashFeatureType for debug display since grovedbg_types may not
            // have KVDigestCount
            MerkProofNode::KVValueHashFeatureType(
                key,
                grovedbg_types::Element::Item {
                    value: vec![],
                    element_flags: None,
                },
                hash,
                grovedbg_types::TreeFeatureType::ProvableCountedMerkNode(count),
            )
        }
        Node::KV(key, value) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KV(key, proof_element_to_grovedbg(element))
        }
        Node::KVValueHash(key, value, hash) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KVValueHash(key, proof_element_to_grovedbg(element), hash)
        }
        Node::KVValueHashFeatureType(key, value, hash, feature_type) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            let node_feature_type = match feature_type {
                TreeFeatureType::BasicMerkNode => grovedbg_types::TreeFeatureType::BasicMerkNode,
                TreeFeatureType::SummedMerkNode(sum) => {
                    grovedbg_types::TreeFeatureType::SummedMerkNode(sum)
                }
                TreeFeatureType::BigSummedMerkNode(sum) => {
                    grovedbg_types::TreeFeatureType::BigSummedMerkNode(sum)
                }
                TreeFeatureType::CountedMerkNode(count) => {
                    grovedbg_types::TreeFeatureType::CountedMerkNode(count)
                }
                TreeFeatureType::CountedSummedMerkNode(count, sum) => {
                    grovedbg_types::TreeFeatureType::CountedSummedMerkNode(count, sum)
                }
                TreeFeatureType::ProvableCountedMerkNode(count) => {
                    grovedbg_types::TreeFeatureType::ProvableCountedMerkNode(count)
                }
                TreeFeatureType::ProvableCountedSummedMerkNode(count, sum) => {
                    grovedbg_types::TreeFeatureType::ProvableCountedSummedMerkNode(count, sum)
                }
                TreeFeatureType::ProvableSummedMerkNode(sum) => {
                    grovedbg_types::TreeFeatureType::ProvableSummedMerkNode(sum)
                }
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum) => {
                    grovedbg_types::TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                        count, sum,
                    )
                }
            };
            MerkProofNode::KVValueHashFeatureType(
                key,
                proof_element_to_grovedbg(element),
                hash,
                node_feature_type,
            )
        }
        Node::KVRefValueHash(key, value, hash) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KVRefValueHash(key, proof_element_to_grovedbg(element), hash)
        }
        Node::KVCount(key, value, count) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            let val_hash = value_hash(&value).unwrap();
            MerkProofNode::KVValueHashFeatureType(
                key,
                proof_element_to_grovedbg(element),
                val_hash,
                grovedbg_types::TreeFeatureType::ProvableCountedMerkNode(count),
            )
        }
        Node::KVHashCount(hash, count) => MerkProofNode::KVValueHashFeatureType(
            vec![],
            grovedbg_types::Element::Item {
                value: vec![],
                element_flags: None,
            },
            hash,
            grovedbg_types::TreeFeatureType::ProvableCountedMerkNode(count),
        ),
        Node::KVRefValueHashCount(key, value, hash, count) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            // Note: Treating as KVValueHashFeatureType for debug display purposes
            // since grovedbg_types may not have KVRefValueHashCount
            MerkProofNode::KVValueHashFeatureType(
                key,
                proof_element_to_grovedbg(element),
                hash,
                grovedbg_types::TreeFeatureType::ProvableCountedMerkNode(count),
            )
        }
        Node::KVValueHashFeatureTypeWithChildHash(key, value, hash, feature_type, _child_hash) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            let node_feature_type = match feature_type {
                TreeFeatureType::BasicMerkNode => grovedbg_types::TreeFeatureType::BasicMerkNode,
                TreeFeatureType::SummedMerkNode(sum) => {
                    grovedbg_types::TreeFeatureType::SummedMerkNode(sum)
                }
                TreeFeatureType::BigSummedMerkNode(sum) => {
                    grovedbg_types::TreeFeatureType::BigSummedMerkNode(sum)
                }
                TreeFeatureType::CountedMerkNode(count) => {
                    grovedbg_types::TreeFeatureType::CountedMerkNode(count)
                }
                TreeFeatureType::CountedSummedMerkNode(count, sum) => {
                    grovedbg_types::TreeFeatureType::CountedSummedMerkNode(count, sum)
                }
                TreeFeatureType::ProvableCountedMerkNode(count) => {
                    grovedbg_types::TreeFeatureType::ProvableCountedMerkNode(count)
                }
                TreeFeatureType::ProvableCountedSummedMerkNode(count, sum) => {
                    grovedbg_types::TreeFeatureType::ProvableCountedSummedMerkNode(count, sum)
                }
                TreeFeatureType::ProvableSummedMerkNode(sum) => {
                    grovedbg_types::TreeFeatureType::ProvableSummedMerkNode(sum)
                }
                TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum) => {
                    grovedbg_types::TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                        count, sum,
                    )
                }
            };
            MerkProofNode::KVValueHashFeatureType(
                key,
                proof_element_to_grovedbg(element),
                hash,
                node_feature_type,
            )
        }
        // HashWithCount is the self-verifying compressed-subtree variant used
        // by AggregateCountOnRange proofs. The debugger UI doesn't have a
        // dedicated rendering for it yet — surface its committed node hash
        // (computed from the four committed fields) and the count via the
        // existing KVValueHashFeatureType slot, the same approach used for
        // KVHashCount above.
        Node::HashWithCount(kv_hash, left_child_hash, right_child_hash, count) => {
            use grovedb_merk::tree::node_hash_with_count;
            let computed_node_hash =
                node_hash_with_count(&kv_hash, &left_child_hash, &right_child_hash, count).unwrap();
            MerkProofNode::KVValueHashFeatureType(
                vec![],
                grovedbg_types::Element::Item {
                    value: vec![],
                    element_flags: None,
                },
                computed_node_hash,
                grovedbg_types::TreeFeatureType::ProvableCountedMerkNode(count),
            )
        }
        // ProvableSumTree proof variants. Same approach as the Count
        // family — flatten through `KVValueHashFeatureType` slots using
        // `ProvableSummedMerkNode` as the embedded feature type.
        Node::KVSum(key, value, sum) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            let val_hash = value_hash(&value).unwrap();
            MerkProofNode::KVValueHashFeatureType(
                key,
                proof_element_to_grovedbg(element),
                val_hash,
                grovedbg_types::TreeFeatureType::ProvableSummedMerkNode(sum),
            )
        }
        Node::KVHashSum(hash, sum) => MerkProofNode::KVValueHashFeatureType(
            vec![],
            grovedbg_types::Element::Item {
                value: vec![],
                element_flags: None,
            },
            hash,
            grovedbg_types::TreeFeatureType::ProvableSummedMerkNode(sum),
        ),
        Node::KVRefValueHashSum(key, value, hash, sum) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KVValueHashFeatureType(
                key,
                proof_element_to_grovedbg(element),
                hash,
                grovedbg_types::TreeFeatureType::ProvableSummedMerkNode(sum),
            )
        }
        Node::KVDigestSum(key, hash, sum) => MerkProofNode::KVValueHashFeatureType(
            key,
            grovedbg_types::Element::Item {
                value: vec![],
                element_flags: None,
            },
            hash,
            grovedbg_types::TreeFeatureType::ProvableSummedMerkNode(sum),
        ),
        Node::HashWithSum(kv_hash, left_child_hash, right_child_hash, sum) => {
            use grovedb_merk::tree::node_hash_with_sum;
            let computed_node_hash =
                node_hash_with_sum(&kv_hash, &left_child_hash, &right_child_hash, sum).unwrap();
            MerkProofNode::KVValueHashFeatureType(
                vec![],
                grovedbg_types::Element::Item {
                    value: vec![],
                    element_flags: None,
                },
                computed_node_hash,
                grovedbg_types::TreeFeatureType::ProvableSummedMerkNode(sum),
            )
        }
        // ProvableCountProvableSumTree proof-node variants. Same approach
        // as the Count and Sum families above — flatten through
        // `KVValueHashFeatureType` slots using
        // `ProvableCountedAndProvableSummedMerkNode` as the embedded
        // feature type.
        Node::KVCountSum(key, value, count, sum) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            let val_hash = value_hash(&value).unwrap();
            MerkProofNode::KVValueHashFeatureType(
                key,
                proof_element_to_grovedbg(element),
                val_hash,
                grovedbg_types::TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                    count, sum,
                ),
            )
        }
        Node::KVHashCountSum(hash, count, sum) => MerkProofNode::KVValueHashFeatureType(
            vec![],
            grovedbg_types::Element::Item {
                value: vec![],
                element_flags: None,
            },
            hash,
            grovedbg_types::TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum),
        ),
        Node::KVRefValueHashCountSum(key, value, hash, count, sum) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KVValueHashFeatureType(
                key,
                proof_element_to_grovedbg(element),
                hash,
                grovedbg_types::TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                    count, sum,
                ),
            )
        }
        Node::KVDigestCountSum(key, hash, count, sum) => MerkProofNode::KVValueHashFeatureType(
            key,
            grovedbg_types::Element::Item {
                value: vec![],
                element_flags: None,
            },
            hash,
            grovedbg_types::TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum),
        ),
        Node::HashWithCountAndSum(kv_hash, left_child_hash, right_child_hash, count, sum) => {
            use grovedb_merk::tree::node_hash_with_count_and_sum;
            let computed_node_hash = node_hash_with_count_and_sum(
                &kv_hash,
                &left_child_hash,
                &right_child_hash,
                count,
                sum,
            )
            .unwrap();
            MerkProofNode::KVValueHashFeatureType(
                vec![],
                grovedbg_types::Element::Item {
                    value: vec![],
                    element_flags: None,
                },
                computed_node_hash,
                grovedbg_types::TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                    count, sum,
                ),
            )
        }
    })
}

fn prove_options_to_grovedbg(options: ProveOptions) -> grovedbg_types::ProveOptions {
    grovedbg_types::ProveOptions {
        decrease_limit_on_empty_sub_query_result: options.decrease_limit_on_empty_sub_query_result,
    }
}

fn path_query_to_grovedb(query: PathQuery) -> crate::PathQuery {
    let PathQuery {
        path,
        query:
            SizedQuery {
                limit,
                offset,
                query: inner_query,
            },
    } = query;

    crate::PathQuery {
        path,
        query: crate::SizedQuery {
            query: query_to_grovedb(inner_query),
            limit,
            offset,
        },
    }
}

fn query_to_grovedb(query: Query) -> crate::Query {
    crate::Query {
        items: query.items.into_iter().map(query_item_to_grovedb).collect(),
        default_subquery_branch: subquery_branch_to_grovedb(query.default_subquery_branch),
        conditional_subquery_branches: conditional_subquery_branches_to_grovedb(
            query.conditional_subquery_branches,
        ),
        left_to_right: query.left_to_right,
        add_parent_tree_on_subquery: query.add_parent_tree_on_subquery,
        // The grovedbg wire type has no read-mode vocabulary (the
        // debugger UI cannot express axis or sum-budget reads, same as
        // it cannot express aggregate items), so debugger-issued
        // queries are always plain key selection.
        read_mode: None,
        limit: None,
    }
}

fn conditional_subquery_branches_to_grovedb(
    conditional_subquery_branches: Vec<(QueryItem, SubqueryBranch)>,
) -> Option<IndexMap<crate::QueryItem, grovedb_merk::proofs::query::SubqueryBranch>> {
    if conditional_subquery_branches.is_empty() {
        None
    } else {
        Some(
            conditional_subquery_branches
                .into_iter()
                .map(|(item, branch)| {
                    (
                        query_item_to_grovedb(item),
                        subquery_branch_to_grovedb(branch),
                    )
                })
                .collect(),
        )
    }
}

fn subquery_branch_to_grovedb(
    subquery_branch: SubqueryBranch,
) -> grovedb_merk::proofs::query::SubqueryBranch {
    grovedb_merk::proofs::query::SubqueryBranch {
        subquery_path: subquery_branch.subquery_path,
        subquery: subquery_branch
            .subquery
            .map(|q| Box::new(query_to_grovedb(*q))),
    }
}

fn query_item_to_grovedb(item: QueryItem) -> crate::QueryItem {
    match item {
        QueryItem::Key(x) => crate::QueryItem::Key(x),
        QueryItem::Range { start, end } => crate::QueryItem::Range(start..end),
        QueryItem::RangeInclusive { start, end } => crate::QueryItem::RangeInclusive(start..=end),
        QueryItem::RangeFull => crate::QueryItem::RangeFull(..),
        QueryItem::RangeFrom(x) => crate::QueryItem::RangeFrom(x..),
        QueryItem::RangeTo(x) => crate::QueryItem::RangeTo(..x),
        QueryItem::RangeToInclusive(x) => crate::QueryItem::RangeToInclusive(..=x),
        QueryItem::RangeAfter(x) => crate::QueryItem::RangeAfter(x..),
        QueryItem::RangeAfterTo { after, to } => crate::QueryItem::RangeAfterTo(after..to),
        QueryItem::RangeAfterToInclusive { after, to } => {
            crate::QueryItem::RangeAfterToInclusive(after..=to)
        }
    }
}

/// Convert a [`crate::ReferencePathType`] plus optional element flags
/// into the corresponding `grovedbg_types::Reference` wire variant.
/// Shared by the plain `Element::Reference`, the
/// `Element::ReferenceWithSumItem`, and the
/// `Element::BidirectionalReference` arms of [`element_to_grovedbg`].
fn reference_path_to_grovedbg(
    reference_path: ReferencePathType,
    element_flags: Option<Vec<u8>>,
) -> grovedbg_types::Reference {
    match reference_path {
        ReferencePathType::AbsolutePathReference(path) => {
            grovedbg_types::Reference::AbsolutePathReference {
                path,
                element_flags,
            }
        }
        ReferencePathType::UpstreamRootHeightReference(n_keep, path_append) => {
            grovedbg_types::Reference::UpstreamRootHeightReference {
                n_keep: n_keep.into(),
                path_append,
                element_flags,
            }
        }
        ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(
            n_keep,
            path_append,
        ) => grovedbg_types::Reference::UpstreamRootHeightWithParentPathAdditionReference {
            n_keep: n_keep.into(),
            path_append,
            element_flags,
        },
        ReferencePathType::UpstreamFromElementHeightReference(n_remove, path_append) => {
            grovedbg_types::Reference::UpstreamFromElementHeightReference {
                n_remove: n_remove.into(),
                path_append,
                element_flags,
            }
        }
        ReferencePathType::CousinReference(swap_parent) => {
            grovedbg_types::Reference::CousinReference {
                swap_parent,
                element_flags,
            }
        }
        ReferencePathType::RemovedCousinReference(swap_parent) => {
            grovedbg_types::Reference::RemovedCousinReference {
                swap_parent,
                element_flags,
            }
        }
        ReferencePathType::SiblingReference(sibling_key) => {
            grovedbg_types::Reference::SiblingReference {
                sibling_key,
                element_flags,
            }
        }
    }
}

/// Proofs carry stripped elements; the empty list cannot tell us the
/// number of referrers registered on the stored node.
fn proof_element_to_grovedbg(element: crate::Element) -> grovedbg_types::Element {
    let mut element = element_to_grovedbg(element);
    match &mut element {
        grovedbg_types::Element::ItemWithBackwardsReferences {
            backward_references_count,
            ..
        }
        | grovedbg_types::Element::SumItemWithBackwardsReferences {
            backward_references_count,
            ..
        }
        | grovedbg_types::Element::ItemWithSumItemWithBackwardsReferences {
            backward_references_count,
            ..
        }
        | grovedbg_types::Element::BidirectionalReference {
            backward_references_count,
            ..
        } => {
            *backward_references_count = None;
        }
        _ => {}
    }
    element
}

/// Convert a stored element with its actual referrer count. Proofs must
/// use `proof_element_to_grovedbg` instead.
fn element_to_grovedbg(element: crate::Element) -> grovedbg_types::Element {
    match element {
        crate::Element::Item(value, element_flags) => grovedbg_types::Element::Item {
            value,
            element_flags,
        },
        crate::Element::ItemWithBackwardsReferences(value, backward_references, element_flags) => {
            grovedbg_types::Element::ItemWithBackwardsReferences {
                value,
                max_incoming_references: backward_references.max_incoming,
                backward_references_count: Some(backward_references.entries.len() as u16),
                element_flags,
            }
        }
        crate::Element::Tree(root_key, element_flags) => grovedbg_types::Element::Subtree {
            root_key,
            element_flags,
        },
        crate::Element::Reference(reference_path, _, element_flags) => {
            grovedbg_types::Element::Reference(reference_path_to_grovedbg(
                reference_path,
                element_flags,
            ))
        }
        crate::Element::BidirectionalReference(reference, flags) => {
            grovedbg_types::Element::BidirectionalReference {
                reference: reference_path_to_grovedbg(reference.forward_reference_path, flags),
                cascade_on_update: reference.cascade_on_update,
                backward_references_count: Some(reference.backward_references.len() as u16),
            }
        }
        crate::Element::ReferenceWithSumItem(
            reference_path,
            _max_hop,
            sum_item_value,
            element_flags,
        ) => grovedbg_types::Element::ReferenceWithSumItem {
            reference: reference_path_to_grovedbg(reference_path, element_flags),
            sum_item_value,
        },
        crate::Element::SumItem(value, element_flags) => grovedbg_types::Element::SumItem {
            value,
            element_flags,
        },
        crate::Element::SumItemWithBackwardsReferences(
            value,
            backward_references,
            element_flags,
        ) => grovedbg_types::Element::SumItemWithBackwardsReferences {
            value,
            max_incoming_references: backward_references.max_incoming,
            backward_references_count: Some(backward_references.entries.len() as u16),
            element_flags,
        },
        crate::Element::ItemWithSumItem(value, sum_value, element_flags) => {
            grovedbg_types::Element::ItemWithSumItem {
                value,
                sum_item_value: sum_value,
                element_flags,
            }
        }
        crate::Element::ItemWithSumItemWithBackwardsReferences(
            value,
            sum_value,
            backward_references,
            element_flags,
        ) => grovedbg_types::Element::ItemWithSumItemWithBackwardsReferences {
            value,
            sum_item_value: sum_value,
            max_incoming_references: backward_references.max_incoming,
            backward_references_count: Some(backward_references.entries.len() as u16),
            element_flags,
        },
        crate::Element::SumTree(root_key, sum, element_flags) => grovedbg_types::Element::Sumtree {
            root_key,
            sum,
            element_flags,
        },
        crate::Element::BigSumTree(root_key, sum, element_flags) => {
            grovedbg_types::Element::BigSumTree {
                root_key,
                sum,
                element_flags,
            }
        }
        crate::Element::CountTree(root_key, count, element_flags) => {
            grovedbg_types::Element::CountTree {
                root_key,
                count,
                element_flags,
            }
        }
        crate::Element::CountSumTree(root_key, count, sum, element_flags) => {
            grovedbg_types::Element::CountSumTree {
                root_key,
                count,
                sum,
                element_flags,
            }
        }
        crate::Element::ProvableCountTree(root_key, count, element_flags) => {
            grovedbg_types::Element::ProvableCountTree {
                root_key,
                count,
                element_flags,
            }
        }
        crate::Element::ProvableCountSumTree(root_key, count, sum, element_flags) => {
            grovedbg_types::Element::ProvableCountSumTree {
                root_key,
                count,
                sum,
                element_flags,
            }
        }
        crate::Element::ProvableSumTree(root_key, sum, element_flags) => {
            grovedbg_types::Element::ProvableSumTree {
                root_key,
                sum,
                element_flags,
            }
        }
        crate::Element::ProvableCountProvableSumTree(root_key, count, sum, element_flags) => {
            grovedbg_types::Element::ProvableCountProvableSumTree {
                root_key,
                count,
                sum,
                element_flags,
            }
        }
        crate::Element::CommitmentTree(_, _, element_flags) => grovedbg_types::Element::Subtree {
            root_key: None,
            element_flags,
        },
        crate::Element::MmrTree(_, element_flags) => grovedbg_types::Element::Subtree {
            root_key: None,
            element_flags,
        },
        crate::Element::BulkAppendTree(_, _, element_flags) => grovedbg_types::Element::Subtree {
            root_key: None,
            element_flags,
        },
        crate::Element::DenseAppendOnlyFixedSizeTree(_, _, element_flags) => {
            grovedbg_types::Element::Subtree {
                root_key: None,
                element_flags,
            }
        }
        crate::Element::PrivateDocumentStore(_, _, _, element_flags) => {
            grovedbg_types::Element::Subtree {
                root_key: None,
                element_flags,
            }
        }
        // The visualizer wire format has no wrapper variants; render the
        // inner element. The wrapper is invisible at the debug-UI layer.
        crate::Element::NonCounted(inner)
        | crate::Element::NotSummed(inner)
        | crate::Element::NotCountedOrSummed(inner) => element_to_grovedbg(*inner),
        // Indexed-tree variants are not yet represented in the
        // grovedbg wire format; render them as a generic subtree pointing
        // at the primary's root key. The secondary (or axes TLV for
        // PCPSIT) is invisible to the debug UI for now.
        crate::Element::ProvableSumIndexedTree(primary_root_key, _, _, element_flags)
        | crate::Element::ProvableCountIndexedTree(primary_root_key, _, _, element_flags) => {
            grovedbg_types::Element::Subtree {
                root_key: primary_root_key,
                element_flags,
            }
        }
        crate::Element::ProvableCountProvableSumIndexedTree(
            primary_root_key,
            _,
            _,
            _,
            element_flags,
        ) => grovedbg_types::Element::Subtree {
            root_key: primary_root_key,
            element_flags,
        },
    }
}

fn node_to_update(
    path: Path,
    NodeDbg {
        key,
        value,
        left_child,
        left_merk_hash,
        right_child,
        right_merk_hash,
        value_hash,
        kv_digest_hash,
        feature_type,
    }: NodeDbg,
) -> Result<NodeUpdate, crate::Error> {
    let grovedb_element = crate::Element::deserialize(&value, GroveVersion::latest())?;

    let element = element_to_grovedbg(grovedb_element);

    Ok(NodeUpdate {
        path,
        key,
        element,
        left_child,
        left_merk_hash,
        right_child,
        right_merk_hash,
        feature_type: match feature_type {
            TreeFeatureType::BasicMerkNode => grovedbg_types::TreeFeatureType::BasicMerkNode,
            TreeFeatureType::SummedMerkNode(sum) => {
                grovedbg_types::TreeFeatureType::SummedMerkNode(sum)
            }
            TreeFeatureType::BigSummedMerkNode(sum) => {
                grovedbg_types::TreeFeatureType::BigSummedMerkNode(sum)
            }
            TreeFeatureType::CountedMerkNode(count) => {
                grovedbg_types::TreeFeatureType::CountedMerkNode(count)
            }
            TreeFeatureType::CountedSummedMerkNode(count, sum) => {
                grovedbg_types::TreeFeatureType::CountedSummedMerkNode(count, sum)
            }
            TreeFeatureType::ProvableCountedMerkNode(count) => {
                grovedbg_types::TreeFeatureType::ProvableCountedMerkNode(count)
            }
            TreeFeatureType::ProvableCountedSummedMerkNode(count, sum) => {
                grovedbg_types::TreeFeatureType::ProvableCountedSummedMerkNode(count, sum)
            }
            TreeFeatureType::ProvableSummedMerkNode(sum) => {
                grovedbg_types::TreeFeatureType::ProvableSummedMerkNode(sum)
            }
            TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(count, sum) => {
                grovedbg_types::TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(
                    count, sum,
                )
            }
        },
        value_hash,
        kv_digest_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_to_grovedbg_converts_item_with_sum_item() {
        let flags = Some(vec![1, 2, 3]);
        let element = crate::Element::ItemWithSumItem(b"dbg".to_vec(), -5, flags.clone());
        match element_to_grovedbg(element) {
            grovedbg_types::Element::ItemWithSumItem {
                value,
                sum_item_value,
                element_flags,
            } => {
                assert_eq!(value, b"dbg");
                assert_eq!(sum_item_value, -5);
                assert_eq!(element_flags, flags);
            }
            _ => panic!("unexpected debugger conversion"),
        }
    }

    /// `ReferenceWithSumItem` is converted to the dedicated
    /// `grovedbg_types::Element::ReferenceWithSumItem` wire variant
    /// — the path is forwarded via the shared
    /// `reference_path_to_grovedbg` helper and the explicit
    /// `sum_item_value` (independent of the resolved target) is
    /// preserved on the wire.
    #[test]
    fn element_to_grovedbg_converts_reference_with_sum_item_absolute_path() {
        let flags = Some(vec![9, 9, 9]);
        let path = ReferencePathType::AbsolutePathReference(vec![
            b"some_leaf".to_vec(),
            b"target".to_vec(),
        ]);
        let element = crate::Element::ReferenceWithSumItem(path, Some(3), 42, flags.clone());
        match element_to_grovedbg(element) {
            grovedbg_types::Element::ReferenceWithSumItem {
                reference,
                sum_item_value,
            } => {
                assert_eq!(sum_item_value, 42);
                match reference {
                    grovedbg_types::Reference::AbsolutePathReference {
                        path,
                        element_flags,
                    } => {
                        assert_eq!(path, vec![b"some_leaf".to_vec(), b"target".to_vec()]);
                        assert_eq!(element_flags, flags);
                    }
                    other => panic!("unexpected wire reference: {other:?}"),
                }
            }
            other => panic!("unexpected debugger conversion: {other:?}"),
        }
    }

    /// A non-absolute reference-with-sum-item (here `SiblingReference`)
    /// also flows through `reference_path_to_grovedbg` unchanged.
    /// Exercises one of the six non-absolute path variants to confirm
    /// the helper covers the full discriminant set.
    #[test]
    fn element_to_grovedbg_converts_reference_with_sum_item_sibling() {
        let element = crate::Element::ReferenceWithSumItem(
            ReferencePathType::SiblingReference(b"sib".to_vec()),
            None,
            -7,
            None,
        );
        match element_to_grovedbg(element) {
            grovedbg_types::Element::ReferenceWithSumItem {
                reference,
                sum_item_value,
            } => {
                assert_eq!(sum_item_value, -7);
                assert!(matches!(
                    reference,
                    grovedbg_types::Reference::SiblingReference {
                        sibling_key,
                        element_flags: None,
                    } if sibling_key == b"sib".to_vec()
                ));
            }
            other => panic!("unexpected debugger conversion: {other:?}"),
        }
    }

    /// Backward-references items map to their dedicated wire variants,
    /// carrying capacity and occupancy instead of collapsing to the
    /// plain counterparts.
    #[test]
    fn element_to_grovedbg_converts_backwards_references_items() {
        use crate::{bidirectional_references::BackwardReference, BackwardReferences};

        let mut backward_references = BackwardReferences::with_max_incoming(8);
        backward_references.entries.push(BackwardReference {
            inverted_reference: ReferencePathType::SiblingReference(b"referrer".to_vec()),
            cascade_on_update: false,
        });

        let element = crate::Element::ItemWithBackwardsReferences(
            b"data".to_vec(),
            backward_references.clone(),
            Some(vec![9]),
        );
        match element_to_grovedbg(element) {
            grovedbg_types::Element::ItemWithBackwardsReferences {
                value,
                max_incoming_references,
                backward_references_count,
                element_flags,
            } => {
                assert_eq!(value, b"data");
                assert_eq!(max_incoming_references, 8);
                assert_eq!(backward_references_count, Some(1));
                assert_eq!(element_flags, Some(vec![9]));
            }
            other => panic!("unexpected debugger conversion: {other:?}"),
        }

        let element =
            crate::Element::SumItemWithBackwardsReferences(-3, backward_references.clone(), None);
        match element_to_grovedbg(element) {
            grovedbg_types::Element::SumItemWithBackwardsReferences {
                value,
                max_incoming_references,
                backward_references_count,
                element_flags,
            } => {
                assert_eq!(value, -3);
                assert_eq!(max_incoming_references, 8);
                assert_eq!(backward_references_count, Some(1));
                assert_eq!(element_flags, None);
            }
            other => panic!("unexpected debugger conversion: {other:?}"),
        }

        let element = crate::Element::ItemWithSumItemWithBackwardsReferences(
            b"both".to_vec(),
            12,
            backward_references,
            None,
        );
        match element_to_grovedbg(element) {
            grovedbg_types::Element::ItemWithSumItemWithBackwardsReferences {
                value,
                sum_item_value,
                max_incoming_references,
                backward_references_count,
                element_flags,
            } => {
                assert_eq!(value, b"both");
                assert_eq!(sum_item_value, 12);
                assert_eq!(max_incoming_references, 8);
                assert_eq!(backward_references_count, Some(1));
                assert_eq!(element_flags, None);
            }
            other => panic!("unexpected debugger conversion: {other:?}"),
        }
    }

    /// A bidirectional reference keeps the plain-reference wire shape
    /// for its forward path (flags included) and additionally exposes
    /// the cascade policy and its own referrer count.
    #[test]
    fn element_to_grovedbg_converts_bidirectional_reference() {
        use crate::bidirectional_references::BidirectionalReference;

        let element = crate::Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                    b"leaf".to_vec(),
                    b"target".to_vec(),
                ]),
                cascade_on_update: true,
                max_hop: Some(3),
                backward_references: Vec::new(),
            },
            Some(vec![4, 5]),
        );
        match element_to_grovedbg(element) {
            grovedbg_types::Element::BidirectionalReference {
                reference,
                cascade_on_update,
                backward_references_count,
            } => {
                assert!(cascade_on_update);
                assert_eq!(backward_references_count, Some(0));
                match reference {
                    grovedbg_types::Reference::AbsolutePathReference {
                        path,
                        element_flags,
                    } => {
                        assert_eq!(path, vec![b"leaf".to_vec(), b"target".to_vec()]);
                        assert_eq!(element_flags, Some(vec![4, 5]));
                    }
                    other => panic!("unexpected wire reference: {other:?}"),
                }
            }
            other => panic!("unexpected debugger conversion: {other:?}"),
        }
    }

    #[test]
    fn proof_counts_are_unknown_for_every_backward_references_variant() {
        use crate::{bidirectional_references::BackwardReference, BackwardReferences};
        let version = GroveVersion::latest();
        let count = |element: grovedbg_types::Element| match element {
            grovedbg_types::Element::ItemWithBackwardsReferences {
                backward_references_count,
                ..
            }
            | grovedbg_types::Element::SumItemWithBackwardsReferences {
                backward_references_count,
                ..
            }
            | grovedbg_types::Element::ItemWithSumItemWithBackwardsReferences {
                backward_references_count,
                ..
            }
            | grovedbg_types::Element::BidirectionalReference {
                backward_references_count,
                ..
            } => backward_references_count,
            other => panic!("unexpected element: {other:?}"),
        };
        for entries in [
            vec![],
            vec![BackwardReference {
                inverted_reference: ReferencePathType::SiblingReference(b"referrer".to_vec()),
                cascade_on_update: true,
            }],
        ] {
            let expected_count = Some(entries.len() as u16);
            let refs = BackwardReferences::new(8, entries.clone());
            let elements = [
                crate::Element::ItemWithBackwardsReferences(b"data".to_vec(), refs.clone(), None),
                crate::Element::SumItemWithBackwardsReferences(-3, refs.clone(), None),
                crate::Element::ItemWithSumItemWithBackwardsReferences(
                    b"data".to_vec(),
                    3,
                    refs,
                    None,
                ),
                crate::Element::BidirectionalReference(
                    crate::BidirectionalReference {
                        forward_reference_path: ReferencePathType::SiblingReference(
                            b"target".to_vec(),
                        ),
                        cascade_on_update: true,
                        max_hop: None,
                        backward_references: entries,
                    },
                    None,
                ),
            ];
            for element in elements {
                assert_eq!(count(element_to_grovedbg(element.clone())), expected_count);
                let bytes = element
                    .stripped_of_backward_references()
                    .serialize(version)
                    .unwrap();
                assert_eq!(
                    count(proof_element_to_grovedbg(
                        crate::Element::deserialize(&bytes, version).unwrap()
                    )),
                    None
                );
            }
        }
    }

    #[test]
    fn legacy_format_preserves_previous_element_shapes() {
        use grovedbg_types::Element;
        let reference = grovedbg_types::Reference::SiblingReference {
            sibling_key: b"target".to_vec(),
            element_flags: Some(vec![7]),
        };
        let pairs = [
            (
                Element::ItemWithBackwardsReferences {
                    value: b"data".to_vec(),
                    max_incoming_references: 8,
                    backward_references_count: Some(1),
                    element_flags: Some(vec![1]),
                },
                Element::Item {
                    value: b"data".to_vec(),
                    element_flags: Some(vec![1]),
                },
            ),
            (
                Element::SumItemWithBackwardsReferences {
                    value: -3,
                    max_incoming_references: 8,
                    backward_references_count: Some(1),
                    element_flags: Some(vec![2]),
                },
                Element::SumItem {
                    value: -3,
                    element_flags: Some(vec![2]),
                },
            ),
            (
                Element::ItemWithSumItemWithBackwardsReferences {
                    value: b"both".to_vec(),
                    sum_item_value: 3,
                    max_incoming_references: 8,
                    backward_references_count: Some(1),
                    element_flags: Some(vec![3]),
                },
                Element::ItemWithSumItem {
                    value: b"both".to_vec(),
                    sum_item_value: 3,
                    element_flags: Some(vec![3]),
                },
            ),
            (
                Element::BidirectionalReference {
                    reference: reference.clone(),
                    cascade_on_update: true,
                    backward_references_count: Some(0),
                },
                Element::Reference(reference),
            ),
        ];
        let mut headers = HeaderMap::new();
        for (extended, legacy) in pairs {
            // Missing and unrecognized capabilities must keep the old shapes.
            for value in [None, Some("false"), Some("unsupported")] {
                headers.remove("x-grovedbg-backward-references");
                if let Some(value) = value {
                    headers.insert("x-grovedbg-backward-references", value.parse().unwrap());
                }
                assert_eq!(
                    BackwardReferencesFormat::from(&headers).element(extended.clone()),
                    legacy
                );
            }
            headers.insert("x-grovedbg-backward-references", "true".parse().unwrap());
            assert_eq!(
                BackwardReferencesFormat::from(&headers).element(extended.clone()),
                extended
            );
        }
    }

    #[test]
    fn legacy_proof_format_covers_nested_layers_and_element_bearing_nodes() {
        let make_proof = |element: grovedbg_types::Element| {
            let nodes = [
                MerkProofNode::KV(b"a".to_vec(), element.clone()),
                MerkProofNode::KVValueHash(b"b".to_vec(), element.clone(), [1; 32]),
                MerkProofNode::KVValueHashFeatureType(
                    b"c".to_vec(),
                    element.clone(),
                    [2; 32],
                    grovedbg_types::TreeFeatureType::SummedMerkNode(3),
                ),
                MerkProofNode::KVRefValueHash(b"d".to_vec(), element, [3; 32]),
                MerkProofNode::Hash([4; 32]),
                MerkProofNode::KVHash([5; 32]),
                MerkProofNode::KVDigest(b"e".to_vec(), [6; 32]),
            ];
            grovedbg_types::Proof {
                root_layer: grovedbg_types::ProofLayer {
                    merk_proof: nodes
                        .iter()
                        .cloned()
                        .map(MerkProofOp::Push)
                        .chain([MerkProofOp::Parent, MerkProofOp::Child])
                        .collect(),
                    lower_layers: BTreeMap::from([(
                        b"subtree".to_vec(),
                        grovedbg_types::ProofLayer {
                            merk_proof: nodes
                                .into_iter()
                                .map(MerkProofOp::PushInverted)
                                .chain([MerkProofOp::ParentInverted, MerkProofOp::ChildInverted])
                                .collect(),
                            lower_layers: BTreeMap::new(),
                        },
                    )]),
                },
                prove_options: grovedbg_types::ProveOptions {
                    decrease_limit_on_empty_sub_query_result: true,
                },
            }
        };
        let extended = make_proof(grovedbg_types::Element::ItemWithBackwardsReferences {
            value: b"data".to_vec(),
            max_incoming_references: 8,
            backward_references_count: None,
            element_flags: Some(vec![9]),
        });
        let legacy = make_proof(grovedbg_types::Element::Item {
            value: b"data".to_vec(),
            element_flags: Some(vec![9]),
        });
        assert_eq!(
            BackwardReferencesFormat::Legacy.proof(extended.clone()),
            legacy
        );
        assert_eq!(
            BackwardReferencesFormat::Extended.proof(extended.clone()),
            extended
        );
    }

    /// Exercise the same session and handlers as the UI, both at the root
    /// and under a subtree, with a real registered bidirectional reference.
    #[tokio::test]
    async fn handlers_negotiate_format_and_proofs_do_not_claim_zero_referrers() {
        use crate::{operations::insert::InsertOptions, Element};
        let version = GroveVersion::latest();
        for path in [vec![], vec![b"leaf".to_vec()]] {
            let temp = tempdir().unwrap();
            let db = Arc::new(GroveDb::open(temp.path()).unwrap());
            if let Some(key) = path.first() {
                db.insert(
                    SubtreePath::empty(),
                    key,
                    Element::empty_tree(),
                    None,
                    None,
                    version,
                )
                .unwrap()
                .unwrap();
            }
            db.insert(
                path.as_slice(),
                b"target",
                Element::new_item_allowing_bidirectional_references(b"payload".to_vec()),
                None,
                None,
                version,
            )
            .unwrap()
            .unwrap();
            db.insert(
                path.as_slice(),
                b"referrer",
                Element::BidirectionalReference(
                    crate::BidirectionalReference {
                        forward_reference_path: ReferencePathType::SiblingReference(
                            b"target".to_vec(),
                        ),
                        cascade_on_update: true,
                        max_hop: None,
                        backward_references: vec![],
                    },
                    Some(vec![7]),
                ),
                Some(InsertOptions {
                    propagate_backward_references: true,
                    ..Default::default()
                }),
                None,
                version,
            )
            .unwrap()
            .unwrap();
            let state = AppState {
                cancellation_token: CancellationToken::new(),
                grovedb: Arc::downgrade(&db),
                sessions: Default::default(),
            };
            let session_id = state.new_session().await.unwrap();
            let query = PathQuery {
                path: path.clone(),
                query: SizedQuery {
                    query: Query {
                        items: vec![QueryItem::Key(b"target".to_vec())],
                        default_subquery_branch: SubqueryBranch {
                            subquery_path: None,
                            subquery: None,
                        },
                        conditional_subquery_branches: vec![],
                        left_to_right: true,
                        add_parent_tree_on_subquery: false,
                    },
                    limit: None,
                    offset: None,
                },
            };
            for extended in [false, true] {
                let mut headers = HeaderMap::new();
                if extended {
                    headers.insert("x-grovedbg-backward-references", "true".parse().unwrap());
                }
                let node = fetch_node(
                    State(state.clone()),
                    headers.clone(),
                    Json(WithSession {
                        session_id,
                        request: NodeFetchRequest {
                            path: path.clone(),
                            key: b"target".to_vec(),
                        },
                    }),
                )
                .await
                .unwrap()
                .0
                .unwrap();
                let expected = if extended {
                    grovedbg_types::Element::ItemWithBackwardsReferences {
                        value: b"payload".to_vec(),
                        max_incoming_references: crate::DEFAULT_BACKWARD_REFERENCES_CAPACITY,
                        backward_references_count: Some(1),
                        element_flags: None,
                    }
                } else {
                    grovedbg_types::Element::Item {
                        value: b"payload".to_vec(),
                        element_flags: None,
                    }
                };
                assert_eq!(node.element, expected);
                let nodes = fetch_with_path_query(
                    State(state.clone()),
                    headers.clone(),
                    Json(WithSession {
                        session_id,
                        request: query.clone(),
                    }),
                )
                .await
                .unwrap()
                .0;
                assert_eq!(nodes, vec![node]);
                let referrer = fetch_node(
                    State(state.clone()),
                    headers.clone(),
                    Json(WithSession {
                        session_id,
                        request: NodeFetchRequest {
                            path: path.clone(),
                            key: b"referrer".to_vec(),
                        },
                    }),
                )
                .await
                .unwrap()
                .0
                .unwrap();
                let reference = grovedbg_types::Reference::SiblingReference {
                    sibling_key: b"target".to_vec(),
                    element_flags: Some(vec![7]),
                };
                assert_eq!(
                    referrer.element,
                    if extended {
                        grovedbg_types::Element::BidirectionalReference {
                            reference,
                            cascade_on_update: true,
                            backward_references_count: Some(0),
                        }
                    } else {
                        grovedbg_types::Element::Reference(reference)
                    }
                );

                if path.is_empty() {
                    let root = fetch_root_node(
                        State(state.clone()),
                        headers.clone(),
                        Json(WithSession {
                            session_id,
                            request: (),
                        }),
                    )
                    .await
                    .unwrap()
                    .0
                    .unwrap();
                    assert_eq!(root.key, b"target");
                    assert_eq!(root.element, expected);
                }
                let proof = prove_path_query(
                    State(state.clone()),
                    headers,
                    Json(WithSession {
                        session_id,
                        request: query.clone(),
                    }),
                )
                .await
                .unwrap()
                .0;
                let layer = path
                    .iter()
                    .fold(&proof.root_layer, |layer, key| &layer.lower_layers[key]);
                let proof_element = layer
                    .merk_proof
                    .iter()
                    .find_map(|op| match op {
                        MerkProofOp::Push(MerkProofNode::KVValueHash(key, element, _))
                        | MerkProofOp::PushInverted(MerkProofNode::KVValueHash(key, element, _))
                            if key == b"target" =>
                        {
                            Some(element)
                        }
                        _ => None,
                    })
                    .expect("target proof node");
                if extended {
                    assert!(matches!(
                        proof_element,
                        grovedbg_types::Element::ItemWithBackwardsReferences {
                            backward_references_count: None,
                            ..
                        }
                    ));
                } else {
                    assert_eq!(proof_element, &expected);
                }
                // This is the codec used by dump_proof_grovedbg_stdout.
                let bytes =
                    bincode::serde::encode_to_vec(&proof, bincode::config::standard()).unwrap();
                let (decoded, consumed): (grovedbg_types::Proof, _) =
                    bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
                assert_eq!(decoded, proof);
                assert_eq!(consumed, bytes.len());
            }
        }
    }
}
