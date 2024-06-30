//! GroveDB debugging support module.

use std::{fs, net::Ipv4Addr, sync::Weak};

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use grovedb_merk::debugger::NodeDbg;
use grovedb_path::SubtreePath;
use grovedbg_types::{NodeFetchRequest, NodeUpdate, Path};
use tokio::{
    net::ToSocketAddrs,
    sync::mpsc::{self, Sender},
};
use tower_http::services::ServeDir;

use crate::{reference_path::ReferencePathType, GroveDb};

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
        let grovedbg_www = grovedbg_tmp.path().join("grovedbg_www");

        fs::write(&grovedbg_zip, &GROVEDBG_ZIP).expect("cannot crate grovedbg.zip");
        zip_extensions::read::zip_extract(&grovedbg_zip, &grovedbg_www)
            .expect("cannot extract grovedbg contents");

        let (shutdown_send, mut shutdown_receive) = mpsc::channel::<()>(1);
        let app = Router::new()
            .route("/fetch_node", post(fetch_node))
            .route("/fetch_root_node", post(fetch_root_node))
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
        shutdown.send(()).await.ok();
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
) -> Result<NodeUpdate, crate::Error> {
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
            ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(n_keep, path_append),
            ..,
        ) => grovedbg_types::Element::UpstreamRootHeightWithParentPathAdditionReference {
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
