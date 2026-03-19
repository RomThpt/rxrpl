use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers;
use crate::metrics;

/// Dispatch an RPC method call to the appropriate handler.
pub async fn dispatch(
    method: &str,
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let start = std::time::Instant::now();
    let result = dispatch_inner(method, params, ctx).await;

    ::metrics::counter!(metrics::RPC_REQUESTS_TOTAL, "method" => method.to_string()).increment(1);
    ::metrics::histogram!(metrics::RPC_REQUEST_DURATION_SECONDS, "method" => method.to_string())
        .record(start.elapsed().as_secs_f64());

    result
}

async fn dispatch_inner(
    method: &str,
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    match method {
        // Core
        "ping" => handlers::ping(params, ctx).await,
        "server_info" => handlers::server_info(params, ctx).await,
        "server_state" => handlers::server_state(params, ctx).await,
        "fee" => handlers::fee(params, ctx).await,
        "version" => handlers::version(params, ctx).await,

        // Account queries
        "account_info" => handlers::account_info(params, ctx).await,
        "account_objects" => handlers::account_objects(params, ctx).await,
        "account_lines" => handlers::account_lines(params, ctx).await,
        "account_offers" => handlers::account_offers(params, ctx).await,
        "account_channels" => handlers::account_channels(params, ctx).await,
        "account_currencies" => handlers::account_currencies(params, ctx).await,
        "account_nfts" => handlers::account_nfts(params, ctx).await,
        "account_tx" => handlers::account_tx(params, ctx).await,
        "gateway_balances" => handlers::gateway_balances(params, ctx).await,
        "noripple_check" => handlers::noripple_check(params, ctx).await,

        // Ledger queries
        "ledger" => handlers::ledger(params, ctx).await,
        "ledger_accept" => handlers::ledger_accept(params, ctx).await,
        "ledger_closed" => handlers::ledger_closed(params, ctx).await,
        "ledger_current" => handlers::ledger_current(params, ctx).await,
        "ledger_entry" => handlers::ledger_entry(params, ctx).await,
        "ledger_data" => handlers::ledger_data(params, ctx).await,
        "ledger_header" => handlers::ledger_header(params, ctx).await,
        "ledger_range" => handlers::ledger_range(params, ctx).await,
        "ledger_request" => handlers::ledger_request(params, ctx).await,

        // Transaction
        "submit" => handlers::submit(params, ctx).await,
        "submit_multisigned" => handlers::submit_multisigned(params, ctx).await,
        "tx" => handlers::tx(params, ctx).await,
        "tx_history" => handlers::tx_history(params, ctx).await,
        "transaction_entry" => handlers::transaction_entry(params, ctx).await,
        "sign" => handlers::sign(params, ctx).await,
        "sign_for" => handlers::sign_for(params, ctx).await,
        "simulate" => handlers::simulate(params, ctx).await,

        // Trading & NFT
        "book_offers" => handlers::book_offers(params, ctx).await,
        "book_changes" => handlers::book_changes(params, ctx).await,
        "amm_info" => handlers::amm_info(params, ctx).await,
        "nft_buy_offers" => handlers::nft_buy_offers(params, ctx).await,
        "nft_sell_offers" => handlers::nft_sell_offers(params, ctx).await,
        "nft_info" => handlers::nft_info(params, ctx).await,
        "nft_history" => handlers::nft_history(params, ctx).await,
        "account_nfts_by_issuer" => handlers::account_nfts_by_issuer(params, ctx).await,

        // Oracle
        "get_aggregate_price" => handlers::get_aggregate_price(params, ctx).await,

        // Vault
        "vault_info" => handlers::vault_info(params, ctx).await,

        // Pathfinding
        "ripple_path_find" => handlers::ripple_path_find(params, ctx).await,

        // Server utilities
        "wallet_propose" => handlers::wallet_propose(params, ctx).await,
        "random" => handlers::random(params, ctx).await,
        "server_definitions" => handlers::server_definitions(params, ctx).await,
        "feature" => handlers::feature(params, ctx).await,
        "deposit_authorized" => handlers::deposit_authorized(params, ctx).await,
        "channel_authorize" => handlers::channel_authorize(params, ctx).await,
        "channel_verify" => handlers::channel_verify(params, ctx).await,
        "manifest" => handlers::manifest(params, ctx).await,

        // Admin
        "peers" => handlers::peers(params, ctx).await,
        "consensus_info" => handlers::consensus_info(params, ctx).await,
        "validators" => handlers::validators(params, ctx).await,
        "stop" => handlers::stop(params, ctx).await,
        "log_level" => handlers::log_level(params, ctx).await,
        "connect" => handlers::connect(params, ctx).await,
        "validation_create" => handlers::validation_create(params, ctx).await,
        "validation_seed" => handlers::validation_seed(params, ctx).await,
        "validator_info" => handlers::validator_info(params, ctx).await,
        "peer_reservations_add" => handlers::peer_reservations_add(params, ctx).await,
        "peer_reservations_del" => handlers::peer_reservations_del(params, ctx).await,
        "peer_reservations_list" => handlers::peer_reservations_list(params, ctx).await,
        "validator_list_sites" => handlers::validator_list_sites(params, ctx).await,
        "fetch_info" => handlers::fetch_info(params, ctx).await,
        "print" => handlers::print(params, ctx).await,
        "ledger_cleaner" => handlers::ledger_cleaner(params, ctx).await,
        "ledger_diff" => handlers::ledger_diff(params, ctx).await,
        "can_delete" => handlers::can_delete(params, ctx).await,
        "logrotate" => handlers::logrotate(params, ctx).await,
        "crawl" => handlers::crawl(params, ctx).await,
        "tx_reduce_relay" => handlers::tx_reduce_relay(params, ctx).await,
        "server_subscribe" => handlers::server_subscribe(params, ctx).await,
        "path_find" => handlers::path_find(params, ctx).await,
        "json" => handlers::json(params, ctx).await,
        "batch" => handlers::batch(params, ctx).await,

        "subscribe" | "unsubscribe" => Err(RpcServerError::InvalidParams(
            "subscribe/unsubscribe only available over WebSocket".into(),
        )),

        _ => Err(RpcServerError::MethodNotFound(method.to_string())),
    }
}
