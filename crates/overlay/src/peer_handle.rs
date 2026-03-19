use std::sync::Arc;

use rxrpl_p2p_proto::codec::PeerMessage;
use rxrpl_primitives::Hash256;
use tokio::sync::mpsc;

use crate::peer_set::PeerInfo;

/// Handle to a connected peer, used for sending messages.
pub struct PeerHandle {
    pub node_id: Hash256,
    pub info: Arc<PeerInfo>,
    pub tx: mpsc::Sender<PeerMessage>,
}
