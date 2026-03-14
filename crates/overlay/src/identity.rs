use rxrpl_primitives::Hash256;

/// Node identity (keypair and derived node ID).
#[derive(Clone, Debug)]
pub struct NodeIdentity {
    /// The node's public key (hex-encoded).
    pub public_key: String,
    /// The node ID derived from the public key.
    pub node_id: Hash256,
}

impl NodeIdentity {
    /// Create a node identity from a public key hex string.
    pub fn new(public_key: String) -> Self {
        let node_id = rxrpl_crypto::sha512_half::sha512_half(&[public_key.as_bytes()]);
        Self {
            public_key,
            node_id,
        }
    }
}
