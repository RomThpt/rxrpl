use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rxrpl_p2p_proto::codec::{PeerCodec, PeerMessage};
use rxrpl_primitives::Hash256;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::event::PeerEvent;
use crate::tls::PeerStream;

/// Read loop for a single peer connection.
///
/// Reads messages from the framed TLS stream and forwards them as PeerEvents.
/// Sends a Disconnected event on EOF or error, then returns.
pub async fn run_peer_read_loop(
    node_id: Hash256,
    mut read: SplitStream<Framed<PeerStream, PeerCodec>>,
    event_tx: mpsc::UnboundedSender<PeerEvent>,
) {
    loop {
        match read.next().await {
            Some(Ok(msg)) => {
                let event = PeerEvent::Message {
                    from: node_id,
                    msg_type: msg.msg_type,
                    payload: msg.payload,
                };
                if event_tx.send(event).is_err() {
                    break;
                }
            }
            Some(Err(e)) => {
                tracing::debug!("peer {} read error: {}", node_id, e);
                let _ = event_tx.send(PeerEvent::Disconnected { node_id });
                break;
            }
            None => {
                tracing::debug!("peer {} connection closed", node_id);
                let _ = event_tx.send(PeerEvent::Disconnected { node_id });
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
        if let Err(e) = write.send(msg).await {
            tracing::debug!("peer write error: {}", e);
            break;
        }
    }
}
