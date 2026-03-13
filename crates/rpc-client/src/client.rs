use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use rxrpl_rpc_api::responses::{
    AccountCurrenciesResponse, AccountInfoResponse, AccountLinesResponse, AccountNftsResponse,
    AccountObjectsResponse, AccountOffersResponse, AmmInfoResponse, BookOffersResponse,
    FeeResponse, LedgerClosedResponse, LedgerResponse, ServerInfoResponse, SubmitResponse,
    TxResponse,
};

use crate::builder::ClientBuilder;
use crate::error::ClientError;
use crate::transport::TransportKind;

/// Async XRPL RPC client.
pub struct XrplClient {
    transport: TransportKind,
}

impl XrplClient {
    pub fn new(transport: TransportKind) -> Self {
        Self { transport }
    }

    pub fn builder() -> ClientBuilderInit {
        ClientBuilderInit
    }

    /// Send a raw RPC request.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        self.transport.request(method, params).await
    }

    // -- Typed convenience methods --

    pub async fn server_info(&self) -> Result<Value, ClientError> {
        self.request("server_info", serde_json::json!({})).await
    }

    pub async fn server_state(&self) -> Result<Value, ClientError> {
        self.request("server_state", serde_json::json!({})).await
    }

    pub async fn fee(&self) -> Result<Value, ClientError> {
        self.request("fee", serde_json::json!({})).await
    }

    pub async fn ping(&self) -> Result<Value, ClientError> {
        self.request("ping", serde_json::json!({})).await
    }

    pub async fn random(&self) -> Result<Value, ClientError> {
        self.request("random", serde_json::json!({})).await
    }

    pub async fn account_info(&self, account: &str) -> Result<Value, ClientError> {
        self.request(
            "account_info",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn account_tx(
        &self,
        account: &str,
        limit: Option<u32>,
    ) -> Result<Value, ClientError> {
        let mut params = serde_json::json!({
            "account": account,
        });
        if let Some(l) = limit {
            params["limit"] = serde_json::json!(l);
        }
        self.request("account_tx", params).await
    }

    pub async fn account_lines(&self, account: &str) -> Result<Value, ClientError> {
        self.request(
            "account_lines",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn account_objects(&self, account: &str) -> Result<Value, ClientError> {
        self.request(
            "account_objects",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn account_offers(&self, account: &str) -> Result<Value, ClientError> {
        self.request(
            "account_offers",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn account_currencies(&self, account: &str) -> Result<Value, ClientError> {
        self.request(
            "account_currencies",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn tx(&self, hash: &str) -> Result<Value, ClientError> {
        self.request(
            "tx",
            serde_json::json!({
                "transaction": hash,
            }),
        )
        .await
    }

    pub async fn submit(&self, tx_blob: &str) -> Result<Value, ClientError> {
        self.request(
            "submit",
            serde_json::json!({
                "tx_blob": tx_blob,
            }),
        )
        .await
    }

    pub async fn ledger(&self, ledger_index: &str) -> Result<Value, ClientError> {
        self.request(
            "ledger",
            serde_json::json!({
                "ledger_index": ledger_index,
            }),
        )
        .await
    }

    pub async fn ledger_closed(&self) -> Result<Value, ClientError> {
        self.request("ledger_closed", serde_json::json!({})).await
    }

    pub async fn ledger_current(&self) -> Result<Value, ClientError> {
        self.request("ledger_current", serde_json::json!({})).await
    }

    pub async fn ledger_entry(&self, params: Value) -> Result<Value, ClientError> {
        self.request("ledger_entry", params).await
    }

    pub async fn book_offers(
        &self,
        taker_gets: Value,
        taker_pays: Value,
    ) -> Result<Value, ClientError> {
        self.request(
            "book_offers",
            serde_json::json!({
                "taker_gets": taker_gets,
                "taker_pays": taker_pays,
            }),
        )
        .await
    }

    pub async fn nft_buy_offers(&self, nft_id: &str) -> Result<Value, ClientError> {
        self.request(
            "nft_buy_offers",
            serde_json::json!({
                "nft_id": nft_id,
            }),
        )
        .await
    }

    pub async fn nft_sell_offers(&self, nft_id: &str) -> Result<Value, ClientError> {
        self.request(
            "nft_sell_offers",
            serde_json::json!({
                "nft_id": nft_id,
            }),
        )
        .await
    }

    pub async fn subscribe(&self, streams: Vec<String>) -> Result<Value, ClientError> {
        self.request(
            "subscribe",
            serde_json::json!({
                "streams": streams,
            }),
        )
        .await
    }

    pub async fn unsubscribe(&self, streams: Vec<String>) -> Result<Value, ClientError> {
        self.request(
            "unsubscribe",
            serde_json::json!({
                "streams": streams,
            }),
        )
        .await
    }

    pub async fn wallet_propose(&self, key_type: Option<&str>) -> Result<Value, ClientError> {
        let mut params = serde_json::json!({});
        if let Some(kt) = key_type {
            params["key_type"] = serde_json::json!(kt);
        }
        self.request("wallet_propose", params).await
    }

    pub async fn deposit_authorized(
        &self,
        source: &str,
        destination: &str,
    ) -> Result<Value, ClientError> {
        self.request(
            "deposit_authorized",
            serde_json::json!({
                "source_account": source,
                "destination_account": destination,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn gateway_balances(&self, account: &str) -> Result<Value, ClientError> {
        self.request(
            "gateway_balances",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn server_definitions(&self) -> Result<Value, ClientError> {
        self.request("server_definitions", serde_json::json!({}))
            .await
    }

    pub async fn feature(&self) -> Result<Value, ClientError> {
        self.request("feature", serde_json::json!({})).await
    }

    pub async fn manifest(&self, public_key: &str) -> Result<Value, ClientError> {
        self.request(
            "manifest",
            serde_json::json!({
                "public_key": public_key,
            }),
        )
        .await
    }

    pub async fn amm_info(&self, amm_account: &str) -> Result<Value, ClientError> {
        self.request(
            "amm_info",
            serde_json::json!({
                "amm_account": amm_account,
            }),
        )
        .await
    }

    pub async fn amm_info_by_assets(
        &self,
        asset: Value,
        asset2: Value,
    ) -> Result<Value, ClientError> {
        self.request(
            "amm_info",
            serde_json::json!({
                "asset": asset,
                "asset2": asset2,
            }),
        )
        .await
    }

    pub async fn account_nfts(&self, account: &str) -> Result<Value, ClientError> {
        self.request(
            "account_nfts",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    // -- Typed convenience methods (deserialize into rpc-api response structs) --

    async fn request_typed<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, ClientError> {
        let value = self.request(method, params).await?;
        serde_json::from_value(value).map_err(ClientError::from)
    }

    pub async fn account_info_typed(
        &self,
        account: &str,
    ) -> Result<AccountInfoResponse, ClientError> {
        self.request_typed(
            "account_info",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn fee_typed(&self) -> Result<FeeResponse, ClientError> {
        self.request_typed("fee", serde_json::json!({})).await
    }

    pub async fn tx_typed(&self, hash: &str) -> Result<TxResponse, ClientError> {
        self.request_typed(
            "tx",
            serde_json::json!({
                "transaction": hash,
            }),
        )
        .await
    }

    pub async fn submit_typed(&self, tx_blob: &str) -> Result<SubmitResponse, ClientError> {
        self.request_typed(
            "submit",
            serde_json::json!({
                "tx_blob": tx_blob,
            }),
        )
        .await
    }

    /// Submit a multisigned transaction (JSON, not blob).
    pub async fn submit_multisigned(&self, tx_json: &Value) -> Result<Value, ClientError> {
        self.request(
            "submit_multisigned",
            serde_json::json!({
                "tx_json": tx_json,
            }),
        )
        .await
    }

    /// Submit a multisigned transaction with typed response.
    pub async fn submit_multisigned_typed(
        &self,
        tx_json: &Value,
    ) -> Result<SubmitResponse, ClientError> {
        self.request_typed(
            "submit_multisigned",
            serde_json::json!({
                "tx_json": tx_json,
            }),
        )
        .await
    }

    pub async fn account_lines_typed(
        &self,
        account: &str,
    ) -> Result<AccountLinesResponse, ClientError> {
        self.request_typed(
            "account_lines",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn account_objects_typed(
        &self,
        account: &str,
    ) -> Result<AccountObjectsResponse, ClientError> {
        self.request_typed(
            "account_objects",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn account_offers_typed(
        &self,
        account: &str,
    ) -> Result<AccountOffersResponse, ClientError> {
        self.request_typed(
            "account_offers",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn account_currencies_typed(
        &self,
        account: &str,
    ) -> Result<AccountCurrenciesResponse, ClientError> {
        self.request_typed(
            "account_currencies",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn account_nfts_typed(
        &self,
        account: &str,
    ) -> Result<AccountNftsResponse, ClientError> {
        self.request_typed(
            "account_nfts",
            serde_json::json!({
                "account": account,
                "ledger_index": "validated"
            }),
        )
        .await
    }

    pub async fn server_info_typed(&self) -> Result<ServerInfoResponse, ClientError> {
        self.request_typed("server_info", serde_json::json!({}))
            .await
    }

    pub async fn book_offers_typed(
        &self,
        taker_gets: Value,
        taker_pays: Value,
    ) -> Result<BookOffersResponse, ClientError> {
        self.request_typed(
            "book_offers",
            serde_json::json!({
                "taker_gets": taker_gets,
                "taker_pays": taker_pays,
            }),
        )
        .await
    }

    pub async fn ledger_closed_typed(&self) -> Result<LedgerClosedResponse, ClientError> {
        self.request_typed("ledger_closed", serde_json::json!({}))
            .await
    }

    pub async fn amm_info_typed(
        &self,
        amm_account: &str,
    ) -> Result<AmmInfoResponse, ClientError> {
        self.request_typed(
            "amm_info",
            serde_json::json!({
                "amm_account": amm_account,
            }),
        )
        .await
    }

    pub async fn amm_info_by_assets_typed(
        &self,
        asset: Value,
        asset2: Value,
    ) -> Result<AmmInfoResponse, ClientError> {
        self.request_typed(
            "amm_info",
            serde_json::json!({
                "asset": asset,
                "asset2": asset2,
            }),
        )
        .await
    }

    pub async fn ledger_typed(
        &self,
        ledger_index: &str,
    ) -> Result<LedgerResponse, ClientError> {
        self.request_typed(
            "ledger",
            serde_json::json!({
                "ledger_index": ledger_index,
            }),
        )
        .await
    }

    /// Submit a signed transaction and poll until validated or timeout.
    ///
    /// Returns the validated transaction result, or `ClientError::Timeout` if
    /// the transaction is not validated within `timeout_secs`.
    pub async fn submit_and_wait(
        &self,
        tx_blob: &str,
        tx_hash: &str,
        timeout_secs: u64,
    ) -> Result<Value, ClientError> {
        let submit_result = self.submit(tx_blob).await?;

        // Check for immediate rejection
        if let Some(code) = submit_result["engine_result_code"].as_i64() {
            if code >= 100 {
                return Err(ClientError::Other(format!(
                    "transaction rejected: {}",
                    submit_result["engine_result"]
                        .as_str()
                        .unwrap_or("unknown"),
                )));
            }
        }

        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        let poll_interval = Duration::from_secs(1);

        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(ClientError::Timeout);
            }

            tokio::time::sleep(poll_interval).await;

            match self.tx(tx_hash).await {
                Ok(result) => {
                    if result.get("validated").and_then(|v| v.as_bool()) == Some(true) {
                        return Ok(result);
                    }
                    // Not yet validated, keep polling
                }
                Err(ClientError::Rpc { ref error, .. }) if error.contains("txnNotFound") => {
                    // Not yet in a ledger, keep polling
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Get an independent subscription event stream.
    /// Returns `None` for HTTP-based clients.
    pub fn subscription_stream(&self) -> Option<crate::subscription::SubscriptionStream> {
        self.transport.subscription_stream()
    }

    /// Gracefully close the transport connection.
    pub async fn close(&self) -> Result<(), ClientError> {
        self.transport.close().await
    }

    /// Get the inner transport for direct access.
    pub fn transport(&self) -> &TransportKind {
        &self.transport
    }
}

/// Initial builder state before URL is set.
pub struct ClientBuilderInit;

impl ClientBuilderInit {
    pub fn url(self, url: impl Into<String>) -> ClientBuilder {
        ClientBuilder::new(url)
    }
}
