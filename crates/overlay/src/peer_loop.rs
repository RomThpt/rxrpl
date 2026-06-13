use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rxrpl_p2p_proto::codec::{PeerCodec, PeerMessage};
use rxrpl_primitives::Hash256;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::event::PeerEvent;
use crate::tls::PeerStream;

/// Idle read timeout: a peer that sends nothing for this long is treated as
/// dead and disconnected, which triggers the fixed-peer reconnect path.
///
/// A healthy peer on a consensus network is never this quiet — proposals,
/// validations, status changes and periodic pings all arrive far more often.
/// Without this, a silent partition (cable pulled, NAT drop, frozen peer)
/// leaves `read.next()` blocked forever: the dead connection lingers in the
/// peer set and reconnection never fires. 60s is conservative enough to avoid
/// false positives during brief lulls while still detecting a real partition.
const PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Read loop for a single peer connection.
///
/// Reads messages from the framed TLS stream and forwards them as PeerEvents.
/// Sends a Disconnected event on EOF, error, or idle timeout, then returns.
pub async fn run_peer_read_loop(
    node_id: Hash256,
    mut read: SplitStream<Framed<PeerStream, PeerCodec>>,
    event_tx: mpsc::Sender<PeerEvent>,
) {
    loop {
        let next = match tokio::time::timeout(PEER_IDLE_TIMEOUT, read.next()).await {
            Ok(next) => next,
            Err(_elapsed) => {
                tracing::debug!(
                    "peer {} idle for {}s, treating as dead",
                    node_id,
                    PEER_IDLE_TIMEOUT.as_secs()
                );
                let _ = event_tx.send(PeerEvent::Disconnected { node_id }).await;
                break;
            }
        };
        match next {
            Some(Ok(msg)) => {
                tracing::debug!(
                    "peer {} msg {:?} ({} bytes)",
                    node_id,
                    msg.msg_type,
                    msg.payload.len()
                );
                let event = PeerEvent::Message {
                    from: node_id,
                    msg_type: msg.msg_type,
                    payload: msg.payload,
                };
                if event_tx.send(event).await.is_err() {
                    break;
                }
            }
            Some(Err(e)) => {
                tracing::debug!("peer {} read error: {}", node_id, e);
                let _ = event_tx.send(PeerEvent::Disconnected { node_id }).await;
                break;
            }
            None => {
                tracing::debug!("peer {} connection closed", node_id);
                let _ = event_tx.send(PeerEvent::Disconnected { node_id }).await;
                break;
            }
        }
    }
}

/// Write loop for a single peer connection.
///
/// Receives messages from the channel and writes them to the framed TLS stream.
/// Returns when the channel is closed or a write error occurs.
pub async fn run_peer_write_loop(
    mut write: SplitSink<Framed<PeerStream, PeerCodec>, PeerMessage>,
    mut rx: mpsc::Receiver<PeerMessage>,
) {
    while let Some(msg) = rx.recv().await {
        tracing::debug!("sending {:?} ({} bytes)", msg.msg_type, msg.payload.len());
        if let Err(e) = write.send(msg).await {
            tracing::debug!("peer write error: {}", e);
            break;
        }
    }
}
