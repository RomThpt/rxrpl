use serde_json::Value;
use tokio::sync::broadcast;

use crate::error::ClientError;

/// A subscription stream that yields events from the XRPL WebSocket.
///
/// Each `SubscriptionStream` has its own independent broadcast receiver,
/// so multiple consumers can receive events concurrently without
/// interfering with each other.
pub struct SubscriptionStream {
    receiver: broadcast::Receiver<Value>,
}

impl SubscriptionStream {
    pub(crate) fn new(receiver: broadcast::Receiver<Value>) -> Self {
        Self { receiver }
    }

    /// Wait for and return the next subscription event.
    pub async fn next(&mut self) -> Result<Value, ClientError> {
        loop {
            match self.receiver.recv().await {
                Ok(value) => return Ok(value),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("subscription stream lagged, skipped {n} events");
                    // Continue to receive the next available event
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(ClientError::SubscriptionClosed);
                }
            }
        }
    }
}
