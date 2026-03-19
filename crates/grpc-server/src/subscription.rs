use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use rxrpl_rpc_server::ServerContext;
use rxrpl_rpc_server::events::{ServerEvent, event_to_json};
use rxrpl_rpc_server::subscriptions::ConnectionSubscriptions;

use crate::service::proto;
use proto::xrpl_subscription_server::XrplSubscription;
use proto::*;

pub struct XrplSubscriptionService {
    pub ctx: Arc<ServerContext>,
}

#[tonic::async_trait]
impl XrplSubscription for XrplSubscriptionService {
    type SubscribeStream = ReceiverStream<Result<ServerEventMessage, Status>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let req = request.into_inner();

        // Build subscription filter from request
        let mut subs = ConnectionSubscriptions::new();
        let mut params = serde_json::json!({});

        if !req.streams.is_empty() {
            params["streams"] = serde_json::Value::Array(
                req.streams.iter().map(|s| serde_json::Value::String(s.clone())).collect(),
            );
        }

        if !req.accounts.is_empty() {
            params["accounts"] = serde_json::Value::Array(
                req.accounts.iter().map(|a| serde_json::Value::String(a.clone())).collect(),
            );
        }

        if !req.books.is_empty() {
            let books: Vec<serde_json::Value> = req
                .books
                .iter()
                .filter_map(|b| {
                    let pays: serde_json::Value = serde_json::from_str(&b.taker_pays_json).ok()?;
                    let gets: serde_json::Value = serde_json::from_str(&b.taker_gets_json).ok()?;
                    Some(serde_json::json!({"taker_pays": pays, "taker_gets": gets}))
                })
                .collect();
            params["books"] = serde_json::Value::Array(books);
        }

        subs.apply_subscribe(&params)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        // Subscribe to the event broadcast channel
        let mut event_rx: broadcast::Receiver<ServerEvent> =
            self.ctx.event_sender().subscribe();

        let (tx, rx) = tokio::sync::mpsc::channel(256);

        tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(event) => {
                        if subs.matches(&event) {
                            let json = event_to_json(&event);
                            let event_type = json
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let msg = ServerEventMessage {
                                event_type,
                                json_payload: serde_json::to_string(&json).unwrap_or_default(),
                            };
                            if tx.send(Ok(msg)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("gRPC subscriber lagged, skipped {} events", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
