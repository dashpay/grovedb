//! GroveDB debugging support module.

use std::{net::Ipv4Addr, sync::Weak};

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use grovedb_merk::debugger::NodeDbg;
use grovedb_path::SubtreePath;
use grovedbg_types::{NodeFetchRequest, NodeUpdate, Path};
use tokio::sync::mpsc::{self, Sender};

use crate::{reference_path::ReferencePathType, GroveDb};

pub(super) fn start_visualizer(grovedb: Weak<GroveDb>, port: u16) {
    std::thread::spawn(move || {
        let (shutdown_send, mut shutdown_receive) = mpsc::channel::<()>(1);
        let app = Router::new()
            .route("/fetch_node", get(fetch_node))
            .with_state((shutdown_send, grovedb));

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                    .await
                    .expect("can't bind visualizer port");
                axum::serve(listener, app).with_graceful_shutdown(async move {
                    shutdown_receive.recv().await;
                })
            });
    });
}

enum AppError {
    Closed,
    Any(anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        match self {
            AppError::Closed => {
                (StatusCode::SERVICE_UNAVAILABLE, "GroveDB is closed").into_response()
            }
            AppError::Any(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self::Any(err.into())
    }
}

async fn fetch_node(
    State((shutdown, grovedb)): State<(Sender<()>, Weak<GroveDb>)>,
    Json(NodeFetchRequest { path, key }): Json<NodeFetchRequest>,
) -> Result<Json<Option<NodeUpdate>>, AppError> {
    let Some(db) = grovedb.upgrade() else {
        shutdown.send(()).await;
        return Err(AppError::Closed);
    };

    let merk = db
        .open_non_transactional_merk_at_path(path.as_slice().into(), None)
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
        shutdown.send(()).await;
        return Err(AppError::Closed);
    };

    let merk = db
        .open_non_transactional_merk_at_path(SubtreePath::empty(), None)
        .unwrap()?;

    let node = merk.get_root_node_dbg()?;

    if let Some(node) = node {
        let node_update: NodeUpdate = node_to_update(Vec::new(), node)?;
        Ok(Json(Some(node_update)))
    } else {
        Ok(None.into())
    }
}

fn node_to_update(
    path: Path,
    NodeDbg {
        key,
        value,
        left_child,
        right_child,
    }: NodeDbg,
) -> Result<NodeUpdate, anyhow::Error> {
    let grovedb_element = crate::Element::deserialize(&value)?;

    let element = match grovedb_element {
        crate::Element::Item(value, ..) => grovedbg_types::Element::Item { value },
        crate::Element::Tree(root_key, ..) => grovedbg_types::Element::Subtree { root_key },
        crate::Element::Reference(ReferencePathType::AbsolutePathReference(path), ..) => {
            grovedbg_types::Element::AbsolutePathReference { path }
        }
        crate::Element::Reference(
            ReferencePathType::UpstreamRootHeightReference(n_keep, path_append),
            ..,
        ) => grovedbg_types::Element::UpstreamRootHeightReference {
            n_keep: n_keep.into(),
            path_append,
        },
        crate::Element::Reference(
            ReferencePathType::UpstreamFromElementHeightReference(n_remove, path_append),
            ..,
        ) => grovedbg_types::Element::UpstreamFromElementHeightReference {
            n_remove: n_remove.into(),
            path_append,
        },
        crate::Element::Reference(ReferencePathType::CousinReference(swap_parent), ..) => {
            grovedbg_types::Element::CousinReference { swap_parent }
        }
        crate::Element::Reference(ReferencePathType::RemovedCousinReference(swap_parent), ..) => {
            grovedbg_types::Element::RemovedCousinReference { swap_parent }
        }
        crate::Element::Reference(ReferencePathType::SiblingReference(sibling_key), ..) => {
            grovedbg_types::Element::SiblingReference { sibling_key }
        }
        crate::Element::SumItem(value, _) => grovedbg_types::Element::SumItem { value },
        crate::Element::SumTree(root_key, sum, _) => {
            grovedbg_types::Element::Sumtree { root_key, sum }
        }
    };

    Ok(NodeUpdate {
        path,
        key,
        element,
        left_child,
        right_child,
    })
}

// use std::sync::Weak;

// use grovedb_merk::debugger::NodeDbg;
// pub use grovedbg_grpc::grove_dbg_server::GroveDbgServer;
// use grovedbg_grpc::{
//     grove_dbg_server::GroveDbg, tonic, DbMessage, FetchRequest, NodeUpdate, StartStream,
// };
// use tokio_stream::wrappers::UnboundedReceiverStream;

// use crate::{reference_path::ReferencePathType, GroveDb};

// pub struct GroveDbgService {
//     grovedb: Weak<GroveDb>,
// }

// impl GroveDbgService {
//     pub fn new(grovedb: Weak<GroveDb>) -> Self {
//         GroveDbgService { grovedb }
//     }
// }

// #[tonic::async_trait]
// impl GroveDbg for GroveDbgService {
//     type DbEventsStream = UnboundedReceiverStream<Result<DbMessage, tonic::Status>>;

//     async fn db_events(
//         &self,
//         request: tonic::Request<StartStream>,
//     ) -> Result<tonic::Response<Self::DbEventsStream>, tonic::Status> {
//         todo!()
//     }

//     async fn fetch_node(
//         &self,
//         request: tonic::Request<FetchRequest>,
//     ) -> Result<tonic::Response<NodeUpdate>, tonic::Status> {
//         let FetchRequest { path, key } = request.into_inner();

//         let db = self.grovedb.upgrade().expect("GroveDB is closed");

//         let merk = db
//             .open_non_transactional_merk_at_path(path.as_slice().into(), None)
//             .unwrap()
//             .map_err(|e| tonic::Status::internal(e.to_string()))?;

//         let node = if path.is_empty() && key.is_empty() {
//             // The GroveDB root is a corner case and handled separately
//             merk.get_root_node_dbg()
//                 .map_err(|e| tonic::Status::internal(e.to_string()))?
//         } else {
//             merk.get_node_dbg(&key)
//                 .map_err(|e| tonic::Status::internal(e.to_string()))?
//         };

//         if let Some(NodeDbg {
//             key,
//         if let Some(NodeDbg {
//             key,
//             value,
//             left_child,
//             right_child,
//         }) = node
//         {
//             let grovedb_element = crate::Element::deserialize(&value)
//                 .map_err(|e| tonic::Status::internal(e.to_string()))?;

//             let element = match grovedb_element {
//                 crate::Element::Item(bytes, ..) => grovedbg_grpc::Element {
//                     element: Some(grovedbg_grpc::element::Element::Item(grovedbg_grpc::Item {
//                         value: bytes,
//                     })),
//                 },
//                 crate::Element::Tree(root_key, ..) => grovedbg_grpc::Element {
//                     element: Some(grovedbg_grpc::element::Element::Subtree(
//                         grovedbg_grpc::Subtree { root_key },
//                     )),
//                 },
//                 crate::Element::Reference(ReferencePathType::AbsolutePathReference(path), ..) => {
//                     grovedbg_grpc::Element {
//                         element: Some(grovedbg_grpc::element::Element::AbsolutePathReference(
//                             grovedbg_grpc::AbsolutePathReference { path },
//                         )),
//                     }
//                 }
//                 crate::Element::Reference(
//                     ReferencePathType::UpstreamRootHeightReference(n_keep, path_append),
//                     ..,
//                 ) => grovedbg_grpc::Element {
//                     element: Some(
//                         grovedbg_grpc::element::Element::UpstreamRootHeightReference(
//                             grovedbg_grpc::UpstreamRootHeightReference {
//                                 n_keep: n_keep.into(),
//                                 path_append,
//                             },
//                         ),
//                     ),
//                 },
//                 crate::Element::Reference(
//                     ReferencePathType::UpstreamFromElementHeightReference(n_remove, path_append),
//                     ..,
//                 ) => grovedbg_grpc::Element {
//                     element: Some(
//                         grovedbg_grpc::element::Element::UpstreamFromElementHeightReference(
//                             grovedbg_grpc::UpstreamFromElementHeightReference {
//                                 n_remove: n_remove.into(),
//                                 path_append,
//                             },
//                         ),
//                     ),
//                 },
//                 crate::Element::Reference(ReferencePathType::CousinReference(swap_parent), ..) => {
//                     grovedbg_grpc::Element {
//                         element: Some(grovedbg_grpc::element::Element::CousinReference(
//                             grovedbg_grpc::CousinReference { swap_parent },
//                         )),
//                     }
//                 }
//                 crate::Element::Reference(
//                     ReferencePathType::RemovedCousinReference(swap_parent),
//                     ..,
//                 ) => grovedbg_grpc::Element {
//                     element: Some(grovedbg_grpc::element::Element::RemovedCousinReference(
//                         grovedbg_grpc::RemovedCousinReference { swap_parent },
//                     )),
//                 },
//                 crate::Element::Reference(ReferencePathType::SiblingReference(sibling_key), ..) => {
//                     grovedbg_grpc::Element {
//                         element: Some(grovedbg_grpc::element::Element::SiblingReference(
//                             grovedbg_grpc::SiblingReference { sibling_key },
//                         )),
//                     }
//                 }
//                 crate::Element::SumItem(sum, _) => grovedbg_grpc::Element {
//                     element: Some(grovedbg_grpc::element::Element::SumItem(
//                         grovedbg_grpc::SumItem { value: sum },
//                     )),
//                 },
//                 crate::Element::SumTree(root_key, sum, _) => grovedbg_grpc::Element {
//                     element: Some(grovedbg_grpc::element::Element::Sumtree(
//                         grovedbg_grpc::Sumtree { root_key, sum },
//                     )),
//                 },
//             };

//             value,
//             left_child,
//             right_child,
//         }) = node
//         {
//             let grovedb_element = crate::Element::deserialize(&value)
//                 .map_err(|e| tonic::Status::internal(e.to_string()))?;

//             let element = match grovedb_element {
//                 crate::Element::Item(bytes, ..) => grovedbg_grpc::Element {
//                     element: Some(grovedbg_grpc::element::Element::Item(grovedbg_grpc::Item {
//                         value: bytes,
//                     })),
//                 },
//                 crate::Element::Tree(root_key, ..) => grovedbg_grpc::Element {
//                     element: Some(grovedbg_grpc::element::Element::Subtree(
//                         grovedbg_grpc::Subtree { root_key },
//                     )),
//                 },
//                 crate::Element::Reference(ReferencePathType::AbsolutePathReference(path), ..) => {
//                     grovedbg_grpc::Element {
//                         element: Some(grovedbg_grpc::element::Element::AbsolutePathReference(
//                             grovedbg_grpc::AbsolutePathReference { path },
//                         )),
//                     }
//                 }
//                 crate::Element::Reference(
//                     ReferencePathType::UpstreamRootHeightReference(n_keep, path_append),
//                     ..,
//                 ) => grovedbg_grpc::Element {
//                     element: Some(
//                         grovedbg_grpc::element::Element::UpstreamRootHeightReference(
//                             grovedbg_grpc::UpstreamRootHeightReference {
//                                 n_keep: n_keep.into(),
//                                 path_append,
//                             },
//                         ),
//                     ),
//                 },
//                 crate::Element::Reference(
//                     ReferencePathType::UpstreamFromElementHeightReference(n_remove, path_append),
//                     ..,
//                 ) => grovedbg_grpc::Element {
//                     element: Some(
//                         grovedbg_grpc::element::Element::UpstreamFromElementHeightReference(
//                             grovedbg_grpc::UpstreamFromElementHeightReference {
//                                 n_remove: n_remove.into(),
//                                 path_append,
//                             },
//                         ),
//                     ),
//                 },
//                 crate::Element::Reference(ReferencePathType::CousinReference(swap_parent), ..) => {
//                     grovedbg_grpc::Element {
//                         element: Some(grovedbg_grpc::element::Element::CousinReference(
//                             grovedbg_grpc::CousinReference { swap_parent },
//                         )),
//                     }
//                 }
//                 crate::Element::Reference(
//                     ReferencePathType::RemovedCousinReference(swap_parent),
//                     ..,
//                 ) => grovedbg_grpc::Element {
//                     element: Some(grovedbg_grpc::element::Element::RemovedCousinReference(
//                         grovedbg_grpc::RemovedCousinReference { swap_parent },
//                     )),
//                 },
//                 crate::Element::Reference(ReferencePathType::SiblingReference(sibling_key), ..) => {
//                     grovedbg_grpc::Element {
//                         element: Some(grovedbg_grpc::element::Element::SiblingReference(
//                             grovedbg_grpc::SiblingReference { sibling_key },
//                         )),
//                     }
//                 }
//                 crate::Element::SumItem(sum, _) => grovedbg_grpc::Element {
//                     element: Some(grovedbg_grpc::element::Element::SumItem(
//                         grovedbg_grpc::SumItem { value: sum },
//                     )),
//                 },
//                 crate::Element::SumTree(root_key, sum, _) => grovedbg_grpc::Element {
//                     element: Some(grovedbg_grpc::element::Element::Sumtree(
//                         grovedbg_grpc::Sumtree { root_key, sum },
//                     )),
//                 },
//             };

//             Ok(tonic::Response::new(NodeUpdate {
//                 path,
//                 key,
//                 element: Some(element),
//                 left_child,
//                 right_child,
//             }))
//         } else {
//             Ok(tonic::Response::new(NodeUpdate {
//                 path,
//                 key,
//                 element: None,
//                 left_child: None,
//                 right_child: None,
//             }))
//         }
//     }
// }
