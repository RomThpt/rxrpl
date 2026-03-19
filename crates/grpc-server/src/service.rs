use std::sync::Arc;

use tonic::{Request, Response, Status};

use rxrpl_rpc_server::ServerContext;
use rxrpl_rpc_server::role::{ConnectionRole, RequestContext};
use rxrpl_rpc_server::router::dispatch;

use crate::convert;

pub mod proto {
    tonic::include_proto!("xrpl");
}

use proto::xrpl_node_server::XrplNode;
use proto::*;

pub struct XrplNodeService {
    pub ctx: Arc<ServerContext>,
}

fn admin_req_ctx() -> RequestContext {
    RequestContext {
        role: ConnectionRole::Admin,
        api_version: Default::default(),
    }
}

async fn call_dispatch(
    ctx: &Arc<ServerContext>,
    method: &str,
    params: serde_json::Value,
) -> Result<String, Status> {
    let req_ctx = admin_req_ctx();
    match dispatch(method, params, ctx, &req_ctx).await {
        Ok(value) => Ok(convert::json_to_string(&value)),
        Err(e) => Err(Status::internal(e.to_string())),
    }
}

#[tonic::async_trait]
impl XrplNode for XrplNodeService {
    async fn ping(
        &self,
        _request: Request<PingRequest>,
    ) -> Result<Response<PingResponse>, Status> {
        let _ = call_dispatch(&self.ctx, "ping", serde_json::json!({})).await?;
        Ok(Response::new(PingResponse {}))
    }

    async fn server_info(
        &self,
        _request: Request<ServerInfoRequest>,
    ) -> Result<Response<ServerInfoResponse>, Status> {
        let json_result = call_dispatch(&self.ctx, "server_info", serde_json::json!({})).await?;
        Ok(Response::new(ServerInfoResponse { json_result }))
    }

    async fn fee(
        &self,
        _request: Request<FeeRequest>,
    ) -> Result<Response<FeeResponse>, Status> {
        let json_result = call_dispatch(&self.ctx, "fee", serde_json::json!({})).await?;
        Ok(Response::new(FeeResponse { json_result }))
    }

    async fn account_info(
        &self,
        request: Request<AccountInfoRequest>,
    ) -> Result<Response<AccountInfoResponse>, Status> {
        let req = request.into_inner();
        let params = convert::account_params(&req.account);
        let json_result = call_dispatch(&self.ctx, "account_info", params).await?;
        Ok(Response::new(AccountInfoResponse { json_result }))
    }

    async fn account_tx(
        &self,
        request: Request<AccountTxRequest>,
    ) -> Result<Response<AccountTxResponse>, Status> {
        let req = request.into_inner();
        let params = convert::account_tx_params(&req.account, req.limit, &req.marker);
        let json_result = call_dispatch(&self.ctx, "account_tx", params).await?;
        Ok(Response::new(AccountTxResponse { json_result }))
    }

    async fn submit(
        &self,
        request: Request<SubmitRequest>,
    ) -> Result<Response<SubmitResponse>, Status> {
        let req = request.into_inner();
        let params = convert::submit_params(&req.tx_blob, &req.tx_json);
        let json_result = call_dispatch(&self.ctx, "submit", params).await?;
        Ok(Response::new(SubmitResponse { json_result }))
    }

    async fn tx(
        &self,
        request: Request<TxRequest>,
    ) -> Result<Response<TxResponse>, Status> {
        let req = request.into_inner();
        let params = convert::tx_params(&req.transaction);
        let json_result = call_dispatch(&self.ctx, "tx", params).await?;
        Ok(Response::new(TxResponse { json_result }))
    }

    async fn ledger_closed(
        &self,
        _request: Request<LedgerClosedRequest>,
    ) -> Result<Response<LedgerClosedResponse>, Status> {
        let json_result =
            call_dispatch(&self.ctx, "ledger_closed", serde_json::json!({})).await?;
        Ok(Response::new(LedgerClosedResponse { json_result }))
    }
}
