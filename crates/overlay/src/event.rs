use std::sync::Arc;

use rxrpl_p2p_proto::MessageType;
use rxrpl_p2p_proto::codec::PeerMessage;
use rxrpl_primitives::Hash256;
use tokio::sync::mpsc;

use crate::peer_set::PeerInfo;

/// Events received from connected peers.
pub enum PeerEvent {
    /// A new peer has connected and completed handshake.
    Connected {
        node_id: Hash256,
        info: Arc<PeerInfo>,
        write_tx: mpsc::Sender<PeerMessage>,
    },
    /// A message was received from a peer.
    Message {
        from: Hash256,
        msg_type: MessageType,
        payload: Vec<u8>,
    },
    /// A peer disconnected.
    Disconnected { node_id: Hash256 },
}
