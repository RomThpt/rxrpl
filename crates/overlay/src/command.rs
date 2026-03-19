use rxrpl_p2p_proto::MessageType;
use rxrpl_primitives::Hash256;

/// Commands sent to the overlay network manager.
pub enum OverlayCommand {
    /// Broadcast a message to all connected peers.
    Broadcast {
        msg_type: MessageType,
        payload: Vec<u8>,
    },
    /// Send a message to a specific peer.
    SendTo {
        node_id: Hash256,
        msg_type: MessageType,
        payload: Vec<u8>,
    },
    /// Connect to a peer at the given address.
    ConnectTo { addr: String },
    /// Request a specific ledger from peers.
    RequestLedger { seq: u32, hash: Option<Hash256> },
}
