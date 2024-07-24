//! GroveDB debugging support module.

use std::{collections::BTreeMap, fs, net::Ipv4Addr, sync::Weak};

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use grovedb_merk::{
    debugger::NodeDbg,
    proofs::{Decoder, Node, Op},
    TreeFeatureType,
};
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;
use grovedbg_types::{
    Key, MerkProofNode, MerkProofOp, NodeFetchRequest, NodeUpdate, Path, PathQuery, Query,
    QueryItem, SizedQuery, SubqueryBranch,
};
use indexmap::IndexMap;
use tokio::{
    net::ToSocketAddrs,
    sync::mpsc::{self, Sender},
};
use tower_http::services::ServeDir;

use crate::{
    operations::proof::{GroveDBProof, LayerProof, ProveOptions},
    reference_path::ReferencePathType,
    GroveDb,
};

const GROVEDBG_ZIP: [u8; include_bytes!(concat!(env!("OUT_DIR"), "/grovedbg.zip")).len()] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/grovedbg.zip"));

pub(super) fn start_visualizer<A>(grovedb: Weak<GroveDb>, addr: A)
where
    A: ToSocketAddrs + Send + 'static,
{
    std::thread::spawn(move || {
        let grovedbg_tmp =
            tempfile::tempdir().expect("cannot create tempdir for grovedbg contents");
        let grovedbg_zip = grovedbg_tmp.path().join("grovedbg.zip");
        // let grovedbg_www = grovedbg_tmp.path().join("grovedbg_www");
        let grovedbg_www = "/home/yolo/dash/grovedbg/dist";

        // fs::write(&grovedbg_zip, &GROVEDBG_ZIP).expect("cannot crate grovedbg.zip");
        // zip_extensions::read::zip_extract(&grovedbg_zip, &grovedbg_www)
        //     .expect("cannot extract grovedbg contents");

        let (shutdown_send, mut shutdown_receive) = mpsc::channel::<()>(1);
        let app = Router::new()
            .route("/fetch_node", post(fetch_node))
            .route("/fetch_root_node", post(fetch_root_node))
            .route("/execute_path_query", post(execute_path_query))
            .fallback_service(ServeDir::new(grovedbg_www))
            .with_state((shutdown_send, grovedb));

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind(addr)
                    .await
                    .expect("can't bind visualizer port");
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        shutdown_receive.recv().await;
                    })
                    .await
                    .unwrap()
            });
    });
}

