//! GroveDB debugging support module.

use std::sync::Weak;

use grovedb_merk::debugger::NodeDbg;
pub use grovedbg_grpc::grove_dbg_server::GroveDbgServer;
use grovedbg_grpc::{
    grove_dbg_server::GroveDbg, tonic, DbMessage, FetchRequest, NodeUpdate, StartStream,
};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::{reference_path::ReferencePathType, GroveDb};

pub struct GroveDbgService {
    grovedb: Weak<GroveDb>,
}

impl GroveDbgService {
    pub fn new(grovedb: Weak<GroveDb>) -> Self {
        GroveDbgService { grovedb }
    }
}

#[tonic::async_trait]
impl GroveDbg for GroveDbgService {
    type DbEventsStream = UnboundedReceiverStream<Result<DbMessage, tonic::Status>>;

    async fn db_events(
        &self,
        request: tonic::Request<StartStream>,
    ) -> Result<tonic::Response<Self::DbEventsStream>, tonic::Status> {
        todo!()
    }

    async fn fetch_node(
        &self,
        request: tonic::Request<FetchRequest>,
    ) -> Result<tonic::Response<NodeUpdate>, tonic::Status> {
        let FetchRequest { path, key } = request.into_inner();

        let db = self.grovedb.upgrade().expect("GroveDB is closed");

        let merk = db
            .open_non_transactional_merk_at_path(path.as_slice().into(), None)
            .unwrap()
            .map_err(|e| tonic::Status::internal(e.to_string()))?;

        let node = if path.is_empty() && key.is_empty() {
            // The GroveDB root is a corner case and handled separately
            merk.get_root_node_dbg()
                .map_err(|e| tonic::Status::internal(e.to_string()))?
        } else {
            merk.get_node_dbg(&key)
                .map_err(|e| tonic::Status::internal(e.to_string()))?
        };

        if let Some(NodeDbg {
            key,
            value,
            left_child,
            right_child,
        }) = node
        {
            let grovedb_element = crate::Element::deserialize(&value)
                .map_err(|e| tonic::Status::internal(e.to_string()))?;

            let element = match grovedb_element {
                crate::Element::Item(bytes, ..) => grovedbg_grpc::Element {
                    element: Some(grovedbg_grpc::element::Element::Item(grovedbg_grpc::Item {
                        value: bytes,
                    })),
                },
                crate::Element::Tree(root_key, ..) | crate::Element::SumTree(root_key, ..) => {
                    grovedbg_grpc::Element {
                        element: Some(grovedbg_grpc::element::Element::Subtree(
                            grovedbg_grpc::Subtree { root_key },
                        )),
                    }
                }
                crate::Element::Reference(ReferencePathType::AbsolutePathReference(path), ..) => {
                    grovedbg_grpc::Element {
                        element: Some(grovedbg_grpc::element::Element::AbsolutePathReference(
                            grovedbg_grpc::AbsolutePathReference { path },
                        )),
                    }
                }
                crate::Element::Reference(
                    ReferencePathType::UpstreamRootHeightReference(n_keep, path_append),
                    ..,
                ) => grovedbg_grpc::Element {
                    element: Some(
                        grovedbg_grpc::element::Element::UpstreamRootHeightReference(
                            grovedbg_grpc::UpstreamRootHeightReference {
                                n_keep: n_keep.into(),
                                path_append,
                            },
                        ),
                    ),
                },
                crate::Element::Reference(
                    ReferencePathType::UpstreamFromElementHeightReference(n_remove, path_append),
                    ..,
                ) => grovedbg_grpc::Element {
                    element: Some(
                        grovedbg_grpc::element::Element::UpstreamFromElementHeightReference(
                            grovedbg_grpc::UpstreamFromElementHeightReference {
                                n_remove: n_remove.into(),
                                path_append,
                            },
                        ),
                    ),
                },
                crate::Element::Reference(ReferencePathType::CousinReference(swap_parent), ..) => {
                    grovedbg_grpc::Element {
                        element: Some(grovedbg_grpc::element::Element::CousinReference(
                            grovedbg_grpc::CousinReference { swap_parent },
                        )),
                    }
                }
                crate::Element::Reference(
                    ReferencePathType::RemovedCousinReference(swap_parent),
                    ..,
                ) => grovedbg_grpc::Element {
                    element: Some(grovedbg_grpc::element::Element::RemovedCousinReference(
                        grovedbg_grpc::RemovedCousinReference { swap_parent },
                    )),
                },
                crate::Element::Reference(ReferencePathType::SiblingReference(sibling_key), ..) => {
                    grovedbg_grpc::Element {
                        element: Some(grovedbg_grpc::element::Element::SiblingReference(
                            grovedbg_grpc::SiblingReference { sibling_key },
                        )),
                    }
                }
                _ => todo!(),
            };

            Ok(tonic::Response::new(NodeUpdate {
                path,
                key,
                element: Some(element),
                left_child,
                right_child,
            }))
        } else {
            Ok(tonic::Response::new(NodeUpdate {
                path,
                key,
                element: None,
                left_child: None,
                right_child: None,
            }))
        }
    }
}
