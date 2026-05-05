use std::net::SocketAddr;
use std::sync::Arc;

use tonic::transport::Server;

use rxrpl_rpc_server::ServerContext;

use crate::service::XrplNodeService;
use crate::service::proto::xrpl_node_server::XrplNodeServer;
use crate::service::proto::xrpl_subscription_server::XrplSubscriptionServer;
use crate::subscription::XrplSubscriptionService;

/// Start the gRPC server on the given address.
///
/// This spawns the server as a background task and returns immediately.
pub async fn start_grpc_server(
    addr: SocketAddr,
    ctx: Arc<ServerContext>,
) -> Result<(), tonic::transport::Error> {
    let node_service = XrplNodeService {
        ctx: Arc::clone(&ctx),
    };
    let sub_service = XrplSubscriptionService { ctx };

    tracing::info!("starting gRPC server on {}", addr);

    Server::builder()
        .add_service(XrplNodeServer::new(node_service))
        .add_service(XrplSubscriptionServer::new(sub_service))
        .serve(addr)
        .await
}