enum AppError {
    Closed,
    Any(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        match self {
            AppError::Closed => {
                (StatusCode::SERVICE_UNAVAILABLE, "GroveDB is closed").into_response()
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

async fn fetch_node(
    State((shutdown, grovedb)): State<(Sender<()>, Weak<GroveDb>)>,
    Json(NodeFetchRequest { path, key }): Json<NodeFetchRequest>,
) -> Result<Json<Option<NodeUpdate>>, AppError> {
    let Some(db) = grovedb.upgrade() else {
        shutdown.send(()).await.ok();
        return Err(AppError::Closed);
    };

    // todo: GroveVersion::latest() to actual version
    let merk = db
        .open_non_transactional_merk_at_path(path.as_slice().into(), None, GroveVersion::latest())
        .unwrap()?;
    let node = merk.get_node_dbg(&key)?;

    if let Some(node) = node {
        let node_update: NodeUpdate = node_to_update(path, node)?;
        Ok(Json(Some(node_update)))
    } else {
        Ok(None.into())
    }
}

async fn fetch_root_node(
    State((shutdown, grovedb)): State<(Sender<()>, Weak<GroveDb>)>,
) -> Result<Json<Option<NodeUpdate>>, AppError> {
    let Some(db) = grovedb.upgrade() else {
        shutdown.send(()).await.ok();
        return Err(AppError::Closed);
    };

    // todo: GroveVersion::latest() to actual version
    let merk = db
        .open_non_transactional_merk_at_path(SubtreePath::empty(), None, GroveVersion::latest())
        .unwrap()?;

    let node = merk.get_root_node_dbg()?;

    if let Some(node) = node {
        let node_update: NodeUpdate = node_to_update(Vec::new(), node)?;
        Ok(Json(Some(node_update)))
    } else {
        Ok(None.into())
    }
}

async fn execute_path_query(
    State((shutdown, grovedb)): State<(Sender<()>, Weak<GroveDb>)>,
    Json(json_path_query): Json<PathQuery>,
) -> Result<Json<grovedbg_types::Proof>, AppError> {
    let Some(db) = grovedb.upgrade() else {
        shutdown.send(()).await.ok();
        return Err(AppError::Closed);
    };

    let path_query = path_query_to_grovedb(json_path_query);

    let grovedb_proof = db
        .prove_internal(&path_query, None, &GroveVersion::latest())
        .unwrap()?;
    Ok(Json(proof_to_grovedbg(grovedb_proof)?))
}

fn proof_to_grovedbg(proof: GroveDBProof) -> Result<grovedbg_types::Proof, crate::Error> {
    match proof {
        GroveDBProof::V0(p) => Ok(grovedbg_types::Proof {
            root_layer: proof_layer_to_grovedbg(p.root_layer)?,
            prove_options: prove_options_to_grovedbg(p.prove_options),
        }),
    }
}

fn proof_layer_to_grovedbg(
    proof_layer: LayerProof,
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
        Node::KVDigest(key, hash) => MerkProofNode::KVDigest(key, hash),
        Node::KV(key, value) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KV(key, element_to_grovedbg(element))
        }
        Node::KVValueHash(key, value, hash) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KVValueHash(key, element_to_grovedbg(element), hash)
        }
        Node::KVValueHashFeatureType(key, value, hash, TreeFeatureType::BasicMerkNode) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KVValueHashFeatureType(
                key,
                element_to_grovedbg(element),
                hash,
                grovedbg_types::TreeFeatureType::BasicMerkNode,
            )
        }
        Node::KVValueHashFeatureType(key, value, hash, TreeFeatureType::SummedMerkNode(sum)) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KVValueHashFeatureType(
                key,
                element_to_grovedbg(element),
                hash,
                grovedbg_types::TreeFeatureType::SummedMerkNode(sum),
            )
        }
        Node::KVRefValueHash(key, value, hash) => {
            let element = crate::Element::deserialize(&value, GroveVersion::latest())?;
            MerkProofNode::KVRefValueHash(key, element_to_grovedbg(element), hash)
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

fn element_to_grovedbg(element: crate::Element) -> grovedbg_types::Element {
    match element {
        crate::Element::Item(value, element_flags) => grovedbg_types::Element::Item {
            value,
            element_flags,
        },
        crate::Element::Tree(root_key, element_flags) => grovedbg_types::Element::Subtree {
            root_key,
            element_flags,
        },
        crate::Element::Reference(
            ReferencePathType::AbsolutePathReference(path),
            _,
            element_flags,
        ) => grovedbg_types::Element::AbsolutePathReference {
            path,
            element_flags,
        },
        crate::Element::Reference(
            ReferencePathType::UpstreamRootHeightReference(n_keep, path_append),
            _,
            element_flags,
        ) => grovedbg_types::Element::UpstreamRootHeightReference {
            n_keep: n_keep.into(),
            path_append,
            element_flags,
        },
        crate::Element::Reference(
            ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(
                n_keep,
                path_append,
            ),
            _,
            element_flags,
        ) => grovedbg_types::Element::UpstreamRootHeightWithParentPathAdditionReference {
            n_keep: n_keep.into(),
            path_append,
            element_flags,
        },
        crate::Element::Reference(
            ReferencePathType::UpstreamFromElementHeightReference(n_remove, path_append),
            _,
            element_flags,
        ) => grovedbg_types::Element::UpstreamFromElementHeightReference {
            n_remove: n_remove.into(),
            path_append,
            element_flags,
        },
        crate::Element::Reference(
            ReferencePathType::CousinReference(swap_parent),
            _,
            element_flags,
        ) => grovedbg_types::Element::CousinReference {
            swap_parent,
            element_flags,
        },
        crate::Element::Reference(
            ReferencePathType::RemovedCousinReference(swap_parent),
            _,
            element_flags,
        ) => grovedbg_types::Element::RemovedCousinReference {
            swap_parent,
            element_flags,
        },
        crate::Element::Reference(
            ReferencePathType::SiblingReference(sibling_key),
            _,
            element_flags,
        ) => grovedbg_types::Element::SiblingReference {
            sibling_key,
            element_flags,
        },
        crate::Element::SumItem(value, element_flags) => grovedbg_types::Element::SumItem {
            value,
            element_flags,
        },
        crate::Element::SumTree(root_key, sum, element_flags) => grovedbg_types::Element::Sumtree {
            root_key,
            sum,
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
        right_child,
        value_hash,
        kv_digest_hash,
        feature_type,
    }: NodeDbg,
) -> Result<NodeUpdate, crate::Error> {
    // todo: GroveVersion::latest() to actual version
    let grovedb_element = crate::Element::deserialize(&value, GroveVersion::latest())?;

    let element = element_to_grovedbg(grovedb_element);

    Ok(NodeUpdate {
        path,
        key,
        element,
        left_child,
        right_child,
        feature_type: match feature_type {
            TreeFeatureType::BasicMerkNode => grovedbg_types::TreeFeatureType::BasicMerkNode,
            TreeFeatureType::SummedMerkNode(x) => {
                grovedbg_types::TreeFeatureType::SummedMerkNode(x)
            }
        },
        value_hash,
        kv_digest_hash,
    })
}
